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
    /// each node's game port (players are proxied there)
    node_addrs: Mutex<HashMap<NodeId, String>>,
    /// proxied players: node now simulating them, and their proxy
    players: Mutex<HashMap<u128, (NodeId, mpsc::Sender<crate::net::fleet_proxy::ProxyCmd>)>>,
    start: Instant,
}

static ROUTER: std::sync::OnceLock<Arc<RouterState>> = std::sync::OnceLock::new();

/// The router's fleet state, when this process is a router with nodes.
pub fn router() -> Option<Arc<RouterState>> {
    ROUTER.get().filter(|r| !r.node_addrs.lock().is_empty()).cloned()
}

impl RouterState {
    /// Node to simulate `cell` (assigning it if new) and its game address.
    pub async fn node_for(&self, cell: CellId) -> Option<(NodeId, String)> {
        let owner = self.balancer.lock().owner_of(cell)?;
        let addr = self.node_addrs.lock().get(&owner).cloned()?;
        let tx = self.nodes.lock().get(&owner).cloned();
        if let Some(tx) = tx {
            let _ = tx.send(FleetMsg::Assign { cells: vec![cell] }).await;
        }
        Some((owner, addr))
    }

    pub fn player_on(&self, player: u128, node: NodeId, tx: mpsc::Sender<crate::net::fleet_proxy::ProxyCmd>) {
        self.players.lock().insert(player, (node, tx));
    }

    pub fn player_gone(&self, player: u128) {
        self.players.lock().remove(&player);
    }
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
        node_addrs: Mutex::new(HashMap::new()),
        players: Mutex::new(HashMap::new()),
        start: Instant::now(),
    });
    let _ = ROUTER.set(rs.clone());

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
    let FleetMsg::Register { cores, version, game_addr } = first else { anyhow::bail!("expected Register") };
    tracing::info!("fleet node {node_id} joined ({cores} cores, {version}, players via {game_addr})");
    rs.node_addrs.lock().insert(node_id, game_addr);

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
                    // answered concurrently: a node asks for hundreds at once
                    let (st, tx) = (state.clone(), tx.clone());
                    tokio::spawn(async move {
                        let nbt = tokio::task::spawn_blocking(move || st.store.load_chunk(dim, Kind::Region, x, z)).await;
                        if let Ok(Ok(nbt)) = nbt {
                            let _ = tx.send(FleetMsg::ChunkData { world, x, z, nbt }).await;
                        }
                    });
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
                FleetMsg::PlayerHandoff { player, cell, state: data } => {
                    // only the node simulating the player may move them
                    let cur = rs.players.lock().get(&player).cloned();
                    let Some((on, proxy)) = cur else { continue };
                    if on != node_id {
                        continue;
                    }
                    if let Some((to, addr)) = rs.node_for(cell).await {
                        if to != node_id {
                            rs.players.lock().insert(player, (to, proxy.clone()));
                            let _ = proxy.send(crate::net::fleet_proxy::ProxyCmd::Switch { node: to, addr, state: data }).await;
                        }
                    }
                }
                FleetMsg::PlayerData { player, state: data } => {
                    let cur = rs.players.lock().get(&player).map(|p| p.0);
                    // a late save from the node the player just left is stale
                    if cur.is_some_and(|c| c != node_id) {
                        continue;
                    }
                    let world = state.world_dir.clone();
                    match tokio::task::spawn_blocking(move || crate::game::playerdata::save_raw(&world, uuid::Uuid::from_u128(player), &data)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::warn!("saving player data from node {node_id}: {e:#}"),
                        Err(e) => tracing::warn!("saving player data from node {node_id}: {e}"),
                    }
                }
                FleetMsg::ServerPacket { .. } => {}
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

/// A node's link to its router (survives reconnects).
pub struct NodeLink {
    tx: Mutex<Option<mpsc::Sender<FleetMsg>>>,
    owned: Mutex<HashSet<CellId>>,
    /// chunk requests waiting for the router's answer
    pending: Mutex<HashMap<(u32, i32, i32), Vec<std::sync::mpsc::Sender<Option<Vec<u8>>>>>>,
    cell_shift: u32,
}

static NODE: std::sync::OnceLock<Arc<NodeLink>> = std::sync::OnceLock::new();

pub fn node_link() -> Option<Arc<NodeLink>> {
    NODE.get().cloned()
}

fn world_id(dim: Dim) -> u32 {
    match dim {
        Dim::Overworld => 0,
        Dim::Nether => 1,
        Dim::End => 2,
    }
}

impl NodeLink {
    fn send(&self, m: FleetMsg) -> bool {
        let tx = self.tx.lock().clone();
        match tx {
            Some(tx) => tx.try_send(m).is_ok(),
            None => false,
        }
    }

    pub fn owns_chunk(&self, x: i32, z: i32) -> bool {
        self.owned.lock().contains(&CellId::of_chunk(0, x, z, self.cell_shift))
    }

    /// The player leaves this node: the router keeps their file.
    pub fn player_left(&self, p: &crate::game::Player) {
        let state = crate::game::playerdata::to_nbt(p).to_root_bytes("");
        self.send(FleetMsg::PlayerData { player: p.uuid.as_u128(), state });
    }
}

