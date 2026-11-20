//! Vanilla structures: structure sets and their placement, structure starts
//! (`ChunkGenerator.createStructures`), references, the `Beardifier` that
//! fits terrain to structures, and piece placement during decoration
//! (`StructureStart.placeInChunk`). Ported from the 26.1 bytecode.
//!
//! Ported structure types: jigsaw (villages, pillager outposts, ancient
//! cities, trail ruins, trial chambers). The others never start yet.

pub mod jigsaw;
pub mod placement;
pub mod template;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use parking_lot::Mutex;
use serde_json::Value;

use super::chunk::{HEIGHT, MIN_Y};
use super::feature::{Builder, Hm, Region};
use super::noise::clamped_map;
use super::random::{Rng, WgRandom};
use jigsaw::{ElemKind, Junction, Pools, Projection};
use placement::Placement;
use template::{bb_center, bb_encapsulate, bb_intersects, bb_intersects_xz, Proc, Settings};

pub type P = [i32; 3];
/// inclusive (min, max) block box
pub type Bb = (P, P);

/// What structure generation needs from the chunk generator.
pub trait Ctx: Sync {
    /// `ChunkGenerator.getBaseHeight` (= `getFirstFreeHeight`)
    fn base_height(&self, x: i32, z: i32, hm: Hm) -> i32;
    /// `BiomeSource.getNoiseBiome` (network id)
    fn noise_biome(&self, qx: i32, qy: i32, qz: i32) -> u8;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Adapt {
    None,
    Bury,
    BeardThin,
    BeardBox,
    Encapsulate,
}

enum Kind {
    Jigsaw(jigsaw::JigsawStructure),
    /// structure type not ported yet: never starts
    Unported,
}

struct Structure {
    name: String,
    kind: Kind,
    /// biome network id -> allowed
    biomes: Vec<bool>,
    step: usize,
    adapt: Adapt,
}

struct Set {
    placement: Placement,
    /// (structure index, weight)
    structures: Vec<(usize, i32)>,
}

pub struct Piece {
    pub elem: Arc<jigsaw::Elem>,
    pub pos: P,
    pub rot: i32,
    pub bb: Bb,
    pub ground_delta: i32,
    pub junctions: Vec<Junction>,
    /// `LiquidSettings.APPLY_WATERLOGGING`
    pub waterlog: bool,
}

pub struct Start {
    structure: usize,
    pieces: Vec<Piece>,
    /// `getBoundingBox` (adjusted for terrain adaptation)
    bb: Bb,
}

pub struct Structures {
    seed: i64,
    structures: Vec<Structure>,
    sets: Vec<Set>,
    set_by_name: HashMap<String, usize>,
    pools: Pools,
    /// per generation step: structure indices in registry order
    by_step: Vec<Vec<usize>>,
    starts: Mutex<HashMap<(i32, i32), Arc<Vec<Arc<Start>>>>>,
    heights: Mutex<HashMap<(i32, i32, u8), i32>>,
}

const STEPS: [&str; 11] = [
    "raw_generation",
    "lakes",
    "local_modifications",
    "underground_structures",
    "surface_structures",
    "strongholds",
    "underground_ores",
    "underground_decoration",
    "fluid_springs",
    "vegetal_decoration",
    "top_layer_modification",
];

fn biome_tag(b: &Builder, tag: &str, out: &mut HashSet<String>, seen: &mut HashSet<String>) -> Result<()> {
    let tag = tag.trim_start_matches('#').trim_start_matches("minecraft:");
    if !seen.insert(tag.to_string()) {
        return Ok(());
    }
    let v = b.load(&format!("tags/worldgen/biome/{tag}"))?;
    for e in v["values"].as_array().into_iter().flatten() {
        let id = match e {
            Value::String(s) => s.as_str(),
            o => o["id"].as_str().unwrap_or(""),
        };
        if let Some(t) = id.strip_prefix('#') {
            biome_tag(b, t, out, seen)?;
        } else {
            out.insert(if id.contains(':') { id.to_string() } else { format!("minecraft:{id}") });
        }
    }
    Ok(())
}

impl Structures {
    /// `structure_names` / `set_names`: every structure / structure set of
    /// the data pack (registry order is sorted by id); `possible`: biome ids
    /// the overworld biome source can return.
    pub fn new(b: &mut Builder, seed: i64, structure_names: &[String], set_names: &[String], possible: &[u8]) -> Result<(Structures, Vec<String>)> {
        let names = crate::world::chunk::Biomes::get();
        let mut pools = Pools::default();
        let mut structures = Vec::new();
        let mut unported = Vec::new();
        let mut by_step = vec![Vec::new(); STEPS.len()];
        let mut index = BTreeMap::new();
        for name in structure_names {
            let v = b.load(&format!("worldgen/structure/{name}"))?;
            let mut biomes = vec![false; 256];
            let mut set = HashSet::new();
            match &v["biomes"] {
                Value::String(s) if s.starts_with('#') => biome_tag(b, s, &mut set, &mut HashSet::new())?,
                Value::String(s) => {
                    set.insert(s.clone());
                }
                Value::Array(a) => set.extend(a.iter().filter_map(|x| x.as_str()).map(String::from)),
                _ => {}
            }
            for n in &set {
                if let Some(id) = names.id(n) {
                    biomes[id as usize] = true;
                }
            }
            let step = STEPS.iter().position(|s| Some(*s) == v["step"].as_str()).unwrap_or(4);
            let adapt = match v["terrain_adaptation"].as_str().unwrap_or("none") {
                "bury" => Adapt::Bury,
                "beard_thin" => Adapt::BeardThin,
                "beard_box" => Adapt::BeardBox,
                "encapsulate" => Adapt::Encapsulate,
                _ => Adapt::None,
            };
            let overworld = possible.iter().any(|&p| biomes[p as usize]);
            let kind = match v["type"].as_str().unwrap_or("") {
                "minecraft:jigsaw" if overworld => Kind::Jigsaw(jigsaw::JigsawStructure::parse(&mut pools, b, &v)?),
                t => {
                    if overworld {
                        unported.push(format!("{name} ({})", t.trim_start_matches("minecraft:")));
                    }
                    Kind::Unported
                }
            };
            by_step[step].push(structures.len());
            index.insert(name.clone(), structures.len());
            structures.push(Structure { name: name.clone(), kind, biomes, step, adapt });
        }
        let mut sets = Vec::new();
        let mut set_by_name = HashMap::new();
        for name in set_names {
            let v = b.load(&format!("worldgen/structure_set/{name}"))?;
            let placement = Placement::parse(&v["placement"])?;
            let list: Vec<(usize, i32)> = v["structures"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| Some((*index.get(e["structure"].as_str()?.trim_start_matches("minecraft:"))?, e["weight"].as_i64().unwrap_or(1) as i32)))
                .collect();
            set_by_name.insert(name.clone(), sets.len());
            sets.push(Set { placement, structures: list });
        }
        let s = Structures { seed, structures, sets, set_by_name, pools, by_step, starts: Mutex::new(HashMap::new()), heights: Mutex::new(HashMap::new()) };
        Ok((s, unported))
    }

