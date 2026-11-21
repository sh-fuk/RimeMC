//! Java → Bedrock tables (generated from GeyserMC/minecraft-data mappings,
//! see `assets/bedrock/`) and the Bedrock chunk encoding.
//!
//! Block runtime ids are the FNV-1a hashes of the Bedrock block state NBT,
//! so the StartGame packet sets `block_network_ids_are_hashes` and no
//! palette has to be agreed on.

use std::io::Read;
use std::sync::OnceLock;

use crate::registry::BlockState;
use crate::world::chunk::{Biomes, Chunk, Section, MIN_Y, SECTIONS};

use super::codec::W;

pub struct Tables {
    /// Bedrock block hash per Java block state id
    blocks: Vec<u32>,
    /// Bedrock biome id per Java biome id
    biomes: Vec<u32>,
    /// (name, id, component based, version)
    pub items: Vec<(String, i16, bool, i32)>,
}

const BIOME_ALIASES: &[(&str, &str)] = &[
    ("badlands", "mesa"),
    ("dark_forest", "roofed_forest"),
    ("end_barrens", "the_end"),
    ("end_highlands", "the_end"),
    ("end_midlands", "the_end"),
    ("small_end_islands", "the_end"),
    ("eroded_badlands", "mesa_bryce"),
    ("ice_spikes", "ice_plains_spikes"),
    ("mushroom_fields", "mushroom_island"),
    ("nether_wastes", "hell"),
    ("old_growth_birch_forest", "birch_forest_mutated"),
    ("old_growth_pine_taiga", "mega_taiga"),
    ("old_growth_spruce_taiga", "redwood_taiga_mutated"),
    ("snowy_beach", "cold_beach"),
    ("snowy_plains", "ice_plains"),
    ("snowy_taiga", "cold_taiga"),
    ("soul_sand_valley", "soulsand_valley"),
    ("sparse_jungle", "jungle_edge"),
    ("stony_shore", "stone_beach"),
    ("swamp", "swampland"),
    ("the_void", "plains"),
    ("windswept_forest", "extreme_hills_plus_trees"),
    ("windswept_gravelly_hills", "extreme_hills_mutated"),
    ("windswept_hills", "extreme_hills"),
    ("windswept_savanna", "savanna_mutated"),
    ("wooded_badlands", "mesa_plateau_stone"),
];

impl Tables {
    pub fn get() -> &'static Tables {
        static T: OnceLock<Tables> = OnceLock::new();
        T.get_or_init(|| {
            let mut raw = Vec::new();
            flate2::read::GzDecoder::new(&include_bytes!("../../../assets/bedrock/blocks.bin.gz")[..]).read_to_end(&mut raw).expect("bedrock block table");
            let blocks = raw.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
            let bedrock_biomes: std::collections::HashMap<&str, u32> = include_str!("../../../assets/bedrock/biomes.tsv")
                .lines()
                .filter_map(|l| {
                    let (n, id) = l.split_once('\t')?;
                    Some((n, id.parse().ok()?))
                })
                .collect();
            let plains = bedrock_biomes.get("plains").copied().unwrap_or(64);
            let biomes = Biomes::get()
                .names
                .iter()
                .map(|n| {
                    let n = n.trim_start_matches("minecraft:");
                    let n = BIOME_ALIASES.iter().find(|(j, _)| *j == n).map(|(_, b)| *b).unwrap_or(n);
                    bedrock_biomes.get(n).copied().unwrap_or(plains)
                })
                .collect();
            let items = include_str!("../../../assets/bedrock/items.tsv")
                .lines()
                .filter_map(|l| {
                    let mut f = l.split('\t');
                    Some((f.next()?.to_string(), f.next()?.parse().ok()?, f.next()? == "1", f.next()?.parse().ok()?))
                })
                .collect();
            Tables { blocks, biomes, items }
        })
    }

    pub fn block(&self, s: BlockState) -> u32 {
        self.blocks.get(s.0 as usize).copied().unwrap_or(self.blocks[0])
    }

    pub fn air(&self) -> u32 {
        self.blocks[0]
    }

    fn biome(&self, id: u8) -> u32 {
        self.biomes.get(id as usize).copied().unwrap_or(64)
    }
}

