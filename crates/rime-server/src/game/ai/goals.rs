//! The vanilla goal library (`net.minecraft.world.entity.ai.goal`): each
//! variant keeps the goal's fields and runs its `canUse` / `canContinueToUse`
//! / `start` / `stop` / `tick` as vanilla does.

use rand::Rng;

use super::{bp, dist2, reduced, Ctx, Seen, JUMP, LOOK, MOVE, TARGET};
use crate::game::mob::Mob;
use crate::game::mobtick::Act;

/// Who a targeting / looking / avoiding goal picks.
pub type Sel = fn(&Mob, &Seen) -> bool;

pub fn any(_: &Mob, _: &Seen) -> bool {
    true
}

pub fn players(_: &Mob, s: &Seen) -> bool {
    s.player
}

/// `TargetGoal` bookkeeping.
#[derive(Clone, Debug, Default)]
pub struct TState {
    pub must_see: bool,
    pub must_reach: bool,
    pub target_mob: Option<i32>,
    pub unseen: i32,
    pub unseen_memory: i32,
    pub reach_cache: i32,
    pub reach_time: i32,
    /// fixed follow distance (`getFollowDistance` override), else the attribute
    pub follow: Option<f64>,
}

impl TState {
    pub fn new(must_see: bool, must_reach: bool) -> TState {
        TState { must_see, must_reach, unseen_memory: 60, ..Default::default() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrollKind {
    /// `RandomStrollGoal` (DefaultRandomPos 10, 7)
    Default,
    /// `WaterAvoidingRandomStrollGoal(probability)`
    WaterAvoiding(f32),
    /// `RandomSwimmingGoal`
    Swim,
    /// `WaterAvoidingRandomFlyingGoal`
    Fly,
    /// `GolemRandomStrollInVillageGoal` (approximated by land positions)
    Golem,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MeleeKind {
    Plain,
    /// `ZombieAttackGoal` (raises arms)
    Zombie,
    /// `Spider.SpiderAttackGoal`
    Spider,
    /// `OcelotAttackGoal`
    Ocelot,
    /// `PolarBear.PolarBearMeleeAttackGoal`
    PolarBear,
    /// `Rabbit.EvilRabbitAttackGoal`
    KillerBunny,
    /// `Panda.PandaAttackGoal`
    Panda,
    /// `Fox.FoxBehaviorGoal` style (pounce handled elsewhere)
    Fox,
    /// `Vindicator.VindicatorMeleeAttackGoal`
    Vindicator,
    /// `Ravager.RavagerMeleeAttackGoal`
    Ravager,
    /// `Drowned.DrownedAttackGoal`
    Drowned,
    /// `Turtle` none; `Dolphin` / `Goat` ramming handled elsewhere
    Bee,
}

#[derive(Clone, Debug)]
pub enum Goal {
    Float,
    Panic { speed: f64, pos: [f64; 3], running: bool },
    Breed { speed: f64, partner: Option<i32>, love_time: i32 },
    Tempt { speed: f64, items: &'static str, scare: bool, stop: f64, player: Option<i32>, calm: i32, p: [f64; 3], rot: (f32, f32), flying: bool },
    FollowParent { speed: f64, parent: Option<i32>, recalc: i32 },
    Stroll { speed: f64, interval: i32, kind: StrollKind, check_no_action: bool, wanted: [f64; 3], force: bool },
    LookAt { range: f32, prob: f32, sel: Sel, players: bool, only_h: bool, at: Option<i32>, time: i32 },
    RandomLook { rel: [f64; 2], time: i32 },
    Melee { speed: f64, unseen: bool, kind: MeleeKind, recalc: i32, attack: i32, pathed: [f64; 3], last_check: i64, ticks: i32, has_path: bool },
    Nearest { sel: Sel, players: bool, interval: i32, found: Option<i32>, st: TState, only_dark: bool },
    HurtBy { alert: bool, ignore: &'static [&'static str], alert_ignore: &'static [&'static str], stamp: i32, st: TState },
    OwnerHurtBy { stamp: i32, st: TState, hurt: Option<i32> },
    OwnerHurt { stamp: i32, st: TState, hurt: Option<i32> },
    Avoid { sel: Sel, dist: f32, walk: f64, sprint: f64, avoid: Option<i32>, path: Option<super::path::Path> },
    Leap { yd: f32 },
    RangedBow { speed: f64, interval: i32, r2: f32, attack_time: i32, see_time: i32, strafe_time: i32, clockwise: bool, backwards: bool },
    Ranged { speed: f64, min: i32, max: i32, radius: f32, attack_time: i32, see_time: i32 },
    Crossbow { speed: f64, r2: f32, state: u8, see_time: i32, attack_delay: i32, update_path: i32 },
    Swell { target: Option<i32> },
    RestrictSun,
    FleeSun { speed: f64, wanted: [f64; 3] },
    EatBlock { tick: i32 },
    TryFindWater,
    FollowOwner { speed: f64, start: f32, stop: f32, recalc: i32, old_water: f32 },
    Sit,
    MoveTowardsTarget { speed: f64, within: f32, wanted: [f64; 3] },
    MoveTowardsRestriction { speed: f64, wanted: [f64; 3] },
    Special(Box<super::mobs::Special>),
}

fn target_of<'a>(m: &Mob, cx: &'a Ctx) -> Option<&'a Seen> {
    m.target.and_then(|t| cx.find(t))
}

fn alive(s: Option<&Seen>) -> bool {
    s.is_some_and(|s| s.health > 0.0)
}

impl Goal {
    pub fn flags(&self) -> u8 {
        match self {
            Goal::Float => JUMP,
            Goal::Panic { .. } | Goal::Stroll { .. } | Goal::Avoid { .. } | Goal::Swell { .. } | Goal::FleeSun { .. } | Goal::MoveTowardsTarget { .. } | Goal::MoveTowardsRestriction { .. } => MOVE,
            Goal::Breed { .. } | Goal::Tempt { .. } | Goal::Melee { .. } | Goal::RangedBow { .. } | Goal::Ranged { .. } | Goal::Crossbow { .. } | Goal::FollowOwner { .. } => MOVE | LOOK,
            Goal::FollowParent { .. } | Goal::RestrictSun | Goal::TryFindWater => 0,
            Goal::LookAt { .. } => LOOK,
            Goal::RandomLook { .. } => MOVE | LOOK,
            Goal::Nearest { .. } | Goal::HurtBy { .. } | Goal::OwnerHurtBy { .. } | Goal::OwnerHurt { .. } => TARGET,
            Goal::Leap { .. } => JUMP | MOVE,
            Goal::EatBlock { .. } => MOVE | LOOK | JUMP,
            Goal::Sit => JUMP | MOVE,
            Goal::Special(s) => s.flags(),
        }
    }

    /// `Goal.isInterruptable`
    pub fn interruptable(&self) -> bool {
        match self {
            Goal::Special(s) => s.interruptable(),
            _ => true,
        }
    }

