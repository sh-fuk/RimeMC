//! Vanilla feature decoration (`ChunkGenerator.applyBiomeDecoration`):
//! placed features, the global feature order (`FeatureSorter`), decoration
//! seeds and a `WorldGenRegion`-like 3x3 chunk view.

pub mod extra;
pub mod extra2;
pub mod extra3;
pub mod extra4;
pub mod fossil;
pub mod feat;
pub mod place;
pub mod survive;
pub mod tree;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use super::chunk::{HEIGHT, MIN_Y};
use super::density::Loader;
use super::noise::PerlinSimplexNoise;
use super::random::{Legacy, WgRandom};
use crate::registry::{blocks, vstate, BlockState};
use feat::Feature;
use place::{BlockSet, Modifier};

/// Heightmap kinds (vanilla types that share a predicate are merged).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hm {
    /// WORLD_SURFACE(_WG): not air
    NotAir = 0,
    /// OCEAN_FLOOR(_WG): blocks motion
    Motion = 1,
    /// MOTION_BLOCKING: blocks motion or has fluid
    MotionFluid = 2,
    /// MOTION_BLOCKING_NO_LEAVES
    NoLeaves = 3,
}

impl Hm {
    pub fn parse(s: &str) -> Hm {
        match s {
            "WORLD_SURFACE" | "WORLD_SURFACE_WG" => Hm::NotAir,
            "OCEAN_FLOOR" | "OCEAN_FLOOR_WG" => Hm::Motion,
            "MOTION_BLOCKING" => Hm::MotionFluid,
            _ => Hm::NoLeaves,
        }
    }
    pub fn test(self, s: BlockState) -> bool {
        match self {
            Hm::NotAir => !vstate::air(s),
            Hm::Motion => vstate::blocks_motion(s),
            Hm::MotionFluid => vstate::blocks_motion(s) || vstate::has_fluid(s),
            Hm::NoLeaves => (vstate::blocks_motion(s) || vstate::has_fluid(s)) && !is_leaves(s),
        }
    }
}

pub fn is_leaves(s: BlockState) -> bool {
    s.name().ends_with("_leaves")
}

/// A chunk between the carver and feature steps (and while decorating).
#[derive(Clone)]
pub struct Proto {
    /// `((y - MIN_Y) * 16 + z) * 16 + x`
    pub blocks: Vec<BlockState>,
    /// network biome ids per section
    pub biomes: Vec<[u8; 64]>,
    /// top block y per heightmap kind (MIN_Y - 1 when empty)
    pub hm: [[i32; 256]; 4],
    /// positions marked for post processing (`markPosForPostprocessing`)
    pub post: Vec<[i32; 3]>,
    /// scheduled fluid ticks (`scheduleTick`)
    pub ticks: Vec<[i32; 3]>,
    /// entities added during generation (`addFreshEntity`): (type, pos)
    pub entities: Vec<(&'static str, [i32; 3])>,
}

#[inline]
fn idx(x: i32, y: i32, z: i32) -> usize {
    (((y - MIN_Y) * 16 + z) * 16 + x) as usize
}

impl Proto {
    pub fn new(blocks: Vec<BlockState>, biomes: Vec<[u8; 64]>) -> Self {
        let mut hm = [[MIN_Y - 1; 256]; 4];
        for (k, h) in hm.iter_mut().enumerate() {
            let kind = [Hm::NotAir, Hm::Motion, Hm::MotionFluid, Hm::NoLeaves][k];
            for z in 0..16 {
                for x in 0..16 {
                    for y in (MIN_Y..MIN_Y + HEIGHT).rev() {
                        if kind.test(blocks[idx(x, y, z)]) {
                            h[(z * 16 + x) as usize] = y;
                            break;
                        }
                    }
                }
            }
        }
        Proto { blocks, biomes, hm, post: Vec::new(), ticks: Vec::new(), entities: Vec::new() }
    }

