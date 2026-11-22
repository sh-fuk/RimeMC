//! World queries and actions used by block behaviour: primed TNT, entities
//! in a box (pressure plates, hoppers), light and sun for daylight
//! detectors, piston entity pushing, and the dispenser item behaviours.

use std::sync::atomic::Ordering;

use rand::Rng;

use super::block::{self, container::default_dispense, container::dispense_pos};
use super::entity::{EKind, Entity};
use super::level::*;
use super::projectile::{shoot_dir, PType, Projectile};
use super::Game;
use crate::anticheat::combat::Aabb;
use crate::data::{Items, Stack};
use crate::registry::{BlockRegistry, BlockState};
use crate::world::chunk::{HEIGHT, MIN_Y};

/// `(minY, maxY)` of the build height.
pub fn chunk_min_max_y() -> (i32, i32) {
    (MIN_Y, MIN_Y + HEIGHT)
}

impl Game {
    /// `Level.getGameTime`
    pub fn game_time(&self) -> u64 {
        self.now.load(Ordering::Relaxed)
    }

    /// `ServerLevel.isHandlingTick`: running scheduled block ticks
    pub fn handling_tick(&self) -> bool {
        self.lv(|l| l.handling)
    }

    pub fn sky_light(&self, p: [i32; 3]) -> u8 {
        self.world.light_at(p[0], p[1], p[2]).map(|l| l.0).unwrap_or(15)
    }

    /// `Level.getSunAngle` (radians)
    pub fn sun_angle(&self) -> f32 {
        let t = self.day_time() as f64 / 24000.0;
        let frac = (t - 0.25).rem_euclid(1.0);
        let tod = (frac * 2.0 + (0.5 - (frac * std::f64::consts::PI).cos() / 2.0)) / 3.0;
        (tod * std::f64::consts::PI * 2.0) as f32
    }

    /// comparator reading an item frame on the block in front (none exist yet)
    pub fn item_frame_analog(&self, _p: [i32; 3], _d: u8) -> Option<u8> {
        None
    }

    /// an arrow stuck in the block at `p` (wooden buttons)
    pub fn arrow_in_block(&self, p: [i32; 3], _s: BlockState) -> bool {
        self.entities.lock().values().any(|e| match &e.kind {
            EKind::Projectile(pr) => pr.ty.is_arrow() && pr.in_ground && pr.stuck.is_some_and(|(q, _)| q == p),
            _ => false,
        })
    }

    /// entities touching `area`; `living_only` for stone plates
    pub fn count_entities_in(&self, area: &Aabb, living_only: bool) -> usize {
        let mut n = 0;
        for p in self.players() {
            if p.gamemode.load(Ordering::Relaxed) == 3 {
                continue;
            }
            if Aabb::entity(p.pose.lock().pos, 0.6, 1.8).intersects(area) {
                n += 1;
            }
        }
        for e in self.entities.lock().values() {
            let counts = match &e.kind {
                EKind::Mob(m) => m.death == 0,
                EKind::Lightning { .. } | EKind::Cloud(_) | EKind::Fangs { .. } => false,
                _ => !living_only,
            };
            if counts {
                let (half, h) = e.size();
                if Aabb::entity(e.pos, half * 2.0, h).intersects(area) {
                    n += 1;
                }
            }
        }
        n
    }


    /// item entities touching `area`, as (eid, stack)
    pub fn items_in(&self, area: &Aabb) -> Vec<(i32, Stack)> {
        let mut v: Vec<(i32, Stack)> = self
            .entities
            .lock()
            .values()
            .filter_map(|e| match e.kind {
                EKind::Item { stack, .. } if Aabb::entity(e.pos, 0.25, 0.25).intersects(area) => Some((e.eid, stack)),
                _ => None,
            })
            .collect();
        v.sort_by_key(|x| x.0);
        v
    }

