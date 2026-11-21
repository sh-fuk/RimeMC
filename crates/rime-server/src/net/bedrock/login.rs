//! Bedrock login: identity verification (Xbox/OIDC multiplayer token, the
//! legacy Mojang-signed chain, or self-signed in offline mode), the client
//! data token, and the ECDH handshake that turns on AES-256-CTR encryption.

use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aes::cipher::{KeyIvInit, StreamCipher};
use anyhow::{anyhow, bail, ensure, Context, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use p384::ecdsa::signature::{Signer, Verifier};
use p384::ecdsa::{Signature, SigningKey, VerifyingKey};
use p384::pkcs8::{DecodePublicKey, EncodePublicKey};
use parking_lot::Mutex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::codec::R;

/// Mojang's root key for the legacy login chain.
const MOJANG_KEY: &str = "MHYwEAYHKoZIzj0CAQYFK4EEACIDYgAECRXueJeTDqNRRgJi/vlRufByu/2G0i2Ebt6YMar5QX/R0DIIyrJMcUpruK4QveTfJSTp3Shlq4Gk34cD/4GUWwkv0DVuzeuB+tXija7HBxii03NHDbPAD0AKnLr2wdAp";
const DISCOVERY_URL: &str = "https://client.discovery.minecraft-services.net/api/v1.0/discovery/MinecraftPE/builds/";
const AUDIENCE: &str = "api://auth-minecraft-services/multiplayer";

/// Who logged in.
#[derive(Clone, Debug)]
pub struct Identity {
    pub name: String,
    pub xuid: String,
    pub uuid: uuid::Uuid,
    pub authenticated: bool,
    pub client_key: VerifyingKey,
    /// skin and device data (client data token claims)
    pub client: Value,
}

struct Jwt<'a> {
    header: Value,
    claims: Value,
    signed: &'a str,
    sig: Vec<u8>,
}

fn parse_jwt(t: &str) -> Result<Jwt<'_>> {
    ensure!(t.len() < 1 << 20, "token too large");
    let mut parts = t.split('.');
    let (Some(h), Some(c), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { bail!("malformed JWT") };
    let header = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h)?)?;
    let claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(c)?)?;
    let sig = URL_SAFE_NO_PAD.decode(s)?;
    Ok(Jwt { header, claims, signed: &t[..h.len() + 1 + c.len()], sig })
}

fn key_from_b64(s: &str) -> Result<VerifyingKey> {
    let der = STANDARD.decode(s.trim()).context("bad public key encoding")?;
    VerifyingKey::from_public_key_der(&der).map_err(|_| anyhow!("public key is not EC P-384"))
}

fn key_b64(k: &VerifyingKey) -> String {
    STANDARD.encode(k.to_public_key_der().expect("encodable key").as_bytes())
}

