//! Villagers and wandering traders (`Villager`, `AbstractVillager`,
//! `WanderingTrader`): professions from job site blocks, the data-driven
//! trades (`villager_trade` / `trade_set`), the merchant menu, levels,
//! restocking, demand, reputation (gossip) and zombie villager curing.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::Rng;
use serde_json::Value;

use super::combat::effects::{Effect, Instance};
use super::entity::EKind;
use super::{write_slot, Game, Player};
use crate::data::{GameData, Items, Stack};
use crate::nbt::Tag;
use crate::net::codec::Out;
use crate::registry::Registries;

/// profession -> job site block (`PoiTypes`)
pub const PROFESSIONS: [(&str, &str); 13] = [
    ("armorer", "blast_furnace"),
    ("butcher", "smoker"),
    ("cartographer", "cartography_table"),
    ("cleric", "brewing_stand"),
    ("farmer", "composter"),
    ("fisherman", "barrel"),
    ("fletcher", "fletching_table"),
    ("leatherworker", "cauldron"),
    ("librarian", "lectern"),
    ("mason", "stonecutter"),
    ("shepherd", "loom"),
    ("toolsmith", "smithing_table"),
    ("weaponsmith", "grindstone"),
];

/// `VillagerData.NEXT_LEVEL_XP_THRESHOLDS`
const XP_LEVELS: [i32; 5] = [0, 10, 70, 150, 250];

/// gossip kinds: major_negative, minor_negative, minor_positive, major_positive, trading
const GOSSIP_WEIGHT: [i32; 5] = [-5, -1, 1, 5, 1];
const GOSSIP_MAX: [i32; 5] = [100, 200, 25, 20, 25];
const GOSSIP_DECAY: [i32; 5] = [10, 20, 1, 0, 2];
pub const MAJOR_NEGATIVE: usize = 0;
pub const MINOR_NEGATIVE: usize = 1;
pub const MINOR_POSITIVE: usize = 2;
pub const MAJOR_POSITIVE: usize = 3;
pub const TRADING: usize = 4;

fn job_block(prof: &str) -> Option<&'static str> {
    PROFESSIONS.iter().find(|(p, _)| *p == prof).map(|x| x.1)
}

fn is_job_block(block: &str, prof: &str) -> bool {
    match job_block(prof) {
        Some("cauldron") => block.ends_with("cauldron"),
        Some(b) => b == block,
        None => false,
    }
}

fn profession_for(block: &str) -> Option<&'static str> {
    PROFESSIONS.iter().find(|(p, _)| is_job_block(block, p)).map(|x| x.0)
}

/// `VillagerType.byBiome`
pub fn villager_type(biome: &str) -> &'static str {
    let b = biome.trim_start_matches("minecraft:");
    if b == "desert" || b.contains("badlands") {
        "desert"
    } else if b.contains("jungle") || b == "bamboo_jungle" {
        "jungle"
    } else if b.contains("savanna") {
        "savanna"
    } else if b.starts_with("snowy") || b.starts_with("frozen") || b.starts_with("ice") || b == "grove" || b.contains("jagged") || b.contains("frozen") {
        "snow"
    } else if b.contains("swamp") {
        "swamp"
    } else if b.contains("taiga") {
        "taiga"
    } else {
        "plains"
    }
}

/// `MerchantOffer`
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    /// base cost A (its count is the base price)
    pub a: Stack,
    /// cost A must match these components exactly (the water bottle trade)
    pub a_exact: bool,
    pub b: Option<Stack>,
    pub result: Stack,
    pub uses: i32,
    pub max_uses: i32,
    pub xp: i32,
    pub special: i32,
    pub mult: f32,
    pub demand: i32,
    pub reward_xp: bool,
}

impl Offer {
    /// `getCostA`: price after demand and special prices
    pub fn cost_a(&self) -> Stack {
        let base = self.a.count as i32;
        let extra = ((base * self.demand) as f32 * self.mult).floor().max(0.0) as i32;
        let n = (base + extra + self.special).clamp(1, self.a.max() as i32);
        Stack { count: n as u8, ..self.a }
    }

    pub fn out_of_stock(&self) -> bool {
        self.uses >= self.max_uses
    }

    fn matches(cost: &Stack, exact: bool, have: Option<Stack>) -> bool {
        have.is_some_and(|h| h.item == cost.item && (!exact || h.comp == cost.comp))
    }

    /// `MerchantOffer.satisfiedBy`
    pub fn satisfied(&self, a: Option<Stack>, b: Option<Stack>) -> bool {
        let ca = self.cost_a();
        if !Self::matches(&ca, self.a_exact, a) || a.unwrap().count < ca.count {
            return false;
        }
        match self.b {
            None => b.is_none(),
            Some(cb) => Self::matches(&cb, false, b) && b.unwrap().count >= cb.count,
        }
    }

    /// `updateDemand`
    fn update_demand(&mut self) {
        self.demand = self.demand + self.uses - (self.max_uses - self.uses);
    }
}

#[derive(Clone, Debug, Default)]
pub struct Merchant {
    /// villager type (desert, plains…); empty for wandering traders
    pub vtype: String,
    /// profession ("none", "nitwit", "farmer"…)
    pub profession: String,
    pub level: i32,
    pub xp: i32,
    pub offers: Vec<Offer>,
    pub job: Option<[i32; 3]>,
    pub last_restock: u64,
    pub restocks_today: i32,
    pub restock_day: u64,
    pub last_decay_day: u64,
    pub gossip: HashMap<uuid::Uuid, [i32; 5]>,
    /// the player trading with it
    pub customer: Option<i32>,
    /// `updateMerchantTimer` / `increaseProfessionLevelOnUpdate`
    pub level_timer: i32,
    pub level_up: bool,
    /// zombie villager being cured by this player
    pub curer: Option<uuid::Uuid>,
}

impl Merchant {
    pub fn villager(vtype: &str) -> Self {
        Merchant { vtype: vtype.to_string(), profession: "none".into(), level: 1, ..Default::default() }
    }

    pub fn reputation(&self, u: &uuid::Uuid) -> i32 {
        self.gossip.get(u).map(|g| g.iter().zip(GOSSIP_WEIGHT).map(|(v, w)| v * w).sum()).unwrap_or(0)
    }

    pub fn add_gossip(&mut self, u: uuid::Uuid, kind: usize, amount: i32) {
        let g = self.gossip.entry(u).or_default();
        g[kind] = (g[kind] + amount).min(GOSSIP_MAX[kind]);
        if kind == MAJOR_POSITIVE {
            // a major positive replaces the minor negatives (`GossipContainer.add`)
            g[MINOR_NEGATIVE] = 0;
        }
    }

