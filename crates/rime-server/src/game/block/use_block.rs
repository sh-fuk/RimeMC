//! `BlockBehaviour.useItemOn` / `useWithoutItem` of interactive blocks,
//! beds (`ServerPlayer.startSleepInBed`, `ServerLevel` sleeping) and signs.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::*;
use crate::data::{Items, Stack};
use crate::game::blockentity::Screen;
use crate::game::Player;
use crate::nbt::Tag;
use crate::net::codec::Out;

/// What the session has to do after a right click on a block.
pub enum Use {
    /// nothing happened: go on with the held item (placing)
    Pass,
    /// handled
    Consumed,
    /// open a container screen (key position, screen, title)
    Open([i32; 3], Screen, &'static str),
    Crafting,
    /// open the sign editor (position, front side)
    Sign([i32; 3], bool),
}

pub struct Ctx<'a> {
    pub player: &'a Arc<Player>,
    pub held: Option<Stack>,
    pub sneaking: bool,
    pub creative: bool,
    /// clicked face
    pub face: u8,
    /// the player's horizontal facing
    pub horiz: u8,
}

fn msg(g: &Game, p: &Player, key: &str, overlay: bool) {
    let mut b = Tag::from_json(&serde_json::json!({ "translate": key })).to_network_bytes();
    b.bool(overlay);
    p.send(g.encode("system_chat", &b));
}

