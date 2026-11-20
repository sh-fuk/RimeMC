//! `TreeFeature` with its trunk, foliage and root placers and tree
//! decorators (ported from the 26.1 bytecode).
//!
//! Vanilla keeps the placed positions in `java.util.HashSet`s and the
//! decorators iterate them (sorted by y, stably), so their iteration order is
//! reproduced exactly with [`JSet`].

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::place::{between, BlockSet, IntP, StateP};
use super::{feat::Feature, Builder, Hm, Region};
use crate::registry::{vstate, BlockRegistry, BlockState};
use crate::worldgen::carver::{mcos, msin};
use crate::worldgen::chunk::{HEIGHT, MIN_Y};
use crate::worldgen::random::{Rng, WgRandom};

type P = [i32; 3];

const DOWN: P = [0, -1, 0];
const UP: P = [0, 1, 0];
const NORTH: P = [0, 0, -1];
const SOUTH: P = [0, 0, 1];
const WEST: P = [-1, 0, 0];
const EAST: P = [1, 0, 0];
/// `Direction.values()`
const ALL: [P; 6] = [DOWN, UP, NORTH, SOUTH, WEST, EAST];
/// `Direction.Plane.HORIZONTAL`
const HORIZONTAL: [P; 4] = [NORTH, EAST, SOUTH, WEST];

#[inline]
fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
fn rel(a: P, d: P, k: i32) -> P {
    [a[0] + d[0] * k, a[1] + d[1] * k, a[2] + d[2] * k]
}
#[inline]
fn above(a: P, k: i32) -> P {
    [a[0], a[1] + k, a[2]]
}
fn neg(d: P) -> P {
    [-d[0], -d[1], -d[2]]
}
fn clockwise(d: P) -> P {
    [-d[2], 0, d[0]]
}
fn manhattan(a: P, b: P) -> i32 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()
}
fn random_dir(r: &mut WgRandom) -> P {
    HORIZONTAL[r.next_int_bound(4) as usize]
}
fn dir_name(d: P) -> &'static str {
    match d {
        NORTH => "north",
        SOUTH => "south",
        WEST => "west",
        EAST => "east",
        UP => "up",
        _ => "down",
    }
}
fn parse_dir(s: &str) -> P {
    match s.trim_start_matches("minecraft:") {
        "north" => NORTH,
        "south" => SOUTH,
        "west" => WEST,
        "east" => EAST,
        "up" => UP,
        _ => DOWN,
    }
}

// ------------------------------------------------------------ java.util.HashSet

/// A `java.util.HashSet<BlockPos>` replica: same bucket layout, so
/// iteration order and `iterator().next()` match vanilla.
#[derive(Default, Clone)]
pub struct JSet {
    table: Vec<Vec<P>>,
    size: usize,
}

fn jhash(p: P) -> u32 {
    let h = (p[1].wrapping_add(p[2].wrapping_mul(31))).wrapping_mul(31).wrapping_add(p[0]) as u32;
    h ^ (h >> 16)
}

impl JSet {
    fn cap(&self) -> usize {
        self.table.len()
    }
    pub fn contains(&self, p: P) -> bool {
        self.size > 0 && self.table[jhash(p) as usize & (self.cap() - 1)].contains(&p)
    }
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
    fn resize(&mut self) {
        let n = if self.table.is_empty() { 16 } else { self.cap() * 2 };
        let mut t: Vec<Vec<P>> = vec![Vec::new(); n];
        for b in &self.table {
            for &p in b {
                t[jhash(p) as usize & (n - 1)].push(p);
            }
        }
        self.table = t;
    }
    pub fn add(&mut self, p: P) {
        if self.table.is_empty() {
            self.resize();
        }
        let i = jhash(p) as usize & (self.cap() - 1);
        if self.table[i].contains(&p) {
            return;
        }
        self.table[i].push(p);
        // treeifyBin on a small table resizes instead
        if self.table[i].len() > 8 && self.cap() < 64 {
            self.resize();
        }
        self.size += 1;
        if self.size > self.cap() * 3 / 4 {
            self.resize();
        }
    }
    /// `iterator().next()` + `remove()`
    fn pop_first(&mut self) -> Option<P> {
        for b in &mut self.table {
            if !b.is_empty() {
                self.size -= 1;
                return Some(b.remove(0));
            }
        }
        None
    }
    pub fn iter(&self) -> impl Iterator<Item = P> + '_ {
        self.table.iter().flatten().copied()
    }
    /// `new ObjectArrayList<>(set)` sorted (stably) by y.
    fn sorted_by_y(&self) -> Vec<P> {
        let mut v: Vec<P> = self.iter().collect();
        v.sort_by_key(|p| p[1]);
        v
    }
}

// ------------------------------------------------------------ config

#[derive(Clone)]
pub struct Tags {
    replaceable: BlockSet,
    logs: BlockSet,
    leaves: BlockSet,
    no_decay: BlockSet,
}

impl Tags {
    pub fn load(b: &mut Builder) -> Result<Tags> {
        Ok(Tags { replaceable: b.tag("replaceable_by_trees")?, logs: b.tag("logs")?, leaves: b.tag("leaves")?, no_decay: b.tag("prevents_nearby_leaf_decay")? })
    }
}

enum Size {
    Two { limit: i32, lower: i32, upper: i32 },
    Three { limit: i32, upper_limit: i32, lower: i32, middle: i32, upper: i32 },
}

impl Size {
    fn at(&self, height: i32, y: i32) -> i32 {
        match *self {
            Size::Two { limit, lower, upper } => {
                if y < limit {
                    lower
                } else {
                    upper
                }
            }
            Size::Three { limit, upper_limit, lower, middle, upper } => {
                if y < limit {
                    lower
                } else if y >= height - upper_limit {
                    upper
                } else {
                    middle
                }
            }
        }
    }
}

enum TrunkKind {
    Straight,
    Forking,
    Giant,
    MegaJungle,
    DarkOak,
    Fancy,
    Bending { min_leaves: i32, bend: IntP },
    Upwards { steps: IntP, prob: f32, len: IntP, through: BlockSet },
    Cherry { count: IntP, hlen: IntP, start: (i32, i32), end: IntP },
}

struct Trunk {
    base: i32,
    a: i32,
    b: i32,
    kind: TrunkKind,
}

enum FoliageKind {
    Blob(i32),
    Bush(i32),
    Fancy(i32),
    Acacia,
    DarkOak,
    Jungle(i32),
    MegaPine(IntP),
    Pine(IntP),
    Spruce(IntP),
    RandomSpread(IntP, i32),
    Cherry { height: IntP, wide: f32, corner: f32, hang: f32, ext: f32 },
}

struct Foliage {
    radius: IntP,
    offset: IntP,
    kind: FoliageKind,
}

struct Roots {
    offset: IntP,
    provider: StateP,
    above: Option<(f32, StateP)>,
    through: BlockSet,
    muddy_in: BlockSet,
    muddy: StateP,
    max_len: i32,
    max_width: i32,
    skew: f32,
}

enum Deco {
    Beehive(f32),
    Cocoa(f32),
    TrunkVine,
    LeaveVine(f32),
    AlterGround(StateP),
    AttachedToLeaves { prob: f32, xz: i32, y: i32, provider: StateP, empty: i32, dirs: Vec<P> },
    PlaceOnGround { tries: i32, radius: i32, height: i32, provider: StateP },
    CreakingHeart(f32),
    PaleMoss { leaves: f32, trunk: f32, ground: f32, patch: Arc<Feature> },
}

pub struct Tree {
    trunk_provider: StateP,
    foliage_provider: StateP,
    below_trunk: StateP,
    trunk: Trunk,
    foliage: Foliage,
    roots: Option<Roots>,
    size: Size,
    min_clipped: Option<i32>,
    ignore_vines: bool,
    decorators: Vec<Deco>,
    tags: Tags,
}

