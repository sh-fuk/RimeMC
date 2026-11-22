//! Vanilla density functions: parsed from the data-pack JSON into an arena
//! graph, with noises wired exactly like `RandomState` does.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::noise::{clamped_map, floor, AnyRng, BlendedNoise, NormalNoise};
use super::random::{Rng, XoroshiroFactory};

pub type Loader<'a> = dyn Fn(&str) -> Result<Value> + 'a;

pub type Id = u32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Map {
    Abs,
    Square,
    Cube,
    HalfNegative,
    QuarterNegative,
    Invert,
    Squeeze,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Add,
    Mul,
    Min,
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Marker {
    Interpolated,
    FlatCache,
    Cache2d,
    CacheOnce,
    CacheAllInCell,
}

#[derive(Clone)]
pub struct Spline {
    pub coord: Id,
    pub locations: Vec<f32>,
    pub values: Vec<SplineValue>,
    pub derivatives: Vec<f32>,
}

#[derive(Clone)]
pub enum SplineValue {
    Const(f32),
    Spline(Box<Spline>),
}

#[derive(Clone)]
pub enum Node {
    Const(f64),
    Ap2(Op, Id, Id),
    Mapped(Map, Id),
    Clamp(Id, f64, f64),
    YGrad { from_y: i32, to_y: i32, from_v: f64, to_v: f64 },
    Noise { noise: usize, xz: f64, y: f64 },
    ShiftedNoise { sx: Id, sy: Id, sz: Id, xz: f64, y: f64, noise: usize },
    /// 0 = shift_a, 1 = shift_b, 2 = shift
    Shift(u8, usize),
    RangeChoice { input: Id, min: f64, max: f64, inside: Id, outside: Id },
    Spline(Box<Spline>),
    /// mapper: 1 = type_1, 2 = type_2
    Weird { input: Id, noise: usize, mapper: u8 },
    Blended(usize),
    Marker(Marker, Id),
    BlendAlpha,
    BlendOffset,
    BlendDensity(Id),
    Beardifier,
    FindTopSurface { density: Id, upper: Id, lower: i32, cell: i32 },
    EndIslands,
}

pub struct Graph {
    pub nodes: Vec<Node>,
    pub noises: Vec<Arc<NormalNoise>>,
    pub noise_names: Vec<String>,
    pub blended: Vec<BlendedNoise>,
    /// node id of each `interpolated` marker, in creation order
    pub interpolated: Vec<Id>,
    pub flat_caches: Vec<Id>,
    /// node -> index in `interpolated` / `flat_caches`
    pub slot: Vec<u32>,
    /// noise cell layout of the dimension
    pub shape: Shape,
    /// `EndIslandDensityFunction` noise
    pub end_islands: Option<crate::worldgen::noise::SimplexNoise>,
}

/// `NoiseSettings`: the vertical noise range and the cell size.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub min_y: i32,
    pub height: i32,
    pub cell_w: i32,
    pub cell_h: i32,
}

impl Default for Shape {
    fn default() -> Self {
        Shape { min_y: -64, height: 384, cell_w: 4, cell_h: 8 }
    }
}

impl Shape {
    pub fn parse(v: &Value) -> Shape {
        let n = &v["noise"];
        Shape {
            min_y: n["min_y"].as_i64().unwrap_or(-64) as i32,
            height: n["height"].as_i64().unwrap_or(384) as i32,
            cell_w: n["size_horizontal"].as_i64().unwrap_or(1) as i32 * 4,
            cell_h: n["size_vertical"].as_i64().unwrap_or(2) as i32 * 4,
        }
    }
    pub fn cells_y(&self) -> i32 {
        self.height / self.cell_h
    }
}

