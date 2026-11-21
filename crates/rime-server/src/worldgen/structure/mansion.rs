//! Woodland mansion layout (`WoodlandMansionPieces`): `MansionGrid` and
//! `MansionPiecePlacer`, ported from the 26.1 bytecode. The result is a list
//! of `woodland_mansion/*` template placements.

use super::piece::{clockwise, opposite, EAST, NORTH, SOUTH, UP, WEST};
use super::template::{rot_add, zero_position_with_transform};
use super::P;
use crate::worldgen::random::Rng;

const CLEAR: i32 = 0;
const CORRIDOR: i32 = 1;
const ROOM: i32 = 2;
const START_ROOM: i32 = 3;
const TEST_ROOM: i32 = 4;
const BLOCKED: i32 = 5;
const ROOM_1X1: i32 = 0x10000;
const ROOM_1X2: i32 = 0x20000;
const ROOM_2X2: i32 = 0x40000;
const ROOM_ORIGIN: i32 = 0x100000;
const ROOM_DOOR: i32 = 0x200000;
const ROOM_STAIRS: i32 = 0x400000;
const ROOM_CORRIDOR: i32 = 0x800000;
const ROOM_TYPE: i32 = 0xF0000;
const ROOM_ID: i32 = 0xFFFF;

/// `Direction.Plane.HORIZONTAL` iteration order
const HORIZONTAL: [u8; 4] = [NORTH, EAST, SOUTH, WEST];

fn sx(d: u8) -> i32 {
    match d {
        WEST => -1,
        EAST => 1,
        _ => 0,
    }
}

fn sz(d: u8) -> i32 {
    match d {
        NORTH => -1,
        SOUTH => 1,
        _ => 0,
    }
}

fn ccw(d: u8) -> u8 {
    match d {
        NORTH => WEST,
        WEST => SOUTH,
        SOUTH => EAST,
        _ => NORTH,
    }
}

/// `Direction.from2DDataValue`
fn from_2d(i: i32) -> u8 {
    [SOUTH, WEST, NORTH, EAST][(i & 3) as usize]
}

/// `Rotation.rotate(Direction)` for a horizontal direction
fn rot_dir(rot: i32, d: u8) -> u8 {
    let mut d = d;
    for _ in 0..(rot & 3) {
        d = clockwise(d);
    }
    d
}

fn rel(p: P, d: u8, n: i32) -> P {
    [p[0] + sx(d) * n, p[1], p[2] + sz(d) * n]
}

fn above(p: P, n: i32) -> P {
    [p[0], p[1] + n, p[2]]
}

/// `BlockPos.rotate(Rotation)`
fn rotate_pos(p: P, rot: i32) -> P {
    match rot & 3 {
        1 => [-p[2], p[1], p[0]],
        2 => [-p[0], p[1], -p[2]],
        3 => [p[2], p[1], -p[0]],
        _ => p,
    }
}

/// `WoodlandMansionPieces.SimpleGrid`
#[derive(Clone)]
struct Grid {
    g: Vec<i32>,
    w: i32,
    h: i32,
    outside: i32,
}

impl Grid {
    fn new(w: i32, h: i32, outside: i32) -> Grid {
        Grid { g: vec![0; (w * h) as usize], w, h, outside }
    }
    fn set(&mut self, x: i32, y: i32, v: i32) {
        if x >= 0 && x < self.w && y >= 0 && y < self.h {
            self.g[(x * self.h + y) as usize] = v;
        }
    }
    fn set_box(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, v: i32) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.set(x, y, v);
            }
        }
    }
    fn get(&self, x: i32, y: i32) -> i32 {
        if x >= 0 && x < self.w && y >= 0 && y < self.h {
            self.g[(x * self.h + y) as usize]
        } else {
            self.outside
        }
    }
    fn setif(&mut self, x: i32, y: i32, if_: i32, v: i32) {
        if self.get(x, y) == if_ {
            self.set(x, y, v);
        }
    }
    fn edges_to(&self, x: i32, y: i32, v: i32) -> bool {
        self.get(x - 1, y) == v || self.get(x + 1, y) == v || self.get(x, y + 1) == v || self.get(x, y - 1) == v
    }
}

fn is_house(g: &Grid, x: i32, y: i32) -> bool {
    matches!(g.get(x, y), CORRIDOR | ROOM | START_ROOM | TEST_ROOM)
}

fn shuffle<T>(l: &mut [T], r: &mut dyn Rng) {
    super::jigsaw::shuffle(l, r);
}

/// `WoodlandMansionPieces.MansionGrid`
struct MansionGrid {
    base: Grid,
    third: Grid,
    floors: [Grid; 3],
    entrance_x: i32,
    entrance_y: i32,
}

