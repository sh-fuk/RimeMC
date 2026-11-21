//! RimeFleet router side of a player: the client stays connected to the
//! router while the node simulating them changes. A move to another node
//! uses the protocol's reconfiguration (`start_configuration`), the way
//! proxies switch servers: no reconnect, only a short loading screen.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use rime_fleet::balancer::{CellId, NodeId};
use rime_fleet::proto::PlayerForward;
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::codec::Reader;
use super::conn;
use super::crypt::{CryptStream, Profile};
use crate::fleet_net::RouterState;
use crate::registry::PacketIds;
use crate::ServerState;

/// Commands from the fleet router to one proxied player.
pub enum ProxyCmd {
    /// continue on another node with this state
    Switch { node: NodeId, addr: String, state: Vec<u8> },
}

fn id(state: &str, dir: &str, name: &str) -> i32 {
    PacketIds::get().id(state, dir, name)
}

fn packet_id(p: &[u8]) -> Option<i32> {
    Reader::new(p).varint().ok()
}

/// Re-frame a decoded packet (id ‖ body) for a connection's compression.
fn frame(p: &[u8], threshold: i32) -> Vec<u8> {
    let mut r = Reader::new(p);
    let pid = r.varint().unwrap_or(0);
    conn::encode(pid, &p[r.pos..], threshold)
}

struct Backend {
    rd: ReadHalf<TcpStream>,
    wr: WriteHalf<TcpStream>,
    threshold: i32,
}

/// Connect to a node's game port and log in there with the forwarded
/// identity. Returns once the node is in its configuration phase.
async fn connect_backend(addr: &str, st: &ServerState, profile: &Profile, state: Option<Vec<u8>>) -> anyhow::Result<Backend> {
    let mut s = timeout(Duration::from_secs(5), TcpStream::connect(addr)).await??;
    s.set_nodelay(true)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let fwd = PlayerForward { uuid: profile.uuid.as_u128(), name: profile.name.clone(), props: profile.properties.clone(), state, time_ms: now }.encode();
    let tag = rime_fleet::crypto::forward_mac(st.cfg.fleet.secret.as_bytes(), &fwd);
    let mut head = Vec::with_capacity(fwd.len() + 36);
    head.extend_from_slice(&((fwd.len() + 32) as u32).to_le_bytes());
    head.extend_from_slice(&fwd);
    head.extend_from_slice(&tag);
    s.write_all(&head).await?;
    let mut threshold = -1;
    loop {
        let p = timeout(Duration::from_secs(10), conn::read(&mut s, threshold >= 0, 1 << 20)).await??;
        let mut r = Reader::new(&p);
        let pid = r.varint()?;
        if pid == id("login", "clientbound", "login_compression") {
            threshold = r.varint()?;
        } else if pid == id("login", "clientbound", "login_finished") {
            break;
        } else if pid == id("login", "clientbound", "login_disconnect") {
            anyhow::bail!("node refused the player: {}", r.string(1 << 16).unwrap_or("?"));
        }
    }
    s.write_all(&conn::encode(id("login", "serverbound", "login_acknowledged"), &[], threshold)).await?;
    let (rd, wr) = tokio::io::split(s);
    Ok(Backend { rd, wr, threshold })
}

enum Outcome {
    ClientGone,
    BackendGone,
    Switch(NodeId, String, Vec<u8>),
}

