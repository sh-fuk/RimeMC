//! Mobs: every living entity type of vanilla 26.1 besides players and armor
//! stands. Types, sizes and attributes come from the generated entity type
//! table; the AI (goals, navigation, controls) lives in `ai`, the per-type
//! behaviour in `ai::mobs`, the tick in `mobtick`.

use rand::Rng;

use super::ai::control::{LookControl, MoveControl, MoveKind, Motion};
use super::ai::path::{NavKind, Navigation, PATH_TYPES};
use super::ai::{Slot, WalkKind};
use super::etypes::{EntityTypeInfo, EntityTypes};
use crate::net::codec::Out;
use crate::registry::BlockState;

/// A living, non-player entity type (`minecraft:<name>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MobType(pub &'static str);

const NOT_MOBS: [&str; 3] = ["player", "armor_stand", "mannequin"];

const ANIMALS: [&str; 34] = [
    "armadillo", "axolotl", "bee", "camel", "camel_husk", "cat", "chicken", "cow", "donkey", "fox", "frog", "goat", "hoglin", "horse", "llama", "mooshroom", "mule", "nautilus", "ocelot", "panda", "parrot", "pig", "polar_bear", "rabbit", "sheep", "skeleton_horse", "sniffer", "strider", "trader_llama", "turtle", "wolf", "zombie_horse", "zombie_nautilus", "happy_ghast",
];

const MONSTERS: [&str; 38] = [
    "blaze", "bogged", "breeze", "cave_spider", "creaking", "creeper", "drowned", "elder_guardian", "enderman", "endermite", "evoker", "giant", "guardian", "husk", "illusioner", "parched", "piglin", "piglin_brute", "pillager", "ravager", "silverfish", "skeleton", "spider", "stray", "vex", "vindicator", "warden", "witch", "wither", "wither_skeleton", "zoglin", "zombie", "zombie_villager", "zombified_piglin", "ghast", "phantom", "slime", "magma_cube",
];

impl MobType {
    pub const ZOMBIE: MobType = MobType("zombie");
    pub const CHICKEN: MobType = MobType("chicken");

