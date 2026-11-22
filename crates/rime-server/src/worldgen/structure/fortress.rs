//! Nether fortresses (`NetherFortressPieces`): piece graph generation ported
//! by hand, the `pp_*` block placement functions generated from the 26.1
//! bytecode by the stack-machine translator.

#![allow(clippy::all, unused_parens, unused_mut, unused_variables, unused_assignments, non_snake_case)]

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use super::legacy::{LLib, LPiece};
use super::piece::{self, state, with, Sp, EAST, NORTH, SOUTH, WEST};
use super::stronghold::orient_box;
use super::template::{bb_inside, bb_intersects};
use super::{Bb, P};
use crate::registry::{blocks, BlockState};
use crate::worldgen::feature::Region;
use crate::worldgen::random::{Legacy, Rng};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FK {
    BridgeCrossing,
    BridgeEndFiller,
    BridgeStraight,
    CorridorStairs,
    TBalcony,
    CastleEntrance,
    SmallCorridorCrossing,
    SmallCorridorLeftTurn,
    SmallCorridor,
    SmallCorridorRightTurn,
    StalkRoom,
    MonsterThrone,
    RoomCrossing,
    StairsRoom,
}

pub struct FPiece {
    pub kind: FK,
    pub bb: Bb,
    pub orient: u8,
    pub depth: i32,
    /// `BridgeEndFiller.selfSeed`
    pub seed: i32,
    /// `isNeedingChest` of the corridor turns
    pub need_chest: AtomicBool,
    /// `MonsterThrone.hasPlacedSpawner`
    pub placed: AtomicBool,
}

struct Weight {
    kind: FK,
    weight: i32,
    max: i32,
    count: i32,
    in_row: bool,
}

impl Weight {
    fn valid(&self) -> bool {
        self.max == 0 || self.count < self.max
    }
}

fn bridge_weights() -> Vec<Weight> {
    let w = |kind, weight, max, in_row| Weight { kind, weight, max, count: 0, in_row };
    vec![w(FK::BridgeStraight, 30, 0, true), w(FK::BridgeCrossing, 10, 4, false), w(FK::RoomCrossing, 10, 4, false), w(FK::StairsRoom, 10, 3, false), w(FK::MonsterThrone, 5, 2, false), w(FK::CastleEntrance, 5, 1, false)]
}

fn castle_weights() -> Vec<Weight> {
    let w = |kind, weight, max, in_row| Weight { kind, weight, max, count: 0, in_row };
    vec![
        w(FK::SmallCorridor, 25, 0, true),
        w(FK::SmallCorridorCrossing, 15, 5, false),
        w(FK::SmallCorridorRightTurn, 5, 10, false),
        w(FK::SmallCorridorLeftTurn, 5, 10, false),
        w(FK::CorridorStairs, 10, 3, true),
        w(FK::TBalcony, 7, 2, false),
        w(FK::StalkRoom, 5, 2, false),
    ]
}

struct Gen<'r> {
    pieces: Vec<FPiece>,
    pending: Vec<usize>,
    bridge: Vec<Weight>,
    castle: Vec<Weight>,
    /// `previousPiece`: (castle list?, kind)
    previous: Option<FK>,
    r: &'r mut dyn Rng,
}

