//! `StructureTemplate` placement (`placeInWorld`, `processBlockInfos`), the
//! rotation / mirror transforms and the structure processors, ported from
//! the 26.1 bytecode. Block rotation and mirroring use tables dumped from
//! vanilla (`assets/26.1/struct.bin`), so every block turns exactly like
//! vanilla's `BlockState.rotate` / `mirror`.

use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use super::super::feature::place::{block_set, BlockSet, IntP};
use super::super::feature::{Builder as Lib, Hm, Region};
use super::super::random::{block_seed, Legacy, Rng};
use super::{Bb, P};
use crate::registry::{vstate, BlockRegistry, BlockState};

// ------------------------------------------------------------ state tables

struct Tables {
    /// per state: rot cw90, rot 180, rot ccw90, mirror left_right, mirror front_back
    tr: Vec<[u16; 5]>,
    flags: Vec<u8>,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let d: &[u8] = include_bytes!("../../../assets/26.1/struct.bin");
        let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) as usize;
        let mut tr = Vec::with_capacity(n);
        let mut flags = Vec::with_capacity(n);
        for i in 0..n {
            let o = 4 + i * 11;
            let s = |k: usize| u16::from_be_bytes([d[o + k * 2], d[o + k * 2 + 1]]);
            tr.push([s(0), s(1), s(2), s(3), s(4)]);
            flags.push(d[o + 10]);
        }
        Tables { tr, flags }
    })
}

fn flag(s: BlockState, bit: u8) -> bool {
    tables().flags.get(s.0 as usize).is_some_and(|f| f & bit != 0)
}

/// not `hasDynamicShape` and `isCollisionShapeFullBlock` (template block order)
pub fn template_full(s: BlockState) -> bool {
    flag(s, 1)
}
/// `Block.isShapeFullBlock(getShape)`
pub fn shape_full(s: BlockState) -> bool {
    flag(s, 2)
}
/// `SimpleWaterloggedBlock`
pub fn waterloggable(s: BlockState) -> bool {
    flag(s, 4)
}
/// block entity is a `RandomizableContainer`
pub fn randomizable(s: BlockState) -> bool {
    flag(s, 8)
}

/// Rotation ordinals: NONE, CLOCKWISE_90, CLOCKWISE_180, COUNTERCLOCKWISE_90.
pub fn rotate_state(s: BlockState, rot: i32) -> BlockState {
    match rot & 3 {
        0 => s,
        r => BlockState(tables().tr.get(s.0 as usize).map_or(s.0, |t| t[r as usize - 1])),
    }
}

/// Mirror ordinals: NONE, LEFT_RIGHT, FRONT_BACK.
pub fn mirror_state(s: BlockState, m: i32) -> BlockState {
    match m {
        1 => BlockState(tables().tr.get(s.0 as usize).map_or(s.0, |t| t[3])),
        2 => BlockState(tables().tr.get(s.0 as usize).map_or(s.0, |t| t[4])),
        _ => s,
    }
}

/// `Rotation.getRotated(rotation)`
pub fn rot_add(a: i32, b: i32) -> i32 {
    (a + b) & 3
}

// ------------------------------------------------------------ transforms

pub fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// `StructureTemplate.transform(pos, mirror, rotation, pivot)`
pub fn transform(p: P, mirror: i32, rot: i32, pivot: P) -> P {
    let [mut x, y, mut z] = p;
    match mirror {
        1 => z = -z,
        2 => x = -x,
        _ => {}
    }
    let (i, j) = (pivot[0], pivot[2]);
    match rot & 3 {
        1 => [i + j - z, y, j - i + x],
        2 => [i + i - x, y, j + j - z],
        3 => [i - j + z, y, i + j - x],
        _ => [x, y, z],
    }
}

pub fn bb_from_corners(a: P, b: P) -> Bb {
    ([a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])], [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])])
}

pub fn bb_move(b: Bb, d: P) -> Bb {
    (add(b.0, d), add(b.1, d))
}

pub fn bb_inside(b: &Bb, p: P) -> bool {
    (0..3).all(|i| p[i] >= b.0[i] && p[i] <= b.1[i])
}

pub fn bb_intersects(a: &Bb, b: &Bb) -> bool {
    (0..3).all(|i| a.1[i] >= b.0[i] && a.0[i] <= b.1[i])
}

/// `BoundingBox.intersects(minX, minZ, maxX, maxZ)`
pub fn bb_intersects_xz(a: &Bb, x0: i32, z0: i32, x1: i32, z1: i32) -> bool {
    a.1[0] >= x0 && a.0[0] <= x1 && a.1[2] >= z0 && a.0[2] <= z1
}

