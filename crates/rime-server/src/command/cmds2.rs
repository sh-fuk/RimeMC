//! World, scoreboard and server commands.

use super::args::{score_slot, Arg, ArgType as A};
use super::state::{self, Bar, Objective, Team};
use super::tree::{Tree, B};
use super::{err, names_json, Ctx, Target, CR};
use crate::game::Request;
use crate::nbt::Tag;
use crate::net::codec::Out;
use crate::registry::BlockState;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

const ENT: A = A::Entity { single: false, players: false };
const PLAYERS: A = A::Entity { single: false, players: true };

pub fn register(t: &mut Tree) {
    // ---------------------------------------------------------- time & weather
    t.cmd("time", 2, |b| {
        b.lit("set", |b| {
            for w in ["day", "noon", "night", "midnight"] {
                b.lit(w, |b| b.exec(time));
            }
            b.arg("time", A::Time(0), |b| b.exec(time));
        });
        b.lit("add", |b| {
            b.arg("time", A::Time(0), |b| b.exec(time));
        });
        b.lit("query", |b| {
            for w in ["daytime", "gametime", "day"] {
                b.lit(w, |b| b.exec(time));
            }
        });
    });
    t.cmd("weather", 2, |b| {
        for w in ["clear", "rain", "thunder"] {
            b.lit(w, |b| {
                b.exec(weather);
                b.arg("duration", A::Time(1), |b| b.exec(weather));
            });
        }
    });
    t.cmd("gamerule", 2, |b| {
        for (name, _, min, max) in crate::game::rules::RULES {
            b.lit(name, |b| {
                b.exec(gamerule);
                let ty = if min > max { A::Bool } else { A::Int(*min, *max) };
                b.arg("value", ty, |b| b.exec(gamerule));
            });
        }
    });
    // ---------------------------------------------------------- blocks
    t.cmd("setblock", 2, |b| {
        b.arg("pos", A::BlockPos, |b| {
            b.arg("block", A::BlockState, |b| {
                b.exec(setblock);
                for m in ["destroy", "keep", "replace", "strict"] {
                    b.lit(m, |b| b.exec(setblock));
                }
            });
        });
    });
    t.cmd("fill", 2, |b| {
        b.arg("from", A::BlockPos, |b| {
            b.arg("to", A::BlockPos, |b| {
                b.arg("block", A::BlockState, |b| {
                    b.exec(fill);
                    for m in ["destroy", "hollow", "keep", "outline", "strict"] {
                        b.lit(m, |b| b.exec(fill));
                    }
                    b.lit("replace", |b| {
                        b.exec(fill);
                        b.arg("filter", A::BlockPredicate, |b| {
                            b.exec(fill);
                            for m in ["destroy", "hollow", "keep", "outline", "strict"] {
                                b.lit(m, |b| b.exec(fill));
                            }
                        });
                    });
                });
            });
        });
    });
    t.cmd("clone", 2, |b| {
        b.arg("begin", A::BlockPos, |b| {
            b.arg("end", A::BlockPos, |b| {
                b.arg("destination", A::BlockPos, |b| {
                    let modes = |b: &mut B| {
                        b.exec(clone);
                        for m in ["force", "move", "normal"] {
                            b.lit(m, |b| b.exec(clone));
                        }
                    };
                    modes(b);
                    b.lit("replace", modes);
                    b.lit("masked", modes);
                    b.lit("filtered", |b| {
                        b.arg("filter", A::BlockPredicate, modes);
                    });
                });
            });
        });
    });
    // ---------------------------------------------------------- sound & particles
    t.cmd("playsound", 2, |b| {
        b.arg("sound", A::Id, |b| {
            for src in SOUND_SOURCES {
                b.lit(src, |b| {
                    b.arg("targets", PLAYERS, |b| {
                        b.exec(playsound);
                        b.arg("pos", A::Vec3, |b| {
                            b.exec(playsound);
                            b.arg("volume", A::Float(0.0, f32::MAX), |b| {
                                b.exec(playsound);
                                b.arg("pitch", A::Float(0.0, 2.0), |b| {
                                    b.exec(playsound);
                                    b.arg("minVolume", A::Float(0.0, 1.0), |b| b.exec(playsound));
                                });
                            });
                        });
                    });
                });
            }
        });
    });
    t.cmd("stopsound", 2, |b| {
        b.arg("targets", PLAYERS, |b| {
            b.exec(stopsound);
            b.lit("*", |b| {
                b.exec(stopsound);
                b.arg("sound", A::Id, |b| b.exec(stopsound));
            });
            for src in SOUND_SOURCES {
                b.lit(src, |b| {
                    b.exec(stopsound);
                    b.arg("sound", A::Id, |b| b.exec(stopsound));
                });
            }
        });
    });
    t.cmd("particle", 2, |b| {
        b.arg("name", A::Particle, |b| {
            b.exec(particle);
            b.arg("pos", A::Vec3, |b| {
                b.exec(particle);
                b.arg("delta", A::Vec3, |b| {
                    b.arg("speed", A::Float(0.0, f32::MAX), |b| {
                        b.arg("count", A::Int(0, i32::MAX), |b| {
                            b.exec(particle);
                            for m in ["force", "normal"] {
                                b.lit(m, |b| {
                                    b.exec(particle);
                                    b.arg("viewers", PLAYERS, |b| b.exec(particle));
                                });
                            }
                        });
                    });
                });
            });
        });
    });
    // ---------------------------------------------------------- scoreboard
    t.cmd("scoreboard", 2, |b| {
        b.lit("objectives", |b| {
            b.lit("list", |b| b.exec(objectives));
            b.lit("add", |b| {
                b.arg("objective", A::Word, |b| {
                    b.arg("criteria", A::Criteria, |b| {
                        b.exec(objectives);
                        b.arg("displayName", A::Component, |b| b.exec(objectives));
                    });
                });
            });
            b.lit("remove", |b| {
                b.arg("objective", A::Objective, |b| b.exec(objectives));
            });
            b.lit("setdisplay", |b| {
                b.arg("slot", A::ScoreSlot, |b| {
                    b.exec(objectives);
                    b.arg("objective", A::Objective, |b| b.exec(objectives));
                });
            });
            b.lit("modify", |b| {
                b.arg("objective", A::Objective, |b| {
                    b.lit("displayname", |b| {
                        b.arg("displayName", A::Component, |b| b.exec(objectives));
                    });
                    b.lit("rendertype", |b| {
                        b.lit("hearts", |b| b.exec(objectives));
                        b.lit("integer", |b| b.exec(objectives));
                    });
                    b.lit("displayautoupdate", |b| {
                        b.arg("value", A::Bool, |b| b.exec(objectives));
                    });
                });
            });
        });
        b.lit("players", |b| {
            b.lit("list", |b| {
                b.exec(players);
                b.arg("target", A::ScoreHolder { multiple: false }, |b| b.exec(players));
            });
            for op in ["set", "add", "remove"] {
                b.lit(op, |b| {
                    b.arg("targets", A::ScoreHolder { multiple: true }, |b| {
                        b.arg("objective", A::Objective, |b| {
                            b.arg("score", A::Int(if op == "set" { i32::MIN } else { 0 }, i32::MAX), |b| b.exec(players));
                        });
                    });
                });
            }
            b.lit("get", |b| {
                b.arg("target", A::ScoreHolder { multiple: false }, |b| {
                    b.arg("objective", A::Objective, |b| b.exec(players));
                });
            });
            b.lit("reset", |b| {
                b.arg("targets", A::ScoreHolder { multiple: true }, |b| {
                    b.exec(players);
                    b.arg("objective", A::Objective, |b| b.exec(players));
                });
            });
            b.lit("enable", |b| {
                b.arg("targets", A::ScoreHolder { multiple: true }, |b| {
                    b.arg("objective", A::Objective, |b| b.exec(players));
                });
            });
            b.lit("operation", |b| {
                b.arg("targets", A::ScoreHolder { multiple: true }, |b| {
                    b.arg("targetObjective", A::Objective, |b| {
                        b.arg("operation", A::Operation, |b| {
                            b.arg("source", A::ScoreHolder { multiple: true }, |b| {
                                b.arg("sourceObjective", A::Objective, |b| b.exec(players));
                            });
                        });
                    });
                });
            });
        });
    });
    t.cmd("trigger", 0, |b| {
        b.arg("objective", A::Objective, |b| {
            b.exec(trigger);
            b.lit("add", |b| {
                b.arg("value", A::Int(i32::MIN, i32::MAX), |b| b.exec(trigger));
            });
            b.lit("set", |b| {
                b.arg("value", A::Int(i32::MIN, i32::MAX), |b| b.exec(trigger));
            });
        });
    });
    t.cmd("team", 2, |b| {
        b.lit("list", |b| {
            b.exec(team);
            b.arg("team", A::Team, |b| b.exec(team));
        });
        b.lit("add", |b| {
            b.arg("team", A::Word, |b| {
                b.exec(team);
                b.arg("displayName", A::Component, |b| b.exec(team));
            });
        });
        b.lit("remove", |b| {
            b.arg("team", A::Team, |b| b.exec(team));
        });
        b.lit("empty", |b| {
            b.arg("team", A::Team, |b| b.exec(team));
        });
        b.lit("join", |b| {
            b.arg("team", A::Team, |b| {
                b.exec(team);
                b.arg("members", A::ScoreHolder { multiple: true }, |b| b.exec(team));
            });
        });
        b.lit("leave", |b| {
            b.arg("members", A::ScoreHolder { multiple: true }, |b| b.exec(team));
        });
        b.lit("modify", |b| {
            b.arg("team", A::Team, |b| {
                b.lit("displayName", |b| {
                    b.arg("displayName", A::Component, |b| b.exec(team));
                });
                b.lit("color", |b| {
                    b.arg("value", A::Color, |b| b.exec(team));
                });
                for k in ["friendlyFire", "seeFriendlyInvisibles"] {
                    b.lit(k, |b| {
                        b.arg("allowed", A::Bool, |b| b.exec(team));
                    });
                }
                for (k, vals) in [
                    ("nametagVisibility", &["never", "hideForOtherTeams", "hideForOwnTeam", "always"][..]),
                    ("deathMessageVisibility", &["never", "hideForOtherTeams", "hideForOwnTeam", "always"][..]),
                    ("collisionRule", &["always", "never", "pushOtherTeams", "pushOwnTeam"][..]),
                ] {
                    b.lit(k, |b| {
                        for v in vals {
                            b.lit(v, |b| b.exec(team));
                        }
                    });
                }
                for k in ["prefix", "suffix"] {
                    b.lit(k, |b| {
                        b.arg(k, A::Component, |b| b.exec(team));
                    });
                }
            });
        });
    });
    t.cmd("bossbar", 2, |b| {
        b.lit("add", |b| {
            b.arg("id", A::Id, |b| {
                b.arg("name", A::Component, |b| b.exec(bossbar));
            });
        });
        b.lit("remove", |b| {
            b.arg("id", A::Id, |b| b.exec(bossbar));
        });
        b.lit("list", |b| b.exec(bossbar));
        b.lit("get", |b| {
            b.arg("id", A::Id, |b| {
                for k in ["max", "players", "value", "visible"] {
                    b.lit(k, |b| b.exec(bossbar));
                }
            });
        });
        b.lit("set", |b| {
            b.arg("id", A::Id, |b| {
                b.lit("color", |b| {
                    for c in ["blue", "green", "pink", "purple", "red", "white", "yellow"] {
                        b.lit(c, |b| b.exec(bossbar));
                    }
                });
                b.lit("max", |b| {
                    b.arg("max", A::Int(1, i32::MAX), |b| b.exec(bossbar));
                });
                b.lit("name", |b| {
                    b.arg("name", A::Component, |b| b.exec(bossbar));
                });
                b.lit("players", |b| {
                    b.exec(bossbar);
                    b.arg("targets", PLAYERS, |b| b.exec(bossbar));
                });
                b.lit("style", |b| {
                    for s in ["notched_6", "notched_10", "notched_12", "notched_20", "progress"] {
                        b.lit(s, |b| b.exec(bossbar));
                    }
                });
                b.lit("value", |b| {
                    b.arg("value", A::Int(0, i32::MAX), |b| b.exec(bossbar));
                });
                b.lit("visible", |b| {
                    b.arg("visible", A::Bool, |b| b.exec(bossbar));
                });
            });
        });
    });
    t.cmd("worldborder", 2, |b| {
        b.lit("add", |b| {
            b.arg("distance", A::Double(-5.9999968e7, 5.9999968e7), |b| {
                b.exec(worldborder);
                b.arg("time", A::Time(0), |b| b.exec(worldborder));
            });
        });
        b.lit("set", |b| {
            b.arg("distance", A::Double(-5.9999968e7, 5.9999968e7), |b| {
                b.exec(worldborder);
                b.arg("time", A::Time(0), |b| b.exec(worldborder));
            });
        });
        b.lit("center", |b| {
            b.arg("pos", A::Vec2, |b| b.exec(worldborder));
        });
        b.lit("damage", |b| {
            b.lit("amount", |b| {
                b.arg("damagePerBlock", A::Float(0.0, f32::MAX), |b| b.exec(worldborder));
            });
            b.lit("buffer", |b| {
                b.arg("distance", A::Double(0.0, f64::MAX), |b| b.exec(worldborder));
            });
        });
        b.lit("get", |b| b.exec(worldborder));
        b.lit("warning", |b| {
            b.lit("distance", |b| {
                b.arg("distance", A::Int(0, i32::MAX), |b| b.exec(worldborder));
            });
            b.lit("time", |b| {
                b.arg("time", A::Time(0), |b| b.exec(worldborder));
            });
        });
    });
    // ---------------------------------------------------------- server
    t.cmd("op", 3, |b| {
        b.arg("targets", A::Profile, |b| b.exec(op));
    });
    t.cmd("deop", 3, |b| {
        b.arg("targets", A::Profile, |b| b.exec(op));
    });
    t.cmd("kick", 3, |b| {
        b.arg("targets", PLAYERS, |b| {
            b.exec(kick);
            b.arg("reason", A::Message, |b| b.exec(kick));
        });
    });
    t.cmd("ban", 3, |b| {
        b.arg("targets", A::Profile, |b| {
            b.exec(ban);
            b.arg("reason", A::Message, |b| b.exec(ban));
        });
    });
    t.cmd("ban-ip", 3, |b| {
        b.arg("target", A::Word, |b| {
            b.exec(ban_ip);
            b.arg("reason", A::Message, |b| b.exec(ban_ip));
        });
    });
    t.cmd("pardon", 3, |b| {
        b.arg("targets", A::Word, |b| b.exec(pardon));
    });
    t.cmd("pardon-ip", 3, |b| {
        b.arg("target", A::Word, |b| b.exec(pardon));
    });
    t.cmd("banlist", 3, |b| {
        b.exec(banlist);
        b.lit("players", |b| b.exec(banlist));
        b.lit("ips", |b| b.exec(banlist));
    });
    t.cmd("whitelist", 3, |b| {
        for w in ["on", "off", "list", "reload"] {
            b.lit(w, |b| b.exec(whitelist));
        }
        for w in ["add", "remove"] {
            b.lit(w, |b| {
                b.arg("targets", A::Profile, |b| b.exec(whitelist));
            });
        }
    });
    t.cmd("stop", 4, |b| b.exec(stop));
    t.cmd("save-all", 4, |b| {
        b.exec(save_all);
        b.lit("flush", |b| b.exec(save_all));
    });
    t.cmd("save-on", 4, |b| b.exec(save_all));
    t.cmd("save-off", 4, |b| b.exec(save_all));
    t.cmd("reload", 2, |b| b.exec(reload));
}

