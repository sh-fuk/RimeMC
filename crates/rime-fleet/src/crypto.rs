//! Mutually authenticated, encrypted channel between RimeRouter and RimeNodes.
//!
//! ## Handshake
//! 1. Each side sends `Hello { role, node_id, nonce: [u8; 32] }` in the clear.
//! 2. Both derive keys with HKDF-SHA256:
//!    `ikm = shared secret`, `salt = nonce_router ‖ nonce_node`,
//!    separate keys for router→node, node→router and an auth key.
//! 3. Each side sends `HMAC-SHA256(auth_key, role ‖ transcript)` and verifies the
//!    peer's tag in constant time. A peer without the secret cannot produce it,
//!    and fresh nonces on both sides prevent replaying an old handshake.
//!
//! ## Records
//! `u32 length ‖ ChaCha20-Poly1305(key_dir, nonce = 0u32 ‖ seq_u64, payload)`.
//! The sequence number is implicit and strictly increasing per direction, so
//! replayed, reordered, dropped or forged records fail authentication and the
//! connection is torn down.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

/// Minimum accepted length of the shared secret.
pub const MIN_SECRET_LEN: usize = 32;
/// Maximum plaintext size of one record (chunks are the largest payload).
pub const MAX_RECORD: usize = 16 * 1024 * 1024;
pub const TAG_LEN: usize = 16;
pub const HELLO_LEN: usize = 1 + 8 + 32;
pub const AUTH_LEN: usize = 32;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("fleet secret must be at least {MIN_SECRET_LEN} bytes")]
    WeakSecret,
    #[error("malformed handshake")]
    BadHandshake,
    #[error("peer failed authentication (wrong secret?)")]
    AuthFailed,
    #[error("record authentication failed (tampered, replayed or reordered)")]
    BadRecord,
    #[error("record too large")]
    TooLarge,
    #[error("sequence number exhausted; reconnect required")]
    SeqExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
    Router = 1,
    Node = 2,
}

/// Handshake state for one side.
pub struct Handshake {
    secret: Vec<u8>,
    role: Role,
    node_id: u64,
    nonce: [u8; 32],
    peer: Option<(u64, [u8; 32])>,
    keys: Option<Keys>,
}

struct Keys {
    send: [u8; 32],
    recv: [u8; 32],
    auth: [u8; 32],
    transcript: Vec<u8>,
}

impl Handshake {
    pub fn new(secret: &[u8], role: Role, node_id: u64) -> Result<Self, CryptoError> {
        if secret.len() < MIN_SECRET_LEN {
            return Err(CryptoError::WeakSecret);
        }
        let mut nonce = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        Ok(Self { secret: secret.to_vec(), role, node_id, nonce, peer: None, keys: None })
    }

    /// Our hello message.
    pub fn hello(&self) -> [u8; HELLO_LEN] {
        let mut out = [0u8; HELLO_LEN];
        out[0] = self.role as u8;
        out[1..9].copy_from_slice(&self.node_id.to_le_bytes());
        out[9..].copy_from_slice(&self.nonce);
        out
    }

