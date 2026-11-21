//! Predictive chunk pre-generation.
//!
//! For every player we extrapolate the movement (position + velocity, with
//! extra weight on fast travel such as elytra / boats on ice), and queue the
//! chunks that will enter view distance soonest, *before* the player gets
//! there. Jobs run on a dedicated pool sized to leave a core free for the main
//! tick, are deduplicated, and are batched so the GPU can take them.

use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;
use std::thread;

use parking_lot::{Condvar, Mutex};

use super::chunk::Chunk;
use super::generator::Generator;

/// Predict which chunks a player will need next, best first.
///
/// * `pos` – block position (x, z)
/// * `vel` – blocks per tick (x, z)
/// * `view` – view distance in chunks
/// * `lookahead_ticks` – how far to extrapolate
/// * `candidates` – number of chunks to return
pub fn predict(pos: (f64, f64), vel: (f64, f64), view: i32, lookahead_ticks: f64, candidates: usize) -> Vec<(i32, i32)> {
    let speed = (vel.0 * vel.0 + vel.1 * vel.1).sqrt();
    let pcx = (pos.0 / 16.0).floor() as i32;
    let pcz = (pos.1 / 16.0).floor() as i32;
    let reach = view + 1;
    let mut scored: Vec<(f64, (i32, i32))> = Vec::new();

    if !speed.is_finite() || speed < 0.05 {
        // Standing still: the ring just outside view distance, nearest first.
        for dx in -reach..=reach {
            for dz in -reach..=reach {
                let d2 = (dx * dx + dz * dz) as f64;
                if d2 > (view * view) as f64 && d2 <= (reach * reach) as f64 {
                    scored.push((d2, (pcx + dx, pcz + dz)));
                }
            }
        }
    } else {
        let lookahead = if lookahead_ticks.is_finite() { lookahead_ticks.clamp(1.0, 2400.0) } else { 80.0 };
        // Sample the future path. A chunk is a candidate only if it will really
        // come within view distance of a predicted position; its score is the
        // first time that happens (plus a small distance tie-breaker).
        let steps = 32;
        let mut seen = HashSet::new();
        for s in 1..=steps {
            let t = lookahead * s as f64 / steps as f64;
            let fcx = ((pos.0 + vel.0 * t) / 16.0).floor() as i32;
            let fcz = ((pos.1 + vel.1 * t) / 16.0).floor() as i32;
            for dx in -view..=view {
                for dz in -view..=view {
                    let d2 = dx * dx + dz * dz;
                    if d2 > view * view {
                        continue;
                    }
                    let c = (fcx + dx, fcz + dz);
                    let (ox, oz) = (c.0 - pcx, c.1 - pcz);
                    if ox * ox + oz * oz <= view * view {
                        continue; // already in the client's current view
                    }
                    if seen.insert(c) {
                        scored.push((t + d2 as f64 * 0.001, c));
                    }
                }
            }
        }
    }
    scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(candidates).map(|(_, c)| c).collect()
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Job {
    // max-heap: newest prediction round first, then best-ranked candidate
    seq: u64,
    rank: std::cmp::Reverse<usize>,
    coord: (i32, i32),
}

struct Shared {
    queue: Mutex<(BinaryHeap<Job>, HashSet<(i32, i32)>, bool)>,
    cv: Condvar,
}

/// Background generator pool. `exists` tells whether a chunk is already
/// generated; `sink` receives finished chunks (e.g. to store them).
pub struct Pregen {
    shared: Arc<Shared>,
    max_queue: usize,
    workers: Vec<thread::JoinHandle<()>>,
    seq: Mutex<u64>,
}

impl Pregen {
    pub fn start(
        generator: Arc<Generator>,
        threads: usize,
        max_queue: usize,
        exists: Arc<dyn Fn(i32, i32) -> bool + Send + Sync>,
        sink: Arc<dyn Fn(Chunk) + Send + Sync>,
    ) -> Self {
        let shared = Arc::new(Shared { queue: Mutex::new((BinaryHeap::new(), HashSet::new(), false)), cv: Condvar::new() });
        let workers = (0..threads.max(1))
            .map(|i| {
                let (shared, generator, exists, sink) = (shared.clone(), generator.clone(), exists.clone(), sink.clone());
                thread::Builder::new()
                    .name(format!("rime-pregen-{i}"))
                    .spawn(move || loop {
                        let batch: Vec<(i32, i32)> = {
                            let mut q = shared.queue.lock();
                            while q.0.is_empty() && !q.2 {
                                shared.cv.wait(&mut q);
                            }
                            if q.2 {
                                return;
                            }
                            // take a batch so the GPU path gets useful work sizes
                            let mut b = Vec::new();
                            while b.len() < 16 {
                                match q.0.pop() {
                                    Some(j) => {
                                        q.1.remove(&j.coord);
                                        b.push(j.coord);
                                    }
                                    None => break,
                                }
                            }
                            b
                        };
                        let todo: Vec<(i32, i32)> = batch.into_iter().filter(|&(x, z)| !exists(x, z)).collect();
                        if todo.is_empty() {
                            continue;
                        }
                        // isolate panics so a bad chunk can never kill the worker pool
                        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| generator.generate_background(&todo)));
                        match res {
                            Ok(chunks) => chunks.into_iter().for_each(|c| sink(c)),
                            Err(_) => tracing::error!("pregen worker panicked on batch {todo:?}"),
                        }
                    })
                    .expect("spawn pregen thread")
            })
            .collect();
        Self { shared, max_queue, workers, seq: Mutex::new(0) }
    }

    /// Queue predicted chunks. Earlier entries in `coords` get higher priority;
    /// newer predictions beat older ones.
    pub fn enqueue(&self, coords: &[(i32, i32)]) {
        let seq = {
            let mut s = self.seq.lock();
            *s += 1;
            *s
        };
        let mut q = self.shared.queue.lock();
        for (i, c) in coords.iter().enumerate() {
            if q.0.len() >= self.max_queue {
                break;
            }
            if q.1.insert(*c) {
                q.0.push(Job { seq, rank: std::cmp::Reverse(i), coord: *c });
            }
        }
        drop(q);
        self.shared.cv.notify_all();
    }

    pub fn shutdown(mut self) {
        self.shared.queue.lock().2 = true;
        self.shared.cv.notify_all();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn predicts_ahead_of_movement() {
        // flying +x at elytra speed (~1.6 blocks/tick)
        let c = predict((0.0, 0.0), (1.6, 0.0), 8, 80.0, 30);
        assert_eq!(c.len(), 30);
        assert!(c.iter().all(|(x, _)| *x > 0), "{c:?}");
        // first candidates are the nearest leading-edge chunks
        assert!(c[0].0 <= 10);
    }

    #[test]
    fn stationary_ring() {
        let c = predict((8.0, 8.0), (0.0, 0.0), 4, 80.0, 1000);
        assert!(!c.is_empty());
        for (x, z) in &c {
            let d2 = x * x + z * z;
            assert!(d2 > 16 && d2 <= 25);
        }
    }

    #[test]
    fn nan_input_is_harmless() {
        let _ = predict((f64::NAN, 0.0), (f64::INFINITY, f64::NAN), 8, f64::NAN, 10);
    }

    #[test]
    fn pool_generates_and_dedupes() {
        let generator = Arc::new(Generator::new(1, false, 8));
        let count = Arc::new(AtomicUsize::new(0));
        let c2 = count.clone();
        let p = Pregen::start(generator, 2, 100, Arc::new(|_, _| false), Arc::new(move |_| {
            c2.fetch_add(1, Ordering::SeqCst);
        }));
        p.enqueue(&[(0, 0), (0, 1), (0, 0), (1, 1)]);
        for _ in 0..200 {
            if count.load(Ordering::SeqCst) >= 3 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(count.load(Ordering::SeqCst), 3);
        p.shutdown();
    }
}
