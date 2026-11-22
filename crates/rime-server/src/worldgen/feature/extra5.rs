//! Nether and End features (ported from the 26.1 bytecode): glowstone,
//! weeping / twisting vines, nether forest vegetation, replace blobs,
//! basalt pillars and columns, deltas, huge fungi, end islands, end
//! gateways, the end platform, end spikes and chorus plants.

use anyhow::Result;
use serde_json::Value;

use super::extra::{is, st, with, P};
use super::place::{mth_next_int, parse_state, IntP, Pred, StateP};
use super::{Builder, Region};
use crate::registry::{vstate, BlockState};
use crate::worldgen::random::{Legacy, Rng, WgRandom};

fn i32v(v: &Value, k: &str, d: i32) -> i32 {
    v[k].as_i64().map(|x| x as i32).unwrap_or(d)
}

pub struct Fungus {
    valid_base: BlockState,
    stem: BlockState,
    hat: BlockState,
    decor: BlockState,
    replaceable: Pred,
    planted: bool,
}

pub enum Extra5 {
    Glowstone,
    WeepingVines,
    TwistingVines { width: i32, height: i32, max_height: i32 },
    ForestVegetation { provider: StateP, width: i32, height: i32 },
    ReplaceBlobs { target: BlockState, state: BlockState, radius: IntP },
    BasaltPillar,
    BasaltColumns { reach: IntP, height: IntP },
    Delta { contents: BlockState, rim: BlockState, size: IntP, rim_size: IntP },
    HugeFungus(Box<Fungus>),
    EndIsland,
    EndPlatform,
    EndGateway { exit: Option<P>, exact: bool },
    EndSpike { invulnerable: bool, spikes: Vec<Spike> },
    ChorusPlant,
}

#[derive(Clone, Copy, Debug)]
pub struct Spike {
    pub x: i32,
    pub z: i32,
    pub radius: i32,
    pub height: i32,
    pub guarded: bool,
}

