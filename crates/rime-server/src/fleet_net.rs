//! RimeFleet networking: router and node runtimes over the authenticated,
//! encrypted channel from `rime_fleet::crypto`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rime_fleet::balancer::{Balancer, BalancerConfig, CellId, NodeId};
use rime_fleet::crypto::{Handshake, Role, SecureChannel, AUTH_LEN, HELLO_LEN};
use rime_fleet::proto::{CellLoad, FleetMsg};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::world::storage::{Dim, Kind};
use crate::ServerState;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Run the handshake on a fresh connection; returns the split, encrypted halves.
async fn secure(mut sock: TcpStream, secret: &[u8], role: Role, my_id: u64) -> anyhow::Result<(EncReader, EncWriter, u64)> {
    sock.set_nodelay(true)?;
    let fut = async {
        let mut hs = Handshake::new(secret, role, my_id)?;
        sock.write_all(&hs.hello()).await?;
        let mut peer_hello = [0u8; HELLO_LEN];
        sock.read_exact(&mut peer_hello).await?;
        let tag = hs.on_peer_hello(&peer_hello)?;
        sock.write_all(&tag).await?;
        let mut peer_tag = [0u8; AUTH_LEN];
        sock.read_exact(&mut peer_tag).await?;
        let (chan, peer_id) = hs.finish(&peer_tag)?;
        anyhow::Ok((chan, peer_id))
    };
    let (chan, peer_id) = tokio::time::timeout(HANDSHAKE_TIMEOUT, fut).await??;
    // The channel holds both directions' state; split it into two halves that
    // share it through a mutex (seal/open never run concurrently on one side).
    let chan = Arc::new(Mutex::new(chan));
    let (r, w) = sock.into_split();
    Ok((EncReader { r, chan: chan.clone() }, EncWriter { w, chan }, peer_id))
}

struct EncReader {
    r: OwnedReadHalf,
    chan: Arc<Mutex<SecureChannel>>,
}

impl EncReader {
    async fn recv(&mut self) -> anyhow::Result<FleetMsg> {
        let len = self.r.read_u32_le().await?;
        let len = SecureChannel::check_len(len)?;
        let mut body = vec![0u8; len];
        self.r.read_exact(&mut body).await?;
        let pt = self.chan.lock().open(&body)?;
        Ok(FleetMsg::decode(&pt)?)
    }
}

struct EncWriter {
    w: OwnedWriteHalf,
    chan: Arc<Mutex<SecureChannel>>,
}

impl EncWriter {
    async fn send(&mut self, m: &FleetMsg) -> anyhow::Result<()> {
        let rec = self.chan.lock().seal(&m.encode())?;
        self.w.write_all(&rec).await?;
        Ok(())
    }
}

fn now_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

fn dim_of(world: u32) -> Option<Dim> {
    match world {
        0 => Some(Dim::Overworld),
        1 => Some(Dim::Nether),
        2 => Some(Dim::End),
        _ => None,
    }
}

// ============================================================== router

pub struct RouterState {
    balancer: Mutex<Balancer>,
    nodes: Mutex<HashMap<NodeId, mpsc::Sender<FleetMsg>>>,
    start: Instant,
}

pub async fn run_router(state: Arc<ServerState>) -> anyhow::Result<()> {
    let f = &state.cfg.fleet;
    let listener = TcpListener::bind(&f.router_address).await?;
    tracing::info!("RimeFleet router listening for nodes on {}", f.router_address);
    let rs = Arc::new(RouterState {
        balancer: Mutex::new(Balancer::new(BalancerConfig {
            imbalance_threshold: f.imbalance_threshold,
            heartbeat_timeout_ms: f.heartbeat_timeout_ms,
            ..Default::default()
        })),
        nodes: Mutex::new(HashMap::new()),
        start: Instant::now(),
    });

    // balancing loop
    {
        let rs = rs.clone();
        let interval = f.rebalance_interval_ms.max(100);
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_millis(interval));
            loop {
                t.tick().await;
                let moves = rs.balancer.lock().tick(now_ms(rs.start));
                apply_moves(&rs, &moves).await;
            }
        });
    }

    loop {
        let (sock, addr) = listener.accept().await?;
        let (st, rs) = (state.clone(), rs.clone());
        tokio::spawn(async move {
            if let Err(e) = router_conn(sock, st, rs).await {
                tracing::warn!("fleet node {addr}: {e:#}");
            }
        });
    }
}

