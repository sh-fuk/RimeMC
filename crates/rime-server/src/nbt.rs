//! Minimal, allocation-bounded NBT (Named Binary Tag) implementation.
//!
//! The reader is hardened for untrusted input: nesting depth, element counts
//! and total size are bounded, and every length is checked against the
//! remaining input *before* allocating.

#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(Vec<Tag>),
    Compound(Vec<(String, Tag)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

pub const MAX_DEPTH: usize = 512;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum NbtError {
    #[error("unexpected end of NBT data")]
    Eof,
    #[error("invalid NBT tag id {0}")]
    BadTag(u8),
    #[error("NBT nested too deeply")]
    TooDeep,
    #[error("negative or oversized NBT length")]
    BadLength,
    #[error("invalid UTF-8 in NBT string")]
    BadUtf8,
    #[error("heterogeneous NBT list")]
    BadList,
}

impl Tag {
    pub fn id(&self) -> u8 {
        match self {
            Tag::Byte(_) => 1,
            Tag::Short(_) => 2,
            Tag::Int(_) => 3,
            Tag::Long(_) => 4,
            Tag::Float(_) => 5,
            Tag::Double(_) => 6,
            Tag::ByteArray(_) => 7,
            Tag::String(_) => 8,
            Tag::List(_) => 9,
            Tag::Compound(_) => 10,
            Tag::IntArray(_) => 11,
            Tag::LongArray(_) => 12,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(v) => v.iter().find(|(k, _)| k == key).map(|(_, t)| t),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Tag::Byte(v) => Some(v as i64),
            Tag::Short(v) => Some(v as i64),
            Tag::Int(v) => Some(v as i64),
            Tag::Long(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::String(s) => Some(s),
            _ => None,
        }
    }

    // ------------------------------------------------------------- writing

    /// Write as a named root tag (the format used in files and chunks).
    pub fn to_root_bytes(&self, name: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(4096);
        out.push(self.id());
        write_str(&mut out, name);
        self.write_payload(&mut out);
        out
    }

    /// Network NBT (1.20.2+): root without a name.
    pub fn to_network_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(self.id());
        self.write_payload(&mut out);
        out
    }

    fn write_payload(&self, out: &mut Vec<u8>) {
        match self {
            Tag::Byte(v) => out.push(*v as u8),
            Tag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::Double(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::ByteArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                out.extend(v.iter().map(|b| *b as u8));
            }
            Tag::String(s) => write_str(out, s),
            Tag::List(v) => {
                out.push(v.first().map(|t| t.id()).unwrap_or(0));
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for t in v {
                    t.write_payload(out);
                }
            }
            Tag::Compound(v) => {
                for (k, t) in v {
                    out.push(t.id());
                    write_str(out, k);
                    t.write_payload(out);
                }
                out.push(0);
            }
            Tag::IntArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for x in v {
                    out.extend_from_slice(&x.to_be_bytes());
                }
            }
            Tag::LongArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for x in v {
                    out.extend_from_slice(&x.to_be_bytes());
                }
            }
        }
    }

    // ------------------------------------------------------------- reading

    /// Parse a named root tag. Returns (name, tag).
    pub fn from_root_bytes(buf: &[u8]) -> Result<(String, Tag), NbtError> {
        let mut r = Reader { buf, pos: 0 };
        let id = r.u8()?;
        if id == 0 {
            return Err(NbtError::BadTag(0));
        }
        let name = r.string()?;
        let tag = r.payload(id, 0)?;
        Ok((name, tag))
    }
}

impl Tag {
    /// Convert JSON (datapack format) to NBT the way vanilla's codecs accept it:
    /// integers → Int/Long, fractions → Double, booleans → Byte, and
    /// heterogeneous lists wrapped as `{"": value}` compounds.
    pub fn from_json(v: &serde_json::Value) -> Tag {
        use serde_json::Value as V;
        match v {
            V::Null => Tag::Compound(vec![]),
            V::Bool(b) => Tag::Byte(*b as i8),
            V::Number(n) => {
                if let Some(i) = n.as_i64() {
                    if i >= i32::MIN as i64 && i <= i32::MAX as i64 { Tag::Int(i as i32) } else { Tag::Long(i) }
                } else {
                    Tag::Double(n.as_f64().unwrap_or(0.0))
                }
            }
            V::String(s) => Tag::String(s.clone()),
            V::Array(a) => {
                let items: Vec<Tag> = a.iter().map(Tag::from_json).collect();
                let homogeneous = items.windows(2).all(|w| w[0].id() == w[1].id());
                if homogeneous {
                    Tag::List(items)
                } else {
                    Tag::List(items.into_iter().map(|t| if let Tag::Compound(_) = t { t } else { Tag::Compound(vec![(String::new(), t)]) }).collect())
                }
            }
            V::Object(o) => Tag::Compound(o.iter().map(|(k, v)| (k.clone(), Tag::from_json(v))).collect()),
        }
    }

    /// Plain text chat component.
    pub fn text(s: &str) -> Tag {
        Tag::Compound(vec![("text".into(), Tag::String(s.into()))])
    }
}

