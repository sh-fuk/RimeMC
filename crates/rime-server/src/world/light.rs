//! Sky and block light, computed like vanilla's `LightEngine`
//! (26.1): light spreads to the six neighbours, losing
//! `max(1, getLightDampening)` per step unless the faces between the two
//! blocks occlude it; full sky light (15) falls straight down through blocks
//! that do not dampen it.
//!
//! The per-state facts (emission, dampening, face occlusion shapes as 16x16
//! masks) are dumped from vanilla into `assets/26.1/light.bin`.
//!
//! Light reaches at most 15 blocks, so a chunk's light only depends on its
//! 3x3 neighbourhood.

use std::sync::OnceLock;

use super::chunk::{Chunk, HEIGHT, MIN_Y, SECTIONS};
use crate::registry::BlockState;

struct Table {
    emission: Vec<u8>,
    damp: Vec<u8>,
    /// bit 0: propagates skylight down, bit 1: empty occlusion shape
    flags: Vec<u8>,
    /// per state and `Direction` (D, U, N, S, W, E): mask index
    faces: Vec<[u16; 6]>,
    masks: Vec<[u64; 4]>,
}

fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| {
        let d: &[u8] = include_bytes!("../../assets/26.1/light.bin");
        let be32 = |o: usize| u32::from_be_bytes(d[o..o + 4].try_into().unwrap()) as usize;
        let n = be32(0);
        let m = be32(4);
        let mut o = 8;
        let mut masks = Vec::with_capacity(m);
        for _ in 0..m {
            let mut k = [0u64; 4];
            for w in &mut k {
                *w = u64::from_be_bytes(d[o..o + 8].try_into().unwrap());
                o += 8;
            }
            masks.push(k);
        }
        let (mut emission, mut damp, mut flags, mut faces) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
        for _ in 0..n {
            emission.push(d[o]);
            damp.push(d[o + 1]);
            flags.push(d[o + 2]);
            let mut f = [0u16; 6];
            for (k, v) in f.iter_mut().enumerate() {
                *v = u16::from_le_bytes([d[o + 3 + 2 * k], d[o + 4 + 2 * k]]);
            }
            faces.push(f);
            o += 15;
        }
        Table { emission, damp, flags, faces, masks }
    })
}

pub fn emission(s: BlockState) -> u8 {
    table().emission.get(s.0 as usize).copied().unwrap_or(0)
}

pub fn dampening(s: BlockState) -> u8 {
    table().damp.get(s.0 as usize).copied().unwrap_or(15)
}

/// `Block.isShapeFullBlock(state.getFaceOcclusionShape(dir))`
pub fn face_full(s: BlockState, dir: usize) -> bool {
    if crate::registry::vstate::solid_render(s) {
        return true;
    }
    let t = table();
    let i = s.0 as usize;
    if t.flags.get(i).is_none_or(|f| f & 2 != 0) {
        return false;
    }
    t.masks[t.faces[i][dir] as usize] == [u64::MAX; 4]
}

/// `Direction` ordinals
const DOWN: usize = 0;
const OPPOSITE: [usize; 6] = [1, 0, 3, 2, 5, 4];

/// `LightEngine.shapeOccludes(from, to, dir)`
fn occludes(t: &Table, from: BlockState, to: BlockState, dir: usize) -> bool {
    let (a, b) = (from.0 as usize, to.0 as usize);
    let ea = t.flags.get(a).is_none_or(|f| f & 2 != 0);
    let eb = t.flags.get(b).is_none_or(|f| f & 2 != 0);
    if ea && eb {
        return false;
    }
    let ma = if ea { [0; 4] } else { t.masks[t.faces[a][dir] as usize] };
    let mb = if eb { [0; 4] } else { t.masks[t.faces[b][OPPOSITE[dir]] as usize] };
    (0..4).all(|k| ma[k] | mb[k] == u64::MAX)
}

