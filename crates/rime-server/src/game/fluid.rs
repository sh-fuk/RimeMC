//! Water and lava flow, following vanilla's `FlowingFluid` algorithm
//! (source conversion, falling fluid, slope search toward the nearest hole,
//! drop-off per block, tick delays, lava/water interactions).

use crate::registry::{blocks, BlockRegistry, BlockState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fluid {
    Water,
    Lava,
}

/// Fluid state of a block: none, a source, or flowing with an amount 1..=8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FState {
    Empty,
    Source(Fluid),
    Flowing { fluid: Fluid, amount: u8, falling: bool },
}

impl FState {
    pub fn fluid(self) -> Option<Fluid> {
        match self {
            FState::Empty => None,
            FState::Source(f) | FState::Flowing { fluid: f, .. } => Some(f),
        }
    }
    pub fn amount(self) -> u8 {
        match self {
            FState::Empty => 0,
            FState::Source(_) => 8,
            FState::Flowing { amount, .. } => amount,
        }
    }
    pub fn is_source(self) -> bool {
        matches!(self, FState::Source(_))
    }
    pub fn falling(self) -> bool {
        matches!(self, FState::Flowing { falling: true, .. })
    }
    fn is_source_of(self, f: Fluid) -> bool {
        self == FState::Source(f)
    }

    /// The fluid block for this state (`level` property as vanilla's LiquidBlock).
    pub fn block(self) -> BlockState {
        let reg = BlockRegistry::get();
        let (name, level) = match self {
            FState::Empty => return blocks::AIR,
            FState::Source(f) => (f, 0),
            FState::Flowing { fluid, amount, falling } => (fluid, 8 - amount.min(8) as u32 + if falling { 8 } else { 0 }),
        };
        let base = if name == Fluid::Water { blocks::WATER } else { blocks::LAVA };
        reg.with(base, "level", &level.to_string()).unwrap_or(base)
    }
}

pub fn fluid_state(b: BlockState) -> FState {
    let reg = BlockRegistry::get();
    let name = b.name();
    let f = match name {
        "water" => Fluid::Water,
        "lava" => Fluid::Lava,
        "bubble_column" | "kelp" | "kelp_plant" | "seagrass" | "tall_seagrass" => return FState::Source(Fluid::Water),
        _ => {
            return if reg.property(b, "waterlogged") == Some("true") { FState::Source(Fluid::Water) } else { FState::Empty };
        }
    };
    let level: u8 = reg.property(b, "level").and_then(|l| l.parse().ok()).unwrap_or(0);
    match level {
        0 => FState::Source(f),
        1..=7 => FState::Flowing { fluid: f, amount: 8 - level, falling: false },
        _ => FState::Flowing { fluid: f, amount: 8, falling: true },
    }
}

pub fn tick_delay(f: Fluid) -> u32 {
    match f {
        Fluid::Water => 5,
        Fluid::Lava => 30, // overworld
    }
}

fn drop_off(f: Fluid) -> u8 {
    match f {
        Fluid::Water => 1,
        Fluid::Lava => 2,
    }
}

fn slope_find_distance(f: Fluid) -> u32 {
    match f {
        Fluid::Water => 4,
        Fluid::Lava => 2,
    }
}

/// Access to the world for the fluid simulation.
pub trait FluidWorld {
    fn block(&self, p: [i32; 3]) -> Option<BlockState>;
    /// Replace a block (with neighbour updates). `drops` = destroyed block drops its loot.
    fn set(&mut self, p: [i32; 3], b: BlockState, drops: bool);
    fn schedule(&mut self, p: [i32; 3], delay: u32);
}

/// Plane.HORIZONTAL order: north, east, south, west.
const HORIZONTAL: [[i32; 3]; 4] = [[0, 0, -1], [1, 0, 0], [0, 0, 1], [-1, 0, 0]];

fn add(p: [i32; 3], d: [i32; 3]) -> [i32; 3] {
    [p[0] + d[0], p[1] + d[1], p[2] + d[2]]
}

fn neg(d: [i32; 3]) -> [i32; 3] {
    [-d[0], -d[1], -d[2]]
}