pub fn bb_encapsulate(a: Bb, b: &Bb) -> Bb {
    ([a.0[0].min(b.0[0]), a.0[1].min(b.0[1]), a.0[2].min(b.0[2])], [a.1[0].max(b.1[0]), a.1[1].max(b.1[1]), a.1[2].max(b.1[2])])
}

pub fn bb_inflate(a: Bb, d: i32) -> Bb {
    (sub(a.0, [d, d, d]), add(a.1, [d, d, d]))
}

pub fn bb_center(a: &Bb) -> P {
    [a.0[0] + (a.1[0] - a.0[0] + 1) / 2, a.0[1] + (a.1[1] - a.0[1] + 1) / 2, a.0[2] + (a.1[2] - a.0[2] + 1) / 2]
}

// ------------------------------------------------------------ templates

#[derive(Clone)]
pub struct BlockInfo {
    pub pos: P,
    pub state: BlockState,
    pub nbt: Option<Arc<Value>>,
}

#[derive(Clone)]
pub struct Jigsaw {
    pub pos: P,
    /// jigsaw block state (orientation = FrontAndTop)
    pub state: BlockState,
    pub name: String,
    pub pool: String,
    pub target: String,
    pub rollable: bool,
    pub placement_priority: i32,
    pub selection_priority: i32,
}

pub struct Template {
    pub name: String,
    pub size: P,
    /// `Palette.blocks()` order: full blocks, other blocks, blocks with nbt,
    /// each sorted by y, x, z
    pub blocks: Vec<BlockInfo>,
    /// the other palettes (`palettes[1..]`, e.g. shipwreck wood types)
    pub alt: Vec<Vec<BlockInfo>>,
    /// indices of jigsaw blocks in `blocks`
    pub jigsaws: Vec<usize>,
    /// data structure blocks (`structure_block` in DATA mode): (pos, metadata)
    pub data_markers: Vec<(P, String)>,
}

fn ints(v: &Value) -> Result<P> {
    let a = v.as_array().ok_or_else(|| anyhow!("bad vector"))?;
    if a.len() != 3 {
        bail!("bad vector");
    }
    Ok([a[0].as_i64().unwrap_or(0) as i32, a[1].as_i64().unwrap_or(0) as i32, a[2].as_i64().unwrap_or(0) as i32])
}

/// `minecraft:block[prop=value,...]`
pub fn parse_state_string(s: &str) -> Option<BlockState> {
    let (name, props) = match s.find('[') {
        Some(i) => (&s[..i], s[i + 1..].trim_end_matches(']')),
        None => (s, ""),
    };
    let name = name.trim_start_matches("minecraft:");
    let pairs: Vec<(&str, &str)> = props.split(',').filter(|p| !p.is_empty()).filter_map(|p| p.split_once('=')).collect();
    BlockRegistry::get().state(name, &pairs)
}

/// Direction ordinals: down, up, north, south, west, east.
pub fn dir_step(d: u8) -> P {
    match d {
        0 => [0, -1, 0],
        1 => [0, 1, 0],
        2 => [0, 0, -1],
        3 => [0, 0, 1],
        4 => [-1, 0, 0],
        _ => [1, 0, 0],
    }
}

pub fn dir_opposite(d: u8) -> u8 {
    d ^ 1
}

pub fn dir_parse(s: &str) -> u8 {
    match s {
        "down" => 0,
        "up" => 1,
        "north" => 2,
        "south" => 3,
        "west" => 4,
        _ => 5,
    }
}

/// `JigsawBlock.getFrontFacing` / `getTopFacing`
pub fn jigsaw_front_top(s: BlockState) -> (u8, u8) {
    let o = BlockRegistry::get().property(s, "orientation").unwrap_or("north_up");
    let (f, t) = o.split_once('_').unwrap_or(("north", "up"));
    (dir_parse(f), dir_parse(t))
}

