//! Item stack components.
//!
//! A `Stack` stays a small `Copy` value: its component *patch* (what differs
//! from the item's defaults: damage, enchantments, potion, name…) is interned
//! here and referenced by id, 0 being "no patch". Equal patches always get
//! the same id, so stacks compare and merge by `(item, comp)` like vanilla's
//! `ItemStack.isSameItemSameComponents`.
//!
//! Default components (attribute modifiers, durability, equippable slot,
//! shield blocking, weapon, …) come from `assets/26.1/item_components.json.gz`,
//! generated from the vanilla data reports.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;
use serde_json::Value;

use crate::nbt::Tag;
use crate::net::codec::{Out, Reader};
use crate::registry::Registries;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Patch {
    pub damage: Option<i32>,
    /// (enchantment name without namespace, level), in insertion order
    pub enchantments: Vec<(String, u8)>,
    pub stored_enchantments: Vec<(String, u8)>,
    /// potion id (no namespace)
    pub potion: Option<String>,
    pub custom_name: Option<String>,
    pub repair_cost: Option<i32>,
    pub unbreakable: bool,
    /// `charged_projectiles`: (item, count, patch)
    pub charged: Vec<(i32, u8, u32)>,
    /// `intangible_projectile`: only picked up in creative
    pub intangible: bool,
    /// `trim`: (material, pattern), no namespace
    pub trim: Option<(String, String)>,
    /// `banner_patterns`: (pattern, dye colour id), bottom layer first
    pub banner: Vec<(String, u8)>,
    /// `dyed_color` (RGB)
    pub dyed_color: Option<i32>,
}

/// Network ids of the synchronized data-driven registries (trim materials,
/// banner patterns…), set once from the configuration data.
static DATA_REGS: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();

pub fn set_data_registries(m: HashMap<String, Vec<String>>) {
    let _ = DATA_REGS.set(m);
}

/// Network id of `name` (no namespace) in a data registry (`minecraft:trim_material`).
pub fn data_reg_id(registry: &str, name: &str) -> Option<i32> {
    let full = format!("minecraft:{}", name.trim_start_matches("minecraft:"));
    DATA_REGS.get()?.get(registry)?.iter().position(|n| *n == full).map(|i| i as i32)
}

pub fn data_reg_name(registry: &str, id: i32) -> Option<String> {
    DATA_REGS.get()?.get(registry)?.get(id as usize).map(|n| n.trim_start_matches("minecraft:").to_string())
}

pub const DYE_COLORS: [&str; 16] = ["white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray", "cyan", "purple", "blue", "brown", "green", "red", "black"];

pub fn dye_id(name: &str) -> Option<u8> {
    DYE_COLORS.iter().position(|c| *c == name).map(|i| i as u8)
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        *self == Patch::default()
    }
}

#[derive(Default)]
struct Store {
    patches: Vec<Arc<Patch>>,
    ids: HashMap<Arc<Patch>, u32>,
}

fn store() -> &'static RwLock<Store> {
    static S: OnceLock<RwLock<Store>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(Store { patches: vec![Arc::new(Patch::default())], ids: HashMap::new() }))
}

/// Interns a patch; returns its id (0 for the empty patch).
pub fn intern(p: Patch) -> u32 {
    if p.is_empty() {
        return 0;
    }
    if let Some(id) = store().read().ids.get(&p) {
        return *id;
    }
    let mut s = store().write();
    if let Some(id) = s.ids.get(&p) {
        return *id;
    }
    let id = s.patches.len() as u32;
    let a = Arc::new(p);
    s.patches.push(a.clone());
    s.ids.insert(a, id);
    id
}

pub fn patch(id: u32) -> Arc<Patch> {
    let s = store().read();
    s.patches.get(id as usize).cloned().unwrap_or_else(|| s.patches[0].clone())
}

// ------------------------------------------------------------------ defaults

#[derive(Clone, Debug)]
pub struct AttrMod {
    pub attr: crate::game::combat::attr::Attr,
    pub modifier: crate::game::combat::attr::Modifier,
    /// equipment slot group: any, mainhand, offhand, hand, feet, legs,
    /// chest, head, armor, body, saddle
    pub slot: String,
}

