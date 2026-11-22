//! Mobs picking up items (`Mob.aiStep` -> `pickUpItem` /
//! `equipItemIfPossible`) and piglin bartering (`PiglinAi`): zombies and
//! skeletons that can pick up loot wear better armour and weapons they walk
//! over; piglins admire gold ingots for a while and hand out a barter.

use rand::Rng;

use super::combat::attr::{Attr, Op};
use super::combat::living::{BODY, CHEST, FEET, HEAD, LEGS, MAINHAND, OFFHAND};
use super::entity::EKind;
use super::mob::Mob;
use super::Game;
use crate::anticheat::combat::Aabb;
use crate::components;
use crate::data::{GameData, Items, Stack};
use crate::net::codec::Out;

/// `PiglinAi.ADMIRE_DURATION`
pub const ADMIRE_TICKS: i32 = 119;

/// Mobs that equip what they pick up (when `CanPickUpLoot`).
fn equipper(n: &str) -> bool {
    matches!(n, "zombie" | "husk" | "drowned" | "zombie_villager" | "zombified_piglin" | "skeleton" | "stray" | "wither_skeleton" | "bogged" | "parched" | "piglin" | "pillager" | "vindicator")
}

/// `Mob.getEquipmentSlotForItem`
pub fn slot_for(item: i32) -> usize {
    match components::defaults(item).equippable_slot.as_deref() {
        Some("head") => HEAD,
        Some("chest") => CHEST,
        Some("legs") => LEGS,
        Some("feet") => FEET,
        Some("body") => BODY,
        Some("offhand") => OFFHAND,
        _ => MAINHAND,
    }
}

fn slot_name(slot: usize) -> &'static str {
    match slot {
        HEAD => "head",
        CHEST => "chest",
        LEGS => "legs",
        FEET => "feet",
        BODY => "body",
        OFFHAND => "offhand",
        _ => "mainhand",
    }
}

/// `getApproximateAttributeWith`: the item's add_value modifiers for `attr`
/// in `slot` (the mob's base value is the same on both sides).
fn attr_of(st: &Stack, attr: Attr, slot: usize) -> f64 {
    let s = slot_name(slot);
    components::defaults(st.item)
        .modifiers
        .iter()
        .filter(|m| m.attr == attr && matches!(m.modifier.op, Op::AddValue) && (m.slot == s || m.slot == "any" || (m.slot == "armor" && matches!(slot, HEAD | CHEST | LEGS | FEET | BODY)) || (m.slot == "hand" && matches!(slot, MAINHAND | OFFHAND))))
        .map(|m| m.modifier.amount)
        .sum()
}

/// `canReplaceEqualItem`
fn replace_equal(new: &Stack, cur: &Stack) -> bool {
    let (np, cp) = (new.patch(), cur.patch());
    if np.enchantments.len() != cp.enchantments.len() {
        return np.enchantments.len() > cp.enchantments.len();
    }
    if new.damage_value() != cur.damage_value() {
        return new.damage_value() < cur.damage_value();
    }
    np.custom_name.is_some() && cp.custom_name.is_none()
}

fn preferred_tag(n: &str) -> Option<&'static str> {
    match n {
        "skeleton" | "stray" | "wither_skeleton" | "bogged" | "parched" => Some("skeleton_preferred_weapons"),
        "piglin" => Some("piglin_preferred_weapons"),
        "pillager" => Some("pillager_preferred_weapons"),
        "drowned" => Some("drowned_preferred_weapons"),
        _ => None,
    }
}