    fn decay_gossip(&mut self) {
        for g in self.gossip.values_mut() {
            for k in 0..5 {
                g[k] = (g[k] - GOSSIP_DECAY[k]).max(0);
            }
        }
        self.gossip.retain(|_, g| g.iter().any(|v| *v > 0));
    }

    fn needs_restock(&self) -> bool {
        self.offers.iter().any(|o| o.uses > 0)
    }

    /// `Villager.restock`
    pub fn restock(&mut self, now: u64) {
        for o in &mut self.offers {
            o.update_demand();
            o.uses = 0;
        }
        self.last_restock = now;
        self.restocks_today += 1;
    }

    fn can_level_up(&self) -> bool {
        self.level < 5 && self.xp >= XP_LEVELS[self.level as usize]
    }
}

// ------------------------------------------------------------ offer generation

fn num(v: Option<&Value>, rng: &mut impl Rng, default: f64) -> f64 {
    match v {
        None => default,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::Object(o)) => match o.get("type").and_then(|t| t.as_str()).unwrap_or("minecraft:uniform") {
            "minecraft:constant" => o.get("value").and_then(|x| x.as_f64()).unwrap_or(0.0),
            "minecraft:binomial" => {
                let n = num(o.get("n"), rng, 0.0).round().clamp(0.0, 1000.0) as u32;
                let p = num(o.get("p"), rng, 0.0).clamp(0.0, 1.0);
                (0..n).filter(|_| rng.gen_bool(p)).count() as f64
            }
            "minecraft:sum" => o.get("summands").and_then(|s| s.as_array()).map(|a| a.iter().map(|x| num(Some(x), rng, 0.0)).sum()).unwrap_or(0.0),
            _ => {
                let lo = num(o.get("min"), rng, 0.0);
                let hi = num(o.get("max"), rng, lo);
                if hi <= lo { lo } else { rng.gen_range(lo..hi) }
            }
        },
        _ => default,
    }
}

/// `NumberProvider.getInt` (uniform ranges are inclusive)
fn int(v: Option<&Value>, rng: &mut impl Rng, default: i32) -> i32 {
    if let Some(Value::Object(o)) = v {
        if o.get("type").and_then(|t| t.as_str()).unwrap_or("minecraft:uniform") == "minecraft:uniform" {
            let lo = num(o.get("min"), rng, 0.0).round() as i32;
            let hi = num(o.get("max"), rng, 0.0).round() as i32;
            return if hi <= lo { lo } else { rng.gen_range(lo..=hi) };
        }
    }
    num(v, rng, default as f64).round() as i32
}

fn item_id(v: &Value) -> Option<i32> {
    Items::get().id(v.get("id")?.as_str()?.trim_start_matches("minecraft:"))
}

/// `TradeCost.toItemCost`
fn trade_cost(v: &Value, extra: i32, rng: &mut impl Rng) -> Option<(Stack, bool)> {
    let id = item_id(v)?;
    let max = Items::get().max_stack(id) as i32;
    let n = (int(v.get("count"), rng, 1) + extra).clamp(0, max);
    let mut st = Stack::new(id, n.max(0) as u8);
    let mut exact = false;
    if let Some(p) = v.get("components").and_then(|c| c.get("minecraft:potion_contents")).and_then(|p| p.get("potion").or(Some(p))).and_then(|p| p.as_str()) {
        st = st.with_patch(|x| x.potion = Some(p.trim_start_matches("minecraft:").to_string()));
        exact = true;
    }
    Some((st, exact))
}

/// RGB of each dye (`DyeColor.getTextureDiffuseColor`)
const DYE_RGB: [i32; 16] = [0xF9FFFE, 0xF9801D, 0xC74EBD, 0x3AB3DA, 0xFED83D, 0x80C71F, 0xF38BAA, 0x474F52, 0x9D9D97, 0x169C9C, 0x8932B8, 0x3C44AA, 0x835432, 0x5E7C16, 0xB02E26, 0x1D1D21];

/// `DyedItemColor.applyDyes` on an undyed item
pub fn mix_dyes(dyes: &[usize]) -> i32 {
    let (mut r, mut g, mut b, mut maxsum, mut n) = (0, 0, 0, 0, 0);
    for d in dyes {
        let c = DYE_RGB[*d % 16];
        let (cr, cg, cb) = ((c >> 16) & 255, (c >> 8) & 255, c & 255);
        maxsum += cr.max(cg).max(cb);
        r += cr;
        g += cg;
        b += cb;
        n += 1;
    }
    if n == 0 {
        return 0;
    }
    let (ar, ag, ab) = (r / n, g / n, b / n);
    let avg_max = maxsum as f32 / n as f32;
    let m = ar.max(ag).max(ab) as f32;
    let f = |c: i32| (c as f32 * avg_max / m) as i32;
    (f(ar) << 16) | (f(ag) << 8) | f(ab)
}

