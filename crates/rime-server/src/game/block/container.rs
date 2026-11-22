//! Block containers seen from the world: double chests, comparator readings
//! (`AbstractContainerMenu.getRedstoneSignalFromContainer`), hoppers
//! (`HopperBlockEntity`), dispensers and droppers (`DispenserBlock`,
//! `DefaultDispenseItemBehavior` and the dispense behaviours), crafters.

use rand::Rng;

use super::*;
use crate::data::{Items, Stack};
use crate::game::blockentity::{self, Screen};
use crate::anticheat::combat::Aabb;

// ------------------------------------------------------------ chests

/// `ChestBlock.getConnectedDirection`
pub fn chest_connected(s: BlockState) -> u8 {
    let f = facing(s);
    if prop(s, "type") == "left" {
        cw(f)
    } else {
        ccw(f)
    }
}

pub fn chest_update_shape(_g: &Game, _p: [i32; 3], s: BlockState, dir: u8, ns: BlockState) -> BlockState {
    if same_block(ns, s) && axis(dir) != 1 {
        let nt = prop(ns, "type");
        if prop(s, "type") == "single" && nt != "single" && facing(s) == facing(ns) && chest_connected(ns) == opp(dir) {
            return with(s, "type", if nt == "left" { "right" } else { "left" });
        }
    } else if prop(s, "type") != "single" && chest_connected(s) == dir {
        return with(s, "type", "single");
    }
    s
}

/// `ChestBlock.getStateForPlacement`
pub fn chest_placement(g: &Game, p: [i32; 3], s: BlockState, horiz: u8, clicked_face: u8, sneaking: bool) -> BlockState {
    let candidate = |d: u8| {
        let bs = g.bs(rel(p, d));
        (same_block(bs, s) && prop(bs, "type") == "single").then(|| facing(bs))
    };
    let mut ty = "single";
    let mut d = opp(horiz);
    if axis(clicked_face) != 1 && sneaking {
        if let Some(d2) = candidate(opp(clicked_face)) {
            if axis(d2) != axis(clicked_face) {
                d = d2;
                ty = if ccw(d2) == opp(clicked_face) { "right" } else { "left" };
            }
        }
    }
    if ty == "single" && !sneaking {
        if candidate(cw(d)) == Some(d) {
            ty = "left";
        } else if candidate(ccw(d)) == Some(d) {
            ty = "right";
        }
    }
    with(with_facing(s, d), "type", ty)
}

/// The block entity positions making up the container at `p`, first half first.
pub fn parts(g: &Game, p: [i32; 3]) -> Vec<[i32; 3]> {
    let s = g.bs(p);
    if matches!(class(s), Class::Chest | Class::TrappedChest) && prop(s, "type") != "single" {
        let q = rel(p, chest_connected(s));
        let o = g.bs(q);
        if same_block(o, s) && prop(o, "type") != "single" && chest_connected(o) == opp(chest_connected(s)) {
            // ChestBlock.getBlockType: RIGHT is the first half
            return if prop(s, "type") == "right" { vec![p, q] } else { vec![q, p] };
        }
    }
    vec![p]
}

/// Screen and title of the container at `p` (double chests: 6 rows).
pub fn screen_at(g: &Game, p: [i32; 3]) -> Option<([i32; 3], Screen, &'static str)> {
    let s = g.bs(p);
    let (sc, title) = blockentity::screen_of(s.name())?;
    let ps = parts(g, p);
    if ps.len() == 2 {
        return Some((ps[0], Screen::Rows(6), "Large Chest"));
    }
    Some((p, sc, title))
}

fn part_size(g: &Game, p: [i32; 3]) -> usize {
    blockentity::screen_of(g.bs(p).name()).map(|(sc, _)| sc.size()).unwrap_or(0)
}

