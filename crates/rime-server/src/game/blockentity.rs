//! Block entities (chests, barrels, furnaces, …) stored as vanilla NBT in the
//! chunk (`block_entities`), plus the furnace logic of
//! `AbstractFurnaceBlockEntity.serverTick`.

use crate::data::{CookKind, GameData, Items, Stack};
use crate::nbt::Tag;

/// Block entity type id of a block (None: the block has none).
pub fn be_id(block: &str) -> Option<&'static str> {
    Some(match block {
        "chest" => "minecraft:chest",
        "trapped_chest" => "minecraft:trapped_chest",
        "barrel" => "minecraft:barrel",
        "furnace" => "minecraft:furnace",
        "blast_furnace" => "minecraft:blast_furnace",
        "smoker" => "minecraft:smoker",
        "dispenser" => "minecraft:dispenser",
        "dropper" => "minecraft:dropper",
        "hopper" => "minecraft:hopper",
        "spawner" => "minecraft:mob_spawner",
        "trial_spawner" => "minecraft:trial_spawner",
        "vault" => "minecraft:vault",
        "suspicious_sand" | "suspicious_gravel" => "minecraft:brushable_block",
        "decorated_pot" => "minecraft:decorated_pot",
        "ender_chest" => "minecraft:ender_chest",
        "enchanting_table" => "minecraft:enchanting_table",
        "bell" => "minecraft:bell",
        "lectern" => "minecraft:lectern",
        "jukebox" => "minecraft:jukebox",
        "beacon" => "minecraft:beacon",
        "campfire" | "soul_campfire" => "minecraft:campfire",
        "beehive" | "bee_nest" => "minecraft:beehive",
        "chiseled_bookshelf" => "minecraft:chiseled_bookshelf",
        "brewing_stand" => "minecraft:brewing_stand",
        "crafter" => "minecraft:crafter",
        "sculk_sensor" => "minecraft:sculk_sensor",
        "calibrated_sculk_sensor" => "minecraft:calibrated_sculk_sensor",
        "sculk_catalyst" => "minecraft:sculk_catalyst",
        "sculk_shrieker" => "minecraft:sculk_shrieker",
        "conduit" => "minecraft:conduit",
        "daylight_detector" => "minecraft:daylight_detector",
        "comparator" => "minecraft:comparator",
        "creaking_heart" => "minecraft:creaking_heart",
        n if n.ends_with("shulker_box") => "minecraft:shulker_box",
        n if n.ends_with("_bed") => "minecraft:bed",
        n if n.ends_with("_hanging_sign") => "minecraft:hanging_sign",
        n if n.ends_with("_sign") => "minecraft:sign",
        n if n.ends_with("_banner") => "minecraft:banner",
        n if n.ends_with("_skull") || n.ends_with("_head") => "minecraft:skull",
        n if n.ends_with("copper_golem_statue") => "minecraft:copper_golem_statue",
        n if n.ends_with("_shelf") => "minecraft:shelf",
        _ => return None,
    })
}

/// A container's screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// generic 9×rows (chest, barrel, shulker box)
    Rows(u8),
    /// dispenser / dropper (3×3)
    Grid3,
    Hopper,
    Furnace(CookKind),
}

impl Screen {
    pub fn size(self) -> usize {
        match self {
            Screen::Rows(r) => r as usize * 9,
            Screen::Grid3 => 9,
            Screen::Hopper => 5,
            Screen::Furnace(_) => 3,
        }
    }

    /// `minecraft:menu` registry key
    pub fn menu(self) -> &'static str {
        match self {
            Screen::Rows(1) => "minecraft:generic_9x1",
            Screen::Rows(2) => "minecraft:generic_9x2",
            Screen::Rows(3) => "minecraft:generic_9x3",
            Screen::Rows(4) => "minecraft:generic_9x4",
            Screen::Rows(5) => "minecraft:generic_9x5",
            Screen::Rows(_) => "minecraft:generic_9x6",
            Screen::Grid3 => "minecraft:generic_3x3",
            Screen::Hopper => "minecraft:hopper",
            Screen::Furnace(CookKind::Blasting) => "minecraft:blast_furnace",
            Screen::Furnace(CookKind::Smoking) => "minecraft:smoker",
            Screen::Furnace(_) => "minecraft:furnace",
        }
    }
}

