//! Custom spawners (`ServerLevel.customSpawners`): wandering traders,
//! pillager patrols, phantoms, village cats, and iron golems summoned by
//! villagers.

use std::sync::atomic::Ordering;

use rand::Rng;

use super::combat::living::HEAD;
use super::entity::EKind;
use super::mob::MobType;
use super::Game;
use crate::world::storage::Dim;

#[derive(Debug)]
pub struct Spawners {
    /// `WanderingTraderSpawner`: (tick delay, spawn delay, chance %)
    pub trader: (i32, i32, i32),
    /// `PatrolSpawner.nextTick`
    pub patrol: i32,
    /// `PhantomSpawner.nextTick`
    pub phantom: i32,
    /// `CatSpawner.nextTick`
    pub cat: i32,
}

impl Default for Spawners {
    fn default() -> Self {
        Spawners { trader: (1200, 24000, 25), patrol: 12000, phantom: 1200, cat: 600 }
    }
}

fn d2h(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)
}

impl Game {
    fn count_near(&self, ty: &str, p: [f64; 3], r: f64) -> usize {
        self.entities.lock().values().filter(|e| matches!(&e.kind, EKind::Mob(m) if m.ty.is(ty) && m.death == 0) && d2h(e.pos, p) < r * r && (e.pos[1] - p[1]).abs() < r).count()
    }

    fn surface(&self, x: i32, z: i32) -> Option<[f64; 3]> {
        let y = self.motion_blocking_height(x, z);
        let below = self.world.get_block(x, y - 1, z)?;
        if !below.is_solid() || below.name().contains("leaves") {
            return None;
        }
        Some([x as f64 + 0.5, y as f64, z as f64 + 0.5])
    }

