//! Advancements (`ServerAdvancementManager` / `PlayerAdvancements`): the
//! vanilla advancement JSON, the tree layout (`TreeNodePosition`), per-player
//! progress in `world/advancements/<uuid>.json`, visibility, the
//! `update_advancements` packet, rewards and chat announcements. Criteria
//! triggers are evaluated by `Game::trigger` with the predicate helpers below.

use super::{Game, Player};
use crate::data::{Items, Stack};
use crate::net::codec::Out;
use crate::registry::BlockState;
use crate::world::storage::Dim;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

// ------------------------------------------------------------------ definitions

pub struct Display {
    pub title: Value,
    pub description: Value,
    pub icon: (i32, i32),
    /// 0 task, 1 challenge, 2 goal
    pub frame: i32,
    pub show_toast: bool,
    pub announce: bool,
    pub hidden: bool,
    pub background: Option<String>,
    pub x: f32,
    pub y: f32,
}

pub struct Criterion {
    pub name: String,
    pub trigger: String,
    pub conditions: Value,
}

pub struct Adv {
    pub id: String,
    pub parent: Option<usize>,
    pub display: Option<Display>,
    pub criteria: Vec<Criterion>,
    pub requirements: Vec<Vec<String>>,
    pub xp: i32,
    pub recipes: Vec<String>,
    pub telemetry: bool,
}

#[derive(Default)]
pub struct Tree {
    pub advs: Vec<Adv>,
    pub ids: HashMap<String, usize>,
    pub children: Vec<Vec<usize>>,
    /// trigger → (advancement, criterion index)
    pub by_trigger: HashMap<String, Vec<(usize, usize)>>,
}

static TREE: OnceLock<Tree> = OnceLock::new();

pub fn tree() -> &'static Tree {
    TREE.get_or_init(Tree::default)
}

fn strip(s: &str) -> &str {
    s.strip_prefix("minecraft:").unwrap_or(s)
}

fn full(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

/// Reads `data/minecraft/advancement/**.json`.
pub fn load(dir: &Path) {
    let mut files = Vec::new();
    fn walk(base: &Path, cur: &Path, out: &mut Vec<(String, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(cur) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else if p.extension().is_some_and(|x| x == "json") {
                let rel = p.strip_prefix(base).unwrap().with_extension("");
                out.push((format!("minecraft:{}", rel.to_string_lossy().replace('\\', "/")), p));
            }
        }
    }
    walk(dir, dir, &mut files);
    files.sort();
    let mut raw = Vec::new();
    for (id, p) in files {
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        raw.push((id, v));
    }
    let t = build(raw);
    tracing::info!("{} advancements loaded", t.advs.len());
    let _ = TREE.set(t);
}

fn build(raw: Vec<(String, Value)>) -> Tree {
    let mut t = Tree::default();
    for (i, (id, _)) in raw.iter().enumerate() {
        t.ids.insert(id.clone(), i);
    }
    for (id, v) in &raw {
        let parent = v.get("parent").and_then(|p| p.as_str()).and_then(|p| t.ids.get(&full(p)).copied());
        let display = v.get("display").map(|d| {
            let icon = d.get("icon").map(|i| (i.get("id").and_then(|x| x.as_str()).and_then(|x| Items::get().id(strip(x))).unwrap_or(0), i.get("count").and_then(|x| x.as_i64()).unwrap_or(1) as i32)).unwrap_or((0, 1));
            Display {
                title: d.get("title").cloned().unwrap_or(json!("")),
                description: d.get("description").cloned().unwrap_or(json!("")),
                icon,
                frame: match d.get("frame").and_then(|x| x.as_str()).unwrap_or("task") {
                    "challenge" => 1,
                    "goal" => 2,
                    _ => 0,
                },
                show_toast: d.get("show_toast").and_then(|x| x.as_bool()).unwrap_or(true),
                announce: d.get("announce_to_chat").and_then(|x| x.as_bool()).unwrap_or(true),
                hidden: d.get("hidden").and_then(|x| x.as_bool()).unwrap_or(false),
                background: d.get("background").and_then(|x| x.as_str()).map(full),
                x: 0.0,
                y: 0.0,
            }
        });
        let criteria: Vec<Criterion> = v
            .get("criteria")
            .and_then(|c| c.as_object())
            .map(|o| o.iter().map(|(k, c)| Criterion { name: k.clone(), trigger: strip(c.get("trigger").and_then(|x| x.as_str()).unwrap_or("impossible")).to_string(), conditions: c.get("conditions").cloned().unwrap_or(json!({})) }).collect())
            .unwrap_or_default();
        let requirements = match v.get("requirements").and_then(|r| r.as_array()) {
            Some(r) => r.iter().map(|g| g.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()).collect(),
            // AdvancementRequirements.allOf
            None => criteria.iter().map(|c| vec![c.name.clone()]).collect(),
        };
        let rewards = v.get("rewards");
        t.advs.push(Adv {
            id: id.clone(),
            parent,
            display,
            criteria,
            requirements,
            xp: rewards.and_then(|r| r.get("experience")).and_then(|x| x.as_i64()).unwrap_or(0) as i32,
            recipes: rewards.and_then(|r| r.get("recipes")).and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(|s| strip(s).to_string())).collect()).unwrap_or_default(),
            telemetry: v.get("sends_telemetry_event").and_then(|x| x.as_bool()).unwrap_or(false),
        });
    }
    t.children = vec![Vec::new(); t.advs.len()];
    for (i, a) in t.advs.iter().enumerate() {
        if let Some(p) = a.parent {
            t.children[p].push(i);
        }
        for (ci, c) in a.criteria.iter().enumerate() {
            t.by_trigger.entry(c.trigger.clone()).or_default().push((i, ci));
        }
    }
    let roots: Vec<usize> = (0..t.advs.len()).filter(|i| t.advs[*i].parent.is_none() && t.advs[*i].display.is_some()).collect();
    for r in roots {
        layout(&mut t, r);
    }
    t
}

pub fn root_of(t: &Tree, mut i: usize) -> usize {
    while let Some(p) = t.advs[i].parent {
        i = p;
    }
    i
}

// ------------------------------------------------------------------ layout

/// `TreeNodePosition` (Buchheim–Walker, as vanilla runs it on the server).
struct Pos {
    adv: usize,
    parent: Option<usize>,
    prev: Option<usize>,
    child_index: i32,
    children: Vec<usize>,
    ancestor: usize,
    thread: Option<usize>,
    x: i32,
    y: f32,
    modv: f32,
    change: f32,
    shift: f32,
}

