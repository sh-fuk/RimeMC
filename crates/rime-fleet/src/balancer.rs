//! Cell → node assignment with automatic rebalancing and fail-over.
//!
//! The world is cut into *cells* (one cell = one 32×32-chunk region by
//! default). Every cell is simulated by exactly one node at a time.
//!
//! * **Initial placement** uses weighted rendezvous hashing, so placement is
//!   stable and spreads evenly without coordination.
//! * **Rebalancing**: every round, if the busiest node's normalised load is
//!   above `avg × (1 + imbalance_threshold)`, cells are moved from it to the
//!   least busy node. Cells that border cells already owned by the target are
//!   preferred (fewer cross-node borders), and a per-cell cooldown prevents
//!   ping-ponging.
//! * **Redstone**: cells with active redstone are pinned to one designated
//!   *redstone node*, so all redstone runs in a single deterministic tick loop.
//! * **Fail-over**: a node that misses heartbeats for `heartbeat_timeout_ms` is
//!   declared dead and all its cells (including the redstone role) are re-homed
//!   on the survivors immediately. With one survivor it simply runs everything.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

pub type NodeId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CellId {
    pub world: u32,
    pub x: i32,
    pub z: i32,
}

impl CellId {
    pub fn of_chunk(world: u32, chunk_x: i32, chunk_z: i32, cell_shift: u32) -> Self {
        Self { world, x: chunk_x >> cell_shift, z: chunk_z >> cell_shift }
    }
    fn neighbours(&self) -> [CellId; 4] {
        let w = self.world;
        [
            CellId { world: w, x: self.x + 1, z: self.z },
            CellId { world: w, x: self.x - 1, z: self.z },
            CellId { world: w, x: self.x, z: self.z + 1 },
            CellId { world: w, x: self.x, z: self.z - 1 },
        ]
    }
}

#[derive(Clone, Debug)]
pub struct BalancerConfig {
    pub imbalance_threshold: f64,
    pub heartbeat_timeout_ms: u64,
    pub move_cooldown_ms: u64,
    pub max_moves_per_round: usize,
}

impl Default for BalancerConfig {
    fn default() -> Self {
        Self { imbalance_threshold: 0.20, heartbeat_timeout_ms: 3000, move_cooldown_ms: 30_000, max_moves_per_round: 4 }
    }
}

#[derive(Clone, Debug)]
struct NodeState {
    alive: bool,
    last_heartbeat_ms: u64,
    capacity: f64,
}

#[derive(Clone, Debug, Default)]
struct CellState {
    owner: Option<NodeId>,
    load: f64,
    redstone: bool,
    last_move_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub cell: CellId,
    pub from: Option<NodeId>,
    pub to: Option<NodeId>,
}