impl Gen<'_> {
    /// `X.createPiece`
    fn create(&mut self, kind: FK, x: i32, y: i32, z: i32, dir: u8, depth: i32) -> Option<FPiece> {
        let (ox, oy, oz, w, h, d) = match kind {
            FK::BridgeCrossing => (-8, -3, 0, 19, 10, 19),
            FK::BridgeEndFiller => (-1, -3, 0, 5, 10, 8),
            FK::BridgeStraight => (-1, -3, 0, 5, 10, 19),
            FK::CorridorStairs => (-1, -7, 0, 5, 14, 10),
            FK::TBalcony => (-3, 0, 0, 9, 7, 9),
            FK::CastleEntrance | FK::StalkRoom => (-5, -3, 0, 13, 14, 13),
            FK::SmallCorridorCrossing | FK::SmallCorridorLeftTurn | FK::SmallCorridor | FK::SmallCorridorRightTurn => (-1, 0, 0, 5, 7, 5),
            FK::MonsterThrone => (-2, 0, 0, 7, 8, 9),
            FK::RoomCrossing => (-2, 0, 0, 7, 9, 7),
            FK::StairsRoom => (-2, 0, 0, 7, 11, 7),
        };
        let bb = orient_box(x, y, z, ox, oy, oz, w, h, d, dir);
        if bb.0[1] <= 10 || self.pieces.iter().any(|p| bb_intersects(&p.bb, &bb)) {
            return None;
        }
        let mut p = FPiece { kind, bb, orient: dir, depth, seed: 0, need_chest: AtomicBool::new(false), placed: AtomicBool::new(false) };
        match kind {
            FK::BridgeEndFiller => p.seed = self.r.next_int(),
            FK::SmallCorridorLeftTurn | FK::SmallCorridorRightTurn => p.need_chest = AtomicBool::new(self.r.next_int_bound(3) == 0),
            _ => {}
        }
        Some(p)
    }

    /// `NetherBridgePiece.generatePiece`
    fn generate_piece(&mut self, castle: bool, x: i32, y: i32, z: i32, dir: u8, depth: i32) -> Option<FPiece> {
        let list = if castle { &self.castle } else { &self.bridge };
        let mut any = false;
        let mut total = 0;
        for w in list {
            if w.max > 0 && w.count < w.max {
                any = true;
            }
            total += w.weight;
        }
        let total = if any { total } else { -1 };
        let ok = total > 0 && depth <= 30;
        let mut tries = 0;
        while tries < 5 && ok {
            tries += 1;
            let mut n = self.r.next_int_bound(total);
            let len = if castle { self.castle.len() } else { self.bridge.len() };
            for i in 0..len {
                let (kind, weight, valid, in_row) = {
                    let w = if castle { &self.castle[i] } else { &self.bridge[i] };
                    (w.kind, w.weight, w.valid(), w.in_row)
                };
                n -= weight;
                if n < 0 {
                    if !valid || self.previous == Some(kind) && !in_row {
                        break;
                    }
                    if let Some(p) = self.create(kind, x, y, z, dir, depth) {
                        let list = if castle { &mut self.castle } else { &mut self.bridge };
                        list[i].count += 1;
                        self.previous = Some(kind);
                        if !list[i].valid() {
                            list.remove(i);
                        }
                        return Some(p);
                    }
                }
            }
        }
        self.create(FK::BridgeEndFiller, x, y, z, dir, depth)
    }

    /// `NetherBridgePiece.generateAndAddPiece`
    fn add(&mut self, x: i32, y: i32, z: i32, dir: u8, depth: i32, castle: bool) {
        let s = self.pieces[0].bb;
        if (x - s.0[0]).abs() <= 112 && (z - s.0[2]).abs() <= 112 {
            if let Some(p) = self.generate_piece(castle, x, y, z, dir, depth + 1) {
                self.pieces.push(p);
                self.pending.push(self.pieces.len() - 1);
            }
        } else {
            // the filler is made (consuming the random) but never added
            self.create(FK::BridgeEndFiller, x, y, z, dir, depth);
        }
    }

    fn forward(&mut self, i: usize, xo: i32, yo: i32, castle: bool) {
        let p = &self.pieces[i];
        let (b, d, depth) = (p.bb, p.orient, p.depth);
        match d {
            NORTH => self.add(b.0[0] + xo, b.0[1] + yo, b.0[2] - 1, d, depth, castle),
            SOUTH => self.add(b.0[0] + xo, b.0[1] + yo, b.1[2] + 1, d, depth, castle),
            WEST => self.add(b.0[0] - 1, b.0[1] + yo, b.0[2] + xo, d, depth, castle),
            _ => self.add(b.1[0] + 1, b.0[1] + yo, b.0[2] + xo, d, depth, castle),
        }
    }

    fn left(&mut self, i: usize, yo: i32, zo: i32, castle: bool) {
        let p = &self.pieces[i];
        let (b, d, depth) = (p.bb, p.orient, p.depth);
        match d {
            NORTH | SOUTH => self.add(b.0[0] - 1, b.0[1] + yo, b.0[2] + zo, WEST, depth, castle),
            _ => self.add(b.0[0] + zo, b.0[1] + yo, b.0[2] - 1, NORTH, depth, castle),
        }
    }

    fn right(&mut self, i: usize, yo: i32, zo: i32, castle: bool) {
        let p = &self.pieces[i];
        let (b, d, depth) = (p.bb, p.orient, p.depth);
        match d {
            NORTH | SOUTH => self.add(b.1[0] + 1, b.0[1] + yo, b.0[2] + zo, EAST, depth, castle),
            _ => self.add(b.0[0] + zo, b.0[1] + yo, b.1[2] + 1, SOUTH, depth, castle),
        }
    }

    /// `X.addChildren`
    fn add_children(&mut self, i: usize) {
        match self.pieces[i].kind {
            FK::BridgeCrossing => {
                self.forward(i, 8, 3, false);
                self.left(i, 3, 8, false);
                self.right(i, 3, 8, false);
            }
            FK::BridgeStraight => self.forward(i, 1, 3, false),
            FK::CorridorStairs | FK::SmallCorridor => self.forward(i, 1, 0, true),
            FK::TBalcony => {
                let d = self.pieces[i].orient;
                let k = if d == WEST || d == NORTH { 5 } else { 1 };
                let a = self.r.next_int_bound(8) > 0;
                self.left(i, 0, k, a);
                let b = self.r.next_int_bound(8) > 0;
                self.right(i, 0, k, b);
            }
            FK::CastleEntrance => self.forward(i, 5, 3, true),
            FK::SmallCorridorCrossing => {
                self.forward(i, 1, 0, true);
                self.left(i, 0, 1, true);
                self.right(i, 0, 1, true);
            }
            FK::SmallCorridorLeftTurn => self.left(i, 0, 1, true),
            FK::SmallCorridorRightTurn => self.right(i, 0, 1, true),
            FK::StalkRoom => {
                self.forward(i, 5, 3, true);
                self.forward(i, 5, 11, true);
            }
            FK::RoomCrossing => {
                self.forward(i, 2, 0, false);
                self.left(i, 0, 2, false);
                self.right(i, 0, 2, false);
            }
            FK::StairsRoom => self.right(i, 6, 2, false),
            FK::BridgeEndFiller | FK::MonsterThrone => {}
        }
    }
}

/// `NetherFortressStructure.generatePieces` for the start in chunk (cx, cz).
pub fn generate(cx: i32, cz: i32, r: &mut dyn Rng) -> Vec<FPiece> {
    let dir = piece::random_horizontal(r);
    let start = FPiece {
        kind: FK::BridgeCrossing,
        bb: piece::make_bb(cx * 16 + 2, 64, cz * 16 + 2, dir, 19, 10, 19),
        orient: dir,
        depth: 0,
        seed: 0,
        need_chest: AtomicBool::new(false),
        placed: AtomicBool::new(false),
    };
    let mut g = Gen { pieces: vec![start], pending: Vec::new(), bridge: bridge_weights(), castle: castle_weights(), previous: None, r };
    g.add_children(0);
    while !g.pending.is_empty() {
        let k = g.r.next_int_bound(g.pending.len() as i32) as usize;
        let p = g.pending.remove(k);
        g.add_children(p);
    }
    let mut pieces = g.pieces;
    // StructurePiecesBuilder.moveInsideHeights(random, 48, 70)
    let mut all = pieces[0].bb;
    for p in &pieces[1..] {
        all = super::template::bb_encapsulate(all, &p.bb);
    }
    let room = 70 - 48 + 1 - (all.1[1] - all.0[1] + 1);
    let y = if room > 1 { 48 + r.next_int_bound(room) } else { 48 };
    let k = y - all.0[1];
    for p in &mut pieces {
        p.bb.0[1] += k;
        p.bb.1[1] += k;
    }
    pieces
}

