//! Gameplay data from the official vanilla data pack: items, tags, block loot
//! tables and crafting recipes.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use rand::Rng;
use serde_json::Value;

use crate::registry::{BlockRegistry, BlockState};
use crate::vanilla::VanillaData;

// ------------------------------------------------------------------ items

pub struct Items {
    names: Vec<String>,
    stack: Vec<u8>,
    by_name: HashMap<String, i32>,
}

impl Items {
    pub fn get() -> &'static Items {
        static I: OnceLock<Items> = OnceLock::new();
        I.get_or_init(|| {
            let mut names = Vec::new();
            let mut stack = Vec::new();
            for line in include_str!("../assets/26.1/items.tsv").lines().filter(|l| !l.is_empty()) {
                let (n, s) = line.split_once('\t').expect("items.tsv");
                names.push(n.to_string());
                stack.push(s.parse().expect("items.tsv stack"));
            }
            let by_name = names.iter().enumerate().map(|(i, n)| (n.clone(), i as i32)).collect();
            Items { names, stack, by_name }
        })
    }
    pub fn id(&self, name: &str) -> Option<i32> {
        self.by_name.get(name.trim_start_matches("minecraft:")).copied()
    }
    pub fn name(&self, id: i32) -> &str {
        self.names.get(id as usize).map(|s| s.as_str()).unwrap_or("air")
    }
    pub fn max_stack(&self, id: i32) -> u8 {
        self.stack.get(id as usize).copied().unwrap_or(64).max(1)
    }
    pub fn valid(&self, id: i32) -> bool {
        id > 0 && (id as usize) < self.names.len()
    }

    /// Block placed by an item (most share the block's name).
    pub fn block_for(&self, id: i32) -> Option<BlockState> {
        let name = self.name(id);
        let block = match name {
            "redstone" => "redstone_wire",
            "string" => "tripwire",
            "wheat_seeds" => "wheat",
            "beetroot_seeds" => "beetroots",
            "carrot" => "carrots",
            "potato" => "potatoes",
            "melon_seeds" => "melon_stem",
            "pumpkin_seeds" => "pumpkin_stem",
            "sweet_berries" => "sweet_berry_bush",
            "cocoa_beans" => "cocoa",
            "glow_berries" => "cave_vines",
            "torchflower_seeds" => "torchflower_crop",
            "pitcher_pod" => "pitcher_crop",
            "air" => return None,
            n => n,
        };
        BlockRegistry::get().by_name(block).map(|b| BlockState(b.default))
    }
}

// ------------------------------------------------------------------ stacks

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stack {
    pub item: i32,
    pub count: u8,
}

impl Stack {
    pub fn new(item: i32, count: u8) -> Self {
        Self { item, count }
    }
    pub fn max(&self) -> u8 {
        Items::get().max_stack(self.item)
    }
}

// ------------------------------------------------------------------ data

pub struct Ingredient(pub HashSet<i32>);

pub enum RecipeShape {
    Shaped { w: usize, h: usize, cells: Vec<Option<Ingredient>> },
    Shapeless(Vec<Ingredient>),
}

pub struct Recipe {
    pub shape: RecipeShape,
    pub result: Stack,
}