const W: usize = 48;
const H: usize = HEIGHT as usize;

/// Light of the centre chunk: (sky, block) nibble arrays for the
/// `SECTIONS + 2` light sections (one below and one above the world).
pub struct ChunkLight {
    pub sky: Vec<Option<Box<[u8; 2048]>>>,
    pub block: Vec<Option<Box<[u8; 2048]>>>,
}

/// `around[i]` is the chunk at (dx, dz) = (i / 3 - 1, i % 3 - 1); missing
/// neighbours count as air.
pub fn compute(around: &[Option<&Chunk>; 9]) -> ChunkLight {
    let t = table();
    // states of the 48x48 window, index (y * W + z) * W + x
    let mut st = vec![0u16; W * W * H];
    for (i, c) in around.iter().enumerate() {
        let Some(c) = c else { continue };
        let (ox, oz) = ((i / 3) * 16, (i % 3) * 16);
        for y in 0..H {
            for z in 0..16 {
                for x in 0..16 {
                    st[(y * W + oz + z) * W + ox + x] = c.get(x, y as i32 + MIN_Y, z).0;
                }
            }
        }
    }
    let state = |i: usize| BlockState(st[i]);
    let damp = |i: usize| t.damp.get(st[i] as usize).copied().unwrap_or(15);
    let neighbours = |i: usize| -> [Option<usize>; 6] {
        let x = i % W;
        let z = (i / W) % W;
        let y = i / (W * W);
        [
            (y > 0).then(|| i - W * W),
            (y + 1 < H).then(|| i + W * W),
            (z > 0).then(|| i - W),
            (z + 1 < W).then(|| i + W),
            (x > 0).then(|| i - 1),
            (x + 1 < W).then(|| i + 1),
        ]
    };
    let spread = |level: &mut Vec<u8>, queue: &mut Vec<usize>, sky: bool| {
        while let Some(i) = queue.pop() {
            let l = level[i];
            if l <= 1 {
                continue;
            }
            let from = state(i);
            for (dir, n) in neighbours(i).into_iter().enumerate() {
                let Some(n) = n else { continue };
                let to = state(n);
                if occludes(t, from, to, dir) {
                    continue;
                }
                let d = damp(n);
                let v = if sky && l == 15 && dir == DOWN && d == 0 { 15 } else { l.saturating_sub(d.max(1)) };
                if v > level[n] {
                    level[n] = v;
                    queue.push(n);
                }
            }
        }
    };

    // sky: 15 straight down from the top until something dampens or occludes
    let mut sky = vec![0u8; W * W * H];
    let mut queue = Vec::new();
    for z in 0..W {
        for x in 0..W {
            let mut above = crate::registry::blocks::AIR;
            for y in (0..H).rev() {
                let i = (y * W + z) * W + x;
                let s = state(i);
                if damp(i) != 0 || occludes(t, above, s, DOWN) {
                    break;
                }
                sky[i] = 15;
                above = s;
            }
        }
    }
    for i in 0..sky.len() {
        if sky[i] == 15 && neighbours(i).iter().flatten().any(|&n| sky[n] < 14) {
            queue.push(i);
        }
    }
    spread(&mut sky, &mut queue, true);

    // block light from emitters
    let mut block = vec![0u8; W * W * H];
    for (i, s) in st.iter().enumerate() {
        let e = t.emission.get(*s as usize).copied().unwrap_or(0);
        if e > 0 {
            block[i] = e;
            queue.push(i);
        }
    }
    spread(&mut block, &mut queue, false);

    let pack = |level: &[u8], ls: usize| -> Option<Box<[u8; 2048]>> {
        let mut arr = Box::new([0u8; 2048]);
        let mut any = false;
        for ly in 0..16 {
            let y = (ls - 1) * 16 + ly;
            for z in 0..16 {
                for x in 0..16 {
                    let v = level[(y * W + z + 16) * W + x + 16];
                    if v != 0 {
                        let k = (ly << 8) | (z << 4) | x;
                        arr[k >> 1] |= v << ((k & 1) * 4);
                        any = true;
                    }
                }
            }
        }
        any.then_some(arr)
    };
    let mut out = ChunkLight { sky: Vec::with_capacity(SECTIONS + 2), block: Vec::with_capacity(SECTIONS + 2) };
    for ls in 0..SECTIONS + 2 {
        if ls == 0 {
            out.sky.push(None);
            out.block.push(None);
        } else if ls == SECTIONS + 1 {
            out.sky.push(Some(Box::new([0xFF; 2048])));
            out.block.push(None);
        } else {
            out.sky.push(pack(&sky, ls));
            out.block.push(pack(&block, ls));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{blocks, BlockRegistry};

    fn flat() -> Chunk {
        let mut c = Chunk::empty(0, 0);
        for y in MIN_Y..60 {
            for z in 0..16 {
                for x in 0..16 {
                    c.set(x, y, z, blocks::STONE);
                }
            }
        }
        c.recalc_heightmap();
        c
    }

    fn get(arr: &[Option<Box<[u8; 2048]>>], x: usize, y: i32, z: usize) -> u8 {
        let ls = ((y - MIN_Y) / 16 + 1) as usize;
        let k = ((((y - MIN_Y) % 16) as usize) << 8) | (z << 4) | x;
        arr[ls].as_ref().map(|a| (a[k >> 1] >> ((k & 1) * 4)) & 15).unwrap_or(0)
    }

    #[test]
    fn table_facts() {
        let r = BlockRegistry::get();
        assert_eq!(emission(r.state("glowstone", &[]).unwrap()), 15);
        assert_eq!(emission(r.state("torch", &[]).unwrap()), 14);
        assert_eq!(dampening(blocks::STONE), 15);
        assert_eq!(dampening(blocks::AIR), 0);
        assert_eq!(dampening(blocks::WATER), 1);
    }

    #[test]
    fn sky_and_torch() {
        let mut c = flat();
        // a cave room under a roof, opening on one side
        for y in 50..55 {
            for x in 2..10 {
                for z in 2..10 {
                    c.set(x, y, z, blocks::AIR);
                }
            }
        }
        for y in 50..60 {
            c.set(2, y, 5, blocks::AIR); // shaft to the sky
        }
        let torch = BlockRegistry::get().state("torch", &[]).unwrap();
        c.set(9, 50, 9, torch);
        let n = [None, None, None, None, Some(&c), None, None, None, None];
        let l = compute(&n);
        assert_eq!(get(&l.sky, 5, 70, 5), 15);
        assert_eq!(get(&l.sky, 5, 59, 5), 0);
        assert_eq!(get(&l.sky, 2, 50, 5), 15, "shaft is lit straight down");
        assert_eq!(get(&l.sky, 3, 50, 5), 14);
        assert_eq!(get(&l.sky, 5, 50, 5), 12);
        assert_eq!(get(&l.block, 9, 50, 9), 14);
        assert_eq!(get(&l.block, 8, 50, 9), 13);
        assert_eq!(get(&l.block, 9, 49, 9), 0, "stone stops block light");
    }

    #[test]
    fn slabs_occlude_by_shape() {
        let mut c = flat();
        let r = BlockRegistry::get();
        // bottom slab roof at y=60 over x=0..16: light under it comes only sideways
        let slab = r.state("stone_slab", &[("type", "bottom")]).unwrap();
        for x in 0..16 {
            for z in 0..16 {
                c.set(x, 60, z, slab);
            }
        }
        let n = [None, None, None, None, Some(&c), None, None, None, None];
        let l = compute(&n);
        // the slab's own cell is lit from above (top face of a bottom slab is open)
        assert!(get(&l.sky, 8, 60, 8) > 0);
        assert_eq!(get(&l.sky, 8, 61, 8), 15);
    }
}
