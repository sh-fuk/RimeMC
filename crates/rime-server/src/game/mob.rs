//! Mobs: the common overworld monsters and farm animals, with vanilla's
//! attributes (health, speed, attack damage, size), simple goal-based AI
//! (wander, panic, chase + melee, skeleton archery, creeper fuse), natural
//! spawning rules (darkness for monsters, grass for animals) and explosions.

use rand::Rng;

use crate::net::codec::Out;
use crate::registry::BlockState;

/// The mob types RimeMC simulates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MobType {
    Zombie,
    Skeleton,
    Creeper,
    Spider,
    CaveSpider,
    Silverfish,
    Cow,
    Pig,
    Sheep,
    Chicken,
}

pub const ALL: [MobType; 10] = [
    MobType::Zombie,
    MobType::Skeleton,
    MobType::Creeper,
    MobType::Spider,
    MobType::CaveSpider,
    MobType::Silverfish,
    MobType::Cow,
    MobType::Pig,
    MobType::Sheep,
    MobType::Chicken,
];

impl MobType {
    pub fn from_name(n: &str) -> Option<MobType> {
        let n = n.trim_start_matches("minecraft:");
        ALL.into_iter().find(|m| m.name() == n)
    }

    /// registry name
    pub fn name(self) -> &'static str {
        match self {
            MobType::Zombie => "zombie",
            MobType::Skeleton => "skeleton",
            MobType::Creeper => "creeper",
            MobType::Spider => "spider",
            MobType::CaveSpider => "cave_spider",
            MobType::Silverfish => "silverfish",
            MobType::Cow => "cow",
            MobType::Pig => "pig",
            MobType::Sheep => "sheep",
            MobType::Chicken => "chicken",
        }
    }

    /// name in death messages
    pub fn display(self) -> &'static str {
        match self {
            MobType::Zombie => "Zombie",
            MobType::Skeleton => "Skeleton",
            MobType::Creeper => "Creeper",
            MobType::Spider => "Spider",
            MobType::CaveSpider => "Cave Spider",
            MobType::Silverfish => "Silverfish",
            MobType::Cow => "Cow",
            MobType::Pig => "Pig",
            MobType::Sheep => "Sheep",
            MobType::Chicken => "Chicken",
        }
    }

    /// `max_health` attribute
    pub fn max_health(self) -> f32 {
        match self {
            MobType::Zombie | MobType::Skeleton | MobType::Creeper => 20.0,
            MobType::Spider => 16.0,
            MobType::CaveSpider => 12.0,
            MobType::Silverfish | MobType::Sheep => 8.0,
            MobType::Cow | MobType::Pig => 10.0,
            MobType::Chicken => 4.0,
        }
    }

    /// (width, height) of the bounding box
    pub fn size(self) -> (f64, f64) {
        match self {
            MobType::Zombie => (0.6, 1.95),
            MobType::Skeleton => (0.6, 1.99),
            MobType::Creeper => (0.6, 1.7),
            MobType::Spider => (1.4, 0.9),
            MobType::CaveSpider => (0.7, 0.5),
            MobType::Silverfish => (0.4, 0.3),
            MobType::Cow => (0.9, 1.4),
            MobType::Pig => (0.9, 0.9),
            MobType::Sheep => (0.9, 1.3),
            MobType::Chicken => (0.4, 0.7),
        }
    }

    /// `movement_speed` attribute
    pub fn speed(self) -> f64 {
        match self {
            MobType::Zombie | MobType::Sheep => 0.23,
            MobType::Skeleton | MobType::Creeper | MobType::Pig | MobType::Chicken | MobType::Silverfish => 0.25,
            MobType::Spider | MobType::CaveSpider => 0.3,
            MobType::Cow => 0.2,
        }
    }

    /// melee `attack_damage` (normal difficulty)
    pub fn attack(self) -> f32 {
        match self {
            MobType::Zombie => 3.0,
            MobType::Spider | MobType::CaveSpider => 2.0,
            MobType::Silverfish => 1.0,
            _ => 0.0,
        }
    }

    pub fn hostile(self) -> bool {
        matches!(self, MobType::Zombie | MobType::Skeleton | MobType::Creeper | MobType::Spider | MobType::CaveSpider | MobType::Silverfish)
    }

    /// burns in daylight
    pub fn burns(self) -> bool {
        matches!(self, MobType::Zombie | MobType::Skeleton)
    }

    pub fn climbs(self) -> bool {
        matches!(self, MobType::Spider | MobType::CaveSpider)
    }

    pub fn ambient_sound(self) -> &'static str {
        match self {
            MobType::Zombie => "minecraft:entity.zombie.ambient",
            MobType::Skeleton => "minecraft:entity.skeleton.ambient",
            MobType::Creeper => "",
            MobType::Spider | MobType::CaveSpider => "minecraft:entity.spider.ambient",
            MobType::Silverfish => "minecraft:entity.silverfish.ambient",
            MobType::Cow => "minecraft:entity.cow.ambient",
            MobType::Pig => "minecraft:entity.pig.ambient",
            MobType::Sheep => "minecraft:entity.sheep.ambient",
            MobType::Chicken => "minecraft:entity.chicken.ambient",
        }
    }

    pub fn loot_table(self) -> String {
        format!("entities/{}", self.name())
    }
}

