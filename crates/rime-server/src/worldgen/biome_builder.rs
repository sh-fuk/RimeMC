//! Port of vanilla's OverworldBiomeBuilder: the multi-noise parameter list
//! of the overworld biome source (same entries, same order).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct P {
    pub min: i64,
    pub max: i64,
}

pub fn q(f: f32) -> i64 {
    (f * 10000.0) as i64
}
fn span(a: f32, b: f32) -> P {
    P { min: q(a), max: q(b) }
}
fn spanp(a: P, b: P) -> P {
    P { min: a.min, max: b.max }
}
fn point(f: f32) -> P {
    P { min: q(f), max: q(f) }
}

/// temperature, humidity, continentalness, erosion, depth, weirdness, offset
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub p: [P; 6],
    pub offset: i64,
}

type B = &'static str;

const OCEANS: [[B; 5]; 2] = [
    ["deep_frozen_ocean", "deep_cold_ocean", "deep_ocean", "deep_lukewarm_ocean", "warm_ocean"],
    ["frozen_ocean", "cold_ocean", "ocean", "lukewarm_ocean", "warm_ocean"],
];
const MIDDLE: [[B; 5]; 5] = [
    ["snowy_plains", "snowy_plains", "snowy_plains", "snowy_taiga", "taiga"],
    ["plains", "plains", "forest", "taiga", "old_growth_spruce_taiga"],
    ["flower_forest", "plains", "forest", "birch_forest", "dark_forest"],
    ["savanna", "savanna", "forest", "jungle", "jungle"],
    ["desert", "desert", "desert", "desert", "desert"],
];
const MIDDLE_VARIANT: [[Option<B>; 5]; 5] = [
    [Some("ice_spikes"), None, Some("snowy_taiga"), None, None],
    [None, None, None, None, Some("old_growth_pine_taiga")],
    [Some("sunflower_plains"), None, None, Some("old_growth_birch_forest"), None],
    [None, None, Some("plains"), Some("sparse_jungle"), Some("bamboo_jungle")],
    [None, None, None, None, None],
];
const PLATEAU: [[B; 5]; 5] = [
    ["snowy_plains", "snowy_plains", "snowy_plains", "snowy_taiga", "snowy_taiga"],
    ["meadow", "meadow", "forest", "taiga", "old_growth_spruce_taiga"],
    ["meadow", "meadow", "meadow", "meadow", "pale_garden"],
    ["savanna_plateau", "savanna_plateau", "forest", "forest", "jungle"],
    ["badlands", "badlands", "badlands", "wooded_badlands", "wooded_badlands"],
];
const PLATEAU_VARIANT: [[Option<B>; 5]; 5] = [
    [Some("ice_spikes"), None, None, None, None],
    [Some("cherry_grove"), None, Some("meadow"), Some("meadow"), Some("old_growth_pine_taiga")],
    [Some("cherry_grove"), Some("cherry_grove"), Some("forest"), Some("birch_forest"), None],
    [None, None, None, None, None],
    [Some("eroded_badlands"), Some("eroded_badlands"), None, None, None],
];
const SHATTERED: [[Option<B>; 5]; 5] = [
    [Some("windswept_gravelly_hills"), Some("windswept_gravelly_hills"), Some("windswept_hills"), Some("windswept_forest"), Some("windswept_forest")],
    [Some("windswept_gravelly_hills"), Some("windswept_gravelly_hills"), Some("windswept_hills"), Some("windswept_forest"), Some("windswept_forest")],
    [Some("windswept_hills"), Some("windswept_hills"), Some("windswept_hills"), Some("windswept_forest"), Some("windswept_forest")],
    [None, None, None, None, None],
    [None, None, None, None, None],
];

struct Builder {
    out: Vec<(Point, B)>,
    full: P,
    temps: [P; 5],
    hums: [P; 5],
    eros: [P; 7],
    frozen: P,
    unfrozen: P,
    mushroom: P,
    deep_ocean: P,
    ocean: P,
    coast: P,
    inland: P,
    near: P,
    mid: P,
    far: P,
}

