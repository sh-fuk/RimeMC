//! Vanilla commands: a brigadier-style tree (sent to clients for
//! completion), entity selectors, `execute`, and the command set.

pub mod args;
mod cmds;
pub mod cmds2;
mod execute;
pub mod reader;
pub mod selector;
pub mod state;
pub mod tree;

use crate::game::cmdapi::EntSnap;
use crate::game::{Game, Player};
use crate::world::storage::Dim;
use crate::ServerState;
use args::{Arg, BlockInput, Coords, ItemInput};
use parking_lot::Mutex;
use rand::seq::SliceRandom;
use selector::{Kind, Selector, Sort};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use tree::{Args, Tree};

#[derive(Clone)]
pub enum Output {
    Player(Arc<Player>),
    /// console / control socket: collected plain lines
    Console(Arc<Mutex<Vec<String>>>),
    None,
}

/// Where a store (`execute store`) writes the result.
#[derive(Clone, Debug)]
pub enum StoreTo {
    Score(Vec<String>, String),
    Bar(String, bool),
}

/// `CommandSourceStack`.
#[derive(Clone)]
pub struct Source {
    /// the executing entity when it is a player
    pub player: Option<Arc<Player>>,
    /// the executing entity (player or not)
    pub entity: Option<i32>,
    pub dim: Dim,
    pub pos: [f64; 3],
    /// yaw, pitch
    pub rot: [f32; 2],
    pub eyes: bool,
    pub level: i32,
    pub name: String,
    pub display: Value,
    pub out: Output,
    pub silent: bool,
    /// (target, store the result (true) or success (false))
    pub stores: Vec<(StoreTo, bool)>,
}

impl Source {
    pub fn of_player(p: &Arc<Player>, level: i32) -> Source {
        let pose = *p.pose.lock();
        Source {
            player: Some(p.clone()),
            entity: Some(p.eid),
            dim: Dim::from_index(p.dim.load(Ordering::Relaxed)),
            pos: pose.pos,
            rot: [pose.yaw, pose.pitch],
            eyes: false,
            level,
            name: p.name.clone(),
            display: json!({ "text": p.name }),
            out: Output::Player(p.clone()),
            silent: false,
            stores: vec![],
        }
    }

    pub fn console(st: &ServerState, buf: Arc<Mutex<Vec<String>>>) -> Source {
        Source {
            player: None,
            entity: None,
            dim: Dim::Overworld,
            pos: st.spawn(),
            rot: [0.0, 0.0],
            eyes: false,
            level: 4,
            name: "Server".into(),
            display: json!({ "text": "Server" }),
            out: Output::Console(buf),
            silent: false,
            stores: vec![],
        }
    }

    /// Position after the entity anchor.
    pub fn anchor_pos(&self, st: &ServerState) -> [f64; 3] {
        if !self.eyes {
            return self.pos;
        }
        let eye = match (&self.player, self.entity) {
            (Some(_), _) => 1.62,
            (None, Some(e)) => st.level(self.dim).entity_snap(e).map(|s| s.eye).unwrap_or(0.0),
            _ => 0.0,
        };
        [self.pos[0], self.pos[1] + eye, self.pos[2]]
    }

    fn send(&self, st: &ServerState, v: &Value) {
        match &self.out {
            Output::Player(p) => {
                let mut body = crate::nbt::Tag::from_json(v).to_network_bytes();
                crate::net::codec::Out::bool(&mut body, false);
                p.send(st.game.encode("system_chat", &body));
            }
            Output::Console(buf) => buf.lock().push(render_plain(v)),
            Output::None => {}
        }
    }
}

/// An entity a selector found.
#[derive(Clone)]
pub enum Target {
    Player(Arc<Player>),
    Ent(Dim, EntSnap),
}

