//! Block collision shapes (dumped from vanilla 26.1, `assets/26.1/collision.txt.gz`),
//! `Entity.collide` with step-up, and `Level.clip` for the collider shapes.

use std::io::Read;
use std::sync::OnceLock;

use crate::anticheat::combat::Aabb;
use crate::registry::BlockState;

const EPS: f64 = 1.0e-7;

struct Table {
    shapes: Vec<Vec<Aabb>>,
    of_state: Vec<u16>,
}

fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| {
        let mut raw = String::new();
        flate2::read::GzDecoder::new(&include_bytes!("../../assets/26.1/collision.txt.gz")[..]).read_to_string(&mut raw).expect("collision shapes");
        let mut lines = raw.lines();
        let n: usize = lines.next().and_then(|l| l.parse().ok()).expect("shape count");
        let mut shapes = Vec::with_capacity(n);
        for _ in 0..n {
            let l = lines.next().unwrap_or("");
            let boxes = l
                .split(';')
                .filter(|b| !b.is_empty())
                .map(|b| {
                    let v: Vec<f64> = b.split(',').map(|x| x.parse().unwrap_or(0.0)).collect();
                    Aabb { min: [v[0], v[1], v[2]], max: [v[3], v[4], v[5]] }
                })
                .collect();
            shapes.push(boxes);
        }
        let states: usize = lines.next().and_then(|l| l.parse().ok()).expect("state count");
        let of_state = (0..states).map(|_| lines.next().and_then(|l| l.parse().ok()).unwrap_or(0)).collect();
        Table { shapes, of_state }
    })
}

/// The collision boxes of a state, relative to its block origin.
pub fn collision(s: BlockState) -> &'static [Aabb] {
    let t = table();
    t.of_state.get(s.0 as usize).and_then(|i| t.shapes.get(*i as usize)).map(|v| v.as_slice()).unwrap_or(&[])
}

pub fn has_collision(s: BlockState) -> bool {
    !collision(s).is_empty()
}

impl Aabb {
    pub fn translate(&self, d: [f64; 3]) -> Aabb {
        Aabb { min: [self.min[0] + d[0], self.min[1] + d[1], self.min[2] + d[2]], max: [self.max[0] + d[0], self.max[1] + d[1], self.max[2] + d[2]] }
    }
    /// `AABB.expandTowards`
    pub fn expand_towards(&self, d: [f64; 3]) -> Aabb {
        let mut r = *self;
        for i in 0..3 {
            if d[i] < 0.0 {
                r.min[i] += d[i];
            } else {
                r.max[i] += d[i];
            }
        }
        r
    }
    /// strict overlap
    pub fn overlaps(&self, o: &Aabb) -> bool {
        (0..3).all(|i| self.min[i] < o.max[i] && self.max[i] > o.min[i])
    }
    pub fn contains(&self, p: [f64; 3]) -> bool {
        (0..3).all(|i| p[i] >= self.min[i] && p[i] < self.max[i])
    }
    /// `AABB.distanceToSqr(AABB)`
    pub fn distance_sqr_box(&self, o: &Aabb) -> f64 {
        let mut s = 0.0;
        for i in 0..3 {
            let d = (self.min[i] - o.max[i]).max(o.min[i] - self.max[i]).max(0.0);
            s += d * d;
        }
        s
    }
    /// `AABB.clip`: entry point of the segment into the box and the face hit
    /// (hits need `0 < t < 1`; a segment ending on a face misses).
    pub fn clip(&self, from: [f64; 3], to: [f64; 3]) -> Option<([f64; 3], u8)> {
        const E: f64 = 1.0e-7;
        let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let mut scale = 1.0f64;
        let mut face = 255u8;
        // AABB.getDirection: per axis, the face the segment enters through
        for a in 0..3 {
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            let (plane, f) = if d[a] > E {
                (self.min[a], axis_face(a, false))
            } else if d[a] < -E {
                (self.max[a], axis_face(a, true))
            } else {
                continue;
            };
            let t = (plane - from[a]) / d[a];
            let pb = from[b] + t * d[b];
            let pc = from[c] + t * d[c];
            if 0.0 < t && t < scale && self.min[b] - E < pb && pb < self.max[b] + E && self.min[c] - E < pc && pc < self.max[c] + E {
                scale = t;
                face = f;
            }
        }
        if face == 255 {
            return None;
        }
        Some(([from[0] + d[0] * scale, from[1] + d[1] * scale, from[2] + d[2] * scale], face))
    }
}

