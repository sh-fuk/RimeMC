//! Per-block behaviour (vanilla `Block` subclasses): neighbour and shape
//! updates, placement, scheduled ticks, signals, interaction. The class of a
//! block comes from the 26.1 class chains in `assets/26.1/block_classes.txt`.

pub mod container;
pub mod door;
pub mod piston;
pub mod place;
pub mod redstone;
pub mod tripwire;
pub mod rail;
pub mod use_block;

use std::sync::OnceLock;

use super::fluid::{self, fluid_state};
use super::level::*;
use super::{is_gravity_block, Game, TickKind};
use crate::registry::{blocks, rstate, vstate, BlockRegistry, BlockState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Other,
    Wire,
    Torch,
    WallTorch,
    Repeater,
    Comparator,
    Lamp,
    PoweredBlock,
    Observer,
    Piston,
    PistonHead,
    MovingPiston,
    Dispenser,
    Dropper,
    Hopper,
    NoteBlock,
    Tnt,
    Daylight,
    Target,
    LightningRod,
    CopperBulb,
    Door,
    TrapDoor,
    FenceGate,
    Button,
    Lever,
    Plate,
    WeightedPlate,
    Bed,
    Bell,
    RedstoneOre,
    TripWireHook,
    TripWire,
    Rail,
    PoweredRail,
    DetectorRail,
    Chest,
    TrappedChest,
    Crafter,
    Sculk,
}

const BY_CLASS: [(&str, Class); 38] = [
    ("PistonHeadBlock", Class::PistonHead),
    ("MovingPistonBlock", Class::MovingPiston),
    ("PistonBaseBlock", Class::Piston),
    ("RedStoneWireBlock", Class::Wire),
    ("RedstoneWallTorchBlock", Class::WallTorch),
    ("RedstoneTorchBlock", Class::Torch),
    ("RepeaterBlock", Class::Repeater),
    ("ComparatorBlock", Class::Comparator),
    ("RedstoneLampBlock", Class::Lamp),
    ("PoweredBlock", Class::PoweredBlock),
    ("ObserverBlock", Class::Observer),
    ("DropperBlock", Class::Dropper),
    ("DispenserBlock", Class::Dispenser),
    ("HopperBlock", Class::Hopper),
    ("NoteBlock", Class::NoteBlock),
    ("TntBlock", Class::Tnt),
    ("DaylightDetectorBlock", Class::Daylight),
    ("TargetBlock", Class::Target),
    ("LightningRodBlock", Class::LightningRod),
    ("CopperBulbBlock", Class::CopperBulb),
    ("DoorBlock", Class::Door),
    ("TrapDoorBlock", Class::TrapDoor),
    ("FenceGateBlock", Class::FenceGate),
    ("ButtonBlock", Class::Button),
    ("LeverBlock", Class::Lever),
    ("WeightedPressurePlateBlock", Class::WeightedPlate),
    ("PressurePlateBlock", Class::Plate),
    ("BedBlock", Class::Bed),
    ("BellBlock", Class::Bell),
    ("RedStoneOreBlock", Class::RedstoneOre),
    ("TripWireHookBlock", Class::TripWireHook),
    ("TripWireBlock", Class::TripWire),
    ("PoweredRailBlock", Class::PoweredRail),
    ("DetectorRailBlock", Class::DetectorRail),
    ("RailBlock", Class::Rail),
    ("TrappedChestBlock", Class::TrappedChest),
    ("ChestBlock", Class::Chest),
    ("CrafterBlock", Class::Crafter),
];

fn classes() -> &'static Vec<Class> {
    static C: OnceLock<Vec<Class>> = OnceLock::new();
    C.get_or_init(|| {
        let mut by_name = std::collections::HashMap::new();
        for line in include_str!("../../../assets/26.1/block_classes.txt").lines() {
            let mut it = line.split_whitespace();
            let Some(name) = it.next() else { continue };
            let chain: Vec<&str> = it.collect();
            let c = BY_CLASS.iter().find(|(k, _)| chain.contains(k)).map(|(_, c)| *c).unwrap_or(Class::Other);
            by_name.insert(name.to_string(), c);
        }
        let reg = BlockRegistry::get();
        (0..reg.state_count()).map(|i| *by_name.get(BlockState(i as u16).name()).unwrap_or(&Class::Other)).collect()
    })
}

