//! Mob AI: vanilla's `GoalSelector` (priorities + MOVE / LOOK / JUMP / TARGET
//! flags), `Sensing`, `TargetingConditions`, the random position helpers
//! (`DefaultRandomPos`, `LandRandomPos`, `AirRandomPos`...), pathfinding
//! (`path`), the move / look / jump controls and `travel` (`control`),
//! the goal library (`goals`) and per-type setups and behaviours (`mobs`).

pub mod control;
pub mod custom;
pub mod goals;
pub mod mobs;
pub mod path;
pub mod special;

use std::collections::HashMap;

use rand::Rng;

use super::mob::Mob;
use super::mobtick::Act;
use super::Game;
use crate::anticheat::combat::Aabb;
use crate::data::Stack;
use crate::registry::{blocks, BlockState};

pub const MOVE: u8 = 1;
pub const LOOK: u8 = 2;
pub const JUMP: u8 = 4;
pub const TARGET: u8 = 8;

/// What a mob can see of another living entity this tick.
#[derive(Clone, Debug)]
pub struct Seen {
    pub eid: i32,
    pub uuid: uuid::Uuid,
    pub ty: &'static str,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub width: f64,
    pub height: f64,
    pub eye: f64,
    pub player: bool,
    /// player game mode (-1 for mobs)
    pub gamemode: i32,
    pub yaw: f32,
    pub pitch: f32,
    pub hands: [Option<Stack>; 2],
    pub head: Option<Stack>,
    pub baby: bool,
    pub in_love: bool,
    pub age: i32,
    pub owner: Option<uuid::Uuid>,
    pub tame: bool,
    pub target: Option<i32>,
    pub invisible: bool,
    pub health: f32,
    pub monster: bool,
    pub panicking: bool,
    pub sleeping: bool,
    pub armor_cover: f32,
    pub on_ground: bool,
    pub in_water: bool,
    pub swimming: bool,
    pub sprinting: bool,
    pub in_vehicle: bool,
    /// `!level.isBrightOutside()` (same for everyone)
    pub night: bool,
    pub undead: bool,
    pub raider: bool,
    /// armadillo rolled up
    pub scared: bool,
    pub wears_gold: bool,
    pub size: i32,
    /// sheep wool colour
    pub color: u8,
    pub effects: Vec<&'static str>,
    /// `getLastHurtByMob` (eid, tick)
    pub last_hurt_by: Option<(i32, i32)>,
    /// `getLastHurtMob` (eid, tick)
    pub last_hurt: Option<(i32, i32)>,
}

impl Seen {
    pub fn bbox(&self) -> Aabb {
        Aabb::entity(self.pos, self.width, self.height)
    }
    pub fn eye_pos(&self) -> [f64; 3] {
        [self.pos[0], self.pos[1] + self.eye, self.pos[2]]
    }
    /// players in survival / adventure (`NO_CREATIVE_OR_SPECTATOR`)
    pub fn attackable_player(&self) -> bool {
        !self.player || self.gamemode == 0 || self.gamemode == 2
    }
    pub fn spectator(&self) -> bool {
        self.player && self.gamemode == 3
    }
    pub fn holding_id(&self, f: impl Fn(i32) -> bool) -> bool {
        self.hands.iter().flatten().any(|s| f(s.item))
    }
    pub fn holding(&self, f: impl Fn(&str) -> bool) -> bool {
        self.hands.iter().flatten().any(|s| f(crate::data::Items::get().name(s.item)))
    }
}

/// Everything a goal needs besides the mob itself.
pub struct Ctx<'a> {
    pub game: &'a Game,
    pub get: &'a dyn Fn(i32, i32, i32) -> Option<BlockState>,
    pub seen: &'a [Seen],
    pub now: i64,
    pub eid: i32,
    pub uuid: uuid::Uuid,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub on_ground: bool,
    pub acts: &'a mut Vec<Act>,
    pub rng: rand::rngs::ThreadRng,
    pub day: bool,
    pub raining: bool,
    pub thundering: bool,
    pub difficulty: i32,
    pub los: HashMap<i32, bool>,
    pub mob_griefing: bool,
}

