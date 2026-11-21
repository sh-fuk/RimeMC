//! Player hunger, air, fire and fall damage, following vanilla `FoodData`,
//! `LivingEntity.baseTick` (drowning, burning), `causeFallDamage`, lava / fire
//! and the void. Health itself lives in [`Living`] (exact damage pipeline).

use crate::game::combat::damage::DamageSource;
use crate::game::combat::living::Living;

/// Health and hunger state of one player.
#[derive(Clone, Debug)]
pub struct Vitals {
    pub living: Living,
    pub food: i32,
    pub saturation: f32,
    pub exhaustion: f32,
    food_timer: i32,
    pub air: i32,
    /// remaining burning ticks (`remainingFireTicks`)
    pub fire: i32,
    pub fall_distance: f64,
    /// the client needs a `set_health`
    pub dirty: bool,
    /// the air supply / burning flag changed (entity data)
    pub data_dirty: bool,
    /// (item, start tick, duration) while eating
    pub eating: Option<(i32, u64, u64)>,
    pub xp_level: i32,
    pub xp_progress: f32,
    pub xp_total: i32,
    pub xp_dirty: bool,
}

pub const MAX_AIR: i32 = 300;

impl Default for Vitals {
    fn default() -> Self {
        Vitals {
            living: Living::player(),
            food: 20,
            saturation: 5.0,
            exhaustion: 0.0,
            food_timer: 0,
            air: MAX_AIR,
            fire: 0,
            fall_distance: 0.0,
            xp_level: 0,
            xp_progress: 0.0,
            xp_total: 0,
            xp_dirty: true,
            dirty: true,
            data_dirty: true,
            eating: None,
        }
    }
}

/// What the world around the player looks like this tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct Surroundings {
    pub eyes_in_water: bool,
    pub in_water: bool,
    pub in_lava: bool,
    pub in_fire: bool,
    pub bubble_column: bool,
    /// standing in rain / bubble column etc. that puts fire out
    pub y: f64,
    pub min_y: i32,
    /// game mode (0 survival, 1 creative, 2 adventure, 3 spectator)
    pub gamemode: i32,
    /// difficulty (0 peaceful .. 3 hard)
    pub difficulty: i32,
}

impl Vitals {
    pub fn health(&self) -> f32 {
        self.living.health
    }
    /// dead or dying (death screen)
    pub fn dead(&self) -> bool {
        self.living.dead || self.living.health <= 0.0
    }

    pub fn heal(&mut self, amount: f32) {
        self.living.heal(amount);
    }

    pub fn add_exhaustion(&mut self, v: f32) {
        self.exhaustion = (self.exhaustion + v).min(40.0);
    }

    /// `FoodData.eat(nutrition, saturationModifier)`
    pub fn eat(&mut self, nutrition: i32, modifier: f32) {
        self.food = (self.food + nutrition).clamp(0, 20);
        self.saturation = (self.saturation + nutrition as f32 * modifier * 2.0).clamp(0.0, self.food as f32);
        self.dirty = true;
    }

