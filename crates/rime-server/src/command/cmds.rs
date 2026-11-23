//! Player and entity commands.

use super::args::ArgType as A;
use super::tree::{Tree, B};
use super::{err, one_or_many, state, Ctx, Target, CR};
use crate::data::{Items, Stack};
use crate::game::combat::damage::DamageSource;
use crate::game::combat::effects::{Effect, Instance};
use crate::game::{Game, Request};
use crate::world::storage::Dim;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;

const ENT: A = A::Entity { single: false, players: false };
const ENT1: A = A::Entity { single: true, players: false };
const PLAYERS: A = A::Entity { single: false, players: true };
const PLAYER1: A = A::Entity { single: true, players: true };

pub fn register(t: &mut Tree) {
    // ---------------------------------------------------------- gamemode
    t.cmd("gamemode", 2, |b| {
        b.arg("gamemode", A::Gamemode, |b| {
            b.exec(gamemode);
            b.arg("target", PLAYERS, |b| b.exec(gamemode));
        });
    });
    t.cmd("defaultgamemode", 2, |b| {
        b.arg("gamemode", A::Gamemode, |b| b.exec(defaultgamemode));
    });
    t.cmd("difficulty", 2, |b| {
        b.exec(difficulty);
        for d in crate::game::rules::DIFFICULTIES {
            b.lit(d, |b| b.exec(difficulty));
        }
    });
    // ---------------------------------------------------------- teleport
    let tp = |b: &mut B| {
        b.arg("location", A::Vec3, |b| b.exec(teleport));
        b.arg("destination", ENT1, |b| b.exec(teleport));
        b.arg("targets", ENT, |b| {
            b.arg("location", A::Vec3, |b| {
                b.exec(teleport);
                b.arg("rotation", A::Rotation, |b| b.exec(teleport));
                b.lit("facing", |b| {
                    b.arg("facingLocation", A::Vec3, |b| b.exec(teleport));
                    b.lit("entity", |b| {
                        b.arg("facingEntity", ENT1, |b| {
                            b.exec(teleport);
                            b.arg("facingAnchor", A::Anchor, |b| b.exec(teleport));
                        });
                    });
                });
            });
            b.arg("destination", ENT1, |b| b.exec(teleport));
        });
    };
    t.cmd("teleport", 2, tp);
    t.cmd("tp", 2, tp);
    // ---------------------------------------------------------- items
    t.cmd("give", 2, |b| {
        b.arg("targets", PLAYERS, |b| {
            b.arg("item", A::Item, |b| {
                b.exec(give);
                b.arg("count", A::Int(1, i32::MAX), |b| b.exec(give));
            });
        });
    });
    t.cmd("clear", 2, |b| {
        b.exec(clear);
        b.arg("targets", PLAYERS, |b| {
            b.exec(clear);
            b.arg("item", A::ItemPredicate, |b| {
                b.exec(clear);
                b.arg("maxCount", A::Int(0, i32::MAX), |b| b.exec(clear));
            });
        });
    });
    t.cmd("enchant", 2, |b| {
        b.arg("targets", ENT, |b| {
            b.arg("enchantment", A::Resource("minecraft:enchantment"), |b| {
                b.exec(enchant);
                b.arg("level", A::Int(0, i32::MAX), |b| b.exec(enchant));
            });
        });
    });
    // ---------------------------------------------------------- effects
    t.cmd("effect", 2, |b| {
        b.lit("clear", |b| {
            b.exec(effect_clear);
            b.arg("targets", ENT, |b| {
                b.exec(effect_clear);
                b.arg("effect", A::Resource("minecraft:mob_effect"), |b| b.exec(effect_clear));
            });
        });
        b.lit("give", |b| {
            b.arg("targets", ENT, |b| {
                b.arg("effect", A::Resource("minecraft:mob_effect"), |b| {
                    b.exec(effect_give);
                    b.arg("seconds", A::Int(1, 1_000_000), |b| {
                        b.exec(effect_give);
                        b.arg("amplifier", A::Int(0, 255), |b| {
                            b.exec(effect_give);
                            b.arg("hideParticles", A::Bool, |b| b.exec(effect_give));
                        });
                    });
                    b.lit("infinite", |b| {
                        b.exec(effect_give);
                        b.arg("amplifier", A::Int(0, 255), |b| {
                            b.exec(effect_give);
                            b.arg("hideParticles", A::Bool, |b| b.exec(effect_give));
                        });
                    });
                });
            });
        });
    });
    // ---------------------------------------------------------- experience
    let xp = |b: &mut B| {
        for op in ["add", "set"] {
            b.lit(op, |b| {
                b.arg("targets", PLAYERS, |b| {
                    b.arg("amount", A::Int(if op == "set" { 0 } else { i32::MIN }, i32::MAX), |b| {
                        b.exec(experience);
                        b.lit("points", |b| b.exec(experience));
                        b.lit("levels", |b| b.exec(experience));
                    });
                });
            });
        }
        b.lit("query", |b| {
            b.arg("target", PLAYER1, |b| {
                b.lit("points", |b| b.exec(experience));
                b.lit("levels", |b| b.exec(experience));
            });
        });
    };
    t.cmd("experience", 2, xp);
    t.cmd("xp", 2, xp);
    // ---------------------------------------------------------- entities
    t.cmd("kill", 2, |b| {
        b.exec(kill);
        b.arg("targets", ENT, |b| b.exec(kill));
    });
    t.cmd("summon", 2, |b| {
        b.arg("entity", A::Resource("minecraft:entity_type"), |b| {
            b.exec(summon);
            b.arg("pos", A::Vec3, |b| {
                b.exec(summon);
                b.arg("nbt", A::NbtCompound, |b| b.exec(summon));
            });
        });
    });
    t.cmd("damage", 2, |b| {
        b.arg("target", ENT1, |b| {
            b.arg("amount", A::Float(0.0, f32::MAX), |b| {
                b.exec(damage);
                b.arg("damageType", A::Resource("minecraft:damage_type"), |b| {
                    b.exec(damage);
                    b.lit("at", |b| {
                        b.arg("location", A::Vec3, |b| b.exec(damage));
                    });
                    b.lit("by", |b| {
                        b.arg("entity", ENT1, |b| {
                            b.exec(damage);
                            b.lit("from", |b| {
                                b.arg("cause", ENT1, |b| b.exec(damage));
                            });
                        });
                    });
                });
            });
        });
    });
    t.cmd("tag", 2, |b| {
        b.arg("targets", ENT, |b| {
            b.lit("add", |b| {
                b.arg("name", A::Word, |b| b.exec(tag));
            });
            b.lit("remove", |b| {
                b.arg("name", A::Word, |b| b.exec(tag));
            });
            b.lit("list", |b| b.exec(tag));
        });
    });
    t.cmd("ride", 2, |b| {
        b.arg("target", ENT1, |b| {
            b.lit("mount", |b| {
                b.arg("vehicle", ENT1, |b| b.exec(ride));
            });
            b.lit("dismount", |b| b.exec(ride));
        });
    });
    t.cmd("attribute", 2, |b| {
        b.arg("target", ENT1, |b| {
            b.arg("attribute", A::Resource("minecraft:attribute"), |b| {
                b.lit("get", |b| {
                    b.exec(attribute);
                    b.arg("scale", A::Double(f64::MIN, f64::MAX), |b| b.exec(attribute));
                });
                b.lit("base", |b| {
                    b.lit("get", |b| {
                        b.exec(attribute);
                        b.arg("scale", A::Double(f64::MIN, f64::MAX), |b| b.exec(attribute));
                    });
                    b.lit("set", |b| {
                        b.arg("value", A::Double(f64::MIN, f64::MAX), |b| b.exec(attribute));
                    });
                    b.lit("reset", |b| b.exec(attribute));
                });
                b.lit("modifier", |b| {
                    b.lit("add", |b| {
                        b.arg("id", A::Id, |b| {
                            b.arg("value", A::Double(f64::MIN, f64::MAX), |b| {
                                for op in ["add_value", "add_multiplied_base", "add_multiplied_total"] {
                                    b.lit(op, |b| b.exec(attribute));
                                }
                            });
                        });
                    });
                    b.lit("remove", |b| {
                        b.arg("id", A::Id, |b| b.exec(attribute));
                    });
                    b.lit("value", |b| {
                        b.lit("get", |b| {
                            b.arg("id", A::Id, |b| {
                                b.exec(attribute);
                                b.arg("scale", A::Double(f64::MIN, f64::MAX), |b| b.exec(attribute));
                            });
                        });
                    });
                });
            });
        });
    });
    // ---------------------------------------------------------- chat
    t.cmd("say", 2, |b| {
        b.arg("message", A::Message, |b| b.exec(say));
    });
    t.cmd("me", 0, |b| {
        b.arg("action", A::Message, |b| b.exec(me));
    });
    for name in ["msg", "tell", "w"] {
        t.cmd(name, 0, |b| {
            b.arg("targets", PLAYERS, |b| {
                b.arg("message", A::Message, |b| b.exec(msg));
            });
        });
    }
    for name in ["teammsg", "tm"] {
        t.cmd(name, 0, |b| {
            b.arg("message", A::Message, |b| b.exec(teammsg));
        });
    }
    t.cmd("tellraw", 2, |b| {
        b.arg("targets", PLAYERS, |b| {
            b.arg("message", A::Component, |b| b.exec(tellraw));
        });
    });
    t.cmd("title", 2, |b| {
        b.arg("targets", PLAYERS, |b| {
            b.lit("clear", |b| b.exec(title));
            b.lit("reset", |b| b.exec(title));
            for k in ["title", "subtitle", "actionbar"] {
                b.lit(k, |b| {
                    b.arg("title", A::Component, |b| b.exec(title));
                });
            }
            b.lit("times", |b| {
                b.arg("fadeIn", A::Time(0), |b| {
                    b.arg("stay", A::Time(0), |b| {
                        b.arg("fadeOut", A::Time(0), |b| b.exec(title));
                    });
                });
            });
        });
    });
    t.cmd("list", 0, |b| {
        b.exec(list);
        b.lit("uuids", |b| b.exec(list));
    });
    t.cmd("seed", 2, |b| b.exec(seed));
    t.cmd("spawnpoint", 2, |b| {
        b.exec(spawnpoint);
        b.arg("targets", PLAYERS, |b| {
            b.exec(spawnpoint);
            b.arg("pos", A::BlockPos, |b| {
                b.exec(spawnpoint);
                b.arg("angle", A::Angle, |b| b.exec(spawnpoint));
            });
        });
    });
    t.cmd("setworldspawn", 2, |b| {
        b.exec(setworldspawn);
        b.arg("pos", A::BlockPos, |b| {
            b.exec(setworldspawn);
            b.arg("angle", A::Angle, |b| b.exec(setworldspawn));
        });
    });
    t.cmd("spawn", 0, |b| b.exec(spawn));
    t.cmd("help", 0, |b| {
        b.exec(help);
        b.arg("command", A::Greedy, |b| b.exec(help));
    });
}

