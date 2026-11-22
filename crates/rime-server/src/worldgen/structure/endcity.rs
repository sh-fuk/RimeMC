//! `EndCityPieces`: the recursive tower/bridge/house generator of end cities.

use std::sync::Arc;

use super::template::{bb_intersects, transform, Template};
use super::{Bb, P};
use crate::worldgen::random::Rng;

/// A placed end city template (`EndCityPiece`).
pub struct Ecp {
    pub name: &'static str,
    pub pos: P,
    pub rot: i32,
    /// `overwrite`: ignore only structure blocks (else also air)
    pub overwrite: bool,
    pub depth: i32,
    pub bb: Bb,
}

const TOWER_BRIDGES: [(i32, P); 4] = [(0, [1, -1, 0]), (1, [6, -1, 1]), (3, [0, -1, 5]), (2, [5, -1, 6])];
const FAT_TOWER_BRIDGES: [(i32, P); 4] = [(0, [4, -1, 0]), (1, [12, -1, 4]), (3, [0, -1, 8]), (2, [8, -1, 12])];

#[derive(Clone, Copy)]
enum Gen {
    House,
    Tower,
    Bridge,
    Fat,
}

struct Builder<'a> {
    tpl: &'a dyn Fn(&str) -> Arc<Template>,
    r: &'a mut dyn Rng,
    ship: bool,
}