    /// `Goal.requiresUpdateEveryTick`
    pub fn every_tick(&self) -> bool {
        match self {
            Goal::Float | Goal::Melee { .. } | Goal::RandomLook { .. } | Goal::RangedBow { .. } | Goal::Ranged { .. } | Goal::Crossbow { .. } | Goal::Swell { .. } | Goal::EatBlock { .. } => true,
            Goal::Special(s) => s.every_tick(),
            _ => false,
        }
    }

    fn adjusted(&self, n: i32) -> i32 {
        if self.every_tick() { n } else { reduced(n) }
    }

    pub fn can_use(&mut self, m: &mut Mob, cx: &mut Ctx) -> bool {
        let every = self.every_tick();
        let adj = |n: i32| if every { n } else { reduced(n) };
        match self {
            Goal::Float => (m.mo.in_water && m.mo.water_height > fluid_jump_threshold(m)) || m.mo.in_lava,
            Goal::Panic { pos, .. } => {
                if !should_panic(m) {
                    return false;
                }
                if m.fire > 0 {
                    if let Some(w) = look_for_water(m, cx, 5) {
                        *pos = [w[0] as f64, w[1] as f64, w[2] as f64];
                        return true;
                    }
                }
                match m.random_pos(cx, 5, 4) {
                    Some(p) => {
                        *pos = p;
                        true
                    }
                    None => false,
                }
            }
            Goal::Breed { partner, .. } => {
                if !m.in_love() {
                    return false;
                }
                *partner = free_partner(m, cx);
                partner.is_some()
            }
            Goal::Tempt { calm, items, player, .. } => {
                if *calm > 0 {
                    *calm -= 1;
                    return false;
                }
                let range = m.living.attrs.value(crate::game::combat::attr::Attr::TemptRange);
                let tag = *items;
                let game = cx.game;
                let found = m.nearest_player(cx, range, false, false, &|s| s.holding_id(|i| game.data.item_in(tag, i)));
                *player = found.map(|s| s.eid);
                player.is_some()
            }
            Goal::FollowParent { parent, .. } => {
                if m.age >= 0 {
                    return false;
                }
                let bb = crate::anticheat::combat::Aabb::entity(cx.pos, m.width(), m.height()).inflate(8.0, 4.0, 8.0);
                let mut best: Option<(f64, i32)> = None;
                for s in cx.seen.iter() {
                    if s.ty == m.ty.name() && s.age >= 0 && !s.baby && s.eid != cx.eid && s.bbox().intersects(&bb) {
                        let d = dist2(cx.pos, s.pos);
                        if best.is_none_or(|b| d < b.0) {
                            best = Some((d, s.eid));
                        }
                    }
                }
                match best {
                    Some((d, e)) if d >= 9.0 => {
                        *parent = Some(e);
                        true
                    }
                    _ => false,
                }
            }
            Goal::Stroll { interval, kind, check_no_action, wanted, force, .. } => {
                if !*force {
                    if *check_no_action && m.no_action >= 100 {
                        return false;
                    }
                    if cx.rng.gen_range(0..reduced(*interval).max(1)) != 0 {
                        return false;
                    }
                }
                let p = match *kind {
                    StrollKind::Default => m.random_pos(cx, 10, 7),
                    StrollKind::WaterAvoiding(prob) => {
                        if m.mo.in_water {
                            m.land_pos(cx, 15, 7).or_else(|| m.random_pos(cx, 10, 7))
                        } else if cx.rng.r#gen::<f32>() >= prob {
                            m.land_pos(cx, 10, 7)
                        } else {
                            m.random_pos(cx, 10, 7)
                        }
                    }
                    StrollKind::Swim => m.swim_pos(cx, 10, 7),
                    StrollKind::Fly => {
                        let view = super::control::view(m.mo.pitch, m.mo.yaw);
                        if m.mo.in_water {
                            m.land_pos(cx, 15, 15)
                        } else if cx.rng.r#gen::<f32>() >= 0.001 {
                            m.air_pos(cx, 8, 7, 1, Some(view)).or_else(|| m.random_pos(cx, 10, 7))
                        } else {
                            m.random_pos(cx, 10, 7)
                        }
                    }
                    StrollKind::Golem => m.land_pos(cx, 10, 7),
                };
                match p {
                    Some(p) => {
                        *wanted = p;
                        *force = false;
                        true
                    }
                    None => false,
                }
            }
            Goal::LookAt { range, prob, sel, players, at, .. } => {
                if cx.rng.r#gen::<f32>() >= *prob {
                    return false;
                }
                if m.target.is_some() {
                    *at = m.target;
                }
                let s = *sel;
                let me = &*m;
                let found = if *players {
                    me.nearest_player(cx, *range as f64, false, false, &|t| s(me, t))
                } else {
                    me.nearest(cx, *range as f64, 3.0, false, false, &|t| !t.player && s(me, t))
                };
                *at = found.map(|f| f.eid);
                at.is_some()
            }
            Goal::RandomLook { .. } => cx.rng.r#gen::<f32>() < 0.02,
            Goal::Melee { last_check, has_path, kind, .. } => {
                if *kind == MeleeKind::Spider && m.mo.in_water {
                    // spider: no special canUse, but the vehicle check
                }
                if cx.now - *last_check < 20 {
                    return false;
                }
                *last_check = cx.now;
                let Some(t) = target_of(m, cx).cloned() else { return false };
                if t.health <= 0.0 {
                    return false;
                }
                if *kind == MeleeKind::Panda && m.is_baby() {
                    return false;
                }
                let p = m.create_path_to(cx, t.pos, 0);
                *has_path = p.is_some();
                m.v.p0 = None;
                if let Some(p) = p {
                    m.nav.path = Some(p);
                    m.nav.speed = 0.0;
                    true
                } else {
                    m.in_melee_range(cx, &t)
                }
            }
            Goal::Nearest { sel, players, interval, found, st, only_dark } => {
                if *interval > 0 && cx.rng.gen_range(0..*interval) != 0 {
                    return false;
                }
                if *only_dark {
                    // Spider.SpiderTargetGoal: only in the dark
                    if cx.light_cost(cx.block_pos()) + 0.5 >= 0.5 {
                        return false;
                    }
                }
                let range = st.follow.unwrap_or_else(|| m.follow_range());
                let s = *sel;
                let me = &*m;
                let f = if *players {
                    me.nearest_player(cx, range, true, st.must_see, &|t| s(me, t))
                } else {
                    me.nearest(cx, range, range, true, st.must_see, &|t| s(me, t))
                };
                *found = None;
                let Some(f) = f else { return false };
                if !target_ok(m, cx, &f, st) {
                    return false;
                }
                *found = Some(f.eid);
                true
            }
            Goal::HurtBy { ignore, stamp, st, .. } => {
                let Some((by, at)) = m.living.last_hurt_by_mob else { return false };
                if at == *stamp {
                    return false;
                }
                let Some(t) = cx.find(by).cloned() else { return false };
                if ignore.contains(&t.ty) {
                    return false;
                }
                let follow = st.follow;
                let _ = follow;
                m.can_target(cx, &t, true, 0.0, false, false) && target_ok(m, cx, &t, st)
            }
            Goal::OwnerHurtBy { stamp, st, hurt } => owner_hurt_can_use(m, cx, stamp, st, hurt, true),
            Goal::OwnerHurt { stamp, st, hurt } => owner_hurt_can_use(m, cx, stamp, st, hurt, false),
            Goal::Avoid { sel, dist, avoid, path, .. } => {
                let s = *sel;
                let me = &*m;
                let found = me.nearest(cx, *dist as f64, 3.0, true, false, &|t| t.attackable_player() && s(me, t));
                let Some(f) = found else { return false };
                *avoid = Some(f.eid);
                let Some(p) = m.random_pos_away(cx, 16, 7, f.pos) else { return false };
                if dist2(f.pos, p) < dist2(f.pos, cx.pos) {
                    return false;
                }
                *path = m.create_path_to(cx, p, 0);
                path.is_some()
            }
            Goal::Leap { .. } => {
                let Some(t) = target_of(m, cx) else { return false };
                let d = dist2(cx.pos, t.pos);
                if !(4.0..=16.0).contains(&d) {
                    return false;
                }
                cx.on_ground && cx.rng.gen_range(0..reduced(5)) == 0
            }
            Goal::RangedBow { .. } => target_of(m, cx).is_some() && super::mobs::holding_bow(m),
            Goal::Ranged { .. } => alive(target_of(m, cx)),
            Goal::Crossbow { .. } => alive(target_of(m, cx)) && super::mobs::holding_crossbow(m),
            Goal::Swell { .. } => {
                let t = target_of(m, cx);
                m.v.swell_dir > 0 || t.is_some_and(|t| dist2(cx.pos, t.pos) < 9.0)
            }
            Goal::RestrictSun => cx.bright() && m.eq[crate::game::combat::living::HEAD].is_none() && matches!(m.nav.kind, super::path::NavKind::Ground | super::path::NavKind::Climber),
            Goal::FleeSun { wanted, .. } => {
                if m.target.is_some() || !cx.bright() || m.fire <= 0 || !cx.sky(cx.block_pos()) || m.eq[crate::game::combat::living::HEAD].is_some() {
                    return false;
                }
                match hiding_place(m, cx) {
                    Some(p) => {
                        *wanted = p;
                        true
                    }
                    None => false,
                }
            }
            Goal::EatBlock { .. } => {
                if cx.rng.gen_range(0..if m.is_baby() { 50 } else { 1000 }) != 0 {
                    return false;
                }
                let p = cx.block_pos();
                cx.block(p).name() == "short_grass" || cx.block([p[0], p[1] - 1, p[2]]).name() == "grass_block"
            }
            Goal::TryFindWater => cx.on_ground && !m.mo.in_water,
            Goal::FollowOwner { start, .. } => {
                let Some(o) = owner_of(m, cx) else { return false };
                if o.spectator() || unable_to_move_to_owner(m) {
                    return false;
                }
                dist2(cx.pos, o.pos) >= (*start * *start) as f64
            }
            Goal::Sit => {
                if !m.tame || m.mo.in_water || !cx.on_ground {
                    return false;
                }
                match owner_of(m, cx) {
                    None => true,
                    Some(o) => {
                        if dist2(cx.pos, o.pos) < 144.0 && o.last_hurt_by.is_some() {
                            return false;
                        }
                        m.ordered_sit
                    }
                }
            }
            Goal::MoveTowardsTarget { within, wanted, .. } => {
                let Some(t) = target_of(m, cx).cloned() else { return false };
                if dist2(cx.pos, t.pos) > (*within * *within) as f64 {
                    return false;
                }
                let dir = [t.pos[0] - cx.pos[0], 0.0, t.pos[2] - cx.pos[2]];
                match m.random_pos_dir(cx, 16, 7, dir, std::f64::consts::FRAC_PI_2) {
                    Some(p) => {
                        *wanted = p;
                        true
                    }
                    None => false,
                }
            }
            Goal::MoveTowardsRestriction { wanted, .. } => {
                let Some((h, _)) = m.home else { return false };
                if m.within_home(cx.block_pos()) {
                    return false;
                }
                let dir = [h[0] as f64 + 0.5 - cx.pos[0], 0.0, h[2] as f64 + 0.5 - cx.pos[2]];
                match m.random_pos_dir(cx, 16, 7, dir, std::f64::consts::FRAC_PI_2) {
                    Some(p) => {
                        *wanted = p;
                        true
                    }
                    None => false,
                }
            }
            Goal::Special(s) => {
                let _ = adj;
                s.can_use(m, cx)
            }
        }
    }

    pub fn can_continue(&mut self, m: &mut Mob, cx: &mut Ctx) -> bool {
        match self {
            Goal::Panic { .. } | Goal::Stroll { .. } | Goal::Avoid { .. } | Goal::FleeSun { .. } => !m.nav.is_done(),
            Goal::Breed { partner, love_time, .. } => {
                let p = partner.and_then(|p| cx.find(p));
                p.is_some_and(|p| p.health > 0.0 && p.in_love && !p.panicking) && *love_time < 60
            }
            Goal::Tempt { scare, player, p, rot, .. } => {
                if *scare {
                    if let Some(pl) = player.and_then(|e| cx.find(e)) {
                        if dist2(cx.pos, pl.pos) < 36.0 {
                            if dist2(pl.pos, *p) > 0.010000000000000002 {
                                return false;
                            }
                            if (pl.pitch - rot.0).abs() > 5.0 || (pl.yaw - rot.1).abs() > 5.0 {
                                return false;
                            }
                        } else {
                            *p = pl.pos;
                            *rot = (pl.pitch, pl.yaw);
                        }
                    }
                }
                self.can_use(m, cx)
            }
            Goal::FollowParent { parent, .. } => {
                if m.age >= 0 {
                    return false;
                }
                let Some(p) = parent.and_then(|p| cx.find(p)) else { return false };
                if p.health <= 0.0 {
                    return false;
                }
                let d = dist2(cx.pos, p.pos);
                (9.0..=256.0).contains(&d)
            }
            Goal::LookAt { range, at, time, .. } => {
                let Some(t) = at.and_then(|a| cx.find(a)) else { return false };
                if t.health <= 0.0 {
                    return false;
                }
                dist2(cx.pos, t.pos) <= (*range * *range) as f64 && *time > 0
            }
            Goal::RandomLook { time, .. } => *time >= 0,
            Goal::Melee { unseen, kind, .. } => {
                let Some(t) = target_of(m, cx).cloned() else { return false };
                if t.health <= 0.0 {
                    return false;
                }
                if *kind == MeleeKind::Spider {
                    let br = cx.light_cost(cx.block_pos()) + 0.5;
                    if br >= 0.5 && cx.rng.gen_range(0..100) == 0 {
                        m.target = None;
                        return false;
                    }
                }
                if !*unseen {
                    !m.nav.is_done()
                } else {
                    m.within_home(bp(t.pos)) && !(t.player && !t.attackable_player())
                }
            }
            Goal::Nearest { st, .. } | Goal::HurtBy { st, .. } | Goal::OwnerHurtBy { st, .. } | Goal::OwnerHurt { st, .. } => target_continue(m, cx, st),
            Goal::Leap { .. } => !cx.on_ground,
            Goal::RangedBow { .. } => (self_can_use_bow(m, cx) || !m.nav.is_done()) && super::mobs::holding_bow(m),
            Goal::Ranged { .. } => {
                let t = target_of(m, cx);
                alive(t) || (alive(t) && !m.nav.is_done())
            }
            Goal::Crossbow { .. } => (alive(target_of(m, cx)) || !m.nav.is_done()) && super::mobs::holding_crossbow(m),
            Goal::EatBlock { tick } => *tick > 0,
            Goal::FollowOwner { stop, .. } => {
                if m.nav.is_done() || unable_to_move_to_owner(m) {
                    return false;
                }
                owner_of(m, cx).is_some_and(|o| dist2(cx.pos, o.pos) > (*stop * *stop) as f64)
            }
            Goal::Sit => m.ordered_sit,
            Goal::MoveTowardsTarget { within, .. } => {
                let t = target_of(m, cx);
                !m.nav.is_done() && t.is_some_and(|t| t.health > 0.0 && dist2(cx.pos, t.pos) < (*within * *within) as f64)
            }
            Goal::MoveTowardsRestriction { .. } => !m.nav.is_done(),
            Goal::Special(s) => s.can_continue(m, cx),
            _ => self.can_use(m, cx),
        }
    }

    pub fn start(&mut self, m: &mut Mob, cx: &mut Ctx) {
        let every = self.every_tick();
        let adj = |n: i32| if every { n } else { reduced(n) };
        match self {
            Goal::Panic { speed, pos, running } => {
                m.move_to(cx, *pos, *speed);
                *running = true;
            }
            Goal::Tempt { player, p, rot, .. } => {
                if let Some(pl) = player.and_then(|e| cx.find(e)) {
                    *p = pl.pos;
                    *rot = (pl.pitch, pl.yaw);
                }
            }
            Goal::FollowParent { recalc, .. } => *recalc = 0,
            Goal::Stroll { speed, wanted, .. } => {
                m.move_to(cx, *wanted, *speed);
            }
            Goal::LookAt { time, .. } => *time = adj(40 + cx.rng.gen_range(0..40)),
            Goal::RandomLook { rel, time } => {
                let a = std::f64::consts::TAU * cx.rng.r#gen::<f64>();
                *rel = [a.cos(), a.sin()];
                *time = 20 + cx.rng.gen_range(0..20);
            }
            Goal::Melee { speed, recalc, attack, ticks, .. } => {
                let p = m.nav.path.take();
                m.follow_path(cx, p, *speed);
                m.aggressive = true;
                *recalc = 0;
                *attack = 0;
                *ticks = 0;
            }
            Goal::Nearest { found, st, .. } => {
                m.target = *found;
                target_start(st);
            }
            Goal::HurtBy { alert, alert_ignore, stamp, st, .. } => {
                let by = m.living.last_hurt_by_mob;
                m.target = by.map(|b| b.0);
                st.target_mob = m.target;
                *stamp = by.map(|b| b.1).unwrap_or(0);
                st.unseen_memory = 300;
                if *alert {
                    if let Some(t) = m.target {
                        alert_others(m, cx, t, alert_ignore);
                    }
                }
                target_start(st);
            }
            Goal::OwnerHurtBy { st, hurt, .. } | Goal::OwnerHurt { st, hurt, .. } => {
                m.target = *hurt;
                target_start(st);
            }
            Goal::Avoid { walk, path, .. } => {
                let p = path.take();
                m.follow_path(cx, p, *walk);
            }
            Goal::Leap { yd } => {
                if let Some(t) = target_of(m, cx) {
                    let d = [t.pos[0] - cx.pos[0], 0.0, t.pos[2] - cx.pos[2]];
                    let l2 = d[0] * d[0] + d[2] * d[2];
                    let mut v = cx.vel;
                    if l2 > 1.0e-7 {
                        let l = l2.sqrt();
                        v[0] = d[0] / l * 0.4 + cx.vel[0] * 0.2;
                        v[2] = d[2] / l * 0.4 + cx.vel[2] * 0.2;
                    }
                    v[1] = *yd as f64;
                    cx.vel = v;
                }
            }
            Goal::RangedBow { .. } => m.aggressive = true,
            Goal::Crossbow { .. } => m.aggressive = true,
            Goal::Swell { target } => {
                m.nav.stop();
                *target = m.target;
            }
            Goal::RestrictSun => m.nav.avoid_sun = true,
            Goal::FleeSun { speed, wanted } => {
                m.move_to(cx, *wanted, *speed);
            }
            Goal::EatBlock { tick } => {
                *tick = adj(40);
                m.event.push(10);
                m.nav.stop();
            }
            Goal::TryFindWater => {
                let p = cx.block_pos();
                'out: for x in p[0] - 2..=p[0] + 2 {
                    for y in p[1] - 2..=p[1] + 2 {
                        for z in p[2] - 2..=p[2] + 2 {
                            if crate::registry::vstate::is_water(cx.block([x, y, z])) {
                                m.mc.set_wanted([x as f64, y as f64, z as f64], 1.0);
                                break 'out;
                            }
                        }
                    }
                }
            }
            Goal::FollowOwner { recalc, old_water, .. } => {
                *recalc = 0;
                *old_water = m.malus[super::path::PathType::Water as usize];
                m.malus[super::path::PathType::Water as usize] = 0.0;
            }
            Goal::Sit => {
                m.nav.stop();
                m.sitting = true;
            }
            Goal::MoveTowardsTarget { speed, wanted, .. } | Goal::MoveTowardsRestriction { speed, wanted } => {
                m.move_to(cx, *wanted, *speed);
            }
            Goal::Special(s) => s.start(m, cx),
            Goal::Float | Goal::Breed { .. } | Goal::Ranged { .. } => {}
        }
    }

    pub fn stop(&mut self, m: &mut Mob, cx: &mut Ctx) {
        match self {
            Goal::Panic { running, .. } => *running = false,
            Goal::Breed { partner, love_time, .. } => {
                *partner = None;
                *love_time = 0;
            }
            Goal::Tempt { player, calm, .. } => {
                *player = None;
                m.nav.stop();
                if matches!(m.mc.kind, super::control::MoveKind::Flying { .. }) {
                    m.mc.op = super::control::MoveOp::Wait;
                }
                *calm = reduced(100);
            }
            Goal::FollowParent { parent, .. } => *parent = None,
            Goal::Stroll { .. } => m.nav.stop(),
            Goal::LookAt { at, .. } => *at = None,
            Goal::Melee { .. } => {
                if let Some(t) = target_of(m, cx) {
                    if !t.attackable_player() {
                        m.target = None;
                    }
                }
                m.aggressive = false;
                m.nav.stop();
                if m.ty.is_zombie() || m.ty.is("zombified_piglin") {
                    m.aggressive = false;
                }
            }
            Goal::Nearest { st, .. } | Goal::HurtBy { st, .. } | Goal::OwnerHurtBy { st, .. } | Goal::OwnerHurt { st, .. } => {
                m.target = None;
                st.target_mob = None;
            }
            Goal::Avoid { avoid, .. } => *avoid = None,
            Goal::RangedBow { see_time, attack_time, .. } => {
                m.aggressive = false;
                *see_time = 0;
                *attack_time = -1;
                m.living.using = None;
            }
            Goal::Ranged { see_time, attack_time, .. } => {
                m.target = None;
                *see_time = 0;
                *attack_time = -1;
            }
            Goal::Crossbow { see_time, state, .. } => {
                m.aggressive = false;
                m.target = None;
                *see_time = 0;
                if m.living.using.is_some() {
                    m.living.using = None;
                    m.v.charging = false;
                }
                *state = 0;
            }
            Goal::Swell { target } => *target = None,
            Goal::RestrictSun => m.nav.avoid_sun = false,
            Goal::EatBlock { tick } => *tick = 0,
            Goal::FollowOwner { old_water, .. } => {
                m.nav.stop();
                m.malus[super::path::PathType::Water as usize] = *old_water;
            }
            Goal::Sit => m.sitting = false,
            Goal::Special(s) => s.stop(m, cx),
            _ => {}
        }
    }

    pub fn tick(&mut self, m: &mut Mob, cx: &mut Ctx) {
        let every = self.every_tick();
        let adj = |n: i32| if every { n } else { reduced(n) };
        match self {
            Goal::Float => {
                if cx.rng.r#gen::<f32>() < 0.8 {
                    m.mo.jump_req = true;
                }
            }
            Goal::Breed { speed, partner, love_time } => {
                let Some(p) = partner.and_then(|p| cx.find(p)).cloned() else { return };
                m.look_at_seen(&p, 10.0, m.ty.max_head_x());
                m.move_to_entity(cx, p.pos, *speed);
                *love_time += 1;
                if *love_time >= adj(60) && dist2(cx.pos, p.pos) < 9.0 {
                    cx.acts.push(Act::Breed { a: cx.eid, b: p.eid });
                    *love_time = 60;
                }
            }
            Goal::Tempt { speed, stop, player, flying, .. } => {
                let Some(pl) = player.and_then(|e| cx.find(e)).cloned() else { return };
                m.look_at_seen(&pl, m.ty.max_head_y() + 20.0, m.ty.max_head_x());
                if dist2(cx.pos, pl.pos) < *stop * *stop {
                    if *flying {
                        m.mc.op = super::control::MoveOp::Wait;
                    } else {
                        m.nav.stop();
                    }
                } else if *flying {
                    let e = pl.eye_pos();
                    let r = cx.rng.r#gen::<f64>();
                    let t = [cx.pos[0] + (e[0] - cx.pos[0]) * r, cx.pos[1] + (e[1] - cx.pos[1]) * r, cx.pos[2] + (e[2] - cx.pos[2]) * r];
                    m.mc.set_wanted(t, *speed);
                } else {
                    m.move_to_entity(cx, pl.pos, *speed);
                }
            }
            Goal::FollowParent { speed, parent, recalc } => {
                *recalc -= 1;
                if *recalc <= 0 {
                    *recalc = adj(10);
                    if let Some(p) = parent.and_then(|p| cx.find(p)).cloned() {
                        m.move_to_entity(cx, p.pos, *speed);
                    }
                }
            }
            Goal::LookAt { at, time, only_h, .. } => {
                if let Some(t) = at.and_then(|a| cx.find(a)).cloned() {
                    if t.health > 0.0 {
                        let y = if *only_h { cx.pos[1] + m.eye() } else { t.eye_pos()[1] };
                        m.look_default([t.pos[0], y, t.pos[2]]);
                        *time -= 1;
                    }
                }
            }
            Goal::RandomLook { rel, time } => {
                *time -= 1;
                let e = m.eye();
                m.look_default([cx.pos[0] + rel[0], cx.pos[1] + e, cx.pos[2] + rel[1]]);
            }
            Goal::Melee { speed, unseen, kind, recalc, attack, pathed, ticks, .. } => {
                let Some(t) = target_of(m, cx).cloned() else { return };
                m.look_at_seen(&t, 30.0, 30.0);
                *recalc = (*recalc - 1).max(0);
                if (*unseen || m.can_see(cx, &t)) && *recalc <= 0 && (*pathed == [0.0; 3] || dist2(t.pos, *pathed) >= 1.0 || cx.rng.r#gen::<f32>() < 0.05) {
                    *pathed = t.pos;
                    *recalc = 4 + cx.rng.gen_range(0..7);
                    let d = dist2(cx.pos, t.pos);
                    if d > 1024.0 {
                        *recalc += 10;
                    } else if d > 256.0 {
                        *recalc += 5;
                    }
                    if !m.move_to_entity(cx, t.pos, *speed) {
                        *recalc += 15;
                    }
                    *recalc = adj(*recalc);
                }
                *attack = (*attack - 1).max(0);
                *ticks += 1;
                if matches!(kind, MeleeKind::Zombie | MeleeKind::Drowned) {
                    m.aggressive = *ticks >= 5 && *attack < adj(20) / 2;
                }
                if *attack <= 0 && m.in_melee_range(cx, &t) && m.can_see(cx, &t) {
                    *attack = adj(20);
                    m.swing = true;
                    cx.acts.push(Act::MobAttack { mob: cx.eid, target: t.eid });
                }
                if *kind == MeleeKind::PolarBear {
                    super::mobs::polar_bear_melee(m, cx, &t, attack);
                }
            }
            Goal::Avoid { avoid, walk, sprint, .. } => {
                if let Some(a) = avoid.and_then(|a| cx.find(a)) {
                    m.nav.speed = if dist2(cx.pos, a.pos) < 49.0 { *sprint } else { *walk };
                }
            }
            Goal::RangedBow { speed, interval, r2, attack_time, see_time, strafe_time, clockwise, backwards } => {
                let Some(t) = target_of(m, cx).cloned() else { return };
                let d = dist2(cx.pos, t.pos);
                let los = m.can_see(cx, &t);
                let was = *see_time > 0;
                if los != was {
                    *see_time = 0;
                }
                if los {
                    *see_time += 1;
                } else {
                    *see_time -= 1;
                }
                if d <= *r2 as f64 && *see_time >= 20 {
                    m.nav.stop();
                    *strafe_time += 1;
                } else {
                    m.move_to_entity(cx, t.pos, *speed);
                    *strafe_time = -1;
                }
                if *strafe_time >= 20 {
                    if cx.rng.r#gen::<f32>() < 0.3 {
                        *clockwise = !*clockwise;
                    }
                    if cx.rng.r#gen::<f32>() < 0.3 {
                        *backwards = !*backwards;
                    }
                    *strafe_time = 0;
                }
                if *strafe_time > -1 {
                    if d > (*r2 * 0.75) as f64 {
                        *backwards = false;
                    } else if d < (*r2 * 0.25) as f64 {
                        *backwards = true;
                    }
                    m.mc.strafe(if *backwards { -0.5 } else { 0.5 }, if *clockwise { 0.5 } else { -0.5 });
                    m.turn_to(cx, t.pos, 30.0, 30.0);
                } else {
                    m.look_at_seen(&t, 30.0, 30.0);
                }
                if let Some(u) = m.living.using {
                    if !los && *see_time < -60 {
                        m.living.using = None;
                    } else if los && u.elapsed >= 20 {
                        m.living.using = None;
                        super::mobs::bow_shot(m, cx, &t, 1.0);
                        *attack_time = *interval;
                    }
                } else {
                    *attack_time -= 1;
                    if *attack_time <= 0 && *see_time >= -60 {
                        super::mobs::start_using(m, super::mobs::bow_hand(m));
                    }
                }
            }
            Goal::Ranged { speed, min, max, radius, attack_time, see_time } => {
                let Some(t) = target_of(m, cx).cloned() else { return };
                let d = dist2(cx.pos, [t.pos[0], t.pos[1], t.pos[2]]);
                let los = m.can_see(cx, &t);
                if los {
                    *see_time += 1;
                } else {
                    *see_time = 0;
                }
                if d <= (*radius * *radius) as f64 && *see_time >= 5 {
                    m.nav.stop();
                } else {
                    m.move_to_entity(cx, t.pos, *speed);
                }
                m.look_at_seen(&t, 30.0, 30.0);
                *attack_time -= 1;
                if *attack_time == 0 {
                    if !los {
                        return;
                    }
                    let f = d.sqrt() as f32 / *radius;
                    let power = f.clamp(0.1, 1.0);
                    super::mobs::ranged_attack(m, cx, &t, power);
                    *attack_time = (f * (*max - *min) as f32 + *min as f32).floor() as i32;
                } else if *attack_time < 0 {
                    let f = d.sqrt() as f32 / *radius;
                    *attack_time = (*min as f32 + f * (*max - *min) as f32).floor() as i32;
                }
            }
            Goal::Crossbow { speed, r2, state, see_time, attack_delay, update_path } => {
                let Some(t) = target_of(m, cx).cloned() else { return };
                let los = m.can_see(cx, &t);
                let was = *see_time > 0;
                if los != was {
                    *see_time = 0;
                }
                if los {
                    *see_time += 1;
                } else {
                    *see_time -= 1;
                }
                let d = dist2(cx.pos, t.pos);
                let can_shoot = *state == 3 || *state == 4;
                let far = (d > *r2 as f64 || *see_time < 5) && *attack_delay == 0;
                if far {
                    *update_path -= 1;
                    if *update_path <= 0 {
                        let sp = if can_shoot { *speed * 0.5 } else { *speed };
                        m.move_to_entity(cx, t.pos, sp);
                        *update_path = 20 + cx.rng.gen_range(0..20);
                    }
                } else {
                    *update_path = 0;
                    m.nav.stop();
                }
                m.look_at_seen(&t, 30.0, 30.0);
                match *state {
                    0 => {
                        if !far {
                            super::mobs::start_using(m, super::mobs::crossbow_hand(m));
                            *state = 1;
                            m.v.charging = true;
                        }
                    }
                    1 => {
                        if m.living.using.is_none() {
                            *state = 0;
                        }
                        let charge = 25; // CrossbowItem.getChargeDuration without quick charge
                        if m.living.using.is_some_and(|u| u.elapsed >= charge) {
                            m.living.using = None;
                            *state = 2;
                            *attack_delay = 20 + cx.rng.gen_range(0..20);
                            m.v.charging = false;
                        }
                    }
                    2 => {
                        *attack_delay -= 1;
                        if *attack_delay == 0 {
                            *state = 3;
                        }
                    }
                    3 if los => {
                        super::mobs::crossbow_shot(m, cx, &t);
                        *state = 0;
                    }
                    _ => {}
                }
            }
            Goal::Swell { target } => {
                let t = target.and_then(|t| cx.find(t)).cloned();
                m.v.swell_dir = match t {
                    None => -1,
                    Some(t) if dist2(cx.pos, t.pos) > 49.0 => -1,
                    Some(t) if !m.can_see(cx, &t) => -1,
                    Some(_) => 1,
                };
            }
            Goal::EatBlock { tick } => {
                *tick = (*tick - 1).max(0);
                if *tick == adj(4) {
                    let p = cx.block_pos();
                    let below = [p[0], p[1] - 1, p[2]];
                    if cx.block(p).name() == "short_grass" {
                        if cx.mob_griefing {
                            cx.acts.push(Act::Destroy { pos: p, drops: false });
                        }
                        super::mobs::ate(m, cx);
                    } else if cx.block(below).name() == "grass_block" {
                        if cx.mob_griefing {
                            cx.acts.push(Act::LevelEvent { ev: 2001, pos: below, data: crate::game::mobtick::state_id(cx.block(below)) });
                            cx.acts.push(Act::SetBlock { pos: below, state: crate::registry::BlockState::by_name("dirt"), drops: false });
                        }
                        super::mobs::ate(m, cx);
                    }
                }
                m.v.eat_tick = *tick;
            }
            Goal::FollowOwner { speed, recalc, .. } => {
                let Some(o) = owner_of(m, cx).cloned() else { return };
                let far = dist2(cx.pos, o.pos) >= 144.0;
                if !far {
                    m.look_at_seen(&o, 10.0, m.ty.max_head_x());
                }
                *recalc -= 1;
                if *recalc <= 0 {
                    *recalc = adj(10);
                    if far {
                        super::mobs::teleport_to_owner(m, cx, &o);
                    } else {
                        m.move_to_entity(cx, o.pos, *speed);
                    }
                }
            }
            Goal::Special(s) => s.tick(m, cx),
            _ => {}
        }
    }
}

