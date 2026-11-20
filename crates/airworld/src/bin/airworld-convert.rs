//! `airworld-convert` — convert worlds between vanilla Anvil and AirWorld.
//!
//! ```text
//! airworld-convert to-air  <vanilla_world> <output_world> [--level 6] [--no-verify]
//! airworld-convert to-mca  <air_world>     <output_world> [--no-verify]
//! airworld-convert inspect <file.air>
//! ```

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

use airworld::convert::{convert_world, Direction, Options};
use airworld::AirRegion;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "airworld-convert", version, about = "Convert Minecraft worlds between Anvil (.mca) and AirWorld (.air)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Number of worker threads (default: all cores)
    #[arg(long, global = true)]
    threads: Option<usize>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Vanilla (.mca) world -> AirWorld (.air)
    ToAir {
        src: PathBuf,
        dst: PathBuf,
        /// zstd level 1..=22 (higher = smaller, slower to write; reading speed barely changes)
        #[arg(long, default_value_t = 6)]
        level: i32,
        #[arg(long)]
        no_verify: bool,
    },
    /// AirWorld (.air) world -> vanilla (.mca)
    ToMca {
        src: PathBuf,
        dst: PathBuf,
        #[arg(long)]
        no_verify: bool,
    },
    /// Print information about one .air file
    Inspect { file: PathBuf },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(t) = cli.threads {
        rayon::ThreadPoolBuilder::new().num_threads(t.max(1)).build_global()?;
    }
    let log = |s: String| eprintln!("{s}");
    let (src, dst, dir, opts) = match cli.cmd {
        Cmd::ToAir { src, dst, level, no_verify } => {
            anyhow::ensure!((1..=22).contains(&level), "--level must be 1..=22");
            (src, dst, Direction::McaToAir, Options { level, verify: !no_verify })
        }
        Cmd::ToMca { src, dst, no_verify } => (src, dst, Direction::AirToMca, Options { level: 6, verify: !no_verify }),
        Cmd::Inspect { file } => {
            let (r, damaged) = AirRegion::read_file(&file)?;
            let raw: u64 = r.iter().map(|(_, s)| s.raw_len as u64).sum();
            let comp: u64 = r.iter().map(|(_, s)| s.compressed.len() as u64).sum();
            println!("region ({}, {}), {} chunks, zstd level {}", r.x, r.z, r.chunk_count(), r.level);
            println!("raw NBT {raw} bytes -> {comp} bytes ({:.1}x)", raw as f64 / comp.max(1) as f64);
            for (i, e) in damaged {
                println!("DAMAGED chunk {i}: {e}");
            }
            return Ok(());
        }
    };
    let t = Instant::now();
    let st = convert_world(&src, &dst, dir, &opts, &log)?;
    let (bi, bo) = (st.bytes_in.load(Ordering::Relaxed), st.bytes_out.load(Ordering::Relaxed));
    println!(
        "done in {:.2}s: {} regions, {} chunks, {} damaged chunks skipped, {:.1} MiB -> {:.1} MiB",
        t.elapsed().as_secs_f64(),
        st.regions.load(Ordering::Relaxed),
        st.chunks.load(Ordering::Relaxed),
        st.damaged_chunks.load(Ordering::Relaxed),
        bi as f64 / 1048576.0,
        bo as f64 / 1048576.0
    );
    Ok(())
}