struct Layout<'a> {
    t: &'a Tree,
    n: Vec<Pos>,
}

impl Layout<'_> {
    fn new_node(&mut self, adv: usize, parent: Option<usize>, prev: Option<usize>, child_index: i32, x: i32) -> usize {
        let me = self.n.len();
        self.n.push(Pos { adv, parent, prev, child_index, children: vec![], ancestor: me, thread: None, x, y: -1.0, modv: 0.0, change: 0.0, shift: 0.0 });
        let mut previous = None;
        for c in self.t.children[adv].clone() {
            previous = self.add_child(me, c, previous);
        }
        me
    }

    fn add_child(&mut self, me: usize, adv: usize, previous: Option<usize>) -> Option<usize> {
        if self.t.advs[adv].display.is_some() {
            let idx = self.n[me].children.len() as i32 + 1;
            let x = self.n[me].x + 1;
            let c = self.new_node(adv, Some(me), previous, idx, x);
            self.n[me].children.push(c);
            Some(c)
        } else {
            let mut previous = previous;
            for g in self.t.children[adv].clone() {
                previous = self.add_child(me, g, previous);
            }
            previous
        }
    }

    fn first_walk(&mut self, me: usize) {
        let children = self.n[me].children.clone();
        if children.is_empty() {
            self.n[me].y = match self.n[me].prev {
                Some(p) => self.n[p].y + 1.0,
                None => 0.0,
            };
        } else {
            let mut default_ancestor: Option<usize> = None;
            for &c in &children {
                self.first_walk(c);
                default_ancestor = Some(self.apportion(c, default_ancestor.unwrap_or(c)));
            }
            self.execute_shifts(me);
            let mid = (self.n[children[0]].y + self.n[*children.last().unwrap()].y) / 2.0;
            match self.n[me].prev {
                Some(p) => {
                    self.n[me].y = self.n[p].y + 1.0;
                    self.n[me].modv = self.n[me].y - mid;
                }
                None => self.n[me].y = mid,
            }
        }
    }

    fn second_walk(&mut self, me: usize, m: f32, depth: i32, mut min: f32) -> f32 {
        self.n[me].y += m;
        self.n[me].x = depth;
        if self.n[me].y < min {
            min = self.n[me].y;
        }
        let md = self.n[me].modv;
        for c in self.n[me].children.clone() {
            min = self.second_walk(c, m + md, depth + 1, min);
        }
        min
    }

    fn third_walk(&mut self, me: usize, m: f32) {
        self.n[me].y += m;
        for c in self.n[me].children.clone() {
            self.third_walk(c, m);
        }
    }

    fn execute_shifts(&mut self, me: usize) {
        let mut shift = 0.0;
        let mut change = 0.0;
        for &c in self.n[me].children.clone().iter().rev() {
            self.n[c].y += shift;
            self.n[c].modv += shift;
            change += self.n[c].change;
            shift += self.n[c].shift + change;
        }
    }

    fn prev_or_thread(&self, i: usize) -> Option<usize> {
        self.n[i].thread.or_else(|| self.n[i].children.first().copied())
    }

    fn next_or_thread(&self, i: usize) -> Option<usize> {
        self.n[i].thread.or_else(|| self.n[i].children.last().copied())
    }

    fn apportion(&mut self, me: usize, mut default_ancestor: usize) -> usize {
        let Some(prev) = self.n[me].prev else { return default_ancestor };
        let parent = self.n[me].parent.unwrap();
        let mut vip = me;
        let mut vop = me;
        let mut vim = prev;
        let mut vom = self.n[parent].children[0];
        let mut sip = self.n[me].modv;
        let mut sop = self.n[me].modv;
        let mut sim = self.n[vim].modv;
        let mut som = self.n[vom].modv;
        while let (Some(nvim), Some(nvip)) = (self.next_or_thread(vim), self.prev_or_thread(vip)) {
            vim = nvim;
            vip = nvip;
            vom = self.prev_or_thread(vom).unwrap_or(vom);
            vop = self.next_or_thread(vop).unwrap_or(vop);
            self.n[vop].ancestor = me;
            let shift = self.n[vim].y + sim - (self.n[vip].y + sip) + 1.0;
            if shift > 0.0 {
                let a = self.get_ancestor(vim, me, default_ancestor);
                self.move_subtree(a, me, shift);
                sip += shift;
                sop += shift;
            }
            sim += self.n[vim].modv;
            sip += self.n[vip].modv;
            som += self.n[vom].modv;
            sop += self.n[vop].modv;
        }
        if self.next_or_thread(vim).is_some() && self.next_or_thread(vop).is_none() {
            self.n[vop].thread = self.next_or_thread(vim);
            self.n[vop].modv += sim - sop;
        } else {
            if self.prev_or_thread(vip).is_some() && self.prev_or_thread(vom).is_none() {
                self.n[vom].thread = self.prev_or_thread(vip);
                self.n[vom].modv += sip - som;
            }
            default_ancestor = me;
        }
        default_ancestor
    }

    fn move_subtree(&mut self, me: usize, right: usize, shift: f32) {
        let subtrees = (self.n[right].child_index - self.n[me].child_index) as f32;
        if subtrees != 0.0 {
            self.n[right].change -= shift / subtrees;
            self.n[me].change += shift / subtrees;
        }
        self.n[right].shift += shift;
        self.n[right].y += shift;
        self.n[right].modv += shift;
    }

    fn get_ancestor(&self, me: usize, other: usize, default_ancestor: usize) -> usize {
        let a = self.n[me].ancestor;
        let siblings = self.n[other].parent.map(|p| &self.n[p].children);
        if siblings.is_some_and(|s| s.contains(&a)) {
            a
        } else {
            default_ancestor
        }
    }
}

fn layout(t: &mut Tree, root: usize) {
    let positions = {
        let mut l = Layout { t, n: Vec::new() };
        let r = l.new_node(root, None, None, 1, 0);
        l.first_walk(r);
        let y = l.n[r].y;
        let min = l.second_walk(r, 0.0, 0, y);
        if min < 0.0 {
            l.third_walk(r, -min);
        }
        l.n.iter().map(|p| (p.adv, p.x, p.y)).collect::<Vec<_>>()
    };
    for (adv, x, y) in positions {
        if let Some(d) = &mut t.advs[adv].display {
            d.x = x as f32;
            d.y = y;
        }
    }
}

