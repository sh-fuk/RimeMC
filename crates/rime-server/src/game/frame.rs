//! `ItemFrame` / `GlowItemFrame`: placing, filling, rotating, popping off.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::Rng;

use super::entity::{EKind, Entity};
use super::level::OFFSETS;
use super::{write_slot, Game, Player};
use crate::anticheat::combat::Aabb;
use crate::data::{Items, Stack};
use crate::net::codec::Out;

/// Direction 3D data values: down, up, north, south, west, east.
pub fn face_of(name: &str) -> u8 {
    match name {
        "down" => 0,
        "up" => 1,
        "north" => 2,
        "south" => 3,
        "west" => 4,
        _ => 5,
    }
}

/// The frame's centre for the block cell `p` it hangs in, facing `facing`.
pub fn centre(p: [i32; 3], facing: u8) -> [f64; 3] {
    let o = OFFSETS[facing as usize];
    [p[0] as f64 + 0.5 - o[0] as f64 * 0.46875, p[1] as f64 + 0.5 - o[1] as f64 * 0.46875, p[2] as f64 + 0.5 - o[2] as f64 * 0.46875]
}

/// The frame's bounding box (`ItemFrame.calculateBoundingBox`).
pub fn frame_box(c: [f64; 3], facing: u8) -> Aabb {
    let o = OFFSETS[facing as usize];
    let half = |axis: usize| if o[axis] != 0 { 0.03125 } else { 0.375 };
    Aabb { min: [c[0] - half(0), c[1] - half(1), c[2] - half(2)], max: [c[0] + half(0), c[1] + half(1), c[2] + half(2)] }
}

fn cell(c: [f64; 3]) -> [i32; 3] {
    [c[0].floor() as i32, c[1].floor() as i32, c[2].floor() as i32]
}