    pub fn info(self) -> &'static EntityTypeInfo {
        EntityTypes::get().by_name(self.0).expect("mob type in table")
    }

    pub fn from_name(n: &str) -> Option<MobType> {
        let n = n.trim_start_matches("minecraft:");
        if NOT_MOBS.contains(&n) {
            return None;
        }
        let t = EntityTypes::get().by_name(n)?;
        if t.attrs.is_empty() {
            return None;
        }
        Some(MobType(t.name))
    }

    pub fn all() -> Vec<MobType> {
        EntityTypes::get().list.iter().filter(|t| !t.attrs.is_empty() && !NOT_MOBS.contains(&t.name)).map(|t| MobType(t.name)).collect()
    }

    pub fn name(self) -> &'static str {
        self.0
    }

    pub fn is(self, n: &str) -> bool {
        self.0 == n
    }

    /// name in death messages
    pub fn display(self) -> String {
        self.0.split('_').map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }).collect::<Vec<_>>().join(" ")
    }

    pub fn category(self) -> &'static str {
        self.info().category
    }

    /// `Enemy` (counts against the monster cap, hostile sound source)
    pub fn hostile(self) -> bool {
        self.category() == "monster"
    }

    pub fn is_animal(self) -> bool {
        ANIMALS.contains(&self.0)
    }

    pub fn is_monster_class(self) -> bool {
        MONSTERS.contains(&self.0)
    }

    /// `AgeableMob` (can be a baby with an age counter)
    pub fn ageable(self) -> bool {
        self.is_animal() || matches!(self.0, "villager" | "wandering_trader" | "dolphin" | "squid" | "glow_squid")
    }

    /// has a baby form at all (zombies, piglins and zoglins too)
    pub fn can_be_baby(self) -> bool {
        self.ageable() || matches!(self.0, "zombie" | "husk" | "drowned" | "zombie_villager" | "zombified_piglin" | "piglin" | "zoglin")
    }

    pub fn is_raider(self) -> bool {
        matches!(self.0, "pillager" | "vindicator" | "evoker" | "illusioner" | "ravager" | "witch")
    }

    pub fn is_undead(self) -> bool {
        matches!(self.0, "skeleton" | "stray" | "wither_skeleton" | "skeleton_horse" | "bogged" | "parched" | "zombie_horse" | "camel_husk" | "zombie" | "zombie_villager" | "zombified_piglin" | "zoglin" | "drowned" | "husk" | "zombie_nautilus" | "wither" | "phantom")
    }

    pub fn is_zombie(self) -> bool {
        matches!(self.0, "zombie" | "husk" | "drowned" | "zombie_villager")
    }

    pub fn is_skeleton(self) -> bool {
        matches!(self.0, "skeleton" | "stray" | "wither_skeleton" | "bogged" | "parched")
    }

    pub fn is_fish(self) -> bool {
        matches!(self.0, "cod" | "salmon" | "pufferfish" | "tropical_fish")
    }

    pub fn is_water_animal(self) -> bool {
        self.is_fish() || matches!(self.0, "squid" | "glow_squid" | "dolphin" | "tadpole")
    }

    pub fn is_horse(self) -> bool {
        matches!(self.0, "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" | "llama" | "trader_llama" | "camel" | "camel_husk")
    }

    pub fn tamable(self) -> bool {
        matches!(self.0, "wolf" | "cat" | "parrot")
    }

    /// (width, height) at scale 1
    pub fn size(self) -> (f64, f64) {
        let i = self.info();
        (i.width, i.height)
    }

    pub fn eye_height(self) -> f64 {
        self.info().eye_height
    }

    pub fn fire_immune(self) -> bool {
        self.info().fire_immune
    }

    /// `getDimensions` scale for the current state.
    pub fn scale_of(self, m: &Mob) -> f64 {
        match self.0 {
            "slime" | "magma_cube" => m.size.max(1) as f64,
            "phantom" => 1.0 + 0.15 * m.size as f64,
            "pufferfish" => [0.5, 0.7, 1.0][m.size.clamp(0, 2) as usize],
            "turtle" if m.is_baby() => 0.3,
            _ if m.is_baby() => 0.5,
            _ => 1.0,
        }
    }

    pub fn size_of(self, m: &Mob) -> (f64, f64) {
        let (w, h) = self.size();
        let s = self.scale_of(m);
        if self.0 == "shulker" {
            // the box grows upwards while peeking
            let peek = m.v.peek as f64 / 100.0;
            let open = 0.5 - (0.5 + peek * std::f64::consts::PI).sin() * 0.5;
            return (1.0, 1.0 + open);
        }
        if m.is_sitting() && matches!(self.0, "wolf" | "cat") {
            return (w * s, h * s);
        }
        (w * s, h * s)
    }

    /// `Mob.getHeadRotSpeed`
    pub fn head_rot_speed(self) -> f32 {
        10.0
    }

    /// `Mob.getMaxHeadXRot`
    pub fn max_head_x(self) -> f32 {
        match self.0 {
            "dolphin" | "squid" | "glow_squid" | "tadpole" => 1.0,
            "turtle" | "axolotl" | "frog" => 20.0,
            "llama" | "trader_llama" | "horse" | "donkey" | "mule" | "camel" | "camel_husk" => 30.0,
            "panda" | "sniffer" => 10.0,
            _ => 40.0,
        }
    }

    /// `Mob.getMaxHeadYRot`
    pub fn max_head_y(self) -> f32 {
        match self.0 {
            "dolphin" | "squid" | "glow_squid" | "frog" => 1.0,
            "camel" | "camel_husk" => 30.0,
            "axolotl" | "turtle" | "panda" | "sniffer" => 20.0,
            _ => 75.0,
        }
    }

    pub fn walk_kind(self) -> WalkKind {
        match self.0 {
            "silverfish" => WalkKind::Custom(super::ai::mobs::silverfish_walk),
            "guardian" | "elder_guardian" => WalkKind::Custom(super::ai::mobs::guardian_walk),
            "turtle" => WalkKind::Custom(super::ai::mobs::turtle_walk),
            "strider" => WalkKind::Custom(super::ai::mobs::strider_walk),
            "piglin" | "piglin_brute" | "hoglin" | "zoglin" | "zombified_piglin" | "blaze" | "wither_skeleton" | "magma_cube" | "ghast" | "endermite" | "breeze" | "creaking" | "warden" => WalkKind::None,
            "axolotl" | "frog" | "bee" | "parrot" | "allay" => WalkKind::None,
            _ if self.is_animal() => WalkKind::Animal,
            _ if self.is_monster_class() => WalkKind::Monster,
            _ => WalkKind::None,
        }
    }

    pub fn nav_kind(self) -> NavKind {
        match self.0 {
            "spider" | "cave_spider" => NavKind::Climber,
            "bee" | "parrot" | "allay" | "vex" | "wither" | "ghast" | "happy_ghast" | "bat" | "phantom" | "ender_dragon" => NavKind::Flying,
            "cod" | "salmon" | "pufferfish" | "tropical_fish" | "squid" | "glow_squid" | "dolphin" | "guardian" | "elder_guardian" | "tadpole" | "nautilus" | "zombie_nautilus" => NavKind::Water,
            "axolotl" | "frog" | "turtle" | "drowned" => NavKind::Amphibious,
            _ => NavKind::Ground,
        }
    }

    pub fn move_kind(self) -> MoveKind {
        match self.0 {
            "bee" => MoveKind::Flying { max_turn: 20.0, hover: true },
            "parrot" | "allay" | "breeze" => MoveKind::Flying { max_turn: 10.0, hover: false },
            "dolphin" => MoveKind::SmoothSwim { max_x: 85.0, max_y: 10.0, in_water: 0.02, outside: 0.1, gravity: true },
            "axolotl" => MoveKind::SmoothSwim { max_x: 85.0, max_y: 10.0, in_water: 0.1, outside: 0.5, gravity: false },
            "frog" => MoveKind::SmoothSwim { max_x: 85.0, max_y: 10.0, in_water: 0.02, outside: 0.1, gravity: true },
            "nautilus" | "zombie_nautilus" => MoveKind::SmoothSwim { max_x: 85.0, max_y: 10.0, in_water: 0.02, outside: 0.1, gravity: true },
            "cod" | "salmon" | "pufferfish" | "tropical_fish" | "tadpole" => MoveKind::Fish,
            "wither" => MoveKind::Flying { max_turn: 10.0, hover: false },
            "slime" | "magma_cube" | "ghast" | "happy_ghast" | "phantom" | "vex" | "bat" | "squid" | "glow_squid" | "shulker" | "ender_dragon" | "guardian" | "elder_guardian" | "turtle" | "drowned" => MoveKind::Custom,
            _ => MoveKind::Default,
        }
    }

    /// sounds use `entity.<sound_name>.*`
    pub fn sound_name(self) -> &'static str {
        match self.0 {
            "trader_llama" => "llama",
            "giant" => "zombie",
            "illusioner" => "illusioner",
            "tropical_fish" => "tropical_fish",
            "mooshroom" => "cow",
            "camel_husk" => "camel_husk",
            n => n,
        }
    }

    pub fn ambient_sound(self) -> Option<String> {
        let silent = ["creeper", "cod", "salmon", "pufferfish", "tropical_fish", "slime", "magma_cube", "iron_golem", "snow_golem", "shulker", "giant", "tadpole", "sniffer", "copper_golem", "creaking"];
        if silent.contains(&self.0) {
            return None;
        }
        let n = self.sound_name();
        Some(format!("minecraft:entity.{n}.ambient"))
    }

    pub fn hurt_sound(self) -> String {
        format!("minecraft:entity.{}.hurt", self.sound_name())
    }

    pub fn death_sound(self) -> String {
        format!("minecraft:entity.{}.death", self.sound_name())
    }

    /// `Mob.getAmbientSoundInterval`
    pub fn ambient_interval(self) -> i32 {
        match self.0 {
            "bee" | "nautilus" | "zombie_nautilus" => 400,
            "dolphin" | "squid" | "glow_squid" | "cod" | "salmon" | "pufferfish" | "tropical_fish" => 120,
            "wolf" | "cat" | "ocelot" => 120,
            "phantom" | "vex" => 80,
            _ => 80,
        }
    }

    pub fn loot_table(self) -> String {
        format!("entities/{}", self.0)
    }

    /// `Mob.getBaseExperienceReward` (before equipment bonuses)
    pub fn xp(self) -> i32 {
        match self.0 {
            "ender_dragon" => 12000,
            "wither" => 50,
            "warden" => 5,
            "elder_guardian" => 10,
            "blaze" | "guardian" | "evoker" | "piglin_brute" | "ravager" => 10,
            "endermite" | "silverfish" | "vex" | "creaking" => 3,
            "breeze" => 10,
            "slime" | "magma_cube" => 0,
            "bat" | "snow_golem" | "iron_golem" | "copper_golem" | "villager" | "wandering_trader" | "allay" => 0,
            _ if self.is_animal() || self.is_water_animal() => 1,
            _ if self.hostile() => 5,
            _ => 0,
        }
    }

    /// Ignores fall damage (`fall_damage_immune` entity tag).
    pub fn fall_immune(self) -> bool {
        matches!(self.0, "copper_golem" | "iron_golem" | "snow_golem" | "shulker" | "allay" | "bat" | "bee" | "blaze" | "cat" | "chicken" | "ghast" | "happy_ghast" | "phantom" | "magma_cube" | "ocelot" | "parrot" | "wither" | "breeze")
    }

    /// Breathes water only.
    pub fn water_breather(self) -> bool {
        self.is_water_animal() || matches!(self.0, "nautilus" | "zombie_nautilus")
    }

    /// Breathes in water as well as air (`canBreatheUnderwater`).
    pub fn breathes_underwater(self) -> bool {
        self.is_undead() || matches!(self.0, "axolotl" | "frog" | "guardian" | "elder_guardian" | "turtle" | "glow_squid" | "cod" | "pufferfish" | "salmon" | "squid" | "tropical_fish" | "tadpole" | "copper_golem" | "nautilus")
    }

    /// Takes damage from water (`isSensitiveToWater`).
    pub fn water_sensitive(self) -> bool {
        matches!(self.0, "enderman" | "blaze" | "snow_golem" | "strider")
    }
}

