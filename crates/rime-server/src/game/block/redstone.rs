//! Redstone components, ported from the 26.1 classes: `RedStoneWireBlock`
//! (+ `DefaultRedstoneWireEvaluator`), `RedstoneTorchBlock`, `DiodeBlock` /
//! `RepeaterBlock` / `ComparatorBlock`, `RedstoneLampBlock`, `ObserverBlock`,
//! `ButtonBlock`, `LeverBlock`, `BasePressurePlateBlock`, `NoteBlock`,
//! `TntBlock`, `CopperBulbBlock`, `BellBlock`.

use super::*;

// ------------------------------------------------------------ wire

fn is_wire(s: BlockState) -> bool {
    class(s) == Class::Wire
}

fn power(s: BlockState) -> u8 {
    int(s, "power") as u8
}

fn side(s: BlockState, d: u8) -> &'static str {
    prop(s, DIR_NAMES[d as usize])
}

fn connected(s: BlockState, d: u8) -> bool {
    side(s, d) != "none"
}

fn is_cross(s: BlockState) -> bool {
    HORIZONTAL.iter().all(|&d| connected(s, d))
}

fn is_dot(s: BlockState) -> bool {
    HORIZONTAL.iter().all(|&d| !connected(s, d))
}

/// `RedStoneWireBlock.canSurviveOn`
fn survives_on(g: &Game, p: [i32; 3]) -> bool {
    let s = g.bs(p);
    vstate::sturdy(s, UP) || s.name() == "hopper"
}

/// `RedStoneWireBlock.shouldConnectTo(state, dir)`
fn should_connect_to(s: BlockState, dir: Option<u8>) -> bool {
    match class(s) {
        Class::Wire => true,
        Class::Repeater => dir.is_some_and(|d| facing(s) == d || opp(facing(s)) == d),
        Class::Observer => dir == Some(facing(s)),
        _ => rstate::signal_source(s) && dir.is_some(),
    }
}

/// `RedStoneWireBlock.getConnectingSide(level, pos, dir, canConnectUp)`
fn connecting_side(g: &Game, p: [i32; 3], d: u8, up_ok: bool) -> &'static str {
    let np = rel(p, d);
    let ns = g.bs(np);
    if up_ok {
        let b = class(ns) == Class::TrapDoor || survives_on(g, np);
        if b && should_connect_to(g.bs(rel(np, UP)), None) {
            return if vstate::sturdy(ns, opp(d)) { "up" } else { "side" };
        }
    }
    if should_connect_to(ns, Some(d)) || (!rstate::conductor(ns) && should_connect_to(g.bs(rel(np, DOWN)), None)) {
        "side"
    } else {
        "none"
    }
}

fn up_ok(g: &Game, p: [i32; 3]) -> bool {
    !rstate::conductor(g.bs(rel(p, UP)))
}

/// `RedStoneWireBlock.getMissingConnections`
fn missing_connections(g: &Game, s: BlockState, p: [i32; 3]) -> BlockState {
    let up = up_ok(g, p);
    let mut out = s;
    for d in HORIZONTAL_ORDER {
        if !connected(out, d) {
            out = with(out, DIR_NAMES[d as usize], connecting_side(g, p, d, up));
        }
    }
    out
}

/// `RedStoneWireBlock.getConnectionState`
pub fn connection_state(g: &Game, s: BlockState, p: [i32; 3]) -> BlockState {
    let dot = is_dot(s);
    let base = with_int(s.default_state(), "power", power(s) as i32);
    let mut out = missing_connections(g, base, p);
    if dot && is_dot(out) {
        return out;
    }
    let (n, so, e, w) = (connected(out, NORTH), connected(out, SOUTH), connected(out, EAST), connected(out, WEST));
    let ns_none = !n && !so;
    let ew_none = !e && !w;
    if !w && ns_none {
        out = with(out, "west", "side");
    }
    if !e && ns_none {
        out = with(out, "east", "side");
    }
    if !n && ew_none {
        out = with(out, "north", "side");
    }
    if !so && ew_none {
        out = with(out, "south", "side");
    }
    out
}

fn cross_state(s: BlockState) -> BlockState {
    let mut c = s.default_state();
    for d in HORIZONTAL {
        c = with(c, DIR_NAMES[d as usize], "side");
    }
    c
}

/// `RedStoneWireBlock.getStateForPlacement`
pub fn wire_placement(g: &Game, p: [i32; 3], s: BlockState) -> BlockState {
    connection_state(g, cross_state(s), p)
}