    pub fn get(&self, x: i32, y: i32, z: i32) -> BlockState {
        if !(MIN_Y..MIN_Y + HEIGHT).contains(&y) {
            return blocks::VOID_AIR;
        }
        self.blocks[idx(x, y, z)]
    }

    pub fn set(&mut self, x: i32, y: i32, z: i32, s: BlockState) {
        if !(MIN_Y..MIN_Y + HEIGHT).contains(&y) {
            return;
        }
        self.blocks[idx(x, y, z)] = s;
        let col = (z * 16 + x) as usize;
        for (k, kind) in [Hm::NotAir, Hm::Motion, Hm::MotionFluid, Hm::NoLeaves].into_iter().enumerate() {
            let top = &mut self.hm[k][col];
            if kind.test(s) {
                if y > *top {
                    *top = y;
                }
            } else if y == *top {
                let mut h = y - 1;
                while h >= MIN_Y && !kind.test(self.blocks[idx(x, h, z)]) {
                    h -= 1;
                }
                *top = h;
            }
        }
    }
}

/// `WorldGenRegion` for decorating chunk (cx, cz): its 3x3 neighbourhood.
pub struct Region<'a> {
    pub cx: i32,
    pub cz: i32,
    /// index (dx + 1) * 3 + (dz + 1)
    pub chunks: Vec<Box<Proto>>,
    pub lib: &'a Lib,
    pub zoom_seed: i64,
    /// world seed (`WorldGenLevel.getSeed`)
    pub seed: i64,
    /// noise biome outside the region (computed from the biome source)
    pub outside: &'a (dyn Fn(i32, i32, i32) -> u8 + Sync),
}

impl Region<'_> {
    #[inline]
    fn slot(&self, x: i32, z: i32) -> Option<usize> {
        let dx = (x >> 4) - self.cx + 1;
        let dz = (z >> 4) - self.cz + 1;
        if (0..3).contains(&dx) && (0..3).contains(&dz) {
            Some((dx * 3 + dz) as usize)
        } else {
            None
        }
    }

    pub fn get(&self, p: [i32; 3]) -> BlockState {
        match self.slot(p[0], p[2]) {
            Some(i) => self.chunks[i].get(p[0] & 15, p[1], p[2] & 15),
            None => blocks::VOID_AIR,
        }
    }

    /// `WorldGenRegion.setBlock`: false outside the writable 3x3 area.
    pub fn set(&mut self, p: [i32; 3], s: BlockState) -> bool {
        match self.slot(p[0], p[2]) {
            Some(i) => {
                self.chunks[i].set(p[0] & 15, p[1], p[2] & 15, s);
                true
            }
            None => false,
        }
    }

    /// Section write without heightmap updates (`BulkSectionAccess`).
    pub fn set_raw(&mut self, p: [i32; 3], s: BlockState) {
        if let Some(i) = self.slot(p[0], p[2]) {
            if (MIN_Y..MIN_Y + HEIGHT).contains(&p[1]) {
                self.chunks[i].blocks[idx(p[0] & 15, p[1], p[2] & 15)] = s;
            }
        }
    }

    /// `ensureCanWrite`
    pub fn can_write(&self, p: [i32; 3]) -> bool {
        self.slot(p[0], p[2]).is_some()
    }

    /// `getHeight(type, x, z)`: first free y above the top block.
    pub fn height(&self, hm: Hm, x: i32, z: i32) -> i32 {
        match self.slot(x, z) {
            Some(i) => self.chunks[i].hm[hm as usize][((z & 15) * 16 + (x & 15)) as usize] + 1,
            None => MIN_Y,
        }
    }

    /// `getFluidTicks().schedule` (world coordinates)
    pub fn schedule_tick(&mut self, p: [i32; 3]) {
        if let Some(i) = self.slot(p[0], p[2]) {
            self.chunks[i].ticks.push(p);
        }
    }

    /// `markPosForPostprocessing` (world coordinates)
    pub fn mark_post(&mut self, p: [i32; 3]) {
        if let Some(i) = self.slot(p[0], p[2]) {
            self.chunks[i].post.push(p);
        }
    }

    /// `addFreshEntity` during generation
    pub fn spawn_entity(&mut self, kind: &'static str, p: [i32; 3]) {
        if let Some(i) = self.slot(p[0], p[2]) {
            self.chunks[i].entities.push((kind, p));
        }
    }

    pub fn is_empty(&self, p: [i32; 3]) -> bool {
        vstate::air(self.get(p))
    }

    fn noise_biome(&self, qx: i32, qy: i32, qz: i32) -> u8 {
        let qy = qy.clamp(MIN_Y >> 2, ((MIN_Y + HEIGHT) >> 2) - 1);
        let Some(i) = self.slot(qx << 2, qz << 2) else {
            return (self.outside)(qx, qy, qz);
        };
        let sec = &self.chunks[i].biomes[((qy - (MIN_Y >> 2)) >> 2) as usize];
        sec[(((qy & 3) << 4) | ((qz & 3) << 2) | (qx & 3)) as usize]
    }

    /// `getBiome(pos)` (fuzzy zoom), network biome id.
    pub fn biome(&self, p: [i32; 3]) -> u8 {
        super::surface::fuzzy_biome(self.zoom_seed, p[0], p[1], p[2], &|x, y, z| self.noise_biome(x, y, z))
    }
}

