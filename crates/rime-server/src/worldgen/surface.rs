//! Port of vanilla's SurfaceSystem + data-driven SurfaceRules, BiomeManager's
//! fuzzy biome zoom and biome temperature.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::biome::BiomeTree;
use super::chunk::{NoiseChunk, HEIGHT, MIN_Y};
use super::density::{make_noise, Loader, Router};
use super::noise::{floor, lerp2, NormalNoise, PerlinSimplexNoise};
use super::random::{Legacy, Rng, XoroshiroFactory};
use crate::registry::{blocks, BlockRegistry, BlockState};

#[derive(Clone)]
enum Cond {
    Biome(Vec<u8>),
    Noise { n: usize, min: f64, max: f64 },
    VGrad { rf: XoroshiroFactory, true_y: i32, false_y: i32 },
    YAbove { y: i32, mult: i32, add: bool },
    Water { offset: i32, mult: i32, add: bool },
    Temperature,
    Steep,
    Not(Box<Cond>),
    Hole,
    AbovePreliminary,
    StoneDepth { offset: i32, add_surface: bool, range: i32, ceiling: bool },
}

#[derive(Clone)]
enum Rule {
    Block(BlockState),
    Seq(Vec<Rule>),
    Cond(Cond, Box<Rule>),
    Bandlands,
}

#[derive(Clone, Copy)]
pub struct Climate {
    pub temperature: f32,
    pub frozen: bool,
}

pub struct BiomeTemps {
    temperature: PerlinSimplexNoise,
    frozen: PerlinSimplexNoise,
    info: PerlinSimplexNoise,
}

impl BiomeTemps {
    pub fn new() -> Self {
        BiomeTemps {
            temperature: PerlinSimplexNoise::new(&mut Legacy::new(1234), &[0]),
            frozen: PerlinSimplexNoise::new(&mut Legacy::new(3456), &[-2, -1, 0]),
            info: PerlinSimplexNoise::new(&mut Legacy::new(2345), &[0]),
        }
    }

    /// `Biome.getTemperature(pos, seaLevel)`
    pub fn temperature(&self, c: Climate, x: i32, y: i32, z: i32, sea_level: i32) -> f32 {
        let mut t = c.temperature;
        if c.frozen {
            let d = self.frozen.value(x as f64 * 0.05, z as f64 * 0.05) * 7.0;
            let e = self.info.value(x as f64 * 0.2, z as f64 * 0.2);
            if d + e < 0.3 {
                let g = self.info.value(x as f64 * 0.09, z as f64 * 0.09);
                if g < 0.8 {
                    t = 0.2;
                }
            }
        }
        let i = sea_level + 17;
        if y > i {
            let g = (self.temperature.value((x as f32 / 8.0) as f64, (z as f32 / 8.0) as f64) * 8.0) as f32;
            return t - (g + y as f32 - i as f32) * 0.05 / 40.0;
        }
        t
    }
}