impl Ctx<'_> {
    pub fn block(&self, p: [i32; 3]) -> BlockState {
        (self.get)(p[0], p[1], p[2]).unwrap_or(blocks::AIR)
    }

    pub fn find(&self, eid: i32) -> Option<&Seen> {
        self.seen.iter().find(|s| s.eid == eid)
    }

    pub fn block_pos(&self) -> [i32; 3] {
        bp(self.pos)
    }

    /// `Level.canSeeSky`
    pub fn sky(&self, p: [i32; 3]) -> bool {
        self.game.world.surface_y(p[0], p[2]).is_none_or(|t| p[1] > t)
    }

    /// `Level.isBrightOutside` (day and not raining or thundering heavily)
    pub fn bright(&self) -> bool {
        self.day && !self.raining
    }

    /// `getLightLevelDependentMagicValue - 0.5` (`getPathfindingCostFromLightLevels`)
    pub fn light_cost(&self, p: [i32; 3]) -> f32 {
        let darken = self.game.sky_darken();
        let b = self.game.raw_brightness(p, darken).clamp(0, 15) as f32 / 15.0;
        b / (4.0 - 3.0 * b) - 0.5
    }
}

pub fn bp(p: [f64; 3]) -> [i32; 3] {
    [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32]
}

pub fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// `Goal.reducedTickDelay`
pub fn reduced(n: i32) -> i32 {
    (n + 1) / 2
}

// ---------------------------------------------------------------- sensing

impl Mob {
    pub fn eye(&self) -> f64 {
        self.ty.eye_height() * if self.is_baby() { 0.5 } else { 1.0 }
    }

    pub fn width(&self) -> f64 {
        self.ty.size_of(self).0
    }

    pub fn height(&self) -> f64 {
        self.ty.size_of(self).1
    }

    /// `Sensing.hasLineOfSight` (cached per tick)
    pub fn can_see(&self, cx: &mut Ctx, t: &Seen) -> bool {
        if let Some(v) = cx.los.get(&t.eid) {
            return *v;
        }
        let from = [cx.pos[0], cx.pos[1] + self.eye(), cx.pos[2]];
        let to = t.eye_pos();
        let v = dist2(from, to) < 128.0 * 128.0 && crate::game::physics::clip(&control::World(cx.get), from, to).is_none();
        cx.los.insert(t.eid, v);
        v
    }

    pub fn follow_range(&self) -> f64 {
        self.living.attrs.peek(super::combat::attr::Attr::FollowRange)
    }

    /// `TargetingConditions.test` with this mob as the targeter.
    pub fn can_target(&self, cx: &mut Ctx, t: &Seen, combat: bool, range: f64, los: bool, test_invisible: bool) -> bool {
        if t.eid == cx.eid || t.spectator() || t.health <= 0.0 {
            return false;
        }
        if combat {
            if t.player && !t.attackable_player() {
                return false;
            }
            if !self.can_attack_type(t) {
                return false;
            }
            if t.player && cx.difficulty == 0 {
                return false;
            }
        }
        if range > 0.0 {
            let mut m = if test_invisible { visibility(t, self.ty.name()) } else { 1.0 };
            if !test_invisible {
                m = 1.0;
            }
            let vis = (range * m).max(2.0);
            if dist2(cx.pos, t.pos) > vis * vis {
                return false;
            }
        }
        if los && !self.can_see(cx, t) {
            return false;
        }
        true
    }

    /// `LivingEntity.canAttack` (+ the common overrides)
    pub fn can_attack_type(&self, t: &Seen) -> bool {
        if t.ty == "armor_stand" || t.ty == "mannequin" {
            return false;
        }
        // allied illagers / undead don't fight each other
        if self.ty.is_raider() && super::mob::MobType::from_name(t.ty).is_some_and(|o| o.is_raider()) {
            return false;
        }
        if self.ty.name() == "ghast" && t.ty == "ghast" {
            return false;
        }
        true
    }

