//! `MoveControl` (+ flying / smooth swimming variants), `LookControl`,
//! `JumpControl`, `BodyRotationControl` and `LivingEntity.travel` for mobs.

use crate::anticheat::combat::Aabb;
use crate::game::physics::{self, Blocks};
use crate::registry::{blocks, vstate, BlockRegistry, BlockState};

/// `Mth.wrapDegrees`
/// `Entity.calculateViewVector` (degrees).
pub fn view(pitch: f32, yaw: f32) -> [f64; 3] {
    let p = (pitch as f64).to_radians();
    let y = (-yaw as f64).to_radians();
    let (ps, pc) = p.sin_cos();
    [y.sin() * pc, -ps, y.cos() * pc]
}

pub fn wrap(d: f32) -> f32 {
    let mut d = d % 360.0;
    if d >= 180.0 {
        d -= 360.0;
    }
    if d < -180.0 {
        d += 360.0;
    }
    d
}

/// `MoveControl.rotlerp`
pub fn rotlerp(a: f32, b: f32, max: f32) -> f32 {
    let diff = wrap(b - a).clamp(-max, max);
    let mut r = a + diff;
    if r < 0.0 {
        r += 360.0;
    } else if r > 360.0 {
        r -= 360.0;
    }
    r
}

/// `Control.rotateTowards`
pub fn rotate_towards(from: f32, to: f32, max: f32) -> f32 {
    from + wrap(to - from).clamp(-max, max)
}

