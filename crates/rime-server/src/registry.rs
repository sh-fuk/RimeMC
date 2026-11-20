//! Protocol tables for the supported Minecraft version: block states, built-in
//! registries and packet ids. The tables are generated from the official
//! data-generator reports by `tools/gen_tables.py` and embedded at compile time.

use std::collections::HashMap;
use std::sync::OnceLock;

/// A global block-state id (what goes over the network and in chunk palettes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct BlockState(pub u16);

/// Default-state constants for every block (`blocks::STONE`, `blocks::OAK_LOG`, …).
#[allow(dead_code)]
pub mod blocks {
    use super::BlockState;
    include!(concat!(env!("OUT_DIR"), "/blocks_gen.rs"));
}

const BLOCKS_TSV: &str = include_str!("../assets/26.1/blocks.tsv");
const REGISTRIES_TSV: &str = include_str!("../assets/26.1/registries.tsv");
const PACKETS_TSV: &str = include_str!("../assets/26.1/packets.tsv");
pub const PROTOCOL_VERSION: i32 = 775;

pub struct BlockInfo {
    pub name: String,
    pub first: u16,
    pub default: u16,
    /// properties sorted by name, with their possible values
    pub props: Vec<(String, Vec<String>)>,
    strides: Vec<u16>,
}

impl BlockInfo {
    fn state_count(&self) -> u16 {
        self.props.iter().map(|(_, v)| v.len() as u16).product::<u16>().max(1)
    }
}

const F_AIR: u8 = 1;
const F_FLUID: u8 = 2;
const F_SOLID: u8 = 4;
const F_OPAQUE: u8 = 8;
const F_ORE: u8 = 16;
const F_WATERLOGGED: u8 = 32;

pub struct BlockRegistry {
    pub blocks: Vec<BlockInfo>,
    by_name: HashMap<String, usize>,
    state_block: Vec<u16>,
    flags: Vec<u8>,
}

/// Blocks without a collision box (players walk through them).
const NO_COLLISION: &[&str] = &[
    "short_grass", "tall_grass", "fern", "large_fern", "dead_bush", "bush", "dandelion", "poppy", "blue_orchid", "allium", "azure_bluet", "tulip",
    "oxeye_daisy", "cornflower", "lily_of_the_valley", "wither_rose", "sunflower", "lilac", "rose_bush", "peony", "sapling", "torch", "redstone_wire",
    "rail", "button", "pressure_plate", "sign", "banner", "vine", "sugar_cane", "kelp", "seagrass", "coral_fan", "_coral", "brown_mushroom",
    "red_mushroom", "wheat", "carrots", "potatoes", "beetroots", "nether_sprouts", "_roots", "fire", "tripwire", "lever", "structure_void", "light",
    "nether_wart", "cobweb", "pink_petals", "wildflowers", "leaf_litter", "glow_lichen", "sculk_vein", "spore_blossom", "hanging_roots", "cave_vines",
    "weeping_vines", "twisting_vines", "torchflower", "pitcher", "small_dripleaf", "firefly_bush", "short_dry_grass", "tall_dry_grass", "bubble_column",
    "end_gateway", "nether_portal", "end_portal", "moss_carpet", "resin_clump", "pale_hanging_moss", "open_eyeblossom", "closed_eyeblossom",
];
/// Blocks that let light/vision through even though they are solid.
const SEE_THROUGH: &[&str] = &["glass", "leaves", "ice", "slab", "stairs", "fence", "wall", "pane", "bars", "door", "trapdoor", "chest", "carpet", "snow", "farmland", "path", "lantern", "chain", "candle", "bed", "anvil", "cactus", "scaffolding", "honey_block", "slime_block", "spawner", "barrier", "piston_head", "cake", "pot", "head", "skull", "bell", "campfire", "lectern", "enchanting_table", "end_rod", "lightning_rod", "amethyst_cluster", "bud", "pointed_dripstone", "azalea", "dripleaf", "sniffer_egg", "turtle_egg", "sea_pickle", "conduit", "copper_grate", "vault", "trial_spawner", "heavy_core", "decorated_pot", "hopper", "grindstone", "stonecutter", "brewing_stand", "cauldron", "composter", "daylight_detector", "repeater", "comparator", "frogspawn", "mangrove_roots", "creaking_heart", "shelf", "golem_statue"];

