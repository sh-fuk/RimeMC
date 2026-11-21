//! `Consumable` / `FoodProperties` / `PotionContents` / consume effects:
//! what happens when a player finishes eating or drinking an item.

use std::sync::{Arc, OnceLock};

use rand::Rng;
use serde_json::Value;

use super::combat::effects::{instance_from_json, Effect, Instance};
use super::combat::living::{Body, Equipment, Living};
use super::{Game, Player, Request};
use crate::data::Stack;
use crate::registry::vstate;

/// `Potions`: potion name -> (effect, duration, amplifier)
pub fn potion_effects(name: &str) -> &'static [(Effect, i32, i32)] {
    static T: OnceLock<std::collections::HashMap<String, Vec<(Effect, i32, i32)>>> = OnceLock::new();
    let t = T.get_or_init(|| {
        include_str!("../../assets/26.1/potions.tsv")
            .lines()
            .map(|l| {
                let mut it = l.split('\t');
                let name = it.next().unwrap_or("").to_string();
                let effects = it
                    .filter_map(|e| {
                        let mut p = e.split(',');
                        Some((Effect::from_name(p.next()?)?, p.next()?.parse().ok()?, p.next()?.parse().ok()?))
                    })
                    .collect();
                (name, effects)
            })
            .collect()
    });
    t.get(name.trim_start_matches("minecraft:")).map(|v| v.as_slice()).unwrap_or(&[])
}

/// The `consumable` component of a stack, if any.
pub fn consumable(stack: &Stack) -> Option<&'static Value> {
    stack.defaults().raw.get("consumable")
}

/// `Consumable.consumeTicks` (`consume_seconds`, default 1.6).
pub fn consume_ticks(c: &Value) -> u64 {
    (c.get("consume_seconds").and_then(|s| s.as_f64()).unwrap_or(1.6) * 20.0) as u64
}

/// `FoodProperties`: (nutrition, saturation, can_always_eat)
pub fn food(stack: &Stack) -> Option<(i32, f32, bool)> {
    let f = stack.defaults().raw.get("food")?;
    Some((
        f.get("nutrition").and_then(|n| n.as_i64()).unwrap_or(0) as i32,
        f.get("saturation").and_then(|n| n.as_f64()).unwrap_or(0.0) as f32,
        f.get("can_always_eat").and_then(|n| n.as_bool()).unwrap_or(false),
    ))
}

/// `use_remainder` item name.
pub fn remainder(stack: &Stack) -> Option<&'static str> {
    stack.defaults().raw.get("use_remainder")?.get("id")?.as_str()
}

/// `use_cooldown` in ticks.
pub fn use_cooldown(stack: &Stack) -> Option<i32> {
    let c = stack.defaults().raw.get("use_cooldown")?;
    Some((c.get("seconds")?.as_f64()? * 20.0) as i32)
}