/// Screen and title of a container block.
pub fn screen_of(block: &str) -> Option<(Screen, &'static str)> {
    Some(match block {
        "chest" | "trapped_chest" => (Screen::Rows(3), "Chest"),
        "barrel" => (Screen::Rows(3), "Barrel"),
        "dispenser" => (Screen::Grid3, "Dispenser"),
        "dropper" => (Screen::Grid3, "Dropper"),
        "hopper" => (Screen::Hopper, "Item Hopper"),
        "furnace" => (Screen::Furnace(CookKind::Smelting), "Furnace"),
        "blast_furnace" => (Screen::Furnace(CookKind::Blasting), "Blast Furnace"),
        "smoker" => (Screen::Furnace(CookKind::Smoking), "Smoker"),
        n if n.ends_with("shulker_box") => (Screen::Rows(3), "Shulker Box"),
        _ => return None,
    })
}

pub fn new_tag(id: &str, p: [i32; 3]) -> Tag {
    Tag::Compound(vec![("id".into(), Tag::String(id.into())), ("x".into(), Tag::Int(p[0])), ("y".into(), Tag::Int(p[1])), ("z".into(), Tag::Int(p[2]))])
}

fn put(t: &mut Tag, key: &str, v: Tag) {
    if let Tag::Compound(c) = t {
        match c.iter_mut().find(|(k, _)| k == key) {
            Some(e) => e.1 = v,
            None => c.push((key.to_string(), v)),
        }
    }
}

fn remove(t: &mut Tag, key: &str) {
    if let Tag::Compound(c) = t {
        c.retain(|(k, _)| k != key);
    }
}

/// `Items` list → slots.
pub fn items(t: &Tag, size: usize) -> Vec<Option<Stack>> {
    let mut out = vec![None; size];
    let reg = Items::get();
    if let Some(Tag::List(l)) = t.get("Items") {
        for e in l {
            let Some(slot) = e.get("Slot").and_then(|s| s.as_i64()) else { continue };
            let Some(id) = e.get("id").and_then(|s| s.as_str()).and_then(|n| reg.id(n)) else { continue };
            let n = e.get("count").or(e.get("Count")).and_then(|c| c.as_i64()).unwrap_or(1).clamp(1, 99) as u8;
            if (0..size as i64).contains(&slot) {
                let mut s = Stack::new(id, n);
                if let Some(c) = e.get("components") {
                    s.comp = crate::components::from_nbt(c);
                }
                out[slot as usize] = Some(s);
            }
        }
    }
    out
}

pub fn set_items(t: &mut Tag, slots: &[Option<Stack>]) {
    let reg = Items::get();
    let list = slots
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let s = (*s)?;
            (s.count > 0).then(|| {
                let mut f = vec![
                    ("Slot".into(), Tag::Byte(i as i8)),
                    ("id".into(), Tag::String(format!("minecraft:{}", reg.name(s.item)))),
                    ("count".into(), Tag::Int(s.count as i32)),
                ];
                if let Some(c) = crate::components::to_nbt(s.comp) {
                    f.push(("components".into(), c));
                }
                Tag::Compound(f)
            })
        })
        .collect();
    put(t, "Items", Tag::List(list));
}