pub struct Balancer {
    cfg: BalancerConfig,
    nodes: BTreeMap<NodeId, NodeState>,
    cells: HashMap<CellId, CellState>,
    redstone_node: Option<NodeId>,
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn rendezvous_score(cell: &CellId, node: NodeId, capacity: f64) -> f64 {
    let h = mix(mix(mix(cell.world as u64) ^ cell.x as u32 as u64) ^ ((cell.z as u32 as u64) << 32) ^ mix(node));
    // map to (0,1), weighted rendezvous: -w / ln(u)
    let u = ((h >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
    -capacity / u.ln()
}

impl Balancer {
    pub fn new(cfg: BalancerConfig) -> Self {
        Self { cfg, nodes: BTreeMap::new(), cells: HashMap::new(), redstone_node: None }
    }

    pub fn alive_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.iter().filter(|(_, n)| n.alive).map(|(id, _)| *id)
    }

    pub fn redstone_node(&self) -> Option<NodeId> {
        self.redstone_node
    }

    /// Register (or revive) a node. `capacity` is relative (e.g. core count).
    pub fn node_joined(&mut self, id: NodeId, capacity: f64, now_ms: u64) {
        let capacity = if capacity.is_finite() && capacity > 0.0 { capacity } else { 1.0 };
        self.nodes.insert(id, NodeState { alive: true, last_heartbeat_ms: now_ms, capacity });
        if self.redstone_node.is_none() {
            self.redstone_node = Some(id);
        }
    }

    pub fn heartbeat(&mut self, id: NodeId, now_ms: u64) {
        if let Some(n) = self.nodes.get_mut(&id) {
            n.last_heartbeat_ms = now_ms;
        }
    }

    /// Explicit disconnect (connection closed). Returns the resulting moves.
    pub fn node_left(&mut self, id: NodeId, now_ms: u64) -> Vec<Move> {
        if let Some(n) = self.nodes.get_mut(&id) {
            n.alive = false;
        }
        self.rehome_dead(now_ms)
    }

    /// Report the measured cost of a cell (e.g. ms of tick time per second).
    pub fn report_cell(&mut self, cell: CellId, load: f64, redstone: bool) {
        let c = self.cells.entry(cell).or_default();
        c.load = if load.is_finite() && load >= 0.0 { load } else { 0.0 };
        c.redstone = redstone;
    }

    /// Owner of a cell, assigning it if it has none yet.
    pub fn owner_of(&mut self, cell: CellId) -> Option<NodeId> {
        if let Some(o) = self.cells.get(&cell).and_then(|c| c.owner) {
            if self.nodes.get(&o).is_some_and(|n| n.alive) {
                return Some(o);
            }
        }
        let owner = self.place(&cell);
        self.cells.entry(cell).or_default().owner = owner;
        owner
    }

    fn place(&self, cell: &CellId) -> Option<NodeId> {
        if self.cells.get(cell).is_some_and(|c| c.redstone) {
            if let Some(r) = self.redstone_node {
                return Some(r);
            }
        }
        self.nodes
            .iter()
            .filter(|(_, n)| n.alive)
            .map(|(id, n)| (*id, rendezvous_score(cell, *id, n.capacity)))
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
            .map(|(id, _)| id)
    }

    fn node_loads(&self) -> BTreeMap<NodeId, f64> {
        let mut loads: BTreeMap<NodeId, f64> = self.alive_nodes().map(|id| (id, 0.0)).collect();
        for c in self.cells.values() {
            if let Some(o) = c.owner {
                if let Some(l) = loads.get_mut(&o) {
                    *l += c.load;
                }
            }
        }
        for (id, l) in loads.iter_mut() {
            *l /= self.nodes[id].capacity;
        }
        loads
    }

    fn rehome_dead(&mut self, now_ms: u64) -> Vec<Move> {
        let mut moves = Vec::new();
        if self.redstone_node.is_some_and(|r| !self.nodes.get(&r).is_some_and(|n| n.alive)) {
            // pick the least loaded survivor as new redstone node
            let loads = self.node_loads();
            self.redstone_node = loads.iter().min_by(|a, b| a.1.total_cmp(b.1).then(a.0.cmp(b.0))).map(|(id, _)| *id);
        }
        let orphaned: Vec<CellId> = self
            .cells
            .iter()
            .filter(|(_, c)| c.owner.is_some_and(|o| !self.nodes.get(&o).is_some_and(|n| n.alive)))
            .map(|(id, _)| *id)
            .collect();
        for cell in orphaned {
            let from = self.cells[&cell].owner;
            let to = self.place(&cell);
            let c = self.cells.get_mut(&cell).unwrap();
            c.owner = to;
            c.last_move_ms = now_ms;
            moves.push(Move { cell, from, to });
        }
        moves
    }

    /// One balancing round: detect dead nodes, enforce redstone pinning and
    /// even out load. Returns the hand-offs the router must perform.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Move> {
        for n in self.nodes.values_mut() {
            if n.alive && now_ms.saturating_sub(n.last_heartbeat_ms) > self.cfg.heartbeat_timeout_ms {
                n.alive = false;
            }
        }
        let mut moves = self.rehome_dead(now_ms);

        // Redstone pinning.
        if let Some(r) = self.redstone_node {
            for (id, c) in self.cells.iter_mut() {
                if c.redstone && c.owner.is_some() && c.owner != Some(r) {
                    moves.push(Move { cell: *id, from: c.owner, to: Some(r) });
                    c.owner = Some(r);
                    c.last_move_ms = now_ms;
                }
            }
        }

        // Load balancing.
        for _ in 0..self.cfg.max_moves_per_round {
            let loads = self.node_loads();
            if loads.len() < 2 {
                break;
            }
            let avg = loads.values().sum::<f64>() / loads.len() as f64;
            let (&hot, &hot_load) = loads.iter().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
            let (&cold, &cold_load) = loads.iter().min_by(|a, b| a.1.total_cmp(b.1)).unwrap();
            if avg <= 0.0 || hot_load <= avg * (1.0 + self.cfg.imbalance_threshold) {
                break;
            }
            // ideal amount of raw load to move
            let gap = (hot_load - cold_load) / 2.0 * self.nodes[&hot].capacity;
            let cooldown = self.cfg.move_cooldown_ms;
            let candidate = self
                .cells
                .iter()
                .filter(|(_, c)| {
                    c.owner == Some(hot) && !c.redstone && c.load > 0.0 && c.load <= gap * 1.5 && now_ms.saturating_sub(c.last_move_ms) >= cooldown
                })
                .map(|(id, c)| {
                    let adjacency = id.neighbours().iter().filter(|n| self.cells.get(n).is_some_and(|nc| nc.owner == Some(cold))).count();
                    let fit = 1.0 - ((c.load - gap).abs() / gap.max(1e-9)).min(1.0);
                    (*id, fit + adjacency as f64 * 0.25)
                })
                .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
                .map(|(id, _)| id);
            let Some(cell) = candidate else { break };
            let c = self.cells.get_mut(&cell).unwrap();
            c.owner = Some(cold);
            c.last_move_ms = now_ms;
            moves.push(Move { cell, from: Some(hot), to: Some(cold) });
        }
        moves
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: i32, z: i32) -> CellId {
        CellId { world: 0, x, z }
    }

    #[test]
    fn placement_uses_all_nodes() {
        let mut b = Balancer::new(BalancerConfig::default());
        b.node_joined(1, 1.0, 0);
        b.node_joined(2, 1.0, 0);
        let mut count = [0; 3];
        for x in 0..20 {
            for z in 0..20 {
                count[b.owner_of(cell(x, z)).unwrap() as usize] += 1;
            }
        }
        assert!(count[1] > 120 && count[2] > 120, "{count:?}");
    }

    #[test]
    fn failover_moves_everything_to_survivor() {
        let mut b = Balancer::new(BalancerConfig::default());
        b.node_joined(1, 1.0, 0);
        b.node_joined(2, 1.0, 0);
        for x in 0..10 {
            b.owner_of(cell(x, 0));
        }
        b.heartbeat(2, 5000);
        let moves = b.tick(5000); // node 1 timed out
        assert!(!moves.is_empty());
        for x in 0..10 {
            assert_eq!(b.owner_of(cell(x, 0)), Some(2));
        }
        assert_eq!(b.redstone_node(), Some(2));
    }

    #[test]
    fn redstone_is_pinned_to_one_node() {
        let mut b = Balancer::new(BalancerConfig::default());
        b.node_joined(1, 1.0, 0);
        b.node_joined(2, 1.0, 0);
        for x in 0..10 {
            b.owner_of(cell(x, 0));
            b.report_cell(cell(x, 0), 1.0, true);
        }
        b.tick(0);
        let r = b.redstone_node().unwrap();
        for x in 0..10 {
            assert_eq!(b.owner_of(cell(x, 0)), Some(r));
        }
    }

    #[test]
    fn hotspot_gets_rebalanced() {
        let mut b = Balancer::new(BalancerConfig { move_cooldown_ms: 0, ..Default::default() });
        b.node_joined(1, 1.0, 0);
        for x in 0..8 {
            b.owner_of(cell(x, 0));
            b.report_cell(cell(x, 0), 10.0, false);
        }
        b.node_joined(2, 1.0, 0);
        for t in 0..4 {
            b.tick(t);
        }
        let on2 = (0..8).filter(|x| b.owner_of(cell(*x, 0)) == Some(2)).count();
        assert!((3..=5).contains(&on2), "{on2}");
    }
}