    /// Nearest entity matching `f` within `range` (inflate box `range`, `vy` vertical).
    pub fn nearest<'s>(&self, cx: &mut Ctx<'s>, range: f64, vy: f64, combat: bool, los: bool, f: &dyn Fn(&Seen) -> bool) -> Option<Seen> {
        let mut best: Option<(f64, Seen)> = None;
        let seen: &'s [Seen] = cx.seen;
        let eye = [cx.pos[0], cx.pos[1] + self.eye(), cx.pos[2]];
        let (w, h) = (self.width(), self.height());
        let bb = Aabb::entity(cx.pos, w, h).inflate(range, vy, range);
        for s in seen {
            if !s.bbox().intersects(&bb) || !f(s) {
                continue;
            }
            if !self.can_target(cx, s, combat, range, los, true) {
                continue;
            }
            let d = dist2(eye, s.pos);
            if best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, s.clone()));
            }
        }
        best.map(|b| b.1)
    }

    /// Nearest player (`getNearestPlayer` with conditions).
    pub fn nearest_player(&self, cx: &mut Ctx, range: f64, combat: bool, los: bool, f: &dyn Fn(&Seen) -> bool) -> Option<Seen> {
        let seen = cx.seen;
        let eye = [cx.pos[0], cx.pos[1] + self.eye(), cx.pos[2]];
        let mut best: Option<(f64, Seen)> = None;
        for s in seen.iter().filter(|s| s.player) {
            if !f(s) || !self.can_target(cx, s, combat, range, los, true) {
                continue;
            }
            let d = dist2(eye, s.pos);
            if best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, s.clone()));
            }
        }
        best.map(|b| b.1)
    }
}

/// `LivingEntity.getVisibilityPercent`
pub fn visibility(t: &Seen, by: &str) -> f64 {
    let mut v = 1.0;
    if t.invisible {
        v *= 0.7 * (t.armor_cover.max(0.1) as f64);
    }
    if let Some(h) = t.head {
        let n = crate::data::Items::get().name(h.item);
        let half = matches!((by, n), ("skeleton", "skeleton_skull") | ("zombie", "zombie_head") | ("piglin" | "piglin_brute", "piglin_head") | ("creeper", "creeper_head"));
        if half {
            v *= 0.5;
        }
    }
    v
}

// ---------------------------------------------------------------- navigation helpers

impl Mob {
    pub fn nav_mob(&mut self, cx: &Ctx) -> path::NavMob {
        let (w, h) = (self.width(), self.height());
        let in_water = self.mo.in_water;
        let max_fall = self.max_fall_distance(cx);
        path::NavMob {
            pos: cx.pos,
            width: w,
            height: h,
            on_ground: cx.on_ground,
            in_water,
            in_liquid: in_water || self.mo.in_lava,
            passenger: false,
            speed: self.mo.speed,
            follow_range: self.follow_range(),
            path: path::PathMob {
                pos: cx.pos,
                width: w,
                height: h,
                on_ground: cx.on_ground,
                in_water,
                max_up_step: self.max_up_step,
                max_fall,
                malus: self.malus,
                can_open_doors: false,
                can_pass_doors: true,
                can_float: false,
                can_walk_over_fences: false,
            },
        }
    }

    /// `Mob.getMaxFallDistance`
    pub fn max_fall_distance(&mut self, cx: &Ctx) -> i32 {
        let safe = self.living.attrs.value(super::combat::attr::Attr::SafeFallDistance) as f32;
        if self.target.is_none() {
            return (safe).floor() as i32;
        }
        let max = self.living.max_health();
        let mut sacrifice = (self.living.health - max * 0.33) as i32;
        sacrifice -= (3 - cx.difficulty) * 4;
        (sacrifice.max(0) as f32 + safe).floor() as i32
    }

    pub fn move_to(&mut self, cx: &mut Ctx, to: [f64; 3], speed: f64) -> bool {
        self.move_to_reach(cx, to, 1, speed)
    }

