//! Vanilla pathfinding: `PathType` classification (dumped per block state
//! from `WalkNodeEvaluator.getPathTypeFromState`), the walk / fly / swim /
//! amphibious node evaluators, `PathFinder` (A*), `Path` and `PathNavigation`.

use std::collections::{BinaryHeap, HashMap};
use std::io::Read;
use std::sync::OnceLock;

use crate::anticheat::combat::Aabb;
use crate::game::physics::{self, Blocks};
use crate::registry::{blocks, BlockState};
use crate::world::chunk::MIN_Y;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PathType {
    Blocked,
    Open,
    Walkable,
    WalkableDoor,
    Trapdoor,
    PowderSnow,
    OnTopOfPowderSnow,
    Fence,
    Lava,
    Water,
    WaterBorder,
    Rail,
    UnpassableRail,
    FireInNeighbor,
    Fire,
    DamagingInNeighbor,
    Damaging,
    DoorOpen,
    DoorWoodClosed,
    DoorIronClosed,
    Breach,
    Leaves,
    StickyHoney,
    Cocoa,
    DamageCautious,
    OnTopOfTrapdoor,
    BigMobsCloseToDanger,
}

pub const PATH_TYPES: usize = 27;

impl PathType {
    fn from_u8(v: u8) -> PathType {
        if (v as usize) < PATH_TYPES {
            // SAFETY: repr(u8) with contiguous discriminants 0..27
            unsafe { std::mem::transmute::<u8, PathType>(v) }
        } else {
            PathType::Blocked
        }
    }

    /// `PathType.getMalus`
    pub fn malus(self) -> f32 {
        use PathType::*;
        match self {
            Blocked | PowderSnow | Fence | Lava | UnpassableRail | Damaging | DoorWoodClosed | DoorIronClosed | Leaves => -1.0,
            Water | WaterBorder | FireInNeighbor | DamagingInNeighbor | StickyHoney => 8.0,
            Fire => 16.0,
            Breach | BigMobsCloseToDanger => 4.0,
            _ => 0.0,
        }
    }
}

struct StateInfo {
    of_state: Vec<u16>,
}

fn table() -> &'static StateInfo {
    static T: OnceLock<StateInfo> = OnceLock::new();
    T.get_or_init(|| {
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(&include_bytes!("../../../assets/26.1/pathtypes.bin.gz")[..]).read_to_end(&mut raw).expect("path types");
        StateInfo { of_state: raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect() }
    })
}

fn raw(s: BlockState) -> u16 {
    table().of_state.get(s.0 as usize).copied().unwrap_or(0)
}

/// `WalkNodeEvaluator.getPathTypeFromState`
pub fn state_type(s: BlockState) -> PathType {
    static T: OnceLock<Vec<u8>> = OnceLock::new();
    let t = T.get_or_init(|| {
        let n = crate::registry::BlockRegistry::get().state_count();
        (0..n).map(|i| compute_type(BlockState(i as u16)) as u8).collect()
    });
    PathType::from_u8(t.get(s.0 as usize).copied().unwrap_or(0))
}

fn compute_type(s: BlockState) -> PathType {
    let n = s.name();
    if s.is_air() {
        return PathType::Open;
    }
    if n.ends_with("_trapdoor") || n == "lily_pad" || n == "big_dripleaf" {
        return PathType::Trapdoor;
    }
    match n {
        "powder_snow" => return PathType::PowderSnow,
        "cactus" | "sweet_berry_bush" => return PathType::Damaging,
        "honey_block" => return PathType::StickyHoney,
        "cocoa" => return PathType::Cocoa,
        "wither_rose" | "pointed_dripstone" => return PathType::DamageCautious,
        _ => {}
    }
    if n == "lava" {
        return PathType::Lava;
    }
    // isBurningBlock
    if matches!(n, "fire" | "soul_fire" | "magma_block" | "lava_cauldron") || matches!(n, "campfire" | "soul_campfire") && s.prop("lit") == Some("true") {
        return PathType::Fire;
    }
    if n.ends_with("_door") {
        if s.prop("open") == Some("true") {
            return PathType::DoorOpen;
        }
        return if n == "iron_door" { PathType::DoorIronClosed } else { PathType::DoorWoodClosed };
    }
    if matches!(n, "rail" | "powered_rail" | "detector_rail" | "activator_rail") {
        return PathType::Rail;
    }
    if n.ends_with("_leaves") {
        return PathType::Leaves;
    }
    let fence_gate_closed = n.ends_with("_fence_gate") && s.prop("open") != Some("true");
    if n.ends_with("_fence") || n.ends_with("_wall") || fence_gate_closed {
        return PathType::Fence;
    }
    if !pathfindable(s, 0) {
        return PathType::Blocked;
    }
    if crate::registry::vstate::is_water(s) { PathType::Water } else { PathType::Open }
}

/// `BlockState.isSolidRender`
pub fn solid_render(s: BlockState) -> bool {
    raw(s) >> 5 & 1 != 0
}

/// legacy `BlockState.isSolid`
pub fn legacy_solid(s: BlockState) -> bool {
    raw(s) >> 5 & 2 != 0
}

/// `isPathfindable(LAND / WATER / AIR)`
pub fn pathfindable(s: BlockState, kind: u8) -> bool {
    raw(s) >> 5 & (4 << kind) != 0
}

pub fn blocks_motion(s: BlockState) -> bool {
    raw(s) >> 5 & 32 != 0
}

/// Block friction, speed and jump factors.
pub fn block_factors(name: &str) -> (f32, f32, f32) {
    static T: OnceLock<HashMap<&'static str, (f32, f32, f32)>> = OnceLock::new();
    let t = T.get_or_init(|| {
        include_str!("../../../assets/26.1/blockphys.tsv")
            .lines()
            .filter_map(|l| {
                let f: Vec<&'static str> = l.split('\t').collect();
                Some((f[0], (f.get(1)?.parse().ok()?, f.get(2)?.parse().ok()?, f.get(3)?.parse().ok()?)))
            })
            .collect()
    });
    t.get(name).copied().unwrap_or((0.6, 1.0, 1.0))
}

// ---------------------------------------------------------------- mob view

/// What the evaluators need to know about the mob.
#[derive(Clone, Debug)]
pub struct PathMob {
    pub pos: [f64; 3],
    pub width: f64,
    pub height: f64,
    pub on_ground: bool,
    pub in_water: bool,
    pub max_up_step: f32,
    pub max_fall: i32,
    pub malus: [f32; PATH_TYPES],
    pub can_open_doors: bool,
    pub can_pass_doors: bool,
    pub can_float: bool,
    pub can_walk_over_fences: bool,
}

impl PathMob {
    pub fn malus(&self, t: PathType) -> f32 {
        let m = self.malus[t as usize];
        if m.is_nan() { t.malus() } else { m }
    }

    fn bbox(&self) -> Aabb {
        Aabb::entity(self.pos, self.width, self.height)
    }

    fn block_pos(&self) -> [i32; 3] {
        [self.pos[0].floor() as i32, self.pos[1].floor() as i32, self.pos[2].floor() as i32]
    }
}

pub fn default_malus() -> [f32; PATH_TYPES] {
    [f32::NAN; PATH_TYPES]
}

/// Block lookups with a per-search cache (`PathNavigationRegion`).
struct Region<'a> {
    w: &'a dyn Fn(i32, i32, i32) -> Option<BlockState>,
    cache: HashMap<[i32; 3], BlockState>,
}

impl Region<'_> {
    fn get(&mut self, x: i32, y: i32, z: i32) -> BlockState {
        if let Some(s) = self.cache.get(&[x, y, z]) {
            return *s;
        }
        let s = (self.w)(x, y, z).unwrap_or(blocks::AIR);
        self.cache.insert([x, y, z], s);
        s
    }
}

struct RegionBlocks<'r, 'a>(std::cell::RefCell<&'r mut Region<'a>>);