pub fn wire_update_shape(g: &Game, p: [i32; 3], s: BlockState, dir: u8, np: [i32; 3], ns: BlockState) -> BlockState {
    if dir == DOWN {
        let _ = ns;
        return if survives_on(g, np) { s } else { blocks::AIR };
    }
    if dir == UP {
        return connection_state(g, s, p);
    }
    let sd = connecting_side(g, p, dir, up_ok(g, p));
    let name = DIR_NAMES[dir as usize];
    if (sd != "none") == connected(s, dir) && !is_cross(s) {
        with(s, name, sd)
    } else {
        connection_state(g, with(with_int(cross_state(s), "power", power(s) as i32), name, sd), p)
    }
}

pub fn wire_indirect_shapes(g: &Game, p: [i32; 3], s: BlockState, f: u32, limit: i32) {
    for d in HORIZONTAL_ORDER {
        if connected(s, d) && !is_wire(g.bs(rel(p, d))) {
            let down = rel(rel(p, d), DOWN);
            if is_wire(g.bs(down)) {
                let q = rel(down, opp(d));
                g.neighbour_shape_changed(opp(d), down, q, g.bs(q), f, limit);
            }
            let up = rel(rel(p, d), UP);
            if is_wire(g.bs(up)) {
                let q = rel(up, opp(d));
                g.neighbour_shape_changed(opp(d), up, q, g.bs(q), f, limit);
            }
        }
    }
}

pub fn wire_signal(g: &Game, p: [i32; 3], s: BlockState, dir: u8) -> u8 {
    if g.lv(|l| l.wire_silent) || dir == DOWN {
        return 0;
    }
    let i = power(s);
    if i == 0 {
        return 0;
    }
    if dir == UP || connected(connection_state(g, s, p), opp(dir)) {
        i
    } else {
        0
    }
}

fn wire_power_at(g: &Game, p: [i32; 3]) -> u8 {
    let s = g.bs(p);
    if is_wire(s) {
        power(s)
    } else {
        0
    }
}

/// `DefaultRedstoneWireEvaluator.calculateTargetStrength`
fn target_strength(g: &Game, p: [i32; 3]) -> u8 {
    g.lv(|l| l.wire_silent = true);
    let i = g.best_neighbor_signal(p);
    g.lv(|l| l.wire_silent = false);
    let mut j = 0;
    if i < 15 {
        for d in HORIZONTAL_ORDER {
            let np = rel(p, d);
            let ns = g.bs(np);
            j = j.max(wire_power_at(g, np));
            let above = rel(p, UP);
            if rstate::conductor(ns) && !rstate::conductor(g.bs(above)) {
                j = j.max(wire_power_at(g, rel(np, UP)));
            } else if !rstate::conductor(ns) {
                j = j.max(wire_power_at(g, rel(np, DOWN)));
            }
        }
    }
    i.max(j.saturating_sub(1))
}

/// `DefaultRedstoneWireEvaluator.updatePowerStrength`
fn update_power(g: &Game, p: [i32; 3], s: BlockState) {
    let target = target_strength(g, p);
    if power(s) != target {
        if g.bs(p) == s {
            g.set_block(p, with_int(s, "power", target as i32), flags::CLIENTS);
        }
        let mut set = vec![p];
        for d in 0..6u8 {
            set.push(rel(p, d));
        }
        let wire = BlockState::by_name("redstone_wire");
        for q in java_hashset_order(&set) {
            g.update_neighbors_at(q, wire);
        }
    }
}

fn check_corner(g: &Game, p: [i32; 3]) {
    let s = g.bs(p);
    if is_wire(s) {
        g.update_neighbors_at(p, s);
        for d in 0..6u8 {
            g.update_neighbors_at(rel(p, d), s);
        }
    }
}

fn update_neighbors_of_wires(g: &Game, p: [i32; 3]) {
    for d in HORIZONTAL_ORDER {
        check_corner(g, rel(p, d));
    }
    for d in HORIZONTAL_ORDER {
        let np = rel(p, d);
        if rstate::conductor(g.bs(np)) {
            check_corner(g, rel(np, UP));
        } else {
            check_corner(g, rel(np, DOWN));
        }
    }
}

pub fn wire_on_place(g: &Game, p: [i32; 3], s: BlockState, old: BlockState) {
    if !is_wire(old) {
        update_power(g, p, s);
        for d in [DOWN, UP] {
            g.update_neighbors_at(rel(p, d), s);
        }
        update_neighbors_of_wires(g, p);
    }
}

pub fn wire_after_removal(g: &Game, p: [i32; 3], old: BlockState) {
    for d in 0..6u8 {
        g.update_neighbors_at(rel(p, d), old);
    }
    update_power(g, p, old);
    update_neighbors_of_wires(g, p);
}

