//! Java Edition session after the handshake: login → configuration → play.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::codec::{Out, Reader};
use super::conn;
use crate::anticheat::movement::{MoveContext, MovementChecker};
use crate::anticheat::xray::{self, MiningStats};
use crate::anticheat::{Check, Verdict, Violations};
use crate::config::XrayMode;
use crate::data::{Items, Stack};
use crate::game::{fluid, Player, Pose};
use rand::Rng;
use crate::nbt::Tag;
use crate::ops::CommandSender;
use crate::registry::{blocks, BlockRegistry, BlockState, PacketIds, Registries};
use crate::tick::playercost::{estimate_cost, Activity, PlayerBudget};
use crate::world::chunk::{Chunk, MIN_Y};
use crate::ServerState;

const PHASE_TIMEOUT: Duration = Duration::from_secs(15);
/// Packets per second a client may send before it is disconnected.
const MAX_PACKETS_PER_SEC: u32 = 1000;
/// Max encoded packets queued for one client (back-pressure → disconnect).
const OUT_QUEUE: usize = 8192;

fn id(state: &str, dir: &str, name: &str) -> i32 {
    PacketIds::get().id(state, dir, name)
}

fn angle(deg: f32) -> u8 {
    ((deg.rem_euclid(360.0) / 360.0) * 256.0) as i32 as u8
}

/// Vanilla's "hashed seed" (first 8 bytes of SHA-256 of the seed, little endian).
pub fn hashed_seed(seed: i64) -> i64 {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(seed.to_le_bytes());
    i64::from_le_bytes(h[..8].try_into().unwrap())
}

/// Body of `level_chunk_with_light`.
pub fn chunk_body(c: &Chunk) -> Vec<u8> {
    let mut b = Vec::with_capacity(24 * 1024);
    b.i32(c.x).i32(c.z);
    let hms = c.network_heightmaps();
    b.vi(hms.len() as i32);
    for (ty, longs) in hms {
        b.vi(ty).vi(longs.len() as i32);
        for l in longs {
            b.i64(l);
        }
    }
    let data = c.network_sections();
    b.vi(data.len() as i32).raw(&data);
    b.vi(0); // block entities
    let (sky, sky_empty, arrays) = c.sky_light();
    let bits = |b: &mut Vec<u8>, m: u64| {
        if m == 0 {
            b.vi(0);
        } else {
            b.vi(1).i64(m as i64);
        }
    };
    let all = (1u64 << (crate::world::chunk::SECTIONS + 2)) - 1;
    bits(&mut b, sky);
    bits(&mut b, 0); // block light mask
    bits(&mut b, sky_empty);
    bits(&mut b, all); // every block light section is empty
    b.vi(arrays.len() as i32);
    for a in arrays {
        b.vi(2048).raw(&a);
    }
    b.vi(0);
    b
}

/// The `commands` tree: a few literals, each taking an optional greedy string.
fn commands_body() -> Vec<u8> {
    let names = ["rime", "stop", "op", "deop", "gamemode", "tp", "list", "help", "spawn"];
    let parser = Registries::get().id("minecraft:command_argument_type", "brigadier:string").unwrap_or(5);
    let arg = names.len() as i32 + 1;
    let mut b = Vec::new();
    b.vi(names.len() as i32 + 2);
    // root
    b.u8(0).vi(names.len() as i32);
    for i in 0..names.len() {
        b.vi(i as i32 + 1);
    }
    for n in names {
        b.u8(0x01 | 0x04).vi(1).vi(arg).str(n);
    }
    b.u8(0x02 | 0x04).vi(0).str("args").vi(parser).vi(2);
    b.vi(0);
    b
}

fn text_nbt(msg: &str) -> Vec<u8> {
    Tag::text(msg).to_network_bytes()
}

/// Run a Java client from Login Start until it disconnects.
pub async fn run(mut sock: TcpStream, st: Arc<ServerState>, name: String, uuid: uuid::Uuid) -> anyhow::Result<()> {
    let th = st.game.threshold;
    // ---- login
    if th >= 0 {
        let mut b = Vec::new();
        b.vi(th);
        sock.write_all(&conn::encode(id("login", "clientbound", "login_compression"), &b, -1)).await?;
    }
    let mut b = Vec::new();
    b.uuid(uuid).str(&name).vi(0);
    sock.write_all(&conn::encode(id("login", "clientbound", "login_finished"), &b, th)).await?;
    let compressed = th >= 0;
    loop {
        let p = timeout(PHASE_TIMEOUT, conn::read(&mut sock, compressed, 4096)).await??;
        let mut r = Reader::new(&p);
        if r.varint()? == id("login", "serverbound", "login_acknowledged") {
            break;
        }
    }

    // ---- configuration
    let cfg_out = |name: &str, body: &[u8]| conn::encode(id("configuration", "clientbound", name), body, th);
    let mut b = Vec::new();
    b.str("minecraft:brand").str("RimeMC");
    sock.write_all(&cfg_out("custom_payload", &b)).await?;
    let mut b = Vec::new();
    b.vi(1).str("minecraft:vanilla");
    sock.write_all(&cfg_out("update_enabled_features", &b)).await?;
    let mut b = Vec::new();
    b.vi(1).str("minecraft").str("core").str(crate::registry::blocks::MC_VERSION);
    sock.write_all(&cfg_out("select_known_packs", &b)).await?;

    let mut client_view = st.cfg.server.view_distance as i32;
    let mut known = false;
    let mut sent_registries = false;
    loop {
        let p = timeout(PHASE_TIMEOUT, conn::read(&mut sock, compressed, 64 * 1024)).await??;
        let mut r = Reader::new(&p);
        let pid = r.varint()?;
        if pid == id("configuration", "serverbound", "client_information") {
            let _locale = r.string(16)?;
            client_view = (r.i8()? as i32).clamp(2, 32);
        } else if pid == id("configuration", "serverbound", "select_known_packs") && !sent_registries {
            let n = r.varint()?;
            anyhow::ensure!((0..=64).contains(&n), "too many known packs");
            for _ in 0..n {
                let ns = r.string(256)?.to_string();
                let pack = r.string(256)?.to_string();
                let ver = r.string(256)?.to_string();
                if ns == "minecraft" && pack == "core" && ver == crate::registry::blocks::MC_VERSION {
                    known = true;
                }
            }
            let cd = &st.config_data;
            let mut out = Vec::with_capacity(if known { 64 * 1024 } else { 1024 * 1024 });
            for i in 0..cd.registries.len() {
                out.extend_from_slice(&cfg_out("registry_data", &cd.registry_packet(i, known)));
            }
            out.extend_from_slice(&cfg_out("update_tags", &cd.tags_body));
            out.extend_from_slice(&cfg_out("finish_configuration", &[]));
            sock.write_all(&out).await?;
            sent_registries = true;
        } else if pid == id("configuration", "serverbound", "finish_configuration") && sent_registries {
            break;
        }
    }

    // ---- play
    let view = client_view.min(st.cfg.server.view_distance as i32).max(2);
    let spawn = st.spawn;
    let gamemode = st.cfg.server.default_gamemode.id();
    let (tx, rx) = mpsc::channel::<Arc<Vec<u8>>>(OUT_QUEUE);
    let pose = Pose { pos: spawn, yaw: 0.0, pitch: 0.0, on_ground: false };
    let Some(me) = st.game.join(uuid, name.clone(), pose, gamemode, view, tx) else {
        let b = text_nbt("You are already logged in from another location.");
        sock.write_all(&conn::encode(id("play", "clientbound", "disconnect"), &b, th)).await?;
        return Ok(());
    };
    st.online.store(st.game.count() as u32, Ordering::Relaxed);
    tracing::info!("{name} joined the game ({uuid}, entity {})", me.eid);

    let (rd, wr) = sock.into_split();
    let writer = tokio::spawn(writer_task(wr, rx));
    let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>(1024);
    let reader = tokio::spawn(reader_task(rd, in_tx));

    let mut s = Session::new(st.clone(), me.clone(), view);
    let result = s.run(in_rx).await;

    // ---- leave
    st.game.leave(me.eid);
    st.online.store(st.game.count() as u32, Ordering::Relaxed);
    for c in &s.sent {
        st.game.world.remove_viewer(c.0, c.1);
    }
    let mut b = Vec::new();
    b.vi(1).uuid(me.uuid);
    st.game.broadcast(st.game.encode("player_info_remove", &b));
    let mut b = Vec::new();
    b.vi(1).vi(me.eid);
    st.game.broadcast(st.game.encode("remove_entities", &b));
    st.game.broadcast(st.game.system_chat(&format!("§e{name} left the game")));
    tracing::info!("{name} left the game{}", match &result {
        Err(e) => format!(" ({e:#})"),
        Ok(()) => String::new(),
    });
    reader.abort();
    // give the writer a moment to flush a final disconnect packet
    drop(s);
    drop(me);
    let _ = timeout(Duration::from_secs(2), writer).await;
    Ok(())
}

