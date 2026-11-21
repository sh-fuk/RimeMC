//! Jigsaw structures: template pools, pool elements, pool aliases and the
//! piece assembly of `JigsawPlacement.addPieces` / `JigsawPlacement.Placer`,
//! ported from the 26.1 bytecode.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::template::{self, add, bb_inside, dir_opposite, dir_step, jigsaw_front_top, sub, Jigsaw, Proc, Template};
use super::{Bb, Ctx, Piece, P};
use crate::worldgen::feature::place::HeightP;
use crate::worldgen::feature::{Builder, Hm, Placed};
use crate::worldgen::random::{Legacy, Rng};

// ------------------------------------------------------------ elements

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Projection {
    Rigid,
    TerrainMatching,
}

pub enum ElemKind {
    Empty,
    /// single / legacy single pool element
    Single { template: Arc<Template>, processors: Vec<Proc>, legacy: bool, waterlog: Option<bool> },
    Feature(Arc<Placed>),
    List(Vec<Arc<Elem>>),
}

pub struct Elem {
    pub kind: ElemKind,
    pub projection: Projection,
}

impl Elem {
    /// short description (template name) for debugging
    pub fn describe(&self) -> String {
        match &self.kind {
            ElemKind::Empty => "empty".into(),
            ElemKind::Single { template, .. } => template.name.clone(),
            ElemKind::Feature(f) => format!("feature {}", f.name.clone().unwrap_or_default()),
            ElemKind::List(l) => format!("list[{}]", l.iter().map(|e| e.describe()).collect::<Vec<_>>().join(",")),
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self.kind, ElemKind::Empty)
    }

    /// `getSize(rotation)`
    fn size(&self, rot: i32) -> P {
        match &self.kind {
            ElemKind::Single { template, .. } => template.rotated_size(rot),
            ElemKind::List(l) => {
                let mut s = [0, 0, 0];
                for e in l {
                    let t = e.size(rot);
                    s = [s[0].max(t[0]), s[1].max(t[1]), s[2].max(t[2])];
                }
                s
            }
            _ => [0, 0, 0],
        }
    }

    /// `getBoundingBox(manager, pos, rotation)`
    pub fn bounding_box(&self, pos: P, rot: i32) -> Bb {
        match &self.kind {
            ElemKind::Single { template, .. } => template.bounding_box(pos, rot, [0, 0, 0], 0),
            ElemKind::List(l) => {
                let mut it = l.iter().filter(|e| !e.is_empty()).map(|e| e.bounding_box(pos, rot));
                let first = it.next().unwrap_or((pos, pos));
                it.fold(first, |a, b| template::bb_encapsulate(a, &b))
            }
            _ => {
                let s = self.size(rot);
                (pos, add(pos, s))
            }
        }
    }

    /// `getShuffledJigsawBlocks(manager, pos, rotation, random)`
    pub fn shuffled_jigsaws(&self, pos: P, rot: i32, r: &mut dyn Rng) -> Vec<Jigsaw> {
        match &self.kind {
            ElemKind::Single { template, .. } => {
                let mut l = template.jigsaws(pos, rot);
                shuffle(&mut l, r);
                // sortBySelectionPriority: stable, highest first
                l.sort_by(|a, b| b.selection_priority.cmp(&a.selection_priority));
                l
            }
            ElemKind::List(l) => match l.first() {
                Some(e) => e.shuffled_jigsaws(pos, rot, r),
                None => Vec::new(),
            },
            ElemKind::Feature(_) => vec![Jigsaw {
                pos,
                state: crate::registry::BlockRegistry::get().state("jigsaw", &[("orientation", "down_south")]).expect("jigsaw"),
                name: "bottom".into(),
                pool: "empty".into(),
                target: "empty".into(),
                rollable: true,
                placement_priority: 0,
                selection_priority: 0,
            }],
            ElemKind::Empty => Vec::new(),
        }
    }
}

/// `Util.shuffle`
pub fn shuffle<T>(l: &mut [T], r: &mut dyn Rng) {
    let mut i = l.len();
    while i > 1 {
        let j = r.next_int_bound(i as i32) as usize;
        l.swap(i - 1, j);
        i -= 1;
    }
}

pub struct Pool {
    /// elements repeated by weight (`templates`)
    pub templates: Vec<Arc<Elem>>,
    pub fallback: String,
    max_size: std::sync::OnceLock<i32>,
}