    fn base_height(&self, ctx: &dyn Ctx, x: i32, z: i32, hm: Hm) -> i32 {
        let k = (x, z, hm as u8);
        if let Some(&h) = self.heights.lock().get(&k) {
            return h;
        }
        let h = ctx.base_height(x, z, hm);
        let mut c = self.heights.lock();
        if c.len() > 1 << 20 {
            c.clear();
        }
        c.insert(k, h);
        h
    }

    fn other_placement(&self, name: &str) -> Option<Placement> {
        self.set_by_name.get(name).map(|&i| self.sets[i].placement.clone())
    }

    /// Whether any structure of a set can be in this world at all.
    fn set_possible(&self, s: &Set) -> bool {
        s.structures.iter().any(|&(i, _)| matches!(self.structures[i].kind, Kind::Jigsaw(_)))
    }

    /// Structure starts in chunk (cx, cz) (`createStructures`).
    fn starts(&self, ctx: &dyn Ctx, cx: i32, cz: i32) -> Arc<Vec<Arc<Start>>> {
        if let Some(s) = self.starts.lock().get(&(cx, cz)) {
            return s.clone();
        }
        let mut out = Vec::new();
        for set in &self.sets {
            if !self.set_possible(set) || !set.placement.is_structure_chunk(self.seed, cx, cz, &|n| self.other_placement(n)) {
                continue;
            }
            if set.structures.len() == 1 {
                if let Some(s) = self.try_start(ctx, set.structures[0].0, cx, cz) {
                    out.push(Arc::new(s));
                }
                continue;
            }
            let mut list = set.structures.clone();
            let mut r = placement::large_feature_seed(self.seed, cx, cz);
            let mut total: i32 = list.iter().map(|e| e.1).sum();
            while !list.is_empty() {
                let mut j = r.next_int_bound(total.max(1));
                let mut k = 0;
                for (n, e) in list.iter().enumerate() {
                    k = n;
                    j -= e.1;
                    if j < 0 {
                        break;
                    }
                }
                let e = list[k];
                if let Some(s) = self.try_start(ctx, e.0, cx, cz) {
                    out.push(Arc::new(s));
                    break;
                }
                list.remove(k);
                total -= e.1;
            }
        }
        let out = Arc::new(out);
        self.starts.lock().insert((cx, cz), out.clone());
        out
    }

