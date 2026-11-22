//! Workstation menus: `EnchantmentMenu`, `AnvilMenu`, `SmithingMenu`,
//! `GrindstoneMenu`, `StonecutterMenu`, `LoomMenu`, `CartographyTableMenu`,
//! `BeaconMenu` (slots and results; the brewing stand and beacon block
//! entities tick in `workstation.rs`).

use serde_json::Value;

use super::blockentity::Screen;
use super::combat::enchant::Enchantments;
use super::inventory::{Inventory, Window};
use crate::components::{self, dye_id, Patch};
use crate::data::{GameData, Items, Smithing, Stack};
use crate::worldgen::random::{Legacy, Rng as JRng};

/// Side effects of a menu action, applied by the session.
#[derive(Clone, Debug)]
pub enum MenuEvent {
    /// add (negative: remove) experience levels
    Levels(i32),
    /// `onEnchantmentPerformed`: levels spent and a new enchantment seed
    Enchanted(i32),
    Sound(&'static str),
    /// grindstone experience orbs
    XpOrbs(i32),
    /// anvil used: may get damaged
    AnvilUsed,
    Stat(&'static str),
}

#[derive(Clone, Debug, Default)]
pub struct MenuState {
    pub pos: [i32; 3],
    pub xp_level: i32,
    pub creative: bool,
    pub events: Vec<MenuEvent>,
    /// `container_set_data` values (index = property)
    pub props: Vec<i32>,
    // anvil
    pub rename: Option<String>,
    pub cost: i32,
    pub repair_count: i32,
    // enchanting
    pub seed: i32,
    pub bookshelves: i32,
    // stonecutter / loom
    pub selected: i32,
    pub last_input: Option<i32>,
    pub list: Vec<usize>,
    pub patterns: Vec<String>,
}

/// window slot of a screen's result
pub fn result_slot(sc: Screen) -> Option<usize> {
    match sc {
        Screen::Anvil | Screen::Grindstone | Screen::Cartography => Some(2),
        Screen::Smithing | Screen::Loom => Some(3),
        Screen::Stonecutter => Some(1),
        _ => None,
    }
}

pub fn is_menu(sc: Screen) -> bool {
    matches!(sc, Screen::Enchanting | Screen::Anvil | Screen::Smithing | Screen::Grindstone | Screen::Stonecutter | Screen::Loom | Screen::Cartography | Screen::Beacon)
}

fn screen(inv: &Inventory) -> Option<Screen> {
    match inv.window {
        Window::Container(_, sc, _) => Some(sc),
        _ => None,
    }
}

fn name(st: &Stack) -> &'static str {
    Items::get().name(st.item)
}

fn get(inv: &Inventory, i: usize) -> Option<Stack> {
    inv.get_w(i)
}

fn set(inv: &mut Inventory, i: usize, s: Option<Stack>) {
    inv.set_w(i, s)
}

fn shrink(inv: &mut Inventory, i: usize, n: u8) {
    if let Some(mut s) = get(inv, i) {
        s.count = s.count.saturating_sub(n);
        set(inv, i, (s.count > 0).then_some(s));
    }
}

// ------------------------------------------------------------ item helpers

fn max_damage(st: &Stack) -> i32 {
    components::defaults(st.item).max_damage.unwrap_or(0)
}

fn damageable(st: &Stack) -> bool {
    max_damage(st) > 0 && !st.patch().unbreakable
}

fn damage(st: &Stack) -> i32 {
    st.patch().damage.unwrap_or(0)
}

fn with(st: Stack, f: impl FnOnce(&mut Patch)) -> Stack {
    st.with_patch(f)
}

fn enchants_for_crafting(st: &Stack) -> Vec<(String, u8)> {
    let p = st.patch();
    if name(st) == "enchanted_book" {
        p.stored_enchantments.clone()
    } else {
        p.enchantments.clone()
    }
}

fn set_enchants(st: Stack, list: Vec<(String, u8)>) -> Stack {
    let book = name(&st) == "enchanted_book";
    with(st, |p| {
        if book {
            p.stored_enchantments = list;
        } else {
            p.enchantments = list;
        }
    })
}

fn repair_cost(st: &Stack) -> i32 {
    st.patch().repair_cost.unwrap_or(0)
}

/// `AnvilMenu.calculateIncreasedRepairCost`
pub fn increased_repair_cost(c: i32) -> i32 {
    ((c as i64) * 2 + 1).min(i32::MAX as i64) as i32
}

/// the item set of a holder set JSON value (`#tag`, `id`, or a list)
fn set_contains(v: &Value, item: i32, data: &GameData) -> bool {
    match v {
        Value::String(s) => {
            if let Some(t) = s.strip_prefix('#') {
                data.item_in(t.trim_start_matches("minecraft:"), item)
            } else {
                Items::get().id(s).is_some_and(|i| i == item)
            }
        }
        Value::Array(a) => a.iter().any(|x| set_contains(x, item, data)),
        _ => false,
    }
}

fn ench_raw(n: &str) -> Option<&'static Value> {
    Enchantments::get().by(n).map(|e| &e.raw)
}

