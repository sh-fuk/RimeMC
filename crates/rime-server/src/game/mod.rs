//! Live game state shared by all connections: loaded chunks and online players.
//!
//! Chunks live in 64 independently locked shards so connections on different
//! cores never contend. Each loaded chunk caches its encoded network packet,
//! so sending a chunk to N players costs one serialisation, not N.

pub mod ai;
pub mod block;
pub mod blockhelp;
pub use blockhelp::chunk_min_max_y;
pub mod level;
pub mod blockentity;
pub mod combat;
pub mod consume;
pub mod hazards;
pub mod randomtick;
pub mod physics;
pub mod projectile;
pub mod etypes;
pub mod hurt;
pub mod weather;
pub mod mob;
pub mod mobtick;
pub mod entity;
pub mod fluid;
pub mod interact;
pub mod inventory;
pub mod menu;
pub mod workstation;
pub mod playerdata;
pub mod portal;
pub mod survival;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use rand::Rng;
use rayon::prelude::*;

/// Threads for work the game tick waits on (light updates), separate from the
/// global pool that generates and loads chunks.
pub fn tick_pool() -> &'static rayon::ThreadPool {
    static P: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let n = (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) / 2).max(1);
        rayon::ThreadPoolBuilder::new().num_threads(n).thread_name(|i| format!("rime-tickpool-{i}")).build().expect("tick pool")
    })
}
use tokio::sync::mpsc;

use crate::data::{GameData, Items, Stack};
use crate::net::codec::Out;
use crate::registry::{blocks, BlockRegistry, BlockState, PacketIds, Registries};
use crate::world::chunk::{Chunk, HEIGHT, MIN_Y};
use entity::{Cell, EKind, Entity};
use fluid::{fluid_state, Flow, FluidWorld};
use inventory::Inventory;
use crate::world::generator::Generator;
use crate::world::storage::{Dim, Kind, WorldStore};

const SHARDS: usize = 64;

struct Entry {
    chunk: Chunk,
    dirty: bool,
    viewers: u32,
    /// bumped on every change (lets packets be built outside the lock)
    version: u64,
    /// encoded `level_chunk_with_light` body (invalidated on change)
    packet: Option<Arc<Vec<u8>>>,
    /// last computed light (random ticks, spawning)
    light: Option<Arc<crate::world::light::ChunkLight>>,
}

pub struct World {
    /// which dimension's region files this world reads and writes
    pub dim: Dim,
    shards: Vec<Mutex<HashMap<(i32, i32), Entry>>>,
    /// fluid ticks of freshly generated chunks, scheduled by the next game tick
    pending_fluid: Mutex<Vec<[i32; 3]>>,
    /// chunks whose light must be recomputed and sent again
    light_dirty: Mutex<HashSet<(i32, i32)>>,
}

