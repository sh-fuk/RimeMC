//! Whole-world conversion between vanilla Anvil (`.mca`) and AirWorld (`.air`).
//!
//! Only region-type directories (`region`, `entities`, `poi`) are converted;
//! every other file (level.dat, playerdata, data, datapacks …) is copied
//! unchanged, so the result is a complete, playable world. Conversion runs on
//! all CPU cores (one region per task) and the source world is never modified.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use crate::mca::{self, McaChunk};
use crate::region::{AirRegion, Error, Result};

const REGION_DIRS: [&str; 3] = ["region", "entities", "poi"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    McaToAir,
    AirToMca,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub regions: AtomicU64,
    pub chunks: AtomicU64,
    pub damaged_chunks: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
}

pub struct Options {
    pub level: i32,
    pub verify: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { level: 6, verify: true }
    }
}

/// Convert a whole world directory into `dst` (which must not exist or be empty).
pub fn convert_world(src: &Path, dst: &Path, dir: Direction, opts: &Options, log: &(dyn Fn(String) + Sync)) -> anyhow::Result<Stats> {
    anyhow::ensure!(src.is_dir(), "source world {} is not a directory", src.display());
    let src = src.canonicalize()?;
    if dst.exists() {
        anyhow::ensure!(fs::read_dir(dst)?.next().is_none(), "destination {} is not empty", dst.display());
    }
    fs::create_dir_all(dst)?;
    let dst = dst.canonicalize()?;
    anyhow::ensure!(!dst.starts_with(&src) && !src.starts_with(&dst), "source and destination must not be nested");

    // Collect work items while copying everything else.
    let mut jobs: Vec<(PathBuf, PathBuf, i32, i32)> = Vec::new();
    copy_tree(&src, &dst, dir, &mut jobs)?;

    let stats = Stats::default();
    let failures: Vec<String> = jobs
        .par_iter()
        .filter_map(|(from, to, x, z)| match convert_region(from, to, *x, *z, dir, opts, &stats) {
            Ok(dmg) => {
                for (i, e) in dmg {
                    log(format!("warning: {} chunk {i} skipped: {e}", from.display()));
                }
                None
            }
            Err(e) => Some(format!("{}: {e}", from.display())),
        })
        .collect();
    if !failures.is_empty() {
        for f in &failures {
            log(format!("error: {f}"));
        }
        anyhow::bail!("{} region file(s) failed to convert", failures.len());
    }
    Ok(stats)
}

fn copy_tree(src: &Path, dst: &Path, dir: Direction, jobs: &mut Vec<(PathBuf, PathBuf, i32, i32)>) -> anyhow::Result<()> {
    let (from_ext, to_ext) = match dir {
        Direction::McaToAir => ("mca", "air"),
        Direction::AirToMca => ("air", "mca"),
    };
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        let s = entry.path();
        let d = dst.join(&name);
        if ft.is_symlink() {
            // Never follow symlinks out of the world directory.
            continue;
        }
        if ft.is_dir() {
            fs::create_dir_all(&d)?;
            copy_tree(&s, &d, dir, jobs)?;
            continue;
        }
        let in_region_dir = src
            .file_name()
            .map(|n| REGION_DIRS.contains(&n.to_string_lossy().as_ref()))
            .unwrap_or(false);
        if in_region_dir {
            if let Some((x, z)) = mca::parse_region_name(&name_s, from_ext) {
                jobs.push((s, dst.join(format!("r.{x}.{z}.{to_ext}")), x, z));
                continue;
            }
            // external oversized chunk files are folded into the region on conversion
            if name_s.ends_with(".mcc") || name_s.ends_with(".tmp") {
                continue;
            }
        }
        if name_s == "session.lock" {
            continue;
        }
        fs::copy(&s, &d)?;
    }
    Ok(())
}