// ------------------------------------------------------------------ helpers

pub fn game_of<'a>(ctx: &'a Ctx, t: &Target) -> &'a Arc<Game> {
    ctx.st.level(t.dim())
}

fn targets_or_self(ctx: &Ctx, n: &str) -> Result<Vec<Target>, Value> {
    if ctx.has(n) {
        ctx.targets(n)
    } else {
        ctx.me_target().map(|t| vec![t]).ok_or_else(|| err("permissions.requires.entity", vec![]))
    }
}

const GM_KEYS: [&str; 4] = ["survival", "creative", "adventure", "spectator"];

fn gm_name(g: i32) -> Value {
    json!({ "translate": format!("gameMode.{}", GM_KEYS[g.clamp(0, 3) as usize]) })
}

// ------------------------------------------------------------------ commands

fn gamemode(ctx: &mut Ctx) -> CR {
    let gm = ctx.int("gamemode");
    let targets = if ctx.has("target") { ctx.players("target")? } else { vec![ctx.me()?] };
    let mut n = 0;
    for p in targets {
        if p.gamemode.load(Ordering::Relaxed) == gm {
            continue;
        }
        p.requests.lock().push(Request::Gamemode(gm));
        n += 1;
        let me = ctx.src.player.as_ref().is_some_and(|m| m.eid == p.eid);
        if me {
            ctx.ok(err("commands.gamemode.success.self", vec![gm_name(gm)]), true);
        } else {
            if crate::game::rules::on("send_command_feedback") {
                let mut body = crate::nbt::Tag::from_json(&err("gameMode.changed", vec![gm_name(gm)])).to_network_bytes();
                crate::net::codec::Out::bool(&mut body, false);
                p.send(ctx.st.game.encode("system_chat", &body));
            }
            ctx.ok(err("commands.gamemode.success.other", vec![json!({ "text": p.name }), gm_name(gm)]), true);
        }
    }
    Ok(n)
}

