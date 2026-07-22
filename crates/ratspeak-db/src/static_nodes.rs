//! Bundled static propagation nodes from `nodes.json` (compile-time
//! `include_str!`, no runtime fetch).

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::Deserialize;

pub const DEFAULT_STATIC_PRIORITY: u16 = 1_000;

#[derive(Debug, Clone)]
pub struct StaticPropNode {
    pub hash: [u8; 16],
    pub display_name: String,
    pub region: Option<String>,
    pub role: Option<String>,
    pub priority: u16,
}

/// On-disk shape: `hash` is a 32-char hex string for the 16-byte truncated
/// destination hash.
#[derive(Debug, Deserialize)]
struct RawStaticPropNode {
    hash: String,
    display_name: String,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default = "default_static_priority")]
    priority: u16,
}

const NODES_JSON: &str = include_str!("../nodes.json");

static NODES: OnceLock<Vec<StaticPropNode>> = OnceLock::new();
static HASHES: OnceLock<HashSet<[u8; 16]>> = OnceLock::new();

fn default_static_priority() -> u16 {
    DEFAULT_STATIC_PRIORITY
}

/// Parsed once, lazily. Malformed JSON → warn + empty list.
pub fn load() -> &'static Vec<StaticPropNode> {
    NODES.get_or_init(parse_nodes_json)
}

pub fn hash_set() -> &'static HashSet<[u8; 16]> {
    HASHES.get_or_init(|| load().iter().map(|n| n.hash).collect())
}

pub fn priority_for(hash: &[u8; 16]) -> u16 {
    node_for(hash)
        .map(|node| node.priority)
        .unwrap_or(DEFAULT_STATIC_PRIORITY)
}

pub fn node_for(hash: &[u8; 16]) -> Option<&'static StaticPropNode> {
    load().iter().find(|node| &node.hash == hash)
}

fn parse_nodes_json() -> Vec<StaticPropNode> {
    let raw: Vec<RawStaticPropNode> = match serde_json::from_str(NODES_JSON) {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(
                reason = "parse_failed",
                "static nodes.json failed to parse; bundled list is empty for this session"
            );
            return Vec::new();
        }
    };

    raw.into_iter()
        .filter_map(|r| {
            let bytes = match hex::decode(&r.hash) {
                Ok(b) => b,
                Err(_) => {
                    tracing::warn!(
                        reason = "invalid_hex",
                        "static node hash is not valid hex; skipping"
                    );
                    return None;
                }
            };
            if bytes.len() != 16 {
                tracing::warn!(
                    bytes = bytes.len(),
                    reason = "invalid_length",
                    "static node hash is not 16 bytes; skipping"
                );
                return None;
            }
            let mut hash = [0u8; 16];
            hash.copy_from_slice(&bytes);
            Some(StaticPropNode {
                hash,
                display_name: r.display_name,
                region: r.region,
                role: r.role,
                priority: r.priority,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_json_parses_at_compile_time() {
        let _ = parse_nodes_json();
    }

    #[test]
    fn bundled_nodes_include_sync_hub_with_top_priority() {
        let nodes = load();
        assert_eq!(nodes.len(), 10);
        let sync_hub = nodes
            .iter()
            .find(|n| hex::encode(n.hash) == "deadbeefbadfceeae39c1aceb911e205")
            .expect("sync hub present");
        assert_eq!(sync_hub.role.as_deref(), Some("sync_hub"));
        assert_eq!(sync_hub.priority, 0);
        assert_eq!(hash_set().len(), 10);
        assert_eq!(priority_for(&sync_hub.hash), 0);
    }

    #[test]
    fn malformed_hash_skipped() {
        let raw = r#"[{"hash":"not-hex","display_name":"Bad"}]"#;
        let parsed: Vec<RawStaticPropNode> = serde_json::from_str(raw).unwrap();
        let nodes: Vec<StaticPropNode> = parsed
            .into_iter()
            .filter_map(|r| {
                let bytes = hex::decode(&r.hash).ok()?;
                if bytes.len() != 16 {
                    return None;
                }
                let mut h = [0u8; 16];
                h.copy_from_slice(&bytes);
                Some(StaticPropNode {
                    hash: h,
                    display_name: r.display_name,
                    region: r.region,
                    role: r.role,
                    priority: r.priority,
                })
            })
            .collect();
        assert!(nodes.is_empty());
    }
}