fn f32v(v: &Value, k: &str) -> f32 {
    v[k].as_f64().unwrap_or(0.0) as f32
}
fn i32v(v: &Value, k: &str, d: i32) -> i32 {
    v[k].as_i64().map(|x| x as i32).unwrap_or(d)
}

impl Tree {
    pub fn parse(b: &mut Builder, c: &Value) -> Result<Tree> {
        let t = &c["trunk_placer"];
        let tk = match t["type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
            "straight_trunk_placer" => TrunkKind::Straight,
            "forking_trunk_placer" => TrunkKind::Forking,
            "giant_trunk_placer" => TrunkKind::Giant,
            "mega_jungle_trunk_placer" => TrunkKind::MegaJungle,
            "dark_oak_trunk_placer" => TrunkKind::DarkOak,
            "fancy_trunk_placer" => TrunkKind::Fancy,
            "bending_trunk_placer" => TrunkKind::Bending { min_leaves: i32v(t, "min_height_for_leaves", 1), bend: IntP::parse(&t["bend_length"])? },
            "upwards_branching_trunk_placer" => TrunkKind::Upwards {
                steps: IntP::parse(&t["extra_branch_steps"])?,
                prob: f32v(t, "place_branch_per_log_probability"),
                len: IntP::parse(&t["extra_branch_length"])?,
                through: super::place::block_set(b, &t["can_grow_through"])?,
            },
            "cherry_trunk_placer" => TrunkKind::Cherry {
                count: IntP::parse(&t["branch_count"])?,
                hlen: IntP::parse(&t["branch_horizontal_length"])?,
                start: (i32v(&t["branch_start_offset_from_top"], "min_inclusive", 0), i32v(&t["branch_start_offset_from_top"], "max_inclusive", 0)),
                end: IntP::parse(&t["branch_end_offset_from_top"])?,
            },
            other => return Err(anyhow!("trunk placer {other}")),
        };
        let trunk = Trunk { base: i32v(t, "base_height", 0), a: i32v(t, "height_rand_a", 0), b: i32v(t, "height_rand_b", 0), kind: tk };
        let f = &c["foliage_placer"];
        let fk = match f["type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
            "blob_foliage_placer" => FoliageKind::Blob(i32v(f, "height", 0)),
            "bush_foliage_placer" => FoliageKind::Bush(i32v(f, "height", 0)),
            "fancy_foliage_placer" => FoliageKind::Fancy(i32v(f, "height", 0)),
            "acacia_foliage_placer" => FoliageKind::Acacia,
            "dark_oak_foliage_placer" => FoliageKind::DarkOak,
            "jungle_foliage_placer" => FoliageKind::Jungle(i32v(f, "height", 0)),
            "mega_pine_foliage_placer" => FoliageKind::MegaPine(IntP::parse(&f["crown_height"])?),
            "pine_foliage_placer" => FoliageKind::Pine(IntP::parse(&f["height"])?),
            "spruce_foliage_placer" => FoliageKind::Spruce(IntP::parse(&f["trunk_height"])?),
            "random_spread_foliage_placer" => FoliageKind::RandomSpread(IntP::parse(&f["foliage_height"])?, i32v(f, "leaf_placement_attempts", 0)),
            "cherry_foliage_placer" => FoliageKind::Cherry {
                height: IntP::parse(&f["height"])?,
                wide: f32v(f, "wide_bottom_layer_hole_chance"),
                corner: f32v(f, "corner_hole_chance"),
                hang: f32v(f, "hanging_leaves_chance"),
                ext: f32v(f, "hanging_leaves_extension_chance"),
            },
            other => return Err(anyhow!("foliage placer {other}")),
        };
        let foliage = Foliage { radius: IntP::parse(&f["radius"])?, offset: IntP::parse(&f["offset"])?, kind: fk };
        let s = &c["minimum_size"];
        let size = if s["type"].as_str() == Some("minecraft:three_layers_feature_size") {
            Size::Three { limit: i32v(s, "limit", 1), upper_limit: i32v(s, "upper_limit", 1), lower: i32v(s, "lower_size", 0), middle: i32v(s, "middle_size", 1), upper: i32v(s, "upper_size", 1) }
        } else {
            Size::Two { limit: i32v(s, "limit", 1), lower: i32v(s, "lower_size", 0), upper: i32v(s, "upper_size", 1) }
        };
        let min_clipped = s["min_clipped_height"].as_i64().map(|x| x as i32);
        let roots = if c["root_placer"].is_object() {
            let rp = &c["root_placer"];
            let m = &rp["mangrove_root_placement"];
            let above = if rp["above_root_placement"].is_object() {
                let a = &rp["above_root_placement"];
                Some((f32v(a, "above_root_placement_chance"), StateP::parse(b, &a["above_root_provider"])?))
            } else {
                None
            };
            Some(Roots {
                offset: IntP::parse(&rp["trunk_offset_y"])?,
                provider: StateP::parse(b, &rp["root_provider"])?,
                above,
                through: super::place::block_set(b, &m["can_grow_through"])?,
                muddy_in: super::place::block_set(b, &m["muddy_roots_in"])?,
                muddy: StateP::parse(b, &m["muddy_roots_provider"])?,
                max_len: i32v(m, "max_root_length", 0),
                max_width: i32v(m, "max_root_width", 0),
                skew: f32v(m, "random_skew_chance"),
            })
        } else {
            None
        };
        let mut decorators = Vec::new();
        for d in c["decorators"].as_array().into_iter().flatten() {
            decorators.push(match d["type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
                "beehive" => Deco::Beehive(f32v(d, "probability")),
                "cocoa" => Deco::Cocoa(f32v(d, "probability")),
                "trunk_vine" => Deco::TrunkVine,
                "leave_vine" => Deco::LeaveVine(f32v(d, "probability")),
                "alter_ground" => Deco::AlterGround(StateP::parse(b, &d["provider"])?),
                "attached_to_leaves" => Deco::AttachedToLeaves {
                    prob: f32v(d, "probability"),
                    xz: i32v(d, "exclusion_radius_xz", 0),
                    y: i32v(d, "exclusion_radius_y", 0),
                    provider: StateP::parse(b, &d["block_provider"])?,
                    empty: i32v(d, "required_empty_blocks", 0),
                    dirs: d["directions"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).map(parse_dir).collect(),
                },
                "place_on_ground" => Deco::PlaceOnGround { tries: i32v(d, "tries", 128), radius: i32v(d, "radius", 2), height: i32v(d, "height", 1), provider: StateP::parse(b, &d["block_state_provider"])? },
                "creaking_heart" => Deco::CreakingHeart(f32v(d, "probability")),
                "pale_moss" => Deco::PaleMoss {
                    leaves: f32v(d, "leaves_probability"),
                    trunk: f32v(d, "trunk_probability"),
                    ground: f32v(d, "ground_probability"),
                    patch: b.configured(&Value::String("minecraft:pale_moss_patch".into()))?,
                },
                other => return Err(anyhow!("tree decorator {other}")),
            });
        }
        Ok(Tree {
            trunk_provider: StateP::parse(b, &c["trunk_provider"])?,
            foliage_provider: StateP::parse(b, &c["foliage_provider"])?,
            below_trunk: StateP::parse(b, &c["below_trunk_provider"])?,
            trunk,
            foliage,
            roots,
            size,
            min_clipped,
            ignore_vines: c["ignore_vines"].as_bool().unwrap_or(false),
            decorators,
            tags: Tags::load(b)?,
        })
    }
}

// ------------------------------------------------------------ placement state

struct Sets {
    roots: JSet,
    trunk: JSet,
    foliage: JSet,
    decos: JSet,
}

struct Attach {
    pos: P,
    radius_offset: i32,
    double: bool,
}

fn attach(pos: P, radius_offset: i32, double: bool) -> Attach {
    Attach { pos, radius_offset, double }
}

struct G<'t, 'r, 'l> {
    t: &'t Tree,
    lv: &'r mut Region<'l>,
    s: Sets,
}