pub struct GameData {
    pub item_tags: HashMap<String, HashSet<i32>>,
    /// block tags as sets of block names (without namespace)
    pub block_tags: HashMap<String, HashSet<String>>,
    loot: HashMap<String, Value>,
    pub recipes: Vec<Recipe>,
    /// furnace / blast furnace / smoker / campfire recipes
    pub cooking: Vec<Cooking>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CookKind {
    Smelting,
    Blasting,
    Smoking,
    Campfire,
}

pub struct Cooking {
    pub kind: CookKind,
    pub input: Ingredient,
    pub result: Stack,
    pub time: i32,
    pub xp: f32,
}

fn strip(s: &str) -> &str {
    s.trim_start_matches("minecraft:")
}

/// Load `tags/<dir>` and resolve nested `#tag` references into plain names.
fn load_tags(v: &VanillaData, dir: &str) -> anyhow::Result<HashMap<String, HashSet<String>>> {
    let mut raw: HashMap<String, Vec<String>> = HashMap::new();
    for (key, path) in v.entries(&format!("tags/{dir}")) {
        let json: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        let vals = json.get("values").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        let list = vals
            .iter()
            .filter_map(|e| match e {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => Some(o.get("id")?.as_str()?.to_string()),
                _ => None,
            })
            .collect();
        // tag names are kept without the namespace ("planks", "mineable/pickaxe")
        raw.insert(strip(&key).to_string(), list);
    }
    fn resolve(tag: &str, raw: &HashMap<String, Vec<String>>, stack: &mut Vec<String>, out: &mut HashSet<String>) {
        let Some(list) = raw.get(tag) else { return };
        if stack.iter().any(|t| t == tag) {
            return;
        }
        stack.push(tag.to_string());
        for e in list {
            match e.strip_prefix('#') {
                Some(t) => resolve(strip(t), raw, stack, out),
                None => {
                    out.insert(strip(e).to_string());
                }
            }
        }
        stack.pop();
    }
    let mut out = HashMap::new();
    for k in raw.keys() {
        let mut set = HashSet::new();
        resolve(k, &raw, &mut Vec::new(), &mut set);
        out.insert(k.clone(), set);
    }
    Ok(out)
}

impl GameData {
    pub fn load(v: &VanillaData) -> anyhow::Result<Self> {
        let items = Items::get();
        let item_tags = load_tags(v, "item")?.into_iter().map(|(k, names)| (k, names.iter().filter_map(|n| items.id(n)).collect())).collect();
        let block_tags = load_tags(v, "block")?;
        let mut loot = HashMap::new();
        for (key, path) in v.entries("loot_table/blocks") {
            let json: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            loot.insert(strip(&key).to_string(), json);
        }
        // chest, entity, gameplay… tables keep their folder ("chests/igloo_chest")
        for dir in ["chests", "entities", "gameplay", "archaeology", "dispensers", "spawners", "equipment", "shearing"] {
            for (key, path) in v.entries(&format!("loot_table/{dir}")) {
                let json: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
                loot.insert(format!("{dir}/{}", strip(&key)), json);
            }
        }
        let mut d = GameData { item_tags, block_tags, loot, recipes: Vec::new(), cooking: Vec::new() };
        for (_, path) in v.entries("recipe") {
            let json: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            if let Some(r) = d.parse_recipe(&json) {
                d.recipes.push(r);
            } else if let Some(c) = d.parse_cooking(&json) {
                d.cooking.push(c);
            }
        }
        Ok(d)
    }

    pub fn empty() -> Self {
        GameData { item_tags: HashMap::new(), block_tags: HashMap::new(), loot: HashMap::new(), recipes: Vec::new(), cooking: Vec::new() }
    }

    pub fn block_in(&self, tag: &str, block: &str) -> bool {
        self.block_tags.get(tag).is_some_and(|s| s.contains(block))
    }

    pub fn item_in(&self, tag: &str, item: i32) -> bool {
        self.item_tags.get(tag).is_some_and(|s| s.contains(&item))
    }

    fn ingredient(&self, v: &Value) -> Option<Ingredient> {
        let items = Items::get();
        let mut set = HashSet::new();
        let mut add = |s: &str| {
            if let Some(t) = s.strip_prefix('#') {
                if let Some(ids) = self.item_tags.get(strip(t)) {
                    set.extend(ids.iter().copied());
                }
            } else if let Some(id) = items.id(s) {
                set.insert(id);
            }
        };
        match v {
            Value::String(s) => add(s),
            Value::Array(a) => a.iter().filter_map(|x| x.as_str()).for_each(&mut add),
            Value::Object(o) => {
                // older forms: {"item": ..} / {"tag": ..}
                if let Some(s) = o.get("item").and_then(|x| x.as_str()) {
                    add(s)
                }
                if let Some(s) = o.get("tag").and_then(|x| x.as_str()) {
                    add(&format!("#{s}"))
                }
            }
            _ => {}
        }
        (!set.is_empty()).then_some(Ingredient(set))
    }

