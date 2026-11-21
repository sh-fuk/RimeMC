//! Per-type setup (vanilla `registerGoals`, pathfinding malus, navigation
//! flags), selectors, helpers the goals call back into, and the synced
//! entity data of every mob type.

use rand::Rng;

use super::goals::{mk, MeleeKind, Sel};
use super::path::PathType;
use super::{dist2, Ctx, Seen, Slot};
use crate::data::{Items, Stack};
use crate::game::combat::living::{UseItem, HEAD, MAINHAND, OFFHAND};
use crate::game::mob::Mob;
use crate::game::mobtick::Act;
use crate::net::codec::Out;

pub use super::special::Special;

// ---------------------------------------------------------------- selectors

fn villagers(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "villager" | "wandering_trader")
}
fn adult_villagers(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "villager" | "wandering_trader") && !s.baby
}
fn iron_golems(_: &Mob, s: &Seen) -> bool {
    s.ty == "iron_golem"
}
/// `Turtle.BABY_ON_LAND_SELECTOR`
fn baby_turtles(_: &Mob, s: &Seen) -> bool {
    s.ty == "turtle" && s.baby && !s.in_water
}
fn piglins(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "piglin" | "piglin_brute")
}
fn zombies_not_piglin(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "zombie" | "husk" | "drowned" | "zombie_villager")
}
fn illagers(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "evoker" | "illusioner" | "pillager" | "vindicator")
}
fn enemies(_: &Mob, s: &Seen) -> bool {
    s.monster && !s.player
}
fn enemies_not_creeper(_: &Mob, s: &Seen) -> bool {
    s.monster && !s.player && s.ty != "creeper"
}
fn chickens(_: &Mob, s: &Seen) -> bool {
    s.ty == "chicken"
}
fn rabbits(_: &Mob, s: &Seen) -> bool {
    s.ty == "rabbit"
}
fn foxes(m: &Mob, s: &Seen) -> bool {
    s.ty == "fox" && !m.is_baby()
}
/// `Wolf.PREY_SELECTOR`
fn wolf_prey(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "sheep" | "rabbit" | "fox")
}
fn skeletons(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "skeleton" | "stray" | "wither_skeleton" | "bogged" | "parched")
}
fn endermites(_: &Mob, s: &Seen) -> bool {
    s.ty == "endermite"
}
fn axolotls(_: &Mob, s: &Seen) -> bool {
    s.ty == "axolotl"
}
fn wolves(_: &Mob, s: &Seen) -> bool {
    s.ty == "wolf" && !s.tame
}
fn wild_wolves_not_defending(_: &Mob, s: &Seen) -> bool {
    s.ty == "wolf" && !s.tame
}
fn polar_bears(_: &Mob, s: &Seen) -> bool {
    s.ty == "polar_bear"
}
fn llamas(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "llama" | "trader_llama")
}
fn cats(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "cat" | "ocelot")
}
fn creakings(_: &Mob, s: &Seen) -> bool {
    s.ty == "creaking"
}
fn calm_armadillos(_: &Mob, s: &Seen) -> bool {
    s.ty == "armadillo" && !s.scared
}
fn guardians(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "guardian" | "elder_guardian")
}
fn same_type(m: &Mob, s: &Seen) -> bool {
    s.ty == m.ty.name()
}
fn non_players(_: &Mob, s: &Seen) -> bool {
    !s.player
}
fn all_but_players(_: &Mob, s: &Seen) -> bool {
    !s.player
}
fn monsters_any(_: &Mob, s: &Seen) -> bool {
    s.monster
}
/// `NeutralMob.isAngryAt`
fn angry_at(m: &Mob, s: &Seen) -> bool {
    m.v.anger_target == Some(s.uuid)
}
fn polar_bear_players(m: &Mob, s: &Seen) -> bool {
    angry_at(m, s)
}
fn drowned_ok(_: &Mob, s: &Seen) -> bool {
    // Drowned.okTarget: at night or in water
    s.in_water || s.night
}
/// `Guardian.GuardianAttackSelector`
fn guardian_prey(_: &Mob, s: &Seen) -> bool {
    s.player || matches!(s.ty, "squid" | "glow_squid" | "axolotl")
}
/// `WitherBoss.LIVING_ENTITY_SELECTOR`
fn wither_prey(_: &Mob, s: &Seen) -> bool {
    !s.undead
}
fn zoglin_prey(_: &Mob, s: &Seen) -> bool {
    !matches!(s.ty, "creeper" | "zoglin" | "ghast")
}
fn piglin_prey(_: &Mob, s: &Seen) -> bool {
    s.player && !s.wears_gold
}
fn hoglin_prey(_: &Mob, s: &Seen) -> bool {
    s.player
}
fn brute_prey(_: &Mob, s: &Seen) -> bool {
    s.player || matches!(s.ty, "wither_skeleton" | "wither")
}
fn axolotl_prey(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "cod" | "salmon" | "pufferfish" | "tropical_fish" | "squid" | "glow_squid" | "tadpole" | "drowned" | "guardian" | "elder_guardian")
}
fn frog_prey(_: &Mob, s: &Seen) -> bool {
    s.ty == "slime" && s.size == 1 || s.ty == "magma_cube" && s.size == 1
}
fn goat_rammable(_: &Mob, s: &Seen) -> bool {
    !matches!(s.ty, "goat") && s.ty != "armor_stand"
}
fn warden_prey(_: &Mob, s: &Seen) -> bool {
    !matches!(s.ty, "warden" | "armor_stand")
}
fn breeze_prey(_: &Mob, s: &Seen) -> bool {
    s.player || s.ty == "iron_golem"
}
fn fish_threat(_: &Mob, s: &Seen) -> bool {
    s.player && !s.spectator()
}

// ---------------------------------------------------------------- setup

fn g(m: &mut Mob, prio: i32, goal: super::goals::Goal) {
    m.goals.push(Slot::new(prio, goal));
}

fn t(m: &mut Mob, prio: i32, goal: super::goals::Goal) {
    m.targets.push(Slot::new(prio, goal));
}

fn malus(m: &mut Mob, t: PathType, v: f32) {
    m.malus[t as usize] = v;
}

fn players_t(m: &mut Mob, prio: i32) {
    t(m, prio, mk::target_players(true));
}

fn tg(m: &mut Mob, prio: i32, sel: Sel, must_see: bool) {
    t(m, prio, mk::target(sel, must_see));
}

/// The common animal set: float, panic, breed, tempt, follow parent, stroll, look.
fn farm(m: &mut Mob, panic: f64, tempt: f64, food: &'static str, parent: f64) {
    g(m, 0, mk::float());
    g(m, 1, mk::panic(panic));
    g(m, 2, mk::breed(1.0));
    g(m, 3, mk::tempt(tempt, food, false));
    g(m, 4, mk::follow_parent(parent));
    g(m, 5, mk::wander(1.0));
    g(m, 6, mk::look_player(6.0));
    g(m, 7, mk::random_look());
}

fn skeleton_common(m: &mut Mob) {
    g(m, 2, mk::restrict_sun());
    g(m, 3, mk::flee_sun(1.0));
    g(m, 3, mk::avoid(wolves, 6.0, 1.0, 1.2));
    g(m, 5, mk::wander(1.0));
    g(m, 6, mk::look_player(8.0));
    g(m, 6, mk::random_look());
    t(m, 1, mk::hurt_by());
    players_t(m, 2);
    tg(m, 3, iron_golems, true);
    t(m, 3, mk::target_full(baby_turtles, false, 10, true, false));
    if m.ty.is("wither_skeleton") {
        tg(m, 3, piglins, true);
    }
}

/// `AbstractSkeleton.reassessWeaponGoal`
pub fn reassess_weapon(m: &mut Mob, difficulty: i32) {
    m.goals.retain(|s| !(s.prio == 4 && matches!(s.goal, super::goals::Goal::RangedBow { .. } | super::goals::Goal::Melee { .. })));
    if holding_bow(m) {
        let (hard, normal) = if matches!(m.ty.name(), "bogged" | "parched") { (50, 70) } else { (20, 40) };
        let interval = if difficulty == 3 { hard } else { normal };
        g(m, 4, mk::bow(1.0, interval, 15.0));
    } else {
        g(m, 4, mk::melee(1.2, false));
    }
}

fn zombie_common(m: &mut Mob) {
    let piglin = m.ty.is("zombified_piglin");
    if !piglin {
        g(m, 4, mk::special(Special::AttackTurtleEgg { speed: 1.0, ticks: 0, pos: None }));
        g(m, 8, mk::look_player(8.0));
        g(m, 8, mk::random_look());
    }
    if m.ty.is("drowned") {
        g(m, 1, mk::special(Special::DrownedGoToWater { speed: 1.0, wanted: [0.0; 3] }));
        g(m, 2, mk::ranged(1.0, 40, 40, 10.0));
        g(m, 2, mk::melee_k(1.0, false, MeleeKind::Drowned));
        g(m, 5, mk::special(Special::DrownedGoToBeach { speed: 1.0, wanted: [0.0; 3] }));
        g(m, 6, mk::special(Special::DrownedSwimUp { speed: 1.0, sea: 63, stuck: false }));
        g(m, 7, mk::stroll(1.0));
        t(m, 1, mk::hurt_by_alert(&["drowned"], &["zombified_piglin"]));
        t(m, 2, mk::target_full(drowned_ok, true, 10, true, false));
        tg(m, 3, villagers, false);
        tg(m, 3, iron_golems, true);
        tg(m, 3, axolotls, true);
        t(m, 5, mk::target_full(baby_turtles, false, 10, true, false));
        return;
    }
    g(m, if piglin { 2 } else { 3 }, mk::melee_k(1.0, false, MeleeKind::Zombie));
    g(m, 7, mk::wander(1.0));
    if piglin {
        t(m, 1, mk::hurt_by_alert(&[], &[]));
        t(m, 2, mk::target_full(angry_at, true, 10, true, false));
        return;
    }
    g(m, 6, mk::special(Special::MoveThroughVillage { speed: 1.0 }));
    t(m, 1, mk::hurt_by_alert(&[], &["zombified_piglin"]));
    players_t(m, 2);
    tg(m, 3, villagers, false);
    tg(m, 3, iron_golems, true);
    t(m, 5, mk::target_full(baby_turtles, false, 10, true, false));
}