pub fn wire_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    if survives_on(g, rel(p, DOWN)) {
        update_power(g, p, s);
    } else {
        g.drop_loot(p, s, None);
        g.remove_block(p, false);
    }
}

/// right click on a dot / cross toggles it
pub fn wire_use(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    if is_cross(s) || is_dot(s) {
        let mut ns = if is_cross(s) { s.default_state() } else { cross_state(s) };
        ns = with_int(ns, "power", power(s) as i32);
        ns = connection_state(g, ns, p);
        if ns != s {
            g.set_block(p, ns, flags::ALL);
            // updatesOnShapeChange
            for d in HORIZONTAL_ORDER {
                let q = rel(p, d);
                if connected(s, d) != connected(ns, d) && rstate::conductor(g.bs(q)) {
                    g.update_neighbors_except(q, ns, opp(d));
                }
            }
            return true;
        }
    }
    false
}

// ------------------------------------------------------------ torches

fn torch_has_signal(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    if class(s) == Class::WallTorch {
        let d = opp(facing(s));
        g.has_signal(rel(p, d), d)
    } else {
        g.has_signal(rel(p, DOWN), DOWN)
    }
}

pub fn torch_notify(g: &Game, p: [i32; 3], s: BlockState) {
    for d in 0..6u8 {
        g.update_neighbors_at(rel(p, d), s);
    }
}

pub fn torch_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    if flag(s, "lit") == torch_has_signal(g, p, s) && !g.will_tick_this_tick(p, s) {
        g.schedule_tick(p, s, 2, prio::NORMAL);
    }
}

fn toggled_too_often(g: &Game, p: [i32; 3], add: bool) -> bool {
    let now = g.game_time();
    g.lv(|l| {
        if add {
            l.torch_toggles.push((p, now));
        }
        l.torch_toggles.iter().filter(|t| t.0 == p).count() >= 8
    })
}

pub fn torch_tick(g: &Game, p: [i32; 3], s: BlockState) {
    let b = torch_has_signal(g, p, s);
    let now = g.game_time();
    g.lv(|l| {
        while l.torch_toggles.first().is_some_and(|t| now - t.1 > 60) {
            l.torch_toggles.remove(0);
        }
    });
    if flag(s, "lit") {
        if b {
            g.set_block(p, with_bool(s, "lit", false), flags::ALL);
            if toggled_too_often(g, p, true) {
                g.level_event(1502, p, 0);
                let now_s = g.bs(p);
                g.schedule_tick(p, now_s, 160, prio::NORMAL);
            }
        }
    } else if !b && !toggled_too_often(g, p, false) {
        g.set_block(p, with_bool(s, "lit", true), flags::ALL);
    }
}

// ------------------------------------------------------------ diodes

/// output side: towards `facing.opposite`
pub fn diode_signal(g: &Game, p: [i32; 3], s: BlockState, dir: u8) -> u8 {
    if !flag(s, "powered") {
        return 0;
    }
    if facing(s) == dir {
        diode_output(g, p, s)
    } else {
        0
    }
}

fn diode_output(g: &Game, p: [i32; 3], s: BlockState) -> u8 {
    if class(s) == Class::Comparator {
        comparator_output(g, p)
    } else {
        15
    }
}

fn delay(s: BlockState) -> u32 {
    if class(s) == Class::Comparator {
        2
    } else {
        (int(s, "delay").max(1) * 2) as u32
    }
}

/// `DiodeBlock.getInputSignal` (+ the comparator's container reading)
fn input_signal(g: &Game, p: [i32; 3], s: BlockState) -> u8 {
    let d = facing(s);
    let ip = rel(p, d);
    let i = g.get_signal(ip, d);
    let i = if i >= 15 {
        i
    } else {
        let is = g.bs(ip);
        i.max(if is_wire(is) { power(is) } else { 0 })
    };
    if class(s) != Class::Comparator {
        return i;
    }
    let bs = g.bs(ip);
    if rstate::analog(bs) {
        return analog(g, ip, bs, opp(d));
    }
    if i < 15 && rstate::conductor(bs) {
        let bp = rel(ip, d);
        let b2 = g.bs(bp);
        let frame = g.item_frame_analog(bp, d);
        let k = frame.or(if rstate::analog(b2) { Some(analog(g, bp, b2, opp(d))) } else { None });
        if let Some(k) = k {
            return k;
        }
    }
    i
}

