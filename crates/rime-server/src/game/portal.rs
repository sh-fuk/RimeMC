//! Nether and end portals: `PortalShape`, `BaseFireBlock` portal lighting,
//! `NetherPortalBlock` (shape updates, destinations), `PortalForcer`
//! (finding and creating exit portals), `EndPortalFrameBlock` and the end
//! spawn platform.

use super::level::flags;
use super::{Game, Player};
use crate::registry::{blocks, BlockRegistry, BlockState};
use crate::world::storage::Dim;

const MAX_SIZE: i32 = 21;

fn is_frame(s: BlockState) -> bool {
    s.name() == "obsidian"
}

/// `PortalShape.isEmpty`
fn is_empty(s: BlockState) -> bool {
    s.is_air() || matches!(s.name(), "fire" | "soul_fire" | "nether_portal")
}

/// Horizontal axis: 0 = x, 2 = z.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    X,
    Z,
}

impl Axis {
    fn name(self) -> &'static str {
        match self {
            Axis::X => "x",
            Axis::Z => "z",
        }
    }
    fn other(self) -> Axis {
        match self {
            Axis::X => Axis::Z,
            Axis::Z => Axis::X,
        }
    }
    pub fn of(s: BlockState) -> Option<Axis> {
        match s.prop("axis") {
            Some("x") => Some(Axis::X),
            Some("z") => Some(Axis::Z),
            _ => None,
        }
    }
    /// `rightDir` of a portal on this axis (west for x, south for z).
    fn right(self) -> [i32; 3] {
        match self {
            Axis::X => [-1, 0, 0],
            Axis::Z => [0, 0, 1],
        }
    }
}

fn off(p: [i32; 3], d: [i32; 3], n: i32) -> [i32; 3] {
    [p[0] + d[0] * n, p[1] + d[1] * n, p[2] + d[2] * n]
}

fn neg(d: [i32; 3]) -> [i32; 3] {
    [-d[0], -d[1], -d[2]]
}

#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub axis: Axis,
    pub portal_blocks: i32,
    right: [i32; 3],
    pub bottom_left: [i32; 3],
    pub width: i32,
    pub height: i32,
}

impl Shape {
    pub fn is_valid(&self) -> bool {
        (2..=MAX_SIZE).contains(&self.width) && (3..=MAX_SIZE).contains(&self.height)
    }
    pub fn is_complete(&self) -> bool {
        self.is_valid() && self.portal_blocks == self.width * self.height
    }

    /// `PortalShape.createPortalBlocks`
    pub fn create_portal_blocks(&self, g: &Game) {
        let Some(st) = BlockRegistry::get().state("nether_portal", &[("axis", self.axis.name())]) else { return };
        for h in 0..self.height {
            for w in 0..self.width {
                let p = off(off(self.bottom_left, [0, 1, 0], h), self.right, w);
                g.set_block(p, st, flags::CLIENTS | flags::KNOWN_SHAPE);
            }
        }
    }
}

/// `PortalShape.findAnyShape`
pub fn find_any_shape(g: &Game, pos: [i32; 3], axis: Axis) -> Shape {
    let right = axis.right();
    let empty = Shape { axis, portal_blocks: 0, right, bottom_left: pos, width: 0, height: 0 };
    let Some(bl) = bottom_left(g, right, pos) else { return empty };
    let w = distance_until_edge_above_frame(g, bl, right);
    let width = if (2..=MAX_SIZE).contains(&w) { w } else { 0 };
    if width == 0 {
        return Shape { bottom_left: bl, ..empty };
    }
    let mut count = 0;
    let h = distance_until_top(g, bl, right, width, &mut count);
    let height = if (3..=MAX_SIZE).contains(&h) && has_top_frame(g, bl, right, width, h) { h } else { 0 };
    Shape { axis, portal_blocks: count, right, bottom_left: bl, width, height }
}

