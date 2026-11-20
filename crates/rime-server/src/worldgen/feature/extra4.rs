//! Sculk patches and fossils (ported from the 26.1 bytecode).

use anyhow::Result;
use serde_json::Value;

use super::extra::{add, dir_index, face_name, is, is_water_block, neg, prop, st, with, P, ALL, DOWN, EAST, NORTH, SOUTH, UP, WEST};
use super::extra2::{all_shuffled, mf_has_face, mf_state, mf_valid, same_axis};
use super::place::IntP;
use super::tree::shuffle;
use super::{Builder, Region};
use crate::registry::{vstate, BlockState};
use crate::worldgen::random::{Rng, WgRandom};

fn i32v(v: &Value, k: &str, d: i32) -> i32 {
    v[k].as_i64().map(|x| x as i32).unwrap_or(d)
}

pub struct SculkPatch {
    charge_count: i32,
    amount_per_charge: i32,
    spread_attempts: i32,
    growth_rounds: i32,
    spread_rounds: i32,
    extra_rare_growths: IntP,
    catalyst_chance: f32,
}

pub enum Extra4 {
    Sculk(Box<SculkPatch>),
    Fossil(Box<super::fossil::Fossil>),
}

impl Extra4 {
    pub fn parse(b: &mut Builder, ty: &str, c: &Value) -> Option<Result<Extra4>> {
        let r = (|| -> Result<Option<Extra4>> {
            Ok(Some(match ty {
                "sculk_patch" => Extra4::Sculk(Box::new(SculkPatch {
                    charge_count: i32v(c, "charge_count", 0),
                    amount_per_charge: i32v(c, "amount_per_charge", 0),
                    spread_attempts: i32v(c, "spread_attempts", 0),
                    growth_rounds: i32v(c, "growth_rounds", 0),
                    spread_rounds: i32v(c, "spread_rounds", 0),
                    extra_rare_growths: IntP::parse(&c["extra_rare_growths"])?,
                    catalyst_chance: c["catalyst_chance"].as_f64().unwrap_or(0.0) as f32,
                })),
                "fossil" => Extra4::Fossil(Box::new(super::fossil::Fossil::parse(b, c)?)),
                _ => return Ok(None),
            }))
        })();
        match r {
            Ok(Some(e)) => Some(Ok(e)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }

    pub fn place(&self, lv: &mut Region, r: &mut WgRandom, o: P) -> bool {
        match self {
            Extra4::Sculk(c) => sculk_patch(lv, r, o, c),
            Extra4::Fossil(f) => f.place(lv, r, o),
        }
    }
}

// ------------------------------------------------------------ sculk

/// `SculkSpreader.createWorldGenSpreader`
const GROWTH_SPAWN_COST: i32 = 50;
const NO_GROWTH_RADIUS: i32 = 1;
const CHARGE_DECAY_RATE: i32 = 5;
const ADDITIONAL_DECAY_RATE: i32 = 10;

#[derive(Clone, Copy, PartialEq)]
enum Beh {
    Default,
    Sculk,
    Vein,
}

fn beh(s: BlockState) -> Beh {
    match s.name() {
        "sculk" => Beh::Sculk,
        "sculk_vein" => Beh::Vein,
        _ => Beh::Default,
    }
}

fn vein() -> BlockState {
    st("sculk_vein")
}

fn is_vein(s: BlockState) -> bool {
    is(s, "sculk_vein")
}

fn manhattan(a: P, b: P) -> i32 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()
}

fn dist_sqr(a: P, b: P) -> f64 {
    let d = |i: usize| (a[i] - b[i]) as f64;
    d(0) * d(0) + d(1) * d(1) + d(2) * d(2)
}

/// `Vec3i.closerThan`
fn closer_than(a: P, b: P, d: f64) -> bool {
    dist_sqr(a, b) < d * d
}

fn sturdy_toward(lv: &Region, p: P, d: P) -> bool {
    vstate::sturdy(lv.get(add(p, d)), dir_index(neg(d)))
}

/// `SculkVeinSpreaderConfig.stateCanBeReplaced`
fn vein_can_replace(lv: &Region, src: P, q: P, f: P, t: BlockState) -> bool {
    let o = lv.get(add(q, f));
    if is(o, "sculk") || is(o, "sculk_catalyst") || is(o, "moving_piston") {
        return false;
    }
    if manhattan(src, q) == 2 {
        let b = add(src, neg(f));
        if vstate::sturdy(lv.get(b), dir_index(f)) {
            return false;
        }
    }
    let fl = vstate::fluid(t);
    if fl != 0 && fl != 1 {
        return false;
    }
    if lv.lib.extra_tags.fire.contains(t) {
        return false;
    }
    vstate::replaceable(t) || vstate::air(t) || is_vein(t) || (is_water_block(t) && vstate::fluid_source(t))
}

/// `MultifaceSpreader.spreadAll` with a `SculkVeinSpreaderConfig`;
/// `same_space` is the `SAME_POSITION`-only spreader.
fn vein_spread_all(lv: &mut Region, s: BlockState, p: P, same_space: bool) -> i64 {
    let other_source = !is_vein(s);
    let block = vein();
    let mut n = 0;
    for face in ALL {
        if !(other_source || mf_has_face(s, face)) {
            continue;
        }
        for sd in ALL {
            if same_axis(sd, face) {
                continue;
            }
            if !other_source && (!mf_has_face(s, face) || mf_has_face(s, sd)) {
                continue;
            }
            let all = [(p, sd), (add(p, sd), face), (add(add(p, sd), face), neg(sd))];
            let types = if same_space { &all[..1] } else { &all[..] };
            let mut found = None;
            for &(q, f) in types {
                let t = lv.get(q);
                if vein_can_replace(lv, p, q, f, t) && mf_valid(lv, block, t, q, f) {
                    found = Some((q, f));
                    break;
                }
            }
            let Some((q, f)) = found else { continue };
            let old = lv.get(q);
            if let Some(ns) = mf_state(lv, block, old, q, f) {
                lv.mark_post(q);
                if lv.set(q, ns) {
                    n += 1;
                }
            }
        }
    }
    n
}

/// `SculkVeinBlock.regrow`
fn regrow(lv: &mut Region, p: P, s: BlockState, facings: &[P]) -> bool {
    let mut ns = vein();
    let mut any = false;
    for &d in facings {
        if sturdy_toward(lv, p, d) {
            ns = with(ns, face_name(d), "true");
            any = true;
        }
    }
    if !any {
        return false;
    }
    if vstate::has_fluid(s) {
        ns = with(ns, "waterlogged", "true");
    }
    lv.set(p, ns);
    true
}

fn attempt_spread_vein(b: Beh, lv: &mut Region, p: P, s: BlockState, facings: &Option<Vec<P>>) -> bool {
    if b == Beh::Default {
        match facings {
            None => return vein_spread_all(lv, lv.get(p), p, true) > 0,
            Some(f) if !f.is_empty() => {
                return if vstate::air(s) || vstate::fluid(s) == 1 { regrow(lv, p, s, f) } else { false };
            }
            Some(_) => {}
        }
    }
    vein_spread_all(lv, s, p, false) > 0
}

/// `SculkVeinBlock.onDischarged`
fn on_discharged(b: Beh, lv: &mut Region, s: BlockState, p: P) {
    if b != Beh::Vein || !is_vein(s) {
        return;
    }
    let mut s = s;
    for d in ALL {
        if mf_has_face(s, d) && is(lv.get(add(p, d)), "sculk") {
            s = with(s, face_name(d), "false");
        }
    }
    if !ALL.iter().any(|&d| mf_has_face(s, d)) {
        s = if vstate::has_fluid(lv.get(p)) { crate::registry::blocks::WATER } else { crate::registry::blocks::AIR };
    }
    lv.set(p, s);
}

/// `SculkVeinBlock.attemptPlaceSculk`
fn attempt_place_sculk(lv: &mut Region, p: P, r: &mut WgRandom) -> bool {
    let s = lv.get(p);
    for d in all_shuffled(r) {
        if !mf_has_face(s, d) {
            continue;
        }
        let n = add(p, d);
        let ns = lv.get(n);
        if !lv.lib.extra_tags.sculk_replaceable_world_gen.contains(ns) {
            continue;
        }
        let sculk = st("sculk");
        lv.set(n, sculk);
        vein_spread_all(lv, sculk, n, false);
        let opp = neg(d);
        for dd in ALL {
            if dd == opp {
                continue;
            }
            let q = add(n, dd);
            let qs = lv.get(q);
            if is_vein(qs) {
                on_discharged(Beh::Vein, lv, qs, q);
            }
        }
        return true;
    }
    false
}

/// `SculkBlock.canPlaceGrowth`
fn can_place_growth(lv: &Region, p: P) -> bool {
    let a = lv.get(add(p, UP));
    if !(vstate::air(a) || (is(a, "water") && vstate::fluid(a) == 1)) {
        return false;
    }
    let mut n = 0;
    for y in p[1]..=p[1] + 2 {
        for z in p[2] - 4..=p[2] + 4 {
            for x in p[0] - 4..=p[0] + 4 {
                let s = lv.get([x, y, z]);
                if is(s, "sculk_sensor") || is(s, "sculk_shrieker") {
                    n += 1;
                    if n > 2 {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// `SculkBlock.getDecayPenalty`
fn decay_penalty(p: P, origin: P, charge: i32) -> i32 {
    let r = NO_GROWTH_RADIUS;
    let f = (dist_sqr(p, origin).sqrt() as f32 - r as f32).powi(2);
    let g = (24 - r) * (24 - r);
    let h = 1.0f32.min(f / g as f32);
    1.max((charge as f32 * h * 0.5) as i32)
}

struct Cursor {
    pos: P,
    charge: i32,
    decay_delay: i32,
    update_delay: i32,
    facings: Option<Vec<P>>,
}

fn attempt_use_charge(b: Beh, c: &Cursor, lv: &mut Region, origin: P, r: &mut WgRandom, spread_veins: bool) -> i32 {
    let charge = c.charge;
    match b {
        Beh::Default => {
            if c.decay_delay > 0 {
                charge
            } else {
                0
            }
        }
        Beh::Vein => {
            if spread_veins && attempt_place_sculk(lv, c.pos, r) {
                charge - 1
            } else if r.next_int_bound(CHARGE_DECAY_RATE) == 0 {
                (charge as f32 * 0.5).floor() as i32
            } else {
                charge
            }
        }
        Beh::Sculk => {
            if charge == 0 || r.next_int_bound(CHARGE_DECAY_RATE) != 0 {
                return charge;
            }
            let p = c.pos;
            let near = closer_than(p, origin, NO_GROWTH_RADIUS as f64);
            if !near && can_place_growth(lv, p) {
                let cost = GROWTH_SPAWN_COST;
                if r.next_int_bound(cost) < charge {
                    let above = add(p, UP);
                    let mut g = if r.next_int_bound(11) == 0 { with(st("sculk_shrieker"), "can_summon", "true") } else { st("sculk_sensor") };
                    if prop(g, "waterlogged").is_some() && vstate::has_fluid(lv.get(above)) {
                        g = with(g, "waterlogged", "true");
                    }
                    lv.set(above, g);
                }
                return 0.max(charge - cost);
            }
            if r.next_int_bound(ADDITIONAL_DECAY_RATE) != 0 {
                charge
            } else {
                charge - if near { 1 } else { decay_penalty(p, origin, charge) }
            }
        }
    }
}

/// `SculkSpreader.ChargeCursor.NON_CORNER_NEIGHBOURS`
fn non_corner() -> Vec<P> {
    let mut v = Vec::with_capacity(18);
    for z in -1..=1 {
        for y in -1..=1 {
            for x in -1..=1 {
                if (x == 0 || y == 0 || z == 0) && [x, y, z] != [0, 0, 0] {
                    v.push([x, y, z]);
                }
            }
        }
    }
    v
}

/// `SculkVeinBlock.hasSubstrateAccess`
fn has_substrate(lv: &Region, s: BlockState, p: P) -> bool {
    is_vein(s) && ALL.iter().any(|&d| mf_has_face(s, d) && lv.lib.extra_tags.sculk_replaceable.contains(lv.get(add(p, d))))
}

fn movement_unobstructed(lv: &Region, from: P, to: P) -> bool {
    if manhattan(from, to) == 1 {
        return true;
    }
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let dx = if d[0] < 0 { WEST } else { EAST };
    let dy = if d[1] < 0 { DOWN } else { UP };
    let dz = if d[2] < 0 { NORTH } else { SOUTH };
    let free = |dir: P| !sturdy_toward(lv, from, dir);
    if d[0] == 0 {
        free(dy) || free(dz)
    } else if d[1] == 0 {
        free(dx) || free(dz)
    } else {
        free(dx) || free(dy)
    }
}

fn valid_movement_pos(lv: &Region, p: P, r: &mut WgRandom) -> Option<P> {
    let mut offs = non_corner();
    shuffle(&mut offs, r);
    let mut res = p;
    for o in offs {
        let c = add(p, o);
        let cs = lv.get(c);
        if beh(cs) != Beh::Default && movement_unobstructed(lv, p, c) {
            res = c;
            if has_substrate(lv, cs, c) {
                break;
            }
        }
    }
    (res != p).then_some(res)
}

/// `SculkSpreader.ChargeCursor.update` (world generation)
fn update(c: &mut Cursor, lv: &mut Region, origin: P, r: &mut WgRandom, spread_veins: bool) {
    if c.charge <= 0 {
        return;
    }
    if c.update_delay > 0 {
        c.update_delay -= 1;
        return;
    }
    let mut s = lv.get(c.pos);
    let mut b = beh(s);
    if spread_veins && attempt_spread_vein(b, lv, c.pos, s, &c.facings) && b != Beh::Sculk {
        s = lv.get(c.pos);
        b = beh(s);
    }
    c.charge = attempt_use_charge(b, c, lv, origin, r, spread_veins);
    if c.charge <= 0 {
        on_discharged(b, lv, s, c.pos);
        return;
    }
    let Some(np) = valid_movement_pos(lv, c.pos, r) else { return };
    on_discharged(b, lv, s, c.pos);
    c.pos = np;
    if !closer_than(np, [origin[0], np[1], origin[2]], 15.0) {
        c.charge = 0;
        return;
    }
    let ns = lv.get(np);
    if beh(ns) != Beh::Default {
        c.facings = Some(ALL.iter().copied().filter(|&d| mf_has_face(ns, d)).collect());
    }
    c.decay_delay = if b == Beh::Default { (c.decay_delay - 1).max(0) } else { 1 };
    c.update_delay = 1;
}

/// `SculkPatchFeature.canSpreadFrom`
fn can_spread_from(lv: &Region, o: P) -> bool {
    let s = lv.get(o);
    if beh(s) != Beh::Default {
        return true;
    }
    if !(vstate::air(s) || (is(s, "water") && vstate::fluid_source(s))) {
        return false;
    }
    ALL.iter().any(|&d| vstate::full_block(lv.get(add(o, d))))
}

fn sculk_patch(lv: &mut Region, r: &mut WgRandom, o: P, c: &SculkPatch) -> bool {
    if !can_spread_from(lv, o) {
        return false;
    }
    let mut cursors: Vec<Cursor> = Vec::new();
    for round in 0..c.spread_rounds + c.growth_rounds {
        for _ in 0..c.charge_count {
            let mut amount = c.amount_per_charge;
            while amount > 0 {
                let ch = amount.min(1000);
                if cursors.len() < 32 {
                    cursors.push(Cursor { pos: o, charge: ch, decay_delay: 1, update_delay: 0, facings: None });
                }
                amount -= ch;
            }
        }
        let spread_veins = round < c.spread_rounds;
        for _ in 0..c.spread_attempts {
            if cursors.is_empty() {
                continue;
            }
            let mut keep = Vec::with_capacity(cursors.len());
            for mut cur in cursors.drain(..) {
                update(&mut cur, lv, o, r, spread_veins);
                if cur.charge > 0 {
                    keep.push(cur);
                }
            }
            cursors = keep;
        }
        cursors.clear();
    }
    let below = add(o, DOWN);
    if r.next_float() <= c.catalyst_chance && vstate::full_block(lv.get(below)) {
        lv.set(o, st("sculk_catalyst"));
    }
    let n = c.extra_rare_growths.sample(r);
    for _ in 0..n {
        let dx = r.next_int_bound(5) - 2;
        let dz = r.next_int_bound(5) - 2;
        let p = add(o, [dx, 0, dz]);
        if vstate::air(lv.get(p)) && vstate::sturdy(lv.get(add(p, DOWN)), 1) {
            lv.set(p, with(st("sculk_shrieker"), "can_summon", "true"));
        }
    }
    true
}
