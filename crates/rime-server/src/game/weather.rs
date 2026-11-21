//! Weather (`ServerLevel.advanceWeatherCycle`), precipitation per biome,
//! thunderstorm lightning (`tickThunder`) and the `LightningBolt` entity.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;

use rand::Rng;

use super::combat::damage::DamageSource;
use super::entity::{EKind, Entity};
use super::mobtick::Act;
use super::Game;
use crate::net::codec::Out;
use crate::registry::blocks;
use crate::vanilla::VanillaData;
use crate::worldgen::surface::{BiomeTemps, Climate};

/// `WeatherData` + rain / thunder levels.
#[derive(Clone, Debug, Default)]
pub struct Weather {
    pub clear_time: i32,
    pub rain_time: i32,
    pub thunder_time: i32,
    pub raining: bool,
    pub thundering: bool,
    pub rain_level: f32,
    pub thunder_level: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precipitation {
    None,
    Rain,
    Snow,
}

struct BiomeClimate {
    climate: Climate,
    has_precipitation: bool,
}

static CLIMATES: OnceLock<(HashMap<String, BiomeClimate>, BiomeTemps)> = OnceLock::new();

/// Loads biome temperatures / precipitation from the vanilla data.
pub fn load(v: &VanillaData) -> anyhow::Result<()> {
    let mut m = HashMap::new();
    for (key, path) in v.entries("worldgen/biome") {
        let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        m.insert(
            key.trim_start_matches("minecraft:").to_string(),
            BiomeClimate {
                climate: Climate { temperature: j["temperature"].as_f64().unwrap_or(0.5) as f32, frozen: j["temperature_modifier"].as_str() == Some("frozen") },
                has_precipitation: j["has_precipitation"].as_bool().unwrap_or(true),
            },
        );
    }
    let _ = CLIMATES.set((m, BiomeTemps::new()));
    Ok(())
}

/// `Biome.getPrecipitationAt(pos, seaLevel)`
pub fn precipitation(biome: &str, x: i32, y: i32, z: i32) -> Precipitation {
    let Some((m, temps)) = CLIMATES.get() else { return Precipitation::Rain };
    let Some(b) = m.get(biome.trim_start_matches("minecraft:")) else { return Precipitation::Rain };
    if !b.has_precipitation {
        Precipitation::None
    } else if temps.temperature(b.climate, x, y, z, 63) < 0.15 {
        Precipitation::Snow
    } else {
        Precipitation::Rain
    }
}

/// `Biome.getTemperature(pos, seaLevel)` (cold biomes freeze water, snow golems melt, …)
pub fn temperature(biome: &str, x: i32, y: i32, z: i32) -> f32 {
    let Some((m, temps)) = CLIMATES.get() else { return 0.8 };
    m.get(biome.trim_start_matches("minecraft:")).map(|b| temps.temperature(b.climate, x, y, z, 63)).unwrap_or(0.8)
}

impl Game {
    /// `Level.isRaining` (rain level above 0.2)
    pub fn raining(&self) -> bool {
        self.weather.lock().rain_level > 0.2
    }

    /// `Level.isThundering` (thunder level × rain level above 0.9)
    pub fn thundering(&self) -> bool {
        let w = self.weather.lock();
        w.thunder_level * w.rain_level > 0.9
    }

    /// `Level.isRainingAt`: rain falls on this block.
    pub fn raining_at(&self, p: [i32; 3]) -> bool {
        if !self.raining() {
            return false;
        }
        if !self.world.surface_y(p[0], p[2]).is_some_and(|top| p[1] > top) {
            return false;
        }
        self.world.biome(p[0], p[1], p[2]).is_some_and(|b| precipitation(&b, p[0], p[1], p[2]) == Precipitation::Rain)
    }

    /// `/weather` and the weather saved with the world.
    pub fn set_weather(&self, clear: i32, rain: i32, raining: bool, thundering: bool) {
        let mut w = self.weather.lock();
        w.clear_time = clear;
        w.rain_time = rain;
        w.thunder_time = rain;
        w.raining = raining;
        w.thundering = thundering;
    }