impl Game {
    /// `Consumable.onConsume` on a player (food, sound, consume effects and
    /// potion contents). The caller removes the item and hands out the remainder.
    pub fn consume(&self, p: &Arc<Player>, stack: Stack) {
        let Some(c) = consumable(&stack) else { return };
        let pos = p.pose.lock().pos;
        let mut rng = rand::thread_rng();
        // sound
        let sound = match c.get("sound") {
            Some(Value::String(s)) => s.clone(),
            Some(o) => o.get("sound_id").and_then(|s| s.as_str()).unwrap_or("minecraft:entity.generic.eat").to_string(),
            None => "minecraft:entity.generic.eat".into(),
        };
        // Consumable.emitParticlesAndSounds
        let drink = c.get("animation").and_then(|a| a.as_str()) == Some("drink");
        let (vol, pitch) = if drink {
            (0.5, 0.9 + rng.r#gen::<f32>() * 0.1)
        } else {
            (if rng.r#gen::<bool>() { 0.5 } else { 1.0 }, 1.0 + 0.2 * (rng.r#gen::<f32>() - rng.r#gen::<f32>()))
        };
        self.play_sound(&sound, 6, pos, vol, pitch);
        let food = food(&stack);
        if food.is_some() {
            // FoodProperties.onConsume
            self.play_sound(&sound, 6, pos, 1.0, 1.0 + 0.4 * (rng.r#gen::<f32>() - rng.r#gen::<f32>()));
            self.play_sound("minecraft:entity.player.burp", 7, pos, 0.5, 0.9 + rng.r#gen::<f32>() * 0.1);
        }
        if let Some((n, s, _)) = food {
            let mut v = p.vit.lock();
            v.food = (v.food + n).clamp(0, 20);
            v.saturation = (v.saturation + s).clamp(0.0, v.food as f32);
            v.dirty = true;
        }
        // consume effects
        if let Some(list) = c.get("on_consume_effects").and_then(|l| l.as_array()) {
            for e in list {
                self.consume_effect(p, e, &mut rng);
            }
        }
        // PotionContents.onConsume
        let patch = stack.patch();
        if let Some(pot) = &patch.potion {
            let scale = stack.defaults().raw.get("potion_duration_scale").and_then(|s| s.as_f64()).unwrap_or(1.0) as f32;
            let effects: Vec<_> = potion_effects(pot).to_vec();
            let eid = p.eid;
            self.with_player(p, |l, eq, body| {
                for (e, dur, amp) in effects {
                    apply_potion_effect(l, eq, body, e, dur, amp, scale, eid, &mut rand::thread_rng());
                }
            });
        }
    }

    fn consume_effect(&self, p: &Arc<Player>, e: &Value, rng: &mut impl Rng) {
        match e.get("type").and_then(|t| t.as_str()).unwrap_or("").trim_start_matches("minecraft:") {
            "apply_effects" => {
                let prob = e.get("probability").and_then(|x| x.as_f64()).unwrap_or(1.0) as f32;
                if rng.r#gen::<f32>() >= prob {
                    return;
                }
                let list: Vec<Instance> = e.get("effects").and_then(|l| l.as_array()).map(|l| l.iter().filter_map(instance_from_json).collect()).unwrap_or_default();
                self.with_player(p, |l, _, body| {
                    for i in list {
                        l.add_effect(&body.info, i);
                    }
                });
            }
            "remove_effects" => {
                let names: Vec<Effect> = match e.get("effects") {
                    Some(Value::String(s)) if s.starts_with('#') => Vec::new(),
                    Some(Value::String(s)) => Effect::from_name(s).into_iter().collect(),
                    Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).filter_map(Effect::from_name).collect(),
                    _ => Vec::new(),
                };
                self.with_player(p, |l, _, _| {
                    for n in names {
                        l.remove_effect(n);
                    }
                });
            }
            "clear_all_effects" => {
                self.with_player(p, |l, _, _| l.remove_all_effects());
            }
            "play_sound" => {
                let s = match e.get("sound") {
                    Some(Value::String(s)) => s.clone(),
                    Some(o) => o.get("sound_id").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                    None => return,
                };
                self.play_sound(&s, 7, p.pose.lock().pos, 1.0, 1.0);
            }
            "teleport_randomly" => {
                let d = e.get("diameter").and_then(|x| x.as_f64()).unwrap_or(16.0);
                self.teleport_randomly(p, d, rng);
            }
            _ => {}
        }
    }

    /// `TeleportRandomlyConsumeEffect` + `LivingEntity.randomTeleport`.
    fn teleport_randomly(&self, p: &Arc<Player>, diameter: f64, rng: &mut impl Rng) {
        let pos = p.pose.lock().pos;
        use crate::world::chunk::{HEIGHT, MIN_Y};
        for _ in 0..16 {
            let x = pos[0] + (rng.r#gen::<f64>() - 0.5) * diameter;
            let y = (pos[1] + (rng.gen_range(0..diameter as i32) - (diameter as i32) / 2) as f64).clamp(MIN_Y as f64, (MIN_Y + HEIGHT - 1) as f64);
            let z = pos[2] + (rng.r#gen::<f64>() - 0.5) * diameter;
            if let Some(dest) = self.random_teleport_target([x, y, z]) {
                let pose = *p.pose.lock();
                p.requests.lock().push(Request::Teleport(dest, pose.yaw, pose.pitch));
                self.play_sound("minecraft:item.chorus_fruit.teleport", 7, pos, 1.0, 1.0);
                self.play_sound("minecraft:item.chorus_fruit.teleport", 7, dest, 1.0, 1.0);
                p.vit.lock().fall_distance = 0.0;
                return;
            }
        }
    }

    /// `randomTeleport`: drop to the first motion-blocking block below and
    /// accept the spot when the player's box there is free of blocks and liquid.
    fn random_teleport_target(&self, p: [f64; 3]) -> Option<[f64; 3]> {
        let (bx, bz) = (p[0].floor() as i32, p[2].floor() as i32);
        let mut by = p[1].floor() as i32;
        let w = &self.world;
        let block = |y: i32| w.get_block(bx, y, bz);
        let mut found = false;
        while by > crate::world::chunk::MIN_Y {
            let below = block(by - 1)?;
            if vstate::blocks_motion(below) {
                found = true;
                break;
            }
            by -= 1;
        }
        if !found {
            return None;
        }
        let dest = [p[0], by as f64, p[2]];
        // 0.6 x 1.8 box: check every block it overlaps
        let (x0, x1) = ((dest[0] - 0.3).floor() as i32, (dest[0] + 0.3).floor() as i32);
        let (z0, z1) = ((dest[2] - 0.3).floor() as i32, (dest[2] + 0.3).floor() as i32);
        for x in x0..=x1 {
            for z in z0..=z1 {
                for y in by..=by + 1 {
                    let b = w.get_block(x, y, z)?;
                    if vstate::blocks_motion(b) || vstate::has_fluid(b) {
                        return None;
                    }
                }
            }
        }
        Some(dest)
    }
}

/// `PotionContents.applyToLivingEntity` for one effect.
#[allow(clippy::too_many_arguments)]
pub fn apply_potion_effect(l: &mut Living, eq: &mut Equipment, body: &mut Body, e: Effect, dur: i32, amp: i32, scale: f32, _by: i32, rng: &mut impl Rng) {
    if e.is_instant() {
        l.apply_instant(eq, body, e, amp, 1.0, None, rng);
    } else {
        // MobEffectInstance.withScaledDuration
        let d = if scale == 1.0 { dur } else { ((dur as f32 * scale).floor() as i32).max(1) };
        if d > 0 {
            l.add_effect(&body.info, Instance::new(e, d, amp));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn potions() {
        assert_eq!(potion_effects("minecraft:strong_turtle_master"), &[(Effect::Slowness, 400, 5), (Effect::Resistance, 400, 3)]);
        assert_eq!(potion_effects("healing"), &[(Effect::InstantHealth, 1, 0)]);
        assert!(potion_effects("water").is_empty());
    }
}
