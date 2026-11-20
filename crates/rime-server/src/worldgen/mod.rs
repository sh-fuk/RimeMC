//! Vanilla-identical world generation (Minecraft 26.1 overworld).
//!
//! Everything is a bit-exact port of the vanilla algorithms, driven by the
//! vanilla data pack (density functions, noises, surface rules, biomes), so
//! the same seed produces the same terrain as a vanilla server.

pub mod biome;
pub mod biome_builder;
pub mod chunk;
pub mod density;
pub mod noise;
pub mod random;
pub mod surface;

#[cfg(test)]
mod tests;

use anyhow::Result;
use serde_json::Value;

use crate::registry::BlockState;
use biome::BiomeTree;
use chunk::{NoiseChunk, Settings, HEIGHT, MIN_Y};
use density::{Loader, Router};
use random::{Xoroshiro, XoroshiroFactory};
use surface::{BiomeCache, BiomeSource, ChunkBiomes, SurfaceSystem};

pub struct VanillaGenerator {
    pub seed: i64,
    pub router: Router,
    pub settings: Settings,
    tree: BiomeTree,
    /// tree value index -> biome network id
    tree_ids: Vec<u8>,
    surface: SurfaceSystem,
    aquifer_random: XoroshiroFactory,
    ore_random: XoroshiroFactory,
    biome_cache: BiomeCache,
}

/// A generated chunk: blocks `((y - MIN_Y) * 16 + z) * 16 + x`, biome ids per
/// section `[(qy << 4) | (qz << 2) | qx]` and the WORLD_SURFACE_WG heights.
pub struct Generated {
    pub blocks: Vec<BlockState>,
    pub biomes: Vec<[u8; 64]>,
    pub heights: [i32; 256],
}

impl VanillaGenerator {
    pub fn new(load: &Loader, seed: i64) -> Result<Self> {
        let ns: Value = load("worldgen/noise_settings/overworld")?;
        let factory = Xoroshiro::new(seed).fork_positional();
        let router = Router::new(load, &ns, factory)?;
        let surface = SurfaceSystem::new(load, &ns, factory, seed)?;
        let settings = Settings {
            sea_level: ns["sea_level"].as_i64().unwrap_or(63) as i32,
            default_block: surface.default_block,
            default_fluid: crate::registry::BlockRegistry::get().state(ns["default_fluid"]["Name"].as_str().unwrap_or("minecraft:water"), &[]).unwrap_or(crate::registry::blocks::WATER),
            aquifers: ns["aquifers_enabled"].as_bool().unwrap_or(true),
            ore_veins: ns["ore_veins_enabled"].as_bool().unwrap_or(true),
        };
        let tree = BiomeTree::new(&biome_builder::overworld());
        let names = crate::world::chunk::Biomes::get();
        let tree_ids = tree.values.iter().map(|b| names.id(&format!("minecraft:{b}")).unwrap_or(0)).collect();
        Ok(VanillaGenerator {
            seed,
            router,
            settings,
            tree,
            tree_ids,
            surface,
            aquifer_random: factory.from_hash_of("minecraft:aquifer").fork_positional(),
            ore_random: factory.from_hash_of("minecraft:ore").fork_positional(),
            biome_cache: BiomeCache::default(),
        })
    }

    /// noise + biomes + surface (no carvers / features / structures yet).
    pub fn generate(&self, cx: i32, cz: i32) -> Generated {
        let nc = NoiseChunk::new(&self.router.graph, cx, cz);
        let mut blocks = chunk::fill_with(&nc, &self.router, &self.settings, self.aquifer_random, self.ore_random);
        let raw = match self.biome_cache.get(cx, cz) {
            Some(b) => b,
            None => {
                let b = ChunkBiomes::new(biome::chunk_biomes(&self.router, &nc, &self.tree, cx, cz));
                self.biome_cache.insert(cx, cz, b.clone());
                b
            }
        };
        let biomes = raw.iter().map(|s| s.map(|i| self.tree_ids[i as usize])).collect();
        let mut heights = [MIN_Y - 1; 256];
        for z in 0..16 {
            for x in 0..16 {
                for y in (MIN_Y..MIN_Y + HEIGHT).rev() {
                    if !blocks[(((y - MIN_Y) * 16 + z) * 16 + x) as usize].is_air() {
                        heights[(z * 16 + x) as usize] = y;
                        break;
                    }
                }
            }
        }
        let bs = BiomeSource::new(&self.router, &self.tree, &nc, &self.tree_ids, &self.biome_cache);
        self.surface.build(&self.router, &bs, cx, cz, &mut blocks, &mut heights);
        Generated { blocks, biomes, heights }
    }
}
