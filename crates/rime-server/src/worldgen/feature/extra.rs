//! The remaining overworld feature types (ported from the 26.1 bytecode):
//! lakes, springs, snow and ice, ocean plants, vines, dungeons, huge
//! mushrooms, bamboo, ice spikes and more (see `Extra`).

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::place::{block_set, parse_state, BlockSet, IntP, Pred, StateP};
use super::{Builder, Hm, Region};
use crate::registry::{blocks, vstate, BlockRegistry, BlockState};
use crate::worldgen::chunk::{HEIGHT, MIN_Y};
use crate::worldgen::random::{Rng, WgRandom};

pub type P = [i32; 3];

pub const DOWN: P = [0, -1, 0];
pub const UP: P = [0, 1, 0];
pub const NORTH: P = [0, 0, -1];
pub const SOUTH: P = [0, 0, 1];
pub const WEST: P = [-1, 0, 0];
pub const EAST: P = [1, 0, 0];
/// `Direction.values()`
pub const ALL: [P; 6] = [DOWN, UP, NORTH, SOUTH, WEST, EAST];
/// `Direction.Plane.HORIZONTAL`
pub const HORIZONTAL: [P; 4] = [NORTH, EAST, SOUTH, WEST];
pub const MAX_Y: i32 = MIN_Y + HEIGHT - 1;

#[inline]
pub fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
pub fn up(a: P, k: i32) -> P {
    [a[0], a[1] + k, a[2]]
}
pub fn neg(d: P) -> P {
    [-d[0], -d[1], -d[2]]
}
pub fn dir_index(d: P) -> u8 {
    ALL.iter().position(|x| *x == d).unwrap_or(0) as u8
}
pub fn face_name(d: P) -> &'static str {
    match d {
        DOWN => "down",
        UP => "up",
        NORTH => "north",
        SOUTH => "south",
        WEST => "west",
        _ => "east",
    }
}

pub fn st(name: &str) -> BlockState {
    BlockRegistry::get().state(name, &[]).unwrap_or_else(|| panic!("block {name}"))
}
pub fn with(s: BlockState, k: &str, v: &str) -> BlockState {
    BlockRegistry::get().with(s, k, v).unwrap_or(s)
}
pub fn prop(s: BlockState, k: &str) -> Option<&'static str> {
    BlockRegistry::get().property(s, k)
}
pub fn is(s: BlockState, name: &str) -> bool {
    s.name() == name
}
pub fn is_water_block(s: BlockState) -> bool {
    s.name() == "water"
}

/// Tags used by these features.
pub struct Tags {
    pub features_cannot_replace: BlockSet,
    pub lava_pool_stone_cannot_replace: BlockSet,
    pub replaceable_by_mushrooms: BlockSet,
    pub bamboo_plantable_on: BlockSet,
    pub beneath_bamboo_podzol_replaceable: BlockSet,
    pub leaves: BlockSet,
    pub base_stone_overworld: BlockSet,
    pub dripstone_replaceable: BlockSet,
    pub corals: BlockSet,
    pub wall_corals: BlockSet,
    pub coral_blocks: BlockSet,
    pub geode_invalid: BlockSet,
    pub sculk_replaceable: BlockSet,
    pub sculk_replaceable_world_gen: BlockSet,
    pub fire: BlockSet,
    pub logs: BlockSet,
    pub dirt: BlockSet,
    pub replaceable_by_trees: BlockSet,
}

impl Tags {
    pub fn load(b: &mut Builder) -> Result<Tags> {
        Ok(Tags {
            features_cannot_replace: b.tag("features_cannot_replace")?,
            lava_pool_stone_cannot_replace: b.tag("lava_pool_stone_cannot_replace")?,
            replaceable_by_mushrooms: b.tag("replaceable_by_mushrooms")?,
            bamboo_plantable_on: b.tag("supports_bamboo")?,
            beneath_bamboo_podzol_replaceable: b.tag("beneath_bamboo_podzol_replaceable")?,
            leaves: b.tag("leaves")?,
            base_stone_overworld: b.tag("base_stone_overworld")?,
            dripstone_replaceable: b.tag("dripstone_replaceable_blocks")?,
            corals: b.tag("corals")?,
            wall_corals: b.tag("wall_corals")?,
            coral_blocks: b.tag("coral_blocks")?,
            geode_invalid: b.tag("geode_invalid_blocks")?,
            sculk_replaceable: b.tag("sculk_replaceable")?,
            sculk_replaceable_world_gen: b.tag("sculk_replaceable_world_gen")?,
            fire: b.tag("fire")?,
            logs: b.tag("logs")?,
            dirt: b.tag("dirt")?,
            replaceable_by_trees: b.tag("replaceable_by_trees")?,
        })
    }
}

