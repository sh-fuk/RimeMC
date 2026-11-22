//! Bit-exact ports of vanilla's random sources (Xoroshiro128++ and the
//! legacy Java LCG) and their positional factories.

use md5::{Digest, Md5};

pub const GOLDEN_RATIO_64: i64 = -7046029254386353131;
pub const SILVER_RATIO_64: i64 = 7640891576956012809;

pub fn mix_stafford13(mut z: i64) -> i64 {
    z = (z ^ ((z as u64) >> 30) as i64).wrapping_mul(-4658895280553007687);
    z = (z ^ ((z as u64) >> 27) as i64).wrapping_mul(-7723592293110705685);
    z ^ ((z as u64) >> 31) as i64
}

pub fn upgrade_seed_to_128(seed: i64) -> (i64, i64) {
    let lo = seed ^ SILVER_RATIO_64;
    let hi = lo.wrapping_add(GOLDEN_RATIO_64);
    (mix_stafford13(lo), mix_stafford13(hi))
}

pub fn seed_from_hash_of(s: &str) -> (i64, i64) {
    let d = Md5::digest(s.as_bytes());
    (i64::from_be_bytes(d[0..8].try_into().unwrap()), i64::from_be_bytes(d[8..16].try_into().unwrap()))
}

/// `Mth.getSeed(x, y, z)`
pub fn block_seed(x: i32, y: i32, z: i32) -> i64 {
    let mut l = (x.wrapping_mul(3129871) as i64) ^ (z as i64).wrapping_mul(116129781) ^ y as i64;
    l = l.wrapping_mul(l).wrapping_mul(42317861).wrapping_add(l.wrapping_mul(11));
    l >> 16
}

pub trait Rng {
    fn next_long(&mut self) -> i64;
    fn next_int(&mut self) -> i32;
    fn next_int_bound(&mut self, bound: i32) -> i32;
    fn next_double(&mut self) -> f64;
    fn next_float(&mut self) -> f32;
    fn next_bool(&mut self) -> bool;
    fn next_gaussian(&mut self) -> f64 {
        unimplemented!("gaussian on this random source")
    }
    fn consume(&mut self, n: usize) {
        for _ in 0..n {
            self.next_int();
        }
    }
    fn next_int_between(&mut self, min: i32, max_inclusive: i32) -> i32 {
        self.next_int_bound(max_inclusive - min + 1) + min
    }
}

/// `XoroshiroRandomSource`; with `leg` set it behaves as the
/// `LegacyRandomSource` of a dimension whose noise settings use
/// `legacy_random_source` (nether, end).
#[derive(Clone, Debug)]
pub struct Xoroshiro {
    lo: i64,
    hi: i64,
    leg: Option<Legacy>,
}

impl Xoroshiro {
    pub fn new(seed: i64) -> Self {
        let (lo, hi) = upgrade_seed_to_128(seed);
        Self::from_parts(lo, hi)
    }
    pub fn from_parts(lo: i64, hi: i64) -> Self {
        if lo == 0 && hi == 0 { Self { lo: GOLDEN_RATIO_64, hi: SILVER_RATIO_64, leg: None } } else { Self { lo, hi, leg: None } }
    }
    pub fn from_legacy(l: Legacy) -> Self {
        Self { lo: 0, hi: 0, leg: Some(l) }
    }
    fn bits(&mut self, n: u32) -> u64 {
        (self.next_long() as u64) >> (64 - n)
    }
    pub fn fork(&mut self) -> Xoroshiro {
        if let Some(l) = &mut self.leg {
            return Xoroshiro::from_legacy(Legacy::new(l.next_long()));
        }
        let a = self.next_long();
        let b = self.next_long();
        Xoroshiro::from_parts(a, b)
    }
    pub fn fork_positional(&mut self) -> XoroshiroFactory {
        if let Some(l) = &mut self.leg {
            return XoroshiroFactory { lo: l.next_long(), hi: 0, leg: true };
        }
        let a = self.next_long();
        let b = self.next_long();
        XoroshiroFactory { lo: a, hi: b, leg: false }
    }
}