fn convert_region(from: &Path, to: &Path, x: i32, z: i32, dir: Direction, opts: &Options, stats: &Stats) -> Result<Vec<(usize, Error)>> {
    stats.bytes_in.fetch_add(fs::metadata(from).map(|m| m.len()).unwrap_or(0), Ordering::Relaxed);
    let damaged = match dir {
        Direction::McaToAir => {
            let (chunks, damaged) = mca::read_mca(from, x, z)?;
            let mut region = AirRegion::new(x, z, opts.level);
            for c in &chunks {
                region.put_raw(c.index, &c.raw, c.timestamp)?;
            }
            region.write_file(to)?;
            if opts.verify && !region.is_empty() {
                let (back, dmg) = AirRegion::read_file_expect(to, x, z)?;
                for c in &chunks {
                    if back.get_raw(c.index)?.as_deref() != Some(&c.raw[..]) || !dmg.is_empty() {
                        return Err(Error::Malformed("verification failed after writing .air"));
                    }
                }
            }
            stats.chunks.fetch_add(chunks.len() as u64, Ordering::Relaxed);
            damaged
        }
        Direction::AirToMca => {
            let (region, damaged) = AirRegion::read_file_expect(from, x, z)?;
            let mut chunks = Vec::with_capacity(region.chunk_count());
            for (i, slot) in region.iter() {
                chunks.push(McaChunk { index: i, timestamp: slot.timestamp, raw: slot.decompress(i)? });
            }
            mca::write_mca(to, x, z, &chunks)?;
            if opts.verify {
                let (back, dmg) = mca::read_mca(to, x, z)?;
                if !dmg.is_empty() || back.len() != chunks.len() || back.iter().zip(&chunks).any(|(a, b)| a.raw != b.raw || a.index != b.index) {
                    return Err(Error::Malformed("verification failed after writing .mca"));
                }
            }
            stats.chunks.fetch_add(chunks.len() as u64, Ordering::Relaxed);
            damaged
        }
    };
    stats.regions.fetch_add(1, Ordering::Relaxed);
    stats.damaged_chunks.fetch_add(damaged.len() as u64, Ordering::Relaxed);
    stats.bytes_out.fetch_add(fs::metadata(to).map(|m| m.len()).unwrap_or(0), Ordering::Relaxed);
    Ok(damaged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_roundtrip() {
        let base = std::env::temp_dir().join(format!("aw-conv-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let w = base.join("world");
        fs::create_dir_all(w.join("region")).unwrap();
        fs::create_dir_all(w.join("DIM-1/region")).unwrap();
        fs::write(w.join("level.dat"), b"LEVEL").unwrap();
        let chunks: Vec<McaChunk> =
            (0..50).map(|i| McaChunk { index: i * 7, timestamp: i as u32, raw: vec![i as u8; 3000 + i * 11] }).collect();
        mca::write_mca(&w.join("region/r.0.-1.mca"), 0, -1, &chunks).unwrap();
        mca::write_mca(&w.join("DIM-1/region/r.2.2.mca"), 2, 2, &chunks[..3]).unwrap();

        let air = base.join("air");
        let log = |_s: String| {};
        let st = convert_world(&w, &air, Direction::McaToAir, &Options::default(), &log).unwrap();
        assert_eq!(st.regions.load(Ordering::Relaxed), 2);
        assert!(air.join("region/r.0.-1.air").exists());
        assert_eq!(fs::read(air.join("level.dat")).unwrap(), b"LEVEL");

        let back = base.join("back");
        convert_world(&air, &back, Direction::AirToMca, &Options::default(), &log).unwrap();
        let (c2, _) = mca::read_mca(&back.join("region/r.0.-1.mca"), 0, -1).unwrap();
        assert_eq!(c2.len(), 50);
        for (a, b) in c2.iter().zip(&chunks) {
            assert_eq!(a.raw, b.raw);
            assert_eq!(a.timestamp, b.timestamp);
        }
        fs::remove_dir_all(&base).unwrap();
    }
}