fn illager_avoid_creaking(m: &mut Mob, walk: f64, sprint: f64) {
    g(m, 1, mk::avoid(creakings, 8.0, walk, sprint));
}

/// `registerGoals` + constructor settings of every type.
pub fn setup(m: &mut Mob) {
    let n = m.ty.name();
    if m.ty.is_monster_class() && !matches!(n, "ghast" | "phantom" | "slime" | "magma_cube") || m.ty.is_animal() {
        // PathfinderMob defaults
    }
    match n {
        // ------------------------------------------------ farm animals
        "chicken" => {
            farm(m, 1.4, 1.0, "chicken_food", 1.1);
            malus(m, PathType::Water, 0.0);
        }
        "cow" | "mooshroom" => farm(m, 2.0, 1.25, "cow_food", 1.25),
        "sheep" => {
            g(m, 0, mk::float());
            g(m, 1, mk::panic(1.25));
            g(m, 2, mk::breed(1.0));
            g(m, 3, mk::tempt(1.1, "sheep_food", false));
            g(m, 4, mk::follow_parent(1.1));
            g(m, 5, mk::eat_block());
            g(m, 6, mk::wander(1.0));
            g(m, 7, mk::look_player(6.0));
            g(m, 8, mk::random_look());
        }
        "pig" => {
            g(m, 0, mk::float());
            g(m, 1, mk::panic(1.25));
            g(m, 3, mk::breed(1.0));
            g(m, 4, mk::tempt(1.2, "rime:carrot_on_a_stick", false));
            g(m, 4, mk::tempt(1.2, "pig_food", false));
            g(m, 5, mk::follow_parent(1.1));
            g(m, 6, mk::wander(1.0));
            g(m, 7, mk::look_player(6.0));
            g(m, 8, mk::random_look());
        }
        "rabbit" => {
            g(m, 1, mk::float());
            g(m, 1, mk::panic(2.2));
            g(m, 2, mk::breed(0.8));
            g(m, 3, mk::tempt(1.0, "rabbit_food", false));
            g(m, 4, mk::avoid(super::goals::players, 8.0, 2.2, 2.2));
            g(m, 4, mk::avoid(wolves_any, 10.0, 2.2, 2.2));
            g(m, 4, mk::avoid(monsters_any, 4.0, 2.2, 2.2));
            g(m, 5, mk::special(Special::RaidGarden { wanted: None, can_raid: false, tries: 0 }));
            g(m, 6, mk::wander_p(0.6, 0.001));
            g(m, 11, mk::look_player(10.0));
            malus(m, PathType::Water, 0.0);
        }
        "turtle" => {
            g(m, 0, mk::panic(1.2));
            g(m, 1, mk::breed(1.0));
            g(m, 1, mk::special(Special::TurtleLayEgg { ticks: 0, wanted: None }));
            g(m, 2, mk::tempt(1.1, "turtle_food", false));
            g(m, 3, mk::special(Special::TurtleGoToWater { wanted: None }));
            g(m, 4, mk::special(Special::TurtleGoHome { stuck: false, ticks: 0 }));
            g(m, 7, mk::special(Special::TurtleTravel { stuck: false }));
            g(m, 8, mk::look_player(8.0));
            g(m, 9, mk::stroll_i(1.0, 100));
            malus(m, PathType::Water, 0.0);
            malus(m, PathType::DoorIronClosed, -1.0);
            malus(m, PathType::DoorWoodClosed, -1.0);
            malus(m, PathType::DoorOpen, -1.0);
            m.max_up_step = 1.0;
        }
        "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" | "camel" | "camel_husk" => {
            g(m, 0, mk::float());
            if !matches!(n, "skeleton_horse" | "zombie_horse" | "camel_husk") {
                g(m, 1, mk::panic(1.2));
                g(m, 2, mk::breed(1.0));
            }
            let tempt = match n {
                "camel" => "camel_food",
                "camel_husk" => "camel_husk_food",
                "zombie_horse" => "zombie_horse_food",
                _ => "horse_tempt_items",
            };
            if n != "skeleton_horse" {
                g(m, 3, mk::tempt(1.25, tempt, false));
            }
            g(m, 4, mk::follow_parent(1.0));
            g(m, 6, mk::wander(0.7));
            g(m, 7, mk::look_player(6.0));
            g(m, 8, mk::random_look());
            if !matches!(n, "camel" | "camel_husk") {
                g(m, 9, mk::special(Special::RandomStand));
            }
        }
        "llama" | "trader_llama" => {
            g(m, 0, mk::float());
            g(m, 3, mk::ranged(1.25, 40, 40, 20.0));
            g(m, 3, mk::panic(1.2));
            g(m, 4, mk::breed(1.0));
            g(m, 5, mk::tempt(1.25, "llama_tempt_items", false));
            g(m, 6, mk::follow_parent(1.0));
            g(m, 7, mk::wander(0.7));
            g(m, 8, mk::look_player(6.0));
            g(m, 9, mk::random_look());
            t(m, 1, mk::hurt_by());
            t(m, 2, mk::target(wolves, false));
            if n == "trader_llama" {
                g(m, 1, mk::panic(2.0));
                tg(m, 2, zombies_not_piglin, true);
                tg(m, 2, illagers, true);
            }
        }
        "cat" | "ocelot" => {
            let cat = n == "cat";
            g(m, 1, mk::float());
            if cat {
                g(m, 1, mk::panic(1.5));
                g(m, 2, mk::sit());
                g(m, 6, mk::follow_owner(1.0, 10.0, 5.0));
                g(m, 4, mk::tempt(0.6, "cat_food", true));
            } else {
                g(m, 3, mk::tempt(0.6, "ocelot_food", true));
                g(m, 4, mk::avoid(super::goals::players, 16.0, 0.8, 1.33));
            }
            g(m, if cat { 8 } else { 7 }, mk::leap(0.3));
            g(m, if cat { 9 } else { 8 }, mk::melee_k(1.0, true, MeleeKind::Ocelot));
            g(m, if cat { 10 } else { 9 }, mk::breed(0.8));
            g(m, if cat { 11 } else { 10 }, mk::wander_p(0.8, 1.0000001e-5));
            g(m, if cat { 12 } else { 11 }, mk::look_player(10.0));
            if cat {
                t(m, 1, mk::special(Special::NonTameTarget { sel: rabbits, st: super::goals::TState::new(false, false), found: None }));
                t(m, 1, mk::special(Special::NonTameTarget { sel: baby_turtles, st: super::goals::TState::new(false, false), found: None }));
            } else {
                tg(m, 1, chickens, false);
                t(m, 1, mk::target_full(baby_turtles, false, 10, false, false));
            }
        }
        "wolf" => {
            g(m, 1, mk::float());
            g(m, 1, mk::special(Special::TamePanic { speed: 1.5, inner: Box::new(mk::panic(1.5)) }));
            g(m, 2, mk::sit());
            g(m, 3, mk::avoid(llamas, 24.0, 1.5, 1.5));
            g(m, 4, mk::leap(0.4));
            g(m, 5, mk::melee(1.0, true));
            g(m, 6, mk::follow_owner(1.0, 10.0, 2.0));
            g(m, 7, mk::breed(1.0));
            g(m, 8, mk::wander(1.0));
            g(m, 9, mk::special(Special::Beg { range: 8.0, player: None, time: 0 }));
            g(m, 10, mk::look_player(8.0));
            g(m, 10, mk::random_look());
            t(m, 1, mk::owner_hurt_by());
            t(m, 2, mk::owner_hurt());
            t(m, 3, mk::hurt_by_alert(&[], &[]));
            t(m, 4, mk::target_full(angry_at, true, 10, true, false));
            t(m, 5, mk::special(Special::NonTameTarget { sel: wolf_prey, st: super::goals::TState::new(false, false), found: None }));
            t(m, 6, mk::special(Special::NonTameTarget { sel: baby_turtles, st: super::goals::TState::new(false, false), found: None }));
            tg(m, 7, skeletons, false);
            malus(m, PathType::PowderSnow, -1.0);
            malus(m, PathType::OnTopOfPowderSnow, -1.0);
        }
        "parrot" => {
            g(m, 0, mk::panic(1.25));
            g(m, 0, mk::float());
            g(m, 1, mk::look_player(8.0));
            g(m, 2, mk::sit());
            g(m, 2, mk::follow_owner(1.0, 5.0, 1.0));
            g(m, 2, mk::fly(1.0));
            g(m, 3, mk::special(Special::FollowMob { speed: 1.0, stop: 3.0, area: 7.0, following: None, recalc: 0 }));
            malus(m, PathType::FireInNeighbor, -1.0);
            malus(m, PathType::Fire, -1.0);
            malus(m, PathType::Cocoa, -1.0);
        }
        "fox" => {
            g(m, 0, mk::float());
            g(m, 2, mk::panic(2.2));
            g(m, 3, mk::breed(1.0));
            g(m, 4, mk::avoid(wild_wolves_not_defending, 8.0, 1.6, 1.4));
            g(m, 4, mk::avoid(polar_bears, 8.0, 1.6, 1.4));
            g(m, 7, mk::melee_k(1.2, true, MeleeKind::Fox));
            g(m, 8, mk::follow_parent(1.25));
            g(m, 10, mk::special(Special::EatBerries { speed: 1.2, wanted: None, ticks: 0 }));
            g(m, 10, mk::leap(0.4));
            g(m, 11, mk::wander(1.0));
            g(m, 12, mk::look_player(24.0));
            t(m, 3, mk::target_full(fox_prey, false, 10, false, false));
            malus(m, PathType::DamagingInNeighbor, 0.0);
            malus(m, PathType::Damaging, 0.0);
        }
        "panda" => {
            g(m, 0, mk::float());
            g(m, 2, mk::panic(2.0));
            g(m, 2, mk::breed(1.0));
            g(m, 3, mk::melee_k(1.2, true, MeleeKind::Panda));
            g(m, 4, mk::tempt(1.0, "panda_food", false));
            g(m, 6, mk::avoid(super::goals::players, 8.0, 2.0, 2.0));
            g(m, 6, mk::avoid(monsters_any, 4.0, 2.0, 2.0));
            g(m, 8, mk::special(Special::PandaSneeze));
            g(m, 10, mk::random_look());
            g(m, 13, mk::follow_parent(1.25));
            g(m, 14, mk::wander(1.0));
            t(m, 1, mk::hurt_by_alert(&[], &[]));
        }
        "polar_bear" => {
            g(m, 0, mk::float());
            g(m, 1, mk::melee_k(1.25, true, MeleeKind::PolarBear));
            g(m, 1, mk::panic(2.0));
            g(m, 4, mk::follow_parent(1.25));
            g(m, 5, mk::stroll(1.0));
            g(m, 6, mk::look_player(6.0));
            g(m, 7, mk::random_look());
            t(m, 1, mk::hurt_by_alert(&[], &[]));
            t(m, 2, mk::special(Special::PolarBearAttackPlayers { inner: Box::new(mk::target_players(true)) }));
            t(m, 3, mk::target_full(polar_bear_players, true, 10, true, false));
            t(m, 4, mk::target_full(foxes, false, 10, true, true));
        }
        "bee" => {
            g(m, 0, mk::melee_k(1.4, true, MeleeKind::Bee));
            g(m, 2, mk::breed(1.0));
            g(m, 3, mk::tempt(1.25, "bee_food", false));
            g(m, 5, mk::follow_parent(1.25));
            g(m, 8, mk::fly(1.0));
            g(m, 9, mk::float());
            t(m, 1, mk::hurt_by_alert(&[], &[]));
            t(m, 2, mk::target_full(angry_at, true, 10, true, false));
            malus(m, PathType::FireInNeighbor, -1.0);
            malus(m, PathType::Water, -1.0);
            malus(m, PathType::WaterBorder, 16.0);
            malus(m, PathType::Cocoa, -1.0);
            malus(m, PathType::Fence, -1.0);
        }
        "strider" => {
            g(m, 1, mk::panic(1.65));
            g(m, 2, mk::breed(1.0));
            g(m, 3, mk::tempt(1.4, "strider_tempt_items", false));
            g(m, 4, mk::special(Special::StriderGoToLava { wanted: None }));
            g(m, 5, mk::follow_parent(1.0));
            g(m, 7, mk::stroll_i(1.0, 60));
            g(m, 8, mk::look_player(8.0));
            g(m, 8, mk::random_look());
            g(m, 9, mk::look_mob(8.0, same_type));
            malus(m, PathType::Water, -1.0);
            malus(m, PathType::Lava, 0.0);
            malus(m, PathType::FireInNeighbor, 0.0);
            malus(m, PathType::Fire, 0.0);
        }
        "goat" => {
            g(m, 0, mk::float());
            g(m, 1, mk::panic(2.0));
            g(m, 2, mk::breed(1.0));
            g(m, 3, mk::tempt(1.25, "goat_food", false));
            g(m, 4, mk::follow_parent(1.25));
            g(m, 5, mk::special(Special::GoatRam { cooldown: 300, state: 0, ticks: 0, target: None, dir: [0.0; 3] }));
            g(m, 6, mk::wander(1.0));
            g(m, 7, mk::look_player(6.0));
            g(m, 8, mk::random_look());
            malus(m, PathType::PowderSnow, -1.0);
        }
        "armadillo" => {
            g(m, 0, mk::float());
            g(m, 1, mk::special(Special::ArmadilloRoll { scared: 0 }));
            g(m, 2, mk::panic(2.0));
            g(m, 3, mk::breed(1.0));
            g(m, 4, mk::tempt(1.25, "armadillo_food", false));
            g(m, 5, mk::follow_parent(1.25));
            g(m, 6, mk::wander(1.0));
            g(m, 7, mk::look_player(6.0));
            g(m, 8, mk::random_look());
        }
        "sniffer" => {
            g(m, 0, mk::float());
            g(m, 1, mk::panic(2.0));
            g(m, 2, mk::breed(1.0));
            g(m, 3, mk::tempt(1.25, "sniffer_food", false));
            g(m, 4, mk::follow_parent(1.25));
            g(m, 5, mk::special(Special::SnifferDig { cooldown: 9600, ticks: 0 }));
            g(m, 6, mk::wander(1.0));
            g(m, 7, mk::look_player(6.0));
            g(m, 8, mk::random_look());
        }
        "frog" => {
            g(m, 0, mk::float());
            g(m, 2, mk::breed(1.0));
            g(m, 3, mk::tempt(1.25, "frog_food", false));
            g(m, 4, mk::melee(1.25, false));
            g(m, 5, mk::wander(1.0));
            g(m, 6, mk::look_player(6.0));
            g(m, 7, mk::random_look());
            t(m, 1, mk::target_full(frog_prey, false, 10, true, false));
            malus(m, PathType::Water, 4.0);
            malus(m, PathType::Trapdoor, -1.0);
        }
        "tadpole" => {
            g(m, 0, mk::panic(1.25));
            g(m, 3, mk::tempt(1.25, "frog_food", false));
            g(m, 4, mk::swim(1.0, 10));
        }
        "axolotl" => {
            g(m, 1, mk::special(Special::PlayDead { ticks: 0 }));
            g(m, 2, mk::breed(0.2));
            g(m, 3, mk::tempt(0.2, "axolotl_food", false));
            g(m, 4, mk::melee(0.6, true));
            g(m, 5, mk::follow_parent(0.2));
            g(m, 6, mk::swim(0.2, 40));
            g(m, 6, mk::try_find_water());
            g(m, 7, mk::look_player(6.0));
            t(m, 1, mk::target_full(axolotl_prey, false, 10, true, false));
            malus(m, PathType::Water, 0.0);
        }
        "nautilus" | "zombie_nautilus" => {
            g(m, 1, mk::panic(1.5));
            g(m, 2, mk::breed(1.0));
            g(m, 3, mk::tempt(1.25, "nautilus_food", false));
            g(m, 4, mk::melee(1.2, true));
            g(m, 5, mk::follow_parent(1.25));
            g(m, 6, mk::swim(1.0, 20));
            g(m, 7, mk::look_player(6.0));
            t(m, 1, mk::hurt_by());
            if n == "zombie_nautilus" {
                players_t(m, 2);
            }
            malus(m, PathType::Water, 0.0);
        }
        "happy_ghast" => {
            g(m, 3, mk::tempt_fly(1.0, "happy_ghast_tempt_items", 3.0));
            g(m, 5, mk::special(Special::GhastFloat { range: 16 }));
        }
        // ------------------------------------------------ water
        "cod" | "salmon" | "tropical_fish" | "pufferfish" => {
            g(m, 0, mk::panic(1.25));
            g(m, 2, mk::avoid(fish_threat, 8.0, 1.6, 1.4));
            g(m, 4, mk::swim(1.0, 40));
            if n != "pufferfish" {
                g(m, 5, mk::special(Special::FollowFlockLeader { leader: None, next: 0 }));
            } else {
                g(m, 1, mk::special(Special::PufferfishPuff));
            }
        }
        "squid" | "glow_squid" => {
            g(m, 0, mk::special(Special::SquidRandom));
            g(m, 1, mk::special(Special::SquidFlee { ticks: 0 }));
        }
        "dolphin" => {
            g(m, 0, mk::special(Special::BreathAir));
            g(m, 0, mk::try_find_water());
            g(m, 2, mk::special(Special::SwimWithPlayer { speed: 4.0, player: None }));
            g(m, 4, mk::swim(1.0, 10));
            g(m, 4, mk::random_look());
            g(m, 5, mk::look_player(6.0));
            g(m, 5, mk::special(Special::DolphinJump { interval: 10, breached: false }));
            g(m, 6, mk::melee(1.2, true));
            g(m, 9, mk::avoid(guardians, 8.0, 1.0, 1.0));
            t(m, 1, mk::hurt_by_alert(&["guardian"], &[]));
        }
        "guardian" | "elder_guardian" => {
            g(m, 4, mk::special(Special::GuardianAttack { ticks: 0, elder: n == "elder_guardian" }));
            let mtr = mk::towards_restriction(1.0);
            g(m, 5, mtr);
            g(m, 7, mk::swim(1.0, 80));
            g(m, 8, mk::look_player(8.0));
            g(m, 8, mk::look_mob(12.0, guardians));
            g(m, 9, mk::random_look());
            t(m, 1, mk::target_full(guardian_prey, true, 10, true, false));
            malus(m, PathType::Water, 0.0);
        }
        // ------------------------------------------------ golems & villagers
        "iron_golem" => {
            g(m, 1, mk::melee(1.0, true));
            g(m, 2, mk::towards_target(0.9, 32.0));
            g(m, 4, mk::golem_stroll(0.6));
            g(m, 5, mk::special(Special::OfferFlower { tick: 0, villager: None }));
            g(m, 7, mk::look_player(6.0));
            g(m, 8, mk::random_look());
            t(m, 2, mk::hurt_by());
            t(m, 3, mk::target_full(angry_at, true, 10, true, false));
            t(m, 3, mk::target_full(enemies_not_creeper, false, 5, false, false));
        }
        "snow_golem" => {
            g(m, 1, mk::ranged(1.25, 20, 20, 10.0));
            g(m, 2, mk::wander_p(1.0, 1.0000001e-5));
            g(m, 3, mk::look_player(6.0));
            g(m, 4, mk::random_look());
            t(m, 1, mk::target_full(enemies, false, 10, true, false));
        }
        "copper_golem" => {
            g(m, 1, mk::panic(1.5));
            g(m, 5, mk::wander(1.0));
            g(m, 6, mk::look_player(6.0));
            g(m, 7, mk::random_look());
        }
        "villager" => {
            g(m, 0, mk::float());
            g(m, 1, mk::avoid(zombies_and_raiders, 8.0, 0.5, 0.5));
            g(m, 1, mk::panic(0.5));
            g(m, 5, mk::wander_p(0.5, 0.001));
            g(m, 9, mk::look_player_p(3.0, 1.0));
            g(m, 10, mk::look_mob(8.0, non_players));
        }
        "wandering_trader" => {
            g(m, 0, mk::float());
            g(m, 1, mk::avoid(zombies_not_piglin, 8.0, 0.5, 0.5));
            g(m, 1, mk::avoid(evokers, 12.0, 0.5, 0.5));
            g(m, 1, mk::avoid(vindicators, 8.0, 0.5, 0.5));
            g(m, 1, mk::avoid(vexes, 8.0, 0.5, 0.5));
            g(m, 1, mk::avoid(pillagers, 15.0, 0.5, 0.5));
            g(m, 1, mk::avoid(illusioners, 12.0, 0.5, 0.5));
            g(m, 1, mk::avoid(zoglins, 10.0, 0.5, 0.5));
            g(m, 1, mk::panic(0.5));
            g(m, 4, mk::towards_restriction(0.35));
            g(m, 8, mk::wander(0.35));
            g(m, 9, mk::look_player_p(3.0, 1.0));
            g(m, 10, mk::look_mob(8.0, non_players));
        }
        "allay" => {
            g(m, 0, mk::float());
            g(m, 1, mk::tempt_fly(1.25, "rime:any", 2.0));
            g(m, 5, mk::fly(1.0));
            g(m, 6, mk::look_player(6.0));
        }
        // ------------------------------------------------ monsters
        "zombie" | "husk" | "zombie_villager" | "zombified_piglin" | "drowned" => zombie_common(m),
        "giant" => {}
        "skeleton" | "stray" | "wither_skeleton" | "bogged" | "parched" => {
            skeleton_common(m);
            reassess_weapon(m, 2);
        }
        "creeper" => {
            g(m, 1, mk::float());
            g(m, 2, mk::swell());
            g(m, 3, mk::avoid(cats, 6.0, 1.0, 1.2));
            g(m, 4, mk::melee(1.0, false));
            g(m, 5, mk::wander_p(0.8, 0.001));
            g(m, 6, mk::look_player(8.0));
            g(m, 6, mk::random_look());
            players_t(m, 1);
            t(m, 2, mk::hurt_by());
        }
        "spider" | "cave_spider" => {
            g(m, 1, mk::float());
            g(m, 2, mk::avoid(calm_armadillos, 6.0, 1.0, 1.2));
            g(m, 3, mk::leap(0.4));
            g(m, 4, mk::melee_k(1.0, true, MeleeKind::Spider));
            g(m, 5, mk::wander_p(0.8, 0.001));
            g(m, 6, mk::look_player(8.0));
            g(m, 6, mk::random_look());
            t(m, 1, mk::hurt_by());
            t(m, 2, super::goals::Goal::Nearest { sel: super::goals::any, players: true, interval: super::reduced(10), found: None, st: super::goals::TState::new(true, false), only_dark: true });
            t(m, 3, super::goals::Goal::Nearest { sel: iron_golems, players: false, interval: super::reduced(10), found: None, st: super::goals::TState::new(true, false), only_dark: true });
        }
        "enderman" => {
            g(m, 0, mk::float());
            g(m, 1, mk::special(Special::EndermanFreeze { target: None }));
            g(m, 2, mk::melee(1.0, false));
            g(m, 7, mk::wander_p(1.0, 0.0));
            g(m, 8, mk::look_player(8.0));
            g(m, 8, mk::random_look());
            g(m, 10, mk::special(Special::EndermanLeaveBlock));
            g(m, 11, mk::special(Special::EndermanTakeBlock));
            t(m, 1, mk::special(Special::EndermanLookForPlayer { pending: None, aggro: 0, teleport: 0, st: super::goals::TState::new(false, false) }));
            t(m, 2, mk::hurt_by());
            t(m, 3, mk::target_full(endermites, false, 10, true, false));
            malus(m, PathType::Water, -1.0);
        }
        "endermite" | "silverfish" => {
            g(m, 1, mk::float());
            if n == "endermite" {
                g(m, 2, mk::melee(1.0, false));
                g(m, 3, mk::wander(1.0));
                g(m, 7, mk::look_player(8.0));
                g(m, 8, mk::random_look());
            } else {
                g(m, 3, mk::special(Special::SilverfishWakeUp { ticks: 0 }));
                g(m, 4, mk::melee(1.0, false));
                g(m, 5, mk::special(Special::SilverfishMerge { dir: None, do_merge: false, inner: Box::new(mk::stroll_i(1.0, 10)) }));
            }
            t(m, 1, mk::hurt_by_alert(&[], &[]));
            players_t(m, 2);
        }
        "blaze" => {
            g(m, 4, mk::special(Special::BlazeAttack { step: 0, time: 0, last_seen: 0 }));
            g(m, 5, mk::towards_restriction(1.0));
            g(m, 7, mk::wander_p(1.0, 0.0));
            g(m, 8, mk::look_player(8.0));
            g(m, 8, mk::random_look());
            t(m, 1, mk::hurt_by_alert(&[], &[]));
            players_t(m, 2);
            malus(m, PathType::Water, -1.0);
            malus(m, PathType::Lava, 8.0);
            malus(m, PathType::FireInNeighbor, 0.0);
            malus(m, PathType::Fire, 0.0);
        }
        "ghast" => {
            g(m, 5, mk::special(Special::GhastFloat { range: 64 }));
            g(m, 7, mk::special(Special::GhastLook));
            g(m, 7, mk::special(Special::GhastShoot { charge: 0 }));
            t(m, 1, mk::special(Special::GhastTarget { inner: Box::new(mk::target_full(super::goals::any, true, 10, true, false)) }));
        }
        "slime" | "magma_cube" => {
            g(m, 1, mk::special(Special::SlimeFloat));
            g(m, 2, mk::special(Special::SlimeAttack { growl: 0 }));
            g(m, 3, mk::special(Special::SlimeRandomDir { yrot: 0.0, next: 0 }));
            g(m, 5, mk::special(Special::SlimeKeepJumping));
            t(m, 1, mk::target_full(super::goals::any, true, 10, true, false));
            tg(m, 3, iron_golems, true);
        }
        "phantom" => {
            g(m, 1, mk::special(Special::PhantomStrategy { next_sweep: 0 }));
            g(m, 2, mk::special(Special::PhantomSweep { cat_scare: 0, sweep: false }));
            g(m, 3, mk::special(Special::PhantomCircle { angle: 0.0, distance: 0.0, height: 0.0, clockwise: 1.0 }));
            t(m, 1, mk::special(Special::PhantomTarget { next_scan: 20 }));
        }
        "witch" => {
            g(m, 1, mk::float());
            g(m, 2, mk::ranged(1.0, 60, 60, 10.0));
            g(m, 2, mk::wander(1.0));
            g(m, 3, mk::look_player(8.0));
            g(m, 3, mk::random_look());
            t(m, 1, mk::hurt_by_ignore(&["pillager", "vindicator", "evoker", "illusioner", "ravager", "witch"]));
            t(m, 3, mk::target_full(super::goals::any, true, 10, true, false));
        }
        "pillager" => {
            g(m, 0, mk::float());
            illager_avoid_creaking(m, 1.0, 1.2);
            g(m, 3, mk::crossbow(1.0, 8.0));
            g(m, 8, mk::stroll(0.6));
            g(m, 9, mk::look_player_p(15.0, 1.0));
            g(m, 10, mk::look_mob(15.0, non_players));
            raider_targets(m, true);
        }
        "vindicator" => {
            g(m, 0, mk::float());
            illager_avoid_creaking(m, 1.0, 1.2);
            g(m, 5, mk::melee_k(1.0, false, MeleeKind::Vindicator));
            g(m, 8, mk::stroll(0.6));
            g(m, 9, mk::look_player_p(3.0, 1.0));
            g(m, 10, mk::look_mob(8.0, non_players));
            t(m, 1, mk::hurt_by_alert(&["pillager", "vindicator", "evoker", "illusioner", "ravager", "witch"], &[]));
            players_t(m, 2);
            tg(m, 3, villagers, true);
            tg(m, 3, iron_golems, true);
        }
        "evoker" | "illusioner" => {
            let evoker = n == "evoker";
            g(m, 0, mk::float());
            g(m, 1, mk::special(Special::CastingSpell));
            if evoker {
                g(m, 2, mk::avoid(super::goals::players, 8.0, 0.6, 1.0));
                g(m, 3, mk::avoid(creakings, 8.0, 0.6, 1.0));
                g(m, 4, mk::special(Special::Spell { kind: 1, warmup: 0, next: 0, cast_time: 100, interval: 340, warmup_time: 20 }));
                g(m, 5, mk::special(Special::Spell { kind: 2, warmup: 0, next: 0, cast_time: 40, interval: 100, warmup_time: 20 }));
                g(m, 6, mk::special(Special::Spell { kind: 3, warmup: 0, next: 0, cast_time: 40, interval: 140, warmup_time: 40 }));
            } else {
                g(m, 3, mk::avoid(creakings, 8.0, 1.0, 1.2));
                g(m, 4, mk::special(Special::Spell { kind: 4, warmup: 0, next: 0, cast_time: 20, interval: 340, warmup_time: 20 }));
                g(m, 5, mk::special(Special::Spell { kind: 5, warmup: 0, next: 0, cast_time: 20, interval: 180, warmup_time: 20 }));
                g(m, 6, mk::bow(0.5, 20, 15.0));
            }
            g(m, 8, mk::stroll(0.6));
            g(m, 9, mk::look_player_p(3.0, 1.0));
            g(m, 10, mk::look_mob(8.0, non_players));
            t(m, 1, mk::hurt_by_alert(&["pillager", "vindicator", "evoker", "illusioner", "ravager", "witch"], &[]));
            let mut p = mk::target_players(true);
            if let super::goals::Goal::Nearest { st, .. } = &mut p {
                st.unseen_memory = 300;
            }
            t(m, 2, p);
            let mut v = mk::target(villagers, false);
            if let super::goals::Goal::Nearest { st, .. } = &mut v {
                st.unseen_memory = 300;
            }
            t(m, 3, v);
            tg(m, 3, iron_golems, false);
        }
        "ravager" => {
            g(m, 0, mk::float());
            g(m, 4, mk::melee_k(1.0, true, MeleeKind::Ravager));
            g(m, 5, mk::wander(0.4));
            g(m, 6, mk::look_player(6.0));
            g(m, 10, mk::look_mob(8.0, non_players));
            t(m, 2, mk::hurt_by_alert(&["pillager", "vindicator", "evoker", "illusioner", "ravager", "witch"], &[]));
            players_t(m, 3);
            tg(m, 4, adult_villagers, true);
            tg(m, 4, iron_golems, true);
            malus(m, PathType::Leaves, 0.0);
        }
        "vex" => {
            g(m, 0, mk::float());
            g(m, 4, mk::special(Special::VexCharge));
            g(m, 8, mk::special(Special::VexRandomMove));
            g(m, 9, mk::look_player_p(3.0, 1.0));
            g(m, 10, mk::look_mob(8.0, non_players));
            t(m, 1, mk::hurt_by_alert(&["pillager", "vindicator", "evoker", "illusioner", "ravager", "witch"], &[]));
            t(m, 2, mk::special(Special::VexCopyOwnerTarget));
            players_t(m, 3);
        }
        "shulker" => {
            g(m, 1, super::goals::Goal::LookAt { range: 8.0, prob: 0.02, sel: super::goals::any, players: true, only_h: true, at: None, time: 0 });
            g(m, 4, mk::special(Special::ShulkerAttack { time: 0 }));
            g(m, 7, mk::special(Special::ShulkerPeek { time: 0 }));
            g(m, 8, mk::random_look());
            t(m, 1, mk::hurt_by_alert(&["shulker"], &[]));
            t(m, 2, mk::target_full(super::goals::any, true, 10, true, false));
            t(m, 3, mk::target_full(enemies, false, 10, true, false));
        }
        "wither" => {
            g(m, 0, mk::special(Special::WitherDoNothing));
            g(m, 2, mk::ranged(1.0, 40, 40, 20.0));
            g(m, 5, mk::fly(1.0));
            g(m, 6, mk::look_player(8.0));
            g(m, 7, mk::random_look());
            t(m, 1, mk::hurt_by());
            t(m, 2, mk::target_full(wither_prey, false, 0, false, false));
            malus(m, PathType::Water, -1.0);
        }
        "ender_dragon" => {
            g(m, 0, mk::special(Special::DragonFly { node: [0.0, 100.0, 0.0], phase_ticks: 0, strafe: 0 }));
            t(m, 0, mk::target_full(super::goals::any, true, 0, false, false));
        }
        "bat" => {}
        "piglin" | "piglin_brute" | "hoglin" | "zoglin" => {
            g(m, 0, mk::float());
            let sel: Sel = match n {
                "piglin" => piglin_prey,
                "piglin_brute" => brute_prey,
                "hoglin" => hoglin_prey,
                _ => zoglin_prey,
            };
            if n == "piglin" {
                g(m, 2, mk::crossbow(1.0, 8.0));
            }
            g(m, 3, mk::melee(1.0, true));
            if n == "hoglin" {
                g(m, 1, mk::avoid(warped_fungus_holders_none, 6.0, 1.0, 1.2));
                g(m, 2, mk::breed(1.0));
                g(m, 4, mk::tempt(1.0, "hoglin_food", false));
                g(m, 5, mk::follow_parent(1.0));
            }
            g(m, 7, mk::wander(if n == "piglin" { 0.6 } else { 0.4 }));
            g(m, 8, mk::look_player(8.0));
            g(m, 9, mk::random_look());
            t(m, 1, mk::hurt_by_alert(&[], &[]));
            t(m, 2, mk::target_full(sel, n != "zoglin", 10, true, false));
        }
        "warden" => {
            g(m, 0, mk::special(Special::WardenRoar { ticks: 0 }));
            g(m, 1, mk::special(Special::WardenSonic { cooldown: 0, ticks: 0 }));
            g(m, 2, mk::melee(1.2, true));
            g(m, 6, mk::special(Special::WardenSniff { ticks: 0 }));
            g(m, 7, mk::wander(0.5));
            t(m, 1, mk::hurt_by());
            t(m, 2, mk::target_full(warden_prey, false, 10, false, false));
            malus(m, PathType::UnpassableRail, 8.0);
            malus(m, PathType::Lava, 8.0);
            malus(m, PathType::OnTopOfPowderSnow, 8.0);
        }
        "breeze" => {
            g(m, 1, mk::special(Special::BreezeShoot { cooldown: 0, ticks: 0 }));
            g(m, 2, mk::special(Special::BreezeJump { cooldown: 0 }));
            g(m, 6, mk::stroll(0.6));
            g(m, 7, mk::look_player(16.0));
            t(m, 1, mk::hurt_by());
            t(m, 2, mk::target_full(breeze_prey, false, 10, true, false));
        }
        "creaking" => {
            g(m, 1, mk::special(Special::CreakingFreeze));
            g(m, 2, mk::melee(1.0, true));
            g(m, 6, mk::stroll(0.4));
            players_t(m, 1);
        }
        _ => {}
    }
    m.nav.can_float = m.goals.iter().any(|s| matches!(s.goal, super::goals::Goal::Float));
    m.nav.can_pass_doors = true;
    if matches!(n, "zombie" | "husk" | "drowned" | "zombie_villager" | "vindicator" | "villager" | "wandering_trader" | "pillager" | "evoker" | "illusioner" | "piglin" | "piglin_brute") {
        m.nav.can_open_doors = matches!(n, "villager" | "vindicator" | "pillager" | "evoker" | "illusioner" | "piglin" | "piglin_brute");
    }
    if matches!(n, "skeleton" | "stray" | "wither_skeleton" | "bogged" | "parched" | "zombie" | "husk" | "zombie_villager") {
        // vanilla: AbstractSkeleton / Zombie avoid sun via RestrictSun only
    }
    if m.ty.is_animal() || matches!(n, "iron_golem" | "snow_golem") {
        malus(m, PathType::FireInNeighbor, if m.malus[PathType::FireInNeighbor as usize].is_nan() { 16.0 } else { m.malus[PathType::FireInNeighbor as usize] });
        malus(m, PathType::Fire, if m.malus[PathType::Fire as usize].is_nan() { -1.0 } else { m.malus[PathType::Fire as usize] });
    }
    if m.ty.is_skeleton() || m.ty.is_zombie() && !m.ty.is("drowned") {
        // undead in daylight
    }
    if m.ty.fire_immune() {
        malus(m, PathType::Lava, if m.malus[PathType::Lava as usize].is_nan() { 8.0 } else { m.malus[PathType::Lava as usize] });
        malus(m, PathType::FireInNeighbor, 0.0);
        malus(m, PathType::Fire, 0.0);
    }
    if m.ty.is_horse() {
        m.max_up_step = 1.0;
    }
    m.goals.sort_by_key(|s| s.prio);
    m.targets.sort_by_key(|s| s.prio);
}

