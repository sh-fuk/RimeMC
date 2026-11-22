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
    move |rel: &str| {
        if rel.starts_with("structure/") {
            return crate::nbt::gz_file_json(&dir.join("minecraft").join(format!("{rel}.nbt")));
        }
        Ok(serde_json::from_str(&std::fs::read_to_string(dir.join("minecraft").join(format!("{rel}.json")))?)?)
    }
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
    let spawn = generator.find_spawn(&|cx, cz| generator.generate_many(&[(cx, cz)]).pop().unwrap());
    assert_eq!(spawn, (96, 136, -32));
}

/// Featured chunks against a vanilla reference whose not-yet-ported
/// configured features are replaced with `no_op` (RIME_VREF_F = its `region`
/// directory). Prints mismatches by kind.
#[test]
fn features_match_vanilla() {
    let Some(dir) = data_dir() else { return };
    let Ok(vdir) = std::env::var("RIME_VREF_F") else { return };
    let generator = super::VanillaGenerator::new(&loader(dir), 12345).unwrap();
    let r = std::env::var("RIME_RADIUS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    // RIME_CENTER=cx,cz moves the compared area (chunk coordinates)
    let (ox, oz) = std::env::var("RIME_CENTER").ok().and_then(|v| v.split_once(',').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))).unwrap_or((0, 0));
    let coords: Vec<(i32, i32)> = (-r..r).flat_map(|x| (-r..r).map(move |z| (ox + x, oz + z))).collect();
    let t = std::time::Instant::now();
    let mut gens = generator.generate_many(&coords);
    eprintln!("{} chunks in {:.2}s", coords.len(), t.elapsed().as_secs_f64());
    let read = |cx: i32, cz: i32| {
        let (rx, rz) = (cx >> 5, cz >> 5);
        let (chunks, _) = airworld::mca::read_mca(&PathBuf::from(&vdir).join(format!("r.{rx}.{rz}.mca")), rx, rz).ok()?;
        let idx = ((cz & 31) * 32 + (cx & 31)) as usize;
        let c = chunks.into_iter().find(|c| c.index == idx)?;
        crate::world::chunk::Chunk::from_nbt(&c.raw)
    };
    // RIME_VREF_F2: compare two vanilla runs instead (how much vanilla itself varies)
    if let Ok(d2) = std::env::var("RIME_VREF_F2") {
        for (&(cx, cz), g) in coords.iter().zip(gens.iter_mut()) {
            let (rx, rz) = (cx >> 5, cz >> 5);
            let (chunks, _) = airworld::mca::read_mca(&PathBuf::from(&d2).join(format!("r.{rx}.{rz}.mca")), rx, rz).unwrap();
            let idx = ((cz & 31) * 32 + (cx & 31)) as usize;
            let c = crate::world::chunk::Chunk::from_nbt(&chunks.into_iter().find(|c| c.index == idx).unwrap().raw).unwrap();
            for y in -64..320 {
                for z in 0..16 {
                    for x in 0..16 {
                        g.blocks[(((y + 64) * 16 + z) * 16 + x) as usize] = c.get(x as usize, y, z as usize);
                    }
                }
            }
        }
    }
    // RIME_DUMP=x,y,z: print both worlds around a block (L log, l leaves + distance, . air)
    if let Some(d) = std::env::var("RIME_DUMP").ok() {
        let v: Vec<i32> = d.split(',').map(|x| x.parse().unwrap()).collect();
        let reg = crate::registry::BlockRegistry::get();
        let ch = |s: crate::registry::BlockState| -> String {
            let n = s.name();
            if s.is_air() {
                ".".into()
            } else if n.ends_with("_log") || n.ends_with("_wood") {
                "L".into()
            } else if n.ends_with("leaves") {
                reg.property(s, "distance").unwrap_or("?").into()
            } else {
                n[..1].to_uppercase()
            }
        };
        for y in v[1] - 3..=v[1] + 3 {
            eprintln!("y={y}  ours | vanilla");
            for z in v[2] - 5..=v[2] + 5 {
                let mut line = String::new();
                for side in 0..2 {
                    for x in v[0] - 5..=v[0] + 5 {
                        let (cx, cz) = (x >> 4, z >> 4);
                        let s = if side == 0 {
                            let i = coords.iter().position(|c| *c == (cx, cz)).unwrap();
                            gens[i].blocks[(((y + 64) * 16 + (z & 15)) * 16 + (x & 15)) as usize]
                        } else {
                            read(cx, cz).unwrap().get((x & 15) as usize, y, (z & 15) as usize)
                        };
                        line += &ch(s);
                    }
                    line += "   ";
                }
                eprintln!("{line}");
            }
        }
    }
    let mut total = 0;
    let mut by_kind: std::collections::BTreeMap<String, usize> = Default::default();
    for (&(cx, cz), g) in coords.iter().zip(&gens) {
        let Some(v) = read(cx, cz) else { eprintln!("no reference for {cx},{cz}"); continue };
        let mut shown = 0;
        for y in -64..320 {
            for z in 0..16 {
                for x in 0..16 {
                    let a = g.blocks[(((y + 64) * 16 + z) * 16 + x) as usize];
                    let b = v.get(x as usize, y, z as usize);
                    if a == b {
                        continue;
                    }
                    let reg = crate::registry::BlockRegistry::get();
                    if a.is_air() && reg.property(b, "level").is_some_and(|l| l != "0") {
                        continue;
                    }
                    *by_kind.entry(format!("{} -> {}", a.name(), b.name())).or_default() += 1;
                    if std::env::var("RIME_SHOW").is_ok_and(|v| shown < v.parse().unwrap_or(2)) {
                        let full = |s: crate::registry::BlockState| if std::env::var("RIME_PROPS").is_ok() { format!("{}{:?}", s.name(), reg.properties(s)) } else { s.name().to_string() };
                        eprintln!("{cx},{cz} at {},{y},{}: ours {} vanilla {}", cx * 16 + x, cz * 16 + z, full(a), full(b));
                        shown += 1;
                    }
                    total += 1;
                }
            }
        }
    }
    eprintln!("{total} mismatches: {by_kind:?}");
    assert_eq!(total, 0);
}

