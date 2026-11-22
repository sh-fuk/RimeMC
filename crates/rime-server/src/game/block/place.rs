//! `Block.getStateForPlacement` of the blocks with behaviour, and
//! `setPlacedBy`.

use super::*;

pub struct PlaceCtx {
    /// `getHorizontalDirection`
    pub horiz: u8,
    /// `getNearestLookingDirection`
    pub look: u8,
    /// clicked face
    pub face: u8,
    /// click location (world coordinates)
    pub click: [f64; 3],
    pub sneaking: bool,
}

/// `Direction.orderedByNearest(entity)[0]`
pub fn nearest_looking(yaw: f32, pitch: f32) -> u8 {
    let (y, p) = (yaw.to_radians(), pitch.to_radians());
    let v = [-y.sin() * p.cos(), -p.sin(), y.cos() * p.cos()];
    let ax = v.iter().map(|x| x.abs()).enumerate().fold((0, -1.0f32), |b, (i, x)| if x > b.1 { (i, x) } else { b }).0;
    match ax {
        0 => {
            if v[0] > 0.0 {
                EAST
            } else {
                WEST
            }
        }
        1 => {
            if v[1] > 0.0 {
                UP
            } else {
                DOWN
            }
        }
        _ => {
            if v[2] > 0.0 {
                SOUTH
            } else {
                NORTH
            }
        }
    }
}

/// The state to place at `p`, or None when it cannot be placed.
pub fn placement(g: &Game, p: [i32; 3], s: BlockState, cx: &PlaceCtx) -> Option<BlockState> {
    let name = s.name();
    let s = match class(s) {
        Class::Door => {
            let powered = g.has_neighbor_signal(p) || g.has_neighbor_signal(rel(p, UP));
            let st = with_facing(s, cx.horiz);
            let st = with(st, "hinge", door::door_hinge(g, p, cx.horiz, cx.click));
            with_bool(with_bool(with(st, "half", "lower"), "powered", powered), "open", powered)
        }
        Class::TrapDoor => {
            let mut st = s;
            if axis(cx.face) != 1 {
                st = with_facing(st, cx.face);
                st = with(st, "half", if cx.click[1] - p[1] as f64 > 0.5 { "top" } else { "bottom" });
            } else {
                st = with_facing(st, opp(cx.horiz));
                st = with(st, "half", if cx.face == UP { "bottom" } else { "top" });
            }
            let powered = g.has_neighbor_signal(p);
            with_bool(with_bool(st, "powered", powered), "open", powered)
        }
        Class::FenceGate => door::gate_placement(g, p, s, cx.horiz),
        Class::Button | Class::Lever => {
            let (face, f) = match cx.face {
                UP => ("floor", cx.horiz),
                DOWN => ("ceiling", cx.horiz),
                d => ("wall", d),
            };
            let st = with_facing(with(s, "face", face), f);
            if !redstone::face_attached_survives(g, p, st) {
                return None;
            }
            st
        }
        Class::Wire => redstone::wire_placement(g, p, s),
        Class::Repeater => redstone::repeater_placement(g, p, with_facing(s, opp(cx.horiz))),
        Class::Comparator => with_facing(s, opp(cx.horiz)),
        Class::Observer => with_facing(s, cx.look),
        Class::Piston | Class::Dispenser | Class::Dropper => with_facing(s, opp(cx.look)),
        Class::Hopper => {
            let d = opp(cx.face);
            with_bool(with_facing(s, if axis(d) == 1 { DOWN } else { d }), "enabled", true)
        }
        Class::Crafter => {
            let d = opp(cx.look);
            let top = match d {
                DOWN => opp(cx.horiz),
                UP => cx.horiz,
                _ => UP,
            };
            let o = format!("{}_{}", DIR_NAMES[d as usize], DIR_NAMES[top as usize]);
            with_bool(with(s, "orientation", &o), "triggered", g.has_neighbor_signal(p))
        }
        Class::Chest | Class::TrappedChest => container::chest_placement(g, p, s, cx.horiz, cx.face, cx.sneaking),
        Class::Rail | Class::PoweredRail | Class::DetectorRail => rail::placement(s, cx.horiz),
        Class::TripWire => tripwire::wire_placement(g, p, s),
        Class::Lamp => with_bool(s, "lit", g.has_neighbor_signal(p)),
        Class::NoteBlock => redstone::note_instrument(g, p, s),
        Class::LightningRod => with_facing(s, cx.face),
        Class::TripWireHook => {
            if axis(cx.face) == 1 {
                return None;
            }
            with_facing(s, cx.face)
        }
        Class::Plate | Class::WeightedPlate => {
            if !redstone::plate_survives(g, p) {
                return None;
            }
            s
        }
        Class::Torch | Class::WallTorch => {
            if !g.can_survive(p, s) {
                return None;
            }
            s
        }
        _ => {
            if name.ends_with("_stairs") {
                with_facing(s, cx.horiz)
            } else if name == "end_rod" {
                with_facing(s, cx.face)
            } else if name.ends_with("_glazed_terracotta") {
                with_facing(s, opp(cx.horiz))
            } else if name == "anvil" || name == "chipped_anvil" || name == "damaged_anvil" {
                with_facing(s, cw(cx.horiz))
            } else {
                s
            }
        }
    };
    // connections towards the neighbours (fences, walls, panes, stairs…)
    let shaped = if matches!(class(s), Class::Other | Class::FenceGate) { crate::worldgen::feature::shape_update(g, s, p) } else { s };
    if shaped.is_air() {
        return None;
    }
    Some(shaped)
}

/// `Block.setPlacedBy`
pub fn placed_by(g: &Game, p: [i32; 3], s: BlockState) {
    if is_diode(s) {
        redstone::diode_placed(g, p, s);
    }
    if class(s) == Class::TripWireHook {
        tripwire::hook_placed(g, p, s);
    }
}
