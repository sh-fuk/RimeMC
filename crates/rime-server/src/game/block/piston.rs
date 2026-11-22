//! `PistonBaseBlock`, `PistonStructureResolver`, `PistonHeadBlock` and the
//! moving blocks (`MovingPistonBlock` + `PistonMovingBlockEntity`).

use std::collections::HashMap;

use super::*;
use crate::game::chunk_min_max_y;

/// A block in motion (`PistonMovingBlockEntity`).
#[derive(Clone, Copy, Debug)]
pub struct Moving {
    pub moved: BlockState,
    pub dir: u8,
    pub extending: bool,
    pub source: bool,
    pub progress: f32,
    pub progress_o: f32,
    pub last_ticked: u64,
}

fn sticky_piston(s: BlockState) -> bool {
    s.name() == "sticky_piston"
}

fn is_piston(s: BlockState) -> bool {
    matches!(s.name(), "piston" | "sticky_piston")
}

/// `PistonBaseBlock.getNeighborSignal` (with quasi-connectivity)
fn neighbor_signal(g: &Game, p: [i32; 3], facing_dir: u8) -> bool {
    for d in 0..6u8 {
        if d != facing_dir && g.has_signal(rel(p, d), d) {
            return true;
        }
    }
    if g.has_signal(p, DOWN) {
        return true;
    }
    let above = rel(p, UP);
    (0..6u8).any(|d| d != DOWN && g.has_signal(rel(above, d), d))
}

pub fn neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    check_if_extend(g, p, s);
}

/// `PistonBaseBlock.checkIfExtend`
pub fn check_if_extend(g: &Game, p: [i32; 3], s: BlockState) {
    let d = facing(s);
    let b = neighbor_signal(g, p, d);
    let extended = flag(s, "extended");
    if b && !extended {
        if Resolver::new(g, p, d, true).resolve().is_some() {
            g.block_event(p, s, 0, d as i32);
        }
    } else if !b && extended {
        let bp = rel(rel(p, d), d);
        let bs = g.bs(bp);
        let mut i = 1;
        if class(bs) == Class::MovingPiston && facing(bs) == d {
            if let Some(m) = g.lv(|l| l.moving.get(&bp).copied()) {
                if m.extending && (m.progress < 0.5 || g.game_time() == m.last_ticked || g.handling_tick()) {
                    i = 2;
                }
            }
        }
        g.block_event(p, s, i, d as i32);
    }
}

/// `PistonBaseBlock.isPushable`
pub fn is_pushable(g: &Game, s: BlockState, p: [i32; 3], push: u8, allow_destroy: bool, piston_facing: u8) -> bool {
    let (min_y, max_y) = chunk_min_max_y();
    if p[1] < min_y || p[1] > max_y - 1 {
        return false;
    }
    if s.is_air() {
        return true;
    }
    if matches!(s.name(), "obsidian" | "crying_obsidian" | "respawn_anchor" | "reinforced_deepslate") {
        return false;
    }
    if push == DOWN && p[1] == min_y {
        return false;
    }
    if push == UP && p[1] == max_y - 1 {
        return false;
    }
    if !is_piston(s) {
        if rstate::unbreakable(s) {
            return false;
        }
        match rstate::push_reaction(s) {
            2 => return false,
            1 => return allow_destroy,
            4 => return push == piston_facing,
            _ => {}
        }
    } else if flag(s, "extended") {
        return false;
    }
    let _ = g;
    !rstate::block_entity(s)
}

fn is_sticky(s: BlockState) -> bool {
    matches!(s.name(), "slime_block" | "honey_block")
}

fn can_stick(a: BlockState, b: BlockState) -> bool {
    if a.name() == "honey_block" && b.name() == "slime_block" || a.name() == "slime_block" && b.name() == "honey_block" {
        return false;
    }
    is_sticky(a) || is_sticky(b)
}

fn rel_n(p: [i32; 3], d: u8, n: i32) -> [i32; 3] {
    let o = OFFSETS[d as usize];
    [p[0] + o[0] * n, p[1] + o[1] * n, p[2] + o[2] * n]
}

/// `PistonStructureResolver`
pub struct Resolver<'a> {
    g: &'a Game,
    piston: [i32; 3],
    extending: bool,
    start: [i32; 3],
    push: u8,
    piston_dir: u8,
    pub to_push: Vec<[i32; 3]>,
    pub to_destroy: Vec<[i32; 3]>,
}