fn ench_max(n: &str) -> i32 {
    Enchantments::get().by(n).map(|e| e.max_level).unwrap_or(1)
}

fn level_cost(v: &Value, lvl: i32) -> i32 {
    let base = v.get("base").and_then(|x| x.as_i64()).unwrap_or(1) as i32;
    let per = v.get("per_level_above_first").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
    base + per * (lvl - 1)
}

fn min_cost(n: &str, lvl: i32) -> i32 {
    ench_raw(n).map(|r| level_cost(&r["min_cost"], lvl)).unwrap_or(1)
}

fn max_cost(n: &str, lvl: i32) -> i32 {
    ench_raw(n).map(|r| level_cost(&r["max_cost"], lvl)).unwrap_or(1)
}

fn anvil_cost(n: &str) -> i32 {
    ench_raw(n).and_then(|r| r["anvil_cost"].as_i64()).unwrap_or(1) as i32
}

fn weight(n: &str) -> i32 {
    ench_raw(n).and_then(|r| r["weight"].as_i64()).unwrap_or(1) as i32
}

/// `Enchantment.canEnchant` (supported items)
fn can_enchant(n: &str, st: &Stack, data: &GameData) -> bool {
    ench_raw(n).is_some_and(|r| set_contains(&r["supported_items"], st.item, data))
}

/// `Enchantment.isPrimaryItem`
fn is_primary(n: &str, st: &Stack, data: &GameData) -> bool {
    ench_raw(n).is_some_and(|r| match r.get("primary_items") {
        Some(p) => set_contains(p, st.item, data),
        None => set_contains(&r["supported_items"], st.item, data),
    })
}

fn tag_list<'a>(data: &'a GameData, reg: &str, tag: &str) -> &'a [String] {
    data.menus.ordered_tags.get(&format!("{reg}/{}", tag.trim_start_matches("minecraft:"))).map(|v| v.as_slice()).unwrap_or(&[])
}

fn exclusive_contains(n: &str, other: &str, data: &GameData) -> bool {
    match ench_raw(n).and_then(|r| r.get("exclusive_set")) {
        Some(Value::String(s)) => match s.strip_prefix('#') {
            Some(t) => tag_list(data, "enchantment", t).iter().any(|x| x == other),
            None => s.trim_start_matches("minecraft:") == other,
        },
        Some(Value::Array(a)) => a.iter().any(|x| x.as_str().is_some_and(|s| s.trim_start_matches("minecraft:") == other)),
        _ => false,
    }
}

/// `Enchantment.areCompatible`
fn compatible(a: &str, b: &str, data: &GameData) -> bool {
    a != b && !exclusive_contains(a, b, data) && !exclusive_contains(b, a, data)
}

fn is_curse(n: &str, data: &GameData) -> bool {
    tag_list(data, "enchantment", "curse").iter().any(|x| x == n)
}

// ------------------------------------------------------------ dispatch

/// `AbstractContainerMenu.slotsChanged`
pub fn slots_changed(inv: &mut Inventory, data: &GameData) {
    let Some(sc) = screen(inv) else { return };
    match sc {
        Screen::Anvil => anvil_result(inv, data),
        Screen::Grindstone => grindstone_result(inv, data),
        Screen::Smithing => smithing_result(inv, data),
        Screen::Stonecutter => stonecutter_changed(inv, data),
        Screen::Loom => loom_changed(inv, data),
        Screen::Cartography => cartography_result(inv),
        Screen::Enchanting => enchant_changed(inv, data),
        _ => {}
    }
}