impl Template {
    pub fn load(lib_load: &dyn Fn(&str) -> Result<Value>, name: &str) -> Result<Template> {
        let key = name.trim_start_matches("minecraft:");
        let v = lib_load(&format!("structure/{key}"))?;
        let size = ints(&v["size"])?;
        let reg = BlockRegistry::get();
        // `StructureTemplate.Palette.blocks()` of one palette
        let build = |pal: &Value| -> Result<Vec<BlockInfo>> {
            let mut states = Vec::new();
            for e in pal.as_array().into_iter().flatten() {
                let n = e["Name"].as_str().unwrap_or("").trim_start_matches("minecraft:");
                let props: Vec<(String, String)> = e["Properties"].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect();
                let pr: Vec<(&str, &str)> = props.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                states.push(reg.state(n, &pr).ok_or_else(|| anyhow!("unknown template block {n} in {key}"))?);
            }
            let (mut full, mut other, mut with_nbt) = (Vec::new(), Vec::new(), Vec::new());
            for e in v["blocks"].as_array().into_iter().flatten() {
                let pos = ints(&e["pos"])?;
                let state = *states.get(e["state"].as_u64().unwrap_or(0) as usize).ok_or_else(|| anyhow!("bad palette index"))?;
                let nbt = e.get("nbt").filter(|n| n.is_object()).map(|n| Arc::new(n.clone()));
                let b = BlockInfo { pos, state, nbt };
                if b.nbt.is_some() {
                    with_nbt.push(b);
                } else if template_full(state) {
                    full.push(b);
                } else {
                    other.push(b);
                }
            }
            let key_of = |b: &BlockInfo| (b.pos[1], b.pos[0], b.pos[2]);
            full.sort_by_key(key_of);
            other.sort_by_key(key_of);
            with_nbt.sort_by_key(key_of);
            full.extend(other);
            full.extend(with_nbt);
            Ok(full)
        };
        let (full, alt) = if v["palette"].is_array() {
            (build(&v["palette"])?, Vec::new())
        } else {
            let pals: Vec<&Value> = v["palettes"].as_array().into_iter().flatten().collect();
            let first = build(pals.first().copied().unwrap_or(&Value::Null))?;
            (first, pals.iter().skip(1).map(|p| build(p)).collect::<Result<Vec<_>>>()?)
        };
        let jigsaws = full.iter().enumerate().filter(|(_, b)| b.state.name() == "jigsaw").map(|(i, _)| i).collect();
        let data_markers = full
            .iter()
            .filter(|b| b.state.name() == "structure_block" && reg.property(b.state, "mode") == Some("data"))
            .map(|b| (b.pos, b.nbt.as_ref().and_then(|n| n["metadata"].as_str()).unwrap_or("").to_string()))
            .collect();
        Ok(Template { name: key.to_string(), size, blocks: full, alt, jigsaws, data_markers })
    }

    /// `getSize(rotation)`
    pub fn rotated_size(&self, rot: i32) -> P {
        if rot & 1 == 1 {
            [self.size[2], self.size[1], self.size[0]]
        } else {
            self.size
        }
    }

    /// `getBoundingBox(pos, rotation, pivot, mirror)`
    pub fn bounding_box(&self, pos: P, rot: i32, pivot: P, mirror: i32) -> Bb {
        let s = sub(self.size, [1, 1, 1]);
        let a = transform([0, 0, 0], mirror, rot, pivot);
        let b = transform(s, mirror, rot, pivot);
        bb_move(bb_from_corners(a, b), pos)
    }

    /// `getJigsaws(pos, rotation)`: world positions, rotated states.
    pub fn jigsaws(&self, pos: P, rot: i32) -> Vec<Jigsaw> {
        self.jigsaws
            .iter()
            .map(|&i| {
                let b = &self.blocks[i];
                let n = b.nbt.as_deref().unwrap_or(&Value::Null);
                let state = rotate_state(b.state, rot);
                let s = |k: &str, d: &str| n[k].as_str().unwrap_or(d).trim_start_matches("minecraft:").to_string();
                let rollable = match n["joint"].as_str() {
                    Some("rollable") => true,
                    Some("aligned") => false,
                    _ => jigsaw_front_top(state).0 < 2,
                };
                Jigsaw {
                    pos: add(transform(b.pos, 0, rot, [0, 0, 0]), pos),
                    state,
                    name: s("name", "empty"),
                    pool: s("pool", "empty"),
                    target: s("target", "empty"),
                    rollable,
                    placement_priority: n["placement_priority"].as_i64().unwrap_or(0) as i32,
                    selection_priority: n["selection_priority"].as_i64().unwrap_or(0) as i32,
                }
            })
            .collect()
    }
}

/// `getZeroPositionWithTransform(pos, mirror, rotation, sizeX, sizeZ)`
pub fn zero_position_with_transform(p: P, mirror: i32, rot: i32, sx: i32, sz: i32) -> P {
    let (i, j) = (sx - 1, sz - 1);
    let k = if mirror == 2 { i } else { 0 };
    let l = if mirror == 1 { j } else { 0 };
    match rot & 3 {
        3 => add(p, [l, 0, i - k]),
        1 => add(p, [j - l, 0, k]),
        2 => add(p, [i - k, 0, j - l]),
        _ => add(p, [k, 0, l]),
    }
}

// ------------------------------------------------------------ processors

#[derive(Clone)]
pub enum RuleTest {
    Always,
    Block(String),
    State(BlockState),
    Tag(BlockSet),
    RandomBlock(String, f32),
    RandomState(BlockState, f32),
}

