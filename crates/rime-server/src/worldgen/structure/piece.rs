//! `StructurePiece` helpers shared by the hand-ported (non-template) pieces:
//! local coordinates, `placeBlock`, boxes, chests and height probing.

use super::template::{bb_inside, mirror_state, rotate_state};
use super::{Bb, P};
use crate::registry::{blocks, vstate, BlockRegistry, BlockState};
use crate::worldgen::chunk::MIN_Y;
use crate::worldgen::feature::{Hm, Region};
use crate::worldgen::random::Rng;

/// Direction ordinals (vanilla `Direction`).
pub const DOWN: u8 = 0;
pub const UP: u8 = 1;
pub const NORTH: u8 = 2;
pub const SOUTH: u8 = 3;
pub const WEST: u8 = 4;
pub const EAST: u8 = 5;
/// `Direction.Plane.HORIZONTAL` order
pub const HORIZONTAL: [u8; 4] = [NORTH, EAST, SOUTH, WEST];

pub fn dir_name(d: u8) -> &'static str {
    ["down", "up", "north", "south", "west", "east"][d as usize]
}

pub fn step(d: u8) -> P {
    super::template::dir_step(d)
}

pub fn opposite(d: u8) -> u8 {
    d ^ 1
}

/// `Direction.getClockWise` (horizontal)
pub fn clockwise(d: u8) -> u8 {
    match d {
        NORTH => EAST,
        EAST => SOUTH,
        SOUTH => WEST,
        _ => NORTH,
    }
}

/// `getRandomHorizontalDirection`
pub fn random_horizontal(r: &mut dyn Rng) -> u8 {
    HORIZONTAL[r.next_int_bound(4) as usize]
}

/// `StructurePiece.makeBoundingBox`
pub fn make_bb(x: i32, y: i32, z: i32, dir: u8, w: i32, h: i32, d: i32) -> Bb {
    if dir == NORTH || dir == SOUTH {
        ([x, y, z], [x + w - 1, y + h - 1, z + d - 1])
    } else {
        ([x, y, z], [x + d - 1, y + h - 1, z + w - 1])
    }
}

/// (mirror, rotation) of `setOrientation`
pub fn orientation_transform(o: Option<u8>) -> (i32, i32) {
    match o {
        Some(SOUTH) => (1, 0),
        Some(WEST) => (1, 1),
        Some(EAST) => (0, 1),
        _ => (0, 0),
    }
}

pub fn state(name: &str, props: &[(&str, &str)]) -> BlockState {
    BlockRegistry::get().state(&format!("minecraft:{name}"), props).unwrap_or_else(|| panic!("block {name}"))
}

pub fn with(s: BlockState, k: &str, v: &str) -> BlockState {
    BlockRegistry::get().with(s, k, v).unwrap_or(s)
}

/// `StructurePiece.SHAPE_CHECK_BLOCKS`
pub fn shape_check(s: BlockState) -> bool {
    super::mineshaft::shape_check(s)
}

/// `isReplaceableByStructures`
pub fn replaceable_by_structures(s: BlockState) -> bool {
    vstate::air(s) || vstate::liquid(s) || matches!(s.name(), "glow_lichen" | "seagrass" | "tall_seagrass")
}

/// `Direction.get2DDataValue`
pub fn data2d(d: u8) -> i32 {
    match d {
        SOUTH => 0,
        WEST => 1,
        NORTH => 2,
        _ => 3,
    }
}

pub fn dir_parse_name(n: &str) -> u8 {
    ["down", "up", "north", "south", "west", "east"].iter().position(|x| *x == n).unwrap_or(2) as u8
}

/// `StructurePiece.BlockSelector`
pub trait Selector {
    fn next(&self, r: &mut dyn Rng, x: i32, y: i32, z: i32, edge: bool) -> BlockState;
}

/// `JungleTemplePiece.MossStoneSelector`
pub struct MossStone;

impl Selector for MossStone {
    fn next(&self, r: &mut dyn Rng, _x: i32, _y: i32, _z: i32, _edge: bool) -> BlockState {
        if r.next_float() < 0.4 {
            state("cobblestone", &[])
        } else {
            state("mossy_cobblestone", &[])
        }
    }
}

/// One piece being placed in one chunk box.
pub struct Sp<'a, 'b> {
    pub lv: &'a mut Region<'b>,
    /// chunk box (`postProcess` box argument)
    pub cb: Bb,
    pub bb: Bb,
    pub orient: Option<u8>,
}

