//! Movement checks: Fly, Speed, NoFall, Timer.
//!
//! The checker simulates vanilla vertical physics (gravity 0.08, drag 0.98)
//! and compares the client's movement against it. It only ever flags movement
//! that is *more* than physics allows (rising, hovering, too fast); falling is
//! never a fly violation, which is what makes elytra falls, post-glide drops
//! and similar cases safe.
//!
//! Exemptions (with a grace period after they end): creative/spectator flight,
//! elytra gliding, riptide, levitation, liquids, climbing, cobwebs, vehicles,
//! server-applied velocity (knockback, explosions, wind charges), teleports,
//! bubble columns, slime/bed bounces, and powder snow.

use super::Check;

const GRAVITY: f64 = 0.08;
const DRAG: f64 = 0.98;
const SLOW_FALL_GRAVITY: f64 = 0.01;
const JUMP_VELOCITY: f64 = 0.42;
const STEP_HEIGHT: f64 = 0.6;
/// ticks of grace after an exemption ends
const GRACE: u32 = 20;

/// Per-tick state of the player as known by the *server*.
#[derive(Clone, Copy, Debug, Default)]
pub struct MoveContext {
    pub may_fly: bool,
    pub gliding: bool,
    pub riptide: bool,
    pub levitation: bool,
    pub slow_falling: bool,
    pub in_liquid: bool,
    pub climbing: bool,
    pub in_web_or_powder_snow: bool,
    pub in_vehicle: bool,
    pub bounced: bool,
    /// server applied velocity this tick (knockback, explosion, …)
    pub server_velocity: bool,
    pub teleporting: bool,
    pub speed_amplifier: u8,
    pub jump_boost: u8,
    pub on_ice: bool,
    pub sprinting: bool,
    /// is there a solid block directly under the player's feet (server world)?
    pub solid_below: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Flag {
    pub check: Check,
    pub amount: f64,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MoveResult {
    pub flags: Vec<Flag>,
    /// fall damage the server must apply (computed server-side; defeats NoFall)
    pub fall_damage: f32,
}

#[derive(Debug)]
pub struct MovementChecker {
    pub last_legit: [f64; 3],
    last_pos: Option<[f64; 3]>,
    last_dy: f64,
    air_ticks: u32,
    hover_ticks: u32,
    grace: u32,
    speed_buffer: f64,
    fly_buffer: f64,
    fall_distance: f64,
    // timer
    timer_balance: f64,
    last_packet_ms: Option<u64>,
}

impl Default for MovementChecker {
    fn default() -> Self {
        Self {
            last_legit: [0.0; 3],
            last_pos: None,
            last_dy: 0.0,
            air_ticks: 0,
            hover_ticks: 0,
            grace: GRACE,
            speed_buffer: 0.0,
            fly_buffer: 0.0,
            fall_distance: 0.0,
            timer_balance: 0.0,
            last_packet_ms: None,
        }
    }
}

fn finite(p: &[f64; 3]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() < 3.0e7)
}

impl MovementChecker {
    /// Timer check: each movement packet represents 50 ms of game time.
    /// Lag bursts are tolerated because the balance may go negative (bounded),
    /// but sending faster than real time for long builds positive balance.
    pub fn on_move_packet(&mut self, now_ms: u64) -> Option<Flag> {
        if let Some(last) = self.last_packet_ms {
            let elapsed = now_ms.saturating_sub(last) as f64;
            self.timer_balance = (self.timer_balance + 50.0 - elapsed).max(-1000.0);
        }
        self.last_packet_ms = Some(now_ms);
        if self.timer_balance > 250.0 {
            let excess = self.timer_balance;
            self.timer_balance = 0.0;
            return Some(Flag { check: Check::Timer, amount: (excess / 100.0).min(5.0), detail: format!("{excess:.0}ms ahead") });
        }
        None
    }

