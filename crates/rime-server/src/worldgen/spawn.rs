//! Vanilla world spawn selection (`MinecraftServer.setInitialSpawn`,
//! `Climate.SpawnFinder`, `PlayerSpawnFinder.getSpawnPosInChunk`).

use super::biome::quantize;
use super::biome_builder::q;
use super::chunk::{r, HEIGHT, MIN_Y};
use super::{Generated, VanillaGenerator};
use crate::registry::{BlockRegistry, BlockState};

/// (min, max) quantized climate parameter.
type Param = (i64, i64);

fn dist(p: Param, v: i64) -> i64 {
    if v > p.1 {
        v - p.1
    } else if p.0 > v {
        p.0 - v
    } else {
        0
    }
}

impl VanillaGenerator {
    /// Fitness of a block column for spawning: climate distance to
    /// `OverworldBiomeBuilder.spawnTarget()` plus squared distance to 0,0.
    fn spawn_fitness(&self, x: i32, z: i32) -> i64 {
        let (bx, bz) = ((x >> 2) << 2, (z >> 2) << 2);
        let s = |i: usize| quantize(self.router.eval(i, bx, 0, bz));
        let t = [s(r::TEMPERATURE), s(r::VEGETATION), s(r::CONTINENTS), s(r::EROSION), 0, s(r::RIDGES)];
        let full = (q(-1.0), q(1.0));
        let inland = (q(-0.11), q(0.55));
        // Parameter.span(a, b) = (a.min, b.max)
        let cont = (inland.0, full.1);
        let depth = (q(0.0), q(0.0));
        let mut best = i64::MAX;
        for weird in [(q(-1.0), q(-0.16)), (q(0.16), q(1.0))] {
            let ps = [full, full, cont, full, depth, weird];
            let f: i64 = ps.iter().zip(t).map(|(p, v)| dist(*p, v).pow(2)).sum();
            best = best.min(f);
        }
        best * 2048 * 2048 + (x as i64).pow(2) + (z as i64).pow(2)
    }

    /// `Climate.findSpawnPosition`: best climate column near 0,0 (x, z).
    pub fn climate_spawn(&self) -> (i32, i32) {
        let mut best = ((0, 0), self.spawn_fitness(0, 0));
        for (max, step) in [(2048.0f32, 512.0f32), (512.0, 32.0)] {
            let (cx, cz) = best.0;
            let mut angle = 0.0f32;
            let mut radius = step;
            while radius <= max {
                let x = cx + ((angle as f64).sin() * radius as f64) as i32;
                let z = cz + ((angle as f64).cos() * radius as f64) as i32;
                let f = self.spawn_fitness(x, z);
                if f < best.1 {
                    best = ((x, z), f);
                }
                angle += step / radius;
                if angle as f64 > std::f64::consts::TAU {
                    angle = 0.0;
                    radius += step;
                }
            }
        }
        best.0
    }

    /// `MinecraftServer.setInitialSpawn`: the world spawn block position.
    /// `chunk` generates (or loads) a chunk.
    pub fn find_spawn(&self, chunk: &dyn Fn(i32, i32) -> Generated) -> (i32, i32, i32) {
        let (x, z) = self.climate_spawn();
        let (scx, scz) = (x >> 4, z >> 4);
        let (mut j, mut k, mut l, mut m) = (0i32, 0i32, 0i32, -1i32);
        for _ in 0..11 * 11 {
            if (-5..=5).contains(&j) && (-5..=5).contains(&k) {
                let (cx, cz) = (scx + j, scz + k);
                if let Some(p) = spawn_in_chunk(&chunk(cx, cz), cx, cz) {
                    return p;
                }
            }
            if j == k || (j < 0 && j == -k) || (j > 0 && j == 1 - k) {
                let o = l;
                l = -m;
                m = o;
            }
            j += l;
            k += m;
        }
        (scx * 16 + 8, 64, scz * 16 + 8)
    }
}

fn full_top_face(b: BlockState) -> bool {
    let reg = BlockRegistry::get();
    reg.is_opaque(b) || b.name().ends_with("leaves") || b.name().ends_with("glass")
}

/// `PlayerSpawnFinder.getSpawnPosInChunk` on a freshly generated chunk.
pub fn spawn_in_chunk(g: &Generated, cx: i32, cz: i32) -> Option<(i32, i32, i32)> {
    let reg = BlockRegistry::get();
    let at = |x: i32, y: i32, z: i32| g.blocks[(((y - MIN_Y) * 16 + z) * 16 + x) as usize];
    // "first available" heights (top + 1) for the three heightmaps
    let height = |x: i32, z: i32, pred: &dyn Fn(BlockState) -> bool| {
        (MIN_Y..MIN_Y + HEIGHT).rev().find(|&y| pred(at(x, y, z))).map_or(MIN_Y, |y| y + 1)
    };
    let blocks_motion = |b: BlockState| reg.is_solid(b);
    for x in 0..16 {
        for z in 0..16 {
            let mb = height(x, z, &|b| blocks_motion(b) || reg.has_fluid(b));
            if mb < MIN_Y {
                continue;
            }
            let ws = height(x, z, &|b| !b.is_air());
            if ws <= mb && ws > height(x, z, &|b| blocks_motion(b)) {
                continue;
            }
            let mut y = (mb + 1).min(MIN_Y + HEIGHT - 1);
            while y >= MIN_Y {
                let b = at(x, y, z);
                if reg.has_fluid(b) {
                    break;
                }
                if full_top_face(b) {
                    return Some((cx * 16 + x, y + 1, cz * 16 + z));
                }
                y -= 1;
            }
        }
    }
    None
}
