//! `BlockState.canSurvive` for the blocks world generation places, ported
//! from the 26.1 block classes (`assets/26.1/block_classes.txt` maps every
//! block to its class chain).

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::Result;

use super::place::BlockSet;
use super::{Builder, Region};
use crate::registry::{vstate, BlockRegistry, BlockState};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Always,
    Vegetation,
    DryVegetation,
    Azalea,
    Mushroom,
    Propagule,
    Seagrass,
    TallSeagrass,
    SeaPickle,
    LilyPad,
    CactusFlower,
    DoublePlant,
    LeafLitter,
    SugarCane,
    Cactus,
    SnowLayer,
}

fn kinds() -> &'static Vec<Kind> {
    static K: OnceLock<Vec<Kind>> = OnceLock::new();
    K.get_or_init(|| {
        let mut by_name: HashMap<&str, Kind> = HashMap::new();
        for line in include_str!("../../../assets/26.1/block_classes.txt").lines() {
            let mut it = line.split_whitespace();
            let Some(name) = it.next() else { continue };
            let chain: Vec<&str> = it.collect();
            let has = |c: &str| chain.contains(&c);
            let kind = if has("SnowLayerBlock") {
                Kind::SnowLayer
            } else if has("SugarCaneBlock") {
                Kind::SugarCane
            } else if has("CactusBlock") {
                Kind::Cactus
            } else if has("LeafLitterBlock") {
                Kind::LeafLitter
            } else if has("TallSeagrassBlock") {
                Kind::TallSeagrass
            } else if has("SeagrassBlock") {
                Kind::Seagrass
            } else if has("SeaPickleBlock") {
                Kind::SeaPickle
            } else if has("LilyPadBlock") {
                Kind::LilyPad
            } else if has("CactusFlowerBlock") {
                Kind::CactusFlower
            } else if has("MangrovePropaguleBlock") {
                Kind::Propagule
            } else if has("MushroomBlock") {
                Kind::Mushroom
            } else if has("AzaleaBlock") {
                Kind::Azalea
            } else if has("DoublePlantBlock") {
                Kind::DoublePlant
            } else if has("DryVegetationBlock") {
                Kind::DryVegetation
            } else if has("VegetationBlock") {
                Kind::Vegetation
            } else {
                Kind::Always
            };
            by_name.insert(name, kind);
        }
        let reg = BlockRegistry::get();
        (0..reg.state_count()).map(|i| *by_name.get(BlockState(i as u16).name()).unwrap_or(&Kind::Always)).collect()
    })
}

pub fn kind(s: BlockState) -> Kind {
    kinds().get(s.0 as usize).copied().unwrap_or(Kind::Always)
}

/// Block tags used by survival rules.
pub struct Tags {
    vegetation: BlockSet,
    dry: BlockSet,
    azalea: BlockSet,
    propagule: BlockSet,
    hanging_propagule: BlockSet,
    no_seagrass: BlockSet,
    lily_pad: BlockSet,
    cactus_flower_override: BlockSet,
    sugar_cane: BlockSet,
    sugar_cane_adjacent: BlockSet,
    cactus: BlockSet,
    no_snow: BlockSet,
    snow_override: BlockSet,
    mushroom_override: BlockSet,
}

impl Tags {
    pub fn load(b: &mut Builder) -> Result<Tags> {
        Ok(Tags {
            vegetation: b.tag("supports_vegetation")?,
            dry: b.tag("supports_dry_vegetation")?,
            azalea: b.tag("supports_azalea")?,
            propagule: b.tag("supports_mangrove_propagule")?,
            hanging_propagule: b.tag("supports_hanging_mangrove_propagule")?,
            no_seagrass: b.tag("cannot_support_seagrass")?,
            lily_pad: b.tag("supports_lily_pad")?,
            cactus_flower_override: b.tag("support_override_cactus_flower")?,
            sugar_cane: b.tag("supports_sugar_cane")?,
            sugar_cane_adjacent: b.tag("supports_sugar_cane_adjacently")?,
            cactus: b.tag("supports_cactus")?,
            no_snow: b.tag("cannot_support_snow_layer")?,
            snow_override: b.tag("support_override_snow_layer")?,
            mushroom_override: b.tag("overrides_mushroom_light_requirement")?,
        })
    }
}

