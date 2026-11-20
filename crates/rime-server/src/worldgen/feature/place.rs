//! Value providers, block predicates, block state providers, rule tests and
//! placement modifiers (vanilla 26.1 semantics, parsed from the data pack).

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::{Builder, Hm, Region};
use crate::registry::{vstate, BlockRegistry, BlockState};
use crate::worldgen::carver::{anchor, random_between};
use crate::worldgen::chunk::{HEIGHT, MIN_Y};
use crate::worldgen::random::{Rng, WgRandom};

// ---------------------------------------------------------------- helpers

/// `Mth.randomBetweenInclusive`
pub fn between(r: &mut impl Rng, min: i32, max: i32) -> i32 {
    r.next_int_bound(max - min + 1) + min
}

/// `Mth.nextInt(random, a, b)`
pub fn mth_next_int(r: &mut impl Rng, a: i32, b: i32) -> i32 {
    if a >= b {
        a
    } else {
        r.next_int_bound(b - a + 1) + a
    }
}

/// Parse `{"Name": ..., "Properties": {...}}`.
pub fn parse_state(v: &Value) -> Result<BlockState> {
    let name = v["Name"].as_str().ok_or_else(|| anyhow!("block state without Name: {v}"))?;
    let props: Vec<(String, String)> = v["Properties"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect()).unwrap_or_default();
    let props: Vec<(&str, &str)> = props.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    BlockRegistry::get().state(name, &props).ok_or_else(|| anyhow!("unknown block state {v}"))
}

fn ints(v: &Value) -> [i32; 3] {
    let a = v.as_array();
    let g = |i: usize| a.and_then(|a| a.get(i)).and_then(|x| x.as_i64()).unwrap_or(0) as i32;
    [g(0), g(1), g(2)]
}

/// Set of blocks (by block, not state), as a per-state table.
#[derive(Clone)]
pub struct BlockSet(pub Arc<Vec<bool>>);

impl BlockSet {
    pub fn contains(&self, s: BlockState) -> bool {
        self.0.get(s.0 as usize).copied().unwrap_or(false)
    }
    fn from_names(names: &[String]) -> BlockSet {
        let names: std::collections::HashSet<&str> = names.iter().map(|n| n.trim_start_matches("minecraft:")).collect();
        BlockSet(Arc::new((0..BlockRegistry::get().state_count()).map(|i| names.contains(BlockState(i as u16).name())).collect()))
    }
}

/// Parse a block list / `#tag` / single block (HolderSet<Block>).
pub fn block_set(lib: &mut Builder, v: &Value) -> Result<BlockSet> {
    match v {
        Value::String(s) if s.starts_with('#') => lib.tag(s),
        Value::String(s) => Ok(BlockSet::from_names(std::slice::from_ref(s))),
        Value::Array(a) => Ok(BlockSet::from_names(&a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>())),
        _ => Err(anyhow!("bad block set {v}")),
    }
}

pub fn tag_set(lib: &mut Builder, tag: &str) -> Result<BlockSet> {
    lib.tag(tag)
}

// ---------------------------------------------------------------- int providers

#[derive(Clone, Debug)]
pub enum IntP {
    Const(i32),
    Uniform(i32, i32),
    Biased(i32, i32),
    Clamped(Box<IntP>, i32, i32),
    ClampedNormal { mean: f32, dev: f32, min: i32, max: i32 },
    Weighted(Vec<(IntP, i32)>, i32),
    Trapezoid { min: i32, max: i32, plateau: i32 },
}