impl Target {
    pub fn eid(&self) -> i32 {
        match self {
            Target::Player(p) => p.eid,
            Target::Ent(_, s) => s.eid,
        }
    }
    pub fn uuid(&self) -> uuid::Uuid {
        match self {
            Target::Player(p) => p.uuid,
            Target::Ent(_, s) => s.uuid,
        }
    }
    pub fn dim(&self) -> Dim {
        match self {
            Target::Player(p) => Dim::from_index(p.dim.load(Ordering::Relaxed)),
            Target::Ent(d, _) => *d,
        }
    }
    pub fn pos(&self) -> [f64; 3] {
        match self {
            Target::Player(p) => p.pose.lock().pos,
            Target::Ent(_, s) => s.pos,
        }
    }
    pub fn rot(&self) -> [f32; 2] {
        match self {
            Target::Player(p) => {
                let q = p.pose.lock();
                [q.yaw, q.pitch]
            }
            Target::Ent(_, s) => s.rot,
        }
    }
    pub fn eye(&self) -> f64 {
        match self {
            Target::Player(_) => 1.62,
            Target::Ent(_, s) => s.eye,
        }
    }
    pub fn type_name(&self) -> &str {
        match self {
            Target::Player(_) => "player",
            Target::Ent(_, s) => &s.ty,
        }
    }
    pub fn player(&self) -> Option<&Arc<Player>> {
        match self {
            Target::Player(p) => Some(p),
            _ => None,
        }
    }
    pub fn living(&self) -> bool {
        match self {
            Target::Player(_) => true,
            Target::Ent(_, s) => s.living,
        }
    }
    /// Scoreboard name: player name or UUID.
    pub fn holder(&self) -> String {
        match self {
            Target::Player(p) => p.name.clone(),
            Target::Ent(_, s) => s.uuid.hyphenated().to_string(),
        }
    }
    pub fn name_json(&self) -> Value {
        match self {
            Target::Player(p) => json!({ "text": p.name }),
            Target::Ent(_, s) => match &s.custom_name {
                Some(n) => json!({ "text": n }),
                None => json!({ "translate": format!("entity.minecraft.{}", s.ty) }),
            },
        }
    }
    pub fn plain_name(&self) -> String {
        match self {
            Target::Player(p) => p.name.clone(),
            Target::Ent(_, s) => s.custom_name.clone().unwrap_or_else(|| s.ty.clone()),
        }
    }
}

pub struct Ctx {
    pub st: Arc<ServerState>,
    pub src: Source,
    pub args: Args,
    pub line: String,
}

pub type CR = Result<i32, Value>;

pub fn err(key: &str, with: Vec<Value>) -> Value {
    reader::tr(key, with)
}