#[derive(Clone, Debug)]
pub struct Reduction {
    pub angle: f32,
    pub types: Option<String>,
    pub base: f32,
    pub factor: f32,
}

#[derive(Clone, Debug)]
pub struct BlocksAttacks {
    pub block_delay_seconds: f32,
    pub disable_cooldown_scale: f32,
    pub reductions: Vec<Reduction>,
    /// (threshold, base, factor)
    pub item_damage: (f32, f32, f32),
    /// damage type tag or list (`#minecraft:bypasses_shield`)
    pub bypassed_by: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Defaults {
    pub modifiers: Vec<AttrMod>,
    pub max_damage: Option<i32>,
    pub equippable_slot: Option<String>,
    pub enchantable: Option<i32>,
    /// (item_damage_per_attack, disable_blocking_for_seconds)
    pub weapon: Option<(i32, f32)>,
    pub blocks_attacks: Option<BlocksAttacks>,
    pub death_protection: Option<Value>,
    /// damage type tag the item (as an entity) is immune to
    pub damage_resistant: Option<String>,
    pub minimum_attack_charge: Option<f32>,
    pub glider: bool,
    pub raw: Value,
}

pub fn defaults(item: i32) -> &'static Defaults {
    static D: OnceLock<Vec<Defaults>> = OnceLock::new();
    let all = D.get_or_init(|| {
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(&include_bytes!("../assets/26.1/item_components.json.gz")[..]).read_to_end(&mut raw).expect("item components");
        let v: Value = serde_json::from_slice(&raw).expect("item components json");
        let items = crate::data::Items::get();
        let n = items.count();
        (0..n as i32).map(|i| parse_defaults(v.get(items.name(i)).cloned().unwrap_or(Value::Null))).collect()
    });
    static EMPTY: OnceLock<Defaults> = OnceLock::new();
    all.get(item as usize).unwrap_or_else(|| EMPTY.get_or_init(Defaults::default))
}

fn parse_defaults(v: Value) -> Defaults {
    use crate::game::combat::attr::{Attr, Modifier, Op};
    let f = |x: &Value, k: &str, d: f64| x.get(k).and_then(|v| v.as_f64()).unwrap_or(d) as f32;
    let mut d = Defaults { raw: v.clone(), ..Default::default() };
    if let Some(list) = v.get("attribute_modifiers").and_then(|a| a.as_array()) {
        for m in list {
            let (Some(attr), Some(op)) = (m["type"].as_str().and_then(Attr::from_name), m["operation"].as_str().and_then(Op::parse)) else { continue };
            d.modifiers.push(AttrMod {
                attr,
                modifier: Modifier { id: m["id"].as_str().unwrap_or("").to_string(), amount: m["amount"].as_f64().unwrap_or(0.0), op },
                slot: m["slot"].as_str().unwrap_or("any").to_string(),
            });
        }
    }
    d.max_damage = v.get("max_damage").and_then(|x| x.as_i64()).map(|x| x as i32);
    d.equippable_slot = v.get("equippable").and_then(|e| e["slot"].as_str()).map(String::from);
    d.enchantable = v.get("enchantable").and_then(|e| e["value"].as_i64()).map(|x| x as i32);
    if let Some(w) = v.get("weapon") {
        d.weapon = Some((w.get("item_damage_per_attack").and_then(|x| x.as_i64()).unwrap_or(1) as i32, f(w, "disable_blocking_for_seconds", 0.0)));
    }
    if let Some(b) = v.get("blocks_attacks") {
        let reductions = match b.get("damage_reductions").and_then(|r| r.as_array()) {
            Some(rs) => rs
                .iter()
                .map(|r| Reduction { angle: f(r, "horizontal_blocking_angle", 90.0), types: r.get("type").map(type_list), base: f(r, "base", 0.0), factor: f(r, "factor", 0.0) })
                .collect(),
            None => vec![Reduction { angle: 90.0, types: None, base: 0.0, factor: 1.0 }],
        };
        let item_damage = match b.get("item_damage") {
            Some(i) => (f(i, "threshold", 0.0), f(i, "base", 0.0), f(i, "factor", 0.0)),
            None => (1.0, 0.0, 1.0),
        };
        d.blocks_attacks = Some(BlocksAttacks {
            block_delay_seconds: f(b, "block_delay_seconds", 0.0),
            disable_cooldown_scale: f(b, "disable_cooldown_scale", 1.0),
            reductions,
            item_damage,
            bypassed_by: b.get("bypassed_by").map(type_list),
        });
    }
    d.death_protection = v.get("death_protection").cloned();
    d.damage_resistant = v.get("damage_resistant").and_then(|r| r["types"].as_str()).map(String::from);
    d.minimum_attack_charge = v.get("minimum_attack_charge").and_then(|x| x.as_f64()).map(|x| x as f32);
    d.glider = v.get("glider").is_some();
    d
}

