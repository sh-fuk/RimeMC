//! Vanilla carvers (`CaveWorldCarver`, `CanyonWorldCarver`), ported from
//! the 26.1 bytecode. Every float/double step mirrors vanilla so the carved
//! blocks are bit-identical.

use std::collections::HashSet;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::chunk::{Aquifer, HEIGHT, MIN_Y};
use super::density::Loader;
use super::random::{Legacy, Rng};
use crate::registry::{blocks, BlockRegistry, BlockState};

const PI_F: f32 = std::f32::consts::PI;

fn sin_table() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| (0..65536).map(|i| (i as f64 / 10430.378350470453).sin() as f32).collect())
}

/// `Mth.sin` (65536-entry lookup table).
pub fn msin(d: f64) -> f32 {
    sin_table()[((d * 10430.378350470453) as i64 & 65535) as usize]
}

/// `Mth.cos`.
pub fn mcos(d: f64) -> f32 {
    sin_table()[((d * 10430.378350470453 + 16384.0) as i64 & 65535) as usize]
}

/// `FloatProvider`.
#[derive(Clone, Debug)]
pub enum FloatP {
    Const(f32),
    Uniform(f32, f32),
    Trapezoid { min: f32, max: f32, plateau: f32 },
}

impl FloatP {
    pub fn parse(v: &Value) -> Result<Self> {
        if let Some(f) = v.as_f64() {
            return Ok(FloatP::Const(f as f32));
        }
        let g = |k: &str| v[k].as_f64().map(|f| f as f32).ok_or_else(|| anyhow!("float provider: missing {k}"));
        Ok(match v["type"].as_str().unwrap_or("") {
            "minecraft:constant" => FloatP::Const(g("value")?),
            "minecraft:uniform" => FloatP::Uniform(g("min_inclusive")?, g("max_exclusive")?),
            "minecraft:trapezoid" => FloatP::Trapezoid { min: g("min")?, max: g("max")?, plateau: g("plateau")? },
            t => return Err(anyhow!("unsupported float provider {t}")),
        })
    }

    pub fn sample(&self, r: &mut impl Rng) -> f32 {
        match *self {
            FloatP::Const(c) => c,
            FloatP::Uniform(min, max) => random_between(r, min, max),
            FloatP::Trapezoid { min, max, plateau } => {
                let range = max - min;
                let f3 = (range - plateau) / 2.0;
                let f4 = range - f3;
                min + r.next_float() * f4 + r.next_float() * f3
            }
        }
    }
}

/// `Mth.randomBetween`.
pub fn random_between(r: &mut impl Rng, min: f32, max: f32) -> f32 {
    r.next_float() * (max - min) + min
}

/// `VerticalAnchor` resolved against the overworld height.
pub fn anchor(v: &Value) -> Result<i32> {
    if let Some(a) = v["absolute"].as_i64() {
        return Ok(a as i32);
    }
    if let Some(a) = v["above_bottom"].as_i64() {
        return Ok(MIN_Y + a as i32);
    }
    if let Some(a) = v["below_top"].as_i64() {
        return Ok(MIN_Y + HEIGHT - 1 - a as i32);
    }
    Err(anyhow!("bad vertical anchor {v}"))
}

/// `HeightProvider`.
#[derive(Clone, Debug)]
pub enum HeightP {
    Const(i32),
    Uniform(i32, i32),
}

impl HeightP {
    pub fn parse(v: &Value) -> Result<Self> {
        match v["type"].as_str() {
            Some("minecraft:uniform") => Ok(HeightP::Uniform(anchor(&v["min_inclusive"])?, anchor(&v["max_inclusive"])?)),
            Some("minecraft:constant") => Ok(HeightP::Const(anchor(&v["value"])?)),
            None => Ok(HeightP::Const(anchor(v)?)),
            Some(t) => Err(anyhow!("unsupported height provider {t}")),
        }
    }

    pub fn sample(&self, r: &mut impl Rng) -> i32 {
        match *self {
            HeightP::Const(c) => c,
            HeightP::Uniform(min, max) if min > max => min,
            HeightP::Uniform(min, max) => r.next_int_bound(max - min + 1) + min,
        }
    }
}

