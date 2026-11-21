//! Player inventory and container click handling, simulated on the server
//! (the client's prediction is only compared, never trusted).
//!
//! Player menu slots: 0 craft result, 1–4 craft grid, 5–8 armor, 9–35 main,
//! 36–44 hotbar, 45 offhand. A crafting table window uses 0 result, 1–9 grid,
//! then 10–36 main and 37–45 hotbar.

use crate::data::{GameData, Items, Stack};
use super::blockentity::{self, Screen};

pub const SLOTS: usize = 46;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Player,
    /// crafting table (window id)
    Crafting(u8),
    /// a block container: window id, screen, block position
    Container(u8, Screen, [i32; 3]),
}

pub struct Inventory {
    pub slots: [Option<Stack>; SLOTS],
    /// 3x3 grid of an open crafting table
    pub table: [Option<Stack>; 10],
    pub cursor: Option<Stack>,
    pub state_id: i32,
    pub window: Window,
    /// contents of an open block container (`Window::Container`)
    pub ext: Vec<Option<Stack>>,
    /// slots changed since the last sync: (window id, slot)
    pub dirty: Vec<(u8, i16)>,
    drag: Option<(u8, Vec<i16>)>,
    /// furnace shift-click target decided by the caller (needs recipes)
    quick_furnace_target: Option<Vec<usize>>,
}

impl Default for Inventory {
    fn default() -> Self {
        Self { slots: [None; SLOTS], table: [None; 10], cursor: None, state_id: 0, window: Window::Player, ext: Vec::new(), dirty: Vec::new(), drag: None, quick_furnace_target: None }
    }
}

/// Result of a click: stacks the server dropped into the world (thrown items).
#[derive(Default, Debug)]
pub struct ClickOutcome {
    pub thrown: Vec<Stack>,
}

impl Inventory {
    pub fn held(&self, hotbar: i16) -> Option<Stack> {
        self.slots[36 + hotbar.clamp(0, 8) as usize]
    }

    pub fn set(&mut self, slot: usize, s: Option<Stack>) {
        let s = s.filter(|s| s.count > 0 && Items::get().valid(s.item));
        if self.slots[slot] != s {
            self.slots[slot] = s;
            self.dirty.push((0, slot as i16));
        }
    }

    /// Take `n` items from a slot (e.g. placing a block in survival).
    pub fn consume(&mut self, slot: usize, n: u8) {
        if let Some(mut s) = self.slots[slot] {
            s.count = s.count.saturating_sub(n);
            self.set(slot, Some(s));
        }
    }

    /// Insert items in vanilla order (selected slot, offhand, hotbar, main;
    /// stacking first, then empty slots). Returns what did not fit.
    pub fn add(&mut self, mut st: Stack, selected: i16) -> u8 {
        let max = st.max();
        let order: Vec<usize> = std::iter::once(36 + selected.clamp(0, 8) as usize).chain(std::iter::once(45)).chain(36..45).chain(9..36).collect();
        for &i in &order {
            if let Some(mut s) = self.slots[i] {
                if s.item == st.item && s.count < max {
                    let n = (max - s.count).min(st.count);
                    s.count += n;
                    st.count -= n;
                    self.set(i, Some(s));
                    if st.count == 0 {
                        return 0;
                    }
                }
            }
        }
        for i in (36..45).chain(9..36) {
            if self.slots[i].is_none() {
                let n = st.count.min(max);
                self.set(i, Some(Stack::new(st.item, n)));
                st.count -= n;
                if st.count == 0 {
                    return 0;
                }
            }
        }
        st.count
    }

    // ---------------------------------------------------------- windows

    fn window_id(&self) -> u8 {
        match self.window {
            Window::Player => 0,
            Window::Crafting(id) | Window::Container(id, ..) => id,
        }
    }

    fn container(&self) -> Option<(Screen, usize)> {
        match self.window {
            Window::Container(_, sc, _) => Some((sc, sc.size())),
            _ => None,
        }
    }

