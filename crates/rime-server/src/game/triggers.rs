//! Advancement criteria triggers (`CriteriaTriggers`): each helper runs one
//! trigger with the instance's conditions tested against the event.

use super::advancement::{in_range, Eval, View};
use super::{Game, Player};
use crate::data::Stack;
use crate::game::combat::damage::DamageSource;
use crate::registry::BlockState;
use crate::world::storage::Dim;
use serde_json::Value;
use std::sync::Arc;

fn strip(s: &str) -> &str {
    s.strip_prefix("minecraft:").unwrap_or(s)
}

impl Eval<'_> {
    /// Location-style conditions of `placed_block` / `item_used_on_block`:
    /// loot conditions about a block, its position and the tool.
    pub fn block_conditions(&self, conds: Option<&Value>, pos: [i32; 3], state: BlockState, tool: Option<Stack>) -> bool {
        let Some(Value::Array(list)) = conds else { return true };
        list.iter().all(|c| self.block_condition(c, pos, state, tool))
    }

    fn block_condition(&self, c: &Value, pos: [i32; 3], state: BlockState, tool: Option<Stack>) -> bool {
        let terms = || c.get("terms").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        match c.get("condition").and_then(|x| x.as_str()).map(strip).unwrap_or("") {
            "location_check" => {
                let off = |k: &str| c.get(format!("offset{k}").as_str()).and_then(|x| x.as_i64()).unwrap_or(0) as f64;
                let p = [pos[0] as f64 + 0.5 + off("X"), pos[1] as f64 + 0.5 + off("Y"), pos[2] as f64 + 0.5 + off("Z")];
                c.get("predicate").is_none_or(|pr| self.location(pr, self.g.dim, p))
            }
            "block_state_property" => {
                if let Some(b) = c.get("block").and_then(|x| x.as_str()) {
                    if strip(b) != state.name() {
                        return false;
                    }
                }
                c.get("properties").and_then(|p| p.as_object()).is_none_or(|props| {
                    props.iter().all(|(k, v)| {
                        let Some(cur) = state.prop(k) else { return false };
                        match v {
                            Value::String(s) => cur == s,
                            o => cur.parse::<f64>().is_ok_and(|n| in_range(Some(o), n)),
                        }
                    })
                })
            }
            "match_tool" => self.item(c.get("predicate"), tool),
            "inverted" => c.get("term").is_some_and(|t| !self.block_condition(t, pos, state, tool)),
            "any_of" => terms().iter().any(|t| self.block_condition(t, pos, state, tool)),
            "all_of" => terms().iter().all(|t| self.block_condition(t, pos, state, tool)),
            _ => false,
        }
    }

    /// `effects` of an `EntityPredicate`-style map against a player.
    fn effects(&self, p: Option<&Value>, who: &Arc<Player>) -> bool {
        let Some(map) = p.and_then(|x| x.as_object()) else { return true };
        let v = who.vit.lock();
        map.iter().all(|(name, cond)| {
            let Some(e) = crate::game::combat::effects::Effect::from_name(strip(name)) else { return false };
            match v.living.effect(e) {
                Some(i) => in_range(cond.get("amplifier"), i.amplifier as f64) && in_range(cond.get("duration"), i.duration as f64),
                None => false,
            }
        })
    }
}

impl Game {
    fn view_opt(&self, eid: Option<i32>) -> Option<View> {
        eid.and_then(|e| self.view_any(e))
    }

    pub fn trigger_recipe_crafted(&self, p: &Arc<Player>, id: &str) {
        self.trigger(p, "recipe_crafted", |c, _| c.get("recipe_id").and_then(|x| x.as_str()).is_none_or(|r| strip(r) == id));
    }

    /// `InventoryChangeTrigger`
    pub fn trigger_inventory(&self, p: &Arc<Player>) {
        let stacks: Vec<Stack> = p.inv.lock().slots.iter().flatten().copied().collect();
        let (full, empty) = {
            let inv = p.inv.lock();
            let main = &inv.slots[9..45];
            (main.iter().flatten().filter(|s| s.count >= s.max()).count(), main.iter().filter(|s| s.is_none()).count())
        };
        let occupied = 36 - empty;
        self.trigger(p, "inventory_changed", |c, ev| {
            if let Some(sl) = c.get("slots") {
                if !(in_range(sl.get("occupied"), occupied as f64) && in_range(sl.get("full"), full as f64) && in_range(sl.get("empty"), empty as f64)) {
                    return false;
                }
            }
            let Some(items) = c.get("items").and_then(|x| x.as_array()) else { return true };
            let mut unmatched: Vec<&Value> = items.iter().collect();
            for s in &stacks {
                unmatched.retain(|pr| !ev.item(Some(pr), Some(*s)));
                if unmatched.is_empty() {
                    break;
                }
            }
            unmatched.is_empty()
        });
        // recipe advancements also unlock from `recipe_unlocked`
    }