#[inline]
pub fn class(s: BlockState) -> Class {
    classes().get(s.0 as usize).copied().unwrap_or(Class::Other)
}

/// the chain of vanilla classes of a block (for `instanceof` checks)
pub fn has_class(s: BlockState, cls: &str) -> bool {
    static C: OnceLock<std::collections::HashMap<String, Vec<String>>> = OnceLock::new();
    let m = C.get_or_init(|| {
        include_str!("../../../assets/26.1/block_classes.txt")
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.to_string(), it.map(str::to_string).collect()))
            })
            .collect()
    });
    m.get(s.name()).is_some_and(|v| v.iter().any(|c| c == cls))
}

pub fn is_diode(s: BlockState) -> bool {
    matches!(class(s), Class::Repeater | Class::Comparator)
}

// ------------------------------------------------------------ state helpers

#[inline]
pub fn prop(s: BlockState, k: &str) -> &'static str {
    s.prop(k).unwrap_or("")
}

#[inline]
pub fn flag(s: BlockState, k: &str) -> bool {
    s.prop(k) == Some("true")
}

#[inline]
pub fn int(s: BlockState, k: &str) -> i32 {
    s.prop(k).and_then(|v| v.parse().ok()).unwrap_or(0)
}

#[inline]
pub fn with(s: BlockState, k: &str, v: &str) -> BlockState {
    BlockRegistry::get().with(s, k, v).unwrap_or(s)
}

#[inline]
pub fn with_bool(s: BlockState, k: &str, v: bool) -> BlockState {
    with(s, k, if v { "true" } else { "false" })
}

#[inline]
pub fn with_int(s: BlockState, k: &str, v: i32) -> BlockState {
    with(s, k, &v.to_string())
}

pub fn facing(s: BlockState) -> u8 {
    dir_by_name(prop(s, "facing")).unwrap_or(NORTH)
}

pub fn with_facing(s: BlockState, d: u8) -> BlockState {
    with(s, "facing", DIR_NAMES[d as usize])
}

/// `isFaceSturdy(dir, FULL)` of the block at `p`
pub fn sturdy(g: &Game, p: [i32; 3], dir: u8) -> bool {
    vstate::sturdy(g.bs(p), dir)
}

/// `Block.canSupportRigidBlock` (approximated by a full top face)
pub fn can_support_rigid(g: &Game, p: [i32; 3]) -> bool {
    let s = g.bs(p);
    vstate::sturdy(s, UP) || vstate::sturdy_up_center(s) && vstate::full_up_face(s)
}

/// `Block.canSupportCenter(level, pos, dir)`
pub fn can_support_center(g: &Game, p: [i32; 3], dir: u8) -> bool {
    let s = g.bs(p);
    if dir == UP {
        vstate::sturdy_up_center(s)
    } else {
        vstate::sturdy(s, dir)
    }
}

pub fn sound(g: &Game, p: [i32; 3], name: &str, volume: f32, pitch: f32) {
    g.play_sound(name, 1, [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5], volume, pitch);
}

// ------------------------------------------------------------ dispatch

