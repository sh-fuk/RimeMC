//! Non-jigsaw structures built from single templates or hand-placed
//! blocks: shipwrecks, igloos, ocean ruins, ruined portals, buried treasure
//! and swamp huts (ported from the 26.1 bytecode).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::Mutex;
use serde_json::Value;

use super::jigsaw::Pools;
use super::piece::{self, random_horizontal, state, Sp};
use super::template::{self, add, bb_center, bb_encapsulate, bb_from_corners, bb_inside, bb_intersects, place_in_world, randomizable, transform, Proc, Rule, RuleTest, Settings, Template};
use super::{Bb, Ctx, P};
use crate::registry::{blocks, vstate, BlockState};
use crate::worldgen::chunk::{HEIGHT, MIN_Y};
use crate::worldgen::feature::place::{BlockSet, IntP};
use crate::worldgen::feature::{Builder, Hm, Region};
use crate::worldgen::random::Rng;

const MAX_Y: i32 = MIN_Y + HEIGHT - 1;

/// A placed structure piece of a legacy structure.
pub trait LPiece: Send + Sync {
    fn bb(&self) -> Bb;
    fn name(&self) -> String;
    fn post_process(&self, lv: &mut Region, lib: &LLib, seed: i64, r: &mut dyn Rng, cb: &Bb, reference: P);
    /// `Structure.afterPlace` (called once per start and chunk)
    fn after_place(&self, _lv: &mut Region, _cb: &Bb, _all: &[Box<dyn LPiece>]) {}
}

/// Templates and tags the legacy structures need.
#[derive(Default)]
pub struct LLib {
    tpl: HashMap<String, Arc<Template>>,
    fcr: Option<BlockSet>,
    /// `#required_ocean_monument_surrounding` by network biome id
    monument_biomes: Vec<bool>,
}