impl Pool {
    /// `getMaxSize`: tallest element (y span), ignoring empty elements
    fn max_size(&self) -> i32 {
        *self.max_size.get_or_init(|| {
            self.templates
                .iter()
                .filter(|e| !e.is_empty())
                .map(|e| {
                    let b = e.bounding_box([0, 0, 0], 0);
                    b.1[1] - b.0[1] + 1
                })
                .max()
                .unwrap_or(0)
        })
    }
}

// ------------------------------------------------------------ library

/// All template pools, elements and templates the overworld structures use.
#[derive(Default)]
pub struct Pools {
    pub pools: HashMap<String, Arc<Pool>>,
    templates: HashMap<String, Arc<Template>>,
    processor_lists: HashMap<String, Vec<Proc>>,
}

fn key(s: &str) -> String {
    s.trim_start_matches('#').trim_start_matches("minecraft:").to_string()
}

impl Pools {
    /// Loads `pool` and every pool reachable from its elements' jigsaws.
    pub fn load_pool(&mut self, b: &mut Builder, pool: &str) -> Result<()> {
        let mut todo = vec![key(pool)];
        while let Some(name) = todo.pop() {
            if self.pools.contains_key(&name) {
                continue;
            }
            if name == "empty" {
                self.pools.insert(name, Arc::new(Pool { templates: Vec::new(), fallback: "empty".into(), max_size: Default::default() }));
                continue;
            }
            // jigsaws may name pools that only exist as aliases: vanilla skips
            // a pool it cannot find ("Empty or non-existent pool")
            let Ok(v) = b.load(&format!("worldgen/template_pool/{name}")) else { continue };
            let mut templates = Vec::new();
            for e in v["elements"].as_array().into_iter().flatten() {
                let el = Arc::new(self.element(b, &e["element"])?);
                let w = e["weight"].as_i64().unwrap_or(1).max(0);
                for j in self.element_jigsaw_pools(&el) {
                    todo.push(j);
                }
                for _ in 0..w {
                    templates.push(el.clone());
                }
            }
            let fallback = key(v["fallback"].as_str().unwrap_or("minecraft:empty"));
            todo.push(fallback.clone());
            self.pools.insert(name, Arc::new(Pool { templates, fallback, max_size: Default::default() }));
        }
        Ok(())
    }

    fn element_jigsaw_pools(&self, e: &Elem) -> Vec<String> {
        match &e.kind {
            ElemKind::Single { template, .. } => template.jigsaws([0, 0, 0], 0).into_iter().map(|j| j.pool).collect(),
            ElemKind::List(l) => l.iter().flat_map(|x| self.element_jigsaw_pools(x)).collect(),
            _ => Vec::new(),
        }
    }

    pub fn template(&mut self, b: &mut Builder, name: &str) -> Result<Arc<Template>> {
        let k = key(name);
        if let Some(t) = self.templates.get(&k) {
            return Ok(t.clone());
        }
        // a missing template is an empty one (`StructureTemplateManager.getOrCreate`)
        let t = Arc::new(Template::load(&|rel| b.load(rel), &k).unwrap_or_else(|e| {
            tracing::debug!("structure template {k}: {e:#}");
            Template { name: k.clone(), size: [0, 0, 0], blocks: Vec::new(), alt: Vec::new(), jigsaws: Vec::new(), data_markers: Vec::new() }
        }));
        self.templates.insert(k, t.clone());
        Ok(t)
    }

    fn processors(&mut self, b: &mut Builder, v: &Value) -> Result<Vec<Proc>> {
        if let Some(name) = v.as_str() {
            let k = key(name);
            if let Some(p) = self.processor_lists.get(&k) {
                return Ok(p.clone());
            }
            let j = b.load(&format!("worldgen/processor_list/{k}"))?;
            let p = self.processors(b, &j)?;
            self.processor_lists.insert(k, p.clone());
            return Ok(p);
        }
        let list = if v.is_array() { v } else { &v["processors"] };
        list.as_array().into_iter().flatten().map(|p| Proc::parse(b, p)).collect()
    }