impl Default for BiomeTemps {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SurfaceSystem {
    pub default_block: BlockState,
    pub sea_level: i32,
    clay_bands_offset: NormalNoise,
    clay_bands: Vec<BlockState>,
    surface: NormalNoise,
    surface_secondary: NormalNoise,
    badlands_pillar: NormalNoise,
    badlands_pillar_roof: NormalNoise,
    badlands_surface: NormalNoise,
    iceberg_pillar: NormalNoise,
    iceberg_pillar_roof: NormalNoise,
    iceberg_surface: NormalNoise,
    noise_random: XoroshiroFactory,
    noises: Vec<NormalNoise>,
    rule: Rule,
    pub climates: Vec<Climate>,
    temps: BiomeTemps,
    pub zoom_seed: i64,
    eroded_badlands: u8,
    frozen_oceans: [u8; 2],
}

fn biome_id(name: &str) -> Result<u8> {
    let n = if name.contains(':') { name.to_string() } else { format!("minecraft:{name}") };
    crate::world::chunk::Biomes::get().id(&n).with_context(|| format!("unknown biome {n}"))
}

fn anchor(v: &Value) -> Result<i32> {
    if let Some(a) = v.get("absolute").and_then(Value::as_i64) {
        Ok(a as i32)
    } else if let Some(a) = v.get("above_bottom").and_then(Value::as_i64) {
        Ok(MIN_Y + a as i32)
    } else if let Some(a) = v.get("below_top").and_then(Value::as_i64) {
        Ok(MIN_Y + HEIGHT - 1 - a as i32)
    } else {
        bail!("bad vertical anchor {v}")
    }
}

fn block_state(v: &Value) -> Result<BlockState> {
    let name = v["Name"].as_str().context("Name")?;
    let mut props = Vec::new();
    if let Some(p) = v.get("Properties").and_then(Value::as_object) {
        for (k, val) in p {
            props.push((k.as_str(), val.as_str().unwrap_or("")));
        }
    }
    BlockRegistry::get().state(name, &props).with_context(|| format!("unknown block {name}"))
}

/// `BiomeManager.obfuscateSeed`: first 8 bytes (LE) of sha256(seed as LE long).
pub fn obfuscate_seed(seed: i64) -> i64 {
    let h = Sha256::digest(seed.to_le_bytes());
    i64::from_le_bytes(h[0..8].try_into().unwrap())
}

impl SurfaceSystem {
    pub fn new(load: &Loader, settings: &Value, random: XoroshiroFactory, seed: i64) -> Result<Self> {
        let n = |name: &str| make_noise(load, &random, name);
        let mut s = SurfaceSystem {
            default_block: block_state(&settings["default_block"])?,
            sea_level: settings["sea_level"].as_i64().unwrap_or(63) as i32,
            clay_bands_offset: n("clay_bands_offset")?,
            clay_bands: generate_bands(&mut random.from_hash_of("minecraft:clay_bands")),
            surface: n("surface")?,
            surface_secondary: n("surface_secondary")?,
            badlands_pillar: n("badlands_pillar")?,
            badlands_pillar_roof: n("badlands_pillar_roof")?,
            badlands_surface: n("badlands_surface")?,
            iceberg_pillar: n("iceberg_pillar")?,
            iceberg_pillar_roof: n("iceberg_pillar_roof")?,
            iceberg_surface: n("iceberg_surface")?,
            noise_random: random,
            noises: Vec::new(),
            rule: Rule::Seq(Vec::new()),
            climates: Vec::new(),
            temps: BiomeTemps::new(),
            zoom_seed: obfuscate_seed(seed),
            eroded_badlands: biome_id("eroded_badlands")?,
            frozen_oceans: [biome_id("frozen_ocean")?, biome_id("deep_frozen_ocean")?],
        };
        for name in &crate::world::chunk::Biomes::get().names {
            let j = load(&format!("worldgen/biome/{}", name.trim_start_matches("minecraft:")))?;
            s.climates.push(Climate { temperature: j["temperature"].as_f64().unwrap_or(0.5) as f32, frozen: j["temperature_modifier"].as_str() == Some("frozen") });
        }
        let mut noise_ids: HashMap<String, usize> = HashMap::new();
        s.rule = s.parse_rule(&settings["surface_rule"], load, &random, &mut noise_ids)?;
        Ok(s)
    }

    fn parse_rule(&mut self, v: &Value, load: &Loader, random: &XoroshiroFactory, ids: &mut HashMap<String, usize>) -> Result<Rule> {
        let t = v["type"].as_str().context("rule type")?.trim_start_matches("minecraft:");
        Ok(match t {
            "block" => Rule::Block(block_state(&v["result_state"])?),
            "sequence" => Rule::Seq(v["sequence"].as_array().context("sequence")?.iter().map(|r| self.parse_rule(r, load, random, ids)).collect::<Result<_>>()?),
            "condition" => Rule::Cond(self.parse_cond(&v["if_true"], load, random, ids)?, Box::new(self.parse_rule(&v["then_run"], load, random, ids)?)),
            "bandlands" => Rule::Bandlands,
            other => bail!("unsupported surface rule {other}"),
        })
    }