pub enum Extra {
    Freeze,
    Lake { fluid: StateP, barrier: StateP },
    Spring { state: BlockState, below: bool, rock: i32, hole: i32, valid: BlockSet },
    Seagrass(f32),
    Kelp,
    SeaPickle(IntP),
    Vines,
    BlueIce,
    UnderwaterMagma { range: i32, prob: f32, radius: i32 },
    MonsterRoom,
    HugeMushroom { red: bool, cap: StateP, stem: StateP, radius: i32, on: Pred },
    Bamboo(f32),
    Spike { state: BlockState, on: Pred, replace: Pred },
    Other(Box<super::extra2::Extra2>),
}

fn f32v(v: &Value, k: &str) -> f32 {
    v[k].as_f64().unwrap_or(0.0) as f32
}
fn i32v(v: &Value, k: &str, d: i32) -> i32 {
    v[k].as_i64().map(|x| x as i32).unwrap_or(d)
}

impl Extra {
    /// None: not one of these types.
    pub fn parse(b: &mut Builder, ty: &str, c: &Value) -> Option<Result<Extra>> {
        let r = (|| -> Result<Option<Extra>> {
            Ok(Some(match ty {
                "freeze_top_layer" => Extra::Freeze,
                "lake" => Extra::Lake { fluid: StateP::parse(b, &c["fluid"])?, barrier: StateP::parse(b, &c["barrier"])? },
                "spring_feature" => {
                    let name = c["state"]["Name"].as_str().unwrap_or("minecraft:water");
                    Extra::Spring { state: BlockRegistry::get().state(name, &[("level", "0")]).ok_or_else(|| anyhow!("spring {name}"))?, below: c["requires_block_below"].as_bool().unwrap_or(true), rock: i32v(c, "rock_count", 4), hole: i32v(c, "hole_count", 1), valid: block_set(b, &c["valid_blocks"])? }
                }
                "seagrass" => Extra::Seagrass(f32v(c, "probability")),
                "kelp" => Extra::Kelp,
                "sea_pickle" => Extra::SeaPickle(IntP::parse(&c["count"])?),
                "vines" => Extra::Vines,
                "blue_ice" => Extra::BlueIce,
                "underwater_magma" => Extra::UnderwaterMagma { range: i32v(c, "floor_search_range", 5), prob: f32v(c, "placement_probability_per_valid_position"), radius: i32v(c, "placement_radius_around_floor", 1) },
                "monster_room" => Extra::MonsterRoom,
                "huge_red_mushroom" | "huge_brown_mushroom" => Extra::HugeMushroom {
                    red: ty == "huge_red_mushroom",
                    cap: StateP::parse(b, &c["cap_provider"])?,
                    stem: StateP::parse(b, &c["stem_provider"])?,
                    radius: i32v(c, "foliage_radius", 2),
                    on: if c["can_place_on"].is_object() { Pred::parse(b, &c["can_place_on"])? } else { Pred::True },
                },
                "bamboo" => Extra::Bamboo(f32v(c, "probability")),
                "spike" => Extra::Spike { state: parse_state(&c["state"])?, on: Pred::parse(b, &c["can_place_on"])?, replace: Pred::parse(b, &c["can_replace"])? },
                _ => match super::extra2::Extra2::parse(b, ty, c) {
                    Some(r) => Extra::Other(Box::new(r?)),
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
            Extra::Freeze => freeze(lv, o),
            Extra::Lake { fluid, barrier } => lake(lv, r, o, fluid, barrier),
            Extra::Spring { state, below, rock, hole, valid } => spring(lv, o, *state, *below, *rock, *hole, valid),
            Extra::Seagrass(p) => seagrass(lv, r, o, *p),
            Extra::Kelp => kelp(lv, r, o),
            Extra::SeaPickle(n) => sea_pickle(lv, r, o, n),
            Extra::Vines => vines(lv, o),
            Extra::BlueIce => blue_ice(lv, r, o),
            Extra::UnderwaterMagma { range, prob, radius } => underwater_magma(lv, r, o, *range, *prob, *radius),
            Extra::MonsterRoom => monster_room(lv, r, o),
            Extra::HugeMushroom { red, cap, stem, radius, on } => huge_mushroom(lv, r, o, *red, cap, stem, *radius, on),
            Extra::Bamboo(p) => bamboo(lv, r, o, *p),
            Extra::Spike { state, on, replace } => spike(lv, r, o, *state, on, replace),
            Extra::Other(e) => e.place(lv, r, o),
        }
    }
}

// ------------------------------------------------------------ climate

/// `Biome.getTemperature(pos, seaLevel)` of the biome `biome`.
pub fn temperature(lv: &Region, biome: u8, p: P) -> f32 {
    let lib = lv.lib;
    match lib.climates.get(biome as usize) {
        Some(c) => lib.temps.temperature(*c, p[0], p[1], p[2], lib.sea_level),
        None => 0.8,
    }
}

/// `Biome.shouldFreeze(level, pos, false)` (no block light during generation)
pub fn should_freeze(lv: &Region, biome: u8, p: P) -> bool {
    if temperature(lv, biome, p) >= 0.15 {
        return false;
    }
    if !(MIN_Y..=MAX_Y).contains(&p[1]) {
        return false;
    }
    let s = lv.get(p);
    is_water_block(s) && prop(s, "level") == Some("0")
}

/// `Biome.shouldSnow(level, pos)`
pub fn should_snow(lv: &Region, biome: u8, p: P) -> bool {
    if temperature(lv, biome, p) >= 0.15 {
        return false;
    }
    if !(MIN_Y..=MAX_Y).contains(&p[1]) {
        return false;
    }
    let s = lv.get(p);
    (vstate::air(s) || is(s, "snow")) && super::survive::can_survive(lv, st("snow"), p)
}

fn freeze(lv: &mut Region, o: P) -> bool {
    for i in 0..16 {
        for j in 0..16 {
            let (x, z) = (o[0] + i, o[2] + j);
            let y = lv.height(Hm::MotionFluid, x, z);
            let p = [x, y, z];
            let below = [x, y - 1, z];
            let biome = lv.biome(p);
            if should_freeze(lv, biome, below) {
                lv.set(below, st("ice"));
            }
            if should_snow(lv, biome, p) {
                lv.set(p, st("snow"));
                let b = lv.get(below);
                if prop(b, "snowy").is_some() {
                    lv.set(below, with(b, "snowy", "true"));
                }
            }
        }
    }
    true
}

// ------------------------------------------------------------ lakes, springs

fn mark_above(lv: &mut Region, p: P) {
    let mut q = p;
    for _ in 0..2 {
        q = up(q, 1);
        if vstate::air(lv.get(q)) {
            return;
        }
        lv.mark_post(q);
    }
}

fn lake(lv: &mut Region, r: &mut WgRandom, o: P, fluid: &StateP, barrier: &StateP) -> bool {
    if o[1] <= MIN_Y + 4 {
        return false;
    }
    let o = add(o, [-8, -4, -8]);
    let mut g = vec![false; 2048];
    let at = |x: i32, z: i32, y: i32| ((x * 16 + z) * 8 + y) as usize;
    let n = r.next_int_bound(4) + 4;
    for _ in 0..n {
        let d = r.next_double() * 6.0 + 3.0;
        let e = r.next_double() * 4.0 + 2.0;
        let f = r.next_double() * 6.0 + 3.0;
        let gx = r.next_double() * (16.0 - d - 2.0) + 1.0 + d / 2.0;
        let gy = r.next_double() * (8.0 - e - 4.0) + 2.0 + e / 2.0;
        let gz = r.next_double() * (16.0 - f - 2.0) + 1.0 + f / 2.0;
        for x in 1..15 {
            for z in 1..15 {
                for y in 1..7 {
                    let a = (x as f64 - gx) / (d / 2.0);
                    let b = (y as f64 - gy) / (e / 2.0);
                    let c = (z as f64 - gz) / (f / 2.0);
                    if a * a + b * b + c * c < 1.0 {
                        g[at(x, z, y)] = true;
                    }
                }
            }
        }
    }
    let fl = fluid.get(lv, r, o).unwrap_or(blocks::WATER);
    let edge = |g: &[bool], x: i32, z: i32, y: i32| {
        !g[at(x, z, y)]
            && (x < 15 && g[at(x + 1, z, y)] || x > 0 && g[at(x - 1, z, y)] || z < 15 && g[at(x, z + 1, y)] || z > 0 && g[at(x, z - 1, y)] || y < 7 && g[at(x, z, y + 1)] || y > 0 && g[at(x, z, y - 1)])
    };
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..8 {
                if edge(&g, x, z, y) {
                    let s = lv.get(add(o, [x, y, z]));
                    if y >= 4 && vstate::liquid(s) {
                        return false;
                    }
                    if y < 4 && !vstate::solid(s) && s != fl {
                        return false;
                    }
                }
            }
        }
    }
    let tags = &lv.lib.extra_tags;
    let cave_air = st("cave_air");
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..8 {
                if g[at(x, z, y)] {
                    let p = add(o, [x, y, z]);
                    if tags.features_cannot_replace.contains(lv.get(p)) {
                        continue;
                    }
                    let air = y >= 4;
                    lv.set(p, if air { cave_air } else { fl });
                    if air {
                        mark_above(lv, p);
                    }
                }
            }
        }
    }
    let bar = barrier.get(lv, r, o).unwrap_or(blocks::AIR);
    if !vstate::air(bar) {
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..8 {
                    if edge(&g, x, z, y) && (y < 4 || r.next_int_bound(2) != 0) {
                        let p = add(o, [x, y, z]);
                        let s = lv.get(p);
                        if vstate::solid(s) && !lv.lib.extra_tags.lava_pool_stone_cannot_replace.contains(s) {
                            lv.set(p, bar);
                            mark_above(lv, p);
                        }
                    }
                }
            }
        }
    }
    if vstate::is_water(fl) {
        for x in 0..16 {
            for z in 0..16 {
                let p = add(o, [x, 4, z]);
                let biome = lv.biome(p);
                if should_freeze(lv, biome, p) && !lv.lib.extra_tags.features_cannot_replace.contains(lv.get(p)) {
                    lv.set(p, st("ice"));
                }
            }
        }
    }
    true
}

