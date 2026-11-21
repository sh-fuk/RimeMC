//! Blocks that act on entities inside or on top of them (`entityInside`,
//! `stepOn`, `fallOn`): fire, lava, campfires, cactus, sweet berry bushes,
//! wither roses, powder snow, magma, dripstone, hay, honey, beds, slime.

use crate::anticheat::combat::Aabb;
use crate::registry::{vstate, BlockRegistry, BlockState};

/// What an entity's box touches this tick (`checkInsideBlocks`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Inside {
    pub water: bool,
    pub lava: bool,
    /// fire block damage (1 fire, 2 soul fire)
    pub fire: Option<f32>,
    /// lit campfire damage (1 campfire, 2 soul campfire)
    pub campfire: Option<f32>,
    pub cactus: bool,
    /// a grown (age > 0) sweet berry bush
    pub berry_bush: bool,
    /// any sweet berry bush (slows down)
    pub any_berry_bush: bool,
    pub wither_rose: bool,
    pub powder_snow: bool,
    pub cobweb: bool,
    pub honey: bool,
}

/// Blocks touching the box (deflated by 1e-5 like `checkInsideBlocks`).
pub fn inside(get: &dyn Fn(i32, i32, i32) -> Option<BlockState>, bbox: &Aabb) -> Inside {
    let reg = BlockRegistry::get();
    let mut r = Inside::default();
    let lo = [(bbox.min[0] + 1e-5).floor() as i32, (bbox.min[1] + 1e-5).floor() as i32, (bbox.min[2] + 1e-5).floor() as i32];
    let hi = [(bbox.max[0] - 1e-5).floor() as i32, (bbox.max[1] - 1e-5).floor() as i32, (bbox.max[2] - 1e-5).floor() as i32];
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                let Some(s) = get(x, y, z) else { continue };
                if s.is_air() {
                    continue;
                }
                // fluids: the fluid surface must reach into the box
                if vstate::has_fluid(s) {
                    let h = y as f64 + fluid_height(s);
                    if h >= bbox.min[1] {
                        if vstate::is_water(s) {
                            r.water = true;
                        } else {
                            r.lava = true;
                        }
                    }
                }
                match s.name() {
                    "fire" => r.fire = Some(r.fire.unwrap_or(0.0).max(1.0)),
                    "soul_fire" => r.fire = Some(2.0),
                    "campfire" if reg.property(s, "lit") == Some("true") => r.campfire = Some(r.campfire.unwrap_or(0.0).max(1.0)),
                    "soul_campfire" if reg.property(s, "lit") == Some("true") => r.campfire = Some(2.0),
                    "cactus" => r.cactus = true,
                    "sweet_berry_bush" => {
                        r.any_berry_bush = true;
                        if reg.property(s, "age") != Some("0") {
                            r.berry_bush = true;
                        }
                    }
                    "wither_rose" => r.wither_rose = true,
                    "powder_snow" => r.powder_snow = true,
                    "cobweb" => r.cobweb = true,
                    "honey_block" => r.honey = true,
                    "bubble_column" => r.water = true,
                    _ => {}
                }
            }
        }
    }
    r
}

/// `FluidState.getHeight` for a fluid block (source 8/9, levels below).
fn fluid_height(s: BlockState) -> f64 {
    let reg = BlockRegistry::get();
    let level: i32 = reg.property(s, "level").and_then(|l| l.parse().ok()).unwrap_or(0);
    if level == 0 || level >= 8 {
        // source or falling: a falling fluid fills the block
        if level >= 8 { 1.0 } else { 8.0 / 9.0 }
    } else {
        (8 - level) as f64 / 9.0
    }
}

/// `Block.fallOn`: (damage multiplier, extra fall distance, damage type);
/// None when the landing does no damage at all (slime, powder snow).
pub fn landing(s: BlockState, sneaking: bool) -> Option<(f32, f64, &'static str)> {
    let reg = BlockRegistry::get();
    match s.name() {
        "slime_block" if !sneaking => None,
        "hay_block" | "honey_block" => Some((0.2, 0.0, "fall")),
        n if n.ends_with("_bed") => Some((0.5, 0.0, "fall")),
        "pointed_dripstone" if reg.property(s, "vertical_direction") == Some("up") && reg.property(s, "thickness") == Some("tip") => Some((2.0, 2.5, "stalagmite")),
        _ => Some((1.0, 0.0, "fall")),
    }
}

/// `MagmaBlock.stepOn`: hurts unless stepping carefully (sneaking).
pub fn hot_floor(below: BlockState, sneaking: bool) -> bool {
    below.name() == "magma_block" && !sneaking
}

/// `EntityTypeTags.FREEZE_IMMUNE_WEARABLES`
pub fn freeze_immune_wearable(item: &str) -> bool {
    matches!(item, "leather_boots" | "leather_leggings" | "leather_chestplate" | "leather_helmet" | "leather_horse_armor")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::blocks;

    #[test]
    fn inside_fire_and_water() {
        let reg = BlockRegistry::get();
        let fire = reg.state("fire", &[]).expect("fire");
        let get = move |_x: i32, y: i32, _z: i32| Some(if y == 64 { fire } else { blocks::AIR });
        let r = inside(&get, &Aabb::entity([0.5, 64.0, 0.5], 0.6, 1.8));
        assert_eq!(r.fire, Some(1.0));
        let get = |_x: i32, y: i32, _z: i32| Some(if y == 64 { blocks::WATER } else { blocks::AIR });
        assert!(inside(&get, &Aabb::entity([0.5, 64.5, 0.5], 0.6, 1.8)).water);
        // standing on top of water (feet at 64.95): source height 0.888 does not reach
        assert!(!inside(&get, &Aabb::entity([0.5, 64.95, 0.5], 0.6, 1.8)).water);
    }
}
