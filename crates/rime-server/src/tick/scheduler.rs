//! Parallel, region-sharded tick scheduler.
//!
//! The world is divided into cells (the same cells RimeFleet distributes).
//! Each cell owns its entities exclusively, so all cells are ticked in
//! parallel on every core with **no locks** in the hot path. Entities that
//! leave their cell are collected and moved after the parallel phase, which
//! keeps results independent of thread scheduling (deterministic).
//!
//! Redstone/block ticks are *not* sharded here: they are run by a single
//! sequential pass (and in a fleet, by a single node), preserving vanilla
//! update order.

use std::collections::HashMap;
use std::time::Instant;

use rayon::prelude::*;

use super::leveltick::{chunk_interval, ticks_to_run, MobFlags};
use crate::config::LevelTickCfg;

#[allow(dead_code)] // Item/Other are spawned by gameplay code
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityKind {
    Mob,
    Item,
    Other,
}

#[derive(Clone, Debug)]
pub struct Entity {
    pub id: u32,
    pub kind: EntityKind,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub on_ground: bool,
    pub age: u64,
    pub flags: MobFlags,
}

impl Entity {
    /// Vanilla-style free-fall integration for `n` ticks (gravity 0.08, drag 0.98).
    fn physics(&mut self, n: u32) {
        for _ in 0..n {
            if self.on_ground {
                self.vel[1] = 0.0;
            } else {
                self.vel[1] = (self.vel[1] - 0.08) * 0.98;
            }
            for i in 0..3 {
                self.pos[i] += self.vel[i];
            }
            self.vel[0] *= 0.91;
            self.vel[2] *= 0.91;
            if self.pos[1] < -128.0 {
                self.pos[1] = -128.0; // void floor; real removal is done by the world
                self.on_ground = true;
            }
        }
        self.age += n as u64;
    }

    pub fn chunk(&self) -> (i32, i32) {
        ((self.pos[0].floor() as i32) >> 4, (self.pos[2].floor() as i32) >> 4)
    }
}

#[derive(Default)]
pub struct Cell {
    pub entities: Vec<Entity>,
    /// last measured tick cost of this cell in milliseconds
    pub last_ms: f64,
    pub redstone_active: bool,
}

#[derive(Default, Debug, Clone)]
pub struct TickStats {
    pub tick: u64,
    pub mspt: f64,
    pub entities: usize,
    pub mob_updates: u64,
    pub throttled_chunks: usize,
}

pub struct Scheduler {
    pub cells: HashMap<(i32, i32), Cell>,
    pub cell_shift: u32,
    pub tick: u64,
}

impl Scheduler {
    pub fn new(cell_shift: u32) -> Self {
        Self { cells: HashMap::new(), cell_shift, tick: 0 }
    }

    pub fn cell_of(&self, chunk: (i32, i32)) -> (i32, i32) {
        (chunk.0 >> self.cell_shift, chunk.1 >> self.cell_shift)
    }

    pub fn spawn(&mut self, e: Entity) {
        let c = self.cell_of(e.chunk());
        self.cells.entry(c).or_default().entities.push(e);
    }

    pub fn entity_count(&self) -> usize {
        self.cells.values().map(|c| c.entities.len()).sum()
    }

    /// Run one game tick over all cells in parallel.
    pub fn tick(&mut self, lt: &LevelTickCfg) -> TickStats {
        let start = Instant::now();
        self.tick += 1;
        let tick = self.tick;
        let shift = self.cell_shift;

        let results: Vec<((i32, i32), Vec<Entity>, u64, usize)> = self
            .cells
            .par_iter_mut()
            .map(|(&key, cell)| {
                let t0 = Instant::now();
                // mobs per chunk → Level Tick interval per chunk
                let mut per_chunk: HashMap<(i32, i32), u32> = HashMap::new();
                for e in cell.entities.iter().filter(|e| e.kind == EntityKind::Mob) {
                    *per_chunk.entry(e.chunk()).or_default() += 1;
                }
                let throttled = per_chunk.values().filter(|n| chunk_interval(lt, **n) > 1).count();
                let mut updates = 0u64;
                for e in cell.entities.iter_mut() {
                    let n = match e.kind {
                        EntityKind::Mob => {
                            let interval = chunk_interval(lt, per_chunk.get(&e.chunk()).copied().unwrap_or(0));
                            ticks_to_run(interval, tick, e.id, &e.flags)
                        }
                        _ => 1,
                    };
                    if n > 0 {
                        if e.kind == EntityKind::Mob {
                            updates += 1;
                        }
                        e.physics(n);
                    }
                }
                // move out entities that left this cell
                let mut leaving = Vec::new();
                let mut i = 0;
                while i < cell.entities.len() {
                    let ch = cell.entities[i].chunk();
                    if (ch.0 >> shift, ch.1 >> shift) != key {
                        leaving.push(cell.entities.swap_remove(i));
                    } else {
                        i += 1;
                    }
                }
                cell.last_ms = t0.elapsed().as_secs_f64() * 1000.0;
                (key, leaving, updates, throttled)
            })
            .collect();

        let mut stats = TickStats { tick, ..Default::default() };
        // Deterministic re-insertion order (sorted by entity id).
        let mut movers: Vec<Entity> = Vec::new();
        for (_, leaving, updates, throttled) in results {
            stats.mob_updates += updates;
            stats.throttled_chunks += throttled;
            movers.extend(leaving);
        }
        movers.sort_by_key(|e| e.id);
        for e in movers {
            self.spawn(e);
        }
        self.cells.retain(|_, c| !c.entities.is_empty() || c.redstone_active);
        stats.entities = self.entity_count();
        stats.mspt = start.elapsed().as_secs_f64() * 1000.0;
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mob(id: u32, x: f64, z: f64) -> Entity {
        Entity { id, kind: EntityKind::Mob, pos: [x, 64.0, z], vel: [0.0; 3], on_ground: true, age: 0, flags: MobFlags::default() }
    }

    #[test]
    fn crowded_chunk_is_throttled_without_losing_time() {
        let lt = LevelTickCfg::default();
        let mut s = Scheduler::new(5);
        for i in 0..100 {
            s.spawn(mob(i, 1.0, 1.0)); // 100 mobs in chunk (0,0) → interval 4
        }
        s.spawn(mob(1000, 100.0, 100.0)); // a lone mob elsewhere
        let mut updates = 0;
        for _ in 0..8 {
            updates += s.tick(&lt).mob_updates;
        }
        // crowded: 100 mobs × 8 ticks / 4 = 200 updates; lone mob: 8
        assert_eq!(updates, 208);
        for c in s.cells.values() {
            for e in &c.entities {
                assert_eq!(e.age, 8, "entity {} lost time", e.id);
            }
        }
    }

    #[test]
    fn entities_move_between_cells() {
        let lt = LevelTickCfg::default();
        let mut s = Scheduler::new(0);
        let mut e = mob(1, 15.5, 0.0);
        e.vel = [1.0, 0.0, 0.0];
        e.on_ground = false;
        s.spawn(e);
        s.tick(&lt);
        assert!(s.cells.contains_key(&(1, 0)));
        assert_eq!(s.entity_count(), 1);
    }
}
