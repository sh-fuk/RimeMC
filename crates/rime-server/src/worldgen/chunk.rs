//! Port of vanilla's NoiseChunk fill (`NoiseBasedChunkGenerator.doFill`):
//! cell interpolation, flat caches, the noise-based aquifer and ore veins.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use super::density::{Cache, Graph, Id, Node, Router};
use super::noise::{clamped_map, floor, lerp};
use super::random::{Rng, XoroshiroFactory};
use crate::registry::{blocks, BlockState};

pub const MIN_Y: i32 = -64;
pub const HEIGHT: i32 = 384;
pub const CELL_W: i32 = 4;
pub const CELL_H: i32 = 8;
const CELLS_Y: i32 = HEIGHT / CELL_H;
const CORNERS_Y: usize = CELLS_Y as usize + 1;

/// Router indices (see density::ROUTER).
pub mod r {
    pub const BARRIER: usize = 0;
    pub const FLOODEDNESS: usize = 1;
    pub const SPREAD: usize = 2;
    pub const LAVA: usize = 3;
    pub const TEMPERATURE: usize = 4;
    pub const VEGETATION: usize = 5;
    pub const CONTINENTS: usize = 6;
    pub const EROSION: usize = 7;
    pub const DEPTH: usize = 8;
    pub const RIDGES: usize = 9;
    pub const PRELIMINARY_SURFACE: usize = 10;
    pub const FINAL_DENSITY: usize = 11;
    pub const VEIN_TOGGLE: usize = 12;
    pub const VEIN_RIDGED: usize = 13;
    pub const VEIN_GAP: usize = 14;
}

/// Per-chunk caches shared by both evaluation flavours.
pub struct NoiseChunk<'a> {
    pub g: &'a Graph,
    pub min_x: i32,
    pub min_z: i32,
    first_qx: i32,
    first_qz: i32,
    /// flat_cache values, 5x5 quarts per slot (NaN = not computed yet)
    flat: Vec<Vec<Cell<f64>>>,
    /// interpolated corner values per slot: [(cx*5 + cz) * CORNERS_Y + cy]
    corners: Vec<Vec<f64>>,
    psl_cache: RefCell<HashMap<(i32, i32), i32>>,
}

/// Evaluation at a block of this chunk during the fill (NoiseChunk as context).
pub struct InChunk<'c, 'a>(pub &'c NoiseChunk<'a>);
/// Evaluation at an arbitrary point through the chunk's wrapped functions
/// (SinglePointContext): flat caches apply, interpolation does not.
pub struct AtPoint<'c, 'a>(pub &'c NoiseChunk<'a>);

impl Cache for InChunk<'_, '_> {
    fn interpolated(&self, slot: u32, x: i32, y: i32, z: i32) -> Option<f64> {
        self.0.interpolate(slot, x, y, z)
    }
    fn flat(&self, g: &Graph, slot: u32, x: i32, z: i32) -> Option<f64> {
        self.0.flat_value(g, slot, x, z)
    }
}

impl Cache for AtPoint<'_, '_> {
    fn interpolated(&self, _: u32, _: i32, _: i32, _: i32) -> Option<f64> {
        None
    }
    fn flat(&self, g: &Graph, slot: u32, x: i32, z: i32) -> Option<f64> {
        self.0.flat_value(g, slot, x, z)
    }
}