/// Per-type state that does not fit the shared fields.
#[derive(Clone, Debug, Default)]
pub struct Vars {
    /// name tag
    pub custom_name: Option<String>,
    /// piglin admiring the gold in its off hand (ticks left)
    pub admire: i32,
    /// ocelot trust
    pub trusting: bool,
    /// wolf / cat collar colour
    pub collar: u8,
    // creeper
    pub swell: i32,
    pub old_swell: i32,
    pub swell_dir: i32,
    pub powered: bool,
    pub ignited: bool,
    // sheep
    pub wool: u8,
    pub sheared: bool,
    pub eat_tick: i32,
    // chicken / turtle / sniffer
    pub egg_time: i32,
    pub chicken_jockey: bool,
    // slime / rabbit / frog jumps
    pub jump_delay: i32,
    pub squish: f32,
    // enderman
    pub carry: Option<BlockState>,
    pub creepy: bool,
    pub stared: bool,
    pub last_stare: i32,
    pub tp_cooldown: i32,
    // angerable mobs (wolf, bee, enderman, zombified piglin, polar bear, iron golem, piglin)
    pub anger_end: i64,
    pub anger_target: Option<uuid::Uuid>,
    pub interested: bool,
    // spellcasters
    pub spell: u8,
    pub spell_ticks: i32,
    pub spell_cooldown: i32,
    // crossbow users
    pub charging: bool,
    // witch / items
    pub using_ticks: i32,
    pub drink: Option<String>,
    // ghast / blaze / guardian / ranged
    pub charge: i32,
    pub flags: u8,
    pub attack_step: i32,
    pub attack_time: i32,
    pub beam_target: i32,
    pub beam_ticks: i32,
    // ravager / iron golem / hoglin / zoglin / warden
    pub attack_anim: i32,
    pub stun: i32,
    pub roar: i32,
    pub offer_flower: i32,
    // phantom
    pub anchor: [i32; 3],
    pub move_target: [f64; 3],
    pub phase: u8,
    // vex / dragon
    pub life: i32,
    pub bound: Option<[i32; 3]>,
    /// dragon: the crystal healing it, damage taken while perched, time in phase
    pub dragon_crystal: Option<i32>,
    /// pig / strider: total ticks of the last carrot / fungus boost
    pub boost: i32,
    pub sit_damage: f32,
    pub phase_time: i32,
    // shulker
    pub peek: i32,
    pub attach_face: u8,
    pub color: u8,
    // fish / dolphin / axolotl
    pub from_bucket: bool,
    pub moistness: i32,
    pub got_fish: bool,
    pub puff: i32,
    pub inflate_ticks: i32,
    pub deflate_timer: i32,
    // bat
    pub resting: bool,
    pub target_pos: Option<[i32; 3]>,
    // turtle
    pub home_pos: Option<[i32; 3]>,
    pub has_egg: bool,
    pub laying: i32,
    pub going_home: bool,
    // bee
    pub has_nectar: bool,
    pub has_stung: bool,
    pub hive: Option<[i32; 3]>,
    pub flower: Option<[i32; 3]>,
    // villager / trader
    pub trade_unhappy: i32,
    pub despawn_delay: i32,
    // horses
    pub temper: i32,
    pub eating: i32,
    pub standing: i32,
    pub chested: bool,
    pub saddled: bool,
    // mooshroom / fox / rabbit / axolotl / misc variant
    pub variant: i32,
    pub variant2: i32,
    // panda genes
    pub genes: (u8, u8),
    pub panda_flags: u8,
    pub sneeze: i32,
    pub unhappy: i32,
    pub eat: i32,
    // goat
    pub horns: (bool, bool),
    pub screaming: bool,
    pub ram_cooldown: i32,
    // fox / polar bear / iron golem / snow golem
    pub fox_flags: u8,
    pub pumpkin: bool,
    pub player_made: bool,
    // zombie conversion / drowning
    pub in_water_time: i32,
    pub conversion: i32,
    pub special: i32,
    pub can_break_doors: bool,
    // strider
    pub suffocating: bool,
    // wither
    pub invul: i32,
    pub heads: [i32; 3],
    pub head_cd: [i32; 2],
    pub block_break: i32,
    // warden
    pub anger: i32,
    pub sniff: i32,
    pub dig: i32,
    pub sonic: i32,
    // creaking
    pub home_heart: Option<[i32; 3]>,
    pub active: bool,
    pub can_move: bool,
    // breeze
    pub jump_cd: i32,
    pub shoot_cd: i32,
    // misc timers
    pub t0: i32,
    pub t1: i32,
    pub t2: i32,
    pub p0: Option<[f64; 3]>,
    pub e0: Option<i32>,
    // custom move controls
    pub yrot_w: f32,
    pub slime_aggr: bool,
    pub was_ground: bool,
    pub float_dur: i32,
    pub fly_speed: f32,
    pub tent: f32,
    pub tent_speed: f32,
    pub rot_speed: f32,
    pub height_offset: f32,
    pub height_change: i32,
    pub yrot_a: f32,
    pub idle_heads: [i32; 2],
    pub next_head: [i32; 2],
    pub alt_targets: [i32; 3],
    pub destroy_blocks: i32,
    pub overworld_time: i32,
    pub target_change: i32,
    pub climbing: bool,
}