impl RuleTest {
    fn parse(lib: &mut Lib, v: &Value) -> Result<RuleTest> {
        let ty = v["predicate_type"].as_str().unwrap_or("").trim_start_matches("minecraft:");
        let block = || v["block"].as_str().unwrap_or("").trim_start_matches("minecraft:").to_string();
        Ok(match ty {
            "always_true" => RuleTest::Always,
            "block_match" => RuleTest::Block(block()),
            "blockstate_match" => RuleTest::State(super::super::feature::place::parse_state(&v["block_state"])?),
            "tag_match" => RuleTest::Tag(lib.tag(v["tag"].as_str().unwrap_or(""))?),
            "random_block_match" => RuleTest::RandomBlock(block(), v["probability"].as_f64().unwrap_or(0.0) as f32),
            "random_blockstate_match" => RuleTest::RandomState(super::super::feature::place::parse_state(&v["block_state"])?, v["probability"].as_f64().unwrap_or(0.0) as f32),
            t => bail!("rule test {t}"),
        })
    }

    fn test(&self, s: BlockState, r: &mut dyn Rng) -> bool {
        match self {
            RuleTest::Always => true,
            RuleTest::Block(n) => s.name() == n,
            RuleTest::State(t) => s == *t,
            RuleTest::Tag(t) => t.contains(s),
            RuleTest::RandomBlock(n, p) => s.name() == n && r.next_float() < *p,
            RuleTest::RandomState(t, p) => s == *t && r.next_float() < *p,
        }
    }
}

#[derive(Clone)]
pub enum PosTest {
    Always,
    Linear { min_chance: f32, max_chance: f32, min_dist: i32, max_dist: i32 },
    AxisLinear { min_chance: f32, max_chance: f32, min_dist: i32, max_dist: i32, axis: usize },
}

fn inverse_lerp_f(v: f32, a: f32, b: f32) -> f32 {
    (v - a) / (b - a)
}

fn clamped_lerp_f(t: f32, a: f32, b: f32) -> f32 {
    if t < 0.0 {
        a
    } else if t > 1.0 {
        b
    } else {
        a + t * (b - a)
    }
}

impl PosTest {
    fn parse(v: &Value) -> Result<PosTest> {
        let f = |k: &str, d: f64| v[k].as_f64().unwrap_or(d) as f32;
        let i = |k: &str, d: i64| v[k].as_i64().unwrap_or(d) as i32;
        Ok(match v["predicate_type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
            "always_true" => PosTest::Always,
            "linear_pos" => PosTest::Linear { min_chance: f("min_chance", 0.0), max_chance: f("max_chance", 0.0), min_dist: i("min_dist", 0), max_dist: i("max_dist", 0) },
            "axis_aligned_linear_pos" => PosTest::AxisLinear {
                min_chance: f("min_chance", 0.0),
                max_chance: f("max_chance", 0.0),
                min_dist: i("min_dist", 0),
                max_dist: i("max_dist", 0),
                axis: match v["axis"].as_str().unwrap_or("y") {
                    "x" => 0,
                    "z" => 2,
                    _ => 1,
                },
            },
            t => bail!("pos rule test {t}"),
        })
    }

    fn test(&self, world: P, structure: P, r: &mut dyn Rng) -> bool {
        match *self {
            PosTest::Always => true,
            PosTest::Linear { min_chance, max_chance, min_dist, max_dist } => {
                let d = (world[0] - structure[0]).abs() + (world[1] - structure[1]).abs() + (world[2] - structure[2]).abs();
                let f = r.next_float();
                f <= clamped_lerp_f(inverse_lerp_f(d as f32, min_dist as f32, max_dist as f32), min_chance, max_chance)
            }
            PosTest::AxisLinear { min_chance, max_chance, min_dist, max_dist, axis } => {
                let d = (world[axis] - structure[axis]).abs();
                let f = r.next_float();
                f <= clamped_lerp_f(inverse_lerp_f(d as f32, min_dist as f32, max_dist as f32), min_chance, max_chance)
            }
        }
    }
}

impl Rule {
    /// `new ProcessorRule(input, location, output)`
    pub fn new(input: RuleTest, loc: RuleTest, output: BlockState, adds_nbt: bool) -> Rule {
        Rule { input, loc, pos: PosTest::Always, output, adds_nbt }
    }
}

#[derive(Clone)]
pub struct Rule {
    input: RuleTest,
    loc: RuleTest,
    pos: PosTest,
    output: BlockState,
    /// a block entity modifier that leaves nbt behind (append_loot / append_static)
    adds_nbt: bool,
}

