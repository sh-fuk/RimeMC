//! Type-specific goals (the inner `Goal` classes of vanilla mobs).

use rand::Rng;

use super::goals::{Goal, Sel, TState};
use super::{bp, dist2, reduced, Ctx, Seen, JUMP, LOOK, MOVE, TARGET};
use crate::anticheat::combat::Aabb;
use crate::game::mob::Mob;
use crate::game::mobtick::Act;
use crate::game::projectile::PType;

#[derive(Clone, Debug)]
pub enum Special {
    // zombies / drowned
    AttackTurtleEgg { speed: f64, ticks: i32, pos: Option<[i32; 3]> },
    MoveThroughVillage { speed: f64 },
    DrownedGoToWater { speed: f64, wanted: [f64; 3] },
    DrownedGoToBeach { speed: f64, wanted: [f64; 3] },
    DrownedSwimUp { speed: f64, sea: i32, stuck: bool },
    // animals
    RaidGarden { wanted: Option<[i32; 3]>, can_raid: bool, tries: i32 },
    TurtleLayEgg { ticks: i32, wanted: Option<[i32; 3]> },
    TurtleGoToWater { wanted: Option<[f64; 3]> },
    TurtleGoHome { stuck: bool, ticks: i32 },
    TurtleTravel { stuck: bool },
    RandomStand,
    NonTameTarget { sel: Sel, st: TState, found: Option<i32> },
    TamePanic { speed: f64, inner: Box<Goal> },
    Beg { range: f32, player: Option<i32>, time: i32 },
    FollowMob { speed: f64, stop: f32, area: f32, following: Option<i32>, recalc: i32 },
    EatBerries { speed: f64, wanted: Option<[i32; 3]>, ticks: i32 },
    PandaSneeze,
    PolarBearAttackPlayers { inner: Box<Goal> },
    StriderGoToLava { wanted: Option<[f64; 3]> },
    GoatRam { cooldown: i32, state: u8, ticks: i32, target: Option<i32>, dir: [f64; 3] },
    ArmadilloRoll { scared: i32 },
    SnifferDig { cooldown: i32, ticks: i32 },
    PlayDead { ticks: i32 },
    // water
    FollowFlockLeader { leader: Option<i32>, next: i32 },
    PufferfishPuff,
    SquidRandom,
    SquidFlee { ticks: i32 },
    BreathAir,
    SwimWithPlayer { speed: f64, player: Option<i32> },
    DolphinJump { interval: i32, breached: bool },
    GuardianAttack { ticks: i32, elder: bool },
    // golems
    OfferFlower { tick: i32, villager: Option<i32> },
    // monsters
    EndermanFreeze { target: Option<i32> },
    EndermanLeaveBlock,
    EndermanTakeBlock,
    EndermanLookForPlayer { pending: Option<i32>, aggro: i32, teleport: i32, st: TState },
    SilverfishWakeUp { ticks: i32 },
    SilverfishMerge { dir: Option<u8>, do_merge: bool, inner: Box<Goal> },
    BlazeAttack { step: i32, time: i32, last_seen: i32 },
    GhastFloat { range: i32 },
    GhastLook,
    GhastShoot { charge: i32 },
    GhastTarget { inner: Box<Goal> },
    SlimeFloat,
    SlimeAttack { growl: i32 },
    SlimeRandomDir { yrot: f32, next: i32 },
    SlimeKeepJumping,
    PhantomStrategy { next_sweep: i32 },
    PhantomSweep { cat_scare: i32, sweep: bool },
    PhantomCircle { angle: f32, distance: f32, height: f32, clockwise: f32 },
    PhantomTarget { next_scan: i32 },
    CastingSpell,
    Spell { kind: u8, warmup: i32, next: i32, cast_time: i32, interval: i32, warmup_time: i32 },
    VexCharge,
    VexRandomMove,
    VexCopyOwnerTarget,
    ShulkerAttack { time: i32 },
    ShulkerPeek { time: i32 },
    WitherDoNothing,
    DragonFly { node: [f64; 3], phase_ticks: i32, strafe: i32 },
    WardenRoar { ticks: i32 },
    WardenSonic { cooldown: i32, ticks: i32 },
    WardenSniff { ticks: i32 },
    BreezeShoot { cooldown: i32, ticks: i32 },
    BreezeJump { cooldown: i32 },
    CreakingFreeze,
}

fn target_of(m: &Mob, cx: &Ctx) -> Option<Seen> {
    m.target.and_then(|t| cx.find(t)).cloned()
}

fn alive(t: &Option<Seen>) -> bool {
    t.as_ref().is_some_and(|t| t.health > 0.0)
}

/// `Entity.isEmptyBlock`-ish: nothing that collides.
fn empty(cx: &Ctx, p: [i32; 3]) -> bool {
    !crate::game::physics::has_collision(cx.block(p))
}

impl Special {
    pub fn flags(&self) -> u8 {
        use Special::*;
        match self {
            AttackTurtleEgg { .. } | MoveThroughVillage { .. } | DrownedGoToWater { .. } | DrownedGoToBeach { .. } | DrownedSwimUp { .. } | RaidGarden { .. } | TurtleLayEgg { .. } | TurtleGoToWater { .. } | TurtleGoHome { .. } | TurtleTravel { .. } | EatBerries { .. } | StriderGoToLava { .. } | GoatRam { .. } | SnifferDig { .. } | FollowFlockLeader { .. } | SwimWithPlayer { .. } | DolphinJump { .. } | GhastFloat { .. } | PhantomSweep { .. } | PhantomCircle { .. } | VexCharge | VexRandomMove | BreezeJump { .. } | SquidFlee { .. } | SquidRandom => MOVE,
            TamePanic { .. } => MOVE,
            RandomStand => MOVE | LOOK,
            Beg { .. } => LOOK,
            FollowMob { .. } => MOVE | LOOK,
            NonTameTarget { .. } | PolarBearAttackPlayers { .. } | EndermanLookForPlayer { .. } | GhastTarget { .. } | PhantomTarget { .. } | VexCopyOwnerTarget => TARGET,
            BreathAir => MOVE | LOOK,
            GuardianAttack { .. } | BlazeAttack { .. } => MOVE | LOOK,
            OfferFlower { .. } => MOVE | LOOK,
            EndermanFreeze { .. } => JUMP | MOVE,
            SilverfishMerge { .. } => MOVE,
            GhastLook | SlimeAttack { .. } | SlimeRandomDir { .. } => LOOK,
            SlimeFloat | SlimeKeepJumping => JUMP | MOVE,
            CastingSpell => MOVE | LOOK,
            ShulkerAttack { .. } => MOVE | LOOK,
            WitherDoNothing => MOVE | JUMP | LOOK,
            PlayDead { .. } => MOVE | LOOK | JUMP,
            ArmadilloRoll { .. } => MOVE | LOOK | JUMP,
            CreakingFreeze => MOVE | JUMP | LOOK,
            WardenRoar { .. } | WardenSonic { .. } | WardenSniff { .. } => MOVE | LOOK,
            BreezeShoot { .. } => LOOK,
            DragonFly { .. } => MOVE | LOOK,
            _ => 0,
        }
    }

    pub fn interruptable(&self) -> bool {
        !matches!(self, Special::GoatRam { state: 1.., .. } | Special::WardenRoar { .. } | Special::WardenSonic { .. })
    }

    pub fn every_tick(&self) -> bool {
        use Special::*;
        matches!(self, GuardianAttack { .. } | BlazeAttack { .. } | GhastLook | GhastShoot { .. } | SlimeFloat | SlimeAttack { .. } | CastingSpell | VexCharge | ShulkerAttack { .. } | EndermanFreeze { .. } | SquidFlee { .. } | PhantomSweep { .. } | BreezeShoot { .. } | GoatRam { .. } | WardenSonic { .. } | WardenRoar { .. } | DolphinJump { .. } | BreathAir | TurtleLayEgg { .. })
    }