/// Blocks that stop fluids even though players can walk through them.
fn blocks_fluid(name: &str) -> bool {
    name.ends_with("_door")
        || name.ends_with("_sign")
        || name.ends_with("_hanging_sign")
        || matches!(name, "ladder" | "sugar_cane" | "bubble_column" | "nether_portal" | "end_portal" | "end_gateway" | "structure_void" | "barrier")
}

/// Can a fluid flow into this block (destroying it if it is not air)?
fn can_hold_any(b: BlockState) -> bool {
    if b.is_air() {
        return true;
    }
    let reg = BlockRegistry::get();
    if reg.is_fluid(b) {
        return true;
    }
    if reg.property(b, "waterlogged").is_some() || blocks_fluid(b.name()) {
        return false; // fluid containers / fluid-tight blocks
    }
    !b.is_solid()
}

fn can_hold(b: BlockState, f: Fluid) -> bool {
    let _ = f;
    can_hold_any(b)
}

/// `FluidState.canBeReplacedWith` for the fluid currently at a position.
fn replaceable_by(cur: FState, by: Fluid, down: bool) -> bool {
    match cur {
        FState::Empty => true,
        st => match st.fluid() {
            Some(Fluid::Water) => down && by != Fluid::Water,
            Some(Fluid::Lava) => st.amount() >= 4 && by == Fluid::Water,
            None => true,
        },
    }
}

pub struct Flow<'a, W: FluidWorld> {
    pub w: &'a mut W,
    pub fluid: Fluid,
}