impl IntP {
    pub fn parse(v: &Value) -> Result<Self> {
        if let Some(i) = v.as_i64() {
            return Ok(IntP::Const(i as i32));
        }
        let g = |k: &str| v[k].as_i64().map(|x| x as i32).ok_or_else(|| anyhow!("int provider missing {k}: {v}"));
        Ok(match v["type"].as_str().unwrap_or("") {
            "minecraft:constant" => IntP::Const(g("value")?),
            "minecraft:uniform" => IntP::Uniform(g("min_inclusive")?, g("max_inclusive")?),
            "minecraft:biased_to_bottom" => IntP::Biased(g("min_inclusive")?, g("max_inclusive")?),
            "minecraft:clamped" => IntP::Clamped(Box::new(IntP::parse(&v["source"])?), g("min_inclusive")?, g("max_inclusive")?),
            "minecraft:clamped_normal" => IntP::ClampedNormal { mean: v["mean"].as_f64().unwrap_or(0.0) as f32, dev: v["deviation"].as_f64().unwrap_or(0.0) as f32, min: g("min_inclusive")?, max: g("max_inclusive")? },
            "minecraft:weighted_list" => {
                let mut list = Vec::new();
                for e in v["distribution"].as_array().into_iter().flatten() {
                    list.push((IntP::parse(&e["data"])?, e["weight"].as_i64().unwrap_or(1) as i32));
                }
                let total = list.iter().map(|x| x.1).sum();
                IntP::Weighted(list, total)
            }
            "minecraft:trapezoid" => IntP::Trapezoid { min: g("min")?, max: g("max")?, plateau: v["plateau"].as_i64().unwrap_or(0) as i32 },
            t => return Err(anyhow!("unsupported int provider {t}")),
        })
    }

    pub fn sample(&self, r: &mut WgRandom) -> i32 {
        match self {
            IntP::Const(c) => *c,
            IntP::Uniform(a, b) => between(r, *a, *b),
            IntP::Biased(a, b) => {
                let i = r.next_int_bound(b - a + 1) + 1;
                a + r.next_int_bound(i)
            }
            IntP::Clamped(s, a, b) => s.sample(r).clamp(*a, *b),
            IntP::ClampedNormal { mean, dev, min, max } => {
                let n = mean + r.next_gaussian() as f32 * dev;
                n.clamp(*min as f32, *max as f32) as i32
            }
            IntP::Weighted(list, total) => {
                let mut i = r.next_int_bound(*total);
                for (p, w) in list {
                    i -= w;
                    if i < 0 {
                        return p.sample(r);
                    }
                }
                0
            }
            IntP::Trapezoid { min, max, plateau } => {
                if *plateau == 0 && *max == -*min {
                    let a = r.next_int_bound(max + 1);
                    let b = r.next_int_bound(max + 1);
                    return a - b;
                }
                let range = max - min;
                if *plateau == range {
                    return between(r, *min, *max);
                }
                let f3 = (range - plateau) / 2;
                let f4 = range - f3;
                min + between(r, 0, f4) + between(r, 0, f3)
            }
        }
    }

    pub fn max_value(&self) -> i32 {
        match self {
            IntP::Const(c) => *c,
            IntP::Uniform(_, b) | IntP::Biased(_, b) => *b,
            IntP::Clamped(_, _, b) => *b,
            IntP::ClampedNormal { max, .. } => *max,
            IntP::Weighted(l, _) => l.iter().map(|x| x.0.max_value()).max().unwrap_or(0),
            IntP::Trapezoid { max, .. } => *max,
        }
    }
}

// ---------------------------------------------------------------- float providers



// ---------------------------------------------------------------- height providers

#[derive(Clone, Debug)]
pub enum HeightP {
    Const(i32),
    Uniform(i32, i32),
    Trapezoid(i32, i32, i32),
    Biased(i32, i32, i32),
    VeryBiased(i32, i32, i32),
    Weighted(Vec<(HeightP, i32)>, i32),
}

impl HeightP {
    pub fn parse(v: &Value) -> Result<Self> {
        let a = |k: &str| anchor(&v[k]);
        let inner = v["inner"].as_i64().unwrap_or(1) as i32;
        Ok(match v["type"].as_str() {
            None => HeightP::Const(anchor(v)?),
            Some("minecraft:constant") => HeightP::Const(a("value")?),
            Some("minecraft:uniform") => HeightP::Uniform(a("min_inclusive")?, a("max_inclusive")?),
            Some("minecraft:trapezoid") => HeightP::Trapezoid(a("min_inclusive")?, a("max_inclusive")?, v["plateau"].as_i64().unwrap_or(0) as i32),
            Some("minecraft:biased_to_bottom") => HeightP::Biased(a("min_inclusive")?, a("max_inclusive")?, inner),
            Some("minecraft:very_biased_to_bottom") => HeightP::VeryBiased(a("min_inclusive")?, a("max_inclusive")?, inner),
            Some("minecraft:weighted_list") => {
                let mut list = Vec::new();
                for e in v["distribution"].as_array().into_iter().flatten() {
                    list.push((HeightP::parse(&e["data"])?, e["weight"].as_i64().unwrap_or(1) as i32));
                }
                let total = list.iter().map(|x| x.1).sum();
                HeightP::Weighted(list, total)
            }
            Some(t) => return Err(anyhow!("unsupported height provider {t}")),
        })
    }