    /// Can `item` go into window slot `slot` (furnace fuel / output rules)?
    fn may_place(&self, slot: usize, item: i32, data: &GameData) -> bool {
        match self.window {
            Window::Container(_, Screen::Furnace(_), _) => match slot {
                2 => false,
                1 => {
                    let n = Items::get().name(item);
                    n == "bucket" || blockentity::fuel(n, data) > 0
                }
                _ => true,
            },
            Window::Container(_, _, _) => true,
            _ => true,
        }
    }

    /// The stack a window slot holds, for syncing (`win` must be the open window).
    pub fn sync_slot(&self, win: u8, slot: i16) -> Option<Stack> {
        if win == 0 {
            return self.slots.get(slot as usize).copied().flatten();
        }
        if win != self.window_id() {
            return None;
        }
        self.get_w(slot as usize)
    }

    fn window_len(&self) -> usize {
        match self.window {
            Window::Player => SLOTS,
            Window::Crafting(_) => 46,
            Window::Container(_, sc, _) => sc.size() + 36,
        }
    }

    /// Window slot → storage. Crafting table: 0..=9 table, 10..=45 → player 9..=44.
    fn get_w(&self, slot: usize) -> Option<Stack> {
        match self.window {
            Window::Player => self.slots[slot],
            Window::Crafting(_) if slot <= 9 => self.table[slot],
            Window::Crafting(_) => self.slots[slot - 1],
            Window::Container(_, sc, _) => {
                let n = sc.size();
                if slot < n {
                    self.ext.get(slot).copied().flatten()
                } else if slot < n + 27 {
                    self.slots[9 + slot - n]
                } else {
                    self.slots.get(36 + slot - n - 27).copied().flatten()
                }
            }
        }
    }

    fn set_w(&mut self, slot: usize, s: Option<Stack>) {
        let s = s.filter(|s| s.count > 0);
        match self.window {
            Window::Player => self.set(slot, s),
            Window::Crafting(id) if slot <= 9 => {
                if self.table[slot] != s {
                    self.table[slot] = s;
                    self.dirty.push((id, slot as i16));
                }
            }
            Window::Crafting(_) => self.set(slot - 1, s),
            Window::Container(id, sc, _) => {
                let n = sc.size();
                if slot < n {
                    if self.ext.len() < n {
                        self.ext.resize(n, None);
                    }
                    if self.ext[slot] != s {
                        self.ext[slot] = s;
                        self.dirty.push((id, slot as i16));
                    }
                } else if slot < n + 27 {
                    self.set(9 + slot - n, s)
                } else if slot < n + 36 {
                    self.set(36 + slot - n - 27, s)
                }
            }
        }
    }

    fn grid(&self) -> (Vec<usize>, usize) {
        match self.window {
            Window::Player => ((1..=4).collect(), 2),
            Window::Crafting(_) => ((1..=9).collect(), 3),
            Window::Container(..) => (Vec::new(), 0),
        }
    }

    fn is_result(&self, slot: usize) -> bool {
        slot == 0 && self.container().is_none()
    }

    fn is_armor(&self, slot: usize) -> bool {
        self.window == Window::Player && (5..=8).contains(&slot)
    }

    /// Recompute the crafting result slot.
    pub fn update_craft(&mut self, data: &GameData) {
        let (cells, size) = self.grid();
        if cells.is_empty() {
            return;
        }
        let grid: Vec<Option<i32>> = cells.iter().map(|&i| self.get_w(i).map(|s| s.item)).collect();
        let r = data.craft(&grid, size);
        self.set_w(0, r);
    }

    fn take_result(&mut self, data: &GameData) -> Option<Stack> {
        let r = self.get_w(0)?;
        let (cells, _) = self.grid();
        for i in cells {
            if let Some(mut s) = self.get_w(i) {
                s.count -= 1;
                self.set_w(i, Some(s));
            }
        }
        self.update_craft(data);
        Some(r)
    }