impl<'a> NoiseChunk<'a> {
    pub fn new(g: &'a Graph, cx: i32, cz: i32) -> Self {
        let min_x = cx * 16;
        let min_z = cz * 16;
        let mut nc = NoiseChunk {
            g,
            min_x,
            min_z,
            first_qx: min_x >> 2,
            first_qz: min_z >> 2,
            flat: g.flat_caches.iter().map(|_| (0..25).map(|_| Cell::new(f64::NAN)).collect()).collect(),
            corners: Vec::new(),
            psl_cache: RefCell::new(HashMap::new()),
        };
        let mut corners = Vec::with_capacity(g.interpolated.len());
        for &id in &g.interpolated {
            let Node::Marker(_, arg) = g.nodes[id as usize] else { unreachable!() };
            let mut v = vec![0.0; 25 * CORNERS_Y];
            for cx in 0..5 {
                for cz in 0..5 {
                    for cy in 0..CORNERS_Y {
                        let (x, y, z) = (min_x + cx as i32 * CELL_W, MIN_Y + cy as i32 * CELL_H, min_z + cz as i32 * CELL_W);
                        v[(cx * 5 + cz) * CORNERS_Y + cy] = g.eval(arg, x, y, z, &AtPoint(&nc));
                    }
                }
            }
            corners.push(v);
        }
        nc.corners = corners;
        nc
    }

    fn flat_value(&self, g: &Graph, slot: u32, x: i32, z: i32) -> Option<f64> {
        let i = (x >> 2) - self.first_qx;
        let j = (z >> 2) - self.first_qz;
        if !(0..5).contains(&i) || !(0..5).contains(&j) {
            return None;
        }
        let c = &self.flat[slot as usize][(i * 5 + j) as usize];
        let v = c.get();
        if !v.is_nan() {
            return Some(v);
        }
        let Node::Marker(_, arg) = g.nodes[g.flat_caches[slot as usize] as usize] else { unreachable!() };
        let v = g.eval(arg, (self.first_qx + i) << 2, 0, (self.first_qz + j) << 2, &AtPoint(self));
        c.set(v);
        Some(v)
    }

    /// Vanilla interpolation order: y, then x, then z.
    fn interpolate(&self, slot: u32, x: i32, y: i32, z: i32) -> Option<f64> {
        let (lx, ly, lz) = (x - self.min_x, y - MIN_Y, z - self.min_z);
        if !(0..16).contains(&lx) || !(0..16).contains(&lz) || !(0..HEIGHT).contains(&ly) {
            return None;
        }
        let (cx, cy, cz) = ((lx / CELL_W) as usize, (ly / CELL_H) as usize, (lz / CELL_W) as usize);
        let fx = (lx % CELL_W) as f64 / CELL_W as f64;
        let fy = (ly % CELL_H) as f64 / CELL_H as f64;
        let fz = (lz % CELL_W) as f64 / CELL_W as f64;
        let v = &self.corners[slot as usize];
        let at = |dx: usize, dy: usize, dz: usize| v[((cx + dx) * 5 + cz + dz) * CORNERS_Y + cy + dy];
        let xz00 = lerp(fy, at(0, 0, 0), at(0, 1, 0));
        let xz10 = lerp(fy, at(1, 0, 0), at(1, 1, 0));
        let xz01 = lerp(fy, at(0, 0, 1), at(0, 1, 1));
        let xz11 = lerp(fy, at(1, 0, 1), at(1, 1, 1));
        let z0 = lerp(fx, xz00, xz10);
        let z1 = lerp(fx, xz01, xz11);
        Some(lerp(fz, z0, z1))
    }

    pub fn at_point(&self, id: Id, x: i32, y: i32, z: i32) -> f64 {
        self.g.eval(id, x, y, z, &AtPoint(self))
    }

    pub fn in_chunk(&self, id: Id, x: i32, y: i32, z: i32) -> f64 {
        self.g.eval(id, x, y, z, &InChunk(self))
    }

    /// `NoiseChunk.preliminarySurfaceLevel`
    pub fn preliminary_surface_level(&self, router: &Router, x: i32, z: i32) -> i32 {
        let (x, z) = ((x >> 2) << 2, (z >> 2) << 2);
        if let Some(&v) = self.psl_cache.borrow().get(&(x, z)) {
            return v;
        }
        let v = floor(self.at_point(router.ids[r::PRELIMINARY_SURFACE], x, 0, z));
        self.psl_cache.borrow_mut().insert((x, z), v);
        v
    }