impl Ctx {
    pub fn has(&self, n: &str) -> bool {
        !matches!(self.args.get(n), None | Some(Arg::Lit))
    }
    /// Did the parse pass the literal `n`?
    pub fn lit(&self, n: &str) -> bool {
        self.args.contains_key(n)
    }
    pub fn int(&self, n: &str) -> i32 {
        match self.args.get(n) {
            Some(Arg::Int(v)) => *v,
            Some(Arg::Double(v)) => *v as i32,
            Some(Arg::Long(v)) => *v as i32,
            _ => 0,
        }
    }
    pub fn int_or(&self, n: &str, d: i32) -> i32 {
        if self.has(n) {
            self.int(n)
        } else {
            d
        }
    }
    pub fn f64(&self, n: &str) -> f64 {
        match self.args.get(n) {
            Some(Arg::Double(v)) => *v,
            Some(Arg::Int(v)) => *v as f64,
            _ => 0.0,
        }
    }
    pub fn bool(&self, n: &str) -> bool {
        matches!(self.args.get(n), Some(Arg::Bool(true)))
    }
    pub fn str(&self, n: &str) -> String {
        match self.args.get(n) {
            Some(Arg::Str(s)) => s.clone(),
            _ => String::new(),
        }
    }
    pub fn coords(&self, n: &str) -> Coords {
        match self.args.get(n) {
            Some(Arg::Coords(c)) => *c,
            _ => Coords::Local([0.0; 3]),
        }
    }
    /// A `vec3` argument resolved against the source.
    pub fn vec3(&self, n: &str) -> [f64; 3] {
        self.coords(n).resolve(self.local_base(n), self.src.rot)
    }
    fn local_base(&self, n: &str) -> [f64; 3] {
        match self.coords(n) {
            Coords::Local(_) => self.src.anchor_pos(&self.st),
            _ => self.src.pos,
        }
    }
    pub fn block_pos(&self, n: &str) -> [i32; 3] {
        self.coords(n).block(self.local_base(n), self.src.rot)
    }
    pub fn block(&self, n: &str) -> BlockInput {
        match self.args.get(n) {
            Some(Arg::Block(b)) => b.clone(),
            _ => unreachable!("block argument {n}"),
        }
    }
    pub fn item(&self, n: &str) -> ItemInput {
        match self.args.get(n) {
            Some(Arg::Item(b)) => b.clone(),
            _ => ItemInput { item: None, tag: None, comp: 0 },
        }
    }
    pub fn text(&self, n: &str) -> Value {
        match self.args.get(n) {
            Some(Arg::Text(v)) => self.resolve_component(v, 0),
            _ => json!(""),
        }
    }
    pub fn range(&self, n: &str) -> (Option<f64>, Option<f64>) {
        match self.args.get(n) {
            Some(Arg::Range(a, b)) => (*a, *b),
            _ => (None, None),
        }
    }
    pub fn nbt(&self, n: &str) -> Option<crate::nbt::Tag> {
        match self.args.get(n) {
            Some(Arg::Nbt(t)) => Some(t.clone()),
            _ => None,
        }
    }
    pub fn level(&self) -> &Arc<Game> {
        self.st.level(self.src.dim)
    }
    pub fn sel(&self, n: &str) -> Option<Selector> {
        match self.args.get(n) {
            Some(Arg::Sel(s)) => Some(s.clone()),
            _ => None,
        }
    }

    /// Entities of an entity argument; errors when none match.
    pub fn targets(&self, n: &str) -> Result<Vec<Target>, Value> {
        let Some(s) = self.sel(n) else { return Ok(vec![]) };
        let t = resolve(&self.st, &self.src, &s);
        if t.is_empty() {
            return Err(err(if s.players_only() { "argument.entity.notfound.player" } else { "argument.entity.notfound.entity" }, vec![]));
        }
        Ok(t)
    }
    pub fn target(&self, n: &str) -> Result<Target, Value> {
        Ok(self.targets(n)?.remove(0))
    }
    pub fn players(&self, n: &str) -> Result<Vec<Arc<Player>>, Value> {
        let t = self.targets(n)?;
        let p: Vec<Arc<Player>> = t.into_iter().filter_map(|t| t.player().cloned()).collect();
        if p.is_empty() {
            return Err(err("argument.player.unknown", vec![]));
        }
        Ok(p)
    }
    /// The executing player, or the vanilla "requires a player" error.
    pub fn me(&self) -> Result<Arc<Player>, Value> {
        self.src.player.clone().ok_or_else(|| err("permissions.requires.player", vec![]))
    }
    /// The executing entity, if any.
    pub fn me_target(&self) -> Option<Target> {
        if let Some(p) = &self.src.player {
            return Some(Target::Player(p.clone()));
        }
        let e = self.src.entity?;
        self.st.level(self.src.dim).entity_snap(e).map(|s| Target::Ent(self.src.dim, s))
    }
    /// Score holders of a `score_holder` argument.
    pub fn holders(&self, n: &str) -> Result<Vec<String>, Value> {
        match self.args.get(n) {
            Some(Arg::Str(s)) if s == "*" => Ok(state::read(|st| st.scores.keys().cloned().collect())),
            Some(Arg::Str(s)) => Ok(vec![s.clone()]),
            Some(Arg::Sel(_)) => Ok(self.targets(n)?.iter().map(|t| t.holder()).collect()),
            _ => Ok(vec![]),
        }
    }