fn defaultgamemode(ctx: &mut Ctx) -> CR {
    let gm = ctx.int("gamemode");
    crate::game::rules::set_default_gamemode(gm);
    ctx.ok(err("commands.defaultgamemode.success", vec![gm_name(gm)]), true);
    Ok(0)
}

fn difficulty(ctx: &mut Ctx) -> CR {
    let words: Vec<&str> = ctx.line.split_whitespace().collect();
    let cur = crate::game::rules::difficulty();
    let name = |d: i32| json!({ "translate": format!("options.difficulty.{}", crate::game::rules::DIFFICULTIES[d as usize]) });
    match words.get(1).and_then(|w| crate::game::rules::DIFFICULTIES.iter().position(|d| d == w)) {
        None => {
            ctx.ok(err("commands.difficulty.query", vec![name(cur)]), false);
            Ok(cur)
        }
        Some(d) => {
            let d = d as i32;
            if d == cur {
                return Err(err("commands.difficulty.failure", vec![json!(crate::game::rules::DIFFICULTIES[d as usize])]));
            }
            crate::game::rules::set_difficulty(d);
            let mut b = Vec::new();
            crate::net::codec::Out::u8(&mut b, d as u8);
            crate::net::codec::Out::bool(&mut b, crate::game::rules::difficulty_locked());
            ctx.st.broadcast_all(ctx.st.game.encode("change_difficulty", &b));
            ctx.ok(err("commands.difficulty.success", vec![name(d)]), true);
            Ok(0)
        }
    }
}

/// Moves a player or entity, across dimensions if needed.
pub fn move_target(ctx: &Ctx, t: &Target, dim: Dim, pos: [f64; 3], rot: [f32; 2]) {
    match t {
        Target::Player(p) => {
            p.requests.lock().push(Request::Travel(dim, pos, rot[0], rot[1]));
        }
        Target::Ent(d, s) => {
            if *d == dim {
                ctx.st.level(dim).move_entity(s.eid, pos, Some(rot));
            } else if let Some(mut e) = ctx.st.level(*d).take_entity(s.eid) {
                e.pos = pos;
                e.synced = pos;
                let to = ctx.st.level(dim).clone();
                let (cx, cz) = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
                to.world.ensure_loaded(&[(cx, cz)], &ctx.st.store, ctx.st.generator_of(dim));
                to.put_entity(e);
            }
        }
    }
}

fn look_at(from: [f64; 3], to: [f64; 3]) -> [f32; 2] {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let h = (d[0] * d[0] + d[2] * d[2]).sqrt();
    let yaw = (d[2].atan2(d[0]).to_degrees() - 90.0) as f32;
    let pitch = (-(d[1].atan2(h).to_degrees())) as f32;
    [crate::game::ai::control::wrap(yaw), crate::game::ai::control::wrap(pitch)]
}

fn teleport(ctx: &mut Ctx) -> CR {
    let targets = if ctx.has("targets") { ctx.targets("targets")? } else { vec![ctx.me_target().ok_or_else(|| err("permissions.requires.entity", vec![]))?] };
    let (dim, pos, dest_rot, dest_name) = if ctx.has("destination") {
        let d = ctx.target("destination")?;
        (d.dim(), d.pos(), Some(d.rot()), Some(d.name_json()))
    } else {
        (ctx.src.dim, ctx.vec3("location"), None, None)
    };
    for t in &targets {
        let mut rot = dest_rot.unwrap_or_else(|| t.rot());
        if ctx.has("rotation") {
            rot = ctx.coords("rotation").rotation(t.rot());
        } else if ctx.has("facingLocation") {
            rot = look_at([pos[0], pos[1] + t.eye(), pos[2]], ctx.vec3("facingLocation"));
        } else if ctx.has("facingEntity") {
            let f = ctx.target("facingEntity")?;
            let eyes = matches!(ctx.args.get("facingAnchor"), Some(super::args::Arg::Anchor(true)));
            let fp = f.pos();
            rot = look_at([pos[0], pos[1] + t.eye(), pos[2]], [fp[0], fp[1] + if eyes { f.eye() } else { 0.0 }, fp[2]]);
        }
        move_target(ctx, t, dim, pos, rot);
    }
    let fmt = |v: f64| json!(format!("{v:.2}").trim_end_matches('0').trim_end_matches('.').to_string());
    match dest_name {
        Some(n) => ctx.ok(one_or_many(&targets, "commands.teleport.success.entity.single", "commands.teleport.success.entity.multiple", vec![n]), true),
        None => ctx.ok(one_or_many(&targets, "commands.teleport.success.location.single", "commands.teleport.success.location.multiple", vec![fmt(pos[0]), fmt(pos[1]), fmt(pos[2])]), true),
    }
    Ok(targets.len() as i32)
}

pub fn give_stack(ctx: &Ctx, p: &Arc<crate::game::Player>, item: i32, comp: u32, count: i32) {
    let g = ctx.st.level_of(p).clone();
    let max = Items::get().max_stack(item) as i32;
    let mut left = count;
    let held = p.held.load(Ordering::Relaxed) as i16;
    while left > 0 {
        let n = left.min(max);
        left -= n;
        let st = Stack { item, count: n as u8, comp };
        let rest = p.inv.lock().add(st, held);
        if rest > 0 {
            let pos = p.pose.lock().pos;
            g.spawn_item(pos, Stack { item, count: rest, comp }, [0.0; 3], 0);
        }
    }
    g.flush_inventory(p);
    let pos = p.pose.lock().pos;
    g.play_sound("entity.item.pickup", 7, pos, 0.2, ((rand::random::<f32>() - rand::random::<f32>()) * 0.7 + 1.0) * 2.0);
}

