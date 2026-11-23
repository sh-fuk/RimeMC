//! Block-entity side of the workstations: `BrewingStandBlockEntity`,
//! `BeaconBlockEntity`, enchanting table bookshelves, anvil damage, and the
//! `update_recipes` packet (stonecutter recipes and recipe property sets).

use std::sync::Arc;

use super::block::with_bool;
use super::blockentity::{self, Screen};
use super::combat::effects::{Effect, Instance};
use super::inventory::Window;
use super::level::flags;
use super::{Game, Player};
use crate::data::{CookKind, GameData, Items, Stack};
use crate::nbt::Tag;
use crate::net::codec::Out;
use crate::registry::BlockState;

// ------------------------------------------------------------ brewing

/// `PotionBrewing.addVanillaMixes`: (from potion, ingredient, to potion)
fn potion_mixes() -> &'static Vec<(&'static str, &'static str, &'static str)> {
    static M: std::sync::OnceLock<Vec<(&str, &str, &str)>> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        let mut v: Vec<(&str, &str, &str)> = vec![
            ("water", "glowstone_dust", "thick"),
            ("water", "redstone", "mundane"),
            ("water", "nether_wart", "awkward"),
        ];
        let mut start = |v: &mut Vec<(&'static str, &'static str, &'static str)>, item: &'static str, potion: &'static str| {
            v.push(("water", item, "mundane"));
            v.push(("awkward", item, potion));
        };
        start(&mut v, "breeze_rod", "wind_charged");
        start(&mut v, "slime_block", "oozing");
        start(&mut v, "stone", "infested");
        start(&mut v, "cobweb", "weaving");
        v.extend([
            ("awkward", "golden_carrot", "night_vision"),
            ("night_vision", "redstone", "long_night_vision"),
            ("night_vision", "fermented_spider_eye", "invisibility"),
            ("long_night_vision", "fermented_spider_eye", "long_invisibility"),
            ("invisibility", "redstone", "long_invisibility"),
        ]);
        start(&mut v, "magma_cream", "fire_resistance");
        v.push(("fire_resistance", "redstone", "long_fire_resistance"));
        start(&mut v, "rabbit_foot", "leaping");
        v.extend([
            ("leaping", "redstone", "long_leaping"),
            ("leaping", "glowstone_dust", "strong_leaping"),
            ("leaping", "fermented_spider_eye", "slowness"),
            ("long_leaping", "fermented_spider_eye", "long_slowness"),
            ("slowness", "redstone", "long_slowness"),
            ("slowness", "glowstone_dust", "strong_slowness"),
            ("awkward", "turtle_helmet", "turtle_master"),
            ("turtle_master", "redstone", "long_turtle_master"),
            ("turtle_master", "glowstone_dust", "strong_turtle_master"),
            ("swiftness", "fermented_spider_eye", "slowness"),
            ("long_swiftness", "fermented_spider_eye", "long_slowness"),
        ]);
        start(&mut v, "sugar", "swiftness");
        v.extend([
            ("swiftness", "redstone", "long_swiftness"),
            ("swiftness", "glowstone_dust", "strong_swiftness"),
            ("awkward", "pufferfish", "water_breathing"),
            ("water_breathing", "redstone", "long_water_breathing"),
        ]);
        start(&mut v, "glistering_melon_slice", "healing");
        v.extend([
            ("healing", "glowstone_dust", "strong_healing"),
            ("healing", "fermented_spider_eye", "harming"),
            ("strong_healing", "fermented_spider_eye", "strong_harming"),
            ("harming", "glowstone_dust", "strong_harming"),
            ("poison", "fermented_spider_eye", "harming"),
            ("long_poison", "fermented_spider_eye", "harming"),
            ("strong_poison", "fermented_spider_eye", "strong_harming"),
        ]);
        start(&mut v, "spider_eye", "poison");
        v.extend([("poison", "redstone", "long_poison"), ("poison", "glowstone_dust", "strong_poison")]);
        start(&mut v, "ghast_tear", "regeneration");
        v.extend([("regeneration", "redstone", "long_regeneration"), ("regeneration", "glowstone_dust", "strong_regeneration")]);
        start(&mut v, "blaze_powder", "strength");
        v.extend([
            ("strength", "redstone", "long_strength"),
            ("strength", "glowstone_dust", "strong_strength"),
            ("water", "fermented_spider_eye", "weakness"),
            ("weakness", "redstone", "long_weakness"),
            ("awkward", "phantom_membrane", "slow_falling"),
            ("slow_falling", "redstone", "long_slow_falling"),
        ]);
        v
    })
}