/// `LevelChunk.postProcessGeneration` for the centre chunk of `lv`:
/// `Block.updateFromNeighbourShapes` at each marked position, which for the
/// blocks features leave there means plants that lost their support break.
pub fn post_process(lv: &mut Region) {
    let marks = std::mem::take(&mut lv.chunks[4].post);
    for p in marks {
        let s = lv.get(p);
        if vstate::air(s) || vstate::liquid(s) {
            continue;
        }
        if !survive::can_survive(lv, s, p) {
            lv.set(p, if vstate::is_water(s) { blocks::WATER } else { blocks::AIR });
            continue;
        }
        let n = shape_update(lv, s, p);
        if n != s {
            lv.set(p, n);
        }
    }
}

/// `Block.updateFromNeighbourShapes` (UPDATE_SHAPE_ORDER: west, east,
/// north, south, down, up) for the blocks structures and features leave:
/// fence / pane / wall connections and the support of torches and ladders.
pub fn shape_update(lv: &Region, s: BlockState, p: [i32; 3]) -> BlockState {
    let mut out = s;
    for (d, o) in UPDATE_ORDER {
        let q = [p[0] + o[0], p[1] + o[1], p[2] + o[2]];
        out = update_shape(lv, out, p, d, lv.get(q));
        if out.is_air() {
            break;
        }
    }
    out
}

/// direction ordinal and offset, in `Block.UPDATE_SHAPE_ORDER`
pub const UPDATE_ORDER: [(u8, [i32; 3]); 6] = [(4, [-1, 0, 0]), (5, [1, 0, 0]), (2, [0, 0, -1]), (3, [0, 0, 1]), (0, [0, -1, 0]), (1, [0, 1, 0])];

fn dir_prop(d: u8) -> &'static str {
    ["down", "up", "north", "south", "west", "east"][d as usize]
}

/// `Block.isExceptionForConnection`
fn connection_exception(nb: BlockState) -> bool {
    let nn = nb.name();
    is_leaves(nb) || matches!(nn, "barrier" | "carved_pumpkin" | "jack_o_lantern" | "melon" | "pumpkin") || nn.ends_with("shulker_box")
}

fn is_wall(n: &str) -> bool {
    n.ends_with("_wall")
}

/// `instanceof IronBarsBlock` (iron / copper bars and glass panes)
fn is_bars(n: &str) -> bool {
    n.ends_with("_bars") || n.ends_with("glass_pane")
}

/// `FenceGateBlock.connectsToDirection`
fn gate_connects(nb: BlockState, d: u8) -> bool {
    nb.name().ends_with("_fence_gate") && {
        let f = crate::registry::BlockRegistry::get().property(nb, "facing").unwrap_or("north");
        matches!(f, "east" | "west") != (d >= 4)
    }
}