    /// Process the peer's hello, derive keys, and return our auth tag.
    pub fn on_peer_hello(&mut self, msg: &[u8]) -> Result<[u8; AUTH_LEN], CryptoError> {
        if msg.len() != HELLO_LEN || self.peer.is_some() {
            return Err(CryptoError::BadHandshake);
        }
        let expected_peer_role = match self.role {
            Role::Router => Role::Node,
            Role::Node => Role::Router,
        };
        if msg[0] != expected_peer_role as u8 {
            return Err(CryptoError::BadHandshake);
        }
        let peer_id = u64::from_le_bytes(msg[1..9].try_into().unwrap());
        let mut peer_nonce = [0u8; 32];
        peer_nonce.copy_from_slice(&msg[9..]);
        if peer_nonce == self.nonce {
            // reflection attack: our own hello bounced back
            return Err(CryptoError::BadHandshake);
        }
        self.peer = Some((peer_id, peer_nonce));

        let (router_hello, node_hello) = match self.role {
            Role::Router => (self.hello().to_vec(), msg.to_vec()),
            Role::Node => (msg.to_vec(), self.hello().to_vec()),
        };
        let mut transcript = router_hello.clone();
        transcript.extend_from_slice(&node_hello);
        let salt = [&router_hello[9..], &node_hello[9..]].concat();
        let hk = Hkdf::<Sha256>::new(Some(&salt), &self.secret);
        let mut r2n = [0u8; 32];
        let mut n2r = [0u8; 32];
        let mut auth = [0u8; 32];
        hk.expand(b"rime-fleet v1 router->node", &mut r2n).map_err(|_| CryptoError::BadHandshake)?;
        hk.expand(b"rime-fleet v1 node->router", &mut n2r).map_err(|_| CryptoError::BadHandshake)?;
        hk.expand(b"rime-fleet v1 auth", &mut auth).map_err(|_| CryptoError::BadHandshake)?;
        let (send, recv) = match self.role {
            Role::Router => (r2n, n2r),
            Role::Node => (n2r, r2n),
        };
        let tag = auth_tag(&auth, self.role as u8, &transcript);
        self.keys = Some(Keys { send, recv, auth, transcript });
        Ok(tag)
    }

    /// Verify the peer's auth tag and produce the secure channel.
    pub fn finish(self, peer_tag: &[u8]) -> Result<(SecureChannel, u64), CryptoError> {
        let keys = self.keys.ok_or(CryptoError::BadHandshake)?;
        let (peer_id, _) = self.peer.ok_or(CryptoError::BadHandshake)?;
        let peer_role = match self.role {
            Role::Router => Role::Node,
            Role::Node => Role::Router,
        } as u8;
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&keys.auth).map_err(|_| CryptoError::BadHandshake)?;
        mac.update(&[peer_role]);
        mac.update(&keys.transcript);
        // constant-time comparison
        mac.verify_slice(peer_tag).map_err(|_| CryptoError::AuthFailed)?;
        Ok((
            SecureChannel {
                send: ChaCha20Poly1305::new(Key::from_slice(&keys.send)),
                recv: ChaCha20Poly1305::new(Key::from_slice(&keys.recv)),
                send_seq: 0,
                recv_seq: 0,
            },
            peer_id,
        ))
    }
}

fn auth_tag(key: &[u8; 32], role: u8, transcript: &[u8]) -> [u8; AUTH_LEN] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac key");
    mac.update(&[role]);
    mac.update(transcript);
    mac.finalize().into_bytes().into()
}

/// MAC for a router → node player forward (`PlayerForward` on the game port).
pub fn forward_mac(secret: &[u8], payload: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(b"rime-fleet-forward"), secret);
    let mut key = [0u8; 32];
    hk.expand(b"player forward v1", &mut key).expect("hkdf length");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key).expect("hmac key");
    mac.update(payload);
    mac.finalize().into_bytes().into()
}

/// Constant-time check of a forward MAC.
pub fn verify_forward(secret: &[u8], payload: &[u8], tag: &[u8]) -> bool {
    let hk = Hkdf::<Sha256>::new(Some(b"rime-fleet-forward"), secret);
    let mut key = [0u8; 32];
    hk.expand(b"player forward v1", &mut key).expect("hkdf length");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key).expect("hmac key");
    mac.update(payload);
    mac.verify_slice(tag).is_ok()
}

/// Established channel. `seal`/`open` must be called in stream order.
pub struct SecureChannel {
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
    send_seq: u64,
    recv_seq: u64,
}

fn nonce_for(seq: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&seq.to_le_bytes());
    n
}