/// `OwnerHurtByTargetGoal` / `OwnerHurtTargetGoal.canUse`
fn owner_hurt_can_use(m: &mut Mob, cx: &mut Ctx, stamp: &mut i32, st: &mut TState, hurt: &mut Option<i32>, by: bool) -> bool {
    if !m.tame || m.ordered_sit {
        return false;
    }
    let Some(owner) = owner_of(m, cx).cloned() else { return false };
    let rec = if by { owner.last_hurt_by } else { owner.last_hurt };
    let Some((eid, at)) = rec else { return false };
    if at == *stamp {
        return false;
    }
    let Some(t) = cx.find(eid).cloned() else { return false };
    if !m.can_target(cx, &t, true, 0.0, false, true) || !super::mobs::wants_to_attack(m, &t, &owner) {
        return false;
    }
    *hurt = Some(t.eid);
    *stamp = at;
    target_ok(m, cx, &t, st)
}

fn self_can_use_bow(m: &Mob, cx: &Ctx) -> bool {
    target_of(m, cx).is_some()
}

/// `FloatGoal`: `getFluidJumpThreshold`
fn fluid_jump_threshold(m: &Mob) -> f64 {
    if m.eye() < 0.4 { 0.0 } else { 0.4 }
}

/// `PanicGoal.shouldPanic` (`panic_causes` within the last 40 ticks)
pub fn should_panic(m: &Mob) -> bool {
    let Some((src, at)) = &m.living.last_damage else { return false };
    if m.living.tick_count - at > 40 {
        return false;
    }
    let env = ["cactus", "freeze", "hot_floor", "in_fire", "lava", "lightning_bolt", "on_fire"];
    let other = ["arrow", "dragon_breath", "explosion", "fireball", "fireworks", "indirect_magic", "magic", "mob_attack", "mob_projectile", "player_explosion", "sonic_boom", "sting", "thrown", "trident", "unattributed_fireball", "wind_charge", "wither", "wither_skull", "player_attack", "spear"];
    env.contains(&src.ty) || other.contains(&src.ty)
}

