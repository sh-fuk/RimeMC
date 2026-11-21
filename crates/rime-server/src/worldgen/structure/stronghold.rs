//! Strongholds (`StrongholdPieces`): piece graph generation ported by hand,
//! the `pp_*` / `m_*` block placement functions generated from the 26.1
//! bytecode by the stack-machine translator.

#![allow(clippy::all, unused_parens, unused_mut, unused_variables, unused_assignments, non_snake_case)]

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use super::legacy::{LLib, LPiece};
use super::piece::{self, state, with, Selector, Sp, EAST, NORTH, SOUTH, WEST};
use super::placement::large_feature_seed;
use super::template::{bb_inside, bb_intersects};
use super::{Bb, P};
use crate::registry::{blocks, BlockState};
use crate::worldgen::chunk::MIN_Y;
use crate::worldgen::feature::Region;
use crate::worldgen::random::{Legacy, Rng};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SK {
    Straight,
    PrisonHall,
    LeftTurn,
    RightTurn,
    RoomCrossing,
    StraightStairsDown,
    StairsDown,
    FiveCrossing,
    ChestCorridor,
    Library,
    PortalRoom,
    FillerCorridor,
}

pub struct SPiece {
    pub kind: SK,
    pub bb: Bb,
    pub orient: u8,
    pub depth: i32,
    /// `entryDoor` ordinal: OPENING, WOOD_DOOR, GRATES, IRON_DOOR
    pub door: u8,
    pub f: [bool; 4],
    pub typ: i32,
    pub steps: i32,
    pub placed: AtomicBool,
}

impl SPiece {
    fn new(kind: SK, bb: Bb, orient: u8, depth: i32) -> SPiece {
        SPiece { kind, bb, orient, depth, door: 0, f: [false; 4], typ: 0, steps: 0, placed: AtomicBool::new(false) }
    }
}

/// `StructurePiece.BlockSelector` of strongholds
pub struct SmoothStone;

impl Selector for SmoothStone {
    fn next(&self, r: &mut dyn Rng, _x: i32, _y: i32, _z: i32, edge: bool) -> BlockState {
        if !edge {
            return state("cave_air", &[]);
        }
        let f = r.next_float();
        if f < 0.2 {
            state("cracked_stone_bricks", &[])
        } else if f < 0.5 {
            state("mossy_stone_bricks", &[])
        } else if f < 0.55 {
            state("infested_stone_bricks", &[])
        } else {
            state("stone_bricks", &[])
        }
    }
}

/// `BoundingBox.orientBox`
#[allow(clippy::too_many_arguments)]
pub fn orient_box(x: i32, y: i32, z: i32, ox: i32, oy: i32, oz: i32, w: i32, h: i32, d: i32, dir: u8) -> Bb {
    match dir {
        SOUTH => ([x + ox, y + oy, z + oz], [x + w - 1 + ox, y + h - 1 + oy, z + d - 1 + oz]),
        WEST => ([x - d + 1 + oz, y + oy, z + ox], [x + oz, y + h - 1 + oy, z + w - 1 + ox]),
        EAST => ([x + oz, y + oy, z + ox], [x + d - 1 + oz, y + h - 1 + oy, z + w - 1 + ox]),
        _ => ([x + ox, y + oy, z - d + 1 + oz], [x + w - 1 + ox, y + h - 1 + oy, z + oz]),
    }
}

struct Weight {
    kind: SK,
    weight: i32,
    max: i32,
    count: i32,
}

impl Weight {
    fn valid(&self) -> bool {
        self.max == 0 || self.count < self.max
    }
    fn do_place(&self, depth: i32) -> bool {
        self.valid()
            && match self.kind {
                SK::Library => depth > 4,
                SK::PortalRoom => depth > 5,
                _ => true,
            }
    }
}

struct Gen<'r> {
    pieces: Vec<SPiece>,
    pending: Vec<usize>,
    weights: Vec<Weight>,
    total: i32,
    imposed: Option<SK>,
    previous: Option<SK>,
    portal: bool,
    r: &'r mut Legacy,
}