    fn parse_cond(&mut self, v: &Value, load: &Loader, random: &XoroshiroFactory, ids: &mut HashMap<String, usize>) -> Result<Cond> {
        let t = v["type"].as_str().context("condition type")?.trim_start_matches("minecraft:");
        let i = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0) as i32;
        let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        Ok(match t {
            "biome" => Cond::Biome(v["biome_is"].as_array().context("biome_is")?.iter().filter_map(Value::as_str).map(biome_id).collect::<Result<_>>()?),
            "noise_threshold" => {
                let name = v["noise"].as_str().context("noise")?.to_string();
                let n = match ids.get(&name) {
                    Some(&n) => n,
                    None => {
                        self.noises.push(make_noise(load, random, &name)?);
                        ids.insert(name, self.noises.len() - 1);
                        self.noises.len() - 1
                    }
                };
                Cond::Noise { n, min: v["min_threshold"].as_f64().unwrap_or(0.0), max: v["max_threshold"].as_f64().unwrap_or(0.0) }
            }
            "vertical_gradient" => {
                let name = v["random_name"].as_str().context("random_name")?;
                let name = if name.contains(':') { name.to_string() } else { format!("minecraft:{name}") };
                Cond::VGrad { rf: random.from_hash_of(&name).fork_positional(), true_y: anchor(&v["true_at_and_below"])?, false_y: anchor(&v["false_at_and_above"])? }
            }
            "y_above" => Cond::YAbove { y: anchor(&v["anchor"])?, mult: i("surface_depth_multiplier"), add: b("add_stone_depth") },
            "water" => Cond::Water { offset: i("offset"), mult: i("surface_depth_multiplier"), add: b("add_stone_depth") },
            "temperature" => Cond::Temperature,
            "steep" => Cond::Steep,
            "not" => Cond::Not(Box::new(self.parse_cond(&v["invert"], load, random, ids)?)),
            "hole" => Cond::Hole,
            "above_preliminary_surface" => Cond::AbovePreliminary,
            "stone_depth" => Cond::StoneDepth { offset: i("offset"), add_surface: b("add_surface_depth"), range: i("secondary_depth_range"), ceiling: v["surface_type"].as_str() == Some("ceiling") },
            other => bail!("unsupported surface condition {other}"),
        })
    }

    fn surface_depth(&self, x: i32, z: i32) -> i32 {
        let d = self.surface.value(x as f64, 0.0, z as f64);
        (d * 2.75 + 3.0 + self.noise_random.at(x, 0, z).next_double() * 0.25) as i32
    }

    fn band(&self, x: i32, y: i32, z: i32) -> BlockState {
        let i = (self.clay_bands_offset.value(x as f64, 0.0, z as f64) * 4.0).round() as i32;
        let n = self.clay_bands.len() as i32;
        self.clay_bands[(y + i + n).rem_euclid(n) as usize]
    }
}

fn generate_bands(r: &mut super::random::Xoroshiro) -> Vec<BlockState> {
    let mut bs = vec![blocks::TERRACOTTA; 192];
    let mut i = 0;
    while i < bs.len() {
        i += r.next_int_bound(5) as usize + 1;
        if i < bs.len() {
            bs[i] = blocks::ORANGE_TERRACOTTA;
        }
        i += 1;
    }
    make_bands(r, &mut bs, 1, blocks::YELLOW_TERRACOTTA);
    make_bands(r, &mut bs, 2, blocks::BROWN_TERRACOTTA);
    make_bands(r, &mut bs, 1, blocks::RED_TERRACOTTA);
    let j = r.next_int_between(9, 15);
    let mut k = 0;
    let mut l = 0usize;
    while k < j && l < bs.len() {
        bs[l] = blocks::WHITE_TERRACOTTA;
        if l >= 2 && r.next_bool() {
            bs[l - 1] = blocks::LIGHT_GRAY_TERRACOTTA;
        }
        if l + 1 < bs.len() && r.next_bool() {
            bs[l + 1] = blocks::LIGHT_GRAY_TERRACOTTA;
        }
        l += r.next_int_bound(16) as usize + 4;
        k += 1;
    }
    bs
}