/// `Mob.canReplaceCurrentItem`
pub fn can_replace(m: &Mob, new: &Stack, cur: Option<&Stack>, slot: usize, data: &GameData) -> bool {
    let Some(cur) = cur else { return true };
    if matches!(slot, HEAD | CHEST | LEGS | FEET | BODY) {
        if cur.enchantment_level("binding_curse") > 0 {
            return false;
        }
        let (a, b) = (attr_of(new, Attr::Armor, slot), attr_of(cur, Attr::Armor, slot));
        if a != b {
            return a > b;
        }
        let (a, b) = (attr_of(new, Attr::ArmorToughness, slot), attr_of(cur, Attr::ArmorToughness, slot));
        if a != b {
            return a > b;
        }
        return replace_equal(new, cur);
    }
    if slot == MAINHAND {
        if let Some(tag) = preferred_tag(m.ty.name()) {
            let (ni, ci) = (data.item_in(tag, new.item), data.item_in(tag, cur.item));
            if ni && !ci {
                return true;
            }
            if ci && !ni {
                return false;
            }
        }
        let (a, b) = (attr_of(new, Attr::AttackDamage, slot), attr_of(cur, Attr::AttackDamage, slot));
        if a != b {
            return a > b;
        }
        return replace_equal(new, cur);
    }
    false
}

/// `Mob.wantsToPickUp` for the types that pick things up.
fn wants(m: &Mob, st: &Stack, data: &GameData) -> bool {
    let n = m.ty.name();
    let name = Items::get().name(st.item);
    if n == "piglin" {
        // PiglinAi.wantsToPickup: gold to admire (adults with a free off hand)
        if name == "gold_ingot" {
            return !m.is_baby() && m.eq[OFFHAND].is_none() && m.v.admire == 0;
        }
        if !data.item_in("piglin_loved", st.item) {
            return false;
        }
    } else if !m.can_pick_up {
        return false;
    }
    if !equipper(n) {
        return false;
    }
    if n == "zombie" && name == "glow_ink_sac" || n == "drowned" && name == "nautilus_shell" {
        return false;
    }
    if n == "wither_skeleton" && data.item_in("wither_skeleton_disliked_weapons", st.item) {
        return false;
    }
    let slot = slot_for(st.item);
    if slot == BODY || slot == OFFHAND {
        return false;
    }
    can_replace(m, st, m.eq[slot].as_ref(), slot, data)
}

enum Pick {
    /// equipped into slot; the old item (if dropped)
    Equip(usize, Option<Stack>),
    Admire,
}