    pub fn move_to_reach(&mut self, cx: &mut Ctx, to: [f64; 3], reach: i32, speed: f64) -> bool {
        let nm = self.nav_mob(cx);
        let game = cx.game;
        let sky = |x: i32, y: i32, z: i32| game.world.surface_y(x, z).is_none_or(|t| y > t);
        self.nav.move_to(&nm, cx.get, to, reach, speed, cx.now, &sky)
    }

    pub fn move_to_entity(&mut self, cx: &mut Ctx, t: [f64; 3], speed: f64) -> bool {
        let nm = self.nav_mob(cx);
        let game = cx.game;
        let sky = |x: i32, y: i32, z: i32| game.world.surface_y(x, z).is_none_or(|t| y > t);
        self.nav.move_to_entity(&nm, cx.get, t, speed, cx.now, &sky)
    }

    pub fn create_path_to(&mut self, cx: &mut Ctx, t: [f64; 3], reach: i32) -> Option<path::Path> {
        let nm = self.nav_mob(cx);
        self.nav.create_path(&nm, cx.get, bp(t), reach, cx.now)
    }

    pub fn follow_path(&mut self, cx: &mut Ctx, p: Option<path::Path>, speed: f64) -> bool {
        let nm = self.nav_mob(cx);
        let game = cx.game;
        let sky = |x: i32, y: i32, z: i32| game.world.surface_y(x, z).is_none_or(|t| y > t);
        self.nav.move_to_path(&nm, cx.get, p, speed, &sky)
    }

    pub fn look_at(&mut self, p: [f64; 3], y_max: f32, x_max: f32) {
        self.look.set(p, y_max, x_max);
    }

    pub fn look_at_seen(&mut self, t: &Seen, y_max: f32, x_max: f32) {
        self.look.set(t.eye_pos(), y_max, x_max);
    }

    pub fn look_default(&mut self, p: [f64; 3]) {
        let (y, x) = (self.ty.head_rot_speed(), self.ty.max_head_x());
        self.look.set(p, y, x);
    }

    /// `Mob.lookAt(entity, maxY, maxX)`: turns the body instantly.
    pub fn turn_to(&mut self, cx: &Ctx, t: [f64; 3], y_max: f32, x_max: f32) {
        let (xd, zd) = (t[0] - cx.pos[0], t[2] - cx.pos[2]);
        let yd = t[1] - (cx.pos[1] + self.eye());
        let sd = (xd * xd + zd * zd).sqrt();
        let yrot = zd.atan2(xd).to_degrees() as f32 - 90.0;
        let xrot = -(yd.atan2(sd).to_degrees() as f32);
        self.mo.pitch = control::rotlerp(self.mo.pitch, xrot, x_max);
        self.mo.yaw = control::rotlerp(self.mo.yaw, yrot, y_max);
    }

    /// `Mob.isWithinMeleeAttackRange`
    pub fn in_melee_range(&self, cx: &Ctx, t: &Seen) -> bool {
        let reach = (2.04f32.sqrt() - 0.6) as f64;
        let bb = Aabb::entity(cx.pos, self.width(), self.height()).inflate(reach, 0.0, reach);
        bb.intersects(&t.bbox())
    }

    pub fn has_home(&self) -> bool {
        self.home.is_some()
    }

    pub fn within_home(&self, p: [i32; 3]) -> bool {
        match self.home {
            None => true,
            Some((h, r)) => {
                let d = dist2([h[0] as f64 + 0.5, h[1] as f64 + 0.5, h[2] as f64 + 0.5], [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5]);
                d < (r * r) as f64
            }
        }
    }

    /// `PathfinderMob.getWalkTargetValue`
    pub fn walk_value(&self, cx: &Ctx, p: [i32; 3]) -> f32 {
        let below = cx.block([p[0], p[1] - 1, p[2]]);
        match self.ty.walk_kind() {
            WalkKind::Animal => {
                if below.name() == "grass_block" {
                    10.0
                } else {
                    cx.light_cost(p)
                }
            }
            WalkKind::Monster => -cx.light_cost(p),
            WalkKind::Custom(f) => f(cx, p),
            WalkKind::None => 0.0,
        }
    }