fn parse_blocks() -> BlockRegistry {
    let mut blocks = Vec::new();
    let mut by_name = HashMap::new();
    for line in BLOCKS_TSV.lines().filter(|l| !l.is_empty()) {
        let mut f = line.split('\t');
        let name = f.next().unwrap().to_string();
        let first: u16 = f.next().unwrap().parse().unwrap();
        let default: u16 = f.next().unwrap().parse().unwrap();
        let spec = f.next().unwrap_or("");
        let props: Vec<(String, Vec<String>)> = spec
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap();
                (k.to_string(), v.split(',').map(str::to_string).collect())
            })
            .collect();
        let mut strides = vec![0u16; props.len()];
        let mut acc = 1u16;
        for i in (0..props.len()).rev() {
            strides[i] = acc;
            acc *= props[i].1.len() as u16;
        }
        by_name.insert(name.clone(), blocks.len());
        blocks.push(BlockInfo { name, first, default, props, strides });
    }
    let total = blocks::STATE_COUNT as usize;
    let mut state_block = vec![0u16; total];
    let mut flags = vec![0u8; total];
    for (bi, b) in blocks.iter().enumerate() {
        let short = b.name.trim_start_matches("minecraft:");
        let mut f = 0u8;
        if matches!(short, "air" | "cave_air" | "void_air") {
            f |= F_AIR;
        } else if matches!(short, "water" | "lava") {
            f |= F_FLUID;
        } else if !NO_COLLISION.iter().any(|n| short.contains(n)) {
            f |= F_SOLID;
            if !SEE_THROUGH.iter().any(|n| short.contains(n)) {
                f |= F_OPAQUE;
            }
        }
        if short.ends_with("_ore") || short == "ancient_debris" {
            f |= F_ORE;
        }
        for s in b.first..b.first + b.state_count() {
            state_block[s as usize] = bi as u16;
            flags[s as usize] = f;
        }
        // waterlogged=true states contain water
        if let Some(i) = b.props.iter().position(|(k, _)| k == "waterlogged") {
            let ti = b.props[i].1.iter().position(|v| v == "true").unwrap_or(0) as u16;
            for s in b.first..b.first + b.state_count() {
                if ((s - b.first) / b.strides[i]) % b.props[i].1.len() as u16 == ti {
                    flags[s as usize] |= F_WATERLOGGED;
                }
            }
        }
    }
    BlockRegistry { blocks, by_name, state_block, flags }
}

impl BlockRegistry {
    pub fn get() -> &'static BlockRegistry {
        static R: OnceLock<BlockRegistry> = OnceLock::new();
        R.get_or_init(parse_blocks)
    }

    /// Number of block states.
    pub fn state_count(&self) -> usize {
        self.state_block.len()
    }

    pub fn block(&self, s: BlockState) -> &BlockInfo {
        &self.blocks[*self.state_block.get(s.0 as usize).unwrap_or(&0) as usize]
    }

    pub fn by_name(&self, name: &str) -> Option<&BlockInfo> {
        let key = if name.contains(':') { name.to_string() } else { format!("minecraft:{name}") };
        self.by_name.get(&key).map(|i| &self.blocks[*i])
    }

    /// State for `name` with the given properties (others at their defaults).
    pub fn state(&self, name: &str, props: &[(&str, &str)]) -> Option<BlockState> {
        let b = self.by_name(name)?;
        let mut s = BlockState(b.default);
        for (k, v) in props {
            s = self.with(s, k, v)?;
        }
        Some(s)
    }

    /// Change one property of a state.
    pub fn with(&self, s: BlockState, key: &str, value: &str) -> Option<BlockState> {
        let b = self.block(s);
        let i = b.props.iter().position(|(k, _)| k == key)?;
        let vals = &b.props[i].1;
        let new = vals.iter().position(|v| v == value)? as u16;
        let rel = s.0 - b.first;
        let cur = (rel / b.strides[i]) % vals.len() as u16;
        Some(BlockState(b.first + rel - cur * b.strides[i] + new * b.strides[i]))
    }

    /// Property values of a state, in name order.
    pub fn properties(&self, s: BlockState) -> Vec<(&str, &str)> {
        let b = self.block(s);
        let rel = s.0 - b.first;
        b.props
            .iter()
            .enumerate()
            .map(|(i, (k, vals))| (k.as_str(), vals[((rel / b.strides[i]) % vals.len() as u16) as usize].as_str()))
            .collect()
    }

    pub fn property(&self, s: BlockState, key: &str) -> Option<&str> {
        self.properties(s).into_iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    fn flag(&self, s: BlockState, f: u8) -> bool {
        self.flags.get(s.0 as usize).is_some_and(|x| x & f != 0)
    }
    pub fn is_air(&self, s: BlockState) -> bool {
        self.flag(s, F_AIR)
    }
    pub fn is_fluid(&self, s: BlockState) -> bool {
        self.flag(s, F_FLUID)
    }
    pub fn has_fluid(&self, s: BlockState) -> bool {
        self.flag(s, F_FLUID | F_WATERLOGGED)
    }
    pub fn is_solid(&self, s: BlockState) -> bool {
        self.flag(s, F_SOLID)
    }
    /// Full, light-blocking cube.
    pub fn is_opaque(&self, s: BlockState) -> bool {
        self.flag(s, F_OPAQUE)
    }
    pub fn is_ore(&self, s: BlockState) -> bool {
        self.flag(s, F_ORE)
    }
}

