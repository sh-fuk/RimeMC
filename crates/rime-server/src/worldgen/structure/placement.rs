//! `StructurePlacement`: which chunks may hold a structure of a set
//! (random spread with frequency reduction and exclusion zones).

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use anyhow::{bail, Result};
use serde_json::Value;

use crate::worldgen::random::{Legacy, Rng};

/// `WorldgenRandom.setLargeFeatureSeed` on a legacy random source.
pub fn large_feature_seed(seed: i64, cx: i32, cz: i32) -> Legacy {
    let mut r = Legacy::new(seed);
    let a = r.next_long();
    let b = r.next_long();
    Legacy::new((cx as i64).wrapping_mul(a) ^ (cz as i64).wrapping_mul(b) ^ seed)
}

/// `WorldgenRandom.setLargeFeatureWithSalt`
pub fn large_feature_with_salt(seed: i64, cx: i32, cz: i32, salt: i32) -> Legacy {
    Legacy::new((cx as i64).wrapping_mul(341873128712).wrapping_add((cz as i64).wrapping_mul(132897987541)).wrapping_add(seed).wrapping_add(salt as i64))
}

#[derive(Clone, Copy, Debug)]
pub enum Reduction {
    Default,
    Legacy1,
    Legacy2,
    Legacy3,
}

#[derive(Clone, Debug)]
pub enum Kind {
    RandomSpread { spacing: i32, separation: i32, triangular: bool },
    /// strongholds: chunk positions on rings around the origin
    ConcentricRings { distance: i32, spread: i32, count: i32, preferred: String },
}

#[derive(Clone, Debug)]
pub struct Placement {
    pub kind: Kind,
    pub salt: i32,
    pub frequency: f32,
    pub reduction: Reduction,
    /// (other structure set, chunk count)
    pub exclusion: Option<(String, i32)>,
    /// ring chunk positions (concentric rings), computed on first use
    pub rings: Arc<OnceLock<HashSet<(i32, i32)>>>,
}

impl Placement {
    pub fn parse(v: &Value) -> Result<Placement> {
        let ty = v["type"].as_str().unwrap_or("").trim_start_matches("minecraft:");
        let kind = match ty {
            "random_spread" => Kind::RandomSpread {
                spacing: v["spacing"].as_i64().unwrap_or(1) as i32,
                separation: v["separation"].as_i64().unwrap_or(0) as i32,
                triangular: v["spread_type"].as_str() == Some("triangular"),
            },
            "concentric_rings" => Kind::ConcentricRings {
                distance: v["distance"].as_i64().unwrap_or(32) as i32,
                spread: v["spread"].as_i64().unwrap_or(3) as i32,
                count: v["count"].as_i64().unwrap_or(128) as i32,
                preferred: v["preferred_biomes"].as_str().unwrap_or("").to_string(),
            },
            t => bail!("structure placement {t}"),
        };
        Ok(Placement {
            kind,
            salt: v["salt"].as_i64().unwrap_or(0) as i32,
            frequency: v["frequency"].as_f64().unwrap_or(1.0) as f32,
            reduction: match v["frequency_reduction_method"].as_str().unwrap_or("default") {
                "legacy_type_1" => Reduction::Legacy1,
                "legacy_type_2" => Reduction::Legacy2,
                "legacy_type_3" => Reduction::Legacy3,
                _ => Reduction::Default,
            },
            exclusion: v.get("exclusion_zone").map(|e| (e["other_set"].as_str().unwrap_or("").trim_start_matches("minecraft:").to_string(), e["chunk_count"].as_i64().unwrap_or(0) as i32)),
            rings: Arc::new(OnceLock::new()),
        })
    }

    /// `RandomSpreadStructurePlacement.getPotentialStructureChunk`
    pub fn potential_chunk(&self, seed: i64, cx: i32, cz: i32) -> Option<(i32, i32)> {
        let Kind::RandomSpread { spacing, separation, triangular } = self.kind else { return None };
        let i = cx.div_euclid(spacing);
        let j = cz.div_euclid(spacing);
        let mut r = large_feature_with_salt(seed, i, j, self.salt);
        let k = spacing - separation;
        let ev = |r: &mut Legacy| if triangular { (r.next_int_bound(k) + r.next_int_bound(k)) / 2 } else { r.next_int_bound(k) };
        let l = ev(&mut r);
        let m = ev(&mut r);
        Some((i * spacing + l, j * spacing + m))
    }