    fn max_preliminary_surface_level(&self, router: &Router, min_x: i32, min_z: i32, max_x: i32, max_z: i32) -> i32 {
        let mut m = i32::MIN;
        let mut z = min_z;
        while z <= max_z {
            let mut x = min_x;
            while x <= max_x {
                m = m.max(self.preliminary_surface_level(router, x, z));
                x += 4;
            }
            z += 4;
        }
        m
    }
}

// ---------------------------------------------------------------- aquifer

const WAY_BELOW_MIN_Y: i32 = -2032 << 4;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FluidStatus {
    pub level: i32,
    pub fluid: BlockState,
}

impl FluidStatus {
    pub fn at(&self, y: i32) -> BlockState {
        if y < self.level { self.fluid } else { blocks::AIR }
    }
}

pub struct FluidPicker {
    lava: FluidStatus,
    sea: FluidStatus,
    sea_level: i32,
}

impl FluidPicker {
    pub fn overworld(sea_level: i32, default_fluid: BlockState) -> Self {
        FluidPicker { lava: FluidStatus { level: -54, fluid: blocks::LAVA }, sea: FluidStatus { level: sea_level, fluid: default_fluid }, sea_level }
    }
    pub fn compute(&self, _x: i32, y: i32, _z: i32) -> FluidStatus {
        if y < (-54).min(self.sea_level) { self.lava } else { self.sea }
    }
}

const SURFACE_SAMPLING_OFFSETS: [[i32; 2]; 13] = [[0, 0], [-2, -1], [-1, -1], [0, -1], [1, -1], [-3, 0], [-2, 0], [-1, 0], [1, 0], [-2, 1], [-1, 1], [0, 1], [1, 1]];

fn grid_x(i: i32) -> i32 {
    i >> 4
}
fn from_grid_x(g: i32, off: i32) -> i32 {
    (g << 4) + off
}
fn grid_y(i: i32) -> i32 {
    i.div_euclid(12)
}
fn from_grid_y(g: i32, off: i32) -> i32 {
    g * 12 + off
}
fn similarity(a: i32, b: i32) -> f64 {
    1.0 - (b - a) as f64 / 25.0
}

pub struct Aquifer<'n, 'a> {
    nc: &'n NoiseChunk<'a>,
    router: &'n Router,
    random: XoroshiroFactory,
    picker: &'n FluidPicker,
    status_cache: Vec<Option<FluidStatus>>,
    location_cache: Vec<Option<(i32, i32, i32)>>,
    min_gx: i32,
    min_gy: i32,
    min_gz: i32,
    size_x: i32,
    size_z: i32,
    skip_sampling_above_y: i32,
}

impl<'n, 'a> Aquifer<'n, 'a> {
    pub fn new(nc: &'n NoiseChunk<'a>, router: &'n Router, random: XoroshiroFactory, picker: &'n FluidPicker, min_y: i32, height: i32) -> Self {
        let min_gx = grid_x(nc.min_x - 5);
        let max_gx = grid_x(nc.min_x + 15 - 5) + 1;
        let size_x = max_gx - min_gx + 1;
        let min_gy = grid_y(min_y + 1) - 1;
        let max_gy = grid_y(min_y + height + 1) + 1;
        let size_y = max_gy - min_gy + 1;
        let min_gz = grid_x(nc.min_z - 5);
        let max_gz = grid_x(nc.min_z + 15 - 5) + 1;
        let size_z = max_gz - min_gz + 1;
        let n = (size_x * size_y * size_z) as usize;
        let surface = nc.max_preliminary_surface_level(router, from_grid_x(min_gx, 0), from_grid_x(min_gz, 0), from_grid_x(max_gx, 9), from_grid_x(max_gz, 9)) + 8;
        let gy = grid_y(surface + 12) + 1;
        Aquifer {
            nc,
            router,
            random,
            picker,
            status_cache: vec![None; n],
            location_cache: vec![None; n],
            min_gx,
            min_gy,
            min_gz,
            size_x,
            size_z,
            skip_sampling_above_y: from_grid_y(gy, 11) - 1,
        }
    }