fn give(ctx: &mut Ctx) -> CR {
    let item = ctx.item("item");
    let Some(id) = item.item else { return Err(err("argument.item.id.invalid", vec![json!("*")])) };
    let count = ctx.int_or("count", 1);
    let max = Items::get().max_stack(id) as i32 * 100;
    if count > max {
        return Err(err("commands.give.failed.toomanyitems", vec![json!(max), json!({ "translate": format!("item.minecraft.{}", Items::get().name(id)) })]));
    }
    let players = ctx.players("targets")?;
    for p in &players {
        give_stack(ctx, p, id, item.comp, count);
    }
    let name = json!({ "translate": format!("item.minecraft.{}", Items::get().name(id)) });
    let ts: Vec<Target> = players.iter().cloned().map(Target::Player).collect();
    if ts.len() == 1 {
        ctx.ok(err("commands.give.success.single", vec![json!(count), name, ts[0].name_json()]), true);
    } else {
        ctx.ok(err("commands.give.success.multiple", vec![json!(count), name, json!(ts.len())]), true);
    }
    Ok(players.len() as i32)
}

fn clear(ctx: &mut Ctx) -> CR {
    let players = if ctx.has("targets") { ctx.players("targets")? } else { vec![ctx.me()?] };
    let pred = ctx.item("item");
    let max = if ctx.has("maxCount") { ctx.int("maxCount") } else { -1 };
    let data = ctx.st.game.data.clone();
    let mut total = 0;
    for p in &players {
        let mut inv = p.inv.lock();
        let mut left = max;
        for i in 5..crate::game::inventory::SLOTS {
            let Some(s) = inv.slots[i] else { continue };
            if !pred.test(&s, &data) {
                continue;
            }
            if max == 0 {
                total += s.count as i32;
                continue;
            }
            let take = if left < 0 { s.count as i32 } else { left.min(s.count as i32) };
            total += take;
            if left >= 0 {
                left -= take;
            }
            let mut ns = s;
            ns.count -= take as u8;
            inv.set(i, if ns.count == 0 { None } else { Some(ns) });
            if left == 0 {
                break;
            }
        }
        drop(inv);
        ctx.st.level_of(p).flush_inventory(p);
    }
    let ts: Vec<Target> = players.iter().cloned().map(Target::Player).collect();
    if total == 0 {
        return Err(if ts.len() == 1 { err("clear.failed.single", vec![ts[0].name_json()]) } else { err("clear.failed.multiple", vec![json!(ts.len())]) });
    }
    if max == 0 {
        ctx.ok(if ts.len() == 1 { err("commands.clear.test.single", vec![json!(total), ts[0].name_json()]) } else { err("commands.clear.test.multiple", vec![json!(total), json!(ts.len())]) }, true);
    } else if ts.len() == 1 {
        ctx.ok(err("commands.clear.success.single", vec![json!(total), ts[0].name_json()]), true);
    } else {
        ctx.ok(err("commands.clear.success.multiple", vec![json!(total), json!(ts.len())]), true);
    }
    Ok(total)
}

fn enchant(ctx: &mut Ctx) -> CR {
    let name = ctx.str("enchantment");
    let short = name.trim_start_matches("minecraft:").to_string();
    let level = ctx.int_or("level", 1);
    let ench = crate::game::combat::enchant::Enchantments::get().by(&short).ok_or_else(|| err("argument.resource.not_found", vec![json!(name)]))?;
    if level > ench.max_level {
        return Err(err("commands.enchant.failed.level", vec![json!(level), json!(ench.max_level)]));
    }
    let targets = ctx.targets("targets")?;
    let mut n = 0;
    let mut last_err = err("commands.enchant.failed", vec![]);
    for t in &targets {
        let Some(p) = t.player() else {
            last_err = err("commands.enchant.failed.entity", vec![t.name_json()]);
            continue;
        };
        let slot = 36 + p.held.load(Ordering::Relaxed) as usize;
        let mut inv = p.inv.lock();
        let Some(s) = inv.slots[slot] else {
            last_err = err("commands.enchant.failed.itemless", vec![t.name_json()]);
            continue;
        };
        if !crate::game::menu::can_enchant(&short, &s, &ctx.st.game.data) {
            last_err = err("commands.enchant.failed.incompatible", vec![json!({ "translate": format!("item.minecraft.{}", Items::get().name(s.item)) })]);
            continue;
        }
        let ns = s.with_patch(|pt| {
            pt.enchantments.retain(|(e, _)| *e != short);
            pt.enchantments.push((short.clone(), level.clamp(1, 255) as u8));
        });
        inv.set(slot, Some(ns));
        drop(inv);
        ctx.st.level_of(p).flush_inventory(p);
        n += 1;
    }
    if n == 0 {
        return Err(last_err);
    }
    let ename = json!({ "translate": format!("enchantment.minecraft.{short}") });
    ctx.ok(if targets.len() == 1 { err("commands.enchant.success.single", vec![ename.clone(), targets[0].name_json()]) } else { err("commands.enchant.success.multiple", vec![ename, json!(targets.len())]) }, true);
    Ok(n)
}

fn effect_name(e: Effect) -> Value {
    json!({ "translate": format!("effect.minecraft.{}", e.name()) })
}

fn effect_give(ctx: &mut Ctx) -> CR {
    let name = ctx.str("effect");
    let e = Effect::from_name(name.trim_start_matches("minecraft:")).ok_or_else(|| err("argument.resource.not_found", vec![json!(name)]))?;
    let infinite = ctx.line.split_whitespace().any(|w| w == "infinite");
    let dur = if infinite {
        crate::game::combat::effects::INFINITE
    } else if ctx.has("seconds") {
        if e.is_instant() { ctx.int("seconds") } else { ctx.int("seconds") * 20 }
    } else if e.is_instant() {
        1
    } else {
        600
    };
    let amp = ctx.int_or("amplifier", 0);
    let hide = ctx.bool("hideParticles");
    let targets = ctx.targets("targets")?;
    let mut n = 0;
    for t in &targets {
        if !t.living() {
            continue;
        }
        let mut i = Instance::new(e, dur, amp);
        i.visible = !hide;
        if game_of(ctx, t).add_effect(t.eid(), i) {
            n += 1;
        }
    }
    if n == 0 {
        return Err(err("commands.effect.give.failed", vec![]));
    }
    ctx.ok(if targets.len() == 1 { err("commands.effect.give.success.single", vec![effect_name(e), targets[0].name_json(), json!(dur / 20)]) } else { err("commands.effect.give.success.multiple", vec![effect_name(e), json!(targets.len()), json!(dur / 20)]) }, true);
    Ok(n)
}

