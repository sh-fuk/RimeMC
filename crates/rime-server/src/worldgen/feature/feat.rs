//! Configured feature types (ported from the 26.1 `Feature` classes).

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use super::place::{parse_state, IntP, Pred, RuleTest, StateP, DIRS};
use super::{Builder, Placed, Region};
use crate::registry::{vstate, BlockRegistry, BlockState};
use crate::worldgen::carver::msin;
use crate::worldgen::chunk::{HEIGHT, MIN_Y};
use crate::worldgen::random::{Rng, WgRandom};

pub struct OreTarget {
    pub target: RuleTest,
    pub state: BlockState,
}

pub enum Feature {
    NoOp,
    /// a feature type that is not ported yet
    Unsupported(String),
    Ore { size: i32, discard: f32, targets: Vec<OreTarget> },
    ScatteredOre { size: i32, discard: f32, targets: Vec<OreTarget> },
    SimpleBlock { provider: StateP, schedule_tick: bool },
    Disk { provider: StateP, target: Pred, radius: IntP, half_height: i32 },
    RandomSelector { features: Vec<(Arc<Placed>, f32)>, default: Arc<Placed> },
    SimpleRandomSelector(Vec<Arc<Placed>>),
    RandomBoolean(Arc<Placed>, Arc<Placed>),
    Tree(Box<super::tree::Tree>),
    Extra(Box<super::extra::Extra>),
}

fn targets(b: &mut Builder, v: &Value) -> Result<Vec<OreTarget>> {
    let mut out = Vec::new();
    for t in v["targets"].as_array().into_iter().flatten() {
        out.push(OreTarget { target: RuleTest::parse(b, &t["target"])?, state: parse_state(&t["state"])? });
    }
    Ok(out)
}

impl Feature {
    pub fn parse(b: &mut Builder, v: &Value) -> Result<Feature> {
        let ty = v["type"].as_str().unwrap_or("").trim_start_matches("minecraft:").to_string();
        let c = &v["config"];
        Ok(match ty.as_str() {
            "no_op" => Feature::NoOp,
            "ore" => Feature::Ore { size: c["size"].as_i64().unwrap_or(0) as i32, discard: c["discard_chance_on_air_exposure"].as_f64().unwrap_or(0.0) as f32, targets: targets(b, c)? },
            "scattered_ore" => Feature::ScatteredOre { size: c["size"].as_i64().unwrap_or(0) as i32, discard: c["discard_chance_on_air_exposure"].as_f64().unwrap_or(0.0) as f32, targets: targets(b, c)? },
            "simple_block" => Feature::SimpleBlock { provider: StateP::parse(b, &c["to_place"])?, schedule_tick: c["schedule_tick"].as_bool().unwrap_or(false) },
            "disk" => Feature::Disk { provider: StateP::parse(b, &c["state_provider"])?, target: Pred::parse(b, &c["target"])?, radius: IntP::parse(&c["radius"])?, half_height: c["half_height"].as_i64().unwrap_or(0) as i32 },
            "random_selector" => {
                let mut features = Vec::new();
                for f in c["features"].as_array().into_iter().flatten() {
                    features.push((b.placed(&f["feature"])?, f["chance"].as_f64().unwrap_or(0.0) as f32));
                }
                Feature::RandomSelector { features, default: b.placed(&c["default"])? }
            }
            "simple_random_selector" => {
                let list: Vec<Value> = match &c["features"] {
                    Value::Array(a) => a.clone(),
                    other => vec![other.clone()],
                };
                Feature::SimpleRandomSelector(list.iter().map(|f| b.placed(f)).collect::<Result<_>>()?)
            }
            "tree" => match super::tree::Tree::parse(b, c) {
                Ok(t) => Feature::Tree(Box::new(t)),
                Err(e) => {
                    *b.missing.entry(format!("tree ({e})")).or_default() += 1;
                    Feature::Unsupported(ty)
                }
            },
            "random_boolean_selector" => Feature::RandomBoolean(b.placed(&c["feature_true"])?, b.placed(&c["feature_false"])?),
            _ => match super::extra::Extra::parse(b, &ty, c) {
                Some(Ok(e)) => Feature::Extra(Box::new(e)),
                Some(Err(e)) => {
                    *b.missing.entry(format!("{ty} ({e})")).or_default() += 1;
                    Feature::Unsupported(ty)
                }
                None => {
                    *b.missing.entry(ty.clone()).or_default() += 1;
                    Feature::Unsupported(ty)
                }
            },
        })
    }