    /// One server tick; returns the damage this tick deals (applied by the
    /// caller through the exact damage pipeline).
    pub fn tick(&mut self, s: &Surroundings) -> Vec<(DamageSource, f32)> {
        let mut out = Vec::new();
        if self.dead() {
            return out;
        }
        let survival = s.gamemode == 0 || s.gamemode == 2;
        // ---- FoodData.tick
        if survival {
            if self.exhaustion > 4.0 {
                self.exhaustion -= 4.0;
                if self.saturation > 0.0 {
                    self.saturation = (self.saturation - 1.0).max(0.0);
                } else if s.difficulty != 0 {
                    self.food = (self.food - 1).max(0);
                }
                self.dirty = true;
            }
            let max = self.living.max_health();
            let hurt = self.living.health > 0.0 && self.living.health < max;
            if self.saturation > 0.0 && hurt && self.food >= 20 {
                self.food_timer += 1;
                if self.food_timer >= 10 {
                    let f = self.saturation.min(6.0);
                    self.heal(f / 6.0);
                    self.add_exhaustion(f);
                    self.food_timer = 0;
                }
            } else if self.food >= 18 && hurt {
                self.food_timer += 1;
                if self.food_timer >= 80 {
                    self.heal(1.0);
                    self.add_exhaustion(6.0);
                    self.food_timer = 0;
                }
            } else if self.food <= 0 {
                self.food_timer += 1;
                if self.food_timer >= 80 {
                    let h = self.living.health;
                    if h > 10.0 || s.difficulty == 3 || (h > 1.0 && s.difficulty == 2) {
                        out.push((DamageSource::new("starve"), 1.0));
                    }
                    self.food_timer = 0;
                }
            } else {
                self.food_timer = 0;
            }
        }
        // ---- air supply (LivingEntity.baseTick)
        let old_air = self.air;
        let breathing = self.living.has_effect(crate::game::combat::effects::Effect::WaterBreathing) || self.living.has_effect(crate::game::combat::effects::Effect::ConduitPower);
        if s.eyes_in_water && !s.bubble_column {
            if !breathing && survival {
                // decreaseAirSupply (respiration: oxygen_bonus)
                let bonus = self.living.attrs.value(crate::game::combat::attr::Attr::OxygenBonus);
                if !(bonus > 0.0 && rand::random::<f64>() >= 1.0 / (bonus + 1.0)) {
                    self.air -= 1;
                }
                if self.air <= -20 {
                    self.air = 0;
                    out.push((DamageSource::new("drown"), 2.0));
                }
            } else if self.air < MAX_AIR && breathing {
                self.air = (self.air + 4).min(MAX_AIR);
            }
        } else if self.air < MAX_AIR {
            self.air = (self.air + 4).min(MAX_AIR);
        }
        if self.air != old_air {
            self.data_dirty = true;
        }
        // ---- fire
        let was_burning = self.fire > 0;
        if s.in_water {
            self.fire = 0;
        } else if s.in_lava {
            // Entity.lavaHurt: on fire for 15 s (in ticks: 300), hurt 4
            if survival {
                self.fire = self.fire.max(300);
            }
            out.push((DamageSource::new("lava"), 4.0));
        } else if s.in_fire {
            if self.fire < 160 && survival {
                self.fire = 160;
            }
            out.push((DamageSource::new("in_fire"), 1.0));
        }
        if self.fire > 0 && !s.in_lava {
            if self.fire % 20 == 0 {
                out.push((DamageSource::new("on_fire"), 1.0));
            }
            self.fire -= 1;
        }
        if !survival {
            self.fire = 0;
        }
        if (self.fire > 0) != was_burning {
            self.data_dirty = true;
        }
        // ---- void
        if s.y < (s.min_y - 64) as f64 {
            out.push((DamageSource::new("out_of_world"), 4.0));
        }
        out
    }

    /// `Entity.checkFallDamage` with the client-reported movement; returns
    /// the fall damage on landing.
    pub fn on_move(&mut self, dy: f64, on_ground: bool, reset: bool, landing_mult: f32, gm: i32) -> Option<f32> {
        if reset || gm == 1 || gm == 3 {
            self.fall_distance = 0.0;
            return None;
        }
        if on_ground {
            if self.fall_distance > 0.0 {
                let d = self.fall_distance;
                self.fall_distance = 0.0;
                // LivingEntity.calculateFallDamage
                let safe = self.living.attrs.value(crate::game::combat::attr::Attr::SafeFallDistance);
                let mult = self.living.attrs.value(crate::game::combat::attr::Attr::FallDamageMultiplier);
                let power = d + 1.0e-6 - safe;
                let dmg = (power * landing_mult as f64 * mult).floor() as i32;
                if dmg > 0 {
                    return Some(dmg as f32);
                }
            }
        } else if dy < 0.0 {
            self.fall_distance -= dy;
        }
        None
    }

    /// `Player.getXpNeededForNextLevel`
    pub fn xp_needed(&self) -> i32 {
        if self.xp_level >= 30 {
            112 + (self.xp_level - 30) * 9
        } else if self.xp_level >= 15 {
            37 + (self.xp_level - 15) * 5
        } else {
            7 + self.xp_level * 2
        }
    }

    /// `Player.giveExperiencePoints`
    pub fn give_xp(&mut self, n: i32) {
        self.xp_progress += n as f32 / self.xp_needed() as f32;
        self.xp_total = (self.xp_total as i64 + n as i64).clamp(0, i32::MAX as i64) as i32;
        while self.xp_progress < 0.0 {
            let p = self.xp_progress * self.xp_needed() as f32;
            if self.xp_level > 0 {
                self.xp_level -= 1;
                self.xp_progress = 1.0 + p / self.xp_needed() as f32;
            } else {
                self.xp_level = 0;
                self.xp_progress = 0.0;
            }
        }
        while self.xp_progress >= 1.0 {
            self.xp_progress = (self.xp_progress - 1.0) * self.xp_needed() as f32;
            self.xp_level += 1;
            self.xp_progress /= self.xp_needed() as f32;
        }
        self.xp_dirty = true;
    }

    /// Back to full health at respawn (attributes reset, effects cleared).
    pub fn respawn(&mut self) {
        *self = Vitals::default();
    }
}