impl crate::world::storage::RemoteChunks for NodeLink {
    fn load(&self, dim: Dim, cx: i32, cz: i32) -> Option<Option<Vec<u8>>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let key = (world_id(dim), cx, cz);
        let first = {
            let mut p = self.pending.lock();
            let list = p.entry(key).or_default();
            list.push(tx);
            list.len() == 1
        };
        if first && !self.send(FleetMsg::ChunkRequest { world: key.0, x: cx, z: cz }) {
            self.pending.lock().remove(&key);
            return None;
        }
        let got = rx.recv_timeout(Duration::from_secs(10)).ok();
        if got.is_none() {
            tracing::warn!("RimeFleet: router did not answer for chunk {cx},{cz}; using the local copy");
        }
        got
    }

    fn save(&self, dim: Dim, cx: i32, cz: i32, nbt: &[u8]) {
        // the router only accepts chunks of cells this node simulates
        if self.owns_chunk(cx, cz) {
            self.send(FleetMsg::ChunkData { world: world_id(dim), x: cx, z: cz, nbt: Some(nbt.to_vec()) });
        }
    }
}

/// Node game port: the router connects here with each player it proxies.
pub async fn run_node_game(state: Arc<ServerState>) -> anyhow::Result<()> {
    let addr = state.cfg.fleet.node_game_address.clone();
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("RimeFleet node: players arrive from the router on {addr}");
    loop {
        let (sock, peer) = listener.accept().await?;
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = node_game_conn(sock, st).await {
                tracing::warn!("fleet player connection from {peer}: {e:#}");
            }
        });
    }
}

async fn node_game_conn(mut sock: TcpStream, st: Arc<ServerState>) -> anyhow::Result<()> {
    sock.set_nodelay(true)?;
    let len = tokio::time::timeout(HANDSHAKE_TIMEOUT, sock.read_u32_le()).await?? as usize;
    anyhow::ensure!((32..=4 << 20).contains(&len), "bad forward length");
    let mut buf = vec![0u8; len];
    tokio::time::timeout(HANDSHAKE_TIMEOUT, sock.read_exact(&mut buf)).await??;
    let (payload, tag) = buf.split_at(len - 32);
    anyhow::ensure!(rime_fleet::crypto::verify_forward(st.cfg.fleet.secret.as_bytes(), payload, tag), "forward not signed by the router");
    let fwd = rime_fleet::proto::PlayerForward::decode(payload)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    anyhow::ensure!(fwd.time_ms.abs_diff(now) < 30_000, "stale forward");
    let profile = crate::net::crypt::Profile { uuid: uuid::Uuid::from_u128(fwd.uuid), name: fwd.name, properties: fwd.props };
    crate::net::play::run_with_state(crate::net::crypt::CryptStream::new(sock), st, profile, fwd.state).await
}

/// Players standing in cells this node does not simulate are handed to the
/// router, which moves them to the owner.
fn check_handoffs(state: &ServerState, link: &NodeLink, recent: &mut HashMap<u128, Instant>) {
    recent.retain(|_, t| t.elapsed() < Duration::from_secs(5));
    if link.owned.lock().is_empty() {
        return;
    }
    for p in state.game.players() {
        let pos = p.pose.lock().pos;
        let cell = CellId::of_chunk(0, (pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4, link.cell_shift);
        if link.owned.lock().contains(&cell) || recent.contains_key(&p.uuid.as_u128()) || p.vit.lock().dead() {
            continue;
        }
        recent.insert(p.uuid.as_u128(), Instant::now());
        let st = crate::game::playerdata::to_nbt(&p).to_root_bytes("");
        link.send(FleetMsg::PlayerHandoff { player: p.uuid.as_u128(), cell, state: st });
    }
}

pub async fn run_node(state: Arc<ServerState>) -> anyhow::Result<()> {
    let link = NODE
        .get_or_init(|| Arc::new(NodeLink { tx: Mutex::new(None), owned: Mutex::new(HashSet::new()), pending: Mutex::new(HashMap::new()), cell_shift: state.cfg.fleet.cell_shift }))
        .clone();
    state.store.set_remote(link.clone());
    {
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = run_node_game(st).await {
                tracing::error!("RimeFleet node game port: {e:#}");
            }
        });
    }
    {
        let (st, link) = (state.clone(), link.clone());
        tokio::spawn(async move {
            let mut recent = HashMap::new();
            let mut t = tokio::time::interval(Duration::from_millis(500));
            loop {
                t.tick().await;
                check_handoffs(&st, &link, &mut recent);
            }
        });
    }
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
    wr.send(&FleetMsg::Register { cores: state.cfg.worker_threads() as u32, version: env!("CARGO_PKG_VERSION").into(), game_addr: f.node_game_address.clone() }).await?;
    tracing::info!("RimeFleet: connected to router {} as node {}", f.router_address, f.node_id);

    let link = node_link().expect("node link");
    link.owned.lock().clear();
    let (tx, mut rx) = mpsc::channel::<FleetMsg>(1024);
    *link.tx.lock() = Some(tx.clone());
    let hb = {
        let (tx, link, st) = (tx.clone(), link.clone(), state.clone());
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_millis(1000));
            loop {
                t.tick().await;
                let cells = link.owned.lock().iter().map(|c| CellLoad { cell: *c, cost: st.cell_cost(c), redstone: false }).collect();
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
                    link.owned.lock().extend(cells);
                }
                FleetMsg::Revoke { cells } => {
                    let mut o = link.owned.lock();
                    for c in cells {
                        o.remove(&c);
                    }
                }
                FleetMsg::RedstoneRole { active } => state.set_redstone_role(active),
                FleetMsg::ChunkData { world, x, z, nbt } => {
                    for w in link.pending.lock().remove(&(world, x, z)).unwrap_or_default() {
                        let _ = w.send(nbt.clone());
                    }
                }
                FleetMsg::ClientPacket { .. } | FleetMsg::PlayerHandoff { .. } => {}
                FleetMsg::Shutdown => return Ok(()),
                other => tracing::debug!("router sent unexpected {other:?}"),
            }
        }
    }
    .await;
    hb.abort();
    writer.abort();
    *link.tx.lock() = None;
    // wake chunk loads waiting for this connection (they fall back to local)
    link.pending.lock().clear();
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