    /// Success feedback; `broadcast` also tells other operators.
    pub fn ok(&self, msg: Value, broadcast: bool) {
        let feedback = !matches!(self.src.out, Output::Player(_)) || crate::game::rules::on("send_command_feedback");
        if !self.src.silent && feedback {
            self.src.send(&self.st, &msg);
        }
        if broadcast && !self.src.silent {
            let admin = json!({ "translate": "chat.type.admin", "with": [self.src.display.clone(), msg.clone()], "color": "gray", "italic": true });
            let me = self.src.player.as_ref().map(|p| p.eid);
            if crate::game::rules::on("send_command_feedback") {
                for p in self.st.all_players() {
                    if Some(p.eid) != me && self.st.ops.is_op(&crate::ops::CommandSender::Player { name: p.name.clone() }) {
                        let mut body = crate::nbt::Tag::from_json(&admin).to_network_bytes();
                        crate::net::codec::Out::bool(&mut body, false);
                        p.send(self.st.game.encode("system_chat", &body));
                    }
                }
            }
            if !matches!(self.src.out, Output::Console(_)) && crate::game::rules::on("log_admin_commands") {
                tracing::info!("[{}: {}]", self.src.name, render_plain(&msg));
            }
        }
    }

    /// A message to the source regardless of `send_command_feedback`.
    pub fn reply(&self, msg: Value) {
        self.src.send(&self.st, &msg);
    }

    /// Message text with `@` selectors replaced by names.
    pub fn message(&self, n: &str) -> Value {
        let text = self.str(n);
        let mut out = Vec::new();
        let mut plain = String::new();
        let mut r = reader::Reader::new(&text);
        while r.can_read() {
            if r.peek() == '@' && matches!(r.peek_at(1), 'p' | 'a' | 'r' | 's' | 'e' | 'n') && self.src.level >= 2 {
                let save = r.pos;
                if let Ok(sel) = selector::parse(&mut r) {
                    if !plain.is_empty() {
                        out.push(json!({ "text": std::mem::take(&mut plain) }));
                    }
                    out.push(names_json(&resolve(&self.st, &self.src, &sel)));
                    continue;
                }
                r.pos = save;
            }
            plain.push(r.read());
        }
        if !plain.is_empty() {
            out.push(json!({ "text": plain }));
        }
        json!({ "text": "", "extra": if out.is_empty() { vec![json!("")] } else { out } })
    }

    /// `ComponentUtils.updateForEntity`: resolves `selector` / `score` parts.
    pub fn resolve_component(&self, v: &Value, depth: usize) -> Value {
        if depth > 32 {
            return v.clone();
        }
        match v {
            Value::Array(a) => Value::Array(a.iter().map(|x| self.resolve_component(x, depth + 1)).collect()),
            Value::Object(o) => {
                let mut o = o.clone();
                if let Some(sel) = o.get("selector").and_then(|s| s.as_str()).map(String::from) {
                    o.remove("selector");
                    let names = selector::parse(&mut reader::Reader::new(&sel)).map(|s| resolve(&self.st, &self.src, &s)).unwrap_or_default();
                    let n = names_json(&names);
                    o.insert("text".into(), json!(""));
                    o.insert("extra".into(), json!([n]));
                } else if let Some(sc) = o.get("score").cloned() {
                    o.remove("score");
                    let name = sc.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    let obj = sc.get("objective").and_then(|x| x.as_str()).unwrap_or("");
                    let holder = if name == "*" {
                        self.me_target().map(|t| t.holder())
                    } else if name.starts_with('@') {
                        selector::parse(&mut reader::Reader::new(name)).ok().and_then(|s| resolve(&self.st, &self.src, &s).first().map(|t| t.holder()))
                    } else {
                        Some(name.to_string())
                    };
                    let v = holder.and_then(|h| state::read(|s| state::get_score(s, &h, obj)));
                    o.insert("text".into(), json!(v.map(|x| x.to_string()).unwrap_or_default()));
                }
                if let Some(e) = o.get("extra").cloned() {
                    o.insert("extra".into(), self.resolve_component(&e, depth + 1));
                }
                if let Some(w) = o.get("with").cloned() {
                    o.insert("with".into(), self.resolve_component(&w, depth + 1));
                }
                Value::Object(o)
            }
            other => other.clone(),
        }
    }
}