/// One paletted storage (4096 values, Bedrock XZY order) in network form.
fn storage(w: &mut W, values: &[u32; 4096]) {
    let mut palette: Vec<u32> = Vec::new();
    let mut idx = [0u16; 4096];
    for (i, v) in values.iter().enumerate() {
        let p = match palette.iter().position(|x| x == v) {
            Some(p) => p,
            None => {
                palette.push(*v);
                palette.len() - 1
            }
        };
        idx[i] = p as u16;
    }
    let bits: u32 = match palette.len() {
        1 => 0,
        2 => 1,
        3..=4 => 2,
        5..=8 => 3,
        9..=16 => 4,
        17..=32 => 5,
        33..=64 => 6,
        65..=256 => 8,
        _ => 16,
    };
    w.u8(((bits << 1) | 1) as u8);
    if bits == 0 {
        w.zz32(palette[0] as i32);
        return;
    }
    let per_word = 32 / bits;
    let words = 4096usize.div_ceil(per_word as usize);
    for wi in 0..words {
        let mut word = 0u32;
        for j in 0..per_word as usize {
            let i = wi * per_word as usize + j;
            if i < 4096 {
                word |= (idx[i] as u32) << (j as u32 * bits);
            }
        }
        w.u32le(word);
    }
    w.zz32(palette.len() as i32);
    for p in palette {
        w.zz32(p as i32);
    }
}

/// LevelChunk payload: sub-chunks from the bottom up to the highest
/// non-empty one, then one biome storage per sub-chunk of the dimension,
/// no border blocks, no block entities. Returns (sub-chunk count, payload).
pub fn chunk_payload(c: &Chunk) -> (u32, Vec<u8>) {
    let t = Tables::get();
    let air_state = BlockState(0);
    let top = c.sections.iter().rposition(|s| !matches!(s, Section::Uniform(b) if *b == air_state)).map(|i| i + 1).unwrap_or(0);
    let mut w = W(Vec::with_capacity(16 * 1024));
    let mut values = Box::new([0u32; 4096]);
    for (si, s) in c.sections.iter().take(top).enumerate() {
        w.u8(9).u8(1).u8((si as i32 + (MIN_Y >> 4)) as i8 as u8);
        match s {
            Section::Uniform(b) => {
                w.u8(1).zz32(t.block(*b) as i32);
            }
            Section::Full(_) => {
                for x in 0..16 {
                    for z in 0..16 {
                        for y in 0..16 {
                            values[(x << 8) | (z << 4) | y] = t.block(s.get(x, y, z));
                        }
                    }
                }
                storage(&mut w, &values);
            }
        }
    }
    for si in 0..SECTIONS {
        let cells = c.biomes.get(si).copied().unwrap_or([0; 64]);
        if cells.iter().all(|b| *b == cells[0]) {
            w.u8(1).zz32(t.biome(cells[0]) as i32);
            continue;
        }
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    values[(x << 8) | (z << 4) | y] = t.biome(cells[(y >> 2) << 4 | (z >> 2) << 2 | (x >> 2)]);
                }
            }
        }
        storage(&mut w, &values);
    }
    w.u8(0); // border blocks
    (top as u32, w.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::blocks;

    #[test]
    fn tables_load_and_map() {
        let t = Tables::get();
        assert_eq!(t.air(), 0xdbf4_4120, "minecraft:air hashes to -604749536");
        assert_ne!(t.block(blocks::STONE), t.air());
        assert_ne!(t.block(blocks::GRASS_BLOCK), t.block(blocks::STONE));
        assert!(t.items.iter().any(|i| i.0 == "minecraft:diamond_sword" || i.0 == "diamond_sword"));
        let plains = Biomes::plains();
        assert_eq!(t.biome(plains), 64, "bedrock plains");
    }

    #[test]
    fn chunk_encodes() {
        let mut c = Chunk::empty(0, 0);
        c.set(1, 0, 2, blocks::STONE);
        c.set(3, 70, 4, blocks::GRASS_BLOCK);
        let (count, p) = chunk_payload(&c);
        assert_eq!(count as i32, (70 - MIN_Y) / 16 + 1);
        assert!(p.len() > 24);
        assert_eq!(p[0], 9);
        assert_eq!(p[2] as i8, -4);
    }
}