impl LLib {
    fn get(&self, name: &str) -> Arc<Template> {
        self.tpl.get(name).cloned().unwrap_or_else(|| Arc::new(Template { name: name.into(), size: [0, 0, 0], blocks: Vec::new(), jigsaws: Vec::new(), data_markers: Vec::new() }))
    }
    fn fcr(&self, s: BlockState) -> bool {
        self.fcr.as_ref().is_some_and(|t| t.contains(s))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VPlace {
    OnLand,
    PartlyBuried,
    OnOcean,
    InMountain,
    Underground,
    InNether,
}

#[derive(Clone, Debug)]
pub struct PortalSetup {
    placement: VPlace,
    air_pocket: f32,
    mossiness: f32,
    overgrown: bool,
    vines: bool,
    can_be_cold: bool,
    blackstone: bool,
    weight: f32,
}

#[derive(Clone, Debug)]
pub enum LKind {
    Shipwreck { beached: bool },
    Igloo,
    OceanRuin { warm: bool, large_p: f32, cluster_p: f32 },
    RuinedPortal(Vec<PortalSetup>),
    BuriedTreasure,
    SwampHut,
    DesertPyramid,
    JungleTemple,
    Stronghold,
    OceanMonument,
    WoodlandMansion,
}

impl LKind {
    /// Parses a structure json of a legacy type.
    pub fn parse(ty: &str, v: &Value) -> Option<LKind> {
        Some(match ty {
            "shipwreck" => LKind::Shipwreck { beached: v["is_beached"].as_bool().unwrap_or(false) },
            "igloo" => LKind::Igloo,
            "ocean_ruin" => LKind::OceanRuin {
                warm: v["biome_temp"].as_str() == Some("warm"),
                large_p: v["large_probability"].as_f64().unwrap_or(0.0) as f32,
                cluster_p: v["cluster_probability"].as_f64().unwrap_or(0.0) as f32,
            },
            "ruined_portal" => LKind::RuinedPortal(
                v["setups"]
                    .as_array()?
                    .iter()
                    .map(|s| PortalSetup {
                        placement: match s["placement"].as_str().unwrap_or("") {
                            "partly_buried" => VPlace::PartlyBuried,
                            "on_ocean_floor" => VPlace::OnOcean,
                            "in_mountain" => VPlace::InMountain,
                            "underground" => VPlace::Underground,
                            "in_nether" => VPlace::InNether,
                            _ => VPlace::OnLand,
                        },
                        air_pocket: s["air_pocket_probability"].as_f64().unwrap_or(0.0) as f32,
                        mossiness: s["mossiness"].as_f64().unwrap_or(0.0) as f32,
                        overgrown: s["overgrown"].as_bool().unwrap_or(false),
                        vines: s["vines"].as_bool().unwrap_or(false),
                        can_be_cold: s["can_be_cold"].as_bool().unwrap_or(false),
                        blackstone: s["replace_with_blackstone"].as_bool().unwrap_or(false),
                        weight: s["weight"].as_f64().unwrap_or(1.0) as f32,
                    })
                    .collect(),
            ),
            "buried_treasure" => LKind::BuriedTreasure,
            "swamp_hut" => LKind::SwampHut,
            "desert_pyramid" => LKind::DesertPyramid,
            "jungle_temple" => LKind::JungleTemple,
            "stronghold" => LKind::Stronghold,
            "ocean_monument" => LKind::OceanMonument,
            "woodland_mansion" => LKind::WoodlandMansion,
            _ => return None,
        })
    }

    /// Loads the templates of this structure into `lib`.
    pub fn load(&self, pools: &mut Pools, b: &mut Builder, lib: &mut LLib) -> Result<()> {
        let names: Vec<String> = match self {
            LKind::Shipwreck { .. } => SHIP_OCEAN.iter().map(|n| format!("shipwreck/{n}")).collect(),
            LKind::Igloo => IGLOO.iter().map(|n| format!("igloo/{n}")).collect(),
            LKind::OceanRuin { .. } => {
                let mut v = Vec::new();
                for k in ["warm", "brick", "cracked", "mossy"] {
                    for i in 1..=8 {
                        v.push(format!("underwater_ruin/{k}_{i}"));
                    }
                }
                for k in ["brick", "cracked", "mossy"] {
                    for i in [1, 2, 3, 8] {
                        v.push(format!("underwater_ruin/big_{k}_{i}"));
                    }
                }
                for i in 4..=7 {
                    v.push(format!("underwater_ruin/big_warm_{i}"));
                }
                v
            }
            LKind::RuinedPortal(_) => {
                if lib.fcr.is_none() {
                    lib.fcr = Some(b.tag("features_cannot_replace")?);
                }
                (1..=10).map(|i| format!("ruined_portal/portal_{i}")).chain((1..=3).map(|i| format!("ruined_portal/giant_portal_{i}"))).collect()
            }
            LKind::WoodlandMansion => MANSION.split(' ').map(|n| format!("woodland_mansion/{n}")).collect(),
            LKind::OceanMonument => {
                let mut set = std::collections::HashSet::new();
                super::biome_tag(b, "required_ocean_monument_surrounding", &mut set, &mut std::collections::HashSet::new())?;
                let names = crate::world::chunk::Biomes::get();
                lib.monument_biomes = vec![false; 256];
                for n in &set {
                    if let Some(id) = names.id(n) {
                        lib.monument_biomes[id as usize] = true;
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        };
        for n in names {
            if !lib.tpl.contains_key(&n) {
                let t = pools.template(b, &format!("minecraft:{n}"))?;
                lib.tpl.insert(n, t);
            }
        }
        Ok(())
    }
}

const SHIP_BEACHED: [&str; 11] = [
    "with_mast",
    "sideways_full",
    "sideways_fronthalf",
    "sideways_backhalf",
    "rightsideup_full",
    "rightsideup_fronthalf",
    "rightsideup_backhalf",
    "with_mast_degraded",
    "rightsideup_full_degraded",
    "rightsideup_fronthalf_degraded",
    "rightsideup_backhalf_degraded",
];
const SHIP_OCEAN: [&str; 20] = [
    "with_mast",
    "upsidedown_full",
    "upsidedown_fronthalf",
    "upsidedown_backhalf",
    "sideways_full",
    "sideways_fronthalf",
    "sideways_backhalf",
    "rightsideup_full",
    "rightsideup_fronthalf",
    "rightsideup_backhalf",
    "with_mast_degraded",
    "upsidedown_full_degraded",
    "upsidedown_fronthalf_degraded",
    "upsidedown_backhalf_degraded",
    "sideways_full_degraded",
    "sideways_fronthalf_degraded",
    "sideways_backhalf_degraded",
    "rightsideup_full_degraded",
    "rightsideup_fronthalf_degraded",
    "rightsideup_backhalf_degraded",
];
/// top, middle, bottom
const IGLOO: [&str; 3] = ["top", "middle", "bottom"];

const MANSION: &str = "1x1_a1 1x1_a2 1x1_a3 1x1_a4 1x1_a5 1x1_as1 1x1_as2 1x1_as3 1x1_as4 1x1_b1 1x1_b2 1x1_b3 1x1_b4 1x1_b5 1x2_a1 1x2_a2 1x2_a3 1x2_a4 1x2_a5 1x2_a6 1x2_a7 1x2_a8 1x2_a9 1x2_b1 1x2_b2 1x2_b3 1x2_b4 1x2_b5 1x2_c1 1x2_c2 1x2_c3 1x2_c4 1x2_c_stairs 1x2_d1 1x2_d2 1x2_d3 1x2_d4 1x2_d5 1x2_d_stairs 1x2_s1 1x2_s2 1x2_se1 2x2_a1 2x2_a2 2x2_a3 2x2_a4 2x2_b1 2x2_b2 2x2_b3 2x2_b4 2x2_b5 2x2_s1 carpet_east carpet_north carpet_south_1 carpet_south_2 carpet_west_1 carpet_west_2 corridor_floor entrance indoors_door_1 indoors_door_2 indoors_wall_1 indoors_wall_2 roof roof_corner roof_front roof_inner_corner small_wall small_wall_corner wall_corner wall_flat wall_window";
const IGLOO_PIVOT: [P; 3] = [[3, 5, 5], [1, 3, 1], [3, 6, 7]];
const IGLOO_OFFSET: [P; 3] = [[0, 0, 0], [2, -3, 4], [0, -3, -2]];

// ------------------------------------------------------------ random helpers

/// `Mth.nextInt(random, a, b)`
fn mth_next_int(r: &mut dyn Rng, a: i32, b: i32) -> i32 {
    if a >= b {
        a
    } else {
        r.next_int_bound(b - a + 1) + a
    }
}

/// `Mth.randomBetweenInclusive`
fn rbi(r: &mut dyn Rng, a: i32, b: i32) -> i32 {
    r.next_int_bound(b - a + 1) + a
}

fn first_occupied(ctx: &dyn Ctx, x: i32, z: i32, hm: Hm) -> i32 {
    ctx.base_height(x, z, hm) - 1
}

/// `Structure.onTopOfChunkCenter`
fn chunk_center(ctx: &dyn Ctx, cx: i32, cz: i32, hm: Hm) -> P {
    let (x, z) = (cx * 16 + 8, cz * 16 + 8);
    [x, first_occupied(ctx, x, z, hm), z]
}

/// `Structure.getCornerHeights(ctx, x, dx, z, dz)`
fn corners(ctx: &dyn Ctx, x: i32, dx: i32, z: i32, dz: i32) -> [i32; 4] {
    let h = |x, z| first_occupied(ctx, x, z, Hm::NotAir);
    [h(x, z), h(x, z + dz), h(x + dx, z), h(x + dx, z + dz)]
}

/// `Structure.getLowestY(ctx, x, z, w, d)`
pub fn lowest_y(ctx: &dyn Ctx, x: i32, z: i32, w: i32, d: i32) -> i32 {
    let c = corners(ctx, x, w, z, d);
    c[0].min(c[1]).min(c[2].min(c[3]))
}

/// `Structure.getMeanFirstOccupiedHeight(ctx, x, dx, z, dz)`
fn mean_height(ctx: &dyn Ctx, x: i32, dx: i32, z: i32, dz: i32) -> i32 {
    let c = corners(ctx, x, dx, z, dz);
    (c[0] + c[1] + c[2] + c[3]) / 4
}

fn span(b: &Bb, i: usize) -> i32 {
    b.1[i] - b.0[i] + 1
}

// ------------------------------------------------------------ template pieces

#[derive(Clone, Copy)]
enum TKind {
    Shipwreck { beached: bool },
    Igloo(usize),
    OceanRuin,
    Portal { placement: VPlace, cold: bool, overgrown: bool, vines: bool },
    Mansion,
}

struct TState {
    pos: P,
    bb: Bb,
    adjusted: bool,
}

/// `TemplateStructurePiece`
pub struct TPiece {
    t: Arc<Template>,
    rot: i32,
    mirror: i32,
    pivot: P,
    procs: Vec<Proc>,
    waterlog: bool,
    kind: TKind,
    st: Mutex<TState>,
}

impl TPiece {
    #[allow(clippy::too_many_arguments)]
    fn new(t: Arc<Template>, pos: P, rot: i32, mirror: i32, pivot: P, procs: Vec<Proc>, waterlog: bool, kind: TKind) -> TPiece {
        let bb = t.bounding_box(pos, rot, pivot, mirror);
        TPiece { t, rot, mirror, pivot, procs, waterlog, kind, st: Mutex::new(TState { pos, bb, adjusted: false }) }
    }

    /// `TemplateStructurePiece.postProcess` with the template at `pos`;
    /// returns the new bounding box.
    #[allow(clippy::too_many_arguments)]
    fn place(&self, lv: &mut Region, seed: i64, r: &mut dyn Rng, cb: &Bb, reference: P, pos: P) -> Bb {
        let settings = Settings { rot: self.rot, mirror: self.mirror, pivot: self.pivot, bb: Some(*cb), processors: self.procs.clone(), waterlog: self.waterlog, known_shape: false, random_seed: None };
        let bb = self.t.bounding_box(pos, self.rot, self.pivot, self.mirror);
        if place_in_world(lv, seed, &self.t, pos, reference, &settings, r) {
            for (lp, meta) in &self.t.data_markers {
                let wp = add(transform(*lp, self.mirror, self.rot, self.pivot), pos);
                self.marker(lv, r, meta, wp, cb);
            }
        }
        bb
    }

    fn marker(&self, lv: &mut Region, r: &mut dyn Rng, meta: &str, p: P, cb: &Bb) {
        let below = [p[0], p[1] - 1, p[2]];
        match self.kind {
            TKind::Shipwreck { .. } => {
                if matches!(meta, "map_chest" | "treasure_chest" | "supply_chest") && randomizable(lv.get(below)) {
                    r.next_long();
                }
            }
            TKind::Igloo(_) => {
                if meta == "chest" {
                    lv.set(p, blocks::AIR);
                    if lv.get(below).name() == "chest" {
                        r.next_long();
                    }
                }
            }
            TKind::OceanRuin => {
                if meta == "chest" {
                    let water = vstate::is_water(lv.get(p));
                    let s = state("chest", &[("waterlogged", if water { "true" } else { "false" })]);
                    if lv.set(p, s) {
                        r.next_long();
                    }
                } else if meta == "drowned" {
                    lv.set(p, if p[1] > lv.lib.sea_level { blocks::AIR } else { state("water", &[]) });
                }
            }
            TKind::Portal { .. } => {}
            TKind::Mansion => {
                if let Some(face) = meta.strip_prefix("Chest") {
                    let d = match face {
                        "West" => Some("west"),
                        "East" => Some("east"),
                        "South" => Some("south"),
                        "North" => Some("north"),
                        _ => None,
                    };
                    let mut s = state("chest", &[]);
                    if let Some(d) = d {
                        s = template::rotate_state(state("chest", &[("facing", d)]), self.rot);
                    }
                    piece::create_chest(lv, cb, r, p, Some(s));
                } else {
                    // mobs are spawned only once, by the chunk holding the marker
                    if !bb_inside(cb, p) {
                        return;
                    }
                    let mobs: &[&'static str] = match meta {
                        "Mage" => &["evoker"],
                        "Warrior" => &["vindicator"],
                        "Group of Allays" => {
                            let n = (p[0] ^ p[1] ^ p[2]).rem_euclid(3) + 1;
                            &["allay", "allay", "allay"][..n as usize]
                        }
                        _ => &[],
                    };
                    for m in mobs {
                        lv.spawn_entity(m, p);
                        lv.set(p, blocks::AIR);
                    }
                }
            }
        }
    }
}

impl LPiece for TPiece {
    fn bb(&self) -> Bb {
        self.st.lock().bb
    }

    /// `WoodlandMansionStructure.afterPlace`: cobblestone under the floor
    fn after_place(&self, lv: &mut Region, cb: &Bb, all: &[Box<dyn LPiece>]) {
        if !matches!(self.kind, TKind::Mansion) {
            return;
        }
        let bbs: Vec<Bb> = all.iter().map(|p| p.bb()).collect();
        let mut bb = bbs[0];
        for b in &bbs[1..] {
            bb = bb_encapsulate(bb, b);
        }
        let y0 = bb.0[1];
        let cobble = state("cobblestone", &[]);
        for x in bb.0[0]..=bb.1[0] {
            for z in bb.0[2]..=bb.1[2] {
                let p = [x, y0, z];
                if lv.is_empty(p) || !bb_inside(cb, p) || !bbs.iter().any(|b| bb_inside(b, p)) {
                    continue;
                }
                let mut y = y0 - 1;
                while y > MIN_Y {
                    let q = [x, y, z];
                    if !lv.is_empty(q) && !vstate::liquid(lv.get(q)) {
                        break;
                    }
                    lv.set(q, cobble);
                    y -= 1;
                }
            }
        }
    }

    fn name(&self) -> String {
        self.t.name.clone()
    }

    fn post_process(&self, lv: &mut Region, lib: &LLib, seed: i64, r: &mut dyn Rng, cb: &Bb, reference: P) {
        let mut st = self.st.lock();
        let size = self.t.size;
        match self.kind {
            TKind::Shipwreck { beached } => {
                let too_big = size[0] > 32 || size[1] > 32;
                if !st.adjusted && !too_big {
                    let hm = if beached { Hm::NotAir } else { Hm::Motion };
                    let tp = st.pos;
                    let mut i = MAX_Y + 1;
                    let mut j = 0;
                    let k = size[0] * size[2];
                    if k == 0 {
                        j = lv.height(hm, tp[0], tp[2]);
                    } else {
                        for z in tp[2]..=tp[2] + size[2] - 1 {
                            for x in tp[0]..=tp[0] + size[0] - 1 {
                                let h = lv.height(hm, x, z);
                                j += h;
                                i = i.min(h);
                            }
                        }
                        j /= k;
                    }
                    let y = if beached { i - size[1] / 2 - r.next_int_bound(3) } else { j };
                    st.adjusted = true;
                    st.pos[1] = y;
                }
                st.bb = self.place(lv, seed, r, cb, reference, st.pos);
            }
            TKind::Igloo(id) => {
                let off = IGLOO_OFFSET[id];
                let bp = add(st.pos, transform([3 - off[0], 0, -off[2]], 0, self.rot, self.pivot));
                let h = lv.height(Hm::NotAir, bp[0], bp[2]);
                let pos = [st.pos[0], st.pos[1] + h - 90 - 1, st.pos[2]];
                st.bb = self.place(lv, seed, r, cb, reference, pos);
                if id == 0 {
                    let p = add(pos, transform([3, 0, 5], 0, self.rot, self.pivot));
                    let below = lv.get([p[0], p[1] - 1, p[2]]);
                    if !vstate::air(below) && below.name() != "ladder" {
                        lv.set(p, state("snow_block", &[]));
                    }
                }
            }
            TKind::OceanRuin => {
                let tp = st.pos;
                let h = lv.height(Hm::Motion, tp[0], tp[2]);
                let mut tp = [tp[0], h, tp[2]];
                let b = add(transform([size[0] - 1, 0, size[2] - 1], 0, self.rot, [0, 0, 0]), tp);
                tp[1] = ruin_height(lv, tp, b);
                st.pos = tp;
                st.bb = self.place(lv, seed, r, cb, reference, tp);
            }
            TKind::Mansion => {
                let pos = st.pos;
                st.bb = self.place(lv, seed, r, cb, reference, pos);
            }
            TKind::Portal { placement, cold, overgrown, vines } => {
                let tbb = self.t.bounding_box(st.pos, self.rot, self.pivot, self.mirror);
                if !bb_inside(cb, bb_center(&tbb)) {
                    return;
                }
                let area = bb_encapsulate(*cb, &tbb);
                st.bb = self.place(lv, seed, r, &area, reference, st.pos);
                let bb = st.bb;
                drop(st);
                let mut pp = Portal { lv, lib, placement, cold, overgrown, r };
                pp.spread_netherrack(&bb);
                pp.drip_columns_below(&bb);
                if vines || overgrown {
                    for z in bb.0[2]..=bb.1[2] {
                        for y in bb.0[1]..=bb.1[1] {
                            for x in bb.0[0]..=bb.1[0] {
                                if vines {
                                    pp.maybe_vines([x, y, z]);
                                }
                                if overgrown {
                                    pp.maybe_leaves_above([x, y, z]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn is_ice(s: BlockState) -> bool {
    matches!(s.name(), "ice" | "packed_ice" | "blue_ice" | "frosted_ice")
}

/// `OceanRuinPiece.getHeight(pos, level, pos2)`
fn ruin_height(lv: &Region, a: P, b: P) -> i32 {
    let mut i = a[1];
    let mut j = 512;
    let k = i - 1;
    let mut l = 0;
    for z in a[2].min(b[2])..=a[2].max(b[2]) {
        for x in a[0].min(b[0])..=a[0].max(b[0]) {
            let mut y = a[1] - 1;
            loop {
                let s = lv.get([x, y, z]);
                if !(vstate::air(s) || vstate::is_water(s) || is_ice(s)) || y <= MIN_Y + 1 {
                    break;
                }
                y -= 1;
            }
            j = j.min(y);
            if y < k - 2 {
                l += 1;
            }
        }
    }
    let m = (a[0] - b[0]).abs();
    if k - j > 2 && l > m - 2 {
        i = j + 1;
    }
    i
}

struct Portal<'a, 'b, 'c> {
    lv: &'a mut Region<'b>,
    lib: &'a LLib,
    placement: VPlace,
    cold: bool,
    overgrown: bool,
    r: &'c mut dyn Rng,
}

impl Portal<'_, '_, '_> {
    fn hm(&self) -> Hm {
        if self.placement == VPlace::OnOcean {
            Hm::Motion
        } else {
            Hm::NotAir
        }
    }

    fn can_replace(&self, p: P) -> bool {
        let s = self.lv.get(p);
        s.name() != "air" && s.name() != "obsidian" && !self.lib.fcr(s) && (self.placement == VPlace::InNether || s.name() != "lava")
    }

    fn netherrack_or_magma(&mut self, p: P) {
        let s = if !self.cold && self.r.next_float() < 0.07 { state("magma_block", &[]) } else { state("netherrack", &[]) };
        self.lv.set(p, s);
    }

    fn drip_column(&mut self, mut p: P) {
        self.netherrack_or_magma(p);
        let mut i = 8;
        while i > 0 && self.r.next_float() < 0.5 {
            p[1] -= 1;
            i -= 1;
            self.netherrack_or_magma(p);
        }
    }

    fn spread_netherrack(&mut self, bb: &Bb) {
        const CHANCE: [f32; 14] = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.9, 0.9, 0.8, 0.7, 0.6, 0.4, 0.2];
        let flag = matches!(self.placement, VPlace::OnLand | VPlace::OnOcean);
        let c = bb_center(bb);
        let (cx, cz) = (c[0], c[2]);
        let i9 = (span(bb, 0) + span(bb, 2)) / 2;
        let i10 = self.r.next_int_bound(1.max(8 - i9 / 2));
        let min_y = bb.0[1];
        let hm = self.hm();
        for x in cx - 14..=cx + 14 {
            for z in cz - 14..=cz + 14 {
                let d = (x - cx).abs() + (z - cz).abs();
                let i16 = 0.max(d + i10);
                if i16 >= 14 {
                    continue;
                }
                if self.r.next_double() < CHANCE[i16 as usize] as f64 {
                    let y18 = self.lv.height(hm, x, z) - 1;
                    let y19 = if flag { y18 } else { min_y.min(y18) };
                    let p = [x, y19, z];
                    if (y19 - min_y).abs() <= 3 && self.can_replace(p) {
                        self.netherrack_or_magma(p);
                        if self.overgrown {
                            self.maybe_leaves_above(p);
                        }
                        self.drip_column([x, y19 - 1, z]);
                    }
                }
            }
        }
    }

    fn drip_columns_below(&mut self, bb: &Bb) {
        for x in bb.0[0] + 1..bb.1[0] {
            for z in bb.0[2] + 1..bb.1[2] {
                if self.lv.get([x, bb.0[1], z]).name() == "netherrack" {
                    self.drip_column([x, bb.0[1] - 1, z]);
                }
            }
        }
    }

    fn maybe_leaves_above(&mut self, p: P) {
        if self.r.next_float() < 0.5 && self.lv.get(p).name() == "netherrack" {
            let up = [p[0], p[1] + 1, p[2]];
            if vstate::air(self.lv.get(up)) {
                self.lv.set(up, state("jungle_leaves", &[("persistent", "true")]));
            }
        }
    }

    fn maybe_vines(&mut self, p: P) {
        let s = self.lv.get(p);
        if vstate::air(s) || s.name() == "vine" {
            return;
        }
        let d = random_horizontal(self.r);
        let q = add(p, piece::step(d));
        if !vstate::air(self.lv.get(q)) {
            return;
        }
        if !vstate::sturdy(s, d) {
            return;
        }
        self.lv.set(q, state("vine", &[(piece::dir_name(piece::opposite(d)), "true")]));
    }
}

// ------------------------------------------------------------ buried treasure

struct Treasure {
    bb: Mutex<Bb>,
}

impl LPiece for Treasure {
    fn bb(&self) -> Bb {
        *self.bb.lock()
    }
    fn name(&self) -> String {
        "buried_treasure".into()
    }
    fn post_process(&self, lv: &mut Region, _lib: &LLib, _seed: i64, r: &mut dyn Rng, cb: &Bb, _reference: P) {
        let mut bb = self.bb.lock();
        let (x, z) = (bb.0[0], bb.0[2]);
        let mut p = [x, lv.height(Hm::Motion, x, z), z];
        let water = state("water", &[]);
        let lava = state("lava", &[]);
        let liquid = |s: BlockState| s == water || s == lava;
        while p[1] > MIN_Y {
            let s = lv.get(p);
            let below = lv.get([p[0], p[1] - 1, p[2]]);
            if matches!(below.name(), "sandstone" | "stone" | "andesite" | "granite" | "diorite") {
                let s2 = if vstate::air(s) || liquid(s) { state("sand", &[]) } else { s };
                for d in 0..6u8 {
                    let q = add(p, piece::step(d));
                    let n = lv.get(q);
                    if vstate::air(n) || liquid(n) {
                        let b = lv.get([q[0], q[1] - 1, q[2]]);
                        if (vstate::air(b) || liquid(b)) && d != 1 {
                            lv.set(q, below);
                        } else {
                            lv.set(q, s2);
                        }
                    }
                }
                *bb = (p, p);
                piece::create_chest(lv, cb, r, p, None);
                return;
            }
            p[1] -= 1;
        }
    }
}

// ------------------------------------------------------------ swamp hut

/// `ScatteredFeaturePiece` state
pub struct Scattered {
    pub orient: u8,
    pub w: i32,
    pub h: i32,
    pub d: i32,
    /// (box, heightPosition)
    pub st: Mutex<(Bb, i32)>,
}

impl Scattered {
    pub fn new(x: i32, y: i32, z: i32, dir: u8, w: i32, h: i32, d: i32) -> Scattered {
        Scattered { orient: dir, w, h, d, st: Mutex::new((piece::make_bb(x, y, z, dir, w, h, d), -1)) }
    }

    /// `updateAverageGroundHeight`; returns the box to build in
    pub fn average_ground(&self, lv: &Region, cb: &Bb, off: i32) -> Option<Bb> {
        let mut st = self.st.lock();
        if st.1 >= 0 {
            return Some(st.0);
        }
        let (mut sum, mut n) = (0, 0);
        let b = st.0;
        for z in b.0[2]..=b.1[2] {
            for x in b.0[0]..=b.1[0] {
                if bb_inside(cb, [x, 64, z]) {
                    sum += lv.height(Hm::NoLeaves, x, z);
                    n += 1;
                }
            }
        }
        if n == 0 {
            return None;
        }
        st.1 = sum / n;
        let dy = st.1 - b.0[1] + off;
        st.0 = template::bb_move(b, [0, dy, 0]);
        Some(st.0)
    }

    /// `updateAverageGroundHeight` on a placement (updates its box)
    pub fn avg_ground(&self, sp: &mut Sp, off: i32) -> bool {
        match self.average_ground(sp.lv, &sp.cb, off) {
            Some(b) => {
                sp.bb = b;
                true
            }
            None => false,
        }
    }

    /// `updateHeightPositionToLowestGroundHeight` on a placement
    pub fn lowest_ground(&self, sp: &mut Sp, off: i32) {
        sp.bb = self.lowest(sp.lv, off);
    }

    /// `updateHeightPositionToLowestGroundHeight`
    pub fn lowest(&self, lv: &Region, off: i32) -> Bb {
        let mut st = self.st.lock();
        if st.1 >= 0 {
            return st.0;
        }
        let b = st.0;
        let mut min = MAX_Y + 1;
        for z in b.0[2]..=b.1[2] {
            for x in b.0[0]..=b.1[0] {
                min = min.min(lv.height(Hm::NoLeaves, x, z));
            }
        }
        st.1 = min;
        st.0 = template::bb_move(b, [0, min - b.0[1] + off, 0]);
        st.0
    }
}

struct SwampHut(Scattered);

impl LPiece for SwampHut {
    fn bb(&self) -> Bb {
        self.0.st.lock().0
    }
    fn name(&self) -> String {
        "swamp_hut".into()
    }
    fn post_process(&self, lv: &mut Region, _lib: &LLib, _seed: i64, _r: &mut dyn Rng, cb: &Bb, _reference: P) {
        let Some(bb) = self.0.average_ground(lv, cb, 0) else { return };
        let mut sp = Sp { lv, cb: *cb, bb, orient: Some(self.0.orient) };
        let planks = state("spruce_planks", &[]);
        let log = state("oak_log", &[]);
        let fence = state("oak_fence", &[]);
        for b in [[1, 1, 1, 5, 1, 7], [1, 4, 2, 5, 4, 7], [2, 1, 0, 4, 1, 0], [2, 2, 2, 3, 3, 2], [1, 2, 3, 1, 3, 6], [5, 2, 3, 5, 3, 6], [2, 2, 7, 4, 3, 7]] {
            sp.gen_box(b[0], b[1], b[2], b[3], b[4], b[5], planks, planks, false);
        }
        for b in [[1, 0, 2, 1, 3, 2], [5, 0, 2, 5, 3, 2], [1, 0, 7, 1, 3, 7], [5, 0, 7, 5, 3, 7]] {
            sp.gen_box(b[0], b[1], b[2], b[3], b[4], b[5], log, log, false);
        }
        sp.place(fence, 2, 3, 2);
        sp.place(fence, 3, 3, 7);
        sp.place(blocks::AIR, 1, 3, 4);
        sp.place(blocks::AIR, 5, 3, 4);
        sp.place(blocks::AIR, 5, 3, 5);
        sp.place(state("potted_red_mushroom", &[]), 1, 3, 5);
        sp.place(state("crafting_table", &[]), 3, 2, 6);
        sp.place(state("cauldron", &[]), 4, 2, 6);
        sp.place(fence, 1, 2, 1);
        sp.place(fence, 5, 2, 1);
        let st = |f: &str| state("spruce_stairs", &[("facing", f)]);
        let (n, e, w, s) = (st("north"), st("east"), st("west"), st("south"));
        sp.gen_box(0, 4, 1, 6, 4, 1, n, n, false);
        sp.gen_box(0, 4, 2, 0, 4, 7, e, e, false);
        sp.gen_box(6, 4, 2, 6, 4, 7, w, w, false);
        sp.gen_box(0, 4, 8, 6, 4, 8, s, s, false);
        sp.place(piece::with(n, "shape", "outer_right"), 0, 4, 1);
        sp.place(piece::with(n, "shape", "outer_left"), 6, 4, 1);
        sp.place(piece::with(s, "shape", "outer_left"), 0, 4, 8);
        sp.place(piece::with(s, "shape", "outer_right"), 6, 4, 8);
        for z in [2, 7] {
            for x in [1, 5] {
                sp.fill_column_down(log, x, -1, z);
            }
        }
    }
}

// ------------------------------------------------------------ generation

/// `Structure.findGenerationPoint` + `generatePieces` of a legacy
/// structure: the pieces and the stub position (checked against the biome).
pub fn generate(kind: &LKind, lib: &LLib, ctx: &dyn Ctx, seed: i64, cx: i32, cz: i32, r: &mut dyn Rng) -> Option<(Vec<Box<dyn LPiece>>, P)> {
    let (x0, z0) = (cx * 16, cz * 16);
    let mut out: Vec<Box<dyn LPiece>> = Vec::new();
    match kind {
        LKind::Shipwreck { beached } => {
            let stub = chunk_center(ctx, cx, cz, if *beached { Hm::NotAir } else { Hm::Motion });
            let rot = r.next_int_bound(4);
            let pos = [x0, 90, z0];
            let name = if *beached { SHIP_BEACHED[r.next_int_bound(11) as usize] } else { SHIP_OCEAN[r.next_int_bound(20) as usize] };
            let t = lib.get(&format!("shipwreck/{name}"));
            let p = TPiece::new(t.clone(), pos, rot, 0, [4, 0, 15], vec![Proc::Ignore(vec!["structure_block", "air"])], true, TKind::Shipwreck { beached: *beached });
            if t.size[0] > 32 || t.size[1] > 32 {
                let bb = p.bb();
                let y = if *beached {
                    // vanilla passes (minX, xSpan, minZ, zSpan) to getLowestY(x, z, w, d)
                    let j = lowest_y(ctx, bb.0[0], span(&bb, 0), bb.0[2], span(&bb, 2));
                    j - t.size[1] / 2 - r.next_int_bound(3)
                } else {
                    mean_height(ctx, bb.0[0], span(&bb, 0), bb.0[2], span(&bb, 2))
                };
                let mut st = p.st.lock();
                st.adjusted = true;
                st.pos[1] = y;
            }
            out.push(Box::new(p));
            Some((out, stub))
        }
        LKind::Igloo => {
            let stub = chunk_center(ctx, cx, cz, Hm::NotAir);
            let pos = [x0, 90, z0];
            let rot = r.next_int_bound(4);
            let add_piece = |out: &mut Vec<Box<dyn LPiece>>, id: usize, down: i32| {
                let o = IGLOO_OFFSET[id];
                let p = [pos[0] + o[0], pos[1] + o[1] - down, pos[2] + o[2]];
                let t = lib.get(&format!("igloo/{}", IGLOO[id]));
                out.push(Box::new(TPiece::new(t, p, rot, 0, IGLOO_PIVOT[id], vec![Proc::Ignore(vec!["structure_block"])], false, TKind::Igloo(id))));
            };
            if r.next_double() < 0.5 {
                let i = r.next_int_bound(8) + 4;
                add_piece(&mut out, 2, i * 3);
                for j in 0..i - 1 {
                    add_piece(&mut out, 1, j * 3);
                }
            }
            add_piece(&mut out, 0, 0);
            Some((out, stub))
        }
        LKind::OceanRuin { warm, large_p, cluster_p } => {
            let stub = chunk_center(ctx, cx, cz, Hm::Motion);
            let pos = [x0, 90, z0];
            let rot = r.next_int_bound(4);
            let large = r.next_float() <= *large_p;
            let integrity = if large { 0.9 } else { 0.8 };
            ruin_piece(lib, &mut out, r, *warm, pos, rot, large, integrity);
            if large && r.next_float() <= *cluster_p {
                let p6 = [pos[0], 90, pos[2]];
                let p7 = add(transform([15, 0, 15], 0, rot, [0, 0, 0]), p6);
                let bb8 = bb_from_corners(p6, p7);
                let p9 = [p6[0].min(p7[0]), p6[1], p6[2].min(p7[2])];
                let mut list: Vec<P> = Vec::with_capacity(8);
                let offs: [(i32, i32, i32, i32, i32, i32); 8] =
                    [(-16, 1, 8, 16, 1, 7), (-16, 1, 8, 0, 1, 7), (-16, 1, 8, -16, 4, 8), (0, 1, 7, 16, 1, 7), (0, 1, 7, -16, 4, 6), (16, 1, 7, 16, 3, 8), (16, 1, 7, 0, 1, 7), (16, 1, 7, -16, 4, 8)];
                for (bx, xa, xb, bz, za, zb) in offs {
                    let dx = bx + mth_next_int(r, xa, xb);
                    let dz = bz + mth_next_int(r, za, zb);
                    list.push([p9[0] + dx, p9[1], p9[2] + dz]);
                }
                let count = mth_next_int(r, 4, 8);
                for _ in 0..count {
                    if list.is_empty() {
                        continue;
                    }
                    let i = r.next_int_bound(list.len() as i32) as usize;
                    let p = list.remove(i);
                    let rot2 = r.next_int_bound(4);
                    let p16 = add(transform([5, 0, 6], 0, rot2, [0, 0, 0]), p);
                    if !bb_intersects(&bb_from_corners(p, p16), &bb8) {
                        ruin_piece(lib, &mut out, r, *warm, p, rot2, false, 0.8);
                    }
                }
            }
            Some((out, stub))
        }
        LKind::RuinedPortal(setups) => {
            let setup = if setups.len() > 1 {
                let total: f32 = setups.iter().map(|s| s.weight).sum();
                let mut f = r.next_float();
                let mut pick = &setups[0];
                for s in setups {
                    f -= s.weight / total;
                    if f < 0.0 {
                        pick = s;
                        break;
                    }
                }
                pick
            } else {
                &setups[0]
            };
            let air_pocket = if setup.air_pocket == 0.0 {
                false
            } else if setup.air_pocket == 1.0 {
                true
            } else {
                r.next_float() < setup.air_pocket
            };
            let name = if r.next_float() < 0.05 { format!("ruined_portal/giant_portal_{}", r.next_int_bound(3) + 1) } else { format!("ruined_portal/portal_{}", r.next_int_bound(10) + 1) };
            let t = lib.get(&name);
            let rot = r.next_int_bound(4);
            let mirror = if r.next_float() < 0.5 { 0 } else { 2 };
            let pivot = [t.size[0] / 2, 0, t.size[2] / 2];
            let origin = [x0, 0, z0];
            let tbb = t.bounding_box(origin, rot, pivot, mirror);
            let c = bb_center(&tbb);
            let hm = if setup.placement == VPlace::OnOcean { Hm::Motion } else { Hm::NotAir };
            let i = ctx.base_height(c[0], c[2], hm) - 1;
            let y = portal_y(r, ctx, setup.placement, air_pocket, i, span(&tbb, 1), &tbb);
            let pos = [x0, y, z0];
            let cold = setup.can_be_cold && {
                let b = ctx.noise_biome(pos[0] >> 2, pos[1] >> 2, pos[2] >> 2);
                ctx.temperature(b, pos) < 0.15
            };
            let procs = portal_procs(lib, setup, air_pocket, cold);
            let kind = TKind::Portal { placement: setup.placement, cold, overgrown: setup.overgrown, vines: setup.vines };
            out.push(Box::new(TPiece::new(t, pos, rot, mirror, pivot, procs, true, kind)));
            Some((out, pos))
        }
        LKind::BuriedTreasure => {
            let stub = chunk_center(ctx, cx, cz, Hm::Motion);
            let p = [x0 + 9, 90, z0 + 9];
            out.push(Box::new(Treasure { bb: Mutex::new((p, p)) }));
            Some((out, stub))
        }
        LKind::DesertPyramid | LKind::JungleTemple => {
            let (w, d) = if matches!(kind, LKind::DesertPyramid) { (21, 21) } else { (12, 15) };
            if lowest_y(ctx, x0, z0, w, d) < 63 {
                return None;
            }
            let stub = chunk_center(ctx, cx, cz, Hm::NotAir);
            if matches!(kind, LKind::DesertPyramid) {
                out.push(Box::new(super::temples::DesertPyramid::new(r, x0, z0)));
            } else {
                out.push(Box::new(super::temples::JungleTemple::new(r, x0, z0)));
            }
            Some((out, stub))
        }
        LKind::Stronghold => {
            for p in super::stronghold::generate(seed, cx, cz) {
                out.push(Box::new(p));
            }
            Some((out, [x0, 0, z0]))
        }
        LKind::WoodlandMansion => {
            // getLowestYIn5by5BoxOffset7Blocks
            let rot = r.next_int_bound(4);
            let (i, j) = match rot {
                1 => (-5, 5),
                2 => (-5, -5),
                3 => (5, -5),
                _ => (5, 5),
            };
            let (x, z) = (x0 + 7, z0 + 7);
            let y = lowest_y(ctx, x, z, i, j);
            if y < 60 {
                return None;
            }
            let pos = [x, y, z];
            for (name, p, prot, mirror) in super::mansion::generate(pos, rot, r) {
                let t = lib.get(&format!("woodland_mansion/{name}"));
                out.push(Box::new(TPiece::new(t, p, prot, mirror, [0, 0, 0], vec![Proc::Ignore(vec!["structure_block"])], true, TKind::Mansion)));
            }
            Some((out, pos))
        }
        LKind::OceanMonument => {
            // getBiomesWithin(blockX(9), seaLevel, blockZ(9), 29)
            let (x, y, z, rad) = (x0 + 9, 63, z0 + 9, 29);
            for qx in (x - rad) >> 2..=(x + rad) >> 2 {
                for qy in (y - rad) >> 2..=(y + rad) >> 2 {
                    for qz in (z - rad) >> 2..=(z + rad) >> 2 {
                        if !lib.monument_biomes.get(ctx.noise_biome(qx, qy, qz) as usize).copied().unwrap_or(false) {
                            return None;
                        }
                    }
                }
            }
            let stub = chunk_center(ctx, cx, cz, Hm::Motion);
            out.push(Box::new(super::monument::Monument::new(r, cx, cz)));
            Some((out, stub))
        }
        LKind::SwampHut => {
            let stub = chunk_center(ctx, cx, cz, Hm::NotAir);
            let dir = random_horizontal(r);
            out.push(Box::new(SwampHut(Scattered::new(x0, 64, z0, dir, 7, 7, 9))));
            Some((out, stub))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ruin_piece(lib: &LLib, out: &mut Vec<Box<dyn LPiece>>, r: &mut dyn Rng, warm: bool, pos: P, rot: i32, large: bool, integrity: f32) {
    let procs = |integrity: f32| {
        let (from, to) = if warm { ("sand", "suspicious_sand") } else { ("gravel", "suspicious_gravel") };
        vec![
            Proc::Rot { integrity, rottable: None },
            Proc::Ignore(vec!["structure_block", "air"]),
            Proc::Capped { delegate: Box::new(Proc::Rule(vec![Rule::new(RuleTest::Block(from.into()), RuleTest::Always, state(to, &[]), true)])), limit: IntP::Const(5) },
        ]
    };
    let mut push = |name: String, integrity: f32| {
        out.push(Box::new(TPiece::new(lib.get(&name), pos, rot, 0, [0, 0, 0], procs(integrity), true, TKind::OceanRuin)));
    };
    if warm {
        let name = if large { format!("underwater_ruin/big_warm_{}", r.next_int_bound(4) + 4) } else { format!("underwater_ruin/warm_{}", r.next_int_bound(8) + 1) };
        push(name, integrity);
    } else {
        let n = if large { [1, 2, 3, 8][r.next_int_bound(4) as usize] } else { r.next_int_bound(8) + 1 };
        let big = if large { "big_" } else { "" };
        push(format!("underwater_ruin/{big}brick_{n}"), integrity);
        push(format!("underwater_ruin/{big}cracked_{n}"), 0.7);
        push(format!("underwater_ruin/{big}mossy_{n}"), 0.5);
    }
}

/// `RuinedPortalStructure.findSuitableY`
fn portal_y(r: &mut dyn Rng, ctx: &dyn Ctx, placement: VPlace, air_pocket: bool, i: i32, y_span: i32, tbb: &Bb) -> i32 {
    let k = MIN_Y + 15;
    let within = |r: &mut dyn Rng, a: i32, b: i32| if a < b { rbi(r, a, b) } else { b };
    let j = match placement {
        VPlace::InNether => {
            if air_pocket {
                rbi(r, 32, 100)
            } else if r.next_float() < 0.5 {
                rbi(r, 27, 29)
            } else {
                rbi(r, 29, 100)
            }
        }
        VPlace::InMountain => within(r, 70, i - y_span),
        VPlace::Underground => within(r, k, i - y_span),
        VPlace::PartlyBuried => i - y_span + rbi(r, 2, 8),
        _ => i,
    };
    let cols: Vec<Vec<BlockState>> = [[tbb.0[0], tbb.0[2]], [tbb.1[0], tbb.0[2]], [tbb.0[0], tbb.1[2]], [tbb.1[0], tbb.1[2]]].iter().map(|c| ctx.base_column(c[0], c[1])).collect();
    let ok = |s: BlockState| if placement == VPlace::OnOcean { vstate::blocks_motion(s) } else { !vstate::air(s) };
    let mut m = j;
    while m > k {
        let mut n = 0;
        for c in &cols {
            if let Some(&s) = c.get((m - MIN_Y) as usize) {
                if ok(s) {
                    n += 1;
                    if n == 3 {
                        return m;
                    }
                }
            }
        }
        m -= 1;
    }
    m
}

fn portal_procs(lib: &LLib, s: &PortalSetup, air_pocket: bool, cold: bool) -> Vec<Proc> {
    let mut rules = vec![Rule::new(RuleTest::RandomBlock("gold_block".into(), 0.3), RuleTest::Always, blocks::AIR, false)];
    rules.push(if s.placement == VPlace::OnOcean {
        Rule::new(RuleTest::Block("lava".into()), RuleTest::Always, state("magma_block", &[]), false)
    } else if cold {
        Rule::new(RuleTest::Block("lava".into()), RuleTest::Always, state("netherrack", &[]), false)
    } else {
        Rule::new(RuleTest::RandomBlock("lava".into(), 0.2), RuleTest::Always, state("magma_block", &[]), false)
    });
    if !cold {
        rules.push(Rule::new(RuleTest::RandomBlock("netherrack".into(), 0.07), RuleTest::Always, state("magma_block", &[]), false));
    }
    let mut v = vec![
        Proc::Ignore(if air_pocket { vec!["structure_block"] } else { vec!["structure_block", "air"] }),
        Proc::Rule(rules),
        Proc::Age { mossiness: s.mossiness },
        Proc::Protected(lib.fcr.clone().unwrap_or_else(|| BlockSet(Arc::new(Vec::new())))),
        Proc::LavaSubmerged,
    ];
    if s.blackstone {
        v.push(Proc::Blackstone);
    }
    v
}