/// "A, B and C" style list of names.
pub fn names_json(t: &[Target]) -> Value {
    let mut extra = Vec::new();
    for (i, x) in t.iter().enumerate() {
        if i > 0 {
            extra.push(json!({ "text": ", " }));
        }
        extra.push(x.name_json());
    }
    json!({ "text": "", "extra": if extra.is_empty() { vec![json!("")] } else { extra } })
}

/// One name, or "N entities" for several (vanilla `.success.single` / `.multiple`).
pub fn one_or_many(t: &[Target], single: &str, multiple: &str, extra: Vec<Value>) -> Value {
    let mut with = Vec::new();
    if t.len() == 1 {
        with.push(t[0].name_json());
        with.extend(extra);
        err(single, with)
    } else {
        with.push(json!(t.len()));
        with.extend(extra);
        err(multiple, with)
    }
}

/// Plain text of a component, with English for the keys commands use.
pub fn render_plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(render_plain).collect(),
        Value::Object(o) => {
            let mut s = o.get("text").and_then(|t| t.as_str()).map(String::from).unwrap_or_default();
            if let Some(k) = o.get("translate").and_then(|t| t.as_str()) {
                let with: Vec<String> = o.get("with").and_then(|w| w.as_array()).map(|a| a.iter().map(render_plain).collect()).unwrap_or_default();
                if with.is_empty() {
                    s.push_str(k);
                } else {
                    s.push_str(&format!("{k}({})", with.join(", ")));
                }
            }
            if let Some(e) = o.get("extra") {
                s.push_str(&render_plain(e));
            }
            s
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ------------------------------------------------------------------ selectors

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

/// `EntitySelector.findEntities`.
pub fn resolve(st: &Arc<ServerState>, src: &Source, s: &Selector) -> Vec<Target> {
    let all_levels = |only: Option<Dim>| -> Vec<Target> {
        let mut v: Vec<Target> = Vec::new();
        for g in st.levels() {
            if only.is_some_and(|d| d != g.dim) {
                continue;
            }
            v.extend(g.players().into_iter().map(Target::Player));
            if !s.players_only() {
                v.extend(g.entity_snaps().into_iter().map(|e| Target::Ent(g.dim, e)));
            }
        }
        v
    };
    let cands: Vec<Target> = match &s.kind {
        Kind::Name(n) => return st.find_player(n).map(Target::Player).into_iter().collect(),
        Kind::Uuid(u) => {
            return all_levels(None).into_iter().filter(|t| t.uuid() == *u).take(1).collect();
        }
        Kind::S => {
            let me = match (&src.player, src.entity) {
                (Some(p), _) => Some(Target::Player(p.clone())),
                (None, Some(e)) => st.level(src.dim).entity_snap(e).map(|x| Target::Ent(src.dim, x)),
                _ => None,
            };
            me.into_iter().collect()
        }
        Kind::P | Kind::N => all_levels(Some(src.dim)),
        _ => all_levels(if s.world_limited() { Some(src.dim) } else { None }),
    };
    let base = [s.pos[0].unwrap_or(src.pos[0]), s.pos[1].unwrap_or(src.pos[1]), s.pos[2].unwrap_or(src.pos[2])];
    let volume = s.delta.iter().any(|d| d.is_some()).then(|| {
        let mut lo = base;
        let mut hi = base;
        for i in 0..3 {
            let d = s.delta[i].unwrap_or(0.0);
            if d < 0.0 {
                lo[i] += d;
            } else {
                hi[i] += d;
            }
            hi[i] += 1.0;
        }
        (lo, hi)
    });
    let data = &st.game.data;
    let mut out: Vec<Target> = cands
        .into_iter()
        .filter(|t| {
            if s.world_limited() && t.dim() != src.dim {
                return false;
            }
            for (neg, ty) in &s.types {
                let m = match ty.strip_prefix('#') {
                    Some(tag) => data.entity_in(tag, t.type_name()),
                    None => t.type_name() == ty,
                };
                if m == *neg {
                    return false;
                }
            }
            for (neg, n) in &s.names {
                let m = match t {
                    Target::Player(p) => p.name == *n,
                    Target::Ent(_, e) => e.custom_name.as_deref() == Some(n.as_str()),
                };
                if m == *neg {
                    return false;
                }
            }
            if !s.tags.is_empty() {
                let tags = state::read(|x| x.tags.get(&t.uuid()).cloned()).unwrap_or_default();
                for (neg, tag) in &s.tags {
                    let m = if tag.is_empty() { tags.is_empty() } else { tags.contains(tag) };
                    if m == *neg {
                        return false;
                    }
                }
            }
            if !s.teams.is_empty() {
                let team = state::read(|x| state::team_of(x, &t.holder()));
                for (neg, tm) in &s.teams {
                    let m = if tm.is_empty() { team.is_none() } else { team.as_deref() == Some(tm.as_str()) };
                    if m == *neg {
                        return false;
                    }
                }
            }
            if !s.gamemodes.is_empty() {
                let Some(p) = t.player() else { return false };
                let gm = p.gamemode.load(Ordering::Relaxed);
                for (neg, g) in &s.gamemodes {
                    if (gm == *g) == *neg {
                        return false;
                    }
                }
            }
            if let Some(r) = s.level {
                let Some(p) = t.player() else { return false };
                if !args::in_range(r, p.vit.lock().xp_level as f64) {
                    return false;
                }
            }
            let pos = t.pos();
            if let Some(r) = s.distance {
                let d = dist2(pos, base).sqrt();
                if !args::in_range(r, d) {
                    return false;
                }
            }
            if let Some((lo, hi)) = volume {
                if (0..3).any(|i| pos[i] < lo[i] || pos[i] > hi[i]) {
                    return false;
                }
            }
            let rot = t.rot();
            if let Some(r) = s.x_rot {
                if !args::in_range(r, rot[1] as f64) {
                    return false;
                }
            }
            if let Some(r) = s.y_rot {
                let y = (rot[0] as f64 + 180.0).rem_euclid(360.0) - 180.0;
                if !args::in_range(r, y) {
                    return false;
                }
            }
            if !s.scores.is_empty() {
                let h = t.holder();
                let ok = state::read(|x| s.scores.iter().all(|(o, r)| state::get_score(x, &h, o).is_some_and(|v| args::in_range(*r, v as f64))));
                if !ok {
                    return false;
                }
            }
            true
        })
        .collect();
    match s.sort {
        Sort::Nearest => out.sort_by(|a, b| dist2(a.pos(), base).total_cmp(&dist2(b.pos(), base))),
        Sort::Furthest => out.sort_by(|a, b| dist2(b.pos(), base).total_cmp(&dist2(a.pos(), base))),
        Sort::Random => out.shuffle(&mut rand::thread_rng()),
        Sort::Arbitrary => {}
    }
    if let Some(l) = s.limit {
        out.truncate(l);
    }
    out
}

// ------------------------------------------------------------------ dispatch

static SERVER: OnceLock<std::sync::Weak<ServerState>> = OnceLock::new();

/// Remembers the server for game code that has no handle on it.
pub fn set_server(st: &Arc<ServerState>) {
    let _ = SERVER.set(Arc::downgrade(st));
}

pub fn server() -> Option<Arc<ServerState>> {
    SERVER.get()?.upgrade()
}

pub fn tree() -> &'static Tree {
    static T: OnceLock<Tree> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = Tree::new();
        cmds::register(&mut t);
        cmds2::register(&mut t);
        execute::register(&mut t);
        t
    })
}

