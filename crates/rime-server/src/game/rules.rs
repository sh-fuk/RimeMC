//! Game rules and difficulty (`GameRules`, `WorldData.difficulty`), shared
//! by every dimension and saved in `rime-gamerules.txt`.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};

/// (name, default, min, max); min > max means a boolean rule.
pub const RULES: &[(&str, i32, i32, i32)] = &[
    ("advance_time", 1, 1, 0),
    ("advance_weather", 1, 1, 0),
    ("allow_entering_nether_using_portals", 1, 1, 0),
    ("block_drops", 1, 1, 0),
    ("block_explosion_drop_decay", 1, 1, 0),
    ("command_blocks_work", 1, 1, 0),
    ("command_block_output", 1, 1, 0),
    ("drowning_damage", 1, 1, 0),
    ("elytra_movement_check", 1, 1, 0),
    ("ender_pearls_vanish_on_death", 1, 1, 0),
    ("entity_drops", 1, 1, 0),
    ("fall_damage", 1, 1, 0),
    ("fire_damage", 1, 1, 0),
    ("fire_spread_radius_around_player", 128, -1, 1000),
    ("forgive_dead_players", 1, 1, 0),
    ("freeze_damage", 1, 1, 0),
    ("global_sound_events", 1, 1, 0),
    ("immediate_respawn", 0, 1, 0),
    ("keep_inventory", 0, 1, 0),
    ("lava_source_conversion", 0, 1, 0),
    ("limited_crafting", 0, 1, 0),
    ("locator_bar", 1, 1, 0),
    ("log_admin_commands", 1, 1, 0),
    ("max_block_modifications", 32768, 1, i32::MAX),
    ("max_command_forks", 65536, 0, i32::MAX),
    ("max_command_sequence_length", 65536, 0, i32::MAX),
    ("max_entity_cramming", 24, 0, i32::MAX),
    ("max_minecart_speed", 8, 1, 1000),
    ("max_snow_accumulation_height", 1, 0, 8),
    ("mob_drops", 1, 1, 0),
    ("mob_explosion_drop_decay", 1, 1, 0),
    ("mob_griefing", 1, 1, 0),
    ("natural_health_regeneration", 1, 1, 0),
    ("player_movement_check", 1, 1, 0),
    ("players_nether_portal_creative_delay", 0, 0, i32::MAX),
    ("players_nether_portal_default_delay", 80, 0, i32::MAX),
    ("players_sleeping_percentage", 100, 0, i32::MAX),
    ("projectiles_can_break_blocks", 1, 1, 0),
    ("pvp", 1, 1, 0),
    ("raids", 1, 1, 0),
    ("random_tick_speed", 3, 0, i32::MAX),
    ("reduced_debug_info", 0, 1, 0),
    ("respawn_radius", 10, 0, i32::MAX),
    ("send_command_feedback", 1, 1, 0),
    ("show_advancement_messages", 1, 1, 0),
    ("show_death_messages", 1, 1, 0),
    ("spawner_blocks_work", 1, 1, 0),
    ("spawn_mobs", 1, 1, 0),
    ("spawn_monsters", 1, 1, 0),
    ("spawn_patrols", 1, 1, 0),
    ("spawn_phantoms", 1, 1, 0),
    ("spawn_wandering_traders", 1, 1, 0),
    ("spawn_wardens", 1, 1, 0),
    ("spectators_generate_chunks", 1, 1, 0),
    ("spread_vines", 1, 1, 0),
    ("tnt_explodes", 1, 1, 0),
    ("tnt_explosion_drop_decay", 0, 1, 0),
    ("universal_anger", 0, 1, 0),
    ("water_source_conversion", 1, 1, 0),
];

struct State {
    values: HashMap<&'static str, i32>,
    file: Option<PathBuf>,
}

static STATE: RwLock<Option<State>> = RwLock::new(None);
static DIFFICULTY: AtomicI32 = AtomicI32::new(2);
static DIFFICULTY_LOCKED: AtomicI32 = AtomicI32::new(0);
/// `/defaultgamemode` (-1: the configured one)
static DEFAULT_GM: AtomicI32 = AtomicI32::new(-1);