impl<W: FluidWorld> Flow<'_, W> {
    fn get(&self, p: [i32; 3]) -> BlockState {
        // unloaded chunks behave like solid walls: fluids never flow into them
        self.w.block(p).unwrap_or(blocks::BEDROCK)
    }

    fn new_liquid(&self, p: [i32; 3]) -> FState {
        let f = self.fluid;
        let mut max = 0;
        let mut sources = 0;
        for d in HORIZONTAL {
            let s = fluid_state(self.get(add(p, d)));
            if s.fluid() == Some(f) {
                if s.is_source() {
                    sources += 1;
                }
                max = max.max(s.amount());
            }
        }
        if sources >= 2 && f == Fluid::Water {
            let below = self.get(add(p, [0, -1, 0]));
            if below.is_solid() || fluid_state(below).is_source_of(f) {
                return FState::Source(f);
            }
        }
        let above = fluid_state(self.get(add(p, [0, 1, 0])));
        if above.fluid() == Some(f) {
            return FState::Flowing { fluid: f, amount: 8, falling: true };
        }
        let k = max as i32 - drop_off(f) as i32;
        if k <= 0 { FState::Empty } else { FState::Flowing { fluid: f, amount: k as u8, falling: false } }
    }

    fn is_water_hole(&self, below: [i32; 3]) -> bool {
        let b = self.get(below);
        fluid_state(b).fluid() == Some(self.fluid) || can_hold(b, self.fluid)
    }

    fn can_maybe_pass_through(&self, target: BlockState) -> bool {
        !fluid_state(target).is_source_of(self.fluid) && can_hold_any(target)
    }

    fn source_neighbors(&self, p: [i32; 3]) -> usize {
        HORIZONTAL.iter().filter(|d| fluid_state(self.get(add(p, **d))).is_source_of(self.fluid)).count()
    }

    fn slope_distance(&self, p: [i32; 3], depth: u32, from: [i32; 3]) -> u32 {
        let mut best = 1000;
        for d in HORIZONTAL {
            if d == from {
                continue;
            }
            let q = add(p, d);
            let b = self.get(q);
            if !fluid_state(b).is_source_of(self.fluid) && can_hold(b, self.fluid) {
                if self.is_water_hole(add(q, [0, -1, 0])) {
                    return depth;
                }
                if depth < slope_find_distance(self.fluid) {
                    best = best.min(self.slope_distance(q, depth + 1, neg(d)));
                }
            }
        }
        best
    }

    fn spread_to(&mut self, p: [i32; 3], down: bool, st: FState) {
        let cur = self.get(p);
        if self.fluid == Fluid::Lava && down && fluid_state(cur).fluid() == Some(Fluid::Water) {
            if BlockRegistry::get().is_fluid(cur) {
                self.w.set(p, blocks::STONE, false);
            }
            return;
        }
        let drops = !cur.is_air() && !BlockRegistry::get().is_fluid(cur) && self.fluid == Fluid::Water;
        self.w.set(p, st.block(), drops);
    }

    fn spread_to_sides(&mut self, p: [i32; 3], st: FState) {
        let mut amount = st.amount() as i32 - drop_off(self.fluid) as i32;
        if st.falling() {
            amount = 7;
        }
        if amount <= 0 {
            return;
        }
        let mut best = 1000;
        let mut targets: Vec<([i32; 3], FState)> = Vec::new();
        for d in HORIZONTAL {
            let q = add(p, d);
            let b = self.get(q);
            if !self.can_maybe_pass_through(b) {
                continue;
            }
            let nf = self.new_liquid(q);
            let Some(nfluid) = nf.fluid() else { continue };
            if !can_hold(b, nfluid) {
                continue;
            }
            let dist = if self.is_water_hole(add(q, [0, -1, 0])) { 0 } else { self.slope_distance(q, 1, neg(d)) };
            if dist < best {
                targets.clear();
            }
            if dist <= best {
                if replaceable_by(fluid_state(b), nfluid, false) {
                    targets.push((q, nf));
                }
                best = dist;
            }
        }
        for (q, nf) in targets {
            self.spread_to(q, false, nf);
        }
    }

    fn spread(&mut self, p: [i32; 3], st: FState) {
        if st == FState::Empty {
            return;
        }
        let below = add(p, [0, -1, 0]);
        let b = self.get(below);
        if self.can_maybe_pass_through(b) {
            let nf = self.new_liquid(below);
            if let Some(nfluid) = nf.fluid() {
                if replaceable_by(fluid_state(b), nfluid, true) && can_hold(b, nfluid) {
                    self.spread_to(below, true, nf);
                    if self.source_neighbors(p) >= 3 {
                        self.spread_to_sides(p, st);
                    }
                    return;
                }
            }
        }
        if st.is_source() || !self.is_water_hole(below) {
            self.spread_to_sides(p, st);
        }
    }

    /// Scheduled fluid tick at `p`.
    pub fn tick(&mut self, p: [i32; 3]) {
        let Some(b) = self.w.block(p) else { return };
        let mut st = fluid_state(b);
        if st.fluid() != Some(self.fluid) {
            return;
        }
        if !st.is_source() {
            let nf = self.new_liquid(p);
            if nf == FState::Empty {
                st = nf;
                self.w.set(p, blocks::AIR, false);
            } else if nf != st {
                st = nf;
                self.w.set(p, nf.block(), false);
                self.w.schedule(p, tick_delay(self.fluid));
            }
        }
        self.spread(p, st);
    }
}

