//! RakNet connected session: datagram sequence numbers with ACK/NACK,
//! reliable frames with resends, ordering on channels and split packets.
//! Only what Bedrock uses: everything we send is reliable-ordered on
//! channel 0.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

pub const ID_CONNECTED_PING: u8 = 0x00;
pub const ID_CONNECTED_PONG: u8 = 0x03;
pub const ID_OPEN_CONNECTION_REQUEST_1: u8 = 0x05;
pub const ID_OPEN_CONNECTION_REPLY_1: u8 = 0x06;
pub const ID_OPEN_CONNECTION_REQUEST_2: u8 = 0x07;
pub const ID_OPEN_CONNECTION_REPLY_2: u8 = 0x08;
pub const ID_CONNECTION_REQUEST: u8 = 0x09;
pub const ID_CONNECTION_REQUEST_ACCEPTED: u8 = 0x10;
pub const ID_NEW_INCOMING_CONNECTION: u8 = 0x13;
pub const ID_DISCONNECT: u8 = 0x15;
pub const ID_INCOMPATIBLE_PROTOCOL: u8 = 0x19;
pub const ID_GAME: u8 = 0xfe;
pub const RAKNET_PROTOCOL: u8 = 11;

const FLAG_VALID: u8 = 0x80;
const FLAG_ACK: u8 = 0x40;
const FLAG_NACK: u8 = 0x20;
const RESEND_AFTER: Duration = Duration::from_millis(500);
/// fragments of one message / concurrent split messages / buffered out-of-order messages
const MAX_SPLIT_COUNT: u32 = 8192;
const MAX_SPLITS: usize = 32;
const MAX_ORDER_BUFFER: usize = 4096;
const MAX_MESSAGE: usize = 32 << 20;
/// Reliable frames in flight (unacknowledged). Peers keep a 256-entry
/// receive window and drop whatever lies beyond it, so stay well inside.
const SEND_WINDOW: usize = 128;

fn u24(b: &[u8]) -> u32 {
    b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16
}

fn put_u24(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8]);
}

pub fn write_address(o: &mut Vec<u8>, a: SocketAddr) {
    match a {
        SocketAddr::V4(v) => {
            o.push(4);
            o.extend(v.ip().octets().iter().map(|b| !b));
            o.extend_from_slice(&v.port().to_be_bytes());
        }
        SocketAddr::V6(v) => {
            o.push(6);
            o.extend_from_slice(&23u16.to_le_bytes());
            o.extend_from_slice(&v.port().to_be_bytes());
            o.extend_from_slice(&v.flowinfo().to_be_bytes());
            o.extend_from_slice(&v.ip().octets());
            o.extend_from_slice(&v.scope_id().to_be_bytes());
        }
    }
}

fn reliable(r: u8) -> bool {
    matches!(r, 2 | 3 | 4 | 6 | 7)
}
fn sequenced(r: u8) -> bool {
    matches!(r, 1 | 4)
}
fn ordered(r: u8) -> bool {
    matches!(r, 1 | 3 | 4 | 7)
}

#[derive(Clone)]
struct Frame {
    reliability: u8,
    reliable_index: u32,
    sequence_index: u32,
    order_index: u32,
    channel: u8,
    split: Option<(u32, u16, u32)>,
    body: Vec<u8>,
}

impl Frame {
    fn size(&self) -> usize {
        3 + if reliable(self.reliability) { 3 } else { 0 }
            + if sequenced(self.reliability) { 3 } else { 0 }
            + if ordered(self.reliability) { 4 } else { 0 }
            + if self.split.is_some() { 10 } else { 0 }
            + self.body.len()
    }

    fn write(&self, o: &mut Vec<u8>) {
        o.push(self.reliability << 5 | if self.split.is_some() { 0x10 } else { 0 });
        o.extend_from_slice(&((self.body.len() * 8) as u16).to_be_bytes());
        if reliable(self.reliability) {
            put_u24(o, self.reliable_index);
        }
        if sequenced(self.reliability) {
            put_u24(o, self.sequence_index);
        }
        if ordered(self.reliability) {
            put_u24(o, self.order_index);
            o.push(self.channel);
        }
        if let Some((count, id, index)) = self.split {
            o.extend_from_slice(&count.to_be_bytes());
            o.extend_from_slice(&id.to_be_bytes());
            o.extend_from_slice(&index.to_be_bytes());
        }
        o.extend_from_slice(&self.body);
    }