/// `BlockState.updateShape` towards one neighbour (direction ordinal `d`).
pub fn update_shape(lv: &Region, s: BlockState, p: [i32; 3], d: u8, nb: BlockState) -> BlockState {
    let reg = crate::registry::BlockRegistry::get();
    let name = s.name();
    let horizontal = d >= 2;
    if name.ends_with("_fence") && horizontal {
        let wooden = name != "nether_brick_fence";
        let nn = nb.name();
        let same = nn.ends_with("_fence") && (nn != "nether_brick_fence") == wooden;
        let c = (!connection_exception(nb) && vstate::sturdy(nb, d ^ 1)) || same || gate_connects(nb, d);
        return reg.with(s, dir_prop(d), if c { "true" } else { "false" }).unwrap_or(s);
    }
    if is_bars(name) && horizontal {
        let nn = nb.name();
        let c = (!connection_exception(nb) && vstate::sturdy(nb, d ^ 1)) || is_bars(nn) || is_wall(nn);
        return reg.with(s, dir_prop(d), if c { "true" } else { "false" }).unwrap_or(s);
    }
    if is_wall(name) {
        return wall_update(lv, s, p, d, nb);
    }
    if name.ends_with("_stairs") && horizontal {
        return reg.with(s, "shape", stairs_shape(lv, s, p)).unwrap_or(s);
    }
    if matches!(name, "wall_torch" | "soul_wall_torch" | "copper_wall_torch" | "redstone_wall_torch" | "ladder") {
        // support behind: the face pointing the way the block faces
        let f = reg.property(s, "facing").unwrap_or("north");
        if dir_prop(d ^ 1) == f && !vstate::sturdy(nb, d ^ 1) {
            return blocks::AIR;
        }
    }
    s
}

fn facing_offset(f: &str) -> [i32; 3] {
    match f {
        "north" => [0, 0, -1],
        "south" => [0, 0, 1],
        "west" => [-1, 0, 0],
        _ => [1, 0, 0],
    }
}

fn counter_clockwise(f: &str) -> &'static str {
    match f {
        "north" => "west",
        "west" => "south",
        "south" => "east",
        _ => "north",
    }
}

fn opposite_facing(f: &str) -> &'static str {
    match f {
        "north" => "south",
        "south" => "north",
        "west" => "east",
        _ => "west",
    }
}

/// `StairBlock.getStairsShape`
fn stairs_shape(lv: &Region, s: BlockState, p: [i32; 3]) -> &'static str {
    let reg = crate::registry::BlockRegistry::get();
    let prop = |b: BlockState, k: &str| reg.property(b, k).unwrap_or("").to_string();
    let is_stairs = |b: BlockState| b.name().ends_with("_stairs");
    let dir = prop(s, "facing");
    let half = prop(s, "half");
    let x_axis = |f: &str| matches!(f, "east" | "west");
    let at = |f: &str| {
        let o = facing_offset(f);
        lv.get([p[0] + o[0], p[1], p[2] + o[2]])
    };
    // canTakeShape: the block on that side is not the same stairs
    let can_take = |f: &str| {
        let b = at(f);
        !is_stairs(b) || prop(b, "facing") != dir || prop(b, "half") != half
    };
    let front = at(&dir);
    if is_stairs(front) && prop(front, "half") == half {
        let d1 = prop(front, "facing");
        if x_axis(&d1) != x_axis(&dir) && can_take(opposite_facing(&d1)) {
            return if d1 == counter_clockwise(&dir) { "outer_left" } else { "outer_right" };
        }
    }
    let back = at(opposite_facing(&dir));
    if is_stairs(back) && prop(back, "half") == half {
        let d2 = prop(back, "facing");
        if x_axis(&d2) != x_axis(&dir) && can_take(&d2) {
            return if d2 == counter_clockwise(&dir) { "inner_left" } else { "inner_right" };
        }
    }
    "straight"
}

