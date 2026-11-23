//! Fishing (`FishingRodItem` / `FishingHook`): casting the bobber, its
//! flight and bobbing, the lure → approach → bite cycle (rain, sky, Lure),
//! reeling in loot from `gameplay/fishing` (Luck of the Sea), pulling
//! hooked entities, rod damage, experience, statistics and the trigger.

use super::entity::{EKind, Entity};
use super::{Game, Player};
use crate::data::{Items, Stack};
use crate::net::codec::Out;
use crate::registry::blocks;
use rand::Rng;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct Bobber {
    pub owner: i32,
    /// 0 flying, 1 hooked in an entity, 2 bobbing
    pub state: u8,
    pub hooked: Option<i32>,
    pub life: i32,
    pub out_of_water: i32,
    pub nibble: i32,
    pub until_hooked: i32,
    pub until_lured: i32,
    pub fish_angle: f32,
    pub open_water: bool,
    pub biting: bool,
    /// Luck of the Sea level and Lure ticks (5 s per level)
    pub luck: i32,
    pub lure: i32,
    /// what clients were last told: (hooked, biting)
    pub sent: (Option<i32>, bool),
}

fn triangle(rng: &mut impl Rng, mode: f64, dev: f64) -> f64 {
    mode + dev * (rng.r#gen::<f64>() - rng.r#gen::<f64>())
}

/// Water surface height inside the block (`FluidState.getHeight`), 0 outside water.
fn water_height(g: &Game, p: [i32; 3]) -> f64 {
    let s = g.world.get_block(p[0], p[1], p[2]).unwrap_or(blocks::AIR);
    let fs = super::fluid::fluid_state(s);
    if fs.fluid() != Some(super::fluid::Fluid::Water) {
        return 0.0;
    }
    let above = g.world.get_block(p[0], p[1] + 1, p[2]).unwrap_or(blocks::AIR);
    if super::fluid::fluid_state(above).fluid() == Some(super::fluid::Fluid::Water) {
        return 1.0;
    }
    let level: i32 = s.prop("level").and_then(|l| l.parse().ok()).unwrap_or(0);
    if level == 0 || level >= 8 {
        8.0 / 9.0
    } else {
        (8 - level) as f64 / 9.0
    }
}

impl Game {
    /// The player's bobber, if one is out.
    pub fn bobber_of(&self, owner: i32) -> Option<i32> {
        self.entities.lock().values().find(|e| matches!(&e.kind, EKind::Bobber(b) if b.owner == owner)).map(|e| e.eid)
    }

    /// `FishingRodItem.use`: cast, or reel in the bobber that is out.
    pub fn use_fishing_rod(&self, p: &Arc<Player>, rod: Stack, hand: usize) {
        let mut rng = rand::thread_rng();
        let pos = p.pose.lock().pos;
        if let Some(b) = self.bobber_of(p.eid) {
            let dmg = self.retrieve(p, b, rod);
            if dmg > 0 && p.gamemode.load(Ordering::Relaxed) != 1 {
                self.with_player(p, |l, eq, _| {
                    let mut evs = Vec::new();
                    super::combat::living::hurt_and_break(eq, hand, dmg, false, &mut evs, &mut rand::thread_rng());
                    l.events.extend(evs);
                });
                self.flush_inventory(p);
            }
            self.play_sound("minecraft:entity.fishing_bobber.retrieve", 7, pos, 1.0, 0.4 / (rng.r#gen::<f32>() * 0.4 + 0.8));
            return;
        }
        self.play_sound("minecraft:entity.fishing_bobber.throw", 7, pos, 0.5, 0.4 / (rng.r#gen::<f32>() * 0.4 + 0.8));
        let pose = *p.pose.lock();
        let (yr, xr) = (pose.yaw as f64, pose.pitch as f64);
        let cos_y = (-yr.to_radians() - std::f64::consts::PI).cos();
        let sin_y = (-yr.to_radians() - std::f64::consts::PI).sin();
        let cos_x = -(-xr.to_radians()).cos();
        let sin_x = (-xr.to_radians()).sin();
        let start = [pose.pos[0] - sin_y * 0.3, pose.pos[1] + 1.62, pose.pos[2] - cos_y * 0.3];
        let mut v = [-sin_y, (-(sin_x / cos_x)).clamp(-5.0, 5.0), -cos_y];
        let d = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        for c in v.iter_mut() {
            *c *= 0.6 / d + triangle(&mut rng, 0.5, 0.0103365);
        }
        let b = Bobber {
            owner: p.eid,
            luck: rod.enchantment_level("luck_of_the_sea") as i32,
            lure: rod.enchantment_level("lure") as i32 * 100,
            ..Default::default()
        };
        let eid = self.next_eid();
        self.add_entity(Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Bobber(Box::new(b)), pos: start, vel: v, on_ground: false, age: 0, synced: start });
        p.award(super::stats::USED, "fishing_rod", 1);
    }

    /// `FishingHook.retrieve`: returns the rod damage.
    fn retrieve(&self, p: &Arc<Player>, eid: i32, rod: Stack) -> i32 {
        let Some(e) = self.entities.lock().get(&eid).cloned() else { return 0 };
        let EKind::Bobber(b) = &e.kind else { return 0 };
        let mut rng = rand::thread_rng();
        let mut dmg = 0;
        let owner = p.pose.lock().pos;
        if let Some(h) = b.hooked {
            // pullEntity
            let hp = self.entities.lock().get(&h).map(|x| x.pos);
            let is_item = self.entities.lock().get(&h).is_some_and(|x| matches!(x.kind, EKind::Item { .. }));
            if let Some(hp) = hp {
                let d = [owner[0] - hp[0], owner[1] - hp[1], owner[2] - hp[2]];
                self.push_entity(h, [d[0] * 0.1, d[1] * 0.1, d[2] * 0.1]);
            }
            self.trigger_fishing(p, rod, Some(h), &[]);
            self.broadcast_chunk(e.chunk(), &self.entity_event(eid, 31));
            dmg = if is_item { 3 } else { 5 };
        } else if b.nibble > 0 {
            let loot = self.data.roll_fishing(b.luck, b.open_water, self.world.biome(e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32).as_deref(), &mut rng);
            self.trigger_fishing(p, rod, None, &loot);
            for s in &loot {
                let d = [owner[0] - e.pos[0], owner[1] - e.pos[1], owner[2] - e.pos[2]];
                let vel = [d[0] * 0.1, d[1] * 0.1 + (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().sqrt() * 0.08, d[2] * 0.1];
                self.spawn_item(e.pos, *s, vel, 10);
                if self.data.item_in("fishes", s.item) {
                    p.award_custom("fish_caught", 1);
                }
            }
            self.award_xp_orbs([owner[0], owner[1] + 0.5, owner[2] + 0.5], rng.gen_range(1..=6));
            dmg = 1;
        }
        if e.on_ground {
            dmg = 2;
        }
        self.discard(eid);
        dmg
    }

    fn bobber_meta(&self, eid: i32, hooked: Option<i32>, biting: bool) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid).u8(8).vi(1).vi(hooked.map_or(0, |h| h + 1)).u8(9).vi(8).bool(biting).u8(0xFF);
        self.encode("set_entity_data", &b)
    }

    /// `FishingHook.tick` for every bobber.
    pub(crate) fn tick_bobbers(&self) {
        let ids: Vec<i32> = self.entities.lock().values().filter(|e| matches!(e.kind, EKind::Bobber(_))).map(|e| e.eid).collect();
        if ids.is_empty() {
            return;
        }
        let players = self.players();
        let rod = Items::get().id("fishing_rod");
        let mut rng = rand::thread_rng();
        for eid in ids {
            let Some(mut e) = self.entities.lock().get(&eid).cloned() else { continue };
            let EKind::Bobber(mut b) = e.kind.clone() else { continue };
            // shouldStopFishing: owner gone, not holding a rod, or too far
            let owner = players.iter().find(|p| p.eid == b.owner);
            let keep = owner.is_some_and(|p| {
                let inv = p.inv.lock();
                let holds = inv.held(p.held.load(Ordering::Relaxed) as i16).is_some_and(|s| Some(s.item) == rod) || inv.slots[45].is_some_and(|s| Some(s.item) == rod);
                let q = p.pose.lock().pos;
                holds && p.vit.lock().living.health > 0.0 && (q[0] - e.pos[0]).powi(2) + (q[1] - e.pos[1]).powi(2) + (q[2] - e.pos[2]).powi(2) <= 1024.0
            });
            if !keep {
                self.discard(eid);
                continue;
            }
            e.age += 1;
            b.life = if e.on_ground { b.life + 1 } else { 0 };
            if b.life >= 1200 {
                self.discard(eid);
                continue;
            }
            let bp = [e.pos[0].floor() as i32, e.pos[1].floor() as i32, e.pos[2].floor() as i32];
            let f = water_height(self, bp);
            let in_water = f > 0.0 && e.pos[1] < bp[1] as f64 + f + 0.01;
            match b.state {
                0 => {
                    if b.hooked.is_some() {
                        b.state = 1;
                    } else if in_water {
                        e.vel = [e.vel[0] * 0.3, e.vel[1] * 0.2, e.vel[2] * 0.3];
                        b.state = 2;
                    } else {
                        // checkCollision: the first entity along this tick's path
                        let to = [e.pos[0] + e.vel[0], e.pos[1] + e.vel[1], e.pos[2] + e.vel[2]];
                        let hit = self.entity_snaps().into_iter().filter(|s| s.eid != eid && s.eid != b.owner && s.ty != "fishing_bobber" && s.ty != "item").find(|s| {
                            let c = [s.pos[0], s.pos[1] + s.eye.max(0.3) * 0.5, s.pos[2]];
                            seg_dist(e.pos, to, c) < 0.6
                        });
                        let hit = hit.map(|s| s.eid).or_else(|| players.iter().filter(|p| p.eid != b.owner).find(|p| seg_dist(e.pos, to, { let q = p.pose.lock().pos; [q[0], q[1] + 0.9, q[2]] }) < 0.6).map(|p| p.eid));
                        if let Some(h) = hit {
                            b.hooked = Some(h);
                            b.state = 1;
                        }
                    }
                }
                1 => match b.hooked.and_then(|h| self.entities.lock().get(&h).map(|x| x.pos).or_else(|| players.iter().find(|p| p.eid == h).map(|p| p.pose.lock().pos))) {
                    Some(hp) => {
                        e.pos = [hp[0], hp[1] + 0.8, hp[2]];
                        e.vel = [0.0; 3];
                    }
                    None => {
                        b.hooked = None;
                        b.state = 0;
                    }
                },
                _ => {
                    let d0 = e.pos[1] + e.vel[1] - bp[1] as f64 - f;
                    let d = if d0.abs() < 0.01 { d0 + d0.signum() * 0.1 } else { d0 };
                    e.vel = [e.vel[0] * 0.9, e.vel[1] - d * rng.r#gen::<f64>() * 0.2, e.vel[2] * 0.9];
                    b.open_water = if b.nibble <= 0 && b.until_hooked <= 0 { true } else { b.open_water && b.out_of_water < 10 && self.open_water(bp) };
                    if in_water {
                        b.out_of_water = (b.out_of_water - 1).max(0);
                        if b.biting {
                            e.vel[1] -= 0.1 * rng.r#gen::<f64>() * rng.r#gen::<f64>();
                        }
                        self.catching_fish(&mut e, &mut b, bp, &mut rng);
                    } else {
                        b.out_of_water = (b.out_of_water + 1).min(10);
                    }
                }
            }
            if b.state != 1 {
                if !in_water {
                    e.vel[1] -= 0.03;
                }
                let to = [e.pos[0] + e.vel[0], e.pos[1] + e.vel[1], e.pos[2] + e.vel[2]];
                let solid = |p: [f64; 3]| self.world.get_block(p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32).is_some_and(|s| s.is_solid());
                if solid(to) {
                    // land on / against the block
                    if solid([e.pos[0], to[1], e.pos[2]]) && e.vel[1] < 0.0 {
                        e.on_ground = true;
                        e.pos[1] = to[1].floor() + 1.0;
                    }
                    e.vel = [0.0; 3];
                } else {
                    e.pos = to;
                    e.on_ground = false;
                }
                for c in e.vel.iter_mut() {
                    *c *= 0.92;
                }
            }
            let meta = (b.hooked, b.biting) != b.sent;
            b.sent = (b.hooked, b.biting);
            let (hooked, biting) = (b.hooked, b.biting);
            e.kind = EKind::Bobber(b);
            let chunk = e.chunk();
            let pos = e.pos;
            if let Some(slot) = self.entities.lock().get_mut(&eid) {
                *slot = e;
            }
            let mut pk = Vec::new();
            pk.vi(eid).f64(pos[0]).f64(pos[1]).f64(pos[2]).f64(0.0).f64(0.0).f64(0.0).f32(0.0).f32(0.0).bool(false);
            self.broadcast_chunk(chunk, &self.encode("entity_position_sync", &pk));
            if meta {
                self.broadcast_chunk(chunk, &self.bobber_meta(eid, hooked, biting));
            }
        }
    }

    /// `FishingHook.calculateOpenWater`: a 5×4×5 box of water / air only.
    fn open_water(&self, p: [i32; 3]) -> bool {
        for dy in -1..=2 {
            let mut kind = 0; // 1 above water, 2 inside water
            for dx in -2..=2 {
                for dz in -2..=2 {
                    let s = self.world.get_block(p[0] + dx, p[1] + dy, p[2] + dz).unwrap_or(blocks::AIR);
                    let k = if s.is_air() || s.name() == "lily_pad" {
                        1
                    } else if s.name() == "water" && super::fluid::fluid_state(s).is_source() {
                        2
                    } else {
                        return false;
                    };
                    if kind != 0 && k != kind {
                        return false;
                    }
                    kind = k;
                }
            }
        }
        true
    }

    /// `FishingHook.catchingFish`
    fn catching_fish(&self, e: &mut Entity, b: &mut Bobber, bp: [i32; 3], rng: &mut impl Rng) {
        let mut speed = 1;
        let above = [bp[0], bp[1] + 1, bp[2]];
        if rng.r#gen::<f32>() < 0.25 && self.raining_at(above) {
            speed += 1;
        }
        if rng.r#gen::<f32>() < 0.5 && self.world.surface_y(above[0], above[2]).is_some_and(|top| above[1] <= top) {
            speed -= 1;
        }
        if b.nibble > 0 {
            b.nibble -= 1;
            if b.nibble <= 0 {
                b.until_lured = 0;
                b.until_hooked = 0;
                b.biting = false;
            }
        } else if b.until_hooked > 0 {
            b.until_hooked -= speed;
            if b.until_hooked > 0 {
                b.fish_angle += triangle(rng, 0.0, 9.188) as f32;
                let a = (b.fish_angle as f64).to_radians();
                let t = b.until_hooked as f64 * 0.1;
                let fx = e.pos[0] + a.sin() * t;
                let fz = e.pos[2] + a.cos() * t;
                let fy = bp[1] as f64 + 1.0;
                let below = self.world.get_block(fx.floor() as i32, (fy - 1.0).floor() as i32, fz.floor() as i32).unwrap_or(blocks::AIR);
                if below.name() == "water" && rng.r#gen::<f32>() < 0.15 {
                    self.particles("minecraft:bubble", [fx, fy - 0.1, fz], 1, [a.sin() as f32, 0.1, a.cos() as f32], 0.0);
                }
                if below.name() == "water" {
                    self.particles("minecraft:fishing", [fx, fy, fz], 0, [(a.cos() * 0.04) as f32, 0.01, (-a.sin() * 0.04) as f32], 1.0);
                }
            } else {
                self.play_sound("minecraft:entity.fishing_bobber.splash", 7, e.pos, 0.25, 1.0 + (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.4);
                let y = e.pos[1] + 0.5;
                let w = 0.2f32;
                self.particles("minecraft:bubble", [e.pos[0], y, e.pos[2]], 7, [w, 0.0, w], 0.2);
                self.particles("minecraft:fishing", [e.pos[0], y, e.pos[2]], 7, [w, 0.0, w], 0.2);
                b.nibble = rng.gen_range(20..40);
                b.biting = true;
            }
        } else if b.until_lured > 0 {
            b.until_lured -= speed;
            let mut f = 0.15f32;
            if b.until_lured < 20 {
                f += (20 - b.until_lured) as f32 * 0.05;
            } else if b.until_lured < 40 {
                f += (40 - b.until_lured) as f32 * 0.02;
            } else if b.until_lured < 60 {
                f += (60 - b.until_lured) as f32 * 0.01;
            }
            if rng.r#gen::<f32>() < f {
                let a = rng.gen_range(0.0..360.0f64).to_radians();
                let dist = rng.gen_range(25.0..60.0) / 100.0;
                let p = [e.pos[0] + a.sin() * dist * 0.1, bp[1] as f64 + 1.0, e.pos[2] + a.cos() * dist * 0.1];
                self.particles("minecraft:splash", p, 2 + rng.gen_range(0..2), [0.1, 0.0, 0.1], 0.0);
            }
            if b.until_lured <= 0 {
                b.fish_angle = rng.gen_range(0.0..360.0);
                b.until_hooked = rng.gen_range(20..80);
            }
        } else {
            b.until_lured = rng.gen_range(100..600) - b.lure;
        }
    }
}

fn seg_dist(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let l = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
    let t = if l > 0.0 { ((ac[0] * ab[0] + ac[1] * ab[1] + ac[2] * ab[2]) / l).clamp(0.0, 1.0) } else { 0.0 };
    let p = [a[0] + ab[0] * t - c[0], a[1] + ab[1] * t - c[1], a[2] + ab[2] * t - c[2]];
    (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt()
}