impl Rng for Xoroshiro {
    fn next_gaussian(&mut self) -> f64 {
        match &mut self.leg {
            Some(l) => l.next_gaussian(),
            None => unimplemented!("gaussian on this random source"),
        }
    }
    fn next_long(&mut self) -> i64 {
        if let Some(l) = &mut self.leg {
            return l.next_long();
        }
        let (l, mut m) = (self.lo, self.hi);
        let n = l.wrapping_add(m).rotate_left(17).wrapping_add(l);
        m ^= l;
        self.lo = l.rotate_left(49) ^ m ^ (m << 21);
        self.hi = m.rotate_left(28);
        n
    }
    fn next_int(&mut self) -> i32 {
        if let Some(l) = &mut self.leg {
            return l.next_int();
        }
        self.next_long() as i32
    }
    fn next_int_bound(&mut self, bound: i32) -> i32 {
        if let Some(l) = &mut self.leg {
            return l.next_int_bound(bound);
        }
        assert!(bound > 0);
        let b = bound as u64;
        let mut l = self.next_int() as u32 as u64;
        let mut m = l.wrapping_mul(b);
        let mut n = m & 0xFFFF_FFFF;
        if n < b {
            let i = ((!(bound as u32)).wrapping_add(1) % bound as u32) as u64;
            while n < i {
                l = self.next_int() as u32 as u64;
                m = l.wrapping_mul(b);
                n = m & 0xFFFF_FFFF;
            }
        }
        (m >> 32) as i32
    }
    fn next_double(&mut self) -> f64 {
        if let Some(l) = &mut self.leg {
            return l.next_double();
        }
        self.bits(53) as f64 * 1.1102230246251565E-16
    }
    fn next_float(&mut self) -> f32 {
        if let Some(l) = &mut self.leg {
            return l.next_float();
        }
        self.bits(24) as f32 * 5.9604645E-8
    }
    fn next_bool(&mut self) -> bool {
        if let Some(l) = &mut self.leg {
            return l.next_bool();
        }
        self.next_long() & 1 != 0
    }
    fn consume(&mut self, n: usize) {
        if let Some(l) = &mut self.leg {
            return l.consume(n);
        }
        for _ in 0..n {
            self.next_long();
        }
    }
}

/// `PositionalRandomFactory` (xoroshiro, or legacy when `leg`).
#[derive(Clone, Copy, Debug)]
pub struct XoroshiroFactory {
    pub lo: i64,
    pub hi: i64,
    pub leg: bool,
}

impl XoroshiroFactory {
    /// `LegacyRandomSource(seed).forkPositional()`
    pub fn legacy_root(seed: i64) -> Self {
        XoroshiroFactory { lo: Legacy::new(seed).next_long(), hi: 0, leg: true }
    }
    pub fn at(&self, x: i32, y: i32, z: i32) -> Xoroshiro {
        if self.leg {
            return Xoroshiro::from_legacy(Legacy::new(block_seed(x, y, z) ^ self.lo));
        }
        Xoroshiro::from_parts(block_seed(x, y, z) ^ self.lo, self.hi)
    }
    pub fn from_hash_of(&self, s: &str) -> Xoroshiro {
        if self.leg {
            return Xoroshiro::from_legacy(Legacy::new(java_string_hash(s) as i64 ^ self.lo));
        }
        let (a, b) = seed_from_hash_of(s);
        Xoroshiro::from_parts(a ^ self.lo, b ^ self.hi)
    }
    pub fn from_seed(&self, seed: i64) -> Xoroshiro {
        if self.leg {
            return Xoroshiro::from_legacy(Legacy::new(seed ^ self.lo));
        }
        Xoroshiro::from_parts(seed ^ self.lo, seed ^ self.hi)
    }
}

/// `java.util.Random`-compatible LCG (vanilla's LegacyRandomSource).
#[derive(Clone, Debug)]
pub struct Legacy {
    seed: i64,
    next_gaussian: Option<f64>,
}

const MULT: i64 = 0x5DEECE66D;
const MASK: i64 = (1 << 48) - 1;

impl Legacy {
    pub fn new(seed: i64) -> Self {
        Self { seed: (seed ^ MULT) & MASK, next_gaussian: None }
    }
    pub fn set_seed(&mut self, seed: i64) {
        self.seed = (seed ^ MULT) & MASK;
        self.next_gaussian = None;
    }
    pub fn next(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(MULT).wrapping_add(11) & MASK;
        (self.seed >> (48 - bits)) as i32
    }
    pub fn fork_positional(&mut self) -> LegacyFactory {
        LegacyFactory { seed: self.next_long() }
    }
    pub fn next_gaussian(&mut self) -> f64 {
        if let Some(g) = self.next_gaussian.take() {
            return g;
        }
        loop {
            let a = 2.0 * self.next_double() - 1.0;
            let b = 2.0 * self.next_double() - 1.0;
            let s = a * a + b * b;
            if s < 1.0 && s != 0.0 {
                let m = (-2.0 * s.ln() / s).sqrt();
                self.next_gaussian = Some(b * m);
                return a * m;
            }
        }
    }
}

