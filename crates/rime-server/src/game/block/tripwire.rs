//! `TripWireHookBlock` and `TripWireBlock`.

use super::*;
use crate::anticheat::combat::Aabb;

fn is_hook(s: BlockState) -> bool {
    class(s) == Class::TripWireHook
}

fn is_wire(s: BlockState) -> bool {
    class(s) == Class::TripWire
}

fn emit_state(g: &Game, p: [i32; 3], attached: bool, powered: bool, was_attached: bool, was_powered: bool) {
    if powered && !was_powered {
        sound(g, p, "minecraft:block.tripwire.click_on", 0.4, 0.6);
    } else if !powered && was_powered {
        sound(g, p, "minecraft:block.tripwire.click_off", 0.4, 0.5);
    } else if attached && !was_attached {
        sound(g, p, "minecraft:block.tripwire.attach", 0.4, 0.7);
    } else if !attached && was_attached {
        sound(g, p, "minecraft:block.tripwire.detach", 0.4, 1.2 / (rand::random::<f32>() * 0.2 + 0.9));
    }
}

fn notify(g: &Game, p: [i32; 3], s: BlockState, d: u8) {
    g.update_neighbors_at(p, s);
    g.update_neighbors_at(rel(p, opp(d)), s);
}

/// `TripWireHookBlock.calculateState`
pub fn calculate_state(g: &Game, p: [i32; 3], s: BlockState, destroying: bool, notify_neighbours: bool, search: i32, wire_state: Option<BlockState>) {
    let dir = facing(s);
    let was_attached = flag(s, "attached");
    let was_powered = flag(s, "powered");
    let mut attached = !destroying;
    let mut powered = false;
    let mut other = 0;
    let mut wires: [Option<BlockState>; 42] = [None; 42];
    let mut q = p;
    for j in 1..42 {
        q = rel(q, dir);
        let mut bs = g.bs(q);
        if is_hook(bs) {
            if facing(bs) == opp(dir) {
                other = j;
            }
            break;
        }
        if !is_wire(bs) && j != search {
            wires[j as usize] = None;
            attached = false;
        } else {
            if j == search {
                bs = wire_state.unwrap_or(bs);
            }
            let armed = !flag(bs, "disarmed");
            powered |= armed && flag(bs, "powered");
            wires[j as usize] = Some(bs);
            if j == search {
                g.schedule_tick(p, s, 10, prio::NORMAL);
                attached &= armed;
            }
        }
    }
    attached &= other > 1;
    powered &= attached;
    let hook = with_bool(with_bool(BlockState(BlockRegistry::get().block(s).default), "attached", attached), "powered", powered);
    if other > 0 {
        let mut op = p;
        for _ in 0..other {
            op = rel(op, dir);
        }
        let od = opp(dir);
        g.set_block(op, with_facing(hook, od), flags::ALL);
        notify(g, op, s, od);
        emit_state(g, op, attached, powered, was_attached, was_powered);
    }
    emit_state(g, p, attached, powered, was_attached, was_powered);
    if !destroying {
        g.set_block(p, with_facing(hook, dir), flags::ALL);
        if notify_neighbours {
            notify(g, p, s, dir);
        }
    }
    if was_attached != attached {
        let mut q = p;
        for k in 1..other {
            q = rel(q, dir);
            if let Some(bs) = wires[k as usize] {
                let cur = g.bs(q);
                if is_wire(cur) || is_hook(cur) {
                    g.set_block(q, with_bool(bs, "attached", attached), flags::ALL);
                }
            }
        }
    }
}

pub fn hook_tick(g: &Game, p: [i32; 3], s: BlockState) {
    calculate_state(g, p, s, false, true, -1, None);
}

pub fn hook_placed(g: &Game, p: [i32; 3], s: BlockState) {
    calculate_state(g, p, s, false, false, -1, None);
}

pub fn hook_removed(g: &Game, p: [i32; 3], old: BlockState) {
    let attached = flag(old, "attached");
    let powered = flag(old, "powered");
    if attached || powered {
        calculate_state(g, p, old, true, false, -1, None);
    }
    if powered {
        notify(g, p, old, facing(old));
    }
}

// ------------------------------------------------------------ wire

/// `TripWireBlock.shouldConnectTo`
fn connects(ns: BlockState, dir: u8) -> bool {
    if is_hook(ns) {
        facing(ns) == opp(dir)
    } else {
        is_wire(ns)
    }
}

pub fn wire_placement(g: &Game, p: [i32; 3], s: BlockState) -> BlockState {
    let mut st = s;
    for d in HORIZONTAL_ORDER {
        st = with_bool(st, DIR_NAMES[d as usize], connects(g.bs(rel(p, d)), d));
    }
    st
}

pub fn wire_update_shape(s: BlockState, dir: u8, ns: BlockState) -> BlockState {
    if axis(dir) == 1 {
        return s;
    }
    with_bool(s, DIR_NAMES[dir as usize], connects(ns, dir))
}

/// `TripWireBlock.updateSource`
pub fn update_source(g: &Game, p: [i32; 3], s: BlockState) {
    for d in [SOUTH, WEST] {
        let mut q = p;
        for i in 1..42 {
            q = rel(q, d);
            let bs = g.bs(q);
            if is_hook(bs) {
                if facing(bs) == opp(d) {
                    calculate_state(g, q, bs, false, true, i, Some(s));
                }
                break;
            }
            if !is_wire(bs) {
                break;
            }
        }
    }
}

pub fn wire_on_place(g: &Game, p: [i32; 3], s: BlockState, old: BlockState) {
    if !same_block(old, s) {
        update_source(g, p, s);
    }
}

pub fn wire_removed(g: &Game, p: [i32; 3], old: BlockState) {
    update_source(g, p, with_bool(old, "powered", true));
}

fn wire_box(p: [i32; 3], s: BlockState) -> Aabb {
    let (y0, y1) = if flag(s, "attached") { (1.0 / 16.0, 2.5 / 16.0) } else { (0.0, 0.5) };
    Aabb { min: [p[0] as f64, p[1] as f64 + y0, p[2] as f64], max: [p[0] as f64 + 1.0, p[1] as f64 + y1, p[2] as f64 + 1.0] }
}

/// `TripWireBlock.checkPressed`
pub fn check_pressed(g: &Game, p: [i32; 3]) {
    let s = g.bs(p);
    if !is_wire(s) {
        return;
    }
    let was = flag(s, "powered");
    let now = g.count_entities_in(&wire_box(p, s), false) > 0;
    if now != was {
        let ns = with_bool(s, "powered", now);
        g.set_block(p, ns, flags::ALL);
        update_source(g, p, ns);
    }
    if now {
        g.schedule_tick(p, g.bs(p), 10, prio::NORMAL);
    }
}

pub fn wire_tick(g: &Game, p: [i32; 3], s: BlockState) {
    if flag(s, "powered") {
        check_pressed(g, p);
    }
}

pub fn wire_entity_inside(g: &Game, p: [i32; 3], s: BlockState) {
    if !flag(s, "powered") {
        check_pressed(g, p);
    }
}