fn effect_clear(ctx: &mut Ctx) -> CR {
    let targets = targets_or_self(ctx, "targets")?;
    let only = if ctx.has("effect") { Effect::from_name(ctx.str("effect").trim_start_matches("minecraft:")) } else { None };
    let mut n = 0;
    for t in &targets {
        let g = game_of(ctx, t);
        let f = |l: &mut crate::game::combat::living::Living| match only {
            Some(e) => l.remove_effect(e),
            None => l.remove_all_effects(),
        };
        let done = match t {
            Target::Player(p) => g.with_player(p, |l, _, _| f(l)),
            Target::Ent(_, s) => g.with_mob(s.eid, |l, _, _| f(l)).unwrap_or(false),
        };
        if done {
            n += 1;
        }
    }
    if n == 0 {
        return Err(err(if only.is_some() { "commands.effect.clear.specific.failed" } else { "commands.effect.clear.everything.failed" }, vec![]));
    }
    match only {
        Some(e) => ctx.ok(if targets.len() == 1 { err("commands.effect.clear.specific.success.single", vec![effect_name(e), targets[0].name_json()]) } else { err("commands.effect.clear.specific.success.multiple", vec![effect_name(e), json!(targets.len())]) }, true),
        None => ctx.ok(one_or_many(&targets, "commands.effect.clear.everything.success.single", "commands.effect.clear.everything.success.multiple", vec![]), true),
    }
    Ok(n)
}

fn experience(ctx: &mut Ctx) -> CR {
    let words: Vec<&str> = ctx.line.split_whitespace().collect();
    let levels = words.last() == Some(&"levels");
    let unit = if levels { "levels" } else { "points" };
    if words.get(1) == Some(&"query") {
        let t = ctx.target("target")?;
        let p = t.player().ok_or_else(|| err("argument.player.unknown", vec![]))?;
        let v = p.vit.lock();
        let n = if levels { v.xp_level } else { (v.xp_progress * v.xp_needed() as f32) as i32 };
        drop(v);
        ctx.ok(err(&format!("commands.experience.query.{unit}"), vec![t.name_json(), json!(n)]), false);
        return Ok(n);
    }
    let set = words.get(1) == Some(&"set");
    let amount = ctx.int("amount");
    let players = ctx.players("targets")?;
    for p in &players {
        let mut v = p.vit.lock();
        if set {
            if levels {
                v.xp_level = 0;
                v.give_levels(amount);
            } else {
                let needed = v.xp_needed();
                if amount > needed {
                    return Err(err("commands.experience.set.points.invalid", vec![]));
                }
                v.xp_progress = amount as f32 / needed as f32;
                v.xp_dirty = true;
            }
        } else if levels {
            v.give_levels(amount);
        } else {
            v.give_xp(amount);
        }
    }
    let ts: Vec<Target> = players.iter().cloned().map(Target::Player).collect();
    let op = if set { "set" } else { "add" };
    ctx.ok(if ts.len() == 1 { err(&format!("commands.experience.{op}.{unit}.success.single"), vec![json!(amount), ts[0].name_json()]) } else { err(&format!("commands.experience.{op}.{unit}.success.multiple"), vec![json!(amount), json!(ts.len())]) }, true);
    Ok(players.len() as i32)
}

fn kill(ctx: &mut Ctx) -> CR {
    let targets = targets_or_self(ctx, "targets")?;
    for t in &targets {
        let g = game_of(ctx, t);
        if t.living() {
            g.hurt_entity(t.eid(), &DamageSource::new("generic_kill"), f32::MAX);
        } else {
            g.discard(t.eid());
        }
    }
    ctx.ok(one_or_many(&targets, "commands.kill.success.single", "commands.kill.success.multiple", vec![]), true);
    Ok(targets.len() as i32)
}

fn summon(ctx: &mut Ctx) -> CR {
    let name = ctx.str("entity");
    let short = name.trim_start_matches("minecraft:").to_string();
    let pos = if ctx.has("pos") { ctx.vec3("pos") } else { ctx.src.pos };
    if pos[1].abs() > 2.0e7 || pos[0].abs() > 3.0e7 || pos[2].abs() > 3.0e7 {
        return Err(err("commands.summon.invalidPosition", vec![]));
    }
    let g = ctx.level().clone();
    let eid = g.summon(&short, pos).ok_or_else(|| err("commands.summon.failed", vec![]))?;
    if let Some(nbt) = ctx.nbt("nbt") {
        if let Some(n) = nbt.get("CustomName") {
            let text = match n {
                crate::nbt::Tag::String(s) => s.clone(),
                other => super::args::plain(&super::args::tag_to_json(other)),
            };
            g.set_custom_name(eid, Some(text));
        }
        if let Some(crate::nbt::Tag::List(tags)) = nbt.get("Tags") {
            if let Some(s) = g.entity_snap(eid) {
                state::with(|st| {
                    let set = st.tags.entry(s.uuid).or_default();
                    for t in tags {
                        if let Some(x) = t.as_str() {
                            set.insert(x.to_string());
                        }
                    }
                });
            }
        }
        g.apply_mob_nbt(eid, &nbt);
    }
    ctx.ok(err("commands.summon.success", vec![json!({ "translate": format!("entity.minecraft.{short}") })]), true);
    Ok(1)
}