/// which 2D parts (x0, x1, z0, z1 in 1/16) of the bottom face of `above`
/// are covered, for `WallBlock.isCovered`
fn covers(above: BlockState, rect: (i32, i32, i32, i32)) -> bool {
    let reg = crate::registry::BlockRegistry::get();
    let n = above.name();
    if is_wall(n) {
        // collision shape: post (4..12) when up, arms through the centre
        let side = |k: &str| reg.property(above, k).is_some_and(|v| v != "none");
        let (nn, ss, ww, ee) = (side("north"), side("south"), side("west"), side("east"));
        let up = reg.property(above, "up") == Some("true");
        return match rect {
            (7, 9, 0, 9) => nn,
            (7, 9, 7, 16) => ss,
            (0, 9, 7, 9) => ww,
            (7, 16, 7, 9) => ee,
            _ => up || nn || ss || ww || ee,
        };
    }
    if n.ends_with("_fence") || is_bars(n) {
        let side = |k: &str| reg.property(above, k) == Some("true");
        return match rect {
            (7, 9, 0, 9) => side("north"),
            (7, 9, 7, 16) => side("south"),
            (0, 9, 7, 9) => side("west"),
            (7, 16, 7, 9) => side("east"),
            _ => true,
        };
    }
    vstate::sturdy(above, 0)
}

const POST: (i32, i32, i32, i32) = (7, 9, 7, 9);

/// `WallBlock.updateShape`
fn wall_update(lv: &Region, s: BlockState, p: [i32; 3], d: u8, nb: BlockState) -> BlockState {
    let reg = crate::registry::BlockRegistry::get();
    if d == 0 {
        return s;
    }
    let connected = |k: &str| reg.property(s, k).is_some_and(|v| v != "none");
    let mut c = [connected("north"), connected("east"), connected("south"), connected("west")];
    let above_p = [p[0], p[1] + 1, p[2]];
    let above = if d == 1 { nb } else { lv.get(above_p) };
    if d >= 2 {
        let nn = nb.name();
        let attach = is_wall(nn) || (!connection_exception(nb) && vstate::sturdy(nb, d ^ 1)) || is_bars(nn) || gate_connects(nb, d);
        let i = match d {
            2 => 0,
            5 => 1,
            3 => 2,
            _ => 3,
        };
        c[i] = attach;
    }
    const TESTS: [(&str, (i32, i32, i32, i32)); 4] = [("north", (7, 9, 0, 9)), ("east", (7, 16, 7, 9)), ("south", (7, 9, 7, 16)), ("west", (0, 9, 7, 9))];
    let mut out = s;
    let mut sides = ["none"; 4];
    for (i, (k, rect)) in TESTS.iter().enumerate() {
        sides[i] = if !c[i] {
            "none"
        } else if covers(above, *rect) {
            "tall"
        } else {
            "low"
        };
        out = reg.with(out, k, sides[i]).unwrap_or(out);
    }
    // shouldRaisePost
    let up = if is_wall(above.name()) && reg.property(above, "up") == Some("true") {
        true
    } else {
        let [n, e, so, w] = sides;
        let (nn, en, sn, wn) = (n == "none", e == "none", so == "none", w == "none");
        if (nn && sn && wn && en) || nn != sn || wn != en {
            true
        } else if (n == "tall" && so == "tall") || (e == "tall" && w == "tall") {
            false
        } else {
            wall_post_override(above.name()) || covers(above, POST)
        }
    };
    reg.with(out, "up", if up { "true" } else { "false" }).unwrap_or(out)
}

/// `BlockTags.WALL_POST_OVERRIDE`
fn wall_post_override(n: &str) -> bool {
    matches!(n, "torch" | "soul_torch" | "redstone_torch" | "copper_torch" | "tripwire" | "cactus_flower") || (n.ends_with("_sign") && !n.ends_with("hanging_sign")) || n.ends_with("_banner") || n.ends_with("_pressure_plate")
}