/// `BlockBehaviour.neighborChanged`
pub fn neighbor_changed(g: &Game, p: [i32; 3], s: BlockState, src: BlockState, moved: bool) {
    // fluids (LiquidBlock.neighborChanged: shouldSpreadLiquid)
    if let Some(f) = fluid_state(s).fluid() {
        if BlockRegistry::get().is_fluid(s) {
            if fluid::lava_interaction(&mut super::GW(g), p) {
                g.schedule(p, TickKind::Fluid, fluid::tick_delay(f));
            }
            return;
        }
    }
    match class(s) {
        Class::Wire => redstone::wire_neighbor_changed(g, p, s),
        Class::Torch | Class::WallTorch => redstone::torch_neighbor_changed(g, p, s),
        Class::Repeater | Class::Comparator => redstone::diode_neighbor_changed(g, p, s),
        Class::Lamp => redstone::lamp_neighbor_changed(g, p, s),
        Class::Piston => piston::neighbor_changed(g, p, s),
        Class::Dispenser | Class::Dropper => container::dispenser_neighbor_changed(g, p, s),
        Class::Hopper => container::hopper_neighbor_changed(g, p, s),
        Class::NoteBlock => redstone::note_neighbor_changed(g, p, s),
        Class::Tnt => redstone::tnt_check(g, p, s),
        Class::CopperBulb => redstone::bulb_neighbor_changed(g, p, s),
        Class::Door => door::door_neighbor_changed(g, p, s, src),
        Class::TrapDoor => door::trapdoor_neighbor_changed(g, p, s),
        Class::FenceGate => door::gate_neighbor_changed(g, p, s),
        Class::Bell => redstone::bell_neighbor_changed(g, p, s),
        Class::PoweredRail | Class::Rail | Class::DetectorRail => rail::neighbor_changed(g, p, s, src),
        Class::Crafter => container::crafter_neighbor_changed(g, p, s),
        _ => {}
    }
    let _ = moved;
}

/// `BlockBehaviour.updateShape` (plus the generic support / fluid / gravity rules)
pub fn update_shape(g: &Game, p: [i32; 3], s: BlockState, dir: u8, np: [i32; 3], ns: BlockState) -> BlockState {
    if s.is_air() {
        return s;
    }
    // LiquidBlock / waterlogged blocks: schedule the fluid
    if let Some(f) = fluid_state(s).fluid() {
        if BlockRegistry::get().is_fluid(s) {
            if fluid_state(s).is_source() || fluid_state(ns).is_source() || true {
                g.schedule(p, TickKind::Fluid, fluid::tick_delay(f));
            }
            return s;
        }
        g.schedule(p, TickKind::Fluid, fluid::tick_delay(f));
    }
    if is_gravity_block(s.name()) {
        g.schedule(p, TickKind::Gravity, 2);
    }
    let c = class(s);
    let out = match c {
        Class::Wire => return redstone::wire_update_shape(g, p, s, dir, np, ns),
        Class::Repeater | Class::Comparator => redstone::diode_update_shape(g, p, s, dir, np, ns),
        Class::Torch | Class::WallTorch => {
            if !torch_survives(g, p, s) {
                return blocks::AIR;
            }
            s
        }
        Class::Observer => redstone::observer_update_shape(g, p, s, dir),
        Class::NoteBlock => {
            if axis(dir) == 1 {
                redstone::note_instrument(g, p, s)
            } else {
                s
            }
        }
        Class::Door => return door::door_update_shape(g, p, s, dir, ns),
        Class::FenceGate => door::gate_update_shape(g, p, s, dir, ns),
        Class::Button | Class::Lever => {
            if opp(redstone::attached_dir(s)) == dir && !redstone::face_attached_survives(g, p, s) {
                return blocks::AIR;
            }
            s
        }
        Class::Plate | Class::WeightedPlate => {
            if dir == DOWN && !redstone::plate_survives(g, p) {
                return blocks::AIR;
            }
            s
        }
        Class::Bed => return door::bed_update_shape(g, p, s, dir, ns),
        Class::PistonHead => return piston::head_update_shape(g, p, s, dir, ns),
        Class::Chest | Class::TrappedChest => container::chest_update_shape(g, p, s, dir, ns),
        Class::Rail | Class::PoweredRail | Class::DetectorRail => {
            if dir == DOWN && !can_support_rigid(g, rel(p, DOWN)) {
                return blocks::AIR;
            }
            s
        }
        Class::TripWireHook => {
            if opp(facing(s)) == dir && !sturdy(g, rel(p, opp(facing(s))), facing(s)) {
                return blocks::AIR;
            }
            s
        }
        Class::TripWire => return tripwire::wire_update_shape(s, dir, ns),
        _ if s.name() == "nether_portal" => return crate::game::portal::portal_update_shape(g, p, s, dir, ns),
        _ => s,
    };
    // fences, panes, walls, stairs, ladders…
    let out = crate::worldgen::feature::update_shape(g, out, p, dir, ns);
    if out.is_air() {
        return out;
    }
    if handles_own_survival(c) {
        return out;
    }
    if !g.can_survive(p, out) {
        return blocks::AIR;
    }
    out
}