/// `Mth.rotateIfNecessary`
pub fn rotate_if_necessary(base: f32, target: f32, max: f32) -> f32 {
    let diff = wrap(base - target);
    let clamped = diff.clamp(-max, max);
    target + clamped
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MoveKind {
    Default,
    /// `FlyingMoveControl(maxTurn, hoversInPlace)`
    Flying { max_turn: f32, hover: bool },
    /// `SmoothSwimmingMoveControl`
    SmoothSwim { max_x: f32, max_y: f32, in_water: f32, outside: f32, gravity: bool },
    /// `AbstractFish.FishMoveControl`
    Fish,
    /// the mob moves itself (slimes, ghasts, phantoms, vexes, squids, bats...)
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveOp {
    Wait,
    MoveTo,
    Strafe,
    Jumping,
}

#[derive(Clone, Debug)]
pub struct MoveControl {
    pub kind: MoveKind,
    pub op: MoveOp,
    pub wanted: [f64; 3],
    pub speed_mod: f64,
    pub strafe_fwd: f32,
    pub strafe_right: f32,
}

impl MoveControl {
    pub fn new(kind: MoveKind) -> MoveControl {
        MoveControl { kind, op: MoveOp::Wait, wanted: [0.0; 3], speed_mod: 0.0, strafe_fwd: 0.0, strafe_right: 0.0 }
    }

    pub fn has_wanted(&self) -> bool {
        self.op == MoveOp::MoveTo
    }

    pub fn set_wanted(&mut self, p: [f64; 3], speed: f64) {
        self.wanted = p;
        self.speed_mod = speed;
        if self.op != MoveOp::Jumping {
            self.op = MoveOp::MoveTo;
        }
    }

    pub fn strafe(&mut self, fwd: f32, right: f32) {
        self.op = MoveOp::Strafe;
        self.strafe_fwd = fwd;
        self.strafe_right = right;
        self.speed_mod = 0.25;
    }
}

/// `LookControl` state.
#[derive(Clone, Debug)]
pub struct LookControl {
    /// SmoothSwimmingLookControl(maxYRotFromCenter)
    pub smooth_swim: Option<f32>,
    pub wanted: [f64; 3],
    pub y_max: f32,
    pub x_max: f32,
    pub cooldown: i32,
    /// LookControl.resetXRotOnTick
    pub reset_x: bool,
}

impl LookControl {
    pub fn new() -> LookControl {
        LookControl { smooth_swim: None, wanted: [0.0; 3], y_max: 0.0, x_max: 0.0, cooldown: 0, reset_x: true }
    }

    pub fn set(&mut self, p: [f64; 3], y_max: f32, x_max: f32) {
        self.wanted = p;
        self.y_max = y_max;
        self.x_max = x_max;
        self.cooldown = 2;
    }

    pub fn looking(&self) -> bool {
        self.cooldown > 0
    }
}

impl Default for LookControl {
    fn default() -> Self {
        Self::new()
    }
}

/// The physical state of a mob that the controls and `travel` work on.
#[derive(Clone, Debug, Default)]
pub struct Motion {
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: f32,
    pub body_yaw: f32,
    pub xxa: f32,
    pub yya: f32,
    pub zza: f32,
    /// `LivingEntity.speed`
    pub speed: f32,
    pub jumping: bool,
    pub jump_req: bool,
    pub no_jump_delay: i32,
    pub no_gravity: bool,
    pub horizontal_collision: bool,
    pub vertical_collision: bool,
    pub in_water: bool,
    pub in_lava: bool,
    pub water_height: f64,
    pub lava_height: f64,
    pub under_water: bool,
    pub in_powder_snow: bool,
    pub fall: f64,
    pub head_stable: i32,
    pub last_stable_head: f32,
    pub prev: [f64; 3],
    pub sprinting: bool,
}

impl Motion {
    /// `Mob.setSpeed`
    pub fn set_speed(&mut self, s: f32) {
        self.speed = s;
        self.zza = s;
    }
}

/// Mob numbers the controls use.
pub struct ControlMob {
    pub pos: [f64; 3],
    pub width: f64,
    pub height: f64,
    pub eye: f64,
    pub on_ground: bool,
    pub max_up_step: f32,
    pub movement_speed: f64,
    pub flying_speed: f64,
    pub nav_done: bool,
    pub max_head_y: f32,
    pub head_rot_speed: f32,
    pub max_head_x: f32,
}

/// `MoveControl.tick` and its variants.
pub fn move_tick(mc: &mut MoveControl, m: &mut Motion, cm: &ControlMob, vel: &mut [f64; 3], world: &dyn Fn(i32, i32, i32) -> Option<BlockState>, walkable: &dyn Fn(f32, f32) -> bool) {
    match mc.kind {
        MoveKind::Custom => {}
        MoveKind::Default => match mc.op {
            MoveOp::Strafe => {
                let speed = cm.movement_speed as f32;
                let sm = mc.speed_mod as f32 * speed;
                let (mut xa, mut za) = (mc.strafe_fwd, mc.strafe_right);
                let mut dist = (xa * xa + za * za).sqrt();
                if dist < 1.0 {
                    dist = 1.0;
                }
                dist = sm / dist;
                xa *= dist;
                za *= dist;
                let (s, c) = (m.yaw.to_radians().sin(), m.yaw.to_radians().cos());
                let dx = xa * c - za * s;
                let dz = za * c + xa * s;
                if !walkable(dx, dz) {
                    mc.strafe_fwd = 1.0;
                    mc.strafe_right = 0.0;
                }
                m.set_speed(sm);
                m.zza = mc.strafe_fwd;
                m.xxa = mc.strafe_right;
                mc.op = MoveOp::Wait;
            }
            MoveOp::MoveTo => {
                mc.op = MoveOp::Wait;
                let (xd, yd, zd) = (mc.wanted[0] - cm.pos[0], mc.wanted[1] - cm.pos[1], mc.wanted[2] - cm.pos[2]);
                let dd = xd * xd + yd * yd + zd * zd;
                if dd < 2.500_000_3e-7 {
                    m.zza = 0.0;
                    return;
                }
                let yrd = (zd.atan2(xd) * 180.0 / std::f64::consts::PI) as f32 - 90.0;
                m.yaw = rotlerp(m.yaw, yrd, 90.0);
                m.set_speed((mc.speed_mod * cm.movement_speed) as f32);
                let bp = [cm.pos[0].floor() as i32, cm.pos[1].floor() as i32, cm.pos[2].floor() as i32];
                let s = world(bp[0], bp[1], bp[2]).unwrap_or(blocks::AIR);
                let shape = physics::collision(s);
                let top = shape.iter().map(|b| b.max[1]).fold(f64::NAN, f64::max);
                let n = s.name();
                let door_or_fence = n.ends_with("_door") || n.ends_with("_fence") && !n.ends_with("_fence_gate");
                if yd > cm.max_up_step as f64 && xd * xd + zd * zd < cm.width.max(1.0) || !shape.is_empty() && cm.pos[1] < top + bp[1] as f64 && !door_or_fence {
                    m.jump_req = true;
                    mc.op = MoveOp::Jumping;
                }
            }
            MoveOp::Jumping => {
                m.set_speed((mc.speed_mod * cm.movement_speed) as f32);
                if cm.on_ground || m.in_water || m.in_lava {
                    mc.op = MoveOp::Wait;
                }
            }
            MoveOp::Wait => m.zza = 0.0,
        },
        MoveKind::Flying { max_turn, hover } => {
            if mc.op == MoveOp::MoveTo {
                mc.op = MoveOp::Wait;
                m.no_gravity = true;
                let (xd, yd, zd) = (mc.wanted[0] - cm.pos[0], mc.wanted[1] - cm.pos[1], mc.wanted[2] - cm.pos[2]);
                if xd * xd + yd * yd + zd * zd < 2.500_000_3e-7 {
                    m.yya = 0.0;
                    m.zza = 0.0;
                    return;
                }
                let yrd = (zd.atan2(xd).to_degrees()) as f32 - 90.0;
                m.yaw = rotlerp(m.yaw, yrd, 90.0);
                let speed = if cm.on_ground { mc.speed_mod * cm.movement_speed } else { mc.speed_mod * cm.flying_speed } as f32;
                m.set_speed(speed);
                let sd = (xd * xd + zd * zd).sqrt();
                if yd.abs() > 1e-5 || sd.abs() > 1e-5 {
                    let xrd = -(yd.atan2(sd).to_degrees()) as f32;
                    m.pitch = rotlerp(m.pitch, xrd, max_turn);
                    m.yya = if yd > 0.0 { speed } else { -speed };
                }
            } else {
                if !hover {
                    m.no_gravity = false;
                }
                m.yya = 0.0;
                m.zza = 0.0;
            }
        }
        MoveKind::SmoothSwim { max_x, max_y, in_water, outside, gravity } => {
            if gravity && m.in_water {
                vel[1] += 0.005;
            }
            if mc.op == MoveOp::MoveTo && !cm.nav_done {
                let (xd, yd, zd) = (mc.wanted[0] - cm.pos[0], mc.wanted[1] - cm.pos[1], mc.wanted[2] - cm.pos[2]);
                if xd * xd + yd * yd + zd * zd < 2.500_000_3e-7 {
                    m.zza = 0.0;
                } else {
                    let yrd = (zd.atan2(xd).to_degrees()) as f32 - 90.0;
                    m.yaw = rotlerp(m.yaw, yrd, max_y);
                    m.body_yaw = m.yaw;
                    m.head_yaw = m.yaw;
                    let speed = (mc.speed_mod * cm.movement_speed) as f32;
                    if m.in_water {
                        m.set_speed(speed * in_water);
                        let sq = (xd * xd + zd * zd).sqrt();
                        if yd.abs() > 1e-5 || sq.abs() > 1e-5 {
                            let xrd = wrap(-(yd.atan2(sq).to_degrees() as f32)).clamp(-max_x, max_x);
                            m.pitch = rotate_towards(m.pitch, xrd, 5.0);
                        }
                        let (c, s) = (m.pitch.to_radians().cos(), m.pitch.to_radians().sin());
                        m.zza = c * speed;
                        m.yya = -s * speed;
                    } else {
                        let left = wrap(m.yaw - yrd).abs();
                        let f = 1.0 - ((left - 10.0) / 50.0).clamp(0.0, 1.0);
                        m.set_speed(speed * outside * f);
                    }
                }
            } else {
                m.set_speed(0.0);
                m.xxa = 0.0;
                m.yya = 0.0;
                m.zza = 0.0;
            }
        }
        MoveKind::Fish => {
            // AbstractFish.FishMoveControl
            if m.in_water {
                vel[1] += 0.005;
            }
            if mc.op == MoveOp::MoveTo && !cm.nav_done {
                let speed = (mc.speed_mod * cm.movement_speed) as f32;
                m.speed = m.speed + (speed - m.speed) * 0.125;
                let (xd, yd, zd) = (mc.wanted[0] - cm.pos[0], mc.wanted[1] - cm.pos[1], mc.wanted[2] - cm.pos[2]);
                if yd != 0.0 {
                    let dd = (xd * xd + yd * yd + zd * zd).sqrt();
                    vel[1] += m.speed as f64 * (yd / dd) * 0.1;
                }
                if xd != 0.0 || zd != 0.0 {
                    let yrd = (zd.atan2(xd).to_degrees()) as f32 - 90.0;
                    m.yaw = rotlerp(m.yaw, yrd, 90.0);
                    m.body_yaw = m.yaw;
                }
            } else {
                m.speed = 0.0;
            }
            m.zza = m.speed;
        }
    }
}

/// `LookControl.tick` (+ the smooth swimming variant) and body rotation.
pub fn look_tick(lc: &mut LookControl, m: &mut Motion, cm: &ControlMob) {
    let yrd = || -> Option<f32> {
        let (xd, zd) = (lc.wanted[0] - cm.pos[0], lc.wanted[2] - cm.pos[2]);
        (zd.abs() > 1e-5 || xd.abs() > 1e-5).then(|| zd.atan2(xd).to_degrees() as f32 - 90.0)
    };
    let xrd = || -> Option<f32> {
        let (xd, yd, zd) = (lc.wanted[0] - cm.pos[0], lc.wanted[1] - (cm.pos[1] + cm.eye), lc.wanted[2] - cm.pos[2]);
        let sd = (xd * xd + zd * zd).sqrt();
        (yd.abs() > 1e-5 || sd.abs() > 1e-5).then(|| -(yd.atan2(sd).to_degrees()) as f32)
    };
    if let Some(max_center) = lc.smooth_swim {
        if lc.cooldown > 0 {
            lc.cooldown -= 1;
            if let Some(y) = yrd() {
                m.head_yaw = rotate_towards(m.head_yaw, y + 20.0, lc.y_max);
            }
            if let Some(x) = xrd() {
                m.pitch = rotate_towards(m.pitch, x + 10.0, lc.x_max);
            }
        } else {
            if cm.nav_done {
                m.pitch = rotate_towards(m.pitch, 0.0, 5.0);
            }
            m.head_yaw = rotate_towards(m.head_yaw, m.body_yaw, lc.y_max);
        }
        let d = wrap(m.head_yaw - m.body_yaw);
        if d < -max_center {
            m.body_yaw -= 4.0;
        } else if d > max_center {
            m.body_yaw += 4.0;
        }
        return;
    }
    if lc.reset_x {
        m.pitch = 0.0;
    }
    if lc.cooldown > 0 {
        lc.cooldown -= 1;
        if let Some(y) = yrd() {
            m.head_yaw = rotate_towards(m.head_yaw, y, lc.y_max);
        }
        if let Some(x) = xrd() {
            m.pitch = rotate_towards(m.pitch, x, lc.x_max);
        }
    } else {
        m.head_yaw = rotate_towards(m.head_yaw, m.body_yaw, 10.0);
    }
    if !cm.nav_done {
        m.head_yaw = rotate_if_necessary(m.head_yaw, m.body_yaw, cm.max_head_y);
    }
}

/// `BodyRotationControl.clientTick` (server keeps it for head clamping).
pub fn body_tick(m: &mut Motion, pos: [f64; 3], max_head_y: f32) {
    let (xd, zd) = (pos[0] - m.prev[0], pos[2] - m.prev[2]);
    if xd * xd + zd * zd > 2.500_000_3e-7 {
        m.body_yaw = m.yaw;
        m.head_yaw = rotate_if_necessary(m.head_yaw, m.body_yaw, max_head_y);
        m.last_stable_head = m.head_yaw;
        m.head_stable = 0;
    } else if (m.head_yaw - m.last_stable_head).abs() > 15.0 {
        m.head_stable = 0;
        m.last_stable_head = m.head_yaw;
        m.body_yaw = rotate_if_necessary(m.body_yaw, m.head_yaw, max_head_y);
    } else {
        m.head_stable += 1;
        if m.head_stable > 10 {
            let f = ((m.head_stable - 10) as f32 / 10.0).clamp(0.0, 1.0);
            m.body_yaw = rotate_if_necessary(m.body_yaw, m.head_yaw, max_head_y * (1.0 - f));
        }
    }
}

// ---------------------------------------------------------------- travel

pub struct World<'a>(pub &'a dyn Fn(i32, i32, i32) -> Option<BlockState>);

impl Blocks for World<'_> {
    fn block(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
        (self.0)(x, y, z)
    }
}

