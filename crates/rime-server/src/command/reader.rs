//! Brigadier's `StringReader` and SNBT (`TagParser`).

use crate::nbt::Tag;
use serde_json::{json, Value};

/// A failed parse: translatable message and the cursor where it happened.
#[derive(Clone, Debug)]
pub struct ParseErr {
    pub msg: Value,
    pub at: usize,
}

pub fn tr(key: &str, with: Vec<Value>) -> Value {
    if with.is_empty() {
        json!({ "translate": key })
    } else {
        json!({ "translate": key, "with": with })
    }
}

#[derive(Clone)]
pub struct Reader<'a> {
    pub s: &'a str,
    pub pos: usize,
}

pub type R<T> = Result<T, ParseErr>;

fn allowed_unquoted(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+')
}

impl<'a> Reader<'a> {
    pub fn new(s: &'a str) -> Self {
        Self { s, pos: 0 }
    }
    pub fn err(&self, key: &str, with: Vec<Value>) -> ParseErr {
        ParseErr { msg: tr(key, with), at: self.pos }
    }
    pub fn err_at(&self, at: usize, key: &str, with: Vec<Value>) -> ParseErr {
        ParseErr { msg: tr(key, with), at }
    }
    pub fn can_read(&self) -> bool {
        self.pos < self.s.len()
    }
    pub fn rest(&self) -> &'a str {
        &self.s[self.pos..]
    }
    pub fn peek(&self) -> char {
        self.rest().chars().next().unwrap_or('\0')
    }
    pub fn peek_at(&self, n: usize) -> char {
        self.rest().chars().nth(n).unwrap_or('\0')
    }
    pub fn skip(&mut self) {
        if let Some(c) = self.rest().chars().next() {
            self.pos += c.len_utf8();
        }
    }
    pub fn read(&mut self) -> char {
        let c = self.peek();
        self.skip();
        c
    }
    pub fn skip_ws(&mut self) {
        while self.can_read() && self.peek().is_whitespace() {
            self.skip();
        }
    }
    pub fn expect(&mut self, c: char) -> R<()> {
        if self.peek() != c || !self.can_read() {
            return Err(self.err("parsing.expected", vec![json!(c.to_string())]));
        }
        self.skip();
        Ok(())
    }
    /// Characters up to the next space.
    pub fn read_word(&mut self) -> &'a str {
        let start = self.pos;
        while self.can_read() && self.peek() != ' ' {
            self.skip();
        }
        &self.s[start..self.pos]
    }
    pub fn read_unquoted(&mut self) -> &'a str {
        let start = self.pos;
        while self.can_read() && allowed_unquoted(self.peek()) {
            self.skip();
        }
        &self.s[start..self.pos]
    }
    pub fn read_quoted(&mut self) -> R<String> {
        let q = self.peek();
        if q != '"' && q != '\'' {
            return Err(self.err("parsing.quote.expected.start", vec![]));
        }
        self.skip();
        let mut out = String::new();
        let mut esc = false;
        while self.can_read() {
            let c = self.read();
            if esc {
                if c == q || c == '\\' {
                    out.push(c);
                    esc = false;
                } else {
                    self.pos -= c.len_utf8();
                    return Err(self.err("parsing.quote.escape", vec![json!(c.to_string())]));
                }
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                return Ok(out);
            } else {
                out.push(c);
            }
        }
        Err(self.err("parsing.quote.expected.end", vec![]))
    }
    pub fn read_string(&mut self) -> R<String> {
        if matches!(self.peek(), '"' | '\'') && self.can_read() {
            self.read_quoted()
        } else {
            Ok(self.read_unquoted().to_string())
        }
    }
    fn number_chars(&mut self) -> &'a str {
        let start = self.pos;
        while self.can_read() && (self.peek().is_ascii_digit() || matches!(self.peek(), '.' | '-' | '+' | 'e' | 'E')) {
            // stop at ".." (ranges)
            if self.peek() == '.' && self.peek_at(1) == '.' {
                break;
            }
            self.skip();
        }
        &self.s[start..self.pos]
    }
    pub fn read_int(&mut self) -> R<i32> {
        let start = self.pos;
        let t = self.number_chars();
        if t.is_empty() {
            return Err(self.err("parsing.int.expected", vec![]));
        }
        t.parse::<i32>().map_err(|_| self.err_at(start, "parsing.int.invalid", vec![json!(t)]))
    }
    pub fn read_long(&mut self) -> R<i64> {
        let start = self.pos;
        let t = self.number_chars();
        if t.is_empty() {
            return Err(self.err("parsing.long.expected", vec![]));
        }
        t.parse::<i64>().map_err(|_| self.err_at(start, "parsing.long.invalid", vec![json!(t)]))
    }
    pub fn read_double(&mut self) -> R<f64> {
        let start = self.pos;
        let t = self.number_chars();
        if t.is_empty() {
            return Err(self.err("parsing.double.expected", vec![]));
        }
        t.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| self.err_at(start, "parsing.double.invalid", vec![json!(t)]))
    }
    pub fn read_bool(&mut self) -> R<bool> {
        let start = self.pos;
        match self.read_unquoted() {
            "true" => Ok(true),
            "false" => Ok(false),
            "" => Err(self.err("parsing.bool.expected", vec![])),
            v => Err(self.err_at(start, "parsing.bool.invalid", vec![json!(v)])),
        }
    }
    /// `namespace:path` (namespace optional).
    pub fn read_id(&mut self) -> R<String> {
        let start = self.pos;
        while self.can_read() && (self.peek().is_ascii_lowercase() || self.peek().is_ascii_digit() || matches!(self.peek(), '_' | '-' | '.' | '/' | ':')) {
            self.skip();
        }
        let t = &self.s[start..self.pos];
        if t.is_empty() || t.matches(':').count() > 1 {
            return Err(self.err_at(start, "argument.id.invalid", vec![]));
        }
        Ok(if t.contains(':') { t.to_string() } else { format!("minecraft:{t}") })
    }
}