/// `RandomizableContainer.unpackLootTable`: fills the container from its
/// `LootTable` (vanilla `LootTable.fill`: shuffled empty slots, stacks split
/// across them) and removes the table.
pub fn unpack_loot(t: &mut Tag, size: usize, data: &GameData) -> bool {
    let Some(table) = t.get("LootTable").and_then(|v| v.as_str()).map(str::to_string) else { return false };
    let seed = t.get("LootTableSeed").and_then(|v| v.as_i64()).unwrap_or(0);
    remove(t, "LootTable");
    remove(t, "LootTableSeed");
    use rand::{Rng, SeedableRng};
    let mut rng = if seed == 0 { rand::rngs::StdRng::from_entropy() } else { rand::rngs::StdRng::seed_from_u64(seed as u64) };
    let mut slots = items(t, size);
    let mut loot = data.roll_loot(&table, &mut rng);
    let mut empty: Vec<usize> = (0..size).filter(|&i| slots[i].is_none()).collect();
    // shuffle the free slots
    for i in (1..empty.len()).rev() {
        let j = rng.gen_range(0..=i);
        empty.swap(i, j);
    }
    // shuffleAndSplitItems: split stacks while there is room
    let mut whole: Vec<Stack> = Vec::new();
    let mut split: Vec<Stack> = Vec::new();
    for s in loot.drain(..) {
        if s.count > 1 {
            split.push(s);
        } else {
            whole.push(s);
        }
    }
    while empty.len().saturating_sub(whole.len() + split.len()) > 0 && !split.is_empty() {
        let i = rng.gen_range(0..split.len());
        let mut s = split.remove(i);
        let half = rng.gen_range(1..=(s.count / 2).max(1));
        let part = Stack::new(s.item, half);
        s.count -= half;
        if s.count > 1 && rng.gen_bool(0.5) {
            split.push(s);
        } else {
            whole.push(s);
        }
        if part.count > 1 && rng.gen_bool(0.5) {
            split.push(part);
        } else {
            whole.push(part);
        }
    }
    whole.extend(split);
    for i in (1..whole.len()).rev() {
        let j = rng.gen_range(0..=i);
        whole.swap(i, j);
    }
    for s in whole {
        let Some(slot) = empty.pop() else { break };
        slots[slot] = Some(s);
    }
    set_items(t, &slots);
    true
}

// ------------------------------------------------------------ furnaces

/// Burn time in ticks (`FuelValues` of vanilla 26.1).
pub fn fuel(item: &str, data: &GameData) -> i32 {
    let id = Items::get().id(item).unwrap_or(-1);
    let tag = |t: &str| data.item_in(t, id);
    match item {
        "lava_bucket" => 20000,
        "coal_block" => 16000,
        "blaze_rod" => 2400,
        "coal" | "charcoal" => 1600,
        "dried_kelp_block" => 4001,
        "bamboo_mosaic" | "bamboo_mosaic_stairs" | "bamboo_block" | "stripped_bamboo_block" => 300,
        "bamboo_mosaic_slab" => 150,
        "bookshelf" | "chiseled_bookshelf" | "lectern" | "jukebox" | "chest" | "trapped_chest" | "crafting_table" | "daylight_detector" | "note_block" | "cartography_table" | "fletching_table" | "smithing_table" | "loom" | "barrel" | "composter" | "crossbow" | "bow" | "fishing_rod" | "ladder" => 300,
        "wooden_pickaxe" | "wooden_shovel" | "wooden_axe" | "wooden_hoe" | "wooden_sword" => 200,
        "stick" | "bamboo" | "dead_bush" | "short_dry_grass" | "tall_dry_grass" | "bowl" => 100,
        "scaffolding" => 50,
        "azalea" | "flowering_azalea" | "mangrove_roots" | "leaf_litter" => 300,
        _ if item.ends_with("_wool") => 100,
        _ if item.ends_with("_carpet") => 67,
        _ if item.ends_with("_sapling") || item.ends_with("_propagule") => 100,
        _ if item.ends_with("_boat") || item.ends_with("_raft") => 1200,
        _ if item.ends_with("_hanging_sign") => 800,
        _ if item.ends_with("_sign") || item.ends_with("_door") || item.ends_with("_banner") => 200,
        _ if item.ends_with("_shelf") => 300,
        _ if tag("logs") || tag("planks") || tag("wooden_stairs") || tag("wooden_pressure_plates") || tag("wooden_fences") || tag("fence_gates") || tag("wooden_trapdoors") => 300,
        _ if tag("wooden_slabs") => 150,
        _ if tag("wooden_buttons") => 100,
        _ => 0,
    }
}

/// Runtime view of a furnace block entity.
#[derive(Clone, Debug, Default)]
pub struct Furnace {
    /// 0 input, 1 fuel, 2 result
    pub slots: [Option<Stack>; 3],
    pub lit_time: i32,
    pub lit_duration: i32,
    pub cook_progress: i32,
    pub cook_total: i32,
}

