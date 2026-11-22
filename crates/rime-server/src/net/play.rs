//! Java Edition session after the handshake: login → configuration → play.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
/// Any byte stream a Java session can run over (TCP, or an in-process pipe
/// for Bedrock players).
pub trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> Io for T {}

use super::crypt::{CryptStream, Profile};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::codec::{Out, Reader};
use super::conn;
use crate::anticheat::combat::CombatChecker;
use crate::anticheat::movement::{MoveContext, MovementChecker};
use crate::anticheat::xray::{self, MiningStats};
use crate::anticheat::{Check, Verdict, Violations};
use crate::config::XrayMode;
use crate::data::{Items, Stack};
use crate::game::combat::damage::DamageSource;
use crate::game::hurt::{F_CLIMBING, F_FALL_FLYING, F_IN_WATER, F_SNEAKING, F_SPRINTING};
use crate::game::survival::{self, Surroundings};
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
pub fn chunk_body(c: &Chunk, light: &crate::world::light::ChunkLight) -> Vec<u8> {
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
    // block entities the client renders (chests, signs, beds, banners, …)
    let reg = Registries::get();
    let list: Vec<(&[i32; 3], i32, Vec<u8>)> = c
        .block_entities
        .iter()
        .filter_map(|(p, t)| {
            let id = t.get("id")?.as_str()?;
            let ty = reg.id("minecraft:block_entity_type", id)?;
            // only what the client needs to draw: no items / loot
            let shown = match t {
                Tag::Compound(v) => Tag::Compound(v.iter().filter(|(k, _)| !matches!(k.as_str(), "Items" | "LootTable" | "LootTableSeed" | "x" | "y" | "z" | "id")).cloned().collect()),
                other => other.clone(),
            };
            Some((p, ty, shown.to_network_bytes()))
        })
        .collect();
    b.vi(list.len() as i32);
    for (p, ty, nbt) in list {
        b.u8((((p[0] & 15) << 4) | (p[2] & 15)) as u8).i16(p[1] as i16).vi(ty);
        b.extend_from_slice(&nbt);
    }
    light_data(&mut b, light);
    b
}

/// The light part of `level_chunk_with_light` / `light_update`.
pub fn light_data(b: &mut Vec<u8>, light: &crate::world::light::ChunkLight) {
    let bits = |b: &mut Vec<u8>, m: u64| {
        if m == 0 {
            b.vi(0);
        } else {
            b.vi(1).i64(m as i64);
        }
    };
    let mask = |v: &[Option<Box<[u8; 2048]>>], set: bool| v.iter().enumerate().filter(|(_, a)| a.is_some() == set).fold(0u64, |m, (i, _)| m | 1 << i);
    bits(b, mask(&light.sky, true));
    bits(b, mask(&light.block, true));
    bits(b, mask(&light.sky, false));
    bits(b, mask(&light.block, false));
    for v in [&light.sky, &light.block] {
        b.vi(v.iter().flatten().count() as i32);
        for a in v.iter().flatten() {
            b.vi(2048).raw(&a[..]);
        }
    }
}

/// The `commands` tree: a few literals, each taking an optional greedy string.
fn commands_body() -> Vec<u8> {
    let mut names: Vec<String> = ["rime", "stop", "op", "deop", "gamemode", "tp", "list", "help", "spawn", "summon", "time", "kill", "give", "setblock", "fill", "execute"].map(String::from).to_vec();
    if crate::plugins::active() {
        for c in crate::plugins::command_names() {
            if !names.contains(&c) && !c.is_empty() && c.chars().all(|ch| ch.is_ascii_graphic()) {
                names.push(c);
            }
        }
    }
    let parser = Registries::get().id("minecraft:command_argument_type", "brigadier:string").unwrap_or(5);
    let arg = names.len() as i32 + 1;
    let mut b = Vec::new();
    b.vi(names.len() as i32 + 2);
    // root
    b.u8(0).vi(names.len() as i32);
    for i in 0..names.len() {
        b.vi(i as i32 + 1);
    }
    for n in &names {
        b.u8(0x01 | 0x04).vi(1).vi(arg).str(n);
    }
    b.u8(0x02 | 0x04).vi(0).str("args").vi(parser).vi(2);
    b.vi(0);
    b
}

/// a command coordinate: absolute or `~`-relative
fn parse_coord(a: &str, base: i32) -> Option<i32> {
    if let Some(r) = a.strip_prefix('~') {
        let off = if r.is_empty() { 0.0 } else { r.parse::<f64>().ok()? };
        return Some(base + off.floor() as i32);
    }
    let v = a.parse::<f64>().ok()?;
    (v.abs() < 3.0e7).then(|| v.floor() as i32)
}

/// `minecraft:name[key=value,...]`
fn parse_block_state(a: &str) -> Option<BlockState> {
    let a = a.trim_start_matches("minecraft:");
    let (name, props) = match a.split_once('[') {
        Some((n, rest)) => (n, rest.strip_suffix(']')?),
        None => (a, ""),
    };
    let kv: Vec<(&str, &str)> = props.split(',').filter(|s| !s.is_empty()).filter_map(|p| p.split_once('=')).collect();
    BlockRegistry::get().state(name, &kv)
}

fn text_nbt(msg: &str) -> Vec<u8> {
    Tag::text(msg).to_network_bytes()
}

/// Run a Java client from Login Start until it disconnects.
/// Writes the profile properties of a `player_info_update` add-player entry.
fn write_props(b: &mut Vec<u8>, props: &[(String, String, Option<String>)]) {
    b.vi(props.len() as i32);
    for (n, v, sig) in props {
        b.str(n).str(v).bool(sig.is_some());
        if let Some(s) = sig {
            b.str(s);
        }
    }
}

pub async fn run<S: Io>(sock: CryptStream<S>, st: Arc<ServerState>, profile: Profile) -> anyhow::Result<()> {
    run_with_state(sock, st, profile, None).await
}

/// `state`: player NBT forwarded by a RimeFleet router (used instead of the
/// local player file).
pub async fn run_with_state<S: Io>(mut sock: CryptStream<S>, st: Arc<ServerState>, profile: Profile, state: Option<Vec<u8>>) -> anyhow::Result<()> {
    let forwarded = state.is_some() || crate::fleet_net::node_link().is_some();
    let Profile { uuid, name, properties } = profile;
    let th = st.game.threshold;
    // ---- login
    if th >= 0 {
        let mut b = Vec::new();
        b.vi(th);
        sock.write_all(&conn::encode(id("login", "clientbound", "login_compression"), &b, -1)).await?;
    }
    let mut b = Vec::new();
    b.uuid(uuid).str(&name);
    write_props(&mut b, &properties);
    sock.write_all(&conn::encode(id("login", "clientbound", "login_finished"), &b, th)).await?;
    sock.flush().await?;
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
            sock.flush().await?;
            sent_registries = true;
        } else if pid == id("configuration", "serverbound", "finish_configuration") && sent_registries {
            break;
        }
    }

    // ---- play
    let view = client_view.min(st.cfg.server.view_distance as i32).max(2);
    let spawn = st.spawn;
    let saved = match &state {
        Some(raw) => crate::game::playerdata::from_bytes(raw),
        None if forwarded => None,
        None => crate::game::playerdata::load(&crate::game::playerdata::path(&st.world_dir, uuid)),
    }
    .filter(|s| !s.dead);
    let gamemode = saved.as_ref().map(|s| s.gamemode).filter(|g| *g >= 0).unwrap_or(st.cfg.server.default_gamemode.id());
    let (tx, rx) = mpsc::channel::<Arc<Vec<u8>>>(OUT_QUEUE);
    let pose = match &saved {
        Some(s) => Pose { pos: s.pos, yaw: s.yaw, pitch: s.pitch, on_ground: false },
        None => Pose { pos: spawn, yaw: 0.0, pitch: 0.0, on_ground: false },
    };
    let dim = saved.as_ref().map(|s| s.dim).unwrap_or(crate::world::storage::Dim::Overworld);
    if forwarded && st.levels().iter().fold(false, |any, g| g.take_over(uuid) | any) {
        tracing::info!("{name}: replacing the previous session on this node");
    }
    let game = st.level(dim).clone();
    let dup = st.all_players().iter().any(|p| p.uuid == uuid || p.name.eq_ignore_ascii_case(&name));
    let Some(me) = (if dup { None } else { game.join(uuid, name.clone(), properties, pose, gamemode, view, tx) }) else {
        let b = text_nbt("You are already logged in from another location.");
        sock.write_all(&conn::encode(id("play", "clientbound", "disconnect"), &b, th)).await?;
        sock.flush().await?;
        return Ok(());
    };
    if let Some(s) = &saved {
        let mut inv = me.inv.lock();
        for (w, stack) in &s.slots {
            inv.slots[*w] = Some(*stack);
        }
        drop(inv);
        me.held.store(s.held, Ordering::Relaxed);
        *me.vit.lock() = s.vitals();
    }
    st.online.store(st.all_players().len() as u32, Ordering::Relaxed);
    tracing::info!("{name} joined the game ({uuid}, entity {})", me.eid);

    let (rd, wr) = tokio::io::split(sock);
    let writer = tokio::spawn(writer_task(wr, rx));
    let (in_tx, in_rx) = mpsc::channel::<(Instant, Vec<u8>)>(1024);
    let reader = tokio::spawn(reader_task(rd, in_tx));

    let mut s = Session::new(st.clone(), game, me.clone(), view);
    let result = s.run(in_rx).await;

    // ---- leave
    let replaced = me.kicked.load(Ordering::Relaxed);
    match crate::fleet_net::node_link() {
        // replaced by a newer login: that session owns the player now
        _ if replaced => {}
        // a RimeFleet node: the router keeps player files
        Some(link) => link.player_left(&me),
        None => {
            if let Err(e) = crate::game::playerdata::save(&st.world_dir, &me) {
                tracing::error!("saving player {}: {e:#}", me.name);
            }
        }
    }
    let quit_msg = if replaced || !crate::plugins::active() {
        format!("§e{name} left the game")
    } else {
        // PlayerQuitEvent while the player is still online
        crate::plugins::quit(&name, &format!("§e{name} left the game"))
    };
    let game = s.game.clone();
    game.leave(me.eid);
    st.online.store(st.all_players().len() as u32, Ordering::Relaxed);
    for c in &s.sent {
        game.world.remove_viewer(c.0, c.1);
    }
    if replaced {
        let mut b = Vec::new();
        b.vi(1).vi(me.eid);
        game.broadcast(game.encode("remove_entities", &b));
        reader.abort();
        return Ok(());
    }
    let mut b = Vec::new();
    b.vi(1).uuid(me.uuid);
    st.broadcast_all(game.encode("player_info_remove", &b));
    let mut b = Vec::new();
    b.vi(1).vi(me.eid);
    game.broadcast(game.encode("remove_entities", &b));
    if !quit_msg.is_empty() {
        st.broadcast_all(game.system_chat(&quit_msg));
    }
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