fn alternate_signal(g: &Game, p: [i32; 3], s: BlockState) -> u8 {
    let d = facing(s);
    let (a, b) = (cw(d), ccw(d));
    let only = class(s) == Class::Repeater;
    g.control_input_signal(rel(p, a), a, only).max(g.control_input_signal(rel(p, b), b, only))
}

fn locked(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    class(s) == Class::Repeater && alternate_signal(g, p, s) > 0
}

fn should_turn_on(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    if class(s) == Class::Comparator {
        let i = input_signal(g, p, s);
        if i == 0 {
            return false;
        }
        let j = alternate_signal(g, p, s);
        return i > j || (i == j && prop(s, "mode") == "compare");
    }
    input_signal(g, p, s) > 0
}

fn should_prioritize(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    let d = opp(facing(s));
    let b = g.bs(rel(p, d));
    is_diode(b) && facing(b) != d
}

pub fn diode_update_front(g: &Game, p: [i32; 3], s: BlockState) {
    let d = facing(s);
    let front = rel(p, opp(d));
    g.neighbor_changed(front, s);
    g.update_neighbors_except(front, s, d);
}

fn diode_survives(g: &Game, p: [i32; 3]) -> bool {
    can_support_rigid(g, rel(p, DOWN))
}

pub fn diode_update_shape(g: &Game, p: [i32; 3], s: BlockState, dir: u8, _np: [i32; 3], _ns: BlockState) -> BlockState {
    if dir == DOWN && !diode_survives(g, p) {
        return blocks::AIR;
    }
    if class(s) == Class::Repeater && axis(dir) != axis(facing(s)) {
        return with_bool(s, "locked", locked(g, p, s));
    }
    s
}

pub fn diode_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    if !diode_survives(g, p) {
        g.drop_loot(p, s, None);
        g.remove_block(p, false);
        for d in 0..6u8 {
            g.update_neighbors_at(rel(p, d), s);
        }
        return;
    }
    if class(s) == Class::Comparator {
        if !g.will_tick_this_tick(p, s) {
            let i = calculate_output(g, p, s);
            let j = comparator_output(g, p);
            if i != j || flag(s, "powered") != should_turn_on(g, p, s) {
                let pr = if should_prioritize(g, p, s) { prio::HIGH } else { prio::NORMAL };
                g.schedule_tick(p, s, 2, pr);
            }
        }
        return;
    }
    if !locked(g, p, s) {
        let powered = flag(s, "powered");
        let on = should_turn_on(g, p, s);
        if powered != on && !g.will_tick_this_tick(p, s) {
            let pr = if should_prioritize(g, p, s) {
                prio::EXTREMELY_HIGH
            } else if powered {
                prio::VERY_HIGH
            } else {
                prio::HIGH
            };
            g.schedule_tick(p, s, delay(s), pr);
        }
    }
}

/// `DiodeBlock.tick` (repeater)
pub fn repeater_tick(g: &Game, p: [i32; 3], s: BlockState) {
    if locked(g, p, s) {
        return;
    }
    let powered = flag(s, "powered");
    let on = should_turn_on(g, p, s);
    if powered && !on {
        g.set_block(p, with_bool(s, "powered", false), flags::CLIENTS);
    } else if !powered {
        g.set_block(p, with_bool(s, "powered", true), flags::CLIENTS);
        if !on {
            let now = g.bs(p);
            g.schedule_tick(p, now, delay(s), prio::VERY_HIGH);
        }
    }
}

/// `DiodeBlock.setPlacedBy`: placed already powered
pub fn diode_placed(g: &Game, p: [i32; 3], s: BlockState) {
    if should_turn_on(g, p, s) {
        g.schedule_tick(p, s, 1, prio::NORMAL);
    }
}

pub fn repeater_placement(g: &Game, p: [i32; 3], s: BlockState) -> BlockState {
    with_bool(s, "locked", locked(g, p, s))
}

pub fn repeater_use(g: &Game, p: [i32; 3], s: BlockState) {
    let d = int(s, "delay") % 4 + 1;
    g.set_block(p, with_int(s, "delay", d), flags::ALL);
}

fn comparator_output(g: &Game, p: [i32; 3]) -> u8 {
    g.world.block_entity(p).and_then(|t| t.get("OutputSignal").and_then(|v| v.as_i64())).unwrap_or(0).clamp(0, 15) as u8
}

fn set_comparator_output(g: &Game, p: [i32; 3], v: u8) {
    let mut t = g.world.block_entity(p).unwrap_or_else(|| super::super::blockentity::new_tag("minecraft:comparator", p));
    if let crate::nbt::Tag::Compound(c) = &mut t {
        c.retain(|(k, _)| k != "OutputSignal");
        c.push(("OutputSignal".into(), crate::nbt::Tag::Int(v as i32)));
    }
    g.world.set_block_entity(p, Some(t), false);
}

