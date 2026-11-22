//! Entities kept across restarts: mobs, dropped items, end crystals and item
//! frames are written to `rime-entities-<dimension>.dat` (gzipped NBT) on
//! save and spawned again at start-up.

use std::io::{Read, Write};
use std::path::Path;

use super::entity::{EKind, Entity};
use super::mob::{Mob, MobType};
use super::Game;
use crate::data::{Items, Stack};
use crate::nbt::Tag;

fn s(k: &str, v: Tag) -> (String, Tag) {
    (k.to_string(), v)
}

pub fn stack_nbt(st: &Stack) -> Tag {
    let mut f = vec![s("id", Tag::String(format!("minecraft:{}", Items::get().name(st.item)))), s("count", Tag::Int(st.count as i32))];
    if let Some(c) = crate::components::to_nbt(st.comp) {
        f.push(s("components", c));
    }
    Tag::Compound(f)
}

pub fn stack_from(t: &Tag) -> Option<Stack> {
    let id = Items::get().id(t.get("id")?.as_str()?.trim_start_matches("minecraft:"))?;
    let n = t.get("count").and_then(|v| v.as_i64()).unwrap_or(1).clamp(1, 99) as u8;
    let mut st = Stack::new(id, n);
    if let Some(c) = t.get("components") {
        st.comp = crate::components::from_nbt(c);
    }
    Some(st)
}

fn pos3(p: [f64; 3]) -> Tag {
    Tag::List(p.iter().map(|v| Tag::Double(*v)).collect())
}

fn f64s(t: Option<&Tag>) -> Option<[f64; 3]> {
    match t {
        Some(Tag::List(l)) if l.len() == 3 => {
            let g = |t: &Tag| match t {
                Tag::Double(d) => Some(*d),
                Tag::Float(f) => Some(*f as f64),
                _ => None,
            };
            Some([g(&l[0])?, g(&l[1])?, g(&l[2])?])
        }
        _ => None,
    }
}

fn ints(t: Option<&Tag>) -> Option<[i32; 3]> {
    match t {
        Some(Tag::IntArray(a)) if a.len() == 3 => Some([a[0], a[1], a[2]]),
        _ => None,
    }
}

fn num(t: &Tag, k: &str) -> Option<i64> {
    t.get(k).and_then(|v| v.as_i64())
}

fn float(t: &Tag, k: &str) -> Option<f32> {
    match t.get(k)? {
        Tag::Float(f) => Some(*f),
        Tag::Double(d) => Some(*d as f32),
        other => other.as_i64().map(|v| v as f32),
    }
}

