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
    pub profile_status: String,
    pub is_contact: bool,
    /// Empty for never-seen contacts.
    pub last_interface: String,
    /// Service aspects that make this row actionable in Ratspeak.
    pub services: Vec<String>,
}

/// Interface discovery: listening, joining, and whose announcements count.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NetworkSettings {
    /// Listen for interfaces others publish (`discover_interfaces`).
    #[serde(default)]
    pub discover: bool,
    /// Discovered Backbone servers to keep joined; 0 is off.
    #[serde(default)]
    pub autojoin: u8,
    /// Accept discoveries only from `trusted_publishers`; none listed accepts none.
    #[serde(default)]
    pub trusted_only: bool,
    /// Publisher identity hashes, 32 hex characters each.
    #[serde(default)]
    pub trusted_publishers: Vec<String>,
    /// Stored fields this build does not own; a save carries them through.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredInterfaces {
    /// Discovery is running.
    pub enabled: bool,
    /// Most recently heard first, at most the list limit.
    pub entries: Vec<DiscoveredEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredEntry {
    /// The discovery hash, hex.
    pub id: String,
    pub name: String,
    /// `backbone`, `tcp`, `radio`, `i2p`, or `other`.
    pub kind: String,
    /// `available` (heard within 24 h), `unknown` (3 days), or `stale`.
    pub status: String,
    pub hops: u8,
    /// Unix seconds.
    pub last_heard: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    /// The publisher's identity hash, hex.
    pub publisher: String,
    /// Radios: Hz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spreading_factor: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding_rate: Option<u8>,
    /// It publishes a private network name and passphrase.
    pub private_network: bool,
    /// The publisher relays traffic.
    pub transport: bool,
    /// Join automatically is connected to it now.
    pub joined: bool,
    /// The live interface Join automatically made for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined_interface: Option<String>,
    /// A saved connection already points at it.
    pub added: bool,
    /// Connect can add it: a server with an address this device can reach.
    pub connectable: bool,
}

pub const MAX_DISCOVERED_PROPAGATION_NODES: usize = 512;

/// 48h matches the RNS path-table expiry convention (`PATHFINDER_E`).
pub const PROPAGATION_NODE_TTL_SECS: u64 = 48 * 3600;

/// LXMF destination app-names (wire strings shared by runtime, db, and tauri).
pub const LXMF_DELIVERY_APP_NAME: &str = "lxmf.delivery";
pub const LXMF_PROPAGATION_APP_NAME: &str = "lxmf.propagation";

/// Parse a 32-char hex string into 16 bytes. Byte-wise so malformed
/// (non-ASCII) input yields `None` instead of a slice panic.
pub fn hex_to_array16(s: &str) -> Option<[u8; 16]> {
    fn nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = (nibble(bytes[i * 2])? << 4) | nibble(bytes[i * 2 + 1])?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{NetworkSettings, hex_to_array16};

    #[test]
    fn network_settings_keep_stored_fields_they_do_not_own() {
        let stored =
            r#"{"discover":true,"autojoin":5,"block_list_sources":["cc"],"answer_probes":true}"#;
        let settings: NetworkSettings = serde_json::from_str(stored).unwrap();
        assert!(settings.discover && settings.autojoin == 5 && !settings.trusted_only);
        let saved = serde_json::to_value(&settings).unwrap();
        assert_eq!(saved["block_list_sources"], serde_json::json!(["cc"]));
        assert_eq!(saved["answer_probes"], serde_json::json!(true));
        assert_eq!(
            serde_json::from_str::<NetworkSettings>("{}").unwrap(),
            NetworkSettings::default()
        );
    }

    #[test]
    fn hex_to_array16_parses_and_rejects() {
        assert_eq!(hex_to_array16("00ff00FF00ff00ff00ff00ff00ff00Aa"), {
            let mut v = [0x00, 0xff].repeat(8);
            v[15] = 0xaa;
            let mut a = [0u8; 16];
            a.copy_from_slice(&v);
            Some(a)
        });
        assert!(hex_to_array16("").is_none());
        assert!(hex_to_array16("00ff00ff00ff00ff00ff00ff00ff00f").is_none());
        assert!(hex_to_array16("zzff00ff00ff00ff00ff00ff00ff00ff").is_none());
        // 32 bytes of multibyte UTF-8: must be None, not a char-boundary panic.
        assert!(hex_to_array16("αααααααααααααααα").is_none());
    }
}
