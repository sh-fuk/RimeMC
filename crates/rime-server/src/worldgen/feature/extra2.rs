//! More feature types (cave, lush cave and ocean decoration), ported from
//! the 26.1 bytecode.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::extra::{add, is_water_block, neg, prop, st, up, with, P, ALL, DOWN, EAST, HORIZONTAL, NORTH, SOUTH, UP, WEST};
use super::place::{block_set, parse_state, BlockSet, IntP, Pred, StateP};
use super::tree::{shuffle, JSet};
use super::{Builder, Placed, Region};
use crate::registry::{vstate, BlockState};
use crate::worldgen::chunk::MIN_Y;
use crate::worldgen::random::{Rng, WgRandom};

// ------------------------------------------------------------ helpers

fn f32v(v: &Value, k: &str, d: f32) -> f32 {
    v[k].as_f64().map(|x| x as f32).unwrap_or(d)
}
fn i32v(v: &Value, k: &str, d: i32) -> i32 {
    v[k].as_i64().map(|x| x as i32).unwrap_or(d)
}
fn boolv(v: &Value, k: &str, d: bool) -> bool {
    v[k].as_bool().unwrap_or(d)
}

pub fn clockwise(d: P) -> P {
    [-d[2], 0, d[0]]
}
pub fn counter_clockwise(d: P) -> P {
    [d[2], 0, -d[0]]
}
pub fn dir_from_name(s: &str) -> P {
    match s {
        "down" => DOWN,
        "up" => UP,
        "north" => NORTH,
        "south" => SOUTH,
        "west" => WEST,
        _ => EAST,
    }
}
pub fn same_axis(a: P, b: P) -> bool {
    a == b || a == neg(b)
}

/// `FloatProvider`
#[derive(Clone)]
pub enum FloatP {
    Const(f32),
    Uniform(f32, f32),
    ClampedNormal { mean: f32, dev: f32, min: f32, max: f32 },
    Trapezoid { min: f32, max: f32, plateau: f32 },
}

impl FloatP {
    pub fn parse(v: &Value) -> Result<FloatP> {
        if let Some(f) = v.as_f64() {
            return Ok(FloatP::Const(f as f32));
        }
        let g = |k: &str| v[k].as_f64().map(|x| x as f32).ok_or_else(|| anyhow!("float provider missing {k}: {v}"));
        Ok(match v["type"].as_str().unwrap_or("") {
            "minecraft:constant" => FloatP::Const(g("value")?),
            "minecraft:uniform" => FloatP::Uniform(g("min_inclusive")?, g("max_exclusive")?),
            "minecraft:clamped_normal" => FloatP::ClampedNormal { mean: g("mean")?, dev: g("deviation")?, min: g("min")?, max: g("max")? },
            "minecraft:trapezoid" => FloatP::Trapezoid { min: g("min")?, max: g("max")?, plateau: g("plateau")? },
            t => return Err(anyhow!("unsupported float provider {t}")),
        })
    }
    pub fn sample(&self, r: &mut WgRandom) -> f32 {
        match *self {
            FloatP::Const(c) => c,
            // Mth.randomBetween
            FloatP::Uniform(a, b) => r.next_float() * (b - a) + a,
            FloatP::ClampedNormal { mean, dev, min, max } => (mean + r.next_gaussian() as f32 * dev).clamp(min, max),
            FloatP::Trapezoid { min, max, plateau } => {
                let f = max - min;
                let g = (f - plateau) / 2.0;
                let h = f - g;
                min + r.next_float() * h + r.next_float() * g
            }
        }
    }
}

/// `Util.getRandom(list)`
fn pick<T: Copy>(v: &[T], r: &mut WgRandom) -> T {
    v[r.next_int_bound(v.len() as i32) as usize]
}

/// `Direction.allShuffled`
pub fn all_shuffled(r: &mut WgRandom) -> Vec<P> {
    let mut v = ALL.to_vec();
    shuffle(&mut v, r);
    v
}

// ------------------------------------------------------------ types

pub struct VegPatch {
    replaceable: BlockSet,
    ground: StateP,
    vegetation: Arc<Placed>,
    ceiling: bool,
    depth: IntP,
    extra_bottom: f32,
    vertical_range: i32,
    vegetation_chance: f32,
    xz_radius: IntP,
    extra_edge: f32,
    waterlogged: bool,
}

