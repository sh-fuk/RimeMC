//! Glue between the exact combat rules (`game::combat`) and the world:
//! hurting players and mobs, knockback, `Player.attack`, `Mob.doHurtTarget`,
//! post-attack enchantment effects and the packets all of that produces.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::Rng;

use super::combat::attr::Attr;
use super::combat::damage::{DamageSource, EntityRef};
use super::combat::effects::{Effect, Instance};
use super::combat::enchant::{EntInfo, EntityEffect};
use super::combat::living::{self, Body, Equipment, Event, Living, PlayerSide};
use super::entity::EKind;
use super::etypes::EntityTypes;
use super::{mob, Game, Player, Request};
use crate::data::Stack;
use crate::net::codec::Out;
use crate::registry::Registries;

/// Player flag bits (`Player::flags`).
pub const F_SNEAKING: u8 = 1;
pub const F_SPRINTING: u8 = 2;
pub const F_FALL_FLYING: u8 = 4;
pub const F_SWIMMING: u8 = 8;
pub const F_CLIMBING: u8 = 16;
pub const F_IN_WATER: u8 = 32;
pub const F_PASSENGER: u8 = 64;

/// `EquipmentSlot` → entity event of the break animation.
fn break_event(slot: usize) -> u8 {
    match slot {
        living::MAINHAND => 47,
        living::OFFHAND => 48,
        living::HEAD => 49,
        living::CHEST => 50,
        living::LEGS => 51,
        living::FEET => 52,
        living::BODY => 65,
        _ => 68,
    }
}

/// Window slot of a player's equipment slot.
pub fn player_slot(slot: usize, held: i32) -> Option<usize> {
    match slot {
        living::MAINHAND => Some(36 + held as usize),
        living::OFFHAND => Some(45),
        living::FEET => Some(8),
        living::LEGS => Some(7),
        living::CHEST => Some(6),
        living::HEAD => Some(5),
        _ => None,
    }
}

impl Player {
    /// Equipment by `EquipmentSlot` from the inventory.
    pub fn equipment(&self) -> Equipment {
        let inv = self.inv.lock();
        let held = self.held.load(Ordering::Relaxed);
        let mut eq: Equipment = Default::default();
        for (slot, e) in eq.iter_mut().enumerate() {
            if let Some(w) = player_slot(slot, held) {
                *e = inv.slots[w];
            }
        }
        eq
    }

    fn write_equipment(&self, before: &Equipment, after: &Equipment) {
        let held = self.held.load(Ordering::Relaxed);
        let mut inv = self.inv.lock();
        for slot in 0..8 {
            if before[slot] != after[slot] {
                if let Some(w) = player_slot(slot, held) {
                    inv.set(w, after[slot]);
                }
            }
        }
    }

    pub fn info(&self, v: &super::survival::Vitals) -> EntInfo {
        let f = self.flags.load(Ordering::Relaxed);
        EntInfo {
            type_name: "player",
            on_fire: v.fire > 0,
            sneaking: f & F_SNEAKING != 0,
            sprinting: f & F_SPRINTING != 0,
            swimming: f & F_SWIMMING != 0,
            flying: false,
            fall_flying: f & F_FALL_FLYING != 0,
            baby: false,
            on_ground: self.pose.lock().on_ground,
            in_vehicle: f & F_PASSENGER != 0,
            fall_distance: v.fall_distance,
        }
    }
}

/// Angle from the attacker's yaw used by `causeExtraKnockback`.
fn yaw_dir(yaw: f32) -> (f64, f64) {
    let r = yaw * (std::f32::consts::PI / 180.0);
    (r.sin() as f64, -(r.cos() as f64))
}

/// `Mth.randomBetween`
fn random_between(rng: &mut impl Rng, min: f32, max: f32) -> f32 {
    min + rng.r#gen::<f32>() * (max - min)
}

impl Game {
    pub fn difficulty(&self) -> i32 {
        2
    }

    fn player_side(&self, p: &Player) -> PlayerSide {
        let gm = p.gamemode.load(Ordering::Relaxed);
        PlayerSide {
            abilities_invulnerable: gm == 1 || gm == 3,
            infinite_materials: gm == 1,
            difficulty: self.difficulty(),
            rule_drowning: true,
            rule_fall: true,
            rule_fire: true,
            rule_freeze: true,
        }
    }

