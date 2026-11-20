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
use super::chunk::HEIGHT;
use super::{Generated, VanillaGenerator};
use crate::registry::{BlockRegistry, BlockState};

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

fn around2(c: (i32, i32)) -> impl Iterator<Item = (i32, i32)> {
    (0..25).map(move |i| (c.0 + i / 5 - 2, c.1 + i % 5 - 2))
}

impl PipeState {
    /// the chunk and all its neighbours are final
    fn done(&self, c: (i32, i32)) -> bool {
        around2(c).all(|n| self.decorated.contains(&n))
    }
    fn dirty(&self, c: (i32, i32)) -> bool {
        around(c).any(|n| self.decorated.contains(&n))
    }
}

impl VanillaGenerator {
    fn proto(&self, cx: i32, cz: i32) -> Box<Proto> {
        let g = self.generate(cx, cz);
        let mut p = Proto::new(g.blocks, g.biomes);
        p.post = g.fluid_ticks;
        Box::new(p)
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
            let drop: Vec<_> = st.protos.keys().copied().filter(|&c| !st.dirty(c) || around(c).all(|n| st.done(n))).collect();
            for c in drop {
                st.protos.remove(&c);
            }
        }
        drop(guard);
        coords.iter().map(|c| out.get(c).map(clone_gen).expect("generated")).collect()
    }

    fn run(&self, lib: &Lib, st: &mut PipeState, want: &[(i32, i32)], out: &mut HashMap<(i32, i32), Generated>) {
        // the 3x3 neighbours must be final too: leaf distances look across borders
        let mut dec: Vec<(i32, i32)> = want.iter().flat_map(|&c| around2(c)).filter(|n| !st.decorated.contains(n)).collect();
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
                    seed: self.seed,
                    outside: &outside,
                })
                .collect();
            let place = |lv: &mut Region, r: &mut super::random::WgRandom, dec: i64, step: usize| {
                if let Some(s) = &self.structures {
                    s.place_step(self, lv, r, dec, step);
                }
            };
            regions.par_iter_mut().for_each(|r| lib.decorate(r, self.seed, Some(&place)));
            for r in regions {
                for (n, p) in around((r.cx, r.cz)).zip(r.chunks) {
                    st.protos.insert(n, p);
                }
            }
            st.decorated.extend(batch);
        }
        let made: Vec<_> = want
            .par_iter()
            .map(|&c| {
                let p = &st.protos[&c];
                let mut marks: Vec<[i32; 3]> = p.post.iter().chain(&p.ticks).copied().collect();
                let mut blocks = if p.post.is_empty() {
                    p.blocks.clone()
                } else {
                    // ProtoChunk post processing (at FULL status)
                    let mut r = Region { cx: c.0, cz: c.1, chunks: around(c).map(|n| st.protos[&n].clone()).collect(), lib, zoom_seed, seed: self.seed, outside: &outside };
                    super::feature::post_process(&mut r);
                    std::mem::take(&mut r.chunks[4].blocks)
                };
                fix_leaves(lib, c, &|n| &st.protos[&n].blocks, &mut blocks);
                bubble_columns(&mut blocks);
                // fluids marked for post processing get a fluid tick when loaded (`LiquidBlock.updateShape`)
                marks.sort_unstable();
                marks.dedup();
                marks.retain(|q| crate::registry::vstate::has_fluid(blocks[((q[1] - super::chunk::MIN_Y) * 256 + (q[2] & 15) * 16 + (q[0] & 15)) as usize]));
                (c, Generated { blocks, biomes: p.biomes.clone(), heights: p.hm[Hm::NotAir as usize], fluid_ticks: marks })
            })
            .collect();
        out.extend(made);
    }
}