    fn element(&mut self, b: &mut Builder, v: &Value) -> Result<Elem> {
        let projection = match v["projection"].as_str() {
            Some("terrain_matching") => Projection::TerrainMatching,
            _ => Projection::Rigid,
        };
        let ty = v["element_type"].as_str().unwrap_or("").trim_start_matches("minecraft:");
        let kind = match ty {
            "empty_pool_element" => ElemKind::Empty,
            "single_pool_element" | "legacy_single_pool_element" => {
                let template = self.template(b, v["location"].as_str().unwrap_or(""))?;
                let processors = match v.get("processors") {
                    Some(p) => self.processors(b, p)?,
                    None => Vec::new(),
                };
                let waterlog = v["override_liquid_settings"].as_str().map(|s| s != "ignore_waterlogging");
                ElemKind::Single { template, processors, legacy: ty.starts_with("legacy"), waterlog }
            }
            "feature_pool_element" => ElemKind::Feature(b.placed(&v["feature"])?),
            "list_pool_element" => {
                let mut l = Vec::new();
                for e in v["elements"].as_array().into_iter().flatten() {
                    // ListPoolElement sets its projection on every element
                    let mut el = self.element(b, e)?;
                    el.projection = projection;
                    l.push(Arc::new(el));
                }
                ElemKind::List(l)
            }
            t => return Err(anyhow!("pool element type {t}")),
        };
        Ok(Elem { kind, projection })
    }
}

// ------------------------------------------------------------ aliases

pub enum Alias {
    Direct(String, String),
    Random(String, Vec<(String, i32)>),
    Group(Vec<(Vec<Alias>, i32)>),
}

impl Alias {
    pub fn parse(v: &Value) -> Alias {
        match v["type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
            "direct" => Alias::Direct(key(v["alias"].as_str().unwrap_or("")), key(v["target"].as_str().unwrap_or(""))),
            "random" => Alias::Random(
                key(v["alias"].as_str().unwrap_or("")),
                v["targets"].as_array().into_iter().flatten().map(|t| (key(t["data"].as_str().unwrap_or("")), t["weight"].as_i64().unwrap_or(1) as i32)).collect(),
            ),
            _ => Alias::Group(
                v["groups"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|g| (g["data"].as_array().into_iter().flatten().map(Alias::parse).collect(), g["weight"].as_i64().unwrap_or(1) as i32))
                    .collect(),
            ),
        }
    }

    pub fn targets(&self) -> Vec<String> {
        match self {
            Alias::Direct(_, t) => vec![t.clone()],
            Alias::Random(_, l) => l.iter().map(|x| x.0.clone()).collect(),
            Alias::Group(g) => g.iter().flat_map(|(l, _)| l.iter().flat_map(|a| a.targets())).collect(),
        }
    }

    fn resolve(&self, r: &mut dyn Rng, out: &mut HashMap<String, String>) {
        fn pick<'a, T>(l: &'a [(T, i32)], r: &mut dyn Rng) -> Option<&'a T> {
            let total: i32 = l.iter().map(|x| x.1).sum();
            if total <= 0 {
                return None;
            }
            let mut i = r.next_int_bound(total);
            for (t, w) in l {
                i -= w;
                if i < 0 {
                    return Some(t);
                }
            }
            None
        }
        match self {
            Alias::Direct(a, t) => {
                out.insert(a.clone(), t.clone());
            }
            Alias::Random(a, l) => {
                if let Some(t) = pick(l, r) {
                    out.insert(a.clone(), t.clone());
                }
            }
            Alias::Group(g) => {
                if let Some(list) = pick(g, r) {
                    for a in list {
                        a.resolve(r, out);
                    }
                }
            }
        }
    }
}

/// `PoolAliasLookup.create(aliases, pos, seed)`
fn alias_lookup(aliases: &[Alias], pos: P, seed: i64) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if aliases.is_empty() {
        return out;
    }
    let f = Legacy::new(seed).fork_positional();
    let mut r = f.at(pos[0], pos[1], pos[2]);
    for a in aliases {
        a.resolve(&mut r, &mut out);
    }
    out
}

// ------------------------------------------------------------ structure

pub struct JigsawStructure {
    pub start_pool: String,
    pub start_jigsaw: Option<String>,
    pub max_depth: i32,
    pub start_height: HeightP,
    pub expansion_hack: bool,
    pub project: Option<Hm>,
    pub max_h: i32,
    pub max_v: i32,
    pub aliases: Vec<Alias>,
    /// (bottom, top)
    pub padding: (i32, i32),
    pub waterlog: bool,
}