/// A placed feature.
pub struct Placed {
    pub name: Option<String>,
    pub mods: Vec<Modifier>,
    pub feature: Arc<Feature>,
}

impl Placed {
    fn run(&self, lv: &mut Region, r: &mut WgRandom, top: Option<&str>, i: usize, p: [i32; 3]) -> bool {
        if i == self.mods.len() {
            let ok = self.feature.place(lv, r, p);
            if cfg!(test) && top.is_some() && std::env::var("RIME_TRACE").ok().as_deref() == top {
                eprintln!("trace {:?} chunk {},{} at {:?} -> {ok}", top, lv.cx, lv.cz, p);
            }
            return ok;
        }
        let ps = self.mods[i].positions(lv, r, top, p);
        if cfg!(test) && top.is_some() && std::env::var("RIME_TRACE").ok().as_deref() == top {
            eprintln!("trace-mod {i} {:?} -> {:?}", p, ps);
        }
        let mut any = false;
        for q in ps {
            any |= self.run(lv, r, top, i + 1, q);
        }
        any
    }

    /// `PlacedFeature.place` (no biome check context).
    pub fn place(&self, lv: &mut Region, r: &mut WgRandom, p: [i32; 3]) -> bool {
        self.run(lv, r, None, 0, p)
    }

    /// `placeWithBiomeCheck`
    pub fn place_top(&self, lv: &mut Region, r: &mut WgRandom, p: [i32; 3]) -> bool {
        let name = self.name.clone();
        self.run(lv, r, name.as_deref(), 0, p)
    }
}

/// All features, their global order and per-biome membership.
pub struct Lib {
    /// per step: placed features in `FeatureSorter` order
    pub steps: Vec<Vec<Arc<Placed>>>,
    /// biome id -> per step: indices into `steps[step]`
    biome_steps: Vec<Vec<Vec<usize>>>,
    /// biome id -> placed feature names (any step)
    biome_features: Vec<HashSet<String>>,
    info_noise: PerlinSimplexNoise,
    pub temps: super::surface::BiomeTemps,
    /// biome id -> (temperature, downfall, temperature modifier frozen)
    pub climates: Vec<super::surface::Climate>,
    pub sea_level: i32,
    pub tags: survive::Tags,
    /// per block state: 0 other, 1 has a leaf `distance`, 2 prevents nearby leaf decay
    pub leaf_kind: Vec<u8>,
    pub extra_tags: extra::Tags,
}

/// Parsing state (tags and shared configured / placed features).
pub struct Builder<'l> {
    load: &'l Loader<'l>,
    tags: HashMap<String, BlockSet>,
    configured: HashMap<String, Arc<Feature>>,
    placed: HashMap<String, Arc<Placed>>,
    /// features whose type is not ported yet (reported once)
    pub missing: BTreeMap<String, usize>,
}

impl<'l> Builder<'l> {
    pub fn new(load: &'l Loader<'l>) -> Self {
        Builder { load, tags: HashMap::new(), configured: HashMap::new(), placed: HashMap::new(), missing: BTreeMap::new() }
    }

    /// Reads a data pack file (`worldgen/...`, `tags/...`, `structure/...`).
    pub fn load(&self, rel: &str) -> Result<Value> {
        anyhow::Context::with_context((self.load)(rel), || format!("loading {rel}"))
    }

    pub fn tag(&mut self, tag: &str) -> Result<BlockSet> {
        let key = tag.trim_start_matches('#').trim_start_matches("minecraft:").to_string();
        if let Some(t) = self.tags.get(&key) {
            return Ok(t.clone());
        }
        let t = BlockSet(Arc::new(super::carver::block_tag(self.load, &key)?));
        self.tags.insert(key, t.clone());
        Ok(t)
    }