    pub(crate) fn tick_custom_spawners(&self) {
        let players = self.players();
        // TIME_SINCE_REST
        for p in &players {
            let mut v = p.vit.lock();
            if v.sleeping.is_some() || v.dead() {
                v.since_rest = 0;
            } else {
                v.since_rest = v.since_rest.saturating_add(1);
            }
        }
        if self.world.dim != Dim::Overworld || players.is_empty() {
            return;
        }
        let mut rng = rand::thread_rng();
        let difficulty = self.difficulty();
        let alive: Vec<_> = players.iter().filter(|p| p.gamemode.load(Ordering::Relaxed) != 3 && !p.vit.lock().dead()).cloned().collect();
        if alive.is_empty() {
            return;
        }
        let mut sp = self.spawners.lock();
        // ---------------------------------------------------- wandering trader
        sp.trader.0 -= 1;
        if sp.trader.0 <= 0 {
            sp.trader.0 = 1200;
            sp.trader.1 -= 1200;
            if sp.trader.1 <= 0 {
                sp.trader.1 = 24000;
                let chance = sp.trader.2;
                sp.trader.2 = (sp.trader.2 + 25).clamp(25, 75);
                if rng.gen_range(0..100) < chance && rng.gen_range(0..10) == 0 {
                    let p = &alive[rng.gen_range(0..alive.len())];
                    let pp = p.pose.lock().pos;
                    for _ in 0..10 {
                        let (x, z) = (pp[0] as i32 + rng.gen_range(-48..=48), pp[2] as i32 + rng.gen_range(-48..=48));
                        if let Some(at) = self.surface(x, z) {
                            if let Some(t) = MobType::from_name("wandering_trader") {
                                let tr = self.spawn_mob_finalized(t, at, false);
                                self.with_mob_mut(tr, |m| {
                                    m.v.despawn_delay = 48000;
                                    m.home = Some(([at[0] as i32, at[1] as i32, at[2] as i32], 16));
                                });
                                if let Some(l) = MobType::from_name("trader_llama") {
                                    for _ in 0..2 {
                                        let ll = self.spawn_mob_finalized(l, [at[0] + rng.gen_range(-2.0..2.0), at[1], at[2] + rng.gen_range(-2.0..2.0)], false);
                                        self.attach_leash(ll, tr);
                                    }
                                }
                                sp.trader.2 = 25;
                            }
                            break;
                        }
                    }
                }
            }
        }
        // ---------------------------------------------------- patrols
        sp.patrol -= 1;
        if sp.patrol <= 0 {
            sp.patrol = 12000 + rng.gen_range(0..1200);
            let day = self.day_time() / 24000;
            if difficulty > 0 && day >= 5 && self.is_day() && rng.gen_range(0..5) == 0 {
                let p = &alive[rng.gen_range(0..alive.len())];
                let pp = p.pose.lock().pos;
                if !self.is_village(pp) {
                    let sx = if rng.gen_bool(0.5) { -1 } else { 1 };
                    let sz = if rng.gen_bool(0.5) { -1 } else { 1 };
                    let x = pp[0] as i32 + (24 + rng.gen_range(0..24)) * sx;
                    let z = pp[2] as i32 + (24 + rng.gen_range(0..24)) * sz;
                    let biome = self.world.biome(x, pp[1] as i32, z).unwrap_or_default();
                    if !biome.contains("mushroom") {
                        let n = difficulty + 1 + rng.gen_range(0..2);
                        let pt = MobType::from_name("pillager");
                        for i in 0..n {
                            let (Some(pt), Some(at)) = (pt, self.surface(x + rng.gen_range(-2..=2), z + rng.gen_range(-2..=2))) else { continue };
                            let eid = self.spawn_mob_finalized(pt, at, false);
                            if i == 0 {
                                // PatrollingMonster leader with the ominous banner
                                let pkt = self.with_mob_mut(eid, |m| {
                                    m.eq[HEAD] = super::raid::ominous_banner();
                                    m.drop_chance[HEAD] = 2.0;
                                });
                                let _ = pkt;
                            }
                        }
                    }
                }
            }
        }
        // ---------------------------------------------------- phantoms
        sp.phantom -= 1;
        if sp.phantom <= 0 {
            sp.phantom = (60 + rng.gen_range(0..60)) * 20;
            let dark = self.sky_darken() >= 5;
            if difficulty > 0 && (dark || self.thundering()) {
                for p in &alive {
                    let pp = p.pose.lock().pos;
                    let (x, y, z) = (pp[0].floor() as i32, pp[1].floor() as i32, pp[2].floor() as i32);
                    if y < self.motion_blocking_height(x, z) || pp[1] < 63.0 {
                        continue;
                    }
                    let rest = p.vit.lock().since_rest.clamp(1, i32::MAX);
                    if rng.gen_range(0..rest) < 72000 {
                        continue;
                    }
                    let Some(ph) = MobType::from_name("phantom") else { continue };
                    let n = 1 + rng.gen_range(0..difficulty + 1);
                    let at = [pp[0] + rng.gen_range(-10.0..10.0), pp[1] + 20.0 + rng.gen_range(0.0..15.0), pp[2] + rng.gen_range(-10.0..10.0)];
                    for _ in 0..n {
                        self.spawn_mob_finalized(ph, at, false);
                    }
                }
            }
        }
        // ---------------------------------------------------- cats & golems
        sp.cat -= 1;
        if sp.cat <= 0 {
            sp.cat = 1200;
            for p in &alive {
                let pp = p.pose.lock().pos;
                let (x, z) = (pp[0] as i32 + rng.gen_range(-24..=24), pp[2] as i32 + rng.gen_range(-24..=24));
                let Some(at) = self.surface(x, z) else { continue };
                // CatSpawner.spawnInVillage: villages with fewer than 5 cats
                let villagers = self.count_near("villager", at, 48.0);
                if villagers > 0 && self.count_near("cat", at, 48.0) < 5 {
                    if let Some(c) = MobType::from_name("cat") {
                        self.spawn_mob_finalized(c, at, false);
                    }
                }
                // Villager.spawnGolemIfNeeded: busy villages get an iron golem
                if villagers >= 5 && self.count_near("iron_golem", at, 32.0) == 0 {
                    if let Some(gt) = MobType::from_name("iron_golem") {
                        self.spawn_mob_finalized(gt, at, false);
                        self.play_sound("minecraft:entity.iron_golem.repair", 5, at, 1.0, 1.0);
                    }
                }
            }
        }
        drop(sp);
        // wandering traders leave after their time (`WanderingTrader.maybeDespawn`)
        let gone: Vec<i32> = {
            let mut ents = self.entities.lock();
            let mut v = Vec::new();
            for e in ents.values_mut() {
                if let EKind::Mob(m) = &mut e.kind {
                    if m.ty.is("wandering_trader") && m.v.despawn_delay > 0 && m.merchant.as_ref().is_none_or(|x| x.customer.is_none()) {
                        m.v.despawn_delay -= 1;
                        if m.v.despawn_delay == 0 {
                            v.push(e.eid);
                        }
                    }
                }
            }
            v
        };
        for eid in gone {
            // its llamas are let go
            let llamas: Vec<i32> = self.leashes.lock().holder.iter().filter(|(_, h)| **h == eid).map(|(m, _)| *m).collect();
            for l in llamas {
                self.drop_leash(l, true);
            }
            if let Some(c) = self.entities.lock().remove(&eid).map(|e| e.chunk()) {
                self.broadcast_chunk(c, &self.remove_packet(&[eid]));
            }
        }
    }

    pub(crate) fn with_mob_mut<R>(&self, eid: i32, f: impl FnOnce(&mut super::mob::Mob) -> R) -> Option<R> {
        let (r, pkt) = {
            let mut ents = self.entities.lock();
            let e = ents.get_mut(&eid)?;
            let chunk = e.chunk();
            let EKind::Mob(m) = &mut e.kind else { return None };
            let r = f(m);
            (r, (chunk, self.mob_equipment(eid, m)))
        };
        self.broadcast_chunk(pkt.0, &pkt.1);
        Some(r)
    }
}
