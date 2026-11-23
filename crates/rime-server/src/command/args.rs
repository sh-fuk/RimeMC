//! Argument types: their `commands` packet encoding and their parsers.

use super::reader::{read_compound, read_snbt, ParseErr, Reader, R};
use super::selector::{self, Selector};
use crate::data::Items;
use crate::nbt::Tag;
use crate::net::codec::Out;
use crate::registry::{BlockRegistry, BlockState, Registries};
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub enum ArgType {
    Bool,
    Int(i32, i32),
    Float(f32, f32),
    Double(f64, f64),
    Long,
    Word,
    Phrase,
    Greedy,
    Entity { single: bool, players: bool },
    Profile,
    BlockPos,
    ColumnPos,
    Vec3,
    Vec2,
    Rotation,
    Angle,
    BlockState,
    BlockPredicate,
    Item,
    ItemPredicate,
    Color,
    HexColor,
    Component,
    Message,
    NbtCompound,
    NbtTag,
    NbtPath,
    Objective,
    Criteria,
    Operation,
    Particle,
    ScoreSlot,
    ScoreHolder { multiple: bool },
    Swizzle,
    Team,
    ItemSlot,
    Id,
    Anchor,
    IntRange,
    FloatRange,
    Dimension,
    Gamemode,
    Time(i32),
    Resource(&'static str),
    ResourceOrTag(&'static str),
    Heightmap,
    Uuid,
    TemplateMirror,
    TemplateRotation,
}

/// A parsed argument value.
#[derive(Clone, Debug)]
pub enum Arg {
    Bool(bool),
    Int(i32),
    Long(i64),
    Double(f64),
    Str(String),
    Sel(Selector),
    Coords(Coords),
    Block(BlockInput),
    Item(ItemInput),
    Text(Value),
    Nbt(Tag),
    Range(Option<f64>, Option<f64>),
    Swizzle([bool; 3]),
    /// entity anchor: eyes?
    Anchor(bool),
    Uuid(uuid::Uuid),
    /// a literal the parse went through
    Lit,
}

#[derive(Clone, Copy, Debug)]
pub struct Coord {
    pub rel: bool,
    pub v: f64,
}

#[derive(Clone, Copy, Debug)]
pub enum Coords {
    World([Coord; 3]),
    /// `^left ^up ^forwards`
    Local([f64; 3]),
}

#[derive(Clone, Debug)]
pub struct BlockInput {
    pub state: BlockState,
    /// explicitly given properties (predicates test only these)
    pub props: Vec<(String, String)>,
    pub tag: Option<String>,
    pub nbt: Option<Tag>,
}

impl BlockInput {
    pub fn test(&self, b: BlockState, data: &crate::data::GameData) -> bool {
        let name_ok = match &self.tag {
            Some(t) => data.block_in(t, b.name()),
            None => b.name() == self.state.name(),
        };
        name_ok && self.props.iter().all(|(k, v)| b.prop(k) == Some(v.as_str()))
    }
}

#[derive(Clone, Debug)]
pub struct ItemInput {
    /// None for `*`
    pub item: Option<i32>,
    pub tag: Option<String>,
    pub comp: u32,
}

impl ItemInput {
    pub fn test(&self, s: &crate::data::Stack, data: &crate::data::GameData) -> bool {
        match (&self.tag, self.item) {
            (Some(t), _) => data.item_in(t, s.item),
            (None, Some(i)) => s.item == i,
            (None, None) => true,
        }
    }
}

fn reg_id(name: &str) -> i32 {
    Registries::get().id("minecraft:command_argument_type", name).unwrap_or(5)
}

impl ArgType {
    pub fn write(&self, b: &mut Vec<u8>) {
        use ArgType::*;
        let simple = |b: &mut Vec<u8>, n: &str| {
            b.vi(reg_id(n));
        };
        match self {
            Bool => simple(b, "brigadier:bool"),
            Int(lo, hi) => {
                b.vi(reg_id("brigadier:integer"));
                let f = (*lo != i32::MIN) as u8 | (((*hi != i32::MAX) as u8) << 1);
                b.u8(f);
                if *lo != i32::MIN {
                    b.i32(*lo);
                }
                if *hi != i32::MAX {
                    b.i32(*hi);
                }
            }
            Float(lo, hi) => {
                b.vi(reg_id("brigadier:float"));
                let f = (*lo != f32::MIN) as u8 | (((*hi != f32::MAX) as u8) << 1);
                b.u8(f);
                if *lo != f32::MIN {
                    b.f32(*lo);
                }
                if *hi != f32::MAX {
                    b.f32(*hi);
                }
            }
            Double(lo, hi) => {
                b.vi(reg_id("brigadier:double"));
                let f = (*lo != f64::MIN) as u8 | (((*hi != f64::MAX) as u8) << 1);
                b.u8(f);
                if *lo != f64::MIN {
                    b.f64(*lo);
                }
                if *hi != f64::MAX {
                    b.f64(*hi);
                }
            }
            Long => {
                b.vi(reg_id("brigadier:long")).u8(0);
            }
            Word => {
                b.vi(reg_id("brigadier:string")).vi(0);
            }
            Phrase => {
                b.vi(reg_id("brigadier:string")).vi(1);
            }
            Greedy => {
                b.vi(reg_id("brigadier:string")).vi(2);
            }
            Entity { single, players } => {
                b.vi(reg_id("minecraft:entity")).u8(*single as u8 | ((*players as u8) << 1));
            }
            Profile => simple(b, "minecraft:game_profile"),
            BlockPos => simple(b, "minecraft:block_pos"),
            ColumnPos => simple(b, "minecraft:column_pos"),
            Vec3 => simple(b, "minecraft:vec3"),
            Vec2 => simple(b, "minecraft:vec2"),
            Rotation => simple(b, "minecraft:rotation"),
            Angle => simple(b, "minecraft:angle"),
            BlockState => simple(b, "minecraft:block_state"),
            BlockPredicate => simple(b, "minecraft:block_predicate"),
            Item => simple(b, "minecraft:item_stack"),
            ItemPredicate => simple(b, "minecraft:item_predicate"),
            Color => simple(b, "minecraft:color"),
            HexColor => simple(b, "minecraft:hex_color"),
            Component => simple(b, "minecraft:component"),
            Message => simple(b, "minecraft:message"),
            NbtCompound => simple(b, "minecraft:nbt_compound_tag"),
            NbtTag => simple(b, "minecraft:nbt_tag"),
            NbtPath => simple(b, "minecraft:nbt_path"),
            Objective => simple(b, "minecraft:objective"),
            Criteria => simple(b, "minecraft:objective_criteria"),
            Operation => simple(b, "minecraft:operation"),
            Particle => simple(b, "minecraft:particle"),
            ScoreSlot => simple(b, "minecraft:scoreboard_slot"),
            ScoreHolder { multiple } => {
                b.vi(reg_id("minecraft:score_holder")).u8(*multiple as u8);
            }
            Swizzle => simple(b, "minecraft:swizzle"),
            Team => simple(b, "minecraft:team"),
            ItemSlot => simple(b, "minecraft:item_slot"),
            Id => simple(b, "minecraft:resource_location"),
            Anchor => simple(b, "minecraft:entity_anchor"),
            IntRange => simple(b, "minecraft:int_range"),
            FloatRange => simple(b, "minecraft:float_range"),
            Dimension => simple(b, "minecraft:dimension"),
            Gamemode => simple(b, "minecraft:gamemode"),
            Time(min) => {
                b.vi(reg_id("minecraft:time")).i32(*min);
            }
            Resource(r) => {
                b.vi(reg_id("minecraft:resource")).str(r);
            }
            ResourceOrTag(r) => {
                b.vi(reg_id("minecraft:resource_or_tag")).str(r);
            }
            Heightmap => simple(b, "minecraft:heightmap"),
            Uuid => simple(b, "minecraft:uuid"),
            TemplateMirror => simple(b, "minecraft:template_mirror"),
            TemplateRotation => simple(b, "minecraft:template_rotation"),
        }
    }

    /// Suggestion provider sent with the node (`minecraft:ask_server` …).
    pub fn suggestions(&self) -> Option<&'static str> {
        match self {
            ArgType::Resource("minecraft:entity_type") => Some("minecraft:summonable_entities"),
            ArgType::Id => Some("minecraft:ask_server"),
            _ => None,
        }
    }

    pub fn parse(&self, r: &mut Reader) -> R<Arg> {
        use ArgType::*;
        let start = r.pos;
        Ok(match self {
            Bool => Arg::Bool(r.read_bool()?),
            Int(lo, hi) => {
                let v = r.read_int()?;
                if v < *lo {
                    return Err(r.err_at(start, "argument.integer.low", vec![json!(lo), json!(v)]));
                }
                if v > *hi {
                    return Err(r.err_at(start, "argument.integer.big", vec![json!(hi), json!(v)]));
                }
                Arg::Int(v)
            }
            Float(lo, hi) => {
                let v = r.read_double()?;
                if v < *lo as f64 {
                    return Err(r.err_at(start, "argument.float.low", vec![json!(lo), json!(v)]));
                }
                if v > *hi as f64 {
                    return Err(r.err_at(start, "argument.float.big", vec![json!(hi), json!(v)]));
                }
                Arg::Double(v)
            }
            Double(lo, hi) => {
                let v = r.read_double()?;
                if v < *lo {
                    return Err(r.err_at(start, "argument.double.low", vec![json!(lo), json!(v)]));
                }
                if v > *hi {
                    return Err(r.err_at(start, "argument.double.big", vec![json!(hi), json!(v)]));
                }
                Arg::Double(v)
            }
            Long => Arg::Long(r.read_long()?),
            Word => {
                let w = r.read_unquoted();
                if w.is_empty() {
                    return Err(r.err("argument.id.invalid", vec![]));
                }
                Arg::Str(w.to_string())
            }
            Phrase => Arg::Str(r.read_string()?),
            Greedy | Message => {
                let s = r.rest().to_string();
                r.pos = r.s.len();
                Arg::Str(s)
            }
            Entity { single, players } => {
                let sel = selector::parse(r)?;
                if *single && !sel.single() {
                    return Err(r.err_at(start, if sel.players_only() { "argument.player.toomany" } else { "argument.entity.toomany" }, vec![]));
                }
                // `@s` passes; a non-player source fails when the command runs
                if *players && !sel.players_only() && !matches!(sel.kind, selector::Kind::S) {
                    return Err(r.err_at(start, "argument.player.entities", vec![]));
                }
                Arg::Sel(sel)
            }
            Profile => Arg::Sel(selector::parse(r)?),
            ScoreHolder { .. } => {
                if r.peek() == '*' {
                    r.skip();
                    Arg::Str("*".into())
                } else if r.peek() == '@' {
                    Arg::Sel(selector::parse(r)?)
                } else {
                    let w = r.read_word();
                    if w.is_empty() {
                        return Err(r.err("argument.scoreHolder.empty", vec![]));
                    }
                    Arg::Str(w.to_string())
                }
            }
            BlockPos => Arg::Coords(parse_coords(r, 3, false, true)?),
            ColumnPos => Arg::Coords(parse_coords(r, 2, false, true)?),
            Vec3 => Arg::Coords(parse_coords(r, 3, true, false)?),
            Vec2 => Arg::Coords(parse_coords(r, 2, true, false)?),
            Rotation => Arg::Coords(parse_coords(r, 2, false, false)?),
            Angle => {
                let rel = r.peek() == '~';
                if rel {
                    r.skip();
                }
                let v = if r.can_read() && r.peek() != ' ' { r.read_double()? } else { 0.0 };
                Arg::Coords(Coords::World([Coord { rel, v }, Coord { rel: false, v: 0.0 }, Coord { rel: false, v: 0.0 }]))
            }
            BlockState => Arg::Block(parse_block(r, false)?),
            BlockPredicate => Arg::Block(parse_block(r, true)?),
            Item => Arg::Item(parse_item(r, false)?),
            ItemPredicate => Arg::Item(parse_item(r, true)?),
            Color => {
                let w = r.read_unquoted();
                if !COLORS.contains(&w) && w != "reset" {
                    return Err(r.err_at(start, "argument.color.invalid", vec![json!(w)]));
                }
                Arg::Str(w.to_string())
            }
            HexColor => {
                let w = r.read_unquoted();
                let v = i32::from_str_radix(w, 16).ok().filter(|_| w.len() == 6).ok_or_else(|| r.err_at(start, "argument.hexcolor.invalid", vec![json!(w)]))?;
                Arg::Int(v)
            }
            Component => Arg::Text(parse_component(r)?),
            NbtCompound => Arg::Nbt(read_compound(r)?),
            NbtTag => Arg::Nbt(read_snbt(r)?),
            NbtPath => Arg::Str(r.read_word().to_string()),
            Objective | Team => {
                let w = r.read_unquoted();
                if w.is_empty() {
                    return Err(r.err("argument.id.invalid", vec![]));
                }
                Arg::Str(w.to_string())
            }
            Criteria => {
                let w = r.read_word();
                Arg::Str(w.to_string())
            }
            Operation => {
                let w = r.read_word();
                if !["=", "+=", "-=", "*=", "/=", "%=", "<", ">", "><"].contains(&w) {
                    return Err(r.err_at(start, "arguments.operation.invalid", vec![]));
                }
                Arg::Str(w.to_string())
            }
            Particle => {
                let id = r.read_id()?;
                if Registries::get().id("minecraft:particle_type", &id).is_none() {
                    return Err(r.err_at(start, "particle.notFound", vec![json!(id)]));
                }
                let mut s = id;
                if r.peek() == '{' {
                    let t = read_compound(r)?;
                    s.push_str(&super::reader::to_snbt(&t));
                }
                Arg::Str(s)
            }
            ScoreSlot => {
                let w = r.read_unquoted();
                if score_slot(w).is_none() {
                    return Err(r.err_at(start, "argument.scoreboardDisplaySlot.invalid", vec![json!(w)]));
                }
                Arg::Str(w.to_string())
            }
            Swizzle => {
                let w = r.read_unquoted();
                let mut s = [false; 3];
                for c in w.chars() {
                    let i = match c {
                        'x' => 0,
                        'y' => 1,
                        'z' => 2,
                        _ => return Err(r.err_at(start, "arguments.swizzle.invalid", vec![])),
                    };
                    if s[i] {
                        return Err(r.err_at(start, "arguments.swizzle.invalid", vec![]));
                    }
                    s[i] = true;
                }
                if w.is_empty() {
                    return Err(r.err_at(start, "arguments.swizzle.invalid", vec![]));
                }
                Arg::Swizzle(s)
            }
            ItemSlot => {
                let w = r.read_unquoted();
                let v = item_slot(w).ok_or_else(|| r.err_at(start, "slot.unknown", vec![json!(w)]))?;
                Arg::Int(v)
            }
            Id => Arg::Str(r.read_id()?),
            Anchor => match r.read_unquoted() {
                "eyes" => Arg::Anchor(true),
                "feet" => Arg::Anchor(false),
                w => return Err(r.err_at(start, "argument.anchor.invalid", vec![json!(w)])),
            },
            IntRange | FloatRange => {
                let (lo, hi) = parse_range(r)?;
                if matches!(self, IntRange) && [lo, hi].iter().flatten().any(|v| v.fract() != 0.0) {
                    return Err(r.err_at(start, "parsing.int.invalid", vec![]));
                }
                Arg::Range(lo, hi)
            }
            Dimension => {
                let id = r.read_id()?;
                if crate::world::storage::Dim::from_key(&id).is_none() {
                    return Err(r.err_at(start, "argument.dimension.invalid", vec![json!(id)]));
                }
                Arg::Str(id)
            }
            Gamemode => {
                let w = r.read_unquoted();
                let g = ["survival", "creative", "adventure", "spectator"].iter().position(|m| *m == w).ok_or_else(|| r.err_at(start, "argument.gamemode.invalid", vec![json!(w)]))?;
                Arg::Int(g as i32)
            }
            Time(min) => {
                let v = r.read_double()?;
                let mul = match r.peek() {
                    'd' => {
                        r.skip();
                        24000.0
                    }
                    's' => {
                        r.skip();
                        20.0
                    }
                    't' => {
                        r.skip();
                        1.0
                    }
                    _ => 1.0,
                };
                let t = (v * mul).round() as i32;
                if t < *min {
                    return Err(r.err_at(start, "argument.time.tick_count_too_low", vec![json!(min), json!(t)]));
                }
                Arg::Int(t)
            }
            Resource(reg) => {
                let id = r.read_id()?;
                if !resource_exists(reg, &id) {
                    return Err(r.err_at(start, "argument.resource.not_found", vec![json!(id), json!(reg)]));
                }
                Arg::Str(id)
            }
            ResourceOrTag(reg) => {
                if r.peek() == '#' {
                    r.skip();
                    let id = r.read_id()?;
                    Arg::Str(format!("#{id}"))
                } else {
                    let id = r.read_id()?;
                    if !resource_exists(reg, &id) {
                        return Err(r.err_at(start, "argument.resource.not_found", vec![json!(id), json!(reg)]));
                    }
                    Arg::Str(id)
                }
            }
            Heightmap => {
                let w = r.read_unquoted();
                if !["world_surface", "motion_blocking", "motion_blocking_no_leaves", "ocean_floor"].contains(&w) {
                    return Err(r.err_at(start, "argument.enum.invalid", vec![json!(w)]));
                }
                Arg::Str(w.to_string())
            }
            Uuid => {
                let w = r.read_word();
                Arg::Uuid(uuid::Uuid::parse_str(w).map_err(|_| r.err_at(start, "argument.uuid.invalid", vec![]))?)
            }
            TemplateMirror | TemplateRotation => Arg::Str(r.read_unquoted().to_string()),
        })
    }
}

pub const COLORS: [&str; 16] = ["black", "dark_blue", "dark_green", "dark_aqua", "dark_red", "dark_purple", "gold", "gray", "dark_gray", "blue", "green", "aqua", "red", "light_purple", "yellow", "white"];

pub fn score_slot(w: &str) -> Option<i32> {
    match w {
        "list" => Some(0),
        "sidebar" => Some(1),
        "below_name" | "belowName" => Some(2),
        _ => {
            let c = w.strip_prefix("sidebar.team.")?;
            COLORS.iter().position(|x| *x == c).map(|i| 3 + i as i32)
        }
    }
}

/// `SlotRange` names → container slot index (player inventory numbering).
pub fn item_slot(w: &str) -> Option<i32> {
    let num = |p: &str, base: i32, n: i32| w.strip_prefix(p).and_then(|x| x.parse::<i32>().ok()).filter(|i| (0..n).contains(i)).map(|i| base + i);
    match w {
        "weapon" | "weapon.mainhand" => Some(98),
        "weapon.offhand" => Some(99),
        "armor.head" => Some(103),
        "armor.chest" => Some(102),
        "armor.legs" => Some(101),
        "armor.feet" => Some(100),
        "armor.body" => Some(105),
        "saddle" => Some(400),
        "player.cursor" => Some(499),
        _ => num("container.", 0, 54).or_else(|| num("hotbar.", 0, 9)).or_else(|| num("inventory.", 9, 27)).or_else(|| num("enderchest.", 200, 27)).or_else(|| num("horse.", 500, 15)),
    }
}

fn resource_exists(reg: &str, id: &str) -> bool {
    let short = id.trim_start_matches("minecraft:");
    match reg {
        "minecraft:mob_effect" => crate::game::combat::effects::Effect::from_name(short).is_some(),
        "minecraft:enchantment" => crate::game::combat::enchant::Enchantments::get().by(short).is_some(),
        "minecraft:entity_type" => Registries::get().id(reg, id).is_some(),
        "minecraft:attribute" => crate::game::combat::attr::Attr::from_name(short).is_some(),
        "minecraft:damage_type" => crate::game::combat::damage::DamageTypes::get().types.contains_key(short),
        _ => Registries::get().id(reg, id).is_some() || crate::components::data_reg_id(reg, short).is_some(),
    }
}

/// `a..b`, `a..`, `..b`, `a`
pub fn parse_range(r: &mut Reader) -> R<(Option<f64>, Option<f64>)> {
    let start = r.pos;
    let num = |r: &mut Reader| -> R<Option<f64>> {
        if r.can_read() && (r.peek().is_ascii_digit() || r.peek() == '-' || (r.peek() == '.' && r.peek_at(1) != '.')) {
            Ok(Some(r.read_double()?))
        } else {
            Ok(None)
        }
    };
    let lo = num(r)?;
    if r.rest().starts_with("..") {
        r.pos += 2;
        let hi = num(r)?;
        if lo.is_none() && hi.is_none() {
            return Err(r.err_at(start, "argument.range.empty", vec![]));
        }
        if let (Some(a), Some(b)) = (lo, hi) {
            if a > b {
                return Err(r.err_at(start, "argument.range.swapped", vec![]));
            }
        }
        Ok((lo, hi))
    } else {
        let Some(v) = lo else { return Err(r.err_at(start, "argument.range.empty", vec![])) };
        Ok((Some(v), Some(v)))
    }
}

pub fn in_range(r: (Option<f64>, Option<f64>), v: f64) -> bool {
    r.0.is_none_or(|a| v >= a) && r.1.is_none_or(|b| v <= b)
}

fn parse_coords(r: &mut Reader, n: usize, center: bool, int: bool) -> R<Coords> {
    let start = r.pos;
    if r.peek() == '^' {
        if n != 3 {
            return Err(r.err("argument.pos.mixed", vec![]));
        }
        let mut v = [0.0; 3];
        for (i, slot) in v.iter_mut().enumerate() {
            if i > 0 {
                if r.peek() != ' ' {
                    return Err(r.err_at(start, "argument.pos3d.incomplete", vec![]));
                }
                r.skip();
            }
            if r.peek() != '^' {
                return Err(r.err("argument.pos.mixed", vec![]));
            }
            r.skip();
            if r.can_read() && r.peek() != ' ' {
                *slot = r.read_double()?;
            }
        }
        return Ok(Coords::Local(v));
    }
    let mut c = [Coord { rel: false, v: 0.0 }; 3];
    for i in 0..n {
        if i > 0 {
            if r.peek() != ' ' || !r.can_read() {
                return Err(r.err_at(start, if n == 3 { "argument.pos3d.incomplete" } else { "argument.pos2d.incomplete" }, vec![]));
            }
            r.skip();
        }
        if r.peek() == '^' {
            return Err(r.err("argument.pos.mixed", vec![]));
        }
        let rel = r.peek() == '~' && r.can_read();
        if rel {
            r.skip();
            let v = if r.can_read() && r.peek() != ' ' { r.read_double()? } else { 0.0 };
            c[i] = Coord { rel, v };
        } else {
            if !r.can_read() || r.peek() == ' ' {
                return Err(r.err("argument.pos.missing.double", vec![]));
            }
            let s0 = r.pos;
            let v = if int { r.read_int()? as f64 } else { r.read_double()? };
            let text = &r.s[s0..r.pos];
            // `Vec3Argument`: whole x/z numbers mean the block centre
            let corr = center && !text.contains('.') && (i == 0 || (n == 3 && i == 2) || (n == 2 && i == 1));
            c[i] = Coord { rel: false, v: if corr { v + 0.5 } else { v } };
        }
    }
    if n == 2 {
        // x, z for vec2 / column pos; rotation is (yaw, pitch)
        c[2] = c[1];
    }
    Ok(Coords::World(c))
}

impl Coords {
    /// World position for a source at `pos` looking `rot` (yaw, pitch).
    pub fn resolve(&self, pos: [f64; 3], rot: [f32; 2]) -> [f64; 3] {
        match self {
            Coords::World(c) => {
                let mut out = [0.0; 3];
                for i in 0..3 {
                    out[i] = if c[i].rel { pos[i] + c[i].v } else { c[i].v };
                }
                out
            }
            Coords::Local(l) => {
                let d = std::f32::consts::PI / 180.0;
                let (yaw, pitch) = (rot[0], rot[1]);
                let f = ((yaw + 90.0) * d).cos();
                let f1 = ((yaw + 90.0) * d).sin();
                let f2 = (-pitch * d).cos();
                let f3 = (-pitch * d).sin();
                let f4 = ((-pitch + 90.0) * d).cos();
                let f5 = ((-pitch + 90.0) * d).sin();
                let v1 = [(f * f2) as f64, f3 as f64, (f1 * f2) as f64];
                let v2 = [(f * f4) as f64, f5 as f64, (f1 * f4) as f64];
                let c = [v1[1] * v2[2] - v1[2] * v2[1], v1[2] * v2[0] - v1[0] * v2[2], v1[0] * v2[1] - v1[1] * v2[0]];
                let v3 = [-c[0], -c[1], -c[2]];
                let mut out = pos;
                for i in 0..3 {
                    out[i] += v1[i] * l[2] + v2[i] * l[1] + v3[i] * l[0];
                }
                out
            }
        }
    }

    pub fn block(&self, pos: [f64; 3], rot: [f32; 2]) -> [i32; 3] {
        let p = self.resolve(pos, rot);
        [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32]
    }

    /// Rotation arguments: (yaw, pitch) relative to the source's.
    pub fn rotation(&self, rot: [f32; 2]) -> [f32; 2] {
        match self {
            Coords::World(c) => [
                if c[0].rel { rot[0] + c[0].v as f32 } else { c[0].v as f32 },
                if c[1].rel { rot[1] + c[1].v as f32 } else { c[1].v as f32 },
            ],
            Coords::Local(_) => rot,
        }
    }

    /// `vec2` / `column_pos`: (x, z)
    pub fn xz(&self, pos: [f64; 3]) -> [f64; 2] {
        match self {
            Coords::World(c) => [if c[0].rel { pos[0] + c[0].v } else { c[0].v }, if c[2].rel { pos[2] + c[2].v } else { c[2].v }],
            Coords::Local(_) => [pos[0], pos[2]],
        }
    }
}

fn parse_block(r: &mut Reader, predicate: bool) -> R<BlockInput> {
    let start = r.pos;
    let mut tag = None;
    let name = if predicate && r.peek() == '#' {
        r.skip();
        let t = r.read_id()?;
        tag = Some(t.trim_start_matches("minecraft:").to_string());
        None
    } else {
        Some(r.read_id()?)
    };
    let state = match &name {
        Some(n) => {
            let short = n.trim_start_matches("minecraft:");
            BlockRegistry::get().by_name(short).ok_or_else(|| r.err_at(start, "argument.block.id.invalid", vec![json!(n)]))?;
            BlockState::by_name(short)
        }
        None => BlockState::by_name("air"),
    };
    let mut props = Vec::new();
    let mut st = state;
    if r.peek() == '[' {
        r.skip();
        r.skip_ws();
        while r.can_read() && r.peek() != ']' {
            r.skip_ws();
            let k0 = r.pos;
            let k = r.read_string()?;
            r.skip_ws();
            r.expect('=')?;
            r.skip_ws();
            let v = r.read_string()?;
            if name.is_some() {
                match st.prop(&k) {
                    None => return Err(r.err_at(k0, "argument.block.property.unknown", vec![json!(st.name()), json!(k)])),
                    Some(_) => {
                        st = BlockRegistry::get().with(st, &k, &v).ok_or_else(|| r.err_at(k0, "argument.block.property.invalid", vec![json!(st.name()), json!(v), json!(k)]))?;
                    }
                }
            }
            props.push((k, v));
            r.skip_ws();
            if r.peek() == ',' {
                r.skip();
            } else {
                break;
            }
        }
        r.expect(']')?;
    }
    let nbt = if r.peek() == '{' { Some(read_compound(r)?) } else { None };
    Ok(BlockInput { state: st, props, tag, nbt })
}

fn parse_item(r: &mut Reader, predicate: bool) -> R<ItemInput> {
    let start = r.pos;
    let (item, tag) = if predicate && r.peek() == '*' {
        r.skip();
        (None, None)
    } else if predicate && r.peek() == '#' {
        r.skip();
        (None, Some(r.read_id()?.trim_start_matches("minecraft:").to_string()))
    } else {
        let id = r.read_id()?;
        let i = Items::get().id(id.trim_start_matches("minecraft:")).filter(|i| *i != 0).ok_or_else(|| r.err_at(start, "argument.item.id.invalid", vec![json!(id)]))?;
        (Some(i), None)
    };
    let mut comp = 0;
    if r.peek() == '[' {
        r.skip();
        let mut fields = Vec::new();
        r.skip_ws();
        while r.can_read() && r.peek() != ']' {
            r.skip_ws();
            if predicate {
                // component tests are accepted but not checked
                let mut depth = 0;
                while r.can_read() {
                    match r.peek() {
                        '[' | '{' => depth += 1,
                        '}' => depth -= 1,
                        ']' if depth == 0 => break,
                        ']' => depth -= 1,
                        ',' if depth == 0 => break,
                        '"' | '\'' => {
                            r.read_quoted()?;
                            continue;
                        }
                        _ => {}
                    }
                    r.skip();
                }
            } else {
                let neg = r.peek() == '!';
                if neg {
                    r.skip();
                }
                let k = r.read_id()?;
                if neg {
                    continue;
                }
                r.skip_ws();
                r.expect('=')?;
                let v = read_snbt(r)?;
                fields.push((k, v));
            }
            r.skip_ws();
            if r.peek() == ',' {
                r.skip();
            } else {
                break;
            }
        }
        r.expect(']')?;
        if !fields.is_empty() {
            // text components given as SNBT compounds
            for (k, v) in fields.iter_mut() {
                if k == "minecraft:custom_name" || k == "minecraft:item_name" {
                    if let Tag::Compound(_) = v {
                        *v = Tag::String(plain(&tag_to_json(v)));
                    }
                }
            }
            comp = crate::components::from_nbt(&Tag::Compound(fields));
        }
    }
    if predicate && r.peek() == '{' {
        read_compound(r)?;
    }
    Ok(ItemInput { item, tag, comp })
}

/// SNBT/JSON text component.
pub fn parse_component(r: &mut Reader) -> R<Value> {
    let start = r.pos;
    let t = read_snbt(r).map_err(|e| ParseErr { msg: e.msg, at: e.at.max(start) })?;
    Ok(tag_to_json(&t))
}

/// NBT → JSON with bytes as booleans (text component fields).
pub fn tag_to_json(t: &Tag) -> Value {
    match t {
        Tag::Byte(v) => Value::Bool(*v != 0),
        Tag::List(l) => Value::Array(l.iter().map(|x| if let Tag::Compound(c) = x { if c.len() == 1 && c[0].0.is_empty() { tag_to_json(&c[0].1) } else { tag_to_json(x) } } else { tag_to_json(x) }).collect()),
        Tag::Compound(c) => Value::Object(c.iter().map(|(k, v)| (k.clone(), tag_to_json(v))).collect()),
        _ => t.to_json(),
    }
}

/// Plain text of a JSON component (text / translate key fallback).
pub fn plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(plain).collect(),
        Value::Object(o) => {
            let mut s = o.get("text").and_then(|t| t.as_str()).map(String::from).unwrap_or_default();
            if let Some(t) = o.get("translate").and_then(|t| t.as_str()) {
                s.push_str(t);
            }
            if let Some(e) = o.get("extra") {
                s.push_str(&plain(e));
            }
            s
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coords_parse() {
        let mut r = Reader::new("~ ~1 5");
        let c = parse_coords(&mut r, 3, true, false).unwrap();
        assert_eq!(c.resolve([10.0, 64.0, 0.0], [0.0, 0.0]), [10.0, 65.0, 5.5]);
        let mut r = Reader::new("^ ^ ^1");
        let c = parse_coords(&mut r, 3, true, false).unwrap();
        // yaw 0 faces +z
        let p = c.resolve([0.0, 0.0, 0.0], [0.0, 0.0]);
        assert!((p[2] - 1.0).abs() < 1e-6 && p[0].abs() < 1e-6);
        let mut r = Reader::new("1 ~");
        assert!(parse_coords(&mut r, 3, true, false).is_err());
        let mut r = Reader::new("..5");
        assert_eq!(parse_range(&mut r).unwrap(), (None, Some(5.0)));
    }
}