/// Resolve a block tag (recursively) into a per-state membership table.
pub fn block_tag(load: &Loader, tag: &str) -> Result<Vec<bool>> {
    fn collect(load: &Loader, tag: &str, out: &mut HashSet<String>, seen: &mut HashSet<String>) -> Result<()> {
        let tag = tag.trim_start_matches('#').trim_start_matches("minecraft:");
        if !seen.insert(tag.to_string()) {
            return Ok(());
        }
        let v = load(&format!("tags/block/{tag}"))?;
        for e in v["values"].as_array().into_iter().flatten() {
            let (id, required) = match e {
                Value::String(s) => (s.as_str(), true),
                o => (o["id"].as_str().unwrap_or(""), o["required"].as_bool().unwrap_or(true)),
            };
            if let Some(t) = id.strip_prefix('#') {
                match collect(load, t, out, seen) {
                    Err(e) if required => return Err(e),
                    _ => {}
                }
            } else {
                out.insert(id.trim_start_matches("minecraft:").to_string());
            }
        }
        Ok(())
    }
    let mut names = HashSet::new();
    collect(load, tag, &mut names, &mut HashSet::new())?;
    let reg = BlockRegistry::get();
    Ok((0..reg.state_count()).map(|i| names.contains(BlockState(i as u16).name())).collect())
}

#[derive(Clone, Debug)]
pub enum Kind {
    Cave { horizontal: FloatP, vertical: FloatP, floor: FloatP },
    Canyon { vertical_rotation: FloatP, distance_factor: FloatP, thickness: FloatP, width_smoothness: i32, horizontal_radius_factor: FloatP, vertical_center: f32, vertical_default: f32 },
}

/// A configured carver.
#[derive(Clone, Debug)]
pub struct Carver {
    pub kind: Kind,
    pub probability: f32,
    pub y: HeightP,
    pub y_scale: FloatP,
    pub lava_level: i32,
    pub replaceable: Vec<bool>,
}