fn shard(x: i32, z: i32) -> usize {
    ((x as u32).wrapping_mul(0x9E37_79B9) ^ (z as u32).wrapping_mul(0x85EB_CA6B)) as usize % SHARDS
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl World {
    pub fn new() -> Self {
        Self::with_dim(Dim::Overworld)
    }

    pub fn with_dim(dim: Dim) -> Self {
        Self { dim, shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(), pending_fluid: Mutex::new(Vec::new()), light_dirty: Mutex::new(HashSet::new()) }
    }

    pub fn is_loaded(&self, x: i32, z: i32) -> bool {
        self.shards[shard(x, z)].lock().contains_key(&(x, z))
    }

    pub fn loaded(&self) -> usize {
        self.shards.iter().map(|s| s.lock().len()).sum()
    }

    /// Make sure all `coords` are in memory: read from the world store when
    /// present, otherwise generate them in one batch (GPU / all cores).
    /// Blocking; call from a blocking context.
    pub fn ensure_loaded(&self, coords: &[(i32, i32)], store: &WorldStore, generator: &Generator) {
        let missing: Vec<(i32, i32)> = coords.iter().copied().filter(|&(x, z)| !self.shards[shard(x, z)].lock().contains_key(&(x, z))).collect();
        if missing.is_empty() {
            return;
        }
        let loaded: Vec<((i32, i32), Option<Chunk>)> = missing
            .par_iter()
            .map(|&(x, z)| {
                let c = match store.load_chunk(self.dim, Kind::Region, x, z) {
                    // chunks from another generator are generated again unless players changed them
                    Ok(Some(b)) => Chunk::from_nbt(&b).filter(|c| c.x == x && c.z == z && (c.edited || c.gen_id == generator.gen_id())),
                    Ok(None) => None,
                    Err(e) => {
                        tracing::error!("loading chunk {x},{z}: {e:#}");
                        None
                    }
                };
                ((x, z), c)
            })
            .collect();
        let to_gen: Vec<(i32, i32)> = loaded.iter().filter(|(_, c)| c.is_none()).map(|(k, _)| *k).collect();
        let generated = if to_gen.is_empty() { Vec::new() } else { generator.generate_urgent(&to_gen) };
        let insert = |c: Chunk, dirty: bool| {
            let mut s = self.shards[shard(c.x, c.z)].lock();
            s.entry((c.x, c.z)).or_insert(Entry { chunk: c, dirty, viewers: 0, version: 0, packet: None, light: None });
        };
        for (_, c) in loaded {
            if let Some(c) = c {
                insert(c, false);
            }
        }
        for mut c in generated {
            let ticks = std::mem::take(&mut c.fluid_ticks);
            if !ticks.is_empty() {
                self.pending_fluid.lock().extend(ticks);
            }
            insert(c, true); // new chunks are persisted with the next save
        }
    }

    pub fn add_viewer(&self, x: i32, z: i32) {
        if let Some(e) = self.shards[shard(x, z)].lock().get_mut(&(x, z)) {
            e.viewers += 1;
        }
    }

    pub fn remove_viewer(&self, x: i32, z: i32) {
        if let Some(e) = self.shards[shard(x, z)].lock().get_mut(&(x, z)) {
            e.viewers = e.viewers.saturating_sub(1);
        }
    }

    /// Encoded chunk packet body (cached). `build` runs without any lock held,
    /// so it may itself read neighbouring blocks.
    pub fn chunk_packet(&self, x: i32, z: i32, build: &dyn Fn(&Chunk) -> Vec<u8>) -> Option<Arc<Vec<u8>>> {
        let (copy, ver) = {
            let s = self.shards[shard(x, z)].lock();
            let e = s.get(&(x, z))?;
            if let Some(p) = &e.packet {
                return Some(p.clone());
            }
            (e.chunk.clone(), e.version)
        };
        let p = Arc::new(build(&copy));
        if let Some(e) = self.shards[shard(x, z)].lock().get_mut(&(x, z)) {
            if e.version == ver {
                e.packet = Some(p.clone());
            }
        }
        Some(p)
    }

    /// A copy of a loaded chunk.
    pub fn chunk_copy(&self, x: i32, z: i32) -> Option<Chunk> {
        self.shards[shard(x, z)].lock().get(&(x, z)).map(|e| e.chunk.clone())
    }

    pub fn get_block(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
        let s = self.shards[shard(x >> 4, z >> 4)].lock();
        let e = s.get(&(x >> 4, z >> 4))?;
        Some(e.chunk.get((x & 15) as usize, y, (z & 15) as usize))
    }

    /// Several blocks of one chunk under a single lock (random tick sampling).
    pub fn blocks_in_chunk(&self, cx: i32, cz: i32, at: &[[i32; 3]]) -> Option<Vec<BlockState>> {
        let s = self.shards[shard(cx, cz)].lock();
        let e = s.get(&(cx, cz))?;
        Some(at.iter().map(|p| e.chunk.get((p[0] & 15) as usize, p[1], (p[2] & 15) as usize)).collect())
    }

    /// Change a block; returns the previous state (None when the chunk is not loaded
    /// or y is out of range).
    pub fn set_block(&self, x: i32, y: i32, z: i32, b: BlockState) -> Option<BlockState> {
        if !(MIN_Y..MIN_Y + crate::world::chunk::HEIGHT).contains(&y) {
            return None;
        }
        let mut s = self.shards[shard(x >> 4, z >> 4)].lock();
        let e = s.get_mut(&(x >> 4, z >> 4))?;
        let (lx, lz) = ((x & 15) as usize, (z & 15) as usize);
        let old = e.chunk.get(lx, y, lz);
        if old != b {
            e.chunk.set(lx, y, lz, b);
            e.chunk.update_height(lx, y, lz);
            e.dirty = true;
            e.version += 1;
            e.packet = None;
            drop(s);
            if crate::world::light::affects_light(old, b) {
                // light reaches 15 blocks: the 3x3 chunks around may change
                let mut d = self.light_dirty.lock();
                for dx in -1..=1 {
                    for dz in -1..=1 {
                        d.insert(((x >> 4) + dx, (z >> 4) + dz));
                    }
                }
            }
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (-1, 1), (1, -1), (1, 1)] {
                let (nx, nz) = ((x >> 4) + dx, (z >> 4) + dz);
                if let Some(n) = self.shards[shard(nx, nz)].lock().get_mut(&(nx, nz)) {
                    n.version += 1;
                    n.packet = None;
                }
            }
        }
        Some(old)
    }

    /// Recompute the light of chunks changed since the last call (at most
    /// `max` of them, in parallel); returns `(chunk, light_update body)`.
    pub fn relight(&self, max: usize) -> Vec<((i32, i32), Vec<u8>)> {
        // its own pool: on the global one the tick would wait behind world generation jobs
        tick_pool().install(|| self.relight_inner(max))
    }

    fn relight_inner(&self, max: usize) -> Vec<((i32, i32), Vec<u8>)> {
        let todo: Vec<(i32, i32)> = {
            let mut d = self.light_dirty.lock();
            let v: Vec<(i32, i32)> = d.iter().copied().take(max).collect();
            for c in &v {
                d.remove(c);
            }
            v
        };
        todo.into_par_iter()
            .filter_map(|(cx, cz)| {
                let around: Vec<Option<Chunk>> = (0..9).map(|i| self.chunk_copy(cx + i / 3 - 1, cz + i % 3 - 1)).collect();
                around[4].as_ref()?;
                let refs: [Option<&Chunk>; 9] = std::array::from_fn(|i| around[i].as_ref());
                let light = Arc::new(crate::world::light::compute(&refs));
                self.store_light((cx, cz), light.clone());
                let mut b = Vec::with_capacity(8 * 1024);
                b.vi(cx).vi(cz);
                crate::net::play::light_data(&mut b, &light);
                Some(((cx, cz), b))
            })
            .collect()
    }

    /// Keep a chunk's computed light.
    pub fn store_light(&self, c: (i32, i32), l: Arc<crate::world::light::ChunkLight>) {
        if let Some(e) = self.shards[shard(c.0, c.1)].lock().get_mut(&c) {
            e.light = Some(l);
        }
    }

    /// (sky, block) light at a block; sky falls back to the heightmap when
    /// the chunk's light was never computed.
    pub fn light_at(&self, x: i32, y: i32, z: i32) -> Option<(u8, u8)> {
        let s = self.shards[shard(x >> 4, z >> 4)].lock();
        let e = s.get(&(x >> 4, z >> 4))?;
        let sec = ((y - MIN_Y) >> 4) + 1;
        match &e.light {
            Some(l) if sec >= 0 && (sec as usize) < l.sky.len() => {
                let i = (((y & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize;
                let nib = |a: &Option<Box<[u8; 2048]>>| a.as_ref().map(|a| (a[i >> 1] >> ((i & 1) * 4)) & 15).unwrap_or(0);
                Some((nib(&l.sky[sec as usize]), nib(&l.block[sec as usize])))
            }
            _ => {
                let top = e.chunk.heightmap[((z & 15) * 16 + (x & 15)) as usize] as i32;
                Some((if y > top { 15 } else { 0 }, 0))
            }
        }
    }

    /// Positions of loaded block entities whose id is in `ids`.
    pub fn block_entities_with(&self, ids: &[&str]) -> Vec<[i32; 3]> {
        let mut out = Vec::new();
        for sh in self.shards.iter() {
            for e in sh.lock().values() {
                for (p, t) in &e.chunk.block_entities {
                    if t.get("id").and_then(|v| v.as_str()).is_some_and(|id| ids.contains(&id.trim_start_matches("minecraft:"))) {
                        out.push(*p);
                    }
                }
            }
        }
        out
    }

    /// The block entity at `p` (vanilla NBT), if its chunk is loaded.
    pub fn block_entity(&self, p: [i32; 3]) -> Option<crate::nbt::Tag> {
        let s = self.shards[shard(p[0] >> 4, p[2] >> 4)].lock();
        s.get(&(p[0] >> 4, p[2] >> 4))?.chunk.block_entities.get(&p).cloned()
    }

    /// Set or remove a block entity. `visible` invalidates the cached chunk
    /// packet (only data the client renders, e.g. signs, needs it).
    pub fn set_block_entity(&self, p: [i32; 3], t: Option<crate::nbt::Tag>, visible: bool) {
        let mut s = self.shards[shard(p[0] >> 4, p[2] >> 4)].lock();
        let Some(e) = s.get_mut(&(p[0] >> 4, p[2] >> 4)) else { return };
        match t {
            Some(t) => {
                e.chunk.block_entities.insert(p, t);
            }
            None => {
                e.chunk.block_entities.remove(&p);
            }
        }
        e.chunk.edited = true;
        e.dirty = true;
        if visible {
            e.version += 1;
            e.packet = None;
        }
    }

    /// Entities generation placed in a chunk, handed out once.
    pub fn take_gen_entities(&self, x: i32, z: i32) -> Vec<(String, [i32; 3])> {
        let mut s = self.shards[shard(x, z)].lock();
        match s.get_mut(&(x, z)) {
            Some(e) if !e.chunk.gen_entities.is_empty() => {
                e.dirty = true;
                std::mem::take(&mut e.chunk.gen_entities)
            }
            _ => Vec::new(),
        }
    }

    /// Biome name at a block position (loaded chunks only).
    pub fn biome(&self, x: i32, y: i32, z: i32) -> Option<String> {
        let s = self.shards[shard(x >> 4, z >> 4)].lock();
        let e = s.get(&(x >> 4, z >> 4))?;
        let sec = ((y - MIN_Y) >> 4).clamp(0, e.chunk.biomes.len() as i32 - 1) as usize;
        let i = ((((y - MIN_Y) >> 2) & 3) << 4 | ((z >> 2) & 3) << 2 | ((x >> 2) & 3)) as usize;
        let id = e.chunk.biomes[sec][i];
        crate::world::chunk::Biomes::get().names.get(id as usize).cloned()
    }

    /// Is the chunk loaded and in some player's view?
    pub fn viewed(&self, x: i32, z: i32) -> bool {
        self.shards[shard(x, z)].lock().get(&(x, z)).is_some_and(|e| e.viewers > 0)
    }

    /// Store an entity in its chunk (spawned again when the chunk is next used).
    pub fn stash_entity(&self, name: &str, p: [i32; 3]) -> bool {
        let mut s = self.shards[shard(p[0] >> 4, p[2] >> 4)].lock();
        let Some(e) = s.get_mut(&(p[0] >> 4, p[2] >> 4)) else { return false };
        e.chunk.gen_entities.push((name.to_string(), p));
        e.dirty = true;
        true
    }

    /// Mob spawners in a chunk: (position, block entity).
    pub fn spawners(&self, x: i32, z: i32) -> Vec<([i32; 3], crate::nbt::Tag)> {
        let s = self.shards[shard(x, z)].lock();
        let Some(e) = s.get(&(x, z)) else { return Vec::new() };
        e.chunk.block_entities.iter().filter(|(_, t)| t.get("id").and_then(|v| v.as_str()) == Some("minecraft:mob_spawner")).map(|(p, t)| (*p, t.clone())).collect()
    }

    /// Remember that players changed this chunk (it is then never
    /// regenerated by a newer generator).
    pub fn mark_edited(&self, x: i32, z: i32) {
        if let Some(e) = self.shards[shard(x >> 4, z >> 4)].lock().get_mut(&(x >> 4, z >> 4)) {
            if !e.chunk.edited {
                e.chunk.edited = true;
                e.dirty = true;
            }
        }
    }

    pub fn surface_y(&self, x: i32, z: i32) -> Option<i32> {
        let s = self.shards[shard(x >> 4, z >> 4)].lock();
        let e = s.get(&(x >> 4, z >> 4))?;
        Some(e.chunk.heightmap[((z & 15) * 16 + (x & 15)) as usize] as i32)
    }

    /// Write modified chunks into the world store, then drop chunks nobody views.
    pub fn save(&self, store: &WorldStore, unload: bool) -> usize {
        let mut saved = 0;
        for sh in &self.shards {
            let mut s = sh.lock();
            for e in s.values_mut().filter(|e| e.dirty) {
                match store.save_chunk(self.dim, Kind::Region, e.chunk.x, e.chunk.z, &e.chunk.to_nbt(now_ms())) {
                    Ok(()) => {
                        e.dirty = false;
                        saved += 1;
                    }
                    Err(err) => tracing::error!("saving chunk {},{}: {err:#}", e.chunk.x, e.chunk.z),
                }
            }
            if unload {
                s.retain(|_, e| e.viewers > 0 || e.dirty);
            }
        }
        saved
    }
}

// ------------------------------------------------------------------ players

#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub pos: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

pub struct Player {
    pub eid: i32,
    pub uuid: uuid::Uuid,
    pub name: String,
    /// profile properties (skin `textures`): (name, value, signature)
    pub props: Vec<(String, String, Option<String>)>,
    pub pose: Mutex<Pose>,
    /// incremented on every movement, so viewers know when to resend
    pub moved: AtomicU64,
    pub gamemode: AtomicI32,
    /// selected hotbar slot (0-8)
    pub held: AtomicI32,
    pub inv: Mutex<Inventory>,
    /// health, hunger, air, fire
    pub vit: Mutex<survival::Vitals>,
    /// outgoing, already-encoded packets
    tx: mpsc::Sender<Arc<Vec<u8>>>,
    /// (center chunk x, z, view distance) of this player's client
    pub view: Mutex<(i32, i32, i32)>,
    /// server-side `deltaMovement` (knockback; decays like vanilla's)
    pub motion: Mutex<[f64; 3]>,
    /// sneaking / sprinting / gliding … (`hurt::F_*`)
    pub flags: std::sync::atomic::AtomicU8,
    /// a velocity packet was sent (anticheat must allow the knockback)
    pub knocked: std::sync::atomic::AtomicBool,
    /// `getKnownMovement().horizontalDistanceSqr()` of the last move
    pub known_speed_sq: Mutex<f64>,
    /// set when a newer login of the same player replaced this one
    pub kicked: std::sync::atomic::AtomicBool,
    /// actions requested from outside the session (plugins), run on its tick
    pub requests: Mutex<Vec<Request>>,
    /// the dimension the player is in (`Dim::index`)
    pub dim: std::sync::atomic::AtomicU8,
}

/// Something another part of the server asks a player's session to do.
#[derive(Clone, Debug)]
pub enum Request {
    Teleport([f64; 3], f32, f32),
    Gamemode(i32),
    Kick(String),
    /// run a command as this player (without the leading slash)
    Command(String),
    /// say this in chat as the player
    Chat(String),
    /// stop using the held item (shield disabled)
    StopUsingItem,
    /// the player died from this damage
    Died(Box<combat::damage::DamageSource>),
}

impl Player {
    /// `Player.giveExperiencePoints`
    pub fn give_xp(&self, n: i32) {
        self.vit.lock().give_xp(n);
    }

    /// Queue a packet; a client that cannot keep up is disconnected instead of
    /// letting its queue grow without bound.
    pub fn send(&self, pkt: Arc<Vec<u8>>) {
        if self.tx.try_send(pkt).is_err() {
            tracing::debug!("{}: outgoing queue full or closed", self.name);
        }
    }

    pub fn sees_chunk(&self, cx: i32, cz: i32) -> bool {
        let (x, z, d) = *self.view.lock();
        (cx - x).abs() <= d && (cz - z).abs() <= d
    }
}

/// Item stack in the network `Slot` format (no components).
pub fn write_slot(b: &mut Vec<u8>, s: Option<Stack>) {
    match s {
        Some(s) if s.count > 0 => {
            b.vi(s.count as i32).vi(s.item);
            crate::components::write_patch(b, s.comp);
        }
        _ => {
            b.vi(0);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TickKind {
    Fluid,
    Gravity,
    /// `FireBlock.tick`
    Fire,
    /// `BlockBehaviour.tick` of the block type with this id
    Block(u16),
}

#[derive(Default)]
struct Ticks {
    /// (game time, priority) -> ticks in scheduling order
    queue: BTreeMap<(u64, i8), Vec<([i32; 3], TickKind)>>,
    pending: HashSet<([i32; 3], TickKind)>,
}

/// Max scheduled block ticks run per game tick (the rest wait a tick).
const TICK_BUDGET: usize = 65_536;

pub fn is_gravity_block(name: &str) -> bool {
    name.ends_with("_concrete_powder") || matches!(name, "sand" | "red_sand" | "gravel" | "suspicious_sand" | "suspicious_gravel" | "anvil" | "chipped_anvil" | "damaged_anvil" | "dragon_egg")
}

pub struct Game {
    /// the dimension this game state simulates
    pub dim: Dim,
    pub world: World,
    pub data: Arc<GameData>,
    players: RwLock<HashMap<i32, Arc<Player>>>,
    entities: Mutex<HashMap<i32, Entity>>,
    ticks: Mutex<Ticks>,
    /// furnaces that may be burning or cooking
    pub(crate) furnaces: Mutex<HashSet<[i32; 3]>>,
    /// nether portal blocks (vanilla's portal POIs)
    pub(crate) portals: Mutex<HashSet<[i32; 3]>>,
    now: AtomicU64,
    /// added to `now` by /time set
    time_offset: AtomicU64,
    next_eid: AtomicI32,
    pub threshold: i32,
    pub weather: Mutex<weather::Weather>,
    /// block update state; held for the whole game tick and by any thread
    /// changing blocks, so updates never interleave
    pub(crate) level: parking_lot::ReentrantMutex<std::cell::RefCell<level::LevelState>>,
}

pub(crate) struct GW<'a>(pub(crate) &'a Game);

impl FluidWorld for GW<'_> {
    fn block(&self, p: [i32; 3]) -> Option<BlockState> {
        self.0.world.get_block(p[0], p[1], p[2])
    }
    fn set(&mut self, p: [i32; 3], b: BlockState, drops: bool) {
        self.0.change_block(p, b, drops, None);
    }
    fn schedule(&mut self, p: [i32; 3], delay: u32) {
        self.0.schedule(p, TickKind::Fluid, delay);
    }
}

impl Game {
    pub fn new(compression_threshold: i32, data: GameData) -> Self {
        Self::new_dim(compression_threshold, Arc::new(data), Dim::Overworld)
    }

    pub fn new_dim(compression_threshold: i32, data: Arc<GameData>, dim: Dim) -> Self {
        Self {
            dim,
            world: World::with_dim(dim),
            data,
            players: RwLock::new(HashMap::new()),
            entities: Mutex::new(HashMap::new()),
            ticks: Mutex::new(Ticks::default()),
            furnaces: Mutex::new(HashSet::new()),
            portals: Mutex::new(HashSet::new()),
            now: AtomicU64::new(0),
            time_offset: AtomicU64::new(0),
            next_eid: AtomicI32::new(1),
            threshold: compression_threshold,
            weather: Mutex::new(weather::Weather::default()),
            level: parking_lot::ReentrantMutex::new(std::cell::RefCell::new(level::LevelState::default())),
        }
    }

    pub fn encode(&self, name: &str, body: &[u8]) -> Arc<Vec<u8>> {
        Arc::new(crate::net::conn::encode(PacketIds::get().id("play", "clientbound", name), body, self.threshold))
    }

    pub fn players(&self) -> Vec<Arc<Player>> {
        self.players.read().values().cloned().collect()
    }

    pub fn find(&self, name: &str) -> Option<Arc<Player>> {
        self.players.read().values().find(|p| p.name.eq_ignore_ascii_case(name)).cloned()
    }

    pub fn count(&self) -> usize {
        self.players.read().len()
    }

    pub(crate) fn next_eid(&self) -> i32 {
        // entity ids are unique across all dimensions
        static NEXT: AtomicI32 = AtomicI32::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a player; returns None when the name/UUID is already online.
    #[allow(clippy::too_many_arguments)]
    pub fn join(&self, uuid: uuid::Uuid, name: String, props: Vec<(String, String, Option<String>)>, pose: Pose, gamemode: i32, view: i32, tx: mpsc::Sender<Arc<Vec<u8>>>) -> Option<Arc<Player>> {
        let mut m = self.players.write();
        if m.values().any(|p| p.uuid == uuid || p.name.eq_ignore_ascii_case(&name)) {
            return None;
        }
        let eid = self.next_eid();
        let cx = (pose.pos[0].floor() as i32) >> 4;
        let cz = (pose.pos[2].floor() as i32) >> 4;
        let p = Arc::new(Player {
            eid,
            uuid,
            name,
            props,
            pose: Mutex::new(pose),
            moved: AtomicU64::new(0),
            gamemode: AtomicI32::new(gamemode),
            held: AtomicI32::new(0),
            inv: Mutex::new(Inventory::default()),
            vit: Mutex::new(survival::Vitals::default()),
            tx,
            view: Mutex::new((cx, cz, view)),
            motion: Mutex::new([0.0; 3]),
            flags: std::sync::atomic::AtomicU8::new(0),
            knocked: std::sync::atomic::AtomicBool::new(false),
            known_speed_sq: Mutex::new(0.0),
            kicked: std::sync::atomic::AtomicBool::new(false),
            requests: Mutex::new(Vec::new()),
            dim: std::sync::atomic::AtomicU8::new(self.dim.index()),
        });
        m.insert(eid, p.clone());
        Some(p)
    }

    /// A player arriving from another dimension.
    pub fn adopt(&self, p: Arc<Player>) {
        p.dim.store(self.dim.index(), Ordering::Relaxed);
        self.players.write().insert(p.eid, p);
    }

    pub fn leave(&self, eid: i32) {
        self.players.write().remove(&eid);
    }

    /// A newer login takes over (RimeFleet hand-offs): the old session is
    /// told to end and leaves the game now. Returns false if nobody was online.
    pub fn take_over(&self, uuid: uuid::Uuid) -> bool {
        let mut m = self.players.write();
        let old: Vec<i32> = m.values().filter(|p| p.uuid == uuid).map(|p| p.eid).collect();
        for eid in &old {
            if let Some(p) = m.remove(eid) {
                p.kicked.store(true, Ordering::Relaxed);
            }
        }
        !old.is_empty()
    }

    /// Is this player (the session's own `Player`) still the one in the game?
    pub fn is_current(&self, eid: i32) -> bool {
        self.players.read().contains_key(&eid)
    }

    pub fn broadcast(&self, pkt: Arc<Vec<u8>>) {
        for p in self.players.read().values() {
            p.send(pkt.clone());
        }
    }

    pub(crate) fn broadcast_chunk(&self, c: (i32, i32), pkt: &Arc<Vec<u8>>) {
        for p in self.players.read().values() {
            if p.sees_chunk(c.0, c.1) {
                p.send(pkt.clone());
            }
        }
    }

    /// Send to every player whose view contains the block's chunk.
    pub fn broadcast_block(&self, x: i32, y: i32, z: i32, b: BlockState) {
        let mut body = Vec::new();
        body.pos(x, y, z).vi(b.0 as i32);
        self.broadcast_chunk((x >> 4, z >> 4), &self.encode("block_update", &body));
    }

    pub fn system_chat(&self, msg: &str) -> Arc<Vec<u8>> {
        let mut body = crate::nbt::Tag::text(msg).to_network_bytes();
        body.bool(false);
        self.encode("system_chat", &body)
    }

    pub fn is_replaceable(&self, b: BlockState) -> bool {
        b.is_air() || BlockRegistry::get().is_fluid(b) || self.data.block_in("replaceable", b.name())
    }

    // ------------------------------------------------------------ blocks

    /// Change a block with everything vanilla does around it: broadcast,
    /// optional loot drops of the old block, neighbour updates (fluids,
    /// falling blocks, blocks that lose their support).
    /// `change_block` for a change made by a player.
    pub fn player_change_block(&self, p: [i32; 3], b: BlockState, drops: bool, tool: Option<i32>) -> Option<BlockState> {
        self.world.mark_edited(p[0], p[2]);
        self.change_block(p, b, drops, tool)
    }

    pub fn change_block(&self, p: [i32; 3], b: BlockState, drops: bool, tool: Option<i32>) -> Option<BlockState> {
        let _g = self.level.lock();
        let old = self.world.get_block(p[0], p[1], p[2])?;
        if old == b {
            return Some(old);
        }
        if drops {
            self.drop_loot(p, old, tool);
        }
        self.set_block(p, b, level::flags::ALL);
        Some(old)
    }

    /// The block at `p` changed type: spill and drop the old block entity,
    /// create the new block's.
    pub(crate) fn block_entity_replaced(&self, p: [i32; 3], new: BlockState, spill: bool) {
        if let Some(mut t) = self.world.block_entity(p) {
            if let Some((sc, _)) = blockentity::screen_of(old_name(&t)).filter(|_| spill) {
                blockentity::unpack_loot(&mut t, sc.size(), &self.data);
                let centre = [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5];
                for s in blockentity::items(&t, sc.size()).into_iter().flatten() {
                    self.spawn_item(centre, s, [0.0, 0.1, 0.0], 10);
                }
            }
            self.world.set_block_entity(p, None, true);
            self.furnaces.lock().remove(&p);
            for pl in self.players() {
                let open = matches!(pl.inv.lock().window, inventory::Window::Container(_, _, q) if q == p);
                if open {
                    let id = pl.inv.lock().current_window();
                    let held = pl.held.load(Ordering::Relaxed) as i16;
                    let thrown = pl.inv.lock().close(&self.data, held);
                    let pos = pl.pose.lock().pos;
                    for t in thrown {
                        self.spawn_item([pos[0], pos[1] + 1.3, pos[2]], t, [0.0, 0.0, 0.0], 40);
                    }
                    let mut b = Vec::new();
                    b.vi(id as i32);
                    pl.send(self.encode("container_close", &b));
                    self.send_inventory(&pl);
                }
            }
        }
        if let Some(id) = blockentity::be_id(new.name()) {
            self.world.set_block_entity(p, Some(blockentity::new_tag(id, p)), true);
        }
    }

    /// A container's contents changed: store them and update everyone looking.
    pub fn container_changed(&self, p: [i32; 3], slots: &[Option<Stack>], except: i32) {
        let Some(mut t) = self.world.block_entity(p) else { return };
        blockentity::set_items(&mut t, slots);
        if let Some((blockentity::Screen::Furnace(kind), _)) = self.world.get_block(p[0], p[1], p[2]).and_then(|b| blockentity::screen_of(b.name())) {
            let mut f = blockentity::Furnace::read(&t);
            let before = blockentity::items(&self.world.block_entity(p).unwrap_or_else(|| t.clone()), 3)[0];
            if before.map(|s| s.item) != slots[0].map(|s| s.item) {
                f.input_changed(kind, &self.data);
                f.write(&mut t);
            }
            self.furnaces.lock().insert(p);
        }
        self.world.set_block_entity(p, Some(t), false);
        // BlockEntity.setChanged: comparators read the new contents
        self.update_neighbour_for_output_signal(p, self.bs(p));
        self.resync_viewers(p, except);
    }

    /// Send the stored contents to players (other than `except`) with `p` open.
    fn resync_viewers(&self, p: [i32; 3], except: i32) {
        let Some(t) = self.world.block_entity(p) else { return };
        for pl in self.players() {
            if pl.eid == except {
                continue;
            }
            let mut inv = pl.inv.lock();
            if let inventory::Window::Container(_, sc, q) = inv.window {
                if sc.transient() {
                    continue;
                }
                // a double chest is keyed by its first half
                let viewing = q == p || (matches!(sc, blockentity::Screen::Rows(6)) && block::container::parts(self, q).contains(&p));
                if viewing {
                    inv.ext = match self.container_slots(q) {
                        Some(s) if s.len() == sc.size() => s,
                        _ => blockentity::items(&t, sc.size()),
                    };
                    drop(inv);
                    self.send_inventory(&pl);
                }
            }
        }
    }

    /// `container_set_data` for the open window of one viewer.
    pub fn send_props(&self, pl: &Player, props: &[i32]) {
        let id = pl.inv.lock().current_window();
        for (prop, v) in props.iter().enumerate() {
            let mut b = Vec::new();
            b.vi(id as i32).i16(prop as i16).i16(*v as i16);
            pl.send(self.encode("container_set_data", &b));
        }
    }

    /// Furnace progress bars for one viewer.
    pub fn send_furnace_data(&self, pl: &Player, p: [i32; 3]) {
        let Some(t) = self.world.block_entity(p) else { return };
        let f = blockentity::Furnace::read(&t);
        let id = pl.inv.lock().current_window();
        for (prop, v) in [f.lit_time, f.lit_duration, f.cook_progress, f.cook_total].into_iter().enumerate() {
            let mut b = Vec::new();
            b.vi(id as i32).i16(prop as i16).i16(v.clamp(0, i16::MAX as i32) as i16);
            pl.send(self.encode("container_set_data", &b));
        }
    }

    pub fn watch_furnace(&self, p: [i32; 3]) {
        self.furnaces.lock().insert(p);
    }

    fn tick_furnaces(&self) {
        let list: Vec<[i32; 3]> = self.furnaces.lock().iter().copied().collect();
        for p in list {
            let (Some(b), Some(mut t)) = (self.world.get_block(p[0], p[1], p[2]), self.world.block_entity(p)) else {
                self.furnaces.lock().remove(&p);
                continue;
            };
            let Some((blockentity::Screen::Furnace(kind), _)) = blockentity::screen_of(b.name()) else {
                self.furnaces.lock().remove(&p);
                continue;
            };
            let mut f = blockentity::Furnace::read(&t);
            let (changed, lit_changed) = f.tick(kind, &self.data);
            let idle = !f.lit() && f.cook_progress == 0;
            f.write(&mut t);
            self.world.set_block_entity(p, Some(t), false);
            if lit_changed {
                if let Some(nb) = BlockRegistry::get().with(b, "lit", if f.lit() { "true" } else { "false" }) {
                    self.world.set_block(p[0], p[1], p[2], nb);
                    self.broadcast_block(p[0], p[1], p[2], nb);
                }
            }
            for pl in self.players() {
                let open = matches!(pl.inv.lock().window, inventory::Window::Container(_, _, q) if q == p);
                if open {
                    if changed {
                        self.resync_viewers(p, -1);
                    }
                    self.send_furnace_data(&pl, p);
                }
            }
            if idle {
                self.furnaces.lock().remove(&p);
            }
        }
    }

    pub fn schedule(&self, p: [i32; 3], kind: TickKind, delay: u32) {
        self.schedule_p(p, kind, delay, 0);
    }

    pub fn schedule_p(&self, p: [i32; 3], kind: TickKind, delay: u32, priority: i8) {
        let mut t = self.ticks.lock();
        if t.pending.insert((p, kind)) {
            let at = self.now.load(Ordering::Relaxed) + delay.max(1) as u64;
            t.queue.entry((at, priority)).or_default().push((p, kind));
        }
    }

    /// Simplified `canSurvive`: things that stand on or hang from a block.
    pub(crate) fn can_survive(&self, q: [i32; 3], b: BlockState) -> bool {
        let reg = BlockRegistry::get();
        let name = b.name();
        let at = |dx: i32, dy: i32, dz: i32| self.world.get_block(q[0] + dx, q[1] + dy, q[2] + dz);
        // two-block things: both halves must be present
        if let Some(half) = reg.property(b, "half") {
            if (name.ends_with("_door") || reg.property(b, "half").is_some() && !b.is_solid() && reg.property(b, "facing").is_none()) && matches!(half, "upper" | "lower") {
                let other = if half == "upper" { at(0, -1, 0) } else { at(0, 1, 0) };
                if other.is_some_and(|o| o.name() != name) {
                    return false;
                }
            }
        }
        if name.ends_with("_bed") {
            return true;
        }
        if name.contains("wall_torch") || name.ends_with("_wall_sign") {
            let off = match reg.property(b, "facing") {
                Some("north") => (0, 0, 1),
                Some("south") => (0, 0, -1),
                Some("east") => (-1, 0, 0),
                Some("west") => (1, 0, 0),
                _ => return true,
            };
            return at(off.0, off.1, off.2).is_none_or(|s| s.is_solid());
        }
        if b.is_solid() || reg.is_fluid(b) || reg.property(b, "waterlogged") == Some("true") {
            return true;
        }
        let free_standing = matches!(name, "cobweb" | "vine" | "ladder" | "glow_lichen" | "light" | "structure_void" | "barrier" | "scaffolding" | "sculk_vein" | "resin_clump" | "tripwire" | "fire" | "soul_fire" | "bubble_column" | "moss_carpet" | "pale_moss_carpet" | "hanging_roots" | "spore_blossom" | "cave_vines" | "cave_vines_plant" | "weeping_vines" | "weeping_vines_plant" | "pale_hanging_moss")
            || name.ends_with("_wall_banner")
            || name.ends_with("_button")
            || name.ends_with("lever")
            || name.ends_with("_hanging_sign")
            || name.ends_with("_pane")
            || name.ends_with("_bars")
            || name.ends_with("_chain")
            || name == "chain";
        if free_standing {
            return true;
        }
        // stands on the block below
        at(0, -1, 0).is_none_or(|s| s.is_solid() || s.name() == name || name == "sugar_cane" && s.name() == "sugar_cane")
    }

    /// Spawn the loot of a broken block.
    pub fn drop_loot(&self, p: [i32; 3], state: BlockState, tool: Option<i32>) {
        let drops = self.data.block_drops(state, tool, &mut rand::thread_rng());
        let mut rng = rand::thread_rng();
        for st in drops {
            let pos = [p[0] as f64 + 0.5 + rng.gen_range(-0.25..0.25), p[1] as f64 + 0.5 - 0.125 + rng.gen_range(-0.25..0.25), p[2] as f64 + 0.5 + rng.gen_range(-0.25..0.25)];
            let vel = [rng.gen_range(-0.1..0.1), 0.2, rng.gen_range(-0.1..0.1)];
            self.spawn_item(pos, st, vel, 10);
        }
    }

    // ------------------------------------------------------------ entities

    fn spawn_packets(&self, e: &Entity) -> Vec<Arc<Vec<u8>>> {
        if matches!(e.kind, EKind::Mob(_) | EKind::Projectile(_) | EKind::Cloud(_) | EKind::Lightning { .. }) {
            return self.mob_spawn_packets(e);
        }
        let regs = Registries::get();
        let (ty, data) = match &e.kind {
            EKind::Item { .. } => (regs.id("minecraft:entity_type", "minecraft:item").unwrap_or(0), 0),
            EKind::Falling { state } => (regs.id("minecraft:entity_type", "minecraft:falling_block").unwrap_or(0), state.0 as i32),
            EKind::Tnt { .. } => (regs.id("minecraft:entity_type", "minecraft:tnt").unwrap_or(0), 0),
            _ => unreachable!(),
        };
        let mut b = Vec::new();
        b.vi(e.eid).uuid(e.uuid).vi(ty).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]).u8(0).u8(0).u8(0).u8(0).vi(data);
        let mut out = vec![self.encode("add_entity", &b)];
        if let EKind::Tnt { fuse, .. } = e.kind {
            let mut m = Vec::new();
            m.vi(e.eid).u8(8).vi(1).vi(fuse).u8(0xFF);
            out.push(self.encode("set_entity_data", &m));
        }
        if let EKind::Item { stack, .. } = e.kind {
            out.push(self.item_meta(e.eid, stack));
        }
        out
    }

    pub(crate) fn item_meta(&self, eid: i32, stack: Stack) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid).u8(8).vi(7);
        write_slot(&mut b, Some(stack));
        b.u8(0xFF);
        self.encode("set_entity_data", &b)
    }

    pub(crate) fn remove_packet(&self, eids: &[i32]) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eids.len() as i32);
        for e in eids {
            b.vi(*e);
        }
        self.encode("remove_entities", &b)
    }

    pub fn spawn_item(&self, pos: [f64; 3], stack: Stack, vel: [f64; 3], pickup_delay: u32) {
        if stack.count == 0 || !Items::get().valid(stack.item) {
            return;
        }
        let e = Entity { eid: self.next_eid(), uuid: uuid::Uuid::new_v4(), kind: EKind::Item { stack, pickup_delay }, pos, vel, on_ground: false, age: 0, synced: pos };
        self.add_entity(e);
    }

    pub(crate) fn add_entity(&self, e: Entity) {
        let pkts = self.spawn_packets(&e);
        let c = e.chunk();
        self.entities.lock().insert(e.eid, e);
        for p in pkts {
            self.broadcast_chunk(c, &p);
        }
    }

    /// Spawn packets for all entities in a chunk (sent with the chunk).
    pub fn entity_spawns_in(&self, c: (i32, i32)) -> Vec<Arc<Vec<u8>>> {
        let list: Vec<Entity> = self.entities.lock().values().filter(|e| e.chunk() == c).cloned().collect();
        list.iter().flat_map(|e| self.spawn_packets(e)).collect()
    }

    pub fn entities_in(&self, c: (i32, i32)) -> Vec<i32> {
        self.entities.lock().values().filter(|e| e.chunk() == c).map(|e| e.eid).collect()
    }

    /// Full inventory to the client (join / resync).
    pub fn send_inventory(&self, p: &Player) {
        let mut inv = p.inv.lock();
        inv.state_id = inv.state_id.wrapping_add(1) & 0x7FFF;
        inv.dirty.clear();
        let mut b = Vec::new();
        b.vi(inv.current_window() as i32).vi(inv.state_id);
        let items = inv.window_contents();
        b.vi(items.len() as i32);
        for s in items {
            write_slot(&mut b, s);
        }
        write_slot(&mut b, inv.cursor);
        p.send(self.encode("container_set_content", &b));
    }

    /// Send changed inventory slots.
    pub fn flush_inventory(&self, p: &Player) {
        let mut inv = p.inv.lock();
        if inv.dirty.is_empty() {
            return;
        }
        inv.state_id = inv.state_id.wrapping_add(1) & 0x7FFF;
        let dirty = std::mem::take(&mut inv.dirty);
        let mut seen = HashSet::new();
        for (win, slot) in dirty {
            if !seen.insert((win, slot)) {
                continue;
            }
            let s = inv.sync_slot(win, slot);
            let mut b = Vec::new();
            b.vi(win as i32).vi(inv.state_id).i16(slot);
            write_slot(&mut b, s);
            p.send(self.encode("container_set_slot", &b));
        }
    }

    fn cell(&self, x: i32, y: i32, z: i32) -> Cell {
        match self.world.get_block(x, y, z) {
            None => Cell::Unknown,
            Some(b) if b.is_solid() => Cell::Solid,
            Some(b) if fluid_state(b).fluid() == Some(fluid::Fluid::Water) => Cell::Water,
            Some(_) => Cell::Open,
        }
    }

    // ------------------------------------------------------------ tick

    /// One game tick (runs on the tick thread).
    pub fn tick(&self) {
        let _level = self.level.lock();
        let now = self.now.fetch_add(1, Ordering::Relaxed) + 1;
        self.tick_furnaces();
        // freshly generated fluids start flowing (vanilla post processing)
        let fresh = std::mem::take(&mut *self.world.pending_fluid.lock());
        for p in fresh {
            if let Some(f) = self.world.get_block(p[0], p[1], p[2]).and_then(|b| fluid_state(b).fluid()) {
                self.schedule(p, TickKind::Fluid, fluid::tick_delay(f));
            }
        }
        // scheduled block ticks
        let due: Vec<([i32; 3], TickKind)> = {
            let mut t = self.ticks.lock();
            let mut due = Vec::new();
            while let Some((&key, _)) = t.queue.first_key_value() {
                if key.0 > now || due.len() >= TICK_BUDGET {
                    break;
                }
                let list = t.queue.remove(&key).unwrap_or_default();
                due.extend(list);
            }
            if due.len() > TICK_BUDGET {
                let rest = due.split_off(TICK_BUDGET);
                t.queue.entry((now + 1, i8::MIN)).or_default().extend(rest);
            }
            for d in &due {
                t.pending.remove(d);
            }
            due
        };
        self.lv(|l| {
            l.handling = true;
            l.running = due.iter().filter_map(|(p, k)| if let TickKind::Block(id) = k { Some((*p, *id)) } else { None }).collect();
        });
        // block ticks first, then fluid ticks (LevelTicks of blocks, then of fluids)
        let (block_due, other_due): (Vec<_>, Vec<_>) = due.into_iter().partition(|(_, k)| matches!(k, TickKind::Block(_)));
        for (p, kind) in block_due.into_iter().chain(other_due) {
            let Some(b) = self.world.get_block(p[0], p[1], p[2]) else { continue };
            match kind {
                TickKind::Block(id) => {
                    if level::block_id(b) == id {
                        block::tick(self, p, b);
                    }
                }
                TickKind::Fluid => {
                    if let Some(f) = fluid_state(b).fluid() {
                        Flow { w: &mut GW(self), fluid: f }.tick(p);
                    }
                }
                TickKind::Fire => {
                    if b.name() == "fire" {
                        self.fire_tick(p, b);
                    }
                }
                TickKind::Gravity => {
                    if is_gravity_block(b.name()) && p[1] > MIN_Y {
                        let below = self.world.get_block(p[0], p[1] - 1, p[2]);
                        if below.is_some_and(|x| self.is_replaceable(x) || x.name() == "fire") {
                            self.change_block(p, blocks::AIR, false, None);
                            let pos = [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5];
                            self.add_entity(Entity { eid: self.next_eid(), uuid: uuid::Uuid::new_v4(), kind: EKind::Falling { state: b }, pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos });
                        }
                    }
                }
            }
        }
        self.lv(|l| {
            l.running.clear();
            l.handling = false;
        });
        block::piston::tick_moving(self);
        block::container::tick_block_entities(self);
        self.tick_weather();
        self.tick_sleep();
        self.random_ticks();
        self.run_block_events();
        self.tick_entities();
        self.tick_mobs();
        self.tick_entity_inside();
        // light changed by block updates
        let relit = self.world.relight(256);
        if !relit.is_empty() {
            let players = self.players();
            for ((cx, cz), body) in relit {
                let pkt = self.encode("light_update", &body);
                for p in players.iter().filter(|p| p.sees_chunk(cx, cz)) {
                    p.send(pkt.clone());
                }
            }
        }
    }

    fn tick_entities(&self) {
        let players = self.players();
        let mut sends: Vec<((i32, i32), Arc<Vec<u8>>)> = Vec::new();
        let mut direct: Vec<(Arc<Player>, Arc<Vec<u8>>)> = Vec::new();
        let mut removed: Vec<(i32, (i32, i32))> = Vec::new();
        let mut landed: Vec<([i32; 3], BlockState)> = Vec::new();
        let mut inv_changed: Vec<Arc<Player>> = Vec::new();
        let mut booms: Vec<([f64; 3], Option<i32>)> = Vec::new();
        {
            let mut ents = self.entities.lock();
            for e in ents.values_mut() {
                if matches!(e.kind, EKind::Mob(_) | EKind::Projectile(_) | EKind::Cloud(_) | EKind::Lightning { .. } | EKind::Fangs { .. }) {
                    continue;
                }
                let old_chunk = e.chunk();
                if self.world.get_block(e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32).is_none() {
                    continue; // chunk unloaded: frozen
                }
                let below = self.world.get_block(e.pos[0].floor() as i32, (e.pos[1] - 0.5).floor() as i32, e.pos[2].floor() as i32).map(|b| b.name()).unwrap_or("stone");
                let friction = match below {
                    "ice" | "packed_ice" | "frosted_ice" => 0.98,
                    "blue_ice" => 0.989,
                    "slime_block" => 0.8,
                    _ => 0.6,
                };
                let ground = entity::physics(e, &|x, y, z| self.cell(x, y, z), friction);
                e.age += 1;
                match &mut e.kind {
                    EKind::Item { pickup_delay, .. } => {
                        *pickup_delay = pickup_delay.saturating_sub(1);
                        if e.age > 6000 || e.pos[1] < (MIN_Y - 64) as f64 {
                            removed.push((e.eid, old_chunk));
                            continue;
                        }
                    }
                    EKind::Mob(_) | EKind::Projectile(_) | EKind::Cloud(_) | EKind::Lightning { .. } | EKind::Fangs { .. } => {}
                    EKind::Tnt { fuse, owner } => {
                        *fuse -= 1;
                        if *fuse <= 0 {
                            booms.push((e.pos, *owner));
                            removed.push((e.eid, old_chunk));
                            continue;
                        }
                    }
                    EKind::Falling { state } => {
                        if ground || e.age > 600 {
                            landed.push(([e.pos[0].floor() as i32, e.pos[1].round() as i32, e.pos[2].floor() as i32], *state));
                            removed.push((e.eid, old_chunk));
                            continue;
                        }
                    }
                }
                let new_chunk = e.chunk();
                let moved = (0..3).any(|i| (e.pos[i] - e.synced[i]).abs() > 1.0 / 4096.0);
                if new_chunk != old_chunk {
                    // viewers that lose / gain sight of it
                    let spawn = self.spawn_packets(e);
                    let rm = self.remove_packet(&[e.eid]);
                    for p in &players {
                        let (a, b) = (p.sees_chunk(old_chunk.0, old_chunk.1), p.sees_chunk(new_chunk.0, new_chunk.1));
                        if a && !b {
                            direct.push((p.clone(), rm.clone()));
                        } else if !a && b {
                            for s in &spawn {
                                direct.push((p.clone(), s.clone()));
                            }
                        }
                    }
                }
                if moved {
                    e.synced = e.pos;
                    let mut b = Vec::new();
                    b.vi(e.eid).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]).f64(e.vel[0]).f64(e.vel[1]).f64(e.vel[2]).f32(0.0).f32(0.0).bool(e.on_ground);
                    sends.push((new_chunk, self.encode("entity_position_sync", &b)));
                }
            }
            // pickups
            for e in ents.values_mut() {
                let chunk = e.chunk();
                let EKind::Item { stack, pickup_delay } = &mut e.kind else { continue };
                if *pickup_delay > 0 || removed.iter().any(|r| r.0 == e.eid) {
                    continue;
                }
                for p in &players {
                    if p.gamemode.load(Ordering::Relaxed) == 3 {
                        continue;
                    }
                    let q = p.pose.lock().pos;
                    // player box (0.6 x 1.8) inflated by 1, 0.5, 1
                    let hit = (e.pos[0] - q[0]).abs() < 0.3 + 1.0 + 0.125 && e.pos[1] + 0.25 > q[1] - 0.5 && e.pos[1] < q[1] + 1.8 + 0.5 && (e.pos[2] - q[2]).abs() < 0.3 + 1.0 + 0.125;
                    if !hit {
                        continue;
                    }
                    let held = p.held.load(Ordering::Relaxed) as i16;
                    let left = p.inv.lock().add(*stack, held);
                    let taken = stack.count - left;
                    if taken == 0 {
                        continue;
                    }
                    let mut b = Vec::new();
                    b.vi(e.eid).vi(p.eid).vi(taken as i32);
                    sends.push((chunk, self.encode("take_item_entity", &b)));
                    inv_changed.push(p.clone());
                    if left == 0 {
                        removed.push((e.eid, chunk));
                        break;
                    }
                    stack.count = left;
                    sends.push((chunk, self.item_meta(e.eid, *stack)));
                }
            }
            for (eid, _) in &removed {
                ents.remove(eid);
            }
        }
        for (c, pkt) in sends {
            self.broadcast_chunk(c, &pkt);
        }
        for (p, pkt) in direct {
            p.send(pkt);
        }
        for (eid, c) in removed {
            self.broadcast_chunk(c, &self.remove_packet(&[eid]));
        }
        for p in inv_changed {
            self.flush_inventory(&p);
        }
        for (c, owner) in booms {
            // PrimedTnt.explode: power 4 at y + height / 16
            self.explode_full([c[0], c[1] + 0.98 / 16.0, c[2]], 4.0, false, owner, Some(false));
        }
        for (p, state) in landed {
            match self.world.get_block(p[0], p[1], p[2]) {
                Some(cur) if self.is_replaceable(cur) => {
                    let st = if state.name().ends_with("_concrete_powder") && fluid_state(cur).fluid() == Some(fluid::Fluid::Water) {
                        BlockRegistry::get().by_name(&state.name().replace("_powder", "")).map(|b| BlockState(b.default)).unwrap_or(state)
                    } else {
                        state
                    };
                    self.change_block(p, st, false, None);
                }
                _ => {
                    if let Some(item) = Items::get().id(state.name()) {
                        self.spawn_item([p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5], Stack::new(item, 1), [0.0; 3], 10);
                    }
                }
            }
        }
    }
}