fn make_bands(r: &mut super::random::Xoroshiro, bs: &mut [BlockState], min_width: i32, state: BlockState) {
    let i = r.next_int_between(6, 15);
    for _ in 0..i {
        let k = min_width + r.next_int_bound(3);
        let l = r.next_int_bound(bs.len() as i32) as usize;
        let mut m = 0;
        while l + m < bs.len() && (m as i32) < k {
            bs[l + m] = state;
            m += 1;
        }
    }
}

/// Biomes around a chunk as vanilla's WorldGenRegion sees them: whole
/// chunks filled in vanilla order (the R-tree breaks ties using the previous
/// lookup, so per-chunk fill order matters).
pub struct BiomeSource<'a> {
    pub router: &'a Router,
    pub tree: &'a BiomeTree,
    pub nc: &'a NoiseChunk<'a>,
    /// tree value index -> biome id
    pub ids: &'a [u8],
    cache: &'a BiomeCache,
    local: RefCell<HashMap<(i32, i32), ChunkBiomes>>,
}

pub type ChunkBiomes = std::sync::Arc<Vec<[u16; 64]>>;

/// Noise biomes of whole chunks, shared between generator threads.
#[derive(Default)]
pub struct BiomeCache(parking_lot::Mutex<HashMap<(i32, i32), ChunkBiomes>>);

impl BiomeCache {
    const MAX: usize = 16384;
    pub fn get(&self, cx: i32, cz: i32) -> Option<ChunkBiomes> {
        self.0.lock().get(&(cx, cz)).cloned()
    }
    pub fn insert(&self, cx: i32, cz: i32, b: ChunkBiomes) {
        let mut m = self.0.lock();
        if m.len() >= Self::MAX {
            m.clear();
        }
        m.insert((cx, cz), b);
    }
}

impl<'a> BiomeSource<'a> {
    pub fn new(router: &'a Router, tree: &'a BiomeTree, nc: &'a NoiseChunk<'a>, ids: &'a [u8], cache: &'a BiomeCache) -> Self {
        BiomeSource { router, tree, nc, ids, cache, local: RefCell::new(HashMap::new()) }
    }

    fn chunk(&self, cx: i32, cz: i32) -> ChunkBiomes {
        if let Some(b) = self.local.borrow().get(&(cx, cz)) {
            return b.clone();
        }
        let b = match self.cache.get(cx, cz) {
            Some(b) => b,
            None => {
                let b = ChunkBiomes::new(super::biome::chunk_biomes(self.router, self.nc, self.tree, cx, cz));
                self.cache.insert(cx, cz, b.clone());
                b
            }
        };
        self.local.borrow_mut().insert((cx, cz), b.clone());
        b
    }

    pub fn noise_biome(&self, qx: i32, qy: i32, qz: i32) -> u8 {
        let qy = qy.clamp(MIN_Y >> 2, ((MIN_Y + HEIGHT) >> 2) - 1);
        let c = self.chunk(qx >> 2, qz >> 2);
        let sec = &c[((qy - (MIN_Y >> 2)) >> 2) as usize];
        self.ids[sec[(((qy & 3) << 4) | ((qz & 3) << 2) | (qx & 3)) as usize] as usize]
    }