    /// Where shift-click moves a stack, in priority order.
    fn quick_targets(&self, slot: usize) -> Vec<usize> {
        match self.window {
            Window::Player => match slot {
                0 => (9..45).rev().collect(),
                1..=8 | 45 => (9..45).collect(),
                9..=35 => (36..45).collect(),
                _ => (9..36).collect(),
            },
            Window::Crafting(_) => match slot {
                0 => (10..46).rev().collect(),
                1..=9 => (10..46).collect(),
                10..=36 => (37..46).collect(),
                _ => (10..37).collect(),
            },
            Window::Container(_, Screen::Furnace(_), _) => match slot {
                2 => (3..39).rev().collect(),
                0 | 1 => (3..39).collect(),
                _ => {
                    // smeltable → input, fuel → fuel slot, else inventory <-> hotbar
                    if self.quick_furnace_target.is_some() {
                        self.quick_furnace_target.clone().unwrap_or_default()
                    } else if slot < 30 {
                        (30..39).collect()
                    } else {
                        (3..30).collect()
                    }
                }
            },
            Window::Container(_, sc, _) => {
                let n = sc.size();
                if slot < n {
                    (n..n + 36).rev().collect()
                } else {
                    (0..n).collect()
                }
            }
        }
    }

    fn move_into(&mut self, mut st: Stack, targets: &[usize]) -> u8 {
        let max = st.max();
        for &t in targets {
            if let Some(mut s) = self.get_w(t) {
                if s.item == st.item && s.count < max {
                    let n = (max - s.count).min(st.count);
                    s.count += n;
                    st.count -= n;
                    self.set_w(t, Some(s));
                    if st.count == 0 {
                        return 0;
                    }
                }
            }
        }
        for &t in targets {
            if self.get_w(t).is_none() {
                self.set_w(t, Some(st));
                return 0;
            }
        }
        st.count
    }