fn spring(lv: &mut Region, o: P, state: BlockState, below: bool, rock: i32, hole: i32, valid: &BlockSet) -> bool {
    if !valid.contains(lv.get(up(o, 1))) {
        return false;
    }
    if below && !valid.contains(lv.get(up(o, -1))) {
        return false;
    }
    let s = lv.get(o);
    if !vstate::air(s) && !valid.contains(s) {
        return false;
    }
    let sides = [WEST, EAST, NORTH, SOUTH, DOWN];
    let rocks = sides.iter().filter(|d| valid.contains(lv.get(add(o, **d)))).count() as i32;
    let holes = sides.iter().filter(|d| vstate::air(lv.get(add(o, **d)))).count() as i32;
    if rocks == rock && holes == hole {
        lv.set(o, state);
        lv.schedule_tick(o);
        return true;
    }
    false
}

// ------------------------------------------------------------ ocean

fn seagrass(lv: &mut Region, r: &mut WgRandom, o: P, prob: f32) -> bool {
    let i = r.next_int_bound(8) - r.next_int_bound(8);
    let j = r.next_int_bound(8) - r.next_int_bound(8);
    let y = lv.height(Hm::Motion, o[0] + i, o[2] + j);
    let p = [o[0] + i, y, o[2] + j];
    if !is_water_block(lv.get(p)) {
        return false;
    }
    let tall = r.next_double() < prob as f64;
    let s = if tall { st("tall_seagrass") } else { st("seagrass") };
    if !super::survive::can_survive(lv, s, p) {
        return false;
    }
    if tall {
        let upper = with(s, "half", "upper");
        let a = up(p, 1);
        if is_water_block(lv.get(a)) {
            lv.set(p, s);
            lv.set(a, upper);
        }
    } else {
        lv.set(p, s);
    }
    true
}