fn wolves_any(_: &Mob, s: &Seen) -> bool {
    s.ty == "wolf"
}
fn fox_prey(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "chicken" | "rabbit" | "cod" | "salmon" | "tropical_fish") || (s.ty == "turtle" && s.baby && !s.in_water)
}
fn zombies_and_raiders(_: &Mob, s: &Seen) -> bool {
    matches!(s.ty, "zombie" | "husk" | "drowned" | "zombie_villager" | "pillager" | "vindicator" | "evoker" | "illusioner" | "ravager" | "vex" | "zoglin")
}
fn evokers(_: &Mob, s: &Seen) -> bool {
    s.ty == "evoker"
}
fn vindicators(_: &Mob, s: &Seen) -> bool {
    s.ty == "vindicator"
}
fn vexes(_: &Mob, s: &Seen) -> bool {
    s.ty == "vex"
}
fn pillagers(_: &Mob, s: &Seen) -> bool {
    s.ty == "pillager"
}
fn illusioners(_: &Mob, s: &Seen) -> bool {
    s.ty == "illusioner"
}
fn zoglins(_: &Mob, s: &Seen) -> bool {
    s.ty == "zoglin"
}
fn warped_fungus_holders_none(_: &Mob, _: &Seen) -> bool {
    false
}

fn raider_targets(m: &mut Mob, golem_see: bool) {
    t(m, 1, mk::hurt_by_alert(&["pillager", "vindicator", "evoker", "illusioner", "ravager", "witch"], &[]));
    players_t(m, 2);
    tg(m, 3, villagers, false);
    tg(m, 3, iron_golems, golem_see);
}