fn damage(ctx: &mut Ctx) -> CR {
    let t = ctx.target("target")?;
    let amount = ctx.f64("amount") as f32;
    let ty = if ctx.has("damageType") { ctx.str("damageType") } else { "minecraft:generic".into() };
    let ty: &'static str = crate::game::combat::damage::DamageTypes::get().types.get_key_value(ty.trim_start_matches("minecraft:")).map(|(k, _)| k.as_str()).unwrap_or("generic");
    let src = if ctx.has("entity") {
        let by = ctx.target("entity")?;
        let cause = if ctx.has("cause") { Some(ctx.target("cause")?) } else { None };
        let r = |x: &Target| crate::game::combat::damage::EntityRef::new(x.eid(), crate::game::etypes::EntityTypes::static_name(x.type_name()), x.pos(), x.living());
        let mut d = DamageSource::indirect(ty, r(&by), cause.as_ref().map(r).or_else(|| Some(r(&by))));
        if cause.is_none() {
            d = DamageSource::by(ty, r(&by));
        }
        if let Target::Player(_) = by {
            if let Some(c) = d.causing.as_mut() {
                c.player = true;
            }
        }
        d
    } else if ctx.has("location") {
        DamageSource::at(ty, ctx.vec3("location"))
    } else {
        DamageSource::new(ty)
    };
    if !t.living() || !game_of(ctx, &t).hurt_entity(t.eid(), &src, amount) {
        return Err(err("commands.damage.invulnerable", vec![]));
    }
    ctx.ok(err("commands.damage.success", vec![json!(amount), t.name_json()]), true);
    Ok(1)
}

fn tag(ctx: &mut Ctx) -> CR {
    let targets = ctx.targets("targets")?;
    let words: Vec<&str> = ctx.line.split_whitespace().collect();
    let op = words.iter().rev().find(|w| ["add", "remove", "list"].contains(w)).copied().unwrap_or("list");
    let name = ctx.str("name");
    match op {
        "add" | "remove" => {
            let n = state::with(|s| {
                let mut n = 0;
                for t in &targets {
                    let set = s.tags.entry(t.uuid()).or_default();
                    let changed = if op == "add" { set.len() < 1024 && set.insert(name.clone()) } else { set.remove(&name) };
                    if changed {
                        n += 1;
                    }
                }
                s.tags.retain(|_, v| !v.is_empty());
                n
            });
            if n == 0 {
                return Err(err(&format!("commands.tag.{op}.failed"), vec![]));
            }
            ctx.ok(if targets.len() == 1 { err(&format!("commands.tag.{op}.success.single"), vec![json!(name), targets[0].name_json()]) } else { err(&format!("commands.tag.{op}.success.multiple"), vec![json!(name), json!(targets.len())]) }, true);
            Ok(n)
        }
        _ => {
            let all: std::collections::BTreeSet<String> = state::read(|s| targets.iter().flat_map(|t| s.tags.get(&t.uuid()).cloned().unwrap_or_default()).collect());
            let list = json!(all.iter().cloned().collect::<Vec<_>>().join(", "));
            if targets.len() == 1 {
                if all.is_empty() {
                    ctx.ok(err("commands.tag.list.single.empty", vec![targets[0].name_json()]), false);
                } else {
                    ctx.ok(err("commands.tag.list.single.success", vec![targets[0].name_json(), json!(all.len()), list]), false);
                }
            } else if all.is_empty() {
                ctx.ok(err("commands.tag.list.multiple.empty", vec![json!(targets.len())]), false);
            } else {
                ctx.ok(err("commands.tag.list.multiple.success", vec![json!(targets.len()), json!(all.len()), list]), false);
            }
            Ok(all.len() as i32)
        }
    }
}

fn ride(ctx: &mut Ctx) -> CR {
    let t = ctx.target("target")?;
    let g = game_of(ctx, &t).clone();
    if ctx.has("vehicle") {
        let v = ctx.target("vehicle")?;
        if v.dim() != t.dim() {
            return Err(err("commands.ride.mount.failure.wrong_dimension", vec![]));
        }
        if v.eid() == t.eid() {
            return Err(err("commands.ride.mount.failure.cant_ride_self", vec![]));
        }
        if g.vehicle_of(t.eid()).is_some() {
            return Err(err("commands.ride.already_riding", vec![t.name_json(), g.vehicle_of(t.eid()).map(|_| v.name_json()).unwrap_or_default()]));
        }
        if matches!(v, Target::Player(_)) {
            return Err(err("commands.ride.mount.failure.cant_ride_players", vec![]));
        }
        if !g.mount(t.eid(), v.eid()) {
            return Err(err("commands.ride.mount.failure.generic", vec![]));
        }
        ctx.ok(err("commands.ride.mount.success", vec![t.name_json(), v.name_json()]), true);
    } else {
        let Some(v) = g.vehicle_of(t.eid()) else { return Err(err("commands.ride.not_riding", vec![t.name_json()])) };
        g.dismount(t.eid());
        let vn = g.entity_snap(v).map(|s| Target::Ent(t.dim(), s).name_json()).unwrap_or_default();
        ctx.ok(err("commands.ride.dismount.success", vec![t.name_json(), vn]), true);
    }
    Ok(1)
}