    fn parse_cooking(&self, j: &Value) -> Option<Cooking> {
        let kind = match j.get("type")?.as_str()? {
            "minecraft:smelting" => CookKind::Smelting,
            "minecraft:blasting" => CookKind::Blasting,
            "minecraft:smoking" => CookKind::Smoking,
            "minecraft:campfire_cooking" => CookKind::Campfire,
            _ => return None,
        };
        let res = j.get("result")?;
        let item = Items::get().id(res.get("id")?.as_str()?)?;
        let count = res.get("count").and_then(|c| c.as_u64()).unwrap_or(1).clamp(1, 64) as u8;
        let input = self.ingredient(j.get("ingredient")?)?;
        let default_time = match kind {
            CookKind::Smelting => 200,
            CookKind::Blasting | CookKind::Smoking => 100,
            CookKind::Campfire => 100,
        };
        let time = j.get("cookingtime").and_then(|t| t.as_i64()).unwrap_or(default_time) as i32;
        let xp = j.get("experience").and_then(|t| t.as_f64()).unwrap_or(0.0) as f32;
        Some(Cooking { kind, input, result: Stack::new(item, count), time, xp })
    }

    /// The cooking recipe of `kind` for an input item.
    pub fn cook(&self, kind: CookKind, item: i32) -> Option<&Cooking> {
        self.cooking.iter().find(|c| c.kind == kind && c.input.0.contains(&item))
    }

    /// Rolls a non-block loot table ("chests/simple_dungeon",
    /// "entities/zombie"); items are merged into stacks.
    pub fn roll_loot(&self, table: &str, rng: &mut impl Rng) -> Vec<Stack> {
        let key = strip(table);
        let Some(t) = self.loot.get(key) else { return Vec::new() };
        let mut out = Vec::new();
        let ctx = LootCtx { block: crate::registry::blocks::AIR, tool: None, data: self };
        self.eval_table(t, &ctx, rng, &mut out, 0);
        out
    }

    fn parse_recipe(&self, j: &Value) -> Option<Recipe> {
        let ty = j.get("type")?.as_str()?;
        let res = j.get("result")?;
        let item = Items::get().id(res.get("id")?.as_str()?)?;
        let count = res.get("count").and_then(|c| c.as_u64()).unwrap_or(1).clamp(1, 64) as u8;
        let result = Stack::new(item, count);
        match ty {
            "minecraft:crafting_shaped" => {
                let pattern: Vec<&str> = j.get("pattern")?.as_array()?.iter().filter_map(|p| p.as_str()).collect();
                let key = j.get("key")?.as_object()?;
                let h = pattern.len();
                let w = pattern.iter().map(|p| p.chars().count()).max()?;
                if w == 0 || w > 3 || h == 0 || h > 3 {
                    return None;
                }
                let mut cells = Vec::with_capacity(w * h);
                for row in &pattern {
                    let chars: Vec<char> = row.chars().collect();
                    for x in 0..w {
                        let c = chars.get(x).copied().unwrap_or(' ');
                        if c == ' ' {
                            cells.push(None);
                        } else {
                            cells.push(Some(self.ingredient(key.get(&c.to_string())?)?));
                        }
                    }
                }
                Some(Recipe { shape: RecipeShape::Shaped { w, h, cells }, result })
            }
            "minecraft:crafting_shapeless" => {
                let ings: Option<Vec<Ingredient>> = j.get("ingredients")?.as_array()?.iter().map(|i| self.ingredient(i)).collect();
                let ings = ings?;
                if ings.is_empty() || ings.len() > 9 {
                    return None;
                }
                Some(Recipe { shape: RecipeShape::Shapeless(ings), result })
            }
            _ => None,
        }
    }