/// `EndIslandDensityFunction.getHeightValue`
fn end_height(noise: &crate::worldgen::noise::SimplexNoise, x: i32, z: i32) -> f32 {
    let (i, j, k, l) = (x / 2, z / 2, x % 2, z % 2);
    let mut f = 100.0f32 - ((x * x + z * z) as f32).sqrt() * 8.0;
    f = f.clamp(-100.0, 80.0);
    for i1 in -12..=12i32 {
        for j1 in -12..=12i32 {
            let k1 = (i + i1) as i64;
            let l1 = (j + j1) as i64;
            if k1 * k1 + l1 * l1 > 4096 && noise.value_2d(k1 as f64, l1 as f64) < -0.8999999761581421 {
                let f1 = ((k1 as f32).abs() * 3439.0 + (l1 as f32).abs() * 147.0) % 13.0 + 9.0;
                let f2 = (k - i1 * 2) as f32;
                let f3 = (l - j1 * 2) as f32;
                let f4 = (100.0 - (f2 * f2 + f3 * f3).sqrt() * f1).clamp(-100.0, 80.0);
                f = f.max(f4);
            }
        }
    }
    f
}

/// `RandomState.getOrCreateNoise(key)`
pub fn make_noise(load: &Loader, factory: &XoroshiroFactory, name: &str) -> Result<NormalNoise> {
    let name = if name.contains(':') { name.to_string() } else { format!("minecraft:{name}") };
    let key = name.trim_start_matches("minecraft:");
    let p = load(&format!("worldgen/noise/{key}"))?;
    let first = p["firstOctave"].as_i64().context("firstOctave")? as i32;
    let amps: Vec<f64> = p["amplitudes"].as_array().context("amplitudes")?.iter().filter_map(Value::as_f64).collect();
    Ok(NormalNoise::new(&mut factory.from_hash_of(&name), first, &amps))
}

pub struct Builder<'a> {
    load: &'a Loader<'a>,
    factory: XoroshiroFactory,
    pub g: Graph,
    refs: HashMap<String, Id>,
    noise_ids: HashMap<String, usize>,
    /// world seed when the settings use `legacy_random_source`
    pub legacy: Option<i64>,
}

fn f(v: &Value, k: &str) -> Result<f64> {
    v.get(k).and_then(Value::as_f64).with_context(|| format!("missing number `{k}`"))
}

impl<'a> Builder<'a> {
    pub fn new(load: &'a Loader<'a>, factory: XoroshiroFactory) -> Self {
        Self {
            load,
            factory,
            g: Graph { nodes: Vec::new(), noises: Vec::new(), noise_names: Vec::new(), blended: Vec::new(), interpolated: Vec::new(), flat_caches: Vec::new(), slot: Vec::new(), shape: Shape::default(), end_islands: None },
            legacy: None,
            refs: HashMap::new(),
            noise_ids: HashMap::new(),
        }
    }

    fn push(&mut self, n: Node) -> Id {
        let id = self.g.nodes.len() as Id;
        let mut slot = 0;
        if let Node::Marker(m, _) = n {
            if m == Marker::Interpolated {
                slot = self.g.interpolated.len() as u32;
                self.g.interpolated.push(id);
            } else if m == Marker::FlatCache {
                slot = self.g.flat_caches.len() as u32;
                self.g.flat_caches.push(id);
            }
        }
        self.g.nodes.push(n);
        self.g.slot.push(slot);
        id
    }

    pub fn noise(&mut self, name: &str) -> Result<usize> {
        let name = if name.contains(':') { name.to_string() } else { format!("minecraft:{name}") };
        if let Some(&i) = self.noise_ids.get(&name) {
            return Ok(i);
        }
        let n = match (self.legacy, name.as_str()) {
            // RandomState's NoiseWiringHelper: the nether biome noises keep the old initialization
            (Some(seed), "minecraft:nether/temperature" | "minecraft:nether/vegetation") => {
                let key = name.trim_start_matches("minecraft:");
                let p = (self.load)(&format!("worldgen/noise/{key}"))?;
                let first = p["firstOctave"].as_i64().context("firstOctave")? as i32;
                let amps: Vec<f64> = p["amplitudes"].as_array().context("amplitudes")?.iter().filter_map(Value::as_f64).collect();
                let off = if key.ends_with("temperature") { 0 } else { 1 };
                NormalNoise::legacy_nether_biome(seed.wrapping_add(off), first, &amps)
            }
            _ => make_noise(self.load, &self.factory, &name)?,
        };
        let i = self.g.noises.len();
        self.g.noises.push(Arc::new(n));
        self.g.noise_names.push(name.clone());
        self.noise_ids.insert(name, i);
        Ok(i)
    }