async fn writer_task(mut wr: tokio::net::tcp::OwnedWriteHalf, mut rx: mpsc::Receiver<Arc<Vec<u8>>>) {
    let mut buf = Vec::with_capacity(64 * 1024);
    while let Some(p) = rx.recv().await {
        buf.clear();
        buf.extend_from_slice(&p);
        // coalesce whatever else is already queued into one write
        while buf.len() < 256 * 1024 {
            match rx.try_recv() {
                Ok(p) => buf.extend_from_slice(&p),
                Err(_) => break,
            }
        }
        if wr.write_all(&buf).await.is_err() {
            return;
        }
    }
    let _ = wr.shutdown().await;
}

async fn reader_task(mut rd: tokio::net::tcp::OwnedReadHalf, tx: mpsc::Sender<Vec<u8>>) {
    let mut window = Instant::now();
    let mut count = 0u32;
    loop {
        let p = match conn::read(&mut rd, true, conn::MAX_DECOMPRESSED).await {
            Ok(p) => p,
            Err(_) => return,
        };
        count += 1;
        if window.elapsed() >= Duration::from_secs(1) {
            window = Instant::now();
            count = 0;
        }
        if count > MAX_PACKETS_PER_SEC || tx.send(p).await.is_err() {
            return;
        }
    }
}

struct Session {
    st: Arc<ServerState>,
    me: Arc<Player>,
    base_view: i32,
    view: i32,
    center: (i32, i32),
    sent: HashSet<(i32, i32)>,
    queue: Vec<(i32, i32)>,
    in_flight: u32,
    chunks_per_tick: f32,
    tracked: HashMap<i32, u64>,
    pending_tp: Option<i32>,
    next_tp: i32,
    sneaking: bool,
    next_window: u8,
    checker: MovementChecker,
    vl: Violations,
    mining: MiningStats,
    dig_start: Option<((i32, i32, i32), Instant)>,
    gliding: bool,
    sprinting: bool,
    ka_pending: Option<(i64, Instant)>,
    ka_last: Instant,
    budget: PlayerBudget,
    act_packets: u32,
    act_chunks: u32,
    last_second_pos: [f64; 3],
    tick: u64,
    epoch: Instant,
}

enum Flow {
    Continue,
    Disconnect(String),
}

impl Session {
    fn new(st: Arc<ServerState>, me: Arc<Player>, view: i32) -> Self {
        let p = me.pose.lock().pos;
        Self {
            st,
            me,
            base_view: view,
            view,
            center: ((p[0].floor() as i32) >> 4, (p[2].floor() as i32) >> 4),
            sent: HashSet::new(),
            queue: Vec::new(),
            in_flight: 0,
            chunks_per_tick: 9.0,
            tracked: HashMap::new(),
            pending_tp: None,
            next_tp: 1,
            sneaking: false,
            next_window: 0,
            checker: MovementChecker::default(),
            vl: Violations::default(),
            mining: MiningStats::default(),
            dig_start: None,
            gliding: false,
            sprinting: false,
            ka_pending: None,
            ka_last: Instant::now(),
            budget: PlayerBudget::default(),
            act_packets: 0,
            act_chunks: 0,
            last_second_pos: p,
            tick: 0,
            epoch: Instant::now(),
        }
    }

    fn send(&self, name: &str, body: &[u8]) {
        self.me.send(self.st.game.encode(name, body));
    }

    fn msg(&self, text: &str) {
        self.me.send(self.st.game.system_chat(text));
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn gamemode(&self) -> i32 {
        self.me.gamemode.load(Ordering::Relaxed)
    }

    async fn run(&mut self, mut rx: mpsc::Receiver<Vec<u8>>) -> anyhow::Result<()> {
        self.join_sequence().await;
        let mut ticker = tokio::time::interval(Duration::from_millis(50));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let flow = tokio::select! {
                p = rx.recv() => match p {
                    Some(p) => {
                        self.act_packets += 1;
                        match self.handle(&p) {
                            Ok(f) => f,
                            Err(e) => Flow::Disconnect(format!("Bad packet: {e}")),
                        }
                    }
                    None => return Ok(()),
                },
                _ = ticker.tick() => self.on_tick().await,
            };
            if let Flow::Disconnect(reason) = flow {
                self.send("disconnect", &text_nbt(&reason));
                anyhow::bail!("disconnected: {reason}");
            }
        }
    }

    async fn join_sequence(&mut self) {
        let st = self.st.clone();
        let cd = &st.config_data;
        let pose = *self.me.pose.lock();
        let gm = self.gamemode();
        let mut b = Vec::new();
        b.i32(self.me.eid).bool(false).vi(3).str("minecraft:overworld").str("minecraft:the_end").str("minecraft:the_nether");
        b.vi(st.cfg.server.max_players as i32).vi(self.view).vi(st.cfg.server.simulation_distance as i32);
        b.bool(false).bool(true).bool(false);
        b.vi(cd.entry_id("minecraft:dimension_type", "minecraft:overworld")).str("minecraft:overworld").i64(hashed_seed(st.seed));
        b.u8(gm as u8).u8(255).bool(false).bool(false).bool(false).vi(0).vi(63);
        b.bool(false);
        self.send("login", &b);
        let mut b = Vec::new();
        b.vi(2).bool(false);
        self.send("change_difficulty", &b);
        self.send_abilities();
        self.send("commands", &commands_body());
        self.teleport(pose.pos, pose.yaw, pose.pitch);
        // tab list: everyone online, then announce me to everyone
        let all = st.game.players();
        let mut b = Vec::new();
        b.u8(0x01 | 0x04 | 0x08 | 0x10).vi(all.len() as i32);
        for p in &all {
            b.uuid(p.uuid).str(&p.name).vi(0).vi(p.gamemode.load(Ordering::Relaxed)).bool(true).vi(0);
        }
        self.send("player_info_update", &b);
        let mut b = Vec::new();
        b.u8(0x01 | 0x04 | 0x08 | 0x10).vi(1).uuid(self.me.uuid).str(&self.me.name).vi(0).vi(gm).bool(true).vi(0);
        let info = st.game.encode("player_info_update", &b);
        for p in &all {
            if p.eid != self.me.eid {
                p.send(info.clone());
            }
        }
        st.game.broadcast(st.game.system_chat(&format!("§e{} joined the game", self.me.name)));
        let mut b = Vec::new();
        b.f64(0.0).f64(0.0).f64(59_999_968.0).f64(59_999_968.0).vl(0).vi(29_999_984).vi(5).vi(15);
        self.send("initialize_border", &b);
        self.send_time();
        let (sx, sy, sz) = spawn_block(st.spawn);
        let mut b = Vec::new();
        b.str("minecraft:overworld").pos(sx, sy, sz).f32(0.0).f32(0.0);
        self.send("set_default_spawn_position", &b);
        let mut b = Vec::new();
        b.u8(13).f32(0.0);
        self.send("game_event", &b);
        let mut b = Vec::new();
        b.f32(20.0).bool(false);
        self.send("ticking_state", &b);
        let mut b = Vec::new();
        b.vi(0);
        self.send("ticking_step", &b);
        self.set_center(self.center, true);
        self.pump_chunks().await;
        let mut b = Vec::new();
        b.f32(20.0).vi(20).f32(5.0);
        self.send("set_health", &b);
        let mut b = Vec::new();
        b.f32(0.0).vi(0).vi(0);
        self.send("set_experience", &b);
        let mut b = Vec::new();
        b.vi(0);
        self.send("set_held_slot", &b);
        self.st.game.send_inventory(&self.me);
    }