    /// Apply a `container_click`. `creative` enables clone-click.
    pub fn click(&mut self, data: &GameData, slot: i16, button: i8, mode: i32, creative: bool) -> ClickOutcome {
        let mut out = ClickOutcome::default();
        let len = self.window_len() as i16;
        let valid = (0..len).contains(&slot);
        match mode {
            // ---- normal click
            0 => {
                if slot == -999 {
                    if let Some(mut c) = self.cursor {
                        if button == 0 {
                            out.thrown.push(c);
                            self.cursor = None;
                        } else {
                            out.thrown.push(Stack::new(c.item, 1));
                            c.count -= 1;
                            self.cursor = (c.count > 0).then_some(c);
                        }
                    }
                    return out;
                }
                if !valid {
                    return out;
                }
                let s = slot as usize;
                if self.is_result(s) {
                    if let Some(r) = self.get_w(0) {
                        match self.cursor {
                            None => self.cursor = self.take_result(data),
                            Some(mut c) if c.item == r.item && c.count as u16 + r.count as u16 <= c.max() as u16 => {
                                self.take_result(data);
                                c.count += r.count;
                                self.cursor = Some(c);
                            }
                            _ => {}
                        }
                    }
                    return out;
                }
                let cur = self.get_w(s);
                let placeable = |st: &Stack| self.may_place(s, st.item, data);
                let armor_ok = |st: &Stack| (!self.is_armor(s) || armor_slot_for(st.item) == Some(s)) && placeable(st);
                match (cur, self.cursor, button) {
                    (None, None, _) => {}
                    (Some(c), None, 0) => {
                        self.cursor = Some(c);
                        self.set_w(s, None);
                    }
                    (Some(mut c), None, _) => {
                        let take = c.count.div_ceil(2);
                        self.cursor = Some(Stack::new(c.item, take));
                        c.count -= take;
                        self.set_w(s, Some(c));
                    }
                    (None, Some(mut k), b) => {
                        if !armor_ok(&k) {
                            return out;
                        }
                        let n = if b == 0 { k.count } else { 1 };
                        let n = n.min(if self.is_armor(s) { 1 } else { k.max() });
                        self.set_w(s, Some(Stack::new(k.item, n)));
                        k.count -= n;
                        self.cursor = (k.count > 0).then_some(k);
                    }
                    (Some(mut c), Some(mut k), b) => {
                        if c.item == k.item && placeable(&k) {
                            let room = c.max().saturating_sub(c.count);
                            let n = if b == 0 { k.count.min(room) } else { 1.min(room) };
                            c.count += n;
                            k.count -= n;
                            self.set_w(s, Some(c));
                            self.cursor = (k.count > 0).then_some(k);
                        } else if armor_ok(&k) && k.count <= if self.is_armor(s) { 1 } else { k.max() } {
                            self.set_w(s, Some(k));
                            self.cursor = Some(c);
                        }
                    }
                }
            }
            // ---- shift click
            1 if valid => {
                let s = slot as usize;
                if self.is_result(s) {
                    // craft as many as fit
                    for _ in 0..64 {
                        let Some(r) = self.get_w(0) else { break };
                        let targets = self.quick_targets(0);
                        if !self.fits(r, &targets) {
                            break;
                        }
                        let r = self.take_result(data).expect("result present");
                        self.move_into(r, &targets);
                    }
                    return out;
                }
                if let Some(st) = self.get_w(s) {
                    self.quick_furnace_target = None;
                    if let Window::Container(_, Screen::Furnace(kind), _) = self.window {
                        if s >= 3 {
                            if data.cook(kind, st.item).is_some() {
                                self.quick_furnace_target = Some(vec![0]);
                            } else if self.may_place(1, st.item, data) {
                                self.quick_furnace_target = Some(vec![1]);
                            }
                        }
                    }
                    let targets = self.quick_targets(s);
                    self.quick_furnace_target = None;
                    let left = self.move_into(st, &targets);
                    self.set_w(s, (left > 0).then_some(Stack::new(st.item, left)));
                }
            }
            // ---- number key swap (button 0-8 hotbar, 40 offhand)
            2 if valid => {
                let s = slot as usize;
                let hot = match button {
                    0..=8 => 36 + button as usize,
                    40 => 45,
                    _ => return out,
                };
                let hot_w = match self.window {
                    Window::Player => hot,
                    Window::Crafting(_) if hot == 45 => return out,
                    Window::Crafting(_) => hot + 1,
                    Window::Container(_, sc, _) if hot == 45 => {
                        let _ = sc;
                        return out;
                    }
                    Window::Container(_, sc, _) => sc.size() + 27 + (hot - 36),
                };
                if self.get_w(hot_w).is_some_and(|h| !self.may_place(s, h.item, data)) {
                    return out;
                }
                if self.is_result(s) {
                    if self.get_w(hot_w).is_none() {
                        if let Some(r) = self.take_result(data) {
                            self.set_w(hot_w, Some(r));
                        }
                    }
                    return out;
                }
                let (a, b) = (self.get_w(s), self.get_w(hot_w));
                if self.is_armor(s) && b.is_some_and(|b| armor_slot_for(b.item) != Some(s)) {
                    return out;
                }
                self.set_w(s, b);
                self.set_w(hot_w, a);
            }
            // ---- creative middle-click clone
            3 if valid && creative && self.cursor.is_none() => {
                if let Some(st) = self.get_w(slot as usize) {
                    self.cursor = Some(Stack::new(st.item, st.max()));
                }
            }
            // ---- drop key (Q / ctrl+Q)
            4 if valid => {
                let s = slot as usize;
                if let Some(mut st) = self.get_w(s) {
                    if self.is_result(s) {
                        if let Some(r) = self.take_result(data) {
                            out.thrown.push(r);
                        }
                        return out;
                    }
                    let n = if button == 1 { st.count } else { 1 };
                    out.thrown.push(Stack::new(st.item, n));
                    st.count -= n;
                    self.set_w(s, Some(st));
                }
            }
            // ---- drag (quick craft)
            5 => {
                let stage = button & 3;
                let kind = (button >> 2) as u8; // 0 left, 1 right, 2 middle
                match stage {
                    0 => self.drag = Some((kind, Vec::new())),
                    1 => {
                        if let Some((k, list)) = &mut self.drag {
                            if *k == kind && valid && !list.contains(&slot) && slot != 0 {
                                list.push(slot);
                            }
                        }
                    }
                    2 => {
                        if let (Some((k, list)), Some(mut c)) = (self.drag.take(), self.cursor) {
                            if k == 2 && !creative {
                                return out;
                            }
                            let list: Vec<usize> = list
                                .into_iter()
                                .map(|s| s as usize)
                                .filter(|&s| self.get_w(s).is_none_or(|x| x.item == c.item) && (!self.is_armor(s) || armor_slot_for(c.item) == Some(s)) && self.may_place(s, c.item, data))
                                .collect();
                            if list.is_empty() {
                                return out;
                            }
                            let per = match k {
                                0 => (c.count as usize / list.len()).max(1) as u8,
                                1 => 1,
                                _ => c.max(),
                            };
                            for s in list {
                                if k != 2 && c.count == 0 {
                                    break;
                                }
                                let cur = self.get_w(s).map(|x| x.count).unwrap_or(0);
                                let room = c.max().saturating_sub(cur);
                                let n = if k == 2 { room } else { per.min(room).min(c.count) };
                                self.set_w(s, Some(Stack::new(c.item, cur + n)));
                                if k != 2 {
                                    c.count -= n;
                                }
                            }
                            self.cursor = (c.count > 0).then_some(c);
                        }
                    }
                    _ => self.drag = None,
                }
            }
            // ---- double click: gather matching items into the cursor
            6 => {
                if let Some(mut c) = self.cursor {
                    let first = if self.container().is_some() { 0 } else { 1 };
                    for pass in 0..2 {
                        for s in first..len as usize {
                            if self.container().is_some_and(|(sc, _)| matches!(sc, Screen::Furnace(_))) && s == 2 {
                                continue;
                            }
                            if c.count >= c.max() {
                                break;
                            }
                            if let Some(mut st) = self.get_w(s) {
                                // full stacks are taken on the second pass only
                                if st.item == c.item && (pass == 1 || st.count < st.max()) {
                                    let n = (c.max() - c.count).min(st.count);
                                    c.count += n;
                                    st.count -= n;
                                    self.set_w(s, Some(st));
                                }
                            }
                        }
                    }
                    self.cursor = Some(c);
                }
            }
            _ => {}
        }
        // any change to the grid updates the result
        self.update_craft(data);
        let _ = self.window_id();
        out
    }

