//! Comparisons against values captured from vanilla 26.1 (see testdata/worldgen).
//! Needs the extracted vanilla data pack: set RIME_VANILLA_DATA to the
//! directory that contains `minecraft/worldgen` (tests are skipped otherwise).

use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use super::density::Router;
use super::random::Xoroshiro;

pub fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("RIME_VANILLA_DATA").ok()?);
    p.join("minecraft/worldgen").is_dir().then_some(p)
}

pub fn loader(dir: PathBuf) -> impl Fn(&str) -> Result<Value> {
    move |rel: &str| Ok(serde_json::from_str(&std::fs::read_to_string(dir.join("minecraft").join(format!("{rel}.json")))?)?)
}

fn rows(name: &str) -> Vec<Vec<String>> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/worldgen").join(name);
    std::fs::read_to_string(p).unwrap().lines().map(|l| l.split_whitespace().map(str::to_string).collect()).collect()
}

fn close(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() <= 1e-12 * a.abs().max(1.0)
}

#[test]
fn router_matches_vanilla() {
    let Some(dir) = data_dir() else { return };
    let load = loader(dir);
    let settings = load("worldgen/noise_settings/overworld").unwrap();
    let factory = Xoroshiro::new(12345).fork_positional();
    let r = Router::new(&load, &settings, factory).unwrap();
    let mut bad = 0;
    for row in rows("router_12345.txt") {
        let (x, y, z) = (row[0].parse().unwrap(), row[1].parse().unwrap(), row[2].parse().unwrap());
        for i in 0..15 {
            let want: f64 = row[3 + i].parse().unwrap();
            let got = r.eval(i, x, y, z);
            if !close(got, want) {
                bad += 1;
                eprintln!("{x} {y} {z} {}: got {got} want {want}", super::density::ROUTER[i]);
            }
        }
    }
    assert_eq!(bad, 0);
}

/// Debug aid: RIME_PROBE="x y z minecraft:key;..." prints density function values.
#[test]
fn probe() {
    let (Some(dir), Ok(p)) = (data_dir(), std::env::var("RIME_PROBE")) else { return };
    let load = loader(dir);
    let factory = Xoroshiro::new(12345).fork_positional();
    let mut b = super::density::Builder::new(&load, factory);
    for q in p.split(';') {
        let f: Vec<&str> = q.split_whitespace().collect();
        let id = b.reference(f[3]).unwrap();
        let v = b.g.eval(id, f[0].parse().unwrap(), f[1].parse().unwrap(), f[2].parse().unwrap(), &super::density::NoCache);
        eprintln!("{q} {v}");
    }
}

/// Reference chunks from a vanilla 26.1 server (seed 12345, features and
/// structures disabled): RIME_VREF = its `region` directory.
pub fn vref_chunk(cx: i32, cz: i32) -> Option<crate::world::chunk::Chunk> {
    let dir = PathBuf::from(std::env::var("RIME_VREF").ok()?);
    let (rx, rz) = (cx >> 5, cz >> 5);
    let (chunks, _) = airworld::mca::read_mca(&dir.join(format!("r.{rx}.{rz}.mca")), rx, rz).ok()?;
    let idx = ((cz & 31) * 32 + (cx & 31)) as usize;
    let c = chunks.into_iter().find(|c| c.index == idx)?;
    crate::world::chunk::Chunk::from_nbt(&c.raw)
}

pub fn overworld(seed: i64) -> Option<(Router, super::chunk::Settings, Xoroshiro)> {
    let load = loader(data_dir()?);
    let settings = load("worldgen/noise_settings/overworld").ok()?;
    let mut rng = Xoroshiro::new(seed);
    let factory = rng.fork_positional();
    let r = Router::new(&load, &settings, factory).unwrap();
    use crate::registry::blocks;
    let s = super::chunk::Settings { sea_level: 63, default_block: blocks::STONE, default_fluid: blocks::WATER, aquifers: true, ore_veins: true };
    Some((r, s, rng))
}