impl Game {
    /// Per tick: mobs grab items they touch; piglins finish admiring.
    pub(crate) fn tick_mob_pickup(&self) {
        let mut rng = rand::thread_rng();
        let items: Vec<(i32, [f64; 3], Stack)> = self
            .entities
            .lock()
            .values()
            .filter_map(|e| match e.kind {
                EKind::Item { stack, pickup_delay } if pickup_delay == 0 => Some((e.eid, e.pos, stack)),
                _ => None,
            })
            .collect();
        let mut taken: Vec<(i32, i32, (i32, i32), Stack)> = Vec::new(); // item, mob, chunk, what was taken
        let mut gone: std::collections::HashSet<i32> = Default::default();
        let mut dropped: Vec<([f64; 3], Stack)> = Vec::new();
        let mut equip_pkts = Vec::new();
        let mut barters: Vec<(i32, [f64; 3])> = Vec::new();
        {
            let mut ents = self.entities.lock();
            for e in ents.values_mut() {
                let (pos, eid, chunk) = (e.pos, e.eid, e.chunk());
                let EKind::Mob(m) = &mut e.kind else { continue };
                if m.death > 0 {
                    continue;
                }
                if m.v.admire > 0 {
                    m.v.admire -= 1;
                    if m.v.admire == 0 {
                        barters.push((eid, pos));
                    }
                    continue;
                }
                if items.is_empty() || !(m.can_pick_up || m.ty.is("piglin")) || !equipper(m.ty.name()) || m.no_ai {
                    continue;
                }
                let (w, h) = m.ty.size_of(m);
                let bb = Aabb { min: [pos[0] - w / 2.0 - 1.0, pos[1], pos[2] - w / 2.0 - 1.0], max: [pos[0] + w / 2.0 + 1.0, pos[1] + h, pos[2] + w / 2.0 + 1.0] };
                for (ie, ipos, st) in &items {
                    if gone.contains(ie) || !Aabb::entity(*ipos, 0.25, 0.25).intersects(&bb) || !wants(m, st, &self.data) {
                        continue;
                    }
                    let one = Stack { count: 1, ..*st };
                    let pick = if m.ty.is("piglin") && Items::get().name(st.item) == "gold_ingot" {
                        m.eq[OFFHAND] = Some(one);
                        m.v.admire = ADMIRE_TICKS;
                        Pick::Admire
                    } else {
                        let slot = slot_for(st.item);
                        let old = m.eq[slot];
                        let chance = m.drop_chance[slot];
                        let drop_old = old.filter(|_| (rng.r#gen::<f32>() - 0.1).max(0.0) < chance);
                        m.eq[slot] = Some(one);
                        m.drop_chance[slot] = 2.0;
                        Pick::Equip(slot, drop_old)
                    };
                    m.persistent = true;
                    if let Pick::Equip(_, Some(o)) = pick {
                        dropped.push((pos, o));
                    }
                    equip_pkts.push((chunk, self.mob_equipment(eid, m)));
                    taken.push((*ie, eid, chunk, one));
                    gone.insert(*ie);
                    break;
                }
            }
        }
        for (ie, mob, chunk, one) in taken {
            let mut b = Vec::new();
            b.vi(ie).vi(mob).vi(1);
            self.broadcast_chunk(chunk, &self.encode("take_item_entity", &b));
            let left = items.iter().find(|x| x.0 == ie).map(|x| x.2.count - one.count).unwrap_or(0);
            self.set_item_entity(ie, if left > 0 { Some(Stack { count: left, ..one }) } else { None });
        }
        for (c, p) in equip_pkts {
            self.broadcast_chunk(c, &p);
        }
        for (pos, st) in dropped {
            self.spawn_item([pos[0], pos[1] + 0.5, pos[2]], st, [0.0, 0.1, 0.0], 10);
        }
        for (eid, pos) in barters {
            self.barter(eid, pos);
        }
    }

    /// `PiglinAi.stopHoldingOffHandItem`: a gold ingot turns into a barter.
    fn barter(&self, eid: i32, pos: [f64; 3]) {
        let mut rng = rand::thread_rng();
        let (held, pkt, chunk, yaw) = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return };
            let chunk = e.chunk();
            let EKind::Mob(m) = &mut e.kind else { return };
            let held = m.eq[OFFHAND].take();
            (held, self.mob_equipment(eid, m), chunk, m.mo.yaw)
        };
        self.broadcast_chunk(chunk, &pkt);
        let Some(h) = held else { return };
        let out = if Items::get().name(h.item) == "gold_ingot" { self.data.roll_loot("gameplay/piglin_bartering", &mut rng) } else { vec![h] };
        // throw toward the nearest player, else forward (`BehaviorUtils.throwItem`)
        let target = self.players().iter().map(|p| p.pose.lock().pos).filter(|p| (p[0] - pos[0]).powi(2) + (p[2] - pos[2]).powi(2) < 16.0 * 16.0).min_by(|a, b| {
            let da = (a[0] - pos[0]).powi(2) + (a[2] - pos[2]).powi(2);
            let db = (b[0] - pos[0]).powi(2) + (b[2] - pos[2]).powi(2);
            da.total_cmp(&db)
        });
        let dir = match target {
            Some(t) => {
                let (dx, dz) = (t[0] - pos[0], t[2] - pos[2]);
                let l = (dx * dx + dz * dz).sqrt().max(1e-3);
                [dx / l, dz / l]
            }
            None => {
                let r = (yaw as f64).to_radians();
                [-r.sin(), r.cos()]
            }
        };
        for st in out {
            let v = [dir[0] * 0.3 + rng.gen_range(-0.02..0.02), 0.2, dir[1] * 0.3 + rng.gen_range(-0.02..0.02)];
            self.spawn_item([pos[0], pos[1] + 1.3, pos[2]], st, v, 40);
        }
        self.play_sound("minecraft:entity.piglin.admiring_item", 5, pos, 1.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobs_prefer_better_gear() {
        let items = Items::get();
        let st = |n: &str| Stack::new(items.id(n).unwrap(), 1);
        let z = Mob::new(crate::game::mob::MobType::from_name("zombie").unwrap(), false);
        let d = GameData::empty();
        assert_eq!(slot_for(items.id("iron_helmet").unwrap()), HEAD);
        assert_eq!(slot_for(items.id("diamond_sword").unwrap()), MAINHAND);
        assert!(can_replace(&z, &st("iron_chestplate"), Some(&st("leather_chestplate")), CHEST, &d));
        assert!(!can_replace(&z, &st("leather_chestplate"), Some(&st("iron_chestplate")), CHEST, &d));
        assert!(can_replace(&z, &st("diamond_sword"), Some(&st("iron_shovel")), MAINHAND, &d));
        assert!(!can_replace(&z, &st("wooden_sword"), Some(&st("diamond_sword")), MAINHAND, &d));
        assert!(can_replace(&z, &st("stick"), None, MAINHAND, &d));
    }

    #[test]
    fn zombies_wear_armour_and_piglins_barter() {
        let Some((g, _gen, _store, tmp)) = crate::game::endfight::tests::end_game() else { return };
        let items = Items::get();
        let y = g.motion_blocking_height(0, 0) as f64;
        let zt = crate::game::mob::MobType::from_name("zombie").unwrap();
        let z = g.spawn_mob_with(zt, [0.5, y, 0.5], true, |m, _| m.can_pick_up = true);
        g.spawn_item([0.8, y, 0.5], Stack::new(items.id("iron_helmet").unwrap(), 1), [0.0; 3], 0);
        g.tick_mob_pickup();
        let helm = g.entities.lock().get(&z).and_then(|e| if let EKind::Mob(m) = &e.kind { m.eq[HEAD] } else { None });
        assert_eq!(helm.map(|s| items.name(s.item)), Some("iron_helmet"));
        // piglin: a gold ingot is admired, then traded
        let pt = crate::game::mob::MobType::from_name("piglin").unwrap();
        let p = g.spawn_mob_with(pt, [20.5, y, 0.5], true, |m, _| m.baby = false);
        g.spawn_item([20.7, y, 0.5], Stack::new(items.id("gold_ingot").unwrap(), 3), [0.0; 3], 0);
        g.tick_mob_pickup();
        let adm = g.entities.lock().get(&p).and_then(|e| if let EKind::Mob(m) = &e.kind { Some((m.v.admire, m.eq[OFFHAND])) } else { None }).unwrap();
        assert_eq!(adm.0, ADMIRE_TICKS);
        assert_eq!(adm.1.map(|s| items.name(s.item)), Some("gold_ingot"));
        let left: u8 = g.entities.lock().values().filter_map(|e| match e.kind { EKind::Item { stack, .. } if items.name(stack.item) == "gold_ingot" => Some(stack.count), _ => None }).sum();
        assert_eq!(left, 2, "one ingot taken");
        let before = g.entities.lock().values().filter(|e| matches!(e.kind, EKind::Item { .. })).count();
        for _ in 0..ADMIRE_TICKS {
            g.tick_mob_pickup();
        }
        let off = g.entities.lock().get(&p).and_then(|e| if let EKind::Mob(m) = &e.kind { Some(m.eq[OFFHAND]) } else { None }).unwrap();
        assert!(off.is_none() || off.is_some_and(|s| items.name(s.item) == "gold_ingot"), "ingot spent (it may grab another)");
        let after = g.entities.lock().values().filter(|e| matches!(e.kind, EKind::Item { .. })).count();
        assert!(after + 1 > before, "a barter was thrown");
        let _ = std::fs::remove_dir_all(tmp);
    }
}