// ------------------------------------------------------------------ progress

#[derive(Default)]
pub struct Progress {
    /// advancement → criterion → obtained (ms since epoch)
    pub crit: HashMap<usize, BTreeMap<String, i64>>,
    /// unlocked recipes (ids without namespace)
    pub recipes: BTreeSet<String>,
    pub new_recipes: Vec<String>,
    visible: HashSet<usize>,
    changed: BTreeSet<usize>,
    roots_changed: BTreeSet<usize>,
    synced: bool,
}

impl Progress {
    pub fn obtained(&self, adv: usize, crit: &str) -> bool {
        self.crit.get(&adv).is_some_and(|m| m.contains_key(crit))
    }

    pub fn is_done(&self, t: &Tree, adv: usize) -> bool {
        let a = &t.advs[adv];
        !a.requirements.is_empty() && a.requirements.iter().all(|g| g.iter().any(|c| self.obtained(adv, c)))
    }

    /// (requirement groups met, total)
    pub fn counts(&self, t: &Tree, adv: usize) -> (usize, usize) {
        let a = &t.advs[adv];
        (a.requirements.iter().filter(|g| g.iter().any(|c| self.obtained(adv, c))).count(), a.requirements.len())
    }

    pub fn to_json(&self, t: &Tree) -> Value {
        let mut out = serde_json::Map::new();
        let mut ids: Vec<&usize> = self.crit.keys().collect();
        ids.sort_by_key(|i| &t.advs[**i].id);
        for &i in ids {
            let m = &self.crit[&i];
            if m.is_empty() {
                continue;
            }
            let crit: serde_json::Map<String, Value> = m.iter().map(|(k, v)| (k.clone(), json!(fmt_time(*v)))).collect();
            out.insert(t.advs[i].id.clone(), json!({ "criteria": crit, "done": self.is_done(t, i) }));
        }
        out.insert("rime:recipes".into(), json!(self.recipes));
        out.insert("DataVersion".into(), json!(crate::world::chunk::DATA_VERSION));
        Value::Object(out)
    }

    pub fn from_json(t: &Tree, v: &Value) -> Progress {
        let mut p = Progress::default();
        let Some(o) = v.as_object() else { return p };
        for (k, e) in o {
            if k == "rime:recipes" {
                p.recipes = e.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
                continue;
            }
            let Some(&i) = t.ids.get(k) else { continue };
            let Some(c) = e.get("criteria").and_then(|c| c.as_object()) else { continue };
            let m: BTreeMap<String, i64> = c.iter().filter(|(n, _)| t.advs[i].criteria.iter().any(|x| &x.name == *n)).map(|(n, ts)| (n.clone(), ts.as_str().and_then(parse_time).unwrap_or(0))).collect();
            p.crit.insert(i, m);
        }
        p
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `yyyy-MM-dd HH:mm:ss Z`, as vanilla writes criterion times.
pub fn fmt_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} +0000", rem / 3600, rem / 60 % 60, rem % 60)
}

fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn parse_time(s: &str) -> Option<i64> {
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut secs = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se;
    if let Some(z) = s.get(20..25) {
        let sign = if z.starts_with('-') { -1 } else { 1 };
        let zh = z.get(1..3).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
        let zm = z.get(3..5).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
        secs -= sign * (zh * 3600 + zm * 60);
    }
    Some(secs * 1000)
}

pub fn path(world: &Path, u: uuid::Uuid) -> PathBuf {
    world.join("advancements").join(format!("{}.json", u.hyphenated()))
}

pub fn load_player(world: &Path, u: uuid::Uuid) -> Progress {
    let t = tree();
    std::fs::read_to_string(path(world, u)).ok().and_then(|s| serde_json::from_str(&s).ok()).map(|v| Progress::from_json(t, &v)).unwrap_or_default()
}

pub fn save_player(world: &Path, p: &Player) -> anyhow::Result<()> {
    let target = path(world, p.uuid);
    std::fs::create_dir_all(target.parent().unwrap())?;
    let body = serde_json::to_vec_pretty(&p.adv.lock().to_json(tree()))?;
    let tmp = target.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(tmp, target)?;
    Ok(())
}

// ------------------------------------------------------------------ visibility + packet

#[derive(Clone, Copy, PartialEq)]
enum Rule {
    Show,
    Hide,
    NoChange,
}

/// `AdvancementVisibilityEvaluator`
fn visibility(t: &Tree, prog: &Progress, root: usize, out: &mut Vec<(usize, bool)>) {
    fn rule(t: &Tree, adv: usize, done: bool) -> Rule {
        match &t.advs[adv].display {
            None => Rule::Hide,
            Some(_) if done => Rule::Show,
            Some(d) if d.hidden => Rule::Hide,
            Some(_) => Rule::NoChange,
        }
    }
    fn walk(t: &Tree, prog: &Progress, node: usize, stack: &mut Vec<Rule>, out: &mut Vec<(usize, bool)>) -> bool {
        let done = prog.is_done(t, node);
        let r = rule(t, node, done);
        let mut visible = done;
        stack.push(r);
        for &c in &t.children[node] {
            visible |= walk(t, prog, c, stack, out);
        }
        let show = visible || stack.iter().rev().take(3).find(|r| **r != Rule::NoChange).is_some_and(|r| *r == Rule::Show);
        stack.pop();
        out.push((node, show));
        visible
    }
    let mut stack = vec![Rule::NoChange; 3];
    walk(t, prog, root, &mut stack, out);
}

fn write_component(b: &mut Vec<u8>, v: &Value) {
    b.raw(&crate::nbt::Tag::from_json(v).to_network_bytes());
}

fn write_adv(b: &mut Vec<u8>, t: &Tree, i: usize) {
    let a = &t.advs[i];
    b.str(&a.id);
    match a.parent {
        Some(p) => {
            b.bool(true).str(&t.advs[p].id);
        }
        None => {
            b.bool(false);
        }
    }
    match &a.display {
        Some(d) => {
            b.bool(true);
            write_component(b, &d.title);
            write_component(b, &d.description);
            b.vi(d.icon.0).vi(d.icon.1).vi(0).vi(0);
            b.vi(d.frame);
            let flags = d.background.is_some() as i32 | (d.show_toast as i32) << 1 | (d.hidden as i32) << 2;
            b.i32(flags);
            if let Some(bg) = &d.background {
                b.str(bg);
            }
            b.f32(d.x).f32(d.y);
        }
        None => {
            b.bool(false);
        }
    }
    b.vi(a.requirements.len() as i32);
    for g in &a.requirements {
        b.vi(g.len() as i32);
        for c in g {
            b.str(c);
        }
    }
    b.bool(a.telemetry);
}

