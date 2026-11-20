//! `FossilFeature` with the structure template placement it needs
//! (`StructureTemplate.placeInWorld`, rotation only, block processors
//! `block_rot`, `rule` and `protected_blocks`), ported from the 26.1 bytecode.

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use super::extra::{add, P};
use super::place::BlockSet;
use super::{Builder, Hm, Region};
use crate::registry::{vstate, BlockRegistry, BlockState};
use crate::worldgen::chunk::{HEIGHT, MIN_Y};
use crate::worldgen::random::{Rng, WgRandom};

pub struct Template {
    size: P,
    /// palette blocks in `buildInfoList` order (template coordinates)
    blocks: Vec<(P, BlockState)>,
}

impl Template {
    fn load(b: &mut Builder, name: &str) -> Result<Template> {
        let key = name.trim_start_matches("minecraft:");
        let v = (b.load)(&format!("structure/{key}"))?;
        let ints = |v: &Value| -> Result<P> {
            let a = v.as_array().ok_or_else(|| anyhow!("bad vector"))?;
            if a.len() != 3 {
                bail!("bad vector");
            }
            Ok([a[0].as_i64().unwrap_or(0) as i32, a[1].as_i64().unwrap_or(0) as i32, a[2].as_i64().unwrap_or(0) as i32])
        };
        let size = ints(&v["size"])?;
        let pal = if v["palette"].is_array() { &v["palette"] } else { &v["palettes"][0] };
        let reg = BlockRegistry::get();
        let mut states = Vec::new();
        for e in pal.as_array().into_iter().flatten() {
            let n = e["Name"].as_str().unwrap_or("").trim_start_matches("minecraft:");
            let props: Vec<(String, String)> = e["Properties"].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect();
            let pr: Vec<(&str, &str)> = props.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            states.push(reg.state(n, &pr).ok_or_else(|| anyhow!("unknown template block {n}"))?);
        }
        let (mut full, mut other, mut with_nbt) = (Vec::new(), Vec::new(), Vec::new());
        for e in v["blocks"].as_array().into_iter().flatten() {
            let p = ints(&e["pos"])?;
            let s = *states.get(e["state"].as_u64().unwrap_or(0) as usize).ok_or_else(|| anyhow!("bad palette index"))?;
            if e.get("nbt").is_some() {
                with_nbt.push((p, s));
            } else if vstate::full_block(s) {
                full.push((p, s));
            } else {
                other.push((p, s));
            }
        }
        let key = |b: &(P, BlockState)| (b.0[1], b.0[0], b.0[2]);
        full.sort_by_key(key);
        other.sort_by_key(key);
        with_nbt.sort_by_key(key);
        full.extend(other);
        full.extend(with_nbt);
        Ok(Template { size, blocks: full })
    }

    /// `getSize(rotation)`
    fn rotated_size(&self, rot: i32) -> P {
        if rot == 1 || rot == 3 {
            [self.size[2], self.size[1], self.size[0]]
        } else {
            self.size
        }
    }
}

/// `Rotation` ordinals: NONE, CLOCKWISE_90, CLOCKWISE_180, COUNTERCLOCKWISE_90.
fn transform(p: P, rot: i32) -> P {
    let [x, y, z] = p;
    match rot {
        1 => [-z, y, x],
        2 => [-x, y, -z],
        3 => [z, y, -x],
        _ => p,
    }
}

/// `BlockState.rotate`
fn rotate_state(s: BlockState, rot: i32) -> BlockState {
    if rot == 0 {
        return s;
    }
    let reg = BlockRegistry::get();
    if rot != 2 {
        if let Some(a) = reg.property(s, "axis") {
            let na = match a {
                "x" => "z",
                "z" => "x",
                o => o,
            };
            return reg.with(s, "axis", na).unwrap_or(s);
        }
    }
    if let Some(f) = reg.property(s, "facing") {
        const H: [&str; 4] = ["north", "east", "south", "west"];
        if let Some(i) = H.iter().position(|h| *h == f) {
            return reg.with(s, "facing", H[(i + rot as usize) % 4]).unwrap_or(s);
        }
    }
    s
}

enum Proc {
    Rot { integrity: f32, rottable: Option<BlockSet> },
    Rule(Vec<(String, BlockState)>),
    Protected(BlockSet),
}

fn processors(b: &mut Builder, name: &str) -> Result<Vec<Proc>> {
    let key = name.trim_start_matches("minecraft:");
    let v = (b.load)(&format!("worldgen/processor_list/{key}"))?;
    let mut out = Vec::new();
    for p in v["processors"].as_array().into_iter().flatten() {
        match p["processor_type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
            "block_rot" => out.push(Proc::Rot {
                integrity: p["integrity"].as_f64().unwrap_or(1.0) as f32,
                rottable: match p.get("rottable_blocks") {
                    Some(t) => Some(super::place::block_set(b, t)?),
                    None => None,
                },
            }),
            "rule" => {
                let mut rules = Vec::new();
                for r in p["rules"].as_array().into_iter().flatten() {
                    let inp = &r["input_predicate"];
                    let loc = &r["location_predicate"];
                    if inp["predicate_type"].as_str() != Some("minecraft:block_match") || loc["predicate_type"].as_str() != Some("minecraft:always_true") || r.get("position_predicate").is_some() {
                        bail!("unsupported rule processor");
                    }
                    rules.push((inp["block"].as_str().unwrap_or("").trim_start_matches("minecraft:").to_string(), super::place::parse_state(&r["output_state"])?));
                }
                out.push(Proc::Rule(rules));
            }
            "protected_blocks" => out.push(Proc::Protected(b.tag(p["value"].as_str().unwrap_or(""))?)),
            t => bail!("structure processor {t}"),
        }
    }
    Ok(out)
}