fn calculate_output(g: &Game, p: [i32; 3], s: BlockState) -> u8 {
    let i = input_signal(g, p, s);
    if i == 0 {
        return 0;
    }
    let j = alternate_signal(g, p, s);
    if j > i {
        return 0;
    }
    if prop(s, "mode") == "subtract" {
        i - j
    } else {
        i
    }
}

/// `ComparatorBlock.refreshOutputState` (also its scheduled tick)
pub fn comparator_refresh(g: &Game, p: [i32; 3], s: BlockState) {
    let i = calculate_output(g, p, s);
    let j = comparator_output(g, p);
    set_comparator_output(g, p, i);
    if j != i || prop(s, "mode") == "compare" {
        let on = should_turn_on(g, p, s);
        let powered = flag(s, "powered");
        if powered && !on {
            g.set_block(p, with_bool(s, "powered", false), flags::CLIENTS);
        } else if !powered && on {
            g.set_block(p, with_bool(s, "powered", true), flags::CLIENTS);
        }
        diode_update_front(g, p, g.bs(p));
    }
}

pub fn comparator_use(g: &Game, p: [i32; 3], s: BlockState) {
    let sub = prop(s, "mode") == "compare";
    let ns = with(s, "mode", if sub { "subtract" } else { "compare" });
    sound(g, p, "minecraft:block.comparator.click", 0.3, if sub { 0.55 } else { 0.5 });
    g.set_block(p, ns, flags::CLIENTS);
    comparator_refresh(g, p, ns);
}

// ------------------------------------------------------------ lamp

pub fn lamp_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    let lit = flag(s, "lit");
    if lit != g.has_neighbor_signal(p) {
        if lit {
            g.schedule_tick(p, s, 4, prio::NORMAL);
        } else {
            g.set_block(p, with_bool(s, "lit", true), flags::CLIENTS);
        }
    }
}

// ------------------------------------------------------------ observer

pub fn observer_update_shape(g: &Game, p: [i32; 3], s: BlockState, dir: u8) -> BlockState {
    if facing(s) == dir && !flag(s, "powered") && !g.has_scheduled(p, s) {
        g.schedule_tick(p, s, 2, prio::NORMAL);
    }
    s
}

pub fn observer_update_front(g: &Game, p: [i32; 3], s: BlockState) {
    let d = facing(s);
    let back = rel(p, opp(d));
    g.neighbor_changed(back, s);
    g.update_neighbors_except(back, s, d);
}

pub fn observer_tick(g: &Game, p: [i32; 3], s: BlockState) {
    if flag(s, "powered") {
        g.set_block(p, with_bool(s, "powered", false), flags::CLIENTS);
    } else {
        g.set_block(p, with_bool(s, "powered", true), flags::CLIENTS);
        g.schedule_tick(p, s, 2, prio::NORMAL);
    }
    observer_update_front(g, p, g.bs(p));
}

pub fn observer_on_place(g: &Game, p: [i32; 3], s: BlockState, old: BlockState) {
    if !same_block(s, old) && flag(s, "powered") && !g.has_scheduled(p, s) {
        let ns = with_bool(s, "powered", false);
        g.set_block(p, ns, flags::CLIENTS | flags::KNOWN_SHAPE);
        observer_update_front(g, p, ns);
    }
}

// ------------------------------------------------------------ buttons, levers

/// `FaceAttachedHorizontalDirectionalBlock.getConnectedDirection`
pub fn attached_dir(s: BlockState) -> u8 {
    match prop(s, "face") {
        "ceiling" => DOWN,
        "floor" => UP,
        _ => facing(s),
    }
}

pub fn face_attached_survives(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    let d = opp(attached_dir(s));
    sturdy(g, rel(p, d), opp(d))
}

pub fn face_attached_update(g: &Game, p: [i32; 3], s: BlockState) {
    g.update_neighbors_at(p, s);
    g.update_neighbors_at(rel(p, opp(attached_dir(s))), s);
}

pub fn lever_use(g: &Game, p: [i32; 3], s: BlockState) {
    let ns = with_bool(s, "powered", !flag(s, "powered"));
    g.set_block(p, ns, flags::ALL);
    face_attached_update(g, p, ns);
    sound(g, p, "minecraft:block.lever.click", 0.3, if flag(ns, "powered") { 0.6 } else { 0.5 });
}

