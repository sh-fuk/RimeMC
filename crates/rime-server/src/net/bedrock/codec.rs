//! Bedrock wire primitives: little-endian numbers, unsigned/zigzag varints,
//! varint-prefixed strings and network (varint) NBT.

use anyhow::{bail, Result};

#[derive(Default)]
pub struct W(pub Vec<u8>);

impl W {
    pub fn new() -> Self {
        W(Vec::with_capacity(64))
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(v as u8)
    }
    pub fn u16le(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i16le(&mut self, v: i16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i32le(&mut self, v: i32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32le(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i32be(&mut self, v: i32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i64le(&mut self, v: i64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64le(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn vec3(&mut self, v: [f32; 3]) -> &mut Self {
        self.f32(v[0]).f32(v[1]).f32(v[2])
    }
    pub fn var(&mut self, mut v: u64) -> &mut Self {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.0.push(b);
                return self;
            }
            self.0.push(b | 0x80);
        }
    }
    pub fn vu32(&mut self, v: u32) -> &mut Self {
        self.var(v as u64)
    }
    pub fn zz32(&mut self, v: i32) -> &mut Self {
        self.var(((v << 1) ^ (v >> 31)) as u32 as u64)
    }
    pub fn zz64(&mut self, v: i64) -> &mut Self {
        self.var(((v << 1) ^ (v >> 63)) as u64)
    }
    pub fn str(&mut self, s: &str) -> &mut Self {
        self.vu32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.vu32(b.len() as u32);
        self.0.extend_from_slice(b);
        self
    }
    pub fn raw(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
    /// Bedrock writes a UUID as two little-endian u64 halves.
    pub fn uuid(&mut self, u: uuid::Uuid) -> &mut Self {
        let v = u.as_u128();
        self.u64le((v >> 64) as u64).u64le(v as u64)
    }
    pub fn block_pos(&mut self, p: [i32; 3]) -> &mut Self {
        self.zz32(p[0]).vu32(p[1] as u32).zz32(p[2])
    }
    pub fn block_coords(&mut self, p: [i32; 3]) -> &mut Self {
        self.zz32(p[0]).zz32(p[1]).zz32(p[2])
    }
    /// An empty network-NBT compound.
    pub fn nbt_empty(&mut self) -> &mut Self {
        self.raw(&[0x0a, 0x00, 0x00])
    }
}

pub struct R<'a> {
    pub b: &'a [u8],
    pub pos: usize,
}

impl<'a> R<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        R { b, pos: 0 }
    }
    pub fn left(&self) -> usize {
        self.b.len() - self.pos
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.left() < n {
            bail!("packet too short");
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }
    pub fn u16le(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    pub fn i16le(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into()?))
    }
    pub fn i32le(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into()?))
    }
    pub fn i32be(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into()?))
    }
    pub fn i64le(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into()?))
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into()?))
    }
    pub fn vec3(&mut self) -> Result<[f32; 3]> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
    pub fn var(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = self.u8()?;
            v |= ((b & 0x7f) as u64) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bail!("varint too long")
    }
    pub fn vu32(&mut self) -> Result<u32> {
        Ok(self.var()? as u32)
    }
    pub fn zz32(&mut self) -> Result<i32> {
        let v = self.var()? as u32;
        Ok((v >> 1) as i32 ^ -((v & 1) as i32))
    }
    pub fn zz64(&mut self) -> Result<i64> {
        let v = self.var()?;
        Ok((v >> 1) as i64 ^ -((v & 1) as i64))
    }
    pub fn bytes(&mut self, max: usize) -> Result<&'a [u8]> {
        let n = self.vu32()? as usize;
        if n > max {
            bail!("field too long ({n} > {max})");
        }
        self.take(n)
    }
    pub fn str(&mut self, max: usize) -> Result<String> {
        Ok(String::from_utf8_lossy(self.bytes(max)?).into_owned())
    }
    pub fn le_str(&mut self, max: usize) -> Result<String> {
        let n = self.i32le()?;
        if n < 0 || n as usize > max {
            bail!("field too long");
        }
        Ok(String::from_utf8_lossy(self.take(n as usize)?).into_owned())
    }
    pub fn block_coords(&mut self) -> Result<[i32; 3]> {
        Ok([self.zz32()?, self.zz32()?, self.zz32()?])
    }
    pub fn block_pos(&mut self) -> Result<[i32; 3]> {
        Ok([self.zz32()?, self.vu32()? as i32, self.zz32()?])
    }
}

/// Bedrock packet header: the id lives in the low 10 bits.
pub fn packet(id: u32, body: &W) -> Vec<u8> {
    let mut w = W::new();
    w.vu32(id & 0x3ff);
    w.raw(&body.0);
    w.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        for v in [0i32, 1, -1, 63, -64, 300, i32::MAX, i32::MIN] {
            let mut w = W::new();
            w.zz32(v);
            assert_eq!(R::new(&w.0).zz32().unwrap(), v);
        }
        for v in [0i64, -5, i64::MAX, i64::MIN] {
            let mut w = W::new();
            w.zz64(v);
            assert_eq!(R::new(&w.0).zz64().unwrap(), v);
        }
        let mut w = W::new();
        w.vu32(300);
        assert_eq!(w.0, vec![0xac, 0x02]);
        assert!(R::new(&[0xff; 12]).var().is_err());
    }
}
