//! Combat checks: Reach, KillAura, AutoClicker, FastBreak.

use std::collections::VecDeque;

use super::movement::Flag;
use super::Check;

/// Axis-aligned bounding box.
#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Aabb {
    /// Entity box from feet position, width and height.
    pub fn entity(feet: [f64; 3], width: f64, height: f64) -> Self {
        let h = width / 2.0;
        Self { min: [feet[0] - h, feet[1], feet[2] - h], max: [feet[0] + h, feet[1] + height, feet[2] + h] }
    }
    pub fn distance_to(&self, p: [f64; 3]) -> f64 {
        let mut d2 = 0.0;
        for i in 0..3 {
            let v = p[i].clamp(self.min[i], self.max[i]) - p[i];
            d2 += v * v;
        }
        d2.sqrt()
    }
    pub fn inflate(&self, x: f64, y: f64, z: f64) -> Self {
        Aabb { min: [self.min[0] - x, self.min[1] - y, self.min[2] - z], max: [self.max[0] + x, self.max[1] + y, self.max[2] + z] }
    }

    pub fn intersects(&self, o: &Aabb) -> bool {
        self.min[0] < o.max[0] && self.max[0] > o.min[0] && self.min[1] < o.max[1] && self.max[1] > o.min[1] && self.min[2] < o.max[2] && self.max[2] > o.min[2]
    }

    pub fn center(&self) -> [f64; 3] {
        [(self.min[0] + self.max[0]) / 2.0, (self.min[1] + self.max[1]) / 2.0, (self.min[2] + self.max[2]) / 2.0]
    }
}

/// Look vector from yaw/pitch in degrees (vanilla convention).
pub fn look_vector(yaw: f32, pitch: f32) -> [f64; 3] {
    let (y, p) = ((yaw as f64).to_radians(), (pitch as f64).to_radians());
    [-y.sin() * p.cos(), -p.sin(), y.cos() * p.cos()]
}

pub struct CombatChecker {
    clicks: VecDeque<u64>,
    recent_targets: VecDeque<(u64, u32)>,
}

impl Default for CombatChecker {
    fn default() -> Self {
        Self { clicks: VecDeque::with_capacity(64), recent_targets: VecDeque::with_capacity(16) }
    }
}

/// Survival reach is 3.0; we allow for latency/interpolation of the target.
pub const SURVIVAL_REACH: f64 = 3.0;
pub const CREATIVE_REACH: f64 = 5.0;
pub const REACH_TOLERANCE: f64 = 0.4;

impl CombatChecker {
    /// Called for every attack on an entity.
    ///
    /// `target_boxes` should contain the target's box at the current tick and
    /// the few previous ticks (lag compensation); the closest one counts.
    pub fn on_attack(&mut self, now_ms: u64, eye: [f64; 3], yaw: f32, pitch: f32, target_id: u32, target_boxes: &[Aabb], creative: bool) -> Vec<Flag> {
        let mut flags = Vec::new();
        if target_boxes.is_empty() {
            return flags;
        }
        // Reach
        let dist = target_boxes.iter().map(|b| b.distance_to(eye)).fold(f64::INFINITY, f64::min);
        let max = if creative { CREATIVE_REACH } else { SURVIVAL_REACH } + REACH_TOLERANCE;
        if dist > max {
            flags.push(Flag { check: Check::Reach, amount: 1.0 + (dist - max).min(4.0), detail: format!("{dist:.2} > {max:.2}") });
        }
        // KillAura: hitting something far outside the field of view
        let look = look_vector(yaw, pitch);
        let best_angle = target_boxes
            .iter()
            .map(|b| {
                let c = b.center();
                let v = [c[0] - eye[0], c[1] - eye[1], c[2] - eye[2]];
                let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-6);
                let cos = (v[0] * look[0] + v[1] * look[1] + v[2] * look[2]) / len;
                // close targets subtend a large angle; allow for their size
                let allowance = (1.0 / len).atan().to_degrees();
                cos.clamp(-1.0, 1.0).acos().to_degrees() - allowance
            })
            .fold(f64::INFINITY, f64::min);
        if best_angle > 60.0 {
            flags.push(Flag { check: Check::KillAura, amount: 1.0 + (best_angle - 60.0) / 30.0, detail: format!("angle {best_angle:.0}°") });
        }
        // KillAura: many distinct targets within a short time
        self.recent_targets.push_back((now_ms, target_id));
        while self.recent_targets.front().is_some_and(|(t, _)| now_ms.saturating_sub(*t) > 1000) {
            self.recent_targets.pop_front();
        }
        let mut ids: Vec<u32> = self.recent_targets.iter().map(|(_, id)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.len() > 4 {
            flags.push(Flag { check: Check::KillAura, amount: 1.0, detail: format!("{} targets/s", ids.len()) });
        }
        flags.extend(self.on_click(now_ms));
        flags
    }