impl Game {
    /// Contents of the container at `p` (both halves of a double chest).
    pub fn container_slots(&self, p: [i32; 3]) -> Option<Vec<Option<Stack>>> {
        let ps = parts(self, p);
        let mut out = Vec::new();
        for q in ps {
            let size = part_size(self, q);
            if size == 0 {
                return None;
            }
            let mut t = self.world.block_entity(q).unwrap_or_else(|| blockentity::new_tag(blockentity::be_id(self.bs(q).name()).unwrap_or("minecraft:chest"), q));
            if blockentity::unpack_loot(&mut t, size, &self.data) {
                self.world.set_block_entity(q, Some(t.clone()), false);
            }
            out.extend(blockentity::items(&t, size));
        }
        Some(out)
    }

    /// Store new contents (split over a double chest's halves).
    pub fn set_container_slots(&self, p: [i32; 3], slots: &[Option<Stack>], except: i32) {
        let ps = parts(self, p);
        let mut off = 0;
        for q in ps {
            let size = part_size(self, q);
            if off + size > slots.len() {
                break;
            }
            self.container_changed(q, &slots[off..off + size], except);
            off += size;
        }
    }
}

/// `getRedstoneSignalFromContainer`
fn signal_from(slots: &[Option<Stack>]) -> u8 {
    if slots.is_empty() {
        return 0;
    }
    let mut f = 0.0f32;
    for s in slots.iter().flatten() {
        f += s.count as f32 / (s.max().min(99) as f32).min(64f32.max(s.max() as f32));
    }
    f /= slots.len() as f32;
    if f <= 0.0 {
        return 0;
    }
    ((f * 14.0).floor() as u8) + 1
}

pub fn container_analog(g: &Game, p: [i32; 3]) -> u8 {
    g.container_slots(p).map(|s| signal_from(&s)).unwrap_or(0)
}

pub fn chest_analog(g: &Game, p: [i32; 3], _s: BlockState) -> u8 {
    container_analog(g, p)
}

pub fn trapped_chest_signal(g: &Game, p: [i32; 3]) -> u8 {
    let key = parts(g, p)[0];
    let n = g.players().iter().filter(|pl| matches!(pl.inv.lock().window, crate::game::inventory::Window::Container(_, _, q) if q == key)).count();
    n.min(15) as u8
}

pub fn detector_rail_analog(g: &Game, p: [i32; 3], s: BlockState) -> u8 {
    let _ = (g, p, s);
    0
}

pub fn crafter_analog(g: &Game, p: [i32; 3]) -> u8 {
    g.container_slots(p).map(|s| s.iter().filter(|x| x.is_some()).count() as u8).unwrap_or(0)
}

pub fn jukebox_analog(g: &Game, p: [i32; 3]) -> u8 {
    let Some(t) = g.world.block_entity(p) else { return 0 };
    let Some(item) = t.get("RecordItem").and_then(|r| r.get("id")).and_then(|v| v.as_str()) else { return 0 };
    let n = item.trim_start_matches("minecraft:");
    match n {
        "music_disc_13" => 1,
        "music_disc_cat" => 2,
        "music_disc_blocks" => 3,
        "music_disc_chirp" => 4,
        "music_disc_far" => 5,
        "music_disc_mall" => 6,
        "music_disc_mellohi" => 7,
        "music_disc_stal" => 8,
        "music_disc_strad" => 9,
        "music_disc_ward" => 10,
        "music_disc_11" => 11,
        "music_disc_wait" => 12,
        "music_disc_pigstep" => 13,
        "music_disc_otherside" => 14,
        "music_disc_5" => 15,
        "music_disc_relic" => 14,
        "music_disc_creator" => 12,
        "music_disc_creator_music_box" => 11,
        "music_disc_precipice" => 13,
        "music_disc_tears" => 10,
        "music_disc_lava_chicken" => 9,
        _ => 0,
    }
}