impl<'a> Resolver<'a> {
    pub fn new(g: &'a Game, piston: [i32; 3], d: u8, extending: bool) -> Self {
        let (push, start) = if extending { (d, rel(piston, d)) } else { (opp(d), rel_n(piston, d, 2)) };
        Resolver { g, piston, extending, start, push, piston_dir: d, to_push: Vec::new(), to_destroy: Vec::new() }
    }

    pub fn resolve(mut self) -> Option<Self> {
        self.to_push.clear();
        self.to_destroy.clear();
        let bs = self.g.bs(self.start);
        if !is_pushable(self.g, bs, self.start, self.push, false, self.piston_dir) {
            if self.extending && rstate::push_reaction(bs) == 1 {
                self.to_destroy.push(self.start);
                return Some(self);
            }
            return None;
        }
        if !self.add_line(self.start, self.push) {
            return None;
        }
        let mut i = 0;
        while i < self.to_push.len() {
            let bp = self.to_push[i];
            if is_sticky(self.g.bs(bp)) && !self.add_branching(bp) {
                return None;
            }
            i += 1;
        }
        Some(self)
    }

    fn add_line(&mut self, origin: [i32; 3], dir: u8) -> bool {
        let g = self.g;
        let mut bs = g.bs(origin);
        if bs.is_air() {
            return true;
        }
        if !is_pushable(g, bs, origin, self.push, false, dir) {
            return true;
        }
        if origin == self.piston || self.to_push.contains(&origin) {
            return true;
        }
        let mut i = 1;
        if i + self.to_push.len() > 12 {
            return false;
        }
        let back = opp(self.push);
        while is_sticky(bs) {
            let bp = rel_n(origin, back, i as i32);
            let prev = bs;
            bs = g.bs(bp);
            if bs.is_air() || !can_stick(prev, bs) || !is_pushable(g, bs, bp, self.push, false, back) || bp == self.piston {
                break;
            }
            i += 1;
            if i + self.to_push.len() > 12 {
                return false;
            }
        }
        let mut j = 0;
        for k in (0..i).rev() {
            self.to_push.push(rel_n(origin, back, k as i32));
            j += 1;
        }
        let mut k = 1;
        loop {
            let bp2 = rel_n(origin, self.push, k);
            if let Some(l) = self.to_push.iter().position(|x| *x == bp2) {
                self.reorder(j, l);
                for m in 0..=l + j {
                    let bp3 = self.to_push[m];
                    if is_sticky(g.bs(bp3)) && !self.add_branching(bp3) {
                        return false;
                    }
                }
                return true;
            }
            let s2 = g.bs(bp2);
            if s2.is_air() {
                return true;
            }
            if !is_pushable(g, s2, bp2, self.push, true, self.push) || bp2 == self.piston {
                return false;
            }
            if rstate::push_reaction(s2) == 1 {
                self.to_destroy.push(bp2);
                return true;
            }
            if self.to_push.len() >= 12 {
                return false;
            }
            self.to_push.push(bp2);
            j += 1;
            k += 1;
        }
    }

    fn reorder(&mut self, a: usize, b: usize) {
        let n = self.to_push.len();
        let mut out = Vec::with_capacity(n);
        out.extend_from_slice(&self.to_push[..b]);
        out.extend_from_slice(&self.to_push[n - a..]);
        out.extend_from_slice(&self.to_push[b..n - a]);
        self.to_push = out;
    }

    fn add_branching(&mut self, from: [i32; 3]) -> bool {
        let bs = self.g.bs(from);
        for d in 0..6u8 {
            if axis(d) != axis(self.push) {
                let bp = rel(from, d);
                let s2 = self.g.bs(bp);
                if can_stick(s2, bs) && !self.add_line(bp, d) {
                    return false;
                }
            }
        }
        true
    }
}

fn moving_state(d: u8, sticky: bool) -> BlockState {
    with(with_facing(BlockState::by_name("moving_piston"), d), "type", if sticky { "sticky" } else { "normal" })
}

fn add_moving(g: &Game, p: [i32; 3], moved: BlockState, d: u8, extending: bool, source: bool) {
    let now = g.game_time();
    g.lv(|l| {
        l.moving.insert(p, Moving { moved, dir: d, extending, source, progress: 0.0, progress_o: 0.0, last_ticked: now });
    });
}

