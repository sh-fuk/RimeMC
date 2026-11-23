//! Building golems and the wither from blocks (`CarvedPumpkinBlock` /
//! `WitherSkullBlock.checkSpawn` / `CopperGolemStatueBlock`): the patterns,
//! clearing the blocks and spawning the mob, plus `summoned_entity`.

use super::mob::MobType;
use super::{Game, Player};
use crate::registry::{blocks, BlockState};
use std::sync::Arc;

fn is(s: BlockState, names: &[&str]) -> bool {
    names.contains(&s.name())
}

fn pumpkin(s: BlockState) -> bool {
    is(s, &["carved_pumpkin", "jack_o_lantern"])
}

fn soul(g: &Game, s: BlockState) -> bool {
    g.data.block_in("wither_summon_base_blocks", s.name())
}

fn copper(g: &Game, s: BlockState) -> bool {
    let n = s.name();
    g.data.block_in("copper", n) || n.ends_with("copper_block") || matches!(n, "exposed_copper" | "weathered_copper" | "oxidized_copper" | "waxed_exposed_copper" | "waxed_weathered_copper" | "waxed_oxidized_copper")
}

impl Game {
    fn at(&self, p: [i32; 3]) -> BlockState {
        self.world.get_block(p[0], p[1], p[2]).unwrap_or(blocks::AIR)
    }

    fn clear_pattern(&self, cells: &[[i32; 3]]) {
        for &c in cells {
            let s = self.at(c);
            // destroy-particles like BlockPatternBuilder's clearing
            self.level_event(2001, c, super::mobtick::state_id(s));
            self.set_block(c, blocks::AIR, super::level::flags::ALL);
        }
    }

    fn summoned(&self, eid: i32, pos: [f64; 3]) {
        for p in self.players() {
            let q = p.pose.lock().pos;
            if (q[0] - pos[0]).abs() <= 5.5 && (q[1] - pos[1]).abs() <= 5.5 && (q[2] - pos[2]).abs() <= 5.5 {
                self.trigger_entity(&p, "summoned_entity", eid);
            }
        }
    }

    /// After a pumpkin or wither skeleton skull is placed at `p`.
    pub fn check_build(&self, p: [i32; 3], _by: Option<&Arc<Player>>) -> bool {
        let top = self.at(p);
        if pumpkin(top) {
            return self.check_snow_golem(p) || self.check_iron_golem(p) || self.check_copper_golem(p);
        }
        if is(top, &["wither_skeleton_skull", "wither_skeleton_wall_skull"]) {
            return self.check_wither(p);
        }
        false
    }

    fn check_snow_golem(&self, p: [i32; 3]) -> bool {
        let a = [p[0], p[1] - 1, p[2]];
        let b = [p[0], p[1] - 2, p[2]];
        if !(is(self.at(a), &["snow_block"]) && is(self.at(b), &["snow_block"])) {
            return false;
        }
        self.clear_pattern(&[p, a, b]);
        let Some(ty) = MobType::from_name("snow_golem") else { return false };
        let pos = [b[0] as f64 + 0.5, b[1] as f64 + 0.05, b[2] as f64 + 0.5];
        let eid = self.spawn_mob(ty, pos, true);
        self.summoned(eid, pos);
        true
    }

    fn check_iron_golem(&self, p: [i32; 3]) -> bool {
        let body = [p[0], p[1] - 1, p[2]];
        let legs = [p[0], p[1] - 2, p[2]];
        if !(is(self.at(body), &["iron_block"]) && is(self.at(legs), &["iron_block"])) {
            return false;
        }
        for (dx, dz) in [(1, 0), (0, 1)] {
            let arm1 = [body[0] + dx, body[1], body[2] + dz];
            let arm2 = [body[0] - dx, body[1], body[2] - dz];
            let c1 = [legs[0] + dx, legs[1], legs[2] + dz];
            let c2 = [legs[0] - dx, legs[1], legs[2] - dz];
            if is(self.at(arm1), &["iron_block"]) && is(self.at(arm2), &["iron_block"]) && self.at(c1).is_air() && self.at(c2).is_air() {
                self.clear_pattern(&[p, body, legs, arm1, arm2]);
                let Some(ty) = MobType::from_name("iron_golem") else { return false };
                let pos = [legs[0] as f64 + 0.5, legs[1] as f64 + 0.05, legs[2] as f64 + 0.5];
                let eid = self.spawn_mob_with(ty, pos, true, |m, _| m.v.player_made = true);
                self.summoned(eid, pos);
                return true;
            }
        }
        false
    }

    fn check_copper_golem(&self, p: [i32; 3]) -> bool {
        let below = [p[0], p[1] - 1, p[2]];
        if !copper(self, self.at(below)) {
            return false;
        }
        let Some(ty) = MobType::from_name("copper_golem") else { return false };
        self.clear_pattern(&[p, below]);
        let pos = [below[0] as f64 + 0.5, below[1] as f64 + 0.05, below[2] as f64 + 0.5];
        let eid = self.spawn_mob(ty, pos, true);
        self.summoned(eid, pos);
        true
    }

    /// `WitherSkullBlock.checkSpawn`: three skulls on a T of soul sand / soil.
    fn check_wither(&self, p: [i32; 3]) -> bool {
        if self.dim_is_peaceful() {
            return false;
        }
        let skull = |c: [i32; 3]| is(self.at(c), &["wither_skeleton_skull", "wither_skeleton_wall_skull"]);
        for (dx, dz) in [(1, 0), (0, 1)] {
            // the placed skull may be any of the three
            for k in -1..=1 {
                let mid = [p[0] - dx * k, p[1], p[2] - dz * k];
                let row = |y: i32, i: i32| [mid[0] + dx * i, mid[1] + y, mid[2] + dz * i];
                let ok = (-1..=1).all(|i| skull(row(0, i)))
                    && (-1..=1).all(|i| soul(self, self.at(row(-1, i))))
                    && soul(self, self.at(row(-2, 0)))
                    && self.at(row(-2, -1)).is_air()
                    && self.at(row(-2, 1)).is_air();
                if !ok {
                    continue;
                }
                let cells: Vec<[i32; 3]> = (-1..=1).flat_map(|i| [row(0, i), row(-1, i)]).chain([row(-2, 0)]).collect();
                self.clear_pattern(&cells);
                let Some(ty) = MobType::from_name("wither") else { return false };
                let base = row(-2, 0);
                let pos = [base[0] as f64 + 0.5, base[1] as f64 + 0.55, base[2] as f64 + 0.5];
                let eid = self.spawn_mob_with(ty, pos, true, |m, _| {
                    // WitherBoss.makeInvulnerable
                    m.v.invul = 220;
                    let max = m.living.max_health();
                    m.living.health = max / 3.0;
                });
                self.level_event_global(1023, base);
                self.summoned(eid, pos);
                return true;
            }
        }
        false
    }

    fn dim_is_peaceful(&self) -> bool {
        super::rules::difficulty() == 0
    }
}