/// Runtime state of one mob.
#[derive(Clone, Debug)]
pub struct Mob {
    pub ty: MobType,
    /// health, attributes, effects, invulnerability (vanilla `LivingEntity`)
    pub living: crate::game::combat::living::Living,
    pub eq: crate::game::combat::living::Equipment,
    pub mo: Motion,
    pub nav: Navigation,
    pub mc: MoveControl,
    pub look: LookControl,
    pub malus: [f32; PATH_TYPES],
    pub max_up_step: f32,
    pub goals: Vec<Slot>,
    pub targets: Vec<Slot>,
    pub target: Option<i32>,
    pub aggressive: bool,
    /// remaining burn ticks
    pub fire: i32,
    /// ticks since death (0 = alive)
    pub death: u32,
    pub persistent: bool,
    pub ambient: i32,
    pub no_action: i32,
    /// `AgeableMob.age`: < 0 baby, > 0 breeding cooldown
    pub age: i32,
    /// zombie / piglin / zoglin babies (no age counter)
    pub baby: bool,
    pub love: i32,
    pub love_cause: Option<i32>,
    pub home: Option<([i32; 3], i32)>,
    pub owner: Option<uuid::Uuid>,
    pub tame: bool,
    pub sitting: bool,
    pub ordered_sit: bool,
    pub size: i32,
    pub air: i32,
    pub frozen: i32,
    pub swing: bool,
    pub event: Vec<u8>,
    pub tick_count: i32,
    pub no_ai: bool,
    pub left_handed: bool,
    pub can_pick_up: bool,
    pub drop_chance: [f32; 6],
    pub head_yaw_sent: f32,
    pub rot_sent: (f32, f32),
    pub meta_sent: u64,
    pub v: Vars,
    /// villagers, zombie villagers and wandering traders
    pub merchant: Option<Box<super::merchant::Merchant>>,
}