/// `StructurePiece.getWorldPos` without a level
pub fn world_pos(bb: &Bb, orient: Option<u8>, x: i32, y: i32, z: i32) -> P {
    let wx = match orient {
        Some(NORTH) | Some(SOUTH) => bb.0[0] + x,
        Some(WEST) => bb.1[0] - z,
        Some(_) => bb.0[0] + z,
        None => x,
    };
    let wy = if orient.is_some() { bb.0[1] + y } else { y };
    let wz = match orient {
        Some(NORTH) => bb.1[2] - z,
        Some(SOUTH) => bb.0[2] + z,
        Some(_) => bb.0[2] + x,
        None => z,
    };
    [wx, wy, wz]
}

impl Sp<'_, '_> {
    pub fn wx(&self, x: i32, z: i32) -> i32 {
        let b = &self.bb;
        match self.orient {
            Some(NORTH) | Some(SOUTH) => b.0[0] + x,
            Some(WEST) => b.1[0] - z,
            Some(_) => b.0[0] + z,
            None => x,
        }
    }
    pub fn wy(&self, y: i32) -> i32 {
        match self.orient {
            None => y,
            Some(_) => self.bb.0[1] + y,
        }
    }
    pub fn wz(&self, x: i32, z: i32) -> i32 {
        let b = &self.bb;
        match self.orient {
            Some(NORTH) => b.1[2] - z,
            Some(SOUTH) => b.0[2] + z,
            Some(_) => b.0[2] + x,
            None => z,
        }
    }
    pub fn pos(&self, x: i32, y: i32, z: i32) -> P {
        [self.wx(x, z), self.wy(y), self.wz(x, z)]
    }
    pub fn get_block(&self, x: i32, y: i32, z: i32) -> BlockState {
        let p = self.pos(x, y, z);
        if !bb_inside(&self.cb, p) {
            return blocks::AIR;
        }
        self.lv.get(p)
    }
    pub fn is_interior(&self, x: i32, y: i32, z: i32) -> bool {
        let p = self.pos(x, y + 1, z);
        bb_inside(&self.cb, p) && p[1] < self.lv.height(Hm::Motion, p[0], p[2])
    }
    /// `placeBlock` (default `canBeReplaced`)
    pub fn place(&mut self, s: BlockState, x: i32, y: i32, z: i32) {
        let p = self.pos(x, y, z);
        if !bb_inside(&self.cb, p) {
            return;
        }
        let (m, r) = orientation_transform(self.orient);
        let s = rotate_state(mirror_state(s, m), r);
        self.lv.set(p, s);
        if vstate::has_fluid(self.lv.get(p)) {
            self.lv.schedule_tick(p);
        }
        if shape_check(s) {
            self.lv.mark_post(p);
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn gen_box(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, edge: BlockState, inner: BlockState, exclude_air: bool) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if exclude_air && vstate::air(self.get_block(x, y, z)) {
                        continue;
                    }
                    let e = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    self.place(if e { edge } else { inner }, x, y, z);
                }
            }
        }
    }
    /// `generateBox` with a block selector
    #[allow(clippy::too_many_arguments)]
    pub fn gen_box_sel(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, skip_air: bool, r: &mut dyn Rng, sel: &dyn Selector) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if !skip_air || !vstate::air(self.get_block(x, y, z)) {
                        let e = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                        let s = sel.next(r, x, y, z, e);
                        self.place(s, x, y, z);
                    }
                }
            }
        }
    }
    /// `createDispenser`
    pub fn create_dispenser(&mut self, r: &mut dyn Rng, x: i32, y: i32, z: i32, facing: u8, loot: &str) -> bool {
        let p = self.pos(x, y, z);
        if bb_inside(&self.cb, p) && self.lv.get(p).name() != "dispenser" {
            self.place(state("dispenser", &[("facing", dir_name(facing))]), x, y, z);
            if self.lv.get(p).name() == "dispenser" {
                loot_container(self.lv, p, "minecraft:dispenser", loot, r);
            }
            return true;
        }
        false
    }
    /// `generateAirBox`
    pub fn air_box(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    self.place(blocks::AIR, x, y, z);
                }
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn maybe_box(&mut self, r: &mut dyn Rng, chance: f32, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, edge: BlockState, inner: BlockState, non_air: bool, sky: bool) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if r.next_float() > chance {
                        continue;
                    }
                    if non_air && vstate::air(self.get_block(x, y, z)) {
                        continue;
                    }
                    if sky && !self.is_interior(x, y, z) {
                        continue;
                    }
                    let e = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    self.place(if e { edge } else { inner }, x, y, z);
                }
            }
        }
    }
    pub fn maybe_block(&mut self, r: &mut dyn Rng, chance: f32, x: i32, y: i32, z: i32, s: BlockState) {
        if r.next_float() < chance {
            self.place(s, x, y, z);
        }
    }
    /// `fillColumnDown`
    pub fn fill_column_down(&mut self, s: BlockState, x: i32, y: i32, z: i32) {
        let mut p = self.pos(x, y, z);
        if !bb_inside(&self.cb, p) {
            return;
        }
        while replaceable_by_structures(self.lv.get(p)) && p[1] > MIN_Y + 1 {
            self.lv.set(p, s);
            p[1] -= 1;
        }
    }
    /// `createChest(level, box, random, x, y, z, lootTable)` (local coords)
    pub fn create_chest(&mut self, r: &mut dyn Rng, x: i32, y: i32, z: i32, loot: &str) -> bool {
        let p = self.pos(x, y, z);
        create_chest(self.lv, &self.cb, r, p, None, loot)
    }
}