/// `GrowingPlantBlock.canSurvive` for kelp (growing up).
fn kelp_survives(lv: &Region, p: P) -> bool {
    let b = lv.get(up(p, -1));
    if is(b, "magma_block") {
        return false;
    }
    is(b, "kelp") || is(b, "kelp_plant") || vstate::sturdy(b, 1)
}

fn kelp(lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
    let mut placed = 0;
    let y = lv.height(Hm::Motion, o[0], o[2]);
    let mut p = [o[0], y, o[2]];
    if !is_water_block(lv.get(p)) {
        return false;
    }
    let head = st("kelp");
    let body = st("kelp_plant");
    let n = 1 + r.next_int_bound(10);
    for k in 0..=n {
        if is_water_block(lv.get(p)) && is_water_block(lv.get(up(p, 1))) && kelp_survives(lv, p) {
            if k == n {
                let age = (r.next_int_bound(4) + 20).to_string();
                lv.set(p, with(head, "age", &age));
                placed += 1;
            } else {
                lv.set(p, body);
            }
        } else if k > 0 {
            let b = up(p, -1);
            if kelp_survives(lv, b) && !is(lv.get(up(b, -1)), "kelp") {
                let age = (r.next_int_bound(4) + 20).to_string();
                lv.set(b, with(head, "age", &age));
                placed += 1;
            }
            break;
        } else {
            break;
        }
        p = up(p, 1);
    }
    placed > 0
}

