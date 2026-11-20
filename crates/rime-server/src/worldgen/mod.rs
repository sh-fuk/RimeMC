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
pub mod structure;
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
    structures: Option<structure::Structures>,
    pipeline: parking_lot::Mutex<decorate::PipeState>,
}

/// Structure registry (sorted by id, like a data pack registry).
const STRUCTURES: [&str; 34] = [
    "ancient_city", "bastion_remnant", "buried_treasure", "desert_pyramid", "end_city", "fortress", "igloo", "jungle_pyramid", "mansion", "mineshaft", "mineshaft_mesa", "monument",
    "nether_fossil", "ocean_ruin_cold", "ocean_ruin_warm", "pillager_outpost", "ruined_portal", "ruined_portal_desert", "ruined_portal_jungle", "ruined_portal_mountain",
    "ruined_portal_nether", "ruined_portal_ocean", "ruined_portal_swamp", "shipwreck", "shipwreck_beached", "stronghold", "swamp_hut", "trail_ruins", "trial_chambers",
    "village_desert", "village_plains", "village_savanna", "village_snowy", "village_taiga",
];
const STRUCTURE_SETS: [&str; 20] = [
    "ancient_cities", "buried_treasures", "desert_pyramids", "end_cities", "igloos", "jungle_temples", "mineshafts", "nether_complexes", "nether_fossils", "ocean_monuments",
    "ocean_ruins", "pillager_outposts", "ruined_portals", "shipwrecks", "strongholds", "swamp_huts", "trail_ruins", "trial_chambers", "villages", "woodland_mansions",
];

impl structure::Ctx for VanillaGenerator {
    fn base_height(&self, x: i32, z: i32, hm: feature::Hm) -> i32 {
        chunk::base_height(&self.router, &self.settings, self.aquifer_random, self.ore_random, x, z, &|s| hm.test(s))
    }
    fn noise_biome(&self, qx: i32, qy: i32, qz: i32) -> u8 {
        self.noise_biome_uncached(qx, qy, qz)
    }
}

/// A generated chunk: blocks `((y - MIN_Y) * 16 + z) * 16 + x`, biome ids per
/// section `[(qy << 4) | (qz << 2) | qx]` and the WORLD_SURFACE_WG heights.
pub struct Generated {
    pub blocks: Vec<BlockState>,
    pub biomes: Vec<[u8; 64]>,
    pub heights: [i32; 256],
    /// Before decoration: positions marked for post processing by the noise
    /// fill and carvers. Final chunks: fluid blocks that get a fluid tick
    /// when the chunk is loaded (world coordinates).
    pub fluid_ticks: Vec<[i32; 3]>,
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
        let possible_ids: Vec<u8> = possible.iter().filter_map(|b| names.id(b)).collect();
        let mut sb = feature::Builder::new(load);
        let (structures, unported) = structure::Structures::new(&mut sb, seed, &STRUCTURES.map(String::from), &STRUCTURE_SETS.map(String::from), &possible_ids)?;
        if !unported.is_empty() {
            if cfg!(test) { eprintln!("structures not ported: {}", unported.join(", ")); }
            tracing::info!("structure types not ported yet (skipped): {}", unported.join(", "));
        }
        if !missing.is_empty() {
            let list: Vec<String> = missing.iter().map(|(k, v)| format!("{k} x{v}")).collect();
            if cfg!(test) { eprintln!("missing feature types: {}", list.join(", ")); }
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
            structures: if std::env::var("RIME_NOSTRUCT").is_ok() { None } else { Some(structures) },
            pipeline: Default::default(),
        })
    }

    /// Structure starts in a chunk range: (cx, cz, structure, pieces, box).
    pub fn structure_starts(&self, x0: i32, z0: i32, x1: i32, z1: i32) -> Vec<(i32, i32, String, usize, structure::Bb)> {
        let Some(s) = &self.structures else { return Vec::new() };
        let mut out = Vec::new();
        for cx in x0..=x1 {
            for cz in z0..=z1 {
                for (name, n, bb) in s.starts_info(self, cx, cz) {
                    out.push((cx, cz, name, n, bb));
                }
            }
        }
        out
    }

    /// Starts in a chunk with their piece boxes and templates.
    pub fn structure_pieces(&self, cx: i32, cz: i32) -> Vec<(String, Vec<structure::Bb>, Vec<String>)> {
        self.structures.as_ref().map(|s| s.pieces_info(self, cx, cz)).unwrap_or_default()
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
        let mut marks = Vec::new();
        let beard = self.structures.as_ref().and_then(|s| s.beardifier(self, cx, cz));
        let beard_fn = beard.as_ref().map(|b| move |x: i32, y: i32, z: i32| b.compute(x, y, z));
        let mut blocks = chunk::fill_with(&nc, &self.router, &self.settings, self.aquifer_random, self.ore_random, &mut marks, beard_fn.as_ref().map(|f| f as &dyn Fn(i32, i32, i32) -> f64));
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
            let mut ctx = carver::Carving { cx, cz, blocks: &mut blocks, heights: &mut heights, mask: vec![false; (16 * 16 * HEIGHT) as usize], aquifer: &mut aquifer, top: &top, marks: &mut marks };
            carver::apply(&self.carvers, self.seed, &mut ctx);
        }
        Generated { blocks, biomes, heights, fluid_ticks: marks }
    }
}
