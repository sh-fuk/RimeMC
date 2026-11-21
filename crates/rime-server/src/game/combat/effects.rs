//! Mob effects (`MobEffects`, `MobEffectInstance`): the 40 vanilla effects
//! with their attribute modifiers, tick intervals and instant behaviour.

use super::attr::{Attr, Op};

macro_rules! effects {
    ($($v:ident = $n:literal),* $(,)?) => {
        /// Registry order (= protocol id).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum Effect { $($v),* }
        pub const ALL: &[Effect] = &[$(Effect::$v),*];
        const NAMES: &[&str] = &[$($n),*];
    };
}

effects! {
    Speed = "speed", Slowness = "slowness", Haste = "haste", MiningFatigue = "mining_fatigue",
    Strength = "strength", InstantHealth = "instant_health", InstantDamage = "instant_damage",
    JumpBoost = "jump_boost", Nausea = "nausea", Regeneration = "regeneration",
    Resistance = "resistance", FireResistance = "fire_resistance", WaterBreathing = "water_breathing",
    Invisibility = "invisibility", Blindness = "blindness", NightVision = "night_vision",
    Hunger = "hunger", Weakness = "weakness", Poison = "poison", Wither = "wither",
    HealthBoost = "health_boost", Absorption = "absorption", Saturation = "saturation",
    Glowing = "glowing", Levitation = "levitation", Luck = "luck", Unluck = "unluck",
    SlowFalling = "slow_falling", ConduitPower = "conduit_power", DolphinsGrace = "dolphins_grace",
    BadOmen = "bad_omen", HeroOfTheVillage = "hero_of_the_village", Darkness = "darkness",
    TrialOmen = "trial_omen", RaidOmen = "raid_omen", WindCharged = "wind_charged",
    Weaving = "weaving", Oozing = "oozing", Infested = "infested",
    BreathOfTheNautilus = "breath_of_the_nautilus",
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Beneficial,
    Harmful,
    Neutral,
}

impl Effect {
    pub fn id(self) -> i32 {
        self as i32
    }
    pub fn from_id(id: i32) -> Option<Effect> {
        ALL.get(usize::try_from(id).ok()?).copied()
    }
    pub fn name(self) -> &'static str {
        NAMES[self as usize]
    }
    pub fn from_name(n: &str) -> Option<Effect> {
        let n = n.trim_start_matches("minecraft:");
        NAMES.iter().position(|x| *x == n).map(|i| ALL[i])
    }

    pub fn category(self) -> Category {
        use Effect::*;
        match self {
            Slowness | MiningFatigue | InstantDamage | Nausea | Blindness | Hunger | Weakness | Poison | Wither | Levitation | Unluck | Darkness | WindCharged | Weaving | Oozing | Infested => Category::Harmful,
            Glowing | BadOmen | TrialOmen | RaidOmen => Category::Neutral,
            _ => Category::Beneficial,
        }
    }

    /// `MobEffect.attributeModifiers` (amount is multiplied by amplifier+1).
    pub fn modifiers(self) -> &'static [(Attr, &'static str, f64, Op)] {
        use Effect::*;
        match self {
            Speed => &[(Attr::MovementSpeed, "minecraft:effect.speed", 0.2f32 as f64, Op::AddMultipliedTotal)],
            Slowness => &[(Attr::MovementSpeed, "minecraft:effect.slowness", -0.15f32 as f64, Op::AddMultipliedTotal)],
            Haste => &[(Attr::AttackSpeed, "minecraft:effect.haste", 0.1f32 as f64, Op::AddMultipliedTotal)],
            MiningFatigue => &[(Attr::AttackSpeed, "minecraft:effect.mining_fatigue", -0.1f32 as f64, Op::AddMultipliedTotal)],
            Strength => &[(Attr::AttackDamage, "minecraft:effect.strength", 3.0, Op::AddValue)],
            JumpBoost => &[(Attr::SafeFallDistance, "minecraft:effect.jump_boost", 1.0, Op::AddValue)],
            Invisibility => &[(Attr::WaypointTransmitRange, "minecraft:effect.waypoint_transmit_range_hide", -1.0, Op::AddMultipliedTotal)],
            Weakness => &[(Attr::AttackDamage, "minecraft:effect.weakness", -4.0, Op::AddValue)],
            HealthBoost => &[(Attr::MaxHealth, "minecraft:effect.health_boost", 4.0, Op::AddValue)],
            Absorption => &[(Attr::MaxAbsorption, "minecraft:effect.absorption", 4.0, Op::AddValue)],
            Luck => &[(Attr::Luck, "minecraft:effect.luck", 1.0, Op::AddValue)],
            Unluck => &[(Attr::Luck, "minecraft:effect.unluck", -1.0, Op::AddValue)],
            _ => &[],
        }
    }

    pub fn is_instant(self) -> bool {
        matches!(self, Effect::InstantHealth | Effect::InstantDamage | Effect::Saturation)
    }

    /// `MobEffect.shouldApplyEffectTickThisTick`
    pub fn should_tick(self, tick_count: i32, amp: i32) -> bool {
        let interval = |base: i32| {
            let i = if amp >= 31 { 0 } else { base >> amp };
            if i > 0 {
                tick_count % i == 0
            } else {
                true
            }
        };
        match self {
            Effect::Poison => interval(25),
            Effect::Wither => interval(40),
            Effect::Regeneration => interval(50),
            Effect::Hunger | Effect::Absorption => true,
            e if e.is_instant() => tick_count >= 1,
            _ => false,
        }
    }
}

