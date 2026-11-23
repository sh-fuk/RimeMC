//! Raids (`Raid`, `Raids`, `RaidOmenMobEffect`): Bad Omen turns into Raid
//! Omen in a village, which starts a raid when it runs out. Waves of
//! illagers come from outside the village with a captain carrying the
//! ominous banner; the raid bar shows the countdown and the raiders' health;
//! winning gives Hero of the Village.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::Rng;

use super::ai::Slot;
use super::combat::effects::{Effect, Instance};
use super::combat::living::HEAD;
use super::entity::EKind;
use super::mob::MobType;
use super::{Game, Player};
use crate::data::{Items, Stack};
use crate::net::codec::Out;
use crate::world::storage::Dim;

/// `Raid.VALID_RAID_RADIUS_SQR`
const RADIUS: f64 = 96.0;
const PRE_WAVE: i32 = 300;
const MAX_TICKS: i32 = 48000;
const CELEBRATE: i32 = 600;
const RAID_OMEN_TICKS: i32 = 600;

/// raiders per wave (`Raid.RaiderType`), index = wave number
const WAVES: [(&str, [i32; 8]); 5] = [
    ("vindicator", [0, 0, 2, 0, 1, 4, 2, 5]),
    ("evoker", [0, 0, 0, 0, 0, 1, 1, 2]),
    ("pillager", [0, 4, 3, 3, 4, 4, 4, 2]),
    ("witch", [0, 0, 0, 0, 3, 0, 0, 1]),
    ("ravager", [0, 0, 0, 1, 0, 1, 0, 2]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ongoing,
    Victory,
    Loss,
    Stopped,
}

#[derive(Debug)]
pub struct Raid {
    pub center: [i32; 3],
    /// `raidOmenLevel` (1..=5)
    pub omen: i32,
    /// waves for the difficulty (3 / 5 / 7)
    pub groups: i32,
    pub spawned: i32,
    pub raiders: HashSet<i32>,
    pub total_health: f32,
    pub cooldown: i32,
    pub active: i32,
    pub status: Status,
    pub celebrate: i32,
    pub no_village: i32,
    pub bar: uuid::Uuid,
    pub viewers: HashSet<i32>,
    pub bar_progress: f32,
    pub bar_title: String,
}

impl Raid {
    pub fn new(center: [i32; 3], omen: i32, difficulty: i32) -> Raid {
        Raid {
            center,
            omen: omen.clamp(1, 5),
            groups: match difficulty {
                1 => 3,
                2 => 5,
                _ => 7,
            },
            spawned: 0,
            raiders: HashSet::new(),
            total_health: 0.0,
            cooldown: PRE_WAVE,
            active: 0,
            status: Status::Ongoing,
            celebrate: 0,
            no_village: 0,
            bar: uuid::Uuid::new_v4(),
            viewers: HashSet::new(),
            bar_progress: 0.0,
            bar_title: String::new(),
        }
    }

    pub fn bonus_wave(&self) -> bool {
        self.omen > 1
    }

    pub fn total_waves(&self) -> i32 {
        self.groups + self.bonus_wave() as i32
    }

    fn all_spawned(&self) -> bool {
        self.spawned >= self.total_waves()
    }

    fn centre(&self) -> [f64; 3] {
        [self.center[0] as f64 + 0.5, self.center[1] as f64, self.center[2] as f64 + 0.5]
    }

    fn in_area(&self, p: [f64; 3]) -> bool {
        let c = self.centre();
        (p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2) + (p[2] - c[2]).powi(2) < RADIUS * RADIUS
    }
}

/// `Raid.getDefaultNumSpawns` + `getPotentialBonusSpawns`
pub fn wave_spawns(ty: &str, wave: i32, groups: i32, bonus: bool, difficulty: i32, rng: &mut impl Rng) -> i32 {
    let Some((_, arr)) = WAVES.iter().find(|(n, _)| *n == ty) else { return 0 };
    let idx = if bonus { groups } else { wave } as usize;
    let base = arr.get(idx.min(7)).copied().unwrap_or(0);
    let extra = match ty {
        "vindicator" | "pillager" => {
            let i = match difficulty {
                1 => rng.gen_range(0..2),
                2 => 1,
                _ => 2,
            };
            rng.gen_range(0..=i)
        }
        "witch" => {
            if difficulty == 1 || wave <= 2 || wave == 4 {
                0
            } else {
                rng.gen_range(0..=1)
            }
        }
        "ravager" => {
            if difficulty != 1 && bonus {
                rng.gen_range(0..=1)
            } else {
                0
            }
        }
        _ => 0,
    };
    base + extra
}

/// `Raid.getOminousBannerInstance`
pub fn ominous_banner() -> Option<Stack> {
    let id = Items::get().id("white_banner")?;
    let layers = [("rhombus", "cyan"), ("stripe_bottom", "light_gray"), ("stripe_center", "gray"), ("border", "light_gray"), ("stripe_middle", "black"), ("half_horizontal", "light_gray"), ("circle", "light_gray"), ("border", "black")];
    Some(Stack::new(id, 1).with_patch(|p| {
        p.banner = layers.iter().filter_map(|(n, c)| Some((n.to_string(), crate::components::dye_id(c)?))).collect();
    }))
}

impl Game {
    /// Is there a village around `p` (villagers living nearby)?
    pub fn is_village(&self, p: [f64; 3]) -> bool {
        self.entities.lock().values().any(|e| matches!(&e.kind, EKind::Mob(m) if m.ty.is("villager") && m.death == 0) && (e.pos[0] - p[0]).powi(2) + (e.pos[2] - p[2]).powi(2) < 64.0 * 64.0 && (e.pos[1] - p[1]).abs() < 32.0)
    }

    fn raid_at(&self, p: [f64; 3]) -> Option<usize> {
        self.raids.lock().iter().position(|r| r.status == Status::Ongoing && r.in_area(p))
    }

    /// `Raids.createOrExtendRaid`
    pub fn start_raid(&self, center: [i32; 3], omen: i32) {
        if !super::rules::on("raids") {
            return;
        }
        let pos = [center[0] as f64 + 0.5, center[1] as f64, center[2] as f64 + 0.5];
        if let Some(i) = self.raid_at(pos) {
            let mut raids = self.raids.lock();
            let r = &mut raids[i];
            r.omen = r.omen.max(omen).min(5);
            return;
        }
        let difficulty = self.difficulty();
        if difficulty == 0 {
            return;
        }
        self.raids.lock().push(Raid::new(center, omen, difficulty));
    }

    /// Per tick (overworld): omens, waves, the raid bar, victory and defeat.
    pub(crate) fn tick_raids(&self) {
        if self.world.dim != Dim::Overworld {
            return;
        }
        let players = self.players();
        self.tick_omens(&players);
        if self.raids.lock().is_empty() {
            return;
        }
        let difficulty = self.difficulty();
        let now = self.game_time();
        let n = self.raids.lock().len();
        let mut done = Vec::new();
        for i in 0..n {
            let (status, center, spawn_wave, ended_now) = {
                let mut raids = self.raids.lock();
                let r = &mut raids[i];
                r.active += 1;
                // forget dead / vanished raiders
                let ents = self.entities.lock();
                r.raiders.retain(|e| matches!(ents.get(e).map(|x| &x.kind), Some(EKind::Mob(m)) if m.death == 0));
                drop(ents);
                let mut spawn_wave = false;
                let mut ended_now = false;
                if r.status == Status::Ongoing {
                    if difficulty == 0 || r.active >= MAX_TICKS {
                        r.status = Status::Stopped;
                    } else if now % 20 == 0 {
                        let village = self.is_village(r.centre());
                        r.no_village = if village { 0 } else { r.no_village + 1 };
                        if r.no_village >= 3 {
                            // Raid.tick: no village left
                            r.status = Status::Loss;
                            r.celebrate = CELEBRATE;
                            ended_now = true;
                        }
                    }
                    if r.status == Status::Ongoing && r.raiders.is_empty() {
                        if r.all_spawned() {
                            r.status = Status::Victory;
                            r.celebrate = CELEBRATE;
                            ended_now = true;
                        } else if r.cooldown > 0 {
                            r.cooldown -= 1;
                        } else {
                            spawn_wave = true;
                            r.cooldown = PRE_WAVE;
                        }
                    }
                } else if r.celebrate > 0 {
                    r.celebrate -= 1;
                }
                (r.status, r.center, spawn_wave, ended_now)
            };
            if spawn_wave {
                self.spawn_raid_wave(i, center);
            }
            if ended_now {
                self.end_raid(i, status, &players);
            }
            self.update_raid_bar(i, &players);
            let over = {
                let raids = self.raids.lock();
                let r = &raids[i];
                r.status == Status::Stopped || (r.status != Status::Ongoing && r.celebrate == 0)
            };
            if over {
                done.push(i);
            }
        }
        for i in done.into_iter().rev() {
            let r = self.raids.lock().remove(i);
            for p in players.iter().filter(|p| r.viewers.contains(&p.eid)) {
                let mut b = Vec::new();
                b.extend_from_slice(r.bar.as_bytes());
                b.vi(1);
                p.send(self.encode("boss_event", &b));
            }
        }
    }

    /// Bad Omen in a village becomes Raid Omen; when that runs out the raid
    /// starts where it was triggered (`RaidOmenMobEffect.onMobRemoved`).
    fn tick_omens(&self, players: &[Arc<Player>]) {
        for p in players {
            if p.gamemode.load(Ordering::Relaxed) == 3 || p.vit.lock().dead() || self.difficulty() == 0 {
                continue;
            }
            let pos = p.pose.lock().pos;
            let bad = p.vit.lock().living.effect(Effect::BadOmen).map(|i| i.amplifier);
            if let Some(amp) = bad {
                if self.raid_at(pos).is_none() && self.is_village(pos) {
                    self.with_player(p, |l, _, body| {
                        l.remove_effect(Effect::BadOmen);
                        l.add_effect(&body.info, Instance::new(Effect::RaidOmen, RAID_OMEN_TICKS, amp));
                    });
                    let c = [pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32];
                    self.omens.lock().insert(p.eid, (c, amp + 1, RAID_OMEN_TICKS));
                    self.play_sound("minecraft:event.mob_effect.raid_omen", 6, pos, 1.0, 1.0);
                }
            }
        }
        let mut start = Vec::new();
        {
            let mut omens = self.omens.lock();
            omens.retain(|eid, (c, lvl, t)| {
                let Some(p) = players.iter().find(|p| p.eid == *eid) else { return false };
                // milk (or death) takes the omen away
                if !p.vit.lock().living.has_effect(Effect::RaidOmen) && *t > 1 {
                    return false;
                }
                *t -= 1;
                if *t <= 0 {
                    start.push((*c, *lvl));
                    return false;
                }
                true
            });
        }
        for (c, lvl) in start {
            self.start_raid(c, lvl);
        }
    }

    /// `Raid.findRandomSpawnPos`: a surface spot about 32 blocks out.
    fn raid_spawn_pos(&self, c: [i32; 3]) -> [f64; 3] {
        let mut rng = rand::thread_rng();
        for dist in [32.0, 24.0, 16.0, 8.0] {
            let a: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
            let x = c[0] + (a.cos() * dist) as i32;
            let z = c[2] + (a.sin() * dist) as i32;
            let y = self.motion_blocking_height(x, z);
            let feet = self.world.get_block(x, y - 1, z);
            if feet.is_some_and(|b| b.is_solid()) {
                return [x as f64 + 0.5, y as f64, z as f64 + 0.5];
            }
        }
        [c[0] as f64 + 0.5, c[1] as f64, c[2] as f64 + 0.5]
    }

    /// `Raid.spawnGroup`
    fn spawn_raid_wave(&self, i: usize, center: [i32; 3]) {
        let mut rng = rand::thread_rng();
        let difficulty = self.difficulty();
        let (wave, groups, bonus) = {
            let mut raids = self.raids.lock();
            let r = &mut raids[i];
            let bonus = r.spawned == r.groups && r.bonus_wave();
            r.spawned += 1;
            (r.spawned, r.groups, bonus)
        };
        let pos = self.raid_spawn_pos(center);
        let mut leader = false;
        let mut members = Vec::new();
        for (ty, _) in WAVES {
            let n = wave_spawns(ty, wave, groups, bonus, difficulty, &mut rng);
            let Some(mt) = MobType::from_name(ty) else { continue };
            for k in 0..n {
                let eid = self.spawn_mob_finalized(mt, [pos[0] + rng.gen_range(-1.0..1.0), pos[1], pos[2] + rng.gen_range(-1.0..1.0)], false);
                let captain = !leader && ty != "witch" && ty != "ravager";
                if captain {
                    leader = true;
                }
                self.join_raid(eid, center, captain);
                members.push(eid);
                // ravagers bring riders on the later waves
                if ty == "ravager" {
                    let rider = if wave == 5 {
                        Some("pillager")
                    } else if wave >= 7 {
                        Some(if k == 0 { "evoker" } else { "vindicator" })
                    } else {
                        None
                    };
                    if let Some(rt) = rider.and_then(MobType::from_name) {
                        let r = self.spawn_mob_finalized(rt, pos, false);
                        self.join_raid(r, center, false);
                        self.mount(r, eid);
                        members.push(r);
                    }
                }
            }
        }
        let health: f32 = {
            let ents = self.entities.lock();
            members.iter().filter_map(|e| match ents.get(e).map(|x| &x.kind) {
                Some(EKind::Mob(m)) => Some(m.living.health),
                _ => None,
            }).sum()
        };
        {
            let mut raids = self.raids.lock();
            let r = &mut raids[i];
            r.raiders.extend(members);
            r.total_health = health.max(1.0);
        }
        // the raid horn for everyone in the area
        let c = [center[0] as f64, center[1] as f64, center[2] as f64];
        for p in self.players() {
            let pp = p.pose.lock().pos;
            if (pp[0] - c[0]).powi(2) + (pp[2] - c[2]).powi(2) < RADIUS * RADIUS {
                let (dx, dz) = (pos[0] - pp[0], pos[2] - pp[2]);
                let l = (dx * dx + dz * dz).sqrt().max(1.0);
                let at = [pp[0] + dx / l * 13.0, pp[1], pp[2] + dz / l * 13.0];
                self.play_sound("minecraft:event.raid.horn", 7, at, 64.0, 1.0);
            }
        }
    }

    /// `Raid.joinRaid`: walk to the village; captains carry the banner.
    fn join_raid(&self, eid: i32, center: [i32; 3], captain: bool) {
        let pkt = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return };
            let EKind::Mob(m) = &mut e.kind else { return };
            m.persistent = true;
            m.home = Some((center, 16));
            m.goals.push(Slot::new(4, super::ai::goals::mk::towards_restriction(1.0)));
            if captain {
                m.eq[HEAD] = ominous_banner();
                m.drop_chance[HEAD] = 2.0;
            }
            self.mob_equipment(eid, m)
        };
        if let Some(c) = self.entity_chunk(eid) {
            self.broadcast_chunk(c, &pkt);
        }
    }

    /// Victory: Hero of the Village for everyone who stood by; defeat: the
    /// raiders celebrate.
    fn end_raid(&self, i: usize, status: Status, players: &[Arc<Player>]) {
        let (omen, centre) = {
            let raids = self.raids.lock();
            (raids[i].omen, raids[i].centre())
        };
        if status == Status::Victory {
            for p in players {
                let pp = p.pose.lock().pos;
                if (pp[0] - centre[0]).powi(2) + (pp[1] - centre[1]).powi(2) + (pp[2] - centre[2]).powi(2) < RADIUS * RADIUS && p.gamemode.load(Ordering::Relaxed) != 3 {
                    self.with_player(p, |l, _, body| {
                        l.add_effect(&body.info, Instance::new(Effect::HeroOfTheVillage, 48000, omen - 1));
                    });
                    self.trigger_simple(p, "hero_of_the_village");
                    p.award_custom("raid_win", 1);
                }
            }
            self.play_sound("minecraft:event.raid.horn", 7, centre, 0.0, 1.0);
        }
    }

    fn update_raid_bar(&self, i: usize, players: &[Arc<Player>]) {
        let mut raids = self.raids.lock();
        let r = &mut raids[i];
        let health: f32 = {
            let ents = self.entities.lock();
            r.raiders.iter().filter_map(|e| match ents.get(e).map(|x| &x.kind) {
                Some(EKind::Mob(m)) => Some(m.living.health),
                _ => None,
            }).sum()
        };
        let (progress, title) = match r.status {
            Status::Victory => (0.0, "event.minecraft.raid.victory.full".to_string()),
            Status::Loss => (0.0, "event.minecraft.raid.defeat.full".to_string()),
            _ if r.raiders.is_empty() => (1.0 - r.cooldown as f32 / PRE_WAVE as f32, "event.minecraft.raid".to_string()),
            _ => ((health / r.total_health).clamp(0.0, 1.0), if r.raiders.len() <= 2 { format!("raiders:{}", r.raiders.len()) } else { "event.minecraft.raid".to_string() }),
        };
        let title_nbt = |t: &str| -> Vec<u8> {
            let j = match t.strip_prefix("raiders:") {
                Some(n) => serde_json::json!({"translate": "event.minecraft.raid.raiders_remaining", "with": [n]}),
                None if t.ends_with(".full") => serde_json::json!({"translate": t, "with": [{"translate": "event.minecraft.raid"}]}),
                None => serde_json::json!({"translate": t}),
            };
            crate::nbt::Tag::from_json(&j).to_network_bytes()
        };
        let mut sends: Vec<(Arc<Player>, Arc<Vec<u8>>)> = Vec::new();
        for p in players {
            let near = !p.vit.lock().dead() && r.in_area(p.pose.lock().pos);
            let has = r.viewers.contains(&p.eid);
            if near && !has {
                r.viewers.insert(p.eid);
                let mut b = Vec::new();
                b.extend_from_slice(r.bar.as_bytes());
                b.vi(0).raw(&title_nbt(&title)).f32(progress).vi(2).vi(2).u8(0);
                sends.push((p.clone(), self.encode("boss_event", &b)));
            } else if !near && has {
                r.viewers.remove(&p.eid);
                let mut b = Vec::new();
                b.extend_from_slice(r.bar.as_bytes());
                b.vi(1);
                sends.push((p.clone(), self.encode("boss_event", &b)));
            }
        }
        r.viewers.retain(|v| players.iter().any(|p| p.eid == *v));
        let mut updates = Vec::new();
        if (progress - r.bar_progress).abs() > 1e-3 {
            r.bar_progress = progress;
            let mut b = Vec::new();
            b.extend_from_slice(r.bar.as_bytes());
            b.vi(2).f32(progress);
            updates.push(self.encode("boss_event", &b));
        }
        if title != r.bar_title {
            r.bar_title = title.clone();
            let mut b = Vec::new();
            b.extend_from_slice(r.bar.as_bytes());
            b.vi(3).raw(&title_nbt(&title));
            updates.push(self.encode("boss_event", &b));
        }
        let viewers = r.viewers.clone();
        drop(raids);
        for (p, pkt) in sends {
            p.send(pkt);
        }
        for p in players.iter().filter(|p| viewers.contains(&p.eid)) {
            for u in &updates {
                p.send(u.clone());
            }
        }
    }

    /// Is this mob part of a raid? (raiders in a raid don't despawn)
    pub fn in_raid(&self, eid: i32) -> bool {
        self.raids.lock().iter().any(|r| r.raiders.contains(&eid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_sizes_follow_the_table() {
        let mut rng = rand::thread_rng();
        // wave 1 on normal: 4 pillagers + 0..=1 bonus, no vindicators
        let p = wave_spawns("pillager", 1, 5, false, 2, &mut rng);
        assert!((4..=5).contains(&p));
        assert_eq!(wave_spawns("evoker", 1, 5, false, 2, &mut rng), 0);
        assert_eq!(wave_spawns("ravager", 3, 5, false, 2, &mut rng), 1);
        let r = Raid::new([0, 64, 0], 3, 2);
        assert_eq!(r.total_waves(), 6);
        assert_eq!(Raid::new([0, 64, 0], 1, 3).total_waves(), 7);
        assert_eq!(ominous_banner().map(|s| s.patch().banner.len()), Some(8));
    }

    #[test]
    fn a_raid_runs_its_waves() {
        let Some((g, _gen, _store, tmp)) = crate::game::endfight::tests::end_game() else { return };
        // the helper builds an End game; raids only run in the overworld,
        // so drive the pieces directly
        let y = g.motion_blocking_height(0, 0);
        let vt = MobType::from_name("villager").unwrap();
        g.spawn_mob_with(vt, [0.5, y as f64, 0.5], true, |_, _| {});
        assert!(g.is_village([3.0, y as f64, 3.0]));
        g.raids.lock().push(Raid::new([0, y, 0], 1, 2));
        g.spawn_raid_wave(0, [0, y, 0]);
        let (n, spawned) = {
            let r = g.raids.lock();
            (r[0].raiders.len(), r[0].spawned)
        };
        assert!(n >= 4, "first wave: {n}");
        assert_eq!(spawned, 1);
        let captains = g.entities.lock().values().filter(|e| matches!(&e.kind, EKind::Mob(m) if m.eq[HEAD].is_some_and(|s| Items::get().name(s.item) == "white_banner"))).count();
        assert_eq!(captains, 1);
        let _ = std::fs::remove_dir_all(tmp);
    }
}