    /// `EntityRef` (for damage sources) of any entity.
    pub fn entity_ref(&self, eid: i32) -> Option<EntityRef> {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            let v = p.vit.lock();
            let info = p.info(&v);
            return Some(EntityRef { eid, player: true, living: true, pos: p.pose.lock().pos, info });
        }
        let ents = self.entities.lock();
        let e = ents.get(&eid)?;
        Some(match &e.kind {
            EKind::Mob(m) => EntityRef { eid, player: false, living: true, pos: e.pos, info: mob_info(m, e.on_ground) },
            EKind::Projectile(p) => EntityRef { eid, player: false, living: false, pos: e.pos, info: EntInfo { type_name: p.ty.entity_name(), ..Default::default() } },
            EKind::Cloud(_) => EntityRef { eid, player: false, living: false, pos: e.pos, info: EntInfo { type_name: "area_effect_cloud", ..Default::default() } },
            EKind::Item { .. } => EntityRef { eid, player: false, living: false, pos: e.pos, info: EntInfo { type_name: "item", ..Default::default() } },
            EKind::Falling { .. } => EntityRef { eid, player: false, living: false, pos: e.pos, info: EntInfo { type_name: "falling_block", ..Default::default() } },
            EKind::Lightning { .. } => EntityRef { eid, player: false, living: false, pos: e.pos, info: EntInfo { type_name: "lightning_bolt", ..Default::default() } },
            EKind::Fangs { .. } => EntityRef { eid, player: false, living: false, pos: e.pos, info: EntInfo { type_name: "evoker_fangs", ..Default::default() } },
            EKind::Tnt { .. } => EntityRef { eid, player: false, living: false, pos: e.pos, info: EntInfo { type_name: "tnt", ..Default::default() } },
            EKind::Frame { glow, .. } => EntityRef { eid, player: false, living: false, pos: e.pos, info: EntInfo { type_name: if *glow { "glow_item_frame" } else { "item_frame" }, ..Default::default() } },
            EKind::Crystal { .. } => EntityRef { eid, player: false, living: false, pos: e.pos, info: EntInfo { type_name: "end_crystal", ..Default::default() } },
        })
    }

    /// Runs `f` on a player's `Living` with its equipment and body, then
    /// writes equipment and motion back and handles the events.
    pub fn with_player<R>(&self, p: &Arc<Player>, f: impl FnOnce(&mut Living, &mut Equipment, &mut Body) -> R) -> R {
        let before = p.equipment();
        let mut eq = before;
        let side = self.player_side(p);
        let (r, events, velocity_before, velocity) = {
            let mut v = p.vit.lock();
            let info = p.info(&v);
            let pose = *p.pose.lock();
            let vel = *p.motion.lock();
            let mut body = Body { eid: p.eid, info, pos: pose.pos, head_yaw: pose.yaw, velocity: vel, player: Some(side), invulnerable: false, fire_immune: false, raining: self.raining(), thundering: self.thundering() };
            let r = f(&mut v.living, &mut eq, &mut body);
            v.living.update_equipment(&eq);
            let events = std::mem::take(&mut v.living.events);
            (r, events, vel, body.velocity)
        };
        if eq != before {
            p.write_equipment(&before, &eq);
        }
        if velocity != velocity_before {
            *p.motion.lock() = velocity;
        }
        self.player_events(p, events);
        r
    }

    /// `hurtServer` on a player.
    pub fn hurt_player(&self, p: &Arc<Player>, src: &DamageSource, amount: f32) -> bool {
        self.with_player(p, |l, eq, body| l.hurt(eq, body, src, amount, &mut rand::thread_rng()))
    }

    /// `hurtServer` on a mob.
    pub fn hurt_mob(&self, eid: i32, src: &DamageSource, amount: f32) -> bool {
        if self.with_dragon(eid, |_| ()).is_some() {
            // anything but a direct hit lands on the body part
            return self.hurt_dragon(eid, 3, src, amount);
        }
        self.with_mob(eid, |l, eq, body| l.hurt(eq, body, src, amount, &mut rand::thread_rng())).unwrap_or(false)
    }

    /// `hurtServer` on any living entity.
    pub fn hurt_entity(&self, eid: i32, src: &DamageSource, amount: f32) -> bool {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return self.hurt_player(&p, src, amount);
        }
        self.hurt_mob(eid, src, amount)
    }

    /// Runs `f` on a mob's `Living`; None when there is no such mob.
    pub fn with_mob<R>(&self, eid: i32, f: impl FnOnce(&mut Living, &mut Equipment, &mut Body) -> R) -> Option<R> {
        let (r, events, chunk, pos) = {
            let mut ents = self.entities.lock();
            let e = ents.get_mut(&eid)?;
            let chunk = e.chunk();
            let (pos, on_ground, vel) = (e.pos, e.on_ground, e.vel);
            let EKind::Mob(m) = &mut e.kind else { return None };
            if m.death > 0 {
                return None;
            }
            let info = mob_info(m, on_ground);
            let t = EntityTypes::get().by_name(m.ty.name());
            let mut body = Body {
                eid,
                info,
                pos,
                head_yaw: m.mo.head_yaw,
                velocity: vel,
                player: None,
                invulnerable: false,
                fire_immune: t.is_some_and(|t| t.fire_immune),
                raining: self.raining(),
                thundering: self.thundering(),
            };
            let mut eq = m.eq;
            let r = f(&mut m.living, &mut eq, &mut body);
            m.living.update_equipment(&eq);
            m.eq = eq;
            let events = std::mem::take(&mut m.living.events);
            e.vel = body.velocity;
            (r, events, chunk, pos)
        };
        self.mob_events(eid, chunk, pos, events);
        Some(r)
    }

    /// `LivingEntity.knockback` on any entity (shield bash, attacks, …).
    pub fn knockback_entity(&self, eid: i32, power: f64, xd: f64, zd: f64) {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            self.with_player(&p, |l, _, body| l.knockback(body, power, xd, zd, &mut rand::thread_rng()));
            return;
        }
        self.with_mob(eid, |l, _, body| l.knockback(body, power, xd, zd, &mut rand::thread_rng()));
    }

    // ------------------------------------------------------------ events

    fn damage_event_packet(&self, eid: i32, ty: &str, causing: Option<i32>, direct: Option<i32>, pos: Option<[f64; 3]>) -> Arc<Vec<u8>> {
        let id = crate::net::config_data::dynamic_id("minecraft:damage_type", ty).unwrap_or(0);
        let mut b = Vec::new();
        b.vi(eid).vi(id).vi(causing.map(|c| c + 1).unwrap_or(0)).vi(direct.map(|c| c + 1).unwrap_or(0));
        match pos {
            Some(p) => {
                b.bool(true).f64(p[0]).f64(p[1]).f64(p[2]);
            }
            None => {
                b.bool(false);
            }
        }
        self.encode("damage_event", &b)
    }

    pub(crate) fn entity_event(&self, eid: i32, ev: u8) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.i32(eid).u8(ev);
        self.encode("entity_event", &b)
    }

    fn motion_packet(&self, eid: i32, v: [f64; 3]) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid);
        mob::write_lpvec(&mut b, v);
        self.encode("set_entity_motion", &b)
    }

    /// `update_attributes` with every synced attribute of a `Living`.
    pub fn attributes_packet(&self, eid: i32, l: &mut Living) -> Arc<Vec<u8>> {
        let synced: Vec<Attr> = l.attrs.iter().map(|(a, _)| a).filter(|a| a.synced()).collect();
        let mut b = Vec::new();
        b.vi(eid).vi(synced.len() as i32);
        for a in synced {
            let inst = l.attrs.get(a).expect("attribute");
            b.vi(a.id()).f64(inst.base).vi(inst.modifiers.len() as i32);
            for m in &inst.modifiers {
                b.str(&m.id).f64(m.amount).u8(m.op.id() as u8);
            }
        }
        l.attrs.dirty.clear();
        self.encode("update_attributes", &b)
    }

    /// Plays a sound at a position for everyone in range.
    pub fn play_sound(&self, name: &str, category: i32, pos: [f64; 3], volume: f32, pitch: f32) {
        let Some(id) = Registries::get().id("minecraft:sound_event", name) else { return };
        let mut b = Vec::new();
        b.vi(id + 1).vi(category);
        b.i32((pos[0] * 8.0) as i32).i32((pos[1] * 8.0) as i32).i32((pos[2] * 8.0) as i32);
        b.f32(volume).f32(pitch).i64(rand::thread_rng().r#gen());
        let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
        self.broadcast_chunk(c, &self.encode("sound", &b));
    }

    /// Particles at a position (`sendParticles`).
    #[allow(clippy::too_many_arguments)]
    pub fn particles(&self, name: &str, pos: [f64; 3], count: i32, d: [f32; 3], speed: f32) {
        let Some(id) = Registries::get().id("minecraft:particle_type", name) else { return };
        let mut b = Vec::new();
        b.bool(false).bool(false).f64(pos[0]).f64(pos[1]).f64(pos[2]).f32(d[0]).f32(d[1]).f32(d[2]).f32(speed).i32(count).vi(id);
        let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
        self.broadcast_chunk(c, &self.encode("level_particles", &b));
    }

    fn to_viewers_of(&self, p: &Player, pkt: Arc<Vec<u8>>, me: bool) {
        let pos = p.pose.lock().pos;
        let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
        for q in self.players() {
            if q.eid == p.eid {
                if me {
                    q.send(pkt.clone());
                }
            } else if q.sees_chunk(c.0, c.1) {
                q.send(pkt.clone());
            }
        }
    }

    fn player_events(&self, p: &Arc<Player>, events: Vec<Event>) {
        let mut inv_changed = false;
        for ev in events {
            match ev {
                Event::Damage { ty, causing, direct, pos } => {
                    let pkt = self.damage_event_packet(p.eid, ty, causing, direct, pos);
                    self.to_viewers_of(p, pkt, true);
                }
                Event::Blocked => {
                    let pos = p.pose.lock().pos;
                    self.play_sound("minecraft:item.shield.block", 7, pos, 1.0, 0.8 + rand::thread_rng().r#gen::<f32>() * 0.4);
                }
                Event::VelocityChanged => {
                    let v = *p.motion.lock();
                    p.send(self.motion_packet(p.eid, v));
                    p.knocked.store(true, Ordering::Relaxed);
                }
                Event::KnockAttacker { eid, power, xd, zd } => self.knockback_entity(eid, power, xd, zd),
                Event::ShieldDisabled { item, ticks } => {
                    let name = crate::data::Items::get().name(item);
                    let mut b = Vec::new();
                    b.str(&format!("minecraft:{name}")).vi(ticks);
                    p.send(self.encode("cooldown", &b));
                    p.requests.lock().push(Request::StopUsingItem);
                    let pos = p.pose.lock().pos;
                    self.play_sound("minecraft:item.shield.break", 7, pos, 0.8, 0.8 + rand::thread_rng().r#gen::<f32>() * 0.4);
                }
                Event::SlotChanged(_) => inv_changed = true,
                Event::ItemBroken(slot, item) => {
                    let pkt = self.entity_event(p.eid, break_event(slot));
                    self.to_viewers_of(p, pkt, true);
                    let _ = item;
                    inv_changed = true;
                }
                Event::Totem => {
                    let pkt = self.entity_event(p.eid, 35);
                    self.to_viewers_of(p, pkt, true);
                    inv_changed = true;
                }
                Event::HurtSound => {}
                Event::Died(src) => p.requests.lock().push(Request::Died(Box::new(src))),
                Event::Exhaustion(x) => {
                    if !self.player_side(p).abilities_invulnerable {
                        p.vit.lock().add_exhaustion(x);
                    }
                }
                Event::Eat(n, s) => p.vit.lock().eat(n, s),
                Event::EffectAdded(e) | Event::EffectUpdated(e) => {
                    let inst = p.vit.lock().living.effect(e).cloned();
                    if let Some(i) = inst {
                        let mut b = Vec::new();
                        b.vi(p.eid);
                        i.write(&mut b);
                        p.send(self.encode("update_mob_effect", &b));
                    }
                    p.vit.lock().data_dirty = true;
                }
                Event::EffectRemoved(e) => {
                    let mut b = Vec::new();
                    b.vi(p.eid).vi(e.id());
                    p.send(self.encode("remove_mob_effect", &b));
                    p.vit.lock().data_dirty = true;
                }
                Event::AttributesChanged => {
                    let pkt = self.attributes_packet(p.eid, &mut p.vit.lock().living);
                    self.to_viewers_of(p, pkt, true);
                }
                Event::HealthChanged => p.vit.lock().dirty = true,
            }
        }
        if inv_changed {
            self.flush_inventory(p);
        }
    }

    pub(super) fn mob_events(&self, eid: i32, chunk: (i32, i32), pos: [f64; 3], events: Vec<Event>) {
        let mut loot = None;
        for ev in events {
            match ev {
                Event::Damage { ty, causing, direct, pos } => {
                    let pkt = self.damage_event_packet(eid, ty, causing, direct, pos);
                    self.broadcast_chunk(chunk, &pkt);
                    // HurtByTargetGoal / PanicGoal read `last_hurt_by_mob`
                    let _ = causing;
                }
                Event::Blocked => self.play_sound("minecraft:item.shield.block", 5, pos, 1.0, 1.0),
                Event::VelocityChanged => {
                    let v = self.entities.lock().get(&eid).map(|e| e.vel);
                    if let Some(v) = v {
                        self.broadcast_chunk(chunk, &self.motion_packet(eid, v));
                    }
                }
                Event::KnockAttacker { eid: a, power, xd, zd } => self.knockback_entity(a, power, xd, zd),
                Event::HurtSound => {
                    let snd = {
                        let ents = self.entities.lock();
                        match ents.get(&eid).map(|e| &e.kind) {
                            Some(EKind::Mob(m)) if m.death == 0 => Some((m.ty.hurt_sound(), m.ty.hostile(), m.is_baby())),
                            _ => None,
                        }
                    };
                    if let Some((name, hostile, baby)) = snd {
                        let mut rng = rand::thread_rng();
                        let pitch = (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.2 + if baby { 1.5 } else { 1.0 };
                        self.play_sound(&name, if hostile { 5 } else { 6 }, pos, 1.0, pitch);
                    }
                }
                Event::ShieldDisabled { .. } | Event::Exhaustion(_) | Event::Eat(..) | Event::HealthChanged => {}
                Event::SlotChanged(_) => {}
                Event::ItemBroken(slot, _) => self.broadcast_chunk(chunk, &self.entity_event(eid, break_event(slot))),
                Event::Totem => self.broadcast_chunk(chunk, &self.entity_event(eid, 35)),
                Event::Died(src) => {
                    let mut ents = self.entities.lock();
                    if let Some(e) = ents.get_mut(&eid) {
                        if let EKind::Mob(m) = &mut e.kind {
                            if m.death == 0 {
                                m.death = 1;
                                loot = Some(src);
                                let meta = self.mob_meta(eid, m);
                                drop(ents);
                                self.broadcast_chunk(chunk, &self.entity_event(eid, 3));
                                self.broadcast_chunk(chunk, &meta);
                            }
                        }
                    }
                }
                Event::EffectAdded(e) | Event::EffectUpdated(e) => {
                    let _ = e;
                }
                Event::EffectRemoved(_) => {}
                Event::AttributesChanged => {
                    let pkt = {
                        let mut ents = self.entities.lock();
                        match ents.get_mut(&eid).map(|e| &mut e.kind) {
                            Some(EKind::Mob(m)) => Some(self.attributes_packet(eid, &mut m.living)),
                            _ => None,
                        }
                    };
                    if let Some(pkt) = pkt {
                        self.broadcast_chunk(chunk, &pkt);
                    }
                }
            }
        }
        if let Some(src) = loot {
            self.mob_died(eid, &src);
        }
    }

    // ------------------------------------------------------------ attacks

    /// `Player.attack(entity)`.
    pub fn player_attack(&self, attacker: &Arc<Player>, target: i32) -> bool {
        // EnderDragonPart: the hit goes to the dragon
        let (target, part) = match self.entity_ref(target) {
            Some(_) => (target, 0),
            None => match self.dragon_part(target) {
                Some((d, k, _)) => (d, k),
                None => return false,
            },
        };
        let Some(tref) = self.entity_ref(target) else { return false };
        if matches!(tref.info.type_name, "item_frame" | "glow_item_frame") {
            return self.hit_frame(Some(attacker), target);
        }
        if tref.info.type_name == "end_crystal" {
            return self.destroy_crystal(target, Some(attacker.eid), true);
        }
        if !tref.living {
            return false;
        }
        let mut rng = rand::thread_rng();
        let flags = attacker.flags.load(Ordering::Relaxed);
        let pose = *attacker.pose.lock();
        let weapon = attacker.equipment()[living::MAINHAND];
        let me = self.entity_ref(attacker.eid).expect("attacker");
        // a body for the target, as the enchantment conditions see it
        let tbody = Body { eid: target, info: tref.info, pos: tref.pos, head_yaw: 0.0, velocity: [0.0; 3], player: None, invulnerable: false, fire_immune: false, raining: self.raining(), thundering: self.thundering() };
        let (base, scale, fall_distance, known_speed_sq, speed) = {
            let mut v = attacker.vit.lock();
            let base = v.living.attrs.value(Attr::AttackDamage) as f32;
            let scale = v.living.attack_strength(0.5);
            let fd = v.fall_distance;
            let speed = v.living.attrs.value(Attr::MovementSpeed);
            // onAttack: resetOnlyAttackStrengthTicker
            v.living.attack_ticker = 0;
            (base, scale, fd, *attacker.known_speed_sq.lock(), speed)
        };
        let smash = weapon.is_some_and(|w| crate::data::Items::get().name(w.item) == "mace") && fall_distance > 1.5 && flags & F_FALL_FLYING == 0;
        let mut src = DamageSource::by(if smash { "mace" } else { "player_attack" }, me);
        src.weapon = weapon;
        src.creative_player = attacker.gamemode.load(Ordering::Relaxed) == 1;
        let enchanted = |dmg: f32, body: &Body, rng: &mut rand::rngs::ThreadRng| match weapon {
            Some(w) => living::modify_damage(w, body, &src, dmg, rng),
            None => dmg,
        };
        let magic_boost = scale * (enchanted(base, &tbody, &mut rng) - base);
        let mut base = base * (0.2 + scale * scale * 0.8);
        if !(base > 0.0 || magic_boost > 0.0) {
            return true;
        }
        let full = scale > 0.9;
        let sprinting = flags & F_SPRINTING != 0;
        let knockback_attack = sprinting && full;
        if knockback_attack {
            self.play_sound("minecraft:entity.player.attack.knockback", 7, pose.pos, 1.0, 1.0);
        }
        // MaceItem.getAttackDamageBonus
        if smash {
            let fd = fall_distance;
            let d = if fd <= 3.0 {
                4.0 * fd
            } else if fd <= 8.0 {
                12.0 + 2.0 * (fd - 3.0)
            } else {
                22.0 + fd - 8.0
            };
            let mut extra = 0.0f32;
            if let Some(w) = weapon {
                for (e, l) in super::combat::enchant::on_item(w, None) {
                    let c = super::combat::enchant::Ctx { level: l, this: Some(&tbody.info), attacker: Some(&me.info), direct: Some(&me.info), source: Some(&src), tool: Some(w), raining: false, thundering: false, active: true };
                    super::combat::enchant::apply_values(&e.smash_damage_per_fallen_block, &c, &mut rng, &mut extra);
                }
            }
            base += (d + extra as f64 * fd) as f32;
        }
        let crit = full && fall_distance > 0.0 && !pose.on_ground && flags & (F_CLIMBING | F_IN_WATER | F_PASSENGER | F_SPRINTING) == 0 && !attacker.vit.lock().living.has_effect(Effect::Blindness);
        if crit {
            base *= 1.5;
        }
        let total = base + magic_boost;
        let is_sword = weapon.is_some_and(|w| self.data.item_in("swords", w.item));
        let sweep = full && !crit && !knockback_attack && pose.on_ground && known_speed_sq < (speed * 2.5) * (speed * 2.5) && is_sword;
        let old_health = self.health_of(target);
        let target_player = self.players().into_iter().find(|p| p.eid == target);
        let old_motion = target_player.as_ref().map(|t| *t.motion.lock());
        let was_hurt = if part > 0 { self.hurt_dragon(target, part, &src, total) } else { self.hurt_entity(target, &src, total) };
        if !was_hurt {
            self.play_sound("minecraft:entity.player.attack.nodamage", 7, pose.pos, 1.0, 1.0);
            return true;
        }
        // causeExtraKnockback
        let kb_attr = attacker.vit.lock().living.attrs.value(Attr::AttackKnockback) as f32;
        let kb = match weapon {
            Some(w) => living::modify_knockback(w, &tbody, &src, kb_attr, &mut rng),
            None => kb_attr,
        } / 2.0
            + if knockback_attack { 0.5 } else { 0.0 };
        if kb > 0.0 {
            let (sx, cz) = yaw_dir(pose.yaw);
            self.knockback_entity(target, kb as f64, sx, cz);
            {
                let mut m = attacker.motion.lock();
                m[0] *= 0.6;
                m[2] *= 0.6;
            }
            attacker.flags.fetch_and(!F_SPRINTING, Ordering::Relaxed);
        }
        if let (Some(t), Some(old)) = (&target_player, old_motion) {
            *t.motion.lock() = old;
        }
        if sweep {
            self.play_sound("minecraft:entity.player.attack.sweep", 7, pose.pos, 1.0, 1.0);
            let ratio = attacker.vit.lock().living.attrs.value(Attr::SweepingDamageRatio) as f32;
            let sweep_dmg = 1.0 + ratio * base;
            if let Some(tb) = self.entity_box(target) {
                let area = tb.inflate(1.0, 0.25, 1.0);
                for (eid, pos) in self.living_in(&area) {
                    if eid == attacker.eid || eid == target {
                        continue;
                    }
                    let d2 = (pos[0] - pose.pos[0]).powi(2) + (pos[1] - pose.pos[1]).powi(2) + (pos[2] - pose.pos[2]).powi(2);
                    if d2 >= 9.0 {
                        continue;
                    }
                    let Some(r) = self.entity_ref(eid) else { continue };
                    let nb = Body { eid, info: r.info, pos: r.pos, head_yaw: 0.0, velocity: [0.0; 3], player: None, invulnerable: false, fire_immune: false, raining: false, thundering: false };
                    let dmg = enchanted(sweep_dmg, &nb, &mut rng) * scale;
                    if self.hurt_entity(eid, &src, dmg) {
                        let (sx, cz) = yaw_dir(pose.yaw);
                        self.knockback_entity(eid, 0.4, sx, cz);
                        self.post_attack(eid, &src, None);
                    }
                }
            }
            let (sx, cz) = yaw_dir(pose.yaw);
            self.particles("minecraft:sweep_attack", [pose.pos[0] - sx, pose.pos[1] + 0.9, pose.pos[2] - cz], 0, [-sx as f32, 0.0, -cz as f32], 0.0);
        }
        // attackVisualEffects
        if crit {
            self.play_sound("minecraft:entity.player.attack.crit", 7, pose.pos, 1.0, 1.0);
            self.animate(target, 4);
        }
        if !crit && !sweep {
            self.play_sound(if full { "minecraft:entity.player.attack.strong" } else { "minecraft:entity.player.attack.weak" }, 7, pose.pos, 1.0, 1.0);
        }
        if magic_boost > 0.0 {
            self.animate(target, 5);
        }
        // itemAttackInteraction: weapon durability, mace smash, post-attack enchantments
        if let Some(w) = weapon {
            if let Some((per_attack, _)) = w.defaults().weapon {
                let infinite = attacker.gamemode.load(Ordering::Relaxed) == 1;
                self.with_player(attacker, |_, eq, _| {
                    let mut evs = Vec::new();
                    living::hurt_and_break(eq, living::MAINHAND, per_attack, infinite, &mut evs, &mut rand::thread_rng());
                    evs
                });
            }
            if smash {
                {
                    let mut m = attacker.motion.lock();
                    m[1] = 0.01f32 as f64;
                }
                let v = *attacker.motion.lock();
                attacker.send(self.motion_packet(attacker.eid, v));
                attacker.vit.lock().fall_distance = 0.0;
                let ground = tref.info.on_ground;
                let sound = if ground {
                    if fall_distance > 5.0 {
                        "minecraft:item.mace.smash_ground_heavy"
                    } else {
                        "minecraft:item.mace.smash_ground"
                    }
                } else {
                    "minecraft:item.mace.smash_air"
                };
                self.play_sound(sound, 7, pose.pos, 1.0, 1.0);
                self.mace_knockback(attacker, target, fall_distance);
            }
        }
        self.post_attack(target, &src, weapon);
        // damageStatsAndHearts
        if let (Some(old), Some(new)) = (old_health, self.health_of(target)) {
            let actual = old - new;
            if actual > 2.0 {
                let count = (actual * 0.5) as i32;
                self.particles("minecraft:damage_indicator", [tref.pos[0], tref.pos[1] + 0.9, tref.pos[2]], count, [0.1, 0.0, 0.1], 0.2);
            }
        }
        if attacker.gamemode.load(Ordering::Relaxed) != 1 {
            attacker.vit.lock().add_exhaustion(0.1);
        }
        true
    }

    /// `MaceItem.knockback`: pushes entities near the smashed one.
    fn mace_knockback(&self, attacker: &Arc<Player>, target: i32, fall_distance: f64) {
        let Some(t) = self.entity_ref(target) else { return };
        let area = crate::anticheat::combat::Aabb::entity(t.pos, 7.0, 7.0);
        let apos = attacker.pose.lock().pos;
        for (eid, pos) in self.living_in(&area) {
            if eid == attacker.eid || eid == target {
                continue;
            }
            let d2 = (pos[0] - t.pos[0]).powi(2) + (pos[1] - t.pos[1]).powi(2) + (pos[2] - t.pos[2]).powi(2);
            if d2 > 3.5 * 3.5 {
                continue;
            }
            let v = [pos[0] - t.pos[0], pos[1] - t.pos[1], pos[2] - t.pos[2]];
            let dist = (d2).sqrt();
            let power = (3.5 - dist) * 0.7 * if fall_distance > 5.0 { 2.0 } else { 1.0 };
            let kr = self.with_player_or_mob_attr(eid, Attr::KnockbackResistance).unwrap_or(0.0);
            let power = power * (1.0 - kr);
            let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
            let push = [v[0] / len * power, 0.7f32 as f64 * power.min(1.0), v[2] / len * power];
            if power > 0.0 {
                self.push_entity(eid, push);
            }
            let _ = apos;
        }
    }

    fn with_player_or_mob_attr(&self, eid: i32, a: Attr) -> Option<f64> {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return Some(p.vit.lock().living.attrs.value(a));
        }
        let mut ents = self.entities.lock();
        match ents.get_mut(&eid).map(|e| &mut e.kind) {
            Some(EKind::Mob(m)) => Some(m.living.attrs.value(a)),
            _ => None,
        }
    }

    /// `Entity.push(x, y, z)` (+ velocity sync).
    pub fn push_entity(&self, eid: i32, d: [f64; 3]) {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            let v = {
                let mut m = p.motion.lock();
                m[0] += d[0];
                m[1] += d[1];
                m[2] += d[2];
                *m
            };
            p.send(self.motion_packet(eid, v));
            return;
        }
        let pkt = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return };
            e.vel = [e.vel[0] + d[0], e.vel[1] + d[1], e.vel[2] + d[2]];
            (e.chunk(), self.motion_packet(eid, e.vel))
        };
        self.broadcast_chunk(pkt.0, &pkt.1);
    }

    fn animate(&self, eid: i32, action: u8) {
        let mut b = Vec::new();
        b.vi(eid).u8(action);
        let pkt = self.encode("animate", &b);
        if let Some(r) = self.entity_ref(eid) {
            let c = ((r.pos[0].floor() as i32) >> 4, (r.pos[2].floor() as i32) >> 4);
            self.broadcast_chunk(c, &pkt);
        }
    }

    pub fn health_of(&self, eid: i32) -> Option<f32> {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return Some(p.vit.lock().living.health);
        }
        match self.entities.lock().get(&eid).map(|e| &e.kind) {
            Some(EKind::Mob(m)) => Some(m.living.health),
            _ => None,
        }
    }

    /// Living entities (players included) whose box intersects `area`.
    pub fn living_in(&self, area: &crate::anticheat::combat::Aabb) -> Vec<(i32, [f64; 3])> {
        let mut out = Vec::new();
        for p in self.players() {
            let gm = p.gamemode.load(Ordering::Relaxed);
            if gm == 3 || p.vit.lock().dead() {
                continue;
            }
            let pos = p.pose.lock().pos;
            if crate::anticheat::combat::Aabb::entity(pos, 0.6, 1.8).intersects(area) {
                out.push((p.eid, pos));
            }
        }
        for e in self.entities.lock().values() {
            if let EKind::Mob(m) = &e.kind {
                if m.death > 0 {
                    continue;
                }
                let (half, h) = e.size();
                if crate::anticheat::combat::Aabb::entity(e.pos, half * 2.0, h).intersects(area) {
                    out.push((e.eid, e.pos));
                }
            }
        }
        out
    }

    /// `Mob.doHurtTarget`
    pub fn mob_attack(&self, mob_eid: i32, target: i32) -> bool {
        let Some(me) = self.entity_ref(mob_eid) else { return false };
        let Some(tref) = self.entity_ref(target) else { return false };
        let (dmg, weapon, yaw, kb_attr) = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&mob_eid) else { return false };
            let EKind::Mob(m) = &mut e.kind else { return false };
            (m.living.attrs.value(Attr::AttackDamage) as f32, m.eq[living::MAINHAND], m.mo.yaw, m.living.attrs.value(Attr::AttackKnockback) as f32)
        };
        let mut rng = rand::thread_rng();
        let mut src = DamageSource::by("mob_attack", me);
        src.weapon = weapon;
        let tbody = Body { eid: target, info: tref.info, pos: tref.pos, head_yaw: 0.0, velocity: [0.0; 3], player: None, invulnerable: false, fire_immune: false, raining: self.raining(), thundering: self.thundering() };
        let dmg = match weapon {
            Some(w) => living::modify_damage(w, &tbody, &src, dmg, &mut rng),
            None => dmg,
        };
        let target_player = self.players().into_iter().find(|p| p.eid == target);
        let old_motion = target_player.as_ref().map(|t| *t.motion.lock());
        let hurt = self.hurt_entity(target, &src, dmg);
        if hurt {
            let kb = match weapon {
                Some(w) => living::modify_knockback(w, &tbody, &src, kb_attr, &mut rng),
                None => kb_attr,
            } / 2.0;
            if kb > 0.0 {
                let (sx, cz) = yaw_dir(yaw);
                self.knockback_entity(target, kb as f64, sx, cz);
                if let Some(e) = self.entities.lock().get_mut(&mob_eid) {
                    e.vel[0] *= 0.6;
                    e.vel[2] *= 0.6;
                }
            }
            if let (Some(t), Some(old)) = (&target_player, old_motion) {
                if kb > 0.0 {
                    *t.motion.lock() = old;
                }
            }
            self.post_attack(target, &src, weapon);
        }
        hurt
    }

    /// `EnchantmentHelper.doPostAttackEffectsWithItemSource` + running the
    /// resulting entity effects.
    pub fn post_attack(&self, victim: i32, src: &DamageSource, weapon: Option<Stack>) {
        let mut rng = rand::thread_rng();
        let Some(vref) = self.entity_ref(victim) else { return };
        let veq = match self.players().into_iter().find(|p| p.eid == victim) {
            Some(p) => Some(p.equipment()),
            None => match self.entities.lock().get(&victim).map(|e| &e.kind) {
                Some(EKind::Mob(m)) => Some(m.eq),
                _ => None,
            },
        };
        let vbody = Body { eid: victim, info: vref.info, pos: vref.pos, head_yaw: 0.0, velocity: [0.0; 3], player: None, invulnerable: false, fire_immune: false, raining: self.raining(), thundering: self.thundering() };
        let effects = living::post_attack_effects(veq.as_ref(), &vbody, src, weapon, &mut rng);
        for pa in effects {
            let target = match pa.affected.as_str() {
                "attacker" => src.causing.map(|c| c.eid),
                "damaging_entity" => src.direct.map(|c| c.eid),
                _ => Some(victim),
            };
            let Some(target) = target else { continue };
            // owner of the enchanted item
            let owner = if pa.enchanted_victim { Some(victim) } else { src.causing.map(|c| c.eid) };
            self.run_entity_effect(&pa.effect, pa.level, target, owner, pa.slot, &mut rng);
        }
    }

    /// One enchantment entity effect on `target`.
    fn run_entity_effect(&self, e: &EntityEffect, level: i32, target: i32, owner: Option<i32>, slot: usize, rng: &mut impl Rng) {
        match e {
            EntityEffect::AllOf(list) => {
                for x in list {
                    self.run_entity_effect(x, level, target, owner, slot, rng);
                }
            }
            EntityEffect::Ignite(d) => {
                let ticks = (d.calc(level) * 20.0) as i32;
                self.ignite(target, ticks);
            }
            EntityEffect::ApplyMobEffect { effects, min_duration, max_duration, min_amp, max_amp } => {
                if effects.is_empty() {
                    return;
                }
                let pick = &effects[rng.gen_range(0..effects.len())];
                let Some(effect) = Effect::from_name(pick) else { return };
                let duration = (random_between(rng, min_duration.calc(level), max_duration.calc(level)) * 20.0).round() as i32;
                let amp = random_between(rng, min_amp.calc(level), max_amp.calc(level)).round().max(0.0) as i32;
                self.add_effect(target, Instance::new(effect, duration, amp));
            }
            EntityEffect::DamageEntity { min, max, damage_type } => {
                let amount = random_between(rng, min.calc(level), max.calc(level));
                let ty = DamageTypesName::intern(damage_type);
                let src = match owner.and_then(|o| self.entity_ref(o)) {
                    Some(o) => DamageSource::by(ty, o),
                    None => DamageSource::new(ty),
                };
                self.hurt_entity(target, &src, amount);
            }
            EntityEffect::ChangeItemDamage(v) => {
                let amount = v.calc(level) as i32;
                let Some(o) = owner else { return };
                if let Some(p) = self.players().into_iter().find(|p| p.eid == o) {
                    let infinite = p.gamemode.load(Ordering::Relaxed) == 1;
                    self.with_player(&p, |l, eq, _| {
                        let mut evs = Vec::new();
                        living::hurt_and_break(eq, slot, amount, infinite, &mut evs, &mut rand::thread_rng());
                        l.events.extend(evs);
                    });
                } else {
                    self.with_mob(o, |l, eq, _| {
                        let mut evs = Vec::new();
                        living::hurt_and_break(eq, slot, amount, false, &mut evs, &mut rand::thread_rng());
                        l.events.extend(evs);
                    });
                }
            }
            EntityEffect::ApplyExhaustion(v) => {
                if let Some(p) = self.players().into_iter().find(|p| p.eid == target) {
                    p.vit.lock().add_exhaustion(v.calc(level));
                }
            }
            EntityEffect::PlaySound(name, vol, pitch) => {
                if let Some(r) = self.entity_ref(target) {
                    self.play_sound(name, 7, r.pos, *vol, *pitch);
                }
            }
            EntityEffect::SummonEntity(ty) => {
                if ty.trim_start_matches("minecraft:") == "lightning_bolt" {
                    if let Some(r) = self.entity_ref(target) {
                        self.strike_lightning(r.pos, owner);
                    }
                }
            }
            EntityEffect::Explode(_) | EntityEffect::ApplyImpulse(_) | EntityEffect::Other(_) => {}
        }
    }

    /// Sets an entity on fire for `ticks` (`igniteForTicks`, keeps longer fire).
    pub fn ignite(&self, eid: i32, ticks: i32) {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            let mut v = p.vit.lock();
            if v.fire < ticks {
                v.fire = ticks;
                v.data_dirty = true;
            }
            return;
        }
        let mut ents = self.entities.lock();
        if let Some(e) = ents.get_mut(&eid) {
            if let EKind::Mob(m) = &mut e.kind {
                let immune = EntityTypes::get().by_name(m.ty.name()).is_some_and(|t| t.fire_immune);
                if !immune && m.fire < ticks {
                    m.fire = ticks;
                }
            }
        }
    }

    /// `addEffect` on any living entity.
    pub fn add_effect(&self, eid: i32, i: Instance) -> bool {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return self.with_player(&p, |l, _, body| l.add_effect(&body.info, i));
        }
        self.with_mob(eid, |l, _, body| l.add_effect(&body.info, i)).unwrap_or(false)
    }
}