async fn apply_moves(rs: &RouterState, moves: &[rime_fleet::balancer::Move]) {
    if moves.is_empty() {
        return;
    }
    let mut assign: HashMap<NodeId, Vec<CellId>> = HashMap::new();
    let mut revoke: HashMap<NodeId, Vec<CellId>> = HashMap::new();
    for m in moves {
        if let Some(f) = m.from {
            revoke.entry(f).or_default().push(m.cell);
        }
        if let Some(t) = m.to {
            assign.entry(t).or_default().push(m.cell);
        }
    }
    let redstone = rs.balancer.lock().redstone_node();
    let nodes = rs.nodes.lock().clone();
    // revoke first so a cell is never simulated twice
    for (n, cells) in revoke {
        if let Some(tx) = nodes.get(&n) {
            let _ = tx.send(FleetMsg::Revoke { cells }).await;
        }
    }
    for (n, cells) in assign {
        if let Some(tx) = nodes.get(&n) {
            let _ = tx.send(FleetMsg::RedstoneRole { active: redstone == Some(n) }).await;
            let _ = tx.send(FleetMsg::Assign { cells }).await;
        }
    }
}

async fn router_conn(sock: TcpStream, state: Arc<ServerState>, rs: Arc<RouterState>) -> anyhow::Result<()> {
    let (mut rd, mut wr, node_id) = secure(sock, state.cfg.fleet.secret.as_bytes(), Role::Router, 0).await?;
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, rd.recv()).await??;
    let FleetMsg::Register { cores, version } = first else { anyhow::bail!("expected Register") };
    tracing::info!("fleet node {node_id} joined ({cores} cores, {version})");

    let (tx, mut rx) = mpsc::channel::<FleetMsg>(1024);
    if rs.nodes.lock().insert(node_id, tx.clone()).is_some() {
        tracing::warn!("fleet node id {node_id} reconnected; replacing old session");
    }
    rs.balancer.lock().node_joined(node_id, cores.clamp(1, 1024) as f64, now_ms(rs.start));

    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if wr.send(&m).await.is_err() {
                break;
            }
        }
    });

    let result: anyhow::Result<()> = async {
        loop {
            let msg = tokio::time::timeout(Duration::from_millis(state.cfg.fleet.heartbeat_timeout_ms * 2), rd.recv()).await??;
            match msg {
                FleetMsg::Heartbeat { mspt: _, cells } => {
                    let mut b = rs.balancer.lock();
                    b.heartbeat(node_id, now_ms(rs.start));
                    for CellLoad { cell, cost, redstone } in cells.into_iter().take(100_000) {
                        b.report_cell(cell, cost as f64, redstone);
                    }
                }
                FleetMsg::ChunkRequest { world, x, z } => {
                    let Some(dim) = dim_of(world) else { continue };
                    let st = state.clone();
                    let nbt = tokio::task::spawn_blocking(move || st.store.load_chunk(dim, Kind::Region, x, z)).await??;
                    let _ = tx.send(FleetMsg::ChunkData { world, x, z, nbt }).await;
                }
                FleetMsg::ChunkData { world, x, z, nbt: Some(nbt) } => {
                    // a node may only write chunks of cells it owns
                    let cell = CellId::of_chunk(world, x, z, state.cfg.fleet.cell_shift);
                    if rs.balancer.lock().owner_of(cell) != Some(node_id) {
                        tracing::warn!("node {node_id} tried to write chunk {x},{z} it does not own; ignored");
                        continue;
                    }
                    let Some(dim) = dim_of(world) else { continue };
                    let st = state.clone();
                    tokio::task::spawn_blocking(move || st.store.save_chunk(dim, Kind::Region, x, z, &nbt)).await??;
                }
                FleetMsg::ServerPacket { .. } => { /* forwarded to client sessions once play state exists */ }
                other => tracing::debug!("node {node_id}: unexpected {other:?}"),
            }
        }
    }
    .await;

    writer.abort();
    rs.nodes.lock().remove(&node_id);
    let moves = rs.balancer.lock().node_left(node_id, now_ms(rs.start));
    tracing::warn!("fleet node {node_id} left; {} cells re-homed", moves.len());
    apply_moves(&rs, &moves).await;
    result
}

