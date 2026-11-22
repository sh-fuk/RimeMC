//! Player data (`<world>/playerdata/<uuid>.dat`): gzip NBT with vanilla's
//! keys, so worlds converted with `airworld-convert` keep their players.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use crate::data::{Items, Stack};
use crate::nbt::Tag;

use super::survival::Vitals;
use super::Player;

/// What a player file restores.
#[derive(Clone, Debug)]
pub struct Saved {
    pub pos: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub gamemode: i32,
    pub held: i32,
    /// (window slot, stack)
    pub slots: Vec<(usize, Stack)>,
    pub health: f32,
    pub absorption: f32,
    pub effects: Vec<crate::game::combat::effects::Instance>,
    pub food: i32,
    pub saturation: f32,
    pub exhaustion: f32,
    pub air: i32,
    pub fire: i32,
    pub fall_distance: f64,
    pub dead: bool,
    /// (XpLevel, XpP, XpTotal)
    pub xp: (i32, f32, i32),
    pub respawn: Option<[i32; 3]>,
    pub dim: crate::world::storage::Dim,
}

pub fn path(world: &Path, uuid: uuid::Uuid) -> PathBuf {
    world.join("playerdata").join(format!("{}.dat", uuid.hyphenated()))
}

/// Inventory window slot <-> vanilla `Inventory` slot byte.
fn to_vanilla_slot(window: usize) -> Option<i8> {
    match window {
        36..=44 => Some((window - 36) as i8),
        9..=35 => Some(window as i8),
        5 => Some(103u8 as i8),
        6 => Some(102u8 as i8),
        7 => Some(101u8 as i8),
        8 => Some(100u8 as i8),
        45 => Some(-106),
        _ => None,
    }
}

fn from_vanilla_slot(v: i8) -> Option<usize> {
    match v as u8 {
        0..=8 => Some(v as usize + 36),
        9..=35 => Some(v as usize),
        103 => Some(5),
        102 => Some(6),
        101 => Some(7),
        100 => Some(8),
        _ if v == -106 => Some(45),
        _ => None,
    }
}

fn f(v: &Tag) -> Option<f64> {
    match v {
        Tag::Float(x) => Some(*x as f64),
        Tag::Double(x) => Some(*x),
        other => other.as_i64().map(|i| i as f64),
    }
}

/// Snapshot of a player in vanilla's shape.
pub fn to_nbt(p: &Player) -> Tag {
    let pose = *p.pose.lock();
    let v = p.vit.lock().clone();
    let items = Items::get();
    let mut inv = Vec::new();
    let mut equipment = Vec::new();
    {
        let i = p.inv.lock();
        for (w, s) in i.slots.iter().enumerate() {
            let (Some(s), Some(slot)) = (s, to_vanilla_slot(w)) else { continue };
            if s.count == 0 {
                continue;
            }
            let mut fields = vec![("id".into(), Tag::String(format!("minecraft:{}", items.name(s.item)))), ("count".into(), Tag::Int(s.count as i32))];
            if let Some(c) = crate::components::to_nbt(s.comp) {
                fields.push(("components".into(), c));
            }
            let item = Tag::Compound(fields);
            // 1.21.5+: armour and offhand live in `equipment`
            let key = match slot as u8 {
                103 => Some("head"),
                102 => Some("chest"),
                101 => Some("legs"),
                100 => Some("feet"),
                _ if slot == -106 => Some("offhand"),
                _ => None,
            };
            match key {
                Some(k) => equipment.push((k.to_string(), item)),
                None => {
                    let Tag::Compound(mut c) = item else { unreachable!() };
                    c.insert(0, ("Slot".into(), Tag::Byte(slot)));
                    inv.push(Tag::Compound(c));
                }
            }
        }
    }
    let mut t = Tag::Compound(vec![
        ("DataVersion".into(), Tag::Int(crate::world::chunk::DATA_VERSION)),
        ("Pos".into(), Tag::List(pose.pos.iter().map(|v| Tag::Double(*v)).collect())),
        ("Rotation".into(), Tag::List(vec![Tag::Float(pose.yaw), Tag::Float(pose.pitch)])),
        ("OnGround".into(), Tag::Byte(pose.on_ground as i8)),
        ("Dimension".into(), Tag::String(crate::world::storage::Dim::from_index(p.dim.load(Ordering::Relaxed)).key().into())),
        ("playerGameType".into(), Tag::Int(p.gamemode.load(Ordering::Relaxed))),
        ("SelectedItemSlot".into(), Tag::Int(p.held.load(Ordering::Relaxed))),
        ("Inventory".into(), Tag::List(inv)),
        ("equipment".into(), Tag::Compound(equipment)),
        ("Health".into(), Tag::Float(if v.dead() { 0.0 } else { v.health() })),
        ("AbsorptionAmount".into(), Tag::Float(v.living.absorption)),
        ("active_effects".into(), Tag::List(v.living.effects.iter().map(|e| e.to_nbt()).collect())),
        ("foodLevel".into(), Tag::Int(v.food)),
        ("XpLevel".into(), Tag::Int(v.xp_level)),
        ("XpP".into(), Tag::Float(v.xp_progress)),
        ("XpTotal".into(), Tag::Int(v.xp_total)),
        ("foodSaturationLevel".into(), Tag::Float(v.saturation)),
        ("foodExhaustionLevel".into(), Tag::Float(v.exhaustion)),
        ("Air".into(), Tag::Short(v.air as i16)),
        ("Fire".into(), Tag::Short(v.fire.clamp(i16::MIN as i32, i16::MAX as i32) as i16)),
        ("fall_distance".into(), Tag::Double(v.fall_distance)),
        ("UUID".into(), Tag::IntArray(uuid_ints(p.uuid))),
    ]);
    if let (Some(r), Tag::Compound(f)) = (v.respawn, &mut t) {
        f.push((
            "respawn".into(),
            Tag::Compound(vec![
                ("dimension".into(), Tag::String("minecraft:overworld".into())),
                ("pos".into(), Tag::IntArray(vec![r[0], r[1], r[2]])),
                ("yaw".into(), Tag::Float(0.0)),
                ("pitch".into(), Tag::Float(0.0)),
                ("forced".into(), Tag::Byte(0)),
            ]),
        ));
    }
    t
}

