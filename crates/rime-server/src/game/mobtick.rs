//! Mob and arrow simulation on the game tick: AI, spawning (natural,
//! spawners, generated structures), combat with players and explosions.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::Rng;

use super::entity::{self, Cell, EKind, Entity};
use super::mob::{self, Mob, MobType};
use super::combat::damage::DamageSource;
use super::combat::living::{Body, Event};
use super::{fluid, Game, Player};
use crate::anticheat::combat::Aabb;
use crate::data::Stack;
use crate::net::codec::Out;
use crate::registry::{blocks, Registries};
use crate::world::chunk::MIN_Y;

/// Things decided while the entity map is locked, done afterwards.
pub(super) enum Act {
    /// lightning: `thunderHit` on these entities, fire at these blocks
    Thunder { bolt: i32, cause: Option<i32>, area: Option<Aabb>, fire: Vec<[i32; 3]> },
    /// `Mob.doHurtTarget`
    MobAttack { mob: i32, target: i32 },
    /// an arrow hit a living entity
    ArrowHit { arrow: i32, owner: i32, target: i32, amount: f32, vel: [f64; 3], pos: [f64; 3] },
    /// events of a mob's own `Living` (self damage, effects)
    MobEvents { eid: i32, chunk: (i32, i32), pos: [f64; 3], events: Vec<Event> },
    Shoot { owner: i32, from: [f64; 3], vel: [f64; 3] },
    Explode { pos: [f64; 3], power: f32, by: &'static str },
    Sound { name: &'static str, pos: [f64; 3], hostile: bool },
    Loot { ty: MobType, pos: [f64; 3] },
}

const HOSTILE_CAP: usize = 35;
const CREATURE_CAP: usize = 10;

impl Game {
    /// Daytime per the world clock (`set_time` sends `ticks + 1000`).
    pub fn day_time(&self) -> u64 {
        (self.clock() + 1000) % 24000
    }

    /// World clock ticks (sent as `set_time`'s day time minus 1000).
    pub fn clock(&self) -> u64 {
        self.now.load(Ordering::Relaxed) + self.time_offset.load(Ordering::Relaxed)
    }

    pub fn set_day_time(&self, t: u64) {
        let cur = self.day_time();
        let add = (t % 24000 + 24000 - cur) % 24000;
        self.time_offset.fetch_add(add, Ordering::Relaxed);
    }

    /// Remove every mob (`/kill @e[type=!player]`); returns how many.
    pub fn kill_mobs(&self) -> usize {
        let gone: Vec<(i32, (i32, i32))> = {
            let mut ents = self.entities.lock();
            let ids: Vec<(i32, (i32, i32))> = ents.values().filter(|e| matches!(e.kind, EKind::Mob(_) | EKind::Arrow { .. } | EKind::Lightning { .. })).map(|e| (e.eid, e.chunk())).collect();
            for (id, _) in &ids {
                ents.remove(id);
            }
            ids
        };
        for (eid, c) in &gone {
            self.broadcast_chunk(*c, &self.remove_packet(&[*eid]));
        }
        gone.len()
    }

    pub fn is_day(&self) -> bool {
        let t = self.day_time();
        !(13000..23000).contains(&t)
    }

    fn sky_visible(&self, x: i32, y: i32, z: i32) -> bool {
        self.world.surface_y(x, z).is_some_and(|top| y > top)
    }

    /// Approximate block light at `p`: the brightest emitter minus its
    /// Manhattan distance (no occlusion).
    fn block_light(&self, p: [i32; 3]) -> u8 {
        let mut best = 0i32;
        for dx in -14i32..=14 {
            for dy in -14i32..=14 {
                let r = dx.abs() + dy.abs();
                if r > 14 {
                    continue;
                }
                for dz in -(14 - r)..=(14 - r) {
                    let Some(b) = self.world.get_block(p[0] + dx, p[1] + dy, p[2] + dz) else { continue };
                    let e = crate::world::light::emission(b) as i32;
                    if e > 0 {
                        best = best.max(e - r - dz.abs());
                        if best >= 15 {
                            return 15;
                        }
                    }
                }
            }
        }
        best.max(0) as u8
    }

    pub(super) fn mob_meta(&self, eid: i32, m: &Mob) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid);
        b.u8(0).vi(0).u8(if m.fire > 0 { 1 } else { 0 });
        if m.ty == MobType::Creeper {
            b.u8(16).vi(1).vi(m.fuse_dir);
        }
        if m.death > 0 {
            b.u8(9).vi(3).f32(0.0);
        }
        b.u8(0xFF);
        self.encode("set_entity_data", &b)
    }