/// Direction ids: 0 down, 1 up, 2 north, 3 south, 4 west, 5 east.
/// `max_side`: the face on the max side of the axis was hit.
fn axis_face(axis: usize, max_side: bool) -> u8 {
    match (axis, max_side) {
        (0, false) => 4,
        (0, true) => 5,
        (1, false) => 0,
        (1, true) => 1,
        (2, false) => 2,
        _ => 3,
    }
}

pub fn face_normal(f: u8) -> [i32; 3] {
    match f {
        0 => [0, -1, 0],
        1 => [0, 1, 0],
        2 => [0, 0, -1],
        3 => [0, 0, 1],
        4 => [-1, 0, 0],
        _ => [1, 0, 0],
    }
}

/// A block hit of a clip.
#[derive(Clone, Copy, Debug)]
pub struct BlockHit {
    pub pos: [f64; 3],
    pub block: [i32; 3],
    pub face: u8,
    pub state: BlockState,
}

/// Something that can answer block lookups.
pub trait Blocks {
    fn block(&self, x: i32, y: i32, z: i32) -> Option<BlockState>;
}

impl Blocks for super::World {
    fn block(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
        self.get_block(x, y, z)
    }
}

/// The collision boxes (world space) of every block touching `area`.
pub fn colliders(w: &impl Blocks, area: &Aabb) -> Vec<Aabb> {
    let mut out = Vec::new();
    let lo = [(area.min[0] - EPS).floor() as i32 - 1, (area.min[1] - EPS).floor() as i32 - 1, (area.min[2] - EPS).floor() as i32 - 1];
    let hi = [(area.max[0] + EPS).floor() as i32 + 1, (area.max[1] + EPS).floor() as i32 + 1, (area.max[2] + EPS).floor() as i32 + 1];
    for x in lo[0]..=hi[0] {
        for z in lo[2]..=hi[2] {
            for y in lo[1]..=hi[1] {
                let Some(s) = w.block(x, y, z) else { continue };
                for b in collision(s) {
                    let b = b.translate([x as f64, y as f64, z as f64]);
                    if b.overlaps(area) {
                        out.push(b);
                    }
                }
            }
        }
    }
    out
}

/// `Shapes.collide` along one axis.
fn collide_axis(axis: usize, bx: &Aabb, shapes: &[Aabb], mut m: f64) -> f64 {
    let (o1, o2) = ((axis + 1) % 3, (axis + 2) % 3);
    for s in shapes {
        if m.abs() < EPS {
            return 0.0;
        }
        if !(s.max[o1] - EPS > bx.min[o1] && s.min[o1] + EPS < bx.max[o1] && s.max[o2] - EPS > bx.min[o2] && s.min[o2] + EPS < bx.max[o2]) {
            continue;
        }
        if m > 0.0 && s.min[axis] >= bx.max[axis] - EPS {
            m = m.min(s.min[axis] - bx.max[axis]);
        } else if m < 0.0 && s.max[axis] <= bx.min[axis] + EPS {
            m = m.max(s.max[axis] - bx.min[axis]);
        }
    }
    m
}

/// `Entity.collideWithShapes`
pub fn collide_with_shapes(m: [f64; 3], bx: &Aabb, shapes: &[Aabb]) -> [f64; 3] {
    if shapes.is_empty() {
        return m;
    }
    let order: [usize; 3] = if m[0].abs() < m[2].abs() { [1, 2, 0] } else { [1, 0, 2] };
    let mut r = [0.0; 3];
    for a in order {
        if m[a] != 0.0 {
            r[a] = collide_axis(a, &bx.translate(r), shapes, m[a]);
        }
    }
    r
}