/// Can `item` go into input slot `slot`?
pub fn may_place(sc: Screen, slot: usize, item: i32, data: &GameData) -> bool {
    let n = Items::get().name(item);
    let st = Stack::new(item, 1);
    match (sc, slot) {
        (_, s) if result_slot(sc) == Some(s) => false,
        (Screen::Enchanting, 1) => n == "lapis_lazuli",
        (Screen::Enchanting, 0) => true,
        (Screen::Grindstone, 0 | 1) => damageable(&st) || n == "enchanted_book" || !components::defaults(item).raw.get("enchantments").is_none(),
        (Screen::Smithing, 0) => data.menus.smithing.iter().any(|r| match r {
            Smithing::Transform { template, .. } | Smithing::Trim { template, .. } => template.0.contains(&item),
        }),
        (Screen::Smithing, 1) => data.menus.smithing.iter().any(|r| match r {
            Smithing::Transform { base, .. } | Smithing::Trim { base, .. } => base.0.contains(&item),
        }),
        (Screen::Smithing, 2) => data.menus.smithing.iter().any(|r| match r {
            Smithing::Transform { addition, .. } | Smithing::Trim { addition, .. } => addition.0.contains(&item),
        }),
        (Screen::Loom, 0) => n.ends_with("_banner"),
        (Screen::Loom, 1) => n.ends_with("_dye"),
        (Screen::Loom, 2) => components::defaults(item).raw.get("provides_banner_patterns").is_some(),
        (Screen::Cartography, 0) => n == "filled_map",
        (Screen::Cartography, 1) => matches!(n, "paper" | "map" | "glass_pane"),
        (Screen::Beacon, 0) => data.item_in("beacon_payment_items", item),
        _ => true,
    }
}

/// Shift-click from the player inventory: the input slots to try.
pub fn quick_input(sc: Screen, item: i32, data: &GameData) -> Vec<usize> {
    let size = sc.size();
    (0..size).filter(|&s| result_slot(sc) != Some(s) && may_place(sc, s, item, data)).collect()
}

/// `Slot.mayPickup` of the result slot
pub fn may_take(inv: &Inventory) -> bool {
    match screen(inv) {
        Some(Screen::Anvil) => (inv.menu.creative || inv.menu.xp_level >= inv.menu.cost) && inv.menu.cost > 0,
        _ => true,
    }
}

/// Take the result: consume inputs (`onTake`), return the result stack.
pub fn take(inv: &mut Inventory, data: &GameData) -> Option<Stack> {
    let sc = screen(inv)?;
    let rs = result_slot(sc)?;
    let r = get(inv, rs)?;
    if !may_take(inv) {
        return None;
    }
    match sc {
        Screen::Anvil => {
            if !inv.menu.creative {
                inv.menu.events.push(MenuEvent::Levels(-inv.menu.cost));
            }
            set(inv, 0, None);
            if inv.menu.repair_count > 0 {
                let n = inv.menu.repair_count as u8;
                shrink(inv, 1, n);
            } else {
                set(inv, 1, None);
            }
            inv.menu.cost = 0;
            inv.menu.events.push(MenuEvent::AnvilUsed);
        }
        Screen::Grindstone => {
            let xp = grindstone_xp(get(inv, 0), get(inv, 1), data);
            if xp > 0 {
                inv.menu.events.push(MenuEvent::XpOrbs(xp));
            }
            set(inv, 0, None);
            set(inv, 1, None);
            inv.menu.events.push(MenuEvent::Sound("minecraft:block.grindstone.use"));
        }
        Screen::Smithing => {
            for i in 0..3 {
                shrink(inv, i, 1);
            }
            inv.menu.events.push(MenuEvent::Sound("minecraft:block.smithing_table.use"));
        }
        Screen::Stonecutter => {
            shrink(inv, 0, 1);
            inv.menu.events.push(MenuEvent::Sound("minecraft:ui.stonecutter.take_result"));
        }
        Screen::Loom => {
            shrink(inv, 0, 1);
            shrink(inv, 1, 1);
            inv.menu.events.push(MenuEvent::Sound("minecraft:ui.loom.take_result"));
        }
        Screen::Cartography => {
            shrink(inv, 0, 1);
            shrink(inv, 1, 1);
            inv.menu.events.push(MenuEvent::Sound("minecraft:ui.cartography_table.take_result"));
        }
        _ => return None,
    }
    set(inv, rs, None);
    slots_changed(inv, data);
    Some(r)
}

