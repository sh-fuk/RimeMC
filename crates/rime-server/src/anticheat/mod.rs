//! Built-in anti-cheat.
//!
//! Every check produces a *violation amount*; amounts accumulate into a
//! violation level (VL) per player and check that decays over time, so a single
//! lag spike never punishes anyone. The configured action is applied when the
//! VL passes its threshold. The default action is `setback` (undo the illegal
//! move or hit), which neutralises the cheat without ever wrongly banning a
//! legitimate player; kicks happen only at high VL and bans are off by default.

pub mod combat;
pub mod movement;
pub mod xray;

use std::collections::HashMap;

use crate::config::{AntiCheatCfg, CheckCfg, Punishment};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Check {
    Fly,
    Speed,
    NoFall,
    Timer,
    Reach,
    KillAura,
    AutoClicker,
    FastBreak,
    XrayStats,
}

impl Check {
    pub fn cfg(self, c: &AntiCheatCfg) -> &CheckCfg {
        match self {
            Check::Fly => &c.fly,
            Check::Speed => &c.speed,
            Check::NoFall => &c.nofall,
            Check::Timer => &c.timer,
            Check::Reach => &c.reach,
            Check::KillAura => &c.killaura,
            Check::AutoClicker => &c.autoclicker,
            Check::FastBreak => &c.fastbreak,
            Check::XrayStats => &c.xray_detection,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// nothing to do
    Pass,
    /// log/notify only
    Alert { check: Check, vl: f64, detail: String },
    /// undo the action (teleport back / cancel hit / resend block)
    Setback { check: Check, vl: f64, detail: String },
    Kick { check: Check, vl: f64 },
    Ban { check: Check, vl: f64, minutes: u64 },
}

/// Violation levels of one player.
#[derive(Default)]
pub struct Violations {
    vl: HashMap<Check, (f64, u64)>,
}

/// VL lost per second without new violations.
const DECAY_PER_SEC: f64 = 0.5;

impl Violations {
    pub fn vl(&self, c: Check) -> f64 {
        self.vl.get(&c).map(|v| v.0).unwrap_or(0.0)
    }

    /// Record a violation and decide the action. `now_ms` is a monotonic clock.
    pub fn flag(&mut self, cfg: &AntiCheatCfg, check: Check, amount: f64, now_ms: u64, detail: String) -> Verdict {
        let cc = check.cfg(cfg);
        if !cfg.enabled || !cc.enabled || !amount.is_finite() || amount <= 0.0 {
            return Verdict::Pass;
        }
        let e = self.vl.entry(check).or_insert((0.0, now_ms));
        let secs = now_ms.saturating_sub(e.1) as f64 / 1000.0;
        e.0 = (e.0 - secs * DECAY_PER_SEC).max(0.0) + amount.min(10.0);
        e.1 = now_ms;
        let vl = e.0;
        if cc.ban_at > 0.0 && vl >= cc.ban_at {
            return Verdict::Ban { check, vl, minutes: cfg.ban_minutes };
        }
        if cc.kick_at > 0.0 && vl >= cc.kick_at {
            return Verdict::Kick { check, vl };
        }
        if vl < cc.threshold {
            return Verdict::Alert { check, vl, detail };
        }
        match cc.action {
            Punishment::Alert => Verdict::Alert { check, vl, detail },
            Punishment::Setback => Verdict::Setback { check, vl, detail },
            Punishment::Kick => Verdict::Kick { check, vl },
            Punishment::Ban => Verdict::Ban { check, vl, minutes: cfg.ban_minutes },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalation_and_decay() {
        let cfg = AntiCheatCfg::default();
        let mut v = Violations::default();
        assert!(matches!(v.flag(&cfg, Check::Fly, 1.0, 0, String::new()), Verdict::Alert { .. }));
        for _ in 0..4 {
            v.flag(&cfg, Check::Fly, 1.0, 0, String::new());
        }
        assert!(matches!(v.flag(&cfg, Check::Fly, 1.0, 0, String::new()), Verdict::Setback { .. }));
        // after a long quiet period VL is back to ~0
        v.flag(&cfg, Check::Fly, 0.1, 600_000, String::new());
        assert!(v.vl(Check::Fly) < 1.0);
        for _ in 0..60 {
            v.flag(&cfg, Check::Fly, 10.0, 600_000, String::new());
        }
        assert!(matches!(v.flag(&cfg, Check::Fly, 1.0, 600_000, String::new()), Verdict::Kick { .. }));
    }

    #[test]
    fn disabled_never_flags() {
        let mut cfg = AntiCheatCfg::default();
        cfg.enabled = false;
        let mut v = Violations::default();
        assert_eq!(v.flag(&cfg, Check::Speed, 100.0, 0, String::new()), Verdict::Pass);
    }
}