/// Permission level of a player (ops: 4).
pub fn level_of(st: &ServerState, name: &str) -> i32 {
    if st.ops.is_op(&crate::ops::CommandSender::Player { name: name.to_string() }) {
        4
    } else {
        0
    }
}

/// The `commands` packet body for a player.
pub fn commands_packet(level: i32) -> Vec<u8> {
    let extra = if crate::plugins::active() { crate::plugins::command_names().into_iter().filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_graphic())).collect() } else { Vec::new() };
    tree().packet(level, &extra)
}

/// Is this line one of ours (otherwise: legacy RimeMC admin commands)?
pub fn knows(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or("");
    tree().root_child(first).is_some()
}

/// Runs a command line; returns the summed result.
pub fn run(st: &Arc<ServerState>, src: Source, line: &str) -> i32 {
    let line = line.trim_start_matches('/');
    let t = tree();
    let parsed = match t.parse(line, src.level) {
        Ok(p) => p,
        Err(e) => {
            for l in tree::error_lines(line, &e) {
                src.send(st, &l);
            }
            return 0;
        }
    };
    let mut sources = vec![src];
    let max_forks = crate::game::rules::get("max_command_forks").max(1) as usize;
    for stage in parsed.stages {
        let Some(fork) = stage.fork else { continue };
        let single = sources.len() == 1;
        let mut next = Vec::new();
        for s in sources {
            let mut ctx = Ctx { st: st.clone(), src: s, args: stage.args.clone(), line: line.to_string() };
            match fork(&mut ctx) {
                Ok(v) => next.extend(v),
                Err(e) => {
                    if single {
                        ctx.src.send(st, &with_red(e));
                    }
                }
            }
            if next.len() > max_forks {
                next.truncate(max_forks);
                break;
            }
        }
        sources = next;
        if sources.is_empty() {
            return 0;
        }
    }
    let mut total = 0;
    for s in sources {
        let stores = s.stores.clone();
        let mut ctx = Ctx { st: st.clone(), src: s, args: parsed.args.clone(), line: line.to_string() };
        let (ok, n) = match (parsed.exec)(&mut ctx) {
            Ok(n) => (true, n),
            Err(e) => {
                ctx.src.send(st, &with_red(e));
                (false, 0)
            }
        };
        total += n;
        if !stores.is_empty() {
            execute::apply_stores(st, &stores, ok, n);
        }
    }
    total
}

