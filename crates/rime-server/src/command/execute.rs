//! `/execute`: modifiers, conditions and result stores.

use super::args::{in_range, Arg, ArgType as A};
use super::tree::{Tree, B};
use super::{err, resolve, state, Ctx, Source, StoreTo, Target, CR};
use crate::world::storage::Dim;
use serde_json::{json, Value};
use std::sync::Arc;

const ENT: A = A::Entity { single: false, players: false };

pub fn register(t: &mut Tree) {
    let exec = t.cmd("execute", 2, |_| {});
    let root = 0;
    let mut b = B::at(t, exec, 2);
    b.lit("run", |b| b.redirect(root, None));
    b.lit("as", |b| {
        b.arg("targets", ENT, |b| b.redirect(exec, Some(fork_as)));
    });
    b.lit("at", |b| {
        b.arg("targets", ENT, |b| b.redirect(exec, Some(fork_at)));
    });
    b.lit("positioned", |b| {
        b.arg("pos", A::Vec3, |b| b.redirect(exec, Some(fork_positioned)));
        b.lit("as", |b| {
            b.arg("targets", ENT, |b| b.redirect(exec, Some(fork_positioned)));
        });
        b.lit("over", |b| {
            b.arg("heightmap", A::Heightmap, |b| b.redirect(exec, Some(fork_positioned)));
        });
    });
    b.lit("rotated", |b| {
        b.arg("rot", A::Rotation, |b| b.redirect(exec, Some(fork_rotated)));
        b.lit("as", |b| {
            b.arg("targets", ENT, |b| b.redirect(exec, Some(fork_rotated)));
        });
    });
    b.lit("facing", |b| {
        b.arg("pos", A::Vec3, |b| b.redirect(exec, Some(fork_facing)));
        b.lit("entity", |b| {
            b.arg("targets", ENT, |b| {
                b.arg("anchor", A::Anchor, |b| b.redirect(exec, Some(fork_facing)));
            });
        });
    });
    b.lit("align", |b| {
        b.arg("axes", A::Swizzle, |b| b.redirect(exec, Some(fork_align)));
    });
    b.lit("anchored", |b| {
        b.arg("anchor", A::Anchor, |b| b.redirect(exec, Some(fork_anchored)));
    });
    b.lit("in", |b| {
        b.arg("dimension", A::Dimension, |b| b.redirect(exec, Some(fork_in)));
    });
    b.lit("on", |b| {
        for r in ["vehicle", "passengers", "owner", "target", "leasher", "controller", "attacker", "origin"] {
            b.lit(r, |b| b.redirect(exec, Some(fork_on)));
        }
    });
    b.lit("summon", |b| {
        b.arg("entity", A::Resource("minecraft:entity_type"), |b| b.redirect(exec, Some(fork_summon)));
    });
    for kind in ["if", "unless"] {
        b.lit(kind, |b| {
            let cond = |b: &mut B| {
                b.exec(cond_exec);
                b.redirect(exec, Some(cond_fork));
            };
            b.lit("block", |b| {
                b.arg("pos", A::BlockPos, |b| {
                    b.arg("block", A::BlockPredicate, cond);
                });
            });
            b.lit("blocks", |b| {
                b.arg("start", A::BlockPos, |b| {
                    b.arg("end", A::BlockPos, |b| {
                        b.arg("destination", A::BlockPos, |b| {
                            b.lit("all", cond);
                            b.lit("masked", cond);
                        });
                    });
                });
            });
            b.lit("entity", |b| {
                b.arg("entities", ENT, cond);
            });
            b.lit("score", |b| {
                b.arg("target", A::ScoreHolder { multiple: false }, |b| {
                    b.arg("targetObjective", A::Objective, |b| {
                        for op in ["<", "<=", "=", ">", ">="] {
                            b.lit(op, |b| {
                                b.arg("source", A::ScoreHolder { multiple: false }, |b| {
                                    b.arg("sourceObjective", A::Objective, cond);
                                });
                            });
                        }
                        b.lit("matches", |b| {
                            b.arg("range", A::IntRange, cond);
                        });
                    });
                });
            });
            b.lit("biome", |b| {
                b.arg("pos", A::BlockPos, |b| {
                    b.arg("biome", A::Id, cond);
                });
            });
            b.lit("dimension", |b| {
                b.arg("dimension", A::Dimension, cond);
            });
            b.lit("loaded", |b| {
                b.arg("pos", A::BlockPos, cond);
            });
        });
    }
    b.lit("store", |b| {
        for kind in ["result", "success"] {
            b.lit(kind, |b| {
                b.lit("score", |b| {
                    b.arg("targets", A::ScoreHolder { multiple: true }, |b| {
                        b.arg("objective", A::Objective, |b| b.redirect(exec, Some(fork_store)));
                    });
                });
                b.lit("bossbar", |b| {
                    b.arg("id", A::Id, |b| {
                        b.lit("value", |b| b.redirect(exec, Some(fork_store)));
                        b.lit("max", |b| b.redirect(exec, Some(fork_store)));
                    });
                });
            });
        }
    });
}

