//! `BaseRailBlock`, `RailState`, `RailBlock`, `PoweredRailBlock`,
//! `DetectorRailBlock`.

use super::*;
use crate::anticheat::combat::Aabb;

fn is_rail(s: BlockState) -> bool {
    matches!(class(s), Class::Rail | Class::PoweredRail | Class::DetectorRail)
}

fn rail_at(g: &Game, p: [i32; 3]) -> bool {
    is_rail(g.bs(p))
}

fn straight(s: BlockState) -> bool {
    matches!(class(s), Class::PoweredRail | Class::DetectorRail)
}

pub fn ascending(shape: &str) -> bool {
    shape.starts_with("ascending")
}

fn connections_of(p: [i32; 3], shape: &str) -> Vec<[i32; 3]> {
    let up = |q: [i32; 3]| rel(q, UP);
    let (n, s, w, e) = (rel(p, NORTH), rel(p, SOUTH), rel(p, WEST), rel(p, EAST));
    match shape {
        "north_south" => vec![n, s],
        "east_west" => vec![w, e],
        "ascending_east" => vec![w, up(e)],
        "ascending_west" => vec![up(w), e],
        "ascending_north" => vec![up(n), s],
        "ascending_south" => vec![n, up(s)],
        "south_east" => vec![e, s],
        "south_west" => vec![w, s],
        "north_west" => vec![w, n],
        "north_east" => vec![e, n],
        _ => vec![],
    }
}

/// `RailState`
struct RailState {
    pos: [i32; 3],
    state: BlockState,
    straight: bool,
    conns: Vec<[i32; 3]>,
}

impl RailState {
    fn new(p: [i32; 3], s: BlockState) -> Self {
        RailState { pos: p, state: s, straight: straight(s), conns: connections_of(p, prop(s, "shape")) }
    }

    fn get_rail(g: &Game, p: [i32; 3]) -> Option<RailState> {
        for q in [p, rel(p, UP), rel(p, DOWN)] {
            let s = g.bs(q);
            if is_rail(s) {
                return Some(RailState::new(q, s));
            }
        }
        None
    }

    fn has_rail(g: &Game, p: [i32; 3]) -> bool {
        rail_at(g, p) || rail_at(g, rel(p, UP)) || rail_at(g, rel(p, DOWN))
    }

    fn remove_soft_connections(&mut self, g: &Game) {
        let mut i = 0;
        while i < self.conns.len() {
            match RailState::get_rail(g, self.conns[i]) {
                Some(r) if r.connects_to(self) => {
                    self.conns[i] = r.pos;
                    i += 1;
                }
                _ => {
                    self.conns.remove(i);
                }
            }
        }
    }

    fn has_connection(&self, p: [i32; 3]) -> bool {
        self.conns.iter().any(|c| c[0] == p[0] && c[2] == p[2])
    }

    fn connects_to(&self, other: &RailState) -> bool {
        self.has_connection(other.pos)
    }

    fn count_potential_connections(&self, g: &Game) -> usize {
        HORIZONTAL_ORDER.iter().filter(|&&d| RailState::has_rail(g, rel(self.pos, d))).count()
    }

    fn can_connect_to(&self, other: &RailState) -> bool {
        self.connects_to(other) || self.conns.len() != 2
    }