fn with_red(e: Value) -> Value {
    json!({ "text": "", "color": "red", "extra": [e] })
}

/// Runs a console / control-socket line and returns the output text.
pub fn run_console(st: &Arc<ServerState>, line: &str) -> String {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let src = Source::console(st, buf.clone());
    run(st, src, line);
    let out = buf.lock().join("\n");
    out
}

/// A player's command (from chat or a plugin request).
pub fn run_player(st: &Arc<ServerState>, p: &Arc<Player>, line: &str) -> i32 {
    let level = level_of(st, &p.name);
    run(st, Source::of_player(p, level), line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_parses_and_encodes() {
        let t = tree();
        assert!(t.parse("gamemode creative", 4).is_ok());
        assert!(t.parse("gamemode creative @a", 4).is_ok());
        assert!(t.parse("gamemode nope", 4).is_err());
        assert!(t.parse("gamemode creative", 0).is_err());
        assert!(t.parse("execute as @a at @s run tp ~ ~1 ~", 4).is_ok());
        let p = t.parse("execute as @e[type=cow] run say hi", 4).unwrap();
        assert_eq!(p.stages.len(), 2);
        assert!(t.parse("execute if entity @a", 4).is_ok());
        assert!(t.parse("tp 1 2 3", 4).is_ok());
        assert!(t.parse("give @s diamond 5", 4).is_ok());
        assert!(t.parse("give @s minecraft:diamond_sword[minecraft:damage=5] 1", 4).is_ok());
        assert!(t.parse("setblock ~ ~ ~ oak_stairs[facing=east]", 4).is_ok());
        assert!(t.parse("scoreboard players add @a obj 3", 4).is_ok());
        assert!(t.parse("tellraw @a {text:\"hi\",color:\"red\"}", 4).is_ok());
        assert!(t.parse("effect give @p speed 30 1 true", 4).is_ok());
        assert!(t.parse("list", 0).is_ok());
        let Err(e) = t.parse("gamemode creative @z", 4) else { panic!("parsed") };
        assert!(e.at >= 18);
        let pkt = commands_packet(4);
        assert!(pkt.len() > 1000);
        let small = commands_packet(0);
        assert!(small.len() < pkt.len());
    }
}