    fn noise_field(&mut self, v: &Value) -> Result<usize> {
        match &v["noise"] {
            Value::String(s) => self.noise(s),
            _ => bail!("inline noise parameters are not supported"),
        }
    }

    pub fn reference(&mut self, name: &str) -> Result<Id> {
        let key = name.trim_start_matches("minecraft:").to_string();
        if let Some(&id) = self.refs.get(&key) {
            return Ok(id);
        }
        let v = (self.load)(&format!("worldgen/density_function/{key}"))?;
        let id = self.parse(&v).with_context(|| format!("density function {key}"))?;
        self.refs.insert(key, id);
        Ok(id)
    }

    pub fn parse(&mut self, v: &Value) -> Result<Id> {
        if let Some(c) = v.as_f64() {
            return Ok(self.push(Node::Const(c)));
        }
        if let Some(s) = v.as_str() {
            return self.reference(s);
        }
        let t = v["type"].as_str().context("density function without type")?.trim_start_matches("minecraft:");
        let arg = |b: &mut Self, k: &str| -> Result<Id> { b.parse(v.get(k).with_context(|| format!("missing `{k}`"))?) };
        let node = match t {
            "constant" => Node::Const(f(v, "argument")?),
            "add" | "mul" | "min" | "max" => {
                let a = arg(self, "argument1")?;
                let b = arg(self, "argument2")?;
                let op = match t {
                    "add" => Op::Add,
                    "mul" => Op::Mul,
                    "min" => Op::Min,
                    _ => Op::Max,
                };
                Node::Ap2(op, a, b)
            }
            "abs" | "square" | "cube" | "half_negative" | "quarter_negative" | "invert" | "squeeze" => {
                let m = match t {
                    "abs" => Map::Abs,
                    "square" => Map::Square,
                    "cube" => Map::Cube,
                    "half_negative" => Map::HalfNegative,
                    "quarter_negative" => Map::QuarterNegative,
                    "invert" => Map::Invert,
                    _ => Map::Squeeze,
                };
                Node::Mapped(m, arg(self, "argument")?)
            }
            "clamp" => Node::Clamp(arg(self, "input")?, f(v, "min")?, f(v, "max")?),
            "y_clamped_gradient" => Node::YGrad { from_y: f(v, "from_y")? as i32, to_y: f(v, "to_y")? as i32, from_v: f(v, "from_value")?, to_v: f(v, "to_value")? },
            "noise" => Node::Noise { noise: self.noise_field(v)?, xz: f(v, "xz_scale")?, y: f(v, "y_scale")? },
            "shifted_noise" => Node::ShiftedNoise { sx: arg(self, "shift_x")?, sy: arg(self, "shift_y")?, sz: arg(self, "shift_z")?, xz: f(v, "xz_scale")?, y: f(v, "y_scale")?, noise: self.noise_field(v)? },
            "shift_a" | "shift_b" | "shift" => {
                let n = match &v["argument"] {
                    Value::String(s) => self.noise(s)?,
                    _ => bail!("inline noise"),
                };
                Node::Shift(match t {
                    "shift_a" => 0,
                    "shift_b" => 1,
                    _ => 2,
                }, n)
            }
            "range_choice" => Node::RangeChoice { input: arg(self, "input")?, min: f(v, "min_inclusive")?, max: f(v, "max_exclusive")?, inside: arg(self, "when_in_range")?, outside: arg(self, "when_out_of_range")? },
            "spline" => Node::Spline(Box::new(self.spline(&v["spline"])?)),
            "weird_scaled_sampler" => Node::Weird {
                input: arg(self, "input")?,
                noise: self.noise_field(v)?,
                mapper: if v["rarity_value_mapper"].as_str() == Some("type_1") { 1 } else { 2 },
            },
            "old_blended_noise" => {
                // RandomState wires a fresh BlendedNoise from `fromHashOf("minecraft:terrain")`
                // (legacy worlds: LegacyRandomSource(seed)); see Graph::new.
                let (xz, ys, xf, yf, sm) = (f(v, "xz_scale")?, f(v, "y_scale")?, f(v, "xz_factor")?, f(v, "y_factor")?, f(v, "smear_scale_multiplier")?);
                let b = match self.legacy {
                    Some(seed) => BlendedNoise::new(&mut AnyRng::L(&mut crate::worldgen::random::Legacy::new(seed)), xz, ys, xf, yf, sm),
                    None => BlendedNoise::new(&mut AnyRng::X(&mut self.factory.from_hash_of("minecraft:terrain")), xz, ys, xf, yf, sm),
                };
                self.g.blended.push(b);
                Node::Blended(self.g.blended.len() - 1)
            }
            "interpolated" => Node::Marker(Marker::Interpolated, arg(self, "argument")?),
            "flat_cache" => Node::Marker(Marker::FlatCache, arg(self, "argument")?),
            "cache_2d" => Node::Marker(Marker::Cache2d, arg(self, "argument")?),
            "cache_once" => Node::Marker(Marker::CacheOnce, arg(self, "argument")?),
            "cache_all_in_cell" => Node::Marker(Marker::CacheAllInCell, arg(self, "argument")?),
            "blend_alpha" => Node::BlendAlpha,
            "blend_offset" => Node::BlendOffset,
            "blend_density" => Node::BlendDensity(arg(self, "argument")?),
            "beardifier" => Node::Beardifier,
            "find_top_surface" => Node::FindTopSurface { density: arg(self, "density")?, upper: arg(self, "upper_bound")?, lower: f(v, "lower_bound")? as i32, cell: f(v, "cell_height")? as i32 },
            "end_islands" => {
                if self.g.end_islands.is_none() {
                    let mut r = crate::worldgen::random::Legacy::new(self.legacy.unwrap_or(0));
                    r.consume(17292);
                    self.g.end_islands = Some(crate::worldgen::noise::SimplexNoise::new(&mut r));
                }
                Node::EndIslands
            }
            other => bail!("unsupported density function type {other}"),
        };
        Ok(self.push(node))
    }