fn wooden_button(s: BlockState) -> bool {
    !matches!(s.name(), "stone_button" | "polished_blackstone_button")
}

fn button_sound(s: BlockState, on: bool) -> String {
    let n = s.name();
    let set = if n.starts_with("crimson") || n.starts_with("warped") {
        "nether_wood_button"
    } else if n.starts_with("bamboo") {
        "bamboo_wood_button"
    } else if n.starts_with("cherry") {
        "cherry_wood_button"
    } else if wooden_button(s) {
        "wooden_button"
    } else {
        "stone_button"
    };
    format!("minecraft:block.{set}.click_{}", if on { "on" } else { "off" })
}

pub fn button_press(g: &Game, p: [i32; 3], s: BlockState) {
    let ns = with_bool(s, "powered", true);
    g.set_block(p, ns, flags::ALL);
    face_attached_update(g, p, ns);
    g.schedule_tick(p, ns, if wooden_button(s) { 30 } else { 20 }, prio::NORMAL);
    sound(g, p, &button_sound(s, true), 1.0, 1.0);
}

pub fn button_tick(g: &Game, p: [i32; 3], s: BlockState) {
    if !flag(s, "powered") {
        return;
    }
    if wooden_button(s) && g.arrow_in_block(p, s) {
        g.schedule_tick(p, s, 30, prio::NORMAL);
        return;
    }
    let ns = with_bool(s, "powered", false);
    g.set_block(p, ns, flags::ALL);
    face_attached_update(g, p, ns);
    sound(g, p, &button_sound(s, false), 1.0, 1.0);
}

/// an arrow stuck in a wooden button presses it
pub fn button_arrow(g: &Game, p: [i32; 3], s: BlockState) {
    if wooden_button(s) && !flag(s, "powered") {
        button_press(g, p, s);
    }
}

// ------------------------------------------------------------ pressure plates

pub fn plate_signal(s: BlockState) -> u8 {
    if class(s) == Class::WeightedPlate {
        int(s, "power") as u8
    } else if flag(s, "powered") {
        15
    } else {
        0
    }
}

fn plate_with_signal(s: BlockState, v: u8) -> BlockState {
    if class(s) == Class::WeightedPlate {
        with_int(s, "power", v as i32)
    } else {
        with_bool(s, "powered", v > 0)
    }
}

pub fn plate_survives(g: &Game, p: [i32; 3]) -> bool {
    let b = rel(p, DOWN);
    can_support_rigid(g, b) || can_support_center(g, b, UP)
}

pub fn plate_update(g: &Game, p: [i32; 3], s: BlockState) {
    g.update_neighbors_at(p, s);
    g.update_neighbors_at(rel(p, DOWN), s);
}

fn plate_strength(g: &Game, p: [i32; 3], s: BlockState) -> u8 {
    let bx = crate::anticheat::combat::Aabb { min: [p[0] as f64 + 0.0625, p[1] as f64, p[2] as f64 + 0.0625], max: [p[0] as f64 + 0.9375, p[1] as f64 + 0.25, p[2] as f64 + 0.9375] };
    let name = s.name();
    if class(s) == Class::WeightedPlate {
        let max = if name == "light_weighted_pressure_plate" { 15 } else { 150 };
        let n = g.count_entities_in(&bx, false).min(max);
        if n > 0 {
            let f = n.min(max) as f32 / max as f32;
            (f * 15.0).ceil() as u8
        } else {
            0
        }
    } else {
        // stone / polished blackstone: mobs only; wooden & others: everything
        let mobs_only = matches!(name, "stone_pressure_plate" | "polished_blackstone_pressure_plate");
        if g.count_entities_in(&bx, mobs_only) > 0 {
            15
        } else {
            0
        }
    }
}

fn plate_sound(s: BlockState, on: bool) -> String {
    let n = s.name();
    let set = if class(s) == Class::WeightedPlate {
        "metal_pressure_plate"
    } else if n.starts_with("stone") || n.starts_with("polished_blackstone") {
        "stone_pressure_plate"
    } else if n.starts_with("crimson") || n.starts_with("warped") {
        "nether_wood_pressure_plate"
    } else if n.starts_with("bamboo") {
        "bamboo_wood_pressure_plate"
    } else if n.starts_with("cherry") {
        "cherry_wood_pressure_plate"
    } else {
        "wooden_pressure_plate"
    };
    format!("minecraft:block.{set}.click_{}", if on { "on" } else { "off" })
}