fn uuid_ints(u: uuid::Uuid) -> Vec<i32> {
    let b = u.as_u128();
    vec![(b >> 96) as i32, (b >> 64) as i32, (b >> 32) as i32, b as i32]
}

pub fn from_nbt(t: &Tag) -> Option<Saved> {
    let pos = match t.get("Pos") {
        Some(Tag::List(l)) if l.len() == 3 => [f(&l[0])?, f(&l[1])?, f(&l[2])?],
        _ => return None,
    };
    if !pos.iter().all(|v| v.is_finite() && v.abs() < 3.0e7) {
        return None;
    }
    let (yaw, pitch) = match t.get("Rotation") {
        Some(Tag::List(l)) if l.len() == 2 => (f(&l[0]).unwrap_or(0.0) as f32, f(&l[1]).unwrap_or(0.0) as f32),
        _ => (0.0, 0.0),
    };
    let items = Items::get();
    let stack = |c: &Tag| -> Option<Stack> {
        let id = items.id(c.get("id")?.as_str()?)?;
        let n = c.get("count").or(c.get("Count")).and_then(|v| v.as_i64()).unwrap_or(1).clamp(1, 99) as u8;
        let mut s = Stack::new(id, n);
        if let Some(c) = c.get("components") {
            s.comp = crate::components::from_nbt(c);
        }
        Some(s)
    };
    let mut slots = Vec::new();
    if let Some(Tag::List(l)) = t.get("Inventory") {
        for c in l {
            let Some(slot) = c.get("Slot").and_then(|v| v.as_i64()) else { continue };
            if let (Some(w), Some(s)) = (from_vanilla_slot(slot as i8), stack(c)) {
                slots.push((w, s));
            }
        }
    }
    if let Some(Tag::Compound(eq)) = t.get("equipment") {
        for (k, c) in eq {
            let w = match k.as_str() {
                "head" => 5,
                "chest" => 6,
                "legs" => 7,
                "feet" => 8,
                "offhand" => 45,
                _ => continue,
            };
            if let Some(s) = stack(c) {
                slots.push((w, s));
            }
        }
    }
    let num = |k: &str| t.get(k).and_then(f);
    let health = num("Health").unwrap_or(20.0) as f32;
    Some(Saved {
        pos,
        yaw,
        pitch,
        gamemode: num("playerGameType").map(|v| v as i32).filter(|g| (0..=3).contains(g)).unwrap_or(-1),
        held: num("SelectedItemSlot").map(|v| v as i32).filter(|h| (0..9).contains(h)).unwrap_or(0),
        slots,
        health: health.max(0.0),
        absorption: num("AbsorptionAmount").unwrap_or(0.0).max(0.0) as f32,
        effects: match t.get("active_effects") {
            Some(Tag::List(l)) => l.iter().filter_map(crate::game::combat::effects::Instance::from_nbt).collect(),
            _ => Vec::new(),
        },
        food: num("foodLevel").unwrap_or(20.0).clamp(0.0, 20.0) as i32,
        saturation: num("foodSaturationLevel").unwrap_or(5.0).clamp(0.0, 20.0) as f32,
        exhaustion: num("foodExhaustionLevel").unwrap_or(0.0).clamp(0.0, 40.0) as f32,
        air: num("Air").unwrap_or(300.0).clamp(-20.0, 300.0) as i32,
        fire: num("Fire").unwrap_or(-20.0).max(-20.0) as i32,
        fall_distance: num("fall_distance").or(num("FallDistance")).unwrap_or(0.0).clamp(0.0, 1.0e4),
        dead: health <= 0.0,
        xp: (num("XpLevel").unwrap_or(0.0).max(0.0) as i32, num("XpP").unwrap_or(0.0).clamp(0.0, 1.0) as f32, num("XpTotal").unwrap_or(0.0).max(0.0) as i32),
        respawn: match t.get("respawn").and_then(|r| r.get("pos")) {
            Some(Tag::IntArray(a)) if a.len() == 3 => Some([a[0], a[1], a[2]]),
            _ => None,
        },
        dim: match t.get("Dimension") {
            Some(Tag::String(k)) => crate::world::storage::Dim::from_key(k).unwrap_or(crate::world::storage::Dim::Overworld),
            _ => crate::world::storage::Dim::Overworld,
        },
    })
}