fn with_target(mut s: Source, t: &Target) -> Source {
    s.entity = Some(t.eid());
    s.player = t.player().cloned();
    s.name = t.plain_name();
    s.display = t.name_json();
    s
}

fn fork_as(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let sel = ctx.sel("targets").unwrap_or_else(|| unreachable!());
    Ok(resolve(&ctx.st, &ctx.src, &sel).iter().map(|t| with_target(ctx.src.clone(), t)).collect())
}

fn fork_at(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let sel = ctx.sel("targets").unwrap_or_else(|| unreachable!());
    Ok(resolve(&ctx.st, &ctx.src, &sel)
        .iter()
        .map(|t| {
            let mut s = ctx.src.clone();
            s.pos = t.pos();
            s.rot = t.rot();
            s.dim = t.dim();
            s
        })
        .collect())
}

fn fork_positioned(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let mut s = ctx.src.clone();
    s.eyes = false;
    if ctx.has("pos") {
        s.pos = ctx.vec3("pos");
        return Ok(vec![s]);
    }
    if ctx.has("heightmap") {
        let g = ctx.level();
        let (x, z) = (s.pos[0].floor() as i32, s.pos[2].floor() as i32);
        if let Some(y) = g.world.surface_y(x, z) {
            s.pos[1] = (y + 1) as f64;
        }
        return Ok(vec![s]);
    }
    let sel = ctx.sel("targets").unwrap_or_else(|| unreachable!());
    Ok(resolve(&ctx.st, &ctx.src, &sel)
        .iter()
        .map(|t| {
            let mut s = ctx.src.clone();
            s.pos = t.pos();
            s.eyes = false;
            s
        })
        .collect())
}

fn fork_rotated(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    if ctx.has("rot") {
        let mut s = ctx.src.clone();
        s.rot = ctx.coords("rot").rotation(s.rot);
        return Ok(vec![s]);
    }
    let sel = ctx.sel("targets").unwrap_or_else(|| unreachable!());
    Ok(resolve(&ctx.st, &ctx.src, &sel)
        .iter()
        .map(|t| {
            let mut s = ctx.src.clone();
            s.rot = t.rot();
            s
        })
        .collect())
}

fn look(from: [f64; 3], to: [f64; 3]) -> [f32; 2] {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let h = (d[0] * d[0] + d[2] * d[2]).sqrt();
    [crate::game::ai::control::wrap((d[2].atan2(d[0]).to_degrees() - 90.0) as f32), crate::game::ai::control::wrap((-(d[1].atan2(h).to_degrees())) as f32)]
}

fn fork_facing(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let from = ctx.src.anchor_pos(&ctx.st);
    if ctx.has("pos") {
        let mut s = ctx.src.clone();
        s.rot = look(from, ctx.vec3("pos"));
        return Ok(vec![s]);
    }
    let eyes = matches!(ctx.args.get("anchor"), Some(Arg::Anchor(true)));
    let sel = ctx.sel("targets").unwrap_or_else(|| unreachable!());
    Ok(resolve(&ctx.st, &ctx.src, &sel)
        .iter()
        .map(|t| {
            let mut s = ctx.src.clone();
            let p = t.pos();
            s.rot = look(from, [p[0], p[1] + if eyes { t.eye() } else { 0.0 }, p[2]]);
            s
        })
        .collect())
}

fn fork_align(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let mut s = ctx.src.clone();
    if let Some(Arg::Swizzle(ax)) = ctx.args.get("axes") {
        for i in 0..3 {
            if ax[i] {
                s.pos[i] = s.pos[i].floor();
            }
        }
    }
    Ok(vec![s])
}

