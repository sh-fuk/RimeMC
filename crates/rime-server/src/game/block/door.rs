//! `DoorBlock`, `TrapDoorBlock`, `FenceGateBlock` and the shape rules of
//! `BedBlock`.

use super::*;

/// `BlockSetType.canOpenByHand`
pub fn openable_by_hand(s: BlockState) -> bool {
    !matches!(s.name(), "iron_door" | "iron_trapdoor")
}

fn set_type(s: BlockState) -> &'static str {
    let n = s.name();
    if n.contains("copper") {
        "copper"
    } else if n.starts_with("iron") {
        "iron"
    } else if n.starts_with("crimson") || n.starts_with("warped") {
        "nether_wood"
    } else if n.starts_with("bamboo") {
        "bamboo_wood"
    } else if n.starts_with("cherry") {
        "cherry_wood"
    } else {
        "wooden"
    }
}

fn open_sound(s: BlockState, kind: &str, open: bool) -> String {
    let t = set_type(s);
    let base = match (t, kind) {
        ("wooden", "door") => "minecraft:block.wooden_door".to_string(),
        ("wooden", "trapdoor") => "minecraft:block.wooden_trapdoor".to_string(),
        ("iron", k) => format!("minecraft:block.iron_{k}"),
        ("copper", k) => format!("minecraft:block.copper_{k}"),
        (t, k) => format!("minecraft:block.{t}_{k}"),
    };
    format!("{base}.{}", if open { "open" } else { "close" })
}

fn other_half(s: BlockState) -> u8 {
    if prop(s, "half") == "lower" {
        UP
    } else {
        DOWN
    }
}

// ------------------------------------------------------------ doors

pub fn door_use(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    if !openable_by_hand(s) {
        return false;
    }
    let ns = with_bool(s, "open", !flag(s, "open"));
    g.set_block(p, ns, flags::CLIENTS | 8);
    sound(g, p, &open_sound(s, "door", flag(ns, "open")), 1.0, 0.9 + rand::random::<f32>() * 0.1);
    true
}

pub fn door_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState, src: BlockState) {
    let b = g.has_neighbor_signal(p) || g.has_neighbor_signal(rel(p, other_half(s)));
    if !same_block(s, src) && b != flag(s, "powered") {
        if b != flag(s, "open") {
            sound(g, p, &open_sound(s, "door", b), 1.0, 0.9 + rand::random::<f32>() * 0.1);
        }
        g.set_block(p, with_bool(with_bool(s, "powered", b), "open", b), flags::CLIENTS);
    }
}

pub fn door_update_shape(g: &Game, p: [i32; 3], s: BlockState, dir: u8, ns: BlockState) -> BlockState {
    let lower = prop(s, "half") == "lower";
    if axis(dir) == 1 && lower == (dir == UP) {
        return if class(ns) == Class::Door && prop(ns, "half") != prop(s, "half") { with(ns, "half", prop(s, "half")) } else { blocks::AIR };
    }
    if lower && dir == DOWN && !sturdy(g, rel(p, DOWN), UP) {
        return blocks::AIR;
    }
    s
}

/// `DoorBlock.getHinge`
pub fn door_hinge(g: &Game, p: [i32; 3], facing_dir: u8, click: [f64; 3]) -> &'static str {
    let above = rel(p, UP);
    let l = ccw(facing_dir);
    let r = cw(facing_dir);
    let full = |q: [i32; 3]| vstate::full_block(g.bs(q));
    let i = -(full(rel(p, l)) as i32) - full(rel(above, l)) as i32 + full(rel(p, r)) as i32 + full(rel(above, r)) as i32;
    let lower_door = |q: [i32; 3]| {
        let s = g.bs(q);
        class(s) == Class::Door && prop(s, "half") == "lower"
    };
    let b1 = lower_door(rel(p, l));
    let b2 = lower_door(rel(p, r));
    if (!b1 || b2) && i <= 0 {
        if (!b2 || b1) && i >= 0 {
            let o = OFFSETS[facing_dir as usize];
            let (dx, dz) = (o[0], o[2]);
            let x = click[0] - p[0] as f64;
            let z = click[2] - p[2] as f64;
            if (dx >= 0 || z >= 0.5) && (dx <= 0 || z <= 0.5) && (dz >= 0 || x <= 0.5) && (dz <= 0 || x >= 0.5) {
                "left"
            } else {
                "right"
            }
        } else {
            "left"
        }
    } else {
        "right"
    }
}