impl SecureChannel {
    /// Encrypt a payload into a framed record (`u32 len ‖ ciphertext`).
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if plaintext.len() > MAX_RECORD {
            return Err(CryptoError::TooLarge);
        }
        if self.send_seq == u64::MAX {
            return Err(CryptoError::SeqExhausted);
        }
        let seq = self.send_seq;
        self.send_seq += 1;
        let aad = seq.to_le_bytes();
        let ct = self
            .send
            .encrypt(Nonce::from_slice(&nonce_for(seq)), Payload { msg: plaintext, aad: &aad })
            .map_err(|_| CryptoError::BadRecord)?;
        let mut out = Vec::with_capacity(4 + ct.len());
        out.extend_from_slice(&(ct.len() as u32).to_le_bytes());
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Validate a record length prefix before reading the body from the socket.
    pub fn check_len(len: u32) -> Result<usize, CryptoError> {
        let len = len as usize;
        if !(TAG_LEN..=MAX_RECORD + TAG_LEN).contains(&len) {
            return Err(CryptoError::TooLarge);
        }
        Ok(len)
    }

    /// Decrypt a record body (without the length prefix).
    pub fn open(&mut self, body: &[u8]) -> Result<Vec<u8>, CryptoError> {
        Self::check_len(body.len() as u32)?;
        if self.recv_seq == u64::MAX {
            return Err(CryptoError::SeqExhausted);
        }
        let seq = self.recv_seq;
        let aad = seq.to_le_bytes();
        let pt = self
            .recv
            .decrypt(Nonce::from_slice(&nonce_for(seq)), Payload { msg: body, aad: &aad })
            .map_err(|_| CryptoError::BadRecord)?;
        self.recv_seq += 1;
        Ok(pt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &[u8] = b"0123456789abcdef0123456789abcdef-secret";

    fn pair(rs: &[u8], ns: &[u8]) -> Result<(SecureChannel, SecureChannel), CryptoError> {
        let mut r = Handshake::new(rs, Role::Router, 0)?;
        let mut n = Handshake::new(ns, Role::Node, 7)?;
        let rh = r.hello();
        let nh = n.hello();
        let rt = r.on_peer_hello(&nh)?;
        let nt = n.on_peer_hello(&rh)?;
        let (rc, id) = r.finish(&nt)?;
        assert_eq!(id, 7);
        let (nc, _) = n.finish(&rt)?;
        Ok((rc, nc))
    }

    #[test]
    fn roundtrip() {
        let (mut r, mut n) = pair(S, S).unwrap();
        for i in 0..5u8 {
            let rec = r.seal(&[i; 100]).unwrap();
            assert_eq!(n.open(&rec[4..]).unwrap(), vec![i; 100]);
            let rec = n.seal(b"pong").unwrap();
            assert_eq!(r.open(&rec[4..]).unwrap(), b"pong");
        }
    }

    #[test]
    fn wrong_secret_rejected() {
        let other = b"ffffffffffffffffffffffffffffffffffffffff";
        assert_eq!(pair(S, other).err(), Some(CryptoError::AuthFailed));
    }

    #[test]
    fn weak_secret_rejected() {
        assert!(Handshake::new(b"short", Role::Router, 0).is_err());
    }

    #[test]
    fn replay_and_tamper_rejected() {
        let (mut r, mut n) = pair(S, S).unwrap();
        let a = r.seal(b"first").unwrap();
        let b = r.seal(b"second").unwrap();
        // reordering
        assert_eq!(n.open(&b[4..]), Err(CryptoError::BadRecord));
        assert!(n.open(&a[4..]).is_ok());
        // replay
        assert_eq!(n.open(&a[4..]), Err(CryptoError::BadRecord));
        // tamper
        let mut c = r.seal(b"third").unwrap();
        c[6] ^= 1;
        let (mut r2, mut n2) = pair(S, S).unwrap();
        let mut d = r2.seal(b"x").unwrap();
        d[5] ^= 1;
        assert_eq!(n2.open(&d[4..]), Err(CryptoError::BadRecord));
    }

    #[test]
    fn reflection_rejected() {
        let mut r = Handshake::new(S, Role::Router, 0).unwrap();
        let h = r.hello();
        assert!(r.on_peer_hello(&h).is_err());
    }
}