    /// `ChunkGenerator.tryGenerateStructure` / `Structure.generate`
    fn try_start(&self, ctx: &dyn Ctx, si: usize, cx: i32, cz: i32) -> Option<Start> {
        let s = &self.structures[si];
        let Kind::Jigsaw(j) = &s.kind else { return None };
        let mut r = placement::large_feature_seed(self.seed, cx, cz);
        let hctx = HeightCtx { s: self, ctx };
        let valid = |p: P| s.biomes[ctx.noise_biome(p[0] >> 2, p[1] >> 2, p[2] >> 2) as usize];
        let pieces = jigsaw::generate(j, &self.pools, &hctx, self.seed, cx, cz, &mut r, &valid)?;
        if pieces.is_empty() {
            return None;
        }
        let mut bb = pieces[0].bb;
        for p in &pieces[1..] {
            bb = bb_encapsulate(bb, &p.bb);
        }
        if s.adapt != Adapt::None {
            bb = template::bb_inflate(bb, 12);
        }
        Some(Start { structure: si, pieces, bb })
    }

    /// Starts whose box reaches chunk (cx, cz) (`createReferences`), in
    /// reference order.
    fn references(&self, ctx: &dyn Ctx, cx: i32, cz: i32) -> Vec<Arc<Start>> {
        let (x0, z0) = (cx * 16, cz * 16);
        let mut out = Vec::new();
        for x in cx - 8..=cx + 8 {
            for z in cz - 8..=cz + 8 {
                for s in self.starts(ctx, x, z).iter() {
                    if bb_intersects_xz(&s.bb, x0, z0, x0 + 15, z0 + 15) {
                        out.push(s.clone());
                    }
                }
            }
        }
        out
    }

    /// `Beardifier.forStructuresInChunk`
    pub fn beardifier(&self, ctx: &dyn Ctx, cx: i32, cz: i32) -> Option<Beardifier> {
        let (x0, z0) = (cx * 16, cz * 16);
        let mut rigids = Vec::new();
        let mut junctions = Vec::new();
        let mut affected: Option<Bb> = None;
        let mut include = |b: Bb| affected = Some(match affected {
            Some(a) => bb_encapsulate(a, &b),
            None => b,
        });
        for start in self.references(ctx, cx, cz) {
            let adapt = self.structures[start.structure].adapt;
            if adapt == Adapt::None {
                continue;
            }
            for p in &start.pieces {
                if !bb_intersects_xz(&p.bb, x0 - 12, z0 - 12, x0 + 15 + 12, z0 + 15 + 12) {
                    continue;
                }
                if p.elem.projection == Projection::Rigid {
                    rigids.push((p.bb, adapt, p.ground_delta));
                    include(p.bb);
                }
                for j in &p.junctions {
                    if j.x > x0 - 12 && j.z > z0 - 12 && j.x < x0 + 15 + 12 && j.z < z0 + 15 + 12 {
                        junctions.push(*j);
                        include(([j.x, j.ground_y, j.z], [j.x, j.ground_y, j.z]));
                    }
                }
            }
        }
        let affected = template::bb_inflate(affected?, 24);
        Some(Beardifier { rigids, junctions, affected })
    }