    fn index(&self, x: i32, y: i32, z: i32) -> usize {
        (((y - self.min_gy) * self.size_z + (z - self.min_gz)) * self.size_x + (x - self.min_gx)) as usize
    }

    /// None = keep the solid default block.
    pub fn compute_substance(&mut self, x: i32, y: i32, z: i32, density: f64) -> Option<BlockState> {
        if density > 0.0 {
            return None;
        }
        let global = self.picker.compute(x, y, z);
        if y > self.skip_sampling_above_y {
            return Some(global.at(y));
        }
        if global.at(y) == blocks::LAVA {
            return Some(blocks::LAVA);
        }
        let (gx, gy, gz) = (grid_x(x - 5), grid_y(y + 1), grid_x(z - 5));
        let (mut d1, mut d2, mut d3) = (i32::MAX, i32::MAX, i32::MAX);
        let (mut i1, mut i2, mut i3) = (0usize, 0usize, 0usize);
        for dx in 0..=1 {
            for dy in -1..=1 {
                for dz in 0..=1 {
                    let (ax, ay, az) = (gx + dx, gy + dy, gz + dz);
                    let idx = self.index(ax, ay, az);
                    let loc = match self.location_cache[idx] {
                        Some(l) => l,
                        None => {
                            let mut rng = self.random.at(ax, ay, az);
                            let l = (from_grid_x(ax, rng.next_int_bound(10)), from_grid_y(ay, rng.next_int_bound(9)), from_grid_x(az, rng.next_int_bound(10)));
                            self.location_cache[idx] = Some(l);
                            l
                        }
                    };
                    let (ex, ey, ez) = (loc.0 - x, loc.1 - y, loc.2 - z);
                    let d = ex * ex + ey * ey + ez * ez;
                    if d1 >= d {
                        i3 = i2;
                        i2 = i1;
                        i1 = idx;
                        d3 = d2;
                        d2 = d1;
                        d1 = d;
                    } else if d2 >= d {
                        i3 = i2;
                        i2 = idx;
                        d3 = d2;
                        d2 = d;
                    } else if d3 >= d {
                        i3 = idx;
                        d3 = d;
                    }
                }
            }
        }
        let s1 = self.status(i1);
        let sim = similarity(d1, d2);
        let state = s1.at(y);
        if sim <= 0.0 {
            return Some(state);
        }
        if state == blocks::WATER && self.picker.compute(x, y - 1, z).at(y - 1) == blocks::LAVA {
            return Some(state);
        }
        let mut barrier = f64::NAN;
        let s2 = self.status(i2);
        let p = sim * self.pressure(x, y, z, &mut barrier, s1, s2);
        if density + p > 0.0 {
            return None;
        }
        let s3 = self.status(i3);
        let sim13 = similarity(d1, d3);
        if sim13 > 0.0 {
            let p = sim * sim13 * self.pressure(x, y, z, &mut barrier, s1, s3);
            if density + p > 0.0 {
                return None;
            }
        }
        let sim23 = similarity(d2, d3);
        if sim23 > 0.0 {
            let p = sim * sim23 * self.pressure(x, y, z, &mut barrier, s2, s3);
            if density + p > 0.0 {
                return None;
            }
        }
        Some(state)
    }