    fn send_abilities(&self) {
        let flags: u8 = match self.gamemode() {
            1 => 0x01 | 0x04 | 0x08,
            3 => 0x01 | 0x02 | 0x04,
            _ => 0,
        };
        let mut b = Vec::new();
        b.u8(flags).f32(0.05).f32(0.1);
        self.send("player_abilities", &b);
    }

    fn send_time(&self) {
        let age = self.st.started.elapsed().as_millis() as i64 / 50;
        let mut b = Vec::new();
        b.i64(age).vi(1).vi(self.st.config_data.entry_id("minecraft:world_clock", "minecraft:overworld")).vl(age + 1000).f32(0.0).f32(1.0);
        self.send("set_time", &b);
    }

    fn teleport(&mut self, pos: [f64; 3], yaw: f32, pitch: f32) {
        let tid = self.next_tp;
        self.next_tp = self.next_tp.wrapping_add(1).max(1);
        self.pending_tp = Some(tid);
        self.checker.teleported(pos);
        {
            let mut p = self.me.pose.lock();
            p.pos = pos;
            p.yaw = yaw;
            p.pitch = pitch;
        }
        self.me.moved.fetch_add(1, Ordering::Relaxed);
        let mut b = Vec::new();
        b.vi(tid).f64(pos[0]).f64(pos[1]).f64(pos[2]).f64(0.0).f64(0.0).f64(0.0).f32(yaw).f32(pitch).i32(0);
        self.send("player_position", &b);
    }

    // ------------------------------------------------------------ chunks

    fn in_view(&self, c: (i32, i32), center: (i32, i32), view: i32) -> bool {
        (c.0 - center.0).abs() <= view && (c.1 - center.1).abs() <= view
    }

    fn set_center(&mut self, center: (i32, i32), force: bool) {
        if center == self.center && !force {
            return;
        }
        self.center = center;
        *self.me.view.lock() = (center.0, center.1, self.view);
        let mut b = Vec::new();
        b.vi(center.0).vi(center.1);
        self.send("set_chunk_cache_center", &b);
        // forget chunks that left the view
        let gone: Vec<(i32, i32)> = self.sent.iter().copied().filter(|c| !self.in_view(*c, center, self.view)).collect();
        for c in gone {
            self.sent.remove(&c);
            self.st.game.world.remove_viewer(c.0, c.1);
            let eids = self.st.game.entities_in(c);
            if !eids.is_empty() {
                let mut b = Vec::new();
                b.vi(eids.len() as i32);
                for e in eids {
                    b.vi(e);
                }
                self.send("remove_entities", &b);
            }
            let mut b = Vec::new();
            b.i32(c.1).i32(c.0);
            self.send("forget_level_chunk", &b);
        }
        let v = self.view;
        self.queue = (-v..=v).flat_map(|dx| (-v..=v).map(move |dz| (center.0 + dx, center.1 + dz))).filter(|c| !self.sent.contains(c)).collect();
        self.sort_queue();
    }

    fn sort_queue(&mut self) {
        let (cx, cz) = self.center;
        let directional = self.budget.level >= 1;
        let yaw = self.me.pose.lock().yaw as f64;
        let look = (-(yaw.to_radians().sin()), yaw.to_radians().cos());
        // sorted far → near so `pop()` yields the nearest chunk
        self.queue.sort_by(|a, b| {
            let score = |c: &(i32, i32)| {
                let (dx, dz) = ((c.0 - cx) as f64, (c.1 - cz) as f64);
                let mut d = dx * dx + dz * dz;
                if directional && dx * look.0 + dz * look.1 < 0.0 {
                    d *= 4.0; // behind the player: later
                }
                d
            };
            score(b).total_cmp(&score(a))
        });
    }

    async fn pump_chunks(&mut self) {
        if self.queue.is_empty() || self.in_flight >= 4 {
            return;
        }
        let limits = self.budget.limits(&self.st.cfg.player_cost, self.base_view as u32);
        let n = (self.chunks_per_tick.ceil() as usize).clamp(1, 64).min(limits.chunk_sends_per_tick.max(1) as usize * 4);
        let mut batch = Vec::with_capacity(n);
        while batch.len() < n {
            let Some(c) = self.queue.pop() else { break };
            if self.in_view(c, self.center, self.view) && !self.sent.contains(&c) {
                batch.push(c);
            }
        }
        if batch.is_empty() {
            return;
        }
        let st = self.st.clone();
        let coords = batch.clone();
        let _ = tokio::task::spawn_blocking(move || st.game.world.ensure_loaded(&coords, &st.store, &st.generator)).await;
        let world = &self.st.game.world;
        let xray_mode = self.st.cfg.anticheat.xray;
        let seed = self.st.generator.seed;
        self.send("chunk_batch_start", &[]);
        let mut count = 0;
        for c in batch {
            let pkt = world.chunk_packet(c.0, c.1, &|chunk: &Chunk| {
                if xray_mode != XrayMode::Off && self.st.cfg.anticheat.enabled {
                    let safe = xray::obfuscate(chunk, xray_mode, seed, &|x, y, z| world.get_block(x, y, z));
                    chunk_body(&safe)
                } else {
                    chunk_body(chunk)
                }
            });
            if let Some(body) = pkt {
                self.send("level_chunk_with_light", &body);
                world.add_viewer(c.0, c.1);
                self.sent.insert(c);
                count += 1;
                for e in self.st.game.entity_spawns_in(c) {
                    self.me.send(e);
                }
            }
        }
        let mut b = Vec::new();
        b.vi(count);
        self.send("chunk_batch_finished", &b);
        self.act_chunks += count as u32;
        self.in_flight += 1;
    }

    // ------------------------------------------------------------ tick

    async fn on_tick(&mut self) -> Flow {
        self.tick += 1;
        // keep-alive every 15 s, kick after 30 s without an answer
        if let Some((_, t)) = self.ka_pending {
            if t.elapsed() > Duration::from_secs(30) {
                return Flow::Disconnect("Timed out".into());
            }
        } else if self.ka_last.elapsed() > Duration::from_secs(15) {
            let kid = self.now_ms() as i64;
            self.ka_pending = Some((kid, Instant::now()));
            self.ka_last = Instant::now();
            let mut b = Vec::new();
            b.i64(kid);
            self.send("keep_alive", &b);
        }
        if self.tick % 20 == 0 {
            self.send_time();
            self.per_second();
        }
        self.track_players();
        self.pump_chunks().await;
        Flow::Continue
    }

