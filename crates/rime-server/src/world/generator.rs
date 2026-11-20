//! Terrain generator.
//!
//! The height field uses **integer-only** value noise (wrapping u32 hashes,
//! fixed-point interpolation). Integer arithmetic is bit-exact on every CPU and
//! GPU, so the GPU path ([`super::gpu`]) produces exactly the same world as the
//! CPU path — the GPU is a pure accelerator, never a source of divergence.

use rayon::prelude::*;

use super::chunk::{Biomes, Chunk, MIN_Y};
use crate::registry::{blocks, BlockState};

pub const SEA_LEVEL: i32 = 63;

/// The WGSL version of `hash2`/`value_noise`/`column_height` lives in
/// `gpu_heights.wgsl` and must stay in sync with this code (tested).
#[inline]
pub fn hash2(x: i32, z: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x27d4_eb2d) ^ (z as u32).wrapping_mul(0x1656_67b1) ^ seed;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    h
}

/// Smoothstep in 8.8 fixed point: t ∈ [0,256] → [0,256].
#[inline]
fn smooth(t: i32) -> i32 {
    (t * t * (768 - 2 * t)) >> 16
}

/// Value noise in [-32768, 32767] with lattice spacing 2^shift.
#[inline]
pub fn value_noise(x: i32, z: i32, shift: u32, seed: u32) -> i32 {
    let ix = x >> shift;
    let iz = z >> shift;
    let mask = (1 << shift) - 1;
    // fraction scaled to 0..256
    let fx = smooth(((x & mask) << 8) >> shift);
    let fz = smooth(((z & mask) << 8) >> shift);
    let v = |dx: i32, dz: i32| (hash2(ix + dx, iz + dz, seed) & 0xFFFF) as i32 - 32768;
    let a = v(0, 0) + (((v(1, 0) - v(0, 0)) * fx) >> 8);
    let b = v(0, 1) + (((v(1, 1) - v(0, 1)) * fx) >> 8);
    a + (((b - a) * fz) >> 8)
}

/// Terrain surface height for a block column.
#[inline]
pub fn column_height(x: i32, z: i32, seed: u32) -> i32 {
    let continent = (value_noise(x, z, 9, seed ^ 0xA5A5_A5A5) * 36) >> 15;
    let hills = (value_noise(x, z, 7, seed) * 22) >> 15;
    let mid = (value_noise(x, z, 5, seed.wrapping_add(1)) * 8) >> 15;
    let detail = (value_noise(x, z, 3, seed.wrapping_add(2)) * 3) >> 15;
    (64 + continent + hills + mid + detail).clamp(MIN_Y + 8, 250)
}

/// Heights for one chunk (index = z*16 + x), CPU path.
pub fn chunk_heights_cpu(cx: i32, cz: i32, seed: u32) -> [i32; 256] {
    let mut h = [0i32; 256];
    for z in 0..16 {
        for x in 0..16 {
            h[z * 16 + x] = column_height(cx * 16 + x as i32, cz * 16 + z as i32, seed);
        }
    }
    h
}

struct OreSpec {
    stone: BlockState,
    deep: BlockState,
    tries: u32,
    min_y: i32,
    max_y: i32,
    size: u32,
}

const ORES: [OreSpec; 8] = [
    OreSpec { stone: blocks::COAL_ORE, deep: blocks::DEEPSLATE_COAL_ORE, tries: 20, min_y: 0, max_y: 192, size: 12 },
    OreSpec { stone: blocks::IRON_ORE, deep: blocks::DEEPSLATE_IRON_ORE, tries: 12, min_y: -64, max_y: 72, size: 8 },
    OreSpec { stone: blocks::COPPER_ORE, deep: blocks::DEEPSLATE_COPPER_ORE, tries: 8, min_y: -16, max_y: 112, size: 9 },
    OreSpec { stone: blocks::GOLD_ORE, deep: blocks::DEEPSLATE_GOLD_ORE, tries: 4, min_y: -64, max_y: 32, size: 7 },
    OreSpec { stone: blocks::REDSTONE_ORE, deep: blocks::DEEPSLATE_REDSTONE_ORE, tries: 6, min_y: -64, max_y: 16, size: 7 },
    OreSpec { stone: blocks::LAPIS_ORE, deep: blocks::DEEPSLATE_LAPIS_ORE, tries: 2, min_y: -64, max_y: 64, size: 6 },
    OreSpec { stone: blocks::DIAMOND_ORE, deep: blocks::DEEPSLATE_DIAMOND_ORE, tries: 3, min_y: -64, max_y: 16, size: 5 },
    OreSpec { stone: blocks::EMERALD_ORE, deep: blocks::DEEPSLATE_EMERALD_ORE, tries: 1, min_y: 0, max_y: 160, size: 1 },
];