impl LPiece for FPiece {
    fn bb(&self) -> Bb {
        self.bb
    }
    fn name(&self) -> String {
        format!("fortress/{:?}", self.kind)
    }
    fn post_process(&self, lv: &mut Region, _lib: &LLib, seed: i64, r: &mut dyn Rng, cb: &Bb, reference: P) {
        let mut lr = Legacy::new(seed ^ ((self.bb.0[0] as i64) << 20) ^ self.bb.0[2] as i64);
        let mut sp = Sp { lv, cb: *cb, bb: self.bb, orient: Some(self.orient) };
        let (sp, lr) = (&mut sp, &mut lr);
        match self.kind {
            FK::BridgeCrossing => pp_bridge_crossing(self, sp, r, lr, reference),
            FK::BridgeEndFiller => pp_bridge_end_filler(self, sp, r, lr, reference),
            FK::BridgeStraight => pp_bridge_straight(self, sp, r, lr, reference),
            FK::CorridorStairs => pp_castle_corridor_stairs_piece(self, sp, r, lr, reference),
            FK::TBalcony => pp_castle_corridor_t_balcony_piece(self, sp, r, lr, reference),
            FK::CastleEntrance => pp_castle_entrance(self, sp, r, lr, reference),
            FK::SmallCorridorCrossing => pp_castle_small_corridor_crossing_piece(self, sp, r, lr, reference),
            FK::SmallCorridorLeftTurn => pp_castle_small_corridor_left_turn_piece(self, sp, r, lr, reference),
            FK::SmallCorridor => pp_castle_small_corridor_piece(self, sp, r, lr, reference),
            FK::SmallCorridorRightTurn => pp_castle_small_corridor_right_turn_piece(self, sp, r, lr, reference),
            FK::StalkRoom => pp_castle_stalk_room(self, sp, r, lr, reference),
            FK::MonsterThrone => pp_monster_throne(self, sp, r, lr, reference),
            FK::RoomCrossing => pp_room_crossing(self, sp, r, lr, reference),
            FK::StairsRoom => pp_stairs_room(self, sp, r, lr, reference),
        }
    }
}

