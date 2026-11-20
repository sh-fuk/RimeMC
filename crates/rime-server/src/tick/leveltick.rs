//! Level Tick: mob tick throttling for crowded chunks.
//!
//! When a chunk holds `crowded_threshold` mobs or more, its mobs tick every
//! 2nd tick; every `step` additional mobs add one more tick to the interval,
//! up to `max_interval`. Only *mobs* are affected — redstone, block ticks,
//! fluids, items, minecarts, TNT, projectiles and players always run at full
//! rate, so contraptions behave exactly as in vanilla.
//!
//! To keep gameplay unaffected, a throttled mob still runs at full rate while
//! it is "relevant": it was hurt recently, is in lava/fire, is a passenger or
//! vehicle, is leashed, or is targeting / near a player. The mobs of a crowded
//! chunk are also staggered (`(tick + id) % interval`) so the work is spread
//! evenly instead of everything ticking on the same tick.
//!
//! A throttled mob receives the elapsed tick count (`ticks_to_run`) so timers
//! such as breeding cooldowns or despawn counters advance at vanilla speed.

use crate::config::LevelTickCfg;

#[derive(Clone, Copy, Debug, Default)]
pub struct MobFlags {
    pub recently_hurt: bool,
    pub in_lava_or_fire: bool,
    pub riding_or_ridden: bool,
    pub leashed: bool,
    pub has_player_target: bool,
    /// a player is within 8 blocks
    pub player_nearby: bool,
}

impl MobFlags {
    pub fn is_relevant(&self) -> bool {
        self.recently_hurt || self.in_lava_or_fire || self.riding_or_ridden || self.leashed || self.has_player_target || self.player_nearby
    }
}

/// Tick interval for the mobs of a chunk holding `mobs` mobs.
pub fn chunk_interval(cfg: &LevelTickCfg, mobs: u32) -> u32 {
    if !cfg.enabled || mobs < cfg.crowded_threshold {
        return 1;
    }
    let extra = (mobs - cfg.crowded_threshold) / cfg.step.max(1);
    (2 + extra).min(cfg.max_interval.max(1))
}

/// How many ticks of work this mob should perform on `tick` (0 = skip).
/// Over `interval` ticks the total always equals `interval`, i.e. no time is lost.
pub fn ticks_to_run(interval: u32, tick: u64, entity_id: u32, flags: &MobFlags) -> u32 {
    if interval <= 1 || flags.is_relevant() {
        return 1;
    }
    if (tick.wrapping_add(entity_id as u64)) % interval as u64 == 0 { interval } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LevelTickCfg {
        LevelTickCfg { enabled: true, crowded_threshold: 24, step: 16, max_interval: 4 }
    }

    #[test]
    fn interval_steps() {
        let c = cfg();
        assert_eq!(chunk_interval(&c, 0), 1);
        assert_eq!(chunk_interval(&c, 23), 1);
        assert_eq!(chunk_interval(&c, 24), 2);
        assert_eq!(chunk_interval(&c, 40), 3);
        assert_eq!(chunk_interval(&c, 56), 4);
        assert_eq!(chunk_interval(&c, 10_000), 4);
        let off = LevelTickCfg { enabled: false, ..c };
        assert_eq!(chunk_interval(&off, 10_000), 1);
    }

    #[test]
    fn no_time_lost_and_staggered() {
        let f = MobFlags::default();
        for id in 0..10 {
            let total: u32 = (0..12).map(|t| ticks_to_run(3, t, id, &f)).sum();
            assert_eq!(total, 12);
        }
        // different ids run on different ticks
        let on_tick0: usize = (0..30).filter(|id| ticks_to_run(3, 0, *id, &f) > 0).count();
        assert_eq!(on_tick0, 10);
    }

    #[test]
    fn relevant_mobs_full_rate() {
        let f = MobFlags { recently_hurt: true, ..Default::default() };
        assert!((0..10).all(|t| ticks_to_run(4, t, 1, &f) == 1));
    }
}