/// A damage type holder set as written in JSON: `#tag` or a list.
fn type_list(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(","),
        _ => String::new(),
    }
}

/// Does a holder set string (`#minecraft:tag` or `a,b,c`) contain the damage type?
pub fn damage_set_contains(set: &str, ty: &str) -> bool {
    match set.strip_prefix('#') {
        Some(tag) => crate::game::combat::damage::DamageTypes::get().is(ty, tag.trim_start_matches("minecraft:")),
        None => set.split(',').any(|t| t.trim_start_matches("minecraft:") == ty),
    }
}

// ------------------------------------------------------------------ stack helpers

impl crate::data::Stack {
    pub fn patch(&self) -> Arc<Patch> {
        patch(self.comp)
    }
    pub fn defaults(&self) -> &'static Defaults {
        defaults(self.item)
    }
    pub fn with_patch(mut self, f: impl FnOnce(&mut Patch)) -> Self {
        let mut p = (*self.patch()).clone();
        f(&mut p);
        self.comp = intern(p);
        self
    }
    /// `ItemStack.isSameItemSameComponents`
    pub fn same(&self, o: &Self) -> bool {
        self.item == o.item && self.comp == o.comp
    }
    pub fn damage_value(&self) -> i32 {
        self.patch().damage.unwrap_or(0)
    }
    pub fn max_damage(&self) -> i32 {
        self.defaults().max_damage.unwrap_or(0)
    }
    /// `ItemStack.isDamageableItem`
    pub fn is_damageable(&self) -> bool {
        self.defaults().max_damage.is_some() && !self.patch().unbreakable
    }
    pub fn enchantment_level(&self, name: &str) -> u8 {
        self.patch().enchantments.iter().find(|(n, _)| n == name).map(|(_, l)| *l).unwrap_or(0)
    }
}

// ------------------------------------------------------------------ network

pub fn type_id_pub(name: &str) -> i32 {
    type_id(name)
}

fn type_id(name: &str) -> i32 {
    Registries::get().id("minecraft:data_component_type", name).unwrap_or(-1)
}

/// Global, protocol-ordered list of a data-driven registry (enchantments).
pub fn enchantment_names() -> &'static Vec<String> {
    &crate::game::combat::enchant::Enchantments::get().names
}

fn enchantment_net_id(name: &str) -> i32 {
    enchantment_names().iter().position(|n| n == name).map(|i| i as i32).unwrap_or(0)
}

