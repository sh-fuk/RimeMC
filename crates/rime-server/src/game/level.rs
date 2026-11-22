//! `Level.setBlock` as vanilla does it: the chunk side (block entities,
//! `affectNeighborsAfterRemoval`, `onPlace`), then client sync, neighbour
//! updates and shape updates, all queued through a port of
//! `CollectingNeighborUpdater` so the update order matches vanilla (which is
//! what redstone contraptions depend on). Also scheduled block ticks with
//! priorities, block events (`ServerLevel.blockEvent`) and the signal queries
//! of `SignalGetter`.

use std::cell::RefCell;
use std::collections::VecDeque;

use super::block;
use super::*;
use crate::registry::rstate;

/// `Block.UPDATE_*` flags.
pub mod flags {
    pub const NEIGHBORS: u32 = 1;
    pub const CLIENTS: u32 = 2;
    pub const INVISIBLE: u32 = 4;
    pub const KNOWN_SHAPE: u32 = 16;
    pub const SUPPRESS_DROPS: u32 = 32;
    pub const MOVED: u32 = 64;
    pub const SKIP_WIRE_SHAPE: u32 = 128;
    pub const SKIP_BE_SIDE_EFFECTS: u32 = 256;
    pub const SKIP_ON_PLACE: u32 = 512;
    pub const ALL: u32 = NEIGHBORS | CLIENTS;
}

/// Direction ordinals as in vanilla: down, up, north, south, west, east.
pub const DOWN: u8 = 0;
pub const UP: u8 = 1;
pub const NORTH: u8 = 2;
pub const SOUTH: u8 = 3;
pub const WEST: u8 = 4;
pub const EAST: u8 = 5;
pub const OFFSETS: [[i32; 3]; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];
/// `NeighborUpdater.UPDATE_ORDER`
pub const NEIGHBOR_ORDER: [u8; 6] = [WEST, EAST, DOWN, UP, NORTH, SOUTH];
/// `BlockBehaviour.UPDATE_SHAPE_ORDER`
pub const SHAPE_ORDER: [u8; 6] = [WEST, EAST, NORTH, SOUTH, DOWN, UP];
pub const HORIZONTAL: [u8; 4] = [NORTH, EAST, SOUTH, WEST];
pub const DIR_NAMES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

#[inline]
pub fn rel(p: [i32; 3], d: u8) -> [i32; 3] {
    let o = OFFSETS[d as usize];
    [p[0] + o[0], p[1] + o[1], p[2] + o[2]]
}

#[inline]
pub fn opp(d: u8) -> u8 {
    d ^ 1
}

pub fn dir_by_name(n: &str) -> Option<u8> {
    DIR_NAMES.iter().position(|x| *x == n).map(|i| i as u8)
}

/// `Direction.getClockWise` (horizontal)
pub fn cw(d: u8) -> u8 {
    match d {
        NORTH => EAST,
        EAST => SOUTH,
        SOUTH => WEST,
        _ => NORTH,
    }
}

/// `Direction.getCounterClockWise` (horizontal)
pub fn ccw(d: u8) -> u8 {
    match d {
        NORTH => WEST,
        WEST => SOUTH,
        SOUTH => EAST,
        _ => NORTH,
    }
}

pub fn axis(d: u8) -> u8 {
    match d {
        DOWN | UP => 1,
        NORTH | SOUTH => 2,
        _ => 0,
    }
}

/// `TickPriority`
pub mod prio {
    pub const EXTREMELY_HIGH: i8 = -3;
    pub const VERY_HIGH: i8 = -2;
    pub const HIGH: i8 = -1;
    pub const NORMAL: i8 = 0;
}

/// identity of a block type (the id of its default state)
pub fn block_id(s: BlockState) -> u16 {
    BlockRegistry::get().block(s).default
}

pub fn same_block(a: BlockState, b: BlockState) -> bool {
    block_id(a) == block_id(b)
}

enum Upd {
    Shape { dir: u8, nstate: BlockState, pos: [i32; 3], npos: [i32; 3], flags: u32, limit: i32 },
    Simple { pos: [i32; 3], src: BlockState },
    Full { state: BlockState, pos: [i32; 3], src: BlockState, moved: bool },
    Multi { pos: [i32; 3], src: BlockState, skip: Option<u8>, idx: usize },
}