    /// `PlayerTrigger`s with no extra conditions (`location`, `tick`,
    /// `slept_in_bed`, `hero_of_the_village`, `started_riding` …).
    pub fn trigger_simple(&self, p: &Arc<Player>, trigger: &str) {
        self.trigger(p, trigger, |_, _| true);
    }

    pub fn trigger_recipe_unlocked(&self, p: &Arc<Player>) {
        let have = p.adv.lock().recipes.clone();
        self.trigger(p, "recipe_unlocked", |c, _| c.get("recipe").and_then(|x| x.as_str()).is_some_and(|r| have.contains(strip(r))));
    }

    /// `KilledTrigger` for a player killing something.
    pub fn trigger_player_killed(&self, p: &Arc<Player>, victim: &View, src: &DamageSource) {
        self.trigger(p, "player_killed_entity", |c, ev| ev.ctx_entity(c.get("entity"), victim) && killing_blow(ev, c.get("killing_blow"), src));
        // KilledByArrowTrigger-free part of spear / lightning is not tracked
    }

    /// `KilledTrigger` for something killing a player.
    pub fn trigger_killed_by(&self, p: &Arc<Player>, killer: Option<i32>, src: &DamageSource) {
        let k = self.view_opt(killer);
        self.trigger(p, "entity_killed_player", |c, ev| ev.ctx_entity_opt(c.get("entity"), k.as_ref()) && killing_blow(ev, c.get("killing_blow"), src));
    }

    pub fn trigger_item(&self, p: &Arc<Player>, trigger: &str, s: Stack) {
        self.trigger(p, trigger, |c, ev| ev.item(c.get("item"), Some(s)));
    }

    pub fn trigger_changed_dimension(&self, p: &Arc<Player>, from: Dim, to: Dim) {
        self.trigger(p, "changed_dimension", |c, _| {
            c.get("from").and_then(|x| x.as_str()).is_none_or(|d| d.trim_start_matches("minecraft:") == strip(from.key())) && c.get("to").and_then(|x| x.as_str()).is_none_or(|d| d.trim_start_matches("minecraft:") == strip(to.key()))
        });
    }

    /// `placed_block` / `item_used_on_block` / `allay_drop_item_on_block`
    pub fn trigger_block(&self, p: &Arc<Player>, trigger: &str, pos: [i32; 3], state: BlockState, tool: Option<Stack>) {
        self.trigger(p, trigger, |c, ev| ev.block_conditions(c.get("location"), pos, state, tool));
    }

    pub fn trigger_entity(&self, p: &Arc<Player>, trigger: &str, eid: i32) {
        let v = self.view_any(eid);
        self.trigger(p, trigger, |c, ev| ev.ctx_entity_opt(c.get("entity"), v.as_ref()));
    }

    pub fn trigger_villager_trade(&self, p: &Arc<Player>, villager: i32, item: Stack) {
        let v = self.view_any(villager);
        self.trigger(p, "villager_trade", |c, ev| ev.ctx_entity_opt(c.get("villager"), v.as_ref()) && ev.item(c.get("item"), Some(item)));
    }

    pub fn trigger_enchanted(&self, p: &Arc<Player>, item: Stack, levels: i32) {
        self.trigger(p, "enchanted_item", |c, ev| ev.item(c.get("item"), Some(item)) && in_range(c.get("levels"), levels as f64));
    }

    pub fn trigger_bred(&self, p: &Arc<Player>, parent: i32, partner: i32, child: Option<i32>) {
        let (a, b, ch) = (self.view_any(parent), self.view_any(partner), self.view_opt(child));
        self.trigger(p, "bred_animals", |c, ev| {
            let pair_ok = |x: Option<&View>, y: Option<&View>| ev.ctx_entity_opt(c.get("parent"), x) && ev.ctx_entity_opt(c.get("partner"), y);
            (pair_ok(a.as_ref(), b.as_ref()) || pair_ok(b.as_ref(), a.as_ref())) && ev.ctx_entity_opt(c.get("child"), ch.as_ref())
        });
    }

    pub fn trigger_hurt_entity(&self, p: &Arc<Player>, target: i32, src: &DamageSource, dealt: f32, taken: f32, blocked: bool) {
        let v = self.view_any(target);
        self.trigger(p, "player_hurt_entity", |c, ev| ev.ctx_entity_opt(c.get("entity"), v.as_ref()) && ev.damage(c.get("damage"), dealt, taken, blocked, src));
    }