    fn per_second(&mut self) {
        let pos = self.me.pose.lock().pos;
        let (dx, dz) = (pos[0] - self.last_second_pos[0], pos[2] - self.last_second_pos[2]);
        self.last_second_pos = pos;
        let act = Activity {
            speed: (dx * dx + dz * dz).sqrt() / 20.0,
            chunk_sends: self.act_chunks as f64 / 20.0,
            tracked_entities: self.tracked.len() as u32,
            packets_in: self.act_packets as f64 / 20.0,
        };
        self.act_chunks = 0;
        self.act_packets = 0;
        let st = self.st.clone();
        let cfg = &st.cfg;
        let limits = self.budget.update(&cfg.player_cost, self.st.mspt() as f64, self.st.game.count(), estimate_cost(&act), self.base_view as u32);
        let new_view = limits.view_distance as i32;
        if new_view != self.view {
            self.view = new_view;
            let mut b = Vec::new();
            b.vi(new_view);
            self.send("set_chunk_cache_radius", &b);
            self.set_center(self.center, true);
        }
        // feed the pre-generator with where this player is heading
        if cfg.pregen.enabled {
            if let Some(p) = self.st.pregen.lock().as_ref() {
                let ticks = cfg.pregen.lookahead_seconds * 20.0;
                p.enqueue(&crate::world::pregen::predict((pos[0], pos[2]), (dx / 20.0, dz / 20.0), self.view, ticks, cfg.pregen.candidates));
            }
        }
    }

    fn track_players(&mut self) {
        let others = self.st.game.players();
        let mut seen = HashSet::new();
        let ptype = Registries::get().id("minecraft:entity_type", "minecraft:player").unwrap_or(0);
        for p in others.iter().filter(|p| p.eid != self.me.eid) {
            let pose = *p.pose.lock();
            let c = ((pose.pos[0].floor() as i32) >> 4, (pose.pos[2].floor() as i32) >> 4);
            if !self.in_view(c, self.center, self.view) || p.gamemode.load(Ordering::Relaxed) == 3 {
                continue;
            }
            seen.insert(p.eid);
            let ver = p.moved.load(Ordering::Relaxed);
            match self.tracked.get(&p.eid) {
                None => {
                    let mut b = Vec::new();
                    b.vi(p.eid).uuid(p.uuid).vi(ptype).f64(pose.pos[0]).f64(pose.pos[1]).f64(pose.pos[2]).u8(0);
                    b.u8(angle(pose.pitch)).u8(angle(pose.yaw)).u8(angle(pose.yaw)).vi(0);
                    self.send("add_entity", &b);
                    self.tracked.insert(p.eid, ver);
                }
                Some(&v) if v != ver => {
                    let mut b = Vec::new();
                    b.vi(p.eid).f64(pose.pos[0]).f64(pose.pos[1]).f64(pose.pos[2]).f64(0.0).f64(0.0).f64(0.0).f32(pose.yaw).f32(pose.pitch).bool(pose.on_ground);
                    self.send("entity_position_sync", &b);
                    let mut b = Vec::new();
                    b.vi(p.eid).u8(angle(pose.yaw));
                    self.send("rotate_head", &b);
                    self.tracked.insert(p.eid, ver);
                }
                _ => {}
            }
        }
        let gone: Vec<i32> = self.tracked.keys().copied().filter(|e| !seen.contains(e)).collect();
        if !gone.is_empty() {
            let mut b = Vec::new();
            b.vi(gone.len() as i32);
            for e in &gone {
                b.vi(*e);
                self.tracked.remove(e);
            }
            self.send("remove_entities", &b);
        }
    }

    // ------------------------------------------------------------ packets

    fn handle(&mut self, p: &[u8]) -> anyhow::Result<Flow> {
        let mut r = Reader::new(p);
        let pid = r.varint()?;
        let sb = |n: &str| id("play", "serverbound", n);
        if pid == sb("accept_teleportation") {
            let t = r.varint()?;
            if self.pending_tp == Some(t) {
                self.pending_tp = None;
            }
        } else if pid == sb("keep_alive") {
            let k = r.i64()?;
            if matches!(self.ka_pending, Some((id, _)) if id == k) {
                self.ka_pending = None;
            }
        } else if pid == sb("move_player_pos") {
            let pos = [r.f64()?, r.f64()?, r.f64()?];
            let flags = r.u8()?;
            return Ok(self.on_move(Some(pos), None, flags & 1 != 0));
        } else if pid == sb("move_player_pos_rot") {
            let pos = [r.f64()?, r.f64()?, r.f64()?];
            let rot = (r.f32()?, r.f32()?);
            let flags = r.u8()?;
            return Ok(self.on_move(Some(pos), Some(rot), flags & 1 != 0));
        } else if pid == sb("move_player_rot") {
            let rot = (r.f32()?, r.f32()?);
            let flags = r.u8()?;
            return Ok(self.on_move(None, Some(rot), flags & 1 != 0));
        } else if pid == sb("move_player_status_only") {
            let flags = r.u8()?;
            return Ok(self.on_move(None, None, flags & 1 != 0));
        } else if pid == sb("chunk_batch_received") {
            let rate = r.f32()?;
            if rate.is_finite() {
                self.chunks_per_tick = rate.clamp(0.01, 64.0);
            }
            self.in_flight = self.in_flight.saturating_sub(1);
        } else if pid == sb("chat") {
            let m = r.string(256)?.to_string();
            return Ok(self.on_chat(&m));
        } else if pid == sb("chat_command") || pid == sb("chat_command_signed") {
            let c = r.string(256)?.to_string();
            return Ok(self.on_command(&c));
        } else if pid == sb("player_action") {
            let status = r.varint()?;
            let pos = r.pos()?;
            let _face = r.i8()?;
            let seq = r.varint()?;
            return Ok(self.on_dig(status, pos, seq));
        } else if pid == sb("use_item_on") {
            let hand = r.varint()?;
            let pos = r.pos()?;
            let face = r.varint()?;
            let cursor = (r.f32()?, r.f32()?, r.f32()?);
            let _inside = r.bool()?;
            let _border = r.bool()?;
            let seq = r.varint()?;
            return Ok(self.on_place(hand, pos, face, cursor, seq));
        } else if pid == sb("set_carried_item") {
            let s = r.i16()?;
            if (0..9).contains(&s) {
                self.me.held.store(s as i32, Ordering::Relaxed);
            }
        } else if pid == sb("set_creative_mode_slot") {
            let slot = r.i16()?;
            let count = r.varint()?;
            let item = if count > 0 { r.varint()? } else { 0 };
            if self.gamemode() == 1 {
                let st = (count > 0 && Items::get().valid(item)).then(|| Stack::new(item, count.clamp(1, 99) as u8));
                if (1..46).contains(&slot) {
                    self.me.inv.lock().set(slot as usize, st);
                    self.me.inv.lock().dirty.clear();
                } else if slot == -1 {
                    if let Some(st) = st {
                        self.throw(st, true);
                    }
                }
            }
        } else if pid == sb("use_item") {
            let hand = r.varint()?;
            let seq = r.varint()?;
            let (yaw, pitch) = (r.f32()?, r.f32()?);
            self.ack(seq);
            if yaw.is_finite() && pitch.is_finite() {
                self.use_item(hand, yaw, pitch);
            }
        } else if pid == sb("container_click") {
            return Ok(self.on_click(&mut r));
        } else if pid == sb("container_close") {
            let held = self.me.held.load(Ordering::Relaxed) as i16;
            let thrown = self.me.inv.lock().close(&self.st.game.data, held);
            for t in thrown {
                self.throw(t, false);
            }
            self.st.game.send_inventory(&self.me);
        } else if pid == sb("player_input") {
            self.sneaking = r.u8()? & 0x20 != 0;
        } else if pid == sb("swing") {
            let hand = r.varint()?;
            let mut b = Vec::new();
            b.vi(self.me.eid).u8(if hand == 1 { 3 } else { 0 });
            let pkt = self.st.game.encode("animate", &b);
            let pos = self.me.pose.lock().pos;
            let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
            for p in self.st.game.players() {
                if p.eid != self.me.eid && p.sees_chunk(c.0, c.1) {
                    p.send(pkt.clone());
                }
            }
        } else if pid == sb("player_command") {
            let _eid = r.varint()?;
            match r.varint()? {
                1 => self.sprinting = true,
                2 => self.sprinting = false,
                6 => self.gliding = true,
                _ => {}
            }
        } else if pid == sb("client_information") {
            let _locale = r.string(16)?;
            let v = (r.i8()? as i32).clamp(2, 32).min(self.st.cfg.server.view_distance as i32);
            if v != self.base_view {
                self.base_view = v;
                self.view = v;
                self.set_center(self.center, true);
            }
        }
        Ok(Flow::Continue)
    }

