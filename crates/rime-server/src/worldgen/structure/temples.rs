//! Desert pyramid and jungle temple pieces. The `m_*` / `j_*` functions are
//! generated from the 26.1 bytecode by a stack-machine translator (each
//! basic block is one `match` arm), so they follow vanilla exactly.

#![allow(clippy::all, unused_parens, unused_mut, unused_variables, unused_assignments, non_snake_case)]

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use parking_lot::Mutex;

use super::legacy::{LLib, LPiece, Scattered};
use super::piece::{self, state, with, MossStone, Sp, HORIZONTAL};
use super::template::{bb_center, bb_inside, rotate_state};
use super::{Bb, P};
use crate::registry::{blocks, BlockState};
use crate::worldgen::feature::Region;
use crate::worldgen::random::{Legacy, Rng};

pub struct DesertPyramid {
    pub sc: Scattered,
    placed: [AtomicBool; 4],
    sand: Mutex<Vec<P>>,
    roof: Mutex<P>,
}

impl DesertPyramid {
    pub fn new(r: &mut dyn Rng, x: i32, z: i32) -> Self {
        let dir = piece::random_horizontal(r);
        DesertPyramid { sc: Scattered::new(x, 64, z, dir, 21, 15, 21), placed: Default::default(), sand: Mutex::new(Vec::new()), roof: Mutex::new([0, 0, 0]) }
    }
}

fn level_random(seed: i64, bb: &Bb) -> Legacy {
    // `level.getRandom()` is not seeded in vanilla either
    Legacy::new(seed ^ (bb.0[0] as i64) << 20 ^ bb.0[2] as i64)
}

impl LPiece for DesertPyramid {
    fn bb(&self) -> Bb {
        self.sc.st.lock().0
    }
    fn name(&self) -> String {
        "desert_pyramid".into()
    }
    fn post_process(&self, lv: &mut Region, _lib: &LLib, seed: i64, r: &mut dyn Rng, cb: &Bb, reference: P) {
        let bb = self.bb();
        let mut lr = level_random(seed, &bb);
        let mut sp = Sp { lv, cb: *cb, bb, orient: Some(self.sc.orient) };
        m_post_process(self, &mut sp, r, &mut lr, reference);
    }
    fn after_place(&self, lv: &mut Region, cb: &Bb, all: &[Box<dyn LPiece>]) {
        desert_after_place(self, lv, cb, all);
    }
}

/// `DesertPyramidStructure.afterPlace`
fn desert_after_place(this: &DesertPyramid, lv: &mut Region, cb: &Bb, all: &[Box<dyn LPiece>]) {
    let mut set: Vec<P> = this.sand.lock().clone();
    set.sort_by(|a, b| (a[1], a[2], a[0]).cmp(&(b[1], b[2], b[0])));
    set.dedup();
    let sus = state("suspicious_sand", &[]);
    let roof = *this.roof.lock();
    if bb_inside(cb, roof) {
        lv.set(roof, sus);
    }
    let mut bb = all[0].bb();
    for p in &all[1..] {
        bb = super::template::bb_encapsulate(bb, &p.bb());
    }
    let c = bb_center(&bb);
    let mut rs = Legacy::new(lv.seed).fork_positional().at(c[0], c[1], c[2]);
    let mut list = set;
    for i in (2..=list.len()).rev() {
        let j = rs.next_int_bound(i as i32) as usize;
        list.swap(i - 1, j);
    }
    let mut k = (list.len() as i32).min(5 + rs.next_int_bound(3));
    let sand = state("sand", &[]);
    for p in list {
        if k > 0 {
            k -= 1;
            if bb_inside(cb, p) {
                lv.set(p, sus);
            }
        } else if bb_inside(cb, p) {
            lv.set(p, sand);
        }
    }
}

pub struct JungleTemple {
    pub sc: Scattered,
    placed_main_chest: AtomicBool,
    placed_hidden_chest: AtomicBool,
    placed_trap1: AtomicBool,
    placed_trap2: AtomicBool,
}

impl JungleTemple {
    pub fn new(r: &mut dyn Rng, x: i32, z: i32) -> Self {
        let dir = piece::random_horizontal(r);
        JungleTemple { sc: Scattered::new(x, 64, z, dir, 12, 10, 15), placed_main_chest: AtomicBool::new(false), placed_hidden_chest: AtomicBool::new(false), placed_trap1: AtomicBool::new(false), placed_trap2: AtomicBool::new(false) }
    }
}

impl LPiece for JungleTemple {
    fn bb(&self) -> Bb {
        self.sc.st.lock().0
    }
    fn name(&self) -> String {
        "jungle_temple".into()
    }
    fn post_process(&self, lv: &mut Region, _lib: &LLib, seed: i64, r: &mut dyn Rng, cb: &Bb, reference: P) {
        let bb = self.bb();
        let mut lr = level_random(seed, &bb);
        let mut sp = Sp { lv, cb: *cb, bb, orient: Some(self.sc.orient) };
        j_post_process(self, &mut sp, r, &mut lr, reference);
    }
}

