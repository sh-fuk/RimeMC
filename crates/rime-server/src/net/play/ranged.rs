//! Bows, crossbows, tridents and thrown items (`BowItem`, `CrossbowItem`,
//! `TridentItem`, `ProjectileItem`s) for a player.

use std::sync::atomic::Ordering;

use rand::Rng;

use super::Session;
use crate::data::{Items, Stack};
use crate::game::combat::living::UseItem;
use crate::game::projectile::{enchant_single, enchant_value, PType};

/// `ItemTags.ARROWS`
fn is_arrow(name: &str) -> bool {
    matches!(name, "arrow" | "spectral_arrow" | "tipped_arrow")
}

/// `BowItem.getPowerForTime`
pub fn bow_power(time_held: i32) -> f32 {
    let p = time_held as f32 / 20.0;
    ((p * p + p * 2.0) / 3.0).min(1.0)
}

impl Session {
    fn slot_of_hand(&self, hand: u8) -> usize {
        if hand == 1 { 45 } else { 36 + self.me.held.load(Ordering::Relaxed) as usize }
    }

    /// `Player.getProjectile`: (window slot, stack); creative players get an
    /// arrow out of nothing.
    fn find_ammo(&self, weapon: &str) -> Option<(Option<usize>, Stack)> {
        let items = Items::get();
        let inv = self.me.inv.lock();
        let held_ok = |n: &str| is_arrow(n) || (weapon == "crossbow" && n == "firework_rocket");
        let main = 36 + self.me.held.load(Ordering::Relaxed) as usize;
        for s in [45, main] {
            if let Some(st) = inv.slots[s] {
                if held_ok(items.name(st.item)) {
                    return Some((Some(s), st));
                }
            }
        }
        for s in (36..45).chain(9..36) {
            if let Some(st) = inv.slots[s] {
                if is_arrow(items.name(st.item)) {
                    return Some((Some(s), st));
                }
            }
        }
        drop(inv);
        (self.gamemode() == 1).then(|| (None, Stack::new(items.id("arrow").unwrap_or(0), 1)))
    }

    /// `ProjectileWeaponItem.draw`: takes the ammo out of the inventory.
    fn draw_ammo(&mut self, weapon: Stack, ammo: (Option<usize>, Stack)) -> Vec<Stack> {
        let mut rng = rand::thread_rng();
        let creative = self.gamemode() == 1;
        let n = enchant_value(weapon, "projectile_count", None, None, 1.0, &mut rng).max(1.0) as usize;
        let mut out = Vec::new();
        for i in 0..n {
            let use_n = if i > 0 || creative { 0 } else { enchant_value(weapon, "ammo_use", Some(ammo.1), None, 1.0, &mut rng).max(0.0) as u8 };
            let mut one = ammo.1;
            one.count = 1;
            if use_n == 0 {
                one = one.with_patch(|p| p.intangible = true);
                out.push(one);
            } else if let Some(slot) = ammo.0 {
                let mut inv = self.me.inv.lock();
                let have = inv.slots[slot].map(|s| s.count).unwrap_or(0);
                if use_n > have {
                    continue;
                }
                inv.consume(slot, use_n);
                one.count = use_n;
                out.push(one);
            }
        }
        out
    }