#[derive(Clone)]
pub enum Proc {
    /// `BlockIgnoreProcessor`
    Ignore(Vec<&'static str>),
    JigsawReplace,
    Rule(Vec<Rule>),
    Rot { integrity: f32, rottable: Option<BlockSet> },
    Age { mossiness: f32 },
    Gravity { hm: Hm, offset: i32 },
    Capped { delegate: Box<Proc>, limit: IntP },
    LavaSubmerged,
    Blackstone,
    Protected(BlockSet),
    Nop,
}

impl Proc {
    pub fn parse(lib: &mut Lib, p: &Value) -> Result<Proc> {
        Ok(match p["processor_type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
            "block_rot" => Proc::Rot {
                integrity: p["integrity"].as_f64().unwrap_or(1.0) as f32,
                rottable: match p.get("rottable_blocks") {
                    Some(t) => Some(block_set(lib, t)?),
                    None => None,
                },
            },
            "rule" => {
                let mut rules = Vec::new();
                for r in p["rules"].as_array().into_iter().flatten() {
                    let bem = r["block_entity_modifier"]["type"].as_str().unwrap_or("minecraft:passthrough").trim_start_matches("minecraft:");
                    rules.push(Rule {
                        input: RuleTest::parse(lib, &r["input_predicate"])?,
                        loc: RuleTest::parse(lib, &r["location_predicate"])?,
                        pos: match r.get("position_predicate") {
                            Some(pp) => PosTest::parse(pp)?,
                            None => PosTest::Always,
                        },
                        output: super::super::feature::place::parse_state(&r["output_state"])?,
                        adds_nbt: matches!(bem, "append_loot" | "append_static"),
                    });
                }
                Proc::Rule(rules)
            }
            "block_age" => Proc::Age { mossiness: p["mossiness"].as_f64().unwrap_or(0.0) as f32 },
            "gravity" => Proc::Gravity { hm: Hm::parse(p["heightmap"].as_str().unwrap_or("WORLD_SURFACE_WG")), offset: p["offset"].as_i64().unwrap_or(0) as i32 },
            "capped" => Proc::Capped { delegate: Box::new(Proc::parse(lib, &p["delegate"])?), limit: IntP::parse(&p["limit"])? },
            "lava_submerged_block" => Proc::LavaSubmerged,
            "blackstone_replace" => Proc::Blackstone,
            "protected_blocks" => Proc::Protected(lib.tag(p["value"].as_str().unwrap_or(""))?),
            "block_ignore" => Proc::Ignore(
                p["blocks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b["Name"].as_str())
                    .map(|n| BlockRegistry::get().by_name(n.trim_start_matches("minecraft:")).map(|i| i.name.as_str()).unwrap_or(""))
                    .collect(),
            ),
            "nop" => Proc::Nop,
            t => bail!("structure processor {t}"),
        })
    }
}

/// `StructurePlaceSettings`
#[derive(Clone)]
pub struct Settings {
    pub rot: i32,
    pub mirror: i32,
    pub pivot: P,
    pub bb: Option<Bb>,
    pub processors: Vec<Proc>,
    pub waterlog: bool,
    pub known_shape: bool,
    /// `setRandom`: a fixed seed for all processors; None = `Mth.getSeed(pos)`
    pub random_seed: Option<RandomKind>,
}

#[derive(Clone)]
pub enum RandomKind {
    /// processors share this random (feature placement, e.g. fossils)
    Shared,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { rot: 0, mirror: 0, pivot: [0, 0, 0], bb: None, processors: Vec::new(), waterlog: true, known_shape: false, random_seed: None }
    }
}

/// Per-block random of a processor: `settings.getRandom(pos)`.
enum PRandom<'r> {
    Shared(&'r mut dyn Rng),
    Own(Legacy),
}

fn prandom<'r>(shared: &'r mut Option<&mut dyn Rng>, p: P) -> PRandom<'r> {
    match shared {
        Some(r) => PRandom::Shared(&mut **r),
        None => PRandom::Own(Legacy::new(block_seed(p[0], p[1], p[2]))),
    }
}

impl PRandom<'_> {
    fn get(&mut self) -> &mut dyn Rng {
        match self {
            PRandom::Shared(r) => &mut **r,
            PRandom::Own(l) => l,
        }
    }
}

#[derive(Clone)]
pub struct Info {
    pub pos: P,
    pub state: BlockState,
    pub has_nbt: bool,
}

const STAIRS_NON_MOSSY: [&str; 2] = ["stone_slab", "stone_brick_slab"];