impl MansionGrid {
    fn new(r: &mut dyn Rng) -> MansionGrid {
        let (ex, ey) = (7, 4);
        let mut base = Grid::new(11, 11, BLOCKED);
        base.set_box(ex, ey, ex + 1, ey + 1, START_ROOM);
        base.set_box(ex - 1, ey, ex - 1, ey + 1, ROOM);
        base.set_box(ex + 2, ey - 2, ex + 3, ey + 3, BLOCKED);
        base.set_box(ex + 1, ey - 2, ex + 1, ey - 1, CORRIDOR);
        base.set_box(ex + 1, ey + 2, ex + 1, ey + 3, CORRIDOR);
        base.set(ex - 1, ey - 1, CORRIDOR);
        base.set(ex - 1, ey + 2, CORRIDOR);
        base.set_box(0, 0, 11, 1, BLOCKED);
        base.set_box(0, 9, 11, 11, BLOCKED);
        recursive_corridor(&mut base, r, ex, ey - 2, WEST, 6);
        recursive_corridor(&mut base, r, ex, ey + 3, WEST, 6);
        recursive_corridor(&mut base, r, ex - 2, ey - 1, WEST, 3);
        recursive_corridor(&mut base, r, ex - 2, ey + 2, WEST, 3);
        while clean_edges(&mut base) {}
        let mut floors = [Grid::new(11, 11, BLOCKED), Grid::new(11, 11, BLOCKED), Grid::new(11, 11, BLOCKED)];
        identify_rooms(r, &base, &mut floors[0]);
        identify_rooms(r, &base, &mut floors[1]);
        floors[0].set_box(ex + 1, ey, ex + 1, ey + 1, ROOM_CORRIDOR);
        floors[1].set_box(ex + 1, ey, ex + 1, ey + 1, ROOM_CORRIDOR);
        let third = Grid::new(base.w, base.h, BLOCKED);
        let mut m = MansionGrid { base, third, floors, entrance_x: ex, entrance_y: ey };
        m.setup_third_floor(r);
        let third = m.third.clone();
        identify_rooms(r, &third, &mut m.floors[2]);
        m
    }

    fn is_room_id(&self, floor: usize, x: i32, y: i32, id: i32) -> bool {
        (self.floors[floor].get(x, y) & ROOM_ID) == id
    }

    fn room_1x2_direction(&self, x: i32, y: i32, floor: usize, id: i32) -> Option<u8> {
        HORIZONTAL.into_iter().find(|&d| self.is_room_id(floor, x + sx(d), y + sz(d), id))
    }

    fn setup_third_floor(&mut self, r: &mut dyn Rng) {
        let mut list: Vec<(i32, i32)> = Vec::new();
        for y in 0..self.third.h {
            for x in 0..self.third.w {
                let v = self.floors[1].get(x, y);
                if (v & ROOM_TYPE) == ROOM_1X2 && (v & ROOM_DOOR) == ROOM_DOOR {
                    list.push((x, y));
                }
            }
        }
        if list.is_empty() {
            let (w, h) = (self.third.w, self.third.h);
            self.third.set_box(0, 0, w, h, BLOCKED);
            return;
        }
        let t = list[r.next_int_bound(list.len() as i32) as usize];
        let v = self.floors[1].get(t.0, t.1);
        self.floors[1].set(t.0, t.1, v | ROOM_STAIRS);
        let d = self.room_1x2_direction(t.0, t.1, 1, v & ROOM_ID).expect("1x2 room direction");
        let (i, j) = (t.0 + sx(d), t.1 + sz(d));
        for y in 0..self.third.h {
            for x in 0..self.third.w {
                if !is_house(&self.base, x, y) {
                    self.third.set(x, y, BLOCKED);
                } else if x == t.0 && y == t.1 {
                    self.third.set(x, y, START_ROOM);
                } else if x == i && y == j {
                    self.third.set(x, y, START_ROOM);
                    self.floors[2].set(x, y, ROOM_CORRIDOR);
                }
            }
        }
        let mut dirs = Vec::new();
        for d in HORIZONTAL {
            if self.third.get(i + sx(d), j + sz(d)) == CLEAR {
                dirs.push(d);
            }
        }
        if dirs.is_empty() {
            let (w, h) = (self.third.w, self.third.h);
            self.third.set_box(0, 0, w, h, BLOCKED);
            self.floors[1].set(t.0, t.1, v);
            return;
        }
        let d2 = dirs[r.next_int_bound(dirs.len() as i32) as usize];
        recursive_corridor(&mut self.third, r, i + sx(d2), j + sz(d2), d2, 4);
        while clean_edges(&mut self.third) {}
    }
}

