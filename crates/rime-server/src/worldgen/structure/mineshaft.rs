//! Mineshafts (`MineshaftStructure` / `MineshaftPieces`), ported from the
//! 26.1 bytecode together with the `StructurePiece` helpers they use.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use super::template::{bb_center, bb_inside, bb_intersects, mirror_state, rotate_state};
use super::{Bb, Ctx, P};
use crate::registry::{blocks, vstate, BlockRegistry, BlockState};
use crate::worldgen::chunk::{HEIGHT, MIN_Y};
use crate::worldgen::feature::{Hm, Region};
use crate::worldgen::random::{Legacy, Rng, WgRandom};

/// Direction ordinals (vanilla `Direction`): north, south, west, east.
pub const NORTH: u8 = 2;
pub const SOUTH: u8 = 3;
pub const WEST: u8 = 4;
pub const EAST: u8 = 5;

pub enum Kind {
    Room { entrances: Vec<Bb> },
    Corridor { rails: bool, spider: bool, sections: i32, placed_spider: AtomicBool },
    Crossing { dir: u8, two: bool },
    Stairs,
}

pub struct Piece {
    pub bb: Bb,
    /// `orientation` (`None` = null)
    pub orient: Option<u8>,
    pub depth: i32,
    pub kind: Kind,
}

impl Piece {
    pub fn name(&self) -> &'static str {
        match self.kind {
            Kind::Room { .. } => "MSRoom",
            Kind::Corridor { .. } => "MSCorridor",
            Kind::Crossing { .. } => "MSCrossing",
            Kind::Stairs => "MSStairs",
        }
    }

    fn mv(&mut self, d: P) {
        let m = |b: &mut Bb| {
            for i in 0..3 {
                b.0[i] += d[i];
                b.1[i] += d[i];
            }
        };
        m(&mut self.bb);
        if let Kind::Room { entrances } = &mut self.kind {
            entrances.iter_mut().for_each(m);
        }
    }
}

struct States {
    cave_air: BlockState,
    planks: [BlockState; 2],
    wood: [BlockState; 2],
    fence: [BlockState; 2],
    fence_w: [BlockState; 2],
    fence_e: [BlockState; 2],
    chain: BlockState,
    cobweb: BlockState,
    spawner: BlockState,
    rail_ns: BlockState,
    rail_ew: BlockState,
    torch_s: BlockState,
    torch_n: BlockState,
}

fn st() -> &'static States {
    static S: OnceLock<States> = OnceLock::new();
    S.get_or_init(|| {
        let reg = BlockRegistry::get();
        let g = |n: &str, p: &[(&str, &str)]| reg.state(&format!("minecraft:{n}"), p).unwrap_or_else(|| panic!("{n}"));
        States {
            cave_air: g("cave_air", &[]),
            planks: [g("oak_planks", &[]), g("dark_oak_planks", &[])],
            wood: [g("oak_log", &[]), g("dark_oak_log", &[])],
            fence: [g("oak_fence", &[]), g("dark_oak_fence", &[])],
            fence_w: [g("oak_fence", &[("west", "true")]), g("dark_oak_fence", &[("west", "true")])],
            fence_e: [g("oak_fence", &[("east", "true")]), g("dark_oak_fence", &[("east", "true")])],
            chain: g("iron_chain", &[]),
            cobweb: g("cobweb", &[]),
            spawner: g("spawner", &[]),
            rail_ns: g("rail", &[("shape", "north_south")]),
            rail_ew: g("rail", &[("shape", "east_west")]),
            torch_s: g("wall_torch", &[("facing", "south")]),
            torch_n: g("wall_torch", &[("facing", "north")]),
        }
    })
}

fn is_falling_block(s: BlockState) -> bool {
    static F: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    F.get_or_init(|| {
        include_str!("../../../assets/26.1/block_classes.txt")
            .lines()
            .filter(|l| l.split_whitespace().skip(1).any(|c| c == "FallingBlock"))
            .filter_map(|l| l.split_whitespace().next())
            .collect()
    })
    .contains(s.name())
}

fn same_block(a: BlockState, b: BlockState) -> bool {
    a.name() == b.name()
}

// ------------------------------------------------------------ generation

pub struct Mineshaft {
    pub mesa: bool,
    pub pieces: Vec<Piece>,
}