fn age_full(r: &mut dyn Rng, moss: f32) -> Option<BlockState> {
    if r.next_float() >= 0.5 {
        return None;
    }
    let reg = BlockRegistry::get();
    let stairs = |r: &mut dyn Rng, name: &str| -> BlockState {
        const H: [&str; 4] = ["north", "east", "south", "west"];
        let f = H[r.next_int_bound(4) as usize];
        let half = ["top", "bottom"][r.next_int_bound(2) as usize];
        reg.state(name, &[("facing", f), ("half", half)]).expect("stairs")
    };
    // getRandomBlock(random, non-mossy, mossy): both candidate arrays are built first
    let a0 = reg.state("cracked_stone_bricks", &[]).expect("block");
    let a1 = stairs(r, "stone_brick_stairs");
    let b0 = reg.state("mossy_stone_bricks", &[]).expect("block");
    let b1 = stairs(r, "mossy_stone_brick_stairs");
    let mossy = r.next_float() < moss;
    let pick = r.next_int_bound(2);
    Some(if mossy { [b0, b1][pick as usize] } else { [a0, a1][pick as usize] })
}

fn with_props_of(target: &str, from: BlockState) -> Option<BlockState> {
    let reg = BlockRegistry::get();
    let mut s = reg.state(target, &[])?;
    for (k, v) in reg.properties(from) {
        if let Some(n) = reg.with(s, k, v) {
            s = n;
        }
    }
    Some(s)
}

fn is_tagged(name: &str, suffix: &str) -> bool {
    name.ends_with(suffix)
}

fn blackstone(s: BlockState) -> Option<BlockState> {
    let to = match s.name() {
        "cobblestone" | "mossy_cobblestone" => "blackstone",
        "stone" => "polished_blackstone",
        "stone_bricks" | "mossy_stone_bricks" => "polished_blackstone_bricks",
        "cobblestone_stairs" | "mossy_cobblestone_stairs" => "blackstone_stairs",
        "stone_stairs" => "polished_blackstone_stairs",
        "stone_brick_stairs" | "mossy_stone_brick_stairs" => "polished_blackstone_brick_stairs",
        "cobblestone_slab" | "mossy_cobblestone_slab" => "blackstone_slab",
        "smooth_stone_slab" | "stone_slab" => "polished_blackstone_slab",
        "stone_brick_slab" | "mossy_stone_brick_slab" => "polished_blackstone_brick_slab",
        "stone_brick_wall" | "mossy_stone_brick_wall" => "polished_blackstone_brick_wall",
        "cobblestone_wall" | "mossy_cobblestone_wall" => "blackstone_wall",
        "chiseled_stone_bricks" => "chiseled_polished_blackstone",
        "cracked_stone_bricks" => "cracked_polished_blackstone_bricks",
        "iron_bars" => "iron_chain",
        _ => return None,
    };
    let reg = BlockRegistry::get();
    let mut n = reg.state(to, &[])?;
    for k in ["facing", "half", "type"] {
        if let Some(v) = reg.property(s, k) {
            n = reg.with(n, k, v).unwrap_or(n);
        }
    }
    Some(n)
}

pub struct Ctx<'a, 'b> {
    pub lv: &'a mut Region<'b>,
    pub seed: i64,
}