fn bottom_left(g: &Game, right: [i32; 3], pos: [i32; 3]) -> Option<[i32; 3]> {
    let min_y = (crate::world::chunk::MIN_Y).max(pos[1] - MAX_SIZE);
    let mut p = pos;
    while p[1] > min_y && is_empty(g.bs([p[0], p[1] - 1, p[2]])) {
        p[1] -= 1;
    }
    let left = neg(right);
    let d = distance_until_edge_above_frame(g, p, left) - 1;
    (d >= 0).then(|| off(p, left, d))
}

fn distance_until_edge_above_frame(g: &Game, pos: [i32; 3], dir: [i32; 3]) -> i32 {
    for i in 0..=MAX_SIZE {
        let p = off(pos, dir, i);
        let s = g.bs(p);
        if !is_empty(s) {
            if is_frame(s) {
                return i;
            }
            break;
        }
        if !is_frame(g.bs([p[0], p[1] - 1, p[2]])) {
            break;
        }
    }
    0
}

fn has_top_frame(g: &Game, bl: [i32; 3], right: [i32; 3], width: i32, height: i32) -> bool {
    (0..width).all(|i| is_frame(g.bs(off(off(bl, [0, 1, 0], height), right, i))))
}

fn distance_until_top(g: &Game, bl: [i32; 3], right: [i32; 3], width: i32, count: &mut i32) -> i32 {
    for h in 0..MAX_SIZE {
        let row = off(bl, [0, 1, 0], h);
        if !is_frame(g.bs(off(row, right, -1))) || !is_frame(g.bs(off(row, right, width))) {
            return h;
        }
        for i in 0..width {
            let s = g.bs(off(row, right, i));
            if !is_empty(s) {
                return h;
            }
            if s.name() == "nether_portal" {
                *count += 1;
            }
        }
    }
    MAX_SIZE
}

/// `PortalShape.findEmptyPortalShape`
pub fn find_empty_shape(g: &Game, pos: [i32; 3], axis: Axis) -> Option<Shape> {
    let ok = |s: &Shape| s.is_valid() && s.portal_blocks == 0;
    let s = find_any_shape(g, pos, axis);
    if ok(&s) {
        return Some(s);
    }
    let s = find_any_shape(g, pos, axis.other());
    ok(&s).then_some(s)
}

/// `BaseFireBlock.inPortalDimension`
pub fn in_portal_dimension(g: &Game) -> bool {
    matches!(g.dim, Dim::Overworld | Dim::Nether)
}

/// `BaseFireBlock.onPlace`: a fire inside an empty obsidian frame lights it.
/// True when a portal was created (the fire is gone).
pub fn fire_placed(g: &Game, p: [i32; 3]) -> bool {
    if !in_portal_dimension(g) {
        return false;
    }
    match find_empty_shape(g, p, Axis::X) {
        Some(s) => {
            s.create_portal_blocks(g);
            true
        }
        None => false,
    }
}

/// `BaseFireBlock.getState`: soul fire on soul blocks, else fire with the
/// sides it clings to.
pub fn fire_state(g: &Game, p: [i32; 3]) -> BlockState {
    let reg = BlockRegistry::get();
    let below = g.bs([p[0], p[1] - 1, p[2]]);
    if matches!(below.name(), "soul_sand" | "soul_soil") {
        return reg.state("soul_fire", &[]).unwrap_or(blocks::AIR);
    }
    let mut s = reg.state("fire", &[]).unwrap_or(blocks::AIR);
    let burns = |q: [i32; 3]| super::randomtick::flammability(g.bs(q).name()).0 > 0;
    if !crate::registry::vstate::sturdy(below, 1) && !burns([p[0], p[1] - 1, p[2]]) {
        for (name, d) in [("north", [0, 0, -1]), ("east", [1, 0, 0]), ("south", [0, 0, 1]), ("west", [-1, 0, 0]), ("up", [0, 1, 0])] {
            if burns(off(p, d, 1)) {
                s = reg.with(s, name, "true").unwrap_or(s);
            }
        }
    }
    s
}

