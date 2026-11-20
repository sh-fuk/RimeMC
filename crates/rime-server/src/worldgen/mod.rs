//! Vanilla-identical world generation (Minecraft 26.1 overworld).
//!
//! Everything is a bit-exact port of the vanilla algorithms, driven by the
//! vanilla data pack (density functions, noises, surface rules, biomes), so
//! the same seed produces the same terrain as a vanilla server.

pub mod biome;
pub mod biome_builder;
pub mod carver;
pub mod chunk;
pub mod decorate;
pub mod density;
pub mod feature;
pub mod noise;
pub mod random;
pub mod spawn;
pub mod surface;

#[cfg(test)]
mod tests;

use anyhow::Result;
use serde_json::Value;

use crate::registry::BlockState;
use biome::BiomeTree;
use chunk::{Aquifer, FluidPicker, NoiseChunk, Settings, HEIGHT, MIN_Y};
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
    carvers: Vec<carver::Carver>,
    features: Option<feature::Lib>,
    pipeline: parking_lot::Mutex<decorate::PipeState>,
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
        // every overworld biome uses the same carver list
        let plains: Value = load("worldgen/biome/plains")?;
        let carvers = plains["carvers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c.as_str())
            .map(|c| carver::Carver::load(load, c))
            .collect::<Result<Vec<_>>>()?;
        let tree = BiomeTree::new(&biome_builder::overworld());
        let names = crate::world::chunk::Biomes::get();
        let tree_ids = tree.values.iter().map(|b| names.id(&format!("minecraft:{b}")).unwrap_or(0)).collect();
        // possibleBiomes(): distinct biomes in parameter list order
        let mut possible: Vec<String> = Vec::new();
        for (_, b) in biome_builder::overworld() {
            let n = format!("minecraft:{b}");
            if !possible.contains(&n) {
                possible.push(n);
            }
        }
        let (lib, missing) = feature::Lib::new(load, &possible, surface::BiomeTemps::new(), surface.climates.clone(), settings.sea_level)?;
        if !missing.is_empty() {
            let list: Vec<String> = missing.iter().map(|(k, v)| format!("{k} x{v}")).collect();
            tracing::info!("feature types not ported yet (skipped): {}", list.join(", "));
        }
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
            carvers,
            features: Some(lib),
            pipeline: Default::default(),
        })
    }

    /// `BiomeSource.getNoiseBiome` for any quart position (network id).
    pub fn noise_biome_uncached(&self, qx: i32, qy: i32, qz: i32) -> u8 {
        let (cx, cz) = (qx >> 2, qz >> 2);
        let raw = match self.biome_cache.get(cx, cz) {
            Some(b) => b,
            None => {
                let nc = NoiseChunk::new(&self.router.graph, cx, cz);
                let b = ChunkBiomes::new(biome::chunk_biomes(&self.router, &nc, &self.tree, cx, cz));
                self.biome_cache.insert(cx, cz, b.clone());
                b
            }
        };
        let qy = qy.clamp(MIN_Y >> 2, ((MIN_Y + HEIGHT) >> 2) - 1);
        let sec = &raw[((qy - (MIN_Y >> 2)) >> 2) as usize];
        self.tree_ids[sec[(((qy & 3) << 4) | ((qz & 3) << 2) | (qx & 3)) as usize] as usize]
    }

    /// noise + biomes + surface + carvers: a proto chunk before features.
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
        if !self.carvers.is_empty() {
            let picker = FluidPicker::overworld(self.settings.sea_level, self.settings.default_fluid);
            let mut aquifer = Aquifer::new(&nc, &self.router, self.aquifer_random, &picker, MIN_Y, HEIGHT);
            let top = |x: i32, y: i32, z: i32, fluid: bool, h: &[i32; 256]| self.surface.top_material(&self.router, &bs, h, x, y, z, fluid);
            let mut ctx = carver::Carving { cx, cz, blocks: &mut blocks, heights: &mut heights, mask: vec![false; (16 * 16 * HEIGHT) as usize], aquifer: &mut aquifer, top: &top };
            carver::apply(&self.carvers, self.seed, &mut ctx);
        }
        Generated { blocks, biomes, heights }
    }
}