    pub fn can_use(&mut self, m: &mut Mob, cx: &mut Ctx) -> bool {
        use Special::*;
        match self {
            AttackTurtleEgg { pos, ticks, .. } => {
                if !cx.mob_griefing || cx.rng.gen_range(0..reduced(200).max(1)) != 0 {
                    return false;
                }
                *ticks = 0;
                *pos = find_block(cx, 3, 1, |n| n == "turtle_egg");
                pos.is_some()
            }
            MoveThroughVillage { .. } => false,
            DrownedGoToWater { wanted, .. } => {
                if !cx.bright() || m.mo.in_water || m.target.is_some() {
                    return false;
                }
                match water_pos(m, cx) {
                    Some(p) => {
                        *wanted = p;
                        true
                    }
                    None => false,
                }
            }
            DrownedGoToBeach { wanted, .. } => {
                // Drowned.DrownedGoToBeachGoal: at night, when not searching for prey
                if cx.bright() || !m.mo.in_water || cx.pos[1] < 63.0 - 3.0 || m.target.is_some() || cx.rng.gen_range(0..reduced(40).max(1)) != 0 {
                    return false;
                }
                let p = cx.block_pos();
                for _ in 0..10 {
                    let q = [p[0] + cx.rng.gen_range(-8..=8), p[1] + cx.rng.gen_range(-2..=2), p[2] + cx.rng.gen_range(-8..=8)];
                    if crate::game::physics::has_collision(cx.block([q[0], q[1] - 1, q[2]])) && empty(cx, q) && empty(cx, [q[0], q[1] + 1, q[2]]) && !crate::registry::vstate::is_water(cx.block(q)) {
                        *wanted = [q[0] as f64 + 0.5, q[1] as f64, q[2] as f64 + 0.5];
                        return true;
                    }
                }
                false
            }
            DrownedSwimUp { sea, .. } => !cx.bright() && m.mo.in_water && cx.pos[1] < (*sea - 2) as f64,
            RaidGarden { can_raid, tries, .. } => {
                if *tries <= 0 {
                    if !cx.mob_griefing {
                        return false;
                    }
                    *can_raid = false;
                    *can_raid = m.v.t0 == 0;
                }
                false
            }
            TurtleLayEgg { wanted, .. } => {
                if !m.v.has_egg {
                    return false;
                }
                let h = m.v.home_pos.unwrap_or(cx.block_pos());
                if dist2(cx.pos, [h[0] as f64, h[1] as f64, h[2] as f64]) >= 81.0 {
                    return false;
                }
                *wanted = find_block(cx, 8, 2, |n| n == "sand" || n == "red_sand").map(|p| [p[0], p[1] + 1, p[2]]);
                wanted.is_some()
            }
            TurtleGoToWater { wanted } => {
                if m.is_baby() && !m.mo.in_water || m.mo.in_water || m.v.going_home || m.v.has_egg {
                    return false;
                }
                *wanted = water_pos(m, cx);
                wanted.is_some()
            }
            TurtleGoHome { .. } => {
                if m.is_baby() {
                    return false;
                }
                let Some(h) = m.v.home_pos else { return false };
                if m.v.has_egg {
                    return true;
                }
                if cx.rng.gen_range(0..reduced(700).max(1)) != 0 {
                    return false;
                }
                dist2(cx.pos, [h[0] as f64, h[1] as f64, h[2] as f64]) >= 64.0 * 64.0
            }
            TurtleTravel { .. } => !m.v.going_home && !m.v.has_egg && m.mo.in_water,
            RandomStand => {
                if cx.rng.gen_range(0..reduced(1000).max(1)) == 0 && m.v.standing == 0 && !m.is_baby() {
                    m.v.standing = 20;
                    cx.acts.push(Act::Sound { name: format!("minecraft:entity.{}.angry", m.ty.sound_name()), pos: cx.pos, hostile: false, volume: 1.0, pitch: 1.0 });
                }
                false
            }
            NonTameTarget { sel, st, found } => {
                if m.tame {
                    return false;
                }
                if cx.rng.gen_range(0..reduced(10).max(1)) != 0 {
                    return false;
                }
                let range = m.follow_range();
                let s = *sel;
                let me = &*m;
                let f = me.nearest(cx, range, range, true, st.must_see, &|t| s(me, t));
                *found = f.map(|f| f.eid);
                found.is_some()
            }
            TamePanic { inner, .. } => {
                if m.tame && m.ordered_sit {
                    return false;
                }
                inner.can_use(m, cx)
            }
            Beg { range, player, .. } => {
                let r = *range as f64;
                let wolf_food = |i: i32| cx.game.data.item_in("wolf_food", i) || crate::data::Items::get().name(i) == "bone";
                let found = m.nearest_player(cx, r, false, false, &|s| s.holding_id(wolf_food));
                *player = found.map(|p| p.eid);
                player.is_some()
            }
            FollowMob { area, following, .. } => {
                let a = *area as f64;
                let me_eid = cx.eid;
                let bb = Aabb::entity(cx.pos, m.width(), m.height()).inflate(a, a, a);
                let found = cx.seen.iter().find(|s| s.eid != me_eid && !s.player && !s.invisible && s.ty != "parrot" && s.bbox().intersects(&bb));
                *following = found.map(|s| s.eid);
                following.is_some()
            }
            EatBerries { wanted, ticks, .. } => {
                if m.is_sitting() || cx.rng.gen_range(0..reduced(200).max(1)) != 0 {
                    return false;
                }
                *ticks = 0;
                *wanted = find_block(cx, 12, 1, |n| n == "sweet_berry_bush").filter(|p| {
                    let s = cx.block(*p);
                    s.prop("age").and_then(|a| a.parse::<i32>().ok()).unwrap_or(0) >= 2
                });
                wanted.is_some()
            }
            PandaSneeze => {
                if m.is_baby() && m.v.genes.0 == 4 && m.v.sneeze == 0 && cx.rng.gen_range(0..reduced(500).max(1)) == 0 {
                    m.v.sneeze = 1;
                }
                false
            }
            PolarBearAttackPlayers { inner } => {
                if m.is_baby() {
                    return false;
                }
                if !inner.can_use(m, cx) {
                    return false;
                }
                let me = cx.eid;
                cx.seen.iter().any(|s| s.ty == "polar_bear" && s.baby && s.eid != me && dist2(s.pos, cx.pos) < 64.0)
            }
            StriderGoToLava { wanted } => {
                if m.v.t0 > 0 || cx.block(cx.block_pos()).name() == "lava" {
                    return false;
                }
                *wanted = find_block(cx, 8, 2, |n| n == "lava").map(|p| [p[0] as f64 + 0.5, p[1] as f64 + 1.0, p[2] as f64 + 0.5]);
                wanted.is_some()
            }
            GoatRam { cooldown, state, target, .. } => {
                if *cooldown > 0 {
                    *cooldown -= 1;
                    return false;
                }
                if m.is_baby() {
                    return false;
                }
                let me = &*m;
                let f = me.nearest(cx, 16.0, 4.0, false, true, &|s| {
                    let d = dist2(s.pos, [s.pos[0], s.pos[1], s.pos[2]]);
                    let _ = d;
                    s.ty != "goat"
                });
                let Some(f) = f else { return false };
                let d = dist2(cx.pos, f.pos);
                if !(16.0..=256.0).contains(&d) {
                    return false;
                }
                *target = Some(f.eid);
                *state = 1;
                true
            }
            ArmadilloRoll { scared } => {
                let threat = cx.seen.iter().any(|s| (s.player && s.attackable_player() || matches!(s.ty, "zombie" | "spider" | "cave_spider" | "skeleton" | "husk")) && dist2(s.pos, cx.pos) < 49.0 && (s.player && (s.sprinting || s.in_vehicle) || !s.player));
                if threat || super::goals::should_panic(m) {
                    *scared = 60;
                }
                *scared > 0
            }
            SnifferDig { cooldown, .. } => {
                if *cooldown > 0 {
                    *cooldown -= 1;
                    return false;
                }
                if m.is_baby() || !cx.on_ground || m.mo.in_water {
                    return false;
                }
                let below = cx.block([cx.block_pos()[0], cx.block_pos()[1] - 1, cx.block_pos()[2]]);
                cx.game.data.block_in("sniffer_diggable_block", below.name()) && cx.rng.gen_range(0..200) == 0
            }
            PlayDead { ticks } => {
                if m.mo.in_water && m.living.hurt_time > 0 && m.living.health < m.living.max_health() * 0.5 && *ticks == 0 && cx.rng.gen_range(0..3) == 0 {
                    *ticks = 200;
                }
                *ticks > 0
            }
            FollowFlockLeader { leader, next } => {
                if *next > 0 {
                    *next -= 1;
                    return false;
                }
                *next = reduced(200 + cx.rng.gen_range(0..200) % 20);
                let me = cx.eid;
                let ty = m.ty.name();
                let l = cx.seen.iter().filter(|s| s.ty == ty && s.eid != me && dist2(s.pos, cx.pos) < 64.0).min_by_key(|s| s.eid).map(|s| s.eid);
                if l.is_some_and(|l| l < me) {
                    *leader = l;
                    true
                } else {
                    false
                }
            }
            PufferfishPuff => {
                let me = cx.eid;
                let threat = cx.seen.iter().any(|s| s.eid != me && dist2(s.pos, cx.pos) < 4.0 && !s.spectator() && (s.player && s.gamemode != 1 || matches!(s.ty, "axolotl")) && !s.in_vehicle);
                if threat {
                    if m.size == 0 {
                        m.v.inflate_ticks = 1;
                    }
                    m.v.deflate_timer = 0;
                }
                false
            }
            SquidRandom => true,
            SquidFlee { .. } => {
                let Some((by, _)) = m.living.last_hurt_by_mob else { return false };
                let Some(a) = cx.find(by) else { return false };
                m.mo.in_water && a.health > 0.0 && dist2(a.pos, cx.pos) < 100.0
            }
            BreathAir => m.air < 140,
            SwimWithPlayer { player, .. } => {
                let me = &*m;
                let f = me.nearest_player(cx, 10.0, false, false, &|s| s.swimming);
                *player = f.map(|f| f.eid);
                player.is_some()
            }
            DolphinJump { interval, .. } => {
                if cx.rng.gen_range(0..reduced(*interval).max(1)) != 0 {
                    return false;
                }
                let yaw = m.mo.yaw.to_radians();
                let (sx, sz) = (crate::game::physics::jsign((-yaw.sin()) as f64) as i32, crate::game::physics::jsign((yaw.cos()) as f64) as i32);
                let p = cx.block_pos();
                let steps = [0, 1, 4, 5, 6];
                for s in steps {
                    let q = [p[0] + sx * s, p[1], p[2] + sz * s];
                    if !crate::registry::vstate::is_water(cx.block(q)) || !cx.block([q[0], q[1] + 1, q[2]]).is_air() || !cx.block([q[0], q[1] + 2, q[2]]).is_air() {
                        return false;
                    }
                }
                m.mo.in_water
            }
            GuardianAttack { .. } => alive(&target_of(m, cx)),
            OfferFlower { villager, .. } => {
                if !cx.day || cx.rng.gen_range(0..8000) != 0 {
                    return false;
                }
                let me = &*m;
                *villager = me.nearest(cx, 6.0, 2.0, false, false, &|s| s.ty == "villager").map(|s| s.eid);
                villager.is_some()
            }
            EndermanFreeze { target } => {
                let Some(t) = target_of(m, cx) else { return false };
                if !t.player || dist2(cx.pos, t.pos) > 256.0 {
                    return false;
                }
                *target = Some(t.eid);
                looking_at_me(m, cx, &t)
            }
            EndermanLeaveBlock => m.v.carry.is_some() && cx.mob_griefing && cx.rng.gen_range(0..reduced(2000).max(1)) == 0,
            EndermanTakeBlock => m.v.carry.is_none() && cx.mob_griefing && cx.rng.gen_range(0..reduced(20).max(1)) == 0,
            EndermanLookForPlayer { pending, .. } => {
                if cx.rng.gen_range(0..reduced(10).max(1)) != 0 {
                    return false;
                }
                let range = m.follow_range();
                let mut best: Option<(f64, Seen)> = None;
                let eye = [cx.pos[0], cx.pos[1] + m.eye(), cx.pos[2]];
                for s in cx.seen.iter().filter(|s| s.player && s.attackable_player()) {
                    if dist2(cx.pos, s.pos) > range * range {
                        continue;
                    }
                    let s = s.clone();
                    if !(looking_at_me(m, cx, &s) || m.v.anger_target == Some(s.uuid)) {
                        continue;
                    }
                    let d = dist2(eye, s.pos);
                    if best.as_ref().is_none_or(|b| d < b.0) {
                        best = Some((d, s));
                    }
                }
                *pending = best.map(|b| b.1.eid);
                pending.is_some()
            }
            SilverfishWakeUp { ticks } => *ticks > 0,
            SilverfishMerge { dir, do_merge, inner } => {
                if m.target.is_some() || !m.nav.is_done() {
                    return false;
                }
                if cx.mob_griefing && cx.rng.gen_range(0..reduced(10).max(1)) == 0 {
                    let d = cx.rng.gen_range(0..6) as u8;
                    let p = cx.block_pos();
                    let o = dir_offset(d);
                    let q = [p[0] + o[0], (cx.pos[1] + 0.5).floor() as i32 + o[1], p[2] + o[2]];
                    if infested_of(cx.block(q).name()).is_some() {
                        *dir = Some(d);
                        *do_merge = true;
                        return true;
                    }
                }
                *do_merge = false;
                inner.can_use(m, cx)
            }
            BlazeAttack { .. } => alive(&target_of(m, cx)),
            GhastFloat { .. } => {
                if !m.mc.has_wanted() {
                    return true;
                }
                let d = dist2(m.mc.wanted, cx.pos);
                !(1.0..=3600.0).contains(&d)
            }
            GhastLook => true,
            GhastShoot { .. } => target_of(m, cx).is_some(),
            GhastTarget { inner } => {
                let ok = inner.can_use(m, cx);
                if ok {
                    if let Goal::Nearest { found: Some(f), .. } = inner.as_ref() {
                        if let Some(s) = cx.find(*f) {
                            return (s.pos[1] - cx.pos[1]).abs() <= 4.0;
                        }
                    }
                }
                false
            }
            SlimeFloat => m.mo.in_water || m.mo.in_lava,
            SlimeAttack { .. } => {
                let t = target_of(m, cx);
                alive(&t) && t.as_ref().is_some_and(|t| m.can_target(cx, t, true, 0.0, false, false))
            }
            SlimeRandomDir { .. } => m.target.is_none() && (cx.on_ground || m.mo.in_water || m.mo.in_lava || m.living.has_effect(crate::game::combat::effects::Effect::Levitation)),
            SlimeKeepJumping => true,
            PhantomStrategy { .. } => {
                let t = target_of(m, cx);
                t.as_ref().is_some_and(|t| m.can_target(cx, t, true, 0.0, false, false))
            }
            PhantomSweep { .. } => m.target.is_some() && m.v.phase == 1,
            PhantomCircle { .. } => m.target.is_none() || m.v.phase == 0,
            PhantomTarget { next_scan } => {
                if *next_scan > 0 {
                    *next_scan -= 1;
                    return false;
                }
                *next_scan = reduced(60);
                let bb = Aabb::entity(cx.pos, m.width(), m.height()).inflate(16.0, 64.0, 16.0);
                let mut ps: Vec<Seen> = cx.seen.iter().filter(|s| s.player && s.bbox().intersects(&bb)).cloned().collect();
                ps.sort_by(|a, b| b.pos[1].partial_cmp(&a.pos[1]).unwrap_or(std::cmp::Ordering::Equal));
                for p in ps {
                    if m.can_target(cx, &p, true, 64.0, false, true) {
                        m.target = Some(p.eid);
                        return true;
                    }
                }
                false
            }
            CastingSpell => m.v.spell_ticks > 0,
            Spell { kind, next, .. } => {
                let t = target_of(m, cx);
                if *kind == 3 {
                    // wololo: no target, blue sheep around
                    if m.target.is_some() || m.v.spell_ticks > 0 || m.tick_count < *next || !cx.mob_griefing {
                        return false;
                    }
                    let me = &*m;
                    let sheep = me.nearest(cx, 16.0, 4.0, false, false, &|s| s.ty == "sheep" && s.color == 11);
                    m.v.e0 = sheep.map(|s| s.eid);
                    return m.v.e0.is_some();
                }
                if !alive(&t) || m.v.spell_ticks > 0 || m.tick_count < *next {
                    return false;
                }
                match *kind {
                    1 => {
                        let vexes = cx.seen.iter().filter(|s| s.ty == "vex" && dist2(s.pos, cx.pos) < 256.0).count() as i32;
                        cx.rng.gen_range(0..8) + 1 > vexes
                    }
                    4 => !m.living.has_effect(crate::game::combat::effects::Effect::Invisibility),
                    5 => t.is_some_and(|t| Some(t.eid) != m.v.e0),
                    _ => true,
                }
            }
            VexCharge => {
                let Some(t) = target_of(m, cx) else { return false };
                if m.mc.has_wanted() || cx.rng.gen_range(0..reduced(7).max(1)) != 0 {
                    return false;
                }
                dist2(cx.pos, t.pos) > 4.0
            }
            VexRandomMove => !m.mc.has_wanted() && cx.rng.gen_range(0..reduced(7).max(1)) == 0,
            VexCopyOwnerTarget => {
                let Some(o) = m.v.e0.and_then(|o| cx.find(o)) else { return false };
                let Some(t) = o.target.and_then(|t| cx.find(t)).cloned() else { return false };
                m.can_target(cx, &t, true, 0.0, false, false) && {
                    m.v.t2 = t.eid;
                    true
                }
            }
            ShulkerAttack { .. } => cx.difficulty > 0 && alive(&target_of(m, cx)),
            ShulkerPeek { .. } => m.target.is_none() && cx.rng.gen_range(0..reduced(40).max(1)) == 0,
            WitherDoNothing => m.v.invul > 0,
            DragonFly { .. } => true,
            WardenRoar { ticks } => *ticks > 0,
            WardenSonic { cooldown, .. } => {
                if *cooldown > 0 {
                    *cooldown -= 1;
                    return false;
                }
                let t = target_of(m, cx);
                t.as_ref().is_some_and(|t| {
                    let d = dist2(cx.pos, t.pos);
                    d > 9.0 && d < 225.0 && (t.pos[1] - cx.pos[1]).abs() < 20.0
                }) && { let t = t.unwrap(); m.can_see(cx, &t) }
            }
            WardenSniff { ticks } => {
                if m.target.is_some() || *ticks > 0 {
                    return *ticks > 0;
                }
                if cx.rng.gen_range(0..reduced(200).max(1)) == 0 {
                    *ticks = 84;
                    return true;
                }
                false
            }
            BreezeShoot { cooldown, .. } => {
                if *cooldown > 0 {
                    *cooldown -= 1;
                    return false;
                }
                let t = target_of(m, cx);
                t.as_ref().is_some_and(|t| dist2(cx.pos, t.pos) < 24.0 * 24.0 && dist2(cx.pos, t.pos) > 4.0) && { let t = t.unwrap(); m.can_see(cx, &t) }
            }
            BreezeJump { cooldown } => {
                if *cooldown > 0 {
                    *cooldown -= 1;
                    return false;
                }
                cx.on_ground && m.target.is_some() && cx.rng.gen_range(0..reduced(20).max(1)) == 0
            }
            CreakingFreeze => {
                let watched = cx.seen.iter().any(|s| s.player && s.attackable_player() && dist2(s.pos, cx.pos) < 32.0 * 32.0 && looking_at_me(m, cx, s));
                m.v.can_move = !watched;
                watched
            }
        }
    }