/// Right click on the block at `p`.
pub fn use_block(g: &Game, p: [i32; 3], s: BlockState, cx: &Ctx) -> Use {
    let item = cx.held.map(|h| Items::get().name(h.item)).unwrap_or("");
    // useItemOn: items acting on blocks
    if matches!(item, "flint_and_steel" | "fire_charge") && s.name() == "tnt" {
        g.prime_tnt(p, Some(cx.player.eid));
        g.set_block(p, blocks::AIR, flags::ALL | flags::SUPPRESS_DROPS);
        return Use::Consumed;
    }
    // FlintAndSteelItem.useOn / FireChargeItem.useOn
    if matches!(item, "flint_and_steel" | "fire_charge") {
        let n = s.name();
        let lightable = (n.ends_with("campfire") || n.ends_with("candle") || n.ends_with("candle_cake")) && s.prop("lit") == Some("false") && s.prop("waterlogged") != Some("true");
        let done = if lightable {
            g.set_block(p, with_bool(s, "lit", true), flags::ALL | 8);
            let pos = [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5];
            g.play_sound(if item == "fire_charge" { "minecraft:item.firecharge.use" } else { "minecraft:item.flintandsteel.use" }, 4, pos, 1.0, rand::random::<f32>() * 0.4 + 0.8);
            true
        } else {
            let o = crate::game::level::OFFSETS[cx.face as usize];
            crate::game::portal::light_at(g, [p[0] + o[0], p[1] + o[1], p[2] + o[2]], cx.horiz, item == "fire_charge")
        };
        if done {
            if !cx.creative {
                if item == "fire_charge" {
                    consume_held(g, cx.player, item);
                } else {
                    damage_held(g, cx.player, item);
                }
            }
            return Use::Consumed;
        }
    }
    let secondary = cx.sneaking && cx.held.is_some();
    // EnderEyeItem.useOn
    if item == "ender_eye" && s.name() == "end_portal_frame" && s.prop("eye") == Some("false") {
        let filled = with_bool(s, "eye", true);
        g.set_block(p, filled, flags::CLIENTS);
        g.update_neighbour_for_output_signal(p, filled);
        if !cx.creative {
            consume_held(g, cx.player, "ender_eye");
        }
        g.level_event(1503, p, 0);
        crate::game::portal::try_end_portal(g, p);
        return Use::Consumed;
    }
    if secondary {
        return Use::Pass;
    }
    match class(s) {
        Class::Door => {
            if door::door_use(g, p, s) {
                return Use::Consumed;
            }
            return Use::Pass;
        }
        Class::TrapDoor => {
            if door::trapdoor_use(g, p, s) {
                return Use::Consumed;
            }
            return Use::Pass;
        }
        Class::FenceGate => {
            door::gate_use(g, p, s, cx.horiz);
            return Use::Consumed;
        }
        Class::Lever => {
            redstone::lever_use(g, p, s);
            return Use::Consumed;
        }
        Class::Button => {
            if !flag(s, "powered") {
                redstone::button_press(g, p, s);
            }
            return Use::Consumed;
        }
        Class::Repeater => {
            redstone::repeater_use(g, p, s);
            return Use::Consumed;
        }
        Class::Comparator => {
            redstone::comparator_use(g, p, s);
            return Use::Consumed;
        }
        Class::NoteBlock => {
            if item.ends_with("_head") || item.ends_with("_skull") {
                return Use::Pass;
            }
            redstone::note_use(g, p, s);
            return Use::Consumed;
        }
        Class::Daylight => {
            redstone::daylight_use(g, p, s);
            return Use::Consumed;
        }
        Class::Wire => {
            if redstone::wire_use(g, p, s) {
                return Use::Consumed;
            }
            return Use::Pass;
        }
        Class::RedstoneOre => {
            redstone::redstone_ore_interact(g, p, s);
            return if cx.held.is_some_and(|h| Items::get().block_for(h.item).is_some()) { Use::Pass } else { Use::Consumed };
        }
        Class::Bell => {
            sound(g, p, "minecraft:block.bell.use", 2.0, 1.0);
            g.block_event(p, s, 1, cx.face as i32);
            return Use::Consumed;
        }
        Class::Bed => return bed_use(g, p, s, cx),
        Class::Chest | Class::TrappedChest => {
            let key = container::parts(g, p);
            // ChestBlock.isChestBlockedAt: a conductor above any half
            if key.iter().any(|q| rstate::conductor(g.bs(rel(*q, UP)))) {
                return Use::Consumed;
            }
            return match container::screen_at(g, p) {
                Some((k, sc, t)) => Use::Open(k, sc, t),
                None => Use::Pass,
            };
        }
        _ => {}
    }
    let name = s.name();
    if name == "crafting_table" {
        return Use::Crafting;
    }
    if let Some((sc, t)) = crate::game::blockentity::workstation_of(name) {
        return Use::Open(p, sc, t);
    }
    if let Some((sc, t)) = crate::game::blockentity::screen_of(name) {
        if name.ends_with("shulker_box") && !shulker_can_open(g, p, s) {
            return Use::Consumed;
        }
        return Use::Open(p, sc, t);
    }
    if name == "cake" || name.ends_with("candle_cake") {
        return cake_use(g, p, s, cx);
    }
    if name.ends_with("_sign") && !name.contains("hanging") {
        let t = g.world.block_entity(p);
        if t.as_ref().and_then(|t| t.get("is_waxed")).and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
            return Use::Consumed;
        }
        if let Some(r) = sign_item_use(g, p, s, cx, item) {
            return r;
        }
        return Use::Sign(p, sign_front(g, p, s, cx.player));
    }
    Use::Pass
}

/// `ShulkerBoxBlock.canOpen`: room to open the lid
fn shulker_can_open(g: &Game, p: [i32; 3], s: BlockState) -> bool {
    let f = facing(s);
    let q = rel(p, f);
    let b = g.bs(q);
    !vstate::full_block(b) || b.is_air()
}

// ------------------------------------------------------------ cake