// ------------------------------------------------------------ generated

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_post_process(t: &DesertPyramid, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a10_BlockState: BlockState = blocks::AIR;
    let mut a11_BlockState: BlockState = blocks::AIR;
    let mut a12_stdsliceIterstaticu8: std::slice::Iter<'static, u8> = HORIZONTAL.iter();
    let mut a13_u8: u8 = 0;
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l12: i32 = 0;
    let mut l14: i32 = 0;
    let mut l15: i32 = 0;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = r.next_int_bound(3);
    let t2 = { t.sc.lowest_ground(sp, (-t1)); true };
    pc = if t2 { 18 } else { 17 };
    }
    17 => {
    return;
    }
    18 => {
    sp.gen_box(0, -4, 0, (t.sc.w - 1), 0, (t.sc.d - 1), state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    l8 = 1;
    pc = 58;
    }
    58 => {
    pc = if l8 > 9 { 167 } else { 65 };
    }
    65 => {
    sp.gen_box(l8, l8, l8, ((t.sc.w - 1) - l8), l8, ((t.sc.d - 1) - l8), state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.gen_box((l8 + 1), l8, (l8 + 1), ((t.sc.w - 2) - l8), l8, ((t.sc.d - 2) - l8), state("air", &[]), state("air", &[]), (0 != 0));
    l8 += 1;
    pc = 58;
    }
    167 => {
    l8 = 0;
    pc = 170;
    }
    170 => {
    pc = if l8 >= t.sc.w { 226 } else { 179 };
    }
    179 => {
    l9 = 0;
    pc = 182;
    }
    182 => {
    pc = if l9 >= t.sc.d { 220 } else { 191 };
    }
    191 => {
    l10 = -5;
    sp.fill_column_down(state("sandstone", &[]), l8, -5, l9);
    l9 += 1;
    pc = 182;
    }
    220 => {
    l8 += 1;
    pc = 170;
    }
    226 => {
    a8_BlockState = with(state("sandstone_stairs", &[]), "facing", "north");
    a9_BlockState = with(state("sandstone_stairs", &[]), "facing", "south");
    a10_BlockState = with(state("sandstone_stairs", &[]), "facing", "east");
    a11_BlockState = with(state("sandstone_stairs", &[]), "facing", "west");
    sp.gen_box(0, 0, 0, 4, 9, 4, state("sandstone", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(1, 10, 1, 3, 10, 3, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.place(a8_BlockState, 2, 10, 0);
    sp.place(a9_BlockState, 2, 10, 4);
    sp.place(a10_BlockState, 0, 10, 2);
    sp.place(a11_BlockState, 4, 10, 2);
    sp.gen_box((t.sc.w - 5), 0, 0, (t.sc.w - 1), 9, 4, state("sandstone", &[]), state("air", &[]), (0 != 0));
    sp.gen_box((t.sc.w - 4), 10, 1, (t.sc.w - 2), 10, 3, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.place(a8_BlockState, (t.sc.w - 3), 10, 0);
    sp.place(a9_BlockState, (t.sc.w - 3), 10, 4);
    sp.place(a10_BlockState, (t.sc.w - 5), 10, 2);
    sp.place(a11_BlockState, (t.sc.w - 1), 10, 2);
    sp.gen_box(8, 0, 0, 12, 4, 4, state("sandstone", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(9, 1, 0, 11, 3, 4, state("air", &[]), state("air", &[]), (0 != 0));
    sp.place(state("cut_sandstone", &[]), 9, 1, 1);
    sp.place(state("cut_sandstone", &[]), 9, 2, 1);
    sp.place(state("cut_sandstone", &[]), 9, 3, 1);
    sp.place(state("cut_sandstone", &[]), 10, 3, 1);
    sp.place(state("cut_sandstone", &[]), 11, 3, 1);
    sp.place(state("cut_sandstone", &[]), 11, 2, 1);
    sp.place(state("cut_sandstone", &[]), 11, 1, 1);
    sp.gen_box(4, 1, 1, 8, 3, 3, state("sandstone", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(4, 1, 2, 8, 2, 2, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(12, 1, 1, 16, 3, 3, state("sandstone", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(12, 1, 2, 16, 2, 2, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(5, 4, 5, (t.sc.w - 6), 4, (t.sc.d - 6), state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.gen_box(9, 4, 9, 11, 4, 11, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box(8, 1, 8, 8, 3, 8, state("cut_sandstone", &[]), state("cut_sandstone", &[]), (0 != 0));
    sp.gen_box(12, 1, 8, 12, 3, 8, state("cut_sandstone", &[]), state("cut_sandstone", &[]), (0 != 0));
    sp.gen_box(8, 1, 12, 8, 3, 12, state("cut_sandstone", &[]), state("cut_sandstone", &[]), (0 != 0));
    sp.gen_box(12, 1, 12, 12, 3, 12, state("cut_sandstone", &[]), state("cut_sandstone", &[]), (0 != 0));
    sp.gen_box(1, 1, 5, 4, 4, 11, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.gen_box((t.sc.w - 5), 1, 5, (t.sc.w - 2), 4, 11, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.gen_box(6, 7, 9, 6, 7, 11, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.gen_box((t.sc.w - 7), 7, 9, (t.sc.w - 7), 7, 11, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.gen_box(5, 5, 9, 5, 7, 11, state("cut_sandstone", &[]), state("cut_sandstone", &[]), (0 != 0));
    sp.gen_box((t.sc.w - 6), 5, 9, (t.sc.w - 6), 7, 11, state("cut_sandstone", &[]), state("cut_sandstone", &[]), (0 != 0));
    sp.place(state("air", &[]), 5, 5, 10);
    sp.place(state("air", &[]), 5, 6, 10);
    sp.place(state("air", &[]), 6, 6, 10);
    sp.place(state("air", &[]), (t.sc.w - 6), 5, 10);
    sp.place(state("air", &[]), (t.sc.w - 6), 6, 10);
    sp.place(state("air", &[]), (t.sc.w - 7), 6, 10);
    sp.gen_box(2, 4, 4, 2, 6, 4, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box((t.sc.w - 3), 4, 4, (t.sc.w - 3), 6, 4, state("air", &[]), state("air", &[]), (0 != 0));
    sp.place(a8_BlockState, 2, 4, 5);
    sp.place(a8_BlockState, 2, 3, 4);
    sp.place(a8_BlockState, (t.sc.w - 3), 4, 5);
    sp.place(a8_BlockState, (t.sc.w - 3), 3, 4);
    sp.gen_box(1, 1, 3, 2, 2, 3, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.gen_box((t.sc.w - 3), 1, 3, (t.sc.w - 2), 2, 3, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.place(state("sandstone", &[]), 1, 1, 2);
    sp.place(state("sandstone", &[]), (t.sc.w - 2), 1, 2);
    sp.place(state("sandstone_slab", &[]), 1, 2, 2);
    sp.place(state("sandstone_slab", &[]), (t.sc.w - 2), 2, 2);
    sp.place(a11_BlockState, 2, 1, 2);
    sp.place(a10_BlockState, (t.sc.w - 3), 1, 2);
    sp.gen_box(4, 3, 5, 4, 3, 17, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.gen_box((t.sc.w - 5), 3, 5, (t.sc.w - 5), 3, 17, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.gen_box(3, 1, 5, 4, 2, 16, state("air", &[]), state("air", &[]), (0 != 0));
    sp.gen_box((t.sc.w - 6), 1, 5, (t.sc.w - 5), 2, 16, state("air", &[]), state("air", &[]), (0 != 0));
    l12 = 5;
    pc = 1785;
    }
    1785 => {
    pc = if l12 > 17 { 1876 } else { 1792 };
    }
    1792 => {
    sp.place(state("cut_sandstone", &[]), 4, 1, l12);
    sp.place(state("chiseled_sandstone", &[]), 4, 2, l12);
    sp.place(state("cut_sandstone", &[]), (t.sc.w - 5), 1, l12);
    sp.place(state("chiseled_sandstone", &[]), (t.sc.w - 5), 2, l12);
    l12 += 2;
    pc = 1785;
    }
    1876 => {
    sp.place(state("orange_terracotta", &[]), 10, 0, 7);
    sp.place(state("orange_terracotta", &[]), 10, 0, 8);
    sp.place(state("orange_terracotta", &[]), 9, 0, 9);
    sp.place(state("orange_terracotta", &[]), 11, 0, 9);
    sp.place(state("orange_terracotta", &[]), 8, 0, 10);
    sp.place(state("orange_terracotta", &[]), 12, 0, 10);
    sp.place(state("orange_terracotta", &[]), 7, 0, 10);
    sp.place(state("orange_terracotta", &[]), 13, 0, 10);
    sp.place(state("orange_terracotta", &[]), 9, 0, 11);
    sp.place(state("orange_terracotta", &[]), 11, 0, 11);
    sp.place(state("orange_terracotta", &[]), 10, 0, 12);
    sp.place(state("orange_terracotta", &[]), 10, 0, 13);
    sp.place(state("blue_terracotta", &[]), 10, 0, 10);
    l12 = 0;
    pc = 2113;
    }
    2113 => {
    pc = if l12 > (t.sc.w - 1) { 2504 } else { 2124 };
    }
    2124 => {
    sp.place(state("cut_sandstone", &[]), l12, 2, 1);
    sp.place(state("orange_terracotta", &[]), l12, 2, 2);
    sp.place(state("cut_sandstone", &[]), l12, 2, 3);
    sp.place(state("cut_sandstone", &[]), l12, 3, 1);
    sp.place(state("orange_terracotta", &[]), l12, 3, 2);
    sp.place(state("cut_sandstone", &[]), l12, 3, 3);
    sp.place(state("orange_terracotta", &[]), l12, 4, 1);
    sp.place(state("chiseled_sandstone", &[]), l12, 4, 2);
    sp.place(state("orange_terracotta", &[]), l12, 4, 3);
    sp.place(state("cut_sandstone", &[]), l12, 5, 1);
    sp.place(state("orange_terracotta", &[]), l12, 5, 2);
    sp.place(state("cut_sandstone", &[]), l12, 5, 3);
    sp.place(state("orange_terracotta", &[]), l12, 6, 1);
    sp.place(state("chiseled_sandstone", &[]), l12, 6, 2);
    sp.place(state("orange_terracotta", &[]), l12, 6, 3);
    sp.place(state("orange_terracotta", &[]), l12, 7, 1);
    sp.place(state("orange_terracotta", &[]), l12, 7, 2);
    sp.place(state("orange_terracotta", &[]), l12, 7, 3);
    sp.place(state("cut_sandstone", &[]), l12, 8, 1);
    sp.place(state("cut_sandstone", &[]), l12, 8, 2);
    sp.place(state("cut_sandstone", &[]), l12, 8, 3);
    l12 = (l12 + (t.sc.w - 1));
    pc = 2113;
    }
    2504 => {
    l12 = 2;
    pc = 2507;
    }
    2507 => {
    pc = if l12 > (t.sc.w - 3) { 2928 } else { 2518 };
    }
    2518 => {
    sp.place(state("cut_sandstone", &[]), (l12 - 1), 2, 0);
    sp.place(state("orange_terracotta", &[]), l12, 2, 0);
    sp.place(state("cut_sandstone", &[]), (l12 + 1), 2, 0);
    sp.place(state("cut_sandstone", &[]), (l12 - 1), 3, 0);
    sp.place(state("orange_terracotta", &[]), l12, 3, 0);
    sp.place(state("cut_sandstone", &[]), (l12 + 1), 3, 0);
    sp.place(state("orange_terracotta", &[]), (l12 - 1), 4, 0);
    sp.place(state("chiseled_sandstone", &[]), l12, 4, 0);
    sp.place(state("orange_terracotta", &[]), (l12 + 1), 4, 0);
    sp.place(state("cut_sandstone", &[]), (l12 - 1), 5, 0);
    sp.place(state("orange_terracotta", &[]), l12, 5, 0);
    sp.place(state("cut_sandstone", &[]), (l12 + 1), 5, 0);
    sp.place(state("orange_terracotta", &[]), (l12 - 1), 6, 0);
    sp.place(state("chiseled_sandstone", &[]), l12, 6, 0);
    sp.place(state("orange_terracotta", &[]), (l12 + 1), 6, 0);
    sp.place(state("orange_terracotta", &[]), (l12 - 1), 7, 0);
    sp.place(state("orange_terracotta", &[]), l12, 7, 0);
    sp.place(state("orange_terracotta", &[]), (l12 + 1), 7, 0);
    sp.place(state("cut_sandstone", &[]), (l12 - 1), 8, 0);
    sp.place(state("cut_sandstone", &[]), l12, 8, 0);
    sp.place(state("cut_sandstone", &[]), (l12 + 1), 8, 0);
    l12 = (l12 + ((t.sc.w - 3) - 2));
    pc = 2507;
    }
    2928 => {
    sp.gen_box(8, 4, 0, 12, 6, 0, state("cut_sandstone", &[]), state("cut_sandstone", &[]), (0 != 0));
    sp.place(state("air", &[]), 8, 6, 0);
    sp.place(state("air", &[]), 12, 6, 0);
    sp.place(state("orange_terracotta", &[]), 9, 5, 0);
    sp.place(state("chiseled_sandstone", &[]), 10, 5, 0);
    sp.place(state("orange_terracotta", &[]), 11, 5, 0);
    sp.gen_box(8, -14, 8, 12, -11, 12, state("cut_sandstone", &[]), state("cut_sandstone", &[]), (0 != 0));
    sp.gen_box(8, -10, 8, 12, -10, 12, state("chiseled_sandstone", &[]), state("chiseled_sandstone", &[]), (0 != 0));
    sp.gen_box(8, -9, 8, 12, -9, 12, state("cut_sandstone", &[]), state("cut_sandstone", &[]), (0 != 0));
    sp.gen_box(8, -8, 8, 12, -1, 12, state("sandstone", &[]), state("sandstone", &[]), (0 != 0));
    sp.gen_box(9, -11, 9, 11, -1, 11, state("air", &[]), state("air", &[]), (0 != 0));
    sp.place(state("stone_pressure_plate", &[]), 10, -11, 10);
    sp.gen_box(9, -13, 9, 11, -13, 11, state("tnt", &[]), state("air", &[]), (0 != 0));
    sp.place(state("air", &[]), 8, -11, 10);
    sp.place(state("air", &[]), 8, -10, 10);
    sp.place(state("chiseled_sandstone", &[]), 7, -10, 10);
    sp.place(state("cut_sandstone", &[]), 7, -11, 10);
    sp.place(state("air", &[]), 12, -11, 10);
    sp.place(state("air", &[]), 12, -10, 10);
    sp.place(state("chiseled_sandstone", &[]), 13, -10, 10);
    sp.place(state("cut_sandstone", &[]), 13, -11, 10);
    sp.place(state("air", &[]), 10, -11, 8);
    sp.place(state("air", &[]), 10, -10, 8);
    sp.place(state("chiseled_sandstone", &[]), 10, -10, 7);
    sp.place(state("cut_sandstone", &[]), 10, -11, 7);
    sp.place(state("air", &[]), 10, -11, 12);
    sp.place(state("air", &[]), 10, -10, 12);
    sp.place(state("chiseled_sandstone", &[]), 10, -10, 13);
    sp.place(state("cut_sandstone", &[]), 10, -11, 13);
    a12_stdsliceIterstaticu8 = HORIZONTAL.iter();
    pc = 3565;
    }
    3565 => {
    let t3 = a12_stdsliceIterstaticu8.len() > 0;
    pc = if !(t3) { 3655 } else { 3575 };
    }
    3575 => {
    let t4 = *a12_stdsliceIterstaticu8.next().unwrap();
    a13_u8 = t4;
    pc = if t.placed[piece::data2d(a13_u8) as usize].load(Relaxed) { 3652 } else { 3600 };
    }
    3600 => {
    l14 = (piece::step(a13_u8)[0] * 2);
    l15 = (piece::step(a13_u8)[2] * 2);
    let t5 = sp.create_chest(r, (10 + l14), -11, (10 + l15));
    t.placed[piece::data2d(a13_u8) as usize].store(t5, Relaxed);
    pc = 3652;
    }
    3652 => {
    pc = 3565;
    }
    3655 => {
    m_add_cellar(t, sp, r, lr);
    return;
    }
    _ => unreachable!(),
    }
    }
}

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_add_cellar(t: &DesertPyramid, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng) {
    let mut a3_P: P = [0, 0, 0];
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    a3_P = [16, -4, 13];
    m_add_cellar_stairs(t, sp, r, lr, a3_P);
    m_add_cellar_room(t, sp, r, lr, a3_P);
    return;
    }
    _ => unreachable!(),
    }
    }
}

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_add_cellar_stairs(t: &DesertPyramid, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a0: P) {
    let mut a7_BlockState: BlockState = blocks::AIR;
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l4: i32 = 0;
    let mut l5: i32 = 0;
    let mut l6: i32 = 0;
    let mut sk2_BlockState: BlockState = blocks::AIR;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    l4 = a0[0];
    l5 = a0[1];
    l6 = a0[2];
    a7_BlockState = state("sandstone_stairs", &[]);
    sp.place(rotate_state(a7_BlockState, 3), 13, -1, 17);
    sp.place(rotate_state(a7_BlockState, 3), 14, -2, 17);
    sp.place(rotate_state(a7_BlockState, 3), 15, -3, 17);
    a8_BlockState = state("sand", &[]);
    a9_BlockState = state("sandstone", &[]);
    let t1 = lr.next_bool();
    l10 = (t1) as i32;
    sp.place(a8_BlockState, (l4 - 4), (l5 + 4), (l6 + 4));
    sp.place(a8_BlockState, (l4 - 3), (l5 + 4), (l6 + 4));
    sp.place(a8_BlockState, (l4 - 2), (l5 + 4), (l6 + 4));
    sp.place(a8_BlockState, (l4 - 1), (l5 + 4), (l6 + 4));
    sp.place(a8_BlockState, l4, (l5 + 4), (l6 + 4));
    sp.place(a8_BlockState, (l4 - 2), (l5 + 3), (l6 + 4));
    pc = if l10 == 0 { 244 } else { 239 };
    }
    239 => {
    sk2_BlockState = a8_BlockState;
    pc = 246;
    }
    244 => {
    sk2_BlockState = a9_BlockState;
    pc = 246;
    }
    246 => {
    sp.place(sk2_BlockState, (l4 - 1), (l5 + 3), (l6 + 4));
    pc = if l10 != 0 { 274 } else { 269 };
    }
    269 => {
    sk2_BlockState = a8_BlockState;
    pc = 276;
    }
    274 => {
    sk2_BlockState = a9_BlockState;
    pc = 276;
    }
    276 => {
    sp.place(sk2_BlockState, l4, (l5 + 3), (l6 + 4));
    sp.place(a8_BlockState, (l4 - 1), (l5 + 2), (l6 + 4));
    sp.place(a9_BlockState, l4, (l5 + 2), (l6 + 4));
    sp.place(a8_BlockState, l4, (l5 + 1), (l6 + 4));
    return;
    }
    _ => unreachable!(),
    }
    }
}

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_add_cellar_room(t: &DesertPyramid, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a0: P) {
    let mut a10_BlockState: BlockState = blocks::AIR;
    let mut a7_BlockState: BlockState = blocks::AIR;
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l4: i32 = 0;
    let mut l5: i32 = 0;
    let mut l6: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    l4 = a0[0];
    l5 = a0[1];
    l6 = a0[2];
    a7_BlockState = state("cut_sandstone", &[]);
    a8_BlockState = state("chiseled_sandstone", &[]);
    sp.gen_box((l4 - 3), (l5 + 1), (l6 - 3), (l4 - 3), (l5 + 1), (l6 + 2), a7_BlockState, a7_BlockState, (1 != 0));
    sp.gen_box((l4 + 3), (l5 + 1), (l6 - 3), (l4 + 3), (l5 + 1), (l6 + 2), a7_BlockState, a7_BlockState, (1 != 0));
    sp.gen_box((l4 - 3), (l5 + 1), (l6 - 3), (l4 + 3), (l5 + 1), (l6 - 2), a7_BlockState, a7_BlockState, (1 != 0));
    sp.gen_box((l4 - 3), (l5 + 1), (l6 + 3), (l4 + 3), (l5 + 1), (l6 + 3), a7_BlockState, a7_BlockState, (1 != 0));
    sp.gen_box((l4 - 3), (l5 + 2), (l6 - 3), (l4 - 3), (l5 + 2), (l6 + 2), a8_BlockState, a8_BlockState, (1 != 0));
    sp.gen_box((l4 + 3), (l5 + 2), (l6 - 3), (l4 + 3), (l5 + 2), (l6 + 2), a8_BlockState, a8_BlockState, (1 != 0));
    sp.gen_box((l4 - 3), (l5 + 2), (l6 - 3), (l4 + 3), (l5 + 2), (l6 - 2), a8_BlockState, a8_BlockState, (1 != 0));
    sp.gen_box((l4 - 3), (l5 + 2), (l6 + 3), (l4 + 3), (l5 + 2), (l6 + 3), a8_BlockState, a8_BlockState, (1 != 0));
    sp.gen_box((l4 - 3), -1, (l6 - 3), (l4 - 3), -1, (l6 + 2), a7_BlockState, a7_BlockState, (1 != 0));
    sp.gen_box((l4 + 3), -1, (l6 - 3), (l4 + 3), -1, (l6 + 2), a7_BlockState, a7_BlockState, (1 != 0));
    sp.gen_box((l4 - 3), -1, (l6 - 3), (l4 + 3), -1, (l6 - 2), a7_BlockState, a7_BlockState, (1 != 0));
    sp.gen_box((l4 - 3), -1, (l6 + 3), (l4 + 3), -1, (l6 + 3), a7_BlockState, a7_BlockState, (1 != 0));
    m_place_sand_box(t, sp, r, lr, (l4 - 2), (l5 + 1), (l6 - 2), (l4 + 2), (l5 + 3), (l6 + 2));
    m_place_collapsed_roof(t, sp, r, lr, (l4 - 2), (l5 + 4), (l6 - 2), (l4 + 2), (l6 + 2));
    a9_BlockState = state("orange_terracotta", &[]);
    a10_BlockState = state("blue_terracotta", &[]);
    sp.place(a10_BlockState, l4, l5, l6);
    sp.place(a9_BlockState, (l4 + 1), l5, (l6 - 1));
    sp.place(a9_BlockState, (l4 + 1), l5, (l6 + 1));
    sp.place(a9_BlockState, (l4 - 1), l5, (l6 - 1));
    sp.place(a9_BlockState, (l4 - 1), l5, (l6 + 1));
    sp.place(a9_BlockState, (l4 + 2), l5, l6);
    sp.place(a9_BlockState, (l4 - 2), l5, l6);
    sp.place(a9_BlockState, l4, l5, (l6 + 2));
    sp.place(a9_BlockState, l4, l5, (l6 - 2));
    sp.place(a9_BlockState, (l4 + 3), l5, l6);
    m_place_sand(t, sp, r, lr, (l4 + 3), (l5 + 1), l6);
    m_place_sand(t, sp, r, lr, (l4 + 3), (l5 + 2), l6);
    sp.place(a7_BlockState, (l4 + 4), (l5 + 1), l6);
    sp.place(a8_BlockState, (l4 + 4), (l5 + 2), l6);
    sp.place(a9_BlockState, (l4 - 3), l5, l6);
    m_place_sand(t, sp, r, lr, (l4 - 3), (l5 + 1), l6);
    m_place_sand(t, sp, r, lr, (l4 - 3), (l5 + 2), l6);
    sp.place(a7_BlockState, (l4 - 4), (l5 + 1), l6);
    sp.place(a8_BlockState, (l4 - 4), (l5 + 2), l6);
    sp.place(a9_BlockState, l4, l5, (l6 + 3));
    m_place_sand(t, sp, r, lr, l4, (l5 + 1), (l6 + 3));
    m_place_sand(t, sp, r, lr, l4, (l5 + 2), (l6 + 3));
    sp.place(a9_BlockState, l4, l5, (l6 - 3));
    m_place_sand(t, sp, r, lr, l4, (l5 + 1), (l6 - 3));
    m_place_sand(t, sp, r, lr, l4, (l5 + 2), (l6 - 3));
    sp.place(a7_BlockState, l4, (l5 + 1), (l6 - 4));
    sp.place(a8_BlockState, l4, -2, (l6 - 4));
    return;
    }
    _ => unreachable!(),
    }
    }
}

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_place_sand(t: &DesertPyramid, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a0: i32, a1: i32, a2: i32) {
    let mut a4_P: P = [0, 0, 0];
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = sp.pos(a0, a1, a2);
    a4_P = t1;
    t.sand.lock().push(a4_P);
    return;
    }
    _ => unreachable!(),
    }
    }
}

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_place_sand_box(t: &DesertPyramid, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32, a5: i32) {
    let mut l7: i32 = 0;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    l7 = a1;
    pc = 3;
    }
    3 => {
    pc = if l7 > a4 { 58 } else { 10 };
    }
    10 => {
    l8 = a0;
    pc = 13;
    }
    13 => {
    pc = if l8 > a3 { 52 } else { 20 };
    }
    20 => {
    l9 = a2;
    pc = 23;
    }
    23 => {
    pc = if l9 > a5 { 46 } else { 30 };
    }
    30 => {
    m_place_sand(t, sp, r, lr, l8, l7, l9);
    l9 += 1;
    pc = 23;
    }
    46 => {
    l8 += 1;
    pc = 13;
    }
    52 => {
    l7 += 1;
    pc = 3;
    }
    58 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_place_collapsed_roof_piece(t: &DesertPyramid, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a1: i32, a2: i32, a3: i32) {
    let mut a6_BlockState: BlockState = blocks::AIR;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = lr.next_float();
    pc = if t1 >= 0.33_f32 { 42 } else { 18 };
    }
    18 => {
    a6_BlockState = state("sandstone", &[]);
    sp.place(a6_BlockState, a1, a2, a3);
    pc = 63;
    }
    42 => {
    a6_BlockState = state("sand", &[]);
    sp.place(a6_BlockState, a1, a2, a3);
    pc = 63;
    }
    63 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_place_collapsed_roof(t: &DesertPyramid, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a2: i32, a3: i32, a4: i32, a5: i32, a6: i32) {
    let mut a8_Legacy: Legacy = Legacy::new(0);
    let mut l10: i32 = 0;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    l8 = a2;
    pc = 3;
    }
    3 => {
    pc = if l8 > a5 { 45 } else { 10 };
    }
    10 => {
    l9 = a4;
    pc = 14;
    }
    14 => {
    pc = if l9 > a6 { 39 } else { 21 };
    }
    21 => {
    m_place_collapsed_roof_piece(t, sp, r, lr, l8, a3, l9);
    l9 += 1;
    pc = 14;
    }
    39 => {
    l8 += 1;
    pc = 3;
    }
    45 => {
    let t1 = sp.pos(a2, a3, a4);
    a8_Legacy = Legacy::new(sp.lv.seed).fork_positional().at(t1[0], t1[1], t1[2]);
    let t2 = { let (a, b) = (a2, a5); a + a8_Legacy.next_int_bound(b - a + 1) };
    l9 = t2;
    let t3 = { let (a, b) = (a4, a6); a + a8_Legacy.next_int_bound(b - a + 1) };
    l10 = t3;
    let t4 = sp.wx(l9, l10);
    let t5 = sp.wy(a3);
    let t6 = sp.wz(l9, l10);
    *t.roof.lock() = [t4, t5, t6];
    return;
    }
    _ => unreachable!(),
    }
    }
}

#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn j_post_process(t: &JungleTemple, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a10_BlockState: BlockState = blocks::AIR;
    let mut a11_BlockState: BlockState = blocks::AIR;
    let mut a12_BlockState: BlockState = blocks::AIR;
    let mut a13_BlockState: BlockState = blocks::AIR;
    let mut a8_BlockState: BlockState = blocks::AIR;
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l12: i32 = 0;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = t.sc.avg_ground(sp, 0);
    pc = if t1 { 12 } else { 11 };
    }
    11 => {
    return;
    }
    12 => {
    sp.gen_box_sel(0, -4, 0, (t.sc.w - 1), 0, (t.sc.d - 1), (0 != 0), r, &MossStone);
    sp.gen_box_sel(2, 1, 2, 9, 2, 2, (0 != 0), r, &MossStone);
    sp.gen_box_sel(2, 1, 12, 9, 2, 12, (0 != 0), r, &MossStone);
    sp.gen_box_sel(2, 1, 3, 2, 2, 11, (0 != 0), r, &MossStone);
    sp.gen_box_sel(9, 1, 3, 9, 2, 11, (0 != 0), r, &MossStone);
    sp.gen_box_sel(1, 3, 1, 10, 6, 1, (0 != 0), r, &MossStone);
    sp.gen_box_sel(1, 3, 13, 10, 6, 13, (0 != 0), r, &MossStone);
    sp.gen_box_sel(1, 3, 2, 1, 6, 12, (0 != 0), r, &MossStone);
    sp.gen_box_sel(10, 3, 2, 10, 6, 12, (0 != 0), r, &MossStone);
    sp.gen_box_sel(2, 3, 2, 9, 3, 12, (0 != 0), r, &MossStone);
    sp.gen_box_sel(2, 6, 2, 9, 6, 12, (0 != 0), r, &MossStone);
    sp.gen_box_sel(3, 7, 3, 8, 7, 11, (0 != 0), r, &MossStone);
    sp.gen_box_sel(4, 8, 4, 7, 8, 10, (0 != 0), r, &MossStone);
    sp.air_box(3, 1, 3, 8, 2, 11);
    sp.air_box(4, 3, 6, 7, 3, 9);
    sp.air_box(2, 4, 2, 9, 5, 12);
    sp.air_box(4, 6, 5, 7, 6, 9);
    sp.air_box(5, 7, 6, 6, 7, 8);
    sp.air_box(5, 1, 2, 6, 2, 2);
    sp.air_box(5, 2, 12, 6, 2, 12);
    sp.air_box(5, 5, 1, 6, 5, 1);
    sp.air_box(5, 5, 13, 6, 5, 13);
    sp.place(state("air", &[]), 1, 5, 5);
    sp.place(state("air", &[]), 10, 5, 5);
    sp.place(state("air", &[]), 1, 5, 9);
    sp.place(state("air", &[]), 10, 5, 9);
    l8 = 0;
    pc = 516;
    }
    516 => {
    pc = if l8 > 14 { 617 } else { 523 };
    }
    523 => {
    sp.gen_box_sel(2, 4, l8, 2, 5, l8, (0 != 0), r, &MossStone);
    sp.gen_box_sel(4, 4, l8, 4, 5, l8, (0 != 0), r, &MossStone);
    sp.gen_box_sel(7, 4, l8, 7, 5, l8, (0 != 0), r, &MossStone);
    sp.gen_box_sel(9, 4, l8, 9, 5, l8, (0 != 0), r, &MossStone);
    l8 += 14;
    pc = 516;
    }
    617 => {
    sp.gen_box_sel(5, 6, 0, 6, 6, 0, (0 != 0), r, &MossStone);
    l8 = 0;
    pc = 642;
    }
    642 => {
    pc = if l8 > 11 { 742 } else { 649 };
    }
    649 => {
    l9 = 2;
    pc = 652;
    }
    652 => {
    pc = if l9 > 12 { 688 } else { 659 };
    }
    659 => {
    sp.gen_box_sel(l8, 4, l9, l8, 5, l9, (0 != 0), r, &MossStone);
    l9 += 2;
    pc = 652;
    }
    688 => {
    sp.gen_box_sel(l8, 6, 5, l8, 6, 5, (0 != 0), r, &MossStone);
    sp.gen_box_sel(l8, 6, 9, l8, 6, 9, (0 != 0), r, &MossStone);
    l8 += 11;
    pc = 642;
    }
    742 => {
    sp.gen_box_sel(2, 7, 2, 2, 9, 2, (0 != 0), r, &MossStone);
    sp.gen_box_sel(9, 7, 2, 9, 9, 2, (0 != 0), r, &MossStone);
    sp.gen_box_sel(2, 7, 12, 2, 9, 12, (0 != 0), r, &MossStone);
    sp.gen_box_sel(9, 7, 12, 9, 9, 12, (0 != 0), r, &MossStone);
    sp.gen_box_sel(4, 9, 4, 4, 9, 4, (0 != 0), r, &MossStone);
    sp.gen_box_sel(7, 9, 4, 7, 9, 4, (0 != 0), r, &MossStone);
    sp.gen_box_sel(4, 9, 10, 4, 9, 10, (0 != 0), r, &MossStone);
    sp.gen_box_sel(7, 9, 10, 7, 9, 10, (0 != 0), r, &MossStone);
    sp.gen_box_sel(5, 9, 7, 6, 9, 7, (0 != 0), r, &MossStone);
    a8_BlockState = with(state("cobblestone_stairs", &[]), "facing", "east");
    a9_BlockState = with(state("cobblestone_stairs", &[]), "facing", "west");
    a10_BlockState = with(state("cobblestone_stairs", &[]), "facing", "south");
    a11_BlockState = with(state("cobblestone_stairs", &[]), "facing", "north");
    sp.place(a11_BlockState, 5, 9, 6);
    sp.place(a11_BlockState, 6, 9, 6);
    sp.place(a10_BlockState, 5, 9, 8);
    sp.place(a10_BlockState, 6, 9, 8);
    sp.place(a11_BlockState, 4, 0, 0);
    sp.place(a11_BlockState, 5, 0, 0);
    sp.place(a11_BlockState, 6, 0, 0);
    sp.place(a11_BlockState, 7, 0, 0);
    sp.place(a11_BlockState, 4, 1, 8);
    sp.place(a11_BlockState, 4, 2, 9);
    sp.place(a11_BlockState, 4, 3, 10);
    sp.place(a11_BlockState, 7, 1, 8);
    sp.place(a11_BlockState, 7, 2, 9);
    sp.place(a11_BlockState, 7, 3, 10);
    sp.gen_box_sel(4, 1, 9, 4, 1, 9, (0 != 0), r, &MossStone);
    sp.gen_box_sel(7, 1, 9, 7, 1, 9, (0 != 0), r, &MossStone);
    sp.gen_box_sel(4, 1, 10, 7, 2, 10, (0 != 0), r, &MossStone);
    sp.gen_box_sel(5, 4, 5, 6, 4, 5, (0 != 0), r, &MossStone);
    sp.place(a8_BlockState, 4, 4, 5);
    sp.place(a9_BlockState, 7, 4, 5);
    l12 = 0;
    pc = 1333;
    }
    1333 => {
    pc = if l12 >= 4 { 1412 } else { 1339 };
    }
    1339 => {
    sp.place(a10_BlockState, 5, (0 - l12), (6 + l12));
    sp.place(a10_BlockState, 6, (0 - l12), (6 + l12));
    sp.air_box(5, (0 - l12), (7 + l12), 6, (0 - l12), (9 + l12));
    l12 += 1;
    pc = 1333;
    }
    1412 => {
    sp.air_box(1, -3, 12, 10, -1, 13);
    sp.air_box(1, -3, 1, 3, -1, 13);
    sp.air_box(1, -3, 1, 9, -1, 5);
    l12 = 1;
    pc = 1462;
    }
    1462 => {
    pc = if l12 > 13 { 1498 } else { 1469 };
    }
    1469 => {
    sp.gen_box_sel(1, -3, l12, 1, -2, l12, (0 != 0), r, &MossStone);
    l12 += 2;
    pc = 1462;
    }
    1498 => {
    l12 = 2;
    pc = 1501;
    }
    1501 => {
    pc = if l12 > 12 { 1535 } else { 1508 };
    }
    1508 => {
    sp.gen_box_sel(1, -1, l12, 3, -1, l12, (0 != 0), r, &MossStone);
    l12 += 2;
    pc = 1501;
    }
    1535 => {
    sp.gen_box_sel(2, -2, 1, 5, -2, 1, (0 != 0), r, &MossStone);
    sp.gen_box_sel(7, -2, 1, 9, -2, 1, (0 != 0), r, &MossStone);
    sp.gen_box_sel(6, -3, 1, 6, -3, 1, (0 != 0), r, &MossStone);
    sp.gen_box_sel(6, -1, 1, 6, -1, 1, (0 != 0), r, &MossStone);
    sp.place(with(with(state("tripwire_hook", &[]), "facing", "east"), "attached", if (1 != 0) { "true" } else { "false" }), 1, -3, 8);
    sp.place(with(with(state("tripwire_hook", &[]), "facing", "west"), "attached", if (1 != 0) { "true" } else { "false" }), 4, -3, 8);
    sp.place(with(with(with(state("tripwire", &[]), "east", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), "attached", if (1 != 0) { "true" } else { "false" }), 2, -3, 8);
    sp.place(with(with(with(state("tripwire", &[]), "east", if (1 != 0) { "true" } else { "false" }), "west", if (1 != 0) { "true" } else { "false" }), "attached", if (1 != 0) { "true" } else { "false" }), 3, -3, 8);
    a12_BlockState = with(with(state("redstone_wire", &[]), "north", "side"), "south", "side");
    sp.place(a12_BlockState, 5, -3, 7);
    sp.place(a12_BlockState, 5, -3, 6);
    sp.place(a12_BlockState, 5, -3, 5);
    sp.place(a12_BlockState, 5, -3, 4);
    sp.place(a12_BlockState, 5, -3, 3);
    sp.place(a12_BlockState, 5, -3, 2);
    sp.place(with(with(state("redstone_wire", &[]), "north", "side"), "west", "side"), 5, -3, 1);
    sp.place(with(with(state("redstone_wire", &[]), "east", "side"), "west", "side"), 4, -3, 1);
    sp.place(state("mossy_cobblestone", &[]), 3, -3, 1);
    pc = if t.placed_trap1.load(Relaxed) { 2064 } else { 2041 };
    }
    2041 => {
    let t2 = sp.create_dispenser(r, 3, -2, 1, piece::dir_parse_name("north"));
    t.placed_trap1.store(t2, Relaxed);
    pc = 2064;
    }
    2064 => {
    sp.place(with(state("vine", &[]), "south", if (1 != 0) { "true" } else { "false" }), 3, -2, 2);
    sp.place(with(with(state("tripwire_hook", &[]), "facing", "north"), "attached", if (1 != 0) { "true" } else { "false" }), 7, -3, 1);
    sp.place(with(with(state("tripwire_hook", &[]), "facing", "south"), "attached", if (1 != 0) { "true" } else { "false" }), 7, -3, 5);
    sp.place(with(with(with(state("tripwire", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), "attached", if (1 != 0) { "true" } else { "false" }), 7, -3, 2);
    sp.place(with(with(with(state("tripwire", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), "attached", if (1 != 0) { "true" } else { "false" }), 7, -3, 3);
    sp.place(with(with(with(state("tripwire", &[]), "north", if (1 != 0) { "true" } else { "false" }), "south", if (1 != 0) { "true" } else { "false" }), "attached", if (1 != 0) { "true" } else { "false" }), 7, -3, 4);
    sp.place(with(with(state("redstone_wire", &[]), "east", "side"), "west", "side"), 8, -3, 6);
    sp.place(with(with(state("redstone_wire", &[]), "west", "side"), "south", "side"), 9, -3, 6);
    sp.place(with(with(state("redstone_wire", &[]), "north", "side"), "south", "up"), 9, -3, 5);
    sp.place(state("mossy_cobblestone", &[]), 9, -3, 4);
    sp.place(a12_BlockState, 9, -2, 4);
    pc = if t.placed_trap2.load(Relaxed) { 2542 } else { 2518 };
    }
    2518 => {
    let t3 = sp.create_dispenser(r, 9, -2, 3, piece::dir_parse_name("west"));
    t.placed_trap2.store(t3, Relaxed);
    pc = 2542;
    }
    2542 => {
    sp.place(with(state("vine", &[]), "east", if (1 != 0) { "true" } else { "false" }), 8, -1, 3);
    sp.place(with(state("vine", &[]), "east", if (1 != 0) { "true" } else { "false" }), 8, -2, 3);
    pc = if t.placed_main_chest.load(Relaxed) { 2631 } else { 2610 };
    }
    2610 => {
    let t4 = sp.create_chest(r, 8, -3, 3);
    t.placed_main_chest.store(t4, Relaxed);
    pc = 2631;
    }
    2631 => {
    sp.place(state("mossy_cobblestone", &[]), 9, -3, 2);
    sp.place(state("mossy_cobblestone", &[]), 8, -3, 1);
    sp.place(state("mossy_cobblestone", &[]), 4, -3, 5);
    sp.place(state("mossy_cobblestone", &[]), 5, -2, 5);
    sp.place(state("mossy_cobblestone", &[]), 5, -1, 5);
    sp.place(state("mossy_cobblestone", &[]), 6, -3, 5);
    sp.place(state("mossy_cobblestone", &[]), 7, -2, 5);
    sp.place(state("mossy_cobblestone", &[]), 7, -1, 5);
    sp.place(state("mossy_cobblestone", &[]), 8, -3, 5);
    sp.gen_box_sel(9, -1, 1, 9, -1, 5, (0 != 0), r, &MossStone);
    sp.air_box(8, -3, 8, 10, -1, 10);
    sp.place(state("chiseled_stone_bricks", &[]), 8, -2, 11);
    sp.place(state("chiseled_stone_bricks", &[]), 9, -2, 11);
    sp.place(state("chiseled_stone_bricks", &[]), 10, -2, 11);
    a13_BlockState = with(with(state("lever", &[]), "facing", "north"), "face", "wall");
    sp.place(a13_BlockState, 8, -2, 12);
    sp.place(a13_BlockState, 9, -2, 12);
    sp.place(a13_BlockState, 10, -2, 12);
    sp.gen_box_sel(8, -3, 8, 8, -3, 10, (0 != 0), r, &MossStone);
    sp.gen_box_sel(10, -3, 8, 10, -3, 10, (0 != 0), r, &MossStone);
    sp.place(state("mossy_cobblestone", &[]), 10, -2, 9);
    sp.place(a12_BlockState, 8, -2, 9);
    sp.place(a12_BlockState, 8, -2, 10);
    sp.place(with(with(with(with(state("redstone_wire", &[]), "north", "side"), "south", "side"), "east", "side"), "west", "side"), 10, -1, 9);
    sp.place(with(state("sticky_piston", &[]), "facing", "up"), 9, -2, 8);
    sp.place(with(state("sticky_piston", &[]), "facing", "west"), 10, -2, 8);
    sp.place(with(state("sticky_piston", &[]), "facing", "west"), 10, -1, 8);
    sp.place(with(state("repeater", &[]), "facing", "north"), 10, -2, 10);
    pc = if t.placed_hidden_chest.load(Relaxed) { 3278 } else { 3256 };
    }
    3256 => {
    let t5 = sp.create_chest(r, 9, -3, 10);
    t.placed_hidden_chest.store(t5, Relaxed);
    pc = 3278;
    }
    3278 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}