/// Appends the component patch of a slot (`added count, removed count, …`).
pub fn write_patch(b: &mut Vec<u8>, comp: u32) {
    let p = patch(comp);
    let mut added: Vec<(i32, Vec<u8>)> = Vec::new();
    if let Some(d) = p.damage {
        let mut x = Vec::new();
        x.vi(d);
        added.push((type_id("minecraft:damage"), x));
    }
    if p.unbreakable {
        added.push((type_id("minecraft:unbreakable"), Vec::new()));
    }
    if let Some(n) = &p.custom_name {
        added.push((type_id("minecraft:custom_name"), Tag::text(n).to_network_bytes()));
    }
    for (key, list) in [("minecraft:enchantments", &p.enchantments), ("minecraft:stored_enchantments", &p.stored_enchantments)] {
        if !list.is_empty() {
            let mut x = Vec::new();
            x.vi(list.len() as i32);
            for (n, l) in list {
                x.vi(enchantment_net_id(n)).vi(*l as i32);
            }
            added.push((type_id(key), x));
        }
    }
    if let Some(c) = p.repair_cost {
        let mut x = Vec::new();
        x.vi(c);
        added.push((type_id("minecraft:repair_cost"), x));
    }
    if let Some(pot) = &p.potion {
        let mut x = Vec::new();
        // Optional<Holder<Potion>> (registry holder: id + 1), Optional<int> colour, effects, Optional<String> name
        let id = Registries::get().id("minecraft:potion", &format!("minecraft:{pot}")).unwrap_or(0);
        x.bool(true).vi(id + 1).bool(false).vi(0).bool(false);
        added.push((type_id("minecraft:potion_contents"), x));
    }
    if p.intangible {
        added.push((type_id("minecraft:intangible_projectile"), Vec::new()));
    }
    if !p.charged.is_empty() {
        let mut x = Vec::new();
        x.vi(p.charged.len() as i32);
        for (item, count, comp) in &p.charged {
            crate::game::write_slot(&mut x, Some(crate::data::Stack { item: *item, count: *count, comp: *comp }));
        }
        added.push((type_id("minecraft:charged_projectiles"), x));
    }
    if let Some((m, pt)) = &p.trim {
        if let (Some(a), Some(b)) = (data_reg_id("minecraft:trim_material", m), data_reg_id("minecraft:trim_pattern", pt)) {
            let mut x = Vec::new();
            x.vi(a + 1).vi(b + 1);
            added.push((type_id("minecraft:trim"), x));
        }
    }
    if !p.banner.is_empty() {
        let mut x = Vec::new();
        let layers: Vec<(i32, u8)> = p.banner.iter().filter_map(|(n, c)| Some((data_reg_id("minecraft:banner_pattern", n)?, *c))).collect();
        x.vi(layers.len() as i32);
        for (id, c) in layers {
            x.vi(id + 1).vi(c as i32);
        }
        added.push((type_id("minecraft:banner_patterns"), x));
    }
    if let Some(rgb) = p.dyed_color {
        let mut x = Vec::new();
        x.i32(rgb);
        added.push((type_id("minecraft:dyed_color"), x));
    }
    added.retain(|(id, _)| *id >= 0);
    b.vi(added.len() as i32).vi(0);
    for (id, data) in added {
        b.vi(id).raw(&data);
    }
}

/// Reads a client-sent component patch (creative inventory). Returns None
/// when it contains a component RimeMC can't decode (the stack is then
/// ignored rather than guessed).
pub fn read_patch(r: &mut Reader) -> anyhow::Result<Option<Patch>> {
    let added = r.varint()?;
    let removed = r.varint()?;
    anyhow::ensure!((0..=64).contains(&added) && (0..=64).contains(&removed), "bad component counts");
    let reg = Registries::get();
    let name_of = |id: i32| reg.name("minecraft:data_component_type", id).unwrap_or_default();
    let mut p = Patch::default();
    for _ in 0..added {
        let id = r.varint()?;
        match name_of(id).as_str() {
            "minecraft:damage" => p.damage = Some(r.varint()?.max(0)),
            "minecraft:repair_cost" => p.repair_cost = Some(r.varint()?.max(0)),
            "minecraft:unbreakable" => p.unbreakable = true,
            k @ ("minecraft:enchantments" | "minecraft:stored_enchantments") => {
                let n = r.varint()?;
                anyhow::ensure!((0..=256).contains(&n), "too many enchantments");
                let mut list = Vec::new();
                for _ in 0..n {
                    let e = r.varint()?;
                    let lvl = r.varint()?;
                    if let Some(name) = enchantment_names().get(e as usize) {
                        list.push((name.clone(), lvl.clamp(1, 255) as u8));
                    }
                }
                if k == "minecraft:enchantments" {
                    p.enchantments = list;
                } else {
                    p.stored_enchantments = list;
                }
            }
            "minecraft:potion_contents" => {
                if r.bool()? {
                    let h = r.varint()?;
                    anyhow::ensure!(h > 0, "inline potion");
                    p.potion = reg.name("minecraft:potion", h - 1).map(|n| n.trim_start_matches("minecraft:").to_string());
                }
                if r.bool()? {
                    r.i32()?;
                }
                if r.varint()? != 0 {
                    return Ok(None);
                }
                if r.bool()? {
                    return Ok(None);
                }
            }
            "minecraft:trim" => {
                let a = r.varint()?;
                let b = r.varint()?;
                anyhow::ensure!(a > 0 && b > 0, "inline trim");
                match (data_reg_name("minecraft:trim_material", a - 1), data_reg_name("minecraft:trim_pattern", b - 1)) {
                    (Some(m), Some(pt)) => p.trim = Some((m, pt)),
                    _ => return Ok(None),
                }
            }
            "minecraft:banner_patterns" => {
                let n = r.varint()?;
                anyhow::ensure!((0..=16).contains(&n), "too many banner layers");
                for _ in 0..n {
                    let h = r.varint()?;
                    anyhow::ensure!(h > 0, "inline banner pattern");
                    let c = r.varint()?;
                    let Some(name) = data_reg_name("minecraft:banner_pattern", h - 1) else { return Ok(None) };
                    p.banner.push((name, c.clamp(0, 15) as u8));
                }
            }
            _ => return Ok(None),
        }
    }
    for _ in 0..removed {
        r.varint()?;
    }
    Ok(Some(p))
}

