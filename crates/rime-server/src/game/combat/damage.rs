//! Damage types (`data/minecraft/damage_type/*.json` + `#damage_type`
//! tags from the vanilla data) and `DamageSource`.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde_json::Value;

use crate::vanilla::VanillaData;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scaling {
    Never,
    WhenCausedByLivingNonPlayer,
    Always,
}

#[derive(Clone, Debug)]
pub struct DamageType {
    pub name: String,
    pub message_id: String,
    pub scaling: Scaling,
    pub exhaustion: f32,
    /// `effects`: hurt / thorns / drowning / burning / poking / freezing
    pub effects: String,
    /// `death_message_type`: default / fall_variants / intentional_game_design
    pub death_message: String,
}

#[derive(Default)]
pub struct DamageTypes {
    pub types: HashMap<String, DamageType>,
    /// tag name (no namespace) -> damage type names (no namespace)
    pub tags: HashMap<String, HashSet<String>>,
}

static TYPES: OnceLock<DamageTypes> = OnceLock::new();

impl DamageTypes {
    pub fn load(v: &VanillaData) -> anyhow::Result<()> {
        let mut types = HashMap::new();
        for (key, path) in v.entries("damage_type") {
            let j: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            let name = key.trim_start_matches("minecraft:").to_string();
            let scaling = match j["scaling"].as_str().unwrap_or("never") {
                "always" => Scaling::Always,
                "when_caused_by_living_non_player" => Scaling::WhenCausedByLivingNonPlayer,
                _ => Scaling::Never,
            };
            types.insert(
                name.clone(),
                DamageType {
                    name,
                    message_id: j["message_id"].as_str().unwrap_or("generic").to_string(),
                    scaling,
                    exhaustion: j["exhaustion"].as_f64().unwrap_or(0.0) as f32,
                    effects: j["effects"].as_str().unwrap_or("hurt").to_string(),
                    death_message: j["death_message_type"].as_str().unwrap_or("default").to_string(),
                },
            );
        }
        let tags = crate::data::load_tags(v, "damage_type")?;
        let _ = TYPES.set(DamageTypes { types, tags });
        Ok(())
    }

    pub fn get() -> &'static DamageTypes {
        TYPES.get_or_init(DamageTypes::builtin)
    }

    /// Used by unit tests (no vanilla data): the handful of types and tags
    /// the combat code relies on, as in vanilla 26.1.
    fn builtin() -> DamageTypes {
        let mut d = DamageTypes::default();
        for (n, scaling, ex) in [
            ("generic", Scaling::WhenCausedByLivingNonPlayer, 0.0),
            ("player_attack", Scaling::WhenCausedByLivingNonPlayer, 0.1),
            ("mob_attack", Scaling::WhenCausedByLivingNonPlayer, 0.1),
            ("fall", Scaling::WhenCausedByLivingNonPlayer, 0.0),
            ("lava", Scaling::WhenCausedByLivingNonPlayer, 0.1),
            ("in_fire", Scaling::WhenCausedByLivingNonPlayer, 0.1),
            ("on_fire", Scaling::WhenCausedByLivingNonPlayer, 0.0),
            ("out_of_world", Scaling::WhenCausedByLivingNonPlayer, 0.0),
            ("arrow", Scaling::WhenCausedByLivingNonPlayer, 0.1),
            ("explosion", Scaling::Always, 0.1),
            ("starve", Scaling::WhenCausedByLivingNonPlayer, 0.0),
            ("drown", Scaling::WhenCausedByLivingNonPlayer, 0.0),
        ] {
            d.types.insert(n.into(), DamageType { name: n.into(), message_id: n.into(), scaling, exhaustion: ex, effects: "hurt".into(), death_message: "default".into() });
        }
        let tag = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        d.tags.insert("bypasses_armor".into(), tag(&["fall", "on_fire", "out_of_world", "starve", "drown"]));
        d.tags.insert("bypasses_invulnerability".into(), tag(&["out_of_world"]));
        d.tags.insert("is_fire".into(), tag(&["lava", "in_fire", "on_fire"]));
        d.tags.insert("is_fall".into(), tag(&["fall"]));
        d.tags.insert("is_projectile".into(), tag(&["arrow"]));
        d.tags.insert("is_explosion".into(), tag(&["explosion"]));
        d.tags.insert("no_knockback".into(), tag(&["explosion"]));
        d
    }

    pub fn is(&self, ty: &str, tag: &str) -> bool {
        self.tags.get(tag).is_some_and(|t| t.contains(ty))
    }
}