impl G<'_, '_, '_> {
    fn get(&self, p: P) -> BlockState {
        self.lv.get(p)
    }
    fn is_air(&self, p: P) -> bool {
        vstate::air(self.get(p))
    }
    /// `TreeFeature.validTreePos`
    fn valid_tree_pos(&self, p: P) -> bool {
        let s = self.get(p);
        vstate::air(s) || self.t.tags.replaceable.contains(s)
    }
    /// `TrunkPlacer.validTreePos` (overridden by the upwards branching placer)
    fn trunk_valid(&self, p: P) -> bool {
        if self.valid_tree_pos(p) {
            return true;
        }
        match &self.t.trunk.kind {
            TrunkKind::Upwards { through, .. } => through.contains(self.get(p)),
            _ => false,
        }
    }
    fn is_free(&self, p: P) -> bool {
        self.trunk_valid(p) || self.t.tags.logs.contains(self.get(p))
    }
    fn set_trunk(&mut self, p: P, s: BlockState) {
        self.s.trunk.add(p);
        self.lv.set(p, s);
    }
    fn set_root(&mut self, p: P, s: BlockState) {
        self.s.roots.add(p);
        self.lv.set(p, s);
    }
    fn set_foliage(&mut self, p: P, s: BlockState) {
        self.s.foliage.add(p);
        self.lv.set(p, s);
    }
    fn set_deco(&mut self, p: P, s: BlockState) {
        self.s.decos.add(p);
        self.lv.set(p, s);
    }

    fn below_trunk(&mut self, r: &mut WgRandom, p: P) {
        if let Some(s) = self.t.below_trunk.get(self.lv, r, p) {
            self.set_trunk(p, s);
        }
    }

    fn place_log_with(&mut self, r: &mut WgRandom, p: P, axis: Option<&str>) -> bool {
        if !self.trunk_valid(p) {
            return false;
        }
        let Some(mut s) = self.t.trunk_provider.get(self.lv, r, p) else { return false };
        if let Some(a) = axis {
            s = BlockRegistry::get().with(s, "axis", a).unwrap_or(s);
        }
        self.set_trunk(p, s);
        true
    }
    fn place_log(&mut self, r: &mut WgRandom, p: P) -> bool {
        self.place_log_with(r, p, None)
    }
    fn place_log_if_free(&mut self, r: &mut WgRandom, p: P) {
        if self.is_free(p) {
            self.place_log(r, p);
        }
    }

    // -------------------------------------------------------- trunks

    fn place_trunk(&mut self, r: &mut WgRandom, height: i32, o: P) -> Vec<Attach> {
        let t = self.t;
        match &t.trunk.kind {
            TrunkKind::Straight => {
                self.below_trunk(r, above(o, -1));
                for i in 0..height {
                    self.place_log(r, above(o, i));
                }
                vec![attach(above(o, height), 0, false)]
            }
            TrunkKind::Forking => {
                self.below_trunk(r, above(o, -1));
                let mut list = Vec::new();
                let dir = random_dir(r);
                let i = height - r.next_int_bound(4) - 1;
                let mut j = 3 - r.next_int_bound(3);
                let (mut x, mut z) = (o[0], o[2]);
                let mut top = None;
                for k in 0..height {
                    let y = o[1] + k;
                    if k >= i && j > 0 {
                        x += dir[0];
                        z += dir[2];
                        j -= 1;
                    }
                    if self.place_log(r, [x, y, z]) {
                        top = Some(y + 1);
                    }
                }
                if let Some(y) = top {
                    list.push(attach([x, y, z], 1, false));
                }
                x = o[0];
                z = o[2];
                let dir2 = random_dir(r);
                if dir2 != dir {
                    let l = i - r.next_int_bound(2) - 1;
                    let mut m = 1 + r.next_int_bound(3);
                    top = None;
                    let mut n = l;
                    while n < height && m > 0 {
                        if n >= 1 {
                            let y = o[1] + n;
                            x += dir2[0];
                            z += dir2[2];
                            if self.place_log(r, [x, y, z]) {
                                top = Some(y + 1);
                            }
                        }
                        n += 1;
                        m -= 1;
                    }
                    if let Some(y) = top {
                        list.push(attach([x, y, z], 0, false));
                    }
                }
                list
            }
            TrunkKind::Giant | TrunkKind::MegaJungle => {
                let b = above(o, -1);
                self.below_trunk(r, b);
                self.below_trunk(r, add(b, EAST));
                self.below_trunk(r, add(b, SOUTH));
                self.below_trunk(r, add(add(b, SOUTH), EAST));
                for i in 0..height {
                    self.place_log_if_free(r, add(o, [0, i, 0]));
                    if i < height - 1 {
                        self.place_log_if_free(r, add(o, [1, i, 0]));
                        self.place_log_if_free(r, add(o, [1, i, 1]));
                        self.place_log_if_free(r, add(o, [0, i, 1]));
                    }
                }
                let mut list = vec![attach(above(o, height), 0, true)];
                if matches!(t.trunk.kind, TrunkKind::MegaJungle) {
                    let mut i = height - 2 - r.next_int_bound(4);
                    while i > height / 2 {
                        let f = r.next_float() * 6.283_185_5f32;
                        let (mut j, mut l) = (0, 0);
                        for k in 0..5 {
                            j = (1.5f32 + mcos(f as f64) * k as f32) as i32;
                            l = (1.5f32 + msin(f as f64) * k as f32) as i32;
                            self.place_log(r, add(o, [j, i - 3 + k / 2, l]));
                        }
                        list.push(attach(add(o, [j, i, l]), -2, false));
                        i -= 2 + r.next_int_bound(4);
                    }
                }
                list
            }
            TrunkKind::DarkOak => {
                let mut list = Vec::new();
                let b = above(o, -1);
                self.below_trunk(r, b);
                self.below_trunk(r, add(b, EAST));
                self.below_trunk(r, add(b, SOUTH));
                self.below_trunk(r, add(add(b, SOUTH), EAST));
                let dir = random_dir(r);
                let i = height - r.next_int_bound(4);
                let mut j = 2 - r.next_int_bound(3);
                let (x0, y0, z0) = (o[0], o[1], o[2]);
                let (mut x, mut z) = (x0, z0);
                let top = y0 + height - 1;
                for k in 0..height {
                    if k >= i && j > 0 {
                        x += dir[0];
                        z += dir[2];
                        j -= 1;
                    }
                    let p = [x, y0 + k, z];
                    let s = self.get(p);
                    if vstate::air(s) || t.tags.leaves.contains(s) {
                        self.place_log(r, p);
                        self.place_log(r, add(p, EAST));
                        self.place_log(r, add(p, SOUTH));
                        self.place_log(r, add(add(p, EAST), SOUTH));
                    }
                }
                list.push(attach([x, top, z], 0, true));
                for dx in -1..=2 {
                    for dz in -1..=2 {
                        if (0..=1).contains(&dx) && (0..=1).contains(&dz) {
                            continue;
                        }
                        if r.next_int_bound(3) > 0 {
                            continue;
                        }
                        let n = r.next_int_bound(3) + 2;
                        for k in 0..n {
                            self.place_log(r, [x0 + dx, top - k - 1, z0 + dz]);
                        }
                        list.push(attach([x0 + dx, top, z0 + dz], 0, false));
                    }
                }
                list
            }
            TrunkKind::Fancy => self.fancy(r, height, o),
            TrunkKind::Bending { min_leaves, bend } => {
                let dir = random_dir(r);
                let j = height - 1;
                let mut m = o;
                self.below_trunk(r, above(o, -1));
                let mut list = Vec::new();
                for i in 0..=j {
                    if i + 1 >= j + r.next_int_bound(2) {
                        m = add(m, dir);
                    }
                    if self.valid_tree_pos(m) {
                        self.place_log(r, m);
                    }
                    if i >= *min_leaves {
                        list.push(attach(m, 0, false));
                    }
                    m = add(m, UP);
                }
                let k = bend.sample(r);
                for _ in 0..=k {
                    if self.valid_tree_pos(m) {
                        self.place_log(r, m);
                    }
                    list.push(attach(m, 0, false));
                    m = add(m, dir);
                }
                list
            }
            TrunkKind::Upwards { steps, prob, len, .. } => {
                let mut list = Vec::new();
                for i in 0..height {
                    let y = o[1] + i;
                    let m = [o[0], y, o[2]];
                    if self.place_log(r, m) && i < height - 1 && r.next_float() < *prob {
                        let dir = random_dir(r);
                        let l = len.sample(r);
                        let start = (l - len.sample(r) - 1).max(0);
                        let n = steps.sample(r);
                        self.upwards_branch(r, height, &mut list, m, y, dir, start, n);
                    }
                    if i == height - 1 {
                        list.push(attach([o[0], y + 1, o[2]], 0, false));
                    }
                }
                list
            }
            TrunkKind::Cherry { count, hlen, start, end } => {
                self.below_trunk(r, above(o, -1));
                let i = (height - 1 + between(r, start.0, start.1)).max(0);
                let mut j = (height - 1 + between(r, start.0, start.1 - 1)).max(0);
                if j >= i {
                    j += 1;
                }
                let k = count.sample(r);
                let three = k == 3;
                let two = k >= 2;
                let len = if three {
                    height
                } else if two {
                    i.max(j) + 1
                } else {
                    i + 1
                };
                for l in 0..len {
                    self.place_log(r, above(o, l));
                }
                let mut list = Vec::new();
                if three {
                    list.push(attach(above(o, len), 0, false));
                }
                let dir = random_dir(r);
                list.push(self.cherry_branch(r, height, o, hlen, end, dir, i, i < len - 1));
                if two {
                    list.push(self.cherry_branch(r, height, o, hlen, end, neg(dir), j, j < len - 1));
                }
                list
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn upwards_branch(&mut self, r: &mut WgRandom, height: i32, list: &mut Vec<Attach>, m: P, y: i32, dir: P, start: i32, mut steps: i32) {
        let mut j = y + start;
        let (mut k, mut l) = (m[0], m[2]);
        let mut n = start;
        while n < height && steps > 0 {
            if n >= 1 {
                let o = y + n;
                k += dir[0];
                l += dir[2];
                j = o;
                if self.place_log(r, [k, o, l]) {
                    j += 1;
                }
                list.push(attach([k, o, l], 0, false));
            }
            n += 1;
            steps -= 1;
        }
        if j - y > 1 {
            let p = [k, j, l];
            list.push(attach(p, 0, false));
            list.push(attach(above(p, -2), 0, false));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn cherry_branch(&mut self, r: &mut WgRandom, height: i32, o: P, hlen: &IntP, end: &IntP, dir: P, start: i32, extend: bool) -> Attach {
        let axis = if dir[0] != 0 { "x" } else { "z" };
        let mut m = above(o, start);
        let j = height - 1 + end.sample(r);
        let flag = extend || j < start;
        let k = hlen.sample(r) + flag as i32;
        let target = above(rel(o, dir, k), j);
        let l = if flag { 2 } else { 1 };
        for _ in 0..l {
            m = add(m, dir);
            self.place_log_with(r, m, Some(axis));
        }
        let vdir = if target[1] > m[1] { UP } else { DOWN };
        loop {
            let i = manhattan(m, target);
            if i == 0 {
                break;
            }
            let f = (target[1] - m[1]).abs() as f32 / i as f32;
            let b = r.next_float() < f;
            m = add(m, if b { vdir } else { dir });
            self.place_log_with(r, m, if b { None } else { Some(axis) });
        }
        attach(above(target, 1), 0, false)
    }

    fn fancy(&mut self, r: &mut WgRandom, height: i32, o: P) -> Vec<Attach> {
        let j = height + 2;
        let k = (j as f64 * 0.618).floor() as i32;
        self.below_trunk(r, above(o, -1));
        let l = 1.min((1.382 + (j as f64 / 13.0) * (j as f64 / 13.0)).floor() as i32);
        let m = o[1] + k;
        let mut n = j - 5;
        // (attachment pos, branch base)
        let mut coords: Vec<(P, i32)> = vec![(above(o, n), m)];
        while n >= 0 {
            let f = tree_shape(j, n);
            if f >= 0.0 {
                for _ in 0..l {
                    let e = f as f64 * (r.next_float() as f64 + 0.328);
                    let g = (r.next_float() * 2.0) as f64 * std::f64::consts::PI;
                    let h = e * g.sin() + 0.5;
                    let pp = e * g.cos() + 0.5;
                    let pos = add(o, [h.floor() as i32, n - 1, pp.floor() as i32]);
                    let pos2 = above(pos, 5);
                    if self.make_limb(r, pos, pos2, false) {
                        let q = o[0] - pos[0];
                        let rr = o[2] - pos[2];
                        let s = pos[1] as f64 - ((q * q + rr * rr) as f64).sqrt() * 0.381;
                        let t = if s > m as f64 { m } else { s as i32 };
                        let pos3 = [o[0], t, o[2]];
                        if self.make_limb(r, pos3, pos, false) {
                            coords.push((pos, pos3[1]));
                        }
                    }
                }
            }
            n -= 1;
        }
        self.make_limb(r, o, above(o, k), true);
        for &(pos, base) in &coords {
            let b = [o[0], base, o[2]];
            if b != pos && trim_branches(j, base - o[1]) {
                self.make_limb(r, b, pos, true);
            }
        }
        coords.iter().filter(|(_, base)| trim_branches(j, base - o[1])).map(|(pos, _)| attach(*pos, 0, false)).collect()
    }

    fn make_limb(&mut self, r: &mut WgRandom, start: P, end: P, make: bool) -> bool {
        if !make && start == end {
            return true;
        }
        let d = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
        let steps = d[0].abs().max(d[1].abs()).max(d[2].abs());
        let fx = d[0] as f32 / steps as f32;
        let fy = d[1] as f32 / steps as f32;
        let fz = d[2] as f32 / steps as f32;
        for i in 0..=steps {
            let fl = |v: f32| (0.5f32 + i as f32 * v).floor() as i32;
            let p = add(start, [fl(fx), fl(fy), fl(fz)]);
            if make {
                let dx = (p[0] - start[0]).abs();
                let dz = (p[2] - start[2]).abs();
                let mx = dx.max(dz);
                let axis = if mx > 0 {
                    if dx == mx {
                        "x"
                    } else {
                        "z"
                    }
                } else {
                    "y"
                };
                self.place_log_with(r, p, Some(axis));
            } else if !self.is_free(p) {
                return false;
            }
        }
        true
    }

    // -------------------------------------------------------- foliage

    fn try_place_leaf(&mut self, r: &mut WgRandom, p: P) -> bool {
        let reg = BlockRegistry::get();
        let st = self.get(p);
        if reg.property(st, "persistent") == Some("true") || !self.valid_tree_pos(p) {
            return false;
        }
        let Some(mut s) = self.t.foliage_provider.get(self.lv, r, p) else { return false };
        if reg.property(s, "waterlogged").is_some() {
            let w = vstate::fluid(self.get(p)) == 1;
            s = reg.with(s, "waterlogged", if w { "true" } else { "false" }).unwrap_or(s);
        }
        self.set_foliage(p, s);
        true
    }

    fn skip(&self, r: &mut WgRandom, dx: i32, y: i32, dz: i32, range: i32, large: bool) -> bool {
        match &self.t.foliage.kind {
            FoliageKind::Blob(_) => dx == range && dz == range && (r.next_int_bound(2) == 0 || y == 0),
            FoliageKind::Bush(_) => dx == range && dz == range && r.next_int_bound(2) == 0,
            FoliageKind::Fancy(_) => {
                let a = dx as f32 + 0.5;
                let b = dz as f32 + 0.5;
                a * a + b * b > (range * range) as f32
            }
            FoliageKind::Acacia => {
                if y == 0 {
                    (dx > 1 || dz > 1) && dx != 0 && dz != 0
                } else {
                    dx == range && dz == range && range > 0
                }
            }
            FoliageKind::DarkOak => {
                if y == -1 && !large {
                    dx == range && dz == range
                } else if y == 1 {
                    dx + dz > range * 2 - 2
                } else {
                    false
                }
            }
            FoliageKind::Jungle(_) | FoliageKind::MegaPine(_) => dx + dz >= 7 || dx * dx + dz * dz > range * range,
            FoliageKind::Pine(_) | FoliageKind::Spruce(_) => dx == range && dz == range && range > 0,
            FoliageKind::RandomSpread(..) => false,
            FoliageKind::Cherry { wide, corner, .. } => {
                if y == -1 && (dx == range || dz == range) && r.next_float() < *wide {
                    return true;
                }
                let c = dx == range && dz == range;
                if range > 2 {
                    c || (dx + dz > range * 2 - 2 && r.next_float() < *corner)
                } else {
                    c && r.next_float() < *corner
                }
            }
        }
    }

    fn skip_signed(&self, r: &mut WgRandom, dx: i32, y: i32, dz: i32, range: i32, large: bool) -> bool {
        if matches!(self.t.foliage.kind, FoliageKind::DarkOak) && y == 0 && large && (dx == -range || dx >= range) && (dz == -range || dz >= range) {
            return true;
        }
        let (a, b) = if large { (dx.abs().min((dx - 1).abs()), dz.abs().min((dz - 1).abs())) } else { (dx.abs(), dz.abs()) };
        self.skip(r, a, y, b, range, large)
    }

    fn leaves_row(&mut self, r: &mut WgRandom, pos: P, range: i32, y: i32, large: bool) {
        let d = large as i32;
        for dx in -range..=range + d {
            for dz in -range..=range + d {
                if !self.skip_signed(r, dx, y, dz, range, large) {
                    self.try_place_leaf(r, add(pos, [dx, y, dz]));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn leaves_row_hanging(&mut self, r: &mut WgRandom, pos: P, range: i32, y: i32, large: bool, hang: f32, ext: f32) {
        self.leaves_row(r, pos, range, y, large);
        let d = large as i32;
        let below = above(pos, -1);
        for dir in HORIZONTAL {
            let cw = clockwise(dir);
            let off = if cw[0] + cw[2] > 0 { range + d } else { range };
            let mut m = rel(rel(add(pos, [0, y - 1, 0]), cw, off), dir, -range);
            let mut i = -range;
            while i < range + d {
                let set = self.s.foliage.contains(add(m, UP));
                if set && self.try_extension(r, hang, below, m) {
                    self.try_extension(r, ext, below, above(m, -1));
                }
                i += 1;
                m = add(m, dir);
            }
        }
    }

    fn try_extension(&mut self, r: &mut WgRandom, chance: f32, origin: P, p: P) -> bool {
        if manhattan(p, origin) >= 7 || r.next_float() > chance {
            return false;
        }
        self.try_place_leaf(r, p)
    }

    #[allow(clippy::too_many_arguments)]
    fn create_foliage(&mut self, r: &mut WgRandom, trunk_h: i32, a: &Attach, fh: i32, fr: i32) {
        let offset = self.t.foliage.offset.sample(r);
        let large = a.double;
        let pos = a.pos;
        match &self.t.foliage.kind {
            FoliageKind::Blob(_) => {
                let mut y = offset;
                while y >= offset - fh {
                    let rad = (fr + a.radius_offset - 1 - y / 2).max(0);
                    self.leaves_row(r, pos, rad, y, large);
                    y -= 1;
                }
            }
            FoliageKind::Bush(_) => {
                let mut y = offset;
                while y >= offset - fh {
                    let rad = fr + a.radius_offset - 1 - y;
                    self.leaves_row(r, pos, rad, y, large);
                    y -= 1;
                }
            }
            FoliageKind::Fancy(_) => {
                let mut y = offset;
                while y >= offset - fh {
                    let rad = fr + if y == offset || y == offset - fh { 0 } else { 1 };
                    self.leaves_row(r, pos, rad, y, large);
                    y -= 1;
                }
            }
            FoliageKind::Acacia => {
                let p = above(pos, offset);
                self.leaves_row(r, p, fr + a.radius_offset, -1 - fh, large);
                self.leaves_row(r, p, fr - 1, -fh, large);
                self.leaves_row(r, p, fr + a.radius_offset - 1, 0, large);
            }
            FoliageKind::DarkOak => {
                let p = above(pos, offset);
                if large {
                    self.leaves_row(r, p, fr + 2, -1, large);
                    self.leaves_row(r, p, fr + 3, 0, large);
                    self.leaves_row(r, p, fr + 2, 1, large);
                    if r.next_bool() {
                        self.leaves_row(r, p, fr, 2, large);
                    }
                } else {
                    self.leaves_row(r, p, fr + 2, -1, large);
                    self.leaves_row(r, p, fr + 1, 0, large);
                }
            }
            FoliageKind::Jungle(_) => {
                let h = if large { fh } else { 1 + r.next_int_bound(2) };
                let mut y = offset;
                while y >= offset - h {
                    let rad = fr + a.radius_offset + 1 - y;
                    self.leaves_row(r, pos, rad, y, large);
                    y -= 1;
                }
            }
            FoliageKind::MegaPine(_) => {
                let mut prev = 0;
                let mut y = pos[1] - fh + offset;
                while y <= pos[1] + offset {
                    let k = pos[1] - y;
                    let l = fr + a.radius_offset + (k as f32 / fh as f32 * 3.5f32).floor() as i32;
                    let m = if k > 0 && l == prev && (y & 1) == 0 { l + 1 } else { l };
                    self.leaves_row(r, [pos[0], y, pos[2]], m, 0, large);
                    prev = l;
                    y += 1;
                }
            }
            FoliageKind::Pine(_) => {
                let mut i = 0;
                let mut y = offset;
                while y >= offset - fh {
                    self.leaves_row(r, pos, i, y, large);
                    if i >= 1 && y == offset - fh + 1 {
                        i -= 1;
                    } else if i < fr + a.radius_offset {
                        i += 1;
                    }
                    y -= 1;
                }
            }
            FoliageKind::Spruce(_) => {
                let mut i = r.next_int_bound(2);
                let mut j = 1;
                let mut k = 0;
                let mut y = offset;
                while y >= -fh {
                    self.leaves_row(r, pos, i, y, large);
                    if i >= j {
                        i = k;
                        k = 1;
                        j = (j + 1).min(fr + a.radius_offset);
                    } else {
                        i += 1;
                    }
                    y -= 1;
                }
            }
            FoliageKind::RandomSpread(_, attempts) => {
                for _ in 0..*attempts {
                    let x = r.next_int_bound(fr) - r.next_int_bound(fr);
                    let y = r.next_int_bound(fh) - r.next_int_bound(fh);
                    let z = r.next_int_bound(fr) - r.next_int_bound(fr);
                    self.try_place_leaf(r, add(pos, [x, y, z]));
                }
            }
            FoliageKind::Cherry { hang, ext, .. } => {
                let (hang, ext) = (*hang, *ext);
                let p = above(pos, offset);
                let rad = fr + a.radius_offset - 1;
                self.leaves_row(r, p, rad - 2, fh - 3, large);
                self.leaves_row(r, p, rad - 1, fh - 4, large);
                let mut y = fh - 5;
                while y >= 0 {
                    self.leaves_row(r, p, rad, y, large);
                    y -= 1;
                }
                self.leaves_row_hanging(r, p, rad, -1, large, hang, ext);
                self.leaves_row_hanging(r, p, rad - 1, -2, large, hang, ext);
            }
        }
        let _ = trunk_h;
    }

    fn foliage_height(&self, r: &mut WgRandom, trunk_h: i32) -> i32 {
        match &self.t.foliage.kind {
            FoliageKind::Blob(h) | FoliageKind::Bush(h) | FoliageKind::Fancy(h) | FoliageKind::Jungle(h) => *h,
            FoliageKind::Acacia => 0,
            FoliageKind::DarkOak => 4,
            FoliageKind::MegaPine(p) | FoliageKind::Pine(p) | FoliageKind::RandomSpread(p, _) => p.sample(r),
            FoliageKind::Cherry { height, .. } => height.sample(r),
            FoliageKind::Spruce(t) => 4.max(trunk_h - t.sample(r)),
        }
    }

    fn foliage_radius(&self, r: &mut WgRandom, i: i32) -> i32 {
        let base = self.t.foliage.radius.sample(r);
        match self.t.foliage.kind {
            FoliageKind::Pine(_) => base + r.next_int_bound((i + 1).max(1)),
            _ => base,
        }
    }

    // -------------------------------------------------------- roots

    fn can_place_root(&self, rt: &Roots, p: P) -> bool {
        self.valid_tree_pos(p) || rt.through.contains(self.get(p))
    }

    fn waterlogged(&self, p: P, s: BlockState) -> BlockState {
        let reg = BlockRegistry::get();
        if reg.property(s, "waterlogged").is_some() {
            let w = vstate::is_water(self.get(p));
            return reg.with(s, "waterlogged", if w { "true" } else { "false" }).unwrap_or(s);
        }
        s
    }

    fn place_root(&mut self, rt: &Roots, r: &mut WgRandom, p: P) {
        if rt.muddy_in.contains(self.get(p)) {
            if let Some(s) = rt.muddy.get(self.lv, r, p) {
                let s = self.waterlogged(p, s);
                self.set_root(p, s);
            }
            return;
        }
        if !self.can_place_root(rt, p) {
            return;
        }
        if let Some(s) = rt.provider.get(self.lv, r, p) {
            let s = self.waterlogged(p, s);
            self.set_root(p, s);
        }
        if let Some((chance, prov)) = &rt.above {
            let up = above(p, 1);
            if r.next_float() < *chance && self.is_air(up) {
                if let Some(s) = prov.get(self.lv, r, up) {
                    let s = self.waterlogged(up, s);
                    self.set_root(up, s);
                }
            }
        }
    }

    fn place_roots(&mut self, rt: &Roots, r: &mut WgRandom, pos: P, trunk: P) -> bool {
        let mut list = Vec::new();
        let mut m = pos;
        while m[1] < trunk[1] {
            if !self.can_place_root(rt, m) {
                return false;
            }
            m = add(m, UP);
        }
        list.push(above(trunk, -1));
        for dir in HORIZONTAL {
            let p = add(trunk, dir);
            let mut l2 = Vec::new();
            if !self.simulate_roots(rt, r, p, dir, trunk, &mut l2, 0) {
                return false;
            }
            list.extend(l2);
            list.push(add(trunk, dir));
        }
        for p in list {
            self.place_root(rt, r, p);
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn simulate_roots(&self, rt: &Roots, r: &mut WgRandom, pos: P, dir: P, origin: P, list: &mut Vec<P>, depth: i32) -> bool {
        if depth == rt.max_len || list.len() as i32 > rt.max_len {
            return false;
        }
        for p in potential_roots(rt, pos, dir, r, origin) {
            if self.can_place_root(rt, p) {
                list.push(p);
                if !self.simulate_roots(rt, r, p, dir, origin, list, depth + 1) {
                    return false;
                }
            }
        }
        true
    }

    // -------------------------------------------------------- whole tree

    fn max_free_height(&self, height: i32, o: P) -> i32 {
        for y in 0..=height + 1 {
            let s = self.t.size.at(height, y);
            for dx in -s..=s {
                for dz in -s..=s {
                    let p = add(o, [dx, y, dz]);
                    if !self.is_free(p) || (!self.t.ignore_vines && self.get(p).name() == "vine") {
                        return y - 2;
                    }
                }
            }
        }
        height
    }

    fn do_place(&mut self, r: &mut WgRandom, o: P) -> bool {
        let t = self.t;
        let height = t.trunk.base + r.next_int_bound(t.trunk.a + 1) + r.next_int_bound(t.trunk.b + 1);
        let fh = self.foliage_height(r, height);
        let fr = self.foliage_radius(r, height - fh);
        let trunk_o = match &t.roots {
            Some(rt) => above(o, rt.offset.sample(r)),
            None => o,
        };
        let lo = o[1].min(trunk_o[1]);
        let hi = o[1].max(trunk_o[1]) + height + 1;
        if lo < MIN_Y + 1 || hi > MIN_Y + HEIGHT {
            return false;
        }
        let free = self.max_free_height(height, trunk_o);
        if free < height && t.min_clipped.is_none_or(|m| free < m) {
            return false;
        }
        if let Some(rt) = &t.roots {
            if !self.place_roots(rt, r, o, trunk_o) {
                return false;
            }
        }
        let list = self.place_trunk(r, free, trunk_o);
        for a in &list {
            self.create_foliage(r, free, a, fh, fr);
        }
        true
    }
}

fn tree_shape(height: i32, y: i32) -> f32 {
    if (y as f32) < height as f32 * 0.3f32 {
        return -1.0;
    }
    let f = height as f32 / 2.0;
    let g = f - y as f32;
    let mut h = (f * f - g * g).sqrt();
    if g == 0.0 {
        h = f;
    } else if g.abs() >= f {
        return 0.0;
    }
    h * 0.5
}

fn trim_branches(height: i32, local_y: i32) -> bool {
    local_y as f64 >= height as f64 * 0.2
}

fn potential_roots(rt: &Roots, pos: P, dir: P, r: &mut WgRandom, origin: P) -> Vec<P> {
    let below = above(pos, -1);
    let side = add(pos, dir);
    let i = manhattan(pos, origin);
    let w = rt.max_width;
    if i > w - 3 && i <= w {
        return if r.next_float() < rt.skew { vec![below, above(side, -1)] } else { vec![below] };
    }
    if i > w {
        return vec![below];
    }
    if r.next_float() < rt.skew {
        return vec![below];
    }
    if r.next_bool() {
        vec![side]
    } else {
        vec![below]
    }
}

/// `Util.shuffle`
pub fn shuffle<T>(v: &mut [T], r: &mut WgRandom) {
    let mut i = v.len();
    while i > 1 {
        let j = r.next_int_bound(i as i32) as usize;
        v.swap(i - 1, j);
        i -= 1;
    }
}

/// `TreeFeature.getLowestTrunkOrRootOfTree`
fn lowest(logs: &[P], roots: &[P]) -> Vec<P> {
    let mut out = Vec::new();
    if roots.is_empty() {
        out.extend_from_slice(logs);
    } else if !logs.is_empty() && roots[0][1] == logs[0][1] {
        out.extend_from_slice(logs);
        out.extend_from_slice(roots);
    } else {
        out.extend_from_slice(roots);
    }
    out
}

impl Tree {
    /// `TreeFeature.place`
    pub fn place(&self, lv: &mut Region, r: &mut WgRandom, origin: P) -> bool {
        let mut g = G { t: self, lv, s: Sets { roots: JSet::default(), trunk: JSet::default(), foliage: JSet::default(), decos: JSet::default() } };
        if !g.do_place(r, origin) || (g.s.trunk.is_empty() && g.s.foliage.is_empty()) {
            return false;
        }
        if !self.decorators.is_empty() {
            let roots = g.s.roots.sorted_by_y();
            let logs = g.s.trunk.sorted_by_y();
            let leaves = g.s.foliage.sorted_by_y();
            for d in &self.decorators {
                g.decorate(d, r, &logs, &leaves, &roots);
            }
        }
        // BoundingBox.encapsulatingPositions
        let mut lo = [i32::MAX; 3];
        let mut hi = [i32::MIN; 3];
        for p in g.s.roots.iter().chain(g.s.trunk.iter()).chain(g.s.foliage.iter()).chain(g.s.decos.iter()) {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        if lo[0] > hi[0] {
            return false;
        }
        if cfg!(test) {
            if let Ok(w) = std::env::var("RIME_TREE_AT") {
                let v: Vec<i32> = w.split(',').map(|x| x.parse().unwrap()).collect();
                if g.s.trunk.contains([v[0], v[1], v[2]]) {
                    let f = |s: &JSet| s.iter().map(|p| format!("{},{},{}", p[0], p[1], p[2])).collect::<Vec<_>>().join(";");
                    eprintln!("TREE trunk {}", f(&g.s.trunk));
                    eprintln!("TREE leaves {}", f(&g.s.foliage));
                    eprintln!("TREE other {}", g.s.decos.iter().chain(g.s.roots.iter()).map(|p| format!("{},{},{}", p[0], p[1], p[2])).collect::<Vec<_>>().join(";"));
                    eprintln!("TREE box {},{},{};{},{},{}", lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]);
                }
            }
        }
        let shape = g.update_leaves(lo, hi);
        g.update_shape_at_edge(&shape, lo, hi);
        true
    }
}

struct Shape {
    lo: P,
    size: [i32; 3],
    bits: Vec<bool>,
}

impl Shape {
    fn idx(&self, p: P) -> Option<usize> {
        let x = p[0] - self.lo[0];
        let y = p[1] - self.lo[1];
        let z = p[2] - self.lo[2];
        if x < 0 || y < 0 || z < 0 || x >= self.size[0] || y >= self.size[1] || z >= self.size[2] {
            return None;
        }
        Some(((x * self.size[1] + y) * self.size[2] + z) as usize)
    }
    fn full(&self, p: P) -> bool {
        self.idx(p).is_some_and(|i| self.bits[i])
    }
    fn fill(&mut self, p: P) {
        if let Some(i) = self.idx(p) {
            self.bits[i] = true;
        }
    }
}

impl G<'_, '_, '_> {
    fn optional_distance(&self, s: BlockState) -> Option<i32> {
        if self.t.tags.no_decay.contains(s) {
            return Some(0);
        }
        if self.t.tags.leaves.contains(s) {
            return BlockRegistry::get().property(s, "distance").and_then(|d| d.parse().ok());
        }
        None
    }

    /// `TreeFeature.updateLeaves`
    fn update_leaves(&mut self, lo: P, hi: P) -> Shape {
        let size = [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1];
        let mut shape = Shape { lo, size, bits: vec![false; (size[0] * size[1] * size[2]) as usize] };
        let inside = |p: P| (0..3).all(|a| p[a] >= lo[a] && p[a] <= hi[a]);
        for p in self.s.decos.iter().chain(self.s.roots.iter()) {
            shape.fill(p);
        }
        let mut levels: Vec<JSet> = vec![JSet::default(); 7];
        for p in self.s.trunk.iter() {
            levels[0].add(p);
        }
        let reg = BlockRegistry::get();
        let mut i = 0usize;
        loop {
            while i < 7 && levels[i].is_empty() {
                i += 1;
            }
            if i >= 7 {
                break;
            }
            let p = levels[i].pop_first().expect("non-empty");
            if !inside(p) {
                continue;
            }
            if i != 0 {
                let s = self.get(p);
                if let Some(n) = reg.with(s, "distance", &i.to_string()) {
                    self.lv.set(p, n);
                }
            }
            shape.fill(p);
            for d in ALL {
                let q = add(p, d);
                if !inside(q) || shape.full(q) {
                    continue;
                }
                let Some(od) = self.optional_distance(self.get(q)) else { continue };
                let k = (od as usize).min(i + 1);
                if k < 7 {
                    levels[k].add(q);
                    i = i.min(k);
                }
            }
        }
        shape
    }

    /// `StructureTemplate.updateShapeAtEdge`: blocks next to the tree react
    /// to it (double plants lose their other half, plants lose support).
    fn update_shape_at_edge(&mut self, shape: &Shape, lo: P, hi: P) {
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    let p = [x, y, z];
                    if !shape.full(p) {
                        continue;
                    }
                    for d in ALL {
                        let q = add(p, d);
                        if shape.full(q) {
                            continue;
                        }
                        let a = self.get(p);
                        let b = self.get(q);
                        let a2 = update_shape(self.lv, a, p, d, b);
                        if a2 != a {
                            self.lv.set(p, a2);
                        }
                        let b2 = update_shape(self.lv, b, q, neg(d), a2);
                        if b2 != b {
                            self.lv.set(q, b2);
                        }
                    }
                }
            }
        }
    }
}

/// `BlockState.updateShape` for the blocks that react during world
/// generation.
fn update_shape(lv: &Region, s: BlockState, p: P, dir: P, neighbour: BlockState) -> BlockState {
    use super::survive::{can_survive, kind, Kind};
    let air = crate::registry::blocks::AIR;
    match kind(s) {
        Kind::DoublePlant | Kind::TallSeagrass => {
            let reg = BlockRegistry::get();
            let lower = reg.property(s, "half") == Some("lower");
            if dir[1] == 0 || lower != (dir == UP) || (neighbour.name() == s.name() && reg.property(neighbour, "half") != reg.property(s, "half")) {
                if lower && dir == DOWN && !can_survive(lv, s, p) {
                    return air;
                }
                s
            } else {
                air
            }
        }
        Kind::Vegetation | Kind::DryVegetation | Kind::Azalea | Kind::Mushroom | Kind::Seagrass | Kind::LilyPad | Kind::CactusFlower | Kind::LeafLitter | Kind::SnowLayer => {
            if can_survive(lv, s, p) {
                s
            } else {
                air
            }
        }
        _ => s,
    }
}

// ------------------------------------------------------------ decorators

impl G<'_, '_, '_> {
    fn place_vine(&mut self, p: P, face: P) {
        let reg = BlockRegistry::get();
        let v = reg.state("minecraft:vine", &[]).expect("vine");
        let v = reg.with(v, dir_name(face), "true").unwrap_or(v);
        self.set_deco(p, v);
    }

    fn decorate(&mut self, d: &Deco, r: &mut WgRandom, logs: &[P], leaves: &[P], roots: &[P]) {
        let reg = BlockRegistry::get();
        match d {
            Deco::Beehive(prob) => {
                if logs.is_empty() || r.next_float() >= *prob {
                    return;
                }
                let first = logs[0][1];
                let i = if !leaves.is_empty() { (leaves[0][1] - 1).max(first + 1) } else { (first + 1 + r.next_int_bound(3)).min(logs[logs.len() - 1][1]) };
                let mut list: Vec<P> = Vec::new();
                for &l in logs.iter().filter(|p| p[1] == i) {
                    for dir in [EAST, SOUTH, WEST] {
                        list.push(add(l, dir));
                    }
                }
                if list.is_empty() {
                    return;
                }
                shuffle(&mut list, r);
                let Some(&p) = list.iter().find(|&&p| self.is_air(p) && self.is_air(add(p, SOUTH))) else { return };
                let nest = reg.state("minecraft:bee_nest", &[("facing", "south")]).expect("bee_nest");
                self.s.decos.add(p);
                if self.lv.set(p, nest) {
                    // BeehiveBlockEntity: 2-3 bees with random ticks in the hive
                    let n = 2 + r.next_int_bound(2);
                    for _ in 0..n {
                        r.next_int_bound(599);
                    }
                }
            }
            Deco::Cocoa(prob) => {
                if r.next_float() >= *prob || logs.is_empty() {
                    return;
                }
                let i = logs[0][1];
                for &l in logs.iter().filter(|p| p[1] - i <= 2) {
                    for dir in HORIZONTAL {
                        if r.next_float() <= 0.25 {
                            let o = neg(dir);
                            let p = add(l, [o[0], 0, o[2]]);
                            if self.is_air(p) {
                                let age = r.next_int_bound(3).to_string();
                                let s = reg.state("minecraft:cocoa", &[("age", &age), ("facing", dir_name(dir))]).expect("cocoa");
                                self.set_deco(p, s);
                            }
                        }
                    }
                }
            }
            Deco::TrunkVine => {
                for &l in logs {
                    for (dir, face) in [(WEST, EAST), (EAST, WEST), (NORTH, SOUTH), (SOUTH, NORTH)] {
                        if r.next_int_bound(3) > 0 {
                            let p = add(l, dir);
                            if self.is_air(p) {
                                self.place_vine(p, face);
                            }
                        }
                    }
                }
            }
            Deco::LeaveVine(prob) => {
                for &l in leaves {
                    for (dir, face) in [(WEST, EAST), (EAST, WEST), (NORTH, SOUTH), (SOUTH, NORTH)] {
                        if r.next_float() < *prob {
                            let p = add(l, dir);
                            if self.is_air(p) {
                                self.place_vine(p, face);
                                let mut q = above(p, -1);
                                let mut n = 4;
                                while self.is_air(q) && n > 0 {
                                    self.place_vine(q, face);
                                    q = above(q, -1);
                                    n -= 1;
                                }
                            }
                        }
                    }
                }
            }
            Deco::AlterGround(prov) => {
                let list = lowest(logs, roots);
                if list.is_empty() {
                    return;
                }
                let y = list[0][1];
                for &p in list.iter().filter(|p| p[1] == y) {
                    self.alter_circle(prov, r, add(p, [-1, 0, -1]));
                    self.alter_circle(prov, r, add(p, [2, 0, -1]));
                    self.alter_circle(prov, r, add(p, [-1, 0, 2]));
                    self.alter_circle(prov, r, add(p, [2, 0, 2]));
                    for _ in 0..5 {
                        let i = r.next_int_bound(64);
                        let (j, k) = (i % 8, i / 8);
                        if j == 0 || j == 7 || k == 0 || k == 7 {
                            self.alter_circle(prov, r, add(p, [-3 + j, 0, -3 + k]));
                        }
                    }
                }
            }
            Deco::AttachedToLeaves { prob, xz, y, provider, empty, dirs } => {
                let mut excluded = std::collections::HashSet::new();
                let mut shuffled = leaves.to_vec();
                shuffle(&mut shuffled, r);
                for l in shuffled {
                    let dir = dirs[r.next_int_bound(dirs.len() as i32) as usize];
                    let p = add(l, dir);
                    if excluded.contains(&p) {
                        continue;
                    }
                    if r.next_float() < *prob && (1..=*empty).all(|i| self.is_air(rel(l, dir, i))) {
                        for ex in p[0] - xz..=p[0] + xz {
                            for ey in p[1] - y..=p[1] + y {
                                for ez in p[2] - xz..=p[2] + xz {
                                    excluded.insert([ex, ey, ez]);
                                }
                            }
                        }
                        if let Some(s) = provider.get(self.lv, r, p) {
                            self.set_deco(p, s);
                        }
                    }
                }
            }
            Deco::PlaceOnGround { tries, radius, height, provider } => {
                let list = lowest(logs, roots);
                if list.is_empty() {
                    return;
                }
                let y = list[0][1];
                let (mut x0, mut x1, mut z0, mut z1) = (list[0][0], list[0][0], list[0][2], list[0][2]);
                for p in list.iter().filter(|p| p[1] == y) {
                    x0 = x0.min(p[0]);
                    x1 = x1.max(p[0]);
                    z0 = z0.min(p[2]);
                    z1 = z1.max(p[2]);
                }
                let (bx0, by0, bz0, bx1, by1, bz1) = (x0 - radius, y - height, z0 - radius, x1 + radius, y + height, z1 + radius);
                for _ in 0..*tries {
                    let p = [between(r, bx0, bx1), between(r, by0, by1), between(r, bz0, bz1)];
                    let up = above(p, 1);
                    let a = self.get(up);
                    if (vstate::air(a) || a.name() == "vine") && vstate::solid_render(self.get(p)) && self.lv.height(Hm::NoLeaves, p[0], p[2]) <= up[1] {
                        if let Some(s) = provider.get(self.lv, r, up) {
                            self.set_deco(up, s);
                        }
                    }
                }
            }
            Deco::CreakingHeart(prob) => {
                if logs.is_empty() || r.next_float() >= *prob {
                    return;
                }
                let mut list = logs.to_vec();
                shuffle(&mut list, r);
                let logs_tag = &self.t.tags.logs;
                if let Some(&p) = list.iter().find(|&&p| ALL.iter().all(|d| logs_tag.contains(self.get(add(p, *d))))) {
                    let s = reg.state("minecraft:creaking_heart", &[("creaking_heart_state", "dormant"), ("natural", "true")]).or_else(|| reg.state("minecraft:creaking_heart", &[])).expect("creaking_heart");
                    self.set_deco(p, s);
                }
            }
            Deco::PaleMoss { leaves: lp, trunk: tp, ground, patch } => {
                let mut list = logs.to_vec();
                shuffle(&mut list, r);
                if list.is_empty() {
                    return;
                }
                let mut min = list[0];
                for p in &list {
                    if p[1] < min[1] {
                        min = *p;
                    }
                }
                if r.next_float() < *ground {
                    patch.place(self.lv, r, above(min, 1));
                }
                for &l in logs {
                    if r.next_float() < *tp {
                        let b = above(l, -1);
                        if self.is_air(b) {
                            self.moss_hanger(r, b);
                        }
                    }
                }
                for &l in leaves {
                    if r.next_float() < *lp {
                        let b = above(l, -1);
                        if self.is_air(b) {
                            self.moss_hanger(r, b);
                        }
                    }
                }
            }
        }
    }

    fn moss_hanger(&mut self, r: &mut WgRandom, mut p: P) {
        let reg = BlockRegistry::get();
        let moss = |tip: &str| reg.state("minecraft:pale_hanging_moss", &[("tip", tip)]).expect("pale_hanging_moss");
        while self.is_air(above(p, -1)) && (r.next_float() as f64) >= 0.5 {
            self.set_deco(p, moss("false"));
            p = above(p, -1);
        }
        self.set_deco(p, moss("true"));
    }

    fn alter_circle(&mut self, prov: &StateP, r: &mut WgRandom, c: P) {
        for dx in -2i32..=2 {
            for dz in -2i32..=2 {
                if dx.abs() == 2 && dz.abs() == 2 {
                    continue;
                }
                let p = add(c, [dx, 0, dz]);
                let mut dy = 2;
                while dy >= -3 {
                    let q = above(p, dy);
                    if let Some(s) = prov.get(self.lv, r, q) {
                        self.set_deco(q, s);
                        break;
                    }
                    if !self.is_air(q) && dy < 0 {
                        break;
                    }
                    dy -= 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RIME_JH = output of a java.util.HashSet probe (insertions, iteration, pops)
    #[test]
    fn jset_matches_java() {
        let Ok(f) = std::env::var("RIME_JH") else { return };
        let txt = std::fs::read_to_string(f).unwrap();
        let lines: Vec<Vec<P>> = txt
            .lines()
            .map(|l| {
                l.split(';')
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        let v: Vec<i32> = s.split(',').map(|x| x.parse().unwrap()).collect();
                        [v[0], v[1], v[2]]
                    })
                    .collect()
            })
            .collect();
        let mut s = JSet::default();
        for &p in &lines[0] {
            s.add(p);
        }
        assert_eq!(s.iter().collect::<Vec<_>>(), lines[1]);
        let mut pops = Vec::new();
        for _ in 0..lines[2].len() {
            pops.push(s.pop_first().unwrap());
        }
        assert_eq!(pops, lines[2]);
    }
}
