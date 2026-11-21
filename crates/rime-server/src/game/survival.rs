//! Player health, hunger, air, fire and fall damage, following vanilla
//! `FoodData`, `LivingEntity.hurt` (invulnerability frames), `causeFallDamage`,
//! drowning, lava / fire and the void.

/// Why a player was hurt (vanilla damage type name and death message).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    Fall,
    Drown,
    Lava,
    InFire,
    OnFire,
    Starve,
    Void,
    Generic,
    /// melee hit by a mob (display name)
    Mob(&'static str),
    /// shot by a mob
    Arrow(&'static str),
    /// blown up by a mob
    Explosion(&'static str),
    /// hit by another player (their entity id)
    Player(i32),
}

impl Cause {
    /// `minecraft:damage_type` entry
    pub fn damage_type(self) -> &'static str {
        match self {
            Cause::Fall => "minecraft:fall",
            Cause::Drown => "minecraft:drown",
            Cause::Lava => "minecraft:lava",
            Cause::InFire => "minecraft:in_fire",
            Cause::OnFire => "minecraft:on_fire",
            Cause::Starve => "minecraft:starve",
            Cause::Void => "minecraft:out_of_world",
            Cause::Generic => "minecraft:generic",
            Cause::Mob(_) => "minecraft:mob_attack",
            Cause::Arrow(_) => "minecraft:arrow",
            Cause::Explosion(_) => "minecraft:explosion",
            Cause::Player(_) => "minecraft:player_attack",
        }
    }

    pub fn death_message(self, name: &str) -> String {
        match self {
            Cause::Fall => format!("{name} fell from a high place"),
            Cause::Drown => format!("{name} drowned"),
            Cause::Lava => format!("{name} tried to swim in lava"),
            Cause::InFire => format!("{name} went up in flames"),
            Cause::OnFire => format!("{name} burned to death"),
            Cause::Starve => format!("{name} starved to death"),
            Cause::Void => format!("{name} fell out of the world"),
            Cause::Generic => format!("{name} died"),
            Cause::Mob(m) => format!("{name} was slain by {m}"),
            Cause::Arrow(m) => format!("{name} was shot by {m}"),
            Cause::Explosion(m) => format!("{name} was blown up by {m}"),
            Cause::Player(_) => format!("{name} was slain"),
        }
    }

    /// reduced by armour
    pub fn armor_applies(self) -> bool {
        matches!(self, Cause::Mob(_) | Cause::Arrow(_) | Cause::Explosion(_) | Cause::Player(_) | Cause::Generic)
    }

    /// `#bypasses_invulnerability`
    fn bypasses_invulnerability(self) -> bool {
        self == Cause::Void
    }

    /// `DamageSource.getFoodExhaustion`
    fn exhaustion(self) -> f32 {
        match self {
            Cause::Starve | Cause::Drown | Cause::Void | Cause::Fall => 0.0,
            _ => 0.1,
        }
    }
}

/// Health and hunger state of one player.
#[derive(Clone, Debug)]
pub struct Vitals {
    pub health: f32,
    pub food: i32,
    pub saturation: f32,
    pub exhaustion: f32,
    food_timer: i32,
    pub air: i32,
    /// remaining burning ticks (`remainingFireTicks`)
    pub fire: i32,
    pub fall_distance: f64,
    invulnerable: i32,
    last_hurt: f32,
    pub dead: bool,
    /// the client needs a `set_health`
    pub dirty: bool,
    /// the air supply / burning flag changed (entity data)
    pub data_dirty: bool,
    /// (item, start tick, duration) while eating
    pub eating: Option<(i32, u64, u64)>,
}

pub const MAX_AIR: i32 = 300;

impl Default for Vitals {
    fn default() -> Self {
        Vitals {
            health: 20.0,
            food: 20,
            saturation: 5.0,
            exhaustion: 0.0,
            food_timer: 0,
            air: MAX_AIR,
            fire: 0,
            fall_distance: 0.0,
            invulnerable: 0,
            last_hurt: 0.0,
            dead: false,
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
    /// standing in rain / bubble column etc. that puts fire out
    pub y: f64,
    pub min_y: i32,
    /// game mode (0 survival, 1 creative, 2 adventure, 3 spectator)
    pub gamemode: i32,
    /// difficulty (0 peaceful .. 3 hard)
    pub difficulty: i32,
}

impl Vitals {
    fn vulnerable(&self, gm: i32, cause: Cause) -> bool {
        !self.dead && (gm == 0 || gm == 2 || (cause.bypasses_invulnerability() && gm == 1))
    }

    /// `LivingEntity.hurt` for a player; returns the damage actually dealt.
    pub fn hurt(&mut self, cause: Cause, amount: f32, gm: i32, difficulty: i32) -> Option<f32> {
        if !self.vulnerable(gm, cause) || amount <= 0.0 {
            return None;
        }
        let _ = difficulty; // scaling only applies to mob attacks
        let dealt = if self.invulnerable as f32 > 10.0 {
            if amount <= self.last_hurt {
                return None;
            }
            let d = amount - self.last_hurt;
            self.last_hurt = amount;
            d
        } else {
            self.last_hurt = amount;
            self.invulnerable = 20;
            amount
        };
        self.add_exhaustion(cause.exhaustion());
        self.health = (self.health - dealt).max(0.0);
        self.dirty = true;
        if self.health <= 0.0 {
            self.dead = true;
            self.eating = None;
        }
        Some(dealt)
    }

    pub fn heal(&mut self, amount: f32) {
        if !self.dead && self.health > 0.0 {
            let h = (self.health + amount).min(20.0);
            if h != self.health {
                self.health = h;
                self.dirty = true;
            }
        }
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

    /// One server tick; returns the damage events of this tick.
    pub fn tick(&mut self, s: &Surroundings) -> Vec<(Cause, f32)> {
        let mut out = Vec::new();
        if self.dead {
            return out;
        }
        if self.invulnerable > 0 {
            self.invulnerable -= 1;
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
            let hurt = self.health > 0.0 && self.health < 20.0;
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
                    if self.health > 10.0 || s.difficulty == 3 || (self.health > 1.0 && s.difficulty == 2) {
                        out.push((Cause::Starve, 1.0));
                    }
                    self.food_timer = 0;
                }
            } else {
                self.food_timer = 0;
            }
        }
        // ---- air supply (LivingEntity.baseTick)
        let old_air = self.air;
        if s.eyes_in_water && survival {
            self.air -= 1;
            if self.air <= -20 {
                self.air = 0;
                out.push((Cause::Drown, 2.0));
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
            out.push((Cause::Lava, 4.0));
        } else if s.in_fire {
            if self.fire < 160 && survival {
                self.fire = 160;
            }
            out.push((Cause::InFire, 1.0));
        }
        if self.fire > 0 && !s.in_lava {
            if self.fire % 20 == 0 {
                out.push((Cause::OnFire, 1.0));
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
            out.push((Cause::Void, 4.0));
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
                // LivingEntity.calculateFallDamage: safe fall distance 3
                let dmg = ((d - 3.0) as f32 * landing_mult).ceil();
                if dmg > 0.0 {
                    return Some(dmg);
                }
            }
        } else if dy < 0.0 {
            self.fall_distance -= dy;
        }
        None
    }

    /// Back to full health at respawn.
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
    fn invulnerability_frames() {
        let mut v = Vitals::default();
        assert_eq!(v.hurt(Cause::Generic, 4.0, 0, 2), Some(4.0));
        // a weaker hit during the frames does nothing, a stronger one the difference
        assert_eq!(v.hurt(Cause::Generic, 3.0, 0, 2), None);
        assert_eq!(v.hurt(Cause::Generic, 6.0, 0, 2), Some(2.0));
        assert_eq!(v.health, 14.0);
        for _ in 0..10 {
            v.tick(&s(0));
        }
        assert_eq!(v.hurt(Cause::Generic, 1.0, 0, 2), Some(1.0));
    }

    #[test]
    fn natural_regeneration_uses_saturation() {
        let mut v = Vitals::default();
        v.health = 10.0;
        for _ in 0..10 {
            v.tick(&s(0));
        }
        // saturation 5 → heal 5/6, exhaustion +5 → 1 saturation spent on the next tick
        assert!((v.health - (10.0 + 5.0 / 6.0)).abs() < 1e-4);
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
            hits += v.tick(&w).iter().filter(|(c, _)| *c == Cause::Drown).count();
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