fn write_progress(b: &mut Vec<u8>, t: &Tree, prog: &Progress, i: usize) {
    b.str(&t.advs[i].id);
    let a = &t.advs[i];
    b.vi(a.criteria.len() as i32);
    for c in &a.criteria {
        b.str(&c.name);
        match prog.crit.get(&i).and_then(|m| m.get(&c.name)) {
            Some(ms) => {
                b.bool(true).i64(*ms);
            }
            None => {
                b.bool(false);
            }
        }
    }
}

impl Progress {
    /// `PlayerAdvancements.flushDirty`: the `update_advancements` body, if anything changed.
    pub fn flush(&mut self, show: bool) -> Option<Vec<u8>> {
        let t = tree();
        let reset = !self.synced;
        let roots: Vec<usize> = if reset {
            (0..t.advs.len()).filter(|i| t.advs[*i].parent.is_none()).collect()
        } else {
            std::mem::take(&mut self.roots_changed).into_iter().collect()
        };
        if !reset && roots.is_empty() && self.changed.is_empty() {
            return None;
        }
        self.synced = true;
        self.roots_changed.clear();
        let mut added = Vec::new();
        let mut removed = Vec::new();
        for r in roots {
            let mut out = Vec::new();
            visibility(t, self, r, &mut out);
            for (adv, vis) in out {
                if vis && self.visible.insert(adv) {
                    added.push(adv);
                    self.changed.insert(adv);
                } else if !vis && self.visible.remove(&adv) {
                    removed.push(adv);
                }
            }
        }
        // parents before children (the client links them as they come)
        added.sort_by_key(|i| depth(t, *i));
        let progress: Vec<usize> = std::mem::take(&mut self.changed).into_iter().filter(|i| self.visible.contains(i)).collect();
        if !reset && added.is_empty() && removed.is_empty() && progress.is_empty() {
            return None;
        }
        let mut b = Vec::new();
        b.bool(reset);
        b.vi(added.len() as i32);
        for &i in &added {
            write_adv(&mut b, t, i);
        }
        b.vi(removed.len() as i32);
        for &i in &removed {
            b.str(&t.advs[i].id);
        }
        b.vi(progress.len() as i32);
        for &i in &progress {
            write_progress(&mut b, t, self, i);
        }
        b.bool(show);
        Some(b)
    }

    /// Forces a full resend (`reset`) on the next flush.
    pub fn resync(&mut self) {
        self.synced = false;
        self.visible.clear();
    }

    fn mark(&mut self, t: &Tree, adv: usize) {
        self.changed.insert(adv);
        self.roots_changed.insert(root_of(t, adv));
    }
}

fn depth(t: &Tree, mut i: usize) -> usize {
    let mut d = 0;
    while let Some(p) = t.advs[i].parent {
        i = p;
        d += 1;
    }
    d
}

// ------------------------------------------------------------------ granting

impl Game {
    /// `PlayerAdvancements.award`: grants one criterion; rewards and the
    /// announcement when that completes the advancement.
    pub fn award_criterion(&self, p: &Arc<Player>, adv: usize, crit: &str) -> bool {
        let t = tree();
        let completed = {
            let mut prog = p.adv.lock();
            let was = prog.is_done(t, adv);
            if prog.obtained(adv, crit) || !t.advs[adv].criteria.iter().any(|c| c.name == crit) {
                return false;
            }
            prog.crit.entry(adv).or_default().insert(crit.to_string(), now_ms());
            prog.mark(t, adv);
            !was && prog.is_done(t, adv)
        };
        if completed {
            self.advancement_done(p, adv);
        }
        self.flush_advancements(p);
        true
    }

    /// Revokes one criterion (`/advancement revoke`).
    pub fn revoke_criterion(&self, p: &Arc<Player>, adv: usize, crit: &str) -> bool {
        let t = tree();
        let mut prog = p.adv.lock();
        let removed = prog.crit.get_mut(&adv).is_some_and(|m| m.remove(crit).is_some());
        if removed {
            prog.mark(t, adv);
        }
        drop(prog);
        if removed {
            self.flush_advancements(p);
        }
        removed
    }

    fn advancement_done(&self, p: &Arc<Player>, adv: usize) {
        let t = tree();
        let a = &t.advs[adv];
        // AdvancementRewards.grant
        if a.xp > 0 {
            p.give_xp(a.xp);
        }
        if !a.recipes.is_empty() {
            self.unlock_recipes(p, &a.recipes);
        }
        if let Some(d) = &a.display {
            if d.announce && super::rules::on("show_advancement_messages") {
                let color = if d.frame == 1 { "dark_purple" } else { "green" };
                let kind = ["task", "challenge", "goal"][d.frame as usize];
                let mut title = d.title.clone();
                if let Value::Object(o) = &mut title {
                    o.insert("color".into(), json!(color));
                } else {
                    title = json!({ "text": "", "extra": [title], "color": color });
                }
                let hover = json!({ "text": "", "extra": [title.clone(), "\n", d.description.clone()] });
                let name = json!({ "translate": "chat.square_brackets", "with": [title], "color": color, "hover_event": { "action": "show_text", "value": hover } });
                let msg = json!({ "translate": format!("chat.type.advancement.{kind}"), "with": [{ "text": p.name }, name] });
                let mut b = crate::nbt::Tag::from_json(&msg).to_network_bytes();
                b.bool(false);
                let pkt = self.encode("system_chat", &b);
                match crate::command::server() {
                    Some(st) => st.broadcast_all(pkt),
                    None => self.broadcast(pkt),
                }
            }
        }
    }

    /// Sends pending advancement changes to the player.
    pub fn flush_advancements(&self, p: &Arc<Player>) {
        let body = p.adv.lock().flush(true);
        if let Some(b) = body {
            p.send(self.encode("update_advancements", &b));
        }
        self.flush_recipes(p);
    }

    /// Grants every criterion of an advancement.
    pub fn grant_all(&self, p: &Arc<Player>, adv: usize) -> bool {
        let names: Vec<String> = tree().advs[adv].criteria.iter().map(|c| c.name.clone()).collect();
        let mut any = false;
        for n in names {
            any |= self.award_criterion(p, adv, &n);
        }
        any
    }

