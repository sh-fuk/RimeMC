//! Minecarts (`AbstractMinecart` with `OldMinecartBehavior`): rail
//! following, slopes, powered/activator/detector rails, riding, furnace and
//! TNT minecarts.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::Rng;

use super::entity::{EKind, Entity};
use super::{Game, Player};
use crate::anticheat::combat::Aabb;
use crate::data::{Items, Stack};
use crate::net::codec::Out;
use crate::registry::BlockState;

#[derive(Clone, Debug)]
pub struct Cart {
    pub ty: &'static str,
    pub yaw: f32,
    pub pitch: f32,
    pub damage: f32,
    pub hurt: i32,
    pub hurt_dir: i32,
    /// TNT minecart fuse (-1 = not lit)
    pub fuse: i32,
    /// furnace minecart fuel and push direction
    pub fuel: i32,
    pub push: [f64; 2],
    /// chest (27) / hopper (5) minecart contents
    pub items: Vec<Option<Stack>>,
    /// hopper minecart: off while on a powered activator rail
    pub enabled: bool,
}

impl Cart {
    pub fn new(ty: &'static str) -> Self {
        let size = match ty {
            "chest_minecart" => 27,
            "hopper_minecart" => 5,
            _ => 0,
        };
        Self { ty, yaw: 0.0, pitch: 0.0, damage: 0.0, hurt: 0, hurt_dir: 1, fuse: -1, fuel: 0, push: [0.0; 2], items: vec![None; size], enabled: true }
    }
}

/// What a cart tick asks the level to do once the entity lock is free.
pub enum CartEv {
    Explode([f64; 3], f64),
    Eject(i32),
    Primed([f64; 3]),
}

pub fn is_cart_item(name: &str) -> bool {
    matches!(name, "minecart" | "chest_minecart" | "furnace_minecart" | "tnt_minecart" | "hopper_minecart" | "command_block_minecart")
}

fn is_rail(s: BlockState) -> bool {
    matches!(s.name(), "rail" | "powered_rail" | "detector_rail" | "activator_rail")
}

/// `RailShape` exits: two offsets per shape (`AbstractMinecart.EXITS`).
fn exits(shape: &str) -> [[i32; 3]; 2] {
    match shape {
        "north_south" => [[0, 0, -1], [0, 0, 1]],
        "east_west" => [[-1, 0, 0], [1, 0, 0]],
        "ascending_east" => [[-1, -1, 0], [1, 0, 0]],
        "ascending_west" => [[-1, 0, 0], [1, -1, 0]],
        "ascending_north" => [[0, 0, -1], [0, -1, 1]],
        "ascending_south" => [[0, -1, -1], [0, 0, 1]],
        "south_east" => [[0, 0, 1], [1, 0, 0]],
        "south_west" => [[0, 0, 1], [-1, 0, 0]],
        "north_west" => [[0, 0, -1], [-1, 0, 0]],
        _ => [[0, 0, -1], [1, 0, 0]], // north_east
    }
}

fn hlen(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[2] * v[2]).sqrt()
}

impl Game {
    fn bstate(&self, p: [i32; 3]) -> BlockState {
        self.world.get_block(p[0], p[1], p[2]).unwrap_or(crate::registry::blocks::AIR)
    }