/// container mixes: (from item, ingredient, to item)
const CONTAINER_MIXES: [(&str, &str, &str); 2] = [("potion", "gunpowder", "splash_potion"), ("splash_potion", "dragon_breath", "lingering_potion")];

fn is_potion_item(n: &str) -> bool {
    matches!(n, "potion" | "splash_potion" | "lingering_potion")
}

/// `PotionBrewing.isIngredient`
pub fn is_ingredient(item: i32) -> bool {
    let n = Items::get().name(item);
    CONTAINER_MIXES.iter().any(|m| m.1 == n) || potion_mixes().iter().any(|m| m.1 == n)
}

/// `PotionBrewing.mix`: the brewed bottle, or None when there is no mix.
pub fn mix(ingredient: Stack, bottle: Stack) -> Option<Stack> {
    let items = Items::get();
    let ing = items.name(ingredient.item);
    let bn = items.name(bottle.item);
    if !is_potion_item(bn) {
        return None;
    }
    let potion = bottle.patch().potion.clone()?;
    for (from, i, to) in CONTAINER_MIXES {
        if from == bn && i == ing {
            return items.id(to).map(|id| Stack { item: id, count: bottle.count, comp: bottle.comp });
        }
    }
    for (from, i, to) in potion_mixes() {
        if *from == potion && *i == ing {
            let to = to.to_string();
            return Some(bottle.with_patch(|p| p.potion = Some(to)));
        }
    }
    None
}

fn tag_int(t: &Tag, k: &str) -> i32 {
    t.get(k).and_then(|v| v.as_i64()).unwrap_or(0) as i32
}

fn tag_put(t: &mut Tag, k: &str, v: Tag) {
    if let Tag::Compound(c) = t {
        match c.iter_mut().find(|(n, _)| n == k) {
            Some(e) => e.1 = v,
            None => c.push((k.to_string(), v)),
        }
    }
}

/// (brew time, fuel) of a brewing stand
pub fn brewing_props(g: &Game, p: [i32; 3]) -> Vec<i32> {
    match g.world.block_entity(p) {
        Some(t) => vec![tag_int(&t, "BrewTime"), tag_int(&t, "Fuel")],
        None => vec![0, 0],
    }
}

/// `BrewingStandBlockEntity.serverTick`; false once the stand is gone.
fn brewing_tick(g: &Game, p: [i32; 3]) -> bool {
    let s = g.bs(p);
    if s.name() != "brewing_stand" {
        return false;
    }
    let Some(mut t) = g.world.block_entity(p) else { return false };
    let mut slots = blockentity::items(&t, 5);
    let mut brew = tag_int(&t, "BrewTime");
    let mut fuel = tag_int(&t, "Fuel");
    let mut changed = false;
    let mut slots_changed = false;
    if fuel <= 0 {
        if let Some(f) = slots[4].filter(|f| Items::get().name(f.item) == "blaze_powder") {
            fuel = 20;
            slots[4] = (f.count > 1).then(|| Stack { count: f.count - 1, ..f });
            changed = true;
            slots_changed = true;
        }
    }
    let ingredient = slots[3];
    let brewable = ingredient.is_some_and(|i| is_ingredient(i.item) && slots[..3].iter().flatten().any(|b| mix(i, *b).is_some()));
    let started = t.get("rime_ingredient").and_then(|v| v.as_i64()).map(|v| v as i32);
    if brew > 0 {
        brew -= 1;
        if brew == 0 && brewable {
            let ing = ingredient.expect("brewable");
            for b in slots.iter_mut().take(3) {
                if let Some(bt) = *b {
                    if let Some(r) = mix(ing, bt) {
                        *b = Some(r);
                    }
                }
            }
            let rem = if Items::get().name(ing.item) == "dragon_breath" { Items::get().id("glass_bottle") } else { None };
            slots[3] = (ing.count > 1).then(|| Stack { count: ing.count - 1, ..ing });
            if let Some(r) = rem {
                let st = Stack::new(r, 1);
                if slots[3].is_none() {
                    slots[3] = Some(st);
                } else {
                    g.spawn_item([p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5], st, [0.0, 0.1, 0.0], 10);
                }
            }
            g.level_event(1035, p, 0);
            slots_changed = true;
        } else if !brewable || ingredient.map(|i| i.item) != started {
            brew = 0;
        }
        changed = true;
    } else if brewable && fuel > 0 {
        fuel -= 1;
        brew = 400;
        tag_put(&mut t, "rime_ingredient", Tag::Int(ingredient.map(|i| i.item).unwrap_or(0)));
        changed = true;
    }
    // bottle bits
    let mut ns: BlockState = s;
    for i in 0..3 {
        ns = with_bool(ns, &format!("has_bottle_{i}"), slots[i].is_some());
    }
    if ns != s {
        g.set_block(p, ns, flags::CLIENTS);
    }
    if changed {
        tag_put(&mut t, "BrewTime", Tag::Short(brew as i16));
        tag_put(&mut t, "Fuel", Tag::Byte(fuel as i8));
        blockentity::set_items(&mut t, &slots);
        g.world.set_block_entity(p, Some(t), false);
        if slots_changed {
            g.container_changed(p, &slots, -1);
        }
    }
    let props = vec![brew, fuel];
    for pl in g.players() {
        if matches!(pl.inv.lock().window, Window::Container(_, Screen::Brewing, q) if q == p) {
            g.send_props(&pl, &props);
        }
    }
    true
}