    pub fn revoke_all(&self, p: &Arc<Player>, adv: usize) -> bool {
        let names: Vec<String> = tree().advs[adv].criteria.iter().map(|c| c.name.clone()).collect();
        let mut any = false;
        for n in names {
            any |= self.revoke_criterion(p, adv, &n);
        }
        any
    }

    /// Runs a trigger: every unfinished criterion listening for it whose
    /// `player` conditions hold and for which `test` accepts the conditions.
    pub fn trigger(&self, p: &Arc<Player>, trigger: &str, test: impl Fn(&Value, &Eval) -> bool) {
        let t = tree();
        let Some(list) = t.by_trigger.get(trigger) else { return };
        let pending: Vec<(usize, usize)> = {
            let prog = p.adv.lock();
            list.iter().copied().filter(|(a, c)| !prog.obtained(*a, &t.advs[*a].criteria[*c].name) && !prog.is_done(t, *a)).collect()
        };
        if pending.is_empty() {
            return;
        }
        let ev = Eval { g: self, p };
        let mut hits = Vec::new();
        for (a, c) in pending {
            let cond = &t.advs[a].criteria[c].conditions;
            if ev.ctx_entity(cond.get("player"), &ev.me()) && test(cond, &ev) {
                hits.push((a, t.advs[a].criteria[c].name.clone()));
            }
        }
        for (a, c) in hits {
            self.award_criterion(p, a, &c);
        }
    }
}

// ------------------------------------------------------------------ predicates

/// An entity as predicates see it.
#[derive(Clone)]
pub struct View {
    pub eid: i32,
    pub ty: String,
    pub pos: [f64; 3],
    pub dim: Dim,
    pub baby: bool,
    pub on_fire: bool,
    pub sneaking: bool,
    pub sprinting: bool,
    pub swimming: bool,
    pub flying: bool,
    pub on_ground: bool,
    pub eq: [Option<Stack>; 8],
    /// ("minecraft:cat/variant", "minecraft:black")
    pub variant: Option<(String, String)>,
    pub player: Option<Arc<Player>>,
}

pub struct Eval<'a> {
    pub g: &'a Game,
    pub p: &'a Arc<Player>,
}

/// `MinMaxBounds`: a number or `{min, max}`.
pub fn in_range(v: Option<&Value>, x: f64) -> bool {
    match v {
        None => true,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| (n - x).abs() < 1e-9),
        Some(o) => o.get("min").and_then(|m| m.as_f64()).is_none_or(|m| x >= m) && o.get("max").and_then(|m| m.as_f64()).is_none_or(|m| x <= m),
    }
}

