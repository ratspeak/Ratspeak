//! Shared domain DTOs used across the runtime, db, and tauri layers.

/// LRGP `msg_id` → originating session metadata, used by the runtime to route
/// LXMF delivery proofs back to the correct game session.
#[derive(Clone, Debug)]
pub struct LrgpMsgMeta {
    pub session_id: String,
    pub identity_id: String,
    pub contact_hash: String,
    pub app_id: String,
    pub sent_at: f64,
}

/// One row of the Peers list. `last_interface` is stamped atomically with
/// `last_seen` so the iface badge survives restart.
#[derive(Debug, Clone)]
pub struct PeerRow {
    pub hash: String,
    /// Reticulum identity hash when recovered from a validated announce.
    /// Empty when the row was created from message history or a manual contact.
    pub identity_hash: String,
    /// `None` for contacts with no activity row.
    pub last_seen: Option<f64>,
    /// `None` for contacts with no activity row.
    pub first_seen: Option<f64>,
    pub display_name: String,
    pub is_contact: bool,
    /// Empty for never-seen contacts.
    pub last_interface: String,
    /// Service aspects that make this row actionable in Ratspeak.
    pub services: Vec<String>,
}

pub const MAX_DISCOVERED_PROPAGATION_NODES: usize = 512;

/// 48h matches the RNS path-table expiry convention (`PATHFINDER_E`).
pub const PROPAGATION_NODE_TTL_SECS: u64 = 48 * 3600;