impl Carver {
    pub fn load(load: &Loader, name: &str) -> Result<Self> {
        let v = load(&format!("worldgen/configured_carver/{}", name.trim_start_matches("minecraft:")))?;
        let c = &v["config"];
        let kind = match v["type"].as_str().unwrap_or("") {
            "minecraft:cave" => Kind::Cave { horizontal: FloatP::parse(&c["horizontal_radius_multiplier"])?, vertical: FloatP::parse(&c["vertical_radius_multiplier"])?, floor: FloatP::parse(&c["floor_level"])? },
            "minecraft:canyon" => {
                let s = &c["shape"];
                Kind::Canyon {
                    vertical_rotation: FloatP::parse(&c["vertical_rotation"])?,
                    distance_factor: FloatP::parse(&s["distance_factor"])?,
                    thickness: FloatP::parse(&s["thickness"])?,
                    width_smoothness: s["width_smoothness"].as_i64().unwrap_or(3) as i32,
                    horizontal_radius_factor: FloatP::parse(&s["horizontal_radius_factor"])?,
                    vertical_center: s["vertical_radius_center_factor"].as_f64().unwrap_or(0.0) as f32,
                    vertical_default: s["vertical_radius_default_factor"].as_f64().unwrap_or(1.0) as f32,
                }
            }
            t => return Err(anyhow!("unsupported carver {t}")),
        };
        let replaceable = match &c["replaceable"] {
            Value::String(t) if t.starts_with('#') => block_tag(load, t)?,
            other => {
                let names: HashSet<&str> = match other {
                    Value::String(s) => [s.trim_start_matches("minecraft:")].into(),
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str()).map(|s| s.trim_start_matches("minecraft:")).collect(),
                    _ => HashSet::new(),
                };
                (0..BlockRegistry::get().state_count()).map(|i| names.contains(BlockState(i as u16).name())).collect()
            }
        };
        Ok(Carver {
            kind,
            probability: c["probability"].as_f64().unwrap_or(0.0) as f32,
            y: HeightP::parse(&c["y"])?,
            y_scale: FloatP::parse(&c["yScale"])?,
            lava_level: anchor(&c["lava_level"])?,
            replaceable,
        })
    }

    /// `isStartChunk` then `carve` for the carver started in chunk
    /// (`sx`, `sz`), applied to the chunk in `ctx`.
    pub fn carve(&self, ctx: &mut Carving, r: &mut Legacy, sx: i32, sz: i32) {
        if r.next_float() > self.probability {
            return;
        }
        const RANGE_BLOCKS: i32 = (4 * 2 - 1) * 16;
        match &self.kind {
            Kind::Cave { horizontal, vertical, floor } => {
                let n = {
                    let a = r.next_int_bound(15) + 1;
                    let b = r.next_int_bound(a) + 1;
                    r.next_int_bound(b)
                };
                for _ in 0..n {
                    let x = (sx * 16 + r.next_int_bound(16)) as f64;
                    let y = self.y.sample(r) as f64;
                    let z = (sz * 16 + r.next_int_bound(16)) as f64;
                    let hm = horizontal.sample(r) as f64;
                    let vm = vertical.sample(r) as f64;
                    let fl = floor.sample(r) as f64;
                    let skip = Skip::Cave(fl);
                    let mut count = 1;
                    if r.next_int_bound(4) == 0 {
                        let ys = self.y_scale.sample(r) as f64;
                        let radius = 1.0f32 + r.next_float() * 6.0;
                        let d = 1.5 + (msin(1.5707963705062866) * radius) as f64;
                        self.ellipsoid(ctx, x + 1.0, y, z, d, d * ys, &skip);
                        count += r.next_int_bound(4);
                    }
                    for _ in 0..count {
                        let yaw = r.next_float() * 6.2831855;
                        let pitch = (r.next_float() - 0.5) / 4.0;
                        let thickness = {
                            let mut f = r.next_float() * 2.0 + r.next_float();
                            if r.next_int_bound(10) == 0 {
                                f *= r.next_float() * r.next_float() * 3.0 + 1.0;
                            }
                            f
                        };
                        let dist = RANGE_BLOCKS - r.next_int_bound(RANGE_BLOCKS / 4);
                        let seed = r.next_long();
                        self.tunnel(ctx, seed, [x, y, z], hm, vm, thickness, yaw, pitch, 0, dist, 1.0, &skip);
                    }
                }
            }
            Kind::Canyon { vertical_rotation, distance_factor, thickness, .. } => {
                let x = (sx * 16 + r.next_int_bound(16)) as f64;
                let y = self.y.sample(r);
                let z = (sz * 16 + r.next_int_bound(16)) as f64;
                let yaw = r.next_float() * 6.2831855;
                let pitch = vertical_rotation.sample(r);
                let ys = self.y_scale.sample(r) as f64;
                let th = thickness.sample(r);
                let dist = (RANGE_BLOCKS as f32 * distance_factor.sample(r)) as i32;
                let seed = r.next_long();
                self.canyon(ctx, seed, [x, y as f64, z], th, yaw, pitch, 0, dist, ys);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tunnel(&self, ctx: &mut Carving, seed: i64, p: [f64; 3], hm: f64, vm: f64, thickness: f32, mut yaw: f32, mut pitch: f32, step: i32, dist: i32, y_scale: f64, skip: &Skip) {
        let [mut x, mut y, mut z] = p;
        let mut r = Legacy::new(seed);
        let branch = r.next_int_bound(dist / 2) + dist / 4;
        let steep = r.next_int_bound(6) == 0;
        let mut yaw_mod = 0.0f32;
        let mut pitch_mod = 0.0f32;
        for i in step..dist {
            let d = 1.5 + (msin((PI_F * i as f32 / dist as f32) as f64) * thickness) as f64;
            let e = d * y_scale;
            let c = mcos(pitch as f64);
            x += (mcos(yaw as f64) * c) as f64;
            y += msin(pitch as f64) as f64;
            z += (msin(yaw as f64) * c) as f64;
            pitch *= if steep { 0.92 } else { 0.7 };
            pitch += pitch_mod * 0.1;
            yaw += yaw_mod * 0.1;
            pitch_mod *= 0.9;
            yaw_mod *= 0.75;
            pitch_mod += (r.next_float() - r.next_float()) * r.next_float() * 2.0;
            yaw_mod += (r.next_float() - r.next_float()) * r.next_float() * 4.0;
            if i == branch && thickness > 1.0 {
                let s = r.next_long();
                let t = r.next_float() * 0.5 + 0.5;
                self.tunnel(ctx, s, [x, y, z], hm, vm, t, yaw - 1.5707964, pitch / 3.0, i, dist, 1.0, skip);
                let s = r.next_long();
                let t = r.next_float() * 0.5 + 0.5;
                self.tunnel(ctx, s, [x, y, z], hm, vm, t, yaw + 1.5707964, pitch / 3.0, i, dist, 1.0, skip);
                return;
            }
            if r.next_int_bound(4) != 0 {
                if !ctx.can_reach(x, z, i, dist, thickness) {
                    return;
                }
                self.ellipsoid(ctx, x, y, z, d * hm, e * vm, skip);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn canyon(&self, ctx: &mut Carving, seed: i64, p: [f64; 3], thickness: f32, mut yaw: f32, mut pitch: f32, step: i32, dist: i32, y_scale: f64) {
        let Kind::Canyon { width_smoothness, horizontal_radius_factor, vertical_center, vertical_default, .. } = &self.kind else { return };
        let [mut x, mut y, mut z] = p;
        let mut r = Legacy::new(seed);
        let mut widths = vec![0f32; HEIGHT as usize];
        let mut f = 1.0f32;
        for (i, w) in widths.iter_mut().enumerate() {
            if i == 0 || r.next_int_bound(*width_smoothness) == 0 {
                f = 1.0 + r.next_float() * r.next_float();
            }
            *w = f * f;
        }
        let skip = Skip::Canyon(widths);
        let mut yaw_mod = 0.0f32;
        let mut pitch_mod = 0.0f32;
        for i in step..dist {
            let mut d = 1.5 + (msin((i as f32 * PI_F / dist as f32) as f64) * thickness) as f64;
            let mut e = d * y_scale;
            d *= horizontal_radius_factor.sample(&mut r) as f64;
            // updateVerticalRadius
            let f7 = 1.0f32 - (0.5f32 - i as f32 / dist as f32).abs() * 2.0;
            let f8 = vertical_default + vertical_center * f7;
            e = f8 as f64 * e * random_between(&mut r, 0.75, 1.0) as f64;
            let c = mcos(pitch as f64);
            let s = msin(pitch as f64);
            x += (mcos(yaw as f64) * c) as f64;
            y += s as f64;
            z += (msin(yaw as f64) * c) as f64;
            pitch *= 0.7;
            pitch += pitch_mod * 0.05;
            yaw += yaw_mod * 0.05;
            pitch_mod *= 0.8;
            yaw_mod *= 0.5;
            pitch_mod += (r.next_float() - r.next_float()) * r.next_float() * 2.0;
            yaw_mod += (r.next_float() - r.next_float()) * r.next_float() * 4.0;
            if r.next_int_bound(4) != 0 {
                if !ctx.can_reach(x, z, i, dist, thickness) {
                    return;
                }
                self.ellipsoid(ctx, x, y, z, d, e, &skip);
            }
        }
    }

    /// `WorldCarver.carveEllipsoid`.
    #[allow(clippy::too_many_arguments)]
    fn ellipsoid(&self, ctx: &mut Carving, x: f64, y: f64, z: f64, hr: f64, vr: f64, skip: &Skip) {
        let (min_x, min_z) = (ctx.cx * 16, ctx.cz * 16);
        let d = 16.0 + hr * 2.0;
        if (x - (min_x + 8) as f64).abs() > d || (z - (min_z + 8) as f64).abs() > d {
            return;
        }
        let x0 = ((x - hr).floor() as i32 - min_x - 1).max(0);
        let x1 = ((x + hr).floor() as i32 - min_x).min(15);
        let y0 = ((y - vr).floor() as i32 - 1).max(MIN_Y + 1);
        let y1 = ((y + vr).floor() as i32 + 1).min(MIN_Y + HEIGHT - 1 - 7);
        let z0 = ((z - hr).floor() as i32 - min_z - 1).max(0);
        let z1 = ((z + hr).floor() as i32 - min_z).min(15);
        for lx in x0..=x1 {
            let wx = min_x + lx;
            let rx = (wx as f64 + 0.5 - x) / hr;
            for lz in z0..=z1 {
                let wz = min_z + lz;
                let rz = (wz as f64 + 0.5 - z) / hr;
                if rx * rx + rz * rz >= 1.0 {
                    continue;
                }
                let mut grass = false;
                let mut yy = y1;
                while yy > y0 {
                    let ry = (yy as f64 - 0.5 - y) / vr;
                    if !skip.skip(rx, ry, rz, yy) {
                        let mi = mask_index(lx, yy, lz);
                        if !ctx.mask[mi] {
                            ctx.mask[mi] = true;
                            self.carve_block(ctx, lx, yy, lz, &mut grass);
                        }
                    }
                    yy -= 1;
                }
            }
        }
    }

    /// `WorldCarver.carveBlock`.
    fn carve_block(&self, ctx: &mut Carving, lx: i32, y: i32, lz: i32, grass: &mut bool) {
        let i = idx(lx, y, lz);
        let st = ctx.blocks[i];
        let name = st.name();
        if name == "grass_block" || name == "mycelium" {
            *grass = true;
        }
        if !self.replaceable.get(st.0 as usize).copied().unwrap_or(false) {
            return;
        }
        let (wx, wz) = (ctx.cx * 16 + lx, ctx.cz * 16 + lz);
        let state = if y <= self.lava_level {
            blocks::LAVA
        } else {
            match ctx.aquifer.compute_substance(wx, y, wz, 0.0) {
                Some(s) => s,
                None => return,
            }
        };
        ctx.blocks[i] = state;
        // (the lava branch reads the flag left by the previous aquifer call, like vanilla)
        if ctx.aquifer.schedule && crate::registry::vstate::has_fluid(state) {
            ctx.marks.push([wx, y, wz]);
        }
        let col = (lz * 16 + lx) as usize;
        if state.is_air() && ctx.heights[col] == y {
            let mut h = y - 1;
            while h >= MIN_Y && ctx.blocks[idx(lx, h, lz)].is_air() {
                h -= 1;
            }
            ctx.heights[col] = h;
        }
        if *grass {
            let below = idx(lx, y - 1, lz);
            if ctx.blocks[below].name() == "dirt" {
                let fluid = BlockRegistry::get().has_fluid(state);
                if let Some(s) = (ctx.top)(wx, y - 1, wz, fluid, ctx.heights) {
                    ctx.blocks[below] = s;
                    if crate::registry::vstate::has_fluid(s) {
                        ctx.marks.push([wx, y - 1, wz]);
                    }
                }
            }
        }
    }
}

enum Skip {
    Cave(f64),
    Canyon(Vec<f32>),
}

impl Skip {
    fn skip(&self, rx: f64, ry: f64, rz: f64, y: i32) -> bool {
        match self {
            Skip::Cave(floor) => ry <= *floor || rx * rx + ry * ry + rz * rz >= 1.0,
            Skip::Canyon(w) => (rx * rx + rz * rz) * w[(y - MIN_Y - 1) as usize] as f64 + ry * ry / 6.0 >= 1.0,
        }
    }
}

#[inline]
fn idx(x: i32, y: i32, z: i32) -> usize {
    (((y - MIN_Y) * 16 + z) * 16 + x) as usize
}

#[inline]
fn mask_index(x: i32, y: i32, z: i32) -> usize {
    idx(x, y, z)
}

pub type TopMaterial<'t> = dyn Fn(i32, i32, i32, bool, &[i32; 256]) -> Option<BlockState> + 't;

/// The chunk being carved.
pub struct Carving<'c, 'n, 'a> {
    pub cx: i32,
    pub cz: i32,
    pub blocks: &'c mut [BlockState],
    pub heights: &'c mut [i32; 256],
    pub mask: Vec<bool>,
    pub aquifer: &'c mut Aquifer<'n, 'a>,
    pub top: &'c TopMaterial<'c>,
    /// positions marked for post processing (fluids to update)
    pub marks: &'c mut Vec<[i32; 3]>,
}

impl Carving<'_, '_, '_> {
    /// `WorldCarver.canReach`.
    fn can_reach(&self, x: f64, z: f64, step: i32, dist: i32, thickness: f32) -> bool {
        let dx = x - (self.cx * 16 + 8) as f64;
        let dz = z - (self.cz * 16 + 8) as f64;
        let d = (dist - step) as f64;
        let e = (thickness + 2.0 + 16.0) as f64;
        dx * dx + dz * dz - d * d <= e * e
    }
}

/// `WorldgenRandom.setLargeFeatureSeed`.
pub fn set_large_feature_seed(r: &mut Legacy, seed: i64, cx: i32, cz: i32) {
    r.set_seed(seed);
    let a = r.next_long();
    let b = r.next_long();
    r.set_seed((cx as i64).wrapping_mul(a) ^ (cz as i64).wrapping_mul(b) ^ seed);
}

/// `NoiseBasedChunkGenerator.applyCarvers` for one chunk.
pub fn apply(carvers: &[Carver], seed: i64, ctx: &mut Carving) {
    let mut r = Legacy::new(0);
    for dx in -8..=8 {
        for dz in -8..=8 {
            let (sx, sz) = (ctx.cx + dx, ctx.cz + dz);
            for (i, c) in carvers.iter().enumerate() {
                set_large_feature_seed(&mut r, seed.wrapping_add(i as i64), sx, sz);
                c.carve(ctx, &mut r, sx, sz);
            }
        }
    }
}