fn mob_nbt(m: &Mob) -> Vec<(String, Tag)> {
    let v = &m.v;
    let mut f = vec![
        s("Rotation", Tag::List(vec![Tag::Float(m.mo.yaw), Tag::Float(m.mo.pitch)])),
        s("Health", Tag::Float(m.living.health)),
        s("PersistenceRequired", Tag::Byte(m.persistent as i8)),
        s("Age", Tag::Int(m.age)),
        s("IsBaby", Tag::Byte(m.baby as i8)),
        s("Tame", Tag::Byte(m.tame as i8)),
        s("Sitting", Tag::Byte(m.ordered_sit as i8)),
        s("Size", Tag::Int(m.size)),
        s("NoAI", Tag::Byte(m.no_ai as i8)),
        s("LeftHanded", Tag::Byte(m.left_handed as i8)),
        s("CanPickUpLoot", Tag::Byte(m.can_pick_up as i8)),
        s("Fire", Tag::Int(m.fire)),
        s("DropChances", Tag::List(m.drop_chance.iter().map(|c| Tag::Float(*c)).collect())),
        s("Equipment", Tag::List(m.eq.iter().map(|e| e.as_ref().map(stack_nbt).unwrap_or(Tag::Compound(Vec::new()))).collect())),
        s("CollarColor", Tag::Byte(v.collar as i8)),
        s("Color", Tag::Byte(v.wool as i8)),
        s("Sheared", Tag::Byte(v.sheared as i8)),
        s("Variant", Tag::Int(v.variant)),
        s("Variant2", Tag::Int(v.variant2)),
        s("Powered", Tag::Byte(v.powered as i8)),
        s("Saddle", Tag::Byte(v.saddled as i8)),
        s("ChestedHorse", Tag::Byte(v.chested as i8)),
        s("Temper", Tag::Int(v.temper)),
        s("FromBucket", Tag::Byte(v.from_bucket as i8)),
        s("ShulkerColor", Tag::Byte(v.color as i8)),
        s("Peek", Tag::Int(v.peek)),
        s("AttachFace", Tag::Byte(v.attach_face as i8)),
        s("DragonPhase", Tag::Int(v.phase as i32)),
    ];
    if let Some(o) = m.owner {
        f.push(s("Owner", Tag::String(o.hyphenated().to_string())));
    }
    if let Some(n) = &v.custom_name {
        f.push(s("CustomName", Tag::String(n.clone())));
    }
    if let Some(b) = v.bound {
        f.push(s("Bound", Tag::IntArray(b.to_vec())));
    }
    if let Some((p, r)) = m.home {
        f.push(s("Home", Tag::IntArray(vec![p[0], p[1], p[2], r])));
    }
    if let Some(c) = v.carry {
        f.push(s("CarriedState", Tag::Int(c.0 as i32)));
    }
    f
}

fn mob_from(ty: MobType, t: &Tag) -> Mob {
    let persistent = num(t, "PersistenceRequired").unwrap_or(0) != 0;
    let mut m = Mob::new(ty, persistent);
    if let Some(Tag::List(r)) = t.get("Rotation") {
        if let (Some(Tag::Float(y)), Some(Tag::Float(p))) = (r.first(), r.get(1)) {
            m.mo.yaw = *y;
            m.mo.pitch = *p;
            m.mo.head_yaw = *y;
            m.mo.body_yaw = *y;
        }
    }
    m.age = num(t, "Age").unwrap_or(0) as i32;
    m.baby = num(t, "IsBaby").unwrap_or(0) != 0;
    m.tame = num(t, "Tame").unwrap_or(0) != 0;
    m.ordered_sit = num(t, "Sitting").unwrap_or(0) != 0;
    m.sitting = m.ordered_sit;
    m.size = num(t, "Size").unwrap_or(m.size as i64) as i32;
    m.no_ai = num(t, "NoAI").unwrap_or(0) != 0;
    m.left_handed = num(t, "LeftHanded").unwrap_or(0) != 0;
    m.can_pick_up = num(t, "CanPickUpLoot").unwrap_or(0) != 0;
    m.fire = num(t, "Fire").unwrap_or(0) as i32;
    if let Some(Tag::List(l)) = t.get("DropChances") {
        for (i, c) in l.iter().enumerate().take(m.drop_chance.len()) {
            if let Tag::Float(c) = c {
                m.drop_chance[i] = *c;
            }
        }
    }
    if let Some(Tag::List(l)) = t.get("Equipment") {
        for (i, e) in l.iter().enumerate().take(m.eq.len()) {
            m.eq[i] = stack_from(e);
        }
        let eq = m.eq;
        m.living.update_equipment(&eq);
    }
    m.owner = t.get("Owner").and_then(|o| o.as_str()).and_then(|o| uuid::Uuid::parse_str(o).ok());
    let v = &mut m.v;
    v.collar = num(t, "CollarColor").unwrap_or(v.collar as i64) as u8;
    v.wool = num(t, "Color").unwrap_or(0) as u8;
    v.sheared = num(t, "Sheared").unwrap_or(0) != 0;
    v.variant = num(t, "Variant").unwrap_or(0) as i32;
    v.variant2 = num(t, "Variant2").unwrap_or(0) as i32;
    v.powered = num(t, "Powered").unwrap_or(0) != 0;
    v.saddled = num(t, "Saddle").unwrap_or(0) != 0;
    v.chested = num(t, "ChestedHorse").unwrap_or(0) != 0;
    v.temper = num(t, "Temper").unwrap_or(0) as i32;
    v.from_bucket = num(t, "FromBucket").unwrap_or(0) != 0;
    v.color = num(t, "ShulkerColor").unwrap_or(v.color as i64) as u8;
    v.peek = num(t, "Peek").unwrap_or(0) as i32;
    v.attach_face = num(t, "AttachFace").unwrap_or(0) as u8;
    v.phase = num(t, "DragonPhase").unwrap_or(0) as u8;
    v.custom_name = t.get("CustomName").and_then(|n| n.as_str()).map(str::to_string);
    v.bound = ints(t.get("Bound"));
    v.carry = num(t, "CarriedState").map(|c| crate::registry::BlockState(c as u16 as _));
    if let Some(Tag::IntArray(h)) = t.get("Home") {
        if h.len() == 4 {
            m.home = Some(([h[0], h[1], h[2]], h[3]));
        }
    }
    let max = m.living.attrs.value(crate::game::combat::attr::Attr::MaxHealth) as f32;
    m.living.health = float(t, "Health").unwrap_or(max).clamp(0.5, max.max(0.5));
    m
}