pub fn lectern_analog(g: &Game, p: [i32; 3], s: BlockState) -> u8 {
    if !flag(s, "has_book") {
        return 0;
    }
    let Some(t) = g.world.block_entity(p) else { return 0 };
    let page = t.get("Page").and_then(|v| v.as_i64()).unwrap_or(0) as f32;
    let pages = t
        .get("Book")
        .and_then(|b| b.get("components"))
        .and_then(|c| c.get("minecraft:written_book_content").or(c.get("minecraft:writable_book_content")))
        .and_then(|c| c.get("pages"))
        .and_then(|l| if let crate::nbt::Tag::List(v) = l { Some(v.len()) } else { None })
        .unwrap_or(1)
        .max(1) as f32;
    let f = if pages > 1.0 { page / (pages - 1.0) } else { 1.0 };
    ((f * 14.0).floor() as u8) + if f > 0.0 || page >= 0.0 { 1 } else { 0 }
}

pub fn bookshelf_analog(g: &Game, p: [i32; 3]) -> u8 {
    g.world.block_entity(p).and_then(|t| t.get("last_interacted_slot").and_then(|v| v.as_i64())).map(|v| (v + 1) as u8).unwrap_or(0)
}

// ------------------------------------------------------------ sided access

/// slots reachable from `face` (`WorldlyContainer.getSlotsForFace`)
fn face_slots(g: &Game, p: [i32; 3], face: Option<u8>, size: usize) -> Vec<usize> {
    let name = g.bs(p).name();
    match (name, face) {
        ("furnace" | "blast_furnace" | "smoker", Some(DOWN)) => vec![2, 1],
        ("furnace" | "blast_furnace" | "smoker", Some(UP)) => vec![0],
        ("furnace" | "blast_furnace" | "smoker", Some(_)) => vec![1],
        ("brewing_stand", Some(UP)) => vec![3],
        ("brewing_stand", Some(DOWN)) => vec![0, 1, 2, 3],
        ("brewing_stand", Some(_)) => vec![0, 1, 2, 4],
        _ => (0..size).collect(),
    }
}

fn can_place(g: &Game, p: [i32; 3], st: &Stack, slot: usize, face: Option<u8>, slots: &[Option<Stack>]) -> bool {
    let s = g.bs(p);
    let name = s.name();
    let item = Items::get().name(st.item);
    if name.ends_with("shulker_box") && item.ends_with("shulker_box") {
        return false;
    }
    match name {
        "furnace" | "blast_furnace" | "smoker" => match slot {
            2 => false,
            1 => blockentity::fuel(item, &g.data) > 0 || item == "bucket" && slots[1].is_none_or(|c| Items::get().name(c.item) != "bucket"),
            _ => true,
        },
        "brewing_stand" => match slot {
            3 => is_brewing_ingredient(item),
            4 => item == "blaze_powder",
            _ => matches!(item, "potion" | "splash_potion" | "lingering_potion" | "glass_bottle") && slots[slot].is_none(),
        },
        "crafter" => !crafter_slot_disabled(g, p, slot),
        _ => {
            let _ = face;
            true
        }
    }
}

fn can_take(g: &Game, p: [i32; 3], st: &Stack, slot: usize, face: u8) -> bool {
    let name = g.bs(p).name();
    let item = Items::get().name(st.item);
    match name {
        "furnace" | "blast_furnace" | "smoker" => !(face == DOWN && slot == 1) || matches!(item, "water_bucket" | "bucket"),
        "brewing_stand" => slot != 3 || item == "glass_bottle",
        _ => true,
    }
}

