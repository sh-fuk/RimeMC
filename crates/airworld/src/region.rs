//! In-memory representation and (de)serialisation of a single `.air` region.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use xxhash_rust::xxh3::xxh3_64;

use crate::REGION_CHUNKS;

pub const MAGIC: &[u8; 8] = b"AIRWRLD\0";
pub const FOOTER_MAGIC: &[u8; 8] = b"AIREND\0\0";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 32;
pub const BITMAP_LEN: usize = REGION_CHUNKS / 8;
pub const ENTRY_LEN: usize = 28;
pub const FOOTER_LEN: usize = 16;

/// Hard cap on the uncompressed size of one chunk. Vanilla chunks are rarely
/// larger than a few MiB; anything above this is treated as corruption or an
/// attack (decompression bomb) instead of being allocated.
pub const MAX_RAW_CHUNK: usize = 64 * 1024 * 1024;
/// Hard cap on the size of a whole region file we are willing to read.
pub const MAX_REGION_FILE: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not an airworld file (bad magic)")]
    BadMagic,
    #[error("unsupported airworld version {0}")]
    UnsupportedVersion(u16),
    #[error("airworld header/index checksum mismatch")]
    HeaderChecksum,
    #[error("airworld file truncated or footer damaged")]
    Truncated,
    #[error("airworld file is malformed: {0}")]
    Malformed(&'static str),
    #[error("chunk {0} is corrupted (checksum mismatch)")]
    ChunkChecksum(usize),
    #[error("chunk {0} failed to decompress: {1}")]
    ChunkDecompress(usize, String),
    #[error("chunk is too large ({0} bytes)")]
    ChunkTooLarge(usize),
    #[error("region coordinate mismatch: file says ({0},{1}), expected ({2},{3})")]
    CoordMismatch(i32, i32, i32, i32),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One stored chunk. Data is kept *compressed* in memory, which is what makes
/// the RAM-world mode cheap: a typical chunk is 5–15× smaller compressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkSlot {
    pub timestamp: u32,
    pub raw_len: u32,
    pub compressed: Vec<u8>,
}

impl ChunkSlot {
    /// Compress raw NBT into a slot.
    pub fn from_raw(raw: &[u8], timestamp: u32, level: i32) -> Result<Self> {
        if raw.len() > MAX_RAW_CHUNK {
            return Err(Error::ChunkTooLarge(raw.len()));
        }
        let compressed = zstd::bulk::compress(raw, level)?;
        Ok(Self { timestamp, raw_len: raw.len() as u32, compressed })
    }

    /// Decompress to raw NBT. The output buffer is sized exactly from the
    /// stored length (bounded by `MAX_RAW_CHUNK`) and the result must match it.
    pub fn decompress(&self, index: usize) -> Result<Vec<u8>> {
        let want = self.raw_len as usize;
        if want > MAX_RAW_CHUNK {
            return Err(Error::ChunkTooLarge(want));
        }
        let out = zstd::bulk::decompress(&self.compressed, want)
            .map_err(|e| Error::ChunkDecompress(index, e.to_string()))?;
        if out.len() != want {
            return Err(Error::ChunkDecompress(index, "length mismatch".into()));
        }
        Ok(out)
    }
}

/// A full region (32×32 chunks).
#[derive(Clone, Debug)]
pub struct AirRegion {
    pub x: i32,
    pub z: i32,
    pub level: i32,
    slots: Box<[Option<ChunkSlot>]>,
}

impl AirRegion {
    pub fn new(x: i32, z: i32, level: i32) -> Self {
        Self { x, z, level: level.clamp(1, 22), slots: vec![None; REGION_CHUNKS].into_boxed_slice() }
    }