/// `PanicGoal.lookForWater`
fn look_for_water(m: &Mob, cx: &Ctx, d: i32) -> Option<[i32; 3]> {
    let p = cx.block_pos();
    if crate::game::physics::has_collision(cx.block(p)) {
        return None;
    }
    let _ = m;
    // BlockPos.findClosestMatch: by distance shells
    let mut best: Option<(i32, [i32; 3])> = None;
    for dy in -1..=1 {
        for dx in -d..=d {
            for dz in -d..=d {
                let q = [p[0] + dx, p[1] + dy, p[2] + dz];
                if crate::registry::vstate::is_water(cx.block(q)) {
                    let dd = dx * dx + dy * dy + dz * dz;
                    if best.is_none_or(|b| dd < b.0) {
                        best = Some((dd, q));
                    }
                }
            }
        }
    }
    best.map(|b| b.1)
}

/// `BreedGoal.getFreePartner`
fn free_partner(m: &Mob, cx: &Ctx) -> Option<i32> {
    let bb = crate::anticheat::combat::Aabb::entity(cx.pos, m.width(), m.height()).inflate(8.0, 8.0, 8.0);
    let mut best: Option<(f64, i32)> = None;
    for s in cx.seen.iter() {
        if s.eid == cx.eid || !s.bbox().intersects(&bb) || dist2(cx.pos, s.pos) > 64.0 {
            continue;
        }
        if !super::mobs::can_mate(m, s) || s.panicking {
            continue;
        }
        let d = dist2(cx.pos, s.pos);
        if best.is_none_or(|b| d < b.0) {
            best = Some((d, s.eid));
        }
    }
    best.map(|b| b.1)
}

