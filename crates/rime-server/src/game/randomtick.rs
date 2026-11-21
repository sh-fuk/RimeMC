//! Random block ticks (`ServerLevel.tickChunk`, randomTickSpeed 3), snow and
//! ice from precipitation, and fire spread (`FireBlock.tick`, `LavaFluid.randomTick`).

use std::collections::HashMap;
use std::sync::OnceLock;

use rand::Rng;

use super::{Game, TickKind};
use crate::registry::{blocks, vstate, BlockRegistry, BlockState};
use crate::world::chunk::{HEIGHT, MIN_Y};

/// `GameRules.RANDOM_TICK_SPEED`
pub const RANDOM_TICK_SPEED: usize = 3;

/// Blocks with at least one randomly ticking state (dumped from 26.1).
const RANDOM_TICKING: &str = "grass_block oak_sapling spruce_sapling birch_sapling jungle_sapling acacia_sapling cherry_sapling dark_oak_sapling pale_oak_sapling mangrove_propagule lava oak_leaves spruce_leaves birch_leaves jungle_leaves acacia_leaves cherry_leaves dark_oak_leaves pale_oak_leaves mangrove_leaves azalea_leaves flowering_azalea_leaves brown_mushroom red_mushroom wheat farmland redstone_ore deepslate_redstone_ore snow ice cactus sugar_cane nether_portal copper_bars exposed_copper_bars weathered_copper_bars copper_chain exposed_copper_chain weathered_copper_chain pumpkin_stem melon_stem vine mycelium nether_wart cocoa carrots potatoes chorus_flower torchflower_crop pitcher_crop beetroots kelp turtle_egg dried_ghast bamboo_sapling bamboo copper_lantern exposed_copper_lantern weathered_copper_lantern sweet_berry_bush warped_nylium crimson_nylium weeping_vines twisting_vines budding_amethyst copper_block exposed_copper weathered_copper weathered_cut_copper exposed_cut_copper cut_copper weathered_chiseled_copper exposed_chiseled_copper chiseled_copper weathered_cut_copper_stairs exposed_cut_copper_stairs cut_copper_stairs weathered_cut_copper_slab exposed_cut_copper_slab cut_copper_slab copper_door exposed_copper_door weathered_copper_door copper_trapdoor exposed_copper_trapdoor weathered_copper_trapdoor copper_grate exposed_copper_grate weathered_copper_grate copper_bulb exposed_copper_bulb weathered_copper_bulb copper_chest exposed_copper_chest weathered_copper_chest copper_golem_statue exposed_copper_golem_statue weathered_copper_golem_statue lightning_rod exposed_lightning_rod weathered_lightning_rod pointed_dripstone cave_vines open_eyeblossom closed_eyeblossom potted_open_eyeblossom potted_closed_eyeblossom";

/// Per state id: does its block tick randomly?
fn ticking() -> &'static Vec<bool> {
    static T: OnceLock<Vec<bool>> = OnceLock::new();
    T.get_or_init(|| {
        let reg = BlockRegistry::get();
        let names: std::collections::HashSet<&str> = RANDOM_TICKING.split(' ').collect();
        (0..reg.state_count()).map(|i| names.contains(BlockState(i as u16).name())).collect()
    })
}

/// `FireBlock` ignite / burn odds by block name.
pub fn flammability(name: &str) -> (i32, i32) {
    static T: OnceLock<HashMap<String, (i32, i32)>> = OnceLock::new();
    let t = T.get_or_init(|| {
        include_str!("../../assets/26.1/flammable.tsv")
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                Some((it.next()?.to_string(), (it.next()?.parse().ok()?, it.next()?.parse().ok()?)))
            })
            .collect()
    });
    t.get(name).copied().unwrap_or((0, 0))
}

fn ignite_odds(s: BlockState) -> i32 {
    if BlockRegistry::get().property(s, "waterlogged") == Some("true") {
        return 0;
    }
    flammability(s.name()).0
}

fn burn_odds(s: BlockState) -> i32 {
    if BlockRegistry::get().property(s, "waterlogged") == Some("true") {
        return 0;
    }
    flammability(s.name()).1
}

const DIRS: [[i32; 3]; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];
const HORIZONTAL: [[i32; 3]; 4] = [[0, 0, -1], [1, 0, 0], [0, 0, 1], [-1, 0, 0]];

fn add(p: [i32; 3], d: [i32; 3]) -> [i32; 3] {
    [p[0] + d[0], p[1] + d[1], p[2] + d[2]]
}