/// `HopperBlockEntity.addItem(source, destination, stack, face)` on a slot
/// list; returns the remainder and whether anything moved.
fn add_item(g: &Game, dp: [i32; 3], dest: &mut [Option<Stack>], mut st: Stack, face: Option<u8>) -> (Option<Stack>, bool) {
    let mut moved = false;
    for slot in face_slots(g, dp, face, dest.len()) {
        if st.count == 0 {
            break;
        }
        if slot >= dest.len() || !can_place(g, dp, &st, slot, face, dest) {
            continue;
        }
        match &mut dest[slot] {
            None => {
                dest[slot] = Some(st);
                st.count = 0;
                moved = true;
            }
            Some(cur) => {
                if cur.same(&st) && cur.count < cur.max() {
                    let n = (cur.max() - cur.count).min(st.count);
                    cur.count += n;
                    st.count -= n;
                    moved |= n > 0;
                }
            }
        }
    }
    ((st.count > 0).then_some(st), moved)
}

/// A container at a block position (blocks only; minecart containers are
/// entities and not handled here).
fn container_pos(g: &Game, p: [i32; 3]) -> Option<[i32; 3]> {
    let s = g.bs(p);
    blockentity::screen_of(s.name()).map(|_| p).or_else(|| (s.name() == "brewing_stand").then_some(p))
}

// ------------------------------------------------------------ hoppers

#[derive(Clone, Copy, Default, Debug)]
pub struct HopperState {
    pub cooldown: i32,
    pub ticked: u64,
}

pub fn hopper_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    hopper_check_powered(g, p, s);
}

pub fn hopper_check_powered(g: &Game, p: [i32; 3], s: BlockState) {
    let enabled = !g.has_neighbor_signal(p);
    if enabled != flag(s, "enabled") {
        g.set_block(p, with_bool(s, "enabled", enabled), flags::CLIENTS);
    }
}

/// `HopperBlockEntity.pushItemsTick`
pub fn hopper_tick(g: &Game, p: [i32; 3]) {
    let s = g.bs(p);
    if s.name() != "hopper" {
        return;
    }
    let now = g.game_time();
    let mut st = g.lv(|l| l.hoppers.get(&p).copied().unwrap_or_default());
    st.cooldown -= 1;
    st.ticked = now;
    g.lv(|l| {
        l.hoppers.insert(p, st);
    });
    if st.cooldown > 0 {
        return;
    }
    g.lv(|l| {
        if let Some(h) = l.hoppers.get_mut(&p) {
            h.cooldown = 0;
        }
    });
    if !flag(s, "enabled") {
        return;
    }
    let Some(mut mine) = g.container_slots(p) else { return };
    let mut moved = false;
    if mine.iter().any(|x| x.is_some()) {
        moved = eject(g, p, s, &mut mine);
    }
    if mine.iter().any(|x| x.is_none_or(|x| x.count < x.max())) {
        moved |= suck(g, p, &mut mine);
    }
    if moved {
        g.set_container_slots(p, &mine, -1);
        g.lv(|l| {
            if let Some(h) = l.hoppers.get_mut(&p) {
                h.cooldown = 8;
            }
        });
    }
}

fn set_cooldown_if_fresh(g: &Game, dp: [i32; 3], was_empty: bool, from_hopper: Option<[i32; 3]>) {
    if !was_empty || g.bs(dp).name() != "hopper" {
        return;
    }
    g.lv(|l| {
        let src_ticked = from_hopper.and_then(|h| l.hoppers.get(&h).map(|x| x.ticked));
        let h = l.hoppers.entry(dp).or_default();
        let k = if src_ticked.is_some_and(|t| h.ticked >= t) { 1 } else { 0 };
        h.cooldown = 8 - k;
    });
}

fn eject(g: &Game, p: [i32; 3], s: BlockState, mine: &mut [Option<Stack>]) -> bool {
    let f = facing(s);
    let dp = rel(p, f);
    let Some(dp) = container_pos(g, dp) else { return false };
    let Some(mut dest) = g.container_slots(dp) else { return false };
    let face = opp(f);
    let reach = face_slots(g, dp, Some(face), dest.len());
    if reach.iter().all(|&i| dest.get(i).is_some_and(|x| x.is_some_and(|x| x.count >= x.max()))) {
        return false;
    }
    let was_empty = dest.iter().all(|x| x.is_none());
    for i in 0..mine.len() {
        let Some(cur) = mine[i] else { continue };
        let one = Stack { count: 1, ..cur };
        let (rem, _) = add_item(g, dp, &mut dest, one, Some(face));
        if rem.is_none() {
            mine[i] = if cur.count > 1 { Some(Stack { count: cur.count - 1, ..cur }) } else { None };
            g.set_container_slots(dp, &dest, -1);
            set_cooldown_if_fresh(g, dp, was_empty, Some(p));
            return true;
        }
    }
    false
}

