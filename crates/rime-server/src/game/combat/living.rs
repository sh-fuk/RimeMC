//! `LivingEntity` state shared by players and mobs: health, absorption,
//! invulnerability frames, attributes, equipment modifiers, mob effects and
//! the exact `hurtServer` / `actuallyHurt` / `knockback` / shield / totem
//! pipeline. World side effects come out as [`Event`]s.

use rand::Rng;

use super::attr::{self, Attr, Modifier};
use super::damage::DamageSource;
use super::effects::{self, Effect, Instance};
use super::enchant::{self, Ctx, EntInfo};
use crate::data::Stack;

pub const MAINHAND: usize = 0;
pub const OFFHAND: usize = 1;
pub const FEET: usize = 2;
pub const LEGS: usize = 3;
pub const CHEST: usize = 4;
pub const HEAD: usize = 5;
pub const BODY: usize = 6;
pub const SADDLE: usize = 7;
/// `EquipmentSlot.VALUES` order
pub const SLOT_NAMES: [&str; 8] = ["mainhand", "offhand", "feet", "legs", "chest", "head", "body", "saddle"];

pub type Equipment = [Option<Stack>; 8];

/// What the rest of the server has to do after a call into [`Living`].
#[derive(Clone, Debug)]
pub enum Event {
    /// `broadcastDamageEvent` (damage_event packet + hurt animation)
    Damage { ty: &'static str, causing: Option<i32>, direct: Option<i32>, pos: Option<[f64; 3]> },
    /// shield block sound
    Blocked,
    /// `markHurt`: the velocity must be sent
    VelocityChanged,
    /// `blockedByItem`: knock the attacker back
    KnockAttacker { eid: i32, power: f64, xd: f64, zd: f64 },
    /// shield put on cooldown (item group, ticks) and use stopped
    ShieldDisabled { item: i32, ticks: i32 },
    /// equipment slot changed (durability / broken / totem used)
    SlotChanged(usize),
    /// `onEquippedItemBroken` (entity event 47+)
    ItemBroken(usize, i32),
    /// totem of undying (entity event 35)
    Totem,
    HurtSound,
    /// `die(source)`
    Died(DamageSource),
    Exhaustion(f32),
    /// `FoodData.eat(nutrition, saturation)` from the saturation effect
    Eat(i32, f32),
    EffectAdded(Effect),
    EffectUpdated(Effect),
    EffectRemoved(Effect),
    /// a synced attribute changed (update_attributes)
    AttributesChanged,
    HealthChanged,
}

/// Per-call view of the entity that owns the [`Living`].
pub struct Body {
    pub eid: i32,
    pub info: EntInfo,
    pub pos: [f64; 3],
    pub head_yaw: f32,
    /// `deltaMovement` (read and written by knockback)
    pub velocity: [f64; 3],
    /// `Some` for players
    pub player: Option<PlayerSide>,
    /// `Entity.invulnerable` (NBT flag)
    pub invulnerable: bool,
    pub fire_immune: bool,
    pub raining: bool,
    pub thundering: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct PlayerSide {
    /// `abilities.invulnerable` (creative / spectator)
    pub abilities_invulnerable: bool,
    /// creative mode: items do not lose durability
    pub infinite_materials: bool,
    /// 0 peaceful .. 3 hard
    pub difficulty: i32,
    pub rule_drowning: bool,
    pub rule_fall: bool,
    pub rule_fire: bool,
    pub rule_freeze: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UseItem {
    /// 0 main hand, 1 off hand
    pub hand: u8,
    pub item: i32,
    /// ticks since the use started
    pub elapsed: i32,
}

#[derive(Clone, Debug)]
pub struct Living {
    pub health: f32,
    pub absorption: f32,
    pub last_hurt: f32,
    pub invulnerable_time: i32,
    pub hurt_time: i32,
    /// `LivingEntity.dead` (die() ran)
    pub dead: bool,
    pub attrs: attr::Map,
    pub effects: Vec<Instance>,
    pub last_equipment: Equipment,
    pub using: Option<UseItem>,
    /// item cooldowns: (cooldown group item id, start tick, end tick)
    pub cooldowns: Vec<(i32, i32, i32)>,
    pub tick_count: i32,
    pub attack_ticker: i32,
    pub last_hurt_by_mob: Option<(i32, i32)>,
    /// `lastHurtMob`: (eid, tick) of the last entity this one attacked
    pub last_hurt_mob: Option<(i32, i32)>,
    pub last_hurt_by_player: Option<(i32, i32)>,
    pub last_damage: Option<(DamageSource, i32)>,
    pub events: Vec<Event>,
}

impl Living {
    pub fn new(attrs: attr::Map) -> Living {
        let mut l = Living {
            health: 0.0,
            absorption: 0.0,
            last_hurt: 0.0,
            invulnerable_time: 0,
            hurt_time: 0,
            dead: false,
            attrs,
            effects: Vec::new(),
            last_equipment: Default::default(),
            using: None,
            cooldowns: Vec::new(),
            tick_count: 0,
            attack_ticker: 0,
            last_hurt_by_mob: None,
            last_hurt_mob: None,
            last_hurt_by_player: None,
            last_damage: None,
            events: Vec::new(),
        };
        l.health = l.max_health();
        l
    }

    pub fn player() -> Living {
        Living::new(attr::Map::new(&attr::player()))
    }

    pub fn max_health(&mut self) -> f32 {
        self.attrs.value(Attr::MaxHealth) as f32
    }
    pub fn max_absorption(&mut self) -> f32 {
        self.attrs.value(Attr::MaxAbsorption) as f32
    }
    /// `isDeadOrDying`
    pub fn dead_or_dying(&self) -> bool {
        self.health <= 0.0
    }
    /// `setHealth`
    pub fn set_health(&mut self, h: f32) {
        let max = self.max_health();
        let h = h.clamp(0.0, max);
        if h != self.health {
            self.health = h;
            self.events.push(Event::HealthChanged);
        }
    }
    /// `heal`
    pub fn heal(&mut self, amount: f32) {
        if self.health > 0.0 {
            self.set_health(self.health + amount);
        }
    }
    /// `setAbsorptionAmount`
    pub fn set_absorption(&mut self, a: f32) {
        let max = self.max_absorption();
        let a = a.clamp(0.0, max);
        if a != self.absorption {
            self.absorption = a;
            self.events.push(Event::HealthChanged);
        }
    }

    pub fn effect(&self, e: Effect) -> Option<&Instance> {
        self.effects.iter().find(|i| i.effect == e)
    }
    pub fn has_effect(&self, e: Effect) -> bool {
        self.effect(e).is_some()
    }

    /// `getArmorValue`
    pub fn armor_value(&mut self) -> i32 {
        self.attrs.value(Attr::Armor).floor() as i32
    }

    // ------------------------------------------------------------ attributes

    /// `onAttributeUpdated` for health / absorption after modifiers changed.
    fn refresh_attributes(&mut self) {
        if !self.attrs.dirty.is_empty() {
            let max = self.max_health();
            if self.health > max {
                self.set_health(max);
            }
            let maxa = self.max_absorption();
            if self.absorption > maxa {
                self.set_absorption(maxa);
            }
            if self.attrs.dirty.iter().any(|a| a.synced()) {
                self.events.push(Event::AttributesChanged);
            }
        }
    }

    /// `collectEquipmentChanges`: swaps the attribute modifiers of changed
    /// slots. Returns the changed slots.
    pub fn update_equipment(&mut self, eq: &Equipment) -> Vec<usize> {
        let mut changed = Vec::new();
        for slot in 0..8 {
            let prev = self.last_equipment[slot];
            let cur = eq[slot];
            let same = match (prev, cur) {
                (None, None) => true,
                (Some(a), Some(b)) => a.same(&b) && a.count == b.count,
                _ => false,
            };
            if !same {
                changed.push(slot);
                if let Some(p) = prev {
                    for (a, m) in item_modifiers(p, slot) {
                        self.attrs.remove_modifier(a, &m.id);
                    }
                }
            }
        }
        for &slot in &changed {
            if let Some(c) = eq[slot] {
                if !is_broken(c) {
                    for (a, m) in item_modifiers(c, slot) {
                        self.attrs.remove_modifier(a, &m.id);
                        self.attrs.add_modifier(a, m);
                    }
                }
            }
            self.last_equipment[slot] = eq[slot];
        }
        self.refresh_attributes();
        changed
    }

    // ------------------------------------------------------------ effects

    /// `canBeAffected`
    pub fn can_be_affected(info: &EntInfo, e: Effect) -> bool {
        if enchant::entity_tag(info.type_name, "immune_to_infested") {
            e != Effect::Infested
        } else if enchant::entity_tag(info.type_name, "immune_to_oozing") {
            e != Effect::Oozing
        } else if enchant::entity_tag(info.type_name, "ignores_poison_and_regen") {
            e != Effect::Regeneration && e != Effect::Poison
        } else {
            true
        }
    }

    fn add_effect_modifiers(&mut self, e: Effect, amp: i32) {
        for &(a, id, amount, op) in e.modifiers() {
            if self.attrs.has(a) {
                self.attrs.remove_modifier(a, id);
                self.attrs.add_modifier(a, Modifier { id: id.to_string(), amount: amount * (amp + 1) as f64, op });
            }
        }
    }
    fn remove_effect_modifiers(&mut self, e: Effect) {
        for &(a, id, _, _) in e.modifiers() {
            self.attrs.remove_modifier(a, id);
        }
    }

    /// `addEffect`
    pub fn add_effect(&mut self, info: &EntInfo, new: Instance) -> bool {
        if !Living::can_be_affected(info, new.effect) {
            return false;
        }
        let (e, amp) = (new.effect, new.amplifier);
        let changed = match self.effects.iter().position(|i| i.effect == e) {
            None => {
                self.effects.push(new.clone());
                self.add_effect_modifiers(e, amp);
                self.events.push(Event::EffectAdded(e));
                true
            }
            Some(i) => {
                if self.effects[i].update(&new) {
                    let amp = self.effects[i].amplifier;
                    self.remove_effect_modifiers(e);
                    self.add_effect_modifiers(e, amp);
                    self.events.push(Event::EffectUpdated(e));
                    true
                } else {
                    false
                }
            }
        };
        self.refresh_attributes();
        // onEffectStarted (runs with the *new* instance's amplifier)
        if e == Effect::Absorption {
            let a = self.absorption.max((4 * (1 + amp)) as f32);
            self.set_absorption(a);
        }
        changed
    }

    /// `forceAddEffect`
    pub fn force_add_effect(&mut self, info: &EntInfo, new: Instance) {
        if !Living::can_be_affected(info, new.effect) {
            return;
        }
        let e = new.effect;
        let amp = new.amplifier;
        match self.effects.iter().position(|i| i.effect == e) {
            None => {
                self.effects.push(new);
                self.add_effect_modifiers(e, amp);
                self.events.push(Event::EffectAdded(e));
            }
            Some(i) => {
                self.effects[i] = new;
                self.remove_effect_modifiers(e);
                self.add_effect_modifiers(e, amp);
                self.events.push(Event::EffectUpdated(e));
            }
        }
        self.refresh_attributes();
    }

    pub fn remove_effect(&mut self, e: Effect) -> bool {
        match self.effects.iter().position(|i| i.effect == e) {
            Some(i) => {
                self.effects.remove(i);
                self.remove_effect_modifiers(e);
                self.events.push(Event::EffectRemoved(e));
                self.refresh_attributes();
                true
            }
            None => false,
        }
    }

    /// `removeAllEffects` (milk, totem)
    pub fn remove_all_effects(&mut self) -> bool {
        if self.effects.is_empty() {
            return false;
        }
        for i in std::mem::take(&mut self.effects) {
            self.remove_effect_modifiers(i.effect);
            self.events.push(Event::EffectRemoved(i.effect));
        }
        self.refresh_attributes();
        true
    }

    /// `MobEffect.applyEffectTick`; false removes the effect.
    fn apply_effect_tick(&mut self, eq: &mut Equipment, body: &mut Body, e: Effect, amp: i32, rng: &mut impl Rng) -> bool {
        let inverted = enchant::entity_tag(body.info.type_name, "inverted_healing_and_harm");
        match e {
            Effect::Poison => {
                if self.health > 1.0 {
                    self.hurt(eq, body, &DamageSource::new("magic"), 1.0, rng);
                }
            }
            Effect::Wither => {
                self.hurt(eq, body, &DamageSource::new("wither"), 1.0, rng);
            }
            Effect::Regeneration => {
                if self.health < self.max_health() {
                    self.heal(1.0);
                }
            }
            Effect::Hunger => {
                if body.player.is_some() {
                    self.events.push(Event::Exhaustion(0.005 * (amp + 1) as f32));
                }
            }
            Effect::Saturation => {
                if body.player.is_some() {
                    self.events.push(Event::Eat(amp + 1, 1.0));
                }
            }
            Effect::Absorption => return self.absorption > 0.0,
            Effect::InstantHealth | Effect::InstantDamage => {
                let harm = e == Effect::InstantDamage;
                if harm == inverted {
                    self.heal((4i32.checked_shl(amp as u32).unwrap_or(0)).max(0) as f32);
                } else {
                    self.hurt(eq, body, &DamageSource::new("magic"), (6i32.checked_shl(amp as u32).unwrap_or(0)) as f32, rng);
                }
            }
            _ => {}
        }
        true
    }

    /// `applyInstantenousEffect` (splash potions, arrows, drinking).
    pub fn apply_instant(&mut self, eq: &mut Equipment, body: &mut Body, e: Effect, amp: i32, scale: f64, source: Option<(super::damage::EntityRef, Option<super::damage::EntityRef>)>, rng: &mut impl Rng) {
        let inverted = enchant::entity_tag(body.info.type_name, "inverted_healing_and_harm");
        match e {
            Effect::InstantHealth | Effect::InstantDamage => {
                let harm = e == Effect::InstantDamage;
                if harm == inverted {
                    let amount = (scale * (4i64 << amp.min(31)) as f64 + 0.5) as i32;
                    self.heal(amount as f32);
                } else {
                    let amount = (scale * (6i64 << amp.min(31)) as f64 + 0.5) as i32;
                    let src = match source {
                        None => DamageSource::new("magic"),
                        Some((direct, owner)) => DamageSource::indirect("indirect_magic", direct, owner),
                    };
                    self.hurt(eq, body, &src, amount as f32, rng);
                }
            }
            _ => {
                self.apply_effect_tick(eq, body, e, amp, rng);
            }
        }
    }

    /// `tickEffects` (server side)
    pub fn tick_effects(&mut self, eq: &mut Equipment, body: &mut Body, rng: &mut impl Rng) {
        let list: Vec<Effect> = self.effects.iter().map(|i| i.effect).collect();
        for e in list {
            let Some(inst) = self.effect(e).cloned() else { continue };
            if !inst.has_remaining() {
                self.remove_effect(e);
                continue;
            }
            let tick_count = if inst.infinite() { self.tick_count } else { inst.duration };
            if e.should_tick(tick_count, inst.amplifier) && !self.apply_effect_tick(eq, body, e, inst.amplifier, rng) {
                self.remove_effect(e);
                continue;
            }
            let Some(i) = self.effects.iter().position(|x| x.effect == e) else { continue };
            let (alive, downgraded) = self.effects[i].tick_duration();
            if downgraded {
                let amp = self.effects[i].amplifier;
                self.remove_effect_modifiers(e);
                self.add_effect_modifiers(e, amp);
                self.refresh_attributes();
                self.events.push(Event::EffectUpdated(e));
            }
            if !alive {
                self.remove_effect(e);
            } else if self.effects[i].duration % 600 == 0 {
                self.events.push(Event::EffectUpdated(e));
            }
        }
    }

    /// Per-tick counters (`baseTick` / `LivingEntity.tick`).
    pub fn tick_counters(&mut self) {
        self.tick_count += 1;
        self.attack_ticker += 1;
        if self.hurt_time > 0 {
            self.hurt_time -= 1;
        }
        if self.invulnerable_time > 0 {
            self.invulnerable_time -= 1;
        }
        if let Some(u) = self.using.as_mut() {
            u.elapsed += 1;
        }
        let t = self.tick_count;
        self.cooldowns.retain(|c| c.2 > t);
        if let Some((_, at)) = self.last_hurt_by_mob {
            if t - at > 100 {
                self.last_hurt_by_mob = None;
            }
        }
        if let Some((_, at)) = self.last_hurt_by_player {
            if t - at > 100 {
                self.last_hurt_by_player = None;
            }
        }
    }

    pub fn on_cooldown(&self, item: i32) -> bool {
        self.cooldowns.iter().any(|c| c.0 == item)
    }

    // ------------------------------------------------------------ damage

    /// `Entity.isInvulnerableToBase` + `LivingEntity.isInvulnerableTo`
    /// (+ `Player.isInvulnerableTo` game rules).
    pub fn invulnerable_to(&self, eq: &Equipment, body: &Body, src: &DamageSource, rng: &mut impl Rng) -> bool {
        if (body.invulnerable && !src.is("bypasses_invulnerability") && !src.is_creative_player())
            || (src.is("is_fire") && body.fire_immune)
            || (src.is("is_fall") && enchant::entity_tag(body.info.type_name, "fall_damage_immune"))
        {
            return true;
        }
        if is_immune_to_damage(eq, body, src, rng) {
            return true;
        }
        if let Some(p) = body.player {
            if src.is("is_drowning") {
                return !p.rule_drowning;
            } else if src.is("is_fall") {
                return !p.rule_fall;
            } else if src.is("is_fire") {
                return !p.rule_fire;
            } else if src.is("is_freezing") {
                return !p.rule_freeze;
            }
        }
        false
    }

    /// `Player.hurtServer` / `LivingEntity.hurtServer`. Returns `success`.
    pub fn hurt(&mut self, eq: &mut Equipment, body: &mut Body, src: &DamageSource, mut damage: f32, rng: &mut impl Rng) -> bool {
        if let Some(p) = body.player {
            if self.invulnerable_to(eq, body, src, rng) {
                return false;
            }
            if p.abilities_invulnerable && !src.is("bypasses_invulnerability") {
                return false;
            }
            if self.dead_or_dying() {
                return false;
            }
            if src.scales_with_difficulty() {
                match p.difficulty {
                    0 => damage = 0.0,
                    1 => damage = (damage / 2.0 + 1.0).min(damage),
                    3 => damage = damage * 3.0 / 2.0,
                    _ => {}
                }
            }
            if damage == 0.0 {
                return false;
            }
        }
        self.hurt_living(eq, body, src, damage, rng)
    }

    fn hurt_living(&mut self, eq: &mut Equipment, body: &mut Body, src: &DamageSource, mut damage: f32, rng: &mut impl Rng) -> bool {
        if self.invulnerable_to(eq, body, src, rng) || self.dead_or_dying() {
            return false;
        }
        if src.is("is_fire") && self.has_effect(Effect::FireResistance) {
            return false;
        }
        if damage < 0.0 {
            damage = 0.0;
        }
        let item_in_use = self.using;
        let blocked_amount = self.apply_item_blocking(eq, body, src, damage);
        damage -= blocked_amount;
        let blocked = blocked_amount > 0.0;
        if src.is("is_freezing") && enchant::entity_tag(body.info.type_name, "freeze_hurts_extra_types") {
            damage *= 5.0;
        }
        if src.is("damages_helmet") && eq[HEAD].is_some() {
            self.do_hurt_equipment(eq, body, src, damage, &[HEAD], rng);
            damage *= 0.75;
        }
        if damage.is_nan() || damage.is_infinite() {
            damage = f32::MAX;
        }
        let mut full = true;
        if self.invulnerable_time as f32 > 10.0 && !src.is("bypasses_cooldown") {
            if damage <= self.last_hurt {
                return false;
            }
            self.actually_hurt(eq, body, src, damage - self.last_hurt, rng);
            self.last_hurt = damage;
            full = false;
        } else {
            self.last_hurt = damage;
            self.invulnerable_time = 20;
            self.actually_hurt(eq, body, src, damage, rng);
            self.hurt_time = 10;
        }
        // resolveMobResponsibleForDamage / resolvePlayerResponsibleForDamage
        if let Some(c) = src.causing {
            if c.living && !src.is("no_anger") && (src.ty != "wind_charge" || !enchant::entity_tag(body.info.type_name, "no_anger_from_wind_charge")) {
                self.last_hurt_by_mob = Some((c.eid, self.tick_count));
            }
            if c.player {
                self.last_hurt_by_player = Some((c.eid, self.tick_count));
            }
        }
        if full {
            let shield = item_in_use.and_then(|u| eq[u.hand as usize]).and_then(|s| s.defaults().blocks_attacks.as_ref());
            if blocked && shield.is_some() {
                self.events.push(Event::Blocked);
            } else {
                self.events.push(Event::Damage { ty: src.ty, causing: src.causing.map(|c| c.eid), direct: src.direct.map(|c| c.eid), pos: src.position });
            }
            if !src.is("no_impact") && (!blocked || damage > 0.0) {
                self.events.push(Event::VelocityChanged);
            }
            if !src.is("no_knockback") {
                let (mut xd, mut zd) = (0.0, 0.0);
                if src.projectile {
                    if let Some(d) = src.direct {
                        // Projectile.calculateHorizontalHurtKnockbackDirection: the
                        // projectile's horizontal motion, stored in `pos` delta
                        let v = src.projectile_motion.unwrap_or([body.pos[0] - d.pos[0], 0.0, body.pos[2] - d.pos[2]]);
                        let (x, z) = horizontal_knockback_dir(v);
                        xd = -x;
                        zd = -z;
                    }
                } else if let Some(p) = src.source_position() {
                    xd = p[0] - body.pos[0];
                    zd = p[2] - body.pos[2];
                }
                self.knockback(body, 0.4, xd, zd, rng);
            }
        }
        if self.dead_or_dying() {
            if !self.check_totem(eq, body, src, rng) {
                self.die(body, src);
            }
        } else if full {
            self.events.push(Event::HurtSound);
        }
        let success = !blocked || damage > 0.0;
        if success {
            self.last_damage = Some((src.clone(), self.tick_count));
            // MobEffect.onMobHurt: infested spawns silverfish (handled by caller)
        }
        success
    }

    /// `getItemBlockingWith`
    pub fn blocking_with(&self, eq: &Equipment) -> Option<(usize, Stack)> {
        let u = self.using?;
        let s = eq[u.hand as usize]?;
        let b = s.defaults().blocks_attacks.as_ref()?;
        let delay = (b.block_delay_seconds * 20.0).round() as i32;
        (u.elapsed >= delay).then_some((u.hand as usize, s))
    }

    /// `applyItemBlocking`
    fn apply_item_blocking(&mut self, eq: &mut Equipment, body: &mut Body, src: &DamageSource, damage: f32) -> f32 {
        if damage <= 0.0 {
            return 0.0;
        }
        let Some((hand, stack)) = self.blocking_with(eq) else { return 0.0 };
        let Some(b) = stack.defaults().blocks_attacks.as_ref() else { return 0.0 };
        if b.bypassed_by.as_deref().is_some_and(|set| crate::components::damage_set_contains(set, src.ty)) {
            return 0.0;
        }
        if src.pierce_level > 0 {
            return 0.0;
        }
        let angle = match src.source_position() {
            Some(sp) => {
                let view = view_vector(0.0, body.head_yaw);
                let (dx, dz) = (sp[0] - body.pos[0], sp[2] - body.pos[2]);
                let len = (dx * dx + dz * dz).sqrt();
                let (nx, nz) = if len < 1.0e-5 { (0.0, 0.0) } else { (dx / len, dz / len) };
                (nx * view[0] + nz * view[2]).acos()
            }
            None => std::f32::consts::PI as f64,
        };
        let mut blocked_dmg = 0.0f32;
        for r in &b.reductions {
            if angle > ((std::f32::consts::PI / 180.0) * r.angle) as f64 {
                continue;
            }
            if r.types.as_deref().is_some_and(|t| !crate::components::damage_set_contains(t, src.ty)) {
                continue;
            }
            blocked_dmg += (r.base + r.factor * damage).clamp(0.0, damage);
        }
        let blocked_dmg = blocked_dmg.clamp(0.0, damage);
        // hurtBlockingItem (players only)
        if let Some(p) = body.player {
            let (thr, base, factor) = b.item_damage;
            let item_damage = if blocked_dmg < thr { 0 } else { (base + factor * blocked_dmg).floor() as i32 };
            if item_damage > 0 {
                hurt_and_break(eq, hand, item_damage, p.infinite_materials, &mut self.events, &mut rand::thread_rng());
            }
        }
        if blocked_dmg > 0.0 && !src.is("is_projectile") {
            if let Some(att) = src.direct.filter(|d| d.living) {
                // blockUsingItem -> attacker.blockedByItem(this)
                self.events.push(Event::KnockAttacker { eid: att.eid, power: 0.5, xd: att.pos[0] - body.pos[0], zd: att.pos[2] - body.pos[2] });
                if body.player.is_some() {
                    let secs = src.weapon.and_then(|w| w.defaults().weapon.map(|x| x.1)).unwrap_or(0.0);
                    if secs > 0.0 {
                        let s = secs * b.disable_cooldown_scale;
                        let ticks = if s > 0.0 { (s * 20.0).round() as i32 } else { 0 };
                        if ticks > 0 {
                            self.cooldowns.retain(|c| c.0 != stack.item);
                            self.cooldowns.push((stack.item, self.tick_count, self.tick_count + ticks));
                            self.using = None;
                            self.events.push(Event::ShieldDisabled { item: stack.item, ticks });
                        }
                    }
                }
            }
        }
        blocked_dmg
    }

    /// `actuallyHurt`
    fn actually_hurt(&mut self, eq: &mut Equipment, body: &mut Body, src: &DamageSource, dmg: f32, rng: &mut impl Rng) {
        if self.invulnerable_to(eq, body, src, rng) {
            return;
        }
        let dmg = self.after_armor(eq, body, src, dmg, rng);
        let dmg = self.after_magic(eq, body, src, dmg, rng);
        let left = (dmg - self.absorption).max(0.0);
        self.set_absorption(self.absorption - (dmg - left));
        if left != 0.0 {
            if body.player.is_some() {
                self.events.push(Event::Exhaustion(src.food_exhaustion()));
            }
            self.set_health(self.health - left);
            if body.player.is_none() {
                self.set_absorption(self.absorption - left);
            }
        }
    }

    /// `getDamageAfterArmorAbsorb`
    fn after_armor(&mut self, eq: &mut Equipment, body: &mut Body, src: &DamageSource, dmg: f32, rng: &mut impl Rng) -> f32 {
        if src.is("bypasses_armor") {
            return dmg;
        }
        // hurtArmor: players damage their four armour pieces; horses and wolves
        // their body armour; other mobs nothing
        if body.player.is_some() {
            self.do_hurt_equipment(eq, body, src, dmg, &[FEET, LEGS, CHEST, HEAD], rng);
        } else if matches!(body.info.type_name, "horse" | "wolf") {
            self.do_hurt_equipment(eq, body, src, dmg, &[BODY], rng);
        }
        let armor = self.armor_value() as f32;
        let tough = self.attrs.value(Attr::ArmorToughness) as f32;
        damage_after_absorb(dmg, armor, tough, src, body, rng)
    }

    /// `getDamageAfterMagicAbsorb`
    fn after_magic(&mut self, eq: &Equipment, body: &Body, src: &DamageSource, mut dmg: f32, rng: &mut impl Rng) -> f32 {
        if src.is("bypasses_effects") {
            return dmg;
        }
        if let Some(r) = self.effect(Effect::Resistance) {
            if !src.is("bypasses_resistance") {
                let absorb_value = (r.amplifier + 1) * 5;
                let absorb = 25 - absorb_value;
                let v = dmg * absorb as f32;
                dmg = (v / 25.0).max(0.0);
            }
        }
        if dmg <= 0.0 {
            return 0.0;
        }
        if src.is("bypasses_enchantments") {
            return dmg;
        }
        let prot = damage_protection(eq, body, src, rng);
        if prot > 0.0 {
            dmg = magic_absorb(dmg, prot);
        }
        dmg
    }

    /// `doHurtEquipment`
    fn do_hurt_equipment(&mut self, eq: &mut Equipment, body: &Body, src: &DamageSource, damage: f32, slots: &[usize], rng: &mut impl Rng) {
        if damage <= 0.0 {
            return;
        }
        let infinite = body.player.is_some_and(|p| p.infinite_materials);
        for &slot in slots {
            let Some(s) = eq[slot] else { continue };
            let d = s.defaults();
            let damage_on_hurt = d.raw.get("equippable").map(|e| e["damage_on_hurt"].as_bool().unwrap_or(true)).unwrap_or(false);
            let resistant = d.damage_resistant.as_deref().is_some_and(|t| crate::components::damage_set_contains(t, src.ty));
            if damage_on_hurt && s.is_damageable() && !resistant {
                hurt_and_break(eq, slot, (damage / 4.0).max(1.0) as i32, infinite, &mut self.events, rng);
            }
        }
    }

    /// `LivingEntity.knockback`
    pub fn knockback(&mut self, body: &mut Body, power: f64, mut xd: f64, mut zd: f64, rng: &mut impl Rng) {
        let power = power * (1.0 - self.attrs.value(Attr::KnockbackResistance));
        if power <= 0.0 {
            return;
        }
        while xd * xd + zd * zd < 1.0e-5f32 as f64 {
            xd = (rng.r#gen::<f64>() - rng.r#gen::<f64>()) * 0.01;
            zd = (rng.r#gen::<f64>() - rng.r#gen::<f64>()) * 0.01;
        }
        let len = (xd * xd + zd * zd).sqrt();
        let (dx, dz) = if len < 1.0e-5 { (0.0, 0.0) } else { (xd / len * power, zd / len * power) };
        let v = body.velocity;
        body.velocity = [v[0] / 2.0 - dx, if body.info.on_ground { (v[1] / 2.0 + power).min(0.4) } else { v[1] }, v[2] / 2.0 - dz];
        self.events.push(Event::VelocityChanged);
    }

    /// `checkTotemDeathProtection`
    fn check_totem(&mut self, eq: &mut Equipment, body: &Body, src: &DamageSource, rng: &mut impl Rng) -> bool {
        if src.is("bypasses_invulnerability") {
            return false;
        }
        for hand in [MAINHAND, OFFHAND] {
            let Some(s) = eq[hand] else { continue };
            let Some(prot) = s.defaults().death_protection.clone() else { continue };
            eq[hand] = if s.count > 1 { Some(Stack { count: s.count - 1, ..s }) } else { None };
            self.events.push(Event::SlotChanged(hand));
            self.set_health(1.0);
            for eff in prot["death_effects"].as_array().into_iter().flatten() {
                match eff["type"].as_str().unwrap_or("").trim_start_matches("minecraft:") {
                    "clear_all_effects" => {
                        self.remove_all_effects();
                    }
                    "apply_effects" => {
                        let p = eff["probability"].as_f64().unwrap_or(1.0) as f32;
                        if rng.r#gen::<f32>() < p {
                            for e in eff["effects"].as_array().into_iter().flatten() {
                                if let Some(i) = effects::instance_from_json(e) {
                                    self.add_effect(&body.info, i);
                                }
                            }
                        }
                    }
                    "remove_effects" => {
                        let ids: Vec<Effect> = match &eff["effects"] {
                            serde_json::Value::String(s) => Effect::from_name(s).into_iter().collect(),
                            serde_json::Value::Array(a) => a.iter().filter_map(|x| x.as_str().and_then(Effect::from_name)).collect(),
                            _ => vec![],
                        };
                        for e in ids {
                            self.remove_effect(e);
                        }
                    }
                    _ => {}
                }
            }
            self.events.push(Event::Totem);
            return true;
        }
        false
    }

    /// `die`
    pub fn die(&mut self, _body: &Body, src: &DamageSource) {
        if self.dead {
            return;
        }
        self.using = None;
        self.dead = true;
        self.events.push(Event::Died(src.clone()));
    }

    /// `attackStrengthScale(a)`: 1/attack_speed*20 ticks to recharge.
    pub fn attack_strength(&mut self, a: f32) -> f32 {
        let delay = (1.0 / self.attrs.value(Attr::AttackSpeed) * 20.0) as f32;
        ((self.attack_ticker as f32 + a) / delay).clamp(0.0, 1.0)
    }
}

/// `Projectile.calculateHorizontalHurtKnockbackDirection`
pub fn horizontal_knockback_dir(motion: [f64; 3]) -> (f64, f64) {
    (motion[0], motion[2])
}

/// `Entity.calculateViewVector(xRot, yRot)`
pub fn view_vector(x_rot: f32, y_rot: f32) -> [f64; 3] {
    let xr = x_rot * (std::f32::consts::PI / 180.0);
    let yr = -y_rot * (std::f32::consts::PI / 180.0);
    let ycos = yr.cos();
    let ysin = yr.sin();
    let xcos = xr.cos();
    let xsin = xr.sin();
    [(ysin * xcos) as f64, (-xsin) as f64, (ycos * xcos) as f64]
}

/// `CombatRules.getDamageAfterAbsorb`
pub fn damage_after_absorb(damage: f32, total_armor: f32, armor_toughness: f32, src: &DamageSource, victim: &Body, rng: &mut impl Rng) -> f32 {
    let toughness = 2.0 + armor_toughness / 4.0;
    let real_armor = (total_armor - damage / toughness).clamp(total_armor * 0.2, 20.0);
    let armor_fraction = real_armor / 25.0;
    let blocked = match src.weapon {
        Some(w) => modify_armor_effectiveness(w, victim, src, armor_fraction, rng).clamp(0.0, 1.0),
        None => armor_fraction,
    };
    damage * (1.0 - blocked)
}

/// `CombatRules.getDamageAfterMagicAbsorb`
pub fn magic_absorb(damage: f32, protection: f32) -> f32 {
    let p = protection.clamp(0.0, 20.0);
    damage * (1.0 - p / 25.0)
}

pub fn is_broken(s: Stack) -> bool {
    s.is_damageable() && s.damage_value() >= s.max_damage()
}

/// Attribute modifiers an item gives in `slot` (`ItemStack.forEachModifier`).
pub fn item_modifiers(s: Stack, slot: usize) -> Vec<(Attr, Modifier)> {
    let name = SLOT_NAMES[slot];
    let mut out: Vec<(Attr, Modifier)> = s.defaults().modifiers.iter().filter(|m| enchant::slot_group_matches(&m.slot, name)).map(|m| (m.attr, m.modifier.clone())).collect();
    for (e, lvl) in enchant::on_item(s, Some(name)) {
        for a in &e.attributes {
            out.push((a.attribute, Modifier { id: format!("{}/{}", a.id, name), amount: a.amount.calc(lvl) as f64, op: a.op }));
        }
    }
    out
}

fn ctx<'a>(level: i32, this: &'a EntInfo, src: Option<&'a DamageSource>, tool: Option<Stack>, body: &Body) -> Ctx<'a> {
    Ctx {
        level,
        this: Some(this),
        attacker: src.and_then(|s| s.causing.as_ref()).map(|c| &c.info),
        direct: src.and_then(|s| s.direct.as_ref()).map(|c| &c.info),
        source: src,
        tool,
        raining: body.raining,
        thundering: body.thundering,
        active: true,
    }
}

/// `EnchantmentHelper.runIterationOnEquipment`
fn on_equipment(eq: &Equipment) -> Vec<(&'static enchant::Enchantment, i32, Stack)> {
    let mut out = Vec::new();
    for (slot, s) in eq.iter().enumerate() {
        if let Some(s) = s {
            for (e, l) in enchant::on_item(*s, Some(SLOT_NAMES[slot])) {
                out.push((e, l, *s));
            }
        }
    }
    out
}

/// `EnchantmentHelper.getDamageProtection`
pub fn damage_protection(eq: &Equipment, body: &Body, src: &DamageSource, rng: &mut impl Rng) -> f32 {
    let mut v = 0.0;
    for (e, l, s) in on_equipment(eq) {
        let c = ctx(l, &body.info, Some(src), Some(s), body);
        enchant::apply_values(&e.damage_protection, &c, rng, &mut v);
    }
    v
}

/// `EnchantmentHelper.isImmuneToDamage`
pub fn is_immune_to_damage(eq: &Equipment, body: &Body, src: &DamageSource, rng: &mut impl Rng) -> bool {
    for (e, l, s) in on_equipment(eq) {
        let c = ctx(l, &body.info, Some(src), Some(s), body);
        if e.damage_immunity.iter().any(|r| r.as_ref().is_none_or(|r| r.test(&c, rng))) {
            return true;
        }
    }
    false
}

fn modify_item_value(w: Stack, victim: &Body, src: &DamageSource, v: f32, rng: &mut impl Rng, f: impl Fn(&enchant::Enchantment) -> &Vec<enchant::Conditional<enchant::ValueEffect>>) -> f32 {
    let mut v = v;
    for (e, l) in enchant::on_item(w, None) {
        let c = ctx(l, &victim.info, Some(src), Some(w), victim);
        enchant::apply_values(f(e), &c, rng, &mut v);
    }
    v
}

/// `EnchantmentHelper.modifyArmorEffectiveness`
pub fn modify_armor_effectiveness(w: Stack, victim: &Body, src: &DamageSource, v: f32, rng: &mut impl Rng) -> f32 {
    modify_item_value(w, victim, src, v, rng, |e| &e.armor_effectiveness)
}
/// `EnchantmentHelper.modifyDamage`
pub fn modify_damage(w: Stack, victim: &Body, src: &DamageSource, v: f32, rng: &mut impl Rng) -> f32 {
    modify_item_value(w, victim, src, v, rng, |e| &e.damage)
}
/// `EnchantmentHelper.modifyKnockback`
pub fn modify_knockback(w: Stack, victim: &Body, src: &DamageSource, v: f32, rng: &mut impl Rng) -> f32 {
    modify_item_value(w, victim, src, v, rng, |e| &e.knockback)
}

/// `EnchantmentHelper.processDurabilityChange` (unbreaking)
pub fn process_durability_change(s: Stack, amount: i32, rng: &mut impl Rng) -> i32 {
    let mut v = amount as f32;
    for (e, l) in enchant::on_item(s, None) {
        let c = Ctx { level: l, this: None, attacker: None, direct: None, source: None, tool: Some(s), raining: false, thundering: false, active: true };
        enchant::apply_values(&e.item_damage, &c, rng, &mut v);
    }
    v as i32
}

/// `ItemStack.hurtAndBreak(amount, owner, slot)`
pub fn hurt_and_break(eq: &mut Equipment, slot: usize, amount: i32, infinite_materials: bool, events: &mut Vec<Event>, rng: &mut impl Rng) {
    let Some(s) = eq[slot] else { return };
    if !s.is_damageable() || infinite_materials {
        return;
    }
    let n = if amount > 0 { process_durability_change(s, amount, rng) } else { amount };
    if n == 0 {
        return;
    }
    let new_damage = s.damage_value() + n;
    let s = s.with_patch(|p| p.damage = Some(new_damage.max(0)));
    if is_broken(s) {
        eq[slot] = if s.count > 1 { Some(Stack { count: s.count - 1, ..s }) } else { None };
        events.push(Event::ItemBroken(slot, s.item));
    } else {
        eq[slot] = Some(s);
    }
    events.push(Event::SlotChanged(slot));
}

/// An entity effect from `post_attack` that the caller must apply.
#[derive(Clone, Debug)]
pub struct PostAttack {
    /// "attacker" / "damaging_entity" / "victim"
    pub affected: String,
    pub effect: enchant::EntityEffect,
    pub level: i32,
    /// which stack the enchantment is on (victim armour slot or attacker main hand)
    pub enchanted_victim: bool,
    pub slot: usize,
}

/// `EnchantmentHelper.doPostAttackEffectsWithItemSource`: collects the
/// post_attack effects whose requirements pass.
pub fn post_attack_effects(victim_eq: Option<&Equipment>, victim: &Body, src: &DamageSource, weapon: Option<Stack>, rng: &mut impl Rng) -> Vec<PostAttack> {
    let mut out = Vec::new();
    if let Some(eq) = victim_eq {
        for (slot, s) in eq.iter().enumerate() {
            let Some(s) = s else { continue };
            for (e, l) in enchant::on_item(*s, Some(SLOT_NAMES[slot])) {
                for t in e.post_attack.iter().filter(|t| t.enchanted == "victim") {
                    let c = ctx(l, &victim.info, Some(src), Some(*s), victim);
                    if t.requirements.as_ref().is_none_or(|r| r.test(&c, rng)) {
                        out.push(PostAttack { affected: t.affected.clone(), effect: t.effect.clone(), level: l, enchanted_victim: true, slot });
                    }
                }
            }
        }
    }
    if let (Some(w), true) = (weapon, src.causing.is_some_and(|c| c.living)) {
        for (e, l) in enchant::on_item(w, Some("mainhand")) {
            for t in e.post_attack.iter().filter(|t| t.enchanted == "attacker") {
                let c = ctx(l, &victim.info, Some(src), Some(w), victim);
                if t.requirements.as_ref().is_none_or(|r| r.test(&c, rng)) {
                    out.push(PostAttack { affected: t.affected.clone(), effect: t.effect.clone(), level: l, enchanted_victim: false, slot: MAINHAND });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(player: bool) -> Body {
        Body {
            eid: 1,
            info: EntInfo { type_name: if player { "player" } else { "zombie" }, on_ground: true, ..Default::default() },
            pos: [0.0, 64.0, 0.0],
            head_yaw: 0.0,
            velocity: [0.0; 3],
            player: player.then_some(PlayerSide { abilities_invulnerable: false, infinite_materials: false, difficulty: 2, rule_drowning: true, rule_fall: true, rule_fire: true, rule_freeze: true }),
            invulnerable: false,
            fire_immune: false,
            raining: false,
            thundering: false,
        }
    }

    #[test]
    fn invulnerability_frames_and_zero_damage() {
        let mut l = Living::player();
        let mut eq: Equipment = Default::default();
        let mut b = body(true);
        let mut rng = rand::thread_rng();
        assert!(l.hurt(&mut eq, &mut b, &DamageSource::new("generic"), 4.0, &mut rng));
        assert_eq!(l.health, 16.0);
        assert_eq!(l.invulnerable_time, 20);
        // weaker hit during the frames: ignored
        assert!(!l.hurt(&mut eq, &mut b, &DamageSource::new("generic"), 3.0, &mut rng));
        // stronger hit: only the difference
        assert!(l.hurt(&mut eq, &mut b, &DamageSource::new("generic"), 6.0, &mut rng));
        assert_eq!(l.health, 14.0);
        // zero damage to a player is rejected before anything happens
        for _ in 0..20 {
            l.tick_counters();
        }
        assert!(!l.hurt(&mut eq, &mut b, &DamageSource::new("generic"), 0.0, &mut rng));
        // ... but a mob takes the "hit" (frames + knockback) with 0 damage
        let mut m = Living::new(attr::Map::new(&attr::living()));
        let mut mb = body(false);
        assert!(m.hurt(&mut eq, &mut mb, &DamageSource::new("generic"), 0.0, &mut rng));
        assert_eq!(m.invulnerable_time, 20);
    }

    #[test]
    fn armor_formula() {
        let mut rng = rand::thread_rng();
        let b = body(true);
        let s = DamageSource::new("generic");
        // full diamond (20 armour, 8 toughness) vs 10 damage
        let d = damage_after_absorb(10.0, 20.0, 8.0, &s, &b, &mut rng);
        let t = 2.0f32 + 8.0 / 4.0;
        let r = (20.0f32 - 10.0 / t).clamp(4.0, 20.0) / 25.0;
        assert_eq!(d, 10.0 * (1.0 - r));
        assert_eq!(magic_absorb(10.0, 16.0), 10.0 * (1.0 - 16.0 / 25.0));
    }

    #[test]
    fn knockback_vanilla() {
        let mut l = Living::new(attr::Map::new(&attr::living()));
        let mut b = body(false);
        b.velocity = [0.2, 0.0, 0.0];
        let mut rng = rand::thread_rng();
        l.knockback(&mut b, 0.4, 1.0, 0.0, &mut rng);
        assert_eq!(b.velocity, [0.1 - 0.4, 0.4, 0.0]);
    }

    #[test]
    fn resistance_and_absorption() {
        let mut l = Living::player();
        let mut eq: Equipment = Default::default();
        let mut b = body(true);
        let mut rng = rand::thread_rng();
        l.add_effect(&b.info, Instance::new(Effect::Resistance, 100, 1));
        l.add_effect(&b.info, Instance::new(Effect::Absorption, 100, 0));
        assert_eq!(l.absorption, 4.0);
        l.hurt(&mut eq, &mut b, &DamageSource::new("generic"), 10.0, &mut rng);
        // 10 * 15/25 = 6, absorption takes 4
        assert_eq!(l.absorption, 0.0);
        assert_eq!(l.health, 18.0);
    }
}
