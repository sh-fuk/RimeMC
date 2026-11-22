//! Leads (`Leashable`): tying mobs to players and fence knots, pulling them
//! along and snapping when stretched too far.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::entity::{EKind, Entity};
use super::{Game, Player};
use crate::data::{Items, Stack};
use crate::net::codec::Out;

#[derive(Default)]
pub struct Leashes {
    /// leashed mob -> holder (a player or a knot entity)
    pub holder: HashMap<i32, i32>,
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

impl Game {
    pub(crate) fn link_packet(&self, mob: i32, holder: Option<i32>) -> Arc<Vec<u8>> {
        let mut b = Vec::new();
        b.i32(mob).i32(holder.unwrap_or(0));
        self.encode("set_entity_link", &b)
    }

    pub fn leash_holder(&self, mob: i32) -> Option<i32> {
        self.leashes.lock().holder.get(&mob).copied()
    }

    fn mob_chunk_pos(&self, eid: i32) -> Option<((i32, i32), [f64; 3])> {
        self.entities.lock().get(&eid).map(|e| (e.chunk(), e.pos))
    }

    /// `Mob.canBeLeashed`
    fn can_leash(&self, eid: i32) -> bool {
        let ents = self.entities.lock();
        match ents.get(&eid).map(|e| &e.kind) {
            Some(EKind::Mob(m)) => {
                let n = m.ty.name();
                m.death == 0 && (!m.ty.hostile() || matches!(n, "hoglin" | "zoglin")) && !matches!(n, "villager" | "wandering_trader" | "ender_dragon" | "wither" | "bat")
            }
            _ => false,
        }
    }

    pub fn attach_leash(&self, mob: i32, holder: i32) {
        self.leashes.lock().holder.insert(mob, holder);
        if let Some((c, _)) = self.mob_chunk_pos(mob) {
            self.broadcast_chunk(c, &self.link_packet(mob, Some(holder)));
        }
        let mut ents = self.entities.lock();
        if let Some(EKind::Mob(m)) = ents.get_mut(&mob).map(|e| &mut e.kind) {
            m.persistent = true;
        }
    }

    /// `dropLeash`: untie, dropping the lead item when `drop`.
    pub fn drop_leash(&self, mob: i32, drop: bool) {
        let had = self.leashes.lock().holder.remove(&mob);
        if had.is_none() {
            return;
        }
        if let Some((c, pos)) = self.mob_chunk_pos(mob) {
            self.broadcast_chunk(c, &self.link_packet(mob, None));
            if drop {
                if let Some(id) = Items::get().id("lead") {
                    self.spawn_item([pos[0], pos[1] + 0.5, pos[2]], Stack::new(id, 1), [0.0, 0.1, 0.0], 10);
                }
            }
        }
        if let Some(k) = had {
            self.remove_knot_if_unused(k);
        }
    }

    fn remove_knot_if_unused(&self, k: i32) {
        let is_knot = self.entities.lock().get(&k).is_some_and(|e| matches!(e.kind, EKind::Knot));
        if is_knot && !self.leashes.lock().holder.values().any(|h| *h == k) {
            if let Some(c) = self.entities.lock().remove(&k).map(|e| e.chunk()) {
                self.broadcast_chunk(c, &self.remove_packet(&[k]));
            }
        }
    }

    /// Right click on a mob: put a lead on it, or take ours off.
    pub fn leash_interact(&self, p: &Arc<Player>, eid: i32, held: Option<Stack>, slot: usize) -> Option<bool> {
        if self.leash_holder(eid) == Some(p.eid) {
            self.drop_leash(eid, p.gamemode.load(Ordering::Relaxed) != 1);
            self.play_sound("minecraft:item.lead.untied", 6, p.pose.lock().pos, 1.0, 1.0);
            return Some(true);
        }
        let name = held.map(|s| Items::get().name(s.item)).unwrap_or("");
        if name != "lead" || !self.can_leash(eid) {
            return None;
        }
        if let Some(old) = self.leash_holder(eid) {
            let _ = old;
            self.drop_leash(eid, true);
        }
        self.attach_leash(eid, p.eid);
        if p.gamemode.load(Ordering::Relaxed) != 1 {
            p.inv.lock().consume(slot, 1);
            self.flush_inventory(p);
        }
        let pos = self.mob_chunk_pos(eid).map(|x| x.1).unwrap_or([0.0; 3]);
        self.play_sound("minecraft:item.lead.tied", 6, pos, 1.0, 1.0);
        Some(true)
    }