// ------------------------------------------------------------ beacon

/// `BeaconBlockEntity.BEACON_EFFECTS`
pub const BEACON_EFFECTS: [&[Effect]; 4] = [&[Effect::Speed, Effect::Haste], &[Effect::Resistance, Effect::JumpBoost], &[Effect::Strength], &[Effect::Regeneration]];

fn beacon_effect(t: &Tag, k: &str) -> Option<Effect> {
    t.get(k).and_then(|v| v.as_str()).and_then(Effect::from_name)
}

/// pyramid levels under a beacon (`updateBase`)
fn beacon_levels(g: &Game, p: [i32; 3]) -> i32 {
    let (min_y, _) = super::blockhelp::chunk_min_max_y();
    let mut levels = 0;
    for i in 1..=4 {
        let y = p[1] - i;
        if y < min_y {
            break;
        }
        let ok = (p[0] - i..=p[0] + i).all(|x| (p[2] - i..=p[2] + i).all(|z| g.data.block_in("beacon_base_blocks", g.bs([x, y, z]).name())));
        if !ok {
            break;
        }
        levels = i;
    }
    levels
}

fn beam_clear(g: &Game, p: [i32; 3]) -> bool {
    let (_, max_y) = super::blockhelp::chunk_min_max_y();
    (p[1] + 1..max_y).all(|y| {
        let b = g.bs([p[0], y, p[2]]);
        !b.is_opaque() || b.name() == "bedrock"
    })
}

/// beacon props for the menu: levels, primary, secondary (registry id + 1)
pub fn beacon_props(g: &Game, p: [i32; 3]) -> Vec<i32> {
    let levels = g.lv(|l| l.beacons.get(&p).copied()).unwrap_or_else(|| if beam_clear(g, p) { beacon_levels(g, p) } else { 0 });
    let t = g.world.block_entity(p);
    let enc = |k: &str| t.as_ref().and_then(|t| beacon_effect(t, k)).map(|e| e.id() + 1).unwrap_or(0);
    vec![levels, enc("primary_effect"), enc("secondary_effect")]
}

