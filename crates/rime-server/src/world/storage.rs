//! World storage + RAM world.
//!
//! Regions are always held in memory as [`AirRegion`]s (zstd-compressed chunk
//! blobs), regardless of the on-disk format; the format only matters when a
//! region is read from or written to disk. That makes the RAM world cheap: a
//! region in RAM costs roughly its compressed size.
//!
//! * **RAM world on**: at startup every region file is loaded into memory (up
//!   to `max_memory_mb`; anything beyond stays on disk and is paged in on
//!   demand). All reads/writes hit RAM. Dirty regions are written to disk every
//!   `save_interval_seconds`, on `./rime sync` and on shutdown.
//! * **RAM world off**: regions are cached with LRU eviction under the same
//!   budget, and dirty regions are written on the same schedule.
//!
//! Writes are snapshot-based: the region is cloned under a short lock and
//! written in parallel on all cores without blocking the game. Each disk write
//! is atomic (temp file + fsync + rename).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use airworld::mca::{self, McaChunk};
use airworld::{chunk_index, region_coord, AirRegion};
use parking_lot::{Mutex, RwLock};
use rayon::prelude::*;

use crate::config::WorldFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dim {
    Overworld,
    Nether,
    End,
}

impl Dim {
    fn dir(self) -> &'static str {
        match self {
            Dim::Overworld => "",
            Dim::Nether => "DIM-1",
            Dim::End => "DIM1",
        }
    }
    pub const ALL: [Dim; 3] = [Dim::Overworld, Dim::Nether, Dim::End];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Region,
    Entities,
    Poi,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Region => "region",
            Kind::Entities => "entities",
            Kind::Poi => "poi",
        }
    }
    pub const ALL: [Kind; 3] = [Kind::Region, Kind::Entities, Kind::Poi];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RegionKey {
    pub dim: Dim,
    pub kind: Kind,
    pub x: i32,
    pub z: i32,
}

struct Entry {
    region: AirRegion,
    dirty: bool,
    bytes: u64,
    last_used: u64,
}

#[derive(Debug, Default, Clone)]
pub struct FlushReport {
    pub regions_written: usize,
    pub failures: usize,
    pub millis: u128,
}

/// Chunk source of a RimeFleet node: the router owns the world.
pub trait RemoteChunks: Send + Sync {
    /// The router's copy: `Some(None)` = not generated yet, `None` = the
    /// router could not be asked (use the local copy).
    fn load(&self, dim: Dim, cx: i32, cz: i32) -> Option<Option<Vec<u8>>>;
    fn save(&self, dim: Dim, cx: i32, cz: i32, nbt: &[u8]);
}

pub struct WorldStore {
    /// RimeFleet node: chunks come from / go to the router
    remote: std::sync::OnceLock<std::sync::Arc<dyn RemoteChunks>>,
    root: PathBuf,
    format: WorldFormat,
    level: i32,
    ram_mode: bool,
    budget: u64,
    entries: RwLock<HashMap<RegionKey, Arc<Mutex<Entry>>>>,
    mem_used: AtomicU64,
    clock: AtomicU64,
    /// serialises flushes (periodic vs `./rime sync` vs shutdown)
    flush_lock: Mutex<()>,
}

fn region_bytes(r: &AirRegion) -> u64 {
    r.iter().map(|(_, s)| s.compressed.len() as u64 + 32).sum::<u64>() + 256
}

impl WorldStore {
    pub fn open(root: &Path, format: WorldFormat, level: i32, ram_mode: bool, budget_mb: u64) -> anyhow::Result<Self> {
        fs::create_dir_all(root)?;
        let store = Self {
            remote: std::sync::OnceLock::new(),
            root: root.to_path_buf(),
            format,
            level,
            ram_mode,
            budget: budget_mb.saturating_mul(1024 * 1024),
            entries: RwLock::new(HashMap::new()),
            mem_used: AtomicU64::new(0),
            clock: AtomicU64::new(0),
            flush_lock: Mutex::new(()),
        };
        store.check_format_mismatch()?;
        Ok(store)
    }