impl Game {
    pub(crate) fn frame_meta(&self, eid: i32, item: Option<Stack>, rotation: u8) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.vi(eid).u8(8).vi(7);
        write_slot(&mut b, item);
        b.u8(9).vi(1).vi(rotation as i32).u8(0xFF);
        self.encode("set_entity_data", &b)
    }

    /// Spawn a frame in the cell `p`, facing `facing` (away from the wall).
    pub fn spawn_frame(&self, p: [i32; 3], facing: u8, glow: bool, item: Option<Stack>, fixed: bool) -> i32 {
        let eid = self.next_eid();
        let pos = centre(p, facing);
        self.add_entity(Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Frame { item, rotation: 0, facing, glow, fixed }, pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos });
        eid
    }

    /// `HangingEntity.survives`: the wall behind must be solid.
    fn frame_survives(&self, pos: [f64; 3], facing: u8) -> bool {
        let p = cell(pos);
        let o = OFFSETS[facing as usize];
        let wall = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
        let solid = self.world.get_block(wall[0], wall[1], wall[2]).is_none_or(|b| b.is_solid());
        let free = self.world.get_block(p[0], p[1], p[2]).is_none_or(|b| !b.is_solid());
        solid && free
    }

    /// `HangingEntityItem.useOn` for item frames on face `face` of block `on`.
    pub fn place_frame(&self, on: [i32; 3], face: u8, glow: bool) -> bool {
        let o = OFFSETS[face as usize];
        let p = [on[0] + o[0], on[1] + o[1], on[2] + o[2]];
        let c = centre(p, face);
        if !self.frame_survives(c, face) {
            return false;
        }
        let bb = frame_box(c, face);
        let taken = self.entities.lock().values().any(|e| matches!(&e.kind, EKind::Frame { facing, .. } if frame_box(e.pos, *facing).intersects(&bb)));
        if taken {
            return false;
        }
        self.spawn_frame(p, face, glow, None, false);
        self.play_sound(if glow { "minecraft:entity.glow_item_frame.place" } else { "minecraft:entity.item_frame.place" }, 0, c, 1.0, 1.0);
        true
    }

    fn update_frame(&self, eid: i32, f: impl FnOnce(&mut Option<Stack>, &mut u8)) {
        let pkt = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return };
            let chunk = e.chunk();
            let EKind::Frame { item, rotation, .. } = &mut e.kind else { return };
            f(item, rotation);
            (chunk, self.frame_meta(eid, *item, *rotation))
        };
        self.broadcast_chunk(pkt.0, &pkt.1);
    }

    fn frame_state(&self, eid: i32) -> Option<(Option<Stack>, bool, bool, [f64; 3], u8)> {
        let ents = self.entities.lock();
        let e = ents.get(&eid)?;
        match &e.kind {
            EKind::Frame { item, glow, fixed, facing, .. } => Some((*item, *glow, *fixed, e.pos, *facing)),
            _ => None,
        }
    }

    pub fn is_frame(&self, eid: i32) -> bool {
        self.frame_state(eid).is_some()
    }

    pub fn frame_box_of(&self, eid: i32) -> Option<Aabb> {
        self.frame_state(eid).map(|s| frame_box(s.3, s.4))
    }

    /// `ItemFrame.interact`: put the held item in, or turn the item.
    pub fn use_frame(&self, p: &Arc<Player>, eid: i32, slot: usize) -> bool {
        let Some((item, glow, fixed, pos, _)) = self.frame_state(eid) else { return false };
        if fixed {
            return false;
        }
        let prefix = if glow { "minecraft:entity.glow_item_frame" } else { "minecraft:entity.item_frame" };
        match item {
            None => {
                let held = p.inv.lock().slots[slot];
                let Some(h) = held else { return false };
                self.update_frame(eid, |it, rot| {
                    *it = Some(Stack { count: 1, ..h });
                    *rot = 0;
                });
                if p.gamemode.load(Ordering::Relaxed) != 1 {
                    p.inv.lock().consume(slot, 1);
                    self.flush_inventory(p);
                }
                self.play_sound(&format!("{prefix}.add_item"), 0, pos, 1.0, 1.0);
            }
            Some(_) => {
                self.update_frame(eid, |_, rot| *rot = (*rot + 1) % 8);
                self.play_sound(&format!("{prefix}.rotate_item"), 0, pos, 1.0, 1.0);
            }
        }
        true
    }

    /// `ItemFrame.hurtServer` by a player: pop the item, then the frame.
    pub fn hit_frame(&self, p: Option<&Arc<Player>>, eid: i32) -> bool {
        let Some((item, glow, fixed, pos, _)) = self.frame_state(eid) else { return false };
        if fixed {
            return false;
        }
        let creative = p.is_some_and(|p| p.gamemode.load(Ordering::Relaxed) == 1);
        let prefix = if glow { "minecraft:entity.glow_item_frame" } else { "minecraft:entity.item_frame" };
        if let Some(it) = item {
            self.update_frame(eid, |i, r| {
                *i = None;
                *r = 0;
            });
            if !creative {
                self.drop_from_frame(pos, it);
            }
            self.play_sound(&format!("{prefix}.remove_item"), 0, pos, 1.0, 1.0);
            return true;
        }
        self.break_frame(eid, !creative);
        true
    }

    /// The frame pops off: drops itself (and its item) when `drops`.
    pub fn break_frame(&self, eid: i32, drops: bool) {
        let gone = {
            let mut ents = self.entities.lock();
            match ents.get(&eid).map(|e| &e.kind) {
                Some(EKind::Frame { .. }) => ents.remove(&eid),
                _ => None,
            }
        };
        let Some(e) = gone else { return };
        let EKind::Frame { item, glow, .. } = e.kind else { return };
        self.broadcast_chunk(e.chunk(), &self.remove_packet(&[eid]));
        self.play_sound(if glow { "minecraft:entity.glow_item_frame.break" } else { "minecraft:entity.item_frame.break" }, 0, e.pos, 1.0, 1.0);
        if drops {
            let name = if glow { "glow_item_frame" } else { "item_frame" };
            if let Some(id) = Items::get().id(name) {
                self.drop_from_frame(e.pos, Stack::new(id, 1));
            }
            if let Some(it) = item {
                self.drop_from_frame(e.pos, it);
            }
        }
    }

    fn drop_from_frame(&self, pos: [f64; 3], st: Stack) {
        let mut rng = rand::thread_rng();
        self.spawn_item(pos, st, [rng.gen_range(-0.05..0.05), 0.1, rng.gen_range(-0.05..0.05)], 10);
    }

    /// Every 100 ticks: frames whose wall is gone drop (`HangingEntity.tick`).
    pub(crate) fn check_frames(&self) {
        let list: Vec<(i32, [f64; 3], u8)> = self.entities.lock().values().filter_map(|e| if let EKind::Frame { facing, .. } = e.kind { Some((e.eid, e.pos, facing)) } else { None }).collect();
        for (eid, pos, facing) in list {
            if !self.frame_survives(pos, facing) {
                self.break_frame(eid, true);
            }
        }
    }
}