    pub fn sample(&self, r: &mut WgRandom) -> i32 {
        match self {
            HeightP::Const(c) => *c,
            HeightP::Uniform(a, b) => {
                if a > b {
                    *a
                } else {
                    between(r, *a, *b)
                }
            }
            HeightP::Trapezoid(min, max, plateau) => {
                if min > max {
                    return *min;
                }
                let range = max - min;
                if *plateau >= range {
                    return between(r, *min, *max);
                }
                let f6 = (range - plateau) / 2;
                let f7 = range - f6;
                min + between(r, 0, f7) + between(r, 0, f6)
            }
            HeightP::Biased(min, max, inner) => {
                if max - min - inner + 1 <= 0 {
                    return *min;
                }
                let i = r.next_int_bound(max - min - inner + 1);
                r.next_int_bound(i + inner) + min
            }
            HeightP::VeryBiased(min, max, inner) => {
                if max - min - inner + 1 <= 0 {
                    return *min;
                }
                let i = mth_next_int(r, min + inner, *max);
                let j = mth_next_int(r, *min, i - 1);
                mth_next_int(r, *min, j - 1 + inner)
            }
            HeightP::Weighted(list, total) => {
                let mut i = r.next_int_bound(*total);
                for (p, w) in list {
                    i -= w;
                    if i < 0 {
                        return p.sample(r);
                    }
                }
                0
            }
        }
    }
}

// ---------------------------------------------------------------- block predicates

#[derive(Clone)]
pub enum Pred {
    True,
    Blocks([i32; 3], BlockSet),
    Fluids([i32; 3], Vec<u8>),
    Solid([i32; 3]),
    Replaceable([i32; 3]),
    WouldSurvive([i32; 3], BlockState),
    InsideWorld([i32; 3]),
    Sturdy([i32; 3], u8),
    AllOf(Vec<Pred>),
    AnyOf(Vec<Pred>),
    Not(Box<Pred>),
    Unobstructed,
}

impl Pred {
    pub fn parse(lib: &mut Builder, v: &Value) -> Result<Pred> {
        let off = ints(&v["offset"]);
        let list = |lib: &mut Builder| -> Result<Vec<Pred>> { v["predicates"].as_array().into_iter().flatten().map(|p| Pred::parse(lib, p)).collect() };
        Ok(match v["type"].as_str().unwrap_or("") {
            "minecraft:true" => Pred::True,
            "minecraft:matching_blocks" => Pred::Blocks(off, block_set(lib, &v["blocks"])?),
            "minecraft:matching_block_tag" => Pred::Blocks(off, lib.tag(v["tag"].as_str().unwrap_or(""))?),
            "minecraft:matching_fluids" => {
                let names: Vec<String> = match &v["fluids"] {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                    _ => vec![],
                };
                let mut codes = Vec::new();
                for n in names {
                    match n.trim_start_matches("minecraft:") {
                        "water" => codes.push(1),
                        "flowing_water" => codes.push(2),
                        "lava" => codes.push(3),
                        "flowing_lava" => codes.push(4),
                        "empty" => codes.push(0),
                        _ => {}
                    }
                }
                Pred::Fluids(off, codes)
            }
            "minecraft:solid" => Pred::Solid(off),
            "minecraft:replaceable" => Pred::Replaceable(off),
            "minecraft:would_survive" => Pred::WouldSurvive(off, parse_state(&v["state"])?),
            "minecraft:inside_world_bounds" => Pred::InsideWorld(off),
            "minecraft:has_sturdy_face" => Pred::Sturdy(off, dir_index(v["direction"].as_str().unwrap_or("up"))),
            "minecraft:all_of" => Pred::AllOf(list(lib)?),
            "minecraft:any_of" => Pred::AnyOf(list(lib)?),
            "minecraft:not" => Pred::Not(Box::new(Pred::parse(lib, &v["predicate"])?)),
            "minecraft:unobstructed" => Pred::Unobstructed,
            t => return Err(anyhow!("unsupported block predicate {t}")),
        })
    }