// ------------------------------------------------------------ generated

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_bridge_crossing(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(7, 3, 0, 11, 4, 18, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 3, 7, 18, 4, 11, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(8, 5, 0, 10, 7, 18, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(0, 5, 8, 18, 7, 10, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(7, 5, 0, 7, 5, 7, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(7, 5, 11, 7, 5, 18, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(11, 5, 0, 11, 5, 7, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(11, 5, 11, 11, 5, 18, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 5, 7, 7, 5, 7, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(11, 5, 7, 18, 5, 7, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 5, 11, 7, 5, 11, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(11, 5, 11, 18, 5, 11, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(7, 2, 0, 11, 2, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(7, 2, 13, 11, 2, 18, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(7, 0, 0, 11, 1, 3, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(7, 0, 15, 11, 1, 18, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l8 = 7;
    pc = 474;
    }
    474 => {
    pc = if l8 > 11 { 541 } else { 481 };
    }
    481 => {
    l9 = 0;
    pc = 484;
    }
    484 => {
    pc = if l9 > 2 { 535 } else { 490 };
    }
    490 => {
    sp.fill_column_down(state("nether_bricks", &[]), l8, -1, l9);
    sp.fill_column_down(state("nether_bricks", &[]), l8, -1, (18 - l9));
    l9 += 1;
    pc = 484;
    }
    535 => {
    l8 += 1;
    pc = 474;
    }
    541 => {
    sp.gen_box(0, 2, 7, 5, 2, 11, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(13, 2, 7, 18, 2, 11, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 0, 7, 3, 1, 11, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(15, 0, 7, 18, 1, 11, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l8 = 0;
    pc = 660;
    }
    660 => {
    pc = if l8 > 2 { 728 } else { 666 };
    }
    666 => {
    l9 = 7;
    pc = 670;
    }
    670 => {
    pc = if l9 > 11 { 722 } else { 677 };
    }
    677 => {
    sp.fill_column_down(state("nether_bricks", &[]), l8, -1, l9);
    sp.fill_column_down(state("nether_bricks", &[]), (18 - l8), -1, l9);
    l9 += 1;
    pc = 670;
    }
    722 => {
    l8 += 1;
    pc = 660;
    }
    728 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_bridge_end_filler(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_Legacy: Legacy = Legacy::new(0);
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = Legacy::new((t.seed as i64));
    a8_Legacy = t1;
    l9 = 0;
    pc = 13;
    }
    13 => {
    pc = if l9 > 4 { 82 } else { 19 };
    }
    19 => {
    l10 = 3;
    pc = 22;
    }
    22 => {
    pc = if l10 > 4 { 76 } else { 28 };
    }
    28 => {
    let t2 = a8_Legacy.next_int_bound(8);
    l11 = t2;
    sp.gen_box(l9, l10, 0, l9, l10, l11, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l10 += 1;
    pc = 22;
    }
    76 => {
    l9 += 1;
    pc = 13;
    }
    82 => {
    let t3 = a8_Legacy.next_int_bound(8);
    l9 = t3;
    sp.gen_box(0, 5, 0, 0, 5, l9, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    let t4 = a8_Legacy.next_int_bound(8);
    l9 = t4;
    sp.gen_box(4, 5, 0, 4, 5, l9, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l9 = 0;
    pc = 161;
    }
    161 => {
    pc = if l9 > 4 { 212 } else { 167 };
    }
    167 => {
    let t5 = a8_Legacy.next_int_bound(5);
    l10 = t5;
    sp.gen_box(l9, 2, 0, l9, 2, l10, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l9 += 1;
    pc = 161;
    }
    212 => {
    l9 = 0;
    pc = 215;
    }
    215 => {
    pc = if l9 > 4 { 283 } else { 221 };
    }
    221 => {
    l10 = 0;
    pc = 224;
    }
    224 => {
    pc = if l10 > 1 { 277 } else { 230 };
    }
    230 => {
    let t6 = a8_Legacy.next_int_bound(3);
    l11 = t6;
    sp.gen_box(l9, l10, 0, l9, l10, l11, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l10 += 1;
    pc = 224;
    }
    277 => {
    l9 += 1;
    pc = 215;
    }
    283 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_bridge_straight(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a10_BlockState: BlockState = blocks::AIR;
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 3, 0, 4, 4, 18, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 5, 0, 3, 7, 18, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(0, 5, 0, 0, 5, 18, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(4, 5, 0, 4, 5, 18, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 4, 2, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 13, 4, 2, 18, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 0, 0, 4, 1, 3, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 0, 15, 4, 1, 18, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l8 = 0;
    pc = 220;
    }
    220 => {
    pc = if l8 > 4 { 286 } else { 226 };
    }
    226 => {
    l9 = 0;
    pc = 229;
    }
    229 => {
    pc = if l9 > 2 { 280 } else { 235 };
    }
    235 => {
    sp.fill_column_down(state("nether_bricks", &[]), l8, -1, l9);
    sp.fill_column_down(state("nether_bricks", &[]), l8, -1, (18 - l9));
    l9 += 1;
    pc = 229;
    }
    280 => {
    l8 += 1;
    pc = 220;
    }
    286 => {
    a8_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    a9_BlockState = with(a8_BlockState, "east", if (1 != 0) { "true" } else { "false" });
    a10_BlockState = with(a8_BlockState, "west", if (1 != 0) { "true" } else { "false" });
    sp.gen_box(0, 1, 1, 0, 4, 1, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(0, 3, 4, 0, 4, 4, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(0, 3, 14, 0, 4, 14, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(0, 1, 17, 0, 4, 17, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(4, 1, 1, 4, 4, 1, a10_BlockState, a10_BlockState, (0 != 0));
    sp.gen_box(4, 3, 4, 4, 4, 4, a10_BlockState, a10_BlockState, (0 != 0));
    sp.gen_box(4, 3, 14, 4, 4, 14, a10_BlockState, a10_BlockState, (0 != 0));
    sp.gen_box(4, 1, 17, 4, 4, 17, a10_BlockState, a10_BlockState, (0 != 0));
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_castle_corridor_stairs_piece(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut l12: i32 = 0;
    let mut l13: i32 = 0;
    let mut l14: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    a8_BlockState = with(state("nether_brick_stairs", &[]), "facing", piece::dir_name(piece::SOUTH));
    a9_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    l10 = 0;
    pc = 57;
    }
    57 => {
    pc = if l10 > 9 { 412 } else { 64 };
    }
    64 => {
    l11 = (1).max((7 - l10));
    l12 = (((l11 + 5)).max((14 - l10))).min(13);
    l13 = l10;
    sp.gen_box(0, 0, l13, 4, l11, l13, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, (l11 + 1), l13, 3, (l12 - 1), l13, state("air", &[]), state("air", &[]), (0 != 0));
    pc = if l10 > 6 { 216 } else { 168 };
    }
    168 => {
    sp.place(a8_BlockState, 1, (l11 + 1), l13);
    sp.place(a8_BlockState, 2, (l11 + 1), l13);
    sp.place(a8_BlockState, 3, (l11 + 1), l13);
    pc = 216;
    }
    216 => {
    sp.gen_box(0, l12, l13, 4, l12, l13, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, (l11 + 1), l13, 0, (l12 - 1), l13, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(4, (l11 + 1), l13, 4, (l12 - 1), l13, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    pc = if (l10 & 1) != 0 { 373 } else { 321 };
    }
    321 => {
    sp.gen_box(0, (l11 + 2), l13, 0, (l11 + 3), l13, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(4, (l11 + 2), l13, 4, (l11 + 3), l13, a9_BlockState, a9_BlockState, (0 != 0));
    pc = 373;
    }
    373 => {
    l14 = 0;
    pc = 376;
    }
    376 => {
    pc = if l14 > 4 { 406 } else { 382 };
    }
    382 => {
    sp.fill_column_down(state("nether_bricks", &[]), l14, -1, l13);
    l14 += 1;
    pc = 376;
    }
    406 => {
    l10 += 1;
    pc = 57;
    }
    412 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_castle_corridor_t_balcony_piece(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    a8_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    a9_BlockState = with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    sp.gen_box(0, 0, 0, 8, 1, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 8, 5, 8, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(0, 6, 0, 8, 6, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 2, 5, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(6, 2, 0, 8, 5, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 0, 1, 4, 0, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(7, 3, 0, 7, 4, 0, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(0, 2, 4, 8, 2, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 4, 2, 2, 4, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(6, 1, 4, 7, 2, 4, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(1, 3, 8, 7, 3, 8, a9_BlockState, a9_BlockState, (0 != 0));
    sp.place(with(with(state("nether_brick_fence", &[]), "east", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), 0, 3, 8);
    sp.place(with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), 8, 3, 8);
    sp.gen_box(0, 3, 6, 0, 3, 7, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(8, 3, 6, 8, 3, 7, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(0, 3, 4, 0, 5, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(8, 3, 4, 8, 5, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 5, 2, 5, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(6, 3, 5, 7, 5, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 4, 5, 1, 5, 5, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(7, 4, 5, 7, 5, 5, a9_BlockState, a9_BlockState, (0 != 0));
    l10 = 0;
    pc = 626;
    }
    626 => {
    pc = if l10 > 5 { 672 } else { 632 };
    }
    632 => {
    l11 = 0;
    pc = 635;
    }
    635 => {
    pc = if l11 > 8 { 666 } else { 642 };
    }
    642 => {
    sp.fill_column_down(state("nether_bricks", &[]), l11, -1, l10);
    l11 += 1;
    pc = 635;
    }
    666 => {
    l10 += 1;
    pc = 626;
    }
    672 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_castle_entrance(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a10_P: P = [0, 0, 0];
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 3, 0, 12, 4, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 5, 0, 12, 13, 12, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(0, 5, 0, 1, 12, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(11, 5, 0, 12, 12, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 5, 11, 4, 12, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(8, 5, 11, 10, 12, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(5, 9, 11, 7, 12, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 5, 0, 4, 12, 1, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(8, 5, 0, 10, 12, 1, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(5, 9, 0, 7, 12, 1, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 11, 2, 10, 12, 10, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(5, 8, 0, 7, 8, 0, state("nether_brick_fence", &[]), state("nether_brick_fence", &[]), (0 != 0));
    a8_BlockState = with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    a9_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    l10 = 1;
    pc = 421;
    }
    421 => {
    pc = if l10 > 11 { 673 } else { 428 };
    }
    428 => {
    sp.gen_box(l10, 10, 0, l10, 11, 0, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(l10, 10, 12, l10, 11, 12, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(0, 10, l10, 0, 11, l10, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(12, 10, l10, 12, 11, l10, a9_BlockState, a9_BlockState, (0 != 0));
    sp.place(state("nether_bricks", &[]), l10, 13, 0);
    sp.place(state("nether_bricks", &[]), l10, 13, 12);
    sp.place(state("nether_bricks", &[]), 0, 13, l10);
    sp.place(state("nether_bricks", &[]), 12, 13, l10);
    pc = if l10 == 11 { 667 } else { 601 };
    }
    601 => {
    sp.place(a8_BlockState, (l10 + 1), 13, 0);
    sp.place(a8_BlockState, (l10 + 1), 13, 12);
    sp.place(a9_BlockState, 0, 13, (l10 + 1));
    sp.place(a9_BlockState, 12, 13, (l10 + 1));
    pc = 667;
    }
    667 => {
    l10 += 2;
    pc = 421;
    }
    673 => {
    sp.place(with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), 0, 13, 0);
    sp.place(with(with(state("nether_brick_fence", &[]), "south", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), 0, 13, 12);
    sp.place(with(with(state("nether_brick_fence", &[]), "south", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), 12, 13, 12);
    sp.place(with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), 12, 13, 0);
    l10 = 3;
    pc = 852;
    }
    852 => {
    pc = if l10 > 9 { 963 } else { 859 };
    }
    859 => {
    sp.gen_box(1, 7, l10, 1, 8, l10, with(a9_BlockState, "west", if (1 != 0) { "true" } else { "false" }), with(a9_BlockState, "west", if (1 != 0) { "true" } else { "false" }), (0 != 0));
    sp.gen_box(11, 7, l10, 11, 8, l10, with(a9_BlockState, "east", if (1 != 0) { "true" } else { "false" }), with(a9_BlockState, "east", if (1 != 0) { "true" } else { "false" }), (0 != 0));
    l10 += 2;
    pc = 852;
    }
    963 => {
    sp.gen_box(4, 2, 0, 8, 2, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 4, 12, 2, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(4, 0, 0, 8, 1, 3, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(4, 0, 9, 8, 1, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 0, 4, 3, 1, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(9, 0, 4, 12, 1, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l10 = 4;
    pc = 1134;
    }
    1134 => {
    pc = if l10 > 8 { 1201 } else { 1141 };
    }
    1141 => {
    l11 = 0;
    pc = 1144;
    }
    1144 => {
    pc = if l11 > 2 { 1195 } else { 1150 };
    }
    1150 => {
    sp.fill_column_down(state("nether_bricks", &[]), l10, -1, l11);
    sp.fill_column_down(state("nether_bricks", &[]), l10, -1, (12 - l11));
    l11 += 1;
    pc = 1144;
    }
    1195 => {
    l10 += 1;
    pc = 1134;
    }
    1201 => {
    l10 = 0;
    pc = 1204;
    }
    1204 => {
    pc = if l10 > 2 { 1271 } else { 1210 };
    }
    1210 => {
    l11 = 4;
    pc = 1213;
    }
    1213 => {
    pc = if l11 > 8 { 1265 } else { 1220 };
    }
    1220 => {
    sp.fill_column_down(state("nether_bricks", &[]), l10, -1, l11);
    sp.fill_column_down(state("nether_bricks", &[]), (12 - l10), -1, l11);
    l11 += 1;
    pc = 1213;
    }
    1265 => {
    l10 += 1;
    pc = 1204;
    }
    1271 => {
    sp.gen_box(5, 5, 5, 7, 5, 7, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 6, 6, 4, 6, state("air", &[]), state("air", &[]), (0 != 0));
    sp.place(state("nether_bricks", &[]), 6, 0, 6);
    sp.place(state("lava", &[]), 6, 5, 6);
    let t1 = sp.pos(6, 5, 6);
    a10_P = t1;
    let t2 = bb_inside(&sp.cb, a10_P);
    pc = if !(t2) { 1398 } else { 1386 };
    }
    1386 => {
    sp.lv.schedule_tick(a10_P);
    pc = 1398;
    }
    1398 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_castle_small_corridor_crossing_piece(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 0, 0, 4, 1, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 4, 5, 4, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 0, 5, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(4, 2, 0, 4, 5, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 4, 0, 5, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(4, 2, 4, 4, 5, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 6, 0, 4, 6, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l8 = 0;
    pc = 187;
    }
    187 => {
    pc = if l8 > 4 { 232 } else { 193 };
    }
    193 => {
    l9 = 0;
    pc = 196;
    }
    196 => {
    pc = if l9 > 4 { 226 } else { 202 };
    }
    202 => {
    sp.fill_column_down(state("nether_bricks", &[]), l8, -1, l9);
    l9 += 1;
    pc = 196;
    }
    226 => {
    l8 += 1;
    pc = 187;
    }
    232 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_castle_small_corridor_left_turn_piece(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 0, 0, 4, 1, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 4, 5, 4, state("air", &[]), state("air", &[]), (0 != 0));
    a8_BlockState = with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    a9_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    sp.gen_box(4, 2, 0, 4, 5, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(4, 3, 1, 4, 4, 1, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(4, 3, 3, 4, 4, 3, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(0, 2, 0, 0, 5, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 4, 3, 5, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 4, 1, 4, 4, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(3, 3, 4, 3, 4, 4, a8_BlockState, a8_BlockState, (0 != 0));
    pc = if !(t.need_chest.load(Relaxed)) { 313 } else { 277 };
    }
    277 => {
    let t1 = sp.pos(3, 2, 3);
    let t2 = bb_inside(&sp.cb, t1);
    pc = if !(t2) { 313 } else { 292 };
    }
    292 => {
    t.need_chest.store((0 != 0), Relaxed);
    let t3 = sp.create_chest(r, 3, 2, 3, "chests/nether_bridge");
    pc = 313;
    }
    313 => {
    sp.gen_box(0, 6, 0, 4, 6, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l10 = 0;
    pc = 344;
    }
    344 => {
    pc = if l10 > 4 { 389 } else { 350 };
    }
    350 => {
    l11 = 0;
    pc = 353;
    }
    353 => {
    pc = if l11 > 4 { 383 } else { 359 };
    }
    359 => {
    sp.fill_column_down(state("nether_bricks", &[]), l10, -1, l11);
    l11 += 1;
    pc = 353;
    }
    383 => {
    l10 += 1;
    pc = 344;
    }
    389 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_castle_small_corridor_piece(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 0, 0, 4, 1, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 4, 5, 4, state("air", &[]), state("air", &[]), (0 != 0));
    a8_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    sp.gen_box(0, 2, 0, 0, 5, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(4, 2, 0, 4, 5, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 3, 1, 0, 4, 1, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(0, 3, 3, 0, 4, 3, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(4, 3, 1, 4, 4, 1, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(4, 3, 3, 4, 4, 3, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(0, 6, 0, 4, 6, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l9 = 0;
    pc = 241;
    }
    241 => {
    pc = if l9 > 4 { 286 } else { 247 };
    }
    247 => {
    l10 = 0;
    pc = 250;
    }
    250 => {
    pc = if l10 > 4 { 280 } else { 256 };
    }
    256 => {
    sp.fill_column_down(state("nether_bricks", &[]), l9, -1, l10);
    l10 += 1;
    pc = 250;
    }
    280 => {
    l9 += 1;
    pc = 241;
    }
    286 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_castle_small_corridor_right_turn_piece(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 0, 0, 4, 1, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 4, 5, 4, state("air", &[]), state("air", &[]), (0 != 0));
    a8_BlockState = with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    a9_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    sp.gen_box(0, 2, 0, 0, 5, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 3, 1, 0, 4, 1, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(0, 3, 3, 0, 4, 3, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(4, 2, 0, 4, 5, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 2, 4, 4, 5, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 4, 1, 4, 4, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(3, 3, 4, 3, 4, 4, a8_BlockState, a8_BlockState, (0 != 0));
    pc = if !(t.need_chest.load(Relaxed)) { 313 } else { 277 };
    }
    277 => {
    let t1 = sp.pos(1, 2, 3);
    let t2 = bb_inside(&sp.cb, t1);
    pc = if !(t2) { 313 } else { 292 };
    }
    292 => {
    t.need_chest.store((0 != 0), Relaxed);
    let t3 = sp.create_chest(r, 1, 2, 3, "chests/nether_bridge");
    pc = 313;
    }
    313 => {
    sp.gen_box(0, 6, 0, 4, 6, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l10 = 0;
    pc = 344;
    }
    344 => {
    pc = if l10 > 4 { 389 } else { 350 };
    }
    350 => {
    l11 = 0;
    pc = 353;
    }
    353 => {
    pc = if l11 > 4 { 383 } else { 359 };
    }
    359 => {
    sp.fill_column_down(state("nether_bricks", &[]), l10, -1, l11);
    l11 += 1;
    pc = 353;
    }
    383 => {
    l10 += 1;
    pc = 344;
    }
    389 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_castle_stalk_room(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a10_BlockState: BlockState = blocks::AIR;
    let mut a11_BlockState: BlockState = blocks::AIR;
    let mut a12_BlockState: BlockState = blocks::AIR;
    let mut a13_BlockState: BlockState = blocks::AIR;
    let mut a14_BlockState: BlockState = blocks::AIR;
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l12: i32 = 0;
    let mut l13: i32 = 0;
    let mut l14: i32 = 0;
    let mut l15: i32 = 0;
    let mut l16: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 3, 0, 12, 4, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 5, 0, 12, 13, 12, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(0, 5, 0, 1, 12, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(11, 5, 0, 12, 12, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 5, 11, 4, 12, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(8, 5, 11, 10, 12, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(5, 9, 11, 7, 12, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 5, 0, 4, 12, 1, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(8, 5, 0, 10, 12, 1, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(5, 9, 0, 7, 12, 1, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 11, 2, 10, 12, 10, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    a8_BlockState = with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    a9_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    a10_BlockState = with(a9_BlockState, "west", if (1 != 0) { "true" } else { "false" });
    a11_BlockState = with(a9_BlockState, "east", if (1 != 0) { "true" } else { "false" });
    l12 = 1;
    pc = 426;
    }
    426 => {
    pc = if l12 > 11 { 678 } else { 433 };
    }
    433 => {
    sp.gen_box(l12, 10, 0, l12, 11, 0, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(l12, 10, 12, l12, 11, 12, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(0, 10, l12, 0, 11, l12, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(12, 10, l12, 12, 11, l12, a9_BlockState, a9_BlockState, (0 != 0));
    sp.place(state("nether_bricks", &[]), l12, 13, 0);
    sp.place(state("nether_bricks", &[]), l12, 13, 12);
    sp.place(state("nether_bricks", &[]), 0, 13, l12);
    sp.place(state("nether_bricks", &[]), 12, 13, l12);
    pc = if l12 == 11 { 672 } else { 606 };
    }
    606 => {
    sp.place(a8_BlockState, (l12 + 1), 13, 0);
    sp.place(a8_BlockState, (l12 + 1), 13, 12);
    sp.place(a9_BlockState, 0, 13, (l12 + 1));
    sp.place(a9_BlockState, 12, 13, (l12 + 1));
    pc = 672;
    }
    672 => {
    l12 += 2;
    pc = 426;
    }
    678 => {
    sp.place(with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), 0, 13, 0);
    sp.place(with(with(state("nether_brick_fence", &[]), "south", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" }), 0, 13, 12);
    sp.place(with(with(state("nether_brick_fence", &[]), "south", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), 12, 13, 12);
    sp.place(with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), 12, 13, 0);
    l12 = 3;
    pc = 857;
    }
    857 => {
    pc = if l12 > 9 { 916 } else { 864 };
    }
    864 => {
    sp.gen_box(1, 7, l12, 1, 8, l12, a10_BlockState, a10_BlockState, (0 != 0));
    sp.gen_box(11, 7, l12, 11, 8, l12, a11_BlockState, a11_BlockState, (0 != 0));
    l12 += 2;
    pc = 857;
    }
    916 => {
    a12_BlockState = with(state("nether_brick_stairs", &[]), "facing", piece::dir_name(piece::NORTH));
    l13 = 0;
    pc = 939;
    }
    939 => {
    pc = if l13 > 6 { 1129 } else { 946 };
    }
    946 => {
    l14 = (l13 + 4);
    l15 = 5;
    pc = 955;
    }
    955 => {
    pc = if l15 > 7 { 985 } else { 962 };
    }
    962 => {
    sp.place(a12_BlockState, l15, (5 + l13), l14);
    l15 += 1;
    pc = 955;
    }
    985 => {
    pc = if l14 < 5 { 1033 } else { 991 };
    }
    991 => {
    pc = if l14 > 8 { 1033 } else { 998 };
    }
    998 => {
    sp.gen_box(5, 5, l14, 7, (l13 + 4), l14, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    pc = 1080;
    }
    1033 => {
    pc = if l14 < 9 { 1080 } else { 1040 };
    }
    1040 => {
    pc = if l14 > 10 { 1080 } else { 1047 };
    }
    1047 => {
    sp.gen_box(5, 8, l14, 7, (l13 + 4), l14, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    pc = 1080;
    }
    1080 => {
    pc = if l13 < 1 { 1123 } else { 1086 };
    }
    1086 => {
    sp.gen_box(5, (6 + l13), l14, 7, (9 + l13), l14, state("air", &[]), state("air", &[]), (0 != 0));
    pc = 1123;
    }
    1123 => {
    l13 += 1;
    pc = 939;
    }
    1129 => {
    l13 = 5;
    pc = 1132;
    }
    1132 => {
    pc = if l13 > 7 { 1160 } else { 1139 };
    }
    1139 => {
    sp.place(a12_BlockState, l13, 12, 11);
    l13 += 1;
    pc = 1132;
    }
    1160 => {
    sp.gen_box(5, 6, 7, 5, 7, 7, a11_BlockState, a11_BlockState, (0 != 0));
    sp.gen_box(7, 6, 7, 7, 7, 7, a10_BlockState, a10_BlockState, (0 != 0));
    sp.gen_box(5, 13, 12, 7, 13, 12, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(2, 5, 2, 3, 5, 3, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 5, 9, 3, 5, 10, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 5, 4, 2, 5, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(9, 5, 2, 10, 5, 3, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(9, 5, 9, 10, 5, 10, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(10, 5, 4, 10, 5, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    a13_BlockState = with(a12_BlockState, "facing", piece::dir_name(piece::EAST));
    a14_BlockState = with(a12_BlockState, "facing", piece::dir_name(piece::WEST));
    sp.place(a14_BlockState, 4, 5, 2);
    sp.place(a14_BlockState, 4, 5, 3);
    sp.place(a14_BlockState, 4, 5, 9);
    sp.place(a14_BlockState, 4, 5, 10);
    sp.place(a13_BlockState, 8, 5, 2);
    sp.place(a13_BlockState, 8, 5, 3);
    sp.place(a13_BlockState, 8, 5, 9);
    sp.place(a13_BlockState, 8, 5, 10);
    sp.gen_box(3, 4, 4, 4, 4, 8, state("soul_sand", &[]), state("soul_sand", &[]), (0 != 0));
    sp.gen_box(8, 4, 4, 9, 4, 8, state("soul_sand", &[]), state("soul_sand", &[]), (0 != 0));
    sp.gen_box(3, 5, 4, 4, 5, 8, state("nether_wart", &[]), state("nether_wart", &[]), (0 != 0));
    sp.gen_box(8, 5, 4, 9, 5, 8, state("nether_wart", &[]), state("nether_wart", &[]), (0 != 0));
    sp.gen_box(4, 2, 0, 8, 2, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 4, 12, 2, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(4, 0, 0, 8, 1, 3, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(4, 0, 9, 8, 1, 12, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 0, 4, 3, 1, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(9, 0, 4, 12, 1, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    l15 = 4;
    pc = 1824;
    }
    1824 => {
    pc = if l15 > 8 { 1891 } else { 1831 };
    }
    1831 => {
    l16 = 0;
    pc = 1834;
    }
    1834 => {
    pc = if l16 > 2 { 1885 } else { 1840 };
    }
    1840 => {
    sp.fill_column_down(state("nether_bricks", &[]), l15, -1, l16);
    sp.fill_column_down(state("nether_bricks", &[]), l15, -1, (12 - l16));
    l16 += 1;
    pc = 1834;
    }
    1885 => {
    l15 += 1;
    pc = 1824;
    }
    1891 => {
    l15 = 0;
    pc = 1894;
    }
    1894 => {
    pc = if l15 > 2 { 1961 } else { 1900 };
    }
    1900 => {
    l16 = 4;
    pc = 1903;
    }
    1903 => {
    pc = if l16 > 8 { 1955 } else { 1910 };
    }
    1910 => {
    sp.fill_column_down(state("nether_bricks", &[]), l15, -1, l16);
    sp.fill_column_down(state("nether_bricks", &[]), (12 - l15), -1, l16);
    l16 += 1;
    pc = 1903;
    }
    1955 => {
    l15 += 1;
    pc = 1894;
    }
    1961 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_monster_throne(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a10_P: P = [0, 0, 0];
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 2, 0, 6, 7, 7, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(1, 0, 0, 5, 1, 7, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 2, 1, 5, 2, 7, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 2, 5, 3, 7, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 4, 3, 5, 4, 7, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 2, 0, 1, 4, 2, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(5, 2, 0, 5, 4, 2, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 5, 2, 1, 5, 3, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(5, 5, 2, 5, 5, 3, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 5, 3, 0, 5, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(6, 5, 3, 6, 5, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 5, 8, 5, 5, 8, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    a8_BlockState = with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    a9_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    sp.place(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), 1, 6, 3);
    sp.place(with(state("nether_brick_fence", &[]), "east", if (1 != 0) { "true" } else { "false" }), 5, 6, 3);
    sp.place(with(with(state("nether_brick_fence", &[]), "east", if (1 != 0) { "true" } else { "false" }), "north", if (1 != 0) { "true" } else { "false" }), 0, 6, 3);
    sp.place(with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "north", if (1 != 0) { "true" } else { "false" }), 6, 6, 3);
    sp.gen_box(0, 6, 4, 0, 6, 7, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(6, 6, 4, 6, 6, 7, a9_BlockState, a9_BlockState, (0 != 0));
    sp.place(with(with(state("nether_brick_fence", &[]), "east", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), 0, 6, 8);
    sp.place(with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), 6, 6, 8);
    sp.gen_box(1, 6, 8, 5, 6, 8, a8_BlockState, a8_BlockState, (0 != 0));
    sp.place(with(state("nether_brick_fence", &[]), "east", if (1 != 0) { "true" } else { "false" }), 1, 7, 8);
    sp.gen_box(2, 7, 8, 4, 7, 8, a8_BlockState, a8_BlockState, (0 != 0));
    sp.place(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), 5, 7, 8);
    sp.place(with(state("nether_brick_fence", &[]), "east", if (1 != 0) { "true" } else { "false" }), 2, 8, 8);
    sp.place(a8_BlockState, 3, 8, 8);
    sp.place(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), 4, 8, 8);
    pc = if t.placed.load(Relaxed) { 937 } else { 862 };
    }
    862 => {
    let t1 = sp.pos(3, 5, 5);
    a10_P = t1;
    let t2 = bb_inside(&sp.cb, a10_P);
    pc = if !(t2) { 937 } else { 881 };
    }
    881 => {
    t.placed.store((1 != 0), Relaxed);
    let t3 = sp.lv.set(a10_P, state("spawner", &[]));
    piece::spawner_entity(sp.lv, a10_P, "blaze");
    pc = if !(false) { 937 } else { 920 };
    }
    920 => {
    pc = 937;
    }
    937 => {
    l10 = 0;
    pc = 940;
    }
    940 => {
    pc = if l10 > 6 { 987 } else { 947 };
    }
    947 => {
    l11 = 0;
    pc = 950;
    }
    950 => {
    pc = if l11 > 6 { 981 } else { 957 };
    }
    957 => {
    sp.fill_column_down(state("nether_bricks", &[]), l10, -1, l11);
    l11 += 1;
    pc = 950;
    }
    981 => {
    l10 += 1;
    pc = 940;
    }
    987 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_room_crossing(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 0, 0, 6, 1, 6, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 6, 7, 6, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 1, 6, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 6, 1, 6, 6, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(5, 2, 0, 6, 6, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(5, 2, 6, 6, 6, 6, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 0, 6, 1, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 5, 0, 6, 6, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(6, 2, 0, 6, 6, 1, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(6, 2, 5, 6, 6, 6, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    a8_BlockState = with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    a9_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    sp.gen_box(2, 6, 0, 4, 6, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 5, 0, 4, 5, 0, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(2, 6, 6, 4, 6, 6, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 5, 6, 4, 5, 6, a8_BlockState, a8_BlockState, (0 != 0));
    sp.gen_box(0, 6, 2, 0, 6, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 5, 2, 0, 5, 4, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(6, 6, 2, 6, 6, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(6, 5, 2, 6, 5, 4, a9_BlockState, a9_BlockState, (0 != 0));
    l10 = 0;
    pc = 548;
    }
    548 => {
    pc = if l10 > 6 { 595 } else { 555 };
    }
    555 => {
    l11 = 0;
    pc = 558;
    }
    558 => {
    pc = if l11 > 6 { 589 } else { 565 };
    }
    565 => {
    sp.fill_column_down(state("nether_bricks", &[]), l10, -1, l11);
    l11 += 1;
    pc = 558;
    }
    589 => {
    l10 += 1;
    pc = 548;
    }
    595 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_stairs_room(t: &FPiece, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 0, 0, 6, 1, 6, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 6, 10, 6, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 1, 8, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(5, 2, 0, 6, 8, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 1, 0, 8, 6, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(6, 2, 1, 6, 8, 6, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 2, 6, 5, 8, 6, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    a8_BlockState = with(with(state("nether_brick_fence", &[]), "west", if (1 != 0) { "true" } else { "false" }), "east", if (1 != 0) { "true" } else { "false" });
    a9_BlockState = with(with(state("nether_brick_fence", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" });
    sp.gen_box(0, 3, 2, 0, 5, 4, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(6, 3, 2, 6, 5, 2, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(6, 3, 4, 6, 5, 4, a9_BlockState, a9_BlockState, (0 != 0));
    sp.place(state("nether_bricks", &[]), 5, 2, 5);
    sp.gen_box(4, 2, 5, 4, 3, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(3, 2, 5, 3, 4, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 2, 5, 2, 5, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 2, 5, 1, 6, 5, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(1, 7, 1, 5, 7, 4, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(6, 8, 2, 6, 8, 4, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(2, 6, 0, 4, 8, 0, state("nether_bricks", &[]), state("nether_bricks", &[]), (0 != 0));
    sp.gen_box(2, 5, 0, 4, 5, 0, a8_BlockState, a8_BlockState, (0 != 0));
    l10 = 0;
    pc = 553;
    }
    553 => {
    pc = if l10 > 6 { 600 } else { 560 };
    }
    560 => {
    l11 = 0;
    pc = 563;
    }
    563 => {
    pc = if l11 > 6 { 594 } else { 570 };
    }
    570 => {
    sp.fill_column_down(state("nether_bricks", &[]), l10, -1, l11);
    l11 += 1;
    pc = 563;
    }
    594 => {
    l10 += 1;
    pc = 553;
    }
    600 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