/// Called when a mob grows up or becomes a baby.
pub fn on_age_change(m: &mut Mob) {
    let _ = m;
}

// ---------------------------------------------------------------- walk targets

pub fn silverfish_walk(cx: &Ctx, p: [i32; 3]) -> f32 {
    let below = cx.block([p[0], p[1] - 1, p[2]]);
    if below.name().starts_with("infested_") {
        10.0
    } else {
        -cx.light_cost(p)
    }
}

pub fn guardian_walk(cx: &Ctx, p: [i32; 3]) -> f32 {
    if crate::registry::vstate::is_water(cx.block(p)) {
        10.0 + cx.light_cost(p) + 0.5 - 0.5
    } else {
        -cx.light_cost(p)
    }
}

pub fn turtle_walk(cx: &Ctx, p: [i32; 3]) -> f32 {
    if crate::registry::vstate::is_water(cx.block(p)) {
        return 10.0;
    }
    if cx.block([p[0], p[1] - 1, p[2]]).name() == "sand" {
        10.0
    } else {
        cx.light_cost(p)
    }
}

pub fn strider_walk(cx: &Ctx, p: [i32; 3]) -> f32 {
    let s = cx.block(p);
    if s.name() == "lava" {
        10.0
    } else if s.is_air() && cx.block([p[0], p[1] - 1, p[2]]).name() == "lava" {
        10.0
    } else {
        0.0
    }
}