const SOUND_SOURCES: [&str; 10] = ["master", "music", "record", "weather", "block", "hostile", "neutral", "player", "ambient", "voice"];

fn words(ctx: &Ctx) -> Vec<String> {
    ctx.line.split_whitespace().map(String::from).collect()
}

// ------------------------------------------------------------------ time, weather, rules

fn time(ctx: &mut Ctx) -> CR {
    let w = words(ctx);
    let g = ctx.st.game.clone();
    match (w.get(1).map(|s| s.as_str()), w.get(2).map(|s| s.as_str())) {
        (Some("query"), Some(q)) => {
            let v = match q {
                "daytime" => g.day_time() as i64,
                "gametime" => g.game_time() as i64,
                _ => (g.clock() / 24000) as i64,
            };
            let v = (v % i32::MAX as i64) as i32;
            ctx.ok(err("commands.time.query", vec![json!(v)]), false);
            Ok(v)
        }
        (Some("add"), _) => {
            let n = ctx.int("time").max(0) as u64;
            for lv in ctx.st.levels() {
                lv.add_time(n);
            }
            let d = g.day_time() as i32;
            ctx.ok(err("commands.time.set", vec![json!(d)]), true);
            Ok(d)
        }
        (Some("set"), Some(v)) => {
            let t = match v {
                "day" => 1000,
                "noon" => 6000,
                "night" => 13000,
                "midnight" => 18000,
                _ => ctx.int("time").max(0) as u64,
            };
            for lv in ctx.st.levels() {
                lv.set_day_time(t);
            }
            ctx.ok(err("commands.time.set", vec![json!(t)]), true);
            Ok((t % 24000) as i32)
        }
        _ => Err(err("command.unknown.argument", vec![])),
    }
}

fn weather(ctx: &mut Ctx) -> CR {
    use rand::Rng;
    let w = words(ctx);
    let kind = w.get(1).map(|s| s.as_str()).unwrap_or("clear");
    let mut rng = rand::thread_rng();
    let dur = if ctx.has("duration") { ctx.int("duration") } else { -1 };
    let g = &ctx.st.game;
    match kind {
        "clear" => g.set_weather(if dur < 0 { rng.gen_range(12000..=180000) } else { dur }, 0, false, false),
        "rain" => g.set_weather(0, if dur < 0 { rng.gen_range(12000..=24000) } else { dur }, true, false),
        _ => g.set_weather(0, if dur < 0 { rng.gen_range(3600..=15600) } else { dur }, true, true),
    }
    ctx.ok(err(&format!("commands.weather.set.{kind}"), vec![]), true);
    Ok(dur.max(0))
}

fn gamerule(ctx: &mut Ctx) -> CR {
    let w = words(ctx);
    let name = w.get(1).cloned().unwrap_or_default();
    let key = name.trim_start_matches("minecraft:").to_string();
    let bool_rule = crate::game::rules::is_bool(&key);
    let fmt = |v: i32| if bool_rule { json!((v != 0).to_string()) } else { json!(v.to_string()) };
    if !ctx.has("value") {
        let v = crate::game::rules::get(&key);
        ctx.ok(err("commands.gamerule.query", vec![json!(key), fmt(v)]), false);
        return Ok(v);
    }
    let v = match ctx.args.get("value") {
        Some(Arg::Bool(b)) => *b as i32,
        _ => ctx.int("value"),
    };
    crate::game::rules::set(&key, v).map_err(|e| json!({ "text": e }))?;
    on_rule_changed(ctx, &key, v);
    ctx.ok(err("commands.gamerule.set", vec![json!(key), fmt(v)]), true);
    Ok(v)
}

/// Rules the client must hear about.
fn on_rule_changed(ctx: &Ctx, key: &str, v: i32) {
    match key {
        "reduced_debug_info" | "immediate_respawn" => {
            for p in ctx.st.all_players() {
                if key == "reduced_debug_info" {
                    let mut b = Vec::new();
                    b.i32(p.eid).u8(if v != 0 { 22 } else { 23 });
                    p.send(ctx.st.game.encode("entity_event", &b));
                } else {
                    let mut b = Vec::new();
                    b.u8(11).f32(if v != 0 { 1.0 } else { 0.0 });
                    p.send(ctx.st.game.encode("game_event", &b));
                }
            }
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ blocks

fn max_blocks() -> i64 {
    crate::game::rules::get("max_block_modifications").max(1) as i64
}

/// Sets a block for a command: no drops unless `destroy`, neighbours updated later.
fn put_block(g: &crate::game::Game, p: [i32; 3], s: BlockState, destroy: bool, nbt: Option<&Tag>) -> bool {
    let Some(cur) = g.world.get_block(p[0], p[1], p[2]) else { return false };
    if cur == s && nbt.is_none() {
        return false;
    }
    g.world.mark_edited(p[0], p[2]);
    if destroy {
        g.change_block(p, s, true, None);
    } else {
        // clear the old block entity so containers do not spill
        g.world.set_block_entity(p, None, false);
        g.set_block(p, s, crate::game::level::flags::CLIENTS);
    }
    if let Some(t) = nbt {
        let mut base = g.world.block_entity(p).unwrap_or_else(|| match crate::game::blockentity::be_id(s.name()) {
            Some(id) => Tag::Compound(vec![("id".into(), Tag::String(format!("minecraft:{id}")))]),
            None => Tag::Compound(vec![]),
        });
        if let (Tag::Compound(b), Tag::Compound(extra)) = (&mut base, t) {
            for (k, v) in extra {
                b.retain(|(kk, _)| kk != k);
                b.push((k.clone(), v.clone()));
            }
        }
        g.set_block_entity_visible(p, base);
    }
    true
}

fn update_area(g: &crate::game::Game, lo: [i32; 3], hi: [i32; 3]) {
    let _lv = g.level.lock();
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                if let Some(b) = g.world.get_block(x, y, z) {
                    g.update_neighbors_at([x, y, z], b);
                }
            }
        }
    }
}