/// `BasePressurePlateBlock.checkPressed`
pub fn plate_check(g: &Game, p: [i32; 3], s: BlockState, cur: u8) {
    let j = plate_strength(g, p, s);
    let (was, now) = (cur > 0, j > 0);
    if cur != j {
        let ns = plate_with_signal(s, j);
        g.set_block(p, ns, flags::CLIENTS);
        plate_update(g, p, ns);
    }
    if !now && was {
        sound(g, p, &plate_sound(s, false), 0.3, 0.5);
    } else if now && !was {
        sound(g, p, &plate_sound(s, true), 0.3, 0.6);
    }
    if now {
        let pressed = if class(s) == Class::WeightedPlate { 10 } else { 20 };
        g.schedule_tick(p, g.bs(p), pressed, prio::NORMAL);
    }
}

/// `entityInside`: an entity on an unpressed plate
pub fn plate_entity_inside(g: &Game, p: [i32; 3], s: BlockState) {
    let i = plate_signal(s);
    if i == 0 {
        plate_check(g, p, s, i);
    }
}

// ------------------------------------------------------------ note block

pub fn note_instrument(g: &Game, p: [i32; 3], s: BlockState) -> BlockState {
    let names = instruments();
    let above = rstate::instrument(g.bs(rel(p, UP))) as usize;
    if names[above].3 {
        return with(s, "instrument", names[above].0);
    }
    let below = rstate::instrument(g.bs(rel(p, DOWN))) as usize;
    let i = if names[below].3 { 0 } else { below };
    with(s, "instrument", names[i].0)
}

/// (name, sound, tunable, works above note block)
pub fn instruments() -> &'static [(&'static str, &'static str, bool, bool)] {
    &[
        ("harp", "minecraft:block.note_block.harp", true, false),
        ("basedrum", "minecraft:block.note_block.basedrum", true, false),
        ("snare", "minecraft:block.note_block.snare", true, false),
        ("hat", "minecraft:block.note_block.hat", true, false),
        ("bass", "minecraft:block.note_block.bass", true, false),
        ("flute", "minecraft:block.note_block.flute", true, false),
        ("bell", "minecraft:block.note_block.bell", true, false),
        ("guitar", "minecraft:block.note_block.guitar", true, false),
        ("chime", "minecraft:block.note_block.chime", true, false),
        ("xylophone", "minecraft:block.note_block.xylophone", true, false),
        ("iron_xylophone", "minecraft:block.note_block.iron_xylophone", true, false),
        ("cow_bell", "minecraft:block.note_block.cow_bell", true, false),
        ("didgeridoo", "minecraft:block.note_block.didgeridoo", true, false),
        ("bit", "minecraft:block.note_block.bit", true, false),
        ("banjo", "minecraft:block.note_block.banjo", true, false),
        ("pling", "minecraft:block.note_block.pling", true, false),
        ("trumpet", "minecraft:block.note_block.trumpet", true, false),
        ("trumpet_exposed", "minecraft:block.note_block.trumpet_exposed", true, false),
        ("trumpet_oxidized", "minecraft:block.note_block.trumpet_oxidized", true, false),
        ("trumpet_weathered", "minecraft:block.note_block.trumpet_weathered", true, false),
        ("zombie", "minecraft:block.note_block.imitate.zombie", false, true),
        ("skeleton", "minecraft:block.note_block.imitate.skeleton", false, true),
        ("creeper", "minecraft:block.note_block.imitate.creeper", false, true),
        ("dragon", "minecraft:block.note_block.imitate.ender_dragon", false, true),
        ("wither_skeleton", "minecraft:block.note_block.imitate.wither_skeleton", false, true),
        ("piglin", "minecraft:block.note_block.imitate.piglin", false, true),
        ("custom_head", "minecraft:ui.button.click", false, true),
    ]
}

fn instrument_of(s: BlockState) -> &'static (&'static str, &'static str, bool, bool) {
    let n = prop(s, "instrument");
    instruments().iter().find(|i| i.0 == n).unwrap_or(&instruments()[0])
}

pub fn play_note(g: &Game, p: [i32; 3], s: BlockState) {
    if instrument_of(s).3 || g.bs(rel(p, UP)).is_air() {
        g.block_event(p, s, 0, 0);
    }
}

pub fn note_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    let b = g.has_neighbor_signal(p);
    if b != flag(s, "powered") {
        if b {
            play_note(g, p, s);
        }
        g.set_block(p, with_bool(s, "powered", b), flags::ALL);
    }
}

pub fn note_use(g: &Game, p: [i32; 3], s: BlockState) {
    let ns = with_int(s, "note", (int(s, "note") + 1) % 25);
    g.set_block(p, ns, flags::ALL);
    play_note(g, p, ns);
}