fn beacon_tick(g: &Game, p: [i32; 3]) -> bool {
    if g.bs(p).name() != "beacon" {
        return false;
    }
    let before = g.lv(|l| l.beacons.get(&p).copied()).unwrap_or(0);
    let levels = if beam_clear(g, p) { beacon_levels(g, p) } else { 0 };
    g.lv(|l| l.beacons.insert(p, levels));
    let c = [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5];
    if levels > 0 && levels != before {
        // CONSTRUCT_BEACON for players near the beacon
        for pl in g.players() {
            let q = pl.pose.lock().pos;
            if (q[0] - c[0]).abs() <= 10.5 && (q[1] - c[1]).abs() <= 5.5 && (q[2] - c[2]).abs() <= 10.5 {
                g.trigger_beacon(&pl, levels);
            }
        }
    }
    if levels > 0 {
        g.play_sound("minecraft:block.beacon.ambient", 4, c, 1.0, 1.0);
        if before == 0 {
            g.play_sound("minecraft:block.beacon.activate", 4, c, 1.0, 1.0);
        }
    } else if before > 0 {
        g.play_sound("minecraft:block.beacon.deactivate", 4, c, 1.0, 1.0);
    }
    if levels > 0 {
        let Some(t) = g.world.block_entity(p) else { return true };
        let primary = beacon_effect(&t, "primary_effect");
        let secondary = beacon_effect(&t, "secondary_effect");
        if let Some(pe) = primary {
            let range = (levels * 10 + 10) as f64;
            let amp = if levels >= 4 && Some(pe) == secondary { 1 } else { 0 };
            let dur = (9 + levels * 2) * 20;
            for pl in g.players() {
                let pos = pl.pose.lock().pos;
                let inside = pos[0] >= p[0] as f64 - range
                    && pos[0] <= p[0] as f64 + 1.0 + range
                    && pos[2] >= p[2] as f64 - range
                    && pos[2] <= p[2] as f64 + 1.0 + range
                    && pos[1] >= p[1] as f64 - range;
                if !inside {
                    continue;
                }
                let mut i = Instance::new(pe, dur, amp);
                i.ambient = true;
                g.add_effect(pl.eid, i);
                if let Some(se) = secondary.filter(|s| levels >= 4 && Some(*s) != primary) {
                    let mut i = Instance::new(se, dur, 0);
                    i.ambient = true;
                    g.add_effect(pl.eid, i);
                }
            }
        }
    }
    let props = beacon_props(g, p);
    for pl in g.players() {
        if matches!(pl.inv.lock().window, Window::Container(_, Screen::Beacon, q) if q == p) {
            g.send_props(&pl, &props);
        }
    }
    true
}

/// `BeaconMenu.updateEffects` (from `set_beacon`): pays with the item in the
/// payment slot.
pub fn set_beacon(g: &Game, pl: &Arc<Player>, primary: Option<i32>, secondary: Option<i32>) {
    let (p, paid) = {
        let mut inv = pl.inv.lock();
        let Window::Container(_, Screen::Beacon, p) = inv.window else { return };
        if inv.get_w(0).is_none() {
            return;
        }
        let levels = beacon_props(g, p)[0];
        let valid = |e: Option<i32>, tier_max: i32| -> bool {
            match e {
                None => true,
                Some(id) => Effect::from_id(id).is_some_and(|e| BEACON_EFFECTS.iter().take(tier_max as usize).any(|t| t.contains(&e))),
            }
        };
        let pe = primary.and_then(Effect::from_id);
        let se = secondary.and_then(Effect::from_id);
        let sec_ok = match se {
            None => true,
            Some(s) => levels >= 4 && (s == Effect::Regeneration || Some(s) == pe),
        };
        if pe.is_none() || !valid(primary, levels) || !sec_ok {
            return;
        }
        let mut st = inv.get_w(0);
        if let Some(s) = st.as_mut() {
            s.count -= 1;
        }
        inv.set_w(0, st.filter(|s| s.count > 0));
        (p, (pe, se))
    };
    let Some(mut t) = g.world.block_entity(p).or_else(|| blockentity::be_id("beacon").map(|id| blockentity::new_tag(id, p))) else { return };
    if let Some(e) = paid.0 {
        tag_put(&mut t, "primary_effect", Tag::String(format!("minecraft:{}", e.name())));
    }
    match paid.1 {
        Some(e) => tag_put(&mut t, "secondary_effect", Tag::String(format!("minecraft:{}", e.name()))),
        None => {
            if let Tag::Compound(c) = &mut t {
                c.retain(|(k, _)| k != "secondary_effect");
            }
        }
    }
    g.world.set_block_entity(p, Some(t), false);
    g.lv(|l| {
        l.brewers.insert(p);
    });
    g.flush_inventory(pl);
    g.send_props(pl, &beacon_props(g, p));
}

// ------------------------------------------------------------ ticking

/// Called every tick from `tick_block_entities`.
pub fn tick(g: &Game) {
    let now = g.game_time();
    if now % 20 == 2 {
        let list = g.world.block_entities_with(&["brewing_stand", "beacon"]);
        g.lv(|l| {
            for p in list {
                // the kind is decided by the block when ticking
                l.brewers.insert(p);
            }
        });
    }
    let list: Vec<[i32; 3]> = g.lv(|l| l.brewers.iter().copied().collect());
    for p in list {
        let alive = match g.bs(p).name() {
            "brewing_stand" => brewing_tick(g, p),
            "beacon" => now % 80 != 0 || beacon_tick(g, p),
            _ => false,
        };
        if !alive {
            g.lv(|l| {
                l.brewers.remove(&p);
                l.beacons.remove(&p);
            });
        }
    }
}