// ---------------------------------------------------------------- items

fn item_name(s: Option<Stack>) -> &'static str {
    match s {
        Some(s) => Items::get().name(s.item),
        None => "",
    }
}

pub fn held(m: &Mob, hand: usize) -> &'static str {
    item_name(m.eq[hand])
}

/// `ProjectileUtil.getWeaponHoldingHand`
fn hand_with(m: &Mob, item: &str) -> usize {
    if held(m, MAINHAND) == item { MAINHAND } else { OFFHAND }
}

pub fn holding_bow(m: &Mob) -> bool {
    held(m, MAINHAND) == "bow" || held(m, OFFHAND) == "bow"
}

pub fn holding_crossbow(m: &Mob) -> bool {
    held(m, MAINHAND) == "crossbow" || held(m, OFFHAND) == "crossbow"
}

pub fn bow_hand(m: &Mob) -> usize {
    hand_with(m, "bow")
}

pub fn crossbow_hand(m: &Mob) -> usize {
    hand_with(m, "crossbow")
}

/// `LivingEntity.startUsingItem`
pub fn start_using(m: &mut Mob, hand: usize) {
    if let Some(s) = m.eq[hand] {
        m.living.using = Some(UseItem { hand: if hand == MAINHAND { 0 } else { 1 }, item: s.item, elapsed: 0 });
    }
}

