//! Player Cost: per-player processing budgets inside heavy cells.
//!
//! When a cell (region) exceeds its per-tick time budget, each player in it
//! gets a share of the budget. Players whose estimated cost exceeds their share
//! (typically fast elytra/trident flight that pulls in many new chunks, or
//! standing in huge entity farms) get *invisible-first* mitigations, applied
//! one level at a time and lifted again gradually after the cell recovers:
//!
//! | level | mitigation                                                                 |
//! |-------|----------------------------------------------------------------------------|
//! | 1     | chunk sends rate-limited and ordered by look direction (front first)       |
//! | 2     | entity tracking range of *far* non-player entities reduced (−25 %)         |
//! | 3+    | view distance −1 per level, never below `min_view_distance`                |
//!
//! Nothing that changes game rules is touched: no movement, combat, mining or
//! redstone restrictions, and players/close entities are always tracked.

use crate::config::PlayerCostCfg;

/// Raw activity of a player during the last measurement window.
#[derive(Clone, Copy, Debug, Default)]
pub struct Activity {
    /// horizontal speed in blocks/tick
    pub speed: f64,
    /// chunks sent to the player per tick (avg)
    pub chunk_sends: f64,
    /// entities tracked by the player
    pub tracked_entities: u32,
    /// packets received per tick (avg)
    pub packets_in: f64,
}

/// Estimated cost in "ms per tick" units.
pub fn estimate_cost(a: &Activity) -> f64 {
    // weights measured as typical server cost per unit
    let movement = a.speed.clamp(0.0, 10.0) * 0.4; // collision + chunk ticket churn
    let chunks = a.chunk_sends.clamp(0.0, 64.0) * 0.35; // serialisation + light
    let entities = a.tracked_entities as f64 * 0.004;
    let packets = a.packets_in.clamp(0.0, 200.0) * 0.01;
    0.05 + movement + chunks + entities + packets
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub level: u32,
    pub view_distance: u32,
    pub chunk_sends_per_tick: u32,
    /// multiplier for far entity tracking range (1.0 = vanilla)
    pub far_tracking_scale: f64,
    pub directional_chunk_order: bool,
}

#[derive(Clone, Debug)]
pub struct PlayerBudget {
    pub level: u32,
    calm_ticks: u32,
}

impl Default for PlayerBudget {
    fn default() -> Self {
        Self { level: 0, calm_ticks: 0 }
    }
}

impl PlayerBudget {
    /// Update once per second with the cell's mspt and this player's cost.
    pub fn update(&mut self, cfg: &PlayerCostCfg, cell_mspt: f64, players_in_cell: usize, cost: f64, base_view: u32) -> Limits {
        let max_level = 2 + base_view.saturating_sub(cfg.min_view_distance);
        if !cfg.enabled {
            self.level = 0;
        } else {
            let share = cfg.cell_mspt_budget / players_in_cell.max(1) as f64;
            let heavy = cell_mspt > cfg.cell_mspt_budget;
            if heavy && cost > share {
                self.calm_ticks = 0;
                self.level = (self.level + 1).min(max_level);
            } else {
                self.calm_ticks += 1;
                // hysteresis: relax one level after `recovery_seconds` of calm
                if self.level > 0 && self.calm_ticks as f64 >= cfg.recovery_seconds.max(1.0) {
                    self.level -= 1;
                    self.calm_ticks = 0;
                }
            }
        }
        self.limits(cfg, base_view)
    }

    pub fn limits(&self, cfg: &PlayerCostCfg, base_view: u32) -> Limits {
        let l = self.level;
        Limits {
            level: l,
            directional_chunk_order: l >= 1,
            chunk_sends_per_tick: if l >= 1 { cfg.throttled_chunk_sends_per_tick.max(1) } else { 16 },
            far_tracking_scale: if l >= 2 { 0.75 } else { 1.0 },
            view_distance: base_view.saturating_sub(l.saturating_sub(2)).max(cfg.min_view_distance).min(base_view),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PlayerCostCfg {
        PlayerCostCfg { enabled: true, cell_mspt_budget: 25.0, min_view_distance: 6, throttled_chunk_sends_per_tick: 4, recovery_seconds: 3.0 }
    }

    #[test]
    fn elytra_costs_more_than_walking() {
        let walk = estimate_cost(&Activity { speed: 0.2, chunk_sends: 0.2, tracked_entities: 30, packets_in: 20.0 });
        let fly = estimate_cost(&Activity { speed: 2.5, chunk_sends: 8.0, tracked_entities: 30, packets_in: 20.0 });
        assert!(fly > walk * 3.0);
    }

    #[test]
    fn escalates_only_in_heavy_cell_and_recovers() {
        let c = cfg();
        let mut b = PlayerBudget::default();
        // light cell: never limited
        for _ in 0..10 {
            assert_eq!(b.update(&c, 10.0, 5, 50.0, 10).level, 0);
        }
        // heavy cell, expensive player
        let mut l = b.update(&c, 40.0, 5, 10.0, 10);
        assert_eq!(l.level, 1);
        for _ in 0..20 {
            l = b.update(&c, 40.0, 5, 10.0, 10);
        }
        assert_eq!(l.view_distance, 6); // floor respected
        // recovery is gradual
        let mut steps = 0;
        while b.update(&c, 10.0, 5, 10.0, 10).level > 0 {
            steps += 1;
        }
        assert!(steps >= 3 * (l.level as usize - 1));
        assert_eq!(b.limits(&c, 10).view_distance, 10);
    }

    #[test]
    fn cheap_player_in_heavy_cell_untouched() {
        let c = cfg();
        let mut b = PlayerBudget::default();
        assert_eq!(b.update(&c, 40.0, 5, 0.5, 10).level, 0);
    }
}