    /// A configured feature: name or inline object.
    pub fn configured(&mut self, v: &Value) -> Result<Arc<Feature>> {
        if let Some(name) = v.as_str() {
            let key = name.trim_start_matches("minecraft:").to_string();
            if let Some(f) = self.configured.get(&key) {
                return Ok(f.clone());
            }
            let json = (self.load)(&format!("worldgen/configured_feature/{key}"))?;
            let f = Arc::new(Feature::parse(self, &json)?);
            self.configured.insert(key, f.clone());
            return Ok(f);
        }
        Ok(Arc::new(Feature::parse(self, v)?))
    }

    /// A placed feature: name or inline object.
    pub fn placed(&mut self, v: &Value) -> Result<Arc<Placed>> {
        if let Some(name) = v.as_str() {
            let key = name.trim_start_matches("minecraft:").to_string();
            if let Some(f) = self.placed.get(&key) {
                return Ok(f.clone());
            }
            let json = (self.load)(&format!("worldgen/placed_feature/{key}"))?;
            let mut p = self.placed_inline(&json)?;
            p.name = Some(key.clone());
            let p = Arc::new(p);
            self.placed.insert(key, p.clone());
            return Ok(p);
        }
        Ok(Arc::new(self.placed_inline(v)?))
    }

    fn placed_inline(&mut self, v: &Value) -> Result<Placed> {
        let feature = self.configured(&v["feature"])?;
        let mut mods = Vec::new();
        for m in v["placement"].as_array().into_iter().flatten() {
            mods.push(Modifier::parse(self, m)?);
        }
        Ok(Placed { name: None, mods, feature })
    }
}