    fn spline(&mut self, v: &Value) -> Result<Spline> {
        let coord = self.parse(&v["coordinate"])?;
        let mut s = Spline { coord, locations: Vec::new(), values: Vec::new(), derivatives: Vec::new() };
        for p in v["points"].as_array().context("spline points")? {
            s.locations.push(f(p, "location")? as f32);
            s.derivatives.push(f(p, "derivative")? as f32);
            let val = &p["value"];
            s.values.push(match val.as_f64() {
                Some(c) => SplineValue::Const(c as f32),
                None => SplineValue::Spline(Box::new(self.spline(val)?)),
            });
        }
        Ok(s)
    }
}

/// Evaluation context. `chunk` is set when evaluating inside a noise chunk
/// (vanilla's NoiseChunk as FunctionContext); without it every marker is
/// transparent, as with vanilla's SinglePointContext.
pub trait Cache {
    /// interpolated value of `interpolated` marker number `slot` at the
    /// current block, or None when the position is not the chunk's current
    /// interpolation position.
    fn interpolated(&self, slot: u32, x: i32, y: i32, z: i32) -> Option<f64>;
    fn flat(&self, g: &Graph, slot: u32, x: i32, z: i32) -> Option<f64>;
}

pub struct NoCache;
impl Cache for NoCache {
    fn interpolated(&self, _: u32, _: i32, _: i32, _: i32) -> Option<f64> {
        None
    }
    fn flat(&self, _: &Graph, _: u32, _: i32, _: i32) -> Option<f64> {
        None
    }
}

#[inline]
fn shift(n: &NormalNoise, x: f64, y: f64, z: f64) -> f64 {
    n.value(x * 0.25, y * 0.25, z * 0.25) * 4.0
}

fn spaghetti_rarity_3d(d: f64) -> f64 {
    if d < -0.5 {
        0.75
    } else if d < 0.0 {
        1.0
    } else if d < 0.5 {
        1.5
    } else {
        2.0
    }
}

fn spaghetti_rarity_2d(d: f64) -> f64 {
    if d < -0.75 {
        0.5
    } else if d < -0.5 {
        0.75
    } else if d < 0.5 {
        1.0
    } else if d < 0.75 {
        2.0
    } else {
        3.0
    }
}