/// `Entity.collide`: movement after block collisions, stepping up to
/// `max_up_step` when blocked horizontally.
pub fn collide(w: &impl Blocks, bx: &Aabb, m: [f64; 3], on_ground: bool, max_up_step: f32) -> [f64; 3] {
    let step = if m == [0.0; 3] { m } else { collide_with_shapes(m, bx, &colliders(w, &bx.expand_towards(m))) };
    let (xc, yc, zc) = (m[0] != step[0], m[1] != step[1], m[2] != step[2]);
    let ground_after = yc && m[1] < 0.0;
    if max_up_step > 0.0 && (ground_after || on_ground) && (xc || zc) {
        let grounded = if ground_after { bx.translate([0.0, step[1], 0.0]) } else { *bx };
        let mut up = grounded.expand_towards([m[0], max_up_step as f64, m[2]]);
        if !ground_after {
            up = up.expand_towards([0.0, -1.0e-5, 0.0]);
        }
        let shapes = colliders(w, &up);
        // collectCandidateStepUpHeights
        let skip = step[1] as f32;
        let mut cands: Vec<f32> = Vec::new();
        for s in &shapes {
            for c in [s.min[1], s.max[1]] {
                let rel = (c - grounded.min[1]) as f32;
                if rel >= 0.0 && rel != skip && rel <= max_up_step && !cands.contains(&rel) {
                    cands.push(rel);
                }
            }
        }
        cands.sort_by(|a, b| a.total_cmp(b));
        for h in cands {
            let s = collide_with_shapes([m[0], h as f64, m[2]], &grounded, &shapes);
            if s[0] * s[0] + s[2] * s[2] > step[0] * step[0] + step[2] * step[2] {
                let dist = bx.min[1] - grounded.min[1];
                return [s[0], s[1] - dist, s[2]];
            }
        }
    }
    step
}

/// Is any collision box overlapping `area`?
pub fn no_collision(w: &impl Blocks, area: &Aabb) -> bool {
    colliders(w, area).is_empty()
}

/// `Level.clip` with `ClipContext.Block.COLLIDER` and no fluids.
pub fn clip(w: &impl Blocks, from: [f64; 3], to: [f64; 3]) -> Option<BlockHit> {
    if from == to {
        return None;
    }
    // BlockGetter.traverseBlocks
    let (fx, fy, fz) = (lerp(-EPS, to[0], from[0]), lerp(-EPS, to[1], from[1]), lerp(-EPS, to[2], from[2]));
    let (tx, ty, tz) = (lerp(-EPS, from[0], to[0]), lerp(-EPS, from[1], to[1]), lerp(-EPS, from[2], to[2]));
    let (mut x, mut y, mut z) = (tx.floor() as i32, ty.floor() as i32, tz.floor() as i32);
    let check = |x: i32, y: i32, z: i32| -> Option<BlockHit> {
        let s = w.block(x, y, z)?;
        let mut best: Option<([f64; 3], u8)> = None;
        let mut best_d = f64::MAX;
        let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < 1.0e-7 {
            return None;
        }
        // VoxelShape.clip: a segment starting inside the shape hits right away
        let probe = [from[0] + d[0] * 0.001, from[1] + d[1] * 0.001, from[2] + d[2] * 0.001];
        for b in collision(s) {
            if b.translate([x as f64, y as f64, z as f64]).contains(probe) {
                let (ax, _) = (0..3).map(|i| (i, d[i].abs())).fold((0, -1.0), |acc, v| if v.1 > acc.1 { v } else { acc });
                // Direction.getApproximateNearest(d).getOpposite()
                return Some(BlockHit { pos: probe, block: [x, y, z], face: axis_face(ax, d[ax] < 0.0), state: s });
            }
        }
        for b in collision(s) {
            if let Some((p, f)) = b.translate([x as f64, y as f64, z as f64]).clip(from, to) {
                let d = (p[0] - from[0]).powi(2) + (p[1] - from[1]).powi(2) + (p[2] - from[2]).powi(2);
                if d < best_d {
                    best_d = d;
                    best = Some((p, f));
                }
            }
        }
        best.map(|(pos, face)| BlockHit { pos, block: [x, y, z], face, state: s })
    };
    if let Some(h) = check(x, y, z) {
        return Some(h);
    }
    let (dx, dy, dz) = (fx - tx, fy - ty, fz - tz);
    // Mth.sign: 0 for 0 (f64::signum(0.0) is 1.0)
    let sign = |v: f64| if v > 0.0 { 1 } else if v < 0.0 { -1 } else { 0 };
    let (sx, sy, sz) = (sign(dx), sign(dy), sign(dz));
    let (ix, iy, iz) = (if sx == 0 { f64::MAX } else { sx as f64 / dx }, if sy == 0 { f64::MAX } else { sy as f64 / dy }, if sz == 0 { f64::MAX } else { sz as f64 / dz });
    let frac = |v: f64| v - v.floor();
    let mut px = ix * if sx > 0 { 1.0 - frac(tx) } else { frac(tx) };
    let mut py = iy * if sy > 0 { 1.0 - frac(ty) } else { frac(ty) };
    let mut pz = iz * if sz > 0 { 1.0 - frac(tz) } else { frac(tz) };
    let mut guard = 0;
    while (px <= 1.0 || py <= 1.0 || pz <= 1.0) && guard < 1024 {
        guard += 1;
        if px < py {
            if px < pz {
                x += sx;
                px += ix;
            } else {
                z += sz;
                pz += iz;
            }
        } else if py < pz {
            y += sy;
            py += iy;
        } else {
            z += sz;
            pz += iz;
        }
        if let Some(h) = check(x, y, z) {
            return Some(h);
        }
    }
    None
}