    fn path_type_at(&self, cx: &Ctx, p: [i32; 3]) -> path::PathType {
        let t = path::state_type(cx.block(p));
        if t == path::PathType::Open {
            match path::state_type(cx.block([p[0], p[1] - 1, p[2]])) {
                path::PathType::Open | path::PathType::Water | path::PathType::Lava | path::PathType::Walkable => path::PathType::Open,
                path::PathType::Fire => path::PathType::Fire,
                path::PathType::Damaging => path::PathType::Damaging,
                path::PathType::Blocked | path::PathType::Fence | path::PathType::Leaves => path::PathType::Walkable,
                o => o,
            }
        } else {
            t
        }
    }

    fn malus_of(&self, t: path::PathType) -> f32 {
        let m = self.malus[t as usize];
        if m.is_nan() { t.malus() } else { m }
    }

    fn stable(&self, cx: &Ctx, p: [i32; 3]) -> bool {
        match self.nav.kind {
            path::NavKind::Flying => {
                let s = cx.block(p);
                !s.is_air() && crate::game::physics::has_collision(s)
            }
            path::NavKind::Water => !path::solid_render(cx.block(p)),
            path::NavKind::Amphibious => !cx.block([p[0], p[1] - 1, p[2]]).is_air(),
            _ => path::solid_render(cx.block([p[0], p[1] - 1, p[2]])),
        }
    }

    fn toward(&self, cx: &mut Ctx, h: f64, d: [i32; 3]) -> [i32; 3] {
        let (mut xt, mut zt) = (d[0] as f64, d[2] as f64);
        if let Some((c, _)) = self.home {
            if h > 1.0 {
                if cx.pos[0] > c[0] as f64 {
                    xt -= cx.rng.r#gen::<f64>() * h / 2.0;
                } else {
                    xt += cx.rng.r#gen::<f64>() * h / 2.0;
                }
                if cx.pos[2] > c[2] as f64 {
                    zt -= cx.rng.r#gen::<f64>() * h / 2.0;
                } else {
                    zt += cx.rng.r#gen::<f64>() * h / 2.0;
                }
            }
        }
        bp([xt + cx.pos[0], d[1] as f64 + cx.pos[1], zt + cx.pos[2]])
    }

    fn restricted(&self, cx: &Ctx, h: f64) -> bool {
        match self.home {
            Some((c, r)) => dist2([c[0] as f64 + 0.5, c[1] as f64 + 0.5, c[2] as f64 + 0.5], cx.pos) < (r as f64 + h + 1.0).powi(2),
            None => false,
        }
    }

    fn pick(&self, cx: &mut Ctx, mut pick_fn: impl FnMut(&Self, &mut Ctx) -> Option<[i32; 3]>) -> Option<[f64; 3]> {
        let mut best: Option<(f32, [i32; 3])> = None;
        for _ in 0..10 {
            if let Some(p) = pick_fn(self, cx) {
                let v = self.walk_value(cx, p);
                if best.is_none_or(|b| v > b.0) {
                    best = Some((v, p));
                }
            }
        }
        best.map(|b| [b.1[0] as f64 + 0.5, b.1[1] as f64, b.1[2] as f64 + 0.5])
    }

    fn valid_toward(&self, cx: &mut Ctx, h: f64, restrict: bool, d: [i32; 3], check_malus: bool) -> Option<[i32; 3]> {
        let p = self.toward(cx, h, d);
        if p[1] < crate::world::chunk::MIN_Y || p[1] >= crate::world::chunk::MIN_Y + crate::world::chunk::HEIGHT {
            return None;
        }
        if restrict && !self.within_home(p) {
            return None;
        }
        if !self.stable(cx, p) {
            return None;
        }
        if check_malus && self.malus_of(self.path_type_at(cx, p)) != 0.0 {
            return None;
        }
        Some(p)
    }