fn fork_anchored(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let mut s = ctx.src.clone();
    s.eyes = matches!(ctx.args.get("anchor"), Some(Arg::Anchor(true)));
    Ok(vec![s])
}

fn fork_in(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let mut s = ctx.src.clone();
    s.dim = Dim::from_key(&ctx.str("dimension")).unwrap_or(Dim::Overworld);
    Ok(vec![s])
}

fn fork_on(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let Some(me) = ctx.me_target() else { return Ok(vec![]) };
    let g = ctx.st.level(me.dim()).clone();
    let rel = ["vehicle", "passengers", "owner", "target", "leasher", "controller", "attacker", "origin"].into_iter().find(|r| ctx.lit(r)).unwrap_or("vehicle");
    let eids: Vec<i32> = match rel {
        "vehicle" => g.vehicle_of(me.eid()).into_iter().collect(),
        "passengers" => g.passengers_of(me.eid()),
        "controller" => g.passengers_of(me.eid()).into_iter().take(1).collect(),
        "leasher" => g.leash_holder_of(me.eid()).into_iter().collect(),
        "target" => g.mob_target(me.eid()).into_iter().collect(),
        "owner" => g.mob_owner(me.eid()).and_then(|u| ctx.st.all_players().into_iter().find(|p| p.uuid == u).map(|p| p.eid)).into_iter().collect(),
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for e in eids {
        let t = match g.players().into_iter().find(|p| p.eid == e) {
            Some(p) => Some(Target::Player(p)),
            None => g.entity_snap(e).map(|s| Target::Ent(me.dim(), s)),
        };
        if let Some(t) = t {
            out.push(with_target(ctx.src.clone(), &t));
        }
    }
    Ok(out)
}

fn fork_summon(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let name = ctx.str("entity");
    let g = ctx.level().clone();
    let eid = g.summon(name.trim_start_matches("minecraft:"), ctx.src.pos).ok_or_else(|| err("commands.summon.failed", vec![]))?;
    let snap = g.entity_snap(eid).ok_or_else(|| err("commands.summon.failed", vec![]))?;
    Ok(vec![with_target(ctx.src.clone(), &Target::Ent(ctx.src.dim, snap))])
}

fn fork_store(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let result = ctx.lit("result");
    let to = if ctx.has("objective") {
        StoreTo::Score(ctx.holders("targets")?, ctx.str("objective"))
    } else {
        StoreTo::Bar(ctx.str("id"), ctx.lit("max"))
    };
    let mut s = ctx.src.clone();
    s.stores.push((to, result));
    Ok(vec![s])
}

/// Writes a command's result into the stores collected by `execute store`.
pub fn apply_stores(st: &Arc<crate::ServerState>, stores: &[(StoreTo, bool)], ok: bool, n: i32) {
    for (to, result) in stores {
        let v = if *result { n } else { ok as i32 };
        match to {
            StoreTo::Score(holders, obj) => {
                if state::read(|s| s.objectives.contains_key(obj)) {
                    super::cmds2::write_scores(st, holders, obj, |_| v);
                }
            }
            StoreTo::Bar(id, max) => {
                let old = state::read(|s| s.bars.get(id).cloned());
                if let Some(old) = old {
                    let mut b = old.clone();
                    if *max {
                        b.max = v.max(1);
                    } else {
                        b.value = v.max(0);
                    }
                    state::with(|s| s.bars.insert(id.clone(), b.clone()));
                    super::cmds2::sync_bar(st, Some(&old), Some(&b));
                }
            }
        }
    }
}

/// The condition of an `if` / `unless` node: how many things matched.
fn test(ctx: &Ctx) -> Result<i32, Value> {
    let g = ctx.level().clone();
    if ctx.has("entities") {
        let sel = ctx.sel("entities").unwrap_or_else(|| unreachable!());
        return Ok(resolve(&ctx.st, &ctx.src, &sel).len() as i32);
    }
    if ctx.has("block") {
        let p = ctx.block_pos("pos");
        let b = g.world.get_block(p[0], p[1], p[2]).ok_or_else(|| err("argument.pos.unloaded", vec![]))?;
        return Ok(ctx.block("block").test(b, &g.data) as i32);
    }
    if ctx.has("start") {
        let a = ctx.block_pos("start");
        let c = ctx.block_pos("end");
        let d = ctx.block_pos("destination");
        let masked = ctx.lit("masked");
        let lo = [a[0].min(c[0]), a[1].min(c[1]), a[2].min(c[2])];
        let hi = [a[0].max(c[0]), a[1].max(c[1]), a[2].max(c[2])];
        let vol = (hi[0] - lo[0] + 1) as i64 * (hi[1] - lo[1] + 1) as i64 * (hi[2] - lo[2] + 1) as i64;
        if vol > 32768 {
            return Err(err("commands.execute.blocks.toobig", vec![json!(32768), json!(vol)]));
        }
        let mut n = 0;
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    let s = g.world.get_block(x, y, z).ok_or_else(|| err("argument.pos.unloaded", vec![]))?;
                    if masked && s.is_air() {
                        continue;
                    }
                    let q = [d[0] + x - lo[0], d[1] + y - lo[1], d[2] + z - lo[2]];
                    let t = g.world.get_block(q[0], q[1], q[2]).ok_or_else(|| err("argument.pos.unloaded", vec![]))?;
                    if s != t || g.world.block_entity([x, y, z]).map(strip_pos) != g.world.block_entity(q).map(strip_pos) {
                        return Ok(0);
                    }
                    n += 1;
                }
            }
        }
        return Ok(n);
    }
    if ctx.has("targetObjective") {
        let h = ctx.holders("target")?.into_iter().next().unwrap_or_default();
        let obj = ctx.str("targetObjective");
        let Some(a) = state::read(|s| state::get_score(s, &h, &obj)) else { return Ok(0) };
        if ctx.lit("matches") {
            return Ok(in_range(ctx.range("range"), a as f64) as i32);
        }
        let sh = ctx.holders("source")?.into_iter().next().unwrap_or_default();
        let sobj = ctx.str("sourceObjective");
        let Some(b) = state::read(|s| state::get_score(s, &sh, &sobj)) else { return Ok(0) };
        let r = if ctx.lit("<=") {
            a <= b
        } else if ctx.lit(">=") {
            a >= b
        } else if ctx.lit("<") {
            a < b
        } else if ctx.lit(">") {
            a > b
        } else {
            a == b
        };
        return Ok(r as i32);
    }
    if ctx.has("biome") {
        let p = ctx.block_pos("pos");
        let want = ctx.str("biome");
        let b = g.world.biome(p[0], p[1], p[2]).ok_or_else(|| err("argument.pos.unloaded", vec![]))?;
        return Ok((b.trim_start_matches("minecraft:") == want.trim_start_matches("minecraft:")) as i32);
    }
    if ctx.has("dimension") {
        return Ok((Dim::from_key(&ctx.str("dimension")) == Some(ctx.src.dim)) as i32);
    }
    let p = ctx.block_pos("pos");
    Ok(g.world.get_block(p[0], p[1], p[2]).is_some() as i32)
}