impl Mob {
    pub fn new(ty: MobType, persistent: bool) -> Mob {
        let attrs = EntityTypes::get().by_name(ty.name()).map(|t| t.attributes()).unwrap_or_else(|| crate::game::combat::attr::Map::new(&crate::game::combat::attr::living()));
        let mut living = crate::game::combat::living::Living::new(attrs);
        let step = living.attrs.value(crate::game::combat::attr::Attr::StepHeight) as f32;
        let mut m = Mob {
            ty,
            living,
            eq: Default::default(),
            mo: Motion::default(),
            nav: Navigation::new(ty.nav_kind()),
            mc: MoveControl::new(ty.move_kind()),
            look: LookControl::new(),
            malus: [f32::NAN; PATH_TYPES],
            max_up_step: step,
            goals: Vec::new(),
            targets: Vec::new(),
            target: None,
            aggressive: false,
            fire: 0,
            death: 0,
            persistent,
            ambient: 0,
            no_action: 0,
            age: 0,
            baby: false,
            love: 0,
            love_cause: None,
            home: None,
            owner: None,
            tame: false,
            sitting: false,
            ordered_sit: false,
            size: 1,
            air: 300,
            frozen: 0,
            swing: false,
            event: Vec::new(),
            tick_count: 0,
            no_ai: false,
            left_handed: false,
            can_pick_up: false,
            drop_chance: [0.085; 6],
            head_yaw_sent: 0.0,
            rot_sent: (0.0, 0.0),
            meta_sent: 0,
            v: Vars { swell_dir: -1, can_move: true, moistness: 2400, collar: 14, ..Default::default() },
            merchant: None,
        };
        if ty.water_breather() {
            m.air = 300;
        }
        super::ai::mobs::setup(&mut m);
        m
    }

