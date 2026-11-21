//! Data-driven enchantments (`data/minecraft/enchantment/*.json`):
//! level-based values, value effects, entity effects and the loot
//! conditions they use, evaluated like `Enchantment` / `EnchantmentHelper`.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use rand::Rng;
use serde_json::Value;

use super::damage::DamageSource;
use crate::vanilla::VanillaData;

// ------------------------------------------------------------------ values

#[derive(Clone, Debug)]
pub enum LevelValue {
    Constant(f32),
    Linear(f32, f32),
    Clamped(Box<LevelValue>, f32, f32),
    Fraction(Box<LevelValue>, Box<LevelValue>),
    LevelsSquared(f32),
    Exponent(Box<LevelValue>, Box<LevelValue>),
    Lookup(Vec<f32>, Box<LevelValue>),
}

impl LevelValue {
    pub fn parse(v: &Value) -> LevelValue {
        if let Some(c) = v.as_f64() {
            return LevelValue::Constant(c as f32);
        }
        let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        let b = |k: &str| Box::new(LevelValue::parse(v.get(k).unwrap_or(&Value::Null)));
        match v["type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
            "linear" => LevelValue::Linear(f("base"), f("per_level_above_first")),
            "clamped" => LevelValue::Clamped(b("value"), f("min"), f("max")),
            "fraction" => LevelValue::Fraction(b("numerator"), b("denominator")),
            "levels_squared" => LevelValue::LevelsSquared(f("added")),
            "exponent" => LevelValue::Exponent(b("base"), b("power")),
            "lookup" => LevelValue::Lookup(v["values"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect()).unwrap_or_default(), b("fallback")),
            "constant" => LevelValue::Constant(f("value")),
            _ => LevelValue::Constant(0.0),
        }
    }

