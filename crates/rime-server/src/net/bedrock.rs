//! Built-in Bedrock Edition endpoint (RakNet over UDP).
//!
//! Implemented: RakNet offline discovery (Unconnected Ping/Pong) with the
//! Bedrock MOTD format, so the server appears in the Bedrock server list with
//! live player counts, plus per-IP rate limiting.
//! The connected RakNet session layer and the Bedrock↔Java translation layer
//! follow the play-state protocol milestone.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use crate::ServerState;

pub const RAKNET_MAGIC: [u8; 16] = [0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78];
const ID_UNCONNECTED_PING: u8 = 0x01;
const ID_UNCONNECTED_PING_OPEN: u8 = 0x02;
const ID_UNCONNECTED_PONG: u8 = 0x1c;

/// Strip characters that would break the `;`-separated MOTD format.
fn sanitize(s: &str) -> String {
    s.chars().filter(|c| *c != ';' && !c.is_control()).take(64).collect()
}

pub fn motd_string(state: &ServerState, guid: u64, port: u16) -> String {
    let b = &state.cfg.bedrock;
    format!(
        "MCPE;{};{};{};{};{};{};{};Survival;1;{};{};0;",
        sanitize(&b.motd1),
        b.protocol_version,
        sanitize(&b.game_version),
        state.online_players(),
        state.cfg.server.max_players,
        guid,
        sanitize(&b.motd2),
        port,
        port
    )
}

/// Parse an unconnected ping and build the pong. Returns `None` for anything
/// that is not a well-formed ping (never panics on hostile input).
pub fn handle_ping(pkt: &[u8], server_guid: u64, motd: &str) -> Option<Vec<u8>> {
    if pkt.len() < 1 + 8 + 16 || !matches!(pkt[0], ID_UNCONNECTED_PING | ID_UNCONNECTED_PING_OPEN) {
        return None;
    }
    let time = &pkt[1..9];
    if pkt[9..25] != RAKNET_MAGIC {
        return None;
    }
    let motd = motd.as_bytes();
    let mlen = motd.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(35 + mlen);
    out.push(ID_UNCONNECTED_PONG);
    out.extend_from_slice(time);
    out.extend_from_slice(&server_guid.to_be_bytes());
    out.extend_from_slice(&RAKNET_MAGIC);
    out.extend_from_slice(&(mlen as u16).to_be_bytes());
    out.extend_from_slice(&motd[..mlen]);
    Some(out)
}

pub async fn listen(state: Arc<ServerState>) -> anyhow::Result<()> {
    let sock = UdpSocket::bind(&state.cfg.bedrock.bind).await?;
    let port: u16 = sock.local_addr()?.port();
    tracing::info!("Bedrock Edition listening on {} (UDP)", state.cfg.bedrock.bind);
    let guid: u64 = rand::random();
    let mut buf = vec![0u8; 2048];
    let mut rate: HashMap<IpAddr, (Instant, u32)> = HashMap::new();
    let mut cached: (Instant, String) = (Instant::now() - Duration::from_secs(10), String::new());
    loop {
        let (n, from): (usize, SocketAddr) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        // 20 packets/s per IP for the offline handshake; prevents amplification abuse
        let now = Instant::now();
        if rate.len() > 50_000 {
            rate.retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(1));
        }
        let e = rate.entry(from.ip()).or_insert((now, 0));
        if now.duration_since(e.0) >= Duration::from_secs(1) {
            *e = (now, 0);
        }
        e.1 += 1;
        if e.1 > 20 {
            continue;
        }
        if now.duration_since(cached.0) > Duration::from_secs(1) {
            cached = (now, motd_string(&state, guid, port));
        }
        if let Some(pong) = handle_ping(&buf[..n], guid, &cached.1) {
            let _ = sock.send_to(&pong, from).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_pong() {
        let mut ping = vec![ID_UNCONNECTED_PING];
        ping.extend_from_slice(&42i64.to_be_bytes());
        ping.extend_from_slice(&RAKNET_MAGIC);
        ping.extend_from_slice(&7u64.to_be_bytes());
        let pong = handle_ping(&ping, 99, "MCPE;x;").unwrap();
        assert_eq!(pong[0], ID_UNCONNECTED_PONG);
        assert_eq!(&pong[1..9], &42i64.to_be_bytes());
        assert_eq!(&pong[9..17], &99u64.to_be_bytes());
        assert_eq!(&pong[33..35], &7u16.to_be_bytes());
        assert_eq!(&pong[35..], b"MCPE;x;");
        // garbage
        assert!(handle_ping(&[1, 2, 3], 1, "").is_none());
        let mut bad = ping.clone();
        bad[12] ^= 1;
        assert!(handle_ping(&bad, 1, "").is_none());
    }

    #[test]
    fn motd_sanitized() {
        assert_eq!(sanitize("a;b\nc"), "abc");
    }
}