pub struct RootSystem {
    tree: Arc<Placed>,
    space: i32,
    root_radius: i32,
    root_replaceable: BlockSet,
    root_state: StateP,
    root_attempts: i32,
    column_max: i32,
    hanging_radius: i32,
    hanging_span: i32,
    hanging_state: StateP,
    hanging_attempts: i32,
    water: i32,
    allowed: Pred,
}

pub struct Multiface {
    block: BlockState,
    search_range: i32,
    dirs: Vec<P>,
    spread: f32,
    on: BlockSet,
}

pub enum Extra2 {
    VegPatch(Box<VegPatch>),
    RootSystem(Box<RootSystem>),
    BlockColumn { layers: Vec<(IntP, StateP)>, dir: P, allowed: Pred, tip: bool },
    Multiface(Box<Multiface>),
    Coral(u8),
    BlockBlob { state: BlockState, on: Pred },
    Other(Box<super::extra3::Extra3>),
}

impl Extra2 {
    pub fn parse(b: &mut Builder, ty: &str, c: &Value) -> Option<Result<Extra2>> {
        let r = (|| -> Result<Option<Extra2>> {
            Ok(Some(match ty {
                "vegetation_patch" | "waterlogged_vegetation_patch" => Extra2::VegPatch(Box::new(VegPatch {
                    replaceable: b.tag(c["replaceable"].as_str().unwrap_or(""))?,
                    ground: StateP::parse(b, &c["ground_state"])?,
                    vegetation: b.placed(&c["vegetation_feature"])?,
                    ceiling: c["surface"].as_str() == Some("ceiling"),
                    depth: IntP::parse(&c["depth"])?,
                    extra_bottom: f32v(c, "extra_bottom_block_chance", 0.0),
                    vertical_range: i32v(c, "vertical_range", 0),
                    vegetation_chance: f32v(c, "vegetation_chance", 0.0),
                    xz_radius: IntP::parse(&c["xz_radius"])?,
                    extra_edge: f32v(c, "extra_edge_column_chance", 0.0),
                    waterlogged: ty == "waterlogged_vegetation_patch",
                })),
                "root_system" => Extra2::RootSystem(Box::new(RootSystem {
                    tree: b.placed(&c["feature"])?,
                    space: i32v(c, "required_vertical_space_for_tree", 1),
                    root_radius: i32v(c, "root_radius", 1),
                    root_replaceable: b.tag(c["root_replaceable"].as_str().unwrap_or(""))?,
                    root_state: StateP::parse(b, &c["root_state_provider"])?,
                    root_attempts: i32v(c, "root_placement_attempts", 0),
                    column_max: i32v(c, "root_column_max_height", 1),
                    hanging_radius: i32v(c, "hanging_root_radius", 1),
                    hanging_span: i32v(c, "hanging_roots_vertical_span", 1),
                    hanging_state: StateP::parse(b, &c["hanging_root_state_provider"])?,
                    hanging_attempts: i32v(c, "hanging_root_placement_attempts", 0),
                    water: i32v(c, "allowed_vertical_water_for_tree", 0),
                    allowed: Pred::parse(b, &c["allowed_tree_position"])?,
                })),
                "block_column" => {
                    let mut layers = Vec::new();
                    for l in c["layers"].as_array().into_iter().flatten() {
                        layers.push((IntP::parse(&l["height"])?, StateP::parse(b, &l["provider"])?));
                    }
                    Extra2::BlockColumn { layers, dir: dir_from_name(c["direction"].as_str().unwrap_or("up")), allowed: Pred::parse(b, &c["allowed_placement"])?, tip: boolv(c, "prioritize_tip", false) }
                }
                "multiface_growth" => {
                    let mut dirs = Vec::new();
                    if boolv(c, "can_place_on_ceiling", false) {
                        dirs.push(UP);
                    }
                    if boolv(c, "can_place_on_floor", false) {
                        dirs.push(DOWN);
                    }
                    if boolv(c, "can_place_on_wall", false) {
                        dirs.extend(HORIZONTAL);
                    }
                    let name = c["block"].as_str().unwrap_or("minecraft:glow_lichen");
                    Extra2::Multiface(Box::new(Multiface {
                        block: crate::registry::BlockRegistry::get().state(name, &[]).ok_or_else(|| anyhow!("block {name}"))?,
                        search_range: i32v(c, "search_range", 10),
                        dirs,
                        spread: f32v(c, "chance_of_spreading", 0.5),
                        on: block_set(b, &c["can_be_placed_on"])?,
                    }))
                }
                "coral_tree" => Extra2::Coral(0),
                "coral_claw" => Extra2::Coral(1),
                "coral_mushroom" => Extra2::Coral(2),
                "block_blob" => Extra2::BlockBlob { state: parse_state(&c["state"])?, on: Pred::parse(b, &c["can_place_on"])? },
                _ => match super::extra3::Extra3::parse(b, ty, c) {
                    Some(r) => Extra2::Other(Box::new(r?)),
                    None => return Ok(None),
                },
            }))
        })();
        match r {
            Ok(Some(e)) => Some(Ok(e)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }

    pub fn place(&self, lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
        match self {
            Extra2::VegPatch(v) => veg_patch(lv, r, o, v),
            Extra2::RootSystem(c) => root_system(lv, r, o, c),
            Extra2::BlockColumn { layers, dir, allowed, tip } => block_column(lv, r, o, layers, *dir, allowed, *tip),
            Extra2::Multiface(m) => multiface(lv, r, o, m),
            Extra2::Coral(k) => coral(lv, r, o, *k),
            Extra2::BlockBlob { state, on } => block_blob(lv, r, o, *state, on),
            Extra2::Other(e) => e.place(lv, r, o),
        }
    }
}

// ------------------------------------------------------------ vegetation patches

fn veg_patch(lv: &mut Region, r: &mut WgRandom, o: P, c: &VegPatch) -> bool {
    let xr = c.xz_radius.sample(r) + 1;
    let zr = c.xz_radius.sample(r) + 1;
    let dir = if c.ceiling { UP } else { DOWN };
    let opp = neg(dir);
    let mut set = JSet::default();
    for x in -xr..=xr {
        let bx = x == -xr || x == xr;
        for z in -zr..=zr {
            let bz = z == -zr || z == zr;
            let edge = bx || bz;
            let corner = bx && bz;
            if corner {
                continue;
            }
            if edge && (c.extra_edge == 0.0 || r.next_float() > c.extra_edge) {
                continue;
            }
            let mut m = add(o, [x, 0, z]);
            let mut i = 0;
            while vstate::air(lv.get(m)) && i < c.vertical_range {
                m = add(m, dir);
                i += 1;
            }
            i = 0;
            while !vstate::air(lv.get(m)) && i < c.vertical_range {
                m = add(m, opp);
                i += 1;
            }
            let m2 = add(m, dir);
            let s2 = lv.get(m2);
            if !lv.is_empty(m) || !vstate::sturdy(s2, super::extra::dir_index(opp)) {
                continue;
            }
            let depth = c.depth.sample(r) + if c.extra_bottom > 0.0 && r.next_float() < c.extra_bottom { 1 } else { 0 };
            if place_ground(lv, r, c, m2, depth, dir) {
                set.add(m2);
            }
        }
    }
    let set = if c.waterlogged {
        let mut set2 = JSet::default();
        for p in set.iter() {
            let exposed = [NORTH, EAST, SOUTH, WEST, DOWN].iter().any(|d| !vstate::sturdy(lv.get(add(p, *d)), super::extra::dir_index(neg(*d))));
            if !exposed {
                set2.add(p);
            }
        }
        let water = crate::registry::blocks::WATER;
        for p in set2.iter() {
            lv.set(p, water);
        }
        set2
    } else {
        set
    };
    for p in set.iter() {
        if c.vegetation_chance > 0.0 && r.next_float() < c.vegetation_chance {
            if c.waterlogged {
                let q = up(p, -1);
                if c.vegetation.place(lv, r, add(q, opp)) {
                    let s = lv.get(p);
                    if prop(s, "waterlogged") == Some("false") {
                        lv.set(p, with(s, "waterlogged", "true"));
                    }
                }
            } else {
                c.vegetation.place(lv, r, add(p, opp));
            }
        }
    }
    !set.is_empty()
}

fn place_ground(lv: &mut Region, r: &mut WgRandom, c: &VegPatch, start: P, depth: i32, dir: P) -> bool {
    let mut m = start;
    for i in 0..depth {
        let s = c.ground.get(lv, r, m).unwrap_or(crate::registry::blocks::AIR);
        let cur = lv.get(m);
        if s.name() == cur.name() {
            continue;
        }
        if !c.replaceable.contains(cur) {
            return i != 0;
        }
        lv.set(m, s);
        m = add(m, dir);
    }
    true
}

// ------------------------------------------------------------ azalea roots

fn root_system(lv: &mut Region, r: &mut WgRandom, o: P, c: &RootSystem) -> bool {
    if !vstate::air(lv.get(o)) {
        return false;
    }
    let mut m = o;
    let mut ok = false;
    for i in 0..c.column_max {
        m = up(m, 1);
        if !c.allowed.test(lv, m) || !space_for_tree(lv, c, m) {
            continue;
        }
        let below = up(m, -1);
        let bs = lv.get(below);
        if matches!(vstate::fluid(bs), 3 | 4) || !vstate::solid(bs) {
            return false;
        }
        if c.tree.place(lv, r, m) {
            // placeDirt
            for y in o[1]..o[1] + i {
                rooted_dirt(lv, r, c, [o[0], y, o[2]]);
            }
            ok = true;
            break;
        }
    }
    if ok {
        let (hr, vs) = (c.hanging_radius, c.hanging_span);
        for _ in 0..c.hanging_attempts {
            let dx = r.next_int_bound(hr) - r.next_int_bound(hr);
            let dy = r.next_int_bound(vs) - r.next_int_bound(vs);
            let dz = r.next_int_bound(hr) - r.next_int_bound(hr);
            let p = add(o, [dx, dy, dz]);
            if !lv.is_empty(p) {
                continue;
            }
            let Some(s) = c.hanging_state.get(lv, r, p) else { continue };
            if super::survive::can_survive(lv, s, p) && vstate::sturdy(lv.get(up(p, 1)), 0) {
                lv.set(p, s);
            }
        }
    }
    true
}

fn space_for_tree(lv: &Region, c: &RootSystem, m: P) -> bool {
    let mut p = m;
    for i in 1..=c.space {
        p = up(p, 1);
        let s = lv.get(p);
        let ok = vstate::air(s) || (i + 1 <= c.water && vstate::is_water(s));
        if !ok {
            return false;
        }
    }
    true
}

fn rooted_dirt(lv: &mut Region, r: &mut WgRandom, c: &RootSystem, p: P) {
    let k = c.root_radius;
    for _ in 0..c.root_attempts {
        let dx = r.next_int_bound(k) - r.next_int_bound(k);
        let dz = r.next_int_bound(k) - r.next_int_bound(k);
        let q = add(p, [dx, 0, dz]);
        if c.root_replaceable.contains(lv.get(q)) {
            if let Some(s) = c.root_state.get(lv, r, q) {
                lv.set(q, s);
            }
        }
    }
}

// ------------------------------------------------------------ block columns

fn block_column(lv: &mut Region, r: &mut WgRandom, o: P, layers: &[(IntP, StateP)], dir: P, allowed: &Pred, tip: bool) -> bool {
    let n = layers.len();
    let mut h: Vec<i32> = Vec::with_capacity(n);
    let mut total = 0;
    for (p, _) in layers {
        let v = p.sample(r);
        h.push(v);
        total += v;
    }
    if total == 0 {
        return false;
    }
    let mut m = add(o, dir);
    for i in 0..total {
        if !allowed.test(lv, m) {
            // truncate
            let mut k = total - i;
            let (step, start, end): (i32, i32, i32) = if tip { (1, 0, n as i32) } else { (-1, n as i32 - 1, -1) };
            let mut j = start;
            while j != end && k > 0 {
                let x = h[j as usize];
                let y = x.min(k);
                k -= y;
                h[j as usize] -= y;
                j += step;
            }
            break;
        }
        m = add(m, dir);
    }
    let mut p = o;
    for (i, (_, prov)) in layers.iter().enumerate() {
        for _ in 0..h[i] {
            if let Some(s) = prov.get(lv, r, p) {
                lv.set(p, s);
            }
            p = add(p, dir);
        }
    }
    true
}

// ------------------------------------------------------------ glow lichen

fn air_or_water(s: BlockState) -> bool {
    vstate::air(s) || is_water_block(s)
}

/// `MultifaceBlock.isValidStateForPlacement`
pub fn mf_valid(lv: &Region, block: BlockState, s: BlockState, p: P, d: P) -> bool {
    if s.name() == block.name() && prop(s, super::extra::face_name(d)) == Some("true") {
        return false;
    }
    let n = add(p, d);
    vstate::sturdy(lv.get(n), super::extra::dir_index(neg(d)))
}

/// `MultifaceBlock.getStateForPlacement(state, level, pos, dir)`
pub fn mf_state(lv: &Region, block: BlockState, s: BlockState, p: P, d: P) -> Option<BlockState> {
    if !mf_valid(lv, block, s, p, d) {
        return None;
    }
    let base = if s.name() == block.name() {
        s
    } else if vstate::fluid(s) == 1 {
        with(block, "waterlogged", "true")
    } else {
        block
    };
    Some(with(base, super::extra::face_name(d), "true"))
}

pub fn mf_has_face(s: BlockState, d: P) -> bool {
    prop(s, super::extra::face_name(d)) == Some("true")
}

/// `MultifaceSpreader.spreadFromFaceTowardRandomDirection` (default config)
fn mf_spread(lv: &mut Region, r: &mut WgRandom, block: BlockState, s: BlockState, p: P, face: P) {
    for sd in all_shuffled(r) {
        if same_axis(sd, face) {
            continue;
        }
        if !(mf_has_face(s, face) && !mf_has_face(s, sd)) {
            continue;
        }
        // SAME_POSITION, SAME_PLANE, WRAP_AROUND
        let cands = [(p, sd), (add(p, sd), face), (add(add(p, sd), face), neg(sd))];
        let mut found = None;
        for (q, f) in cands {
            let t = lv.get(q);
            let replaceable = vstate::air(t) || t.name() == block.name() || (is_water_block(t) && vstate::fluid(t) == 1);
            if replaceable && mf_valid(lv, block, t, q, f) {
                found = Some((q, f));
                break;
            }
        }
        let Some((q, f)) = found else { continue };
        let old = lv.get(q);
        if let Some(ns) = mf_state(lv, block, old, q, f) {
            lv.mark_post(q);
            lv.set(q, ns);
            return;
        }
    }
}

fn mf_place_if_possible(lv: &mut Region, r: &mut WgRandom, m: &Multiface, p: P, s: BlockState, dirs: &[P]) -> bool {
    for &d in dirs {
        if !m.on.contains(lv.get(add(p, d))) {
            continue;
        }
        let Some(ns) = mf_state(lv, m.block, s, p, d) else { return false };
        lv.set(p, ns);
        lv.mark_post(p);
        if r.next_float() < m.spread {
            mf_spread(lv, r, m.block, ns, p, d);
        }
        return true;
    }
    false
}

fn multiface(lv: &mut Region, r: &mut WgRandom, o: P, m: &Multiface) -> bool {
    if !air_or_water(lv.get(o)) {
        return false;
    }
    let mut dirs = m.dirs.clone();
    shuffle(&mut dirs, r);
    let s = lv.get(o);
    if mf_place_if_possible(lv, r, m, o, s, &dirs) {
        return true;
    }
    for &d in &dirs {
        let mut except: Vec<P> = m.dirs.iter().copied().filter(|x| *x != neg(d)).collect();
        shuffle(&mut except, r);
        for _ in 0..m.search_range {
            // vanilla re-checks origin + d every step (setWithOffset(origin, d))
            let p = add(o, d);
            let s = lv.get(p);
            if !air_or_water(s) && s.name() != m.block.name() {
                break;
            }
            if mf_place_if_possible(lv, r, m, p, s, &except) {
                return true;
            }
        }
    }
    false
}

// ------------------------------------------------------------ corals

const CORALS: [&str; 10] = ["tube_coral", "brain_coral", "bubble_coral", "fire_coral", "horn_coral", "tube_coral_fan", "brain_coral_fan", "bubble_coral_fan", "fire_coral_fan", "horn_coral_fan"];
const CORAL_BLOCKS: [&str; 5] = ["tube_coral_block", "brain_coral_block", "bubble_coral_block", "fire_coral_block", "horn_coral_block"];
const WALL_CORALS: [&str; 5] = ["tube_coral_wall_fan", "brain_coral_wall_fan", "bubble_coral_wall_fan", "fire_coral_wall_fan", "horn_coral_wall_fan"];

fn coral_block(lv: &mut Region, r: &mut WgRandom, p: P, block: BlockState) -> bool {
    let above = up(p, 1);
    let cur = lv.get(p);
    if !is_water_block(cur) && !CORALS.contains(&cur.name()) || !is_water_block(lv.get(above)) {
        return false;
    }
    lv.set(p, block);
    if r.next_float() < 0.25 {
        let c = pick(&CORALS, r);
        lv.set(above, st(c));
    } else if r.next_float() < 0.05 {
        let n = (r.next_int_bound(4) + 1).to_string();
        lv.set(above, with(st("sea_pickle"), "pickles", &n));
    }
    for d in HORIZONTAL {
        if r.next_float() < 0.2 {
            let q = add(p, d);
            if is_water_block(lv.get(q)) {
                let w = pick(&WALL_CORALS, r);
                lv.set(q, with(st(w), "facing", super::extra::face_name(d)));
            }
        }
    }
    true
}

fn coral(lv: &mut Region, r: &mut WgRandom, o: P, kind: u8) -> bool {
    let block = st(pick(&CORAL_BLOCKS, r));
    match kind {
        0 => {
            let mut m = o;
            let i = r.next_int_bound(3) + 1;
            for _ in 0..i {
                if !coral_block(lv, r, m, block) {
                    return true;
                }
                m = up(m, 1);
            }
            let base = m;
            let k = r.next_int_bound(3) + 2;
            let mut dirs = HORIZONTAL.to_vec();
            shuffle(&mut dirs, r);
            for &d in &dirs[..k as usize] {
                m = add(base, d);
                let l = r.next_int_bound(5) + 2;
                let mut n = 0;
                for o2 in 0..l {
                    if !coral_block(lv, r, m, block) {
                        break;
                    }
                    n += 1;
                    m = up(m, 1);
                    if o2 == 0 || n >= 2 && r.next_float() < 0.25 {
                        m = add(m, d);
                        n = 0;
                    }
                }
            }
            true
        }
        1 => {
            if !coral_block(lv, r, o, block) {
                return false;
            }
            let d = pick(&HORIZONTAL, r);
            let i = r.next_int_bound(2) + 2;
            let mut list = vec![d, clockwise(d), counter_clockwise(d)];
            shuffle(&mut list, r);
            for &dir in &list[..i as usize] {
                let mut m = o;
                let j = r.next_int_bound(2) + 1;
                m = add(m, dir);
                let (e, k);
                if dir == d {
                    e = d;
                    k = r.next_int_bound(3) + 2;
                } else {
                    m = up(m, 1);
                    e = pick(&[dir, UP], r);
                    k = r.next_int_bound(3) + 3;
                }
                for _ in 0..j {
                    if !coral_block(lv, r, m, block) {
                        break;
                    }
                    m = add(m, e);
                }
                m = add(m, neg(e));
                m = up(m, 1);
                for _ in 0..k {
                    m = add(m, d);
                    if !coral_block(lv, r, m, block) {
                        break;
                    }
                    if r.next_float() < 0.25 {
                        m = up(m, 1);
                    }
                }
            }
            true
        }
        _ => {
            let i = r.next_int_bound(3) + 3;
            let j = r.next_int_bound(3) + 3;
            let k = r.next_int_bound(3) + 3;
            let l = r.next_int_bound(3) + 1;
            for a in 0..=j {
                for b in 0..=i {
                    for c in 0..=k {
                        let p = [o[0] + a, o[1] + b - l, o[2] + c];
                        let ea = a == 0 || a == j;
                        let eb = b == 0 || b == i;
                        let ec = c == 0 || c == k;
                        if ea && eb || ec && eb || ea && ec || !ea && !eb && !ec {
                            continue;
                        }
                        if r.next_float() < 0.1 {
                            continue;
                        }
                        coral_block(lv, r, p, block);
                    }
                }
            }
            true
        }
    }
}

// ------------------------------------------------------------ mossy boulders

fn block_blob(lv: &mut Region, r: &mut WgRandom, o: P, state: BlockState, on: &Pred) -> bool {
    let mut o = o;
    while o[1] > MIN_Y + 3 {
        if on.test(lv, up(o, -1)) {
            break;
        }
        o = up(o, -1);
    }
    if o[1] <= MIN_Y + 3 {
        return false;
    }
    for _ in 0..3 {
        let j = r.next_int_bound(2);
        let k = r.next_int_bound(2);
        let l = r.next_int_bound(2);
        let f = (j + k + l) as f32 * 0.333 + 0.5;
        let lim = (f * f) as f64;
        for z in o[2] - l..=o[2] + l {
            for y in o[1] - k..=o[1] + k {
                for x in o[0] - j..=o[0] + j {
                    let (a, b, c) = ((x - o[0]) as f64, (y - o[1]) as f64, (z - o[2]) as f64);
                    if a * a + b * b + c * c <= lim {
                        lv.set([x, y, z], state);
                    }
                }
            }
        }
        o = add(o, [-1 + r.next_int_bound(2), -r.next_int_bound(2), -1 + r.next_int_bound(2)]);
    }
    true
}