impl Builder<'_> {
    fn piece(&self, name: &'static str, pos: P, rot: i32, overwrite: bool) -> Ecp {
        let bb = (self.tpl)(name).bounding_box(pos, rot, [0, 0, 0], 0);
        Ecp { name, pos, rot, overwrite, depth: 0, bb }
    }

    /// `addPiece`: a template joined to `prev` at `offset` (in its frame)
    fn add(&self, list: &mut Vec<Ecp>, prev: usize, offset: P, name: &'static str, rot: i32, overwrite: bool) -> usize {
        let p = &list[prev];
        let d = transform(offset, 0, p.rot, [0, 0, 0]);
        let pos = [p.pos[0] + d[0], p.pos[1] + d[1], p.pos[2] + d[2]];
        let e = self.piece(name, pos, rot, overwrite);
        list.push(e);
        list.len() - 1
    }

    /// `recursiveChildren`: `parent` indexes `pieces`; children are built in
    /// a fresh list that starts with a copy of the parent. Collisions are
    /// checked against `pieces[start..]` (a nested list skips its parent copy).
    fn children(&mut self, g: Gen, depth: i32, pieces: &mut Vec<Ecp>, start: usize, parent: usize, offset: Option<P>) -> bool {
        if depth > 8 {
            return false;
        }
        let pp = &pieces[parent];
        let mut list = vec![Ecp { name: pp.name, pos: pp.pos, rot: pp.rot, overwrite: pp.overwrite, depth: pp.depth, bb: pp.bb }];
        let parent_depth = pp.depth;
        if self.generate(g, depth, &mut list, offset) {
            list.remove(0);
            let id = self.r.next_int();
            let mut collide = false;
            for p in list.iter_mut() {
                p.depth = id;
                if let Some(c) = pieces[start..].iter().find(|q| bb_intersects(&q.bb, &p.bb)) {
                    if c.depth != parent_depth {
                        collide = true;
                        break;
                    }
                }
            }
            if !collide {
                pieces.extend(list);
                return true;
            }
        }
        false
    }

    /// `SectionGenerator.generate`; `list[0]` is the parent piece.
    fn generate(&mut self, g: Gen, depth: i32, list: &mut Vec<Ecp>, offset: Option<P>) -> bool {
        let rot = list[0].rot;
        match g {
            Gen::House => {
                if depth > 8 {
                    return false;
                }
                let mut p = self.add(list, 0, offset.unwrap_or([0, 0, 0]), "base_floor", rot, true);
                match self.r.next_int_bound(3) {
                    0 => {
                        self.add(list, p, [-1, 4, -1], "base_roof", rot, true);
                    }
                    1 => {
                        p = self.add(list, p, [-1, 0, -1], "second_floor_2", rot, false);
                        p = self.add(list, p, [-1, 8, -1], "second_roof", rot, false);
                        self.children(Gen::Tower, depth + 1, list, 1, p, None);
                    }
                    _ => {
                        p = self.add(list, p, [-1, 0, -1], "second_floor_2", rot, false);
                        p = self.add(list, p, [-1, 4, -1], "third_floor_2", rot, false);
                        p = self.add(list, p, [-1, 8, -1], "third_roof", rot, true);
                        self.children(Gen::Tower, depth + 1, list, 1, p, None);
                    }
                }
                true
            }
            Gen::Tower => {
                let a = 3 + self.r.next_int_bound(2);
                let b = 3 + self.r.next_int_bound(2);
                let mut p = self.add(list, 0, [a, -3, b], "tower_base", rot, true);
                p = self.add(list, p, [0, 7, 0], "tower_piece", rot, true);
                let mut bridge = if self.r.next_int_bound(3) == 0 { Some(p) } else { None };
                let n = 1 + self.r.next_int_bound(3);
                for i in 0..n {
                    p = self.add(list, p, [0, 4, 0], "tower_piece", rot, true);
                    if i < n - 1 && self.r.next_bool() {
                        bridge = Some(p);
                    }
                }
                if let Some(bp) = bridge {
                    for (r2, off) in TOWER_BRIDGES {
                        if self.r.next_bool() {
                            let e = self.add(list, bp, off, "bridge_end", (rot + r2) & 3, true);
                            self.children(Gen::Bridge, depth + 1, list, 1, e, None);
                        }
                    }
                    self.add(list, p, [-1, 4, -1], "tower_top", rot, true);
                } else {
                    if depth != 7 {
                        return self.children(Gen::Fat, depth + 1, list, 1, p, None);
                    }
                    self.add(list, p, [-1, 4, -1], "tower_top", rot, true);
                }
                true
            }
            Gen::Bridge => {
                let n = self.r.next_int_bound(4) + 1;
                let mut p = self.add(list, 0, [0, 0, -4], "bridge_piece", rot, true);
                list[p].depth = -1;
                let mut y = 0;
                for _ in 0..n {
                    if self.r.next_bool() {
                        p = self.add(list, p, [0, y, -4], "bridge_piece", rot, true);
                        y = 0;
                    } else {
                        p = if self.r.next_bool() { self.add(list, p, [0, y, -4], "bridge_steep_stairs", rot, true) } else { self.add(list, p, [0, y, -8], "bridge_gentle_stairs", rot, true) };
                        y = 4;
                    }
                }
                if !self.ship && self.r.next_int_bound(10 - depth) == 0 {
                    let a = -8 + self.r.next_int_bound(8);
                    let b = -70 + self.r.next_int_bound(10);
                    self.add(list, p, [a, y, b], "ship", rot, true);
                    self.ship = true;
                } else if !self.children(Gen::House, depth + 1, list, 1, p, Some([-3, y + 1, -11])) {
                    return false;
                }
                let e = self.add(list, p, [4, y, 0], "bridge_end", (rot + 2) & 3, true);
                list[e].depth = -1;
                true
            }
            Gen::Fat => {
                let mut p = self.add(list, 0, [-3, 4, -3], "fat_tower_base", rot, true);
                p = self.add(list, p, [0, 4, 0], "fat_tower_middle", rot, true);
                let mut i = 0;
                while i < 2 && self.r.next_int_bound(3) != 0 {
                    p = self.add(list, p, [0, 8, 0], "fat_tower_middle", rot, true);
                    for (r2, off) in FAT_TOWER_BRIDGES {
                        if self.r.next_bool() {
                            let e = self.add(list, p, off, "bridge_end", (rot + r2) & 3, true);
                            self.children(Gen::Bridge, depth + 1, list, 1, e, None);
                        }
                    }
                    i += 1;
                }
                self.add(list, p, [-2, 8, -2], "fat_tower_top", rot, true);
                true
            }
        }
    }
}

/// `EndCityPieces.startHouseTower`
pub fn start(tpl: &dyn Fn(&str) -> Arc<Template>, pos: P, rot: i32, r: &mut dyn Rng) -> Vec<Ecp> {
    let mut b = Builder { tpl, r, ship: false };
    let mut pieces = vec![b.piece("base_floor", pos, rot, true)];
    let mut p = b.add(&mut pieces, 0, [-1, 0, -1], "second_floor_1", rot, false);
    p = b.add(&mut pieces, p, [-1, 4, -1], "third_floor_1", rot, false);
    p = b.add(&mut pieces, p, [-1, 8, -1], "third_roof", rot, true);
    b.children(Gen::Tower, 1, &mut pieces, 0, p, None);
    pieces
}

pub const NAMES: [&str; 19] = [
    "base_floor",
    "base_roof",
    "bridge_end",
    "bridge_gentle_stairs",
    "bridge_piece",
    "bridge_steep_stairs",
    "fat_tower_base",
    "fat_tower_middle",
    "fat_tower_top",
    "second_floor_1",
    "second_floor_2",
    "second_roof",
    "ship",
    "third_floor_1",
    "third_floor_2",
    "third_roof",
    "tower_base",
    "tower_piece",
    "tower_top",
];