fn sea_pickle(lv: &mut Region, r: &mut WgRandom, o: P, n: &IntP) -> bool {
    let mut placed = 0;
    let count = n.sample(r);
    for _ in 0..count {
        let i = r.next_int_bound(8) - r.next_int_bound(8);
        let j = r.next_int_bound(8) - r.next_int_bound(8);
        let y = lv.height(Hm::Motion, o[0] + i, o[2] + j);
        let p = [o[0] + i, y, o[2] + j];
        let pickles = (r.next_int_bound(4) + 1).to_string();
        let s = with(st("sea_pickle"), "pickles", &pickles);
        if is_water_block(lv.get(p)) && super::survive::can_survive(lv, s, p) {
            lv.set(p, s);
            placed += 1;
        }
    }
    placed > 0
}

fn vines(lv: &mut Region, o: P) -> bool {
    if !vstate::air(lv.get(o)) {
        return false;
    }
    for d in ALL {
        if d == DOWN {
            continue;
        }
        let n = lv.get(add(o, d));
        // VineBlock.isAcceptableNeighbour: the neighbour's face toward us is full
        if vstate::sturdy(n, dir_index(neg(d))) {
            lv.set(o, with(st("vine"), face_name(d), "true"));
            return true;
        }
    }
    false
}

fn blue_ice(lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
    if o[1] > lv.lib.sea_level - 1 {
        return false;
    }
    if !is_water_block(lv.get(o)) && !is_water_block(lv.get(up(o, -1))) {
        return false;
    }
    let mut found = false;
    for d in ALL {
        if d == DOWN {
            continue;
        }
        if is(lv.get(add(o, d)), "packed_ice") {
            found = true;
            break;
        }
    }
    if !found {
        return false;
    }
    let blue = st("blue_ice");
    lv.set(o, blue);
    for _ in 0..200 {
        let i = r.next_int_bound(5) - r.next_int_bound(6);
        let mut j = 3;
        if i < 2 {
            j += i / 2;
        }
        if j < 1 {
            continue;
        }
        let a = r.next_int_bound(j) - r.next_int_bound(j);
        let c = r.next_int_bound(j) - r.next_int_bound(j);
        let p = add(o, [a, i, c]);
        let s = lv.get(p);
        if !(vstate::air(s) || is_water_block(s) || is(s, "packed_ice") || is(s, "ice")) {
            continue;
        }
        for d in ALL {
            if is(lv.get(add(p, d)), "blue_ice") {
                lv.set(p, blue);
                break;
            }
        }
    }
    true
}

/// `Column.scan(...).getFloor()` for `inside` / `edge` predicates.
pub fn column_floor(lv: &Region, o: P, range: i32, inside: &dyn Fn(BlockState) -> bool, edge: &dyn Fn(BlockState) -> bool) -> Option<i32> {
    if !inside(lv.get(o)) {
        return None;
    }
    let mut p = o;
    let mut i = 1;
    while i < range && inside(lv.get(p)) {
        p = up(p, -1);
        i += 1;
    }
    edge(lv.get(p)).then_some(p[1])
}

