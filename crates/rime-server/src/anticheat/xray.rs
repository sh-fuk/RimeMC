//! Anti-X-ray.
//!
//! Prevention: before a chunk is sent, every ore that touches no transparent
//! block (including across chunk borders, via `neighbor`) is replaced by the
//! surrounding stone type, so X-ray clients see nothing. In `obfuscate` mode,
//! hidden stone additionally gets fake ores, making ore-finding mods useless.
//! When a block changes, [`reveal_around`] lists the positions whose real
//! state must be re-sent so legit players never notice anything.
//!
//! Detection: [`MiningStats`] tracks the ratio of valuable ores to stone mined;
//! a ratio far above the natural distribution raises an alert (never an
//! automatic ban, since lucky caving exists).

use crate::world::chunk::{Block, Chunk, MIN_Y};
use crate::world::generator::hash2;

use super::movement::Flag;
use super::Check;
use crate::config::XrayMode;

fn base_stone(y: i32) -> Block {
    if y < 0 { Block::Deepslate } else { Block::Stone }
}

/// Return the copy of `chunk` that is safe to send to clients.
/// `neighbor(x, y, z)` answers for world coordinates outside this chunk
/// (return `None` if that chunk is not loaded → treated as exposed, i.e. the
/// ore stays visible; this errs on the side of correctness).
pub fn obfuscate(chunk: &Chunk, mode: XrayMode, seed: u32, neighbor: &dyn Fn(i32, i32, i32) -> Option<Block>) -> Chunk {
    if mode == XrayMode::Off {
        return chunk.clone();
    }
    let mut out = chunk.clone();
    let bx = chunk.x * 16;
    let bz = chunk.z * 16;
    let get = |x: i32, y: i32, z: i32| -> Option<Block> {
        if (0..16).contains(&x) && (0..16).contains(&z) {
            Some(chunk.get(x as usize, y, z as usize))
        } else {
            neighbor(bx + x, y, bz + z)
        }
    };
    let max_y = chunk.heightmap.iter().copied().max().unwrap_or(0) as i32;
    for y in MIN_Y..=max_y.min(MIN_Y + 383) {
        for z in 0..16i32 {
            for x in 0..16i32 {
                let b = chunk.get(x as usize, y, z as usize);
                let candidate = b.is_ore() || (mode == XrayMode::Obfuscate && matches!(b, Block::Stone | Block::Deepslate));
                if !candidate {
                    continue;
                }
                let exposed = [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)]
                    .iter()
                    .any(|(dx, dy, dz)| match get(x + dx, y + dy, z + dz) {
                        Some(n) => n.is_transparent(),
                        None => true,
                    });
                if exposed {
                    continue;
                }
                if b.is_ore() {
                    out.set(x as usize, y, z as usize, base_stone(y));
                } else {
                    // obfuscate: sprinkle deterministic fake ores into hidden stone
                    let h = hash2(bx + x, bz + z, seed ^ (y as u32).wrapping_mul(0x9E37_79B9));
                    if h % 23 == 0 {
                        let fake = if y < 0 {
                            [Block::DeepslateDiamondOre, Block::DeepslateGoldOre, Block::DeepslateIronOre, Block::DeepslateRedstoneOre][(h >> 8) as usize % 4]
                        } else {
                            [Block::DiamondOre, Block::GoldOre, Block::IronOre, Block::CoalOre][(h >> 8) as usize % 4]
                        };
                        out.set(x as usize, y, z as usize, fake);
                    }
                }
            }
        }
    }
    for s in &mut out.sections {
        s.optimize();
    }
    out
}

/// After a block at `pos` changed, these positions must be re-sent with their
/// real state (their exposure may have changed).
pub fn reveal_around(pos: [i32; 3]) -> impl Iterator<Item = [i32; 3]> {
    [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)].into_iter().map(move |(dx, dy, dz)| [pos[0] + dx, pos[1] + dy, pos[2] + dz])
}

/// Statistical x-ray detection.
#[derive(Default)]
pub struct MiningStats {
    stone: u32,
    valuable: u32,
}

impl MiningStats {
    pub fn on_break(&mut self, b: Block) -> Option<Flag> {
        match b {
            Block::Stone | Block::Deepslate => self.stone += 1,
            Block::DiamondOre | Block::DeepslateDiamondOre | Block::EmeraldOre | Block::DeepslateEmeraldOre | Block::GoldOre | Block::DeepslateGoldOre => {
                self.valuable += 1
            }
            _ => return None,
        }
        // natural rate is ~1 valuable per 150–300 stone when strip-mining
        if self.valuable >= 8 && (self.stone as f64) < self.valuable as f64 * 15.0 {
            let detail = format!("{} valuable ores / {} stone", self.valuable, self.stone);
            // reset so a single streak produces one alert
            *self = Self::default();
            return Some(Flag { check: Check::XrayStats, amount: 1.0, detail: format!("possible x-ray: {detail}") });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_chunk() -> Chunk {
        let mut c = Chunk::empty(0, 0);
        for y in -64..60 {
            for x in 0..16 {
                for z in 0..16 {
                    c.set(x, y, z, if y < 0 { Block::Deepslate } else { Block::Stone });
                }
            }
        }
        c.heightmap = [59; 256];
        c
    }

    #[test]
    fn hidden_ore_removed_exposed_ore_kept() {
        let mut c = solid_chunk();
        c.set(8, 10, 8, Block::DiamondOre); // buried
        c.set(8, 59, 8, Block::CoalOre); // top layer, air above → visible
        c.set(3, -10, 3, Block::DeepslateGoldOre);
        c.set(3, -9, 3, Block::Air); // cave next to it
        let solid = |_x, _y, _z| Some(Block::Stone);
        let o = obfuscate(&c, XrayMode::Hide, 1, &solid);
        assert_eq!(o.get(8, 10, 8), Block::Stone);
        assert_eq!(o.get(8, 59, 8), Block::CoalOre);
        assert_eq!(o.get(3, -10, 3), Block::DeepslateGoldOre);
    }

    #[test]
    fn border_uses_neighbor_and_unknown_is_exposed() {
        let mut c = solid_chunk();
        c.set(0, 10, 5, Block::DiamondOre);
        let o = obfuscate(&c, XrayMode::Hide, 1, &|_, _, _| None);
        assert_eq!(o.get(0, 10, 5), Block::DiamondOre);
        let o = obfuscate(&c, XrayMode::Hide, 1, &|_, _, _| Some(Block::Stone));
        assert_eq!(o.get(0, 10, 5), Block::Stone);
    }

    #[test]
    fn obfuscate_adds_fakes() {
        let o = obfuscate(&solid_chunk(), XrayMode::Obfuscate, 1, &|_, _, _| Some(Block::Stone));
        let mut fakes = 0;
        for y in -60..50 {
            for x in 1..15 {
                for z in 1..15 {
                    fakes += o.get(x, y, z).is_ore() as usize;
                }
            }
        }
        assert!(fakes > 100);
    }

    #[test]
    fn xray_statistics() {
        let mut s = MiningStats::default();
        let mut alert = false;
        for _ in 0..10 {
            for _ in 0..3 {
                s.on_break(Block::Stone);
            }
            alert |= s.on_break(Block::DiamondOre).is_some();
        }
        assert!(alert);
    }
}