/// Damage type names used by enchantment effects, as `&'static str`.
struct DamageTypesName;

impl DamageTypesName {
    fn intern(name: &str) -> &'static str {
        let types = &super::combat::damage::DamageTypes::get().types;
        types.get_key_value(name.trim_start_matches("minecraft:")).map(|(k, _)| k.as_str()).unwrap_or("generic")
    }
}

/// What enchantment conditions see of a mob.
pub fn mob_info(m: &mob::Mob, on_ground: bool) -> EntInfo {
    EntInfo { type_name: EntityTypes::static_name(m.ty.name()), on_fire: m.fire > 0, on_ground, ..Default::default() }
}

impl Game {
    /// Display name component of an entity.
    pub fn display_name(&self, eid: i32) -> serde_json::Value {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return serde_json::json!({"text": p.name});
        }
        match self.entities.lock().get(&eid).map(|e| &e.kind) {
            Some(EKind::Mob(m)) => serde_json::json!({"translate": format!("entity.minecraft.{}", m.ty.name())}),
            Some(EKind::Projectile(p)) => serde_json::json!({"translate": format!("entity.minecraft.{}", p.ty.entity_name())}),
            _ => serde_json::json!({"translate": "entity.minecraft.player"}),
        }
    }

    /// `DamageSource.getLocalizedDeathMessage` / `CombatTracker.getDeathMessage`.
    pub fn death_message(&self, src: &DamageSource, victim: serde_json::Value, kill_credit: Option<i32>) -> serde_json::Value {
        let kind = src.kind();
        let msg_id = kind.map(|k| k.message_id.as_str()).unwrap_or("generic");
        if kind.is_some_and(|k| k.death_message == "fall_variants") && src.is("is_fall") {
            return match kill_credit {
                Some(k) => serde_json::json!({"translate": "death.fell.finish", "with": [victim, self.display_name(k)]}),
                None => serde_json::json!({"translate": "death.fell.accident.generic", "with": [victim]}),
            };
        }
        if kind.is_some_and(|k| k.death_message == "intentional_game_design") {
            return serde_json::json!({"translate": format!("death.attack.{msg_id}.message"), "with": [victim, {"translate": "death.attack.badRespawnPoint.link", "clickEvent": {"action": "open_url", "url": "https://bugs.mojang.com/browse/MCPE-28723"}, "hoverEvent": {"action": "show_text", "value": "MCPE-28723"}}]});
        }
        let key = format!("death.attack.{msg_id}");
        match (src.causing, src.direct) {
            (None, None) => match kill_credit {
                Some(k) => serde_json::json!({"translate": format!("{key}.player"), "with": [victim, self.display_name(k)]}),
                None => serde_json::json!({"translate": key, "with": [victim]}),
            },
            (c, d) => {
                let who = c.or(d).expect("entity");
                let name = self.display_name(who.eid);
                let held = c.and_then(|c| {
                    if let Some(p) = self.players().into_iter().find(|p| p.eid == c.eid) {
                        p.equipment()[living::MAINHAND]
                    } else {
                        match self.entities.lock().get(&c.eid).map(|e| &e.kind) {
                            Some(EKind::Mob(m)) => m.eq[living::MAINHAND],
                            _ => None,
                        }
                    }
                });
                match held.and_then(|h| h.patch().custom_name.clone().map(|n| (h, n))) {
                    Some((_, n)) => serde_json::json!({"translate": format!("{key}.item"), "with": [victim, name, {"text": "[", "extra": [n, "]"]}]}),
                    None => serde_json::json!({"translate": key, "with": [victim, name]}),
                }
            }
        }
    }

    /// Lightning strike (channeling, thunderstorms): placeholder until the
    /// weather module provides the bolt entity.
    pub fn strike_lightning(&self, pos: [f64; 3], cause: Option<i32>) {
        self.lightning_at(pos, cause);
    }
}