pub fn note_trigger(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    let ins = instrument_of(s);
    let note = int(s, "note");
    let pitch = if ins.2 { 2f32.powf((note - 12) as f32 / 12.0) } else { 1.0 };
    let sound_name = if ins.0 == "custom_head" {
        g.world
            .block_entity(rel(p, UP))
            .and_then(|t| t.get("note_block_sound").and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_else(|| ins.1.to_string())
    } else {
        ins.1.to_string()
    };
    g.play_sound(&sound_name, 2, [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5], 3.0, pitch);
    if ins.2 {
        g.particles("minecraft:note", [p[0] as f64 + 0.5, p[1] as f64 + 1.2, p[2] as f64 + 0.5], 0, [note as f32 / 24.0, 0.0, 0.0], 1.0);
    }
    true
}

// ------------------------------------------------------------ TNT

pub fn tnt_check(g: &Game, p: [i32; 3], s: BlockState) {
    if g.has_neighbor_signal(p) {
        g.remove_block(p, false);
        g.prime_tnt(p, None);
    }
    let _ = s;
}

// ------------------------------------------------------------ copper bulb

pub fn bulb_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    let b = g.has_neighbor_signal(p);
    if b != flag(s, "powered") {
        let mut ns = s;
        if !flag(s, "powered") {
            ns = with_bool(ns, "lit", !flag(ns, "lit"));
            sound(g, p, if flag(ns, "lit") { "minecraft:block.copper_bulb.turn_on" } else { "minecraft:block.copper_bulb.turn_off" }, 1.0, 1.0);
        }
        g.set_block(p, with_bool(ns, "powered", b), flags::ALL);
    }
}

// ------------------------------------------------------------ bell

pub fn bell_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    let b = g.has_neighbor_signal(p);
    if b != flag(s, "powered") {
        if b {
            sound(g, p, "minecraft:block.bell.use", 2.0, 1.0);
            g.block_event(p, s, 1, facing(s) as i32);
        }
        g.set_block(p, with_bool(s, "powered", b), flags::ALL);
    }
}

// ------------------------------------------------------------ daylight detector

/// `DaylightDetectorBlock.updateSignalStrength`
pub fn daylight_update(g: &Game, p: [i32; 3], s: BlockState) {
    let sky = g.sky_light(p) as i32 - g.sky_darken();
    let mut i = sky;
    let mut f = g.sun_angle();
    if flag(s, "inverted") {
        i = 15 - i;
    } else if i > 0 {
        let target = if f < std::f32::consts::PI { 0.0 } else { std::f32::consts::TAU };
        f += (target - f) * 0.2;
        i = (i as f32 * f.cos()).round() as i32;
    }
    let i = i.clamp(0, 15);
    if int(s, "power") != i {
        g.set_block(p, with_int(s, "power", i), flags::ALL);
    }
}

pub fn daylight_use(g: &Game, p: [i32; 3], s: BlockState) {
    let ns = with_bool(s, "inverted", !flag(s, "inverted"));
    g.set_block(p, ns, flags::INVISIBLE);
    daylight_update(g, p, ns);
}

// ------------------------------------------------------------ target block

/// `TargetBlock.onProjectileHit`: signal from how close to the centre it hit
pub fn target_hit(g: &Game, p: [i32; 3], s: BlockState, hit: [f64; 3], face: u8, arrow: bool) {
    let c = [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5];
    let d = [(hit[0] - c[0]).abs(), (hit[1] - c[1]).abs(), (hit[2] - c[2]).abs()];
    let e = match axis(face) {
        0 => d[1].max(d[2]),
        1 => d[0].max(d[2]),
        _ => d[0].max(d[1]),
    };
    let i = ((15.0 * ((0.5 - e) / 0.5).clamp(0.0, 1.0)).ceil() as i32).max(1);
    g.set_block(p, with_int(s, "power", i), flags::ALL);
    g.schedule_tick(p, s, if arrow { 20 } else { 8 }, prio::NORMAL);
}

// ------------------------------------------------------------ lightning rod

pub fn lightning_rod_struck(g: &Game, p: [i32; 3], s: BlockState) {
    g.set_block(p, with_bool(s, "powered", true), flags::ALL);
    g.update_neighbors_at(rel(p, opp(facing(s))), s);
    g.schedule_tick(p, s, 8, prio::NORMAL);
}

// ------------------------------------------------------------ redstone ore

pub fn redstone_ore_interact(g: &Game, p: [i32; 3], s: BlockState) {
    if !flag(s, "lit") {
        g.set_block(p, with_bool(s, "lit", true), flags::ALL);
    }
}
