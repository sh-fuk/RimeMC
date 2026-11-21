//! Per-type code outside the goals: the custom move controls (slime, ghast,
//! phantom, vex, guardian, turtle, drowned, squid, bat, dragon),
//! `customServerAiStep` and the `aiStep` / `tick` overrides (creeper swell,
//! sun burning, eggs, conversions, contact damage, witch drinking...).

use rand::Rng;

use super::control::{self, MoveOp, Moved, TravelIn};
use super::{bp, dist2, Ctx, Seen};
use crate::anticheat::combat::Aabb;
use crate::game::combat::attr::Attr;
use crate::game::combat::effects::Effect;
use crate::game::combat::living::{HEAD, MAINHAND};
use crate::game::mob::Mob;
use crate::game::mobtick::Act;
use crate::game::projectile::PType;
use crate::registry::vstate;

fn attr(m: &mut Mob, a: Attr) -> f64 {
    m.living.attrs.value(a)
}

fn sound(cx: &mut Ctx, name: &str, volume: f32, pitch: f32, hostile: bool) {
    cx.acts.push(Act::Sound { name: format!("minecraft:{name}"), pos: cx.pos, hostile, volume, pitch });
}

fn pitch(cx: &mut Ctx) -> f32 {
    (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 1.0
}

// ---------------------------------------------------------------- move controls

/// `MoveControl.tick` of the `MoveKind::Custom` mobs.
pub fn move_tick(m: &mut Mob, cx: &mut Ctx) {
    match m.ty.name() {
        "slime" | "magma_cube" => slime_move(m, cx),
        "ghast" | "happy_ghast" => ghast_move(m, cx),
        "phantom" => phantom_move(m, cx),
        "vex" => vex_move(m, cx),
        "guardian" | "elder_guardian" => guardian_move(m, cx),
        "turtle" => turtle_move(m, cx),
        "drowned" => drowned_move(m, cx),
        // squid / bat / shulker / dragon move themselves elsewhere
        _ => {}
    }
}

/// `Slime.SlimeMoveControl.setDirection`
pub fn slime_direction(m: &mut Mob, yrot: f32, aggressive: bool) {
    m.v.yrot_w = yrot;
    m.v.slime_aggr = aggressive;
}

/// `Slime.SlimeMoveControl.setWantedMovement`
pub fn slime_wanted(m: &mut Mob, speed: f64) {
    m.mc.speed_mod = speed;
    m.mc.op = MoveOp::MoveTo;
}

/// `Slime.getJumpDelay` (magma cubes wait four times as long)
fn slime_jump_delay(m: &Mob, cx: &mut Ctx) -> i32 {
    let d = cx.rng.gen_range(0..20) + 10;
    if m.ty.is("magma_cube") { d * 4 } else { d }
}

fn slime_pitch(m: &Mob, cx: &mut Ctx) -> f32 {
    let k = if m.size <= 1 { 1.4 } else { 0.8 };
    pitch(cx) * k
}

fn slime_move(m: &mut Mob, cx: &mut Ctx) {
    m.mo.yaw = control::rotlerp(m.mo.yaw, m.v.yrot_w, 90.0);
    m.mo.head_yaw = m.mo.yaw;
    m.mo.body_yaw = m.mo.yaw;
    if m.mc.op != MoveOp::MoveTo {
        m.mo.zza = 0.0;
        return;
    }
    m.mc.op = MoveOp::Wait;
    let speed = (m.mc.speed_mod * attr(m, Attr::MovementSpeed)) as f32;
    if cx.on_ground {
        m.mo.set_speed(speed);
        let jd = m.v.jump_delay;
        m.v.jump_delay -= 1;
        if jd <= 0 {
            m.v.jump_delay = slime_jump_delay(m, cx);
            if m.v.slime_aggr {
                m.v.jump_delay /= 3;
            }
            m.mo.jump_req = true;
            if m.size > 0 {
                let name = if m.ty.is("magma_cube") { "entity.magma_cube.jump" } else if m.size <= 1 { "entity.slime.jump_small" } else { "entity.slime.jump" };
                let p = slime_pitch(m, cx);
                sound(cx, name, 0.4 * m.size as f32, p, true);
            }
        } else {
            m.mo.xxa = 0.0;
            m.mo.zza = 0.0;
            m.mo.set_speed(0.0);
        }
    } else {
        m.mo.set_speed(speed);
    }
}

/// `Ghast.GhastMoveControl`
fn ghast_move(m: &mut Mob, cx: &mut Ctx) {
    if m.mc.op != MoveOp::MoveTo {
        return;
    }
    let fd = m.v.float_dur;
    m.v.float_dur -= 1;
    if fd > 0 {
        return;
    }
    m.v.float_dur += cx.rng.gen_range(0..5) + 2;
    let t = [m.mc.wanted[0] - cx.pos[0], m.mc.wanted[1] - cx.pos[1], m.mc.wanted[2] - cx.pos[2]];
    let len = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
    if len < 1e-7 {
        return;
    }
    // canReach: no collision along the way
    let bb = Aabb::entity(cx.pos, m.width(), m.height());
    let steps = len.ceil() as i32;
    let w = control::World(cx.get);
    let mut ok = true;
    for i in 1..steps.max(1) {
        let k = i as f64 / len;
        let moved = bb.translate([t[0] * k, t[1] * k, t[2] * k]);
        if !crate::game::physics::no_collision(&w, &moved) {
            ok = false;
            break;
        }
    }
    if ok {
        let s = attr(m, Attr::FlyingSpeed) * 5.0 / 3.0;
        for i in 0..3 {
            cx.vel[i] += t[i] / len * s;
        }
    } else {
        m.mc.op = MoveOp::Wait;
    }
}

/// `Phantom.PhantomMoveControl`
fn phantom_move(m: &mut Mob, cx: &mut Ctx) {
    if m.v.fly_speed == 0.0 {
        m.v.fly_speed = 0.1;
    }
    if m.mo.horizontal_collision {
        m.mo.yaw += 180.0;
        m.v.fly_speed = 0.1;
    }
    let mt = m.v.move_target;
    let (mut tdx, tdy, mut tdz) = (mt[0] - cx.pos[0], mt[1] - cx.pos[1], mt[2] - cx.pos[2]);
    let mut sd = (tdx * tdx + tdz * tdz).sqrt();
    if sd.abs() <= 1e-5 {
        return;
    }
    let scale = 1.0 - (tdy * 0.7).abs() / sd;
    tdx *= scale;
    tdz *= scale;
    sd = (tdx * tdx + tdz * tdz).sqrt();
    let sd2 = (tdx * tdx + tdz * tdz + tdy * tdy).sqrt();
    let prev = m.mo.yaw;
    let angle = tdz.atan2(tdx) as f32;
    let a = control::wrap(m.mo.yaw + 90.0);
    let b = control::wrap(angle.to_degrees());
    m.mo.yaw = approach_degrees(a, b, 4.0) - 90.0;
    m.mo.body_yaw = m.mo.yaw;
    if control::wrap(prev - m.mo.yaw).abs() < 3.0 {
        m.v.fly_speed = approach(m.v.fly_speed, 1.8, 0.005 * (1.8 / m.v.fly_speed));
    } else {
        m.v.fly_speed = approach(m.v.fly_speed, 0.2, 0.025);
    }
    let xrot = -((-tdy).atan2(sd).to_degrees() as f32);
    m.mo.pitch = xrot;
    let ma = (m.mo.yaw + 90.0).to_radians();
    let s = m.v.fly_speed as f64;
    let t = [s * ma.cos() as f64 * (tdx / sd2).abs(), s * xrot.to_radians().sin() as f64 * (tdy / sd2).abs(), s * ma.sin() as f64 * (tdz / sd2).abs()];
    for i in 0..3 {
        cx.vel[i] += (t[i] - cx.vel[i]) * 0.2;
    }
}

/// `Mth.approach`
pub fn approach(v: f32, target: f32, step: f32) -> f32 {
    let step = step.abs();
    if v < target { (v + step).min(target) } else { (v - step).max(target) }
}

/// `Mth.approachDegrees`
pub fn approach_degrees(v: f32, target: f32, step: f32) -> f32 {
    let d = control::wrap(target - v);
    approach(v, v + d, step)
}

/// `Vex.VexMoveControl`
fn vex_move(m: &mut Mob, cx: &mut Ctx) {
    if m.mc.op != MoveOp::MoveTo {
        return;
    }
    let d = [m.mc.wanted[0] - cx.pos[0], m.mc.wanted[1] - cx.pos[1], m.mc.wanted[2] - cx.pos[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    // AABB.getSize: average edge length
    let size = (m.width() * 2.0 + m.height()) / 3.0;
    if len < size {
        m.mc.op = MoveOp::Wait;
        for v in &mut cx.vel {
            *v *= 0.5;
        }
        return;
    }
    let k = m.mc.speed_mod * 0.05 / len;
    for i in 0..3 {
        cx.vel[i] += d[i] * k;
    }
    let target = m.target.and_then(|t| cx.find(t)).map(|t| t.pos);
    m.mo.yaw = match target {
        None => -(cx.vel[0].atan2(cx.vel[2]).to_degrees() as f32),
        Some(t) => -((t[0] - cx.pos[0]).atan2(t[2] - cx.pos[2]).to_degrees() as f32),
    };
    m.mo.body_yaw = m.mo.yaw;
}

/// `Guardian.GuardianMoveControl`
fn guardian_move(m: &mut Mob, cx: &mut Ctx) {
    if m.mc.op == MoveOp::MoveTo && !m.nav.is_done() {
        let d = [m.mc.wanted[0] - cx.pos[0], m.mc.wanted[1] - cx.pos[1], m.mc.wanted[2] - cx.pos[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-7);
        let (xd, yd, zd) = (d[0] / len, d[1] / len, d[2] / len);
        let yrot = (d[2].atan2(d[0]).to_degrees() as f32) - 90.0;
        m.mo.yaw = control::rotlerp(m.mo.yaw, yrot, 90.0);
        m.mo.body_yaw = m.mo.yaw;
        let target = (m.mc.speed_mod * attr(m, Attr::MovementSpeed)) as f32;
        let speed = m.mo.speed + (target - m.mo.speed) * 0.125;
        m.mo.set_speed(speed);
        let t = (m.tick_count + cx.eid) as f64;
        let push = (t * 0.5).sin() * 0.05;
        let (s, c) = ((m.mo.yaw as f64).to_radians().sin(), (m.mo.yaw as f64).to_radians().cos());
        let ypush = (t * 0.75).sin() * 0.05;
        cx.vel[0] += push * c;
        cx.vel[1] += ypush * (s + c) * 0.25 + speed as f64 * yd * 0.1;
        cx.vel[2] += push * s;
        let eye = cx.pos[1] + m.eye();
        let new = [cx.pos[0] + xd * 2.0, eye + yd / len, cx.pos[2] + zd * 2.0];
        let old = if m.look.looking() { m.look.wanted } else { new };
        let l = [old[0] + (new[0] - old[0]) * 0.125, old[1] + (new[1] - old[1]) * 0.125, old[2] + (new[2] - old[2]) * 0.125];
        m.look.set(l, 10.0, 40.0);
        m.v.flags |= 2; // moving
    } else {
        m.mo.set_speed(0.0);
        m.v.flags &= !2;
    }
}

/// `Turtle.TurtleMoveControl`
fn turtle_move(m: &mut Mob, cx: &mut Ctx) {
    // updateSpeed
    if m.mo.in_water {
        cx.vel[1] += 0.005;
        let home = m.v.home_pos.unwrap_or(cx.block_pos());
        if dist2(cx.pos, [home[0] as f64 + 0.5, home[1] as f64 + 0.5, home[2] as f64 + 0.5]) >= 256.0 {
            m.mo.speed = (m.mo.speed / 2.0).max(0.08);
        }
        if m.is_baby() {
            m.mo.speed = (m.mo.speed / 3.0).max(0.06);
        }
    } else if cx.on_ground {
        m.mo.speed = (m.mo.speed / 2.0).max(0.06);
    }
    if m.mc.op == MoveOp::MoveTo && !m.nav.is_done() {
        let d = [m.mc.wanted[0] - cx.pos[0], m.mc.wanted[1] - cx.pos[1], m.mc.wanted[2] - cx.pos[2]];
        let dd = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if dd < 1e-5 {
            m.mo.set_speed(0.0);
        } else {
            let yd = d[1] / dd;
            let yrot = (d[2].atan2(d[0]).to_degrees() as f32) - 90.0;
            m.mo.yaw = control::rotlerp(m.mo.yaw, yrot, 90.0);
            m.mo.body_yaw = m.mo.yaw;
            let target = (m.mc.speed_mod * attr(m, Attr::MovementSpeed)) as f32;
            let s = m.mo.speed + (target - m.mo.speed) * 0.125;
            m.mo.set_speed(s);
            cx.vel[1] += s as f64 * yd * 0.1;
        }
    } else {
        m.mo.set_speed(0.0);
    }
}

/// `Drowned.wantsToSwim`
pub fn drowned_wants_to_swim(m: &Mob, cx: &Ctx) -> bool {
    if m.v.flags & 4 != 0 {
        return true;
    }
    m.target.and_then(|t| cx.find(t)).is_some_and(|t| t.in_water)
}

/// `Drowned.DrownedMoveControl`
fn drowned_move(m: &mut Mob, cx: &mut Ctx) {
    let target_y = m.target.and_then(|t| cx.find(t)).map(|t| t.pos[1]);
    if drowned_wants_to_swim(m, cx) && m.mo.in_water {
        if target_y.is_some_and(|y| y > cx.pos[1]) || m.v.flags & 4 != 0 {
            cx.vel[1] += 0.002;
        }
        if m.mc.op != MoveOp::MoveTo || m.nav.is_done() {
            m.mo.set_speed(0.0);
            return;
        }
        let d = [m.mc.wanted[0] - cx.pos[0], m.mc.wanted[1] - cx.pos[1], m.mc.wanted[2] - cx.pos[2]];
        let dd = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-7);
        let yd = d[1] / dd;
        let yrot = (d[2].atan2(d[0]).to_degrees() as f32) - 90.0;
        m.mo.yaw = control::rotlerp(m.mo.yaw, yrot, 90.0);
        m.mo.body_yaw = m.mo.yaw;
        let target = (m.mc.speed_mod * attr(m, Attr::MovementSpeed)) as f32;
        let s = m.mo.speed + (target - m.mo.speed) * 0.125;
        m.mo.set_speed(s);
        cx.vel[0] += s as f64 * d[0] * 0.005;
        cx.vel[1] += s as f64 * yd * 0.1;
        cx.vel[2] += s as f64 * d[2] * 0.005;
    } else {
        if !cx.on_ground {
            cx.vel[1] -= 0.008;
        }
        default_move(m, cx);
    }
}

/// The plain `MoveControl.tick` for a mob whose own control falls back to it.
fn default_move(m: &mut Mob, cx: &mut Ctx) {
    let saved = m.mc.kind;
    m.mc.kind = control::MoveKind::Default;
    let cm = control_mob(m, cx);
    let get = cx.get;
    let mut vel = cx.vel;
    let walk = |_: f32, _: f32| true;
    control::move_tick(&mut m.mc, &mut m.mo, &cm, &mut vel, get, &walk);
    cx.vel = vel;
    m.mc.kind = saved;
}

pub fn control_mob(m: &mut Mob, cx: &Ctx) -> control::ControlMob {
    control::ControlMob {
        pos: cx.pos,
        width: m.width(),
        height: m.height(),
        eye: m.eye(),
        on_ground: cx.on_ground,
        max_up_step: m.max_up_step,
        movement_speed: attr(m, Attr::MovementSpeed),
        flying_speed: attr(m, Attr::FlyingSpeed),
        nav_done: m.nav.is_done(),
        max_head_y: m.ty.max_head_y(),
        head_rot_speed: m.ty.head_rot_speed(),
        max_head_x: m.ty.max_head_x(),
    }
}

// ---------------------------------------------------------------- travel

/// Types whose `travel` / `travelInWater` replace the standard one.
/// Returns None to run `control::travel`.
pub fn travel(m: &mut Mob, cx: &mut Ctx, ti: &TravelIn) -> Option<Moved> {
    let get = cx.get;
    match m.ty.name() {
        // Squid.travel: just move
        "squid" | "glow_squid" => {
            let (mut pos, mut vel) = (cx.pos, cx.vel);
            let r = control::do_move(get, &mut pos, &mut vel, ti, cx.on_ground, &mut m.mo);
            cx.pos = pos;
            cx.vel = vel;
            cx.on_ground = r.on_ground;
            Some(r)
        }
        // noPhysics: straight through blocks
        "vex" | "ender_dragon" => {
            let input = [m.mo.xxa, m.mo.yya, m.mo.zza];
            if m.ty.is("vex") {
                // travelInAir with no gravity (Vex.setNoGravity)
                let d = control::input_vector(input, 0.02, m.mo.yaw);
                for i in 0..3 {
                    cx.vel[i] += d[i];
                }
            }
            for i in 0..3 {
                cx.pos[i] += cx.vel[i];
            }
            if m.ty.is("vex") {
                cx.vel[0] *= 0.91;
                cx.vel[1] *= 0.98;
                cx.vel[2] *= 0.91;
            }
            cx.on_ground = false;
            m.mo.horizontal_collision = false;
            Some(Moved { on_ground: false, horizontal: false, landed_fall: None })
        }
        // Guardian / Turtle / Drowned.travelInWater
        "guardian" | "elder_guardian" | "turtle" | "drowned" if m.mo.in_water || control::fluid_heights(get, &Aabb::entity(cx.pos, ti.width, ti.height)).0 > 0.0 => {
            if m.ty.is("drowned") && !(m.mo.under_water && drowned_wants_to_swim(m, cx)) {
                return None;
            }
            let speed = if m.ty.is("drowned") { 0.01 } else { 0.1 };
            let d = control::input_vector([m.mo.xxa, m.mo.yya, m.mo.zza], speed, m.mo.yaw);
            for i in 0..3 {
                cx.vel[i] += d[i];
            }
            let (mut pos, mut vel) = (cx.pos, cx.vel);
            let r = control::do_move(get, &mut pos, &mut vel, ti, cx.on_ground, &mut m.mo);
            cx.pos = pos;
            cx.vel = [vel[0] * 0.9, vel[1] * 0.9, vel[2] * 0.9];
            cx.on_ground = r.on_ground;
            match m.ty.name() {
                "guardian" | "elder_guardian" => {
                    if m.v.flags & 2 == 0 && m.target.is_none() {
                        cx.vel[1] -= 0.005;
                    }
                }
                "turtle" => {
                    let near_home = m.v.home_pos.is_some_and(|h| dist2(cx.pos, [h[0] as f64 + 0.5, h[1] as f64 + 0.5, h[2] as f64 + 0.5]) < 400.0);
                    if m.target.is_none() && (!m.v.going_home || !near_home) {
                        cx.vel[1] -= 0.005;
                    }
                }
                _ => {}
            }
            Some(r)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- enderman

/// `EnderMan.teleport()`: a random spot within 32 blocks.
pub fn enderman_teleport(m: &mut Mob, cx: &mut Ctx) -> bool {
    let x = cx.pos[0] + (cx.rng.r#gen::<f64>() - 0.5) * 64.0;
    let y = cx.pos[1] + (cx.rng.gen_range(0..64) - 32) as f64;
    let z = cx.pos[2] + (cx.rng.r#gen::<f64>() - 0.5) * 64.0;
    teleport(m, cx, [x, y, z])
}

/// `EnderMan.teleportTowards`
pub fn enderman_teleport_towards(m: &mut Mob, cx: &mut Ctx, to: [f64; 3]) -> bool {
    let d = [cx.pos[0] - to[0], cx.pos[1] + m.height() * 0.5 - (to[1] + 1.62), cx.pos[2] - to[2]];
    let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-7);
    let d = [d[0] / l, d[1] / l, d[2] / l];
    let x = cx.pos[0] + (cx.rng.r#gen::<f64>() - 0.5) * 8.0 - d[0] * 16.0;
    let y = cx.pos[1] + (cx.rng.gen_range(0..16) - 8) as f64 - d[1] * 16.0;
    let z = cx.pos[2] + (cx.rng.r#gen::<f64>() - 0.5) * 8.0 - d[2] * 16.0;
    teleport(m, cx, [x, y, z])
}

/// `EnderMan.teleport(x, y, z)` + `LivingEntity.randomTeleport`
fn teleport(m: &mut Mob, cx: &mut Ctx, to: [f64; 3]) -> bool {
    let min_y = crate::world::chunk::MIN_Y;
    let mut p = bp(to);
    while p[1] > min_y && !super::path::blocks_motion(cx.block(p)) {
        p[1] -= 1;
    }
    let s = cx.block(p);
    if !super::path::blocks_motion(s) || vstate::is_water(s) {
        return false;
    }
    if random_teleport(m, cx, to) {
        let old = cx.pos;
        cx.acts.push(Act::Sound { name: "minecraft:entity.enderman.teleport".into(), pos: m.v.p0.unwrap_or(old), hostile: true, volume: 1.0, pitch: 1.0 });
        true
    } else {
        false
    }
}

/// `LivingEntity.randomTeleport(x, y, z, showParticles)`
pub fn random_teleport(m: &mut Mob, cx: &mut Ctx, to: [f64; 3]) -> bool {
    let min_y = crate::world::chunk::MIN_Y;
    let mut p = bp(to);
    let mut y = to[1];
    let mut found = false;
    while !found && p[1] > min_y {
        let below = [p[0], p[1] - 1, p[2]];
        if super::path::blocks_motion(cx.block(below)) {
            found = true;
        } else {
            y -= 1.0;
            p = below;
        }
    }
    if !found {
        return false;
    }
    let dest = [to[0], y, to[2]];
    let bb = Aabb::entity(dest, m.width(), m.height());
    let w = control::World(cx.get);
    if !crate::game::physics::no_collision(&w, &bb) || control::fluid_heights(cx.get, &bb) != (0.0, 0.0) {
        return false;
    }
    let old = cx.pos;
    cx.acts.push(Act::Sound { name: "minecraft:entity.enderman.teleport".into(), pos: old, hostile: true, volume: 1.0, pitch: 1.0 });
    cx.acts.push(Act::EntityEvent { eid: cx.eid, ev: 46 });
    cx.pos = dest;
    cx.vel = [0.0; 3];
    m.nav.stop();
    m.v.p0 = None;
    true
}

// ---------------------------------------------------------------- dragon

/// `EnderDragon.aiStep` flight with a holding pattern around the spawn
/// point and strafing runs at players (fireballs).
pub fn dragon_tick(m: &mut Mob, cx: &mut Ctx, node: &mut [f64; 3], phase_ticks: &mut i32, strafe: &mut i32) {
    let centre = m.v.bound.unwrap_or_else(|| {
        let p = cx.block_pos();
        m.v.bound = Some(p);
        p
    });
    *phase_ticks += 1;
    let target = m.target.and_then(|t| cx.find(t)).cloned();
    // pick a flight target
    let mut speed = 0.6f32;
    if *strafe > 0 {
        if let Some(t) = &target {
            *node = [t.pos[0], t.pos[1] + 2.0, t.pos[2]];
            speed = 0.6;
            let d = dist2(cx.pos, t.pos);
            *strafe -= 1;
            if d < 64.0 * 64.0 && m.can_see(cx, t) && *strafe % 20 == 0 && *strafe < 100 {
                // StrafePlayer: shoot a fireball from the head
                let view = control::view(0.0, m.mo.yaw);
                let head = [cx.pos[0] - view[0] * 1.0, cx.pos[1] + 2.5, cx.pos[2] - view[2] * 1.0];
                let dir = [t.pos[0] - head[0], t.pos[1] + 1.0 - head[1], t.pos[2] - head[2]];
                cx.acts.push(Act::LevelEvent { ev: 1017, pos: cx.block_pos(), data: 0 });
                cx.acts.push(Act::Fire { ty: PType::DragonFireball, owner: cx.eid, from: head, dir, target: None });
                *strafe = 0;
            }
        } else {
            *strafe = 0;
        }
    }
    if *strafe <= 0 && (dist2(cx.pos, *node) < 100.0 || dist2(cx.pos, *node) > 22500.0 || m.mo.horizontal_collision || *node == [0.0; 3]) {
        // HoldingPattern: next node on the circle
        let ang = (*phase_ticks as f64 * 0.05) + cx.rng.r#gen::<f64>() * 0.5;
        let r = 60.0;
        let top = cx.game.world.surface_y(centre[0], centre[2]).unwrap_or(centre[1]);
        *node = [centre[0] as f64 + ang.cos() * r, (top + 20) as f64 + cx.rng.r#gen::<f64>() * 20.0, centre[2] as f64 + ang.sin() * r];
        if target.is_some() && cx.rng.gen_range(0..3) == 0 {
            *strafe = 200;
        }
    }
    if target.is_none() && *phase_ticks % 20 == 0 {
        let me = &*m;
        m.target = me.nearest_player(cx, 150.0, true, false, &|_| true).map(|p| p.eid);
    }
    // flight
    let (xdd, mut ydd, zdd) = (node[0] - cx.pos[0], node[1] - cx.pos[1], node[2] - cx.pos[2]);
    let dist = xdd * xdd + ydd * ydd + zdd * zdd;
    let hd = (xdd * xdd + zdd * zdd).sqrt();
    if hd > 0.0 {
        ydd = (ydd / hd).clamp(-speed as f64, speed as f64);
    }
    cx.vel[1] += ydd * 0.01;
    m.mo.yaw = control::wrap(m.mo.yaw);
    let aim = {
        let l = dist.sqrt().max(1e-7);
        [xdd / l, (node[1] - cx.pos[1]) / l, zdd / l]
    };
    let yr = m.mo.yaw.to_radians() as f64;
    let dir = {
        let d = [yr.sin(), cx.vel[1], -yr.cos()];
        let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-7);
        [d[0] / l, d[1] / l, d[2] / l]
    };
    let dot = (((dir[0] * aim[0] + dir[1] * aim[1] + dir[2] * aim[2]) as f32 + 0.5) / 1.5).max(0.0);
    if xdd.abs() > 1e-5 || zdd.abs() > 1e-5 {
        let yrd = control::wrap(180.0 - xdd.atan2(zdd).to_degrees() as f32 - m.mo.yaw).clamp(-50.0, 50.0);
        let hs = (cx.vel[0] * cx.vel[0] + cx.vel[2] * cx.vel[2]).sqrt() as f32 + 1.0;
        let turn = 0.7 / hs.min(40.0) / hs;
        m.v.yrot_a *= 0.8;
        m.v.yrot_a += yrd * turn;
        m.mo.yaw += m.v.yrot_a * 0.1;
    }
    let span = (2.0 / (dist + 1.0)) as f32;
    let f = 0.06 * (dot * span + (1.0 - span));
    let d = control::input_vector([0.0, 0.0, -1.0], f, m.mo.yaw);
    for i in 0..3 {
        cx.vel[i] += d[i];
    }
    let mv = {
        let l = (cx.vel[0] * cx.vel[0] + cx.vel[1] * cx.vel[1] + cx.vel[2] * cx.vel[2]).sqrt().max(1e-7);
        [cx.vel[0] / l, cx.vel[1] / l, cx.vel[2] / l]
    };
    let slide = 0.8 + 0.15 * (mv[0] * dir[0] + mv[1] * dir[1] + mv[2] * dir[2] + 1.0) / 2.0;
    // movement happens in travel (noPhysics); apply the slide after it
    m.v.p0 = Some([slide, 0.91, slide]);
    m.mo.body_yaw = m.mo.yaw;
    m.mo.head_yaw = m.mo.yaw;
    // wings knock back, the head hurts (knockBack / hurt in aiStep)
    if m.living.hurt_time == 0 {
        let body = Aabb::entity([cx.pos[0], cx.pos[1] - 2.0, cx.pos[2]], 8.0, 4.0);
        let view = control::view(0.0, m.mo.yaw);
        let head_c = [cx.pos[0] - view[0] * 6.5, cx.pos[1] + 1.0, cx.pos[2] - view[2] * 6.5];
        let head = Aabb::entity([head_c[0], head_c[1], head_c[2]], 2.0, 2.0).inflate(1.0, 1.0, 1.0);
        let victims: Vec<Seen> = cx.seen.iter().filter(|s| s.eid != cx.eid && !s.spectator() && (s.player && s.attackable_player() || !s.player)).cloned().collect();
        for s in victims {
            if s.bbox().intersects(&body) {
                let (xd, zd) = (s.pos[0] - cx.pos[0], s.pos[2] - cx.pos[2]);
                let dd = (xd * xd + zd * zd).max(0.1);
                cx.acts.push(Act::Push { target: s.eid, vel: [xd / dd * 4.0, 0.2, zd / dd * 4.0] });
                if m.living.hurt_time == 0 {
                    cx.acts.push(Act::Hurt { target: s.eid, kind: "mob_attack", by: Some(cx.eid), amount: 5.0 });
                }
            }
            if s.bbox().intersects(&head) {
                cx.acts.push(Act::Hurt { target: s.eid, kind: "mob_attack", by: Some(cx.eid), amount: 10.0 });
            }
        }
    }
    // checkWalls: break what the body flies through
    if cx.mob_griefing {
        let bb = Aabb::entity([cx.pos[0], cx.pos[1] - 2.0, cx.pos[2]], 8.0, 6.0);
        let mut broke = false;
        for x in bb.min[0].floor() as i32..=bb.max[0].floor() as i32 {
            for y in bb.min[1].floor() as i32..=bb.max[1].floor() as i32 {
                for z in bb.min[2].floor() as i32..=bb.max[2].floor() as i32 {
                    let s = cx.block([x, y, z]);
                    if !s.is_air() && !vstate::has_fluid(s) && !cx.game.data.block_in("dragon_immune", s.name()) && super::path::blocks_motion(s) {
                        cx.acts.push(Act::Destroy { pos: [x, y, z], drops: false });
                        broke = true;
                    }
                }
            }
        }
        if broke {
            cx.acts.push(Act::LevelEvent { ev: 2008, pos: cx.block_pos(), data: 0 });
        }
    }
}

// ---------------------------------------------------------------- per-tick extras

/// `Mob.isSunBurnTick`
fn sun_burn_tick(m: &Mob, cx: &Ctx) -> bool {
    if !cx.bright() {
        return false;
    }
    let p = bp([cx.pos[0], cx.pos[1] + m.eye(), cx.pos[2]]);
    let br = cx.light_cost(p) + 0.5;
    let mut rng = rand::thread_rng();
    let wet = m.mo.in_water || cx.game.raining_at(p) || m.v.climbing && false;
    br > 0.5 && rng.r#gen::<f32>() * 30.0 < (br - 0.4) * 2.0 && !wet && !m.mo.in_powder_snow && cx.sky(p)
}

/// Undead that burn in daylight (`Mob.burnUndead` users).
fn burns_in_daylight(m: &Mob) -> bool {
    match m.ty.name() {
        "zombie" | "zombie_villager" | "drowned" | "skeleton" | "stray" | "bogged" | "phantom" => true,
        _ => false,
    }
}

/// `aiStep` work done before `travel` (velocity tweaks, burning, eggs...).
pub fn ai_step_pre(m: &mut Mob, cx: &mut Ctx) {
    let name = m.ty.name();
    // burning undead (Zombie / AbstractSkeleton / Phantom aiStep)
    if burns_in_daylight(m) && sun_burn_tick(m, cx) {
        let mut burn = true;
        if let Some(h) = m.eq[HEAD] {
            burn = false;
            let max = h.max_damage();
            if max > 0 {
                let mut h = h;
                let dmg = h.patch().damage.unwrap_or(0) + cx.rng.gen_range(0..2);
                if dmg >= max {
                    m.eq[HEAD] = None;
                    m.event.push(47);
                } else {
                    h.with_patch(|p| p.damage = Some(dmg));
                    m.eq[HEAD] = Some(h);
                }
            }
        }
        if burn && !m.ty.fire_immune() {
            m.fire = m.fire.max(160);
        }
    }
    match name {
        "blaze" => {
            if !cx.on_ground && cx.vel[1] < 0.0 {
                cx.vel[1] *= 0.6;
            }
        }
        "wither" => {
            cx.vel[1] *= 0.6;
            if let Some(t) = m.target.and_then(|t| cx.find(t)).cloned() {
                let powered = m.living.health <= m.living.max_health() / 2.0;
                let mut yd = cx.vel[1];
                if cx.pos[1] < t.pos[1] || !powered && cx.pos[1] < t.pos[1] + 5.0 {
                    yd = yd.max(0.0);
                    yd += 0.3 - yd * 0.6;
                }
                cx.vel[1] = yd;
                let d = [t.pos[0] - cx.pos[0], t.pos[2] - cx.pos[2]];
                if d[0] * d[0] + d[1] * d[1] > 9.0 {
                    let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
                    cx.vel[0] += d[0] / l * 0.3 - cx.vel[0] * 0.6;
                    cx.vel[2] += d[1] / l * 0.3 - cx.vel[2] * 0.6;
                }
            }
            if cx.vel[0] * cx.vel[0] + cx.vel[2] * cx.vel[2] > 0.05 {
                m.mo.yaw = cx.vel[2].atan2(cx.vel[0]).to_degrees() as f32 - 90.0;
            }
        }
        "spider" | "cave_spider" => m.v.climbing = m.mo.horizontal_collision,
        _ => {}
    }
}

/// `customServerAiStep` of the types that have one.
pub fn server_step(m: &mut Mob, cx: &mut Ctx) {
    match m.ty.name() {
        "enderman" => {
            if cx.bright() && m.tick_count >= m.v.target_change + 600 {
                let p = bp([cx.pos[0], cx.pos[1] + m.eye(), cx.pos[2]]);
                let br = cx.light_cost(cx.block_pos()) + 0.5;
                if br > 0.5 && cx.sky(p) && cx.rng.r#gen::<f32>() * 30.0 < (br - 0.4) * 2.0 {
                    m.target = None;
                    enderman_teleport(m, cx);
                }
            }
        }
        "blaze" => {
            m.v.height_change -= 1;
            if m.v.height_change <= 0 {
                m.v.height_change = 100;
                m.v.height_offset = (0.5 + super::special::triangle(&mut cx.rng, 0.5, 6.891)) as f32;
            }
            if let Some(t) = m.target.and_then(|t| cx.find(t)) {
                if t.pos[1] + t.eye > cx.pos[1] + m.eye() + m.v.height_offset as f64 {
                    cx.vel[1] += (0.3 - cx.vel[1]) * 0.3;
                }
            }
        }
        "elder_guardian" => {
            if (m.tick_count + cx.eid) % 1200 == 0 {
                cx.acts.push(Act::ElderCurse { pos: cx.pos });
            }
            if m.home.is_none() {
                m.home = Some((cx.block_pos(), 16));
            }
        }
        "guardian" => {}
        "bat" => bat_step(m, cx),
        "wither" => wither_step(m, cx),
        "vex" => {
            if m.v.life > 0 {
                m.v.life -= 1;
                if m.v.life <= 0 {
                    m.v.life = 20;
                    cx.acts.push(Act::Hurt { target: cx.eid, kind: "starve", by: None, amount: 1.0 });
                }
            }
        }
        "witch" => witch_step(m, cx),
        "creaking" => {}
        "warden" => {
            if m.tick_count % 20 == 0 {
                // the darkness pulse
                for s in cx.seen.iter().filter(|s| s.player && s.attackable_player() && dist2(s.pos, cx.pos) < 400.0) {
                    cx.acts.push(Act::Effect { target: s.eid, effect: "darkness", duration: 260, amp: 0 });
                }
            }
        }
        "piglin" | "piglin_brute" | "hoglin" => {
            // overworld zombification after 15 seconds
            if !m.no_ai {
                m.v.overworld_time += 1;
                if m.v.overworld_time > 300 {
                    let to = if m.ty.is("hoglin") { "zoglin" } else { "zombified_piglin" };
                    cx.acts.push(Act::Convert { eid: cx.eid, to, effect: Some(("nausea", 200)) });
                    sound(cx, &format!("entity.{}.converted_to_zombified", m.ty.name()), 1.0, 1.0, true);
                }
            }
        }
        _ => {}
    }
}

/// `Bat.customServerAiStep`
fn bat_step(m: &mut Mob, cx: &mut Ctx) {
    let pos = cx.block_pos();
    let above = [pos[0], pos[1] + 1, pos[2]];
    let conductor = |s: crate::registry::BlockState| super::path::solid_render(s);
    if m.v.resting {
        if conductor(cx.block(above)) {
            if cx.rng.gen_range(0..200) == 0 {
                m.mo.head_yaw = cx.rng.gen_range(0..360) as f32;
            }
            let near = cx.seen.iter().any(|s| s.player && s.attackable_player() && dist2(s.pos, cx.pos) < 16.0);
            if near {
                m.v.resting = false;
                cx.acts.push(Act::LevelEvent { ev: 1025, pos, data: 0 });
            }
        } else {
            m.v.resting = false;
            cx.acts.push(Act::LevelEvent { ev: 1025, pos, data: 0 });
        }
        return;
    }
    if let Some(t) = m.v.target_pos {
        if !cx.block(t).is_air() || t[1] <= crate::world::chunk::MIN_Y {
            m.v.target_pos = None;
        }
    }
    let close = m.v.target_pos.is_some_and(|t| dist2([t[0] as f64 + 0.5, t[1] as f64 + 0.5, t[2] as f64 + 0.5], cx.pos) < 4.0);
    if m.v.target_pos.is_none() || cx.rng.gen_range(0..30) == 0 || close {
        let x = cx.pos[0] + (cx.rng.gen_range(0..7) - cx.rng.gen_range(0..7)) as f64;
        let y = cx.pos[1] + cx.rng.gen_range(0..6) as f64 - 2.0;
        let z = cx.pos[2] + (cx.rng.gen_range(0..7) - cx.rng.gen_range(0..7)) as f64;
        m.v.target_pos = Some(bp([x, y, z]));
    }
    let t = m.v.target_pos.unwrap_or(pos);
    let d = [t[0] as f64 + 0.5 - cx.pos[0], t[1] as f64 + 0.1 - cx.pos[1], t[2] as f64 + 0.5 - cx.pos[2]];
    let v = cx.vel;
    let nv = [v[0] + (crate::game::physics::jsign(d[0]) * 0.5 - v[0]) * 0.1, v[1] + (crate::game::physics::jsign(d[1]) * 0.7 - v[1]) * 0.1, v[2] + (crate::game::physics::jsign(d[2]) * 0.5 - v[2]) * 0.1];
    cx.vel = nv;
    let yrd = nv[2].atan2(nv[0]).to_degrees() as f32 - 90.0;
    let diff = control::wrap(yrd - m.mo.yaw);
    m.mo.zza = 0.5;
    m.mo.yaw += diff;
    if cx.rng.gen_range(0..100) == 0 && conductor(cx.block(above)) {
        m.v.resting = true;
    }
}

/// `WitherBoss.customServerAiStep`
fn wither_step(m: &mut Mob, cx: &mut Ctx) {
    if m.v.invul > 0 {
        let n = m.v.invul - 1;
        if n <= 0 {
            cx.acts.push(Act::Explode { pos: [cx.pos[0], cx.pos[1] + m.eye(), cx.pos[2]], power: 7.0, fire: false, by: Some(cx.eid) });
            cx.acts.push(Act::GlobalEvent { ev: 1023, pos: cx.block_pos() });
        }
        m.v.invul = n;
        if m.tick_count % 10 == 0 {
            m.living.heal(10.0);
        }
        return;
    }
    for i in 1..3usize {
        if m.tick_count < m.v.next_head[i - 1] {
            continue;
        }
        m.v.next_head[i - 1] = m.tick_count + 10 + cx.rng.gen_range(0..10);
        if cx.difficulty >= 2 {
            let idle = m.v.idle_heads[i - 1];
            m.v.idle_heads[i - 1] += 1;
            if idle > 15 {
                let t = [cx.pos[0] + cx.rng.gen_range(-10.0..10.0), cx.pos[1] + cx.rng.gen_range(-5.0..5.0), cx.pos[2] + cx.rng.gen_range(-10.0..10.0)];
                super::special::wither_shoot(m, cx, i, 0, t, true);
                m.v.idle_heads[i - 1] = 0;
            }
        }
        let ht = m.v.alt_targets[i];
        if ht > 0 {
            let cur = cx.find(ht).cloned();
            match cur {
                Some(c) if m.can_attack_type(&c) && c.health > 0.0 && dist2(cx.pos, c.pos) <= 900.0 && m.can_see(cx, &c) => {
                    let to = [c.pos[0], c.pos[1] + c.eye * 0.5, c.pos[2]];
                    super::special::wither_shoot(m, cx, i, c.eid, to, false);
                    m.v.next_head[i - 1] = m.tick_count + 40 + cx.rng.gen_range(0..20);
                    m.v.idle_heads[i - 1] = 0;
                }
                _ => m.v.alt_targets[i] = 0,
            }
        } else {
            let bb = Aabb::entity(cx.pos, m.width(), m.height()).inflate(20.0, 8.0, 20.0);
            let list: Vec<i32> = cx.seen.iter().filter(|s| s.eid != cx.eid && s.bbox().intersects(&bb) && s.health > 0.0 && !s.undead && (s.player && s.attackable_player() || !s.player)).map(|s| s.eid).collect();
            if !list.is_empty() {
                m.v.alt_targets[i] = list[cx.rng.gen_range(0..list.len())];
            }
        }
    }
    m.v.alt_targets[0] = m.target.unwrap_or(0);
    if m.v.destroy_blocks > 0 {
        m.v.destroy_blocks -= 1;
        if m.v.destroy_blocks == 0 && cx.mob_griefing {
            let w = (m.width() / 2.0 + 1.0).floor() as i32;
            let h = m.height().floor() as i32;
            let p = cx.block_pos();
            let mut broke = false;
            for x in p[0] - w..=p[0] + w {
                for y in p[1]..=p[1] + h {
                    for z in p[2] - w..=p[2] + w {
                        let s = cx.block([x, y, z]);
                        if !s.is_air() && !cx.game.data.block_in("wither_immune", s.name()) {
                            cx.acts.push(Act::Destroy { pos: [x, y, z], drops: true });
                            broke = true;
                        }
                    }
                }
            }
            if broke {
                cx.acts.push(Act::LevelEvent { ev: 1022, pos: p, data: 0 });
            }
        }
    }
    if m.tick_count % 20 == 0 {
        m.living.heal(1.0);
    }
}

/// `Witch.aiStep`: drinking potions.
fn witch_step(m: &mut Mob, cx: &mut Ctx) {
    if m.v.using_ticks > 0 {
        m.v.using_ticks -= 1;
        if m.v.using_ticks <= 0 {
            if let Some(p) = m.v.drink.take() {
                for (e, dur, amp) in crate::game::consume::potion_effects(&p).iter().copied() {
                    cx.acts.push(Act::EffectRaw { target: cx.eid, effect: e, duration: dur, amp });
                }
            }
            m.eq[MAINHAND] = None;
            m.living.attrs.remove_modifier(Attr::MovementSpeed, "minecraft:drinking");
            m.v.charging = false;
        }
        return;
    }
    let target = m.target.and_then(|t| cx.find(t)).cloned();
    let r1 = cx.rng.r#gen::<f32>();
    let eye_in_water = m.mo.under_water;
    let potion = if r1 < 0.15 && eye_in_water && !m.living.has_effect(Effect::WaterBreathing) {
        Some("water_breathing")
    } else if cx.rng.r#gen::<f32>() < 0.15 && (m.fire > 0 || m.living.last_damage.as_ref().is_some_and(|(s, _)| s.is("is_fire"))) && !m.living.has_effect(Effect::FireResistance) {
        Some("fire_resistance")
    } else if cx.rng.r#gen::<f32>() < 0.05 && m.living.health < m.living.max_health() {
        Some("healing")
    } else if cx.rng.r#gen::<f32>() < 0.5 && target.as_ref().is_some_and(|t| dist2(cx.pos, t.pos) > 121.0) && !m.living.has_effect(Effect::Speed) {
        Some("swiftness")
    } else {
        None
    };
    if let Some(p) = potion {
        if let Some(id) = crate::data::Items::get().id("potion") {
            let mut s = crate::data::Stack::new(id, 1);
            s.with_patch(|pa| pa.potion = Some(format!("minecraft:{p}")));
            m.eq[MAINHAND] = Some(s);
        }
        m.v.drink = Some(p.to_string());
        m.v.using_ticks = 32;
        m.v.charging = true; // DATA_USING_ITEM
        let pi = 0.8 + cx.rng.r#gen::<f32>() * 0.4;
        sound(cx, "entity.witch.drink", 1.0, pi, true);
        m.living.attrs.add_modifier(Attr::MovementSpeed, crate::game::combat::attr::Modifier { id: "minecraft:drinking".into(), amount: -0.25, op: crate::game::combat::attr::Op::AddValue });
    }
    if cx.rng.r#gen::<f32>() < 7.5e-4 {
        m.event.push(15);
    }
}

/// `aiStep` / `tick` work after `travel`.
pub fn ai_step_post(m: &mut Mob, cx: &mut Ctx, landed: bool) {
    let name = m.ty.name();
    match name {
        "creeper" => creeper_tick(m, cx),
        "chicken" => {
            if !cx.on_ground && cx.vel[1] < 0.0 {
                cx.vel[1] *= 0.6;
            }
            m.v.egg_time -= 1;
            if !m.is_baby() && !m.v.chicken_jockey && m.v.egg_time <= 0 {
                let p = pitch(cx);
                sound(cx, "entity.chicken.egg", 1.0, p, false);
                let egg = match m.v.variant {
                    1 => "blue_egg",
                    2 => "brown_egg",
                    _ => "egg",
                };
                if let Some(id) = crate::data::Items::get().id(egg) {
                    cx.acts.push(Act::Drop { pos: cx.pos, stack: crate::data::Stack::new(id, 1) });
                }
                m.v.egg_time = cx.rng.gen_range(0..6000) + 6000;
            }
        }
        "bat" => {
            if m.v.resting {
                cx.vel = [0.0; 3];
                cx.pos[1] = cx.pos[1].floor() + 1.0 - m.height();
            } else {
                cx.vel[1] *= 0.6;
            }
        }
        "squid" | "glow_squid" => squid_step(m, cx),
        "slime" | "magma_cube" => {
            if landed && m.size > 0 {
                let name = if m.ty.is("magma_cube") { "entity.magma_cube.squish" } else if m.size <= 1 { "entity.slime.squish_small" } else { "entity.slime.squish" };
                let p = slime_pitch(m, cx) / 0.8;
                sound(cx, name, 0.4 * m.size as f32, p, true);
            }
            if m.ty.is("magma_cube") && m.mo.no_jump_delay == 10 && cx.vel[1] > 0.0 && !m.v.was_ground {
                cx.vel[1] += 0.1 * m.size as f64;
            }
            slime_touch(m, cx);
        }
        "ghast" => {}
        "guardian" | "elder_guardian" => {
            if m.mo.in_water {
                m.air = 300;
            } else if cx.on_ground {
                cx.vel[0] += ((cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.4) as f64;
                cx.vel[1] += 0.5;
                cx.vel[2] += ((cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.4) as f64;
                m.mo.yaw = cx.rng.r#gen::<f32>() * 360.0;
                cx.on_ground = false;
            }
            if m.v.beam_target != 0 {
                m.mo.yaw = m.mo.head_yaw;
            }
        }
        "cod" | "salmon" | "pufferfish" | "tropical_fish" | "tadpole" => {
            if !m.mo.in_water && cx.on_ground && m.mo.vertical_collision {
                cx.vel[0] += ((cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.05) as f64;
                cx.vel[1] += 0.4;
                cx.vel[2] += ((cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.05) as f64;
                cx.on_ground = false;
                let f = format!("entity.{}.flop", if name == "tadpole" { "tadpole" } else { name });
                sound(cx, &f, 1.0, 1.0, false);
            }
            if name == "pufferfish" {
                pufferfish_tick(m, cx);
            }
        }
        "ender_dragon" => {
            if let Some(s) = m.v.p0 {
                cx.vel = [cx.vel[0] * s[0], cx.vel[1] * s[1], cx.vel[2] * s[2]];
            }
        }
        "iron_golem" => {
            if m.v.attack_anim > 0 {
                m.v.attack_anim -= 1;
            }
            if m.v.offer_flower > 0 {
                m.v.offer_flower -= 1;
            }
        }
        "ravager" => {
            if m.v.attack_anim > 0 {
                m.v.attack_anim -= 1;
            }
            if m.v.stun > 0 {
                m.v.stun -= 1;
                if m.v.stun == 0 {
                    ravager_roar(m, cx);
                }
            }
            if m.v.roar > 0 {
                m.v.roar -= 1;
                if m.v.roar == 10 {
                    ravager_roar(m, cx);
                }
            }
            // break leaves it walks into
            if m.mo.horizontal_collision && cx.mob_griefing {
                let bb = Aabb::entity(cx.pos, m.width(), m.height()).inflate(0.2, 0.0, 0.2);
                for x in bb.min[0].floor() as i32..=bb.max[0].floor() as i32 {
                    for y in bb.min[1].floor() as i32..=bb.max[1].floor() as i32 {
                        for z in bb.min[2].floor() as i32..=bb.max[2].floor() as i32 {
                            if cx.game.data.block_in("leaves", cx.block([x, y, z]).name()) {
                                cx.acts.push(Act::Destroy { pos: [x, y, z], drops: true });
                            }
                        }
                    }
                }
            }
        }
        "hoglin" | "zoglin" => {
            if m.v.attack_anim > 0 {
                m.v.attack_anim -= 1;
            }
        }
        "sheep" => {
            if m.v.eat_tick > 0 {
                m.v.eat_tick -= 1;
            }
        }
        "turtle" => {
            if m.is_baby() && m.v.home_pos.is_none() {
                m.v.home_pos = Some(cx.block_pos());
            }
            if m.v.home_pos.is_none() {
                m.v.home_pos = Some(cx.block_pos());
            }
        }
        "dolphin" => {
            if m.mo.in_water || cx.game.raining_at(cx.block_pos()) {
                m.v.moistness = 2400;
            } else {
                m.v.moistness -= 1;
                if m.v.moistness <= 0 {
                    cx.acts.push(Act::Hurt { target: cx.eid, kind: "dry_out", by: None, amount: 1.0 });
                }
                if cx.on_ground {
                    cx.vel[0] += ((cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.2) as f64;
                    cx.vel[1] += 0.5;
                    cx.vel[2] += ((cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.2) as f64;
                    m.mo.yaw = cx.rng.r#gen::<f32>() * 360.0;
                    cx.on_ground = false;
                }
            }
        }
        "axolotl" => {
            if m.mo.in_water || cx.game.raining_at(cx.block_pos()) {
                m.air = 6000;
            } else {
                m.air -= 1;
                if m.air == -20 {
                    m.air = 0;
                    cx.acts.push(Act::Hurt { target: cx.eid, kind: "dry_out", by: None, amount: 2.0 });
                }
            }
        }
        "snow_golem" => {
            // melts in hot biomes; leaves snow behind
            let p = cx.block_pos();
            let temp = temp_at(cx, p);
            if temp > 1.0 {
                cx.acts.push(Act::Hurt { target: cx.eid, kind: "on_fire", by: None, amount: 1.0 });
            }
            if cx.mob_griefing {
                for i in 0..4 {
                    let x = (cx.pos[0] + ((i % 2) as f64 * 2.0 - 1.0) * 0.25).floor() as i32;
                    let z = (cx.pos[2] + ((i / 2 % 2) as f64 * 2.0 - 1.0) * 0.25).floor() as i32;
                    let q = [x, p[1], z];
                    let below = cx.block([x, p[1] - 1, z]);
                    if cx.block(q).is_air() && temp_at(cx, q) < 0.8 && super::path::solid_render(below) {
                        cx.acts.push(Act::SetBlock { pos: q, state: crate::registry::BlockState::by_name("snow"), drops: false });
                    }
                }
            }
        }
        "strider" => {
            let cold = cx.block(cx.block_pos()).name() != "lava" && cx.block([cx.block_pos()[0], cx.block_pos()[1] - 1, cx.block_pos()[2]]).name() != "lava";
            m.v.suffocating = cold;
        }
        _ => {}
    }
    m.v.was_ground = cx.on_ground;
}

/// `Ravager.roar`: knocks back everything around it.
fn ravager_roar(m: &mut Mob, cx: &mut Ctx) {
    let bb = Aabb::entity(cx.pos, m.width(), m.height()).inflate(4.0, 4.0, 4.0);
    let list: Vec<Seen> = cx.seen.iter().filter(|s| s.eid != cx.eid && s.bbox().intersects(&bb) && !s.raider).cloned().collect();
    for s in list {
        if !s.player || s.attackable_player() {
            cx.acts.push(Act::Hurt { target: s.eid, kind: "mob_attack", by: Some(cx.eid), amount: 6.0 });
        }
        let d = [s.pos[0] - cx.pos[0], s.pos[2] - cx.pos[2]];
        let l = (d[0] * d[0] + d[1] * d[1]).max(0.001);
        cx.acts.push(Act::Push { target: s.eid, vel: [d[0] / l * 4.0, 0.2, d[1] / l * 4.0] });
    }
    cx.acts.push(Act::EntityEvent { eid: cx.eid, ev: 39 });
    sound(cx, "entity.ravager.roar", 1.0, 1.0, true);
}

/// `Creeper.tick`: the fuse.
fn creeper_tick(m: &mut Mob, cx: &mut Ctx) {
    m.v.old_swell = m.v.swell;
    if m.v.ignited {
        m.v.swell_dir = 1;
    }
    let dir = m.v.swell_dir;
    if dir > 0 && m.v.swell == 0 {
        sound(cx, "entity.creeper.primed", 1.0, 0.5, true);
    }
    m.v.swell = (m.v.swell + dir).max(0);
    if m.v.swell >= 30 {
        m.v.swell = 30;
        let power = if m.v.powered { 6.0 } else { 3.0 };
        cx.acts.push(Act::Explode { pos: cx.pos, power, fire: false, by: Some(cx.eid) });
        cx.acts.push(Act::Remove { eid: cx.eid });
        m.death = 1;
    }
}

/// `Squid.aiStep`: tentacle-driven swimming.
fn squid_step(m: &mut Mob, cx: &mut Ctx) {
    if m.v.tent_speed == 0.0 {
        m.v.tent_speed = 1.0 / (cx.rng.r#gen::<f32>() + 1.0) * 0.2;
    }
    m.v.tent += m.v.tent_speed;
    let tau = std::f32::consts::TAU;
    if m.v.tent > tau {
        m.v.tent -= tau;
        if cx.rng.gen_range(0..10) == 0 {
            m.v.tent_speed = 1.0 / (cx.rng.r#gen::<f32>() + 1.0) * 0.2;
        }
        m.event.push(19);
    }
    if m.mo.in_water {
        if m.v.tent < std::f32::consts::PI {
            let s = m.v.tent / std::f32::consts::PI;
            if s > 0.75 {
                if let Some(v) = m.v.p0 {
                    cx.vel = v;
                }
                m.v.rot_speed = 1.0;
            } else {
                m.v.rot_speed *= 0.8;
            }
        } else {
            for v in &mut cx.vel {
                *v *= 0.9;
            }
            m.v.rot_speed *= 0.99;
        }
        let v = cx.vel;
        m.mo.body_yaw += (-(v[0].atan2(v[2]).to_degrees() as f32) - m.mo.body_yaw) * 0.1;
        m.mo.yaw = m.mo.body_yaw;
    } else {
        let mut yd = cx.vel[1];
        if let Some(amp) = m.living.effect(Effect::Levitation).map(|i| i.amplifier) {
            yd = 0.05 * (amp + 1) as f64;
        } else {
            yd -= 0.08;
        }
        cx.vel = [0.0, yd * 0.98, 0.0];
    }
}

/// `Pufferfish.tick`: puffing up and down.
fn pufferfish_tick(m: &mut Mob, cx: &mut Ctx) {
    if m.v.inflate_ticks > 0 {
        if m.size == 0 {
            sound(cx, "entity.puffer_fish.blow_up", 1.0, 1.0, false);
            m.size = 1;
        } else if m.v.inflate_ticks > 40 && m.size == 1 {
            sound(cx, "entity.puffer_fish.blow_up", 1.0, 1.0, false);
            m.size = 2;
        }
        m.v.inflate_ticks += 1;
    } else if m.size != 0 {
        if m.v.deflate_timer > 60 && m.size == 2 {
            sound(cx, "entity.puffer_fish.blow_out", 1.0, 1.0, false);
            m.size = 1;
        } else if m.v.deflate_timer > 100 && m.size == 1 {
            sound(cx, "entity.puffer_fish.blow_out", 1.0, 1.0, false);
            m.size = 0;
        }
        m.v.deflate_timer += 1;
    }
    // touch: stings what is close
    if m.size > 0 {
        let bb = Aabb::entity(cx.pos, m.width(), m.height()).inflate(0.3, 0.3, 0.3);
        let list: Vec<Seen> = cx.seen.iter().filter(|s| s.eid != cx.eid && s.bbox().intersects(&bb) && s.health > 0.0).cloned().collect();
        for s in list {
            if s.player && !s.attackable_player() || !s.player && MobTypeIs::water(&s) {
                continue;
            }
            cx.acts.push(Act::Hurt { target: s.eid, kind: "mob_attack", by: Some(cx.eid), amount: (1 + m.size) as f32 });
            cx.acts.push(Act::Effect { target: s.eid, effect: "poison", duration: 60 * m.size, amp: 0 });
            if s.player {
                cx.acts.push(Act::GameEvent { player: s.eid, event: 9, value: 0.0 });
            }
            sound(cx, "entity.puffer_fish.sting", 1.0, 1.0, false);
        }
    }
}

struct MobTypeIs;
impl MobTypeIs {
    fn water(s: &Seen) -> bool {
        crate::game::mob::MobType::from_name(s.ty).is_some_and(|t| t.is_water_animal())
    }
}

/// `Slime.playerTouch` / `push(IronGolem)`
fn slime_touch(m: &mut Mob, cx: &mut Ctx) {
    let deals = m.ty.is("magma_cube") || m.size > 1;
    if !deals || m.death > 0 {
        return;
    }
    let me = Aabb::entity(cx.pos, m.width(), m.height());
    let list: Vec<Seen> = cx.seen.iter().filter(|s| (s.player && s.attackable_player() || s.ty == "iron_golem") && s.health > 0.0).cloned().collect();
    for s in list {
        // Player.touch: player box inflated (1, 0.5, 1); golems by push
        let touching = if s.player { s.bbox().inflate(1.0, 0.5, 1.0).intersects(&me) } else { s.bbox().intersects(&me) };
        if touching && m.in_melee_range(cx, &s) && m.can_see(cx, &s) {
            let dmg = attr(m, Attr::AttackDamage) as f32;
            cx.acts.push(Act::SlimeHit { slime: cx.eid, target: s.eid, amount: dmg });
        }
    }
}

/// Mobs that split on death (`Slime.remove`).
pub fn on_death(m: &Mob, pos: [f64; 3], rng: &mut impl Rng) -> Vec<(i32, [f64; 3], f32)> {
    let mut out = Vec::new();
    if (m.ty.is("slime") || m.ty.is("magma_cube")) && m.size > 1 {
        let w = m.width() as f32;
        let off = w / 2.0;
        let half = m.size / 2;
        let count = 2 + rng.gen_range(0..3);
        for i in 0..count {
            let xd = ((i % 2) as f32 - 0.5) * off;
            let zd = ((i / 2) as f32 - 0.5) * off;
            out.push((half, [pos[0] + xd as f64, pos[1] + 0.5, pos[2] + zd as f64], rng.r#gen::<f32>() * 360.0));
        }
    }
    out
}

/// `LivingEntity.baseTick` air + `Entity.baseTick` fire + blocks the mob is
/// in; returns self damage.
pub fn environment(m: &mut Mob, cx: &mut Ctx) -> Vec<(crate::game::combat::damage::DamageSource, f32)> {
    use crate::game::combat::damage::DamageSource;
    let mut out = Vec::new();
    let bb = Aabb::entity(cx.pos, m.width(), m.height());
    let inside = crate::game::hazards::inside(cx.get, &bb);
    let eye = bp([cx.pos[0], cx.pos[1] + m.eye(), cx.pos[2]]);
    let eye_water = vstate::is_water(cx.block(eye));
    // air
    let name = m.ty.name();
    if m.ty.water_breather() {
        // WaterAnimal.handleAirSupply
        if !inside.water {
            m.air -= 1;
            if m.air == -20 {
                m.air = 0;
                out.push((DamageSource::new("drown"), 2.0));
            }
        } else {
            m.air = 300;
        }
    } else if name == "dolphin" {
        if eye_water {
            m.air -= 1;
            if m.air <= -20 {
                m.air = 0;
                out.push((DamageSource::new("drown"), 2.0));
            }
        } else {
            m.air = 4800;
        }
    } else if name != "axolotl" {
        let breathing = m.ty.breathes_underwater() || m.living.has_effect(Effect::WaterBreathing) || m.living.has_effect(Effect::ConduitPower);
        if eye_water && !breathing {
            m.air -= 1;
            if m.air <= -20 {
                m.air = 0;
                out.push((DamageSource::new("drown"), 2.0));
            }
        } else if m.air < 300 {
            m.air = (m.air + 4).min(300);
        }
    }
    // water / rain sensitivity
    if m.ty.water_sensitive() && (inside.water || cx.game.raining_at(cx.block_pos())) {
        out.push((DamageSource::new("drown"), 1.0));
    }
    // fire
    let fire_immune = m.ty.fire_immune() || m.living.has_effect(Effect::FireResistance);
    if m.fire > 0 {
        if fire_immune {
            m.fire = (m.fire - 4).max(0);
        } else {
            if m.fire % 20 == 0 && !inside.lava {
                out.push((DamageSource::new("on_fire"), 1.0));
            }
            m.fire -= 1;
        }
    }
    let freeze_immune = m.ty.is("stray") || m.ty.is("polar_bear") || m.ty.is("snow_golem") || m.ty.is("wither") || m.ty.is("strider") || m.ty.is("blaze") || m.ty.is("magma_cube") || m.ty.is("ghast") || m.ty.is("ender_dragon");
    if inside.powder_snow {
        if !freeze_immune {
            m.frozen = (m.frozen + 1).min(140);
        }
        // skeletons in powder snow become strays
        if m.ty.is("skeleton") {
            m.v.in_water_time += 1;
            if m.v.in_water_time >= 140 && m.v.conversion == 0 {
                m.v.conversion = 300;
            }
        }
    } else {
        if m.frozen > 0 {
            m.frozen = (m.frozen - 2).max(0);
        }
        if m.ty.is("skeleton") && m.v.conversion == 0 {
            m.v.in_water_time = 0;
        }
    }
    if let Some(d) = inside.fire {
        m.frozen = 0;
        if !fire_immune {
            m.fire = m.fire.max(160);
        }
        out.push((DamageSource::new("in_fire"), d));
    }
    if inside.lava {
        if !m.ty.fire_immune() {
            m.fire = m.fire.max(300);
        }
        if !(m.ty.is("strider")) {
            out.push((DamageSource::new("lava"), 4.0));
        }
    }
    if inside.water || inside.powder_snow {
        m.fire = 0;
    }
    if let Some(d) = inside.campfire {
        out.push((DamageSource::new("campfire"), d));
    }
    if inside.cactus {
        out.push((DamageSource::new("cactus"), 1.0));
    }
    if inside.berry_bush && !matches!(name, "fox" | "bee") && (cx.vel[0].abs() > 0.003 || cx.vel[2].abs() > 0.003) {
        out.push((DamageSource::new("sweet_berry_bush"), 1.0));
    }
    if inside.wither_rose && cx.difficulty != 0 && !m.ty.is("wither") {
        cx.acts.push(Act::Effect { target: cx.eid, effect: "wither", duration: 40, amp: 0 });
    }
    let below = cx.block([cx.block_pos()[0], (cx.pos[1] - 0.2).floor() as i32, cx.block_pos()[2]]);
    if cx.on_ground && crate::game::hazards::hot_floor(below, false) && !fire_immune {
        out.push((DamageSource::new("hot_floor"), 1.0));
    }
    if m.frozen >= 140 && !freeze_immune && m.tick_count % 40 == 0 {
        let amount = if matches!(name, "stray" | "polar_bear" | "snow_golem") { 1.0 } else if matches!(name, "blaze" | "magma_cube" | "strider") { 5.0 } else { 1.0 };
        out.push((DamageSource::new("freeze"), amount));
    }
    // zombie conversions (drowning zombie -> drowned, husk -> zombie)
    if matches!(name, "zombie" | "husk") && !m.no_ai {
        if m.v.conversion > 0 {
            m.v.conversion -= 1;
            if m.v.conversion <= 0 {
                let to = if name == "husk" { "zombie" } else { "drowned" };
                cx.acts.push(Act::Convert { eid: cx.eid, to, effect: None });
                let ev = if name == "husk" { 1041 } else { 1040 };
                cx.acts.push(Act::LevelEvent { ev, pos: cx.block_pos(), data: 0 });
            }
        } else if eye_water {
            m.v.in_water_time += 1;
            if m.v.in_water_time >= 600 {
                m.v.conversion = 300;
                m.v.flags |= 8; // DATA_DROWNED_CONVERSION_ID
            }
        } else {
            m.v.in_water_time = -1;
        }
    }
    if m.ty.is("skeleton") && m.v.conversion > 0 {
        m.v.conversion -= 1;
        if m.v.conversion <= 0 {
            cx.acts.push(Act::Convert { eid: cx.eid, to: "stray", effect: None });
            cx.acts.push(Act::LevelEvent { ev: 1048, pos: cx.block_pos(), data: 0 });
        }
    }
    let _ = inside.cobweb;
    out
}

fn temp_at(cx: &Ctx, p: [i32; 3]) -> f32 {
    let b = cx.game.world.biome(p[0], p[1], p[2]).unwrap_or_default();
    crate::game::weather::temperature(&b, p[0], p[1], p[2])
}