// ------------------------------------------------------------ trapdoors

pub fn trapdoor_use(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    if !openable_by_hand(s) {
        return false;
    }
    let ns = with_bool(s, "open", !flag(s, "open"));
    g.set_block(p, ns, flags::CLIENTS);
    if flag(ns, "waterlogged") {
        g.schedule(p, TickKind::Fluid, 5);
    }
    sound(g, p, &open_sound(s, "trapdoor", flag(ns, "open")), 1.0, 0.9 + rand::random::<f32>() * 0.1);
    true
}

pub fn trapdoor_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    let b = g.has_neighbor_signal(p);
    if b != flag(s, "powered") {
        let mut ns = s;
        if flag(s, "open") != b {
            ns = with_bool(ns, "open", b);
            sound(g, p, &open_sound(s, "trapdoor", b), 1.0, 0.9 + rand::random::<f32>() * 0.1);
        }
        g.set_block(p, with_bool(ns, "powered", b), flags::CLIENTS);
        if flag(ns, "waterlogged") {
            g.schedule(p, TickKind::Fluid, 5);
        }
    }
}

// ------------------------------------------------------------ fence gates

pub fn gate_use(g: &Game, p: [i32; 3], s: BlockState, player_dir: u8) {
    let ns = if flag(s, "open") {
        with_bool(s, "open", false)
    } else {
        let mut x = s;
        if facing(s) == opp(player_dir) {
            x = with_facing(x, player_dir);
        }
        with_bool(x, "open", true)
    };
    g.set_block(p, ns, flags::CLIENTS | 8);
    let t = set_type(s);
    let base = if t == "wooden" { "minecraft:block.fence_gate".to_string() } else { format!("minecraft:block.{t}_fence_gate") };
    sound(g, p, &format!("{base}.{}", if flag(ns, "open") { "open" } else { "close" }), 1.0, 0.9 + rand::random::<f32>() * 0.1);
}

pub fn gate_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    let b = g.has_neighbor_signal(p);
    if flag(s, "powered") != b {
        g.set_block(p, with_bool(with_bool(s, "powered", b), "open", b), flags::CLIENTS);
        if flag(s, "open") != b {
            let t = set_type(s);
            let base = if t == "wooden" { "minecraft:block.fence_gate".to_string() } else { format!("minecraft:block.{t}_fence_gate") };
            sound(g, p, &format!("{base}.{}", if b { "open" } else { "close" }), 1.0, 0.9 + rand::random::<f32>() * 0.1);
        }
    }
}

fn is_wall(s: BlockState) -> bool {
    s.name().ends_with("_wall") && !s.name().ends_with("_sign") && !s.name().contains("wall_")
}

pub fn gate_update_shape(g: &Game, p: [i32; 3], s: BlockState, dir: u8, ns: BlockState) -> BlockState {
    if axis(dir) == 1 || axis(cw(facing(s))) != axis(dir) {
        return s;
    }
    let b = is_wall(ns) || is_wall(g.bs(rel(p, opp(dir))));
    with_bool(s, "in_wall", b)
}

pub fn gate_placement(g: &Game, p: [i32; 3], s: BlockState, horiz: u8) -> BlockState {
    let powered = g.has_neighbor_signal(p);
    let x_axis = axis(horiz) == 0;
    let in_wall = if !x_axis { is_wall(g.bs(rel(p, WEST))) || is_wall(g.bs(rel(p, EAST))) } else { is_wall(g.bs(rel(p, NORTH))) || is_wall(g.bs(rel(p, SOUTH))) };
    with_bool(with_bool(with_bool(with_facing(s, horiz), "open", powered), "powered", powered), "in_wall", in_wall)
}

// ------------------------------------------------------------ beds

/// the direction from the foot to the head (`BedBlock.getNeighbourDirection`)
pub fn bed_other(s: BlockState) -> u8 {
    if prop(s, "part") == "foot" {
        facing(s)
    } else {
        opp(facing(s))
    }
}

pub fn bed_update_shape(_g: &Game, _p: [i32; 3], s: BlockState, dir: u8, ns: BlockState) -> BlockState {
    if dir == bed_other(s) {
        return if class(ns) == Class::Bed && prop(ns, "part") != prop(s, "part") { with_bool(s, "occupied", flag(ns, "occupied")) } else { blocks::AIR };
    }
    s
}