    /// Match a square crafting grid (`size` 2 or 3, row-major, item ids or None).
    pub fn craft(&self, grid: &[Option<i32>], size: usize) -> Option<Stack> {
        let filled: Vec<(usize, usize)> = (0..size * size).filter(|&i| grid[i].is_some()).map(|i| (i % size, i / size)).collect();
        if filled.is_empty() {
            return None;
        }
        let (minx, maxx) = (filled.iter().map(|p| p.0).min()?, filled.iter().map(|p| p.0).max()?);
        let (miny, maxy) = (filled.iter().map(|p| p.1).min()?, filled.iter().map(|p| p.1).max()?);
        let (bw, bh) = (maxx - minx + 1, maxy - miny + 1);
        let at = |x: usize, y: usize| grid[(miny + y) * size + minx + x];
        'recipes: for r in &self.recipes {
            match &r.shape {
                RecipeShape::Shaped { w, h, cells } => {
                    if *w != bw || *h != bh {
                        continue;
                    }
                    for mirror in [false, true] {
                        let ok = (0..bh).all(|y| {
                            (0..bw).all(|x| {
                                let cx = if mirror { bw - 1 - x } else { x };
                                match (&cells[y * w + cx], at(x, y)) {
                                    (None, None) => true,
                                    (Some(ing), Some(item)) => ing.0.contains(&item),
                                    _ => false,
                                }
                            })
                        });
                        if ok {
                            return Some(r.result);
                        }
                    }
                }
                RecipeShape::Shapeless(ings) => {
                    let items: Vec<i32> = grid.iter().flatten().copied().collect();
                    if items.len() != ings.len() {
                        continue;
                    }
                    // small bipartite match (≤ 9 items)
                    let mut used = vec![false; items.len()];
                    fn assign(i: usize, ings: &[Ingredient], items: &[i32], used: &mut [bool]) -> bool {
                        if i == ings.len() {
                            return true;
                        }
                        for k in 0..items.len() {
                            if !used[k] && ings[i].0.contains(&items[k]) {
                                used[k] = true;
                                if assign(i + 1, ings, items, used) {
                                    return true;
                                }
                                used[k] = false;
                            }
                        }
                        false
                    }
                    if assign(0, ings, &items, &mut used) {
                        return Some(r.result);
                    }
                    continue 'recipes;
                }
            }
        }
        None
    }

    // ------------------------------------------------------------ loot

    /// Can `tool` harvest `block` (vanilla "requires correct tool for drops")?
    pub fn can_harvest(&self, block: BlockState, tool: Option<i32>) -> bool {
        let name = block.name();
        let needs = |kind: &str| self.block_in(&format!("mineable/{kind}"), name);
        // blocks that need a tool to drop anything
        let required = needs("pickaxe") || matches!(name, "cobweb") || self.block_in("needs_stone_tool", name);
        if !required {
            return true;
        }
        let Some(tool) = tool else { return false };
        let tname = Items::get().name(tool);
        let kind_ok = (needs("pickaxe") && self.item_in("pickaxes", tool))
            || (needs("axe") && self.item_in("axes", tool))
            || (needs("shovel") && self.item_in("shovels", tool))
            || (needs("hoe") && self.item_in("hoes", tool))
            || (name == "cobweb" && (self.item_in("swords", tool) || tname == "shears"));
        if !kind_ok {
            return false;
        }
        let material = ["wooden", "stone", "copper", "iron", "golden", "diamond", "netherite"].into_iter().find(|m| tname.starts_with(&format!("{m}_")));
        match material {
            Some(m) => {
                let m = if m == "golden" { "gold" } else { m };
                !self.block_in(&format!("incorrect_for_{m}_tool"), name)
            }
            None => true,
        }
    }