/// What dealt the damage (`DamageSource.causingEntity` / `directEntity`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntityRef {
    pub eid: i32,
    pub player: bool,
    pub living: bool,
    pub pos: [f64; 3],
    /// entity type (no namespace) and what loot conditions can see of it
    pub info: super::enchant::EntInfo,
}

impl EntityRef {
    pub fn new(eid: i32, type_name: &'static str, pos: [f64; 3], living: bool) -> Self {
        EntityRef { eid, player: type_name == "player", living, pos, info: super::enchant::EntInfo { type_name, ..Default::default() } }
    }
}

#[derive(Clone, Debug)]
pub struct DamageSource {
    pub ty: &'static str,
    pub causing: Option<EntityRef>,
    pub direct: Option<EntityRef>,
    /// explicit position (explosions, `bad_respawn_point`)
    pub position: Option<[f64; 3]>,
    /// direct entity is a projectile (`AbstractArrow` etc.)
    pub projectile: bool,
    /// arrows that pierce ignore shields
    pub pierce_level: u8,
    /// weapon used (for enchantment armor effectiveness, breach, …)
    pub weapon: Option<crate::data::Stack>,
    pub creative_player: bool,
    /// projectile motion (knockback direction of projectile hits)
    pub projectile_motion: Option<[f64; 3]>,
}

impl DamageSource {
    pub fn new(ty: &'static str) -> Self {
        Self { ty, causing: None, direct: None, position: None, projectile: false, pierce_level: 0, weapon: None, creative_player: false, projectile_motion: None }
    }
    pub fn by(ty: &'static str, e: EntityRef) -> Self {
        Self { causing: Some(e), direct: Some(e), ..Self::new(ty) }
    }
    pub fn indirect(ty: &'static str, direct: EntityRef, causing: Option<EntityRef>) -> Self {
        Self { causing, direct: Some(direct), ..Self::new(ty) }
    }
    pub fn at(ty: &'static str, pos: [f64; 3]) -> Self {
        Self { position: Some(pos), ..Self::new(ty) }
    }

    pub fn is(&self, tag: &str) -> bool {
        DamageTypes::get().is(self.ty, tag)
    }

    pub fn kind(&self) -> Option<&'static DamageType> {
        DamageTypes::get().types.get(self.ty)
    }

    /// `DamageSource.scalesWithDifficulty`
    pub fn scales_with_difficulty(&self) -> bool {
        match self.kind().map(|t| t.scaling).unwrap_or(Scaling::Never) {
            Scaling::Never => false,
            Scaling::WhenCausedByLivingNonPlayer => self.causing.is_some_and(|c| c.living && !c.player),
            Scaling::Always => true,
        }
    }

    pub fn food_exhaustion(&self) -> f32 {
        self.kind().map(|t| t.exhaustion).unwrap_or(0.0)
    }

    pub fn is_direct(&self) -> bool {
        match (self.causing, self.direct) {
            (Some(a), Some(b)) => a.eid == b.eid,
            (None, None) => true,
            _ => false,
        }
    }

    /// `DamageSource.getSourcePosition`
    pub fn source_position(&self) -> Option<[f64; 3]> {
        self.position.or(self.direct.map(|d| d.pos))
    }

    /// `DamageSource.isCreativePlayer` (set by the caller)
    pub fn is_creative_player(&self) -> bool {
        self.creative_player
    }
}