impl Lib {
    /// `biomes`: overworld biome names in `possibleBiomes()` order.
    pub fn new(load: &Loader, biomes: &[String], temps: super::surface::BiomeTemps, climates: Vec<super::surface::Climate>, sea_level: i32) -> Result<(Lib, BTreeMap<String, usize>)> {
        let mut b = Builder { load, tags: HashMap::new(), configured: HashMap::new(), placed: HashMap::new(), missing: BTreeMap::new() };
        let names = crate::world::chunk::Biomes::get();
        // per biome: per step list of placed feature names
        let mut lists: Vec<(String, Vec<Vec<String>>)> = Vec::new();
        for biome in biomes {
            let json = load(&format!("worldgen/biome/{}", biome.trim_start_matches("minecraft:")))?;
            let steps: Vec<Vec<String>> = json["features"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| match s {
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str()).map(|x| x.trim_start_matches("minecraft:").to_string()).collect(),
                    Value::String(x) => vec![x.trim_start_matches("minecraft:").to_string()],
                    _ => vec![],
                })
                .collect();
            lists.push((biome.clone(), steps));
        }
        // FeatureSorter.buildFeaturesPerStep
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut graph: BTreeMap<(usize, usize), BTreeSet<(usize, usize)>> = BTreeMap::new();
        let mut by_key: HashMap<(usize, usize), String> = HashMap::new();
        let mut max_steps = 0;
        for (_, steps) in &lists {
            max_steps = max_steps.max(steps.len());
            let mut list = Vec::new();
            for (step, fs) in steps.iter().enumerate() {
                for f in fs {
                    let n = index.len();
                    let i = *index.entry(f.clone()).or_insert(n);
                    by_key.insert((step, i), f.clone());
                    list.push((step, i));
                }
            }
            for i in 0..list.len() {
                let set = graph.entry(list[i]).or_default();
                if i + 1 < list.len() {
                    set.insert(list[i + 1]);
                }
            }
        }
        fn dfs(graph: &BTreeMap<(usize, usize), BTreeSet<(usize, usize)>>, done: &mut BTreeSet<(usize, usize)>, visiting: &mut BTreeSet<(usize, usize)>, out: &mut Vec<(usize, usize)>, n: (usize, usize)) -> bool {
            if done.contains(&n) {
                return false;
            }
            if visiting.contains(&n) {
                return true;
            }
            visiting.insert(n);
            if let Some(next) = graph.get(&n) {
                for &t in next {
                    if dfs(graph, done, visiting, out, t) {
                        return true;
                    }
                }
            }
            visiting.remove(&n);
            done.insert(n);
            out.push(n);
            false
        }
        let (mut done, mut visiting, mut sorted) = (BTreeSet::new(), BTreeSet::new(), Vec::new());
        for &k in graph.keys() {
            if !done.contains(&k) && dfs(&graph, &mut done, &mut visiting, &mut sorted, k) {
                anyhow::bail!("feature order cycle");
            }
        }
        sorted.reverse();
        let steps_len = max_steps.max(11);
        let mut steps: Vec<Vec<Arc<Placed>>> = vec![Vec::new(); steps_len];
        let mut pos: Vec<HashMap<String, usize>> = vec![HashMap::new(); steps_len];
        for (step, i) in sorted {
            let name = &by_key[&(step, i)];
            pos[step].insert(name.clone(), steps[step].len());
            steps[step].push(b.placed(&Value::String(name.clone()))?);
        }
        let n = names.names.len();
        let mut biome_steps = vec![vec![Vec::new(); steps_len]; n];
        let mut biome_features = vec![HashSet::new(); n];
        for (biome, fs) in &lists {
            let Some(id) = names.id(biome) else { continue };
            for (step, list) in fs.iter().enumerate() {
                for f in list {
                    biome_steps[id as usize][step].push(pos[step][f]);
                    biome_features[id as usize].insert(f.clone());
                }
            }
        }
        let tags = survive::Tags::load(&mut b)?;
        let no_decay = b.tag("prevents_nearby_leaf_decay")?;
        let reg = crate::registry::BlockRegistry::get();
        let leaf_kind = (0..reg.state_count())
            .map(|i| {
                let s = BlockState(i as u16);
                if no_decay.contains(s) {
                    2
                } else if reg.property(s, "distance").is_some() && reg.property(s, "persistent").is_some() {
                    1
                } else {
                    0
                }
            })
            .collect();
        let extra_tags = extra::Tags::load(&mut b)?;
        let missing = std::mem::take(&mut b.missing);
        let lib = Lib { steps, biome_steps, biome_features, info_noise: PerlinSimplexNoise::new(&mut Legacy::new(2345), &[0]), temps, climates, sea_level, tags, leaf_kind, extra_tags };
        Ok((lib, missing))
    }

    pub fn biome_has(&self, biome: u8, feature: &str) -> bool {
        self.biome_features.get(biome as usize).is_some_and(|s| s.contains(feature))
    }

    /// `Biome.BIOME_INFO_NOISE.getValue(x, z, false)`
    pub fn biome_info_noise(&self, x: f64, z: f64) -> f64 {
        self.info_noise.value(x, z)
    }

    /// `applyBiomeDecoration` for `lv.cx, lv.cz`. `structures` places the
    /// structure pieces of a step before its features.
    pub fn decorate(&self, lv: &mut Region, seed: i64, structures: Option<&(dyn Fn(&mut Region, &mut WgRandom, i64, usize) + Sync)>) {
        let (x0, z0) = (lv.cx * 16, lv.cz * 16);
        let mut r = WgRandom::new(0);
        let dec = r.set_decoration_seed(seed, x0, z0);
        let mut present = [false; 256];
        for c in &lv.chunks {
            for sec in &c.biomes {
                for &b in sec {
                    present[b as usize] = true;
                }
            }
        }
        for step in 0..self.steps.len() {
            if let Some(f) = structures {
                f(lv, &mut r, dec, step);
            }
            let mut list: Vec<usize> = Vec::new();
            for (b, _) in present.iter().enumerate().filter(|x| *x.1) {
                if let Some(s) = self.biome_steps.get(b).and_then(|v| v.get(step)) {
                    list.extend(s);
                }
            }
            list.sort_unstable();
            list.dedup();
            for fi in list {
                r.set_feature_seed(dec, fi as i32, step as i32);
                self.steps[step][fi].place_top(lv, &mut r, [x0, MIN_Y, z0]);
            }
        }
    }
}