    /// `use_item` for ranged weapons and thrown items; true when handled.
    pub(super) fn use_ranged(&mut self, hand: i32, held: Stack) -> bool {
        let items = Items::get();
        let name = items.name(held.item).to_string();
        let slot = if hand == 1 { 45 } else { 36 + self.me.held.load(Ordering::Relaxed) as usize };
        let gm = self.gamemode();
        if self.me.vit.lock().living.on_cooldown(held.item) {
            return true;
        }
        match name.as_str() {
            "bow" | "trident" => {
                if name == "bow" && self.find_ammo("bow").is_none() {
                    return true;
                }
                if name == "trident" {
                    // TridentItem.use: not when about to break, riptide needs water or rain
                    if held.is_damageable() && held.damage_value() >= held.max_damage() - 1 {
                        return true;
                    }
                    let riptide = enchant_single(held, "trident_spin_attack_strength", 0.0, &mut rand::thread_rng());
                    if riptide > 0.0 && !self.in_water_or_rain() {
                        return true;
                    }
                }
                self.start_using(hand as u8, held);
                true
            }
            "crossbow" => {
                let charged = held.patch().charged.clone();
                if !charged.is_empty() {
                    let ammo: Vec<Stack> = charged.iter().map(|(i, c, p)| Stack { item: *i, count: *c, comp: *p }).collect();
                    let power = if ammo.iter().any(|s| items.name(s.item) == "firework_rocket") { 1.6 } else { 3.15 };
                    let st = self.st.clone();
                    let pose = *self.me.pose.lock();
                    let eye = [pose.pos[0], pose.pos[1] + 1.62 - 0.1, pose.pos[2]];
                    let motion = *self.me.motion.lock();
                    // clear the charge first
                    let empty = held.with_patch(|p| p.charged.clear());
                    self.me.inv.lock().set(slot, Some(empty));
                    let arrows: Vec<Stack> = ammo.iter().copied().filter(|s| items.name(s.item) != "firework_rocket").collect();
                    self.game.shoot_arrows(self.me.eid, eye, pose.yaw, pose.pitch, motion, pose.on_ground, empty, &arrows, power, 1.0, true, gm == 1, false);
                    let mut rng = rand::thread_rng();
                    for i in 0..ammo.len() {
                        // CrossbowItem.getShotPitch
                        let high = i % 2 == 1;
                        let r = 1.0 / (rng.r#gen::<f32>() * 0.5 + 1.8);
                        let pitch = if i == 0 { 1.0 } else if high { r + 0.63 } else { r + 0.43 };
                        self.game.play_sound("minecraft:item.crossbow.shoot", 7, pose.pos, 1.0, pitch);
                    }
                    self.damage_weapon(slot, ammo.len() as i32);
                    self.game.flush_inventory(&self.me);
                } else if self.find_ammo("crossbow").is_some() {
                    self.start_using(hand as u8, held);
                }
                true
            }
            n if PType::for_thrown(n).is_some() => {
                let st = self.st.clone();
                self.game.throw_item(&self.me, held);
                if let Some(t) = crate::game::consume::use_cooldown(&held) {
                    let mut v = self.me.vit.lock();
                    let now = v.living.tick_count;
                    v.living.cooldowns.push((held.item, now, now + t));
                    drop(v);
                    let mut b = Vec::new();
                    crate::net::codec::Out::str(&mut b, &format!("minecraft:{n}"));
                    crate::net::codec::Out::vi(&mut b, t);
                    self.send("cooldown", &b);
                }
                if gm != 1 {
                    self.me.inv.lock().consume(slot, 1);
                }
                self.game.flush_inventory(&self.me);
                true
            }
            _ => false,
        }
    }

    fn start_using(&mut self, hand: u8, held: Stack) {
        let mut v = self.me.vit.lock();
        v.living.using = Some(UseItem { hand, item: held.item, elapsed: 0 });
        v.data_dirty = true;
    }

    fn in_water_or_rain(&self) -> bool {
        let pos = self.me.pose.lock().pos;
        let w = &self.game.world;
        let (x, y, z) = (pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32);
        let wet = w.get_block(x, y, z).is_some_and(crate::registry::vstate::is_water);
        wet || self.game.raining_at([x, y, z])
    }

    /// `ItemStack.hurtAndBreak` on the weapon in `slot`.
    fn damage_weapon(&mut self, slot: usize, amount: i32) {
        if self.gamemode() == 1 {
            return;
        }
        let eq_slot = if slot == 45 { crate::game::combat::living::OFFHAND } else { crate::game::combat::living::MAINHAND };
        let st = self.st.clone();
        self.game.with_player(&self.me, |l, eq, _| {
            let mut events = Vec::new();
            crate::game::combat::living::hurt_and_break(eq, eq_slot, amount, false, &mut events, &mut rand::thread_rng());
            l.events.extend(events);
        });
    }

    /// Called every tick while an item is in use (`onUseTick`): crossbow loading.
    pub(super) fn tick_using(&mut self) {
        let Some(u) = self.me.vit.lock().living.using else { return };
        let slot = self.slot_of_hand(u.hand);
        let Some(held) = self.me.inv.lock().slots[slot] else {
            self.stop_using_item();
            return;
        };
        if held.item != u.item {
            self.stop_using_item();
            return;
        }
        let items = Items::get();
        if items.name(held.item) != "crossbow" {
            return;
        }
        let mut rng = rand::thread_rng();
        let dur = ((enchant_single(held, "crossbow_charge_time", 1.25, &mut rng)) * 20.0).floor().max(1.0);
        let now = u.elapsed as f32 / dur;
        let before = (u.elapsed - 1) as f32 / dur;
        let pos = self.me.pose.lock().pos;
        let st = self.st.clone();
        let quick = held.enchantment_level("quick_charge");
        if now >= 0.2 && before < 0.2 {
            let s = match quick {
                0 => "minecraft:item.crossbow.loading_start",
                1 => "minecraft:item.crossbow.quick_charge_1",
                2 => "minecraft:item.crossbow.quick_charge_2",
                _ => "minecraft:item.crossbow.quick_charge_3",
            };
            self.game.play_sound(s, 7, pos, 0.5, 1.0);
        }
        if now >= 0.5 && before < 0.5 && quick == 0 {
            self.game.play_sound("minecraft:item.crossbow.loading_middle", 7, pos, 0.5, 1.0);
        }
        if now >= 1.0 && held.patch().charged.is_empty() {
            let Some(ammo) = self.find_ammo("crossbow") else { return };
            let drawn = self.draw_ammo(held, ammo);
            if drawn.is_empty() {
                return;
            }
            let charged = held.with_patch(|p| p.charged = drawn.iter().map(|s| (s.item, s.count, s.comp)).collect());
            self.me.inv.lock().set(slot, Some(charged));
            self.game.flush_inventory(&self.me);
            self.game.play_sound("minecraft:item.crossbow.loading_end", 7, pos, 1.0, 1.0 / (rng.r#gen::<f32>() * 0.5 + 1.0) + 0.2);
        }
    }

    /// `releaseUsing` (player_action 5): fires bows and tridents.
    pub(super) fn release_using(&mut self) {
        let Some(u) = self.me.vit.lock().living.using else { return };
        let slot = self.slot_of_hand(u.hand);
        let Some(held) = self.me.inv.lock().slots[slot] else { return };
        if held.item != u.item {
            return;
        }
        let items = Items::get();
        let st = self.st.clone();
        let gm = self.gamemode();
        let pose = *self.me.pose.lock();
        let eye = [pose.pos[0], pose.pos[1] + 1.62 - 0.1, pose.pos[2]];
        let motion = *self.me.motion.lock();
        let mut rng = rand::thread_rng();
        match items.name(held.item) {
            "bow" => {
                let Some(ammo) = self.find_ammo("bow") else { return };
                let pow = bow_power(u.elapsed);
                if pow < 0.1 {
                    return;
                }
                let infinite = enchant_value(held, "ammo_use", Some(ammo.1), None, 1.0, &mut rng) == 0.0;
                let drawn = self.draw_ammo(held, ammo);
                if !drawn.is_empty() {
                    self.game.shoot_arrows(self.me.eid, eye, pose.yaw, pose.pitch, motion, pose.on_ground, held, &drawn, pow * 3.0, 1.0, pow == 1.0, gm == 1, infinite);
                    self.damage_weapon(slot, drawn.len() as i32);
                }
                self.game.play_sound("minecraft:entity.arrow.shoot", 7, pose.pos, 1.0, 1.0 / (rng.r#gen::<f32>() * 0.4 + 1.2) + pow * 0.5);
                self.game.flush_inventory(&self.me);
            }
            "trident" => {
                if u.elapsed < 10 {
                    return;
                }
                let riptide = enchant_single(held, "trident_spin_attack_strength", 0.0, &mut rng);
                if riptide > 0.0 && !self.in_water_or_rain() {
                    return;
                }
                if held.is_damageable() && held.damage_value() >= held.max_damage() - 1 {
                    return;
                }
                let sound = match (riptide > 0.0, held.enchantment_level("riptide")) {
                    (true, 1) => "minecraft:item.trident.riptide_1",
                    (true, 2) => "minecraft:item.trident.riptide_2",
                    (true, _) => "minecraft:item.trident.riptide_3",
                    _ => "minecraft:item.trident.throw",
                };
                // hurtWithoutBreaking(1)
                self.damage_weapon(slot, 1);
                if riptide == 0.0 {
                    let thrown = self.me.inv.lock().slots[slot].unwrap_or(held);
                    let mut one = thrown;
                    one.count = 1;
                    if gm != 1 {
                        self.me.inv.lock().consume(slot, 1);
                    }
                    let mut p = crate::game::projectile::Projectile::new(PType::Trident, Some(self.me.eid), one);
                    p.weapon = Some(one);
                    p.pickup = if gm == 1 { crate::game::projectile::Pickup::CreativeOnly } else { crate::game::projectile::Pickup::Allowed };
                    p.loyalty = enchant_value(one, "trident_return_acceleration", None, None, 0.0, &mut rng).clamp(0.0, 127.0) as u8;
                    p.foil = !one.patch().enchantments.is_empty();
                    let vel = crate::game::projectile::shoot_vector(pose.pitch, pose.yaw, 0.0, 2.5, 1.0, &mut rng);
                    let vel = [vel[0] + motion[0], vel[1] + if pose.on_ground { 0.0 } else { motion[1] }, vel[2] + motion[2]];
                    self.game.launch(p, eye, vel);
                    self.game.play_sound(sound, 7, pose.pos, 1.0, 1.0);
                } else {
                    // riptide: push the player along the look vector
                    let r = std::f32::consts::PI / 180.0;
                    let (y, x) = (pose.yaw * r, pose.pitch * r);
                    let mut d = [-(y.sin()) * x.cos(), -(x.sin()), y.cos() * x.cos()];
                    let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                    for c in d.iter_mut() {
                        *c *= riptide / l;
                    }
                    let mut push = [d[0] as f64, d[1] as f64, d[2] as f64];
                    if pose.on_ground {
                        push[1] += 1.1999999;
                    }
                    self.game.push_entity(self.me.eid, push);
                    self.game.play_sound(sound, 7, pose.pos, 1.0, 1.0);
                }
                self.game.flush_inventory(&self.me);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power() {
        assert_eq!(bow_power(20), 1.0);
        assert!((bow_power(10) - 0.41666666).abs() < 1e-6);
        assert!(bow_power(1) < 0.1);
    }
}