/// Java "modified UTF-8" is identical to UTF-8 for everything except NUL and
/// supplementary characters; we write plain UTF-8, which vanilla accepts.
fn write_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let len = b.len().min(u16::MAX as usize);
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(&b[..len]);
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], NbtError> {
        let end = self.pos.checked_add(n).ok_or(NbtError::Eof)?;
        if end > self.buf.len() {
            return Err(NbtError::Eof);
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, NbtError> {
        Ok(self.take(1)?[0])
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], NbtError> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn len(&mut self, elem_size: usize) -> Result<usize, NbtError> {
        let n = i32::from_be_bytes(self.arr()?);
        if n < 0 {
            return Err(NbtError::BadLength);
        }
        let n = n as usize;
        // every element needs at least `elem_size` bytes of remaining input
        if n.saturating_mul(elem_size.max(1)) > self.buf.len() - self.pos {
            return Err(NbtError::BadLength);
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String, NbtError> {
        let n = u16::from_be_bytes(self.arr()?) as usize;
        let b = self.take(n)?;
        // Accept modified UTF-8 leniently: fall back to lossy decoding.
        match std::str::from_utf8(b) {
            Ok(s) => Ok(s.to_owned()),
            Err(_) => Ok(String::from_utf8_lossy(b).into_owned()),
        }
    }
    fn payload(&mut self, id: u8, depth: usize) -> Result<Tag, NbtError> {
        if depth > MAX_DEPTH {
            return Err(NbtError::TooDeep);
        }
        Ok(match id {
            1 => Tag::Byte(self.u8()? as i8),
            2 => Tag::Short(i16::from_be_bytes(self.arr()?)),
            3 => Tag::Int(i32::from_be_bytes(self.arr()?)),
            4 => Tag::Long(i64::from_be_bytes(self.arr()?)),
            5 => Tag::Float(f32::from_be_bytes(self.arr()?)),
            6 => Tag::Double(f64::from_be_bytes(self.arr()?)),
            7 => {
                let n = self.len(1)?;
                Tag::ByteArray(self.take(n)?.iter().map(|b| *b as i8).collect())
            }
            8 => Tag::String(self.string()?),
            9 => {
                let elem = self.u8()?;
                let min = match elem {
                    0 => 0,
                    1 => 1,
                    2 => 2,
                    3 | 5 => 4,
                    4 | 6 => 8,
                    7 | 9 | 11 | 12 => 4,
                    8 => 2,
                    10 => 1,
                    other => return Err(NbtError::BadTag(other)),
                };
                let n = self.len(min)?;
                if elem == 0 && n > 0 {
                    return Err(NbtError::BadList);
                }
                let mut v = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    v.push(self.payload(elem, depth + 1)?);
                }
                Tag::List(v)
            }
            10 => {
                let mut v = Vec::new();
                loop {
                    let t = self.u8()?;
                    if t == 0 {
                        break;
                    }
                    let k = self.string()?;
                    v.push((k, self.payload(t, depth + 1)?));
                }
                Tag::Compound(v)
            }
            11 => {
                let n = self.len(4)?;
                Tag::IntArray(self.take(n * 4)?.chunks_exact(4).map(|c| i32::from_be_bytes(c.try_into().unwrap())).collect())
            }
            12 => {
                let n = self.len(8)?;
                Tag::LongArray(self.take(n * 8)?.chunks_exact(8).map(|c| i64::from_be_bytes(c.try_into().unwrap())).collect())
            }
            other => return Err(NbtError::BadTag(other)),
        })
    }
}

/// Convenience builder: `compound![("a", Tag::Int(1))]`.
#[macro_export]
macro_rules! compound {
    ($(($k:expr, $v:expr)),* $(,)?) => {
        $crate::nbt::Tag::Compound(vec![$(($k.to_string(), $v)),*])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let t = compound![
            ("b", Tag::Byte(-1)),
            ("s", Tag::String("hello ✓".into())),
            ("l", Tag::List(vec![Tag::Int(1), Tag::Int(2)])),
            ("la", Tag::LongArray(vec![i64::MIN, 0, i64::MAX])),
            ("c", compound![("d", Tag::Double(1.5))]),
            ("e", Tag::List(vec![])),
        ];
        let b = t.to_root_bytes("root");
        let (name, back) = Tag::from_root_bytes(&b).unwrap();
        assert_eq!(name, "root");
        assert_eq!(back, t);
    }

    #[test]
    fn hostile_input() {
        // huge list length
        assert!(Tag::from_root_bytes(&[10, 0, 0, 9, 0, 1, b'x', 3, 0x7F, 0xFF, 0xFF, 0xFF]).is_err());
        // deep nesting of lists
        let mut b = vec![9, 0, 0];
        for _ in 0..2000 {
            b.extend_from_slice(&[9, 0, 0, 0, 1]);
        }
        assert!(Tag::from_root_bytes(&b).is_err());
        // garbage never panics
        for i in 0..=255u8 {
            let _ = Tag::from_root_bytes(&[i, 0, 0, i, i, i, i, i]);
        }
    }
}