/// Lava touching water hardens (`LiquidBlock.shouldSpreadLiquid`). Returns
/// false when the lava block was converted.
pub fn lava_interaction<W: FluidWorld>(w: &mut W, p: [i32; 3]) -> bool {
    let Some(b) = w.block(p) else { return true };
    let st = fluid_state(b);
    if st.fluid() != Some(Fluid::Lava) || !BlockRegistry::get().is_fluid(b) {
        return true;
    }
    // directions: DOWN, SOUTH, NORTH, EAST, WEST → neighbours above and around
    for d in [[0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]] {
        if let Some(n) = w.block(add(p, d)) {
            if fluid_state(n).fluid() == Some(Fluid::Water) {
                w.set(p, if st.is_source() { blocks::OBSIDIAN } else { blocks::COBBLESTONE }, false);
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};

    /// Test world: a stone floor at y=0, everything else air, with a tick queue.
    #[derive(Default)]
    struct W {
        blocks: HashMap<[i32; 3], BlockState>,
        queue: BTreeMap<u64, Vec<[i32; 3]>>,
        now: u64,
    }

    impl FluidWorld for W {
        fn block(&self, p: [i32; 3]) -> Option<BlockState> {
            if p[0].abs() > 20 || p[2].abs() > 20 {
                return None;
            }
            Some(*self.blocks.get(&p).unwrap_or(if p[1] <= 0 { &blocks::STONE } else { &blocks::AIR }))
        }
        fn set(&mut self, p: [i32; 3], b: BlockState, _drops: bool) {
            self.blocks.insert(p, b);
            // neighbour updates: fluids around re-evaluate
            for d in [[0, 0, 0], [1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]] {
                let q = add(p, d);
                if let Some(f) = self.block(q).and_then(|b| fluid_state(b).fluid()) {
                    if lava_interaction(self, q) {
                        self.schedule(q, tick_delay(f));
                    }
                }
            }
        }
        fn schedule(&mut self, p: [i32; 3], delay: u32) {
            let at = self.now + delay as u64;
            let v = self.queue.entry(at).or_default();
            if !v.contains(&p) {
                v.push(p);
            }
        }
    }

    impl W {
        fn run(&mut self, ticks: u64) {
            for _ in 0..ticks {
                self.now += 1;
                if let Some(list) = self.queue.remove(&self.now) {
                    for p in list {
                        if let Some(f) = self.block(p).and_then(|b| fluid_state(b).fluid()) {
                            Flow { w: self, fluid: f }.tick(p);
                        }
                    }
                }
            }
        }
        fn fs(&self, p: [i32; 3]) -> FState {
            fluid_state(self.block(p).unwrap())
        }
    }

    #[test]
    fn state_roundtrip() {
        for st in [FState::Source(Fluid::Water), FState::Flowing { fluid: Fluid::Water, amount: 3, falling: false }, FState::Flowing { fluid: Fluid::Lava, amount: 8, falling: true }] {
            assert_eq!(fluid_state(st.block()), st);
        }
        assert_eq!(fluid_state(blocks::WATER), FState::Source(Fluid::Water));
    }

    #[test]
    fn water_spreads_seven_blocks_on_flat_ground() {
        let mut w = W::default();
        w.set([0, 1, 0], blocks::WATER, false);
        w.run(200);
        assert_eq!(w.fs([1, 1, 0]).amount(), 7);
        assert_eq!(w.fs([7, 1, 0]).amount(), 1);
        assert_eq!(w.fs([8, 1, 0]), FState::Empty);
        assert_eq!(w.fs([3, 1, 4]).amount(), 1); // manhattan distance 7
        assert_eq!(w.fs([0, 2, 0]), FState::Empty);
        // remove the source: everything dries up
        w.set([0, 1, 0], blocks::AIR, false);
        w.run(200);
        assert!((-8..=8).all(|x| w.fs([x, 1, 0]) == FState::Empty));
    }

    #[test]
    fn water_falls_and_two_sources_make_a_third() {
        let mut w = W::default();
        w.set([0, 5, 0], blocks::WATER, false);
        w.run(100);
        assert!(w.fs([0, 3, 0]).falling());
        assert_eq!(w.fs([0, 1, 0]).fluid(), Some(Fluid::Water));
        let mut w = W::default();
        for x in [-1, 1] {
            w.set([x, 1, 5], blocks::WATER, false);
        }
        w.run(100);
        assert!(w.fs([0, 1, 5]).is_source(), "infinite water source");
    }

    #[test]
    fn water_seeks_the_nearest_hole() {
        let mut w = W::default();
        w.blocks.insert([3, 0, 0], blocks::AIR); // hole 3 blocks east
        w.set([0, 1, 0], blocks::WATER, false);
        w.run(12); // a few flow steps
        assert!(w.fs([1, 1, 0]).fluid().is_some());
        assert_eq!(w.fs([-1, 1, 0]), FState::Empty, "flows only toward the hole first");
    }

    #[test]
    fn lava_spreads_less_and_hardens_with_water() {
        let mut w = W::default();
        w.set([0, 1, 0], blocks::LAVA, false);
        w.run(400);
        assert_eq!(w.fs([3, 1, 0]).amount(), 2);
        assert_eq!(w.fs([4, 1, 0]), FState::Empty);
        w.set([0, 1, -1], blocks::WATER, false);
        assert_eq!(w.block([0, 1, 0]), Some(blocks::OBSIDIAN));
    }
}