impl JigsawStructure {
    pub fn parse(pools: &mut Pools, b: &mut Builder, v: &Value) -> Result<JigsawStructure> {
        let (max_h, max_v) = match &v["max_distance_from_center"] {
            Value::Number(n) => (n.as_i64().unwrap_or(80) as i32, n.as_i64().unwrap_or(80) as i32),
            o => (o["horizontal"].as_i64().unwrap_or(80) as i32, o["vertical"].as_i64().unwrap_or(o["horizontal"].as_i64().unwrap_or(80)) as i32),
        };
        let padding = match &v["dimension_padding"] {
            Value::Number(n) => (n.as_i64().unwrap_or(0) as i32, n.as_i64().unwrap_or(0) as i32),
            Value::Null => (0, 0),
            o => (o["bottom"].as_i64().unwrap_or(0) as i32, o["top"].as_i64().unwrap_or(0) as i32),
        };
        let aliases: Vec<Alias> = v["pool_aliases"].as_array().into_iter().flatten().map(Alias::parse).collect();
        let start_pool = key(v["start_pool"].as_str().unwrap_or(""));
        pools.load_pool(b, &start_pool)?;
        for a in &aliases {
            for t in a.targets() {
                pools.load_pool(b, &t)?;
            }
        }
        Ok(JigsawStructure {
            start_pool,
            start_jigsaw: v["start_jigsaw_name"].as_str().map(key),
            max_depth: v["size"].as_i64().unwrap_or(1) as i32,
            start_height: HeightP::parse(&v["start_height"])?,
            expansion_hack: v["use_expansion_hack"].as_bool().unwrap_or(false),
            project: v["project_start_to_heightmap"].as_str().map(Hm::parse),
            max_h,
            max_v,
            aliases,
            padding,
            waterlog: v["liquid_settings"].as_str() != Some("ignore_waterlogging"),
        })
    }
}

/// Free space a piece may still be placed in: `outer` minus `cut` boxes
/// (the VoxelShape of vanilla; all boxes are block aligned).
struct Free {
    outer: ([f64; 3], [f64; 3]),
    cut: Vec<Bb>,
}

impl Free {
    fn from_bb(b: &Bb) -> Free {
        Free { outer: ([b.0[0] as f64, b.0[1] as f64, b.0[2] as f64], [(b.1[0] + 1) as f64, (b.1[1] + 1) as f64, (b.1[2] + 1) as f64]), cut: Vec::new() }
    }

    /// `!joinIsNotEmpty(free, box(bb).deflate(0.25), ONLY_SECOND)`
    fn fits(&self, b: &Bb) -> bool {
        for i in 0..3 {
            if (b.0[i] as f64 + 0.25) < self.outer.0[i] || ((b.1[i] + 1) as f64 - 0.25) > self.outer.1[i] {
                return false;
            }
        }
        !self.cut.iter().any(|c| template::bb_intersects(c, b))
    }
}

/// `SequencedPriorityIterator`: highest priority first, FIFO within one.
#[derive(Default)]
struct Queue<T> {
    by: BTreeMap<i32, VecDeque<T>>,
}

impl<T> Queue<T> {
    fn add(&mut self, t: T, prio: i32) {
        self.by.entry(prio).or_default().push_back(t);
    }
    fn next(&mut self) -> Option<T> {
        loop {
            let (&k, q) = self.by.iter_mut().next_back()?;
            if let Some(t) = q.pop_front() {
                return Some(t);
            }
            self.by.remove(&k);
        }
    }
}

struct Placer<'a> {
    pools: &'a Pools,
    max_depth: i32,
    ctx: &'a dyn Ctx,
    aliases: &'a HashMap<String, String>,
    waterlog: bool,
    pieces: Vec<Piece>,
    /// free spaces (index 0: the structure's whole area)
    frees: Vec<Free>,
    queue: Queue<(usize, usize, i32)>,
}