    pub fn test(&self, lv: &Region, p: [i32; 3]) -> bool {
        let at = |o: &[i32; 3]| [p[0] + o[0], p[1] + o[1], p[2] + o[2]];
        match self {
            Pred::True | Pred::Unobstructed => true,
            Pred::Blocks(o, set) => set.contains(lv.get(at(o))),
            Pred::Fluids(o, codes) => {
                let f = vstate::fluid(lv.get(at(o)));
                codes.contains(&f)
            }
            Pred::Solid(o) => vstate::solid(lv.get(at(o))),
            Pred::Replaceable(o) => vstate::replaceable(lv.get(at(o))),
            Pred::WouldSurvive(o, s) => super::survive::can_survive(lv, *s, at(o)),
            Pred::InsideWorld(o) => {
                let y = p[1] + o[1];
                (MIN_Y..MIN_Y + HEIGHT).contains(&y)
            }
            Pred::Sturdy(o, d) => super::survive::face_sturdy(lv.get(at(o)), *d),
            Pred::AllOf(l) => l.iter().all(|x| x.test(lv, p)),
            Pred::AnyOf(l) => l.iter().any(|x| x.test(lv, p)),
            Pred::Not(x) => !x.test(lv, p),
        }
    }
}

/// Direction index in vanilla order: down, up, north, south, west, east.
pub fn dir_index(s: &str) -> u8 {
    match s {
        "down" => 0,
        "up" => 1,
        "north" => 2,
        "south" => 3,
        "west" => 4,
        _ => 5,
    }
}

pub const DIRS: [[i32; 3]; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];

// ---------------------------------------------------------------- state providers

#[derive(Clone)]
pub enum StateP {
    Simple(BlockState),
    Weighted(Vec<(BlockState, i32)>, i32),
    RandomizedInt { source: Box<StateP>, property: String, values: IntP },
    Rotated(BlockState),
    RuleBased { fallback: Option<Box<StateP>>, rules: Vec<(Pred, StateP)> },
    /// noise based providers (not ported yet): their default state
    Unsupported(BlockState),
}

impl StateP {
    pub fn parse(lib: &mut Builder, v: &Value) -> Result<StateP> {
        Ok(match v["type"].as_str().unwrap_or("") {
            "minecraft:simple_state_provider" => StateP::Simple(parse_state(&v["state"])?),
            "minecraft:rotated_block_provider" => StateP::Rotated(parse_state(&v["state"])?),
            "minecraft:weighted_state_provider" => {
                let mut list = Vec::new();
                for e in v["entries"].as_array().into_iter().flatten() {
                    list.push((parse_state(&e["data"])?, e["weight"].as_i64().unwrap_or(1) as i32));
                }
                let total = list.iter().map(|x| x.1).sum();
                StateP::Weighted(list, total)
            }
            "minecraft:randomized_int_state_provider" => StateP::RandomizedInt { source: Box::new(StateP::parse(lib, &v["source"])?), property: v["property"].as_str().unwrap_or("").to_string(), values: IntP::parse(&v["values"])? },
            "minecraft:rule_based_state_provider" => {
                let fallback = if v["fallback"].is_object() { Some(Box::new(StateP::parse(lib, &v["fallback"])?)) } else { None };
                let mut rules = Vec::new();
                for r in v["rules"].as_array().into_iter().flatten() {
                    rules.push((Pred::parse(lib, &r["if_true"])?, StateP::parse(lib, &r["then"])?));
                }
                StateP::RuleBased { fallback, rules }
            }
            _ => {
                let s = v["default_state"].as_object().map(|_| parse_state(&v["default_state"])).transpose()?.or_else(|| v["states"].as_array().and_then(|a| a.first()).and_then(|s| parse_state(s).ok()));
                StateP::Unsupported(s.unwrap_or(crate::registry::blocks::AIR))
            }
        })
    }

    /// `BlockStateProvider.getState(level, random, pos)`; None when a
    /// rule based provider has no matching rule and no fallback.
    pub fn get(&self, lv: &Region, r: &mut WgRandom, p: [i32; 3]) -> Option<BlockState> {
        Some(match self {
            StateP::Simple(s) | StateP::Unsupported(s) => *s,
            StateP::Weighted(list, total) => {
                let mut i = r.next_int_bound(*total);
                let mut out = list[0].0;
                for (s, w) in list {
                    i -= w;
                    if i < 0 {
                        out = *s;
                        break;
                    }
                }
                out
            }
            StateP::RandomizedInt { source, property, values } => {
                let s = source.get(lv, r, p)?;
                let v = values.sample(r);
                BlockRegistry::get().with(s, property, &v.to_string()).unwrap_or(s)
            }
            StateP::Rotated(s) => {
                let axis = ["x", "y", "z"][r.next_int_bound(3) as usize];
                BlockRegistry::get().with(*s, "axis", axis).unwrap_or(*s)
            }
            StateP::RuleBased { fallback, rules } => {
                for (pred, then) in rules {
                    if pred.test(lv, p) {
                        return then.get(lv, r, p);
                    }
                }
                return fallback.as_ref().and_then(|f| f.get(lv, r, p));
            }
        })
    }
}