    /// Items dropped when `block` is broken with `tool` (no enchantments).
    pub fn block_drops(&self, block: BlockState, tool: Option<i32>, rng: &mut impl Rng) -> Vec<Stack> {
        if !self.can_harvest(block, tool) {
            return Vec::new();
        }
        let Some(table) = self.loot.get(block.name()) else { return Vec::new() };
        let mut out = Vec::new();
        let ctx = LootCtx { block, tool, data: self };
        self.eval_table(table, &ctx, rng, &mut out, 0);
        // merge identical items, split by stack size
        let mut merged: Vec<Stack> = Vec::new();
        for s in out {
            match merged.iter_mut().find(|m| m.item == s.item && (m.count as u32 + s.count as u32) <= m.max() as u32) {
                Some(m) => m.count += s.count,
                None => merged.push(s),
            }
        }
        merged
    }

    fn eval_table(&self, table: &Value, ctx: &LootCtx, rng: &mut impl Rng, out: &mut Vec<Stack>, depth: u32) {
        if depth > 8 {
            return;
        }
        for pool in table.get("pools").and_then(|p| p.as_array()).into_iter().flatten() {
            if !conditions(pool, ctx, rng) {
                continue;
            }
            let rolls = pool.get("rolls").map(|r| number(r, rng)).unwrap_or(1.0).round().max(0.0) as u32;
            for _ in 0..rolls.min(64) {
                let mut cands: Vec<&Value> = Vec::new();
                for e in pool.get("entries").and_then(|e| e.as_array()).into_iter().flatten() {
                    expand(e, ctx, rng, &mut cands);
                }
                let pick = match cands.len() {
                    0 => continue,
                    1 => cands[0],
                    _ => {
                        let total: i64 = cands.iter().map(|c| c.get("weight").and_then(|w| w.as_i64()).unwrap_or(1).max(0)).sum();
                        if total <= 0 {
                            continue;
                        }
                        let mut r = rng.gen_range(0..total);
                        let mut chosen = cands[0];
                        for c in &cands {
                            r -= c.get("weight").and_then(|w| w.as_i64()).unwrap_or(1).max(0);
                            if r < 0 {
                                chosen = c;
                                break;
                            }
                        }
                        chosen
                    }
                };
                match pick.get("type").and_then(|t| t.as_str()) {
                    Some("minecraft:item") => {
                        let Some(item) = pick.get("name").and_then(|n| n.as_str()).and_then(|n| Items::get().id(n)) else { continue };
                        let mut count: i32 = 1;
                        let fns = pool.get("functions").and_then(|f| f.as_array()).into_iter().flatten().chain(pick.get("functions").and_then(|f| f.as_array()).into_iter().flatten());
                        for f in fns {
                            if conditions(f, ctx, rng) {
                                count = apply_fn(f, count, rng);
                            }
                        }
                        let max = Items::get().max_stack(item) as i32;
                        while count > 0 {
                            let n = count.min(max);
                            out.push(Stack::new(item, n as u8));
                            count -= n;
                        }
                    }
                    Some("minecraft:loot_table") => {
                        if let Some(t) = pick.get("value").and_then(|v| v.as_str()).and_then(|v| self.loot.get(strip(v).trim_start_matches("blocks/")).or_else(|| self.loot.get(strip(v)))) {
                            self.eval_table(t, ctx, rng, out, depth + 1);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

struct LootCtx<'a> {
    block: BlockState,
    tool: Option<i32>,
    data: &'a GameData,
}

fn expand<'v>(e: &'v Value, ctx: &LootCtx, rng: &mut impl Rng, out: &mut Vec<&'v Value>) -> bool {
    if !conditions(e, ctx, rng) {
        return false;
    }
    match e.get("type").and_then(|t| t.as_str()) {
        Some("minecraft:alternatives") => {
            for c in e.get("children").and_then(|c| c.as_array()).into_iter().flatten() {
                if expand(c, ctx, rng, out) {
                    return true;
                }
            }
            false
        }
        Some("minecraft:group") => {
            for c in e.get("children").and_then(|c| c.as_array()).into_iter().flatten() {
                expand(c, ctx, rng, out);
            }
            true
        }
        Some("minecraft:sequence") => {
            for c in e.get("children").and_then(|c| c.as_array()).into_iter().flatten() {
                if !expand(c, ctx, rng, out) {
                    break;
                }
            }
            true
        }
        Some("minecraft:empty") => true,
        _ => {
            out.push(e);
            true
        }
    }
}

fn conditions(v: &Value, ctx: &LootCtx, rng: &mut impl Rng) -> bool {
    v.get("conditions").and_then(|c| c.as_array()).into_iter().flatten().all(|c| condition(c, ctx, rng))
}

fn condition(c: &Value, ctx: &LootCtx, rng: &mut impl Rng) -> bool {
    let terms = |c: &Value| c.get("terms").and_then(|t| t.as_array()).cloned().unwrap_or_default();
    match c.get("condition").and_then(|t| t.as_str()).unwrap_or("") {
        "minecraft:survives_explosion" => true,
        // mob loot is rolled for kills; no looting / fire / baby state yet
        "minecraft:killed_by_player" => true,
        "minecraft:random_chance" => {
            let p = c.get("chance").map(|x| number(x, rng)).unwrap_or(1.0).clamp(0.0, 1.0);
            rng.gen_bool(p)
        }
        "minecraft:random_chance_with_enchanted_bonus" => {
            let p = c.get("unenchanted_chance").map(|x| number(x, rng)).unwrap_or(0.0).clamp(0.0, 1.0);
            rng.gen_bool(p)
        }
        "minecraft:table_bonus" => {
            let p = c.get("chances").and_then(|x| x.as_array()).and_then(|a| a.first()).and_then(|x| x.as_f64()).unwrap_or(0.0);
            rng.gen_bool(p.clamp(0.0, 1.0))
        }
        "minecraft:inverted" => c.get("term").is_some_and(|t| !condition(t, ctx, rng)),
        "minecraft:any_of" => terms(c).iter().any(|t| condition(t, ctx, rng)),
        "minecraft:all_of" => terms(c).iter().all(|t| condition(t, ctx, rng)),
        "minecraft:match_tool" => {
            let Some(pred) = c.get("predicate") else { return true };
            // no enchantments / components on tools yet
            if pred.get("predicates").is_some() || pred.get("components").is_some() {
                return false;
            }
            match (pred.get("items"), ctx.tool) {
                (None, _) => ctx.tool.is_some(),
                (Some(_), None) => false,
                (Some(items), Some(tool)) => {
                    let check = |s: &str| match s.strip_prefix('#') {
                        Some(tag) => ctx.data.item_in(strip(tag), tool),
                        None => Items::get().id(s) == Some(tool),
                    };
                    match items {
                        Value::String(s) => check(s),
                        Value::Array(a) => a.iter().filter_map(|x| x.as_str()).any(check),
                        _ => false,
                    }
                }
            }
        }
        "minecraft:block_state_property" => {
            if let Some(b) = c.get("block").and_then(|b| b.as_str()) {
                if strip(b) != ctx.block.name() {
                    return false;
                }
            }
            let reg = BlockRegistry::get();
            c.get("properties").and_then(|p| p.as_object()).into_iter().flatten().all(|(k, v)| {
                let Some(cur) = reg.property(ctx.block, k) else { return false };
                match v {
                    Value::String(s) => cur == s,
                    Value::Object(range) => {
                        let Ok(n) = cur.parse::<f64>() else { return false };
                        range.get("min").and_then(|m| m.as_f64()).is_none_or(|m| n >= m) && range.get("max").and_then(|m| m.as_f64()).is_none_or(|m| n <= m)
                    }
                    other => cur == other.to_string(),
                }
            })
        }
        _ => false,
    }
}

/// Number provider (constant, uniform, binomial).
fn number(v: &Value, rng: &mut impl Rng) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::Object(o) => match o.get("type").and_then(|t| t.as_str()).unwrap_or("minecraft:uniform") {
            "minecraft:constant" => o.get("value").and_then(|x| x.as_f64()).unwrap_or(0.0),
            "minecraft:binomial" => {
                let n = o.get("n").map(|x| number(x, rng)).unwrap_or(0.0).round().clamp(0.0, 1000.0) as u32;
                let p = o.get("p").map(|x| number(x, rng)).unwrap_or(0.0).clamp(0.0, 1.0);
                (0..n).filter(|_| rng.gen_bool(p)).count() as f64
            }
            _ => {
                let lo = o.get("min").map(|x| number(x, rng)).unwrap_or(0.0);
                let hi = o.get("max").map(|x| number(x, rng)).unwrap_or(lo);
                if hi <= lo { lo } else { rng.gen_range(lo..hi) }
            }
        },
        _ => 0.0,
    }
}

fn int_number(v: &Value, rng: &mut impl Rng) -> i32 {
    // uniform integer ranges are inclusive in vanilla
    if let Value::Object(o) = v {
        if o.get("type").and_then(|t| t.as_str()).unwrap_or("minecraft:uniform") == "minecraft:uniform" {
            let lo = o.get("min").map(|x| number(x, rng)).unwrap_or(0.0).round() as i32;
            let hi = o.get("max").map(|x| number(x, rng)).unwrap_or(0.0).round() as i32;
            return if hi <= lo { lo } else { rng.gen_range(lo..=hi) };
        }
    }
    number(v, rng).round() as i32
}

fn apply_fn(f: &Value, count: i32, rng: &mut impl Rng) -> i32 {
    match f.get("function").and_then(|x| x.as_str()).unwrap_or("") {
        "minecraft:set_count" => {
            let n = f.get("count").map(|c| int_number(c, rng)).unwrap_or(1);
            let add = f.get("add").and_then(|a| a.as_bool()).unwrap_or(false);
            (if add { count + n } else { n }).max(0)
        }
        "minecraft:apply_bonus" => {
            // Fortune level 0: only the binomial formula adds items
            if f.get("formula").and_then(|x| x.as_str()) == Some("minecraft:binomial_with_bonus_count") {
                let p = f.get("parameters");
                let extra = p.and_then(|p| p.get("extra")).and_then(|x| x.as_i64()).unwrap_or(0).clamp(0, 64) as u32;
                let prob = p.and_then(|p| p.get("probability")).and_then(|x| x.as_f64()).unwrap_or(0.0).clamp(0.0, 1.0);
                count + (0..extra).filter(|_| rng.gen_bool(prob)).count() as i32
            } else {
                count
            }
        }
        "minecraft:limit_count" => {
            let l = f.get("limit");
            let lo = l.and_then(|l| l.get("min")).map(|x| int_number(x, rng)).unwrap_or(i32::MIN);
            let hi = l.and_then(|l| l.get("max")).map(|x| int_number(x, rng)).unwrap_or(i32::MAX);
            count.clamp(lo.min(hi), hi)
        }
        _ => count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_table() {
        let i = Items::get();
        assert_eq!(i.id("minecraft:air"), Some(0));
        assert_eq!(i.max_stack(i.id("ender_pearl").unwrap()), 16);
        assert_eq!(i.max_stack(i.id("diamond_pickaxe").unwrap()), 1);
        assert_eq!(i.block_for(i.id("oak_log").unwrap()).map(|b| b.name()), Some("oak_log"));
        assert_eq!(i.block_for(i.id("wheat_seeds").unwrap()).map(|b| b.name()), Some("wheat"));
        assert_eq!(i.block_for(i.id("diamond").unwrap()), None);
    }

    fn data_with(loot: &[(&str, &str)], tags: &[(&str, &[&str])]) -> GameData {
        let mut d = GameData::empty();
        for (k, v) in loot {
            d.loot.insert(k.to_string(), serde_json::from_str(v).unwrap());
        }
        for (k, v) in tags {
            d.block_tags.insert(k.to_string(), v.iter().map(|s| s.to_string()).collect());
        }
        d
    }

    #[test]
    fn loot_alternatives_and_tools() {
        let stone = r#"{"pools":[{"rolls":1.0,"entries":[{"type":"minecraft:alternatives","children":[
            {"type":"minecraft:item","name":"minecraft:stone","conditions":[{"condition":"minecraft:match_tool","predicate":{"predicates":{"minecraft:enchantments":[]}}}]},
            {"type":"minecraft:item","name":"minecraft:cobblestone"}]}],"conditions":[{"condition":"minecraft:survives_explosion"}]}]}"#;
        let mut d = data_with(&[("stone", stone)], &[("mineable/pickaxe", &["stone"])]);
        let pick = Items::get().id("wooden_pickaxe").unwrap();
        d.item_tags.insert("pickaxes".into(), [pick].into_iter().collect());
        let mut rng = rand::thread_rng();
        assert!(d.block_drops(crate::registry::blocks::STONE, None, &mut rng).is_empty(), "stone needs a pickaxe");
        let drops = d.block_drops(crate::registry::blocks::STONE, Some(pick), &mut rng);
        assert_eq!(drops, vec![Stack::new(Items::get().id("cobblestone").unwrap(), 1)]);
    }

    #[test]
    fn loot_slab_double() {
        let slab = r#"{"pools":[{"rolls":1.0,"entries":[{"type":"minecraft:item","name":"minecraft:oak_slab","functions":[
            {"function":"minecraft:set_count","count":2.0,"add":false,"conditions":[{"condition":"minecraft:block_state_property","block":"minecraft:oak_slab","properties":{"type":"double"}}]}]}]}]}"#;
        let d = data_with(&[("oak_slab", slab)], &[]);
        let reg = BlockRegistry::get();
        let double = reg.state("oak_slab", &[("type", "double")]).unwrap();
        let single = reg.state("oak_slab", &[("type", "bottom")]).unwrap();
        let mut rng = rand::thread_rng();
        assert_eq!(d.block_drops(double, None, &mut rng)[0].count, 2);
        assert_eq!(d.block_drops(single, None, &mut rng)[0].count, 1);
    }

    #[test]
    fn crafting_shapes() {
        let items = Items::get();
        let (log, planks, stick, table) = (items.id("oak_log").unwrap(), items.id("oak_planks").unwrap(), items.id("stick").unwrap(), items.id("crafting_table").unwrap());
        let mut d = GameData::empty();
        d.item_tags.insert("planks".into(), [planks].into_iter().collect());
        d.item_tags.insert("oak_logs".into(), [log].into_iter().collect());
        for j in [
            r##"{"type":"minecraft:crafting_shapeless","ingredients":["#minecraft:oak_logs"],"result":{"count":4,"id":"minecraft:oak_planks"}}"##,
            r##"{"type":"minecraft:crafting_shaped","key":{"#":"#minecraft:planks"},"pattern":["#","#"],"result":{"count":4,"id":"minecraft:stick"}}"##,
            r###"{"type":"minecraft:crafting_shaped","key":{"#":"#minecraft:planks"},"pattern":["##","##"],"result":{"id":"minecraft:crafting_table"}}"###,
        ] {
            let r = d.parse_recipe(&serde_json::from_str(j).unwrap()).unwrap();
            d.recipes.push(r);
        }
        assert_eq!(d.craft(&[None, None, None, Some(log)], 2), Some(Stack::new(planks, 4)));
        assert_eq!(d.craft(&[None, Some(planks), None, Some(planks)], 2), Some(Stack::new(stick, 4)));
        assert_eq!(d.craft(&[Some(planks); 4], 2), Some(Stack::new(table, 1)));
        // sticks anywhere in a 3x3 grid
        let mut g = [None; 9];
        g[5] = Some(planks);
        g[8] = Some(planks);
        assert_eq!(d.craft(&g, 3), Some(Stack::new(stick, 4)));
        assert_eq!(d.craft(&[Some(planks), Some(planks), None, None], 2), None);
    }
}

#[cfg(test)]
pub mod tests_support {
    use super::*;
    pub fn parse(d: &GameData, j: &Value) -> Recipe {
        d.parse_recipe(j).expect("valid recipe")
    }
}