impl Eval<'_> {
    pub fn me(&self) -> View {
        self.g.player_view(self.p)
    }

    /// `ContextAwarePredicate` (a list of loot conditions about `this`) or
    /// a plain `EntityPredicate`; absent means anything.
    pub fn ctx_entity(&self, pred: Option<&Value>, who: &View) -> bool {
        match pred {
            None | Some(Value::Null) => true,
            Some(Value::Array(conds)) => conds.iter().all(|c| self.loot_condition(c, who)),
            Some(p) => self.entity(p, who),
        }
    }

    pub fn ctx_entity_opt(&self, pred: Option<&Value>, who: Option<&View>) -> bool {
        match (pred, who) {
            (None | Some(Value::Null), _) => true,
            (Some(_), None) => false,
            (p, Some(w)) => self.ctx_entity(p, w),
        }
    }

    fn loot_condition(&self, c: &Value, who: &View) -> bool {
        let terms = || c.get("terms").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        match c.get("condition").and_then(|x| x.as_str()).map(strip).unwrap_or("") {
            "entity_properties" => c.get("predicate").is_none_or(|p| self.entity(p, who)),
            "inverted" => c.get("term").is_some_and(|t| !self.loot_condition(t, who)),
            "any_of" => terms().iter().any(|t| self.loot_condition(t, who)),
            "all_of" => terms().iter().all(|t| self.loot_condition(t, who)),
            "location_check" => {
                let off = |k: &str| c.get(format!("offset{k}").as_str()).and_then(|x| x.as_f64()).unwrap_or(0.0);
                c.get("predicate").is_none_or(|p| self.location(p, who.dim, [who.pos[0] + off("X"), who.pos[1] + off("Y"), who.pos[2] + off("Z")]))
            }
            "random_chance" => rand::random::<f64>() < c.get("chance").and_then(|x| x.as_f64()).unwrap_or(1.0),
            "weather_check" => {
                c.get("raining").and_then(|x| x.as_bool()).is_none_or(|r| r == self.g.raining()) && c.get("thundering").and_then(|x| x.as_bool()).is_none_or(|r| r == self.g.thundering())
            }
            "time_check" => in_range(c.get("value"), (self.g.day_time() % c.get("period").and_then(|x| x.as_u64()).filter(|x| *x > 0).unwrap_or(u64::MAX)) as f64),
            _ => false,
        }
    }

    fn type_ok(&self, t: Option<&Value>, ty: &str) -> bool {
        let one = |s: &str| match s.strip_prefix('#') {
            Some(tag) => self.g.data.entity_in(strip(tag), ty),
            None => strip(s) == ty,
        };
        match t {
            None => true,
            Some(Value::String(s)) => one(s),
            Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).any(one),
            _ => false,
        }
    }

    /// `EntityPredicate`
    pub fn entity(&self, p: &Value, e: &View) -> bool {
        let Some(o) = p.as_object() else { return true };
        for (k, v) in o {
            let ok = match k.as_str() {
                "type" => self.type_ok(Some(v), &e.ty),
                "location" => self.location(v, e.dim, e.pos),
                "stepping_on" => self.location(v, e.dim, [e.pos[0], e.pos[1] - 0.5, e.pos[2]]),
                "movement_affected_by" => self.location(v, e.dim, [e.pos[0], e.pos[1] - 0.5000001, e.pos[2]]),
                "distance" => {
                    let me = self.p.pose.lock().pos;
                    let d = [e.pos[0] - me[0], e.pos[1] - me[1], e.pos[2] - me[2]];
                    in_range(v.get("x"), d[0].abs())
                        && in_range(v.get("y"), d[1].abs())
                        && in_range(v.get("z"), d[2].abs())
                        && in_range(v.get("horizontal"), (d[0] * d[0] + d[2] * d[2]).sqrt())
                        && in_range(v.get("absolute"), (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt())
                }
                "flags" => {
                    let f = |n: &str, x: bool| v.get(n).and_then(|b| b.as_bool()).is_none_or(|b| b == x);
                    f("is_on_fire", e.on_fire) && f("is_sneaking", e.sneaking) && f("is_sprinting", e.sprinting) && f("is_swimming", e.swimming) && f("is_baby", e.baby) && f("is_on_ground", e.on_ground) && f("is_flying", e.flying)
                }
                "equipment" => {
                    let slots = [("mainhand", 0), ("offhand", 1), ("feet", 2), ("legs", 3), ("chest", 4), ("head", 5), ("body", 6), ("saddle", 7)];
                    slots.iter().all(|(n, i)| v.get(*n).is_none_or(|ip| self.item(Some(ip), e.eq[*i])))
                }
                "components" => v.as_object().is_none_or(|m| {
                    m.iter().all(|(ck, cv)| match (&e.variant, cv.as_str()) {
                        (Some((k2, val)), Some(want)) => k2 == ck && full(want) == *val,
                        _ => false,
                    })
                }),
                "vehicle" => self.g.vehicle_of(e.eid).and_then(|veh| self.g.view(veh)).is_some_and(|ve| self.entity(v, &ve)),
                "passenger" => self.g.passengers_of(e.eid).into_iter().filter_map(|x| self.g.view(x)).any(|pe| self.entity(v, &pe)),
                "type_specific" => self.type_specific(v, e),
                "nbt" | "effects" | "slots" | "periodic_tick" | "team" | "targeted_entity" | "predicates" => true,
                _ => true,
            };
            if !ok {
                return false;
            }
        }
        true
    }

    fn type_specific(&self, v: &Value, e: &View) -> bool {
        match v.get("type").and_then(|x| x.as_str()).map(strip).unwrap_or("") {
            "player" => {
                let Some(pl) = &e.player else { return false };
                if let Some(gm) = v.get("gamemode") {
                    let cur = ["survival", "creative", "adventure", "spectator"][pl.gamemode.load(std::sync::atomic::Ordering::Relaxed).clamp(0, 3) as usize];
                    let ok = match gm {
                        Value::String(s) => s == cur,
                        Value::Array(a) => a.iter().any(|x| x.as_str() == Some(cur)),
                        _ => true,
                    };
                    if !ok {
                        return false;
                    }
                }
                if let Some(l) = v.get("level") {
                    if !in_range(Some(l), pl.vit.lock().xp_level as f64) {
                        return false;
                    }
                }
                if let Some(look) = v.get("looking_at") {
                    return self.g.looked_at(pl).and_then(|x| self.g.view(x)).is_some_and(|t| self.entity(look, &t));
                }
                true
            }
            "lightning" => true,
            "slime" => true,
            _ => true,
        }
    }

    /// `LocationPredicate`
    pub fn location(&self, p: &Value, dim: Dim, pos: [f64; 3]) -> bool {
        let b = [pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32];
        if let Some(d) = p.get("dimension").and_then(|x| x.as_str()) {
            if full(d) != dim.key() {
                return false;
            }
        }
        if let Some(ps) = p.get("position") {
            if !(in_range(ps.get("x"), pos[0]) && in_range(ps.get("y"), pos[1]) && in_range(ps.get("z"), pos[2])) {
                return false;
            }
        }
        let g = if dim == self.g.dim { Some(self.g) } else { None };
        if let Some(bs) = p.get("biomes") {
            let Some(here) = g.and_then(|g| g.world.biome(b[0], b[1], b[2])) else { return false };
            let one = |s: &str| match s.strip_prefix('#') {
                Some(_) => false,
                None => strip(s) == strip(&here),
            };
            let ok = match bs {
                Value::String(s) => one(s),
                Value::Array(a) => a.iter().filter_map(|x| x.as_str()).any(one),
                _ => false,
            };
            if !ok {
                return false;
            }
        }
        if let Some(ss) = p.get("structures") {
            let here = crate::worldgen::generator_for(dim.index() as usize).map(|generator| generator.structures_at(b)).unwrap_or_default();
            let ok = match ss {
                Value::String(s) => here.iter().any(|h| strip(h) == strip(s)),
                Value::Array(a) => a.iter().filter_map(|x| x.as_str()).any(|s| here.iter().any(|h| strip(h) == strip(s))),
                _ => false,
            };
            if !ok {
                return false;
            }
        }
        if let Some(bp) = p.get("block") {
            let Some(state) = g.and_then(|g| g.world.get_block(b[0], b[1], b[2])) else { return false };
            if !self.block(bp, state) {
                return false;
            }
        }
        if let Some(fp) = p.get("fluid") {
            let Some(state) = g.and_then(|g| g.world.get_block(b[0], b[1], b[2])) else { return false };
            let fl = super::fluid::fluid_state(state).fluid();
            let name = match fl {
                Some(super::fluid::Fluid::Water) => "water",
                Some(super::fluid::Fluid::Lava) => "lava",
                None => "empty",
            };
            if let Some(f) = fp.get("fluids").and_then(|x| x.as_str()) {
                let ok = match f.strip_prefix('#') {
                    Some(tag) => strip(tag) == name,
                    None => strip(f).ends_with(name),
                };
                if !ok {
                    return false;
                }
            }
        }
        if let Some(sm) = p.get("smokey").and_then(|x| x.as_bool()) {
            let Some(g) = g else { return false };
            let smoky = (1..=5).any(|dy| g.world.get_block(b[0], b[1] - dy, b[2]).is_some_and(|s| s.name().ends_with("campfire") && s.prop("lit") == Some("true")));
            if smoky != sm {
                return false;
            }
        }
        if let Some(cs) = p.get("can_see_sky").and_then(|x| x.as_bool()) {
            let Some(g) = g else { return false };
            if g.world.surface_y(b[0], b[2]).is_none_or(|top| b[1] <= top) == cs {
                return false;
            }
        }
        true
    }

    /// `BlockPredicate`
    pub fn block(&self, p: &Value, state: BlockState) -> bool {
        let name = state.name();
        if let Some(bs) = p.get("blocks") {
            let one = |s: &str| match s.strip_prefix('#') {
                Some(tag) => self.g.data.block_in(strip(tag), name),
                None => strip(s) == name,
            };
            let ok = match bs {
                Value::String(s) => one(s),
                Value::Array(a) => a.iter().filter_map(|x| x.as_str()).any(one),
                _ => false,
            };
            if !ok {
                return false;
            }
        }
        if let Some(st) = p.get("state").and_then(|x| x.as_object()) {
            for (k, v) in st {
                let Some(cur) = state.prop(k) else { return false };
                let ok = match v {
                    Value::String(s) => cur == s,
                    o => cur.parse::<f64>().is_ok_and(|n| in_range(Some(o), n)),
                };
                if !ok {
                    return false;
                }
            }
        }
        true
    }

    /// `ItemPredicate`
    pub fn item(&self, p: Option<&Value>, s: Option<Stack>) -> bool {
        let Some(p) = p else { return true };
        let Some(s) = s.filter(|s| s.count > 0) else { return false };
        let name = Items::get().name(s.item);
        if let Some(items) = p.get("items") {
            let one = |x: &str| match x.strip_prefix('#') {
                Some(tag) => self.g.data.item_in(strip(tag), s.item),
                None => strip(x) == name,
            };
            let ok = match items {
                Value::String(x) => one(x),
                Value::Array(a) => a.iter().filter_map(|x| x.as_str()).any(one),
                _ => false,
            };
            if !ok {
                return false;
            }
        }
        if !in_range(p.get("count"), s.count as f64) {
            return false;
        }
        if let Some(preds) = p.get("predicates").and_then(|x| x.as_object()) {
            for (k, v) in preds {
                let ok = match strip(k) {
                    "enchantments" => v.as_array().is_none_or(|a| {
                        a.iter().all(|e| match e.get("enchantments").and_then(|x| x.as_str()) {
                            Some(n) => in_range(e.get("levels").or(Some(&json!({"min": 1}))), s.enchantment_level(strip(n)) as f64),
                            None => true,
                        })
                    }),
                    "jukebox_playable" => crate::components::defaults(s.item).raw.get("minecraft:jukebox_playable").is_some(),
                    "damage" => {
                        let d = s.damage_value() as f64;
                        in_range(v.get("damage"), d) && in_range(v.get("durability"), (s.max_damage() as f64) - d)
                    }
                    _ => true,
                };
                if !ok {
                    return false;
                }
            }
        }
        if let Some(comps) = p.get("components").and_then(|x| x.as_object()) {
            // only exact checks the server can see
            for (k, _) in comps {
                if !matches!(strip(k), "banner_patterns" | "pot_decorations") {
                    continue;
                }
                return false;
            }
        }
        true
    }

    /// `DamagePredicate` (`player_hurt_entity` / `entity_hurt_player`).
    pub fn damage(&self, p: Option<&Value>, dealt: f32, taken: f32, blocked: bool, src: &crate::game::combat::damage::DamageSource) -> bool {
        let Some(p) = p else { return true };
        if !in_range(p.get("dealt"), dealt as f64) || !in_range(p.get("taken"), taken as f64) {
            return false;
        }
        if p.get("blocked").and_then(|x| x.as_bool()).is_some_and(|b| b != blocked) {
            return false;
        }
        if let Some(t) = p.get("type") {
            if let Some(tags) = t.get("tags").and_then(|x| x.as_array()) {
                for tag in tags {
                    let id = tag.get("id").and_then(|x| x.as_str()).map(strip).unwrap_or("");
                    let expected = tag.get("expected").and_then(|x| x.as_bool()).unwrap_or(true);
                    if src.is(id) != expected {
                        return false;
                    }
                }
            }
            if let Some(d) = t.get("direct_entity") {
                let v = src.direct.as_ref().and_then(|c| self.g.view_any(c.eid));
                if !self.ctx_entity_opt(Some(d), v.as_ref()) {
                    return false;
                }
            }
            if let Some(d) = t.get("source_entity") {
                let v = src.causing.as_ref().and_then(|c| self.g.view_any(c.eid));
                if !self.ctx_entity_opt(Some(d), v.as_ref()) {
                    return false;
                }
            }
        }
        true
    }
}

