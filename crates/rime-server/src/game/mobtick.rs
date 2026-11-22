//! Mob simulation on the game tick, in vanilla order: `baseTick` (air,
//! fire, blocks inside), `aiStep` (sensing, target / goal selectors,
//! navigation, `customServerAiStep`, move / look / jump controls, `travel`),
//! body rotation; natural spawning (`NaturalSpawner`), spawners, generated
//! structure mobs; explosions; deaths (loot, equipment, experience, splits).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::Rng;

use super::ai::{self, control, Ctx, Seen};
use super::combat::attr::Attr;
use super::combat::damage::DamageSource;
use super::combat::effects::{Effect, Instance};
use super::combat::living::{self, Body, Event};
use super::entity::{EKind, Entity};
use super::mob::{self, Mob, MobType};
use super::projectile::{PType, Projectile};
use super::{Game, Player};
use crate::anticheat::combat::Aabb;
use crate::data::{Items, Stack};
use crate::net::codec::Out;
use crate::registry::{blocks, vstate, BlockState, Registries};
use crate::world::chunk::MIN_Y;

const SEA_LEVEL: i32 = 63;

/// Things decided while the entity map is locked, done afterwards.
#[derive(Debug)]
pub enum Act {
    /// lightning: `thunderHit` on these entities, fire at these blocks
    Thunder { bolt: i32, cause: Option<i32>, area: Option<Aabb>, fire: Vec<[i32; 3]> },
    /// `Mob.doHurtTarget`
    MobAttack { mob: i32, target: i32 },
    /// projectile / area effect cloud work
    Proj(super::projectile::PAct),
    /// events of a mob's own `Living` (self damage, effects)
    MobEvents { eid: i32, chunk: (i32, i32), pos: [f64; 3], events: Vec<Event> },
    Explode { pos: [f64; 3], power: f32, fire: bool, by: Option<i32> },
    /// `DragonSittingFlamingPhase`: a lingering cloud of dragon's breath
    Breath { owner: i32, pos: [f64; 3] },
    Sound { name: String, pos: [f64; 3], hostile: bool, volume: f32, pitch: f32 },
    /// `ProjectileUtil.getMobArrow` + `shoot`
    Arrow { owner: i32, from: [f64; 3], target: [f64; 3], power: f32, tipped: Option<&'static str>, fire: bool, velocity: f32, uncertainty: f32 },
    Throw { owner: i32, item: &'static str, from: [f64; 3], dir: [f64; 3], velocity: f32, uncertainty: f32 },
    Spit { owner: i32, from: [f64; 3], dir: [f64; 3] },
    Trident { owner: i32, from: [f64; 3], dir: [f64; 3], uncertainty: f32 },
    Potion { owner: i32, from: [f64; 3], dir: [f64; 3], potion: String, velocity: f32, uncertainty: f32 },
    /// fireballs, skulls, bullets, wind charges (`target Some(-1)`: a dangerous skull)
    Fire { ty: PType, owner: i32, from: [f64; 3], dir: [f64; 3], target: Option<i32> },
    Breed { a: i32, b: i32 },
    SetTarget { eid: i32, target: i32 },
    Destroy { pos: [i32; 3], drops: bool },
    DestroyInfested { pos: [i32; 3], host: String },
    SetBlock { pos: [i32; 3], state: BlockState, drops: bool },
    LevelEvent { ev: i32, pos: [i32; 3], data: i32 },
    GlobalEvent { ev: i32, pos: [i32; 3] },
    Hurt { target: i32, kind: &'static str, by: Option<i32>, amount: f32 },
    Knock { target: i32, power: f64, xd: f64, zd: f64 },
    Push { target: i32, vel: [f64; 3] },
    Effect { target: i32, effect: &'static str, duration: i32, amp: i32 },
    EffectRaw { target: i32, effect: Effect, duration: i32, amp: i32 },
    Drop { pos: [f64; 3], stack: Stack },
    Remove { eid: i32 },
    SummonVex { owner: i32, pos: [i32; 3], life: i32 },
    Fang { owner: i32, pos: [f64; 3], yaw: f32, delay: i32 },
    SheepColor { eid: i32, color: u8 },
    Convert { eid: i32, to: &'static str, effect: Option<(&'static str, i32)> },
    ElderCurse { pos: [f64; 3] },
    EntityEvent { eid: i32, ev: u8 },
    SlimeHit { slime: i32, target: i32, amount: f32 },
    GameEvent { player: i32, event: u8, value: f32 },
}

/// World state shared by all mobs this tick.
struct Env {
    now: u64,
    day: bool,
    raining: bool,
    thundering: bool,
    difficulty: i32,
    /// (eid, pos, gamemode) of the players
    players: Vec<(i32, [f64; 3], i32)>,
}

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
            let ids: Vec<(i32, (i32, i32))> = ents.values().filter(|e| matches!(e.kind, EKind::Mob(_) | EKind::Projectile(_) | EKind::Cloud(_) | EKind::Lightning { .. } | EKind::Fangs { .. })).map(|e| (e.eid, e.chunk())).collect();
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

    /// `Level.isBrightOutside` without the weather part: daytime.
    pub fn is_day(&self) -> bool {
        self.sky_darken() < 4
    }

    pub(super) fn mob_meta(&self, eid: i32, m: &Mob) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid);
        ai::mobs::meta(m, self.clock() as i64, &mut b);
        b.u8(0xFF);
        self.encode("set_entity_data", &b)
    }