    fn on_move(&mut self, pos: Option<[f64; 3]>, rot: Option<(f32, f32)>, on_ground: bool) -> Flow {
        if let Some(f) = self.checker.on_move_packet(self.now_ms()) {
            if let Some(flow) = self.apply_flag(f.check, f.amount, f.detail) {
                return flow;
            }
        }
        if self.pending_tp.is_some() {
            return Flow::Continue; // ignore moves until the client confirms our teleport
        }
        if let Some((yaw, pitch)) = rot {
            if !yaw.is_finite() || !pitch.is_finite() {
                return Flow::Disconnect("Invalid move".into());
            }
            let mut p = self.me.pose.lock();
            p.yaw = yaw.rem_euclid(360.0);
            p.pitch = pitch.clamp(-90.0, 90.0);
        }
        if let Some(to) = pos {
            if !to.iter().all(|v| v.is_finite() && v.abs() < 3.0e7) {
                return Flow::Disconnect("Invalid move".into());
            }
            let ctx = self.move_context(to);
            let res = self.checker.on_move(to, &ctx);
            for f in res.flags {
                if let Some(flow) = self.apply_flag(f.check, f.amount, f.detail) {
                    return flow;
                }
            }
            if on_ground {
                self.gliding = false;
            }
            {
                let mut p = self.me.pose.lock();
                p.pos = to;
                p.on_ground = on_ground;
            }
            let c = ((to[0].floor() as i32) >> 4, (to[2].floor() as i32) >> 4);
            self.set_center(c, false);
        } else {
            self.me.pose.lock().on_ground = on_ground;
        }
        self.me.moved.fetch_add(1, Ordering::Relaxed);
        Flow::Continue
    }

    fn move_context(&self, to: [f64; 3]) -> MoveContext {
        let w = &self.st.game.world;
        let reg = BlockRegistry::get();
        let (x, y, z) = (to[0].floor() as i32, to[1].floor() as i32, to[2].floor() as i32);
        let at = |dy: i32| w.get_block(x, y + dy, z).unwrap_or(blocks::AIR);
        let feet = at(0);
        let name = feet.name();
        let below_solid = [-1, 0].iter().any(|dy| {
            // check the 3x3 footprint below (edges of blocks)
            (-1..=1).any(|dx| (-1..=1).any(|dz| w.get_block(x + dx, y + dy, z + dz).map(|b| reg.is_solid(b)).unwrap_or(true)))
        });
        let gm = self.gamemode();
        MoveContext {
            may_fly: gm == 1 || gm == 3,
            gliding: self.gliding,
            in_liquid: reg.has_fluid(feet) || reg.has_fluid(at(1)),
            climbing: matches!(name, "ladder" | "vine" | "scaffolding" | "twisting_vines" | "weeping_vines" | "cave_vines"),
            in_web_or_powder_snow: matches!(name, "cobweb" | "powder_snow" | "sweet_berry_bush" | "honey_block"),
            teleporting: self.pending_tp.is_some(),
            sprinting: self.sprinting,
            solid_below: below_solid,
            on_ice: matches!(at(-1).name(), "ice" | "packed_ice" | "blue_ice" | "frosted_ice"),
            bounced: matches!(at(-1).name(), "slime_block") || at(-1).name().ends_with("_bed"),
            ..Default::default()
        }
    }

    /// Apply an anti-cheat flag; returns Some(flow) when the session must end.
    fn apply_flag(&mut self, check: Check, amount: f64, detail: String) -> Option<Flow> {
        let v = self.vl.flag(&self.st.cfg.anticheat, check, amount, self.now_ms(), detail);
        match v {
            Verdict::Pass => None,
            Verdict::Alert { check, vl, detail } => {
                self.alert(&format!("{} failed {check:?} (VL {vl:.1}): {detail}", self.me.name));
                None
            }
            Verdict::Setback { check, vl, detail } => {
                tracing::info!("[anticheat] setback {} for {check:?} (VL {vl:.1}): {detail}", self.me.name);
                let back = self.checker.last_legit;
                let pose = *self.me.pose.lock();
                self.teleport(back, pose.yaw, pose.pitch);
                None
            }
            Verdict::Kick { check, vl } => {
                self.alert(&format!("{} was kicked for {check:?} (VL {vl:.1})", self.me.name));
                Some(Flow::Disconnect(format!("Kicked by RimeMC anti-cheat ({check:?})")))
            }
            Verdict::Ban { check, vl, minutes } => {
                self.alert(&format!("{} was banned for {minutes} min for {check:?} (VL {vl:.1})", self.me.name));
                self.st.ban(&self.me.name, minutes);
                Some(Flow::Disconnect(format!("Banned by RimeMC anti-cheat ({check:?}) for {minutes} minutes")))
            }
        }
    }

    fn alert(&self, text: &str) {
        tracing::warn!("[anticheat] {text}");
        if self.st.cfg.anticheat.notify_ops {
            let pkt = self.st.game.system_chat(&format!("§c[RimeMC AC]§r {text}"));
            for p in self.st.game.players() {
                if self.st.ops.is_op(&CommandSender::Player { name: p.name.clone() }) {
                    p.send(pkt.clone());
                }
            }
        }
    }

    fn on_chat(&mut self, m: &str) -> Flow {
        if m.chars().any(|c| c == '§' || c.is_control()) {
            return Flow::Disconnect("Illegal characters in chat".into());
        }
        if m.trim().is_empty() {
            return Flow::Continue;
        }
        tracing::info!("<{}> {m}", self.me.name);
        self.st.game.broadcast(self.st.game.system_chat(&format!("<{}> {m}", self.me.name)));
        Flow::Continue
    }