fn recursive_corridor(g: &mut Grid, r: &mut dyn Rng, x: i32, y: i32, dir: u8, depth: i32) {
    if depth <= 0 {
        return;
    }
    g.set(x, y, CORRIDOR);
    g.setif(x + sx(dir), y + sz(dir), CLEAR, CORRIDOR);
    for _ in 0..8 {
        let d = from_2d(r.next_int_bound(4));
        if d == opposite(dir) || (d == EAST && r.next_bool()) {
            continue;
        }
        let (j, k) = (x + sx(dir), y + sz(dir));
        if g.get(j + sx(d), k + sz(d)) == CLEAR && g.get(j + sx(d) * 2, k + sz(d) * 2) == CLEAR {
            recursive_corridor(g, r, x + sx(dir) + sx(d), y + sz(dir) + sz(d), d, depth - 1);
            break;
        }
    }
    let (cw, cc) = (clockwise(dir), ccw(dir));
    g.setif(x + sx(cw), y + sz(cw), CLEAR, ROOM);
    g.setif(x + sx(cc), y + sz(cc), CLEAR, ROOM);
    g.setif(x + sx(dir) + sx(cw), y + sz(dir) + sz(cw), CLEAR, ROOM);
    g.setif(x + sx(dir) + sx(cc), y + sz(dir) + sz(cc), CLEAR, ROOM);
    g.setif(x + sx(dir) * 2, y + sz(dir) * 2, CLEAR, ROOM);
    g.setif(x + sx(cw) * 2, y + sz(cw) * 2, CLEAR, ROOM);
    g.setif(x + sx(cc) * 2, y + sz(cc) * 2, CLEAR, ROOM);
}

fn clean_edges(g: &mut Grid) -> bool {
    let mut flag = false;
    for i in 0..g.h {
        for j in 0..g.w {
            if g.get(j, i) != CLEAR {
                continue;
            }
            let h = |x, y| is_house(g, x, y) as i32;
            let k = h(j + 1, i) + h(j - 1, i) + h(j, i + 1) + h(j, i - 1);
            if k >= 3 {
                g.set(j, i, ROOM);
                flag = true;
            } else if k == 2 {
                let l = h(j + 1, i + 1) + h(j - 1, i + 1) + h(j + 1, i - 1) + h(j - 1, i - 1);
                if l <= 1 {
                    g.set(j, i, ROOM);
                    flag = true;
                }
            }
        }
    }
    flag
}

fn identify_rooms(r: &mut dyn Rng, g: &Grid, rooms: &mut Grid) {
    let mut list: Vec<(i32, i32)> = Vec::new();
    for y in 0..g.h {
        for x in 0..g.w {
            if g.get(x, y) == ROOM {
                list.push((x, y));
            }
        }
    }
    shuffle(&mut list, r);
    let mut id = 10;
    for (x, y) in list {
        if rooms.get(x, y) == 0 {
            let (mut x0, mut x1, mut y0, mut y1) = (x, x, y, y);
            let mut kind = ROOM_1X1;
            let free = |rooms: &Grid, a: i32, b: i32| rooms.get(a, b) == 0 && g.get(a, b) == ROOM;
            if free(rooms, x + 1, y) && free(rooms, x, y + 1) && free(rooms, x + 1, y + 1) {
                x1 += 1;
                y1 += 1;
                kind = ROOM_2X2;
            } else if free(rooms, x - 1, y) && free(rooms, x, y + 1) && free(rooms, x - 1, y + 1) {
                x0 -= 1;
                y1 += 1;
                kind = ROOM_2X2;
            } else if free(rooms, x - 1, y) && free(rooms, x, y - 1) && free(rooms, x - 1, y - 1) {
                x0 -= 1;
                y0 -= 1;
                kind = ROOM_2X2;
            } else if free(rooms, x + 1, y) {
                x1 += 1;
                kind = ROOM_1X2;
            } else if free(rooms, x, y + 1) {
                y1 += 1;
                kind = ROOM_1X2;
            } else if free(rooms, x - 1, y) {
                x0 -= 1;
                kind = ROOM_1X2;
            } else if free(rooms, x, y - 1) {
                y0 -= 1;
                kind = ROOM_1X2;
            }
            let mut ox = if r.next_bool() { x0 } else { x1 };
            let mut oy = if r.next_bool() { y0 } else { y1 };
            let mut door = ROOM_DOOR;
            if !g.edges_to(ox, oy, CORRIDOR) {
                ox = if ox == x0 { x1 } else { x0 };
                oy = if oy == y0 { y1 } else { y0 };
                if !g.edges_to(ox, oy, CORRIDOR) {
                    oy = if oy == y0 { y1 } else { y0 };
                    if !g.edges_to(ox, oy, CORRIDOR) {
                        ox = if ox == x0 { x1 } else { x0 };
                        oy = if oy == y0 { y1 } else { y0 };
                        if !g.edges_to(ox, oy, CORRIDOR) {
                            door = 0;
                            ox = x0;
                            oy = y0;
                        }
                    }
                }
            }
            for yy in y0..=y1 {
                for xx in x0..=x1 {
                    if xx == ox && yy == oy {
                        rooms.set(xx, yy, ROOM_ORIGIN | door | kind | id);
                    } else {
                        rooms.set(xx, yy, kind | id);
                    }
                }
            }
            id += 1;
        }
    }
}