impl Gen<'_> {
    fn collision(&self, b: &Bb) -> Option<usize> {
        self.pieces.iter().position(|p| bb_intersects(&p.bb, b))
    }

    fn door(&mut self) -> u8 {
        match self.r.next_int_bound(5) {
            2 => 1,
            3 => 2,
            4 => 3,
            _ => 0,
        }
    }

    /// `X.createPiece`
    fn create(&mut self, kind: SK, x: i32, y: i32, z: i32, dir: u8, depth: i32) -> Option<SPiece> {
        let ob = |ox, oy, oz, w, h, d| orient_box(x, y, z, ox, oy, oz, w, h, d, dir);
        let ok = |s: &Self, b: &Bb| b.0[1] > 10 && s.collision(b).is_none();
        let bb = match kind {
            SK::Straight | SK::ChestCorridor => ob(-1, -1, 0, 5, 5, 7),
            SK::PrisonHall => ob(-1, -1, 0, 9, 5, 11),
            SK::LeftTurn | SK::RightTurn => ob(-1, -1, 0, 5, 5, 5),
            SK::RoomCrossing => ob(-4, -1, 0, 11, 7, 11),
            SK::StraightStairsDown => ob(-1, -7, 0, 5, 11, 8),
            SK::StairsDown => ob(-1, -7, 0, 5, 11, 5),
            SK::FiveCrossing => ob(-4, -3, 0, 10, 9, 11),
            SK::Library => {
                let tall = ob(-4, -1, 0, 14, 11, 15);
                if ok(self, &tall) {
                    tall
                } else {
                    ob(-4, -1, 0, 14, 6, 15)
                }
            }
            SK::PortalRoom => ob(-4, -1, 0, 11, 8, 16),
            SK::FillerCorridor => return None,
        };
        if !ok(self, &bb) {
            return None;
        }
        let mut p = SPiece::new(kind, bb, dir, depth);
        match kind {
            SK::PortalRoom => {}
            SK::Straight => {
                p.door = self.door();
                p.f[0] = self.r.next_int_bound(2) == 0;
                p.f[1] = self.r.next_int_bound(2) == 0;
            }
            SK::RoomCrossing => {
                p.door = self.door();
                p.typ = self.r.next_int_bound(5);
            }
            SK::FiveCrossing => {
                p.door = self.door();
                p.f[0] = self.r.next_bool();
                p.f[1] = self.r.next_bool();
                p.f[2] = self.r.next_bool();
                p.f[3] = self.r.next_int_bound(3) > 0;
            }
            SK::Library => {
                p.door = self.door();
                p.f[0] = bb.1[1] - bb.0[1] + 1 > 6;
            }
            _ => p.door = self.door(),
        }
        Some(p)
    }

    fn update_weights(&mut self) -> bool {
        let mut flag = false;
        self.total = 0;
        for w in &self.weights {
            if w.max > 0 && w.count < w.max {
                flag = true;
            }
            self.total += w.weight;
        }
        flag
    }

    /// `generatePieceFromSmallDoor`
    fn from_small_door(&mut self, x: i32, y: i32, z: i32, dir: u8, depth: i32) -> Option<SPiece> {
        if !self.update_weights() {
            return None;
        }
        if let Some(k) = self.imposed.take() {
            if let Some(p) = self.create(k, x, y, z, dir, depth) {
                return Some(p);
            }
        }
        let mut i = 0;
        while i < 5 {
            i += 1;
            let mut j = self.r.next_int_bound(self.total);
            let mut n = 0;
            while n < self.weights.len() {
                j -= self.weights[n].weight;
                if j < 0 {
                    let w = &self.weights[n];
                    if !w.do_place(depth) || Some(w.kind) == self.previous {
                        break;
                    }
                    let k = w.kind;
                    if let Some(p) = self.create(k, x, y, z, dir, depth) {
                        self.weights[n].count += 1;
                        self.previous = Some(k);
                        if !self.weights[n].valid() {
                            self.weights.remove(n);
                        }
                        return Some(p);
                    }
                }
                n += 1;
            }
        }
        // FillerCorridor.findPieceBox
        let bb = orient_box(x, y, z, -1, -1, 0, 5, 5, 4, dir);
        let c = self.collision(&bb)?;
        let cb = self.pieces[c].bb;
        if cb.0[1] == bb.0[1] {
            for j in (1..=2).rev() {
                let b = orient_box(x, y, z, -1, -1, 0, 5, 5, j, dir);
                if !bb_intersects(&cb, &b) {
                    let fb = orient_box(x, y, z, -1, -1, 0, 5, 5, j + 1, dir);
                    if fb.0[1] > 1 {
                        let mut p = SPiece::new(SK::FillerCorridor, fb, dir, depth);
                        p.steps = if dir == NORTH || dir == SOUTH { fb.1[2] - fb.0[2] + 1 } else { fb.1[0] - fb.0[0] + 1 };
                        return Some(p);
                    }
                    return None;
                }
            }
        }
        None
    }

    fn generate_and_add(&mut self, x: i32, y: i32, z: i32, dir: u8, depth: i32) {
        if depth > 50 {
            return;
        }
        let s = self.pieces[0].bb.0;
        if (x - s[0]).abs() > 112 || (z - s[2]).abs() > 112 {
            return;
        }
        if let Some(p) = self.from_small_door(x, y, z, dir, depth + 1) {
            self.pieces.push(p);
            self.pending.push(self.pieces.len() - 1);
        }
    }

    fn forward(&mut self, i: usize, xo: i32, yo: i32) {
        let (b, o, d) = (self.pieces[i].bb, self.pieces[i].orient, self.pieces[i].depth);
        match o {
            NORTH => self.generate_and_add(b.0[0] + xo, b.0[1] + yo, b.0[2] - 1, o, d),
            SOUTH => self.generate_and_add(b.0[0] + xo, b.0[1] + yo, b.1[2] + 1, o, d),
            WEST => self.generate_and_add(b.0[0] - 1, b.0[1] + yo, b.0[2] + xo, o, d),
            _ => self.generate_and_add(b.1[0] + 1, b.0[1] + yo, b.0[2] + xo, o, d),
        }
    }

    fn left(&mut self, i: usize, yo: i32, ho: i32) {
        let (b, o, d) = (self.pieces[i].bb, self.pieces[i].orient, self.pieces[i].depth);
        match o {
            NORTH | SOUTH => self.generate_and_add(b.0[0] - 1, b.0[1] + yo, b.0[2] + ho, WEST, d),
            _ => self.generate_and_add(b.0[0] + ho, b.0[1] + yo, b.0[2] - 1, NORTH, d),
        }
    }

    fn right(&mut self, i: usize, yo: i32, ho: i32) {
        let (b, o, d) = (self.pieces[i].bb, self.pieces[i].orient, self.pieces[i].depth);
        match o {
            NORTH | SOUTH => self.generate_and_add(b.1[0] + 1, b.0[1] + yo, b.0[2] + ho, EAST, d),
            _ => self.generate_and_add(b.0[0] + ho, b.0[1] + yo, b.1[2] + 1, SOUTH, d),
        }
    }

    fn add_children(&mut self, i: usize) {
        let p = &self.pieces[i];
        let (kind, o, f) = (p.kind, p.orient, p.f);
        match kind {
            SK::Straight => {
                self.forward(i, 1, 1);
                if f[0] {
                    self.left(i, 1, 2);
                }
                if f[1] {
                    self.right(i, 1, 2);
                }
            }
            SK::PrisonHall | SK::ChestCorridor | SK::StraightStairsDown => self.forward(i, 1, 1),
            SK::LeftTurn => {
                if o == NORTH || o == EAST {
                    self.left(i, 1, 1)
                } else {
                    self.right(i, 1, 1)
                }
            }
            SK::RightTurn => {
                if o == NORTH || o == EAST {
                    self.right(i, 1, 1)
                } else {
                    self.left(i, 1, 1)
                }
            }
            SK::RoomCrossing => {
                self.forward(i, 4, 1);
                self.left(i, 1, 4);
                self.right(i, 1, 4);
            }
            SK::StairsDown => {
                if f[0] {
                    self.imposed = Some(SK::FiveCrossing);
                }
                self.forward(i, 1, 1);
            }
            SK::FiveCrossing => {
                let (mut a, mut b) = (3, 5);
                if o == WEST || o == NORTH {
                    a = 8 - a;
                    b = 8 - b;
                }
                self.forward(i, 5, 1);
                if f[0] {
                    self.left(i, a, 1);
                }
                if f[1] {
                    self.left(i, b, 7);
                }
                if f[2] {
                    self.right(i, a, 1);
                }
                if f[3] {
                    self.right(i, b, 7);
                }
            }
            SK::PortalRoom => self.portal = true,
            SK::Library | SK::FillerCorridor => {}
        }
    }
}

fn weights() -> Vec<Weight> {
    use SK::*;
    [(Straight, 40, 0), (PrisonHall, 5, 5), (LeftTurn, 20, 0), (RightTurn, 20, 0), (RoomCrossing, 10, 6), (StraightStairsDown, 5, 5), (StairsDown, 5, 5), (FiveCrossing, 5, 4), (ChestCorridor, 5, 4), (Library, 10, 2), (PortalRoom, 20, 1)]
        .into_iter()
        .map(|(kind, weight, max)| Weight { kind, weight, max, count: 0 })
        .collect()
}