fn attribute(ctx: &mut Ctx) -> CR {
    use crate::game::combat::attr::{Attr, Modifier, Op};
    let t = ctx.target("target")?;
    let an = ctx.str("attribute");
    let a = Attr::from_name(an.trim_start_matches("minecraft:")).ok_or_else(|| err("argument.resource.not_found", vec![json!(an)]))?;
    let aname = json!({ "translate": format!("attribute.name.{}", a.name()) });
    let words: Vec<String> = ctx.line.split_whitespace().map(String::from).collect();
    let after: Vec<&str> = words.iter().skip(3).map(|s| s.as_str()).collect();
    let scale = if ctx.has("scale") { ctx.f64("scale") } else { 1.0 };
    let g = game_of(ctx, &t).clone();
    let tn = t.name_json();
    let mut run = |f: &mut dyn FnMut(&mut crate::game::combat::attr::Map) -> Result<(Value, i32), Value>| -> CR {
        let r = match &t {
            Target::Player(p) => g.with_player(p, |l, _, _| f(&mut l.attrs)),
            Target::Ent(_, s) => g.with_mob(s.eid, |l, _, _| f(&mut l.attrs)).unwrap_or_else(|| Err(err("commands.attribute.failed.entity", vec![tn.clone()]))),
        };
        let (m, n) = r?;
        ctx.ok(m, true);
        Ok(n)
    };
    match after.as_slice() {
        ["get", ..] => run(&mut |m| {
            if !m.has(a) {
                return Err(err("commands.attribute.failed.no_attribute", vec![tn.clone(), aname.clone()]));
            }
            let v = m.value(a);
            Ok((err("commands.attribute.value.get.success", vec![aname.clone(), tn.clone(), json!(v)]), (v * scale) as i32))
        }),
        ["base", "get", ..] => run(&mut |m| {
            if !m.has(a) {
                return Err(err("commands.attribute.failed.no_attribute", vec![tn.clone(), aname.clone()]));
            }
            let v = m.base(a);
            Ok((err("commands.attribute.base_value.get.success", vec![aname.clone(), tn.clone(), json!(v)]), (v * scale) as i32))
        }),
        ["base", "set", ..] => {
            let v = ctx.f64("value");
            run(&mut |m| {
                if !m.has(a) {
                    return Err(err("commands.attribute.failed.no_attribute", vec![tn.clone(), aname.clone()]));
                }
                m.set_base(a, v);
                Ok((err("commands.attribute.base_value.set.success", vec![aname.clone(), tn.clone(), json!(v)]), 1))
            })
        }
        ["base", "reset", ..] => run(&mut |m| {
            let v = a.default_value();
            m.set_base(a, v);
            Ok((err("commands.attribute.base_value.reset.success", vec![aname.clone(), tn.clone(), json!(v)]), 1))
        }),
        ["modifier", "add", ..] => {
            let id = ctx.str("id");
            let v = ctx.f64("value");
            let op = Op::parse(words.last().map(|s| s.as_str()).unwrap_or("add_value")).unwrap_or(Op::AddValue);
            run(&mut |m| {
                if !m.has(a) {
                    return Err(err("commands.attribute.failed.no_attribute", vec![tn.clone(), aname.clone()]));
                }
                if m.get(a).is_some_and(|i| i.modifiers.iter().any(|x| x.id == id)) {
                    return Err(err("commands.attribute.failed.modifier_already_present", vec![json!(id), aname.clone(), tn.clone()]));
                }
                m.add_modifier(a, Modifier { id: id.clone(), amount: v, op });
                Ok((err("commands.attribute.modifier.add.success", vec![json!(id), aname.clone(), tn.clone()]), 1))
            })
        }
        ["modifier", "remove", ..] => {
            let id = ctx.str("id");
            run(&mut |m| {
                if !m.remove_modifier(a, &id) {
                    return Err(err("commands.attribute.failed.no_modifier", vec![aname.clone(), tn.clone(), json!(id)]));
                }
                Ok((err("commands.attribute.modifier.remove.success", vec![json!(id), aname.clone(), tn.clone()]), 1))
            })
        }
        _ => {
            let id = ctx.str("id");
            run(&mut |m| {
                let v = m.get(a).and_then(|i| i.modifiers.iter().find(|x| x.id == id)).map(|x| x.amount).ok_or_else(|| err("commands.attribute.failed.no_modifier", vec![aname.clone(), tn.clone(), json!(id)]))?;
                Ok((err("commands.attribute.modifier.value.get.success", vec![json!(id), aname.clone(), tn.clone(), json!(v)]), (v * scale) as i32))
            })
        }
    }
}

fn broadcast_chat(ctx: &Ctx, v: &Value) {
    let mut body = crate::nbt::Tag::from_json(v).to_network_bytes();
    crate::net::codec::Out::bool(&mut body, false);
    ctx.st.broadcast_all(ctx.st.game.encode("system_chat", &body));
    tracing::info!("{}", super::render_plain(v));
}

fn say(ctx: &mut Ctx) -> CR {
    let m = ctx.message("message");
    broadcast_chat(ctx, &json!({ "translate": "chat.type.announcement", "with": [ctx.src.display.clone(), m] }));
    Ok(1)
}

fn me(ctx: &mut Ctx) -> CR {
    let m = ctx.message("action");
    broadcast_chat(ctx, &json!({ "translate": "chat.type.emote", "with": [ctx.src.display.clone(), m] }));
    Ok(1)
}

fn msg(ctx: &mut Ctx) -> CR {
    let m = ctx.message("message");
    let players = ctx.players("targets")?;
    for p in &players {
        let v = json!({ "translate": "commands.message.display.incoming", "with": [ctx.src.display.clone(), m.clone()], "color": "gray", "italic": true });
        let mut body = crate::nbt::Tag::from_json(&v).to_network_bytes();
        crate::net::codec::Out::bool(&mut body, false);
        p.send(ctx.st.game.encode("system_chat", &body));
        ctx.reply(json!({ "translate": "commands.message.display.outgoing", "with": [{ "text": p.name }, m.clone()], "color": "gray", "italic": true }));
    }
    Ok(players.len() as i32)
}

fn teammsg(ctx: &mut Ctx) -> CR {
    let me = ctx.me_target().ok_or_else(|| err("permissions.requires.entity", vec![]))?;
    let team = state::read(|s| state::team_of(s, &me.holder()).and_then(|n| s.teams.get(&n).map(|t| (n.clone(), t.clone()))));
    let Some((tn, team)) = team else { return Err(err("commands.teammsg.failed.noteam", vec![])) };
    let m = ctx.message("message");
    let tdisp = json!({ "text": "", "extra": [{ "text": "[" }, team.display.clone(), { "text": "]" }], "color": team.color });
    let v = json!({ "translate": "chat.type.team.text", "with": [tdisp, ctx.src.display.clone(), m] });
    let mut body = crate::nbt::Tag::from_json(&v).to_network_bytes();
    crate::net::codec::Out::bool(&mut body, false);
    let pkt = ctx.st.game.encode("system_chat", &body);
    let mut n = 0;
    for p in ctx.st.all_players() {
        if team.members.contains(&p.name) {
            p.send(pkt.clone());
            n += 1;
        }
    }
    let _ = tn;
    Ok(n)
}

fn tellraw(ctx: &mut Ctx) -> CR {
    let v = ctx.text("message");
    let mut body = crate::nbt::Tag::from_json(&v).to_network_bytes();
    crate::net::codec::Out::bool(&mut body, false);
    let pkt = ctx.st.game.encode("system_chat", &body);
    let players = ctx.players("targets")?;
    for p in &players {
        p.send(pkt.clone());
    }
    Ok(players.len() as i32)
}

