//! Simple server-simulated entities: dropped items and falling blocks.

use crate::data::Stack;
use crate::registry::BlockState;

#[derive(Clone, Debug)]
pub enum EKind {
    Item { stack: Stack, pickup_delay: u32 },
    Falling { state: BlockState },
    Mob(Box<super::mob::Mob>),
    /// arrows, tridents and thrown items
    Projectile(Box<super::projectile::Projectile>),
    /// `AreaEffectCloud`
    Cloud(Box<super::projectile::Cloud>),
    /// `LightningBolt`
    Lightning { life: i32, flashes: i32, cause: Option<i32>, visual: bool, hit: Vec<i32> },
}

#[derive(Clone, Debug)]
pub struct Entity {
    pub eid: i32,
    pub uuid: uuid::Uuid,
    pub kind: EKind,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub on_ground: bool,
    pub age: u32,
    /// last position sent to clients
    pub synced: [f64; 3],
}

impl Entity {
    pub fn chunk(&self) -> (i32, i32) {
        ((self.pos[0].floor() as i32) >> 4, (self.pos[2].floor() as i32) >> 4)
    }

    /// (half width, height)
    pub fn size(&self) -> (f64, f64) {
        match &self.kind {
            EKind::Item { .. } => (0.125, 0.25),
            EKind::Falling { .. } => (0.49, 0.98),
            EKind::Mob(m) => {
                let (w, h) = m.ty.size();
                (w / 2.0, h)
            }
            EKind::Projectile(p) => {
                let (w, h) = p.ty.size();
                (w / 2.0, h)
            }
            EKind::Cloud(c) => (c.radius as f64, 0.5),
            EKind::Lightning { .. } => (0.0, 0.0),
        }
    }
}

/// What the physics step needs to know about a block position.
#[derive(Clone, Copy, PartialEq)]
pub enum Cell {
    Solid,
    Water,
    Open,
    /// not loaded: treated as solid so nothing falls out of the world
    Unknown,
}

/// Move with block collision (axis separated: y, x, z). Returns
/// (hit ground, hit horizontally).
pub fn move_entity(pos: &mut [f64; 3], vel: &mut [f64; 3], half: f64, h: f64, cell: &dyn Fn(i32, i32, i32) -> Cell) -> (bool, bool) {
    let blocked = |p: &[f64; 3]| {
        let (x0, x1) = ((p[0] - half).floor() as i32, (p[0] + half - 1e-7).floor() as i32);
        let (y0, y1) = (p[1].floor() as i32, (p[1] + h - 1e-7).floor() as i32);
        let (z0, z1) = ((p[2] - half).floor() as i32, (p[2] + half - 1e-7).floor() as i32);
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    if matches!(cell(x, y, z), Cell::Solid | Cell::Unknown) {
                        return true;
                    }
                }
            }
        }
        false
    };
    let mut ground = false;
    let mut horizontal = false;
    for axis in [1usize, 0, 2] {
        let d = vel[axis];
        if d == 0.0 {
            continue;
        }
        // sub-steps of at most 0.25 blocks so fast entities can't tunnel
        let steps = (d.abs() / 0.25).ceil().max(1.0) as usize;
        let step = d / steps as f64;
        for _ in 0..steps {
            let mut next = *pos;
            next[axis] += step;
            if blocked(&next) {
                // snap flush against the obstacle
                if axis == 1 {
                    if step < 0.0 {
                        let snapped = [pos[0], next[1].floor() + 1.0, pos[2]];
                        if !blocked(&snapped) && snapped[1] <= pos[1] {
                            *pos = snapped;
                        }
                        ground = true;
                    }
                } else {
                    horizontal = true;
                }
                vel[axis] = 0.0;
                break;
            }
            *pos = next;
        }
    }
    (ground, horizontal)
}

/// One tick of item / falling-block motion (vanilla constants).
/// Returns true when the entity is resting on the ground.
pub fn physics(e: &mut Entity, cell: &dyn Fn(i32, i32, i32) -> Cell, friction_below: f64) -> bool {
    let (half, h) = e.size();
    let in_water = cell(e.pos[0].floor() as i32, (e.pos[1] + 0.1).floor() as i32, e.pos[2].floor() as i32) == Cell::Water;
    match e.kind {
        EKind::Item { .. } if in_water => {
            // items float up in water
            e.vel[0] *= 0.99;
            e.vel[2] *= 0.99;
            if e.vel[1] < 0.06 {
                e.vel[1] += 5.0e-4;
            }
        }
        _ => e.vel[1] -= 0.04,
    }
    let (ground, _) = move_entity(&mut e.pos, &mut e.vel, half, h, cell);
    e.on_ground = ground;
    match e.kind {
        EKind::Item { .. } => {
            let f = if ground { friction_below * 0.98 } else { 0.98 };
            e.vel[0] *= f;
            e.vel[1] *= 0.98;
            e.vel[2] *= f;
            if ground && e.vel[1] < 0.0 {
                e.vel[1] *= -0.5;
            }
        }
        EKind::Falling { .. } => {
            for v in &mut e.vel {
                *v *= 0.98;
            }
        }
        _ => {}
    }
    for v in &mut e.vel {
        if v.abs() < 0.003 {
            *v = 0.0;
        }
    }
    ground
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::blocks;

    fn floor(_x: i32, y: i32, _z: i32) -> Cell {
        if y < 64 { Cell::Solid } else { Cell::Open }
    }

    #[test]
    fn item_falls_and_rests_on_floor() {
        let mut e = Entity { eid: 1, uuid: uuid::Uuid::nil(), kind: EKind::Item { stack: Stack::new(1, 1), pickup_delay: 10 }, pos: [0.5, 70.0, 0.5], vel: [0.05, 0.2, 0.0], on_ground: false, age: 0, synced: [0.0; 3] };
        for _ in 0..100 {
            physics(&mut e, &floor, 0.6);
        }
        assert!(e.on_ground);
        assert!((e.pos[1] - 64.0).abs() < 1e-6, "{:?}", e.pos);
        assert_eq!(e.vel, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn falling_block_lands_on_top() {
        let mut e = Entity { eid: 1, uuid: uuid::Uuid::nil(), kind: EKind::Falling { state: blocks::SAND }, pos: [0.5, 100.0, 0.5], vel: [0.0; 3], on_ground: false, age: 0, synced: [0.0; 3] };
        let mut ticks = 0;
        while !physics(&mut e, &floor, 0.6) {
            ticks += 1;
            assert!(ticks < 200);
        }
        assert_eq!(e.pos[1].floor() as i32, 64);
    }
}