/// Magma blocks under water tick once loaded (`MagmaBlock.tick`) and
/// raise a downward bubble column through the water sources above them
/// (`BubbleColumnBlock.updateColumn`); done here so the column is there
/// when the chunk is first sent.
fn bubble_columns(blocks: &mut [BlockState]) {
    use std::sync::OnceLock;
    static S: OnceLock<(BlockState, BlockState)> = OnceLock::new();
    let (magma, column) = *S.get_or_init(|| {
        let r = crate::registry::BlockRegistry::get();
        (r.state("magma_block", &[]).expect("magma_block"), r.state("bubble_column", &[("drag", "true")]).expect("bubble_column"))
    });
    let h = (HEIGHT as usize) - 1;
    for col in 0..256 {
        for y in 0..h {
            if blocks[y * 256 + col] != magma {
                continue;
            }
            let mut yy = y + 1;
            while yy <= h {
                let s = &mut blocks[yy * 256 + col];
                if *s == crate::registry::blocks::WATER || *s == column {
                    *s = column;
                    yy += 1;
                } else {
                    break;
                }
            }
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

/// Trees leave scheduled ticks on their leaves; when the chunk is loaded
/// those ticks (and the shape updates they cause) settle every leaf at its
/// distance to the nearest log, capped at 7 (`LeavesBlock.updateDistance`).
/// That fixpoint only depends on blocks within 6 of the chunk, so it is
/// computed here over the 3x3 neighbourhood.
fn fix_leaves<'a>(lib: &Lib, c: (i32, i32), chunk: &dyn Fn((i32, i32)) -> &'a Vec<BlockState>, out: &mut [BlockState]) {
    use std::collections::VecDeque;
    let kind = |s: BlockState| lib.leaf_kind[s.0 as usize];
    // y range holding leaves in the centre chunk
    let (mut lo, mut hi) = (usize::MAX, 0);
    for (i, s) in out.iter().enumerate() {
        if kind(*s) == 1 {
            let y = i / 256;
            lo = lo.min(y);
            hi = hi.max(y);
        }
    }
    if lo > hi {
        return;
    }
    let y0 = lo.saturating_sub(6);
    let y1 = (hi + 6).min(HEIGHT as usize - 1);
    let h = y1 - y0 + 1;
    const W: usize = 48;
    let at = |x: usize, y: usize, z: usize| ((y - y0) * W + z) * W + x;
    // 0..=7 distance, 255 not a leaf
    let mut d = vec![255u8; W * W * h];
    let mut q = VecDeque::new();
    for i in 0..9 {
        let n = (c.0 + i / 3 - 1, c.1 + i % 3 - 1);
        let b = chunk(n);
        let (ox, oz) = ((i / 3) as usize * 16, (i % 3) as usize * 16);
        for y in y0..=y1 {
            for z in 0..16 {
                for x in 0..16 {
                    match kind(b[(y * 16 + z) * 16 + x]) {
                        1 => d[at(ox + x, y, oz + z)] = 7,
                        2 => {
                            d[at(ox + x, y, oz + z)] = 0;
                            q.push_back((ox + x, y, oz + z));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    while let Some((x, y, z)) = q.pop_front() {
        let nd = d[at(x, y, z)] + 1;
        if nd >= 7 {
            continue;
        }
        let mut visit = |x: usize, y: usize, z: usize| {
            let i = at(x, y, z);
            if d[i] != 255 && d[i] > nd {
                d[i] = nd;
                q.push_back((x, y, z));
            }
        };
        if x > 0 {
            visit(x - 1, y, z);
        }
        if x + 1 < W {
            visit(x + 1, y, z);
        }
        if z > 0 {
            visit(x, y, z - 1);
        }
        if z + 1 < W {
            visit(x, y, z + 1);
        }
        if y > y0 {
            visit(x, y - 1, z);
        }
        if y < y1 {
            visit(x, y + 1, z);
        }
    }
    let reg = BlockRegistry::get();
    for y in lo..=hi {
        for z in 0..16 {
            for x in 0..16 {
                let i = (y * 16 + z) * 16 + x;
                let s = out[i];
                if kind(s) != 1 {
                    continue;
                }
                let v = d[at(x + 16, y, z + 16)].to_string();
                if reg.property(s, "distance") != Some(v.as_str()) {
                    out[i] = reg.with(s, "distance", &v).unwrap_or(s);
                }
            }
        }
    }
}

fn clone_gen(g: &Generated) -> Generated {
    Generated { blocks: g.blocks.clone(), biomes: g.biomes.clone(), heights: g.heights, fluid_ticks: g.fluid_ticks.clone() }
}