impl Extra5 {
    pub fn parse(b: &mut Builder, ty: &str, c: &Value) -> Option<Result<Extra5>> {
        let r = (|| -> Result<Option<Extra5>> {
            Ok(Some(match ty {
                "glowstone_blob" => Extra5::Glowstone,
                "weeping_vines" => Extra5::WeepingVines,
                "twisting_vines" => Extra5::TwistingVines { width: i32v(c, "spread_width", 8), height: i32v(c, "spread_height", 4), max_height: i32v(c, "max_height", 8) },
                "nether_forest_vegetation" => Extra5::ForestVegetation { provider: StateP::parse(b, &c["state_provider"])?, width: i32v(c, "spread_width", 8), height: i32v(c, "spread_height", 4) },
                "netherrack_replace_blobs" => Extra5::ReplaceBlobs { target: parse_state(&c["target"])?, state: parse_state(&c["state"])?, radius: IntP::parse(&c["radius"])? },
                "basalt_pillar" => Extra5::BasaltPillar,
                "basalt_columns" => Extra5::BasaltColumns { reach: IntP::parse(&c["reach"])?, height: IntP::parse(&c["height"])? },
                "delta_feature" => Extra5::Delta { contents: parse_state(&c["contents"])?, rim: parse_state(&c["rim"])?, size: IntP::parse(&c["size"])?, rim_size: IntP::parse(&c["rim_size"])? },
                "huge_fungus" => Extra5::HugeFungus(Box::new(Fungus {
                    valid_base: parse_state(&c["valid_base_block"])?,
                    stem: parse_state(&c["stem_state"])?,
                    hat: parse_state(&c["hat_state"])?,
                    decor: parse_state(&c["decor_state"])?,
                    replaceable: Pred::parse(b, &c["replaceable_blocks"])?,
                    planted: c["planted"].as_bool().unwrap_or(false),
                })),
                "end_island" => Extra5::EndIsland,
                "end_platform" => Extra5::EndPlatform,
                "end_gateway" => Extra5::EndGateway {
                    exit: c["exit"].as_array().filter(|a| a.len() == 3).map(|a| [a[0].as_i64().unwrap_or(0) as i32, a[1].as_i64().unwrap_or(0) as i32, a[2].as_i64().unwrap_or(0) as i32]),
                    exact: c["exact"].as_bool().unwrap_or(false),
                },
                "end_spike" => Extra5::EndSpike {
                    invulnerable: c["crystal_invulnerable"].as_bool().unwrap_or(false),
                    spikes: c["spikes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|s| Spike {
                            x: i32v(s, "centerX", 0),
                            z: i32v(s, "centerZ", 0),
                            radius: i32v(s, "radius", 0),
                            height: i32v(s, "height", 0),
                            guarded: s["guarded"].as_bool().unwrap_or(false),
                        })
                        .collect(),
                },
                "chorus_plant" => Extra5::ChorusPlant,
                _ => return Ok(None),
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
            Extra5::Glowstone => glowstone(lv, r, o),
            Extra5::WeepingVines => weeping_vines(lv, r, o),
            Extra5::TwistingVines { width, height, max_height } => twisting_vines(lv, r, o, *width, *height, *max_height),
            Extra5::ForestVegetation { provider, width, height } => forest_vegetation(lv, r, o, provider, *width, *height),
            Extra5::ReplaceBlobs { target, state, radius } => replace_blobs(lv, r, o, *target, *state, radius),
            Extra5::BasaltPillar => basalt_pillar(lv, r, o),
            Extra5::BasaltColumns { reach, height } => basalt_columns(lv, r, o, reach, height),
            Extra5::Delta { contents, rim, size, rim_size } => delta(lv, r, o, *contents, *rim, size, rim_size),
            Extra5::HugeFungus(f) => huge_fungus(lv, r, o, f),
            Extra5::EndIsland => end_island(lv, r, o),
            Extra5::EndPlatform => {
                end_platform(lv, o);
                true
            }
            Extra5::EndGateway { exit, exact } => end_gateway(lv, o, *exit, *exact),
            Extra5::EndSpike { invulnerable, spikes } => end_spikes(lv, r, o, *invulnerable, spikes),
            Extra5::ChorusPlant => {
                if lv.is_empty(o) && is(lv.get(below(o)), "end_stone") {
                    chorus_generate(lv, o, r, 8);
                    true
                } else {
                    false
                }
            }
        }
    }
}

fn off(p: P, x: i32, y: i32, z: i32) -> P {
    [p[0] + x, p[1] + y, p[2] + z]
}
fn below(p: P) -> P {
    off(p, 0, -1, 0)
}
fn above(p: P) -> P {
    off(p, 0, 1, 0)
}
/// `Direction.values()` offsets: down, up, north, south, west, east.
const DIRS: [P; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];
/// `Direction.Plane.HORIZONTAL`: north, east, south, west.
const HORIZ: [P; 4] = [[0, 0, -1], [1, 0, 0], [0, 0, 1], [-1, 0, 0]];

fn manhattan(a: P, b: P) -> i32 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()
}

/// `BlockPos.withinManhattan(origin, rx, ry, rz)` in its iteration order, up
/// to manhattan distance `limit`.
fn within_manhattan(o: P, rx: i32, ry: i32, rz: i32, limit: i32) -> Vec<P> {
    let max_depth = rx + ry + rz;
    let mut out = Vec::new();
    let (mut depth, mut max_x, mut max_y, mut x, mut y) = (0i32, 0i32, 0i32, 0i32, 0i32);
    loop {
        let mut found = None;
        while found.is_none() {
            if y > max_y {
                x += 1;
                if x > max_x {
                    depth += 1;
                    if depth > max_depth || depth > limit {
                        return out;
                    }
                    max_x = rx.min(depth);
                    x = -max_x;
                }
                max_y = ry.min(depth - x.abs());
                y = -max_y;
            }
            let z = depth - x.abs() - y.abs();
            if z <= rz {
                found = Some((x, y, z));
            }
            y += 1;
        }
        let (fx, fy, fz) = found.expect("found");
        out.push([o[0] + fx, o[1] + fy, o[2] + fz]);
        if fz != 0 {
            out.push([o[0] + fx, o[1] + fy, o[2] - fz]);
        }
    }
}

// ------------------------------------------------------------ nether

fn glowstone(lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
    if !lv.is_empty(o) {
        return false;
    }
    let a = lv.get(above(o));
    if !(is(a, "netherrack") || is(a, "basalt") || is(a, "blackstone")) {
        return false;
    }
    let glow = st("glowstone");
    lv.set(o, glow);
    for _ in 0..1500 {
        let x = r.next_int_bound(8) - r.next_int_bound(8);
        let y = -r.next_int_bound(12);
        let z = r.next_int_bound(8) - r.next_int_bound(8);
        let p = off(o, x, y, z);
        if !lv.get(p).is_air() {
            continue;
        }
        let mut n = 0;
        for d in DIRS {
            if is(lv.get(off(p, d[0], d[1], d[2])), "glowstone") {
                n += 1;
            }
            if n > 1 {
                break;
            }
        }
        if n == 1 {
            lv.set(p, glow);
        }
    }
    true
}

fn weeping_vines(lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
    if !lv.is_empty(o) {
        return false;
    }
    let a = lv.get(above(o));
    if !is(a, "netherrack") && !is(a, "nether_wart_block") {
        return false;
    }
    // placeRoofNetherWart
    let wart = st("nether_wart_block");
    lv.set(o, wart);
    for _ in 0..200 {
        let x = r.next_int_bound(6) - r.next_int_bound(6);
        let y = r.next_int_bound(2) - r.next_int_bound(5);
        let z = r.next_int_bound(6) - r.next_int_bound(6);
        let m = off(o, x, y, z);
        if lv.is_empty(m) {
            let mut j = 0;
            for d in DIRS {
                let s = lv.get(off(m, d[0], d[1], d[2]));
                if is(s, "netherrack") || is(s, "nether_wart_block") {
                    j += 1;
                }
                if j > 1 {
                    break;
                }
            }
            if j == 1 {
                lv.set(m, wart);
            }
        }
    }
    // placeRoofWeepingVines
    for _ in 0..100 {
        let x = r.next_int_bound(8) - r.next_int_bound(8);
        let y = r.next_int_bound(2) - r.next_int_bound(7);
        let z = r.next_int_bound(8) - r.next_int_bound(8);
        let m = off(o, x, y, z);
        if lv.is_empty(m) {
            let a = lv.get(above(m));
            if is(a, "netherrack") || is(a, "nether_wart_block") {
                let mut len = mth_next_int(r, 1, 8);
                if r.next_int_bound(6) == 0 {
                    len *= 2;
                }
                if r.next_int_bound(5) == 0 {
                    len = 1;
                }
                weeping_column(lv, r, m, len, 17, 25);
            }
        }
    }
    true
}

/// `WeepingVinesFeature.placeWeepingVinesColumn`
pub fn weeping_column(lv: &mut Region, r: &mut WgRandom, mut p: P, len: i32, min_age: i32, max_age: i32) {
    for i in 0..=len {
        if lv.is_empty(p) {
            if i == len || !lv.is_empty(below(p)) {
                let age = mth_next_int(r, min_age, max_age);
                lv.set(p, with(st("weeping_vines"), "age", &age.to_string()));
                break;
            }
            lv.set(p, st("weeping_vines_plant"));
        }
        p = below(p);
    }
}

fn twisting_invalid(lv: &Region, p: P) -> bool {
    if !lv.is_empty(p) {
        return true;
    }
    let b = lv.get(below(p));
    !is(b, "netherrack") && !is(b, "warped_nylium") && !is(b, "warped_wart_block")
}

fn twisting_vines(lv: &mut Region, r: &mut WgRandom, o: P, w: i32, h: i32, max_h: i32) -> bool {
    if twisting_invalid(lv, o) {
        return false;
    }
    for _ in 0..w * w {
        let x = mth_next_int(r, -w, w);
        let y = mth_next_int(r, -h, h);
        let z = mth_next_int(r, -w, w);
        let mut m = off(o, x, y, z);
        // findFirstAirBlockAboveGround
        let found = loop {
            m = below(m);
            if lv.outside_build(m[1]) {
                break false;
            }
            if !lv.get(m).is_air() {
                m = above(m);
                break true;
            }
        };
        if found && !twisting_invalid(lv, m) {
            let mut len = mth_next_int(r, 1, max_h);
            if r.next_int_bound(6) == 0 {
                len *= 2;
            }
            if r.next_int_bound(5) == 0 {
                len = 1;
            }
            // TwistingVinesFeature.placeWeepingVinesColumn
            let mut p = m;
            for i in 1..=len {
                if lv.is_empty(p) {
                    if i == len || !lv.is_empty(above(p)) {
                        let age = mth_next_int(r, 17, 25);
                        lv.set(p, with(st("twisting_vines"), "age", &age.to_string()));
                        break;
                    }
                    lv.set(p, st("twisting_vines_plant"));
                }
                p = above(p);
            }
        }
    }
    true
}

fn forest_vegetation(lv: &mut Region, r: &mut WgRandom, o: P, provider: &StateP, w: i32, h: i32) -> bool {
    let b = lv.get(below(o));
    if !matches!(b.name(), "crimson_nylium" | "warped_nylium") {
        return false;
    }
    let y = o[1];
    if !(y >= lv.lib.dim_min + 1 && y + 1 <= lv.lib.dim_min + lv.lib.dim_height - 1) {
        return false;
    }
    let mut n = 0;
    for _ in 0..w * w {
        let x = r.next_int_bound(w) - r.next_int_bound(w);
        let yy = r.next_int_bound(h) - r.next_int_bound(h);
        let z = r.next_int_bound(w) - r.next_int_bound(w);
        let p = off(o, x, yy, z);
        let Some(s) = provider.get(lv, r, p) else { continue };
        if lv.is_empty(p) && p[1] > lv.lib.dim_min && super::survive::can_survive(lv, s, p) {
            lv.set(p, s);
            n += 1;
        }
    }
    n > 0
}

fn replace_blobs(lv: &mut Region, r: &mut WgRandom, o: P, target: BlockState, state: BlockState, radius: &IntP) -> bool {
    let tname = target.name();
    let max_y = lv.lib.dim_min + lv.lib.dim_height - 1;
    let mut m = [o[0], o[1].clamp(lv.lib.dim_min + 1, max_y), o[2]];
    // findTarget
    let found = loop {
        if m[1] <= lv.lib.dim_min + 1 {
            break None;
        }
        if lv.get(m).name() == tname {
            break Some(m);
        }
        m = below(m);
    };
    let Some(pos) = found else { return false };
    let rx = radius.sample(r);
    let ry = radius.sample(r);
    let rz = radius.sample(r);
    let max = rx.max(ry.max(rz));
    let mut placed = false;
    for p in within_manhattan(pos, rx, ry, rz, max) {
        if manhattan(p, pos) > max {
            break;
        }
        if lv.get(p).name() == tname {
            lv.set(p, state);
            placed = true;
        }
    }
    placed
}

fn basalt_pillar(lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
    if !lv.is_empty(o) || lv.is_empty(above(o)) {
        return false;
    }
    let basalt = st("basalt");
    let hang = |lv: &mut Region, r: &mut WgRandom, p: P| -> bool {
        if r.next_int_bound(10) != 0 {
            lv.set(p, basalt);
            true
        } else {
            false
        }
    };
    let mut m = o;
    let (mut n, mut s, mut w, mut e) = (true, true, true, true);
    while lv.is_empty(m) {
        if lv.outside_build(m[1]) {
            return true;
        }
        lv.set(m, basalt);
        n = n && hang(lv, r, off(m, 0, 0, -1));
        s = s && hang(lv, r, off(m, 0, 0, 1));
        w = w && hang(lv, r, off(m, -1, 0, 0));
        e = e && hang(lv, r, off(m, 1, 0, 0));
        m = below(m);
    }
    m = above(m);
    for d in [[0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]] {
        if r.next_bool() {
            lv.set(off(m, d[0], d[1], d[2]), basalt);
        }
    }
    m = below(m);
    for dx in -3i32..4 {
        for dz in -3i32..4 {
            let d = dx.abs() * dz.abs();
            if r.next_int_bound(10) < 10 - d {
                let mut q = off(m, dx, 0, dz);
                let mut k = 3;
                while lv.is_empty(below(q)) {
                    q = below(q);
                    k -= 1;
                    if k <= 0 {
                        break;
                    }
                }
                if !lv.is_empty(below(q)) {
                    lv.set(q, basalt);
                }
            }
        }
    }
    true
}

/// `BasaltColumnsFeature.CANNOT_PLACE_ON`
fn cannot_place_on(s: BlockState) -> bool {
    matches!(s.name(), "lava" | "bedrock" | "magma_block" | "soul_sand" | "nether_bricks" | "nether_brick_fence" | "nether_brick_stairs" | "nether_wart" | "chest" | "spawner")
}

fn air_or_lava_ocean(lv: &Region, sea: i32, p: P) -> bool {
    let s = lv.get(p);
    s.is_air() || is(s, "lava") && p[1] <= sea
}

fn columns_can_place(lv: &Region, sea: i32, p: P) -> bool {
    if !air_or_lava_ocean(lv, sea, p) {
        return false;
    }
    let b = lv.get(below(p));
    !b.is_air() && !cannot_place_on(b)
}

fn basalt_columns(lv: &mut Region, r: &mut WgRandom, o: P, reach: &IntP, height: &IntP) -> bool {
    let sea = lv.lib.sea_level;
    if !columns_can_place(lv, sea, o) {
        return false;
    }
    let h = height.sample(r);
    let big = r.next_float() < 0.9;
    let range = h.min(if big { 5 } else { 8 });
    let limit = if big { 50 } else { 15 };
    let basalt = st("basalt");
    let mut any = false;
    let w = 2 * range + 1;
    for _ in 0..limit {
        // BlockPos.randomBetweenClosed (one y)
        let p = [o[0] - range + r.next_int_bound(w), o[1] + r.next_int_bound(1), o[2] - range + r.next_int_bound(w)];
        let dist = h - manhattan(p, o);
        if dist < 0 {
            continue;
        }
        let rc = reach.sample(r);
        // placeColumn
        for dz in -rc..=rc {
            for dx in -rc..=rc {
                let q0 = [p[0] + dx, p[1], p[2] + dz];
                let d = manhattan(q0, p);
                let found = if air_or_lava_ocean(lv, sea, q0) {
                    // findSurface
                    let mut m = q0;
                    let mut dd = d;
                    loop {
                        if !(m[1] > lv.lib.dim_min + 1 && dd > 0) {
                            break None;
                        }
                        dd -= 1;
                        if columns_can_place(lv, sea, m) {
                            break Some(m);
                        }
                        m = below(m);
                    }
                } else {
                    // findAir
                    let mut m = q0;
                    let mut dd = d;
                    let max_y = lv.lib.dim_min + lv.lib.dim_height - 1;
                    loop {
                        if !(m[1] <= max_y && dd > 0) {
                            break None;
                        }
                        dd -= 1;
                        let s = lv.get(m);
                        if cannot_place_on(s) {
                            break None;
                        }
                        if s.is_air() {
                            break Some(m);
                        }
                        m = above(m);
                    }
                };
                if let Some(mut m) = found {
                    let mut n = dist - d / 2;
                    while n >= 0 {
                        if air_or_lava_ocean(lv, sea, m) {
                            lv.set(m, basalt);
                            m = above(m);
                            any = true;
                        } else {
                            if !is(lv.get(m), "basalt") {
                                break;
                            }
                            m = above(m);
                        }
                        n -= 1;
                    }
                }
            }
        }
    }
    any
}

fn delta(lv: &mut Region, r: &mut WgRandom, o: P, contents: BlockState, rim: BlockState, size: &IntP, rim_size: &IntP) -> bool {
    let cname = contents.name();
    let clear = |lv: &Region, p: P| -> bool {
        let s = lv.get(p);
        if s.name() == cname {
            return false;
        }
        if matches!(s.name(), "bedrock" | "nether_bricks" | "nether_brick_fence" | "nether_brick_stairs" | "nether_wart" | "chest" | "spawner") {
            return false;
        }
        for (i, d) in DIRS.iter().enumerate() {
            let air = lv.get(off(p, d[0], d[1], d[2])).is_air();
            let up = i == 1;
            if air && !up || !air && up {
                return false;
            }
        }
        true
    };
    let mut any = false;
    let rim_on = r.next_double() < 0.9;
    let rx = if rim_on { rim_size.sample(r) } else { 0 };
    let rz = if rim_on { rim_size.sample(r) } else { 0 };
    let has_rim = rim_on && rx != 0 && rz != 0;
    let sx = size.sample(r);
    let sz = size.sample(r);
    let max = sx.max(sz);
    for p in within_manhattan(o, sx, 0, sz, max) {
        if manhattan(p, o) > max {
            break;
        }
        if clear(lv, p) {
            if has_rim {
                any = true;
                lv.set(p, rim);
            }
            let q = off(p, rx, 0, rz);
            if clear(lv, q) {
                any = true;
                lv.set(q, contents);
            }
        }
    }
    any
}

fn huge_fungus(lv: &mut Region, r: &mut WgRandom, o: P, f: &Fungus) -> bool {
    if lv.get(below(o)).name() != f.valid_base.name() {
        return false;
    }
    let mut h = mth_next_int(r, 4, 13);
    if r.next_int_bound(12) == 0 {
        h *= 2;
    }
    if !f.planted && o[1] + h + 1 >= lv.lib.gen_depth {
        return false;
    }
    let big = !f.planted && r.next_float() < 0.06;
    lv.set(o, crate::registry::blocks::AIR);
    let replaceable = |lv: &Region, p: P, check: bool| -> bool { vstate::replaceable(lv.get(p)) || check && f.replaceable.test(lv, p) };
    // WorldGenRegion.destroyBlock (no drops during generation)
    let destroy = |lv: &mut Region, p: P| {
        let s = lv.get(p);
        if !s.is_air() && !matches!(s.name(), "water" | "lava") {
            lv.set(p, crate::registry::blocks::AIR);
        }
    };
    // placeStem
    let rr: i32 = if big { 1 } else { 0 };
    for dx in -rr..=rr {
        let dx: i32 = dx;
        for dz in -rr..=rr {
            let dz: i32 = dz;
            let corner = big && dx.abs() == rr && dz.abs() == rr;
            for y in 0..h {
                let m = off(o, dx, y, dz);
                if replaceable(lv, m, true) {
                    if f.planted {
                        if !lv.get(below(m)).is_air() {
                            destroy(lv, m);
                        }
                        lv.set(m, f.stem);
                    } else if corner {
                        if r.next_float() < 0.1 {
                            lv.set(m, f.stem);
                        }
                    } else {
                        lv.set(m, f.stem);
                    }
                }
            }
        }
    }
    // placeHat
    let wart = is(f.hat, "nether_wart_block");
    let hat_h = (r.next_int_bound(1 + h / 3) + 5).min(h);
    let start = h - hat_h;
    for y in start..=h {
        let mut rad = if y < h - r.next_int_bound(3) { 2 } else { 1 };
        if hat_h > 8 && y < start + 4 {
            rad = 3;
        }
        if big {
            rad += 1;
        }
        for dx in -rad..=rad {
            for dz in -rad..=rad {
                let ex = dx == -rad || dx == rad;
                let ez = dz == -rad || dz == rad;
                let inner = !ex && !ez && y != h;
                let corner = ex && ez;
                let low = y < start + 3;
                let m = off(o, dx, y, dz);
                if replaceable(lv, m, false) {
                    if f.planted && !lv.get(below(m)).is_air() {
                        destroy(lv, m);
                    }
                    if low {
                        if !inner {
                            // placeHatDropBlock
                            if lv.get(below(m)).name() == f.hat.name() {
                                lv.set(m, f.hat);
                            } else if (r.next_float() as f64) < 0.15 {
                                lv.set(m, f.hat);
                                if wart && r.next_int_bound(11) == 0 {
                                    fungus_vines(lv, r, m);
                                }
                            }
                        }
                    } else {
                        let (decor, hatc, vine) = if inner {
                            (0.1, 0.2, if wart { 0.1 } else { 0.0 })
                        } else if corner {
                            (0.01, 0.7, if wart { 0.083 } else { 0.0 })
                        } else {
                            (5.0e-4, 0.98, if wart { 0.07 } else { 0.0 })
                        };
                        // placeHatBlock
                        if r.next_float() < decor {
                            lv.set(m, f.decor);
                        } else if r.next_float() < hatc {
                            lv.set(m, f.hat);
                            if r.next_float() < vine {
                                fungus_vines(lv, r, m);
                            }
                        }
                    }
                }
            }
        }
    }
    true
}

/// `HugeFungusFeature.tryPlaceWeepingVines`
fn fungus_vines(lv: &mut Region, r: &mut WgRandom, p: P) {
    let m = below(p);
    if !lv.is_empty(m) {
        return;
    }
    let mut len = mth_next_int(r, 1, 5);
    if r.next_int_bound(7) == 0 {
        len *= 2;
    }
    weeping_column(lv, r, m, len, 23, 25);
}

// ------------------------------------------------------------ end

fn end_island(lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
    let stone = st("end_stone");
    let mut rad = r.next_int_bound(3) as f32 + 4.0;
    let mut y = 0;
    while rad > 0.5 {
        let lo = (-rad).floor() as i32;
        let hi = rad.ceil() as i32;
        for x in lo..=hi {
            for z in lo..=hi {
                if ((x * x + z * z) as f32) <= (rad + 1.0) * (rad + 1.0) {
                    lv.set(off(o, x, y, z), stone);
                }
            }
        }
        rad -= r.next_int_bound(2) as f32 + 0.5;
        y -= 1;
    }
    true
}

/// `EndPlatformFeature.createEndPlatform` (no drops)
fn end_platform(lv: &mut Region, o: P) {
    let obsidian = st("obsidian");
    for dx in -2..=2 {
        for dz in -2..=2 {
            for dy in -1..3 {
                let p = off(o, dz, dy, dx);
                let b = if dy == -1 { obsidian } else { crate::registry::blocks::AIR };
                if lv.get(p).name() != b.name() {
                    lv.set(p, b);
                }
            }
        }
    }
}

fn end_gateway(lv: &mut Region, o: P, exit: Option<P>, exact: bool) -> bool {
    let bedrock = st("bedrock");
    let air = crate::registry::blocks::AIR;
    for z in -1..=1 {
        for y in -2..=2 {
            for x in -1..=1 {
                let p = off(o, x, y, z);
                let sx = x == 0;
                let sy = y == 0;
                let sz = z == 0;
                let far = y.abs() == 2;
                if sx && sy && sz {
                    lv.set(p, st("end_gateway"));
                    lv.set_block_entity(p, gateway_nbt(exit, exact));
                } else if sy {
                    lv.set(p, air);
                } else if far && sx && sz {
                    lv.set(p, bedrock);
                } else if (sx || sz) && !far {
                    lv.set(p, bedrock);
                } else {
                    lv.set(p, air);
                }
            }
        }
    }
    true
}

/// `TheEndGatewayBlockEntity` NBT
pub fn gateway_nbt(exit: Option<P>, exact: bool) -> crate::nbt::Tag {
    use crate::nbt::Tag;
    let mut c = vec![("id".to_string(), Tag::String("minecraft:end_gateway".into())), ("Age".to_string(), Tag::Long(0))];
    if let Some(e) = exit {
        c.push(("exit_portal".to_string(), Tag::IntArray(vec![e[0], e[1], e[2]])));
    }
    if exact {
        c.push(("ExactTeleport".to_string(), Tag::Byte(1)));
    }
    Tag::Compound(c)
}

/// `SpikeFeature.getSpikesForLevel` (`SpikeCacheLoader`)
pub fn spikes_for_seed(seed: i64) -> Vec<Spike> {
    let key = Legacy::new(seed).next_long() & 65535;
    let mut r = Legacy::new(key);
    let mut list: Vec<i32> = (0..10).collect();
    // Util.shuffle
    let mut i = list.len();
    while i > 1 {
        let j = r.next_int_bound(i as i32) as usize;
        list.swap(i - 1, j);
        i -= 1;
    }
    (0..10)
        .map(|i| {
            let a = 2.0 * (-std::f64::consts::PI + std::f64::consts::PI / 10.0 * i as f64);
            let k = list[i];
            Spike { x: (42.0 * a.cos()).floor() as i32, z: (42.0 * a.sin()).floor() as i32, radius: 2 + k / 3, height: 76 + k * 3, guarded: k == 1 || k == 2 }
        })
        .collect()
}

fn end_spikes(lv: &mut Region, r: &mut WgRandom, o: P, invulnerable: bool, cfg: &[Spike]) -> bool {
    let spikes = if cfg.is_empty() { spikes_for_seed(lv.seed) } else { cfg.to_vec() };
    for s in spikes {
        if s.x >> 4 == o[0] >> 4 && s.z >> 4 == o[2] >> 4 {
            place_spike(lv, r, s, invulnerable);
        }
    }
    true
}

fn place_spike(lv: &mut Region, r: &mut WgRandom, s: Spike, _invulnerable: bool) {
    let rad = s.radius;
    let obsidian = st("obsidian");
    let air = crate::registry::blocks::AIR;
    for z in s.z - rad..=s.z + rad {
        for y in lv.lib.dim_min..=s.height + 10 {
            for x in s.x - rad..=s.x + rad {
                let (dx, dz) = ((x - s.x) as f64, (z - s.z) as f64);
                let p = [x, y, z];
                if dx * dx + dz * dz <= (rad * rad + 1) as f64 && y < s.height {
                    lv.set(p, obsidian);
                } else if y > 65 {
                    lv.set(p, air);
                }
            }
        }
    }
    if s.guarded {
        let bars = st("iron_bars");
        for dx in -2i32..=2 {
            for dz in -2i32..=2 {
                for dy in 0..=3 {
                    let ex = dx.abs() == 2;
                    let ez = dz.abs() == 2;
                    let top = dy == 3;
                    if ex || ez || top {
                        let wx = dx == -2 || dx == 2 || top;
                        let wz = dz == -2 || dz == 2 || top;
                        let b = |v: bool| if v { "true" } else { "false" };
                        let mut st_ = with(bars, "north", b(wx && dz != -2));
                        st_ = with(st_, "south", b(wx && dz != 2));
                        st_ = with(st_, "west", b(wz && dx != -2));
                        st_ = with(st_, "east", b(wz && dx != 2));
                        lv.set([s.x + dx, s.height + dy, s.z + dz], st_);
                    }
                }
            }
        }
    }
    // the end crystal (its yaw uses one random float)
    let _yaw = r.next_float() * 360.0;
    let top = [s.x, s.height + 1, s.z];
    lv.spawn_entity("end_crystal", top);
    lv.set(below(top), st("bedrock"));
    lv.set(top, st("fire"));
}

/// `ChorusPlantBlock.getStateWithConnections`
fn chorus_connections(lv: &Region, p: P, s: BlockState) -> BlockState {
    if !is(s, "chorus_plant") {
        return s;
    }
    let plant = |q: P| matches!(lv.get(q).name(), "chorus_plant" | "chorus_flower");
    let b = |v: bool| if v { "true" } else { "false" };
    let mut out = with(s, "down", b(plant(below(p)) || is(lv.get(below(p)), "end_stone")));
    out = with(out, "up", b(plant(above(p))));
    out = with(out, "north", b(plant(off(p, 0, 0, -1))));
    out = with(out, "east", b(plant(off(p, 1, 0, 0))));
    out = with(out, "south", b(plant(off(p, 0, 0, 1))));
    with(out, "west", b(plant(off(p, -1, 0, 0))))
}

/// `ChorusFlowerBlock.generatePlant`
pub fn chorus_generate(lv: &mut Region, p: P, r: &mut WgRandom, spread: i32) {
    let s = chorus_connections(lv, p, st("chorus_plant"));
    lv.set(p, s);
    chorus_grow(lv, p, r, p, spread, 0);
}

fn chorus_neighbors_empty(lv: &Region, p: P, ignore: Option<usize>) -> bool {
    HORIZ.iter().enumerate().all(|(i, d)| Some(i) == ignore || lv.is_empty(off(p, d[0], d[1], d[2])))
}

fn chorus_grow(lv: &mut Region, cur: P, r: &mut WgRandom, root: P, spread: i32, depth: i32) {
    let mut h = r.next_int_bound(4) + 1;
    if depth == 0 {
        h += 1;
    }
    for i in 0..h {
        let p = off(cur, 0, i + 1, 0);
        if !chorus_neighbors_empty(lv, p, None) {
            return;
        }
        let s = chorus_connections(lv, p, st("chorus_plant"));
        lv.set(p, s);
        let bp = below(p);
        let bs = chorus_connections(lv, bp, lv.get(bp));
        lv.set(bp, bs);
    }
    let mut grew = false;
    if depth < 4 {
        let mut n = r.next_int_bound(4);
        if depth == 0 {
            n += 1;
        }
        for _ in 0..n {
            let di = r.next_int_bound(4) as usize;
            let d = HORIZ[di];
            let p = off(off(cur, 0, h, 0), d[0], 0, d[2]);
            let opp = (di + 2) % 4;
            if (p[0] - root[0]).abs() < spread && (p[2] - root[2]).abs() < spread && lv.is_empty(p) && lv.is_empty(below(p)) && chorus_neighbors_empty(lv, p, Some(opp)) {
                grew = true;
                let s = chorus_connections(lv, p, st("chorus_plant"));
                lv.set(p, s);
                let back = off(p, -d[0], 0, -d[2]);
                let bs = chorus_connections(lv, back, lv.get(back));
                lv.set(back, bs);
                chorus_grow(lv, p, r, root, spread, depth + 1);
            }
        }
    }
    if !grew {
        lv.set(off(cur, 0, h, 0), with(st("chorus_flower"), "age", "5"));
    }
}