// ------------------------------------------------------------------ views

impl Game {
    pub fn player_view(&self, p: &Arc<Player>) -> View {
        let pose = *p.pose.lock();
        let f = p.flags.load(std::sync::atomic::Ordering::Relaxed);
        use super::hurt::{F_FALL_FLYING, F_SNEAKING, F_SPRINTING, F_SWIMMING};
        View {
            eid: p.eid,
            ty: "player".into(),
            pos: pose.pos,
            dim: self.dim,
            baby: false,
            on_fire: p.vit.lock().fire > 0,
            sneaking: f & F_SNEAKING != 0,
            sprinting: f & F_SPRINTING != 0,
            swimming: f & F_SWIMMING != 0,
            flying: f & F_FALL_FLYING != 0,
            on_ground: pose.on_ground,
            eq: p.equipment(),
            variant: None,
            player: Some(p.clone()),
        }
    }

    /// A non-player entity's view.
    pub fn view(&self, eid: i32) -> Option<View> {
        if let Some(p) = self.players().into_iter().find(|p| p.eid == eid) {
            return Some(self.player_view(&p));
        }
        let ents = self.entities.lock();
        let e = ents.get(&eid)?;
        let mut v = View { eid, ty: e.type_name(), pos: e.pos, dim: self.dim, baby: false, on_fire: false, sneaking: false, sprinting: false, swimming: false, flying: false, on_ground: e.on_ground, eq: [None; 8], variant: None, player: None };
        if let super::entity::EKind::Mob(m) = &e.kind {
            v.baby = m.baby;
            v.on_fire = m.fire > 0;
            v.eq = m.eq;
            v.variant = variant_component(&v.ty, m.v.variant);
        }
        Some(v)
    }

    /// Any entity in any level.
    pub fn view_any(&self, eid: i32) -> Option<View> {
        if let Some(v) = self.view(eid) {
            return Some(v);
        }
        let st = crate::command::server()?;
        st.levels().iter().find_map(|g| g.view(eid))
    }

    /// The entity a player looks at within 64 blocks (`Player.isLookingAtMe` style, cone of ~0.9).
    pub fn looked_at(&self, p: &Arc<Player>) -> Option<i32> {
        let pose = *p.pose.lock();
        let eye = [pose.pos[0], pose.pos[1] + 1.62, pose.pos[2]];
        let (yaw, pitch) = ((pose.yaw as f64).to_radians(), (pose.pitch as f64).to_radians());
        let dir = [-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()];
        let mut best: Option<(f64, i32)> = None;
        for s in self.entity_snaps() {
            let c = [s.pos[0] - eye[0], s.pos[1] + s.eye.max(0.5) * 0.8 - eye[1], s.pos[2] - eye[2]];
            let d = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
            if d > 100.0 || d < 0.1 {
                continue;
            }
            let dot = (c[0] * dir[0] + c[1] * dir[1] + c[2] * dir[2]) / d;
            // a target several blocks across needs less precision far away
            if dot > 1.0 - 0.025 / d.max(1.0).sqrt() && best.is_none_or(|b| d < b.0) {
                best = Some((d, s.eid));
            }
        }
        best.map(|b| b.1)
    }
}