fn handles_own_survival(c: Class) -> bool {
    !matches!(c, Class::Other | Class::Chest | Class::TrappedChest | Class::Hopper | Class::NoteBlock)
}

fn torch_survives(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    if class(s) == Class::WallTorch {
        let f = facing(s);
        sturdy(g, rel(p, opp(f)), f)
    } else {
        can_support_center(g, rel(p, DOWN), UP)
    }
}

/// `BlockBehaviour.onPlace`
pub fn on_place(g: &Game, p: [i32; 3], s: BlockState, old: BlockState, moved: bool) {
    let name = s.name();
    if matches!(name, "fire" | "soul_fire") && !same_block(old, s) {
        // BaseFireBlock.onPlace: an empty obsidian frame becomes a portal
        if crate::game::portal::fire_placed(g, p) {
            return;
        }
        // FireBlock.onPlace
        g.schedule(p, TickKind::Fire, 30 + rand::Rng::gen_range(&mut rand::thread_rng(), 0..10));
    }
    if let Some(f) = fluid_state(s).fluid() {
        if fluid::lava_interaction(&mut super::GW(g), p) {
            g.schedule(p, TickKind::Fluid, fluid::tick_delay(f));
        }
    }
    if !same_block(old, s) {
        container::ticker_placed(g, p, s);
    }
    if is_gravity_block(name) {
        g.schedule(p, TickKind::Gravity, 2);
    }
    let changed = !same_block(old, s);
    match class(s) {
        Class::Wire => redstone::wire_on_place(g, p, s, old),
        Class::TripWire => tripwire::wire_on_place(g, p, s, old),
        // RedstoneTorchBlock / DiodeBlock.onPlace: on every state change
        Class::Torch | Class::WallTorch => redstone::torch_notify(g, p, s),
        Class::Repeater | Class::Comparator => redstone::diode_update_front(g, p, s),
        Class::Observer => redstone::observer_on_place(g, p, s, old),
        Class::Piston => {
            if changed {
                piston::check_if_extend(g, p, s);
            }
        }
        Class::Tnt => {
            if changed {
                redstone::tnt_check(g, p, s);
            }
        }
        Class::Hopper => {
            if changed {
                container::hopper_check_powered(g, p, s);
            }
        }
        Class::CopperBulb => {
            if changed {
                redstone::bulb_neighbor_changed(g, p, s);
            }
        }
        Class::PoweredRail | Class::Rail | Class::DetectorRail => {
            if changed {
                rail::on_place(g, p, s);
            }
        }
        _ => {}
    }
    let _ = moved;
}