/// `Column.scan(...)`: (floor, ceiling)
pub fn column_scan(lv: &Region, o: P, range: i32, inside: &dyn Fn(BlockState) -> bool, edge: &dyn Fn(BlockState) -> bool) -> Option<(Option<i32>, Option<i32>)> {
    if !inside(lv.get(o)) {
        return None;
    }
    let scan = |dy: i32| {
        let mut p = o;
        let mut i = 1;
        while i < range && inside(lv.get(p)) {
            p = up(p, dy);
            i += 1;
        }
        edge(lv.get(p)).then_some(p[1])
    };
    let ceiling = scan(1);
    let floor = scan(-1);
    Some((floor, ceiling))
}

/// `isVisibleFromOutside`: the face toward `dir` is not a full block face.
fn visible_from_outside(lv: &Region, p: P, dir: P) -> bool {
    let s = lv.get(p);
    !(vstate::solid_render(s) || crate::world::light::face_full(s, dir_index(dir) as usize))
}

fn underwater_magma(lv: &mut Region, r: &mut WgRandom, o: P, range: i32, prob: f32, radius: i32) -> bool {
    let Some(fy) = column_floor(lv, o, range, &|s| is_water_block(s), &|s| !is_water_block(s)) else { return false };
    let c = [o[0], fy, o[2]];
    let mut n = 0;
    for z in c[2] - radius..=c[2] + radius {
        for y in c[1] - radius..=c[1] + radius {
            for x in c[0] - radius..=c[0] + radius {
                let p = [x, y, z];
                if r.next_float() >= prob {
                    continue;
                }
                let s = lv.get(p);
                if is_water_block(s) || vstate::air(s) || visible_from_outside(lv, up(p, -1), UP) {
                    continue;
                }
                if HORIZONTAL.iter().any(|d| visible_from_outside(lv, add(p, *d), neg(*d))) {
                    continue;
                }
                lv.set(p, st("magma_block"));
                n += 1;
            }
        }
    }
    n > 0
}

// ------------------------------------------------------------ dungeons

/// `StructurePiece.reorient` for a chest.
fn reorient_chest(lv: &Region, p: P) -> BlockState {
    let chest = st("chest");
    let mut dir = None;
    for d in HORIZONTAL {
        let s = lv.get(add(p, d));
        if is(s, "chest") {
            return chest;
        }
        if vstate::solid_render(s) {
            if dir.is_some() {
                dir = None;
                break;
            }
            dir = Some(d);
        }
    }
    if let Some(d) = dir {
        return with(chest, "facing", face_name(neg(d)));
    }
    let mut d = NORTH;
    if vstate::solid_render(lv.get(add(p, d))) {
        d = neg(d);
    }
    if vstate::solid_render(lv.get(add(p, d))) {
        d = [-d[2], 0, d[0]];
    }
    if vstate::solid_render(lv.get(add(p, d))) {
        d = neg(d);
    }
    with(chest, "facing", face_name(d))
}

fn monster_room(lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
    let cannot = lv.lib.extra_tags.features_cannot_replace.clone();
    let safe_set = |lv: &mut Region, p: P, s: BlockState| {
        if !cannot.contains(lv.get(p)) {
            lv.set(p, s);
        }
    };
    let j = r.next_int_bound(2) + 2;
    let (k, l) = (-j - 1, j + 1);
    let o2 = r.next_int_bound(2) + 2;
    let (p0, q0) = (-o2 - 1, o2 + 1);
    let mut holes = 0;
    for x in k..=l {
        for y in -1..=4 {
            for z in p0..=q0 {
                let p = add(o, [x, y, z]);
                let solid = vstate::solid(lv.get(p));
                if y == -1 && !solid {
                    return false;
                }
                if y == 4 && !solid {
                    return false;
                }
                if (x == k || x == l || z == p0 || z == q0) && y == 0 && vstate::air(lv.get(p)) && vstate::air(lv.get(up(p, 1))) {
                    holes += 1;
                }
            }
        }
    }
    if !(1..=5).contains(&holes) {
        return false;
    }
    let cave_air = st("cave_air");
    for x in k..=l {
        let mut y = 3;
        while y >= -1 {
            for z in p0..=q0 {
                let p = add(o, [x, y, z]);
                let s = lv.get(p);
                if x != k && y != -1 && z != p0 && x != l && y != 4 && z != q0 {
                    if !is(s, "chest") && !is(s, "spawner") {
                        safe_set(lv, p, cave_air);
                    }
                } else if p[1] >= MIN_Y && !vstate::solid(lv.get(up(p, -1))) {
                    lv.set(p, cave_air);
                } else if vstate::solid(s) && !is(s, "chest") {
                    if y == -1 && r.next_int_bound(4) != 0 {
                        safe_set(lv, p, st("mossy_cobblestone"));
                    } else {
                        safe_set(lv, p, st("cobblestone"));
                    }
                }
            }
            y -= 1;
        }
    }
    for _ in 0..2 {
        for _ in 0..3 {
            let x = o[0] + r.next_int_bound(j * 2 + 1) - j;
            let z = o[2] + r.next_int_bound(o2 * 2 + 1) - o2;
            let p = [x, o[1], z];
            if !vstate::air(lv.get(p)) {
                continue;
            }
            let n = HORIZONTAL.iter().filter(|d| vstate::solid(lv.get(add(p, **d)))).count();
            if n == 1 {
                let c = reorient_chest(lv, p);
                safe_set(lv, p, c);
                r.next_long(); // loot table seed
                break;
            }
        }
    }
    safe_set(lv, o, st("spawner"));
    r.next_int_bound(4); // spawned mob
    true
}