fn cake_use(g: &Game, p: [i32; 3], s: BlockState, cx: &Ctx) -> Use {
    let name = s.name();
    let item = cx.held.map(|h| Items::get().name(h.item)).unwrap_or("");
    if name == "cake" && item.ends_with("candle") && int(s, "bites") == 0 {
        if let Some(b) = BlockRegistry::get().by_name(&format!("{item}_cake")) {
            g.set_block(p, BlockState(b.default), flags::ALL);
            sound(g, p, "minecraft:block.cake.add_candle", 1.0, 1.0);
            if !cx.creative {
                let slot = 36 + cx.player.held.load(Ordering::Relaxed) as usize;
                cx.player.inv.lock().consume(slot, 1);
                g.flush_inventory(cx.player);
            }
            return Use::Consumed;
        }
    }
    let mut v = cx.player.vit.lock();
    if !(cx.creative || v.food < 20) {
        return Use::Pass;
    }
    v.eat(2, 0.1);
    v.dirty = true;
    drop(v);
    if name.ends_with("candle_cake") {
        // CandleCakeBlock: eating drops the candle
        if let Some(c) = Items::get().id(name.trim_end_matches("_cake")) {
            g.spawn_item([p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5], Stack::new(c, 1), [0.0, 0.1, 0.0], 10);
        }
    }
    let bites = if name == "cake" { int(s, "bites") } else { 0 };
    if bites < 6 {
        let cake = BlockRegistry::get().by_name("cake").map(|b| BlockState(b.default)).unwrap_or(s);
        g.set_block(p, with_int(cake, "bites", bites + 1), flags::ALL);
    } else {
        g.remove_block(p, false);
    }
    Use::Consumed
}

// ------------------------------------------------------------ beds

/// the head position of the bed at `p`
pub fn bed_head(p: [i32; 3], s: BlockState) -> [i32; 3] {
    if prop(s, "part") == "head" {
        p
    } else {
        rel(p, facing(s))
    }
}

fn bed_use(g: &Game, p: [i32; 3], s: BlockState, cx: &Ctx) -> Use {
    let head = bed_head(p, s);
    let hs = g.bs(head);
    if class(hs) != Class::Bed {
        return Use::Consumed;
    }
    if flag(hs, "occupied") {
        msg(g, cx.player, "block.minecraft.bed.occupied", true);
        return Use::Consumed;
    }
    start_sleep(g, cx.player, head, hs);
    Use::Consumed
}

/// `ServerPlayer.startSleepInBed`
fn start_sleep(g: &Game, pl: &Arc<Player>, head: [i32; 3], hs: BlockState) {
    let pos = pl.pose.lock().pos;
    let c = [head[0] as f64 + 0.5, head[1] as f64, head[2] as f64 + 0.5];
    // bedInRange: within 3 horizontally / 2 vertically of the bed's centre
    let near = |q: [f64; 3]| (pos[0] - q[0]).abs() <= 3.0 && (pos[1] - q[1]).abs() <= 2.0 && (pos[2] - q[2]).abs() <= 3.0;
    let foot = rel(head, opp(facing(hs)));
    let fc = [foot[0] as f64 + 0.5, foot[1] as f64, foot[2] as f64 + 0.5];
    if !near(c) && !near(fc) {
        msg(g, pl, "block.minecraft.bed.too_far_away", true);
        return;
    }
    if rstate::suffocating(g.bs(rel(head, UP))) {
        msg(g, pl, "block.minecraft.bed.obstructed", true);
        return;
    }
    // setRespawnPosition
    {
        let mut v = pl.vit.lock();
        if v.respawn != Some(head) {
            v.respawn = Some(head);
            drop(v);
            msg(g, pl, "block.minecraft.set_spawn", false);
        }
    }
    if g.is_day() && !g.weather.lock().thundering {
        msg(g, pl, "block.minecraft.bed.no_sleep", true);
        return;
    }
    if pl.gamemode.load(Ordering::Relaxed) != 1 && g.monster_near_bed(head) {
        msg(g, pl, "block.minecraft.bed.not_safe", true);
        return;
    }
    // LivingEntity.startSleeping
    g.set_block(head, with_bool(hs, "occupied", true), flags::ALL);
    pl.vit.lock().sleeping = Some((head, 0));
    pl.requests.lock().push(crate::game::Request::Teleport([c[0], head[1] as f64 + 0.6875, c[2]], 0.0, 0.0));
    g.broadcast_sleep_pose(pl, Some(head));
}