impl Graph {
    pub fn eval(&self, id: Id, x: i32, y: i32, z: i32, c: &dyn Cache) -> f64 {
        match &self.nodes[id as usize] {
            Node::Const(v) => *v,
            Node::Ap2(op, a, b) => {
                let d = self.eval(*a, x, y, z, c);
                match op {
                    Op::Add => d + self.eval(*b, x, y, z, c),
                    Op::Mul => {
                        if d == 0.0 {
                            0.0
                        } else {
                            d * self.eval(*b, x, y, z, c)
                        }
                    }
                    Op::Min => jmin(d, self.eval(*b, x, y, z, c)),
                    Op::Max => jmax(d, self.eval(*b, x, y, z, c)),
                }
            }
            Node::Mapped(m, a) => {
                let d = self.eval(*a, x, y, z, c);
                match m {
                    Map::Abs => d.abs(),
                    Map::Square => d * d,
                    Map::Cube => d * d * d,
                    Map::HalfNegative => {
                        if d > 0.0 {
                            d
                        } else {
                            d * 0.5
                        }
                    }
                    Map::QuarterNegative => {
                        if d > 0.0 {
                            d
                        } else {
                            d * 0.25
                        }
                    }
                    Map::Invert => 1.0 / d,
                    Map::Squeeze => {
                        let e = d.clamp(-1.0, 1.0);
                        e / 2.0 - e * e * e / 24.0
                    }
                }
            }
            Node::Clamp(a, lo, hi) => {
                let d = self.eval(*a, x, y, z, c);
                if d < *lo {
                    *lo
                } else if d > *hi {
                    *hi
                } else {
                    d
                }
            }
            Node::YGrad { from_y, to_y, from_v, to_v } => clamped_map(y as f64, *from_y as f64, *to_y as f64, *from_v, *to_v),
            Node::Noise { noise, xz, y: ys } => self.noises[*noise].value(x as f64 * xz, y as f64 * ys, z as f64 * xz),
            Node::ShiftedNoise { sx, sy, sz, xz, y: ys, noise } => {
                let d = x as f64 * xz + self.eval(*sx, x, y, z, c);
                let e = y as f64 * ys + self.eval(*sy, x, y, z, c);
                let f = z as f64 * xz + self.eval(*sz, x, y, z, c);
                self.noises[*noise].value(d, e, f)
            }
            Node::Shift(kind, n) => {
                let n = &self.noises[*n];
                match kind {
                    0 => shift(n, x as f64, 0.0, z as f64),
                    1 => shift(n, z as f64, x as f64, 0.0),
                    _ => shift(n, x as f64, y as f64, z as f64),
                }
            }
            Node::RangeChoice { input, min, max, inside, outside } => {
                let d = self.eval(*input, x, y, z, c);
                if d >= *min && d < *max { self.eval(*inside, x, y, z, c) } else { self.eval(*outside, x, y, z, c) }
            }
            Node::Spline(s) => self.spline(s, x, y, z, c) as f64,
            Node::Weird { input, noise, mapper } => {
                let d = self.eval(*input, x, y, z, c);
                let e = if *mapper == 1 { spaghetti_rarity_3d(d) } else { spaghetti_rarity_2d(d) };
                e * self.noises[*noise].value(x as f64 / e, y as f64 / e, z as f64 / e).abs()
            }
            Node::Blended(b) => self.blended[*b].compute(x, y, z),
            Node::Marker(m, a) => {
                let slot = self.slot[id as usize];
                match m {
                    Marker::Interpolated => c.interpolated(slot, x, y, z).unwrap_or_else(|| self.eval(*a, x, y, z, c)),
                    Marker::FlatCache => c.flat(self, slot, x, z).unwrap_or_else(|| self.eval(*a, x, y, z, c)),
                    _ => self.eval(*a, x, y, z, c),
                }
            }
            Node::BlendAlpha => 1.0,
            Node::BlendOffset => 0.0,
            Node::BlendDensity(a) => self.eval(*a, x, y, z, c),
            Node::Beardifier => 0.0,
            Node::FindTopSurface { density, upper, lower, cell } => {
                let i = floor(self.eval(*upper, x, y, z, c) / *cell as f64) * cell;
                if i <= *lower {
                    return *lower as f64;
                }
                let mut j = i;
                while j >= *lower {
                    if self.eval(*density, x, j, z, &NoCache) > 0.0 {
                        return j as f64;
                    }
                    j -= cell;
                }
                *lower as f64
            }
            Node::EndIslands => match &self.end_islands {
                Some(n) => (end_height(n, x / 8, z / 8) as f64 - 8.0) / 128.0,
                None => 0.0,
            },
        }
    }

