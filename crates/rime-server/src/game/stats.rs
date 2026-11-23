//! Player statistics (`ServerStatsCounter`): counting, `world/stats/<uuid>.json`
//! (vanilla's format), the statistics screen (`award_stats`) and the
//! scoreboard criteria built on them (`minecraft.custom:minecraft.jump` …).

use super::Player;
use crate::registry::Registries;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// `minecraft:stat_type` in registry order.
pub const TYPES: [&str; 9] = ["mined", "crafted", "used", "broken", "picked_up", "dropped", "killed", "killed_by", "custom"];
pub const MINED: u8 = 0;
pub const CRAFTED: u8 = 1;
pub const USED: u8 = 2;
pub const BROKEN: u8 = 3;
pub const PICKED_UP: u8 = 4;
pub const DROPPED: u8 = 5;
pub const KILLED: u8 = 6;
pub const KILLED_BY: u8 = 7;
pub const CUSTOM: u8 = 8;

/// The registry a stat type's values come from.
fn value_registry(ty: u8) -> &'static str {
    match ty {
        MINED => "minecraft:block",
        KILLED | KILLED_BY => "minecraft:entity_type",
        CUSTOM => "minecraft:custom_stat",
        _ => "minecraft:item",
    }
}

#[derive(Default)]
pub struct Stats {
    /// (type, value without namespace) → count
    pub map: BTreeMap<(u8, String), i32>,
    dirty: BTreeSet<(u8, String)>,
}

impl Stats {
    pub fn get(&self, ty: u8, key: &str) -> i32 {
        self.map.get(&(ty, key.to_string())).copied().unwrap_or(0)
    }

    /// Sets a value; returns the change.
    fn set(&mut self, ty: u8, key: &str, v: i32) -> i32 {
        let k = (ty, key.to_string());
        let old = self.map.get(&k).copied().unwrap_or(0);
        if old != v {
            self.map.insert(k.clone(), v);
            self.dirty.insert(k);
        }
        v.wrapping_sub(old)
    }

    /// `award_stats` with the changed values (or everything).
    pub fn packet_body(&mut self, all: bool) -> Vec<u8> {
        use crate::net::codec::Out;
        let reg = Registries::get();
        let keys: Vec<(u8, String)> = if all { self.map.keys().cloned().collect() } else { std::mem::take(&mut self.dirty).into_iter().collect() };
        if all {
            self.dirty.clear();
        }
        let mut rows = Vec::new();
        for (ty, key) in keys {
            let Some(id) = reg.id(value_registry(ty), &key) else { continue };
            rows.push((ty as i32, id, self.get(ty, &key)));
        }
        let mut b = Vec::new();
        b.vi(rows.len() as i32);
        for (t, id, v) in rows {
            b.vi(t).vi(id).vi(v);
        }
        b
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        for ((ty, key), v) in &self.map {
            let e = out.entry(format!("minecraft:{}", TYPES[*ty as usize])).or_insert_with(|| serde_json::json!({}));
            e[format!("minecraft:{key}")] = (*v).into();
        }
        serde_json::json!({ "stats": out, "DataVersion": crate::world::chunk::DATA_VERSION })
    }

    pub fn from_json(v: &serde_json::Value) -> Stats {
        let mut s = Stats::default();
        let Some(o) = v.get("stats").and_then(|x| x.as_object()) else { return s };
        for (ty, vals) in o {
            let Some(t) = TYPES.iter().position(|x| *x == ty.trim_start_matches("minecraft:")) else { continue };
            let Some(vals) = vals.as_object() else { continue };
            for (k, n) in vals {
                if let Some(n) = n.as_i64() {
                    s.map.insert((t as u8, k.trim_start_matches("minecraft:").to_string()), n.clamp(0, i32::MAX as i64) as i32);
                }
            }
        }
        s
    }
}

pub fn path(world: &Path, u: uuid::Uuid) -> PathBuf {
    world.join("stats").join(format!("{}.json", u.hyphenated()))
}

pub fn load(world: &Path, u: uuid::Uuid) -> Stats {
    std::fs::read_to_string(path(world, u)).ok().and_then(|s| serde_json::from_str(&s).ok()).map(|v| Stats::from_json(&v)).unwrap_or_default()
}

pub fn save(world: &Path, p: &Player) -> anyhow::Result<()> {
    let target = path(world, p.uuid);
    std::fs::create_dir_all(target.parent().unwrap())?;
    let body = serde_json::to_vec(&p.stats.lock().to_json())?;
    let tmp = target.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(tmp, target)?;
    Ok(())
}

/// Scoreboard criterion name of a stat (`minecraft.custom:minecraft.jump`).
pub fn criterion(ty: u8, key: &str) -> String {
    format!("minecraft.{}:minecraft.{}", TYPES[ty as usize], key)
}

fn scores(p: &Player, ty: u8, key: &str, delta: i32) {
    if delta == 0 {
        return;
    }
    let Some(st) = crate::command::server() else { return };
    for pkt in crate::command::state::award(&st.game, &p.name, &criterion(ty, key), delta) {
        st.broadcast_all(pkt);
    }
}

impl Player {
    /// `Player.awardStat`
    pub fn award(&self, ty: u8, key: &str, n: i32) {
        if n <= 0 {
            return;
        }
        let d = {
            let mut s = self.stats.lock();
            let v = s.get(ty, key).saturating_add(n);
            s.set(ty, key, v)
        };
        scores(self, ty, key, d);
    }

    pub fn award_custom(&self, key: &str, n: i32) {
        self.award(CUSTOM, key, n);
    }

    /// `Player.resetStat`
    pub fn reset_stat(&self, ty: u8, key: &str) {
        let d = self.stats.lock().set(ty, key, 0);
        scores(self, ty, key, d);
    }

    pub fn stat(&self, ty: u8, key: &str) -> i32 {
        self.stats.lock().get(ty, key)
    }

    /// Statistics counted every tick (`ServerPlayer.tick` / `Player.tick`).
    pub fn tick_stats(&self, sneaking: bool, sleeping: bool) {
        if self.vit.lock().living.health <= 0.0 {
            return;
        }
        self.award_custom("play_time", 1);
        self.award_custom("total_world_time", 1);
        self.award_custom("time_since_death", 1);
        if sneaking {
            self.award_custom("sneak_time", 1);
        }
        if !sleeping {
            self.award_custom("time_since_rest", 1);
        }
    }
}

/// Damage amounts are counted in tenths of a heart point.
pub fn damage_units(amount: f32) -> i32 {
    (amount * 10.0).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_and_packet() {
        let mut s = Stats::default();
        s.set(CUSTOM, "jump", 5);
        s.set(MINED, "stone", 2);
        s.set(KILLED, "zombie", 1);
        let j = s.to_json();
        assert_eq!(j["stats"]["minecraft:custom"]["minecraft:jump"], 5);
        let mut t = Stats::from_json(&j);
        assert_eq!(t.get(MINED, "stone"), 2);
        assert_eq!(t.get(KILLED, "zombie"), 1);
        let b = t.packet_body(true);
        assert_eq!(b[0], 3);
        assert_eq!(criterion(CUSTOM, "jump"), "minecraft.custom:minecraft.jump");
    }
}