/// `clickMenuButton`
pub fn button(inv: &mut Inventory, data: &GameData, id: i32) -> bool {
    match screen(inv) {
        Some(Screen::Enchanting) => enchant_button(inv, data, id),
        Some(Screen::Stonecutter) => {
            if id >= 0 && (id as usize) < inv.menu.list.len() {
                inv.menu.selected = id;
                stonecutter_result(inv, data);
                true
            } else {
                false
            }
        }
        Some(Screen::Loom) => {
            if id >= 0 && (id as usize) < inv.menu.patterns.len() {
                inv.menu.selected = id;
                loom_result(inv);
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

// ------------------------------------------------------------ anvil

/// `AnvilMenu.createResult`
fn anvil_result(inv: &mut Inventory, data: &GameData) {
    let creative = inv.menu.creative;
    let mut out = |inv: &mut Inventory, r: Option<Stack>, cost: i32, count: i32| {
        inv.menu.cost = cost;
        inv.menu.repair_count = count;
        set(inv, 2, r);
        inv.menu.props = vec![cost];
    };
    let Some(input) = get(inv, 0) else {
        out(inv, None, 0, 0);
        return;
    };
    let mut result = input;
    let right = get(inv, 1);
    let mut i: i64 = 0;
    let mut j: i64 = repair_cost(&input) as i64 + right.map(|r| repair_cost(&r) as i64).unwrap_or(0);
    let mut k = 0i64;
    let mut repair_count = 0;
    let mut enchants = enchants_for_crafting(&result);
    if let Some(right) = right {
        let is_book = !right.patch().stored_enchantments.is_empty();
        let repairable = components::defaults(input.item).raw.get("repairable").map(|r| set_contains(&r["items"], right.item, data)).unwrap_or(false);
        if damageable(&result) && repairable {
            let mut l = damage(&result).min(max_damage(&result) / 4);
            if l <= 0 {
                out(inv, None, 0, 0);
                return;
            }
            let mut n = 0;
            while l > 0 && n < right.count as i32 {
                let d = damage(&result) - l;
                result = with(result, |p| p.damage = Some(d));
                i += 1;
                l = damage(&result).min(max_damage(&result) / 4);
                n += 1;
            }
            repair_count = n;
        } else {
            if !is_book && (result.item != right.item || !damageable(&result)) {
                out(inv, None, 0, 0);
                return;
            }
            if damageable(&result) && !is_book {
                let m = max_damage(&input);
                let l = m - damage(&input);
                let i1 = max_damage(&right) - damage(&right);
                let j1 = i1 + m * 12 / 100;
                let k1 = l + j1;
                let l1 = (m - k1).max(0);
                if l1 < damage(&result) {
                    result = with(result, |p| p.damage = Some(l1));
                    i += 2;
                }
            }
            let right_ench = enchants_for_crafting(&right);
            let mut applied = false;
            let mut incompatible = false;
            for (en, lvl) in right_ench {
                let cur = enchants.iter().find(|(n, _)| *n == en).map(|(_, l)| *l as i32).unwrap_or(0);
                let mut j2 = if cur == lvl as i32 { lvl as i32 + 1 } else { (lvl as i32).max(cur) };
                let mut ok = can_enchant(&en, &input, data);
                if creative || name(&input) == "enchanted_book" {
                    ok = true;
                }
                for (other, _) in &enchants {
                    if *other != en && !compatible(&en, other, data) {
                        ok = false;
                        i += 1;
                    }
                }
                if !ok {
                    incompatible = true;
                } else {
                    applied = true;
                    if j2 > ench_max(&en) {
                        j2 = ench_max(&en);
                    }
                    match enchants.iter_mut().find(|(n, _)| *n == en) {
                        Some(e) => e.1 = j2 as u8,
                        None => enchants.push((en.clone(), j2 as u8)),
                    }
                    let mut k3 = anvil_cost(&en);
                    if is_book {
                        k3 = (k3 / 2).max(1);
                    }
                    i += (k3 * j2) as i64;
                    if input.count > 1 {
                        i = 40;
                    }
                }
            }
            if incompatible && !applied {
                out(inv, None, 0, 0);
                return;
            }
        }
    }
    let cur_name = input.patch().custom_name.clone();
    match inv.menu.rename.as_deref().map(str::trim) {
        None | Some("") => {
            if cur_name.is_some() {
                k = 1;
                i += k;
                result = with(result, |p| p.custom_name = None);
            }
        }
        Some(n) => {
            let shown = cur_name.clone().unwrap_or_else(|| default_name(&input));
            if n != shown {
                k = 1;
                i += k;
                let n = n.to_string();
                result = with(result, |p| p.custom_name = Some(n));
            }
        }
    }
    let mut cost = (j + i).clamp(0, i32::MAX as i64) as i32;
    let mut res = Some(result);
    if i <= 0 {
        res = None;
    }
    if k == i && k > 0 && cost >= 40 {
        cost = 39;
    }
    if cost >= 40 && !creative {
        res = None;
    }
    if let Some(r) = res {
        let mut k2 = repair_cost(&r);
        if let Some(right) = right {
            k2 = k2.max(repair_cost(&right));
        }
        if k != i || k == 0 {
            k2 = increased_repair_cost(k2);
        }
        let r = with(r, |p| p.repair_cost = (k2 > 0).then_some(k2));
        res = Some(set_enchants(r, enchants));
    }
    out(inv, res, cost, repair_count);
}

/// the item's English name as the client shows it (rename comparison)
fn default_name(st: &Stack) -> String {
    name(st)
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------------------ grindstone

fn strip_non_curses(st: Stack, data: &GameData) -> Stack {
    let keep: Vec<(String, u8)> = enchants_for_crafting(&st).into_iter().filter(|(n, _)| is_curse(n, data)).collect();
    let mut r = set_enchants(st, keep.clone());
    if name(&r) == "enchanted_book" && keep.is_empty() {
        if let Some(b) = Items::get().id("book") {
            r = Stack { item: b, count: r.count, comp: r.comp };
            r = with(r, |p| p.stored_enchantments.clear());
        }
    }
    let mut cost = 0;
    for _ in 0..keep.len() {
        cost = increased_repair_cost(cost);
    }
    with(r, |p| p.repair_cost = (cost > 0).then_some(cost))
}

/// `GrindstoneMenu.computeResult`
fn grindstone_result(inv: &mut Inventory, data: &GameData) {
    let (a, b) = (get(inv, 0), get(inv, 1));
    let r = match (a, b) {
        (None, None) => None,
        (Some(x), None) | (None, Some(x)) => {
            if enchants_for_crafting(&x).is_empty() {
                None
            } else {
                Some(strip_non_curses(Stack { count: x.count, ..x }, data))
            }
        }
        (Some(x), Some(y)) => {
            if x.item != y.item || x.count > 1 || y.count > 1 || !damageable(&x) {
                None
            } else {
                let m = max_damage(&x);
                let total = (m - damage(&x)) + (m - damage(&y)) + m * 5 / 100;
                let dmg = (m - total).max(0);
                let mut ench = enchants_for_crafting(&x);
                for (n, l) in enchants_for_crafting(&y) {
                    if is_curse(&n, data) {
                        match ench.iter_mut().find(|(e, _)| *e == n) {
                            Some(e) => e.1 = e.1.max(l),
                            None => ench.push((n, l)),
                        }
                    }
                }
                let merged = set_enchants(with(x, |p| p.damage = Some(dmg)), ench);
                Some(strip_non_curses(merged, data))
            }
        }
    };
    set(inv, 2, r);
}

/// `GrindstoneMenu.getExperienceAmount`
fn grindstone_xp(a: Option<Stack>, b: Option<Stack>, data: &GameData) -> i32 {
    let mut i = 0;
    for st in [a, b].into_iter().flatten() {
        for (n, l) in enchants_for_crafting(&st) {
            if !is_curse(&n, data) {
                i += min_cost(&n, l as i32);
            }
        }
    }
    if i > 0 {
        let l = (i as f64 / 2.0).ceil() as i32;
        l + rand::random::<u32>() as i32 % l.max(1)
    } else {
        0
    }
}

// ------------------------------------------------------------ smithing

fn trim_material(addition: &Stack) -> Option<String> {
    components::defaults(addition.item).raw.get("provides_trim_material").and_then(|v| v.as_str()).map(|s| s.trim_start_matches("minecraft:").to_string())
}

/// `SmithingMenu.createResult`
fn smithing_result(inv: &mut Inventory, data: &GameData) {
    let (t, b, a) = (get(inv, 0), get(inv, 1), get(inv, 2));
    let mut r = None;
    if let (Some(t), Some(b), Some(a)) = (t, b, a) {
        for rec in &data.menus.smithing {
            match rec {
                Smithing::Transform { template, base, addition, result } => {
                    if template.0.contains(&t.item) && base.0.contains(&b.item) && addition.0.contains(&a.item) {
                        r = Some(Stack { item: result.item, count: result.count, comp: b.comp });
                        break;
                    }
                }
                Smithing::Trim { template, base, addition, pattern } => {
                    if template.0.contains(&t.item) && base.0.contains(&b.item) && addition.0.contains(&a.item) {
                        if let Some(m) = trim_material(&a) {
                            let new = Some((m, pattern.clone()));
                            if b.patch().trim != new {
                                r = Some(with(Stack { count: 1, ..b }, |p| p.trim = new));
                            }
                        }
                        break;
                    }
                }
            }
        }
    }
    set(inv, 3, r);
}

// ------------------------------------------------------------ stonecutter

fn stonecutter_changed(inv: &mut Inventory, data: &GameData) {
    let input = get(inv, 0).map(|s| s.item);
    if input != inv.menu.last_input {
        inv.menu.last_input = input;
        inv.menu.selected = -1;
        inv.menu.list = match input {
            Some(i) => data.menus.stonecutting.iter().enumerate().filter(|(_, (ing, _))| ing.0.contains(&i)).map(|(k, _)| k).collect(),
            None => Vec::new(),
        };
        set(inv, 1, None);
    } else {
        stonecutter_result(inv, data);
    }
    inv.menu.props = vec![inv.menu.selected];
}

fn stonecutter_result(inv: &mut Inventory, data: &GameData) {
    let sel = inv.menu.selected;
    let r = if get(inv, 0).is_some() && sel >= 0 && (sel as usize) < inv.menu.list.len() {
        data.menus.stonecutting.get(inv.menu.list[sel as usize]).map(|(_, r)| *r)
    } else {
        None
    };
    set(inv, 1, r);
    inv.menu.props = vec![sel];
}

// ------------------------------------------------------------ loom

fn selectable_patterns(pattern: Option<Stack>, data: &GameData) -> Vec<String> {
    match pattern {
        None => tag_list(data, "banner_pattern", "no_item_required").to_vec(),
        Some(p) => match components::defaults(p.item).raw.get("provides_banner_patterns").and_then(|v| v.as_str()) {
            Some(t) => tag_list(data, "banner_pattern", t.trim_start_matches('#')).to_vec(),
            None => Vec::new(),
        },
    }
}

/// `LoomMenu.slotsChanged`
fn loom_changed(inv: &mut Inventory, data: &GameData) {
    let (banner, dye, pattern) = (get(inv, 0), get(inv, 1), get(inv, 2));
    if banner.is_some() && dye.is_some() {
        let i = inv.menu.selected;
        let valid = i >= 0 && (i as usize) < inv.menu.patterns.len();
        let old = std::mem::take(&mut inv.menu.patterns);
        inv.menu.patterns = selectable_patterns(pattern, data);
        let holder = if inv.menu.patterns.len() == 1 {
            inv.menu.selected = 0;
            Some(0)
        } else if !valid {
            inv.menu.selected = -1;
            None
        } else {
            match inv.menu.patterns.iter().position(|p| *p == old[i as usize]) {
                Some(j) => {
                    inv.menu.selected = j as i32;
                    Some(j)
                }
                None => {
                    inv.menu.selected = -1;
                    None
                }
            }
        };
        match holder {
            Some(_) => {
                if banner.is_some_and(|b| b.patch().banner.len() >= 6) {
                    inv.menu.selected = -1;
                    set(inv, 3, None);
                } else {
                    loom_result(inv);
                }
            }
            None => set(inv, 3, None),
        }
    } else {
        set(inv, 3, None);
        inv.menu.patterns.clear();
        inv.menu.selected = -1;
    }
    inv.menu.props = vec![inv.menu.selected];
}

fn loom_result(inv: &mut Inventory) {
    let (Some(banner), Some(dye)) = (get(inv, 0), get(inv, 1)) else {
        set(inv, 3, None);
        return;
    };
    let sel = inv.menu.selected;
    let Some(pat) = (sel >= 0).then(|| inv.menu.patterns.get(sel as usize).cloned()).flatten() else {
        set(inv, 3, None);
        return;
    };
    let Some(color) = dye_id(name(&dye).trim_end_matches("_dye")) else {
        set(inv, 3, None);
        return;
    };
    let r = with(Stack { count: 1, ..banner }, |p| p.banner.push((pat, color)));
    set(inv, 3, Some(r));
    inv.menu.props = vec![sel];
}

// ------------------------------------------------------------ cartography

/// `CartographyTableMenu.setupResultSlot` (maps carry no map data yet:
/// copying with an empty map works, zooming and locking keep the map)
fn cartography_result(inv: &mut Inventory) {
    let (Some(map), Some(extra)) = (get(inv, 0), get(inv, 1)) else {
        set(inv, 2, None);
        return;
    };
    let r = match name(&extra) {
        "map" => Some(Stack { count: 2, ..map }),
        "paper" | "glass_pane" => Some(Stack { count: 1, ..map }),
        _ => None,
    };
    set(inv, 2, r);
}

// ------------------------------------------------------------ enchanting

/// `EnchantmentHelper.getEnchantmentCost`
fn enchantment_cost(rng: &mut Legacy, slot: i32, shelves: i32, st: &Stack) -> i32 {
    let Some(_) = components::defaults(st.item).enchantable else { return 0 };
    let bs = shelves.min(15);
    let i = rng.next_int_bound(8) + 1 + (bs >> 1) + rng.next_int_bound(bs + 1);
    match slot {
        0 => (i / 3).max(1),
        1 => i * 2 / 3 + 1,
        _ => i.max(bs * 2),
    }
}

/// `EnchantmentHelper.selectEnchantment`
fn select_enchantment(rng: &mut Legacy, st: &Stack, level: i32, possible: &[String], data: &GameData) -> Vec<(String, i32)> {
    let mut list = Vec::new();
    let Some(ench) = components::defaults(st.item).enchantable else { return list };
    let mut level = level + 1 + rng.next_int_bound(ench / 4 + 1) + rng.next_int_bound(ench / 4 + 1);
    let f = (rng.next_float() + rng.next_float() - 1.0) * 0.15;
    level = ((level as f32 + level as f32 * f).round() as i32).max(1);
    let book = name(st) == "book";
    // getAvailableEnchantmentResults
    let mut avail: Vec<(String, i32)> = Vec::new();
    for e in possible {
        if !book && !is_primary(e, st, data) {
            continue;
        }
        for lvl in (1..=ench_max(e)).rev() {
            if level >= min_cost(e, lvl) && level <= max_cost(e, lvl) {
                avail.push((e.clone(), lvl));
                break;
            }
        }
    }
    let pick = |rng: &mut Legacy, avail: &[(String, i32)]| -> Option<(String, i32)> {
        let total: i32 = avail.iter().map(|(e, _)| weight(e)).sum();
        if total <= 0 {
            return None;
        }
        let mut r = rng.next_int_bound(total);
        for x in avail {
            r -= weight(&x.0);
            if r < 0 {
                return Some(x.clone());
            }
        }
        None
    };
    if !avail.is_empty() {
        if let Some(x) = pick(rng, &avail) {
            list.push(x);
        }
        while rng.next_int_bound(50) <= level {
            if let Some(last) = list.last() {
                let last = last.0.clone();
                avail.retain(|(e, _)| compatible(&last, e, data));
            }
            if avail.is_empty() {
                break;
            }
            if let Some(x) = pick(rng, &avail) {
                list.push(x);
            }
            level /= 2;
        }
    }
    list
}

/// `EnchantmentMenu.getEnchantmentList`
fn enchantment_list(inv: &Inventory, st: &Stack, slot: i32, cost: i32, data: &GameData) -> Vec<(String, i32)> {
    let mut rng = Legacy::new(inv.menu.seed as i64 + slot as i64);
    let possible = tag_list(data, "enchantment", "in_enchanting_table");
    let mut list = select_enchantment(&mut rng, st, cost, possible, data);
    if name(st) == "book" && list.len() > 1 {
        let i = rng.next_int_bound(list.len() as i32) as usize;
        list.remove(i);
    }
    list
}

/// `EnchantmentMenu.slotsChanged`
fn enchant_changed(inv: &mut Inventory, data: &GameData) {
    let mut costs = [0i32; 3];
    let mut clue = [-1i32; 3];
    let mut lvl = [-1i32; 3];
    if let Some(st) = get(inv, 0) {
        let enchantable = components::defaults(st.item).enchantable.is_some() && st.patch().enchantments.is_empty();
        if enchantable {
            let mut rng = Legacy::new(inv.menu.seed as i64);
            for (i, c) in costs.iter_mut().enumerate() {
                *c = enchantment_cost(&mut rng, i as i32, inv.menu.bookshelves, &st);
                if *c < i as i32 + 1 {
                    *c = 0;
                }
            }
            for i in 0..3 {
                if costs[i] > 0 {
                    let list = enchantment_list(inv, &st, i as i32, costs[i], data);
                    if !list.is_empty() {
                        let k = rng.next_int_bound(list.len() as i32) as usize;
                        clue[i] = components::enchantment_names().iter().position(|n| *n == list[k].0).map(|x| x as i32).unwrap_or(-1);
                        lvl[i] = list[k].1;
                    }
                }
            }
        }
    }
    let mut p = costs.to_vec();
    p.push(inv.menu.seed & -16);
    p.extend_from_slice(&clue);
    p.extend_from_slice(&lvl);
    inv.menu.props = p;
}

fn enchant_button(inv: &mut Inventory, data: &GameData, id: i32) -> bool {
    if !(0..3).contains(&id) || inv.menu.props.len() < 3 {
        return false;
    }
    let creative = inv.menu.creative;
    let cost_levels = id + 1;
    let lapis = get(inv, 1).map(|s| s.count as i32).unwrap_or(0);
    let Some(st) = get(inv, 0) else { return false };
    let shown = inv.menu.props[id as usize];
    if (lapis < cost_levels) && !creative {
        return false;
    }
    if shown <= 0 || (inv.menu.xp_level < cost_levels || inv.menu.xp_level < shown) && !creative {
        return false;
    }
    let list = enchantment_list(inv, &st, id, shown, data);
    if list.is_empty() {
        return false;
    }
    inv.menu.events.push(MenuEvent::Enchanted(cost_levels));
    let mut out = st;
    if name(&st) == "book" {
        if let Some(b) = Items::get().id("enchanted_book") {
            out = Stack { item: b, ..st };
        }
        out = with(out, |p| {
            for (n, l) in &list {
                p.stored_enchantments.push((n.clone(), *l as u8));
            }
        });
    } else {
        out = with(out, |p| {
            for (n, l) in &list {
                p.enchantments.push((n.clone(), *l as u8));
            }
        });
    }
    set(inv, 0, Some(out));
    if !creative {
        shrink(inv, 1, cost_levels as u8);
    }
    inv.menu.events.push(MenuEvent::Stat("enchant_item"));
    inv.menu.events.push(MenuEvent::Sound("minecraft:block.enchantment_table.use"));
    // the seed changes before the menu is refreshed (by the session)
    true
}