    /// Weather packets for a joining player (`PlayerList.sendLevelInfo`).
    pub fn weather_packets(&self) -> Vec<std::sync::Arc<Vec<u8>>> {
        let w = self.weather.lock().clone();
        if w.rain_level <= 0.2 {
            return Vec::new();
        }
        let ev = |id: u8, v: f32| {
            let mut b = Vec::new();
            b.u8(id).f32(v);
            self.encode("game_event", &b)
        };
        vec![ev(1, 0.0), ev(7, w.rain_level), ev(8, w.thunder_level)]
    }

    /// `advanceWeatherCycle` + thunder strikes; once per game tick.
    pub(super) fn tick_weather(&self) {
        let mut rng = rand::thread_rng();
        let was_raining = self.raining();
        let (o_rain, o_thunder, rain, thunder) = {
            let mut w = self.weather.lock();
            if w.clear_time > 0 {
                w.clear_time -= 1;
                w.thunder_time = if w.thundering { 0 } else { 1 };
                w.rain_time = if w.raining { 0 } else { 1 };
                w.thundering = false;
                w.raining = false;
            } else {
                if w.thunder_time > 0 {
                    w.thunder_time -= 1;
                    if w.thunder_time == 0 {
                        w.thundering = !w.thundering;
                    }
                } else if w.thundering {
                    w.thunder_time = rng.gen_range(3600..=15600);
                } else {
                    w.thunder_time = rng.gen_range(12000..=180000);
                }
                if w.rain_time > 0 {
                    w.rain_time -= 1;
                    if w.rain_time == 0 {
                        w.raining = !w.raining;
                    }
                } else if w.raining {
                    w.rain_time = rng.gen_range(12000..=24000);
                } else {
                    w.rain_time = rng.gen_range(12000..=180000);
                }
            }
            let (ot, or) = (w.thunder_level, w.rain_level);
            w.thunder_level = (w.thunder_level + if w.thundering { 0.01 } else { -0.01 }).clamp(0.0, 1.0);
            w.rain_level = (w.rain_level + if w.raining { 0.01 } else { -0.01 }).clamp(0.0, 1.0);
            (or, ot, w.rain_level, w.thunder_level)
        };
        let ev = |id: u8, v: f32| {
            let mut b = Vec::new();
            b.u8(id).f32(v);
            self.encode("game_event", &b)
        };
        if o_rain != rain {
            self.broadcast(ev(7, rain));
        }
        if o_thunder != thunder {
            self.broadcast(ev(8, thunder));
        }
        let now_raining = self.raining();
        if was_raining != now_raining {
            self.broadcast(ev(if was_raining { 2 } else { 1 }, 0.0));
            self.broadcast(ev(7, rain));
            self.broadcast(ev(8, thunder));
        }
        // tickThunder for every chunk in players' simulation range
        if now_raining && self.thundering() {
            let mut chunks = std::collections::HashSet::new();
            for p in self.players() {
                let pos = p.pose.lock().pos;
                let (cx, cz) = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
                for dx in -8..=8 {
                    for dz in -8..=8 {
                        chunks.insert((cx + dx, cz + dz));
                    }
                }
            }
            for (cx, cz) in chunks {
                if rng.gen_range(0..100000) != 0 {
                    continue;
                }
                let x = cx * 16 + rng.gen_range(0..16);
                let z = cz * 16 + rng.gen_range(0..16);
                let Some(top) = self.world.surface_y(x, z) else { continue };
                let mut pos = [x, top + 1, z];
                // findLightningTargetAround: a living entity under the sky nearby
                let area = crate::anticheat::combat::Aabb { min: [x as f64 - 3.0, top as f64 - 3.0, z as f64 - 3.0], max: [x as f64 + 4.0, 320.0 + 3.0, z as f64 + 4.0] };
                let cands: Vec<[f64; 3]> = self.living_in(&area).into_iter().map(|(_, p)| p).filter(|p| self.world.surface_y(p[0].floor() as i32, p[2].floor() as i32).is_some_and(|t| p[1].floor() as i32 > t)).collect();
                if !cands.is_empty() {
                    let c = cands[rng.gen_range(0..cands.len())];
                    pos = [c[0].floor() as i32, c[1].floor() as i32, c[2].floor() as i32];
                }
                if self.raining_at(pos) {
                    self.lightning_at([pos[0] as f64 + 0.5, pos[1] as f64, pos[2] as f64 + 0.5], None);
                }
            }
        }
    }

