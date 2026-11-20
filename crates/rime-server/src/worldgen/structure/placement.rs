//! `StructurePlacement`: which chunks may hold a structure of a set
//! (random spread with frequency reduction and exclusion zones).

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
    /// strongholds (not ported yet: never placed)
    ConcentricRings,
}

#[derive(Clone, Debug)]
pub struct Placement {
    pub kind: Kind,
    pub salt: i32,
    pub frequency: f32,
    pub reduction: Reduction,
    /// (other structure set, chunk count)
    pub exclusion: Option<(String, i32)>,
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
            "concentric_rings" => Kind::ConcentricRings,
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
        })
    }

    /// `RandomSpreadStructurePlacement.getPotentialStructureChunk`
    pub fn potential_chunk(&self, seed: i64, cx: i32, cz: i32) -> Option<(i32, i32)> {
        let Kind::RandomSpread { spacing, separation, triangular } = self.kind else { return None };
        let i = cx.div_euclid(spacing);
        let j = cz.div_euclid(spacing);
        let mut r = large_feature_with_salt(seed, i, j, self.salt);
        let k = spacing - separation;
        let mut ev = |r: &mut Legacy| if triangular { (r.next_int_bound(k) + r.next_int_bound(k)) / 2 } else { r.next_int_bound(k) };
        let l = ev(&mut r);
        let m = ev(&mut r);
        Some((i * spacing + l, j * spacing + m))
    }

    /// `isPlacementChunk`
    fn placement_chunk(&self, seed: i64, cx: i32, cz: i32) -> bool {
        match self.kind {
            Kind::RandomSpread { .. } => self.potential_chunk(seed, cx, cz) == Some((cx, cz)),
            Kind::ConcentricRings => false,
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
