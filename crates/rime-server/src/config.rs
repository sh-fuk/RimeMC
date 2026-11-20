//! `rime.toml` — every RimeMC feature is configured here. Missing keys fall
//! back to defaults, so old config files keep working after upgrades.

use std::path::Path;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub server: ServerCfg,
    pub world: WorldCfg,
    pub gpu: GpuCfg,
    pub pregen: PregenCfg,
    pub ram_world: RamWorldCfg,
    pub level_tick: LevelTickCfg,
    pub player_cost: PlayerCostCfg,
    pub fleet: FleetCfg,
    pub anticheat: AntiCheatCfg,
    pub bedrock: BedrockCfg,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerCfg {
    pub bind: String,
    pub motd: String,
    pub max_players: u32,
    pub view_distance: u32,
    pub simulation_distance: u32,
    /// 0 = use every CPU core
    pub worker_threads: usize,
    /// Max simultaneous connections from one IP (anti connection-flood).
    pub max_connections_per_ip: u32,
    /// Max new connections per second from one IP.
    pub connection_rate_per_ip: u32,
    pub compression_threshold: i32,
    /// Game mode for joining players: survival / creative / adventure / spectator
    pub default_gamemode: GameMode,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

impl GameMode {
    pub fn id(self) -> i32 {
        self as i32
    }
}

impl Default for ServerCfg {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:25565".into(),
            motd: "§bRimeMC§r - fast by design".into(),
            max_players: 100,
            view_distance: 10,
            simulation_distance: 8,
            worker_threads: 0,
            max_connections_per_ip: 5,
            connection_rate_per_ip: 5,
            compression_threshold: 256,
            default_gamemode: GameMode::Survival,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorldFormat {
    /// RimeMC AirWorld (.air) — default
    Airworld,
    /// Vanilla Anvil (.mca)
    Vanilla,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldCfg {
    pub name: String,
    pub format: WorldFormat,
    /// 0 = random seed on first start (then stored in the world)
    pub seed: i64,
    /// zstd level for AirWorld (1..=22)
    pub compression_level: i32,
}

impl Default for WorldCfg {
    fn default() -> Self {
        Self { name: "world".into(), format: WorldFormat::Airworld, seed: 0, compression_level: 6 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuCfg {
    /// Use the GPU for world generation when one is available (falls back to CPU automatically).
    pub enabled: bool,
    /// Minimum number of chunks in a batch before it is worth sending to the GPU.
    pub min_batch: usize,
}

impl Default for GpuCfg {
    fn default() -> Self {
        Self { enabled: true, min_batch: 8 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PregenCfg {
    pub enabled: bool,
    /// How many predicted chunks to queue per player per prediction round.
    pub candidates: usize,
    /// How far ahead (seconds) the movement predictor looks.
    pub lookahead_seconds: f64,
    /// Global cap of queued pre-generation jobs.
    pub max_queue: usize,
    /// Threads for pre-generation (0 = cores - 1, so ticking always has a core).
    pub threads: usize,
}

impl Default for PregenCfg {
    fn default() -> Self {
        Self { enabled: true, candidates: 48, lookahead_seconds: 4.0, max_queue: 4096, threads: 0 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RamWorldCfg {
    pub enabled: bool,
    /// Memory (MB) the RAM world may use for region data. Regions beyond this
    /// stay on disk and are paged in on demand ("partial RAM world").
    pub max_memory_mb: u64,
    /// Seconds between automatic syncs to disk. `./rime sync` forces one.
    pub save_interval_seconds: u64,
}

impl Default for RamWorldCfg {
    fn default() -> Self {
        Self { enabled: true, max_memory_mb: 2048, save_interval_seconds: 30 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LevelTickCfg {
    pub enabled: bool,
    /// Mobs in one chunk at or above this count make the chunk "crowded".
    pub crowded_threshold: u32,
    /// Every additional `step` mobs above the threshold adds one tick to the interval.
    pub step: u32,
    /// Upper bound for the interval (e.g. 4 = mobs tick at least every 4th tick).
    pub max_interval: u32,
}

impl Default for LevelTickCfg {
    fn default() -> Self {
        Self { enabled: true, crowded_threshold: 24, step: 16, max_interval: 4 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerCostCfg {
    pub enabled: bool,
    /// A cell (region) using more than this many ms per tick is "heavy".
    pub cell_mspt_budget: f64,
    /// View distance never goes below this.
    pub min_view_distance: u32,
    /// Chunk sends per tick allowed for an over-budget player (normal: unlimited up to 16).
    pub throttled_chunk_sends_per_tick: u32,
    /// Seconds a player must stay under budget before restrictions are lifted step by step.
    pub recovery_seconds: f64,
}

impl Default for PlayerCostCfg {
    fn default() -> Self {
        Self { enabled: true, cell_mspt_budget: 25.0, min_view_distance: 6, throttled_chunk_sends_per_tick: 4, recovery_seconds: 5.0 }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FleetMode {
    Standalone,
    Router,
    Node,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetCfg {
    pub mode: FleetMode,
    /// Shared secret (>= 32 chars). Used to authenticate and encrypt all fleet traffic.
    pub secret: String,
    /// Router: address nodes connect to. Node: router address to connect to.
    pub router_address: String,
    pub node_id: u64,
    /// log2 of the cell size in chunks (5 = 32×32 chunks = one region).
    pub cell_shift: u32,
    pub imbalance_threshold: f64,
    pub heartbeat_timeout_ms: u64,
    pub rebalance_interval_ms: u64,
}

impl Default for FleetCfg {
    fn default() -> Self {
        Self {
            mode: FleetMode::Standalone,
            secret: String::new(),
            router_address: "127.0.0.1:25600".into(),
            node_id: 1,
            cell_shift: 5,
            imbalance_threshold: 0.2,
            heartbeat_timeout_ms: 3000,
            rebalance_interval_ms: 1000,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Punishment {
    /// Only log + notify ops
    Alert,
    /// Undo the illegal action (teleport back / cancel hit) — never wrongly bans anybody
    Setback,
    Kick,
    /// Temporary ban (minutes in `ban_minutes`)
    Ban,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckCfg {
    pub enabled: bool,
    /// Violation level at which `action` is applied.
    pub threshold: f64,
    pub action: Punishment,
    /// Violation level at which the player is kicked (0 = never).
    pub kick_at: f64,
    /// Violation level at which the player is banned (0 = never).
    pub ban_at: f64,
}

impl CheckCfg {
    fn new(threshold: f64, kick_at: f64) -> Self {
        Self { enabled: true, threshold, action: Punishment::Setback, kick_at, ban_at: 0.0 }
    }
}

impl Default for CheckCfg {
    fn default() -> Self {
        Self::new(5.0, 40.0)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum XrayMode {
    Off,
    /// Hide ores that are fully enclosed (cheap, recommended)
    Hide,
    /// Additionally sprinkle fake ores into hidden stone (stronger, more bandwidth)
    Obfuscate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AntiCheatCfg {
    pub enabled: bool,
    pub notify_ops: bool,
    pub ban_minutes: u64,
    pub fly: CheckCfg,
    pub speed: CheckCfg,
    pub nofall: CheckCfg,
    pub timer: CheckCfg,
    pub reach: CheckCfg,
    pub killaura: CheckCfg,
    pub autoclicker: CheckCfg,
    pub fastbreak: CheckCfg,
    pub xray: XrayMode,
    /// Statistical x-ray detection (ore/stone ratio). Alert-only by default.
    pub xray_detection: CheckCfg,
    /// Hide players/entities behind walls from ESP cheats.
    pub entity_hider: bool,
}

impl Default for AntiCheatCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            notify_ops: true,
            ban_minutes: 60,
            fly: CheckCfg::new(5.0, 50.0),
            speed: CheckCfg::new(5.0, 50.0),
            nofall: CheckCfg::new(3.0, 30.0),
            timer: CheckCfg::new(10.0, 60.0),
            reach: CheckCfg::new(3.0, 20.0),
            killaura: CheckCfg::new(5.0, 25.0),
            autoclicker: CheckCfg::new(10.0, 40.0),
            fastbreak: CheckCfg::new(3.0, 20.0),
            xray: XrayMode::Hide,
            xray_detection: CheckCfg { enabled: true, threshold: 1.0, action: Punishment::Alert, kick_at: 0.0, ban_at: 0.0 },
            entity_hider: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BedrockAuth {
    /// Xbox Live account required
    Online,
    /// No authentication (only behind a trusted network!)
    Offline,
}

/// Built-in cross-play (Bedrock Edition). Option names follow GeyserMC where
/// an equivalent exists.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BedrockCfg {
    pub enabled: bool,
    pub bind: String,
    pub motd1: String,
    pub motd2: String,
    pub auth_type: BedrockAuth,
    /// Prefix added to Bedrock usernames to avoid clashes with Java names.
    pub username_prefix: String,
    /// Replace spaces in Bedrock gamertags.
    pub replace_spaces: bool,
    pub show_cooldown: bool,
    pub show_coordinates: bool,
    pub above_bedrock_nether_building: bool,
    pub allow_custom_skulls: bool,
    pub max_visible_custom_skulls: u32,
    pub add_non_bedrock_items: bool,
    pub xbox_achievements_enabled: bool,
    pub forward_player_ping: bool,
    /// Show Java skins on Bedrock and Bedrock skins on Java.
    pub skins: bool,
    pub mtu: u16,
    /// Bedrock protocol number advertised in the server list.
    pub protocol_version: u32,
    pub game_version: String,
}

impl Default for BedrockCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: "0.0.0.0:19132".into(),
            motd1: "RimeMC".into(),
            motd2: "Cross-play enabled".into(),
            auth_type: BedrockAuth::Online,
            username_prefix: ".".into(),
            replace_spaces: true,
            show_cooldown: true,
            show_coordinates: true,
            above_bedrock_nether_building: false,
            allow_custom_skulls: true,
            max_visible_custom_skulls: 128,
            add_non_bedrock_items: true,
            xbox_achievements_enabled: false,
            forward_player_ping: false,
            skins: true,
            mtu: 1400,
            protocol_version: 766,
            game_version: "1.21.50".into(),
        }
    }
}

impl Config {
    /// Load `path`; create it with defaults if it does not exist.
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        let cfg = if path.exists() {
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
        } else {
            let cfg = Config::default();
            std::fs::write(path, cfg.to_toml()?)?;
            cfg
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(format!("# RimeMC configuration. Delete a key to get its default back.\n\n{}", toml::to_string_pretty(self)?))
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if !(1..=22).contains(&self.world.compression_level) {
            bail!("world.compression_level must be 1..=22");
        }
        if !(2..=32).contains(&self.server.view_distance) {
            bail!("server.view_distance must be 2..=32");
        }
        if self.player_cost.min_view_distance < 2 || self.player_cost.min_view_distance > self.server.view_distance {
            bail!("player_cost.min_view_distance must be between 2 and server.view_distance");
        }
        if self.level_tick.max_interval == 0 || self.level_tick.step == 0 {
            bail!("level_tick.max_interval and level_tick.step must be >= 1");
        }
        if self.ram_world.save_interval_seconds == 0 {
            bail!("ram_world.save_interval_seconds must be >= 1");
        }
        if self.world.name.is_empty() || self.world.name.contains(['/', '\\']) || self.world.name.contains("..") {
            bail!("world.name must be a plain directory name");
        }
        if self.fleet.mode != FleetMode::Standalone {
            if self.world.format != WorldFormat::Airworld {
                bail!("RimeFleet requires world.format = \"airworld\"");
            }
            if self.fleet.secret.len() < rime_fleet::crypto::MIN_SECRET_LEN {
                bail!("fleet.secret must be at least {} characters (generate one with `rime generator-secret`)", rime_fleet::crypto::MIN_SECRET_LEN);
            }
            if self.fleet.cell_shift > 10 {
                bail!("fleet.cell_shift must be <= 10");
            }
        }
        Ok(())
    }

    pub fn worker_threads(&self) -> usize {
        if self.server.worker_threads == 0 {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
        } else {
            self.server.worker_threads
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip_and_validate() {
        let c = Config::default();
        c.validate().unwrap();
        assert_eq!(c.world.format, WorldFormat::Airworld);
        assert_eq!(c.ram_world.save_interval_seconds, 30);
        assert!(c.gpu.enabled);
        let text = c.to_toml().unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        back.validate().unwrap();
    }

    #[test]
    fn partial_config_uses_defaults() {
        let c: Config = toml::from_str("[pregen]\ncandidates = 10\n").unwrap();
        assert_eq!(c.pregen.candidates, 10);
        assert_eq!(c.server.max_players, 100);
    }

    #[test]
    fn fleet_requires_airworld_and_secret() {
        let mut c = Config::default();
        c.fleet.mode = FleetMode::Node;
        assert!(c.validate().is_err());
        c.fleet.secret = "x".repeat(40);
        c.validate().unwrap();
        c.world.format = WorldFormat::Vanilla;
        assert!(c.validate().is_err());
    }
}