fn loaded_box(g: &crate::game::Game, lo: [i32; 3], hi: [i32; 3]) -> bool {
    let (min, max) = (crate::world::chunk::MIN_Y, crate::world::chunk::MIN_Y + crate::world::chunk::HEIGHT);
    if lo[1] < min || hi[1] >= max {
        return false;
    }
    for cx in (lo[0] >> 4)..=(hi[0] >> 4) {
        for cz in (lo[2] >> 4)..=(hi[2] >> 4) {
            if g.world.get_block(cx << 4, lo[1], cz << 4).is_none() {
                return false;
            }
        }
    }
    true
}

fn setblock(ctx: &mut Ctx) -> CR {
    let p = ctx.block_pos("pos");
    let b = ctx.block("block");
    let mode = words(ctx).last().cloned().unwrap_or_default();
    let g = ctx.level().clone();
    if !loaded_box(&g, p, p) {
        return Err(err("argument.pos.unloaded", vec![]));
    }
    let cur = g.world.get_block(p[0], p[1], p[2]).unwrap_or(BlockState(0));
    if mode == "keep" && !cur.is_air() {
        return Err(err("commands.setblock.failed", vec![]));
    }
    if !put_block(&g, p, b.state, mode == "destroy", b.nbt.as_ref()) {
        return Err(err("commands.setblock.failed", vec![]));
    }
    if mode != "strict" {
        update_area(&g, p, p);
    }
    ctx.ok(err("commands.setblock.success", vec![json!(p[0]), json!(p[1]), json!(p[2])]), true);
    Ok(1)
}

fn fill(ctx: &mut Ctx) -> CR {
    let a = ctx.block_pos("from");
    let c = ctx.block_pos("to");
    let b = ctx.block("block");
    let filter = if ctx.has("filter") { Some(ctx.block("filter")) } else { None };
    let w = words(ctx);
    let mode = w.last().map(|s| s.as_str()).filter(|m| ["destroy", "hollow", "keep", "outline", "strict"].contains(m)).unwrap_or("replace").to_string();
    let lo = [a[0].min(c[0]), a[1].min(c[1]), a[2].min(c[2])];
    let hi = [a[0].max(c[0]), a[1].max(c[1]), a[2].max(c[2])];
    let vol = (hi[0] - lo[0] + 1) as i64 * (hi[1] - lo[1] + 1) as i64 * (hi[2] - lo[2] + 1) as i64;
    if vol > max_blocks() {
        return Err(err("commands.fill.toobig", vec![json!(max_blocks()), json!(vol)]));
    }
    let g = ctx.level().clone();
    if !loaded_box(&g, lo, hi) {
        return Err(err("argument.pos.unloaded", vec![]));
    }
    let data = g.data.clone();
    let mut n = 0;
    let air = BlockState::by_name("air");
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                let p = [x, y, z];
                let edge = x == lo[0] || x == hi[0] || y == lo[1] || y == hi[1] || z == lo[2] || z == hi[2];
                let cur = g.world.get_block(x, y, z).unwrap_or(air);
                if let Some(f) = &filter {
                    if !f.test(cur, &data) {
                        continue;
                    }
                }
                let target = match mode.as_str() {
                    "hollow" if !edge => air,
                    "outline" if !edge => continue,
                    "keep" if !cur.is_air() => continue,
                    _ => b.state,
                };
                if put_block(&g, p, target, mode == "destroy", if target == b.state { b.nbt.as_ref() } else { None }) {
                    n += 1;
                }
            }
        }
    }
    if mode != "strict" {
        update_area(&g, lo, hi);
    }
    if n == 0 {
        return Err(err("commands.fill.failed", vec![]));
    }
    ctx.ok(err("commands.fill.success", vec![json!(n)]), true);
    Ok(n)
}

fn clone(ctx: &mut Ctx) -> CR {
    let a = ctx.block_pos("begin");
    let c = ctx.block_pos("end");
    let d = ctx.block_pos("destination");
    let w = words(ctx);
    let has = |k: &str| w.iter().skip(4).any(|x| x == k);
    let masked = has("masked");
    let mv = has("move");
    let force = has("force");
    let filter = if ctx.has("filter") { Some(ctx.block("filter")) } else { None };
    let lo = [a[0].min(c[0]), a[1].min(c[1]), a[2].min(c[2])];
    let hi = [a[0].max(c[0]), a[1].max(c[1]), a[2].max(c[2])];
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let dhi = [d[0] + size[0], d[1] + size[1], d[2] + size[2]];
    let vol = (size[0] + 1) as i64 * (size[1] + 1) as i64 * (size[2] + 1) as i64;
    if vol > max_blocks() {
        return Err(err("commands.clone.toobig", vec![json!(max_blocks()), json!(vol)]));
    }
    let overlap = (0..3).all(|i| d[i] <= hi[i] && dhi[i] >= lo[i]);
    if overlap && !force {
        return Err(err("commands.clone.overlap", vec![]));
    }
    let g = ctx.level().clone();
    if !loaded_box(&g, lo, hi) || !loaded_box(&g, d, dhi) {
        return Err(err("argument.pos.unloaded", vec![]));
    }
    let data = g.data.clone();
    let air = BlockState::by_name("air");
    let mut copy = Vec::new();
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                let s = g.world.get_block(x, y, z).unwrap_or(air);
                if masked && s.is_air() {
                    continue;
                }
                if let Some(f) = &filter {
                    if !f.test(s, &data) {
                        continue;
                    }
                }
                copy.push(([x - lo[0], y - lo[1], z - lo[2]], s, g.world.block_entity([x, y, z])));
            }
        }
    }
    if mv {
        for (o, _, _) in &copy {
            let p = [lo[0] + o[0], lo[1] + o[1], lo[2] + o[2]];
            g.world.set_block_entity(p, None, false);
            put_block(&g, p, air, false, None);
        }
    }
    let mut n = 0;
    for (o, s, be) in &copy {
        let p = [d[0] + o[0], d[1] + o[1], d[2] + o[2]];
        let changed = put_block(&g, p, *s, false, None);
        if let Some(mut t) = be.clone() {
            if let Tag::Compound(f) = &mut t {
                f.retain(|(k, _)| !matches!(k.as_str(), "x" | "y" | "z"));
                f.push(("x".into(), Tag::Int(p[0])));
                f.push(("y".into(), Tag::Int(p[1])));
                f.push(("z".into(), Tag::Int(p[2])));
            }
            g.set_block_entity_visible(p, t);
        }
        if changed || be.is_some() {
            n += 1;
        }
    }
    update_area(&g, d, dhi);
    if mv {
        update_area(&g, lo, hi);
    }
    if n == 0 {
        return Err(err("commands.clone.failed", vec![]));
    }
    ctx.ok(err("commands.clone.success", vec![json!(n)]), true);
    Ok(n)
}

// ------------------------------------------------------------------ sound & particles

fn playsound(ctx: &mut Ctx) -> CR {
    let sound = ctx.str("sound");
    let w = words(ctx);
    let src = w.get(2).and_then(|s| SOUND_SOURCES.iter().position(|x| x == s)).unwrap_or(0) as i32;
    let players = ctx.players("targets")?;
    let pos = if ctx.has("pos") { ctx.vec3("pos") } else { ctx.src.pos };
    let volume = if ctx.has("volume") { ctx.f64("volume") as f32 } else { 1.0 };
    let pitch = if ctx.has("pitch") { ctx.f64("pitch") as f32 } else { 1.0 };
    let min_vol = ctx.f64("minVolume") as f32;
    let range = if volume > 1.0 { 16.0 * volume as f64 } else { 16.0 };
    let mut n = 0;
    for p in &players {
        let pp = p.pose.lock().pos;
        let d2: f64 = (0..3).map(|i| (pp[i] - pos[i]).powi(2)).sum();
        let (mut at, mut vol) = (pos, volume);
        if d2 > range * range {
            if min_vol <= 0.0 {
                continue;
            }
            let d = d2.sqrt();
            at = [pp[0] + (pos[0] - pp[0]) / d * 2.0, pp[1] + (pos[1] - pp[1]) / d * 2.0, pp[2] + (pos[2] - pp[2]) / d * 2.0];
            vol = min_vol;
        }
        let mut b = Vec::new();
        match crate::registry::Registries::get().id("minecraft:sound_event", &sound) {
            Some(id) => {
                b.vi(id + 1);
            }
            None => {
                b.vi(0).str(&sound).bool(false);
            }
        }
        b.vi(src).i32((at[0] * 8.0) as i32).i32((at[1] * 8.0) as i32).i32((at[2] * 8.0) as i32).f32(vol).f32(pitch).i64(rand::random());
        p.send(ctx.st.game.encode("sound", &b));
        n += 1;
    }
    if n == 0 {
        return Err(err("commands.playsound.failed", vec![]));
    }
    let ts: Vec<Target> = players.iter().cloned().map(Target::Player).collect();
    ctx.ok(if ts.len() == 1 { err("commands.playsound.success.single", vec![json!(sound), ts[0].name_json()]) } else { err("commands.playsound.success.multiple", vec![json!(sound), json!(ts.len())]) }, true);
    Ok(n)
}

fn stopsound(ctx: &mut Ctx) -> CR {
    let players = ctx.players("targets")?;
    let w = words(ctx);
    let src = w.get(2).and_then(|s| SOUND_SOURCES.iter().position(|x| x == s));
    let sound = if ctx.has("sound") { Some(ctx.str("sound")) } else { None };
    let mut b = Vec::new();
    b.u8(src.is_some() as u8 | ((sound.is_some() as u8) << 1));
    if let Some(s) = src {
        b.vi(s as i32);
    }
    if let Some(s) = &sound {
        b.str(s);
    }
    let pkt = ctx.st.game.encode("stop_sound", &b);
    for p in &players {
        p.send(pkt.clone());
    }
    let key = match (src.is_some(), sound.is_some()) {
        (false, false) => "commands.stopsound.success.sourceless.any",
        (false, true) => "commands.stopsound.success.sourceless.sound",
        (true, false) => "commands.stopsound.success.source.any",
        (true, true) => "commands.stopsound.success.source.sound",
    };
    let mut with = Vec::new();
    if let Some(s) = sound {
        with.push(json!(s));
    }
    if let Some(s) = src {
        with.insert(0, json!(SOUND_SOURCES[s]));
    }
    ctx.ok(err(key, with), true);
    Ok(players.len() as i32)
}