/// One processor on one block. `orig` is the template-local block, `cur`
/// the block as processed so far (world position).
fn process(p: &Proc, lv: &Region, offset: P, pos: P, orig: &BlockInfo, cur: Info, settings: &Settings, shared: &mut Option<&mut dyn Rng>) -> Option<Info> {
    let _ = settings;
    match p {
        Proc::Nop => Some(cur),
        Proc::Ignore(list) => (!list.contains(&cur.state.name())).then_some(cur),
        Proc::JigsawReplace => {
            if cur.state.name() != "jigsaw" {
                return Some(cur);
            }
            let Some(n) = orig.nbt.as_deref() else { return Some(cur) };
            let fs = n["final_state"].as_str().unwrap_or("minecraft:air");
            let s = parse_state_string(fs)?;
            if s.name() == "structure_void" {
                return None;
            }
            Some(Info { pos: cur.pos, state: s, has_nbt: false })
        }
        Proc::Rule(rules) => {
            // RuleProcessor always seeds from the position (not settings.getRandom)
            let mut r = Legacy::new(block_seed(cur.pos[0], cur.pos[1], cur.pos[2]));
            let existing = lv.get(cur.pos);
            for rule in rules {
                if rule.input.test(cur.state, &mut r) && rule.loc.test(existing, &mut r) && rule.pos.test(cur.pos, pos, &mut r) {
                    return Some(Info { pos: cur.pos, state: rule.output, has_nbt: cur.has_nbt || rule.adds_nbt });
                }
            }
            Some(cur)
        }
        Proc::Rot { integrity, rottable } => {
            if let Some(set) = rottable {
                if !set.contains(orig.state) {
                    return Some(cur);
                }
            }
            let mut pr = prandom(shared, cur.pos);
            (pr.get().next_float() <= *integrity).then_some(cur)
        }
        Proc::Age { mossiness } => {
            let mut pr = prandom(shared, cur.pos);
            let r = pr.get();
            let s = cur.state;
            let n = s.name();
            let out = if matches!(n, "stone_bricks" | "stone" | "chiseled_stone_bricks") {
                age_full(r, *mossiness)
            } else if is_tagged(n, "_stairs") {
                if r.next_float() >= 0.5 {
                    None
                } else {
                    let reg = BlockRegistry::get();
                    let mossy = [with_props_of("mossy_stone_brick_stairs", s), reg.state("mossy_stone_brick_slab", &[])];
                    let plain = STAIRS_NON_MOSSY.map(|x| reg.state(x, &[]));
                    let m = r.next_float() < *mossiness;
                    let k = r.next_int_bound(2) as usize;
                    if m { mossy[k] } else { plain[k] }
                }
            } else if is_tagged(n, "_slab") {
                if r.next_float() < *mossiness { with_props_of("mossy_stone_brick_slab", s) } else { None }
            } else if is_tagged(n, "_wall") {
                if r.next_float() < *mossiness { with_props_of("mossy_stone_brick_wall", s) } else { None }
            } else if n == "obsidian" {
                if r.next_float() < 0.15 { BlockRegistry::get().state("crying_obsidian", &[]) } else { None }
            } else {
                None
            };
            Some(match out {
                Some(o) => Info { state: o, ..cur },
                None => cur,
            })
        }
        Proc::Gravity { hm, offset: off } => {
            let h = lv.height(*hm, cur.pos[0], cur.pos[2]) + off;
            Some(Info { pos: [cur.pos[0], h + orig.pos[1], cur.pos[2]], ..cur })
        }
        Proc::LavaSubmerged => {
            if lv.get(cur.pos).name() == "lava" && !shape_full(cur.state) {
                Some(Info { state: BlockRegistry::get().state("lava", &[]).expect("lava"), ..cur })
            } else {
                Some(cur)
            }
        }
        Proc::Blackstone => Some(match blackstone(cur.state) {
            Some(s) => Info { state: s, ..cur },
            None => cur,
        }),
        Proc::Protected(set) => (!set.contains(lv.get(cur.pos))).then_some(cur),
        Proc::Capped { .. } => Some(cur),
    }
    .map(|i| {
        let _ = offset;
        i
    })
}

/// `processBlockInfos`: (template block, processed block) for every block
/// the processors keep.
pub fn process_blocks(lv: &Region, seed: i64, offset: P, pos: P, settings: &Settings, blocks: &[BlockInfo], shared: &mut Option<&mut dyn Rng>) -> Vec<Info> {
    let mut origs: Vec<&BlockInfo> = Vec::new();
    let mut out: Vec<Info> = Vec::new();
    'b: for b in blocks {
        let wp = add(transform(b.pos, settings.mirror, settings.rot, settings.pivot), offset);
        let mut cur = Info { pos: wp, state: b.state, has_nbt: b.nbt.is_some() };
        for p in &settings.processors {
            match process(p, lv, offset, pos, b, cur, settings, shared) {
                Some(c) => cur = c,
                None => continue 'b,
            }
        }
        origs.push(b);
        out.push(cur);
    }
    // finalizeProcessing (only CappedProcessor does anything)
    for p in &settings.processors {
        if let Proc::Capped { delegate, limit } = p {
            if limit.max_value() == 0 || out.is_empty() {
                continue;
            }
            // RandomSource.createThreadLocalInstance(seed).forkPositional().at(offset)
            let mut base = Legacy::new(seed);
            let f = base.fork_positional();
            let mut r = f.at(offset[0], offset[1], offset[2]);
            let n = limit.sample(&mut r).min(out.len() as i32);
            if n < 1 {
                continue;
            }
            let mut idx: Vec<usize> = (0..out.len()).collect();
            for j in (2..=idx.len()).rev() {
                let k = r.next_int_bound(j as i32) as usize;
                idx.swap(j - 1, k);
            }
            let mut done = 0;
            for &i in &idx {
                if done >= n {
                    break;
                }
                let before = out[i].clone();
                if let Some(c) = process(delegate, lv, offset, pos, origs[i], before.clone(), settings, shared) {
                    if c.pos != before.pos || c.state != before.state || c.has_nbt != before.has_nbt {
                        done += 1;
                        out[i] = c;
                    }
                }
            }
        }
    }
    out
}