    /// `DefaultRandomPos.getPos`
    pub fn random_pos(&self, cx: &mut Ctx, h: i32, v: i32) -> Option<[f64; 3]> {
        let restrict = self.restricted(cx, h as f64);
        self.pick(cx, |m, cx| {
            let d = rand_dir(&mut cx.rng, h, v);
            m.valid_toward(cx, h as f64, restrict, d, true)
        })
    }

    /// `DefaultRandomPos.getPosAway` / `getPosTowards`
    pub fn random_pos_dir(&self, cx: &mut Ctx, h: i32, v: i32, dir: [f64; 3], spread: f64) -> Option<[f64; 3]> {
        let restrict = self.restricted(cx, h as f64);
        self.pick(cx, |m, cx| {
            let d = rand_dir_within(&mut cx.rng, 0.0, h as f64, v, 0, dir[0], dir[2], spread)?;
            m.valid_toward(cx, h as f64, restrict, d, true)
        })
    }

    pub fn random_pos_away(&self, cx: &mut Ctx, h: i32, v: i32, from: [f64; 3]) -> Option<[f64; 3]> {
        let dir = [cx.pos[0] - from[0], 0.0, cx.pos[2] - from[2]];
        self.random_pos_dir(cx, h, v, dir, std::f64::consts::FRAC_PI_2)
    }

    fn land_fix(&self, cx: &Ctx, p: [i32; 3]) -> Option<[i32; 3]> {
        let top = crate::world::chunk::MIN_Y + crate::world::chunk::HEIGHT - 1;
        let mut q = p;
        while q[1] <= top && path::legacy_solid(cx.block(q)) {
            q[1] += 1;
        }
        if crate::registry::vstate::is_water(cx.block(q)) || self.malus_of(self.path_type_at(cx, q)) != 0.0 {
            return None;
        }
        Some(q)
    }

    /// `LandRandomPos.getPos`
    pub fn land_pos(&self, cx: &mut Ctx, h: i32, v: i32) -> Option<[f64; 3]> {
        let restrict = self.restricted(cx, h as f64);
        self.pick(cx, |m, cx| {
            let d = rand_dir(&mut cx.rng, h, v);
            let p = m.valid_toward(cx, h as f64, restrict, d, false)?;
            m.land_fix(cx, p)
        })
    }