/// `BlockTags.LOGS`-ish: what keeps leaves alive.
fn is_log(name: &str) -> bool {
    name.ends_with("_log") || name.ends_with("_wood") || name.ends_with("_stem") && !name.contains("pumpkin") && !name.contains("melon") || name.ends_with("_hyphae") || name == "mangrove_roots" || name == "pale_oak_log"
}

fn is_leaves(name: &str) -> bool {
    name.ends_with("_leaves")
}

/// `BlockTags.MAINTAINS_FARMLAND`
fn maintains_farmland(name: &str) -> bool {
    matches!(name, "wheat" | "carrots" | "potatoes" | "beetroots" | "melon_stem" | "pumpkin_stem" | "attached_melon_stem" | "attached_pumpkin_stem" | "torchflower_crop" | "pitcher_crop")
}

/// copper weathering stage (0..3) and the block name without its stage prefix
fn copper_stage(name: &str) -> Option<(usize, String)> {
    if name.starts_with("waxed_") || !name.contains("copper") && !name.contains("lightning_rod") {
        return None;
    }
    let (stage, rest) = if let Some(r) = name.strip_prefix("exposed_") {
        (1, r)
    } else if let Some(r) = name.strip_prefix("weathered_") {
        (2, r)
    } else if let Some(r) = name.strip_prefix("oxidized_") {
        (3, r)
    } else {
        (0, name)
    };
    let base = if rest == "copper" { "copper_block" } else { rest };
    Some((stage, base.to_string()))
}

fn copper_name(stage: usize, base: &str) -> String {
    let base = if base == "copper_block" && stage > 0 { "copper" } else { base };
    match stage {
        0 => base.to_string(),
        1 => format!("exposed_{base}"),
        2 => format!("weathered_{base}"),
        _ => format!("oxidized_{base}"),
    }
}

impl Game {
    /// Same block with its properties carried over (as far as they exist).
    fn same_props(&self, from: BlockState, name: &str) -> Option<BlockState> {
        let reg = BlockRegistry::get();
        let mut s = reg.state(name, &[])?;
        for (k, v) in reg.properties(from) {
            if let Some(n) = reg.with(s, k, v) {
                s = n;
            }
        }
        Some(s)
    }

    fn set_prop(&self, p: [i32; 3], s: BlockState, key: &str, value: &str) {
        if let Some(n) = BlockRegistry::get().with(s, key, value) {
            self.change_block(p, n, false, None);
        }
    }

    fn block_at(&self, p: [i32; 3]) -> BlockState {
        self.world.get_block(p[0], p[1], p[2]).unwrap_or(blocks::AIR)
    }

    /// `Level.getSkyDarken`
    pub fn sky_darken(&self) -> i32 {
        let t = self.day_time() as f64 / 24000.0;
        // DimensionType.timeOfDay
        let frac = (t - 0.25).rem_euclid(1.0);
        let tod = (frac * 2.0 + (0.5 - (frac * std::f64::consts::PI).cos() / 2.0)) / 3.0;
        let w = self.weather.lock().clone();
        let mut d = 1.0 - ((tod * std::f64::consts::PI * 2.0).cos() * 2.0 + 0.5).clamp(0.0, 1.0);
        d = 1.0 - d;
        d *= 1.0 - w.rain_level as f64 * 5.0 / 16.0;
        d *= 1.0 - (w.thunder_level * w.rain_level) as f64 * 5.0 / 16.0;
        d = 1.0 - d;
        (d * 11.0) as i32
    }

    /// `getRawBrightness(pos, darken)`
    pub fn raw_brightness(&self, p: [i32; 3], darken: i32) -> i32 {
        let (sky, block) = self.world.light_at(p[0], p[1], p[2]).unwrap_or((15, 0));
        (sky as i32 - darken).max(block as i32)
    }

    pub fn block_light_at(&self, p: [i32; 3]) -> i32 {
        self.world.light_at(p[0], p[1], p[2]).map(|l| l.1 as i32).unwrap_or(0)
    }