fn strip_pos(mut t: crate::nbt::Tag) -> crate::nbt::Tag {
    if let crate::nbt::Tag::Compound(f) = &mut t {
        f.retain(|(k, _)| !matches!(k.as_str(), "x" | "y" | "z"));
    }
    t
}

fn passes(ctx: &Ctx) -> Result<(bool, i32), Value> {
    let n = test(ctx)?;
    let unless = ctx.lit("unless");
    Ok((if unless { n == 0 } else { n > 0 }, n))
}

fn cond_exec(ctx: &mut Ctx) -> CR {
    let (ok, n) = passes(ctx)?;
    let counted = ctx.has("entities") || ctx.has("start");
    if !ok {
        return Err(if counted && !ctx.lit("unless") { err("commands.execute.conditional.fail_count", vec![json!(n)]) } else { err("commands.execute.conditional.fail", vec![]) });
    }
    if counted && !ctx.lit("unless") {
        ctx.ok(err("commands.execute.conditional.pass_count", vec![json!(n)]), false);
        Ok(n)
    } else {
        ctx.ok(err("commands.execute.conditional.pass", vec![]), false);
        Ok(1)
    }
}

fn cond_fork(ctx: &mut Ctx) -> Result<Vec<Source>, Value> {
    let (ok, _) = passes(ctx)?;
    Ok(if ok { vec![ctx.src.clone()] } else { vec![] })
}