    /// `LandRandomPos.getPosAway` / `getPosTowards`
    pub fn land_pos_dir(&self, cx: &mut Ctx, h: i32, v: i32, dir: [f64; 3]) -> Option<[f64; 3]> {
        let restrict = self.restricted(cx, h as f64);
        let dir = if dir[0] == 0.0 && dir[2] == 0.0 { [cx.rng.r#gen::<f64>() - 0.5, 0.0, cx.rng.r#gen::<f64>() - 0.5] } else { dir };
        self.pick(cx, |m, cx| {
            let d = rand_dir_within(&mut cx.rng, 0.0, h as f64, v, 0, dir[0], dir[2], std::f64::consts::FRAC_PI_2)?;
            let p = m.valid_toward(cx, h as f64, restrict, d, false)?;
            m.land_fix(cx, p)
        })
    }

    /// `AirAndWaterRandomPos` / `HoverRandomPos` (flyers): a free spot around.
    pub fn air_pos(&self, cx: &mut Ctx, h: i32, v: i32, fly_height: i32, dir: Option<[f64; 3]>) -> Option<[f64; 3]> {
        let restrict = self.restricted(cx, h as f64);
        self.pick(cx, |m, cx| {
            let d = match dir {
                Some(dir) => rand_dir_within(&mut cx.rng, 0.0, h as f64, v, fly_height, dir[0], dir[2], std::f64::consts::FRAC_PI_2)?,
                None => {
                    let mut d = rand_dir(&mut cx.rng, h, v);
                    d[1] += fly_height;
                    d
                }
            };
            let p = m.toward(cx, h as f64, d);
            if restrict && !m.within_home(p) {
                return None;
            }
            let top = crate::world::chunk::MIN_Y + crate::world::chunk::HEIGHT - 1;
            let mut q = p;
            while q[1] <= top && path::legacy_solid(cx.block(q)) {
                q[1] += 1;
            }
            if m.malus_of(m.path_type_at(cx, q)) < 0.0 {
                return None;
            }
            Some(q)
        })
    }

    /// `BehaviorUtils.getRandomSwimmablePos`
    pub fn swim_pos(&self, cx: &mut Ctx, h: i32, v: i32) -> Option<[f64; 3]> {
        for _ in 0..10 {
            let p = self.random_pos(cx, h, v)?;
            let q = bp(p);
            if crate::registry::vstate::is_water(cx.block(q)) {
                return Some(p);
            }
        }
        None
    }
}

pub enum WalkKind {
    Animal,
    Monster,
    Custom(fn(&Ctx, [i32; 3]) -> f32),
    None,
}

/// `RandomPos.generateRandomDirection`
pub fn rand_dir(rng: &mut impl Rng, h: i32, v: i32) -> [i32; 3] {
    [rng.gen_range(0..2 * h + 1) - h, rng.gen_range(0..2 * v + 1) - v, rng.gen_range(0..2 * h + 1) - h]
}

/// `RandomPos.generateRandomDirectionWithinRadians`
#[allow(clippy::too_many_arguments)]
pub fn rand_dir_within(rng: &mut impl Rng, min_h: f64, max_h: f64, v: i32, fly: i32, xd: f64, zd: f64, spread: f64) -> Option<[i32; 3]> {
    let center = zd.atan2(xd) - std::f64::consts::FRAC_PI_2;
    let yr = center + (2.0 * rng.r#gen::<f32>() as f64 - 1.0) * spread;
    let dist = (min_h + (rng.r#gen::<f64>().sqrt()) * (max_h - min_h)) * std::f64::consts::SQRT_2;
    let xt = -dist * yr.sin();
    let zt = dist * yr.cos();
    if xt.abs() > max_h || zt.abs() > max_h {
        return None;
    }
    let yt = rng.gen_range(0..2 * v + 1) - v + fly;
    Some([xt.floor() as i32, yt, zt.floor() as i32])
}

// ---------------------------------------------------------------- goal selector

/// One registered goal (`WrappedGoal`).
#[derive(Clone, Debug)]
pub struct Slot {
    pub prio: i32,
    pub running: bool,
    pub goal: goals::Goal,
}

impl Slot {
    pub fn new(prio: i32, goal: goals::Goal) -> Slot {
        Slot { prio, running: false, goal }
    }
}

/// `GoalSelector.tick` (full) / `tickRunningGoals(false)`.
pub fn run_selector(slots: &mut [Slot], m: &mut Mob, cx: &mut Ctx, full: bool, disabled: u8) {
    if full {
        for s in slots.iter_mut() {
            if s.running && (s.goal.flags() & disabled != 0 || !s.goal.can_continue(m, cx)) {
                s.running = false;
                s.goal.stop(m, cx);
            }
        }
        for i in 0..slots.len() {
            if slots[i].running {
                continue;
            }
            let flags = slots[i].goal.flags();
            if flags & disabled != 0 {
                continue;
            }
            // every flag must be free or held by a goal of lower priority (higher number)
            let prio = slots[i].prio;
            let blocked = (0..slots.len()).any(|j| j != i && slots[j].running && slots[j].goal.flags() & flags != 0 && !(slots[j].goal.interruptable() && prio < slots[j].prio));
            if blocked || !slots[i].goal.can_use(m, cx) {
                continue;
            }
            for j in 0..slots.len() {
                if j != i && slots[j].running && slots[j].goal.flags() & flags != 0 {
                    slots[j].running = false;
                    slots[j].goal.stop(m, cx);
                }
            }
            slots[i].running = true;
            slots[i].goal.start(m, cx);
        }
    }
    for s in slots.iter_mut() {
        if s.running && (full || s.goal.every_tick()) {
            s.goal.tick(m, cx);
        }
    }
}
