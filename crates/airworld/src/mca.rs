//! Reader / writer for vanilla Anvil (`.mca`) region files, used by the
//! converter and by RimeMC's "vanilla" storage backend.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::region::{Error, Result, MAX_RAW_CHUNK, MAX_REGION_FILE};
use crate::REGION_CHUNKS;

const SECTOR: usize = 4096;
const MAX_SECTORS_PER_CHUNK: usize = 255;

/// One chunk read from an `.mca` file.
pub struct McaChunk {
    pub index: usize,
    pub timestamp: u32,
    /// Uncompressed NBT.
    pub raw: Vec<u8>,
}

fn decompress(kind: u8, data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let limit = MAX_RAW_CHUNK as u64 + 1;
    match kind {
        1 => {
            GzDecoder::new(data).take(limit).read_to_end(&mut out)?;
        }
        2 => {
            ZlibDecoder::new(data).take(limit).read_to_end(&mut out)?;
        }
        3 => out.extend_from_slice(data),
        4 => return Err(Error::Malformed("LZ4-compressed .mca chunks are not supported yet; re-save the world with zlib")),
        127 => return Err(Error::Malformed("custom-compressed .mca chunks are not supported")),
        _ => return Err(Error::Malformed("unknown .mca compression type")),
    }
    if out.len() > MAX_RAW_CHUNK {
        return Err(Error::ChunkTooLarge(out.len()));
    }
    Ok(out)
}

/// Read every chunk of an `.mca` file. `region_x/z` are needed to locate
/// external `.mcc` files for oversized chunks. Damaged chunks are returned in
/// the second list instead of aborting the whole region.
pub fn read_mca(path: &Path, region_x: i32, region_z: i32) -> Result<(Vec<McaChunk>, Vec<(usize, Error)>)> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    if len > MAX_REGION_FILE {
        return Err(Error::Malformed("region file too large"));
    }
    let mut buf = Vec::with_capacity(len as usize);
    f.read_to_end(&mut buf)?;
    if buf.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    if buf.len() < 2 * SECTOR {
        return Err(Error::Truncated);
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut chunks = Vec::new();
    let mut damaged = Vec::new();
    for i in 0..REGION_CHUNKS {
        let loc = u32::from_be_bytes(buf[i * 4..i * 4 + 4].try_into().unwrap());
        if loc == 0 {
            continue;
        }
        let ts = u32::from_be_bytes(buf[SECTOR + i * 4..SECTOR + i * 4 + 4].try_into().unwrap());
        let sector = (loc >> 8) as usize;
        let count = (loc & 0xFF) as usize;
        let start = sector * SECTOR;
        if sector < 2 || count == 0 || start + 5 > buf.len() {
            damaged.push((i, Error::Malformed("bad chunk location")));
            continue;
        }
        let length = u32::from_be_bytes(buf[start..start + 4].try_into().unwrap()) as usize;
        let kind = buf[start + 4];
        let result = if kind & 0x80 != 0 {
            let cx = region_x * 32 + (i % 32) as i32;
            let cz = region_z * 32 + (i / 32) as i32;
            let ext = dir.join(format!("c.{cx}.{cz}.mcc"));
            fs::read(&ext).map_err(Error::from).and_then(|d| decompress(kind & 0x7F, &d))
        } else if length == 0 || start + 4 + length > buf.len() {
            Err(Error::Malformed("chunk length out of bounds"))
        } else {
            decompress(kind, &buf[start + 5..start + 4 + length])
        };
        match result {
            Ok(raw) => chunks.push(McaChunk { index: i, timestamp: ts, raw }),
            Err(e) => damaged.push((i, e)),
        }
    }
    Ok((chunks, damaged))
}

/// Write an `.mca` file (zlib, like vanilla). Chunks too big for 255 sectors go
/// to an external `.mcc` file exactly like vanilla does. Atomic via temp+rename.
pub fn write_mca(path: &Path, region_x: i32, region_z: i32, chunks: &[McaChunk]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut header = vec![0u8; 2 * SECTOR];
    let mut body: Vec<u8> = Vec::new();
    let mut next_sector = 2usize;
    for c in chunks {
        if c.index >= REGION_CHUNKS {
            return Err(Error::Malformed("chunk index out of range"));
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&c.raw)?;
        let data = enc.finish()?;
        let cx = region_x * 32 + (c.index % 32) as i32;
        let cz = region_z * 32 + (c.index / 32) as i32;
        let ext = dir.join(format!("c.{cx}.{cz}.mcc"));
        let mut rec = Vec::new();
        if data.len() + 5 > MAX_SECTORS_PER_CHUNK * SECTOR {
            fs::write(&ext, &data)?;
            rec.extend_from_slice(&1u32.to_be_bytes());
            rec.push(0x82);
        } else {
            let _ = fs::remove_file(&ext);
            rec.extend_from_slice(&((data.len() + 1) as u32).to_be_bytes());
            rec.push(2);
            rec.extend_from_slice(&data);
        }
        let sectors = rec.len().div_ceil(SECTOR);
        rec.resize(sectors * SECTOR, 0);
        if next_sector >= (1 << 24) {
            return Err(Error::Malformed("region too large for .mca"));
        }
        let loc = ((next_sector as u32) << 8) | sectors as u32;
        header[c.index * 4..c.index * 4 + 4].copy_from_slice(&loc.to_be_bytes());
        header[SECTOR + c.index * 4..SECTOR + c.index * 4 + 4].copy_from_slice(&c.timestamp.to_be_bytes());
        body.extend_from_slice(&rec);
        next_sector += sectors;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    {
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
        f.write_all(&header)?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Parse `r.<x>.<z>.<ext>` → (x, z).
pub fn parse_region_name(name: &str, ext: &str) -> Option<(i32, i32)> {
    let rest = name.strip_prefix("r.")?.strip_suffix(ext)?.strip_suffix('.')?;
    let (x, z) = rest.split_once('.')?;
    Some((x.parse().ok()?, z.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(parse_region_name("r.-1.20.mca", "mca"), Some((-1, 20)));
        assert_eq!(parse_region_name("r.0.0.air", "air"), Some((0, 0)));
        assert_eq!(parse_region_name("r.0.mca", "mca"), None);
        assert_eq!(parse_region_name("level.dat", "mca"), None);
    }

    #[test]
    fn mca_roundtrip_with_oversized() {
        let dir = std::env::temp_dir().join(format!("mca-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("r.1.-1.mca");
        // pseudo-random data so zlib can't shrink it below 1 MiB
        let mut s = 99u64;
        let big: Vec<u8> = (0..1_200_000)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (s >> 56) as u8
            })
            .collect();
        let chunks = vec![
            McaChunk { index: 3, timestamp: 42, raw: b"hello nbt".to_vec() },
            McaChunk { index: 900, timestamp: 7, raw: big.clone() },
        ];
        write_mca(&p, 1, -1, &chunks).unwrap();
        let (back, dmg) = read_mca(&p, 1, -1).unwrap();
        assert!(dmg.is_empty());
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].raw, b"hello nbt");
        assert_eq!(back[0].timestamp, 42);
        assert_eq!(back[1].raw, big);
        fs::remove_dir_all(&dir).unwrap();
    }
}
