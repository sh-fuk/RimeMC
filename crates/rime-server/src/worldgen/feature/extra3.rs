//! Icebergs, geodes, dripstone, fallen trees and desert wells (ported from
//! the 26.1 bytecode).

use anyhow::Result;
use serde_json::Value;

use super::extra::{add, column_scan, is, is_water_block, neg, prop, st, up, with, P, ALL, DOWN, HORIZONTAL, UP};
use super::extra2::FloatP;
use super::place::{parse_state, BlockSet, IntP, StateP};
use super::tree::{shuffle, JSet};
use super::{Builder, Hm, Region};
use crate::registry::{blocks, vstate, BlockState};
use crate::worldgen::carver::{mcos, msin};
use crate::worldgen::chunk::MIN_Y;
use crate::worldgen::noise::NormalNoise;
use crate::worldgen::random::{Rng, WgRandom};

fn f32v(v: &Value, k: &str, d: f32) -> f32 {
    v[k].as_f64().map(|x| x as f32).unwrap_or(d)
}
fn f64v(v: &Value, k: &str, d: f64) -> f64 {
    v[k].as_f64().unwrap_or(d)
}
fn i32v(v: &Value, k: &str, d: i32) -> i32 {
    v[k].as_i64().map(|x| x as i32).unwrap_or(d)
}

/// `Mth.ceil(float)`
fn ceil(f: f32) -> i32 {
    let i = f as i32;
    if f > i as f32 {
        i + 1
    } else {
        i
    }
}

fn inv_sqrt(d: f64) -> f64 {
    1.0 / d.sqrt()
}

fn dist_sqr(a: P, b: P) -> f64 {
    let (x, y, z) = ((a[0] - b[0]) as f64, (a[1] - b[1]) as f64, (a[2] - b[2]) as f64);
    x * x + y * y + z * z
}

pub struct Geode {
    filling: StateP,
    inner: StateP,
    alternate: StateP,
    middle: StateP,
    outer: StateP,
    placements: Vec<BlockState>,
    cannot_replace: BlockSet,
    invalid: BlockSet,
    layers: [f64; 4],
    crack_chance: f64,
    crack_size: f64,
    crack_offset: i32,
    noise_mul: f64,
    alt_chance: f64,
    potential_chance: f64,
    require_alt: bool,
    outer_wall: IntP,
    distribution: IntP,
    point_offset: IntP,
    min_off: i32,
    max_off: i32,
    invalid_threshold: i32,
}

pub struct Cluster {
    range: i32,
    height: IntP,
    radius: IntP,
    max_diff: i32,
    height_dev: i32,
    thickness: IntP,
    density: FloatP,
    wetness: FloatP,
    chance_at_max: f32,
    edge_dist: i32,
    center_dist: i32,
}

pub struct Large {
    range: i32,
    col_radius: IntP,
    col_min: i32,
    col_max: i32,
    height_scale: FloatP,
    ratio: f32,
    stalactite_blunt: FloatP,
    stalagmite_blunt: FloatP,
    wind_speed: FloatP,
    min_radius_wind: i32,
    min_blunt_wind: f32,
}

pub struct Fallen {
    trunk: StateP,
    length: IntP,
    stump_decos: Vec<FallenDeco>,
    log_decos: Vec<FallenDeco>,
}

pub enum FallenDeco {
    TrunkVine,
    AttachedToLogs { prob: f32, provider: StateP, dirs: Vec<P> },
}

pub enum Extra3 {
    Iceberg(BlockState),
    Geode(Box<Geode>),
    Pointed { dir: f32, r2: f32, r3: f32, taller: f32 },
    Cluster(Box<Cluster>),
    Large(Box<Large>),
    Fallen(Box<Fallen>),
    DesertWell,
    Other(Box<super::extra4::Extra4>),
}