    fn fits(&self, st: Stack, targets: &[usize]) -> bool {
        let mut need = st.count as i32;
        for &t in targets {
            match self.get_w(t) {
                None => need -= st.max() as i32,
                Some(s) if s.item == st.item => need -= (s.max() - s.count) as i32,
                _ => {}
            }
            if need <= 0 {
                return true;
            }
        }
        false
    }

    /// Close the current window: grid items and cursor go back to the
    /// inventory; what doesn't fit is thrown.
    pub fn close(&mut self, data: &GameData, selected: i16) -> Vec<Stack> {
        let mut back: Vec<Stack> = Vec::new();
        let (cells, _) = self.grid();
        for i in cells {
            if let Some(s) = self.get_w(i) {
                back.push(s);
                self.set_w(i, None);
            }
        }
        self.set_w(0, None);
        if let Some(c) = self.cursor.take() {
            back.push(c);
        }
        self.window = Window::Player;
        self.table = [None; 10];
        self.ext.clear();
        self.update_craft(data);
        let mut thrown = Vec::new();
        for s in back {
            let left = self.add(s, selected);
            if left > 0 {
                thrown.push(Stack::new(s.item, left));
            }
        }
        thrown
    }

    /// Contents of the open window in window-slot order.
    pub fn window_contents(&self) -> Vec<Option<Stack>> {
        (0..self.window_len()).map(|i| self.get_w(i)).collect()
    }

    pub fn current_window(&self) -> u8 {
        self.window_id()
    }
}