    pub fn can_continue(&mut self, m: &mut Mob, cx: &mut Ctx) -> bool {
        use Special::*;
        match self {
            AttackTurtleEgg { ticks, pos, .. } => *ticks < 1200 && pos.is_some_and(|p| cx.block(p).name() == "turtle_egg"),
            DrownedGoToWater { .. } | DrownedGoToBeach { .. } | TurtleGoToWater { .. } | StriderGoToLava { .. } | GhastFloat { .. } => !m.nav.is_done(),
            DrownedSwimUp { sea, stuck, .. } => cx.pos[1] < (*sea - 1) as f64 && !*stuck && !cx.bright(),
            TurtleLayEgg { ticks, .. } => m.v.has_egg && *ticks < 200,
            TurtleGoHome { stuck, ticks } => {
                let Some(h) = m.v.home_pos else { return false };
                !*stuck && *ticks < 600 && dist2(cx.pos, [h[0] as f64, h[1] as f64, h[2] as f64]) > 49.0
            }
            TurtleTravel { stuck } => !m.nav.is_done() && !*stuck && !m.v.going_home && !m.v.has_egg,
            NonTameTarget { st, .. } => {
                let mut st2 = st.clone();
                let ok = target_continue_simple(m, cx, &mut st2);
                *st = st2;
                ok
            }
            TamePanic { inner, .. } => inner.can_continue(m, cx),
            Beg { range, player, time } => {
                let Some(p) = player.and_then(|p| cx.find(p)) else { return false };
                if p.health <= 0.0 || dist2(cx.pos, p.pos) > (*range * *range) as f64 {
                    return false;
                }
                *time > 0
            }
            FollowMob { following, stop, .. } => following.and_then(|f| cx.find(f)).is_some_and(|f| !m.nav.is_done() && dist2(cx.pos, f.pos) > (*stop * *stop) as f64),
            EatBerries { ticks, wanted, .. } => *ticks < 1200 && wanted.is_some_and(|w| cx.block(w).name() == "sweet_berry_bush"),
            PolarBearAttackPlayers { inner } => inner.can_continue(m, cx),
            GoatRam { state, .. } => *state > 0,
            ArmadilloRoll { scared } => *scared > 0,
            SnifferDig { ticks, .. } => *ticks > 0,
            PlayDead { ticks } => *ticks > 0,
            FollowFlockLeader { leader, .. } => leader.and_then(|l| cx.find(l)).is_some_and(|l| l.health > 0.0 && dist2(l.pos, cx.pos) <= 121.0),
            SquidFlee { ticks } => *ticks <= 100 && self_squid_flee_ok(m, cx),
            BreathAir => m.air < 140 && !m.mo.under_water || m.air < 300 && m.mo.under_water,
            SwimWithPlayer { player, .. } => player.and_then(|p| cx.find(p)).is_some_and(|p| p.swimming && dist2(p.pos, cx.pos) < 256.0),
            DolphinJump { breached, .. } => {
                let vy = cx.vel[1];
                !(vy * vy < 0.03 && m.mo.pitch != 0.0 && m.mo.pitch.abs() < 10.0 && m.mo.in_water) && !(*breached && cx.on_ground)
            }
            GuardianAttack { elder, .. } => {
                let t = target_of(m, cx);
                alive(&t) && (*elder || t.is_some_and(|t| dist2(cx.pos, t.pos) > 9.0))
            }
            OfferFlower { tick, .. } => *tick > 0,
            EndermanFreeze { .. } => self.can_use(m, cx),
            EndermanLookForPlayer { pending, st, .. } => {
                if let Some(p) = pending.and_then(|p| cx.find(p)).cloned() {
                    if !looking_at_me(m, cx, &p) {
                        return false;
                    }
                    m.look_at_seen(&p, 10.0, 10.0);
                    return true;
                }
                if let Some(t) = target_of(m, cx) {
                    if t.player && t.attackable_player() && t.health > 0.0 {
                        return true;
                    }
                }
                target_continue_simple(m, cx, st)
            }
            SilverfishWakeUp { ticks } => *ticks > 0,
            SilverfishMerge { do_merge, inner, .. } => if *do_merge { false } else { inner.can_continue(m, cx) },
            BlazeAttack { .. } => alive(&target_of(m, cx)),
            GhastShoot { .. } => target_of(m, cx).is_some(),
            GhastTarget { inner } => inner.can_continue(m, cx),
            SlimeAttack { growl } => {
                *growl -= 1;
                alive(&target_of(m, cx)) && *growl > 0
            }
            PhantomStrategy { .. } => self.can_use(m, cx),
            PhantomSweep { cat_scare, sweep } => {
                let Some(t) = target_of(m, cx) else { return false };
                if t.health <= 0.0 || (t.player && !t.attackable_player()) {
                    return false;
                }
                if m.v.phase != 1 {
                    return false;
                }
                if m.tick_count > *cat_scare {
                    *cat_scare = m.tick_count + 20;
                    let near_cat = cx.seen.iter().any(|s| s.ty == "cat" && dist2(s.pos, cx.pos) < 256.0);
                    *sweep = !near_cat;
                    if near_cat {
                        for c in cx.seen.iter().filter(|s| s.ty == "cat" && dist2(s.pos, cx.pos) < 256.0) {
                            cx.acts.push(Act::Sound { name: "minecraft:entity.cat.hiss".into(), pos: c.pos, hostile: false, volume: 1.0, pitch: 1.0 });
                        }
                    }
                }
                *sweep
            }
            PhantomTarget { .. } => target_of(m, cx).is_some_and(|t| t.health > 0.0 && t.attackable_player()),
            Spell { warmup, .. } => alive(&target_of(m, cx)) && *warmup > 0 || (m.v.e0.is_some() && *warmup > 0),
            VexCharge => m.mc.has_wanted() && m.aggressive && alive(&target_of(m, cx)),
            VexCopyOwnerTarget => alive(&target_of(m, cx)),
            ShulkerPeek { time } => m.target.is_none() && *time > 0,
            WardenSonic { ticks, .. } => *ticks > 0,
            BreezeShoot { ticks, .. } => *ticks > 0,
            CreakingFreeze => self.can_use(m, cx),
            WitherDoNothing => m.v.invul > 0,
            _ => self.can_use(m, cx),
        }
    }