    /// One tick of random ticks for the chunks near players.
    pub(super) fn random_ticks(&self) {
        let mut chunks: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();
        for p in self.players() {
            if p.gamemode.load(std::sync::atomic::Ordering::Relaxed) == 3 {
                continue;
            }
            let pos = p.pose.lock().pos;
            let (pcx, pcz) = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
            for cx in pcx - 8..=pcx + 8 {
                for cz in pcz - 8..=pcz + 8 {
                    // ChunkMap.anyPlayerCloseEnoughForSpawning: within 128 blocks of the chunk centre
                    let (dx, dz) = ((cx * 16 + 8) as f64 - pos[0], (cz * 16 + 8) as f64 - pos[2]);
                    if dx * dx + dz * dz < 16384.0 {
                        chunks.insert((cx, cz));
                    }
                }
            }
        }
        let mut rng = rand::thread_rng();
        let tick = ticking();
        for (cx, cz) in chunks {
            if rng.gen_range(0..16) == 0 {
                self.tick_precipitation(cx, cz, &mut rng);
            }
            let picks: Vec<([i32; 3], BlockState)> = {
                let mut out = Vec::new();
                for sec in 0..(HEIGHT / 16) {
                    for _ in 0..RANDOM_TICK_SPEED {
                        let (x, y, z) = (rng.gen_range(0..16), MIN_Y + sec * 16 + rng.gen_range(0..16), rng.gen_range(0..16));
                        out.push(([cx * 16 + x, y, cz * 16 + z], BlockState(0)));
                    }
                }
                let states = self.world.blocks_in_chunk(cx, cz, &out.iter().map(|o| o.0).collect::<Vec<_>>());
                let Some(states) = states else { continue };
                out.into_iter().zip(states).filter(|(_, s)| tick.get(s.0 as usize).copied().unwrap_or(false)).map(|((p, _), s)| (p, s)).collect()
            };
            for (p, s) in picks {
                // the block may have changed since sampling
                if self.block_at(p) != s {
                    continue;
                }
                self.random_tick(p, s, &mut rng);
            }
        }
    }

