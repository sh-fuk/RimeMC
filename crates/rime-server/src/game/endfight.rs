//! `EndDragonFight`: the dragon, its boss bar, the exit portal and podium,
//! the end gateways, end crystals and the respawn ritual.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::seq::SliceRandom;
use rand::Rng;

use super::combat::damage::DamageSource;
use super::entity::{EKind, Entity};
use super::mob::MobType;
use super::{Game, Player};
use crate::anticheat::combat::Aabb;
use crate::net::codec::Out;
use crate::registry::{blocks, BlockRegistry, BlockState};
use crate::world::storage::Dim;

/// Where the dragon spawns and the boss bar is centred.
const CENTRE: [f64; 3] = [0.0, 128.0, 0.0];
const BAR_RANGE: f64 = 192.0;

/// `DragonRespawnAnimation`
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Respawn {
    Start,
    Preparing,
    Pillars,
    Summoning,
}

pub struct Fight {
    /// state loaded and the first scan done
    pub started: bool,
    pub killed: bool,
    pub previously_killed: bool,
    pub dragon: Option<i32>,
    /// the exit portal's origin (the podium centre at portal height)
    pub portal: Option<[i32; 3]>,
    /// gateway indices still to open
    pub gateways: Vec<i32>,
    pub bar: uuid::Uuid,
    pub viewers: HashSet<i32>,
    pub crystals: usize,
    pub bar_health: f32,
    pub respawn: Option<(Respawn, i32, Vec<i32>)>,
    pub dirty: bool,
    since_seen: i32,
}

impl Default for Fight {
    fn default() -> Self {
        Self {
            started: false,
            killed: false,
            previously_killed: false,
            dragon: None,
            portal: None,
            gateways: Vec::new(),
            bar: uuid::Uuid::new_v4(),
            viewers: HashSet::new(),
            crystals: 0,
            bar_health: -1.0,
            respawn: None,
            dirty: false,
            since_seen: 0,
        }
    }
}

fn state(name: &str) -> BlockState {
    BlockRegistry::get().state(name, &[]).unwrap_or(blocks::AIR)
}

