//! Messages exchanged inside the encrypted fleet channel.

use bincode::Options;
use serde::{Deserialize, Serialize};

use crate::balancer::CellId;

/// Upper bound for a decoded message (protects against hostile lengths even
/// after authentication, e.g. from a compromised node).
pub const MAX_MESSAGE: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FleetMsg {
    /// node → router after the handshake
    Register { cores: u32, version: String, game_addr: String },
    /// node → router, every second
    Heartbeat { mspt: f32, cells: Vec<CellLoad> },
    /// router → node: you now own / no longer own these cells
    Assign { cells: Vec<CellId> },
    Revoke { cells: Vec<CellId> },
    /// router → node: you are (not) the redstone node
    RedstoneRole { active: bool },
    /// node → router: please send chunk data (router owns the world)
    ChunkRequest { world: u32, x: i32, z: i32 },
    /// router → node / node → router: raw chunk NBT (None = not generated yet)
    ChunkData { world: u32, x: i32, z: i32, nbt: Option<Vec<u8>> },
    /// Player simulation moves between nodes. `state` is the serialized
    /// entity/player state; the client connection stays on the router.
    PlayerHandoff { player: u128, cell: CellId, state: Vec<u8> },
    /// node → router: a player's saved state (vanilla player NBT) when they
    /// leave the node; the router owns the player files
    PlayerData { player: u128, state: Vec<u8> },
    /// router → node: packet from a client (already decoded to game action)
    ClientPacket { player: u128, data: Vec<u8> },
    /// node → router: packet to forward to a client
    ServerPacket { player: u128, data: Vec<u8> },
    /// router → node: graceful shutdown, flush state
    Shutdown,
}

/// Sent by the router as the first record on a node's game port: who the
/// player is and their saved state. Authenticated with `crypto::forward_mac`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerForward {
    pub uuid: u128,
    pub name: String,
    /// profile properties (skin): (name, value, signature)
    pub props: Vec<(String, String, Option<String>)>,
    /// vanilla player NBT (None: new player)
    pub state: Option<Vec<u8>>,
    /// milliseconds since the Unix epoch (replay window)
    pub time_ms: u64,
}

impl PlayerForward {
    pub fn encode(&self) -> Vec<u8> {
        opts().serialize(self).expect("serialization cannot fail")
    }
    pub fn decode(b: &[u8]) -> Result<Self, bincode::Error> {
        opts().deserialize(b)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellLoad {
    pub cell: CellId,
    /// milliseconds of tick time per second spent on this cell
    pub cost: f32,
    pub redstone: bool,
}

fn opts() -> impl Options {
    bincode::DefaultOptions::new().with_limit(MAX_MESSAGE).reject_trailing_bytes()
}

impl FleetMsg {
    pub fn encode(&self) -> Vec<u8> {
        opts().serialize(self).expect("fleet message serialization cannot fail")
    }
    pub fn decode(b: &[u8]) -> Result<Self, bincode::Error> {
        opts().deserialize(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_garbage() {
        let m = FleetMsg::Heartbeat { mspt: 3.5, cells: vec![CellLoad { cell: CellId { world: 0, x: 1, z: -1 }, cost: 2.0, redstone: true }] };
        assert_eq!(FleetMsg::decode(&m.encode()).unwrap(), m);
        // a huge declared vec length must not allocate
        assert!(FleetMsg::decode(&[7, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]).is_err());
    }

    #[test]
    fn forward_is_signed() {
        let f = PlayerForward { uuid: 7, name: "Steve".into(), props: vec![], state: Some(vec![1, 2, 3]), time_ms: 5 };
        let bytes = f.encode();
        let secret = b"0123456789abcdef0123456789abcdef";
        let tag = crate::crypto::forward_mac(secret, &bytes);
        assert!(crate::crypto::verify_forward(secret, &bytes, &tag));
        assert!(!crate::crypto::verify_forward(b"another-secret-another-secret-xx", &bytes, &tag));
        let mut forged = bytes.clone();
        forged[0] ^= 1;
        assert!(!crate::crypto::verify_forward(secret, &forged, &tag));
        assert_eq!(PlayerForward::decode(&bytes).unwrap(), f);
    }
}
