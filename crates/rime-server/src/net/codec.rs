//! Java Edition wire primitives (VarInt, strings, framing) with strict bounds.

use tokio::io::{AsyncRead, AsyncReadExt};

/// Vanilla's maximum packet length (2^21 - 1).
pub const MAX_PACKET: usize = 2_097_151;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CodecError {
    #[error("VarInt too long")]
    VarIntTooLong,
    #[error("unexpected end of packet")]
    Eof,
    #[error("packet too large ({0} bytes)")]
    TooLarge(usize),
    #[error("string too long")]
    StringTooLong,
    #[error("invalid UTF-8")]
    BadUtf8,
    #[error("negative length")]
    Negative,
}

pub fn write_varint(out: &mut Vec<u8>, v: i32) {
    let mut v = v as u32;
    loop {
        if v & !0x7F == 0 {
            out.push(v as u8);
            return;
        }
        out.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
}

pub fn write_string(out: &mut Vec<u8>, s: &str) {
    write_varint(out, s.len() as i32);
    out.extend_from_slice(s.as_bytes());
}

/// Build a framed packet: length ‖ id ‖ body (no compression).
pub fn frame(id: i32, body: &[u8]) -> Vec<u8> {
    let mut inner = Vec::with_capacity(body.len() + 5);
    write_varint(&mut inner, id);
    inner.extend_from_slice(body);
    let mut out = Vec::with_capacity(inner.len() + 5);
    write_varint(&mut out, inner.len() as i32);
    out.extend_from_slice(&inner);
    out
}

/// Cursor over a received packet body.
pub struct Reader<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn varint(&mut self) -> Result<i32, CodecError> {
        let mut v: u32 = 0;
        for i in 0..5 {
            let b = *self.buf.get(self.pos).ok_or(CodecError::Eof)?;
            self.pos += 1;
            v |= ((b & 0x7F) as u32) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v as i32);
            }
        }
        Err(CodecError::VarIntTooLong)
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        if self.remaining() < n {
            return Err(CodecError::Eof);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    pub fn i64(&mut self) -> Result<i64, CodecError> {
        Ok(i64::from_be_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    /// String with a maximum length in *characters* (vanilla semantics).
    pub fn string(&mut self, max_chars: usize) -> Result<&'a str, CodecError> {
        let len = self.varint()?;
        if len < 0 {
            return Err(CodecError::Negative);
        }
        let len = len as usize;
        if len > max_chars * 3 {
            return Err(CodecError::StringTooLong);
        }
        let s = std::str::from_utf8(self.bytes(len)?).map_err(|_| CodecError::BadUtf8)?;
        if s.chars().count() > max_chars {
            return Err(CodecError::StringTooLong);
        }
        Ok(s)
    }
}

/// Read one framed packet from a stream (uncompressed state).
pub async fn read_packet<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> anyhow::Result<Vec<u8>> {
    let mut len: u32 = 0;
    for i in 0..3 {
        // 3 bytes are enough for MAX_PACKET; longer prefixes are hostile
        let b = r.read_u8().await?;
        len |= ((b & 0x7F) as u32) << (7 * i);
        if b & 0x80 == 0 {
            let len = len as usize;
            if len == 0 || len > max.min(MAX_PACKET) {
                return Err(CodecError::TooLarge(len).into());
            }
            let mut buf = vec![0u8; len];
            r.read_exact(&mut buf).await?;
            return Ok(buf);
        }
    }
    Err(CodecError::VarIntTooLong.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [0, 1, 127, 128, 255, 25565, 2_097_151, i32::MAX, -1, i32::MIN] {
            let mut b = Vec::new();
            write_varint(&mut b, v);
            assert!(b.len() <= 5);
            assert_eq!(Reader::new(&b).varint().unwrap(), v);
        }
        assert_eq!(Reader::new(&[0xFF; 6]).varint(), Err(CodecError::VarIntTooLong));
    }

    #[test]
    fn strings_are_bounded() {
        let mut b = Vec::new();
        write_string(&mut b, "Steve");
        assert_eq!(Reader::new(&b).string(16).unwrap(), "Steve");
        let mut b = Vec::new();
        write_string(&mut b, &"x".repeat(17));
        assert!(Reader::new(&b).string(16).is_err());
        let mut b = Vec::new();
        write_varint(&mut b, 1_000_000);
        assert!(Reader::new(&b).string(16).is_err());
    }

    #[tokio::test]
    async fn framing_limits() {
        let p = frame(0, b"hi");
        let mut r = &p[..];
        assert_eq!(read_packet(&mut r, 100).await.unwrap(), [0, b'h', b'i']);
        let mut huge: &[u8] = &[0xFF, 0xFF, 0x7F];
        assert!(read_packet(&mut huge, 100).await.is_err());
    }
}