    /// Places the pieces of every structure of generation step `step` that
    /// reach the region's centre chunk (the structure part of
    /// `applyBiomeDecoration`).
    pub fn place_step(&self, ctx: &dyn Ctx, lv: &mut Region, r: &mut WgRandom, dec: i64, step: usize) {
        let Some(list) = self.by_step.get(step) else { return };
        if list.is_empty() {
            return;
        }
        let refs = self.references(ctx, lv.cx, lv.cz);
        let (x0, z0) = (lv.cx * 16, lv.cz * 16);
        let area: Bb = ([x0, MIN_Y + 1, z0], [x0 + 15, MIN_Y + HEIGHT - 1, z0 + 15]);
        for (m, &si) in list.iter().enumerate() {
            r.set_feature_seed(dec, m as i32, step as i32);
            for start in refs.iter().filter(|s| s.structure == si) {
                let Some(first) = start.pieces.first() else { continue };
                let c = bb_center(&first.bb);
                let reference = [c[0], first.bb.0[1], c[2]];
                for p in &start.pieces {
                    if bb_intersects(&p.bb, &area) {
                        place_elem(lv, self.seed, &p.elem, p.pos, p.rot, &area, reference, p.waterlog, r);
                    }
                }
            }
        }
    }

    pub fn pieces_info(&self, ctx: &dyn Ctx, cx: i32, cz: i32) -> Vec<(String, Vec<Bb>, Vec<String>)> {
        self.starts(ctx, cx, cz)
            .iter()
            .map(|s| {
                let names = s.pieces.iter().map(|p| p.elem.describe()).collect();
                (self.structures[s.structure].name.clone(), s.pieces.iter().map(|p| p.bb).collect(), names)
            })
            .collect()
    }

    /// (structure name, piece count, box) of the starts in a chunk.
    pub fn starts_info(&self, ctx: &dyn Ctx, cx: i32, cz: i32) -> Vec<(String, usize, Bb)> {
        self.starts(ctx, cx, cz).iter().map(|s| (self.structures[s.structure].name.clone(), s.pieces.len(), s.bb)).collect()
    }
}

struct HeightCtx<'a> {
    s: &'a Structures,
    ctx: &'a dyn Ctx,
}

impl Ctx for HeightCtx<'_> {
    fn base_height(&self, x: i32, z: i32, hm: Hm) -> i32 {
        self.s.base_height(self.ctx, x, z, hm)
    }
    fn noise_biome(&self, qx: i32, qy: i32, qz: i32) -> u8 {
        self.ctx.noise_biome(qx, qy, qz)
    }
}

/// `StructurePoolElement.place`
#[allow(clippy::too_many_arguments)]
fn place_elem(lv: &mut Region, seed: i64, e: &jigsaw::Elem, pos: P, rot: i32, area: &Bb, reference: P, waterlog: bool, r: &mut WgRandom) -> bool {
    match &e.kind {
        ElemKind::Empty => true,
        ElemKind::Feature(f) => f.place(lv, r, pos),
        ElemKind::List(l) => l.iter().all(|x| place_elem(lv, seed, x, pos, rot, area, reference, waterlog, r)),
        ElemKind::Single { template, processors, legacy, waterlog: over } => {
            let mut procs = vec![Proc::Ignore(if *legacy { vec!["structure_block", "air"] } else { vec!["structure_block"] }), Proc::JigsawReplace];
            procs.extend(processors.iter().cloned());
            if e.projection == Projection::TerrainMatching {
                procs.push(Proc::Gravity { hm: Hm::NotAir, offset: -1 });
            }
            let settings = Settings { rot, mirror: 0, pivot: [0, 0, 0], bb: Some(*area), processors: procs, waterlog: over.unwrap_or(waterlog), known_shape: true, random_seed: None };
            template::place_in_world(lv, seed, template, pos, reference, &settings, r)
        }
    }
}