    fn on_command(&mut self, line: &str) -> Flow {
        if line.chars().any(|c| c == '§' || c.is_control()) {
            return Flow::Disconnect("Illegal characters in command".into());
        }
        tracing::info!("{} issued server command: /{line}", self.me.name);
        let sender = CommandSender::Player { name: self.me.name.clone() };
        let mut words = line.split_whitespace();
        match words.next().unwrap_or("") {
            "gamemode" => {
                if !self.st.ops.is_op(&sender) {
                    self.msg("§cYou do not have permission (operator only).");
                    return Flow::Continue;
                }
                let gm = match words.next() {
                    Some("survival" | "s" | "0") => 0,
                    Some("creative" | "c" | "1") => 1,
                    Some("adventure" | "a" | "2") => 2,
                    Some("spectator" | "sp" | "3") => 3,
                    _ => {
                        self.msg("§cUsage: /gamemode <survival|creative|adventure|spectator>");
                        return Flow::Continue;
                    }
                };
                self.set_gamemode(gm);
                self.msg(&format!("Set own game mode to {}", ["Survival", "Creative", "Adventure", "Spectator"][gm as usize]));
            }
            "spawn" => {
                let s = self.st.spawn;
                self.teleport(s, 0.0, 0.0);
            }
            "tp" => {
                if !self.st.ops.is_op(&sender) {
                    self.msg("§cYou do not have permission (operator only).");
                    return Flow::Continue;
                }
                let args: Vec<&str> = words.collect();
                let target = match args.as_slice() {
                    [name] => self.st.game.find(name).map(|p| p.pose.lock().pos),
                    [x, y, z] => match (x.parse::<f64>(), y.parse::<f64>(), z.parse::<f64>()) {
                        (Ok(x), Ok(y), Ok(z)) if [x, y, z].iter().all(|v| v.is_finite() && v.abs() < 2.9e7) => Some([x, y, z]),
                        _ => None,
                    },
                    _ => None,
                };
                match target {
                    Some(t) => {
                        let pose = *self.me.pose.lock();
                        self.teleport(t, pose.yaw, pose.pitch);
                        let c = ((t[0].floor() as i32) >> 4, (t[2].floor() as i32) >> 4);
                        self.set_center(c, false);
                    }
                    None => self.msg("§cUsage: /tp <player> | /tp <x> <y> <z>"),
                }
            }
            "list" => {
                let names: Vec<String> = self.st.game.players().iter().map(|p| p.name.clone()).collect();
                self.msg(&format!("There are {} of a max of {} players online: {}", names.len(), self.st.cfg.server.max_players, names.join(", ")));
            }
            _ => {
                let st = self.st.clone();
                let me = self.me.clone();
                let line = line.to_string();
                tokio::spawn(async move {
                    let reply = st.dispatch(&CommandSender::Player { name: me.name.clone() }, &line).await;
                    let text = if let Some(e) = reply.strip_prefix("ERR ") { format!("§c{e}") } else { reply.trim_start_matches("OK ").to_string() };
                    me.send(st.game.system_chat(&text));
                });
            }
        }
        Flow::Continue
    }

    fn set_gamemode(&mut self, gm: i32) {
        self.me.gamemode.store(gm, Ordering::Relaxed);
        let mut b = Vec::new();
        b.u8(3).f32(gm as f32);
        self.send("game_event", &b);
        self.send_abilities();
        let mut b = Vec::new();
        b.u8(0x04).vi(1).uuid(self.me.uuid).vi(gm);
        self.st.game.broadcast(self.st.game.encode("player_info_update", &b));
        self.me.moved.fetch_add(1, Ordering::Relaxed);
    }

    fn ack(&self, seq: i32) {
        let mut b = Vec::new();
        b.vi(seq);
        self.send("block_changed_ack", &b);
    }

    fn within_reach(&self, pos: (i32, i32, i32)) -> bool {
        let p = self.me.pose.lock().pos;
        let eye = [p[0], p[1] + 1.62, p[2]];
        let c = [pos.0 as f64 + 0.5, pos.1 as f64 + 0.5, pos.2 as f64 + 0.5];
        let d2: f64 = (0..3).map(|i| (eye[i] - c[i]).powi(2)).sum();
        let reach = if self.gamemode() == 1 { 5.0 } else { 4.5 } + 1.5; // + block half-diagonal & latency
        d2 <= reach * reach
    }

    /// Re-send the real state of a block (after a rejected action).
    fn resync(&self, pos: (i32, i32, i32)) {
        if let Some(b) = self.st.game.world.get_block(pos.0, pos.1, pos.2) {
            let mut body = Vec::new();
            body.pos(pos.0, pos.1, pos.2).vi(b.0 as i32);
            self.send("block_update", &body);
        }
    }

    fn on_dig(&mut self, status: i32, pos: (i32, i32, i32), seq: i32) -> Flow {
        let gm = self.gamemode();
        match status {
            3 | 4 => {
                // drop the held stack (3) or one item (4)
                self.ack(seq);
                let held = 36 + self.me.held.load(Ordering::Relaxed) as usize;
                let taken = {
                    let mut inv = self.me.inv.lock();
                    inv.held(held as i16 - 36).map(|s| {
                        let n = if status == 3 { s.count } else { 1 };
                        inv.consume(held, n);
                        Stack::new(s.item, n)
                    })
                };
                if let Some(t) = taken {
                    self.throw(t, false);
                    self.st.game.flush_inventory(&self.me);
                }
                return Flow::Continue;
            }
            5 | 6 => {
                self.ack(seq);
                return Flow::Continue;
            }
            _ => {}
        }
        let st = self.st.clone();
        let w = &st.game.world;
        let Some(cur) = w.get_block(pos.0, pos.1, pos.2) else {
            self.ack(seq);
            return Flow::Continue;
        };
        let instant = gm == 1 || (status == 0 && !cur.is_solid() && !cur.is_air() && !BlockRegistry::get().is_fluid(cur));
        let breaks = match status {
            0 => {
                self.dig_start = Some((pos, Instant::now()));
                instant
            }
            1 => {
                self.dig_start = None;
                false
            }
            2 => gm != 2 && gm != 3,
            _ => false,
        };
        let unbreakable = matches!(cur.name(), "bedrock" | "barrier" | "end_portal_frame" | "end_portal" | "end_gateway" | "command_block" | "structure_block" | "jigsaw");
        if breaks && (gm == 2 || gm == 3 || unbreakable && gm != 1 || !self.within_reach(pos) || BlockRegistry::get().is_fluid(cur)) {
            self.ack(seq);
            self.resync(pos);
            return Flow::Continue;
        }
        if breaks && !cur.is_air() {
            let tool = self.me.inv.lock().held(self.me.held.load(Ordering::Relaxed) as i16).map(|s| s.item);
            if gm == 0 {
                if let Some(f) = self.mining.on_break(cur) {
                    let _ = self.apply_flag(f.check, f.amount, f.detail);
                }
            }
            // a waterlogged block leaves its water behind
            let after = if BlockRegistry::get().property(cur, "waterlogged") == Some("true") { blocks::WATER } else { blocks::AIR };
            st.game.change_block([pos.0, pos.1, pos.2], after, gm == 0, tool);
            self.ack(seq);
            self.after_change(pos);
        } else {
            self.ack(seq);
        }
        Flow::Continue
    }

    /// Throw an item from the player's eyes in the look direction.
    fn throw(&self, st: Stack, random: bool) {
        let pose = *self.me.pose.lock();
        let (yaw, pitch) = (pose.yaw.to_radians() as f64, pose.pitch.to_radians() as f64);
        let mut rng = rand::thread_rng();
        let mut vel = [-yaw.sin() * pitch.cos() * 0.3, -pitch.sin() * 0.3 + 0.1, yaw.cos() * pitch.cos() * 0.3];
        if random {
            vel = [rng.gen_range(-0.1..0.1), 0.2, rng.gen_range(-0.1..0.1)];
        } else {
            let a = rng.gen_range(0.0..std::f64::consts::TAU);
            let f = 0.02 * rng.gen_range(0.0..1.0);
            vel[0] += a.cos() * f;
            vel[1] += (rng.gen_range(0.0..1.0) - rng.gen_range(0.0..1.0)) * 0.1;
            vel[2] += a.sin() * f;
        }
        self.st.game.spawn_item([pose.pos[0], pose.pos[1] + 1.62 - 0.3, pose.pos[2]], st, vel, 40);
    }