    /// `MinecartItem.useOn`: put a cart on a rail.
    pub fn place_minecart(&self, item: &str, on: [i32; 3]) -> bool {
        let s = self.bstate(on);
        if !is_rail(s) {
            return false;
        }
        let ascending = s.prop("shape").is_some_and(|x| x.starts_with("ascending"));
        let pos = [on[0] as f64 + 0.5, on[1] as f64 + 0.0625 + if ascending { 0.5 } else { 0.0 }, on[2] as f64 + 0.5];
        let ty: &'static str = Box::leak(item.to_string().into_boxed_str());
        let eid = self.next_eid();
        self.add_entity(Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Minecart(Box::new(Cart::new(ty))), pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos });
        true
    }

    pub(crate) fn cart_meta(&self, eid: i32, c: &Cart) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid).u8(8).vi(1).vi(c.hurt).u8(9).vi(1).vi(c.hurt_dir).u8(10).vi(3).f32(c.damage);
        match c.ty {
            // MinecartFurnace.DATA_ID_FUEL
            "furnace_minecart" => {
                b.u8(13).vi(8).bool(c.fuel > 0);
            }
            _ => {}
        }
        b.u8(0xFF);
        self.encode("set_entity_data", &b)
    }

    /// minecarts touching `area` (detector rails)
    pub fn minecarts_in(&self, area: &Aabb) -> usize {
        self.entities.lock().values().filter(|e| matches!(e.kind, EKind::Minecart(_)) && Aabb::entity(e.pos, 0.98, 0.7).intersects(area)).count()
    }

    /// `VehicleEntity.hurtServer` for carts.
    pub fn hit_minecart(&self, p: Option<&Arc<Player>>, eid: i32, amount: f32) -> bool {
        let creative = p.is_some_and(|p| p.gamemode.load(Ordering::Relaxed) == 1);
        let (broken, pkt, chunk, pos, ty) = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return false };
            let chunk = e.chunk();
            let pos = e.pos;
            let EKind::Minecart(c) = &mut e.kind else { return false };
            let _ = &c.items;
            c.hurt_dir = -c.hurt_dir;
            c.hurt = 10;
            c.damage += amount * 10.0;
            let broken = creative || c.damage > 40.0;
            (broken, self.cart_meta(eid, c), chunk, pos, c.ty)
        };
        self.broadcast_chunk(chunk, &pkt);
        if broken {
            self.eject_all(eid);
            self.close_viewers([super::block::container::ENTITY_KEY, eid, 0]);
            let gone = self.entities.lock().remove(&eid);
            self.broadcast_chunk(chunk, &self.remove_packet(&[eid]));
            // Containers.dropContents: the cart's items spill out (even in creative)
            if let Some(EKind::Minecart(c)) = gone.map(|e| e.kind) {
                self.spill(pos, &c.items);
            }
            if !creative {
                if let Some(id) = Items::get().id(ty) {
                    self.spawn_item(pos, Stack::new(id, 1), [0.0, 0.1, 0.0], 10);
                }
            }
        }
        true
    }

    /// Right click on a cart: ride it, or fuel a furnace minecart.
    pub fn use_minecart(&self, p: &Arc<Player>, eid: i32, held: Option<Stack>, slot: usize) -> bool {
        let ty = match self.entities.lock().get(&eid).map(|e| &e.kind) {
            Some(EKind::Minecart(c)) => c.ty,
            _ => return false,
        };
        match ty {
            "chest_minecart" | "hopper_minecart" => {
                let (screen, title) = if ty == "chest_minecart" { (super::blockentity::Screen::Rows(3), "Minecart with Chest") } else { (super::blockentity::Screen::Hopper, "Minecart with Hopper") };
                p.requests.lock().push(super::Request::OpenEntity(eid, screen, title));
                true
            }
            "minecart" => {
                if p.flags.load(Ordering::Relaxed) & super::hurt::F_SNEAKING != 0 {
                    return false;
                }
                self.mount(p.eid, eid)
            }
            "furnace_minecart" => {
                // MinecartFurnace.interact: fuel with coal/charcoal, push away from the player
                let name = held.map(|s| Items::get().name(s.item)).unwrap_or("");
                let ppos = p.pose.lock().pos;
                let mut ents = self.entities.lock();
                let Some(e) = ents.get_mut(&eid) else { return false };
                let epos = e.pos;
                let EKind::Minecart(c) = &mut e.kind else { return false };
                if matches!(name, "coal" | "charcoal") && c.fuel + 3600 <= 32000 {
                    c.fuel += 3600;
                    drop(ents);
                    if p.gamemode.load(Ordering::Relaxed) != 1 {
                        p.inv.lock().consume(slot, 1);
                        self.flush_inventory(p);
                    }
                    ents = self.entities.lock();
                }
                if let Some(EKind::Minecart(c)) = ents.get_mut(&eid).map(|e| &mut e.kind) {
                    if c.fuel > 0 {
                        c.push = [epos[0] - ppos[0], epos[2] - ppos[2]];
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// `AbstractMinecart.activateMinecart` on a powered activator rail.
    fn activate_cart(&self, eid: i32, e: &mut Entity, on: bool, booms: &mut Vec<CartEv>) {
        let EKind::Minecart(c) = &mut e.kind else { return };
        match c.ty {
            "minecart" if on => {
                // eject (done after the entity lock is released)
                booms.push(CartEv::Eject(eid));
            }
            "hopper_minecart" => c.enabled = !on,
            "tnt_minecart" if on && c.fuse < 0 => {
                c.fuse = 80;
                booms.push(CartEv::Primed(e.pos));
            }
            _ => {}
        }
    }

    /// One cart tick (`AbstractMinecart.tick` + `OldMinecartBehavior`).
    /// Returns true when the cart is gone (a TNT minecart went off).
    pub(crate) fn cart_tick(&self, e: &mut Entity, intent: Option<[f64; 2]>, ridden: bool, booms: &mut Vec<CartEv>) -> bool {
        let eid = e.eid;
        {
            let EKind::Minecart(c) = &mut e.kind else { return false };
            if c.hurt > 0 {
                c.hurt -= 1;
            }
            if c.damage > 0.0 {
                c.damage -= 1.0;
            }
            if c.fuse > 0 {
                c.fuse -= 1;
                if c.fuse == 0 {
                    booms.push(CartEv::Explode(e.pos, hlen(e.vel)));
                    return true;
                }
            }
        }
        e.vel[1] -= 0.04;
        let mut p = [e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32];
        if is_rail(self.bstate([p[0], p[1] - 1, p[2]])) {
            p[1] -= 1;
        }
        let s = self.bstate(p);
        if is_rail(s) {
            self.move_along_track(e, p, s, intent, ridden);
            if s.name() == "activator_rail" {
                let on = s.prop("powered") == Some("true");
                self.activate_cart(eid, e, on, booms);
            }
        } else {
            // comeOffTrack
            let max = 0.4;
            e.vel[0] = e.vel[0].clamp(-max, max);
            e.vel[2] = e.vel[2].clamp(-max, max);
            if e.on_ground {
                e.vel[0] *= 0.5;
                e.vel[1] *= 0.5;
                e.vel[2] *= 0.5;
            }
            let cell = |x: i32, y: i32, z: i32| self.cell(x, y, z);
            let (ground, _) = super::entity::move_entity(&mut e.pos, &mut e.vel, 0.49, 0.7, &cell);
            e.on_ground = ground;
            if !ground {
                e.vel[0] *= 0.95;
                e.vel[1] *= 0.95;
                e.vel[2] *= 0.95;
            }
        }
        // rotation from motion
        if hlen(e.vel) > 1e-4 {
            if let EKind::Minecart(c) = &mut e.kind {
                c.yaw = (e.vel[2].atan2(e.vel[0]).to_degrees() as f32) + 90.0;
            }
        }
        false
    }

    fn move_along_track(&self, e: &mut Entity, p: [i32; 3], s: BlockState, intent: Option<[f64; 2]>, ridden: bool) {
        let mut y = p[1] as f64;
        let mut powered = false;
        let mut halt = false;
        if s.name() == "powered_rail" {
            powered = s.prop("powered") == Some("true");
            halt = !powered;
        }
        let in_water = self.world.get_block(p[0], p[1], p[2]).is_some_and(|b| super::fluid::fluid_state(b).fluid() == Some(super::fluid::Fluid::Water));
        let mut slow = 0.0078125;
        if in_water {
            slow *= 0.2;
        }
        let shape = s.prop("shape").unwrap_or("north_south");
        match shape {
            "ascending_east" => {
                e.vel[0] -= slow;
                y += 1.0;
            }
            "ascending_west" => {
                e.vel[0] += slow;
                y += 1.0;
            }
            "ascending_north" => {
                e.vel[2] += slow;
                y += 1.0;
            }
            "ascending_south" => {
                e.vel[2] -= slow;
                y += 1.0;
            }
            _ => {}
        }
        let ex = exits(shape);
        let (mut dx, mut dz) = ((ex[1][0] - ex[0][0]) as f64, (ex[1][2] - ex[0][2]) as f64);
        let len = (dx * dx + dz * dz).sqrt();
        if e.vel[0] * dx + e.vel[2] * dz < 0.0 {
            dx = -dx;
            dz = -dz;
        }
        let hs = hlen(e.vel).min(2.0);
        e.vel = [hs * dx / len, e.vel[1], hs * dz / len];
        // a rider pushes a standing cart (getLastClientMoveIntent)
        if let Some(i) = intent {
            let d = i[0] * i[0] + i[1] * i[1];
            if d > 1e-4 && hlen(e.vel).powi(2) < 0.01 {
                e.vel[0] += i[0] * 0.1;
                e.vel[2] += i[1] * 0.1;
                halt = false;
            }
        }
        if halt {
            let h = hlen(e.vel);
            if h < 0.03 {
                e.vel = [0.0; 3];
            } else {
                e.vel[0] *= 0.5;
                e.vel[1] = 0.0;
                e.vel[2] *= 0.5;
            }
        }
        let (x0, z0) = (p[0] as f64 + 0.5 + ex[0][0] as f64 * 0.5, p[2] as f64 + 0.5 + ex[0][2] as f64 * 0.5);
        let (x1, z1) = (p[0] as f64 + 0.5 + ex[1][0] as f64 * 0.5, p[2] as f64 + 0.5 + ex[1][2] as f64 * 0.5);
        let (ddx, ddz) = (x1 - x0, z1 - z0);
        let t = if ddx == 0.0 {
            e.pos[2] - p[2] as f64
        } else if ddz == 0.0 {
            e.pos[0] - p[0] as f64
        } else {
            ((e.pos[0] - x0) * ddx + (e.pos[2] - z0) * ddz) * 2.0
        };
        let x = x0 + ddx * t;
        let z = z0 + ddz * t;
        e.pos = [x, y, z];
        // furnace push
        let furnace = if let EKind::Minecart(c) = &mut e.kind {
            if c.ty == "furnace_minecart" {
                if c.fuel > 0 {
                    c.fuel -= 1;
                } else {
                    c.push = [0.0; 2];
                }
                Some(c.push)
            } else {
                None
            }
        } else {
            None
        };
        let m = if ridden { 0.75 } else { 1.0 };
        let max = if in_water { 0.2 } else if furnace.is_some() { 0.2 } else { 0.4 };
        let mut mv = [(m * e.vel[0]).clamp(-max, max), 0.0, (m * e.vel[2]).clamp(-max, max)];
        let cell = |x: i32, y: i32, z: i32| self.cell(x, y, z);
        let mut pos = e.pos;
        super::entity::move_entity(&mut pos, &mut mv, 0.49, 0.7, &cell);
        e.pos[0] = pos[0];
        e.pos[2] = pos[2];
        let fx = e.pos[0].floor() as i32 - p[0];
        let fz = e.pos[2].floor() as i32 - p[2];
        if ex[0][1] != 0 && fx == ex[0][0] && fz == ex[0][2] {
            e.pos[1] = y + ex[0][1] as f64;
        } else if ex[1][1] != 0 && fx == ex[1][0] && fz == ex[1][2] {
            e.pos[1] = y + ex[1][1] as f64;
        }
        // applyNaturalSlowdown
        match furnace {
            Some(push) => {
                let d = (push[0] * push[0] + push[1] * push[1]).sqrt();
                if d > 1e-4 {
                    let (px, pz) = (push[0] / d, push[1] / d);
                    e.vel[0] = e.vel[0] * 0.8 + px * 0.05;
                    e.vel[2] = e.vel[2] * 0.8 + pz * 0.05;
                } else {
                    e.vel[0] *= 0.98;
                    e.vel[2] *= 0.98;
                }
                e.vel[1] = 0.0;
            }
            None => {
                let mut f = if ridden { 0.997 } else { 0.96 };
                if in_water {
                    f *= 0.95;
                }
                e.vel = [e.vel[0] * f, 0.0, e.vel[2] * f];
            }
        }
        // the cart sits on the rail: slope height from the position along it
        let np = [e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32];
        let mut rail = np;
        if is_rail(self.bstate([np[0], np[1] - 1, np[2]])) {
            rail[1] -= 1;
        }
        let rs = self.bstate(rail);
        if is_rail(rs) {
            let sh = rs.prop("shape").unwrap_or("north_south");
            let fxz = |v: f64| v - v.floor();
            let h = match sh {
                "ascending_east" => fxz(e.pos[0]),
                "ascending_west" => 1.0 - fxz(e.pos[0]),
                "ascending_north" => 1.0 - fxz(e.pos[2]),
                "ascending_south" => fxz(e.pos[2]),
                _ => 0.0,
            };
            let new_y = rail[1] as f64 + 0.0625 + h;
            let dy = (e.pos[1] - new_y) * 0.05;
            let hl = hlen(e.vel);
            if hl > 0.0 {
                e.vel[0] *= (hl + dy) / hl;
                e.vel[2] *= (hl + dy) / hl;
            }
            e.pos[1] = new_y;
        }
        if np[0] != p[0] || np[2] != p[2] {
            let hl = hlen(e.vel);
            e.vel[0] = hl * (np[0] - p[0]) as f64;
            e.vel[2] = hl * (np[2] - p[2]) as f64;
        }
        if powered {
            let hl = hlen(e.vel);
            if hl > 0.01 {
                e.vel[0] += e.vel[0] / hl * 0.06;
                e.vel[2] += e.vel[2] / hl * 0.06;
            } else {
                let solid = |q: [i32; 3]| self.world.get_block(q[0], q[1], q[2]).is_some_and(|b| b.is_solid());
                if shape == "east_west" {
                    if solid([p[0] - 1, p[1], p[2]]) {
                        e.vel[0] = 0.02;
                    } else if solid([p[0] + 1, p[1], p[2]]) {
                        e.vel[0] = -0.02;
                    }
                } else if shape == "north_south" {
                    if solid([p[0], p[1], p[2] - 1]) {
                        e.vel[2] = 0.02;
                    } else if solid([p[0], p[1], p[2] + 1]) {
                        e.vel[2] = -0.02;
                    }
                }
            }
        }
    }

    /// TNT minecart blast (`MinecartTNT.explode`): power 4 + speed.
    pub(crate) fn cart_explode(&self, pos: [f64; 3], speed: f64) {
        let mut rng = rand::thread_rng();
        let power = 4.0 + rng.r#gen::<f64>() * 1.5 * speed.min(5.0).sqrt();
        self.explode_full(pos, power as f32, false, None, Some(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::BlockRegistry;

    #[test]
    fn carts_follow_rails_and_powered_rails_push() {
        let Some((g, _gen, _store, tmp)) = crate::game::endfight::tests::end_game() else { return };
        let y = g.motion_blocking_height(0, 0);
        let rail = BlockRegistry::get().state("rail", &[("shape", "east_west")]).unwrap();
        let powered = BlockRegistry::get().state("powered_rail", &[("shape", "east_west"), ("powered", "true")]).unwrap();
        // the powered rail needs real power, or its update turns it off
        g.change_block([3, y - 1, 0], BlockRegistry::get().state("redstone_block", &[]).unwrap(), false, None);
        for x in 0..40 {
            g.change_block([x, y, 0], if x == 3 { powered } else { rail }, false, None);
        }
        assert!(g.place_minecart("minecart", [1, y, 0]));
        let eid = g.entities.lock().values().find(|e| matches!(e.kind, EKind::Minecart(_))).map(|e| e.eid).unwrap();
        g.entities.lock().get_mut(&eid).unwrap().vel = [0.2, 0.0, 0.0];
        let mut evs = Vec::new();
        let mut fastest: f64 = 0.0;
        for _ in 0..40 {
            let mut ents = g.entities.lock();
            let e = ents.get_mut(&eid).unwrap();
            let mut copy = e.clone();
            drop(ents);
            g.cart_tick(&mut copy, None, false, &mut evs);
            fastest = fastest.max(copy.vel[0]);
            *g.entities.lock().get_mut(&eid).unwrap() = copy;
        }
        let e = g.entities.lock().get(&eid).cloned().unwrap();
        assert!(e.pos[0] > 6.0, "cart moved along the rail: {:?}", e.pos);
        assert!((e.pos[1] - (y as f64 + 0.0625)).abs() < 1e-6, "on the rail: {:?}", e.pos);
        assert!((e.pos[2] - 0.5).abs() < 1e-6);
        assert!(fastest > 0.25, "powered rail boost {fastest}");
        // riding bookkeeping
        let pig = g.spawn_mob_with(crate::game::mob::MobType::from_name("pig").unwrap(), [5.5, y as f64 + 1.0, 5.5], true, |_, _| {});
        assert!(g.mount(pig, eid));
        assert_eq!(g.vehicle_of(pig), Some(eid));
        assert!(!g.mount(pig + 1000, eid) || g.passengers_of(eid).len() == 1);
        g.dismount(pig);
        assert!(g.passengers_of(eid).is_empty());
        // leads: a knot holds the pig, a far knot snaps
        g.change_block([5, y, 8], BlockRegistry::get().state("oak_fence", &[]).unwrap(), false, None);
        let knot = g.next_eid();
        let kpos = [5.5, y as f64 + 0.375, 8.5];
        g.add_entity(Entity { eid: knot, uuid: uuid::Uuid::new_v4(), kind: EKind::Knot, pos: kpos, vel: [0.0; 3], on_ground: false, age: 0, synced: kpos });
        g.attach_leash(pig, knot);
        g.entities.lock().get_mut(&pig).unwrap().pos = [5.5, y as f64 + 1.0, 16.0];
        g.tick_leashes();
        assert_eq!(g.leash_holder(pig), Some(knot));
        assert!(g.entities.lock().get(&pig).unwrap().vel[2] < 0.0, "pulled toward the knot");
        g.entities.lock().get_mut(&pig).unwrap().pos = [5.5, y as f64 + 1.0, 30.0];
        g.tick_leashes();
        assert_eq!(g.leash_holder(pig), None, "snapped");
        assert!(!g.entities.lock().contains_key(&knot), "unused knot removed");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn cart_inventories_feed_hoppers() {
        let Some((g, _gen, _store, tmp)) = crate::game::endfight::tests::end_game() else { return };
        let r = BlockRegistry::get();
        let items = crate::data::Items::get();
        let y = g.motion_blocking_height(0, 0);
        let rail = r.state("rail", &[("shape", "east_west")]).unwrap();
        // a hopper minecart under a chest pulls from it
        g.change_block([0, y, 0], rail, false, None);
        g.change_block([0, y + 1, 0], r.state("chest", &[]).unwrap(), false, None);
        let dirt = Stack::new(items.id("dirt").unwrap(), 3);
        g.set_container_slots([0, y + 1, 0], &{
            let mut v = vec![None; 27];
            v[0] = Some(dirt);
            v
        }, -1);
        assert!(g.place_minecart("hopper_minecart", [0, y, 0]));
        let hop = g.entities.lock().values().find(|e| matches!(&e.kind, EKind::Minecart(c) if c.ty == "hopper_minecart")).map(|e| (e.eid, e.pos)).unwrap();
        for _ in 0..3 {
            crate::game::block::container::hopper_cart_tick(&g, hop.0, hop.1);
        }
        let got = g.entity_slots(hop.0).unwrap();
        assert_eq!(got[0].map(|s| s.count), Some(3), "{got:?}");
        assert!(g.container_slots([0, y + 1, 0]).unwrap().iter().all(|s| s.is_none()));
        // a hopper block under a chest minecart pulls from the cart
        g.change_block([5, y, 0], rail, false, None);
        g.change_block([5, y - 1, 0], r.state("hopper", &[]).unwrap(), false, None);
        assert!(g.place_minecart("chest_minecart", [5, y, 0]));
        let chest = g.entities.lock().values().find(|e| matches!(&e.kind, EKind::Minecart(c) if c.ty == "chest_minecart")).map(|e| e.eid).unwrap();
        let mut v = vec![None; 27];
        v[4] = Some(Stack::new(items.id("stone").unwrap(), 2));
        g.set_entity_slots(chest, &v);
        for _ in 0..20 {
            crate::game::block::container::hopper_tick(&g, [5, y - 1, 0]);
        }
        let h = g.container_slots([5, y - 1, 0]).unwrap();
        assert_eq!(h.iter().flatten().map(|s| s.count as i32).sum::<i32>(), 2, "{h:?}");
        // breaking a chest minecart spills what it holds
        let mut v = vec![None; 27];
        v[0] = Some(Stack::new(items.id("stone").unwrap(), 5));
        g.set_entity_slots(chest, &v);
        let before = g.entities.lock().values().filter(|e| matches!(e.kind, EKind::Item { .. })).count();
        g.hit_minecart(None, chest, 100.0);
        let after = g.entities.lock().values().filter(|e| matches!(e.kind, EKind::Item { .. })).count();
        assert_eq!(after, before + 2, "contents and the cart itself drop");
        let _ = std::fs::remove_dir_all(tmp);
    }
}
