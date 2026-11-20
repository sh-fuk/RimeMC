//! Framing with optional zlib compression (vanilla "set compression" format).

use std::io::{Read, Write};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::codec::{write_varint, CodecError, Reader, MAX_PACKET};

/// Largest decompressed packet accepted from a client (vanilla: 8 MiB).
pub const MAX_DECOMPRESSED: usize = 8 * 1024 * 1024;

/// Encode one packet. `threshold < 0` = compression off.
pub fn encode(id: i32, body: &[u8], threshold: i32) -> Vec<u8> {
    let mut inner = Vec::with_capacity(body.len() + 5);
    write_varint(&mut inner, id);
    inner.extend_from_slice(body);
    let mut out = Vec::with_capacity(inner.len() + 10);
    if threshold < 0 {
        write_varint(&mut out, inner.len() as i32);
        out.extend_from_slice(&inner);
    } else if inner.len() < threshold as usize {
        write_varint(&mut out, inner.len() as i32 + 1);
        out.push(0);
        out.extend_from_slice(&inner);
    } else {
        let mut z = ZlibEncoder::new(Vec::with_capacity(inner.len() / 2), Compression::new(4));
        z.write_all(&inner).expect("in-memory write");
        let comp = z.finish().expect("in-memory write");
        let mut dlen = Vec::new();
        write_varint(&mut dlen, inner.len() as i32);
        write_varint(&mut out, (dlen.len() + comp.len()) as i32);
        out.extend_from_slice(&dlen);
        out.extend_from_slice(&comp);
    }
    out
}

/// Read one packet (id ‖ body), decompressing when compression is on.
pub async fn read<R: AsyncRead + Unpin>(r: &mut R, compressed: bool, max: usize) -> anyhow::Result<Vec<u8>> {
    let mut len: u32 = 0;
    let mut done = false;
    for i in 0..3 {
        let b = r.read_u8().await?;
        len |= ((b & 0x7F) as u32) << (7 * i);
        if b & 0x80 == 0 {
            done = true;
            break;
        }
    }
    anyhow::ensure!(done, CodecError::VarIntTooLong);
    let len = len as usize;
    if len == 0 || len > MAX_PACKET {
        return Err(CodecError::TooLarge(len).into());
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    if !compressed {
        anyhow::ensure!(len <= max, CodecError::TooLarge(len));
        return Ok(buf);
    }
    let mut rd = Reader::new(&buf);
    let dlen = rd.varint()?;
    if dlen == 0 {
        let rest = buf[rd.pos..].to_vec();
        anyhow::ensure!(rest.len() <= max, CodecError::TooLarge(rest.len()));
        return Ok(rest);
    }
    anyhow::ensure!(dlen > 0 && dlen as usize <= max.min(MAX_DECOMPRESSED), CodecError::TooLarge(dlen as usize));
    let mut out = Vec::with_capacity(dlen as usize);
    // never inflate more than the declared size (zip-bomb guard)
    ZlibDecoder::new(&buf[rd.pos..]).take(dlen as u64 + 1).read_to_end(&mut out)?;
    anyhow::ensure!(out.len() == dlen as usize, "decompressed size mismatch");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_with_and_without_compression() {
        let body: Vec<u8> = (0..5000u32).map(|i| (i % 7) as u8).collect();
        for th in [-1, 0, 256, 100_000] {
            let p = encode(0x2d, &body, th);
            let mut r = &p[..];
            let got = read(&mut r, th >= 0, MAX_DECOMPRESSED).await.unwrap();
            assert_eq!(got[0], 0x2d);
            assert_eq!(&got[1..], &body[..]);
        }
    }

    #[tokio::test]
    async fn lying_size_is_rejected() {
        // claims 10 bytes decompressed, actually contains 5000
        let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
        z.write_all(&[0u8; 5000]).unwrap();
        let comp = z.finish().unwrap();
        let mut inner = Vec::new();
        write_varint(&mut inner, 10);
        inner.extend_from_slice(&comp);
        let mut p = Vec::new();
        write_varint(&mut p, inner.len() as i32);
        p.extend_from_slice(&inner);
        let mut r = &p[..];
        assert!(read(&mut r, true, MAX_DECOMPRESSED).await.is_err());
    }
}
