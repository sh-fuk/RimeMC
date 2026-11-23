//! Entity selectors (`EntitySelectorParser`): `@p @a @r @s @e @n`, names, UUIDs.

use super::args::parse_range;
use super::reader::{read_compound, Reader, R};
use crate::nbt::Tag;
use serde_json::json;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Name(String),
    Uuid(uuid::Uuid),
    /// nearest player
    P,
    A,
    R,
    S,
    E,
    /// nearest entity
    N,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sort {
    Arbitrary,
    Nearest,
    Furthest,
    Random,
}

pub type Range = (Option<f64>, Option<f64>);

#[derive(Clone, Debug)]
pub struct Selector {
    pub kind: Kind,
    pub limit: Option<usize>,
    pub sort: Sort,
    /// (negated, type or `#tag`, no namespace)
    pub types: Vec<(bool, String)>,
    pub names: Vec<(bool, String)>,
    pub tags: Vec<(bool, String)>,
    pub teams: Vec<(bool, String)>,
    pub gamemodes: Vec<(bool, i32)>,
    pub distance: Option<Range>,
    pub level: Option<Range>,
    pub pos: [Option<f64>; 3],
    pub delta: [Option<f64>; 3],
    pub x_rot: Option<Range>,
    pub y_rot: Option<Range>,
    pub scores: Vec<(String, Range)>,
    pub nbt: Vec<(bool, Tag)>,
}

impl Selector {
    fn new(kind: Kind) -> Self {
        Selector {
            kind,
            limit: None,
            sort: Sort::Arbitrary,
            types: vec![],
            names: vec![],
            tags: vec![],
            teams: vec![],
            gamemodes: vec![],
            distance: None,
            level: None,
            pos: [None; 3],
            delta: [None; 3],
            x_rot: None,
            y_rot: None,
            scores: vec![],
            nbt: vec![],
        }
    }

    pub fn single(&self) -> bool {
        match self.kind {
            Kind::Name(_) | Kind::Uuid(_) | Kind::S => true,
            _ => self.limit == Some(1),
        }
    }

    pub fn players_only(&self) -> bool {
        match self.kind {
            Kind::Name(_) | Kind::P | Kind::A | Kind::R => true,
            Kind::Uuid(_) | Kind::S | Kind::E | Kind::N => self.types.iter().any(|(neg, t)| !neg && t == "player"),
        }
    }

    /// Does a position / volume / distance option restrict the search to the source's level?
    pub fn world_limited(&self) -> bool {
        self.distance.is_some() || self.pos.iter().chain(&self.delta).any(|v| v.is_some())
    }
}