impl BlockState {
    /// Block name without the `minecraft:` namespace (e.g. `oak_log`).
    pub fn name(self) -> &'static str {
        BlockRegistry::get().block(self).name.trim_start_matches("minecraft:")
    }
    pub fn is_air(self) -> bool {
        BlockRegistry::get().is_air(self)
    }
    pub fn is_solid(self) -> bool {
        BlockRegistry::get().is_solid(self)
    }
    pub fn is_opaque(self) -> bool {
        BlockRegistry::get().is_opaque(self)
    }
    pub fn is_ore(self) -> bool {
        BlockRegistry::get().is_ore(self)
    }
    /// Can you see through it (X-ray exposure / light)?
    pub fn is_transparent(self) -> bool {
        !self.is_opaque()
    }
}

// ------------------------------------------------------------- registries

pub struct Registries {
    regs: HashMap<String, (Vec<String>, HashMap<String, i32>)>,
}

impl Registries {
    pub fn get() -> &'static Registries {
        static R: OnceLock<Registries> = OnceLock::new();
        R.get_or_init(|| {
            let mut regs = HashMap::new();
            for line in REGISTRIES_TSV.lines().filter(|l| !l.is_empty()) {
                let (name, entries) = line.split_once('\t').unwrap_or((line, ""));
                let list: Vec<String> = entries.split(' ').filter(|s| !s.is_empty()).map(str::to_string).collect();
                let idx = list.iter().enumerate().map(|(i, n)| (n.clone(), i as i32)).collect();
                regs.insert(name.to_string(), (list, idx));
            }
            Registries { regs }
        })
    }

    pub fn id(&self, registry: &str, name: &str) -> Option<i32> {
        let r = &self.regs.get(registry)?.1;
        r.get(name).copied().or_else(|| if name.contains(':') { None } else { r.get(&format!("minecraft:{name}")).copied() })
    }

    pub fn entries(&self, registry: &str) -> Option<&[String]> {
        self.regs.get(registry).map(|r| r.0.as_slice())
    }
}

// ------------------------------------------------------------- packets

pub struct PacketIds {
    map: HashMap<(String, String, String), i32>,
}