    /// `BiomeManager.getBiome(pos)` (fuzzy zoom).
    pub fn biome(&self, zoom_seed: i64, x: i32, y: i32, z: i32) -> u8 {
        let i = x - 2;
        let j = y - 2;
        let k = z - 2;
        let (l, m, n) = (i >> 2, j >> 2, k >> 2);
        let d = (i & 3) as f64 / 4.0;
        let e = (j & 3) as f64 / 4.0;
        let f = (k & 3) as f64 / 4.0;
        let mut o = 0;
        let mut g = f64::INFINITY;
        for p in 0..8 {
            let (bx, by, bz) = (p & 4 == 0, p & 2 == 0, p & 1 == 0);
            let q = if bx { l } else { l + 1 };
            let r = if by { m } else { m + 1 };
            let s = if bz { n } else { n + 1 };
            let h = if bx { d } else { d - 1.0 };
            let t = if by { e } else { e - 1.0 };
            let u = if bz { f } else { f - 1.0 };
            let v = fiddled_distance(zoom_seed, q, r, s, h, t, u);
            if g > v {
                o = p;
                g = v;
            }
        }
        let q = if o & 4 == 0 { l } else { l + 1 };
        let r = if o & 2 == 0 { m } else { m + 1 };
        let s = if o & 1 == 0 { n } else { n + 1 };
        self.noise_biome(q, r, s)
    }
}

#[inline]
fn lcg(l: i64, m: i64) -> i64 {
    l.wrapping_mul(l.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407)).wrapping_add(m)
}

fn fiddle(l: i64) -> f64 {
    let d = (l >> 24).rem_euclid(1024) as f64 / 1024.0;
    (d - 0.5) * 0.9
}

#[allow(clippy::too_many_arguments)]
fn fiddled_distance(seed: i64, x: i32, y: i32, z: i32, dx: f64, dy: f64, dz: f64) -> f64 {
    let mut m = lcg(seed, x as i64);
    m = lcg(m, y as i64);
    m = lcg(m, z as i64);
    m = lcg(m, x as i64);
    m = lcg(m, y as i64);
    m = lcg(m, z as i64);
    let d = fiddle(m);
    m = lcg(m, seed);
    let e = fiddle(m);
    m = lcg(m, seed);
    let f = fiddle(m);
    (dz + f) * (dz + f) + (dy + e) * (dy + e) + (dx + d) * (dx + d)
}

/// Surface rule evaluation context (vanilla SurfaceRules.Context).
struct Ctx<'s, 'b> {
    sys: &'s SurfaceSystem,
    bs: &'s BiomeSource<'b>,
    router: &'s Router,
    heights: &'s [i32; 256],
    x: i32,
    y: i32,
    z: i32,
    stone_above: i32,
    stone_below: i32,
    water_height: i32,
    surface_depth: i32,
    secondary: Cell<Option<f64>>,
    min_surface: Cell<Option<i32>>,
    biome: Cell<Option<u8>>,
    steep: Cell<Option<bool>>,
}

impl Ctx<'_, '_> {
    fn biome(&self) -> u8 {
        if let Some(b) = self.biome.get() {
            return b;
        }
        let b = self.bs.biome(self.sys.zoom_seed, self.x, self.y, self.z);
        self.biome.set(Some(b));
        b
    }

    fn min_surface_level(&self) -> i32 {
        if let Some(v) = self.min_surface.get() {
            return v;
        }
        let i = self.x >> 4;
        let j = self.z >> 4;
        let nc = self.bs.nc;
        let k = nc.preliminary_surface_level(self.router, i << 4, j << 4);
        let l = nc.preliminary_surface_level(self.router, (i + 1) << 4, j << 4);
        let m = nc.preliminary_surface_level(self.router, i << 4, (j + 1) << 4);
        let n = nc.preliminary_surface_level(self.router, (i + 1) << 4, (j + 1) << 4);
        let o = floor(lerp2((self.x & 15) as f64 / 16.0, (self.z & 15) as f64 / 16.0, k as f64, l as f64, m as f64, n as f64));
        let v = o + self.surface_depth - 8;
        self.min_surface.set(Some(v));
        v
    }