fn prop<'a>(s: BlockState, k: &str) -> Option<&'a str> {
    BlockRegistry::get().property(s, k)
}

fn add(p: [i32; 3], d: [i32; 3]) -> [i32; 3] {
    [p[0] + d[0], p[1] + d[1], p[2] + d[2]]
}

const HORIZONTAL: [[i32; 3]; 4] = [[0, 0, -1], [1, 0, 0], [0, 0, 1], [-1, 0, 0]];

/// `state.canSurvive(level, pos)`
pub fn can_survive(lv: &Region, s: BlockState, p: [i32; 3]) -> bool {
    let t = &lv.lib.tags;
    let below = || lv.get(add(p, [0, -1, 0]));
    match kind(s) {
        Kind::Always => true,
        Kind::Vegetation => t.vegetation.contains(below()),
        Kind::DryVegetation => t.dry.contains(below()),
        Kind::Azalea => t.azalea.contains(below()),
        Kind::Mushroom => {
            let b = below();
            // world generation has no light yet, so the raw brightness is 0
            t.mushroom_override.contains(b) || vstate::solid_render(b)
        }
        Kind::Propagule => {
            if prop(s, "hanging") == Some("true") {
                t.hanging_propagule.contains(lv.get(add(p, [0, 1, 0])))
            } else {
                t.propagule.contains(below())
            }
        }
        Kind::Seagrass => {
            let b = below();
            vstate::sturdy(b, 1) && !t.no_seagrass.contains(b)
        }
        Kind::TallSeagrass => {
            if prop(s, "half") == Some("upper") {
                let b = below();
                b.name() == s.name() && prop(b, "half") == Some("lower")
            } else {
                let b = below();
                let here = lv.get(p);
                vstate::sturdy(b, 1) && !t.no_seagrass.contains(b) && vstate::fluid(here) == 1
            }
        }
        Kind::SeaPickle => {
            let b = below();
            !vstate::up_face_empty(b) || vstate::sturdy(b, 1)
        }
        Kind::LilyPad => {
            let b = below();
            (vstate::is_water(b) || t.lily_pad.contains(b)) && vstate::fluid(lv.get(p)) == 0
        }
        Kind::CactusFlower => {
            let b = below();
            t.cactus_flower_override.contains(b) || vstate::sturdy_up_center(b)
        }
        Kind::DoublePlant => {
            if prop(s, "half") == Some("upper") {
                let b = below();
                b.name() == s.name() && prop(b, "half") == Some("lower")
            } else {
                t.vegetation.contains(below())
            }
        }
        Kind::LeafLitter => vstate::sturdy(below(), 1),
        Kind::SugarCane => {
            let b = below();
            if b.name() == "sugar_cane" {
                return true;
            }
            if !t.sugar_cane.contains(b) {
                return false;
            }
            let bp = add(p, [0, -1, 0]);
            HORIZONTAL.iter().any(|d| {
                let q = add(bp, *d);
                let st = lv.get(q);
                vstate::is_water(st) || t.sugar_cane_adjacent.contains(st)
            })
        }
        Kind::Cactus => {
            for d in HORIZONTAL {
                let st = lv.get(add(p, d));
                if vstate::solid(st) || matches!(vstate::fluid(st), 3 | 4) {
                    return false;
                }
            }
            let b = below();
            (b.name() == "cactus" || t.cactus.contains(b)) && !vstate::liquid(lv.get(add(p, [0, 1, 0])))
        }
        Kind::SnowLayer => {
            let b = below();
            if t.no_snow.contains(b) {
                return false;
            }
            if t.snow_override.contains(b) {
                return true;
            }
            vstate::full_up_face(b) || (b.name() == "snow" && prop(b, "layers") == Some("8"))
        }
    }
}

/// `isFaceSturdy(dir, FULL)`
pub fn face_sturdy(s: BlockState, dir: u8) -> bool {
    vstate::sturdy(s, dir)
}