impl Blocks for RegionBlocks<'_, '_> {
    fn block(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
        Some(self.0.borrow_mut().get(x, y, z))
    }
}

// ---------------------------------------------------------------- nodes

#[derive(Clone, Debug)]
pub struct Node {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub ty: PathType,
    pub malus: f32,
    g: f32,
    h: f32,
    f: f32,
    walked: f32,
    came_from: Option<usize>,
    closed: bool,
    in_open: bool,
}

impl Node {
    fn new(x: i32, y: i32, z: i32) -> Node {
        Node { x, y, z, ty: PathType::Blocked, malus: 0.0, g: 0.0, h: 0.0, f: 0.0, walked: 0.0, came_from: None, closed: false, in_open: false }
    }

    fn dist(&self, o: [i32; 3]) -> f32 {
        let (dx, dy, dz) = ((o[0] - self.x) as f32, (o[1] - self.y) as f32, (o[2] - self.z) as f32);
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    fn manhattan(&self, o: [i32; 3]) -> f32 {
        ((o[0] - self.x).abs() + (o[1] - self.y).abs() + (o[2] - self.z).abs()) as f32
    }

    pub fn pos(&self) -> [i32; 3] {
        [self.x, self.y, self.z]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvalKind {
    Walk,
    Fly,
    Swim { breach: bool },
    Amphibious { shallow: bool },
}

const H_DIRS: [[i32; 2]; 4] = [[0, 1], [-1, 0], [0, -1], [1, 0]]; // S W N E (2D data value order)

struct Eval<'a, 'r> {
    kind: EvalKind,
    mob: PathMob,
    level: &'r mut Region<'a>,
    nodes: Vec<Node>,
    index: HashMap<[i32; 3], usize>,
    types: HashMap<[i32; 3], PathType>,
    collisions: HashMap<[i64; 6], bool>,
    ew: i32,
    eh: i32,
    ed: i32,
}

impl Eval<'_, '_> {
    fn node(&mut self, x: i32, y: i32, z: i32) -> usize {
        if let Some(&i) = self.index.get(&[x, y, z]) {
            return i;
        }
        self.nodes.push(Node::new(x, y, z));
        self.index.insert([x, y, z], self.nodes.len() - 1);
        self.nodes.len() - 1
    }

    fn st(&mut self, x: i32, y: i32, z: i32) -> PathType {
        state_type(self.level.get(x, y, z))
    }

    fn amphibious(&self) -> bool {
        matches!(self.kind, EvalKind::Amphibious { .. })
    }

    fn can_float(&self) -> bool {
        self.mob.can_float
    }

    /// `WalkNodeEvaluator.getPathTypeStatic` (+ the amphibious / fly variants)
    fn path_type(&mut self, x: i32, y: i32, z: i32) -> PathType {
        let t = self.st(x, y, z);
        match self.kind {
            EvalKind::Fly => {
                let mut t = t;
                if t == PathType::Open && y >= MIN_Y + 1 {
                    let below = self.st(x, y - 1, z);
                    t = match below {
                        PathType::Fire | PathType::Lava => PathType::Fire,
                        PathType::Damaging => PathType::Damaging,
                        PathType::Cocoa => PathType::Cocoa,
                        PathType::Fence if [x, y - 1, z] != self.mob.block_pos() => PathType::Fence,
                        PathType::Fence => PathType::Open,
                        PathType::Walkable | PathType::Open | PathType::Water => PathType::Open,
                        _ => PathType::Walkable,
                    };
                }
                if matches!(t, PathType::Walkable | PathType::Open) {
                    t = self.neighbours(x, y, z, t);
                }
                return t;
            }
            EvalKind::Amphibious { .. } if t == PathType::Water => {
                for d in [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]] {
                    if self.st(x + d[0], y + d[1], z + d[2]) == PathType::Blocked {
                        return PathType::WaterBorder;
                    }
                }
                return PathType::Water;
            }
            _ => {}
        }
        if t == PathType::Open && y >= MIN_Y + 1 {
            return match self.st(x, y - 1, z) {
                PathType::Open | PathType::Water | PathType::Lava | PathType::Walkable => PathType::Open,
                PathType::Fire => PathType::Fire,
                PathType::Damaging => PathType::Damaging,
                PathType::StickyHoney => PathType::StickyHoney,
                PathType::PowderSnow => PathType::OnTopOfPowderSnow,
                PathType::DamageCautious => PathType::DamageCautious,
                PathType::Trapdoor => PathType::OnTopOfTrapdoor,
                _ => self.neighbours(x, y, z, PathType::Walkable),
            };
        }
        t
    }

    /// `checkNeighbourBlocks`
    fn neighbours(&mut self, x: i32, y: i32, z: i32, t: PathType) -> PathType {
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    match self.st(x + dx, y + dy, z + dz) {
                        PathType::Damaging => return PathType::DamagingInNeighbor,
                        PathType::Fire | PathType::Lava => return PathType::FireInNeighbor,
                        PathType::Water => return PathType::WaterBorder,
                        PathType::DamageCautious => return PathType::DamageCautious,
                        _ => {}
                    }
                }
            }
        }
        t
    }

    /// `getPathTypeOfMob` (cached)
    fn cached(&mut self, x: i32, y: i32, z: i32) -> PathType {
        if let Some(t) = self.types.get(&[x, y, z]) {
            return *t;
        }
        let t = match self.kind {
            EvalKind::Swim { .. } => self.swim_type(x, y, z),
            _ => self.mob_type(x, y, z),
        };
        self.types.insert([x, y, z], t);
        t
    }

    fn mob_type(&mut self, x: i32, y: i32, z: i32) -> PathType {
        let mut set: Vec<PathType> = Vec::new();
        let mp = self.mob.block_pos();
        for dx in 0..self.ew {
            for dy in 0..self.eh {
                for dz in 0..self.ed {
                    let mut t = self.path_type(x + dx, y + dy, z + dz);
                    if t == PathType::DoorWoodClosed && self.mob.can_open_doors && self.mob.can_pass_doors {
                        t = PathType::WalkableDoor;
                    }
                    if t == PathType::DoorOpen && !self.mob.can_pass_doors {
                        t = PathType::Blocked;
                    }
                    if t == PathType::Rail && self.path_type(mp[0], mp[1], mp[2]) != PathType::Rail && self.path_type(mp[0], mp[1] - 1, mp[2]) != PathType::Rail {
                        t = PathType::UnpassableRail;
                    }
                    if !set.contains(&t) {
                        set.push(t);
                    }
                }
            }
        }
        if set.len() == 1 {
            return set[0];
        }
        if set.contains(&PathType::Fence) {
            return PathType::Fence;
        }
        if set.contains(&PathType::UnpassableRail) {
            return PathType::UnpassableRail;
        }
        let mut best = PathType::Blocked;
        let mut best_m = self.mob.malus(best);
        // EnumSet iterates in ordinal order
        set.sort_by_key(|t| *t as u8);
        for t in set {
            let m = self.mob.malus(t);
            if m < 0.0 {
                return t;
            }
            if m >= best_m {
                best_m = m;
                best = t;
            }
        }
        let cur = self.path_type(x, y, z);
        if self.ew > 1 {
            let cheaper = self.mob.malus(cur) < best_m;
            if cheaper && self.mob.malus(PathType::BigMobsCloseToDanger) < best_m {
                return PathType::BigMobsCloseToDanger;
            }
            best
        } else if cur == PathType::Open && best != PathType::Open && best_m == 0.0 {
            PathType::Open
        } else {
            best
        }
    }

    /// `SwimNodeEvaluator.getPathTypeOfMob`
    fn swim_type(&mut self, x: i32, y: i32, z: i32) -> PathType {
        for xx in x..x + self.ew {
            for yy in y..y + self.eh {
                for zz in z..z + self.ed {
                    let s = self.level.get(xx, yy, zz);
                    let below = self.level.get(xx, yy - 1, zz);
                    let fluid = crate::registry::vstate::has_fluid(s);
                    if !fluid && pathfindable(below, 1) && s.is_air() {
                        return PathType::Breach;
                    }
                    if !crate::registry::vstate::is_water(s) {
                        return PathType::Blocked;
                    }
                }
            }
        }
        let s = self.level.get(x + self.ew - 1, y + self.eh - 1, z + self.ed - 1);
        if pathfindable(s, 1) { PathType::Water } else { PathType::Blocked }
    }

    fn floor_level(&mut self, p: [i32; 3]) -> f64 {
        if (self.can_float() || self.amphibious()) && crate::registry::vstate::is_water(self.level.get(p[0], p[1], p[2])) {
            return p[1] as f64 + 0.5;
        }
        floor_level(&mut |x, y, z| self.level.get(x, y, z), p)
    }

    fn has_collisions(&mut self, b: &Aabb) -> bool {
        let key = [b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]].map(|v| (v * 1e6) as i64);
        if let Some(v) = self.collisions.get(&key) {
            return *v;
        }
        let rb = RegionBlocks(std::cell::RefCell::new(&mut *self.level));
        let v = !physics::no_collision(&rb, b);
        self.collisions.insert(key, v);
        v
    }

    fn get_start(&mut self) -> Option<usize> {
        let m = self.mob.clone();
        let bb = m.bbox();
        match self.kind {
            EvalKind::Swim { .. } => return Some(self.node(bb.min[0].floor() as i32, (bb.min[1] + 0.5).floor() as i32, bb.min[2].floor() as i32)),
            EvalKind::Amphibious { .. } if m.in_water => {
                let p = [bb.min[0].floor() as i32, (bb.min[1] + 0.5).floor() as i32, bb.min[2].floor() as i32];
                return Some(self.start_node(p));
            }
            EvalKind::Fly => {
                let mut y = (m.pos[1] + 0.5).floor() as i32;
                if self.can_float() && m.in_water {
                    y = m.pos[1].floor() as i32;
                    while self.level.get(m.pos[0].floor() as i32, y, m.pos[2].floor() as i32).name() == "water" {
                        y += 1;
                    }
                }
                let p = [m.pos[0].floor() as i32, y, m.pos[2].floor() as i32];
                let ok = |e: &mut Self, p: [i32; 3]| {
                    let t = e.cached(p[0], p[1], p[2]);
                    e.mob.malus(t) >= 0.0
                };
                if !ok(self, p) {
                    for c in [[bb.min[0], bb.min[2]], [bb.min[0], bb.max[2]], [bb.max[0], bb.min[2]], [bb.max[0], bb.max[2]]] {
                        let q = [c[0].floor() as i32, m.pos[1].floor() as i32, c[1].floor() as i32];
                        if ok(self, q) {
                            return Some(self.start_node(q));
                        }
                    }
                }
                return Some(self.start_node(p));
            }
            _ => {}
        }
        let (bx, bz) = (m.pos[0].floor() as i32, m.pos[2].floor() as i32);
        let mut y = m.pos[1].floor() as i32;
        let s = self.level.get(bx, y, bz);
        if self.can_float() && m.in_water {
            let mut s = s;
            loop {
                if s.name() != "water" {
                    y -= 1;
                    break;
                }
                y += 1;
                s = self.level.get(bx, y, bz);
            }
        } else if m.on_ground {
            y = (m.pos[1] + 0.5).floor() as i32;
        } else {
            let mut cy = (m.pos[1] + 1.0).floor() as i32;
            while cy > MIN_Y {
                y = cy;
                cy -= 1;
                let b = self.level.get(bx, cy, bz);
                if !b.is_air() && !pathfindable(b, 0) {
                    break;
                }
            }
        }
        let can_start = |e: &mut Self, p: [i32; 3]| {
            let t = e.cached(p[0], p[1], p[2]);
            t != PathType::Open && e.mob.malus(t) >= 0.0
        };
        if !can_start(self, [bx, y, bz]) {
            for c in [[bb.min[0], bb.min[2]], [bb.min[0], bb.max[2]], [bb.max[0], bb.min[2]], [bb.max[0], bb.max[2]]] {
                let q = [c[0].floor() as i32, y, c[1].floor() as i32];
                if can_start(self, q) {
                    return Some(self.start_node(q));
                }
            }
        }
        Some(self.start_node([bx, y, bz]))
    }

    fn start_node(&mut self, p: [i32; 3]) -> usize {
        let i = self.node(p[0], p[1], p[2]);
        let t = self.cached(p[0], p[1], p[2]);
        self.nodes[i].ty = t;
        self.nodes[i].malus = self.mob.malus(t);
        i
    }

    fn update_max(&mut self, x: i32, y: i32, z: i32, t: PathType, cost: f32) -> usize {
        let i = self.node(x, y, z);
        self.nodes[i].ty = t;
        self.nodes[i].malus = self.nodes[i].malus.max(cost);
        i
    }

    fn blocked_node(&mut self, x: i32, y: i32, z: i32) -> usize {
        let i = self.node(x, y, z);
        self.nodes[i].ty = PathType::Blocked;
        self.nodes[i].malus = -1.0;
        i
    }

    fn neighbors(&mut self, cur: usize) -> Vec<usize> {
        match self.kind {
            EvalKind::Walk => self.walk_neighbors(cur),
            EvalKind::Amphibious { shallow } => {
                let mut out = self.walk_neighbors(cur);
                let (x, y, z) = (self.nodes[cur].x, self.nodes[cur].y, self.nodes[cur].z);
                let above = self.cached(x, y + 1, z);
                let current = self.cached(x, y, z);
                let jump = if self.mob.malus(above) >= 0.0 && current != PathType::StickyHoney { self.mob.max_up_step.max(1.0).floor() as i32 } else { 0 };
                let h = self.floor_level([x, y, z]);
                let up = self.accepted(x, y + 1, z, (jump - 1).max(0), h, [0, 0], current);
                let down = self.accepted(x, y - 1, z, jump, h, [0, 0], current);
                let cur_malus = self.nodes[cur].malus;
                let valid = |e: &Self, n: Option<usize>| n.is_some_and(|n| !e.nodes[n].closed && (e.nodes[n].malus >= 0.0 || cur_malus < 0.0) && e.nodes[n].ty == PathType::Water);
                if valid(self, up) {
                    out.push(up.unwrap_or(0));
                }
                if valid(self, down) && current != PathType::Trapdoor {
                    out.push(down.unwrap_or(0));
                }
                if shallow {
                    for &n in &out {
                        if self.nodes[n].ty == PathType::Water && self.nodes[n].y < 63 - 10 {
                            self.nodes[n].malus += 1.0;
                        }
                    }
                }
                out
            }
            EvalKind::Fly => self.fly_neighbors(cur),
            EvalKind::Swim { breach } => self.swim_neighbors(cur, breach),
        }
    }

    fn walk_neighbors(&mut self, cur: usize) -> Vec<usize> {
        let (x, y, z, cur_malus) = (self.nodes[cur].x, self.nodes[cur].y, self.nodes[cur].z, self.nodes[cur].malus);
        let above = self.cached(x, y + 1, z);
        let current = self.cached(x, y, z);
        let jump = if self.mob.malus(above) >= 0.0 && current != PathType::StickyHoney { self.mob.max_up_step.max(1.0).floor() as i32 } else { 0 };
        let h = self.floor_level([x, y, z]);
        let mut out = Vec::new();
        let mut side: [Option<usize>; 4] = [None; 4];
        for (i, d) in H_DIRS.iter().enumerate() {
            let n = self.accepted(x + d[0], y, z + d[1], jump, h, *d, current);
            side[i] = n;
            if let Some(n) = n {
                if !self.nodes[n].closed && (self.nodes[n].malus >= 0.0 || cur_malus < 0.0) {
                    out.push(n);
                }
            }
        }
        for i in 0..4 {
            // getClockWise of S W N E: W N E S
            let j = (i + 1) % 4;
            if self.diagonal_ok(cur, side[i], side[j]) {
                let (d1, d2) = (H_DIRS[i], H_DIRS[j]);
                let n = self.accepted(x + d1[0] + d2[0], y, z + d1[1] + d2[1], jump, h, d1, current);
                if let Some(n) = n {
                    if !self.nodes[n].closed && self.nodes[n].ty != PathType::WalkableDoor && self.nodes[n].malus >= 0.0 {
                        out.push(n);
                    }
                }
            }
        }
        out
    }

    fn diagonal_ok(&self, pos: usize, ew: Option<usize>, ns: Option<usize>) -> bool {
        let (Some(ew), Some(ns)) = (ew, ns) else { return false };
        let (p, e, n) = (&self.nodes[pos], &self.nodes[ew], &self.nodes[ns]);
        if n.y > p.y || e.y > p.y {
            return false;
        }
        if e.ty == PathType::WalkableDoor || n.ty == PathType::WalkableDoor {
            return false;
        }
        if self.mob.width > 1.0 && (e.malus > 0.0 || n.malus > 0.0) {
            return false;
        }
        let posts = n.ty == PathType::Fence && e.ty == PathType::Fence && self.mob.width < 0.5;
        (n.y < p.y || n.malus >= 0.0 || posts) && (e.y < p.y || e.malus >= 0.0 || posts)
    }

    /// `WalkNodeEvaluator.findAcceptedNode`
    #[allow(clippy::too_many_arguments)]
    fn accepted(&mut self, x: i32, y: i32, z: i32, jump: i32, node_h: f64, dir: [i32; 2], current: PathType) -> Option<usize> {
        let max_y = self.floor_level([x, y, z]);
        if max_y - node_h > (self.mob.max_up_step as f64).max(1.125) {
            return None;
        }
        let t = self.cached(x, y, z);
        let cost = self.mob.malus(t);
        let mut best = if cost >= 0.0 { Some(self.update_max(x, y, z, t, cost)) } else { None };
        let partial = |t: PathType| matches!(t, PathType::Fence | PathType::DoorWoodClosed | PathType::DoorIronClosed);
        if partial(current) && best.is_some_and(|b| self.nodes[b].malus >= 0.0) && !self.reach_without_collision(best.unwrap_or(0)) {
            best = None;
        }
        if t == PathType::Walkable || self.amphibious() && t == PathType::Water {
            return best;
        }
        if (best.is_none() || best.is_some_and(|b| self.nodes[b].malus < 0.0)) && jump > 0 && (t != PathType::Fence || self.mob.can_walk_over_fences) && !matches!(t, PathType::UnpassableRail | PathType::Trapdoor | PathType::PowderSnow) {
            best = self.jump_on(x, y, z, jump, node_h, dir, current);
        } else if !self.amphibious() && t == PathType::Water && !self.can_float() {
            let mut yy = y - 1;
            while yy > MIN_Y {
                let tl = self.cached(x, yy, z);
                if tl != PathType::Water {
                    return best;
                }
                let m = self.mob.malus(tl);
                best = Some(self.update_max(x, yy, z, tl, m));
                yy -= 1;
            }
        } else if t == PathType::Open {
            return Some(self.first_ground_below(x, y, z));
        } else if partial(t) && best.is_none() {
            let i = self.node(x, y, z);
            self.nodes[i].closed = true;
            self.nodes[i].ty = t;
            self.nodes[i].malus = t.malus();
            best = Some(i);
        }
        best
    }

    fn reach_without_collision(&mut self, n: usize) -> bool {
        let mut bb = self.mob.bbox();
        let (xs, ys) = (bb.max[0] - bb.min[0], bb.max[1] - bb.min[1]);
        let node = &self.nodes[n];
        let mut d = [node.x as f64 - self.mob.pos[0] + xs / 2.0, node.y as f64 - self.mob.pos[1] + ys / 2.0, node.z as f64 - self.mob.pos[2] + xs / 2.0];
        let size = (xs + ys + xs) / 3.0;
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let steps = (len / size).ceil().max(1.0) as i32;
        for v in &mut d {
            *v /= steps as f64;
        }
        for _ in 0..steps {
            bb = bb.translate(d);
            if self.has_collisions(&bb) {
                return false;
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn jump_on(&mut self, x: i32, y: i32, z: i32, jump: i32, node_h: f64, dir: [i32; 2], current: PathType) -> Option<usize> {
        let above = self.accepted(x, y + 1, z, jump - 1, node_h, dir, current)?;
        if self.mob.width >= 1.0 {
            return Some(above);
        }
        let at = self.nodes[above].ty;
        if at != PathType::Open && at != PathType::Walkable {
            return Some(above);
        }
        let cx = (x - dir[0]) as f64 + 0.5;
        let cz = (z - dir[1]) as f64 + 0.5;
        let hw = self.mob.width / 2.0;
        let f1 = self.floor_level([cx.floor() as i32, y + 1, cz.floor() as i32]);
        let an = &self.nodes[above];
        let ap = [an.x, an.y, an.z];
        let f2 = self.floor_level(ap);
        let grow = Aabb { min: [cx - hw, f1 + 0.001, cz - hw], max: [cx + hw, self.mob.height + f2 - 0.002, cz + hw] };
        if self.has_collisions(&grow) { None } else { Some(above) }
    }

    fn first_ground_below(&mut self, x: i32, y: i32, z: i32) -> usize {
        let mut cy = y - 1;
        while cy >= MIN_Y {
            if y - cy > self.mob.max_fall {
                return self.blocked_node(x, cy, z);
            }
            let t = self.cached(x, cy, z);
            let cost = self.mob.malus(t);
            if t != PathType::Open {
                if cost >= 0.0 {
                    return self.update_max(x, cy, z, t, cost);
                }
                return self.blocked_node(x, cy, z);
            }
            cy -= 1;
        }
        self.blocked_node(x, y, z)
    }

    fn fly_accept(&mut self, x: i32, y: i32, z: i32) -> Option<usize> {
        let t = self.cached(x, y, z);
        let cost = self.mob.malus(t);
        if cost < 0.0 {
            return None;
        }
        let i = self.update_max(x, y, z, t, cost);
        if t == PathType::Walkable {
            self.nodes[i].malus += 1.0;
        }
        Some(i)
    }

    fn fly_neighbors(&mut self, cur: usize) -> Vec<usize> {
        let (x, y, z) = (self.nodes[cur].x, self.nodes[cur].y, self.nodes[cur].z);
        let mut found: HashMap<[i32; 3], Option<usize>> = HashMap::new();
        let mut get = |e: &mut Self, d: [i32; 3]| -> Option<usize> {
            if let Some(v) = found.get(&d) {
                return *v;
            }
            let v = e.fly_accept(x + d[0], y + d[1], z + d[2]);
            found.insert(d, v);
            v
        };
        let mut out = Vec::new();
        let open = |e: &Self, n: Option<usize>| n.is_some_and(|n| !e.nodes[n].closed);
        let has = |e: &Self, n: Option<usize>| n.is_some_and(|n| e.nodes[n].malus >= 0.0);
        // straight: S W E N U D
        let straight = [[0, 0, 1], [-1, 0, 0], [1, 0, 0], [0, 0, -1], [0, 1, 0], [0, -1, 0]];
        for d in straight {
            let n = get(self, d);
            if open(self, n) {
                out.push(n.unwrap_or(0));
            }
        }
        // edges: horizontal + vertical, and horizontal pairs
        let horizontal = [[0, 0, 1], [-1, 0, 0], [1, 0, 0], [0, 0, -1]];
        for v in [1, -1] {
            for h in horizontal {
                let d = [h[0], v, h[2]];
                let n = get(self, d);
                let (a, b) = (get(self, h), get(self, [0, v, 0]));
                if open(self, n) && has(self, a) && has(self, b) {
                    out.push(n.unwrap_or(0));
                }
            }
        }
        for (a, b) in [([0, 0, -1], [1, 0, 0]), ([0, 0, 1], [1, 0, 0]), ([0, 0, -1], [-1, 0, 0]), ([0, 0, 1], [-1, 0, 0])] {
            let d = [a[0] + b[0], 0, a[2] + b[2]];
            let n = get(self, d);
            let (na, nb) = (get(self, a), get(self, b));
            if open(self, n) && has(self, na) && has(self, nb) {
                out.push(n.unwrap_or(0));
            }
        }
        for v in [1, -1] {
            for (a, b) in [([0, 0, -1], [1, 0, 0]), ([0, 0, 1], [1, 0, 0]), ([0, 0, -1], [-1, 0, 0]), ([0, 0, 1], [-1, 0, 0])] {
                let d = [a[0] + b[0], v, a[2] + b[2]];
                let n = get(self, d);
                let req = [[a[0] + b[0], 0, a[2] + b[2]], a, b, [0, v, 0], [a[0], v, a[2]], [b[0], v, b[2]]];
                let ok = req.iter().all(|r| {
                    let x = get(self, *r);
                    has(self, x)
                });
                if open(self, n) && ok {
                    out.push(n.unwrap_or(0));
                }
            }
        }
        out
    }

    fn swim_accept(&mut self, x: i32, y: i32, z: i32, breach: bool) -> Option<usize> {
        let t = self.cached(x, y, z);
        if !(breach && t == PathType::Breach || t == PathType::Water) {
            return None;
        }
        let cost = self.mob.malus(t);
        if cost < 0.0 {
            return None;
        }
        let i = self.update_max(x, y, z, t, cost);
        if !crate::registry::vstate::has_fluid(self.level.get(x, y, z)) {
            self.nodes[i].malus += 8.0;
        }
        Some(i)
    }

    fn swim_neighbors(&mut self, cur: usize, breach: bool) -> Vec<usize> {
        let (x, y, z) = (self.nodes[cur].x, self.nodes[cur].y, self.nodes[cur].z);
        let mut out = Vec::new();
        let dirs = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];
        let mut by: HashMap<[i32; 3], Option<usize>> = HashMap::new();
        for d in dirs {
            let n = self.swim_accept(x + d[0], y + d[1], z + d[2], breach);
            by.insert(d, n);
            if let Some(n) = n {
                if !self.nodes[n].closed {
                    out.push(n);
                }
            }
        }
        let has = |e: &Self, n: Option<usize>| n.is_some_and(|n| e.nodes[n].malus >= 0.0);
        for i in 0..4 {
            let (a, b) = (H_DIRS[i], H_DIRS[(i + 1) % 4]);
            let (na, nb) = (by.get(&[a[0], 0, a[1]]).copied().flatten(), by.get(&[b[0], 0, b[1]]).copied().flatten());
            if has(self, na) && has(self, nb) {
                if let Some(n) = self.swim_accept(x + a[0] + b[0], y, z + a[1] + b[1], breach) {
                    if !self.nodes[n].closed {
                        out.push(n);
                    }
                }
            }
        }
        out
    }
}

/// `WalkNodeEvaluator.getFloorLevel(level, pos)`
pub fn floor_level(get: &mut dyn FnMut(i32, i32, i32) -> BlockState, p: [i32; 3]) -> f64 {
    let below = get(p[0], p[1] - 1, p[2]);
    let top = physics::collision(below).iter().map(|b| b.max[1]).fold(f64::NAN, f64::max);
    (p[1] - 1) as f64 + if top.is_nan() { 0.0 } else { top }
}

// ---------------------------------------------------------------- path finder

#[derive(Clone, Debug)]
pub struct Path {
    pub nodes: Vec<Node>,
    pub next: usize,
    pub target: [i32; 3],
    pub reached: bool,
    pub dist_to_target: f32,
}

impl Path {
    pub fn is_done(&self) -> bool {
        self.next >= self.nodes.len()
    }

    pub fn advance(&mut self) {
        self.next += 1;
    }

    pub fn next_node(&self) -> &Node {
        &self.nodes[self.next]
    }

    pub fn end(&self) -> Option<&Node> {
        self.nodes.last()
    }

    /// `getEntityPosAtNode`
    pub fn entity_pos(&self, width: f64, i: usize) -> [f64; 3] {
        let n = &self.nodes[i];
        let off = ((width + 1.0) as i32) as f64 * 0.5;
        [n.x as f64 + off, n.y as f64, n.z as f64 + off]
    }

    pub fn can_reach(&self) -> bool {
        self.reached
    }

    pub fn truncate(&mut self, len: usize) {
        self.nodes.truncate(len);
    }

    fn same_as(&self, o: &Path) -> bool {
        self.nodes.len() == o.nodes.len() && self.nodes.iter().zip(&o.nodes).all(|(a, b)| a.pos() == b.pos())
    }
}

struct Open(f32, usize);
impl PartialEq for Open {
    fn eq(&self, o: &Self) -> bool {
        self.0 == o.0
    }
}
impl Eq for Open {}
impl PartialOrd for Open {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Open {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.0.total_cmp(&self.0)
    }
}

/// `PathFinder.findPath`
#[allow(clippy::too_many_arguments)]
pub fn find_path(kind: EvalKind, mob: &PathMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, targets: &[[i32; 3]], max_len: f32, reach: i32, max_visited: i32) -> Option<Path> {
    let mut region = Region { w: get, cache: HashMap::new() };
    let mut mob = mob.clone();
    if let EvalKind::Amphibious { .. } = kind {
        mob.malus[PathType::Water as usize] = 0.0;
        mob.malus[PathType::Walkable as usize] = 6.0;
        mob.malus[PathType::WaterBorder as usize] = 4.0;
    }
    let ew = (mob.width + 1.0).floor() as i32;
    let eh = (mob.height + 1.0).floor() as i32;
    let mut ev = Eval { kind, mob, level: &mut region, nodes: Vec::new(), index: HashMap::new(), types: HashMap::new(), collisions: HashMap::new(), ew, eh, ed: ew };
    let from = ev.get_start()?;
    // targets
    let tnodes: Vec<(usize, [i32; 3])> = targets
        .iter()
        .map(|t| {
            let ty = if matches!(kind, EvalKind::Amphibious { .. }) { [t[0], t[1], t[2]] } else { *t };
            (ev.node(ty[0], ty[1], ty[2]), *t)
        })
        .collect();
    let mut best: Vec<(f32, Option<usize>)> = vec![(f32::MAX, None); tnodes.len()];
    let tpos: Vec<[i32; 3]> = tnodes.iter().map(|(i, _)| ev.nodes[*i].pos()).collect();
    let best_h = |ev: &Eval, n: usize, best: &mut Vec<(f32, Option<usize>)>| -> f32 {
        let mut bh = f32::MAX;
        for (k, t) in tpos.iter().enumerate() {
            let h = ev.nodes[n].dist(*t);
            if h < best[k].0 {
                best[k] = (h, Some(n));
            }
            bh = bh.min(h);
        }
        bh
    };
    ev.nodes[from].g = 0.0;
    let h = best_h(&ev, from, &mut best);
    ev.nodes[from].h = h;
    ev.nodes[from].f = h;
    let mut open = BinaryHeap::new();
    open.push(Open(h, from));
    ev.nodes[from].in_open = true;
    let mut reached: Vec<usize> = Vec::new();
    let mut count = 0;
    let from_pos = ev.nodes[from].pos();
    while let Some(Open(f, cur)) = open.pop() {
        if !ev.nodes[cur].in_open || (ev.nodes[cur].f - f).abs() > 1e-6 {
            continue; // stale entry
        }
        count += 1;
        if count >= max_visited {
            break;
        }
        ev.nodes[cur].in_open = false;
        ev.nodes[cur].closed = true;
        for (k, t) in tpos.iter().enumerate() {
            if ev.nodes[cur].manhattan(*t) <= reach as f32 {
                if !reached.contains(&k) {
                    reached.push(k);
                }
                // Target.setReached: the target node itself is reached; path ends at its best node
                best[k] = (best[k].0.min(0.0), Some(cur));
            }
        }
        if !reached.is_empty() {
            break;
        }
        if ev.nodes[cur].dist(from_pos) >= max_len {
            continue;
        }
        for n in ev.neighbors(cur) {
            let d = ev.nodes[cur].dist(ev.nodes[n].pos());
            ev.nodes[n].walked = ev.nodes[cur].walked + d;
            let g = ev.nodes[cur].g + d + ev.nodes[n].malus;
            if ev.nodes[n].walked < max_len && (!ev.nodes[n].in_open || g < ev.nodes[n].g) {
                ev.nodes[n].came_from = Some(cur);
                ev.nodes[n].g = g;
                let h = best_h(&ev, n, &mut best) * 1.5;
                ev.nodes[n].h = h;
                ev.nodes[n].f = g + h;
                ev.nodes[n].in_open = true;
                open.push(Open(g + h, n));
            }
        }
    }
    let build = |ev: &Eval, end: usize, target: [i32; 3], reached: bool| -> Path {
        let mut nodes = vec![ev.nodes[end].clone()];
        let mut n = end;
        while let Some(p) = ev.nodes[n].came_from {
            nodes.push(ev.nodes[p].clone());
            n = p;
        }
        nodes.reverse();
        let dist = nodes.last().map(|l| l.manhattan(target)).unwrap_or(f32::MAX);
        Path { nodes, next: 0, target, reached, dist_to_target: dist }
    };
    let paths: Vec<Path> = if !reached.is_empty() {
        reached.iter().filter_map(|&k| best[k].1.map(|b| build(&ev, b, tnodes[k].1, true))).collect()
    } else {
        (0..tnodes.len()).filter_map(|k| best[k].1.map(|b| build(&ev, b, tnodes[k].1, false))).collect()
    };
    if !reached.is_empty() {
        paths.into_iter().min_by_key(|p| p.nodes.len())
    } else {
        paths.into_iter().min_by(|a, b| a.dist_to_target.total_cmp(&b.dist_to_target).then(a.nodes.len().cmp(&b.nodes.len())))
    }
}

// ---------------------------------------------------------------- navigation

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKind {
    Ground,
    Climber,
    Flying,
    Water,
    Amphibious,
}

/// `PathNavigation` state.
#[derive(Clone, Debug)]
pub struct Navigation {
    pub kind: NavKind,
    pub path: Option<Path>,
    pub speed: f64,
    tick: i64,
    last_stuck_check: i64,
    last_stuck_pos: [f64; 3],
    timeout_node: [i32; 3],
    timeout_timer: i64,
    last_timeout_check: i64,
    timeout_limit: f64,
    pub max_dist_to_waypoint: f32,
    delayed_recompute: bool,
    last_recompute: i64,
    target_pos: Option<[i32; 3]>,
    reach: i32,
    pub stuck: bool,
    pub avoid_sun: bool,
    pub can_float: bool,
    pub can_open_doors: bool,
    pub can_pass_doors: bool,
    pub can_walk_over_fences: bool,
    pub below_surface: bool,
    pub breach: bool,
    pub required_len: f32,
    /// WallClimberNavigation.pathToPosition
    climb_to: Option<[i32; 3]>,
}

/// What navigation needs about the mob right now.
pub struct NavMob {
    pub pos: [f64; 3],
    pub width: f64,
    pub height: f64,
    pub on_ground: bool,
    pub in_water: bool,
    pub in_liquid: bool,
    pub passenger: bool,
    pub speed: f32,
    pub follow_range: f64,
    pub path: PathMob,
}

impl Navigation {
    pub fn new(kind: NavKind) -> Navigation {
        Navigation {
            kind,
            path: None,
            speed: 0.0,
            tick: 0,
            last_stuck_check: 0,
            last_stuck_pos: [0.0; 3],
            timeout_node: [0; 3],
            timeout_timer: 0,
            last_timeout_check: 0,
            timeout_limit: 0.0,
            max_dist_to_waypoint: 0.5,
            delayed_recompute: false,
            last_recompute: 0,
            target_pos: None,
            reach: 0,
            stuck: false,
            avoid_sun: false,
            can_float: false,
            can_open_doors: false,
            can_pass_doors: true,
            can_walk_over_fences: false,
            below_surface: false,
            breach: false,
            required_len: 16.0,
            climb_to: None,
        }
    }

    pub fn is_done(&self) -> bool {
        self.path.as_ref().is_none_or(|p| p.is_done())
    }

    pub fn in_progress(&self) -> bool {
        !self.is_done()
    }

    pub fn stop(&mut self) {
        self.path = None;
    }

    fn eval_kind(&self) -> EvalKind {
        match self.kind {
            NavKind::Ground | NavKind::Climber => EvalKind::Walk,
            NavKind::Flying => EvalKind::Fly,
            NavKind::Water => EvalKind::Swim { breach: self.breach },
            NavKind::Amphibious => EvalKind::Amphibious { shallow: false },
        }
    }

    fn can_update(&self, m: &NavMob) -> bool {
        match self.kind {
            NavKind::Ground | NavKind::Climber => m.on_ground || m.in_liquid || m.passenger,
            NavKind::Flying => self.can_float && m.in_liquid || !m.passenger,
            NavKind::Water => self.breach || m.in_liquid,
            NavKind::Amphibious => true,
        }
    }

    fn temp_pos(&self, m: &NavMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>) -> [f64; 3] {
        match self.kind {
            NavKind::Ground | NavKind::Climber => {
                let y = if m.in_water && self.can_float {
                    let mut s = m.pos[1].floor() as i32;
                    let mut steps = 0;
                    loop {
                        let b = get(m.pos[0].floor() as i32, s, m.pos[2].floor() as i32).unwrap_or(blocks::AIR);
                        if b.name() != "water" {
                            break s;
                        }
                        s += 1;
                        steps += 1;
                        if steps > 16 {
                            break m.pos[1].floor() as i32;
                        }
                    }
                } else {
                    (m.pos[1] + 0.5).floor() as i32
                };
                [m.pos[0], y as f64, m.pos[2]]
            }
            NavKind::Flying => m.pos,
            NavKind::Water | NavKind::Amphibious => [m.pos[0], m.pos[1] + m.height * 0.5, m.pos[2]],
        }
    }

    fn max_path_len(&self, m: &NavMob) -> f32 {
        (m.follow_range as f32).max(self.required_len)
    }

    /// `createPath(BlockPos, reach)` (+ ground surface search)
    pub fn create_path(&mut self, m: &NavMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, pos: [i32; 3], reach: i32, now: i64) -> Option<Path> {
        self.create_path_full(m, get, &[pos], 8, false, reach, None, now)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_path_full(&mut self, m: &NavMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, targets: &[[i32; 3]], radius_offset: i32, above: bool, reach: i32, max_len: Option<f32>, now: i64) -> Option<Path> {
        let _ = (radius_offset, now);
        let mut targets: Vec<[i32; 3]> = targets.to_vec();
        if matches!(self.kind, NavKind::Ground | NavKind::Climber) && !self.below_surface {
            for t in &mut targets {
                get(t[0], t[1], t[2])?;
                *t = surface(get, *t);
            }
        }
        if matches!(self.kind, NavKind::Climber) {
            self.climb_to = targets.first().copied();
        }
        if targets.is_empty() || m.pos[1] < MIN_Y as f64 || !self.can_update(m) {
            return None;
        }
        if let Some(p) = &self.path {
            if !p.is_done() && self.target_pos.is_some_and(|t| targets.contains(&t)) {
                return Some(p.clone());
            }
        }
        let max_len = max_len.unwrap_or_else(|| self.max_path_len(m));
        let mut pm = m.path.clone();
        if above {
            pm.pos[1] += 1.0;
        }
        pm.can_float = self.can_float;
        pm.can_open_doors = self.can_open_doors;
        pm.can_pass_doors = self.can_pass_doors;
        pm.can_walk_over_fences = self.can_walk_over_fences;
        let max_visited = (self.max_path_len(m) * 16.0).floor() as i32;
        let path = find_path(self.eval_kind(), &pm, get, &targets, max_len, reach, max_visited);
        if let Some(p) = &path {
            self.target_pos = Some(p.target);
            self.reach = reach;
            self.reset_stuck();
        }
        path
    }

    /// `moveTo(Path, speed)`
    pub fn move_to_path(&mut self, m: &NavMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, path: Option<Path>, speed: f64, sky: &dyn Fn(i32, i32, i32) -> bool) -> bool {
        let Some(np) = path else {
            self.path = None;
            return false;
        };
        if !self.path.as_ref().is_some_and(|p| p.same_as(&np)) {
            self.path = Some(np);
        }
        if self.is_done() {
            return false;
        }
        self.trim(m, get, sky);
        if self.path.as_ref().is_none_or(|p| p.nodes.is_empty()) {
            return false;
        }
        self.speed = speed;
        self.last_stuck_check = self.tick;
        self.last_stuck_pos = self.temp_pos(m, get);
        true
    }

    /// `moveTo(x, y, z, reach, speed)`
    #[allow(clippy::too_many_arguments)]
    pub fn move_to(&mut self, m: &NavMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, to: [f64; 3], reach: i32, speed: f64, now: i64, sky: &dyn Fn(i32, i32, i32) -> bool) -> bool {
        let p = self.create_path(m, get, [to[0].floor() as i32, to[1].floor() as i32, to[2].floor() as i32], reach, now);
        if p.is_none() && self.kind == NavKind::Climber {
            self.speed = speed;
            return true;
        }
        self.move_to_path(m, get, p, speed, sky)
    }

    /// `moveTo(Entity, speed)` (entity paths aim one block above for walkers)
    pub fn move_to_entity(&mut self, m: &NavMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, target: [f64; 3], speed: f64, now: i64, sky: &dyn Fn(i32, i32, i32) -> bool) -> bool {
        let tp = [target[0].floor() as i32, target[1].floor() as i32, target[2].floor() as i32];
        let reach = if self.kind == NavKind::Climber { 0 } else { 1 };
        let p = match self.kind {
            NavKind::Ground | NavKind::Climber | NavKind::Flying => self.create_path(m, get, tp, reach, now),
            _ => self.create_path_full(m, get, &[tp], 16, true, reach, None, now),
        };
        if p.is_none() && self.kind == NavKind::Climber {
            self.climb_to = Some(tp);
            self.speed = speed;
            return true;
        }
        p.is_some() && self.move_to_path(m, get, p, speed, sky)
    }

    fn trim(&mut self, m: &NavMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, sky: &dyn Fn(i32, i32, i32) -> bool) {
        let Some(path) = &mut self.path else { return };
        for i in 0..path.nodes.len() {
            let n = path.nodes[i].clone();
            let s = get(n.x, n.y, n.z).unwrap_or(blocks::AIR);
            if s.name().ends_with("cauldron") {
                path.nodes[i].y += 1;
                if i + 1 < path.nodes.len() && n.y >= path.nodes[i + 1].y {
                    path.nodes[i + 1].y = n.y + 1;
                }
            }
        }
        if self.avoid_sun && matches!(self.kind, NavKind::Ground | NavKind::Climber) {
            if sky(m.pos[0].floor() as i32, (m.pos[1] + 0.5).floor() as i32, m.pos[2].floor() as i32) {
                return;
            }
            for i in 0..path.nodes.len() {
                let n = &path.nodes[i];
                if sky(n.x, n.y, n.z) {
                    path.truncate(i);
                    return;
                }
            }
        }
    }

    pub fn recompute(&mut self, m: &NavMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, now: i64) {
        if now - self.last_recompute <= 20 || !self.can_update(m) {
            self.delayed_recompute = true;
        } else if let Some(t) = self.target_pos {
            self.path = None;
            self.path = self.create_path(m, get, t, self.reach, now);
            self.last_recompute = now;
            self.delayed_recompute = false;
        }
    }

    /// `PathNavigation.tick`: returns the wanted position for the move control.
    pub fn tick(&mut self, m: &NavMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, now: i64) -> Option<([f64; 3], f64)> {
        if self.kind == NavKind::Climber && self.is_done() {
            let p = self.climb_to?;
            let c = [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5];
            let close = |q: [f64; 3]| dist2(q, m.pos) < m.width * m.width;
            let c2 = [p[0] as f64 + 0.5, m.pos[1].floor() + 0.5, p[2] as f64 + 0.5];
            if !close(c) && (m.pos[1] <= p[1] as f64 || !close(c2)) {
                return Some(([p[0] as f64, p[1] as f64, p[2] as f64], self.speed));
            }
            self.climb_to = None;
            return None;
        }
        self.tick += 1;
        if self.delayed_recompute {
            self.recompute(m, get, now);
        }
        if self.is_done() {
            return None;
        }
        if self.can_update(m) {
            self.follow(m, get, now);
        } else if self.path.is_some() {
            let mp = self.temp_pos_static(m);
            let p = self.path.as_mut().unwrap();
            if !p.is_done() {
                let np = p.entity_pos(m.width, p.next);
                let same_col = mp[0].floor() == np[0].floor() && mp[2].floor() == np[2].floor();
                let adv = if self.kind == NavKind::Flying { m.pos.iter().zip(np.iter()).all(|(a, b)| a.floor() == b.floor()) } else { mp[1] > np[1] && !m.on_ground && same_col };
                if adv {
                    p.advance();
                }
            }
        }
        if self.is_done() {
            return None;
        }
        let p = self.path.as_ref()?;
        let t = p.entity_pos(m.width, p.next);
        let y = match self.kind {
            NavKind::Ground | NavKind::Climber => {
                let bp = [t[0].floor() as i32, t[1].floor() as i32, t[2].floor() as i32];
                if get(bp[0], bp[1] - 1, bp[2]).unwrap_or(blocks::AIR).is_air() {
                    t[1]
                } else {
                    floor_level(&mut |x, y, z| get(x, y, z).unwrap_or(blocks::AIR), bp)
                }
            }
            _ => t[1],
        };
        Some(([t[0], y, t[2]], self.speed))
    }

    fn temp_pos_static(&self, m: &NavMob) -> [f64; 3] {
        match self.kind {
            NavKind::Ground | NavKind::Climber => [m.pos[0], (m.pos[1] + 0.5).floor(), m.pos[2]],
            NavKind::Flying => m.pos,
            _ => [m.pos[0], m.pos[1] + m.height * 0.5, m.pos[2]],
        }
    }

    fn follow(&mut self, m: &NavMob, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, now: i64) {
        let mp = self.temp_pos(m, get);
        self.max_dist_to_waypoint = if m.width > 0.75 { m.width as f32 / 2.0 } else { 0.75 - m.width as f32 / 2.0 };
        let Some(p) = &mut self.path else { return };
        let n = p.next_node().clone();
        let xd = (m.pos[0] - (n.x as f64 + 0.5)).abs();
        let yd = (m.pos[1] - n.y as f64).abs();
        let zd = (m.pos[2] - (n.z as f64 + 0.5)).abs();
        let close = xd < self.max_dist_to_waypoint as f64 && zd < self.max_dist_to_waypoint as f64 && yd < 1.0;
        let can_cut = !matches!(n.ty, PathType::FireInNeighbor | PathType::DamagingInNeighbor | PathType::WalkableDoor);
        let kind = self.kind;
        if close || can_cut && next_in_direction(p, mp, m, kind, get) {
            p.advance();
        }
        self.stuck_detection(mp, m, now);
    }

    fn stuck_detection(&mut self, mp: [f64; 3], m: &NavMob, now: i64) {
        if self.tick - self.last_stuck_check > 100 {
            let eff = if m.speed >= 1.0 { m.speed } else { m.speed * m.speed };
            let thr = (eff * 100.0 * 0.25) as f64;
            if dist2(mp, self.last_stuck_pos) < thr * thr {
                self.stuck = true;
                self.stop();
            } else {
                self.stuck = false;
            }
            self.last_stuck_check = self.tick;
            self.last_stuck_pos = mp;
        }
        if let Some(p) = &self.path {
            if !p.is_done() {
                let pos = p.next_node().pos();
                if pos == self.timeout_node {
                    self.timeout_timer += now - self.last_timeout_check;
                } else {
                    self.timeout_node = pos;
                    let c = [pos[0] as f64 + 0.5, pos[1] as f64, pos[2] as f64 + 0.5];
                    let d = dist2(mp, c).sqrt();
                    self.timeout_limit = if m.speed > 0.0 { d / m.speed as f64 * 20.0 } else { 0.0 };
                }
                if self.timeout_limit > 0.0 && self.timeout_timer as f64 > self.timeout_limit * 3.0 {
                    self.reset_stuck();
                    self.stop();
                }
                self.last_timeout_check = now;
            }
        }
    }

    fn reset_stuck(&mut self) {
        self.timeout_node = [0; 3];
        self.timeout_timer = 0;
        self.timeout_limit = 0.0;
        self.stuck = false;
    }

    /// `shouldRecomputePath(pos)`
    pub fn should_recompute(&self, pos: [i32; 3], mob: [f64; 3]) -> bool {
        if self.delayed_recompute {
            return false;
        }
        let Some(p) = &self.path else { return false };
        if p.is_done() || p.nodes.is_empty() {
            return false;
        }
        let Some(t) = p.end() else { return false };
        let mid = [(t.x as f64 + mob[0]) / 2.0, (t.y as f64 + mob[1]) / 2.0, (t.z as f64 + mob[2]) / 2.0];
        let c = [pos[0] as f64 + 0.5, pos[1] as f64 + 0.5, pos[2] as f64 + 0.5];
        let r = (p.nodes.len() - p.next) as f64;
        dist2(c, mid) < r * r
    }

    pub fn target(&self) -> Option<[i32; 3]> {
        self.target_pos
    }
}

/// `shouldTargetNextNodeInDirection` (+ `canMoveDirectly` for flyers / swimmers)
fn next_in_direction(p: &Path, mp: [f64; 3], m: &NavMob, kind: NavKind, get: &dyn Fn(i32, i32, i32) -> Option<BlockState>) -> bool {
    if p.next + 1 >= p.nodes.len() {
        return false;
    }
    let cn = p.nodes[p.next].pos();
    let cur = [cn[0] as f64 + 0.5, cn[1] as f64, cn[2] as f64 + 0.5];
    if dist2(mp, cur) >= 4.0 {
        return false;
    }
    let direct = match kind {
        NavKind::Flying => clear_between(m, mp, p.entity_pos(m.width, p.next), get),
        NavKind::Water => clear_between(m, mp, p.entity_pos(m.width, p.next), get),
        NavKind::Amphibious if m.in_liquid => clear_between(m, mp, p.entity_pos(m.width, p.next), get),
        _ => false,
    };
    if direct {
        return true;
    }
    let nn = p.nodes[p.next + 1].pos();
    let next = [nn[0] as f64 + 0.5, nn[1] as f64, nn[2] as f64 + 0.5];
    let to_cur = [cur[0] - mp[0], cur[1] - mp[1], cur[2] - mp[2]];
    let to_next = [next[0] - mp[0], next[1] - mp[1], next[2] - mp[2]];
    let (cs, ns) = (len2(to_cur), len2(to_next));
    if !(ns < cs) && !(cs < 0.5) {
        return false;
    }
    let (a, b) = (norm(to_cur), norm(to_next));
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] < 0.0
}

fn clear_between(m: &NavMob, from: [f64; 3], to: [f64; 3], get: &dyn Fn(i32, i32, i32) -> Option<BlockState>) -> bool {
    struct G<'a>(&'a dyn Fn(i32, i32, i32) -> Option<BlockState>);
    impl Blocks for G<'_> {
        fn block(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
            (self.0)(x, y, z)
        }
    }
    physics::clip(&G(get), from, [to[0], to[1] + m.height * 0.5, to[2]]).is_none()
}