// ------------------------------------------------------------ huge mushrooms

#[allow(clippy::too_many_arguments)]
fn huge_mushroom(lv: &mut Region, r: &mut WgRandom, o: P, red: bool, cap: &StateP, stem: &StateP, radius: i32, on: &Pred) -> bool {
    let mut h = r.next_int_bound(3) + 4;
    if r.next_int_bound(12) == 0 {
        h *= 2;
    }
    // isValidPosition
    if o[1] < MIN_Y + 1 || o[1] + h + 1 > MAX_Y {
        return false;
    }
    if !on.test(lv, up(o, -1)) {
        return false;
    }
    let tags = &lv.lib.extra_tags;
    for y in 0..=h {
        let rr = if red { 0 } else if y <= 3 { 0 } else { radius };
        for dx in -rr..=rr {
            for dz in -rr..=rr {
                let s = lv.get(add(o, [dx, y, dz]));
                if !vstate::air(s) && !tags.leaves.contains(s) {
                    return false;
                }
            }
        }
    }
    let place = |lv: &mut Region, p: P, s: BlockState| {
        let cur = lv.get(p);
        if vstate::air(cur) || lv.lib.extra_tags.replaceable_by_mushrooms.contains(cur) {
            lv.set(p, s);
        }
    };
    let has4 = |s: BlockState| ["west", "east", "north", "south"].iter().all(|k| prop(s, k).is_some());
    let b = |v: bool| if v { "true" } else { "false" };
    if red {
        for y in h - 3..=h {
            let rr = if y < h { radius } else { radius - 1 };
            let j = radius - 2;
            for dx in -rr..=rr {
                for dz in -rr..=rr {
                    let (b1, b2, b3, b4) = (dx == -rr, dx == rr, dz == -rr, dz == rr);
                    let b5 = b1 || b2;
                    let b6 = b3 || b4;
                    if y < h && b5 == b6 {
                        continue;
                    }
                    let p = add(o, [dx, y, dz]);
                    let mut s = cap.get(lv, r, o).unwrap_or(blocks::AIR);
                    if has4(s) && prop(s, "up").is_some() {
                        s = with(s, "up", b(y >= h - 1));
                        s = with(s, "west", b(dx < -j));
                        s = with(s, "east", b(dx > j));
                        s = with(s, "north", b(dz < -j));
                        s = with(s, "south", b(dz > j));
                    }
                    place(lv, p, s);
                }
            }
        }
    } else {
        let rr = radius;
        for dx in -rr..=rr {
            for dz in -rr..=rr {
                let (b1, b2, b3, b4) = (dx == -rr, dx == rr, dz == -rr, dz == rr);
                let b5 = b1 || b2;
                let b6 = b3 || b4;
                if b5 && b6 {
                    continue;
                }
                let p = add(o, [dx, h, dz]);
                let w = b1 || b6 && dx == 1 - rr;
                let e = b2 || b6 && dx == rr - 1;
                let n = b3 || b5 && dz == 1 - rr;
                let so = b4 || b5 && dz == rr - 1;
                let mut s = cap.get(lv, r, o).unwrap_or(blocks::AIR);
                if has4(s) {
                    s = with(s, "west", b(w));
                    s = with(s, "east", b(e));
                    s = with(s, "north", b(n));
                    s = with(s, "south", b(so));
                }
                place(lv, p, s);
            }
        }
    }
    for i in 0..h {
        let s = stem.get(lv, r, o).unwrap_or(blocks::AIR);
        place(lv, up(o, i), s);
    }
    true
}

