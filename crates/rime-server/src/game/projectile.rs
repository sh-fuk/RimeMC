//! Projectiles: `AbstractArrow` (arrow, spectral arrow, trident),
//! `ThrowableProjectile` (snowball, egg, ender pearl, splash / lingering
//! potion, bottle o' enchanting) and `AreaEffectCloud`, ported from 26.1.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::Rng;

use super::combat::damage::{DamageSource, EntityRef};
use super::combat::effects::{Effect, Instance};
use super::combat::enchant::{self, Ctx, EntInfo};
use super::combat::living::{self, Body};
use super::consume::potion_effects;
use super::entity::{EKind, Entity};
use super::physics::{self, BlockHit};
use super::{Game, Player, Request};
use crate::anticheat::combat::Aabb;
use crate::data::{Items, Stack};
use crate::net::codec::Out;
use crate::registry::{vstate, BlockState, Registries};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PType {
    Arrow,
    SpectralArrow,
    Trident,
    Snowball,
    Egg,
    EnderPearl,
    SplashPotion,
    LingeringPotion,
    XpBottle,
    /// ghast fireball
    LargeFireball,
    /// blaze fireball
    SmallFireball,
    WitherSkull,
    DragonFireball,
    ShulkerBullet,
    LlamaSpit,
    /// breeze wind charge
    WindCharge,
}

impl PType {
    pub fn entity_name(self) -> &'static str {
        match self {
            PType::Arrow => "arrow",
            PType::SpectralArrow => "spectral_arrow",
            PType::Trident => "trident",
            PType::Snowball => "snowball",
            PType::Egg => "egg",
            PType::EnderPearl => "ender_pearl",
            PType::SplashPotion => "splash_potion",
            PType::LingeringPotion => "lingering_potion",
            PType::XpBottle => "experience_bottle",
            PType::LargeFireball => "fireball",
            PType::SmallFireball => "small_fireball",
            PType::WitherSkull => "wither_skull",
            PType::DragonFireball => "dragon_fireball",
            PType::ShulkerBullet => "shulker_bullet",
            PType::LlamaSpit => "llama_spit",
            PType::WindCharge => "breeze_wind_charge",
        }
    }
    /// `AbstractHurtingProjectile` (+ shulker bullets and llama spit, which
    /// fly by their own rules)
    pub fn is_hurting(self) -> bool {
        matches!(self, PType::LargeFireball | PType::SmallFireball | PType::WitherSkull | PType::DragonFireball | PType::ShulkerBullet | PType::LlamaSpit | PType::WindCharge)
    }
    pub fn is_arrow(self) -> bool {
        matches!(self, PType::Arrow | PType::SpectralArrow | PType::Trident)
    }
    /// `getDefaultGravity`
    pub fn gravity(self) -> f64 {
        match self {
            PType::Arrow | PType::SpectralArrow | PType::Trident => 0.05,
            PType::SplashPotion | PType::LingeringPotion => 0.05,
            PType::XpBottle => 0.07,
            _ => 0.03,
        }
    }
    /// (width, height) of the entity type
    pub fn size(self) -> (f64, f64) {
        match self {
            PType::Arrow | PType::SpectralArrow | PType::Trident => (0.5, 0.5),
            PType::LargeFireball | PType::DragonFireball => (1.0, 1.0),
            PType::SmallFireball | PType::WitherSkull => (0.3125, 0.3125),
            PType::ShulkerBullet => (0.3125, 0.3125),
            PType::WindCharge => (0.3125, 0.3125),
            _ => (0.25, 0.25),
        }
    }
    /// The thrown item -> projectile type.
    pub fn for_thrown(item: &str) -> Option<PType> {
        Some(match item {
            "snowball" => PType::Snowball,
            "egg" | "blue_egg" | "brown_egg" => PType::Egg,
            "ender_pearl" => PType::EnderPearl,
            "splash_potion" => PType::SplashPotion,
            "lingering_potion" => PType::LingeringPotion,
            "experience_bottle" => PType::XpBottle,
            _ => return None,
        })
    }
}

/// `AbstractArrow.Pickup`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pickup {
    Disallowed,
    Allowed,
    CreativeOnly,
}

#[derive(Clone, Debug)]
pub struct Projectile {
    pub ty: PType,
    pub owner: Option<i32>,
    /// the item it was made from (and is picked up as)
    pub item: Stack,
    /// the bow / crossbow / trident that fired it
    pub weapon: Option<Stack>,
    pub base_damage: f64,
    pub crit: bool,
    pub pierce: u8,
    pub pierced: Vec<i32>,
    pub in_ground: bool,
    pub in_ground_time: i32,
    pub stuck: Option<([i32; 3], BlockState)>,
    pub life: i32,
    pub shake: i32,
    pub pickup: Pickup,
    pub on_fire: bool,
    pub yaw: f32,
    pub pitch: f32,
    pub no_physics: bool,
    // trident
    pub loyalty: u8,
    pub dealt_damage: bool,
    pub foil: bool,
    /// homing target (shulker bullets)
    pub target: Option<i32>,
    /// `accelerationPower`
    pub accel: f64,
    /// `Arrow.addEffect`: extra effects (stray slowness, bogged poison...)
    pub extra_effects: Vec<(Effect, i32, i32)>,
    /// wither skulls: `setDangerous`
    pub dangerous: bool,
}

impl Projectile {
    pub fn new(ty: PType, owner: Option<i32>, item: Stack) -> Projectile {
        Projectile {
            ty,
            owner,
            item,
            weapon: None,
            base_damage: 2.0,
            crit: false,
            pierce: 0,
            pierced: Vec::new(),
            in_ground: false,
            in_ground_time: 0,
            stuck: None,
            life: 0,
            shake: 0,
            pickup: Pickup::Disallowed,
            on_fire: false,
            yaw: 0.0,
            pitch: 0.0,
            no_physics: false,
            loyalty: 0,
            dealt_damage: false,
            foil: false,
            target: None,
            accel: 0.1,
            extra_effects: Vec::new(),
            dangerous: false,
        }
    }
}

/// `AreaEffectCloud`
#[derive(Clone, Debug)]
pub struct Cloud {
    pub owner: Option<i32>,
    pub potion: Option<String>,
    pub scale: f32,
    pub radius: f32,
    pub radius_on_use: f32,
    pub radius_per_tick: f32,
    pub duration: i32,
    pub wait_time: i32,
    pub reapplication_delay: i32,
    pub victims: Vec<(i32, i32)>,
    pub color: i32,
}