// ------------------------------------------------------------------ NBT

/// The `components` compound of a saved stack (None when empty).
pub fn to_nbt(comp: u32) -> Option<Tag> {
    let p = patch(comp);
    if p.is_empty() {
        return None;
    }
    let mut c = Vec::new();
    if let Some(d) = p.damage {
        c.push(("minecraft:damage".into(), Tag::Int(d)));
    }
    if p.unbreakable {
        c.push(("minecraft:unbreakable".into(), Tag::Compound(vec![])));
    }
    if let Some(n) = &p.custom_name {
        c.push(("minecraft:custom_name".into(), Tag::String(n.clone())));
    }
    for (key, list) in [("minecraft:enchantments", &p.enchantments), ("minecraft:stored_enchantments", &p.stored_enchantments)] {
        if !list.is_empty() {
            c.push((key.into(), Tag::Compound(list.iter().map(|(n, l)| (format!("minecraft:{n}"), Tag::Int(*l as i32))).collect())));
        }
    }
    if let Some(r) = p.repair_cost {
        c.push(("minecraft:repair_cost".into(), Tag::Int(r)));
    }
    if let Some(pot) = &p.potion {
        c.push(("minecraft:potion_contents".into(), Tag::Compound(vec![("potion".into(), Tag::String(format!("minecraft:{pot}")))])));
    }
    if p.intangible {
        c.push(("minecraft:intangible_projectile".into(), Tag::Compound(vec![])));
    }
    if let Some(rgb) = p.dyed_color {
        c.push(("minecraft:dyed_color".into(), Tag::Int(rgb)));
    }
    if let Some((m, pt)) = &p.trim {
        c.push(("minecraft:trim".into(), Tag::Compound(vec![("material".into(), Tag::String(format!("minecraft:{m}"))), ("pattern".into(), Tag::String(format!("minecraft:{pt}")))])));
    }
    if !p.banner.is_empty() {
        let l = p
            .banner
            .iter()
            .map(|(n, col)| Tag::Compound(vec![("pattern".into(), Tag::String(format!("minecraft:{n}"))), ("color".into(), Tag::String(DYE_COLORS[*col as usize % 16].into()))]))
            .collect();
        c.push(("minecraft:banner_patterns".into(), Tag::List(l)));
    }
    if !p.charged.is_empty() {
        let items = crate::data::Items::get();
        let list = p
            .charged
            .iter()
            .map(|(item, count, comp)| {
                let mut e = vec![("id".to_string(), Tag::String(format!("minecraft:{}", items.name(*item)))), ("count".to_string(), Tag::Int(*count as i32))];
                if let Some(c) = to_nbt(*comp) {
                    e.push(("components".to_string(), c));
                }
                Tag::Compound(e)
            })
            .collect();
        c.push(("minecraft:charged_projectiles".into(), Tag::List(list)));
    }
    Some(Tag::Compound(c))
}