pub struct Fossil {
    fossils: Vec<Template>,
    overlays: Vec<Template>,
    fossil_procs: Vec<Proc>,
    overlay_procs: Vec<Proc>,
    max_empty_corners: i32,
}

/// `BoundingBox` as (min, max), inclusive.
type Bb = (P, P);

fn inside(bb: &Bb, p: P) -> bool {
    (0..3).all(|i| p[i] >= bb.0[i] && p[i] <= bb.1[i])
}

impl Fossil {
    pub fn parse(b: &mut Builder, c: &Value) -> Result<Fossil> {
        let list = |b: &mut Builder, v: &Value| -> Result<Vec<Template>> { v.as_array().into_iter().flatten().map(|n| Template::load(b, n.as_str().unwrap_or(""))).collect() };
        let fossils = list(b, &c["fossil_structures"])?;
        let overlays = list(b, &c["overlay_structures"])?;
        if fossils.is_empty() || fossils.len() != overlays.len() {
            bail!("fossil structure lists differ");
        }
        Ok(Fossil {
            fossils,
            overlays,
            fossil_procs: processors(b, c["fossil_processors"].as_str().unwrap_or(""))?,
            overlay_procs: processors(b, c["overlay_processors"].as_str().unwrap_or(""))?,
            max_empty_corners: c["max_empty_corners_allowed"].as_i64().unwrap_or(0) as i32,
        })
    }

    pub fn place(&self, lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
        let rot = r.next_int_bound(4);
        let i = r.next_int_bound(self.fossils.len() as i32) as usize;
        let t = &self.fossils[i];
        let cx = o[0] & !15;
        let cz = o[2] & !15;
        let clip: Bb = ([cx - 16, MIN_Y, cz - 16], [cx + 15 + 16, MIN_Y + HEIGHT - 1, cz + 15 + 16]);
        let size = t.rotated_size(rot);
        let p = add(o, [-size[0] / 2, 0, -size[2] / 2]);
        let mut y = o[1];
        for x in 0..size[0] {
            for z in 0..size[2] {
                y = y.min(lv.height(Hm::Motion, p[0] + x, p[2] + z));
            }
        }
        let y = (y - 15 - r.next_int_bound(10)).max(MIN_Y + 10);
        let (sx, sz) = (t.size[0] - 1, t.size[2] - 1);
        let off = match rot {
            1 => [sz, 0, 0],
            2 => [sx, 0, sz],
            3 => [0, 0, sx],
            _ => [0, 0, 0],
        };
        let zero = add([p[0], y, p[2]], off);
        let a = transform([0, 0, 0], rot);
        let b = transform([t.size[0] - 1, t.size[1] - 1, t.size[2] - 1], rot);
        let bb: Bb = (add(zero, [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])]), add(zero, [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])]));
        let mut empty = 0;
        for cx in [bb.0[0], bb.1[0]] {
            for cy in [bb.0[1], bb.1[1]] {
                for cz in [bb.0[2], bb.1[2]] {
                    let s = lv.get([cx, cy, cz]);
                    if vstate::air(s) || matches!(s.name(), "lava" | "water") {
                        empty += 1;
                    }
                }
            }
        }
        if empty > self.max_empty_corners {
            return false;
        }
        place_template(lv, r, t, zero, rot, &clip, &self.fossil_procs);
        place_template(lv, r, &self.overlays[i], zero, rot, &clip, &self.overlay_procs);
        true
    }
}

fn place_template(lv: &mut Region, r: &mut WgRandom, t: &Template, zero: P, rot: i32, clip: &Bb, procs: &[Proc]) {
    // getRandomPalette (one palette)
    r.next_int_bound(1);
    let mut out = Vec::with_capacity(t.blocks.len());
    'blocks: for &(rel, s0) in &t.blocks {
        let wp = add(transform(rel, rot), zero);
        let mut s = s0;
        for p in procs {
            match p {
                Proc::Rot { integrity, rottable } => {
                    if rottable.as_ref().is_none_or(|set| set.contains(s0)) && r.next_float() > *integrity {
                        continue 'blocks;
                    }
                }
                Proc::Rule(rules) => {
                    if let Some((_, o)) = rules.iter().find(|(n, _)| s.name() == n) {
                        s = *o;
                    }
                }
                Proc::Protected(set) => {
                    if set.contains(lv.get(wp)) {
                        continue 'blocks;
                    }
                }
            }
        }
        out.push((wp, s));
    }
    for (wp, s) in out {
        if inside(clip, wp) {
            lv.set(wp, rotate_state(s, rot));
        }
    }
}