// ------------------------------------------------------------ MansionPiecePlacer

/// One `WoodlandMansionPiece`: (template name, position, rotation, mirror)
pub type MPiece = (String, P, i32, i32);

struct Placement {
    rot: i32,
    pos: P,
    wall: &'static str,
}

#[derive(Clone, Copy)]
enum Floor {
    First,
    Second,
}

impl Floor {
    fn r1x1(self, r: &mut dyn Rng) -> String {
        match self {
            Floor::First => format!("1x1_a{}", r.next_int_bound(5) + 1),
            Floor::Second => format!("1x1_b{}", r.next_int_bound(5) + 1),
        }
    }
    fn r1x1_secret(self, r: &mut dyn Rng) -> String {
        format!("1x1_as{}", r.next_int_bound(4) + 1)
    }
    fn side(self, r: &mut dyn Rng, stairs: bool) -> String {
        match self {
            Floor::First => format!("1x2_a{}", r.next_int_bound(9) + 1),
            Floor::Second if stairs => "1x2_c_stairs".into(),
            Floor::Second => format!("1x2_c{}", r.next_int_bound(4) + 1),
        }
    }
    fn front(self, r: &mut dyn Rng, stairs: bool) -> String {
        match self {
            Floor::First => format!("1x2_b{}", r.next_int_bound(5) + 1),
            Floor::Second if stairs => "1x2_d_stairs".into(),
            Floor::Second => format!("1x2_d{}", r.next_int_bound(5) + 1),
        }
    }
    fn r1x2_secret(self, r: &mut dyn Rng) -> String {
        match self {
            Floor::First => format!("1x2_s{}", r.next_int_bound(2) + 1),
            Floor::Second => format!("1x2_se{}", r.next_int_bound(1) + 1),
        }
    }
    fn r2x2(self, r: &mut dyn Rng) -> String {
        match self {
            Floor::First => format!("2x2_a{}", r.next_int_bound(4) + 1),
            Floor::Second => format!("2x2_b{}", r.next_int_bound(5) + 1),
        }
    }
    fn r2x2_secret(self) -> String {
        "2x2_s1".into()
    }
}

struct Placer<'r> {
    r: &'r mut dyn Rng,
    start_x: i32,
    start_y: i32,
    out: Vec<MPiece>,
}

const NONE: i32 = 0;
const CW90: i32 = 1;
const CW180: i32 = 2;
const CCW90: i32 = 3;
const LEFT_RIGHT: i32 = 1;
const FRONT_BACK: i32 = 2;