    fn pressure(&self, x: i32, y: i32, z: i32, barrier: &mut f64, a: FluidStatus, b: FluidStatus) -> f64 {
        let (sa, sb) = (a.at(y), b.at(y));
        if (sa == blocks::LAVA && sb == blocks::WATER) || (sa == blocks::WATER && sb == blocks::LAVA) {
            return 2.0;
        }
        let diff = (a.level - b.level).abs();
        if diff == 0 {
            return 0.0;
        }
        let d9 = 0.5 * (a.level + b.level) as f64;
        let d11 = y as f64 + 0.5 - d9;
        let d13 = diff as f64 / 2.0;
        let d27 = d13 - d11.abs();
        let d29 = if d11 > 0.0 {
            let d31 = 0.0 + d27;
            if d31 > 0.0 { d31 / 1.5 } else { d31 / 2.5 }
        } else {
            let d31 = 3.0 + d27;
            if d31 > 0.0 { d31 / 3.0 } else { d31 / 10.0 }
        };
        let d33 = if !(-2.0..=2.0).contains(&d29) {
            0.0
        } else {
            if barrier.is_nan() {
                *barrier = self.nc.in_chunk(self.router.ids[r::BARRIER], x, y, z);
            }
            *barrier
        };
        2.0 * (d33 + d29)
    }

    fn status(&mut self, idx: usize) -> FluidStatus {
        if let Some(s) = self.status_cache[idx] {
            return s;
        }
        let (x, y, z) = self.location_cache[idx].expect("location computed first");
        let s = self.compute_fluid(x, y, z);
        self.status_cache[idx] = Some(s);
        s
    }

    fn compute_fluid(&self, x: i32, y: i32, z: i32) -> FluidStatus {
        let global = self.picker.compute(x, y, z);
        let mut min_surface = i32::MAX;
        let top = y + 12;
        let bottom = y - 12;
        let mut flag = false;
        for off in SURFACE_SAMPLING_OFFSETS {
            let bx = x + (off[0] << 4);
            let bz = z + (off[1] << 4);
            let psl = self.nc.preliminary_surface_level(self.router, bx, bz);
            let s = psl + 8;
            let center = off == [0, 0];
            if center && bottom > s {
                return global;
            }
            let above = top > s;
            if above || center {
                let st = self.picker.compute(bx, s, bz);
                if !st.at(s).is_air() {
                    if center {
                        flag = true;
                    }
                    if above {
                        return st;
                    }
                }
            }
            min_surface = min_surface.min(psl);
        }
        let level = self.surface_level(x, y, z, global, min_surface, flag);
        FluidStatus { level, fluid: self.fluid_type(x, y, z, global, level) }
    }

    fn surface_level(&self, x: i32, y: i32, z: i32, global: FluidStatus, min_surface: i32, flag: bool) -> i32 {
        let ids = &self.router.ids;
        let (d8, d10);
        if self.nc.at_point(ids[r::EROSION], x, y, z) < -0.22499999403953552 && self.nc.at_point(ids[r::DEPTH], x, y, z) > 0.8999999761581421 {
            d8 = -1.0;
            d10 = -1.0;
        } else {
            let i12 = min_surface + 8 - y;
            let d14 = if flag { clamped_map(i12 as f64, 0.0, 64.0, 1.0, 0.0) } else { 0.0 };
            let d16 = self.nc.at_point(ids[r::FLOODEDNESS], x, y, z).clamp(-1.0, 1.0);
            let d18 = map(d14, 1.0, 0.0, -0.3, 0.8);
            let d20 = map(d14, 1.0, 0.0, -0.8, 0.4);
            d8 = d16 - d20;
            d10 = d16 - d18;
        }
        if d10 > 0.0 {
            global.level
        } else if d8 > 0.0 {
            let (gx, gy, gz) = (x.div_euclid(16), y.div_euclid(40), z.div_euclid(16));
            let base = gy * 40 + 20;
            let d = self.nc.at_point(ids[r::SPREAD], gx, gy, gz) * 10.0;
            let q = floor(d / 3.0) * 3;
            min_surface.min(base + q)
        } else {
            WAY_BELOW_MIN_Y
        }
    }