    pub fn is_baby(&self) -> bool {
        if self.ty.ageable() { self.age < 0 } else { self.baby }
    }

    pub fn is_sitting(&self) -> bool {
        self.sitting
    }

    pub fn in_love(&self) -> bool {
        self.love > 0
    }

    pub fn angry(&self, now: i64) -> bool {
        self.v.anger_end > now
    }

    pub fn set_baby(&mut self, b: bool) {
        if self.ty.ageable() {
            self.age = if b { -24000 } else { 0 };
        } else {
            self.baby = b;
        }
        super::ai::mobs::on_age_change(self);
    }

    pub fn max_health(&mut self) -> f32 {
        self.living.max_health()
    }

    /// `Mob.setTarget` + `setAggressive` bookkeeping.
    pub fn set_target(&mut self, t: Option<i32>) {
        self.target = t;
    }
}

/// `Sheep.getRandomSheepColor`
pub fn random_sheep_color(rng: &mut impl Rng) -> u8 {
    let i = rng.gen_range(0..100);
    if i < 5 {
        15 // black
    } else if i < 10 {
        7 // gray
    } else if i < 15 {
        8 // light gray
    } else if i < 18 {
        12 // brown
    } else if rng.gen_range(0..500) == 0 {
        6 // pink
    } else {
        0
    }
}

/// Melee damage of a held item (`attack_damage` incl. the base 1).
pub fn item_attack_damage(item: &str) -> f32 {
    let tier = |n: &str| -> Option<usize> {
        ["wooden_", "golden_", "stone_", "copper_", "iron_", "diamond_", "netherite_"].iter().position(|t| n.starts_with(t))
    };
    if item.ends_with("_sword") {
        // wood, gold, stone, copper, iron, diamond, netherite
        return [4.0, 4.0, 5.0, 5.0, 6.0, 7.0, 8.0][tier(item).unwrap_or(0)];
    }
    if item.ends_with("_axe") {
        return [7.0, 7.0, 9.0, 9.0, 9.0, 9.0, 10.0][tier(item).unwrap_or(0)];
    }
    if item.ends_with("_pickaxe") {
        return [2.0, 2.0, 3.0, 3.0, 4.0, 5.0, 6.0][tier(item).unwrap_or(0)];
    }
    if item.ends_with("_shovel") {
        return [2.5, 2.5, 3.5, 3.5, 4.5, 5.5, 6.5][tier(item).unwrap_or(0)];
    }
    if item == "trident" {
        return 9.0;
    }
    if item == "mace" {
        return 6.0;
    }
    1.0
}

/// Armour points and toughness of a worn piece.
pub fn armor_of(item: &str) -> (f32, f32) {
    let slot = if item.ends_with("_helmet") {
        0
    } else if item.ends_with("_chestplate") {
        1
    } else if item.ends_with("_leggings") {
        2
    } else if item.ends_with("_boots") {
        3
    } else if item == "turtle_helmet" {
        return (2.0, 0.0);
    } else {
        return (0.0, 0.0);
    };
    let (pts, tough): ([f32; 4], f32) = if item.starts_with("leather_") {
        ([1.0, 3.0, 2.0, 1.0], 0.0)
    } else if item.starts_with("golden_") {
        ([2.0, 5.0, 3.0, 1.0], 0.0)
    } else if item.starts_with("chainmail_") {
        ([2.0, 5.0, 4.0, 1.0], 0.0)
    } else if item.starts_with("copper_") {
        ([2.0, 4.0, 3.0, 1.0], 0.0)
    } else if item.starts_with("iron_") {
        ([2.0, 6.0, 5.0, 2.0], 0.0)
    } else if item.starts_with("diamond_") {
        ([3.0, 8.0, 6.0, 3.0], 2.0)
    } else if item.starts_with("netherite_") {
        ([3.0, 8.0, 6.0, 3.0], 3.0)
    } else {
        ([0.0; 4], 0.0)
    };
    (pts[slot], tough)
}

