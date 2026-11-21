//! Built-in Bedrock Edition endpoint (RakNet over UDP): server-list
//! discovery (Unconnected Ping/Pong with the Bedrock MOTD), the RakNet
//! connection handshake, and one [`session`] per connected player.

pub mod codec;
pub mod login;
pub mod mapping;
pub mod raknet;
pub mod session;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

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

/// Reply to OpenConnectionRequest 1/2 (`None` for anything malformed).
fn open_connection(pkt: &[u8], guid: u64, from: SocketAddr, max_mtu: usize) -> Option<(Vec<u8>, Option<usize>)> {
    match *pkt.first()? {
        raknet::ID_OPEN_CONNECTION_REQUEST_1 => {
            if pkt.len() < 18 || pkt[1..17] != RAKNET_MAGIC {
                return None;
            }
            let mut o = Vec::with_capacity(28);
            if !(10..=raknet::RAKNET_PROTOCOL).contains(&pkt[17]) {
                o.push(raknet::ID_INCOMPATIBLE_PROTOCOL);
                o.push(raknet::RAKNET_PROTOCOL);
                o.extend_from_slice(&RAKNET_MAGIC);
                o.extend_from_slice(&guid.to_be_bytes());
                return Some((o, None));
            }
            // the request is padded to the client's MTU (+ IP/UDP headers)
            let mtu = (pkt.len() + 28).min(max_mtu);
            o.push(raknet::ID_OPEN_CONNECTION_REPLY_1);
            o.extend_from_slice(&RAKNET_MAGIC);
            o.extend_from_slice(&guid.to_be_bytes());
            o.push(0);
            o.extend_from_slice(&(mtu as u16).to_be_bytes());
            Some((o, None))
        }
        raknet::ID_OPEN_CONNECTION_REQUEST_2 => {
            if pkt.len() < 17 || pkt[1..17] != RAKNET_MAGIC {
                return None;
            }
            let rest = &pkt[17..];
            let addr_len = match rest.first()? {
                4 => 7,
                6 => 29,
                _ => return None,
            };
            if rest.len() < addr_len + 10 {
                return None;
            }
            let mtu = u16::from_be_bytes([rest[addr_len], rest[addr_len + 1]]) as usize;
            let mtu = mtu.clamp(576, max_mtu);
            let mut o = Vec::with_capacity(40);
            o.push(raknet::ID_OPEN_CONNECTION_REPLY_2);
            o.extend_from_slice(&RAKNET_MAGIC);
            o.extend_from_slice(&guid.to_be_bytes());
            raknet::write_address(&mut o, from);
            o.extend_from_slice(&(mtu as u16).to_be_bytes());
            o.push(0);
            Some((o, Some(mtu)))
        }
        _ => None,
    }
}

pub async fn listen(state: Arc<ServerState>) -> anyhow::Result<()> {
    let sock = Arc::new(UdpSocket::bind(&state.cfg.bedrock.bind).await?);
    let port: u16 = sock.local_addr()?.port();
    tracing::info!("Bedrock Edition listening on {} (UDP, protocol {} / {})", state.cfg.bedrock.bind, state.cfg.bedrock.protocol_version, state.cfg.bedrock.game_version);
    let guid: u64 = rand::random();
    let max_mtu = (state.cfg.bedrock.mtu as usize).clamp(576, 1500);
    let mut buf = vec![0u8; 2048];
    let mut rate: HashMap<IpAddr, (Instant, u32)> = HashMap::new();
    let mut cached: (Instant, String) = (Instant::now() - Duration::from_secs(10), String::new());
    let mut sessions: HashMap<SocketAddr, mpsc::Sender<Vec<u8>>> = HashMap::new();
    let (gone_tx, mut gone_rx) = mpsc::unbounded_channel::<SocketAddr>();
    loop {
        let (n, from): (usize, SocketAddr) = tokio::select! {
            r = sock.recv_from(&mut buf) => match r {
                Ok(v) => v,
                Err(_) => continue,
            },
            Some(a) = gone_rx.recv() => {
                sessions.remove(&a);
                continue;
            }
        };
        let pkt = &buf[..n];
        // connected peers: datagrams go to their session
        if let Some(tx) = sessions.get(&from) {
            if pkt[0] & 0x80 != 0 {
                if tx.try_send(pkt.to_vec()).is_err() && tx.is_closed() {
                    sessions.remove(&from);
                }
                continue;
            }
        }
        // offline handshake: 20 packets/s per IP (prevents amplification abuse)
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
        if let Some(pong) = handle_ping(pkt, guid, &cached.1) {
            let _ = sock.send_to(&pong, from).await;
            continue;
        }
        if let Some((reply, mtu)) = open_connection(pkt, guid, from, max_mtu) {
            let _ = sock.send_to(&reply, from).await;
            if let Some(mtu) = mtu {
                if sessions.contains_key(&from) {
                    continue;
                }
                let per_ip = sessions.keys().filter(|a| a.ip() == from.ip()).count();
                if sessions.len() >= state.cfg.server.max_players as usize * 2 + 16 || per_ip >= state.cfg.server.max_connections_per_ip.max(1) as usize {
                    continue;
                }
                let (tx, rx) = mpsc::channel(1024);
                sessions.insert(from, tx);
                let (st, s, gone) = (state.clone(), sock.clone(), gone_tx.clone());
                tokio::spawn(async move {
                    session::run(st, s, from, mtu, rx).await;
                    let _ = gone.send(from);
                });
            }
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