fn suck(g: &Game, p: [i32; 3], mine: &mut [Option<Stack>]) -> bool {
    let up = rel(p, UP);
    if let Some(sp) = container_pos(g, up) {
        let Some(mut src) = g.container_slots(sp) else { return false };
        for slot in face_slots(g, sp, Some(DOWN), src.len()) {
            let Some(cur) = src.get(slot).copied().flatten() else { continue };
            if !can_take(g, sp, &cur, slot, DOWN) {
                continue;
            }
            let one = Stack { count: 1, ..cur };
            let (rem, _) = add_item(g, p, mine, one, None);
            if rem.is_none() {
                src[slot] = if cur.count > 1 { Some(Stack { count: cur.count - 1, ..cur }) } else { None };
                g.set_container_slots(sp, &src, -1);
                return true;
            }
        }
        return false;
    }
    let above = g.bs(up);
    if vstate::full_block(above) && !g.data.block_in("does_not_block_hoppers", above.name()) {
        return false;
    }
    let area = Aabb { min: [p[0] as f64, p[1] as f64 + 11.0 / 16.0, p[2] as f64], max: [p[0] as f64 + 1.0, p[1] as f64 + 2.0, p[2] as f64 + 1.0] };
    for (eid, st) in g.items_in(&area) {
        let (rem, moved) = add_item(g, p, mine, st, None);
        if moved || rem.is_none() {
            g.set_item_entity(eid, rem);
            return true;
        }
    }
    false
}

// ------------------------------------------------------------ dispensers

pub fn dispenser_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    let b = g.has_neighbor_signal(p) || g.has_neighbor_signal(rel(p, UP));
    let t = flag(s, "triggered");
    if b && !t {
        g.schedule_tick(p, s, 4, prio::NORMAL);
        g.set_block(p, with_bool(s, "triggered", true), flags::CLIENTS);
    } else if !b && t {
        g.set_block(p, with_bool(s, "triggered", false), flags::CLIENTS);
    }
}

/// `DispenserBlockEntity.getRandomSlot`
fn random_slot(slots: &[Option<Stack>], rng: &mut impl Rng) -> Option<usize> {
    let mut i = None;
    let mut j = 1;
    for (k, s) in slots.iter().enumerate() {
        if s.is_some() {
            if rng.gen_range(0..j) == 0 {
                i = Some(k);
            }
            j += 1;
        }
    }
    i
}

/// `DispenserBlock.dispenseFrom` / `DropperBlock.dispenseFrom`
pub fn dispense(g: &Game, p: [i32; 3], s: BlockState) {
    let mut rng = rand::thread_rng();
    let Some(mut slots) = g.container_slots(p) else { return };
    let Some(slot) = random_slot(&slots, &mut rng) else {
        g.level_event(1001, p, 0);
        return;
    };
    let st = slots[slot].expect("slot");
    let d = facing(s);
    if class(s) == Class::Dropper {
        let tp = rel(p, d);
        if let Some(cp) = container_pos(g, tp) {
            if let Some(mut dest) = g.container_slots(cp) {
                let was_empty = dest.iter().all(|x| x.is_none());
                let (rem, _) = add_item(g, cp, &mut dest, Stack { count: 1, ..st }, Some(opp(d)));
                if rem.is_none() {
                    g.set_container_slots(cp, &dest, -1);
                    set_cooldown_if_fresh(g, cp, was_empty, None);
                    slots[slot] = if st.count > 1 { Some(Stack { count: st.count - 1, ..st }) } else { None };
                    g.set_container_slots(p, &slots, -1);
                }
            }
            return;
        }
        let left = default_dispense(g, p, d, st, &mut rng);
        slots[slot] = left;
        g.set_container_slots(p, &slots, -1);
        return;
    }
    let (left, extra) = g.dispense_item(p, d, st, &mut rng);
    slots[slot] = left;
    if let Some(x) = extra {
        let (rem, _) = add_item(g, p, &mut slots, x, None);
        if let Some(r) = rem {
            default_dispense(g, p, d, r, &mut rng);
        }
    }
    g.set_container_slots(p, &slots, -1);
}