    fn read(b: &[u8], pos: &mut usize) -> Result<Frame> {
        let need = |pos: usize, n: usize| -> Result<()> {
            if pos + n > b.len() {
                bail!("truncated frame");
            }
            Ok(())
        };
        need(*pos, 3)?;
        let flags = b[*pos];
        let bits = u16::from_be_bytes([b[*pos + 1], b[*pos + 2]]) as usize;
        *pos += 3;
        let reliability = flags >> 5;
        let mut f = Frame { reliability, reliable_index: 0, sequence_index: 0, order_index: 0, channel: 0, split: None, body: Vec::new() };
        if reliable(reliability) {
            need(*pos, 3)?;
            f.reliable_index = u24(&b[*pos..]);
            *pos += 3;
        }
        if sequenced(reliability) {
            need(*pos, 3)?;
            f.sequence_index = u24(&b[*pos..]);
            *pos += 3;
        }
        if ordered(reliability) {
            need(*pos, 4)?;
            f.order_index = u24(&b[*pos..]);
            f.channel = b[*pos + 3];
            *pos += 4;
            if f.channel >= 32 {
                bail!("bad order channel");
            }
        }
        if flags & 0x10 != 0 {
            need(*pos, 10)?;
            let count = u32::from_be_bytes(b[*pos..*pos + 4].try_into()?);
            let id = u16::from_be_bytes(b[*pos + 4..*pos + 6].try_into()?);
            let index = u32::from_be_bytes(b[*pos + 6..*pos + 10].try_into()?);
            *pos += 10;
            if count == 0 || count > MAX_SPLIT_COUNT || index >= count {
                bail!("bad split");
            }
            f.split = Some((count, id, index));
        }
        let len = bits.div_ceil(8);
        need(*pos, len)?;
        f.body = b[*pos..*pos + len].to_vec();
        *pos += len;
        Ok(f)
    }
}

struct Split {
    parts: Vec<Option<Vec<u8>>>,
    have: u32,
    size: usize,
}

/// One RakNet peer.
pub struct Conn {
    pub addr: SocketAddr,
    mtu: usize,
    // receiving
    acks: Vec<u32>,
    nacks: Vec<u32>,
    next_dgram: u32,
    rel_base: u32,
    rel_seen: HashSet<u32>,
    order_next: [u32; 32],
    order_buf: [BTreeMap<u32, Vec<u8>>; 32],
    splits: HashMap<u16, Split>,
    // sending
    send_seq: u32,
    rel_index: u32,
    order_index: u32,
    split_id: u16,
    queue: Vec<Frame>,
    unacked: BTreeMap<u32, (Instant, Vec<Frame>)>,
    pub last_recv: Instant,
}

impl Conn {
    pub fn new(addr: SocketAddr, mtu: usize) -> Conn {
        Conn {
            addr,
            mtu: mtu.clamp(400, 1500),
            acks: Vec::new(),
            nacks: Vec::new(),
            next_dgram: 0,
            rel_base: 0,
            rel_seen: HashSet::new(),
            order_next: [0; 32],
            order_buf: std::array::from_fn(|_| BTreeMap::new()),
            splits: HashMap::new(),
            send_seq: 0,
            rel_index: 0,
            order_index: 0,
            split_id: 0,
            queue: Vec::new(),
            unacked: BTreeMap::new(),
            last_recv: Instant::now(),
        }
    }

    /// Largest datagram we send (MTU minus IP/UDP headers).
    fn max_datagram(&self) -> usize {
        self.mtu - 28
    }

