//! # RimeFleet
//!
//! Splits the simulation of **one** world across several RimeMC processes:
//!
//! ```text
//!  players ──► RimeRouter ──┬──► RimeNode 1   (simulates cells A, B, …)
//!   (Java/Bedrock)          ├──► RimeNode 2   (simulates cells C, D, …)
//!                           └──► RimeNode n
//! ```
//!
//! * Clients only ever talk to the **router**. Their TCP/UDP connection never
//!   moves, so crossing from a cell owned by node 1 into one owned by node 2 is
//!   a server-internal hand-off: no reconnect, no loading screen.
//! * The router owns the world data (AirWorld only) and streams chunks to the
//!   nodes; nodes write changes back.
//! * [`balancer`] continuously moves cells between nodes to avoid hot spots and
//!   re-homes everything if a node dies. Redstone is always simulated by one
//!   designated node so that redstone timing stays deterministic.
//! * All traffic is encrypted and authenticated with ChaCha20-Poly1305 using
//!   keys derived from a shared secret ([`crypto`]).

pub mod balancer;
pub mod crypto;
pub mod proto;

pub use balancer::{Balancer, BalancerConfig, CellId, NodeId};
pub use crypto::{Handshake, SecureChannel};