    /// change or remove an item entity's stack
    pub fn set_item_entity(&self, eid: i32, stack: Option<Stack>) {
        let chunk = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return };
            let c = e.chunk();
            match stack {
                Some(s) => {
                    if let EKind::Item { stack, .. } = &mut e.kind {
                        *stack = s;
                    }
                }
                None => {
                    ents.remove(&eid);
                }
            }
            c
        };
        match stack {
            Some(s) => self.broadcast_chunk(chunk, &self.item_meta(eid, s)),
            None => self.broadcast_chunk(chunk, &self.remove_packet(&[eid])),
        }
    }

    /// `TntBlock.prime` / `PrimedTnt`
    pub fn prime_tnt(&self, p: [i32; 3], owner: Option<i32>) {
        self.spawn_tnt(p, owner, 80);
        let c = [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5];
        self.play_sound("minecraft:entity.tnt.primed", 1, c, 1.0, 1.0);
    }

    pub fn spawn_tnt(&self, p: [i32; 3], owner: Option<i32>, fuse: i32) {
        let mut rng = rand::thread_rng();
        let a = rng.r#gen::<f64>() * std::f64::consts::PI * 2.0;
        let pos = [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5];
        let vel = [-a.sin() * 0.02, 0.2, -a.cos() * 0.02];
        self.add_entity(Entity { eid: self.next_eid(), uuid: uuid::Uuid::new_v4(), kind: EKind::Tnt { fuse, owner }, pos, vel, on_ground: false, age: 0, synced: pos });
    }

    /// `PistonMovingBlockEntity.moveCollidedEntities` (block boxes only)
    pub fn push_entities_by_piston(&self, p: [i32; 3], moved: BlockState, dir: u8, extending: bool, amount: f32) {
        if moved.is_air() || !moved.is_solid() && moved.name() != "slime_block" && moved.name() != "honey_block" {
            return;
        }
        let mv = if extending { dir } else { opp(dir) };
        let o = OFFSETS[mv as usize];
        let a = amount as f64;
        // the block's box at its current progress, swept by this step
        let base = Aabb { min: [p[0] as f64, p[1] as f64, p[2] as f64], max: [p[0] as f64 + 1.0, p[1] as f64 + 1.0, p[2] as f64 + 1.0] };
        let back = 1.0 - a;
        let mut bx = Aabb { min: base.min, max: base.max };
        for i in 0..3 {
            let s = o[i] as f64;
            bx.min[i] -= s * back;
            bx.max[i] -= s * back;
            if s > 0.0 {
                bx.max[i] += s * a;
            } else if s < 0.0 {
                bx.min[i] += s * a;
            }
        }
        let d = [o[0] as f64 * a, o[1] as f64 * a, o[2] as f64 * a];
        let slime = moved.name() == "slime_block" && extending;
        for pl in self.players() {
            if pl.gamemode.load(Ordering::Relaxed) == 3 {
                continue;
            }
            let pos = pl.pose.lock().pos;
            if Aabb::entity(pos, 0.6, 1.8).intersects(&bx) {
                if slime {
                    let mut v = [0.0; 3];
                    v[axis(mv) as usize] = o[axis(mv) as usize] as f64;
                    self.push_entity(pl.eid, v);
                } else {
                    self.push_entity(pl.eid, [d[0] + 0.01 * o[0] as f64, d[1] + 0.01 * o[1] as f64, d[2] + 0.01 * o[2] as f64]);
                }
            }
        }
        let mut moved_ids = Vec::new();
        {
            let mut ents = self.entities.lock();
            for e in ents.values_mut() {
                if matches!(e.kind, EKind::Lightning { .. } | EKind::Cloud(_) | EKind::Fangs { .. }) {
                    continue;
                }
                let (half, h) = e.size();
                if Aabb::entity(e.pos, half * 2.0, h).intersects(&bx) {
                    if slime {
                        e.vel[axis(mv) as usize] = o[axis(mv) as usize] as f64;
                    } else {
                        for i in 0..3 {
                            e.pos[i] += d[i] + 0.01 * o[i] as f64;
                        }
                    }
                    moved_ids.push(e.eid);
                }
            }
        }
        let _ = moved_ids;
    }

    /// `DispenserBlock.dispenseFrom` behaviours (`DispenseItemBehavior`):
    /// returns what stays in the slot and an extra stack to store.
    pub fn dispense_item(&self, p: [i32; 3], d: u8, st: Stack, rng: &mut impl Rng) -> (Option<Stack>, Option<Stack>) {
        let items = Items::get();
        let name = items.name(st.item);
        let rest = (st.count > 1).then_some(Stack { count: st.count - 1, ..st });
        let front = rel(p, d);
        let fs = self.bs(front);
        let o = OFFSETS[d as usize];
        let dirv = [o[0] as f64, o[1] as f64 + 0.1, o[2] as f64];
        let item = |n: &str| items.id(n).map(|i| Stack::new(i, 1));
        let shoot = |ty: PType, pow: f32, unc: f32, rng: &mut dyn rand::RngCore| {
            let mut pr = Projectile::new(ty, None, Stack { count: 1, ..st });
            if ty.is_arrow() {
                pr.pickup = super::projectile::Pickup::Allowed;
            }
            let mut r = rand::thread_rng();
            let _ = rng;
            let vel = shoot_dir(dirv, pow, unc, &mut r);
            self.launch(pr, dispense_pos(p, d), vel);
        };
        match name {
            "arrow" | "tipped_arrow" => {
                shoot(PType::Arrow, 1.1, 6.0, rng);
                self.level_event(1002, p, 0);
                return (rest, None);
            }
            "spectral_arrow" => {
                shoot(PType::SpectralArrow, 1.1, 6.0, rng);
                self.level_event(1002, p, 0);
                return (rest, None);
            }
            "snowball" | "egg" | "blue_egg" | "brown_egg" => {
                shoot(PType::for_thrown(name).unwrap_or(PType::Snowball), 1.1, 6.0, rng);
                self.level_event(1002, p, 0);
                return (rest, None);
            }
            "splash_potion" | "lingering_potion" | "experience_bottle" => {
                shoot(PType::for_thrown(name).unwrap_or(PType::SplashPotion), 1.375, 3.0, rng);
                self.level_event(1002, p, 0);
                return (rest, None);
            }
            "fire_charge" => {
                let g = |rng: &mut dyn rand::RngCore| {
                    let u: f64 = rng.r#gen::<f64>().max(1e-12);
                    let v: f64 = rng.r#gen();
                    (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos() * 0.05
                };
                let mut r = rand::thread_rng();
                let dv = [o[0] as f64 + g(&mut r), o[1] as f64 + g(&mut r), o[2] as f64 + g(&mut r)];
                let mut pr = Projectile::new(PType::SmallFireball, None, Stack { count: 1, ..st });
                pr.accel = 0.1;
                let len = (dv[0] * dv[0] + dv[1] * dv[1] + dv[2] * dv[2]).sqrt().max(1e-9);
                self.launch(pr, dispense_pos(p, d), [dv[0] / len * 0.1, dv[1] / len * 0.1, dv[2] / len * 0.1]);
                self.level_event(1018, p, 0);
                return (rest, None);
            }
            "tnt" => {
                self.spawn_tnt(front, None, 80);
                self.play_sound("minecraft:entity.tnt.primed", 1, [front[0] as f64 + 0.5, front[1] as f64, front[2] as f64 + 0.5], 1.0, 1.0);
                return (rest, None);
            }
            "water_bucket" | "lava_bucket" | "powder_snow_bucket" => {
                let placed = match name {
                    "powder_snow_bucket" => fs.is_air() && BlockRegistry::get().state("powder_snow", &[]).is_some_and(|b| self.set_block(front, b, flags::ALL)),
                    _ => {
                        let fluid = if name == "water_bucket" { "water" } else { "lava" };
                        if fluid == "water" && self.dim == crate::world::storage::Dim::Nether && self.is_replaceable(fs) {
                            // evaporates (BucketItem.emptyContents)
                            self.play_sound("minecraft:block.fire.extinguish", 4, [front[0] as f64 + 0.5, front[1] as f64 + 0.5, front[2] as f64 + 0.5], 0.5, 2.6);
                            true
                        } else if flag_wl(fs) && fluid == "water" && !block::flag(fs, "waterlogged") {
                            self.set_block(front, block::with_bool(fs, "waterlogged", true), flags::ALL);
                            self.schedule(front, super::TickKind::Fluid, 5);
                            true
                        } else if self.is_replaceable(fs) {
                            let b = BlockRegistry::get().state(fluid, &[]).unwrap_or(fs);
                            if !fs.is_air() && super::fluid::fluid_state(fs).fluid().is_none() {
                                self.drop_loot(front, fs, None);
                            }
                            self.set_block(front, b, flags::ALL);
                            true
                        } else {
                            false
                        }
                    }
                };
                if placed {
                    let snd = match name {
                        "lava_bucket" => "minecraft:item.bucket.empty_lava",
                        "powder_snow_bucket" => "minecraft:item.bucket.empty_powder_snow",
                        _ => "minecraft:item.bucket.empty",
                    };
                    block::sound(self, front, snd, 1.0, 1.0);
                    return (item("bucket"), None);
                }
            }
            "bucket" => {
                let filled = if block::flag(fs, "waterlogged") {
                    self.set_block(front, block::with_bool(fs, "waterlogged", false), flags::ALL);
                    Some("water_bucket")
                } else if fs.name() == "powder_snow" {
                    self.set_block(front, crate::registry::blocks::AIR, flags::ALL);
                    Some("powder_snow_bucket")
                } else if matches!(fs.name(), "water" | "lava") && fs.prop("level") == Some("0") {
                    self.set_block(front, crate::registry::blocks::AIR, flags::ALL);
                    Some(if fs.name() == "water" { "water_bucket" } else { "lava_bucket" })
                } else {
                    None
                };
                if let Some(f) = filled {
                    if rest.is_none() {
                        return (item(f), None);
                    }
                    return (rest, item(f));
                }
            }
            "flint_and_steel" => {
                if fs.is_air() {
                    if let Some(fire) = BlockRegistry::get().state("fire", &[]) {
                        if self.can_survive(front, fire) {
                            self.set_block(front, fire, flags::ALL);
                            block::sound(self, front, "minecraft:item.flintandsteel.use", 1.0, rng.r#gen::<f32>() * 0.4 + 0.8);
                            return (Some(st), None);
                        }
                    }
                } else if fs.name() == "tnt" {
                    self.prime_tnt(front, None);
                    self.remove_block(front, false);
                    return (Some(st), None);
                } else if (fs.name().ends_with("campfire") || fs.name().ends_with("candle") || fs.name().ends_with("candle_cake")) && fs.prop("lit") == Some("false") && fs.prop("waterlogged") != Some("true") {
                    self.set_block(front, block::with_bool(fs, "lit", true), flags::ALL | 8);
                    return (Some(st), None);
                }
                self.level_event(1001, p, 0);
                return (Some(st), None);
            }
            n if n.ends_with("_spawn_egg") => {
                let ty = n.trim_end_matches("_spawn_egg");
                let pos = [front[0] as f64 + 0.5, front[1] as f64, front[2] as f64 + 0.5];
                if self.summon(ty, pos).is_some() {
                    self.level_event(1000, p, 0);
                    return (rest, None);
                }
            }
            n if n.ends_with("shulker_box") => {
                if self.is_replaceable(fs) {
                    if let Some(b) = BlockRegistry::get().by_name(n) {
                        let b = block::with_facing(BlockState(b.default), d);
                        self.set_block(front, b, flags::ALL);
                        self.level_event(1000, p, 0);
                        return (rest, None);
                    }
                }
                self.level_event(1001, p, 0);
                return (Some(st), None);
            }
            "carved_pumpkin" | "wither_skeleton_skull" => {
                if self.is_replaceable(fs) {
                    if let Some(b) = BlockRegistry::get().by_name(name) {
                        let mut b = BlockState(b.default);
                        b = if name == "carved_pumpkin" { block::with_facing(b, if axis(d) == 1 { NORTH } else { opp(d) }) } else { b };
                        self.set_block(front, b, flags::ALL);
                        self.level_event(1000, p, 0);
                        return (rest, None);
                    }
                }
            }
            "glass_bottle" => {
                if super::fluid::fluid_state(fs).fluid() == Some(super::fluid::Fluid::Water) && fs.prop("level") == Some("0") {
                    let water = items.id("potion").map(|i| Stack::new(i, 1));
                    if let Some(w) = water {
                        let w = w.with_patch(|pt| pt.potion = Some("water".into()));
                        return if rest.is_none() { (Some(w), None) } else { (rest, Some(w)) };
                    }
                }
            }
            _ => {}
        }
        (default_dispense(self, p, d, st, rng), None)
    }
}

fn flag_wl(s: BlockState) -> bool {
    s.prop("waterlogged").is_some()
}

impl Game {
    /// `Entity.checkInsideBlocks` for the blocks that react to entities in
    /// them: pressure plates, tripwire, detector rails.
    pub fn tick_entity_inside(&self) {
        let mut boxes: Vec<Aabb> = Vec::new();
        for p in self.players() {
            if p.gamemode.load(Ordering::Relaxed) == 3 || p.vit.lock().dead() {
                continue;
            }
            boxes.push(Aabb::entity(p.pose.lock().pos, 0.6, 1.8));
        }
        for e in self.entities.lock().values() {
            if matches!(e.kind, EKind::Lightning { .. } | EKind::Cloud(_) | EKind::Fangs { .. }) {
                continue;
            }
            let (half, h) = e.size();
            boxes.push(Aabb::entity(e.pos, half * 2.0, h));
        }
        let mut seen = std::collections::HashSet::new();
        for b in boxes {
            let e = 1.0e-5;
            for x in (b.min[0] + e).floor() as i32..=(b.max[0] - e).floor() as i32 {
                for y in (b.min[1] + e).floor() as i32..=(b.max[1] - e).floor() as i32 {
                    for z in (b.min[2] + e).floor() as i32..=(b.max[2] - e).floor() as i32 {
                        let q = [x, y, z];
                        let s = self.bs(q);
                        let c = block::class(s);
                        if !matches!(c, block::Class::Plate | block::Class::WeightedPlate | block::Class::TripWire | block::Class::DetectorRail | block::Class::Button) || !seen.insert(q) {
                            continue;
                        }
                        match c {
                            block::Class::TripWire => block::tripwire::wire_entity_inside(self, q, s),
                            block::Class::DetectorRail => block::rail::detector_entity_inside(self, q, s),
                            block::Class::Button => {
                                if self.arrow_in_block(q, s) {
                                    block::redstone::button_arrow(self, q, s);
                                }
                            }
                            _ => block::redstone::plate_entity_inside(self, q, s),
                        }
                    }
                }
            }
        }
    }
}

impl Game {
    /// `DoublePlantBlock.preventDropFromBottomPart` / `BedBlock.playerWillDestroy`
    pub fn prevent_drop_from_other_part(&self, p: [i32; 3], s: BlockState) {
        let other = match block::class(s) {
            block::Class::Bed if block::prop(s, "part") == "foot" => Some(rel(p, block::facing(s))),
            _ if block::prop(s, "half") == "upper" => Some(rel(p, DOWN)),
            _ => None,
        };
        let Some(q) = other else { return };
        let os = self.bs(q);
        if !same_block(os, s) || os == s {
            return;
        }
        let air = if block::flag(os, "waterlogged") { crate::registry::blocks::WATER } else { crate::registry::blocks::AIR };
        let _lv = self.level.lock();
        self.set_block(q, air, flags::ALL | flags::SUPPRESS_DROPS);
        self.level_event(2001, q, crate::game::mobtick::state_id(os));
    }
}