    /// Spawns a `LightningBolt` (life 2, 1–3 flashes).
    pub fn lightning_at(&self, pos: [f64; 3], cause: Option<i32>) {
        let flashes = rand::thread_rng().gen_range(0..3) + 1;
        let e = Entity { eid: self.next_eid(), uuid: uuid::Uuid::new_v4(), kind: EKind::Lightning { life: 2, flashes, cause, visual: false, hit: Vec::new() }, pos, vel: [0.0; 3], on_ground: false, age: 0, synced: pos };
        // thunder is heard everywhere (the client plays it from the entity)
        self.add_entity(e);
    }

    /// `LightningBolt.tick`; true when the bolt is gone.
    pub(super) fn tick_lightning(&self, e: &mut Entity, rng: &mut impl Rng, acts: &mut Vec<Act>) -> bool {
        let pos = e.pos;
        let EKind::Lightning { life, flashes, cause, visual, hit } = &mut e.kind else { return true };
        let mut fire = Vec::new();
        if *life == 2 && !*visual && self.difficulty() >= 2 {
            fire.extend(self.lightning_fire(pos, 4, rng));
        }
        *life -= 1;
        if *life < 0 {
            if *flashes == 0 {
                return true;
            } else if *life < -rng.gen_range(0..10) {
                *flashes -= 1;
                *life = 1;
                if !*visual {
                    fire.extend(self.lightning_fire(pos, 0, rng));
                }
            }
        }
        let _ = hit;
        let area = (*life >= 0 && !*visual).then(|| crate::anticheat::combat::Aabb { min: [pos[0] - 3.0, pos[1] - 3.0, pos[2] - 3.0], max: [pos[0] + 3.0, pos[1] + 6.0 + 3.0, pos[2] + 3.0] });
        if area.is_some() || !fire.is_empty() {
            acts.push(Act::Thunder { bolt: e.eid, cause: *cause, area, fire });
        }
        false
    }

    /// `LightningBolt.spawnFire` (positions only; placed after the tick).
    fn lightning_fire(&self, pos: [f64; 3], extra: i32, rng: &mut impl Rng) -> Vec<[i32; 3]> {
        let c = [pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32];
        let mut out = vec![c];
        for _ in 0..extra {
            out.push([c[0] + rng.gen_range(0..3) - 1, c[1] + rng.gen_range(0..3) - 1, c[2] + rng.gen_range(0..3) - 1]);
        }
        out
    }

    /// `thunderHit` on the struck entities and fire on the ground.
    pub(super) fn thunder(&self, bolt: i32, _cause: Option<i32>, area: Option<crate::anticheat::combat::Aabb>, fire: Vec<[i32; 3]>) {
        for p in fire {
            let Some(b) = self.world.get_block(p[0], p[1], p[2]) else { continue };
            let below = self.world.get_block(p[0], p[1] - 1, p[2]);
            if b.is_air() && below.is_some_and(|x| x.is_solid()) {
                let f = if below.is_some_and(|x| matches!(x.name(), "soul_sand" | "soul_soil")) { "soul_fire" } else { "fire" };
                if let Some(s) = crate::registry::BlockRegistry::get().state(f, &[]) {
                    self.change_block(p, s, false, None);
                }
            }
        }
        let bref = super::combat::damage::EntityRef::new(bolt, "lightning_bolt", [0.0; 3], false);
        // all entities in the box: living ones and items (items burn up)
        let hit: Vec<i32> = match area {
            Some(a) => self.living_in(&a).into_iter().map(|(e, _)| e).collect(),
            None => Vec::new(),
        };
        for eid in hit {
            // Entity.thunderHit: remainingFireTicks + 1, ignite 8 s when that gives 0
            self.thunder_fire(eid);
            let src = DamageSource { direct: Some(bref), ..DamageSource::new("lightning_bolt") };
            self.hurt_entity(eid, &src, 5.0);
        }
        let _ = blocks::AIR;
        let _ = Ordering::Relaxed;
    }

    fn thunder_fire(&self, eid: i32) {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            let mut v = p.vit.lock();
            v.fire += 1;
            if v.fire == 0 {
                v.fire = 160;
            }
            v.data_dirty = true;
            return;
        }
        let mut ents = self.entities.lock();
        if let Some(e) = ents.get_mut(&eid) {
            if let EKind::Mob(m) = &mut e.kind {
                m.fire += 1;
                if m.fire == 0 {
                    m.fire = 160;
                }
            }
        }
    }
}
