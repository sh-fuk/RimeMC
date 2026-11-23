//! The command tree (brigadier `CommandDispatcher`): building, the
//! `commands` packet, and parsing a line into a chain of stages.

use super::args::{Arg, ArgType};
use super::reader::{tr, ParseErr, Reader};
use super::{Ctx, Source};
use crate::net::codec::Out;
use serde_json::{json, Value};
use std::collections::HashMap;

pub type Exec = fn(&mut Ctx) -> Result<i32, Value>;
/// `execute` modifiers: one source in, any number out.
pub type Fork = fn(&mut Ctx) -> Result<Vec<Source>, Value>;

pub enum Kind {
    Root,
    Literal,
    Arg(ArgType),
}

pub struct Node {
    pub name: &'static str,
    pub kind: Kind,
    pub children: Vec<usize>,
    pub exec: Option<Exec>,
    pub redirect: Option<usize>,
    pub fork: Option<Fork>,
    /// permission level needed
    pub level: i32,
}

pub struct Tree {
    pub nodes: Vec<Node>,
}

pub type Args = HashMap<&'static str, Arg>;

/// One step of a parsed line: the arguments seen and how the step changes
/// the sources (`execute as …`), or the command run at the end.
pub struct Stage {
    pub args: Args,
    pub fork: Option<Fork>,
}

pub struct Parsed {
    pub stages: Vec<Stage>,
    pub args: Args,
    pub exec: Exec,
}

pub struct B<'t> {
    t: &'t mut Tree,
    pub idx: usize,
    level: i32,
}

impl<'t> B<'t> {
    /// A builder adding children to an existing node.
    pub fn at(t: &'t mut Tree, idx: usize, level: i32) -> B<'t> {
        B { t, idx, level }
    }

    fn add(&mut self, name: &'static str, kind: Kind, f: impl FnOnce(&mut B)) -> usize {
        let idx = self.t.nodes.len();
        self.t.nodes.push(Node { name, kind, children: vec![], exec: None, redirect: None, fork: None, level: self.level });
        self.t.nodes[self.idx].children.push(idx);
        let mut b = B { t: self.t, idx, level: self.level };
        f(&mut b);
        idx
    }
    pub fn lit(&mut self, name: &'static str, f: impl FnOnce(&mut B)) -> usize {
        self.add(name, Kind::Literal, f)
    }
    pub fn arg(&mut self, name: &'static str, ty: ArgType, f: impl FnOnce(&mut B)) -> usize {
        self.add(name, Kind::Arg(ty), f)
    }
    pub fn exec(&mut self, e: Exec) {
        self.t.nodes[self.idx].exec = Some(e);
    }
    pub fn redirect(&mut self, to: usize, fork: Option<Fork>) {
        let n = &mut self.t.nodes[self.idx];
        n.redirect = Some(to);
        n.fork = fork;
    }
}

impl Tree {
    pub fn new() -> Tree {
        Tree { nodes: vec![Node { name: "", kind: Kind::Root, children: vec![], exec: None, redirect: None, fork: None, level: 0 }] }
    }

    /// A top-level command needing `level`.
    pub fn cmd(&mut self, name: &'static str, level: i32, f: impl FnOnce(&mut B)) -> usize {
        let mut b = B { t: self, idx: 0, level };
        b.lit(name, f)
    }

    pub fn root_child(&self, name: &str) -> Option<usize> {
        self.nodes[0].children.iter().copied().find(|c| self.nodes[*c].name == name)
    }

    /// The `commands` packet for someone with permission `level`, plus
    /// extra literal commands (plugins) taking a greedy string.
    pub fn packet(&self, level: i32, extra: &[String]) -> Vec<u8> {
        // visible nodes and their packet indices
        let mut index = vec![-1i32; self.nodes.len()];
        let mut order = Vec::new();
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            if index[i] >= 0 || self.nodes[i].level > level {
                continue;
            }
            index[i] = order.len() as i32;
            order.push(i);
            for c in self.nodes[i].children.iter().rev() {
                stack.push(*c);
            }
            if let Some(r) = self.nodes[i].redirect {
                stack.push(r);
            }
        }
        let extra: Vec<&String> = extra.iter().filter(|e| self.root_child(e).is_none()).collect();
        let base = order.len() as i32;
        let total = base + extra.len() as i32 * 2;
        let mut b = Vec::new();
        b.vi(total);
        for &i in &order {
            let n = &self.nodes[i];
            let kids: Vec<i32> = n.children.iter().map(|c| index[*c]).filter(|c| *c >= 0).collect();
            let mut flags = match n.kind {
                Kind::Root => 0u8,
                Kind::Literal => 1,
                Kind::Arg(_) => 2,
            };
            if n.exec.is_some() {
                flags |= 0x04;
            }
            let redirect = n.redirect.map(|r| index[r]).filter(|r| *r >= 0);
            if redirect.is_some() {
                flags |= 0x08;
            }
            let sugg = if let Kind::Arg(t) = &n.kind { t.suggestions() } else { None };
            if sugg.is_some() {
                flags |= 0x10;
            }
            if n.level >= 2 && !matches!(n.kind, Kind::Root) {
                flags |= 0x20;
            }
            b.u8(flags);
            let extra_kids = if i == 0 { extra.len() } else { 0 };
            b.vi((kids.len() + extra_kids) as i32);
            for k in kids {
                b.vi(k);
            }
            if i == 0 {
                for j in 0..extra.len() {
                    b.vi(base + j as i32 * 2);
                }
            }
            if let Some(r) = redirect {
                b.vi(r);
            }
            match &n.kind {
                Kind::Root => {}
                Kind::Literal => {
                    b.str(n.name);
                }
                Kind::Arg(t) => {
                    b.str(n.name);
                    t.write(&mut b);
                    if let Some(s) = sugg {
                        b.str(s);
                    }
                }
            }
        }
        for (j, name) in extra.iter().enumerate() {
            b.u8(0x01 | 0x04).vi(1).vi(base + j as i32 * 2 + 1).str(name);
            b.u8(0x02 | 0x04).vi(0).str("args");
            ArgType::Greedy.write(&mut b);
        }
        b.vi(0);
        b
    }

