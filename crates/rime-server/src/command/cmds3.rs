//! `/advancement`, `/recipe` and server-side suggestions for them.

use super::args::ArgType as A;
use super::tree::Tree;
use super::{err, Ctx, CR};
use crate::game::advancement::{root_of, tree as adv_tree};
use serde_json::{json, Value};

const PLAYERS: A = A::Entity { single: false, players: true };

pub fn register(t: &mut Tree) {
    t.cmd("advancement", 2, |b| {
        for op in ["grant", "revoke"] {
            b.lit(op, |b| {
                b.arg("targets", PLAYERS, |b| {
                    b.lit("everything", |b| b.exec(advancement));
                    b.lit("only", |b| {
                        b.arg("advancement", A::Id, |b| {
                            b.exec(advancement);
                            b.arg("criterion", A::Greedy, |b| b.exec(advancement));
                        });
                    });
                    for mode in ["from", "through", "until"] {
                        b.lit(mode, |b| {
                            b.arg("advancement", A::Id, |b| b.exec(advancement));
                        });
                    }
                });
            });
        }
    });
    t.cmd("recipe", 2, |b| {
        for op in ["give", "take"] {
            b.lit(op, |b| {
                b.arg("targets", PLAYERS, |b| {
                    b.lit("*", |b| b.exec(recipe));
                    b.arg("recipe", A::Id, |b| b.exec(recipe));
                });
            });
        }
    });
}