    /// `ConfiguredFeature.place`: only when the origin is writable.
    pub fn place(&self, lv: &mut Region, r: &mut WgRandom, p: [i32; 3]) -> bool {
        if !lv.can_write(p) {
            return false;
        }
        match self {
            Feature::NoOp | Feature::Unsupported(_) => false,
            Feature::Tree(t) => t.place(lv, r, p),
            Feature::Extra(e) => e.place(lv, r, p),
            Feature::Ore { size, discard, targets } => ore(lv, r, p, *size, *discard, targets),
            Feature::ScatteredOre { size, discard, targets } => {
                let n = r.next_int_bound(size + 1);
                for j in 0..n {
                    let range = j.min(7) as f32;
                    let mut off = [0; 3];
                    for o in &mut off {
                        let f = (r.next_float() - r.next_float()) * range;
                        *o = (f as f64 + 0.5).floor() as i32;
                    }
                    let q = [p[0] + off[0], p[1] + off[1], p[2] + off[2]];
                    let st = lv.get(q);
                    for t in targets {
                        if can_place_ore(lv, st, r, *discard, t, q) {
                            lv.set(q, t.state);
                            break;
                        }
                    }
                }
                true
            }
            Feature::SimpleBlock { provider, .. } => {
                let Some(s) = provider.get(lv, r, p) else { return false };
                if !super::survive::can_survive(lv, s, p) {
                    return false;
                }
                if super::survive::kind(s) == super::survive::Kind::DoublePlant || s.name().starts_with("tall_seagrass") {
                    let up = [p[0], p[1] + 1, p[2]];
                    if !lv.is_empty(up) {
                        return false;
                    }
                    place_double_plant(lv, s, p);
                } else {
                    lv.set(p, s);
                }
                true
            }
            Feature::Disk { provider, target, radius, half_height } => {
                let top = p[1] + half_height;
                let bottom = p[1] - half_height - 1;
                let l = radius.sample(r);
                let mut placed = false;
                for z in p[2] - l..=p[2] + l {
                    for x in p[0] - l..=p[0] + l {
                        let (m, n) = (x - p[0], z - p[2]);
                        if m * m + n * n > l * l {
                            continue;
                        }
                        let mut y = top;
                        while y > bottom {
                            let q = [x, y, z];
                            if target.test(lv, q) {
                                if let Some(s) = provider.get(lv, r, q) {
                                    lv.set(q, s);
                                    placed = true;
                                }
                            }
                            y -= 1;
                        }
                    }
                }
                placed
            }
            Feature::RandomSelector { features, default } => {
                for (f, chance) in features {
                    if r.next_float() < *chance {
                        return f.place(lv, r, p);
                    }
                }
                default.place(lv, r, p)
            }
            Feature::SimpleRandomSelector(list) => {
                let i = r.next_int_bound(list.len() as i32) as usize;
                list[i].place(lv, r, p)
            }
            Feature::RandomBoolean(t, f) => {
                if r.next_bool() {
                    t.place(lv, r, p)
                } else {
                    f.place(lv, r, p)
                }
            }
        }
    }
}

/// `DoublePlantBlock.placeAt`
pub fn place_double_plant(lv: &mut Region, s: BlockState, p: [i32; 3]) {
    let reg = BlockRegistry::get();
    let up = [p[0], p[1] + 1, p[2]];
    let water = |lv: &Region, q| reg.with(s, "waterlogged", "false").is_some() && vstate::is_water(lv.get(q));
    let lower = reg.with(s, "half", "lower").unwrap_or(s);
    let lower = if water(lv, p) { reg.with(lower, "waterlogged", "true").unwrap_or(lower) } else { lower };
    lv.set(p, lower);
    let upper = reg.with(s, "half", "upper").unwrap_or(s);
    let upper = if water(lv, up) { reg.with(upper, "waterlogged", "true").unwrap_or(upper) } else { upper };
    lv.set(up, upper);
}

/// `Feature.isAdjacentToAir`
fn adjacent_to_air(lv: &Region, p: [i32; 3]) -> bool {
    DIRS.iter().any(|d| vstate::air(lv.get([p[0] + d[0], p[1] + d[1], p[2] + d[2]])))
}

/// `OreFeature.canPlaceOre`
fn can_place_ore(lv: &Region, st: BlockState, r: &mut WgRandom, discard: f32, t: &OreTarget, p: [i32; 3]) -> bool {
    if !t.target.test(st, r) {
        return false;
    }
    // shouldSkipAirCheck
    let skip = if discard <= 0.0 {
        true
    } else if discard >= 1.0 {
        false
    } else {
        r.next_float() >= discard
    };
    skip || !adjacent_to_air(lv, p)
}

