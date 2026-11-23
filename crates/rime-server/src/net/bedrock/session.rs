//! One Bedrock player. The Bedrock game protocol (batches, compression,
//! encryption, login, spawn) runs on a RakNet connection; the player itself
//! is a regular Java session running in-process over a pipe, so every game
//! rule, anticheat check, command and plugin hook applies unchanged. This
//! module translates between the two:
//!
//! * Java → Bedrock: world chunks (blocks + biomes), block changes, chat and
//!   system messages, time, health, other players and mobs (spawn, movement,
//!   despawn), teleports, disconnects.
//! * Bedrock → Java: movement (PlayerAuthInput), chat, commands, hotbar
//!   selection, block breaking and placing, attacking entities, respawn.
//!
//! Not translated (yet): inventories and containers (the Bedrock inventory
//! screen stays empty), item entities, entity metadata/animations, sounds,
//! particles, skins (Bedrock players see a default skin), scoreboards,
//! titles, boss bars, other dimensions.

use std::collections::HashMap;
use std::io::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use tokio::io::{AsyncWriteExt, DuplexStream};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::codec::{packet, R, W};
use super::login::{self, Cipher, Identity};
use super::mapping::{chunk_payload, Tables};
use super::raknet::{self, Conn};
use crate::net::codec::{Out, Reader};
use crate::net::conn;
use crate::net::crypt::{CryptStream, Profile};
use crate::registry::{BlockRegistry, BlockState, PacketIds, Registries};
use crate::ServerState;

// Bedrock packet ids
const LOGIN: u32 = 0x01;
const PLAY_STATUS: u32 = 0x02;
const SERVER_TO_CLIENT_HANDSHAKE: u32 = 0x03;
const CLIENT_TO_SERVER_HANDSHAKE: u32 = 0x04;
const DISCONNECT: u32 = 0x05;
const RESOURCE_PACKS_INFO: u32 = 0x06;
const RESOURCE_PACK_STACK: u32 = 0x07;
const RESOURCE_PACK_CLIENT_RESPONSE: u32 = 0x08;
const TEXT: u32 = 0x09;
const SET_TIME: u32 = 0x0a;
const START_GAME: u32 = 0x0b;
const ADD_PLAYER: u32 = 0x0c;
const ADD_ENTITY: u32 = 0x0d;
const REMOVE_ENTITY: u32 = 0x0e;
const MOVE_ENTITY: u32 = 0x12;
const MOVE_PLAYER: u32 = 0x13;
const UPDATE_BLOCK: u32 = 0x15;
const INVENTORY_TRANSACTION: u32 = 0x1e;
const MOB_EQUIPMENT: u32 = 0x1f;
const PLAYER_ACTION: u32 = 0x24;
const SET_HEALTH: u32 = 0x2a;
const ANIMATE: u32 = 0x2c;
const RESPAWN: u32 = 0x2d;
const LEVEL_CHUNK: u32 = 0x3a;
const SET_PLAYER_GAME_TYPE: u32 = 0x3e;
const PLAYER_LIST: u32 = 0x3f;
const REQUEST_CHUNK_RADIUS: u32 = 0x45;
const CHUNK_RADIUS_UPDATE: u32 = 0x46;
const COMMAND_REQUEST: u32 = 0x4d;
const SET_LOCAL_PLAYER_AS_INITIALIZED: u32 = 0x71;
const AVAILABLE_ENTITY_IDENTIFIERS: u32 = 0x77;
const NETWORK_CHUNK_PUBLISHER_UPDATE: u32 = 0x79;
const BIOME_DEFINITION_LIST: u32 = 0x7a;
const NETWORK_SETTINGS: u32 = 0x8f;
const PLAYER_AUTH_INPUT: u32 = 0x90;
const CREATIVE_CONTENT: u32 = 0x91;
const ITEM_REGISTRY: u32 = 0xa2;
const UPDATE_ABILITIES: u32 = 0xbb;
const REQUEST_NETWORK_SETTINGS: u32 = 0xc1;

const COMPRESS_THRESHOLD: usize = 256;
const EYE: f64 = 1.62;
const TIMEOUT: Duration = Duration::from_secs(15);

/// Entities shown to Bedrock players (Java name = Bedrock identifier).
const SHOWN_ENTITIES: &[&str] = &["zombie", "skeleton", "creeper", "spider", "cave_spider", "silverfish", "cow", "pig", "sheep", "chicken", "arrow"];

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
enum Phase {
    Connecting,
    Settings,
    Login,
    Handshake,
    Packs,
    Starting,
    Playing,
}

fn jid(state: &str, dir: &str, name: &str) -> i32 {
    PacketIds::get().id(state, dir, name)
}

fn java_packet(name: &str, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(body.len() + 2);
    p.vi(jid("play", "serverbound", name));
    p.extend_from_slice(body);
    p
}

fn angle(deg: f32) -> u8 {
    (deg.rem_euclid(360.0) / 360.0 * 256.0) as u8
}

fn gamemode_to_bedrock(gm: i32) -> i32 {
    match gm {
        3 => 6,
        g => g.clamp(0, 2),
    }
}

/// Java-safe player name for a Bedrock gamertag.
pub fn java_name(prefix: &str, gamertag: &str, replace_spaces: bool) -> String {
    let mut n = String::from(prefix);
    for c in gamertag.chars() {
        if c == ' ' && replace_spaces {
            n.push('_');
        } else if c.is_ascii_alphanumeric() || c == '_' {
            n.push(c);
        }
    }
    n.chars().take(16).collect()
}

/// Floodgate-style UUID for an Xbox account; offline UUID otherwise.
pub fn java_uuid(id: &Identity, name: &str) -> uuid::Uuid {
    match id.xuid.parse::<u64>() {
        Ok(x) if id.authenticated => uuid::Uuid::from_u128(x as u128),
        _ => uuid::Uuid::new_v3(&uuid::Uuid::NAMESPACE_OID, format!("OfflinePlayer:{name}").as_bytes()),
    }
}

struct Tracked {
    player: bool,
    uuid: uuid::Uuid,
}

pub struct Session {
    st: Arc<ServerState>,
    sock: Arc<UdpSocket>,
    rak: Conn,
    phase: Phase,
    compression: bool,
    cipher: Option<Cipher>,
    out: Vec<Vec<u8>>,
    identity: Option<Identity>,
    java: Option<mpsc::Sender<Vec<u8>>>,
    java_started: bool,
    closed: bool,
    // Java-side state
    eid: i32,
    gamemode: i32,
    pos: [f64; 3],
    yaw: f32,
    pitch: f32,
    have_pos: bool,
    radius: i32,
    pending_chunks: Vec<(i32, i32)>,
    chunks_sent: usize,
    spawned: bool,
    start_sent: Option<Instant>,
    publisher_at: (i32, i32),
    tracked: HashMap<i32, Tracked>,
    dead: bool,
    last_move: Instant,
    breaking: Option<[i32; 3]>,
    seq: i32,
    /// blocks touched per action sequence: resent when the server acknowledges
    touched: Vec<(i32, [i32; 3])>,
}