    /// Handles one datagram; returns the complete messages it finished.
    pub fn receive(&mut self, d: &[u8]) -> Result<Vec<Vec<u8>>> {
        let Some(&flags) = d.first() else { return Ok(Vec::new()) };
        if flags & FLAG_VALID == 0 {
            return Ok(Vec::new());
        }
        self.last_recv = Instant::now();
        if flags & FLAG_ACK != 0 {
            for s in read_ranges(&d[1..])? {
                self.unacked.remove(&s);
            }
            return Ok(Vec::new());
        }
        if flags & FLAG_NACK != 0 {
            for s in read_ranges(&d[1..])? {
                if let Some((_, frames)) = self.unacked.remove(&s) {
                    self.queue.extend(frames);
                }
            }
            return Ok(Vec::new());
        }
        if d.len() < 4 {
            bail!("short datagram");
        }
        let seq = u24(&d[1..]);
        if self.acks.len() < 4096 {
            self.acks.push(seq);
        }
        let ahead = seq.wrapping_sub(self.next_dgram) & 0xffffff;
        if ahead < 0x800000 {
            // missing datagrams in between: ask for them again
            for s in self.next_dgram..seq.min(self.next_dgram + 256) {
                self.nacks.push(s & 0xffffff);
            }
            self.next_dgram = (seq + 1) & 0xffffff;
        } else {
            self.nacks.retain(|s| *s != seq);
        }
        let mut out = Vec::new();
        let mut pos = 4;
        while pos < d.len() {
            let f = Frame::read(d, &mut pos)?;
            self.frame(f, &mut out)?;
        }
        Ok(out)
    }

    fn frame(&mut self, f: Frame, out: &mut Vec<Vec<u8>>) -> Result<()> {
        if reliable(f.reliability) {
            let i = f.reliable_index;
            if i.wrapping_sub(self.rel_base) & 0xffffff >= 0x800000 || self.rel_seen.contains(&i) {
                return Ok(()); // duplicate
            }
            if self.rel_seen.len() > 65536 {
                bail!("too many reliable frames in flight");
            }
            self.rel_seen.insert(i);
            while self.rel_seen.remove(&self.rel_base) {
                self.rel_base = (self.rel_base + 1) & 0xffffff;
            }
        }
        let body = match f.split {
            None => f.body,
            Some((count, id, index)) => {
                if !self.splits.contains_key(&id) && self.splits.len() >= MAX_SPLITS {
                    bail!("too many split packets");
                }
                let s = self.splits.entry(id).or_insert_with(|| Split { parts: vec![None; count as usize], have: 0, size: 0 });
                if s.parts.len() != count as usize {
                    bail!("split count changed");
                }
                if s.parts[index as usize].is_none() {
                    s.size += f.body.len();
                    if s.size > MAX_MESSAGE {
                        bail!("split message too large");
                    }
                    s.parts[index as usize] = Some(f.body);
                    s.have += 1;
                }
                if s.have < count {
                    return Ok(());
                }
                let s = self.splits.remove(&id).unwrap();
                let mut all = Vec::with_capacity(s.size);
                for p in s.parts {
                    all.extend_from_slice(&p.unwrap());
                }
                all
            }
        };
        if ordered(f.reliability) && !sequenced(f.reliability) {
            let ch = f.channel as usize;
            let next = self.order_next[ch];
            let d = f.order_index.wrapping_sub(next) & 0xffffff;
            if d == 0 {
                out.push(body);
                self.order_next[ch] = (next + 1) & 0xffffff;
                while let Some(b) = self.order_buf[ch].remove(&self.order_next[ch]) {
                    out.push(b);
                    self.order_next[ch] = (self.order_next[ch] + 1) & 0xffffff;
                }
            } else if d < 0x800000 {
                if self.order_buf[ch].len() >= MAX_ORDER_BUFFER {
                    bail!("ordering buffer overflow");
                }
                self.order_buf[ch].insert(f.order_index, body);
            }
            return Ok(());
        }
        out.push(body);
        Ok(())
    }