pub fn load(path: &Path) -> Option<Saved> {
    let file = std::fs::File::open(path).ok()?;
    let mut raw = Vec::new();
    // bounded: a player file is a few KiB
    flate2::read::GzDecoder::new(file).take(16 << 20).read_to_end(&mut raw).ok()?;
    let (_, root) = Tag::from_root_bytes(&raw).ok()?;
    from_nbt(&root)
}

/// Player NBT (uncompressed root bytes) → saved state.
pub fn from_bytes(raw: &[u8]) -> Option<Saved> {
    let (_, root) = Tag::from_root_bytes(raw).ok()?;
    from_nbt(&root)
}

/// The file's NBT as uncompressed root bytes (RimeFleet forwards it).
pub fn load_raw(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(file).take(16 << 20).read_to_end(&mut raw).ok()?;
    Tag::from_root_bytes(&raw).ok()?;
    Some(raw)
}

/// Writes atomically (temp file + rename), like vanilla's `.dat_old` dance.
pub fn save(world: &Path, p: &Player) -> anyhow::Result<()> {
    save_raw(world, p.uuid, &to_nbt(p).to_root_bytes(""))
}

/// Saves player NBT given as uncompressed root bytes (validated first).
pub fn save_raw(world: &Path, uuid: uuid::Uuid, bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(bytes.len() < 16 << 20, "player data too large");
    from_bytes(bytes).ok_or_else(|| anyhow::anyhow!("invalid player data"))?;
    let dir = world.join("playerdata");
    std::fs::create_dir_all(&dir)?;
    let target = path(world, uuid);
    let tmp = target.with_extension("dat.tmp");
    {
        let f = std::fs::File::create(&tmp)?;
        let mut gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        gz.write_all(bytes)?;
        gz.finish()?.sync_all()?;
    }
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

impl Saved {
    /// Restores vitals (a dead player comes back dead and gets the death screen).
    pub fn vitals(&self) -> Vitals {
        let mut v = Vitals::default();
        for e in &self.effects {
            let info = crate::game::combat::enchant::EntInfo { type_name: "player", ..Default::default() };
            v.living.force_add_effect(&info, e.clone());
        }
        v.living.events.clear();
        // health is clamped to max health (incl. health boost) like setHealth
        v.living.set_health(self.health);
        v.living.set_absorption(self.absorption);
        v.living.events.clear();
        v.food = self.food;
        v.saturation = self.saturation;
        v.exhaustion = self.exhaustion;
        v.air = self.air;
        v.fire = self.fire;
        v.fall_distance = self.fall_distance;
        (v.xp_level, v.xp_progress, v.xp_total) = self.xp;
        v.respawn = self.respawn;
        if self.dead {
            v.living.health = 0.0;
            v.living.dead = true;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_mapping_round_trips() {
        for w in 5..=45 {
            if let Some(v) = to_vanilla_slot(w) {
                assert_eq!(from_vanilla_slot(v), Some(w), "window slot {w}");
            }
        }
        assert_eq!(to_vanilla_slot(36), Some(0));
        assert_eq!(to_vanilla_slot(45), Some(-106));
    }
}