    fn mob_equipment(&self, eid: i32, m: &Mob) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid);
        for slot in 0..8 {
            b.u8(slot as u8 | if slot < 7 { 0x80 } else { 0 });
            super::write_slot(&mut b, m.eq[slot]);
        }
        self.encode("set_equipment", &b)
    }

    pub(super) fn mob_spawn_packets(&self, e: &Entity) -> Vec<Arc<Vec<u8>>> {
        let regs = Registries::get();
        let mut b = Vec::new();
        match &e.kind {
            EKind::Mob(m) => {
                let ty = regs.id("minecraft:entity_type", &format!("minecraft:{}", m.ty.name())).unwrap_or(0);
                b.vi(e.eid).uuid(e.uuid).vi(ty).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]);
                mob::write_lpvec(&mut b, e.vel);
                b.u8(mob::angle_byte(m.mo.pitch)).u8(mob::angle_byte(m.mo.yaw)).u8(mob::angle_byte(m.mo.head_yaw)).vi(0);
                let mut out = vec![self.encode("add_entity", &b), self.mob_meta(e.eid, m)];
                if m.eq.iter().any(|s| s.is_some()) {
                    out.push(self.mob_equipment(e.eid, m));
                }
                out
            }
            EKind::Fangs { yaw, .. } => {
                let ty = regs.id("minecraft:entity_type", "minecraft:evoker_fangs").unwrap_or(0);
                b.vi(e.eid).uuid(e.uuid).vi(ty).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]).u8(0).u8(0).u8(mob::angle_byte(*yaw)).u8(0).vi(0);
                vec![self.encode("add_entity", &b)]
            }
            EKind::Projectile(_) | EKind::Cloud(_) => self.projectile_spawn_packets(e),
            EKind::Lightning { .. } => {
                let ty = regs.id("minecraft:entity_type", "minecraft:lightning_bolt").unwrap_or(0);
                b.vi(e.eid).uuid(e.uuid).vi(ty).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]).u8(0).u8(0).u8(0).u8(0).vi(0);
                vec![self.encode("add_entity", &b)]
            }
            _ => Vec::new(),
        }
    }

    /// Spawn a mob at the given feet position (no `finalizeSpawn`).
    pub fn spawn_mob(&self, ty: MobType, pos: [f64; 3], persistent: bool) -> i32 {
        self.spawn_mob_with(ty, pos, persistent, |_, _| {})
    }

    /// Spawn a mob after letting `f` set it up.
    pub fn spawn_mob_with(&self, ty: MobType, pos: [f64; 3], persistent: bool, f: impl FnOnce(&mut Mob, &mut rand::rngs::ThreadRng)) -> i32 {
        let mut rng = rand::thread_rng();
        let mut m = Mob::new(ty, persistent);
        m.mo.yaw = rng.gen_range(0.0..360.0);
        m.mo.head_yaw = m.mo.yaw;
        m.mo.body_yaw = m.mo.yaw;
        f(&mut m, &mut rng);
        let eid = if m.ty.is("ender_dragon") { self.next_eids(9) } else { self.next_eid() };
        let e = Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Mob(Box::new(m)), pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos };
        self.add_entity(e);
        eid
    }

    /// Spawn with `finalizeSpawn` (equipment, babies, variants).
    pub fn spawn_mob_finalized(&self, ty: MobType, pos: [f64; 3], natural: bool) -> i32 {
        let difficulty = self.difficulty();
        let biome = self.world.biome(pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32).unwrap_or_default();
        self.spawn_mob_with(ty, pos, false, |m, rng| ai::mobs::finalize_spawn(m, rng, difficulty, &biome, natural))
    }

    fn sound(&self, name: &str, pos: [f64; 3], hostile: bool, volume: f32, pitch: f32) {
        let Some(id) = Registries::get().id("minecraft:sound_event", name) else { return };
        let mut b = Vec::new();
        b.vi(id + 1).vi(if hostile { 5 } else { 6 });
        b.i32((pos[0] * 8.0) as i32).i32((pos[1] * 8.0) as i32).i32((pos[2] * 8.0) as i32);
        b.f32(volume).f32(pitch).i64(rand::thread_rng().r#gen());
        let c = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
        self.broadcast_chunk(c, &self.encode("sound", &b));
    }

    // ------------------------------------------------------------ sensing

    fn seen_player(&self, p: &Player, night: bool) -> Seen {
        let pose = *p.pose.lock();
        let gm = p.gamemode.load(Ordering::Relaxed);
        let f = p.flags.load(Ordering::Relaxed);
        let eq = p.equipment();
        let v = p.vit.lock();
        let gold = eq[2..6].iter().flatten().any(|s| Items::get().name(s.item).starts_with("golden_"));
        let armor = eq[2..6].iter().filter(|s| s.is_some()).count() as f32 / 4.0;
        let swimming = f & super::hurt::F_SWIMMING != 0;
        let sneaking = f & super::hurt::F_SNEAKING != 0;
        let h = if swimming { 0.6 } else if sneaking { 1.5 } else { 1.8 };
        let eye = if swimming { 0.4 } else if sneaking { 1.27 } else { 1.62 };
        Seen {
            eid: p.eid,
            uuid: p.uuid,
            ty: "player",
            pos: pose.pos,
            vel: *p.motion.lock(),
            width: 0.6,
            height: h,
            eye,
            player: true,
            gamemode: gm,
            yaw: pose.yaw,
            pitch: pose.pitch,
            hands: [eq[0], eq[1]],
            head: eq[5],
            baby: false,
            in_love: false,
            age: 0,
            owner: None,
            tame: false,
            target: None,
            invisible: v.living.has_effect(Effect::Invisibility),
            health: if v.dead() { 0.0 } else { v.living.health },
            monster: false,
            panicking: false,
            sleeping: false,
            armor_cover: armor,
            on_ground: pose.on_ground,
            in_water: f & super::hurt::F_IN_WATER != 0,
            swimming,
            sprinting: f & super::hurt::F_SPRINTING != 0,
            in_vehicle: f & super::hurt::F_PASSENGER != 0,
            night,
            undead: false,
            raider: false,
            scared: false,
            wears_gold: gold,
            size: 0,
            color: 0,
            effects: v.living.effects.iter().map(|i| i.effect.name()).collect(),
            last_hurt_by: v.living.last_hurt_by_mob,
            last_hurt: v.living.last_hurt_mob,
        }
    }

    fn seen_mob(e: &Entity, m: &Mob, night: bool) -> Seen {
        let gold = m.eq[2..6].iter().flatten().any(|s| Items::get().name(s.item).starts_with("golden_"));
        Seen {
            eid: e.eid,
            uuid: e.uuid,
            ty: m.ty.name(),
            pos: e.pos,
            vel: e.vel,
            width: m.width(),
            height: m.height(),
            eye: m.eye(),
            player: false,
            gamemode: -1,
            yaw: m.mo.head_yaw,
            pitch: m.mo.pitch,
            hands: [m.eq[0], m.eq[1]],
            head: m.eq[5],
            baby: m.is_baby(),
            in_love: m.in_love(),
            age: m.age,
            owner: m.owner,
            tame: m.tame,
            target: m.target,
            invisible: m.living.has_effect(Effect::Invisibility),
            health: if m.death > 0 { 0.0 } else { m.living.health },
            monster: m.ty.hostile(),
            panicking: false,
            sleeping: false,
            armor_cover: m.eq[2..6].iter().filter(|s| s.is_some()).count() as f32 / 4.0,
            on_ground: e.on_ground,
            in_water: m.mo.in_water,
            swimming: false,
            sprinting: false,
            in_vehicle: false,
            night,
            undead: m.ty.is_undead(),
            raider: m.ty.is_raider(),
            scared: m.ty.is("armadillo") && m.v.t2 > 0,
            wears_gold: gold,
            size: m.size,
            color: m.v.wool,
            effects: m.living.effects.iter().map(|i| i.effect.name()).collect(),
            last_hurt_by: m.living.last_hurt_by_mob,
            last_hurt: m.living.last_hurt_mob,
        }
    }

    // ------------------------------------------------------------ tick

    pub(super) fn tick_mobs(&self) {
        let now = self.now.load(Ordering::Relaxed);
        let players = self.players();
        let raining = self.raining();
        let thundering = self.thundering();
        let day = self.is_day();
        let night = !(day && !raining);
        let mut rng = rand::thread_rng();
        let mut acts: Vec<Act> = Vec::new();
        let mut sends: Vec<((i32, i32), Arc<Vec<u8>>)> = Vec::new();
        let mut direct: Vec<(Arc<Player>, Arc<Vec<u8>>)> = Vec::new();
        let mut removed: Vec<(i32, (i32, i32))> = Vec::new();
        let env = Env {
            now,
            day,
            raining,
            thundering,
            difficulty: self.difficulty(),
            players: players.iter().filter(|p| !p.vit.lock().dead()).map(|p| (p.eid, p.pose.lock().pos, p.gamemode.load(Ordering::Relaxed))).collect(),
        };
        let mut seen: Vec<Seen> = players.iter().filter(|p| !p.vit.lock().dead()).map(|p| self.seen_player(p, night)).collect();
        // for the projectiles
        let targets: Vec<(Arc<Player>, [f64; 3], i32)> = players.iter().filter(|p| !p.vit.lock().dead()).map(|p| (p.clone(), p.pose.lock().pos, p.gamemode.load(Ordering::Relaxed))).collect();
        let mut pacts: Vec<super::projectile::PAct> = Vec::new();
        {
            let mut ents = self.entities.lock();
            let mut ptargets: Vec<super::projectile::Target> = targets
                .iter()
                .filter(|(_, _, gm)| *gm != 3)
                .map(|(p, pos, gm)| super::projectile::Target { eid: p.eid, bbox: Aabb::entity(*pos, 0.6, 1.8), player: true, eye: [pos[0], pos[1] + 1.62, pos[2]], creative: *gm == 1 })
                .collect();
            for e in ents.values() {
                if let EKind::Mob(m) = &e.kind {
                    if m.death == 0 {
                        ptargets.push(super::projectile::Target { eid: e.eid, bbox: Aabb::entity(e.pos, m.width(), m.height()), player: false, eye: [e.pos[0], e.pos[1] + m.eye(), e.pos[2]], creative: false });
                        seen.push(Self::seen_mob(e, m, night));
                    }
                }
            }
            for e in ents.values_mut() {
                if !matches!(e.kind, EKind::Mob(_) | EKind::Projectile(_) | EKind::Cloud(_) | EKind::Lightning { .. } | EKind::Fangs { .. }) {
                    continue;
                }
                let old_chunk = e.chunk();
                if self.world.get_block(e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32).is_none() && e.pos[1] >= MIN_Y as f64 {
                    continue; // chunk unloaded: frozen
                }
                e.age += 1;
                let gone = match e.kind {
                    EKind::Mob(_) => self.tick_mob(e, &seen, &env, &mut acts, &mut sends),
                    EKind::Projectile(_) => {
                        let before = match &e.kind {
                            EKind::Projectile(p) => (p.in_ground, p.on_fire, p.crit),
                            _ => (false, false, false),
                        };
                        let gone = self.tick_projectile(e, &ptargets, &mut pacts);
                        if let EKind::Projectile(p) = &e.kind {
                            if !gone && before != (p.in_ground, p.on_fire, p.crit) {
                                sends.push((e.chunk(), self.projectile_meta(e.eid, p)));
                            }
                        }
                        gone
                    }
                    EKind::Cloud(_) => self.tick_cloud(e, &ptargets, &mut pacts, &mut sends),
                    EKind::Lightning { .. } => self.tick_lightning(e, &mut rng, &mut acts),
                    EKind::Fangs { .. } => self.tick_fangs(e, &seen, &mut acts, &mut sends),
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
                let (yaw, pitch) = match &e.kind {
                    EKind::Mob(m) => (m.mo.yaw, m.mo.pitch),
                    EKind::Projectile(p) => (p.yaw, p.pitch),
                    _ => {
                        let h = (e.vel[0] * e.vel[0] + e.vel[2] * e.vel[2]).sqrt();
                        (e.vel[0].atan2(e.vel[2]).to_degrees() as f32, e.vel[1].atan2(h).to_degrees() as f32)
                    }
                };
                if let EKind::Mob(m) = &mut e.kind {
                    // ServerEntity.sendChanges: every 3 ticks, relative moves in 1/4096
                    // blocks, a full sync when the delta overflows, after 400 ticks, or
                    // when the mob changed chunks (new viewers saw its exact spot)
                    let tc = m.tick_count;
                    if tc % 3 == 0 || new_chunk != old_chunk || m.death > 0 {
                        let q = |v: f64| (v * 4096.0).round() as i64;
                        let d: Vec<i64> = (0..3).map(|i| q(e.pos[i]) - q(e.synced[i])).collect();
                        let moved = d.iter().any(|v| *v != 0);
                        let rb = (mob::angle_byte(yaw), mob::angle_byte(pitch));
                        let turned = rb != (mob::angle_byte(m.rot_sent.0), mob::angle_byte(m.rot_sent.1));
                        let overflow = d.iter().any(|v| v.abs() > 32767);
                        let force = overflow || new_chunk != old_chunk || (tc % 400 == 0 && moved);
                        let mut b = Vec::new();
                        if force {
                            e.synced = e.pos;
                            m.rot_sent = (yaw, pitch);
                            b.vi(e.eid).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]).f64(e.vel[0]).f64(e.vel[1]).f64(e.vel[2]).f32(yaw).f32(pitch).bool(e.on_ground);
                            sends.push((new_chunk, self.encode("entity_position_sync", &b)));
                        } else if moved || turned {
                            for i in 0..3 {
                                e.synced[i] += d[i] as f64 / 4096.0;
                            }
                            b.vi(e.eid);
                            if moved {
                                b.i16(d[0] as i16).i16(d[1] as i16).i16(d[2] as i16);
                            }
                            if turned {
                                m.rot_sent = (yaw, pitch);
                                b.u8(rb.0).u8(rb.1);
                            }
                            b.bool(e.on_ground);
                            let name = match (moved, turned) {
                                (true, true) => "move_entity_pos_rot",
                                (true, false) => "move_entity_pos",
                                _ => "move_entity_rot",
                            };
                            sends.push((new_chunk, self.encode(name, &b)));
                        }
                    }
                } else {
                    let moved = (0..3).any(|i| (e.pos[i] - e.synced[i]).abs() > 1.0 / 4096.0);
                    if moved {
                        e.synced = e.pos;
                        let mut b = Vec::new();
                        b.vi(e.eid).f64(e.pos[0]).f64(e.pos[1]).f64(e.pos[2]).f64(e.vel[0]).f64(e.vel[1]).f64(e.vel[2]).f32(yaw).f32(pitch).bool(e.on_ground);
                        sends.push((new_chunk, self.encode("entity_position_sync", &b)));
                    }
                }
                if let EKind::Mob(m) = &mut e.kind {
                    if (m.mo.head_yaw - m.head_yaw_sent).abs() > 1.0 {
                        m.head_yaw_sent = m.mo.head_yaw;
                        let mut b = Vec::new();
                        b.vi(e.eid).u8(mob::angle_byte(m.mo.head_yaw));
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
            self.run_act(a, &mut pacts);
        }
        self.projectile_acts(pacts);
        self.natural_spawns(&env, &mut rng);
        if now % 20 == 0 {
            self.spawn_generated(&env);
        }
        self.tick_spawners(&env);
    }

    /// One mob tick; true when the mob is removed.
    fn tick_mob(&self, e: &mut Entity, seen: &[Seen], env: &Env, acts: &mut Vec<Act>, sends: &mut Vec<((i32, i32), Arc<Vec<u8>>)>) -> bool {
        let eid = e.eid;
        let uuid = e.uuid;
        let chunk = e.chunk();
        let EKind::Mob(m) = &mut e.kind else { return true };
        if m.death > 0 && m.ty.is("ender_dragon") {
            m.death += 1;
            return self.dragon_death_tick(eid, &mut e.pos, m);
        }
        if m.death > 0 {
            // LivingEntity.tickDeath
            m.death += 1;
            if m.death >= 20 {
                sends.push((chunk, self.entity_event(eid, 60)));
                return true;
            }
            return false;
        }
        if e.pos[1] < (MIN_Y - 64) as f64 {
            return true;
        }
        // Mob.checkDespawn
        let nearest = env.players.iter().filter(|p| p.2 != 3).map(|p| dist2(p.1, e.pos)).fold(f64::INFINITY, f64::min);
        if env.difficulty == 0 && m.ty.hostile() && !matches!(m.ty.name(), "shulker" | "ender_dragon") {
            return true;
        }
        if !m.persistent && nearest.is_finite() {
            let removes = removes_when_far(m);
            let far = despawn_distance(m.ty.category());
            if nearest > far * far && removes {
                return true;
            }
            let no_despawn = 32.0;
            if m.no_action > 600 && rand::thread_rng().gen_range(0..800) == 0 && nearest > no_despawn * no_despawn && removes {
                return true;
            }
            if nearest < no_despawn * no_despawn {
                m.no_action = 0;
            }
        } else if m.persistent {
            m.no_action = 0;
        }
        m.tick_count += 1;
        m.living.tick_counters();
        if m.love > 0 {
            m.love -= 1;
        }
        if m.ty.ageable() {
            // AgeableMob.aiStep
            if m.age < 0 {
                m.age += 1;
                if m.age == 0 {
                    ai::mobs::on_age_change(m);
                }
            } else if m.age > 0 {
                m.age -= 1;
            }
        }
        let meta_before = meta_hash(m, self.clock() as i64);
        let eq_before = m.eq;
        let get = |x: i32, y: i32, z: i32| self.world.get_block(x, y, z);
        let mut cx = Ctx {
            game: self,
            get: &get,
            seen,
            now: env.now as i64,
            eid,
            uuid,
            pos: e.pos,
            vel: e.vel,
            on_ground: e.on_ground,
            acts,
            rng: rand::thread_rng(),
            day: env.day,
            raining: env.raining,
            thundering: env.thundering,
            difficulty: env.difficulty,
            los: std::collections::HashMap::new(),
            mob_griefing: true,
        };
        // ---- Entity.baseTick / LivingEntity.baseTick
        let self_damage = ai::custom::environment(m, &mut cx);
        // ---- LivingEntity.aiStep
        let mut landed = false;
        let alive = m.living.health > 0.0;
        if alive {
            for v in cx.vel.iter_mut() {
                if v.abs() < 0.003 {
                    *v = 0.0;
                }
            }
            ai::custom::ai_step_pre(m, &mut cx);
            if !m.no_ai {
                // Mob.serverAiStep
                m.no_action += 1;
                let full = (m.tick_count + eid) % 2 == 0 || m.tick_count <= 1;
                let mut targets = std::mem::take(&mut m.targets);
                ai::run_selector(&mut targets, m, &mut cx, full, 0);
                m.targets = targets;
                let mut goals = std::mem::take(&mut m.goals);
                ai::run_selector(&mut goals, m, &mut cx, full, 0);
                m.goals = goals;
                // navigation
                let nm = m.nav_mob(&cx);
                if let Some((w, speed)) = m.nav.tick(&nm, cx.get, cx.now) {
                    m.mc.set_wanted(w, speed);
                }
                ai::custom::server_step(m, &mut cx);
                // move control
                if m.mc.kind == control::MoveKind::Custom {
                    ai::custom::move_tick(m, &mut cx);
                } else {
                    let cm = ai::custom::control_mob(m, &cx);
                    let mut vel = cx.vel;
                    let malus = m.malus;
                    let pos = cx.pos;
                    let walk = |dx: f32, dz: f32| {
                        let p = [(pos[0] + dx as f64).floor() as i32, pos[1].floor() as i32, (pos[2] + dz as f64).floor() as i32];
                        let t = ai::path::state_type(get(p[0], p[1], p[2]).unwrap_or(blocks::AIR));
                        let mal = malus[t as usize];
                        let mal = if mal.is_nan() { t.malus() } else { mal };
                        mal >= 0.0
                    };
                    control::move_tick(&mut m.mc, &mut m.mo, &cm, &mut vel, cx.get, &walk);
                    cx.vel = vel;
                }
                // look + jump controls
                let cm = ai::custom::control_mob(m, &cx);
                control::look_tick(&mut m.look, &mut m.mo, &cm);
                m.mo.jumping = m.mo.jump_req;
                m.mo.jump_req = false;
            } else {
                m.mo.jumping = false;
                m.mo.xxa = 0.0;
                m.mo.zza = 0.0;
            }
            // travel
            let ti = self.travel_in(m);
            let was_ground = cx.on_ground;
            let moved = match ai::custom::travel(m, &mut cx, &ti) {
                Some(r) => r,
                None => {
                    let (mut pos, mut vel, mut og) = (cx.pos, cx.vel, cx.on_ground);
                    let r = control::travel(cx.get, &mut pos, &mut vel, &mut og, &mut m.mo, &ti);
                    cx.pos = pos;
                    cx.vel = vel;
                    cx.on_ground = og;
                    r
                }
            };
            landed = !was_ground && cx.on_ground;
            if let Some(fall) = moved.landed_fall {
                if !m.ty.fall_immune() {
                    let safe = m.living.attrs.value(Attr::SafeFallDistance);
                    let mult = m.living.attrs.value(Attr::FallDamageMultiplier);
                    let below = cx.block([cx.block_pos()[0], (cx.pos[1] - 0.2).floor() as i32, cx.block_pos()[2]]);
                    if let Some((k, extra, kind)) = super::hazards::landing(below, false) {
                        let d = (((fall + extra) + 1.0e-6 - safe) * mult * k as f64).floor();
                        if d > 0.0 {
                            cx.acts.push(Act::Hurt { target: eid, kind, by: None, amount: d as f32 });
                        }
                    }
                }
            }
            ai::custom::ai_step_post(m, &mut cx, landed);
            // LivingEntity.tick: body rotation
            if m.ty.name() != "slime" && m.ty.name() != "magma_cube" {
                control::body_tick(&mut m.mo, cx.pos, m.ty.max_head_y());
            }
            // Mob.baseTick: ambient sound
            m.ambient += 1;
            if cx.rng.gen_range(0..1000) < m.ambient {
                m.ambient = -m.ty.ambient_interval();
                if let Some(s) = ambient_sound(m) {
                    let pitch = (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + if m.is_baby() { 1.5 } else { 1.0 };
                    cx.acts.push(Act::Sound { name: s, pos: cx.pos, hostile: m.ty.hostile(), volume: 1.0, pitch });
                }
            }
        }
        let _ = landed;
        let (pos, vel, on_ground) = (cx.pos, cx.vel, cx.on_ground);
        drop(cx);
        e.pos = pos;
        e.vel = vel;
        e.on_ground = on_ground;
        let c = e.chunk();
        let EKind::Mob(m) = &mut e.kind else { return true };
        // effects and self damage
        {
            let info = super::hurt::mob_info(m, e.on_ground);
            let mut body = Body { eid, info, pos: e.pos, head_yaw: m.mo.head_yaw, velocity: e.vel, player: None, invulnerable: false, fire_immune: m.ty.fire_immune(), raining: env.raining, thundering: env.thundering };
            let mut eq = m.eq;
            let mut rng = rand::thread_rng();
            for (src, amount) in self_damage {
                m.living.hurt(&mut eq, &mut body, &src, amount, &mut rng);
            }
            m.living.tick_effects(&mut eq, &mut body, &mut rng);
            m.living.update_equipment(&eq);
            m.eq = eq;
            e.vel = body.velocity;
            let events = std::mem::take(&mut m.living.events);
            if !events.is_empty() {
                acts.push(Act::MobEvents { eid, chunk: c, pos: e.pos, events });
            }
        }
        // client sync
        for ev in std::mem::take(&mut m.event) {
            sends.push((c, self.entity_event(eid, ev)));
        }
        if std::mem::take(&mut m.swing) {
            let mut b = Vec::new();
            b.vi(eid).u8(0);
            sends.push((c, self.encode("animate", &b)));
        }
        if m.eq != eq_before {
            sends.push((c, self.mob_equipment(eid, m)));
        }
        let h = meta_hash(m, self.clock() as i64);
        if h != meta_before || m.meta_sent == 0 {
            m.meta_sent = h | 1;
            sends.push((c, self.mob_meta(eid, m)));
        }
        if m.living.attrs.dirty.iter().any(|a| a.synced()) {
            sends.push((c, self.attributes_packet(eid, &mut m.living)));
        }
        m.living.attrs.dirty.clear();
        false
    }

    /// The physics constants of a mob for `travel`.
    fn travel_in(&self, m: &mut Mob) -> control::TravelIn {
        let n = m.ty.name();
        let flying = match n {
            "ghast" | "happy_ghast" => Some((0.02, 0.02, 0.02)),
            "phantom" => Some((0.2, 0.2, 0.2)),
            "allay" | "bee" | "parrot" | "breeze" => None,
            _ => None,
        };
        let no_gravity = matches!(n, "vex" | "phantom" | "wither" | "ender_dragon") || matches!(m.mc.kind, control::MoveKind::Flying { .. }) ;
        m.mo.no_gravity = no_gravity;
        control::TravelIn {
            width: m.width(),
            height: m.height(),
            eye: m.eye(),
            max_up_step: m.max_up_step,
            gravity: m.living.attrs.value(Attr::Gravity),
            jump_strength: m.living.attrs.value(Attr::JumpStrength),
            jump_boost: m.living.effect(Effect::JumpBoost).map(|i| 0.1 * (i.amplifier + 1) as f64).unwrap_or(0.0),
            water_efficiency: m.living.attrs.value(Attr::WaterMovementEfficiency),
            levitation: m.living.effect(Effect::Levitation).map(|i| i.amplifier),
            slow_falling: m.living.has_effect(Effect::SlowFalling),
            dolphins_grace: m.living.has_effect(Effect::DolphinsGrace),
            flying_animal: matches!(n, "bee" | "parrot" | "allay"),
            flying,
            fluid_affected: !matches!(n, "ghast" | "happy_ghast" | "phantom" | "ender_dragon" | "vex"),
            can_stand_on_lava: n == "strider",
            water_slowdown: if matches!(n, "skeleton_horse" | "polar_bear") { 0.98 } else if n == "turtle" || n == "axolotl" { 0.9 } else { 0.8 },
            climbing: m.v.climbing,
        }
    }

    /// `EvokerFangs.tick`
    fn tick_fangs(&self, e: &mut Entity, seen: &[Seen], acts: &mut Vec<Act>, sends: &mut Vec<((i32, i32), Arc<Vec<u8>>)>) -> bool {
        let (chunk, eid, pos) = (e.chunk(), e.eid, e.pos);
        let EKind::Fangs { owner, warmup, life, sent, .. } = &mut e.kind else { return true };
        *warmup -= 1;
        if *warmup < 0 {
            if *warmup == -8 {
                let bb = Aabb::entity(pos, 0.5, 0.8).inflate(0.2, 0.0, 0.2);
                for s in seen.iter().filter(|s| s.bbox().intersects(&bb) && s.health > 0.0 && !s.spectator()) {
                    if Some(s.eid) == *owner || s.raider && owner.is_some() {
                        continue;
                    }
                    if s.player && !s.attackable_player() {
                        continue;
                    }
                    acts.push(Act::Hurt { target: s.eid, kind: "magic", by: *owner, amount: 6.0 });
                }
            }
            if !*sent {
                *sent = true;
                sends.push((chunk, self.entity_event(eid, 4)));
                acts.push(Act::Sound { name: "minecraft:entity.evoker_fangs.attack".into(), pos, hostile: true, volume: 1.0, pitch: rand::thread_rng().r#gen::<f32>() * 0.2 + 0.85 });
            }
            *life -= 1;
            if *life < 0 {
                return true;
            }
        }
        false
    }

    // ------------------------------------------------------------ acts

    fn run_act(&self, a: Act, pacts: &mut Vec<super::projectile::PAct>) {
        let mut rng = rand::thread_rng();
        match a {
            Act::MobAttack { mob, target } => {
                self.mob_do_hurt_target(mob, target);
            }
            Act::Proj(p) => pacts.push(p),
            Act::MobEvents { eid, chunk, pos, events } => self.mob_events(eid, chunk, pos, events),
            Act::Thunder { bolt, cause, area, fire } => self.thunder(bolt, cause, area, fire),
            Act::Explode { pos, power, fire, by } => self.explode_full(pos, power, fire, by, Some(true)),
            Act::Breath { owner, pos } => {
                let c = super::projectile::Cloud { owner: Some(owner), potion: Some("harming".into()), scale: 1.0, radius: 5.0, radius_on_use: 0.0, radius_per_tick: 0.0, duration: 200, wait_time: 0, reapplication_delay: 20, victims: Vec::new(), color: 0x9900ff };
                let eid = self.next_eid();
                self.add_entity(Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Cloud(Box::new(c)), pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos });
            }
            Act::Sound { name, pos, hostile, volume, pitch } => self.sound(&name, pos, hostile, volume, pitch),
            Act::Arrow { owner, from, target, power, tipped, fire, velocity, uncertainty } => {
                // ProjectileUtil.getMobArrow: setBaseDamageFromMob(power)
                let d = self.difficulty() as f64;
                let damage = power as f64 * 2.0 + super::ai::special::triangle(&mut rng, d * 0.11, 0.57425);
                let dx = target[0] - from[0];
                let dz = target[2] - from[2];
                let hd = (dx * dx + dz * dz).sqrt();
                let dy = target[1] - from[1] + hd * 0.2;
                let vel = super::projectile::shoot_dir([dx, dy, dz], velocity, uncertainty, &mut rng);
                let item = Items::get().id("arrow").unwrap_or(0);
                let mut p = Projectile::new(PType::Arrow, Some(owner), Stack::new(item, 1));
                p.base_damage = damage;
                p.on_fire = fire;
                match tipped {
                    Some("slowness") => p.extra_effects.push((Effect::Slowness, 600, 0)),
                    Some("poison") => p.extra_effects.push((Effect::Poison, 100, 0)),
                    Some("weakness") => p.extra_effects.push((Effect::Weakness, 600, 0)),
                    _ => {}
                }
                // the shooter's weapon enchantments (power, punch)
                let weapon = self.entities.lock().get(&owner).and_then(|e| match &e.kind {
                    EKind::Mob(m) => m.eq[living::MAINHAND].or(m.eq[living::OFFHAND]),
                    _ => None,
                });
                if let Some(w) = weapon {
                    let pw = w.enchantment_level("power");
                    if pw > 0 {
                        p.base_damage += pw as f64 * 0.5 + 0.5;
                    }
                    if w.enchantment_level("flame") > 0 {
                        p.on_fire = true;
                    }
                    p.weapon = Some(w);
                }
                self.launch(p, from, vel);
                let name = self.entities.lock().get(&owner).and_then(|e| match &e.kind {
                    EKind::Mob(m) => Some(m.ty.sound_name()),
                    _ => None,
                });
                if let Some(n) = name {
                    if n != "pillager" && n != "piglin" {
                        let snd = if n == "illusioner" { "minecraft:entity.illusioner.cast_spell".to_string() } else { format!("minecraft:entity.{n}.shoot") };
                        let snd = if n == "illusioner" { "minecraft:entity.skeleton.shoot".to_string() } else { snd };
                        self.sound(&snd, from, true, 1.0, 1.0 / (rng.r#gen::<f32>() * 0.4 + 0.8));
                    }
                }
            }
            Act::Throw { owner, item, from, dir, velocity, uncertainty } => {
                let ty = match item {
                    "snowball" => PType::Snowball,
                    "egg" => PType::Egg,
                    _ => PType::Snowball,
                };
                let id = Items::get().id(item).unwrap_or(0);
                let p = Projectile::new(ty, Some(owner), Stack::new(id, 1));
                let vel = super::projectile::shoot_dir(dir, velocity, uncertainty, &mut rng);
                self.launch(p, from, vel);
            }
            Act::Spit { owner, from, dir } => {
                let p = Projectile::new(PType::LlamaSpit, Some(owner), Stack::new(0, 1));
                let vel = super::projectile::shoot_dir(dir, 1.5, 10.0, &mut rng);
                self.launch(p, from, vel);
                self.sound("minecraft:entity.llama.spit", from, false, 1.0, 1.0 + (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.2);
            }
            Act::Trident { owner, from, dir, uncertainty } => {
                let id = Items::get().id("trident").unwrap_or(0);
                let mut p = Projectile::new(PType::Trident, Some(owner), Stack::new(id, 1));
                p.base_damage = 8.0;
                let vel = super::projectile::shoot_dir(dir, 1.6, uncertainty, &mut rng);
                self.launch(p, from, vel);
                self.sound("minecraft:entity.drowned.shoot", from, true, 1.0, 1.0 / (rng.r#gen::<f32>() * 0.4 + 0.8));
            }
            Act::Potion { owner, from, dir, potion, velocity, uncertainty } => {
                let id = Items::get().id("splash_potion").unwrap_or(0);
                let mut s = Stack::new(id, 1);
                s.with_patch(|p| p.potion = Some(potion.clone()));
                let mut p = Projectile::new(PType::SplashPotion, Some(owner), s);
                p.pitch = -20.0;
                let vel = super::projectile::shoot_dir(dir, velocity, uncertainty, &mut rng);
                self.launch(p, from, vel);
            }
            Act::Fire { ty, owner, from, dir, target } => {
                if ty == PType::WitherSkull && target == Some(-1) {
                    let mut p = Projectile::new(ty, Some(owner), Stack::new(0, 1));
                    p.dangerous = true;
                    let l = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt().max(1e-7);
                    self.launch(p, from, [dir[0] / l * 0.1, dir[1] / l * 0.1, dir[2] / l * 0.1]);
                } else {
                    self.mob_shoot(ty, owner, from, dir, target);
                }
            }
            Act::Breed { a, b } => self.breed(a, b),
            Act::SetTarget { eid, target } => {
                if let Some(e) = self.entities.lock().get_mut(&eid) {
                    if let EKind::Mob(m) = &mut e.kind {
                        m.target = Some(target);
                    }
                }
            }
            Act::Destroy { pos, drops } => {
                if let Some(s) = self.world.get_block(pos[0], pos[1], pos[2]) {
                    if !s.is_air() {
                        self.level_event(2001, pos, state_id(s));
                        self.change_block(pos, blocks::AIR, drops, None);
                    }
                }
            }
            Act::DestroyInfested { pos, host } => {
                // InfestedBlock: silverfish come out, host block stays (mob griefing)
                if let Some(s) = crate::registry::BlockRegistry::get().state(&host, &[]) {
                    self.change_block(pos, s, false, None);
                    self.level_event(2001, pos, state_id(s));
                }
                self.spawn_mob(MobType("silverfish"), [pos[0] as f64 + 0.5, pos[1] as f64, pos[2] as f64 + 0.5], false);
            }
            Act::SetBlock { pos, state, drops } => {
                self.change_block(pos, state, drops, None);
            }
            Act::LevelEvent { ev, pos, data } => self.level_event(ev, pos, data),
            Act::GlobalEvent { ev, pos } => {
                for p in self.players() {
                    let mut b = Vec::new();
                    b.i32(ev).pos(pos[0], pos[1], pos[2]).i32(0).bool(true);
                    p.send(self.encode("level_event", &b));
                }
            }
            Act::Hurt { target, kind, by, amount } => {
                let src = match by.and_then(|b| self.entity_ref(b)) {
                    Some(r) => DamageSource::by(kind, r),
                    None => DamageSource::new(kind),
                };
                self.hurt_entity(target, &src, amount);
            }
            Act::Knock { target, power, xd, zd } => self.knockback_entity(target, power, xd, zd),
            Act::Push { target, vel } => self.push_entity(target, vel),
            Act::Effect { target, effect, duration, amp } => {
                if let Some(e) = Effect::from_name(effect) {
                    self.add_effect(target, Instance::new(e, duration, amp));
                }
            }
            Act::EffectRaw { target, effect, duration, amp } => {
                if effect.is_instant() {
                    self.with_living(target, move |l, eq, body| l.apply_instant(eq, body, effect, amp, 1.0, None, &mut rand::thread_rng()));
                } else {
                    self.add_effect(target, Instance::new(effect, duration, amp));
                }
            }
            Act::Drop { pos, stack } => {
                self.spawn_item([pos[0], pos[1] + 0.5, pos[2]], stack, [rng.gen_range(-0.1..0.1), 0.2, rng.gen_range(-0.1..0.1)], 10);
            }
            Act::Remove { eid } => {
                let c = self.entities.lock().remove(&eid).map(|e| e.chunk());
                if let Some(c) = c {
                    self.broadcast_chunk(c, &self.remove_packet(&[eid]));
                }
            }
            Act::SummonVex { owner, pos, life } => {
                let difficulty = self.difficulty();
                let biome = String::new();
                self.spawn_mob_with(MobType("vex"), [pos[0] as f64 + 0.5, pos[1] as f64, pos[2] as f64 + 0.5], false, |m, rng| {
                    ai::mobs::finalize_spawn(m, rng, difficulty, &biome, false);
                    m.v.e0 = Some(owner);
                    m.v.bound = Some(pos);
                    m.v.life = life;
                });
            }
            Act::Fang { owner, pos, yaw, delay } => {
                let e = Entity { eid: self.next_eid(), uuid: uuid::Uuid::new_v4(), kind: EKind::Fangs { owner: Some(owner), warmup: delay, life: 22, sent: false, yaw: yaw.to_degrees() - 90.0 }, pos, vel: [0.0; 3], on_ground: true, age: 0, synced: pos };
                self.add_entity(e);
            }
            Act::SheepColor { eid, color } => {
                let mut ents = self.entities.lock();
                if let Some(e) = ents.get_mut(&eid) {
                    if let EKind::Mob(m) = &mut e.kind {
                        m.v.wool = color;
                    }
                }
            }
            Act::Convert { eid, to, effect } => self.convert_mob(eid, to, effect),
            Act::ElderCurse { pos } => {
                for p in self.players() {
                    let pp = p.pose.lock().pos;
                    if dist2(pp, pos) > 2500.0 || !matches!(p.gamemode.load(Ordering::Relaxed), 0 | 2) {
                        continue;
                    }
                    let has = p.vit.lock().living.effect(Effect::MiningFatigue).is_some_and(|i| i.amplifier >= 2 && i.duration >= 1200);
                    if !has {
                        self.add_effect(p.eid, Instance::new(Effect::MiningFatigue, 6000, 2));
                        let mut b = Vec::new();
                        b.u8(10).f32(1.0);
                        p.send(self.encode("game_event", &b));
                    }
                }
            }
            Act::EntityEvent { eid, ev } => {
                let c = self.entities.lock().get(&eid).map(|e| e.chunk());
                if let Some(c) = c {
                    self.broadcast_chunk(c, &self.entity_event(eid, ev));
                }
            }
            Act::SlimeHit { slime, target, amount } => {
                let Some(me) = self.entity_ref(slime) else { return };
                if self.hurt_entity(target, &DamageSource::by("mob_attack", me), amount) {
                    let pitch = (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.2 + 1.0;
                    self.sound("minecraft:entity.slime.attack", me.pos, true, 1.0, pitch);
                    self.post_attack(target, &DamageSource::by("mob_attack", me), None);
                }
            }
            Act::GameEvent { player, event, value } => {
                if let Some(p) = self.players().into_iter().find(|p| p.eid == player) {
                    let mut b = Vec::new();
                    b.u8(event).f32(value);
                    p.send(self.encode("game_event", &b));
                }
            }
        }
    }

    /// `Mob.doHurtTarget` + the type overrides.
    pub fn mob_do_hurt_target(&self, mob: i32, target: i32) -> bool {
        let (name, on_fire, empty_hand, chunk, pos, baby) = {
            let ents = self.entities.lock();
            let Some(e) = ents.get(&mob) else { return false };
            let EKind::Mob(m) = &e.kind else { return false };
            (m.ty.name(), m.fire > 0, m.eq[living::MAINHAND].is_none(), e.chunk(), e.pos, m.is_baby())
        };
        let mut rng = rand::thread_rng();
        let uses_event = matches!(name, "iron_golem" | "ravager" | "hoglin" | "zoglin" | "warden" | "evoker" | "goat");
        if uses_event {
            self.broadcast_chunk(chunk, &self.entity_event(mob, 4));
        } else {
            let mut b = Vec::new();
            b.vi(mob).u8(0);
            self.broadcast_chunk(chunk, &self.encode("animate", &b));
        }
        // the golem / hoglin damage spread
        let spread = matches!(name, "iron_golem") || (matches!(name, "hoglin" | "zoglin") && !baby);
        let hurt = if spread {
            let dmg = {
                let mut ents = self.entities.lock();
                match ents.get_mut(&mob).map(|e| &mut e.kind) {
                    Some(EKind::Mob(m)) => m.living.attrs.value(Attr::AttackDamage) as f32,
                    _ => 0.0,
                }
            };
            let actual = if dmg as i32 > 0 { dmg / 2.0 + rng.gen_range(0..dmg as i32) as f32 } else { dmg };
            let Some(me) = self.entity_ref(mob) else { return false };
            let src = DamageSource::by("mob_attack", me);
            let ok = self.hurt_entity(target, &src, actual);
            if ok {
                self.post_attack(target, &src, None);
            }
            ok
        } else {
            self.mob_attack(mob, target)
        };
        if let Some(e) = self.entities.lock().get_mut(&mob) {
            if let EKind::Mob(m) = &mut e.kind {
                m.living.last_hurt_mob = Some((target, m.living.tick_count));
                if matches!(name, "iron_golem" | "ravager") {
                    m.v.attack_anim = 10;
                }
                if matches!(name, "hoglin" | "zoglin") {
                    m.v.attack_anim = 10;
                }
            }
        }
        if !hurt {
            return false;
        }
        let diff = self.difficulty();
        let effective = match diff {
            0 => 0.0,
            1 => 0.75,
            2 => 1.5,
            _ => 2.25,
        };
        match name {
            "zombie" | "zombie_villager" | "husk" | "drowned" => {
                if empty_hand && on_fire && rng.r#gen::<f32>() < effective * 0.3 {
                    self.ignite(target, 2 * diff * 20);
                }
                if name == "husk" {
                    self.add_effect(target, Instance::new(Effect::Hunger, 140 * effective as i32, 0));
                }
            }
            "cave_spider" => {
                let secs = match diff {
                    2 => 7,
                    3 => 15,
                    _ => 0,
                };
                if secs > 0 {
                    self.add_effect(target, Instance::new(Effect::Poison, secs * 20, 0));
                }
            }
            "wither_skeleton" => {
                self.add_effect(target, Instance::new(Effect::Wither, 200, 0));
            }
            "bee" => {
                let secs = match diff {
                    2 => 10,
                    3 => 18,
                    _ => 0,
                };
                if secs > 0 {
                    self.add_effect(target, Instance::new(Effect::Poison, secs * 20, 0));
                }
                if let Some(e) = self.entities.lock().get_mut(&mob) {
                    if let EKind::Mob(m) = &mut e.kind {
                        m.v.has_stung = true;
                        m.target = None;
                    }
                }
                self.sound("minecraft:entity.bee.sting", pos, true, 1.0, 1.0);
            }
            "iron_golem" => {
                self.push_entity(target, [0.0, 0.4, 0.0]);
                self.sound("minecraft:entity.iron_golem.attack", pos, false, 1.0, 1.0);
            }
            "hoglin" | "zoglin" => {
                // HoglinBase.throwTarget
                let kb = {
                    let mut ents = self.entities.lock();
                    match ents.get_mut(&mob).map(|e| &mut e.kind) {
                        Some(EKind::Mob(m)) => m.living.attrs.value(Attr::AttackKnockback),
                        _ => 0.0,
                    }
                };
                let tpos = self.entity_ref(target).map(|t| t.pos).unwrap_or(pos);
                let res = self.with_player_or_mob_kb_res(target);
                let strength = kb - res;
                if strength > 0.0 {
                    let (dx, dz) = (tpos[0] - pos[0], tpos[2] - pos[2]);
                    let l = (dx * dx + dz * dz).sqrt().max(1e-7);
                    let ang = ((rng.gen_range(0..21) - 10) as f64).to_radians();
                    let horiz = strength * (rng.r#gen::<f32>() as f64 * 0.5 + 0.2);
                    let (x, z) = (dx / l * horiz, dz / l * horiz);
                    let (s, c) = ang.sin_cos();
                    let y = strength * rng.r#gen::<f32>() as f64 * 0.5;
                    self.push_entity(target, [x * c + z * s, y, z * c - x * s]);
                }
                self.sound(&format!("minecraft:entity.{name}.attack"), pos, true, 1.0, 1.0);
            }
            "ravager" => self.sound("minecraft:entity.ravager.attack", pos, true, 1.0, 1.0),
            "warden" => self.sound("minecraft:entity.warden.attack_impact", pos, true, 10.0, rng.r#gen::<f32>() * 0.2 + 0.9),
            "polar_bear" => {}
            _ => {}
        }
        true
    }

    fn with_player_or_mob_kb_res(&self, eid: i32) -> f64 {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return p.vit.lock().living.attrs.value(Attr::KnockbackResistance);
        }
        let mut ents = self.entities.lock();
        match ents.get_mut(&eid).map(|e| &mut e.kind) {
            Some(EKind::Mob(m)) => m.living.attrs.value(Attr::KnockbackResistance),
            _ => 0.0,
        }
    }

    /// `Animal.spawnChildFromBreeding`
    fn breed(&self, a: i32, b: i32) {
        let mut rng = rand::thread_rng();
        let (ty, pos, variant, chunk) = {
            let mut ents = self.entities.lock();
            let Some(ea) = ents.get_mut(&a) else { return };
            let pos = ea.pos;
            let chunk = ea.chunk();
            let EKind::Mob(ma) = &mut ea.kind else { return };
            if ma.death > 0 {
                return;
            }
            ma.age = 6000;
            ma.love = 0;
            let ty = ma.ty;
            let va = ma.v.variant;
            if let Some(EKind::Mob(mb)) = ents.get_mut(&b).map(|e| &mut e.kind) {
                mb.age = 6000;
                mb.love = 0;
            }
            (ty, pos, va, chunk)
        };
        // turtles lay eggs instead; frogs carry spawn
        if ty.is("turtle") {
            if let Some(EKind::Mob(m)) = self.entities.lock().get_mut(&a).map(|e| &mut e.kind) {
                m.v.has_egg = true;
            }
        } else if ty.is("frog") {
            if let Some(EKind::Mob(m)) = self.entities.lock().get_mut(&a).map(|e| &mut e.kind) {
                m.v.has_egg = true;
            }
        } else {
            self.spawn_mob_with(ty, pos, false, |m, _| {
                m.set_baby(true);
                m.v.variant = variant;
            });
        }
        self.broadcast_chunk(chunk, &self.entity_event(a, 18));
        self.award_xp_orbs(pos, rng.gen_range(1..=7));
    }

    pub fn convert_mob_pub(&self, eid: i32, to: &'static str) {
        self.convert_mob(eid, to, None);
    }

    /// `Mob.convertTo`: replaces a mob with another type at the same spot.
    fn convert_mob(&self, eid: i32, to: &'static str, effect: Option<(&'static str, i32)>) {
        let Some(ty) = MobType::from_name(to) else { return };
        let old = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.remove(&eid) else { return };
            e
        };
        self.broadcast_chunk(old.chunk(), &self.remove_packet(&[eid]));
        let EKind::Mob(om) = old.kind else { return };
        let difficulty = self.difficulty();
        self.spawn_mob_with(ty, old.pos, om.persistent, |m, rng| {
            ai::mobs::finalize_spawn(m, rng, difficulty, "", false);
            m.eq = om.eq;
            m.mo.yaw = om.mo.yaw;
            m.mo.body_yaw = om.mo.body_yaw;
            m.mo.head_yaw = om.mo.head_yaw;
            if om.is_baby() {
                m.set_baby(true);
            }
            m.no_ai = om.no_ai;
            if let Some((e, d)) = effect {
                if let Some(ef) = Effect::from_name(e) {
                    let info = super::hurt::mob_info(m, false);
                    m.living.add_effect(&info, Instance::new(ef, d, 0));
                }
            }
        });
    }

    /// Death of a mob: loot, equipment, experience, slime splits.
    pub(super) fn mob_died(&self, eid: i32, src: &DamageSource) {
        let mut rng = rand::thread_rng();
        let (m, pos) = {
            let ents = self.entities.lock();
            let Some(e) = ents.get(&eid) else { return };
            let EKind::Mob(m) = &e.kind else { return };
            (m.clone(), e.pos)
        };
        let by_player = m.living.last_hurt_by_player.is_some() || m.living.last_hurt_by_mob.is_some_and(|(k, _)| self.players().iter().any(|p| p.eid == k));
        // death sound
        let pitch = (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.2 + if m.is_baby() { 1.5 } else { 1.0 };
        self.sound(&m.ty.death_sound(), pos, m.ty.hostile(), 1.0, pitch);
        // LivingEntity.dropAllDeathLoot: the loot table, with this mob and its killer
        if !m.is_baby() || !m.ty.ageable() {
            let attacker = src.causing.as_ref().map(|c| c.info.type_name.to_string());
            let attacker_variant = src.causing.as_ref().and_then(|c| {
                let ents = self.entities.lock();
                match ents.get(&c.eid).map(|e| &e.kind) {
                    Some(EKind::Mob(a)) if a.ty.is("frog") => Some(["temperate", "warm", "cold"][a.v.variant.clamp(0, 2) as usize].to_string()),
                    _ => None,
                }
            });
            let weapon = src.weapon;
            let ctx = crate::data::EntityLoot {
                ty: m.ty.name().to_string(),
                on_fire: m.fire > 0,
                baby: m.is_baby(),
                sheep_color: m.ty.is("sheep").then(|| wool_name(m.v.wool)),
                sheared: m.v.sheared,
                slime_size: m.size,
                captain: m.ty.is_raider() && m.eq[5].is_some_and(|s| Items::get().name(s.item).ends_with("_banner")),
                by_player,
                looting: weapon.map(|w| w.enchantment_level("looting") as i32).unwrap_or(0),
                attacker,
                attacker_variant,
                direct: src.direct.as_ref().map(|d| d.info.type_name.to_string()),
                source_tags: ["is_lightning", "is_projectile", "is_fire", "is_explosion"].iter().filter(|t| src.is(t)).map(|t| t.to_string()).collect(),
                smelts: weapon.is_some_and(|w| w.enchantment_level("fire_aspect") > 0),
                vehicle: None,
            };
            for s in self.data.roll_entity_loot(&m.ty.loot_table(), &ctx, &mut rng) {
                self.spawn_item([pos[0], pos[1] + 0.5, pos[2]], s, [rng.gen_range(-0.1..0.1), 0.2, rng.gen_range(-0.1..0.1)], 10);
            }
        }
        // dropCustomDeathLoot: equipment by drop chance
        for (slot, s) in m.eq.iter().enumerate().take(6) {
            let Some(mut s) = *s else { continue };
            let chance = m.drop_chance[slot];
            if chance > 0.0 && (chance > 1.0 || by_player && rng.r#gen::<f32>() < chance) {
                if chance <= 1.0 && s.max_damage() > 0 {
                    let max = s.max_damage();
                    let dmg = max - 1 - rng.gen_range(0..(max - 3).max(1));
                    s.with_patch(|p| p.damage = Some(dmg.max(0)));
                }
                self.spawn_item([pos[0], pos[1] + 0.5, pos[2]], s, [rng.gen_range(-0.1..0.1), 0.2, rng.gen_range(-0.1..0.1)], 10);
            }
        }
        if let Some(c) = m.v.carry {
            if let Some(id) = Items::get().id(c.name()) {
                self.spawn_item([pos[0], pos[1] + 0.5, pos[2]], Stack::new(id, 1), [0.0, 0.2, 0.0], 10);
            }
        }
        // experience (only when a player was involved)
        if by_player && !m.is_baby() {
            let xp = experience(&m, &mut rng);
            if xp > 0 {
                self.award_xp_orbs(pos, xp);
            }
        }
        // slimes split
        for (size, p, yaw) in ai::custom::on_death(&m, pos, &mut rng) {
            let persistent = m.persistent;
            self.spawn_mob_with(m.ty, p, persistent, |s, _| {
                ai::mobs::set_slime_size(s, size);
                s.mo.yaw = yaw;
            });
        }
    }

    // ------------------------------------------------------------ explosions

    /// Explosion: destroys blocks (1/power of them drop), hurts players and
    /// mobs, and shows the blast.
    pub fn explode(&self, c: [f64; 3], power: f32, _by: &'static str) {
        // PrimedTnt explodes at y + height / 16; tntExplosionDropDecay is false
        self.explode_full([c[0], c[1] + 0.98 / 16.0, c[2]], power, false, None, Some(false));
    }

    pub fn entity_box(&self, eid: i32) -> Option<Aabb> {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return Some(Aabb::entity(p.pose.lock().pos, 0.6, 1.8));
        }
        let found = {
            let ents = self.entities.lock();
            ents.get(&eid).map(|e| {
                let (half, h) = e.size();
                Aabb::entity(e.pos, half * 2.0, h)
            })
        };
        found.or_else(|| self.dragon_part(eid).map(|d| d.2))
    }

    /// Fireballs and skulls: mob explosions (mobExplosionDropDecay).
    pub fn explode_with_fire(&self, c: [f64; 3], power: f32, fire: bool) {
        self.explode_full(c, power, fire, None, Some(true));
    }

    /// `ServerExplosion.explode`. `blocks`: None keeps the blocks, Some(decay)
    /// destroys them, with drops decaying by 1 / radius when `decay`.
    pub fn explode_full(&self, c: [f64; 3], power: f32, fire: bool, by: Option<i32>, blocks_mode: Option<bool>) {
        let mut rng = rand::thread_rng();
        let get = |x: i32, y: i32, z: i32| self.world.get_block(x, y, z);
        let hit = if blocks_mode.is_some() { mob::explosion_blocks(c, power, &get, &mut rng) } else { Vec::new() };
        let griefing = blocks_mode.is_some();
        let n = hit.len();
        // interactWithBlocks: collect the drops (merged), clear the blocks, then pop them
        let radius = if blocks_mode == Some(true) { Some(power) } else { None };
        let mut stacks: Vec<(Stack, [i32; 3])> = Vec::new();
        for p in &hit {
            let Some(state) = self.world.get_block(p[0], p[1], p[2]) else { continue };
            if state.is_air() {
                continue;
            }
            if state.name() == "tnt" {
                // TntBlock.wasExploded
                self.change_block(*p, blocks::AIR, false, None);
                let f = rng.gen_range(0..20) + 10;
                self.spawn_tnt(*p, by, f);
                continue;
            }
            for st in self.data.explosion_drops(state, radius, &mut rng) {
                // Explosion.addOrAppendStack
                let mut st = st;
                for (have, _) in stacks.iter_mut() {
                    if have.same(&st) && have.count < have.max() {
                        let moved = (have.max() - have.count).min(st.count);
                        have.count += moved;
                        st.count -= moved;
                        if st.count == 0 {
                            break;
                        }
                    }
                }
                if st.count > 0 {
                    stacks.push((st, *p));
                }
            }
            self.change_block(*p, blocks::AIR, false, None);
        }
        for (st, p) in stacks {
            // Block.popResource
            let pos = [p[0] as f64 + 0.5 + rng.gen_range(-0.25..0.25), p[1] as f64 + 0.5 - 0.125 + rng.gen_range(-0.25..0.25), p[2] as f64 + 0.5 + rng.gen_range(-0.25..0.25)];
            self.spawn_item(pos, st, [rng.gen_range(-0.1..0.1), 0.2, rng.gen_range(-0.1..0.1)], 10);
        }
        if fire {
            for p in &hit {
                if rng.gen_range(0..3) == 0 && self.world.get_block(p[0], p[1], p[2]).is_some_and(|b| b.is_air()) {
                    let below = self.world.get_block(p[0], p[1] - 1, p[2]).unwrap_or(blocks::AIR);
                    if ai::path::solid_render(below) {
                        if let Some(f) = crate::registry::BlockRegistry::get().state("fire", &[]) {
                            self.change_block(*p, f, false, None);
                        }
                    }
                }
            }
        }
        // entities
        let r = power as f64 * 2.0;
        let area = Aabb { min: [c[0] - r - 1.0, c[1] - r - 1.0, c[2] - r - 1.0], max: [c[0] + r + 1.0, c[1] + r + 1.0, c[2] + r + 1.0] };
        let source = by.and_then(|b| self.entity_ref(b));
        let mut victims: Vec<(i32, [f64; 3], f64, f64, bool)> = Vec::new();
        for p in self.players() {
            if !matches!(p.gamemode.load(Ordering::Relaxed), 0 | 2) {
                continue;
            }
            let pos = p.pose.lock().pos;
            victims.push((p.eid, pos, 1.62, 1.8, true));
        }
        let mut crystals = Vec::new();
        for e in self.entities.lock().values() {
            if let EKind::Mob(m) = &e.kind {
                if m.death == 0 && Some(e.eid) != by {
                    victims.push((e.eid, e.pos, m.eye(), m.height(), false));
                }
            }
            if let EKind::Crystal { invulnerable: false, .. } = e.kind {
                if Some(e.eid) != by && Aabb::entity(e.pos, 2.0, 2.0).intersects(&area) && dist2(e.pos, c).sqrt() / r <= 1.0 {
                    crystals.push(e.eid);
                }
            }
        }
        for eid in crystals {
            self.destroy_crystal(eid, by, false);
        }
        for (eid, pos, eye, h, player) in victims {
            if !Aabb::entity(pos, 0.0, h).intersects(&area) {
                continue;
            }
            let dist = (dist2(pos, c).sqrt()) / r;
            if dist > 1.0 {
                continue;
            }
            let d = [pos[0] - c[0], pos[1] + eye - c[1], pos[2] - c[2]];
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if l == 0.0 {
                continue;
            }
            let width = if player { 0.6 } else { 0.6 };
            let seen = exposure(&get, c, &Aabb::entity(pos, width, h));
            let impact = (1.0 - dist) * seen;
            let dmg = ((impact * impact + impact) / 2.0 * 7.0 * r + 1.0) as f32;
            let src = match source {
                Some(s) => DamageSource { position: Some(c), ..DamageSource::by("explosion", s) },
                None => DamageSource::at("explosion", c),
            };
            self.hurt_entity(eid, &src, dmg);
            let res = self.explosion_kb_res(eid);
            let k = impact * (1.0 - res);
            if k > 0.0 {
                self.push_entity(eid, [d[0] / l * k, d[1] / l * k, d[2] / l * k]);
            }
        }
        // the blast itself
        let regs = Registries::get();
        let mut b = Vec::new();
        b.f64(c[0]).f64(c[1]).f64(c[2]).f32(power).i32(n as i32).bool(false);
        let particle = if power >= 2.0 && griefing { "minecraft:explosion_emitter" } else { "minecraft:explosion" };
        b.vi(regs.id("minecraft:particle_type", particle).unwrap_or(0));
        b.vi(regs.id("minecraft:sound_event", "minecraft:entity.generic.explode").map(|i| i + 1).unwrap_or(0));
        b.vi(0);
        let chunk = ((c[0].floor() as i32) >> 4, (c[2].floor() as i32) >> 4);
        self.broadcast_chunk(chunk, &self.encode("explode", &b));
    }

    fn explosion_kb_res(&self, eid: i32) -> f64 {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return p.vit.lock().living.attrs.value(Attr::ExplosionKnockbackResistance);
        }
        let mut ents = self.entities.lock();
        match ents.get_mut(&eid).map(|e| &mut e.kind) {
            Some(EKind::Mob(m)) => m.living.attrs.value(Attr::ExplosionKnockbackResistance),
            _ => 0.0,
        }
    }

    // ------------------------------------------------------------ spawning

    /// `NaturalSpawner.spawnForChunk` for the chunks around each player.
    fn natural_spawns(&self, env: &Env, rng: &mut impl Rng) {
        if env.players.is_empty() {
            return;
        }
        // mob counts per category near each player
        let mobs: Vec<([f64; 3], &'static str)> = self
            .entities
            .lock()
            .values()
            .filter_map(|e| match &e.kind {
                EKind::Mob(m) if m.death == 0 => Some((e.pos, m.ty.category())),
                _ => None,
            })
            .collect();
        let cats: [(&str, usize); 7] = [("monster", 70), ("creature", 10), ("ambient", 15), ("axolotls", 5), ("underground_water_creature", 5), ("water_creature", 5), ("water_ambient", 20)];
        for (_, ppos, gm) in &env.players {
            if *gm == 3 {
                continue;
            }
            for (cat, cap) in cats {
                if cat == "creature" && env.now % 400 != 0 {
                    continue;
                }
                if cat == "monster" && env.difficulty == 0 {
                    continue;
                }
                let count = mobs.iter().filter(|(p, c)| *c == cat && dist2(*p, *ppos) < 128.0 * 128.0).count();
                if count >= cap {
                    continue;
                }
                // a share of the 17x17 chunks each tick
                let tries = if cat == "creature" { 289 } else { 24 };
                for _ in 0..tries {
                    let cx = ((ppos[0].floor() as i32) >> 4) + rng.gen_range(-8..=8);
                    let cz = ((ppos[2].floor() as i32) >> 4) + rng.gen_range(-8..=8);
                    let x = (cx << 4) + rng.gen_range(0..16);
                    let z = (cz << 4) + rng.gen_range(0..16);
                    let Some(top) = self.world.surface_y(x, z) else { continue };
                    let y = rng.gen_range(MIN_Y..=top + 1);
                    self.spawn_category_at(cat, [x, y, z], env, rng);
                }
            }
        }
    }

    /// `NaturalSpawner.spawnCategoryForPosition`
    fn spawn_category_at(&self, cat: &str, start: [i32; 3], env: &Env, rng: &mut impl Rng) {
        let Some(s) = self.world.get_block(start[0], start[1], start[2]) else { return };
        if ai::path::solid_render(s) {
            return;
        }
        let mut cluster = 0;
        for _ in 0..3 {
            let (mut x, mut z) = (start[0], start[2]);
            let y = start[1];
            let mut spawner: Option<(MobType, i32)> = None;
            let mut in_group = 0;
            let rolls = (rng.r#gen::<f32>() * 4.0).ceil() as i32;
            for _ in 0..rolls {
                x += rng.gen_range(0..6) - rng.gen_range(0..6);
                z += rng.gen_range(0..6) - rng.gen_range(0..6);
                let p = [x, y, z];
                let feet = [x as f64 + 0.5, y as f64, z as f64 + 0.5];
                let near = env.players.iter().map(|q| dist2(q.1, feet)).fold(f64::INFINITY, f64::min);
                if near <= 576.0 {
                    continue;
                }
                if spawner.is_none() {
                    let Some(biome) = self.world.biome(x, y, z) else { break };
                    let below_bricks = self.world.get_block(x, y - 1, z).is_some_and(|b| b.name() == "nether_bricks");
                    let over = crate::worldgen::generator_for(self.dim.index() as usize).and_then(|g| g.structure_mobs_at(cat, p, below_bricks));
                    let Some(list) = over.as_ref().or_else(|| self.data.spawners.get(biome.trim_start_matches("minecraft:")).and_then(|c| c.get(cat))) else { break };
                    let total: u32 = list.iter().map(|e| e.1).sum();
                    if total == 0 {
                        break;
                    }
                    let mut r = rng.gen_range(0..total);
                    let mut pick = None;
                    for (name, w, min, max) in list {
                        if r < *w {
                            pick = Some((name.clone(), *min, *max));
                            break;
                        }
                        r -= w;
                    }
                    let Some((name, min, max)) = pick else { break };
                    let Some(ty) = MobType::from_name(name.trim_start_matches("minecraft:")) else { break };
                    let size = min as i32 + rng.gen_range(0..=(max as i32 - min as i32).max(0));
                    spawner = Some((ty, size));
                }
                let (ty, group) = spawner.unwrap();
                let far = despawn_distance(ty.category());
                if near > far * far {
                    continue;
                }
                if !self.valid_spawn(ty, p, env, rng) {
                    continue;
                }
                let biome = self.world.biome(x, y, z).unwrap_or_default();
                let difficulty = env.difficulty;
                self.spawn_mob_with(ty, feet, false, |m, r| {
                    ai::mobs::finalize_spawn(m, r, difficulty, &biome, true);
                });
                cluster += 1;
                in_group += 1;
                if cluster >= max_cluster(ty) {
                    return;
                }
                if in_group >= group {
                    break;
                }
            }
        }
    }

    /// `SpawnPlacements` (placement type + spawn rules) and the collision check.
    fn valid_spawn(&self, ty: MobType, p: [i32; 3], env: &Env, rng: &mut impl Rng) -> bool {
        let get = |q: [i32; 3]| self.world.get_block(q[0], q[1], q[2]).unwrap_or(blocks::AIR);
        let at = get(p);
        let below = get([p[0], p[1] - 1, p[2]]);
        let above = get([p[0], p[1] + 1, p[2]]);
        let n = ty.name();
        let water_mob = matches!(n, "cod" | "salmon" | "tropical_fish" | "pufferfish" | "squid" | "glow_squid" | "dolphin" | "axolotl" | "guardian" | "drowned" | "tadpole" | "nautilus");
        // placement type
        if water_mob {
            if !vstate::is_water(at) || ai::path::solid_render(above) {
                return false;
            }
        } else if n == "strider" {
            if at.name() != "lava" {
                return false;
            }
        } else if !matches!(n, "ghast" | "phantom" | "ender_dragon") {
            // ON_GROUND: valid floor, two empty blocks
            if !valid_floor(below, ty) || !empty_spawn(at) || !empty_spawn(above) {
                return false;
            }
        }
        let tag = |t: &str, b: BlockState| self.data.block_in(t, b.name());
        let light = |q: [i32; 3]| self.world.light_at(q[0], q[1], q[2]).unwrap_or((15, 0));
        let raw = |q: [i32; 3]| self.raw_brightness(q, self.sky_darken());
        let dark = |rng: &mut dyn rand::RngCore| {
            // Monster.isDarkEnoughToSpawn
            let (sky, block) = light(p);
            if sky as u32 > rng.next_u32() % 32 {
                return false;
            }
            if block > 0 {
                return false;
            }
            let b = if env.thundering { self.raw_brightness(p, 10) } else { raw(p) };
            b <= (rng.next_u32() % 8) as i32
        };
        let animal_ok = |floor: bool| floor && raw(p) > 8;
        let ok = match n {
            "zombie" | "skeleton" | "creeper" | "spider" | "enderman" | "witch" | "zombie_villager" | "bogged" | "cave_spider" | "silverfish" => env.difficulty > 0 && dark(rng),
            "husk" | "stray" | "parched" => env.difficulty > 0 && dark(rng) && self.world.surface_y(p[0], p[2]).is_none_or(|t| p[1] > t),
            "slime" => {
                if env.difficulty == 0 {
                    false
                } else {
                    let biome = self.world.biome(p[0], p[1], p[2]).unwrap_or_default();
                    let swamp = biome.contains("swamp") && p[1] > 50 && p[1] < 70 && rng.r#gen::<f32>() < 0.5 && rng.r#gen::<f32>() < moon_brightness(self.clock()) && raw(p) <= rng.gen_range(0..8);
                    let chunk = slime_chunk(p[0] >> 4, p[2] >> 4) && rng.gen_range(0..10) == 0 && p[1] < 40;
                    swamp || chunk
                }
            }
            "drowned" => {
                let biome = self.world.biome(p[0], p[1], p[2]).unwrap_or_default();
                let below_water = vstate::is_water(below);
                env.difficulty > 0 && dark(rng) && below_water && if biome.contains("river") { rng.gen_range(0..15) == 0 } else { rng.gen_range(0..40) == 0 && p[1] < SEA_LEVEL - 5 }
            }
            "guardian" => env.difficulty > 0 && vstate::is_water(below) && (rng.gen_range(0..20) == 0 || self.world.surface_y(p[0], p[2]).is_some_and(|t| p[1] < t)),
            "bat" => {
                p[1] < SEA_LEVEL && raw(p) <= rng.gen_range(0..4) && ai::path::solid_render(below)
            }
            "squid" | "dolphin" | "cod" | "salmon" | "pufferfish" | "nautilus" => p[1] >= SEA_LEVEL - 13 && p[1] <= SEA_LEVEL && vstate::is_water(below) && vstate::is_water(above),
            "tropical_fish" => p[1] >= SEA_LEVEL - 13 && p[1] <= SEA_LEVEL && vstate::is_water(below) && vstate::is_water(above),
            "glow_squid" => p[1] <= SEA_LEVEL - 33 && light(p).0 == 0 && light(p).1 == 0 && vstate::is_water(below),
            "axolotl" => tag("axolotls_spawnable_on", below),
            "rabbit" => tag("rabbits_spawnable_on", below) && raw(p) > 8,
            "fox" => tag("foxes_spawnable_on", below) && raw(p) > 8,
            "polar_bear" => (tag("polar_bears_spawnable_on_alternate", below) || tag("animals_spawnable_on", below)) && raw(p) > 8,
            "goat" => tag("goats_spawnable_on", below) && raw(p) > 8,
            "mooshroom" => tag("mooshrooms_spawnable_on", below) && raw(p) > 8,
            "parrot" => tag("parrots_spawnable_on", below) && raw(p) > 8,
            "turtle" => p[1] < SEA_LEVEL + 4 && tag("turtles_spawnable_on", below) && raw(p) > 8,
            "frog" => tag("frogs_spawnable_on", below) && raw(p) > 8,
            "armadillo" => tag("armadillo_spawnable_on", below) && raw(p) > 8,
            "wolf" => tag("wolves_spawnable_on", below) && raw(p) > 8,
            "camel" => tag("camels_spawnable_on", below) && raw(p) > 8,
            "ocelot" => p[1] >= SEA_LEVEL && (below.name() == "grass_block" || tag("leaves", below)) || rng.gen_range(0..3) == 0 && p[1] >= SEA_LEVEL && tag("animals_spawnable_on", below),
            "strider" => {
                let mut q = [p[0], p[1] + 1, p[2]];
                while get(q).name() == "lava" {
                    q[1] += 1;
                }
                get(q).is_air()
            }
            _ if ty.is_animal() => animal_ok(tag("animals_spawnable_on", below)),
            _ if ty.hostile() => env.difficulty > 0 && dark(rng),
            _ => true,
        };
        if !ok {
            return false;
        }
        // no collision with blocks or other mobs
        let (w, h) = ty.size();
        let bb = Aabb::entity([p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5], w, h);
        let world = control::World(&|x, y, z| self.world.get_block(x, y, z));
        if !super::physics::no_collision(&world, &bb) {
            return false;
        }
        if !water_mob && control::fluid_heights(&|x, y, z| self.world.get_block(x, y, z), &bb) != (0.0, 0.0) && n != "strider" {
            return false;
        }
        true
    }

    /// Entities world generation placed in chunks near players.
    fn spawn_generated(&self, env: &Env) {
        for (_, pos, _) in &env.players {
            let (cx, cz) = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
            for dx in -3..=3 {
                for dz in -3..=3 {
                    for (name, p) in self.world.take_gen_entities(cx + dx, cz + dz) {
                        if name == "end_crystal" {
                            self.spawn_crystal([p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5], true);
                            continue;
                        }
                        if let Some(ty) = MobType::from_name(&name) {
                            let difficulty = env.difficulty;
                            self.spawn_mob_with(ty, [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5], true, |m, rng| {
                                ai::mobs::finalize_spawn(m, rng, difficulty, "", false);
                            });
                        }
                    }
                }
            }
        }
    }

    /// `BaseSpawner.serverTick`: spawns while a player is within 16 blocks.
    fn tick_spawners(&self, env: &Env) {
        let mut rng = rand::thread_rng();
        let mut seen = std::collections::HashSet::new();
        for (_, pos, gm) in &env.players {
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
                        if !env.players.iter().any(|(_, q, g)| *g != 3 && dist2(*q, c) < 16.0 * 16.0) {
                            continue;
                        }
                        let mob_name = t.get("SpawnData").and_then(|d| d.get("entity")).and_then(|e| e.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let Some(ty) = MobType::from_name(mob_name.trim_start_matches("minecraft:")) else { continue };
                        let delay = t.get("Delay").and_then(|v| v.as_i64()).unwrap_or(20) as i32;
                        let next = if delay > 0 {
                            delay - 1
                        } else {
                            // spawnCount 4, spawnRange 4, maxNearbyEntities 6
                            let nearby = self.entities.lock().values().filter(|e| matches!(&e.kind, EKind::Mob(m) if m.ty == ty) && (e.pos[0] - c[0]).abs() < 4.5 && (e.pos[1] - c[1]).abs() < 1.5 && (e.pos[2] - c[2]).abs() < 4.5).count();
                            let nearby_wide = self.entities.lock().values().filter(|e| matches!(&e.kind, EKind::Mob(m) if m.ty == ty) && (e.pos[0] - c[0]).abs() < 9.0 && (e.pos[1] - c[1]).abs() < 5.0 && (e.pos[2] - c[2]).abs() < 9.0).count();
                            let _ = nearby;
                            if nearby_wide < 6 {
                                let (w, h) = ty.size();
                                for _ in 0..4 {
                                    let q = [c[0] + (rng.r#gen::<f64>() - rng.r#gen::<f64>()) * 4.0, (p[1] + rng.gen_range(0..3) - 1) as f64, c[2] + (rng.r#gen::<f64>() - rng.r#gen::<f64>()) * 4.0];
                                    let bb = Aabb::entity(q, w, h);
                                    let world = control::World(&|x, y, z| self.world.get_block(x, y, z));
                                    if !super::physics::no_collision(&world, &bb) {
                                        continue;
                                    }
                                    // SpawnData light limits: monsters need block light <= 11 at most
                                    let qb = [q[0].floor() as i32, q[1].floor() as i32, q[2].floor() as i32];
                                    if ty.hostile() && self.block_light_at(qb) > 11 {
                                        continue;
                                    }
                                    let difficulty = env.difficulty;
                                    let eid = self.spawn_mob_with(ty, q, false, |m, rng| {
                                        ai::mobs::finalize_spawn(m, rng, difficulty, "", false);
                                    });
                                    self.level_event(2004, p, 0);
                                    let _ = eid;
                                }
                            }
                            200 + rng.gen_range(0..600)
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

    /// Mobs that stay (persistent, or never despawn) go back into their
    /// chunks before those are saved: all of them (`all`), or those nobody views.
    pub fn stash_mobs(&self, all: bool) {
        let mut ents = self.entities.lock();
        ents.retain(|_, e| {
            let EKind::Mob(m) = &e.kind else { return true };
            let c = e.chunk();
            let keeps = m.persistent || !removes_when_far(m);
            if !keeps || m.death > 0 || !(all || !self.world.viewed(c.0, c.1)) {
                return true;
            }
            let p = [e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32];
            !self.world.stash_entity(m.ty.name(), p)
        });
    }

    /// `/summon`: spawn with `finalizeSpawn`; None for unknown types.
    pub fn summon(&self, name: &str, pos: [f64; 3]) -> Option<i32> {
        let ty = MobType::from_name(name.trim_start_matches("minecraft:"))?;
        Some(self.spawn_mob_finalized(ty, pos, false))
    }

    pub fn mob_count(&self) -> usize {
        self.entities.lock().values().filter(|e| matches!(e.kind, EKind::Mob(_))).count()
    }

    /// Loot of a mob type (for tests).
    pub fn roll_mob_loot(&self, ty: MobType) -> Vec<Stack> {
        self.data.roll_loot(&ty.loot_table(), &mut rand::thread_rng())
    }
}

/// `Mob.removeWhenFarAway`
fn removes_when_far(m: &Mob) -> bool {
    match m.ty.name() {
        "chicken" => m.v.chicken_jockey,
        "cat" | "ocelot" => !m.tame && m.tick_count > 2400,
        "cod" | "salmon" | "pufferfish" | "tropical_fish" | "axolotl" | "tadpole" => !m.v.from_bucket,
        "iron_golem" | "snow_golem" | "copper_golem" | "villager" | "wandering_trader" | "warden" | "zombie_horse" | "skeleton_horse" | "camel_husk" | "nautilus" | "zombie_nautilus" | "allay" | "ender_dragon" | "wither" => false,
        _ if m.ty.is_animal() => false,
        _ => true,
    }
}

/// `MobCategory.getDespawnDistance`
fn despawn_distance(cat: &str) -> f64 {
    if cat == "water_ambient" { 64.0 } else { 128.0 }
}

/// `Mob.getMaxSpawnClusterSize`
fn max_cluster(ty: MobType) -> i32 {
    match ty.name() {
        "wolf" => 8,
        "ghast" => 1,
        "cod" | "salmon" | "tropical_fish" | "pufferfish" => 8,
        "squid" | "glow_squid" => 4,
        _ => 4,
    }
}

/// `BlockState.isValidSpawn` for the floor block.
fn valid_floor(b: BlockState, ty: MobType) -> bool {
    let n = b.name();
    if matches!(n, "bedrock" | "barrier" | "light") || n.ends_with("_leaves") && !matches!(ty.name(), "ocelot" | "parrot") {
        return false;
    }
    if n == "magma_block" && !ty.fire_immune() {
        return false;
    }
    if matches!(n, "ice" | "packed_ice") && ty.name() != "polar_bear" && ty.name() != "stray" {
        return ai::path::solid_render(b);
    }
    ai::path::solid_render(b) || n.ends_with("_slab") && crate::registry::BlockRegistry::get().property(b, "type") != Some("bottom") || n.ends_with("_stairs") && crate::registry::BlockRegistry::get().property(b, "half") == Some("top") || n == "soul_sand" || n == "mud" || n == "farmland" || n == "dirt_path"
}

/// `NaturalSpawner.isValidEmptySpawnBlock`
fn empty_spawn(b: BlockState) -> bool {
    !super::physics::has_collision(b) && !vstate::has_fluid(b) && !matches!(b.name(), "rail" | "powered_rail" | "detector_rail" | "activator_rail" | "fire" | "soul_fire" | "sweet_berry_bush" | "cactus" | "powder_snow" | "wither_rose" | "lava")
}

/// `DimensionType.moonBrightness` for the current phase.
fn moon_brightness(clock: u64) -> f32 {
    const B: [f32; 8] = [1.0, 0.75, 0.5, 0.25, 0.0, 0.25, 0.5, 0.75];
    B[((clock / 24000) % 8) as usize]
}

/// Slime chunks (`WorldgenRandom.seedSlimeChunk(x, z, seed, 987234911).nextInt(10) == 0`)
/// with the world seed folded into a hash.
fn slime_chunk(x: i32, z: i32) -> bool {
    let seed: i64 = 0;
    let s = seed
        .wrapping_add((x as i64).wrapping_mul(x as i64).wrapping_mul(4987142))
        .wrapping_add((x as i64).wrapping_mul(5947611))
        .wrapping_add((z as i64).wrapping_mul(z as i64).wrapping_mul(4392871))
        .wrapping_add((z as i64).wrapping_mul(389711))
        ^ 987234911;
    // java.util.Random.nextInt(10)
    let mut r = (s ^ 0x5DEECE66D) & ((1i64 << 48) - 1);
    loop {
        r = (r.wrapping_mul(0x5DEECE66D).wrapping_add(0xB)) & ((1i64 << 48) - 1);
        let bits = (r >> 17) as i32;
        let val = bits % 10;
        if bits - val + 9 >= 0 {
            return val == 0;
        }
    }
}

/// `ServerExplosion.getSeenPercent`
fn exposure(get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, c: [f64; 3], bb: &Aabb) -> f64 {
    let xs = 1.0 / ((bb.max[0] - bb.min[0]) * 2.0 + 1.0);
    let ys = 1.0 / ((bb.max[1] - bb.min[1]) * 2.0 + 1.0);
    let zs = 1.0 / ((bb.max[2] - bb.min[2]) * 2.0 + 1.0);
    let xo = (1.0 - ((1.0 / xs).floor() * xs)) / 2.0;
    let zo = (1.0 - ((1.0 / zs).floor() * zs)) / 2.0;
    if xs < 0.0 || ys < 0.0 || zs < 0.0 {
        return 0.0;
    }
    let w = control::World(get);
    let (mut hits, mut count) = (0, 0);
    let mut xx = 0.0;
    while xx <= 1.0 {
        let mut yy = 0.0;
        while yy <= 1.0 {
            let mut zz = 0.0;
            while zz <= 1.0 {
                let x = bb.min[0] + (bb.max[0] - bb.min[0]) * xx;
                let y = bb.min[1] + (bb.max[1] - bb.min[1]) * yy;
                let z = bb.min[2] + (bb.max[2] - bb.min[2]) * zz;
                if super::physics::clip(&w, [x + xo, y, z + zo], c).is_none() {
                    hits += 1;
                }
                count += 1;
                zz += zs;
            }
            yy += ys;
        }
        xx += xs;
    }
    if count == 0 { 0.0 } else { hits as f64 / count as f64 }
}

/// `getExperienceReward` of a mob killed by a player.
fn experience(m: &Mob, rng: &mut impl Rng) -> i32 {
    let n = m.ty.name();
    let base = match n {
        "slime" | "magma_cube" => m.size,
        _ if m.ty.is_animal() || matches!(n, "squid" | "glow_squid" | "dolphin" | "cod" | "salmon" | "pufferfish" | "tropical_fish" | "axolotl" | "frog" | "tadpole") => 1 + rng.gen_range(0..3),
        "ender_dragon" => 0,
        _ => m.ty.xp(),
    };
    if m.ty.hostile() && base > 0 {
        // Mob.getBaseExperienceReward: equipment bonus
        let mut xp = base;
        for (slot, s) in m.eq.iter().enumerate().take(6) {
            if s.is_some() && m.drop_chance[slot] <= 1.0 {
                xp += 1 + rng.gen_range(0..3);
            }
        }
        return xp;
    }
    base
}

pub fn wool_color_name(c: u8) -> &'static str {
    wool_name(c)
}

fn wool_name(c: u8) -> &'static str {
    ["white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray", "cyan", "purple", "blue", "brown", "green", "red", "black"][c as usize % 16]
}

fn ambient_sound(m: &Mob) -> Option<String> {
    let n = m.ty.name();
    if m.death > 0 {
        return None;
    }
    match n {
        "wolf" if m.angry(0) => Some("minecraft:entity.wolf.growl".into()),
        "enderman" if m.v.creepy => Some("minecraft:entity.enderman.scream".into()),
        "piglin" if m.aggressive => Some("minecraft:entity.piglin.angry".into()),
        "zombified_piglin" if m.aggressive => Some("minecraft:entity.zombified_piglin.angry".into()),
        "fox" if m.sitting => Some("minecraft:entity.fox.sleep".into()),
        "bat" if m.v.resting && m.living.tick_count % 4 != 0 => None,
        "cat" if m.tame && m.in_love() => Some("minecraft:entity.cat.purr".into()),
        "dolphin" | "squid" | "glow_squid" if !m.mo.in_water => None,
        "goat" if m.v.screaming => Some("minecraft:entity.goat.screaming.ambient".into()),
        _ => m.ty.ambient_sound(),
    }
}

fn meta_hash(m: &Mob, now: i64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut b = Vec::new();
    ai::mobs::meta(m, now, &mut b);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish() | 1
}

pub fn state_id(s: BlockState) -> i32 {
    s.0 as i32
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_on_flat_ground() {
        crate::registry::BlockRegistry::get();
        let stone = blocks::STONE;
        let get = move |_x: i32, y: i32, _z: i32| Some(if y < 64 { stone } else { blocks::AIR });
        let bb = Aabb::entity([2.5, 64.0, 1.5], 0.6, 1.8);
        let e = exposure(&get, [0.5, 64.0, 0.5], &bb);
        assert!(e > 0.99, "exposure {e}");
    }
}