#[test]
fn vstate_water() {
    use crate::registry::{blocks, vstate};
    let w = blocks::WATER;
    eprintln!("water {} {:?} fluid={} air={}", w.0, w.name(), vstate::fluid(w), vstate::air(blocks::AIR));
    assert_eq!(vstate::fluid(w), 1);
}

#[test]
fn vstate_lava() {
    use crate::registry::{vstate, BlockState};
    for i in 0..crate::registry::BlockRegistry::get().state_count() {
        let s = BlockState(i as u16);
        let f = vstate::fluid(s);
        assert_eq!(matches!(f, 3 | 4), s.name() == "lava", "{} {f}", s.name());
    }
}

/// RIME_FIND=cx,cz,step,name,x,y,z,block: which feature index of `step`
/// writes `block` at (x,y,z) when decorating chunk cx,cz (debugging seeds).
#[test]
fn find_feature_index() {
    let Some(dir) = data_dir() else { return };
    let Ok(spec) = std::env::var("RIME_FIND") else { return };
    let v: Vec<&str> = spec.split(',').collect();
    let (cx, cz, step): (i32, i32, usize) = (v[0].parse().unwrap(), v[1].parse().unwrap(), v[2].parse().unwrap());
    let name = v[3];
    let p = [v[4].parse::<i32>().unwrap(), v[5].parse::<i32>().unwrap(), v[6].parse::<i32>().unwrap()];
    let want = v[7];
    let g = super::VanillaGenerator::new(&loader(dir), 12345).unwrap();
    let lib = g.features.as_ref().unwrap();
    let fi = lib.steps[step].iter().position(|f| f.name.as_deref() == Some(name)).unwrap();
    eprintln!("{name} has index {fi} of {}", lib.steps[step].len());
    let outside = |qx: i32, qy: i32, qz: i32| g.noise_biome_uncached(qx, qy, qz);
    for k in 0..lib.steps[step].len() + 5 {
        let chunks = (0..9).map(|i| { let gg = g.generate(cx + i / 3 - 1, cz + i % 3 - 1); Box::new(super::feature::Proto::new(gg.blocks, gg.biomes)) }).collect();
        let mut lv = super::feature::Region { cx, cz, chunks, lib, zoom_seed: g.surface.zoom_seed, seed: g.seed, outside: &outside };
        let mut r = crate::worldgen::random::WgRandom::new(0);
        let dec = r.set_decoration_seed(g.seed, cx * 16, cz * 16);
        r.set_feature_seed(dec, k as i32, step as i32);
        lib.steps[step][fi].place_top(&mut lv, &mut r, [cx * 16, crate::worldgen::chunk::MIN_Y, cz * 16]);
        if lv.get(p).name() == want {
            eprintln!("index {k} matches");
        }
    }
}