impl Extra3 {
    pub fn parse(b: &mut Builder, ty: &str, c: &Value) -> Option<Result<Extra3>> {
        let r = (|| -> Result<Option<Extra3>> {
            Ok(Some(match ty {
                "iceberg" => Extra3::Iceberg(parse_state(&c["state"])?),
                "geode" => {
                    let bl = &c["blocks"];
                    let l = &c["layers"];
                    let cr = &c["crack"];
                    let mut placements = Vec::new();
                    for p in bl["inner_placements"].as_array().into_iter().flatten() {
                        placements.push(parse_state(p)?);
                    }
                    let outer_wall = IntP::parse(&c["outer_wall_distance"])?;
                    Extra3::Geode(Box::new(Geode {
                        filling: StateP::parse(b, &bl["filling_provider"])?,
                        inner: StateP::parse(b, &bl["inner_layer_provider"])?,
                        alternate: StateP::parse(b, &bl["alternate_inner_layer_provider"])?,
                        middle: StateP::parse(b, &bl["middle_layer_provider"])?,
                        outer: StateP::parse(b, &bl["outer_layer_provider"])?,
                        placements,
                        cannot_replace: b.tag(bl["cannot_replace"].as_str().unwrap_or("#minecraft:features_cannot_replace"))?,
                        invalid: b.tag(bl["invalid_blocks"].as_str().unwrap_or("#minecraft:geode_invalid_blocks"))?,
                        layers: [f64v(l, "filling", 1.7), f64v(l, "inner_layer", 2.2), f64v(l, "middle_layer", 3.2), f64v(l, "outer_layer", 4.2)],
                        crack_chance: f64v(cr, "generate_crack_chance", 1.0),
                        crack_size: f64v(cr, "base_crack_size", 2.0),
                        crack_offset: i32v(cr, "crack_point_offset", 2),
                        noise_mul: f64v(c, "noise_multiplier", 0.05),
                        alt_chance: f64v(c, "use_alternate_layer0_chance", 0.0),
                        potential_chance: f64v(c, "use_potential_placements_chance", 0.35),
                        require_alt: c["placements_require_layer0_alternate"].as_bool().unwrap_or(true),
                        outer_wall,
                        distribution: IntP::parse(&c["distribution_points"])?,
                        point_offset: IntP::parse(&c["point_offset"])?,
                        min_off: i32v(c, "min_gen_offset", -16),
                        max_off: i32v(c, "max_gen_offset", 16),
                        invalid_threshold: i32v(c, "invalid_blocks_threshold", 0),
                    }))
                }
                "pointed_dripstone" => Extra3::Pointed { dir: f32v(c, "chance_of_directional_spread", 0.7), r2: f32v(c, "chance_of_spread_radius2", 0.5), r3: f32v(c, "chance_of_spread_radius3", 0.5), taller: f32v(c, "chance_of_taller_dripstone", 0.2) },
                "dripstone_cluster" => Extra3::Cluster(Box::new(Cluster {
                    range: i32v(c, "floor_to_ceiling_search_range", 12),
                    height: IntP::parse(&c["height"])?,
                    radius: IntP::parse(&c["radius"])?,
                    max_diff: i32v(c, "max_stalagmite_stalactite_height_diff", 1),
                    height_dev: i32v(c, "height_deviation", 3),
                    thickness: IntP::parse(&c["dripstone_block_layer_thickness"])?,
                    density: FloatP::parse(&c["density"])?,
                    wetness: FloatP::parse(&c["wetness"])?,
                    chance_at_max: f32v(c, "chance_of_dripstone_column_at_max_distance_from_center", 0.1),
                    edge_dist: i32v(c, "max_distance_from_edge_affecting_chance_of_dripstone_column", 3),
                    center_dist: i32v(c, "max_distance_from_center_affecting_height_bias", 8),
                })),
                "large_dripstone" => {
                    let col_radius = IntP::parse(&c["column_radius"])?;
                    let (col_min, col_max) = match &col_radius {
                        IntP::Uniform(a, b) => (*a, *b),
                        IntP::Const(a) => (*a, *a),
                        _ => (1, col_radius.max_value()),
                    };
                    Extra3::Large(Box::new(Large {
                        range: i32v(c, "floor_to_ceiling_search_range", 30),
                        col_radius,
                        col_min,
                        col_max,
                        height_scale: FloatP::parse(&c["height_scale"])?,
                        ratio: f32v(c, "max_column_radius_to_cave_height_ratio", 0.33),
                        stalactite_blunt: FloatP::parse(&c["stalactite_bluntness"])?,
                        stalagmite_blunt: FloatP::parse(&c["stalagmite_bluntness"])?,
                        wind_speed: FloatP::parse(&c["wind_speed"])?,
                        min_radius_wind: i32v(c, "min_radius_for_wind", 4),
                        min_blunt_wind: f32v(c, "min_bluntness_for_wind", 0.6),
                    }))
                }
                "fallen_tree" => {
                    let decos = |b: &mut Builder, v: &Value| -> Result<Vec<FallenDeco>> {
                        let mut out = Vec::new();
                        for d in v.as_array().into_iter().flatten() {
                            match d["type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
                                "trunk_vine" => out.push(FallenDeco::TrunkVine),
                                "attached_to_logs" => out.push(FallenDeco::AttachedToLogs {
                                    prob: f32v(d, "probability", 0.0),
                                    provider: StateP::parse(b, &d["block_provider"])?,
                                    dirs: d["directions"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).map(super::extra2::dir_from_name).collect(),
                                }),
                                t => anyhow::bail!("fallen tree decorator {t}"),
                            }
                        }
                        Ok(out)
                    };
                    Extra3::Fallen(Box::new(Fallen {
                        trunk: StateP::parse(b, &c["trunk_provider"])?,
                        length: IntP::parse(&c["log_length"])?,
                        stump_decos: decos(b, &c["stump_decorators"])?,
                        log_decos: decos(b, &c["log_decorators"])?,
                    }))
                }
                "desert_well" => Extra3::DesertWell,
                _ => match super::extra4::Extra4::parse(b, ty, c) {
                    Some(r) => Extra3::Other(Box::new(r?)),
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
            Extra3::Iceberg(s) => iceberg(lv, r, o, *s),
            Extra3::Geode(g) => geode(lv, r, o, g),
            Extra3::Pointed { dir, r2, r3, taller } => pointed(lv, r, o, *dir, *r2, *r3, *taller),
            Extra3::Cluster(c) => cluster(lv, r, o, c),
            Extra3::Large(c) => large(lv, r, o, c),
            Extra3::Fallen(f) => fallen(lv, r, o, f),
            Extra3::DesertWell => desert_well(lv, r, o),
            Extra3::Other(e) => e.place(lv, r, o),
        }
    }
}

// ------------------------------------------------------------ icebergs

fn iceberg_state(s: BlockState) -> bool {
    is(s, "packed_ice") || is(s, "snow_block") || is(s, "blue_ice")
}

fn ellipse_dist(x: i32, z: i32, c: P, a: i32, b: i32, ang: f64) -> f64 {
    let (dx, dz) = ((x - c[0]) as f64, (z - c[2]) as f64);
    let u = (dx * ang.cos() - dz * ang.sin()) / a as f64;
    let v = (dx * ang.sin() + dz * ang.cos()) / b as f64;
    u.powi(2) + v.powi(2) - 1.0
}

fn circle_dist(r: &mut WgRandom, x: i32, z: i32, radius: i32) -> f64 {
    let f = 10.0 * r.next_float().clamp(0.2, 0.8) / radius as f32;
    f as f64 + (x as f64).powi(2) + (z as f64).powi(2) - (radius as f64).powi(2)
}

fn radius_round(r: &mut WgRandom, y: i32, h: i32, n: i32) -> i32 {
    let f = 3.5 - r.next_float();
    let mut g = (1.0 - ((y as f64).powi(2) as f32) / (h as f32 * f)) * n as f32;
    if h > 15 + r.next_int_bound(5) {
        let k = if y < 3 + r.next_int_bound(6) { y / 2 } else { y };
        g = (1.0 - k as f32 / (h as f32 * f * 0.4)) * n as f32;
    }
    ceil(g / 2.0)
}

fn radius_ellipse(y: i32, h: i32, n: i32) -> i32 {
    let g = (1.0 - ((y as f64).powi(2) as f32) / (h as f32 * 1.0)) * n as f32;
    ceil(g / 2.0)
}

fn radius_steep(r: &mut WgRandom, y: i32, h: i32, n: i32) -> i32 {
    let f = 1.0 + r.next_float() / 2.0;
    let g = (1.0 - y as f32 / (h as f32 * f)) * n as f32;
    ceil(g / 2.0)
}

#[allow(clippy::too_many_arguments)]
fn iceberg_block(lv: &mut Region, r: &mut WgRandom, o: P, h: i32, x: i32, y: i32, z: i32, radius: i32, m: i32, ellipse: bool, j: i32, ang: f64, snow: bool, state: BlockState) {
    let d = if ellipse {
        let mut c = j;
        if y > 0 && h - y <= 3 {
            c -= 4 - (h - y);
        }
        ellipse_dist(x, z, [0, 0, 0], m, c, ang)
    } else {
        circle_dist(r, x, z, radius)
    };
    if d >= 0.0 {
        return;
    }
    let p = add(o, [x, y, z]);
    let e = if ellipse { -0.5 } else { (-6 - r.next_int_bound(3)) as f64 };
    if d > e && r.next_double() > 0.9 {
        return;
    }
    // setIcebergBlock
    let s = lv.get(p);
    if vstate::air(s) || is(s, "snow_block") || is(s, "ice") || is_water_block(s) {
        let b = !ellipse || r.next_double() > 0.05;
        let i = if ellipse { 3 } else { 2 };
        let diff = h - y;
        if snow && !is_water_block(s) && (diff as f64) <= r.next_int_bound((h / i).max(1)) as f64 + h as f64 * 0.6 && b {
            lv.set(p, st("snow_block"));
        } else {
            lv.set(p, state);
        }
    }
}

fn iceberg(lv: &mut Region, r: &mut WgRandom, o: P, state: BlockState) -> bool {
    let o = [o[0], lv.lib.sea_level, o[2]];
    let snow = r.next_double() > 0.7;
    let ang = r.next_double() * 2.0 * std::f64::consts::PI;
    let i = 11 - r.next_int_bound(5);
    let j = 3 + r.next_int_bound(3);
    let ellipse = r.next_double() > 0.7;
    let mut l = if ellipse { r.next_int_bound(6) + 6 } else { r.next_int_bound(15) + 3 };
    if !ellipse && r.next_double() > 0.9 {
        l += r.next_int_bound(19) + 7;
    }
    let m = (l + r.next_int_bound(11)).min(18);
    let n = (l + r.next_int_bound(7) - r.next_int_bound(5)).min(11);
    let w = if ellipse { i } else { 11 };
    for x in -w..w {
        for z in -w..w {
            for y in 0..l {
                let s = if ellipse { radius_ellipse(y, l, n) } else { radius_round(r, y, l, n) };
                if ellipse || x < s {
                    iceberg_block(lv, r, o, l, x, y, z, s, w, ellipse, j, ang, snow, state);
                }
            }
        }
    }
    // smooth
    let k = if ellipse { i } else { n / 2 };
    for x in -k..=k {
        for z in -k..=k {
            for y in 0..=l {
                let p = add(o, [x, y, z]);
                let s = lv.get(p);
                if !(iceberg_state(s) || is(s, "snow")) {
                    continue;
                }
                if vstate::air(lv.get(up(p, -1))) {
                    lv.set(p, blocks::AIR);
                    lv.set(up(p, 1), blocks::AIR);
                } else if iceberg_state(s) {
                    let around = [[-1, 0, 0], [1, 0, 0], [0, 0, -1], [0, 0, 1]];
                    let c = around.iter().filter(|d| !iceberg_state(lv.get(add(p, **d)))).count();
                    if c >= 3 {
                        lv.set(p, blocks::AIR);
                    }
                }
            }
        }
    }
    for x in -w..w {
        for z in -w..w {
            let mut y = -1;
            while y > -m {
                let s = if ellipse { ceil(w as f32 * (1.0 - ((y as f64).powi(2) as f32) / (m as f32 * 8.0))) } else { w };
                let t = radius_steep(r, -y, m, n);
                if x < t {
                    iceberg_block(lv, r, o, m, x, y, z, t, s, ellipse, j, ang, snow, state);
                }
                y -= 1;
            }
        }
    }
    let cut = if ellipse { r.next_double() > 0.1 } else { r.next_double() > 0.7 };
    if cut {
        cut_out(lv, r, n, l, o, ellipse, i, ang, j);
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn cut_out(lv: &mut Region, r: &mut WgRandom, n: i32, l: i32, o: P, ellipse: bool, i: i32, ang: f64, j: i32) {
    let a = if r.next_bool() { -1 } else { 1 };
    let b = if r.next_bool() { -1 } else { 1 };
    let mut c = r.next_int_bound((n / 2 - 2).max(1));
    if r.next_bool() {
        c = n / 2 + 1 - r.next_int_bound((n - n / 2 - 1).max(1));
    }
    let mut e = r.next_int_bound((n / 2 - 2).max(1));
    if r.next_bool() {
        e = n / 2 + 1 - r.next_int_bound((n - n / 2 - 1).max(1));
    }
    if ellipse {
        let v = r.next_int_bound((i - 5).max(1));
        c = v;
        e = v;
    }
    let off = [a * c, 0, b * e];
    let ang = if ellipse { ang + std::f64::consts::FRAC_PI_2 } else { r.next_double() * 2.0 * std::f64::consts::PI };
    for y in 0..l - 3 {
        let rr = radius_round(r, y, l, n);
        carve(lv, rr, y, o, false, ang, off, i, j);
    }
    let mut y = -1;
    while y > -l + r.next_int_bound(5) {
        let rr = radius_steep(r, -y, l, n);
        carve(lv, rr, y, o, true, ang, off, i, j);
        y -= 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn carve(lv: &mut Region, rr: i32, y: i32, o: P, under: bool, ang: f64, off: P, i: i32, j: i32) {
    let a = rr + 1 + i / 3;
    let c = (rr - 3).min(3) + j / 2 - 1;
    for x in -a..a {
        for z in -a..a {
            if ellipse_dist(x, z, off, a, c, ang) >= 0.0 {
                continue;
            }
            let p = add(o, [x, y, z]);
            let s = lv.get(p);
            if iceberg_state(s) || is(s, "snow_block") {
                if under {
                    lv.set(p, blocks::WATER);
                } else {
                    lv.set(p, blocks::AIR);
                    if is(lv.get(up(p, 1)), "snow") {
                        lv.set(up(p, 1), blocks::AIR);
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------ geodes

fn geode(lv: &mut Region, r: &mut WgRandom, o: P, g: &Geode) -> bool {
    let cannot = &g.cannot_replace;
    let safe_set = |lv: &mut Region, p: P, s: BlockState| {
        if !cannot.contains(lv.get(p)) {
            lv.set(p, s);
        }
    };
    let mut points: Vec<(P, i32)> = Vec::new();
    let n = g.distribution.sample(r);
    let noise = NormalNoise::new_legacy(lv.seed, -4, &[1.0]);
    let mut cracks: Vec<P> = Vec::new();
    let d = n as f64 / g.outer_wall.max_value() as f64;
    let e = 1.0 / g.layers[0].sqrt();
    let f = 1.0 / (g.layers[1] + d).sqrt();
    let gg = 1.0 / (g.layers[2] + d).sqrt();
    let h = 1.0 / (g.layers[3] + d).sqrt();
    let l = 1.0 / (g.crack_size + r.next_double() / 2.0 + if n > 3 { d } else { 0.0 }).sqrt();
    let crack = (r.next_float() as f64) < g.crack_chance;
    let mut invalid = 0;
    for _ in 0..n {
        let a = g.outer_wall.sample(r);
        let b = g.outer_wall.sample(r);
        let c = g.outer_wall.sample(r);
        let p = add(o, [a, b, c]);
        let s = lv.get(p);
        if vstate::air(s) || g.invalid.contains(s) {
            invalid += 1;
            if invalid > g.invalid_threshold {
                return false;
            }
        }
        points.push((p, g.point_offset.sample(r)));
    }
    if crack {
        let i = r.next_int_bound(4);
        let j = n * 2 + 1;
        let offs: [P; 3] = match i {
            0 => [[j, 7, 0], [j, 5, 0], [j, 1, 0]],
            1 => [[0, 7, j], [0, 5, j], [0, 1, j]],
            2 => [[j, 7, j], [j, 5, j], [j, 1, j]],
            _ => [[0, 7, 0], [0, 5, 0], [0, 1, 0]],
        };
        for q in offs {
            cracks.push(add(o, q));
        }
    }
    let mut list2: Vec<P> = Vec::new();
    let (lo, hi) = (g.min_off, g.max_off);
    for z in o[2] + lo..=o[2] + hi {
        for y in o[1] + lo..=o[1] + hi {
            for x in o[0] + lo..=o[0] + hi {
                let p = [x, y, z];
                let k = noise.value(x as f64, y as f64, z as f64) * g.noise_mul;
                let mut s = 0.0;
                let mut t = 0.0;
                for (q, off) in &points {
                    s += inv_sqrt(dist_sqr(p, *q) + *off as f64) + k;
                }
                for q in &cracks {
                    t += inv_sqrt(dist_sqr(p, *q) + g.crack_offset as f64) + k;
                }
                if s < h {
                    continue;
                }
                if crack && t >= l && s < e {
                    safe_set(lv, p, blocks::AIR);
                } else if s >= e {
                    if let Some(st) = g.filling.get(lv, r, p) {
                        safe_set(lv, p, st);
                    }
                } else if s >= f {
                    let alt = (r.next_float() as f64) < g.alt_chance;
                    let st = if alt { g.alternate.get(lv, r, p) } else { g.inner.get(lv, r, p) };
                    if let Some(st) = st {
                        safe_set(lv, p, st);
                    }
                    if (!g.require_alt || alt) && (r.next_float() as f64) < g.potential_chance {
                        list2.push(p);
                    }
                } else if s >= gg {
                    if let Some(st) = g.middle.get(lv, r, p) {
                        safe_set(lv, p, st);
                    }
                } else if s >= h {
                    if let Some(st) = g.outer.get(lv, r, p) {
                        safe_set(lv, p, st);
                    }
                }
            }
        }
    }
    for p in list2 {
        let mut s = g.placements[r.next_int_bound(g.placements.len() as i32) as usize];
        for d in ALL {
            if prop(s, "facing").is_some() {
                s = with(s, "facing", super::extra::face_name(d));
            }
            let q = add(p, d);
            let ns = lv.get(q);
            if prop(s, "waterlogged").is_some() {
                s = with(s, "waterlogged", if vstate::fluid_source(ns) { "true" } else { "false" });
            }
            // BuddingAmethystBlock.canClusterGrowAtState
            if vstate::air(ns) || is_water_block(ns) && vstate::fluid(ns) == 1 {
                safe_set(lv, q, s);
                break;
            }
        }
    }
    true
}

// ------------------------------------------------------------ dripstone

fn empty_or_water(s: BlockState) -> bool {
    vstate::air(s) || is_water_block(s)
}
fn neither_empty_nor_water(s: BlockState) -> bool {
    !empty_or_water(s)
}
fn empty_water_lava(s: BlockState) -> bool {
    vstate::air(s) || is_water_block(s) || is(s, "lava")
}
fn dripstone_base(lv: &Region, s: BlockState) -> bool {
    is(s, "dripstone_block") || lv.lib.extra_tags.dripstone_replaceable.contains(s)
}

fn place_dripstone_block(lv: &mut Region, p: P) -> bool {
    if lv.lib.extra_tags.dripstone_replaceable.contains(lv.get(p)) {
        lv.set(p, st("dripstone_block"));
        true
    } else {
        false
    }
}

fn pointed_state(dir: P, thickness: &str) -> BlockState {
    with(with(st("pointed_dripstone"), "vertical_direction", if dir == UP { "up" } else { "down" }), "thickness", thickness)
}

/// `DripstoneUtils.growPointedDripstone`
fn grow_pointed(lv: &mut Region, p: P, dir: P, h: i32, merge: bool) {
    if !dripstone_base(lv, lv.get(add(p, neg(dir)))) {
        return;
    }
    let mut list: Vec<&str> = Vec::new();
    if h >= 3 {
        list.push("base");
        for _ in 0..h - 3 {
            list.push("middle");
        }
    }
    if h >= 2 {
        list.push("frustum");
    }
    if h >= 1 {
        list.push(if merge { "tip_merge" } else { "tip" });
    }
    let mut m = p;
    for t in list {
        let mut s = pointed_state(dir, t);
        s = with(s, "waterlogged", if vstate::is_water(lv.get(m)) { "true" } else { "false" });
        lv.set(m, s);
        m = add(m, dir);
    }
}

#[allow(clippy::too_many_arguments)]
fn pointed(lv: &mut Region, r: &mut WgRandom, o: P, cd: f32, c2: f32, c3: f32, taller: f32) -> bool {
    let a = dripstone_base(lv, lv.get(up(o, 1)));
    let b = dripstone_base(lv, lv.get(up(o, -1)));
    let tip = if a && b {
        if r.next_bool() {
            DOWN
        } else {
            UP
        }
    } else if a {
        DOWN
    } else if b {
        UP
    } else {
        return false;
    };
    let base = add(o, neg(tip));
    place_dripstone_block(lv, base);
    for d in HORIZONTAL {
        if r.next_float() > cd {
            continue;
        }
        let p6 = add(base, d);
        place_dripstone_block(lv, p6);
        if r.next_float() > c2 {
            continue;
        }
        let p7 = add(p6, ALL[r.next_int_bound(6) as usize]);
        place_dripstone_block(lv, p7);
        if r.next_float() > c3 {
            continue;
        }
        let p8 = add(p7, ALL[r.next_int_bound(6) as usize]);
        place_dripstone_block(lv, p8);
    }
    let h = if r.next_float() < taller && empty_or_water(lv.get(add(o, tip))) { 2 } else { 1 };
    grow_pointed(lv, o, tip, h, false);
    true
}

/// `Mth.clampedMap` in floats
fn clamped_map_f(v: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    let t = (v - a) / (b - a);
    if t < 0.0 {
        c
    } else if t > 1.0 {
        d
    } else {
        c + t * (d - c)
    }
}

fn cluster_height(r: &mut WgRandom, x: i32, z: i32, density: f32, lim: i32, c: &Cluster) -> i32 {
    if r.next_float() > density {
        return 0;
    }
    let i = x.abs() + z.abs();
    let f = crate::worldgen::noise::clamped_map(i as f64, 0.0, c.center_dist as f64, lim as f64 / 2.0, 0.0) as f32;
    // ClampedNormalFloat.sample(random, mean, deviation, min, max)
    let n = f + r.next_gaussian() as f32 * c.height_dev as f32;
    n.clamp(0.0, lim as f32) as i32
}

fn can_be_adjacent_to_water(lv: &Region, p: P) -> bool {
    let s = lv.get(p);
    lv.lib.extra_tags.base_stone_overworld.contains(s) || vstate::is_water(s)
}

fn can_place_pool(lv: &Region, p: P) -> bool {
    let s = lv.get(p);
    if is_water_block(s) || is(s, "dripstone_block") || is(s, "pointed_dripstone") {
        return false;
    }
    if vstate::is_water(lv.get(up(p, 1))) {
        return false;
    }
    for d in HORIZONTAL {
        if !can_be_adjacent_to_water(lv, add(p, d)) {
            return false;
        }
    }
    can_be_adjacent_to_water(lv, up(p, -1))
}

fn replace_with_dripstone(lv: &mut Region, p: P, n: i32, dir: P) {
    let mut m = p;
    for _ in 0..n {
        if !place_dripstone_block(lv, m) {
            return;
        }
        m = add(m, dir);
    }
}

#[allow(clippy::too_many_arguments)]
fn cluster_column(lv: &mut Region, r: &mut WgRandom, p: P, x: i32, z: i32, wetness: f32, chance: f64, height: i32, density: f32, c: &Cluster) {
    let Some((floor0, ceil)) = column_scan(lv, p, c.range, &empty_or_water, &neither_empty_nor_water) else { return };
    if ceil.is_none() && floor0.is_none() {
        return;
    }
    let wet = r.next_float() < wetness;
    let floor = match floor0 {
        Some(f) if wet && can_place_pool(lv, [p[0], f, p[2]]) => {
            lv.set([p[0], f, p[2]], blocks::WATER);
            Some(f - 1)
        }
        _ => floor0,
    };
    let col_height = match (floor, ceil) {
        (Some(f), Some(c)) => Some(c - f - 1),
        _ => None,
    };
    let b = r.next_double() < chance;
    let tip_h = match ceil {
        Some(cy) if b && !is(lv.get([p[0], cy, p[2]]), "lava") => {
            let t = c.thickness.sample(r);
            replace_with_dripstone(lv, [p[0], cy, p[2]], t, UP);
            let lim = match floor {
                Some(f) => height.min(cy - f),
                None => height,
            };
            cluster_height(r, x, z, density, lim, c)
        }
        _ => 0,
    };
    let b2 = r.next_double() < chance;
    let mite_h = match floor {
        Some(fy) if b2 && !is(lv.get([p[0], fy, p[2]]), "lava") => {
            let t = c.thickness.sample(r);
            replace_with_dripstone(lv, [p[0], fy, p[2]], t, DOWN);
            if ceil.is_some() {
                0.max(tip_h + r.next_int_bound(2 * c.max_diff + 1) - c.max_diff)
            } else {
                cluster_height(r, x, z, density, height, c)
            }
        }
        _ => 0,
    };
    let (th, mh) = match (ceil, floor) {
        (Some(cy), Some(fy)) if cy - tip_h <= fy + mite_h => {
            let a = (cy - tip_h).max(fy + 1);
            let b = (fy + mite_h).min(cy - 1);
            let cc = r.next_int_bound(b + 1 - a + 1) + a;
            let dd = cc - 1;
            (cy - cc, dd - fy)
        }
        _ => (tip_h, mite_h),
    };
    let merge = r.next_bool() && th > 0 && mh > 0 && col_height.is_some_and(|h| th + mh == h);
    if let Some(cy) = ceil {
        grow_pointed(lv, [p[0], cy - 1, p[2]], DOWN, th, merge);
    }
    if let Some(fy) = floor {
        grow_pointed(lv, [p[0], fy + 1, p[2]], UP, mh, merge);
    }
}

fn cluster(lv: &mut Region, r: &mut WgRandom, o: P, c: &Cluster) -> bool {
    if !empty_or_water(lv.get(o)) {
        return false;
    }
    let height = c.height.sample(r);
    let wetness = c.wetness.sample(r);
    let density = c.density.sample(r);
    let rx = c.radius.sample(r);
    let rz = c.radius.sample(r);
    for x in -rx..=rx {
        for z in -rz..=rz {
            let i = rx - x.abs();
            let j = rz - z.abs();
            let k = i.min(j);
            let chance = clamped_map_f(k as f32, 0.0, c.edge_dist as f32, c.chance_at_max, 1.0) as f64;
            cluster_column(lv, r, add(o, [x, 0, z]), x, z, wetness, chance, height, density, c);
        }
    }
    true
}

struct LargeD {
    root: P,
    up: bool,
    radius: i32,
    blunt: f64,
    scale: f64,
}

/// `DripstoneUtils.getDripstoneHeight`
fn dripstone_height(d: f64, e: f64, f: f64, g: f64) -> f64 {
    let d = if d < g { g } else { d };
    let i = d / e * 0.384;
    let j = 0.75 * i.powf(1.3333333333333333);
    let k = i.powf(0.6666666666666666);
    let l = 0.3333333333333333 * i.ln();
    let m = (f * (j - k - l)).max(0.0);
    m / 0.384 * e
}

impl LargeD {
    fn height_at(&self, f: f32) -> i32 {
        dripstone_height(f as f64, self.radius as f64, self.scale, self.blunt) as i32
    }
}

struct Wind {
    origin_y: i32,
    speed: Option<(f64, f64)>,
}

impl Wind {
    fn offset(&self, p: P) -> P {
        let Some((x, z)) = self.speed else { return p };
        let i = (self.origin_y - p[1]) as f64;
        [p[0] + (x * i).floor() as i32, p[1], p[2] + (z * i).floor() as i32]
    }
}

fn circle_embedded(lv: &Region, p: P, radius: i32) -> bool {
    if empty_water_lava(lv.get(p)) {
        return false;
    }
    let g = 6.0f32 / radius as f32;
    let mut h = 0.0f32;
    while h < 6.2831855 {
        let i = (mcos(h as f64) * radius as f32) as i32;
        let j = (msin(h as f64) * radius as f32) as i32;
        if empty_water_lava(lv.get(add(p, [i, 0, j]))) {
            return false;
        }
        h += g;
    }
    true
}

fn move_back(lv: &Region, d: &mut LargeD, w: &Wind) -> bool {
    while d.radius > 1 {
        let mut m = d.root;
        let n = 10.min(d.height_at(0.0));
        for _ in 0..n {
            if is(lv.get(m), "lava") {
                return false;
            }
            if circle_embedded(lv, w.offset(m), d.radius) {
                d.root = m;
                return true;
            }
            m = up(m, if d.up { -1 } else { 1 });
        }
        d.radius /= 2;
    }
    false
}

fn place_large(lv: &mut Region, r: &mut WgRandom, d: &LargeD, w: &Wind) {
    for i in -d.radius..=d.radius {
        for j in -d.radius..=d.radius {
            let f = ((i * i + j * j) as f32).sqrt();
            if f > d.radius as f32 {
                continue;
            }
            let mut k = d.height_at(f);
            if k <= 0 {
                continue;
            }
            if (r.next_float() as f64) < 0.2 {
                k = (k as f32 * (r.next_float() * (1.0f32 - 0.8f32) + 0.8)) as i32;
            }
            let mut m = add(d.root, [i, 0, j]);
            let mut placed = false;
            let max_y = if d.up { lv.height(Hm::NotAir, m[0], m[2]) } else { i32::MAX };
            for _ in 0..k {
                if m[1] >= max_y {
                    break;
                }
                let p = w.offset(m);
                if empty_water_lava(lv.get(p)) {
                    placed = true;
                    lv.set(p, st("dripstone_block"));
                } else if placed && lv.lib.extra_tags.base_stone_overworld.contains(lv.get(p)) {
                    break;
                }
                m = up(m, if d.up { 1 } else { -1 });
            }
        }
    }
}

fn large(lv: &mut Region, r: &mut WgRandom, o: P, c: &Large) -> bool {
    if !empty_or_water(lv.get(o)) {
        return false;
    }
    let Some((Some(floor), Some(ceil))) = column_scan(lv, o, c.range, &empty_or_water, &neither_empty_nor_water) else { return false };
    let height = ceil - floor - 1;
    if height < 4 {
        return false;
    }
    let i = (height as f32 * c.ratio) as i32;
    let j = i.clamp(c.col_min, c.col_max);
    let k = r.next_int_bound(j - c.col_min + 1) + c.col_min;
    let _ = &c.col_radius;
    let mk = |r: &mut WgRandom, root: P, up: bool, blunt: &FloatP| {
        let b = blunt.sample(r) as f64;
        let s = c.height_scale.sample(r) as f64;
        LargeD { root, up, radius: k, blunt: b, scale: s }
    };
    let mut tite = mk(r, [o[0], ceil - 1, o[2]], false, &c.stalactite_blunt);
    let mut mite = mk(r, [o[0], floor + 1, o[2]], true, &c.stalagmite_blunt);
    let suitable = |d: &LargeD| d.radius >= c.min_radius_wind && d.blunt >= c.min_blunt_wind as f64;
    let wind = if suitable(&tite) && suitable(&mite) {
        let f = c.wind_speed.sample(r);
        let g = r.next_float() * std::f32::consts::PI;
        Wind { origin_y: o[1], speed: Some(((mcos(g as f64) * f) as f64, (msin(g as f64) * f) as f64)) }
    } else {
        Wind { origin_y: 0, speed: None }
    };
    let b1 = move_back(lv, &mut tite, &wind);
    let b2 = move_back(lv, &mut mite, &wind);
    if b1 {
        place_large(lv, r, &tite, &wind);
    }
    if b2 {
        place_large(lv, r, &mite, &wind);
    }
    true
}

// ------------------------------------------------------------ fallen trees

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

fn valid_tree_pos(lv: &Region, p: P) -> bool {
    let s = lv.get(p);
    vstate::air(s) || lv.lib.extra_tags.replaceable_by_trees.contains(s)
}

fn over_solid(lv: &Region, p: P) -> bool {
    vstate::sturdy(lv.get(up(p, -1)), 1)
}

fn fallen_decorate(lv: &mut Region, r: &mut WgRandom, logs: &[P], decos: &[FallenDeco]) {
    for d in decos {
        match d {
            FallenDeco::TrunkVine => {
                for &l in logs {
                    for (dir, face) in [([-1, 0, 0], "east"), ([1, 0, 0], "west"), ([0, 0, -1], "south"), ([0, 0, 1], "north")] {
                        if r.next_int_bound(3) > 0 {
                            let p = add(l, dir);
                            if vstate::air(lv.get(p)) {
                                lv.set(p, with(st("vine"), face, "true"));
                            }
                        }
                    }
                }
            }
            FallenDeco::AttachedToLogs { prob, provider, dirs } => {
                let mut list = logs.to_vec();
                shuffle(&mut list, r);
                for l in list {
                    let d = dirs[r.next_int_bound(dirs.len() as i32) as usize];
                    let p = add(l, d);
                    if r.next_float() <= *prob && vstate::air(lv.get(p)) {
                        if let Some(s) = provider.get(lv, r, p) {
                            lv.set(p, s);
                        }
                    }
                }
            }
        }
    }
}

fn fallen_log(lv: &mut Region, r: &mut WgRandom, f: &Fallen, p: P, axis: Option<&str>) -> P {
    let mut s = f.trunk.get(lv, r, p).unwrap_or(blocks::AIR);
    if let Some(a) = axis {
        s = with(s, "axis", a);
    }
    lv.set(p, s);
    mark_above(lv, p);
    p
}

fn fallen(lv: &mut Region, r: &mut WgRandom, o: P, f: &Fallen) -> bool {
    let stump = fallen_log(lv, r, f, o, None);
    if !f.stump_decos.is_empty() {
        fallen_decorate(lv, r, &[stump], &f.stump_decos);
    }
    let dir = HORIZONTAL[r.next_int_bound(4) as usize];
    let len = f.length.sample(r) - 2;
    let k = 2 + r.next_int_bound(2);
    let mut m = add(o, [dir[0] * k, 0, dir[2] * k]);
    // setGroundHeightForFallenLogStartPos
    m = up(m, 1);
    for _ in 0..6 {
        if valid_tree_pos(lv, m) && over_solid(lv, m) {
            break;
        }
        m = up(m, -1);
    }
    // canPlaceEntireFallenLog
    let mut c = m;
    let mut gap = 0;
    for _ in 0..len {
        if !valid_tree_pos(lv, c) {
            return true;
        }
        if over_solid(lv, c) {
            gap = 0;
        } else {
            gap += 1;
            if gap > 2 {
                return true;
            }
        }
        c = add(c, dir);
    }
    let axis = if dir[0] != 0 { "x" } else { "z" };
    let mut set = JSet::default();
    for _ in 0..len {
        set.add(fallen_log(lv, r, f, m, Some(axis)));
        m = add(m, dir);
    }
    if !f.log_decos.is_empty() {
        let mut logs: Vec<P> = set.iter().collect();
        logs.sort_by_key(|p| p[1]);
        fallen_decorate(lv, r, &logs, &f.log_decos);
    }
    true
}

// ------------------------------------------------------------ desert wells

fn desert_well(lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
    let mut p = up(o, 1);
    while lv.is_empty(p) && p[1] > MIN_Y + 2 {
        p = up(p, -1);
    }
    if !is(lv.get(p), "sand") {
        return false;
    }
    for i in -2..=2 {
        for j in -2..=2 {
            if lv.is_empty(add(p, [i, -1, j])) && lv.is_empty(add(p, [i, -2, j])) {
                return false;
            }
        }
    }
    let sandstone = st("sandstone");
    let slab = st("sandstone_slab");
    let sand = st("sand");
    for y in -2..=0 {
        for x in -2..=2 {
            for z in -2..=2 {
                lv.set(add(p, [x, y, z]), sandstone);
            }
        }
    }
    lv.set(p, blocks::WATER);
    for d in HORIZONTAL {
        lv.set(add(p, d), blocks::WATER);
    }
    let b = up(p, -1);
    lv.set(b, sand);
    for d in HORIZONTAL {
        lv.set(add(b, d), sand);
    }
    for x in -2..=2 {
        for z in -2..=2 {
            if x == -2 || x == 2 || z == -2 || z == 2 {
                lv.set(add(p, [x, 1, z]), sandstone);
            }
        }
    }
    for q in [[2, 1, 0], [-2, 1, 0], [0, 1, 2], [0, 1, -2]] {
        lv.set(add(p, q), slab);
    }
    for x in -1..=1 {
        for z in -1..=1 {
            lv.set(add(p, [x, 4, z]), if x == 0 && z == 0 { sandstone } else { slab });
        }
    }
    for y in 1..=3 {
        for q in [[-1, y, -1], [-1, y, 1], [1, y, -1], [1, y, 1]] {
            lv.set(add(p, q), sandstone);
        }
    }
    let list = [add(p, [1, 0, 0]), add(p, [0, 0, 1]), add(p, [-1, 0, 0]), add(p, [0, 0, -1])];
    let a = list[r.next_int_bound(4) as usize];
    lv.set(up(a, -1), st("suspicious_sand"));
    let c = list[r.next_int_bound(4) as usize];
    lv.set(up(c, -2), st("suspicious_sand"));
    true
}