fn particle(ctx: &mut Ctx) -> CR {
    let name = ctx.str("name");
    let (id, opts) = match name.find('{') {
        Some(i) => (name[..i].to_string(), Some(name[i..].to_string())),
        None => (name.clone(), None),
    };
    let pid = crate::registry::Registries::get().id("minecraft:particle_type", &id).ok_or_else(|| err("particle.notFound", vec![json!(id)]))?;
    let mut extra = Vec::new();
    match id.trim_start_matches("minecraft:") {
        "block" | "block_marker" | "falling_dust" | "dust_pillar" | "block_crumble" => {
            let st = opts.as_deref().and_then(|o| super::reader::read_snbt(&mut super::reader::Reader::new(o)).ok()).and_then(|t| t.get("block_state").and_then(|b| b.get("Name").or(Some(b))).and_then(|n| n.as_str().map(String::from)));
            let s = st.map(|n| BlockState::by_name(n.trim_start_matches("minecraft:"))).unwrap_or(BlockState::by_name("stone"));
            extra.vi(s.0 as i32);
        }
        "dust" => {
            extra.i32(0xFF0000).f32(1.0);
        }
        "item" => {
            crate::game::write_slot(&mut extra, Some(crate::data::Stack::new(crate::data::Items::get().id("stone").unwrap_or(1), 1)));
        }
        "entity_effect" => {
            extra.i32(-1);
        }
        "sculk_charge" => {
            extra.f32(0.0);
        }
        "shriek" | "vibration" | "trail" | "dust_color_transition" => return Err(err("particle.notFound", vec![json!(id)])),
        _ => {}
    }
    let pos = if ctx.has("pos") { ctx.vec3("pos") } else { ctx.src.pos };
    let delta = if ctx.has("delta") { ctx.coords("delta").resolve([0.0; 3], [0.0, 0.0]) } else { [0.0; 3] };
    let speed = ctx.f64("speed") as f32;
    let count = ctx.int("count");
    let force = words(ctx).iter().any(|w| w == "force");
    let mut b = Vec::new();
    b.bool(force).bool(false).f64(pos[0]).f64(pos[1]).f64(pos[2]).f32(delta[0] as f32).f32(delta[1] as f32).f32(delta[2] as f32).f32(speed).i32(count).vi(pid).raw(&extra);
    let pkt = ctx.st.game.encode("level_particles", &b);
    let viewers = if ctx.has("viewers") { ctx.players("viewers")? } else { ctx.level().players() };
    let range = if force { 512.0 } else { 32.0 };
    let mut n = 0;
    for p in viewers {
        let pp = p.pose.lock().pos;
        if crate::command::Target::Player(p.clone()).dim() == ctx.src.dim && (0..3).map(|i| (pp[i] - pos[i]).powi(2)).sum::<f64>() <= range * range {
            p.send(pkt.clone());
            n += 1;
        }
    }
    if n == 0 {
        return Err(err("commands.particle.failed", vec![]));
    }
    ctx.ok(err("commands.particle.success", vec![json!(id)]), true);
    Ok(n)
}

// ------------------------------------------------------------------ scoreboard

fn obj_display(name: &str, o: &Objective) -> Value {
    json!({ "text": "[", "extra": [o.display.clone(), { "text": "]" }], "hoverEvent": { "action": "show_text", "value": { "text": name } } })
}

fn objective_exists(name: &str) -> Result<Objective, Value> {
    state::read(|s| s.objectives.get(name).cloned()).ok_or_else(|| err("arguments.objective.notFound", vec![json!(name)]))
}

const CRITERIA: [&str; 15] = ["dummy", "trigger", "deathCount", "playerKillCount", "totalKillCount", "health", "xp", "level", "food", "air", "armor", "killedByTeam", "teamkill", "minecraft.custom", "minecraft.mined"];

fn objectives(ctx: &mut Ctx) -> CR {
    let w = words(ctx);
    let g = ctx.st.game.clone();
    match w.get(2).map(|s| s.as_str()) {
        Some("list") => {
            let all = state::read(|s| s.objectives.iter().map(|(n, o)| obj_display(n, o)).collect::<Vec<_>>());
            if all.is_empty() {
                ctx.ok(err("commands.scoreboard.objectives.list.empty", vec![]), false);
            } else {
                let mut extra = Vec::new();
                for (i, v) in all.iter().enumerate() {
                    if i > 0 {
                        extra.push(json!(", "));
                    }
                    extra.push(v.clone());
                }
                ctx.ok(err("commands.scoreboard.objectives.list.success", vec![json!(all.len()), json!({ "text": "", "extra": extra })]), false);
            }
            Ok(all.len() as i32)
        }
        Some("add") => {
            let name = ctx.str("objective");
            let crit = ctx.str("criteria");
            let base = crit.split(':').next().unwrap_or("");
            if !CRITERIA.contains(&base) && !crit.starts_with("minecraft.") && !crit.starts_with("teamkill.") && !crit.starts_with("killedByTeam.") {
                return Err(err("argument.criteria.invalid", vec![json!(crit)]));
            }
            if name.len() > 32767 {
                return Err(err("commands.scoreboard.objectives.add.longName", vec![]));
            }
            let display = if ctx.has("displayName") { ctx.text("displayName") } else { json!({ "text": name }) };
            let o = Objective { criteria: crit.clone(), display, hearts: crit == "health", auto_update: false };
            let dup = state::with(|s| {
                if s.objectives.contains_key(&name) {
                    return true;
                }
                s.objectives.insert(name.clone(), o.clone());
                false
            });
            if dup {
                return Err(err("commands.scoreboard.objectives.add.duplicate", vec![]));
            }
            ctx.st.broadcast_all(state::objective_packet(&g, &name, Some(&o)));
            ctx.ok(err("commands.scoreboard.objectives.add.success", vec![obj_display(&name, &o)]), true);
            Ok(state::read(|s| s.objectives.len()) as i32)
        }
        Some("remove") => {
            let name = ctx.str("objective");
            let o = objective_exists(&name)?;
            state::with(|s| {
                s.objectives.remove(&name);
                for m in s.scores.values_mut() {
                    m.remove(&name);
                }
                s.scores.retain(|_, m| !m.is_empty());
                s.display.retain(|_, v| *v != name);
            });
            ctx.st.broadcast_all(state::objective_packet(&g, &name, None));
            ctx.ok(err("commands.scoreboard.objectives.remove.success", vec![obj_display(&name, &o)]), true);
            Ok(state::read(|s| s.objectives.len()) as i32)
        }
        Some("setdisplay") => {
            let slot_name = ctx.str("slot");
            let slot = score_slot(&slot_name).unwrap_or(1);
            if ctx.has("objective") {
                let name = ctx.str("objective");
                let o = objective_exists(&name)?;
                let same = state::with(|s| s.display.insert(slot, name.clone()).as_deref() == Some(name.as_str()));
                if same {
                    return Err(err("commands.scoreboard.objectives.display.alreadySet", vec![]));
                }
                ctx.st.broadcast_all(state::display_packet(&g, slot, &name));
                ctx.ok(err("commands.scoreboard.objectives.display.set", vec![json!(slot_name), obj_display(&name, &o)]), true);
            } else {
                if state::with(|s| s.display.remove(&slot)).is_none() {
                    return Err(err("commands.scoreboard.objectives.display.alreadyEmpty", vec![]));
                }
                ctx.st.broadcast_all(state::display_packet(&g, slot, ""));
                ctx.ok(err("commands.scoreboard.objectives.display.cleared", vec![json!(slot_name)]), true);
            }
            Ok(0)
        }
        Some("modify") => {
            let name = ctx.str("objective");
            let mut o = objective_exists(&name)?;
            let what = w.get(4).map(|s| s.as_str()).unwrap_or("");
            match what {
                "displayname" => {
                    o.display = ctx.text("displayName");
                    ctx.ok(err("commands.scoreboard.objectives.modify.displayname", vec![json!(name), o.display.clone()]), true);
                }
                "rendertype" => {
                    o.hearts = w.get(5).map(|s| s.as_str()) == Some("hearts");
                    ctx.ok(err("commands.scoreboard.objectives.modify.rendertype", vec![obj_display(&name, &o)]), true);
                }
                _ => {
                    o.auto_update = ctx.bool("value");
                    ctx.ok(err(if o.auto_update { "commands.scoreboard.objectives.modify.displayAutoUpdate.enable" } else { "commands.scoreboard.objectives.modify.displayAutoUpdate.disable" }, vec![obj_display(&name, &o)]), true);
                }
            }
            state::with(|s| s.objectives.insert(name.clone(), o.clone()));
            ctx.st.broadcast_all(state::objective_update_packet(&g, &name, &o));
            Ok(0)
        }
        _ => Err(err("command.unknown.argument", vec![])),
    }
}

/// Writes scores and broadcasts them.
pub fn write_scores(st: &Arc<crate::ServerState>, holders: &[String], obj: &str, f: impl Fn(Option<i32>) -> i32) -> i32 {
    let mut last = 0;
    let mut pkts = Vec::new();
    state::with(|s| {
        for h in holders {
            let cur = state::get_score(s, h, obj);
            let v = f(cur);
            state::set_score(s, h, obj, v);
            s.triggers.remove(&(h.clone(), obj.to_string()));
            last = v;
            pkts.push(state::score_packet(&st.game, h, obj, v));
        }
    });
    for p in pkts {
        st.broadcast_all(p);
    }
    last
}

fn holder_json(h: &str) -> Value {
    json!({ "text": h })
}