/// `StrongholdStructure.generatePieces`
pub fn generate(seed: i64, cx: i32, cz: i32) -> Vec<SPiece> {
    let mut i: i64 = 0;
    loop {
        let mut r = large_feature_seed(seed.wrapping_add(i), cx, cz);
        i += 1;
        let dir = piece::random_horizontal(&mut r);
        let mut start = SPiece::new(SK::StairsDown, piece::make_bb(cx * 16 + 2, 64, cz * 16 + 2, dir, 5, 11, 5), dir, 0);
        start.f[0] = true;
        let mut g = Gen { pieces: vec![start], pending: Vec::new(), weights: weights(), total: 0, imposed: None, previous: None, portal: false, r: &mut r };
        g.add_children(0);
        while !g.pending.is_empty() {
            let k = g.r.next_int_bound(g.pending.len() as i32) as usize;
            let p = g.pending.remove(k);
            g.add_children(p);
        }
        let portal = g.portal;
        let mut pieces = g.pieces;
        // moveBelowSeaLevel(63, minY, random, 10)
        let mut all = pieces[0].bb;
        for p in &pieces[1..] {
            all = super::template::bb_encapsulate(all, &p.bb);
        }
        let top = 63 - 10;
        let mut j = all.1[1] - all.0[1] + 1 + MIN_Y + 1;
        if j < top {
            j += r.next_int_bound(top - j);
        }
        let k = j - all.1[1];
        for p in &mut pieces {
            p.bb.0[1] += k;
            p.bb.1[1] += k;
        }
        if portal {
            return pieces;
        }
    }
}

impl LPiece for SPiece {
    fn bb(&self) -> Bb {
        self.bb
    }
    fn name(&self) -> String {
        format!("stronghold/{:?}", self.kind)
    }
    fn post_process(&self, lv: &mut Region, _lib: &LLib, seed: i64, r: &mut dyn Rng, cb: &Bb, reference: P) {
        let mut lr = Legacy::new(seed ^ ((self.bb.0[0] as i64) << 20) ^ self.bb.0[2] as i64);
        let mut sp = Sp { lv, cb: *cb, bb: self.bb, orient: Some(self.orient) };
        let (sp, lr) = (&mut sp, &mut lr);
        match self.kind {
            SK::Straight => pp_straight(self, sp, r, lr, reference),
            SK::PrisonHall => pp_prison_hall(self, sp, r, lr, reference),
            SK::LeftTurn => pp_left_turn(self, sp, r, lr, reference),
            SK::RightTurn => pp_right_turn(self, sp, r, lr, reference),
            SK::RoomCrossing => pp_room_crossing(self, sp, r, lr, reference),
            SK::StraightStairsDown => pp_straight_stairs_down(self, sp, r, lr, reference),
            SK::StairsDown => pp_stairs_down(self, sp, r, lr, reference),
            SK::FiveCrossing => pp_five_crossing(self, sp, r, lr, reference),
            SK::ChestCorridor => pp_chest_corridor(self, sp, r, lr, reference),
            SK::Library => pp_library(self, sp, r, lr, reference),
            SK::PortalRoom => pp_portal_room(self, sp, r, lr, reference),
            SK::FillerCorridor => pp_filler_corridor(self, sp, r, lr, reference),
        }
    }
}