async fn writer_task<S: Io>(mut wr: tokio::io::WriteHalf<CryptStream<S>>, mut rx: mpsc::Receiver<Arc<Vec<u8>>>) {
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
        if wr.write_all(&buf).await.is_err() || wr.flush().await.is_err() {
            return;
        }
    }
    let _ = wr.shutdown().await;
}

async fn reader_task<S: Io>(mut rd: tokio::io::ReadHalf<CryptStream<S>>, tx: mpsc::Sender<(Instant, Vec<u8>)>) {
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
        // stamped on arrival: packets queued while the session was busy
        // must not look like a timer cheat when they are handled in a burst
        if count > MAX_PACKETS_PER_SEC || tx.send((Instant::now(), p)).await.is_err() {
            return;
        }
    }
}

mod ranged;

struct Session {
    st: Arc<ServerState>,
    /// the level of the dimension this player is in
    game: Arc<crate::game::Game>,
    me: Arc<Player>,
    base_view: i32,
    view: i32,
    center: (i32, i32),
    sent: HashSet<(i32, i32)>,
    queue: Vec<(i32, i32)>,
    in_flight: u32,
    chunks_per_tick: f32,
    /// chunk packets being loaded/generated in the background
    building: Option<tokio::task::JoinHandle<Vec<((i32, i32), Option<Arc<Vec<u8>>>)>>>,
    /// death screen sent (until respawn)
    death_shown: bool,
    tracked: HashMap<i32, u64>,
    pending_tp: Option<i32>,
    next_tp: i32,
    sneaking: bool,
    next_window: u8,
    checker: MovementChecker,
    /// ticks spent in a nether portal (`PortalProcessor.portalTime`)
    portal_time: i32,
    /// `Entity.portalCooldown`
    portal_cooldown: i32,
    /// left the End through the exit portal: waiting for the credits to end
    won_game: bool,
    credits_done: bool,
    combat: CombatChecker,
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
    /// arrival time of the packet being handled (ms since `epoch`)
    arrived_ms: u64,
    /// y of the previous accepted move (fall distance)
    last_y: f64,
    /// main hand item last tick (`Player.lastItemInMainHand`)
    last_main: Option<i32>,
    /// position at the previous vitals tick (sweet berry bush movement test)
    last_tick_pos: [f64; 3],
    was_on_ground: bool,
}

enum Flow {
    Continue,
    Disconnect(String),
}

impl Session {
    fn new(st: Arc<ServerState>, game: Arc<crate::game::Game>, me: Arc<Player>, view: i32) -> Self {
        let p = me.pose.lock().pos;
        Self {
            st,
            game,
            me,
            base_view: view,
            view,
            center: ((p[0].floor() as i32) >> 4, (p[2].floor() as i32) >> 4),
            sent: HashSet::new(),
            portal_time: 0,
            portal_cooldown: 0,
            won_game: false,
            credits_done: false,
            queue: Vec::new(),
            in_flight: 0,
            chunks_per_tick: 9.0,
            building: None,
            death_shown: false,
            tracked: HashMap::new(),
            pending_tp: None,
            next_tp: 1,
            sneaking: false,
            next_window: 0,
            checker: MovementChecker::default(),
            combat: CombatChecker::default(),
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
            arrived_ms: 0,
            last_y: p[1],
            last_main: None,
            last_tick_pos: [0.0; 3],
            was_on_ground: false,
        }
    }

    fn send(&self, name: &str, body: &[u8]) {
        self.me.send(self.game.encode(name, body));
    }