// ============================================================== node

pub async fn run_node(state: Arc<ServerState>) -> anyhow::Result<()> {
    let mut backoff = Duration::from_millis(250);
    loop {
        match node_session(state.clone()).await {
            Ok(()) => return Ok(()),
            Err(e) => tracing::warn!("RimeFleet: lost router connection: {e:#}; retrying in {backoff:?}"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(10));
    }
}

async fn node_session(state: Arc<ServerState>) -> anyhow::Result<()> {
    let f = &state.cfg.fleet;
    let sock = TcpStream::connect(&f.router_address).await?;
    let (mut rd, mut wr, _) = secure(sock, f.secret.as_bytes(), Role::Node, f.node_id).await?;
    wr.send(&FleetMsg::Register { cores: state.cfg.worker_threads() as u32, version: env!("CARGO_PKG_VERSION").into() }).await?;
    tracing::info!("RimeFleet: connected to router {} as node {}", f.router_address, f.node_id);

    let owned: Arc<Mutex<HashSet<CellId>>> = Arc::new(Mutex::new(HashSet::new()));
    let (tx, mut rx) = mpsc::channel::<FleetMsg>(1024);
    let hb = {
        let (tx, owned, st) = (tx.clone(), owned.clone(), state.clone());
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_millis(1000));
            loop {
                t.tick().await;
                let cells = owned.lock().iter().map(|c| CellLoad { cell: *c, cost: st.cell_cost(c), redstone: false }).collect();
                if tx.send(FleetMsg::Heartbeat { mspt: st.mspt(), cells }).await.is_err() {
                    break;
                }
            }
        })
    };
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if wr.send(&m).await.is_err() {
                break;
            }
        }
    });
    let res: anyhow::Result<()> = async {
        loop {
            match rd.recv().await? {
                FleetMsg::Assign { cells } => {
                    tracing::info!("RimeFleet: now simulating {} more cell(s)", cells.len());
                    owned.lock().extend(cells);
                }
                FleetMsg::Revoke { cells } => {
                    let mut o = owned.lock();
                    for c in cells {
                        o.remove(&c);
                    }
                }
                FleetMsg::RedstoneRole { active } => state.set_redstone_role(active),
                FleetMsg::ChunkData { .. } | FleetMsg::ClientPacket { .. } | FleetMsg::PlayerHandoff { .. } => {}
                FleetMsg::Shutdown => return Ok(()),
                other => tracing::debug!("router sent unexpected {other:?}"),
            }
        }
    }
    .await;
    hb.abort();
    writer.abort();
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handshake_over_tcp() {
        let secret = b"a-very-long-shared-secret-for-the-fleet-0123".to_vec();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let s2 = secret.clone();
        let server = tokio::spawn(async move {
            let (sock, _) = l.accept().await.unwrap();
            let (mut r, mut w, id) = secure(sock, &s2, Role::Router, 0).await.unwrap();
            assert_eq!(id, 5);
            let m = r.recv().await.unwrap();
            w.send(&m).await.unwrap();
        });
        let sock = TcpStream::connect(addr).await.unwrap();
        let (mut r, mut w, _) = secure(sock, &secret, Role::Node, 5).await.unwrap();
        let msg = FleetMsg::ChunkRequest { world: 0, x: 1, z: 2 };
        w.send(&msg).await.unwrap();
        assert_eq!(r.recv().await.unwrap(), msg);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn wrong_secret_fails() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (sock, _) = l.accept().await.unwrap();
            secure(sock, b"0000000000000000000000000000000000000000", Role::Router, 0).await.is_err()
        });
        let sock = TcpStream::connect(addr).await.unwrap();
        let client = secure(sock, b"1111111111111111111111111111111111111111", Role::Node, 5).await;
        assert!(client.is_err());
        assert!(server.await.unwrap());
    }
}