    /// `isPlacementChunk`
    fn placement_chunk(&self, seed: i64, cx: i32, cz: i32) -> bool {
        match self.kind {
            Kind::RandomSpread { .. } => self.potential_chunk(seed, cx, cz) == Some((cx, cz)),
            Kind::ConcentricRings { .. } => self.rings.get().is_some_and(|r| r.contains(&(cx, cz))),
        }
    }

    /// `applyAdditionalChunkRestrictions`
    fn frequency_ok(&self, seed: i64, cx: i32, cz: i32) -> bool {
        if self.frequency >= 1.0 {
            return true;
        }
        let f = self.frequency;
        match self.reduction {
            Reduction::Default => large_feature_with_salt(seed, cx, cz, self.salt).next_float() < f,
            Reduction::Legacy1 => {
                let (i, j) = (cx >> 4, cz >> 4);
                let mut r = Legacy::new(((i ^ (j << 4)) as i64) ^ seed);
                r.next_int();
                r.next_int_bound((1.0 / f) as i32) == 0
            }
            Reduction::Legacy2 => large_feature_with_salt(seed, cx, cz, 10387320).next_float() < f,
            Reduction::Legacy3 => large_feature_seed(seed, cx, cz).next_double() < f as f64,
        }
    }

    /// `isStructureChunk` (exclusion zones resolved through `other`)
    pub fn is_structure_chunk(&self, seed: i64, cx: i32, cz: i32, other: &dyn Fn(&str) -> Option<Placement>) -> bool {
        if !self.placement_chunk(seed, cx, cz) || !self.frequency_ok(seed, cx, cz) {
            return false;
        }
        if let Some((set, n)) = &self.exclusion {
            if let Some(p) = other(set) {
                for x in cx - n..=cx + n {
                    for z in cz - n..=cz + n {
                        if p.is_structure_chunk(seed, x, z, other) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }
}

/// `ChunkGeneratorStructureState.generateRingPositions`: `preferred` is the
/// biome id set of `preferred_biomes`, `noise_biome` the biome source.
pub fn ring_positions(seed: i64, distance: i32, spread: i32, count: i32, preferred: &[bool], noise_biome: &(dyn Fn(i32, i32, i32) -> u8 + Sync)) -> Vec<(i32, i32)> {
    if count == 0 {
        return Vec::new();
    }
    let mut random = Legacy::new(seed);
    let mut d = random.next_double() * std::f64::consts::PI * 2.0;
    let (mut i, mut j) = (0, 0);
    let mut spread = spread;
    let mut jobs = Vec::new();
    for k in 0..count {
        let e = (4 * distance + distance * j * 6) as f64 + (random.next_double() - 0.5) * distance as f64 * 2.5;
        let l = (d.cos() * e + 0.5).floor() as i64 as i32;
        let m = (d.sin() * e + 0.5).floor() as i64 as i32;
        let fork = Legacy::new(random.next_long());
        jobs.push((l, m, fork));
        d += std::f64::consts::PI * 2.0 / spread as f64;
        i += 1;
        if i == spread {
            j += 1;
            i = 0;
            spread += 2 * spread / (j + 1);
            spread = spread.min(count - k);
            d += random.next_double() * std::f64::consts::PI * 2.0;
        }
    }
    use rayon::prelude::*;
    jobs.into_par_iter()
        .map(|(l, m, mut r)| match find_biome_horizontal((l << 4) + 8, 0, (m << 4) + 8, 112, preferred, &mut r, noise_biome) {
            Some((x, z)) => (x >> 4, z >> 4),
            None => (l, m),
        })
        .collect()
}

/// `BiomeSource.findBiomeHorizontal(x, y, z, radius, 1, predicate, random, false)`
fn find_biome_horizontal(x: i32, y: i32, z: i32, radius: i32, ok: &[bool], r: &mut Legacy, noise_biome: &dyn Fn(i32, i32, i32) -> u8) -> Option<(i32, i32)> {
    let (i, j, k, l) = (x >> 2, z >> 2, radius >> 2, y >> 2);
    let mut result = None;
    let mut m = 0;
    let o = k;
    for p in -o..=o {
        for q in -o..=o {
            let (rx, sz) = (i + q, j + p);
            if ok[noise_biome(rx, l, sz) as usize] {
                if result.is_none() || r.next_int_bound(m + 1) == 0 {
                    result = Some((rx << 2, sz << 2));
                }
                m += 1;
            }
        }
    }
    result
}