/// `AbstractSkeleton.performRangedAttack` (and the illusioner's)
pub fn bow_shot(m: &mut Mob, cx: &mut Ctx, t: &Seen, power: f32) {
    let from = [cx.pos[0], cx.pos[1] + m.eye() - 0.1, cx.pos[2]];
    let to = [t.pos[0], t.pos[1] + t.height / 3.0, t.pos[2]];
    let arrow = match m.ty.name() {
        "stray" => Some("slowness"),
        "bogged" => Some("poison"),
        "parched" => Some("weakness"),
        _ => None,
    };
    let fire = m.ty.is("wither_skeleton") || m.eq[MAINHAND].is_some_and(|s| s.enchantment_level("flame") > 0);
    cx.acts.push(Act::Arrow { owner: cx.eid, from, target: to, power, tipped: arrow, fire, velocity: 1.6, uncertainty: (14 - cx.difficulty * 4) as f32 });
}

/// `CrossbowAttackMob.performCrossbowAttack`
pub fn crossbow_shot(m: &mut Mob, cx: &mut Ctx, t: &Seen) {
    let from = [cx.pos[0], cx.pos[1] + m.eye() - 0.1, cx.pos[2]];
    let to = [t.pos[0], t.pos[1] + t.height / 3.0, t.pos[2]];
    cx.acts.push(Act::Arrow { owner: cx.eid, from, target: to, power: 1.0, tipped: None, fire: false, velocity: 1.6, uncertainty: (14 - cx.difficulty * 4) as f32 });
    cx.acts.push(Act::Sound { name: "minecraft:item.crossbow.shoot".into(), pos: cx.pos, hostile: true, volume: 1.0, pitch: 1.0 });
}

/// `RangedAttackMob.performRangedAttack` of the non-bow users.
pub fn ranged_attack(m: &mut Mob, cx: &mut Ctx, t: &Seen, power: f32) {
    let eye = cx.pos[1] + m.eye();
    match m.ty.name() {
        "snow_golem" => {
            let from = [cx.pos[0], eye - 0.1, cx.pos[2]];
            let ty = t.pos[1] + t.eye - 1.1;
            let d = [t.pos[0] - from[0], ty - from[1], t.pos[2] - from[2]];
            let h = (d[0] * d[0] + d[2] * d[2]).sqrt() * 0.2;
            cx.acts.push(Act::Throw { owner: cx.eid, item: "snowball", from, dir: [d[0], d[1] + h, d[2]], velocity: 1.6, uncertainty: 12.0 });
            cx.acts.push(Act::Sound { name: "minecraft:entity.snow_golem.shoot".into(), pos: cx.pos, hostile: false, volume: 1.0, pitch: 0.4 / (cx.rng.r#gen::<f32>() * 0.4 + 0.8) });
        }
        "llama" | "trader_llama" => {
            let from = [cx.pos[0] - m.width() / 2.0 * (m.mo.body_yaw.to_radians().sin() as f64) * 0.0, eye - 0.1, cx.pos[2]];
            let d = [t.pos[0] - cx.pos[0], t.pos[1] + t.height / 3.0 - from[1], t.pos[2] - cx.pos[2]];
            let h = (d[0] * d[0] + d[2] * d[2]).sqrt() * 0.2;
            cx.acts.push(Act::Spit { owner: cx.eid, from, dir: [d[0], d[1] + h, d[2]] });
            m.v.t0 = 0;
        }
        "witch" => super::special::witch_throw(m, cx, t),
        "drowned" => {
            let from = [cx.pos[0], eye - 0.1, cx.pos[2]];
            let d = [t.pos[0] - from[0], t.pos[1] + t.height / 3.0 - from[1], t.pos[2] - from[2]];
            let h = (d[0] * d[0] + d[2] * d[2]).sqrt() * 0.2;
            cx.acts.push(Act::Trident { owner: cx.eid, from, dir: [d[0], d[1] + h, d[2]], uncertainty: (14 - cx.difficulty * 4) as f32 });
        }
        "wither" => super::special::wither_shoot(m, cx, 0, t.eid, t.pos, false),
        _ => {
            let _ = power;
        }
    }
}

/// `TemptGoal` / food checks: is `item` breeding food for this type?
pub fn is_food(m: &Mob, item: i32, cx: &Ctx) -> bool {
    let tag = match m.ty.name() {
        "cow" | "mooshroom" => "cow_food",
        "sheep" => "sheep_food",
        "pig" => "pig_food",
        "chicken" => "chicken_food",
        "rabbit" => "rabbit_food",
        "horse" | "donkey" | "mule" => "horse_food",
        "llama" | "trader_llama" => "llama_food",
        "camel" => "camel_food",
        "camel_husk" => "camel_husk_food",
        "cat" => "cat_food",
        "ocelot" => "ocelot_food",
        "wolf" => "wolf_food",
        "fox" => "fox_food",
        "panda" => "panda_food",
        "bee" => "bee_food",
        "turtle" => "turtle_food",
        "goat" => "goat_food",
        "frog" => "frog_food",
        "axolotl" => "axolotl_food",
        "strider" => "strider_food",
        "hoglin" => "hoglin_food",
        "armadillo" => "armadillo_food",
        "sniffer" => "sniffer_food",
        "parrot" => "parrot_food",
        "nautilus" => "nautilus_food",
        "zombie_horse" => "zombie_horse_food",
        "happy_ghast" => "happy_ghast_food",
        _ => return false,
    };
    cx.game.data.item_in(tag, item)
}

/// `Animal.canMate`
pub fn can_mate(m: &Mob, s: &Seen) -> bool {
    if s.eid == 0 || !s.in_love || !m.in_love() {
        return false;
    }
    let same = s.ty == m.ty.name() || (m.ty.is_horse() && matches!((m.ty.name(), s.ty), ("horse", "donkey") | ("donkey", "horse")));
    if !same {
        return false;
    }
    if m.ty.tamable() && (!m.tame || !s.tame || m.sitting) {
        return false;
    }
    true
}

/// `Wolf.wantsToAttack` / `TamableAnimal.wantsToAttack`
pub fn wants_to_attack(m: &Mob, t: &Seen, owner: &Seen) -> bool {
    if matches!(t.ty, "creeper" | "ghast" | "armor_stand") {
        return false;
    }
    if t.tame && t.owner == m.owner && t.owner.is_some() {
        return false;
    }
    if t.player && owner.player && t.eid == owner.eid {
        return false;
    }
    if t.ty == "horse" && t.tame {
        return false;
    }
    true
}

/// `Animal.ate` hooks (sheep regrow wool).
pub fn ate(m: &mut Mob, cx: &mut Ctx) {
    if m.ty.is("sheep") {
        m.v.sheared = false;
        if m.is_baby() {
            m.age = (m.age + (-m.age / 20 * 3).max(0).min(-m.age)).min(0);
        }
    }
    let _ = cx;
}

/// `TamableAnimal.tryToTeleportToOwner`
pub fn teleport_to_owner(m: &mut Mob, cx: &mut Ctx, o: &Seen) {
    let op = super::bp(o.pos);
    for _ in 0..10 {
        let dx: i32 = cx.rng.gen_range(-3..=3);
        let dy: i32 = cx.rng.gen_range(-1..=1);
        let dz: i32 = cx.rng.gen_range(-3..=3);
        if dx.abs() < 2 && dz.abs() < 2 {
            continue;
        }
        let p = [op[0] + dx, op[1] + dy, op[2] + dz];
        let below = cx.block([p[0], p[1] - 1, p[2]]);
        if !super::path::solid_render(below) && !super::path::legacy_solid(below) {
            continue;
        }
        if below.name().ends_with("_leaves") && !m.ty.is("parrot") {
            continue;
        }
        let free = (0..m.height().ceil() as i32).all(|y| !crate::game::physics::has_collision(cx.block([p[0], p[1] + y, p[2]])));
        if !free {
            continue;
        }
        cx.pos = [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5];
        m.nav.stop();
        return;
    }
}

/// `PolarBearMeleeAttackGoal.checkAndPerformAttack` (standing up)
pub fn polar_bear_melee(m: &mut Mob, cx: &mut Ctx, t: &Seen, attack: &mut i32) {
    if m.in_melee_range(cx, t) {
        if *attack <= 10 {
            m.v.standing = 1;
        }
        if *attack <= 0 {
            m.v.standing = 0;
        }
    } else {
        *attack = 20;
        m.v.standing = 0;
    }
}

// ---------------------------------------------------------------- spawning

/// `Mob.finalizeSpawn` and the type overrides: equipment, babies, variants.
pub fn finalize_spawn(m: &mut Mob, rng: &mut impl Rng, difficulty: i32, biome: &str, natural: bool) {
    let n = m.ty.name();
    let items = Items::get();
    let st = |name: &str| items.id(name).map(|i| Stack::new(i, 1));
    // special difficulty (local difficulty approximated by the global one)
    let effective = match difficulty {
        0 => 0.0,
        1 => 0.75,
        2 => 1.5,
        _ => 2.25,
    };
    let special_mult: f32 = if effective < 2.0 { 0.0 } else if effective > 4.0 { 1.0 } else { (effective - 2.0) / 2.0 };
    let _ = natural;
    m.left_handed = rng.r#gen::<f32>() < 0.05;
    m.can_pick_up = rng.r#gen::<f32>() < 0.55 * special_mult;
    match n {
        "zombie" | "husk" | "drowned" | "zombie_villager" => {
            m.baby = rng.r#gen::<f32>() < 0.05;
            if rng.r#gen::<f32>() < (if difficulty == 3 { 0.05 } else { 0.01 }) && n != "drowned" {
                let w = if rng.gen_range(0..3) == 0 { "iron_sword" } else { "iron_shovel" };
                m.eq[MAINHAND] = st(w);
            }
            populate_armor(m, rng, special_mult, difficulty);
            if n == "drowned" {
                if rng.r#gen::<f32>() > 0.9 {
                    let r = rng.gen_range(0..16);
                    if r < 10 {
                        m.eq[MAINHAND] = st("trident");
                    } else {
                        m.eq[MAINHAND] = st("fishing_rod");
                    }
                }
                if rng.r#gen::<f32>() < 0.03 {
                    m.eq[OFFHAND] = st("nautilus_shell");
                    m.drop_chance[OFFHAND] = 2.0;
                }
            }
            m.v.can_break_doors = rng.r#gen::<f32>() < special_mult * 0.1;
            // ZombieGroupData: knockback resistance bonus etc.
            let kb = rng.r#gen::<f64>() * 0.05;
            m.living.attrs.add_modifier(crate::game::combat::attr::Attr::KnockbackResistance, modifier("minecraft:random_spawn_bonus", kb, crate::game::combat::attr::Op::AddValue));
            let fr = rng.r#gen::<f64>() * 1.5 * special_mult as f64;
            if fr > 1.0 {
                m.living.attrs.add_modifier(crate::game::combat::attr::Attr::FollowRange, modifier("minecraft:random_zombie_spawn_bonus", fr, crate::game::combat::attr::Op::AddMultipliedTotal));
            }
            if rng.r#gen::<f32>() < special_mult * 0.05 {
                m.living.attrs.add_modifier(crate::game::combat::attr::Attr::SpawnReinforcements, modifier("minecraft:leader_zombie_bonus", rng.r#gen::<f64>() * 0.25 + 0.5, crate::game::combat::attr::Op::AddValue));
                m.living.attrs.add_modifier(crate::game::combat::attr::Attr::MaxHealth, modifier("minecraft:leader_zombie_bonus", rng.r#gen::<f64>() * 3.0 + 1.0, crate::game::combat::attr::Op::AddMultipliedTotal));
                m.v.can_break_doors = true;
            }
        }
        "zombified_piglin" => {
            m.baby = rng.r#gen::<f32>() < 0.05;
            m.eq[MAINHAND] = st("golden_sword");
        }
        "skeleton" | "stray" | "bogged" | "parched" => {
            populate_armor(m, rng, special_mult, difficulty);
            m.eq[MAINHAND] = st("bow");
            reassess_weapon(m, difficulty);
        }
        "wither_skeleton" => {
            m.eq[MAINHAND] = st("stone_sword");
            reassess_weapon(m, difficulty);
        }
        "piglin" => {
            m.baby = rng.r#gen::<f32>() < 0.2;
            if !m.baby {
                m.eq[MAINHAND] = if rng.r#gen::<f32>() < 0.5 { st("crossbow") } else { st("golden_sword") };
                for (slot, item) in [(HEAD, "golden_helmet"), (crate::game::combat::living::CHEST, "golden_chestplate"), (crate::game::combat::living::LEGS, "golden_leggings"), (crate::game::combat::living::FEET, "golden_boots")] {
                    if rng.r#gen::<f32>() < 0.1 {
                        m.eq[slot] = st(item);
                    }
                }
            }
        }
        "piglin_brute" => m.eq[MAINHAND] = st("golden_axe"),
        "pillager" => m.eq[MAINHAND] = st("crossbow"),
        "vindicator" => m.eq[MAINHAND] = st("iron_axe"),
        "illusioner" => m.eq[MAINHAND] = st("bow"),
        "vex" => {
            m.eq[MAINHAND] = st("iron_sword");
            m.drop_chance[MAINHAND] = 0.0;
        }
        "fox" => {
            m.v.variant = if biome.contains("snowy") || biome.contains("frozen") || biome.contains("grove") { 1 } else { 0 };
            let r = rng.r#gen::<f32>();
            m.eq[MAINHAND] = if r < 0.2 {
                let x = rng.r#gen::<f32>();
                if x < 0.5 { st("emerald") } else if x < 0.6 { st("rabbit_foot") } else if x < 0.7 { st("rabbit_hide") } else if x < 0.8 { st("egg") } else if x < 0.85 { st("feather") } else { st("wheat") }
            } else {
                None
            };
        }
        "sheep" => m.v.wool = crate::game::mob::random_sheep_color(rng),
        "rabbit" => {
            m.v.variant = if biome.contains("desert") {
                4
            } else if biome.contains("snowy") || biome.contains("frozen") {
                if rng.gen_range(0..5) < 4 { 1 } else { 3 }
            } else {
                match rng.gen_range(0..100) {
                    0..=49 => 0,
                    50..=89 => 5,
                    _ => 2,
                }
            };
        }
        "horse" => m.v.variant = rng.gen_range(0..7) | (rng.gen_range(0..5) << 8),
        "llama" | "trader_llama" => {
            m.v.variant = rng.gen_range(0..4);
            let strength_max = if rng.r#gen::<f32>() < 0.04 { 5 } else { 3 };
            m.v.t1 = 1 + rng.gen_range(0..strength_max);
        }
        "parrot" => m.v.variant = rng.gen_range(0..5),
        "axolotl" => m.v.variant = if rng.gen_range(0..1200) == 0 { 4 } else { rng.gen_range(0..4) },
        "tropical_fish" => {
            let common = [0x1010001, 0x1070701, 0x0010100, 0x1060001, 0x0070101, 0x1010101, 0x1060101, 0x0000001, 0x00e0001, 0x0040100, 0x00e0101, 0x10e0700, 0x10e0e00, 0x0040001, 0x0040301, 0x10e0e01, 0x1000100, 0x10e0101, 0x1060700, 0x0060600, 0x1040100, 0x00e0100];
            m.v.variant = if rng.r#gen::<f32>() < 0.9 { common[rng.gen_range(0..common.len())] } else { rng.gen_range(0..2) | (rng.gen_range(0..6) << 8) | (rng.gen_range(0..16) << 16) | (rng.gen_range(0..16) << 24) };
        }
        "salmon" => m.v.variant = match rng.gen_range(0..10) { 0..=1 => 0, 2..=8 => 1, _ => 2 },
        "panda" => {
            let gene = |rng: &mut dyn rand::RngCore| -> u8 {
                let i = rand::Rng::gen_range(rng, 0..16);
                match i {
                    0 => 1,      // lazy
                    1 => 2,      // worried
                    2 => 3,      // playful
                    3 => 4,      // brown (recessive)
                    4..=5 => 6,  // aggressive
                    6..=8 => 5,  // weak (recessive)
                    _ => 0,      // normal
                }
            };
            m.v.genes = (gene(rng), gene(rng));
        }
        "mooshroom" => m.v.variant = 0,
        "goat" => {
            m.v.screaming = rng.r#gen::<f64>() < 0.02;
            m.v.horns = (true, true);
        }
        "slime" | "magma_cube" => {
            let mut s = rng.gen_range(0..3);
            if s < 2 && rng.r#gen::<f32>() < 0.5 * special_mult {
                s += 1;
            }
            set_slime_size(m, 1 << s);
        }
        "phantom" => m.size = 0,
        "shulker" => m.v.color = 16,
        "spider" | "cave_spider" => {
            if difficulty == 3 && rng.r#gen::<f32>() < 0.1 * special_mult {
                let e = ["speed", "strength", "regeneration", "invisibility"][rng.gen_range(0..4)];
                if let Some(eff) = crate::game::combat::effects::Effect::from_name(e) {
                    let info = crate::game::combat::enchant::EntInfo { type_name: crate::game::etypes::EntityTypes::static_name(n), ..Default::default() };
                    m.living.add_effect(&info, crate::game::combat::effects::Instance::new(eff, -1, 0));
                }
            }
        }
        "cat" => m.v.variant = rng.gen_range(0..11),
        "wolf" => m.v.variant2 = 14,
        "creeper" => {}
        "witch" | "evoker" | "ravager" => {}
        "chicken" => m.v.egg_time = rng.gen_range(6000..12000),
        "turtle" => {}
        "dolphin" => m.air = 4800,
        "villager" => m.v.variant = 0,
        "wandering_trader" => m.v.despawn_delay = 48000,
        "strider" => {}
        "bee" => {}
        "warden" => {}
        _ => {}
    }
    if m.ty.ageable() && rng.r#gen::<f32>() < 0.05 && !matches!(n, "villager" | "wandering_trader") && m.ty.is_animal() && natural {
        m.age = -24000;
    }
    m.living.health = m.living.max_health();
}

