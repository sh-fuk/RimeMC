//! In-memory chunk model, vanilla chunk NBT (disk) and network serialisation.

use std::sync::OnceLock;

use crate::compound;
use crate::nbt::Tag;
use crate::registry::{blocks, BlockRegistry, BlockState};

pub const MIN_Y: i32 = -64;
pub const HEIGHT: i32 = 384;
pub const SECTIONS: usize = (HEIGHT / 16) as usize;
/// Data version written into chunks (Minecraft 26.1).
pub const DATA_VERSION: i32 = 4786;

/// Biome ids = index in the alphabetically sorted biome registry (as vanilla sends it).
pub struct Biomes {
    pub names: Vec<String>,
}

impl Biomes {
    pub fn get() -> &'static Biomes {
        static B: OnceLock<Biomes> = OnceLock::new();
        B.get_or_init(|| Biomes { names: include_str!("../../assets/26.1/biomes.txt").lines().filter(|l| !l.is_empty()).map(str::to_string).collect() })
    }
    pub fn id(&self, name: &str) -> Option<u8> {
        self.names.iter().position(|n| n == name).map(|i| i as u8)
    }
    pub fn plains() -> u8 {
        Self::get().id("minecraft:plains").unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Section {
    Uniform(BlockState),
    Full(Box<[BlockState; 4096]>),
}

impl Section {
    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> BlockState {
        match self {
            Section::Uniform(b) => *b,
            Section::Full(a) => a[(y << 8) | (z << 4) | x],
        }
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, b: BlockState) {
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

    pub fn optimize(&mut self) {
        if let Section::Full(a) = self {
            let first = a[0];
            if a.iter().all(|b| *b == first) {
                *self = Section::Uniform(first);
            }
        }
    }

    fn iter(&self) -> Box<dyn Iterator<Item = BlockState> + '_> {
        match self {
            Section::Uniform(b) => Box::new(std::iter::repeat_n(*b, 4096)),
            Section::Full(a) => Box::new(a.iter().copied()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Chunk {
    pub x: i32,
    pub z: i32,
    pub sections: Vec<Section>,
    /// biome per section per 4×4×4 cell, index (y<<4)|(z<<2)|x
    pub biomes: Vec<[u8; 64]>,
    /// highest non-air block per column (absolute Y), MIN_Y-1 if empty
    pub heightmap: [i16; 256],
    /// generator that produced the chunk (`Generator::gen_id`; 0 = unknown/old)
    pub gen_id: u32,
    /// changed by players since it was generated
    pub edited: bool,
    /// fluids to tick once the freshly generated chunk is loaded (not saved)
    pub fluid_ticks: Vec<[i32; 3]>,
}

impl Chunk {
    pub fn empty(x: i32, z: i32) -> Self {
        Self {
            x,
            z,
            sections: vec![Section::Uniform(blocks::AIR); SECTIONS],
            biomes: vec![[Biomes::plains(); 64]; SECTIONS],
            heightmap: [(MIN_Y - 1) as i16; 256],
            gen_id: 0,
            edited: false,
            fluid_ticks: Vec::new(),
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: i32, z: usize) -> BlockState {
        let ry = y - MIN_Y;
        if !(0..HEIGHT).contains(&ry) {
            return blocks::AIR;
        }
        self.sections[(ry >> 4) as usize].get(x, (ry & 15) as usize, z)
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: i32, z: usize, b: BlockState) {
        let ry = y - MIN_Y;
        if (0..HEIGHT).contains(&ry) {
            self.sections[(ry >> 4) as usize].set(x, (ry & 15) as usize, z, b);
        }
    }

    /// Recompute the heightmap (highest non-air block).
    pub fn recalc_heightmap(&mut self) {
        for z in 0..16 {
            for x in 0..16 {
                let mut h = MIN_Y - 1;
                for y in (MIN_Y..MIN_Y + HEIGHT).rev() {
                    if !self.get(x, y, z).is_air() {
                        h = y;
                        break;
                    }
                }
                self.heightmap[z * 16 + x] = h as i16;
            }
        }
    }

    /// Update the heightmap after a single block change.
    pub fn update_height(&mut self, x: usize, y: i32, z: usize) {
        let i = z * 16 + x;
        let cur = self.heightmap[i] as i32;
        if !self.get(x, y, z).is_air() {
            if y > cur {
                self.heightmap[i] = y as i16;
            }
        } else if y == cur {
            let mut h = MIN_Y - 1;
            for yy in (MIN_Y..y).rev() {
                if !self.get(x, yy, z).is_air() {
                    h = yy;
                    break;
                }
            }
            self.heightmap[i] = h as i16;
        }
    }

    // ------------------------------------------------------------- disk NBT

    pub fn to_nbt(&self, last_update: i64) -> Vec<u8> {
        let reg = BlockRegistry::get();
        let biome_names = &Biomes::get().names;
        let mut sections = Vec::with_capacity(SECTIONS);
        for (i, s) in self.sections.iter().enumerate() {
            let y = (MIN_Y >> 4) + i as i32;
            let (palette, idx) = local_palette(s);
            let pal = Tag::List(
                palette
                    .iter()
                    .map(|b| {
                        let mut v = vec![("Name".to_string(), Tag::String(reg.block(*b).name.clone()))];
                        let props = reg.properties(*b);
                        if !props.is_empty() {
                            v.push(("Properties".to_string(), Tag::Compound(props.iter().map(|(k, v)| (k.to_string(), Tag::String(v.to_string()))).collect())));
                        }
                        Tag::Compound(v)
                    })
                    .collect(),
            );
            let block_states = if palette.len() == 1 {
                compound![("palette", pal)]
            } else {
                compound![("palette", pal), ("data", Tag::LongArray(pack(&idx, bits_for(palette.len(), 4))))]
            };
            let (bpal, bidx) = biome_palette(&self.biomes[i]);
            let bnames = Tag::List(bpal.iter().map(|b| Tag::String(biome_names.get(*b as usize).cloned().unwrap_or_default())).collect());
            let biomes = if bpal.len() == 1 {
                compound![("palette", bnames)]
            } else {
                compound![("palette", bnames), ("data", Tag::LongArray(pack(&bidx, bits_for(bpal.len(), 1))))]
            };
            sections.push(compound![("Y", Tag::Byte(y as i8)), ("block_states", block_states), ("biomes", biomes)]);
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
            ("RimeGen", Tag::Int(self.gen_id as i32)),
            ("RimeEdited", Tag::Byte(self.edited as i8)),
        ];
        root.to_root_bytes("")
    }

    /// Parse vanilla chunk NBT. Unknown block names become stone (never holes).
    pub fn from_nbt(bytes: &[u8]) -> Option<Chunk> {
        let reg = BlockRegistry::get();
        let biomes = Biomes::get();
        let (_, root) = Tag::from_root_bytes(bytes).ok()?;
        let x = root.get("xPos")?.as_i64()? as i32;
        let z = root.get("zPos")?.as_i64()? as i32;
        let mut c = Chunk::empty(x, z);
        c.gen_id = root.get("RimeGen").and_then(|t| t.as_i64()).unwrap_or(0) as u32;
        c.edited = root.get("RimeEdited").and_then(|t| t.as_i64()).unwrap_or(0) != 0;
        if let Some(Tag::List(secs)) = root.get("sections") {
            for s in secs {
                let y = s.get("Y").and_then(|t| t.as_i64()).unwrap_or(i64::MIN);
                let idx = y - (MIN_Y >> 4) as i64;
                if !(0..SECTIONS as i64).contains(&idx) {
                    continue;
                }
                let idx = idx as usize;
                if let Some(bs) = s.get("block_states") {
                    if let Some(Tag::List(pal)) = bs.get("palette") {
                        let palette: Vec<BlockState> = pal
                            .iter()
                            .map(|p| {
                                let name = p.get("Name").and_then(|n| n.as_str()).unwrap_or("minecraft:air");
                                let mut props: Vec<(&str, &str)> = Vec::new();
                                if let Some(Tag::Compound(pp)) = p.get("Properties") {
                                    for (k, v) in pp {
                                        if let Some(v) = v.as_str() {
                                            props.push((k, v));
                                        }
                                    }
                                }
                                reg.state(name, &props).or_else(|| reg.state(name, &[])).unwrap_or(blocks::STONE)
                            })
                            .collect();
                        if palette.len() == 1 {
                            c.sections[idx] = Section::Uniform(palette[0]);
                        } else if !palette.is_empty() {
                            if let Some(Tag::LongArray(data)) = bs.get("data") {
                                if let Some(v) = unpack(data, bits_for(palette.len(), 4), 4096) {
                                    let mut arr = Box::new([blocks::AIR; 4096]);
                                    for (i, p) in v.iter().enumerate() {
                                        arr[i] = palette.get(*p as usize).copied().unwrap_or(blocks::STONE);
                                    }
                                    let mut sec = Section::Full(arr);
                                    sec.optimize();
                                    c.sections[idx] = sec;
                                }
                            }
                        }
                    }
                }
                if let Some(bm) = s.get("biomes") {
                    if let Some(Tag::List(pal)) = bm.get("palette") {
                        let palette: Vec<u8> = pal.iter().map(|p| p.as_str().and_then(|n| biomes.id(n)).unwrap_or(Biomes::plains())).collect();
                        if palette.len() == 1 {
                            c.biomes[idx] = [palette[0]; 64];
                        } else if let Some(Tag::LongArray(data)) = bm.get("data") {
                            if let Some(v) = unpack(data, bits_for(palette.len(), 1), 64) {
                                for (i, p) in v.iter().enumerate() {
                                    c.biomes[idx][i] = palette.get(*p as usize).copied().unwrap_or(Biomes::plains());
                                }
                            }
                        }
                    }
                }
            }
        }
        c.recalc_heightmap();
        Some(c)
    }

    // ------------------------------------------------------------- network

    /// Section data for `level_chunk_with_light` (26.1 format: non-air count,
    /// fluid count, block container, biome container; no array length prefixes).
    pub fn network_sections(&self) -> Vec<u8> {
        let reg = BlockRegistry::get();
        let global_bits = bits_for(blocks::STATE_COUNT as usize, 1);
        let biome_bits = bits_for(Biomes::get().names.len(), 1);
        let mut out = Vec::with_capacity(16 * 1024);
        for (i, s) in self.sections.iter().enumerate() {
            let (non_air, fluid) = s.iter().fold((0i16, 0i16), |(a, f), b| (a + !reg.is_air(b) as i16, f + reg.has_fluid(b) as i16));
            out.extend_from_slice(&non_air.to_be_bytes());
            out.extend_from_slice(&fluid.to_be_bytes());
            let (palette, idx) = local_palette(s);
            let pal_ids: Vec<u32> = palette.iter().map(|b| b.0 as u32).collect();
            write_container(&mut out, &pal_ids, &idx, 4, 8, global_bits);
            let (bpal, bidx) = biome_palette(&self.biomes[i]);
            let bpal_ids: Vec<u32> = bpal.iter().map(|b| *b as u32).collect();
            write_container(&mut out, &bpal_ids, &bidx, 1, 3, biome_bits);
        }
        out
    }

    /// Heightmaps for the chunk packet: (type id, packed longs).
    /// Types: 1 = WORLD_SURFACE, 4 = MOTION_BLOCKING, 5 = MOTION_BLOCKING_NO_LEAVES.
    pub fn network_heightmaps(&self) -> Vec<(i32, Vec<i64>)> {
        let vals: Vec<u16> = self.heightmap.iter().map(|h| (*h as i32 - MIN_Y + 1).max(0) as u16).collect();
        let packed = pack(&vals, bits_for(HEIGHT as usize + 1, 1));
        vec![(4, packed.clone()), (5, packed.clone()), (1, packed)]
    }

    /// Simple sky light: 15 above the highest light-blocking block of a column,
    /// 0 below. Returns (mask over SECTIONS+2 light sections, arrays).
    pub fn sky_light(&self) -> (u64, u64, Vec<[u8; 2048]>) {
        let mut top = [MIN_Y - 1; 256];
        for z in 0..16 {
            for x in 0..16 {
                let start = self.heightmap[z * 16 + x] as i32;
                let mut y = start;
                while y >= MIN_Y && !self.get(x, y, z).is_opaque() {
                    y -= 1;
                }
                top[z * 16 + x] = y;
            }
        }
        let mut mask = 0u64;
        let mut empty = 0u64;
        let mut arrays = Vec::new();
        for ls in 0..SECTIONS + 2 {
            let base_y = MIN_Y + (ls as i32 - 1) * 16;
            let mut arr = [0u8; 2048];
            let mut any = false;
            for ly in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        if base_y + ly as i32 > top[z * 16 + x] {
                            let i = (ly << 8) | (z << 4) | x;
                            arr[i >> 1] |= 15 << ((i & 1) * 4);
                            any = true;
                        }
                    }
                }
            }
            if any {
                mask |= 1 << ls;
                arrays.push(arr);
            } else {
                empty |= 1 << ls;
            }
        }
        (mask, empty, arrays)
    }
}

fn local_palette(s: &Section) -> (Vec<BlockState>, Vec<u16>) {
    match s {
        Section::Uniform(b) => (vec![*b], vec![0; 4096]),
        Section::Full(arr) => {
            let mut palette: Vec<BlockState> = Vec::new();
            let mut lut: std::collections::HashMap<BlockState, u16> = std::collections::HashMap::new();
            let mut idx = vec![0u16; 4096];
            for (i, b) in arr.iter().enumerate() {
                let p = *lut.entry(*b).or_insert_with(|| {
                    palette.push(*b);
                    (palette.len() - 1) as u16
                });
                idx[i] = p;
            }
            (palette, idx)
        }
    }
}

fn biome_palette(b: &[u8; 64]) -> (Vec<u8>, Vec<u16>) {
    let mut palette: Vec<u8> = Vec::new();
    let mut idx = vec![0u16; 64];
    for (i, v) in b.iter().enumerate() {
        let p = match palette.iter().position(|p| p == v) {
            Some(p) => p,
            None => {
                palette.push(*v);
                palette.len() - 1
            }
        };
        idx[i] = p as u16;
    }
    (palette, idx)
}

fn write_varint(out: &mut Vec<u8>, v: u32) {
    crate::net::codec::write_varint(out, v as i32);
}

/// Paletted container (network). Single value → bits 0; small palette →
/// indirect; otherwise direct with global ids. No data length prefix (1.21.5+).
fn write_container(out: &mut Vec<u8>, palette: &[u32], idx: &[u16], min_bits: usize, max_bits: usize, global_bits: usize) {
    if palette.len() == 1 {
        out.push(0);
        write_varint(out, palette[0]);
        return;
    }
    let bits = bits_for(palette.len(), min_bits);
    let longs = if bits <= max_bits {
        out.push(bits as u8);
        write_varint(out, palette.len() as u32);
        for p in palette {
            write_varint(out, *p);
        }
        pack(idx, bits)
    } else {
        out.push(global_bits as u8);
        let direct: Vec<u16> = idx.iter().map(|i| palette[*i as usize] as u16).collect();
        pack(&direct, global_bits)
    };
    for l in longs {
        out.extend_from_slice(&l.to_be_bytes());
    }
}

pub fn bits_for(n: usize, min: usize) -> usize {
    let mut bits = 0;
    while (1usize << bits) < n {
        bits += 1;
    }
    bits.max(min)
}

pub fn pack(values: &[u16], bits: usize) -> Vec<i64> {
    let per_long = 64 / bits;
    let mut out = vec![0i64; values.len().div_ceil(per_long)];
    for (i, v) in values.iter().enumerate() {
        out[i / per_long] |= ((*v as u64) << ((i % per_long) * bits)) as i64;
    }
    out
}

fn unpack(data: &[i64], bits: usize, n: usize) -> Option<Vec<u16>> {
    let per_long = 64 / bits;
    if data.len() < n.div_ceil(per_long) {
        return None;
    }
    let mask = (1u64 << bits) - 1;
    Some((0..n).map(|i| (((data[i / per_long] as u64) >> ((i % per_long) * bits)) & mask) as u16).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nbt_roundtrip_with_properties_and_biomes() {
        let reg = BlockRegistry::get();
        let mut c = Chunk::empty(3, -9);
        let stairs = reg.state("oak_stairs", &[("facing", "east"), ("half", "top")]).unwrap();
        for x in 0..16 {
            for z in 0..16 {
                c.set(x, -64, z, blocks::BEDROCK);
                c.set(x, 10, z, if (x + z) % 3 == 0 { blocks::DIAMOND_ORE } else { blocks::STONE });
            }
        }
        c.set(5, 100, 5, stairs);
        c.biomes[5][7] = Biomes::get().id("minecraft:desert").unwrap();
        let back = Chunk::from_nbt(&c.to_nbt(0)).unwrap();
        assert_eq!((back.x, back.z), (3, -9));
        assert_eq!(back.get(5, 100, 5), stairs);
        assert_eq!(back.get(0, 10, 0), blocks::DIAMOND_ORE);
        assert_eq!(back.biomes[5][7], c.biomes[5][7]);
        assert_eq!(back.heightmap[5 * 16 + 5], 100);
    }

    #[test]
    fn packing_rules() {
        assert_eq!(bits_for(2, 4), 4);
        assert_eq!(bits_for(17, 4), 5);
        assert_eq!(pack(&vec![0; 4096], 5).len(), 342);
        assert_eq!(bits_for(blocks::STATE_COUNT as usize, 1), 15);
    }
}
