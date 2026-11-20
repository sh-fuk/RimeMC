//! Bit-exact ports of vanilla's ImprovedNoise / PerlinNoise / NormalNoise /
//! BlendedNoise.

use super::random::{Legacy, Rng, Xoroshiro};

#[inline]
pub fn floor(d: f64) -> i32 {
    let i = d as i32;
    if d < i as f64 { i - 1 } else { i }
}
#[inline]
pub fn lfloor(d: f64) -> i64 {
    let i = d as i64;
    if d < i as f64 { i - 1 } else { i }
}
#[inline]
pub fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}
#[inline]
pub fn lerp2(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> f64 {
    lerp(b, lerp(a, c, d), lerp(a, e, f))
}
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn lerp3(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, i: f64, j: f64, k: f64) -> f64 {
    lerp(c, lerp2(a, b, d, e, f, g), lerp2(a, b, h, i, j, k))
}
#[inline]
pub fn clamped_lerp(t: f64, a: f64, b: f64) -> f64 {
    if t < 0.0 {
        a
    } else if t > 1.0 {
        b
    } else {
        lerp(t, a, b)
    }
}
#[inline]
pub fn smoothstep(d: f64) -> f64 {
    d * d * d * (d * (d * 6.0 - 15.0) + 10.0)
}
#[inline]
pub fn inverse_lerp(v: f64, a: f64, b: f64) -> f64 {
    (v - a) / (b - a)
}
#[inline]
pub fn clamped_map(v: f64, a: f64, b: f64, c: f64, d: f64) -> f64 {
    clamped_lerp(inverse_lerp(v, a, b), c, d)
}
pub fn wrap(d: f64) -> f64 {
    d - lfloor(d / 3.3554432E7 + 0.5) as f64 * 3.3554432E7
}

const GRADIENT: [[f64; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

#[inline]
fn grad_dot(i: i32, x: f64, y: f64, z: f64) -> f64 {
    let g = &GRADIENT[(i & 15) as usize];
    g[0] * x + g[1] * y + g[2] * z
}

pub enum AnyRng<'a> {
    X(&'a mut Xoroshiro),
    L(&'a mut Legacy),
}
impl AnyRng<'_> {
    fn r(&mut self) -> &mut dyn Rng {
        match self {
            AnyRng::X(x) => *x,
            AnyRng::L(l) => *l,
        }
    }
}

#[derive(Clone)]
pub struct ImprovedNoise {
    p: [u8; 256],
    pub xo: f64,
    pub yo: f64,
    pub zo: f64,
}

impl ImprovedNoise {
    pub fn new(r: &mut dyn Rng) -> Self {
        let xo = r.next_double() * 256.0;
        let yo = r.next_double() * 256.0;
        let zo = r.next_double() * 256.0;
        let mut p = [0u8; 256];
        for (i, v) in p.iter_mut().enumerate() {
            *v = i as u8;
        }
        for i in 0..256 {
            let j = r.next_int_bound(256 - i as i32) as usize;
            p.swap(i, i + j);
        }
        Self { p, xo, yo, zo }
    }

    #[inline]
    fn p(&self, i: i32) -> i32 {
        self.p[(i & 255) as usize] as i32
    }

    pub fn noise(&self, x: f64, y: f64, z: f64) -> f64 {
        self.noise_scaled(x, y, z, 0.0, 0.0)
    }

    pub fn noise_scaled(&self, x: f64, y: f64, z: f64, y_scale: f64, y_max: f64) -> f64 {
        let d = x + self.xo;
        let e = y + self.yo;
        let f = z + self.zo;
        let i = floor(d);
        let j = floor(e);
        let k = floor(f);
        let g = d - i as f64;
        let h = e - j as f64;
        let l = f - k as f64;
        let n = if y_scale != 0.0 {
            let m = if y_max >= 0.0 && y_max < h { y_max } else { h };
            floor(m / y_scale + 1.0E-7f32 as f64) as f64 * y_scale
        } else {
            0.0
        };
        self.sample_and_lerp(i, j, k, g, h - n, l, h)
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_and_lerp(&self, x: i32, y: i32, z: i32, dx: f64, dy: f64, dz: f64, fade_y: f64) -> f64 {
        let i = self.p(x);
        let j = self.p(x.wrapping_add(1));
        let k = self.p(i + y);
        let l = self.p(i + y + 1);
        let m = self.p(j + y);
        let n = self.p(j + y + 1);
        let d = grad_dot(self.p(k + z), dx, dy, dz);
        let e = grad_dot(self.p(m + z), dx - 1.0, dy, dz);
        let f = grad_dot(self.p(l + z), dx, dy - 1.0, dz);
        let g = grad_dot(self.p(n + z), dx - 1.0, dy - 1.0, dz);
        let h = grad_dot(self.p(k + z + 1), dx, dy, dz - 1.0);
        let o = grad_dot(self.p(m + z + 1), dx - 1.0, dy, dz - 1.0);
        let q = grad_dot(self.p(l + z + 1), dx, dy - 1.0, dz - 1.0);
        let r = grad_dot(self.p(n + z + 1), dx - 1.0, dy - 1.0, dz - 1.0);
        lerp3(smoothstep(dx), smoothstep(fade_y), smoothstep(dz), d, e, f, g, h, o, q, r)
    }
}

#[derive(Clone)]
pub struct PerlinNoise {
    levels: Vec<Option<ImprovedNoise>>,
    amplitudes: Vec<f64>,
    lowest_freq_value_factor: f64,
    lowest_freq_input_factor: f64,
    pub max_value: f64,
}

impl PerlinNoise {
    /// `PerlinNoise.create(random, firstOctave, amplitudes)` (new initialization).
    pub fn new(r: &mut Xoroshiro, first_octave: i32, amplitudes: &[f64]) -> Self {
        let f = r.fork_positional();
        let levels = amplitudes
            .iter()
            .enumerate()
            .map(|(k, &a)| if a != 0.0 { Some(ImprovedNoise::new(&mut f.from_hash_of(&format!("octave_{}", first_octave + k as i32)))) } else { None })
            .collect();
        Self::finish(levels, first_octave, amplitudes.to_vec())
    }

    /// `PerlinNoise.create` with a legacy random (new initialization).
    pub fn new_legacy(r: &mut Legacy, first_octave: i32, amplitudes: &[f64]) -> Self {
        let f = r.fork_positional();
        let levels = amplitudes
            .iter()
            .enumerate()
            .map(|(k, &a)| if a != 0.0 { Some(ImprovedNoise::new(&mut f.from_hash_of(&format!("octave_{}", first_octave + k as i32)))) } else { None })
            .collect();
        Self::finish(levels, first_octave, amplitudes.to_vec())
    }

    /// Legacy initialization (`createLegacyForBlendedNoise`): octaves
    /// `first..=0`, all with amplitude 1.
    pub fn legacy(r: &mut AnyRng, first_octave: i32) -> Self {
        let i = (-first_octave + 1) as usize;
        let j = -first_octave as usize;
        let amplitudes = vec![1.0; i];
        let mut levels: Vec<Option<ImprovedNoise>> = vec![None; i];
        levels[j] = Some(ImprovedNoise::new(r.r()));
        for k in (0..j).rev() {
            levels[k] = Some(ImprovedNoise::new(r.r()));
        }
        Self::finish(levels, first_octave, amplitudes)
    }

    fn finish(levels: Vec<Option<ImprovedNoise>>, first_octave: i32, amplitudes: Vec<f64>) -> Self {
        let i = amplitudes.len() as i32;
        let j = -first_octave;
        let mut s = Self {
            levels,
            amplitudes,
            lowest_freq_input_factor: 2f64.powi(-j),
            lowest_freq_value_factor: 2f64.powi(i - 1) / (2f64.powi(i) - 1.0),
            max_value: 0.0,
        };
        s.max_value = s.edge_value(2.0);
        s
    }

    fn edge_value(&self, d: f64) -> f64 {
        let mut e = 0.0;
        let mut f = self.lowest_freq_value_factor;
        for (l, a) in self.levels.iter().zip(&self.amplitudes) {
            if l.is_some() {
                e += a * d * f;
            }
            f /= 2.0;
        }
        e
    }

    pub fn max_broken_value(&self, d: f64) -> f64 {
        self.edge_value(d + 2.0)
    }

    pub fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        let mut d = 0.0;
        let mut e = self.lowest_freq_input_factor;
        let mut f = self.lowest_freq_value_factor;
        for (l, a) in self.levels.iter().zip(&self.amplitudes) {
            if let Some(n) = l {
                let g = n.noise(wrap(x * e), wrap(y * e), wrap(z * e));
                d += a * g * f;
            }
            e *= 2.0;
            f /= 2.0;
        }
        d
    }

    pub fn octave(&self, i: usize) -> Option<&ImprovedNoise> {
        self.levels[self.levels.len() - 1 - i].as_ref()
    }
}

#[derive(Clone)]
pub struct NormalNoise {
    value_factor: f64,
    first: PerlinNoise,
    second: PerlinNoise,
    pub max_value: f64,
}

impl NormalNoise {
    pub fn new(r: &mut Xoroshiro, first_octave: i32, amplitudes: &[f64]) -> Self {
        let first = PerlinNoise::new(r, first_octave, amplitudes);
        let second = PerlinNoise::new(r, first_octave, amplitudes);
        Self::finish(first, second, amplitudes)
    }

    /// `NormalNoise.create(new WorldgenRandom(new LegacyRandomSource(seed)), ...)`
    pub fn new_legacy(seed: i64, first_octave: i32, amplitudes: &[f64]) -> Self {
        let mut r = Legacy::new(seed);
        let first = PerlinNoise::new_legacy(&mut r, first_octave, amplitudes);
        let second = PerlinNoise::new_legacy(&mut r, first_octave, amplitudes);
        Self::finish(first, second, amplitudes)
    }

    fn finish(first: PerlinNoise, second: PerlinNoise, amplitudes: &[f64]) -> Self {
        let mut j = i32::MAX;
        let mut k = i32::MIN;
        for (l, &a) in amplitudes.iter().enumerate() {
            if a != 0.0 {
                j = j.min(l as i32);
                k = k.max(l as i32);
            }
        }
        let expected = 0.1 * (1.0 + 1.0 / (k - j + 1) as f64);
        let value_factor = 0.16666666666666666 / expected;
        let max_value = (first.max_value + second.max_value) * value_factor;
        Self { value_factor, first, second, max_value }
    }

    pub fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        const F: f64 = 1.0181268882175227;
        (self.first.value(x, y, z) + self.second.value(x * F, y * F, z * F)) * self.value_factor
    }
}

#[derive(Clone)]
pub struct BlendedNoise {
    min_limit: PerlinNoise,
    max_limit: PerlinNoise,
    main: PerlinNoise,
    xz_multiplier: f64,
    y_multiplier: f64,
    xz_factor: f64,
    y_factor: f64,
    smear_scale_multiplier: f64,
}

impl BlendedNoise {
    pub fn new(r: &mut AnyRng, xz_scale: f64, y_scale: f64, xz_factor: f64, y_factor: f64, smear: f64) -> Self {
        let min_limit = PerlinNoise::legacy(r, -15);
        let max_limit = PerlinNoise::legacy(r, -15);
        let main = PerlinNoise::legacy(r, -7);
        let xz_multiplier = 684.412 * xz_scale;
        let y_multiplier = 684.412 * y_scale;
        Self { min_limit, max_limit, main, xz_multiplier, y_multiplier, xz_factor, y_factor, smear_scale_multiplier: smear }
    }

    pub fn compute(&self, x: i32, y: i32, z: i32) -> f64 {
        let d = x as f64 * self.xz_multiplier;
        let e = y as f64 * self.y_multiplier;
        let f = z as f64 * self.xz_multiplier;
        let g = d / self.xz_factor;
        let h = e / self.y_factor;
        let i = f / self.xz_factor;
        let j = self.y_multiplier * self.smear_scale_multiplier;
        let k = j / self.y_factor;
        let (mut l, mut m, mut n) = (0.0, 0.0, 0.0);
        let mut o = 1.0;
        for p in 0..8 {
            if let Some(q) = self.main.octave(p) {
                n += q.noise_scaled(wrap(g * o), wrap(h * o), wrap(i * o), k * o, h * o) / o;
            }
            o /= 2.0;
        }
        let q = (n / 10.0 + 1.0) / 2.0;
        let bl2 = q >= 1.0;
        let bl3 = q <= 0.0;
        o = 1.0;
        for r in 0..16 {
            let s = wrap(d * o);
            let t = wrap(e * o);
            let u = wrap(f * o);
            let v = j * o;
            if !bl2 {
                if let Some(w) = self.min_limit.octave(r) {
                    l += w.noise_scaled(s, t, u, v, e * o) / o;
                }
            }
            if !bl3 {
                if let Some(w) = self.max_limit.octave(r) {
                    m += w.noise_scaled(s, t, u, v, e * o) / o;
                }
            }
            o /= 2.0;
        }
        clamped_lerp(q, l / 512.0, m / 512.0) / 128.0
    }
}

const F2: f64 = 0.3660254037844386; // 0.5 * (sqrt(3) - 1)
const G2: f64 = 0.21132486540518713; // (3 - sqrt(3)) / 6

#[derive(Clone)]
pub struct SimplexNoise {
    p: [i32; 512],
    pub xo: f64,
    pub yo: f64,
    pub zo: f64,
}

impl SimplexNoise {
    pub fn new(r: &mut dyn Rng) -> Self {
        let xo = r.next_double() * 256.0;
        let yo = r.next_double() * 256.0;
        let zo = r.next_double() * 256.0;
        let mut p = [0i32; 512];
        for (i, v) in p.iter_mut().enumerate().take(256) {
            *v = i as i32;
        }
        for i in 0..256 {
            let j = r.next_int_bound(256 - i as i32) as usize;
            p.swap(i, j + i);
        }
        Self { p, xo, yo, zo }
    }

    #[inline]
    fn p(&self, i: i32) -> i32 {
        self.p[(i & 255) as usize]
    }

    fn corner(i: i32, x: f64, y: f64, z: f64, base: f64) -> f64 {
        let f = base - x * x - y * y - z * z;
        if f < 0.0 {
            0.0
        } else {
            let f = f * f;
            let g = &GRADIENT[i as usize];
            f * f * (g[0] * x + g[1] * y + g[2] * z)
        }
    }

    pub fn value_2d(&self, d: f64, e: f64) -> f64 {
        let h = (d + e) * F2;
        let i = floor(d + h);
        let j = floor(e + h);
        let k = (i + j) as f64 * G2;
        let l = i as f64 - k;
        let m = j as f64 - k;
        let n = d - l;
        let o = e - m;
        let (q, r) = if n > o { (1, 0) } else { (0, 1) };
        let s = n - q as f64 + G2;
        let t = o - r as f64 + G2;
        let u = n - 1.0 + 2.0 * G2;
        let v = o - 1.0 + 2.0 * G2;
        let w = i & 255;
        let x = j & 255;
        let y = self.p(w + self.p(x)) % 12;
        let z = self.p(w + q + self.p(x + r)) % 12;
        let aa = self.p(w + 1 + self.p(x + 1)) % 12;
        let ab = Self::corner(y, n, o, 0.0, 0.5);
        let ac = Self::corner(z, s, t, 0.0, 0.5);
        let ad = Self::corner(aa, u, v, 0.0, 0.5);
        70.0 * (ab + ac + ad)
    }
}

/// `PerlinSimplexNoise` (only the 2D lookup used by biome temperature).
#[derive(Clone)]
pub struct PerlinSimplexNoise {
    levels: Vec<Option<SimplexNoise>>,
    highest_freq_input_factor: f64,
    highest_freq_value_factor: f64,
}

impl PerlinSimplexNoise {
    pub fn new(r: &mut Legacy, octaves: &[i32]) -> Self {
        let first = *octaves.iter().min().unwrap();
        let last = *octaves.iter().max().unwrap();
        let i = -first;
        let j = last;
        let k = (i + j + 1) as usize;
        let s = SimplexNoise::new(r);
        let l = j;
        let mut levels: Vec<Option<SimplexNoise>> = vec![None; k];
        let has = |o: i32| octaves.contains(&o);
        if l >= 0 && (l as usize) < k && has(0) {
            levels[l as usize] = Some(s.clone());
        }
        for m in (l + 1) as usize..k {
            if has(l - m as i32) {
                levels[m] = Some(SimplexNoise::new(r));
            } else {
                r.consume(262);
            }
        }
        assert!(j <= 0, "positive simplex octaves are not needed");
        Self { levels, highest_freq_input_factor: 2f64.powi(j), highest_freq_value_factor: 1.0 / (2f64.powi(k as i32) - 1.0) }
    }

    pub fn value(&self, x: f64, y: f64) -> f64 {
        let mut d = 0.0;
        let mut e = self.highest_freq_input_factor;
        let mut f = self.highest_freq_value_factor;
        for s in &self.levels {
            if let Some(s) = s {
                d += s.value_2d(x * e, y * e) * f;
            }
            e /= 2.0;
            f *= 2.0;
        }
        d
    }
}