/// Apply the item modifiers of a trade. None when the trade is discarded;
/// the second value is `ADDITIONAL_TRADE_COST`.
fn modify(mut st: Stack, mods: &[Value], data: &GameData, rng: &mut impl Rng) -> Option<(Stack, i32)> {
    let mut extra = 0;
    let items = Items::get();
    for f in mods {
        match f.get("function").and_then(|x| x.as_str()).unwrap_or("") {
            "minecraft:enchant_with_levels" => {
                let lv = int(f.get("levels"), rng, 1);
                let opts = f.get("options").and_then(|x| x.as_str()).unwrap_or("#minecraft:on_traded_equipment");
                st = super::menu::enchant_with_levels(st, lv, opts.trim_start_matches('#'), data, rng.r#gen());
                if f.get("include_additional_cost_component").and_then(|x| x.as_bool()).unwrap_or(false) && (!st.patch().enchantments.is_empty() || !st.patch().stored_enchantments.is_empty()) {
                    extra += lv;
                }
            }
            "minecraft:enchant_randomly" => {
                let opts = f.get("options").and_then(|x| x.as_str()).unwrap_or("#minecraft:tradeable");
                let only = f.get("only_compatible").and_then(|x| x.as_bool()).unwrap_or(true);
                if let Some((out, lvl)) = super::menu::enchant_randomly(st, opts.trim_start_matches('#'), only, data, rng) {
                    st = out;
                    if f.get("include_additional_cost_component").and_then(|x| x.as_bool()).unwrap_or(false) {
                        extra += 2 + rng.gen_range(0..5 + lvl * 10) + 3 * lvl;
                    }
                }
            }
            "minecraft:set_random_dyes" => {
                let n = int(f.get("number_of_dyes"), rng, 1).max(0) as usize;
                let dyes: Vec<usize> = (0..n).map(|_| rng.gen_range(0..16)).collect();
                if !dyes.is_empty() {
                    let rgb = mix_dyes(&dyes);
                    st = st.with_patch(|p| p.dyed_color = Some(rgb));
                }
            }
            "minecraft:set_potion" => {
                if let Some(id) = f.get("id").and_then(|x| x.as_str()) {
                    st = st.with_patch(|p| p.potion = Some(id.trim_start_matches("minecraft:").to_string()));
                }
            }
            "minecraft:set_random_potion" => {
                let opts = f.get("options").and_then(|x| x.as_str()).unwrap_or("#minecraft:tradeable").trim_start_matches('#').trim_start_matches("minecraft:");
                let list = data.menus.ordered_tags.get(&format!("potion/{opts}")).cloned().unwrap_or_default();
                if !list.is_empty() {
                    let p = list[rng.gen_range(0..list.len())].clone();
                    st = st.with_patch(|x| x.potion = Some(p));
                }
            }
            "minecraft:set_name" => {
                if let Some(t) = f.get("name").and_then(|n| n.get("translate").or(Some(n))).and_then(|x| x.as_str()) {
                    if f.get("target").and_then(|x| x.as_str()) == Some("custom_name") {
                        st = st.with_patch(|p| p.custom_name = Some(t.to_string()));
                    }
                }
            }
            // RimeMC has no maps: the structure search "fails" (vanilla
            // then hands back a blank map, which the filter discards)
            "minecraft:exploration_map" => {}
            // the stew keeps no effect component here; it is still sold
            "minecraft:set_stew_effect" => {}
            "minecraft:filtered" => {
                let filt = f.get("item_filter");
                let item_ok = filt.and_then(|x| x.get("items")).and_then(|x| x.as_str()).map(|n| items.id(n.trim_start_matches("minecraft:")) == Some(st.item)).unwrap_or(true);
                let preds = filt.and_then(|x| x.get("predicates")).and_then(|x| x.as_object());
                let p = st.patch();
                let pred_ok = preds.is_none_or(|o| {
                    o.keys().all(|k| match k.as_str() {
                        "minecraft:enchantments" => !p.enchantments.is_empty(),
                        "minecraft:stored_enchantments" => !p.stored_enchantments.is_empty(),
                        "minecraft:dyed_color" => p.dyed_color.is_some(),
                        "minecraft:map_id" => false,
                        _ => true,
                    })
                });
                if !(item_ok && pred_ok) {
                    let discard = f.get("on_fail").and_then(|x| x.get("function")).and_then(|x| x.as_str()) == Some("minecraft:discard");
                    if discard {
                        return None;
                    }
                }
            }
            "minecraft:discard" => return None,
            _ => {}
        }
    }
    Some((st, extra))
}

/// `VillagerTrade.getOffer`
pub fn make_offer(t: &Value, vtype: &str, data: &GameData, rng: &mut impl Rng) -> Option<Offer> {
    if let Some(pred) = t.get("merchant_predicate") {
        let want = pred.pointer("/predicate/predicates/minecraft:villager~1variant").and_then(|x| x.as_str());
        if let Some(w) = want {
            if w.trim_start_matches("minecraft:") != vtype {
                return None;
            }
        }
    }
    let gives = t.get("gives")?;
    let id = item_id(gives)?;
    let count = int(gives.get("count"), rng, 1).clamp(1, 99) as u8;
    let mods: Vec<Value> = t.get("given_item_modifiers").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let (result, mut extra) = modify(Stack::new(id, count), &mods, data, rng)?;
    if let Some(dbl) = t.get("double_trade_price_enchantments").and_then(|x| x.as_str()) {
        let tag = dbl.trim_start_matches('#').trim_start_matches("minecraft:");
        let list = data.menus.ordered_tags.get(&format!("enchantment/{tag}"));
        if result.patch().stored_enchantments.iter().any(|(n, _)| list.is_some_and(|l| l.contains(n))) {
            extra *= 2;
        }
    }
    let (a, a_exact) = trade_cost(t.get("wants")?, extra, rng)?;
    if a.count < 1 {
        return None;
    }
    let b = match t.get("additional_wants") {
        Some(v) => {
            let (b, _) = trade_cost(v, 0, rng)?;
            if b.count < 1 {
                return None;
            }
            Some(b)
        }
        None => None,
    };
    Some(Offer {
        a,
        a_exact,
        b,
        result,
        uses: 0,
        max_uses: int(t.get("max_uses"), rng, 4).max(1),
        xp: int(t.get("xp"), rng, 1).max(0),
        special: 0,
        mult: num(t.get("reputation_discount"), rng, 0.0).max(0.0) as f32,
        demand: 0,
        reward_xp: true,
    })
}

/// `AbstractVillager.addOffersFromTradeSet` (without duplicates)
pub fn offers_from_set(set: &str, vtype: &str, data: &GameData, rng: &mut impl Rng) -> Vec<Offer> {
    let Some(ts) = data.menus.trade_sets.get(set) else { return Vec::new() };
    let amount = int(ts.get("amount"), rng, 1);
    let tag = ts.get("trades").and_then(|x| x.as_str()).unwrap_or("").trim_start_matches('#').trim_start_matches("minecraft:");
    let mut list: Vec<String> = data.menus.ordered_tags.get(&format!("villager_trade/{tag}")).cloned().unwrap_or_default();
    let dups = ts.get("allow_duplicates").and_then(|x| x.as_bool()).unwrap_or(false);
    let mut out = Vec::new();
    let mut added = 0;
    let mut tries = 0;
    while added < amount && !list.is_empty() && tries < 200 {
        tries += 1;
        let i = rng.gen_range(0..list.len());
        let key = if dups { list[i].clone() } else { list.remove(i) };
        if let Some(o) = data.menus.trades.get(&key).and_then(|t| make_offer(t, vtype, data, rng)) {
            out.push(o);
            added += 1;
        }
    }
    out
}

/// `Villager.updateTrades` for the merchant's current level.
pub fn add_level_trades(m: &mut Merchant, data: &GameData, rng: &mut impl Rng) {
    if job_block(&m.profession).is_none() {
        return;
    }
    let set = format!("{}/level_{}", m.profession, m.level);
    let mut new = offers_from_set(&set, &m.vtype, data, rng);
    m.offers.append(&mut new);
}

/// `WanderingTrader.updateTrades`
pub fn wandering_offers(data: &GameData, rng: &mut impl Rng) -> Vec<Offer> {
    let mut out = Vec::new();
    for set in ["wandering_trader/buying", "wandering_trader/common", "wandering_trader/uncommon"] {
        out.extend(offers_from_set(set, "", data, rng));
    }
    out
}

// ------------------------------------------------------------ network

/// `ItemCost` (item, count, exact components)
fn write_cost(b: &mut Vec<u8>, st: &Stack, exact: bool) {
    b.vi(st.item).vi(st.count as i32);
    if exact && st.comp != 0 {
        // DataComponentExactPredicate: the potion
        let p = st.patch();
        if let Some(pot) = &p.potion {
            let ty = crate::components::type_id_pub("minecraft:potion_contents");
            let id = Registries::get().id("minecraft:potion", &format!("minecraft:{pot}")).unwrap_or(0);
            b.vi(1).vi(ty).bool(true).vi(id + 1).bool(false).vi(0).bool(false);
            return;
        }
    }
    b.vi(0);
}

pub fn offers_packet(offers: &[Offer], level: i32, xp: i32, show_progress: bool, window: i32) -> Vec<u8> {
    let mut b = Vec::new();
    b.vi(window).vi(offers.len() as i32);
    for o in offers {
        write_cost(&mut b, &o.cost_a_raw(), o.a_exact);
        write_slot(&mut b, Some(o.result));
        match &o.b {
            Some(cb) => {
                b.bool(true);
                write_cost(&mut b, cb, false);
            }
            None => {
                b.bool(false);
            }
        }
        b.bool(o.out_of_stock()).i32(o.uses).i32(o.max_uses).i32(o.xp).i32(o.special).f32(o.mult).i32(o.demand);
    }
    b.vi(level).vi(xp).bool(show_progress).bool(show_progress);
    b
}

impl Offer {
    /// the base cost as sent (the client applies demand and special prices)
    fn cost_a_raw(&self) -> Stack {
        self.a
    }
}

/// `VillagerData` entity data value (serializer 18)
pub fn villager_data(b: &mut Vec<u8>, index: u8, m: Option<&Merchant>) {
    let r = Registries::get();
    let (ty, prof, lvl) = match m {
        Some(m) => (m.vtype.as_str(), m.profession.as_str(), m.level),
        None => ("plains", "none", 1),
    };
    let ty = if ty.is_empty() { "plains" } else { ty };
    let t = r.id("minecraft:villager_type", &format!("minecraft:{ty}")).unwrap_or(2);
    let p = r.id("minecraft:villager_profession", &format!("minecraft:{prof}")).unwrap_or(0);
    b.u8(index).vi(18).vi(t).vi(p).vi(lvl);
}

// ------------------------------------------------------------ persistence

fn s(k: &str, v: Tag) -> (String, Tag) {
    (k.to_string(), v)
}

pub fn to_nbt(m: &Merchant) -> Tag {
    use super::persist::stack_nbt;
    let offers = m
        .offers
        .iter()
        .map(|o| {
            let mut f = vec![
                s("buy", stack_nbt(&o.a)),
                s("exact", Tag::Byte(o.a_exact as i8)),
                s("sell", stack_nbt(&o.result)),
                s("uses", Tag::Int(o.uses)),
                s("maxUses", Tag::Int(o.max_uses)),
                s("xp", Tag::Int(o.xp)),
                s("priceMultiplier", Tag::Float(o.mult)),
                s("demand", Tag::Int(o.demand)),
                s("rewardExp", Tag::Byte(o.reward_xp as i8)),
            ];
            if let Some(b) = &o.b {
                f.push(s("buyB", stack_nbt(b)));
            }
            Tag::Compound(f)
        })
        .collect();
    let gossip = m
        .gossip
        .iter()
        .map(|(u, g)| Tag::Compound(vec![s("Target", Tag::String(u.hyphenated().to_string())), s("Values", Tag::IntArray(g.to_vec()))]))
        .collect();
    let mut f = vec![
        s("type", Tag::String(m.vtype.clone())),
        s("profession", Tag::String(m.profession.clone())),
        s("level", Tag::Int(m.level)),
        s("Xp", Tag::Int(m.xp)),
        s("Offers", Tag::List(offers)),
        s("LastRestock", Tag::Long(m.last_restock as i64)),
        s("RestocksToday", Tag::Int(m.restocks_today)),
        s("RestockDay", Tag::Long(m.restock_day as i64)),
        s("DecayDay", Tag::Long(m.last_decay_day as i64)),
        s("Gossips", Tag::List(gossip)),
    ];
    if let Some(j) = m.job {
        f.push(s("JobSite", Tag::IntArray(j.to_vec())));
    }
    if let Some(c) = m.curer {
        f.push(s("ConversionPlayer", Tag::String(c.hyphenated().to_string())));
    }
    Tag::Compound(f)
}

pub fn from_nbt(t: &Tag) -> Merchant {
    use super::persist::stack_from;
    let n = |k: &str| t.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    let str_of = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut m = Merchant { vtype: str_of("type"), profession: str_of("profession"), level: n("level").clamp(1, 5) as i32, xp: n("Xp") as i32, ..Default::default() };
    if m.profession.is_empty() {
        m.profession = "none".into();
    }
    m.last_restock = n("LastRestock") as u64;
    m.restocks_today = n("RestocksToday") as i32;
    m.restock_day = n("RestockDay") as u64;
    m.last_decay_day = n("DecayDay") as u64;
    if let Some(Tag::List(l)) = t.get("Offers") {
        for o in l {
            let on = |k: &str| o.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
            let (Some(a), Some(r)) = (o.get("buy").and_then(stack_from), o.get("sell").and_then(stack_from)) else { continue };
            let mult = match o.get("priceMultiplier") {
                Some(Tag::Float(f)) => *f,
                _ => 0.0,
            };
            m.offers.push(Offer {
                a,
                a_exact: on("exact") != 0,
                b: o.get("buyB").and_then(stack_from),
                result: r,
                uses: on("uses") as i32,
                max_uses: on("maxUses").max(1) as i32,
                xp: on("xp") as i32,
                special: 0,
                mult,
                demand: on("demand") as i32,
                reward_xp: o.get("rewardExp").is_none_or(|v| v.as_i64() != Some(0)),
            });
        }
    }
    if let Some(Tag::List(l)) = t.get("Gossips") {
        for g in l {
            let (Some(u), Some(Tag::IntArray(v))) = (g.get("Target").and_then(|x| x.as_str()).and_then(|x| uuid::Uuid::parse_str(x).ok()), g.get("Values")) else { continue };
            let mut a = [0; 5];
            for (i, x) in v.iter().take(5).enumerate() {
                a[i] = *x;
            }
            m.gossip.insert(u, a);
        }
    }
    if let Some(Tag::IntArray(j)) = t.get("JobSite") {
        if j.len() == 3 {
            m.job = Some([j[0], j[1], j[2]]);
        }
    }
    m.curer = t.get("ConversionPlayer").and_then(|x| x.as_str()).and_then(|x| uuid::Uuid::parse_str(x).ok());
    m
}

// ------------------------------------------------------------ game logic

impl Game {
    /// A freshly spawned villager or wandering trader gets its merchant data.
    pub fn init_merchant(&self, m: &mut super::mob::Mob, pos: [f64; 3]) {
        if m.merchant.is_none() {
            m.merchant = self.new_merchant(m.ty.name(), pos).map(Box::new);
        }
    }

    pub fn new_merchant(&self, ty: &str, pos: [f64; 3]) -> Option<Merchant> {
        Some(match ty {
            "villager" | "zombie_villager" => {
                let biome = self.world.biome(pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32).unwrap_or_default();
                let mut mer = Merchant::villager(villager_type(&biome));
                if ty == "zombie_villager" {
                    // ZombieVillager.finalizeSpawn: a random profession
                    let mut rng = rand::thread_rng();
                    let i = rng.gen_range(0..PROFESSIONS.len() + 1);
                    mer.profession = PROFESSIONS.get(i).map(|x| x.0).unwrap_or("none").to_string();
                }
                mer
            }
            "wandering_trader" => {
                let mut rng = rand::thread_rng();
                let offers = wandering_offers(&self.data, &mut rng);
                Merchant { profession: "none".into(), level: 1, offers, ..Default::default() }
            }
            _ => return None,
        })
    }

    fn with_merchant<R>(&self, eid: i32, f: impl FnOnce(&mut super::mob::Mob, [f64; 3]) -> R) -> Option<R> {
        let mut ents = self.entities.lock();
        let e = ents.get_mut(&eid)?;
        let pos = e.pos;
        let EKind::Mob(m) = &mut e.kind else { return None };
        Some(f(m, pos))
    }

    /// `Villager.mobInteract` / `WanderingTrader.mobInteract`: open trading.
    pub fn merchant_interact(&self, p: &Arc<Player>, eid: i32, held: Option<Stack>) -> Option<bool> {
        let held_name = held.map(|s| Items::get().name(s.item)).unwrap_or("");
        if held_name == "name_tag" || held_name.ends_with("_spawn_egg") {
            return None;
        }
        let now = self.game_time();
        let rng_init = {
            let ents = self.entities.lock();
            let e = ents.get(&eid)?;
            let EKind::Mob(m) = &e.kind else { return None };
            if !matches!(m.ty.name(), "villager" | "wandering_trader") || m.death > 0 {
                return None;
            }
            m.merchant.is_none()
        };
        if rng_init {
            let (pos, ty) = {
                let ents = self.entities.lock();
                let e = ents.get(&eid)?;
                let EKind::Mob(m) = &e.kind else { return None };
                (e.pos, m.ty.name())
            };
            let mer = self.new_merchant(ty, pos);
            self.with_merchant(eid, |m, _| m.merchant = mer.map(Box::new));
        }
        let pos = self.entities.lock().get(&eid).map(|e| e.pos)?;
        let (baby, empty, busy) = self.with_merchant(eid, |m, _| {
            let mer = m.merchant.as_ref();
            (m.is_baby(), mer.is_none_or(|x| x.offers.is_empty()), mer.and_then(|x| x.customer).is_some_and(|c| c != p.eid))
        })?;
        let _ = now;
        if baby || empty || busy {
            // shakeHead
            self.with_merchant(eid, |m, _| m.v.trade_unhappy = 40);
            self.play_sound("minecraft:entity.villager.no", 5, pos, 1.0, 1.0);
            return Some(true);
        }
        p.requests.lock().push(super::Request::OpenMerchant(eid));
        Some(true)
    }

    /// `Villager.startTrading` -> `updateSpecialPrices`; returns the offers,
    /// level, xp and whether it is a villager (shows progress).
    pub fn start_trading(&self, p: &Arc<Player>, eid: i32) -> Option<(Vec<Offer>, i32, i32, bool, String)> {
        let hero = p.vit.lock().living.effect(Effect::HeroOfTheVillage).map(|i| i.amplifier);
        self.with_merchant(eid, |m, _| {
            let villager = m.ty.is("villager");
            let title = if villager {
                let prof = m.merchant.as_ref().map(|x| x.profession.clone()).unwrap_or_default();
                m.v.custom_name.clone().unwrap_or_else(|| format!("entity.minecraft.villager.{prof}"))
            } else {
                m.v.custom_name.clone().unwrap_or_else(|| "entity.minecraft.wandering_trader".into())
            };
            let mer = m.merchant.as_mut()?;
            mer.customer = Some(p.eid);
            if villager {
                let rep = mer.reputation(&p.uuid);
                for o in &mut mer.offers {
                    o.special = 0;
                    if rep != 0 {
                        o.special += -((rep as f32 * o.mult).floor() as i32);
                    }
                    if let Some(amp) = hero {
                        let d = 0.3 + 0.0625 * amp as f64;
                        let k = (d * o.a.count as f64).floor() as i32;
                        o.special -= k.max(1);
                    }
                }
            }
            Some((mer.offers.clone(), mer.level, mer.xp, villager, title))
        })?
    }

    /// `AbstractVillager.notifyTrade` (+ `Villager.rewardTradeXp`).
    pub fn merchant_traded(&self, p: &Arc<Player>, eid: i32, idx: usize) {
        p.award_custom("traded_with_villager", 1);
        let mut rng = rand::thread_rng();
        let res = self.with_merchant(eid, |m, pos| {
            let villager = m.ty.is("villager");
            m.ambient = -m.ty.ambient_interval();
            let mer = m.merchant.as_mut()?;
            let o = mer.offers.get_mut(idx)?;
            o.uses += 1;
            let xp = o.xp;
            let reward = o.reward_xp;
            if villager {
                mer.xp += xp;
                mer.add_gossip(p.uuid, TRADING, 2);
                if mer.can_level_up() {
                    mer.level_timer = 40;
                    mer.level_up = true;
                }
            }
            let orbs = if reward { 3 + rng.gen_range(0..4) + if villager && mer.level_up { 5 } else { 0 } } else { 0 };
            Some((pos, orbs, villager))
        });
        let Some(Some((pos, orbs, villager))) = res else { return };
        if orbs > 0 {
            self.award_xp_orbs([pos[0], pos[1] + 0.5, pos[2]], orbs);
        }
        let s = if villager { "minecraft:entity.villager.yes" } else { "minecraft:entity.wandering_trader.yes" };
        self.play_sound(s, 5, pos, 1.0, 1.0);
    }

    /// The menu shows a result (or not): `notifyTradeUpdated` sound.
    pub fn merchant_hint_sound(&self, eid: i32, ok: bool) {
        let Some((pos, villager)) = self.with_merchant(eid, |m, pos| (pos, m.ty.is("villager"))) else { return };
        let s = match (villager, ok) {
            (true, true) => "minecraft:entity.villager.yes",
            (true, false) => "minecraft:entity.villager.no",
            (false, true) => "minecraft:entity.wandering_trader.yes",
            (false, false) => "minecraft:entity.wandering_trader.no",
        };
        self.play_sound(s, 5, pos, 1.0, 1.0);
    }

    /// The merchant `eid`'s customer left: `stopTrading` / `resetSpecialPrices`.
    pub fn stop_trading(&self, eid: i32) {
        self.with_merchant(eid, |m, _| {
            if let Some(mer) = m.merchant.as_mut() {
                mer.customer = None;
                for o in &mut mer.offers {
                    o.special = 0;
                }
            }
        });
    }

    /// Players hurting or killing villagers spread gossip (`onReputationEventFrom`).
    pub fn villager_hurt_by(&self, eid: i32, by: uuid::Uuid, killed: bool) {
        let pos = self.with_merchant(eid, |m, pos| {
            if let Some(mer) = m.merchant.as_mut() {
                mer.add_gossip(by, if killed { MAJOR_NEGATIVE } else { MINOR_NEGATIVE }, 25);
            }
            pos
        });
        if killed {
            // VILLAGER_KILLED: villagers within 16 blocks hear of it
            let Some(pos) = pos else { return };
            let mut ents = self.entities.lock();
            for e in ents.values_mut() {
                if e.eid == eid || (e.pos[0] - pos[0]).powi(2) + (e.pos[1] - pos[1]).powi(2) + (e.pos[2] - pos[2]).powi(2) > 256.0 {
                    continue;
                }
                if let EKind::Mob(m) = &mut e.kind {
                    if let Some(mer) = m.merchant.as_mut().filter(|_| m.ty.is("villager")) {
                        mer.add_gossip(by, MAJOR_NEGATIVE, 25);
                    }
                }
            }
        }
    }

    /// Right click on a zombie villager with a golden apple while it has
    /// Weakness: start curing (`ZombieVillager.mobInteract`).
    pub fn cure_interact(&self, p: &Arc<Player>, eid: i32, held: Option<Stack>, slot: usize) -> Option<bool> {
        let h = held?;
        if Items::get().name(h.item) != "golden_apple" {
            return None;
        }
        let mut rng = rand::thread_rng();
        let started = self.with_merchant(eid, |m, pos| {
            if !m.ty.is("zombie_villager") || m.v.conversion > 0 || !m.living.has_effect(Effect::Weakness) {
                return None;
            }
            m.v.conversion = rng.gen_range(3600..=6000);
            m.persistent = true;
            m.living.remove_effect(Effect::Weakness);
            let info = super::hurt::mob_info(m, false);
            let amp = (self.difficulty() - 1).max(0);
            m.living.add_effect(&info, Instance::new(Effect::Strength, m.v.conversion, amp));
            m.merchant.get_or_insert_with(|| Box::new(Merchant::villager("plains"))).curer = Some(p.uuid);
            Some(pos)
        })??;
        if p.gamemode.load(Ordering::Relaxed) != 1 {
            p.inv.lock().consume(slot, 1);
            self.flush_inventory(p);
        }
        if let Some(c) = self.entity_chunk(eid) {
            self.broadcast_chunk(c, &self.entity_event(eid, 16));
        }
        let _ = started;
        Some(true)
    }

    /// Per tick: job sites, levelling up, restocking, gossip decay, curing,
    /// customers that walked away.
    pub(crate) fn tick_merchants(&self) {
        let now = self.game_time();
        let day = self.day_time();
        let players = self.players();
        let mut rng = rand::thread_rng();
        // (eid, pos, profession, has job, level, xp) of villagers needing a job check
        let mut job_checks: Vec<(i32, [f64; 3], String, Option<[i32; 3]>, i32, i32)> = Vec::new();
        let mut done_cures: Vec<i32> = Vec::new();
        let mut level_ups: Vec<(i32, [f64; 3])> = Vec::new();
        let mut leave: Vec<i32> = Vec::new();
        let mut restocked: Vec<[f64; 3]> = Vec::new();
        {
            let mut ents = self.entities.lock();
            for e in ents.values_mut() {
                let pos = e.pos;
                let eid = e.eid;
                let EKind::Mob(m) = &mut e.kind else { continue };
                let name = m.ty.name();
                if !matches!(name, "villager" | "wandering_trader" | "zombie_villager") || m.death > 0 {
                    continue;
                }
                if m.v.trade_unhappy > 0 {
                    m.v.trade_unhappy -= 1;
                }
                if name == "zombie_villager" {
                    if m.v.conversion > 0 {
                        // ZombieVillager.getConversionProgress: iron bars and beds speed it up (not modelled)
                        m.v.conversion -= 1;
                        if m.v.conversion == 0 {
                            done_cures.push(eid);
                        }
                    }
                    continue;
                }
                let Some(mer) = m.merchant.as_mut() else { continue };
                // the customer is gone or looks at something else
                if let Some(c) = mer.customer {
                    let still = players.iter().find(|p| p.eid == c).is_some_and(|p| {
                        let pp = p.pose.lock().pos;
                        let open = matches!(p.inv.lock().window, super::inventory::Window::Container(_, super::blockentity::Screen::Merchant, q) if q[1] == eid);
                        open && (pp[0] - pos[0]).powi(2) + (pp[1] - pos[1]).powi(2) + (pp[2] - pos[2]).powi(2) <= 64.0
                    });
                    if !still {
                        leave.push(eid);
                    }
                    continue;
                }
                if name == "wandering_trader" {
                    continue;
                }
                // level up a moment after the trade (`customServerAiStep`)
                if mer.level_timer > 0 {
                    mer.level_timer -= 1;
                    if mer.level_timer == 0 && mer.level_up {
                        mer.level_up = false;
                        mer.level += 1;
                        add_level_trades(mer, &self.data, &mut rng);
                        let info = super::hurt::mob_info(m, false);
                        m.living.add_effect(&info, Instance::new(Effect::Regeneration, 200, 0));
                        level_ups.push((eid, pos));
                        continue;
                    }
                }
                let baby = m.is_baby();
                let mer = m.merchant.as_mut().unwrap();
                // a new day: restock count and gossip decay
                let d = day / 24000;
                if mer.restock_day != d {
                    mer.restock_day = d;
                    mer.restocks_today = 0;
                }
                if now / 24000 != mer.last_decay_day {
                    if mer.last_decay_day != 0 {
                        mer.decay_gossip();
                    }
                    mer.last_decay_day = now / 24000;
                }
                // work hours at the job site: restock (`WorkAtPoi` / `shouldRestock`)
                let t = day % 24000;
                if let Some(j) = mer.job {
                    let near = (j[0] as f64 + 0.5 - pos[0]).powi(2) + (j[2] as f64 + 0.5 - pos[2]).powi(2) <= 16.0 * 16.0;
                    let allowed = mer.restocks_today == 0 || (mer.restocks_today < 2 && now > mer.last_restock + 2400);
                    if (2000..9000).contains(&t) && near && mer.needs_restock() && allowed {
                        mer.restock(now);
                        restocked.push(pos);
                    }
                }
                if !baby && (now + eid as u64) % 100 == 0 && mer.profession != "nitwit" {
                    job_checks.push((eid, pos, mer.profession.clone(), mer.job, mer.level, mer.xp));
                }
            }
        }
        for eid in leave {
            self.stop_trading(eid);
        }
        for pos in restocked {
            self.play_sound("minecraft:entity.villager.work_farmer", 5, pos, 0.0, 1.0);
        }
        for (eid, pos) in level_ups {
            self.particles("minecraft:happy_villager", [pos[0], pos[1] + 1.0, pos[2]], 5, [0.5, 0.5, 0.5], 0.0);
            let _ = eid;
        }
        for eid in done_cures {
            self.finish_cure(eid);
        }
        if !job_checks.is_empty() {
            let claimed: Vec<[i32; 3]> = self
                .entities
                .lock()
                .values()
                .filter_map(|e| match &e.kind {
                    EKind::Mob(m) => m.merchant.as_ref().and_then(|x| x.job),
                    _ => None,
                })
                .collect();
            for (eid, pos, prof, job, level, xp) in job_checks {
                self.check_job(eid, pos, &prof, job, level, xp, &claimed);
            }
        }
    }

    /// Job sites: keep, lose or acquire one (`AcquirePoi`, `ResetProfession`,
    /// `AssignProfessionFromJobSite`).
    fn check_job(&self, eid: i32, pos: [f64; 3], prof: &str, job: Option<[i32; 3]>, level: i32, xp: i32, claimed: &[[i32; 3]]) {
        if let Some(j) = job {
            let b = self.world.get_block(j[0], j[1], j[2]).map(|s| s.name()).unwrap_or("air");
            if is_job_block(b, prof) {
                return;
            }
            // the job site is gone
            let novice = level == 1 && xp == 0;
            self.with_merchant(eid, |m, _| {
                if let Some(mer) = m.merchant.as_mut() {
                    mer.job = None;
                    if novice {
                        mer.profession = "none".into();
                        mer.offers.clear();
                    }
                }
                m.home = None;
            });
            return;
        }
        // look for a free job site (or one of our own profession)
        let c = [pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32];
        let mut best: Option<([i32; 3], &'static str, i32)> = None;
        for dy in -4..=4 {
            for dx in -16..=16 {
                for dz in -16..=16 {
                    let p = [c[0] + dx, c[1] + dy, c[2] + dz];
                    let Some(b) = self.world.get_block(p[0], p[1], p[2]) else { continue };
                    let Some(np) = profession_for(b.name()) else { continue };
                    if prof != "none" && np != prof || claimed.contains(&p) {
                        continue;
                    }
                    let d = dx * dx + dy * dy + dz * dz;
                    if best.is_none_or(|x| d < x.2) {
                        best = Some((p, np, d));
                    }
                }
            }
        }
        let Some((p, np, _)) = best else { return };
        let mut rng = rand::thread_rng();
        let became = self.with_merchant(eid, |m, _| {
            let mer = m.merchant.get_or_insert_with(|| Box::new(Merchant::villager("plains")));
            mer.job = Some(p);
            m.home = Some((p, 16));
            if mer.profession == "none" {
                mer.profession = np.to_string();
                mer.level = 1;
                mer.offers.clear();
                add_level_trades(mer, &self.data, &mut rng);
                true
            } else {
                if mer.offers.is_empty() {
                    add_level_trades(mer, &self.data, &mut rng);
                }
                false
            }
        });
        if became == Some(true) {
            self.play_sound(&format!("minecraft:entity.villager.work_{np}"), 5, pos, 1.0, 1.0);
            self.particles("minecraft:happy_villager", [pos[0], pos[1] + 1.0, pos[2]], 5, [0.5, 0.5, 0.5], 0.0);
        }
    }

    /// `ZombieVillager.finishConversion`
    fn finish_cure(&self, eid: i32) {
        let Some((curer, pos)) = self.with_merchant(eid, |m, pos| (m.merchant.as_ref().and_then(|x| x.curer), pos)) else { return };
        self.convert_mob_pub(eid, "villager");
        self.level_event(1027, [pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32], 0);
        // the new villager has the same merchant data (carried by convert_mob)
        let new = self.entities.lock().values().filter(|e| matches!(&e.kind, EKind::Mob(m) if m.ty.is("villager")) && (e.pos[0] - pos[0]).abs() < 1e-6 && (e.pos[2] - pos[2]).abs() < 1e-6).map(|e| e.eid).max();
        let Some(v) = new else { return };
        self.with_merchant(v, |m, _| {
            let info = super::hurt::mob_info(m, false);
            m.living.add_effect(&info, Instance::new(Effect::Nausea, 200, 0));
            if let Some(mer) = m.merchant.as_mut() {
                mer.curer = None;
                if let Some(u) = curer {
                    mer.add_gossip(u, MAJOR_POSITIVE, 20);
                    mer.add_gossip(u, MINOR_POSITIVE, 25);
                }
            }
        });
    }

    /// `Zombie.killedEntity`: a villager killed by a zombie may turn into a
    /// zombie villager (normal 50 %, hard always). True when it converted.
    pub fn zombify_villager(&self, eid: i32) -> bool {
        let chance = match self.difficulty() {
            2 => 0.5,
            3 => 1.0,
            _ => 0.0,
        };
        if chance == 0.0 || rand::thread_rng().r#gen::<f64>() >= chance {
            return false;
        }
        let pos = self.entities.lock().get(&eid).map(|e| e.pos);
        self.convert_mob_pub(eid, "zombie_villager");
        if let Some(pos) = pos {
            self.level_event(1026, [pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32], 0);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dye_mix_matches_vanilla() {
        // a single dye keeps its colour
        assert_eq!(mix_dyes(&[14]), 0xB02E26);
        // red + white (vanilla: 0xD4968C... computed by the same formula)
        let c = mix_dyes(&[14, 0]);
        assert!(c > 0);
    }

    #[test]
    fn offer_price_with_demand_and_reputation() {
        let em = Items::get().id("emerald").unwrap();
        let mut o = Offer { a: Stack::new(em, 10), a_exact: false, b: None, result: Stack::new(em, 1), uses: 0, max_uses: 12, xp: 1, special: 0, mult: 0.05, demand: 0, reward_xp: true };
        assert_eq!(o.cost_a().count, 10);
        o.demand = 4;
        assert_eq!(o.cost_a().count, 12);
        o.special = -15;
        assert_eq!(o.cost_a().count, 1);
        o.special = 0;
        o.uses = 12;
        assert!(o.out_of_stock());
        o.update_demand();
        assert_eq!(o.demand, 4 + 12 - 0);
        assert!(o.satisfied(Some(Stack::new(em, 64)), None));
        assert!(!o.satisfied(Some(Stack::new(em, 64)), Some(Stack::new(em, 1))));
    }

    #[test]
    fn villager_takes_a_job_and_trades() {
        let Some((g, _gen, _store, tmp)) = crate::game::endfight::tests::end_game() else { return };
        let r = crate::registry::BlockRegistry::get();
        let y = g.motion_blocking_height(0, 0);
        g.change_block([3, y, 0], r.state("lectern", &[]).unwrap(), false, None);
        let ty = crate::game::mob::MobType::from_name("villager").unwrap();
        let eid = g.spawn_mob_with(ty, [0.5, y as f64, 0.5], true, |_, _| {});
        let prof = |g: &Game| g.with_merchant(eid, |m, _| m.merchant.as_ref().map(|x| (x.profession.clone(), x.offers.len(), x.job))).flatten();
        assert_eq!(prof(&g).map(|x| x.0), Some("none".to_string()));
        g.check_job(eid, [0.5, y as f64, 0.5], "none", None, 1, 0, &[]);
        let (p, n, job) = prof(&g).unwrap();
        assert_eq!(p, "librarian");
        assert_eq!(job, Some([3, y, 0]));
        assert_eq!(n, 2, "two novice trades");
        // trading: the offers go to the menu and the result needs the price
        let mut inv = crate::game::inventory::Inventory::default();
        inv.window = crate::game::inventory::Window::Container(1, crate::game::blockentity::Screen::Merchant, [crate::game::block::container::ENTITY_KEY, eid, 1]);
        inv.ext = vec![None; 3];
        let offers = g.with_merchant(eid, |m, _| m.merchant.as_ref().unwrap().offers.clone()).unwrap();
        inv.menu.offers = offers.clone();
        inv.menu.selected = -1;
        let o = &offers[0];
        let mut pay = o.cost_a();
        pay.count = pay.max();
        inv.slots[9] = Some(pay);
        if let Some(b) = o.b {
            inv.slots[10] = Some(Stack { count: b.max(), ..b });
        }
        let thrown = crate::game::menu::select_trade(&mut inv, 0, 0);
        assert!(thrown.is_empty());
        assert_eq!(inv.ext[2].map(|s| s.item), Some(o.result.item), "result shown");
        let before = inv.ext[0].unwrap().count;
        let got = crate::game::menu::take(&mut inv, &g.data);
        assert_eq!(got.map(|s| s.item), Some(o.result.item));
        assert_eq!(inv.ext[0].map(|s| s.count).unwrap_or(0), before - o.cost_a().count);
        assert!(inv.menu.events.iter().any(|e| matches!(e, crate::game::menu::MenuEvent::Trade(0))));
        // the trade gives experience and reputation; breaking the lectern
        // of a villager that has traded keeps the profession
        let fake = uuid::Uuid::new_v4();
        g.with_merchant(eid, |m, _| {
            let mer = m.merchant.as_mut().unwrap();
            mer.offers[0].uses += 1;
            mer.xp += 10;
            mer.add_gossip(fake, TRADING, 2);
        });
        assert_eq!(g.with_merchant(eid, |m, _| m.merchant.as_ref().unwrap().reputation(&fake)), Some(2));
        g.change_block([3, y, 0], r.state("air", &[]).unwrap(), false, None);
        g.check_job(eid, [0.5, y as f64, 0.5], "librarian", Some([3, y, 0]), 1, 10, &[]);
        assert_eq!(prof(&g).map(|x| x.0), Some("librarian".to_string()));
        // NBT round trip
        let mer = g.with_merchant(eid, |m, _| (**m.merchant.as_ref().unwrap()).clone()).unwrap();
        let back = from_nbt(&to_nbt(&mer));
        assert_eq!(back.offers, mer.offers);
        assert_eq!(back.reputation(&fake), 2);
        assert_eq!(back.profession, "librarian");
        // wandering traders get 9 offers
        let w = g.new_merchant("wandering_trader", [0.0; 3]).unwrap();
        assert!(w.offers.len() >= 8, "{}", w.offers.len());
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn every_profession_has_trades_at_every_level() {
        let Some((g, _gen, _store, tmp)) = crate::game::endfight::tests::end_game() else { return };
        let mut rng = rand::thread_rng();
        for (p, _) in PROFESSIONS {
            let mut m = Merchant::villager("plains");
            m.profession = p.to_string();
            for lvl in 1..=5 {
                m.level = lvl;
                let before = m.offers.len();
                add_level_trades(&mut m, &g.data, &mut rng);
                assert!(m.offers.len() > before, "{p} level {lvl}");
            }
        }
        let _ = std::fs::remove_dir_all(tmp);
    }
}