/// `CombatRules.getDamageAfterAbsorb`
pub fn after_armor(damage: f32, armor: f32, toughness: f32) -> f32 {
    let t = 2.0 + toughness / 4.0;
    let r = (armor - damage / t).clamp(armor * 0.2, 20.0);
    damage * (1.0 - r / 25.0)
}

/// Vanilla's low-precision velocity encoding (`LpVec3`).
pub fn write_lpvec(b: &mut Vec<u8>, v: [f64; 3]) {
    let san = |x: f64| if x.is_nan() { 0.0 } else { x.clamp(-1.7179869183e10, 1.7179869183e10) };
    let (x, y, z) = (san(v[0]), san(v[1]), san(v[2]));
    let max = x.abs().max(y.abs()).max(z.abs());
    if max < 3.051944088384301e-5 {
        b.u8(0);
        return;
    }
    let scale = max.ceil() as i64;
    let extended = (scale & 3) != scale;
    let markers = if extended { (scale & 3) | 4 } else { scale };
    let pack = |v: f64| ((v * 0.5 + 0.5) * 32766.0).round() as i64;
    let s = scale as f64;
    let buf = markers | (pack(x / s) << 3) | (pack(y / s) << 18) | (pack(z / s) << 33);
    b.u8(buf as u8).u8((buf >> 8) as u8).i32((buf >> 16) as i32);
    if extended {
        b.vi((scale >> 2) as i32);
    }
}

/// Angle in 1/256 turns, as entity packets send it.
pub fn angle_byte(deg: f32) -> u8 {
    ((deg / 360.0 * 256.0).floor() as i32 & 0xFF) as u8
}

/// Blast resistance (`explosion_resistance`) of common blocks.
pub fn blast_resistance(b: BlockState) -> f32 {
    let n = b.name();
    if b.is_air() {
        return 0.0;
    }
    match n {
        "bedrock" | "barrier" | "command_block" | "chain_command_block" | "repeating_command_block" | "structure_block" | "jigsaw" | "end_portal" | "end_portal_frame" | "end_gateway" | "light" | "moving_piston" => 3_600_000.0,
        "obsidian" | "crying_obsidian" | "reinforced_deepslate" | "ancient_debris" | "netherite_block" | "respawn_anchor" | "enchanting_table" | "anvil" | "chipped_anvil" | "damaged_anvil" => 1200.0,
        "ender_chest" => 600.0,
        "water" | "lava" => 100.0,
        "end_stone" | "end_stone_bricks" => 9.0,
        "iron_bars" | "iron_door" | "iron_trapdoor" | "iron_block" | "diamond_block" | "emerald_block" | "gold_block" | "copper_block" | "spawner" | "bell" => 6.0,
        "grass_block" | "dirt" | "coarse_dirt" | "rooted_dirt" | "podzol" | "mycelium" | "farmland" | "dirt_path" | "sand" | "red_sand" | "clay" | "soul_sand" | "soul_soil" | "mud" => 0.5,
        "gravel" => 0.6,
        "netherrack" => 0.4,
        "glass" | "glass_pane" | "glowstone" | "sea_lantern" | "beacon" => 0.3,
        "ice" | "packed_ice" => 0.5,
        "snow" | "snow_block" => 0.1,
        _ if n.ends_with("_leaves") => 0.2,
        _ if n.ends_with("_wool") || n.ends_with("_carpet") => 0.8,
        _ if n.ends_with("_glass") || n.ends_with("_glass_pane") => 0.3,
        _ if n.ends_with("_ore") => 3.0,
        _ if n.contains("deepslate") => 6.0,
        _ if n.ends_with("_log") || n.ends_with("_wood") || n.ends_with("_stem") || n.ends_with("_hyphae") => 2.0,
        _ if n.ends_with("_planks") || n.contains("_fence") || n.ends_with("_door") || n.ends_with("_trapdoor") || n == "chest" || n == "barrel" || n == "crafting_table" => 3.0,
        _ if n.contains("stone") || n.contains("brick") || n.contains("cobble") || n.contains("andesite") || n.contains("diorite") || n.contains("granite") || n.contains("tuff") || n.contains("basalt") || n.contains("blackstone") || n.contains("terracotta") || n.ends_with("_concrete") => 6.0,
        _ if n.ends_with("_concrete_powder") => 0.5,
        _ if !b.is_solid() => 0.0,
        _ => 3.0,
    }
}