    fn slope(&self, g: &Game, shape: &'static str) -> &'static str {
        let p = self.pos;
        let mut shape = shape;
        if shape == "north_south" {
            if rail_at(g, rel(rel(p, NORTH), UP)) {
                shape = "ascending_north";
            }
            if rail_at(g, rel(rel(p, SOUTH), UP)) {
                shape = "ascending_south";
            }
        }
        if shape == "east_west" {
            if rail_at(g, rel(rel(p, EAST), UP)) {
                shape = "ascending_east";
            }
            if rail_at(g, rel(rel(p, WEST), UP)) {
                shape = "ascending_west";
            }
        }
        shape
    }

    fn connect_to(&mut self, g: &Game, other: &RailState) {
        self.conns.push(other.pos);
        let p = self.pos;
        let bn = self.has_connection(rel(p, NORTH));
        let bs = self.has_connection(rel(p, SOUTH));
        let bw = self.has_connection(rel(p, WEST));
        let be = self.has_connection(rel(p, EAST));
        let mut shape: Option<&'static str> = None;
        if bn || bs {
            shape = Some("north_south");
        }
        if bw || be {
            shape = Some("east_west");
        }
        if !self.straight {
            if bs && be && !bn && !bw {
                shape = Some("south_east");
            }
            if bs && bw && !bn && !be {
                shape = Some("south_west");
            }
            if bn && bw && !bs && !be {
                shape = Some("north_west");
            }
            if bn && be && !bs && !bw {
                shape = Some("north_east");
            }
        }
        let shape = self.slope(g, shape.unwrap_or("north_south"));
        self.state = with(self.state, "shape", shape);
        g.set_block(p, self.state, flags::ALL);
    }

    fn has_neighbor_rail(&self, g: &Game, p: [i32; 3]) -> bool {
        match RailState::get_rail(g, p) {
            Some(mut r) => {
                r.remove_soft_connections(g);
                r.can_connect_to(self)
            }
            None => false,
        }
    }

    fn place(mut self, g: &Game, powered: bool, always: bool, old: &'static str) -> RailState {
        let p = self.pos;
        let bn = self.has_neighbor_rail(g, rel(p, NORTH));
        let bs = self.has_neighbor_rail(g, rel(p, SOUTH));
        let bw = self.has_neighbor_rail(g, rel(p, WEST));
        let be = self.has_neighbor_rail(g, rel(p, EAST));
        let mut shape: Option<&'static str> = None;
        let ns = bn || bs;
        let ew = bw || be;
        if ns && !ew {
            shape = Some("north_south");
        }
        if ew && !ns {
            shape = Some("east_west");
        }
        let (se, sw, ne, nw) = (bs && be, bs && bw, bn && be, bn && bw);
        if !self.straight {
            if se && !bn && !bw {
                shape = Some("south_east");
            }
            if sw && !bn && !be {
                shape = Some("south_west");
            }
            if nw && !bs && !be {
                shape = Some("north_west");
            }
            if ne && !bs && !bw {
                shape = Some("north_east");
            }
        }
        if shape.is_none() {
            if ns && ew {
                shape = Some(old);
            } else if ns {
                shape = Some("north_south");
            } else if ew {
                shape = Some("east_west");
            }
            if !self.straight {
                let order: [(bool, &str); 4] = if powered {
                    [(se, "south_east"), (sw, "south_west"), (ne, "north_east"), (nw, "north_west")]
                } else {
                    [(nw, "north_west"), (ne, "north_east"), (sw, "south_west"), (se, "south_east")]
                };
                for (b, n) in order {
                    if b {
                        shape = Some(n);
                    }
                }
            }
        }
        let mut shape = shape.unwrap_or(old);
        if shape == "north_south" || shape == "east_west" {
            shape = self.slope(g, shape);
        }
        self.conns = connections_of(p, shape);
        self.state = with(self.state, "shape", shape);
        if always || g.bs(p) != self.state {
            g.set_block(p, self.state, flags::ALL);
            for c in self.conns.clone() {
                if let Some(mut r) = RailState::get_rail(g, c) {
                    r.remove_soft_connections(g);
                    if r.can_connect_to(&self) {
                        r.connect_to(g, &self);
                    }
                }
            }
        }
        self
    }
}

/// `BaseRailBlock.getStateForPlacement`
pub fn placement(s: BlockState, horiz: u8) -> BlockState {
    with(s, "shape", if axis(horiz) == 0 { "east_west" } else { "north_south" })
}

fn update_dir(g: &Game, p: [i32; 3], s: BlockState, always: bool) -> BlockState {
    let old = prop(s, "shape");
    RailState::new(p, s).place(g, g.has_neighbor_signal(p), always, old).state
}

/// `BaseRailBlock.updateState(state, level, pos, movedByPiston)`
fn update_state_placed(g: &Game, p: [i32; 3], s: BlockState) -> BlockState {
    let s = update_dir(g, p, s, true);
    if straight(s) {
        g.neighbor_changed_full(s, p, s, false);
    }
    s
}

pub fn on_place(g: &Game, p: [i32; 3], s: BlockState) {
    let s = update_state_placed(g, p, s);
    if class(s) == Class::DetectorRail {
        detector_check(g, p, g.bs(p));
    }
}

fn should_be_removed(g: &Game, p: [i32; 3], shape: &str) -> bool {
    if !can_support_rigid(g, rel(p, DOWN)) {
        return true;
    }
    let d = match shape {
        "ascending_east" => EAST,
        "ascending_west" => WEST,
        "ascending_north" => NORTH,
        "ascending_south" => SOUTH,
        _ => return false,
    };
    !can_support_rigid(g, rel(p, d))
}

/// `BaseRailBlock.neighborChanged`
pub fn neighbor_changed(g: &Game, p: [i32; 3], s: BlockState, src: BlockState) {
    if !is_rail(g.bs(p)) {
        return;
    }
    if should_be_removed(g, p, prop(s, "shape")) {
        g.drop_loot(p, s, None);
        g.remove_block(p, false);
        return;
    }
    match class(s) {
        Class::Rail => {
            if rstate::signal_source(BlockState(BlockRegistry::get().block(src).default)) && RailState::new(p, s).count_potential_connections(g) == 3 {
                update_dir(g, p, s, false);
            }
        }
        Class::PoweredRail => powered_update(g, p, s),
        _ => {}
    }
}

pub fn after_removal(g: &Game, p: [i32; 3], old: BlockState) {
    if ascending(prop(old, "shape")) {
        g.update_neighbors_at(rel(p, UP), old);
    }
    if straight(old) {
        g.update_neighbors_at(p, old);
        g.update_neighbors_at(rel(p, DOWN), old);
    }
}

// ------------------------------------------------------------ powered rails

fn powered_update(g: &Game, p: [i32; 3], s: BlockState) {
    let b = flag(s, "powered");
    let b1 = g.has_neighbor_signal(p) || find_signal(g, p, s, true, 0) || find_signal(g, p, s, false, 0);
    if b1 != b {
        g.set_block(p, with_bool(s, "powered", b1), flags::ALL);
        g.update_neighbors_at(rel(p, DOWN), s);
        if ascending(prop(s, "shape")) {
            g.update_neighbors_at(rel(p, UP), s);
        }
    }
}

/// `PoweredRailBlock.findPoweredRailSignal`
fn find_signal(g: &Game, p: [i32; 3], s: BlockState, fwd: bool, depth: i32) -> bool {
    if depth >= 8 {
        return false;
    }
    let [mut x, mut y, mut z] = p;
    let mut flag = true;
    let mut shape = prop(s, "shape");
    match shape {
        "north_south" => z += if fwd { 1 } else { -1 },
        "east_west" => x += if fwd { -1 } else { 1 },
        "ascending_east" => {
            if fwd {
                x -= 1;
            } else {
                x += 1;
                y += 1;
                flag = false;
            }
            shape = "east_west";
        }
        "ascending_west" => {
            if fwd {
                x -= 1;
                y += 1;
                flag = false;
            } else {
                x += 1;
            }
            shape = "east_west";
        }
        "ascending_north" => {
            if fwd {
                z += 1;
            } else {
                z -= 1;
                y += 1;
                flag = false;
            }
            shape = "north_south";
        }
        "ascending_south" => {
            if fwd {
                z += 1;
                y += 1;
                flag = false;
            } else {
                z -= 1;
            }
            shape = "north_south";
        }
        _ => {}
    }
    same_rail_with_power(g, [x, y, z], s, fwd, depth, shape) || flag && same_rail_with_power(g, [x, y - 1, z], s, fwd, depth, shape)
}

fn same_rail_with_power(g: &Game, p: [i32; 3], me: BlockState, fwd: bool, depth: i32, shape: &str) -> bool {
    let bs = g.bs(p);
    if !same_block(bs, me) {
        return false;
    }
    let s2 = prop(bs, "shape");
    if shape == "east_west" && matches!(s2, "north_south" | "ascending_north" | "ascending_south") {
        return false;
    }
    if shape == "north_south" && matches!(s2, "east_west" | "ascending_east" | "ascending_west") {
        return false;
    }
    if !flag(bs, "powered") {
        return false;
    }
    g.has_neighbor_signal(p) || find_signal(g, p, bs, fwd, depth + 1)
}

// ------------------------------------------------------------ detector rails

fn detector_box(p: [i32; 3]) -> Aabb {
    Aabb { min: [p[0] as f64 + 0.2, p[1] as f64, p[2] as f64 + 0.2], max: [p[0] as f64 + 0.8, p[1] as f64 + 0.8, p[2] as f64 + 0.8] }
}

/// `DetectorRailBlock.checkPressed`
pub fn detector_check(g: &Game, p: [i32; 3], s: BlockState) {
    if class(s) != Class::DetectorRail || should_be_removed(g, p, prop(s, "shape")) {
        return;
    }
    let was = flag(s, "powered");
    let now = g.minecarts_in(&detector_box(p)) > 0;
    if now != was {
        let ns = with_bool(s, "powered", now);
        g.set_block(p, ns, flags::ALL);
        g.update_neighbors_at(p, ns);
        g.update_neighbors_at(rel(p, DOWN), ns);
    }
    if now {
        g.schedule_tick(p, g.bs(p), 20, prio::NORMAL);
    }
    g.update_neighbour_for_output_signal(p, g.bs(p));
}

pub fn detector_entity_inside(g: &Game, p: [i32; 3], s: BlockState) {
    if !flag(s, "powered") {
        detector_check(g, p, s);
    }
}