fn with(s: BlockState, k: &str, v: &str) -> BlockState {
    BlockRegistry::get().with(s, k, v).unwrap_or(s)
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// The part boxes of a dragon at `pos` facing `yaw` (`EnderDragon.tickPart`
/// without the flight history): head, neck, body, tail ×3, wing ×2.
pub fn part_boxes(pos: [f64; 3], yaw: f32) -> [Aabb; 8] {
    let r = (yaw as f64).to_radians();
    let (s, c) = (r.sin(), r.cos());
    let at = |dx: f64, dy: f64, dz: f64, w: f64, h: f64| Aabb::entity([pos[0] + dx, pos[1] + dy, pos[2] + dz], w, h);
    // forward is (-sin, cos) in Minecraft's yaw
    let (fx, fz) = (-s, c);
    [
        at(fx * 6.5, 1.0, fz * 6.5, 1.0, 1.0),
        at(fx * 5.5, 1.0, fz * 5.5, 3.0, 3.0),
        at(fx * 0.5, 0.0, fz * 0.5, 5.0, 3.0),
        at(-fx * 2.0, 0.5, -fz * 2.0, 2.0, 2.0),
        at(-fx * 4.0, 0.5, -fz * 4.0, 2.0, 2.0),
        at(-fx * 6.0, 0.5, -fz * 6.0, 2.0, 2.0),
        at(c * 4.5, 2.0, s * 4.5, 4.0, 2.0),
        at(-c * 4.5, 2.0, -s * 4.5, 4.0, 2.0),
    ]
}

impl Game {
    // ------------------------------------------------------------ crystals

    /// `EndCrystal.hurtServer` / explosion: remove the crystal, explode when
    /// `explode`, and tell the fight. False when there is no such crystal.
    pub fn destroy_crystal(&self, eid: i32, by: Option<i32>, explode: bool) -> bool {
        let (pos, chunk) = {
            let mut ents = self.entities.lock();
            match ents.get(&eid) {
                Some(e) if matches!(e.kind, EKind::Crystal { .. }) => {
                    if let EKind::Crystal { invulnerable: true, .. } = e.kind {
                        return false;
                    }
                    let r = (e.pos, e.chunk());
                    ents.remove(&eid);
                    r
                }
                _ => return false,
            }
        };
        self.broadcast_chunk(chunk, &self.remove_packet(&[eid]));
        if explode {
            self.explode_full(pos, 6.0, false, None, Some(true));
        }
        if self.dim == Dim::End {
            self.on_crystal_destroyed(eid, by);
        }
        true
    }

    /// `EndDragonFight.onCrystalDestroyed`
    fn on_crystal_destroyed(&self, eid: i32, by: Option<i32>) {
        let respawning = {
            let mut f = self.fight.lock();
            f.crystals = f.crystals.saturating_sub(1);
            f.respawn.as_ref().is_some_and(|r| r.2.contains(&eid))
        };
        if respawning {
            // a ritual crystal broke: start over
            let mut f = self.fight.lock();
            let crystals = f.respawn.take().map(|r| r.2).unwrap_or_default();
            drop(f);
            for c in crystals {
                self.set_crystal(c, |_, beam, _| *beam = None);
            }
            self.reset_spike_crystals();
            return;
        }
        // EnderDragon.onCrystalDestroyed: the healing crystal hurts the dragon
        let dragon = self.fight.lock().dragon;
        if let Some(d) = dragon {
            let healing = {
                let ents = self.entities.lock();
                matches!(ents.get(&d).map(|e| &e.kind), Some(EKind::Mob(m)) if m.v.dragon_crystal == Some(eid))
            };
            if healing {
                let src = match by.and_then(|b| self.entity_ref(b)) {
                    Some(r) if r.player => DamageSource::by("player_explosion", r),
                    _ => DamageSource::new("explosion"),
                };
                self.hurt_dragon(d, 0, &src, 10.0);
                self.with_dragon(d, |m| m.v.dragon_crystal = None);
            }
        }
    }

    fn set_crystal(&self, eid: i32, f: impl FnOnce(&mut bool, &mut Option<[i32; 3]>, &mut bool)) {
        let pkt = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return };
            let chunk = e.chunk();
            let EKind::Crystal { show_bottom, beam, invulnerable } = &mut e.kind else { return };
            f(show_bottom, beam, invulnerable);
            (chunk, self.crystal_meta(eid, *show_bottom, *beam))
        };
        self.broadcast_chunk(pkt.0, &pkt.1);
    }

    pub fn is_crystal(&self, eid: i32) -> bool {
        self.entities.lock().get(&eid).is_some_and(|e| matches!(e.kind, EKind::Crystal { .. }))
    }

    pub fn spawn_crystal(&self, pos: [f64; 3], show_bottom: bool) -> i32 {
        let eid = self.next_eid();
        self.add_entity(Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Crystal { show_bottom, beam: None, invulnerable: false }, pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos });
        eid
    }

    /// `EndCrystalItem.useOn`
    pub fn place_end_crystal(&self, on: [i32; 3]) -> bool {
        let s = self.world.get_block(on[0], on[1], on[2]).unwrap_or(blocks::AIR);
        if !matches!(s.name(), "obsidian" | "bedrock") {
            return false;
        }
        let above = [on[0], on[1] + 1, on[2]];
        if !self.world.get_block(above[0], above[1], above[2]).is_some_and(|b| b.is_air()) {
            return false;
        }
        let bb = Aabb { min: [above[0] as f64, above[1] as f64, above[2] as f64], max: [above[0] as f64 + 1.0, above[1] as f64 + 2.0, above[2] as f64 + 1.0] };
        let blocked = self.entities.lock().values().any(|e| {
            let (half, h) = e.size();
            Aabb::entity(e.pos, half * 2.0, h).intersects(&bb)
        }) || self.players().iter().any(|p| Aabb::entity(p.pose.lock().pos, 0.6, 1.8).intersects(&bb));
        if blocked {
            return false;
        }
        self.spawn_crystal([above[0] as f64 + 0.5, above[1] as f64, above[2] as f64 + 0.5], false);
        if self.dim == Dim::End {
            self.try_respawn();
        }
        true
    }

    // ------------------------------------------------------------ dragon

    pub(crate) fn with_dragon<R>(&self, eid: i32, f: impl FnOnce(&mut super::mob::Mob) -> R) -> Option<R> {
        let mut ents = self.entities.lock();
        match ents.get_mut(&eid).map(|e| &mut e.kind) {
            Some(EKind::Mob(m)) if m.ty.is("ender_dragon") => Some(f(m)),
            _ => None,
        }
    }

    /// The dragon a part entity id belongs to: (dragon eid, part 1..=8, box).
    pub fn dragon_part(&self, eid: i32) -> Option<(i32, usize, Aabb)> {
        let ents = self.entities.lock();
        for k in 1..=8 {
            if let Some(e) = ents.get(&(eid - k)) {
                if let EKind::Mob(m) = &e.kind {
                    if m.ty.is("ender_dragon") && m.death == 0 {
                        return Some((e.eid, k as usize, part_boxes(e.pos, m.mo.yaw)[k as usize - 1]));
                    }
                }
                return None;
            }
        }
        None
    }

    /// `EnderDragon.hurt(part, source, damage)`: non-head parts take a quarter,
    /// only players and explosions hurt it, arrows bounce off a sitting dragon.
    pub fn hurt_dragon(&self, eid: i32, part: usize, src: &DamageSource, amount: f32) -> bool {
        let Some((phase, max)) = self.with_dragon(eid, |m| (m.v.phase, m.living.attrs.value(super::combat::attr::Attr::MaxHealth) as f32)) else { return false };
        if phase == 9 {
            return false;
        }
        let sitting = matches!(phase, 5..=7);
        let mut d = amount;
        if sitting && src.projectile {
            return false;
        }
        if part > 1 {
            d = d / 4.0 + d.min(1.0);
        }
        if d < 0.01 {
            return false;
        }
        let by_player = src.causing.as_ref().is_some_and(|c| c.player);
        if !(by_player || src.is("is_explosion")) {
            return false;
        }
        let hurt = self.with_mob(eid, |l, eq, body| l.hurt(eq, body, src, d, &mut rand::thread_rng())).unwrap_or(false);
        if hurt && sitting {
            self.with_dragon(eid, |m| {
                m.v.sit_damage += d;
                if m.v.sit_damage > 0.25 * max {
                    m.v.sit_damage = 0.0;
                    m.v.phase = 4;
                    m.v.phase_time = 0;
                }
            });
        }
        hurt
    }

    /// The dragon's death animation (`EnderDragon.tickDeath`); true when done.
    pub(crate) fn dragon_death_tick(&self, eid: i32, pos: &mut [f64; 3], m: &mut super::mob::Mob) -> bool {
        let t = m.death;
        if t == 1 {
            m.v.phase = 9;
            self.level_event_global(1028, [pos[0] as i32, pos[1] as i32, pos[2] as i32]);
        }
        pos[1] += 0.1;
        m.mo.yaw += 20.0;
        m.mo.body_yaw = m.mo.yaw;
        let first = !self.fight.lock().previously_killed;
        let xp = if first { 12000 } else { 500 };
        if t > 150 && t % 5 == 0 {
            self.award_dragon_xp(*pos, (xp as f32 * 0.08) as i32);
        }
        if t >= 200 {
            self.award_dragon_xp(*pos, (xp as f32 * 0.2) as i32);
            self.dragon_killed(eid);
            return true;
        }
        false
    }

    fn award_dragon_xp(&self, pos: [f64; 3], amount: i32) {
        let near = self.players().into_iter().filter(|p| !p.vit.lock().dead()).min_by(|a, b| dist2(a.pose.lock().pos, pos).total_cmp(&dist2(b.pose.lock().pos, pos)));
        if let Some(p) = near {
            if dist2(p.pose.lock().pos, pos) < 256.0 * 256.0 {
                p.give_xp(amount);
                let mut rng = rand::thread_rng();
                self.play_sound("minecraft:entity.experience_orb.pickup", 7, p.pose.lock().pos, 0.1, (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.35 + 0.9);
            }
        }
    }

    /// Spawn a fight dragon at the centre (`createNewDragon`).
    fn create_dragon(&self) -> i32 {
        let portal = self.fight.lock().portal;
        let eid = self.spawn_mob_with(MobType::from_name("ender_dragon").expect("dragon"), [CENTRE[0], CENTRE[1], CENTRE[2]], true, |m, _| {
            m.v.bound = portal.or(Some([0, 64, 0]));
            m.v.phase = 0;
        });
        tracing::info!("the ender dragon spawns");
        eid
    }

    /// Living dragons in this level.
    fn dragons(&self) -> Vec<i32> {
        self.entities.lock().values().filter(|e| matches!(&e.kind, EKind::Mob(m) if m.ty.is("ender_dragon") && m.death == 0)).map(|e| e.eid).collect()
    }

    /// `setDragonKilled`
    fn dragon_killed(&self, eid: i32) {
        let first = {
            let mut f = self.fight.lock();
            if f.dragon.is_some_and(|d| d != eid) {
                return;
            }
            f.dragon = None;
            f.killed = true;
            f.dirty = true;
            let first = !f.previously_killed;
            f.previously_killed = true;
            first
        };
        self.remove_boss_bar();
        self.spawn_new_gateway();
        let portal = self.spawn_exit_portal(true);
        if first {
            let egg = state("dragon_egg");
            let top = self.motion_blocking_height(portal[0], portal[2]);
            self.change_block([portal[0], top, portal[2]], egg, false, None);
        }
        tracing::info!("the ender dragon was slain");
    }

    // ------------------------------------------------------------ portal / gateways

    /// `EndPodiumFeature` at the exit portal location; returns its origin.
    fn spawn_exit_portal(&self, active: bool) -> [i32; 3] {
        let origin = {
            let f = self.fight.lock();
            f.portal
        }
        .unwrap_or_else(|| {
            // EndDragonFight: heightmap at the origin, skipping down through bedrock
            let mut y = self.motion_blocking_height(0, 0) - 1;
            while y > 63 && self.world.get_block(0, y, 0).is_some_and(|b| b.name() == "bedrock") {
                y -= 1;
            }
            [0, y.max(self.world_min_y() + 1), 0]
        });
        {
            let mut f = self.fight.lock();
            f.portal = Some(origin);
            f.dirty = true;
        }
        let (bedrock, end_stone, air) = (state("bedrock"), state("end_stone"), blocks::AIR);
        let portal = if active { state("end_portal") } else { air };
        for y in origin[1] - 1..=origin[1] + 32 {
            for x in origin[0] - 4..=origin[0] + 4 {
                for z in origin[2] - 4..=origin[2] + 4 {
                    let (dx, dy, dz) = ((x - origin[0]) as f64, (y - origin[1]) as f64, (z - origin[2]) as f64);
                    let d = dx * dx + dy * dy + dz * dz;
                    let flag = d <= 2.5 * 2.5;
                    if !flag && d > 3.5 * 3.5 {
                        continue;
                    }
                    let s = if y < origin[1] {
                        if flag {
                            bedrock
                        } else {
                            end_stone
                        }
                    } else if y > origin[1] {
                        air
                    } else if !flag {
                        bedrock
                    } else {
                        portal
                    };
                    self.change_block([x, y, z], s, false, None);
                }
            }
        }
        for dy in 0..4 {
            self.change_block([origin[0], origin[1] + dy, origin[2]], bedrock, false, None);
        }
        let torch = state("wall_torch");
        for (dx, dz, facing) in [(0, -1, "north"), (0, 1, "south"), (-1, 0, "west"), (1, 0, "east")] {
            self.change_block([origin[0] + dx, origin[1] + 2, origin[2] + dz], with(torch, "facing", facing), false, None);
        }
        origin
    }

    fn world_min_y(&self) -> i32 {
        if self.dim == Dim::Overworld {
            -64
        } else {
            0
        }
    }

    /// `spawnNewGateway`
    fn spawn_new_gateway(&self) {
        let i = {
            let mut f = self.fight.lock();
            f.dirty = true;
            f.gateways.pop()
        };
        let Some(i) = i else { return };
        let a = 2.0 * (-std::f64::consts::PI + std::f64::consts::PI / 20.0 * i as f64);
        let p = [(96.0 * a.cos()).floor() as i32, 75, (96.0 * a.sin()).floor() as i32];
        self.level_event(3000, p, 0);
        self.place_gateway(p, None, false);
    }

    /// `EndGatewayFeature`
    pub fn place_gateway(&self, o: [i32; 3], exit: Option<[i32; 3]>, exact: bool) {
        let (bedrock, gateway) = (state("bedrock"), state("end_gateway"));
        for dy in -2..=2 {
            for dx in -1..=1 {
                for dz in -1..=1 {
                    let p = [o[0] + dx, o[1] + dy, o[2] + dz];
                    let (sx, sy, sz, far) = (dx == 0, dy == 0, dz == 0, dy.abs() == 2);
                    if sx && sy && sz {
                        self.change_block(p, gateway, false, None);
                        self.world.set_block_entity(p, Some(crate::worldgen::feature::extra5::gateway_nbt(exit, exact)), true);
                    } else if sy {
                        self.change_block(p, blocks::AIR, false, None);
                    } else if (far && sx && sz) || ((sx || sz) && !far) {
                        self.change_block(p, bedrock, false, None);
                    } else {
                        self.change_block(p, blocks::AIR, false, None);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ respawn

    /// `EndDragonFight.tryRespawn`: four crystals on the exit portal's edges.
    pub fn try_respawn(&self) {
        let (portal, busy) = {
            let f = self.fight.lock();
            (f.portal, !f.killed || f.respawn.is_some())
        };
        if busy {
            return;
        }
        let Some(portal) = portal else { return };
        let c = [portal[0], portal[1] + 1, portal[2]];
        let mut found = Vec::new();
        for d in [[0, -1], [0, 1], [-1, 0], [1, 0]] {
            let q = [c[0] + d[0] * 3, c[1], c[2] + d[1] * 3];
            let bb = Aabb { min: [q[0] as f64, q[1] as f64, q[2] as f64], max: [q[0] as f64 + 1.0, q[1] as f64 + 1.0, q[2] as f64 + 1.0] };
            let here: Vec<i32> = self.entities.lock().values().filter(|e| matches!(e.kind, EKind::Crystal { .. }) && Aabb::entity(e.pos, 2.0, 2.0).intersects(&bb)).map(|e| e.eid).collect();
            if here.is_empty() {
                return;
            }
            found.extend(here);
        }
        tracing::info!("the dragon respawn ritual begins");
        // respawnDragon: the gateways the old exit portal used are cleared
        self.fight.lock().respawn = Some((Respawn::Start, 0, found));
    }

    fn spikes(&self) -> Vec<crate::worldgen::feature::extra5::Spike> {
        let seed = crate::worldgen::generator_for(self.dim.index() as usize).map(|g| g.seed).unwrap_or(0);
        crate::worldgen::feature::extra5::spikes_for_seed(seed)
    }

    fn tick_respawn(&self) {
        let Some((stage, time, crystals)) = self.fight.lock().respawn.clone() else { return };
        let mut next = (stage, time + 1);
        let top = |s: &crate::worldgen::feature::extra5::Spike| [s.x, s.height + 1, s.z];
        match stage {
            Respawn::Start => {
                if time == 0 {
                    for c in &crystals {
                        self.set_crystal(*c, |_, beam, _| *beam = Some([0, 128, 0]));
                    }
                }
                if time >= 150 {
                    next = (Respawn::Preparing, 0);
                }
            }
            Respawn::Preparing => {
                if time >= 100 {
                    next = (Respawn::Pillars, 0);
                }
            }
            Respawn::Pillars => {
                let spikes = self.spikes();
                let i = (time / 40) as usize;
                if i >= spikes.len() {
                    next = (Respawn::Summoning, 0);
                } else if time % 40 == 0 {
                    let s = spikes[i];
                    let t = top(&s);
                    for c in &crystals {
                        self.set_crystal(*c, |_, beam, _| *beam = Some(t));
                    }
                    // the old pillar blows up and a fresh one grows in its place
                    self.explode_full([s.x as f64 + 0.5, s.height as f64, s.z as f64 + 0.5], 5.0, false, None, Some(true));
                    self.place_spike(s);
                }
            }
            Respawn::Summoning => {
                if time == 0 {
                    for c in &crystals {
                        self.set_crystal(*c, |_, beam, _| *beam = Some([0, 128, 0]));
                    }
                }
                if time >= 100 {
                    // END: the ritual crystals explode, the dragon returns
                    self.fight.lock().respawn = None;
                    for c in &crystals {
                        let pos = self.entities.lock().get(c).map(|e| e.pos);
                        if let Some(pos) = pos {
                            self.entities.lock().remove(c);
                            self.broadcast(self.remove_packet(&[*c]));
                            self.explode_full(pos, 6.0, false, None, None);
                        }
                    }
                    self.reset_spike_crystals();
                    self.spawn_exit_portal(false);
                    let d = self.create_dragon();
                    let mut f = self.fight.lock();
                    f.dragon = Some(d);
                    f.killed = false;
                    f.dirty = true;
                    return;
                }
            }
        }
        if let Some(r) = self.fight.lock().respawn.as_mut() {
            r.0 = next.0;
            r.1 = next.1;
        }
    }

    /// `SpikeFeature.placeSpike` at run time.
    fn place_spike(&self, s: crate::worldgen::feature::extra5::Spike) {
        let (obsidian, air) = (state("obsidian"), blocks::AIR);
        let rad = s.radius;
        for y in 0..=s.height + 10 {
            for z in s.z - rad..=s.z + rad {
                for x in s.x - rad..=s.x + rad {
                    let (dx, dz) = ((x - s.x) as f64, (z - s.z) as f64);
                    if dx * dx + dz * dz <= (rad * rad + 1) as f64 && y < s.height {
                        self.change_block([x, y, z], obsidian, false, None);
                    } else if y > 65 {
                        if !self.world.get_block(x, y, z).is_some_and(|b| b.is_air()) {
                            self.change_block([x, y, z], air, false, None);
                        }
                    }
                }
            }
        }
        if s.guarded {
            let bars = state("iron_bars");
            for dx in -2i32..=2 {
                for dz in -2i32..=2 {
                    for dy in 0..=3 {
                        let top = dy == 3;
                        if dx.abs() == 2 || dz.abs() == 2 || top {
                            let wx = dx.abs() == 2 || top;
                            let wz = dz.abs() == 2 || top;
                            let b = |v: bool| if v { "true" } else { "false" };
                            let mut st = with(bars, "north", b(wx && dz != -2));
                            st = with(st, "south", b(wx && dz != 2));
                            st = with(st, "west", b(wz && dx != -2));
                            st = with(st, "east", b(wz && dx != 2));
                            self.change_block([s.x + dx, s.height + dy, s.z + dz], st, false, None);
                        }
                    }
                }
            }
        }
        let top = [s.x, s.height + 1, s.z];
        self.change_block([top[0], top[1] - 1, top[2]], state("bedrock"), false, None);
        self.change_block(top, state("fire"), false, None);
        let eid = self.spawn_crystal([top[0] as f64 + 0.5, top[1] as f64, top[2] as f64 + 0.5], true);
        self.set_crystal(eid, |_, beam, inv| {
            *beam = Some([0, 128, 0]);
            *inv = true;
        });
    }

    /// `resetSpikeCrystals`: pillar crystals become breakable again.
    fn reset_spike_crystals(&self) {
        for s in self.spikes() {
            let c = [s.x as f64 + 0.5, (s.height + 1) as f64, s.z as f64 + 0.5];
            let ids: Vec<i32> = self.entities.lock().values().filter(|e| matches!(e.kind, EKind::Crystal { .. }) && dist2(e.pos, c) < 4.0).map(|e| e.eid).collect();
            for id in ids {
                self.set_crystal(id, |_, beam, inv| {
                    *beam = None;
                    *inv = false;
                });
            }
        }
    }

    // ------------------------------------------------------------ boss bar

    fn boss_add(&self, p: &Player, uuid: uuid::Uuid, health: f32) {
        let mut b = Vec::new();
        b.extend_from_slice(uuid.as_bytes());
        b.vi(0);
        b.extend_from_slice(&crate::nbt::Tag::from_json(&serde_json::json!({"translate": "entity.minecraft.ender_dragon"})).to_network_bytes());
        // pink, progress, music + fog
        b.f32(health).vi(0).vi(0).u8(6);
        p.send(self.encode("boss_event", &b));
    }

    fn boss_remove(&self, p: &Player, uuid: uuid::Uuid) {
        let mut b = Vec::new();
        b.extend_from_slice(uuid.as_bytes());
        b.vi(1);
        p.send(self.encode("boss_event", &b));
    }

    fn remove_boss_bar(&self) {
        let (uuid, viewers) = {
            let mut f = self.fight.lock();
            (f.bar, std::mem::take(&mut f.viewers))
        };
        for p in self.players().into_iter().filter(|p| viewers.contains(&p.eid)) {
            self.boss_remove(&p, uuid);
        }
    }

    /// A player left this level: forget them as a boss bar viewer.
    pub fn fight_player_left(&self, p: &Player) {
        let (uuid, had) = {
            let mut f = self.fight.lock();
            (f.bar, f.viewers.remove(&p.eid))
        };
        if had {
            self.boss_remove(p, uuid);
        }
    }

    fn update_boss_bar(&self, players: &[Arc<Player>]) {
        let (uuid, dragon) = {
            let f = self.fight.lock();
            (f.bar, if f.killed { None } else { f.dragon })
        };
        let health = dragon.and_then(|d| self.with_dragon(d, |m| m.living.health / m.living.attrs.value(super::combat::attr::Attr::MaxHealth).max(1.0) as f32));
        let Some(health) = health else {
            self.remove_boss_bar();
            return;
        };
        let mut f = self.fight.lock();
        for p in players {
            let near = !p.vit.lock().dead() && dist2(p.pose.lock().pos, CENTRE) < BAR_RANGE * BAR_RANGE;
            let has = f.viewers.contains(&p.eid);
            if near && !has {
                f.viewers.insert(p.eid);
                self.boss_add(p, uuid, health);
            } else if !near && has {
                f.viewers.remove(&p.eid);
                self.boss_remove(p, uuid);
            }
        }
        f.viewers.retain(|v| players.iter().any(|p| p.eid == *v));
        if (health - f.bar_health).abs() > 1e-4 {
            f.bar_health = health;
            let mut b = Vec::new();
            b.extend_from_slice(uuid.as_bytes());
            b.vi(2).f32(health);
            let pkt = self.encode("boss_event", &b);
            for p in players.iter().filter(|p| f.viewers.contains(&p.eid)) {
                p.send(pkt.clone());
            }
        }
    }

    // ------------------------------------------------------------ tick

    /// `EndDragonFight.tick` (End only).
    pub(crate) fn tick_end_fight(&self) {
        if self.dim != Dim::End {
            return;
        }
        let players = self.players();
        if players.is_empty() {
            return;
        }
        // wait for the centre to be loaded
        if self.world.get_block(0, 64, 0).is_none() {
            return;
        }
        let now = self.now.load(Ordering::Relaxed);
        if !self.fight.lock().started {
            self.start_fight();
        }
        // dragon healing from the nearest crystal (EnderDragon.checkCrystals)
        let dragon = self.fight.lock().dragon;
        if let Some(d) = dragon {
            self.dragon_crystals(d);
        }
        if now % 20 == 0 {
            self.update_boss_bar(&players);
        } else if let Some(d) = dragon {
            // health changes show at once
            let h = self.with_dragon(d, |m| m.living.health / m.living.attrs.value(super::combat::attr::Attr::MaxHealth).max(1.0) as f32);
            let (cur, uuid) = {
                let f = self.fight.lock();
                (f.bar_health, f.bar)
            };
            if let Some(h) = h {
                if (h - cur).abs() > 1e-4 && !self.fight.lock().viewers.is_empty() {
                    self.fight.lock().bar_health = h;
                    let mut b = Vec::new();
                    b.extend_from_slice(uuid.as_bytes());
                    b.vi(2).f32(h);
                    let pkt = self.encode("boss_event", &b);
                    let viewers = self.fight.lock().viewers.clone();
                    for p in players.iter().filter(|p| viewers.contains(&p.eid)) {
                        p.send(pkt.clone());
                    }
                }
            }
        }
        if now % 100 == 0 {
            let n = self.entities.lock().values().filter(|e| matches!(e.kind, EKind::Crystal { .. }) && e.pos[0].abs() < 64.0 && e.pos[2].abs() < 64.0).count();
            self.fight.lock().crystals = n;
        }
        // a lost dragon is found again or replaced (ticksSinceDragonSeen)
        let lost = {
            let mut f = self.fight.lock();
            if !f.killed && f.respawn.is_none() {
                let alive = f.dragon.is_some_and(|d| self.entities.try_lock().map(|e| e.contains_key(&d)).unwrap_or(true));
                if alive {
                    f.since_seen = 0;
                    false
                } else {
                    f.since_seen += 1;
                    f.since_seen >= 1200
                }
            } else {
                false
            }
        };
        if lost {
            let found = self.dragons().first().copied();
            let d = found.unwrap_or_else(|| self.create_dragon());
            let mut f = self.fight.lock();
            f.dragon = Some(d);
            f.since_seen = 0;
        }
        self.tick_respawn();
    }

    /// The first tick with the centre loaded (`EndDragonFight` constructor +
    /// `scanState`).
    fn start_fight(&self) {
        let (fresh, killed) = {
            let mut f = self.fight.lock();
            f.started = true;
            if f.gateways.is_empty() && !f.previously_killed {
                let mut g: Vec<i32> = (0..20).collect();
                g.shuffle(&mut rand::thread_rng());
                f.gateways = g;
                f.dirty = true;
            }
            (f.portal.is_none(), f.killed)
        };
        let existing = self.dragons();
        if let Some(d) = existing.first() {
            let mut f = self.fight.lock();
            f.dragon = Some(*d);
            f.killed = false;
        } else if !killed {
            if fresh {
                self.spawn_exit_portal(false);
            }
            let d = self.create_dragon();
            let mut f = self.fight.lock();
            f.dragon = Some(d);
            f.dirty = true;
        } else if fresh {
            self.spawn_exit_portal(true);
        }
        let portal = self.fight.lock().portal;
        let dragon = self.fight.lock().dragon;
        if let (Some(d), Some(p)) = (dragon, portal) {
            self.with_dragon(d, |m| m.v.bound = Some(p));
        }
        let n = self.entities.lock().values().filter(|e| matches!(e.kind, EKind::Crystal { .. })).count();
        self.fight.lock().crystals = n;
    }

    /// `EnderDragon.checkCrystals`
    fn dragon_crystals(&self, d: i32) {
        let mut rng = rand::thread_rng();
        let Some((pos, cur, tick)) = ({
            let ents = self.entities.lock();
            match ents.get(&d) {
                Some(e) => match &e.kind {
                    EKind::Mob(m) if m.death == 0 => Some((e.pos, m.v.dragon_crystal, m.tick_count)),
                    _ => None,
                },
                None => None,
            }
        }) else {
            return;
        };
        let alive = cur.is_some_and(|c| self.entities.lock().get(&c).is_some_and(|e| matches!(e.kind, EKind::Crystal { .. })));
        if alive {
            if tick % 10 == 0 {
                self.with_mob(d, |l, _, _| {
                    if l.health < l.attrs.value(super::combat::attr::Attr::MaxHealth) as f32 {
                        l.heal(1.0);
                    }
                });
            }
        } else if cur.is_some() {
            self.with_dragon(d, |m| m.v.dragon_crystal = None);
        }
        if rng.gen_range(0..10) == 0 {
            let nearest = self
                .entities
                .lock()
                .values()
                .filter(|e| matches!(e.kind, EKind::Crystal { .. }) && dist2(e.pos, pos) < 32.0 * 32.0)
                .min_by(|a, b| dist2(a.pos, pos).total_cmp(&dist2(b.pos, pos)))
                .map(|e| e.eid);
            self.with_dragon(d, |m| m.v.dragon_crystal = nearest);
        }
    }

    pub fn end_crystal_count(&self) -> usize {
        self.fight.lock().crystals
    }

    pub fn exit_portal(&self) -> Option<[i32; 3]> {
        self.fight.lock().portal
    }

    // ------------------------------------------------------------ persistence

    pub fn load_fight(&self, path: &std::path::Path) {
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let mut f = self.fight.lock();
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let key = it.next().unwrap_or("");
            let nums: Vec<i32> = it.filter_map(|t| t.parse().ok()).collect();
            match key {
                "killed" => f.killed = nums.first() == Some(&1),
                "previously_killed" => f.previously_killed = nums.first() == Some(&1),
                "portal" if nums.len() == 3 => f.portal = Some([nums[0], nums[1], nums[2]]),
                "gateways" => f.gateways = nums,
                _ => {}
            }
        }
    }

    pub fn save_fight(&self, path: &std::path::Path) -> std::io::Result<()> {
        let text = {
            let mut f = self.fight.lock();
            if !f.dirty && !f.started {
                return Ok(());
            }
            f.dirty = false;
            let mut s = format!("killed {}\nprevious_killed {}\n", f.killed as i32, f.previously_killed as i32).replace("previous_killed", "previously_killed");
            if let Some(p) = f.portal {
                s += &format!("portal {} {} {}\n", p[0], p[1], p[2]);
            }
            s += "gateways";
            for g in &f.gateways {
                s += &format!(" {g}");
            }
            s += "\n";
            s
        };
        std::fs::write(path, text)
    }

    // ------------------------------------------------------------ gateways

    /// `TheEndGatewayBlockEntity.teleportEntity`: where a gateway sends you.
    /// Inner gateways without an exit find the outer islands along their
    /// direction and get a return gateway there.
    pub fn gateway_exit(&self, at: [i32; 3]) -> Option<([f64; 3], bool)> {
        let be = self.world.block_entity(at);
        let exact = be.as_ref().and_then(|t| t.get("ExactTeleport")).is_some();
        let exit = be.as_ref().and_then(|t| match t.get("exit_portal") {
            Some(crate::nbt::Tag::IntArray(a)) if a.len() == 3 => Some([a[0], a[1], a[2]]),
            _ => None,
        });
        let exit = exit?;
        if exact {
            return Some(([exit[0] as f64 + 0.5, exit[1] as f64, exit[2] as f64 + 0.5], true));
        }
        // findExitPosition: a free two-block space near the exit
        for dy in 0..16 {
            for (dx, dz) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, -1), (1, -1), (-1, 1)] {
                let p = [exit[0] + dx, exit[1] + dy, exit[2] + dz];
                let free = |q: [i32; 3]| self.world.get_block(q[0], q[1], q[2]).is_some_and(|b| b.is_air());
                let floor = self.world.get_block(p[0], p[1] - 1, p[2]).is_some_and(|b| !b.is_air() && b.name() != "end_gateway");
                if free(p) && free([p[0], p[1] + 1, p[2]]) && floor {
                    return Some(([p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5], false));
                }
            }
        }
        Some(([exit[0] as f64 + 0.5, exit[1] as f64 + 1.0, exit[2] as f64 + 0.5], false))
    }

    /// For an inner gateway without an exit: the chunk area to load before
    /// `make_gateway_exit` (`findExitPortalXZPosTentative`).
    pub fn gateway_exit_target(&self, at: [i32; 3]) -> Option<(i32, i32)> {
        let be = self.world.block_entity(at);
        if be.as_ref().and_then(|t| t.get("exit_portal")).is_some() {
            return None;
        }
        let l = ((at[0] * at[0] + at[2] * at[2]) as f64).sqrt().max(1e-6);
        let (nx, nz) = (at[0] as f64 / l, at[2] as f64 / l);
        Some(((nx * 1024.0) as i32, (nz * 1024.0) as i32))
    }

    /// Finds land near `target` (loaded), builds a return gateway there and
    /// records it as the inner gateway's exit.
    pub fn make_gateway_exit(&self, at: [i32; 3], target: (i32, i32)) {
        let (cx, cz) = (target.0 >> 4, target.1 >> 4);
        // findTallestBlock over the chunks around the target
        let mut best: Option<[i32; 3]> = None;
        for qx in (cx - 1) * 16..(cx + 2) * 16 {
            for qz in (cz - 1) * 16..(cz + 2) * 16 {
                let y = self.motion_blocking_height(qx, qz) - 1;
                if y <= 30 {
                    continue;
                }
                if self.world.get_block(qx, y, qz).is_some_and(|b| b.name() == "end_stone") {
                    let d = (qx - target.0).pow(2) + (qz - target.1).pow(2);
                    let better = match best {
                        None => true,
                        Some(b) => y > b[1] || (y == b[1] && d < (b[0] - target.0).pow(2) + (b[2] - target.1).pow(2)),
                    };
                    if better {
                        best = Some([qx, y, qz]);
                    }
                }
            }
        }
        let land = best.unwrap_or_else(|| {
            // no island: grow a small one (EndIslandFeature)
            let c = [target.0, 75, target.1];
            let mut rng = rand::thread_rng();
            let mut r = rng.gen_range(0..3) as f32 + 4.0;
            let es = state("end_stone");
            let mut y = 0;
            while r > 0.5 {
                for dx in (-r).floor() as i32..=r.ceil() as i32 {
                    for dz in (-r).floor() as i32..=r.ceil() as i32 {
                        if ((dx * dx + dz * dz) as f32) <= (r + 1.0) * (r + 1.0) {
                            self.change_block([c[0] + dx, c[1] + y, c[2] + dz], es, false, None);
                        }
                    }
                }
                r -= rng.gen_range(0..2) as f32 + 0.5;
                y -= 1;
            }
            c
        });
        let gate = [land[0], land[1] + 10, land[2]];
        self.place_gateway(gate, Some(at), false);
        // the inner gateway now leads to the new one
        let mut t = self.world.block_entity(at).unwrap_or_else(|| crate::worldgen::feature::extra5::gateway_nbt(None, false));
        if let crate::nbt::Tag::Compound(c) = &mut t {
            c.retain(|(k, _)| k != "exit_portal");
            c.push(("exit_portal".into(), crate::nbt::Tag::IntArray(vec![gate[0], gate[1], gate[2]])));
        }
        self.world.set_block_entity(at, Some(t), true);
    }

    /// Gateway beam when an entity passes (`TheEndGatewayBlockEntity.triggerCooldown`).
    pub fn gateway_beam(&self, at: [i32; 3]) {
        let mut b = Vec::new();
        let id = crate::registry::Registries::get().id("minecraft:block", "minecraft:end_gateway").unwrap_or(0);
        b.pos(at[0], at[1], at[2]).u8(1).u8(0).vi(id);
        self.broadcast_chunk((at[0] >> 4, at[2] >> 4), &self.encode("block_event", &b));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::generator::Generator;
    use crate::world::storage::WorldStore;

    /// Needs the server directory with the vanilla cache: RIME_SERVER_DIR.
    fn end_game() -> Option<(Game, Generator, WorldStore, std::path::PathBuf)> {
        let dir = std::path::PathBuf::from(std::env::var("RIME_SERVER_DIR").ok()?);
        let vanilla = crate::vanilla::VanillaData::ensure(&dir).ok()?;
        let cd = crate::net::config_data::ConfigData::build(&vanilla).ok()?;
        crate::components::set_data_registries(cd.names());
        crate::game::combat::load(&vanilla).ok()?;
        let data = crate::data::GameData::load(&vanilla).ok()?;
        let g = Game::new_dim(256, Arc::new(data), Dim::End);
        let generator = Generator::new(1, false, 1);
        let tmp = std::env::temp_dir().join(format!("rime-fight-test-{}", std::process::id()));
        let store = WorldStore::open(&tmp, crate::config::WorldFormat::Airworld, 3, true, 64).ok()?;
        let all: Vec<(i32, i32)> = (-8..=8).flat_map(|x| (-8..=8).map(move |z| (x, z))).collect();
        g.world.ensure_loaded(&all, &store, &generator);
        Some((g, generator, store, tmp))
    }

    #[test]
    fn dragon_fight_lifecycle() {
        let Some((g, _gen, _store, tmp)) = end_game() else {
            eprintln!("dragon fight test skipped");
            return;
        };
        eprintln!("dragon fight test running");
        g.start_fight();
        let d = g.fight.lock().dragon.expect("a dragon spawns");
        let portal = g.fight.lock().portal.expect("an exit portal");
        assert_eq!(g.world.get_block(portal[0], portal[1] + 3, portal[2]).map(|b| b.name()), Some("bedrock"));
        assert_eq!(g.world.get_block(portal[0] + 1, portal[1], portal[2]).map(|b| b.name()), Some("air"), "inactive portal");
        // parts map to the dragon
        assert_eq!(g.dragon_part(d + 1).map(|p| (p.0, p.1)), Some((d, 1)));
        assert_eq!(g.dragon_part(d + 8).map(|p| (p.0, p.1)), Some((d, 8)));
        // no damage from plain sources
        assert!(!g.hurt_mob(d, &DamageSource::new("magic"), 50.0));
        // explosions hurt it; the body takes a quarter (+1)
        let before = g.health_of(d).unwrap();
        assert!(g.hurt_mob(d, &DamageSource::new("explosion"), 40.0));
        let after = g.health_of(d).unwrap();
        assert!((before - after - 11.0).abs() < 0.01, "{before} -> {after}");
        // kill it, then run the death animation
        for _ in 0..200 {
            g.with_dragon(d, |m| {
                m.living.hurt_time = 0;
                m.living.invulnerable_time = 0;
            });
            if g.hurt_mob(d, &DamageSource::new("explosion"), 1000.0) && g.health_of(d).unwrap_or(0.0) <= 0.0 {
                break;
            }
        }
        let mut done = false;
        for _ in 0..205 {
            let mut ents = g.entities.lock();
            let e = ents.get_mut(&d).expect("dragon");
            let mut pos = e.pos;
            let EKind::Mob(m) = &mut e.kind else { panic!() };
            assert!(m.death > 0, "dragon died");
            m.death += 1;
            let mut mm = std::mem::replace(m, Box::new(crate::game::mob::Mob::new(m.ty, true)));
            drop(ents);
            done = g.dragon_death_tick(d, &mut pos, &mut mm);
            let mut ents = g.entities.lock();
            if let Some(EKind::Mob(m)) = ents.get_mut(&d).map(|e| &mut e.kind) {
                *m = mm;
            }
            if done {
                break;
            }
        }
        assert!(done);
        let f = g.fight.lock();
        assert!(f.killed && f.previously_killed);
        assert_eq!(f.gateways.len(), 19);
        drop(f);
        assert_eq!(g.world.get_block(portal[0] + 1, portal[1], portal[2]).map(|b| b.name()), Some("end_portal"));
        let top = g.motion_blocking_height(portal[0], portal[2]);
        assert_eq!(g.world.get_block(portal[0], top - 1, portal[2]).map(|b| b.name()), Some("dragon_egg"));
        let _ = std::fs::remove_dir_all(tmp);
    }
}