impl PacketIds {
    pub fn get() -> &'static PacketIds {
        static P: OnceLock<PacketIds> = OnceLock::new();
        P.get_or_init(|| {
            let mut map = HashMap::new();
            for line in PACKETS_TSV.lines().filter(|l| !l.is_empty()) {
                let f: Vec<&str> = line.split('\t').collect();
                map.insert((f[0].to_string(), f[1].to_string(), f[3].trim_start_matches("minecraft:").to_string()), f[2].parse().unwrap());
            }
            PacketIds { map }
        })
    }

    /// Packet id, panicking on an unknown name (caught by tests).
    pub fn id(&self, state: &str, dir: &str, name: &str) -> i32 {
        *self.map.get(&(state.to_string(), dir.to_string(), name.to_string())).unwrap_or_else(|| panic!("unknown packet {state}/{dir}/{name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_match_vanilla_ids() {
        let r = BlockRegistry::get();
        assert_eq!(blocks::AIR, BlockState(0));
        assert_eq!(blocks::GRASS_BLOCK, BlockState(9)); // snowy=false
        assert_eq!(r.state("grass_block", &[("snowy", "true")]), Some(BlockState(8)));
        // chest: facing, type, waterlogged (alphabetical, last fastest)
        let left_north_wet = r.state("chest", &[("type", "left"), ("facing", "north"), ("waterlogged", "true")]).unwrap();
        assert_eq!(left_north_wet, BlockState(3989));
        assert_eq!(r.properties(left_north_wet), vec![("facing", "north"), ("type", "left"), ("waterlogged", "true")]);
        assert!(r.has_fluid(left_north_wet));
        assert!(blocks::STONE.is_opaque());
        assert!(!blocks::SHORT_GRASS.is_solid());
        assert!(blocks::DIAMOND_ORE.is_ore());
        assert!(blocks::WATER.is_transparent());
    }

    #[test]
    fn registries_and_packets() {
        let r = Registries::get();
        assert!(r.id("minecraft:entity_type", "minecraft:player").is_some());
        assert_eq!(r.id("minecraft:block", "minecraft:air"), Some(0));
        let p = PacketIds::get();
        assert_eq!(p.id("play", "clientbound", "login"), 49);
        assert_eq!(p.id("play", "serverbound", "move_player_pos"), 30);
    }
}

/// Exact per-state facts dumped from vanilla 26.1 (`assets/26.1/state_flags.bin`):
/// four tables of one byte per state id: flags, fluid, sturdy faces, extra flags.
pub mod vstate {
    use super::BlockState;

    const DATA: &[u8] = include_bytes!("../assets/26.1/state_flags.bin");

    #[inline]
    fn flags(s: BlockState) -> u8 {
        DATA.get(s.0 as usize).copied().unwrap_or(0)
    }
    /// `BlockState.isAir`
    pub fn air(s: BlockState) -> bool {
        flags(s) & 1 != 0
    }
    /// `BlockState.blocksMotion`
    pub fn blocks_motion(s: BlockState) -> bool {
        flags(s) & 2 != 0
    }
    /// `BlockState.isSolid` (legacy solid)
    pub fn solid(s: BlockState) -> bool {
        flags(s) & 4 != 0
    }
    /// `BlockState.canBeReplaced`
    pub fn replaceable(s: BlockState) -> bool {
        flags(s) & 8 != 0
    }
    /// collision shape has a full top face
    pub fn full_up_face(s: BlockState) -> bool {
        flags(s) & 16 != 0
    }
    /// `isCollisionShapeFullBlock`
    pub fn full_block(s: BlockState) -> bool {
        flags(s) & 32 != 0
    }
    /// 0 none, 1 water source, 2 flowing water, 3 lava source, 4 flowing lava
    pub fn fluid(s: BlockState) -> u8 {
        table(1, s)
    }
    #[inline]
    fn table(i: usize, s: BlockState) -> u8 {
        let n = DATA.len() / 4;
        DATA.get(i * n + s.0 as usize).copied().unwrap_or(0)
    }
    /// `isFaceSturdy(dir, FULL)`; dir in vanilla order down, up, north, south, west, east
    pub fn sturdy(s: BlockState, dir: u8) -> bool {
        table(2, s) >> dir & 1 != 0
    }
    /// `isSolidRender`
    pub fn solid_render(s: BlockState) -> bool {
        table(3, s) & 1 != 0
    }
    /// `isFaceSturdy(UP, CENTER)`
    pub fn sturdy_up_center(s: BlockState) -> bool {
        table(3, s) & 2 != 0
    }
    /// collision shape's top face is empty
    pub fn up_face_empty(s: BlockState) -> bool {
        table(3, s) & 4 != 0
    }
    /// `BlockState.liquid()`
    pub fn liquid(s: BlockState) -> bool {
        table(3, s) & 8 != 0
    }
    /// fluid is a source
    pub fn fluid_source(s: BlockState) -> bool {
        table(3, s) & 16 != 0
    }
    pub fn has_fluid(s: BlockState) -> bool {
        fluid(s) != 0
    }
    pub fn is_water(s: BlockState) -> bool {
        matches!(fluid(s), 1 | 2)
    }
}