// ---------------------------------------------------------------- rule tests (ores)

#[derive(Clone)]
pub enum RuleTest {
    Always,
    Tag(BlockSet),
    Block(BlockSet),
    State(BlockState),
    RandomBlock(BlockSet, f32),
}

impl RuleTest {
    pub fn parse(lib: &mut Builder, v: &Value) -> Result<RuleTest> {
        Ok(match v["predicate_type"].as_str().unwrap_or("") {
            "minecraft:always_true" => RuleTest::Always,
            "minecraft:tag_match" => RuleTest::Tag(lib.tag(v["tag"].as_str().unwrap_or(""))?),
            "minecraft:block_match" => RuleTest::Block(block_set(lib, &v["block"])?),
            "minecraft:blockstate_match" => RuleTest::State(parse_state(&v["block_state"])?),
            "minecraft:random_block_match" => RuleTest::RandomBlock(block_set(lib, &v["block"])?, v["probability"].as_f64().unwrap_or(0.0) as f32),
            t => return Err(anyhow!("unsupported rule test {t}")),
        })
    }

    pub fn test(&self, s: BlockState, r: &mut WgRandom) -> bool {
        match self {
            RuleTest::Always => true,
            RuleTest::Tag(t) | RuleTest::Block(t) => t.contains(s),
            RuleTest::State(x) => *x == s,
            RuleTest::RandomBlock(t, p) => t.contains(s) && r.next_float() < *p,
        }
    }
}

// ---------------------------------------------------------------- placement modifiers

#[derive(Clone)]
pub enum Modifier {
    Biome,
    InSquare,
    Count(IntP),
    Heightmap(Hm),
    Filter(Pred),
    RandomOffset(IntP, IntP),
    HeightRange(HeightP),
    Rarity(i32),
    WaterDepth(i32),
    Scan { dir: u8, target: Pred, allowed: Pred, max_steps: i32 },
    NoiseThreshold { level: f64, below: i32, above: i32 },
    NoiseBased { ratio: i32, factor: f64, offset: f64 },
    SurfaceRelative { hm: Hm, min: i32, max: i32 },
    Fixed(Vec<[i32; 3]>),
}

impl Modifier {
    pub fn parse(lib: &mut Builder, v: &Value) -> Result<Modifier> {
        let i = |k: &str| v[k].as_i64().unwrap_or(0) as i32;
        Ok(match v["type"].as_str().unwrap_or("") {
            "minecraft:biome" => Modifier::Biome,
            "minecraft:in_square" => Modifier::InSquare,
            "minecraft:count" => Modifier::Count(IntP::parse(&v["count"])?),
            "minecraft:heightmap" => Modifier::Heightmap(Hm::parse(v["heightmap"].as_str().unwrap_or(""))),
            "minecraft:block_predicate_filter" => Modifier::Filter(Pred::parse(lib, &v["predicate"])?),
            "minecraft:random_offset" => Modifier::RandomOffset(IntP::parse(&v["xz_spread"])?, IntP::parse(&v["y_spread"])?),
            "minecraft:height_range" => Modifier::HeightRange(HeightP::parse(&v["height"])?),
            "minecraft:rarity_filter" => Modifier::Rarity(i("chance")),
            "minecraft:surface_water_depth_filter" => Modifier::WaterDepth(i("max_water_depth")),
            "minecraft:environment_scan" => Modifier::Scan {
                dir: dir_index(v["direction_of_search"].as_str().unwrap_or("down")),
                target: Pred::parse(lib, &v["target_condition"])?,
                allowed: if v["allowed_search_condition"].is_object() { Pred::parse(lib, &v["allowed_search_condition"])? } else { Pred::True },
                max_steps: i("max_steps"),
            },
            "minecraft:noise_threshold_count" => Modifier::NoiseThreshold { level: v["noise_level"].as_f64().unwrap_or(0.0), below: i("below_noise"), above: i("above_noise") },
            "minecraft:noise_based_count" => Modifier::NoiseBased { ratio: i("noise_to_count_ratio"), factor: v["noise_factor"].as_f64().unwrap_or(1.0), offset: v["noise_offset"].as_f64().unwrap_or(0.0) },
            "minecraft:surface_relative_threshold_filter" => Modifier::SurfaceRelative { hm: Hm::parse(v["heightmap"].as_str().unwrap_or("")), min: v["min_inclusive"].as_i64().map_or(i32::MIN, |x| x as i32), max: v["max_inclusive"].as_i64().map_or(i32::MAX, |x| x as i32) },
            "minecraft:fixed_placement" => Modifier::Fixed(v["positions"].as_array().into_iter().flatten().map(ints).collect()),
            t => return Err(anyhow!("unsupported placement modifier {t}")),
        })
    }