impl Builder {
    fn new() -> Self {
        let temps = [span(-1.0, -0.45), span(-0.45, -0.15), span(-0.15, 0.2), span(0.2, 0.55), span(0.55, 1.0)];
        Builder {
            out: Vec::new(),
            full: span(-1.0, 1.0),
            temps,
            hums: [span(-1.0, -0.35), span(-0.35, -0.1), span(-0.1, 0.1), span(0.1, 0.3), span(0.3, 1.0)],
            eros: [span(-1.0, -0.78), span(-0.78, -0.375), span(-0.375, -0.2225), span(-0.2225, 0.05), span(0.05, 0.45), span(0.45, 0.55), span(0.55, 1.0)],
            frozen: temps[0],
            unfrozen: spanp(temps[1], temps[4]),
            mushroom: span(-1.2, -1.05),
            deep_ocean: span(-1.05, -0.455),
            ocean: span(-0.455, -0.19),
            coast: span(-0.19, -0.11),
            inland: span(-0.11, 0.55),
            near: span(-0.11, 0.03),
            mid: span(0.03, 0.3),
            far: span(0.3, 1.0),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn surface(&mut self, t: P, h: P, c: P, e: P, w: P, off: f32, b: B) {
        self.out.push((Point { p: [t, h, c, e, point(0.0), w], offset: q(off) }, b));
        self.out.push((Point { p: [t, h, c, e, point(1.0), w], offset: q(off) }, b));
    }
    #[allow(clippy::too_many_arguments)]
    fn underground(&mut self, t: P, h: P, c: P, e: P, w: P, off: f32, b: B) {
        self.out.push((Point { p: [t, h, c, e, span(0.2, 0.9), w], offset: q(off) }, b));
    }
    #[allow(clippy::too_many_arguments)]
    fn bottom(&mut self, t: P, h: P, c: P, e: P, w: P, off: f32, b: B) {
        self.out.push((Point { p: [t, h, c, e, point(1.1), w], offset: q(off) }, b));
    }

    fn middle(i: usize, j: usize, w: P) -> B {
        if w.max < 0 { MIDDLE[i][j] } else { MIDDLE_VARIANT[i][j].unwrap_or(MIDDLE[i][j]) }
    }
    fn middle_or_badlands(i: usize, j: usize, w: P) -> B {
        if i == 4 { Self::badlands(j, w) } else { Self::middle(i, j, w) }
    }
    fn middle_or_badlands_or_slope(i: usize, j: usize, w: P) -> B {
        if i == 0 { Self::slope(i, j, w) } else { Self::middle_or_badlands(i, j, w) }
    }
    fn windswept_savanna(i: usize, j: usize, w: P, b: B) -> B {
        if i > 1 && j < 4 && w.max >= 0 { "windswept_savanna" } else { b }
    }
    fn shattered_coast(i: usize, j: usize, w: P) -> B {
        let b = if w.max >= 0 { Self::middle(i, j, w) } else { Self::beach(i, j) };
        Self::windswept_savanna(i, j, w, b)
    }
    fn beach(i: usize, _j: usize) -> B {
        match i {
            0 => "snowy_beach",
            4 => "desert",
            _ => "beach",
        }
    }
    fn badlands(j: usize, w: P) -> B {
        if j < 2 {
            if w.max < 0 { "badlands" } else { "eroded_badlands" }
        } else if j < 3 {
            "badlands"
        } else {
            "wooded_badlands"
        }
    }
    fn plateau(i: usize, j: usize, w: P) -> B {
        if w.max >= 0 {
            if let Some(v) = PLATEAU_VARIANT[i][j] {
                return v;
            }
        }
        PLATEAU[i][j]
    }
    fn peak(i: usize, j: usize, w: P) -> B {
        if i <= 2 {
            if w.max < 0 { "jagged_peaks" } else { "frozen_peaks" }
        } else if i == 3 {
            "stony_peaks"
        } else {
            Self::badlands(j, w)
        }
    }
    fn slope(i: usize, j: usize, w: P) -> B {
        if i >= 3 {
            Self::plateau(i, j, w)
        } else if j <= 1 {
            "snowy_slopes"
        } else {
            "grove"
        }
    }
    fn shattered(i: usize, j: usize, w: P) -> B {
        SHATTERED[i][j].unwrap_or_else(|| Self::middle(i, j, w))
    }

    fn off_coast(&mut self) {
        let f = self.full;
        self.surface(f, f, self.mushroom, f, f, 0.0, "mushroom_fields");
        for i in 0..5 {
            let t = self.temps[i];
            self.surface(t, f, self.deep_ocean, f, f, 0.0, OCEANS[0][i]);
            self.surface(t, f, self.ocean, f, f, 0.0, OCEANS[1][i]);
        }
    }

    fn inland_biomes(&mut self) {
        self.mid_slice(span(-1.0, -0.93333334));
        self.high_slice(span(-0.93333334, -0.7666667));
        self.peaks(span(-0.7666667, -0.56666666));
        self.high_slice(span(-0.56666666, -0.4));
        self.mid_slice(span(-0.4, -0.26666668));
        self.low_slice(span(-0.26666668, -0.05));
        self.valleys(span(-0.05, 0.05));
        self.low_slice(span(0.05, 0.26666668));
        self.mid_slice(span(0.26666668, 0.4));
        self.high_slice(span(0.4, 0.56666666));
        self.peaks(span(0.56666666, 0.7666667));
        self.high_slice(span(0.7666667, 0.93333334));
        self.mid_slice(span(0.93333334, 1.0));
    }

    fn peaks(&mut self, w: P) {
        let (e, coast, near, mid, far) = (self.eros, self.coast, self.near, self.mid, self.far);
        for i in 0..5 {
            let t = self.temps[i];
            for j in 0..5 {
                let h = self.hums[j];
                let k0 = Self::middle(i, j, w);
                let k1 = Self::middle_or_badlands(i, j, w);
                let k2 = Self::middle_or_badlands_or_slope(i, j, w);
                let k3 = Self::plateau(i, j, w);
                let k4 = Self::shattered(i, j, w);
                let k5 = Self::windswept_savanna(i, j, w, k4);
                let k6 = Self::peak(i, j, w);
                self.surface(t, h, spanp(coast, far), e[0], w, 0.0, k6);
                self.surface(t, h, spanp(coast, near), e[1], w, 0.0, k2);
                self.surface(t, h, spanp(mid, far), e[1], w, 0.0, k6);
                self.surface(t, h, spanp(coast, near), spanp(e[2], e[3]), w, 0.0, k0);
                self.surface(t, h, spanp(mid, far), e[2], w, 0.0, k3);
                self.surface(t, h, mid, e[3], w, 0.0, k1);
                self.surface(t, h, far, e[3], w, 0.0, k3);
                self.surface(t, h, spanp(coast, far), e[4], w, 0.0, k0);
                self.surface(t, h, spanp(coast, near), e[5], w, 0.0, k5);
                self.surface(t, h, spanp(mid, far), e[5], w, 0.0, k4);
                self.surface(t, h, spanp(coast, far), e[6], w, 0.0, k0);
            }
        }
    }

    fn high_slice(&mut self, w: P) {
        let (e, coast, near, mid, far) = (self.eros, self.coast, self.near, self.mid, self.far);
        for i in 0..5 {
            let t = self.temps[i];
            for j in 0..5 {
                let h = self.hums[j];
                let k0 = Self::middle(i, j, w);
                let k1 = Self::middle_or_badlands(i, j, w);
                let k2 = Self::middle_or_badlands_or_slope(i, j, w);
                let k3 = Self::plateau(i, j, w);
                let k4 = Self::shattered(i, j, w);
                let k5 = Self::windswept_savanna(i, j, w, k0);
                let k6 = Self::slope(i, j, w);
                let k7 = Self::peak(i, j, w);
                self.surface(t, h, coast, spanp(e[0], e[1]), w, 0.0, k0);
                self.surface(t, h, near, e[0], w, 0.0, k6);
                self.surface(t, h, spanp(mid, far), e[0], w, 0.0, k7);
                self.surface(t, h, near, e[1], w, 0.0, k2);
                self.surface(t, h, spanp(mid, far), e[1], w, 0.0, k6);
                self.surface(t, h, spanp(coast, near), spanp(e[2], e[3]), w, 0.0, k0);
                self.surface(t, h, spanp(mid, far), e[2], w, 0.0, k3);
                self.surface(t, h, mid, e[3], w, 0.0, k1);
                self.surface(t, h, far, e[3], w, 0.0, k3);
                self.surface(t, h, spanp(coast, far), e[4], w, 0.0, k0);
                self.surface(t, h, spanp(coast, near), e[5], w, 0.0, k5);
                self.surface(t, h, spanp(mid, far), e[5], w, 0.0, k4);
                self.surface(t, h, spanp(coast, far), e[6], w, 0.0, k0);
            }
        }
    }

    fn swamps(&mut self, w: P, c: P) {
        let (f, e, t) = (self.full, self.eros, self.temps);
        self.surface(spanp(t[1], t[2]), f, c, e[6], w, 0.0, "swamp");
        self.surface(spanp(t[3], t[4]), f, c, e[6], w, 0.0, "mangrove_swamp");
    }

    fn mid_slice(&mut self, w: P) {
        let (f, e, coast, near, mid, far) = (self.full, self.eros, self.coast, self.near, self.mid, self.far);
        self.surface(f, f, coast, spanp(e[0], e[2]), w, 0.0, "stony_shore");
        self.swamps(w, spanp(near, far));
        for i in 0..5 {
            let t = self.temps[i];
            for j in 0..5 {
                let h = self.hums[j];
                let k0 = Self::middle(i, j, w);
                let k1 = Self::middle_or_badlands(i, j, w);
                let k2 = Self::middle_or_badlands_or_slope(i, j, w);
                let k3 = Self::shattered(i, j, w);
                let k4 = Self::plateau(i, j, w);
                let k5 = Self::beach(i, j);
                let k6 = Self::windswept_savanna(i, j, w, k0);
                let k7 = Self::shattered_coast(i, j, w);
                let k8 = Self::slope(i, j, w);
                self.surface(t, h, spanp(near, far), e[0], w, 0.0, k8);
                self.surface(t, h, spanp(near, mid), e[1], w, 0.0, k2);
                self.surface(t, h, far, e[1], w, 0.0, if i == 0 { k8 } else { k4 });
                self.surface(t, h, near, e[2], w, 0.0, k0);
                self.surface(t, h, mid, e[2], w, 0.0, k1);
                self.surface(t, h, far, e[2], w, 0.0, k4);
                self.surface(t, h, spanp(coast, near), e[3], w, 0.0, k0);
                self.surface(t, h, spanp(mid, far), e[3], w, 0.0, k1);
                if w.max < 0 {
                    self.surface(t, h, coast, e[4], w, 0.0, k5);
                    self.surface(t, h, spanp(near, far), e[4], w, 0.0, k0);
                } else {
                    self.surface(t, h, spanp(coast, far), e[4], w, 0.0, k0);
                }
                self.surface(t, h, coast, e[5], w, 0.0, k7);
                self.surface(t, h, near, e[5], w, 0.0, k6);
                self.surface(t, h, spanp(mid, far), e[5], w, 0.0, k3);
                if w.max < 0 {
                    self.surface(t, h, coast, e[6], w, 0.0, k5);
                } else {
                    self.surface(t, h, coast, e[6], w, 0.0, k0);
                }
                if i == 0 {
                    self.surface(t, h, spanp(near, far), e[6], w, 0.0, k0);
                }
            }
        }
    }

    fn low_slice(&mut self, w: P) {
        let (f, e, coast, near, mid, far) = (self.full, self.eros, self.coast, self.near, self.mid, self.far);
        self.surface(f, f, coast, spanp(e[0], e[2]), w, 0.0, "stony_shore");
        self.swamps(w, spanp(near, far));
        for i in 0..5 {
            let t = self.temps[i];
            for j in 0..5 {
                let h = self.hums[j];
                let k0 = Self::middle(i, j, w);
                let k1 = Self::middle_or_badlands(i, j, w);
                let k2 = Self::middle_or_badlands_or_slope(i, j, w);
                let k3 = Self::beach(i, j);
                let k4 = Self::windswept_savanna(i, j, w, k0);
                let k5 = Self::shattered_coast(i, j, w);
                self.surface(t, h, near, spanp(e[0], e[1]), w, 0.0, k1);
                self.surface(t, h, spanp(mid, far), spanp(e[0], e[1]), w, 0.0, k2);
                self.surface(t, h, near, spanp(e[2], e[3]), w, 0.0, k0);
                self.surface(t, h, spanp(mid, far), spanp(e[2], e[3]), w, 0.0, k1);
                self.surface(t, h, coast, spanp(e[3], e[4]), w, 0.0, k3);
                self.surface(t, h, spanp(near, far), e[4], w, 0.0, k0);
                self.surface(t, h, coast, e[5], w, 0.0, k5);
                self.surface(t, h, near, e[5], w, 0.0, k4);
                self.surface(t, h, spanp(mid, far), e[5], w, 0.0, k0);
                self.surface(t, h, coast, e[6], w, 0.0, k3);
                if i == 0 {
                    self.surface(t, h, spanp(near, far), e[6], w, 0.0, k0);
                }
            }
        }
    }

    fn valleys(&mut self, w: P) {
        let (f, e, coast, near, mid, far) = (self.full, self.eros, self.coast, self.near, self.mid, self.far);
        let (fr, un) = (self.frozen, self.unfrozen);
        self.surface(fr, f, coast, spanp(e[0], e[1]), w, 0.0, if w.max < 0 { "stony_shore" } else { "frozen_river" });
        self.surface(un, f, coast, spanp(e[0], e[1]), w, 0.0, if w.max < 0 { "stony_shore" } else { "river" });
        self.surface(fr, f, near, spanp(e[0], e[1]), w, 0.0, "frozen_river");
        self.surface(un, f, near, spanp(e[0], e[1]), w, 0.0, "river");
        self.surface(fr, f, spanp(coast, far), spanp(e[2], e[5]), w, 0.0, "frozen_river");
        self.surface(un, f, spanp(coast, far), spanp(e[2], e[5]), w, 0.0, "river");
        self.surface(fr, f, coast, e[6], w, 0.0, "frozen_river");
        self.surface(un, f, coast, e[6], w, 0.0, "river");
        self.swamps(w, spanp(self.inland, far));
        self.surface(fr, f, spanp(self.inland, far), e[6], w, 0.0, "frozen_river");
        for i in 0..5 {
            let t = self.temps[i];
            for j in 0..5 {
                let h = self.hums[j];
                let k = Self::middle_or_badlands(i, j, w);
                self.surface(t, h, spanp(mid, far), spanp(e[0], e[1]), w, 0.0, k);
            }
        }
    }

    fn underground_biomes(&mut self) {
        let f = self.full;
        self.underground(f, f, span(0.8, 1.0), f, f, 0.0, "dripstone_caves");
        self.underground(f, span(0.7, 1.0), f, f, f, 0.0, "lush_caves");
        self.bottom(f, f, f, spanp(self.eros[0], self.eros[1]), f, 0.0, "deep_dark");
    }
}

/// The overworld parameter list, in vanilla order.
/// `MultiNoiseBiomeSourceParameterList.Preset.NETHER`
pub fn nether() -> Vec<(Point, &'static str)> {
    let pt = |t: f32, h: f32, off: f32| Point { p: [point(t), point(h), point(0.0), point(0.0), point(0.0), point(0.0)], offset: q(off) };
    vec![
        (pt(0.0, 0.0, 0.0), "nether_wastes"),
        (pt(0.0, -0.5, 0.0), "soul_sand_valley"),
        (pt(0.4, 0.0, 0.0), "crimson_forest"),
        (pt(0.0, 0.5, 0.375), "warped_forest"),
        (pt(-0.5, 0.0, 0.175), "basalt_deltas"),
    ]
}

/// `TheEndBiomeSource` biomes in `collectPossibleBiomes` order (picked by
/// `BiomeTree::end`, not by parameters).
pub fn end() -> Vec<(Point, &'static str)> {
    let pt = Point { p: [point(0.0); 6], offset: 0 };
    vec![(pt, "the_end"), (pt, "end_highlands"), (pt, "end_midlands"), (pt, "small_end_islands"), (pt, "end_barrens")]
}

pub fn overworld() -> Vec<(Point, &'static str)> {
    let mut b = Builder::new();
    b.off_coast();
    b.inland_biomes();
    b.underground_biomes();
    b.out
}