#[test]
fn terrain_matches_vanilla() {
    use crate::registry::blocks;
    let Some((router, settings, _)) = overworld(12345) else { return };
    if vref_chunk(0, 0).is_none() {
        return;
    }
    let factory = Xoroshiro::new(12345).fork_positional();
    let aq = factory.from_hash_of("minecraft:aquifer").fork_positional();
    let ore = factory.from_hash_of("minecraft:ore").fork_positional();
    let class = |b: crate::registry::BlockState| {
        if b.name() == "water" {
            1
        } else if b.name() == "lava" {
            2
        } else if b.is_air() {
            0
        } else {
            3
        }
    };
    let special = [blocks::GRANITE, blocks::TUFF, blocks::COPPER_ORE, blocks::DEEPSLATE_IRON_ORE, blocks::RAW_COPPER_BLOCK, blocks::RAW_IRON_BLOCK];
    let tree = super::biome::BiomeTree::new(&super::biome_builder::overworld());
    let names = crate::world::chunk::Biomes::get();
    let mut biome_bad = 0;
    let mut total_bad = 0;
    let mut tested = 0;
    for cx in -8..8 {
      for cz in -8..8 {
        if (cx * 7 + cz * 3) % 5 != 0 { continue; }
        let Some(v) = vref_chunk(cx, cz) else { continue };
        tested += 1;
        let ours = super::chunk::fill(&router, &settings, aq, ore, cx, cz);
        let nc = super::chunk::NoiseChunk::new(&router.graph, cx, cz);
        let bio = super::biome::chunk_biomes(&router, &nc, &tree, cx, cz);
        for (si, sec) in bio.iter().enumerate() {
            for (i, &b) in sec.iter().enumerate() {
                let want = &names.names[v.biomes[si][i] as usize];
                let got = format!("minecraft:{}", tree.values[b as usize]);
                if &got != want {
                    if biome_bad < 5 { eprintln!("biome {cx},{cz} section {si} idx {i}: ours {got} vanilla {want}"); }
                    biome_bad += 1;
                }
            }
        }
        let mut bad = 0;
        for y in -64..320 {
            for z in 0..16 {
                for x in 0..16 {
                    let a = ours[(((y + 64) * 16 + z) * 16 + x) as usize];
                    let b = v.get(x as usize, y, z as usize);
                    // flowing water/lava in the reference = fluid ticks that ran after generation
                    let flowing = class(b) != 3 && crate::registry::BlockRegistry::get().property(b, "level").is_some_and(|l| l != "0");
                    let ok = (class(a) == class(b) || (flowing && class(a) == 0)) && (!special.contains(&a) || a == b);
                    if !ok {
                        if bad < 5 {
                            eprintln!("chunk {cx},{cz} at {x},{y},{z}: ours {} vanilla {} {:?}", a.name(), b.name(), crate::registry::BlockRegistry::get().properties(b));
                        }
                        bad += 1;
                    }
                }
            }
        }
        if bad > 0 { eprintln!("chunk {cx},{cz}: {bad} mismatches"); }
        total_bad += bad;
      }
    }
    eprintln!("{tested} chunks compared");
    assert!(tested > 0);
    eprintln!("biome mismatches: {biome_bad}");
    assert_eq!(total_bad, 0);
    assert_eq!(biome_bad, 0);
}

/// RIME_BIOME_PARAMS: parameter list dumped from vanilla (Ref params).
#[test]
fn biome_parameters_match_vanilla() {
    let Ok(p) = std::env::var("RIME_BIOME_PARAMS") else { return };
    let want: Vec<String> = std::fs::read_to_string(p).unwrap().lines().map(str::to_string).collect();
    let got: Vec<String> = super::biome_builder::overworld()
        .iter()
        .map(|(pt, b)| {
            let mut s = String::new();
            for x in pt.p {
                s += &format!("{} {} ", x.min, x.max);
            }
            format!("{s}{} minecraft:{b}", pt.offset)
        })
        .collect();
    for (i, (a, b)) in got.iter().zip(&want).enumerate() {
        assert_eq!(a, b, "entry {i}");
    }
    assert_eq!(got.len(), want.len());
}

#[test]
fn full_chunks_match_vanilla() {
    let Some(dir) = data_dir() else { return };
    if vref_chunk(0, 0).is_none() {
        return;
    }
    let generator = super::VanillaGenerator::new(&loader(dir), 12345).unwrap();
    let mut total = 0;
    let mut tested = 0;
    let mut by_kind: std::collections::BTreeMap<String, usize> = Default::default();
    let mut coords: Vec<(i32, i32)> = (-8..8).flat_map(|x| (-8..8).map(move |z| (x, z))).filter(|(x, z)| (x * 5 + z * 11) % 4 == 0).collect();
    // RIME_CHUNKS="x,z;x,z": extra reference chunks (special biomes)
    if let Ok(list) = std::env::var("RIME_CHUNKS") {
        for p in list.split(';') {
            let mut it = p.split(',').map(|v| v.trim().parse::<i32>().unwrap());
            coords.push((it.next().unwrap(), it.next().unwrap()));
        }
    }
    {
        for (cx, cz) in coords {
            let Some(v) = vref_chunk(cx, cz) else { eprintln!("no reference for {cx},{cz}"); continue };
            tested += 1;
            let g = generator.generate(cx, cz);
            let mut bad = 0;
            let mut bb = 0;
            for (si, sec) in g.biomes.iter().enumerate() {
                for (i, &b) in sec.iter().enumerate() {
                    if b != v.biomes[si][i] {
                        bb += 1;
                    }
                }
            }
            if bb > 0 {
                eprintln!("chunk {cx},{cz}: {bb} biome mismatches");
                *by_kind.entry("biome".into()).or_default() += bb;
                bad += bb;
            }
            for y in -64..320 {
                for z in 0..16 {
                    for x in 0..16 {
                        let a = g.blocks[(((y + 64) * 16 + z) * 16 + x) as usize];
                        let b = v.get(x as usize, y, z as usize);
                        if a == b {
                            continue;
                        }
                        // fluid ticks that ran after generation
                        let reg = crate::registry::BlockRegistry::get();
                        if a.is_air() && reg.property(b, "level").is_some_and(|l| l != "0") {
                            continue;
                        }
                        *by_kind.entry(format!("{} -> {}", a.name(), b.name())).or_default() += 1;
                        if bad < 3 {
                            eprintln!("chunk {cx},{cz} at {x},{y},{z}: ours {} vanilla {}", a.name(), b.name());
                        }
                        bad += 1;
                    }
                }
            }
            total += bad;
        }
    }
    eprintln!("{tested} chunks, {total} mismatches: {by_kind:?}");
    assert!(tested > 0);
    assert_eq!(total, 0);
}