// ------------------------------------------------------------------ SNBT

pub fn read_snbt(r: &mut Reader) -> R<Tag> {
    snbt_value(r, 0)
}

pub fn read_compound(r: &mut Reader) -> R<Tag> {
    r.skip_ws();
    if r.peek() != '{' {
        return Err(r.err("argument.nbt.expected.compound", vec![]));
    }
    snbt_value(r, 0)
}

fn snbt_value(r: &mut Reader, depth: usize) -> R<Tag> {
    if depth > 512 {
        return Err(r.err("argument.nbt.invalid", vec![]));
    }
    r.skip_ws();
    if !r.can_read() {
        return Err(r.err("argument.nbt.expected.value", vec![]));
    }
    match r.peek() {
        '{' => {
            r.skip();
            let mut v = Vec::new();
            r.skip_ws();
            while r.can_read() && r.peek() != '}' {
                r.skip_ws();
                let k = r.read_string()?;
                if k.is_empty() {
                    return Err(r.err("argument.nbt.expected.key", vec![]));
                }
                r.skip_ws();
                r.expect(':')?;
                let val = snbt_value(r, depth + 1)?;
                v.retain(|(kk, _): &(String, Tag)| *kk != k);
                v.push((k, val));
                r.skip_ws();
                if r.peek() == ',' {
                    r.skip();
                    r.skip_ws();
                } else {
                    break;
                }
            }
            r.skip_ws();
            r.expect('}')?;
            Ok(Tag::Compound(v))
        }
        '[' => {
            r.skip();
            // typed arrays
            if matches!(r.peek(), 'B' | 'I' | 'L') && r.peek_at(1) == ';' {
                let ty = r.read();
                r.skip();
                let mut nums = Vec::new();
                r.skip_ws();
                while r.can_read() && r.peek() != ']' {
                    let t = snbt_value(r, depth + 1)?;
                    nums.push(t.as_i64().ok_or_else(|| r.err("argument.nbt.array.invalid", vec![]))?);
                    r.skip_ws();
                    if r.peek() == ',' {
                        r.skip();
                        r.skip_ws();
                    } else {
                        break;
                    }
                }
                r.expect(']')?;
                return Ok(match ty {
                    'B' => Tag::ByteArray(nums.into_iter().map(|n| n as i8).collect()),
                    'I' => Tag::IntArray(nums.into_iter().map(|n| n as i32).collect()),
                    _ => Tag::LongArray(nums),
                });
            }
            let mut v = Vec::new();
            r.skip_ws();
            while r.can_read() && r.peek() != ']' {
                v.push(snbt_value(r, depth + 1)?);
                r.skip_ws();
                if r.peek() == ',' {
                    r.skip();
                    r.skip_ws();
                } else {
                    break;
                }
            }
            r.expect(']')?;
            let homogeneous = v.windows(2).all(|w| w[0].id() == w[1].id());
            Ok(Tag::List(if homogeneous { v } else { v.into_iter().map(|t| if let Tag::Compound(_) = t { t } else { Tag::Compound(vec![(String::new(), t)]) }).collect() }))
        }
        '"' | '\'' => Ok(Tag::String(r.read_quoted()?)),
        _ => {
            let start = r.pos;
            let w = r.read_unquoted();
            if w.is_empty() {
                return Err(r.err_at(start, "argument.nbt.expected.value", vec![]));
            }
            Ok(scalar(w))
        }
    }
}