/// `BedBlock.findStandUpPosition` (simplified: the free sides of the bed)
pub fn stand_up_pos(g: &Game, head: [i32; 3]) -> Option<[f64; 3]> {
    let hs = g.bs(head);
    if class(hs) != Class::Bed {
        return None;
    }
    let f = facing(hs);
    let foot = rel(head, opp(f));
    let free = |q: [i32; 3]| {
        let below = g.bs(rel(q, DOWN));
        !rstate::suffocating(g.bs(q)) && !rstate::suffocating(g.bs(rel(q, UP))) && (vstate::sturdy(below, UP) || class(below) == Class::Bed)
    };
    for base in [foot, head] {
        for d in [cw(f), ccw(f), opp(f), f] {
            let q = rel(base, d);
            if class(g.bs(q)) == Class::Bed {
                continue;
            }
            if free(q) {
                return Some([q[0] as f64 + 0.5, q[1] as f64, q[2] as f64 + 0.5]);
            }
        }
    }
    let up = rel(head, UP);
    free(up).then(|| [up[0] as f64 + 0.5, up[1] as f64, up[2] as f64 + 0.5])
}

impl Game {
    /// a monster preventing rest within 8 x 5 x 8 of the bed
    pub fn monster_near_bed(&self, p: [i32; 3]) -> bool {
        let c = [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5];
        self.living_in(&crate::anticheat::combat::Aabb { min: [c[0] - 8.0, c[1] - 5.0, c[2] - 8.0], max: [c[0] + 8.0, c[1] + 5.0, c[2] + 8.0] })
            .into_iter()
            .any(|(eid, _)| self.mob_prevents_rest(eid))
    }

