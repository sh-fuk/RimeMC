//! Live game state shared by all connections: loaded chunks and online players.
//!
//! Chunks live in 64 independently locked shards so connections on different
//! cores never contend. Each loaded chunk caches its encoded network packet,
//! so sending a chunk to N players costs one serialisation, not N.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use rayon::prelude::*;
use tokio::sync::mpsc;

use crate::net::codec::Out;
use crate::registry::{BlockState, PacketIds};
use crate::world::chunk::{Chunk, MIN_Y};
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
}

pub struct World {
    shards: Vec<Mutex<HashMap<(i32, i32), Entry>>>,
}

fn shard(x: i32, z: i32) -> usize {
    ((x as u32).wrapping_mul(0x9E37_79B9) ^ (z as u32).wrapping_mul(0x85EB_CA6B)) as usize % SHARDS
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl World {
    pub fn new() -> Self {
        Self { shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect() }
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
                let c = match store.load_chunk(Dim::Overworld, Kind::Region, x, z) {
                    Ok(Some(b)) => Chunk::from_nbt(&b).filter(|c| c.x == x && c.z == z),
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
        let generated = if to_gen.is_empty() { Vec::new() } else { generator.generate_batch(&to_gen) };
        let insert = |c: Chunk, dirty: bool| {
            let mut s = self.shards[shard(c.x, c.z)].lock();
            s.entry((c.x, c.z)).or_insert(Entry { chunk: c, dirty, viewers: 0, version: 0, packet: None });
        };
        for (_, c) in loaded {
            if let Some(c) = c {
                insert(c, false);
            }
        }
        for c in generated {
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

    pub fn get_block(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
        let s = self.shards[shard(x >> 4, z >> 4)].lock();
        let e = s.get(&(x >> 4, z >> 4))?;
        Some(e.chunk.get((x & 15) as usize, y, (z & 15) as usize))
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
        }
        Some(old)
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
                match store.save_chunk(Dim::Overworld, Kind::Region, e.chunk.x, e.chunk.z, &e.chunk.to_nbt(now_ms())) {
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
    pub pose: Mutex<Pose>,
    /// incremented on every movement, so viewers know when to resend
    pub moved: AtomicU64,
    pub gamemode: AtomicI32,
    /// outgoing, already-encoded packets
    tx: mpsc::Sender<Arc<Vec<u8>>>,
    /// (center chunk x, z, view distance) of this player's client
    pub view: Mutex<(i32, i32, i32)>,
}

impl Player {
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

pub struct Game {
    pub world: World,
    players: RwLock<HashMap<i32, Arc<Player>>>,
    next_eid: AtomicI32,
    pub threshold: i32,
}

impl Game {
    pub fn new(compression_threshold: i32) -> Self {
        Self { world: World::new(), players: RwLock::new(HashMap::new()), next_eid: AtomicI32::new(1), threshold: compression_threshold }
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

    /// Register a player; returns None when the name/UUID is already online.
    pub fn join(&self, uuid: uuid::Uuid, name: String, pose: Pose, gamemode: i32, view: i32, tx: mpsc::Sender<Arc<Vec<u8>>>) -> Option<Arc<Player>> {
        let mut m = self.players.write();
        if m.values().any(|p| p.uuid == uuid || p.name.eq_ignore_ascii_case(&name)) {
            return None;
        }
        let eid = self.next_eid.fetch_add(1, Ordering::Relaxed);
        let cx = (pose.pos[0].floor() as i32) >> 4;
        let cz = (pose.pos[2].floor() as i32) >> 4;
        let p = Arc::new(Player {
            eid,
            uuid,
            name,
            pose: Mutex::new(pose),
            moved: AtomicU64::new(0),
            gamemode: AtomicI32::new(gamemode),
            tx,
            view: Mutex::new((cx, cz, view)),
        });
        m.insert(eid, p.clone());
        Some(p)
    }

    pub fn leave(&self, eid: i32) {
        self.players.write().remove(&eid);
    }

    pub fn broadcast(&self, pkt: Arc<Vec<u8>>) {
        for p in self.players.read().values() {
            p.send(pkt.clone());
        }
    }

    /// Send to every player whose view contains the block's chunk.
    pub fn broadcast_block(&self, x: i32, y: i32, z: i32, b: BlockState) {
        let mut body = Vec::new();
        body.pos(x, y, z).vi(b.0 as i32);
        let pkt = self.encode("block_update", &body);
        for p in self.players.read().values() {
            if p.sees_chunk(x >> 4, z >> 4) {
                p.send(pkt.clone());
            }
        }
    }

    pub fn system_chat(&self, msg: &str) -> Arc<Vec<u8>> {
        let mut body = crate::nbt::Tag::text(msg).to_network_bytes();
        body.bool(false);
        self.encode("system_chat", &body)
    }
}

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
}