fn owner_of<'a>(m: &Mob, cx: &'a Ctx) -> Option<&'a Seen> {
    let u = m.owner?;
    cx.seen.iter().find(|s| s.uuid == u)
}

/// `TamableAnimal.unableToMoveToOwner`
fn unable_to_move_to_owner(m: &Mob) -> bool {
    m.ordered_sit
}

/// `FleeSunGoal.getHidePos`
fn hiding_place(m: &Mob, cx: &mut Ctx) -> Option<[f64; 3]> {
    let p = cx.block_pos();
    for _ in 0..10 {
        let q = [p[0] + cx.rng.gen_range(0..20) - 10, p[1] + cx.rng.gen_range(0..6) - 3, p[2] + cx.rng.gen_range(0..20) - 10];
        if !cx.sky(q) && m.walk_value(cx, q) < 0.0 {
            return Some([q[0] as f64, q[1] as f64, q[2] as f64]);
        }
    }
    None
}

fn target_start(st: &mut TState) {
    st.reach_cache = 0;
    st.reach_time = 0;
    st.unseen = 0;
}

/// `TargetGoal.canAttack` (home + reach check; conditions are tested by the caller)
fn target_ok(m: &mut Mob, cx: &mut Ctx, t: &Seen, st: &mut TState) -> bool {
    if !m.within_home(bp(t.pos)) {
        return false;
    }
    if st.must_reach {
        st.reach_time -= 1;
        if st.reach_time <= 0 {
            st.reach_cache = 0;
        }
        if st.reach_cache == 0 {
            st.reach_time = reduced(10 + cx.rng.gen_range(0..5));
            let ok = match m.create_path_to(cx, t.pos, 0) {
                Some(p) => p.end().is_some_and(|n| {
                    let (dx, dz) = (n.x - bp(t.pos)[0], n.z - bp(t.pos)[2]);
                    ((dx * dx + dz * dz) as f64) <= 2.25
                }),
                None => false,
            };
            st.reach_cache = if ok { 1 } else { 2 };
        }
        if st.reach_cache == 2 {
            return false;
        }
    }
    true
}