fn title(ctx: &mut Ctx) -> CR {
    let players = ctx.players("targets")?;
    let words: Vec<&str> = ctx.line.split_whitespace().collect();
    let kind = words.iter().skip(2).find(|w| ["clear", "reset", "title", "subtitle", "actionbar", "times"].contains(w)).copied().unwrap_or("clear");
    let g = &ctx.st.game;
    let pkt = match kind {
        "clear" | "reset" => {
            let mut b = Vec::new();
            crate::net::codec::Out::bool(&mut b, kind == "reset");
            g.encode("clear_titles", &b)
        }
        "times" => {
            let mut b = Vec::new();
            crate::net::codec::Out::i32(&mut b, ctx.int("fadeIn"));
            crate::net::codec::Out::i32(&mut b, ctx.int("stay"));
            crate::net::codec::Out::i32(&mut b, ctx.int("fadeOut"));
            g.encode("set_titles_animation", &b)
        }
        k => {
            let v = ctx.text("title");
            let b = crate::nbt::Tag::from_json(&v).to_network_bytes();
            g.encode(
                match k {
                    "title" => "set_title_text",
                    "subtitle" => "set_subtitle_text",
                    _ => "set_action_bar_text",
                },
                &b,
            )
        }
    };
    for p in &players {
        p.send(pkt.clone());
    }
    let ts: Vec<Target> = players.iter().cloned().map(Target::Player).collect();
    let key = match kind {
        "clear" => "cleared",
        "reset" => "reset",
        "times" => "times",
        "title" => "title",
        "subtitle" => "subtitle",
        _ => "actionbar",
    };
    ctx.ok(one_or_many(&ts, &format!("commands.title.{key}.single"), &format!("commands.title.{key}.multiple"), vec![]), true);
    Ok(players.len() as i32)
}

fn list(ctx: &mut Ctx) -> CR {
    let uuids = ctx.line.split_whitespace().any(|w| w == "uuids");
    let all = ctx.st.all_players();
    let names: Vec<String> = all.iter().map(|p| if uuids { format!("{} ({})", p.name, p.uuid.hyphenated()) } else { p.name.clone() }).collect();
    ctx.ok(err("commands.list.players", vec![json!(all.len()), json!(ctx.st.cfg.server.max_players), json!(names.join(", "))]), false);
    Ok(all.len() as i32)
}

fn seed(ctx: &mut Ctx) -> CR {
    let s = ctx.st.seed;
    ctx.ok(err("commands.seed.success", vec![json!({ "text": format!("[{s}]"), "color": "green", "clickEvent": { "action": "copy_to_clipboard", "value": s.to_string() }, "hoverEvent": { "action": "show_text", "value": { "translate": "chat.copy.click" } } })]), false);
    Ok(s as i32)
}

fn spawnpoint(ctx: &mut Ctx) -> CR {
    let players = if ctx.has("targets") { ctx.players("targets")? } else { vec![ctx.me()?] };
    let pos = if ctx.has("pos") { ctx.block_pos("pos") } else { [ctx.src.pos[0].floor() as i32, ctx.src.pos[1].floor() as i32, ctx.src.pos[2].floor() as i32] };
    let angle = if ctx.has("angle") { ctx.coords("angle").rotation(ctx.src.rot)[0] } else { 0.0 };
    for p in &players {
        let mut v = p.vit.lock();
        v.respawn = Some(pos);
        v.respawn_dim = ctx.src.dim.index();
        v.respawn_forced = true;
    }
    let ts: Vec<Target> = players.iter().cloned().map(Target::Player).collect();
    let extra = vec![json!(pos[0]), json!(pos[1]), json!(pos[2]), json!(angle), json!(0.0), json!(ctx.src.dim.key())];
    ctx.ok(one_or_many(&ts, "commands.spawnpoint.success.single", "commands.spawnpoint.success.multiple", extra), true);
    Ok(players.len() as i32)
}

fn setworldspawn(ctx: &mut Ctx) -> CR {
    if ctx.src.dim != Dim::Overworld {
        return Err(err("commands.setworldspawn.failure.not_overworld", vec![]));
    }
    let pos = if ctx.has("pos") { ctx.block_pos("pos") } else { [ctx.src.pos[0].floor() as i32, ctx.src.pos[1].floor() as i32, ctx.src.pos[2].floor() as i32] };
    let angle = if ctx.has("angle") { ctx.coords("angle").rotation(ctx.src.rot)[0] } else { 0.0 };
    ctx.st.set_spawn([pos[0] as f64 + 0.5, pos[1] as f64, pos[2] as f64 + 0.5]);
    let mut b = Vec::new();
    {
        use crate::net::codec::Out;
        b.str("minecraft:overworld").pos(pos[0], pos[1], pos[2]).f32(angle).f32(0.0);
    }
    ctx.st.broadcast_all(ctx.st.game.encode("set_default_spawn_position", &b));
    ctx.ok(err("commands.setworldspawn.success", vec![json!(pos[0]), json!(pos[1]), json!(pos[2]), json!(angle), json!(0.0), json!("minecraft:overworld")]), true);
    Ok(1)
}

fn spawn(ctx: &mut Ctx) -> CR {
    let p = ctx.me()?;
    let s = ctx.st.spawn();
    p.requests.lock().push(Request::Travel(Dim::Overworld, s, 0.0, 0.0));
    Ok(1)
}

fn help(ctx: &mut Ctx) -> CR {
    let t = super::tree();
    let filter = ctx.str("command");
    let mut n = 0;
    for &c in &t.nodes[0].children {
        let node = &t.nodes[c];
        if node.level > ctx.src.level || (!filter.is_empty() && !filter.split_whitespace().next().is_some_and(|f| f == node.name)) {
            continue;
        }
        ctx.reply(json!({ "text": format!("/{}{}", node.name, usage(t, c, 0)) }));
        n += 1;
    }
    if n == 0 {
        return Err(err("commands.help.failed", vec![]));
    }
    Ok(n)
}

/// A short usage string (`<a> [<b>]`, `(x|y)`).
fn usage(t: &Tree, node: usize, depth: usize) -> String {
    let n = &t.nodes[node];
    if depth > 2 || n.children.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = n
        .children
        .iter()
        .map(|c| {
            let cn = &t.nodes[*c];
            match cn.kind {
                super::tree::Kind::Literal => cn.name.to_string(),
                _ => format!("<{}>", cn.name),
            }
        })
        .collect();
    let inner = if parts.len() == 1 { format!("{}{}", parts[0], usage(t, n.children[0], depth + 1)) } else { format!("({})", parts.join("|")) };
    if n.exec.is_some() {
        format!(" [{inner}]")
    } else {
        format!(" {inner}")
    }
}

