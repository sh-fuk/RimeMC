//! # AirWorld region format (`.air`)
//!
//! AirWorld is RimeMC's region storage format. It is inspired by the Linear
//! region format (xymb-endcrystalme/LinearRegionFileFormatTools) but fixes the
//! main weaknesses of that format:
//!
//! | Problem in Linear                                   | AirWorld                                              |
//! |-----------------------------------------------------|-------------------------------------------------------|
//! | Whole region is one zstd stream → reading one chunk  | Every chunk is an independent zstd frame, so a single |
//! | decompresses all 1024 chunks                         | chunk can be read with one `pread`-sized slice        |
//! | One flipped byte destroys the whole region           | Per-chunk xxh3 checksums: damage stays in that chunk  |
//! | No integrity check of the header                     | Header + index are checksummed                        |
//! | Truncated files are hard to detect                   | Footer carries the total length + magic               |
//! | Untrusted sizes → decompression bombs                | Hard caps on every length, exact-size decompression   |
//!
//! Chunk payloads are stored as **raw, uncompressed NBT** inside the zstd frame,
//! exactly as vanilla writes it before its own compression step. Conversion
//! between `.mca` and `.air` is therefore lossless (byte-identical NBT) in both
//! directions, including the per-chunk timestamps and oversized chunks that
//! vanilla stores in external `.mcc` files.
//!
//! ## On-disk layout (all integers little-endian)
//!
//! ```text
//! Header (32 bytes)
//!   0  [u8; 8]  magic  = "AIRWRLD\0"
//!   8  u16      version = 1
//!  10  u16      flags (reserved, must be 0 in v1)
//!  12  i32      region x
//!  16  i32      region z
//!  20  u16      chunk count (0..=1024)
//!  22  i8       zstd level used when written (informational)
//!  23  u8       reserved = 0
//!  24  u64      newest chunk timestamp (seconds)
//! Presence bitmap (128 bytes, bit i = chunk index i present)
//! Index (chunk_count × 28 bytes, in ascending chunk index order)
//!   u64 absolute offset of compressed data
//!   u32 compressed length
//!   u32 raw (uncompressed NBT) length
//!   u32 timestamp (seconds, as in .mca)
//!   u64 xxh3-64 of the compressed bytes
//! u64 xxh3-64 of header + bitmap + index
//! Chunk data blobs (independent zstd frames)
//! Footer (16 bytes)
//!   u64      total file length
//!   [u8; 8]  magic = "AIREND\0\0"
//! ```

pub mod convert;
pub mod mca;
pub mod region;

pub use region::{AirRegion, ChunkSlot, Error, Result};

/// Number of chunks along one side of a region.
pub const REGION_SIDE: usize = 32;
/// Number of chunks in a region.
pub const REGION_CHUNKS: usize = REGION_SIDE * REGION_SIDE;

/// Index of a chunk inside its region (0..1024) from absolute chunk coordinates.
#[inline]
pub fn chunk_index(chunk_x: i32, chunk_z: i32) -> usize {
    ((chunk_x & 31) as usize) + ((chunk_z & 31) as usize) * REGION_SIDE
}

/// Region coordinate containing the given chunk coordinate.
#[inline]
pub fn region_coord(chunk: i32) -> i32 {
    chunk >> 5
}
