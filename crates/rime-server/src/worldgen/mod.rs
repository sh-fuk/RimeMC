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

thread_local! {
    /// `WorldGenerationContext` (min y, height) while a generator's data is
    /// loaded: vertical anchors resolve against it.
    static GEN_CTX: std::cell::Cell<(i32, i32)> = const { std::cell::Cell::new((MIN_Y, HEIGHT)) };
}

pub fn gen_min_y() -> i32 {
    GEN_CTX.with(|c| c.get().0)
}

pub fn gen_height() -> i32 {
    GEN_CTX.with(|c| c.get().1)
}

/// Which dimension a generator makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimKind {
    Overworld,
    Nether,
    End,
}

pub struct VanillaGenerator {
    pub dim: DimKind,
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

impl VanillaGenerator {
    /// Slow one-time setup (stronghold rings), so the first chunk near a
    /// player does not wait for it.
    pub fn warm_up(&self) {
        if let Some(s) = &self.structures {
            s.init_rings(self);
        }
    }

    /// Stronghold ring placements, once computed (for the world cache).
    pub fn rings(&self) -> Option<Vec<Vec<(i32, i32)>>> {
        let s = self.structures.as_ref()?;
        s.rings_ready().then(|| s.rings())
    }

    pub fn set_rings(&self, r: Vec<Vec<(i32, i32)>>) {
        if let Some(s) = &self.structures {
            s.set_rings(r);
        }
    }
}

/// The vanilla generator of each dimension (`Dim::index` order), for
/// game-time questions such as structure spawn lists.
static GENERATORS: parking_lot::RwLock<[Option<std::sync::Arc<VanillaGenerator>>; 3]> = parking_lot::RwLock::new([None, None, None]);

pub fn register_generator(g: std::sync::Arc<VanillaGenerator>) {
    let i = match g.dim {
        DimKind::Overworld => 0,
        DimKind::Nether => 1,
        DimKind::End => 2,
    };
    GENERATORS.write()[i] = Some(g);
}

pub fn generator_for(dim_index: usize) -> Option<std::sync::Arc<VanillaGenerator>> {
    GENERATORS.read().get(dim_index).cloned().flatten()
}

impl VanillaGenerator {
    /// Structure spawn overrides at `p` (see `Structures::mobs_at`).
    pub fn structure_mobs_at(&self, cat: &str, p: [i32; 3], on_nether_bricks: bool) -> Option<Vec<(String, u32, u32, u32)>> {
        self.structures.as_ref()?.mobs_at(self, cat, p, on_nether_bricks)
    }
}

impl structure::Ctx for VanillaGenerator {
    fn gen_range(&self) -> (i32, i32) {
        match self.dim {
            DimKind::Overworld => (MIN_Y, HEIGHT),
            _ => (0, 128),
        }
    }
    fn base_height(&self, x: i32, z: i32, hm: feature::Hm) -> i32 {
        chunk::base_height_stop(&self.router, &self.settings, self.aquifer_random, self.ore_random, x, z, &|s| hm.test(s))
    }
    fn base_column(&self, x: i32, z: i32) -> Vec<crate::registry::BlockState> {
        chunk::base_column(&self.router, &self.settings, self.aquifer_random, self.ore_random, x, z)
    }
    fn noise_biome(&self, qx: i32, qy: i32, qz: i32) -> u8 {
        self.noise_biome_uncached(qx, qy, qz)
    }
    fn temperature(&self, biome: u8, p: [i32; 3]) -> f32 {
        match &self.features {
            Some(l) => l.climates.get(biome as usize).map_or(0.8, |c| l.temps.temperature(*c, p[0], p[1], p[2], l.sea_level)),
            None => 0.8,
        }
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
    /// block entities of the final chunk
    pub block_entities: Vec<([i32; 3], crate::nbt::Tag)>,
    /// entities added by generation: (type, position)
    pub entities: Vec<(&'static str, [i32; 3])>,
}

impl VanillaGenerator {
    pub fn new(load: &Loader, seed: i64) -> Result<Self> {
        Self::new_dim(load, seed, DimKind::Overworld)
    }

    pub fn new_dim(load: &Loader, seed: i64, dim: DimKind) -> Result<Self> {
        let key = match dim {
            DimKind::Overworld => "overworld",
            DimKind::Nether => "nether",
            DimKind::End => "end",
        };
        let ns: Value = load(&format!("worldgen/noise_settings/{key}"))?;
        let legacy = ns["legacy_random_source"].as_bool().unwrap_or(false);
        let factory = if legacy { random::XoroshiroFactory::legacy_root(seed) } else { Xoroshiro::new(seed).fork_positional() };
        let shape = density::Shape::parse(&ns);
        // WorldGenerationContext: the dimension's build range clipped to the noise range
        let (dim_min, dim_h) = match dim {
            DimKind::Overworld => (MIN_Y, HEIGHT),
            _ => (0, 256),
        };
        let ctx_min = shape.min_y.max(dim_min);
        let ctx_h = (shape.min_y + shape.height).min(dim_min + dim_h) - ctx_min;
        GEN_CTX.with(|c| c.set((ctx_min, ctx_h)));
        let r = Self::build(load, seed, dim, &ns, factory, legacy);
        GEN_CTX.with(|c| c.set((MIN_Y, HEIGHT)));
        r
    }

    fn build(load: &Loader, seed: i64, dim: DimKind, ns: &Value, factory: random::XoroshiroFactory, legacy: bool) -> Result<Self> {
        let ns = ns.clone();
        let router = Router::new_dim(load, &ns, factory, legacy.then_some(seed))?;
        let surface = SurfaceSystem::new(load, &ns, factory, seed)?;
        let settings = Settings {
            sea_level: ns["sea_level"].as_i64().unwrap_or(63) as i32,
            default_block: surface.default_block,
            default_fluid: crate::registry::BlockRegistry::get().state(ns["default_fluid"]["Name"].as_str().unwrap_or("minecraft:water"), &[]).unwrap_or(crate::registry::blocks::WATER),
            aquifers: ns["aquifers_enabled"].as_bool().unwrap_or(true),
            ore_veins: ns["ore_veins_enabled"].as_bool().unwrap_or(true),
        };
        let params = match dim {
            DimKind::Overworld => biome_builder::overworld(),
            DimKind::Nether => biome_builder::nether(),
            DimKind::End => biome_builder::end(),
        };
        // every biome of a dimension uses the same carver list
        let first: Value = load(&format!("worldgen/biome/{}", params[0].1))?;
        let carver_names: Vec<&str> = match &first["carvers"] {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(|c| c.as_str()).collect(),
            _ => Vec::new(),
        };
        let carvers = carver_names
            .into_iter()
            .map(|c| carver::Carver::load(load, c))
            .collect::<Result<Vec<_>>>()?;
        let mut tree = BiomeTree::new(&params);
        tree.end = dim == DimKind::End;
        let names = crate::world::chunk::Biomes::get();
        let tree_ids = tree.values.iter().map(|b| names.id(&format!("minecraft:{b}")).unwrap_or(0)).collect();
        // possibleBiomes(): distinct biomes in parameter list order
        let mut possible: Vec<String> = Vec::new();
        for (_, b) in &params {
            let n = format!("minecraft:{b}");
            if !possible.contains(&n) {
                possible.push(n);
            }
        }
        let (mut lib, missing) = feature::Lib::new(load, &possible, surface::BiomeTemps::new(), surface.climates.clone(), settings.sea_level)?;
        if dim != DimKind::Overworld {
            lib.dim_min = 0;
            lib.dim_height = 256;
            lib.gen_depth = 128;
        }
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
            dim,
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
            aquifer.disabled = !self.settings.aquifers;
            let top = |x: i32, y: i32, z: i32, fluid: bool, h: &[i32; 256]| self.surface.top_material(&self.router, &bs, h, x, y, z, fluid);
            let mut ctx = carver::Carving { cx, cz, blocks: &mut blocks, heights: &mut heights, mask: vec![false; (16 * 16 * HEIGHT) as usize], aquifer: &mut aquifer, top: &top, marks: &mut marks };
            carver::apply(&self.carvers, self.seed, &mut ctx);
        }
        Generated { blocks, biomes, heights, fluid_ticks: marks, block_entities: Vec::new(), entities: Vec::new() }
    }
}