    pub(super) fn mob_spawn_packets(&self, e: &Entity) -> Vec<Arc<Vec<u8>>> {
        let regs = Registries::get();
        let mut b = Vec::new();
        match &e.kind {
            EKind::Mob(m) => {
                let ty = regs.id("minecraft:entity_type", &format!("minecraft:{}", m.ty.name())).unwrap_or(0);
                b.vi(e.eid).uuid(e.uuid).vi(ty).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]);
                mob::write_lpvec(&mut b, e.vel);
                let yaw = mob::angle_byte(m.yaw);
                b.u8(0).u8(yaw).u8(yaw).vi(0);
                vec![self.encode("add_entity", &b), self.mob_meta(e.eid, m)]
            }
            EKind::Arrow { owner, .. } => {
                let ty = regs.id("minecraft:entity_type", "minecraft:arrow").unwrap_or(0);
                b.vi(e.eid).uuid(e.uuid).vi(ty).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]);
                mob::write_lpvec(&mut b, e.vel);
                let h = (e.vel[0] * e.vel[0] + e.vel[2] * e.vel[2]).sqrt();
                let yaw = (e.vel[0].atan2(e.vel[2])).to_degrees() as f32;
                let pitch = (e.vel[1].atan2(h)).to_degrees() as f32;
                b.u8(mob::angle_byte(pitch)).u8(mob::angle_byte(yaw)).u8(0).vi(*owner);
                vec![self.encode("add_entity", &b)]
            }
            EKind::Lightning { .. } => {
                let ty = regs.id("minecraft:entity_type", "minecraft:lightning_bolt").unwrap_or(0);
                b.vi(e.eid).uuid(e.uuid).vi(ty).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]).u8(0).u8(0).u8(0).u8(0).vi(0);
                vec![self.encode("add_entity", &b)]
            }
            _ => Vec::new(),
        }
    }

    /// Spawn a mob at the given feet position.
    pub fn spawn_mob(&self, ty: MobType, pos: [f64; 3], persistent: bool) -> i32 {
        let mut m = Mob::new(ty, persistent);
        m.yaw = rand::thread_rng().gen_range(0.0..360.0);
        let eid = self.next_eid();
        let e = Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Mob(Box::new(m)), pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos };
        self.add_entity(e);
        eid
    }

    fn sound(&self, name: &str, pos: [f64; 3], hostile: bool, volume: f32) {
        let Some(id) = Registries::get().id("minecraft:sound_event", name) else { return };
        let mut b = Vec::new();
        b.vi(id + 1).vi(if hostile { 5 } else { 6 });
        b.i32((pos[0] * 8.0) as i32).i32((pos[1] * 8.0) as i32).i32((pos[2] * 8.0) as i32);
        let pitch = 1.0 + (rand::thread_rng().r#gen::<f32>() - rand::thread_rng().r#gen::<f32>()) * 0.2;
        b.f32(volume).f32(pitch).i64(rand::thread_rng().r#gen());
        let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
        self.broadcast_chunk(c, &self.encode("sound", &b));
    }

    fn line_of_sight(&self, a: [f64; 3], b: [f64; 3]) -> bool {
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let steps = (len / 0.25).ceil() as i32;
        for i in 1..steps {
            let t = i as f64 / steps as f64;
            let p = [a[0] + d[0] * t, a[1] + d[1] * t, a[2] + d[2] * t];
            if self.world.get_block(p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32).is_none_or(|b| b.is_opaque()) {
                return false;
            }
        }
        true
    }

    // ------------------------------------------------------------ tick

    pub(super) fn tick_mobs(&self) {
        let now = self.now.load(Ordering::Relaxed);
        let players = self.players();
        let day = self.is_day();
        let mut rng = rand::thread_rng();
        let mut acts: Vec<Act> = Vec::new();
        let mut sends: Vec<((i32, i32), Arc<Vec<u8>>)> = Vec::new();
        let mut direct: Vec<(Arc<Player>, Arc<Vec<u8>>)> = Vec::new();
        let mut removed: Vec<(i32, (i32, i32))> = Vec::new();
        // player positions (alive, not spectating)
        let targets: Vec<(Arc<Player>, [f64; 3], i32)> = players
            .iter()
            .filter(|p| !p.vit.lock().dead())
            .map(|p| (p.clone(), p.pose.lock().pos, p.gamemode.load(Ordering::Relaxed)))
            .collect();
        {
            let mut ents = self.entities.lock();
            for e in ents.values_mut() {
                if !matches!(e.kind, EKind::Mob(_) | EKind::Arrow { .. } | EKind::Lightning { .. }) {
                    continue;
                }
                let old_chunk = e.chunk();
                if !self.world.get_block(e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32).is_some() && e.pos[1] >= MIN_Y as f64 {
                    continue; // chunk unloaded: frozen
                }
                e.age += 1;
                let gone = match e.kind {
                    EKind::Mob(_) => self.tick_mob(e, &targets, day, now, &mut rng, &mut acts, &mut sends),
                    EKind::Arrow { .. } => self.tick_arrow(e, &targets, &mut acts),
                    EKind::Lightning { .. } => self.tick_lightning(e, &mut rng, &mut acts),
                    _ => false,
                };
                if gone {
                    removed.push((e.eid, old_chunk));
                    continue;
                }
                let new_chunk = e.chunk();
                if new_chunk != old_chunk {
                    let spawn = self.mob_spawn_packets(e);
                    let rm = self.remove_packet(&[e.eid]);
                    for p in &players {
                        let (a, b) = (p.sees_chunk(old_chunk.0, old_chunk.1), p.sees_chunk(new_chunk.0, new_chunk.1));
                        if a && !b {
                            direct.push((p.clone(), rm.clone()));
                        } else if !a && b {
                            for s in &spawn {
                                direct.push((p.clone(), s.clone()));
                            }
                        }
                    }
                }
                let moved = (0..3).any(|i| (e.pos[i] - e.synced[i]).abs() > 1.0 / 4096.0);
                if moved {
                    e.synced = e.pos;
                    let (yaw, pitch) = match &e.kind {
                        EKind::Mob(m) => (m.yaw, 0.0),
                        _ => {
                            let h = (e.vel[0] * e.vel[0] + e.vel[2] * e.vel[2]).sqrt();
                            (e.vel[0].atan2(e.vel[2]).to_degrees() as f32, e.vel[1].atan2(h).to_degrees() as f32)
                        }
                    };
                    let mut b = Vec::new();
                    b.vi(e.eid).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]).f64(e.vel[0]).f64(e.vel[1]).f64(e.vel[2]).f32(yaw).f32(pitch).bool(e.on_ground);
                    sends.push((new_chunk, self.encode("entity_position_sync", &b)));
                }
                if let EKind::Mob(m) = &mut e.kind {
                    if (m.yaw - m.head_yaw_sent).abs() > 2.0 {
                        m.head_yaw_sent = m.yaw;
                        let mut b = Vec::new();
                        b.vi(e.eid).u8(mob::angle_byte(m.yaw));
                        sends.push((new_chunk, self.encode("rotate_head", &b)));
                    }
                }
            }
            for (eid, _) in &removed {
                ents.remove(eid);
            }
        }
        for (c, pkt) in sends {
            self.broadcast_chunk(c, &pkt);
        }
        for (p, pkt) in direct {
            p.send(pkt);
        }
        for (eid, c) in removed {
            self.broadcast_chunk(c, &self.remove_packet(&[eid]));
        }
        for a in acts {
            match a {
                Act::MobAttack { mob, target } => {
                    self.mob_attack(mob, target);
                }
                Act::ArrowHit { arrow, owner, target, amount, vel, pos } => {
                    let aref = crate::game::combat::damage::EntityRef::new(arrow, "arrow", pos, false);
                    let mut src = DamageSource::indirect("arrow", aref, self.entity_ref(owner));
                    src.projectile = true;
                    src.projectile_motion = Some(vel);
                    if self.hurt_entity(target, &src, amount) {
                        self.post_attack(target, &src, None);
                    }
                }
                Act::MobEvents { eid, chunk, pos, events } => self.mob_events(eid, chunk, pos, events),
                Act::Thunder { bolt, cause, area, fire } => self.thunder(bolt, cause, area, fire),
                Act::Shoot { owner, from, vel } => {
                    let e = Entity { eid: self.next_eid(), uuid: uuid::Uuid::new_v4(), kind: EKind::Arrow { owner, damage: 2.0, stuck: 0 }, pos: from, vel, on_ground: false, age: 0, synced: from };
                    self.add_entity(e);
                    self.sound("minecraft:entity.skeleton.shoot", from, true, 1.0);
                }
                Act::Explode { pos, power, by } => self.explode(pos, power, by),
                Act::Sound { name, pos, hostile } => self.sound(name, pos, hostile, 1.0),
                Act::Loot { ty, pos } => self.mob_loot(ty, pos),
            }
        }
        if now % 10 == 0 {
            self.natural_spawns(&targets, now, day);
        }
        if now % 20 == 0 {
            self.spawn_generated(&targets);
            self.tick_spawners(&targets);
        }
    }

    /// One mob tick; true when the mob is removed.
    #[allow(clippy::too_many_arguments)]
    fn tick_mob(&self, e: &mut Entity, targets: &[(Arc<Player>, [f64; 3], i32)], day: bool, now: u64, rng: &mut impl Rng, acts: &mut Vec<Act>, sends: &mut Vec<((i32, i32), Arc<Vec<u8>>)>) -> bool {
        let eid_ = e.eid;
        let EKind::Mob(m) = &mut e.kind else { return true };
        let ty = m.ty;
        if m.death > 0 {
            m.death += 1;
            return m.death > 20;
        }
        m.living.tick_counters();
        m.attack_cd = m.attack_cd.saturating_sub(1);
        m.panic = m.panic.saturating_sub(1);
        let (w, h) = ty.size();
        let feet = [e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32];
        if e.pos[1] < (MIN_Y - 64) as f64 {
            return true;
        }
        // despawning
        let nearest = targets.iter().map(|(_, p, _)| dist2(*p, e.pos)).fold(f64::INFINITY, f64::min);
        if !m.persistent {
            if nearest > 128.0 * 128.0 {
                return true;
            }
            if nearest > 32.0 * 32.0 && rng.gen_range(0..800) == 0 {
                return true;
            }
        }
        let fire_before = m.fire > 0;
        let feet_block = self.world.get_block(feet[0], feet[1], feet[2]).unwrap_or(blocks::AIR);
        let in_water = fluid::fluid_state(feet_block).fluid() == Some(fluid::Fluid::Water);
        let in_lava = feet_block.name() == "lava";
        let mut self_damage: Vec<(DamageSource, f32)> = Vec::new();
        if in_water {
            m.fire = 0;
        } else if in_lava {
            m.fire = 300;
            if now % 10 == 0 {
                self_damage.push((DamageSource::new("lava"), 4.0));
            }
        } else if ty.burns() && day && self.sky_visible(feet[0], feet[1] + 1, feet[2]) && rng.gen_range(0..20) == 0 {
            m.fire = m.fire.max(160);
        }
        if m.fire > 0 {
            m.fire -= 1;
            if m.fire % 20 == 0 {
                self_damage.push((DamageSource::new("on_fire"), 1.0));
            }
        }
        // targeting
        let hostile_now = ty.hostile() && !(matches!(ty, MobType::Spider) && day && self.sky_visible(feet[0], feet[1] + 1, feet[2]));
        if m.target.is_some_and(|t| !targets.iter().any(|(p, pos, gm)| p.eid == t && (*gm == 0 || *gm == 2) && dist2(*pos, e.pos) < 32.0 * 32.0)) {
            m.target = None;
        }
        let eye = [e.pos[0], e.pos[1] + h * 0.85, e.pos[2]];
        if hostile_now && m.target.is_none() && e.age % 10 == 0 {
            let mut best: Option<(f64, i32)> = None;
            for (p, pos, gm) in targets {
                if *gm != 0 && *gm != 2 {
                    continue;
                }
                let d = dist2(*pos, e.pos);
                if d < 16.0 * 16.0 && best.is_none_or(|b| d < b.0) && self.line_of_sight(eye, [pos[0], pos[1] + 1.62, pos[2]]) {
                    best = Some((d, p.eid));
                }
            }
            m.target = best.map(|b| b.1);
        }
        let target = m.target.and_then(|t| targets.iter().find(|(p, _, _)| p.eid == t)).cloned();
        // decide where to go
        let mut goal: Option<([f64; 3], f64)> = None;
        if let Some((tp, tpos, _)) = &target {
            let d = dist2(*tpos, e.pos).sqrt();
            m.yaw = yaw_to(e.pos, *tpos);
            match ty {
                MobType::Skeleton => {
                    let sees = self.line_of_sight(eye, [tpos[0], tpos[1] + 1.62, tpos[2]]);
                    if d > 12.0 || !sees {
                        goal = Some((*tpos, 1.0));
                    }
                    if sees && d < 15.0 && m.attack_cd == 0 {
                        m.attack_cd = 40;
                        let from = [e.pos[0], eye[1] - 0.1, e.pos[2]];
                        let dx = tpos[0] - from[0];
                        let dz = tpos[2] - from[2];
                        let hd = (dx * dx + dz * dz).sqrt();
                        let dy = tpos[1] + 1.8 / 3.0 - from[1] + hd * 0.2;
                        let len = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-6);
                        let spread = || rng_gauss(&mut rand::thread_rng()) * 0.0075 * 6.0;
                        let vel = [(dx / len + spread()) * 1.6, (dy / len + spread()) * 1.6, (dz / len + spread()) * 1.6];
                        acts.push(Act::Shoot { owner: e.eid, from, vel });
                    }
                }
                MobType::Creeper => {
                    if d < 3.0 && m.fuse_dir < 0 {
                        m.fuse_dir = 1;
                        acts.push(Act::Sound { name: "minecraft:entity.creeper.primed", pos: e.pos, hostile: true });
                        sends.push((chunk_of(e.pos), self.mob_meta(eid_, m)));
                    } else if d > 7.0 && m.fuse_dir > 0 {
                        m.fuse_dir = -1;
                        sends.push((chunk_of(e.pos), self.mob_meta(eid_, m)));
                    }
                    if m.fuse_dir < 0 {
                        goal = Some((*tpos, 1.0));
                    }
                }
                _ => {
                    goal = Some((*tpos, 1.0));
                    // melee reach: (2 × width)² + target width
                    let reach = (w * 2.0) * (w * 2.0) + 0.6;
                    let dy = (tpos[1] - e.pos[1]).abs();
                    if dist2(*tpos, e.pos) <= reach && dy < h.max(1.8) && m.attack_cd == 0 && ty.attack() > 0.0 {
                        m.attack_cd = 20;
                        acts.push(Act::MobAttack { mob: e.eid, target: tp.eid });
                    }
                }
            }
        } else if m.panic > 0 {
            if m.wander.is_none() || e.age % 20 == 0 {
                let away = norm2([e.pos[0] - m.flee_from[0], e.pos[2] - m.flee_from[2]]);
                let jitter = rng.gen_range(-0.6..0.6);
                let (s, c) = (jitter as f64).sin_cos();
                let dir = [away[0] * c - away[1] * s, away[0] * s + away[1] * c];
                m.wander = Some([e.pos[0] + dir[0] * 8.0, e.pos[1], e.pos[2] + dir[1] * 8.0]);
            }
            goal = m.wander.map(|g| (g, 1.6));
        } else {
            if m.wander.is_none() && rng.gen_range(0..120) == 0 {
                m.wander = Some([e.pos[0] + rng.gen_range(-10.0..10.0), e.pos[1], e.pos[2] + rng.gen_range(-10.0..10.0)]);
            }
            if let Some(g) = m.wander {
                if dist2h(g, e.pos) < 1.0 || e.age % 200 == 0 {
                    m.wander = None;
                } else {
                    goal = Some((g, if ty == MobType::Creeper { 0.8 } else { 1.0 }));
                }
            }
        }
        // creeper fuse
        if ty == MobType::Creeper {
            m.fuse = (m.fuse + m.fuse_dir).clamp(0, 30);
            if m.fuse >= 30 {
                acts.push(Act::Explode { pos: e.pos, power: 3.0, by: ty.display() });
                return true;
            }
        }
        // steering
        let mut want = [0.0, 0.0];
        if let Some((g, modifier)) = goal {
            let d = [g[0] - e.pos[0], g[2] - e.pos[2]];
            let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
            if len > 0.2 {
                let s = ty.speed() * modifier;
                let v = (2.92 * s * s).min(0.3);
                want = [d[0] / len * v, d[1] / len * v];
                m.yaw = yaw_to(e.pos, g);
                // do not walk off cliffs while idle
                if target.is_none() && m.panic == 0 {
                    let ahead = [e.pos[0] + d[0] / len * (w / 2.0 + 0.5), e.pos[2] + d[1] / len * (w / 2.0 + 0.5)];
                    let (ax, az) = (ahead[0].floor() as i32, ahead[1].floor() as i32);
                    let floor = (1..=3).any(|dy| self.cell(ax, feet[1] - dy, az) == Cell::Solid);
                    let hazard = self.world.get_block(ax, feet[1], az).is_some_and(|b| matches!(b.name(), "lava" | "fire" | "cactus" | "sweet_berry_bush" | "magma_block")) || self.world.get_block(ax, feet[1] - 1, az).is_some_and(|b| matches!(b.name(), "lava" | "magma_block"));
                    if !floor || hazard {
                        want = [0.0, 0.0];
                        m.wander = None;
                    }
                }
            }
        }
        if e.on_ground {
            e.vel[0] = want[0];
            e.vel[2] = want[1];
        } else {
            e.vel[0] += (want[0] - e.vel[0]) * 0.1;
            e.vel[2] += (want[1] - e.vel[2]) * 0.1;
        }
        if in_water || in_lava {
            e.vel[1] = (e.vel[1] + 0.04).min(0.1);
            e.vel[0] *= 0.8;
            e.vel[2] *= 0.8;
        } else {
            e.vel[1] -= 0.08;
        }
        let was_ground = e.on_ground;
        let vy = e.vel[1];
        let (ground, horiz) = entity::move_entity(&mut e.pos, &mut e.vel, w / 2.0, h, &|x, y, z| self.cell(x, y, z));
        e.on_ground = ground || (was_ground && vy <= 0.0 && e.vel[1] == 0.0 && self.cell(e.pos[0].floor() as i32, (e.pos[1] - 0.05).floor() as i32, e.pos[2].floor() as i32) == Cell::Solid);
        if horiz && (want[0] != 0.0 || want[1] != 0.0) {
            if ty.climbs() {
                e.vel[1] = 0.2;
            } else if e.on_ground {
                e.vel[1] = 0.42;
            }
        }
        if ty == MobType::Chicken && e.vel[1] < 0.0 && !e.on_ground {
            e.vel[1] *= 0.6;
        }
        e.vel[1] *= 0.98;
        // falling
        if e.on_ground {
            if m.fall > 3.0 && ty != MobType::Chicken && !in_water {
                // calculateFallDamage
                let safe = m.living.attrs.value(super::combat::attr::Attr::SafeFallDistance);
                let mult = m.living.attrs.value(super::combat::attr::Attr::FallDamageMultiplier);
                let d = ((m.fall + 1.0e-6 - safe) * mult).floor();
                if d > 0.0 {
                    self_damage.push((DamageSource::new("fall"), d as f32));
                }
            }
            m.fall = 0.0;
        } else if vy < 0.0 && !in_water {
            m.fall -= vy;
        }
        if in_water {
            m.fall = 0.0;
        }
        {
            let info = super::hurt::mob_info(m, e.on_ground);
            let fire_immune = super::etypes::EntityTypes::get().by_name(ty.name()).is_some_and(|t| t.fire_immune);
            let mut body = Body { eid: eid_, info, pos: e.pos, head_yaw: m.yaw, velocity: e.vel, player: None, invulnerable: false, fire_immune, raining: false, thundering: false };
            let mut eq = m.eq;
            for (src, amount) in self_damage {
                m.living.hurt(&mut eq, &mut body, &src, amount, rng);
            }
            m.living.tick_effects(&mut eq, &mut body, rng);
            m.living.update_equipment(&eq);
            m.eq = eq;
            e.vel = body.velocity;
            let events = std::mem::take(&mut m.living.events);
            if !events.is_empty() {
                acts.push(Act::MobEvents { eid: eid_, chunk: chunk_of(e.pos), pos: e.pos, events });
            }
        }
        if fire_before != (m.fire > 0) {
            sends.push((chunk_of(e.pos), self.mob_meta(eid_, m)));
        }
        // ambient sounds
        m.ambient += 1;
        if m.ambient > 80 && rng.gen_range(0..1000) < m.ambient && !ty.ambient_sound().is_empty() && nearest < 24.0 * 24.0 {
            m.ambient = -80;
            acts.push(Act::Sound { name: ty.ambient_sound(), pos: e.pos, hostile: ty.hostile() });
        }
        false
    }

    /// One arrow tick; true when the arrow is removed.
    fn tick_arrow(&self, e: &mut Entity, targets: &[(Arc<Player>, [f64; 3], i32)], acts: &mut Vec<Act>) -> bool {
        let EKind::Arrow { owner, damage, stuck } = &mut e.kind else { return true };
        if *stuck > 0 {
            *stuck += 1;
            return *stuck > 1200;
        }
        if e.age > 1200 || e.pos[1] < (MIN_Y - 64) as f64 {
            return true;
        }
        let speed = (e.vel[0] * e.vel[0] + e.vel[1] * e.vel[1] + e.vel[2] * e.vel[2]).sqrt();
        let steps = (speed / 0.2).ceil().max(1.0) as i32;
        for _ in 0..steps {
            let next = [e.pos[0] + e.vel[0] / steps as f64, e.pos[1] + e.vel[1] / steps as f64, e.pos[2] + e.vel[2] / steps as f64];
            for (p, pos, gm) in targets {
                if p.eid == *owner && e.age < 5 || *gm == 3 {
                    continue;
                }
                let bx = Aabb::entity(*pos, 0.6 + 0.3, 1.8 + 0.3);
                if bx.distance_to(next) == 0.0 {
                    let amount = (speed * *damage).clamp(0.0, i32::MAX as f64).ceil() as f32;
                    acts.push(Act::ArrowHit { arrow: e.eid, owner: *owner, target: p.eid, amount, vel: e.vel, pos: next });
                    return true;
                }
            }
            if self.cell(next[0].floor() as i32, next[1].floor() as i32, next[2].floor() as i32) == Cell::Solid {
                *stuck = 1;
                e.vel = [0.0; 3];
                return false;
            }
            e.pos = next;
        }
        e.vel[0] *= 0.99;
        e.vel[1] = e.vel[1] * 0.99 - 0.05;
        e.vel[2] *= 0.99;
        false
    }

    pub(super) fn mob_loot(&self, ty: MobType, pos: [f64; 3]) {
        let mut rng = rand::thread_rng();
        for s in self.data.roll_loot(&ty.loot_table(), &mut rng) {
            self.spawn_item([pos[0], pos[1] + 0.5, pos[2]], s, [rng.gen_range(-0.1..0.1), 0.2, rng.gen_range(-0.1..0.1)], 10);
        }
    }

    // ------------------------------------------------------------ combat

    /// The box of an entity (players included), for reach checks.
    pub fn entity_box(&self, eid: i32) -> Option<Aabb> {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return Some(Aabb::entity(p.pose.lock().pos, 0.6, 1.8));
        }
        let ents = self.entities.lock();
        let e = ents.get(&eid)?;
        let (half, h) = e.size();
        Some(Aabb::entity(e.pos, half * 2.0, h))
    }

    /// Explosion: destroys blocks (1/power of them drop), hurts players and
    /// mobs, and shows the blast.
    pub fn explode(&self, c: [f64; 3], power: f32, by: &'static str) {
        let mut rng = rand::thread_rng();
        let centre = [c[0], c[1] + 0.06125 * 2.0, c[2]];
        let hit = mob::explosion_blocks(centre, power, &|x, y, z| self.world.get_block(x, y, z), &mut rng);
        let n = hit.len();
        for p in &hit {
            let drops = rng.r#gen::<f32>() < 1.0 / power;
            self.change_block(*p, blocks::AIR, drops, None);
        }
        for p in self.players() {
            let pos = p.pose.lock().pos;
            let d = dist2([pos[0], pos[1] + 0.9, pos[2]], centre).sqrt();
            let dmg = mob::explosion_damage(power, d);
            if dmg > 0.0 {
                let src = DamageSource::at("explosion", c);
                self.hurt_player(&p, &src, dmg);
                let dir = norm2([pos[0] - c[0], pos[2] - c[2]]);
                let impact = 1.0 - d / (power as f64 * 2.0);
                let kr = p.vit.lock().living.attrs.value(super::combat::attr::Attr::ExplosionKnockbackResistance);
                let k = impact * (1.0 - kr);
                if k > 0.0 {
                    self.push_entity(p.eid, [dir[0] * k, k, dir[1] * k]);
                }
            }
        }
        let victims: Vec<(i32, f32)> = self
            .entities
            .lock()
            .values()
            .filter(|e| matches!(e.kind, EKind::Mob(_)))
            .filter_map(|e| {
                let d = dist2(e.pos, centre).sqrt();
                let dmg = mob::explosion_damage(power, d);
                (dmg > 0.0).then_some((e.eid, dmg))
            })
            .collect();
        for (eid, dmg) in victims {
            self.hurt_mob(eid, &DamageSource::at("explosion", c), dmg);
        }
        // the blast itself
        let regs = Registries::get();
        let mut b = Vec::new();
        b.f64(c[0]).f64(c[1]).f64(c[2]).f32(power).i32(n as i32).bool(false);
        let particle = if power >= 2.0 { "minecraft:explosion_emitter" } else { "minecraft:explosion" };
        b.vi(regs.id("minecraft:particle_type", particle).unwrap_or(0));
        b.vi(regs.id("minecraft:sound_event", "minecraft:entity.generic.explode").map(|i| i + 1).unwrap_or(0));
        b.vi(0);
        let chunk = ((c[0].floor() as i32) >> 4, (c[2].floor() as i32) >> 4);
        self.broadcast_chunk(chunk, &self.encode("explode", &b));
    }

    // ------------------------------------------------------------ spawning

    fn count_near(&self, pos: [f64; 3], hostile: bool) -> usize {
        self.entities.lock().values().filter(|e| matches!(&e.kind, EKind::Mob(m) if m.ty.hostile() == hostile && m.death == 0) && dist2(e.pos, pos) < 128.0 * 128.0).count()
    }

    /// Can a mob of this size stand with its feet in block `p`?
    fn fits(&self, p: [i32; 3], h: f64) -> bool {
        if self.cell(p[0], p[1] - 1, p[2]) != Cell::Solid {
            return false;
        }
        let below = self.world.get_block(p[0], p[1] - 1, p[2]).map(|b| b.name()).unwrap_or("");
        if matches!(below, "bedrock" | "barrier" | "magma_block") {
            return false;
        }
        (0..h.ceil() as i32).all(|dy| self.world.get_block(p[0], p[1] + dy, p[2]).is_some_and(|b| !b.is_solid() && fluid::fluid_state(b).fluid().is_none() && !matches!(b.name(), "fire" | "lava" | "powder_snow" | "sweet_berry_bush" | "cactus")))
    }

    fn natural_spawns(&self, targets: &[(Arc<Player>, [f64; 3], i32)], now: u64, day: bool) {
        let mut rng = rand::thread_rng();
        for (_, pos, gm) in targets {
            if *gm == 3 {
                continue;
            }
            // monsters: every half second, one pack attempt
            if self.count_near(*pos, true) < HOSTILE_CAP {
                self.try_pack(*pos, targets, true, day, &mut rng);
            }
            // animals: rarely (vanilla only spawns persistent creatures every 400 ticks)
            if now % 400 == 0 && self.count_near(*pos, false) < CREATURE_CAP {
                self.try_pack(*pos, targets, false, day, &mut rng);
            }
        }
    }

    fn try_pack(&self, around: [f64; 3], targets: &[(Arc<Player>, [f64; 3], i32)], hostile: bool, day: bool, rng: &mut impl Rng) {
        let ang = rng.gen_range(0.0..std::f64::consts::TAU);
        let r = rng.gen_range(24.0..64.0);
        let x = (around[0] + ang.cos() * r).floor() as i32;
        let z = (around[2] + ang.sin() * r).floor() as i32;
        let Some(top) = self.world.surface_y(x, z) else { return };
        let y = if hostile { rng.gen_range(MIN_Y..=top + 1) } else { top + 1 };
        let Some(biome) = self.world.biome(x, y, z) else { return };
        let list = if hostile { mob::monsters(&biome) } else { mob::creatures(&biome) };
        let Some(ty) = mob::pick(list, rng) else { return };
        let (_, h) = ty.size();
        let count = if hostile { rng.gen_range(1..=4) } else { 4 };
        let mut spawned = 0;
        let (mut px, mut pz) = (x, z);
        for _ in 0..count * 3 {
            if spawned >= count {
                break;
            }
            px += rng.gen_range(-5..=5);
            pz += rng.gen_range(-5..=5);
            let py = if hostile { y } else { match self.world.surface_y(px, pz) { Some(t) => t + 1, None => continue } };
            let p = [px, py, pz];
            let feet = [px as f64 + 0.5, py as f64, pz as f64 + 0.5];
            if targets.iter().any(|(_, q, _)| dist2(*q, feet) < 24.0 * 24.0) || !self.fits(p, h) {
                continue;
            }
            if hostile {
                // darkness: no block light, and no daylight
                let sky = self.sky_visible(px, py, pz);
                if sky && (day || rng.gen_range(0..8) < 4) {
                    continue;
                }
                if self.block_light(p) > 0 {
                    continue;
                }
            } else {
                let below = self.world.get_block(px, py - 1, pz).map(|b| b.name()).unwrap_or("");
                if below != "grass_block" || !self.sky_visible(px, py, pz) {
                    continue;
                }
            }
            self.spawn_mob(ty, feet, false);
            spawned += 1;
        }
    }

    /// Entities world generation placed in chunks near players.
    fn spawn_generated(&self, targets: &[(Arc<Player>, [f64; 3], i32)]) {
        for (_, pos, _) in targets {
            let (cx, cz) = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
            for dx in -3..=3 {
                for dz in -3..=3 {
                    for (name, p) in self.world.take_gen_entities(cx + dx, cz + dz) {
                        if let Some(ty) = MobType::from_name(&name) {
                            self.spawn_mob(ty, [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5], true);
                        }
                    }
                }
            }
        }
    }

    /// `BaseSpawner`: spawns while a player is within 16 blocks.
    fn tick_spawners(&self, targets: &[(Arc<Player>, [f64; 3], i32)]) {
        let mut rng = rand::thread_rng();
        let mut seen = std::collections::HashSet::new();
        for (_, pos, gm) in targets {
            if *gm == 3 {
                continue;
            }
            let (cx, cz) = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
            for dx in -2..=2 {
                for dz in -2..=2 {
                    for (p, mut t) in self.world.spawners(cx + dx, cz + dz) {
                        if !seen.insert(p) {
                            continue;
                        }
                        let c = [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5];
                        if !targets.iter().any(|(_, q, g)| *g != 3 && dist2(*q, c) < 16.0 * 16.0) {
                            continue;
                        }
                        let mob_name = t.get("SpawnData").and_then(|d| d.get("entity")).and_then(|e| e.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let Some(ty) = MobType::from_name(&mob_name) else { continue };
                        let delay = t.get("Delay").and_then(|v| v.as_i64()).unwrap_or(20) as i32 - 20;
                        let next = if delay > 0 {
                            delay
                        } else {
                            let nearby = self.entities.lock().values().filter(|e| matches!(&e.kind, EKind::Mob(m) if m.ty == ty) && (e.pos[0] - c[0]).abs() < 9.0 && (e.pos[1] - c[1]).abs() < 5.0 && (e.pos[2] - c[2]).abs() < 9.0).count();
                            if nearby < 6 {
                                let (_, h) = ty.size();
                                let mut n = 0;
                                for _ in 0..8 {
                                    if n >= 4 {
                                        break;
                                    }
                                    let q = [p[0] + rng.gen_range(-4..=4), p[1] + rng.gen_range(-1..=1), p[2] + rng.gen_range(-4..=4)];
                                    if self.fits(q, h) && self.block_light(q) <= 11 {
                                        self.spawn_mob(ty, [q[0] as f64 + 0.5, q[1] as f64, q[2] as f64 + 0.5], false);
                                        n += 1;
                                    }
                                }
                            }
                            rng.gen_range(200..800)
                        };
                        if let crate::nbt::Tag::Compound(c) = &mut t {
                            match c.iter_mut().find(|(k, _)| k == "Delay") {
                                Some(e) => e.1 = crate::nbt::Tag::Short(next as i16),
                                None => c.push(("Delay".into(), crate::nbt::Tag::Short(next as i16))),
                            }
                        }
                        self.world.set_block_entity(p, Some(t), false);
                    }
                }
            }
        }
    }

    /// Persistent mobs (animals, structure mobs) go back into their chunks
    /// before those are saved: all of them (`all`), or those nobody views.
    pub fn stash_mobs(&self, all: bool) {
        let mut out = Vec::new();
        {
            let mut ents = self.entities.lock();
            ents.retain(|_, e| {
                let EKind::Mob(m) = &e.kind else { return true };
                let c = e.chunk();
                if !m.persistent || m.death > 0 || !(all || !self.world.viewed(c.0, c.1)) {
                    return true;
                }
                let p = [e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32];
                if self.world.stash_entity(m.ty.name(), p) {
                    out.push(e.eid);
                    false
                } else {
                    true
                }
            });
        }
        let _ = out;
    }

    /// `/summon`-style spawn next to a position; None for unknown types.
    pub fn summon(&self, name: &str, pos: [f64; 3]) -> Option<i32> {
        let ty = MobType::from_name(name)?;
        Some(self.spawn_mob(ty, pos, true))
    }

    pub fn mob_count(&self) -> usize {
        self.entities.lock().values().filter(|e| matches!(e.kind, EKind::Mob(_))).count()
    }

    /// Loot of a mob type (for tests).
    pub fn roll_mob_loot(&self, ty: MobType) -> Vec<Stack> {
        self.data.roll_loot(&ty.loot_table(), &mut rand::thread_rng())
    }
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

fn dist2h(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)
}

fn norm2(v: [f64; 2]) -> [f64; 2] {
    let l = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if l < 1e-6 {
        [0.0, 0.0]
    } else {
        [v[0] / l, v[1] / l]
    }
}

fn yaw_to(from: [f64; 3], to: [f64; 3]) -> f32 {
    ((to[2] - from[2]).atan2(to[0] - from[0]).to_degrees() - 90.0) as f32
}

fn rng_gauss(rng: &mut impl Rng) -> f64 {
    // Box-Muller
    let u1: f64 = rng.gen_range(1e-9..1.0);
    let u2: f64 = rng.r#gen();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

fn chunk_of(p: [f64; 3]) -> (i32, i32) {
    ((p[0].floor() as i32) >> 4, (p[2].floor() as i32) >> 4)
}
