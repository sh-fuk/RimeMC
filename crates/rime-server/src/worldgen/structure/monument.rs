//! Ocean monument (`OceanMonumentPieces`): the room graph and fitters are
//! hand-ported; the `pp_*` / `m_generate_*` placement functions are generated
//! from the 26.1 bytecode by the stack-machine translator.

#![allow(clippy::all, unused_parens, unused_mut, unused_variables, unused_assignments, non_snake_case, dead_code)]

use std::sync::Arc;

use super::legacy::{LLib, LPiece};
use super::piece::{self, make_bb, state, world_pos, Sp, DOWN, EAST, NORTH, SOUTH, UP, WEST};
use super::template::{bb_from_corners, bb_intersects, bb_move};
use super::{Bb, P};
use crate::registry::{blocks, BlockState};
use crate::worldgen::feature::Region;
use crate::worldgen::random::{Legacy, Rng};

const SEA_LEVEL: i32 = 63;

fn room_index(x: i32, y: i32, z: i32) -> i32 {
    y * 25 + z * 5 + x
}

/// `OceanMonumentPieces.RoomDefinition`
#[derive(Clone, Copy, Default, Debug)]
pub struct Room {
    pub index: i32,
    pub conn: [Option<u16>; 6],
    pub open: [bool; 6],
    pub claimed: bool,
    is_source: bool,
    scan: i32,
}

impl Room {
    fn new(index: i32) -> Room {
        Room { index, ..Default::default() }
    }
    pub fn count_openings(&self) -> i32 {
        self.open.iter().filter(|o| **o).count() as i32
    }
    fn is_special(&self) -> bool {
        self.index >= 75
    }
}

struct Graph(Vec<Room>);

impl Graph {
    fn add(&mut self, index: i32) -> u16 {
        self.0.push(Room::new(index));
        (self.0.len() - 1) as u16
    }
    fn set_connection(&mut self, a: u16, d: u8, b: u16) {
        self.0[a as usize].conn[d as usize] = Some(b);
        self.0[b as usize].conn[piece::opposite(d) as usize] = Some(a);
    }
    fn update_openings(&mut self, a: u16) {
        let r = &mut self.0[a as usize];
        for i in 0..6 {
            r.open[i] = r.conn[i].is_some();
        }
    }
    fn find_source(&mut self, a: u16, scan: i32) -> bool {
        if self.0[a as usize].is_source {
            return true;
        }
        self.0[a as usize].scan = scan;
        for i in 0..6 {
            let r = self.0[a as usize];
            if let Some(c) = r.conn[i] {
                if r.open[i] && self.0[c as usize].scan != scan && self.find_source(c, scan) {
                    return true;
                }
            }
        }
        false
    }
    fn c(&self, a: u16, d: u8) -> u16 {
        self.0[a as usize].conn[d as usize].expect("monument connection")
    }
    fn claim(&mut self, a: u16) {
        self.0[a as usize].claimed = true;
    }
}

/// `Direction.values()` order with steps
const DIRS: [(u8, [i32; 3]); 6] = [(DOWN, [0, -1, 0]), (UP, [0, 1, 0]), (NORTH, [0, 0, -1]), (SOUTH, [0, 0, 1]), (WEST, [-1, 0, 0]), (EAST, [1, 0, 0])];