    fn fluid_type(&self, x: i32, y: i32, z: i32, global: FluidStatus, level: i32) -> BlockState {
        if level <= -10 && level != WAY_BELOW_MIN_Y && global.fluid != blocks::LAVA {
            let v = self.nc.at_point(self.router.ids[r::LAVA], x.div_euclid(64), y.div_euclid(40), z.div_euclid(64));
            if v.abs() > 0.3 {
                return blocks::LAVA;
            }
        }
        global.fluid
    }
}

fn map(v: f64, a: f64, b: f64, c: f64, d: f64) -> f64 {
    lerp((v - a) / (b - a), c, d)
}

// ---------------------------------------------------------------- ore veins

fn ore_vein(nc: &NoiseChunk, router: &Router, random: &XoroshiroFactory, x: i32, y: i32, z: i32) -> Option<BlockState> {
    let d = nc.in_chunk(router.ids[r::VEIN_TOGGLE], x, y, z);
    let (ore, raw, filler, min_y, max_y) = if d > 0.0 {
        (blocks::COPPER_ORE, blocks::RAW_COPPER_BLOCK, blocks::GRANITE, 0, 50)
    } else {
        (blocks::DEEPSLATE_IRON_ORE, blocks::RAW_IRON_BLOCK, blocks::TUFF, -60, -8)
    };
    let e = d.abs();
    let j = max_y - y;
    let k = y - min_y;
    if k < 0 || j < 0 {
        return None;
    }
    let l = j.min(k);
    let f = clamped_map(l as f64, 0.0, 20.0, -0.2, 0.0);
    if e + f < 0.4000000059604645 {
        return None;
    }
    let mut rng = random.at(x, y, z);
    if rng.next_float() > 0.7 {
        return None;
    }
    if nc.in_chunk(router.ids[r::VEIN_RIDGED], x, y, z) >= 0.0 {
        return None;
    }
    let g = clamped_map(e, 0.4000000059604645, 0.6000000238418579, 0.10000000149011612, 0.30000001192092896);
    if (rng.next_float() as f64) < g && nc.in_chunk(router.ids[r::VEIN_GAP], x, y, z) > -0.30000001192092896 {
        return Some(if rng.next_float() < 0.02 { raw } else { ore });
    }
    Some(filler)
}

// ---------------------------------------------------------------- fill

pub struct Settings {
    pub sea_level: i32,
    pub default_block: BlockState,
    pub default_fluid: BlockState,
    pub aquifers: bool,
    pub ore_veins: bool,
}

/// Terrain of one chunk: blocks indexed `((y - MIN_Y) * 16 + z) * 16 + x`.
pub fn fill(router: &Router, settings: &Settings, aquifer_random: XoroshiroFactory, ore_random: XoroshiroFactory, cx: i32, cz: i32) -> Vec<BlockState> {
    let nc = NoiseChunk::new(&router.graph, cx, cz);
    let picker = FluidPicker::overworld(settings.sea_level, settings.default_fluid);
    let mut aq = Aquifer::new(&nc, router, aquifer_random, &picker, MIN_Y, HEIGHT);
    let fd = router.ids[r::FINAL_DENSITY];
    let mut out = vec![blocks::AIR; (16 * 16 * HEIGHT) as usize];
    for y in MIN_Y..MIN_Y + HEIGHT {
        for z in 0..16 {
            for x in 0..16 {
                let (bx, bz) = (nc.min_x + x, nc.min_z + z);
                let density = nc.in_chunk(fd, bx, y, bz);
                let state = if settings.aquifers {
                    aq.compute_substance(bx, y, bz, density)
                } else if density > 0.0 {
                    None
                } else {
                    Some(picker.compute(bx, y, bz).at(y))
                };
                let state = match state {
                    Some(s) => Some(s),
                    None if settings.ore_veins => ore_vein(&nc, router, &ore_random, bx, y, bz),
                    None => None,
                }
                .unwrap_or(settings.default_block);
                out[(((y - MIN_Y) * 16 + z) * 16 + x) as usize] = state;
            }
        }
    }
    out
}
