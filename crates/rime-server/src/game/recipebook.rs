//! The recipe book (`ServerRecipeBook`): unlocked crafting recipes are sent
//! with `recipe_book_add` (display data the client draws), recipe
//! advancements unlock them, and `place_recipe` fills the crafting grid.

use super::{Game, Player};
use crate::data::{Ingredient, Recipe, RecipeShape, Stack};
use crate::net::codec::Out;
use crate::registry::Registries;
use std::sync::Arc;

fn reg(r: &str, name: &str) -> i32 {
    Registries::get().id(r, name).unwrap_or(0)
}

fn sorted(i: &Ingredient) -> Vec<i32> {
    let mut v: Vec<i32> = i.0.iter().copied().collect();
    v.sort_unstable();
    v
}

/// `SlotDisplay` of an ingredient: one item, or the alternatives in turn.
fn slot_ingredient(b: &mut Vec<u8>, i: Option<&Ingredient>) {
    match i.map(sorted) {
        None => {
            b.vi(reg("minecraft:slot_display", "minecraft:empty"));
        }
        Some(items) if items.len() == 1 => {
            b.vi(reg("minecraft:slot_display", "minecraft:item")).vi(items[0]);
        }
        Some(items) => {
            b.vi(reg("minecraft:slot_display", "minecraft:composite")).vi(items.len() as i32);
            for it in items {
                b.vi(reg("minecraft:slot_display", "minecraft:item")).vi(it);
            }
        }
    }
}

fn slot_stack(b: &mut Vec<u8>, s: Stack) {
    b.vi(reg("minecraft:slot_display", "minecraft:item_stack")).vi(s.item).vi(s.count as i32).vi(0).vi(0);
}

fn slot_item(b: &mut Vec<u8>, name: &str) {
    let id = crate::data::Items::get().id(name).unwrap_or(0);
    b.vi(reg("minecraft:slot_display", "minecraft:item")).vi(id);
}

/// `RecipeDisplayEntry` of a crafting recipe.
fn write_entry(b: &mut Vec<u8>, idx: usize, r: &Recipe, groups: &mut Vec<String>) {
    b.vi(idx as i32);
    let ings: Vec<Option<&Ingredient>> = match &r.shape {
        RecipeShape::Shaped { w, h, cells } => {
            b.vi(reg("minecraft:recipe_display", "minecraft:crafting_shaped")).vi(*w as i32).vi(*h as i32);
            b.vi(cells.len() as i32);
            for c in cells {
                slot_ingredient(b, c.as_ref());
            }
            cells.iter().map(|c| c.as_ref()).collect()
        }
        RecipeShape::Shapeless(list) => {
            b.vi(reg("minecraft:recipe_display", "minecraft:crafting_shapeless")).vi(list.len() as i32);
            for c in list {
                slot_ingredient(b, Some(c));
            }
            list.iter().map(Some).collect()
        }
    };
    slot_stack(b, r.result);
    slot_item(b, "crafting_table");
    // group: OPTIONAL_VAR_INT (0 = none)
    if r.group.is_empty() {
        b.vi(0);
    } else {
        let g = groups.iter().position(|x| *x == r.group).unwrap_or_else(|| {
            groups.push(r.group.clone());
            groups.len() - 1
        });
        b.vi(g as i32 + 1);
    }
    let cat = match r.category.as_str() {
        "building" => "minecraft:crafting_building_blocks",
        "redstone" => "minecraft:crafting_redstone",
        "equipment" => "minecraft:crafting_equipment",
        _ => "minecraft:crafting_misc",
    };
    b.vi(reg("minecraft:recipe_book_category", cat));
    // crafting requirements: the ingredients (for "craftable" highlighting)
    let reqs: Vec<&Ingredient> = ings.into_iter().flatten().collect();
    b.bool(true).vi(reqs.len() as i32);
    for i in reqs {
        let items = sorted(i);
        b.vi(items.len() as i32 + 1);
        for it in items {
            b.vi(it);
        }
    }
}

impl Game {
    fn recipe_index(&self, id: &str) -> Option<usize> {
        let id = id.strip_prefix("minecraft:").unwrap_or(id);
        self.data.recipes.iter().position(|r| r.id == id)
    }

    /// `ServerRecipeBook.addRecipes`
    pub fn unlock_recipes(&self, p: &Arc<Player>, ids: &[String]) -> usize {
        let mut n = 0;
        let mut prog = p.adv.lock();
        for id in ids {
            let id = id.strip_prefix("minecraft:").unwrap_or(id).to_string();
            if self.recipe_index(&id).is_some() && prog.recipes.insert(id.clone()) {
                prog.new_recipes.push(id);
                n += 1;
            }
        }
        n
    }