fn armor_slot_for(item: i32) -> Option<usize> {
    let n = Items::get().name(item);
    if n.ends_with("_helmet") || n == "turtle_helmet" || n.ends_with("_head") || n.ends_with("_skull") || n == "carved_pumpkin" {
        Some(5)
    } else if n.ends_with("_chestplate") || n == "elytra" {
        Some(6)
    } else if n.ends_with("_leggings") {
        Some(7)
    } else if n.ends_with("_boots") {
        Some(8)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: &str) -> i32 {
        Items::get().id(n).unwrap()
    }

    #[test]
    fn add_stacks_then_fills_hotbar_first() {
        let mut inv = Inventory::default();
        assert_eq!(inv.add(Stack::new(id("dirt"), 70), 0), 0);
        assert_eq!(inv.slots[36], Some(Stack::new(id("dirt"), 64)));
        assert_eq!(inv.slots[37], Some(Stack::new(id("dirt"), 6)));
        assert_eq!(inv.add(Stack::new(id("dirt"), 10), 0), 0);
        assert_eq!(inv.slots[37], Some(Stack::new(id("dirt"), 16)));
        assert_eq!(inv.add(Stack::new(id("ender_pearl"), 20), 0), 0);
        assert_eq!(inv.slots[38].unwrap().count, 16);
    }

    #[test]
    fn full_inventory_reports_leftover() {
        let mut inv = Inventory::default();
        for i in 9..45 {
            inv.slots[i] = Some(Stack::new(id("stone"), 64));
        }
        inv.slots[45] = Some(Stack::new(id("stone"), 64));
        assert_eq!(inv.add(Stack::new(id("dirt"), 5), 0), 5);
    }

    #[test]
    fn clicks_pick_place_split_swap() {
        let d = GameData::empty();
        let mut inv = Inventory::default();
        inv.slots[36] = Some(Stack::new(id("dirt"), 10));
        inv.click(&d, 36, 1, 0, false); // right click: take half
        assert_eq!(inv.cursor, Some(Stack::new(id("dirt"), 5)));
        assert_eq!(inv.slots[36].unwrap().count, 5);
        inv.click(&d, 9, 1, 0, false); // right click on empty: place one
        assert_eq!(inv.slots[9].unwrap().count, 1);
        inv.click(&d, 36, 0, 0, false); // left click same item: merge
        assert_eq!(inv.slots[36].unwrap().count, 9);
        assert_eq!(inv.cursor, None);
        inv.click(&d, 9, 0, 2, false); // hotbar key 1 swap
        assert_eq!(inv.slots[36], Some(Stack::new(id("dirt"), 1)));
        assert_eq!(inv.slots[9], Some(Stack::new(id("dirt"), 9)));
        let o = inv.click(&d, 9, 1, 4, false); // ctrl+Q
        assert_eq!(o.thrown, vec![Stack::new(id("dirt"), 9)]);
        assert_eq!(inv.slots[9], None);
        // dirt can't go into the helmet slot
        inv.click(&d, 36, 0, 0, false);
        inv.click(&d, 5, 0, 0, false);
        assert_eq!(inv.slots[5], None);
    }

    #[test]
    fn crafting_in_the_2x2_grid() {
        let mut d = GameData::empty();
        let (log, planks) = (id("oak_log"), id("oak_planks"));
        d.item_tags.insert("oak_logs".into(), [log].into_iter().collect());
        let r = serde_json::json!({"type":"minecraft:crafting_shapeless","ingredients":["#minecraft:oak_logs"],"result":{"count":4,"id":"minecraft:oak_planks"}});
        d.recipes.push(crate::data::tests_support::parse(&d, &r));
        let mut inv = Inventory::default();
        inv.slots[36] = Some(Stack::new(log, 3));
        inv.click(&d, 36, 0, 0, false);
        inv.click(&d, 1, 0, 0, false); // put 3 logs into the grid
        assert_eq!(inv.slots[0], Some(Stack::new(planks, 4)));
        inv.click(&d, 0, 0, 1, false); // shift-click result: craft all
        assert_eq!(inv.slots[1], None);
        assert_eq!(inv.slots[0], None);
        // vanilla fills back to front from the result slot
        assert_eq!(inv.slots[44], Some(Stack::new(planks, 12)));
    }

    #[test]
    fn drag_distributes_evenly() {
        let d = GameData::empty();
        let mut inv = Inventory::default();
        inv.cursor = Some(Stack::new(id("dirt"), 9));
        inv.click(&d, -999, 0, 5, false);
        for s in [9, 10, 11] {
            inv.click(&d, s, 1, 5, false);
        }
        inv.click(&d, -999, 2, 5, false);
        assert_eq!(inv.slots[9].unwrap().count, 3);
        assert_eq!(inv.slots[11].unwrap().count, 3);
        assert_eq!(inv.cursor, None);
    }
}