/// `TargetGoal.canContinueToUse`
fn target_continue(m: &mut Mob, cx: &mut Ctx, st: &mut TState) -> bool {
    let t = m.target.or(st.target_mob);
    let Some(t) = t.and_then(|t| cx.find(t)).cloned() else { return false };
    if t.health <= 0.0 || t.spectator() || (t.player && !t.attackable_player()) {
        return false;
    }
    if !m.can_attack_type(&t) {
        return false;
    }
    let within = st.follow.unwrap_or_else(|| m.follow_range());
    if dist2(cx.pos, t.pos) > within * within {
        return false;
    }
    if st.must_see {
        if m.can_see(cx, &t) {
            st.unseen = 0;
        } else {
            st.unseen += 1;
            if st.unseen > reduced(st.unseen_memory) {
                return false;
            }
        }
    }
    m.target = Some(t.eid);
    true
}

/// `HurtByTargetGoal.alertOthers`
fn alert_others(m: &mut Mob, cx: &mut Ctx, attacker: i32, ignore: &[&str]) {
    let within = m.follow_range();
    let p = cx.pos;
    let bb = crate::anticheat::combat::Aabb { min: [p[0].floor() - within, p[1].floor() - 10.0, p[2].floor() - within], max: [p[0].floor() + 1.0 + within, p[1].floor() + 11.0, p[2].floor() + 1.0 + within] };
    for s in cx.seen.iter() {
        if s.eid == cx.eid || s.player || s.ty != m.ty.name() || s.target.is_some() || !s.bbox().intersects(&bb) {
            continue;
        }
        if m.ty.tamable() && s.owner != m.owner {
            continue;
        }
        if ignore.contains(&s.ty) {
            continue;
        }
        cx.acts.push(Act::SetTarget { eid: s.eid, target: attacker });
    }
}