    /// `ServerRecipeBook.removeRecipes`
    pub fn lock_recipes(&self, p: &Arc<Player>, ids: &[String]) -> usize {
        let mut removed = Vec::new();
        {
            let mut prog = p.adv.lock();
            for id in ids {
                let id = id.strip_prefix("minecraft:").unwrap_or(id);
                if prog.recipes.remove(id) {
                    if let Some(i) = self.recipe_index(id) {
                        removed.push(i);
                    }
                }
            }
        }
        if !removed.is_empty() {
            let mut b = Vec::new();
            b.vi(removed.len() as i32);
            for i in &removed {
                b.vi(*i as i32);
            }
            p.send(self.encode("recipe_book_remove", &b));
        }
        removed.len()
    }

    /// Sends newly unlocked recipes (with the toast).
    pub fn flush_recipes(&self, p: &Arc<Player>) {
        let new = std::mem::take(&mut p.adv.lock().new_recipes);
        if new.is_empty() {
            return;
        }
        let idx: Vec<usize> = new.iter().filter_map(|id| self.recipe_index(id)).collect();
        p.send(self.encode("recipe_book_add", &self.recipe_book_body(&idx, 3, false)));
        self.trigger_recipe_unlocked(p);
    }

    /// The whole book on join.
    pub fn send_recipe_book(&self, p: &Arc<Player>) {
        let idx: Vec<usize> = {
            let mut prog = p.adv.lock();
            prog.new_recipes.clear();
            prog.recipes.iter().filter_map(|id| self.recipe_index(id)).collect()
        };
        p.send(self.encode("recipe_book_add", &self.recipe_book_body(&idx, 0, true)));
    }

    fn recipe_book_body(&self, idx: &[usize], flags: u8, replace: bool) -> Vec<u8> {
        let mut groups = Vec::new();
        let mut b = Vec::new();
        b.vi(idx.len() as i32);
        for &i in idx {
            write_entry(&mut b, i, &self.data.recipes[i], &mut groups);
            b.u8(flags);
        }
        b.bool(replace);
        b
    }

    /// `place_recipe`: moves ingredients from the inventory into the
    /// crafting grid (one set, or as many as fit with `max`).
    pub fn place_recipe(&self, p: &Arc<Player>, display: i32, max: bool) {
        let Some(r) = usize::try_from(display).ok().and_then(|i| self.data.recipes.get(i)) else { return };
        if !p.adv.lock().recipes.contains(&r.id) && p.gamemode.load(std::sync::atomic::Ordering::Relaxed) != 1 {
            return;
        }
        let mut inv = p.inv.lock();
        let held = p.held.load(std::sync::atomic::Ordering::Relaxed) as i16;
        // put back what is in the grid first
        let Some((size, cells)) = inv.grid_layout() else { return };
        let back = inv.clear_grid(held);
        if !back.is_empty() {
            let pos = p.pose.lock().pos;
            for s in back {
                self.spawn_item([pos[0], pos[1] + 1.0, pos[2]], s, [0.0, 0.1, 0.0], 40);
            }
        }
        let layout: Vec<Option<&Ingredient>> = match &r.shape {
            RecipeShape::Shaped { w, h, cells: c } => {
                if *w > size || *h > size {
                    return;
                }
                let mut v = vec![None; size * size];
                for y in 0..*h {
                    for x in 0..*w {
                        v[y * size + x] = c[y * w + x].as_ref();
                    }
                }
                v
            }
            RecipeShape::Shapeless(list) => {
                if list.len() > size * size {
                    return;
                }
                let mut v: Vec<Option<&Ingredient>> = list.iter().map(Some).collect();
                v.resize(size * size, None);
                v
            }
        };
        let sets = if max { 64 } else { 1 };
        for _ in 0..sets {
            // every cell must be fillable for another set
            let mut plan = Vec::new();
            let mut ok = true;
            let mut taken: Vec<(usize, u8)> = Vec::new();
            for (ci, ing) in layout.iter().enumerate() {
                let Some(ing) = ing else { continue };
                let cur = inv.get_w(cells[ci]);
                if cur.is_some_and(|c| c.count >= c.max()) {
                    ok = false;
                    break;
                }
                let found = (9..45).find(|&s| {
                    inv.slots[s].is_some_and(|st| ing.0.contains(&st.item) && cur.is_none_or(|c| c.item == st.item && c.comp == st.comp) && st.count as i32 > taken.iter().filter(|t| t.0 == s).map(|t| t.1 as i32).sum::<i32>())
                });
                match found {
                    Some(s) => {
                        taken.push((s, 1));
                        plan.push((ci, s));
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok || plan.is_empty() {
                break;
            }
            for (ci, s) in plan {
                let st = inv.slots[s].unwrap();
                inv.consume(s, 1);
                let w = cells[ci];
                let cur = inv.get_w(w);
                inv.set_w(w, Some(Stack { count: cur.map_or(1, |c| c.count + 1), ..st }));
            }
        }
        inv.update_craft(&self.data);
        drop(inv);
        self.send_inventory(p);
    }
}