/// Build a full chunk from its height field (CPU, cheap compared to noise).
pub fn build_chunk(cx: i32, cz: i32, seed: u32, heights: &[i32; 256]) -> Chunk {
    let mut c = Chunk::empty(cx, cz);
    for z in 0..16usize {
        for x in 0..16usize {
            let h = heights[z * 16 + x];
            let wx = cx * 16 + x as i32;
            let wz = cz * 16 + z as i32;
            let r = hash2(wx, wz, seed ^ 0x5bd1_e995);
            for y in MIN_Y..=h.max(SEA_LEVEL - 1) {
                let b = if y == MIN_Y || (y < MIN_Y + 5 && (r >> ((y - MIN_Y) as u32 * 3)) & 7 < (5 - (y - MIN_Y)) as u32) {
                    blocks::BEDROCK
                } else if y > h {
                    blocks::WATER
                } else if y < 0 {
                    blocks::DEEPSLATE
                } else if y < h - 3 {
                    blocks::STONE
                } else if h <= SEA_LEVEL + 1 {
                    if y < h - 1 && h < SEA_LEVEL - 4 { blocks::GRAVEL } else if y < h - 2 { blocks::SANDSTONE } else { blocks::SAND }
                } else if y < h {
                    blocks::DIRT
                } else {
                    blocks::GRASS_BLOCK
                };
                c.set(x, y, z, b);
            }
            c.heightmap[z * 16 + x] = h.max(SEA_LEVEL - 1) as i16; // water surface counts
        }
    }
    let b = Biomes::get();
    let (ocean, beach, plains) = (b.id("minecraft:ocean").unwrap_or(0), b.id("minecraft:beach").unwrap_or(0), Biomes::plains());
    for bz in 0..4 {
        for bx in 0..4 {
            let h = heights[(bz * 4 + 2) * 16 + bx * 4 + 2];
            let id = if h < SEA_LEVEL - 4 {
                ocean
            } else if h <= SEA_LEVEL + 1 {
                beach
            } else {
                plains
            };
            for sec in c.biomes.iter_mut() {
                for by in 0..4 {
                    sec[(by << 4) | (bz << 2) | bx] = id;
                }
            }
        }
    }
    // Ores: deterministic per-chunk blobs, replacing only stone/deepslate.
    for (oi, ore) in ORES.iter().enumerate() {
        for t in 0..ore.tries {
            let h = hash2(cx.wrapping_mul(31).wrapping_add(t as i32), cz.wrapping_mul(17).wrapping_add(oi as i32), seed ^ 0x0BE5);
            let (ox, oz) = ((h & 15) as i32, ((h >> 4) & 15) as i32);
            let span = (ore.max_y - ore.min_y).max(1) as u32;
            let oy = ore.min_y + ((h >> 8) % span) as i32;
            let mut s = h;
            for _ in 0..ore.size {
                s = hash2(s as i32, oy, seed);
                let x = (ox + (s & 3) as i32 - 1).clamp(0, 15) as usize;
                let y = oy + ((s >> 2) & 3) as i32 - 1;
                let z = (oz + ((s >> 4) & 3) as i32 - 1).clamp(0, 15) as usize;
                match c.get(x, y, z) {
                    blocks::STONE => c.set(x, y, z, ore.stone),
                    blocks::DEEPSLATE => c.set(x, y, z, ore.deep),
                    _ => {}
                }
            }
        }
    }
    for s in &mut c.sections {
        s.optimize();
    }
    c
}

/// Generator front-end: batches go to the GPU when available, else to all CPU cores.
pub struct Generator {
    pub seed: u32,
    /// vanilla-identical generator (preferred when set)
    vanilla: Option<std::sync::Arc<crate::worldgen::VanillaGenerator>>,
    #[cfg(feature = "gpu")]
    gpu: Option<super::gpu::GpuHeights>,
    min_gpu_batch: usize,
}