    /// `LevelBasedValue.calculate`
    pub fn calc(&self, level: i32) -> f32 {
        match self {
            LevelValue::Constant(c) => *c,
            LevelValue::Linear(base, per) => base + per * (level - 1) as f32,
            LevelValue::Clamped(v, min, max) => v.calc(level).clamp(*min, *max),
            LevelValue::Fraction(n, d) => {
                let d = d.calc(level);
                if d == 0.0 {
                    0.0
                } else {
                    n.calc(level) / d
                }
            }
            LevelValue::LevelsSquared(a) => (level * level) as f32 + a,
            LevelValue::Exponent(b, p) => (b.calc(level) as f64).powf(p.calc(level) as f64) as f32,
            LevelValue::Lookup(vals, fb) => {
                if level as usize <= vals.len() && level >= 1 {
                    vals[level as usize - 1]
                } else {
                    fb.calc(level)
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum ValueEffect {
    Add(LevelValue),
    Multiply(LevelValue),
    Set(LevelValue),
    RemoveBinomial(LevelValue),
    AllOf(Vec<ValueEffect>),
}

impl ValueEffect {
    fn parse(v: &Value) -> ValueEffect {
        let lv = |k: &str| LevelValue::parse(v.get(k).unwrap_or(&Value::Null));
        match v["type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
            "add" => ValueEffect::Add(lv("value")),
            "multiply" => ValueEffect::Multiply(lv("factor")),
            "set" => ValueEffect::Set(lv("value")),
            "remove_binomial" => ValueEffect::RemoveBinomial(lv("chance")),
            "all_of" => ValueEffect::AllOf(v["effects"].as_array().map(|a| a.iter().map(ValueEffect::parse).collect()).unwrap_or_default()),
            _ => ValueEffect::AllOf(Vec::new()),
        }
    }

    /// `EnchantmentValueEffect.process`
    pub fn process(&self, level: i32, rng: &mut impl Rng, input: f32) -> f32 {
        match self {
            ValueEffect::Add(v) => input + v.calc(level),
            ValueEffect::Multiply(v) => input * v.calc(level),
            ValueEffect::Set(v) => v.calc(level),
            ValueEffect::RemoveBinomial(c) => {
                let p = c.calc(level);
                let n = input;
                let mut drop: i32 = 0;
                if !(n <= 128.0) && !(n * p < 20.0) && !(n * (1.0 - p) < 20.0) {
                    let miu = (n * p).floor() as f64;
                    let sigma = ((n * p * (1.0 - p)) as f64).sqrt();
                    drop = (miu + gaussian(rng) * sigma).round() as i32;
                    drop = drop.clamp(0, n as i32);
                } else {
                    for _ in 0..n as i32 {
                        if rng.r#gen::<f32>() < p {
                            drop += 1;
                        }
                    }
                }
                n - drop as f32
            }
            ValueEffect::AllOf(list) => list.iter().fold(input, |v, e| e.process(level, rng, v)),
        }
    }
}

pub fn gaussian(rng: &mut impl Rng) -> f64 {
    // Box–Muller (vanilla uses Marsaglia's polar method; the distribution is the same)
    let u1: f64 = rng.r#gen::<f64>().max(1e-300);
    let u2: f64 = rng.r#gen();
    (-2.0f64 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// Effects applied to an entity (post_attack, hit_block, tick, …).
#[derive(Clone, Debug)]
pub enum EntityEffect {
    Ignite(LevelValue),
    ApplyMobEffect { effects: Vec<String>, min_duration: LevelValue, max_duration: LevelValue, min_amp: LevelValue, max_amp: LevelValue },
    DamageEntity { min: LevelValue, max: LevelValue, damage_type: String },
    ChangeItemDamage(LevelValue),
    ApplyExhaustion(LevelValue),
    /// summon_entity (lightning bolts for channeling)
    SummonEntity(String),
    PlaySound(String, f32, f32),
    Explode(Value),
    ApplyImpulse(Value),
    AllOf(Vec<EntityEffect>),
    /// parsed but not simulated (particles, run_function, …)
    Other(String),
}

impl EntityEffect {
    fn parse(v: &Value) -> EntityEffect {
        let lv = |k: &str| LevelValue::parse(v.get(k).unwrap_or(&Value::Null));
        let ty = v["type"].as_str().unwrap_or("").trim_start_matches("minecraft:").to_string();
        match ty.as_str() {
            "ignite" => EntityEffect::Ignite(lv("duration")),
            "apply_mob_effect" => EntityEffect::ApplyMobEffect {
                effects: match &v["to_apply"] {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                    _ => vec![],
                },
                min_duration: lv("min_duration"),
                max_duration: lv("max_duration"),
                min_amp: lv("min_amplifier"),
                max_amp: lv("max_amplifier"),
            },
            "damage_entity" => EntityEffect::DamageEntity { min: lv("min_damage"), max: lv("max_damage"), damage_type: v["damage_type"].as_str().unwrap_or("generic").trim_start_matches("minecraft:").to_string() },
            "change_item_damage" => EntityEffect::ChangeItemDamage(lv("amount")),
            "apply_exhaustion" => EntityEffect::ApplyExhaustion(lv("amount")),
            "summon_entity" => EntityEffect::SummonEntity(match &v["entity"] {
                Value::String(s) => s.clone(),
                Value::Array(a) => a.first().and_then(|x| x.as_str()).unwrap_or("").to_string(),
                _ => String::new(),
            }),
            "play_sound" => EntityEffect::PlaySound(
                match &v["sound"] {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a.first().and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    _ => String::new(),
                },
                v["volume"].as_f64().unwrap_or(1.0) as f32,
                v["pitch"].as_f64().unwrap_or(1.0) as f32,
            ),
            "explode" => EntityEffect::Explode(v.clone()),
            "apply_impulse" => EntityEffect::ApplyImpulse(v.clone()),
            "all_of" => EntityEffect::AllOf(v["effects"].as_array().map(|a| a.iter().map(EntityEffect::parse).collect()).unwrap_or_default()),
            _ => EntityEffect::Other(ty),
        }
    }
}

// ------------------------------------------------------------------ conditions

/// What a loot condition can look at about an entity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EntInfo {
    pub type_name: &'static str,
    pub on_fire: bool,
    pub sneaking: bool,
    pub sprinting: bool,
    pub swimming: bool,
    pub flying: bool,
    pub fall_flying: bool,
    pub baby: bool,
    pub on_ground: bool,
    pub in_vehicle: bool,
    pub fall_distance: f64,
}

pub struct Ctx<'a> {
    pub level: i32,
    pub this: Option<&'a EntInfo>,
    pub attacker: Option<&'a EntInfo>,
    pub direct: Option<&'a EntInfo>,
    pub source: Option<&'a DamageSource>,
    pub tool: Option<crate::data::Stack>,
    pub raining: bool,
    pub thundering: bool,
    /// enchantment_active_check: whether the location effect is active
    pub active: bool,
}

#[derive(Clone, Debug)]
pub enum Cond {
    AllOf(Vec<Cond>),
    AnyOf(Vec<Cond>),
    Inverted(Box<Cond>),
    Entity { who: String, pred: Value },
    DamageSource(Value),
    MatchTool(Value),
    RandomChance(LevelValue),
    Weather { raining: Option<bool>, thundering: Option<bool> },
    EnchantmentActive(bool),
    /// location_check: needs block access; evaluated as false
    Location(Value),
    Unknown,
}

impl Cond {
    fn parse(v: &Value) -> Cond {
        let terms = |k: &str| v[k].as_array().map(|a| a.iter().map(Cond::parse).collect()).unwrap_or_default();
        match v["condition"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
            "all_of" => Cond::AllOf(terms("terms")),
            "any_of" => Cond::AnyOf(terms("terms")),
            "inverted" => Cond::Inverted(Box::new(Cond::parse(&v["term"]))),
            "entity_properties" => Cond::Entity { who: v["entity"].as_str().unwrap_or("this").to_string(), pred: v["predicate"].clone() },
            "damage_source_properties" => Cond::DamageSource(v["predicate"].clone()),
            "match_tool" => Cond::MatchTool(v["predicate"].clone()),
            "random_chance" => Cond::RandomChance(match &v["chance"] {
                Value::Object(o) if o.get("type").and_then(|t| t.as_str()).is_some_and(|t| t.ends_with("enchantment_level")) => LevelValue::parse(&v["chance"]["amount"]),
                other => LevelValue::parse(other),
            }),
            "weather_check" => Cond::Weather { raining: v["raining"].as_bool(), thundering: v["thundering"].as_bool() },
            "enchantment_active_check" => Cond::EnchantmentActive(v["active"].as_bool().unwrap_or(true)),
            "location_check" => Cond::Location(v["predicate"].clone()),
            _ => Cond::Unknown,
        }
    }

    pub fn test(&self, c: &Ctx, rng: &mut impl Rng) -> bool {
        match self {
            Cond::AllOf(t) => t.iter().all(|x| x.test(c, rng)),
            Cond::AnyOf(t) => t.iter().any(|x| x.test(c, rng)),
            Cond::Inverted(t) => !t.test(c, rng),
            Cond::Entity { who, pred } => {
                let e = match who.as_str() {
                    "this" => c.this,
                    "attacker" => c.attacker,
                    "direct_attacker" => c.direct,
                    _ => None,
                };
                e.is_some_and(|e| entity_matches(e, pred))
            }
            Cond::DamageSource(p) => c.source.is_some_and(|s| damage_matches(s, p, c)),
            Cond::MatchTool(p) => c.tool.is_some_and(|t| item_matches(t, p)),
            Cond::RandomChance(v) => rng.r#gen::<f32>() < v.calc(c.level),
            Cond::Weather { raining, thundering } => raining.is_none_or(|r| r == c.raining) && thundering.is_none_or(|t| t == c.thundering),
            Cond::EnchantmentActive(a) => *a == c.active,
            Cond::Location(_) | Cond::Unknown => false,
        }
    }
}

fn entity_matches(e: &EntInfo, p: &Value) -> bool {
    if let Some(t) = p.get("type") {
        let ok = match t {
            Value::String(s) => type_matches(e.type_name, s),
            Value::Array(a) => a.iter().filter_map(|x| x.as_str()).any(|s| type_matches(e.type_name, s)),
            _ => true,
        };
        if !ok {
            return false;
        }
    }
    if let Some(f) = p.get("flags").and_then(|f| f.as_object()) {
        for (k, v) in f {
            let want = v.as_bool().unwrap_or(false);
            let have = match k.as_str() {
                "is_on_fire" => e.on_fire,
                "is_sneaking" => e.sneaking,
                "is_sprinting" => e.sprinting,
                "is_swimming" => e.swimming,
                "is_flying" => e.flying,
                "is_fall_flying" => e.fall_flying,
                "is_baby" => e.baby,
                "is_on_ground" => e.on_ground,
                _ => want,
            };
            if have != want {
                return false;
            }
        }
    }
    if p.get("vehicle").is_some() && !e.in_vehicle {
        return false;
    }
    if let Some(fd) = p.get("movement").and_then(|m| m.get("fall_distance")) {
        if let Some(min) = fd.get("min").and_then(|x| x.as_f64()) {
            if e.fall_distance < min {
                return false;
            }
        }
        if let Some(max) = fd.get("max").and_then(|x| x.as_f64()) {
            if e.fall_distance > max {
                return false;
            }
        }
    }
    true
}

fn type_matches(ty: &str, spec: &str) -> bool {
    let ty = ty.trim_start_matches("minecraft:");
    match spec.strip_prefix('#') {
        Some(tag) => entity_tag(ty, tag),
        None => spec.trim_start_matches("minecraft:") == ty,
    }
}

/// `entity.getType().is(#tag)`
pub fn entity_tag(ty: &str, tag: &str) -> bool {
    Enchantments::get().entity_tags.get(tag.trim_start_matches("minecraft:")).is_some_and(|s| s.contains(ty.trim_start_matches("minecraft:")))
}

fn damage_matches(s: &DamageSource, p: &Value, c: &Ctx) -> bool {
    if let Some(tags) = p.get("tags").and_then(|t| t.as_array()) {
        for t in tags {
            let id = t["id"].as_str().unwrap_or("").trim_start_matches("minecraft:");
            if s.is(id) != t["expected"].as_bool().unwrap_or(true) {
                return false;
            }
        }
    }
    if let Some(d) = p.get("is_direct").and_then(|x| x.as_bool()) {
        if s.is_direct() != d {
            return false;
        }
    }
    if let Some(e) = p.get("source_entity") {
        if !c.attacker.is_some_and(|a| entity_matches(a, e)) {
            return false;
        }
    }
    if let Some(e) = p.get("direct_entity") {
        if !c.direct.is_some_and(|a| entity_matches(a, e)) {
            return false;
        }
    }
    true
}

fn item_matches(s: crate::data::Stack, p: &Value) -> bool {
    let items = crate::data::Items::get();
    let name = items.name(s.item);
    match p.get("items") {
        Some(Value::String(spec)) => match spec.strip_prefix('#') {
            Some(tag) => Enchantments::get().item_tags.get(tag.trim_start_matches("minecraft:")).is_some_and(|t| t.contains(name)),
            None => spec.trim_start_matches("minecraft:") == name,
        },
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).any(|x| x.trim_start_matches("minecraft:") == name),
        _ => true,
    }
}

// ------------------------------------------------------------------ enchantments

#[derive(Clone, Debug)]
pub struct Conditional<T> {
    pub effect: T,
    pub requirements: Option<Cond>,
}

#[derive(Clone, Debug)]
pub struct Targeted {
    pub effect: EntityEffect,
    /// attacker / victim
    pub enchanted: String,
    /// attacker / damaging_entity / victim
    pub affected: String,
    pub requirements: Option<Cond>,
}

#[derive(Clone, Debug)]
pub struct AttributeEffect {
    pub id: String,
    pub attribute: super::attr::Attr,
    pub amount: LevelValue,
    pub op: super::attr::Op,
}

#[derive(Clone, Debug, Default)]
pub struct Enchantment {
    pub name: String,
    pub max_level: i32,
    /// equipment slot groups (`mainhand`, `armor`, `any`, …)
    pub slots: Vec<String>,
    pub damage_protection: Vec<Conditional<ValueEffect>>,
    pub damage: Vec<Conditional<ValueEffect>>,
    pub knockback: Vec<Conditional<ValueEffect>>,
    pub armor_effectiveness: Vec<Conditional<ValueEffect>>,
    pub item_damage: Vec<Conditional<ValueEffect>>,
    pub damage_immunity: Vec<Option<Cond>>,
    pub smash_damage_per_fallen_block: Vec<Conditional<ValueEffect>>,
    pub post_attack: Vec<Targeted>,
    pub attributes: Vec<AttributeEffect>,
    pub raw: Value,
}

#[derive(Default)]
pub struct Enchantments {
    /// protocol order (sorted keys, no namespace)
    pub names: Vec<String>,
    pub by_name: HashMap<String, Enchantment>,
    pub entity_tags: HashMap<String, HashSet<String>>,
    pub item_tags: HashMap<String, HashSet<String>>,
}

static ENCH: OnceLock<Enchantments> = OnceLock::new();

impl Enchantments {
    pub fn load(v: &VanillaData) -> anyhow::Result<()> {
        let mut e = Enchantments { entity_tags: crate::data::load_tags(v, "entity_type")?, item_tags: crate::data::load_tags(v, "item")?, ..Default::default() };
        for (key, path) in v.entries("enchantment") {
            let j: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            let name = key.trim_start_matches("minecraft:").to_string();
            e.names.push(name.clone());
            e.by_name.insert(name.clone(), parse_enchantment(name, &j));
        }
        let _ = ENCH.set(e);
        Ok(())
    }

    pub fn get() -> &'static Enchantments {
        ENCH.get_or_init(Enchantments::default)
    }

    pub fn by(&self, name: &str) -> Option<&Enchantment> {
        self.by_name.get(name.trim_start_matches("minecraft:"))
    }
}

fn conditionals(v: &Value) -> Vec<Conditional<ValueEffect>> {
    v.as_array()
        .map(|a| a.iter().map(|x| Conditional { effect: ValueEffect::parse(&x["effect"]), requirements: x.get("requirements").map(Cond::parse) }).collect())
        .unwrap_or_default()
}

pub fn parse_enchantment(name: String, j: &Value) -> Enchantment {
    let eff = &j["effects"];
    let g = |k: &str| eff.get(format!("minecraft:{k}")).cloned().unwrap_or(Value::Null);
    Enchantment {
        name,
        max_level: j["max_level"].as_i64().unwrap_or(1) as i32,
        slots: j["slots"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
        damage_protection: conditionals(&g("damage_protection")),
        damage: conditionals(&g("damage")),
        knockback: conditionals(&g("knockback")),
        armor_effectiveness: conditionals(&g("armor_effectiveness")),
        item_damage: conditionals(&g("item_damage")),
        smash_damage_per_fallen_block: conditionals(&g("smash_damage_per_fallen_block")),
        damage_immunity: g("damage_immunity").as_array().map(|a| a.iter().map(|x| x.get("requirements").map(Cond::parse)).collect()).unwrap_or_default(),
        post_attack: g("post_attack")
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| Targeted {
                        effect: EntityEffect::parse(&x["effect"]),
                        enchanted: x["enchanted"].as_str().unwrap_or("attacker").to_string(),
                        affected: x["affected"].as_str().unwrap_or("victim").to_string(),
                        requirements: x.get("requirements").map(Cond::parse),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        attributes: g("attributes")
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        Some(AttributeEffect {
                            id: x["id"].as_str()?.to_string(),
                            attribute: super::attr::Attr::from_name(x["attribute"].as_str()?)?,
                            amount: LevelValue::parse(&x["amount"]),
                            op: super::attr::Op::parse(x["operation"].as_str()?)?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        raw: j.clone(),
    }
}

/// `EquipmentSlotGroup.test`
pub fn slot_group_matches(group: &str, slot: &str) -> bool {
    match group {
        "any" => true,
        "hand" => matches!(slot, "mainhand" | "offhand"),
        "armor" => matches!(slot, "feet" | "legs" | "chest" | "head" | "body"),
        g => g == slot,
    }
}

impl Enchantment {
    pub fn matching_slot(&self, slot: &str) -> bool {
        self.slots.iter().any(|g| slot_group_matches(g, slot))
    }
}

/// Applies matching conditional value effects to `value`.
pub fn apply_values(effects: &[Conditional<ValueEffect>], c: &Ctx, rng: &mut impl Rng, value: &mut f32) {
    for e in effects {
        if e.requirements.as_ref().is_none_or(|r| r.test(c, rng)) {
            *value = e.effect.process(c.level, rng, *value);
        }
    }
}

/// Enchantments of a stack that apply in `slot` (None = any slot, like
/// `runIterationOnItem(stack, visitor)` without a slot).
pub fn on_item(s: crate::data::Stack, slot: Option<&str>) -> Vec<(&'static Enchantment, i32)> {
    let e = Enchantments::get();
    s.patch()
        .enchantments
        .iter()
        .filter_map(|(n, l)| {
            let en = e.by(n)?;
            slot.is_none_or(|sl| en.matching_slot(sl)).then_some((en, *l as i32))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        let lin = LevelValue::parse(&serde_json::json!({"type":"minecraft:linear","base":1.0,"per_level_above_first":0.5}));
        assert_eq!(lin.calc(5), 3.0);
        let frac = LevelValue::parse(&serde_json::json!({"type":"minecraft:fraction","numerator":{"type":"minecraft:linear","base":1.0,"per_level_above_first":1.0},"denominator":{"type":"minecraft:linear","base":2.0,"per_level_above_first":1.0}}));
        assert_eq!(frac.calc(3), 0.75);
        let lk = LevelValue::parse(&serde_json::json!({"type":"minecraft:lookup","values":[1.2,1.75,2.2],"fallback":{"type":"minecraft:linear","base":1.5,"per_level_above_first":0.35}}));
        assert_eq!(lk.calc(2), 1.75);
        assert!((lk.calc(4) - 2.55).abs() < 1e-6);
        let mut rng = rand::thread_rng();
        assert_eq!(ValueEffect::Add(lin.clone()).process(5, &mut rng, 1.0), 4.0);
    }

    #[test]
    fn protection_condition() {
        let j: Value = serde_json::from_str(r#"{"max_level":4,"slots":["armor"],"effects":{"minecraft:damage_protection":[{"effect":{"type":"minecraft:add","value":{"type":"minecraft:linear","base":3.0,"per_level_above_first":3.0}},"requirements":{"condition":"minecraft:damage_source_properties","predicate":{"tags":[{"expected":true,"id":"minecraft:is_fall"},{"expected":false,"id":"minecraft:bypasses_invulnerability"}]}}}]}}"#).unwrap();
        let ff = parse_enchantment("feather_falling".into(), &j);
        let mut rng = rand::thread_rng();
        let fall = DamageSource::new("fall");
        let lava = DamageSource::new("lava");
        let mut v = 0.0;
        let ctx = |s| Ctx { level: 4, this: None, attacker: None, direct: None, source: Some(s), tool: None, raining: false, thundering: false, active: true };
        apply_values(&ff.damage_protection, &ctx(&fall), &mut rng, &mut v);
        assert_eq!(v, 12.0);
        let mut w = 0.0;
        apply_values(&ff.damage_protection, &ctx(&lava), &mut rng, &mut w);
        assert_eq!(w, 0.0);
        assert!(ff.matching_slot("feet") && !ff.matching_slot("mainhand"));
    }
}