impl Rng for Legacy {
    fn next_gaussian(&mut self) -> f64 {
        Legacy::next_gaussian(self)
    }
    fn next_long(&mut self) -> i64 {
        let a = self.next(32) as i64;
        let b = self.next(32) as i64;
        (a << 32).wrapping_add(b)
    }
    fn next_int(&mut self) -> i32 {
        self.next(32)
    }
    fn next_int_bound(&mut self, bound: i32) -> i32 {
        assert!(bound > 0);
        if bound & (bound - 1) == 0 {
            return ((bound as i64 * self.next(31) as i64) >> 31) as i32;
        }
        loop {
            let i = self.next(31);
            let j = i % bound;
            if i.wrapping_sub(j).wrapping_add(bound - 1) >= 0 {
                return j;
            }
        }
    }
    fn next_double(&mut self) -> f64 {
        let i = self.next(26) as i64;
        let j = self.next(27) as i64;
        ((i << 27) + j) as f64 * 1.110223E-16f32 as f64
    }
    fn next_float(&mut self) -> f32 {
        self.next(24) as f32 * 5.9604645E-8
    }
    fn next_bool(&mut self) -> bool {
        self.next(1) != 0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LegacyFactory {
    pub seed: i64,
}

impl LegacyFactory {
    pub fn at(&self, x: i32, y: i32, z: i32) -> Legacy {
        Legacy::new(block_seed(x, y, z) ^ self.seed)
    }
    pub fn from_hash_of(&self, s: &str) -> Legacy {
        Legacy::new(java_string_hash(s) as i64 ^ self.seed)
    }
}

pub fn java_string_hash(s: &str) -> i32 {
    s.encode_utf16().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_random_matches() {
        // new java.util.Random(42).nextInt() == -1170105035
        let mut r = Legacy::new(42);
        assert_eq!(r.next_int(), -1170105035);
        assert_eq!(r.next_int_bound(10), 3);
    }
}

/// `WorldgenRandom` wrapping a `XoroshiroRandomSource`: every draw goes
/// through `next(bits)` = top bits of the xoroshiro `nextLong`, using the
/// `BitRandomSource` (java.util.Random style) algorithms.
#[derive(Clone, Debug)]
pub struct WgRandom {
    x: Xoroshiro,
    gauss: Option<f64>,
}

impl WgRandom {
    pub fn new(seed: i64) -> Self {
        WgRandom { x: Xoroshiro::new(seed), gauss: None }
    }
    /// `setSeed` (does not reset the cached gaussian, like vanilla)
    pub fn set_seed(&mut self, seed: i64) {
        self.x = Xoroshiro::new(seed);
    }
    pub fn next(&mut self, bits: u32) -> i32 {
        ((self.x.next_long() as u64) >> (64 - bits)) as i32
    }
    /// `setDecorationSeed`; returns the decoration seed.
    pub fn set_decoration_seed(&mut self, seed: i64, x: i32, z: i32) -> i64 {
        self.set_seed(seed);
        let a = self.next_long() | 1;
        let b = self.next_long() | 1;
        let s = (x as i64).wrapping_mul(a).wrapping_add((z as i64).wrapping_mul(b)) ^ seed;
        self.set_seed(s);
        s
    }
    /// `setFeatureSeed`
    pub fn set_feature_seed(&mut self, decoration: i64, index: i32, step: i32) {
        self.set_seed(decoration.wrapping_add(index as i64).wrapping_add(10000 * step as i64));
    }
    pub fn next_gaussian(&mut self) -> f64 {
        if let Some(g) = self.gauss.take() {
            return g;
        }
        loop {
            let a = 2.0 * self.next_double() - 1.0;
            let b = 2.0 * self.next_double() - 1.0;
            let s = a * a + b * b;
            if s < 1.0 && s != 0.0 {
                let m = (-2.0 * s.ln() / s).sqrt();
                self.gauss = Some(b * m);
                return a * m;
            }
        }
    }
}

impl Rng for WgRandom {
    fn next_gaussian(&mut self) -> f64 {
        WgRandom::next_gaussian(self)
    }
    fn next_long(&mut self) -> i64 {
        let a = self.next(32) as i64;
        let b = self.next(32) as i64;
        (a << 32).wrapping_add(b)
    }
    fn next_int(&mut self) -> i32 {
        self.next(32)
    }
    fn next_int_bound(&mut self, bound: i32) -> i32 {
        assert!(bound > 0);
        if bound & (bound - 1) == 0 {
            return ((bound as i64 * self.next(31) as i64) >> 31) as i32;
        }
        loop {
            let i = self.next(31);
            let j = i % bound;
            if i.wrapping_sub(j).wrapping_add(bound - 1) >= 0 {
                return j;
            }
        }
    }
    fn next_double(&mut self) -> f64 {
        let i = self.next(26) as i64;
        let j = self.next(27) as i64;
        ((i << 27) + j) as f64 * 1.110223E-16f32 as f64
    }
    fn next_float(&mut self) -> f32 {
        self.next(24) as f32 * 5.9604645E-8
    }
    fn next_bool(&mut self) -> bool {
        self.next(1) != 0
    }
}
