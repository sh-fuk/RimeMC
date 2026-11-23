//! Scoreboard, teams, custom boss bars, entity tags and the world border:
//! server-wide state that commands change, saved in `rime-commands.json`.

use crate::game::Game;
use crate::nbt::Tag;
use crate::net::codec::Out;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Serialize, Deserialize)]
pub struct Objective {
    pub criteria: String,
    pub display: Value,
    pub hearts: bool,
    #[serde(default)]
    pub auto_update: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Team {
    pub display: Value,
    pub color: String,
    pub prefix: Value,
    pub suffix: Value,
    pub friendly_fire: bool,
    pub see_invisible: bool,
    pub nametag: String,
    pub death_messages: String,
    pub collision: String,
    pub members: BTreeSet<String>,
}

impl Team {
    pub fn new(name: &str, display: Option<Value>) -> Team {
        Team {
            display: display.unwrap_or_else(|| json!({ "text": name })),
            color: "reset".into(),
            prefix: json!(""),
            suffix: json!(""),
            friendly_fire: true,
            see_invisible: true,
            nametag: "always".into(),
            death_messages: "always".into(),
            collision: "always".into(),
            members: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Bar {
    pub uuid: uuid::Uuid,
    pub name: Value,
    pub color: String,
    pub style: String,
    pub value: i32,
    pub max: i32,
    pub visible: bool,
    /// player UUIDs
    pub players: BTreeSet<uuid::Uuid>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Border {
    pub center: [f64; 2],
    pub size: f64,
    /// lerping towards (target, ticks left)
    pub target: f64,
    pub lerp_ticks: i64,
    pub damage: f64,
    pub buffer: f64,
    pub warn_distance: i32,
    pub warn_time: i32,
}

impl Default for Border {
    fn default() -> Self {
        Border { center: [0.0, 0.0], size: 59_999_968.0, target: 59_999_968.0, lerp_ticks: 0, damage: 0.2, buffer: 5.0, warn_distance: 5, warn_time: 15 }
    }
}

impl Border {
    pub fn min_x(&self) -> f64 {
        self.center[0] - self.size / 2.0
    }
    pub fn max_x(&self) -> f64 {
        self.center[0] + self.size / 2.0
    }
    pub fn min_z(&self) -> f64 {
        self.center[1] - self.size / 2.0
    }
    pub fn max_z(&self) -> f64 {
        self.center[1] + self.size / 2.0
    }
    /// Distance inside the border (negative outside).
    pub fn distance_inside(&self, x: f64, z: f64) -> f64 {
        (x - self.min_x()).min(self.max_x() - x).min(z - self.min_z()).min(self.max_z() - z)
    }
    pub fn contains(&self, x: f64, z: f64) -> bool {
        self.distance_inside(x, z) > 0.0
    }
    pub fn tick(&mut self) -> bool {
        if self.lerp_ticks <= 0 {
            return false;
        }
        self.size += (self.target - self.size) / self.lerp_ticks as f64;
        self.lerp_ticks -= 1;
        if self.lerp_ticks == 0 {
            self.size = self.target;
        }
        true
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct State {
    pub objectives: BTreeMap<String, Objective>,
    /// holder → objective → score
    pub scores: BTreeMap<String, BTreeMap<String, i32>>,
    /// display slot → objective
    pub display: BTreeMap<i32, String>,
    pub teams: BTreeMap<String, Team>,
    pub bars: BTreeMap<String, Bar>,
    pub tags: HashMap<uuid::Uuid, BTreeSet<String>>,
    pub border: Border,
    /// `data … storage` compounds (SNBT)
    #[serde(default)]
    pub storage: BTreeMap<String, String>,
    /// players allowed to use `trigger` on an objective: (holder, objective)
    #[serde(default)]
    pub triggers: BTreeSet<(String, String)>,
}

struct Holder {
    state: State,
    file: Option<PathBuf>,
    dirty: bool,
}

static STATE: Mutex<Option<Holder>> = Mutex::new(None);

/// Runs `f` on the state; `changed` marks it for saving.
pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock();
    let h = g.get_or_insert_with(|| Holder { state: State::default(), file: None, dirty: false });
    h.dirty = true;
    f(&mut h.state)
}

pub fn read<R>(f: impl FnOnce(&State) -> R) -> R {
    let mut g = STATE.lock();
    let h = g.get_or_insert_with(|| Holder { state: State::default(), file: None, dirty: false });
    f(&h.state)
}

pub fn load(world_dir: &std::path::Path) {
    let file = world_dir.join("rime-commands.json");
    let state = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<State>(&t).map_err(|e| tracing::error!("reading {}: {e}", file.display())).ok()).unwrap_or_default();
    *STATE.lock() = Some(Holder { state, file: Some(file), dirty: false });
}

pub fn save() {
    let mut g = STATE.lock();
    let Some(h) = g.as_mut() else { return };
    let (Some(f), true) = (&h.file, h.dirty) else { return };
    match serde_json::to_string(&h.state) {
        Ok(t) => {
            if let Err(e) = std::fs::write(f, t) {
                tracing::error!("saving {}: {e}", f.display());
            } else {
                h.dirty = false;
            }
        }
        Err(e) => tracing::error!("encoding command state: {e}"),
    }
}

// ------------------------------------------------------------------ packets

fn comp(b: &mut Vec<u8>, v: &Value) {
    b.raw(&Tag::from_json(v).to_network_bytes());
}

pub fn color_id(c: &str) -> i32 {
    // ChatFormatting ordinal: colours 0-15, reset 21
    super::args::COLORS.iter().position(|x| *x == c).map(|i| i as i32).unwrap_or(21)
}

pub fn objective_packet(g: &Game, name: &str, o: Option<&Objective>) -> Arc<Vec<u8>> {
    let mut b = Vec::new();
    b.str(name);
    match o {
        Some(o) => {
            b.u8(0);
            comp(&mut b, &o.display);
            b.vi(o.hearts as i32).bool(false);
        }
        None => {
            b.u8(1);
        }
    }
    g.encode("set_objective", &b)
}

pub fn objective_update_packet(g: &Game, name: &str, o: &Objective) -> Arc<Vec<u8>> {
    let mut b = Vec::new();
    b.str(name).u8(2);
    comp(&mut b, &o.display);
    b.vi(o.hearts as i32).bool(false);
    g.encode("set_objective", &b)
}

pub fn score_packet(g: &Game, holder: &str, obj: &str, v: i32) -> Arc<Vec<u8>> {
    let mut b = Vec::new();
    b.str(holder).str(obj).vi(v).bool(false).bool(false);
    g.encode("set_score", &b)
}

pub fn reset_score_packet(g: &Game, holder: &str, obj: Option<&str>) -> Arc<Vec<u8>> {
    let mut b = Vec::new();
    b.str(holder);
    match obj {
        Some(o) => {
            b.bool(true).str(o);
        }
        None => {
            b.bool(false);
        }
    }
    g.encode("reset_score", &b)
}

pub fn display_packet(g: &Game, slot: i32, obj: &str) -> Arc<Vec<u8>> {
    let mut b = Vec::new();
    b.vi(slot).str(obj);
    g.encode("set_display_objective", &b)
}

fn vis_id(s: &str) -> i32 {
    match s {
        "never" => 1,
        "hideForOtherTeams" => 2,
        "hideForOwnTeam" => 3,
        _ => 0,
    }
}

fn collision_id(s: &str) -> i32 {
    match s {
        "never" => 1,
        "pushOtherTeams" => 2,
        "pushOwnTeam" => 3,
        _ => 0,
    }
}

fn team_info(b: &mut Vec<u8>, t: &Team) {
    comp(b, &t.display);
    b.u8(t.friendly_fire as u8 | ((t.see_invisible as u8) << 1));
    b.vi(vis_id(&t.nametag)).vi(collision_id(&t.collision)).vi(color_id(&t.color));
    comp(b, &t.prefix);
    comp(b, &t.suffix);
}

/// method 0 create, 1 remove, 2 update
pub fn team_packet(g: &Game, name: &str, t: &Team, method: u8) -> Arc<Vec<u8>> {
    let mut b = Vec::new();
    b.str(name).u8(method);
    if method == 0 || method == 2 {
        team_info(&mut b, t);
    }
    if method == 0 {
        b.vi(t.members.len() as i32);
        for m in &t.members {
            b.str(m);
        }
    }
    g.encode("set_player_team", &b)
}

/// method 3 add, 4 remove
pub fn team_members_packet(g: &Game, name: &str, members: &[String], add: bool) -> Arc<Vec<u8>> {
    let mut b = Vec::new();
    b.str(name).u8(if add { 3 } else { 4 }).vi(members.len() as i32);
    for m in members {
        b.str(m);
    }
    g.encode("set_player_team", &b)
}

pub fn bar_color(c: &str) -> i32 {
    ["pink", "blue", "red", "green", "yellow", "purple", "white"].iter().position(|x| *x == c).unwrap_or(6) as i32
}

pub fn bar_style(s: &str) -> i32 {
    ["progress", "notched_6", "notched_10", "notched_12", "notched_20"].iter().position(|x| *x == s).unwrap_or(0) as i32
}

pub fn bar_add_packet(g: &Game, bar: &Bar) -> Arc<Vec<u8>> {
    let mut b = Vec::new();
    b.uuid(bar.uuid).vi(0);
    comp(&mut b, &bar.name);
    b.f32(if bar.max > 0 { bar.value as f32 / bar.max as f32 } else { 0.0 }).vi(bar_color(&bar.color)).vi(bar_style(&bar.style)).u8(0);
    g.encode("boss_event", &b)
}

pub fn bar_remove_packet(g: &Game, bar: &Bar) -> Arc<Vec<u8>> {
    let mut b = Vec::new();
    b.uuid(bar.uuid).vi(1);
    g.encode("boss_event", &b)
}

pub fn border_init_packet(g: &Game, w: &Border) -> Arc<Vec<u8>> {
    let mut b = Vec::new();
    b.f64(w.center[0]).f64(w.center[1]).f64(w.size).f64(if w.lerp_ticks > 0 { w.target } else { w.size }).vl(w.lerp_ticks * 50).vi(29_999_984).vi(w.warn_distance).vi(w.warn_time);
    g.encode("initialize_border", &b)
}

/// Everything a joining player must know.
pub fn join_packets(g: &Game, uuid: uuid::Uuid) -> Vec<Arc<Vec<u8>>> {
    read(|s| {
        let mut out = Vec::new();
        for (n, o) in &s.objectives {
            out.push(objective_packet(g, n, Some(o)));
        }
        for (h, m) in &s.scores {
            for (o, v) in m {
                if s.objectives.contains_key(o) {
                    out.push(score_packet(g, h, o, *v));
                }
            }
        }
        for (slot, o) in &s.display {
            out.push(display_packet(g, *slot, o));
        }
        for (n, t) in &s.teams {
            out.push(team_packet(g, n, t, 0));
        }
        for bar in s.bars.values() {
            if bar.visible && bar.players.contains(&uuid) {
                out.push(bar_add_packet(g, bar));
            }
        }
        out.push(border_init_packet(g, &s.border));
        out
    })
}

/// Sets a score and returns the packet to broadcast.
pub fn set_score(s: &mut State, holder: &str, obj: &str, v: i32) {
    s.scores.entry(holder.to_string()).or_default().insert(obj.to_string(), v);
}

pub fn get_score(s: &State, holder: &str, obj: &str) -> Option<i32> {
    s.scores.get(holder)?.get(obj).copied()
}

/// The team a holder belongs to.
pub fn team_of(s: &State, holder: &str) -> Option<String> {
    s.teams.iter().find(|(_, t)| t.members.contains(holder)).map(|(n, _)| n.clone())
}

/// Adds `delta` to every objective with this criterion for `holder`
/// (`deathCount`, `playerKillCount`, statistics…); returns score packets.
pub fn award(g: &Game, holder: &str, criterion: &str, delta: i32) -> Vec<Arc<Vec<u8>>> {
    let mut g2 = STATE.lock();
    let Some(h) = g2.as_mut() else { return vec![] };
    let objs: Vec<String> = h.state.objectives.iter().filter(|(_, o)| o.criteria == criterion).map(|(n, _)| n.clone()).collect();
    if objs.is_empty() {
        return vec![];
    }
    h.dirty = true;
    let mut out = Vec::new();
    for o in objs {
        let e = h.state.scores.entry(holder.to_string()).or_default().entry(o.clone()).or_insert(0);
        *e = e.saturating_add(delta);
        out.push(score_packet(g, holder, &o, *e));
    }
    out
}

/// Sets every objective with a read-only criterion (`health`, `food` …).
pub fn update_criterion(g: &Game, holder: &str, criterion: &str, value: i32) -> Vec<Arc<Vec<u8>>> {
    let mut g2 = STATE.lock();
    let Some(h) = g2.as_mut() else { return vec![] };
    let objs: Vec<String> = h.state.objectives.iter().filter(|(_, o)| o.criteria == criterion).map(|(n, _)| n.clone()).collect();
    let mut out = Vec::new();
    for o in objs {
        let e = h.state.scores.entry(holder.to_string()).or_default().entry(o.clone()).or_insert(i32::MIN);
        if *e != value {
            *e = value;
            h.dirty = true;
            out.push(score_packet(g, holder, &o, value));
        }
    }
    out
}

pub fn has_tag(u: uuid::Uuid, t: &str) -> bool {
    read(|s| s.tags.get(&u).is_some_and(|x| x.contains(t)))
}
