//! Riding: passengers and vehicles (`Entity.startRiding` / `stopRiding`),
//! boats, saddled and tamed mounts, item-steered pigs and striders.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::Rng;

use super::combat::living::SADDLE;
use super::entity::{EKind, Entity};
use super::hurt::F_SNEAKING;
use super::{Game, Player};
use crate::anticheat::combat::Aabb;
use crate::data::{Items, Stack};
use crate::net::codec::Out;

#[derive(Default)]
pub struct Rides {
    pub vehicle_of: HashMap<i32, i32>,
    pub passengers: HashMap<i32, Vec<i32>>,
}

/// Boat and raft entity types (each has a chest variant).
pub fn is_boat_type(name: &str) -> bool {
    name.ends_with("_boat") || name.ends_with("_raft")
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

impl Game {
    // ------------------------------------------------------------ bookkeeping

    pub fn vehicle_of(&self, eid: i32) -> Option<i32> {
        self.rides.lock().vehicle_of.get(&eid).copied()
    }

    pub fn passengers_of(&self, v: i32) -> Vec<i32> {
        self.rides.lock().passengers.get(&v).cloned().unwrap_or_default()
    }

    pub(crate) fn passengers_packet(&self, v: i32) -> Arc<Vec<u8>> {
        let list = self.passengers_of(v);
        let mut b = Vec::new();
        b.vi(v).vi(list.len() as i32);
        for p in list {
            b.vi(p);
        }
        self.encode("set_passengers", &b)
    }

    pub(crate) fn entity_chunk(&self, eid: i32) -> Option<(i32, i32)> {
        if let Some(e) = self.entities.lock().get(&eid) {
            return Some(e.chunk());
        }
        self.players().into_iter().find(|p| p.eid == eid).map(|p| {
            let q = p.pose.lock().pos;
            ((q[0].floor() as i32) >> 4, (q[2].floor() as i32) >> 4)
        })
    }

    fn broadcast_passengers(&self, v: i32) {
        if let Some(c) = self.entity_chunk(v) {
            self.broadcast_chunk(c, &self.passengers_packet(v));
        }
    }

    /// How many may ride `v` (`canAddPassenger`).
    fn capacity(&self, v: i32) -> usize {
        let ents = self.entities.lock();
        match ents.get(&v).map(|e| &e.kind) {
            Some(EKind::Boat { ty, .. }) => {
                if ty.contains("chest") {
                    1
                } else {
                    2
                }
            }
            Some(EKind::Minecart(c)) if c.ty == "minecart" => 1,
            Some(EKind::Mob(m)) => match m.ty.name() {
                "camel" | "camel_husk" => 2,
                "happy_ghast" => 4,
                _ => 1,
            },
            _ => 0,
        }
    }

    /// `startRiding`. False when there is no room or the vehicle is gone.
    pub fn mount(&self, rider: i32, v: i32) -> bool {
        if rider == v || self.vehicle_of(v) == Some(rider) {
            return false;
        }
        let cap = self.capacity(v);
        if self.passengers_of(v).len() >= cap {
            return false;
        }
        if self.vehicle_of(rider).is_some() {
            self.dismount(rider);
        }
        {
            let mut r = self.rides.lock();
            r.vehicle_of.insert(rider, v);
            r.passengers.entry(v).or_default().push(rider);
        }
        if let Some(p) = self.players().into_iter().find(|p| p.eid == rider) {
            p.flags.fetch_or(super::hurt::F_PASSENGER, Ordering::Relaxed);
            let vp = self.entities.lock().get(&v).map(|e| e.pos);
            if let Some(vp) = vp {
                p.pose.lock().pos = vp;
            }
        }
        self.broadcast_passengers(v);
        true
    }

    /// `stopRiding`, putting the rider down next to the vehicle.
    pub fn dismount(&self, rider: i32) {
        let v = {
            let mut r = self.rides.lock();
            let Some(v) = r.vehicle_of.remove(&rider) else { return };
            if let Some(l) = r.passengers.get_mut(&v) {
                l.retain(|x| *x != rider);
                if l.is_empty() {
                    r.passengers.remove(&v);
                }
            }
            v
        };
        self.broadcast_passengers(v);
        let vpos = self.entities.lock().get(&v).map(|e| (e.pos, e.size()));
        let player = self.players().into_iter().find(|p| p.eid == rider);
        if let Some(p) = &player {
            p.flags.fetch_and(!super::hurt::F_PASSENGER, Ordering::Relaxed);
        }
        let Some((vp, (half, h))) = vpos else { return };
        let to = self.dismount_spot(vp, half, h);
        match player {
            Some(p) => {
                let (yaw, pitch) = {
                    let pose = p.pose.lock();
                    (pose.yaw, pose.pitch)
                };
                p.pose.lock().pos = to;
                p.requests.lock().push(super::Request::Teleport(to, yaw, pitch));
            }
            None => {
                if let Some(e) = self.entities.lock().get_mut(&rider) {
                    e.pos = to;
                }
            }
        }
    }

    /// Everyone off (the vehicle died or was removed).
    pub fn eject_all(&self, v: i32) {
        for p in self.passengers_of(v) {
            self.dismount(p);
        }
    }

    /// `getDismountLocationForPassenger`: a free 2-block space beside the vehicle.
    fn dismount_spot(&self, vp: [f64; 3], half: f64, h: f64) -> [f64; 3] {
        let d = half + 0.8;
        let free = |q: [f64; 3]| {
            let b = [q[0].floor() as i32, q[1].floor() as i32, q[2].floor() as i32];
            let ok = |y: i32| self.world.get_block(b[0], y, b[2]).is_some_and(|s| !super::ai::path::blocks_motion(s));
            let floor = self.world.get_block(b[0], b[1] - 1, b[2]).is_some_and(|s| super::ai::path::blocks_motion(s) || s.name() == "water");
            ok(b[1]) && ok(b[1] + 1) && floor
        };
        for (dx, dz) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (1.0, 1.0), (-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0)] {
            for dy in [0.0, 1.0, -1.0] {
                let q = [vp[0] + dx * d, (vp[1] + dy).floor(), vp[2] + dz * d];
                if free(q) {
                    return [q[0], q[1], q[2]];
                }
            }
        }
        [vp[0], vp[1] + h, vp[2]]
    }

    /// The player steering `v` (`getControllingPassenger`), if any.
    pub fn controller(&self, v: i32) -> Option<Arc<Player>> {
        let first = *self.passengers_of(v).first()?;
        let p = self.players().into_iter().find(|p| p.eid == first)?;
        let ents = self.entities.lock();
        let e = ents.get(&v)?;
        let ok = match &e.kind {
            EKind::Boat { .. } => true,
            EKind::Mob(m) => {
                let held = |name: &str| {
                    let eq = p.equipment();
                    eq[0].is_some_and(|s| Items::get().name(s.item) == name) || eq[1].is_some_and(|s| Items::get().name(s.item) == name)
                };
                let saddled = m.eq[SADDLE].is_some();
                match m.ty.name() {
                    "pig" => saddled && held("carrot_on_a_stick"),
                    "strider" => saddled && held("warped_fungus_on_a_stick"),
                    "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" => saddled && m.tame,
                    "camel" | "camel_husk" => saddled,
                    "happy_ghast" => m.eq[super::combat::living::BODY].is_some(),
                    _ => false,
                }
            }
            _ => false,
        };
        ok.then_some(p)
    }

    /// `ServerboundMoveVehiclePacket` from the steering player.
    pub fn move_vehicle(&self, p: &Arc<Player>, pos: [f64; 3], yaw: f32, pitch: f32, on_ground: bool) {
        let Some(v) = self.vehicle_of(p.eid) else { return };
        if self.controller(v).is_none_or(|c| c.eid != p.eid) {
            return;
        }
        let mut ents = self.entities.lock();
        let Some(e) = ents.get_mut(&v) else { return };
        // ignore absurd jumps (the client is resynced by the next teleport)
        if dist2(e.pos, pos) > 100.0 * 100.0 || pos.iter().any(|x| !x.is_finite()) {
            return;
        }
        e.vel = [pos[0] - e.pos[0], pos[1] - e.pos[1], pos[2] - e.pos[2]];
        e.pos = pos;
        e.on_ground = on_ground;
        // Player.checkRidingStatistics
        let d = (e.vel[0] * e.vel[0] + e.vel[1] * e.vel[1] + e.vel[2] * e.vel[2]).sqrt();
        let cm = (d * 100.0).round() as i32;
        let stat = match e.type_name().as_str() {
            t if t.ends_with("minecart") => Some("minecart_one_cm"),
            t if t.ends_with("boat") || t.ends_with("raft") => Some("boat_one_cm"),
            "pig" => Some("pig_one_cm"),
            "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" | "llama" | "trader_llama" | "camel" | "camel_husk" => Some("horse_one_cm"),
            "strider" => Some("strider_one_cm"),
            "happy_ghast" => Some("happy_ghast_one_cm"),
            "nautilus" | "zombie_nautilus" => Some("nautilus_one_cm"),
            _ => None,
        };
        match &mut e.kind {
            EKind::Boat { yaw: y, .. } => *y = yaw,
            EKind::Mob(m) => {
                m.mo.yaw = yaw;
                m.mo.body_yaw = yaw;
                m.mo.head_yaw = yaw;
                m.mo.pitch = pitch;
            }
            _ => {}
        }
        drop(ents);
        p.pose.lock().pos = pos;
        if let Some(st) = stat.filter(|_| cm > 0) {
            p.award_custom(st, cm);
        }
    }

    /// Per tick: passengers follow their vehicles; untamed horses buck
    /// their riders off or give in; boosts run out.
    pub(crate) fn tick_rides(&self) {
        let list: Vec<(i32, Vec<i32>)> = self.rides.lock().passengers.iter().map(|(v, l)| (*v, l.clone())).collect();
        let players = self.players();
        let mut rng = rand::thread_rng();
        for (v, riders) in list {
            let Some((vp, alive)) = self.entities.lock().get(&v).map(|e| (e.pos, !matches!(&e.kind, EKind::Mob(m) if m.death > 0))) else {
                // vehicle gone: riders stay where they are
                let mut r = self.rides.lock();
                r.passengers.remove(&v);
                for x in &riders {
                    r.vehicle_of.remove(x);
                }
                drop(r);
                for p in players.iter().filter(|p| riders.contains(&p.eid)) {
                    p.flags.fetch_and(!super::hurt::F_PASSENGER, Ordering::Relaxed);
                }
                continue;
            };
            if !alive {
                self.eject_all(v);
                continue;
            }
            for (i, r) in riders.iter().enumerate() {
                if let Some(p) = players.iter().find(|p| p.eid == *r) {
                    if p.vit.lock().dead() {
                        self.dismount(*r);
                        continue;
                    }
                    p.pose.lock().pos = vp;
                } else if let Some(e) = self.entities.lock().get_mut(r) {
                    e.pos = [vp[0] + if i == 1 { -0.5 } else { 0.0 }, vp[1] + 0.4, vp[2]];
                    e.vel = [0.0; 3];
                }
            }
            // RunAroundLikeCrazyGoal: an untamed horse tests its rider
            let horse = {
                let ents = self.entities.lock();
                match ents.get(&v).map(|e| &e.kind) {
                    Some(EKind::Mob(m)) if m.ty.is_horse() && !m.tame && !matches!(m.ty.name(), "camel" | "camel_husk" | "llama" | "trader_llama") => Some(m.v.temper),
                    _ => None,
                }
            };
            if let (Some(temper), Some(rider)) = (horse, riders.first().and_then(|r| players.iter().find(|p| p.eid == *r))) {
                if rng.gen_range(0..50) == 0 {
                    if temper > 0 && rng.gen_range(0..100) < temper {
                        // tameWithName
                        let uuid = rider.uuid;
                        let pkt = {
                            let mut ents = self.entities.lock();
                            match ents.get_mut(&v).map(|e| &mut e.kind) {
                                Some(EKind::Mob(m)) => {
                                    m.tame = true;
                                    m.owner = Some(uuid);
                                    m.persistent = true;
                                    Some(self.mob_meta(v, m))
                                }
                                _ => None,
                            }
                        };
                        if let (Some(pkt), Some(c)) = (pkt, self.entity_chunk(v)) {
                            self.broadcast_chunk(c, &pkt);
                            self.broadcast_chunk(c, &self.entity_event(v, 7));
                        }
                    } else {
                        // throw the rider off and rear
                        self.with_mob(v, |_, _, _| ());
                        {
                            let mut ents = self.entities.lock();
                            if let Some(EKind::Mob(m)) = ents.get_mut(&v).map(|e| &mut e.kind) {
                                m.v.temper += 5;
                                m.v.standing = 20;
                            }
                        }
                        self.eject_all(v);
                        if let Some(c) = self.entity_chunk(v) {
                            self.broadcast_chunk(c, &self.entity_event(v, 6));
                        }
                        let snd = if self.entities.lock().get(&v).is_some_and(|e| matches!(&e.kind, EKind::Mob(m) if m.ty.is("donkey") || m.ty.is("mule"))) { "minecraft:entity.donkey.angry" } else { "minecraft:entity.horse.angry" };
                        self.play_sound(snd, 6, vp, 1.0, 1.0);
                    }
                }
            }
        }
    }

    /// Right click on a mob with riding in mind: saddling, then mounting.
    /// None when the click is not about riding.
    pub fn ride_interact(&self, p: &Arc<Player>, eid: i32, held: Option<Stack>, slot: usize) -> Option<bool> {
        let name = held.map(|s| Items::get().name(s.item)).unwrap_or("");
        let sneaking = p.flags.load(Ordering::Relaxed) & F_SNEAKING != 0;
        let creative = p.gamemode.load(Ordering::Relaxed) == 1;
        let (ty, baby, tame, saddled, harnessed, food) = {
            let ents = self.entities.lock();
            let EKind::Mob(m) = &ents.get(&eid)?.kind else { return None };
            if m.death > 0 {
                return None;
            }
            let food = held.is_some_and(|s| super::ai::mobs::food_tag(m.ty.name()).is_some_and(|t| self.data.item_in(t, s.item)));
            (m.ty, m.is_baby(), m.tame, m.eq[SADDLE].is_some(), m.eq[super::combat::living::BODY].is_some(), food)
        };
        let n = ty.name();
        let saddleable = matches!(n, "pig" | "strider" | "camel" | "camel_husk") || (ty.is_horse() && tame && !matches!(n, "llama" | "trader_llama"));
        // Saddleable / Equipable: put a saddle on
        if name == "saddle" && saddleable && !saddled && !baby {
            let pkt = {
                let mut ents = self.entities.lock();
                let EKind::Mob(m) = &mut ents.get_mut(&eid)?.kind else { return None };
                m.eq[SADDLE] = held.map(|s| Stack { count: 1, ..s });
                m.persistent = true;
                self.mob_equipment(eid, m)
            };
            if let Some(c) = self.entity_chunk(eid) {
                self.broadcast_chunk(c, &pkt);
            }
            if !creative {
                p.inv.lock().consume(slot, 1);
                self.flush_inventory(p);
            }
            let snd = match n {
                "pig" => "minecraft:entity.pig.saddle",
                "strider" => "minecraft:entity.strider.saddle",
                "camel" | "camel_husk" => "minecraft:entity.camel.saddle",
                _ => "minecraft:entity.horse.saddle",
            };
            let pos = self.entities.lock().get(&eid).map(|e| e.pos).unwrap_or([0.0; 3]);
            self.play_sound(snd, 6, pos, 0.5, 1.0);
            return Some(true);
        }
        if food || sneaking || baby {
            return None;
        }
        let rideable = match n {
            "pig" | "strider" => saddled && (held.is_none() || matches!(name, "carrot_on_a_stick" | "warped_fungus_on_a_stick")),
            "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" => held.is_none() || (!tame && name != "saddle"),
            "camel" | "camel_husk" => held.is_none(),
            "llama" | "trader_llama" => tame && held.is_none(),
            "happy_ghast" => harnessed && held.is_none(),
            _ => false,
        };
        if !rideable {
            return None;
        }
        Some(self.mount(p.eid, eid))
    }

    /// `ItemSteerable.boost` on a saddled pig / strider being steered.
    pub fn boost(&self, v: i32, stick: &str) -> bool {
        let want = if stick == "carrot_on_a_stick" { "pig" } else { "strider" };
        let pkt = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&v) else { return false };
            let chunk = e.chunk();
            let EKind::Mob(m) = &mut e.kind else { return false };
            if !m.ty.is(want) || m.eq[SADDLE].is_none() {
                return false;
            }
            m.v.boost = rand::thread_rng().gen_range(0..841) + 140;
            (chunk, self.mob_meta(v, m))
        };
        self.broadcast_chunk(pkt.0, &pkt.1);
        true
    }

    /// `Boat.setPaddleState` (seen by the other players).
    pub fn paddle(&self, v: i32, left: bool, right: bool) {
        let Some(c) = self.entities.lock().get(&v).filter(|e| matches!(e.kind, EKind::Boat { .. })).map(|e| e.chunk()) else { return };
        let mut b = Vec::new();
        b.vi(v).u8(11).vi(8).bool(left).u8(12).vi(8).bool(right).u8(0xFF);
        self.broadcast_chunk(c, &self.encode("set_entity_data", &b));
    }

    // ------------------------------------------------------------ boats

    /// `BoatItem.use`: put a boat on the water or ground the player looks at.
    pub fn place_boat(&self, item: &str, at: [f64; 3], yaw: f32) -> bool {
        let ty: &'static str = Box::leak(item.to_string().into_boxed_str());
        let bb = Aabb::entity(at, 1.375, 0.5625);
        let world = super::ai::control::World(&|x, y, z| self.world.get_block(x, y, z));
        if !super::physics::no_collision(&world, &bb.inflate(-0.1, -0.1, -0.1)) {
            return false;
        }
        let eid = self.next_eid();
        self.add_entity(Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Boat { ty, yaw, damage: 0.0, hurt: 0, hurt_dir: 1, items: vec![None; if ty.contains("chest") { 27 } else { 0 }] }, pos: at, vel: [0.0; 3], on_ground: false, age: 0, synced: at });
        self.play_sound("minecraft:entity.boat.paddle_water", 4, at, 1.0, 1.0);
        true
    }

    pub(crate) fn boat_meta(&self, eid: i32, hurt: i32, dir: i32, damage: f32) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid).u8(8).vi(1).vi(hurt).u8(9).vi(1).vi(dir).u8(10).vi(3).f32(damage).u8(0xFF);
        self.encode("set_entity_data", &b)
    }

    /// `Containers.dropContents` for an entity's slots.
    pub(crate) fn spill(&self, pos: [f64; 3], items: &[Option<Stack>]) {
        use rand::Rng;
        if items.is_empty() {
            return;
        }
        let mut rng = rand::thread_rng();
        for s in items.iter().flatten() {
            self.spawn_item([pos[0], pos[1] + 0.3, pos[2]], *s, [rng.gen_range(-0.1..0.1), 0.2, rng.gen_range(-0.1..0.1)], 10);
        }
    }

    /// `VehicleEntity.hurtServer` by a player.
    pub fn hit_boat(&self, p: Option<&Arc<Player>>, eid: i32, amount: f32) -> bool {
        let creative = p.is_some_and(|p| p.gamemode.load(Ordering::Relaxed) == 1);
        let (broken, pkt, chunk, pos, ty) = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return false };
            let chunk = e.chunk();
            let pos = e.pos;
            let EKind::Boat { ty, damage, hurt, hurt_dir, .. } = &mut e.kind else { return false };
            *hurt_dir = -*hurt_dir;
            *hurt = 10;
            *damage += amount * 10.0;
            let broken = creative || *damage > 40.0;
            (broken, self.boat_meta(eid, *hurt, *hurt_dir, *damage), chunk, pos, *ty)
        };
        self.broadcast_chunk(chunk, &pkt);
        if broken {
            self.eject_all(eid);
            self.close_viewers([super::block::container::ENTITY_KEY, eid, 0]);
            let gone = self.entities.lock().remove(&eid);
            self.broadcast_chunk(chunk, &self.remove_packet(&[eid]));
            if let Some(EKind::Boat { items, .. }) = gone.map(|e| e.kind) {
                self.spill(pos, &items);
            }
            if !creative {
                if let Some(id) = Items::get().id(ty) {
                    self.spawn_item(pos, Stack::new(id, 1), [0.0, 0.1, 0.0], 10);
                }
            }
        }
        true
    }

    /// Boat motion when nobody steers it: floats, drifts, slows down.
    pub(crate) fn boat_physics(&self, e: &mut Entity) {
        let cell = |x: i32, y: i32, z: i32| self.cell(x, y, z);
        let water_at = |y: f64| self.world.get_block(e.pos[0].floor() as i32, y.floor() as i32, e.pos[2].floor() as i32).is_some_and(|b| super::fluid::fluid_state(b).fluid() == Some(super::fluid::Fluid::Water));
        let under = water_at(e.pos[1] + 0.1);
        let floating = !under && water_at(e.pos[1] - 0.1);
        if let EKind::Boat { hurt, .. } = &mut e.kind {
            if *hurt > 0 {
                *hurt -= 1;
            }
        }
        if under {
            // Boat.floatBoat: rise to the surface
            e.vel[1] = (e.vel[1] + 0.06).min(0.1);
            e.vel[0] *= 0.9;
            e.vel[2] *= 0.9;
        } else if floating {
            e.vel[1] = 0.0;
            e.vel[0] *= 0.9;
            e.vel[2] *= 0.9;
        } else {
            e.vel[1] -= 0.04;
        }
        let (ground, _) = super::entity::move_entity(&mut e.pos, &mut e.vel, 0.6875, 0.5625, &cell);
        e.on_ground = ground;
        if ground {
            e.vel[0] *= 0.5;
            e.vel[2] *= 0.5;
        }
    }
}