/// Java `Math.signum` / `Mth.sign`: 0 for 0 (Rust's `signum(0.0)` is 1).
pub fn jsign(v: f64) -> f64 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        0.0
    }
}

fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{blocks, BlockRegistry};

    struct Flat;
    impl Blocks for Flat {
        fn block(&self, _x: i32, y: i32, _z: i32) -> Option<BlockState> {
            Some(if y < 64 { blocks::STONE } else { blocks::AIR })
        }
    }

    #[test]
    fn shapes() {
        BlockRegistry::get();
        assert_eq!(collision(blocks::AIR).len(), 0);
        assert_eq!(collision(blocks::STONE).len(), 1);
    }

    #[test]
    fn landing_and_clip() {
        let bx = Aabb::entity([0.5, 64.5, 0.5], 0.6, 1.8);
        let m = collide(&Flat, &bx, [0.0, -1.0, 0.0], false, 0.6);
        assert!((m[1] + 0.5).abs() < 1e-9, "{m:?}");
        let h = clip(&Flat, [0.5, 70.0, 0.5], [0.5, 60.0, 0.5]).expect("hit");
        assert_eq!(h.block, [0, 63, 0]);
        assert_eq!(h.face, 1);
        assert!((h.pos[1] - 64.0).abs() < 1e-9);
    }

    #[test]
    fn step_up() {
        struct Step;
        impl Blocks for Step {
            fn block(&self, x: i32, y: i32, _z: i32) -> Option<BlockState> {
                Some(if y < 64 || (x >= 1 && y == 64) { blocks::STONE } else { blocks::AIR })
            }
        }
        let bx = Aabb::entity([0.6, 64.0, 0.5], 0.6, 1.8);
        // a full block is higher than a 0.6 step: blocked
        let m = collide(&Step, &bx, [0.2, 0.0, 0.0], true, 0.6);
        assert!(m[0] < 0.2 && m[1] == 0.0, "{m:?}");
        let m = collide(&Step, &bx, [0.2, 0.0, 0.0], true, 1.0);
        assert!((m[1] - 1.0).abs() < 1e-6 && (m[0] - 0.2).abs() < 1e-9, "{m:?}");
    }
}
