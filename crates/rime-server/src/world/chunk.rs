//! In-memory chunk model and vanilla (1.21.x) chunk NBT serialisation.

use crate::compound;
use crate::nbt::Tag;

pub const MIN_Y: i32 = -64;
pub const HEIGHT: i32 = 384;
pub const SECTIONS: usize = (HEIGHT / 16) as usize;
/// Data version written into chunks (Minecraft 1.21.4).
pub const DATA_VERSION: i32 = 4189;

/// Blocks RimeMC currently generates. Index = runtime id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum Block {
    Air = 0,
    Stone,
    Deepslate,
    Dirt,
    GrassBlock,
    Bedrock,
    Water,
    Sand,
    Gravel,
    Sandstone,
    CoalOre,
    IronOre,
    CopperOre,
    GoldOre,
    RedstoneOre,
    LapisOre,
    DiamondOre,
    EmeraldOre,
    DeepslateCoalOre,
    DeepslateIronOre,
    DeepslateCopperOre,
    DeepslateGoldOre,
    DeepslateRedstoneOre,
    DeepslateLapisOre,
    DeepslateDiamondOre,
    DeepslateEmeraldOre,
}

impl Block {
    pub const ALL: [Block; 26] = [
        Block::Air,
        Block::Stone,
        Block::Deepslate,
        Block::Dirt,
        Block::GrassBlock,
        Block::Bedrock,
        Block::Water,
        Block::Sand,
        Block::Gravel,
        Block::Sandstone,
        Block::CoalOre,
        Block::IronOre,
        Block::CopperOre,
        Block::GoldOre,
        Block::RedstoneOre,
        Block::LapisOre,
        Block::DiamondOre,
        Block::EmeraldOre,
        Block::DeepslateCoalOre,
        Block::DeepslateIronOre,
        Block::DeepslateCopperOre,
        Block::DeepslateGoldOre,
        Block::DeepslateRedstoneOre,
        Block::DeepslateLapisOre,
        Block::DeepslateDiamondOre,
        Block::DeepslateEmeraldOre,
    ];

    pub fn from_id(id: u16) -> Block {
        Self::ALL.get(id as usize).copied().unwrap_or(Block::Air)
    }