impl Furnace {
    pub fn read(t: &Tag) -> Furnace {
        let s = items(t, 3);
        let get = |k: &str, old: &str| t.get(k).or(t.get(old)).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        Furnace {
            slots: [s[0], s[1], s[2]],
            lit_time: get("lit_time_remaining", "BurnTime"),
            lit_duration: get("lit_total_time", "BurnTime"),
            cook_progress: get("cooking_time_spent", "CookTime"),
            cook_total: get("cooking_total_time", "CookTimeTotal"),
        }
    }

    pub fn write(&self, t: &mut Tag) {
        set_items(t, &self.slots);
        put(t, "lit_time_remaining", Tag::Short(self.lit_time.clamp(0, i16::MAX as i32) as i16));
        put(t, "lit_total_time", Tag::Short(self.lit_duration.clamp(0, i16::MAX as i32) as i16));
        put(t, "cooking_time_spent", Tag::Short(self.cook_progress as i16));
        put(t, "cooking_total_time", Tag::Short(self.cook_total as i16));
    }

    pub fn lit(&self) -> bool {
        self.lit_time > 0
    }

    /// `canBurn`: the result fits into the output slot
    fn can_burn(&self, result: Option<Stack>) -> bool {
        let Some(r) = result else { return false };
        match self.slots[2] {
            None => true,
            Some(o) => o.same(&r) && o.count as u32 + r.count as u32 <= o.max() as u32,
        }
    }

    /// One tick; returns (changed, lit state changed).
    pub fn tick(&mut self, kind: CookKind, data: &GameData) -> (bool, bool) {
        let was_lit = self.lit();
        let mut changed = false;
        if self.lit() {
            self.lit_time -= 1;
        }
        let items = Items::get();
        let recipe = self.slots[0].and_then(|i| data.cook(kind, i.item));
        let fuel_item = self.slots[1];
        let has_input = self.slots[0].is_some();
        let has_fuel = fuel_item.is_some();
        if self.lit() || (has_fuel && has_input) {
            let result = recipe.map(|r| r.result);
            let can = recipe.is_some() && self.can_burn(result);
            if !self.lit() && can {
                let f = fuel_item.map(|s| fuel(items.name(s.item), data)).unwrap_or(0);
                self.lit_time = f;
                self.lit_duration = f;
                if self.lit() {
                    changed = true;
                    if let Some(mut s) = self.slots[1] {
                        let name = items.name(s.item).to_string();
                        s.count -= 1;
                        self.slots[1] = if s.count > 0 {
                            Some(s)
                        } else if name == "lava_bucket" {
                            items.id("bucket").map(|b| Stack::new(b, 1))
                        } else {
                            None
                        };
                    }
                }
            }
            if self.lit() && can {
                self.cook_progress += 1;
                if self.cook_progress == self.cook_total {
                    self.cook_progress = 0;
                    self.cook_total = recipe.map(|r| r.time).unwrap_or(200);
                    // burn: one input becomes the result
                    let r = result.expect("checked");
                    self.slots[2] = Some(match self.slots[2] {
                        Some(mut o) => {
                            o.count += r.count;
                            o
                        }
                        None => r,
                    });
                    if let Some(mut i) = self.slots[0] {
                        let wet_sponge = items.name(i.item) == "wet_sponge";
                        i.count -= 1;
                        self.slots[0] = (i.count > 0).then_some(i);
                        // a wet sponge fills an empty bucket in the fuel slot
                        if wet_sponge && self.slots[1].is_some_and(|f| items.name(f.item) == "bucket") {
                            self.slots[1] = items.id("water_bucket").map(|b| Stack::new(b, 1));
                        }
                    }
                    changed = true;
                }
            } else {
                self.cook_progress = 0;
            }
        } else if !self.lit() && self.cook_progress > 0 {
            self.cook_progress = (self.cook_progress - 2).clamp(0, self.cook_total);
        }
        if was_lit != self.lit() {
            changed = true;
        }
        (changed, was_lit != self.lit())
    }

    /// A new input item restarts the cooking (`setItem` of slot 0).
    pub fn input_changed(&mut self, kind: CookKind, data: &GameData) {
        self.cook_total = self.slots[0].and_then(|i| data.cook(kind, i.item)).map(|r| r.time).unwrap_or(200);
        self.cook_progress = 0;
    }
}
