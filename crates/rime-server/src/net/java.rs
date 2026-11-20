//! Java Edition listener: handshake, status (server list), ping, login start.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use super::codec::{frame, read_packet, write_string, Reader};
use crate::ServerState;

pub use crate::registry::PROTOCOL_VERSION;
pub const VERSION_NAME: &str = "RimeMC 26.1";

/// Per-IP connection accounting (anti connection-flood / slowloris).
#[derive(Default)]
pub struct IpLimiter {
    inner: Mutex<HashMap<IpAddr, (u32, Instant, u32)>>, // (open, window start, new in window)
}

impl IpLimiter {
    pub fn try_acquire(self: &Arc<Self>, ip: IpAddr, max_open: u32, max_rate: u32) -> Option<IpGuard> {
        let mut m = self.inner.lock();
        let now = Instant::now();
        if m.len() > 100_000 {
            m.retain(|_, v| v.0 > 0);
        }
        let e = m.entry(ip).or_insert((0, now, 0));
        if now.duration_since(e.1) >= Duration::from_secs(1) {
            e.1 = now;
            e.2 = 0;
        }
        if e.0 >= max_open || e.2 >= max_rate {
            return None;
        }
        e.0 += 1;
        e.2 += 1;
        Some(IpGuard { limiter: self.clone(), ip })
    }
}

pub struct IpGuard {
    limiter: Arc<IpLimiter>,
    ip: IpAddr,
}

impl Drop for IpGuard {
    fn drop(&mut self) {
        let mut m = self.limiter.inner.lock();
        if let Some(e) = m.get_mut(&self.ip) {
            e.0 = e.0.saturating_sub(1);
        }
    }
}

pub async fn listen(state: Arc<ServerState>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(&state.cfg.server.bind).await?;
    tracing::info!("Java Edition listening on {}", state.cfg.server.bind);
    let limiter = Arc::new(IpLimiter::default());
    loop {
        let (sock, addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Some(guard) = limiter.try_acquire(addr.ip(), state.cfg.server.max_connections_per_ip, state.cfg.server.connection_rate_per_ip) else {
            drop(sock);
            continue;
        };
        let _ = sock.set_nodelay(true);
        let st = state.clone();
        tokio::spawn(async move {
            let _guard = guard;
            // every pre-play phase has its own timeout inside `handle`
            if let Err(e) = handle(sock, st).await {
                tracing::debug!("{addr}: {e:#}");
            }
        });
    }
}

fn json_text(msg: &str) -> String {
    serde_json::json!({ "text": msg }).to_string()
}

async fn handle(mut sock: TcpStream, state: Arc<ServerState>) -> anyhow::Result<()> {
    let pkt = timeout(Duration::from_secs(10), read_packet(&mut sock, 1024)).await??;
    let mut r = Reader::new(&pkt);
    anyhow::ensure!(r.varint()? == 0x00, "expected handshake");
    let protocol = r.varint()?;
    let _host = r.string(255)?;
    let _port = r.u16()?;
    let next = r.varint()?;
    match next {
        1 => timeout(Duration::from_secs(15), status(sock, state, protocol)).await?,
        2 | 3 => login(sock, state, protocol).await,
        _ => anyhow::bail!("invalid next state {next}"),
    }
}

async fn status(mut sock: TcpStream, state: Arc<ServerState>, client_protocol: i32) -> anyhow::Result<()> {
    let pkt = read_packet(&mut sock, 16).await?;
    anyhow::ensure!(Reader::new(&pkt).varint()? == 0x00, "expected status request");
    let proto = if client_protocol == PROTOCOL_VERSION { client_protocol } else { PROTOCOL_VERSION };
    let body = serde_json::json!({
        "version": { "name": VERSION_NAME, "protocol": proto },
        "players": { "max": state.cfg.server.max_players, "online": state.online_players(), "sample": [] },
        "description": { "text": state.cfg.server.motd },
        "enforcesSecureChat": false,
    })
    .to_string();
    let mut out = Vec::new();
    write_string(&mut out, &body);
    sock.write_all(&frame(0x00, &out)).await?;
    // ping (optional)
    if let Ok(Ok(pkt)) = timeout(Duration::from_secs(5), read_packet(&mut sock, 16)).await {
        let mut r = Reader::new(&pkt);
        if r.varint()? == 0x01 {
            let payload = r.i64()?;
            sock.write_all(&frame(0x01, &payload.to_be_bytes())).await?;
        }
    }
    Ok(())
}

pub fn valid_username(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Offline-mode UUID, identical to vanilla (`OfflinePlayer:<name>`, MD5 v3).
pub fn offline_uuid(name: &str) -> uuid::Uuid {
    use md5::{Digest, Md5};
    let mut h: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}").as_bytes()).into();
    h[6] = (h[6] & 0x0f) | 0x30;
    h[8] = (h[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(h)
}

async fn login(mut sock: TcpStream, state: Arc<ServerState>, protocol: i32) -> anyhow::Result<()> {
    let pkt = timeout(Duration::from_secs(10), read_packet(&mut sock, 256)).await??;
    let mut r = Reader::new(&pkt);
    anyhow::ensure!(r.varint()? == 0x00, "expected login start");
    let name = r.string(16)?.to_owned();
    let _client_uuid = r.uuid()?; // ignored in offline mode (derived from the name like vanilla)
    let disconnect = |msg: String| {
        let mut out = Vec::new();
        write_string(&mut out, &json_text(&msg));
        frame(0x00, &out)
    };
    if !valid_username(&name) {
        sock.write_all(&disconnect("Invalid username".into())).await?;
        return Ok(());
    }
    if protocol != PROTOCOL_VERSION {
        sock.write_all(&disconnect(format!("Please use Minecraft {}", VERSION_NAME.trim_start_matches("RimeMC ")))).await?;
        return Ok(());
    }
    if state.online_players() >= state.cfg.server.max_players {
        sock.write_all(&disconnect("The server is full!".into())).await?;
        return Ok(());
    }
    if let Some(left) = state.banned_for(&name) {
        sock.write_all(&disconnect(format!("You are temporarily banned ({} min left)", left.div_ceil(60)))).await?;
        return Ok(());
    }
    // offline mode: same UUID derivation as vanilla
    super::play::run(sock, state, name.clone(), offline_uuid(&name)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_uuid_matches_vanilla() {
        // vanilla: UUID.nameUUIDFromBytes("OfflinePlayer:Notch")
        assert_eq!(offline_uuid("Notch").to_string(), "b50ad385-829d-3141-a216-7e7d7539ba7f");
    }

    #[test]
    fn usernames() {
        assert!(valid_username("Steve_123"));
        assert!(!valid_username(""));
        assert!(!valid_username("a b"));
        assert!(!valid_username("§cAdmin"));
        assert!(!valid_username("abcdefghijklmnopq"));
    }

    #[test]
    fn ip_limiter() {
        let l = Arc::new(IpLimiter::default());
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let a = l.try_acquire(ip, 2, 10).unwrap();
        let _b = l.try_acquire(ip, 2, 10).unwrap();
        assert!(l.try_acquire(ip, 2, 10).is_none());
        drop(a);
        assert!(l.try_acquire(ip, 2, 10).is_some());
    }
}