/// `StructurePiece.reorient` for a chest
pub fn reorient(lv: &Region, p: P, s: BlockState) -> BlockState {
    let mut dir: Option<u8> = None;
    for d in HORIZONTAL {
        let o = step(d);
        let n = lv.get([p[0] + o[0], p[1], p[2] + o[2]]);
        if n.name() == "chest" {
            return s;
        }
        if vstate::solid_render(n) {
            if dir.is_none() {
                dir = Some(d);
            } else {
                dir = None;
                break;
            }
        }
    }
    if let Some(d) = dir {
        return with(s, "facing", dir_name(opposite(d)));
    }
    let reg = BlockRegistry::get();
    let mut f = match reg.property(s, "facing").unwrap_or("north") {
        "south" => SOUTH,
        "west" => WEST,
        "east" => EAST,
        _ => NORTH,
    };
    let solid = |f: u8| {
        let o = step(f);
        vstate::solid_render(lv.get([p[0] + o[0], p[1], p[2] + o[2]]))
    };
    if solid(f) {
        f = opposite(f);
    }
    if solid(f) {
        f = clockwise(f);
    }
    if solid(f) {
        f = opposite(f);
    }
    with(s, "facing", dir_name(f))
}

/// `StructurePiece.createChest(level, box, random, pos, loot, state)`
pub fn create_chest(lv: &mut Region, cb: &Bb, r: &mut dyn Rng, p: P, s: Option<BlockState>, loot: &str) -> bool {
    if !bb_inside(cb, p) || lv.get(p).name() == "chest" {
        return false;
    }
    let s = s.unwrap_or_else(|| reorient(lv, p, state("chest", &[])));
    if lv.set(p, s) && lv.get(p).name() == "chest" {
        loot_container(lv, p, "minecraft:chest", loot, r);
    }
    true
}

/// `RandomizableContainer.setBlockEntityLootTable`: the loot table and a
/// seed drawn from the structure random.
pub fn loot_container(lv: &mut Region, p: P, id: &str, loot: &str, r: &mut dyn Rng) {
    use crate::nbt::Tag;
    let seed = r.next_long();
    let mut t = crate::game::blockentity::new_tag(id, p);
    if let Tag::Compound(c) = &mut t {
        c.push(("LootTable".into(), Tag::String(format!("minecraft:{loot}"))));
        c.push(("LootTableSeed".into(), Tag::Long(seed)));
    }
    lv.set_block_entity(p, t);
}

/// A monster spawner block entity for `mob`.
pub fn spawner_entity(lv: &mut Region, p: P, mob: &str) {
    use crate::nbt::Tag;
    let mut t = crate::game::blockentity::new_tag("minecraft:mob_spawner", p);
    if let Tag::Compound(c) = &mut t {
        let entity = Tag::Compound(vec![("id".into(), Tag::String(format!("minecraft:{mob}")))]);
        c.push(("SpawnData".into(), Tag::Compound(vec![("entity".into(), entity)])));
        c.push(("Delay".into(), Tag::Short(20)));
    }
    lv.set_block_entity(p, t);
}