pub fn from_nbt(t: &Tag) -> u32 {
    let mut p = Patch::default();
    let ench = |t: Option<&Tag>| -> Vec<(String, u8)> {
        // 1.21+: {"minecraft:sharpness": 5}; older: {levels: {...}}
        let t = t.map(|t| t.get("levels").unwrap_or(t));
        match t {
            Some(Tag::Compound(v)) => v.iter().filter_map(|(k, l)| Some((k.trim_start_matches("minecraft:").to_string(), l.as_i64()?.clamp(1, 255) as u8))).collect(),
            _ => Vec::new(),
        }
    };
    p.damage = t.get("minecraft:damage").and_then(|d| d.as_i64()).map(|d| d.max(0) as i32);
    p.unbreakable = t.get("minecraft:unbreakable").is_some();
    p.custom_name = t.get("minecraft:custom_name").map(|n| n.as_str().map(String::from).unwrap_or_else(|| n.plain_text()));
    p.enchantments = ench(t.get("minecraft:enchantments"));
    p.stored_enchantments = ench(t.get("minecraft:stored_enchantments"));
    p.repair_cost = t.get("minecraft:repair_cost").and_then(|d| d.as_i64()).map(|d| d as i32);
    p.potion = t.get("minecraft:potion_contents").and_then(|pc| pc.get("potion").or(Some(pc))).and_then(|x| x.as_str()).map(|s| s.trim_start_matches("minecraft:").to_string());
    p.intangible = t.get("minecraft:intangible_projectile").is_some();
    p.dyed_color = t.get("minecraft:dyed_color").and_then(|d| d.as_i64().or_else(|| d.get("rgb").and_then(|x| x.as_i64()))).map(|d| d as i32);
    if let Some(tr) = t.get("minecraft:trim") {
        let s = |k: &str| tr.get(k).and_then(|v| v.as_str()).map(|x| x.trim_start_matches("minecraft:").to_string());
        if let (Some(m), Some(pt)) = (s("material"), s("pattern")) {
            p.trim = Some((m, pt));
        }
    }
    if let Some(Tag::List(l)) = t.get("minecraft:banner_patterns") {
        for e in l {
            let (Some(n), Some(c)) = (e.get("pattern").and_then(|v| v.as_str()), e.get("color").and_then(|v| v.as_str()).and_then(dye_id)) else { continue };
            p.banner.push((n.trim_start_matches("minecraft:").to_string(), c));
        }
    }
    if let Some(Tag::List(l)) = t.get("minecraft:charged_projectiles") {
        let items = crate::data::Items::get();
        for e in l {
            let Some(id) = e.get("id").and_then(|i| i.as_str()).and_then(|n| items.id(n.trim_start_matches("minecraft:"))) else { continue };
            let count = e.get("count").and_then(|c| c.as_i64()).unwrap_or(1).clamp(1, 99) as u8;
            let comp = e.get("components").map(from_nbt).unwrap_or(0);
            p.charged.push((id, count, comp));
        }
    }
    intern(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_and_defaults() {
        let a = intern(Patch { damage: Some(3), ..Default::default() });
        let b = intern(Patch { damage: Some(3), ..Default::default() });
        assert_eq!(a, b);
        assert_ne!(a, 0);
        assert_eq!(intern(Patch::default()), 0);
        let items = crate::data::Items::get();
        let sword = defaults(items.id("diamond_sword").unwrap());
        assert_eq!(sword.max_damage, Some(1561));
        assert!(sword.modifiers.iter().any(|m| m.modifier.amount == 6.0));
        let shield = defaults(items.id("shield").unwrap());
        let b = shield.blocks_attacks.as_ref().unwrap();
        assert_eq!(b.reductions.len(), 1);
        assert_eq!(b.item_damage, (3.0, 1.0, 1.0));
        assert_eq!(defaults(items.id("diamond_chestplate").unwrap()).equippable_slot.as_deref(), Some("chest"));
    }
}