impl Game {
    /// Entities worth keeping, as NBT.
    fn entities_nbt(&self) -> Tag {
        let ents = self.entities.lock();
        let mut out = Vec::new();
        for e in ents.values() {
            let mut f = vec![s("Pos", pos3(e.pos)), s("Motion", pos3(e.vel)), s("UUID", Tag::String(e.uuid.hyphenated().to_string()))];
            match &e.kind {
                EKind::Mob(m) if m.death == 0 => {
                    f.push(s("id", Tag::String(format!("minecraft:{}", m.ty.name()))));
                    f.extend(mob_nbt(m));
                }
                EKind::Item { stack, pickup_delay } => {
                    f.push(s("id", Tag::String("minecraft:item".into())));
                    f.push(s("Item", stack_nbt(stack)));
                    f.push(s("PickupDelay", Tag::Int(*pickup_delay as i32)));
                    f.push(s("Age", Tag::Int(e.age as i32)));
                }
                EKind::Crystal { show_bottom, beam, invulnerable } => {
                    f.push(s("id", Tag::String("minecraft:end_crystal".into())));
                    f.push(s("ShowBottom", Tag::Byte(*show_bottom as i8)));
                    f.push(s("Invulnerable", Tag::Byte(*invulnerable as i8)));
                    if let Some(b) = beam {
                        f.push(s("beam_target", Tag::IntArray(b.to_vec())));
                    }
                }
                EKind::Boat { ty, yaw, .. } => {
                    f.push(s("id", Tag::String(format!("minecraft:{ty}"))));
                    f.push(s("Yaw", Tag::Float(*yaw)));
                }
                EKind::Minecart(c) => {
                    f.push(s("id", Tag::String(format!("minecraft:{}", c.ty))));
                    f.push(s("Fuel", Tag::Int(c.fuel)));
                    f.push(s("Push", Tag::List(vec![Tag::Double(c.push[0]), Tag::Double(c.push[1])])));
                    f.push(s("Yaw", Tag::Float(c.yaw)));
                }
                EKind::Frame { item, rotation, facing, glow, fixed } => {
                    f.push(s("id", Tag::String(if *glow { "minecraft:glow_item_frame" } else { "minecraft:item_frame" }.into())));
                    if let Some(i) = item {
                        f.push(s("Item", stack_nbt(i)));
                    }
                    f.push(s("ItemRotation", Tag::Byte(*rotation as i8)));
                    f.push(s("Facing", Tag::Byte(*facing as i8)));
                    f.push(s("Fixed", Tag::Byte(*fixed as i8)));
                }
                _ => continue,
            }
            out.push(Tag::Compound(f));
        }
        Tag::Compound(vec![s("Entities", Tag::List(out))])
    }