/// `BaseFireBlock.canBePlacedAt`; `horiz` = player's horizontal facing
/// (direction ordinal).
pub fn fire_can_be_placed_at(g: &Game, p: [i32; 3], horiz: u8) -> bool {
    if !g.bs(p).is_air() {
        return false;
    }
    let st = fire_state(g, p);
    if g.can_survive(p, st) {
        return true;
    }
    // BaseFireBlock.isPortal
    if !in_portal_dimension(g) {
        return false;
    }
    if !super::level::OFFSETS.iter().any(|d| is_frame(g.bs(off(p, *d, 1)))) {
        return false;
    }
    // counter-clockwise of the facing: north/south → x? (getCounterClockWise of
    // north is west, axis x)
    let axis = match horiz {
        2 | 3 => Axis::X,
        _ => Axis::Z,
    };
    find_empty_shape(g, p, axis).is_some()
}

/// `FlintAndSteelItem.useOn` / `FireChargeItem.useOn` fire placement next to
/// the clicked face. True when fire was placed.
pub fn light_at(g: &Game, p: [i32; 3], horiz: u8, charge: bool) -> bool {
    if !fire_can_be_placed_at(g, p, horiz) {
        return false;
    }
    let st = fire_state(g, p);
    let pos = [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5];
    let pitch = rand::random::<f32>() * 0.4 + 0.8;
    if charge {
        g.play_sound("minecraft:item.firecharge.use", 4, pos, 1.0, (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0);
    } else {
        g.play_sound("minecraft:item.flintandsteel.use", 4, pos, 1.0, pitch);
    }
    g.set_block(p, st, flags::ALL | 8);
    true
}

/// `NetherPortalBlock.updateShape`
pub fn portal_update_shape(g: &Game, p: [i32; 3], s: BlockState, dir: u8, ns: BlockState) -> BlockState {
    let Some(axis) = Axis::of(s) else { return s };
    let dir_axis = match dir {
        0 | 1 => None,
        2 | 3 => Some(Axis::Z),
        _ => Some(Axis::X),
    };
    let crosswise = dir_axis.is_some_and(|a| a != axis);
    if !crosswise && ns.name() != "nether_portal" && !find_any_shape(g, p, axis).is_complete() {
        return blocks::AIR;
    }
    s
}

// ------------------------------------------------------------ portal search

/// `PortalForcer.findClosestPortalPosition`: the nearest portal block within
/// 16 (to the nether) or 128 blocks.
pub fn find_closest(g: &Game, exit: [i32; 3], to_nether: bool) -> Option<[i32; 3]> {
    let r = if to_nether { 16 } else { 128 };
    let max_y = g.dim_logical_top();
    let portals = g.portals.lock();
    portals
        .iter()
        .copied()
        .filter(|p| (p[0] - exit[0]).abs() <= r && (p[2] - exit[2]).abs() <= r && p[1] <= max_y)
        .filter(|p| g.bs(*p).name() == "nether_portal")
        .min_by(|a, b| {
            let d = |p: &[i32; 3]| {
                let (x, y, z) = ((p[0] - exit[0]) as i64, (p[1] - exit[1]) as i64, (p[2] - exit[2]) as i64);
                x * x + y * y + z * z
            };
            d(a).cmp(&d(b)).then(a[1].cmp(&b[1]))
        })
}

/// `BlockUtil.getLargestRectangleAround` for portal blocks (min corner,
/// width along the axis, height).
pub fn portal_rect(g: &Game, p: [i32; 3], axis: Axis) -> ([i32; 3], i32, i32) {
    let s = g.bs(p);
    let along = match axis {
        Axis::X => [1, 0, 0],
        Axis::Z => [0, 0, 1],
    };
    let same = |q: [i32; 3]| g.bs(q) == s;
    let mut lo = p;
    let mut n = 0;
    while n < MAX_SIZE && same(off(lo, along, -1)) {
        lo = off(lo, along, -1);
        n += 1;
    }
    // find the full rectangle: lowest y of this column, then extents
    let mut bottom = lo;
    let mut k = 0;
    while k < MAX_SIZE && same([bottom[0], bottom[1] - 1, bottom[2]]) {
        bottom[1] -= 1;
        k += 1;
    }
    let mut w = 1;
    while w < MAX_SIZE && same(off(bottom, along, w)) {
        w += 1;
    }
    let mut h = 1;
    while h < MAX_SIZE && (0..w).all(|i| same([bottom[0] + along[0] * i, bottom[1] + h, bottom[2] + along[2] * i])) {
        h += 1;
    }
    (bottom, w, h)
}

fn can_portal_replace(g: &Game, p: [i32; 3]) -> bool {
    let s = g.bs(p);
    g.is_replaceable(s) && super::fluid::fluid_state(s).fluid().is_none()
}

/// `PortalForcer.createPortal`: returns the new portal's min corner.
pub fn create_portal(g: &Game, pos: [i32; 3], axis: Axis) -> Option<[i32; 3]> {
    let dir = match axis {
        Axis::X => [1, 0, 0],
        Axis::Z => [0, 0, 1],
    };
    // Direction.getClockWise of east is south, of south is west
    let cw = match axis {
        Axis::X => [0, 0, 1],
        Axis::Z => [-1, 0, 0],
    };
    let min_y = crate::world::chunk::MIN_Y;
    let max_y = g.dim_logical_top();
    let host = |o: [i32; 3], offset: i32| -> bool {
        for i in -1..3 {
            for j in -1..4 {
                let m = [o[0] + dir[0] * i + cw[0] * offset, o[1] + j, o[2] + dir[2] * i + cw[2] * offset];
                if j < 0 && !g.bs(m).is_solid() {
                    return false;
                }
                if j >= 0 && !can_portal_replace(g, m) {
                    return false;
                }
            }
        }
        true
    };
    let mut best: Option<(i64, [i32; 3])> = None;
    let mut fallback: Option<(i64, [i32; 3])> = None;
    for dx in -16..=16 {
        for dz in -16..=16 {
            let (x, z) = (pos[0] + dx, pos[2] + dz);
            let top = max_y.min(g.motion_blocking_height(x, z));
            let mut y = top;
            while y >= min_y {
                let c = [x, y, z];
                if can_portal_replace(g, c) {
                    let y0 = y;
                    while y > min_y && can_portal_replace(g, [x, y - 1, z]) {
                        y -= 1;
                    }
                    if y + 4 <= max_y {
                        let gap = y0 - y;
                        if gap <= 0 || gap >= 3 {
                            let c = [x, y, z];
                            if host(c, 0) {
                                let d = {
                                    let (a, b, e) = ((x - pos[0]) as i64, (y - pos[1]) as i64, (z - pos[2]) as i64);
                                    a * a + b * b + e * e
                                };
                                if host(c, -1) && host(c, 1) && best.is_none_or(|(bd, _)| bd > d) {
                                    best = Some((d, c));
                                }
                                if best.is_none() && fallback.is_none_or(|(fd, _)| fd > d) {
                                    fallback = Some((d, c));
                                }
                            }
                        }
                    }
                }
                y -= 1;
            }
        }
    }
    let obsidian = BlockRegistry::get().state("obsidian", &[]).unwrap_or(blocks::AIR);
    let origin = match best.or(fallback) {
        Some((_, c)) => c,
        None => {
            let lo = (min_y + 1).max(70);
            let hi = max_y - 9;
            if hi < lo {
                return None;
            }
            let o = [pos[0] - dir[0], pos[1].clamp(lo, hi), pos[2] - dir[2]];
            for i in -1..2 {
                for j in 0..2 {
                    for k in -1..3 {
                        let s = if k < 0 { obsidian } else { blocks::AIR };
                        let m = [o[0] + j * dir[0] + i * cw[0], o[1] + k, o[2] + j * dir[2] + i * cw[2]];
                        g.set_block(m, s, flags::ALL);
                    }
                }
            }
            o
        }
    };
    for l in -1..3 {
        for j in -1..4 {
            if l == -1 || l == 2 || j == -1 || j == 3 {
                g.set_block([origin[0] + l * dir[0], origin[1] + j, origin[2] + l * dir[2]], obsidian, flags::ALL);
            }
        }
    }
    let portal = BlockRegistry::get().state("nether_portal", &[("axis", axis.name())]).unwrap_or(blocks::AIR);
    for k in 0..2 {
        for l in 0..3 {
            g.set_block([origin[0] + k * dir[0], origin[1] + l, origin[2] + k * dir[2]], portal, flags::CLIENTS | flags::KNOWN_SHAPE);
        }
    }
    Some(origin)
}

/// Where an entity standing in the portal at `from` (in `g`) comes out in
/// `dest`: `NetherPortalBlock.getPortalDestination`. The destination area
/// must be loaded. Returns (position, yaw change).
pub fn nether_destination(g: &Game, dest: &Game, from: [i32; 3], pos: [f64; 3]) -> Option<([f64; 3], f32)> {
    let to_nether = dest.dim == Dim::Nether;
    let scale = if to_nether { 1.0 / 8.0 } else { 8.0 };
    let clamp = |v: f64| v.clamp(-29_999_872.0, 29_999_872.0);
    let exit = [clamp(pos[0] * scale).floor() as i32, pos[1].floor() as i32, clamp(pos[2] * scale).floor() as i32];
    let src_state = g.bs(from);
    let src_axis = Axis::of(src_state).unwrap_or(Axis::X);
    let (corner, axis2, w, h) = match find_closest(dest, exit, to_nether) {
        Some(p) => {
            let a = Axis::of(dest.bs(p)).unwrap_or(Axis::X);
            let (c, w, h) = portal_rect(dest, p, a);
            (c, a, w, h)
        }
        None => {
            let c = create_portal(dest, exit, src_axis)?;
            (c, src_axis, 2, 3)
        }
    };
    // Entity.getRelativePortalPosition: keep the relative place in the frame
    let (src_corner, sw, sh) = portal_rect(g, from, src_axis);
    let along = |p: [f64; 3], a: Axis| if a == Axis::X { p[0] } else { p[2] };
    let rel_w = ((along(pos, src_axis) - along([src_corner[0] as f64, 0.0, src_corner[2] as f64], src_axis) - 0.3) / (sw as f64 - 0.6).max(1e-6)).clamp(0.0, 1.0);
    let rel_h = ((pos[1] - src_corner[1] as f64) / (sh as f64 - 1.8).max(1e-6)).clamp(0.0, 1.0);
    let wpos = 0.3 + rel_w * (w as f64 - 0.6);
    let hpos = rel_h * (h as f64 - 1.8).max(0.0);
    let out = match axis2 {
        Axis::X => [corner[0] as f64 + wpos, corner[1] as f64 + hpos, corner[2] as f64 + 0.5],
        Axis::Z => [corner[0] as f64 + 0.5, corner[1] as f64 + hpos, corner[2] as f64 + wpos],
    };
    let yaw = if axis2 == src_axis { 0.0 } else { 90.0 };
    Some((out, yaw))
}

// ------------------------------------------------------------ end portals

/// `EndPortalFrameBlock.getOrCreatePortalShape` match after an eye was put
/// into the frame at `p`: fills the 3x3 portal. True when it was created.
pub fn try_end_portal(g: &Game, p: [i32; 3]) -> bool {
    // the frame ring is a 5x5 square with the corners missing; `p` may be
    // any of its 12 frames
    let frame_ok = |q: [i32; 3], facing: &str| {
        let s = g.bs(q);
        s.name() == "end_portal_frame" && s.prop("eye") == Some("true") && s.prop("facing") == Some(facing)
    };
    for cx in p[0] - 4..=p[0] + 4 {
        for cz in p[2] - 4..=p[2] + 4 {
            // center of the 3x3 inside
            let y = p[1];
            let ok = (-1..=1).all(|i| {
                frame_ok([cx + i, y, cz - 2], "south") && frame_ok([cx + i, y, cz + 2], "north") && frame_ok([cx - 2, y, cz + i], "east") && frame_ok([cx + 2, y, cz + i], "west")
            });
            if !ok {
                continue;
            }
            let portal = BlockRegistry::get().state("end_portal", &[]).unwrap_or(blocks::AIR);
            for i in -1..=1 {
                for j in -1..=1 {
                    g.set_block([cx + i, y, cz + j], portal, flags::CLIENTS);
                }
            }
            // LevelEvent.SOUND_END_PORTAL_SPAWN (global)
            g.level_event_global(1038, [cx, y, cz]);
            return true;
        }
    }
    false
}

/// `ServerLevel.END_SPAWN_POINT` platform (`EndPlatformFeature`): a 5x5
/// obsidian floor with three layers of air above it.
pub fn end_platform(g: &Game, at: [i32; 3]) {
    let obsidian = BlockRegistry::get().state("obsidian", &[]).unwrap_or(blocks::AIR);
    for x in -2..=2 {
        for z in -2..=2 {
            for y in -1..3 {
                let p = [at[0] + x, at[1] + y, at[2] + z];
                let s = if y == -1 { obsidian } else { blocks::AIR };
                if g.bs(p) != s {
                    g.destroy_block(p, true, 512);
                    g.set_block(p, s, flags::ALL);
                }
            }
        }
    }
}

pub const END_SPAWN: [i32; 3] = [100, 50, 0];

// ------------------------------------------------------------ portal registry

impl Game {
    /// Top of the dimension's logical height (portals are never placed above).
    pub fn dim_logical_top(&self) -> i32 {
        match self.dim {
            Dim::Nether => 127,
            Dim::End => 255,
            Dim::Overworld => 319,
        }
    }

    /// `MOTION_BLOCKING` height (first y above the top motion-blocking block).
    pub fn motion_blocking_height(&self, x: i32, z: i32) -> i32 {
        let min = crate::world::chunk::MIN_Y;
        let mut y = min + crate::world::chunk::HEIGHT - 1;
        while y >= min {
            let s = self.bs([x, y, z]);
            if s.is_solid() || super::fluid::fluid_state(s).fluid().is_some() {
                return y + 1;
            }
            y -= 1;
        }
        min
    }

    /// Track nether portal blocks (the vanilla portal POIs).
    pub(crate) fn portal_changed(&self, p: [i32; 3], old: BlockState, new: BlockState) {
        if new.name() == "nether_portal" {
            self.portals.lock().insert(p);
        } else if old.name() == "nether_portal" {
            self.portals.lock().remove(&p);
        }
    }

    pub fn load_portals(&self, path: &std::path::Path) {
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let mut set = self.portals.lock();
        for line in text.lines() {
            let v: Vec<i32> = line.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            if v.len() == 3 {
                set.insert([v[0], v[1], v[2]]);
            }
        }
    }

    pub fn save_portals(&self, path: &std::path::Path) -> std::io::Result<()> {
        let set: Vec<[i32; 3]> = self.portals.lock().iter().copied().collect();
        if set.is_empty() && !path.exists() {
            return Ok(());
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text: String = set.iter().map(|p| format!("{} {} {}\n", p[0], p[1], p[2])).collect();
        std::fs::write(path, text)
    }
}

/// The portal block a player's bounding box touches, if any:
/// (position, kind) with kind "nether_portal" / "end_portal" / "end_gateway".
pub fn portal_touched(g: &Game, p: &Player) -> Option<([i32; 3], &'static str)> {
    let pos = p.pose.lock().pos;
    let (x0, x1) = ((pos[0] - 0.3).floor() as i32, (pos[0] + 0.3 - 1e-7).floor() as i32);
    let (z0, z1) = ((pos[2] - 0.3).floor() as i32, (pos[2] + 0.3 - 1e-7).floor() as i32);
    let (y0, y1) = (pos[1].floor() as i32, (pos[1] + 1.8 - 1e-7).floor() as i32);
    for y in y0..=y1 {
        for x in x0..=x1 {
            for z in z0..=z1 {
                let s = g.bs([x, y, z]);
                match s.name() {
                    "nether_portal" => return Some(([x, y, z], "nether_portal")),
                    // EndPortalBlock: entity inside the 12/16-high shape
                    "end_portal" if pos[1] < y as f64 + 0.75 => return Some(([x, y, z], "end_portal")),
                    "end_gateway" => return Some(([x, y, z], "end_gateway")),
                    _ => {}
                }
            }
        }
    }
    None
}