/// All eight reference areas (12x12 chunks each) against two full vanilla
/// runs (RIME_VREF_A / RIME_VREF_B = their `region` directories): blocks and
/// light, skipping positions where the two vanilla runs disagree.
/// RIME_AREAS=i,j,.. limits the areas; RIME_SHOW=n prints examples.
#[test]
fn areas_match_vanilla() {
    use crate::nbt::Tag;
    use crate::world::chunk::Chunk;
    let Some(dir) = data_dir() else { return };
    let Ok(va) = std::env::var("RIME_VREF_A") else { return };
    let vb = std::env::var("RIME_VREF_B").ok();
    let generator = super::VanillaGenerator::new(&loader(dir), 12345).unwrap();
    const AREAS: [(i32, i32); 8] = [(0, 0), (-100, 0), (0, 300), (300, 0), (0, -100), (150, 150), (-200, -200), (100, -300)];
    let pick: Option<Vec<usize>> = std::env::var("RIME_AREAS").ok().map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect());
    let show: usize = std::env::var("RIME_SHOW").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    // (blocks, light layers per light section index 0..26: (sky, block))
    type Ref = (Chunk, Vec<(Option<Vec<u8>>, Option<Vec<u8>>)>);
    let read = |d: &str, cx: i32, cz: i32| -> Option<Ref> {
        let (rx, rz) = (cx >> 5, cz >> 5);
        let (chunks, _) = airworld::mca::read_mca(&PathBuf::from(d).join(format!("r.{rx}.{rz}.mca")), rx, rz).ok()?;
        let idx = ((cz & 31) * 32 + (cx & 31)) as usize;
        let c = chunks.into_iter().find(|c| c.index == idx)?;
        let chunk = Chunk::from_nbt(&c.raw)?;
        let (_, root) = Tag::from_root_bytes(&c.raw).ok()?;
        // only finished chunks are comparable (forceload leaves partial neighbours)
        if !matches!(root.get("Status"), Some(Tag::String(s)) if s == "minecraft:full") {
            return None;
        }
        let mut light = vec![(None, None); 26];
        if let Some(Tag::List(secs)) = root.get("sections") {
            for s in secs {
                let y = s.get("Y").and_then(|t| t.as_i64()).unwrap_or(-100);
                let i = y + 5;
                if !(0..26).contains(&i) {
                    continue;
                }
                let arr = |k: &str| match s.get(k) {
                    Some(Tag::ByteArray(a)) if a.len() == 2048 => Some(a.iter().map(|b| *b as u8).collect::<Vec<u8>>()),
                    _ => None,
                };
                light[i as usize] = (arr("SkyLight"), arr("BlockLight"));
            }
        }
        Some((chunk, light))
    };
    let nib = |a: &[u8], k: usize| (a[k >> 1] >> ((k & 1) * 4)) & 15;
    let reg = crate::registry::BlockRegistry::get();
    let mut grand = 0usize;
    let only_chunk: Option<(i32, i32)> = std::env::var("RIME_CHUNK").ok().and_then(|v| v.split_once(',').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))));
    // RIME_AREA_LIST="cx,cz;..": other area centres (chunk coordinates)
    let list: Vec<(i32, i32)> = match std::env::var("RIME_AREA_LIST") {
        Ok(v) => v.split(';').filter_map(|c| c.split_once(',').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))).collect(),
        Err(_) => AREAS.to_vec(),
    };
    for (ai, &(ox, oz)) in list.iter().enumerate() {
        if pick.as_ref().is_some_and(|p| !p.contains(&ai)) {
            continue;
        }
        let coords: Vec<(i32, i32)> = (-6..6).flat_map(|x| (-6..6).map(move |z| (ox + x, oz + z))).collect();
        let show_here = |cx: i32, cz: i32| only_chunk.is_none_or(|c| c == (cx, cz));
        let t = std::time::Instant::now();
        let gens = generator.generate_many(&coords);
        let secs = t.elapsed().as_secs_f64();
        let ours: std::collections::HashMap<(i32, i32), Chunk> = coords
            .iter()
            .zip(&gens)
            .map(|(&(cx, cz), g)| {
                let mut c = Chunk::empty(cx, cz);
                for y in -64..320 {
                    for z in 0..16 {
                        for x in 0..16 {
                            c.set(x, y, z, g.blocks[(((y + 64) * 16 + z as i32) * 16 + x as i32) as usize]);
                        }
                    }
                }
                c.recalc_heightmap();
                ((cx, cz), c)
            })
            .collect();
        // RIME_FLOW=ticks: let the generated fluids flow like a loaded vanilla chunk
        let flow: Option<u64> = std::env::var("RIME_FLOW").ok().and_then(|v| v.parse().ok());
        let mut ours = ours;
        if let Some(ticks) = flow {
            use crate::game::fluid::{fluid_state, lava_interaction, tick_delay, Flow, FluidWorld};
            struct FW<'a> {
                chunks: &'a mut std::collections::HashMap<(i32, i32), Chunk>,
                queue: std::collections::BTreeMap<u64, Vec<[i32; 3]>>,
                now: u64,
                cur: [i32; 3],
                root: std::collections::HashMap<[i32; 3], [i32; 3]>,
                parent: std::collections::HashMap<[i32; 3], ([i32; 3], u64)>,
                ticking: [i32; 3],
            }
            impl FluidWorld for FW<'_> {
                fn block(&self, p: [i32; 3]) -> Option<crate::registry::BlockState> {
                    if !(-64..320).contains(&p[1]) {
                        return None;
                    }
                    self.chunks.get(&(p[0] >> 4, p[2] >> 4)).map(|c| c.get((p[0] & 15) as usize, p[1], (p[2] & 15) as usize))
                }
                fn set(&mut self, p: [i32; 3], b: crate::registry::BlockState, _drops: bool) {
                    if let Some(c) = self.chunks.get_mut(&(p[0] >> 4, p[2] >> 4)) {
                        c.set((p[0] & 15) as usize, p[1], (p[2] & 15) as usize, b);
                    }
                    let cur = self.cur;
                    self.root.insert(p, cur);
                    let (t, now) = (self.ticking, self.now);
                    self.parent.entry(p).or_insert((t, now));
                    for d in [[0, 0, 0], [1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]] {
                        let q = [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
                        if let Some(f) = self.block(q).and_then(|b| fluid_state(b).fluid()) {
                            if lava_interaction(self, q) {
                                self.schedule(q, tick_delay(f));
                            }
                        }
                    }
                }
                fn schedule(&mut self, p: [i32; 3], delay: u32) {
                    let v = self.queue.entry(self.now + delay.max(1) as u64).or_default();
                    if !v.contains(&p) {
                        v.push(p);
                    }
                }
            }
            let mut w = FW { chunks: &mut ours, queue: Default::default(), now: 0, cur: [0; 3], root: Default::default(), parent: Default::default(), ticking: [0; 3] };
            let mut n = 0;
            for g in &gens {
                for &p in &g.fluid_ticks {
                    if let Some(f) = w.block(p).and_then(|b| fluid_state(b).fluid()) {
                        w.schedule(p, tick_delay(f));
                        n += 1;
                    }
                }
            }
            eprintln!("{n} fluid ticks scheduled");
            if let Ok(near) = std::env::var("RIME_NEAR") {
                let v: Vec<i32> = near.split(',').map(|x| x.parse().unwrap()).collect();
                for g in &gens {
                    for p in &g.fluid_ticks {
                        if (p[0] - v[0]).abs() <= v[3] && (p[1] - v[1]).abs() <= v[3] && (p[2] - v[2]).abs() <= v[3] {
                            eprintln!("  tick at {p:?}: {:?}", w.block(*p).map(|b| (b.name(), reg.properties(b))));
                        }
                    }
                }
            }
            for _ in 0..ticks {
                w.now += 1;
                if let Some(list) = w.queue.remove(&w.now) {
                    for p in list {
                        if let Some(f) = w.block(p).and_then(|b| fluid_state(b).fluid()) {
                            w.cur = *w.root.get(&p).unwrap_or(&p);
                            w.ticking = p;
                            Flow { w: &mut w, fluid: f }.tick(p);
                        }
                    }
                }
            }
            if let Ok(near) = std::env::var("RIME_ROOT") {
                let v: Vec<i32> = near.split(',').map(|x| x.parse().unwrap()).collect();
                eprintln!("root of {v:?}: {:?}", w.root.get(&[v[0], v[1], v[2]]));
                let mut p = [v[0], v[1], v[2]];
                for _ in 0..80 {
                    let Some(&(q, t)) = w.parent.get(&p) else { break };
                    let vb = read(&va, p[0] >> 4, p[2] >> 4).map(|r| r.0.get((p[0] & 15) as usize, p[1], (p[2] & 15) as usize));
                    let ob = w.block(p);
                    eprintln!("  {p:?} t={t} ours {:?} vanilla {:?}", ob.map(|b| (b.name(), reg.property(b, "level"))), vb.map(|b| (b.name(), reg.property(b, "level"))));
                    if q == p {
                        break;
                    }
                    p = q;
                }
            }
        }
        if let Ok(at) = std::env::var("RIME_AT") {
            let v: Vec<i32> = at.split(',').map(|x| x.parse().unwrap()).collect();
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let mut line = String::new();
                    for dx in -1..=1 {
                        let (x, y, z) = (v[0] + dx, v[1] + dy, v[2] + dz);
                        let o = ours.get(&(x >> 4, z >> 4)).map(|c| c.get((x & 15) as usize, y, (z & 15) as usize));
                        let a = read(&va, x >> 4, z >> 4).map(|r| r.0.get((x & 15) as usize, y, (z & 15) as usize));
                        let f = |s: Option<crate::registry::BlockState>| s.map(|s| format!("{}{}", s.name(), reg.property(s, "level").map(|l| format!("@{l}")).unwrap_or_default())).unwrap_or("-".into());
                        line += &format!("{:>14}|{:<14} ", f(o), f(a));
                    }
                    eprintln!("dy={dy} dz={dz}: {line}");
                }
            }
        }
        let mut by_kind: std::collections::BTreeMap<String, usize> = Default::default();
        let (mut blocks, mut sky, mut blk, mut noisy) = (0usize, 0usize, 0usize, 0usize);
        let mut shown = 0;
        let per_chunk = std::env::var("RIME_PER_CHUNK").is_ok();
        for &(cx, cz) in &coords {
            let (b0, n0) = (blocks, noisy);
            let Some((a, la)) = read(&va, cx, cz) else { eprintln!("no reference for {cx},{cz}"); continue };
            let b = vb.as_deref().and_then(|d| read(d, cx, cz));
            let o = &ours[&(cx, cz)];
            for y in -64..320 {
                for z in 0..16 {
                    for x in 0..16 {
                        let (s, v) = (o.get(x, y, z), a.get(x, y, z));
                        // RIME_GREP=block: every position of that block in ours or vanilla
                        if let Ok(g) = std::env::var("RIME_GREP") {
                            if show_here(cx, cz) && (s.name().contains(g.as_str()) || v.name().contains(g.as_str())) {
                                eprintln!("  grep {},{y},{}: ours {} vanilla {}", cx * 16 + x as i32, cz * 16 + z as i32, s.name(), v.name());
                            }
                        }
                        if b.as_ref().is_some_and(|b| b.0.get(x, y, z) != v) {
                            noisy += 1;
                            continue;
                        }
                        if s == v || (flow.is_none() && s.is_air() && reg.property(v, "level").is_some_and(|l| l != "0")) {
                            continue;
                        }
                        blocks += 1;
                        *by_kind.entry(format!("{} -> {}", s.name(), v.name())).or_default() += 1;
                        let only = std::env::var("RIME_ONLY").ok();
                        if shown < show && show_here(cx, cz) && only.as_deref().is_none_or(|o| s.name() == o || v.name() == o) {
                            eprintln!("  {},{y},{}: ours {} {:?} vanilla {} {:?}", cx * 16 + x as i32, cz * 16 + z as i32, s.name(), reg.properties(s), v.name(), reg.properties(v));
                            shown += 1;
                        }
                    }
                }
            }
            if per_chunk && blocks + noisy > b0 + n0 {
                eprintln!("  chunk {cx},{cz}: ours {} vanilla-noise {}", blocks - b0, noisy - n0);
            }
            // light: inner chunks only (the outer ring's neighbours are not final)
            if (cx - ox).abs() > 4 || cx - ox == -6 || (cz - oz).abs() > 4 || cz - oz == -6 {
                continue;
            }
            let vanilla_blocks: Vec<Option<Chunk>> = if std::env::var("RIME_LIGHT_ON_VANILLA").is_ok() { (0..9).map(|i| read(&va, cx + i / 3 - 1, cz + i % 3 - 1).map(|r| r.0)).collect() } else { Vec::new() };
            let refs: [Option<&Chunk>; 9] = if vanilla_blocks.is_empty() { std::array::from_fn(|i| ours.get(&(cx + i as i32 / 3 - 1, cz + i as i32 % 3 - 1))) } else { std::array::from_fn(|i| vanilla_blocks[i].as_ref()) };
            let o = refs[4].unwrap();
            let l = crate::world::light::compute(&refs);
            for ls in 1..25 {
                for (kind, ours_l, va_l, vb_l) in [(0, &l.sky[ls], &la[ls].0, b.as_ref().map(|b| &b.1[ls].0)), (1, &l.block[ls], &la[ls].1, b.as_ref().map(|b| &b.1[ls].1))] {
                    let Some(va_l) = va_l else { continue };
                    for k in 0..4096 {
                        let want = nib(va_l, k);
                        if vb_l.is_some_and(|vb| vb.as_ref().map(|v| nib(v, k)) != Some(want)) {
                            continue;
                        }
                        let got = ours_l.as_ref().map(|a| nib(&a[..], k)).unwrap_or(0);
                        if got != want {
                            if kind == 0 {
                                sky += 1;
                            } else {
                                blk += 1;
                            }
                            if shown < show && show_here(cx, cz) && std::env::var("RIME_SHOW_LIGHT").is_ok() {
                                let y = (ls as i32 - 5) * 16 + (k >> 8) as i32;
                                eprintln!("  {} light {},{y},{}: ours {got} vanilla {want} ({})", if kind == 0 { "sky" } else { "block" }, cx * 16 + (k & 15) as i32, cz * 16 + ((k >> 4) & 15) as i32, o.get(k & 15, y, (k >> 4) & 15).name());
                                let (wx, wz) = (cx * 16 + (k & 15) as i32, cz * 16 + ((k >> 4) & 15) as i32);
                                let mut ns = String::new();
                                for dx in -3..=3 {
                                    for dy in -3..=3 {
                                        for dz in -3..=3 {
                                            let (nx, ny, nz) = (wx + dx, y + dy, wz + dz);
                                            if let Some(c) = ours.get(&(nx >> 4, nz >> 4)) {
                                                let s = c.get((nx & 15) as usize, ny, (nz & 15) as usize);
                                                let e = crate::world::light::emission(s);
                                                if e > 0 {
                                                    let vs = read(&va, nx >> 4, nz >> 4).map(|r| r.0.get((nx & 15) as usize, ny, (nz & 15) as usize).name()).unwrap_or("?");
                                                    ns += &format!("[{dx},{dy},{dz}] {}({e}) vanilla {vs}; ", s.name());
                                                }
                                            }
                                        }
                                    }
                                }
                                eprintln!("     around: {ns}");
                                shown += 1;
                            }
                        }
                    }
                }
            }
        }
        let mut kinds: Vec<_> = by_kind.into_iter().collect();
        kinds.sort_by(|a, b| b.1.cmp(&a.1));
        kinds.truncate(std::env::var("RIME_KINDS").ok().and_then(|v| v.parse().ok()).unwrap_or(12));
        eprintln!("area {ai} ({ox},{oz}) {secs:.1}s: blocks {blocks}, sky {sky}, block light {blk}, vanilla-noise {noisy}; {kinds:?}");
        grand += blocks + sky + blk;
    }
    eprintln!("total {grand}");
}