pub use super::Slot;

/// Shorthands used by `mobs::setup`.
pub mod mk {
    use super::*;

    pub fn float() -> Goal {
        Goal::Float
    }
    pub fn panic(speed: f64) -> Goal {
        Goal::Panic { speed, pos: [0.0; 3], running: false }
    }
    pub fn breed(speed: f64) -> Goal {
        Goal::Breed { speed, partner: None, love_time: 0 }
    }
    pub fn tempt(speed: f64, items: &'static str, scare: bool) -> Goal {
        Goal::Tempt { speed, items, scare, stop: 2.5, player: None, calm: 0, p: [0.0; 3], rot: (0.0, 0.0), flying: false }
    }
    pub fn tempt_fly(speed: f64, items: &'static str, stop: f64) -> Goal {
        Goal::Tempt { speed, items, scare: false, stop, player: None, calm: 0, p: [0.0; 3], rot: (0.0, 0.0), flying: true }
    }
    pub fn follow_parent(speed: f64) -> Goal {
        Goal::FollowParent { speed, parent: None, recalc: 0 }
    }
    pub fn stroll(speed: f64) -> Goal {
        Goal::Stroll { speed, interval: 120, kind: StrollKind::Default, check_no_action: true, wanted: [0.0; 3], force: false }
    }
    pub fn stroll_i(speed: f64, interval: i32) -> Goal {
        Goal::Stroll { speed, interval, kind: StrollKind::Default, check_no_action: true, wanted: [0.0; 3], force: false }
    }
    pub fn wander(speed: f64) -> Goal {
        Goal::Stroll { speed, interval: 120, kind: StrollKind::WaterAvoiding(0.001), check_no_action: true, wanted: [0.0; 3], force: false }
    }
    pub fn wander_p(speed: f64, p: f32) -> Goal {
        Goal::Stroll { speed, interval: 120, kind: StrollKind::WaterAvoiding(p), check_no_action: true, wanted: [0.0; 3], force: false }
    }
    pub fn swim(speed: f64, interval: i32) -> Goal {
        Goal::Stroll { speed, interval, kind: StrollKind::Swim, check_no_action: true, wanted: [0.0; 3], force: false }
    }
    pub fn fly(speed: f64) -> Goal {
        Goal::Stroll { speed, interval: 120, kind: StrollKind::Fly, check_no_action: true, wanted: [0.0; 3], force: false }
    }
    pub fn golem_stroll(speed: f64) -> Goal {
        Goal::Stroll { speed, interval: 240, kind: StrollKind::Golem, check_no_action: false, wanted: [0.0; 3], force: false }
    }
    pub fn look_player(range: f32) -> Goal {
        Goal::LookAt { range, prob: 0.02, sel: any, players: true, only_h: false, at: None, time: 0 }
    }
    pub fn look_player_p(range: f32, prob: f32) -> Goal {
        Goal::LookAt { range, prob, sel: any, players: true, only_h: false, at: None, time: 0 }
    }
    pub fn look_mob(range: f32, sel: Sel) -> Goal {
        Goal::LookAt { range, prob: 0.02, sel, players: false, only_h: false, at: None, time: 0 }
    }
    pub fn random_look() -> Goal {
        Goal::RandomLook { rel: [0.0; 2], time: 0 }
    }
    pub fn melee(speed: f64, unseen: bool) -> Goal {
        melee_k(speed, unseen, MeleeKind::Plain)
    }
    pub fn melee_k(speed: f64, unseen: bool, kind: MeleeKind) -> Goal {
        Goal::Melee { speed, unseen, kind, recalc: 0, attack: 0, pathed: [0.0; 3], last_check: -100, ticks: 0, has_path: false }
    }
    pub fn target_players(must_see: bool) -> Goal {
        Goal::Nearest { sel: any, players: true, interval: reduced(10), found: None, st: TState::new(must_see, false), only_dark: false }
    }
    pub fn target(sel: Sel, must_see: bool) -> Goal {
        Goal::Nearest { sel, players: false, interval: reduced(10), found: None, st: TState::new(must_see, false), only_dark: false }
    }
    pub fn target_full(sel: Sel, players: bool, interval: i32, must_see: bool, must_reach: bool) -> Goal {
        Goal::Nearest { sel, players, interval: reduced(interval), found: None, st: TState::new(must_see, must_reach), only_dark: false }
    }
    pub fn hurt_by() -> Goal {
        Goal::HurtBy { alert: false, ignore: &[], alert_ignore: &[], stamp: 0, st: TState::new(true, false) }
    }
    pub fn hurt_by_ignore(ignore: &'static [&'static str]) -> Goal {
        Goal::HurtBy { alert: false, ignore, alert_ignore: &[], stamp: 0, st: TState::new(true, false) }
    }
    pub fn hurt_by_alert(ignore: &'static [&'static str], alert_ignore: &'static [&'static str]) -> Goal {
        Goal::HurtBy { alert: true, ignore, alert_ignore, stamp: 0, st: TState::new(true, false) }
    }
    pub fn owner_hurt_by() -> Goal {
        Goal::OwnerHurtBy { stamp: 0, st: TState::new(false, false), hurt: None }
    }
    pub fn owner_hurt() -> Goal {
        Goal::OwnerHurt { stamp: 0, st: TState::new(false, false), hurt: None }
    }
    pub fn avoid(sel: Sel, dist: f32, walk: f64, sprint: f64) -> Goal {
        Goal::Avoid { sel, dist, walk, sprint, avoid: None, path: None }
    }
    pub fn leap(yd: f32) -> Goal {
        Goal::Leap { yd }
    }
    pub fn bow(speed: f64, interval: i32, radius: f32) -> Goal {
        Goal::RangedBow { speed, interval, r2: radius * radius, attack_time: -1, see_time: 0, strafe_time: -1, clockwise: false, backwards: false }
    }
    pub fn ranged(speed: f64, min: i32, max: i32, radius: f32) -> Goal {
        Goal::Ranged { speed, min, max, radius, attack_time: -1, see_time: 0 }
    }
    pub fn crossbow(speed: f64, radius: f32) -> Goal {
        Goal::Crossbow { speed, r2: radius * radius, state: 0, see_time: 0, attack_delay: 0, update_path: 0 }
    }
    pub fn swell() -> Goal {
        Goal::Swell { target: None }
    }
    pub fn restrict_sun() -> Goal {
        Goal::RestrictSun
    }
    pub fn flee_sun(speed: f64) -> Goal {
        Goal::FleeSun { speed, wanted: [0.0; 3] }
    }
    pub fn eat_block() -> Goal {
        Goal::EatBlock { tick: 0 }
    }
    pub fn try_find_water() -> Goal {
        Goal::TryFindWater
    }
    pub fn follow_owner(speed: f64, start: f32, stop: f32) -> Goal {
        Goal::FollowOwner { speed, start, stop, recalc: 0, old_water: 0.0 }
    }
    pub fn sit() -> Goal {
        Goal::Sit
    }
    pub fn towards_target(speed: f64, within: f32) -> Goal {
        Goal::MoveTowardsTarget { speed, within, wanted: [0.0; 3] }
    }
    pub fn towards_restriction(speed: f64) -> Goal {
        Goal::MoveTowardsRestriction { speed, wanted: [0.0; 3] }
    }
    pub fn special(s: super::super::mobs::Special) -> Goal {
        Goal::Special(Box::new(s))
    }
}