    pub fn start(&mut self, m: &mut Mob, cx: &mut Ctx) {
        use Special::*;
        match self {
            DrownedGoToWater { speed, wanted } | DrownedGoToBeach { speed, wanted } => {
                m.move_to(cx, *wanted, *speed);
            }
            DrownedSwimUp { .. } => {
                m.v.flags |= 4; // searching for land
                *self_stuck(self) = false;
            }
            TurtleLayEgg { ticks, .. } => *ticks = 0,
            TurtleGoToWater { wanted } => {
                if let Some(w) = *wanted {
                    m.move_to(cx, w, 1.0);
                }
            }
            TurtleGoHome { stuck, ticks } => {
                m.v.going_home = true;
                *stuck = false;
                *ticks = 0;
            }
            TurtleTravel { stuck } => {
                *stuck = false;
                let p = cx.block_pos();
                let t = [p[0] + cx.rng.gen_range(-16..=16), p[1] + cx.rng.gen_range(-3..=3), p[2] + cx.rng.gen_range(-16..=16)];
                m.move_to(cx, [t[0] as f64 + 0.5, t[1] as f64, t[2] as f64 + 0.5], 1.0);
            }
            NonTameTarget { found, .. } => m.target = *found,
            TamePanic { inner, .. } => inner.start(m, cx),
            Beg { time, .. } => {
                m.v.interested = true;
                *time = reduced(40 + cx.rng.gen_range(0..40));
            }
            FollowMob { recalc, .. } => *recalc = 0,
            EatBerries { wanted, speed, .. } => {
                if let Some(w) = *wanted {
                    m.move_to_reach(cx, [w[0] as f64 + 0.5, w[1] as f64, w[2] as f64 + 0.5], 1, *speed);
                }
            }
            PolarBearAttackPlayers { inner } => inner.start(m, cx),
            StriderGoToLava { wanted } => {
                if let Some(w) = *wanted {
                    m.move_to(cx, w, 1.0);
                }
            }
            GoatRam { ticks, .. } => {
                *ticks = 0;
                m.nav.stop();
            }
            ArmadilloRoll { .. } => {
                m.nav.stop();
                m.v.t2 = 1;
                cx.acts.push(Act::Sound { name: "minecraft:entity.armadillo.roll".into(), pos: cx.pos, hostile: false, volume: 1.0, pitch: 1.0 });
            }
            SnifferDig { ticks, .. } => {
                *ticks = 120;
                m.nav.stop();
                m.v.t2 = 1;
            }
            PlayDead { .. } => {
                m.nav.stop();
                m.v.t2 = 1;
            }
            FollowFlockLeader { .. } => {}
            SquidFlee { ticks } => *ticks = 0,
            SwimWithPlayer { player, speed } => {
                if let Some(p) = player.and_then(|p| cx.find(p)).cloned() {
                    m.move_to_entity(cx, p.pos, *speed);
                    cx.acts.push(Act::Effect { target: p.eid, effect: "dolphins_grace", duration: 100, amp: 0 });
                }
            }
            DolphinJump { breached, .. } => {
                let yaw = m.mo.yaw.to_radians();
                cx.vel = [cx.vel[0] + (-yaw.sin() as f64) * 0.6, 0.7, cx.vel[2] + (yaw.cos() as f64) * 0.6];
                m.nav.stop();
                *breached = false;
            }
            GuardianAttack { ticks, .. } => {
                *ticks = -10;
                m.nav.stop();
                if let Some(t) = target_of(m, cx) {
                    m.look_at_seen(&t, 90.0, 90.0);
                }
            }
            OfferFlower { tick, .. } => {
                *tick = reduced(400);
                m.v.offer_flower = 400;
                m.event.push(11);
            }
            EndermanFreeze { .. } => m.nav.stop(),
            EndermanLookForPlayer { aggro, teleport, .. } => {
                *aggro = reduced(5);
                *teleport = 0;
                m.v.stared = true;
            }
            SilverfishMerge { do_merge, inner, .. } => {
                if !*do_merge {
                    inner.start(m, cx);
                } else {
                    let d = match self {
                        SilverfishMerge { dir, .. } => *dir,
                        _ => None,
                    };
                    if let Some(d) = d {
                        let p = cx.block_pos();
                        let o = dir_offset(d);
                        let q = [p[0] + o[0], (cx.pos[1] + 0.5).floor() as i32 + o[1], p[2] + o[2]];
                        if let Some(inf) = infested_of(cx.block(q).name()) {
                            cx.acts.push(Act::SetBlock { pos: q, state: crate::registry::BlockState::by_name(inf), drops: false });
                            cx.acts.push(Act::Remove { eid: cx.eid });
                        }
                    }
                }
            }
            BlazeAttack { step, .. } => *step = 0,
            GhastFloat { range } => {
                let r = *range as f64;
                let w = [cx.pos[0] + (cx.rng.r#gen::<f64>() * 2.0 - 1.0) * 16.0f64.min(r), cx.pos[1] + (cx.rng.r#gen::<f64>() * 2.0 - 1.0) * 16.0f64.min(r), cx.pos[2] + (cx.rng.r#gen::<f64>() * 2.0 - 1.0) * 16.0f64.min(r)];
                m.mc.set_wanted(w, 1.0);
            }
            GhastShoot { charge } => *charge = 0,
            GhastTarget { inner } => inner.start(m, cx),
            SlimeAttack { growl } => *growl = reduced(300),
            PhantomStrategy { next_sweep } => {
                *next_sweep = reduced(10);
                m.v.phase = 0;
                phantom_anchor_above_target(m, cx);
            }
            PhantomCircle { angle, distance, height, clockwise } => {
                *distance = 5.0 + cx.rng.r#gen::<f32>() * 10.0;
                *height = -4.0 + cx.rng.r#gen::<f32>() * 9.0;
                *clockwise = if cx.rng.r#gen::<bool>() { 1.0 } else { -1.0 };
                phantom_select_next(m, cx, angle, *distance, *height, *clockwise);
            }
            Spell { kind, warmup, next, cast_time, interval, warmup_time } => {
                *warmup = reduced(*warmup_time);
                m.v.spell_ticks = *cast_time;
                *next = m.tick_count + *interval;
                let sound = match *kind {
                    1 => "minecraft:entity.evoker.prepare_summon",
                    2 => "minecraft:entity.evoker.prepare_attack",
                    3 => "minecraft:entity.evoker.prepare_wololo",
                    4 => "minecraft:entity.illusioner.prepare_mirror",
                    _ => "minecraft:entity.illusioner.prepare_blindness",
                };
                cx.acts.push(Act::Sound { name: sound.into(), pos: cx.pos, hostile: true, volume: 1.0, pitch: 1.0 });
                m.v.spell = *kind;
            }
            CastingSpell => m.nav.stop(),
            VexCharge => {
                if let Some(t) = target_of(m, cx) {
                    m.mc.set_wanted(t.eye_pos(), 1.0);
                }
                m.aggressive = true;
                cx.acts.push(Act::Sound { name: "minecraft:entity.vex.charge".into(), pos: cx.pos, hostile: true, volume: 1.0, pitch: 1.0 });
            }
            VexCopyOwnerTarget => m.target = Some(m.v.t2),
            ShulkerAttack { time } => {
                *time = 20;
                m.v.peek = 100;
            }
            ShulkerPeek { time } => {
                *time = 20 * (1 + cx.rng.gen_range(0..3));
                m.v.peek = 30;
            }
            WardenSonic { ticks, .. } => {
                *ticks = 60;
                m.nav.stop();
                cx.acts.push(Act::Sound { name: "minecraft:entity.warden.sonic_charge".into(), pos: cx.pos, hostile: true, volume: 3.0, pitch: 1.0 });
            }
            WardenSniff { .. } => {
                m.v.sniff = 84;
                m.nav.stop();
            }
            BreezeShoot { ticks, .. } => {
                *ticks = 23;
                cx.acts.push(Act::Sound { name: "minecraft:entity.breeze.inhale".into(), pos: cx.pos, hostile: true, volume: 1.0, pitch: 1.0 });
            }
            BreezeJump { cooldown } => {
                let to = target_of(m, cx).map(|t| t.pos).unwrap_or(cx.pos);
                let d = [to[0] - cx.pos[0], 0.0, to[2] - cx.pos[2]];
                let l = (d[0] * d[0] + d[2] * d[2]).sqrt().max(1e-3);
                let k = (l / 8.0).min(1.0) * 0.6;
                cx.vel = [d[0] / l * k, 0.9 + cx.rng.r#gen::<f64>() * 0.3, d[2] / l * k];
                *cooldown = 10 + cx.rng.gen_range(0..30);
                cx.acts.push(Act::Sound { name: "minecraft:entity.breeze.jump".into(), pos: cx.pos, hostile: true, volume: 1.0, pitch: 1.0 });
            }
            CreakingFreeze => m.nav.stop(),
            WitherDoNothing => m.nav.stop(),
            _ => {}
        }
    }

    pub fn stop(&mut self, m: &mut Mob, cx: &mut Ctx) {
        use Special::*;
        match self {
            DrownedSwimUp { .. } => m.v.flags &= !4,
            TurtleGoHome { .. } => m.v.going_home = false,
            NonTameTarget { st, .. } => {
                m.target = None;
                st.target_mob = None;
            }
            TamePanic { inner, .. } => inner.stop(m, cx),
            Beg { player, .. } => {
                m.v.interested = false;
                *player = None;
            }
            FollowMob { following, .. } => {
                *following = None;
                m.nav.stop();
            }
            PolarBearAttackPlayers { inner } => inner.stop(m, cx),
            GoatRam { cooldown, state, target, .. } => {
                *cooldown = 600 + cx.rng.gen_range(0..5400);
                *state = 0;
                *target = None;
            }
            ArmadilloRoll { .. } => {
                m.v.t2 = 0;
                cx.acts.push(Act::Sound { name: "minecraft:entity.armadillo.unroll_finish".into(), pos: cx.pos, hostile: false, volume: 1.0, pitch: 1.0 });
            }
            SnifferDig { cooldown, .. } => {
                *cooldown = 9600;
                m.v.t2 = 0;
            }
            PlayDead { .. } => m.v.t2 = 0,
            FollowFlockLeader { leader, .. } => *leader = None,
            GuardianAttack { .. } => {
                m.v.beam_target = 0;
                m.target = None;
            }
            OfferFlower { villager, .. } => {
                *villager = None;
                m.v.offer_flower = 0;
                m.event.push(34);
            }
            EndermanLookForPlayer { pending, .. } => {
                *pending = None;
                m.v.stared = false;
            }
            SilverfishMerge { inner, do_merge, .. } => {
                if !*do_merge {
                    inner.stop(m, cx);
                }
            }
            BlazeAttack { last_seen, .. } => {
                m.v.flags &= !1;
                *last_seen = 0;
            }
            GhastShoot { .. } => m.v.charge = 0,
            GhastTarget { inner } => inner.stop(m, cx),
            PhantomStrategy { .. } => {
                let a = m.v.anchor;
                let top = cx.game.world.surface_y(a[0], a[2]).unwrap_or(a[1]);
                m.v.anchor = [a[0], top + 10 + cx.rng.gen_range(0..20), a[2]];
            }
            PhantomSweep { .. } => {
                m.target = None;
                m.v.phase = 0;
            }
            PhantomTarget { .. } => m.target = None,
            CastingSpell => m.v.spell = 0,
            VexCharge => m.aggressive = false,
            VexCopyOwnerTarget => m.target = None,
            ShulkerAttack { .. } => m.v.peek = 0,
            ShulkerPeek { .. } => {
                if m.target.is_none() {
                    m.v.peek = 0;
                }
            }
            WardenSonic { cooldown, .. } => *cooldown = 40,
            WardenSniff { .. } => m.v.sniff = 0,
            BreezeShoot { cooldown, .. } => *cooldown = 20 + cx.rng.gen_range(0..20),
            CreakingFreeze => m.v.can_move = true,
            _ => {}
        }
    }

    pub fn tick(&mut self, m: &mut Mob, cx: &mut Ctx) {
        use Special::*;
        match self {
            AttackTurtleEgg { speed, ticks, pos } => {
                *ticks += 1;
                let Some(p) = *pos else { return };
                let c = [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5];
                if dist2(cx.pos, c) > 1.5 * 1.5 {
                    if *ticks % 40 == 0 {
                        m.move_to(cx, c, *speed);
                    }
                } else if cx.rng.gen_range(0..30) == 0 {
                    cx.acts.push(Act::Sound { name: "minecraft:entity.zombie.destroy_egg".into(), pos: c, hostile: true, volume: 0.7, pitch: 0.9 + cx.rng.r#gen::<f32>() * 0.2 });
                    if *ticks > 60 {
                        cx.acts.push(Act::Destroy { pos: p, drops: false });
                        cx.acts.push(Act::Sound { name: "minecraft:entity.turtle.egg_break".into(), pos: c, hostile: true, volume: 0.7, pitch: 0.9 + cx.rng.r#gen::<f32>() * 0.2 });
                        *pos = None;
                    }
                }
            }
            DrownedSwimUp { speed, sea, stuck } => {
                if cx.pos[1] < (*sea - 1) as f64 && (m.nav.is_done() || m.nav.target().is_some_and(|t| (t[1] as f64) < cx.pos[1] + 1.0)) {
                    let dir = [0.0, 1.0, 0.0];
                    match m.random_pos_dir(cx, 4, 8, dir, std::f64::consts::FRAC_PI_2) {
                        Some(p) => {
                            m.move_to(cx, p, *speed);
                        }
                        None => *stuck = true,
                    }
                }
            }
            TurtleLayEgg { ticks, wanted } => {
                let Some(w) = *wanted else { return };
                let c = [w[0] as f64 + 0.5, w[1] as f64, w[2] as f64 + 0.5];
                if dist2(cx.pos, c) > 1.5 {
                    if *ticks % 40 == 0 {
                        m.move_to(cx, c, 1.0);
                    }
                } else {
                    m.v.laying = m.v.laying.max(1);
                    if m.v.laying > reduced(200) {
                        let n = cx.rng.gen_range(1..=4);
                        cx.acts.push(Act::SetBlock { pos: w, state: crate::registry::BlockState::by_name("turtle_egg").with("eggs", &n.to_string()), drops: false });
                        cx.acts.push(Act::Sound { name: "minecraft:entity.turtle.lay_egg".into(), pos: cx.pos, hostile: false, volume: 0.3, pitch: 0.9 + cx.rng.r#gen::<f32>() * 0.2 });
                        m.v.has_egg = false;
                        m.v.laying = 0;
                        m.love = 600;
                    }
                    m.v.laying += 1;
                }
                *ticks += 1;
            }
            TurtleGoHome { stuck, ticks } => {
                let Some(h) = m.v.home_pos else { return };
                let c = [h[0] as f64 + 0.5, h[1] as f64, h[2] as f64 + 0.5];
                *ticks += 1;
                if m.nav.is_done() {
                    let dir = [c[0] - cx.pos[0], 0.0, c[2] - cx.pos[2]];
                    match m.random_pos_dir(cx, 16, 3, dir, std::f64::consts::PI / 10.0) {
                        Some(p) => {
                            m.move_to(cx, p, 1.0);
                        }
                        None => *stuck = true,
                    }
                }
            }
            TurtleTravel { stuck } => {
                if m.nav.is_done() {
                    *stuck = true;
                }
            }
            TamePanic { inner, .. } => inner.tick(m, cx),
            Beg { player, time, .. } => {
                if let Some(p) = player.and_then(|p| cx.find(p)).cloned() {
                    m.look_at(p.eye_pos(), 10.0, m.ty.max_head_x());
                }
                *time -= 1;
            }
            FollowMob { speed, stop, following, recalc, .. } => {
                let Some(f) = following.and_then(|f| cx.find(f)).cloned() else { return };
                m.look_at_seen(&f, 10.0, m.ty.max_head_x());
                *recalc -= 1;
                if *recalc <= 0 {
                    *recalc = reduced(10);
                    if dist2(cx.pos, f.pos) > (*stop * *stop) as f64 {
                        m.move_to_entity(cx, f.pos, *speed);
                    } else {
                        m.nav.stop();
                    }
                }
            }
            EatBerries { speed, wanted, ticks } => {
                *ticks += 1;
                let Some(w) = *wanted else { return };
                let c = [w[0] as f64 + 0.5, w[1] as f64, w[2] as f64 + 0.5];
                if dist2(cx.pos, c) > 2.0 * 2.0 {
                    if *ticks % 40 == 0 {
                        m.move_to_reach(cx, c, 1, *speed);
                    }
                } else if *ticks > 40 {
                    let s = cx.block(w);
                    let age = s.prop("age").and_then(|a| a.parse::<i32>().ok()).unwrap_or(0);
                    if age >= 2 {
                        let count = 1 + cx.rng.gen_range(0..2) + if age == 3 { 1 } else { 0 };
                        if let Some(id) = crate::data::Items::get().id("sweet_berries") {
                            if m.eq[crate::game::combat::living::MAINHAND].is_none() {
                                m.eq[crate::game::combat::living::MAINHAND] = Some(crate::data::Stack::new(id, 1));
                                if count > 1 {
                                    cx.acts.push(Act::Drop { pos: c, stack: crate::data::Stack::new(id, (count - 1) as u8) });
                                }
                            } else {
                                cx.acts.push(Act::Drop { pos: c, stack: crate::data::Stack::new(id, count as u8) });
                            }
                        }
                        cx.acts.push(Act::Sound { name: "minecraft:block.sweet_berry_bush.pick_berries".into(), pos: c, hostile: false, volume: 1.0, pitch: 1.0 });
                        cx.acts.push(Act::SetBlock { pos: w, state: s.with("age", "1"), drops: false });
                    }
                    *wanted = None;
                }
            }
            PolarBearAttackPlayers { inner } => inner.tick(m, cx),
            GoatRam { state, ticks, target, dir, .. } => {
                *ticks += 1;
                match *state {
                    1 => {
                        if let Some(t) = target.and_then(|t| cx.find(t)).cloned() {
                            m.look_at_seen(&t, 30.0, 30.0);
                            if *ticks == 1 {
                                cx.acts.push(Act::Sound { name: format!("minecraft:entity.goat.{}prepare_ram", if m.v.screaming { "screaming." } else { "" }), pos: cx.pos, hostile: false, volume: 1.0, pitch: 1.0 });
                            }
                            if *ticks >= 40 {
                                let d = [t.pos[0] - cx.pos[0], 0.0, t.pos[2] - cx.pos[2]];
                                let l = (d[0] * d[0] + d[2] * d[2]).sqrt().max(1e-3);
                                *dir = [d[0] / l, 0.0, d[2] / l];
                                *state = 2;
                                *ticks = 0;
                            }
                        } else {
                            *state = 0;
                        }
                    }
                    2 => {
                        m.mc.set_wanted([cx.pos[0] + dir[0] * 3.0, cx.pos[1], cx.pos[2] + dir[2] * 3.0], 3.0);
                        let me_box = Aabb::entity(cx.pos, m.width(), m.height());
                        let hit = cx.seen.iter().find(|s| s.eid != cx.eid && s.bbox().intersects(&me_box) && !s.spectator()).cloned();
                        if let Some(h) = hit {
                            let dmg = if m.is_baby() { 1.0 } else { 2.0 };
                            cx.acts.push(Act::Hurt { target: h.eid, kind: "mob_attack", by: Some(cx.eid), amount: dmg });
                            let speed = 2.5 * (1.0 + 0.0);
                            cx.acts.push(Act::Knock { target: h.eid, power: speed, xd: -dir[0], zd: -dir[2] });
                            cx.acts.push(Act::Sound { name: format!("minecraft:entity.goat.{}ram_impact", if m.v.screaming { "screaming." } else { "" }), pos: cx.pos, hostile: false, volume: 1.0, pitch: 1.0 });
                            *state = 0;
                        } else if m.mo.horizontal_collision || *ticks > 40 {
                            *state = 0;
                        }
                    }
                    _ => {}
                }
            }
            ArmadilloRoll { scared } => {
                *scared -= 1;
            }
            SnifferDig { ticks, .. } => {
                *ticks -= 1;
                if *ticks == 0 {
                    let seed = if cx.rng.r#gen::<bool>() { "torchflower_seeds" } else { "pitcher_pod" };
                    if let Some(id) = crate::data::Items::get().id(seed) {
                        cx.acts.push(Act::Drop { pos: cx.pos, stack: crate::data::Stack::new(id, 1) });
                    }
                    cx.acts.push(Act::Sound { name: "minecraft:entity.sniffer.drop_seed".into(), pos: cx.pos, hostile: false, volume: 1.0, pitch: 1.0 });
                }
            }
            PlayDead { ticks } => {
                *ticks -= 1;
                if *ticks == 199 {
                    cx.acts.push(Act::Effect { target: cx.eid, effect: "regeneration", duration: 200, amp: 0 });
                }
            }
            FollowFlockLeader { leader, .. } => {
                if let Some(l) = leader.and_then(|l| cx.find(l)).cloned() {
                    if m.nav.is_done() || cx.rng.gen_range(0..10) == 0 {
                        m.move_to_entity(cx, l.pos, 1.0);
                    }
                }
            }
            SquidRandom => {
                if m.no_action > 100 {
                    m.v.p0 = Some([0.0; 3]);
                } else if cx.rng.gen_range(0..reduced(50).max(1)) == 0 || !m.mo.in_water || m.v.p0.is_none() {
                    let a = cx.rng.r#gen::<f32>() * std::f32::consts::TAU;
                    m.v.p0 = Some([(a.cos() * 0.2) as f64, (-0.1 + cx.rng.r#gen::<f32>() * 0.2) as f64, (a.sin() * 0.2) as f64]);
                }
            }
            SquidFlee { ticks } => {
                *ticks += 1;
                let Some((by, _)) = m.living.last_hurt_by_mob else { return };
                let Some(a) = cx.find(by).cloned() else { return };
                let mut d = [cx.pos[0] - a.pos[0], cx.pos[1] - a.pos[1], cx.pos[2] - a.pos[2]];
                let q = bp([cx.pos[0] + d[0], cx.pos[1] + d[1], cx.pos[2] + d[2]]);
                let st = cx.block(q);
                if crate::registry::vstate::is_water(st) || st.is_air() {
                    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                    if len > 0.0 {
                        for v in &mut d {
                            *v /= len;
                        }
                        let mut scale = 3.0;
                        if len > 5.0 {
                            scale -= (len - 5.0) / 5.0;
                        }
                        if scale > 0.0 {
                            for v in &mut d {
                                *v *= scale;
                            }
                        }
                    }
                    if st.is_air() {
                        d[1] = 0.0;
                    }
                    m.v.p0 = Some([d[0] / 20.0, d[1] / 20.0, d[2] / 20.0]);
                }
            }
            BreathAir => {
                // find air above and swim to it
                let p = cx.block_pos();
                let mut target = None;
                for y in 1..16 {
                    let q = [p[0], p[1] + y, p[2]];
                    if cx.block(q).is_air() {
                        target = Some([q[0] as f64 + 0.5, q[1] as f64, q[2] as f64 + 0.5]);
                        break;
                    }
                }
                if let Some(t) = target {
                    m.move_to(cx, t, 1.0);
                }
                cx.vel[1] += 0.002;
            }
            SwimWithPlayer { player, speed } => {
                if let Some(p) = player.and_then(|p| cx.find(p)).cloned() {
                    m.look_at_seen(&p, m.ty.max_head_y() + 20.0, m.ty.max_head_x());
                    if dist2(cx.pos, p.pos) < 6.25 {
                        m.nav.stop();
                    } else {
                        m.move_to_entity(cx, p.pos, *speed);
                    }
                    if p.swimming && cx.rng.gen_range(0..6) == 0 {
                        cx.acts.push(Act::Effect { target: p.eid, effect: "dolphins_grace", duration: 100, amp: 0 });
                    }
                }
            }
            DolphinJump { breached, .. } => {
                let out = !m.mo.in_water;
                if out && !*breached {
                    *breached = true;
                }
                let v = cx.vel;
                if v[1] * v[1] < 0.03 && m.mo.pitch != 0.0 {
                    m.mo.pitch = super::control::rotlerp(m.mo.pitch, 0.0, 0.2);
                } else if v[0] * v[0] + v[2] * v[2] > 1e-7 {
                    let h = (v[0] * v[0] + v[2] * v[2]).sqrt();
                    let ang = (crate::game::physics::jsign(v[1]) * (v[1] / h).abs().atan()).to_degrees() as f32;
                    m.mo.pitch = ang;
                }
            }
            GuardianAttack { ticks, elder } => {
                let Some(t) = target_of(m, cx) else { return };
                m.nav.stop();
                m.look_at_seen(&t, 90.0, 90.0);
                if !m.can_see(cx, &t) {
                    m.target = None;
                    return;
                }
                *ticks += 1;
                if *ticks == 0 {
                    m.v.beam_target = t.eid;
                    m.event.push(21);
                } else if *ticks >= 80 {
                    let mut dmg = 1.0;
                    if cx.difficulty == 3 {
                        dmg += 2.0;
                    }
                    if *elder {
                        dmg += 2.0;
                    }
                    cx.acts.push(Act::Hurt { target: t.eid, kind: "indirect_magic", by: Some(cx.eid), amount: dmg });
                    cx.acts.push(Act::MobAttack { mob: cx.eid, target: t.eid });
                    m.target = None;
                }
                m.v.beam_ticks = *ticks;
            }
            OfferFlower { tick, villager } => {
                if let Some(v) = villager.and_then(|v| cx.find(v)).cloned() {
                    m.look_at_seen(&v, 30.0, 30.0);
                }
                *tick -= 1;
            }
            EndermanFreeze { target } => {
                if let Some(t) = target.and_then(|t| cx.find(t)).cloned() {
                    m.look_at(t.eye_pos(), 10.0, 10.0);
                }
            }
            EndermanLeaveBlock => {
                let x = (cx.pos[0] - 1.0 + cx.rng.r#gen::<f64>() * 2.0).floor() as i32;
                let y = (cx.pos[1] + cx.rng.r#gen::<f64>() * 2.0).floor() as i32;
                let z = (cx.pos[2] - 1.0 + cx.rng.r#gen::<f64>() * 2.0).floor() as i32;
                let p = [x, y, z];
                let Some(carried) = m.v.carry else { return };
                let at = cx.block(p);
                let below = cx.block([x, y - 1, z]);
                let free = !cx.seen.iter().any(|s| s.bbox().intersects(&Aabb { min: [x as f64, y as f64, z as f64], max: [x as f64 + 1.0, y as f64 + 1.0, z as f64 + 1.0] }));
                if at.is_air() && !below.is_air() && below.name() != "bedrock" && super::path::solid_render(below) && free {
                    cx.acts.push(Act::SetBlock { pos: p, state: carried, drops: false });
                    m.v.carry = None;
                }
            }
            EndermanTakeBlock => {
                let x = (cx.pos[0] - 2.0 + cx.rng.r#gen::<f64>() * 4.0).floor() as i32;
                let y = (cx.pos[1] + cx.rng.r#gen::<f64>() * 3.0).floor() as i32;
                let z = (cx.pos[2] - 2.0 + cx.rng.r#gen::<f64>() * 4.0).floor() as i32;
                let p = [x, y, z];
                let s = cx.block(p);
                if !cx.game.data.block_in("enderman_holdable", s.name()) {
                    return;
                }
                let from = [cx.pos[0].floor() + 0.5, y as f64 + 0.5, cx.pos[2].floor() + 0.5];
                let to = [x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5];
                let hit = crate::game::physics::clip(&super::control::World(cx.get), from, to);
                if hit.is_some_and(|h| h.block != p) {
                    return;
                }
                cx.acts.push(Act::SetBlock { pos: p, state: crate::registry::blocks::AIR, drops: false });
                m.v.carry = Some(s.default_state());
            }
            EndermanLookForPlayer { pending, aggro, teleport, st } => {
                if m.target.is_none() {
                    if let Some(p) = *pending {
                        *aggro -= 1;
                        if *aggro <= 0 {
                            m.target = Some(p);
                            st.target_mob = Some(p);
                            *pending = None;
                            m.v.creepy = true;
                        }
                    }
                } else if let Some(t) = target_of(m, cx) {
                    if t.player && looking_at_me(m, cx, &t) {
                        if dist2(cx.pos, t.pos) < 16.0 {
                            super::custom::enderman_teleport(m, cx);
                        }
                        *teleport = 0;
                    } else if dist2(cx.pos, t.pos) > 256.0 {
                        let tt = *teleport;
                        *teleport += 1;
                        if tt >= reduced(30) && super::custom::enderman_teleport_towards(m, cx, t.pos) {
                            *teleport = 0;
                        }
                    }
                }
            }
            SilverfishWakeUp { ticks } => {
                *ticks -= 1;
                if *ticks <= 0 && cx.mob_griefing {
                    let p = cx.block_pos();
                    'done: for y in 0..=5 {
                        for dy in [y, -y] {
                            for x in 0..=10 {
                                for dx in [x, -x] {
                                    for z in 0..=10 {
                                        for dz in [z, -z] {
                                            let q = [p[0] + dx, p[1] + dy, p[2] + dz];
                                            let s = cx.block(q);
                                            if let Some(host) = s.name().strip_prefix("infested_") {
                                                cx.acts.push(Act::DestroyInfested { pos: q, host: host.to_string() });
                                                if cx.rng.r#gen::<bool>() {
                                                    break 'done;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            SilverfishMerge { inner, do_merge, .. } => {
                if !*do_merge {
                    inner.tick(m, cx);
                }
            }
            BlazeAttack { step, time, last_seen } => {
                *time -= 1;
                let Some(t) = target_of(m, cx) else { return };
                let los = m.can_see(cx, &t);
                if los {
                    *last_seen = 0;
                } else {
                    *last_seen += 1;
                }
                let d = dist2(cx.pos, t.pos);
                if d < 4.0 {
                    if !los {
                        return;
                    }
                    if *time <= 0 {
                        *time = 20;
                        cx.acts.push(Act::MobAttack { mob: cx.eid, target: t.eid });
                    }
                    m.mc.set_wanted(t.pos, 1.0);
                } else if d < m.follow_range().powi(2) && los {
                    let xd = t.pos[0] - cx.pos[0];
                    let yd = t.pos[1] + t.height * 0.5 - (cx.pos[1] + m.height() * 0.5);
                    let zd = t.pos[2] - cx.pos[2];
                    if *time <= 0 {
                        *step += 1;
                        if *step == 1 {
                            *time = 60;
                            m.v.flags |= 1;
                        } else if *step <= 4 {
                            *time = 6;
                        } else {
                            *time = 100;
                            *step = 0;
                            m.v.flags &= !1;
                        }
                        if *step > 1 {
                            let sq = d.sqrt().sqrt() * 0.5;
                            cx.acts.push(Act::LevelEvent { ev: 1018, pos: cx.block_pos(), data: 0 });
                            let dir = [triangle(&mut cx.rng, xd, 2.297 * sq), yd, triangle(&mut cx.rng, zd, 2.297 * sq)];
                            let l = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt().max(1e-7);
                            cx.acts.push(Act::Fire { ty: PType::SmallFireball, owner: cx.eid, from: [cx.pos[0], cx.pos[1] + m.height() * 0.5 + 0.5, cx.pos[2]], dir: [dir[0] / l, dir[1] / l, dir[2] / l], target: None });
                        }
                    }
                    m.look_at_seen(&t, 10.0, 10.0);
                } else if *last_seen < 5 {
                    m.mc.set_wanted(t.pos, 1.0);
                }
            }
            GhastLook => {
                if let Some(t) = target_of(m, cx) {
                    if dist2(cx.pos, t.pos) < 4096.0 {
                        let (xd, zd) = (t.pos[0] - cx.pos[0], t.pos[2] - cx.pos[2]);
                        m.mo.yaw = -(xd.atan2(zd).to_degrees() as f32);
                        m.mo.body_yaw = m.mo.yaw;
                    }
                } else {
                    let v = cx.vel;
                    m.mo.yaw = -(v[0].atan2(v[2]).to_degrees() as f32);
                    m.mo.body_yaw = m.mo.yaw;
                }
            }
            GhastShoot { charge } => {
                let Some(t) = target_of(m, cx) else { return };
                if dist2(cx.pos, t.pos) < 4096.0 && m.can_see(cx, &t) {
                    *charge += 1;
                    if *charge == 10 {
                        cx.acts.push(Act::LevelEvent { ev: 1015, pos: cx.block_pos(), data: 0 });
                    }
                    if *charge == 20 {
                        let view = super::control::view(m.mo.pitch, m.mo.yaw);
                        let from = [cx.pos[0] + view[0] * 4.0, cx.pos[1] + m.height() * 0.5 + 0.5, cx.pos[2] + view[2] * 4.0];
                        let d = [t.pos[0] - (cx.pos[0] + view[0] * 4.0), t.pos[1] + t.height * 0.5 - (0.5 + cx.pos[1] + m.height() * 0.5), t.pos[2] - (cx.pos[2] + view[2] * 4.0)];
                        let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-7);
                        cx.acts.push(Act::LevelEvent { ev: 1016, pos: cx.block_pos(), data: 0 });
                        cx.acts.push(Act::Fire { ty: PType::LargeFireball, owner: cx.eid, from, dir: [d[0] / l, d[1] / l, d[2] / l], target: None });
                        *charge = -40;
                    }
                } else if *charge > 0 {
                    *charge -= 1;
                }
                m.v.charge = *charge;
            }
            GhastTarget { inner } => inner.tick(m, cx),
            SlimeFloat => {
                if cx.rng.r#gen::<f32>() < 0.8 {
                    m.mo.jump_req = true;
                }
                super::custom::slime_wanted(m, 1.2);
            }
            SlimeAttack { .. } => {
                if let Some(t) = target_of(m, cx) {
                    m.turn_to(cx, t.pos, 10.0, 10.0);
                }
                let yaw = m.mo.yaw;
                super::custom::slime_direction(m, yaw, deals_damage(m));
            }
            SlimeRandomDir { yrot, next } => {
                *next -= 1;
                if *next <= 0 {
                    *next = reduced(40 + cx.rng.gen_range(0..60));
                    *yrot = cx.rng.gen_range(0..360) as f32;
                }
                super::custom::slime_direction(m, *yrot, false);
            }
            SlimeKeepJumping => super::custom::slime_wanted(m, 1.0),
            PhantomStrategy { next_sweep } => {
                if m.v.phase == 0 {
                    *next_sweep -= 1;
                    if *next_sweep <= 0 {
                        m.v.phase = 1;
                        phantom_anchor_above_target(m, cx);
                        *next_sweep = reduced((8 + cx.rng.gen_range(0..4)) * 20);
                        cx.acts.push(Act::Sound { name: "minecraft:entity.phantom.swoop".into(), pos: cx.pos, hostile: true, volume: 10.0, pitch: 0.95 + cx.rng.r#gen::<f32>() * 0.1 });
                    }
                }
            }
            PhantomSweep { .. } => {
                let Some(t) = target_of(m, cx) else { return };
                m.v.move_target = [t.pos[0], t.pos[1] + t.height * 0.5, t.pos[2]];
                let me = Aabb::entity(cx.pos, m.width(), m.height()).inflate(0.2, 0.2, 0.2);
                if me.intersects(&t.bbox()) {
                    cx.acts.push(Act::MobAttack { mob: cx.eid, target: t.eid });
                    m.v.phase = 0;
                    cx.acts.push(Act::LevelEvent { ev: 1039, pos: cx.block_pos(), data: 0 });
                } else if m.mo.horizontal_collision || m.living.hurt_time > 0 {
                    m.v.phase = 0;
                }
            }
            PhantomCircle { angle, distance, height, clockwise } => {
                if cx.rng.gen_range(0..reduced(350).max(1)) == 0 {
                    *height = -4.0 + cx.rng.r#gen::<f32>() * 9.0;
                }
                if cx.rng.gen_range(0..reduced(250).max(1)) == 0 {
                    *distance += 1.0;
                    if *distance > 15.0 {
                        *distance = 5.0;
                        *clockwise = -*clockwise;
                    }
                }
                if cx.rng.gen_range(0..reduced(450).max(1)) == 0 {
                    *angle = cx.rng.r#gen::<f32>() * 2.0 * std::f32::consts::PI;
                    phantom_select_next(m, cx, angle, *distance, *height, *clockwise);
                }
                if dist2(m.v.move_target, cx.pos) < 4.0 {
                    phantom_select_next(m, cx, angle, *distance, *height, *clockwise);
                }
                let p = cx.block_pos();
                if m.v.move_target[1] < cx.pos[1] && !cx.block([p[0], p[1] - 1, p[2]]).is_air() {
                    *height = height.max(1.0);
                    phantom_select_next(m, cx, angle, *distance, *height, *clockwise);
                }
                if m.v.move_target[1] > cx.pos[1] && !cx.block([p[0], p[1] + 1, p[2]]).is_air() {
                    *height = height.min(-1.0);
                    phantom_select_next(m, cx, angle, *distance, *height, *clockwise);
                }
            }
            CastingSpell => {
                if let Some(t) = target_of(m, cx) {
                    m.look_at_seen(&t, m.ty.max_head_y(), m.ty.max_head_x());
                }
            }
            Spell { kind, warmup, .. } => {
                *warmup -= 1;
                if *warmup == 0 {
                    cast_spell(m, cx, *kind);
                    let sound = match *kind {
                        1 => "minecraft:entity.evoker.cast_spell",
                        2 => "minecraft:entity.evoker.cast_spell",
                        3 => "minecraft:entity.evoker.cast_spell",
                        _ => "minecraft:entity.illusioner.cast_spell",
                    };
                    cx.acts.push(Act::Sound { name: sound.into(), pos: cx.pos, hostile: true, volume: 1.0, pitch: 1.0 });
                }
            }
            VexCharge => {
                let Some(t) = target_of(m, cx) else { return };
                let me = Aabb::entity(cx.pos, m.width(), m.height());
                if me.intersects(&t.bbox()) {
                    cx.acts.push(Act::MobAttack { mob: cx.eid, target: t.eid });
                    m.aggressive = false;
                } else if dist2(cx.pos, t.pos) < 9.0 {
                    m.mc.set_wanted(t.eye_pos(), 1.0);
                }
            }
            VexRandomMove => {
                let origin = m.v.bound.unwrap_or(cx.block_pos());
                for _ in 0..3 {
                    let q = [origin[0] + cx.rng.gen_range(0..15) - 7, origin[1] + cx.rng.gen_range(0..11) - 5, origin[2] + cx.rng.gen_range(0..15) - 7];
                    if cx.block(q).is_air() {
                        let c = [q[0] as f64 + 0.5, q[1] as f64 + 0.5, q[2] as f64 + 0.5];
                        m.mc.set_wanted(c, 0.25);
                        if m.target.is_none() {
                            m.look_at(c, 180.0, 20.0);
                        }
                        break;
                    }
                }
            }
            ShulkerAttack { time } => {
                if cx.difficulty == 0 {
                    return;
                }
                *time -= 1;
                let Some(t) = target_of(m, cx) else { return };
                m.look_at_seen(&t, 180.0, 180.0);
                if dist2(cx.pos, t.pos) < 400.0 {
                    if *time <= 0 {
                        *time = 20 + cx.rng.gen_range(0..10) * 20 / 2;
                        cx.acts.push(Act::Fire { ty: PType::ShulkerBullet, owner: cx.eid, from: [cx.pos[0], cx.pos[1] + 0.5, cx.pos[2]], dir: [0.0, 0.0, 0.0], target: Some(t.eid) });
                        cx.acts.push(Act::Sound { name: "minecraft:entity.shulker.shoot".into(), pos: cx.pos, hostile: true, volume: 2.0, pitch: (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 1.0 });
                    }
                } else {
                    m.target = None;
                }
            }
            ShulkerPeek { time } => *time -= 1,
            DragonFly { node, phase_ticks, strafe } => super::custom::dragon_tick(m, cx, node, phase_ticks, strafe),
            WardenRoar { ticks } => {
                *ticks -= 1;
                m.v.roar = *ticks;
            }
            WardenSonic { ticks, .. } => {
                *ticks -= 1;
                let Some(t) = target_of(m, cx) else { return };
                m.look_at_seen(&t, 30.0, 30.0);
                if *ticks == 26 {
                    cx.acts.push(Act::Sound { name: "minecraft:entity.warden.sonic_boom".into(), pos: cx.pos, hostile: true, volume: 3.0, pitch: 1.0 });
                    cx.acts.push(Act::Hurt { target: t.eid, kind: "sonic_boom", by: Some(cx.eid), amount: 10.0 });
                    let d = [t.pos[0] - cx.pos[0], 0.0, t.pos[2] - cx.pos[2]];
                    let l = (d[0] * d[0] + d[2] * d[2]).sqrt().max(1e-3);
                    cx.acts.push(Act::Push { target: t.eid, vel: [d[0] / l * 2.5, 0.5, d[2] / l * 2.5] });
                }
            }
            WardenSniff { ticks } => {
                *ticks -= 1;
                m.v.sniff = *ticks;
                if *ticks == 50 {
                    cx.acts.push(Act::Sound { name: "minecraft:entity.warden.sniff".into(), pos: cx.pos, hostile: true, volume: 5.0, pitch: 1.0 });
                }
            }
            BreezeShoot { ticks, .. } => {
                *ticks -= 1;
                let Some(t) = target_of(m, cx) else { return };
                m.look_at_seen(&t, 30.0, 30.0);
                if *ticks == 0 {
                    let from = [cx.pos[0], cx.pos[1] + m.height() * 0.5 + 0.3, cx.pos[2]];
                    let d = [t.pos[0] - from[0], t.pos[1] + t.height * 0.5 - from[1], t.pos[2] - from[2]];
                    let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-7);
                    let v = 0.7;
                    let unc = (5 - cx.difficulty) as f64 * 4.0 * 0.0075;
                    let dir = [d[0] / l * v + cx.rng.r#gen::<f64>() * unc, d[1] / l * v + cx.rng.r#gen::<f64>() * unc, d[2] / l * v + cx.rng.r#gen::<f64>() * unc];
                    cx.acts.push(Act::Fire { ty: PType::WindCharge, owner: cx.eid, from, dir, target: None });
                    cx.acts.push(Act::Sound { name: "minecraft:entity.breeze.shoot".into(), pos: cx.pos, hostile: true, volume: 1.5, pitch: 1.0 });
                }
            }
            _ => {}
        }
    }
}

fn self_stuck(s: &mut Special) -> &mut bool {
    match s {
        Special::DrownedSwimUp { stuck, .. } => stuck,
        _ => unreachable!(),
    }
}

fn self_squid_flee_ok(m: &Mob, cx: &Ctx) -> bool {
    let Some((by, _)) = m.living.last_hurt_by_mob else { return false };
    cx.find(by).is_some_and(|a| a.health > 0.0 && dist2(a.pos, cx.pos) < 100.0)
}

fn target_continue_simple(m: &mut Mob, cx: &mut Ctx, st: &mut TState) -> bool {
    let t = m.target.or(st.target_mob);
    let Some(t) = t.and_then(|t| cx.find(t)).cloned() else { return false };
    if t.health <= 0.0 || (t.player && !t.attackable_player()) {
        return false;
    }
    let within = st.follow.unwrap_or_else(|| m.follow_range());
    if dist2(cx.pos, t.pos) > within * within {
        return false;
    }
    m.target = Some(t.eid);
    true
}

/// `RandomSource.triangle(mode, deviation)`
pub fn triangle(rng: &mut impl Rng, mode: f64, dev: f64) -> f64 {
    mode + dev * (rng.r#gen::<f64>() - rng.r#gen::<f64>())
}

fn deals_damage(m: &Mob) -> bool {
    m.ty.is("magma_cube") || m.size > 1
}

/// `EnderMan.isBeingStaredBy` / `isLookingAtMe`
pub fn looking_at_me(m: &Mob, cx: &mut Ctx, p: &Seen) -> bool {
    if p.head.is_some_and(|h| crate::data::Items::get().name(h.item) == "carved_pumpkin") {
        return false;
    }
    let view = super::control::view(p.pitch, p.yaw);
    let me_eye = [cx.pos[0], cx.pos[1] + m.eye(), cx.pos[2]];
    let pe = p.eye_pos();
    let mut d = [me_eye[0] - pe[0], me_eye[1] - pe[1], me_eye[2] - pe[2]];
    let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if dist < 1e-6 {
        return false;
    }
    for v in &mut d {
        *v /= dist;
    }
    let dot = view[0] * d[0] + view[1] * d[1] + view[2] * d[2];
    dot > 1.0 - 0.025 / dist && m.can_see(cx, p)
}

fn find_block(cx: &Ctx, h: i32, v: i32, f: impl Fn(&str) -> bool) -> Option<[i32; 3]> {
    let p = cx.block_pos();
    let mut best: Option<(i32, [i32; 3])> = None;
    for dy in -v..=v {
        for dx in -h..=h {
            for dz in -h..=h {
                let q = [p[0] + dx, p[1] + dy, p[2] + dz];
                if f(cx.block(q).name()) {
                    let d = dx * dx + dy * dy + dz * dz;
                    if best.is_none_or(|b| d < b.0) {
                        best = Some((d, q));
                    }
                }
            }
        }
    }
    best.map(|b| b.1)
}

fn water_pos(m: &Mob, cx: &mut Ctx) -> Option<[f64; 3]> {
    let _ = m;
    let p = cx.block_pos();
    for _ in 0..10 {
        let q = [p[0] + cx.rng.gen_range(0..20) - 10, p[1] + cx.rng.gen_range(0..6) - 3, p[2] + cx.rng.gen_range(0..20) - 10];
        if crate::registry::vstate::is_water(cx.block(q)) {
            return Some([q[0] as f64 + 0.5, q[1] as f64, q[2] as f64 + 0.5]);
        }
    }
    None
}

fn dir_offset(d: u8) -> [i32; 3] {
    [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]][d as usize % 6]
}

/// `InfestedBlock.infestedStateByHost`
fn infested_of(host: &str) -> Option<&'static str> {
    Some(match host {
        "stone" => "infested_stone",
        "cobblestone" => "infested_cobblestone",
        "stone_bricks" => "infested_stone_bricks",
        "mossy_stone_bricks" => "infested_mossy_stone_bricks",
        "cracked_stone_bricks" => "infested_cracked_stone_bricks",
        "chiseled_stone_bricks" => "infested_chiseled_stone_bricks",
        "deepslate" => "infested_deepslate",
        _ => return None,
    })
}

fn phantom_anchor_above_target(m: &mut Mob, cx: &mut Ctx) {
    if let Some(t) = target_of(m, cx) {
        let p = bp(t.pos);
        let mut a = [p[0], p[1] + 20 + cx.rng.gen_range(0..20), p[2]];
        if a[1] < 63 {
            a[1] = 64;
        }
        m.v.anchor = a;
    }
}

fn phantom_select_next(m: &mut Mob, cx: &mut Ctx, angle: &mut f32, distance: f32, height: f32, clockwise: f32) {
    if m.v.anchor == [0, 0, 0] {
        m.v.anchor = cx.block_pos();
    }
    *angle += clockwise * 15.0 * std::f32::consts::PI / 180.0;
    let a = m.v.anchor;
    m.v.move_target = [a[0] as f64 + (distance * angle.cos()) as f64, a[1] as f64 - 4.0 + height as f64, a[2] as f64 + (distance * angle.sin()) as f64];
}

fn cast_spell(m: &mut Mob, cx: &mut Ctx, kind: u8) {
    match kind {
        1 => {
            for _ in 0..3 {
                let p = cx.block_pos();
                let q = [p[0] - 2 + cx.rng.gen_range(0..5), p[1] + 1, p[2] - 2 + cx.rng.gen_range(0..5)];
                let life = 20 * (30 + cx.rng.gen_range(0..90));
                cx.acts.push(Act::SummonVex { owner: cx.eid, pos: q, life });
            }
        }
        2 => {
            let Some(t) = target_of(m, cx) else { return };
            let min_y = t.pos[1].min(cx.pos[1]);
            let max_y = t.pos[1].max(cx.pos[1]) + 1.0;
            let angle = (t.pos[2] - cx.pos[2]).atan2(t.pos[0] - cx.pos[0]) as f32;
            if dist2(cx.pos, t.pos) < 9.0 {
                for i in 0..5 {
                    let a = angle + i as f32 * std::f32::consts::PI * 0.4;
                    fang(cx, cx.pos[0] + a.cos() as f64 * 1.5, cx.pos[2] + a.sin() as f64 * 1.5, min_y, max_y, a, 0);
                }
                for i in 0..8 {
                    let a = angle + i as f32 * std::f32::consts::PI * 2.0 / 8.0 + std::f32::consts::PI * 2.0 / 5.0;
                    fang(cx, cx.pos[0] + a.cos() as f64 * 2.5, cx.pos[2] + a.sin() as f64 * 2.5, min_y, max_y, a, 3);
                }
            } else {
                for i in 0..16 {
                    let d = 1.25 * (i + 1) as f64;
                    fang(cx, cx.pos[0] + angle.cos() as f64 * d, cx.pos[2] + angle.sin() as f64 * d, min_y, max_y, angle, i);
                }
            }
        }
        3 => {
            if let Some(s) = m.v.e0 {
                cx.acts.push(Act::SheepColor { eid: s, color: 14 });
            }
        }
        4 => {
            cx.acts.push(Act::Effect { target: cx.eid, effect: "invisibility", duration: 1200, amp: 0 });
        }
        5 => {
            if let Some(t) = target_of(m, cx) {
                cx.acts.push(Act::Effect { target: t.eid, effect: "blindness", duration: 400, amp: 0 });
                m.v.e0 = Some(t.eid);
            }
        }
        _ => {}
    }
}

/// `Evoker.EvokerAttackSpellGoal.createSpellEntity`: finds the floor for a fang.
fn fang(cx: &mut Ctx, x: f64, z: f64, min_y: f64, max_y: f64, yaw: f32, delay: i32) {
    let mut p = [x.floor() as i32, max_y.floor() as i32, z.floor() as i32];
    let mut found = false;
    let mut offset = 0.0;
    loop {
        let below = [p[0], p[1] - 1, p[2]];
        let bs = cx.block(below);
        if crate::game::physics::has_collision(bs) && super::path::solid_render(bs) {
            if !cx.block(p).is_air() {
                let st = cx.block(p);
                if crate::game::physics::has_collision(st) {
                    offset = 1.0;
                }
            }
            found = true;
            break;
        }
        p[1] -= 1;
        if (p[1] as f64) < min_y.floor() - 1.0 {
            break;
        }
    }
    if found {
        cx.acts.push(Act::Fang { owner: cx.eid, pos: [x, p[1] as f64 + offset, z], yaw, delay });
    }
}

/// `Witch.performRangedAttack`
pub fn witch_throw(m: &mut Mob, cx: &mut Ctx, t: &Seen) {
    if m.v.using_ticks > 0 {
        return;
    }
    let xd = t.pos[0] + t.vel[0] - cx.pos[0];
    let yd = t.pos[1] + t.eye - 1.1 - cx.pos[1];
    let zd = t.pos[2] + t.vel[2] - cx.pos[2];
    let dist = (xd * xd + zd * zd).sqrt();
    let mut potion = "harming";
    if t.raider {
        potion = if t.health <= 4.0 { "healing" } else { "regeneration" };
        m.target = None;
    } else if dist >= 8.0 && !t.effects.contains(&"slowness") {
        potion = "slowness";
    } else if t.health >= 8.0 && !t.effects.contains(&"poison") {
        potion = "poison";
    } else if dist <= 3.0 && !t.effects.contains(&"weakness") && cx.rng.r#gen::<f32>() < 0.25 {
        potion = "weakness";
    }
    let from = [cx.pos[0], cx.pos[1] + m.eye() - 0.1, cx.pos[2]];
    cx.acts.push(Act::Potion { owner: cx.eid, from, dir: [xd, yd + dist * 0.2, zd], potion: potion.to_string(), velocity: 0.75, uncertainty: 8.0 });
    cx.acts.push(Act::Sound { name: "minecraft:entity.witch.throw".into(), pos: cx.pos, hostile: true, volume: 1.0, pitch: 0.8 + cx.rng.r#gen::<f32>() * 0.4 });
}

/// `WitherBoss.performRangedAttack(head, x, y, z, dangerous)`
pub fn wither_shoot(m: &mut Mob, cx: &mut Ctx, head: usize, target: i32, to: [f64; 3], dangerous: bool) {
    let hp = wither_head_pos(m, cx, head);
    let d = [to[0] - hp[0], to[1] + 0.5 - hp[1], to[2] - hp[2]];
    let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-7);
    let danger = dangerous || cx.rng.r#gen::<f32>() < 0.001;
    cx.acts.push(Act::Fire { ty: PType::WitherSkull, owner: cx.eid, from: hp, dir: [d[0] / l, d[1] / l, d[2] / l], target: if danger { Some(-1) } else { None } });
    cx.acts.push(Act::LevelEvent { ev: 1024, pos: cx.block_pos(), data: 0 });
    let _ = target;
}

pub fn wither_head_pos(m: &Mob, cx: &Ctx, head: usize) -> [f64; 3] {
    if head == 0 {
        return [cx.pos[0], cx.pos[1] + 3.0, cx.pos[2]];
    }
    let a = (m.mo.body_yaw + (180 * (head as i32 - 1)) as f32).to_radians();
    [cx.pos[0] + a.cos() as f64 * 1.3, cx.pos[1] + 2.2, cx.pos[2] + a.sin() as f64 * 1.3]
}