/// Debug aid: RIME_FIND=1 prints chunks whose surface biome is one of the
/// rarely-covered surface-rule cases.
#[test]
fn find_special_biomes() {
    let (Some(dir), Ok(_)) = (data_dir(), std::env::var("RIME_FIND")) else { return };
    let load = loader(dir);
    let settings = load("worldgen/noise_settings/overworld").unwrap();
    let r = Router::new(&load, &settings, Xoroshiro::new(12345).fork_positional()).unwrap();
    let tree = super::biome::BiomeTree::new(&super::biome_builder::overworld());
    use super::chunk::r as ri;
    let fns = [ri::TEMPERATURE, ri::VEGETATION, ri::CONTINENTS, ri::EROSION, ri::DEPTH, ri::RIDGES];
    let mut found: std::collections::BTreeMap<&str, (i32, i32)> = Default::default();
    for d in 0i32..200 {
        for cx in -d..=d {
            for cz in -d..=d {
                if cx.abs() != d && cz.abs() != d {
                    continue;
                }
                if (cx + cz) % 3 != 0 {
                    continue;
                }
                let t = fns.map(|f| super::biome::quantize(r.eval(f, cx * 16 + 8, 64, cz * 16 + 8)));
                let b = tree.values[tree.find(t)];
                if ["eroded_badlands", "badlands", "wooded_badlands", "frozen_ocean", "deep_frozen_ocean", "frozen_peaks", "snowy_slopes", "jagged_peaks", "ice_spikes", "mangrove_swamp", "swamp", "desert", "stony_peaks", "mushroom_fields", "grove", "snowy_beach", "windswept_gravelly_hills"].contains(&b) {
                    found.entry(b).or_insert((cx, cz));
                }
            }
        }
    }
    eprintln!("{found:?}");
}

/// Debug aid: RIME_COLUMN="cx,cz,x,z" prints a column (ours vs vanilla).
#[test]
fn column_dump() {
    let (Some(dir), Ok(c)) = (data_dir(), std::env::var("RIME_COLUMN")) else { return };
    let v: Vec<i32> = c.split(',').map(|s| s.parse().unwrap()).collect();
    let generator = super::VanillaGenerator::new(&loader(dir), 12345).unwrap();
    let g = generator.generate(v[0], v[1]);
    let r = vref_chunk(v[0], v[1]).unwrap();
    for y in (20..80).rev() {
        let a = g.blocks[(((y + 64) * 16 + v[3]) * 16 + v[2]) as usize];
        let b = r.get(v[2] as usize, y, v[3] as usize);
        eprintln!("{y}: {} {} {}", a.name(), b.name(), if a == b { "" } else { "<<<" });
    }
}

/// RIME_BENCH=1: chunks per second on all cores.
#[test]
fn bench_generation() {
    let (Some(dir), Ok(_)) = (data_dir(), std::env::var("RIME_BENCH")) else { return };
    use rayon::prelude::*;
    let generator = super::VanillaGenerator::new(&loader(dir), 42).unwrap();
    let coords: Vec<(i32, i32)> = (0..16).flat_map(|x| (0..16).map(move |z| (x + 100, z - 300))).collect();
    let t = std::time::Instant::now();
    let n: usize = coords.par_iter().map(|&(x, z)| generator.generate(x, z).blocks.len()).count();
    let s = t.elapsed().as_secs_f64();
    eprintln!("{n} chunks in {s:.2}s = {:.0} chunks/s on {} threads", n as f64 / s, rayon::current_num_threads());
}

/// World spawn for seed 12345, as chosen by a vanilla 26.1 server whose
/// biomes had no features (terrain + carvers only, like this generator so
/// far).
#[test]
fn spawn_matches_vanilla() {
    let Some(dir) = data_dir() else { return };
    let generator = super::VanillaGenerator::new(&loader(dir), 12345).unwrap();
    let spawn = generator.find_spawn(&|cx, cz| generator.generate(cx, cz));
    assert_eq!(spawn, (96, 136, -32));
}