/// `Beardifier`: density added to every block of the noise fill.
pub struct Beardifier {
    rigids: Vec<(Bb, Adapt, i32)>,
    junctions: Vec<Junction>,
    affected: Bb,
}

static BEARD_KERNEL: std::sync::LazyLock<Vec<f32>> = std::sync::LazyLock::new(|| {
    let mut k = vec![0f32; 24 * 24 * 24];
    for i in 0..24 {
        for j in 0..24 {
            for l in 0..24 {
                let (x, y, z) = ((j - 12) as f64, (l - 12) as f64 + 0.5, (i - 12) as f64);
                let d = x * x + y * y + z * z;
                k[(i * 24 * 24 + j * 24 + l) as usize] = std::f64::consts::E.powf(-d / 16.0) as f32;
            }
        }
    }
    k
});

/// `Mth.fastInvSqrt`
fn fast_inv_sqrt(x: f64) -> f64 {
    let h = 0.5 * x;
    let i = 6910469410427058090i64.wrapping_sub((x.to_bits() as i64) >> 1);
    let y = f64::from_bits(i as u64);
    y * (1.5 - h * y * y)
}

fn bury(x: f64, y: f64, z: f64) -> f64 {
    let d = (x * x + y * y + z * z).sqrt();
    clamped_map(d, 0.0, 6.0, 1.0, 0.0)
}

fn beard(x: i32, y: i32, z: i32, y_rel: i32) -> f64 {
    let (i, j, k) = (x + 12, y + 12, z + 12);
    if !(0..24).contains(&i) || !(0..24).contains(&j) || !(0..24).contains(&k) {
        return 0.0;
    }
    let d = y_rel as f64 + 0.5;
    let e = (x as f64) * (x as f64) + d * d + (z as f64) * (z as f64);
    let f = -d * fast_inv_sqrt(e / 2.0) / 2.0;
    f * BEARD_KERNEL[(k * 24 * 24 + i * 24 + j) as usize] as f64
}

impl Beardifier {
    pub fn compute(&self, x: i32, y: i32, z: i32) -> f64 {
        if !template::bb_inside(&self.affected, [x, y, z]) {
            return 0.0;
        }
        let mut d = 0.0;
        for &(b, adapt, gd) in &self.rigids {
            let i = 0.max((b.0[0] - x).max(x - b.1[0]));
            let j = 0.max((b.0[2] - z).max(z - b.1[2]));
            let base = b.0[1] + gd;
            let rel = y - base;
            let k = match adapt {
                Adapt::None => 0,
                Adapt::Bury | Adapt::BeardThin => rel,
                Adapt::BeardBox => 0.max((base - y).max(y - b.1[1])),
                Adapt::Encapsulate => 0.max((b.0[1] - y).max(y - b.1[1])),
            };
            d += match adapt {
                Adapt::None => 0.0,
                Adapt::Bury => bury(i as f64, k as f64 / 2.0, j as f64),
                Adapt::BeardThin | Adapt::BeardBox => beard(i, k, j, rel) * 0.8,
                Adapt::Encapsulate => bury(i as f64 / 2.0, k as f64 / 2.0, j as f64 / 2.0) * 0.8,
            };
        }
        for jn in &self.junctions {
            let (dx, dy, dz) = (x - jn.x, y - jn.ground_y, z - jn.z);
            d += beard(dx, dy, dz, dy) * 0.4;
        }
        d
    }
}