fn full(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

/// `Advancement.name`: the title in brackets (or the id).
fn adv_name(i: usize) -> Value {
    let t = adv_tree();
    match &t.advs[i].display {
        Some(d) => json!({ "translate": "chat.square_brackets", "with": [d.title.clone()], "color": if d.frame == 1 { "dark_purple" } else { "green" } }),
        None => json!(t.advs[i].id),
    }
}

fn descendants(i: usize, out: &mut Vec<usize>) {
    out.push(i);
    for &c in &adv_tree().children[i] {
        descendants(c, out);
    }
}

fn advancement(ctx: &mut Ctx) -> CR {
    let t = adv_tree();
    let grant = ctx.lit("grant");
    let op = if grant { "grant" } else { "revoke" };
    let players = ctx.players("targets")?;
    let target = if ctx.has("advancement") {
        let id = full(&ctx.str("advancement"));
        Some(*t.ids.get(&id).ok_or_else(|| err("advancement.advancementNotFound", vec![json!(id)]))?)
    } else {
        None
    };
    // criterion only
    if ctx.has("criterion") {
        let i = target.unwrap();
        let c = ctx.str("criterion");
        if !t.advs[i].criteria.iter().any(|x| x.name == c) {
            return Err(err("commands.advancement.criterionNotFound", vec![adv_name(i), json!(c)]));
        }
        let mut n = 0;
        for p in &players {
            let g = ctx.st.level_of_player(p);
            if if grant { g.award_criterion(p, i, &c) } else { g.revoke_criterion(p, i, &c) } {
                n += 1;
            }
        }
        if n == 0 {
            return Err(if players.len() == 1 { err(&format!("commands.advancement.{op}.criterion.to.one.failure"), vec![json!(c), adv_name(i), json!(players[0].name)]) } else { err(&format!("commands.advancement.{op}.criterion.to.many.failure"), vec![json!(c), adv_name(i), json!(players.len())]) });
        }
        ctx.ok(if players.len() == 1 { err(&format!("commands.advancement.{op}.criterion.to.one.success"), vec![json!(c), adv_name(i), json!(players[0].name)]) } else { err(&format!("commands.advancement.{op}.criterion.to.many.success"), vec![json!(c), adv_name(i), json!(players.len())]) }, true);
        return Ok(n);
    }
    let list: Vec<usize> = match target {
        None => (0..t.advs.len()).collect(),
        Some(i) => {
            let mut v = Vec::new();
            if ctx.lit("only") {
                v.push(i);
            } else {
                if ctx.lit("until") || ctx.lit("through") {
                    let mut a = t.advs[i].parent;
                    while let Some(p) = a {
                        v.push(p);
                        a = t.advs[p].parent;
                    }
                    v.reverse();
                }
                if ctx.lit("until") {
                    v.push(i);
                } else {
                    descendants(i, &mut v);
                }
            }
            v
        }
    };
    let mut n = 0;
    for p in &players {
        let g = ctx.st.level_of_player(p);
        for &a in &list {
            if if grant { g.grant_all(p, a) } else { g.revoke_all(p, a) } {
                n += 1;
            }
        }
    }
    let _ = root_of;
    let one = list.len() == 1;
    let what = if one { adv_name(list[0]) } else { json!(list.len()) };
    let who = if players.len() == 1 { json!(players[0].name) } else { json!(players.len()) };
    let key = format!("commands.advancement.{op}.{}.to.{}", if one { "one" } else { "many" }, if players.len() == 1 { "one" } else { "many" });
    if n == 0 {
        return Err(err(&format!("{key}.failure"), vec![what, who]));
    }
    ctx.ok(err(&format!("{key}.success"), vec![what, who]), true);
    Ok(n)
}

fn recipe(ctx: &mut Ctx) -> CR {
    let give = ctx.lit("give");
    let players = ctx.players("targets")?;
    let ids: Vec<String> = if ctx.has("recipe") {
        let id = ctx.str("recipe");
        let short = id.strip_prefix("minecraft:").unwrap_or(&id).to_string();
        if !ctx.st.game.data.recipes.iter().any(|r| r.id == short) {
            return Err(err("recipe.notFound", vec![json!(full(&id))]));
        }
        vec![short]
    } else {
        ctx.st.game.data.recipes.iter().map(|r| r.id.clone()).collect()
    };
    let mut n = 0;
    for p in &players {
        let g = ctx.st.level_of_player(p);
        if give {
            n += g.unlock_recipes(p, &ids) as i32;
            g.flush_recipes(p);
        } else {
            n += g.lock_recipes(p, &ids) as i32;
        }
    }
    let op = if give { "give" } else { "take" };
    if n == 0 {
        return Err(err(&format!("commands.recipe.{op}.failed"), vec![]));
    }
    let who = if players.len() == 1 { json!(players[0].name) } else { json!(players.len()) };
    ctx.ok(err(&format!("commands.recipe.{op}.success.{}", if players.len() == 1 { "single" } else { "multiple" }), vec![json!(n), who]), true);
    Ok(n)
}

/// Server-side suggestions (`ask_server`) for advancement and recipe ids:
/// (start offset, matches).
pub fn suggest(line: &str) -> Option<(usize, Vec<String>)> {
    let words: Vec<&str> = line.split(' ').collect();
    let cmd = words.first()?.trim_start_matches('/');
    let last = *words.last()?;
    let start = line.len() - last.len();
    let pool: Vec<String> = match (cmd, words.len()) {
        ("advancement", 5) if matches!(words[3], "only" | "from" | "through" | "until") => adv_tree().advs.iter().filter(|a| a.display.is_some() || !a.id.contains("recipes/")).map(|a| a.id.clone()).collect(),
        ("advancement", 6) if words[3] == "only" => {
            let id = full(words[4]);
            let t = adv_tree();
            t.ids.get(&id).map(|i| t.advs[*i].criteria.iter().map(|c| c.name.clone()).collect()).unwrap_or_default()
        }
        ("recipe", 4) => crate::command::server().map(|st| st.game.data.recipes.iter().map(|r| format!("minecraft:{}", r.id)).collect()).unwrap_or_default(),
        _ => return None,
    };
    let needle = last.to_ascii_lowercase();
    let mut m: Vec<String> = pool.into_iter().filter(|x| x.starts_with(&needle) || x.trim_start_matches("minecraft:").starts_with(&needle)).collect();
    m.sort();
    m.truncate(200);
    Some((start, m))
}