fn players(ctx: &mut Ctx) -> CR {
    let w = words(ctx);
    let g = ctx.st.game.clone();
    let op = w.get(2).cloned().unwrap_or_default();
    match op.as_str() {
        "list" => {
            if ctx.has("target") {
                let h = ctx.holders("target")?.into_iter().next().unwrap_or_default();
                let scores = state::read(|s| s.scores.get(&h).cloned().unwrap_or_default());
                if scores.is_empty() {
                    ctx.ok(err("commands.scoreboard.players.list.entity.empty", vec![holder_json(&h)]), false);
                } else {
                    ctx.ok(err("commands.scoreboard.players.list.entity.success", vec![holder_json(&h), json!(scores.len())]), false);
                    for (o, v) in &scores {
                        let od = state::read(|s| s.objectives.get(o).map(|x| obj_display(o, x))).unwrap_or(json!(o));
                        ctx.ok(err("commands.scoreboard.players.list.entity.entry", vec![od, json!(v)]), false);
                    }
                }
                Ok(scores.len() as i32)
            } else {
                let all: Vec<String> = state::read(|s| s.scores.keys().cloned().collect());
                if all.is_empty() {
                    ctx.ok(err("commands.scoreboard.players.list.empty", vec![]), false);
                } else {
                    ctx.ok(err("commands.scoreboard.players.list.success", vec![json!(all.len()), json!(all.join(", "))]), false);
                }
                Ok(all.len() as i32)
            }
        }
        "set" | "add" | "remove" => {
            let holders = ctx.holders("targets")?;
            let obj = ctx.str("objective");
            let o = objective_exists(&obj)?;
            if is_readonly(&o.criteria) {
                return Err(err("arguments.objective.readonly", vec![json!(obj)]));
            }
            let n = ctx.int("score");
            let v = write_scores(&ctx.st, &holders, &obj, |cur| match op.as_str() {
                "set" => n,
                "add" => cur.unwrap_or(0).wrapping_add(n),
                _ => cur.unwrap_or(0).wrapping_sub(n),
            });
            let key = format!("commands.scoreboard.players.{op}.success");
            if holders.len() == 1 {
                ctx.ok(err(&format!("{key}.single"), vec![json!(n), obj_display(&obj, &o), holder_json(&holders[0]), json!(v)]), true);
            } else {
                ctx.ok(err(&format!("{key}.multiple"), vec![json!(n), obj_display(&obj, &o), json!(holders.len())]), true);
            }
            Ok(if holders.len() == 1 { v } else { holders.len() as i32 })
        }
        "get" => {
            let h = ctx.holders("target")?.into_iter().next().unwrap_or_default();
            let obj = ctx.str("objective");
            let o = objective_exists(&obj)?;
            let v = state::read(|s| state::get_score(s, &h, &obj)).ok_or_else(|| err("commands.scoreboard.players.get.null", vec![json!(obj), holder_json(&h)]))?;
            ctx.ok(err("commands.scoreboard.players.get.success", vec![holder_json(&h), json!(v), obj_display(&obj, &o)]), false);
            Ok(v)
        }
        "reset" => {
            let holders = ctx.holders("targets")?;
            let obj = if ctx.has("objective") { Some(ctx.str("objective")) } else { None };
            state::with(|s| {
                for h in &holders {
                    match &obj {
                        Some(o) => {
                            if let Some(m) = s.scores.get_mut(h) {
                                m.remove(o);
                            }
                        }
                        None => {
                            s.scores.remove(h);
                        }
                    }
                }
                s.scores.retain(|_, m| !m.is_empty());
            });
            for h in &holders {
                ctx.st.broadcast_all(state::reset_score_packet(&g, h, obj.as_deref()));
            }
            let key = if obj.is_some() { "commands.scoreboard.players.reset.specific" } else { "commands.scoreboard.players.reset.all" };
            let mut with = Vec::new();
            if let Some(o) = &obj {
                with.push(json!(o));
            }
            if holders.len() == 1 {
                with.push(holder_json(&holders[0]));
                ctx.ok(err(&format!("{key}.single"), with), true);
            } else {
                with.push(json!(holders.len()));
                ctx.ok(err(&format!("{key}.multiple"), with), true);
            }
            Ok(holders.len() as i32)
        }
        "enable" => {
            let holders = ctx.holders("targets")?;
            let obj = ctx.str("objective");
            let o = objective_exists(&obj)?;
            if o.criteria != "trigger" {
                return Err(err("commands.scoreboard.players.enable.invalid", vec![]));
            }
            let mut n = 0;
            state::with(|s| {
                for h in &holders {
                    if s.triggers.insert((h.clone(), obj.clone())) {
                        n += 1;
                    }
                }
            });
            if n == 0 {
                return Err(err("commands.scoreboard.players.enable.failed", vec![]));
            }
            if holders.len() == 1 {
                ctx.ok(err("commands.scoreboard.players.enable.success.single", vec![obj_display(&obj, &o), holder_json(&holders[0])]), true);
            } else {
                ctx.ok(err("commands.scoreboard.players.enable.success.multiple", vec![obj_display(&obj, &o), json!(holders.len())]), true);
            }
            Ok(n)
        }
        "operation" => {
            let targets = ctx.holders("targets")?;
            let tobj = ctx.str("targetObjective");
            objective_exists(&tobj)?;
            let sources = ctx.holders("source")?;
            let sobj = ctx.str("sourceObjective");
            objective_exists(&sobj)?;
            let op = ctx.str("operation");
            let mut result = 0;
            for t in &targets {
                for s in &sources {
                    let (a, b) = state::read(|x| (state::get_score(x, t, &tobj).unwrap_or(0), state::get_score(x, s, &sobj)));
                    let Some(b) = b else { return Err(err("commands.scoreboard.players.get.null", vec![json!(sobj), holder_json(s)])) };
                    let (na, nb) = match op.as_str() {
                        "=" => (b, b),
                        "+=" => (a.wrapping_add(b), b),
                        "-=" => (a.wrapping_sub(b), b),
                        "*=" => (a.wrapping_mul(b), b),
                        "/=" => (if b != 0 { a.div_euclid(b) } else { a }, b),
                        "%=" => (if b != 0 { a.rem_euclid(b) } else { a }, b),
                        "<" => (a.min(b), b),
                        ">" => (a.max(b), b),
                        _ => (b, a),
                    };
                    write_scores(&ctx.st, std::slice::from_ref(t), &tobj, |_| na);
                    if op == "><" {
                        write_scores(&ctx.st, std::slice::from_ref(s), &sobj, |_| nb);
                    }
                    result = na;
                }
            }
            if targets.len() == 1 {
                ctx.ok(err("commands.scoreboard.players.operation.success.single", vec![json!(tobj), holder_json(&targets[0]), json!(result)]), true);
            } else {
                ctx.ok(err("commands.scoreboard.players.operation.success.multiple", vec![json!(tobj), json!(targets.len())]), true);
            }
            Ok(result)
        }
        _ => Err(err("command.unknown.argument", vec![])),
    }
}

fn is_readonly(c: &str) -> bool {
    matches!(c, "health" | "xp" | "level" | "food" | "air" | "armor")
}

fn trigger(ctx: &mut Ctx) -> CR {
    let p = ctx.me()?;
    let obj = ctx.str("objective");
    let o = objective_exists(&obj)?;
    if o.criteria != "trigger" {
        return Err(err("commands.trigger.failed.invalid", vec![]));
    }
    if !state::read(|s| s.triggers.contains(&(p.name.clone(), obj.clone()))) {
        return Err(err("commands.trigger.failed.unprimed", vec![]));
    }
    let w = words(ctx);
    let set = w.get(2).map(|s| s.as_str()) == Some("set");
    let n = if ctx.has("value") { ctx.int("value") } else { 1 };
    write_scores(&ctx.st, std::slice::from_ref(&p.name), &obj, |cur| if set { n } else { cur.unwrap_or(0).wrapping_add(n) });
    let key = if set { "commands.trigger.set.success" } else if ctx.has("value") { "commands.trigger.add.success" } else { "commands.trigger.simple.success" };
    let mut with = vec![obj_display(&obj, &o)];
    if ctx.has("value") {
        with.push(json!(n));
    }
    ctx.ok(err(key, with), false);
    Ok(n)
}

// ------------------------------------------------------------------ teams

fn team_display(name: &str, t: &Team) -> Value {
    json!({ "text": "[", "extra": [t.display.clone(), { "text": "]" }], "color": if t.color == "reset" { "white".to_string() } else { t.color.clone() }, "hoverEvent": { "action": "show_text", "value": { "text": name } } })
}

fn get_team(name: &str) -> Result<Team, Value> {
    state::read(|s| s.teams.get(name).cloned()).ok_or_else(|| err("team.notFound", vec![json!(name)]))
}