/// Inputs of one travel step.
pub struct TravelIn {
    pub width: f64,
    pub height: f64,
    pub eye: f64,
    pub max_up_step: f32,
    pub gravity: f64,
    pub jump_strength: f64,
    pub jump_boost: f64,
    pub water_efficiency: f64,
    pub levitation: Option<i32>,
    pub slow_falling: bool,
    pub dolphins_grace: bool,
    /// FlyingAnimal: vertical friction like horizontal
    pub flying_animal: bool,
    /// `travelFlying` mobs (ghast, vex, allay, phantom...): (water, lava, air) speeds
    pub flying: Option<(f32, f32, f32)>,
    pub fluid_affected: bool,
    pub can_stand_on_lava: bool,
    pub water_slowdown: f32,
    /// spiders on a wall count as climbing
    pub climbing: bool,
}

/// `updateFluidHeightAndDoFluidPushing` (heights only, no flow pushing).
pub fn fluid_heights(world: &dyn Fn(i32, i32, i32) -> Option<BlockState>, bb: &Aabb) -> (f64, f64) {
    let b = Aabb { min: [bb.min[0] + 0.001, bb.min[1] + 0.001, bb.min[2] + 0.001], max: [bb.max[0] - 0.001, bb.max[1] - 0.001, bb.max[2] - 0.001] };
    let (mut water, mut lava) = (0.0f64, 0.0f64);
    let reg = BlockRegistry::get();
    for x in b.min[0].floor() as i32..=b.max[0].floor() as i32 {
        for y in b.min[1].floor() as i32..=b.max[1].floor() as i32 {
            for z in b.min[2].floor() as i32..=b.max[2].floor() as i32 {
                let Some(s) = world(x, y, z) else { continue };
                if !vstate::has_fluid(s) {
                    continue;
                }
                let level: i32 = reg.property(s, "level").and_then(|l| l.parse().ok()).unwrap_or(0);
                // a fluid with the same fluid above fills the block
                let above = world(x, y + 1, z).unwrap_or(blocks::AIR);
                let water_here = vstate::is_water(s);
                let full = if water_here { vstate::is_water(above) } else { above.name() == "lava" };
                let h = if full {
                    1.0
                } else if level == 0 || level >= 8 {
                    if level >= 8 { 1.0 } else { 8.0 / 9.0 }
                } else {
                    (8 - level) as f64 / 9.0
                };
                let top = y as f64 + h;
                if top >= b.min[1] {
                    let d = top - b.min[1];
                    if water_here {
                        water = water.max(d);
                    } else {
                        lava = lava.max(d);
                    }
                }
            }
        }
    }
    (water, lava)
}