    pub fn trigger_hurt_player(&self, p: &Arc<Player>, src: &DamageSource, dealt: f32, taken: f32, blocked: bool) {
        self.trigger(p, "entity_hurt_player", |c, ev| ev.damage(c.get("damage"), dealt, taken, blocked, src));
    }

    pub fn trigger_effects(&self, p: &Arc<Player>, source: Option<i32>) {
        let s = self.view_opt(source);
        let me = p.clone();
        self.trigger(p, "effects_changed", |c, ev| ev.effects(c.get("effects"), &me) && ev.ctx_entity_opt(c.get("source"), s.as_ref()));
    }

    pub fn trigger_beacon(&self, p: &Arc<Player>, level: i32) {
        self.trigger(p, "construct_beacon", |c, _| in_range(c.get("level"), level as f64));
    }

    pub fn trigger_container_loot(&self, p: &Arc<Player>, table: &str) {
        self.trigger(p, "player_generates_container_loot", |c, _| c.get("loot_table").and_then(|x| x.as_str()).is_some_and(|t| strip(t) == strip(table)));
    }

    pub fn trigger_interact_entity(&self, p: &Arc<Player>, item: Option<Stack>, eid: i32) {
        let v = self.view_any(eid);
        self.trigger(p, "player_interacted_with_entity", |c, ev| ev.ctx_entity_opt(c.get("entity"), v.as_ref()) && (c.get("item").is_none() || ev.item(c.get("item"), item)));
    }

    pub fn trigger_fishing(&self, p: &Arc<Player>, rod: Stack, hooked: Option<i32>, caught: &[Stack]) {
        let v = self.view_opt(hooked);
        let caught = caught.to_vec();
        self.trigger(p, "fishing_rod_hooked", |c, ev| ev.item(c.get("rod"), Some(rod)) && ev.ctx_entity_opt(c.get("entity"), v.as_ref()) && (c.get("item").is_none() || caught.iter().any(|s| ev.item(c.get("item"), Some(*s)))));
    }

    pub fn trigger_using_item(&self, p: &Arc<Player>, item: Stack) {
        self.trigger(p, "using_item", |c, ev| ev.item(c.get("item"), Some(item)));
    }

    pub fn trigger_enter_block(&self, p: &Arc<Player>, state: BlockState) {
        self.trigger(p, "enter_block", |c, _| {
            c.get("block").and_then(|x| x.as_str()).is_none_or(|b| strip(b) == state.name())
                && c.get("state").and_then(|x| x.as_object()).is_none_or(|m| m.iter().all(|(k, v)| state.prop(k).is_some_and(|cur| v.as_str() == Some(cur))))
        });
    }

    pub fn trigger_levitation(&self, p: &Arc<Player>, start: [f64; 3], ticks: i32) {
        let now = p.pose.lock().pos;
        self.trigger(p, "levitation", |c, _| {
            let d = [now[0] - start[0], now[1] - start[1], now[2] - start[2]];
            distance_ok(c.get("distance"), d) && in_range(c.get("duration"), ticks as f64)
        });
    }

    /// `fall_from_height` / `ride_entity_in_lava` / `nether_travel` /
    /// `fall_after_explosion`: a distance from a start position.
    pub fn trigger_distance(&self, p: &Arc<Player>, trigger: &str, start: [f64; 3]) {
        let now = p.pose.lock().pos;
        let dim = self.dim;
        self.trigger(p, trigger, |c, ev| {
            let d = [now[0] - start[0], now[1] - start[1], now[2] - start[2]];
            distance_ok(c.get("distance"), d) && c.get("start_position").is_none_or(|sp| ev.location(sp, dim, start))
        });
    }

    pub fn trigger_slide(&self, p: &Arc<Player>, state: BlockState) {
        self.trigger(p, "slide_down_block", |c, _| c.get("block").and_then(|x| x.as_str()).is_none_or(|b| strip(b) == state.name()));
    }

    pub fn trigger_target_hit(&self, p: &Arc<Player>, signal: i32, projectile: Option<i32>) {
        let v = self.view_opt(projectile);
        self.trigger(p, "target_hit", |c, ev| in_range(c.get("signal_strength"), signal as f64) && ev.ctx_entity_opt(c.get("projectile"), v.as_ref()));
    }

    pub fn trigger_bee_nest(&self, p: &Arc<Player>, state: BlockState, tool: Option<Stack>, bees: i32) {
        self.trigger(p, "bee_nest_destroyed", |c, ev| c.get("block").and_then(|x| x.as_str()).is_none_or(|b| strip(b) == state.name()) && (c.get("item").is_none() || ev.item(c.get("item"), tool)) && in_range(c.get("num_bees_inside"), bees as f64));
    }