/// The `<type>/variant` component of a mob with a data-driven variant.
fn variant_component(ty: &str, variant: i32) -> Option<(String, String)> {
    let reg = match ty {
        "cat" => "minecraft:cat_variant",
        "frog" => "minecraft:frog_variant",
        "wolf" => "minecraft:wolf_variant",
        "pig" => "minecraft:pig_variant",
        "cow" => "minecraft:cow_variant",
        "chicken" => "minecraft:chicken_variant",
        _ => return None,
    };
    let name = crate::net::config_data::dynamic_name(reg, variant)?;
    Some((format!("minecraft:{ty}/variant"), name))
}

// ------------------------------------------------------------------ player state

impl Player {
    pub fn adv_done(&self, id: &str) -> bool {
        let t = tree();
        t.ids.get(&full(id)).is_some_and(|i| self.adv.lock().is_done(t, *i))
    }
}

/// Shared progress lock type for `Player::adv`.
pub type ProgressLock = Mutex<Progress>;

#[cfg(test)]
mod tests {
    use super::*;

    fn adv(parent: Option<&str>) -> Value {
        let mut v = json!({ "display": { "icon": { "id": "minecraft:stone" }, "title": "t", "description": "d" }, "criteria": { "a": { "trigger": "minecraft:impossible" } } });
        if let Some(p) = parent {
            v["parent"] = json!(p);
        }
        v
    }

    #[test]
    fn layout_and_progress() {
        let raw = vec![
            ("minecraft:r".to_string(), adv(None)),
            ("minecraft:r/a".to_string(), adv(Some("minecraft:r"))),
            ("minecraft:r/b".to_string(), adv(Some("minecraft:r"))),
            ("minecraft:r/a1".to_string(), adv(Some("minecraft:r/a"))),
            ("minecraft:r/a2".to_string(), adv(Some("minecraft:r/a"))),
        ];
        let t = build(raw);
        let pos = |id: &str| t.advs[t.ids[id]].display.as_ref().map(|d| (d.x, d.y)).unwrap();
        assert_eq!(pos("minecraft:r").0, 0.0);
        assert_eq!(pos("minecraft:r/a").0, 1.0);
        assert_eq!(pos("minecraft:r/a1").0, 2.0);
        // siblings never overlap and the parent sits between its children
        assert!(pos("minecraft:r/a1").1 != pos("minecraft:r/a2").1);
        assert!(pos("minecraft:r/b").1 > pos("minecraft:r/a").1);
        let a = pos("minecraft:r/a");
        assert!((a.1 - (pos("minecraft:r/a1").1 + pos("minecraft:r/a2").1) / 2.0).abs() < 1e-6);
        let mut p = Progress::default();
        let i = t.ids["minecraft:r/a"];
        assert!(!p.is_done(&t, i));
        p.crit.entry(i).or_default().insert("a".into(), 1_700_000_000_000);
        assert!(p.is_done(&t, i));
        let j = p.to_json(&t);
        let q = Progress::from_json(&t, &j);
        assert!(q.is_done(&t, i));
        assert_eq!(parse_time(&fmt_time(1_700_000_000_000)), Some(1_700_000_000_000));
        assert_eq!(fmt_time(0), "1970-01-01 00:00:00 +0000");
    }

    /// The real vanilla advancements (needs RIME_SERVER_DIR with the cache).
    #[test]
    fn vanilla_tree_loads() {
        let Ok(dir) = std::env::var("RIME_SERVER_DIR") else { return };
        let path = std::path::Path::new(&dir).join("cache/26.1/data/minecraft/advancement");
        if !path.exists() {
            return;
        }
        load(&path);
        let t = tree();
        assert!(t.advs.len() > 1500, "{}", t.advs.len());
        let roots: Vec<&str> = t.advs.iter().filter(|a| a.parent.is_none() && a.display.is_some()).map(|a| a.id.as_str()).collect();
        assert_eq!(roots.len(), 5, "{roots:?}");
        for a in &t.advs {
            if let Some(d) = &a.display {
                assert!(d.x >= 0.0 && d.y >= 0.0, "{} at {} {}", a.id, d.x, d.y);
            }
        }
        // no two advancements of a tab share a cell
        let mut cells = HashSet::new();
        for (i, a) in t.advs.iter().enumerate() {
            if let Some(d) = &a.display {
                assert!(cells.insert((root_of(t, i), d.x as i32, (d.y * 2.0) as i32)), "overlap at {}", a.id);
            }
        }
        let story = t.ids["minecraft:story/root"];
        let mut p = Progress::default();
        p.crit.entry(story).or_default().insert("crafting_table".into(), 1);
        p.mark(t, story);
        let b = p.flush(true).unwrap();
        assert!(b[0] == 1 && b.len() > 100);
        // nothing new afterwards
        assert!(p.flush(true).is_none());
    }

    #[test]
    fn visibility_rules() {
        let raw = vec![("minecraft:r".to_string(), adv(None)), ("minecraft:r/a".to_string(), adv(Some("minecraft:r"))), ("minecraft:r/a/b".to_string(), adv(Some("minecraft:r/a"))), ("minecraft:r/a/b/c".to_string(), adv(Some("minecraft:r/a/b")))];
        let t = build(raw);
        let p = Progress::default();
        let mut out = Vec::new();
        visibility(&t, &p, 0, &mut out);
        let vis: HashMap<usize, bool> = out.into_iter().collect();
        // nothing done: no SHOW rule above, so nothing is visible yet
        assert!(!vis[&0]);
        let mut p = Progress::default();
        p.crit.entry(0).or_default().insert("a".into(), 1);
        let mut out = Vec::new();
        visibility(&t, &p, 0, &mut out);
        let vis: HashMap<usize, bool> = out.into_iter().collect();
        assert!(vis[&0] && vis[&1] && vis[&2]);
        assert!(!vis[&3]);
    }
}