/// `GroundPathNavigation.findSurfacePosition`
fn surface(get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, p: [i32; 3]) -> [i32; 3] {
    let at = |y: i32| get(p[0], y, p[2]).unwrap_or(blocks::AIR);
    let top = MIN_Y + crate::world::chunk::HEIGHT - 1;
    let mut pos = p;
    if at(p[1]).is_air() {
        let mut y = p[1] - 1;
        while y >= MIN_Y && at(y).is_air() {
            y -= 1;
        }
        if y >= MIN_Y {
            return [p[0], y + 1, p[2]];
        }
        let mut y = p[1] + 1;
        while y <= top && at(y).is_air() {
            y += 1;
        }
        pos = [p[0], y, p[2]];
    }
    if !legacy_solid(at(pos[1])) {
        return pos;
    }
    let mut y = pos[1] + 1;
    while y <= top && legacy_solid(at(y)) {
        y += 1;
    }
    [p[0], y, p[2]]
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

fn len2(a: [f64; 3]) -> f64 {
    a[0] * a[0] + a[1] * a[1] + a[2] * a[2]
}

fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = len2(a).sqrt();
    if l < 1e-5 { [0.0; 3] } else { [a[0] / l, a[1] / l, a[2] / l] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::BlockRegistry;

    fn zombie_at(pos: [f64; 3]) -> NavMob {
        NavMob {
            pos,
            width: 0.6,
            height: 1.95,
            on_ground: true,
            in_water: false,
            in_liquid: false,
            passenger: false,
            speed: 0.23,
            follow_range: 35.0,
            path: PathMob { pos, width: 0.6, height: 1.95, on_ground: true, in_water: false, max_up_step: 0.6, max_fall: 3, malus: default_malus(), can_open_doors: false, can_pass_doors: true, can_float: false, can_walk_over_fences: false },
        }
    }

    #[test]
    fn types_table() {
        BlockRegistry::get();
        assert_eq!(state_type(blocks::AIR), PathType::Open);
        assert_eq!(state_type(blocks::STONE), PathType::Blocked);
        assert_eq!(state_type(blocks::WATER), PathType::Water);
        let fence = BlockRegistry::get().state("oak_fence", &[]).unwrap();
        assert_eq!(state_type(fence), PathType::Fence);
        assert!(solid_render(blocks::STONE));
        assert_eq!(block_factors("ice").0, 0.98);
    }

    #[test]
    fn walks_around_a_wall_and_up_a_step() {
        // floor at y=63, wall at x=3 for z in -2..=2, a one-block step at x=6
        let get = |x: i32, y: i32, z: i32| -> Option<BlockState> {
            Some(if y < 64 || (x == 3 && (-2..=2).contains(&z) && y < 67) || (x >= 6 && y == 64) { blocks::STONE } else { blocks::AIR })
        };
        let m = zombie_at([0.5, 64.0, 0.5]);
        let mut nav = Navigation::new(NavKind::Ground);
        let p = nav.create_path(&m, &get, [8, 65, 0], 1, 0).expect("path");
        assert!(p.reached, "{:?}", p.nodes.iter().map(|n| n.pos()).collect::<Vec<_>>());
        assert!(p.nodes.iter().any(|n| n.z.abs() >= 3), "goes around the wall");
        assert!(p.nodes.last().unwrap().y == 65);
        // no path into a sealed box
        let sealed = |x: i32, y: i32, z: i32| -> Option<BlockState> {
            Some(if y < 64 || ((x - 10).abs() <= 1 && (z).abs() <= 1 && y <= 70 && !(x == 10 && z == 0 && (65..=66).contains(&y))) { blocks::STONE } else { blocks::AIR })
        };
        let p = nav.create_path(&m, &sealed, [10, 65, 0], 0, 100);
        assert!(p.is_none_or(|p| !p.reached));
    }
}
