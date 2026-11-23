//! What commands need from a level's entities.

use super::entity::{EKind, Entity};
use super::Game;

/// A snapshot of a non-player entity for selectors.
#[derive(Clone, Debug)]
pub struct EntSnap {
    pub eid: i32,
    pub uuid: uuid::Uuid,
    pub pos: [f64; 3],
    pub rot: [f32; 2],
    pub ty: String,
    pub custom_name: Option<String>,
    pub living: bool,
    pub eye: f64,
}

impl Entity {
    /// Entity type name without namespace.
    pub fn type_name(&self) -> String {
        match &self.kind {
            EKind::Item { .. } => "item".into(),
            EKind::Falling { .. } => "falling_block".into(),
            EKind::Mob(m) => m.ty.name().into(),
            EKind::Projectile(p) => p.ty.entity_name().into(),
            EKind::Cloud(_) => "area_effect_cloud".into(),
            EKind::Lightning { .. } => "lightning_bolt".into(),
            EKind::Fangs { .. } => "evoker_fangs".into(),
            EKind::Tnt { .. } => "tnt".into(),
            EKind::Crystal { .. } => "end_crystal".into(),
            EKind::Boat { ty, .. } => (*ty).into(),
            EKind::Minecart(c) => c.ty.to_string(),
            EKind::Knot => "leash_knot".into(),
            EKind::Bobber(_) => "fishing_bobber".into(),
            EKind::Frame { glow, .. } => if *glow { "glow_item_frame" } else { "item_frame" }.into(),
        }
    }

    fn snap(&self) -> EntSnap {
        let (rot, name, living, eye) = match &self.kind {
            EKind::Mob(m) => ([m.mo.yaw, m.mo.pitch], m.v.custom_name.clone(), m.death == 0, m.ty.eye_height()),
            _ => ([0.0, 0.0], None, false, 0.0),
        };
        EntSnap { eid: self.eid, uuid: self.uuid, pos: self.pos, rot, ty: self.type_name(), custom_name: name, living, eye }
    }
}

impl Game {
    pub fn entity_snaps(&self) -> Vec<EntSnap> {
        self.entities.lock().values().filter(|e| !matches!(&e.kind, EKind::Mob(m) if m.death > 0)).map(|e| e.snap()).collect()
    }

    pub fn entity_snap(&self, eid: i32) -> Option<EntSnap> {
        self.entities.lock().get(&eid).map(|e| e.snap())
    }

    /// Removes an entity without dropping anything (`discard`).
    pub fn discard(&self, eid: i32) -> bool {
        let Some(e) = self.entities.lock().remove(&eid) else { return false };
        self.broadcast_chunk(e.chunk(), &self.remove_packet(&[eid]));
        true
    }

    pub fn take_entity(&self, eid: i32) -> Option<Entity> {
        let e = self.entities.lock().remove(&eid)?;
        self.broadcast_chunk(e.chunk(), &self.remove_packet(&[eid]));
        Some(e)
    }

    pub fn put_entity(&self, e: Entity) {
        self.add_entity(e);
    }

    /// Moves an entity within this level (clients get a full position sync).
    pub fn move_entity(&self, eid: i32, pos: [f64; 3], rot: Option<[f32; 2]>) -> bool {
        let removed = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return false };
            let old = e.chunk();
            e.pos = pos;
            e.vel = [0.0; 3];
            if let (Some(r), EKind::Mob(m)) = (rot, &mut e.kind) {
                m.mo.yaw = r[0];
                m.mo.pitch = r[1];
                m.mo.head_yaw = r[0];
                m.mo.body_yaw = r[0];
            }
            m_nav_stop(e);
            if old != e.chunk() {
                ents.remove(&eid).map(|x| (x, old))
            } else {
                None
            }
        };
        if let Some((e, old)) = removed {
            // re-add so viewers of the new chunk get a spawn packet
            self.broadcast_chunk(old, &self.remove_packet(&[eid]));
            self.add_entity(e);
        }
        true
    }

    pub fn leash_holder_of(&self, eid: i32) -> Option<i32> {
        self.leashes.lock().holder.get(&eid).copied()
    }

    pub fn mob_target(&self, eid: i32) -> Option<i32> {
        match &self.entities.lock().get(&eid)?.kind {
            EKind::Mob(m) => m.target,
            _ => None,
        }
    }

    pub fn mob_owner(&self, eid: i32) -> Option<uuid::Uuid> {
        match &self.entities.lock().get(&eid)?.kind {
            EKind::Mob(m) => m.owner,
            _ => None,
        }
    }

    /// Sets a mob's name tag.
    pub fn set_custom_name(&self, eid: i32, name: Option<String>) {
        let pkt = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return };
            let EKind::Mob(m) = &mut e.kind else { return };
            m.v.custom_name = name;
            m.persistent = true;
            (self.mob_meta(eid, m), e.chunk())
        };
        self.broadcast_chunk(pkt.1, &pkt.0);
    }
}

fn m_nav_stop(e: &mut Entity) {
    if let EKind::Mob(m) = &mut e.kind {
        m.nav.stop();
    }
}