    fn secondary(&self) -> f64 {
        if let Some(v) = self.secondary.get() {
            return v;
        }
        let v = self.sys.surface_secondary.value(self.x as f64, 0.0, self.z as f64);
        self.secondary.set(Some(v));
        v
    }

    fn steep(&self) -> bool {
        if let Some(v) = self.steep.get() {
            return v;
        }
        let h = |x: i32, z: i32| self.heights[(z * 16 + x) as usize];
        let i = self.x & 15;
        let j = self.z & 15;
        let k = (j - 1).max(0);
        let l = (j + 1).min(15);
        let v = if h(i, l) >= h(i, k) + 4 {
            true
        } else {
            let o = (i - 1).max(0);
            let p = (i + 1).min(15);
            h(o, j) >= h(p, j) + 4
        };
        self.steep.set(Some(v));
        v
    }

    fn test(&self, c: &Cond) -> bool {
        match c {
            Cond::Biome(list) => list.contains(&self.biome()),
            Cond::Noise { n, min, max } => {
                let v = self.sys.noises[*n].value(self.x as f64, 0.0, self.z as f64);
                v >= *min && v <= *max
            }
            Cond::VGrad { rf, true_y, false_y } => {
                let k = self.y;
                if k <= *true_y {
                    return true;
                }
                if k >= *false_y {
                    return false;
                }
                let d = lerp_map(k as f64, *true_y as f64, *false_y as f64, 1.0, 0.0);
                (rf.at(self.x, k, self.z).next_float() as f64) < d
            }
            Cond::YAbove { y, mult, add } => self.y + if *add { self.stone_above } else { 0 } >= y + self.surface_depth * mult,
            Cond::Water { offset, mult, add } => self.water_height == i32::MIN || self.y + if *add { self.stone_above } else { 0 } >= self.water_height + offset + self.surface_depth * mult,
            Cond::Temperature => {
                let b = self.biome();
                let t = self.sys.temps.temperature(self.sys.climates[b as usize], self.x, self.y, self.z, self.sys.sea_level);
                t < 0.15
            }
            Cond::Steep => self.steep(),
            Cond::Not(c) => !self.test(c),
            Cond::Hole => self.surface_depth <= 0,
            Cond::AbovePreliminary => self.y >= self.min_surface_level(),
            Cond::StoneDepth { offset, add_surface, range, ceiling } => {
                let i = if *ceiling { self.stone_below } else { self.stone_above };
                let j = if *add_surface { self.surface_depth } else { 0 };
                let k = if *range == 0 { 0 } else { lerp_map(self.secondary(), -1.0, 1.0, 0.0, *range as f64) as i32 };
                i <= 1 + offset + j + k
            }
        }
    }

    fn apply(&self, rule: &Rule) -> Option<BlockState> {
        match rule {
            Rule::Block(b) => Some(*b),
            Rule::Seq(list) => list.iter().find_map(|r| self.apply(r)),
            Rule::Cond(c, r) => {
                if self.test(c) {
                    self.apply(r)
                } else {
                    None
                }
            }
            Rule::Bandlands => Some(self.sys.band(self.x, self.y, self.z)),
        }
    }
}

fn lerp_map(v: f64, a: f64, b: f64, c: f64, d: f64) -> f64 {
    super::noise::lerp((v - a) / (b - a), c, d)
}

#[inline]
fn idx(x: i32, y: i32, z: i32) -> usize {
    (((y - MIN_Y) * 16 + z) * 16 + x) as usize
}

fn is_fluid(b: BlockState) -> bool {
    BlockRegistry::get().has_fluid(b)
}