// ------------------------------------------------------------ bamboo, spikes

fn bamboo(lv: &mut Region, r: &mut WgRandom, o: P, prob: f32) -> bool {
    let mut placed = 0;
    let mut m = o;
    if !vstate::air(lv.get(m)) {
        return false;
    }
    if !lv.lib.extra_tags.bamboo_plantable_on.contains(lv.get(up(m, -1))) {
        return false;
    }
    let n = r.next_int_bound(12) + 5;
    if r.next_float() < prob {
        let k = r.next_int_bound(4) + 1;
        for x in o[0] - k..=o[0] + k {
            for z in o[2] - k..=o[2] + k {
                let (a, c) = (x - o[0], z - o[2]);
                if a * a + c * c > k * k {
                    continue;
                }
                let p = [x, lv.height(Hm::NotAir, x, z) - 1, z];
                if lv.lib.extra_tags.beneath_bamboo_podzol_replaceable.contains(lv.get(p)) {
                    lv.set(p, st("podzol"));
                }
            }
        }
    }
    let bamboo = st("bamboo");
    let trunk = with(with(with(bamboo, "age", "1"), "leaves", "none"), "stage", "0");
    let final_large = with(with(trunk, "leaves", "large"), "stage", "1");
    let top_large = with(trunk, "leaves", "large");
    let top_small = with(trunk, "leaves", "small");
    for _ in 0..n {
        if !vstate::air(lv.get(m)) {
            break;
        }
        lv.set(m, trunk);
        m = up(m, 1);
    }
    if m[1] - o[1] >= 3 {
        lv.set(m, final_large);
        m = up(m, -1);
        lv.set(m, top_large);
        m = up(m, -1);
        lv.set(m, top_small);
    }
    placed += 1;
    placed > 0
}

fn spike(lv: &mut Region, r: &mut WgRandom, o: P, state: BlockState, on: &Pred, replace: &Pred) -> bool {
    let mut o = o;
    while vstate::air(lv.get(o)) && o[1] > MIN_Y + 2 {
        o = up(o, -1);
    }
    if !on.test(lv, o) {
        return false;
    }
    o = up(o, r.next_int_bound(4));
    let i = r.next_int_bound(4) + 7;
    let j = i / 4 + r.next_int_bound(2);
    if j > 1 && r.next_int_bound(60) == 0 {
        o = up(o, 10 + r.next_int_bound(30));
    }
    for k in 0..i {
        let f = (1.0 - k as f32 / i as f32) * j as f32;
        let l = f.ceil() as i32;
        for m in -l..=l {
            let g = m.abs() as f32 - 0.25;
            for n in -l..=l {
                let h = n.abs() as f32 - 0.25;
                if (m == 0 && n == 0 || g * g + h * h <= f * f) && (m != -l && m != l && n != -l && n != l || r.next_float() <= 0.75) {
                    let p = add(o, [m, k, n]);
                    let s = lv.get(p);
                    if vstate::air(s) || replace.test(lv, p) {
                        lv.set(p, state);
                    }
                    if k != 0 && l > 1 {
                        let p = add(o, [m, -k, n]);
                        let s = lv.get(p);
                        if vstate::air(s) || replace.test(lv, p) {
                            lv.set(p, state);
                        }
                    }
                }
            }
        }
    }
    let mut k = j - 1;
    if k < 0 {
        k = 0;
    } else if k > 1 {
        k = 1;
    }
    for m in -k..=k {
        for n in -k..=k {
            let mut p = add(o, [m, -1, n]);
            let mut q = 50;
            if m.abs() == 1 && n.abs() == 1 {
                q = r.next_int_bound(5);
            }
            while p[1] > 50 {
                let s = lv.get(p);
                if !(vstate::air(s) || replace.test(lv, p) || s == state) {
                    break;
                }
                lv.set(p, state);
                p = up(p, -1);
                q -= 1;
                if q <= 0 {
                    p = up(p, -(r.next_int_bound(5) + 1));
                    q = r.next_int_bound(5);
                }
            }
        }
    }
    true
}