    /// Any left click (attack or swing). Detects inhuman click rate and
    /// machine-perfect click timing.
    pub fn on_click(&mut self, now_ms: u64) -> Option<Flag> {
        self.clicks.push_back(now_ms);
        while self.clicks.len() > 40 || self.clicks.front().is_some_and(|t| now_ms.saturating_sub(*t) > 2000) {
            self.clicks.pop_front();
        }
        let n = self.clicks.len();
        if n < 10 {
            return None;
        }
        let span = (self.clicks[n - 1] - self.clicks[0]).max(1) as f64;
        let cps = (n - 1) as f64 * 1000.0 / span;
        if cps > 20.0 {
            return Some(Flag { check: Check::AutoClicker, amount: (cps - 20.0) / 5.0 + 1.0, detail: format!("{cps:.1} cps") });
        }
        if n >= 20 && cps > 8.0 {
            let iv: Vec<f64> = self.clicks.iter().zip(self.clicks.iter().skip(1)).map(|(a, b)| (*b - *a) as f64).collect();
            let mean = iv.iter().sum::<f64>() / iv.len() as f64;
            let sd = (iv.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / iv.len() as f64).sqrt();
            if sd < 2.0 {
                return Some(Flag { check: Check::AutoClicker, amount: 1.0, detail: format!("σ={sd:.2}ms at {cps:.1} cps") });
            }
        }
        None
    }

    /// FastBreak: `expected_ms` is the vanilla break time for the tool/block/effects.
    pub fn on_block_break(&mut self, started_ms: u64, now_ms: u64, expected_ms: u64) -> Option<Flag> {
        if expected_ms == 0 {
            return None; // instant-break block
        }
        let took = now_ms.saturating_sub(started_ms);
        // 30 % tolerance + one tick of latency
        if (took + 50) as f64 * 1.3 < expected_ms as f64 {
            return Some(Flag { check: Check::FastBreak, amount: 1.0, detail: format!("{took}ms < {expected_ms}ms") });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target_at(x: f64, z: f64) -> Aabb {
        Aabb::entity([x, 64.0, z], 0.6, 1.8)
    }

    #[test]
    fn reach() {
        let mut c = CombatChecker::default();
        let eye = [0.0, 65.62, 0.0];
        // facing +z (yaw 0)
        assert!(c.on_attack(0, eye, 0.0, 0.0, 1, &[target_at(0.0, 3.0)], false).is_empty());
        let f = c.on_attack(1000, eye, 0.0, 0.0, 1, &[target_at(0.0, 5.0)], false);
        assert!(f.iter().any(|f| f.check == Check::Reach));
        // lag compensation: an older position within reach counts
        assert!(c.on_attack(2000, eye, 0.0, 0.0, 1, &[target_at(0.0, 5.0), target_at(0.0, 3.2)], false).is_empty());
    }

    #[test]
    fn aura_behind_back() {
        let mut c = CombatChecker::default();
        let f = c.on_attack(0, [0.0, 65.62, 0.0], 0.0, 0.0, 1, &[target_at(0.0, -2.5)], false);
        assert!(f.iter().any(|f| f.check == Check::KillAura));
    }

    #[test]
    fn human_clicking_is_fine_bot_is_not() {
        let mut c = CombatChecker::default();
        // human: ~10 cps with jitter
        let mut t = 0;
        for i in 0..40u64 {
            t += 85 + (i * 37 % 31);
            assert!(c.on_click(t).is_none());
        }
        let mut bot = CombatChecker::default();
        let mut flagged = false;
        for i in 0..40 {
            flagged |= bot.on_click(i * 100).is_some();
        }
        assert!(flagged);
    }

    #[test]
    fn fastbreak() {
        let mut c = CombatChecker::default();
        assert!(c.on_block_break(0, 750, 750).is_none());
        assert!(c.on_block_break(0, 100, 750).is_some());
        assert!(c.on_block_break(0, 0, 0).is_none());
    }
}