impl Session {
    fn new(st: Arc<ServerState>, sock: Arc<UdpSocket>, addr: SocketAddr, mtu: usize) -> Session {
        let radius = st.cfg.server.view_distance as i32;
        Session {
            st,
            sock,
            rak: Conn::new(addr, mtu),
            phase: Phase::Connecting,
            compression: false,
            cipher: None,
            out: Vec::new(),
            identity: None,
            java: None,
            java_started: false,
            closed: false,
            eid: 0,
            gamemode: 0,
            pos: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            have_pos: false,
            radius,
            pending_chunks: Vec::new(),
            chunks_sent: 0,
            spawned: false,
            start_sent: None,
            publisher_at: (i32::MAX, i32::MAX),
            tracked: HashMap::new(),
            dead: false,
            last_move: Instant::now(),
            breaking: None,
            seq: 0,
            touched: Vec::new(),
        }
    }

    // ------------------------------------------------------------ output

    fn send(&mut self, id: u32, body: &W) {
        self.out.push(packet(id, body));
    }

    /// Packs queued game packets into one batch and hands it to RakNet.
    fn flush_batch(&mut self) {
        if self.out.is_empty() {
            return;
        }
        let mut raw = W::new();
        for p in self.out.drain(..) {
            raw.bytes(&p);
        }
        let mut data = Vec::with_capacity(raw.0.len() / 2 + 8);
        if self.compression {
            if raw.0.len() > COMPRESS_THRESHOLD {
                data.push(0);
                let mut z = flate2::write::DeflateEncoder::new(data, flate2::Compression::fast());
                let _ = z.write_all(&raw.0);
                data = z.finish().unwrap_or_default();
            } else {
                data.push(0xff);
                data.extend_from_slice(&raw.0);
            }
        } else {
            data = raw.0;
        }
        if let Some(c) = &mut self.cipher {
            data = c.encrypt(data);
        }
        let mut msg = Vec::with_capacity(data.len() + 1);
        msg.push(raknet::ID_GAME);
        msg.extend_from_slice(&data);
        self.rak.send(&msg);
    }

    async fn flush(&mut self) {
        self.flush_batch();
        for d in self.rak.flush(Instant::now()) {
            let _ = self.sock.send_to(&d, self.rak.addr).await;
        }
    }

    fn disconnect(&mut self, msg: &str) {
        let mut w = W::new();
        w.zz32(0).bool(false).str(msg).str("");
        self.send(DISCONNECT, &w);
        self.closed = true;
    }

    fn play_status(&mut self, status: i32) {
        let mut w = W::new();
        w.i32be(status);
        self.send(PLAY_STATUS, &w);
    }

    fn to_java(&self, p: Vec<u8>) {
        if let Some(j) = &self.java {
            let _ = j.try_send(p);
        }
    }

    // ------------------------------------------------------------- input