/// Runtime state of one mob.
#[derive(Clone, Debug)]
pub struct Mob {
    pub ty: MobType,
    /// health, attributes, effects, invulnerability (vanilla `LivingEntity`)
    pub living: crate::game::combat::living::Living,
    pub eq: crate::game::combat::living::Equipment,
    pub yaw: f32,
    pub head_yaw_sent: f32,
    pub attack_cd: u32,
    pub target: Option<i32>,
    pub wander: Option<[f64; 3]>,
    /// fleeing (passive mobs after a hit)
    pub panic: u32,
    pub flee_from: [f64; 3],
    /// creeper fuse ticks (0..=30)
    pub fuse: i32,
    pub fuse_dir: i32,
    /// remaining burn ticks
    pub fire: i32,
    pub fire_sent: bool,
    /// ticks since death (0 = alive)
    pub death: u32,
    pub fall: f64,
    pub persistent: bool,
    pub ambient: i32,
}

impl Mob {
    pub fn new(ty: MobType, persistent: bool) -> Mob {
        let attrs = crate::game::etypes::EntityTypes::get().by_name(ty.name()).map(|t| t.attributes()).unwrap_or_else(|| crate::game::combat::attr::Map::new(&crate::game::combat::attr::living()));
        Mob {
            ty,
            living: crate::game::combat::living::Living::new(attrs),
            eq: Default::default(),
            yaw: 0.0,
            head_yaw_sent: 0.0,
            attack_cd: 0,
            target: None,
            wander: None,
            panic: 0,
            flee_from: [0.0; 3],
            fuse: 0,
            fuse_dir: -1,
            fire: 0,
            fire_sent: false,
            death: 0,
            fall: 0.0,
            persistent: persistent || !ty.hostile(),
            ambient: 0,
        }
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

/// Creatures for a biome's natural spawns: (type, weight).
pub fn creatures(biome: &str) -> &'static [(MobType, u32)] {
    const FARM: [(MobType, u32); 4] = [(MobType::Sheep, 12), (MobType::Pig, 10), (MobType::Chicken, 10), (MobType::Cow, 8)];
    let b = biome.trim_start_matches("minecraft:");
    if b.contains("ocean") || b.contains("river") || b.contains("beach") || b.contains("desert") || b.contains("badlands") || b.starts_with("snowy") || b.starts_with("frozen") || b.contains("ice") || b.contains("mushroom") || b.contains("deep_dark") || b.contains("dripstone") || b.contains("lush") || b.contains("peaks") || b.contains("slopes") || b.contains("swamp") || b == "stony_shore" {
        return &[];
    }
    &FARM
}

/// Monsters for a biome's natural spawns: (type, weight).
pub fn monsters(biome: &str) -> &'static [(MobType, u32)] {
    const MONSTERS: [(MobType, u32); 4] = [(MobType::Spider, 100), (MobType::Zombie, 95), (MobType::Skeleton, 100), (MobType::Creeper, 100)];
    let b = biome.trim_start_matches("minecraft:");
    if b.contains("mushroom") || b.contains("deep_dark") {
        return &[];
    }
    &MONSTERS
}

pub fn pick(list: &[(MobType, u32)], rng: &mut impl Rng) -> Option<MobType> {
    let total: u32 = list.iter().map(|x| x.1).sum();
    if total == 0 {
        return None;
    }
    let mut r = rng.gen_range(0..total);
    for &(m, w) in list {
        if r < w {
            return Some(m);
        }
        r -= w;
    }
    None
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
        for m in ALL {
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
        for ty in ALL {
            let mut total = 0usize;
            for _ in 0..50 {
                total += data.roll_loot(&ty.loot_table(), &mut rng).iter().map(|s| s.count as usize).sum::<usize>();
            }
            eprintln!("{}: {total} items in 50 kills", ty.name());
            if matches!(ty, MobType::Zombie | MobType::Skeleton | MobType::Cow | MobType::Chicken | MobType::Spider | MobType::Creeper | MobType::Pig) {
                assert!(total > 0, "{} drops nothing", ty.name());
            }
        }
    }
}