/// `PistonBaseBlock.moveBlocks`
fn move_blocks(g: &Game, p: [i32; 3], d: u8, extending: bool, sticky: bool) -> bool {
    let head = rel(p, d);
    if !extending && class(g.bs(head)) == Class::PistonHead {
        g.set_block(head, blocks::AIR, 276);
    }
    let Some(r) = Resolver::new(g, p, d, extending).resolve() else { return false };
    let to_push = r.to_push.clone();
    let to_destroy = r.to_destroy.clone();
    let states: Vec<BlockState> = to_push.iter().map(|q| g.bs(*q)).collect();
    // HashMap<BlockPos, BlockState> of vacated positions
    let mut map: Vec<([i32; 3], BlockState)> = to_push.iter().zip(&states).map(|(q, s)| (*q, *s)).collect();
    let mut arr: Vec<BlockState> = Vec::with_capacity(to_push.len() + to_destroy.len());
    let move_dir = if extending { d } else { opp(d) };
    for &bp in to_destroy.iter().rev() {
        let bs = g.bs(bp);
        g.drop_loot(bp, bs, None);
        g.block_entity_replaced(bp, blocks::AIR, true);
        g.set_block(bp, blocks::AIR, flags::CLIENTS | flags::KNOWN_SHAPE);
        arr.push(bs);
    }
    for k in (0..to_push.len()).rev() {
        let bp0 = to_push[k];
        let bs = g.bs(bp0);
        let bp = rel(bp0, move_dir);
        map.retain(|(q, _)| *q != bp);
        let mp = moving_state(d, false);
        g.set_block(bp, mp, flags::INVISIBLE | flags::MOVED);
        add_moving(g, bp, states[k], d, extending, false);
        arr.push(bs);
    }
    if extending {
        let head_state = with(with_facing(BlockState::by_name("piston_head"), d), "type", if sticky { "sticky" } else { "normal" });
        let mp = moving_state(d, sticky);
        map.retain(|(q, _)| *q != head);
        g.set_block(head, mp, flags::INVISIBLE | flags::MOVED);
        add_moving(g, head, head_state, d, true, true);
    }
    let keys: Vec<[i32; 3]> = map.iter().map(|(q, _)| *q).collect();
    let order = java_hashset_order(&keys);
    for q in &order {
        g.set_block(*q, blocks::AIR, flags::CLIENTS | flags::KNOWN_SHAPE | flags::MOVED);
    }
    for q in &order {
        let bs = map.iter().find(|(x, _)| x == q).map(|(_, s)| *s).unwrap_or(blocks::AIR);
        indirect_shapes(g, *q, bs, flags::CLIENTS, 511);
        for dd in SHAPE_ORDER {
            g.neighbour_shape_changed(opp(dd), rel(*q, dd), *q, blocks::AIR, flags::CLIENTS, 511);
        }
    }
    let mut j = 0;
    for &bp in to_destroy.iter().rev() {
        let bs = arr[j];
        j += 1;
        after_removal(g, bp, bs, false);
        indirect_shapes(g, bp, bs, flags::CLIENTS, 511);
        g.update_neighbors_at(bp, bs);
    }
    for k in (0..to_push.len()).rev() {
        g.update_neighbors_at(to_push[k], arr[j]);
        j += 1;
    }
    if extending {
        g.update_neighbors_at(head, BlockState::by_name("piston_head"));
    }
    true
}

/// `PistonBaseBlock.triggerEvent`
pub fn trigger_event(g: &Game, p: [i32; 3], s: BlockState, b0: i32, b1: i32) -> bool {
    let d = facing(s);
    let ext = with_bool(s, "extended", true);
    let b = neighbor_signal(g, p, d);
    if b && (b0 == 1 || b0 == 2) {
        g.set_block(p, ext, flags::CLIENTS);
        return false;
    }
    if !b && b0 == 0 {
        return false;
    }
    let sticky = sticky_piston(s);
    let pos = [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5];
    if b0 == 0 {
        if !move_blocks(g, p, d, true, sticky) {
            return false;
        }
        g.set_block(p, ext, flags::ALL | flags::MOVED);
        g.play_sound("minecraft:block.piston.extend", 1, pos, 0.5, rand::random::<f32>() * 0.25 + 0.6);
    } else if b0 == 1 || b0 == 2 {
        let headp = rel(p, d);
        if g.lv(|l| l.moving.contains_key(&headp)) {
            final_tick(g, headp);
        }
        let mp = moving_state(d, sticky);
        g.set_block(p, mp, 276);
        add_moving(g, p, with_facing(with_bool(s, "extended", false), (b1 & 7) as u8), d, false, true);
        g.update_neighbors_at(p, mp);
        for dd in SHAPE_ORDER {
            g.neighbour_shape_changed(opp(dd), rel(p, dd), p, mp, flags::CLIENTS, 511);
        }
        if sticky {
            let bp = rel_n(p, d, 2);
            let bs = g.bs(bp);
            let mut done = false;
            if class(bs) == Class::MovingPiston {
                if let Some(m) = g.lv(|l| l.moving.get(&bp).copied()) {
                    if m.dir == d && m.extending {
                        final_tick(g, bp);
                        done = true;
                    }
                }
            }
            if !done {
                if b0 != 1 || bs.is_air() || !is_pushable(g, bs, bp, opp(d), false, d) || rstate::push_reaction(bs) != 0 && !is_piston(bs) {
                    g.remove_block(headp, false);
                } else {
                    move_blocks(g, p, d, false, sticky);
                }
            }
        } else {
            g.remove_block(headp, false);
        }
        g.play_sound("minecraft:block.piston.contract", 1, pos, 0.5, rand::random::<f32>() * 0.15 + 0.6);
    }
    true
}