struct Gen<'r> {
    pieces: Vec<Piece>,
    start_min: (i32, i32),
    r: &'r mut Legacy,
}

fn moved(b: Bb, x: i32, y: i32, z: i32) -> Bb {
    ([b.0[0] + x, b.0[1] + y, b.0[2] + z], [b.1[0] + x, b.1[1] + y, b.1[2] + z])
}

fn span(b: &Bb, i: usize) -> i32 {
    b.1[i] - b.0[i] + 1
}

impl Gen<'_> {
    fn collides(&self, b: &Bb) -> bool {
        self.pieces.iter().any(|p| bb_intersects(&p.bb, b))
    }

    /// `MineshaftPieces.generateAndAddPiece`: the child's box when added.
    fn generate_and_add(&mut self, x: i32, y: i32, z: i32, dir: u8, depth: i32) -> Option<Bb> {
        if depth > 8 {
            return None;
        }
        if (x - self.start_min.0).abs() > 80 || (z - self.start_min.1).abs() > 80 {
            return None;
        }
        let p = self.create_random(x, y, z, dir, depth + 1)?;
        let bb = p.bb;
        self.pieces.push(p);
        let i = self.pieces.len() - 1;
        self.add_children(i);
        Some(bb)
    }

    /// `createRandomShaftPiece`
    fn create_random(&mut self, x: i32, y: i32, z: i32, dir: u8, depth: i32) -> Option<Piece> {
        let i = self.r.next_int_bound(100);
        if i >= 80 {
            let h = if self.r.next_int_bound(4) == 0 { 6 } else { 2 };
            let b = match dir {
                NORTH => ([-1, 0, -4], [3, h, 0]),
                SOUTH => ([-1, 0, 0], [3, h, 4]),
                WEST => ([-4, 0, -1], [0, h, 3]),
                _ => ([0, 0, -1], [4, h, 3]),
            };
            let b = moved(b, x, y, z);
            if self.collides(&b) {
                return None;
            }
            Some(Piece { bb: b, orient: None, depth, kind: Kind::Crossing { dir, two: span(&b, 1) > 3 } })
        } else if i >= 70 {
            let b = match dir {
                NORTH => ([0, -5, -8], [2, 2, 0]),
                SOUTH => ([0, -5, 0], [2, 2, 8]),
                WEST => ([-8, -5, 0], [0, 2, 2]),
                _ => ([0, -5, 0], [8, 2, 2]),
            };
            let b = moved(b, x, y, z);
            if self.collides(&b) {
                return None;
            }
            Some(Piece { bb: b, orient: Some(dir), depth, kind: Kind::Stairs })
        } else {
            let mut n = self.r.next_int_bound(3) + 2;
            let mut found = None;
            while n > 0 {
                let len = n * 5;
                let b = match dir {
                    NORTH => ([0, 0, -(len - 1)], [2, 2, 0]),
                    SOUTH => ([0, 0, 0], [2, 2, len - 1]),
                    WEST => ([-(len - 1), 0, 0], [0, 2, 2]),
                    _ => ([0, 0, 0], [len - 1, 2, 2]),
                };
                let b = moved(b, x, y, z);
                if !self.collides(&b) {
                    found = Some(b);
                    break;
                }
                n -= 1;
            }
            let b = found?;
            let rails = self.r.next_int_bound(3) == 0;
            let spider = !rails && self.r.next_int_bound(23) == 0;
            let sections = if dir == NORTH || dir == SOUTH { span(&b, 2) / 5 } else { span(&b, 0) / 5 };
            Some(Piece { bb: b, orient: Some(dir), depth, kind: Kind::Corridor { rails, spider, sections, placed_spider: AtomicBool::new(false) } })
        }
    }

    fn add_children(&mut self, i: usize) {
        let (bb, depth) = (self.pieces[i].bb, self.pieces[i].depth);
        let ([x0, y0, z0], [x1, _, z1]) = bb;
        match self.pieces[i].kind {
            Kind::Room { .. } => {
                let k = (span(&bb, 1) - 3 - 1).max(1);
                let add = |g: &mut Self, e: Bb| {
                    if let Kind::Room { entrances } = &mut g.pieces[i].kind {
                        entrances.push(e);
                    }
                };
                let xs = span(&bb, 0);
                let zs = span(&bb, 2);
                let mut j = 0;
                while j < xs {
                    j += self.r.next_int_bound(xs);
                    if j + 3 > xs {
                        break;
                    }
                    let y = y0 + self.r.next_int_bound(k) + 1;
                    if let Some(c) = self.generate_and_add(x0 + j, y, z0 - 1, NORTH, depth) {
                        add(self, ([c.0[0], c.0[1], z0], [c.1[0], c.1[1], z0 + 1]));
                    }
                    j += 4;
                }
                j = 0;
                while j < xs {
                    j += self.r.next_int_bound(xs);
                    if j + 3 > xs {
                        break;
                    }
                    let y = y0 + self.r.next_int_bound(k) + 1;
                    if let Some(c) = self.generate_and_add(x0 + j, y, z1 + 1, SOUTH, depth) {
                        add(self, ([c.0[0], c.0[1], z1 - 1], [c.1[0], c.1[1], z1]));
                    }
                    j += 4;
                }
                j = 0;
                while j < zs {
                    j += self.r.next_int_bound(zs);
                    if j + 3 > zs {
                        break;
                    }
                    let y = y0 + self.r.next_int_bound(k) + 1;
                    if let Some(c) = self.generate_and_add(x0 - 1, y, z0 + j, WEST, depth) {
                        add(self, ([x0, c.0[1], c.0[2]], [x0 + 1, c.1[1], c.1[2]]));
                    }
                    j += 4;
                }
                j = 0;
                while j < zs {
                    j += self.r.next_int_bound(zs);
                    if j + 3 > zs {
                        break;
                    }
                    let y = y0 + self.r.next_int_bound(k) + 1;
                    if let Some(c) = self.generate_and_add(x1 + 1, y, z0 + j, EAST, depth) {
                        add(self, ([x1 - 1, c.0[1], c.0[2]], [x1, c.1[1], c.1[2]]));
                    }
                    j += 4;
                }
            }
            Kind::Corridor { .. } => {
                let dir = self.pieces[i].orient.unwrap_or(NORTH);
                let j = self.r.next_int_bound(4);
                let y = y0 - 1 + self.r.next_int_bound(3);
                match dir {
                    NORTH => match j {
                        0 | 1 => self.generate_and_add(x0, y, z0 - 1, NORTH, depth),
                        2 => self.generate_and_add(x0 - 1, y, z0, WEST, depth),
                        _ => self.generate_and_add(x1 + 1, y, z0, EAST, depth),
                    },
                    SOUTH => match j {
                        0 | 1 => self.generate_and_add(x0, y, z1 + 1, SOUTH, depth),
                        2 => self.generate_and_add(x0 - 1, y, z1 - 3, WEST, depth),
                        _ => self.generate_and_add(x1 + 1, y, z1 - 3, EAST, depth),
                    },
                    WEST => match j {
                        0 | 1 => self.generate_and_add(x0 - 1, y, z0, WEST, depth),
                        2 => self.generate_and_add(x0, y, z0 - 1, NORTH, depth),
                        _ => self.generate_and_add(x0, y, z1 + 1, SOUTH, depth),
                    },
                    _ => match j {
                        0 | 1 => self.generate_and_add(x1 + 1, y, z0, EAST, depth),
                        2 => self.generate_and_add(x1 - 3, y, z0 - 1, NORTH, depth),
                        _ => self.generate_and_add(x1 - 3, y, z1 + 1, SOUTH, depth),
                    },
                };
                if depth < 8 {
                    if dir == NORTH || dir == SOUTH {
                        let mut k = z0 + 3;
                        while k + 3 <= z1 {
                            match self.r.next_int_bound(5) {
                                0 => {
                                    self.generate_and_add(x0 - 1, y0, k, WEST, depth + 1);
                                }
                                1 => {
                                    self.generate_and_add(x1 + 1, y0, k, EAST, depth + 1);
                                }
                                _ => {}
                            }
                            k += 5;
                        }
                    } else {
                        let mut k = x0 + 3;
                        while k + 3 <= x1 {
                            match self.r.next_int_bound(5) {
                                0 => {
                                    self.generate_and_add(k, y0, z0 - 1, NORTH, depth + 1);
                                }
                                1 => {
                                    self.generate_and_add(k, y0, z1 + 1, SOUTH, depth + 1);
                                }
                                _ => {}
                            }
                            k += 5;
                        }
                    }
                }
            }
            Kind::Crossing { dir, two } => {
                match dir {
                    NORTH => {
                        self.generate_and_add(x0 + 1, y0, z0 - 1, NORTH, depth);
                        self.generate_and_add(x0 - 1, y0, z0 + 1, WEST, depth);
                        self.generate_and_add(x1 + 1, y0, z0 + 1, EAST, depth);
                    }
                    SOUTH => {
                        self.generate_and_add(x0 + 1, y0, z1 + 1, SOUTH, depth);
                        self.generate_and_add(x0 - 1, y0, z0 + 1, WEST, depth);
                        self.generate_and_add(x1 + 1, y0, z0 + 1, EAST, depth);
                    }
                    WEST => {
                        self.generate_and_add(x0 + 1, y0, z0 - 1, NORTH, depth);
                        self.generate_and_add(x0 + 1, y0, z1 + 1, SOUTH, depth);
                        self.generate_and_add(x0 - 1, y0, z0 + 1, WEST, depth);
                    }
                    _ => {
                        self.generate_and_add(x0 + 1, y0, z0 - 1, NORTH, depth);
                        self.generate_and_add(x0 + 1, y0, z1 + 1, SOUTH, depth);
                        self.generate_and_add(x1 + 1, y0, z0 + 1, EAST, depth);
                    }
                }
                if two {
                    if self.r.next_bool() {
                        self.generate_and_add(x0 + 1, y0 + 3 + 1, z0 - 1, NORTH, depth);
                    }
                    if self.r.next_bool() {
                        self.generate_and_add(x0 - 1, y0 + 3 + 1, z0 + 1, WEST, depth);
                    }
                    if self.r.next_bool() {
                        self.generate_and_add(x1 + 1, y0 + 3 + 1, z0 + 1, EAST, depth);
                    }
                    if self.r.next_bool() {
                        self.generate_and_add(x0 + 1, y0 + 3 + 1, z1 + 1, SOUTH, depth);
                    }
                }
            }
            Kind::Stairs => {
                match self.pieces[i].orient.unwrap_or(NORTH) {
                    NORTH => self.generate_and_add(x0, y0, z0 - 1, NORTH, depth),
                    SOUTH => self.generate_and_add(x0, y0, z1 + 1, SOUTH, depth),
                    WEST => self.generate_and_add(x0 - 1, y0, z0, WEST, depth),
                    _ => self.generate_and_add(x1 + 1, y0, z0, EAST, depth),
                };
            }
        }
    }
}