    /// Queues a reliable-ordered message, split to fit the MTU.
    pub fn send(&mut self, body: &[u8]) {
        let order = self.order_index;
        self.order_index = (self.order_index + 1) & 0xffffff;
        let room = self.max_datagram() - 4 - 10;
        if body.len() <= room {
            let f = Frame { reliability: 3, reliable_index: self.next_rel(), sequence_index: 0, order_index: order, channel: 0, split: None, body: body.to_vec() };
            self.queue.push(f);
            return;
        }
        let part = room - 10;
        let count = body.len().div_ceil(part) as u32;
        let id = self.split_id;
        self.split_id = self.split_id.wrapping_add(1);
        for (i, chunk) in body.chunks(part).enumerate() {
            let f = Frame { reliability: 3, reliable_index: self.next_rel(), sequence_index: 0, order_index: order, channel: 0, split: Some((count, id, i as u32)), body: chunk.to_vec() };
            self.queue.push(f);
        }
    }

    fn next_rel(&mut self) -> u32 {
        let r = self.rel_index;
        self.rel_index = (self.rel_index + 1) & 0xffffff;
        r
    }

    /// Datagrams to put on the wire now: ACKs, NACKs, queued frames and
    /// frames whose datagram was not acknowledged in time.
    pub fn flush(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if !self.acks.is_empty() {
            let mut a = std::mem::take(&mut self.acks);
            out.extend(write_ranges(FLAG_VALID | FLAG_ACK, &mut a));
        }
        if !self.nacks.is_empty() {
            let mut n = std::mem::take(&mut self.nacks);
            out.extend(write_ranges(FLAG_VALID | FLAG_NACK, &mut n));
        }
        let late: Vec<u32> = self.unacked.iter().filter(|(_, (t, _))| now.duration_since(*t) >= RESEND_AFTER).map(|(s, _)| *s).collect();
        for s in late {
            if let Some((_, frames)) = self.unacked.remove(&s) {
                self.queue.extend(frames);
            }
        }
        // resends first, then new frames, while the window has room
        let mut in_flight: usize = self.unacked.values().map(|(_, f)| f.len()).sum();
        let queue = std::mem::take(&mut self.queue);
        let max = self.max_datagram();
        let mut cur: Vec<Frame> = Vec::new();
        let mut size = 4;
        let mut rest = Vec::new();
        for f in queue {
            if !rest.is_empty() || in_flight + cur.len() >= SEND_WINDOW {
                rest.push(f);
                continue;
            }
            if size + f.size() > max && !cur.is_empty() {
                in_flight += cur.len();
                out.push(self.datagram(std::mem::take(&mut cur), now));
                size = 4;
                if in_flight >= SEND_WINDOW {
                    rest.push(f);
                    continue;
                }
            }
            size += f.size();
            cur.push(f);
        }
        if !cur.is_empty() {
            out.push(self.datagram(cur, now));
        }
        self.queue = rest;
        out
    }

    fn datagram(&mut self, frames: Vec<Frame>, now: Instant) -> Vec<u8> {
        let seq = self.send_seq;
        self.send_seq = (self.send_seq + 1) & 0xffffff;
        let mut d = vec![FLAG_VALID | 0x04];
        put_u24(&mut d, seq);
        for f in &frames {
            f.write(&mut d);
        }
        self.unacked.insert(seq, (now, frames));
        d
    }

    pub fn pending(&self) -> usize {
        self.unacked.len() + self.queue.len()
    }
}

fn read_ranges(b: &[u8]) -> Result<Vec<u32>> {
    if b.len() < 2 {
        bail!("short ack");
    }
    let n = u16::from_be_bytes([b[0], b[1]]) as usize;
    let mut pos = 2;
    let mut out = Vec::new();
    for _ in 0..n {
        if pos >= b.len() {
            bail!("short ack");
        }
        let single = b[pos] != 0;
        pos += 1;
        if pos + 3 > b.len() {
            bail!("short ack");
        }
        let start = u24(&b[pos..]);
        pos += 3;
        let end = if single {
            start
        } else {
            if pos + 3 > b.len() {
                bail!("short ack");
            }
            let e = u24(&b[pos..]);
            pos += 3;
            e
        };
        if end < start || end - start > 8192 || out.len() > 65536 {
            bail!("bad ack range");
        }
        out.extend(start..=end);
    }
    Ok(out)
}