/// `Foods`: (nutrition, saturation modifier, eat ticks, can always eat)
pub fn food(item: &str) -> Option<(i32, f32, u64, bool)> {
    let (n, s) = match item {
        "apple" => (4, 0.3),
        "baked_potato" => (5, 0.6),
        "beef" => (3, 0.3),
        "beetroot" => (1, 0.6),
        "beetroot_soup" => (6, 0.6),
        "bread" => (5, 0.6),
        "carrot" => (3, 0.6),
        "chicken" => (2, 0.3),
        "chorus_fruit" => (4, 0.3),
        "cod" => (2, 0.1),
        "cooked_beef" => (8, 0.8),
        "cooked_chicken" => (6, 0.6),
        "cooked_cod" => (5, 0.6),
        "cooked_mutton" => (6, 0.8),
        "cooked_porkchop" => (8, 0.8),
        "cooked_rabbit" => (5, 0.6),
        "cooked_salmon" => (6, 0.8),
        "cookie" => (2, 0.1),
        "dried_kelp" => (1, 0.3),
        "enchanted_golden_apple" => (4, 1.2),
        "golden_apple" => (4, 1.2),
        "golden_carrot" => (6, 1.2),
        "honey_bottle" => (6, 0.1),
        "melon_slice" => (2, 0.3),
        "mushroom_stew" => (6, 0.6),
        "mutton" => (2, 0.3),
        "poisonous_potato" => (2, 0.3),
        "porkchop" => (3, 0.3),
        "potato" => (1, 0.3),
        "pufferfish" => (1, 0.1),
        "pumpkin_pie" => (8, 0.3),
        "rabbit" => (3, 0.3),
        "rabbit_stew" => (10, 0.6),
        "rotten_flesh" => (4, 0.1),
        "salmon" => (2, 0.1),
        "spider_eye" => (2, 0.8),
        "suspicious_stew" => (6, 0.6),
        "sweet_berries" => (2, 0.1),
        "glow_berries" => (2, 0.1),
        "tropical_fish" => (1, 0.1),
        _ => return None,
    };
    let ticks = match item {
        "dried_kelp" => 16,
        "honey_bottle" => 40,
        _ => 32,
    };
    let always = matches!(item, "golden_apple" | "enchanted_golden_apple" | "chorus_fruit" | "honey_bottle" | "suspicious_stew");
    Some((n, s, ticks, always))
}

/// Item left behind after eating (bowls, bottles).
pub fn food_remainder(item: &str) -> Option<&'static str> {
    match item {
        "mushroom_stew" | "rabbit_stew" | "beetroot_soup" | "suspicious_stew" => Some("bowl"),
        "honey_bottle" => Some("glass_bottle"),
        _ => None,
    }
}

/// Fall damage multiplier of the block landed on (`fallOn`); None = no damage.
pub fn landing_multiplier(block: &str, sneaking: bool) -> Option<f32> {
    match block {
        "slime_block" if !sneaking => None,
        "hay_block" | "honey_block" => Some(0.2),
        b if b.ends_with("_bed") => Some(0.5),
        "pointed_dripstone" => Some(2.0),
        "powder_snow" => None,
        _ => Some(1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(gm: i32) -> Surroundings {
        Surroundings { gamemode: gm, difficulty: 2, y: 64.0, min_y: -64, ..Default::default() }
    }

    #[test]
    fn fall_damage_like_vanilla() {
        let mut v = Vitals::default();
        // fall 10 blocks: ceil(10 - 3) = 7
        for _ in 0..10 {
            assert_eq!(v.on_move(-1.0, false, false, 1.0, 0), None);
        }
        assert_eq!(v.on_move(0.0, true, false, 1.0, 0), Some(7.0));
        // 3 blocks is safe
        for _ in 0..3 {
            v.on_move(-1.0, false, false, 1.0, 0);
        }
        assert_eq!(v.on_move(0.0, true, false, 1.0, 0), None);
        // creative never takes fall damage
        for _ in 0..30 {
            v.on_move(-1.0, false, false, 1.0, 1);
        }
        assert_eq!(v.on_move(0.0, true, false, 1.0, 1), None);
    }

    #[test]
    fn natural_regeneration_uses_saturation() {
        let mut v = Vitals::default();
        v.living.health = 10.0;
        for _ in 0..10 {
            v.tick(&s(0));
        }
        // saturation 5 → heal 5/6, exhaustion +5 → 1 saturation spent on the next tick
        assert!((v.living.health - (10.0 + 5.0 / 6.0)).abs() < 1e-4);
        v.tick(&s(0));
        assert_eq!(v.saturation, 4.0);
    }

    #[test]
    fn drowning_after_air_runs_out() {
        let mut v = Vitals::default();
        let mut w = s(0);
        w.eyes_in_water = true;
        let mut hits = 0;
        for _ in 0..(300 + 20) {
            hits += v.tick(&w).iter().filter(|(c, _)| c.ty == "drown").count();
        }
        assert_eq!(hits, 1);
    }

    #[test]
    fn eating_caps_saturation_at_food() {
        let mut v = Vitals::default();
        v.food = 10;
        v.saturation = 0.0;
        v.eat(8, 0.8);
        assert_eq!(v.food, 18);
        assert!((v.saturation - 12.8).abs() < 1e-4);
    }
}