    /// entity data: pose and sleeping position
    pub fn broadcast_sleep_pose(&self, pl: &Arc<Player>, bed: Option<[i32; 3]>) {
        let mut b = Vec::new();
        b.vi(pl.eid);
        b.u8(6).vi(20).vi(if bed.is_some() { 2 } else { 0 });
        b.u8(14).vi(11);
        match bed {
            Some(h) => {
                b.bool(true).pos(h[0], h[1], h[2]);
            }
            None => {
                b.bool(false);
            }
        }
        b.u8(0xFF);
        let pkt = self.encode("set_entity_data", &b);
        let pos = pl.pose.lock().pos;
        self.broadcast_chunk(((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4), &pkt);
        pl.send(pkt);
    }

    /// `Player.stopSleepInBed`
    pub fn wake_up(&self, pl: &Arc<Player>) {
        let Some((head, _)) = pl.vit.lock().sleeping.take() else { return };
        let hs = self.bs(head);
        if class(hs) == Class::Bed {
            self.set_block(head, with_bool(hs, "occupied", false), flags::ALL);
        }
        self.broadcast_sleep_pose(pl, None);
        let mut b = Vec::new();
        b.vi(pl.eid).u8(2);
        let pkt = self.encode("animate", &b);
        let pos = pl.pose.lock().pos;
        self.broadcast_chunk(((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4), &pkt);
        pl.send(pkt);
        if let Some(to) = stand_up_pos(self, head) {
            pl.requests.lock().push(crate::game::Request::Teleport(to, pl.pose.lock().yaw, 0.0));
        }
    }

    /// `ServerLevel.tick` sleeping: everyone (but spectators) asleep for
    /// 100 ticks skips the night and clears the weather.
    pub fn tick_sleep(&self) {
        let players = self.players();
        let mut any = false;
        let mut all = true;
        for p in &players {
            if p.gamemode.load(Ordering::Relaxed) == 3 {
                continue;
            }
            let mut v = p.vit.lock();
            match &mut v.sleeping {
                Some((head, n)) => {
                    any = true;
                    *n = (*n + 1).min(110);
                    if *n < 100 {
                        all = false;
                    }
                    let _ = head;
                }
                None => all = false,
            }
        }
        // woken by day / a broken bed
        for p in &players {
            let bed = p.vit.lock().sleeping.map(|x| x.0);
            if let Some(h) = bed {
                if class(self.bs(h)) != Class::Bed || (self.is_day() && !self.weather.lock().thundering && !all) {
                    self.wake_up(p);
                }
            }
        }
        if any && all {
            let t = self.day_time();
            if t >= 12000 || self.weather.lock().thundering {
                self.set_day_time(0);
            }
            self.clear_weather_after_sleep();
            for p in &players {
                self.wake_up(p);
            }
        }
    }
}

// ------------------------------------------------------------ signs

/// `SignBlock.isFacingFrontText`
fn sign_front(_g: &Game, p: [i32; 3], s: BlockState, pl: &Player) -> bool {
    let pos = pl.pose.lock().pos;
    let rot = if let Some(r) = s.prop("rotation") {
        r.parse::<f32>().unwrap_or(0.0) * 22.5
    } else {
        // wall signs: the facing's yRot
        match facing(s) {
            SOUTH => 0.0,
            WEST => 90.0,
            NORTH => 180.0,
            _ => 270.0,
        }
    };
    let dx = pos[0] - (p[0] as f64 + 0.5);
    let dz = pos[2] - (p[2] as f64 + 0.5);
    let yaw = (dz.atan2(dx).to_degrees() as f32) - 90.0;
    let diff = ((rot - yaw + 540.0) % 360.0) - 180.0;
    diff.abs() <= 90.0
}

fn side_key(front: bool) -> &'static str {
    if front {
        "front_text"
    } else {
        "back_text"
    }
}

/// dyes, glow ink and honeycomb on a sign
fn sign_item_use(g: &Game, p: [i32; 3], s: BlockState, cx: &Ctx, item: &str) -> Option<Use> {
    let front = sign_front(g, p, s, cx.player);
    let mut t = g.world.block_entity(p)?;
    let key = side_key(front);
    let has_text = t.get(key).and_then(|x| x.get("messages")).is_some_and(|m| match m {
        Tag::List(l) => l.iter().any(|x| x.as_str().is_some_and(|s| !s.is_empty() && s != "\"\"") || matches!(x, Tag::Compound(_))),
        _ => false,
    });
    let set = |t: &mut Tag, k: &str, v: Tag| {
        if let Tag::Compound(f) = t {
            if let Some((_, side)) = f.iter_mut().find(|(n, _)| n == key) {
                if let Tag::Compound(sf) = side {
                    sf.retain(|(n, _)| n != k);
                    sf.push((k.to_string(), v));
                }
            }
        }
    };
    let done = match item {
        "honeycomb" => {
            if let Tag::Compound(f) = &mut t {
                f.retain(|(n, _)| n != "is_waxed");
                f.push(("is_waxed".into(), Tag::Byte(1)));
            }
            g.level_event(3003, p, 0);
            true
        }
        "glow_ink_sac" if has_text => {
            set(&mut t, "has_glowing_text", Tag::Byte(1));
            sound(g, p, "minecraft:item.glow_ink_sac.use", 1.0, 1.0);
            true
        }
        "ink_sac" if has_text => {
            set(&mut t, "has_glowing_text", Tag::Byte(0));
            sound(g, p, "minecraft:item.ink_sac.use", 1.0, 1.0);
            true
        }
        d if d.ends_with("_dye") && has_text => {
            set(&mut t, "color", Tag::String(d.trim_end_matches("_dye").to_string()));
            sound(g, p, "minecraft:item.dye.use", 1.0, 1.0);
            true
        }
        _ => false,
    };
    if !done {
        return None;
    }
    g.set_block_entity_visible(p, t);
    if !cx.creative {
        let slot = 36 + cx.player.held.load(Ordering::Relaxed) as usize;
        cx.player.inv.lock().consume(slot, 1);
        g.flush_inventory(cx.player);
    }
    Some(Use::Consumed)
}

fn empty_side() -> Tag {
    Tag::Compound(vec![
        ("has_glowing_text".into(), Tag::Byte(0)),
        ("color".into(), Tag::String("black".into())),
        ("messages".into(), Tag::List(vec![Tag::String(String::new()); 4])),
    ])
}

impl Game {
    /// store a block entity and send it to the players seeing it
    pub fn set_block_entity_visible(&self, p: [i32; 3], t: Tag) {
        self.world.set_block_entity(p, Some(t.clone()), true);
        let Some(id) = t.get("id").and_then(|v| v.as_str()) else { return };
        let Some(ty) = crate::registry::Registries::get().id("minecraft:block_entity_type", id) else { return };
        let shown = match &t {
            Tag::Compound(v) => Tag::Compound(v.iter().filter(|(k, _)| !matches!(k.as_str(), "Items" | "LootTable" | "LootTableSeed" | "x" | "y" | "z" | "id")).cloned().collect()),
            o => o.clone(),
        };
        let mut b = Vec::new();
        b.pos(p[0], p[1], p[2]).vi(ty);
        b.extend_from_slice(&shown.to_network_bytes());
        self.broadcast_chunk((p[0] >> 4, p[2] >> 4), &self.encode("block_entity_data", &b));
    }