/// `BlockBehaviour.affectNeighborsAfterRemoval`
pub fn after_removal(g: &Game, p: [i32; 3], old: BlockState, moved: bool) {
    match class(old) {
        Class::Wire => {
            if !moved {
                redstone::wire_after_removal(g, p, old);
            }
        }
        Class::Torch | Class::WallTorch => {
            if !moved {
                redstone::torch_notify(g, p, old);
            }
        }
        Class::Repeater | Class::Comparator => {
            if !moved {
                redstone::diode_update_front(g, p, old);
            }
        }
        Class::Observer => {
            if !moved && flag(old, "powered") && g.has_scheduled(p, old) {
                redstone::observer_update_front(g, p, with_bool(old, "powered", false));
            }
        }
        Class::Button | Class::Lever => {
            if !moved && flag(old, "powered") {
                redstone::face_attached_update(g, p, old);
            }
        }
        Class::Plate | Class::WeightedPlate => {
            if !moved && redstone::plate_signal(old) > 0 {
                redstone::plate_update(g, p, old);
            }
        }
        Class::PoweredBlock | Class::Daylight | Class::Target | Class::LightningRod | Class::TrappedChest | Class::DetectorRail => {
            if !moved {
                g.update_neighbors_at(p, old);
                if matches!(class(old), Class::LightningRod | Class::DetectorRail) {
                    g.update_neighbors_at(rel(p, DOWN), old);
                }
            }
        }
        Class::PoweredRail | Class::Rail => {
            if !moved {
                rail::after_removal(g, p, old);
            }
        }
        Class::PistonHead => piston::head_removed(g, p, old),
        Class::TripWireHook => {
            if !moved {
                tripwire::hook_removed(g, p, old);
            }
        }
        Class::TripWire => {
            if !moved {
                tripwire::wire_removed(g, p, old);
            }
        }
        Class::Piston => {}
        _ => {}
    }
    if rstate::analog(old) {
        g.update_neighbour_for_output_signal(p, old);
    }
}

/// `BlockBehaviour.tick` (scheduled)
pub fn tick(g: &Game, p: [i32; 3], s: BlockState) {
    match class(s) {
        Class::Wire => {}
        Class::Torch | Class::WallTorch => redstone::torch_tick(g, p, s),
        Class::Repeater => redstone::repeater_tick(g, p, s),
        Class::Comparator => redstone::comparator_refresh(g, p, s),
        Class::Lamp => {
            if flag(s, "lit") && !g.has_neighbor_signal(p) {
                g.set_block(p, with_bool(s, "lit", false), flags::CLIENTS);
            }
        }
        Class::Observer => redstone::observer_tick(g, p, s),
        Class::Dispenser | Class::Dropper => container::dispense(g, p, s),
        Class::Button => redstone::button_tick(g, p, s),
        Class::Plate | Class::WeightedPlate => {
            let i = redstone::plate_signal(s);
            if i > 0 {
                redstone::plate_check(g, p, s, i);
            }
        }
        Class::Target | Class::LightningRod => {
            if class(s) == Class::Target {
                if int(s, "power") != 0 {
                    g.set_block(p, with_int(s, "power", 0), flags::ALL);
                }
            } else if flag(s, "powered") {
                g.set_block(p, with_bool(s, "powered", false), flags::ALL);
                g.update_neighbors_at(rel(p, opp(facing(s))), s);
            }
        }
        Class::PistonHead | Class::MovingPiston => piston::finish_moving(g, p, s),
        Class::DetectorRail => rail::detector_check(g, p, s),
        Class::TripWireHook => tripwire::hook_tick(g, p, s),
        Class::TripWire => tripwire::wire_tick(g, p, s),
        Class::Crafter => container::crafter_tick(g, p, s),
        Class::RedstoneOre => {
            if flag(s, "lit") {
                g.set_block(p, with_bool(s, "lit", false), flags::ALL);
            }
        }
        _ => {}
    }
}

/// `BlockBehaviour.getSignal`
pub fn signal(g: &Game, p: [i32; 3], s: BlockState, dir: u8) -> u8 {
    if !rstate::signal_source(s) {
        return 0;
    }
    match class(s) {
        Class::Wire => redstone::wire_signal(g, p, s, dir),
        Class::Torch => {
            if flag(s, "lit") && dir != UP {
                15
            } else {
                0
            }
        }
        Class::WallTorch => {
            if flag(s, "lit") && facing(s) != dir {
                15
            } else {
                0
            }
        }
        Class::Repeater | Class::Comparator => redstone::diode_signal(g, p, s, dir),
        Class::PoweredBlock => 15,
        Class::Observer => {
            if flag(s, "powered") && facing(s) == dir {
                15
            } else {
                0
            }
        }
        Class::Button | Class::Lever => {
            if flag(s, "powered") {
                15
            } else {
                0
            }
        }
        Class::Plate | Class::WeightedPlate => redstone::plate_signal(s),
        Class::Daylight | Class::Target => int(s, "power") as u8,
        Class::LightningRod => {
            if flag(s, "powered") {
                15
            } else {
                0
            }
        }
        Class::TrappedChest => container::trapped_chest_signal(g, p),
        Class::DetectorRail => {
            if flag(s, "powered") {
                15
            } else {
                0
            }
        }
        Class::TripWireHook => {
            if flag(s, "powered") {
                15
            } else {
                0
            }
        }
        Class::Sculk => 0,
        _ => 0,
    }
}