fn verify_es384(j: &Jwt, key: &VerifyingKey) -> Result<()> {
    ensure!(j.header["alg"] == "ES384", "JWT must use ES384");
    let sig = Signature::from_slice(&j.sig).map_err(|_| anyhow!("bad JWT signature"))?;
    key.verify(j.signed.as_bytes(), &sig).map_err(|_| anyhow!("JWT signature does not verify"))
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// exp/nbf with a minute of clock skew.
fn check_times(c: &Value) -> Result<()> {
    let now = now_secs();
    if let Some(exp) = c["exp"].as_i64() {
        ensure!(now <= exp + 60, "login token expired");
    }
    if let Some(nbf) = c["nbf"].as_i64() {
        ensure!(now + 60 >= nbf, "login token not valid yet");
    }
    Ok(())
}

fn x5u(j: &Jwt) -> Result<VerifyingKey> {
    key_from_b64(j.header["x5u"].as_str().ok_or_else(|| anyhow!("JWT without x5u"))?)
}

/// Parses and verifies a Login packet body (after the packet id).
/// Returns the protocol version and the verified identity.
pub fn verify_login(body: &[u8], allow_offline: bool, game_version: &str) -> Result<(i32, Identity)> {
    let mut r = R::new(body);
    let protocol = r.i32be()?;
    let tokens = r.bytes(4 << 20)?;
    let mut t = R::new(tokens);
    let identity = t.le_str(1 << 20)?;
    let client = t.le_str(2 << 20)?;
    let env: Value = serde_json::from_str(&identity).context("login identity is not JSON")?;
    if env["AuthenticationType"] == 1 {
        bail!("guest logins are not supported");
    }
    let token = env["Token"].as_str().unwrap_or("").trim_start_matches("MCToken ").to_string();
    let chain: Vec<String> = if let Some(cert) = env["Certificate"].as_str() {
        let c: Value = serde_json::from_str(cert)?;
        c["chain"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
    } else {
        env["chain"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    let mut id = if !token.is_empty() {
        multiplayer_token(&token, allow_offline, game_version)?
    } else {
        legacy_chain(&chain, allow_offline)?
    };
    let cd = parse_jwt(&client)?;
    verify_es384(&cd, &id.client_key).context("client data token")?;
    id.client = cd.claims;
    validate(&id)?;
    Ok((protocol, id))
}

fn validate(id: &Identity) -> Result<()> {
    let n = id.name.chars().count();
    ensure!((1..=32).contains(&n) && !id.name.chars().any(|c| c.is_control()), "invalid gamertag");
    if id.authenticated {
        ensure!(!id.xuid.is_empty() && id.xuid.len() <= 20 && id.xuid.bytes().all(|b| b.is_ascii_digit()), "invalid XUID");
    }
    Ok(())
}

fn legacy_chain(chain: &[String], allow_offline: bool) -> Result<Identity> {
    ensure!(chain.len() == 1 || chain.len() == 3, "unexpected login chain length {}", chain.len());
    let t0 = parse_jwt(&chain[0])?;
    verify_es384(&t0, &x5u(&t0)?)?;
    let next = key_from_b64(t0.claims["identityPublicKey"].as_str().unwrap_or(""))?;
    let (claims, key, authenticated) = if chain.len() == 1 {
        ensure!(allow_offline, "this server requires Xbox Live authentication");
        check_times(&t0.claims)?;
        (t0.claims.clone(), next, false)
    } else {
        ensure!(key_b64(&next) == MOJANG_KEY, "login chain is not signed by Mojang");
        let t1 = parse_jwt(&chain[1])?;
        verify_es384(&t1, &next)?;
        check_times(&t1.claims)?;
        let k1 = key_from_b64(t1.claims["identityPublicKey"].as_str().unwrap_or(""))?;
        let t2 = parse_jwt(&chain[2])?;
        verify_es384(&t2, &k1)?;
        check_times(&t2.claims)?;
        let k2 = key_from_b64(t2.claims["identityPublicKey"].as_str().unwrap_or(""))?;
        (t2.claims.clone(), k2, true)
    };
    let x = &claims["extraData"];
    Ok(Identity {
        name: x["displayName"].as_str().unwrap_or("").to_string(),
        xuid: if authenticated { x["XUID"].as_str().unwrap_or("").to_string() } else { String::new() },
        uuid: x["identity"].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok()).unwrap_or_default(),
        authenticated,
        client_key: key,
        client: Value::Null,
    })
}

fn multiplayer_token(token: &str, allow_offline: bool, game_version: &str) -> Result<Identity> {
    let j = parse_jwt(token)?;
    let self_signed = j.header["x5u"].is_string();
    if self_signed {
        ensure!(allow_offline, "this server requires Xbox Live authentication");
        verify_es384(&j, &x5u(&j)?)?;
    } else {
        verify_oidc(&j, game_version)?;
    }
    check_times(&j.claims)?;
    let c = &j.claims;
    let cpk = c["cpk"].as_str().or(j.header["x5u"].as_str()).ok_or_else(|| anyhow!("multiplayer token without client key"))?;
    let xuid = c["xid"].as_str().map(String::from).or(c["xid"].as_i64().map(|v| v.to_string())).unwrap_or_default();
    Ok(Identity {
        name: c["xname"].as_str().or(c["displayName"].as_str()).unwrap_or("").to_string(),
        xuid: if self_signed { String::new() } else { xuid },
        uuid: c["leguuid"].as_str().or(c["identity"].as_str()).and_then(|s| uuid::Uuid::parse_str(s).ok()).unwrap_or_default(),
        authenticated: !self_signed,
        client_key: key_from_b64(cpk)?,
        client: Value::Null,
    })
}

/// RS256 keys of the Minecraft services OIDC issuer (refreshed every 30 min).
struct Jwks {
    issuer: String,
    keys: Vec<Value>,
    at: Instant,
}
static JWKS: OnceLock<Mutex<Option<Jwks>>> = OnceLock::new();

fn http_json(url: &str) -> Result<Value> {
    ensure!(url.starts_with("https://"), "OIDC endpoints must use HTTPS");
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
    let body = agent.get(url).header("User-Agent", "RimeMC").call()?.body_mut().with_config().limit(1 << 20).read_to_string()?;
    Ok(serde_json::from_str(&body)?)
}

fn fetch_jwks(game_version: &str) -> Result<Jwks> {
    let d = http_json(&format!("{DISCOVERY_URL}{game_version}"))?;
    let mut issuer = d["result"]["serviceEnvironments"]["auth"]["prod"]["issuer"].as_str().ok_or_else(|| anyhow!("service discovery has no OIDC issuer"))?.to_string();
    if !issuer.ends_with('/') {
        issuer.push('/');
    }
    let conf = http_json(&format!("{issuer}.well-known/openid-configuration"))?;
    let jwks_uri = conf["jwks_uri"].as_str().ok_or_else(|| anyhow!("OIDC configuration without jwks_uri"))?;
    let keys = http_json(jwks_uri)?["keys"].as_array().cloned().unwrap_or_default();
    Ok(Jwks { issuer, keys, at: Instant::now() })
}

fn verify_oidc(j: &Jwt, game_version: &str) -> Result<()> {
    use rsa::pkcs1v15::{Signature as RsaSig, VerifyingKey as RsaKey};
    use rsa::signature::Verifier as _;
    ensure!(j.header["alg"] == "RS256", "multiplayer token must use RS256");
    let kid = j.header["kid"].as_str().ok_or_else(|| anyhow!("multiplayer token without kid"))?;
    let cell = JWKS.get_or_init(|| Mutex::new(None));
    let find = |c: &Option<Jwks>| c.as_ref().and_then(|k| k.keys.iter().find(|v| v["kid"] == kid).cloned().map(|v| (v, k.issuer.clone())));
    let mut found = find(&cell.lock());
    let stale = cell.lock().as_ref().is_none_or(|k| k.at.elapsed() > Duration::from_secs(1800));
    if found.is_none() || stale {
        let fresh = fetch_jwks(game_version)?;
        *cell.lock() = Some(fresh);
        found = find(&cell.lock());
    }
    let (jwk, issuer) = found.ok_or_else(|| anyhow!("unknown OIDC signing key"))?;
    let n = rsa::BigUint::from_bytes_be(&URL_SAFE_NO_PAD.decode(jwk["n"].as_str().unwrap_or(""))?);
    let e = rsa::BigUint::from_bytes_be(&URL_SAFE_NO_PAD.decode(jwk["e"].as_str().unwrap_or(""))?);
    let key = RsaKey::<Sha256>::new(rsa::RsaPublicKey::new(n, e)?);
    let sig = RsaSig::try_from(j.sig.as_slice())?;
    key.verify(j.signed.as_bytes(), &sig).map_err(|_| anyhow!("multiplayer token signature does not verify"))?;
    let aud_ok = match &j.claims["aud"] {
        Value::String(s) => s == AUDIENCE,
        Value::Array(a) => a.iter().any(|v| v == AUDIENCE),
        _ => false,
    };
    ensure!(aud_ok, "multiplayer token has the wrong audience");
    let iss = j.claims["iss"].as_str().unwrap_or("");
    ensure!(iss.trim_end_matches('/') == issuer.trim_end_matches('/'), "multiplayer token has the wrong issuer");
    ensure!(j.claims["exp"].is_i64(), "multiplayer token without expiry");
    Ok(())
}

/// AES-256-CTR both ways plus the SHA-256 checksum Bedrock appends.
pub struct Cipher {
    key: [u8; 32],
    enc: ctr::Ctr32BE<aes::Aes256>,
    dec: ctr::Ctr32BE<aes::Aes256>,
    send_counter: u64,
    recv_counter: u64,
}

impl Cipher {
    fn new(key: [u8; 32]) -> Cipher {
        let mut iv = [0u8; 16];
        iv[..12].copy_from_slice(&key[..12]);
        iv[15] = 2; // GCM's first counter block, run as plain CTR
        Cipher {
            key,
            enc: ctr::Ctr32BE::<aes::Aes256>::new(&key.into(), &iv.into()),
            dec: ctr::Ctr32BE::<aes::Aes256>::new(&key.into(), &iv.into()),
            send_counter: 0,
            recv_counter: 0,
        }
    }

    fn checksum(&self, counter: u64, data: &[u8]) -> [u8; 8] {
        let mut h = Sha256::new();
        h.update(counter.to_le_bytes());
        h.update(data);
        h.update(self.key);
        h.finalize()[..8].try_into().unwrap()
    }

    pub fn encrypt(&mut self, mut data: Vec<u8>) -> Vec<u8> {
        let sum = self.checksum(self.send_counter, &data);
        self.send_counter += 1;
        data.extend_from_slice(&sum);
        self.enc.apply_keystream(&mut data);
        data
    }

    pub fn decrypt(&mut self, data: &mut Vec<u8>) -> Result<()> {
        self.dec.apply_keystream(data);
        ensure!(data.len() >= 8, "encrypted packet too short");
        let body = data.len() - 8;
        let sum = self.checksum(self.recv_counter, &data[..body]);
        self.recv_counter += 1;
        ensure!(sum[..] == data[body..], "encrypted packet checksum mismatch");
        data.truncate(body);
        Ok(())
    }
}

/// The server half of the key exchange: the handshake JWT to send, and the
/// cipher to switch on right after sending it.
pub fn server_handshake(client: &VerifyingKey) -> Result<(String, Cipher)> {
    let secret = p384::SecretKey::random(&mut rand::rngs::OsRng);
    let signing = SigningKey::from(secret.clone());
    let public = key_b64(signing.verifying_key());
    let salt: [u8; 16] = rand::random();
    let shared = p384::ecdh::diffie_hellman(secret.to_nonzero_scalar(), client.as_affine());
    let mut h = Sha256::new();
    h.update(salt);
    h.update(shared.raw_secret_bytes());
    let key: [u8; 32] = h.finalize().into();
    let header = serde_json::json!({ "alg": "ES384", "x5u": public });
    let claims = serde_json::json!({ "salt": STANDARD.encode(salt), "signedToken": public });
    let signed = format!("{}.{}", URL_SAFE_NO_PAD.encode(header.to_string()), URL_SAFE_NO_PAD.encode(claims.to_string()));
    let sig: Signature = signing.sign(signed.as_bytes());
    Ok((format!("{signed}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes())), Cipher::new(key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(key: &SigningKey, header: Value, claims: Value) -> String {
        let s = format!("{}.{}", URL_SAFE_NO_PAD.encode(header.to_string()), URL_SAFE_NO_PAD.encode(claims.to_string()));
        let sig: Signature = key.sign(s.as_bytes());
        format!("{s}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
    }

    fn login_body(identity: &str, client: &str) -> Vec<u8> {
        let mut tokens = Vec::new();
        tokens.extend_from_slice(&(identity.len() as i32).to_le_bytes());
        tokens.extend_from_slice(identity.as_bytes());
        tokens.extend_from_slice(&(client.len() as i32).to_le_bytes());
        tokens.extend_from_slice(client.as_bytes());
        let mut w = super::super::codec::W::new();
        w.i32be(2225).bytes(&tokens);
        w.0
    }

    #[test]
    fn offline_chain_and_encryption() {
        let key = SigningKey::random(&mut rand::rngs::OsRng);
        let pubk = key_b64(key.verifying_key());
        let h = serde_json::json!({ "alg": "ES384", "x5u": pubk });
        let chain = sign(&key, h.clone(), serde_json::json!({ "identityPublicKey": pubk, "extraData": { "displayName": "Steve Bed", "identity": "8f1e7c6a-1111-4222-8333-444455556666", "XUID": "" } }));
        let client = sign(&key, h, serde_json::json!({ "SkinId": "x" }));
        let identity = serde_json::json!({ "AuthenticationType": 2, "Certificate": serde_json::json!({ "chain": [chain] }).to_string(), "Token": "" }).to_string();
        let body = login_body(&identity, &client);
        assert!(verify_login(&body, false, "1.26.60").is_err(), "offline chain refused in online mode");
        let (proto, id) = verify_login(&body, true, "1.26.60").unwrap();
        assert_eq!(proto, 2225);
        assert_eq!(id.name, "Steve Bed");
        assert!(!id.authenticated);
        // tampered client data
        let bad = login_body(&identity, &sign(&SigningKey::random(&mut rand::rngs::OsRng), serde_json::json!({ "alg": "ES384" }), serde_json::json!({})));
        assert!(verify_login(&bad, true, "1.26.60").is_err());

        // the client derives the same key from our handshake token
        let (token, mut server) = server_handshake(&id.client_key).unwrap();
        let j = parse_jwt(&token).unwrap();
        let spub = x5u(&j).unwrap();
        verify_es384(&j, &spub).unwrap();
        let salt = STANDARD.decode(j.claims["salt"].as_str().unwrap()).unwrap();
        let shared = p384::ecdh::diffie_hellman(p384::SecretKey::from(key.clone()).to_nonzero_scalar(), spub.as_affine());
        let mut hh = Sha256::new();
        hh.update(&salt);
        hh.update(shared.raw_secret_bytes());
        let mut client_side = Cipher::new(hh.finalize().into());
        for msg in [b"hello".to_vec(), vec![9u8; 3000]] {
            let mut wire = server.encrypt(msg.clone());
            client_side.decrypt(&mut wire).unwrap();
            assert_eq!(wire, msg);
        }
        let mut wire = server.encrypt(b"x".to_vec());
        wire[0] ^= 1;
        assert!(client_side.decrypt(&mut wire).is_err());
    }
}