// ------------------------------------------------------------ generated

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_generate_small_door(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a3: u8, a4: i32, a5: i32, a6: i32) {
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    pc = match (a3 as i32) { 0 => 36, 1 => 72, 2 => 273, 3 => 592, _ => 862 };
    }
    36 => {
    sp.gen_box(a4, a5, a6, ((a4 + 3) - 1), ((a5 + 3) - 1), a6, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 862;
    }
    72 => {
    sp.place(state("stone_bricks", &[]), a4, a5, a6);
    sp.place(state("stone_bricks", &[]), a4, (a5 + 1), a6);
    sp.place(state("stone_bricks", &[]), a4, (a5 + 2), a6);
    sp.place(state("stone_bricks", &[]), (a4 + 1), (a5 + 2), a6);
    sp.place(state("stone_bricks", &[]), (a4 + 2), (a5 + 2), a6);
    sp.place(state("stone_bricks", &[]), (a4 + 2), (a5 + 1), a6);
    sp.place(state("stone_bricks", &[]), (a4 + 2), a5, a6);
    sp.place(state("oak_door", &[]), (a4 + 1), a5, a6);
    sp.place(with(state("oak_door", &[]), "half", "upper"), (a4 + 1), (a5 + 1), a6);
    pc = 862;
    }
    273 => {
    sp.place(state("cave_air", &[]), (a4 + 1), a5, a6);
    sp.place(state("cave_air", &[]), (a4 + 1), (a5 + 1), a6);
    sp.place(with(state("iron_bars", &[]), "west", if (1 != 0) { "true" } else { "false" }), a4, a5, a6);
    sp.place(with(state("iron_bars", &[]), "west", if (1 != 0) { "true" } else { "false" }), a4, (a5 + 1), a6);
    sp.place(with(with(state("iron_bars", &[]), "east", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), a4, (a5 + 2), a6);
    sp.place(with(with(state("iron_bars", &[]), "east", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), (a4 + 1), (a5 + 2), a6);
    sp.place(with(with(state("iron_bars", &[]), "east", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), (a4 + 2), (a5 + 2), a6);
    sp.place(with(state("iron_bars", &[]), "east", if (1 != 0) { "true" } else { "false" }), (a4 + 2), (a5 + 1), a6);
    sp.place(with(state("iron_bars", &[]), "east", if (1 != 0) { "true" } else { "false" }), (a4 + 2), a5, a6);
    pc = 862;
    }
    592 => {
    sp.place(state("stone_bricks", &[]), a4, a5, a6);
    sp.place(state("stone_bricks", &[]), a4, (a5 + 1), a6);
    sp.place(state("stone_bricks", &[]), a4, (a5 + 2), a6);
    sp.place(state("stone_bricks", &[]), (a4 + 1), (a5 + 2), a6);
    sp.place(state("stone_bricks", &[]), (a4 + 2), (a5 + 2), a6);
    sp.place(state("stone_bricks", &[]), (a4 + 2), (a5 + 1), a6);
    sp.place(state("stone_bricks", &[]), (a4 + 2), a5, a6);
    sp.place(state("iron_door", &[]), (a4 + 1), a5, a6);
    sp.place(with(state("iron_door", &[]), "half", "upper"), (a4 + 1), (a5 + 1), a6);
    sp.place(with(state("stone_button", &[]), "facing", piece::dir_name(piece::NORTH)), (a4 + 2), (a5 + 1), (a6 + 1));
    sp.place(with(state("stone_button", &[]), "facing", piece::dir_name(piece::SOUTH)), (a4 + 2), (a5 + 1), (a6 - 1));
    pc = 862;
    }
    862 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_straight(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box_sel(0, 0, 0, 4, 4, 6, (1 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, t.door, 1, 1, 0);
    m_generate_small_door(t, sp, r, lr, 0, 1, 1, 6);
    a8_BlockState = with(state("wall_torch", &[]), "facing", piece::dir_name(piece::EAST));
    a9_BlockState = with(state("wall_torch", &[]), "facing", piece::dir_name(piece::WEST));
    sp.maybe_block(r, 0.1_f32, 1, 2, 1, a8_BlockState);
    sp.maybe_block(r, 0.1_f32, 3, 2, 1, a9_BlockState);
    sp.maybe_block(r, 0.1_f32, 1, 2, 5, a8_BlockState);
    sp.maybe_block(r, 0.1_f32, 3, 2, 5, a9_BlockState);
    pc = if !(t.f[0]) { 183 } else { 163 };
    }
    163 => {
    sp.gen_box(0, 1, 2, 0, 3, 4, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 183;
    }
    183 => {
    pc = if !(t.f[1]) { 210 } else { 190 };
    }
    190 => {
    sp.gen_box(4, 1, 2, 4, 3, 4, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 210;
    }
    210 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_prison_hall(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l8: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box_sel(0, 0, 0, 8, 4, 10, (1 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, t.door, 1, 1, 0);
    sp.gen_box(1, 1, 10, 3, 3, 10, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    sp.gen_box_sel(4, 1, 1, 4, 3, 1, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(4, 1, 3, 4, 3, 3, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(4, 1, 7, 4, 3, 7, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(4, 1, 9, 4, 3, 9, (0 != 0), r, &SmoothStone);
    l8 = 1;
    pc = 142;
    }
    142 => {
    pc = if l8 > 3 { 428 } else { 148 };
    }
    148 => {
    sp.place(with(with(state("iron_bars", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), 4, l8, 4);
    sp.place(with(with(with(state("iron_bars", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), 4, l8, 5);
    sp.place(with(with(state("iron_bars", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), 4, l8, 6);
    sp.place(with(with(state("iron_bars", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), 5, l8, 5);
    sp.place(with(with(state("iron_bars", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), 6, l8, 5);
    sp.place(with(with(state("iron_bars", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), 7, l8, 5);
    l8 += 1;
    pc = 142;
    }
    428 => {
    sp.place(with(with(state("iron_bars", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), 4, 3, 2);
    sp.place(with(with(state("iron_bars", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), 4, 3, 8);
    a8_BlockState = with(state("iron_door", &[]), "facing", piece::dir_name(piece::WEST));
    a9_BlockState = with(with(state("iron_door", &[]), "facing", piece::dir_name(piece::WEST)), "half", "upper");
    sp.place(a8_BlockState, 4, 1, 2);
    sp.place(a9_BlockState, 4, 2, 2);
    sp.place(a8_BlockState, 4, 1, 8);
    sp.place(a9_BlockState, 4, 2, 8);
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_left_turn(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_u8: u8 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box_sel(0, 0, 0, 4, 4, 4, (1 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, t.door, 1, 1, 0);
    a8_u8 = sp.orient.unwrap_or(piece::NORTH);
    pc = if a8_u8 == piece::NORTH { 57 } else { 49 };
    }
    49 => {
    pc = if a8_u8 != piece::EAST { 80 } else { 57 };
    }
    57 => {
    sp.gen_box(0, 1, 1, 0, 3, 3, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 100;
    }
    80 => {
    sp.gen_box(4, 1, 1, 4, 3, 3, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 100;
    }
    100 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_right_turn(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_u8: u8 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box_sel(0, 0, 0, 4, 4, 4, (1 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, t.door, 1, 1, 0);
    a8_u8 = sp.orient.unwrap_or(piece::NORTH);
    pc = if a8_u8 == piece::NORTH { 57 } else { 49 };
    }
    49 => {
    pc = if a8_u8 != piece::EAST { 80 } else { 57 };
    }
    57 => {
    sp.gen_box(4, 1, 1, 4, 3, 3, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 100;
    }
    80 => {
    sp.gen_box(0, 1, 1, 0, 3, 3, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 100;
    }
    100 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_room_crossing(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut l8: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box_sel(0, 0, 0, 10, 6, 10, (1 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, t.door, 4, 1, 0);
    sp.gen_box(4, 1, 10, 6, 3, 10, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    sp.gen_box(0, 1, 4, 0, 3, 6, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    sp.gen_box(10, 1, 4, 10, 3, 6, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = match t.typ { 0 => 139, 1 => 438, 2 => 598, _ => 136 };
    }
    136 => {
    pc = 1162;
    }
    139 => {
    sp.place(state("stone_bricks", &[]), 5, 1, 5);
    sp.place(state("stone_bricks", &[]), 5, 2, 5);
    sp.place(state("stone_bricks", &[]), 5, 3, 5);
    sp.place(with(state("wall_torch", &[]), "facing", piece::dir_name(piece::WEST)), 4, 3, 5);
    sp.place(with(state("wall_torch", &[]), "facing", piece::dir_name(piece::EAST)), 6, 3, 5);
    sp.place(with(state("wall_torch", &[]), "facing", piece::dir_name(piece::SOUTH)), 5, 3, 4);
    sp.place(with(state("wall_torch", &[]), "facing", piece::dir_name(piece::NORTH)), 5, 3, 6);
    sp.place(state("smooth_stone_slab", &[]), 4, 1, 4);
    sp.place(state("smooth_stone_slab", &[]), 4, 1, 5);
    sp.place(state("smooth_stone_slab", &[]), 4, 1, 6);
    sp.place(state("smooth_stone_slab", &[]), 6, 1, 4);
    sp.place(state("smooth_stone_slab", &[]), 6, 1, 5);
    sp.place(state("smooth_stone_slab", &[]), 6, 1, 6);
    sp.place(state("smooth_stone_slab", &[]), 5, 1, 4);
    sp.place(state("smooth_stone_slab", &[]), 5, 1, 6);
    pc = 1162;
    }
    438 => {
    l8 = 0;
    pc = 441;
    }
    441 => {
    pc = if l8 >= 5 { 531 } else { 447 };
    }
    447 => {
    sp.place(state("stone_bricks", &[]), 3, 1, (3 + l8));
    sp.place(state("stone_bricks", &[]), 7, 1, (3 + l8));
    sp.place(state("stone_bricks", &[]), (3 + l8), 1, 3);
    sp.place(state("stone_bricks", &[]), (3 + l8), 1, 7);
    l8 += 1;
    pc = 441;
    }
    531 => {
    sp.place(state("stone_bricks", &[]), 5, 1, 5);
    sp.place(state("stone_bricks", &[]), 5, 2, 5);
    sp.place(state("stone_bricks", &[]), 5, 3, 5);
    sp.place(state("water", &[]), 5, 4, 5);
    pc = 1162;
    }
    598 => {
    l8 = 1;
    pc = 601;
    }
    601 => {
    pc = if l8 > 9 { 649 } else { 608 };
    }
    608 => {
    sp.place(state("cobblestone", &[]), 1, 3, l8);
    sp.place(state("cobblestone", &[]), 9, 3, l8);
    l8 += 1;
    pc = 601;
    }
    649 => {
    l8 = 1;
    pc = 652;
    }
    652 => {
    pc = if l8 > 9 { 700 } else { 659 };
    }
    659 => {
    sp.place(state("cobblestone", &[]), l8, 3, 1);
    sp.place(state("cobblestone", &[]), l8, 3, 9);
    l8 += 1;
    pc = 652;
    }
    700 => {
    sp.place(state("cobblestone", &[]), 5, 1, 4);
    sp.place(state("cobblestone", &[]), 5, 1, 6);
    sp.place(state("cobblestone", &[]), 5, 3, 4);
    sp.place(state("cobblestone", &[]), 5, 3, 6);
    sp.place(state("cobblestone", &[]), 4, 1, 5);
    sp.place(state("cobblestone", &[]), 6, 1, 5);
    sp.place(state("cobblestone", &[]), 4, 3, 5);
    sp.place(state("cobblestone", &[]), 6, 3, 5);
    l8 = 1;
    pc = 835;
    }
    835 => {
    pc = if l8 > 3 { 919 } else { 841 };
    }
    841 => {
    sp.place(state("cobblestone", &[]), 4, l8, 4);
    sp.place(state("cobblestone", &[]), 6, l8, 4);
    sp.place(state("cobblestone", &[]), 4, l8, 6);
    sp.place(state("cobblestone", &[]), 6, l8, 6);
    l8 += 1;
    pc = 835;
    }
    919 => {
    sp.place(state("wall_torch", &[]), 5, 3, 5);
    l8 = 2;
    pc = 938;
    }
    938 => {
    pc = if l8 > 8 { 1086 } else { 945 };
    }
    945 => {
    sp.place(state("oak_planks", &[]), 2, 3, l8);
    sp.place(state("oak_planks", &[]), 3, 3, l8);
    pc = if l8 <= 3 { 992 } else { 985 };
    }
    985 => {
    pc = if l8 < 7 { 1044 } else { 992 };
    }
    992 => {
    sp.place(state("oak_planks", &[]), 4, 3, l8);
    sp.place(state("oak_planks", &[]), 5, 3, l8);
    sp.place(state("oak_planks", &[]), 6, 3, l8);
    pc = 1044;
    }
    1044 => {
    sp.place(state("oak_planks", &[]), 7, 3, l8);
    sp.place(state("oak_planks", &[]), 8, 3, l8);
    l8 += 1;
    pc = 938;
    }
    1086 => {
    a8_BlockState = with(state("ladder", &[]), "facing", piece::dir_name(piece::WEST));
    sp.place(a8_BlockState, 9, 1, 3);
    sp.place(a8_BlockState, 9, 2, 3);
    sp.place(a8_BlockState, 9, 3, 3);
    let t1 = sp.create_chest(r, 3, 4, 8, "chests/stronghold_corridor");
    pc = 1162;
    }
    1162 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_straight_stairs_down(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box_sel(0, 0, 0, 4, 10, 7, (1 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, t.door, 1, 7, 0);
    m_generate_small_door(t, sp, r, lr, 0, 1, 1, 7);
    a8_BlockState = with(state("cobblestone_stairs", &[]), "facing", piece::dir_name(piece::SOUTH));
    l9 = 0;
    pc = 77;
    }
    77 => {
    pc = if l9 >= 6 { 219 } else { 84 };
    }
    84 => {
    sp.place(a8_BlockState, 1, (6 - l9), (1 + l9));
    sp.place(a8_BlockState, 2, (6 - l9), (1 + l9));
    sp.place(a8_BlockState, 3, (6 - l9), (1 + l9));
    pc = if l9 >= 5 { 213 } else { 147 };
    }
    147 => {
    sp.place(state("stone_bricks", &[]), 1, (5 - l9), (1 + l9));
    sp.place(state("stone_bricks", &[]), 2, (5 - l9), (1 + l9));
    sp.place(state("stone_bricks", &[]), 3, (5 - l9), (1 + l9));
    pc = 213;
    }
    213 => {
    l9 += 1;
    pc = 77;
    }
    219 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_stairs_down(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box_sel(0, 0, 0, 4, 10, 4, (1 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, t.door, 1, 7, 0);
    m_generate_small_door(t, sp, r, lr, 0, 1, 1, 4);
    sp.place(state("stone_bricks", &[]), 2, 6, 1);
    sp.place(state("stone_bricks", &[]), 1, 5, 1);
    sp.place(state("smooth_stone_slab", &[]), 1, 6, 1);
    sp.place(state("stone_bricks", &[]), 1, 5, 2);
    sp.place(state("stone_bricks", &[]), 1, 4, 3);
    sp.place(state("smooth_stone_slab", &[]), 1, 5, 3);
    sp.place(state("stone_bricks", &[]), 2, 4, 3);
    sp.place(state("stone_bricks", &[]), 3, 3, 3);
    sp.place(state("smooth_stone_slab", &[]), 3, 4, 3);
    sp.place(state("stone_bricks", &[]), 3, 3, 2);
    sp.place(state("stone_bricks", &[]), 3, 2, 1);
    sp.place(state("smooth_stone_slab", &[]), 3, 3, 1);
    sp.place(state("stone_bricks", &[]), 2, 2, 1);
    sp.place(state("stone_bricks", &[]), 1, 1, 1);
    sp.place(state("smooth_stone_slab", &[]), 1, 2, 1);
    sp.place(state("stone_bricks", &[]), 1, 1, 2);
    sp.place(state("smooth_stone_slab", &[]), 1, 1, 3);
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_five_crossing(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box_sel(0, 0, 0, 9, 8, 10, (1 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, t.door, 4, 3, 0);
    pc = if !(t.f[0]) { 65 } else { 45 };
    }
    45 => {
    sp.gen_box(0, 3, 1, 0, 5, 3, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 65;
    }
    65 => {
    pc = if !(t.f[2]) { 94 } else { 72 };
    }
    72 => {
    sp.gen_box(9, 3, 1, 9, 5, 3, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 94;
    }
    94 => {
    pc = if !(t.f[1]) { 124 } else { 101 };
    }
    101 => {
    sp.gen_box(0, 5, 7, 0, 7, 9, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 124;
    }
    124 => {
    pc = if !(t.f[3]) { 156 } else { 131 };
    }
    131 => {
    sp.gen_box(9, 5, 7, 9, 7, 9, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    pc = 156;
    }
    156 => {
    sp.gen_box(5, 1, 10, 7, 3, 10, state("cave_air", &[]), state("cave_air", &[]), (0 != 0));
    sp.gen_box_sel(1, 2, 1, 8, 2, 6, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(4, 1, 5, 4, 4, 9, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(8, 1, 5, 8, 4, 9, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(1, 4, 7, 3, 4, 9, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(1, 3, 5, 3, 3, 6, (0 != 0), r, &SmoothStone);
    sp.gen_box(1, 3, 4, 3, 3, 4, state("smooth_stone_slab", &[]), state("smooth_stone_slab", &[]), (0 != 0));
    sp.gen_box(1, 4, 6, 3, 4, 6, state("smooth_stone_slab", &[]), state("smooth_stone_slab", &[]), (0 != 0));
    sp.gen_box_sel(5, 1, 7, 7, 1, 8, (0 != 0), r, &SmoothStone);
    sp.gen_box(5, 1, 9, 7, 1, 9, state("smooth_stone_slab", &[]), state("smooth_stone_slab", &[]), (0 != 0));
    sp.gen_box(5, 2, 7, 7, 2, 7, state("smooth_stone_slab", &[]), state("smooth_stone_slab", &[]), (0 != 0));
    sp.gen_box(4, 5, 7, 4, 5, 9, state("smooth_stone_slab", &[]), state("smooth_stone_slab", &[]), (0 != 0));
    sp.gen_box(8, 5, 7, 8, 5, 9, state("smooth_stone_slab", &[]), state("smooth_stone_slab", &[]), (0 != 0));
    sp.gen_box(5, 5, 7, 7, 5, 9, with(state("smooth_stone_slab", &[]), "type", "double"), with(state("smooth_stone_slab", &[]), "type", "double"), (0 != 0));
    sp.place(with(state("wall_torch", &[]), "facing", piece::dir_name(piece::SOUTH)), 6, 5, 6);
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_chest_corridor(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut l8: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box_sel(0, 0, 0, 4, 4, 6, (1 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, t.door, 1, 1, 0);
    m_generate_small_door(t, sp, r, lr, 0, 1, 1, 6);
    sp.gen_box(3, 1, 2, 3, 1, 4, state("stone_bricks", &[]), state("stone_bricks", &[]), (0 != 0));
    sp.place(state("stone_brick_slab", &[]), 3, 1, 1);
    sp.place(state("stone_brick_slab", &[]), 3, 1, 5);
    sp.place(state("stone_brick_slab", &[]), 3, 2, 2);
    sp.place(state("stone_brick_slab", &[]), 3, 2, 4);
    l8 = 2;
    pc = 145;
    }
    145 => {
    pc = if l8 > 4 { 174 } else { 151 };
    }
    151 => {
    sp.place(state("stone_brick_slab", &[]), 2, 1, l8);
    l8 += 1;
    pc = 145;
    }
    174 => {
    pc = if t.placed.load(Relaxed) { 217 } else { 181 };
    }
    181 => {
    let t1 = sp.pos(3, 2, 3);
    let t2 = bb_inside(&sp.cb, t1);
    pc = if !(t2) { 217 } else { 196 };
    }
    196 => {
    t.placed.store((1 != 0), Relaxed);
    let t3 = sp.create_chest(r, 3, 2, 3, "chests/stronghold_crossing");
    pc = 217;
    }
    217 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_library(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a11_BlockState: BlockState = blocks::AIR;
    let mut a12_BlockState: BlockState = blocks::AIR;
    let mut a13_BlockState: BlockState = blocks::AIR;
    let mut a16_BlockState: BlockState = blocks::AIR;
    let mut a17_BlockState: BlockState = blocks::AIR;
    let mut a18_BlockState: BlockState = blocks::AIR;
    let mut a19_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut l13: i32 = 0;
    let mut l14: i32 = 0;
    let mut l15: i32 = 0;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    l8 = 11;
    pc = if t.f[0] { 15 } else { 11 };
    }
    11 => {
    l8 = 6;
    pc = 15;
    }
    15 => {
    sp.gen_box_sel(0, 0, 0, 13, (l8 - 1), 14, (1 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, t.door, 4, 1, 0);
    sp.maybe_box(r, 0.07_f32, 2, 1, 1, 11, 4, 13, state("cobweb", &[]), state("cobweb", &[]), (0 != 0), (0 != 0));
    l9 = 1;
    l10 = 12;
    l11 = 1;
    pc = 98;
    }
    98 => {
    pc = if l11 > 13 { 436 } else { 105 };
    }
    105 => {
    pc = if ((l11 - 1) % 4) != 0 { 303 } else { 114 };
    }
    114 => {
    sp.gen_box(1, 1, l11, 1, 4, l11, state("oak_planks", &[]), state("oak_planks", &[]), (0 != 0));
    sp.gen_box(12, 1, l11, 12, 4, l11, state("oak_planks", &[]), state("oak_planks", &[]), (0 != 0));
    sp.place(with(state("wall_torch", &[]), "facing", piece::dir_name(piece::EAST)), 2, 3, l11);
    sp.place(with(state("wall_torch", &[]), "facing", piece::dir_name(piece::WEST)), 11, 3, l11);
    pc = if !(t.f[0]) { 430 } else { 238 };
    }
    238 => {
    sp.gen_box(1, 6, l11, 1, 9, l11, state("oak_planks", &[]), state("oak_planks", &[]), (0 != 0));
    sp.gen_box(12, 6, l11, 12, 9, l11, state("oak_planks", &[]), state("oak_planks", &[]), (0 != 0));
    pc = 430;
    }
    303 => {
    sp.gen_box(1, 1, l11, 1, 4, l11, state("bookshelf", &[]), state("bookshelf", &[]), (0 != 0));
    sp.gen_box(12, 1, l11, 12, 4, l11, state("bookshelf", &[]), state("bookshelf", &[]), (0 != 0));
    pc = if !(t.f[0]) { 430 } else { 368 };
    }
    368 => {
    sp.gen_box(1, 6, l11, 1, 9, l11, state("bookshelf", &[]), state("bookshelf", &[]), (0 != 0));
    sp.gen_box(12, 6, l11, 12, 9, l11, state("bookshelf", &[]), state("bookshelf", &[]), (0 != 0));
    pc = 430;
    }
    430 => {
    l11 += 1;
    pc = 98;
    }
    436 => {
    l11 = 3;
    pc = 439;
    }
    439 => {
    pc = if l11 >= 12 { 540 } else { 446 };
    }
    446 => {
    sp.gen_box(3, 1, l11, 4, 3, l11, state("bookshelf", &[]), state("bookshelf", &[]), (0 != 0));
    sp.gen_box(6, 1, l11, 7, 3, l11, state("bookshelf", &[]), state("bookshelf", &[]), (0 != 0));
    sp.gen_box(9, 1, l11, 10, 3, l11, state("bookshelf", &[]), state("bookshelf", &[]), (0 != 0));
    l11 += 2;
    pc = 439;
    }
    540 => {
    pc = if !(t.f[0]) { 1651 } else { 547 };
    }
    547 => {
    sp.gen_box(1, 5, 1, 3, 5, 13, state("oak_planks", &[]), state("oak_planks", &[]), (0 != 0));
    sp.gen_box(10, 5, 1, 12, 5, 13, state("oak_planks", &[]), state("oak_planks", &[]), (0 != 0));
    sp.gen_box(4, 5, 1, 9, 5, 2, state("oak_planks", &[]), state("oak_planks", &[]), (0 != 0));
    sp.gen_box(4, 5, 12, 9, 5, 13, state("oak_planks", &[]), state("oak_planks", &[]), (0 != 0));
    sp.place(state("oak_planks", &[]), 9, 5, 11);
    sp.place(state("oak_planks", &[]), 8, 5, 11);
    sp.place(state("oak_planks", &[]), 9, 5, 10);
    a11_BlockState = with(with(state("oak_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    a12_BlockState = with(with(state("oak_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    sp.gen_box(3, 6, 3, 3, 6, 11, a12_BlockState, a12_BlockState, (0 != 0));
    sp.gen_box(10, 6, 3, 10, 6, 9, a12_BlockState, a12_BlockState, (0 != 0));
    sp.gen_box(4, 6, 2, 9, 6, 2, a11_BlockState, a11_BlockState, (0 != 0));
    sp.gen_box(4, 6, 12, 7, 6, 12, a11_BlockState, a11_BlockState, (0 != 0));
    sp.place(with(with(state("oak_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), 3, 6, 2);
    sp.place(with(with(state("oak_fence", &[]), "south", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), 3, 6, 12);
    sp.place(with(with(state("oak_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), 10, 6, 2);
    l13 = 0;
    pc = 1003;
    }
    1003 => {
    pc = if l13 > 2 { 1123 } else { 1009 };
    }
    1009 => {
    sp.place(with(with(state("oak_fence", &[]), "south", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), (8 + l13), 6, (12 - l13));
    pc = if l13 == 2 { 1117 } else { 1066 };
    }
    1066 => {
    sp.place(with(with(state("oak_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), (8 + l13), 6, (11 - l13));
    pc = 1117;
    }
    1117 => {
    l13 += 1;
    pc = 1003;
    }
    1123 => {
    a13_BlockState = with(state("ladder", &[]), "facing", piece::dir_name(piece::SOUTH));
    sp.place(a13_BlockState, 10, 1, 13);
    sp.place(a13_BlockState, 10, 2, 13);
    sp.place(a13_BlockState, 10, 3, 13);
    sp.place(a13_BlockState, 10, 4, 13);
    sp.place(a13_BlockState, 10, 5, 13);
    sp.place(a13_BlockState, 10, 6, 13);
    sp.place(a13_BlockState, 10, 7, 13);
    l14 = 7;
    l15 = 7;
    a16_BlockState = with(state("oak_fence", &[]), "east", if (1 != 0) { "true" } else { "false" });
    sp.place(a16_BlockState, 6, 9, 7);
    a17_BlockState = with(state("oak_fence", &[]), "west", if (1 != 0) { "true" } else { "false" });
    sp.place(a17_BlockState, 7, 9, 7);
    sp.place(a16_BlockState, 6, 8, 7);
    sp.place(a17_BlockState, 7, 8, 7);
    a18_BlockState = with(with(a12_BlockState, "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    sp.place(a18_BlockState, 6, 7, 7);
    sp.place(a18_BlockState, 7, 7, 7);
    sp.place(a16_BlockState, 5, 7, 7);
    sp.place(a17_BlockState, 8, 7, 7);
    sp.place(with(a16_BlockState, "north", if (1 != 0) { "true" } else { "false" }), 6, 7, 6);
    sp.place(with(a16_BlockState, "south", if (1 != 0) { "true" } else { "false" }), 6, 7, 8);
    sp.place(with(a17_BlockState, "north", if (1 != 0) { "true" } else { "false" }), 7, 7, 6);
    sp.place(with(a17_BlockState, "south", if (1 != 0) { "true" } else { "false" }), 7, 7, 8);
    a19_BlockState = state("torch", &[]);
    sp.place(a19_BlockState, 5, 8, 7);
    sp.place(a19_BlockState, 8, 8, 7);
    sp.place(a19_BlockState, 6, 8, 6);
    sp.place(a19_BlockState, 6, 8, 8);
    sp.place(a19_BlockState, 7, 8, 6);
    sp.place(a19_BlockState, 7, 8, 8);
    pc = 1651;
    }
    1651 => {
    let t1 = sp.create_chest(r, 3, 3, 5, "chests/stronghold_library");
    pc = if !(t.f[0]) { 1707 } else { 1674 };
    }
    1674 => {
    sp.place(state("cave_air", &[]), 12, 9, 1);
    let t2 = sp.create_chest(r, 12, 8, 1, "chests/stronghold_library");
    pc = 1707;
    }
    1707 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_portal_room(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a10_BlockState: BlockState = blocks::AIR;
    let mut a11_BlockState: BlockState = blocks::AIR;
    let mut a12_BlockState: BlockState = blocks::AIR;
    let mut a13_BlockState: BlockState = blocks::AIR;
    let mut a14_BlockState: BlockState = blocks::AIR;
    let mut a15_BlockState: BlockState = blocks::AIR;
    let mut a17_Vecbool: Vec<bool> = Vec::new();
    let mut a18_BlockState: BlockState = blocks::AIR;
    let mut a18_P: P = [0, 0, 0];
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l11: i32 = 0;
    let mut l12: i32 = 0;
    let mut l16: i32 = 0;
    let mut l18: i32 = 0;
    let mut l8: i32 = 0;
    let mut sk1_i32: i32 = 0;
    let mut sk2_i32: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box_sel(0, 0, 0, 10, 7, 15, (0 != 0), r, &SmoothStone);
    m_generate_small_door(t, sp, r, lr, 2, 4, 1, 0);
    l8 = 6;
    sp.gen_box_sel(1, 6, 1, 1, 6, 14, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(9, 6, 1, 9, 6, 14, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(2, 6, 1, 8, 6, 2, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(2, 6, 14, 8, 6, 14, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(1, 1, 1, 2, 1, 4, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(8, 1, 1, 9, 1, 4, (0 != 0), r, &SmoothStone);
    sp.gen_box(1, 1, 1, 1, 1, 3, state("lava", &[]), state("lava", &[]), (0 != 0));
    sp.gen_box(9, 1, 1, 9, 1, 3, state("lava", &[]), state("lava", &[]), (0 != 0));
    sp.gen_box_sel(3, 1, 8, 7, 1, 12, (0 != 0), r, &SmoothStone);
    sp.gen_box(4, 1, 9, 6, 1, 11, state("lava", &[]), state("lava", &[]), (0 != 0));
    a9_BlockState = with(with(state("iron_bars", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    a10_BlockState = with(with(state("iron_bars", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    l11 = 3;
    pc = 349;
    }
    349 => {
    pc = if l11 >= 14 { 404 } else { 356 };
    }
    356 => {
    sp.gen_box(0, 3, l11, 0, 4, l11, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(10, 3, l11, 10, 4, l11, a9_BlockState, a9_BlockState, (0 != 0));
    l11 += 2;
    pc = 349;
    }
    404 => {
    l11 = 2;
    pc = 407;
    }
    407 => {
    pc = if l11 >= 9 { 442 } else { 414 };
    }
    414 => {
    sp.gen_box(l11, 3, 15, l11, 4, 15, a10_BlockState, a10_BlockState, (0 != 0));
    l11 += 2;
    pc = 407;
    }
    442 => {
    a11_BlockState = with(state("stone_brick_stairs", &[]), "facing", piece::dir_name(piece::NORTH));
    sp.gen_box_sel(4, 1, 5, 6, 1, 7, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(4, 2, 6, 6, 2, 7, (0 != 0), r, &SmoothStone);
    sp.gen_box_sel(4, 3, 7, 6, 3, 7, (0 != 0), r, &SmoothStone);
    l12 = 4;
    pc = 530;
    }
    530 => {
    pc = if l12 > 6 { 583 } else { 537 };
    }
    537 => {
    sp.place(a11_BlockState, l12, 1, 4);
    sp.place(a11_BlockState, l12, 2, 5);
    sp.place(a11_BlockState, l12, 3, 6);
    l12 += 1;
    pc = 530;
    }
    583 => {
    a12_BlockState = with(state("end_portal_frame", &[]), "facing", piece::dir_name(piece::NORTH));
    a13_BlockState = with(state("end_portal_frame", &[]), "facing", piece::dir_name(piece::SOUTH));
    a14_BlockState = with(state("end_portal_frame", &[]), "facing", piece::dir_name(piece::EAST));
    a15_BlockState = with(state("end_portal_frame", &[]), "facing", piece::dir_name(piece::WEST));
    l16 = 1;
    a17_Vecbool = vec![Default::default(); (12) as usize];
    l18 = 0;
    pc = 675;
    }
    675 => {
    pc = if l18 >= (a17_Vecbool.len() as i32) { 722 } else { 683 };
    }
    683 => {
    let t1 = r.next_float();
    sk1_i32 = l18;
    pc = if t1 <= 0.9_f32 { 704 } else { 700 };
    }
    700 => {
    sk1_i32 = sk1_i32;
    sk2_i32 = 1;
    pc = 705;
    }
    704 => {
    sk1_i32 = sk1_i32;
    sk2_i32 = 0;
    pc = 705;
    }
    705 => {
    a17_Vecbool[(sk1_i32) as usize] = (sk2_i32 != 0);
    l16 = (l16 & (a17_Vecbool[(l18) as usize]) as i32);
    l18 += 1;
    pc = 675;
    }
    722 => {
    sp.place(with(a12_BlockState, "has_eye", if a17_Vecbool[(0) as usize] { "true" } else { "false" }), 4, 3, 8);
    sp.place(with(a12_BlockState, "has_eye", if a17_Vecbool[(1) as usize] { "true" } else { "false" }), 5, 3, 8);
    sp.place(with(a12_BlockState, "has_eye", if a17_Vecbool[(2) as usize] { "true" } else { "false" }), 6, 3, 8);
    sp.place(with(a13_BlockState, "has_eye", if a17_Vecbool[(3) as usize] { "true" } else { "false" }), 4, 3, 12);
    sp.place(with(a13_BlockState, "has_eye", if a17_Vecbool[(4) as usize] { "true" } else { "false" }), 5, 3, 12);
    sp.place(with(a13_BlockState, "has_eye", if a17_Vecbool[(5) as usize] { "true" } else { "false" }), 6, 3, 12);
    sp.place(with(a14_BlockState, "has_eye", if a17_Vecbool[(6) as usize] { "true" } else { "false" }), 3, 3, 9);
    sp.place(with(a14_BlockState, "has_eye", if a17_Vecbool[(7) as usize] { "true" } else { "false" }), 3, 3, 10);
    sp.place(with(a14_BlockState, "has_eye", if a17_Vecbool[(8) as usize] { "true" } else { "false" }), 3, 3, 11);
    sp.place(with(a15_BlockState, "has_eye", if a17_Vecbool[(9) as usize] { "true" } else { "false" }), 7, 3, 9);
    sp.place(with(a15_BlockState, "has_eye", if a17_Vecbool[(10) as usize] { "true" } else { "false" }), 7, 3, 10);
    sp.place(with(a15_BlockState, "has_eye", if a17_Vecbool[(11) as usize] { "true" } else { "false" }), 7, 3, 11);
    pc = if l16 == 0 { 1214 } else { 1086 };
    }
    1086 => {
    a18_BlockState = state("end_portal", &[]);
    sp.place(a18_BlockState, 4, 3, 9);
    sp.place(a18_BlockState, 5, 3, 9);
    sp.place(a18_BlockState, 6, 3, 9);
    sp.place(a18_BlockState, 4, 3, 10);
    sp.place(a18_BlockState, 5, 3, 10);
    sp.place(a18_BlockState, 6, 3, 10);
    sp.place(a18_BlockState, 4, 3, 11);
    sp.place(a18_BlockState, 5, 3, 11);
    sp.place(a18_BlockState, 6, 3, 11);
    pc = 1214;
    }
    1214 => {
    pc = if t.placed.load(Relaxed) { 1297 } else { 1221 };
    }
    1221 => {
    let t2 = sp.pos(5, 3, 6);
    a18_P = t2;
    let t3 = bb_inside(&sp.cb, a18_P);
    pc = if !(t3) { 1297 } else { 1241 };
    }
    1241 => {
    t.placed.store((1 != 0), Relaxed);
    let t4 = sp.lv.set(a18_P, state("spawner", &[]));
    piece::spawner_entity(sp.lv, a18_P, "silverfish");
    pc = if !(false) { 1297 } else { 1280 };
    }
    1280 => {
    pc = 1297;
    }
    1297 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_filler_corridor(t: &SPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    l8 = 0;
    pc = 3;
    }
    3 => {
    pc = if l8 >= t.steps { 293 } else { 12 };
    }
    12 => {
    sp.place(state("stone_bricks", &[]), 0, 0, l8);
    sp.place(state("stone_bricks", &[]), 1, 0, l8);
    sp.place(state("stone_bricks", &[]), 2, 0, l8);
    sp.place(state("stone_bricks", &[]), 3, 0, l8);
    sp.place(state("stone_bricks", &[]), 4, 0, l8);
    l9 = 1;
    pc = 100;
    }
    100 => {
    pc = if l9 > 3 { 202 } else { 106 };
    }
    106 => {
    sp.place(state("stone_bricks", &[]), 0, l9, l8);
    sp.place(state("cave_air", &[]), 1, l9, l8);
    sp.place(state("cave_air", &[]), 2, l9, l8);
    sp.place(state("cave_air", &[]), 3, l9, l8);
    sp.place(state("stone_bricks", &[]), 4, l9, l8);
    l9 += 1;
    pc = 100;
    }
    202 => {
    sp.place(state("stone_bricks", &[]), 0, 4, l8);
    sp.place(state("stone_bricks", &[]), 1, 4, l8);
    sp.place(state("stone_bricks", &[]), 2, 4, l8);
    sp.place(state("stone_bricks", &[]), 3, 4, l8);
    sp.place(state("stone_bricks", &[]), 4, 4, l8);
    l8 += 1;
    pc = 3;
    }
    293 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