fn team(ctx: &mut Ctx) -> CR {
    let w = words(ctx);
    let g = ctx.st.game.clone();
    let op = w.get(1).cloned().unwrap_or_default();
    match op.as_str() {
        "list" => {
            if ctx.has("team") {
                let n = ctx.str("team");
                let t = get_team(&n)?;
                if t.members.is_empty() {
                    ctx.ok(err("commands.team.list.members.empty", vec![team_display(&n, &t)]), false);
                } else {
                    let m: Vec<String> = t.members.iter().cloned().collect();
                    ctx.ok(err("commands.team.list.members.success", vec![team_display(&n, &t), json!(m.len()), json!(m.join(", "))]), false);
                }
                Ok(t.members.len() as i32)
            } else {
                let all = state::read(|s| s.teams.iter().map(|(n, t)| team_display(n, t)).collect::<Vec<_>>());
                if all.is_empty() {
                    ctx.ok(err("commands.team.list.teams.empty", vec![]), false);
                } else {
                    ctx.ok(err("commands.team.list.teams.success", vec![json!(all.len()), json!({ "text": "", "extra": all })]), false);
                }
                Ok(all.len() as i32)
            }
        }
        "add" => {
            let n = ctx.str("team");
            let disp = if ctx.has("displayName") { Some(ctx.text("displayName")) } else { None };
            let t = Team::new(&n, disp);
            if state::with(|s| s.teams.insert(n.clone(), t.clone()).is_some()) {
                return Err(err("commands.team.add.duplicate", vec![]));
            }
            ctx.st.broadcast_all(state::team_packet(&g, &n, &t, 0));
            ctx.ok(err("commands.team.add.success", vec![team_display(&n, &t)]), true);
            Ok(state::read(|s| s.teams.len()) as i32)
        }
        "remove" => {
            let n = ctx.str("team");
            let t = get_team(&n)?;
            state::with(|s| s.teams.remove(&n));
            ctx.st.broadcast_all(state::team_packet(&g, &n, &t, 1));
            ctx.ok(err("commands.team.remove.success", vec![team_display(&n, &t)]), true);
            Ok(state::read(|s| s.teams.len()) as i32)
        }
        "empty" => {
            let n = ctx.str("team");
            let t = get_team(&n)?;
            if t.members.is_empty() {
                return Err(err("commands.team.empty.unchanged", vec![]));
            }
            let m: Vec<String> = t.members.iter().cloned().collect();
            state::with(|s| s.teams.get_mut(&n).map(|x| x.members.clear()));
            ctx.st.broadcast_all(state::team_members_packet(&g, &n, &m, false));
            ctx.ok(err("commands.team.empty.success", vec![json!(m.len()), team_display(&n, &t)]), true);
            Ok(m.len() as i32)
        }
        "join" => {
            let n = ctx.str("team");
            let t = get_team(&n)?;
            let members = if ctx.has("members") { ctx.holders("members")? } else { vec![ctx.me_target().ok_or_else(|| err("permissions.requires.entity", vec![]))?.holder()] };
            let mut removed: Vec<(String, String)> = Vec::new();
            state::with(|s| {
                for m in &members {
                    for (tn, tt) in s.teams.iter_mut() {
                        if tn != &n && tt.members.remove(m) {
                            removed.push((tn.clone(), m.clone()));
                        }
                    }
                    if let Some(tt) = s.teams.get_mut(&n) {
                        tt.members.insert(m.clone());
                    }
                }
            });
            for (tn, m) in removed {
                ctx.st.broadcast_all(state::team_members_packet(&g, &tn, &[m], false));
            }
            ctx.st.broadcast_all(state::team_members_packet(&g, &n, &members, true));
            if members.len() == 1 {
                ctx.ok(err("commands.team.join.success.single", vec![holder_json(&members[0]), team_display(&n, &t)]), true);
            } else {
                ctx.ok(err("commands.team.join.success.multiple", vec![json!(members.len()), team_display(&n, &t)]), true);
            }
            Ok(members.len() as i32)
        }
        "leave" => {
            let members = ctx.holders("members")?;
            let mut removed: Vec<(String, String)> = Vec::new();
            state::with(|s| {
                for m in &members {
                    for (tn, tt) in s.teams.iter_mut() {
                        if tt.members.remove(m) {
                            removed.push((tn.clone(), m.clone()));
                        }
                    }
                }
            });
            for (tn, m) in &removed {
                ctx.st.broadcast_all(state::team_members_packet(&g, tn, std::slice::from_ref(m), false));
            }
            if members.len() == 1 {
                ctx.ok(err("commands.team.leave.success.single", vec![holder_json(&members[0])]), true);
            } else {
                ctx.ok(err("commands.team.leave.success.multiple", vec![json!(members.len())]), true);
            }
            Ok(removed.len() as i32)
        }
        "modify" => {
            let n = ctx.str("team");
            let mut t = get_team(&n)?;
            let what = w.get(3).cloned().unwrap_or_default();
            let val = w.get(4).cloned().unwrap_or_default();
            let msg = match what.as_str() {
                "displayName" => {
                    t.display = ctx.text("displayName");
                    err("commands.team.option.name.success", vec![team_display(&n, &t)])
                }
                "color" => {
                    t.color = ctx.str("value");
                    err("commands.team.option.color.success", vec![team_display(&n, &t), json!(t.color)])
                }
                "friendlyFire" => {
                    t.friendly_fire = ctx.bool("allowed");
                    err(if t.friendly_fire { "commands.team.option.friendlyfire.enabled" } else { "commands.team.option.friendlyfire.disabled" }, vec![team_display(&n, &t)])
                }
                "seeFriendlyInvisibles" => {
                    t.see_invisible = ctx.bool("allowed");
                    err(if t.see_invisible { "commands.team.option.seeFriendlyInvisibles.enabled" } else { "commands.team.option.seeFriendlyInvisibles.disabled" }, vec![team_display(&n, &t)])
                }
                "nametagVisibility" => {
                    t.nametag = val.clone();
                    err("commands.team.option.nametagVisibility.success", vec![team_display(&n, &t), json!({ "translate": format!("team.visibility.{val}") })])
                }
                "deathMessageVisibility" => {
                    t.death_messages = val.clone();
                    err("commands.team.option.deathMessageVisibility.success", vec![team_display(&n, &t), json!({ "translate": format!("team.visibility.{val}") })])
                }
                "collisionRule" => {
                    t.collision = val.clone();
                    err("commands.team.option.collisionRule.success", vec![team_display(&n, &t), json!({ "translate": format!("team.collision.{val}") })])
                }
                "prefix" => {
                    t.prefix = ctx.text("prefix");
                    err("commands.team.option.prefix.success", vec![t.prefix.clone()])
                }
                _ => {
                    t.suffix = ctx.text("suffix");
                    err("commands.team.option.suffix.success", vec![t.suffix.clone()])
                }
            };
            state::with(|s| s.teams.insert(n.clone(), t.clone()));
            ctx.st.broadcast_all(state::team_packet(&g, &n, &t, 2));
            ctx.ok(msg, true);
            Ok(0)
        }
        _ => Err(err("command.unknown.argument", vec![])),
    }
}

// ------------------------------------------------------------------ boss bars

fn bar_display(id: &str, b: &Bar) -> Value {
    json!({ "text": "[", "extra": [b.name.clone(), { "text": "]" }], "hoverEvent": { "action": "show_text", "value": { "text": id } } })
}

fn get_bar(id: &str) -> Result<Bar, Value> {
    state::read(|s| s.bars.get(id).cloned()).ok_or_else(|| err("commands.bossbar.unknown", vec![json!(id)]))
}

/// Re-sends a bar to the players who should see it.
pub fn sync_bar(st: &Arc<crate::ServerState>, old: Option<&Bar>, new: Option<&Bar>) {
    let g = &st.game;
    for p in st.all_players() {
        let saw = old.is_some_and(|b| b.visible && b.players.contains(&p.uuid));
        let sees = new.is_some_and(|b| b.visible && b.players.contains(&p.uuid));
        if saw {
            p.send(state::bar_remove_packet(g, old.unwrap()));
        }
        if sees {
            p.send(state::bar_add_packet(g, new.unwrap()));
        }
    }
}

fn bossbar(ctx: &mut Ctx) -> CR {
    let w = words(ctx);
    let op = w.get(1).cloned().unwrap_or_default();
    let id = ctx.str("id");
    match op.as_str() {
        "add" => {
            let b = Bar { uuid: uuid::Uuid::new_v4(), name: ctx.text("name"), color: "white".into(), style: "progress".into(), value: 0, max: 100, visible: true, players: Default::default() };
            if state::with(|s| {
                if s.bars.contains_key(&id) {
                    return true;
                }
                s.bars.insert(id.clone(), b.clone());
                false
            }) {
                return Err(err("commands.bossbar.create.failed", vec![json!(id)]));
            }
            ctx.ok(err("commands.bossbar.create.success", vec![bar_display(&id, &b)]), true);
            Ok(state::read(|s| s.bars.len()) as i32)
        }
        "remove" => {
            let b = get_bar(&id)?;
            state::with(|s| s.bars.remove(&id));
            sync_bar(&ctx.st, Some(&b), None);
            ctx.ok(err("commands.bossbar.remove.success", vec![bar_display(&id, &b)]), true);
            Ok(state::read(|s| s.bars.len()) as i32)
        }
        "list" => {
            let all = state::read(|s| s.bars.iter().map(|(i, b)| bar_display(i, b)).collect::<Vec<_>>());
            if all.is_empty() {
                ctx.ok(err("commands.bossbar.list.bars.none", vec![]), false);
            } else {
                ctx.ok(err("commands.bossbar.list.bars.some", vec![json!(all.len()), json!({ "text": "", "extra": all })]), false);
            }
            Ok(all.len() as i32)
        }
        "get" => {
            let b = get_bar(&id)?;
            match w.get(3).map(|s| s.as_str()) {
                Some("max") => {
                    ctx.ok(err("commands.bossbar.get.max", vec![bar_display(&id, &b), json!(b.max)]), false);
                    Ok(b.max)
                }
                Some("value") => {
                    ctx.ok(err("commands.bossbar.get.value", vec![bar_display(&id, &b), json!(b.value)]), false);
                    Ok(b.value)
                }
                Some("visible") => {
                    ctx.ok(err(if b.visible { "commands.bossbar.get.visible.visible" } else { "commands.bossbar.get.visible.hidden" }, vec![bar_display(&id, &b)]), false);
                    Ok(b.visible as i32)
                }
                _ => {
                    let online: Vec<String> = ctx.st.all_players().into_iter().filter(|p| b.players.contains(&p.uuid)).map(|p| p.name.clone()).collect();
                    if online.is_empty() {
                        ctx.ok(err("commands.bossbar.get.players.none", vec![bar_display(&id, &b)]), false);
                    } else {
                        ctx.ok(err("commands.bossbar.get.players.some", vec![bar_display(&id, &b), json!(online.len()), json!(online.join(", "))]), false);
                    }
                    Ok(online.len() as i32)
                }
            }
        }
        _ => {
            let old = get_bar(&id)?;
            let mut b = old.clone();
            let what = w.get(3).cloned().unwrap_or_default();
            let (msg, n) = match what.as_str() {
                "color" => {
                    b.color = w.get(4).cloned().unwrap_or_default();
                    if b.color == old.color {
                        return Err(err("commands.bossbar.set.color.unchanged", vec![]));
                    }
                    (err("commands.bossbar.set.color.success", vec![bar_display(&id, &b)]), 0)
                }
                "style" => {
                    b.style = w.get(4).cloned().unwrap_or_default();
                    if b.style == old.style {
                        return Err(err("commands.bossbar.set.style.unchanged", vec![]));
                    }
                    (err("commands.bossbar.set.style.success", vec![bar_display(&id, &b)]), 0)
                }
                "max" => {
                    b.max = ctx.int("max");
                    if b.max == old.max {
                        return Err(err("commands.bossbar.set.max.unchanged", vec![]));
                    }
                    (err("commands.bossbar.set.max.success", vec![bar_display(&id, &b), json!(b.max)]), b.max)
                }
                "value" => {
                    b.value = ctx.int("value");
                    if b.value == old.value {
                        return Err(err("commands.bossbar.set.value.unchanged", vec![]));
                    }
                    (err("commands.bossbar.set.value.success", vec![bar_display(&id, &b), json!(b.value)]), b.value)
                }
                "name" => {
                    b.name = ctx.text("name");
                    (err("commands.bossbar.set.name.success", vec![bar_display(&id, &b)]), 0)
                }
                "visible" => {
                    b.visible = ctx.bool("visible");
                    if b.visible == old.visible {
                        return Err(err(if b.visible { "commands.bossbar.set.visibility.unchanged.visible" } else { "commands.bossbar.set.visibility.unchanged.hidden" }, vec![]));
                    }
                    (err(if b.visible { "commands.bossbar.set.visible.success.visible" } else { "commands.bossbar.set.visible.success.hidden" }, vec![bar_display(&id, &b)]), 0)
                }
                _ => {
                    let ps = if ctx.has("targets") { ctx.players("targets")? } else { vec![] };
                    b.players = ps.iter().map(|p| p.uuid).collect();
                    if b.players == old.players {
                        return Err(err("commands.bossbar.set.players.unchanged", vec![]));
                    }
                    let ts: Vec<Target> = ps.into_iter().map(Target::Player).collect();
                    if ts.is_empty() {
                        (err("commands.bossbar.set.players.success.none", vec![bar_display(&id, &b)]), 0)
                    } else {
                        (err("commands.bossbar.set.players.success.some", vec![bar_display(&id, &b), json!(ts.len()), names_json(&ts)]), ts.len() as i32)
                    }
                }
            };
            state::with(|s| s.bars.insert(id.clone(), b.clone()));
            sync_bar(&ctx.st, Some(&old), Some(&b));
            ctx.ok(msg, true);
            Ok(n)
        }
    }
}