    fn spline(&self, s: &Spline, x: i32, y: i32, z: i32, c: &dyn Cache) -> f32 {
        let f = self.eval(s.coord, x, y, z, c) as f32;
        let locs = &s.locations;
        // Mth.binarySearch(0, len, i -> f < locs[i]) - 1
        let (mut from, mut n) = (0usize, locs.len());
        while n > 0 {
            let j = n / 2;
            let k = from + j;
            if f < locs[k] {
                n = j;
            } else {
                from = k + 1;
                n -= j + 1;
            }
        }
        let i = from as isize - 1;
        let last = locs.len() - 1;
        let val = |v: &SplineValue| match v {
            SplineValue::Const(c) => *c,
            SplineValue::Spline(sp) => self.spline(sp, x, y, z, c),
        };
        let extend = |value: f32, i: usize| {
            let g = s.derivatives[i];
            if g == 0.0 { value } else { value + g * (f - locs[i]) }
        };
        if i < 0 {
            return extend(val(&s.values[0]), 0);
        }
        let i = i as usize;
        if i == last {
            return extend(val(&s.values[last]), last);
        }
        let g = locs[i];
        let h = locs[i + 1];
        let k = (f - g) / (h - g);
        let n = s.derivatives[i];
        let o = s.derivatives[i + 1];
        let p = val(&s.values[i]);
        let q = val(&s.values[i + 1]);
        let r = n * (h - g) - (q - p);
        let t = -o * (h - g) + (q - p);
        flerp(k, p, q) + k * (1.0 - k) * flerp(k, r, t)
    }
}

#[inline]
fn flerp(t: f32, a: f32, b: f32) -> f32 {
    a + t * (b - a)
}

/// java.lang.Math.min/max semantics for the values that occur here.
#[inline]
fn jmin(a: f64, b: f64) -> f64 {
    if a <= b { a } else { b }
}
#[inline]
fn jmax(a: f64, b: f64) -> f64 {
    if a >= b { a } else { b }
}

/// The 15 router entries in vanilla order.
pub const ROUTER: [&str; 15] = [
    "barrier",
    "fluid_level_floodedness",
    "fluid_level_spread",
    "lava",
    "temperature",
    "vegetation",
    "continents",
    "erosion",
    "depth",
    "ridges",
    "preliminary_surface_level",
    "final_density",
    "vein_toggle",
    "vein_ridged",
    "vein_gap",
];

pub struct Router {
    pub graph: Graph,
    pub ids: [Id; 15],
}

impl Router {
    pub fn new(load: &Loader, settings: &Value, factory: XoroshiroFactory) -> Result<Router> {
        Self::new_dim(load, settings, factory, None)
    }

    /// `legacy`: the world seed when the settings use `legacy_random_source`.
    pub fn new_dim(load: &Loader, settings: &Value, factory: XoroshiroFactory, legacy: Option<i64>) -> Result<Router> {
        let mut b = Builder::new(load, factory);
        b.legacy = legacy;
        b.g.shape = Shape::parse(settings);
        let r = &settings["noise_router"];
        let mut ids = [0; 15];
        for (i, k) in ROUTER.iter().enumerate() {
            ids[i] = b.parse(&r[*k]).with_context(|| format!("router.{k}"))?;
        }
        Ok(Router { graph: b.g, ids })
    }

    pub fn eval(&self, i: usize, x: i32, y: i32, z: i32) -> f64 {
        self.graph.eval(self.ids[i], x, y, z, &NoCache)
    }
}