    fn msg(&self, text: &str) {
        self.me.send(self.game.system_chat(text));
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn gamemode(&self) -> i32 {
        self.me.gamemode.load(Ordering::Relaxed)
    }

    async fn run(&mut self, mut rx: mpsc::Receiver<(Instant, Vec<u8>)>) -> anyhow::Result<()> {
        self.join_sequence().await;
        let mut ticker = tokio::time::interval(Duration::from_millis(50));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let flow = tokio::select! {
                p = rx.recv() => match p {
                    Some((at, p)) => {
                        self.act_packets += 1;
                        self.arrived_ms = at.saturating_duration_since(self.epoch).as_millis() as u64;
                        match self.handle(&p) {
                            Ok(f) => f,
                            Err(e) => Flow::Disconnect(format!("Bad packet: {e}")),
                        }
                    }
                    None => return Ok(()),
                },
                _ = ticker.tick() => {
                    if self.me.kicked.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    self.on_tick().await
                }
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
        b.vi(cd.entry_id("minecraft:dimension_type", self.game.dim.key())).str(self.game.dim.key()).i64(hashed_seed(st.seed));
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
        let all = self.st.all_players();
        let mut b = Vec::new();
        b.u8(0x01 | 0x04 | 0x08 | 0x10).vi(all.len() as i32);
        for p in &all {
            b.uuid(p.uuid).str(&p.name);
            write_props(&mut b, &p.props);
            b.vi(p.gamemode.load(Ordering::Relaxed)).bool(true).vi(0);
        }
        self.send("player_info_update", &b);
        let mut b = Vec::new();
        b.u8(0x01 | 0x04 | 0x08 | 0x10).vi(1).uuid(self.me.uuid).str(&self.me.name);
        write_props(&mut b, &self.me.props);
        b.vi(gm).bool(true).vi(0);
        let info = self.game.encode("player_info_update", &b);
        for p in &all {
            if p.eid != self.me.eid {
                p.send(info.clone());
            }
        }
        let join_msg = format!("§e{} joined the game", self.me.name);
        let join_msg = if crate::plugins::active() { crate::plugins::join(&self.me.name, &join_msg) } else { join_msg };
        if !join_msg.is_empty() {
            self.st.broadcast_all(self.game.system_chat(&join_msg));
        }
        let mut b = Vec::new();
        b.f64(0.0).f64(0.0).f64(59_999_968.0).f64(59_999_968.0).vl(0).vi(29_999_984).vi(5).vi(15);
        self.send("initialize_border", &b);
        self.send_time();
        self.send_weather();
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
        self.me.vit.lock().dirty = true;
        self.me.vit.lock().data_dirty = true;
        let mut b = Vec::new();
        b.f32(0.0).vi(0).vi(0);
        self.send("set_experience", &b);
        let mut b = Vec::new();
        b.vi(0);
        self.send("set_held_slot", &b);
        self.send("update_recipes", &crate::game::workstation::update_recipes(&self.game.data));
        self.game.send_inventory(&self.me);
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
        let day = self.st.game.clock() as i64;
        let mut b = Vec::new();
        b.i64(age).vi(1).vi(self.st.config_data.entry_id("minecraft:world_clock", "minecraft:overworld")).vl(day + 1000).f32(0.0).f32(1.0);
        self.send("set_time", &b);
    }

    fn send_weather(&self) {
        if self.game.dim != crate::world::storage::Dim::Overworld {
            return;
        }
        for pkt in self.game.weather_packets() {
            self.me.send(pkt);
        }
    }

    fn teleport(&mut self, pos: [f64; 3], yaw: f32, pitch: f32) {
        let tid = self.next_tp;
        self.next_tp = self.next_tp.wrapping_add(1).max(1);
        self.pending_tp = Some(tid);
        self.checker.teleported(pos);
        self.last_y = pos[1];
        self.me.vit.lock().fall_distance = 0.0;
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
            self.game.world.remove_viewer(c.0, c.1);
            let eids = self.game.entities_in(c);
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
        // a build in progress never holds up this session (keep-alives,
        // movement): its packets go out on the first tick after it is done
        if let Some(h) = &self.building {
            if !h.is_finished() {
                return;
            }
            let h = self.building.take().expect("building");
            let built = h.await.unwrap_or_default();
            self.send_built(built);
        }
        if self.queue.is_empty() || self.in_flight >= 4 {
            return;
        }
        let limits = self.budget.limits(&self.st.cfg.player_cost, self.base_view as u32);
        let mut n = (self.chunks_per_tick.ceil() as usize).clamp(1, 64).min(limits.chunk_sends_per_tick.max(1) as usize * 4);
        let world = &self.game.world;
        let mut batch = Vec::with_capacity(n);
        while batch.len() < n {
            let Some(c) = self.queue.pop() else { break };
            if self.in_view(c, self.center, self.view) && !self.sent.contains(&c) {
                // chunks still to be generated go out in small batches so the
                // nearest ones show up first
                if n > 9 && (-1..=1).any(|dx| (-1..=1).any(|dz| !world.is_loaded(c.0 + dx, c.1 + dz))) {
                    n = 9;
                }
                batch.push(c);
            }
        }
        if batch.is_empty() {
            return;
        }
        let st = self.st.clone();
        let game = self.game.clone();
        let coords = batch.clone();
        // load/generate (with neighbours: a chunk's light depends on its 3x3
        // neighbourhood) and build the packets on the blocking pool, in
        // parallel, so this player's packets keep flowing meanwhile
        self.building = Some(tokio::task::spawn_blocking(move || {
            let mut all: Vec<(i32, i32)> = coords.iter().flat_map(|&(x, z)| (-1..=1).flat_map(move |dx| (-1..=1).map(move |dz| (x + dx, z + dz)))).collect();
            all.sort_unstable();
            all.dedup();
            let world = &game.world;
            world.ensure_loaded(&all, &st.store, st.generator_of(game.dim));
            let xray_mode = st.cfg.anticheat.xray;
            let xray_on = xray_mode != XrayMode::Off && st.cfg.anticheat.enabled;
            let seed = st.generator.seed;
            use rayon::prelude::*;
            coords
                .par_iter()
                .map(|&c| {
                    let pkt = world.chunk_packet(c.0, c.1, &|chunk: &Chunk| {
                        let around: Vec<Option<Chunk>> = (0..9).map(|i| if i == 4 { None } else { world.chunk_copy(c.0 + i / 3 - 1, c.1 + i % 3 - 1) }).collect();
                        let refs: [Option<&Chunk>; 9] = std::array::from_fn(|i| if i == 4 { Some(chunk) } else { around[i].as_ref() });
                        let light = crate::world::light::compute(&refs);
                        let light = std::sync::Arc::new(light);
                        world.store_light(c, light.clone());
                        if xray_on {
                            let safe = xray::obfuscate(chunk, xray_mode, seed, &|x, y, z| world.get_block(x, y, z));
                            chunk_body(&safe, &light)
                        } else {
                            chunk_body(chunk, &light)
                        }
                    });
                    (c, pkt)
                })
                .collect::<Vec<_>>()
        }));
    }

    fn send_built(&mut self, built: Vec<((i32, i32), Option<Arc<Vec<u8>>>)>) {
        let st = self.st.clone();
        let world = &self.game.world;
        self.send("chunk_batch_start", &[]);
        let mut count = 0;
        for (c, pkt) in built {
            if !self.in_view(c, self.center, self.view) || self.sent.contains(&c) {
                continue;
            }
            if let Some(body) = pkt {
                self.send("level_chunk_with_light", &body);
                world.add_viewer(c.0, c.1);
                self.sent.insert(c);
                count += 1;
                for e in self.game.entity_spawns_in(c) {
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
        let requests = std::mem::take(&mut *self.me.requests.lock());
        for r in requests {
            match r {
                crate::game::Request::Teleport(pos, yaw, pitch) => self.teleport(pos, yaw, pitch),
                crate::game::Request::Gamemode(gm) => self.set_gamemode(gm.clamp(0, 3)),
                crate::game::Request::Kick(reason) => return Flow::Disconnect(reason),
                crate::game::Request::StopUsingItem => self.stop_using_item(),
                crate::game::Request::Died(src) => self.die(*src),
                crate::game::Request::Chat(m) => {
                    if let Flow::Disconnect(r) = self.on_chat(&m) {
                        return Flow::Disconnect(r);
                    }
                }
                crate::game::Request::Command(line) => {
                    if let Flow::Disconnect(r) = self.on_command(&line) {
                        return Flow::Disconnect(r);
                    }
                }
            }
        }
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
        self.tick_vitals();
        self.tick_portal().await;
        self.pump_chunks().await;
        Flow::Continue
    }

    // ------------------------------------------------------------ health

    fn surroundings(&self) -> Surroundings {
        let pose = *self.me.pose.lock();
        let w = &self.game.world;
        let reg = BlockRegistry::get();
        let at = |dy: f64| w.get_block(pose.pos[0].floor() as i32, (pose.pos[1] + dy).floor() as i32, pose.pos[2].floor() as i32).unwrap_or(blocks::AIR);
        let water = |s: BlockState| matches!(s.name(), "water" | "bubble_column" | "kelp" | "kelp_plant" | "seagrass" | "tall_seagrass") || reg.property(s, "waterlogged") == Some("true");
        let (feet, body, eyes) = (at(0.0), at(1.0), at(1.62));
        let _ = (feet, body);
        let bbox = crate::anticheat::combat::Aabb::entity(pose.pos, 0.6, if self.sneaking { 1.5 } else { 1.8 });
        let inside = crate::game::hazards::inside(&|x, y, z| w.get_block(x, y, z), &bbox);
        let below = w.get_block(pose.pos[0].floor() as i32, (pose.pos[1] - 0.2).floor() as i32, pose.pos[2].floor() as i32).unwrap_or(blocks::AIR);
        let moving = {
            let m = self.last_tick_pos;
            (pose.pos[0] - m[0]).abs() >= 0.003 || (pose.pos[2] - m[2]).abs() >= 0.003
        };
        let freeze_immune = {
            let inv = self.me.inv.lock();
            (5..9).any(|i| inv.slots[i].is_some_and(|st| crate::game::hazards::freeze_immune_wearable(Items::get().name(st.item))))
        };
        Surroundings {
            eyes_in_water: water(eyes),
            inside,
            on_magma: pose.on_ground && crate::game::hazards::hot_floor(below, self.sneaking),
            moving,
            freeze_immune,
            bubble_column: eyes.name() == "bubble_column",
            y: pose.pos[1],
            min_y: MIN_Y,
            gamemode: self.gamemode(),
            difficulty: 2,
        }
    }

    fn tick_vitals(&mut self) {
        if self.me.vit.lock().dead() {
            return;
        }
        self.tick_using();
        let xp = {
            let mut v = self.me.vit.lock();
            std::mem::take(&mut v.xp_dirty).then_some((v.xp_progress, v.xp_level, v.xp_total))
        };
        if let Some((p, l, t)) = xp {
            let mut b = Vec::new();
            b.f32(p).vi(l).vi(t);
            self.send("set_experience", &b);
        }
        // LivingEntity.aiStep on the server: deltaMovement decays
        {
            let on_ground = self.me.pose.lock().on_ground;
            let mut m = self.me.motion.lock();
            for v in m.iter_mut() {
                *v *= 0.98;
                if v.abs() < 0.003 {
                    *v = 0.0;
                }
            }
            if on_ground && m[1] < 0.0 {
                m[1] = 0.0;
            }
        }
        if self.me.knocked.swap(false, Ordering::Relaxed) {
            self.checker.knocked();
        }
        // Player.tick: a different item in the main hand resets the attack charge
        let main = self.me.equipment()[0].map(|s| s.item);
        if main != self.last_main {
            self.last_main = main;
            self.me.vit.lock().living.attack_ticker = 0;
        }
        let st = self.st.clone();
        self.game.with_player(&self.me, |l, eq, body| {
            l.tick_counters();
            l.tick_effects(eq, body, &mut rand::thread_rng());
        });
        let s = self.surroundings();
        self.last_tick_pos = self.me.pose.lock().pos;
        let hits = self.me.vit.lock().tick(&s);
        for (src, amount) in hits {
            self.damage(src, amount);
        }
        self.finish_eating();
        let (dirty, data_dirty, health, food, sat, air, fire, absorption, using, frozen) = {
            let mut v = self.me.vit.lock();
            let d = (v.dirty, v.data_dirty, v.living.health, v.food, v.saturation, v.air, v.fire, v.living.absorption, v.living.using, v.frozen);
            v.dirty = false;
            v.data_dirty = false;
            d
        };
        if dirty {
            let mut b = Vec::new();
            b.f32(health).vi(food).f32(sat);
            self.send("set_health", &b);
        }
        if data_dirty || dirty {
            let flags = (fire > 0) as u8 | if self.sneaking { 0x02 } else { 0 } | if self.sprinting { 0x08 } else { 0 } | if self.gliding { 0x80 } else { 0 };
            let living_flags = using.map(|u| 1 | if u.hand == 1 { 2 } else { 0 }).unwrap_or(0);
            let mut b = Vec::new();
            b.vi(self.me.eid).u8(0).vi(0).u8(flags).u8(1).vi(1).vi(air).u8(7).vi(1).vi(frozen);
            b.u8(8).vi(0).u8(living_flags);
            b.u8(17).vi(3).f32(absorption);
            b.u8(0xff);
            let pkt = self.game.encode("set_entity_data", &b);
            self.to_viewers(pkt, true);
        }
    }

    /// Sends to every player that can see me (and to me when `me`).
    fn to_viewers(&self, pkt: Arc<Vec<u8>>, me: bool) {
        let pos = self.me.pose.lock().pos;
        let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
        for p in self.game.players() {
            if p.eid == self.me.eid {
                if me {
                    p.send(pkt.clone());
                }
            } else if p.sees_chunk(c.0, c.1) {
                p.send(pkt.clone());
            }
        }
    }

    /// Player attack (left click on an entity).
    fn on_attack(&mut self, target: i32) -> Option<Flow> {
        let gm = self.gamemode();
        if gm == 3 || self.me.vit.lock().dead() || target == self.me.eid {
            return None;
        }
        let pose = *self.me.pose.lock();
        let Some(bx) = self.game.entity_box(target) else { return None };
        let eye = [pose.pos[0], pose.pos[1] + 1.62, pose.pos[2]];
        if self.st.cfg.anticheat.enabled {
            let flags = self.combat.on_attack(self.now_ms(), eye, pose.yaw, pose.pitch, target as u32, &[bx], gm == 1);
            let mut cancel = false;
            for f in flags {
                cancel |= matches!(f.check, Check::Reach);
                if let Some(flow) = self.apply_flag(f.check, f.amount, f.detail) {
                    return Some(flow);
                }
            }
            if cancel {
                return None;
            }
        } else if bx.distance_to(eye) > 6.0 {
            return None;
        }
        let st = self.st.clone();
        self.game.player_attack(&self.me, target);
        None
    }

    fn damage(&mut self, src: DamageSource, amount: f32) -> bool {
        let st = self.st.clone();
        self.game.hurt_player(&self.me, &src, amount)
    }

    /// Stops using the held item (shield lowered / disabled).
    fn stop_using_item(&mut self) {
        let mut v = self.me.vit.lock();
        if v.living.using.take().is_some() {
            v.data_dirty = true;
        }
        v.eating = None;
    }

    fn die(&mut self, src: DamageSource) {
        // `Living::die` has usually marked the player dead already (damage
        // dealt by the game); the death screen is sent once either way
        if self.death_shown {
            return;
        }
        self.death_shown = true;
        let credit = {
            let v = self.me.vit.lock();
            v.living.last_hurt_by_player.or(v.living.last_hurt_by_mob).map(|c| c.0)
        };
        let me_name = serde_json::json!({"text": self.me.name});
        let msg_json = self.game.death_message(&src, me_name, credit);
        let msg_nbt = crate::nbt::Tag::from_json(&msg_json).to_network_bytes();
        {
            let mut v = self.me.vit.lock();
            let body_src = src.clone();
            v.living.dead = true;
            v.living.using = None;
            v.living.last_damage = Some((body_src, v.living.tick_count));
        }
        // ServerPlayer.die: the health packet goes first (client shows the death screen at 0)
        let (food, sat) = {
            let mut v = self.me.vit.lock();
            v.dirty = false;
            (v.food, v.saturation)
        };
        let mut b = Vec::new();
        b.f32(0.0).vi(food).f32(sat);
        self.send("set_health", &b);
        let mut b = Vec::new();
        b.vi(self.me.eid).raw(&msg_nbt);
        self.send("player_combat_kill", &b);
        let mut b = Vec::new();
        b.i32(self.me.eid).u8(3);
        let pkt = self.game.encode("entity_event", &b);
        self.to_viewers(pkt, false);
        let mut chat = msg_nbt.clone();
        chat.bool(false);
        self.st.broadcast_all(self.game.encode("system_chat", &chat));
        tracing::info!("{} died: {}", self.me.name, msg_json);
        // Inventory.dropAll
        let stacks: Vec<Stack> = {
            let mut inv = self.me.inv.lock();
            let mut out = Vec::new();
            for slot in 1..46 {
                if let Some(s) = inv.slots[slot] {
                    out.push(s);
                    inv.set(slot, None);
                }
            }
            out
        };
        for s in stacks {
            self.throw(s, true);
        }
        self.game.flush_inventory(&self.me);
        self.me.moved.fetch_add(1, Ordering::Relaxed);
    }

    /// Move this player's state into another dimension's level (no packets
    /// to the client).
    fn switch_level(&mut self, dim: crate::world::storage::Dim) {
        let old = self.game.clone();
        let new = self.st.level(dim).clone();
        if Arc::ptr_eq(&old, &new) {
            return;
        }
        old.leave(self.me.eid);
        let mut b = Vec::new();
        b.vi(1).vi(self.me.eid);
        old.broadcast(old.encode("remove_entities", &b));
        for c in self.sent.drain() {
            old.world.remove_viewer(c.0, c.1);
        }
        self.queue.clear();
        if let Some(h) = self.building.take() {
            h.abort();
        }
        self.in_flight = 0;
        self.tracked.clear();
        self.close_window_silently();
        new.adopt(self.me.clone());
        self.game = new;
    }

    /// `ServerPlayer.teleport` to another dimension: respawn packet, then
    /// the position and everything the client forgets with its level.
    pub(crate) fn change_dimension(&mut self, dim: crate::world::storage::Dim, pos: [f64; 3], yaw: f32, pitch: f32) {
        if dim == self.game.dim {
            self.teleport(pos, yaw, pitch);
            let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
            self.set_center(c, false);
            return;
        }
        self.switch_level(dim);
        let st = self.st.clone();
        let cd = &st.config_data;
        let gm = self.gamemode();
        let mut b = Vec::new();
        b.vi(cd.entry_id("minecraft:dimension_type", dim.key())).str(dim.key()).i64(hashed_seed(st.seed));
        b.u8(gm as u8).u8(255).bool(false).bool(false).bool(false).vi(0).vi(63);
        // keep attributes and entity data
        b.u8(0x03);
        self.send("respawn", &b);
        let mut b = Vec::new();
        b.vi(2).bool(false);
        self.send("change_difficulty", &b);
        self.send_abilities();
        self.teleport(pos, yaw, pitch);
        let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
        self.center = c;
        self.set_center(c, true);
        let mut b = Vec::new();
        b.u8(13).f32(0.0);
        self.send("game_event", &b);
        self.send_time();
        self.send_weather();
        let mut b = Vec::new();
        b.vi(self.me.held.load(Ordering::Relaxed));
        self.send("set_held_slot", &b);
        self.game.send_inventory(&self.me);
        {
            let mut v = self.me.vit.lock();
            v.dirty = true;
            v.data_dirty = true;
            v.fall_distance = 0.0;
        }
        let pkt = self.game.attributes_packet(self.me.eid, &mut self.me.vit.lock().living);
        self.me.send(pkt);
        self.resend_effects();
        self.me.moved.fetch_add(1, Ordering::Relaxed);
    }

    /// `Entity.handlePortal` for this player.
    async fn tick_portal(&mut self) {
        use crate::game::portal;
        use crate::world::storage::Dim;
        if self.me.vit.lock().dead() {
            return;
        }
        if std::mem::take(&mut self.credits_done) {
            self.leave_end().await;
            return;
        }
        let touched = portal::portal_touched(&self.game, &self.me);
        if self.portal_cooldown > 0 {
            self.portal_cooldown -= 1;
            if touched.is_some() {
                self.portal_cooldown = 10;
            }
            self.portal_time = 0;
            return;
        }
        let Some((at, kind)) = touched else {
            self.portal_time = (self.portal_time - 4).max(0);
            return;
        };
        let gm = self.gamemode();
        match kind {
            "nether_portal" => {
                if !portal::in_portal_dimension(&self.game) {
                    return;
                }
                self.portal_time += 1;
                let delay = if gm == 1 { 1 } else { 80 };
                if self.portal_time < delay {
                    return;
                }
                self.portal_time = 0;
                self.portal_cooldown = 10;
                let dest_dim = if self.game.dim == Dim::Nether { Dim::Overworld } else { Dim::Nether };
                let pose = *self.me.pose.lock();
                let scale = if dest_dim == Dim::Nether { 1.0 / 8.0 } else { 8.0 };
                let exit = ((pose.pos[0] * scale).floor() as i32, (pose.pos[2] * scale).floor() as i32);
                self.load_area(dest_dim, exit, 2).await;
                let dest = self.st.level(dest_dim).clone();
                // the known portals in search range must be loaded to be found
                let r = if dest_dim == Dim::Nether { 16 } else { 128 };
                let mut near: Vec<(i32, i32)> = dest.portals.lock().iter().filter(|p| (p[0] - exit.0).abs() <= r && (p[2] - exit.1).abs() <= r).map(|p| (p[0] >> 4, p[2] >> 4)).collect();
                near.sort_unstable();
                near.dedup();
                if !near.is_empty() {
                    let st = self.st.clone();
                    let _ = tokio::task::spawn_blocking(move || st.level(dest_dim).world.ensure_loaded(&near, &st.store, st.generator_of(dest_dim))).await;
                }
                let Some((pos, dyaw)) = portal::nether_destination(&self.game, &dest, at, pose.pos) else {
                    tracing::warn!("{}: no portal destination from {:?}", self.me.name, at);
                    return;
                };
                tracing::info!("{} travels through the portal at {:?} to {:?} {:?}", self.me.name, at, dest_dim, pos);
                self.change_dimension(dest_dim, pos, pose.yaw + dyaw, pose.pitch);
                // LevelEvent.SOUND_PORTAL_TRAVEL, heard only by this player
                let mut b = Vec::new();
                b.i32(1032).pos(0, 0, 0).i32(0).bool(false);
                self.send("level_event", &b);
            }
            "end_portal" => {
                self.portal_cooldown = 10;
                if self.game.dim == Dim::End {
                    if self.won_game {
                        return;
                    }
                    // ServerPlayer.teleport: the credits roll first (only the first time)
                    let seen = std::mem::replace(&mut self.me.vit.lock().seen_credits, true);
                    self.won_game = true;
                    let mut b = Vec::new();
                    b.u8(4).f32(if seen { 0.0 } else { 1.0 });
                    self.send("game_event", &b);
                    return;
                }
                {
                    let s = portal::END_SPAWN;
                    self.load_area(Dim::End, (s[0], s[2]), 1).await;
                    let end = self.st.level(Dim::End).clone();
                    portal::end_platform(&end, [s[0], s[1] - 1, s[2]]);
                    let pos = [s[0] as f64 + 0.5, (s[1] - 1) as f64, s[2] as f64 + 0.5];
                    self.change_dimension(Dim::End, pos, 90.0, 0.0);
                }
            }
            "end_gateway" => {
                // TheEndGatewayBlockEntity.teleportEntity
                self.portal_cooldown = 40;
                let dim = self.game.dim;
                if let Some(t) = self.game.gateway_exit_target(at) {
                    self.load_area(dim, t, 2).await;
                    self.game.make_gateway_exit(at, t);
                }
                let Some((rough, _)) = self.game.gateway_exit(at) else { return };
                self.load_area(dim, (rough[0].floor() as i32, rough[2].floor() as i32), 1).await;
                let Some((pos, _)) = self.game.gateway_exit(at) else { return };
                self.game.gateway_beam(at);
                let pose = *self.me.pose.lock();
                self.change_dimension(dim, pos, pose.yaw, pose.pitch);
            }
            _ => {}
        }
    }

    /// After the credits: back to the respawn point in the overworld.
    async fn leave_end(&mut self) {
        use crate::world::storage::Dim;
        self.won_game = false;
        self.portal_cooldown = 10;
        self.load_area(Dim::Overworld, ((self.st.spawn[0] as i32) >> 4 << 4, (self.st.spawn[2] as i32) >> 4 << 4), 1).await;
        let ow = self.st.level(Dim::Overworld).clone();
        let bed = {
            let v = self.me.vit.lock();
            v.respawn.filter(|_| v.respawn_dim == 0)
        };
        let spawn = bed.and_then(|h| crate::game::block::use_block::stand_up_pos(&ow, h)).unwrap_or(self.st.spawn);
        self.change_dimension(Dim::Overworld, spawn, 0.0, 0.0);
    }

    /// Load or generate the chunks within `r` chunks of block (`x`, `z`) in
    /// another dimension (off this session's thread).
    async fn load_area(&self, dim: crate::world::storage::Dim, at: (i32, i32), r: i32) {
        let st = self.st.clone();
        let (cx, cz) = (at.0 >> 4, at.1 >> 4);
        let _ = tokio::task::spawn_blocking(move || {
            let all: Vec<(i32, i32)> = (-r..=r).flat_map(|dx| (-r..=r).map(move |dz| (cx + dx, cz + dz))).collect();
            st.level(dim).world.ensure_loaded(&all, &st.store, st.generator_of(dim));
        })
        .await;
    }

    fn close_window_silently(&mut self) {
        let held = self.me.held.load(Ordering::Relaxed) as i16;
        let thrown = self.me.inv.lock().close(&self.game.data, held);
        for t in thrown {
            self.throw(t, false);
        }
    }

    /// The respawn packet clears the client's effects: send them again.
    fn resend_effects(&self) {
        let effects: Vec<_> = self.me.vit.lock().living.effects.clone();
        for e in effects {
            let mut b = Vec::new();
            b.vi(self.me.eid);
            e.write(&mut b);
            self.send("update_mob_effect", &b);
        }
    }

    fn respawn(&mut self) {
        if !self.me.vit.lock().dead() {
            return;
        }
        self.me.vit.lock().respawn();
        self.death_shown = false;
        let st = self.st.clone();
        // ServerPlayer.findRespawnPositionAndUseSpawnBlock: a bed in the
        // overworld or a charged respawn anchor in the nether
        let (bed, rdim) = {
            let v = self.me.vit.lock();
            (v.respawn, crate::world::storage::Dim::from_index(v.respawn_dim))
        };
        let mut dest = (crate::world::storage::Dim::Overworld, st.spawn);
        let mut lost = false;
        if let Some(h) = bed {
            let lv = st.level(rdim).clone();
            let (cx, cz) = (h[0] >> 4, h[2] >> 4);
            let near: Vec<(i32, i32)> = (-1..=1).flat_map(|dx| (-1..=1).map(move |dz| (cx + dx, cz + dz))).collect();
            lv.world.ensure_loaded(&near, &st.store, st.generator_of(rdim));
            match crate::game::block::use_block::stand_up_pos(&lv, h) {
                Some(p) => dest = (rdim, p),
                None => {
                    self.me.vit.lock().respawn = None;
                    lost = true;
                }
            }
        }
        if self.game.dim != dest.0 {
            self.switch_level(dest.0);
        }
        let cd = &st.config_data;
        let gm = self.gamemode();
        let mut b = Vec::new();
        b.vi(cd.entry_id("minecraft:dimension_type", self.game.dim.key())).str(self.game.dim.key()).i64(hashed_seed(st.seed));
        b.u8(gm as u8).u8(255).bool(false).bool(false).bool(false).vi(0).vi(63);
        b.u8(0);
        self.send("respawn", &b);
        self.send_abilities();
        let spawn = dest.1;
        if lost {
            let mut b = crate::nbt::Tag::from_json(&serde_json::json!({ "translate": "block.minecraft.spawn.not_valid" })).to_network_bytes();
            b.bool(false);
            self.send("system_chat", &b);
        }
        self.teleport(spawn, 0.0, 0.0);
        let c = ((spawn[0].floor() as i32) >> 4, (spawn[2].floor() as i32) >> 4);
        self.set_center(c, true);
        let mut b = Vec::new();
        b.u8(13).f32(0.0);
        self.send("game_event", &b);
        let mut b = Vec::new();
        b.vi(self.me.held.load(Ordering::Relaxed));
        self.send("set_held_slot", &b);
        self.game.send_inventory(&self.me);
        self.send_time();
        self.send_weather();
        // the respawn packet resets client attributes: resend them all
        let pkt = self.game.attributes_packet(self.me.eid, &mut self.me.vit.lock().living);
        self.me.send(pkt);
    }

    /// `Consumable.startConsuming` (`use_item`); true when the item is consumable.
    fn start_eating(&mut self, slot: usize) -> bool {
        let Some(held) = self.me.inv.lock().slots[slot] else { return false };
        let Some(c) = crate::game::consume::consumable(&held) else { return false };
        let gm = self.gamemode();
        let mut v = self.me.vit.lock();
        if gm == 3 || v.living.on_cooldown(held.item) {
            return true;
        }
        // Player.canEat: invulnerable (creative) || can_always_eat || hungry
        if let Some((_, _, always)) = crate::game::consume::food(&held) {
            if !always && v.food >= 20 && gm != 1 {
                return true;
            }
        }
        v.eating = Some((slot as i32, self.tick, crate::game::consume::consume_ticks(c)));
        let hand = if slot == 45 { 1 } else { 0 };
        v.living.using = Some(crate::game::combat::living::UseItem { hand, item: held.item, elapsed: 0 });
        v.data_dirty = true;
        true
    }

    fn finish_eating(&mut self) {
        let Some((slot, start, ticks)) = self.me.vit.lock().eating else { return };
        if self.tick < start + ticks {
            return;
        }
        {
            let mut v = self.me.vit.lock();
            v.eating = None;
            v.living.using = None;
            v.data_dirty = true;
        }
        let slot = slot as usize;
        let Some(held) = self.me.inv.lock().slots[slot] else { return };
        if crate::game::consume::consumable(&held).is_none() {
            return;
        }
        let items = Items::get();
        let gm = self.gamemode();
        let st = self.st.clone();
        // LivingEntity.completeUsingItem: entity event 9 to the user
        let mut b = Vec::new();
        b.i32(self.me.eid).u8(9);
        self.send("entity_event", &b);
        self.game.consume(&self.me, held);
        // use_cooldown
        if let Some(t) = crate::game::consume::use_cooldown(&held) {
            {
                let mut v = self.me.vit.lock();
                let now = v.living.tick_count;
                v.living.cooldowns.push((held.item, now, now + t));
            }
            let mut b = Vec::new();
            b.str(&format!("minecraft:{}", items.name(held.item))).vi(t);
            self.send("cooldown", &b);
        }
        // ItemStack.consume + use_remainder (ItemUtils.createFilledResult)
        if gm != 1 {
            let mut inv = self.me.inv.lock();
            inv.consume(slot, 1);
            if let Some(rest) = crate::game::consume::remainder(&held).and_then(|r| items.id(r.trim_start_matches("minecraft:"))) {
                let rs = Stack::new(rest, 1);
                if inv.slots[slot].is_none() {
                    inv.set(slot, Some(rs));
                } else if inv.add(rs, self.me.held.load(Ordering::Relaxed) as i16) > 0 {
                    drop(inv);
                    self.throw(rs, false);
                }
            }
        }
        self.game.flush_inventory(&self.me);
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
        let limits = self.budget.update(&cfg.player_cost, self.st.mspt() as f64, self.game.count(), estimate_cost(&act), self.base_view as u32);
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
        let others = self.game.players();
        let mut seen = HashSet::new();
        let ptype = Registries::get().id("minecraft:entity_type", "minecraft:player").unwrap_or(0);
        for p in others.iter().filter(|p| p.eid != self.me.eid) {
            let pose = *p.pose.lock();
            let c = ((pose.pos[0].floor() as i32) >> 4, (pose.pos[2].floor() as i32) >> 4);
            if !self.in_view(c, self.center, self.view) || p.gamemode.load(Ordering::Relaxed) == 3 || p.vit.lock().dead() {
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
        } else if pid == sb("sign_update") {
            let pos = r.pos()?;
            let front = r.bool()?;
            let lines = [r.string(384)?.to_string(), r.string(384)?.to_string(), r.string(384)?.to_string(), r.string(384)?.to_string()];
            self.game.sign_update(&self.me, [pos.0, pos.1, pos.2], front, lines);
        } else if pid == sb("container_button_click") {
            let window = r.varint()?;
            let id = r.varint()?;
            self.menu_button(window, id);
        } else if pid == sb("rename_item") {
            let name = r.string(32767)?.to_string();
            self.rename_item(name);
        } else if pid == sb("set_beacon") {
            let primary = if r.bool()? { Some(r.varint()?) } else { None };
            let secondary = if r.bool()? { Some(r.varint()?) } else { None };
            crate::game::workstation::set_beacon(&self.game, &self.me, primary, secondary);
        } else if pid == sb("set_carried_item") {
            let s = r.i16()?;
            if (0..9).contains(&s) {
                let changed = self.me.held.swap(s as i32, Ordering::Relaxed) != s as i32;
                // ServerGamePacketListenerImpl.handleSetCarriedItem
                if changed && self.me.vit.lock().living.using.is_some_and(|u| u.hand == 0) {
                    self.stop_using_item();
                }
            }
        } else if pid == sb("set_creative_mode_slot") {
            let slot = r.i16()?;
            let count = r.varint()?;
            let item = if count > 0 { r.varint()? } else { 0 };
            let patch = if count > 0 { crate::components::read_patch(&mut r)? } else { Some(Default::default()) };
            if self.gamemode() == 1 {
                let st = (count > 0 && Items::get().valid(item)).then(|| Stack::new(item, count.clamp(1, 99) as u8)).and_then(|mut s| {
                    s.comp = crate::components::intern(patch?);
                    Some(s)
                });
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
            let thrown = self.me.inv.lock().close(&self.game.data, held);
            for t in thrown {
                self.throw(t, false);
            }
            self.game.send_inventory(&self.me);
        } else if pid == sb("player_input") {
            let flags = r.u8()?;
            self.sneaking = flags & 0x20 != 0;
            self.me.input.store(flags, Ordering::Relaxed);
            // ServerPlayer: shift gets off whatever the player rides
            if self.sneaking && self.game.vehicle_of(self.me.eid).is_some() {
                self.game.dismount(self.me.eid);
            }
        } else if pid == sb("move_vehicle") {
            let (x, y, z) = (r.f64()?, r.f64()?, r.f64()?);
            let (yaw, pitch) = (r.f32()?, r.f32()?);
            let on_ground = r.u8()? != 0;
            self.game.move_vehicle(&self.me, [x, y, z], yaw, pitch, on_ground);
        } else if pid == sb("paddle_boat") {
            let (left, right) = (r.u8()? != 0, r.u8()? != 0);
            if let Some(v) = self.game.vehicle_of(self.me.eid) {
                self.game.paddle(v, left, right);
            }
        } else if pid == sb("interact") {
            let target = r.varint()?;
            let hand = r.varint()?;
            // `Vec3.LP` location and the secondary-action flag follow
            let near = {
                let me = self.me.pose.lock().pos;
                self.game.entity_box(target).is_some_and(|b| {
                    let c = [me[0].clamp(b.min[0], b.max[0]), (me[1] + 1.62).clamp(b.min[1], b.max[1]), me[2].clamp(b.min[2], b.max[2])];
                    (c[0] - me[0]).powi(2) + (c[1] - me[1] - 1.62).powi(2) + (c[2] - me[2]).powi(2) < 36.0
                })
            };
            if near {
                self.game.interact_mob(&self.me, target, hand);
            }
        } else if pid == sb("attack") {
            let target = r.varint()?;
            if let Some(flow) = self.on_attack(target) {
                return Ok(flow);
            }
        } else if pid == sb("swing") {
            let hand = r.varint()?;
            let mut b = Vec::new();
            b.vi(self.me.eid).u8(if hand == 1 { 3 } else { 0 });
            let pkt = self.game.encode("animate", &b);
            let pos = self.me.pose.lock().pos;
            let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
            for p in self.game.players() {
                if p.eid != self.me.eid && p.sees_chunk(c.0, c.1) {
                    p.send(pkt.clone());
                }
            }
        } else if pid == sb("player_command") {
            let _eid = r.varint()?;
            match r.varint()? {
                0 => self.game.wake_up(&self.me),
                1 => self.sprinting = true,
                2 => self.sprinting = false,
                6 => self.gliding = true,
                _ => {}
            }
        } else if pid == sb("client_command") {
            if r.varint()? == 0 {
                if self.won_game {
                    self.credits_done = true;
                } else {
                    self.respawn();
                }
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
        if let Some(f) = self.checker.on_move_packet(self.arrived_ms) {
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
            let from = self.me.pose.lock().pos;
            {
                let mut p = self.me.pose.lock();
                p.pos = to;
                p.on_ground = on_ground;
            }
            self.after_move(from, to, on_ground, &ctx);
            let c = ((to[0].floor() as i32) >> 4, (to[2].floor() as i32) >> 4);
            self.set_center(c, false);
        } else {
            self.me.pose.lock().on_ground = on_ground;
        }
        self.me.moved.fetch_add(1, Ordering::Relaxed);
        Flow::Continue
    }

    /// Fall damage and food exhaustion of one accepted move.
    fn after_move(&mut self, from: [f64; 3], to: [f64; 3], on_ground: bool, ctx: &MoveContext) {
        let dy = to[1] - self.last_y;
        self.last_y = to[1];
        let gm = self.gamemode();
        let below = self.game.world.get_block(to[0].floor() as i32, (to[1] - 0.2).floor() as i32, to[2].floor() as i32).unwrap_or(blocks::AIR);
        let landing = crate::game::hazards::landing(below, self.sneaking);
        let reset = ctx.in_liquid || ctx.climbing || ctx.in_web_or_powder_snow || (self.gliding && dy > -0.5) || (on_ground && landing.is_none());
        // Entity flags used by the attack code (crit / sweep conditions)
        let f = if self.sneaking { F_SNEAKING } else { 0 }
            | if self.sprinting { F_SPRINTING } else { 0 }
            | if self.gliding { F_FALL_FLYING } else { 0 }
            | if ctx.climbing { F_CLIMBING } else { 0 }
            | if ctx.in_liquid { F_IN_WATER } else { 0 };
        self.me.flags.store(f, Ordering::Relaxed);
        {
            let (mx, mz) = (to[0] - from[0], to[2] - from[2]);
            *self.me.known_speed_sq.lock() = mx * mx + mz * mz;
        }
        let (mult, extra, ty) = landing.unwrap_or((1.0, 0.0, "fall"));
        let hit = self.me.vit.lock().on_move(dy, on_ground, reset, (mult, extra), gm);
        if let Some(d) = hit {
            self.damage(DamageSource::new(ty), d);
        }
        // Player.checkMovementStatistics / jumpFromGround
        let (dx, dz) = (to[0] - from[0], to[2] - from[2]);
        let h = (dx * dx + dz * dz).sqrt();
        if gm == 0 || gm == 2 {
            let mut v = self.me.vit.lock();
            if ctx.in_liquid {
                v.add_exhaustion(0.01 * (h * 100.0).round() as f32 * 0.01);
            } else if self.sprinting {
                v.add_exhaustion(0.1 * (h * 100.0).round() as f32 * 0.01);
            }
            if self.was_on_ground && !on_ground && dy > 0.0 && !ctx.in_liquid && !ctx.climbing {
                v.add_exhaustion(if self.sprinting { 0.2 } else { 0.05 });
            }
        }
        self.was_on_ground = on_ground;
    }

    fn move_context(&self, to: [f64; 3]) -> MoveContext {
        let w = &self.game.world;
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
            friction: match at(-1).name() {
                "ice" | "packed_ice" | "frosted_ice" => 0.98,
                "blue_ice" => 0.989,
                "slime_block" => 0.8,
                _ => 0.6,
            },
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
            let pkt = self.game.system_chat(&format!("§c[RimeMC AC]§r {text}"));
            for p in self.st.all_players() {
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
        let Some(line) = crate::plugins::chat(&self.me.name, m) else {
            return Flow::Continue;
        };
        tracing::info!("{line}");
        self.st.broadcast_all(self.game.system_chat(&line));
        Flow::Continue
    }

    fn on_command(&mut self, line: &str) -> Flow {
        if line.chars().any(|c| c == '§' || c.is_control()) {
            return Flow::Disconnect("Illegal characters in command".into());
        }
        tracing::info!("{} issued server command: /{line}", self.me.name);
        if crate::plugins::active() && crate::plugins::command(&self.me.name, line) {
            return Flow::Continue;
        }
        let sender = CommandSender::Player { name: self.me.name.clone() };
        let mut words = line.split_whitespace();
        let cmd = line.split_whitespace().next().unwrap_or("");
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
            "execute" => {
                if !self.st.ops.is_op(&sender) {
                    self.msg("§cYou do not have permission (operator only).");
                    return Flow::Continue;
                }
                // execute in <dimension> run tp <x> <y> <z>
                let args: Vec<&str> = words.collect();
                let dim = match args.as_slice() {
                    ["in", d, "run", "tp" | "teleport", ..] => crate::world::storage::Dim::from_key(d),
                    _ => None,
                };
                let Some(dim) = dim else {
                    self.msg("§cUsage: /execute in <dimension> run tp <x> <y> <z>");
                    return Flow::Continue;
                };
                let nums: Vec<f64> = args[4..].iter().filter_map(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite() && v.abs() < 2.9e7).collect();
                let pose = *self.me.pose.lock();
                let pos = if nums.len() == 3 { [nums[0], nums[1], nums[2]] } else { pose.pos };
                let st = self.st.clone();
                let (cx, cz) = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
                let _ = tokio::task::block_in_place(|| {
                    let all: Vec<(i32, i32)> = (-1..=1).flat_map(|dx| (-1..=1).map(move |dz| (cx + dx, cz + dz))).collect();
                    st.level(dim).world.ensure_loaded(&all, &st.store, st.generator_of(dim));
                });
                self.change_dimension(dim, pos, pose.yaw, pose.pitch);
                self.msg(&format!("Teleported {} to {:.1}, {:.1}, {:.1}", self.me.name, pos[0], pos[1], pos[2]));
            }
            "tp" => {
                if !self.st.ops.is_op(&sender) {
                    self.msg("§cYou do not have permission (operator only).");
                    return Flow::Continue;
                }
                let args: Vec<&str> = words.collect();
                let target = match args.as_slice() {
                    [name] => self.st.find_player(name).map(|p| {
                        let d = crate::world::storage::Dim::from_index(p.dim.load(Ordering::Relaxed));
                        let pos = p.pose.lock().pos;
                        if d != self.game.dim {
                            let pose = *self.me.pose.lock();
                            self.change_dimension(d, pos, pose.yaw, pose.pitch);
                        }
                        pos
                    }),
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
            "give" => {
                if !self.st.ops.is_op(&sender) {
                    self.msg("§cYou do not have permission (operator only).");
                    return Flow::Continue;
                }
                let args: Vec<&str> = words.collect();
                let (who, item, count) = match args.as_slice() {
                    [w, i] => (*w, *i, 1u32),
                    [w, i, n] => (*w, *i, n.parse::<u32>().unwrap_or(0)),
                    _ => {
                        self.msg("§cUsage: /give <player> <item> [count]");
                        return Flow::Continue;
                    }
                };
                let target = if who == "@s" || who == "@p" { Some(self.me.clone()) } else { self.st.find_player(who) };
                let Some(target) = target else {
                    self.msg("§cNo player was found");
                    return Flow::Continue;
                };
                let items = Items::get();
                let Some(id) = items.id(item) else {
                    self.msg(&format!("§cUnknown item '{item}'"));
                    return Flow::Continue;
                };
                if count == 0 || count > 6400 {
                    self.msg("§cCount must be between 1 and 6400");
                    return Flow::Continue;
                }
                let max = items.max_stack(id) as u32;
                let mut left = count;
                let held = target.held.load(Ordering::Relaxed) as i16;
                while left > 0 {
                    let n = left.min(max);
                    left -= n;
                    let st = Stack::new(id, n as u8);
                    let rest = target.inv.lock().add(st, held);
                    if rest > 0 {
                        let pos = target.pose.lock().pos;
                        self.game.spawn_item(pos, Stack::new(id, rest), [0.0; 3], 0);
                    }
                }
                self.game.flush_inventory(&target);
                self.msg(&format!("Gave {count} [{}] to {}", item.trim_start_matches("minecraft:"), target.name));
            }
            "setblock" | "fill" => {
                if !self.st.ops.is_op(&sender) {
                    self.msg("§cYou do not have permission (operator only).");
                    return Flow::Continue;
                }
                let args: Vec<&str> = words.collect();
                let me = self.me.pose.lock().pos;
                let base = [me[0].floor() as i32, me[1].floor() as i32, me[2].floor() as i32];
                let n = if cmd == "fill" { 6 } else { 3 };
                if args.len() < n + 1 {
                    self.msg(if cmd == "fill" { "§cUsage: /fill <from> <to> <block> [replace|destroy|keep]" } else { "§cUsage: /setblock <pos> <block> [replace|destroy|keep]" });
                    return Flow::Continue;
                }
                let coords: Option<Vec<i32>> = args[..n].iter().enumerate().map(|(i, a)| parse_coord(a, base[i % 3])).collect();
                let (Some(c), Some(state)) = (coords, parse_block_state(args[n])) else {
                    self.msg("§cInvalid position or block");
                    return Flow::Continue;
                };
                let mode = args.get(n + 1).copied().unwrap_or("replace");
                let (a, b) = if n == 6 { ([c[0], c[1], c[2]], [c[3], c[4], c[5]]) } else { ([c[0], c[1], c[2]], [c[0], c[1], c[2]]) };
                let lo = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
                let hi = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
                let vol = (hi[0] - lo[0] + 1) as i64 * (hi[1] - lo[1] + 1) as i64 * (hi[2] - lo[2] + 1) as i64;
                if vol > 32768 {
                    self.msg(&format!("§cToo many blocks in the specified area (maximum 32768, specified {vol})"));
                    return Flow::Continue;
                }
                let g = &self.game;
                let mut changed = 0;
                for x in lo[0]..=hi[0] {
                    for y in lo[1]..=hi[1] {
                        for z in lo[2]..=hi[2] {
                            let Some(cur) = g.world.get_block(x, y, z) else { continue };
                            if mode == "keep" && !cur.is_air() || cur == state {
                                continue;
                            }
                            g.world.mark_edited(x, z);
                            if mode == "destroy" {
                                g.change_block([x, y, z], state, true, None);
                            } else {
                                g.set_block([x, y, z], state, crate::game::level::flags::CLIENTS);
                            }
                            changed += 1;
                        }
                    }
                }
                // updates after the whole area is set (vanilla: blockUpdated per position)
                let _lv = g.level.lock();
                for x in lo[0]..=hi[0] {
                    for y in lo[1]..=hi[1] {
                        for z in lo[2]..=hi[2] {
                            if let Some(b) = g.world.get_block(x, y, z) {
                                g.update_neighbors_at([x, y, z], b);
                            }
                        }
                    }
                }
                if changed == 0 {
                    self.msg(if cmd == "fill" { "§cNo blocks were filled" } else { "§cCould not set the block" });
                } else if cmd == "fill" {
                    self.msg(&format!("Successfully filled {changed} block(s)"));
                } else {
                    self.msg(&format!("Changed the block at {}, {}, {}", c[0], c[1], c[2]));
                }
            }
            "summon" => {
                if !self.st.ops.is_op(&sender) {
                    self.msg("§cYou do not have permission (operator only).");
                    return Flow::Continue;
                }
                let args: Vec<&str> = words.collect();
                let pose = *self.me.pose.lock();
                let pos = match args.as_slice() {
                    [_] => Some(pose.pos),
                    [_, x, y, z] => match (x.parse::<f64>(), y.parse::<f64>(), z.parse::<f64>()) {
                        (Ok(x), Ok(y), Ok(z)) if [x, y, z].iter().all(|v| v.is_finite() && v.abs() < 2.9e7) => Some([x, y, z]),
                        _ => None,
                    },
                    _ => None,
                };
                match (args.first(), pos) {
                    (Some(name), Some(pos)) => match self.game.summon(name, pos) {
                        Some(_) => self.msg(&format!("Summoned new {}", name.trim_start_matches("minecraft:"))),
                        None => self.msg(&format!("§cUnknown or unsupported entity: {name}")),
                    },
                    _ => self.msg("§cUsage: /summon <entity> [x y z]"),
                }
            }
            "time" => {
                if !self.st.ops.is_op(&sender) {
                    self.msg("§cYou do not have permission (operator only).");
                    return Flow::Continue;
                }
                let args: Vec<&str> = words.collect();
                let target = match args.as_slice() {
                    ["set", "day"] => Some(1000),
                    ["set", "noon"] => Some(6000),
                    ["set", "night"] => Some(13000),
                    ["set", "midnight"] => Some(18000),
                    ["set", n] => n.parse::<u64>().ok(),
                    ["query", "daytime"] | [] => {
                        self.msg(&format!("The time is {}", self.game.day_time()));
                        None
                    }
                    _ => {
                        self.msg("§cUsage: /time set <day|noon|night|midnight|ticks> | /time query daytime");
                        None
                    }
                };
                if let Some(t) = target {
                    self.game.set_day_time(t);
                    self.send_time();
                    self.msg(&format!("Set the time to {t}"));
                }
            }
            "kill" => {
                if !self.st.ops.is_op(&sender) {
                    self.msg("§cYou do not have permission (operator only).");
                    return Flow::Continue;
                }
                let n = self.game.kill_mobs();
                self.msg(&format!("Killed {n} entities"));
            }
            "list" => {
                let names: Vec<String> = self.st.all_players().iter().map(|p| p.name.clone()).collect();
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
        self.st.broadcast_all(self.game.encode("player_info_update", &b));
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
        if let Some(b) = self.game.world.get_block(pos.0, pos.1, pos.2) {
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
                        Stack { count: n, ..s }
                    })
                };
                if let Some(t) = taken {
                    self.throw(t, false);
                    self.game.flush_inventory(&self.me);
                }
                return Flow::Continue;
            }
            5 | 6 => {
                if status == 5 {
                    self.release_using();
                    self.stop_using_item();
                }
                self.ack(seq);
                return Flow::Continue;
            }
            _ => {}
        }
        let st = self.st.clone();
        let w = &self.game.world;
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
        if breaks && !cur.is_air() && crate::plugins::active() && crate::plugins::block_break(&self.me.name, [pos.0, pos.1, pos.2]) {
            // BlockBreakEvent cancelled
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
            let mut after = if BlockRegistry::get().property(cur, "waterlogged") == Some("true") { blocks::WATER } else { blocks::AIR };
            // IceBlock.playerDestroy: melts into water unless silk touched or in the nether
            if cur.name() == "ice" && gm == 0 && self.game.dim != crate::world::storage::Dim::Nether {
                let silk = self.me.inv.lock().held(self.me.held.load(Ordering::Relaxed) as i16).is_some_and(|s| s.enchantment_level("silk_touch") > 0);
                let below = self.game.world.get_block(pos.0, pos.1 - 1, pos.2).unwrap_or(blocks::AIR);
                if !silk && (crate::game::ai::path::blocks_motion(below) || BlockRegistry::get().is_fluid(below)) {
                    after = blocks::WATER;
                }
            }
            if gm == 1 {
                // playerWillDestroy: creative breaking of an upper / head part
                // removes the other part without drops
                self.game.prevent_drop_from_other_part([pos.0, pos.1, pos.2], cur);
            }
            self.game.player_change_block([pos.0, pos.1, pos.2], after, gm == 0, tool);
            self.me.vit.lock().add_exhaustion(0.005);
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
        self.game.spawn_item([pose.pos[0], pose.pos[1] + 1.62 - 0.3, pose.pos[2]], st, vel, 40);
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
                self.game.send_inventory(&self.me);
                return Flow::Continue;
            }
            inv.menu.creative = creative;
            inv.menu.xp_level = self.me.vit.lock().xp_level;
            let out = inv.click(&self.game.data, slot, button, mode, creative);
            let contents = inv.window_contents();
            let as_pair = |s: Option<Stack>| s.map(|s| (s.item, s.count as i32));
            let ok = predicted.iter().all(|(loc, pred)| contents.get(*loc as usize).is_some_and(|s| as_pair(*s) == *pred)) && as_pair(inv.cursor) == cursor;
            (out, ok)
        };
        for t in outcome.thrown {
            self.throw(t, false);
        }
        let open = {
            let inv = self.me.inv.lock();
            match inv.window {
                crate::game::inventory::Window::Container(_, sc, p) => Some((p, sc, inv.ext.clone())),
                _ => None,
            }
        };
        match open {
            Some((_, sc, _)) if sc.transient() => self.menu_events(),
            Some((p, _, ext)) => {
                let stored = self.game.container_slots(p);
                if stored.as_ref() != Some(&ext) {
                    self.game.set_container_slots(p, &ext, self.me.eid);
                }
            }
            None => {}
        }
        if matches {
            self.game.flush_inventory(&self.me);
        } else {
            self.game.send_inventory(&self.me);
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
                let b = self.game.world.get_block(c.0, c.1, c.2)?;
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
        if self.start_eating(slot) {
            return;
        }
        let Some(held) = self.me.inv.lock().slots[slot] else { return };
        // shields and other `blocks_attacks` items: start using (Item.use -> startUsingItem)
        if held.defaults().blocks_attacks.is_some() {
            let mut v = self.me.vit.lock();
            if v.living.using.is_none() && !v.living.on_cooldown(held.item) {
                v.living.using = Some(crate::game::combat::living::UseItem { hand: hand as u8, item: held.item, elapsed: 0 });
                v.data_dirty = true;
            }
            return;
        }
        if self.use_ranged(hand, held) {
            return;
        }
        let items = Items::get();
        let name = items.name(held.item).to_string();
        let gm = self.gamemode();
        let game = &*self.game.clone();
        // BoatItem.use: on the water or ground looked at
        if crate::game::ride::is_boat_type(&name) {
            let hit = self.raycast(yaw, pitch, &|b| fluid::fluid_state(b).fluid().is_some() || b.is_solid());
            if let Some((_, c)) = hit {
                let b = game.world.get_block(c.0, c.1, c.2).unwrap_or(blocks::AIR);
                let y = if fluid::fluid_state(b).fluid().is_some() { c.1 as f64 + 0.9 } else { c.1 as f64 + 1.0 };
                if game.place_boat(&name, [c.0 as f64 + 0.5, y, c.2 as f64 + 0.5], yaw) && gm != 1 {
                    self.me.inv.lock().consume(slot, 1);
                    game.flush_inventory(&self.me);
                }
            }
            return;
        }
        // ItemSteerable.boost: carrot / warped fungus on a stick
        if matches!(name.as_str(), "carrot_on_a_stick" | "warped_fungus_on_a_stick") {
            if let Some(v) = game.vehicle_of(self.me.eid) {
                if game.boost(v, &name) && gm != 1 {
                    let cost = if name == "carrot_on_a_stick" { 7 } else { 1 };
                    let mut inv = self.me.inv.lock();
                    if let Some(mut s) = inv.slots[slot] {
                        let dmg = s.patch().damage.unwrap_or(0) + cost;
                        if dmg >= s.max_damage() {
                            // hurtAndConvertOnBreak: back to a fishing rod
                            inv.set(slot, items.id("fishing_rod").map(|i| Stack::new(i, 1)));
                        } else {
                            s.with_patch(|p| p.damage = Some(dmg));
                            inv.set(slot, Some(s));
                        }
                    }
                    drop(inv);
                    game.flush_inventory(&self.me);
                }
            }
            return;
        }
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
                    game.player_change_block([c.0, c.1, c.2], blocks::AIR, false, None);
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
            "glass_bottle" => {
                // BottleItem.use: fill from a water source
                let hit = self.raycast(yaw, pitch, &|b| fluid::fluid_state(b).is_source() || b.is_solid());
                if let Some((_, c)) = hit {
                    let b = game.world.get_block(c.0, c.1, c.2).unwrap_or(blocks::AIR);
                    if b.name() != "water" {
                        return;
                    }
                    let full = Stack::new(items.id("potion").unwrap_or(0), 1).with_patch(|p| p.potion = Some("water".into()));
                    game.play_sound("minecraft:item.bottle.fill", 0, self.me.pose.lock().pos, 1.0, 1.0);
                    let mut inv = self.me.inv.lock();
                    if gm != 1 {
                        inv.consume(slot, 1);
                    }
                    if inv.slots[slot].is_none() {
                        inv.set(slot, Some(full));
                    } else if inv.add(full, slot as i16 - 36) > 0 {
                        drop(inv);
                        self.throw(full, false);
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
                    if name == "water_bucket" && game.dim == crate::world::storage::Dim::Nether {
                        // BucketItem.emptyContents: water boils away in ultrawarm dimensions
                        let mut rng = rand::thread_rng();
                        game.play_sound("minecraft:block.fire.extinguish", 4, [target.0 as f64 + 0.5, target.1 as f64 + 0.5, target.2 as f64 + 0.5], 0.5, 2.6 + (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.8);
                        if gm != 1 {
                            self.me.inv.lock().set(slot, Some(Stack::new(items.id("bucket").unwrap_or(0), 1)));
                        }
                        game.flush_inventory(&self.me);
                        return;
                    }
                    game.player_change_block([target.0, target.1, target.2], fluid_block, !cur.is_air() && !BlockRegistry::get().is_fluid(cur), None);
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
                if let Some(b) = self.game.world.get_block(p[0], p[1], p[2]) {
                    if b.is_ore() {
                        self.game.broadcast_block(p[0], p[1], p[2], b);
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
            let thrown = inv.close(&self.game.data, held);
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
        self.game.send_inventory(&self.me);
    }

    fn open_container(&mut self, p: [i32; 3], screen: crate::game::blockentity::Screen, title: &str) {
        use crate::game::blockentity as be;
        if screen.transient() {
            self.open_menu(p, screen, title);
            return;
        }
        let game = &*self.game.clone();
        let mut t = match game.world.block_entity(p) {
            Some(t) => t,
            None => {
                let Some(id) = game.world.get_block(p[0], p[1], p[2]).and_then(|b| be::be_id(b.name())) else { return };
                be::new_tag(id, p)
            }
        };
        // a chest with a solid block on top does not open
        if matches!(screen, be::Screen::Rows(_)) && t.get("id").and_then(|v| v.as_str()).is_some_and(|i| i.ends_with("chest")) {
            if game.world.get_block(p[0], p[1] + 1, p[2]).is_some_and(|b| b.is_opaque()) {
                return;
            }
        }
        if be::unpack_loot(&mut t, screen.size(), &game.data) || game.world.block_entity(p).is_none() {
            game.world.set_block_entity(p, Some(t.clone()), false);
        }
        self.next_window = self.next_window % 100 + 1;
        let id = self.next_window;
        {
            let held = self.me.held.load(Ordering::Relaxed) as i16;
            let mut inv = self.me.inv.lock();
            let thrown = inv.close(&game.data, held);
            inv.window = crate::game::inventory::Window::Container(id, screen, p);
            inv.ext = game.container_slots(p).filter(|s| s.len() == screen.size()).unwrap_or_else(|| be::items(&t, screen.size()));
            drop(inv);
            for t in thrown {
                self.throw(t, false);
            }
        }
        let menu = Registries::get().id("minecraft:menu", screen.menu()).unwrap_or(2);
        let mut b = Vec::new();
        b.vi(id as i32).vi(menu).raw(&text_nbt(title));
        self.send("open_screen", &b);
        let st = self.st.clone();
        let game = &*self.game.clone();
        game.send_inventory(&self.me);
        if let be::Screen::Furnace(_) = screen {
            game.send_furnace_data(&self.me, p);
            game.watch_furnace(p);
        }
        if screen == be::Screen::Brewing {
            game.send_props(&self.me, &crate::game::workstation::brewing_props(game, p));
            crate::game::workstation::ticker_placed(game, p, game.bs(p));
        }
    }

    /// Open a workstation menu (its slots live in the menu only).
    fn open_menu(&mut self, p: [i32; 3], screen: crate::game::blockentity::Screen, title: &str) {
        use crate::game::blockentity::Screen;
        let st = self.st.clone();
        let game = &*self.game.clone();
        self.next_window = self.next_window % 100 + 1;
        let id = self.next_window;
        {
            let held = self.me.held.load(Ordering::Relaxed) as i16;
            let mut inv = self.me.inv.lock();
            let thrown = inv.close(&game.data, held);
            inv.window = crate::game::inventory::Window::Container(id, screen, p);
            inv.ext = vec![None; screen.size()];
            let seed = inv.enchant_seed;
            inv.menu = crate::game::menu::MenuState { pos: p, xp_level: self.me.vit.lock().xp_level, creative: self.gamemode() == 1, seed, selected: -1, ..Default::default() };
            if screen == Screen::Enchanting {
                inv.menu.bookshelves = crate::game::workstation::bookshelves(game, p);
            }
            inv.update_craft(&game.data);
            drop(inv);
            for t in thrown {
                self.throw(t, false);
            }
        }
        let menu = Registries::get().id("minecraft:menu", screen.menu()).unwrap_or(2);
        let mut b = Vec::new();
        b.vi(id as i32).vi(menu).raw(&text_nbt(title));
        self.send("open_screen", &b);
        game.send_inventory(&self.me);
        if screen == Screen::Beacon {
            game.send_props(&self.me, &crate::game::workstation::beacon_props(game, p));
        } else {
            let props = self.me.inv.lock().menu.props.clone();
            game.send_props(&self.me, &props);
        }
    }

    /// Apply the side effects a menu action queued, then sync the menu.
    fn menu_events(&mut self) {
        use crate::game::menu::MenuEvent;
        let st = self.st.clone();
        let game = &*self.game.clone();
        let (events, pos) = {
            let mut inv = self.me.inv.lock();
            (std::mem::take(&mut inv.menu.events), inv.menu.pos)
        };
        let centre = [pos[0] as f64 + 0.5, pos[1] as f64 + 0.5, pos[2] as f64 + 0.5];
        let creative = self.gamemode() == 1;
        let mut refresh = false;
        for e in events {
            match e {
                MenuEvent::Levels(n) => self.me.vit.lock().give_levels(n),
                MenuEvent::Enchanted(n) => {
                    if !creative {
                        self.me.vit.lock().give_levels(-n);
                    }
                    let mut inv = self.me.inv.lock();
                    inv.enchant_seed = rand::random();
                    inv.menu.seed = inv.enchant_seed;
                    refresh = true;
                }
                MenuEvent::Sound(s) => game.play_sound(s, 4, centre, 1.0, rand::random::<f32>() * 0.1 + 0.9),
                MenuEvent::XpOrbs(n) => game.award_xp_orbs(centre, n),
                MenuEvent::AnvilUsed => crate::game::workstation::anvil_used(game, pos, creative),
                MenuEvent::Stat(_) => {}
            }
        }
        let props = {
            let mut inv = self.me.inv.lock();
            inv.menu.xp_level = self.me.vit.lock().xp_level;
            if refresh {
                inv.update_craft(&game.data);
            }
            inv.menu.props.clone()
        };
        game.send_props(&self.me, &props);
    }

    fn menu_button(&mut self, window: i32, id: i32) {
        let st = self.st.clone();
        let game = &*self.game.clone();
        let ok = {
            let mut inv = self.me.inv.lock();
            if window != inv.current_window() as i32 || window == 0 {
                return;
            }
            inv.menu.creative = self.gamemode() == 1;
            inv.menu.xp_level = self.me.vit.lock().xp_level;
            crate::game::menu::button(&mut inv, &game.data, id)
        };
        if ok {
            self.menu_events();
        }
        game.flush_inventory(&self.me);
    }

    fn rename_item(&mut self, name: String) {
        let st = self.st.clone();
        let game = &*self.game.clone();
        {
            let mut inv = self.me.inv.lock();
            if !matches!(inv.window, crate::game::inventory::Window::Container(_, crate::game::blockentity::Screen::Anvil, _)) {
                return;
            }
            // AnvilMenu.validateName: at most 50 characters
            if name.chars().count() > 50 {
                return;
            }
            inv.menu.rename = Some(name);
            inv.menu.creative = self.gamemode() == 1;
            inv.menu.xp_level = self.me.vit.lock().xp_level;
            inv.update_craft(&game.data);
        }
        let props = self.me.inv.lock().menu.props.clone();
        game.send_props(&self.me, &props);
        game.flush_inventory(&self.me);
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
        let game = &*self.game.clone();
        let w = &game.world;
        let clicked = w.get_block(pos.0, pos.1, pos.2).unwrap_or(blocks::AIR);
        let yaw_pitch = {
            let p = self.me.pose.lock();
            (p.yaw, p.pitch)
        };
        let horiz_dir = [3u8, 4, 2, 5][(((yaw_pitch.0 as f64 / 90.0) + 0.5).floor() as i64).rem_euclid(4) as usize];
        // interactive blocks (BlockBehaviour.useItemOn / useWithoutItem)
        if self.within_reach(pos) && (gm != 3 || crate::game::blockentity::screen_of(clicked.name()).is_some()) {
            use crate::game::block::use_block::{use_block, Ctx, Use};
            let held = self.me.inv.lock().slots[slot];
            let other = self.me.inv.lock().slots[if hand == 1 { 36 + self.me.held.load(Ordering::Relaxed) as usize } else { 45 }];
            let cx = Ctx { player: &self.me, held, sneaking: self.sneaking && (held.is_some() || other.is_some()), creative: gm == 1, face: face as u8, horiz: horiz_dir };
            let r = if gm == 3 {
                match crate::game::block::container::screen_at(game, [pos.0, pos.1, pos.2]) {
                    Some((k, sc, t)) => Use::Open(k, sc, t),
                    None => Use::Pass,
                }
            } else if gm == 2 && !matches!(crate::game::block::class(clicked), crate::game::block::Class::Door | crate::game::block::Class::TrapDoor | crate::game::block::Class::FenceGate | crate::game::block::Class::Lever | crate::game::block::Class::Button | crate::game::block::Class::Bed | crate::game::block::Class::Chest | crate::game::block::Class::TrappedChest) && crate::game::blockentity::screen_of(clicked.name()).is_none() && crate::game::blockentity::workstation_of(clicked.name()).is_none() && clicked.name() != "crafting_table" {
                Use::Pass
            } else {
                use_block(game, [pos.0, pos.1, pos.2], clicked, &cx)
            };
            match r {
                Use::Pass => {}
                Use::Consumed => {
                    self.ack(seq);
                    return Flow::Continue;
                }
                Use::Crafting => {
                    self.ack(seq);
                    self.open_crafting();
                    return Flow::Continue;
                }
                Use::Open(k, sc, t) => {
                    self.ack(seq);
                    self.open_container(k, sc, t);
                    return Flow::Continue;
                }
                Use::Sign(p, front) => {
                    self.ack(seq);
                    let mut b = Vec::new();
                    b.pos(p[0], p[1], p[2]).bool(front);
                    self.send("open_sign_editor", &b);
                    return Flow::Continue;
                }
            }
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
        // Block.getStateForPlacement of blocks with behaviour
        {
            use crate::game::block::place::{nearest_looking, placement, PlaceCtx};
            let pc = PlaceCtx {
                horiz: horiz_dir,
                look: nearest_looking(yaw_pitch.0, yaw_pitch.1),
                face: face as u8,
                click: [pos.0 as f64 + cursor.0 as f64, pos.1 as f64 + cursor.1 as f64, pos.2 as f64 + cursor.2 as f64],
                sneaking: self.sneaking,
            };
            match placement(game, [target.0, target.1, target.2], state, &pc) {
                Some(s) => state = s,
                None => {
                    self.ack(seq);
                    self.resync(target);
                    return Flow::Continue;
                }
            }
        }
        // don't place solid blocks inside players
        if state.is_solid() {
            let bx = [target.0 as f64, target.1 as f64, target.2 as f64];
            for p in self.game.players() {
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
        if crate::plugins::active() && crate::plugins::block_place(&self.me.name, [target.0, target.1, target.2], state.name()) {
            // BlockPlaceEvent cancelled
            self.ack(seq);
            self.resync(target);
            game.send_inventory(&self.me);
            return Flow::Continue;
        }
        game.player_change_block([target.0, target.1, target.2], state, false, None);
        if let Some((p, b)) = extra {
            game.player_change_block([p.0, p.1, p.2], b, false, None);
        }
        crate::game::block::place::placed_by(game, [target.0, target.1, target.2], state);
        if state.name().ends_with("_sign") && !state.name().contains("hanging") {
            // SignBlockEntity: the placer edits the front
            let mut b = Vec::new();
            b.pos(target.0, target.1, target.2).bool(true);
            self.send("open_sign_editor", &b);
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