// ------------------------------------------------------------------ world border

fn worldborder(ctx: &mut Ctx) -> CR {
    let w = words(ctx);
    let g = ctx.st.game.clone();
    let border = state::read(|s| s.border.clone());
    let op = w.get(1).cloned().unwrap_or_default();
    let send = |ctx: &Ctx, name: &str, b: Vec<u8>| ctx.st.broadcast_all(g.encode(name, &b));
    match op.as_str() {
        "get" => {
            let s = border.size.round() as i32;
            ctx.ok(err("commands.worldborder.get", vec![json!(s)]), false);
            Ok(s)
        }
        "set" | "add" => {
            let d = ctx.f64("distance");
            let size = if op == "add" { border.size + d } else { d };
            if size < 1.0 {
                return Err(err("commands.worldborder.set.failed.small", vec![]));
            }
            if size > 59_999_968.0 {
                return Err(err("commands.worldborder.set.failed.big", vec![json!(59_999_968)]));
            }
            let ticks = ctx.int("time") as i64;
            if (size - border.size).abs() < 1e-9 && ticks == 0 {
                return Err(err("commands.worldborder.set.failed.nochange", vec![]));
            }
            state::with(|s| {
                if ticks > 0 {
                    s.border.target = size;
                    s.border.lerp_ticks = ticks;
                } else {
                    s.border.size = size;
                    s.border.target = size;
                    s.border.lerp_ticks = 0;
                }
            });
            let mut b = Vec::new();
            if ticks > 0 {
                b.f64(border.size).f64(size).vl(ticks * 50);
                send(ctx, "set_border_lerp_size", b);
                let key = if size < border.size { "commands.worldborder.set.shrink" } else { "commands.worldborder.set.grow" };
                ctx.ok(err(key, vec![json!(format!("{size:.1}")), json!(ticks / 20)]), true);
            } else {
                b.f64(size);
                send(ctx, "set_border_size", b);
                ctx.ok(err("commands.worldborder.set.immediate", vec![json!(format!("{size:.1}"))]), true);
            }
            Ok((size - border.size) as i32)
        }
        "center" => {
            let xz = ctx.coords("pos").xz(ctx.src.pos);
            if xz[0].abs() > 2.9999984e7 || xz[1].abs() > 2.9999984e7 {
                return Err(err("argument.pos.outofworld", vec![]));
            }
            state::with(|s| s.border.center = xz);
            let mut b = Vec::new();
            b.f64(xz[0]).f64(xz[1]);
            send(ctx, "set_border_center", b);
            ctx.ok(err("commands.worldborder.center.success", vec![json!(format!("{:.2}", xz[0])), json!(format!("{:.2}", xz[1]))]), true);
            Ok(0)
        }
        "damage" => {
            if w.get(2).map(|s| s.as_str()) == Some("amount") {
                let v = ctx.f64("damagePerBlock");
                state::with(|s| s.border.damage = v);
                ctx.ok(err("commands.worldborder.damage.amount.success", vec![json!(format!("{v:.2}"))]), true);
            } else {
                let v = ctx.f64("distance");
                state::with(|s| s.border.buffer = v);
                ctx.ok(err("commands.worldborder.damage.buffer.success", vec![json!(format!("{v:.2}"))]), true);
            }
            Ok(0)
        }
        "warning" => {
            let mut b = Vec::new();
            if w.get(2).map(|s| s.as_str()) == Some("distance") {
                let v = ctx.int("distance");
                state::with(|s| s.border.warn_distance = v);
                b.vi(v);
                send(ctx, "set_border_warning_distance", b);
                ctx.ok(err("commands.worldborder.warning.distance.success", vec![json!(v)]), true);
            } else {
                let v = ctx.int("time") / 20;
                state::with(|s| s.border.warn_time = v);
                b.vi(v);
                send(ctx, "set_border_warning_delay", b);
                ctx.ok(err("commands.worldborder.warning.time.success", vec![json!(v)]), true);
            }
            Ok(0)
        }
        _ => Err(err("command.unknown.argument", vec![])),
    }
}

/// Once per tick: border lerp and damage outside it (`WorldBorder.tick`,
/// `LivingEntity.baseTick`).
pub fn tick_border(st: &Arc<crate::ServerState>, tick: u64) {
    let b = state::read(|s| s.border.clone());
    if b.lerp_ticks > 0 {
        let mut g = None;
        state::with(|s| {
            s.border.tick();
            g = Some(s.border.clone());
        });
    }
    if b.size >= 59_999_968.0 || tick % 20 != 0 {
        return;
    }
    for lv in st.levels() {
        for p in lv.players() {
            if matches!(p.gamemode.load(Ordering::Relaxed), 1 | 3) {
                continue;
            }
            let pos = p.pose.lock().pos;
            let inside = b.distance_inside(pos[0], pos[2]) + b.buffer;
            if inside < 0.0 {
                let dmg = (-inside * b.damage).max(1.0) as f32;
                if b.damage > 0.0 {
                    lv.hurt_player(&p, &crate::game::combat::damage::DamageSource::new("outside_border"), dmg);
                }
            }
        }
    }
}

// ------------------------------------------------------------------ server

#[derive(Clone, Serialize, Deserialize)]
struct BanEntry {
    #[serde(default)]
    uuid: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    ip: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    source: String,
    #[serde(default = "forever")]
    expires: String,
    #[serde(default)]
    reason: String,
}

fn forever() -> String {
    "forever".into()
}

#[derive(Clone, Serialize, Deserialize)]
struct WlEntry {
    uuid: String,
    name: String,
}

#[derive(Default)]
struct Lists {
    dir: PathBuf,
    bans: Vec<BanEntry>,
    ip_bans: Vec<BanEntry>,
    whitelist: Vec<WlEntry>,
    whitelist_on: bool,
}

static LISTS: Mutex<Option<Lists>> = Mutex::new(None);

fn read_json<T: for<'a> Deserialize<'a>>(p: &std::path::Path) -> Vec<T> {
    std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Loads `banned-players.json`, `banned-ips.json`, `whitelist.json`.
pub fn load_lists(dir: &std::path::Path) {
    let l = Lists {
        dir: dir.to_path_buf(),
        bans: read_json(&dir.join("banned-players.json")),
        ip_bans: read_json(&dir.join("banned-ips.json")),
        whitelist: read_json(&dir.join("whitelist.json")),
        whitelist_on: std::fs::read_to_string(dir.join("rime-whitelist.txt")).is_ok_and(|t| t.trim() == "on"),
    };
    *LISTS.lock() = Some(l);
}

fn save_lists(l: &Lists) {
    let w = |n: &str, t: String| {
        if let Err(e) = std::fs::write(l.dir.join(n), t) {
            tracing::error!("saving {n}: {e}");
        }
    };
    w("banned-players.json", serde_json::to_string_pretty(&l.bans).unwrap_or_default());
    w("banned-ips.json", serde_json::to_string_pretty(&l.ip_bans).unwrap_or_default());
    w("whitelist.json", serde_json::to_string_pretty(&l.whitelist).unwrap_or_default());
    w("rime-whitelist.txt", if l.whitelist_on { "on".into() } else { "off".into() });
}

fn lists<R>(f: impl FnOnce(&mut Lists) -> R) -> R {
    let mut g = LISTS.lock();
    let l = g.get_or_insert_with(Lists::default);
    f(l)
}

/// Why a login is refused (ban / whitelist), if it is.
pub fn login_denied(name: &str, uuid: uuid::Uuid, ip: &str) -> Option<String> {
    lists(|l| {
        let u = uuid.hyphenated().to_string();
        if let Some(b) = l.bans.iter().find(|b| b.uuid == u || b.name.eq_ignore_ascii_case(name)) {
            return Some(format!("You are banned from this server.\nReason: {}", if b.reason.is_empty() { "Banned by an operator." } else { &b.reason }));
        }
        if let Some(b) = l.ip_bans.iter().find(|b| b.ip == ip) {
            return Some(format!("Your IP address is banned from this server.\nReason: {}", if b.reason.is_empty() { "Banned by an operator." } else { &b.reason }));
        }
        if l.whitelist_on && !l.whitelist.iter().any(|w| w.uuid == u || w.name.eq_ignore_ascii_case(name)) {
            return Some("You are not white-listed on this server!".into());
        }
        None
    })
}

/// Names of a `game_profile` argument (online players or a literal name).
fn profiles(ctx: &Ctx, n: &str) -> Result<Vec<(String, uuid::Uuid)>, Value> {
    let Some(sel) = ctx.sel(n) else { return Ok(vec![]) };
    if let super::selector::Kind::Name(name) = &sel.kind {
        if let Some(p) = ctx.st.find_player(name) {
            return Ok(vec![(p.name.clone(), p.uuid)]);
        }
        if !crate::net::java::valid_username(name) {
            return Err(err("argument.player.unknown", vec![]));
        }
        return Ok(vec![(name.clone(), crate::net::java::offline_uuid(name))]);
    }
    Ok(ctx.players(n)?.into_iter().map(|p| (p.name.clone(), p.uuid)).collect())
}

fn now_str() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("@{t}")
}