    /// Check one movement. Returns flags and server-computed fall damage.
    pub fn on_move(&mut self, to: [f64; 3], ctx: &MoveContext) -> MoveResult {
        let mut res = MoveResult::default();
        if !finite(&to) {
            // NaN/Infinity/huge coordinates crash other clients and servers
            res.flags.push(Flag { check: Check::Speed, amount: 10.0, detail: "invalid coordinates".into() });
            return res;
        }
        let Some(from) = self.last_pos else {
            self.last_pos = Some(to);
            self.last_legit = to;
            return res;
        };
        let dx = to[0] - from[0];
        let dy = to[1] - from[1];
        let dz = to[2] - from[2];
        let horizontal = (dx * dx + dz * dz).sqrt();

        let exempt = ctx.may_fly
            || ctx.gliding
            || ctx.riptide
            || ctx.levitation
            || ctx.in_liquid
            || ctx.climbing
            || ctx.in_web_or_powder_snow
            || ctx.in_vehicle
            || ctx.bounced
            || ctx.server_velocity
            || ctx.teleporting;
        if exempt {
            self.grace = GRACE;
        } else if self.grace > 0 {
            self.grace -= 1;
        }

        // ---------------------------------------------------------- fall damage
        if ctx.gliding || ctx.in_liquid || ctx.climbing || ctx.may_fly || ctx.in_web_or_powder_snow || ctx.slow_falling || ctx.riptide || ctx.teleporting {
            self.fall_distance = 0.0;
        } else if dy < 0.0 {
            self.fall_distance -= dy;
        }
        if ctx.solid_below {
            if self.fall_distance > 3.0 && !ctx.bounced {
                res.fall_damage = (self.fall_distance - 3.0).ceil() as f32;
            }
            self.fall_distance = 0.0;
        }

        let in_air = !ctx.solid_below;
        if in_air {
            self.air_ticks += 1;
        } else {
            self.air_ticks = 0;
            self.hover_ticks = 0;
        }

        if self.grace == 0 && !exempt {
            // ---------------------------------------------------------- fly
            if in_air {
                let g = if ctx.slow_falling { SLOW_FALL_GRAVITY } else { GRAVITY };
                let max_dy = if self.air_ticks == 1 {
                    // leaving the ground: jump (or step-up) is allowed
                    JUMP_VELOCITY + 0.1 * ctx.jump_boost as f64 + 0.01
                } else {
                    (self.last_dy - g) * DRAG + 0.03
                };
                if dy > max_dy && dy > 0.0 {
                    self.fly_buffer += 1.0;
                    if self.fly_buffer > 2.0 {
                        res.flags.push(Flag { check: Check::Fly, amount: 1.0 + (dy - max_dy).min(4.0), detail: format!("dy={dy:.3} max={max_dy:.3}") });
                    }
                } else {
                    self.fly_buffer = (self.fly_buffer - 0.1).max(0.0);
                }
                // hovering: no vertical motion for many airborne ticks
                if dy.abs() < 0.003 && self.air_ticks > 3 {
                    self.hover_ticks += 1;
                    if self.hover_ticks > 8 {
                        res.flags.push(Flag { check: Check::Fly, amount: 1.0, detail: "hover".into() });
                    }
                } else {
                    self.hover_ticks = 0;
                }
            } else if dy > STEP_HEIGHT + 0.01 + JUMP_VELOCITY {
                res.flags.push(Flag { check: Check::Fly, amount: 1.0, detail: format!("step dy={dy:.3}") });
            }

            // ---------------------------------------------------------- speed
            let mut limit = if in_air { 0.62 } else if ctx.sprinting { 0.36 } else { 0.29 };
            limit *= 1.0 + 0.2 * ctx.speed_amplifier as f64;
            if ctx.on_ice {
                limit *= 2.6;
            }
            if horizontal > limit {
                self.speed_buffer += (horizontal - limit) * 4.0;
            } else {
                self.speed_buffer = (self.speed_buffer - 0.05).max(0.0);
            }
            if self.speed_buffer > 1.0 {
                res.flags.push(Flag { check: Check::Speed, amount: self.speed_buffer.min(5.0), detail: format!("h={horizontal:.3} max={limit:.3}") });
                self.speed_buffer = 0.5;
            }
        } else {
            self.fly_buffer = 0.0;
            self.hover_ticks = 0;
        }
        // elytra speed sanity (fireworks ≈ 1.7–3.3 b/t; beyond 6 b/t is impossible)
        if ctx.gliding && (horizontal * horizontal + dy * dy).sqrt() > 6.0 && !ctx.server_velocity && !ctx.teleporting {
            res.flags.push(Flag { check: Check::Speed, amount: 2.0, detail: "elytra speed".into() });
        }

        self.last_dy = if in_air { dy } else { 0.0 };
        self.last_pos = Some(to);
        if res.flags.is_empty() {
            self.last_legit = to;
        }
        res
    }

    /// NoFall: the client claims to be on the ground but the server world has
    /// no block there. Fall damage is applied server-side anyway, so this check
    /// only serves for logging/punishment.
    pub fn check_ground_spoof(&self, claims_on_ground: bool, ctx: &MoveContext) -> Option<Flag> {
        if claims_on_ground && !ctx.solid_below && !ctx.in_liquid && !ctx.climbing && !ctx.in_vehicle && !ctx.teleporting && self.fall_distance > 1.0 {
            return Some(Flag { check: Check::NoFall, amount: 1.0, detail: format!("fall={:.1}", self.fall_distance) });
        }
        None
    }