pub fn default_gamemode() -> Option<i32> {
    let g = DEFAULT_GM.load(Ordering::Relaxed);
    (g >= 0).then_some(g)
}

pub fn set_default_gamemode(g: i32) {
    DEFAULT_GM.store(g, Ordering::Relaxed);
    if let Some(s) = STATE.read().as_ref() {
        save(s);
    }
}

pub fn is_bool(name: &str) -> bool {
    RULES.iter().any(|r| r.0 == name && r.2 > r.3)
}

pub fn spec(name: &str) -> Option<(&'static str, i32, i32, i32)> {
    let name = name.trim_start_matches("minecraft:");
    RULES.iter().find(|r| r.0 == name).copied()
}

/// The value of a rule (booleans are 0 / 1).
pub fn get(name: &str) -> i32 {
    if let Some(s) = STATE.read().as_ref() {
        if let Some(v) = s.values.get(name) {
            return *v;
        }
    }
    spec(name).map(|r| r.1).unwrap_or(0)
}

pub fn on(name: &str) -> bool {
    get(name) != 0
}

/// Sets a rule; Err with the vanilla message when out of range.
pub fn set(name: &str, v: i32) -> Result<(), String> {
    let Some((key, _, min, max)) = spec(name) else { return Err(format!("Unknown game rule: {name}")) };
    if min <= max && (v < min || v > max) {
        return Err(format!("Value must be between {min} and {max}, found {v}"));
    }
    let mut g = STATE.write();
    let s = g.get_or_insert_with(|| State { values: HashMap::new(), file: None });
    s.values.insert(key, if min > max { (v != 0) as i32 } else { v });
    save(s);
    Ok(())
}

pub fn difficulty() -> i32 {
    DIFFICULTY.load(Ordering::Relaxed)
}

pub fn set_difficulty(d: i32) {
    DIFFICULTY.store(d.clamp(0, 3), Ordering::Relaxed);
    if let Some(s) = STATE.read().as_ref() {
        save(s);
    }
}

pub fn difficulty_locked() -> bool {
    DIFFICULTY_LOCKED.load(Ordering::Relaxed) != 0
}

pub fn lock_difficulty() {
    DIFFICULTY_LOCKED.store(1, Ordering::Relaxed);
    if let Some(s) = STATE.read().as_ref() {
        save(s);
    }
}

pub const DIFFICULTIES: [&str; 4] = ["peaceful", "easy", "normal", "hard"];

fn save(s: &State) {
    let Some(f) = &s.file else { return };
    let mut out = format!("difficulty={}\ndifficulty_locked={}\ndefault_gamemode={}\n", difficulty(), DIFFICULTY_LOCKED.load(Ordering::Relaxed), DEFAULT_GM.load(Ordering::Relaxed));
    let mut keys: Vec<_> = s.values.iter().collect();
    keys.sort();
    for (k, v) in keys {
        out.push_str(&format!("{k}={v}\n"));
    }
    if let Err(e) = std::fs::write(f, out) {
        tracing::error!("saving game rules: {e}");
    }
}

/// Loads `rime-gamerules.txt` from the world directory (missing = defaults).
pub fn load(world_dir: &std::path::Path) {
    let file = world_dir.join("rime-gamerules.txt");
    let mut values = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(&file) {
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let Ok(v) = v.trim().parse::<i32>() else { continue };
            match k.trim() {
                "difficulty" => DIFFICULTY.store(v.clamp(0, 3), Ordering::Relaxed),
                "difficulty_locked" => DIFFICULTY_LOCKED.store(v, Ordering::Relaxed),
                "default_gamemode" => DEFAULT_GM.store(v, Ordering::Relaxed),
                k => {
                    if let Some(r) = spec(k) {
                        values.insert(r.0, v);
                    }
                }
            }
        }
    }
    *STATE.write() = Some(State { values, file: Some(file) });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_validate() {
        assert_eq!(get("random_tick_speed"), 3);
        assert!(set("players_sleeping_percentage", -1).is_err());
        assert!(set("nonsense", 1).is_err());
        assert!(is_bool("keep_inventory") && !is_bool("random_tick_speed"));
        assert_eq!(RULES.len(), 59);
    }
}