fn op(ctx: &mut Ctx) -> CR {
    let deop = words(ctx).first().map(|s| s.as_str()) == Some("deop");
    let list = profiles(ctx, "targets")?;
    let mut n = 0;
    for (name, _) in &list {
        let sender = crate::ops::CommandSender::Player { name: name.clone() };
        let was = ctx.st.ops.is_op(&sender);
        if was != deop {
            continue;
        }
        let r = if deop { ctx.st.ops.remove(name) } else { ctx.st.ops.add(name) };
        if let Err(e) = r {
            return Err(json!({ "text": e.to_string() }));
        }
        if let Some(p) = ctx.st.find_player(name) {
            p.requests.lock().push(Request::Commands);
        }
        n += 1;
        ctx.ok(err(if deop { "commands.deop.success" } else { "commands.op.success" }, vec![json!(name)]), true);
    }
    if n == 0 {
        return Err(err(if deop { "commands.deop.failed" } else { "commands.op.failed" }, vec![]));
    }
    Ok(n)
}

fn kick(ctx: &mut Ctx) -> CR {
    let players = ctx.players("targets")?;
    let reason = if ctx.has("reason") { super::render_plain(&ctx.message("reason")) } else { "Kicked by an operator".into() };
    for p in &players {
        p.requests.lock().push(Request::Kick(reason.clone()));
        ctx.ok(err("commands.kick.success", vec![json!({ "text": p.name }), json!(reason)]), true);
    }
    Ok(players.len() as i32)
}

fn ban(ctx: &mut Ctx) -> CR {
    let list = profiles(ctx, "targets")?;
    let reason = if ctx.has("reason") { super::render_plain(&ctx.message("reason")) } else { String::new() };
    let mut n = 0;
    for (name, uuid) in &list {
        let u = uuid.hyphenated().to_string();
        let added = lists(|l| {
            if l.bans.iter().any(|b| b.uuid == u) {
                return false;
            }
            l.bans.push(BanEntry { uuid: u.clone(), name: name.clone(), ip: String::new(), created: now_str(), source: ctx.src.name.clone(), expires: forever(), reason: reason.clone() });
            save_lists(l);
            true
        });
        if !added {
            continue;
        }
        n += 1;
        if let Some(p) = ctx.st.find_player(name) {
            p.requests.lock().push(Request::Kick(if reason.is_empty() { "You are banned from this server.".into() } else { format!("You are banned from this server.\nReason: {reason}") }));
        }
        ctx.ok(err("commands.ban.success", vec![json!(name), json!(if reason.is_empty() { "Banned by an operator.".to_string() } else { reason.clone() })]), true);
    }
    if n == 0 {
        return Err(err("commands.ban.failed", vec![]));
    }
    Ok(n)
}

fn ban_ip(ctx: &mut Ctx) -> CR {
    let target = ctx.str("target");
    let reason = if ctx.has("reason") { super::render_plain(&ctx.message("reason")) } else { String::new() };
    let ip = if target.parse::<std::net::IpAddr>().is_ok() {
        target.clone()
    } else {
        return Err(err("commands.banip.invalid", vec![]));
    };
    let added = lists(|l| {
        if l.ip_bans.iter().any(|b| b.ip == ip) {
            return false;
        }
        l.ip_bans.push(BanEntry { uuid: String::new(), name: String::new(), ip: ip.clone(), created: now_str(), source: ctx.src.name.clone(), expires: forever(), reason: reason.clone() });
        save_lists(l);
        true
    });
    if !added {
        return Err(err("commands.banip.failed", vec![]));
    }
    ctx.ok(err("commands.banip.success", vec![json!(ip), json!(reason)]), true);
    Ok(1)
}

fn pardon(ctx: &mut Ctx) -> CR {
    let ip = words(ctx).first().map(|s| s.as_str()) == Some("pardon-ip");
    let t = if ip { ctx.str("target") } else { ctx.str("targets") };
    let removed = lists(|l| {
        let before = l.bans.len() + l.ip_bans.len();
        if ip {
            l.ip_bans.retain(|b| b.ip != t);
        } else {
            l.bans.retain(|b| !b.name.eq_ignore_ascii_case(&t));
        }
        let r = before != l.bans.len() + l.ip_bans.len();
        if r {
            save_lists(l);
        }
        r
    });
    if !removed {
        return Err(err(if ip { "commands.pardonip.failed" } else { "commands.pardon.failed" }, vec![]));
    }
    ctx.ok(err(if ip { "commands.pardonip.success" } else { "commands.pardon.success" }, vec![json!(t)]), true);
    Ok(1)
}

fn banlist(ctx: &mut Ctx) -> CR {
    let w = words(ctx);
    let which = w.get(1).map(|s| s.as_str());
    let entries: Vec<BanEntry> = lists(|l| match which {
        Some("players") => l.bans.clone(),
        Some("ips") => l.ip_bans.clone(),
        _ => l.bans.iter().chain(&l.ip_bans).cloned().collect(),
    });
    if entries.is_empty() {
        ctx.ok(err("commands.banlist.none", vec![]), false);
    } else {
        ctx.ok(err("commands.banlist.list", vec![json!(entries.len())]), false);
        for e in &entries {
            let who = if e.name.is_empty() { e.ip.clone() } else { e.name.clone() };
            ctx.ok(err("commands.banlist.entry", vec![json!(who), json!(e.source), json!(e.reason)]), false);
        }
    }
    Ok(entries.len() as i32)
}

fn whitelist(ctx: &mut Ctx) -> CR {
    let w = words(ctx);
    match w.get(1).map(|s| s.as_str()) {
        Some("on") | Some("off") => {
            let on = w[1] == "on";
            let changed = lists(|l| {
                let c = l.whitelist_on != on;
                l.whitelist_on = on;
                save_lists(l);
                c
            });
            if !changed {
                return Err(err(if on { "commands.whitelist.alreadyOn" } else { "commands.whitelist.alreadyOff" }, vec![]));
            }
            ctx.ok(err(if on { "commands.whitelist.enabled" } else { "commands.whitelist.disabled" }, vec![]), true);
            Ok(1)
        }
        Some("list") => {
            let names: Vec<String> = lists(|l| l.whitelist.iter().map(|e| e.name.clone()).collect());
            if names.is_empty() {
                ctx.ok(err("commands.whitelist.none", vec![]), false);
            } else {
                ctx.ok(err("commands.whitelist.list", vec![json!(names.len()), json!(names.join(", "))]), false);
            }
            Ok(names.len() as i32)
        }
        Some("reload") => {
            let dir = lists(|l| l.dir.clone());
            load_lists(&dir);
            ctx.ok(err("commands.whitelist.reloaded", vec![]), true);
            Ok(1)
        }
        Some(op) => {
            let add = op == "add";
            let list = profiles(ctx, "targets")?;
            let mut n = 0;
            for (name, uuid) in &list {
                let u = uuid.hyphenated().to_string();
                let changed = lists(|l| {
                    let has = l.whitelist.iter().any(|e| e.uuid == u || e.name.eq_ignore_ascii_case(name));
                    if add && !has {
                        l.whitelist.push(WlEntry { uuid: u.clone(), name: name.clone() });
                    } else if !add && has {
                        l.whitelist.retain(|e| e.uuid != u && !e.name.eq_ignore_ascii_case(name));
                    } else {
                        return false;
                    }
                    save_lists(l);
                    true
                });
                if changed {
                    n += 1;
                    ctx.ok(err(if add { "commands.whitelist.add.success" } else { "commands.whitelist.remove.success" }, vec![json!(name)]), true);
                }
            }
            if n == 0 {
                return Err(err(if add { "commands.whitelist.add.failed" } else { "commands.whitelist.remove.failed" }, vec![]));
            }
            Ok(n)
        }
        None => Err(err("command.unknown.argument", vec![])),
    }
}

fn stop(ctx: &mut Ctx) -> CR {
    ctx.ok(err("commands.stop.stopping", vec![]), true);
    ctx.st.request_stop();
    Ok(1)
}

fn save_all(ctx: &mut Ctx) -> CR {
    let w = words(ctx);
    match w.first().map(|s| s.as_str()) {
        Some("save-on") => {
            ctx.ok(err("commands.save.enabled", vec![]), true);
            Ok(1)
        }
        Some("save-off") => {
            ctx.ok(err("commands.save.disabled", vec![]), true);
            Ok(1)
        }
        _ => {
            ctx.ok(err("commands.save.saving", vec![]), false);
            let st = ctx.st.clone();
            let src = ctx.src.clone();
            if let Ok(h) = tokio::runtime::Handle::try_current() {
                h.spawn(async move {
                    let r = st.sync().await;
                    let msg = if r.starts_with("OK") { err("commands.save.success", vec![]) } else { err("commands.save.failed", vec![]) };
                    let c = Ctx { st: st.clone(), src, args: Default::default(), line: String::new() };
                    c.ok(msg, true);
                });
            }
            Ok(1)
        }
    }
}

fn reload(ctx: &mut Ctx) -> CR {
    ctx.ok(err("commands.reload.success", vec![]), true);
    Ok(0)
}

/// Read-only criteria kept up to date once a second.
pub fn update_player_criteria(st: &Arc<crate::ServerState>) {
    let has = state::read(|s| s.objectives.values().any(|o| is_readonly(&o.criteria)));
    if !has {
        return;
    }
    for lv in st.levels() {
        for p in lv.players() {
            let (health, food, air, level, xp, armor) = {
                let v = p.vit.lock();
                (v.living.health.ceil() as i32, v.food, v.air, v.xp_level, v.xp_total, v.living.attrs.peek(crate::game::combat::attr::Attr::Armor) as i32)
            };
            for (c, val) in [("health", health), ("food", food), ("air", air), ("level", level), ("xp", xp), ("armor", armor)] {
                for pkt in state::update_criterion(&st.game, &p.name, c, val) {
                    st.broadcast_all(pkt);
                }
            }
        }
    }
}

/// Scoreboard criteria for a death (`deathCount`, kill counts).
pub fn on_death(st: &Arc<crate::ServerState>, victim: &str, killer: Option<&str>, killer_is_player: bool, victim_is_player: bool) {
    let g = &st.game;
    let mut pkts = Vec::new();
    if victim_is_player {
        pkts.extend(state::award(g, victim, "deathCount", 1));
    }
    if let Some(k) = killer {
        pkts.extend(state::award(g, k, "totalKillCount", 1));
        if victim_is_player && killer_is_player {
            pkts.extend(state::award(g, k, "playerKillCount", 1));
        }
    }
    for p in pkts {
        st.broadcast_all(p);
    }
}