impl SurfaceSystem {
    /// `SurfaceSystem.topMaterial`: the surface rule result for a block
    /// exposed by a carver (stone depth 1 above and below).
    #[allow(clippy::too_many_arguments)]
    pub fn top_material(&self, router: &Router, bs: &BiomeSource, heights: &[i32; 256], x: i32, y: i32, z: i32, has_fluid: bool) -> Option<BlockState> {
        let ctx = Ctx {
            sys: self,
            bs,
            router,
            heights,
            x,
            y,
            z,
            stone_above: 1,
            stone_below: 1,
            water_height: if has_fluid { y + 1 } else { i32::MIN },
            surface_depth: self.surface_depth(x, z),
            secondary: Cell::new(None),
            min_surface: Cell::new(None),
            biome: Cell::new(None),
            steep: Cell::new(None),
        };
        ctx.apply(&self.rule)
    }

    /// `SurfaceSystem.buildSurface` for one chunk. `heights` is the
    /// WORLD_SURFACE_WG heightmap (highest non-air y per column, z*16+x)
    /// and is kept up to date.
    pub fn build(&self, router: &Router, bs: &BiomeSource, cx: i32, cz: i32, blocks_: &mut [BlockState], heights: &mut [i32; 256]) {
        let (min_x, min_z) = (cx * 16, cz * 16);
        let set = |blocks_: &mut [BlockState], heights: &mut [i32; 256], x: i32, y: i32, z: i32, b: BlockState| {
            blocks_[idx(x, y, z)] = b;
            if !b.is_air() && heights[(z * 16 + x) as usize] < y {
                heights[(z * 16 + x) as usize] = y;
            }
        };
        for x in 0..16 {
            for z in 0..16 {
                let (wx, wz) = (min_x + x, min_z + z);
                let top = heights[(z * 16 + x) as usize] + 1;
                let biome = bs.biome(self.zoom_seed, wx, top, wz);
                if biome == self.eroded_badlands {
                    self.eroded_badlands_extension(blocks_, heights, x, z, wx, wz, top, &set);
                }
                let top2 = heights[(z * 16 + x) as usize] + 1;
                let surface_depth = self.surface_depth(wx, wz);
                let mut ctx = Ctx {
                    sys: self,
                    bs,
                    router,
                    heights: &*heights,
                    x: wx,
                    y: 0,
                    z: wz,
                    stone_above: 0,
                    stone_below: 0,
                    water_height: i32::MIN,
                    surface_depth,
                    secondary: Cell::new(None),
                    min_surface: Cell::new(None),
                    biome: Cell::new(None),
                    steep: Cell::new(None),
                };
                let mut stone_above = 0;
                let mut water_height = i32::MIN;
                let mut next_ceiling = i32::MAX;
                let mut y = top2;
                while y >= MIN_Y {
                    let st = if y >= MIN_Y + HEIGHT { blocks::AIR } else { blocks_[idx(x, y, z)] };
                    if st.is_air() {
                        stone_above = 0;
                        water_height = i32::MIN;
                        y -= 1;
                        continue;
                    }
                    if is_fluid(st) {
                        if water_height == i32::MIN {
                            water_height = y + 1;
                        }
                        y -= 1;
                        continue;
                    }
                    if next_ceiling >= y {
                        next_ceiling = -2032 << 4;
                        let mut y2 = y - 1;
                        while y2 >= MIN_Y - 1 {
                            let s2 = if y2 < MIN_Y { blocks::VOID_AIR } else { blocks_[idx(x, y2, z)] };
                            if s2.is_air() || is_fluid(s2) {
                                next_ceiling = y2 + 1;
                                break;
                            }
                            y2 -= 1;
                        }
                    }
                    stone_above += 1;
                    let stone_below = y - next_ceiling + 1;
                    ctx.y = y;
                    ctx.stone_above = stone_above;
                    ctx.stone_below = stone_below;
                    ctx.water_height = water_height;
                    ctx.biome.set(None);
                    if st == self.default_block {
                        // rules only turn stone into other solid blocks, so
                        // the WORLD_SURFACE_WG heightmap is unaffected
                        if let Some(ns) = ctx.apply(&self.rule) {
                            blocks_[idx(x, y, z)] = ns;
                        }
                    }
                    y -= 1;
                }
                let min_surface = ctx.min_surface_level();
                drop(ctx);
                if biome == self.frozen_oceans[0] || biome == self.frozen_oceans[1] {
                    self.frozen_ocean_extension(min_surface, biome, blocks_, heights, x, z, wx, wz, top, &set);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn eroded_badlands_extension(&self, blocks_: &mut [BlockState], heights: &mut [i32; 256], x: i32, z: i32, wx: i32, wz: i32, height: i32, set: &dyn Fn(&mut [BlockState], &mut [i32; 256], i32, i32, i32, BlockState)) {
        let (fx, fz) = (wx as f64, wz as f64);
        let e = (self.badlands_surface.value(fx, 0.0, fz) * 8.25).abs().min(self.badlands_pillar.value(fx * 0.2, 0.0, fz * 0.2) * 15.0);
        if e > 0.0 {
            let h = (self.badlands_pillar_roof.value(fx * 0.75, 0.0, fz * 0.75) * 1.5).abs();
            let i = 64.0 + (e * e * 2.5).min((h * 50.0).ceil() + 24.0);
            let j = floor(i);
            if height <= j {
                let mut k = j;
                while k >= MIN_Y {
                    let s = if k >= MIN_Y + HEIGHT { blocks::AIR } else { blocks_[idx(x, k, z)] };
                    if s == self.default_block {
                        break;
                    }
                    if s.name() == "water" {
                        return;
                    }
                    k -= 1;
                }
                let mut k = j;
                while k >= MIN_Y && (k >= MIN_Y + HEIGHT || blocks_[idx(x, k, z)].is_air()) {
                    if k < MIN_Y + HEIGHT {
                        set(blocks_, heights, x, k, z, self.default_block);
                    }
                    k -= 1;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn frozen_ocean_extension(&self, min_surface: i32, biome: u8, blocks_: &mut [BlockState], heights: &mut [i32; 256], x: i32, z: i32, wx: i32, wz: i32, height: i32, set: &dyn Fn(&mut [BlockState], &mut [i32; 256], i32, i32, i32, BlockState)) {
        let (fx, fz) = (wx as f64, wz as f64);
        let e = (self.iceberg_surface.value(fx, 0.0, fz) * 8.25).abs().min(self.iceberg_pillar.value(fx * 1.28, 0.0, fz * 1.28) * 15.0);
        if e <= 1.8 {
            return;
        }
        let h = (self.iceberg_pillar_roof.value(fx * 1.17, 0.0, fz * 1.17) * 1.5).abs();
        let mut i = (e * e * 1.2).min((h * 40.0).ceil() + 14.0);
        if self.temps.temperature(self.climates[biome as usize], wx, 63, wz, self.sea_level) > 0.1 {
            i -= 2.0;
        }
        let j;
        if i > 2.0 {
            j = self.sea_level as f64 - i - 7.0;
            i += self.sea_level as f64;
        } else {
            i = 0.0;
            j = 0.0;
        }
        let k = i;
        let mut r = self.noise_random.at(wx, 0, wz);
        let l = 2 + r.next_int_bound(4);
        let m = self.sea_level + 18 + r.next_int_bound(10);
        let mut n = 0;
        let mut o = height.max(k as i32 + 1);
        while o >= min_surface {
            let st = if (MIN_Y..MIN_Y + HEIGHT).contains(&o) { blocks_[idx(x, o, z)] } else { blocks::VOID_AIR };
            if (st.is_air() && o < k as i32 && r.next_double() > 0.01) || (st.name() == "water" && o > j as i32 && o < self.sea_level && j != 0.0 && r.next_double() > 0.15) {
                if n <= l && o > m {
                    set(blocks_, heights, x, o, z, blocks::SNOW_BLOCK);
                    n += 1;
                } else {
                    set(blocks_, heights, x, o, z, blocks::PACKED_ICE);
                }
            }
            o -= 1;
        }
    }
}