    /// `LeadItem.bindPlayerMobs`: right click on a fence ties the player's
    /// mobs to a knot there.
    pub fn tie_to_fence(&self, p: &Arc<Player>, fence: [i32; 3]) -> bool {
        let mine: Vec<i32> = self.leashes.lock().holder.iter().filter(|(_, h)| **h == p.eid).map(|(m, _)| *m).collect();
        if mine.is_empty() {
            return false;
        }
        let pos = [fence[0] as f64 + 0.5, fence[1] as f64 + 0.375, fence[2] as f64 + 0.5];
        let existing = self.entities.lock().values().find(|e| matches!(e.kind, EKind::Knot) && dist2(e.pos, pos) < 0.01).map(|e| e.eid);
        let knot = existing.unwrap_or_else(|| {
            let eid = self.next_eid();
            self.add_entity(Entity { eid, uuid: uuid::Uuid::new_v4(), kind: EKind::Knot, pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos });
            self.play_sound("minecraft:entity.leash_knot.place", 4, pos, 1.0, 1.0);
            eid
        });
        for m in mine {
            self.attach_leash(m, knot);
        }
        true
    }

    /// Right click / hit on a knot: everything tied there comes loose.
    pub fn use_knot(&self, p: &Arc<Player>, knot: i32) -> bool {
        if !self.entities.lock().get(&knot).is_some_and(|e| matches!(e.kind, EKind::Knot)) {
            return false;
        }
        // the player's own mobs move to the knot first
        let mine: Vec<i32> = self.leashes.lock().holder.iter().filter(|(_, h)| **h == p.eid).map(|(m, _)| *m).collect();
        if !mine.is_empty() {
            for m in mine {
                self.attach_leash(m, knot);
            }
            return true;
        }
        let tied: Vec<i32> = self.leashes.lock().holder.iter().filter(|(_, h)| **h == knot).map(|(m, _)| *m).collect();
        let creative = p.gamemode.load(Ordering::Relaxed) == 1;
        for m in tied {
            self.drop_leash(m, !creative);
        }
        self.remove_knot_if_unused(knot);
        true
    }

    /// Per tick (`Leashable.tickLeash`): pull, snap, and untie from
    /// holders that are gone.
    pub(crate) fn tick_leashes(&self) {
        let list: Vec<(i32, i32)> = self.leashes.lock().holder.iter().map(|(m, h)| (*m, *h)).collect();
        if list.is_empty() {
            return;
        }
        let players = self.players();
        for (mob, holder) in list {
            let Some((_, mpos)) = self.mob_chunk_pos(mob) else {
                self.leashes.lock().holder.remove(&mob);
                continue;
            };
            let alive = matches!(self.entities.lock().get(&mob).map(|e| &e.kind), Some(EKind::Mob(m)) if m.death == 0);
            if !alive {
                self.drop_leash(mob, true);
                continue;
            }
            let hpos = match players.iter().find(|p| p.eid == holder) {
                Some(p) if !p.vit.lock().dead() => Some(p.pose.lock().pos),
                Some(_) => None,
                None => self.entities.lock().get(&holder).filter(|e| matches!(e.kind, EKind::Knot)).map(|e| e.pos),
            };
            let Some(hpos) = hpos else {
                self.drop_leash(mob, true);
                continue;
            };
            let d = dist2(mpos, hpos).sqrt();
            if d > 12.0 {
                self.drop_leash(mob, true);
                self.play_sound("minecraft:item.lead.break", 6, mpos, 1.0, 1.0);
                continue;
            }
            if d > 6.0 {
                // elasticity: pulled toward the holder
                let k = (d - 6.0) / d * 0.1;
                let mut ents = self.entities.lock();
                if let Some(e) = ents.get_mut(&mob) {
                    e.vel[0] += (hpos[0] - mpos[0]) * k;
                    e.vel[1] += (hpos[1] - mpos[1]) * k * 0.5;
                    e.vel[2] += (hpos[2] - mpos[2]) * k;
                    let cap = 0.6;
                    e.vel[0] = e.vel[0].clamp(-cap, cap);
                    e.vel[2] = e.vel[2].clamp(-cap, cap);
                }
            }
        }
    }

    /// Spawn packets for links (sent with the mob).
    pub(crate) fn leash_spawn_packet(&self, mob: i32) -> Option<Arc<Vec<u8>>> {
        self.leash_holder(mob).map(|h| self.link_packet(mob, Some(h)))
    }
}