    /// Server teleported the player; reset tracking.
    pub fn teleported(&mut self, to: [f64; 3]) {
        self.last_pos = Some(to);
        self.last_legit = to;
        self.last_dy = 0.0;
        self.grace = GRACE;
        self.fall_distance = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ground() -> MoveContext {
        MoveContext { solid_below: true, ..Default::default() }
    }
    fn air() -> MoveContext {
        MoveContext::default()
    }

    fn settle(m: &mut MovementChecker) {
        for _ in 0..30 {
            m.on_move([0.0, 64.0, 0.0], &ground());
        }
    }

    #[test]
    fn legit_jump_is_clean() {
        let mut m = MovementChecker::default();
        settle(&mut m);
        let mut y = 64.0;
        let mut vy = JUMP_VELOCITY;
        for _ in 0..12 {
            y += vy;
            let r = m.on_move([0.0, y, 0.0], &air());
            assert!(r.flags.is_empty(), "{:?}", r.flags);
            vy = (vy - GRAVITY) * DRAG;
            if y <= 64.0 {
                break;
            }
        }
    }

    #[test]
    fn flying_up_is_flagged() {
        let mut m = MovementChecker::default();
        settle(&mut m);
        let mut flagged = false;
        for i in 1..30 {
            let r = m.on_move([0.0, 64.0 + i as f64 * 0.5, 0.0], &air());
            flagged |= r.flags.iter().any(|f| f.check == Check::Fly);
        }
        assert!(flagged);
    }

    #[test]
    fn hovering_is_flagged() {
        let mut m = MovementChecker::default();
        settle(&mut m);
        let mut flagged = false;
        for _ in 0..30 {
            flagged |= m.on_move([0.0, 70.0, 0.0], &air()).flags.iter().any(|f| f.check == Check::Fly);
        }
        assert!(flagged);
    }

    #[test]
    fn elytra_glide_and_fall_afterwards_are_not_flagged() {
        let mut m = MovementChecker::default();
        settle(&mut m);
        let glide = MoveContext { gliding: true, ..Default::default() };
        let mut p = [0.0, 64.0, 0.0];
        for _ in 0..100 {
            p[0] += 1.5;
            p[1] += 0.05; // rising slightly while gliding
            assert!(m.on_move(p, &glide).flags.is_empty());
        }
        // stop gliding mid-air and fall with an elytra on: must never flag
        let mut vy = 0.3; // residual upward momentum from the glide
        for _ in 0..80 {
            p[0] += 0.5;
            p[1] += vy;
            vy = (vy - GRAVITY) * DRAG;
            let r = m.on_move(p, &air());
            assert!(r.flags.iter().all(|f| f.check != Check::Fly), "{:?}", r.flags);
        }
        // landing after the fall: no fall damage because the glide reset it…
        // …but a real fall from height does deal server-side damage
        let r = m.on_move([p[0], p[1], p[2]], &ground());
        assert!(r.fall_damage > 0.0);
    }

    #[test]
    fn speed_hack_flagged_sprint_clean() {
        let mut m = MovementChecker::default();
        settle(&mut m);
        let sprint = MoveContext { sprinting: true, solid_below: true, ..Default::default() };
        let mut x = 0.0;
        for _ in 0..100 {
            x += 0.28;
            assert!(m.on_move([x, 64.0, 0.0], &sprint).flags.is_empty());
        }
        let mut flagged = false;
        for _ in 0..20 {
            x += 1.0;
            flagged |= m.on_move([x, 64.0, 0.0], &sprint).flags.iter().any(|f| f.check == Check::Speed);
        }
        assert!(flagged);
    }

    #[test]
    fn knockback_is_exempt() {
        let mut m = MovementChecker::default();
        settle(&mut m);
        let kb = MoveContext { server_velocity: true, ..Default::default() };
        assert!(m.on_move([1.5, 65.0, 0.0], &kb).flags.is_empty());
        for i in 0..10 {
            assert!(m.on_move([1.5 + i as f64 * 0.4, 65.0 - i as f64 * 0.1, 0.0], &air()).flags.is_empty());
        }
    }

    #[test]
    fn timer_detects_fast_packets_but_tolerates_lag() {
        let mut m = MovementChecker::default();
        let mut t = 0;
        // lag spike: 1s silence then a burst of 20 packets — legit catch-up
        m.on_move_packet(t);
        t += 1000;
        for _ in 0..20 {
            assert!(m.on_move_packet(t).is_none());
        }
        // sustained 2x speed
        let mut flagged = false;
        for _ in 0..200 {
            t += 25;
            flagged |= m.on_move_packet(t).is_some();
        }
        assert!(flagged);
    }

    #[test]
    fn nan_coordinates_rejected() {
        let mut m = MovementChecker::default();
        assert!(!m.on_move([f64::NAN, 0.0, 0.0], &air()).flags.is_empty());
    }
}