fn write_ranges(flags: u8, seqs: &mut Vec<u32>) -> Vec<Vec<u8>> {
    seqs.sort_unstable();
    seqs.dedup();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for &s in seqs.iter() {
        match ranges.last_mut() {
            Some((_, e)) if *e + 1 == s => *e = s,
            _ => ranges.push((s, s)),
        }
    }
    ranges
        .chunks(150)
        .map(|rs| {
            let mut d = vec![flags];
            d.extend_from_slice(&(rs.len() as u16).to_be_bytes());
            for &(a, b) in rs {
                if a == b {
                    d.push(1);
                    put_u24(&mut d, a);
                } else {
                    d.push(0);
                    put_u24(&mut d, a);
                    put_u24(&mut d, b);
                }
            }
            d
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Conn, Conn) {
        let a: SocketAddr = "127.0.0.1:1".parse().unwrap();
        (Conn::new(a, 1400), Conn::new(a, 1400))
    }

    #[test]
    fn split_and_reorder() {
        let (mut tx, mut rx) = pair();
        let big: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        let _ = &mut rx;
        tx.send(b"first");
        tx.send(&big);
        tx.send(b"third");
        let mut dg = tx.flush(Instant::now());
        assert!(dg.len() > 7);
        dg.reverse(); // worst-case arrival order
        let mut got = Vec::new();
        for d in &dg {
            got.extend(rx.receive(d).unwrap());
        }
        assert_eq!(got, vec![b"first".to_vec(), big, b"third".to_vec()]);
        // duplicates are dropped
        assert!(rx.receive(&dg[0]).unwrap().is_empty());
    }

    #[test]
    fn nack_resends_and_ack_clears() {
        let (mut tx, mut rx) = pair();
        tx.send(b"a");
        tx.send(&vec![7u8; 3000]);
        let dg = tx.flush(Instant::now());
        // lose the first datagram
        for d in &dg[1..] {
            rx.receive(d).unwrap();
        }
        for ctl in rx.flush(Instant::now()) {
            tx.receive(&ctl).unwrap();
        }
        let again = tx.flush(Instant::now());
        assert_eq!(again.len(), 1, "only the lost datagram is resent");
        let msgs = rx.receive(&again[0]).unwrap();
        assert_eq!(msgs.len(), 2);
        for ctl in rx.flush(Instant::now()) {
            tx.receive(&ctl).unwrap();
        }
        assert_eq!(tx.pending(), 0);
    }

    #[test]
    fn send_window_limits_in_flight() {
        let (mut tx, mut rx) = pair();
        let big = vec![1u8; 1 << 20];
        tx.send(&big);
        let first = tx.flush(Instant::now());
        let frames: usize = tx.unacked.values().map(|(_, f)| f.len()).sum();
        assert!(frames <= SEND_WINDOW && !tx.queue.is_empty());
        let mut got = Vec::new();
        let mut batch = first;
        for _ in 0..1000 {
            for d in &batch {
                got.extend(rx.receive(d).unwrap());
            }
            for ctl in rx.flush(Instant::now()) {
                tx.receive(&ctl).unwrap();
            }
            batch = tx.flush(Instant::now());
            if batch.is_empty() {
                break;
            }
        }
        assert_eq!(got, vec![big]);
    }

    #[test]
    fn hostile_input_is_rejected() {
        let (_, mut rx) = pair();
        assert!(rx.receive(&[0x84, 0, 0, 0, 0x70, 0xff, 0xff]).is_err());
        let mut d = vec![0x84, 0, 0, 0, 0x70, 0, 8, 0, 0, 0, 0, 0, 0, 0];
        d.extend_from_slice(&u32::MAX.to_be_bytes()); // split count
        d.extend_from_slice(&[0, 1, 0, 0, 0, 0, 1]);
        assert!(rx.receive(&d).is_err());
        assert!(read_ranges(&[0, 1, 0, 0, 0, 0, 0xff, 0xff, 0xff]).is_err());
    }
}