/// `Monster.populateDefaultEquipmentSlots`
fn populate_armor(m: &mut Mob, rng: &mut impl Rng, special: f32, difficulty: i32) {
    if rng.r#gen::<f32>() >= 0.15 * special {
        return;
    }
    let mut tier = rng.gen_range(0..2);
    let keep = if difficulty == 3 { 0.1 } else { 0.25 };
    for _ in 0..3 {
        if rng.r#gen::<f32>() < 0.095 {
            tier += 1;
        }
    }
    let mat = ["leather", "golden", "chainmail", "iron", "diamond"][tier.min(4) as usize];
    let items = Items::get();
    let mut first = true;
    for (slot, piece) in [(crate::game::combat::living::FEET, "boots"), (crate::game::combat::living::LEGS, "leggings"), (crate::game::combat::living::CHEST, "chestplate"), (HEAD, "helmet")] {
        if !first && rng.r#gen::<f32>() < keep {
            break;
        }
        first = false;
        if m.eq[slot].is_none() {
            m.eq[slot] = items.id(&format!("{mat}_{piece}")).map(|i| Stack::new(i, 1));
        }
    }
}

/// `Slime.setSize`
pub fn set_slime_size(m: &mut Mob, size: i32) {
    use crate::game::combat::attr::Attr;
    let s = size.clamp(1, 127);
    m.size = s;
    let magma = m.ty.is("magma_cube");
    m.living.attrs.set_base(Attr::MaxHealth, (s * s) as f64);
    m.living.attrs.set_base(Attr::MovementSpeed, 0.2 + 0.1 * s as f64);
    m.living.attrs.set_base(Attr::AttackDamage, s as f64 * if magma { 1.0 } else { 1.0 });
    if magma {
        m.living.attrs.set_base(Attr::Armor, (s * 3) as f64);
    }
    m.living.health = m.living.max_health();
}

// ---------------------------------------------------------------- entity data

fn d_byte(b: &mut Vec<u8>, i: u8, v: u8) {
    b.u8(i).vi(0).u8(v);
}
fn d_int(b: &mut Vec<u8>, i: u8, v: i32) {
    b.u8(i).vi(1).vi(v);
}
fn d_long(b: &mut Vec<u8>, i: u8, v: i64) {
    b.u8(i).vi(2).vl(v);
}
fn d_float(b: &mut Vec<u8>, i: u8, v: f32) {
    b.u8(i).vi(3).f32(v);
}
fn d_bool(b: &mut Vec<u8>, i: u8, v: bool) {
    b.u8(i).vi(8).bool(v);
}