impl Placer<'_> {
    fn pool(&self, name: &str) -> Option<&Arc<Pool>> {
        let n = self.aliases.get(name).map(String::as_str).unwrap_or(name);
        self.pools.pools.get(n)
    }

    /// `tryPlacingChildren(piece, free, depth, useExpansionHack, ...)`
    fn children(&mut self, pi: usize, free: usize, depth: i32, hack: bool, r: &mut dyn Rng) {
        let (elem, pos, rot, bb) = {
            let p = &self.pieces[pi];
            (p.elem.clone(), p.pos, p.rot, p.bb)
        };
        let rigid = elem.projection == Projection::Rigid;
        let mut inner: Option<usize> = None;
        let min_y = bb.0[1];
        for jig in elem.shuffled_jigsaws(pos, rot, r) {
            let (front, _) = jigsaw_front_top(jig.state);
            let jpos = jig.pos;
            let target_pos = add(jpos, dir_step(front));
            let rel_y = jpos[1] - min_y;
            let mut surface = i32::MIN;
            let pool_name = self.aliases.get(&jig.pool).cloned().unwrap_or_else(|| jig.pool.clone());
            let Some(pool) = self.pools.pools.get(&pool_name).cloned() else { continue };
            if pool.templates.is_empty() && pool_name != "empty" {
                continue;
            }
            let Some(fallback) = self.pools.pools.get(&pool.fallback).cloned() else { continue };
            if fallback.templates.is_empty() && pool.fallback != "empty" {
                continue;
            }
            let fi = if bb_inside(&bb, target_pos) {
                *inner.get_or_insert_with(|| {
                    self.frees.push(Free::from_bb(&bb));
                    self.frees.len() - 1
                })
            } else {
                free
            };
            let mut cands: Vec<Arc<Elem>> = Vec::new();
            if depth != self.max_depth {
                let mut l = pool.templates.clone();
                shuffle(&mut l, r);
                cands.extend(l);
            }
            let mut l = fallback.templates.clone();
            shuffle(&mut l, r);
            cands.extend(l);
            let prio = jig.placement_priority;
            'cand: for cand in cands {
                if cand.is_empty() {
                    break;
                }
                let mut rots = [0, 1, 2, 3];
                shuffle(&mut rots, r);
                for crot in rots {
                    let cjigs = cand.shuffled_jigsaws([0, 0, 0], crot, r);
                    let cbb0 = cand.bounding_box([0, 0, 0], crot);
                    let expand = if !hack || cbb0.1[1] - cbb0.0[1] + 1 > 16 {
                        0
                    } else {
                        cjigs
                            .iter()
                            .map(|cj| {
                                let (f, _) = jigsaw_front_top(cj.state);
                                if !bb_inside(&cbb0, add(cj.pos, dir_step(f))) {
                                    return 0;
                                }
                                let p = self.pool(&cj.pool);
                                let a = p.map_or(0, |p| p.max_size());
                                let b = p.and_then(|p| self.pools.pools.get(&p.fallback)).map_or(0, |p| p.max_size());
                                a.max(b)
                            })
                            .max()
                            .unwrap_or(0)
                    };
                    for cj in &cjigs {
                        if !can_attach(&jig, cj) {
                            continue;
                        }
                        let cpos = cj.pos;
                        let offset = sub(target_pos, cpos);
                        let cbb = cand.bounding_box(offset, crot);
                        let cmin = cbb.0[1];
                        let crigid = cand.projection == Projection::Rigid;
                        let cy = cpos[1];
                        let dy = rel_y - cy + dir_step(front)[1];
                        let base = if rigid && crigid {
                            min_y + dy
                        } else {
                            if surface == i32::MIN {
                                surface = self.ctx.base_height(jpos[0], jpos[2], Hm::NotAir);
                            }
                            surface - cy
                        };
                        let shift = base - cmin;
                        let mut moved = (add(cbb.0, [0, shift, 0]), add(cbb.1, [0, shift, 0]));
                        let npos = add(offset, [0, shift, 0]);
                        if expand > 0 {
                            let k = (expand + 1).max(moved.1[1] - moved.0[1]);
                            moved.1[1] = moved.1[1].max(moved.0[1] + k);
                        }
                        if !self.frees[fi].fits(&moved) {
                            continue;
                        }
                        self.frees[fi].cut.push(moved);
                        let pgd = self.pieces[pi].ground_delta;
                        let cgd = if crigid { pgd - dy } else { 1 };
                        let jy = if rigid {
                            min_y + rel_y
                        } else if crigid {
                            base + cy
                        } else {
                            if surface == i32::MIN {
                                surface = self.ctx.base_height(jpos[0], jpos[2], Hm::NotAir);
                            }
                            surface + dy / 2
                        };
                        self.pieces[pi].junctions.push(Junction { x: target_pos[0], ground_y: jy - rel_y + pgd, z: target_pos[2], dy, rigid: crigid });
                        let ci = self.pieces.len();
                        self.pieces.push(Piece {
                            elem: cand.clone(),
                            pos: npos,
                            rot: crot,
                            bb: moved,
                            ground_delta: cgd,
                            junctions: vec![Junction { x: jpos[0], ground_y: jy - cy + cgd, z: jpos[2], dy: -dy, rigid }],
                            waterlog: self.waterlog,
                        });
                        if depth + 1 <= self.max_depth {
                            self.queue.add((ci, fi, depth + 1), prio);
                        }
                        break 'cand;
                    }
                }
            }
        }
    }
}

