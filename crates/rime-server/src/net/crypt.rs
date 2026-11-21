//! Online-mode login: RSA key exchange, AES-128/CFB8 stream encryption and
//! the Mojang session server check (`hasJoined`), like vanilla.

use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};

use aes::cipher::KeyIvInit;
use rsa::pkcs8::EncodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

type Enc = cfb8::Encryptor<aes::Aes128>;
type Dec = cfb8::Decryptor<aes::Aes128>;

/// The server key pair (1024-bit RSA like vanilla), created once.
pub struct ServerKey {
    key: RsaPrivateKey,
    /// X.509 SubjectPublicKeyInfo (DER), sent in the encryption request
    pub public_der: Vec<u8>,
}

pub fn server_key() -> &'static ServerKey {
    static KEY: OnceLock<ServerKey> = OnceLock::new();
    KEY.get_or_init(|| {
        let key = RsaPrivateKey::new(&mut rand::rngs::OsRng, 1024).expect("RSA key generation");
        let public_der = key.to_public_key().to_public_key_der().expect("public key DER").as_bytes().to_vec();
        ServerKey { key, public_der }
    })
}

impl ServerKey {
    pub fn decrypt(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        Ok(self.key.decrypt(Pkcs1v15Encrypt, data)?)
    }
}

/// `Crypt.digestData` as a Minecraft-style signed hex string.
pub fn server_hash(server_id: &str, secret: &[u8], public_der: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(server_id.as_bytes());
    h.update(secret);
    h.update(public_der);
    let mut d: [u8; 20] = h.finalize().into();
    let negative = d[0] & 0x80 != 0;
    if negative {
        // two's complement
        let mut carry = true;
        for b in d.iter_mut().rev() {
            *b = !*b;
            if carry {
                let (v, c) = b.overflowing_add(1);
                *b = v;
                carry = c;
            }
        }
    }
    let hex = hex::encode(d);
    let hex = hex.trim_start_matches('0');
    if negative {
        format!("-{hex}")
    } else if hex.is_empty() {
        "0".into()
    } else {
        hex.to_string()
    }
}

/// A verified profile from the session server.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub uuid: uuid::Uuid,
    pub name: String,
    /// (name, value, signature)
    pub properties: Vec<(String, String, Option<String>)>,
}

/// `MinecraftSessionService.hasJoinedServer` (blocking HTTP).
pub fn has_joined(name: &str, hash: &str) -> anyhow::Result<Option<Profile>> {
    let url = format!("https://sessionserver.mojang.com/session/minecraft/hasJoined?username={name}&serverId={hash}");
    let mut resp = ureq::get(&url).call()?;
    if resp.status() != 200 {
        return Ok(None);
    }
    let v: serde_json::Value = serde_json::from_str(&resp.body_mut().read_to_string()?)?;
    let id = v["id"].as_str().unwrap_or_default();
    let uuid = uuid::Uuid::parse_str(id)?;
    let name = v["name"].as_str().unwrap_or(name).to_string();
    let properties = v["properties"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["name"].as_str()?.to_string(), p["value"].as_str()?.to_string(), p["signature"].as_str().map(str::to_string))))
        .take(16)
        .collect();
    Ok(Some(Profile { uuid, name, properties }))
}

/// A stream with optional AES/CFB8 encryption in both directions.
pub struct CryptStream<S> {
    inner: S,
    cipher: Option<(Enc, Dec)>,
    /// encrypted bytes not written to `inner` yet
    pending: Vec<u8>,
}

impl<S> CryptStream<S> {
    pub fn new(inner: S) -> Self {
        CryptStream { inner, cipher: None, pending: Vec::new() }
    }

    /// Turns encryption on with the shared secret (key = IV, like vanilla).
    pub fn enable(&mut self, secret: &[u8; 16]) {
        let enc = Enc::new(secret.into(), secret.into());
        let dec = Dec::new(secret.into(), secret.into());
        self.cipher = Some((enc, dec));
    }
}

impl<S: AsyncWrite + Unpin> CryptStream<S> {
    fn drain(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        while !self.pending.is_empty() {
            match Pin::new(&mut self.inner).poll_write(cx, &self.pending) {
                Poll::Ready(Ok(0)) => return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into())),
                Poll::Ready(Ok(n)) => {
                    self.pending.drain(..n);
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for CryptStream<S> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let r = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let (Poll::Ready(Ok(())), Some((_, dec))) = (&r, &mut this.cipher) {
            decrypt_in_place(dec, &mut buf.filled_mut()[before..]);
        }
        r
    }
}

fn decrypt_in_place(dec: &mut Dec, data: &mut [u8]) {
    // CFB8 keeps its state between calls: one stream, any chunking
    use aes::cipher::BlockDecryptMut;
    for b in data.iter_mut() {
        let mut blk = [*b].into();
        dec.decrypt_block_mut(&mut blk);
        *b = blk[0];
    }
}

fn encrypt_in_place(enc: &mut Enc, data: &mut [u8]) {
    use aes::cipher::BlockEncryptMut;
    for b in data.iter_mut() {
        let mut blk = [*b].into();
        enc.encrypt_block_mut(&mut blk);
        *b = blk[0];
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CryptStream<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.cipher.is_none() && this.pending.is_empty() {
            return Pin::new(&mut this.inner).poll_write(cx, buf);
        }
        // accept new data only once the previous chunk is out
        match this.drain(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }
        let start = this.pending.len();
        this.pending.extend_from_slice(buf);
        if let Some((enc, _)) = &mut this.cipher {
            encrypt_in_place(enc, &mut this.pending[start..]);
        }
        if let Poll::Ready(Err(e)) = this.drain(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        match this.drain(cx) {
            Poll::Ready(Ok(())) => Pin::new(&mut this.inner).poll_flush(cx),
            o => o,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        match this.drain(cx) {
            Poll::Ready(Ok(())) => Pin::new(&mut this.inner).poll_shutdown(cx),
            o => o,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_hash_matches_vanilla_examples() {
        // well-known examples of the Minecraft hex digest
        let d = |s: &str| server_hash(s, &[], &[]);
        assert_eq!(d("Notch"), "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48");
        assert_eq!(d("jeb_"), "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1");
        assert_eq!(d("simon"), "88e16a1019277b15d58faf0541e11910eb756f6");
    }

    #[test]
    fn cfb8_round_trip_any_chunking() {
        let key = [7u8; 16];
        let mut e = Enc::new(&key.into(), &key.into());
        let mut d = Dec::new(&key.into(), &key.into());
        let msg: Vec<u8> = (0..200u32).map(|i| (i * 31) as u8).collect();
        let mut c = msg.clone();
        encrypt_in_place(&mut e, &mut c[..50]);
        encrypt_in_place(&mut e, &mut c[50..]);
        decrypt_in_place(&mut d, &mut c[..7]);
        decrypt_in_place(&mut d, &mut c[7..]);
        assert_eq!(c, msg);
    }
}