impl Placer<'_> {
    fn add(&mut self, name: &str, pos: P, rot: i32) {
        self.out.push((name.to_string(), pos, rot, 0));
    }
    fn add_m(&mut self, name: &str, pos: P, rot: i32, mirror: i32) {
        self.out.push((name.to_string(), pos, rot, mirror));
    }

    fn cell(&self, base: P, rot: i32, x: i32, y: i32, dx: i32) -> P {
        let p = rel(base, rot_dir(rot, SOUTH), 8 + (y - self.start_y) * 8);
        rel(p, rot_dir(rot, EAST), dx + (x - self.start_x) * 8)
    }

    fn create(&mut self, pos: P, rot: i32, grid: &MansionGrid) {
        let mut d5 = Placement { rot, pos, wall: "wall_flat" };
        self.entrance(&mut d5);
        let mut d6 = Placement { rot: d5.rot, pos: above(d5.pos, 8), wall: "wall_window" };
        let g7 = &grid.base;
        let g8 = &grid.third;
        self.start_x = grid.entrance_x + 1;
        self.start_y = grid.entrance_y + 1;
        let (i9, i10) = (grid.entrance_x + 1, grid.entrance_y);
        let (sx0, sy0) = (self.start_x, self.start_y);
        self.traverse_outer_walls(&mut d5, g7, SOUTH, sx0, sy0, i9, i10);
        self.traverse_outer_walls(&mut d6, g7, SOUTH, sx0, sy0, i9, i10);
        let mut d11 = Placement { rot: d5.rot, pos: above(d5.pos, 19), wall: "wall_window" };
        let mut flag = false;
        let mut y = 0;
        while y < g8.h && !flag {
            let mut x = g8.w - 1;
            while x >= 0 && !flag {
                if is_house(g8, x, y) {
                    d11.pos = rel(d11.pos, rot_dir(rot, SOUTH), 8 + (y - self.start_y) * 8);
                    d11.pos = rel(d11.pos, rot_dir(rot, EAST), (x - self.start_x) * 8);
                    self.traverse_wall_piece(&mut d11);
                    self.traverse_outer_walls(&mut d11, g8, SOUTH, x, y, x, y);
                    flag = true;
                }
                x -= 1;
            }
            y += 1;
        }
        self.create_roof(above(pos, 16), rot, g7, Some(g8));
        self.create_roof(above(pos, 27), rot, g8, None);
        let colls = [Floor::First, Floor::Second, Floor::Second];
        for floor in 0..3usize {
            let p15 = above(pos, 8 * floor as i32 + if floor == 2 { 3 } else { 0 });
            let g16 = &grid.floors[floor];
            let g17 = if floor == 2 { g8 } else { g7 };
            let s18 = if floor == 0 { "carpet_south_1" } else { "carpet_south_2" };
            let s19 = if floor == 0 { "carpet_west_1" } else { "carpet_west_2" };
            let corr = |x: i32, y: i32| g17.get(x, y) == CORRIDOR || (g16.get(x, y) & ROOM_CORRIDOR) == ROOM_CORRIDOR;
            for y in 0..g17.h {
                for x in 0..g17.w {
                    if g17.get(x, y) != CORRIDOR {
                        continue;
                    }
                    let p22 = self.cell(p15, rot, x, y, 0);
                    self.add("corridor_floor", p22, rot);
                    if corr(x, y - 1) {
                        self.add("carpet_north", above(rel(p22, rot_dir(rot, EAST), 1), 1), rot);
                    }
                    if corr(x + 1, y) {
                        self.add("carpet_east", above(rel(rel(p22, rot_dir(rot, SOUTH), 1), rot_dir(rot, EAST), 5), 1), rot);
                    }
                    if corr(x, y + 1) {
                        self.add(s18, rel(rel(p22, rot_dir(rot, SOUTH), 5), rot_dir(rot, WEST), 1), rot);
                    }
                    if corr(x - 1, y) {
                        self.add(s19, rel(rel(p22, rot_dir(rot, WEST), 1), rot_dir(rot, NORTH), 1), rot);
                    }
                }
            }
            let s20 = if floor == 0 { "indoors_wall_1" } else { "indoors_wall_2" };
            let s21 = if floor == 0 { "indoors_door_1" } else { "indoors_door_2" };
            let coll = colls[floor];
            for y in 0..g17.h {
                for x in 0..g17.w {
                    let mut flag25 = floor == 2 && g17.get(x, y) == START_ROOM;
                    if !(g17.get(x, y) == ROOM || flag25) {
                        continue;
                    }
                    let i26 = g16.get(x, y);
                    let i27 = i26 & ROOM_TYPE;
                    let i28 = i26 & ROOM_ID;
                    flag25 = flag25 && (i26 & ROOM_CORRIDOR) == ROOM_CORRIDOR;
                    let mut dirs: Vec<u8> = Vec::new();
                    if (i26 & ROOM_DOOR) == ROOM_DOOR {
                        for d in HORIZONTAL {
                            if g17.get(x + sx(d), y + sz(d)) == CORRIDOR {
                                dirs.push(d);
                            }
                        }
                    }
                    let mut d29: Option<u8> = None;
                    if !dirs.is_empty() {
                        d29 = Some(dirs[self.r.next_int_bound(dirs.len() as i32) as usize]);
                    } else if (i26 & ROOM_ORIGIN) == ROOM_ORIGIN {
                        d29 = Some(UP);
                    }
                    let p30 = self.cell(p15, rot, x, y, -1);
                    let wall = |d: u8| if d29 == Some(d) { s21 } else { s20 };
                    if is_house(g17, x - 1, y) && !grid.is_room_id(floor, x - 1, y, i28) {
                        self.add(wall(WEST), p30, rot);
                    }
                    if g17.get(x + 1, y) == CORRIDOR && !flag25 {
                        self.add(wall(EAST), rel(p30, rot_dir(rot, EAST), 8), rot);
                    }
                    if is_house(g17, x, y + 1) && !grid.is_room_id(floor, x, y + 1, i28) {
                        let p = rel(rel(p30, rot_dir(rot, SOUTH), 7), rot_dir(rot, EAST), 7);
                        self.add(wall(SOUTH), p, rot_add(rot, CW90));
                    }
                    if g17.get(x, y - 1) == CORRIDOR && !flag25 {
                        let p = rel(rel(p30, rot_dir(rot, NORTH), 1), rot_dir(rot, EAST), 7);
                        self.add(wall(NORTH), p, rot_add(rot, CW90));
                    }
                    if i27 == ROOM_1X1 {
                        self.add_room_1x1(p30, rot, d29, coll);
                    } else if i27 == ROOM_1X2 && d29.is_some() {
                        let d31 = grid.room_1x2_direction(x, y, floor, i28);
                        let stairs = (i26 & ROOM_STAIRS) == ROOM_STAIRS;
                        self.add_room_1x2(p30, rot, d31, d29.unwrap(), coll, stairs);
                    } else if i27 == ROOM_2X2 && d29.is_some() && d29 != Some(UP) {
                        let d = d29.unwrap();
                        let mut d31 = clockwise(d);
                        if !grid.is_room_id(floor, x + sx(d31), y + sz(d31), i28) {
                            d31 = opposite(d31);
                        }
                        self.add_room_2x2(p30, rot, d31, d, coll);
                    } else if i27 == ROOM_2X2 && d29 == Some(UP) {
                        self.add_room_2x2_secret(p30, rot, coll);
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn traverse_outer_walls(&mut self, data: &mut Placement, g: &Grid, dir: u8, sx0: i32, sy0: i32, ex: i32, ey: i32) {
        let (mut i, mut j) = (sx0, sy0);
        let start = dir;
        let mut dir = dir;
        loop {
            if !is_house(g, i + sx(dir), j + sz(dir)) {
                self.traverse_turn(data);
                dir = clockwise(dir);
                if i != ex || j != ey || start != dir {
                    self.traverse_wall_piece(data);
                }
            } else if is_house(g, i + sx(dir), j + sz(dir)) && is_house(g, i + sx(dir) + sx(ccw(dir)), j + sz(dir) + sz(ccw(dir))) {
                self.traverse_inner_turn(data);
                i += sx(dir);
                j += sz(dir);
                dir = ccw(dir);
            } else {
                i += sx(dir);
                j += sz(dir);
                if i != ex || j != ey || start != dir {
                    self.traverse_wall_piece(data);
                }
            }
            if !(i != ex || j != ey || start != dir) {
                break;
            }
        }
    }

    fn create_roof(&mut self, pos: P, rot: i32, g4: &Grid, g5: Option<&Grid>) {
        let e = rot_dir(rot, EAST);
        let s = rot_dir(rot, SOUTH);
        let w = rot_dir(rot, WEST);
        let n = rot_dir(rot, NORTH);
        let h = |g: &Grid, x, y| is_house(g, x, y);
        for y in 0..g4.h {
            for x in 0..g4.w {
                let p8 = self.cell(pos, rot, x, y, 0);
                let up = g5.is_some_and(|g| h(g, x, y));
                if h(g4, x, y) && !up {
                    self.add("roof", above(p8, 3), rot);
                    if !h(g4, x + 1, y) {
                        self.add("roof_front", rel(p8, e, 6), rot);
                    }
                    if !h(g4, x - 1, y) {
                        self.add("roof_front", rel(rel(p8, e, 0), s, 7), rot_add(rot, CW180));
                    }
                    if !h(g4, x, y - 1) {
                        self.add("roof_front", rel(p8, w, 1), rot_add(rot, CCW90));
                    }
                    if !h(g4, x, y + 1) {
                        self.add("roof_front", rel(rel(p8, e, 6), s, 6), rot_add(rot, CW90));
                    }
                }
            }
        }
        if let Some(g5) = g5 {
            for y in 0..g4.h {
                for x in 0..g4.w {
                    let p8 = self.cell(pos, rot, x, y, 0);
                    let up = h(g5, x, y);
                    if !(h(g4, x, y) && up) {
                        continue;
                    }
                    if !h(g4, x + 1, y) {
                        self.add("small_wall", rel(p8, e, 7), rot);
                    }
                    if !h(g4, x - 1, y) {
                        self.add("small_wall", rel(rel(p8, w, 1), s, 6), rot_add(rot, CW180));
                    }
                    if !h(g4, x, y - 1) {
                        self.add("small_wall", rel(rel(p8, w, 0), n, 1), rot_add(rot, CCW90));
                    }
                    if !h(g4, x, y + 1) {
                        self.add("small_wall", rel(rel(p8, e, 6), s, 7), rot_add(rot, CW90));
                    }
                    if !h(g4, x + 1, y) {
                        if !h(g4, x, y - 1) {
                            self.add("small_wall_corner", rel(rel(p8, e, 7), n, 2), rot);
                        }
                        if !h(g4, x, y + 1) {
                            self.add("small_wall_corner", rel(rel(p8, e, 8), s, 7), rot_add(rot, CW90));
                        }
                    }
                    if !h(g4, x - 1, y) {
                        if !h(g4, x, y - 1) {
                            self.add("small_wall_corner", rel(rel(p8, w, 2), n, 1), rot_add(rot, CCW90));
                        }
                        if !h(g4, x, y + 1) {
                            self.add("small_wall_corner", rel(rel(p8, w, 1), s, 8), rot_add(rot, CW180));
                        }
                    }
                }
            }
        }
        for y in 0..g4.h {
            for x in 0..g4.w {
                let p8 = self.cell(pos, rot, x, y, 0);
                let up = g5.is_some_and(|g| h(g, x, y));
                if !h(g4, x, y) || up {
                    continue;
                }
                if !h(g4, x + 1, y) {
                    let p10 = rel(p8, e, 6);
                    if !h(g4, x, y + 1) {
                        self.add("roof_corner", rel(p10, s, 6), rot);
                    } else if h(g4, x + 1, y + 1) {
                        self.add("roof_inner_corner", rel(p10, s, 5), rot);
                    }
                    if !h(g4, x, y - 1) {
                        self.add("roof_corner", p10, rot_add(rot, CCW90));
                    } else if h(g4, x + 1, y - 1) {
                        self.add("roof_inner_corner", rel(rel(p8, e, 9), n, 2), rot_add(rot, CW90));
                    }
                }
                if !h(g4, x - 1, y) {
                    let p10 = rel(rel(p8, e, 0), s, 0);
                    if !h(g4, x, y + 1) {
                        self.add("roof_corner", rel(p10, s, 6), rot_add(rot, CW90));
                    } else if h(g4, x - 1, y + 1) {
                        self.add("roof_inner_corner", rel(rel(p10, s, 8), w, 3), rot_add(rot, CCW90));
                    }
                    if !h(g4, x, y - 1) {
                        self.add("roof_corner", p10, rot_add(rot, CW180));
                    } else if h(g4, x - 1, y - 1) {
                        self.add("roof_inner_corner", rel(p10, s, 1), rot_add(rot, CW180));
                    }
                }
            }
        }
    }

    fn entrance(&mut self, d: &mut Placement) {
        let w = rot_dir(d.rot, WEST);
        self.add("entrance", rel(d.pos, w, 9), d.rot);
        d.pos = rel(d.pos, rot_dir(d.rot, SOUTH), 16);
    }

    fn traverse_wall_piece(&mut self, d: &mut Placement) {
        self.add(d.wall, rel(d.pos, rot_dir(d.rot, EAST), 7), d.rot);
        d.pos = rel(d.pos, rot_dir(d.rot, SOUTH), 8);
    }

    fn traverse_turn(&mut self, d: &mut Placement) {
        d.pos = rel(d.pos, rot_dir(d.rot, SOUTH), -1);
        self.add("wall_corner", d.pos, d.rot);
        d.pos = rel(d.pos, rot_dir(d.rot, SOUTH), -7);
        d.pos = rel(d.pos, rot_dir(d.rot, WEST), -6);
        d.rot = rot_add(d.rot, CW90);
    }

    fn traverse_inner_turn(&mut self, d: &mut Placement) {
        d.pos = rel(d.pos, rot_dir(d.rot, SOUTH), 6);
        d.pos = rel(d.pos, rot_dir(d.rot, EAST), 8);
        d.rot = rot_add(d.rot, CCW90);
    }

    fn add_room_1x1(&mut self, pos: P, rot: i32, dir: Option<u8>, coll: Floor) {
        let mut r6 = NONE;
        let mut s7 = coll.r1x1(self.r);
        match dir {
            Some(EAST) => {}
            Some(NORTH) => r6 = rot_add(r6, CCW90),
            Some(WEST) => r6 = rot_add(r6, CW180),
            Some(SOUTH) => r6 = rot_add(r6, CW90),
            _ => s7 = coll.r1x1_secret(self.r),
        }
        let p8 = zero_position_with_transform([1, 0, 0], 0, r6, 7, 7);
        let r6 = rot_add(r6, rot);
        let p8 = rotate_pos(p8, rot);
        let p9 = [pos[0] + p8[0], pos[1], pos[2] + p8[2]];
        self.add(&s7, p9, r6);
    }

    fn add_room_1x2(&mut self, pos: P, rot: i32, d4: Option<u8>, d5: u8, coll: Floor, stairs: bool) {
        let e = rot_dir(rot, EAST);
        let s = rot_dir(rot, SOUTH);
        let n = rot_dir(rot, NORTH);
        let w = rot_dir(rot, WEST);
        let Some(d4) = d4 else { return };
        match (d5, d4) {
            (EAST, SOUTH) => {
                let name = coll.side(self.r, stairs);
                self.add(&name, rel(pos, e, 1), rot);
            }
            (EAST, NORTH) => {
                let name = coll.side(self.r, stairs);
                self.add_m(&name, rel(rel(pos, e, 1), s, 6), rot, LEFT_RIGHT);
            }
            (WEST, NORTH) => {
                let name = coll.side(self.r, stairs);
                self.add(&name, rel(rel(pos, e, 7), s, 6), rot_add(rot, CW180));
            }
            (WEST, SOUTH) => {
                let name = coll.side(self.r, stairs);
                self.add_m(&name, rel(pos, e, 7), rot, FRONT_BACK);
            }
            (SOUTH, EAST) => {
                let name = coll.side(self.r, stairs);
                self.add_m(&name, rel(pos, e, 1), rot_add(rot, CW90), LEFT_RIGHT);
            }
            (SOUTH, WEST) => {
                let name = coll.side(self.r, stairs);
                self.add(&name, rel(pos, e, 7), rot_add(rot, CW90));
            }
            (NORTH, WEST) => {
                let name = coll.side(self.r, stairs);
                self.add_m(&name, rel(rel(pos, e, 7), s, 6), rot_add(rot, CW90), FRONT_BACK);
            }
            (NORTH, EAST) => {
                let name = coll.side(self.r, stairs);
                self.add(&name, rel(rel(pos, e, 1), s, 6), rot_add(rot, CCW90));
            }
            (SOUTH, NORTH) => {
                let name = coll.front(self.r, stairs);
                self.add(&name, rel(rel(pos, e, 1), n, 8), rot);
            }
            (NORTH, SOUTH) => {
                let name = coll.front(self.r, stairs);
                self.add(&name, rel(rel(pos, e, 7), s, 14), rot_add(rot, CW180));
            }
            (WEST, EAST) => {
                let name = coll.front(self.r, stairs);
                self.add(&name, rel(pos, e, 15), rot_add(rot, CW90));
            }
            (EAST, WEST) => {
                let name = coll.front(self.r, stairs);
                self.add(&name, rel(rel(pos, w, 7), s, 6), rot_add(rot, CCW90));
            }
            (UP, EAST) => {
                let name = coll.r1x2_secret(self.r);
                self.add(&name, rel(pos, e, 15), rot_add(rot, CW90));
            }
            (UP, SOUTH) => {
                let name = coll.r1x2_secret(self.r);
                self.add(&name, rel(rel(pos, e, 1), n, 0), rot);
            }
            _ => {}
        }
    }

    fn add_room_2x2(&mut self, pos: P, rot: i32, d4: u8, d5: u8, coll: Floor) {
        let (mut i7, mut i8, mut r9, mut m10) = (0, 0, rot, NONE);
        match (d5, d4) {
            (EAST, SOUTH) => i7 = -7,
            (EAST, NORTH) => {
                i7 = -7;
                i8 = 6;
                m10 = LEFT_RIGHT;
            }
            (NORTH, EAST) => {
                i7 = 1;
                i8 = 14;
                r9 = rot_add(rot, CCW90);
            }
            (NORTH, WEST) => {
                i7 = 7;
                i8 = 14;
                r9 = rot_add(rot, CCW90);
                m10 = LEFT_RIGHT;
            }
            (SOUTH, WEST) => {
                i7 = 7;
                i8 = -8;
                r9 = rot_add(rot, CW90);
            }
            (SOUTH, EAST) => {
                i7 = 1;
                i8 = -8;
                r9 = rot_add(rot, CW90);
                m10 = LEFT_RIGHT;
            }
            (WEST, NORTH) => {
                i7 = 15;
                i8 = 6;
                r9 = rot_add(rot, CW180);
            }
            (WEST, SOUTH) => {
                i7 = 15;
                m10 = FRONT_BACK;
            }
            _ => {}
        }
        let p = rel(rel(pos, rot_dir(rot, EAST), i7), rot_dir(rot, SOUTH), i8);
        let name = coll.r2x2(self.r);
        self.add_m(&name, p, r9, m10);
    }

    fn add_room_2x2_secret(&mut self, pos: P, rot: i32, coll: Floor) {
        let p = rel(pos, rot_dir(rot, EAST), 1);
        self.add_m(&coll.r2x2_secret(), p, rot, NONE);
    }
}

/// `WoodlandMansionPieces.generateMansion`
pub fn generate(pos: P, rot: i32, r: &mut dyn Rng) -> Vec<MPiece> {
    let grid = MansionGrid::new(r);
    let mut p = Placer { r, start_x: 0, start_y: 0, out: Vec::new() };
    p.create(pos, rot, &grid);
    p.out
}