    /// Parses a whole line (no leading slash).
    pub fn parse(&self, line: &str, level: i32) -> Result<Parsed, ParseErr> {
        let mut r = Reader::new(line);
        let mut best: Option<ParseErr> = None;
        if let Some(p) = self.parse_children(0, &mut r, Args::new(), Vec::new(), level, &mut best) {
            return Ok(p);
        }
        Err(best.unwrap_or_else(|| ParseErr { msg: tr("command.unknown.command", vec![]), at: 0 }))
    }

    fn note(best: &mut Option<ParseErr>, e: ParseErr) {
        if best.as_ref().is_none_or(|b| e.at >= b.at) {
            *best = Some(e);
        }
    }

    fn parse_children(&self, node: usize, r: &mut Reader, args: Args, stages: Vec<Stage>, level: i32, best: &mut Option<ParseErr>) -> Option<Parsed> {
        let start = r.pos;
        let n = &self.nodes[node];
        // literals first, like brigadier's relevant-node order
        let mut lits = Vec::new();
        let mut argn = Vec::new();
        for &c in &n.children {
            if self.nodes[c].level > level {
                continue;
            }
            match self.nodes[c].kind {
                Kind::Literal => lits.push(c),
                _ => argn.push(c),
            }
        }
        let word = {
            let mut rr = r.clone();
            rr.read_word().to_string()
        };
        for c in lits {
            if self.nodes[c].name == word {
                let mut rr = r.clone();
                rr.pos = start + word.len();
                let mut a = args.clone();
                // literals passed are visible to commands (`if` / `unless`, `<=` …)
                a.entry(self.nodes[c].name).or_insert(Arg::Lit);
                if let Some(p) = self.after(c, &mut rr, a, clone_stages(&stages), level, best) {
                    *r = rr;
                    return Some(p);
                }
            }
        }
        for c in argn {
            let Kind::Arg(t) = &self.nodes[c].kind else { continue };
            let mut rr = r.clone();
            match t.parse(&mut rr) {
                Ok(v) => {
                    if rr.can_read() && rr.peek() != ' ' {
                        Self::note(best, ParseErr { msg: tr("command.expected.separator", vec![]), at: rr.pos });
                        continue;
                    }
                    let mut a = args.clone();
                    a.insert(self.nodes[c].name, v);
                    if let Some(p) = self.after(c, &mut rr, a, clone_stages(&stages), level, best) {
                        *r = rr;
                        return Some(p);
                    }
                }
                Err(e) => Self::note(best, e),
            }
        }
        if n.children.iter().all(|c| self.nodes[*c].level > level) || (node == 0) {
            Self::note(best, ParseErr { msg: tr(if node == 0 { "command.unknown.command" } else { "command.unknown.argument" }, vec![]), at: start });
        } else {
            Self::note(best, ParseErr { msg: tr("command.unknown.argument", vec![]), at: start });
        }
        None
    }

    /// After a node's own token: end of input, or a space and more.
    fn after(&self, node: usize, r: &mut Reader, args: Args, mut stages: Vec<Stage>, level: i32, best: &mut Option<ParseErr>) -> Option<Parsed> {
        let n = &self.nodes[node];
        if !r.can_read() {
            return match n.exec {
                Some(exec) => Some(Parsed { stages, args, exec }),
                None => {
                    Self::note(best, ParseErr { msg: tr("command.unknown.command", vec![]), at: r.pos });
                    None
                }
            };
        }
        r.skip(); // the space
        if !r.can_read() {
            Self::note(best, ParseErr { msg: tr("command.unknown.argument", vec![]), at: r.pos });
            return None;
        }
        match n.redirect {
            Some(to) => {
                stages.push(Stage { args, fork: n.fork });
                self.parse_children(to, r, Args::new(), stages, level, best)
            }
            None => self.parse_children(node, r, args, stages, level, best),
        }
    }
}

fn clone_stages(s: &[Stage]) -> Vec<Stage> {
    s.iter().map(|x| Stage { args: x.args.clone(), fork: x.fork }).collect()
}

/// `command.context.here` style feedback for a parse error.
pub fn error_lines(line: &str, e: &ParseErr) -> Vec<Value> {
    let at = e.at.min(line.len());
    let at = (0..=at).rev().find(|i| line.is_char_boundary(*i)).unwrap_or(0);
    let from = line[..at].char_indices().rev().nth(9).map(|(i, _)| i).unwrap_or(0);
    let ctx = format!("{}{}", if from > 0 { "..." } else { "" }, &line[from..at]);
    let rest = &line[at..];
    let mut parts = vec![json!({ "text": ctx, "color": "gray" })];
    if !rest.is_empty() {
        parts.push(json!({ "text": rest, "color": "red", "underlined": true }));
    }
    parts.push(json!({ "translate": "command.context.here", "color": "red", "italic": true }));
    vec![json!({ "text": "", "color": "red", "extra": [e.msg.clone()] }), json!({ "text": "", "extra": parts })]
}