/// The synced data entries of a mob (without the entity id and terminator).
pub fn meta(m: &Mob, now: i64, b: &mut Vec<u8>) {
    let n = m.ty.name();
    // Entity
    let mut flags = 0u8;
    if m.fire > 0 && !m.ty.fire_immune() {
        flags |= 1;
    }
    if m.living.has_effect(crate::game::combat::effects::Effect::Invisibility) {
        flags |= 0x20;
    }
    if m.living.has_effect(crate::game::combat::effects::Effect::Glowing) {
        flags |= 0x40;
    }
    d_byte(b, 0, flags);
    d_int(b, 1, m.air);
    // pose: 0 standing, 7 dying, 1 fall flying, 3 sleeping, 5 crouching
    let pose = if m.death > 0 {
        7
    } else if n == "camel" && m.sitting || (matches!(n, "fox" | "panda" | "goat") && m.sitting) {
        if n == "camel" { 10 } else { 8 }
    } else if n == "warden" && m.v.dig > 0 {
        17
    } else if n == "warden" && m.v.roar > 0 {
        13
    } else if n == "warden" && m.v.sniff > 0 {
        14
    } else if n == "frog" && m.v.t2 > 0 {
        11
    } else {
        0
    };
    if let Some(name) = &m.v.custom_name {
        b.u8(2).vi(6).bool(true);
        b.extend_from_slice(&crate::nbt::Tag::text(name).to_network_bytes());
    }
    b.u8(6).vi(20).vi(pose);
    d_int(b, 7, m.frozen);
    // LivingEntity
    let mut lf = 0u8;
    if m.living.using.is_some() {
        lf |= 1;
        if m.living.using.is_some_and(|u| u.hand == 1) {
            lf |= 2;
        }
    }
    d_byte(b, 8, lf);
    d_float(b, 9, if m.death > 0 { 0.0 } else { m.living.health });
    // Mob
    let mut mf = 0u8;
    if m.no_ai {
        mf |= 1;
    }
    if m.left_handed {
        mf |= 2;
    }
    if m.aggressive {
        mf |= 4;
    }
    d_byte(b, 15, mf);
    if m.ty.ageable() {
        d_bool(b, 16, m.is_baby());
    }
    match n {
        "creeper" => {
            d_int(b, 16, m.v.swell_dir);
            d_bool(b, 17, m.v.powered);
            d_bool(b, 18, m.v.ignited);
        }
        "zombie" | "husk" | "drowned" | "zombie_villager" | "zombified_piglin" => {
            d_bool(b, 16, m.baby);
            d_bool(b, 18, m.v.conversion > 0 && n != "zombie_villager");
            if n == "zombie_villager" {
                d_bool(b, 19, m.v.conversion > 0);
            }
        }
        "zoglin" => d_bool(b, 16, m.baby),
        "piglin" => {
            d_bool(b, 17, m.baby);
            d_bool(b, 18, m.v.charging);
            d_bool(b, 19, m.v.t2 > 0);
        }
        "skeleton" => d_bool(b, 16, m.v.conversion > 0),
        "bogged" => d_bool(b, 16, m.v.sheared),
        "slime" | "magma_cube" => d_int(b, 16, m.size),
        "phantom" => d_int(b, 16, m.size),
        "enderman" => {
            match m.v.carry {
                Some(s) => {
                    b.u8(16).vi(15).vi(crate::game::mobtick::state_id(s));
                }
                None => {
                    b.u8(16).vi(15).vi(0);
                }
            }
            d_bool(b, 17, m.v.creepy);
            d_bool(b, 18, m.v.stared);
        }
        "ghast" => d_bool(b, 16, m.v.charge > 10),
        "blaze" => d_byte(b, 16, if m.v.flags & 1 != 0 { 1 } else { 0 }),
        "spider" | "cave_spider" => d_byte(b, 16, if m.mo.horizontal_collision { 1 } else { 0 }),
        "bat" => d_byte(b, 16, if m.v.resting { 1 } else { 0 }),
        "guardian" | "elder_guardian" => {
            d_bool(b, 16, m.v.flags & 1 != 0);
            d_int(b, 17, m.v.beam_target);
        }
        "witch" => {
            d_bool(b, 16, m.v.flags & 2 != 0);
            d_bool(b, 17, m.v.using_ticks > 0);
        }
        "pillager" => {
            d_bool(b, 16, false);
            d_bool(b, 17, m.v.charging);
        }
        "evoker" | "illusioner" => {
            d_bool(b, 16, false);
            d_byte(b, 17, m.v.spell);
        }
        "vindicator" | "ravager" => d_bool(b, 16, false),
        "vex" => d_byte(b, 16, if m.aggressive { 1 } else { 0 }),
        "shulker" => {
            b.u8(16).vi(12).vi(m.v.attach_face as i32);
            d_byte(b, 17, m.v.peek as u8);
            d_byte(b, 18, m.v.color);
        }
        "iron_golem" => d_byte(b, 16, if m.v.player_made { 1 } else { 0 }),
        "snow_golem" => d_byte(b, 16, if m.v.pumpkin { 0x10 } else { 0 }),
        "sheep" => d_byte(b, 18, (m.v.wool & 0x0F) | if m.v.sheared { 0x10 } else { 0 }),
        "wolf" | "cat" | "parrot" => {
            let mut tf = 0u8;
            if m.sitting {
                tf |= 1;
            }
            if m.tame {
                tf |= 4;
            }
            d_byte(b, 18, tf);
            match m.owner {
                Some(u) => {
                    b.u8(19).vi(13).bool(true).uuid(u);
                }
                None => {
                    b.u8(19).vi(13).bool(false);
                }
            }
            match n {
                "wolf" => {
                    d_bool(b, 20, m.v.interested);
                    d_int(b, 21, m.v.collar as i32);
                    d_long(b, 22, if m.v.anger_end > now { m.v.anger_end } else { -1 });
                }
                "cat" => {
                    d_bool(b, 21, m.v.t2 > 0);
                    d_bool(b, 22, false);
                    d_int(b, 23, m.v.collar as i32);
                }
                _ => d_int(b, 20, m.v.variant),
            }
        }
        "ocelot" => d_bool(b, 18, m.v.trusting),
        "fox" => {
            d_int(b, 18, m.v.variant);
            d_byte(b, 19, m.v.fox_flags);
        }
        "rabbit" => d_int(b, 18, m.v.variant),
        "mooshroom" => d_int(b, 18, m.v.variant),
        "axolotl" => {
            d_int(b, 18, m.v.variant);
            d_bool(b, 19, m.v.t2 > 0);
            d_bool(b, 20, m.v.from_bucket);
        }
        "goat" => {
            d_bool(b, 18, m.v.screaming);
            d_bool(b, 19, m.v.horns.0);
            d_bool(b, 20, m.v.horns.1);
        }
        "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" | "llama" | "trader_llama" | "camel" | "camel_husk" => {
            let mut hf = 0u8;
            if m.tame {
                hf |= 2;
            }
            if m.v.eating > 0 {
                hf |= 0x10;
            }
            if m.v.standing > 0 {
                hf |= 0x20;
            }
            d_byte(b, 18, hf);
            match n {
                "horse" => d_int(b, 19, m.v.variant),
                "llama" | "trader_llama" => {
                    d_bool(b, 19, m.v.chested);
                    d_int(b, 20, m.v.t1.max(1));
                    d_int(b, 21, m.v.variant);
                }
                "donkey" | "mule" => d_bool(b, 19, m.v.chested),
                _ => {}
            }
        }
        "panda" => {
            d_int(b, 18, m.v.unhappy);
            d_int(b, 19, m.v.sneeze);
            d_int(b, 20, m.v.eat);
            d_byte(b, 21, m.v.genes.0);
            d_byte(b, 22, m.v.genes.1);
            d_byte(b, 23, m.v.panda_flags);
        }
        "polar_bear" => d_bool(b, 18, m.v.standing > 0),
        "turtle" => {
            d_bool(b, 18, m.v.has_egg);
            d_bool(b, 19, m.v.laying > 0);
        }
        "dolphin" => {
            d_bool(b, 18, m.v.got_fish);
            d_int(b, 19, m.v.moistness);
        }
        "cod" | "salmon" | "tropical_fish" | "pufferfish" => {
            d_bool(b, 16, m.v.from_bucket);
            match n {
                "pufferfish" => d_int(b, 17, m.size),
                "tropical_fish" => d_int(b, 17, m.v.variant),
                "salmon" => d_int(b, 17, m.v.variant),
                _ => {}
            }
        }
        "tadpole" => d_bool(b, 16, m.v.from_bucket),
        "strider" => {
            d_int(b, 18, 0);
            d_bool(b, 19, m.v.suffocating);
        }
        "bee" => {
            let mut bf = 0u8;
            if m.v.anger_end > now {
                bf |= 2;
            }
            if m.v.has_stung {
                bf |= 4;
            }
            if m.v.has_nectar {
                bf |= 8;
            }
            d_byte(b, 18, bf);
            d_long(b, 19, if m.v.anger_end > now { m.v.anger_end } else { -1 });
        }
        "wither" => {
            d_int(b, 16, m.v.heads[0]);
            d_int(b, 17, m.v.heads[1]);
            d_int(b, 18, m.v.heads[2]);
            d_int(b, 19, m.v.invul);
        }
        "ender_dragon" => d_int(b, 16, m.v.phase as i32),
        "warden" => d_int(b, 16, m.v.anger.min(150)),
        "creaking" => {
            d_bool(b, 16, m.v.can_move);
            d_bool(b, 17, m.v.active);
        }
        "allay" => {
            d_bool(b, 16, false);
            d_bool(b, 17, true);
        }
        "glow_squid" => d_int(b, 18, m.v.t1),
        "hoglin" => d_bool(b, 18, false),
        "piglin_brute" => d_bool(b, 16, false),
        "camel_husk" => {}
        _ => {}
    }
}

fn modifier(id: &str, amount: f64, op: crate::game::combat::attr::Op) -> crate::game::combat::attr::Modifier {
    crate::game::combat::attr::Modifier { id: id.to_string(), amount, op }
}