fn ore(lv: &mut Region, r: &mut WgRandom, p: [i32; 3], size: i32, discard: f32, targets: &[OreTarget]) -> bool {
    let f = r.next_float() * std::f32::consts::PI;
    let g = size as f32 / 8.0;
    let i = ((size as f32 / 16.0 * 2.0 + 1.0) / 2.0).ceil() as i32;
    let (fs, fc) = ((f as f64).sin(), (f as f64).cos());
    let d = p[0] as f64 + fs * g as f64;
    let e = p[0] as f64 - fs * g as f64;
    let h = p[2] as f64 + fc * g as f64;
    let j = p[2] as f64 - fc * g as f64;
    let l = (p[1] + r.next_int_bound(3) - 2) as f64;
    let m = (p[1] + r.next_int_bound(3) - 2) as f64;
    let gc = g.ceil() as i32;
    let n = p[0] - gc - i;
    let o = p[1] - 2 - i;
    let pz = p[2] - gc - i;
    let q = 2 * (gc + i);
    let rr = 2 * (2 + i);
    for s in n..=n + q {
        for t in pz..=pz + q {
            if o <= lv.height(super::Hm::Motion, s, t) {
                return ore_place(lv, r, size, discard, targets, [d, e, h, j, l, m], n, o, pz, q, rr);
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn ore_place(lv: &mut Region, r: &mut WgRandom, size: i32, discard: f32, targets: &[OreTarget], a: [f64; 6], j: i32, k: i32, l: i32, m: i32, n: i32) -> bool {
    let [d, e, f, g, h, i] = a;
    let mut placed = 0;
    // java.util.BitSet grows on demand
    let mut bits = vec![false; (m * n * m).max(0) as usize];
    let p = size as usize;
    let mut ds = vec![0.0f64; p * 4];
    for q in 0..p {
        let rr = q as f32 / p as f32;
        let s = d + rr as f64 * (e - d);
        let t = h + rr as f64 * (i - h);
        let u = f + rr as f64 * (g - f);
        let v = r.next_double() * p as f64 / 16.0;
        let w = ((msin((std::f32::consts::PI * rr) as f64) + 1.0f32) as f64 * v + 1.0) / 2.0;
        ds[q * 4] = s;
        ds[q * 4 + 1] = t;
        ds[q * 4 + 2] = u;
        ds[q * 4 + 3] = w;
    }
    for q in 0..p.saturating_sub(1) {
        if ds[q * 4 + 3] <= 0.0 {
            continue;
        }
        for x in q + 1..p {
            if ds[x * 4 + 3] <= 0.0 {
                continue;
            }
            let s = ds[q * 4] - ds[x * 4];
            let t = ds[q * 4 + 1] - ds[x * 4 + 1];
            let u = ds[q * 4 + 2] - ds[x * 4 + 2];
            let v = ds[q * 4 + 3] - ds[x * 4 + 3];
            if v * v > s * s + t * t + u * u {
                if v > 0.0 {
                    ds[x * 4 + 3] = -1.0;
                } else {
                    ds[q * 4 + 3] = -1.0;
                }
            }
        }
    }
    for x in 0..p {
        let s = ds[x * 4 + 3];
        if s < 0.0 {
            continue;
        }
        let (t, u, v) = (ds[x * 4], ds[x * 4 + 1], ds[x * 4 + 2]);
        let y0 = ((t - s).floor() as i32).max(j);
        let z0 = ((u - s).floor() as i32).max(k);
        let a0 = ((v - s).floor() as i32).max(l);
        let y1 = ((t + s).floor() as i32).max(y0);
        let z1 = ((u + s).floor() as i32).max(z0);
        let a1 = ((v + s).floor() as i32).max(a0);
        for ae in y0..=y1 {
            let af = (ae as f64 + 0.5 - t) / s;
            if af * af >= 1.0 {
                continue;
            }
            for ag in z0..=z1 {
                let ah = (ag as f64 + 0.5 - u) / s;
                if af * af + ah * ah >= 1.0 {
                    continue;
                }
                for ai in a0..=a1 {
                    let aj = (ai as f64 + 0.5 - v) / s;
                    if af * af + ah * ah + aj * aj >= 1.0 || !(MIN_Y..MIN_Y + HEIGHT).contains(&ag) {
                        continue;
                    }
                    let ak = (ae - j + (ag - k) * m + (ai - l) * m * n) as usize;
                    if ak >= bits.len() {
                        bits.resize(ak + 1, false);
                    }
                    if bits[ak] {
                        continue;
                    }
                    bits[ak] = true;
                    let pos = [ae, ag, ai];
                    if !lv.can_write(pos) {
                        continue;
                    }
                    let st = lv.get(pos);
                    for tg in targets {
                        if can_place_ore(lv, st, r, discard, tg, pos) {
                            // section write: no heightmap update (ores replace solid blocks)
                            lv.set_raw(pos, tg.state);
                            placed += 1;
                            break;
                        }
                    }
                }
            }
        }
    }
    placed > 0
}