fn scalar(w: &str) -> Tag {
    let lower = w.to_ascii_lowercase();
    if lower == "true" {
        return Tag::Byte(1);
    }
    if lower == "false" {
        return Tag::Byte(0);
    }
    let (body, suf) = match lower.chars().last() {
        Some(c @ ('b' | 's' | 'l' | 'f' | 'd')) => (&w[..w.len() - 1], Some(c)),
        _ => (w, None),
    };
    let body = body.replace('_', "");
    match suf {
        Some('b') => body.parse::<i8>().map(Tag::Byte).ok(),
        Some('s') => body.parse::<i16>().map(Tag::Short).ok(),
        Some('l') => body.parse::<i64>().map(Tag::Long).ok(),
        Some('f') => body.parse::<f32>().map(Tag::Float).ok(),
        Some('d') => body.parse::<f64>().map(Tag::Double).ok(),
        _ => body.parse::<i32>().map(Tag::Int).ok().or_else(|| if body.contains('.') || body.contains('e') { body.parse::<f64>().map(Tag::Double).ok() } else { None }),
    }
    .unwrap_or_else(|| Tag::String(w.to_string()))
}

/// SNBT text of a tag (for `data get`-style output).
pub fn to_snbt(t: &Tag) -> String {
    fn q(s: &str) -> String {
        if !s.is_empty() && s.chars().all(allowed_unquoted) {
            s.to_string()
        } else {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
        }
    }
    match t {
        Tag::Byte(v) => format!("{v}b"),
        Tag::Short(v) => format!("{v}s"),
        Tag::Int(v) => format!("{v}"),
        Tag::Long(v) => format!("{v}L"),
        Tag::Float(v) => format!("{v}f"),
        Tag::Double(v) => format!("{v}d"),
        Tag::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Tag::ByteArray(v) => format!("[B;{}]", v.iter().map(|x| format!("{x}B")).collect::<Vec<_>>().join(",")),
        Tag::IntArray(v) => format!("[I;{}]", v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")),
        Tag::LongArray(v) => format!("[L;{}]", v.iter().map(|x| format!("{x}L")).collect::<Vec<_>>().join(",")),
        Tag::List(v) => format!("[{}]", v.iter().map(to_snbt).collect::<Vec<_>>().join(",")),
        Tag::Compound(v) => format!("{{{}}}", v.iter().map(|(k, x)| format!("{}:{}", q(k), to_snbt(x))).collect::<Vec<_>>().join(",")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snbt_round_trip() {
        let mut r = Reader::new("{a:1b,b:[1,2,3],\"c d\":'x\\'y',e:[I;1,2],f:1.5f,g:{h:true}}");
        let t = read_snbt(&mut r).unwrap();
        assert_eq!(t.get("a"), Some(&Tag::Byte(1)));
        assert_eq!(t.get("b"), Some(&Tag::List(vec![Tag::Int(1), Tag::Int(2), Tag::Int(3)])));
        assert_eq!(t.get("c d"), Some(&Tag::String("x'y".into())));
        assert_eq!(t.get("e"), Some(&Tag::IntArray(vec![1, 2])));
        assert_eq!(t.get("f"), Some(&Tag::Float(1.5)));
        assert_eq!(t.get("g").and_then(|g| g.get("h")), Some(&Tag::Byte(1)));
        assert_eq!(to_snbt(&Tag::Compound(vec![("a".into(), Tag::Int(1))])), "{a:1}");
    }

    #[test]
    fn reader_numbers() {
        let mut r = Reader::new("12..15");
        assert_eq!(r.read_int().unwrap(), 12);
        assert_eq!(r.rest(), "..15");
        let mut r = Reader::new("-0.5 x");
        assert_eq!(r.read_double().unwrap(), -0.5);
    }
}