/// `MineshaftStructure.findGenerationPoint`: the pieces and the stub
/// position whose biome must be valid.
pub fn generate(ctx: &dyn Ctx, mesa: bool, cx: i32, cz: i32, r: &mut Legacy) -> (Mineshaft, P) {
    r.next_double();
    let (bx, bz) = (cx * 16 + 2, cz * 16 + 2);
    let room_bb = {
        let x1 = bx + 7 + r.next_int_bound(6);
        let y1 = 54 + r.next_int_bound(6);
        let z1 = bz + 7 + r.next_int_bound(6);
        ([bx, 50, bz], [x1, y1, z1])
    };
    let mut g = Gen { pieces: vec![Piece { bb: room_bb, orient: None, depth: 0, kind: Kind::Room { entrances: Vec::new() } }], start_min: (bx, bz), r };
    g.add_children(0);
    let mut pieces = g.pieces;
    let mut all = pieces[0].bb;
    for p in &pieces[1..] {
        all = super::template::bb_encapsulate(all, &p.bb);
    }
    let sea = 63;
    let k = if mesa {
        let c = bb_center(&all);
        let h = ctx.base_height(c[0], c[2], Hm::NotAir);
        let y = if h <= sea { sea } else { r.next_int_between(sea, h) };
        y - c[1]
    } else {
        // StructurePiecesBuilder.moveBelowSeaLevel(63, minY, random, 10)
        let i = sea - 10;
        let mut j = span(&all, 1) + MIN_Y + 1;
        if j < i {
            j += r.next_int_bound(i - j);
        }
        j - all.1[1]
    };
    for p in &mut pieces {
        p.mv([0, k, 0]);
    }
    let stub = [cx * 16 + 8, 50 + k, cz * 16];
    (Mineshaft { mesa, pieces }, stub)
}