impl Generator {
    pub fn new(seed: i64, use_gpu: bool, min_gpu_batch: usize) -> Self {
        let seed = (seed as u64 ^ (seed as u64 >> 32)) as u32;
        #[cfg(feature = "gpu")]
        {
            let gpu = if use_gpu { super::gpu::GpuHeights::new() } else { None };
            match &gpu {
                Some(g) => tracing::info!("GPU world generation enabled on {}", g.adapter_name),
                None if use_gpu => tracing::info!("no usable GPU found; world generation runs on all CPU cores"),
                None => {}
            }
            Self { seed, vanilla: None, gpu, min_gpu_batch }
        }
        #[cfg(not(feature = "gpu"))]
        {
            if use_gpu {
                tracing::info!("built without the `gpu` feature; world generation runs on all CPU cores");
            }
            Self { seed, vanilla: None, min_gpu_batch }
        }
    }

    pub fn with_vanilla(mut self, v: crate::worldgen::VanillaGenerator) -> Self {
        self.vanilla = Some(std::sync::Arc::new(v));
        self
    }

    pub fn is_vanilla(&self) -> bool {
        self.vanilla.is_some()
    }

    pub fn gpu_active(&self) -> bool {
        #[cfg(feature = "gpu")]
        {
            self.gpu.is_some()
        }
        #[cfg(not(feature = "gpu"))]
        {
            false
        }
    }

    /// Generate many chunks at once (parallel / GPU-batched).
    pub fn generate_batch(&self, coords: &[(i32, i32)]) -> Vec<Chunk> {
        if let Some(v) = &self.vanilla {
            return coords.par_iter().map(|&(x, z)| vanilla_chunk(v, x, z)).collect();
        }
        let heights: Vec<[i32; 256]> = self.heights_batch(coords);
        coords.par_iter().zip(heights.par_iter()).map(|(&(x, z), h)| build_chunk(x, z, self.seed, h)).collect()
    }

    fn heights_batch(&self, coords: &[(i32, i32)]) -> Vec<[i32; 256]> {
        #[cfg(feature = "gpu")]
        if coords.len() >= self.min_gpu_batch {
            if let Some(g) = &self.gpu {
                match g.heights(coords, self.seed) {
                    Ok(h) => return h,
                    Err(e) => tracing::warn!("GPU generation failed ({e}); falling back to CPU for this batch"),
                }
            }
        }
        let _ = self.min_gpu_batch;
        coords.par_iter().map(|&(x, z)| chunk_heights_cpu(x, z, self.seed)).collect()
    }
}

fn vanilla_chunk(v: &crate::worldgen::VanillaGenerator, cx: i32, cz: i32) -> Chunk {
    let g = v.generate(cx, cz);
    let mut c = Chunk::empty(cx, cz);
    for (i, &b) in g.blocks.iter().enumerate() {
        if !b.is_air() {
            let i = i as i32;
            c.set((i & 15) as usize, MIN_Y + (i >> 8), ((i >> 4) & 15) as usize, b);
        }
    }
    for (dst, src) in c.biomes.iter_mut().zip(&g.biomes) {
        *dst = *src;
    }
    for (h, &g) in c.heightmap.iter_mut().zip(&g.heights) {
        *h = g as i16;
    }
    for s in &mut c.sections {
        s.optimize();
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_sane() {
        let a = build_chunk(5, -7, 42, &chunk_heights_cpu(5, -7, 42));
        let b = build_chunk(5, -7, 42, &chunk_heights_cpu(5, -7, 42));
        assert_eq!(a.to_nbt(0), b.to_nbt(0));
        assert_eq!(a.get(0, MIN_Y, 0), blocks::BEDROCK);
        for x in -2000..2000 {
            let h = column_height(x * 7, x * -13, 1);
            assert!((MIN_Y + 8..=250).contains(&h));
        }
    }

    #[test]
    fn noise_is_continuous_across_negative_coords() {
        for x in -40..40 {
            let d = (column_height(x, 3, 9) - column_height(x + 1, 3, 9)).abs();
            assert!(d <= 3, "jump at {x}: {d}");
        }
    }

    #[test]
    fn has_ores() {
        let ores: usize = (0..16)
            .map(|i| {
                let c = build_chunk(i, i, 7, &chunk_heights_cpu(i, i, 7));
                let mut n = 0;
                for y in -64..100 {
                    for x in 0..16 {
                        for z in 0..16 {
                            n += c.get(x, y, z).is_ore() as usize;
                        }
                    }
                }
                n
            })
            .sum();
        assert!(ores > 100);
    }
}