/// `JigsawBlock.canAttach(source, target)`
fn can_attach(a: &Jigsaw, b: &Jigsaw) -> bool {
    let (fa, ta) = jigsaw_front_top(a.state);
    let (fb, tb) = jigsaw_front_top(b.state);
    fa == dir_opposite(fb) && (a.rollable || ta == tb) && a.target == b.name
}

#[derive(Clone, Copy, Debug)]
pub struct Junction {
    pub x: i32,
    pub ground_y: i32,
    pub z: i32,
    pub dy: i32,
    pub rigid: bool,
}

/// `JigsawPlacement.addPieces`: the generation stub (center position and
/// pieces), or None.
/// `valid` is the biome check at the stub position, done before the
/// children are assembled (like `findValidGenerationPoint`).
#[allow(clippy::too_many_arguments)]
pub fn generate(s: &JigsawStructure, pools: &Pools, ctx: &dyn Ctx, seed: i64, cx: i32, cz: i32, r: &mut Legacy, valid: &dyn Fn(P) -> bool) -> Option<Vec<Piece>> {
    let y0 = s.start_height.sample(r);
    let start = [cx * 16, y0, cz * 16];
    let aliases = alias_lookup(&s.aliases, start, seed);
    let rot = r.next_int_bound(4);
    let pool_name = aliases.get(&s.start_pool).cloned().unwrap_or_else(|| s.start_pool.clone());
    let pool = pools.pools.get(&pool_name)?;
    if pool.templates.is_empty() {
        return None;
    }
    let elem = pool.templates[r.next_int_bound(pool.templates.len() as i32) as usize].clone();
    if elem.is_empty() {
        return None;
    }
    let anchor = match &s.start_jigsaw {
        Some(name) => elem.shuffled_jigsaws(start, rot, r).into_iter().find(|j| &j.name == name)?.pos,
        None => start,
    };
    let rel = sub(anchor, start);
    let pos = sub(start, rel);
    let bb = elem.bounding_box(pos, rot);
    let mut piece = Piece { elem: elem.clone(), pos, rot, bb, ground_delta: 1, junctions: Vec::new(), waterlog: s.waterlog };
    let ccx = (bb.1[0] + bb.0[0]) / 2;
    let ccz = (bb.1[2] + bb.0[2]) / 2;
    let y = match s.project {
        None => pos[1],
        Some(hm) => start[1] + ctx.base_height(ccx, ccz, hm),
    };
    let dy = y - (bb.0[1] + piece.ground_delta);
    piece.pos[1] += dy;
    piece.bb.0[1] += dy;
    piece.bb.1[1] += dy;
    let (min_y, max_y) = (crate::worldgen::chunk::MIN_Y, crate::worldgen::chunk::MIN_Y + crate::worldgen::chunk::HEIGHT - 1);
    if s.padding != (0, 0) && (piece.bb.0[1] < min_y + s.padding.0 || piece.bb.1[1] > max_y - s.padding.1) {
        return None;
    }
    let center_y = y + rel[1];
    if !valid([ccx, center_y, ccz]) {
        return None;
    }
    let mut pieces = vec![piece];
    if s.max_depth > 0 {
        let lo = [(ccx - s.max_h) as f64, (center_y - s.max_v).max(min_y + s.padding.0) as f64, (ccz - s.max_h) as f64];
        let hi = [(ccx + s.max_h + 1) as f64, (center_y + s.max_v + 1).min(max_y + 1 - s.padding.1) as f64, (ccz + s.max_h + 1) as f64];
        let first_bb = pieces[0].bb;
        let mut pl = Placer { pools, max_depth: s.max_depth, ctx, aliases: &aliases, waterlog: s.waterlog, pieces, frees: vec![Free { outer: (lo, hi), cut: vec![first_bb] }], queue: Queue::default() };
        pl.children(0, 0, 0, s.expansion_hack, r);
        while let Some((pi, fi, depth)) = pl.queue.next() {
            pl.children(pi, fi, depth, s.expansion_hack, r);
        }
        pieces = pl.pieces;
    }
    Some(pieces)
}