const _: i32 = HEIGHT;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::blocks;

    #[test]
    fn set_block_invalidates_cache_and_marks_dirty() {
        let w = World::new();
        let g = Generator::new(1, false, 1);
        let dir = std::env::temp_dir().join(format!("rime-game-test-{}", std::process::id()));
        let store = WorldStore::open(&dir, crate::config::WorldFormat::Airworld, 3, true, 64).unwrap();
        w.ensure_loaded(&[(0, 0)], &store, &g);
        let p1 = w.chunk_packet(0, 0, &|c| vec![c.get(0, 100, 0).0 as u8]).unwrap();
        assert_eq!(w.set_block(1, 100, 1, blocks::GLASS), Some(blocks::AIR));
        let p2 = w.chunk_packet(0, 0, &|_| vec![9]).unwrap();
        assert!(!Arc::ptr_eq(&p1, &p2));
        assert_eq!(w.get_block(1, 100, 1), Some(blocks::GLASS));
        assert_eq!(w.save(&store, true), 1);
        assert_eq!(w.loaded(), 0);
        w.ensure_loaded(&[(0, 0)], &store, &g);
        assert_eq!(w.get_block(1, 100, 1), Some(blocks::GLASS));
        assert_eq!(w.set_block(1, 9999, 1, blocks::GLASS), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn block_changes_relight_the_3x3_chunks() {
        let w = World::new();
        let g = Generator::new(1, false, 1);
        let dir = std::env::temp_dir().join(format!("rime-light-test-{}", std::process::id()));
        let store = WorldStore::open(&dir, crate::config::WorldFormat::Airworld, 3, true, 64).unwrap();
        let around: Vec<(i32, i32)> = (-1..=1).flat_map(|x| (-1..=1).map(move |z| (x, z))).collect();
        w.ensure_loaded(&around, &store, &g);
        assert!(w.relight(1000).is_empty());
        // air -> cave_air does not change light; a torch does
        w.set_block(3, 200, 3, blocks::CAVE_AIR);
        assert!(w.relight(1000).is_empty());
        let torch = BlockRegistry::get().state("torch", &[]).unwrap();
        w.set_block(3, 200, 3, torch);
        let mut got: Vec<(i32, i32)> = w.relight(1000).into_iter().map(|(c, _)| c).collect();
        got.sort_unstable();
        assert_eq!(got, around);
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn old_name(t: &crate::nbt::Tag) -> &str {
    t.get("id").and_then(|v| v.as_str()).map(|s| s.trim_start_matches("minecraft:")).unwrap_or("")
}