/// `BlockBehaviour.getDirectSignal`
pub fn direct_signal(g: &Game, p: [i32; 3], s: BlockState, dir: u8) -> u8 {
    if !rstate::signal_source(s) {
        return 0;
    }
    match class(s) {
        Class::Wire => {
            if g.lv(|l| l.wire_silent) {
                0
            } else {
                redstone::wire_signal(g, p, s, dir)
            }
        }
        Class::Torch | Class::WallTorch => {
            if dir == DOWN {
                signal(g, p, s, dir)
            } else {
                0
            }
        }
        Class::Repeater | Class::Comparator | Class::Observer => signal(g, p, s, dir),
        Class::Button | Class::Lever => {
            if flag(s, "powered") && redstone::attached_dir(s) == dir {
                15
            } else {
                0
            }
        }
        Class::Plate | Class::WeightedPlate => {
            if dir == UP {
                redstone::plate_signal(s)
            } else {
                0
            }
        }
        Class::LightningRod => {
            if flag(s, "powered") && facing(s) == dir {
                15
            } else {
                0
            }
        }
        Class::TrappedChest => {
            if dir == UP {
                container::trapped_chest_signal(g, p)
            } else {
                0
            }
        }
        Class::DetectorRail => {
            if flag(s, "powered") && dir == UP {
                15
            } else {
                0
            }
        }
        Class::TripWireHook => {
            if flag(s, "powered") && facing(s) == dir {
                15
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// `BlockBehaviour.getAnalogOutputSignal`; `dir` is the side read from
pub fn analog(g: &Game, p: [i32; 3], s: BlockState, dir: u8) -> u8 {
    let _ = dir;
    let name = s.name();
    match class(s) {
        Class::CopperBulb => {
            if flag(s, "lit") {
                15
            } else {
                0
            }
        }
        Class::Chest | Class::TrappedChest => container::chest_analog(g, p, s),
        Class::DetectorRail => container::detector_rail_analog(g, p, s),
        Class::Crafter => container::crafter_analog(g, p),
        _ => match name {
            "cake" => ((7 - int(s, "bites")) * 2) as u8,
            "composter" => int(s, "level") as u8,
            "water_cauldron" | "powder_snow_cauldron" => int(s, "level") as u8,
            "lava_cauldron" => 3,
            "end_portal_frame" => {
                if flag(s, "eye") {
                    15
                } else {
                    0
                }
            }
            "respawn_anchor" => ((15 * int(s, "charges")) / 4) as u8,
            "beehive" | "bee_nest" => int(s, "honey_level") as u8,
            "jukebox" => container::jukebox_analog(g, p),
            "lectern" => container::lectern_analog(g, p, s),
            "chiseled_bookshelf" => container::bookshelf_analog(g, p),
            _ => container::container_analog(g, p),
        },
    }
}

/// `BlockBehaviour.triggerEvent` (block events)
pub fn trigger_event(g: &Game, p: [i32; 3], s: BlockState, b0: i32, b1: i32) -> bool {
    match class(s) {
        Class::NoteBlock => redstone::note_trigger(g, p, s),
        Class::Piston => piston::trigger_event(g, p, s, b0, b1),
        Class::Bell => true,
        _ => true,
    }
}

/// `BlockBehaviour.updateIndirectNeighbourShapes` (redstone wire)
pub fn indirect_shapes(g: &Game, p: [i32; 3], s: BlockState, f: u32, limit: i32) {
    if class(s) == Class::Wire {
        redstone::wire_indirect_shapes(g, p, s, f, limit);
    }
}