/// `PistonMovingBlockEntity.finalTick`
pub fn final_tick(g: &Game, p: [i32; 3]) {
    let Some(m) = g.lv(|l| l.moving.remove(&p)) else { return };
    if m.progress_o < 1.0 && class(g.bs(p)) == Class::MovingPiston {
        let bs = if m.source { blocks::AIR } else { crate::worldgen::feature::shape_update(g, m.moved, p) };
        g.set_block(p, bs, flags::ALL);
        g.neighbor_changed(p, bs);
    }
}

/// scheduled tick on moving blocks (unused: they tick through `tick_moving`)
pub fn finish_moving(_g: &Game, _p: [i32; 3], _s: BlockState) {}

/// `PistonMovingBlockEntity.tick` for every block in motion (block entity phase)
pub fn tick_moving(g: &Game) {
    let list: Vec<[i32; 3]> = g.lv(|l| l.moving.keys().copied().collect());
    let now = g.game_time();
    for p in list {
        let Some(mut m) = g.lv(|l| l.moving.get(&p).copied()) else { continue };
        m.last_ticked = now;
        m.progress_o = m.progress;
        if m.progress_o >= 1.0 {
            g.lv(|l| l.moving.remove(&p));
            if class(g.bs(p)) == Class::MovingPiston {
                let bs = crate::worldgen::feature::shape_update(g, m.moved, p);
                if bs.is_air() {
                    g.set_block(p, m.moved, flags::INVISIBLE | flags::KNOWN_SHAPE | flags::MOVED);
                    g.update_or_destroy(m.moved, bs, p, flags::ALL, 512);
                } else {
                    let bs = if flag(bs, "waterlogged") { with_bool(bs, "waterlogged", false) } else { bs };
                    g.set_block(p, bs, flags::ALL | flags::MOVED);
                    g.neighbor_changed(p, bs);
                }
            }
            continue;
        }
        let f = m.progress + 0.5;
        g.push_entities_by_piston(p, m.moved, m.dir, m.extending, f - m.progress);
        m.progress = f.min(1.0);
        g.lv(|l| {
            if let Some(x) = l.moving.get_mut(&p) {
                *x = m;
            }
        });
    }
}

pub fn head_survives(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    let bs = g.bs(rel(p, opp(facing(s))));
    fitting_base(s, bs) || class(bs) == Class::MovingPiston && facing(bs) == facing(s)
}

fn fitting_base(head: BlockState, base: BlockState) -> bool {
    let want = if prop(head, "type") == "sticky" { "sticky_piston" } else { "piston" };
    base.name() == want && flag(base, "extended") && facing(base) == facing(head)
}

pub fn head_update_shape(g: &Game, p: [i32; 3], s: BlockState, dir: u8, _ns: BlockState) -> BlockState {
    if opp(dir) == facing(s) && !head_survives(g, p, s) {
        return blocks::AIR;
    }
    s
}

/// `PistonHeadBlock.affectNeighborsAfterRemoval`: the base goes too
pub fn head_removed(g: &Game, p: [i32; 3], old: BlockState) {
    let bp = rel(p, opp(facing(old)));
    if fitting_base(old, g.bs(bp)) {
        g.destroy_block(bp, true, 512);
    }
}

pub type MovingMap = HashMap<[i32; 3], Moving>;