    /// `BlockState.randomTick` / `FluidState.randomTick`
    fn random_tick(&self, p: [i32; 3], s: BlockState, rng: &mut impl Rng) {
        let reg = BlockRegistry::get();
        let name = s.name();
        let age = || reg.property(s, "age").and_then(|a| a.parse::<i32>().ok()).unwrap_or(0);
        match name {
            "wheat" | "carrots" | "potatoes" | "beetroots" | "torchflower_crop" => {
                let max = match name {
                    "beetroots" => 3,
                    "torchflower_crop" => 2,
                    _ => 7,
                };
                // BeetrootBlock / TorchflowerCropBlock: only 2 of 3 random ticks
                if matches!(name, "beetroots" | "torchflower_crop") && rng.gen_range(0..3) == 0 {
                    return;
                }
                if self.raw_brightness(p, 0) >= 9 && age() < max {
                    let speed = self.growth_speed(p, name);
                    if rng.gen_range(0..((25.0 / speed) as i32 + 1)) == 0 {
                        self.set_prop(p, s, "age", &(age() + 1).to_string());
                    }
                }
            }
            "pumpkin_stem" | "melon_stem" => {
                if self.raw_brightness(p, 0) >= 9 {
                    let speed = self.growth_speed(p, name);
                    if rng.gen_range(0..((25.0 / speed) as i32 + 1)) == 0 {
                        if age() < 7 {
                            self.set_prop(p, s, "age", &(age() + 1).to_string());
                        } else {
                            let d = HORIZONTAL[rng.gen_range(0..4)];
                            let q = add(p, d);
                            let below = self.block_at(add(q, [0, -1, 0])).name();
                            let support = matches!(below, "farmland" | "dirt" | "grass_block" | "podzol" | "coarse_dirt" | "mycelium" | "rooted_dirt" | "moss_block" | "pale_moss_block" | "mud" | "muddy_mangrove_roots");
                            if self.block_at(q).is_air() && support {
                                let (fruit, attached) = if name == "pumpkin_stem" { ("pumpkin", "attached_pumpkin_stem") } else { ("melon", "attached_melon_stem") };
                                let facing = match d {
                                    [0, 0, -1] => "north",
                                    [1, 0, 0] => "east",
                                    [0, 0, 1] => "south",
                                    _ => "west",
                                };
                                if let (Some(f), Some(a)) = (reg.state(fruit, &[]), reg.state(attached, &[("facing", facing)])) {
                                    self.change_block(q, f, false, None);
                                    self.change_block(p, a, false, None);
                                }
                            }
                        }
                    }
                }
            }
            "sugar_cane" => {
                if self.block_at(add(p, [0, 1, 0])).is_air() {
                    let mut h = 1;
                    while self.block_at(add(p, [0, -h, 0])).name() == name {
                        h += 1;
                    }
                    if h < 3 {
                        if age() == 15 {
                            if let Some(d) = reg.state(name, &[]) {
                                self.change_block(add(p, [0, 1, 0]), d, false, None);
                            }
                            self.set_prop(p, s, "age", "0");
                        } else {
                            self.set_prop(p, s, "age", &(age() + 1).to_string());
                        }
                    }
                }
            }
            "cactus" => {
                let above = add(p, [0, 1, 0]);
                if !self.block_at(above).is_air() {
                    return;
                }
                let a = age();
                let mut h = 1;
                while self.block_at(add(p, [0, -h, 0])).name() == name {
                    h += 1;
                    if h == 3 && a == 15 {
                        return;
                    }
                }
                if a == 8 && self.cactus_can_survive(above) {
                    let chance = if h >= 3 { 0.25 } else { 0.1 };
                    if rng.r#gen::<f64>() <= chance {
                        if let Some(f) = reg.state("cactus_flower", &[]) {
                            self.change_block(above, f, false, None);
                        }
                    }
                } else if a == 15 && h < 3 {
                    if let Some(d) = reg.state(name, &[]) {
                        self.change_block(above, d, false, None);
                    }
                    self.set_prop(p, s, "age", "0");
                    return;
                }
                if a < 15 {
                    self.set_prop(p, s, "age", &(a + 1).to_string());
                }
            }
            "sweet_berry_bush" => {
                if age() < 3 && rng.gen_range(0..5) == 0 && self.raw_brightness(add(p, [0, 1, 0]), 0) >= 9 {
                    self.set_prop(p, s, "age", &(age() + 1).to_string());
                }
            }
            "cocoa" => {
                if rng.gen_range(0..5) == 0 && age() < 2 {
                    self.set_prop(p, s, "age", &(age() + 1).to_string());
                }
            }
            "nether_wart" => {
                if age() < 3 && rng.gen_range(0..10) == 0 {
                    self.set_prop(p, s, "age", &(age() + 1).to_string());
                }
            }
            "kelp" | "weeping_vines" | "twisting_vines" | "cave_vines" => {
                let (dir, prob) = match name {
                    "kelp" => ([0, 1, 0], 0.14),
                    "twisting_vines" => ([0, 1, 0], 0.1),
                    _ => ([0, -1, 0], 0.1),
                };
                if age() < 25 && rng.r#gen::<f64>() < prob {
                    let q = add(p, dir);
                    let t = self.block_at(q);
                    let ok = if name == "kelp" { t.name() == "water" && vstate::fluid_source(t) } else { t.is_air() };
                    if ok {
                        if let Some(n) = reg.with(s, "age", &(age() + 1).to_string()) {
                            let n = if name == "cave_vines" { reg.with(n, "berries", if rng.r#gen::<f32>() < 0.11 { "true" } else { "false" }).unwrap_or(n) } else { n };
                            self.change_block(q, n, false, None);
                        }
                    }
                }
            }
            "grass_block" | "mycelium" => {
                if !self.grass_can_stay(p) {
                    self.change_block(p, blocks::DIRT, false, None);
                } else if self.raw_brightness(add(p, [0, 1, 0]), self.sky_darken()) >= 9 {
                    for _ in 0..4 {
                        let q = add(p, [rng.gen_range(0..3) - 1, rng.gen_range(0..5) - 3, rng.gen_range(0..3) - 1]);
                        if self.block_at(q).name() == "dirt" && self.grass_can_stay(q) && !vstate::is_water(self.block_at(add(q, [0, 1, 0]))) {
                            let above = self.block_at(add(q, [0, 1, 0])).name();
                            let snowy = matches!(above, "snow" | "snow_block" | "powder_snow");
                            if let Some(g) = reg.state(name, &[("snowy", if snowy { "true" } else { "false" })]) {
                                self.change_block(q, g, false, None);
                            }
                        }
                    }
                }
            }
            "crimson_nylium" | "warped_nylium" => {
                // NyliumBlock.randomTick: covered nylium turns back into netherrack
                let above = self.block_at(add(p, [0, 1, 0]));
                if vstate::full_block(above) && crate::world::light::dampening(above) >= 15 {
                    if let Some(n) = reg.state("netherrack", &[]) {
                        self.change_block(p, n, false, None);
                    }
                }
            }
            n if is_leaves(n) => {
                if reg.property(s, "persistent") == Some("true") {
                    return;
                }
                // LeavesBlock.updateDistance, then decaying()
                let d = self.leaf_distance(p);
                if d >= 7 {
                    self.change_block(p, blocks::AIR, true, None);
                } else if reg.property(s, "distance") != Some(&*d.to_string()) {
                    self.set_prop(p, s, "distance", &d.to_string());
                }
            }
            "ice" => {
                if self.block_light_at(p) > 11 - crate::world::light::dampening(s) as i32 {
                    self.change_block(p, blocks::WATER, false, None);
                }
            }
            "snow" => {
                if self.block_light_at(p) > 11 {
                    self.change_block(p, blocks::AIR, true, None);
                }
            }
            "farmland" => {
                let m: i32 = reg.property(s, "moisture").and_then(|m| m.parse().ok()).unwrap_or(0);
                if !self.near_water(p) && !self.raining_at(add(p, [0, 1, 0])) {
                    if m > 0 {
                        self.set_prop(p, s, "moisture", &(m - 1).to_string());
                    } else if !maintains_farmland(self.block_at(add(p, [0, 1, 0])).name()) {
                        self.change_block(p, blocks::DIRT, false, None);
                    }
                } else if m < 7 {
                    self.set_prop(p, s, "moisture", "7");
                }
            }
            "budding_amethyst" => {
                if rng.gen_range(0..5) == 0 {
                    let (dname, d) = [("down", [0, -1, 0]), ("up", [0, 1, 0]), ("north", [0, 0, -1]), ("south", [0, 0, 1]), ("west", [-1, 0, 0]), ("east", [1, 0, 0])][rng.gen_range(0..6)];
                    let q = add(p, d);
                    let t = self.block_at(q);
                    let facing_ok = reg.property(t, "facing") == Some(dname);
                    let next = if t.is_air() || t.name() == "water" && vstate::fluid_source(t) {
                        Some("small_amethyst_bud")
                    } else if t.name() == "small_amethyst_bud" && facing_ok {
                        Some("medium_amethyst_bud")
                    } else if t.name() == "medium_amethyst_bud" && facing_ok {
                        Some("large_amethyst_bud")
                    } else if t.name() == "large_amethyst_bud" && facing_ok {
                        Some("amethyst_cluster")
                    } else {
                        None
                    };
                    if let Some(n) = next {
                        let wl = if vstate::is_water(t) { "true" } else { "false" };
                        if let Some(ns) = reg.state(n, &[("facing", dname), ("waterlogged", wl)]) {
                            self.change_block(q, ns, false, None);
                        }
                    }
                }
            }
            "brown_mushroom" | "red_mushroom" => {
                if rng.gen_range(0..25) != 0 {
                    return;
                }
                let mut max = 5;
                for x in -4..=4 {
                    for y in -1..=1 {
                        for z in -4..=4 {
                            if self.block_at(add(p, [x, y, z])).name() == name {
                                max -= 1;
                                if max <= 0 {
                                    return;
                                }
                            }
                        }
                    }
                }
                let mut pos = p;
                let mut off = add(pos, [rng.gen_range(0..3) - 1, rng.gen_range(0..2) - rng.gen_range(0..2), rng.gen_range(0..3) - 1]);
                for _ in 0..4 {
                    if self.block_at(off).is_air() && self.mushroom_can_survive(off) {
                        pos = off;
                    }
                    off = add(pos, [rng.gen_range(0..3) - 1, rng.gen_range(0..2) - rng.gen_range(0..2), rng.gen_range(0..3) - 1]);
                }
                if self.block_at(off).is_air() && self.mushroom_can_survive(off) {
                    self.change_block(off, s, false, None);
                }
            }
            "redstone_ore" | "deepslate_redstone_ore" => {
                if reg.property(s, "lit") == Some("true") {
                    self.set_prop(p, s, "lit", "false");
                }
            }
            "open_eyeblossom" | "closed_eyeblossom" => {
                // EyeblossomBlock: opens at night, closes by day
                let night = !self.is_day();
                let target = if night { "open_eyeblossom" } else { "closed_eyeblossom" };
                if name != target {
                    if let Some(n) = reg.state(target, &[]) {
                        self.change_block(p, n, false, None);
                        self.play_sound(if night { "minecraft:block.eyeblossom.open_long" } else { "minecraft:block.eyeblossom.close_long" }, 4, [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5], 1.0, 1.0);
                    }
                }
            }
            "lava" => self.lava_fire(p, rng),
            n if copper_stage(n).is_some_and(|(st, _)| st < 3) => {
                // ChangeOverTimeBlock.changeOverTime
                if rng.r#gen::<f32>() >= 0.05688889 {
                    return;
                }
                let (own, base) = copper_stage(n).unwrap_or((0, String::new()));
                let (mut same, mut older) = (0, 0);
                for dx in -4i32..=4 {
                    for dy in -4i32..=4 {
                        for dz in -4i32..=4 {
                            let md = dx.abs() + dy.abs() + dz.abs();
                            if md > 4 || md == 0 {
                                continue;
                            }
                            let Some((st, _)) = copper_stage(self.block_at(add(p, [dx, dy, dz])).name()) else { continue };
                            if st < own {
                                return;
                            }
                            if st > own {
                                older += 1;
                            } else {
                                same += 1;
                            }
                        }
                    }
                }
                let chance = (older + 1) as f32 / (older + same + 1) as f32;
                let modifier = if own == 0 { 0.75 } else { 1.0 };
                if rng.r#gen::<f32>() < chance * chance * modifier {
                    if let Some(next) = self.same_props(s, &copper_name(own + 1, &base)) {
                        self.change_block(p, next, false, None);
                    }
                }
            }
            _ => {}
        }
    }

    /// `CropBlock.getGrowthSpeed`
    fn growth_speed(&self, p: [i32; 3], name: &str) -> f32 {
        let reg = BlockRegistry::get();
        let mut speed = 1.0f32;
        for dx in -1..=1 {
            for dz in -1..=1 {
                let b = self.block_at([p[0] + dx, p[1] - 1, p[2] + dz]);
                let mut f = 0.0;
                if b.name() == "farmland" {
                    f = 1.0;
                    if reg.property(b, "moisture").is_some_and(|m| m != "0") {
                        f = 3.0;
                    }
                }
                if dx != 0 || dz != 0 {
                    f /= 4.0;
                }
                speed += f;
            }
        }
        let is = |d: [i32; 3]| self.block_at(add(p, d)).name() == name;
        let horizontal = is([-1, 0, 0]) || is([1, 0, 0]);
        let vertical = is([0, 0, -1]) || is([0, 0, 1]);
        if horizontal && vertical {
            speed /= 2.0;
        } else if is([-1, 0, -1]) || is([1, 0, -1]) || is([1, 0, 1]) || is([-1, 0, 1]) {
            speed /= 2.0;
        }
        speed
    }

    fn cactus_can_survive(&self, p: [i32; 3]) -> bool {
        for d in HORIZONTAL {
            let b = self.block_at(add(p, d));
            if vstate::solid(b) || b.name() == "lava" {
                return false;
            }
        }
        let below = self.block_at(add(p, [0, -1, 0]));
        matches!(below.name(), "cactus" | "sand" | "red_sand" | "suspicious_sand") && !vstate::has_fluid(self.block_at(add(p, [0, 1, 0])))
    }

    fn mushroom_can_survive(&self, p: [i32; 3]) -> bool {
        let below = self.block_at(add(p, [0, -1, 0]));
        if matches!(below.name(), "mycelium" | "podzol" | "crimson_nylium" | "warped_nylium") {
            return true;
        }
        self.world.light_at(p[0], p[1], p[2]).is_some_and(|(sky, block)| sky.max(block) < 13) && vstate::sturdy(below, 1)
    }

    /// `SpreadingSnowyBlock.canStayAlive`
    fn grass_can_stay(&self, p: [i32; 3]) -> bool {
        let above = self.block_at(add(p, [0, 1, 0]));
        if above.name() == "snow" && BlockRegistry::get().property(above, "layers") == Some("1") {
            return true;
        }
        if vstate::has_fluid(above) && vstate::fluid_source(above) {
            return false;
        }
        // LightEngine.getLightBlockInto: opaque full faces block all light
        !(vstate::full_block(above) && crate::world::light::dampening(above) >= 15)
    }

    /// `LeavesBlock.updateDistance`: steps to the nearest log (max 7).
    fn leaf_distance(&self, p: [i32; 3]) -> i32 {
        let mut frontier = vec![p];
        let mut seen = std::collections::HashSet::new();
        seen.insert(p);
        for d in 1..7 {
            let mut next = Vec::new();
            for q in frontier {
                for dir in DIRS {
                    let r = add(q, dir);
                    if !seen.insert(r) {
                        continue;
                    }
                    let b = self.block_at(r);
                    if is_log(b.name()) {
                        return d;
                    }
                    if is_leaves(b.name()) {
                        next.push(r);
                    }
                }
            }
            frontier = next;
        }
        7
    }

    /// `FarmlandBlock.isNearWater`
    fn near_water(&self, p: [i32; 3]) -> bool {
        for dx in -4..=4 {
            for dz in -4..=4 {
                for dy in 0..=1 {
                    if vstate::is_water(self.block_at([p[0] + dx, p[1] + dy, p[2] + dz])) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// `ServerLevel.tickPrecipitation`: ice on cold water, snow layers.
    fn tick_precipitation(&self, cx: i32, cz: i32, rng: &mut impl Rng) {
        let (x, z) = (cx * 16 + rng.gen_range(0..16), cz * 16 + rng.gen_range(0..16));
        let Some(top) = self.world.surface_y(x, z) else { return };
        let below = [x, top, z];
        let top_pos = [x, top + 1, z];
        let Some(biome) = self.world.biome(x, top, z) else { return };
        let cold = |p: [i32; 3]| super::weather::temperature(&biome, p[0], p[1], p[2]) < 0.15;
        // Biome.shouldFreeze (must be at an edge)
        if cold(below) && self.block_light_at(below) < 10 {
            let b = self.block_at(below);
            if b.name() == "water" && vstate::fluid_source(b) {
                let surrounded = HORIZONTAL.iter().all(|d| vstate::is_water(self.block_at(add(below, *d))));
                if !surrounded {
                    if let Some(ice) = BlockRegistry::get().state("ice", &[]) {
                        self.change_block(below, ice, false, None);
                    }
                }
            }
        }
        if self.raining() && cold(top_pos) && self.block_light_at(top_pos) < 10 {
            // Biome.shouldSnow, snowAccumulationHeight 1
            let t = self.block_at(top_pos);
            if t.is_air() && self.snow_can_survive(top_pos) {
                if let Some(sn) = BlockRegistry::get().state("snow", &[]) {
                    self.change_block(top_pos, sn, false, None);
                }
            }
        }
        // cauldrons collect rain / snow
        if self.raining() {
            let b = self.block_at(below);
            if b.name() == "cauldron" {
                let rain = !cold(below);
                // AbstractCauldronBlock.handlePrecipitation: 1/20 (rain) or 1/7 (snow) chance
                let chance = if rain { 0.05 } else { 0.1 };
                if rng.r#gen::<f32>() < chance {
                    let n = if rain { "water_cauldron" } else { "powder_snow_cauldron" };
                    if let Some(c) = BlockRegistry::get().state(n, &[("level", "1")]) {
                        self.change_block(below, c, false, None);
                    }
                }
            }
        }
    }

    fn snow_can_survive(&self, p: [i32; 3]) -> bool {
        let below = self.block_at(add(p, [0, -1, 0]));
        match below.name() {
            "ice" | "packed_ice" | "barrier" => false,
            "honey_block" | "soul_sand" => true,
            "snow" => BlockRegistry::get().property(below, "layers") == Some("8"),
            _ => vstate::full_up_face(below),
        }
    }

    /// `LavaFluid.randomTick`
    fn lava_fire(&self, p: [i32; 3], rng: &mut impl Rng) {
        let fire = BlockRegistry::get().state("fire", &[]).unwrap_or(blocks::AIR);
        let flammable = |q: [i32; 3]| self.world.get_block(q[0], q[1], q[2]).is_some_and(|b| ignite_odds(b) > 0);
        let passes = rng.gen_range(0..3);
        if passes > 0 {
            let mut t = p;
            for _ in 0..passes {
                t = add(t, [rng.gen_range(0..3) - 1, 1, rng.gen_range(0..3) - 1]);
                let Some(b) = self.world.get_block(t[0], t[1], t[2]) else { return };
                if b.is_air() {
                    if DIRS.iter().any(|d| flammable(add(t, *d))) {
                        self.change_block(t, fire, false, None);
                        return;
                    }
                } else if vstate::blocks_motion(b) {
                    return;
                }
            }
        } else {
            for _ in 0..3 {
                let t = add(p, [rng.gen_range(0..3) - 1, 0, rng.gen_range(0..3) - 1]);
                if self.world.get_block(t[0], t[1], t[2]).is_none() {
                    return;
                }
                if self.block_at(add(t, [0, 1, 0])).is_air() && flammable(t) {
                    self.change_block(add(t, [0, 1, 0]), fire, false, None);
                }
            }
        }
    }

    /// `FireBlock.tick`
    pub(super) fn fire_tick(&self, p: [i32; 3], s: BlockState) {
        let mut rng = rand::thread_rng();
        let reg = BlockRegistry::get();
        self.schedule(p, TickKind::Fire, 30 + rng.gen_range(0..10));
        let below_state = self.block_at(add(p, [0, -1, 0]));
        // FireBlock.canSurvive: sturdy block below or a flammable neighbour
        let valid_location = DIRS.iter().any(|d| burn_odds(self.block_at(add(p, *d))) > 0 || ignite_odds(self.block_at(add(p, *d))) > 0);
        if !vstate::sturdy(below_state, 1) && !valid_location {
            self.change_block(p, blocks::AIR, false, None);
            return;
        }
        let infiniburn = matches!(below_state.name(), "netherrack" | "magma_block");
        let age: i32 = reg.property(s, "age").and_then(|a| a.parse().ok()).unwrap_or(0);
        let near_rain = |q: [i32; 3]| self.raining_at(q) || HORIZONTAL.iter().any(|d| self.raining_at(add(q, *d)));
        if !infiniburn && self.raining() && near_rain(p) && rng.r#gen::<f32>() < 0.2 + age as f32 * 0.03 {
            self.change_block(p, blocks::AIR, false, None);
            return;
        }
        let new_age = (age + rng.gen_range(0..3) / 2).min(15);
        let mut s = s;
        if new_age != age {
            if let Some(n) = reg.with(s, "age", &new_age.to_string()) {
                s = n;
                self.world.set_block(p[0], p[1], p[2], n);
                self.broadcast_block(p[0], p[1], p[2], n);
            }
        }
        let _ = s;
        if !infiniburn {
            let can_burn = |q: [i32; 3]| ignite_odds(self.block_at(q)) > 0;
            if !DIRS.iter().any(|d| can_burn(add(p, *d))) {
                if !vstate::sturdy(below_state, 1) || age > 3 {
                    self.change_block(p, blocks::AIR, false, None);
                }
                return;
            }
            if age == 15 && rng.gen_range(0..4) == 0 && !can_burn(add(p, [0, -1, 0])) {
                self.change_block(p, blocks::AIR, false, None);
                return;
            }
        }
        for (d, chance) in [([1, 0, 0], 300), ([-1, 0, 0], 300), ([0, -1, 0], 250), ([0, 1, 0], 250), ([0, 0, -1], 300), ([0, 0, 1], 300)] {
            self.check_burn_out(add(p, d), chance, age, &mut rng);
        }
        let difficulty = self.difficulty();
        for xx in -1..=1 {
            for zz in -1..=1 {
                for yy in -1..=4 {
                    if xx == 0 && yy == 0 && zz == 0 {
                        continue;
                    }
                    let rate = if yy > 1 { 100 + (yy - 1) * 100 } else { 100 };
                    let t = add(p, [xx, yy, zz]);
                    // getIgniteOdds(level, pos): empty block next to flammables
                    if !self.block_at(t).is_air() {
                        continue;
                    }
                    let odds_n = DIRS.iter().map(|d| ignite_odds(self.block_at(add(t, *d)))).max().unwrap_or(0);
                    if odds_n <= 0 {
                        continue;
                    }
                    let odds = (odds_n + 40 + difficulty * 7) / (age + 30);
                    if odds > 0 && rng.gen_range(0..rate) <= odds && (!self.raining() || !near_rain(t)) {
                        let spread = (age + rng.gen_range(0..5) / 4).min(15);
                        if let Some(f) = reg.state("fire", &[("age", &spread.to_string())]) {
                            self.change_block(t, f, false, None);
                        }
                    }
                }
            }
        }
    }

    /// `FireBlock.checkBurnOut`
    fn check_burn_out(&self, p: [i32; 3], chance: i32, age: i32, rng: &mut impl Rng) {
        let old = self.block_at(p);
        let odds = burn_odds(old);
        if rng.gen_range(0..chance) < odds {
            if rng.gen_range(0..(age + 10)) < 5 && !self.raining_at(p) {
                let new_age = (age + rng.gen_range(0..5) / 4).min(15);
                if let Some(f) = BlockRegistry::get().state("fire", &[("age", &new_age.to_string())]) {
                    self.change_block(p, f, false, None);
                }
            } else {
                self.change_block(p, blocks::AIR, false, None);
            }
            if old.name() == "tnt" {
                // TntBlock.prime
                self.explode([p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5], 4.0, "tnt");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables() {
        assert_eq!(flammability("oak_planks"), (5, 20));
        assert_eq!(flammability("stone"), (0, 0));
        assert!(ticking()[BlockRegistry::get().state("wheat", &[]).unwrap().0 as usize]);
        assert_eq!(copper_stage("exposed_cut_copper_slab"), Some((1, "cut_copper_slab".to_string())));
        assert_eq!(copper_name(1, "copper_block"), "exposed_copper");
        assert_eq!(copper_stage("copper_block"), Some((0, "copper_block".to_string())));
        assert_eq!(copper_stage("waxed_copper_block"), None);
    }
}