enum Act {
    Shape { dir: u8, nstate: BlockState, pos: [i32; 3], npos: [i32; 3], flags: u32, limit: i32 },
    Neighbor { pos: [i32; 3], state: Option<BlockState>, src: BlockState, moved: bool },
}

impl Upd {
    /// `NeighborUpdates.runNext`: the next update to run, None when done.
    fn next(&mut self) -> Option<Act> {
        match self {
            Upd::Shape { dir, nstate, pos, npos, flags, limit } => {
                let a = Act::Shape { dir: *dir, nstate: *nstate, pos: *pos, npos: *npos, flags: *flags, limit: *limit };
                *self = Upd::Multi { pos: *pos, src: *nstate, skip: None, idx: 6 };
                Some(a)
            }
            Upd::Simple { pos, src } => {
                let a = Act::Neighbor { pos: *pos, state: None, src: *src, moved: false };
                *self = Upd::Multi { pos: *pos, src: *src, skip: None, idx: 6 };
                Some(a)
            }
            Upd::Full { state, pos, src, moved } => {
                let a = Act::Neighbor { pos: *pos, state: Some(*state), src: *src, moved: *moved };
                *self = Upd::Multi { pos: *pos, src: *src, skip: None, idx: 6 };
                Some(a)
            }
            Upd::Multi { pos, src, skip, idx } => {
                while *idx < 6 && Some(NEIGHBOR_ORDER[*idx]) == *skip {
                    *idx += 1;
                }
                if *idx >= 6 {
                    return None;
                }
                let d = NEIGHBOR_ORDER[*idx];
                *idx += 1;
                Some(Act::Neighbor { pos: rel(*pos, d), state: None, src: *src, moved: false })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockEvent {
    pub pos: [i32; 3],
    pub block: u16,
    pub b0: i32,
    pub b1: i32,
}

/// Mutable level state shared by all block updates (guarded by `Game::level`).
#[derive(Default)]
pub struct LevelState {
    stack: Vec<Upd>,
    added: Vec<Upd>,
    count: usize,
    /// `RedStoneWireBlock.shouldSignal` (false while a wire reads its neighbours)
    pub wire_silent: bool,
    /// `RedstoneTorchBlock.RECENT_TOGGLES`
    pub torch_toggles: Vec<([i32; 3], u64)>,
    pub events: VecDeque<BlockEvent>,
    /// block ticks being run this tick (`willTickThisTick`)
    pub running: HashSet<([i32; 3], u16)>,
    /// `ServerLevel.isHandlingTick`
    pub handling: bool,
    /// piston moving block entities
    pub moving: std::collections::HashMap<[i32; 3], crate::game::block::piston::Moving>,
    /// hopper cooldowns
    pub hoppers: std::collections::HashMap<[i32; 3], crate::game::block::container::HopperState>,
}

const MAX_CHAINED: usize = 1_000_000;

/// Blocks read for shape updates (fences, walls, stairs…).
impl crate::worldgen::feature::Getter for Game {
    fn get(&self, p: [i32; 3]) -> BlockState {
        self.world.get_block(p[0], p[1], p[2]).unwrap_or(blocks::VOID_AIR)
    }
}

impl Game {
    /// Block at `p` (void air outside loaded chunks / the world).
    #[inline]
    pub fn bs(&self, p: [i32; 3]) -> BlockState {
        self.world.get_block(p[0], p[1], p[2]).unwrap_or(blocks::VOID_AIR)
    }

    pub fn loaded(&self, p: [i32; 3]) -> bool {
        self.world.get_block(p[0], p[1], p[2]).is_some()
    }

    pub(crate) fn lv<R>(&self, f: impl FnOnce(&mut LevelState) -> R) -> R {
        let g = self.level.lock();
        let mut s = g.borrow_mut();
        f(&mut s)
    }

    /// `Level.setBlock(pos, state, flags)` (update limit 512).
    pub fn set_block(&self, p: [i32; 3], s: BlockState, flags: u32) -> bool {
        self.set_block_l(p, s, flags, 512)
    }

    /// `Level.setBlock(pos, state, flags, updateLimit)`
    pub fn set_block_l(&self, p: [i32; 3], s: BlockState, flags: u32, limit: i32) -> bool {
        let _g = self.level.lock();
        let Some(old) = self.world.set_block(p[0], p[1], p[2], s) else { return false };
        if old == s {
            return false;
        }
        let moved = flags & flags::MOVED != 0;
        let changed_block = !same_block(old, s);
        // LevelChunk.setBlockState
        if changed_block {
            if flags & flags::SKIP_BE_SIDE_EFFECTS == 0 {
                self.block_entity_replaced(p, s, !moved);
            } else {
                self.block_entity_replaced(p, s, false);
            }
            block::after_removal(self, p, old, moved);
        }
        if flags & flags::SKIP_ON_PLACE == 0 && self.bs(p) == s {
            block::on_place(self, p, s, old, moved);
        }
        // Level.setBlock
        if self.bs(p) != s {
            return true;
        }
        if flags & flags::CLIENTS != 0 {
            self.broadcast_block(p[0], p[1], p[2], s);
        }
        if flags & flags::NEIGHBORS != 0 {
            self.update_neighbors_at(p, old);
            if rstate::analog(s) {
                self.update_neighbour_for_output_signal(p, s);
            }
        }
        if flags & flags::KNOWN_SHAPE == 0 && limit > 0 {
            let f2 = flags & !(flags::SUPPRESS_DROPS | flags::NEIGHBORS);
            block::indirect_shapes(self, p, old, f2, limit - 1);
            for d in SHAPE_ORDER {
                self.neighbour_shape_changed(opp(d), rel(p, d), p, s, f2, limit - 1);
            }
            block::indirect_shapes(self, p, s, f2, limit - 1);
        }
        true
    }

    /// `Level.destroyBlock(pos, drop, null, limit)`
    pub fn destroy_block(&self, p: [i32; 3], drop: bool, limit: i32) -> bool {
        let _g = self.level.lock();
        let s = self.bs(p);
        if s.is_air() {
            return false;
        }
        if !matches!(s.name(), "fire" | "soul_fire") {
            self.level_event(2001, p, super::mobtick::state_id(s));
        }
        if drop {
            self.drop_loot(p, s, None);
        }
        self.set_block_l(p, fluid_legacy_block(s), flags::ALL, limit)
    }

    /// `Level.removeBlock(pos, moved)`
    pub fn remove_block(&self, p: [i32; 3], moved: bool) -> bool {
        let s = self.bs(p);
        self.set_block(p, fluid_legacy_block(s), flags::ALL | if moved { flags::MOVED } else { 0 })
    }

    // -------------------------------------------------------- neighbour updates

    /// `Level.updateNeighborsAt(pos, block)`
    pub fn update_neighbors_at(&self, p: [i32; 3], src: BlockState) {
        self.add_and_run(Upd::Multi { pos: p, src, skip: None, idx: 0 });
    }

    /// `Level.updateNeighborsAtExceptFromFacing`
    pub fn update_neighbors_except(&self, p: [i32; 3], src: BlockState, skip: u8) {
        self.add_and_run(Upd::Multi { pos: p, src, skip: Some(skip), idx: 0 });
    }

    /// `Level.neighborChanged(pos, block, orientation)`
    pub fn neighbor_changed(&self, p: [i32; 3], src: BlockState) {
        self.add_and_run(Upd::Simple { pos: p, src });
    }

    /// `Level.neighborChanged(state, pos, block, orientation, moved)`
    pub fn neighbor_changed_full(&self, state: BlockState, p: [i32; 3], src: BlockState, moved: bool) {
        self.add_and_run(Upd::Full { state, pos: p, src, moved });
    }

    /// `Level.neighbourShapeChanged(dir, pos, neighborPos, neighborState, flags, limit)`
    pub fn neighbour_shape_changed(&self, dir: u8, p: [i32; 3], np: [i32; 3], ns: BlockState, flags: u32, limit: i32) {
        self.add_and_run(Upd::Shape { dir, nstate: ns, pos: p, npos: np, flags, limit });
    }

    /// `Level.updateNeighbourForOutputSignal`: comparators reading `p`.
    pub fn update_neighbour_for_output_signal(&self, p: [i32; 3], src: BlockState) {
        for d in HORIZONTAL_ORDER {
            let mut q = rel(p, d);
            if !self.loaded(q) {
                continue;
            }
            let mut s = self.bs(q);
            if block::class(s) == block::Class::Comparator {
                self.neighbor_changed_full(s, q, src, false);
            } else if rstate::conductor(s) {
                q = rel(q, d);
                s = self.bs(q);
                if block::class(s) == block::Class::Comparator {
                    self.neighbor_changed_full(s, q, src, false);
                }
            }
        }
    }

    fn add_and_run(&self, u: Upd) {
        let g = self.level.lock();
        let run = {
            let mut st = g.borrow_mut();
            let running = st.count > 0;
            let over = st.count >= MAX_CHAINED;
            st.count += 1;
            if over {
                false
            } else if running {
                st.added.push(u);
                false
            } else {
                st.stack.push(u);
                true
            }
        };
        if run {
            self.run_updates(&g);
        }
    }

    fn run_updates(&self, g: &parking_lot::ReentrantMutexGuard<RefCell<LevelState>>) {
        loop {
            let act = {
                let mut st = g.borrow_mut();
                let added = std::mem::take(&mut st.added);
                st.stack.extend(added.into_iter().rev());
                loop {
                    let Some(top) = st.stack.last_mut() else { break None };
                    match top.next() {
                        Some(a) => break Some(a),
                        None => {
                            st.stack.pop();
                        }
                    }
                }
            };
            let Some(act) = act else { break };
            match act {
                Act::Neighbor { pos, state, src, moved } => {
                    if !self.loaded(pos) {
                        continue;
                    }
                    let s = state.unwrap_or_else(|| self.bs(pos));
                    block::neighbor_changed(self, pos, s, src, moved);
                }
                Act::Shape { dir, nstate, pos, npos, flags: f, limit } => {
                    if !self.loaded(pos) {
                        continue;
                    }
                    let old = self.bs(pos);
                    if f & flags::SKIP_WIRE_SHAPE == 0 || block::class(old) != block::Class::Wire {
                        let new = block::update_shape(self, pos, old, dir, npos, nstate);
                        self.update_or_destroy(old, new, pos, f, limit);
                    }
                }
            }
        }
        let mut st = g.borrow_mut();
        st.count = 0;
        st.stack.clear();
        st.added.clear();
    }

    /// `Block.updateOrDestroy`
    pub fn update_or_destroy(&self, old: BlockState, new: BlockState, p: [i32; 3], f: u32, limit: i32) {
        if new != old {
            if new.is_air() {
                self.destroy_block(p, f & flags::SUPPRESS_DROPS == 0, limit);
            } else {
                self.set_block_l(p, new, f & !flags::SUPPRESS_DROPS, limit);
            }
        }
    }

    // -------------------------------------------------------- block ticks

    /// `Level.scheduleTick(pos, block, delay, priority)` (ignored when one is pending)
    pub fn schedule_tick(&self, p: [i32; 3], s: BlockState, delay: u32, priority: i8) {
        self.schedule_p(p, TickKind::Block(block_id(s)), delay, priority);
    }

    /// `LevelTicks.hasScheduledTick`
    pub fn has_scheduled(&self, p: [i32; 3], s: BlockState) -> bool {
        self.ticks.lock().pending.contains(&(p, TickKind::Block(block_id(s))))
    }

    /// `LevelTicks.willTickThisTick`
    pub fn will_tick_this_tick(&self, p: [i32; 3], s: BlockState) -> bool {
        self.lv(|l| l.running.contains(&(p, block_id(s))))
    }

    // -------------------------------------------------------- block events

    /// `Level.blockEvent`: run at the end of the tick, sent to clients when
    /// `triggerEvent` returns true.
    pub fn block_event(&self, p: [i32; 3], s: BlockState, b0: i32, b1: i32) {
        let e = BlockEvent { pos: p, block: block_id(s), b0, b1 };
        self.lv(|l| {
            if !l.events.contains(&e) {
                l.events.push_back(e);
            }
        });
    }

    /// `ServerLevel.runBlockEvents`
    pub fn run_block_events(&self) {
        let _g = self.level.lock();
        loop {
            let Some(e) = self.lv(|l| l.events.pop_front()) else { break };
            let s = self.bs(e.pos);
            if block_id(s) != e.block {
                continue;
            }
            if block::trigger_event(self, e.pos, s, e.b0, e.b1) {
                let block = Registries::get().id("minecraft:block", &format!("minecraft:{}", s.name())).unwrap_or(0);
                let mut b = Vec::new();
                b.pos(e.pos[0], e.pos[1], e.pos[2]).u8(e.b0 as u8).u8(e.b1 as u8).vi(block);
                let pkt = self.encode("block_event", &b);
                let c = [e.pos[0] as f64 + 0.5, e.pos[1] as f64 + 0.5, e.pos[2] as f64 + 0.5];
                for pl in self.players() {
                    let q = pl.pose.lock().pos;
                    let d = (q[0] - c[0]).powi(2) + (q[1] - c[1]).powi(2) + (q[2] - c[2]).powi(2);
                    if d < 64.0 * 64.0 {
                        pl.send(pkt.clone());
                    }
                }
            }
        }
    }

    // -------------------------------------------------------- signals

    /// `SignalGetter.getSignal(pos, dir)`: `dir` points from the asking block to `p`.
    pub fn get_signal(&self, p: [i32; 3], dir: u8) -> u8 {
        let s = self.bs(p);
        let i = block::signal(self, p, s, dir);
        if rstate::conductor(s) {
            i.max(self.direct_signal_to(p))
        } else {
            i
        }
    }

    /// `SignalGetter.getDirectSignal`
    pub fn direct_signal(&self, p: [i32; 3], dir: u8) -> u8 {
        let s = self.bs(p);
        block::direct_signal(self, p, s, dir)
    }

    /// `SignalGetter.getDirectSignalTo`
    pub fn direct_signal_to(&self, p: [i32; 3]) -> u8 {
        let mut i = 0;
        for d in 0..6u8 {
            i = i.max(self.direct_signal(rel(p, d), d));
            if i >= 15 {
                return i;
            }
        }
        i
    }

    pub fn has_signal(&self, p: [i32; 3], dir: u8) -> bool {
        self.get_signal(p, dir) > 0
    }

    /// `SignalGetter.hasNeighborSignal`
    pub fn has_neighbor_signal(&self, p: [i32; 3]) -> bool {
        (0..6u8).any(|d| self.get_signal(rel(p, d), d) > 0)
    }

    /// `SignalGetter.getBestNeighborSignal`
    pub fn best_neighbor_signal(&self, p: [i32; 3]) -> u8 {
        let mut i = 0;
        for d in 0..6u8 {
            i = i.max(self.get_signal(rel(p, d), d));
            if i >= 15 {
                return 15;
            }
        }
        i
    }

    /// `SignalGetter.getControlInputSignal`
    pub fn control_input_signal(&self, p: [i32; 3], dir: u8, diodes_only: bool) -> u8 {
        let s = self.bs(p);
        if diodes_only {
            return if block::is_diode(s) { self.direct_signal(p, dir) } else { 0 };
        }
        match block::class(s) {
            block::Class::PoweredBlock => 15,
            block::Class::Wire => s.prop("power").and_then(|v| v.parse().ok()).unwrap_or(0),
            _ if rstate::signal_source(s) => self.direct_signal(p, dir),
            _ => 0,
        }
    }
}

/// `Direction.Plane.HORIZONTAL` iteration order (north, east, south, west)
pub const HORIZONTAL_ORDER: [u8; 4] = [NORTH, EAST, SOUTH, WEST];

/// `FluidState.createLegacyBlock` of what is left when `s` is removed.
pub fn fluid_legacy_block(s: BlockState) -> BlockState {
    use crate::registry::vstate;
    match vstate::fluid(s) {
        1 if s.name() != "water" => blocks::WATER,
        3 if s.name() != "lava" => BlockState::by_name("lava"),
        _ => blocks::AIR,
    }
}

/// Java `HashSet<BlockPos>` iteration order for a small set (default
/// capacity 16, `Vec3i.hashCode`, `HashMap.hash` spreading); ties keep
/// insertion order.
pub fn java_hashset_order(ps: &[[i32; 3]]) -> Vec<[i32; 3]> {
    let mut uniq: Vec<[i32; 3]> = Vec::new();
    for p in ps {
        if !uniq.contains(p) {
            uniq.push(*p);
        }
    }
    let cap: u32 = if uniq.len() > 12 { 32 } else { 16 };
    let bucket = |p: &[i32; 3]| {
        let h = (p[1].wrapping_add(p[2].wrapping_mul(31))).wrapping_mul(31).wrapping_add(p[0]) as u32;
        (h ^ (h >> 16)) & (cap - 1)
    };
    let mut idx: Vec<usize> = (0..uniq.len()).collect();
    idx.sort_by_key(|&i| bucket(&uniq[i]));
    idx.into_iter().map(|i| uniq[i]).collect()
}