    pub fn trigger_durability(&self, p: &Arc<Player>, item: Stack, delta: i32) {
        self.trigger(p, "item_durability_changed", |c, ev| ev.item(c.get("item"), Some(item)) && in_range(c.get("durability"), (item.max_damage() - item.damage_value()) as f64) && in_range(c.get("delta"), delta as f64));
    }

    pub fn trigger_sheared(&self, p: &Arc<Player>, item: Stack, eid: i32) {
        let v = self.view_any(eid);
        self.trigger(p, "player_sheared_equipment", |c, ev| ev.item(c.get("item"), Some(item)) && ev.ctx_entity_opt(c.get("entity"), v.as_ref()));
    }

    pub fn trigger_lightning(&self, p: &Arc<Player>, bolt: i32) {
        let v = self.view_any(bolt);
        self.trigger(p, "lightning_strike", |c, ev| ev.ctx_entity_opt(c.get("lightning"), v.as_ref()) && c.get("bystander").is_none());
    }

    pub fn trigger_spear(&self, p: &Arc<Player>, count: i32) {
        self.trigger(p, "spear_mobs", |c, _| in_range(c.get("count"), count as f64));
    }

    pub fn trigger_crossbow(&self, p: &Arc<Player>, item: Stack) {
        self.trigger_item(p, "shot_crossbow", item);
    }

    pub fn trigger_killed_by_arrow(&self, p: &Arc<Player>, victims: &[i32], weapon: Option<Stack>) {
        let vs: Vec<View> = victims.iter().filter_map(|e| self.view_any(*e)).collect();
        let types = {
            let mut t: Vec<&str> = vs.iter().map(|v| v.ty.as_str()).collect();
            t.sort_unstable();
            t.dedup();
            t.len()
        };
        self.trigger(p, "killed_by_arrow", |c, ev| {
            if !in_range(c.get("unique_entity_types"), types as f64) {
                return false;
            }
            if c.get("fired_from_weapon").is_some() && !ev.item(c.get("fired_from_weapon"), weapon) {
                return false;
            }
            let Some(preds) = c.get("victims").and_then(|x| x.as_array()) else { return true };
            let mut left: Vec<&View> = vs.iter().collect();
            preds.iter().all(|pr| match left.iter().position(|v| ev.ctx_entity(Some(pr), v)) {
                Some(i) => {
                    left.remove(i);
                    true
                }
                None => false,
            })
        });
    }

    pub fn trigger_channeled(&self, p: &Arc<Player>, victims: &[i32]) {
        let vs: Vec<View> = victims.iter().filter_map(|e| self.view_any(*e)).collect();
        self.trigger(p, "channeled_lightning", |c, ev| c.get("victims").and_then(|x| x.as_array()).is_none_or(|preds| preds.iter().all(|pr| vs.iter().any(|v| ev.ctx_entity(Some(pr), v)))));
    }

    pub fn trigger_thrown_pickup(&self, p: &Arc<Player>, item: Stack, by: i32) {
        let v = self.view_any(by);
        self.trigger(p, "thrown_item_picked_up_by_entity", |c, ev| ev.item(c.get("item"), Some(item)) && ev.ctx_entity_opt(c.get("entity"), v.as_ref()));
    }
}

fn distance_ok(p: Option<&Value>, d: [f64; 3]) -> bool {
    let Some(p) = p else { return true };
    in_range(p.get("x"), d[0].abs()) && in_range(p.get("y"), d[1].abs()) && in_range(p.get("z"), d[2].abs()) && in_range(p.get("horizontal"), (d[0] * d[0] + d[2] * d[2]).sqrt()) && in_range(p.get("absolute"), (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt())
}

/// `DamageSourcePredicate` of a killing blow.
fn killing_blow(ev: &Eval, p: Option<&Value>, src: &DamageSource) -> bool {
    let Some(p) = p else { return true };
    if let Some(tags) = p.get("tags").and_then(|x| x.as_array()) {
        for t in tags {
            let id = t.get("id").and_then(|x| x.as_str()).map(strip).unwrap_or("");
            if src.is(id) != t.get("expected").and_then(|x| x.as_bool()).unwrap_or(true) {
                return false;
            }
        }
    }
    if let Some(d) = p.get("direct_entity") {
        let v = src.direct.as_ref().and_then(|c| ev.g.view_any(c.eid));
        if !ev.ctx_entity_opt(Some(d), v.as_ref()) {
            return false;
        }
    }
    if let Some(d) = p.get("source_entity") {
        let v = src.causing.as_ref().and_then(|c| ev.g.view_any(c.eid));
        if !ev.ctx_entity_opt(Some(d), v.as_ref()) {
            return false;
        }
    }
    true
}