/// Blocks an explosion of `power` at `c` destroys (vanilla's 16³ ray grid).
pub fn explosion_blocks(c: [f64; 3], power: f32, block: &dyn Fn(i32, i32, i32) -> Option<BlockState>, rng: &mut impl Rng) -> Vec<[i32; 3]> {
    let mut out = std::collections::HashSet::new();
    for x in 0..16 {
        for y in 0..16 {
            for z in 0..16 {
                if !(x == 0 || x == 15 || y == 0 || y == 15 || z == 0 || z == 15) {
                    continue;
                }
                let mut d = [x as f64 / 15.0 * 2.0 - 1.0, y as f64 / 15.0 * 2.0 - 1.0, z as f64 / 15.0 * 2.0 - 1.0];
                let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                for v in &mut d {
                    *v /= len;
                }
                let mut rem = power * (0.7 + rng.r#gen::<f32>() * 0.6);
                let mut p = c;
                while rem > 0.0 {
                    let bp = [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32];
                    let Some(b) = block(bp[0], bp[1], bp[2]) else { break };
                    if !b.is_air() {
                        rem -= (blast_resistance(b) + 0.3) * 0.3;
                    }
                    if rem > 0.0 && !b.is_air() {
                        out.insert(bp);
                    }
                    for i in 0..3 {
                        p[i] += d[i] * 0.3;
                    }
                    rem -= 0.225;
                }
            }
        }
    }
    out.into_iter().collect()
}

/// Damage an explosion deals at distance `dist` (exposure taken as 1).
pub fn explosion_damage(power: f32, dist: f64) -> f32 {
    let r = power as f64 * 2.0;
    if dist >= r {
        return 0.0;
    }
    let impact = 1.0 - dist / r;
    (((impact * impact + impact) / 2.0 * 7.0 * r + 1.0) as f32).floor()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lpvec_small_and_zero() {
        let mut b = Vec::new();
        write_lpvec(&mut b, [0.0, 0.0, 0.0]);
        assert_eq!(b, vec![0]);
        let mut b = Vec::new();
        write_lpvec(&mut b, [0.5, -0.25, 0.0]);
        assert_eq!(b.len(), 6);
        // markers = scale 1
        assert_eq!(b[0] & 7, 1);
        let mut b = Vec::new();
        write_lpvec(&mut b, [9.0, 0.0, 0.0]);
        assert_eq!(b[0] & 4, 4, "extended scale");
        assert_eq!(b.len(), 7);
    }

    #[test]
    fn armor_reduces_damage() {
        assert_eq!(after_armor(10.0, 0.0, 0.0), 10.0);
        let full_diamond = 3.0 + 8.0 + 6.0 + 3.0;
        assert!(after_armor(10.0, full_diamond, 8.0) < 4.0);
        assert_eq!(item_attack_damage("diamond_sword"), 7.0);
        assert_eq!(item_attack_damage("stick"), 1.0);
    }

    #[test]
    fn creeper_blast_damage() {
        // point blank creeper (power 3): vanilla deals 43 on normal at exposure 1
        assert_eq!(explosion_damage(3.0, 0.0), 43.0);
        assert_eq!(explosion_damage(3.0, 6.5), 0.0);
    }

    #[test]
    fn names_round_trip() {
        let all = MobType::all();
        assert_eq!(all.len(), 89, "{} mob types", all.len());
        assert!(MobType::from_name("player").is_none());
        for m in all {
            assert_eq!(MobType::from_name(m.name()), Some(m));
            assert_eq!(MobType::from_name(&format!("minecraft:{}", m.name())), Some(m));
        }
    }
}

#[cfg(test)]
mod loot_tests {
    use super::*;

    /// Needs the vanilla data cache (`cache/26.1`) of a server run.
    #[test]
    fn mob_loot_tables_roll() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !root.join("cache/26.1/data").exists() {
            return;
        }
        let v = crate::vanilla::VanillaData::ensure(&root).unwrap();
        let data = crate::data::GameData::load(&v).unwrap();
        let mut rng = rand::thread_rng();
        for ty in MobType::all() {
            let mut total = 0usize;
            for _ in 0..50 {
                total += data.roll_loot(&ty.loot_table(), &mut rng).iter().map(|s| s.count as usize).sum::<usize>();
            }
            eprintln!("{}: {total} items in 50 kills", ty.name());
            if matches!(ty.name(), "zombie" | "skeleton" | "cow" | "chicken" | "spider" | "creeper" | "pig") {
                assert!(total > 0, "{} drops nothing", ty.name());
            }
        }
    }
}