    /// `ServerGamePacketListenerImpl.handleSignUpdate`
    pub fn sign_update(&self, pl: &Player, p: [i32; 3], front: bool, lines: [String; 4]) {
        let s = self.bs(p);
        if !s.name().ends_with("_sign") {
            return;
        }
        let pos = pl.pose.lock().pos;
        let c = [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5];
        if (0..3).map(|i| (pos[i] - c[i]).powi(2)).sum::<f64>() > 64.0 {
            return;
        }
        let mut t = self.world.block_entity(p).unwrap_or_else(|| crate::game::blockentity::new_tag(crate::game::blockentity::be_id(s.name()).unwrap_or("minecraft:sign"), p));
        if t.get("is_waxed").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
            return;
        }
        let key = side_key(front);
        let msgs: Vec<Tag> = lines
            .iter()
            .map(|l| {
                let clean: String = l.chars().filter(|c| !c.is_control() && *c != '\u{a7}').take(384).collect();
                // 26.1 stores text components as NBT: a string is literal text
                Tag::String(clean)
            })
            .collect();
        if let Tag::Compound(f) = &mut t {
            if !f.iter().any(|(n, _)| n == "front_text") {
                f.push(("front_text".into(), empty_side()));
            }
            if !f.iter().any(|(n, _)| n == "back_text") {
                f.push(("back_text".into(), empty_side()));
            }
            if let Some((_, Tag::Compound(sf))) = f.iter_mut().find(|(n, _)| n == key) {
                sf.retain(|(n, _)| n != "messages");
                sf.push(("messages".into(), Tag::List(msgs)));
            }
        }
        self.set_block_entity_visible(p, t);
    }
}

impl Game {
    /// `Monster.isPreventingPlayerRest`
    fn mob_prevents_rest(&self, eid: i32) -> bool {
        let ents = self.entities.lock();
        let Some(e) = ents.get(&eid) else { return false };
        match &e.kind {
            crate::game::entity::EKind::Mob(m) => m.ty.hostile() && m.ty.0 != "zombified_piglin" || m.ty.0 == "zombified_piglin" && m.angry(self.game_time() as i64),
            _ => false,
        }
    }

    /// `ServerLevel.resetWeatherCycle`
    fn clear_weather_after_sleep(&self) {
        let (raining, thundering) = {
            let w = self.weather.lock();
            (w.raining, w.thundering)
        };
        if raining || thundering {
            self.set_weather(0, 0, false, false);
        }
    }
}

/// Remove one of `item` from the hand holding it.
fn consume_held(g: &Game, p: &Arc<Player>, item: &str) {
    let id = Items::get().id(item);
    let main = 36 + p.held.load(Ordering::Relaxed) as usize;
    let mut inv = p.inv.lock();
    let slot = if inv.slots[main].is_some_and(|s| Some(s.item) == id) { main } else { 45 };
    inv.consume(slot, 1);
    drop(inv);
    g.flush_inventory(p);
}

/// `ItemStack.hurtAndBreak(1)` on the hand holding `item`.
fn damage_held(g: &Game, p: &Arc<Player>, item: &str) {
    let id = Items::get().id(item);
    let main = p.inv.lock().slots[36 + p.held.load(Ordering::Relaxed) as usize].is_some_and(|s| Some(s.item) == id);
    let slot = if main { crate::game::combat::living::MAINHAND } else { crate::game::combat::living::OFFHAND };
    g.with_player(p, |l, eq, _| {
        let mut evs = Vec::new();
        crate::game::combat::living::hurt_and_break(eq, slot, 1, false, &mut evs, &mut rand::thread_rng());
        l.events.extend(evs);
    });
    g.flush_inventory(p);
}