/// Things a projectile tick asks for once the entity lock is released.
#[derive(Clone, Debug)]
pub enum PAct {
    /// an arrow / trident hit an entity
    ArrowHit { eid: i32, proj: Box<Projectile>, target: i32, pos: [f64; 3], vel: [f64; 3], removed: bool, chunk: (i32, i32) },
    /// a throwable hit something (removed)
    Land { eid: i32, proj: Box<Projectile>, pos: [f64; 3], vel: [f64; 3], target: Option<i32>, block: Option<BlockHit> },
    /// a player touched a stuck arrow / returned trident
    Pickup { eid: i32, player: i32 },
    Sound { name: &'static str, pos: [f64; 3], volume: f32, pitch: f32 },
    Cloud { owner: Option<i32>, potion: Option<String>, scale: f32, hits: Vec<i32> },
}

/// What projectile ticks can collide with.
#[derive(Clone, Debug)]
pub struct Target {
    pub eid: i32,
    pub bbox: Aabb,
    pub player: bool,
    pub eye: [f64; 3],
    pub creative: bool,
}

/// `Projectile.getMovementToShoot` + `shootFromRotation`.
pub fn shoot_vector(pitch: f32, yaw: f32, y_offset: f32, pow: f32, uncertainty: f32, rng: &mut impl Rng) -> [f64; 3] {
    let r = std::f32::consts::PI / 180.0;
    let xd = (-(yaw * r).sin() * (pitch * r).cos()) as f64;
    let yd = (-((pitch + y_offset) * r).sin()) as f64;
    let zd = ((yaw * r).cos() * (pitch * r).cos()) as f64;
    let len = (xd * xd + yd * yd + zd * zd).sqrt().max(1e-9);
    let tri = |rng: &mut dyn rand::RngCore| 0.0172275 * uncertainty as f64 * (rng.r#gen::<f64>() - rng.r#gen::<f64>());
    [(xd / len + tri(rng)) * pow as f64, (yd / len + tri(rng)) * pow as f64, (zd / len + tri(rng)) * pow as f64]
}

/// `Projectile.shoot` towards a direction (mobs).
pub fn shoot_dir(d: [f64; 3], pow: f32, uncertainty: f32, rng: &mut impl Rng) -> [f64; 3] {
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-9);
    let tri = |rng: &mut dyn rand::RngCore| 0.0172275 * uncertainty as f64 * (rng.r#gen::<f64>() - rng.r#gen::<f64>());
    [(d[0] / len + tri(rng)) * pow as f64, (d[1] / len + tri(rng)) * pow as f64, (d[2] / len + tri(rng)) * pow as f64]
}

fn rot_of(v: [f64; 3]) -> (f32, f32) {
    let h = (v[0] * v[0] + v[2] * v[2]).sqrt();
    (v[0].atan2(v[2]).to_degrees() as f32, v[1].atan2(h).to_degrees() as f32)
}

/// `ProjectileUtil.computeMargin`
fn margin(age: u32) -> f64 {
    ((age as f64 - 2.0) / 20.0).clamp(0.0, 0.3)
}

fn len(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// Value of an enchantment value-effect component (`minecraft:ammo_use`, …)
/// summed over the item's enchantments.
pub fn enchant_value(weapon: Stack, component: &str, tool: Option<Stack>, this: Option<&EntInfo>, base: f32, rng: &mut impl Rng) -> f32 {
    let mut v = base;
    for (e, l) in enchant::on_item(weapon, None) {
        let Some(list) = e.raw["effects"].get(format!("minecraft:{component}")) else { continue };
        let effects = enchant::conditionals_pub(list);
        let c = Ctx { level: l, this, attacker: None, direct: None, source: None, tool: Some(tool.unwrap_or(weapon)), raining: false, thundering: false, active: true };
        enchant::apply_values(&effects, &c, rng, &mut v);
    }
    v
}

/// A single (unconditional) value effect component such as
/// `minecraft:crossbow_charge_time`.
pub fn enchant_single(weapon: Stack, component: &str, base: f32, rng: &mut impl Rng) -> f32 {
    let mut v = base;
    for (e, l) in enchant::on_item(weapon, None) {
        if let Some(x) = e.raw["effects"].get(format!("minecraft:{component}")) {
            if x.is_object() {
                v = enchant::ValueEffect::parse(x).process(l, rng, v);
            }
        }
    }
    v
}

/// Does the weapon have an enchantment with this effect component at all?
pub fn has_enchant_effect(weapon: Stack, component: &str) -> bool {
    enchant::on_item(weapon, None).iter().any(|(e, _)| e.raw["effects"].get(format!("minecraft:{component}")).is_some())
}

impl Game {
    /// Spawns a projectile.
    pub fn launch(&self, p: Projectile, pos: [f64; 3], vel: [f64; 3]) -> i32 {
        let mut p = p;
        let (yaw, pitch) = rot_of(vel);
        p.yaw = yaw;
        p.pitch = pitch;
        let eid = self.next_eid();
        let e = Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Projectile(Box::new(p)), pos, vel, on_ground: false, age: 0, synced: pos };
        self.add_entity(e);
        eid
    }

    pub(super) fn projectile_spawn_packets(&self, e: &Entity) -> Vec<Arc<Vec<u8>>> {
        let regs = Registries::get();
        let mut out = Vec::new();
        match &e.kind {
            EKind::Projectile(p) => {
                let ty = regs.id("minecraft:entity_type", &format!("minecraft:{}", p.ty.entity_name())).unwrap_or(0);
                let mut b = Vec::new();
                b.vi(e.eid).uuid(e.uuid).vi(ty).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]);
                super::mob::write_lpvec(&mut b, e.vel);
                b.u8(super::mob::angle_byte(p.pitch)).u8(super::mob::angle_byte(p.yaw)).u8(0).vi(p.owner.unwrap_or(0));
                out.push(self.encode("add_entity", &b));
                out.push(self.projectile_meta(e.eid, p));
            }
            EKind::Cloud(c) => {
                let ty = regs.id("minecraft:entity_type", "minecraft:area_effect_cloud").unwrap_or(0);
                let mut b = Vec::new();
                b.vi(e.eid).uuid(e.uuid).vi(ty).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]).u8(0).u8(0).u8(0).u8(0).vi(0);
                out.push(self.encode("add_entity", &b));
                out.push(self.cloud_meta(e.eid, c, e.age as i32));
            }
            _ => {}
        }
        out
    }

    pub(super) fn projectile_meta(&self, eid: i32, p: &Projectile) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid);
        b.u8(0).vi(0).u8(if p.on_fire { 1 } else { 0 });
        if p.ty.is_arrow() {
            b.u8(8).vi(0).u8(if p.crit { 1 } else { 0 });
            b.u8(9).vi(0).u8(p.pierce);
            b.u8(10).vi(8).bool(p.in_ground);
            if p.ty == PType::Trident {
                b.u8(11).vi(0).u8(p.loyalty);
                b.u8(12).vi(8).bool(p.foil);
            } else if p.ty == PType::Arrow {
                let color = p.item.patch().potion.as_deref().map(potion_color).unwrap_or(-1);
                b.u8(11).vi(1).vi(color);
            }
        } else {
            b.u8(8).vi(7);
            super::write_slot(&mut b, Some(p.item));
        }
        b.u8(0xFF);
        self.encode("set_entity_data", &b)
    }

    fn cloud_meta(&self, eid: i32, c: &Cloud, age: i32) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid);
        b.u8(8).vi(3).f32(c.radius);
        b.u8(9).vi(8).bool(age < c.wait_time);
        // entity_effect particle with the potion colour
        if let Some(pid) = Registries::get().id("minecraft:particle_type", "minecraft:entity_effect") {
            b.u8(10).vi(16).vi(pid).i32(c.color | 0xFF00_0000u32 as i32);
        }
        b.u8(0xFF);
        self.encode("set_entity_data", &b)
    }

    /// One projectile tick; true when it is removed.
    pub(super) fn tick_projectile(&self, e: &mut Entity, targets: &[Target], acts: &mut Vec<PAct>) -> bool {
        let EKind::Projectile(p) = &mut e.kind else { return true };
        if e.pos[1] < (crate::world::chunk::MIN_Y - 64) as f64 {
            return true;
        }
        if p.ty.is_arrow() {
            self.tick_arrow_like(e, targets, acts)
        } else if p.ty.is_hurting() {
            self.tick_hurting(e, targets, acts)
        } else {
            self.tick_throwable(e, targets, acts)
        }
    }

    /// `ThrowableProjectile.tick`
    fn tick_throwable(&self, e: &mut Entity, targets: &[Target], acts: &mut Vec<PAct>) -> bool {
        let EKind::Projectile(p) = &mut e.kind else { return true };
        let w = &self.world;
        e.vel[1] -= p.ty.gravity();
        let in_water = w.get_block(e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32).is_some_and(vstate::is_water);
        let inertia = if in_water { 0.8 } else { 0.99 };
        e.vel = [e.vel[0] * inertia, e.vel[1] * inertia, e.vel[2] * inertia];
        let from = e.pos;
        let mut to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
        let block = physics::clip(w, from, to);
        if let Some(b) = &block {
            to = b.pos;
        }
        let (wd, h) = p.ty.size();
        let sweep = Aabb::entity(from, wd, h).expand_towards(e.vel).inflate(1.0, 1.0, 1.0);
        let m = margin(e.age);
        let mut hit: Option<(f64, i32, [f64; 3])> = None;
        for t in targets {
            if !t.bbox.intersects(&sweep) {
                continue;
            }
            let bb = t.bbox.inflate(m, m, m);
            let point = if bb.contains(from) { Some(from) } else { bb.clip(from, to).map(|c| c.0) };
            if let Some(pt) = point {
                let d = (pt[0] - from[0]).powi(2) + (pt[1] - from[1]).powi(2) + (pt[2] - from[2]).powi(2);
                if hit.is_none_or(|h| d < h.0) {
                    hit = Some((d, t.eid, pt));
                }
            }
        }
        let (yaw, pitch) = rot_of(e.vel);
        p.yaw = yaw;
        p.pitch = pitch;
        match (hit, block) {
            (Some((_, target, pt)), _) => {
                e.pos = pt;
                acts.push(PAct::Land { eid: e.eid, proj: p.clone(), pos: pt, vel: e.vel, target: Some(target), block: None });
                true
            }
            (None, Some(b)) => {
                e.pos = b.pos;
                acts.push(PAct::Land { eid: e.eid, proj: p.clone(), pos: b.pos, vel: e.vel, target: None, block: Some(b) });
                true
            }
            (None, None) => {
                e.pos = to;
                false
            }
        }
    }

    /// `AbstractHurtingProjectile.tick` / `ShulkerBullet.tick` / `LlamaSpit.tick`
    fn tick_hurting(&self, e: &mut Entity, targets: &[Target], acts: &mut Vec<PAct>) -> bool {
        let EKind::Projectile(p) = &mut e.kind else { return true };
        let w = &self.world;
        p.life += 1;
        if p.life > 1200 && p.ty != PType::LlamaSpit || p.ty == PType::WindCharge && p.life > 100 {
            return true;
        }
        if p.ty == PType::ShulkerBullet {
            // ShulkerBullet: steer towards the target (simplified homing)
            if let Some(t) = p.target.and_then(|t| targets.iter().find(|x| x.eid == t)) {
                let c = [(t.bbox.min[0] + t.bbox.max[0]) / 2.0, (t.bbox.min[1] + t.bbox.max[1]) / 2.0, (t.bbox.min[2] + t.bbox.max[2]) / 2.0];
                let d = [c[0] - e.pos[0], c[1] - e.pos[1], c[2] - e.pos[2]];
                let l = len(d).max(1e-6);
                for i in 0..3 {
                    e.vel[i] = (e.vel[i] + (d[i] / l * 0.15 - e.vel[i]) * 0.2).clamp(-1.0, 1.0);
                }
            } else {
                e.vel[1] -= 0.04;
            }
        }
        let from = e.pos;
        let mut to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
        let block = physics::clip(w, from, to);
        if let Some(b) = &block {
            to = b.pos;
        }
        let (wd, h) = p.ty.size();
        let sweep = Aabb::entity(from, wd, h).expand_towards(e.vel).inflate(1.0, 1.0, 1.0);
        let mut hit: Option<(f64, i32, [f64; 3])> = None;
        for t in targets {
            if Some(t.eid) == p.owner && p.life < 30 || !t.bbox.intersects(&sweep) {
                continue;
            }
            let bb = t.bbox.inflate(0.3, 0.3, 0.3);
            let point = if bb.contains(from) { Some(from) } else { bb.clip(from, to).map(|c| c.0) };
            if let Some(pt) = point {
                let d = (pt[0] - from[0]).powi(2) + (pt[1] - from[1]).powi(2) + (pt[2] - from[2]).powi(2);
                if hit.is_none_or(|h| d < h.0) {
                    hit = Some((d, t.eid, pt));
                }
            }
        }
        match (hit, block) {
            (Some((_, target, pt)), _) => {
                acts.push(PAct::Land { eid: e.eid, proj: p.clone(), pos: pt, vel: e.vel, target: Some(target), block: None });
                return true;
            }
            (None, Some(b)) => {
                acts.push(PAct::Land { eid: e.eid, proj: p.clone(), pos: b.pos, vel: e.vel, target: None, block: Some(b) });
                return true;
            }
            _ => {}
        }
        e.pos = to;
        let in_water = w.get_block(e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32).is_some_and(vstate::is_water);
        match p.ty {
            PType::LlamaSpit => {
                e.vel = [e.vel[0] * 0.99, e.vel[1] * 0.99 - 0.06, e.vel[2] * 0.99];
                if in_water {
                    return true;
                }
            }
            PType::ShulkerBullet => {}
            PType::WindCharge => {}
            _ => {
                let inertia = if in_water { 0.8 } else { 0.95 };
                let l = len(e.vel);
                let n = if l > 1e-7 { [e.vel[0] / l, e.vel[1] / l, e.vel[2] / l] } else { [0.0; 3] };
                for i in 0..3 {
                    e.vel[i] = (e.vel[i] + n[i] * p.accel) * inertia;
                }
            }
        }
        let (yaw, pitch) = rot_of(e.vel);
        p.yaw = yaw;
        p.pitch = pitch;
        false
    }

    /// `AbstractArrow.tick` (+ `ThrownTrident.tick`)
    fn tick_arrow_like(&self, e: &mut Entity, targets: &[Target], acts: &mut Vec<PAct>) -> bool {
        let eid = e.eid;
        let chunk = e.chunk();
        let EKind::Projectile(p) = &mut e.kind else { return true };
        let w = &self.world;
        // ThrownTrident.tick: loyalty return
        if p.ty == PType::Trident {
            if p.in_ground_time > 4 {
                p.dealt_damage = true;
            }
            if p.loyalty > 0 && (p.dealt_damage || p.no_physics) {
                match p.owner.and_then(|o| targets.iter().find(|t| t.eid == o && t.player)) {
                    None => {
                        if p.pickup == Pickup::Allowed {
                            self.spawn_item(e.pos, p.item, [0.0, 0.1, 0.0], 10);
                        }
                        return true;
                    }
                    Some(o) => {
                        if !p.no_physics {
                            acts.push(PAct::Sound { name: "minecraft:item.trident.return", pos: e.pos, volume: 10.0, pitch: 1.0 });
                        }
                        p.no_physics = true;
                        p.in_ground = false;
                        let v = [o.eye[0] - e.pos[0], o.eye[1] - e.pos[1], o.eye[2] - e.pos[2]];
                        e.pos[1] += v[1] * 0.015 * p.loyalty as f64;
                        let accel = 0.05 * p.loyalty as f64;
                        let l = len(v).max(1e-9);
                        e.vel = [e.vel[0] * 0.95 + v[0] / l * accel, e.vel[1] * 0.95 + v[1] / l * accel, e.vel[2] * 0.95 + v[2] / l * accel];
                    }
                }
            }
        }
        let physics_on = !p.no_physics;
        let bp = [e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32];
        let state = w.get_block(bp[0], bp[1], bp[2]);
        if let Some(s) = state {
            if !s.is_air() && physics_on {
                for b in physics::collision(s) {
                    if b.translate([bp[0] as f64, bp[1] as f64, bp[2] as f64]).contains(e.pos) {
                        e.vel = [0.0; 3];
                        p.in_ground = true;
                        break;
                    }
                }
            }
        }
        if p.shake > 0 {
            p.shake -= 1;
        }
        let in_water = state.is_some_and(vstate::is_water);
        // player touch (pickup)
        if (p.in_ground || p.no_physics) && p.shake <= 0 {
            let (wd, h) = p.ty.size();
            let me = Aabb::entity(e.pos, wd, h);
            for t in targets.iter().filter(|t| t.player) {
                if t.bbox.inflate(1.0, 0.5, 1.0).intersects(&me) {
                    let ok = match p.pickup {
                        Pickup::Allowed => true,
                        Pickup::CreativeOnly => t.creative,
                        Pickup::Disallowed => p.ty == PType::Trident && p.no_physics && p.owner == Some(t.eid),
                    };
                    if ok {
                        acts.push(PAct::Pickup { eid, player: t.eid });
                        return false;
                    }
                }
            }
        }
        if p.in_ground && physics_on {
            let changed = p.stuck.is_some_and(|(pos, st)| w.get_block(pos[0], pos[1], pos[2]) != Some(st));
            let (wd, h) = p.ty.size();
            if changed && physics::no_collision(w, &Aabb::entity(e.pos, wd, h).inflate(0.06, 0.06, 0.06)) {
                // startFalling
                p.in_ground = false;
                let mut rng = rand::thread_rng();
                e.vel = [e.vel[0] * rng.r#gen::<f64>() * 0.2, e.vel[1] * rng.r#gen::<f64>() * 0.2, e.vel[2] * rng.r#gen::<f64>() * 0.2];
                p.life = 0;
            } else {
                // tickDespawn
                p.life += 1;
                if p.life >= 1200 {
                    return true;
                }
            }
            p.in_ground_time += 1;
            return false;
        }
        p.in_ground_time = 0;
        if in_water {
            let wi = if p.ty == PType::Trident { 0.99 } else { 0.6 };
            e.vel = [e.vel[0] * wi, e.vel[1] * wi, e.vel[2] * wi];
        }
        let (yaw, pitch) = rot_of(e.vel);
        p.yaw = yaw;
        p.pitch = pitch;
        let from = e.pos;
        if physics_on {
            let full_to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
            let block = physics::clip(w, from, full_to);
            let to = block.map(|b| b.pos).unwrap_or(full_to);
            // findHitEntities
            let (wd, h) = p.ty.size();
            let sweep = Aabb::entity(from, wd, h).expand_towards(e.vel).inflate(1.0, 1.0, 1.0);
            let m = margin(e.age);
            let mut hits: Vec<(f64, i32, [f64; 3])> = Vec::new();
            for t in targets {
                if !t.bbox.intersects(&sweep) || p.pierced.contains(&t.eid) {
                    continue;
                }
                // a trident that already dealt damage hits nothing more
                if p.ty == PType::Trident && p.dealt_damage {
                    continue;
                }
                let bb = t.bbox.inflate(m, m, m);
                let point = if bb.contains(from) { Some(from) } else { bb.clip(from, to).map(|c| c.0) };
                if let Some(pt) = point {
                    hits.push(((pt[0] - from[0]).powi(2) + (pt[1] - from[1]).powi(2) + (pt[2] - from[2]).powi(2), t.eid, pt));
                }
            }
            hits.sort_by(|a, b| a.0.total_cmp(&b.0));
            if hits.is_empty() {
                e.pos = to;
                if let Some(b) = block {
                    // onHitBlock
                    p.stuck = Some((b.block, b.state));
                    let s = [e.vel[0].signum() * 0.05, e.vel[1].signum() * 0.05, e.vel[2].signum() * 0.05];
                    e.pos = [e.pos[0] - s[0], e.pos[1] - s[1], e.pos[2] - s[2]];
                    e.vel = [0.0; 3];
                    let mut rng = rand::thread_rng();
                    let sound = if p.ty == PType::Trident { "minecraft:item.trident.hit_ground" } else { "minecraft:entity.arrow.hit" };
                    acts.push(PAct::Sound { name: sound, pos: e.pos, volume: 1.0, pitch: 1.2 / (rng.r#gen::<f32>() * 0.2 + 0.9) });
                    p.in_ground = true;
                    p.shake = 7;
                    p.crit = false;
                    p.pierce = 0;
                    p.pierced.clear();
                    return false;
                }
            } else {
                // hitTargetsOrDeflectSelf: with piercing, every entity up to the
                // pierce level gets hit this tick
                let max = if p.pierce > 0 { p.pierce as usize + 1 } else { 1 };
                let mut removed = false;
                for (i, (_, target, pt)) in hits.iter().enumerate() {
                    if i >= max {
                        break;
                    }
                    e.pos = *pt;
                    if p.pierce > 0 {
                        if p.pierced.len() >= p.pierce as usize + 1 {
                            removed = true;
                            break;
                        }
                        p.pierced.push(*target);
                    }
                    let last = p.pierce == 0;
                    let is_trident = p.ty == PType::Trident;
                    acts.push(PAct::ArrowHit { eid, proj: p.clone(), target: *target, pos: *pt, vel: e.vel, removed: last && !is_trident, chunk });
                    if is_trident {
                        // ThrownTrident.onHitEntity: deflect REVERSE, then slow down
                        p.dealt_damage = true;
                        e.vel = [-e.vel[0] * 0.5 * 0.02, -e.vel[1] * 0.5 * 0.2, -e.vel[2] * 0.5 * 0.02];
                        p.yaw += 170.0 + rand::thread_rng().r#gen::<f32>() * 20.0;
                        break;
                    }
                    if last {
                        removed = true;
                        break;
                    }
                }
                if removed {
                    return true;
                }
            }
        } else {
            e.pos = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
        }
        if !in_water {
            e.vel = [e.vel[0] * 0.99, e.vel[1] * 0.99, e.vel[2] * 0.99];
        }
        if physics_on && !p.in_ground {
            e.vel[1] -= p.ty.gravity();
        }
        false
    }

    /// `AreaEffectCloud.serverTick`; true when removed.
    pub(super) fn tick_cloud(&self, e: &mut Entity, targets: &[Target], acts: &mut Vec<PAct>, sends: &mut Vec<((i32, i32), Arc<Vec<u8>>)>) -> bool {
        let age = e.age as i32;
        let chunk = e.chunk();
        let pos = e.pos;
        let EKind::Cloud(c) = &mut e.kind else { return true };
        if c.duration != -1 && age - c.wait_time >= c.duration {
            return true;
        }
        if age == c.wait_time {
            sends.push((chunk, self.cloud_meta(e.eid, c, age)));
        }
        if age < c.wait_time {
            return false;
        }
        let mut radius = c.radius;
        if c.radius_per_tick != 0.0 {
            radius += c.radius_per_tick;
            if radius < 0.5 {
                return true;
            }
            c.radius = radius;
        }
        if age % 5 == 0 {
            c.victims.retain(|v| age < v.1);
            let effects = c.potion.as_deref().map(potion_effects).unwrap_or(&[]);
            if effects.is_empty() {
                c.victims.clear();
            } else {
                let area = Aabb { min: [pos[0] - radius as f64, pos[1], pos[2] - radius as f64], max: [pos[0] + radius as f64, pos[1] + 0.5, pos[2] + radius as f64] };
                let mut hits = Vec::new();
                for t in targets {
                    if !t.bbox.intersects(&area) || c.victims.iter().any(|v| v.0 == t.eid) {
                        continue;
                    }
                    let feet = [(t.bbox.min[0] + t.bbox.max[0]) / 2.0, t.bbox.min[1], (t.bbox.min[2] + t.bbox.max[2]) / 2.0];
                    let (xd, zd) = (feet[0] - pos[0], feet[2] - pos[2]);
                    if xd * xd + zd * zd <= (radius * radius) as f64 {
                        c.victims.push((t.eid, age + c.reapplication_delay));
                        hits.push(t.eid);
                        if c.radius_on_use != 0.0 {
                            radius += c.radius_on_use;
                            if radius < 0.5 {
                                acts.push(PAct::Cloud { owner: c.owner, potion: c.potion.clone(), scale: c.scale, hits });
                                return true;
                            }
                            c.radius = radius;
                        }
                    }
                }
                if !hits.is_empty() {
                    acts.push(PAct::Cloud { owner: c.owner, potion: c.potion.clone(), scale: c.scale, hits });
                    sends.push((chunk, self.cloud_meta(e.eid, c, age)));
                }
            }
        }
        false
    }

    /// Runs what projectile ticks asked for.
    pub(super) fn projectile_acts(&self, acts: Vec<PAct>) {
        let mut rng = rand::thread_rng();
        for a in acts {
            match a {
                PAct::Sound { name, pos, volume, pitch } => self.play_sound(name, 7, pos, volume, pitch),
                PAct::Pickup { eid, player } => self.pickup_projectile(eid, player),
                PAct::ArrowHit { eid, proj, target, pos, vel, removed, chunk } => self.arrow_hit(eid, *proj, target, pos, vel, removed, chunk, &mut rng),
                PAct::Land { eid, proj, pos, vel, target, block } => self.throwable_land(eid, *proj, pos, vel, target, block, &mut rng),
                PAct::Cloud { owner, potion, scale, hits } => {
                    let effects = potion.as_deref().map(potion_effects).unwrap_or(&[]).to_vec();
                    for t in hits {
                        let effects = effects.clone();
                        let apply = |l: &mut living::Living, eq: &mut living::Equipment, body: &mut Body| {
                            let mut rng = rand::thread_rng();
                            for (e, dur, amp) in effects {
                                if e.is_instant() {
                                    l.apply_instant(eq, body, e, amp, 0.5, None, &mut rng);
                                } else {
                                    let d = ((dur as f32 * scale).floor() as i32).max(1);
                                    l.add_effect(&body.info, Instance::new(e, d, amp));
                                }
                            }
                        };
                        let _ = owner;
                        if let Some(p) = self.players().into_iter().find(|p| p.eid == t) {
                            self.with_player(&p, apply);
                        } else {
                            self.with_mob(t, apply);
                        }
                    }
                }
            }
        }
    }

    fn victim_body(&self, eid: i32) -> Option<Body> {
        let r = self.entity_ref(eid)?;
        Some(Body { eid, info: r.info, pos: r.pos, head_yaw: 0.0, velocity: [0.0; 3], player: None, invulnerable: false, fire_immune: false, raining: self.raining(), thundering: self.thundering() })
    }

    /// `AbstractArrow.onHitEntity` / `ThrownTrident.onHitEntity`
    #[allow(clippy::too_many_arguments)]
    fn arrow_hit(&self, eid: i32, p: Projectile, target: i32, pos: [f64; 3], vel: [f64; 3], removed: bool, chunk: (i32, i32), rng: &mut impl Rng) {
        let owner = p.owner.and_then(|o| self.entity_ref(o));
        let me = EntityRef::new(eid, p.ty.entity_name(), pos, false);
        let ty = if p.ty == PType::Trident { "trident" } else { "arrow" };
        let mut src = DamageSource::indirect(ty, me, owner.or(Some(me)));
        src.projectile = true;
        src.projectile_motion = Some(vel);
        src.pierce_level = p.pierce;
        src.weapon = p.weapon;
        let Some(vbody) = self.victim_body(target) else { return };
        let is_enderman = vbody.info.type_name == "enderman";
        let mut damage;
        if p.ty == PType::Trident {
            damage = 8.0f32;
            if let Some(w) = p.weapon {
                damage = living::modify_damage(w, &vbody, &src, damage, rng);
            }
        } else {
            let mut base = p.base_damage as f32;
            if let Some(w) = p.weapon {
                base = living::modify_damage(w, &vbody, &src, base, rng);
            }
            let pow = len(vel);
            let mut d = (pow * base as f64).clamp(0.0, 2.147483647e9).ceil() as i64;
            if p.crit {
                let inc = rng.gen_range(0..(d / 2 + 2)) as i64;
                d = (d + inc).min(i32::MAX as i64);
            }
            damage = d as f32;
        }
        if p.on_fire && !is_enderman {
            self.ignite(target, 100);
        }
        let hurt = self.hurt_entity(target, &src, damage);
        if hurt {
            if is_enderman {
                return;
            }
            // doKnockback
            if let Some(w) = p.weapon {
                let kb = living::modify_knockback(w, &vbody, &src, 0.0, rng) as f64;
                if kb > 0.0 {
                    let kr = self.knockback_resistance(target);
                    let h = (vel[0] * vel[0] + vel[2] * vel[2]).sqrt();
                    if h > 0.0 {
                        let s = kb * 0.6 * (1.0 - kr).max(0.0);
                        self.push_entity(target, [vel[0] / h * s, 0.1, vel[2] / h * s]);
                    }
                }
            }
            self.post_attack(target, &src, p.weapon);
            // doPostHurtEffects
            match p.ty {
                PType::Arrow => {
                    let extra = p.extra_effects.clone();
                    if !extra.is_empty() {
                        self.with_living(target, move |l, eq, body| {
                            for (e, dur, amp) in extra {
                                super::consume::apply_potion_effect(l, eq, body, e, dur, amp, 1.0, eid, &mut rand::thread_rng());
                            }
                        });
                    }
                    let pot = p.item.patch().potion.clone();
                    if let Some(pot) = pot {
                        let scale = p.item.defaults().raw.get("potion_duration_scale").and_then(|s| s.as_f64()).unwrap_or(1.0) as f32;
                        let effects = potion_effects(&pot).to_vec();
                        let apply = move |l: &mut living::Living, eq: &mut living::Equipment, body: &mut Body| {
                            for (e, dur, amp) in effects {
                                super::consume::apply_potion_effect(l, eq, body, e, dur, amp, scale, eid, &mut rand::thread_rng());
                            }
                        };
                        self.with_living(target, apply);
                    }
                }
                PType::SpectralArrow => {
                    self.with_living(target, |l, _, body| {
                        l.add_effect(&body.info, Instance::new(Effect::Glowing, 200, 0));
                    });
                }
                _ => {}
            }
            let victim_player = self.players().into_iter().any(|pl| pl.eid == target);
            if victim_player && p.owner != Some(target) {
                if let Some(o) = p.owner.and_then(|o| self.players().into_iter().find(|pl| pl.eid == o)) {
                    let mut b = Vec::new();
                    b.u8(6).f32(0.0);
                    o.send(self.encode("game_event", &b));
                }
            }
            if p.ty == PType::Trident {
                self.play_sound("minecraft:item.trident.hit", 7, pos, 1.0, 1.0);
            } else {
                self.play_sound("minecraft:entity.arrow.hit", 7, pos, 1.0, 1.2 / (rng.r#gen::<f32>() * 0.2 + 0.9));
            }
        } else if p.ty == PType::Trident {
            self.play_sound("minecraft:item.trident.hit", 7, pos, 1.0, 1.0);
        } else if removed {
            // deflect REVERSE, then * 0.2; keep flying (re-add)
            let nv = [-vel[0] * 0.5 * 0.2, -vel[1] * 0.5 * 0.2, -vel[2] * 0.5 * 0.2];
            if nv[0] * nv[0] + nv[1] * nv[1] + nv[2] * nv[2] < 1.0e-7 {
                if p.pickup == Pickup::Allowed {
                    self.spawn_item(pos, p.item, [0.0, 0.1, 0.0], 10);
                }
            } else {
                let mut np = p.clone();
                np.yaw += 170.0 + rng.r#gen::<f32>() * 20.0;
                let _ = chunk;
                self.launch(np, pos, nv);
            }
        }
    }

    fn knockback_resistance(&self, eid: i32) -> f64 {
        use super::combat::attr::Attr;
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return p.vit.lock().living.attrs.value(Attr::KnockbackResistance);
        }
        match self.entities.lock().get_mut(&eid).map(|e| &mut e.kind) {
            Some(EKind::Mob(m)) => m.living.attrs.value(Attr::KnockbackResistance),
            _ => 0.0,
        }
    }

    /// Runs `f` on any living entity.
    pub fn with_living(&self, eid: i32, f: impl FnOnce(&mut living::Living, &mut living::Equipment, &mut Body)) {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            self.with_player(&p, f);
        } else {
            self.with_mob(eid, f);
        }
    }

    /// `ThrowableProjectile` impacts.
    #[allow(clippy::too_many_arguments)]
    fn throwable_land(&self, eid: i32, p: Projectile, pos: [f64; 3], vel: [f64; 3], target: Option<i32>, block: Option<BlockHit>, rng: &mut impl Rng) {
        let owner = p.owner.and_then(|o| self.entity_ref(o));
        let me = EntityRef::new(eid, p.ty.entity_name(), pos, false);
        let thrown = || {
            let mut s = DamageSource::indirect("thrown", me, owner);
            s.projectile = true;
            s.projectile_motion = Some(vel);
            s
        };
        // onHitEntity
        if let Some(t) = target {
            match p.ty {
                PType::Snowball => {
                    let dmg = if self.victim_body(t).is_some_and(|b| b.info.type_name == "blaze") { 3.0 } else { 0.0 };
                    self.hurt_entity(t, &thrown(), dmg);
                }
                PType::Egg | PType::EnderPearl => {
                    self.hurt_entity(t, &thrown(), 0.0);
                }
                PType::LargeFireball => {
                    let s = DamageSource::indirect("fireball", me, owner);
                    self.hurt_entity(t, &s, 6.0);
                }
                PType::SmallFireball => {
                    let immune = self.victim_body(t).is_some_and(|b| super::etypes::EntityTypes::get().by_name(b.info.type_name).is_some_and(|i| i.fire_immune));
                    if !immune {
                        let s = DamageSource::indirect("fireball", me, owner);
                        if self.hurt_entity(t, &s, 5.0) {
                            self.ignite(t, 100);
                        }
                    }
                }
                PType::WitherSkull => {
                    let s = DamageSource::indirect("wither_skull", me, owner);
                    let dmg = if owner.is_some() { 8.0 } else { 5.0 };
                    if self.hurt_entity(t, &s, dmg) {
                        let secs = match self.difficulty() { 2 => 10, 3 => 40, _ => 0 };
                        if secs > 0 {
                            self.add_effect(t, super::combat::effects::Instance::new(super::combat::effects::Effect::Wither, secs * 20, 1));
                        }
                    }
                }
                PType::ShulkerBullet => {
                    let s = DamageSource::indirect("mob_projectile", me, owner);
                    if self.hurt_entity(t, &s, 4.0) {
                        self.add_effect(t, super::combat::effects::Instance::new(super::combat::effects::Effect::Levitation, 200, 0));
                    }
                }
                PType::LlamaSpit => {
                    let s = DamageSource::indirect("spit", me, owner);
                    self.hurt_entity(t, &s, 1.0);
                }
                PType::WindCharge => {
                    let s = DamageSource::indirect("wind_charge", me, owner);
                    self.hurt_entity(t, &s, 1.0);
                }
                _ => {}
            }
        }
        let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
        // onHit
        match p.ty {
            PType::Snowball => self.broadcast_chunk(c, &self.entity_event(eid, 3)),
            PType::Egg => {
                if rng.gen_range(0..8) == 0 {
                    let n = if rng.gen_range(0..32) == 0 { 4 } else { 1 };
                    for _ in 0..n {
                        self.spawn_mob(super::mob::MobType::CHICKEN, pos, false);
                    }
                }
                self.broadcast_chunk(c, &self.entity_event(eid, 3));
            }
            PType::EnderPearl => {
                if let Some(o) = p.owner.and_then(|o| self.players().into_iter().find(|pl| pl.eid == o)) {
                    if !o.vit.lock().dead() {
                        if rng.r#gen::<f32>() < 0.05 {
                            if let Some(t) = super::mob::MobType::from_name("endermite") {
                                self.spawn_mob(t, o.pose.lock().pos, false);
                            }
                        }
                        let pose = *o.pose.lock();
                        o.requests.lock().push(Request::Teleport(pos, pose.yaw, pose.pitch));
                        o.vit.lock().fall_distance = 0.0;
                        self.hurt_player(&o, &DamageSource::new("ender_pearl"), 5.0);
                        self.play_sound("minecraft:entity.player.teleport", 7, pos, 1.0, 1.0);
                    }
                }
            }
            PType::SplashPotion | PType::LingeringPotion => self.potion_land(eid, &p, pos, target, block, owner, rng),
            PType::LargeFireball => {
                self.explode_with_fire(pos, 1.0, true);
            }
            PType::WitherSkull => self.explode_with_fire(pos, 1.0, false),
            PType::SmallFireball => {
                if let Some(b) = block {
                    let n = super::physics::face_normal(b.face);
                    let q = [b.block[0] + n[0], b.block[1] + n[1], b.block[2] + n[2]];
                    if self.world.get_block(q[0], q[1], q[2]).is_some_and(|s| s.is_air()) {
                        if let Some(f) = crate::registry::BlockRegistry::get().state("fire", &[]) {
                            self.change_block(q, f, false, None);
                        }
                    }
                }
            }
            PType::ShulkerBullet => {
                self.particles("minecraft:explosion", pos, 2, [0.2, 0.2, 0.2], 0.0);
                self.play_sound("minecraft:entity.shulker_bullet.hit", 5, pos, 1.0, 1.0);
            }
            PType::WindCharge => self.wind_burst(pos, owner.map(|o| o.eid)),
            PType::DragonFireball => {
                // DragonFireball: a lingering cloud of dragon's breath (instant damage)
                let mut c = Cloud { owner: p.owner, potion: Some("harming".into()), scale: 1.0, radius: 3.0, radius_on_use: 0.0, radius_per_tick: (7.0 - 3.0) / 600.0, duration: 600, wait_time: 0, reapplication_delay: 20, victims: Vec::new(), color: 0x9900ff };
                c.radius_on_use = 0.0;
                let eid = self.next_eid();
                self.add_entity(Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Cloud(Box::new(c)), pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos });
            }
            PType::XpBottle => {
                self.level_event(2002, [pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32], -13083194);
                let xp = 3 + rng.gen_range(0..5) + rng.gen_range(0..5);
                self.award_xp_orbs(pos, xp);
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn potion_land(&self, eid: i32, p: &Projectile, pos: [f64; 3], _target: Option<i32>, block: Option<BlockHit>, owner: Option<EntityRef>, rng: &mut impl Rng) {
        let pot = p.item.patch().potion.clone();
        let bp = [pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32];
        if let (Some(b), Some("water")) = (block, pot.as_deref()) {
            // AbstractThrownPotion.onHitBlock: dowse fire
            let n = physics::face_normal(b.face);
            let ep = [b.block[0] + n[0], b.block[1] + n[1], b.block[2] + n[2]];
            let mut cells = vec![ep, [ep[0] - n[0], ep[1] - n[1], ep[2] - n[2]]];
            for d in [[1, 0, 0], [-1, 0, 0], [0, 0, 1], [0, 0, -1]] {
                cells.push([ep[0] + d[0], ep[1], ep[2] + d[2]]);
            }
            for c in cells {
                if let Some(s) = self.world.get_block(c[0], c[1], c[2]) {
                    if matches!(s.name(), "fire" | "soul_fire") {
                        self.player_change_block(c, crate::registry::blocks::AIR, false, None);
                    }
                }
            }
        }
        let effects = pot.as_deref().map(potion_effects).unwrap_or(&[]).to_vec();
        let (w, h) = p.ty.size();
        let pbox = Aabb::entity(pos, w, h);
        if pot.as_deref() == Some("water") {
            // onHitAsWater
            for (t, tp) in self.living_in(&pbox.inflate(4.0, 2.0, 4.0)) {
                let d = (tp[0] - pos[0]).powi(2) + (tp[1] - pos[1]).powi(2) + (tp[2] - pos[2]).powi(2);
                if d < 16.0 {
                    let sensitive = self.victim_body(t).is_some_and(|b| matches!(b.info.type_name, "enderman" | "blaze" | "snow_golem" | "strider"));
                    if sensitive {
                        let me = EntityRef::new(eid, p.ty.entity_name(), pos, false);
                        self.hurt_entity(t, &DamageSource::indirect("indirect_magic", me, owner), 1.0);
                    }
                    self.extinguish(t);
                }
            }
        } else if !effects.is_empty() {
            if p.ty == PType::LingeringPotion {
                let cloud = Cloud {
                    owner: p.owner,
                    potion: pot.clone(),
                    scale: p.item.defaults().raw.get("potion_duration_scale").and_then(|s| s.as_f64()).unwrap_or(1.0) as f32,
                    radius: 3.0,
                    radius_on_use: -0.5,
                    radius_per_tick: -3.0 / 600.0,
                    duration: 600,
                    wait_time: 10,
                    reapplication_delay: 20,
                    victims: Vec::new(),
                    color: pot.as_deref().map(potion_color).unwrap_or(-1),
                };
                let e = Entity { eid: self.next_eid(), uuid: uuid::Uuid::new_v4(), kind: EKind::Cloud(Box::new(cloud)), pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos };
                self.add_entity(e);
            } else {
                // ThrownSplashPotion.onHitAsPotion
                let m = 0.0f64.max(0.3f64.min((0.0 - 2.0) / 20.0));
                for (t, _) in self.living_in(&pbox.inflate(4.0, 2.0, 4.0)) {
                    let Some(tb) = self.entity_box(t) else { continue };
                    let dist = pbox.distance_sqr_box(&tb.inflate(m, m, m));
                    if dist >= 16.0 {
                        continue;
                    }
                    let scale = 1.0 - dist.sqrt() / 4.0;
                    let effects = effects.clone();
                    self.with_living(t, move |l, eq, body| {
                        let mut rng = rand::thread_rng();
                        for (e, dur, amp) in effects {
                            if e.is_instant() {
                                l.apply_instant(eq, body, e, amp, scale, None, &mut rng);
                            } else {
                                let d = (scale * dur as f64 + 0.5) as i32;
                                if d > 20 {
                                    l.add_effect(&body.info, Instance::new(e, d, amp));
                                }
                            }
                        }
                    });
                }
            }
        }
        let instant = effects.iter().any(|e| e.0.is_instant());
        let color = pot.as_deref().map(potion_color).unwrap_or(-1);
        self.level_event(if instant { 2007 } else { 2002 }, bp, color);
        let _ = rng;
    }

    /// A player picks up a stuck arrow / trident.
    fn pickup_projectile(&self, eid: i32, player: i32) {
        let Some(pl) = self.players().into_iter().find(|p| p.eid == player) else { return };
        let (item, pickup, chunk) = {
            let ents = self.entities.lock();
            let Some(e) = ents.get(&eid) else { return };
            let EKind::Projectile(p) = &e.kind else { return };
            (p.item, p.pickup, e.chunk())
        };
        let creative = pl.gamemode.load(Ordering::Relaxed) == 1;
        let taken = match pickup {
            Pickup::CreativeOnly => creative,
            _ => {
                let mut inv = pl.inv.lock();
                let held = pl.held.load(Ordering::Relaxed) as i16;
                inv.add(item, held) == 0
            }
        };
        if !taken {
            return;
        }
        if self.entities.lock().remove(&eid).is_none() {
            return;
        }
        let mut b = Vec::new();
        b.vi(eid).vi(player).vi(1);
        self.broadcast_chunk(chunk, &self.encode("take_item_entity", &b));
        self.broadcast_chunk(chunk, &self.remove_packet(&[eid]));
        self.flush_inventory(&pl);
    }

    /// `ExperienceOrb.award`: RimeMC has no orb entities yet, so the
    /// experience goes straight to the nearest player within 8 blocks.
    pub fn award_xp_orbs(&self, pos: [f64; 3], amount: i32) {
        let mut best: Option<(f64, Arc<Player>)> = None;
        for p in self.players() {
            let pp = p.pose.lock().pos;
            let d = (pp[0] - pos[0]).powi(2) + (pp[1] - pos[1]).powi(2) + (pp[2] - pos[2]).powi(2);
            if d < 64.0 && best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, p));
            }
        }
        if let Some((_, p)) = best {
            p.give_xp(amount);
            let mut rng = rand::thread_rng();
            self.play_sound("minecraft:entity.experience_orb.pickup", 7, p.pose.lock().pos, 0.1, (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.35 + 0.9);
        }
    }

    pub fn level_event(&self, ev: i32, pos: [i32; 3], data: i32) {
        let mut b = Vec::new();
        b.i32(ev).pos(pos[0], pos[1], pos[2]).i32(data).bool(false);
        let c = (pos[0] >> 4, pos[2] >> 4);
        self.broadcast_chunk(c, &self.encode("level_event", &b));
    }

    /// `Entity.extinguishFire`
    pub fn extinguish(&self, eid: i32) {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            let mut v = p.vit.lock();
            if v.fire > 0 {
                v.fire = 0;
                v.data_dirty = true;
            }
            return;
        }
        if let Some(e) = self.entities.lock().get_mut(&eid) {
            if let EKind::Mob(m) = &mut e.kind {
                m.fire = 0;
            }
        }
    }

    /// A player's bow / crossbow / trident / thrown item: spawns the projectile.
    pub fn throw_item(&self, p: &Arc<Player>, stack: Stack) -> Option<PType> {
        let name = Items::get().name(stack.item);
        let ty = PType::for_thrown(name)?;
        let pose = *p.pose.lock();
        let mut rng = rand::thread_rng();
        let (offset, pow) = match ty {
            PType::SplashPotion | PType::LingeringPotion => (-20.0, 0.5),
            PType::XpBottle => (-20.0, 0.7),
            _ => (0.0, 1.5),
        };
        let mut vel = shoot_vector(pose.pitch, pose.yaw, offset, pow, 1.0, &mut rng);
        let motion = *p.motion.lock();
        let on_ground = pose.on_ground;
        vel = [vel[0] + motion[0], vel[1] + if on_ground { 0.0 } else { motion[1] }, vel[2] + motion[2]];
        let eye = [pose.pos[0], pose.pos[1] + 1.62 - 0.1, pose.pos[2]];
        let mut one = stack;
        one.count = 1;
        self.launch(Projectile::new(ty, Some(p.eid), one), eye, vel);
        let sound = match ty {
            PType::Snowball => "minecraft:entity.snowball.throw",
            PType::Egg => "minecraft:entity.egg.throw",
            PType::EnderPearl => "minecraft:entity.ender_pearl.throw",
            PType::SplashPotion => "minecraft:entity.splash_potion.throw",
            PType::LingeringPotion => "minecraft:entity.lingering_potion.throw",
            _ => "minecraft:entity.experience_bottle.throw",
        };
        self.play_sound(sound, 6, pose.pos, 0.5, 0.4 / (rng.r#gen::<f32>() * 0.4 + 0.8));
        Some(ty)
    }

    /// `ProjectileWeaponItem.shoot` for a player's bow (`power` already ×3)
    /// or crossbow.
    #[allow(clippy::too_many_arguments)]
    pub fn shoot_arrows(&self, shooter: i32, eye: [f64; 3], yaw: f32, pitch: f32, motion: [f64; 3], on_ground: bool, weapon: Stack, ammo: &[Stack], power: f32, uncertainty: f32, crit: bool, creative: bool, infinite: bool) {
        let mut rng = rand::thread_rng();
        let max_angle = enchant_value(weapon, "projectile_spread", None, None, 0.0, &mut rng);
        let n = ammo.len();
        let step = if n == 1 { 0.0 } else { 2.0 * max_angle / (n as f32 - 1.0) };
        let offset = ((n as i32 - 1) % 2) as f32 * step / 2.0;
        let mut dir = 1.0f32;
        for (i, a) in ammo.iter().enumerate() {
            let angle = offset + dir * ((i + 1) / 2) as f32 * step;
            dir = -dir;
            let name = Items::get().name(a.item);
            let ty = match name {
                "spectral_arrow" => PType::SpectralArrow,
                "trident" => PType::Trident,
                _ => PType::Arrow,
            };
            let mut one = *a;
            one.count = 1;
            let mut pr = Projectile::new(ty, Some(shooter), one);
            pr.weapon = Some(weapon);
            pr.crit = crit;
            // ArrowItem.createArrow + AbstractArrow(owner, pickupItem, weapon)
            pr.pickup = if creative || infinite || i > 0 { Pickup::CreativeOnly } else { Pickup::Allowed };
            let pierce = enchant_value(weapon, "projectile_piercing", Some(*a), None, 0.0, &mut rng);
            pr.pierce = pierce.clamp(0.0, 127.0) as u8;
            if has_enchant_effect(weapon, "projectile_spawned") {
                pr.on_fire = true;
            }
            let mut vel = shoot_vector(pitch, yaw + angle, 0.0, power, uncertainty, &mut rng);
            vel = [vel[0] + motion[0], vel[1] + if on_ground { 0.0 } else { motion[1] }, vel[2] + motion[2]];
            self.launch(pr, eye, vel);
        }
    }

    /// A hurting projectile (fireball, skull, bullet, spit, wind charge)
    /// fired by `owner` from `from` along `dir`.
    pub fn mob_shoot(&self, ty: PType, owner: i32, from: [f64; 3], dir: [f64; 3], target: Option<i32>) -> i32 {
        let item = match ty {
            PType::LargeFireball | PType::SmallFireball => "fire_charge",
            _ => "air",
        };
        let mut p = Projectile::new(ty, Some(owner), Stack::new(Items::get().id(item).unwrap_or(0), 1));
        p.target = target;
        let l = len(dir).max(1e-7);
        let vel = match ty {
            PType::LlamaSpit | PType::ShulkerBullet | PType::WindCharge => dir,
            _ => [dir[0] / l * 0.1, dir[1] / l * 0.1, dir[2] / l * 0.1],
        };
        self.launch(p, from, vel)
    }

    /// `AbstractWindCharge.explode`: knocks back what is near (no block damage).
    pub fn wind_burst(&self, c: [f64; 3], owner: Option<i32>) {
        let area = Aabb { min: [c[0] - 2.4, c[1] - 2.4, c[2] - 2.4], max: [c[0] + 2.4, c[1] + 2.4, c[2] + 2.4] };
        for (eid, pos) in self.living_in(&area) {
            let d = [pos[0] - c[0], pos[1] + 0.9 - c[1], pos[2] - c[2]];
            let l = len(d).max(0.1);
            let k = (1.0 - l / 2.4).max(0.0) * 1.22;
            if k > 0.0 && Some(eid) != owner || k > 0.0 {
                self.push_entity(eid, [d[0] / l * k, d[1] / l * k + 0.1, d[2] / l * k]);
            }
        }
        self.particles("minecraft:gust_emitter_small", c, 1, [0.0; 3], 0.0);
        self.play_sound("minecraft:entity.breeze.wind_burst", 5, c, 1.0, 1.0);
    }

    /// Skeletons and other mobs shoot an arrow at a target point.
    pub fn mob_shoot_arrow(&self, owner: i32, from: [f64; 3], target: [f64; 3], pow: f32, uncertainty: f32, damage: f64) {
        let mut rng = rand::thread_rng();
        let dx = target[0] - from[0];
        let dz = target[2] - from[2];
        let hd = (dx * dx + dz * dz).sqrt();
        let dy = target[1] - from[1] + hd * 0.2;
        let vel = shoot_dir([dx, dy, dz], pow, uncertainty, &mut rng);
        let mut p = Projectile::new(PType::Arrow, Some(owner), Stack::new(Items::get().id("arrow").unwrap_or(0), 1));
        p.base_damage = damage;
        self.launch(p, from, vel);
    }
}

/// `PotionContents.getColor` for a plain potion: the averaged effect colours.
pub fn potion_color(potion: &str) -> i32 {
    let effects = potion_effects(potion);
    if effects.is_empty() {
        return -13083194;
    }
    let (mut r, mut g, mut b, mut n) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for (e, _, amp) in effects {
        let c = e.color();
        let w = (amp + 1) as f32;
        r += w * ((c >> 16) & 0xFF) as f32 / 255.0;
        g += w * ((c >> 8) & 0xFF) as f32 / 255.0;
        b += w * (c & 0xFF) as f32 / 255.0;
        n += w;
    }
    let to = |v: f32| ((v / n) * 255.0) as i32;
    (0xFF << 24) | (to(r) << 16) | (to(g) << 8) | to(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bow_power_vector() {
        let mut rng = rand::rngs::mock::StepRng::new(0, 0);
        let v = shoot_vector(0.0, 0.0, 0.0, 3.0, 0.0, &mut rng);
        assert!((v[2] - 3.0).abs() < 1e-6 && v[0].abs() < 1e-6 && v[1].abs() < 1e-6);
        let v = shoot_vector(0.0, 90.0, 0.0, 1.0, 0.0, &mut rng);
        assert!((v[0] + 1.0).abs() < 1e-6, "{v:?}");
    }
}