    fn message(&mut self, m: Vec<u8>, java_in: &mpsc::Sender<Vec<u8>>) -> Result<()> {
        let Some(&id) = m.first() else { return Ok(()) };
        match id {
            raknet::ID_CONNECTION_REQUEST => {
                let mut r = R::new(&m[1..]);
                let _guid = r.take(8)?;
                let time = r.take(8)?.to_vec();
                let mut o = vec![raknet::ID_CONNECTION_REQUEST_ACCEPTED];
                raknet::write_address(&mut o, self.rak.addr);
                o.extend_from_slice(&0u16.to_be_bytes());
                for _ in 0..20 {
                    raknet::write_address(&mut o, "255.255.255.255:19132".parse().unwrap());
                }
                o.extend_from_slice(&time);
                o.extend_from_slice(&(now_ms() as i64).to_be_bytes());
                self.rak.send(&o);
            }
            raknet::ID_NEW_INCOMING_CONNECTION => {
                if self.phase == Phase::Connecting {
                    self.phase = Phase::Settings;
                }
            }
            raknet::ID_CONNECTED_PING => {
                let mut o = vec![raknet::ID_CONNECTED_PONG];
                o.extend_from_slice(m.get(1..9).unwrap_or(&[0; 8]));
                o.extend_from_slice(&(now_ms() as i64).to_be_bytes());
                self.rak.send(&o);
            }
            raknet::ID_DISCONNECT => self.closed = true,
            raknet::ID_GAME => {
                let mut data = m[1..].to_vec();
                if let Some(c) = &mut self.cipher {
                    c.decrypt(&mut data)?;
                }
                let raw = if self.compression {
                    match data.first() {
                        Some(0) => {
                            let mut out = Vec::new();
                            let mut z = flate2::read::DeflateDecoder::new(&data[1..]);
                            std::io::Read::read_to_end(&mut std::io::Read::take(&mut z, 16 << 20), &mut out)?;
                            out
                        }
                        Some(0xff) => data[1..].to_vec(),
                        _ => bail!("unsupported compression"),
                    }
                } else {
                    data
                };
                let mut r = R::new(&raw);
                let mut n = 0;
                while r.left() > 0 {
                    n += 1;
                    if n > 512 {
                        bail!("too many packets in one batch");
                    }
                    let p = r.bytes(8 << 20)?;
                    self.game_packet(p, java_in)?;
                    if self.closed {
                        break;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn game_packet(&mut self, p: &[u8], java_in: &mpsc::Sender<Vec<u8>>) -> Result<()> {
        let mut r = R::new(p);
        let id = r.vu32()? & 0x3ff;
        let cfg = &self.st.cfg.bedrock;
        match (self.phase, id) {
            (Phase::Settings | Phase::Connecting, REQUEST_NETWORK_SETTINGS) => {
                let proto = r.i32be()?;
                if proto != cfg.protocol_version as i32 {
                    self.play_status(if proto < cfg.protocol_version as i32 { 1 } else { 2 });
                    self.closed = true;
                    return Ok(());
                }
                let mut w = W::new();
                w.u16le(COMPRESS_THRESHOLD as u16).u16le(0).bool(false).u8(0).f32(0.0);
                self.send(NETWORK_SETTINGS, &w);
                self.flush_batch();
                self.compression = true;
                self.phase = Phase::Login;
            }
            (Phase::Login, LOGIN) => {
                let allow_offline = matches!(cfg.auth_type, crate::config::BedrockAuth::Offline);
                // OIDC key fetches block: keep them off the async workers
                let res = tokio::task::block_in_place(|| login::verify_login(&p[r.pos..], allow_offline, &cfg.game_version));
                let id = match res {
                    Ok((proto, id)) if proto == cfg.protocol_version as i32 => id,
                    Ok((proto, _)) => {
                        self.play_status(if proto < cfg.protocol_version as i32 { 1 } else { 2 });
                        self.closed = true;
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::info!("Bedrock login from {} refused: {e:#}", self.rak.addr);
                        self.disconnect(&format!("Login failed: {e}"));
                        return Ok(());
                    }
                };
                let (token, cipher) = login::server_handshake(&id.client_key)?;
                let mut w = W::new();
                w.str(&token);
                self.send(SERVER_TO_CLIENT_HANDSHAKE, &w);
                self.flush_batch();
                self.cipher = Some(cipher);
                self.identity = Some(id);
                self.phase = Phase::Handshake;
            }
            (Phase::Handshake, CLIENT_TO_SERVER_HANDSHAKE) => {
                self.play_status(0);
                let mut w = W::new();
                w.bool(false).bool(false).bool(false).bool(false).raw(&[0; 16]).str("").vu32(0);
                self.send(RESOURCE_PACKS_INFO, &w);
                self.phase = Phase::Packs;
            }
            (Phase::Packs, RESOURCE_PACK_CLIENT_RESPONSE) => {
                let status = r.vu32()?;
                match status {
                    1 | 2 => {
                        let mut w = W::new();
                        w.bool(false).vu32(0).str("*").i32le(0).bool(false).bool(false);
                        self.send(RESOURCE_PACK_STACK, &w);
                    }
                    3 => self.start_java(java_in.clone()),
                    _ => self.disconnect("Resource packs refused"),
                }
            }
            (Phase::Starting | Phase::Playing, REQUEST_CHUNK_RADIUS) => {
                let want = r.zz32()?;
                self.radius = want.clamp(2, self.st.cfg.server.view_distance as i32);
                let mut w = W::new();
                w.zz32(self.radius);
                self.send(CHUNK_RADIUS_UPDATE, &w);
            }
            (Phase::Starting | Phase::Playing, SET_LOCAL_PLAYER_AS_INITIALIZED) => {}
            (Phase::Playing, PLAYER_AUTH_INPUT) => self.auth_input(&mut r)?,
            (Phase::Playing, TEXT) => {
                let _needs_translation = r.bool()?;
                let _category = r.u8()?;
                let ty = r.u8()?;
                let msg = match ty {
                    1 | 7 | 8 => {
                        let _source = r.str(64)?;
                        r.str(512)?
                    }
                    0 | 5 | 6 | 9 | 10 | 11 => r.str(512)?,
                    _ => return Ok(()),
                };
                let msg: String = msg.chars().filter(|c| !c.is_control() && *c != '§').take(256).collect();
                if !msg.trim().is_empty() {
                    self.java_chat(&msg);
                }
            }
            (Phase::Playing, COMMAND_REQUEST) => {
                let cmd = r.str(512)?;
                let cmd: String = cmd.trim_start_matches('/').chars().filter(|c| !c.is_control() && *c != '§').take(256).collect();
                let mut b = Vec::new();
                b.str(&cmd);
                self.to_java(java_packet("chat_command", &b));
            }
            (Phase::Playing, MOB_EQUIPMENT) => {
                let _rid = r.var()?;
                skip_item(&mut r)?;
                let _slot = r.u8()?;
                let selected = r.u8()?;
                if selected < 9 {
                    let mut b = Vec::new();
                    b.i16(selected as i16);
                    self.to_java(java_packet("set_carried_item", &b));
                }
            }
            (Phase::Playing, ANIMATE) => {
                if r.u8()? == 1 {
                    let mut b = Vec::new();
                    b.vi(0);
                    self.to_java(java_packet("swing", &b));
                }
            }
            (Phase::Playing, PLAYER_ACTION) => {
                let _rid = r.var()?;
                let action = r.zz32()?;
                let pos = r.block_coords()?;
                let _result = r.block_coords()?;
                let face = r.zz32()?;
                self.block_action(action, pos, face);
            }
            (Phase::Playing, INVENTORY_TRANSACTION) => self.transaction(&mut r)?,
            (Phase::Playing, RESPAWN) => {
                let _pos = r.vec3()?;
                let state = r.u8()?;
                if state == 2 && self.dead {
                    let mut b = Vec::new();
                    b.vi(0); // perform respawn
                    self.to_java(java_packet("client_command", &b));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn java_chat(&self, msg: &str) {
        if let Some(cmd) = msg.strip_prefix('/') {
            let mut b = Vec::new();
            b.str(cmd);
            self.to_java(java_packet("chat_command", &b));
            return;
        }
        let mut b = Vec::new();
        b.str(msg).i64(now_ms() as i64).i64(0).bool(false).vi(0).raw(&[0, 0, 0]).u8(0);
        self.to_java(java_packet("chat", &b));
    }

    fn touch(&mut self, seq: i32, pos: [i32; 3]) {
        if self.touched.len() < 256 {
            self.touched.push((seq, pos));
        }
    }

    fn next_seq(&mut self) -> i32 {
        self.seq += 1;
        self.seq
    }

    fn dig(&mut self, status: i32, pos: [i32; 3], face: i32) {
        let seq = self.next_seq();
        self.touch(seq, pos);
        let mut b = Vec::new();
        b.vi(status).pos(pos[0], pos[1], pos[2]).u8(face.clamp(0, 5) as u8).vi(seq);
        self.to_java(java_packet("player_action", &b));
    }

    fn block_action(&mut self, action: i32, pos: [i32; 3], face: i32) {
        match action {
            // start_break
            0 => {
                self.breaking = Some(pos);
                self.dig(0, pos, face);
            }
            // abort_break
            1 => {
                if let Some(p) = self.breaking.take() {
                    self.dig(1, p, face);
                }
            }
            // stop_break / predict_break
            2 | 26 => {
                if let Some(p) = self.breaking.take() {
                    if self.gamemode != 1 {
                        self.dig(2, p, face);
                    }
                }
            }
            // creative destroy
            13 => {
                if self.breaking.take() != Some(pos) {
                    self.dig(0, pos, face);
                }
            }
            _ => {}
        }
    }

    fn auth_input(&mut self, r: &mut R) -> Result<()> {
        let pitch = r.f32()?;
        let yaw = r.f32()?;
        let p = r.vec3()?;
        if !(p.iter().all(|v| v.is_finite()) && pitch.is_finite() && yaw.is_finite()) {
            return Ok(());
        }
        let feet = [p[0] as f64, p[1] as f64 - EYE, p[2] as f64];
        let moved = (0..3).any(|i| (feet[i] - self.pos[i]).abs() > 1e-4) || (yaw - self.yaw).abs() > 0.01 || (pitch - self.pitch).abs() > 0.01;
        if !moved && self.last_move.elapsed() < Duration::from_millis(1000) {
            self.block_actions(r);
            return Ok(());
        }
        self.last_move = Instant::now();
        self.pos = feet;
        self.yaw = yaw;
        self.pitch = pitch;
        let on_ground = self.on_ground(feet);
        let mut b = Vec::new();
        b.f64(feet[0]).f64(feet[1]).f64(feet[2]).f32(yaw).f32(pitch).u8(on_ground as u8);
        self.to_java(java_packet("move_player_pos_rot", &b));
        self.update_publisher();
        self.block_actions(r);
        Ok(())
    }

    /// The rest of PlayerAuthInput: block actions (best effort; the
    /// optional inventory parts are skipped over).
    fn block_actions(&mut self, r: &mut R) {
        let parse = |r: &mut R| -> Result<Vec<(i32, [i32; 3], i32)>> {
            let _move = (r.f32()?, r.f32()?);
            let _head = r.f32()?;
            let n = r.vu32()?;
            if n > 256 {
                bail!("too many input flags");
            }
            for _ in 0..n {
                r.zz32()?;
            }
            let _mode = r.vu32()?;
            let _play = r.vu32()?;
            let _model = r.zz32()?;
            let _rot = (r.f32()?, r.f32()?);
            let _tick = r.var()?;
            let _delta = r.vec3()?;
            if r.bool()? {
                bail!("item interaction (not parsed)");
            }
            if r.bool()? {
                bail!("item stack request (not parsed)");
            }
            let mut out = Vec::new();
            if r.bool()? {
                let n = r.vu32()?;
                if n > 64 {
                    bail!("too many block actions");
                }
                for _ in 0..n {
                    let a = r.zz32()?;
                    let pos = r.block_coords()?;
                    let face = r.zz32()?;
                    out.push((a, pos, face));
                }
            }
            Ok(out)
        };
        if let Ok(actions) = parse(r) {
            for (a, pos, face) in actions {
                self.block_action(a, pos, face);
            }
        }
    }

    fn on_ground(&self, feet: [f64; 3]) -> bool {
        let reg = BlockRegistry::get();
        let y = (feet[1] - 0.05).floor() as i32;
        for dx in [-0.3, 0.3] {
            for dz in [-0.3, 0.3] {
                let x = (feet[0] + dx).floor() as i32;
                let z = (feet[2] + dz).floor() as i32;
                if let Some(b) = self.st.game.world.get_block(x, y, z) {
                    if reg.is_solid(b) && feet[1] - (y as f64 + 1.0) < 0.06 {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn transaction(&mut self, r: &mut R) -> Result<()> {
        let legacy = r.zz32()?;
        if r.bool()? {
            let n = r.vu32()?;
            for _ in 0..n.min(64) {
                r.u8()?;
                let k = r.vu32()?;
                r.take(k.min(256) as usize)?;
            }
        }
        let _ = legacy;
        let ty = r.vu32()?;
        let n = r.vu32()?;
        if n > 64 {
            bail!("too many inventory actions");
        }
        for _ in 0..n {
            let _source = r.vu32()?;
            if r.bool()? {
                r.u8()?;
            }
            if r.bool()? {
                r.vu32()?;
            }
            let _slot = r.vu32()?;
            skip_item(r)?;
            skip_item(r)?;
        }
        match ty {
            // item use
            2 => {
                let action = r.zz32()?;
                let _trigger = r.u8()?;
                let pos = r.block_coords()?;
                let face = r.u8()? as i32;
                let slot = r.zz32()?;
                let _hand = r.u8()?;
                skip_item(r)?;
                let _player = r.vec3()?;
                let click = r.vec3()?;
                match action {
                    // click block: place / use
                    0 => {
                        if (0..9).contains(&slot) {
                            let mut b = Vec::new();
                            b.i16(slot as i16);
                            self.to_java(java_packet("set_carried_item", &b));
                        }
                        let seq = self.next_seq();
                        self.touch(seq, pos);
                        let d = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]][face.clamp(0, 5) as usize];
                        self.touch(seq, [pos[0] + d[0], pos[1] + d[1], pos[2] + d[2]]);
                        let cursor = [click[0].clamp(0.0, 1.0), click[1].clamp(0.0, 1.0), click[2].clamp(0.0, 1.0)];
                        let mut b = Vec::new();
                        b.vi(0).pos(pos[0], pos[1], pos[2]).vi(face.clamp(0, 5)).f32(cursor[0]).f32(cursor[1]).f32(cursor[2]).bool(false).bool(false).vi(seq);
                        self.to_java(java_packet("use_item_on", &b));
                    }
                    // click air: use item
                    1 => {
                        let seq = self.next_seq();
                        let mut b = Vec::new();
                        b.vi(0).vi(seq).f32(self.yaw).f32(self.pitch);
                        self.to_java(java_packet("use_item", &b));
                    }
                    // break block (non-authoritative breaking)
                    2 => {
                        if self.gamemode == 1 {
                            self.dig(0, pos, face);
                        } else {
                            self.dig(2, pos, face);
                        }
                        self.breaking = None;
                    }
                    _ => {}
                }
            }
            // item use on entity
            3 => {
                let target = r.var()? as i32;
                let action = r.zz32()?;
                let mut b = Vec::new();
                if action == 1 {
                    b.vi(target).vi(1).bool(false);
                } else {
                    b.vi(target).vi(0).vi(0).bool(false);
                }
                self.to_java(java_packet("interact", &b));
            }
            _ => {}
        }
        Ok(())
    }

    // ----------------------------------------------------- Java session

    fn start_java(&mut self, java_in: mpsc::Sender<Vec<u8>>) {
        let Some(id) = self.identity.clone() else { return };
        let cfg = &self.st.cfg.bedrock;
        let name = java_name(&cfg.username_prefix, &id.name, cfg.replace_spaces);
        let uuid = java_uuid(&id, &name);
        if self.st.all_players().iter().any(|p| p.name.eq_ignore_ascii_case(&name) || p.uuid == uuid) {
            self.disconnect("You are already logged in.");
            return;
        }
        tracing::info!("Bedrock player {} ({}, {}) joins as {name}", id.name, if id.authenticated { "Xbox Live" } else { "offline" }, self.rak.addr);
        let (client, server) = tokio::io::duplex(1 << 20);
        let profile = Profile { uuid, name, properties: Vec::new() };
        let st = self.st.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::net::play::run(CryptStream::new(server), st, profile).await {
                tracing::debug!("Bedrock bridge session ended: {e:#}");
            }
        });
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1024);
        let view = self.radius;
        tokio::spawn(java_client(client, java_in, rx, view));
        self.java = Some(tx);
        self.java_started = true;
        self.phase = Phase::Starting;
    }

    fn java_packet(&mut self, p: &[u8]) -> Result<()> {
        let mut r = Reader::new(p);
        let pid = r.varint()?;
        let cb = |name: &str| jid("play", "clientbound", name);
        if pid == cb("login") {
            self.eid = r.i32()?;
            let _hardcore = r.bool()?;
            let n = r.varint()?;
            for _ in 0..n.clamp(0, 64) {
                r.string(256)?;
            }
            let _max = r.varint()?;
            let _view = r.varint()?;
            let _sim = r.varint()?;
            r.bool()?;
            r.bool()?;
            r.bool()?;
            let _dim = r.varint()?;
            r.string(256)?;
            let _seed = r.i64()?;
            self.gamemode = r.u8()? as i32;
        } else if pid == cb("player_position") {
            let tid = r.varint()?;
            let pos = [r.f64()?, r.f64()?, r.f64()?];
            let _vel = [r.f64()?, r.f64()?, r.f64()?];
            let yaw = r.f32()?;
            let pitch = r.f32()?;
            let mut b = Vec::new();
            b.vi(tid);
            self.to_java(java_packet("accept_teleportation", &b));
            let mut b = Vec::new();
            b.f64(pos[0]).f64(pos[1]).f64(pos[2]).f32(yaw).f32(pitch).u8(0);
            self.to_java(java_packet("move_player_pos_rot", &b));
            self.pos = pos;
            self.yaw = yaw;
            self.pitch = pitch;
            self.have_pos = true;
            if self.phase == Phase::Starting && self.start_sent.is_none() {
                self.start_game();
            } else {
                self.move_self(1);
            }
            self.update_publisher();
        } else if pid == cb("level_chunk_with_light") {
            let cx = r.i32()?;
            let cz = r.i32()?;
            if self.start_sent.is_some() {
                self.send_chunk(cx, cz);
            } else {
                self.pending_chunks.push((cx, cz));
            }
        } else if pid == cb("chunk_batch_finished") {
            let mut b = Vec::new();
            b.f32(4.0);
            self.to_java(java_packet("chunk_batch_received", &b));
        } else if pid == cb("block_changed_ack") {
            let acked = r.varint()?;
            let done: Vec<[i32; 3]> = self.touched.iter().filter(|(s, _)| *s <= acked).map(|(_, p)| *p).collect();
            self.touched.retain(|(s, _)| *s > acked);
            for p in done {
                if let Some(b) = self.st.game.world.get_block(p[0], p[1], p[2]) {
                    self.update_block(p, b);
                }
            }
        } else if pid == cb("block_update") {
            let v = r.i64()?;
            let state = r.varint()?;
            let pos = [(v >> 38) as i32, (v << 52 >> 52) as i32, (v << 26 >> 38) as i32];
            self.update_block(pos, BlockState(state as u16));
        } else if pid == cb("section_blocks_update") {
            let s = r.i64()?;
            let (sx, sy, sz) = ((s >> 42) as i32, (s << 44 >> 44) as i32, (s << 22 >> 42) as i32);
            let n = r.varint()?;
            for _ in 0..n.clamp(0, 4096) {
                let v = varlong(&mut r)?;
                let state = (v >> 12) as u16;
                let (lx, lz, ly) = (((v >> 8) & 15) as i32, ((v >> 4) & 15) as i32, (v & 15) as i32);
                self.update_block([sx * 16 + lx, sy * 16 + ly, sz * 16 + lz], BlockState(state));
            }
        } else if pid == cb("system_chat") {
            let (tag, used) = crate::nbt::Tag::from_network_bytes(&p[r.pos..])?;
            r.pos += used;
            let overlay = r.bool().unwrap_or(false);
            let text = tag.plain_text();
            self.text(if overlay { 5 } else { 0 }, &text);
        } else if pid == cb("disconnect") {
            let text = crate::nbt::Tag::from_network_bytes(&p[r.pos..]).map(|(t, _)| t.plain_text()).unwrap_or_default();
            self.disconnect(&text);
        } else if pid == cb("keep_alive") {
            let k = r.i64()?;
            let mut b = Vec::new();
            b.i64(k);
            self.to_java(java_packet("keep_alive", &b));
        } else if pid == cb("ping") {
            let k = r.i32()?;
            let mut b = Vec::new();
            b.i32(k);
            self.to_java(java_packet("pong", &b));
        } else if pid == cb("set_time") {
            let _age = r.i64()?;
            let n = r.varint()?;
            if n >= 1 {
                let _clock = r.varint()?;
                let day = varlong(&mut r)?;
                let mut w = W::new();
                w.zz32(day as i32);
                self.send(SET_TIME, &w);
            }
        } else if pid == cb("set_health") {
            let health = r.f32()?;
            let mut w = W::new();
            w.zz32(health.ceil() as i32);
            self.send(SET_HEALTH, &w);
            if health <= 0.0 && !self.dead {
                self.dead = true;
                let mut w = W::new();
                w.vec3([self.pos[0] as f32, (self.pos[1] + EYE) as f32, self.pos[2] as f32]).u8(0).var(self.eid as u64);
                self.send(RESPAWN, &w);
            } else if health > 0.0 && self.dead {
                self.dead = false;
                let mut w = W::new();
                w.vec3([self.pos[0] as f32, (self.pos[1] + EYE) as f32, self.pos[2] as f32]).u8(1).var(self.eid as u64);
                self.send(RESPAWN, &w);
            }
        } else if pid == cb("game_event") {
            let ev = r.u8()?;
            let v = r.f32()?;
            if ev == 3 {
                self.gamemode = v as i32;
                let mut w = W::new();
                w.zz32(gamemode_to_bedrock(self.gamemode));
                self.send(SET_PLAYER_GAME_TYPE, &w);
                self.abilities();
            }
        } else if pid == cb("add_entity") {
            self.add_entity(&mut r)?;
        } else if pid == cb("entity_position_sync") {
            let eid = r.varint()?;
            let pos = [r.f64()?, r.f64()?, r.f64()?];
            let _vel = [r.f64()?, r.f64()?, r.f64()?];
            let yaw = r.f32()?;
            let pitch = r.f32()?;
            let on_ground = r.bool()?;
            self.move_entity(eid, pos, yaw, pitch, on_ground);
        } else if pid == cb("remove_entities") {
            let n = r.varint()?;
            for _ in 0..n.clamp(0, 4096) {
                let eid = r.varint()?;
                if let Some(t) = self.tracked.remove(&eid) {
                    let mut w = W::new();
                    w.zz64(eid as i64);
                    self.send(REMOVE_ENTITY, &w);
                    if t.player {
                        self.player_list_remove(t.uuid);
                    }
                }
            }
        } else if pid == cb("start_configuration") {
            // a RimeFleet node switch is not supported for Bedrock players
            self.disconnect("Server switch is not supported for Bedrock players yet.");
        }
        Ok(())
    }

    fn text(&mut self, ty: u8, msg: &str) {
        let mut w = W::new();
        w.bool(false).u8(0).u8(ty).str(msg).str("").str("").bool(false);
        self.send(TEXT, &w);
    }

    fn update_block(&mut self, pos: [i32; 3], state: BlockState) {
        if self.start_sent.is_none() {
            return;
        }
        let mut w = W::new();
        w.block_coords(pos).vu32(Tables::get().block(state)).vu32(3).vu32(0);
        self.send(UPDATE_BLOCK, &w);
    }

    fn send_chunk(&mut self, cx: i32, cz: i32) {
        let Some(c) = self.st.game.world.chunk_copy(cx, cz) else { return };
        let (count, payload) = chunk_payload(&c);
        let mut w = W::new();
        w.zz32(cx).zz32(cz).zz32(0).vu32(count).bool(false).bool(false).vu32(0).bytes(&payload).bool(false);
        self.send(LEVEL_CHUNK, &w);
        self.chunks_sent += 1;
    }

    fn update_publisher(&mut self) {
        if self.start_sent.is_none() {
            return;
        }
        let c = ((self.pos[0].floor() as i32) >> 4, (self.pos[2].floor() as i32) >> 4);
        if c == self.publisher_at {
            return;
        }
        self.publisher_at = c;
        let mut w = W::new();
        w.block_coords([self.pos[0].floor() as i32, self.pos[1].floor() as i32, self.pos[2].floor() as i32]).vu32((self.radius * 16) as u32).u32le(0);
        self.send(NETWORK_CHUNK_PUBLISHER_UPDATE, &w);
    }

    /// Mode 0 = normal, 1 = reset (server correction).
    fn move_self(&mut self, mode: u8) {
        if self.start_sent.is_none() {
            return;
        }
        let mut w = W::new();
        w.vu32(self.eid as u32).vec3([self.pos[0] as f32, (self.pos[1] + EYE) as f32, self.pos[2] as f32]).f32(self.pitch).f32(self.yaw).f32(self.yaw).u8(mode).bool(false).vu32(0).bool(false).var(0);
        self.send(MOVE_PLAYER, &w);
    }

    fn abilities(&mut self) {
        let creative = self.gamemode == 1;
        let spectator = self.gamemode == 3;
        let mut enabled: u32 = 0b11_1111; // build, mine, doors, containers, attack players/mobs
        if creative || spectator {
            enabled |= 1 << 10; // may_fly
        }
        if creative {
            enabled |= 1 << 11; // instant_build
        }
        if spectator {
            enabled &= !0b11_0011;
            enabled |= 1 << 9 | 1 << 17; // flying, no_clip
        }
        let op = self.st.game.players().iter().find(|p| p.eid == self.eid).is_some_and(|p| self.st.ops.is_op(&crate::ops::CommandSender::Player { name: p.name.clone() }));
        let mut w = W::new();
        w.i64le(self.eid as i64).u8(if op { 2 } else { 1 }).u8(if op { 1 } else { 0 }).u8(1);
        w.u16le(1).u32le((1 << 19) - 1 | 1 << 19).u32le(enabled).f32(0.05).f32(1.0).f32(0.1);
        self.send(UPDATE_ABILITIES, &w);
    }

    fn start_game(&mut self) {
        let st = self.st.clone();
        let gm = gamemode_to_bedrock(self.gamemode);
        let spawn = st.spawn();
        let mut w = W::new();
        w.zz64(self.eid as i64).var(self.eid as u64).zz32(gm);
        w.vec3([self.pos[0] as f32, (self.pos[1] + EYE) as f32, self.pos[2] as f32]).f32(self.pitch).f32(self.yaw);
        w.u64le(st.seed as u64).i16le(0).str("plains").zz32(0).zz32(1).zz32(gm).bool(false).zz32(2);
        w.block_coords([spawn[0].floor() as i32, spawn[1].floor() as i32, spawn[2].floor() as i32]);
        w.bool(true).zz32(0).bool(false).bool(false).u8(0).zz32(-1).vu32(0).bool(false).str("").f32(0.0).f32(0.0);
        w.bool(false).bool(true).bool(false).vu32(4).vu32(4).bool(true).bool(false);
        w.vu32(0); // game rules
        w.i32le(0).bool(false); // experiments
        w.bool(false).bool(false).u8(1).i32le(4);
        w.bool(false).bool(false).bool(false).bool(false).bool(false).bool(false).bool(false).bool(false).bool(false).bool(false);
        w.str("*").i32le(0).i32le(0).bool(true).str("").str("").bool(false).u8(0).bool(false).zz32(0).bool(false);
        w.str("").str(&st.cfg.bedrock.motd1).str("").bool(false).zz32(0).bool(false);
        w.i64le(st.game.clock() as i64).zz32(0);
        w.vu32(0).str("").bool(false).str("RimeMC").nbt_empty().u64le(0).raw(&[0; 16]).bool(false).bool(true).bool(false).bool(false);
        w.str("").str("").str("").str("");
        self.send(START_GAME, &w);

        let items = &Tables::get().items;
        let mut w = W::new();
        w.vu32(items.len() as u32);
        for (name, id, component, version) in items {
            if name.contains(':') {
                w.str(name);
            } else {
                w.str(&format!("minecraft:{name}"));
            }
            w.i16le(*id).bool(*component).zz32(*version).nbt_empty();
        }
        self.send(ITEM_REGISTRY, &w);
        let mut w = W::new();
        w.vu32(0).vu32(0);
        self.send(BIOME_DEFINITION_LIST, &w);
        let mut w = W::new();
        w.raw(&entity_identifiers());
        self.send(AVAILABLE_ENTITY_IDENTIFIERS, &w);
        let mut w = W::new();
        w.vu32(0).vu32(0);
        self.send(CREATIVE_CONTENT, &w);
        let mut w = W::new();
        w.zz32(self.radius);
        self.send(CHUNK_RADIUS_UPDATE, &w);
        self.abilities();
        let mut w = W::new();
        w.zz32(st.game.clock() as i32);
        self.send(SET_TIME, &w);
        self.start_sent = Some(Instant::now());
        self.update_publisher();
        for (cx, cz) in std::mem::take(&mut self.pending_chunks) {
            self.send_chunk(cx, cz);
        }
    }

    /// PlayStatus(player_spawn) once the chunks around the player arrived.
    fn maybe_spawn(&mut self) {
        let Some(t) = self.start_sent else { return };
        if self.spawned {
            return;
        }
        let want = ((2 * self.radius.min(4) + 1) * (2 * self.radius.min(4) + 1)) as usize;
        if self.chunks_sent >= want || t.elapsed() > Duration::from_secs(5) {
            self.spawned = true;
            self.phase = Phase::Playing;
            self.play_status(3);
            self.text(0, "§7You are playing through RimeMC's built-in Bedrock bridge.");
        }
    }

    fn add_entity(&mut self, r: &mut Reader) -> Result<()> {
        let eid = r.varint()?;
        let uuid = r.uuid()?;
        let ty = r.varint()?;
        let pos = [r.f64()?, r.f64()?, r.f64()?];
        // LpVec3 velocity
        let a = r.u8()?;
        if a != 0 {
            r.u8()?;
            r.bytes(4)?;
            if a & 4 != 0 {
                r.varint()?;
            }
        }
        let pitch = r.u8()? as f32 * 360.0 / 256.0;
        let yaw = r.u8()? as f32 * 360.0 / 256.0;
        let name = Registries::get().entries("minecraft:entity_type").and_then(|e| e.get(ty as usize)).map(|s| s.trim_start_matches("minecraft:").to_string()).unwrap_or_default();
        if name == "player" {
            let Some(p) = self.st.game.players().into_iter().find(|p| p.eid == eid) else { return Ok(()) };
            self.player_list_add(uuid, eid, &p.name);
            let mut w = W::new();
            w.uuid(uuid).str(&p.name).var(eid as u64).str("");
            w.vec3([pos[0] as f32, (pos[1] + EYE) as f32, pos[2] as f32]).vec3([0.0; 3]).f32(pitch).f32(yaw).f32(yaw);
            empty_item(&mut w);
            w.zz32(gamemode_to_bedrock(p.gamemode.load(std::sync::atomic::Ordering::Relaxed)));
            w.vu32(0); // metadata
            w.vu32(0).vu32(0); // properties
            w.i64le(eid as i64).u8(1).u8(0).u8(0); // permission, command permission, ability layers
            w.vu32(0); // links
            w.bool(false); // passenger of block
            w.str("");
            w.i32le(7); // device os (Windows)
            self.send(ADD_PLAYER, &w);
            self.tracked.insert(eid, Tracked { player: true, uuid });
        } else if SHOWN_ENTITIES.contains(&name.as_str()) {
            let mut w = W::new();
            w.zz64(eid as i64).var(eid as u64).str(&format!("minecraft:{name}"));
            w.vec3([pos[0] as f32, pos[1] as f32, pos[2] as f32]).vec3([0.0; 3]).f32(pitch).f32(yaw).f32(yaw).f32(yaw);
            w.vu32(0).vu32(0).vu32(0).vu32(0).vu32(0).bool(false);
            self.send(ADD_ENTITY, &w);
            self.tracked.insert(eid, Tracked { player: false, uuid });
        }
        Ok(())
    }

    fn move_entity(&mut self, eid: i32, pos: [f64; 3], yaw: f32, pitch: f32, on_ground: bool) {
        let Some(t) = self.tracked.get(&eid) else { return };
        let mut w = W::new();
        if t.player {
            w.vu32(eid as u32).vec3([pos[0] as f32, (pos[1] + EYE) as f32, pos[2] as f32]).f32(pitch).f32(yaw).f32(yaw).u8(0).bool(on_ground).vu32(0).bool(false).var(0);
            self.send(MOVE_PLAYER, &w);
        } else {
            w.var(eid as u64).u8(if on_ground { 1 } else { 0 }).vec3([pos[0] as f32, pos[1] as f32, pos[2] as f32]).u8(angle(pitch)).u8(angle(yaw)).u8(angle(yaw));
            self.send(MOVE_ENTITY, &w);
        }
    }

    fn player_list_add(&mut self, uuid: uuid::Uuid, eid: i32, name: &str) {
        let mut w = W::new();
        w.vu32(1).vu32(1).u8(0).uuid(uuid).zz64(eid as i64).str(name).str("").str("").str("").i32le(7);
        default_skin(&mut w, uuid);
        w.bool(false).bool(false).bool(false).i32be(0);
        self.send(PLAYER_LIST, &w);
    }

    fn player_list_remove(&mut self, uuid: uuid::Uuid) {
        let mut w = W::new();
        w.vu32(1).vu32(0).u8(1).uuid(uuid);
        self.send(PLAYER_LIST, &w);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn varlong(r: &mut Reader) -> Result<i64> {
    let mut v = 0i64;
    for i in 0..10 {
        let b = r.u8()?;
        v |= ((b & 0x7f) as i64) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    bail!("varlong too long")
}

/// ItemV4 (as written in 1.26.60): skipped field by field.
fn skip_item(r: &mut R) -> Result<()> {
    let _id = r.i16le()?;
    let _count = r.u16le()?;
    let _meta = r.vu32()?;
    if r.bool()? {
        r.zz32()?;
    }
    let _block = r.vu32()?;
    let n = r.vu32()? as usize;
    r.take(n)?;
    Ok(())
}

fn empty_item(w: &mut W) {
    // network id, count, metadata, no stack id, block runtime id, extra data
    w.i16le(0).u16le(0).vu32(0).bool(false).vu32(0);
    let mut extra = W::new();
    extra.u16le(0).i32le(0).i32le(0);
    w.bytes(&extra.0);
}

/// A plain 64×64 skin (Bedrock refuses players without one).
fn default_skin(w: &mut W, uuid: uuid::Uuid) {
    let mut img = vec![0u8; 64 * 64 * 4];
    for px in img.chunks_exact_mut(4) {
        px.copy_from_slice(&[0x8a, 0x6e, 0x55, 0xff]);
    }
    w.str("Standard_Custom").str(r#"{"geometry":{"default":"geometry.humanoid.custom"}}"#);
    w.i32le(64).i32le(64).bytes(&img);
    w.vu32(0); // animations
    w.i32le(0).i32le(0).bytes(&[]); // cape
    w.str("").str("0.0.0").str("").str("").str(&format!("{uuid}")).u8(1).i32le(0);
    w.vu32(0).vu32(0); // persona pieces, tints
    w.bool(false).bool(false).bool(false).bool(true).bool(false).str("").str("");
}

/// AvailableEntityIdentifiers: network NBT `{idlist: [{id: ..., rid: n}, ...]}`.
fn entity_identifiers() -> Vec<u8> {
    fn s(o: &mut W, v: &str) {
        o.str(v);
    }
    let mut o = W::new();
    o.u8(10);
    s(&mut o, "");
    o.u8(9);
    s(&mut o, "idlist");
    o.u8(10).zz32(SHOWN_ENTITIES.len() as i32 + 1);
    for (i, n) in SHOWN_ENTITIES.iter().chain(std::iter::once(&"player")).enumerate() {
        o.u8(8);
        s(&mut o, "bid");
        s(&mut o, "");
        o.u8(1);
        s(&mut o, "hasspawnegg");
        o.u8(0);
        o.u8(8);
        s(&mut o, "id");
        s(&mut o, &format!("minecraft:{n}"));
        o.u8(3);
        s(&mut o, "rid");
        o.zz32(i as i32 + 1);
        o.u8(1);
        s(&mut o, "summonable");
        o.u8(0);
        o.u8(0);
    }
    o.u8(0);
    o.0
}

/// The in-process Java client: logs in, finishes configuration, then
/// forwards play packets both ways.
async fn java_client(io: DuplexStream, to_session: mpsc::Sender<Vec<u8>>, mut from_session: mpsc::Receiver<Vec<u8>>, view: i32) {
    let res: Result<()> = async {
        let (mut rd, mut wr) = tokio::io::split(io);
        let mut th = -1;
        loop {
            let p = conn::read(&mut rd, th >= 0, 1 << 20).await?;
            let mut r = Reader::new(&p);
            let pid = r.varint()?;
            if pid == jid("login", "clientbound", "login_compression") {
                th = r.varint()?;
            } else if pid == jid("login", "clientbound", "login_finished") {
                break;
            } else if pid == jid("login", "clientbound", "login_disconnect") {
                bail!("refused: {}", r.string(1 << 16).unwrap_or("?"));
            }
        }
        wr.write_all(&conn::encode(jid("login", "serverbound", "login_acknowledged"), &[], th)).await?;
        let mut b = Vec::new();
        b.str("en_us").u8(view.clamp(2, 32) as u8).vi(0).bool(true).u8(0x7f).vi(1).bool(false).bool(true).vi(0);
        wr.write_all(&conn::encode(jid("configuration", "serverbound", "client_information"), &b, th)).await?;
        loop {
            let p = conn::read(&mut rd, th >= 0, 16 << 20).await?;
            let mut r = Reader::new(&p);
            let pid = r.varint()?;
            let cb = |n: &str| jid("configuration", "clientbound", n);
            let sb = |n: &str| jid("configuration", "serverbound", n);
            if pid == cb("select_known_packs") {
                let mut b = Vec::new();
                b.vi(1).str("minecraft").str("core").str(crate::registry::blocks::MC_VERSION);
                wr.write_all(&conn::encode(sb("select_known_packs"), &b, th)).await?;
            } else if pid == cb("finish_configuration") {
                wr.write_all(&conn::encode(sb("finish_configuration"), &[], th)).await?;
                break;
            } else if pid == cb("keep_alive") {
                let k = r.i64()?;
                let mut b = Vec::new();
                b.i64(k);
                wr.write_all(&conn::encode(sb("keep_alive"), &b, th)).await?;
            } else if pid == cb("ping") {
                let k = r.i32()?;
                let mut b = Vec::new();
                b.i32(k);
                wr.write_all(&conn::encode(sb("pong"), &b, th)).await?;
            } else if pid == cb("disconnect") {
                bail!("disconnected during configuration");
            }
        }
        let reader = tokio::spawn(async move {
            while let Ok(p) = conn::read(&mut rd, th >= 0, conn::MAX_DECOMPRESSED).await {
                if to_session.send(p).await.is_err() {
                    break;
                }
            }
        });
        let loaded = {
            let mut p = Vec::new();
            p.vi(jid("play", "serverbound", "player_loaded"));
            p
        };
        let mut loaded = Some(loaded);
        while let Some(p) = from_session.recv().await {
            let mut r = Reader::new(&p);
            let pid = r.varint()?;
            wr.write_all(&conn::encode(pid, &p[r.pos..], th)).await?;
            if pid == jid("play", "serverbound", "move_player_pos_rot") {
                if let Some(l) = loaded.take() {
                    let mut r = Reader::new(&l);
                    let pid = r.varint()?;
                    wr.write_all(&conn::encode(pid, &[], th)).await?;
                }
            }
        }
        reader.abort();
        let _ = wr.shutdown().await;
        Ok(())
    }
    .await;
    if let Err(e) = res {
        tracing::debug!("Bedrock bridge: {e:#}");
    }
}

/// Runs one Bedrock connection until it closes.
pub async fn run(st: Arc<ServerState>, sock: Arc<UdpSocket>, addr: SocketAddr, mtu: usize, mut rx: mpsc::Receiver<Vec<u8>>) {
    let mut s = Session::new(st, sock, addr, mtu);
    let (java_in, mut java_rx) = mpsc::channel::<Vec<u8>>(4096);
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut java_open = true;
    loop {
        tokio::select! {
            d = rx.recv() => match d {
                Some(d) => match s.rak.receive(&d) {
                    Ok(msgs) => {
                        for m in msgs {
                            if let Err(e) = s.message(m, &java_in) {
                                tracing::debug!("Bedrock {addr}: {e:#}");
                                s.disconnect("Bad packet");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Bedrock {addr}: RakNet: {e:#}");
                        s.closed = true;
                    }
                },
                None => break,
            },
            p = java_rx.recv(), if java_open => match p {
                Some(p) => {
                    let mut next = Some(p);
                    let mut n = 0;
                    while let Some(p) = next.take() {
                        if let Err(e) = s.java_packet(&p) {
                            tracing::debug!("Bedrock {addr}: translating: {e:#}");
                        }
                        n += 1;
                        if n < 256 {
                            next = java_rx.try_recv().ok();
                        }
                    }
                }
                None => java_open = false,
            },
            _ = tick.tick() => {
                s.maybe_spawn();
                if s.java_started && s.java.as_ref().is_some_and(|j| j.is_closed()) && !s.closed {
                    s.disconnect("Disconnected");
                }
                s.flush().await;
                if s.rak.last_recv.elapsed() > TIMEOUT {
                    break;
                }
            }
        }
        if s.closed {
            s.flush().await;
            // a moment for the disconnect to arrive (and be acknowledged)
            let end = Instant::now() + Duration::from_millis(500);
            while Instant::now() < end && s.rak.pending() > 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                while let Ok(d) = rx.try_recv() {
                    let _ = s.rak.receive(&d);
                }
                s.flush().await;
            }
            let _ = s.sock.send_to(&[0x84, 0, 0, 0, 0, 0, 8, raknet::ID_DISCONNECT], addr).await;
            break;
        }
    }
    drop(s.java.take());
    drop(java_in);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_uuids() {
        assert_eq!(java_name(".", "Steve Bed", true), ".Steve_Bed");
        assert_eq!(java_name(".", "Ü ber☃LongGamertag123", true), "._berLongGamerta");
        let entity_ids = entity_identifiers();
        assert_eq!(entity_ids[0], 10);
        assert_eq!(*entity_ids.last().unwrap(), 0);
    }

    #[test]
    fn java_packets_exist() {
        for n in ["move_player_pos_rot", "accept_teleportation", "chunk_batch_received", "chat", "chat_command", "player_action", "use_item_on", "use_item", "interact", "set_carried_item", "swing", "client_command", "keep_alive", "pong", "player_loaded"] {
            jid("play", "serverbound", n);
        }
        for n in ["login", "player_position", "level_chunk_with_light", "chunk_batch_finished", "block_update", "section_blocks_update", "system_chat", "disconnect", "keep_alive", "ping", "set_time", "set_health", "game_event", "add_entity", "entity_position_sync", "remove_entities", "start_configuration", "block_changed_ack"] {
            jid("play", "clientbound", n);
        }
    }
}