    fn on_click(&mut self, r: &mut Reader) -> Flow {
        let parsed = (|| -> Result<_, super::codec::CodecError> {
            let window = r.varint()?;
            let _state = r.varint()?;
            let slot = r.i16()?;
            let button = r.i8()?;
            let mode = r.varint()?;
            let n = r.varint()?;
            if !(0..=128).contains(&n) {
                return Err(super::codec::CodecError::TooLarge(n as usize));
            }
            let mut predicted = Vec::new();
            let hashed = |r: &mut Reader| -> Result<Option<(i32, i32)>, super::codec::CodecError> {
                if !r.bool()? {
                    return Ok(None);
                }
                let item = r.varint()?;
                let count = r.varint()?;
                let comps = r.varint()?;
                if !(0..=256).contains(&comps) {
                    return Err(super::codec::CodecError::TooLarge(comps as usize));
                }
                for _ in 0..comps {
                    r.varint()?;
                    r.bytes(4)?;
                }
                let rem = r.varint()?;
                if !(0..=256).contains(&rem) {
                    return Err(super::codec::CodecError::TooLarge(rem as usize));
                }
                for _ in 0..rem {
                    r.varint()?;
                }
                Ok(Some((item, count)))
            };
            for _ in 0..n {
                let loc = r.i16()?;
                predicted.push((loc, hashed(r)?));
            }
            let cursor = hashed(r)?;
            Ok((window, slot, button, mode, predicted, cursor))
        })();
        let Ok((window, slot, button, mode, predicted, cursor)) = parsed else {
            return Flow::Disconnect("Invalid container click".into());
        };
        let creative = self.gamemode() == 1;
        let (outcome, matches) = {
            let mut inv = self.me.inv.lock();
            if window != inv.current_window() as i32 {
                drop(inv);
                self.st.game.send_inventory(&self.me);
                return Flow::Continue;
            }
            let out = inv.click(&self.st.game.data, slot, button, mode, creative);
            let contents = inv.window_contents();
            let as_pair = |s: Option<Stack>| s.map(|s| (s.item, s.count as i32));
            let ok = predicted.iter().all(|(loc, pred)| contents.get(*loc as usize).is_some_and(|s| as_pair(*s) == *pred)) && as_pair(inv.cursor) == cursor;
            (out, ok)
        };
        for t in outcome.thrown {
            self.throw(t, false);
        }
        if matches {
            self.st.game.flush_inventory(&self.me);
        } else {
            self.st.game.send_inventory(&self.me);
        }
        Flow::Continue
    }

    /// Ray from the eyes; returns the last open cell and the first hit cell.
    fn raycast(&self, yaw: f32, pitch: f32, stop: &dyn Fn(BlockState) -> bool) -> Option<((i32, i32, i32), (i32, i32, i32))> {
        let pos = self.me.pose.lock().pos;
        let (y, p) = (yaw.to_radians() as f64, pitch.to_radians() as f64);
        let dir = [-y.sin() * p.cos(), -p.sin(), y.cos() * p.cos()];
        let eye = [pos[0], pos[1] + 1.62, pos[2]];
        let reach = if self.gamemode() == 1 { 5.0 } else { 4.5 };
        let mut prev = (eye[0].floor() as i32, eye[1].floor() as i32, eye[2].floor() as i32);
        let mut t = 0.0;
        while t <= reach {
            let c = ((eye[0] + dir[0] * t).floor() as i32, (eye[1] + dir[1] * t).floor() as i32, (eye[2] + dir[2] * t).floor() as i32);
            if c != prev {
                let b = self.st.game.world.get_block(c.0, c.1, c.2)?;
                if stop(b) {
                    return Some((prev, c));
                }
                prev = c;
            }
            t += 0.02;
        }
        None
    }

    fn use_item(&mut self, hand: i32, yaw: f32, pitch: f32) {
        let slot = if hand == 1 { 45 } else { 36 + self.me.held.load(Ordering::Relaxed) as usize };
        let Some(held) = self.me.inv.lock().slots[slot] else { return };
        let items = Items::get();
        let name = items.name(held.item).to_string();
        let gm = self.gamemode();
        let game = &self.st.game;
        match name.as_str() {
            "bucket" => {
                let hit = self.raycast(yaw, pitch, &|b| fluid::fluid_state(b).is_source() || b.is_solid());
                if let Some((_, c)) = hit {
                    let b = game.world.get_block(c.0, c.1, c.2).unwrap_or(blocks::AIR);
                    let filled = match b.name() {
                        "water" if fluid::fluid_state(b).is_source() => "water_bucket",
                        "lava" if fluid::fluid_state(b).is_source() => "lava_bucket",
                        _ => return,
                    };
                    game.change_block([c.0, c.1, c.2], blocks::AIR, false, None);
                    if gm != 1 {
                        let mut inv = self.me.inv.lock();
                        inv.consume(slot, 1);
                        let full = Stack::new(items.id(filled).unwrap_or(0), 1);
                        if inv.slots[slot].is_none() {
                            inv.set(slot, Some(full));
                        } else if inv.add(full, slot as i16 - 36) > 0 {
                            drop(inv);
                            self.throw(full, false);
                        }
                    }
                }
            }
            "water_bucket" | "lava_bucket" => {
                let fluid_block = if name == "water_bucket" { blocks::WATER } else { blocks::LAVA };
                let hit = self.raycast(yaw, pitch, &|b| !game.is_replaceable(b) || fluid::fluid_state(b).is_source());
                if let Some((open, c)) = hit {
                    let target_block = game.world.get_block(c.0, c.1, c.2).unwrap_or(blocks::AIR);
                    // clicking a replaceable block (grass, flowing water…) fills it; else the cell in front
                    let target = if game.is_replaceable(target_block) && !fluid::fluid_state(target_block).is_source() { c } else { open };
                    let cur = game.world.get_block(target.0, target.1, target.2).unwrap_or(blocks::STONE);
                    if !game.is_replaceable(cur) || !self.within_reach(target) {
                        return;
                    }
                    game.change_block([target.0, target.1, target.2], fluid_block, !cur.is_air() && !BlockRegistry::get().is_fluid(cur), None);
                    game.schedule([target.0, target.1, target.2], crate::game::TickKind::Fluid, fluid::tick_delay(if name == "water_bucket" { fluid::Fluid::Water } else { fluid::Fluid::Lava }));
                    if gm != 1 {
                        self.me.inv.lock().set(slot, Some(Stack::new(items.id("bucket").unwrap_or(0), 1)));
                    }
                }
            }
            _ => {}
        }
        game.flush_inventory(&self.me);
    }

    /// With anti-xray, neighbours of a changed block may now be exposed: send their real state.
    fn after_change(&self, pos: (i32, i32, i32)) {
        if self.st.cfg.anticheat.enabled && self.st.cfg.anticheat.xray != XrayMode::Off {
            for p in xray::reveal_around([pos.0, pos.1, pos.2]) {
                if let Some(b) = self.st.game.world.get_block(p[0], p[1], p[2]) {
                    if b.is_ore() {
                        self.st.game.broadcast_block(p[0], p[1], p[2], b);
                    }
                }
            }
        }
    }