pub fn parse(r: &mut Reader) -> R<Selector> {
    let start = r.pos;
    if r.peek() != '@' {
        let w = r.read_word();
        if w.is_empty() {
            return Err(r.err_at(start, "argument.entity.invalid", vec![]));
        }
        if let Ok(u) = uuid::Uuid::parse_str(w) {
            if w.contains('-') {
                return Ok(Selector::new(Kind::Uuid(u)));
            }
        }
        if w.len() > 16 {
            return Err(r.err_at(start, "argument.entity.invalid", vec![]));
        }
        return Ok(Selector::new(Kind::Name(w.to_string())));
    }
    r.skip();
    let c = r.read();
    let mut s = match c {
        'p' => {
            let mut s = Selector::new(Kind::P);
            s.limit = Some(1);
            s.sort = Sort::Nearest;
            s
        }
        'a' => Selector::new(Kind::A),
        'r' => {
            let mut s = Selector::new(Kind::R);
            s.limit = Some(1);
            s.sort = Sort::Random;
            s
        }
        's' => Selector::new(Kind::S),
        'e' => Selector::new(Kind::E),
        'n' => {
            let mut s = Selector::new(Kind::N);
            s.limit = Some(1);
            s.sort = Sort::Nearest;
            s
        }
        _ => return Err(r.err_at(start, "argument.entity.selector.unknown", vec![json!(format!("@{c}"))])),
    };
    if r.peek() == '[' && r.can_read() {
        r.skip();
        r.skip_ws();
        while r.can_read() && r.peek() != ']' {
            r.skip_ws();
            let k0 = r.pos;
            let key = r.read_unquoted().to_string();
            r.skip_ws();
            if r.peek() != '=' {
                return Err(r.err("argument.entity.options.valueless", vec![json!(key)]));
            }
            r.skip();
            r.skip_ws();
            let neg = |r: &mut Reader| {
                let n = r.peek() == '!';
                if n {
                    r.skip();
                }
                n
            };
            match key.as_str() {
                "limit" => {
                    let v = r.read_int()?;
                    if v < 1 {
                        return Err(r.err_at(k0, "argument.entity.options.limit.toosmall", vec![]));
                    }
                    s.limit = Some(v as usize);
                }
                "sort" => {
                    s.sort = match r.read_unquoted() {
                        "nearest" => Sort::Nearest,
                        "furthest" => Sort::Furthest,
                        "random" => Sort::Random,
                        "arbitrary" => Sort::Arbitrary,
                        w => return Err(r.err_at(k0, "argument.entity.options.sort.irreversible", vec![json!(w)])),
                    }
                }
                "type" => {
                    let n = neg(r);
                    let tag = r.peek() == '#';
                    if tag {
                        r.skip();
                    }
                    let id = r.read_id()?;
                    let short = id.trim_start_matches("minecraft:").to_string();
                    if !tag && crate::registry::Registries::get().id("minecraft:entity_type", &id).is_none() {
                        return Err(r.err_at(k0, "argument.entity.options.type.invalid", vec![json!(id)]));
                    }
                    s.types.push((n, if tag { format!("#{short}") } else { short }));
                }
                "name" => {
                    let n = neg(r);
                    s.names.push((n, r.read_string()?));
                }
                "tag" => {
                    let n = neg(r);
                    s.tags.push((n, r.read_unquoted().to_string()));
                }
                "team" => {
                    let n = neg(r);
                    s.teams.push((n, r.read_unquoted().to_string()));
                }
                "gamemode" => {
                    let n = neg(r);
                    let w = r.read_unquoted();
                    let g = ["survival", "creative", "adventure", "spectator"].iter().position(|m| *m == w).ok_or_else(|| r.err_at(k0, "argument.entity.options.mode.invalid", vec![json!(w)]))?;
                    s.gamemodes.push((n, g as i32));
                }
                "distance" => {
                    let rg = parse_range(r)?;
                    if rg.0.is_some_and(|v| v < 0.0) {
                        return Err(r.err_at(k0, "argument.entity.options.distance.negative", vec![]));
                    }
                    s.distance = Some(rg);
                }
                "level" => s.level = Some(parse_range(r)?),
                "x" | "y" | "z" => {
                    let i = (key.as_bytes()[0] - b'x') as usize;
                    s.pos[i] = Some(r.read_double()?);
                }
                "dx" | "dy" | "dz" => {
                    let i = (key.as_bytes()[1] - b'x') as usize;
                    s.delta[i] = Some(r.read_double()?);
                }
                "x_rotation" => s.x_rot = Some(parse_range(r)?),
                "y_rotation" => s.y_rot = Some(parse_range(r)?),
                "scores" => {
                    r.expect('{')?;
                    r.skip_ws();
                    while r.can_read() && r.peek() != '}' {
                        let obj = r.read_unquoted().to_string();
                        r.skip_ws();
                        r.expect('=')?;
                        r.skip_ws();
                        s.scores.push((obj, parse_range(r)?));
                        r.skip_ws();
                        if r.peek() == ',' {
                            r.skip();
                            r.skip_ws();
                        }
                    }
                    r.expect('}')?;
                }
                "nbt" => {
                    let n = neg(r);
                    s.nbt.push((n, read_compound(r)?));
                }
                "advancements" | "predicate" => {
                    // accepted, not evaluated
                    let mut depth = 0;
                    while r.can_read() {
                        match r.peek() {
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            ',' | ']' if depth == 0 => break,
                            _ => {}
                        }
                        r.skip();
                    }
                }
                _ => return Err(r.err_at(k0, "argument.entity.options.unknown", vec![json!(key)])),
            }
            r.skip_ws();
            if r.peek() == ',' {
                r.skip();
                r.skip_ws();
            } else {
                break;
            }
        }
        r.expect(']')?;
    }
    if matches!(s.kind, Kind::P | Kind::A | Kind::R) && !s.types.iter().any(|t| !t.0 && t.1 == "player") {
        s.types.push((false, "player".into()));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_parse() {
        let mut r = Reader::new("@e[type=!zombie,limit=2,distance=..10,tag=a,scores={x=1..}]");
        let s = parse(&mut r).unwrap();
        assert_eq!(s.kind, Kind::E);
        assert_eq!(s.limit, Some(2));
        assert_eq!(s.types, vec![(true, "zombie".to_string())]);
        assert_eq!(s.distance, Some((None, Some(10.0))));
        assert_eq!(s.scores[0].1, (Some(1.0), None));
        assert!(!r.can_read());
        let mut r = Reader::new("Steve rest");
        assert_eq!(parse(&mut r).unwrap().kind, Kind::Name("Steve".into()));
        assert!(parse(&mut Reader::new("@q")).is_err());
        assert!(parse(&mut Reader::new("@a")).unwrap().players_only());
    }
}