/// Result flags of `Entity.move`.
pub struct Moved {
    pub on_ground: bool,
    pub horizontal: bool,
    pub landed_fall: Option<f64>,
}

/// `Entity.move(SELF)`: collision, ground, fall distance, block speed factor.
pub fn do_move(world: &dyn Fn(i32, i32, i32) -> Option<BlockState>, pos: &mut [f64; 3], vel: &mut [f64; 3], ti: &TravelIn, on_ground: bool, m: &mut Motion) -> Moved {
    let w = World(world);
    let mut mv = *vel;
    // stuck in cobweb / berry bush / powder snow (makeStuckInBlock)
    let bb = Aabb::entity(*pos, ti.width, ti.height);
    let inside = crate::game::hazards::inside(&|x, y, z| world(x, y, z), &bb);
    let stuck = if inside.cobweb {
        Some([0.25, 0.05, 0.25])
    } else if inside.any_berry_bush {
        Some([0.8, 0.75, 0.8])
    } else if inside.powder_snow {
        Some([0.9, 1.5, 0.9])
    } else {
        None
    };
    if let Some(s) = stuck {
        mv = [mv[0] * s[0], mv[1] * s[1], mv[2] * s[2]];
        *vel = [0.0; 3];
        m.fall = 0.0;
    }
    let r = physics::collide(&w, &bb, mv, on_ground, ti.max_up_step);
    for i in 0..3 {
        pos[i] += r[i];
    }
    let xc = (mv[0] - r[0]).abs() > 1e-5;
    let zc = (mv[2] - r[2]).abs() > 1e-5;
    let yc = mv[1] != r[1];
    m.horizontal_collision = xc || zc;
    m.vertical_collision = yc;
    let ground = yc && mv[1] < 0.0;
    let mut landed = None;
    if ground {
        if m.fall > 0.0 {
            landed = Some(m.fall);
        }
        m.fall = 0.0;
    } else if r[1] < 0.0 {
        m.fall -= r[1];
    }
    if xc {
        vel[0] = 0.0;
    }
    if zc {
        vel[2] = 0.0;
    }
    if yc {
        // Block.updateEntityMovementAfterFallOn (slime bounces)
        let below = world(pos[0].floor() as i32, (pos[1] - 0.2).floor() as i32, pos[2].floor() as i32).unwrap_or(blocks::AIR);
        if below.name() == "slime_block" && vel[1] < 0.0 && !m.jumping {
            vel[1] = -vel[1];
        } else if below.name().ends_with("_bed") && vel[1] < 0.0 {
            vel[1] = -vel[1] * 0.66;
        } else {
            vel[1] = 0.0;
        }
    }
    // getBlockSpeedFactor
    let at = world(pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32).unwrap_or(blocks::AIR);
    let mut f = crate::game::ai::path::block_factors(at.name()).1;
    if f == 1.0 && !vstate::is_water(at) && at.name() != "bubble_column" {
        let below = world(pos[0].floor() as i32, (pos[1] - 0.500001).floor() as i32, pos[2].floor() as i32).unwrap_or(blocks::AIR);
        f = crate::game::ai::path::block_factors(below.name()).1;
    }
    if f != 1.0 {
        vel[0] *= f as f64;
        vel[2] *= f as f64;
    }
    Moved { on_ground: ground, horizontal: m.horizontal_collision, landed_fall: landed }
}