    pub fn chunk_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(|s| s.is_none())
    }

    pub fn slot(&self, index: usize) -> Option<&ChunkSlot> {
        self.slots.get(index).and_then(|s| s.as_ref())
    }

    pub fn iter(&self) -> impl Iterator<Item = (usize, &ChunkSlot)> {
        self.slots.iter().enumerate().filter_map(|(i, s)| s.as_ref().map(|s| (i, s)))
    }

    /// Store raw NBT for a chunk (compresses immediately).
    pub fn put_raw(&mut self, index: usize, raw: &[u8], timestamp: u32) -> Result<()> {
        if index >= REGION_CHUNKS {
            return Err(Error::Malformed("chunk index out of range"));
        }
        self.slots[index] = Some(ChunkSlot::from_raw(raw, timestamp, self.level)?);
        Ok(())
    }

    pub fn put_slot(&mut self, index: usize, slot: ChunkSlot) {
        if index < REGION_CHUNKS {
            self.slots[index] = Some(slot);
        }
    }

    pub fn remove(&mut self, index: usize) -> Option<ChunkSlot> {
        self.slots.get_mut(index).and_then(|s| s.take())
    }

    /// Read and decompress one chunk.
    pub fn get_raw(&self, index: usize) -> Result<Option<Vec<u8>>> {
        match self.slot(index) {
            Some(s) => s.decompress(index).map(Some),
            None => Ok(None),
        }
    }

    // ------------------------------------------------------------------ encode

    /// Serialise to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let present: Vec<(usize, &ChunkSlot)> = self.iter().collect();
        let count = present.len();
        let meta_len = HEADER_LEN + BITMAP_LEN + count * ENTRY_LEN + 8;
        let data_len: usize = present.iter().map(|(_, s)| s.compressed.len()).sum();
        let mut out = Vec::with_capacity(meta_len + data_len + FOOTER_LEN);

        let newest = present.iter().map(|(_, s)| s.timestamp as u64).max().unwrap_or(0);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.x.to_le_bytes());
        out.extend_from_slice(&self.z.to_le_bytes());
        out.extend_from_slice(&(count as u16).to_le_bytes());
        out.push(self.level as i8 as u8);
        out.push(0);
        out.extend_from_slice(&newest.to_le_bytes());

        let mut bitmap = [0u8; BITMAP_LEN];
        for (i, _) in &present {
            bitmap[i / 8] |= 1 << (i % 8);
        }
        out.extend_from_slice(&bitmap);

        let mut offset = meta_len as u64;
        for (_, s) in &present {
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&(s.compressed.len() as u32).to_le_bytes());
            out.extend_from_slice(&s.raw_len.to_le_bytes());
            out.extend_from_slice(&s.timestamp.to_le_bytes());
            out.extend_from_slice(&xxh3_64(&s.compressed).to_le_bytes());
            offset += s.compressed.len() as u64;
        }
        let hdr_hash = xxh3_64(&out);
        out.extend_from_slice(&hdr_hash.to_le_bytes());
        debug_assert_eq!(out.len(), meta_len);

        for (_, s) in &present {
            out.extend_from_slice(&s.compressed);
        }
        let total = (out.len() + FOOTER_LEN) as u64;
        out.extend_from_slice(&total.to_le_bytes());
        out.extend_from_slice(FOOTER_MAGIC);
        out
    }

    // ------------------------------------------------------------------ decode

    /// Parse a region from bytes. Every length and offset coming from the file
    /// is validated before use. Damaged chunk payloads are *not* fatal: they are
    /// reported in the returned list and left out, so one bad sector only costs
    /// the affected chunk.
    pub fn from_bytes(buf: &[u8]) -> Result<(Self, Vec<(usize, Error)>)> {
        if buf.len() < HEADER_LEN + BITMAP_LEN + 8 + FOOTER_LEN {
            return Err(Error::Truncated);
        }
        if &buf[0..8] != MAGIC {
            return Err(Error::BadMagic);
        }
        let version = u16::from_le_bytes([buf[8], buf[9]]);
        if version != VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let flags = u16::from_le_bytes([buf[10], buf[11]]);
        if flags != 0 {
            return Err(Error::Malformed("unknown flags"));
        }
        let x = i32::from_le_bytes(buf[12..16].try_into().unwrap());
        let z = i32::from_le_bytes(buf[16..20].try_into().unwrap());
        let count = u16::from_le_bytes([buf[20], buf[21]]) as usize;
        let level = buf[22] as i8 as i32;
        if count > REGION_CHUNKS {
            return Err(Error::Malformed("chunk count > 1024"));
        }

        // footer
        let flen = buf.len();
        if &buf[flen - 8..] != FOOTER_MAGIC {
            return Err(Error::Truncated);
        }
        let total = u64::from_le_bytes(buf[flen - 16..flen - 8].try_into().unwrap());
        if total != flen as u64 {
            return Err(Error::Truncated);
        }

        let meta_len = HEADER_LEN + BITMAP_LEN + count * ENTRY_LEN + 8;
        if meta_len + FOOTER_LEN > flen {
            return Err(Error::Truncated);
        }
        let stored_hash = u64::from_le_bytes(buf[meta_len - 8..meta_len].try_into().unwrap());
        if xxh3_64(&buf[..meta_len - 8]) != stored_hash {
            return Err(Error::HeaderChecksum);
        }

        let bitmap = &buf[HEADER_LEN..HEADER_LEN + BITMAP_LEN];
        let indices: Vec<usize> =
            (0..REGION_CHUNKS).filter(|i| bitmap[i / 8] & (1 << (i % 8)) != 0).collect();
        if indices.len() != count {
            return Err(Error::Malformed("bitmap does not match chunk count"));
        }

        let data_end = (flen - FOOTER_LEN) as u64;
        let mut region = AirRegion::new(x, z, if level <= 0 { 3 } else { level });
        let mut damaged = Vec::new();
        let mut p = HEADER_LEN + BITMAP_LEN;
        for idx in indices {
            let e = &buf[p..p + ENTRY_LEN];
            p += ENTRY_LEN;
            let off = u64::from_le_bytes(e[0..8].try_into().unwrap());
            let clen = u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64;
            let rlen = u32::from_le_bytes(e[12..16].try_into().unwrap());
            let ts = u32::from_le_bytes(e[16..20].try_into().unwrap());
            let hash = u64::from_le_bytes(e[20..28].try_into().unwrap());

            let end = match off.checked_add(clen) {
                Some(end) if off >= meta_len as u64 && end <= data_end => end,
                _ => {
                    damaged.push((idx, Error::Malformed("chunk out of bounds")));
                    continue;
                }
            };
            if rlen as usize > MAX_RAW_CHUNK {
                damaged.push((idx, Error::ChunkTooLarge(rlen as usize)));
                continue;
            }
            let data = &buf[off as usize..end as usize];
            if xxh3_64(data) != hash {
                damaged.push((idx, Error::ChunkChecksum(idx)));
                continue;
            }
            region.slots[idx] = Some(ChunkSlot { timestamp: ts, raw_len: rlen, compressed: data.to_vec() });
        }
        Ok((region, damaged))
    }

    // ------------------------------------------------------------------ files

    pub fn file_name(x: i32, z: i32) -> String {
        format!("r.{x}.{z}.air")
    }

    /// Read a region file. Returns the region and the list of damaged chunks.
    pub fn read_file(path: &Path) -> Result<(Self, Vec<(usize, Error)>)> {
        let mut f = File::open(path)?;
        let len = f.metadata()?.len();
        if len > MAX_REGION_FILE {
            return Err(Error::Malformed("region file too large"));
        }
        let mut buf = Vec::with_capacity(len as usize);
        f.read_to_end(&mut buf)?;
        Self::from_bytes(&buf)
    }

    /// Read a region file and check that it really is region (x, z).
    pub fn read_file_expect(path: &Path, x: i32, z: i32) -> Result<(Self, Vec<(usize, Error)>)> {
        let (r, d) = Self::read_file(path)?;
        if r.x != x || r.z != z {
            return Err(Error::CoordMismatch(r.x, r.z, x, z));
        }
        Ok((r, d))
    }

    /// Crash-safe write: write a temp file, fsync it, atomically rename it over
    /// the target and fsync the directory. A crash at any point leaves either
    /// the old or the new complete file, never a half-written one.
    /// An empty region deletes the file instead.
    pub fn write_file(&self, path: &Path) -> Result<()> {
        if self.is_empty() {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            return Ok(());
        }
        let bytes = self.to_bytes();
        let tmp = tmp_path(path);
        {
            let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        if let Some(dir) = path.parent() {
            // Directory fsync is best-effort (not supported on every platform).
            if let Ok(d) = File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AirRegion {
        let mut r = AirRegion::new(-3, 7, 3);
        for i in [0usize, 1, 31, 32, 500, 1023] {
            let raw: Vec<u8> = (0..(i * 37 + 100)).map(|b| (b * i) as u8).collect();
            r.put_raw(i, &raw, i as u32 * 10).unwrap();
        }
        r
    }

    #[test]
    fn roundtrip() {
        let r = sample();
        let bytes = r.to_bytes();
        let (back, damaged) = AirRegion::from_bytes(&bytes).unwrap();
        assert!(damaged.is_empty());
        assert_eq!(back.x, -3);
        assert_eq!(back.z, 7);
        assert_eq!(back.chunk_count(), 6);
        for (i, s) in r.iter() {
            assert_eq!(back.slot(i), Some(s));
            assert_eq!(back.get_raw(i).unwrap(), r.get_raw(i).unwrap());
        }
    }

    #[test]
    fn corruption_is_local() {
        let r = sample();
        let mut bytes = r.to_bytes();
        // flip a byte inside the last chunk's data
        let n = bytes.len();
        bytes[n - FOOTER_LEN - 3] ^= 0xFF;
        let (back, damaged) = AirRegion::from_bytes(&bytes).unwrap();
        assert_eq!(damaged.len(), 1);
        assert_eq!(back.chunk_count(), 5);
    }

    #[test]
    fn header_and_truncation_detected() {
        let bytes = sample().to_bytes();
        let mut h = bytes.clone();
        h[13] ^= 1;
        assert!(matches!(AirRegion::from_bytes(&h), Err(Error::HeaderChecksum)));
        assert!(matches!(AirRegion::from_bytes(&bytes[..bytes.len() - 1]), Err(Error::Truncated)));
        assert!(AirRegion::from_bytes(&[]).is_err());
    }

    #[test]
    fn fuzz_does_not_panic() {
        let bytes = sample().to_bytes();
        let mut seed = 0x1234_5678u64;
        for _ in 0..2000 {
            let mut b = bytes.clone();
            for _ in 0..4 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let pos = (seed as usize) % b.len();
                b[pos] = (seed >> 32) as u8;
            }
            let _ = AirRegion::from_bytes(&b);
        }
    }

    #[test]
    fn file_write_read() {
        let dir = std::env::temp_dir().join(format!("airworld-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join(AirRegion::file_name(-3, 7));
        sample().write_file(&p).unwrap();
        let (r, _) = AirRegion::read_file_expect(&p, -3, 7).unwrap();
        assert_eq!(r.chunk_count(), 6);
        assert!(AirRegion::read_file_expect(&p, 0, 0).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
