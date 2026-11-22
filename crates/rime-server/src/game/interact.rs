//! `Player.interactOn` → `Mob.interact` / `mobInteract`: feeding, breeding,
//! taming, sitting, shearing, milking, bucketing, dyeing, name tags,
//! igniting creepers, brushing armadillos, healing golems.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::Rng;

use super::combat::effects::{Effect, Instance};
use super::entity::EKind;
use super::mob::Mob;
use super::{Game, Player};
use crate::data::{Items, Stack};

/// What the interaction does to the held stack.
enum Use {
    /// nothing happened (`InteractionResult.PASS`)
    Pass,
    /// handled, the stack is untouched
    Success,
    /// one item used up (`usePlayerItem`)
    Consume,
    /// one item used up and this one given back (`ItemUtils.createFilledResult`)
    Exchange(Stack),
    /// the held tool takes this much durability damage
    Damage(i32),
}

const DYES: [&str; 16] = ["white_dye", "orange_dye", "magenta_dye", "light_blue_dye", "yellow_dye", "lime_dye", "pink_dye", "gray_dye", "light_gray_dye", "cyan_dye", "purple_dye", "blue_dye", "brown_dye", "green_dye", "red_dye", "black_dye"];

impl Game {
    /// Right click on an entity with `hand` (0 main, 1 off). True when handled.
    pub fn interact_mob(&self, p: &Arc<Player>, eid: i32, hand: i32) -> bool {
        let slot = if hand == 1 { 45 } else { 36 + p.held.load(Ordering::Relaxed) as usize };
        let held = p.inv.lock().slots[slot];
        let creative = p.gamemode.load(Ordering::Relaxed) == 1;
        if p.gamemode.load(Ordering::Relaxed) == 3 {
            return false;
        }
        let vehicle = self.entities.lock().get(&eid).map(|e| match &e.kind {
            EKind::Boat { .. } => 1,
            EKind::Minecart(_) => 2,
            _ => 0,
        });
        match vehicle {
            Some(1) => return hand == 0 && p.flags.load(Ordering::Relaxed) & super::hurt::F_SNEAKING == 0 && self.mount(p.eid, eid),
            Some(2) => return hand == 0 && self.use_minecart(p, eid, held, slot),
            _ => {}
        }
        if self.entities.lock().get(&eid).is_some_and(|e| matches!(e.kind, EKind::Knot)) {
            return hand == 0 && self.use_knot(p, eid);
        }
        if let Some(r) = self.leash_interact(p, eid, held, slot) {
            return r;
        }
        if hand == 0 {
            if let Some(r) = self.ride_interact(p, eid, held, slot) {
                return r;
            }
        }
        if self.is_frame(eid) {
            return hand == 0 && self.use_frame(p, eid, slot) || hand == 1 && held.is_some() && self.use_frame(p, eid, slot);
        }
        let items = Items::get();
        let name = held.map(|s| items.name(s.item).to_string()).unwrap_or_default();
        let ppos = p.pose.lock().pos;
        let mut rng = rand::thread_rng();
        let mut after: Vec<Box<dyn FnOnce(&Game)>> = Vec::new();
        let result = {
            let mut ents = self.entities.lock();
            let Some(e) = ents.get_mut(&eid) else { return false };
            let pos = e.pos;
            let chunk = e.chunk();
            let EKind::Mob(m) = &mut e.kind else { return false };
            if m.death > 0 {
                return false;
            }
            let r = self.mob_interact(m, eid, pos, chunk, p, held, &name, creative, &mut rng, &mut after);
            let _ = ppos;
            r
        };
        for f in after {
            f(self);
        }
        let handled = !matches!(result, Use::Pass);
        if !creative || matches!(result, Use::Exchange(_)) {
            let mut inv = p.inv.lock();
            match result {
                Use::Consume if !creative => inv.consume(slot, 1),
                Use::Exchange(full) => {
                    if creative {
                        if !inv.slots.iter().flatten().any(|s| s.item == full.item) {
                            inv.add(full, p.held.load(Ordering::Relaxed) as i16);
                        }
                    } else {
                        inv.consume(slot, 1);
                        if inv.slots[slot].is_none() {
                            inv.set(slot, Some(full));
                        } else if inv.add(full, p.held.load(Ordering::Relaxed) as i16) > 0 {
                            drop(inv);
                            self.spawn_item(ppos, full, [0.0, 0.2, 0.0], 40);
                            inv = p.inv.lock();
                        }
                    }
                }
                Use::Damage(n) if !creative => {
                    if let Some(mut s) = inv.slots[slot] {
                        let max = s.max_damage();
                        if max > 0 {
                            let unbreaking = s.enchantment_level("unbreaking") as i32;
                            let mut taken = 0;
                            for _ in 0..n {
                                if unbreaking == 0 || rng.gen_range(0..unbreaking + 1) == 0 {
                                    taken += 1;
                                }
                            }
                            let dmg = s.patch().damage.unwrap_or(0) + taken;
                            if dmg >= max {
                                inv.set(slot, None);
                                drop(inv);
                                self.play_sound("minecraft:entity.item.break", 7, ppos, 0.8, 0.8 + rng.r#gen::<f32>() * 0.4);
                                inv = p.inv.lock();
                            } else {
                                s.with_patch(|pt| pt.damage = Some(dmg));
                                inv.set(slot, Some(s));
                            }
                        }
                    }
                }
                _ => {}
            }
            drop(inv);
            self.flush_inventory(p);
        }
        if handled {
            // the player swings
            let mut b = Vec::new();
            crate::net::codec::Out::vi(&mut b, p.eid);
            crate::net::codec::Out::u8(&mut b, if hand == 1 { 3 } else { 0 });
            let c = ((ppos[0].floor() as i32) >> 4, (ppos[2].floor() as i32) >> 4);
            self.broadcast_chunk(c, &self.encode("animate", &b));
        }
        handled
    }

    #[allow(clippy::too_many_arguments)]
    fn mob_interact(&self, m: &mut Mob, eid: i32, pos: [f64; 3], chunk: (i32, i32), p: &Arc<Player>, held: Option<Stack>, name: &str, creative: bool, rng: &mut impl Rng, after: &mut Vec<Box<dyn FnOnce(&Game)>>) -> Use {
        let items = Items::get();
        let id = held.map(|s| s.item).unwrap_or(-1);
        let ty = m.ty.name();
        let sound = |after: &mut Vec<Box<dyn FnOnce(&Game)>>, s: &'static str, hostile: bool, pitch: f32| {
            after.push(Box::new(move |g: &Game| g.play_sound(s, if hostile { 5 } else { 6 }, pos, 1.0, pitch)));
        };
        let event = |after: &mut Vec<Box<dyn FnOnce(&Game)>>, ev: u8| {
            after.push(Box::new(move |g: &Game| g.broadcast_chunk(chunk, &g.entity_event(eid, ev))));
        };
        // Mob.interact: name tags
        if name == "name_tag" {
            if let Some(s) = held {
                if s.patch().custom_name.is_some() {
                    m.persistent = true;
                    m.v.custom_name = s.patch().custom_name.clone();
                    return Use::Consume;
                }
            }
            return Use::Pass;
        }
        // spawn eggs on a mob of their type: a baby
        if name.ends_with("_spawn_egg") && name.trim_end_matches("_spawn_egg") == ty && m.ty.ageable() {
            let t = m.ty;
            after.push(Box::new(move |g: &Game| {
                g.spawn_mob_with(t, pos, true, |b, _| b.set_baby(true));
            }));
            return Use::Consume;
        }
        // owner commands (TamableAnimal) when not feeding
        let is_owner = m.tame && m.owner == Some(p.uuid);
        let food = held.is_some() && self.is_breeding_food(ty, id);
        match ty {
            "cow" | "goat" if name == "bucket" && !m.is_baby() => {
                let s = if ty == "goat" { if m.v.screaming { "minecraft:entity.goat.screaming.milk" } else { "minecraft:entity.goat.milk" } } else { "minecraft:entity.cow.milk" };
                sound(after, s, false, 1.0);
                return Use::Exchange(Stack::new(items.id("milk_bucket").unwrap_or(0), 1));
            }
            "mooshroom" => {
                if name == "bowl" && !m.is_baby() {
                    sound(after, "minecraft:entity.mooshroom.milk", false, 1.0);
                    return Use::Exchange(Stack::new(items.id("mushroom_stew").unwrap_or(0), 1));
                }
                if name == "shears" && !m.is_baby() {
                    // MushroomCow.shear: becomes a cow, drops 5 mushrooms
                    let variant = if m.v.variant == 1 { "brown" } else { "red" };
                    let table = format!("shearing/mooshroom/{variant}");
                    after.push(Box::new(move |g: &Game| {
                        let mut rng = rand::thread_rng();
                        for s in g.data.roll_loot(&table, &mut rng) {
                            g.spawn_item([pos[0], pos[1] + 1.0, pos[2]], s, [0.0, 0.2, 0.0], 10);
                        }
                        g.play_sound("minecraft:entity.mooshroom.shear", 7, pos, 1.0, 1.0);
                        g.particles("minecraft:explosion", [pos[0], pos[1] + 0.5, pos[2]], 1, [0.0; 3], 0.0);
                        g.convert_mob_pub(eid, "cow");
                    }));
                    return Use::Damage(1);
                }
            }
            "sheep" => {
                if name == "shears" && !m.v.sheared && !m.is_baby() {
                    m.v.sheared = true;
                    let table = format!("shearing/sheep/{}", super::mobtick::wool_color_name(m.v.wool));
                    after.push(Box::new(move |g: &Game| {
                        let mut rng = rand::thread_rng();
                        // Sheep.shear: each drop lands 1 block up with a random push
                        for s in g.data.roll_loot(&table, &mut rng) {
                            let v = [((rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.1) as f64, (rng.r#gen::<f32>() * 0.05) as f64, ((rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.1) as f64];
                            g.spawn_item([pos[0], pos[1] + 1.0, pos[2]], s, v, 10);
                        }
                        g.play_sound("minecraft:entity.sheep.shear", 7, pos, 1.0, 1.0);
                    }));
                    return Use::Damage(1);
                }
                if let Some(c) = DYES.iter().position(|d| *d == name) {
                    if m.v.wool != c as u8 && !m.v.sheared {
                        m.v.wool = c as u8;
                        sound(after, "minecraft:item.dye.use", false, 1.0);
                        return Use::Consume;
                    }
                }
            }
            "snow_golem" if name == "shears" && m.v.pumpkin => {
                m.v.pumpkin = false;
                sound(after, "minecraft:entity.snow_golem.shear", false, 1.0);
                return Use::Damage(1);
            }
            "bogged" if name == "shears" && !m.v.sheared => {
                m.v.sheared = true;
                after.push(Box::new(move |g: &Game| {
                    let mut rng = rand::thread_rng();
                    for s in g.data.roll_loot("shearing/bogged", &mut rng) {
                        g.spawn_item([pos[0], pos[1] + 1.0, pos[2]], s, [0.0, 0.2, 0.0], 10);
                    }
                    g.play_sound("minecraft:entity.bogged.shear", 5, pos, 1.0, 1.0);
                }));
                return Use::Damage(1);
            }
            "iron_golem" if name == "iron_ingot" => {
                // IronGolem.mobInteract: heal 25
                let before = m.living.health;
                if before >= m.max_health() {
                    return Use::Pass;
                }
                m.living.heal(25.0);
                let pitch = 1.0 + (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.2;
                after.push(Box::new(move |g: &Game| g.play_sound("minecraft:entity.iron_golem.repair", 6, pos, 1.0, pitch)));
                return Use::Consume;
            }
            "creeper" if name == "flint_and_steel" || name == "fire_charge" => {
                let s = if name == "fire_charge" { "minecraft:item.firecharge.use" } else { "minecraft:item.flintandsteel.use" };
                sound(after, s, false, rng.r#gen::<f32>() * 0.4 + 0.8);
                m.v.ignited = true;
                return if name == "fire_charge" { Use::Consume } else { Use::Damage(1) };
            }
            "armadillo" if name == "brush" && !m.is_baby() => {
                after.push(Box::new(move |g: &Game| {
                    if let Some(i) = Items::get().id("armadillo_scute") {
                        g.spawn_item(pos, Stack::new(i, 1), [0.0, 0.2, 0.0], 10);
                    }
                    g.play_sound("minecraft:entity.armadillo.brush", 6, pos, 1.0, 1.0);
                }));
                return Use::Damage(16);
            }
            "cod" | "salmon" | "pufferfish" | "tropical_fish" | "axolotl" | "tadpole" if name == "water_bucket" && m.mo.in_water => {
                // Bucketable.bucketMobPickup
                let bucket = format!("{ty}_bucket");
                let s = match ty {
                    "axolotl" => "minecraft:item.bucket.fill_axolotl",
                    "tadpole" => "minecraft:item.bucket.fill_tadpole",
                    _ => "minecraft:item.bucket.fill_fish",
                };
                sound(after, s, false, 1.0);
                m.death = 19;
                after.push(Box::new(move |g: &Game| {
                    let c = g.entities.lock().remove(&eid).map(|e| e.chunk());
                    if let Some(c) = c {
                        g.broadcast_chunk(c, &g.remove_packet(&[eid]));
                    }
                }));
                return Use::Exchange(Stack::new(items.id(&bucket).unwrap_or(0), 1));
            }
            "wolf" | "cat" | "parrot" if !m.tame => {
                let (taming, chance) = match ty {
                    "wolf" => (name == "bone", 3),
                    "cat" => (self.is_breeding_food("cat", id), 3),
                    _ => (self.data.item_in("parrot_food", id), 10),
                };
                if ty == "parrot" && self.data.item_in("parrot_poisonous_food", id) {
                    after.push(Box::new(move |g: &Game| {
                        g.add_effect(eid, Instance::new(Effect::Poison, 900, 0));
                    }));
                    return Use::Consume;
                }
                if taming && !(ty == "wolf" && m.v.anger_target.is_some()) {
                    if rng.gen_range(0..chance) == 0 {
                        m.tame = true;
                        m.owner = Some(p.uuid);
                        m.persistent = true;
                        m.ordered_sit = true;
                        m.sitting = true;
                        m.target = None;
                        m.nav.stop();
                        event(after, 7);
                    } else {
                        event(after, 6);
                    }
                    return Use::Consume;
                }
            }
            "ocelot" if !m.v.trusting && self.is_breeding_food("ocelot", id) => {
                // Ocelot.mobInteract: trust after a 1/3 roll, when approached slowly
                if rng.gen_range(0..3) == 0 {
                    m.v.trusting = true;
                    m.persistent = true;
                    event(after, 41);
                } else {
                    event(after, 40);
                }
                return Use::Consume;
            }
            _ => {}
        }
        // tamed animals: healing food, collar dye, sit toggle
        if m.tame && is_owner {
            if food && m.living.health < m.max_health() {
                let heal = held.and_then(|s| s.defaults().raw.get("food").and_then(|f| f.get("nutrition")).and_then(|n| n.as_f64())).unwrap_or(2.0) as f32;
                m.living.heal(if ty == "wolf" { 2.0 * heal } else { heal });
                return Use::Consume;
            }
            if ty == "wolf" || ty == "cat" {
                if let Some(c) = DYES.iter().position(|d| *d == name) {
                    if m.v.collar != c as u8 {
                        m.v.collar = c as u8;
                        return Use::Consume;
                    }
                }
            }
            if !food || m.in_love() || m.age != 0 {
                m.ordered_sit = !m.ordered_sit;
                m.sitting = m.ordered_sit;
                m.mo.jumping = false;
                m.nav.stop();
                m.target = None;
                return Use::Success;
            }
        }
        // Animal.mobInteract: breeding food
        if food && m.ty.ageable() {
            if m.age == 0 && !m.in_love() && (!m.ty.tamable() || m.tame) {
                // setInLove
                m.love = 600;
                m.love_cause = Some(p.eid);
                event(after, 18);
                let _ = creative;
                return Use::Consume;
            }
            if m.is_baby() {
                // ageUp(getSpeedUpSecondsWhenFeeding(-age))
                let secs = ((-m.age) as f32 / 20.0 * 0.1) as i32;
                m.age = (m.age + secs * 20).min(0);
                if m.age == 0 {
                    super::ai::mobs::on_age_change(m);
                }
                return Use::Consume;
            }
        }
        Use::Pass
    }

    /// `Animal.isFood` by the type's food tag.
    pub fn is_breeding_food(&self, ty: &str, item: i32) -> bool {
        let tag = match ty {
            "cow" | "mooshroom" => "cow_food",
            "sheep" => "sheep_food",
            "pig" => "pig_food",
            "chicken" => "chicken_food",
            "rabbit" => "rabbit_food",
            "horse" | "donkey" | "mule" | "skeleton_horse" => "horse_food",
            "llama" | "trader_llama" => "llama_food",
            "camel" => "camel_food",
            "camel_husk" => "camel_husk_food",
            "cat" => "cat_food",
            "ocelot" => "ocelot_food",
            "wolf" => "wolf_food",
            "fox" => "fox_food",
            "panda" => "panda_food",
            "bee" => "bee_food",
            "turtle" => "turtle_food",
            "goat" => "goat_food",
            "frog" => "frog_food",
            "axolotl" => "axolotl_food",
            "strider" => "strider_food",
            "hoglin" => "hoglin_food",
            "armadillo" => "armadillo_food",
            "sniffer" => "sniffer_food",
            "parrot" => "parrot_food",
            "nautilus" => "nautilus_food",
            "zombie_horse" => "zombie_horse_food",
            "happy_ghast" => "happy_ghast_food",
            _ => return false,
        };
        self.data.item_in(tag, item)
    }
}
