//! Feature decoration across chunk borders.
//!
//! Vanilla decorates chunk C inside a `WorldGenRegion` that can write to the
//! 3x3 chunks around C, so a chunk is final only once all of its 3x3
//! neighbours have been decorated. Proto chunks (noise + surface + carvers)
//! stay in a cache until then. Where the write regions of two chunks overlap
//! the later one wins, so the order matters (vanilla itself varies here from
//! run to run); see `waves`.

use std::collections::{HashMap, HashSet};

use rayon::prelude::*;

use super::feature::{Hm, Lib, Proto, Region};
use super::{Generated, VanillaGenerator};

/// Protos kept before clean / finished ones are evicted.
const MAX_PROTOS: usize = 4096;

#[derive(Default)]
pub struct PipeState {
    protos: HashMap<(i32, i32), Box<Proto>>,
    decorated: HashSet<(i32, i32)>,
}

fn around(c: (i32, i32)) -> impl Iterator<Item = (i32, i32)> {
    (0..9).map(move |i| (c.0 + i / 3 - 1, c.1 + i % 3 - 1))
}

impl PipeState {
    fn done(&self, c: (i32, i32)) -> bool {
        around(c).all(|n| self.decorated.contains(&n))
    }
    fn dirty(&self, c: (i32, i32)) -> bool {
        around(c).any(|n| self.decorated.contains(&n))
    }
}

impl VanillaGenerator {
    fn proto(&self, cx: i32, cz: i32) -> Box<Proto> {
        let g = self.generate(cx, cz);
        Box::new(Proto::new(g.blocks, g.biomes))
    }

    /// Fully generated chunks (with features), in the order of `coords`.
    pub fn generate_many(&self, coords: &[(i32, i32)]) -> Vec<Generated> {
        let Some(lib) = &self.features else {
            return coords.par_iter().map(|&(x, z)| self.generate(x, z)).collect();
        };
        let mut guard = self.pipeline.lock();
        let st = &mut *guard;
        let mut want: Vec<(i32, i32)> = coords.to_vec();
        want.sort_unstable();
        want.dedup();
        // finished chunks whose proto was evicted (asked for again): rebuild
        // them in a fresh state rather than decorating their neighbours twice
        let (stale, live): (Vec<_>, Vec<_>) = want.into_iter().partition(|c| !st.protos.contains_key(c) && st.done(*c));
        let mut out: HashMap<(i32, i32), Generated> = HashMap::new();
        if !stale.is_empty() {
            let mut fresh = PipeState::default();
            self.run(lib, &mut fresh, &stale, &mut out);
        }
        self.run(lib, st, &live, &mut out);
        if st.protos.len() > MAX_PROTOS {
            let drop: Vec<_> = st.protos.keys().copied().filter(|&c| !st.dirty(c) || st.done(c)).collect();
            for c in drop {
                st.protos.remove(&c);
            }
        }
        drop(guard);
        coords.iter().map(|c| out.get(c).map(clone_gen).expect("generated")).collect()
    }

    fn run(&self, lib: &Lib, st: &mut PipeState, want: &[(i32, i32)], out: &mut HashMap<(i32, i32), Generated>) {
        let mut dec: Vec<(i32, i32)> = want.iter().flat_map(|&c| around(c)).filter(|n| !st.decorated.contains(n)).collect();
        dec.sort_unstable();
        dec.dedup();
        let mut need: Vec<(i32, i32)> = dec.iter().flat_map(|&c| around(c)).chain(want.iter().copied()).filter(|n| !st.protos.contains_key(n)).collect();
        need.sort_unstable();
        need.dedup();
        let made: Vec<_> = need.into_par_iter().map(|c| (c, self.proto(c.0, c.1))).collect();
        st.protos.extend(made);
        let outside = |qx: i32, qy: i32, qz: i32| self.noise_biome_uncached(qx, qy, qz);
        let zoom_seed = self.surface.zoom_seed;
        for batch in waves(&dec) {
            let mut regions: Vec<Region> = batch
                .iter()
                .map(|&(cx, cz)| Region {
                    cx,
                    cz,
                    chunks: around((cx, cz)).map(|n| st.protos.remove(&n).expect("proto")).collect(),
                    lib,
                    zoom_seed,
                    outside: &outside,
                })
                .collect();
            regions.par_iter_mut().for_each(|r| lib.decorate(r, self.seed));
            for r in regions {
                for (n, p) in around((r.cx, r.cz)).zip(r.chunks) {
                    st.protos.insert(n, p);
                }
            }
            st.decorated.extend(batch);
        }
        for &c in want {
            let p = &st.protos[&c];
            out.insert(c, Generated { blocks: p.blocks.clone(), biomes: p.biomes.clone(), heights: p.hm[Hm::NotAir as usize] });
        }
    }
}

/// Splits `dec` (sorted x-major, the order a vanilla server decorates a
/// forceloaded area in) into waves of chunks whose 3x3 write regions do not
/// overlap. Decorating wave by wave gives exactly the result of decorating
/// one chunk after another in that order, but each wave runs in parallel.
fn waves(dec: &[(i32, i32)]) -> Vec<Vec<(i32, i32)>> {
    let mut level: HashMap<(i32, i32), usize> = HashMap::new();
    let mut out: Vec<Vec<(i32, i32)>> = Vec::new();
    for &c in dec {
        let mut l = 0;
        for dx in -2..=2 {
            for dz in -2..=2 {
                if let Some(&p) = level.get(&(c.0 + dx, c.1 + dz)) {
                    l = l.max(p + 1);
                }
            }
        }
        level.insert(c, l);
        if out.len() <= l {
            out.push(Vec::new());
        }
        out[l].push(c);
    }
    out
}

fn clone_gen(g: &Generated) -> Generated {
    Generated { blocks: g.blocks.clone(), biomes: g.biomes.clone(), heights: g.heights }
}