pub const INFINITE: i32 = -1;

/// `MobEffectInstance`
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub effect: Effect,
    pub duration: i32,
    pub amplifier: i32,
    pub ambient: bool,
    pub visible: bool,
    pub show_icon: bool,
    pub hidden: Option<Box<Instance>>,
}

impl Instance {
    pub fn new(effect: Effect, duration: i32, amplifier: i32) -> Self {
        Instance { effect, duration, amplifier: amplifier.clamp(0, 255), ambient: false, visible: true, show_icon: true, hidden: None }
    }
    pub fn infinite(&self) -> bool {
        self.duration == INFINITE
    }
    fn shorter_than(&self, o: &Instance) -> bool {
        !self.infinite() && (self.duration < o.duration || o.infinite())
    }
    pub fn ends_within(&self, ticks: i32) -> bool {
        !self.infinite() && self.duration <= ticks
    }
    fn set_details_from(&mut self, o: &Instance) {
        self.duration = o.duration;
        self.amplifier = o.amplifier;
        self.ambient = o.ambient;
        self.visible = o.visible;
        self.show_icon = o.show_icon;
    }

    /// `MobEffectInstance.update`
    pub fn update(&mut self, take: &Instance) -> bool {
        let mut changed = false;
        if take.amplifier > self.amplifier {
            if take.shorter_than(self) {
                let prev = self.hidden.take();
                let mut h = self.clone();
                h.hidden = prev;
                self.hidden = Some(Box::new(h));
            }
            self.amplifier = take.amplifier;
            self.duration = take.duration;
            changed = true;
        } else if self.shorter_than(take) {
            if take.amplifier == self.amplifier {
                self.duration = take.duration;
                changed = true;
            } else if let Some(h) = self.hidden.as_mut() {
                h.update(take);
            } else {
                let mut t = take.clone();
                t.hidden = None;
                self.hidden = Some(Box::new(t));
            }
        }
        if (!take.ambient && self.ambient) || changed {
            self.ambient = take.ambient;
            changed = true;
        }
        if take.visible != self.visible {
            self.visible = take.visible;
            changed = true;
        }
        if take.show_icon != self.show_icon {
            self.show_icon = take.show_icon;
            changed = true;
        }
        changed
    }

    fn tick_down(&mut self) {
        if let Some(h) = self.hidden.as_mut() {
            h.tick_down();
        }
        if !self.infinite() && self.duration != 0 {
            self.duration -= 1;
        }
    }

    fn downgrade(&mut self) -> bool {
        if self.duration == 0 {
            if let Some(h) = self.hidden.take() {
                self.set_details_from(&h);
                self.hidden = h.hidden;
                return true;
            }
        }
        false
    }