/// `StructureTemplate.placeInWorld`. `random` is the structure / feature
/// random (loot table seeds draw from it). Returns false if nothing placed.
pub fn place_in_world(lv: &mut Region, seed: i64, t: &Template, offset: P, pos: P, settings: &Settings, random: &mut dyn Rng) -> bool {
    if t.blocks.is_empty() {
        return false;
    }
    let infos = {
        let mut shared: Option<&mut dyn Rng> = if settings.random_seed.is_some() { Some(&mut *random) } else { None };
        // StructurePlaceSettings.getRandomPalette(palettes, offset)
        let blocks = if t.alt.is_empty() {
            &t.blocks
        } else {
            let n = t.alt.len() as i32 + 1;
            let i = match shared.as_mut() {
                Some(r) => r.next_int_bound(n),
                None => Legacy::new(block_seed(offset[0], offset[1], offset[2])).next_int_bound(n),
            };
            if i == 0 { &t.blocks } else { &t.alt[i as usize - 1] }
        };
        process_blocks(lv, seed, offset, pos, settings, blocks, &mut shared)
    };
    let mut flowing: Vec<P> = Vec::new();
    let mut sources: Vec<P> = Vec::new();
    let mut placed: Vec<P> = Vec::new();
    for b in infos {
        if let Some(bb) = &settings.bb {
            if !bb_inside(bb, b.pos) {
                continue;
            }
        }
        let fluid = if settings.waterlog { Some(vstate::fluid(lv.get(b.pos))) } else { None };
        let s = rotate_state(mirror_state(b.state, settings.mirror), settings.rot);
        if !lv.set(b.pos, s) {
            continue;
        }
        placed.push(b.pos);
        if b.has_nbt && randomizable(s) {
            random.next_long();
        }
        if let Some(f) = fluid {
            if vstate::fluid_source(s) {
                sources.push(b.pos);
            } else if waterloggable(s) {
                if f == 1 {
                    if let Some(w) = BlockRegistry::get().with(s, "waterlogged", "true") {
                        if BlockRegistry::get().property(s, "waterlogged") == Some("false") {
                            lv.set(b.pos, w);
                        }
                    }
                }
                if f != 1 && f != 3 {
                    flowing.push(b.pos);
                }
            }
        }
    }
    // spread water into waterloggable blocks next to water sources
    let mut changed = true;
    const DIRS: [P; 5] = [[0, 1, 0], [0, 0, -1], [1, 0, 0], [0, 0, 1], [-1, 0, 0]];
    while changed && !flowing.is_empty() {
        changed = false;
        flowing.retain(|&p| {
            let mut f = vstate::fluid(lv.get(p));
            let src = |f: u8| f == 1 || f == 3;
            let mut i = 0;
            while i < DIRS.len() && !src(f) {
                let q = add(p, DIRS[i]);
                let g = vstate::fluid(lv.get(q));
                if src(g) && !sources.contains(&q) {
                    f = g;
                }
                i += 1;
            }
            if src(f) {
                let s = lv.get(p);
                if waterloggable(s) {
                    if f == 1 && BlockRegistry::get().property(s, "waterlogged") == Some("false") {
                        if let Some(w) = BlockRegistry::get().with(s, "waterlogged", "true") {
                            lv.set(p, w);
                        }
                    }
                    changed = true;
                    return false;
                }
            }
            true
        });
    }
    if !settings.known_shape && !placed.is_empty() {
        update_placed_shapes(lv, &placed);
    }
    true
}

/// `StructureTemplate.placeInWorld` without a known shape:
/// `updateShapeAtEdge` around the placed blocks, then
/// `Block.updateFromNeighbourShapes` for each of them.
fn update_placed_shapes(lv: &mut Region, placed: &[P]) {
    use super::super::feature::{shape_update, update_shape, UPDATE_ORDER};
    let set: std::collections::HashSet<P> = placed.iter().copied().collect();
    let (mut lo, mut hi) = (placed[0], placed[0]);
    for p in placed {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    // DiscreteVoxelShape.forAllFaces: axes x, y, z; each face to the outside
    for (axis, dirs) in [(0usize, [(4u8, [-1, 0, 0]), (5u8, [1, 0, 0])]), (1, [(0, [0, -1, 0]), (1, [0, 1, 0])]), (2, [(2, [0, 0, -1]), (3, [0, 0, 1])])] {
        let _ = axis;
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    let p = [x, y, z];
                    if !set.contains(&p) {
                        continue;
                    }
                    for (d, o) in dirs {
                        let q = add(p, o);
                        if set.contains(&q) {
                            continue;
                        }
                        let a = lv.get(p);
                        let b = lv.get(q);
                        let a2 = update_shape(lv, a, p, d, b);
                        if a2 != a {
                            lv.set(p, a2);
                        }
                        let b2 = update_shape(lv, b, q, d ^ 1, lv.get(p));
                        if b2 != b {
                            lv.set(q, b2);
                        }
                    }
                }
            }
        }
    }
    let _ = UPDATE_ORDER;
    for &p in placed {
        let s = lv.get(p);
        let n = shape_update(lv, s, p);
        if n != s {
            lv.set(p, n);
        }
    }
}