    pub fn name(self) -> &'static str {
        match self {
            Block::Air => "minecraft:air",
            Block::Stone => "minecraft:stone",
            Block::Deepslate => "minecraft:deepslate",
            Block::Dirt => "minecraft:dirt",
            Block::GrassBlock => "minecraft:grass_block",
            Block::Bedrock => "minecraft:bedrock",
            Block::Water => "minecraft:water",
            Block::Sand => "minecraft:sand",
            Block::Gravel => "minecraft:gravel",
            Block::Sandstone => "minecraft:sandstone",
            Block::CoalOre => "minecraft:coal_ore",
            Block::IronOre => "minecraft:iron_ore",
            Block::CopperOre => "minecraft:copper_ore",
            Block::GoldOre => "minecraft:gold_ore",
            Block::RedstoneOre => "minecraft:redstone_ore",
            Block::LapisOre => "minecraft:lapis_ore",
            Block::DiamondOre => "minecraft:diamond_ore",
            Block::EmeraldOre => "minecraft:emerald_ore",
            Block::DeepslateCoalOre => "minecraft:deepslate_coal_ore",
            Block::DeepslateIronOre => "minecraft:deepslate_iron_ore",
            Block::DeepslateCopperOre => "minecraft:deepslate_copper_ore",
            Block::DeepslateGoldOre => "minecraft:deepslate_gold_ore",
            Block::DeepslateRedstoneOre => "minecraft:deepslate_redstone_ore",
            Block::DeepslateLapisOre => "minecraft:deepslate_lapis_ore",
            Block::DeepslateDiamondOre => "minecraft:deepslate_diamond_ore",
            Block::DeepslateEmeraldOre => "minecraft:deepslate_emerald_ore",
        }
    }

    fn properties(self) -> Option<Tag> {
        match self {
            Block::GrassBlock => Some(compound![("snowy", Tag::String("false".into()))]),
            Block::Water => Some(compound![("level", Tag::String("0".into()))]),
            Block::RedstoneOre | Block::DeepslateRedstoneOre => Some(compound![("lit", Tag::String("false".into()))]),
            Block::Deepslate => Some(compound![("axis", Tag::String("y".into()))]),
            _ => None,
        }
    }

    pub fn from_name(name: &str) -> Option<Block> {
        Self::ALL.iter().copied().find(|b| b.name() == name)
    }

    pub fn is_ore(self) -> bool {
        (self as u16) >= Block::CoalOre as u16
    }

    /// Blocks you can see through (an ore next to one of these is visible).
    pub fn is_transparent(self) -> bool {
        matches!(self, Block::Air | Block::Water)
    }

    pub fn is_solid(self) -> bool {
        !matches!(self, Block::Air | Block::Water)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Section {
    Uniform(Block),
    Full(Box<[Block; 4096]>),
}

impl Section {
    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> Block {
        match self {
            Section::Uniform(b) => *b,
            Section::Full(a) => a[(y << 8) | (z << 4) | x],
        }
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, b: Block) {
        if let Section::Uniform(u) = self {
            if *u == b {
                return;
            }
            *self = Section::Full(Box::new([*u; 4096]));
        }
        if let Section::Full(a) = self {
            a[(y << 8) | (z << 4) | x] = b;
        }
    }

    /// Collapse to `Uniform` if possible (saves memory and bandwidth).
    pub fn optimize(&mut self) {
        if let Section::Full(a) = self {
            let first = a[0];
            if a.iter().all(|b| *b == first) {
                *self = Section::Uniform(first);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Biome {
    Plains,
    Ocean,
    Beach,
}

impl Biome {
    pub fn name(self) -> &'static str {
        match self {
            Biome::Plains => "minecraft:plains",
            Biome::Ocean => "minecraft:ocean",
            Biome::Beach => "minecraft:beach",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Chunk {
    pub x: i32,
    pub z: i32,
    pub sections: Vec<Section>,
    /// one biome per 4×4 column (16 entries), applied to all heights
    pub biomes: [Biome; 16],
    /// highest non-air block per column (absolute Y)
    pub heightmap: [i16; 256],
}

impl Chunk {
    pub fn empty(x: i32, z: i32) -> Self {
        Self { x, z, sections: vec![Section::Uniform(Block::Air); SECTIONS], biomes: [Biome::Plains; 16], heightmap: [MIN_Y as i16; 256] }
    }

    #[inline]
    pub fn get(&self, x: usize, y: i32, z: usize) -> Block {
        let ry = y - MIN_Y;
        if !(0..HEIGHT).contains(&ry) {
            return Block::Air;
        }
        self.sections[(ry >> 4) as usize].get(x, (ry & 15) as usize, z)
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: i32, z: usize, b: Block) {
        let ry = y - MIN_Y;
        if (0..HEIGHT).contains(&ry) {
            self.sections[(ry >> 4) as usize].set(x, (ry & 15) as usize, z, b);
        }
    }

    /// Serialise to vanilla chunk NBT (root compound bytes, uncompressed).
    pub fn to_nbt(&self, last_update: i64) -> Vec<u8> {
        let mut sections = Vec::with_capacity(SECTIONS);
        for (i, s) in self.sections.iter().enumerate() {
            let y = (MIN_Y >> 4) + i as i32;
            sections.push(compound![
                ("Y", Tag::Byte(y as i8)),
                ("block_states", encode_block_states(s)),
                ("biomes", encode_biomes(&self.biomes)),
            ]);
        }
        let root = compound![
            ("DataVersion", Tag::Int(DATA_VERSION)),
            ("xPos", Tag::Int(self.x)),
            ("zPos", Tag::Int(self.z)),
            ("yPos", Tag::Int(MIN_Y >> 4)),
            ("Status", Tag::String("minecraft:full".into())),
            ("LastUpdate", Tag::Long(last_update)),
            ("InhabitedTime", Tag::Long(0)),
            ("isLightOn", Tag::Byte(0)),
            ("sections", Tag::List(sections)),
            ("block_entities", Tag::List(vec![])),
            ("block_ticks", Tag::List(vec![])),
            ("fluid_ticks", Tag::List(vec![])),
            ("PostProcessing", Tag::List(vec![])),
            ("structures", compound![("References", compound![]), ("starts", compound![])]),
        ];
        root.to_root_bytes("")
    }

    /// Parse block data back from vanilla chunk NBT. Unknown blocks become stone
    /// (solid) so physics/anti-cheat never see holes that are not there.
    pub fn from_nbt(bytes: &[u8]) -> Option<Chunk> {
        let (_, root) = Tag::from_root_bytes(bytes).ok()?;
        let x = root.get("xPos")?.as_i64()? as i32;
        let z = root.get("zPos")?.as_i64()? as i32;
        let mut c = Chunk::empty(x, z);
        let Some(Tag::List(secs)) = root.get("sections") else { return Some(c) };
        for s in secs {
            let y = s.get("Y").and_then(|t| t.as_i64()).unwrap_or(i64::MIN);
            let idx = y - (MIN_Y >> 4) as i64;
            if !(0..SECTIONS as i64).contains(&idx) {
                continue;
            }
            let Some(bs) = s.get("block_states") else { continue };
            let Some(Tag::List(pal)) = bs.get("palette") else { continue };
            let palette: Vec<Block> = pal
                .iter()
                .map(|p| p.get("Name").and_then(|n| n.as_str()).map(|n| Block::from_name(n).unwrap_or(if n.ends_with(":air") { Block::Air } else { Block::Stone })).unwrap_or(Block::Air))
                .collect();
            if palette.is_empty() {
                continue;
            }
            let sec = &mut c.sections[idx as usize];
            if palette.len() == 1 {
                *sec = Section::Uniform(palette[0]);
                continue;
            }
            let Some(Tag::LongArray(data)) = bs.get("data") else { continue };
            let bits = bits_for(palette.len(), 4);
            let per_long = 64 / bits;
            if data.len() < 4096usize.div_ceil(per_long) {
                continue;
            }
            let mask = (1u64 << bits) - 1;
            let mut arr = Box::new([Block::Air; 4096]);
            for (i, slot) in arr.iter_mut().enumerate() {
                let v = ((data[i / per_long] as u64) >> ((i % per_long) * bits)) & mask;
                *slot = palette.get(v as usize).copied().unwrap_or(Block::Stone);
            }
            *sec = Section::Full(arr);
            sec.optimize();
        }
        Some(c)
    }
}

fn bits_for(n: usize, min: usize) -> usize {
    let mut bits = 0;
    while (1usize << bits) < n {
        bits += 1;
    }
    bits.max(min)
}

fn pack(values: &[u16], bits: usize) -> Vec<i64> {
    let per_long = 64 / bits;
    let mut out = vec![0i64; values.len().div_ceil(per_long)];
    for (i, v) in values.iter().enumerate() {
        out[i / per_long] |= ((*v as u64) << ((i % per_long) * bits)) as i64;
    }
    out
}

fn block_state_tag(b: Block) -> Tag {
    let mut v = vec![("Name".to_string(), Tag::String(b.name().into()))];
    if let Some(p) = b.properties() {
        v.push(("Properties".to_string(), p));
    }
    Tag::Compound(v)
}

fn encode_block_states(s: &Section) -> Tag {
    match s {
        Section::Uniform(b) => compound![("palette", Tag::List(vec![block_state_tag(*b)]))],
        Section::Full(arr) => {
            let mut palette: Vec<Block> = Vec::new();
            let mut lut = [u16::MAX; Block::ALL.len()];
            let mut idx = vec![0u16; 4096];
            for (i, b) in arr.iter().enumerate() {
                let id = *b as usize;
                if lut[id] == u16::MAX {
                    lut[id] = palette.len() as u16;
                    palette.push(*b);
                }
                idx[i] = lut[id];
            }
            if palette.len() == 1 {
                return encode_block_states(&Section::Uniform(palette[0]));
            }
            compound![
                ("palette", Tag::List(palette.iter().map(|b| block_state_tag(*b)).collect())),
                ("data", Tag::LongArray(pack(&idx, bits_for(palette.len(), 4)))),
            ]
        }
    }
}

fn encode_biomes(b: &[Biome; 16]) -> Tag {
    let mut palette: Vec<Biome> = Vec::new();
    let mut idx = vec![0u16; 64];
    for y in 0..4 {
        for z in 0..4 {
            for x in 0..4 {
                let biome = b[z * 4 + x];
                let p = match palette.iter().position(|p| *p == biome) {
                    Some(p) => p,
                    None => {
                        palette.push(biome);
                        palette.len() - 1
                    }
                };
                idx[(y << 4) | (z << 2) | x] = p as u16;
            }
        }
    }
    let names = Tag::List(palette.iter().map(|b| Tag::String(b.name().into())).collect());
    if palette.len() == 1 {
        compound![("palette", names)]
    } else {
        compound![("palette", names), ("data", Tag::LongArray(pack(&idx, bits_for(palette.len(), 1))))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nbt_roundtrip_blocks() {
        let mut c = Chunk::empty(3, -9);
        for x in 0..16 {
            for z in 0..16 {
                c.set(x, -64, z, Block::Bedrock);
                c.set(x, 10, z, if (x + z) % 3 == 0 { Block::DiamondOre } else { Block::Stone });
            }
        }
        c.set(5, 100, 5, Block::Water);
        let bytes = c.to_nbt(0);
        let back = Chunk::from_nbt(&bytes).unwrap();
        assert_eq!((back.x, back.z), (3, -9));
        for x in 0..16 {
            for z in 0..16 {
                for y in [-64, 10, 100, 200] {
                    assert_eq!(back.get(x, y, z), c.get(x, y, z));
                }
            }
        }
    }

    #[test]
    fn packing_matches_vanilla_rules() {
        assert_eq!(bits_for(2, 4), 4);
        assert_eq!(bits_for(17, 4), 5);
        // 5 bits → 12 per long → 342 longs for 4096 entries (no spanning)
        assert_eq!(pack(&vec![0; 4096], 5).len(), 342);
    }
}