    pub fn has_remaining(&self) -> bool {
        self.infinite() || self.duration > 0
    }

    /// Duration tick after `applyEffectTick` succeeded. Returns
    /// (still active, downgraded to the hidden effect).
    pub fn tick_duration(&mut self) -> (bool, bool) {
        self.tick_down();
        let d = self.downgrade();
        (self.has_remaining(), d)
    }

    /// `update_mob_effect` packet body after the entity id.
    pub fn write(&self, b: &mut Vec<u8>) {
        use crate::net::codec::Out;
        let flags = self.ambient as u8 | (self.visible as u8) << 1 | (self.show_icon as u8) << 2 | 0x08;
        b.vi(self.effect.id()).vi(self.amplifier).vi(self.duration).u8(flags);
    }

    pub fn to_nbt(&self) -> crate::nbt::Tag {
        use crate::nbt::Tag;
        let mut c = vec![
            ("id".to_string(), Tag::String(format!("minecraft:{}", self.effect.name()))),
            ("amplifier".to_string(), Tag::Byte(self.amplifier as i8)),
            ("duration".to_string(), Tag::Int(self.duration)),
            ("ambient".to_string(), Tag::Byte(self.ambient as i8)),
            ("show_particles".to_string(), Tag::Byte(self.visible as i8)),
            ("show_icon".to_string(), Tag::Byte(self.show_icon as i8)),
        ];
        if let Some(h) = &self.hidden {
            c.push(("hidden_effect".to_string(), h.to_nbt()));
        }
        Tag::Compound(c)
    }

    pub fn from_nbt(t: &crate::nbt::Tag) -> Option<Instance> {
        let effect = Effect::from_name(t.get("id")?.as_str()?)?;
        let num = |k: &str| t.get(k).and_then(|v| v.as_i64());
        let flag = |k: &str, d: bool| num(k).map(|v| v != 0).unwrap_or(d);
        Some(Instance {
            effect,
            duration: num("duration").unwrap_or(0) as i32,
            amplifier: (num("amplifier").unwrap_or(0) as i32) & 0xff,
            ambient: flag("ambient", false),
            visible: flag("show_particles", true),
            show_icon: flag("show_icon", true),
            hidden: t.get("hidden_effect").and_then(Instance::from_nbt).map(Box::new),
        })
    }
}

/// Parses a potion contents / consume effect entry
/// (`{"id": "...", "amplifier": n, "duration": n, ...}`).
pub fn instance_from_json(v: &serde_json::Value) -> Option<Instance> {
    let effect = Effect::from_name(v["id"].as_str()?)?;
    let mut i = Instance::new(effect, v["duration"].as_i64().unwrap_or(0) as i32, v["amplifier"].as_i64().unwrap_or(0) as i32);
    i.ambient = v["ambient"].as_bool().unwrap_or(false);
    i.visible = v["show_particles"].as_bool().unwrap_or(true);
    i.show_icon = v["show_icon"].as_bool().unwrap_or(i.visible);
    Some(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_intervals() {
        assert_eq!(Effect::Speed.id(), 0);
        assert_eq!(Effect::BreathOfTheNautilus.id(), 39);
        assert_eq!(Effect::from_name("minecraft:poison"), Some(Effect::Poison));
        assert!(Effect::Poison.should_tick(25, 0) && !Effect::Poison.should_tick(24, 0));
        assert!(Effect::Poison.should_tick(12, 1) && Effect::Poison.should_tick(7, 5));
        assert!(Effect::Regeneration.should_tick(50, 0) && Effect::Regeneration.should_tick(25, 1));
    }

    #[test]
    fn hidden_effect() {
        // speed II for 10 ticks over speed I for 100: back to speed I afterwards
        let mut e = Instance::new(Effect::Speed, 100, 0);
        assert!(e.update(&Instance::new(Effect::Speed, 10, 1)));
        assert_eq!((e.amplifier, e.duration), (1, 10));
        for _ in 0..10 {
            e.tick_duration();
        }
        assert_eq!((e.amplifier, e.duration), (0, 90));
    }
}