/// Debug aid: RIME_STARTS=r lists structure starts within r chunks of 0,0
/// (seed 12345, or RIME_SEED).
#[test]
fn list_structure_starts() {
    let (Some(dir), Ok(r)) = (data_dir(), std::env::var("RIME_STARTS")) else { return };
    let r: i32 = r.parse().unwrap();
    let seed = std::env::var("RIME_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(12345);
    let g = super::VanillaGenerator::new(&loader(dir), seed).unwrap();
    for (cx, cz, name, n, bb) in g.structure_starts(-r, -r, r, r) {
        eprintln!("{name} at chunk {cx},{cz}: {n} pieces, box {bb:?}");
    }
}

/// Vanilla structure starts saved in a region directory (RIME_VREF_S):
/// chunk -> (structure id, piece boxes, piece element locations).
pub fn vanilla_starts(dir: &str, cx: i32, cz: i32) -> Option<Vec<(String, Vec<super::structure::Bb>, Vec<String>)>> {
    use crate::nbt::Tag;
    let (rx, rz) = (cx >> 5, cz >> 5);
    let (chunks, _) = airworld::mca::read_mca(&PathBuf::from(dir).join(format!("r.{rx}.{rz}.mca")), rx, rz).ok()?;
    let idx = ((cz & 31) * 32 + (cx & 31)) as usize;
    let c = chunks.into_iter().find(|c| c.index == idx)?;
    let (_, root) = Tag::from_root_bytes(&c.raw).ok()?;
    let mut out = Vec::new();
    let Some(Tag::Compound(starts)) = root.get("structures").and_then(|s| s.get("starts")) else { return Some(out) };
    for (id, s) in starts {
        if s.get("id").and_then(|t| t.as_str()) == Some("INVALID") {
            continue;
        }
        let mut boxes = Vec::new();
        let mut names = Vec::new();
        if let Some(Tag::List(ch)) = s.get("Children") {
            for p in ch {
                if let Some(Tag::IntArray(b)) = p.get("BB") {
                    boxes.push(([b[0], b[1], b[2]], [b[3], b[4], b[5]]));
                }
                let loc = p.get("pool_element").or(p.get("Template")).map(|e| format!("{}", e.to_json())).unwrap_or_default();
                if std::env::var("RIME_DUMP_PIECE").is_ok() { eprintln!("   vanilla piece {}", p.to_json()); }
                names.push(loc);
            }
        }
        out.push((id.clone(), boxes, names));
    }
    Some(out)
}

/// RIME_VREF_S=<vanilla region dir> RIME_SCHUNKS="cx,cz;..": compares the
/// structure starts of those chunks with vanilla's.
#[test]
fn starts_match_vanilla() {
    let (Some(dir), Ok(vd), Ok(list)) = (data_dir(), std::env::var("RIME_VREF_S"), std::env::var("RIME_SCHUNKS")) else { return };
    let g = super::VanillaGenerator::new(&loader(dir), 12345).unwrap();
    let mut bad = 0;
    for c in list.split(';') {
        let (a, b) = c.split_once(',').unwrap();
        let (cx, cz): (i32, i32) = (a.parse().unwrap(), b.parse().unwrap());
        let v = vanilla_starts(&vd, cx, cz).unwrap_or_default();
        let ours = g.structure_pieces(cx, cz);
        eprintln!("chunk {cx},{cz}: vanilla {:?} / ours {:?}", v.iter().map(|s| (&s.0, s.1.len())).collect::<Vec<_>>(), ours.iter().map(|s| (&s.0, s.1.len())).collect::<Vec<_>>());
        for (name, boxes, names) in &v {
            let Some(o) = ours.iter().find(|o| name.ends_with(&o.0)) else { bad += 1; continue };
            // vanilla recomputes piece boxes from the template when a start is
            // loaded, so saved boxes lack the jigsaw "expansion hack" height
            let same = |a: &super::structure::Bb, b: &super::structure::Bb| a.0 == b.0 && a.1[0] == b.1[0] && a.1[2] == b.1[2] && a.1[1] >= b.1[1];
            for (i, b) in boxes.iter().enumerate() {
                if !o.1.get(i).is_some_and(|x| same(x, b)) {
                    eprintln!("  piece {i}: vanilla {b:?} {} / ours {:?} {}", names[i], o.1.get(i), o.2.get(i).cloned().unwrap_or_default());
                    bad += 1;
                    break;
                }
            }
            if boxes.len() != o.1.len() {
                bad += 1;
            }
        }
        if ours.len() != v.len() {
            bad += 1;
        }
    }
    assert_eq!(bad, 0);
}

/// RIME_VREF_S=<vanilla region dir> RIME_SRANGE="x0,z0,x1,z1": lists every
/// vanilla structure start in that chunk range, next to ours.
#[test]
fn list_vanilla_starts() {
    let (Some(dir), Ok(vd), Ok(r)) = (data_dir(), std::env::var("RIME_VREF_S"), std::env::var("RIME_SRANGE")) else { return };
    let v: Vec<i32> = r.split(',').map(|x| x.parse().unwrap()).collect();
    let g = super::VanillaGenerator::new(&loader(dir), 12345).unwrap();
    for cx in v[0]..=v[2] {
        for cz in v[1]..=v[3] {
            let s = vanilla_starts(&vd, cx, cz).unwrap_or_default();
            let o = g.structure_pieces(cx, cz);
            if !s.is_empty() || !o.is_empty() {
                eprintln!("chunk {cx},{cz}: vanilla {:?} / ours {:?}", s.iter().map(|s| (&s.0, s.1.len())).collect::<Vec<_>>(), o.iter().map(|s| (&s.0, s.1.len())).collect::<Vec<_>>());
            }
        }
    }
}

/// RIME_VREF_DIM = a vanilla world directory (seed 12345, biome features
/// removed, structures off) with nether and end chunks around 0,0 (and the
/// end around 64,64): compares terrain, surface, carvers and biomes.
#[test]
fn dims_match_vanilla() {
    let (Some(dir), Ok(world)) = (data_dir(), std::env::var("RIME_VREF_DIM")) else { return };
    let load = loader(dir);
    for (dim, sub, chunks) in [
        (super::DimKind::Nether, "dimensions/minecraft/the_nether", (-4..4).flat_map(|x| (-4..4).map(move |z| (x, z))).collect::<Vec<_>>()),
        (super::DimKind::End, "dimensions/minecraft/the_end", (-4..4).flat_map(|x| (-4..4).map(move |z| (x, z))).chain((64..68).flat_map(|x| (64..68).map(move |z| (x, z)))).collect()),
    ] {
        let g = super::VanillaGenerator::new_dim(&load, 12345, dim).unwrap();
        let reg = PathBuf::from(&world).join(sub).join("region");
        let mut by_kind: std::collections::BTreeMap<String, usize> = Default::default();
        let (mut total, mut tested, mut bb) = (0, 0, 0);
        for (cx, cz) in chunks {
            let (rx, rz) = (cx >> 5, cz >> 5);
            let Ok((list, _)) = airworld::mca::read_mca(&reg.join(format!("r.{rx}.{rz}.mca")), rx, rz) else { continue };
            let idx = ((cz & 31) * 32 + (cx & 31)) as usize;
            let Some(v) = list.into_iter().find(|c| c.index == idx).and_then(|c| crate::world::chunk::Chunk::from_nbt(&c.raw)) else { continue };
            tested += 1;
            let ours = g.generate(cx, cz);
            for si in 4..20 {
                for i in 0..64 {
                    if ours.biomes[si][i] != v.biomes[si][i] {
                        bb += 1;
                    }
                }
            }
            let mut bad = 0;
            for y in 0..256 {
                for z in 0..16 {
                    for x in 0..16 {
                        let a = ours.blocks[(((y + 64) * 16 + z) * 16 + x) as usize];
                        let b = v.get(x as usize, y, z as usize);
                        if a == b {
                            continue;
                        }
                        let r = crate::registry::BlockRegistry::get();
                        if a.is_air() && r.property(b, "level").is_some_and(|l| l != "0") {
                            continue;
                        }
                        *by_kind.entry(format!("{} -> {}", a.name(), b.name())).or_default() += 1;
                        if bad < 3 {
                            eprintln!("{sub} chunk {cx},{cz} at {x},{y},{z}: ours {} vanilla {}", a.name(), b.name());
                        }
                        bad += 1;
                    }
                }
            }
            total += bad;
        }
        eprintln!("{sub}: {tested} chunks, {total} block mismatches, {bb} biome mismatches: {by_kind:?}");
        assert!(tested > 0);
    }
}