    pub fn save_entities(&self, path: &Path) -> std::io::Result<()> {
        let tag = self.entities_nbt();
        let bytes = tag.to_root_bytes("");
        let tmp = path.with_extension("dat.tmp");
        {
            let f = std::fs::File::create(&tmp)?;
            let mut gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
            gz.write_all(&bytes)?;
            gz.finish()?.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// Spawn the saved entities again (at start-up).
    pub fn load_entities(&self, path: &Path) -> usize {
        let Ok(f) = std::fs::File::open(path) else { return 0 };
        let mut raw = Vec::new();
        if flate2::read::GzDecoder::new(f).read_to_end(&mut raw).is_err() {
            return 0;
        }
        let Ok((_, root)) = Tag::from_root_bytes(&raw) else { return 0 };
        let Some(Tag::List(list)) = root.get("Entities") else { return 0 };
        let mut n = 0;
        for t in list {
            let Some(pos) = f64s(t.get("Pos")) else { continue };
            let vel = f64s(t.get("Motion")).unwrap_or([0.0; 3]);
            let uuid = t.get("UUID").and_then(|u| u.as_str()).and_then(|u| uuid::Uuid::parse_str(u).ok()).unwrap_or_else(uuid::Uuid::new_v4);
            let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("").trim_start_matches("minecraft:");
            let mut eid_n = 1;
            let kind = match id {
                "item" => {
                    let Some(st) = t.get("Item").and_then(stack_from) else { continue };
                    EKind::Item { stack: st, pickup_delay: num(t, "PickupDelay").unwrap_or(0).max(0) as u32 }
                }
                b if super::ride::is_boat_type(b) => EKind::Boat { ty: Box::leak(b.to_string().into_boxed_str()), yaw: float(t, "Yaw").unwrap_or(0.0), damage: 0.0, hurt: 0, hurt_dir: 1 },
                c if super::minecart::is_cart_item(c) => {
                    let mut cart = super::minecart::Cart::new(Box::leak(c.to_string().into_boxed_str()));
                    cart.fuel = num(t, "Fuel").unwrap_or(0) as i32;
                    cart.yaw = float(t, "Yaw").unwrap_or(0.0);
                    if let Some(Tag::List(l)) = t.get("Push") {
                        if let (Some(Tag::Double(a)), Some(Tag::Double(b))) = (l.first(), l.get(1)) {
                            cart.push = [*a, *b];
                        }
                    }
                    EKind::Minecart(Box::new(cart))
                }
                "end_crystal" => EKind::Crystal { show_bottom: num(t, "ShowBottom").unwrap_or(1) != 0, beam: ints(t.get("beam_target")), invulnerable: num(t, "Invulnerable").unwrap_or(0) != 0 },
                "item_frame" | "glow_item_frame" => EKind::Frame {
                    item: t.get("Item").and_then(stack_from),
                    rotation: num(t, "ItemRotation").unwrap_or(0) as u8 % 8,
                    facing: num(t, "Facing").unwrap_or(2).clamp(0, 5) as u8,
                    glow: id == "glow_item_frame",
                    fixed: num(t, "Fixed").unwrap_or(0) != 0,
                },
                _ => {
                    let Some(ty) = MobType::from_name(id) else { continue };
                    if ty.is("ender_dragon") {
                        eid_n = 9;
                    }
                    EKind::Mob(Box::new(mob_from(ty, t)))
                }
            };
            let age = if id == "item" { num(t, "Age").unwrap_or(0).max(0) as u32 } else { 0 };
            let eid = self.next_eids(eid_n);
            self.add_entity(Entity { eid, uuid, kind, pos, vel, on_ground: false, age, synced: pos });
            n += 1;
        }
        n
    }
}