/// `MonumentBuilding.generateRoomGraph`: (graph, list, source, core)
fn room_graph(r: &mut dyn Rng) -> (Graph, Vec<u16>, u16, u16) {
    let mut g = Graph(Vec::new());
    let mut grid: [Option<u16>; 75] = [None; 75];
    for (y, xs, zs) in [(0, 0..5, 0..4), (1, 0..5, 0..4), (2, 1..4, 0..2)] {
        for x in xs {
            for z in zs.clone() {
                let i = room_index(x, y, z);
                grid[i as usize] = Some(g.add(i));
            }
        }
    }
    let source = grid[room_index(2, 0, 0) as usize].unwrap();
    for x in 0..5 {
        for z in 0..5 {
            for y in 0..3 {
                let Some(a) = grid[room_index(x, y, z) as usize] else { continue };
                for (d, s) in DIRS {
                    let (nx, ny, nz) = (x + s[0], y + s[1], z + s[2]);
                    if !(0..5).contains(&nx) || !(0..5).contains(&nz) || !(0..3).contains(&ny) {
                        continue;
                    }
                    let Some(b) = grid[room_index(nx, ny, nz) as usize] else { continue };
                    if nz == z {
                        g.set_connection(a, d, b);
                    } else {
                        g.set_connection(a, piece::opposite(d), b);
                    }
                }
            }
        }
    }
    let pent = g.add(1003);
    let left = g.add(1001);
    let right = g.add(1002);
    g.set_connection(grid[room_index(2, 2, 0) as usize].unwrap(), UP, pent);
    g.set_connection(grid[room_index(0, 1, 0) as usize].unwrap(), SOUTH, left);
    g.set_connection(grid[room_index(4, 1, 0) as usize].unwrap(), SOUTH, right);
    g.claim(pent);
    g.claim(left);
    g.claim(right);
    g.0[source as usize].is_source = true;
    let core = grid[room_index(r.next_int_bound(4), 0, 2) as usize].unwrap();
    g.claim(core);
    let e = g.c(core, EAST);
    let n = g.c(core, NORTH);
    let en = g.c(e, NORTH);
    let u = g.c(core, UP);
    let eu = g.c(e, UP);
    let nu = g.c(n, UP);
    let enu = g.c(en, UP);
    for a in [e, n, en, u, eu, nu, enu] {
        g.claim(a);
    }
    let mut list: Vec<u16> = Vec::new();
    for a in grid.iter().flatten() {
        g.update_openings(*a);
        list.push(*a);
    }
    g.update_openings(pent);
    super::jigsaw::shuffle(&mut list, r);
    let mut scan = 1;
    for &a in &list {
        let mut removed = 0;
        let mut tries = 0;
        while removed < 2 && tries < 5 {
            tries += 1;
            let j = r.next_int_bound(6) as usize;
            if g.0[a as usize].open[j] {
                let k = piece::opposite(j as u8) as usize;
                let c = g.0[a as usize].conn[j].expect("opening without connection");
                g.0[a as usize].open[j] = false;
                g.0[c as usize].open[k] = false;
                let s1 = scan;
                scan += 1;
                if g.find_source(a, s1) && {
                    let s2 = scan;
                    scan += 1;
                    g.find_source(c, s2)
                } {
                    removed += 1;
                } else {
                    g.0[a as usize].open[j] = true;
                    g.0[c as usize].open[k] = true;
                }
            }
        }
    }
    list.push(pent);
    list.push(left);
    list.push(right);
    (g, list, source, core)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MK {
    Core,
    DoubleX,
    DoubleXY,
    DoubleY,
    DoubleYZ,
    DoubleZ,
    Entry,
    Penthouse,
    Simple,
    SimpleTop,
    Wing,
}

/// One `OceanMonumentPiece` child of the building.
pub struct MRoom {
    kind: MK,
    bb: Bb,
    room: u16,
    g: Arc<Vec<Room>>,
    main_design: i32,
}

impl MRoom {
    fn def(&self) -> Room {
        self.g[self.room as usize]
    }
    fn conn(&self, c: Option<u16>) -> Option<Room> {
        c.map(|c| self.g[c as usize])
    }
}

/// `OceanMonumentPiece(type, depth, dir, def, w, h, d)` bounding box
fn room_bb(dir: u8, index: i32, w: i32, h: i32, d: i32) -> Bb {
    let (x, z, y) = (index % 5, index / 5 % 5, index / 25);
    let bb = make_bb(0, 0, 0, dir, w * 8, h * 4, d * 8);
    let m = match dir {
        NORTH => [x * 8, y * 4, -(z + d) * 8 + 1],
        SOUTH => [x * 8, y * 4, z * 8],
        WEST => [-(z + d) * 8 + 1, y * 4, x * 8],
        _ => [z * 8, y * 4, x * 8],
    };
    bb_move(bb, m)
}

/// `MonumentBuilding`
pub struct Monument {
    bb: Bb,
    orient: u8,
    children: Vec<MRoom>,
}

impl Monument {
    /// `OceanMonumentStructure.createTopPiece`
    pub fn new(r: &mut dyn Rng, cx: i32, cz: i32) -> Monument {
        let (x, z) = (cx * 16 - 29, cz * 16 - 29);
        let dir = piece::random_horizontal(r);
        let bb = make_bb(x, 39, z, dir, 58, 23, 58);
        let (mut g, list, source, core) = room_graph(r);
        g.claim(source);
        // (kind, room, w, h, d, main design)
        let mut rooms: Vec<(MK, u16, [i32; 3], i32)> = vec![(MK::Entry, source, [1, 1, 1], 0), (MK::Core, core, [2, 2, 2], 0)];
        for &a in &list {
            let d = g.0[a as usize];
            if d.claimed || d.is_special() {
                continue;
            }
            let o = |g: &Graph, a: u16, dir: u8| g.0[a as usize].open[dir as usize];
            let cl = |g: &Graph, a: u16, dir: u8| g.0[g.c(a, dir) as usize].claimed;
            if o(&g, a, EAST) && !cl(&g, a, EAST) && o(&g, a, UP) && !cl(&g, a, UP) && o(&g, g.c(a, EAST), UP) && !cl(&g, g.c(a, EAST), UP) {
                g.claim(a);
                let e = g.c(a, EAST);
                g.claim(e);
                g.claim(g.c(a, UP));
                g.claim(g.c(e, UP));
                rooms.push((MK::DoubleXY, a, [2, 2, 1], 0));
            } else if o(&g, a, NORTH) && !cl(&g, a, NORTH) && o(&g, a, UP) && !cl(&g, a, UP) && o(&g, g.c(a, NORTH), UP) && !cl(&g, g.c(a, NORTH), UP) {
                g.claim(a);
                let n = g.c(a, NORTH);
                g.claim(n);
                g.claim(g.c(a, UP));
                g.claim(g.c(n, UP));
                rooms.push((MK::DoubleYZ, a, [1, 2, 2], 0));
            } else if o(&g, a, NORTH) && !cl(&g, a, NORTH) {
                // FitDoubleZRoom.create re-checks (always true here)
                let mut b = a;
                if !o(&g, a, NORTH) || cl(&g, a, NORTH) {
                    b = g.c(a, SOUTH);
                }
                g.claim(b);
                g.claim(g.c(b, NORTH));
                rooms.push((MK::DoubleZ, b, [1, 1, 2], 0));
            } else if o(&g, a, EAST) && !cl(&g, a, EAST) {
                g.claim(a);
                g.claim(g.c(a, EAST));
                rooms.push((MK::DoubleX, a, [2, 1, 1], 0));
            } else if o(&g, a, UP) && !cl(&g, a, UP) {
                g.claim(a);
                g.claim(g.c(a, UP));
                rooms.push((MK::DoubleY, a, [1, 2, 1], 0));
            } else if !o(&g, a, WEST) && !o(&g, a, EAST) && !o(&g, a, NORTH) && !o(&g, a, SOUTH) && !o(&g, a, UP) {
                g.claim(a);
                rooms.push((MK::SimpleTop, a, [1, 1, 1], 0));
            } else {
                g.claim(a);
                let m = r.next_int_bound(3);
                rooms.push((MK::Simple, a, [1, 1, 1], m));
            }
        }
        let g = Arc::new(g.0);
        let o = Some(dir);
        let p = world_pos(&bb, o, 9, 0, 22);
        let mut children: Vec<MRoom> = rooms
            .into_iter()
            .map(|(kind, room, s, m)| MRoom { kind, bb: bb_move(room_bb(dir, g[room as usize].index, s[0], s[1], s[2]), p), room, g: g.clone(), main_design: m })
            .collect();
        let b1 = bb_from_corners(world_pos(&bb, o, 1, 1, 1), world_pos(&bb, o, 23, 8, 21));
        let b2 = bb_from_corners(world_pos(&bb, o, 34, 1, 1), world_pos(&bb, o, 56, 8, 21));
        let b3 = bb_from_corners(world_pos(&bb, o, 22, 13, 22), world_pos(&bb, o, 35, 17, 35));
        let i = r.next_int();
        children.push(MRoom { kind: MK::Wing, bb: b1, room: 0, g: g.clone(), main_design: i & 1 });
        children.push(MRoom { kind: MK::Wing, bb: b2, room: 0, g: g.clone(), main_design: i.wrapping_add(1) & 1 });
        children.push(MRoom { kind: MK::Penthouse, bb: b3, room: 0, g, main_design: 0 });
        Monument { bb, orient: dir, children }
    }
}

impl LPiece for Monument {
    fn bb(&self) -> Bb {
        self.bb
    }
    fn name(&self) -> String {
        "ocean_monument".into()
    }
    fn post_process(&self, lv: &mut Region, _lib: &LLib, seed: i64, r: &mut dyn Rng, cb: &Bb, reference: P) {
        let mut lr = Legacy::new(seed ^ (self.bb.0[0] as i64) << 20 ^ self.bb.0[2] as i64);
        let o = Some(self.orient);
        {
            let mut sp = Sp { lv: &mut *lv, cb: *cb, bb: self.bb, orient: o };
            building_post_process(self, &mut sp, r, &mut lr);
        }
        for c in &self.children {
            if !bb_intersects(&c.bb, cb) {
                continue;
            }
            let mut sp = Sp { lv: &mut *lv, cb: *cb, bb: c.bb, orient: o };
            let f = match c.kind {
                MK::Core => pp_core_room,
                MK::DoubleX => pp_double_x_room,
                MK::DoubleXY => pp_double_x_y_room,
                MK::DoubleY => pp_double_y_room,
                MK::DoubleYZ => pp_double_y_z_room,
                MK::DoubleZ => pp_double_z_room,
                MK::Entry => pp_entry_room,
                MK::Penthouse => pp_penthouse,
                MK::Simple => pp_simple_room,
                MK::SimpleTop => pp_simple_top_room,
                MK::Wing => pp_wing_room,
            };
            f(c, &mut sp, r, &mut lr, reference);
        }
    }
}

/// `MonumentBuilding.postProcess` (without the child loop)
fn building_post_process(t: &Monument, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng) {
    let i = SEA_LEVEL.max(64) - sp.bb.0[1];
    m_generate_water_box(t, sp, r, lr, 0, 0, 0, 58, i, 58);
    m_generate_wing(t, sp, r, lr, false, 0);
    m_generate_wing(t, sp, r, lr, true, 33);
    m_generate_entrance_archs(t, sp, r, lr);
    m_generate_entrance_wall(t, sp, r, lr);
    m_generate_roof_piece(t, sp, r, lr);
    m_generate_lower_wall(t, sp, r, lr);
    m_generate_middle_wall(t, sp, r, lr);
    m_generate_upper_wall(t, sp, r, lr);
    let light = state("prismarine_bricks", &[]);
    for j in 0..7 {
        let mut k = 0;
        while k < 7 {
            if k == 0 && j == 3 {
                k = 6;
            }
            let (l, m) = (j * 9, k * 9);
            for n in 0..4 {
                for o in 0..4 {
                    sp.place(light, l + n, 0, m + o);
                    sp.fill_column_down(light, l + n, -1, m + o);
                }
            }
            if j == 0 || j == 6 {
                k += 1;
            } else {
                k += 6;
            }
        }
    }
    for j in 0..5 {
        m_generate_water_box(t, sp, r, lr, -1 - j, 0 + j * 2, -1 - j, -1 - j, 23, 58 + j);
        m_generate_water_box(t, sp, r, lr, 58 + j, 0 + j * 2, -1 - j, 58 + j, 23, 58 + j);
        m_generate_water_box(t, sp, r, lr, 0 - j, 0 + j * 2, -1 - j, 57 + j, 23, -1 - j);
        m_generate_water_box(t, sp, r, lr, 0 - j, 0 + j * 2, 58 + j, 57 + j, 23, 58 + j);
    }
}

// ------------------------------------------------------------ OceanMonumentPiece helpers

fn keep(s: BlockState) -> bool {
    matches!(s.name(), "ice" | "packed_ice" | "blue_ice" | "water")
}

/// `generateWaterBox`
fn m_generate_water_box<T>(_t: &T, sp: &mut Sp, _r: &mut dyn Rng, _lr: &mut dyn Rng, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32) {
    let fill = state("water", &[]);
    for y in y0..=y1 {
        for x in x0..=x1 {
            for z in z0..=z1 {
                let s = sp.get_block(x, y, z);
                if keep(s) {
                    continue;
                }
                if sp.wy(y) >= SEA_LEVEL && s != fill {
                    sp.place(blocks::AIR, x, y, z);
                } else {
                    sp.place(fill, x, y, z);
                }
            }
        }
    }
}

/// `generateDefaultFloor`
fn m_generate_default_floor<T>(_t: &T, sp: &mut Sp, _r: &mut dyn Rng, _lr: &mut dyn Rng, x: i32, z: i32, open: bool) {
    let gray = state("prismarine", &[]);
    let light = state("prismarine_bricks", &[]);
    if open {
        sp.gen_box(x, 0, z, x + 2, 0, z + 7, gray, gray, false);
        sp.gen_box(x + 5, 0, z, x + 7, 0, z + 7, gray, gray, false);
        sp.gen_box(x + 3, 0, z, x + 4, 0, z + 2, gray, gray, false);
        sp.gen_box(x + 3, 0, z + 5, x + 4, 0, z + 7, gray, gray, false);
        sp.gen_box(x + 3, 0, z + 2, x + 4, 0, z + 2, light, light, false);
        sp.gen_box(x + 3, 0, z + 5, x + 4, 0, z + 5, light, light, false);
        sp.gen_box(x + 2, 0, z + 3, x + 2, 0, z + 4, light, light, false);
        sp.gen_box(x + 5, 0, z + 3, x + 5, 0, z + 4, light, light, false);
    } else {
        sp.gen_box(x, 0, z, x + 7, 0, z + 7, gray, gray, false);
    }
}

/// `generateBoxOnFillOnly`
fn m_generate_box_on_fill_only<T>(_t: &T, sp: &mut Sp, _r: &mut dyn Rng, _lr: &mut dyn Rng, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, s: BlockState) {
    let fill = state("water", &[]);
    for y in y0..=y1 {
        for x in x0..=x1 {
            for z in z0..=z1 {
                if sp.get_block(x, y, z) == fill {
                    sp.place(s, x, y, z);
                }
            }
        }
    }
}

/// `chunkIntersects`
fn m_chunk_intersects<T>(_t: &T, sp: &mut Sp, _r: &mut dyn Rng, _lr: &mut dyn Rng, x0: i32, z0: i32, x1: i32, z1: i32) -> bool {
    let (a, b, c, d) = (sp.wx(x0, z0), sp.wz(x0, z0), sp.wx(x1, z1), sp.wz(x1, z1));
    let cb = &sp.cb;
    a.max(c) >= cb.0[0] && a.min(c) <= cb.1[0] && b.max(d) >= cb.0[2] && b.min(d) <= cb.1[2]
}

/// `spawnElder`: the elder guardian is spawned by the entity layer
fn m_spawn_elder<T>(_t: &T, sp: &mut Sp, _r: &mut dyn Rng, _lr: &mut dyn Rng, x: i32, y: i32, z: i32) {
    let p = sp.pos(x, y, z);
    if super::template::bb_inside(&sp.cb, p) {
        sp.lv.spawn_entity("elder_guardian", p);
    }
}

// ------------------------------------------------------------ generated
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_generate_wing(t: &Monument, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a0: bool, a1: i32) {
    let mut l10: i32 = 0;
    let mut l6: i32 = 0;
    let mut l7: i32 = 0;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut sk0_i32: i32 = 0;
    let mut sk1_i32: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    l6 = 24;
    let t1 = m_chunk_intersects(t, sp, r, lr, a1, 0, (a1 + 23), 20);
    pc = if !(t1) { 713 } else { 21 };
    }
    21 => {
    sp.gen_box((a1 + 0), 0, 0, (a1 + 24), 0, 20, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, (a1 + 0), 1, 0, (a1 + 24), 10, 20);
    l7 = 0;
    pc = 70;
    }
    70 => {
    pc = if l7 >= 4 { 327 } else { 76 };
    }
    76 => {
    sp.gen_box((a1 + l7), (l7 + 1), l7, (a1 + l7), (l7 + 1), 20, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(((a1 + l7) + 7), (l7 + 5), (l7 + 7), ((a1 + l7) + 7), (l7 + 5), 20, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(((a1 + 17) - l7), (l7 + 5), (l7 + 7), ((a1 + 17) - l7), (l7 + 5), 20, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(((a1 + 24) - l7), (l7 + 1), l7, ((a1 + 24) - l7), (l7 + 1), 20, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(((a1 + l7) + 1), (l7 + 1), l7, ((a1 + 23) - l7), (l7 + 1), l7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(((a1 + l7) + 8), (l7 + 5), (l7 + 7), ((a1 + 16) - l7), (l7 + 5), (l7 + 7), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l7 += 1;
    pc = 70;
    }
    327 => {
    sp.gen_box((a1 + 4), 4, 4, (a1 + 6), 4, 20, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box((a1 + 7), 4, 4, (a1 + 17), 4, 6, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box((a1 + 18), 4, 4, (a1 + 20), 4, 20, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box((a1 + 11), 8, 11, (a1 + 13), 8, 20, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.place(state("prismarine_bricks", &[]), (a1 + 12), 9, 12);
    sp.place(state("prismarine_bricks", &[]), (a1 + 12), 9, 15);
    sp.place(state("prismarine_bricks", &[]), (a1 + 12), 9, 18);
    sk0_i32 = a1;
    pc = if !(a0) { 501 } else { 496 };
    }
    496 => {
    sk0_i32 = sk0_i32;
    sk1_i32 = 19;
    pc = 502;
    }
    501 => {
    sk0_i32 = sk0_i32;
    sk1_i32 = 5;
    pc = 502;
    }
    502 => {
    l7 = (sk0_i32 + sk1_i32);
    sk0_i32 = a1;
    pc = if !(a0) { 514 } else { 510 };
    }
    510 => {
    sk0_i32 = sk0_i32;
    sk1_i32 = 5;
    pc = 516;
    }
    514 => {
    sk0_i32 = sk0_i32;
    sk1_i32 = 19;
    pc = 516;
    }
    516 => {
    l8 = (sk0_i32 + sk1_i32);
    l9 = 20;
    pc = 523;
    }
    523 => {
    pc = if l9 < 5 { 550 } else { 529 };
    }
    529 => {
    sp.place(state("prismarine_bricks", &[]), l7, 5, l9);
    l9 += -3;
    pc = 523;
    }
    550 => {
    l9 = 19;
    pc = 554;
    }
    554 => {
    pc = if l9 < 7 { 582 } else { 561 };
    }
    561 => {
    sp.place(state("prismarine_bricks", &[]), l8, 5, l9);
    l9 += -3;
    pc = 554;
    }
    582 => {
    l9 = 0;
    pc = 585;
    }
    585 => {
    pc = if l9 >= 4 { 641 } else { 591 };
    }
    591 => {
    pc = if !(a0) { 610 } else { 595 };
    }
    595 => {
    sk0_i32 = ((a1 + 24) - (17 - (l9 * 3)));
    pc = 619;
    }
    610 => {
    sk0_i32 = ((a1 + 17) - (l9 * 3));
    pc = 619;
    }
    619 => {
    l10 = sk0_i32;
    sp.place(state("prismarine_bricks", &[]), l10, 5, 5);
    l9 += 1;
    pc = 585;
    }
    641 => {
    sp.place(state("prismarine_bricks", &[]), l8, 5, 5);
    sp.gen_box((a1 + 11), 1, 12, (a1 + 13), 7, 12, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box((a1 + 12), 1, 11, (a1 + 12), 7, 13, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    pc = 713;
    }
    713 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_generate_entrance_archs(t: &Monument, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng) {
    let mut l4: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = m_chunk_intersects(t, sp, r, lr, 22, 5, 35, 17);
    pc = if !(t1) { 313 } else { 15 };
    }
    15 => {
    m_generate_water_box(t, sp, r, lr, 25, 0, 0, 32, 8, 20);
    l4 = 0;
    pc = 34;
    }
    34 => {
    pc = if l4 >= 4 { 313 } else { 40 };
    }
    40 => {
    sp.gen_box(24, 2, (5 + (l4 * 4)), 24, 4, (5 + (l4 * 4)), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(22, 4, (5 + (l4 * 4)), 23, 4, (5 + (l4 * 4)), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("prismarine_bricks", &[]), 25, 5, (5 + (l4 * 4)));
    sp.place(state("prismarine_bricks", &[]), 26, 6, (5 + (l4 * 4)));
    sp.place(state("sea_lantern", &[]), 26, 5, (5 + (l4 * 4)));
    sp.gen_box(33, 2, (5 + (l4 * 4)), 33, 4, (5 + (l4 * 4)), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(34, 4, (5 + (l4 * 4)), 35, 4, (5 + (l4 * 4)), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("prismarine_bricks", &[]), 32, 5, (5 + (l4 * 4)));
    sp.place(state("prismarine_bricks", &[]), 31, 6, (5 + (l4 * 4)));
    sp.place(state("sea_lantern", &[]), 31, 5, (5 + (l4 * 4)));
    sp.gen_box(27, 6, (5 + (l4 * 4)), 30, 6, (5 + (l4 * 4)), state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    l4 += 1;
    pc = 34;
    }
    313 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_generate_entrance_wall(t: &Monument, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng) {
    let mut l4: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = m_chunk_intersects(t, sp, r, lr, 15, 20, 42, 21);
    pc = if !(t1) { 877 } else { 16 };
    }
    16 => {
    sp.gen_box(15, 0, 21, 42, 0, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 26, 1, 21, 31, 3, 21);
    sp.gen_box(21, 12, 21, 36, 12, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(17, 11, 21, 40, 11, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(16, 10, 21, 41, 10, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(15, 7, 21, 42, 9, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(16, 6, 21, 41, 6, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(17, 5, 21, 40, 5, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(21, 4, 21, 36, 4, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(22, 3, 21, 26, 3, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(31, 3, 21, 35, 3, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(23, 2, 21, 25, 2, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(32, 2, 21, 34, 2, 21, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(28, 4, 20, 29, 4, 21, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("prismarine_bricks", &[]), 27, 3, 21);
    sp.place(state("prismarine_bricks", &[]), 30, 3, 21);
    sp.place(state("prismarine_bricks", &[]), 26, 2, 21);
    sp.place(state("prismarine_bricks", &[]), 31, 2, 21);
    sp.place(state("prismarine_bricks", &[]), 25, 1, 21);
    sp.place(state("prismarine_bricks", &[]), 32, 1, 21);
    l4 = 0;
    pc = 428;
    }
    428 => {
    pc = if l4 >= 7 { 483 } else { 435 };
    }
    435 => {
    sp.place(state("dark_prismarine", &[]), (28 - l4), (6 + l4), 21);
    sp.place(state("dark_prismarine", &[]), (29 + l4), (6 + l4), 21);
    l4 += 1;
    pc = 428;
    }
    483 => {
    l4 = 0;
    pc = 486;
    }
    486 => {
    pc = if l4 >= 4 { 540 } else { 492 };
    }
    492 => {
    sp.place(state("dark_prismarine", &[]), (28 - l4), (9 + l4), 21);
    sp.place(state("dark_prismarine", &[]), (29 + l4), (9 + l4), 21);
    l4 += 1;
    pc = 486;
    }
    540 => {
    sp.place(state("dark_prismarine", &[]), 28, 12, 21);
    sp.place(state("dark_prismarine", &[]), 29, 12, 21);
    l4 = 0;
    pc = 573;
    }
    573 => {
    pc = if l4 >= 3 { 665 } else { 579 };
    }
    579 => {
    sp.place(state("dark_prismarine", &[]), (22 - (l4 * 2)), 8, 21);
    sp.place(state("dark_prismarine", &[]), (22 - (l4 * 2)), 9, 21);
    sp.place(state("dark_prismarine", &[]), (35 + (l4 * 2)), 8, 21);
    sp.place(state("dark_prismarine", &[]), (35 + (l4 * 2)), 9, 21);
    l4 += 1;
    pc = 573;
    }
    665 => {
    m_generate_water_box(t, sp, r, lr, 15, 13, 21, 42, 15, 21);
    m_generate_water_box(t, sp, r, lr, 15, 1, 21, 15, 6, 21);
    m_generate_water_box(t, sp, r, lr, 16, 1, 21, 16, 5, 21);
    m_generate_water_box(t, sp, r, lr, 17, 1, 21, 20, 4, 21);
    m_generate_water_box(t, sp, r, lr, 21, 1, 21, 21, 3, 21);
    m_generate_water_box(t, sp, r, lr, 22, 1, 21, 22, 2, 21);
    m_generate_water_box(t, sp, r, lr, 23, 1, 21, 24, 1, 21);
    m_generate_water_box(t, sp, r, lr, 42, 1, 21, 42, 6, 21);
    m_generate_water_box(t, sp, r, lr, 41, 1, 21, 41, 5, 21);
    m_generate_water_box(t, sp, r, lr, 37, 1, 21, 40, 4, 21);
    m_generate_water_box(t, sp, r, lr, 36, 1, 21, 36, 3, 21);
    m_generate_water_box(t, sp, r, lr, 33, 1, 21, 34, 1, 21);
    m_generate_water_box(t, sp, r, lr, 35, 1, 21, 35, 2, 21);
    pc = 877;
    }
    877 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_generate_roof_piece(t: &Monument, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng) {
    let mut l4: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = m_chunk_intersects(t, sp, r, lr, 21, 21, 36, 36);
    pc = if !(t1) { 648 } else { 16 };
    }
    16 => {
    sp.gen_box(21, 0, 22, 36, 0, 36, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 21, 1, 22, 36, 23, 36);
    l4 = 0;
    pc = 59;
    }
    59 => {
    pc = if l4 >= 4 { 243 } else { 65 };
    }
    65 => {
    sp.gen_box((21 + l4), (13 + l4), (21 + l4), (36 - l4), (13 + l4), (21 + l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box((21 + l4), (13 + l4), (36 - l4), (36 - l4), (13 + l4), (36 - l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box((21 + l4), (13 + l4), (22 + l4), (21 + l4), (13 + l4), (35 - l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box((36 - l4), (13 + l4), (22 + l4), (36 - l4), (13 + l4), (35 - l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 += 1;
    pc = 59;
    }
    243 => {
    sp.gen_box(25, 16, 25, 32, 16, 32, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(25, 17, 25, 25, 19, 25, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(32, 17, 25, 32, 19, 25, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(25, 17, 32, 25, 19, 32, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(32, 17, 32, 32, 19, 32, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("prismarine_bricks", &[]), 26, 20, 26);
    sp.place(state("prismarine_bricks", &[]), 27, 21, 27);
    sp.place(state("sea_lantern", &[]), 27, 20, 27);
    sp.place(state("prismarine_bricks", &[]), 26, 20, 31);
    sp.place(state("prismarine_bricks", &[]), 27, 21, 30);
    sp.place(state("sea_lantern", &[]), 27, 20, 30);
    sp.place(state("prismarine_bricks", &[]), 31, 20, 31);
    sp.place(state("prismarine_bricks", &[]), 30, 21, 30);
    sp.place(state("sea_lantern", &[]), 30, 20, 30);
    sp.place(state("prismarine_bricks", &[]), 31, 20, 26);
    sp.place(state("prismarine_bricks", &[]), 30, 21, 27);
    sp.place(state("sea_lantern", &[]), 30, 20, 27);
    sp.gen_box(28, 21, 27, 29, 21, 27, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(27, 21, 28, 27, 21, 29, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(28, 21, 30, 29, 21, 30, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(30, 21, 28, 30, 21, 29, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    pc = 648;
    }
    648 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_generate_lower_wall(t: &Monument, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng) {
    let mut l4: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = m_chunk_intersects(t, sp, r, lr, 0, 21, 6, 58);
    pc = if !(t1) { 255 } else { 15 };
    }
    15 => {
    sp.gen_box(0, 0, 21, 6, 0, 57, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 0, 1, 21, 6, 7, 57);
    sp.gen_box(4, 4, 21, 6, 4, 53, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    l4 = 0;
    pc = 78;
    }
    78 => {
    pc = if l4 >= 4 { 122 } else { 84 };
    }
    84 => {
    sp.gen_box(l4, (l4 + 1), 21, l4, (l4 + 1), (57 - l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 += 1;
    pc = 78;
    }
    122 => {
    l4 = 23;
    pc = 126;
    }
    126 => {
    pc = if l4 >= 53 { 152 } else { 133 };
    }
    133 => {
    sp.place(state("prismarine_bricks", &[]), 5, 5, l4);
    l4 += 3;
    pc = 126;
    }
    152 => {
    sp.place(state("prismarine_bricks", &[]), 5, 5, 52);
    l4 = 0;
    pc = 168;
    }
    168 => {
    pc = if l4 >= 4 { 212 } else { 174 };
    }
    174 => {
    sp.gen_box(l4, (l4 + 1), 21, l4, (l4 + 1), (57 - l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 += 1;
    pc = 168;
    }
    212 => {
    sp.gen_box(4, 1, 52, 6, 3, 52, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(5, 1, 51, 5, 3, 53, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    pc = 255;
    }
    255 => {
    let t2 = m_chunk_intersects(t, sp, r, lr, 51, 21, 58, 58);
    pc = if !(t2) { 478 } else { 271 };
    }
    271 => {
    sp.gen_box(51, 0, 21, 57, 0, 57, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 51, 1, 21, 57, 7, 57);
    sp.gen_box(51, 4, 21, 53, 4, 53, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    l4 = 0;
    pc = 337;
    }
    337 => {
    pc = if l4 >= 4 { 387 } else { 343 };
    }
    343 => {
    sp.gen_box((57 - l4), (l4 + 1), 21, (57 - l4), (l4 + 1), (57 - l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 += 1;
    pc = 337;
    }
    387 => {
    l4 = 23;
    pc = 391;
    }
    391 => {
    pc = if l4 >= 53 { 418 } else { 398 };
    }
    398 => {
    sp.place(state("prismarine_bricks", &[]), 52, 5, l4);
    l4 += 3;
    pc = 391;
    }
    418 => {
    sp.place(state("prismarine_bricks", &[]), 52, 5, 52);
    sp.gen_box(51, 1, 52, 53, 3, 52, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(52, 1, 51, 52, 3, 53, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    pc = 478;
    }
    478 => {
    let t3 = m_chunk_intersects(t, sp, r, lr, 0, 51, 57, 57);
    pc = if !(t3) { 588 } else { 493 };
    }
    493 => {
    sp.gen_box(7, 0, 51, 50, 0, 57, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 7, 1, 51, 50, 10, 57);
    l4 = 0;
    pc = 536;
    }
    536 => {
    pc = if l4 >= 4 { 588 } else { 542 };
    }
    542 => {
    sp.gen_box((l4 + 1), (l4 + 1), (57 - l4), (56 - l4), (l4 + 1), (57 - l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 += 1;
    pc = 536;
    }
    588 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_generate_middle_wall(t: &Monument, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng) {
    let mut l4: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = m_chunk_intersects(t, sp, r, lr, 7, 21, 13, 50);
    pc = if !(t1) { 163 } else { 16 };
    }
    16 => {
    sp.gen_box(7, 0, 21, 13, 0, 50, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 7, 1, 21, 13, 10, 50);
    sp.gen_box(11, 8, 21, 13, 8, 53, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    l4 = 0;
    pc = 84;
    }
    84 => {
    pc = if l4 >= 4 { 131 } else { 90 };
    }
    90 => {
    sp.gen_box((l4 + 7), (l4 + 5), 21, (l4 + 7), (l4 + 5), 54, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 += 1;
    pc = 84;
    }
    131 => {
    l4 = 21;
    pc = 135;
    }
    135 => {
    pc = if l4 > 45 { 163 } else { 142 };
    }
    142 => {
    sp.place(state("prismarine_bricks", &[]), 12, 9, l4);
    l4 += 3;
    pc = 135;
    }
    163 => {
    let t2 = m_chunk_intersects(t, sp, r, lr, 44, 21, 50, 54);
    pc = if !(t2) { 326 } else { 179 };
    }
    179 => {
    sp.gen_box(44, 0, 21, 50, 0, 50, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 44, 1, 21, 50, 10, 50);
    sp.gen_box(44, 8, 21, 46, 8, 53, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    l4 = 0;
    pc = 247;
    }
    247 => {
    pc = if l4 >= 4 { 294 } else { 253 };
    }
    253 => {
    sp.gen_box((50 - l4), (l4 + 5), 21, (50 - l4), (l4 + 5), 54, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 += 1;
    pc = 247;
    }
    294 => {
    l4 = 21;
    pc = 298;
    }
    298 => {
    pc = if l4 > 45 { 326 } else { 305 };
    }
    305 => {
    sp.place(state("prismarine_bricks", &[]), 45, 9, l4);
    l4 += 3;
    pc = 298;
    }
    326 => {
    let t3 = m_chunk_intersects(t, sp, r, lr, 8, 44, 49, 54);
    pc = if !(t3) { 721 } else { 342 };
    }
    342 => {
    sp.gen_box(14, 0, 44, 43, 0, 50, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 14, 1, 44, 43, 10, 50);
    l4 = 12;
    pc = 386;
    }
    386 => {
    pc = if l4 > 45 { 621 } else { 393 };
    }
    393 => {
    sp.place(state("prismarine_bricks", &[]), l4, 9, 45);
    sp.place(state("prismarine_bricks", &[]), l4, 9, 52);
    pc = if l4 == 12 { 465 } else { 430 };
    }
    430 => {
    pc = if l4 == 18 { 465 } else { 437 };
    }
    437 => {
    pc = if l4 == 24 { 465 } else { 444 };
    }
    444 => {
    pc = if l4 == 33 { 465 } else { 451 };
    }
    451 => {
    pc = if l4 == 39 { 465 } else { 458 };
    }
    458 => {
    pc = if l4 != 45 { 615 } else { 465 };
    }
    465 => {
    sp.place(state("prismarine_bricks", &[]), l4, 9, 47);
    sp.place(state("prismarine_bricks", &[]), l4, 9, 50);
    sp.place(state("prismarine_bricks", &[]), l4, 10, 45);
    sp.place(state("prismarine_bricks", &[]), l4, 10, 46);
    sp.place(state("prismarine_bricks", &[]), l4, 10, 51);
    sp.place(state("prismarine_bricks", &[]), l4, 10, 52);
    sp.place(state("prismarine_bricks", &[]), l4, 11, 47);
    sp.place(state("prismarine_bricks", &[]), l4, 11, 50);
    sp.place(state("prismarine_bricks", &[]), l4, 12, 48);
    sp.place(state("prismarine_bricks", &[]), l4, 12, 49);
    pc = 615;
    }
    615 => {
    l4 += 3;
    pc = 386;
    }
    621 => {
    l4 = 0;
    pc = 624;
    }
    624 => {
    pc = if l4 >= 3 { 671 } else { 630 };
    }
    630 => {
    sp.gen_box((8 + l4), (5 + l4), 54, (49 - l4), (5 + l4), 54, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    l4 += 1;
    pc = 624;
    }
    671 => {
    sp.gen_box(11, 8, 54, 46, 8, 54, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(14, 8, 44, 43, 8, 53, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    pc = 721;
    }
    721 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn m_generate_upper_wall(t: &Monument, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng) {
    let mut l4: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    let t1 = m_chunk_intersects(t, sp, r, lr, 14, 21, 20, 43);
    pc = if !(t1) { 193 } else { 16 };
    }
    16 => {
    sp.gen_box(14, 0, 21, 20, 0, 43, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 14, 1, 22, 20, 14, 43);
    sp.gen_box(18, 12, 22, 20, 12, 39, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(18, 12, 21, 20, 12, 21, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 = 0;
    pc = 109;
    }
    109 => {
    pc = if l4 >= 4 { 161 } else { 115 };
    }
    115 => {
    sp.gen_box((l4 + 14), (l4 + 9), 21, (l4 + 14), (l4 + 9), (43 - l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 += 1;
    pc = 109;
    }
    161 => {
    l4 = 23;
    pc = 165;
    }
    165 => {
    pc = if l4 > 39 { 193 } else { 172 };
    }
    172 => {
    sp.place(state("prismarine_bricks", &[]), 19, 13, l4);
    l4 += 3;
    pc = 165;
    }
    193 => {
    let t2 = m_chunk_intersects(t, sp, r, lr, 37, 21, 43, 43);
    pc = if !(t2) { 386 } else { 209 };
    }
    209 => {
    sp.gen_box(37, 0, 21, 43, 0, 43, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 37, 1, 22, 43, 14, 43);
    sp.gen_box(37, 12, 22, 39, 12, 39, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(37, 12, 21, 39, 12, 21, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 = 0;
    pc = 302;
    }
    302 => {
    pc = if l4 >= 4 { 354 } else { 308 };
    }
    308 => {
    sp.gen_box((43 - l4), (l4 + 9), 21, (43 - l4), (l4 + 9), (43 - l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 += 1;
    pc = 302;
    }
    354 => {
    l4 = 23;
    pc = 358;
    }
    358 => {
    pc = if l4 > 39 { 386 } else { 365 };
    }
    365 => {
    sp.place(state("prismarine_bricks", &[]), 38, 13, l4);
    l4 += 3;
    pc = 358;
    }
    386 => {
    let t3 = m_chunk_intersects(t, sp, r, lr, 15, 37, 42, 43);
    pc = if !(t3) { 557 } else { 402 };
    }
    402 => {
    sp.gen_box(21, 0, 37, 36, 0, 43, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 21, 1, 37, 36, 14, 43);
    sp.gen_box(21, 12, 37, 36, 12, 39, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    l4 = 0;
    pc = 470;
    }
    470 => {
    pc = if l4 >= 4 { 525 } else { 476 };
    }
    476 => {
    sp.gen_box((15 + l4), (l4 + 9), (43 - l4), (42 - l4), (l4 + 9), (43 - l4), state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l4 += 1;
    pc = 470;
    }
    525 => {
    l4 = 21;
    pc = 529;
    }
    529 => {
    pc = if l4 > 36 { 557 } else { 536 };
    }
    536 => {
    sp.place(state("prismarine_bricks", &[]), l4, 13, 38);
    l4 += 3;
    pc = 529;
    }
    557 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_core_room(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a9_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 1, 8, 0, 14, 8, 14, state("prismarine", &[]));
    l8 = 7;
    a9_BlockState = state("prismarine_bricks", &[]);
    sp.gen_box(0, 7, 0, 0, 7, 15, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(15, 7, 0, 15, 7, 15, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(1, 7, 0, 15, 7, 0, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(1, 7, 15, 14, 7, 15, a9_BlockState, a9_BlockState, (0 != 0));
    l8 = 1;
    pc = 120;
    }
    120 => {
    pc = if l8 > 6 { 329 } else { 127 };
    }
    127 => {
    a9_BlockState = state("prismarine_bricks", &[]);
    pc = if l8 == 2 { 145 } else { 138 };
    }
    138 => {
    pc = if l8 != 6 { 150 } else { 145 };
    }
    145 => {
    a9_BlockState = state("prismarine", &[]);
    pc = 150;
    }
    150 => {
    l10 = 0;
    pc = 153;
    }
    153 => {
    pc = if l10 > 15 { 236 } else { 160 };
    }
    160 => {
    sp.gen_box(l10, l8, 0, l10, l8, 1, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(l10, l8, 6, l10, l8, 9, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(l10, l8, 14, l10, l8, 15, a9_BlockState, a9_BlockState, (0 != 0));
    l10 += 15;
    pc = 153;
    }
    236 => {
    sp.gen_box(1, l8, 0, 1, l8, 0, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(6, l8, 0, 9, l8, 0, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(14, l8, 0, 14, l8, 0, a9_BlockState, a9_BlockState, (0 != 0));
    sp.gen_box(1, l8, 15, 14, l8, 15, a9_BlockState, a9_BlockState, (0 != 0));
    l8 += 1;
    pc = 120;
    }
    329 => {
    sp.gen_box(6, 3, 6, 9, 6, 9, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(7, 4, 7, 8, 5, 8, state("gold_block", &[]), state("gold_block", &[]), (0 != 0));
    l8 = 3;
    pc = 387;
    }
    387 => {
    pc = if l8 > 6 { 449 } else { 394 };
    }
    394 => {
    l9 = 6;
    pc = 398;
    }
    398 => {
    pc = if l9 > 9 { 443 } else { 405 };
    }
    405 => {
    sp.place(state("sea_lantern", &[]), l9, l8, 6);
    sp.place(state("sea_lantern", &[]), l9, l8, 9);
    l9 += 3;
    pc = 398;
    }
    443 => {
    l8 += 3;
    pc = 387;
    }
    449 => {
    sp.gen_box(5, 1, 6, 5, 2, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 1, 9, 5, 2, 9, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(10, 1, 6, 10, 2, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(10, 1, 9, 10, 2, 9, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 5, 6, 2, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(9, 1, 5, 9, 2, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 10, 6, 2, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(9, 1, 10, 9, 2, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 2, 5, 5, 6, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 2, 10, 5, 6, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(10, 2, 5, 10, 6, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(10, 2, 10, 10, 6, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 7, 1, 5, 7, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(10, 7, 1, 10, 7, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 7, 9, 5, 7, 14, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(10, 7, 9, 10, 7, 14, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 7, 5, 6, 7, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 7, 10, 6, 7, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(9, 7, 5, 14, 7, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(9, 7, 10, 14, 7, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(2, 1, 2, 2, 1, 3, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, 1, 2, 3, 1, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(13, 1, 2, 13, 1, 3, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(12, 1, 2, 12, 1, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(2, 1, 12, 2, 1, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, 1, 13, 3, 1, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(13, 1, 12, 13, 1, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(12, 1, 13, 12, 1, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_double_x_room(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_OptionRoom: Option<Room> = Default::default();
    let mut a9_Room: Room = Default::default();
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    a8_OptionRoom = t.conn(t.def().conn[((piece::EAST as i32)) as usize]);
    a9_Room = t.def();
    pc = if (t.def().index / 25) <= 0 { 78 } else { 35 };
    }
    35 => {
    m_generate_default_floor(t, sp, r, lr, 8, 0, a8_OptionRoom.unwrap().open[((piece::DOWN as i32)) as usize]);
    m_generate_default_floor(t, sp, r, lr, 0, 0, a9_Room.open[((piece::DOWN as i32)) as usize]);
    pc = 78;
    }
    78 => {
    pc = if t.conn(a9_Room.conn[((piece::UP as i32)) as usize]).is_some() { 111 } else { 93 };
    }
    93 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 1, 4, 1, 7, 4, 6, state("prismarine", &[]));
    pc = 111;
    }
    111 => {
    pc = if t.conn(a8_OptionRoom.unwrap().conn[((piece::UP as i32)) as usize]).is_some() { 145 } else { 126 };
    }
    126 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 8, 4, 1, 14, 4, 6, state("prismarine", &[]));
    pc = 145;
    }
    145 => {
    sp.gen_box(0, 3, 0, 0, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(15, 3, 0, 15, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 0, 15, 3, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 7, 14, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 0, 2, 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(15, 2, 0, 15, 2, 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(1, 2, 0, 15, 2, 0, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(1, 2, 7, 14, 2, 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(0, 1, 0, 0, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(15, 1, 0, 15, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 0, 15, 1, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 7, 14, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 1, 0, 10, 1, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 2, 0, 9, 2, 3, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(5, 3, 0, 10, 3, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("sea_lantern", &[]), 6, 2, 3);
    sp.place(state("sea_lantern", &[]), 9, 2, 3);
    pc = if !(a9_Room.open[((piece::SOUTH as i32)) as usize]) { 529 } else { 516 };
    }
    516 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 0, 4, 2, 0);
    pc = 529;
    }
    529 => {
    pc = if !(a9_Room.open[((piece::NORTH as i32)) as usize]) { 559 } else { 544 };
    }
    544 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 7, 4, 2, 7);
    pc = 559;
    }
    559 => {
    pc = if !(a9_Room.open[((piece::WEST as i32)) as usize]) { 587 } else { 574 };
    }
    574 => {
    m_generate_water_box(t, sp, r, lr, 0, 1, 3, 0, 2, 4);
    pc = 587;
    }
    587 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::SOUTH as i32)) as usize]) { 617 } else { 602 };
    }
    602 => {
    m_generate_water_box(t, sp, r, lr, 11, 1, 0, 12, 2, 0);
    pc = 617;
    }
    617 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::NORTH as i32)) as usize]) { 649 } else { 632 };
    }
    632 => {
    m_generate_water_box(t, sp, r, lr, 11, 1, 7, 12, 2, 7);
    pc = 649;
    }
    649 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::EAST as i32)) as usize]) { 679 } else { 664 };
    }
    664 => {
    m_generate_water_box(t, sp, r, lr, 15, 1, 3, 15, 2, 4);
    pc = 679;
    }
    679 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_double_x_y_room(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a10_OptionRoom: Option<Room> = Default::default();
    let mut a11_OptionRoom: Option<Room> = Default::default();
    let mut a13_BlockState: BlockState = blocks::AIR;
    let mut a8_OptionRoom: Option<Room> = Default::default();
    let mut a9_Room: Room = Default::default();
    let mut l12: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    a8_OptionRoom = t.conn(t.def().conn[((piece::EAST as i32)) as usize]);
    a9_Room = t.def();
    a10_OptionRoom = t.conn(a9_Room.conn[((piece::UP as i32)) as usize]);
    a11_OptionRoom = t.conn(a8_OptionRoom.unwrap().conn[((piece::UP as i32)) as usize]);
    pc = if (t.def().index / 25) <= 0 { 106 } else { 63 };
    }
    63 => {
    m_generate_default_floor(t, sp, r, lr, 8, 0, a8_OptionRoom.unwrap().open[((piece::DOWN as i32)) as usize]);
    m_generate_default_floor(t, sp, r, lr, 0, 0, a9_Room.open[((piece::DOWN as i32)) as usize]);
    pc = 106;
    }
    106 => {
    pc = if t.conn(a10_OptionRoom.unwrap().conn[((piece::UP as i32)) as usize]).is_some() { 141 } else { 121 };
    }
    121 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 1, 8, 1, 7, 8, 6, state("prismarine", &[]));
    pc = 141;
    }
    141 => {
    pc = if t.conn(a11_OptionRoom.unwrap().conn[((piece::UP as i32)) as usize]).is_some() { 177 } else { 156 };
    }
    156 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 8, 8, 1, 14, 8, 6, state("prismarine", &[]));
    pc = 177;
    }
    177 => {
    l12 = 1;
    pc = 180;
    }
    180 => {
    pc = if l12 > 7 { 304 } else { 187 };
    }
    187 => {
    a13_BlockState = state("prismarine_bricks", &[]);
    pc = if l12 == 2 { 205 } else { 198 };
    }
    198 => {
    pc = if l12 != 6 { 210 } else { 205 };
    }
    205 => {
    a13_BlockState = state("prismarine", &[]);
    pc = 210;
    }
    210 => {
    sp.gen_box(0, l12, 0, 0, l12, 7, a13_BlockState, a13_BlockState, (0 != 0));
    sp.gen_box(15, l12, 0, 15, l12, 7, a13_BlockState, a13_BlockState, (0 != 0));
    sp.gen_box(1, l12, 0, 15, l12, 0, a13_BlockState, a13_BlockState, (0 != 0));
    sp.gen_box(1, l12, 7, 14, l12, 7, a13_BlockState, a13_BlockState, (0 != 0));
    l12 += 1;
    pc = 180;
    }
    304 => {
    sp.gen_box(2, 1, 3, 2, 7, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, 1, 2, 4, 7, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, 1, 5, 4, 7, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(13, 1, 3, 13, 7, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(11, 1, 2, 12, 7, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(11, 1, 5, 12, 7, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 1, 3, 5, 3, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(10, 1, 3, 10, 3, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 7, 2, 10, 7, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 5, 2, 5, 7, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(10, 5, 2, 10, 7, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 5, 5, 5, 7, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(10, 5, 5, 10, 7, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("prismarine_bricks", &[]), 6, 6, 2);
    sp.place(state("prismarine_bricks", &[]), 9, 6, 2);
    sp.place(state("prismarine_bricks", &[]), 6, 6, 5);
    sp.place(state("prismarine_bricks", &[]), 9, 6, 5);
    sp.gen_box(5, 4, 3, 6, 4, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(9, 4, 3, 10, 4, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("sea_lantern", &[]), 5, 4, 2);
    sp.place(state("sea_lantern", &[]), 5, 4, 5);
    sp.place(state("sea_lantern", &[]), 10, 4, 2);
    sp.place(state("sea_lantern", &[]), 10, 4, 5);
    pc = if !(a9_Room.open[((piece::SOUTH as i32)) as usize]) { 774 } else { 761 };
    }
    761 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 0, 4, 2, 0);
    pc = 774;
    }
    774 => {
    pc = if !(a9_Room.open[((piece::NORTH as i32)) as usize]) { 804 } else { 789 };
    }
    789 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 7, 4, 2, 7);
    pc = 804;
    }
    804 => {
    pc = if !(a9_Room.open[((piece::WEST as i32)) as usize]) { 832 } else { 819 };
    }
    819 => {
    m_generate_water_box(t, sp, r, lr, 0, 1, 3, 0, 2, 4);
    pc = 832;
    }
    832 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::SOUTH as i32)) as usize]) { 862 } else { 847 };
    }
    847 => {
    m_generate_water_box(t, sp, r, lr, 11, 1, 0, 12, 2, 0);
    pc = 862;
    }
    862 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::NORTH as i32)) as usize]) { 894 } else { 877 };
    }
    877 => {
    m_generate_water_box(t, sp, r, lr, 11, 1, 7, 12, 2, 7);
    pc = 894;
    }
    894 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::EAST as i32)) as usize]) { 924 } else { 909 };
    }
    909 => {
    m_generate_water_box(t, sp, r, lr, 15, 1, 3, 15, 2, 4);
    pc = 924;
    }
    924 => {
    pc = if !(a10_OptionRoom.unwrap().open[((piece::SOUTH as i32)) as usize]) { 953 } else { 939 };
    }
    939 => {
    m_generate_water_box(t, sp, r, lr, 3, 5, 0, 4, 6, 0);
    pc = 953;
    }
    953 => {
    pc = if !(a10_OptionRoom.unwrap().open[((piece::NORTH as i32)) as usize]) { 984 } else { 968 };
    }
    968 => {
    m_generate_water_box(t, sp, r, lr, 3, 5, 7, 4, 6, 7);
    pc = 984;
    }
    984 => {
    pc = if !(a10_OptionRoom.unwrap().open[((piece::WEST as i32)) as usize]) { 1013 } else { 999 };
    }
    999 => {
    m_generate_water_box(t, sp, r, lr, 0, 5, 3, 0, 6, 4);
    pc = 1013;
    }
    1013 => {
    pc = if !(a11_OptionRoom.unwrap().open[((piece::SOUTH as i32)) as usize]) { 1044 } else { 1028 };
    }
    1028 => {
    m_generate_water_box(t, sp, r, lr, 11, 5, 0, 12, 6, 0);
    pc = 1044;
    }
    1044 => {
    pc = if !(a11_OptionRoom.unwrap().open[((piece::NORTH as i32)) as usize]) { 1077 } else { 1059 };
    }
    1059 => {
    m_generate_water_box(t, sp, r, lr, 11, 5, 7, 12, 6, 7);
    pc = 1077;
    }
    1077 => {
    pc = if !(a11_OptionRoom.unwrap().open[((piece::EAST as i32)) as usize]) { 1108 } else { 1092 };
    }
    1092 => {
    m_generate_water_box(t, sp, r, lr, 15, 5, 3, 15, 6, 4);
    pc = 1108;
    }
    1108 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_double_y_room(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_OptionRoom: Option<Room> = Default::default();
    let mut a9_OptionRoom: Option<Room> = Default::default();
    let mut a9_Room: Room = Default::default();
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut l12: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    pc = if (t.def().index / 25) <= 0 { 36 } else { 13 };
    }
    13 => {
    m_generate_default_floor(t, sp, r, lr, 0, 0, t.def().open[((piece::DOWN as i32)) as usize]);
    pc = 36;
    }
    36 => {
    a8_OptionRoom = t.conn(t.def().conn[((piece::UP as i32)) as usize]);
    pc = if t.conn(a8_OptionRoom.unwrap().conn[((piece::UP as i32)) as usize]).is_some() { 87 } else { 67 };
    }
    67 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 1, 8, 1, 6, 8, 6, state("prismarine", &[]));
    pc = 87;
    }
    87 => {
    sp.gen_box(0, 4, 0, 0, 4, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 4, 0, 7, 4, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 4, 0, 6, 4, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 4, 7, 6, 4, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(2, 4, 1, 2, 4, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 4, 2, 1, 4, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 4, 1, 5, 4, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 4, 2, 6, 4, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(2, 4, 5, 2, 4, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 4, 5, 1, 4, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 4, 5, 5, 4, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 4, 5, 6, 4, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    a9_Room = t.def();
    l10 = 1;
    pc = 350;
    }
    350 => {
    pc = if l10 > 5 { 996 } else { 356 };
    }
    356 => {
    l11 = 0;
    pc = if !(a9_Room.open[((piece::SOUTH as i32)) as usize]) { 457 } else { 374 };
    }
    374 => {
    sp.gen_box(2, l10, l11, 2, (l10 + 2), l11, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, l10, l11, 5, (l10 + 2), l11, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, (l10 + 2), l11, 4, (l10 + 2), l11, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 513;
    }
    457 => {
    sp.gen_box(0, l10, l11, 7, (l10 + 2), l11, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, (l10 + 1), l11, 7, (l10 + 1), l11, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    pc = 513;
    }
    513 => {
    l11 = 7;
    pc = if !(a9_Room.open[((piece::NORTH as i32)) as usize]) { 615 } else { 532 };
    }
    532 => {
    sp.gen_box(2, l10, l11, 2, (l10 + 2), l11, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, l10, l11, 5, (l10 + 2), l11, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, (l10 + 2), l11, 4, (l10 + 2), l11, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 671;
    }
    615 => {
    sp.gen_box(0, l10, l11, 7, (l10 + 2), l11, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, (l10 + 1), l11, 7, (l10 + 1), l11, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    pc = 671;
    }
    671 => {
    l12 = 0;
    pc = if !(a9_Room.open[((piece::WEST as i32)) as usize]) { 772 } else { 689 };
    }
    689 => {
    sp.gen_box(l12, l10, 2, l12, (l10 + 2), 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(l12, l10, 5, l12, (l10 + 2), 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(l12, (l10 + 2), 3, l12, (l10 + 2), 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 828;
    }
    772 => {
    sp.gen_box(l12, l10, 0, l12, (l10 + 2), 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(l12, (l10 + 1), 0, l12, (l10 + 1), 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    pc = 828;
    }
    828 => {
    l12 = 7;
    pc = if !(a9_Room.open[((piece::EAST as i32)) as usize]) { 930 } else { 847 };
    }
    847 => {
    sp.gen_box(l12, l10, 2, l12, (l10 + 2), 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(l12, l10, 5, l12, (l10 + 2), 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(l12, (l10 + 2), 3, l12, (l10 + 2), 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 986;
    }
    930 => {
    sp.gen_box(l12, l10, 0, l12, (l10 + 2), 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(l12, (l10 + 1), 0, l12, (l10 + 1), 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    pc = 986;
    }
    986 => {
    a9_OptionRoom = a8_OptionRoom;
    l10 += 4;
    pc = 350;
    }
    996 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_double_y_z_room(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a10_OptionRoom: Option<Room> = Default::default();
    let mut a11_OptionRoom: Option<Room> = Default::default();
    let mut a13_BlockState: BlockState = blocks::AIR;
    let mut a8_OptionRoom: Option<Room> = Default::default();
    let mut a9_Room: Room = Default::default();
    let mut l12: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    a8_OptionRoom = t.conn(t.def().conn[((piece::NORTH as i32)) as usize]);
    a9_Room = t.def();
    a10_OptionRoom = t.conn(a8_OptionRoom.unwrap().conn[((piece::UP as i32)) as usize]);
    a11_OptionRoom = t.conn(a9_Room.conn[((piece::UP as i32)) as usize]);
    pc = if (t.def().index / 25) <= 0 { 106 } else { 63 };
    }
    63 => {
    m_generate_default_floor(t, sp, r, lr, 0, 8, a8_OptionRoom.unwrap().open[((piece::DOWN as i32)) as usize]);
    m_generate_default_floor(t, sp, r, lr, 0, 0, a9_Room.open[((piece::DOWN as i32)) as usize]);
    pc = 106;
    }
    106 => {
    pc = if t.conn(a11_OptionRoom.unwrap().conn[((piece::UP as i32)) as usize]).is_some() { 141 } else { 121 };
    }
    121 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 1, 8, 1, 6, 8, 7, state("prismarine", &[]));
    pc = 141;
    }
    141 => {
    pc = if t.conn(a10_OptionRoom.unwrap().conn[((piece::UP as i32)) as usize]).is_some() { 177 } else { 156 };
    }
    156 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 1, 8, 8, 6, 8, 14, state("prismarine", &[]));
    pc = 177;
    }
    177 => {
    l12 = 1;
    pc = 180;
    }
    180 => {
    pc = if l12 > 7 { 304 } else { 187 };
    }
    187 => {
    a13_BlockState = state("prismarine_bricks", &[]);
    pc = if l12 == 2 { 205 } else { 198 };
    }
    198 => {
    pc = if l12 != 6 { 210 } else { 205 };
    }
    205 => {
    a13_BlockState = state("prismarine", &[]);
    pc = 210;
    }
    210 => {
    sp.gen_box(0, l12, 0, 0, l12, 15, a13_BlockState, a13_BlockState, (0 != 0));
    sp.gen_box(7, l12, 0, 7, l12, 15, a13_BlockState, a13_BlockState, (0 != 0));
    sp.gen_box(1, l12, 0, 6, l12, 0, a13_BlockState, a13_BlockState, (0 != 0));
    sp.gen_box(1, l12, 15, 6, l12, 15, a13_BlockState, a13_BlockState, (0 != 0));
    l12 += 1;
    pc = 180;
    }
    304 => {
    l12 = 1;
    pc = 307;
    }
    307 => {
    pc = if l12 > 7 { 365 } else { 314 };
    }
    314 => {
    a13_BlockState = state("dark_prismarine", &[]);
    pc = if l12 == 2 { 332 } else { 325 };
    }
    325 => {
    pc = if l12 != 6 { 337 } else { 332 };
    }
    332 => {
    a13_BlockState = state("sea_lantern", &[]);
    pc = 337;
    }
    337 => {
    sp.gen_box(3, l12, 7, 4, l12, 8, a13_BlockState, a13_BlockState, (0 != 0));
    l12 += 1;
    pc = 307;
    }
    365 => {
    pc = if !(a9_Room.open[((piece::SOUTH as i32)) as usize]) { 393 } else { 380 };
    }
    380 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 0, 4, 2, 0);
    pc = 393;
    }
    393 => {
    pc = if !(a9_Room.open[((piece::EAST as i32)) as usize]) { 423 } else { 408 };
    }
    408 => {
    m_generate_water_box(t, sp, r, lr, 7, 1, 3, 7, 2, 4);
    pc = 423;
    }
    423 => {
    pc = if !(a9_Room.open[((piece::WEST as i32)) as usize]) { 451 } else { 438 };
    }
    438 => {
    m_generate_water_box(t, sp, r, lr, 0, 1, 3, 0, 2, 4);
    pc = 451;
    }
    451 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::NORTH as i32)) as usize]) { 481 } else { 466 };
    }
    466 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 15, 4, 2, 15);
    pc = 481;
    }
    481 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::WEST as i32)) as usize]) { 511 } else { 496 };
    }
    496 => {
    m_generate_water_box(t, sp, r, lr, 0, 1, 11, 0, 2, 12);
    pc = 511;
    }
    511 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::EAST as i32)) as usize]) { 543 } else { 526 };
    }
    526 => {
    m_generate_water_box(t, sp, r, lr, 7, 1, 11, 7, 2, 12);
    pc = 543;
    }
    543 => {
    pc = if !(a11_OptionRoom.unwrap().open[((piece::SOUTH as i32)) as usize]) { 572 } else { 558 };
    }
    558 => {
    m_generate_water_box(t, sp, r, lr, 3, 5, 0, 4, 6, 0);
    pc = 572;
    }
    572 => {
    pc = if !(a11_OptionRoom.unwrap().open[((piece::EAST as i32)) as usize]) { 668 } else { 587 };
    }
    587 => {
    m_generate_water_box(t, sp, r, lr, 7, 5, 3, 7, 6, 4);
    sp.gen_box(5, 4, 2, 6, 4, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 2, 6, 3, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 5, 6, 3, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 668;
    }
    668 => {
    pc = if !(a11_OptionRoom.unwrap().open[((piece::WEST as i32)) as usize]) { 757 } else { 683 };
    }
    683 => {
    m_generate_water_box(t, sp, r, lr, 0, 5, 3, 0, 6, 4);
    sp.gen_box(1, 4, 2, 2, 4, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 2, 1, 3, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 5, 1, 3, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 757;
    }
    757 => {
    pc = if !(a10_OptionRoom.unwrap().open[((piece::NORTH as i32)) as usize]) { 788 } else { 772 };
    }
    772 => {
    m_generate_water_box(t, sp, r, lr, 3, 5, 15, 4, 6, 15);
    pc = 788;
    }
    788 => {
    pc = if !(a10_OptionRoom.unwrap().open[((piece::WEST as i32)) as usize]) { 885 } else { 803 };
    }
    803 => {
    m_generate_water_box(t, sp, r, lr, 0, 5, 11, 0, 6, 12);
    sp.gen_box(1, 4, 10, 2, 4, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 10, 1, 3, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 13, 1, 3, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 885;
    }
    885 => {
    pc = if !(a10_OptionRoom.unwrap().open[((piece::EAST as i32)) as usize]) { 989 } else { 900 };
    }
    900 => {
    m_generate_water_box(t, sp, r, lr, 7, 5, 11, 7, 6, 12);
    sp.gen_box(5, 4, 10, 6, 4, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 10, 6, 3, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 13, 6, 3, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 989;
    }
    989 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_double_z_room(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a8_OptionRoom: Option<Room> = Default::default();
    let mut a9_Room: Room = Default::default();
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    a8_OptionRoom = t.conn(t.def().conn[((piece::NORTH as i32)) as usize]);
    a9_Room = t.def();
    pc = if (t.def().index / 25) <= 0 { 78 } else { 35 };
    }
    35 => {
    m_generate_default_floor(t, sp, r, lr, 0, 8, a8_OptionRoom.unwrap().open[((piece::DOWN as i32)) as usize]);
    m_generate_default_floor(t, sp, r, lr, 0, 0, a9_Room.open[((piece::DOWN as i32)) as usize]);
    pc = 78;
    }
    78 => {
    pc = if t.conn(a9_Room.conn[((piece::UP as i32)) as usize]).is_some() { 111 } else { 93 };
    }
    93 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 1, 4, 1, 6, 4, 7, state("prismarine", &[]));
    pc = 111;
    }
    111 => {
    pc = if t.conn(a8_OptionRoom.unwrap().conn[((piece::UP as i32)) as usize]).is_some() { 145 } else { 126 };
    }
    126 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 1, 4, 8, 6, 4, 14, state("prismarine", &[]));
    pc = 145;
    }
    145 => {
    sp.gen_box(0, 3, 0, 0, 3, 15, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 3, 0, 7, 3, 15, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 0, 7, 3, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 15, 6, 3, 15, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 0, 2, 15, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(7, 2, 0, 7, 2, 15, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(1, 2, 0, 7, 2, 0, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(1, 2, 15, 6, 2, 15, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(0, 1, 0, 0, 1, 15, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 1, 0, 7, 1, 15, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 0, 7, 1, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 15, 6, 1, 15, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 1, 1, 1, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 1, 6, 1, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 1, 1, 3, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 3, 1, 6, 3, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 13, 1, 1, 14, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 13, 6, 1, 14, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 13, 1, 3, 14, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 3, 13, 6, 3, 14, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(2, 1, 6, 2, 3, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 1, 6, 5, 3, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(2, 1, 9, 2, 3, 9, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 1, 9, 5, 3, 9, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, 2, 6, 4, 2, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, 2, 9, 4, 2, 9, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(2, 2, 7, 2, 2, 8, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 2, 7, 5, 2, 8, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("sea_lantern", &[]), 2, 2, 5);
    sp.place(state("sea_lantern", &[]), 5, 2, 5);
    sp.place(state("sea_lantern", &[]), 2, 2, 10);
    sp.place(state("sea_lantern", &[]), 5, 2, 10);
    sp.place(state("prismarine_bricks", &[]), 2, 3, 5);
    sp.place(state("prismarine_bricks", &[]), 5, 3, 5);
    sp.place(state("prismarine_bricks", &[]), 2, 3, 10);
    sp.place(state("prismarine_bricks", &[]), 5, 3, 10);
    pc = if !(a9_Room.open[((piece::SOUTH as i32)) as usize]) { 897 } else { 884 };
    }
    884 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 0, 4, 2, 0);
    pc = 897;
    }
    897 => {
    pc = if !(a9_Room.open[((piece::EAST as i32)) as usize]) { 927 } else { 912 };
    }
    912 => {
    m_generate_water_box(t, sp, r, lr, 7, 1, 3, 7, 2, 4);
    pc = 927;
    }
    927 => {
    pc = if !(a9_Room.open[((piece::WEST as i32)) as usize]) { 955 } else { 942 };
    }
    942 => {
    m_generate_water_box(t, sp, r, lr, 0, 1, 3, 0, 2, 4);
    pc = 955;
    }
    955 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::NORTH as i32)) as usize]) { 985 } else { 970 };
    }
    970 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 15, 4, 2, 15);
    pc = 985;
    }
    985 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::WEST as i32)) as usize]) { 1015 } else { 1000 };
    }
    1000 => {
    m_generate_water_box(t, sp, r, lr, 0, 1, 11, 0, 2, 12);
    pc = 1015;
    }
    1015 => {
    pc = if !(a8_OptionRoom.unwrap().open[((piece::EAST as i32)) as usize]) { 1047 } else { 1030 };
    }
    1030 => {
    m_generate_water_box(t, sp, r, lr, 7, 1, 11, 7, 2, 12);
    pc = 1047;
    }
    1047 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_entry_room(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(0, 3, 0, 2, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 3, 0, 7, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 1, 2, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 2, 0, 7, 2, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 1, 0, 0, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 1, 0, 7, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 1, 7, 7, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 0, 2, 3, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 1, 0, 6, 3, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = if !(t.def().open[((piece::NORTH as i32)) as usize]) { 227 } else { 212 };
    }
    212 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 7, 4, 2, 7);
    pc = 227;
    }
    227 => {
    pc = if !(t.def().open[((piece::WEST as i32)) as usize]) { 257 } else { 244 };
    }
    244 => {
    m_generate_water_box(t, sp, r, lr, 0, 1, 3, 1, 2, 4);
    pc = 257;
    }
    257 => {
    pc = if !(t.def().open[((piece::EAST as i32)) as usize]) { 289 } else { 274 };
    }
    274 => {
    m_generate_water_box(t, sp, r, lr, 6, 1, 3, 7, 2, 4);
    pc = 289;
    }
    289 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_penthouse(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut l10: i32 = 0;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    sp.gen_box(2, -1, 2, 11, -1, 11, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, -1, 0, 1, -1, 11, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(12, -1, 0, 13, -1, 11, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(2, -1, 0, 11, -1, 1, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(2, -1, 12, 11, -1, 13, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(0, 0, 0, 0, 0, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(13, 0, 0, 13, 0, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 0, 0, 12, 0, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 0, 13, 12, 0, 13, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l8 = 2;
    pc = 201;
    }
    201 => {
    pc = if l8 > 11 { 257 } else { 208 };
    }
    208 => {
    sp.place(state("sea_lantern", &[]), 0, 0, l8);
    sp.place(state("sea_lantern", &[]), 13, 0, l8);
    sp.place(state("sea_lantern", &[]), l8, 0, 0);
    l8 += 3;
    pc = 201;
    }
    257 => {
    sp.gen_box(2, 0, 3, 4, 0, 9, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(9, 0, 3, 11, 0, 9, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(4, 0, 9, 9, 0, 11, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("prismarine_bricks", &[]), 5, 0, 8);
    sp.place(state("prismarine_bricks", &[]), 8, 0, 8);
    sp.place(state("prismarine_bricks", &[]), 10, 0, 10);
    sp.place(state("prismarine_bricks", &[]), 3, 0, 10);
    sp.gen_box(3, 0, 3, 3, 0, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(10, 0, 3, 10, 0, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(6, 0, 10, 7, 0, 10, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    l8 = 3;
    l9 = 0;
    pc = 456;
    }
    456 => {
    pc = if l9 >= 2 { 512 } else { 462 };
    }
    462 => {
    l10 = 2;
    pc = 465;
    }
    465 => {
    pc = if l10 > 8 { 502 } else { 472 };
    }
    472 => {
    sp.gen_box(l8, 0, l10, l8, 2, l10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l10 += 3;
    pc = 465;
    }
    502 => {
    l8 = 10;
    l9 += 1;
    pc = 456;
    }
    512 => {
    sp.gen_box(5, 0, 10, 5, 2, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(8, 0, 10, 8, 2, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, -1, 7, 7, -1, 8, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    m_generate_water_box(t, sp, r, lr, 6, -1, 3, 7, -1, 4);
    m_spawn_elder(t, sp, r, lr, 6, 1, 6);
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_simple_room(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut l8: i32 = 0;
    let mut sk0_i32: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    pc = if (t.def().index / 25) <= 0 { 36 } else { 13 };
    }
    13 => {
    m_generate_default_floor(t, sp, r, lr, 0, 0, t.def().open[((piece::DOWN as i32)) as usize]);
    pc = 36;
    }
    36 => {
    pc = if t.conn(t.def().conn[((piece::UP as i32)) as usize]).is_some() { 71 } else { 53 };
    }
    53 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 1, 4, 1, 6, 4, 6, state("prismarine", &[]));
    pc = 71;
    }
    71 => {
    pc = if t.main_design == 0 { 137 } else { 78 };
    }
    78 => {
    let t1 = r.next_bool();
    pc = if !(t1) { 137 } else { 88 };
    }
    88 => {
    pc = if t.def().open[((piece::DOWN as i32)) as usize] { 137 } else { 105 };
    }
    105 => {
    pc = if t.def().open[((piece::UP as i32)) as usize] { 137 } else { 122 };
    }
    122 => {
    let t2 = t.def().count_openings();
    pc = if t2 <= 1 { 137 } else { 133 };
    }
    133 => {
    sk0_i32 = 1;
    pc = 138;
    }
    137 => {
    sk0_i32 = 0;
    pc = 138;
    }
    138 => {
    l8 = sk0_i32;
    pc = if t.main_design != 0 { 962 } else { 147 };
    }
    147 => {
    sp.gen_box(0, 1, 0, 2, 1, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 3, 0, 2, 3, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 0, 2, 2, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(1, 2, 0, 2, 2, 0, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.place(state("sea_lantern", &[]), 1, 2, 1);
    sp.gen_box(5, 1, 0, 7, 1, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 3, 0, 7, 3, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 2, 0, 7, 2, 2, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(5, 2, 0, 6, 2, 0, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.place(state("sea_lantern", &[]), 6, 2, 1);
    sp.gen_box(0, 1, 5, 2, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 3, 5, 2, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 5, 0, 2, 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(1, 2, 7, 2, 2, 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.place(state("sea_lantern", &[]), 1, 2, 6);
    sp.gen_box(5, 1, 5, 7, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 3, 5, 7, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 2, 5, 7, 2, 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(5, 2, 7, 6, 2, 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.place(state("sea_lantern", &[]), 6, 2, 6);
    pc = if !(t.def().open[((piece::SOUTH as i32)) as usize]) { 583 } else { 560 };
    }
    560 => {
    sp.gen_box(3, 3, 0, 4, 3, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 643;
    }
    583 => {
    sp.gen_box(3, 3, 0, 4, 3, 1, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, 2, 0, 4, 2, 0, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(3, 1, 0, 4, 1, 1, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 643;
    }
    643 => {
    pc = if !(t.def().open[((piece::NORTH as i32)) as usize]) { 685 } else { 660 };
    }
    660 => {
    sp.gen_box(3, 3, 7, 4, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 751;
    }
    685 => {
    sp.gen_box(3, 3, 6, 4, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, 2, 7, 4, 2, 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(3, 1, 6, 4, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 751;
    }
    751 => {
    pc = if !(t.def().open[((piece::WEST as i32)) as usize]) { 791 } else { 768 };
    }
    768 => {
    sp.gen_box(0, 3, 3, 0, 3, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 851;
    }
    791 => {
    sp.gen_box(0, 3, 3, 1, 3, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 3, 0, 2, 4, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(0, 1, 3, 1, 1, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 851;
    }
    851 => {
    pc = if !(t.def().open[((piece::EAST as i32)) as usize]) { 893 } else { 868 };
    }
    868 => {
    sp.gen_box(7, 3, 3, 7, 3, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 2205;
    }
    893 => {
    sp.gen_box(6, 3, 3, 7, 3, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 2, 3, 7, 2, 4, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(6, 1, 3, 7, 1, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 2205;
    }
    962 => {
    pc = if t.main_design != 1 { 1725 } else { 970 };
    }
    970 => {
    sp.gen_box(2, 1, 2, 2, 3, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(2, 1, 5, 2, 3, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 1, 5, 5, 3, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(5, 1, 2, 5, 3, 2, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("sea_lantern", &[]), 2, 2, 2);
    sp.place(state("sea_lantern", &[]), 2, 2, 5);
    sp.place(state("sea_lantern", &[]), 5, 2, 5);
    sp.place(state("sea_lantern", &[]), 5, 2, 2);
    sp.gen_box(0, 1, 0, 1, 3, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 1, 1, 0, 3, 1, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 1, 7, 1, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 1, 6, 0, 3, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 7, 7, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 1, 6, 7, 3, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 1, 0, 7, 3, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 1, 1, 7, 3, 1, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("prismarine", &[]), 1, 2, 0);
    sp.place(state("prismarine", &[]), 0, 2, 1);
    sp.place(state("prismarine", &[]), 1, 2, 7);
    sp.place(state("prismarine", &[]), 0, 2, 6);
    sp.place(state("prismarine", &[]), 6, 2, 7);
    sp.place(state("prismarine", &[]), 7, 2, 6);
    sp.place(state("prismarine", &[]), 6, 2, 0);
    sp.place(state("prismarine", &[]), 7, 2, 1);
    pc = if t.def().open[((piece::SOUTH as i32)) as usize] { 1470 } else { 1407 };
    }
    1407 => {
    sp.gen_box(1, 3, 0, 6, 3, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 2, 0, 6, 2, 0, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(1, 1, 0, 6, 1, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 1470;
    }
    1470 => {
    pc = if t.def().open[((piece::NORTH as i32)) as usize] { 1556 } else { 1487 };
    }
    1487 => {
    sp.gen_box(1, 3, 7, 6, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 2, 7, 6, 2, 7, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(1, 1, 7, 6, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 1556;
    }
    1556 => {
    pc = if t.def().open[((piece::WEST as i32)) as usize] { 1636 } else { 1573 };
    }
    1573 => {
    sp.gen_box(0, 3, 1, 0, 3, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 1, 0, 2, 6, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(0, 1, 1, 0, 1, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 1636;
    }
    1636 => {
    pc = if t.def().open[((piece::EAST as i32)) as usize] { 2205 } else { 1653 };
    }
    1653 => {
    sp.gen_box(7, 3, 1, 7, 3, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 2, 1, 7, 2, 6, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(7, 1, 1, 7, 1, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 2205;
    }
    1725 => {
    pc = if t.main_design != 2 { 2205 } else { 1733 };
    }
    1733 => {
    sp.gen_box(0, 1, 0, 0, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 1, 0, 7, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 0, 6, 1, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 7, 6, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 0, 2, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(7, 2, 0, 7, 2, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(1, 2, 0, 6, 2, 0, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(1, 2, 7, 6, 2, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(0, 3, 0, 0, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 3, 0, 7, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 0, 6, 3, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 7, 6, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 1, 3, 0, 2, 4, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(7, 1, 3, 7, 2, 4, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(3, 1, 0, 4, 2, 0, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(3, 1, 7, 4, 2, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    pc = if !(t.def().open[((piece::SOUTH as i32)) as usize]) { 2111 } else { 2098 };
    }
    2098 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 0, 4, 2, 0);
    pc = 2111;
    }
    2111 => {
    pc = if !(t.def().open[((piece::NORTH as i32)) as usize]) { 2143 } else { 2128 };
    }
    2128 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 7, 4, 2, 7);
    pc = 2143;
    }
    2143 => {
    pc = if !(t.def().open[((piece::WEST as i32)) as usize]) { 2173 } else { 2160 };
    }
    2160 => {
    m_generate_water_box(t, sp, r, lr, 0, 1, 3, 0, 2, 4);
    pc = 2173;
    }
    2173 => {
    pc = if !(t.def().open[((piece::EAST as i32)) as usize]) { 2205 } else { 2190 };
    }
    2190 => {
    m_generate_water_box(t, sp, r, lr, 7, 1, 3, 7, 2, 4);
    pc = 2205;
    }
    2205 => {
    pc = if l8 == 0 { 2270 } else { 2210 };
    }
    2210 => {
    sp.gen_box(3, 1, 3, 4, 1, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(3, 2, 3, 4, 2, 4, state("prismarine", &[]), state("prismarine", &[]), (0 != 0));
    sp.gen_box(3, 3, 3, 4, 3, 4, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    pc = 2270;
    }
    2270 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_simple_top_room(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut a11_BlockState: BlockState = blocks::AIR;
    let mut l10: i32 = 0;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut sk0_i32: i32 = 0;
    let mut sk1_i32: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    pc = if (t.def().index / 25) <= 0 { 36 } else { 13 };
    }
    13 => {
    m_generate_default_floor(t, sp, r, lr, 0, 0, t.def().open[((piece::DOWN as i32)) as usize]);
    pc = 36;
    }
    36 => {
    pc = if t.conn(t.def().conn[((piece::UP as i32)) as usize]).is_some() { 71 } else { 53 };
    }
    53 => {
    m_generate_box_on_fill_only(t, sp, r, lr, 1, 4, 1, 6, 4, 6, state("prismarine", &[]));
    pc = 71;
    }
    71 => {
    l8 = 1;
    pc = 74;
    }
    74 => {
    pc = if l8 > 6 { 165 } else { 81 };
    }
    81 => {
    l9 = 1;
    pc = 84;
    }
    84 => {
    pc = if l9 > 6 { 159 } else { 91 };
    }
    91 => {
    let t1 = r.next_int_bound(3);
    pc = if t1 == 0 { 153 } else { 102 };
    }
    102 => {
    let t2 = r.next_int_bound(4);
    sk0_i32 = 2;
    pc = if t2 != 0 { 118 } else { 114 };
    }
    114 => {
    sk0_i32 = sk0_i32;
    sk1_i32 = 0;
    pc = 119;
    }
    118 => {
    sk0_i32 = sk0_i32;
    sk1_i32 = 1;
    pc = 119;
    }
    119 => {
    l10 = (sk0_i32 + sk1_i32);
    a11_BlockState = state("wet_sponge", &[]);
    sp.gen_box(l8, l10, l9, l8, 3, l9, a11_BlockState, a11_BlockState, (0 != 0));
    pc = 153;
    }
    153 => {
    l9 += 1;
    pc = 84;
    }
    159 => {
    l8 += 1;
    pc = 74;
    }
    165 => {
    sp.gen_box(0, 1, 0, 0, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 1, 0, 7, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 0, 6, 1, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 1, 7, 6, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 2, 0, 0, 2, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(7, 2, 0, 7, 2, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(1, 2, 0, 6, 2, 0, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(1, 2, 7, 6, 2, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(0, 3, 0, 0, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 3, 0, 7, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 0, 6, 3, 0, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(1, 3, 7, 6, 3, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(0, 1, 3, 0, 2, 4, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(7, 1, 3, 7, 2, 4, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(3, 1, 0, 4, 2, 0, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(3, 1, 7, 4, 2, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    pc = if !(t.def().open[((piece::SOUTH as i32)) as usize]) { 543 } else { 530 };
    }
    530 => {
    m_generate_water_box(t, sp, r, lr, 3, 1, 0, 4, 2, 0);
    pc = 543;
    }
    543 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
#[allow(unused_mut, unused_variables, unused_assignments, clippy::all)]
fn pp_wing_room(t: &MRoom, sp: &mut Sp, r: &mut dyn Rng, lr: &mut dyn Rng, a6: P) {
    let mut l10: i32 = 0;
    let mut l11: i32 = 0;
    let mut l8: i32 = 0;
    let mut l9: i32 = 0;
    let mut pc: u32 = 0;
    loop {
    match pc {
    0 => {
    pc = if t.main_design != 0 { 771 } else { 7 };
    }
    7 => {
    l8 = 0;
    pc = 10;
    }
    10 => {
    pc = if l8 >= 4 { 61 } else { 16 };
    }
    16 => {
    sp.gen_box((10 - l8), (3 - l8), (20 - l8), (12 + l8), (3 - l8), 20, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l8 += 1;
    pc = 10;
    }
    61 => {
    sp.gen_box(7, 0, 6, 15, 0, 16, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(6, 0, 6, 6, 3, 20, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(16, 0, 6, 16, 3, 20, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 1, 7, 7, 1, 20, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(15, 1, 7, 15, 1, 20, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(7, 1, 6, 9, 3, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(13, 1, 6, 15, 3, 6, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(8, 1, 7, 9, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(13, 1, 7, 14, 1, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(9, 0, 5, 13, 0, 5, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(10, 0, 7, 12, 0, 7, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(8, 0, 10, 8, 0, 12, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(14, 0, 10, 14, 0, 12, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    l8 = 18;
    pc = 375;
    }
    375 => {
    pc = if l8 < 7 { 418 } else { 382 };
    }
    382 => {
    sp.place(state("sea_lantern", &[]), 6, 3, l8);
    sp.place(state("sea_lantern", &[]), 16, 3, l8);
    l8 += -3;
    pc = 375;
    }
    418 => {
    sp.place(state("sea_lantern", &[]), 10, 0, 10);
    sp.place(state("sea_lantern", &[]), 12, 0, 10);
    sp.place(state("sea_lantern", &[]), 10, 0, 12);
    sp.place(state("sea_lantern", &[]), 12, 0, 12);
    sp.place(state("sea_lantern", &[]), 8, 3, 6);
    sp.place(state("sea_lantern", &[]), 14, 3, 6);
    sp.place(state("prismarine_bricks", &[]), 4, 2, 4);
    sp.place(state("sea_lantern", &[]), 4, 1, 4);
    sp.place(state("prismarine_bricks", &[]), 4, 0, 4);
    sp.place(state("prismarine_bricks", &[]), 18, 2, 4);
    sp.place(state("sea_lantern", &[]), 18, 1, 4);
    sp.place(state("prismarine_bricks", &[]), 18, 0, 4);
    sp.place(state("prismarine_bricks", &[]), 4, 2, 18);
    sp.place(state("sea_lantern", &[]), 4, 1, 18);
    sp.place(state("prismarine_bricks", &[]), 4, 0, 18);
    sp.place(state("prismarine_bricks", &[]), 18, 2, 18);
    sp.place(state("sea_lantern", &[]), 18, 1, 18);
    sp.place(state("prismarine_bricks", &[]), 18, 0, 18);
    sp.place(state("prismarine_bricks", &[]), 9, 7, 20);
    sp.place(state("prismarine_bricks", &[]), 13, 7, 20);
    sp.gen_box(6, 0, 21, 7, 4, 21, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(15, 0, 21, 16, 4, 21, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    m_spawn_elder(t, sp, r, lr, 11, 2, 16);
    pc = 1215;
    }
    771 => {
    pc = if t.main_design != 1 { 1215 } else { 779 };
    }
    779 => {
    sp.gen_box(9, 3, 18, 13, 3, 20, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(9, 0, 18, 9, 2, 18, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(13, 0, 18, 13, 2, 18, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l8 = 9;
    l9 = 20;
    l10 = 5;
    l11 = 0;
    pc = 865;
    }
    865 => {
    pc = if l11 >= 2 { 927 } else { 871 };
    }
    871 => {
    sp.place(state("prismarine_bricks", &[]), l8, 6, 20);
    sp.place(state("sea_lantern", &[]), l8, 5, 20);
    sp.place(state("prismarine_bricks", &[]), l8, 4, 20);
    l8 = 13;
    l11 += 1;
    pc = 865;
    }
    927 => {
    sp.gen_box(7, 3, 7, 15, 3, 14, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l8 = 10;
    l11 = 0;
    pc = 958;
    }
    958 => {
    pc = if l11 >= 2 { 1084 } else { 964 };
    }
    964 => {
    sp.gen_box(l8, 0, 10, l8, 6, 10, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(l8, 0, 12, l8, 6, 12, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.place(state("sea_lantern", &[]), l8, 0, 10);
    sp.place(state("sea_lantern", &[]), l8, 0, 12);
    sp.place(state("sea_lantern", &[]), l8, 4, 10);
    sp.place(state("sea_lantern", &[]), l8, 4, 12);
    l8 = 12;
    l11 += 1;
    pc = 958;
    }
    1084 => {
    l8 = 8;
    l11 = 0;
    pc = 1091;
    }
    1091 => {
    pc = if l11 >= 2 { 1155 } else { 1097 };
    }
    1097 => {
    sp.gen_box(l8, 0, 7, l8, 2, 7, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    sp.gen_box(l8, 0, 14, l8, 2, 14, state("prismarine_bricks", &[]), state("prismarine_bricks", &[]), (0 != 0));
    l8 = 14;
    l11 += 1;
    pc = 1091;
    }
    1155 => {
    sp.gen_box(8, 3, 8, 8, 3, 13, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    sp.gen_box(14, 3, 8, 14, 3, 13, state("dark_prismarine", &[]), state("dark_prismarine", &[]), (0 != 0));
    m_spawn_elder(t, sp, r, lr, 11, 5, 13);
    pc = 1215;
    }
    1215 => {
    return;
    }
    _ => unreachable!(),
    }
    }
}