// ------------------------------------------------------------ placement

/// The `StructurePiece` helpers for one piece placed in one chunk box.
struct Pp<'a, 'b> {
    lv: &'a mut Region<'b>,
    cb: Bb,
    p: &'a Piece,
    t: usize,
    blocking: &'a [bool],
}

impl Pp<'_, '_> {
    fn wx(&self, x: i32, z: i32) -> i32 {
        let b = &self.p.bb;
        match self.p.orient {
            Some(NORTH) | Some(SOUTH) => b.0[0] + x,
            Some(WEST) => b.1[0] - z,
            Some(_) => b.0[0] + z,
            None => x,
        }
    }
    fn wy(&self, y: i32) -> i32 {
        match self.p.orient {
            None => y,
            Some(_) => self.p.bb.0[1] + y,
        }
    }
    fn wz(&self, x: i32, z: i32) -> i32 {
        let b = &self.p.bb;
        match self.p.orient {
            Some(NORTH) => b.1[2] - z,
            Some(SOUTH) => b.0[2] + z,
            Some(_) => b.0[2] + x,
            None => z,
        }
    }
    fn pos(&self, x: i32, y: i32, z: i32) -> P {
        [self.wx(x, z), self.wy(y), self.wz(x, z)]
    }
    fn get_block(&self, x: i32, y: i32, z: i32) -> BlockState {
        let p = self.pos(x, y, z);
        if !bb_inside(&self.cb, p) {
            return blocks::AIR;
        }
        self.lv.get(p)
    }
    fn is_interior(&self, x: i32, y: i32, z: i32) -> bool {
        let p = self.pos(x, y + 1, z);
        bb_inside(&self.cb, p) && p[1] < self.lv.height(Hm::Motion, p[0], p[2])
    }
    /// `MineShaftPiece.canBeReplaced`
    fn can_be_replaced(&self, x: i32, y: i32, z: i32) -> bool {
        let s = self.get_block(x, y, z);
        let t = st();
        !same_block(s, t.planks[self.t]) && !same_block(s, t.wood[self.t]) && !same_block(s, t.fence[self.t]) && !same_block(s, t.chain)
    }
    fn place(&mut self, s: BlockState, x: i32, y: i32, z: i32) {
        let p = self.pos(x, y, z);
        if !bb_inside(&self.cb, p) || !self.can_be_replaced(x, y, z) {
            return;
        }
        let (m, r) = match self.p.orient {
            Some(SOUTH) => (1, 0),
            Some(WEST) => (1, 1),
            Some(EAST) => (0, 1),
            _ => (0, 0),
        };
        let s = rotate_state(mirror_state(s, m), r);
        self.lv.set(p, s);
        if vstate::has_fluid(self.lv.get(p)) {
            self.lv.schedule_tick(p);
        }
        if shape_check(s) {
            self.lv.mark_post(p);
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn gen_box(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, edge: BlockState, inner: BlockState, exclude_air: bool) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if exclude_air && vstate::air(self.get_block(x, y, z)) {
                        continue;
                    }
                    let e = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    self.place(if e { edge } else { inner }, x, y, z);
                }
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn maybe_box(&mut self, r: &mut WgRandom, chance: f32, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, edge: BlockState, inner: BlockState, non_air: bool, sky: bool) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if r.next_float() > chance {
                        continue;
                    }
                    if non_air && vstate::air(self.get_block(x, y, z)) {
                        continue;
                    }
                    if sky && !self.is_interior(x, y, z) {
                        continue;
                    }
                    let e = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    self.place(if e { edge } else { inner }, x, y, z);
                }
            }
        }
    }
    fn maybe_block(&mut self, r: &mut WgRandom, chance: f32, x: i32, y: i32, z: i32, s: BlockState) {
        if r.next_float() < chance {
            self.place(s, x, y, z);
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn upper_half_sphere(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, s: BlockState) {
        let f = (x1 - x0 + 1) as f32;
        let g = (y1 - y0 + 1) as f32;
        let h = (z1 - z0 + 1) as f32;
        let i = x0 as f32 + f / 2.0;
        let j = z0 as f32 + h / 2.0;
        for y in y0..=y1 {
            let fy = (y - y0) as f32 / g;
            for x in x0..=x1 {
                let fx = (x as f32 - i) / (f * 0.5);
                for z in z0..=z1 {
                    let fz = (z as f32 - j) / (h * 0.5);
                    if fx * fx + fy * fy + fz * fz <= 1.05 {
                        self.place(s, x, y, z);
                    }
                }
            }
        }
    }
    /// `MineShaftPiece.isInInvalidLocation`
    fn invalid(&self) -> bool {
        let (b, c) = (&self.p.bb, &self.cb);
        let x0 = (b.0[0] - 1).max(c.0[0]);
        let y0 = (b.0[1] - 1).max(c.0[1]);
        let z0 = (b.0[2] - 1).max(c.0[2]);
        let x1 = (b.1[0] + 1).min(c.1[0]);
        let y1 = (b.1[1] + 1).min(c.1[1]);
        let z1 = (b.1[2] + 1).min(c.1[2]);
        let centre = [(x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2];
        if self.blocking[self.lv.biome(centre) as usize] {
            return true;
        }
        let liquid = |x, y, z| vstate::liquid(self.lv.get([x, y, z]));
        for x in x0..=x1 {
            for z in z0..=z1 {
                if liquid(x, y0, z) || liquid(x, y1, z) {
                    return true;
                }
            }
        }
        for x in x0..=x1 {
            for y in y0..=y1 {
                if liquid(x, y, z0) || liquid(x, y, z1) {
                    return true;
                }
            }
        }
        for z in z0..=z1 {
            for y in y0..=y1 {
                if liquid(x0, y, z) || liquid(x1, y, z) {
                    return true;
                }
            }
        }
        false
    }
    /// `setPlanksBlock`
    fn planks_block(&mut self, x: i32, y: i32, z: i32) {
        if !self.is_interior(x, y, z) {
            return;
        }
        let p = self.pos(x, y, z);
        if !vstate::sturdy(self.lv.get(p), 1) {
            self.lv.set(p, st().planks[self.t]);
        }
    }

    // ---- corridor

    fn supporting_box(&self, x0: i32, x1: i32, y: i32, z: i32) -> bool {
        (x0..=x1).all(|x| !vstate::air(self.get_block(x, y + 1, z)))
    }
    #[allow(clippy::too_many_arguments)]
    fn place_support(&mut self, x0: i32, y0: i32, z: i32, y1: i32, x1: i32, r: &mut WgRandom) {
        if !self.supporting_box(x0, x1, y1, z) {
            return;
        }
        let t = st();
        let (planks, air) = (t.planks[self.t], t.cave_air);
        self.gen_box(x0, y0, z, x0, y1 - 1, z, t.fence_w[self.t], air, false);
        self.gen_box(x1, y0, z, x1, y1 - 1, z, t.fence_e[self.t], air, false);
        if r.next_int_bound(4) == 0 {
            self.gen_box(x0, y1, z, x0, y1, z, planks, air, false);
            self.gen_box(x1, y1, z, x1, y1, z, planks, air, false);
        } else {
            self.gen_box(x0, y1, z, x1, y1, z, planks, air, false);
            self.maybe_block(r, 0.05, x0 + 1, y1, z - 1, t.torch_s);
            self.maybe_block(r, 0.05, x0 + 1, y1, z + 1, t.torch_n);
        }
    }
    fn sturdy_neighbours(&self, x: i32, y: i32, z: i32, need: i32) -> bool {
        let p = self.pos(x, y, z);
        let mut n = 0;
        for d in 0..6u8 {
            let s = super::template::dir_step(d);
            let q = [p[0] + s[0], p[1] + s[1], p[2] + s[2]];
            if bb_inside(&self.cb, q) && vstate::sturdy(self.lv.get(q), d ^ 1) {
                n += 1;
                if n >= need {
                    return true;
                }
            }
        }
        false
    }
    fn maybe_cobweb(&mut self, r: &mut WgRandom, chance: f32, x: i32, y: i32, z: i32) {
        if self.is_interior(x, y, z) && r.next_float() < chance && self.sturdy_neighbours(x, y, z, 2) {
            self.place(st().cobweb, x, y, z);
        }
    }
    /// `Corridor.createChest`
    fn chest(&mut self, r: &mut WgRandom, x: i32, y: i32, z: i32) {
        let p = self.pos(x, y, z);
        if bb_inside(&self.cb, p) && vstate::air(self.lv.get(p)) && !vstate::air(self.lv.get([p[0], p[1] - 1, p[2]])) {
            let rail = if r.next_bool() { st().rail_ns } else { st().rail_ew };
            self.place(rail, x, y, z);
            // MinecartChest with the abandoned_mineshaft loot table
            r.next_long();
        }
    }
    fn fill_column(&mut self, s: BlockState, x: i32, z: i32, from: i32, to: i32) {
        for y in from..to {
            self.lv.set([x, y, z], s);
        }
    }
    fn pillar_down_or_chain_up(&mut self, x: i32, y: i32, z: i32) {
        let p = self.pos(x, y, z);
        if !bb_inside(&self.cb, p) {
            return;
        }
        let t = st();
        let i = p[1];
        let mut j = 1;
        let (mut down, mut up) = (true, true);
        while down || up {
            if down {
                let yy = i - j;
                let s = self.lv.get([p[0], yy, p[2]]);
                let flag = replaceable_by_structures(s) && !same_block(s, blocks::LAVA);
                if !flag && vstate::sturdy(s, 1) {
                    self.fill_column(t.wood[self.t], p[0], p[2], i - j + 1, i);
                    return;
                }
                down = j <= 20 && flag && yy > MIN_Y + 1;
            }
            if up {
                let yy = i + j;
                let s = self.lv.get([p[0], yy, p[2]]);
                let flag = replaceable_by_structures(s);
                if !flag && vstate::sturdy(s, 0) && !is_falling_block(s) {
                    self.lv.set([p[0], i + 1, p[2]], t.fence[self.t]);
                    self.fill_column(t.chain, p[0], p[2], i + 2, i + j);
                    return;
                }
                up = j <= 50 && flag && yy < MIN_Y + HEIGHT - 1;
            }
            j += 1;
        }
    }
    fn double_support(&mut self, x: i32, y: i32, z: i32) {
        let planks = st().planks[self.t];
        if same_block(self.get_block(x, y, z), planks) {
            self.pillar_down_or_chain_up(x, y, z);
        }
        if same_block(self.get_block(x + 2, y, z), planks) {
            self.pillar_down_or_chain_up(x + 2, y, z);
        }
    }

    fn post_process(&mut self, r: &mut WgRandom) {
        if self.invalid() {
            return;
        }
        let t = st();
        let air = t.cave_air;
        let ([x0, y0, z0], [x1, y1, z1]) = self.p.bb;
        match &self.p.kind {
            Kind::Room { entrances } => {
                self.gen_box(x0, y0 + 1, z0, x1, (y0 + 3).min(y1), z1, air, air, false);
                for b in entrances {
                    self.gen_box(b.0[0], b.1[1] - 2, b.0[2], b.1[0], b.1[1], b.1[2], air, air, false);
                }
                self.upper_half_sphere(x0, y0 + 4, z0, x1, y1, z1, air);
            }
            Kind::Stairs => {
                self.gen_box(0, 5, 0, 2, 7, 1, air, air, false);
                self.gen_box(0, 0, 7, 2, 2, 8, air, air, false);
                for i in 0..5 {
                    self.gen_box(0, 5 - i - if i < 4 { 1 } else { 0 }, 2 + i, 2, 7 - i, 2 + i, air, air, false);
                }
            }
            Kind::Crossing { two, .. } => {
                let planks = t.planks[self.t];
                if *two {
                    self.gen_box(x0 + 1, y0, z0, x1 - 1, y0 + 2, z1, air, air, false);
                    self.gen_box(x0, y0, z0 + 1, x1, y0 + 2, z1 - 1, air, air, false);
                    self.gen_box(x0 + 1, y1 - 2, z0, x1 - 1, y1, z1, air, air, false);
                    self.gen_box(x0, y1 - 2, z0 + 1, x1, y1, z1 - 1, air, air, false);
                    self.gen_box(x0 + 1, y0 + 3, z0 + 1, x1 - 1, y0 + 3, z1 - 1, air, air, false);
                } else {
                    self.gen_box(x0 + 1, y0, z0, x1 - 1, y1, z1, air, air, false);
                    self.gen_box(x0, y0, z0 + 1, x1, y1, z1 - 1, air, air, false);
                }
                for (x, z) in [(x0 + 1, z0 + 1), (x0 + 1, z1 - 1), (x1 - 1, z0 + 1), (x1 - 1, z1 - 1)] {
                    if !vstate::air(self.get_block(x, y1 + 1, z)) {
                        self.gen_box(x, y0, z, x, y1, z, planks, air, false);
                    }
                }
                for x in x0..=x1 {
                    for z in z0..=z1 {
                        self.planks_block(x, y0 - 1, z);
                    }
                }
            }
            Kind::Corridor { rails, spider, sections, placed_spider } => {
                let (rails, spider, sections) = (*rails, *spider, *sections);
                let m = sections * 5 - 1;
                self.gen_box(0, 0, 0, 2, 1, m, air, air, false);
                self.maybe_box(r, 0.8, 0, 2, 0, 2, 2, m, air, air, false, false);
                if spider {
                    self.maybe_box(r, 0.6, 0, 0, 0, 2, 1, m, t.cobweb, air, false, true);
                }
                for s in 0..sections {
                    let z = 2 + s * 5;
                    self.place_support(0, 0, z, 2, 2, r);
                    self.maybe_cobweb(r, 0.1, 0, 2, z - 1);
                    self.maybe_cobweb(r, 0.1, 2, 2, z - 1);
                    self.maybe_cobweb(r, 0.1, 0, 2, z + 1);
                    self.maybe_cobweb(r, 0.1, 2, 2, z + 1);
                    self.maybe_cobweb(r, 0.05, 0, 2, z - 2);
                    self.maybe_cobweb(r, 0.05, 2, 2, z - 2);
                    self.maybe_cobweb(r, 0.05, 0, 2, z + 2);
                    self.maybe_cobweb(r, 0.05, 2, 2, z + 2);
                    if r.next_int_bound(100) == 0 {
                        self.chest(r, 2, 0, z - 1);
                    }
                    if r.next_int_bound(100) == 0 {
                        self.chest(r, 0, 0, z + 1);
                    }
                    if spider && !placed_spider.load(Ordering::Relaxed) {
                        let zz = z - 1 + r.next_int_bound(3);
                        let p = self.pos(1, 0, zz);
                        if bb_inside(&self.cb, p) && self.is_interior(1, 0, zz) {
                            placed_spider.store(true, Ordering::Relaxed);
                            self.lv.set(p, t.spawner);
                        }
                    }
                }
                for x in 0..=2 {
                    for z in 0..=m {
                        self.planks_block(x, -1, z);
                    }
                }
                self.double_support(0, -1, 2);
                if sections > 1 {
                    self.double_support(0, -1, m - 2);
                }
                if rails {
                    for z in 0..=m {
                        let s = self.get_block(1, -1, z);
                        if !vstate::air(s) && vstate::solid_render(s) {
                            let f = if self.is_interior(1, 0, z) { 0.7 } else { 0.9 };
                            self.maybe_block(r, f, 1, 0, z, t.rail_ns);
                        }
                    }
                }
            }
        }
    }
}

/// `StructurePiece.isReplaceableByStructures`
fn replaceable_by_structures(s: BlockState) -> bool {
    vstate::air(s) || vstate::liquid(s) || matches!(s.name(), "glow_lichen" | "seagrass" | "tall_seagrass")
}

/// `StructurePiece.SHAPE_CHECK_BLOCKS`
pub fn shape_check(s: BlockState) -> bool {
    matches!(
        s.name(),
        "nether_brick_fence" | "torch" | "wall_torch" | "oak_fence" | "spruce_fence" | "dark_oak_fence" | "pale_oak_fence" | "acacia_fence" | "birch_fence" | "jungle_fence" | "ladder" | "iron_bars"
    )
}

impl Mineshaft {
    /// `StructureStart.placeInChunk` for the pieces reaching `cb`.
    pub fn place(&self, lv: &mut Region, r: &mut WgRandom, cb: &Bb, blocking: &[bool]) {
        for p in &self.pieces {
            if bb_intersects(&p.bb, cb) {
                Pp { lv, cb: *cb, p, t: self.mesa as usize, blocking }.post_process(r);
            }
        }
    }
}