pub fn ticker_placed(g: &Game, p: [i32; 3], s: BlockState) {
    if matches!(s.name(), "brewing_stand" | "beacon") {
        g.lv(|l| l.brewers.insert(p));
    }
}

// ------------------------------------------------------------ enchanting table

/// `EnchantingTableBlock.BOOKSHELF_OFFSETS` / `isValidBookShelf`
pub fn bookshelves(g: &Game, p: [i32; 3]) -> i32 {
    let mut n = 0;
    for dx in -2..=2i32 {
        for dz in -2..=2i32 {
            if dx.abs() != 2 && dz.abs() != 2 {
                continue;
            }
            for dy in 0..=1 {
                let q = [p[0] + dx, p[1] + dy, p[2] + dz];
                let mid = [p[0] + dx / 2, p[1] + dy / 2, p[2] + dz / 2];
                if g.data.block_in("enchantment_power_provider", g.bs(q).name()) && g.data.block_in("enchantment_power_transmitter", g.bs(mid).name()) {
                    n += 1;
                }
            }
        }
    }
    n
}

// ------------------------------------------------------------ anvil

/// `AnvilMenu.onTake`: 12% chance to damage the anvil.
pub fn anvil_used(g: &Game, p: [i32; 3], creative: bool) {
    let s = g.bs(p);
    let n = s.name();
    if !matches!(n, "anvil" | "chipped_anvil" | "damaged_anvil") {
        return;
    }
    if !creative && rand::random::<f32>() < 0.12 {
        let next = match n {
            "anvil" => Some("chipped_anvil"),
            "chipped_anvil" => Some("damaged_anvil"),
            _ => None,
        };
        match next.map(BlockState::by_name) {
            Some(b) => {
                let ns = super::block::with_facing(b, super::block::facing(s));
                g.set_block(p, ns, flags::CLIENTS);
                g.level_event(1030, p, 0);
            }
            None => {
                g.set_block(p, crate::registry::blocks::AIR, flags::ALL);
                g.level_event(1029, p, 0);
                // the open menus close on the next click
            }
        }
    } else {
        g.level_event(1030, p, 0);
    }
}

// ------------------------------------------------------------ recipes packet

fn holder_set(b: &mut Vec<u8>, items: &[i32]) {
    b.vi(items.len() as i32 + 1);
    for i in items {
        b.vi(*i);
    }
}

fn sorted(set: impl IntoIterator<Item = i32>) -> Vec<i32> {
    let mut v: Vec<i32> = set.into_iter().collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// `ClientboundUpdateRecipesPacket` body: property sets and stonecutter recipes.
pub fn update_recipes(data: &GameData) -> Vec<u8> {
    use crate::data::Smithing;
    let mut sets: Vec<(&str, Vec<i32>)> = Vec::new();
    let pick = |f: &dyn Fn(&Smithing) -> &crate::data::Ingredient| sorted(data.menus.smithing.iter().flat_map(|r| f(r).0.iter().copied()));
    sets.push(("minecraft:smithing_template", pick(&|r| match r {
        Smithing::Transform { template, .. } | Smithing::Trim { template, .. } => template,
    })));
    sets.push(("minecraft:smithing_base", pick(&|r| match r {
        Smithing::Transform { base, .. } | Smithing::Trim { base, .. } => base,
    })));
    sets.push(("minecraft:smithing_addition", pick(&|r| match r {
        Smithing::Transform { addition, .. } | Smithing::Trim { addition, .. } => addition,
    })));
    for (key, kind) in [("minecraft:furnace_input", CookKind::Smelting), ("minecraft:blast_furnace_input", CookKind::Blasting), ("minecraft:smoker_input", CookKind::Smoking), ("minecraft:campfire_input", CookKind::Campfire)] {
        sets.push((key, sorted(data.cooking.iter().filter(|c| c.kind == kind).flat_map(|c| c.input.0.iter().copied()))));
    }
    let mut b = Vec::new();
    b.vi(sets.len() as i32);
    for (k, items) in &sets {
        b.str(k).vi(items.len() as i32);
        for i in items {
            b.vi(*i);
        }
    }
    b.vi(data.menus.stonecutting.len() as i32);
    for (ing, res) in &data.menus.stonecutting {
        holder_set(&mut b, &sorted(ing.0.iter().copied()));
        // SlotDisplay item_stack (5): item, count, empty patch
        b.vi(5).vi(res.item).vi(res.count as i32).vi(0).vi(0);
    }
    b
}
