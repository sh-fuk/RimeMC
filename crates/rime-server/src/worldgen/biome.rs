//! Multi-noise biome lookup: port of vanilla's Climate.RTree (same tree
//! construction and search, so ties resolve the same way).

use std::cell::Cell;

use super::biome_builder::{Point, P};

#[derive(Clone, Copy)]
struct Span {
    min: i64,
    max: i64,
}

impl Span {
    #[inline]
    fn distance(&self, p: i64) -> i64 {
        let l = p - self.max;
        let m = self.min - p;
        if l > 0 { l } else { m.max(0) }
    }
    fn union(&self, o: Option<Span>) -> Span {
        match o {
            None => *self,
            Some(o) => Span { min: self.min.min(o.min), max: self.max.max(o.max) },
        }
    }
}

const DIMS: usize = 7;

struct Node {
    space: [Span; DIMS],
    /// leaf: value index; subtree: children
    leaf: Option<u16>,
    children: Vec<usize>,
}

pub struct BiomeTree {
    nodes: Vec<Node>,
    root: usize,
    pub values: Vec<&'static str>,
}

thread_local! {
    static LAST: Cell<usize> = const { Cell::new(usize::MAX) };
}

fn mid(s: &Span) -> i64 {
    (s.min + s.max) / 2
}

impl BiomeTree {
    pub fn new(list: &[(Point, &'static str)]) -> Self {
        let mut t = BiomeTree { nodes: Vec::new(), root: 0, values: Vec::new() };
        let leaves: Vec<usize> = list
            .iter()
            .enumerate()
            .map(|(i, (pt, b))| {
                let mut space = [Span { min: 0, max: 0 }; DIMS];
                for (k, p) in pt.p.iter().enumerate() {
                    let P { min, max } = *p;
                    space[k] = Span { min, max };
                }
                space[6] = Span { min: pt.offset, max: pt.offset };
                t.values.push(b);
                t.push(Node { space, leaf: Some(i as u16), children: Vec::new() })
            })
            .collect();
        t.root = t.build(leaves);
        t
    }

    fn push(&mut self, n: Node) -> usize {
        self.nodes.push(n);
        self.nodes.len() - 1
    }

    fn subtree(&mut self, children: Vec<usize>) -> usize {
        let mut ps: [Option<Span>; DIMS] = [None; DIMS];
        for &c in &children {
            for (k, p) in ps.iter_mut().enumerate() {
                *p = Some(self.nodes[c].space[k].union(*p));
            }
        }
        let space = ps.map(|p| p.unwrap());
        self.push(Node { space, leaf: None, children })
    }

    fn sort(&self, list: &mut [usize], dim: usize, absolute: bool) {
        list.sort_by(|&a, &b| {
            for k in 0..DIMS {
                let d = (dim + k) % DIMS;
                let key = |n: usize| {
                    let l = mid(&self.nodes[n].space[d]);
                    if absolute { l.abs() } else { l }
                };
                let o = key(a).cmp(&key(b));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            std::cmp::Ordering::Equal
        });
    }

    fn bucketize(&mut self, children: &[usize]) -> Vec<usize> {
        let n = 6f64.powf(((children.len() as f64 - 0.01).ln() / 6f64.ln()).floor()) as usize;
        let mut out = Vec::new();
        let mut cur = Vec::new();
        for &c in children {
            cur.push(c);
            if cur.len() >= n {
                out.push(self.subtree(std::mem::take(&mut cur)));
            }
        }
        if !cur.is_empty() {
            out.push(self.subtree(cur));
        }
        out
    }

    fn build(&mut self, mut children: Vec<usize>) -> usize {
        if children.len() == 1 {
            return children[0];
        }
        if children.len() <= 6 {
            children.sort_by_key(|&n| self.nodes[n].space.iter().map(|p| mid(p).abs()).sum::<i64>());
            return self.subtree(children);
        }
        let mut best = i64::MAX;
        let mut best_dim = 0;
        let mut best_buckets = Vec::new();
        for d in 0..DIMS {
            self.sort(&mut children, d, false);
            let buckets = self.bucketize(&children);
            let cost: i64 = buckets.iter().map(|&b| self.nodes[b].space.iter().map(|p| (p.max - p.min).abs()).sum::<i64>()).sum();
            if best > cost {
                best = cost;
                best_dim = d;
                best_buckets = buckets;
            }
        }
        self.sort(&mut best_buckets, best_dim, true);
        let subs: Vec<usize> = best_buckets
            .iter()
            .map(|&b| {
                let ch = self.nodes[b].children.clone();
                self.build(ch)
            })
            .collect();
        self.subtree(subs)
    }

    #[inline]
    fn distance(&self, n: usize, t: &[i64; DIMS]) -> i64 {
        let s = &self.nodes[n].space;
        let mut l = 0i64;
        for i in 0..DIMS {
            let d = s[i].distance(t[i]);
            l = l.wrapping_add(d.wrapping_mul(d));
        }
        l
    }

    fn search(&self, n: usize, t: &[i64; DIMS], candidate: usize) -> usize {
        let node = &self.nodes[n];
        if node.leaf.is_some() {
            return n;
        }
        let mut l = if candidate == usize::MAX { i64::MAX } else { self.distance(candidate, t) };
        let mut best = candidate;
        for &c in &node.children {
            let m = self.distance(c, t);
            if l > m {
                let leaf = self.search(c, t, best);
                let k = if c == leaf { m } else { self.distance(leaf, t) };
                if l > k {
                    l = k;
                    best = leaf;
                }
            }
        }
        best
    }

    /// Index into `values` for a quantized climate target
    /// (temperature, humidity, continentalness, erosion, depth, weirdness).
    pub fn find(&self, target: [i64; 6]) -> usize {
        let t = [target[0], target[1], target[2], target[3], target[4], target[5], 0];
        let last = LAST.with(|c| c.get());
        let last = if last < self.nodes.len() && self.nodes[last].leaf.is_some() { last } else { usize::MAX };
        let leaf = self.search(self.root, &t, last);
        LAST.with(|c| c.set(leaf));
        self.nodes[leaf].leaf.unwrap() as usize
    }
}

/// `Climate.quantizeCoord((float) value)`
#[inline]
pub fn quantize(v: f64) -> i64 {
    ((v as f32) * 10000.0f32) as i64
}

use super::chunk::{r, NoiseChunk};
use super::density::Router;

/// Biome indices (into `tree.values`) of one chunk, vanilla fill order;
/// `[section][(qy << 4) | (qz << 2) | qx]`.
pub fn chunk_biomes(router: &Router, nc: &NoiseChunk, tree: &BiomeTree, cx: i32, cz: i32) -> Vec<[u16; 64]> {
    let ids = &router.ids;
    let fns = [ids[r::TEMPERATURE], ids[r::VEGETATION], ids[r::CONTINENTS], ids[r::EROSION], ids[r::DEPTH], ids[r::RIDGES]];
    let sections = (super::chunk::HEIGHT / 16) as usize;
    let mut out = vec![[0u16; 64]; sections];
    for (s, sec) in out.iter_mut().enumerate() {
        let qy0 = (super::chunk::MIN_Y >> 2) + s as i32 * 4;
        for qx in 0..4 {
            for qy in 0..4 {
                for qz in 0..4 {
                    let (x, y, z) = ((cx * 4 + qx) << 2, (qy0 + qy) << 2, (cz * 4 + qz) << 2);
                    let t = fns.map(|f| quantize(nc.at_point(f, x, y, z)));
                    sec[((qy << 4) | (qz << 2) | qx) as usize] = tree.find(t) as u16;
                }
            }
        }
    }
    out
}