/// position in front of a dispenser (`DispenserBlock.getDispensePosition`)
pub fn dispense_pos(p: [i32; 3], d: u8) -> [f64; 3] {
    let o = OFFSETS[d as usize];
    [p[0] as f64 + 0.5 + 0.7 * o[0] as f64, p[1] as f64 + 0.5 + 0.7 * o[1] as f64, p[2] as f64 + 0.5 + 0.7 * o[2] as f64]
}

fn triangle(rng: &mut impl Rng, mean: f64, dev: f64) -> f64 {
    mean + dev * (rng.r#gen::<f64>() - rng.r#gen::<f64>())
}

/// `DefaultDispenseItemBehavior.spawnItem` of one item; returns what is left
pub fn default_dispense(g: &Game, p: [i32; 3], d: u8, st: Stack, rng: &mut impl Rng) -> Option<Stack> {
    let one = Stack { count: 1, ..st };
    let o = OFFSETS[d as usize];
    let mut pos = dispense_pos(p, d);
    if axis(d) == 1 {
        pos[1] -= 0.125;
    } else {
        pos[1] -= 0.15625;
    }
    let speed = rng.r#gen::<f64>() * 0.1 + 0.2;
    let vel = [triangle(rng, o[0] as f64 * speed, 0.0172275 * 6.0), triangle(rng, 0.2, 0.0172275 * 6.0), triangle(rng, o[2] as f64 * speed, 0.0172275 * 6.0)];
    g.spawn_item(pos, one, vel, 10);
    g.level_event(1000, p, 0);
    g.level_event(2000, p, d as i32);
    (st.count > 1).then_some(Stack { count: st.count - 1, ..st })
}

// ------------------------------------------------------------ crafter

fn crafter_slot_disabled(g: &Game, p: [i32; 3], slot: usize) -> bool {
    g.world
        .block_entity(p)
        .and_then(|t| t.get("disabled_slots").cloned())
        .is_some_and(|v| match v {
            crate::nbt::Tag::IntArray(a) => a.contains(&(slot as i32)),
            _ => false,
        })
}

pub fn crafter_neighbor_changed(g: &Game, p: [i32; 3], s: BlockState) {
    let b = g.has_neighbor_signal(p);
    let t = flag(s, "triggered");
    if b && !t {
        g.schedule_tick(p, s, 4, prio::NORMAL);
    }
    if b != t {
        g.set_block(p, with_bool(s, "triggered", b), flags::INVISIBLE);
    }
}

/// `CrafterBlock.dispenseFrom`: craft once from the 3x3 grid and eject
pub fn crafter_tick(g: &Game, p: [i32; 3], s: BlockState) {
    let Some(mut slots) = g.container_slots(p) else { return };
    let grid: Vec<Option<i32>> = slots.iter().take(9).map(|x| x.map(|x| x.item)).collect();
    let remainders: Vec<Option<Stack>> = slots.iter().take(9).map(|x| x.and_then(|x| craft_remainder(Items::get().name(x.item)))).collect();
    let Some(result) = g.data.craft(&grid, 3) else {
        g.level_event(1050, p, 0);
        return;
    };
    for (i, sl) in slots.iter_mut().take(9).enumerate() {
        if let Some(x) = sl {
            if x.count > 1 {
                x.count -= 1;
            } else {
                *sl = None;
            }
        }
        if let Some(r) = remainders.get(i).copied().flatten() {
            if sl.is_none() {
                *sl = Some(r);
            }
        }
    }
    g.set_container_slots(p, &slots, -1);
    let d = prop(s, "orientation").split('_').next().and_then(dir_by_name).unwrap_or(NORTH);
    let tp = rel(p, d);
    if let Some(cp) = container_pos(g, tp) {
        if let Some(mut dest) = g.container_slots(cp) {
            let (rem, _) = add_item(g, cp, &mut dest, result, Some(opp(d)));
            g.set_container_slots(cp, &dest, -1);
            if let Some(r) = rem {
                g.spawn_item(dispense_pos(p, d), r, [0.0, 0.1, 0.0], 10);
            }
            g.level_event(1049, p, 0);
            return;
        }
    }
    let o = OFFSETS[d as usize];
    g.spawn_item(dispense_pos(p, d), result, [o[0] as f64 * 0.1, 0.1, o[2] as f64 * 0.1], 10);
    g.level_event(1049, p, 0);
    g.level_event(2010, p, d as i32);
}

fn craft_remainder(item: &str) -> Option<Stack> {
    let r = match item {
        "water_bucket" | "lava_bucket" | "milk_bucket" | "powder_snow_bucket" => "bucket",
        "honey_bottle" | "dragon_breath" => "glass_bottle",
        _ => return None,
    };
    Items::get().id(r).map(|i| Stack::new(i, 1))
}

/// `PotionBrewing.isIngredient`
pub fn is_brewing_ingredient(item: &str) -> bool {
    matches!(
        item,
        "nether_wart" | "redstone" | "glowstone_dust" | "fermented_spider_eye" | "gunpowder" | "dragon_breath" | "sugar" | "rabbit_foot" | "glistering_melon_slice" | "spider_eye"
            | "pufferfish" | "magma_cream" | "golden_carrot" | "blaze_powder" | "ghast_tear" | "turtle_helmet" | "phantom_membrane" | "breeze_rod" | "slime_block" | "stone" | "cobweb"
    )
}

// ------------------------------------------------------------ ticking block entities

/// Hoppers every tick, daylight detectors every 20 ticks (`BlockEntityTicker`);
/// the list is refreshed from the loaded chunks once a second, which also
/// finds furnaces left burning before a restart.
pub fn tick_block_entities(g: &Game) {
    let now = g.game_time();
    if now % 20 == 1 {
        let list = g.world.block_entities_with(&["hopper", "daylight_detector", "furnace", "blast_furnace", "smoker"]);
        let mut hop = Vec::new();
        let mut day = Vec::new();
        for p in list {
            let n = g.bs(p).name();
            match n {
                "hopper" => hop.push(p),
                "daylight_detector" => day.push(p),
                _ => {
                    g.furnaces.lock().insert(p);
                }
            }
        }
        g.lv(|l| {
            l.hoppers.retain(|k, _| hop.contains(k));
            for p in &hop {
                l.hoppers.entry(*p).or_default();
            }
        });
        for p in day {
            let s = g.bs(p);
            if class(s) == Class::Daylight {
                redstone::daylight_update(g, p, s);
            }
        }
    }
    let mut hops: Vec<[i32; 3]> = g.lv(|l| l.hoppers.keys().copied().collect());
    hops.sort_unstable();
    for p in hops {
        hopper_tick(g, p);
    }
}

/// a block entity that ticks was placed
pub fn ticker_placed(g: &Game, p: [i32; 3], s: BlockState) {
    match s.name() {
        "hopper" => g.lv(|l| {
            l.hoppers.entry(p).or_default();
        }),
        "furnace" | "blast_furnace" | "smoker" => {
            g.furnaces.lock().insert(p);
        }
        _ => {}
    }
}