    fn ext(&self) -> &'static str {
        match self.format {
            WorldFormat::Airworld => "air",
            WorldFormat::Vanilla => "mca",
        }
    }

    /// Refuse to start on a world stored in the *other* format instead of
    /// silently generating new terrain over it.
    fn check_format_mismatch(&self) -> anyhow::Result<()> {
        let other = if self.format == WorldFormat::Airworld { "mca" } else { "air" };
        let dir = self.root.join("region");
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().ends_with(&format!(".{other}")) {
                    anyhow::bail!(
                        "world '{}' contains .{other} region files but world.format is '{}'. Convert it with `airworld-convert` or change world.format.",
                        self.root.display(),
                        self.ext()
                    );
                }
            }
        }
        Ok(())
    }

    fn path_of(&self, k: &RegionKey) -> PathBuf {
        let mut p = self.root.clone();
        if !k.dim.dir().is_empty() {
            p.push(k.dim.dir());
        }
        p.push(k.kind.dir());
        p.push(format!("r.{}.{}.{}", k.x, k.z, self.ext()));
        p
    }

    fn read_from_disk(&self, k: &RegionKey) -> anyhow::Result<AirRegion> {
        let path = self.path_of(k);
        if !path.exists() {
            return Ok(AirRegion::new(k.x, k.z, self.level));
        }
        match self.format {
            WorldFormat::Airworld => {
                let (r, damaged) = AirRegion::read_file_expect(&path, k.x, k.z)?;
                for (i, e) in damaged {
                    tracing::error!("{}: chunk {i} is damaged and will be regenerated: {e}", path.display());
                }
                Ok(r)
            }
            WorldFormat::Vanilla => {
                let (chunks, damaged) = mca::read_mca(&path, k.x, k.z)?;
                for (i, e) in damaged {
                    tracing::error!("{}: chunk {i} is damaged and will be regenerated: {e}", path.display());
                }
                let mut r = AirRegion::new(k.x, k.z, self.level);
                for c in chunks {
                    r.put_raw(c.index, &c.raw, c.timestamp)?;
                }
                Ok(r)
            }
        }
    }

    fn write_to_disk(&self, k: &RegionKey, r: &AirRegion) -> anyhow::Result<()> {
        let path = self.path_of(k);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        match self.format {
            WorldFormat::Airworld => r.write_file(&path)?,
            WorldFormat::Vanilla => {
                let mut chunks = Vec::with_capacity(r.chunk_count());
                for (i, s) in r.iter() {
                    chunks.push(McaChunk { index: i, timestamp: s.timestamp, raw: s.decompress(i)? });
                }
                mca::write_mca(&path, k.x, k.z, &chunks)?;
            }
        }
        Ok(())
    }

    fn entry(&self, k: RegionKey) -> anyhow::Result<Arc<Mutex<Entry>>> {
        let now = self.clock.fetch_add(1, Ordering::Relaxed);
        if let Some(e) = self.entries.read().get(&k) {
            e.lock().last_used = now;
            return Ok(e.clone());
        }
        // Load outside the map lock so other regions are not blocked by disk I/O.
        let region = self.read_from_disk(&k)?;
        let bytes = region_bytes(&region);
        let mut map = self.entries.write();
        let e = map
            .entry(k)
            .or_insert_with(|| {
                self.mem_used.fetch_add(bytes, Ordering::Relaxed);
                Arc::new(Mutex::new(Entry { region, dirty: false, bytes, last_used: now }))
            })
            .clone();
        drop(map);
        self.evict_if_needed();
        Ok(e)
    }

    /// Raw chunk NBT, or `None` if never generated.
    pub fn set_remote(&self, r: std::sync::Arc<dyn RemoteChunks>) {
        let _ = self.remote.set(r);
    }

    pub fn load_chunk(&self, dim: Dim, kind: Kind, cx: i32, cz: i32) -> anyhow::Result<Option<Vec<u8>>> {
        if kind == Kind::Region {
            if let Some(r) = self.remote.get() {
                if let Some(v) = r.load(dim, cx, cz) {
                    return Ok(v);
                }
            }
        }
        let k = RegionKey { dim, kind, x: region_coord(cx), z: region_coord(cz) };
        let e = self.entry(k)?;
        let g = e.lock();
        Ok(g.region.get_raw(chunk_index(cx, cz))?)
    }

    pub fn has_chunk(&self, dim: Dim, cx: i32, cz: i32) -> bool {
        let k = RegionKey { dim, kind: Kind::Region, x: region_coord(cx), z: region_coord(cz) };
        self.entry(k).map(|e| e.lock().region.slot(chunk_index(cx, cz)).is_some()).unwrap_or(false)
    }

    /// Store raw chunk NBT (compression happens here, outside of any global lock).
    pub fn save_chunk(&self, dim: Dim, kind: Kind, cx: i32, cz: i32, nbt: &[u8]) -> anyhow::Result<()> {
        if kind == Kind::Region {
            if let Some(r) = self.remote.get() {
                r.save(dim, cx, cz, nbt);
            }
        }
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(0);
        let slot = airworld::ChunkSlot::from_raw(nbt, ts, self.level)?;
        let k = RegionKey { dim, kind, x: region_coord(cx), z: region_coord(cz) };
        let e = self.entry(k)?;
        let mut g = e.lock();
        g.region.put_slot(chunk_index(cx, cz), slot);
        g.dirty = true;
        let nb = region_bytes(&g.region);
        let old = std::mem::replace(&mut g.bytes, nb);
        drop(g);
        if nb >= old {
            self.mem_used.fetch_add(nb - old, Ordering::Relaxed);
        } else {
            self.mem_used.fetch_sub(old - nb, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Drop clean, least-recently-used regions until we fit the budget.
    /// Dirty regions are never evicted (they would lose data).
    fn evict_if_needed(&self) {
        if self.mem_used.load(Ordering::Relaxed) <= self.budget {
            return;
        }
        let mut map = self.entries.write();
        let mut clean: Vec<(RegionKey, u64, u64)> = map
            .iter()
            .filter_map(|(k, e)| e.try_lock().filter(|g| !g.dirty).map(|g| (*k, g.last_used, g.bytes)))
            .collect();
        clean.sort_by_key(|(_, used, _)| *used);
        for (k, _, bytes) in clean {
            if self.mem_used.load(Ordering::Relaxed) <= self.budget {
                break;
            }
            // keep the entry if someone grabbed it in the meantime
            if map.get(&k).is_some_and(|e| Arc::strong_count(e) == 1) {
                map.remove(&k);
                self.mem_used.fetch_sub(bytes, Ordering::Relaxed);
            }
        }
    }

    /// RAM world: copy the whole world (or as much as fits) into RAM. Parallel.
    pub fn preload(&self) -> anyhow::Result<(usize, u64)> {
        if !self.ram_mode {
            return Ok((0, 0));
        }
        let mut keys = Vec::new();
        for dim in Dim::ALL {
            for kind in Kind::ALL {
                let mut dir = self.root.clone();
                if !dim.dir().is_empty() {
                    dir.push(dim.dir());
                }
                dir.push(kind.dir());
                let Ok(rd) = fs::read_dir(&dir) else { continue };
                for e in rd.flatten() {
                    if let Some((x, z)) = mca::parse_region_name(&e.file_name().to_string_lossy(), self.ext()) {
                        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                        keys.push((RegionKey { dim, kind, x, z }, size));
                    }
                }
            }
        }
        // Closest-to-spawn first, so a partial RAM world holds the busiest area.
        keys.sort_by_key(|(k, _)| (k.x as i64).pow(2) + (k.z as i64).pow(2));
        let mut chosen = Vec::new();
        let mut est = 0u64;
        for (k, size) in keys {
            // compressed AirWorld ≈ file size; mca is roughly 1.5× larger than zstd
            let s = if self.format == WorldFormat::Vanilla { size * 2 / 3 } else { size };
            if est + s > self.budget {
                break;
            }
            est += s;
            chosen.push(k);
        }
        let loaded: Vec<anyhow::Result<()>> = chosen.par_iter().map(|k| self.entry(*k).map(|_| ())).collect();
        let n = loaded.iter().filter(|r| r.is_ok()).count();
        for r in loaded.into_iter().filter_map(|r| r.err()) {
            tracing::error!("RAM world preload: {r:#}");
        }
        Ok((n, self.mem_used.load(Ordering::Relaxed)))
    }

    pub fn memory_used(&self) -> u64 {
        self.mem_used.load(Ordering::Relaxed)
    }

    pub fn dirty_regions(&self) -> usize {
        self.entries.read().values().filter(|e| e.lock().dirty).count()
    }

    /// Write every dirty region to disk (in parallel). Safe to call from any thread.
    pub fn flush(&self) -> FlushReport {
        let _g = self.flush_lock.lock();
        let t = Instant::now();
        let snapshot: Vec<(RegionKey, Arc<Mutex<Entry>>, AirRegion)> = self
            .entries
            .read()
            .iter()
            .filter_map(|(k, e)| {
                let mut g = e.lock();
                if g.dirty {
                    g.dirty = false;
                    Some((*k, e.clone(), g.region.clone()))
                } else {
                    None
                }
            })
            .collect();
        let failures: usize = snapshot
            .par_iter()
            .map(|(k, e, r)| match self.write_to_disk(k, r) {
                Ok(()) => 0,
                Err(err) => {
                    tracing::error!("failed to save {}: {err:#}", self.path_of(k).display());
                    e.lock().dirty = true; // retry next time
                    1
                }
            })
            .sum();
        FlushReport { regions_written: snapshot.len() - failures, failures, millis: t.elapsed().as_millis() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("rime-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn save_flush_reload_both_formats() {
        for fmt in [WorldFormat::Airworld, WorldFormat::Vanilla] {
            let root = tmp(&format!("{fmt:?}"));
            {
                let s = WorldStore::open(&root, fmt, 3, true, 64).unwrap();
                s.save_chunk(Dim::Overworld, Kind::Region, -1, 40, b"chunk-a").unwrap();
                s.save_chunk(Dim::Nether, Kind::Region, 5, 5, b"chunk-b").unwrap();
                assert_eq!(s.dirty_regions(), 2);
                let r = s.flush();
                assert_eq!(r.regions_written, 2);
                assert_eq!(s.dirty_regions(), 0);
            }
            let s = WorldStore::open(&root, fmt, 3, true, 64).unwrap();
            let (n, _) = s.preload().unwrap();
            assert_eq!(n, 2);
            assert_eq!(s.load_chunk(Dim::Overworld, Kind::Region, -1, 40).unwrap().unwrap(), b"chunk-a");
            assert_eq!(s.load_chunk(Dim::Nether, Kind::Region, 5, 5).unwrap().unwrap(), b"chunk-b");
            assert!(s.load_chunk(Dim::End, Kind::Region, 5, 5).unwrap().is_none());
            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn eviction_never_drops_dirty_data() {
        let root = tmp("evict");
        let s = WorldStore::open(&root, WorldFormat::Airworld, 3, false, 0).unwrap(); // 0 MB budget
        for i in 0..10 {
            s.save_chunk(Dim::Overworld, Kind::Region, i * 32, 0, &vec![i as u8; 1000]).unwrap();
        }
        for i in 0..10 {
            assert_eq!(s.load_chunk(Dim::Overworld, Kind::Region, i * 32, 0).unwrap().unwrap(), vec![i as u8; 1000]);
        }
        s.flush();
        for i in 0..10 {
            assert_eq!(s.load_chunk(Dim::Overworld, Kind::Region, i * 32, 0).unwrap().unwrap(), vec![i as u8; 1000]);
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refuses_wrong_format() {
        let root = tmp("mismatch");
        fs::create_dir_all(root.join("region")).unwrap();
        fs::write(root.join("region/r.0.0.mca"), b"").unwrap();
        assert!(WorldStore::open(&root, WorldFormat::Airworld, 3, true, 64).is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