/// Serve a logged-in (authenticated) client through the fleet.
pub async fn proxy(client: CryptStream<TcpStream>, st: Arc<ServerState>, profile: Profile, rs: Arc<RouterState>) -> anyhow::Result<()> {
    let th = st.game.threshold;
    let (mut crd, mut cwr) = tokio::io::split(client);
    // ---- login with the client (the nodes' own login is consumed here)
    if th >= 0 {
        let mut b = Vec::new();
        super::codec::write_varint(&mut b, th);
        cwr.write_all(&conn::encode(id("login", "clientbound", "login_compression"), &b, -1)).await?;
    }
    {
        use super::codec::Out;
        let mut b = Vec::new();
        b.uuid(profile.uuid).str(&profile.name);
        b.vi(profile.properties.len() as i32);
        for (n, v, sig) in &profile.properties {
            b.str(n).str(v).bool(sig.is_some());
            if let Some(s) = sig {
                b.str(s);
            }
        }
        cwr.write_all(&conn::encode(id("login", "clientbound", "login_finished"), &b, th)).await?;
        cwr.flush().await?;
    }
    loop {
        let p = timeout(Duration::from_secs(30), conn::read(&mut crd, th >= 0, 4096)).await??;
        if packet_id(&p) == Some(id("login", "serverbound", "login_acknowledged")) {
            break;
        }
    }

    // ---- first node: where the player is
    let uuid = profile.uuid.as_u128();
    let file = crate::game::playerdata::path(&st.world_dir, profile.uuid);
    let mut state = crate::game::playerdata::load_raw(&file);
    let pos = state.as_deref().and_then(crate::game::playerdata::from_bytes).filter(|s| !s.dead).map(|s| s.pos).unwrap_or(st.spawn);
    let cell = CellId::of_chunk(0, (pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4, st.cfg.fleet.cell_shift);
    let Some((mut node, mut addr)) = rs.node_for(cell).await else { anyhow::bail!("no fleet node available") };
    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    rs.player_on(uuid, node, cmd_tx);
    st.online.fetch_add(1, Ordering::Relaxed);
    tracing::info!("{} joined through the fleet (node {node})", profile.name);

    let (cin_tx, mut cin_rx) = mpsc::channel::<Vec<u8>>(1024);
    let creader = tokio::spawn(async move {
        while let Ok(p) = conn::read(&mut crd, th >= 0, conn::MAX_DECOMPRESSED).await {
            if cin_tx.send(p).await.is_err() {
                break;
            }
        }
    });

    let result: anyhow::Result<()> = async {
        loop {
            let b = connect_backend(&addr, &st, &profile, state.take()).await?;
            let Backend { rd: mut brd, wr: mut bwr, threshold: bth } = b;
            let (bin_tx, mut bin_rx) = mpsc::channel::<Vec<u8>>(1024);
            let breader = tokio::spawn(async move {
                while let Ok(p) = conn::read(&mut brd, bth >= 0, 16 << 20).await {
                    if bin_tx.send(p).await.is_err() {
                        break;
                    }
                }
            });
            let outcome = loop {
                tokio::select! {
                    p = cin_rx.recv() => match p {
                        Some(p) => bwr.write_all(&frame(&p, bth)).await?,
                        None => break Outcome::ClientGone,
                    },
                    p = bin_rx.recv() => match p {
                        Some(p) => {
                            let mut out = frame(&p, th);
                            // forward whatever else is already queued in one write
                            while out.len() < 256 * 1024 {
                                match bin_rx.try_recv() {
                                    Ok(q) => out.extend_from_slice(&frame(&q, th)),
                                    Err(_) => break,
                                }
                            }
                            cwr.write_all(&out).await?;
                            cwr.flush().await?;
                        }
                        None => break Outcome::BackendGone,
                    },
                    c = cmd_rx.recv() => if let Some(ProxyCmd::Switch { node, addr, state }) = c {
                        break Outcome::Switch(node, addr, state);
                    },
                }
            };
            breader.abort();
            let _ = bwr.shutdown().await;
            match outcome {
                Outcome::ClientGone | Outcome::BackendGone => return Ok(()),
                Outcome::Switch(n, a, s) => {
                    tracing::info!("{}: node {node} -> node {n}", profile.name);
                    // back to the configuration phase, then log in on the new node
                    cwr.write_all(&conn::encode(id("play", "clientbound", "start_configuration"), &[], th)).await?;
                    cwr.flush().await?;
                    let ack = id("play", "serverbound", "configuration_acknowledged");
                    loop {
                        match timeout(Duration::from_secs(10), cin_rx.recv()).await {
                            Ok(Some(p)) if packet_id(&p) == Some(ack) => break,
                            Ok(Some(_)) => continue, // play packets for the old node
                            _ => return Ok(()),
                        }
                    }
                    node = n;
                    addr = a;
                    state = Some(s);
                }
            }
        }
    }
    .await;
    creader.abort();
    rs.player_gone(uuid);
    st.online.fetch_sub(1, Ordering::Relaxed);
    tracing::info!("{} left the fleet", profile.name);
    result
}