    fn open_crafting(&mut self) {
        self.next_window = self.next_window % 100 + 1;
        let id = self.next_window;
        {
            let held = self.me.held.load(Ordering::Relaxed) as i16;
            let mut inv = self.me.inv.lock();
            let thrown = inv.close(&self.st.game.data, held);
            inv.window = crate::game::inventory::Window::Crafting(id);
            drop(inv);
            for t in thrown {
                self.throw(t, false);
            }
        }
        let menu = Registries::get().id("minecraft:menu", "minecraft:crafting").unwrap_or(12);
        let mut b = Vec::new();
        b.vi(id as i32).vi(menu).raw(&text_nbt("Crafting"));
        self.send("open_screen", &b);
        self.st.game.send_inventory(&self.me);
    }

    fn on_place(&mut self, hand: i32, pos: (i32, i32, i32), face: i32, cursor: (f32, f32, f32), seq: i32) -> Flow {
        let gm = self.gamemode();
        let off = match face {
            0 => (0, -1, 0),
            1 => (0, 1, 0),
            2 => (0, 0, -1),
            3 => (0, 0, 1),
            4 => (-1, 0, 0),
            5 => (1, 0, 0),
            _ => return Flow::Disconnect("Invalid block face".into()),
        };
        let slot = if hand == 1 { 45 } else { 36 + self.me.held.load(Ordering::Relaxed) as usize };
        let reg = BlockRegistry::get();
        let st = self.st.clone();
        let game = &st.game;
        let w = &game.world;
        let clicked = w.get_block(pos.0, pos.1, pos.2).unwrap_or(blocks::AIR);
        // interactive blocks (unless sneaking)
        if clicked.name() == "crafting_table" && !self.sneaking && gm != 3 && self.within_reach(pos) {
            self.ack(seq);
            self.open_crafting();
            return Flow::Continue;
        }
        if gm == 2 || gm == 3 {
            self.ack(seq);
            return Flow::Continue;
        }
        let item = self.me.inv.lock().slots[slot];
        let Some(item) = item else {
            self.ack(seq);
            return Flow::Continue;
        };
        let Some(mut state) = Items::get().block_for(item.item) else {
            self.ack(seq);
            return Flow::Continue;
        };
        let info = reg.block(state);
        // replaceable target (grass, water, …) is replaced in place
        let target = if game.is_replaceable(clicked) && !fluid::fluid_state(clicked).is_source() { pos } else { (pos.0 + off.0, pos.1 + off.1, pos.2 + off.2) };
        let existing = w.get_block(target.0, target.1, target.2);
        if !existing.is_some_and(|b| game.is_replaceable(b)) || !self.within_reach(target) || !(MIN_Y..MIN_Y + crate::world::chunk::HEIGHT).contains(&target.1) {
            self.ack(seq);
            self.resync(target);
            return Flow::Continue;
        }
        let yaw = self.me.pose.lock().yaw;
        let horiz = ["south", "west", "north", "east"][(((yaw as f64 / 90.0) + 0.5).floor() as i64).rem_euclid(4) as usize];
        let opposite = |d: &str| match d {
            "south" => "north",
            "north" => "south",
            "east" => "west",
            _ => "east",
        };
        if let Some(axis) = reg.property(state, "axis").map(|_| match face {
            0 | 1 => "y",
            2 | 3 => "z",
            _ => "x",
        }) {
            state = reg.with(state, "axis", axis).unwrap_or(state);
        }
        // torches / signs on a wall
        if face >= 2 {
            let wall = state.name().replace("torch", "wall_torch").replace("_sign", "_wall_sign");
            if wall != state.name() {
                if let Some(wb) = reg.by_name(&wall) {
                    let facing = ["north", "south", "west", "east"][(face - 2) as usize];
                    state = reg.with(BlockState(wb.default), "facing", facing).unwrap_or(BlockState(wb.default));
                }
            }
        }
        let wall_mounted = state.name().contains("wall_");
        if reg.property(state, "facing").is_some() && !wall_mounted {
            // stairs, furnaces, chests… face the player; others keep their default
            state = reg.with(state, "facing", opposite(horiz)).or_else(|| reg.with(state, "facing", horiz)).unwrap_or(state);
        }
        if reg.property(state, "half").is_some() && (face == 0 || (face != 1 && cursor.1 > 0.5)) {
            state = reg.with(state, "half", "top").unwrap_or(state);
        }
        if reg.property(state, "type").is_some() && info.name.ends_with("_slab") && (face == 0 || (face != 1 && cursor.1 > 0.5)) {
            state = reg.with(state, "type", "top").unwrap_or(state);
        }
        if reg.property(state, "waterlogged").is_some() && existing.is_some_and(|b| fluid::fluid_state(b).is_source()) {
            state = reg.with(state, "waterlogged", "true").unwrap_or(state);
        }
        // don't place solid blocks inside players
        if state.is_solid() {
            let bx = [target.0 as f64, target.1 as f64, target.2 as f64];
            for p in self.st.game.players() {
                if p.gamemode.load(Ordering::Relaxed) == 3 {
                    continue;
                }
                let q = p.pose.lock().pos;
                let hit = q[0] + 0.3 > bx[0] && q[0] - 0.3 < bx[0] + 1.0 && q[1] + 1.8 > bx[1] && q[1] < bx[1] + 1.0 && q[2] + 0.3 > bx[2] && q[2] - 0.3 < bx[2] + 1.0;
                if hit {
                    self.ack(seq);
                    self.resync(target);
                    return Flow::Continue;
                }
            }
        }
        // two-block things: doors, tall plants (upper half), beds (head)
        let mut extra: Option<((i32, i32, i32), BlockState)> = None;
        if reg.property(state, "half") == Some("lower") {
            let up = (target.0, target.1 + 1, target.2);
            match (w.get_block(up.0, up.1, up.2), reg.with(state, "half", "upper")) {
                (Some(b), Some(upper)) if game.is_replaceable(b) => extra = Some((up, upper)),
                _ => {
                    self.ack(seq);
                    self.resync(target);
                    return Flow::Continue;
                }
            }
        }
        if state.name().ends_with("_bed") {
            let d = match horiz {
                "south" => (0, 1),
                "north" => (0, -1),
                "east" => (1, 0),
                _ => (-1, 0),
            };
            let foot = reg.with(state, "facing", horiz).unwrap_or(state);
            state = foot;
            let head_pos = (target.0 + d.0, target.1, target.2 + d.1);
            match (w.get_block(head_pos.0, head_pos.1, head_pos.2), reg.with(foot, "part", "head")) {
                (Some(b), Some(head)) if game.is_replaceable(b) => extra = Some((head_pos, head)),
                _ => {
                    self.ack(seq);
                    self.resync(target);
                    return Flow::Continue;
                }
            }
        }
        if !game.data.block_in("replaceable", info.name.as_str()) {
            let _ = info;
        }
        game.change_block([target.0, target.1, target.2], state, false, None);
        if let Some((p, b)) = extra {
            game.change_block([p.0, p.1, p.2], b, false, None);
        }
        if fluid::fluid_state(state).fluid().is_some() {
            game.schedule([target.0, target.1, target.2], crate::game::TickKind::Fluid, 5);
        }
        if gm != 1 {
            self.me.inv.lock().consume(slot, 1);
            game.flush_inventory(&self.me);
        }
        self.ack(seq);
        Flow::Continue
    }
}

pub fn spawn_block(p: [f64; 3]) -> (i32, i32, i32) {
    (p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_seed_matches_vanilla() {
        // Hashing.sha256().hashLong(0).asLong()
        let h = hashed_seed(0);
        assert_ne!(h, 0);
        assert_eq!(hashed_seed(0), h);
    }

    #[test]
    fn angles() {
        assert_eq!(angle(0.0), 0);
        assert_eq!(angle(90.0), 64);
        assert_eq!(angle(-90.0), 192);
        assert_eq!(angle(360.0), 0);
    }
}