pub fn input_vector(input: [f32; 3], speed: f32, yaw: f32) -> [f64; 3] {
    let l = input[0] * input[0] + input[1] * input[1] + input[2] * input[2];
    if l < 1.0e-7 {
        return [0.0; 3];
    }
    let mut v = input;
    if l > 1.0 {
        let n = l.sqrt();
        v = [v[0] / n, v[1] / n, v[2] / n];
    }
    let v = [v[0] * speed, v[1] * speed, v[2] * speed];
    let (s, c) = (yaw.to_radians().sin(), yaw.to_radians().cos());
    [(v[0] * c - v[2] * s) as f64, v[1] as f64, (v[2] * c + v[0] * s) as f64]
}

/// `LivingEntity.aiStep` jump handling + `travel`; returns the move result.
pub fn travel(world: &dyn Fn(i32, i32, i32) -> Option<BlockState>, pos: &mut [f64; 3], vel: &mut [f64; 3], on_ground: &mut bool, m: &mut Motion, ti: &TravelIn) -> Moved {
    let bb = Aabb::entity(*pos, ti.width, ti.height);
    let (wh, lh) = fluid_heights(world, &bb);
    m.water_height = wh;
    m.lava_height = lh;
    m.in_water = wh > 0.0;
    m.in_lava = lh > 0.0;
    let eye_y = pos[1] + ti.eye;
    let eye_block = world(pos[0].floor() as i32, eye_y.floor() as i32, pos[2].floor() as i32).unwrap_or(blocks::AIR);
    m.under_water = vstate::is_water(eye_block) && m.in_water && pos[1] + wh > eye_y;
    if m.no_jump_delay > 0 {
        m.no_jump_delay -= 1;
    }
    for v in vel.iter_mut() {
        if v.abs() < 0.003 {
            *v = 0.0;
        }
    }
    // jumping
    let threshold = if ti.eye < 0.4 { 0.0 } else { 0.4 };
    if m.jumping && ti.fluid_affected {
        let fh = if m.in_lava { lh } else { wh };
        let in_water_h = m.in_water && fh > 0.0;
        if in_water_h && (!*on_ground || fh > threshold) {
            vel[1] += 0.04;
        } else if m.in_lava && (!*on_ground || fh > threshold) {
            vel[1] += 0.04;
        } else if (*on_ground || in_water_h && fh <= threshold) && m.no_jump_delay == 0 {
            let below = world(pos[0].floor() as i32, (pos[1] - 0.5).floor() as i32, pos[2].floor() as i32).unwrap_or(blocks::AIR);
            let at = world(pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32).unwrap_or(blocks::AIR);
            let jf = {
                let a = crate::game::ai::path::block_factors(at.name()).2;
                if a == 1.0 { crate::game::ai::path::block_factors(below.name()).2 } else { a }
            };
            let power = (ti.jump_strength * jf as f64 + ti.jump_boost) as f32;
            if power > 1.0e-5 {
                vel[1] = (power as f64).max(vel[1]);
                if m.sprinting {
                    let a = m.yaw.to_radians();
                    vel[0] += -(a.sin() as f64) * 0.2;
                    vel[2] += a.cos() as f64 * 0.2;
                }
            }
            m.no_jump_delay = 10;
        }
    } else {
        m.no_jump_delay = 0;
    }
    let input = [m.xxa, m.yya, m.zza];
    let gravity = if m.no_gravity { 0.0 } else if vel[1] <= 0.0 && ti.slow_falling { ti.gravity.min(0.01) } else { ti.gravity };
    if ti.slow_falling || ti.levitation.is_some() {
        m.fall = 0.0;
    }
    let old_y = pos[1];
    if let Some((ws, ls, air)) = ti.flying {
        // travelFlying
        let (speed, drag) = if m.in_water { (ws, 0.8) } else if m.in_lava { (ls, 0.5) } else { (air, 0.91) };
        let d = input_vector(input, speed, m.yaw);
        for i in 0..3 {
            vel[i] += d[i];
        }
        let r = do_move(world, pos, vel, ti, *on_ground, m);
        *on_ground = r.on_ground;
        for v in vel.iter_mut() {
            *v *= drag;
        }
        return r;
    }
    let in_fluid = ti.fluid_affected && (m.in_water || m.in_lava && !ti.can_stand_on_lava);
    if in_fluid {
        let falling = vel[1] <= 0.0;
        let sprinting = m.sprinting;
        let adjust = |v: &mut [f64; 3]| {
            if gravity != 0.0 && !sprinting {
                if falling && (v[1] - 0.005).abs() >= 0.003 && (v[1] - gravity / 16.0).abs() < 0.003 {
                    v[1] = -0.003;
                } else {
                    v[1] -= gravity / 16.0;
                }
            }
        };
        let r;
        if m.in_water {
            let mut slow = if m.sprinting { 0.9 } else { ti.water_slowdown };
            let mut speed = 0.02f32;
            let mut ww = ti.water_efficiency as f32;
            if !*on_ground {
                ww *= 0.5;
            }
            if ww > 0.0 {
                slow += (0.546_000_06 - slow) * ww;
                speed += (m.speed - speed) * ww;
            }
            if ti.dolphins_grace {
                slow = 0.96;
            }
            let d = input_vector(input, speed, m.yaw);
            for i in 0..3 {
                vel[i] += d[i];
            }
            r = do_move(world, pos, vel, ti, *on_ground, m);
            *on_ground = r.on_ground;
            vel[0] *= slow as f64;
            vel[1] *= 0.8;
            vel[2] *= slow as f64;
            adjust(vel);
        } else {
            let d = input_vector(input, 0.02, m.yaw);
            for i in 0..3 {
                vel[i] += d[i];
            }
            r = do_move(world, pos, vel, ti, *on_ground, m);
            *on_ground = r.on_ground;
            if lh <= threshold {
                vel[0] *= 0.5;
                vel[1] *= 0.8;
                vel[2] *= 0.5;
                adjust(vel);
            } else {
                for v in vel.iter_mut() {
                    *v *= 0.5;
                }
            }
            if gravity != 0.0 {
                vel[1] -= gravity / 4.0;
            }
        }
        // jumpOutOfFluid
        if m.horizontal_collision {
            let test = Aabb::entity(*pos, ti.width, ti.height).translate([vel[0], vel[1] + 0.6 - pos[1] + old_y, vel[2]]);
            if physics::no_collision(&World(world), &test) && fluid_heights(world, &test) == (0.0, 0.0) {
                vel[1] = 0.3;
            }
        }
        return r;
    }
    // travelInAir
    let below = world(pos[0].floor() as i32, (pos[1] - 0.500001).floor() as i32, pos[2].floor() as i32).unwrap_or(blocks::AIR);
    let bf = if *on_ground { crate::game::ai::path::block_factors(below.name()).0 } else { 1.0 };
    let friction = bf * 0.91;
    let speed = if *on_ground { m.speed * (0.216_000_02 / (bf * bf * bf)) } else { 0.02 };
    let d = input_vector(input, speed, m.yaw);
    for i in 0..3 {
        vel[i] += d[i];
    }
    // climbable
    let at = world(pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32).unwrap_or(blocks::AIR);
    let climbable = matches!(at.name(), "ladder" | "vine" | "scaffolding" | "twisting_vines" | "twisting_vines_plant" | "weeping_vines" | "weeping_vines_plant" | "cave_vines" | "cave_vines_plant") || ti.climbing;
    if climbable {
        m.fall = 0.0;
        vel[0] = vel[0].clamp(-0.15, 0.15);
        vel[2] = vel[2].clamp(-0.15, 0.15);
        vel[1] = vel[1].max(-0.15);
    }
    let r = do_move(world, pos, vel, ti, *on_ground, m);
    *on_ground = r.on_ground;
    if (m.horizontal_collision || m.jumping) && climbable {
        vel[1] = 0.2;
    }
    let mut vy = vel[1];
    if let Some(amp) = ti.levitation {
        vy += (0.05 * (amp + 1) as f64 - vel[1]) * 0.2;
    } else {
        vy -= gravity;
    }
    let vf = if ti.flying_animal { friction } else { 0.98 };
    vel[0] *= friction as f64;
    vel[1] = vy * vf as f64;
    vel[2] *= friction as f64;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walking_speed_reaches_vanilla_terminal_velocity() {
        crate::registry::BlockRegistry::get();
        let world = |_x: i32, y: i32, _z: i32| Some(if y < 64 { blocks::STONE } else { blocks::AIR });
        let mut pos = [0.5, 64.0, 0.5];
        let mut vel = [0.0; 3];
        let mut ground = true;
        let mut m = Motion::default();
        let ti = TravelIn { width: 0.6, height: 1.95, eye: 1.74, max_up_step: 0.6, gravity: 0.08, jump_strength: 0.42, jump_boost: 0.0, water_efficiency: 0.0, levitation: None, slow_falling: false, dolphins_grace: false, flying_animal: false, flying: None, fluid_affected: true, can_stand_on_lava: false, water_slowdown: 0.8, climbing: false };
        for _ in 0..100 {
            m.set_speed(0.23);
            m.yaw = 0.0;
            travel(&world, &mut pos, &mut vel, &mut ground, &mut m, &ti);
        }
        // v = s² / (1 - 0.6·0.91) on stone: 0.0529 / 0.454
        assert!((vel[2] - 0.0529 / 0.454 * 0.546).abs() < 1e-3 || (vel[2] - 0.0529 * 0.546 / (1.0 - 0.546)).abs() < 1e-3, "{vel:?}");
        assert!(ground);
        assert!((pos[1] - 64.0).abs() < 1e-9);
    }
}