    /// `getPositions`: eager list for one input position.
    pub fn positions(&self, lv: &Region, r: &mut WgRandom, top: Option<&str>, p: [i32; 3]) -> Vec<[i32; 3]> {
        let one = |ok: bool| if ok { vec![p] } else { vec![] };
        match self {
            Modifier::Biome => {
                let b = lv.biome(p);
                one(top.is_some_and(|t| lv.lib.biome_has(b, t)))
            }
            Modifier::InSquare => {
                let x = r.next_int_bound(16) + p[0];
                let z = r.next_int_bound(16) + p[2];
                vec![[x, p[1], z]]
            }
            Modifier::Count(c) => vec![p; c.sample(r).max(0) as usize],
            Modifier::Heightmap(hm) => {
                let y = lv.height(*hm, p[0], p[2]);
                if y > MIN_Y {
                    vec![[p[0], y, p[2]]]
                } else {
                    vec![]
                }
            }
            Modifier::Filter(pred) => one(pred.test(lv, p)),
            Modifier::RandomOffset(xz, y) => {
                let x = p[0] + xz.sample(r);
                let yy = p[1] + y.sample(r);
                let z = p[2] + xz.sample(r);
                vec![[x, yy, z]]
            }
            Modifier::HeightRange(h) => vec![[p[0], h.sample(r), p[2]]],
            Modifier::Rarity(chance) => one(r.next_float() < 1.0 / *chance as f32),
            Modifier::WaterDepth(max) => {
                let floor = lv.height(Hm::Motion, p[0], p[2]);
                let surface = lv.height(Hm::NotAir, p[0], p[2]);
                one(surface - floor <= *max)
            }
            Modifier::Scan { dir, target, allowed, max_steps } => {
                let mut q = p;
                if !allowed.test(lv, q) {
                    return vec![];
                }
                let d = DIRS[*dir as usize];
                for _ in 0..*max_steps {
                    if target.test(lv, q) {
                        return vec![q];
                    }
                    q = [q[0] + d[0], q[1] + d[1], q[2] + d[2]];
                    if !(MIN_Y..MIN_Y + HEIGHT).contains(&q[1]) {
                        return vec![];
                    }
                    if !allowed.test(lv, q) {
                        break;
                    }
                }
                if target.test(lv, q) {
                    vec![q]
                } else {
                    vec![]
                }
            }
            Modifier::NoiseThreshold { level, below, above } => {
                let n = lv.lib.biome_info_noise(p[0] as f64 / 200.0, p[2] as f64 / 200.0);
                vec![p; (if n < *level { *below } else { *above }).max(0) as usize]
            }
            Modifier::NoiseBased { ratio, factor, offset } => {
                let n = lv.lib.biome_info_noise(p[0] as f64 / factor, p[2] as f64 / factor);
                vec![p; (((n + offset) * *ratio as f64).ceil() as i32).max(0) as usize]
            }
            Modifier::SurfaceRelative { hm, min, max } => {
                let h = lv.height(*hm, p[0], p[2]) as i64;
                one(h + *min as i64 <= p[1] as i64 && p[1] as i64 <= h + *max as i64)
            }
            Modifier::Fixed(list) => list.clone(),
        }
    }
}

/// `randomBetween` re-export for features.
pub fn rand_between_f(r: &mut WgRandom, a: f32, b: f32) -> f32 {
    random_between(r, a, b)
}
