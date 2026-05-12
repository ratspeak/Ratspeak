//! Propagation node Off / Auto / Manual mode. Mirrors Python LXMF's singular
//! `outbound_propagation_node`. Selection logic lives in [`auto_select_node`];
//! deterministic dest_hash tie-break avoids announce-flip thrash.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rns_transport::messages::{TransportMessage, TransportQuery, TransportQueryResponse};
use serde_json::json;

use crate::db;
use crate::state::AppState;
use crate::static_nodes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PropagationMode {
    /// Stops active client sync state and blocks new inbox sends/syncs. DB
    /// `propagation_node` is kept dormant for later Auto/Manual use.
    Off,
    /// Hop-count selection from `discovered_propagation_nodes`, optionally
    /// favoring Ratspeak static nodes.
    #[default]
    Auto,
    /// User-pinned hash; Auto logic does not run.
    Manual,
}

impl PropagationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "auto" => Some(Self::Auto),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefreshOutcome {
    Sent { count: usize },
    Throttled,
    Offline,
}

const REFRESH_THROTTLE: Duration = Duration::from_secs(10);

/// 3 sync failures within this window drop the candidate from auto-selection.
const SYNC_FAILURE_WINDOW: Duration = Duration::from_secs(30 * 60);
const SYNC_FAILURE_THRESHOLD: u32 = 3;

const REFRESH_FOLLOWUP_DELAY: Duration = Duration::from_secs(4);
const STATIC_STARTUP_PROBE_BUDGET: usize = 1;
const STATIC_BACKGROUND_PROBE_BUDGET: usize = 1;
const STATIC_AUTO_REFRESH_PROBE_BUDGET: usize = 1;
const STATIC_MANUAL_PROBE_BUDGET: usize = 10;
const STATIC_PROBE_MIN_INTERVAL: f64 = 60.0;
const STATIC_BACKGROUND_INTERVAL: Duration = Duration::from_secs(45);
const STATIC_PROBE_TIMEOUT_GRACE: f64 = 5.0;
const STATIC_FAILURE_BASE_BACKOFF: f64 = 60.0;
const STATIC_FAILURE_MAX_BACKOFF: f64 = 30.0 * 60.0;
const DISCOVERED_REFRESH_BUDGET: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayReadiness {
    Ready,
    Waiting,
    Offline,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayPathState {
    Reachable,
    Offline,
    TransportUnavailable,
}

struct RelayPathSnapshot {
    state: RelayPathState,
    live_paths: HashSet<[u8; 16]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StaticProbeKind {
    Startup,
    Background,
    Manual,
}

/// Defaults to (Auto, true) if no active identity.
pub fn read_settings(state: &AppState) -> (PropagationMode, bool) {
    let identity = match db::get_active_identity(&state.db) {
        Some(id) => id,
        None => return (PropagationMode::Auto, true),
    };
    let mode = identity
        .get("propagation_mode")
        .and_then(|v| v.as_str())
        .and_then(PropagationMode::parse)
        .unwrap_or_default();
    let favor_static = identity
        .get("propagation_auto_favor_static")
        .and_then(|v| v.as_i64())
        .map(|v| v != 0)
        .unwrap_or(true);
    (mode, favor_static)
}

fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn json_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_u64().map(|v| v as f64))
        .or_else(|| value.as_i64().map(|v| v as f64))
}

fn node_state_is_usable(value: &serde_json::Value) -> bool {
    matches!(
        value
            .get("node_state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown"),
        "enabled" | "known"
    )
}

fn relay_path_state_allows_selection(value: &serde_json::Value, now: f64) -> bool {
    let backoff_active = value
        .get("backoff_until")
        .and_then(json_f64)
        .map(|t| t > now)
        .unwrap_or(false);
    if backoff_active {
        return false;
    }

    !matches!(
        value
            .get("path_status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown"),
        "missing" | "failed" | "probing"
    )
}

fn last_seen(value: &serde_json::Value) -> Option<f64> {
    value
        .get("last_seen")
        .and_then(json_f64)
        .filter(|t| *t > 0.0)
}

fn last_seen_is_current(value: &serde_json::Value, now: f64) -> bool {
    last_seen(value)
        .map(|t| now - t < crate::state::PROPAGATION_NODE_TTL_SECS as f64)
        .unwrap_or(false)
}

fn registry_entry_is_static(
    static_set: &std::collections::HashSet<[u8; 16]>,
    hash: &[u8; 16],
    value: &serde_json::Value,
) -> bool {
    static_set.contains(hash)
        || value
            .get("static")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}

fn registry_static_priority(is_static: bool, hash: &[u8; 16]) -> u16 {
    if is_static {
        static_nodes::priority_for(hash)
    } else {
        static_nodes::DEFAULT_STATIC_PRIORITY
    }
}

fn static_entry_is_selectable(value: &serde_json::Value, now: f64) -> bool {
    let status = value
        .get("static_status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    (status == "reachable" || last_seen_is_current(value, now))
        && node_state_is_usable(value)
        && relay_path_state_allows_selection(value, now)
}

fn static_probe_backoff(failures: u64) -> f64 {
    let exponent = failures.saturating_sub(1).min(5) as i32;
    (STATIC_FAILURE_BASE_BACKOFF * 2f64.powi(exponent)).min(STATIC_FAILURE_MAX_BACKOFF)
}

fn static_probe_timeout() -> f64 {
    rns_transport::constants::PATH_REQUEST_TIMEOUT + STATIC_PROBE_TIMEOUT_GRACE
}

fn ensure_static_registry_entry(
    registry: &mut std::collections::HashMap<String, serde_json::Value>,
    node: &crate::static_nodes::StaticPropNode,
) {
    let hash_hex = hex::encode(node.hash);
    registry.entry(hash_hex.clone()).or_insert_with(|| {
        json!({
            "hash": hash_hex,
            "display_name": node.display_name.clone(),
            "hops": null,
            "stamp_cost": null,
            "transfer_limit_kb": null,
            "last_seen": 0.0,
            "node_state": "bootstrap",
            "static_status": "unknown",
            "path_status": "unknown",
            "transaction_status": "unknown",
            "last_probe": null,
            "last_success": null,
            "last_path_success": null,
            "last_deposit_success": null,
            "last_sync_success": null,
            "backoff_until": null,
            "failure_count": 0,
            "last_failure_reason": null,
            "static": true,
            "region": node.region.clone(),
            "role": node.role.clone(),
            "priority": node.priority,
        })
    });
}

fn expire_static_probe_timeouts(state: &AppState, now: f64) {
    let timeout = static_probe_timeout();
    let Ok(mut registry) = state.discovered_propagation_nodes.lock() else {
        return;
    };
    let static_set = static_nodes::hash_set();

    for (hash_hex, value) in registry.iter_mut() {
        let Ok(bytes) = hex::decode(hash_hex) else {
            continue;
        };
        if bytes.len() != 16 {
            continue;
        }
        let mut hash = [0u8; 16];
        hash.copy_from_slice(&bytes);
        if !registry_entry_is_static(static_set, &hash, value) {
            continue;
        }
        if value
            .get("static_status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            != "probing"
        {
            continue;
        }
        let Some(last_probe) = value.get("last_probe").and_then(json_f64) else {
            continue;
        };
        if now - last_probe <= timeout {
            continue;
        }
        let failures = value
            .get("failure_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + 1;
        if let Some(obj) = value.as_object_mut() {
            obj.insert("static_status".to_string(), json!("failed"));
            obj.insert("path_status".to_string(), json!("missing"));
            obj.insert("failure_count".to_string(), json!(failures));
            obj.insert(
                "backoff_until".to_string(),
                json!(now + static_probe_backoff(failures)),
            );
            obj.insert(
                "last_failure_reason".to_string(),
                json!("path_probe_timeout"),
            );
        }
    }
}

fn static_probe_is_in_flight(value: &serde_json::Value, now: f64) -> bool {
    if value
        .get("static_status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        != "probing"
    {
        return false;
    }
    value
        .get("last_probe")
        .and_then(json_f64)
        .map(|t| now - t <= static_probe_timeout())
        .unwrap_or(false)
}

fn static_probe_sort_key(hash: &[u8; 16]) -> (u16, [u8; 16]) {
    (static_nodes::priority_for(hash), *hash)
}

fn select_static_probe_candidates(
    state: &AppState,
    kind: StaticProbeKind,
    now: f64,
) -> Vec<[u8; 16]> {
    let (mode, favor_static) = read_settings(state);
    if mode == PropagationMode::Off {
        return Vec::new();
    }
    if kind != StaticProbeKind::Manual && !favor_static {
        return Vec::new();
    }

    expire_static_probe_timeouts(state, now);

    let auto_favor = mode == PropagationMode::Auto && favor_static;
    let budget = match kind {
        StaticProbeKind::Startup => STATIC_STARTUP_PROBE_BUDGET,
        StaticProbeKind::Background => STATIC_BACKGROUND_PROBE_BUDGET,
        StaticProbeKind::Manual if auto_favor => STATIC_AUTO_REFRESH_PROBE_BUDGET,
        StaticProbeKind::Manual => STATIC_MANUAL_PROBE_BUDGET,
    };
    if budget == 0 {
        return Vec::new();
    }

    let bundle = static_nodes::load();

    let mut candidates = Vec::new();
    if let Ok(mut registry) = state.discovered_propagation_nodes.lock() {
        for node in bundle {
            ensure_static_registry_entry(&mut registry, node);
        }

        let static_set = static_nodes::hash_set();
        let mut static_hashes: Vec<[u8; 16]> = bundle.iter().map(|n| n.hash).collect();
        for (hash_hex, value) in registry.iter() {
            let Ok(bytes) = hex::decode(hash_hex) else {
                continue;
            };
            if bytes.len() != 16 {
                continue;
            }
            let mut hash = [0u8; 16];
            hash.copy_from_slice(&bytes);
            if registry_entry_is_static(static_set, &hash, value) && !static_hashes.contains(&hash)
            {
                static_hashes.push(hash);
            }
        }

        if auto_favor
            && static_hashes.iter().any(|hash| {
                registry
                    .get(&hex::encode(hash))
                    .map(|value| static_probe_is_in_flight(value, now))
                    .unwrap_or(false)
            })
        {
            return Vec::new();
        }

        for hash in static_hashes {
            let hash_hex = hex::encode(hash);
            let Some(value) = registry.get(&hash_hex) else {
                continue;
            };
            if static_entry_is_selectable(value, now) {
                continue;
            }
            let backoff_active = value
                .get("backoff_until")
                .and_then(json_f64)
                .map(|t| t > now)
                .unwrap_or(false);
            let probed_recently = value
                .get("last_probe")
                .and_then(json_f64)
                .map(|t| now - t < STATIC_PROBE_MIN_INTERVAL)
                .unwrap_or(false);
            if probed_recently {
                continue;
            }
            if backoff_active && kind != StaticProbeKind::Manual {
                continue;
            }
            candidates.push(hash);
        }
    }

    candidates.sort_by_key(static_probe_sort_key);
    candidates.truncate(budget);
    candidates
}

fn mark_static_probe_sent(state: &AppState, hashes: &[[u8; 16]], now: f64) {
    if hashes.is_empty() {
        return;
    }
    let Ok(mut registry) = state.discovered_propagation_nodes.lock() else {
        return;
    };

    for hash in hashes {
        let hash_hex = hex::encode(hash);
        let value = registry.entry(hash_hex.clone()).or_insert_with(|| {
            json!({
                "hash": hash_hex,
                "display_name": format!("Ratspeak inbox {}", &hex::encode(hash)[..8]),
                "hops": null,
                "last_seen": 0.0,
                "node_state": "bootstrap",
                "static_status": "unknown",
                "path_status": "unknown",
                "transaction_status": "unknown",
                "static": true,
            })
        });
        if let Some(obj) = value.as_object_mut() {
            obj.insert("static_status".to_string(), json!("probing"));
            obj.insert("path_status".to_string(), json!("probing"));
            obj.insert("last_probe".to_string(), json!(now));
            obj.entry("failure_count".to_string()).or_insert(json!(0));
            obj.insert("static".to_string(), json!(true));
        }
    }
}

pub fn mark_relay_path_success(state: &AppState, hash: [u8; 16]) {
    let static_set = static_nodes::hash_set();
    let is_static = static_set.contains(&hash);
    let now = now_f64();
    let hash_hex = hex::encode(hash);
    if let Ok(mut registry) = state.discovered_propagation_nodes.lock()
        && let Some(value) = registry.get_mut(&hash_hex)
        && let Some(obj) = value.as_object_mut()
    {
        if is_static {
            obj.insert("static_status".to_string(), json!("reachable"));
        }
        obj.insert("path_status".to_string(), json!("reachable"));
        obj.insert("last_success".to_string(), json!(now));
        obj.insert("last_path_success".to_string(), json!(now));
        obj.insert("failure_count".to_string(), json!(0));
        obj.insert("backoff_until".to_string(), serde_json::Value::Null);
        obj.insert("last_failure_reason".to_string(), serde_json::Value::Null);
        if is_static {
            obj.insert("static".to_string(), json!(true));
            let node_state_usable = matches!(
                obj.get("node_state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
                "enabled" | "known"
            );
            let node_state_disabled = obj
                .get("node_state")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s == "disabled");
            if !node_state_usable && !node_state_disabled {
                obj.insert("node_state".to_string(), json!("known"));
            }
            if let Some(node) = static_nodes::node_for(&hash) {
                obj.entry("display_name".to_string())
                    .or_insert_with(|| json!(node.display_name.clone()));
                obj.entry("region".to_string())
                    .or_insert_with(|| json!(node.region.clone()));
                obj.entry("role".to_string())
                    .or_insert_with(|| json!(node.role.clone()));
                obj.entry("priority".to_string())
                    .or_insert(json!(node.priority));
            }
        }
    }
}

pub fn mark_static_probe_success(state: &AppState, hash: [u8; 16]) {
    mark_relay_path_success(state, hash);
}

pub fn mark_relay_transaction_success(state: &AppState, hash: [u8; 16], kind: &str) {
    let static_set = static_nodes::hash_set();
    let is_static = static_set.contains(&hash);
    let now = now_f64();
    let hash_hex = hex::encode(hash);
    if let Ok(mut registry) = state.discovered_propagation_nodes.lock()
        && let Some(value) = registry.get_mut(&hash_hex)
        && let Some(obj) = value.as_object_mut()
    {
        if is_static {
            obj.insert("static_status".to_string(), json!("reachable"));
            obj.insert("static".to_string(), json!(true));
        }
        obj.insert("path_status".to_string(), json!("reachable"));
        obj.insert("transaction_status".to_string(), json!(kind));
        obj.insert("last_success".to_string(), json!(now));
        match kind {
            "deposit_ok" => obj.insert("last_deposit_success".to_string(), json!(now)),
            "sync_ok" => obj.insert("last_sync_success".to_string(), json!(now)),
            _ => None,
        };
        obj.insert("failure_count".to_string(), json!(0));
        obj.insert("backoff_until".to_string(), serde_json::Value::Null);
        obj.insert("last_failure_reason".to_string(), serde_json::Value::Null);
    }
}

pub fn mark_relay_failure(state: &AppState, hash: [u8; 16], reason: &str) {
    let static_set = static_nodes::hash_set();
    let is_static = static_set.contains(&hash);
    let now = now_f64();
    let hash_hex = hex::encode(hash);
    if let Ok(mut registry) = state.discovered_propagation_nodes.lock()
        && let Some(value) = registry.get_mut(&hash_hex)
    {
        let failures = value
            .get("failure_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + 1;
        if let Some(obj) = value.as_object_mut() {
            if is_static {
                obj.insert("static_status".to_string(), json!("failed"));
                obj.insert("static".to_string(), json!(true));
            }
            obj.insert("path_status".to_string(), json!("failed"));
            obj.insert("transaction_status".to_string(), json!("failed"));
            obj.insert("failure_count".to_string(), json!(failures));
            obj.insert(
                "backoff_until".to_string(),
                json!(now + static_probe_backoff(failures)),
            );
            obj.insert("last_failure_reason".to_string(), json!(reason));
        }
    }
}

/// Pure ranking: (favor_static && is_static) DESC, static priority ASC,
/// hops ASC, dest_hash ASC.
/// Recency is intentionally not a tie-break — would flip on every announce
/// and tear down the active link.
pub fn auto_select_node(state: &AppState) -> Option<[u8; 16]> {
    let (_, favor_static) = read_settings(state);
    let static_set = static_nodes::hash_set();
    let now = now_f64();

    let mut candidates: Vec<(bool, u16, u8, [u8; 16])> = {
        let nodes = state.discovered_propagation_nodes.lock().ok()?;
        nodes
            .iter()
            .filter_map(|(hash_hex, value)| {
                let bytes = hex::decode(hash_hex).ok()?;
                if bytes.len() != 16 {
                    return None;
                }
                let mut h = [0u8; 16];
                h.copy_from_slice(&bytes);
                let is_static = registry_entry_is_static(static_set, &h, value);

                if is_static {
                    if !static_entry_is_selectable(value, now) {
                        return None;
                    }
                } else {
                    // Never auto-pick a node we couldn't parse PN metadata for.
                    if !node_state_is_usable(value)
                        || !last_seen_is_current(value, now)
                        || !relay_path_state_allows_selection(value, now)
                    {
                        return None;
                    }
                }
                let hops = value
                    .get("hops")
                    .and_then(|v| v.as_u64())
                    .map(|h| h.min(255) as u8)
                    .unwrap_or(255);
                let priority = registry_static_priority(favor_static && is_static, &h);
                Some((is_static, priority, hops, h))
            })
            .collect()
    };

    if let Ok(failures) = state.auto_failure_counts.lock() {
        candidates.retain(|(_, _, _, h)| match failures.get(h) {
            Some((count, when)) if *count >= SYNC_FAILURE_THRESHOLD => {
                when.elapsed() >= SYNC_FAILURE_WINDOW
            }
            _ => true,
        });
    }

    candidates.sort_by(|a, b| {
        let a_pref = favor_static && a.0;
        let b_pref = favor_static && b.0;
        match b_pref.cmp(&a_pref) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
        match a.1.cmp(&b.1) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
        match a.2.cmp(&b.2) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
        a.3.cmp(&b.3)
    });

    candidates.first().map(|(_, _, _, h)| *h)
}

async fn transport_query(
    state: &AppState,
    query: TransportQuery,
) -> Option<TransportQueryResponse> {
    let handle = state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.clone()))?;
    handle.query_control(query).await
}

async fn relay_path_snapshot(state: &AppState) -> RelayPathSnapshot {
    let now = now_f64();
    let Some(TransportQueryResponse::PathTable(paths)) =
        transport_query(state, TransportQuery::GetPathTable).await
    else {
        return RelayPathSnapshot {
            state: RelayPathState::TransportUnavailable,
            live_paths: HashSet::new(),
        };
    };

    let interface_online: Option<std::collections::HashMap<String, bool>> =
        match transport_query(state, TransportQuery::GetInterfaceStats).await {
            Some(TransportQueryResponse::InterfaceStats(stats)) => {
                if stats.is_empty() {
                    None
                } else {
                    Some(stats.into_iter().map(|s| (s.name, s.online)).collect())
                }
            }
            _ => None,
        };
    let any_interface_online = interface_online
        .as_ref()
        .map(|m| m.values().any(|online| *online))
        .unwrap_or(true);
    if !any_interface_online {
        return RelayPathSnapshot {
            state: RelayPathState::Offline,
            live_paths: HashSet::new(),
        };
    }

    let live_paths = paths
        .into_iter()
        .filter(|entry| entry.expires > now)
        .filter(|entry| {
            interface_online
                .as_ref()
                .and_then(|m| m.get(&entry.interface))
                .copied()
                .unwrap_or(true)
        })
        .map(|entry| entry.hash)
        .collect::<HashSet<_>>();

    RelayPathSnapshot {
        state: RelayPathState::Reachable,
        live_paths,
    }
}

fn auto_select_node_with_live_paths(
    state: &AppState,
    live_paths: &HashSet<[u8; 16]>,
) -> Option<[u8; 16]> {
    let (_, favor_static) = read_settings(state);
    let static_set = static_nodes::hash_set();
    let now = now_f64();

    let mut candidates: Vec<(bool, u16, u8, [u8; 16])> = {
        let nodes = state.discovered_propagation_nodes.lock().ok()?;
        nodes
            .iter()
            .filter_map(|(hash_hex, value)| {
                let bytes = hex::decode(hash_hex).ok()?;
                if bytes.len() != 16 {
                    return None;
                }
                let mut h = [0u8; 16];
                h.copy_from_slice(&bytes);
                if !live_paths.contains(&h) {
                    return None;
                }
                let is_static = registry_entry_is_static(static_set, &h, value);

                if is_static {
                    if !static_entry_is_selectable(value, now) {
                        return None;
                    }
                } else if !node_state_is_usable(value)
                    || !last_seen_is_current(value, now)
                    || !relay_path_state_allows_selection(value, now)
                {
                    return None;
                }

                let hops = value
                    .get("hops")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(255)
                    .min(255) as u8;
                let priority = registry_static_priority(favor_static && is_static, &h);
                Some((favor_static && is_static, priority, hops, h))
            })
            .collect()
    };

    if let Ok(map) = state.auto_failure_counts.lock() {
        let now = Instant::now();
        candidates.retain(|(_, _, _, h)| {
            !map.get(h).is_some_and(|(count, first)| {
                *count >= SYNC_FAILURE_THRESHOLD
                    && now.duration_since(*first) <= SYNC_FAILURE_WINDOW
            })
        });
    }

    candidates.sort_by(|a, b| {
        match b.0.cmp(&a.0) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
        match a.1.cmp(&b.1) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
        match a.2.cmp(&b.2) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
        a.3.cmp(&b.3)
    });

    candidates.first().map(|(_, _, _, h)| *h)
}

async fn request_relay_path(state: &Arc<AppState>, hash: [u8; 16]) {
    let transport_tx = state
        .rns
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|mgr| mgr.handle.transport_tx.clone()));
    if let Some(tx) = transport_tx {
        let _ = tx
            .send(TransportMessage::RequestPath {
                destination_hash: hash,
            })
            .await;
    }
}

fn relay_send_metadata_ready(state: &AppState, hash: &[u8; 16]) -> bool {
    state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| {
            lxmf.as_ref()
                .map(|mgr| mgr.propagation_node_ready_for_send(hash))
        })
        .unwrap_or(false)
}

async fn relay_send_ready_or_waiting(state: &Arc<AppState>, hash: [u8; 16]) -> RelayReadiness {
    if relay_send_metadata_ready(state, &hash) {
        RelayReadiness::Ready
    } else {
        request_relay_path(state, hash).await;
        tracing::info!(
            node = %hex::encode(hash),
            "Offline Inbox path is reachable, waiting for LXMF propagation identity/stamp metadata"
        );
        RelayReadiness::Waiting
    }
}

/// Re-run selection after a propagation announce; switch only if Auto mode
/// and the winner changed.
pub async fn maybe_reselect_on_announce(state: &Arc<AppState>) {
    let (mode, _) = read_settings(state);
    if mode != PropagationMode::Auto {
        return;
    }
    let new_winner = match auto_select_node(state) {
        Some(h) => h,
        None => return,
    };
    let current = state.auto_active_node.read().ok().and_then(|g| *g);
    if current == Some(new_winner) {
        return;
    }
    apply_auto_selection(state, new_winner).await;
}

/// Persist a winner: set node on LXMF mgr, update `auto_active_node`,
/// emit `propagation_update`.
pub async fn apply_auto_selection(state: &Arc<AppState>, hash: [u8; 16]) {
    let identity_id = crate::helpers::active_identity_id(state);
    let hex_hash = hex::encode(hash);

    let st = state.clone();
    let id = identity_id.clone();
    let hex_for_set = hex_hash.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(mut lxmf) = st.lxmf.lock()
            && let Some(mgr) = lxmf.as_mut()
        {
            mgr.set_propagation_node(Some(&hex_for_set), &st.db, &id);
        }
    })
    .await;

    if let Ok(mut slot) = state.auto_active_node.write() {
        *slot = Some(hash);
    }

    emit_propagation_update(state);
    tracing::info!(
        node = %hex_hash,
        "auto-selected propagation node"
    );
}

/// Clear the active Auto-selected relay without changing Off/Manual semantics.
pub async fn clear_auto_selection(state: &Arc<AppState>) {
    let identity_id = crate::helpers::active_identity_id(state);
    let st = state.clone();
    let id = identity_id.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(mut lxmf) = st.lxmf.lock()
            && let Some(mgr) = lxmf.as_mut()
        {
            mgr.set_propagation_node(None, &st.db, &id);
        }
    })
    .await;

    if let Ok(mut slot) = state.auto_active_node.write() {
        *slot = None;
    }

    emit_propagation_update(state);
}

fn configured_client_relay(state: &AppState) -> Option<[u8; 16]> {
    state.lxmf.lock().ok().and_then(|lxmf| {
        lxmf.as_ref()
            .and_then(|mgr| mgr.configured_propagation_node)
    })
}

fn promote_static_live_paths(state: &AppState, live_paths: &HashSet<[u8; 16]>) {
    if live_paths.is_empty() {
        return;
    }
    let static_set = static_nodes::hash_set();
    for hash in live_paths {
        if static_set.contains(hash) {
            mark_relay_path_success(state, *hash);
        }
    }
}

async fn apply_best_live_auto_selection(
    state: &Arc<AppState>,
    live_paths: &HashSet<[u8; 16]>,
) -> Option<[u8; 16]> {
    promote_static_live_paths(state, live_paths);
    let winner = auto_select_node_with_live_paths(state, live_paths)?;
    apply_auto_selection(state, winner).await;
    Some(winner)
}

async fn reselect_from_live_paths_after_probe(state: &Arc<AppState>) {
    let (mode, _) = read_settings(state);
    if mode != PropagationMode::Auto {
        emit_propagation_update(state);
        return;
    }

    let snapshot = relay_path_snapshot(state).await;
    if snapshot.state != RelayPathState::Reachable {
        emit_propagation_update(state);
        return;
    }

    promote_static_live_paths(state, &snapshot.live_paths);
    let current = state.auto_active_node.read().ok().and_then(|g| *g);
    if let Some(winner) = auto_select_node_with_live_paths(state, &snapshot.live_paths)
        && current != Some(winner)
    {
        apply_auto_selection(state, winner).await;
        return;
    }

    emit_propagation_update(state);
}

pub async fn ensure_relay_ready_for_send(state: &Arc<AppState>) -> RelayReadiness {
    let (mode, _) = read_settings(state);
    match mode {
        PropagationMode::Off => RelayReadiness::Unavailable,
        PropagationMode::Manual => {
            let Some(node) = configured_client_relay(state) else {
                return RelayReadiness::Unavailable;
            };
            let snapshot = relay_path_snapshot(state).await;
            match snapshot.state {
                RelayPathState::Offline => RelayReadiness::Offline,
                RelayPathState::TransportUnavailable => RelayReadiness::Waiting,
                _ if snapshot.live_paths.contains(&node) => {
                    relay_send_ready_or_waiting(state, node).await
                }
                _ => {
                    request_relay_path(state, node).await;
                    RelayReadiness::Waiting
                }
            }
        }
        PropagationMode::Auto => {
            let snapshot = relay_path_snapshot(state).await;
            match snapshot.state {
                RelayPathState::Offline => return RelayReadiness::Offline,
                RelayPathState::TransportUnavailable => return RelayReadiness::Waiting,
                RelayPathState::Reachable => {}
            }
            promote_static_live_paths(state, &snapshot.live_paths);

            let active = state.auto_active_node.read().ok().and_then(|g| *g);
            if let Some(winner) = auto_select_node_with_live_paths(state, &snapshot.live_paths)
                && active != Some(winner)
            {
                apply_auto_selection(state, winner).await;
                mark_relay_path_success(state, winner);
                return relay_send_ready_or_waiting(state, winner).await;
            }

            if let Some(active) = active {
                if snapshot.live_paths.contains(&active) {
                    mark_relay_path_success(state, active);
                    return relay_send_ready_or_waiting(state, active).await;
                }
                mark_relay_failure(state, active, "active_relay_path_missing");
                request_relay_path(state, active).await;
            }

            if let Some(winner) = apply_best_live_auto_selection(state, &snapshot.live_paths).await
            {
                mark_relay_path_success(state, winner);
                relay_send_ready_or_waiting(state, winner).await
            } else {
                clear_auto_selection(state).await;
                RelayReadiness::Waiting
            }
        }
    }
}

pub async fn reconcile_active_auto_node(state: &Arc<AppState>) {
    let (mode, _) = read_settings(state);
    if mode != PropagationMode::Auto {
        return;
    }

    let Some(active) = state.auto_active_node.read().ok().and_then(|g| *g) else {
        return;
    };

    let snapshot = relay_path_snapshot(state).await;
    match snapshot.state {
        RelayPathState::Offline | RelayPathState::TransportUnavailable => return,
        RelayPathState::Reachable => {}
    }
    promote_static_live_paths(state, &snapshot.live_paths);

    if let Some(winner) = auto_select_node_with_live_paths(state, &snapshot.live_paths)
        && active != winner
    {
        apply_auto_selection(state, winner).await;
        return;
    }

    if snapshot.live_paths.contains(&active) {
        mark_relay_path_success(state, active);
        return;
    }

    mark_relay_failure(state, active, "active_relay_path_missing");
    request_relay_path(state, active).await;

    if apply_best_live_auto_selection(state, &snapshot.live_paths)
        .await
        .is_none()
    {
        clear_auto_selection(state).await;
    }
}

/// Issue `request_path` for every known propagation-node candidate
/// (statics ∪ discovered). `ignore_throttle = true` is the startup case.
pub async fn refresh_paths(state: &Arc<AppState>, ignore_throttle: bool) -> RefreshOutcome {
    let (mode, favor_static) = read_settings(state);
    if mode == PropagationMode::Off {
        return RefreshOutcome::Sent { count: 0 };
    }

    if !ignore_throttle && let Ok(mut last) = state.last_refresh_request_at.lock() {
        let now = Instant::now();
        if let Some(prev) = *last
            && now.duration_since(prev) < REFRESH_THROTTLE
        {
            return RefreshOutcome::Throttled;
        }
        *last = Some(now);
    }

    let transport_tx = state
        .rns
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|mgr| mgr.handle.transport_tx.clone()));
    let Some(tx) = transport_tx else {
        return RefreshOutcome::Offline;
    };

    let now = now_f64();
    let static_kind = if ignore_throttle {
        StaticProbeKind::Startup
    } else {
        StaticProbeKind::Manual
    };
    let mut candidates: Vec<[u8; 16]> = select_static_probe_candidates(state, static_kind, now);
    let static_candidates = candidates.clone();

    if !ignore_throttle
        && !(mode == PropagationMode::Auto && favor_static)
        && let Ok(reg) = state.discovered_propagation_nodes.lock()
    {
        let static_set = static_nodes::hash_set();
        let mut discovered = Vec::new();
        for hash_hex in reg.keys() {
            if let Ok(bytes) = hex::decode(hash_hex)
                && bytes.len() == 16
            {
                let mut h = [0u8; 16];
                h.copy_from_slice(&bytes);
                let Some(value) = reg.get(hash_hex) else {
                    continue;
                };
                if registry_entry_is_static(static_set, &h, value) {
                    continue;
                }
                if node_state_is_usable(value) && last_seen_is_current(value, now) {
                    discovered.push(h);
                }
            }
        }
        discovered.sort();
        discovered.truncate(DISCOVERED_REFRESH_BUDGET);
        for h in discovered {
            if !candidates.contains(&h) {
                candidates.push(h);
            }
        }
    }
    let count = candidates.len();
    if count == 0 {
        return RefreshOutcome::Sent { count: 0 };
    }

    mark_static_probe_sent(state, &static_candidates, now);

    state.emit_to_all("propagation_refresh_started", json!({ "count": count }));

    for h in &candidates {
        let _ = tx
            .send(TransportMessage::RequestPath {
                destination_hash: *h,
            })
            .await;
    }

    let st = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(REFRESH_FOLLOWUP_DELAY).await;
        reselect_from_live_paths_after_probe(&st).await;
    });

    RefreshOutcome::Sent { count }
}

/// Low-rate Auto-mode background probing for bundled Ratspeak relays.
pub async fn probe_static_nodes_background(state: &Arc<AppState>) {
    let (mode, favor_static) = read_settings(state);
    if mode != PropagationMode::Auto || !favor_static || static_nodes::load().is_empty() {
        return;
    }

    if let Ok(current) = state.auto_active_node.read()
        && let Some(hash) = *current
        && static_nodes::hash_set().contains(&hash)
    {
        return;
    }

    if let Ok(mut last) = state.last_static_probe_at.lock() {
        let now = Instant::now();
        if let Some(prev) = *last
            && now.duration_since(prev) < STATIC_BACKGROUND_INTERVAL
        {
            return;
        }
        *last = Some(now);
    }

    let transport_tx = state
        .rns
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|mgr| mgr.handle.transport_tx.clone()));
    let Some(tx) = transport_tx else {
        return;
    };

    let now = now_f64();
    let candidates = select_static_probe_candidates(state, StaticProbeKind::Background, now);
    if candidates.is_empty() {
        return;
    }
    mark_static_probe_sent(state, &candidates, now);

    for h in candidates {
        let _ = tx
            .send(TransportMessage::RequestPath {
                destination_hash: h,
            })
            .await;
    }

    let st = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(REFRESH_FOLLOWUP_DELAY).await;
        reselect_from_live_paths_after_probe(&st).await;
    });
}

/// Reactive `request_path` for the active node + 3-strikes/30-min counter.
/// Manual mode: never switches.
pub async fn handle_sync_failure(state: &Arc<AppState>) {
    let (mode, _) = read_settings(state);
    let active = state.auto_active_node.read().ok().and_then(|g| *g);

    let Some(node) = active else {
        return;
    };

    if mode != PropagationMode::Auto {
        return;
    }

    let mut hit_threshold = false;
    if let Ok(mut map) = state.auto_failure_counts.lock() {
        let now = Instant::now();
        let entry = map.entry(node).or_insert((0, now));
        if entry.1.elapsed() > SYNC_FAILURE_WINDOW {
            *entry = (1, now);
        } else {
            entry.0 += 1;
            entry.1 = now;
        }
        if entry.0 >= SYNC_FAILURE_THRESHOLD {
            hit_threshold = true;
        }
    }

    if let Some(tx) = state
        .rns
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|mgr| mgr.handle.transport_tx.clone()))
    {
        let _ = tx
            .send(TransportMessage::RequestPath {
                destination_hash: node,
            })
            .await;
    }

    if hit_threshold {
        mark_relay_failure(state, node, "sync_failure_threshold");
        tracing::warn!(
            node = %hex::encode(node),
            "propagation node hit 3 failures within 30 min — dropping from auto-selection"
        );
        match auto_select_node(state) {
            Some(w) if w != node => apply_auto_selection(state, w).await,
            _ => clear_auto_selection(state).await,
        }
    }
}

pub fn persist_settings(
    state: &AppState,
    mode: PropagationMode,
    favor_static: Option<bool>,
) -> (PropagationMode, bool) {
    let identity_id = crate::helpers::active_identity_id(state);
    let current_favor = read_settings(state).1;
    let favor = favor_static.unwrap_or(current_favor);

    if let Ok(conn) = state.db.get() {
        if favor_static.is_some() {
            conn.execute(
                "UPDATE identities
                    SET propagation_mode = ?1,
                        propagation_auto_favor_static = ?2
                    WHERE hash = ?3",
                rusqlite::params![mode.as_str(), if favor { 1 } else { 0 }, identity_id],
            )
            .ok();
        } else {
            conn.execute(
                "UPDATE identities
                    SET propagation_mode = ?1
                    WHERE hash = ?2",
                rusqlite::params![mode.as_str(), identity_id],
            )
            .ok();
        }
    }
    (mode, favor)
}

pub fn emit_propagation_update(state: &AppState) {
    let status = get_status_payload(state);
    state.emit_to_all("propagation_update", status);
}

/// LXMF manager's view + Ratspeak fields: mode, favor_static,
/// auto_active_node, awaiting_discovery, static_nodes_known, pn_parse_failures.
pub fn get_status_payload(state: &AppState) -> serde_json::Value {
    let (mode, favor_static) = read_settings(state);
    let lxmf_status = if let Ok(lxmf) = state.lxmf.lock() {
        lxmf.as_ref().map(|mgr| mgr.get_propagation_status())
    } else {
        None
    }
    .unwrap_or_else(|| {
        json!({
            "enabled": false,
            "node_hash": null,
            "connected": false,
            "message_count": 0,
        })
    });

    let auto_active = state
        .auto_active_node
        .read()
        .ok()
        .and_then(|g| *g)
        .map(hex::encode);
    let static_nodes_known = static_nodes::load().len();
    let awaiting_discovery =
        mode == PropagationMode::Auto && auto_active.is_none() && auto_select_node(state).is_none();
    let pn_parse_failures = state
        .pn_parse_failures
        .load(std::sync::atomic::Ordering::Relaxed);
    let (local_node_hash, local_node_message_count, local_node_stamp_cost) = {
        let hash = if let Ok(lxmf) = state.lxmf.lock() {
            lxmf.as_ref()
                .map(|mgr| hex::encode(mgr.propagation_dest_hash))
        } else {
            None
        };
        let (count, cost) = if let Ok(slot) = state.propagation_node.lock()
            && let Some(node) = slot.as_ref()
            && let Ok(node) = node.lock()
        {
            (node.message_count(), node.min_stamp_cost())
        } else {
            (
                0,
                state
                    .propagation_node_stamp_cost
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
        };
        (hash, count, cost)
    };
    let hosting_enabled = state
        .propagation_node_hosting_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let enforce_stamps = state
        .enforce_stamps
        .load(std::sync::atomic::Ordering::Relaxed);
    let required_stamp_cost = state
        .required_stamp_cost
        .load(std::sync::atomic::Ordering::Relaxed);

    let mut out = lxmf_status;
    if let Some(obj) = out.as_object_mut() {
        if mode == PropagationMode::Off || mode == PropagationMode::Auto {
            obj.insert("node_hash".to_string(), json!(auto_active.clone()));
            obj.insert("propagation_node".to_string(), json!(auto_active.clone()));
        }
        obj.insert("mode".to_string(), json!(mode.as_str()));
        obj.insert("favor_static".to_string(), json!(favor_static));
        obj.insert("auto_active_node".to_string(), json!(auto_active));
        obj.insert("awaiting_discovery".to_string(), json!(awaiting_discovery));
        obj.insert("static_nodes_known".to_string(), json!(static_nodes_known));
        obj.insert("pn_parse_failures".to_string(), json!(pn_parse_failures));
        obj.insert("hosting_enabled".to_string(), json!(hosting_enabled));
        obj.insert("local_node_hash".to_string(), json!(local_node_hash));
        obj.insert(
            "local_node_message_count".to_string(),
            json!(local_node_message_count),
        );
        obj.insert(
            "local_node_stamp_cost".to_string(),
            json!(local_node_stamp_cost),
        );
        obj.insert("enforce_stamps".to_string(), json!(enforce_stamps));
        obj.insert(
            "required_stamp_cost".to_string(),
            json!(required_stamp_cost),
        );
    }
    out
}

/// Seed `discovered_propagation_nodes` from the static bundle so Auto mode
/// has candidates before any announce arrives.
pub fn seed_static_nodes(state: &AppState) {
    let bundle = static_nodes::load();
    if bundle.is_empty() {
        return;
    }
    if let Ok(mut registry) = state.discovered_propagation_nodes.lock() {
        for n in bundle {
            ensure_static_registry_entry(&mut registry, n);
        }
        tracing::info!(
            count = bundle.len(),
            "seeded discovered_propagation_nodes from static bundle"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DashboardConfig;
    use r2d2_sqlite::SqliteConnectionManager;
    use rns_identity::identity::Identity;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_STATE_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_state() -> Arc<AppState> {
        let unique = TEMP_STATE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-prop-test-{}-{}-{unique}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let config = DashboardConfig::from_env_and_defaults(tmp);
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
        crate::db::init_schema(&pool).unwrap();
        Arc::new(AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ))
    }

    fn seed_node(state: &AppState, hash: [u8; 16], hops: u8, last_seen: f64) {
        seed_node_with_state(state, hash, hops, last_seen, "enabled");
    }

    fn set_active_identity_settings(state: &AppState, mode: &str, favor_static: bool) {
        crate::db::save_identity(
            &state.db,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "test",
            "Test",
        );
        crate::db::set_active_identity(&state.db, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let conn = state.db.get().unwrap();
        conn.execute(
            "UPDATE identities
                SET propagation_mode = ?1,
                    propagation_auto_favor_static = ?2
              WHERE hash = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'",
            rusqlite::params![mode, if favor_static { 1 } else { 0 }],
        )
        .unwrap();
    }

    fn seed_node_with_state(
        state: &AppState,
        hash: [u8; 16],
        hops: u8,
        last_seen: f64,
        node_state: &str,
    ) {
        let hash_hex = hex::encode(hash);
        let entry = json!({
            "hash": hash_hex,
            "display_name": format!("Test {}", &hex::encode(hash)[..6]),
            "hops": hops,
            "last_seen": last_seen,
            "node_state": node_state,
        });
        let mut map = state.discovered_propagation_nodes.lock().unwrap();
        map.insert(hash_hex, entry);
    }

    fn seed_static_node(
        state: &AppState,
        hash: [u8; 16],
        hops: Option<u8>,
        last_seen: f64,
        node_state: &str,
        static_status: &str,
    ) {
        let hash_hex = hex::encode(hash);
        let entry = json!({
            "hash": hash_hex,
            "display_name": format!("Static {}", &hex::encode(hash)[..6]),
            "hops": hops,
            "last_seen": last_seen,
            "node_state": node_state,
            "static_status": static_status,
            "static": true,
            "failure_count": 0,
        });
        let mut map = state.discovered_propagation_nodes.lock().unwrap();
        map.insert(hash_hex, entry);
    }

    fn set_node_path_status(state: &AppState, hash: [u8; 16], status: &str) {
        let hash_hex = hex::encode(hash);
        let mut map = state.discovered_propagation_nodes.lock().unwrap();
        let value = map.get_mut(&hash_hex).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("path_status".to_string(), json!(status));
    }

    fn sync_hub_hash() -> [u8; 16] {
        let bytes = hex::decode("deadbeefbadfceeae39c1aceb911e205").unwrap();
        let mut hash = [0u8; 16];
        hash.copy_from_slice(&bytes);
        hash
    }

    fn ratspeak_node_1_hash() -> [u8; 16] {
        let bytes = hex::decode("111111111111425117677c92c1693b92").unwrap();
        let mut hash = [0u8; 16];
        hash.copy_from_slice(&bytes);
        hash
    }

    #[test]
    fn parses_known_modes() {
        assert_eq!(PropagationMode::parse("off"), Some(PropagationMode::Off));
        assert_eq!(PropagationMode::parse("auto"), Some(PropagationMode::Auto));
        assert_eq!(
            PropagationMode::parse("manual"),
            Some(PropagationMode::Manual)
        );
        assert_eq!(PropagationMode::parse("garbage"), None);
        assert_eq!(PropagationMode::default(), PropagationMode::Auto);
    }

    #[test]
    fn empty_registry_returns_none() {
        let state = make_state();
        assert!(auto_select_node(&state).is_none());
    }

    #[test]
    fn lowest_hops_wins_no_static() {
        let state = make_state();
        let now = now_f64();
        seed_node(&state, [0xAA; 16], 5, now);
        seed_node(&state, [0xBB; 16], 2, now);
        seed_node(&state, [0xCC; 16], 3, now);
        assert_eq!(auto_select_node(&state), Some([0xBB; 16]));
    }

    #[test]
    fn equal_hops_tie_breaks_on_dest_hash_not_recency() {
        let state = make_state();
        let now = now_f64();
        seed_node(&state, [0xAA; 16], 2, now - 60.0);
        seed_node(&state, [0xBB; 16], 2, now);
        assert_eq!(auto_select_node(&state), Some([0xAA; 16]));
    }

    #[test]
    fn failure_locked_node_drops_from_candidacy() {
        let state = make_state();
        let now = now_f64();
        seed_node(&state, [0xAA; 16], 1, now);
        seed_node(&state, [0xBB; 16], 5, now);
        let mut map = state.auto_failure_counts.lock().unwrap();
        map.insert([0xAA; 16], (SYNC_FAILURE_THRESHOLD, Instant::now()));
        drop(map);
        assert_eq!(auto_select_node(&state), Some([0xBB; 16]));
    }

    #[test]
    fn discovered_with_no_last_seen_skipped_unless_static() {
        let state = make_state();
        let now = now_f64();
        seed_node(&state, [0xAA; 16], 1, 0.0);
        seed_node(&state, [0xBB; 16], 5, now);
        assert_eq!(auto_select_node(&state), Some([0xBB; 16]));
    }

    #[test]
    fn static_bootstrap_node_is_not_selected_until_reachable() {
        let state = make_state();
        let now = now_f64();
        seed_static_node(&state, [0xAA; 16], None, 0.0, "bootstrap", "unknown");
        seed_node(&state, [0xBB; 16], 4, now);

        assert_eq!(
            auto_select_node(&state),
            Some([0xBB; 16]),
            "bootstrap-only static nodes must not block reachable fallback"
        );
    }

    #[test]
    fn favor_static_prefers_reachable_static_over_lower_hop_nonstatic() {
        let state = make_state();
        let now = now_f64();
        seed_static_node(&state, [0xAA; 16], Some(8), now, "enabled", "reachable");
        seed_node(&state, [0xBB; 16], 1, now);

        assert_eq!(auto_select_node(&state), Some([0xAA; 16]));
    }

    #[test]
    fn favor_static_prefers_sync_hub_over_lower_hop_static_node() {
        let state = make_state();
        let now = now_f64();
        let sync_hub = sync_hub_hash();
        let regional = ratspeak_node_1_hash();
        seed_static_node(&state, sync_hub, Some(9), now, "enabled", "reachable");
        seed_static_node(&state, regional, Some(1), now, "enabled", "reachable");

        assert_eq!(
            auto_select_node(&state),
            Some(sync_hub),
            "Ratspeak static priority should put the sync hub first when reachable"
        );
    }

    #[test]
    fn favor_static_falls_back_to_reachable_regional_node_when_sync_hub_path_missing() {
        let state = make_state();
        let now = now_f64();
        let sync_hub = sync_hub_hash();
        let regional = ratspeak_node_1_hash();
        seed_static_node(&state, sync_hub, Some(1), now, "enabled", "reachable");
        seed_static_node(&state, regional, Some(5), now, "enabled", "reachable");
        set_node_path_status(&state, sync_hub, "missing");

        assert_eq!(auto_select_node(&state), Some(regional));
    }

    #[test]
    fn missing_path_status_blocks_auto_selection_and_falls_back() {
        let state = make_state();
        let now = now_f64();
        seed_static_node(&state, [0xAA; 16], Some(1), now, "enabled", "reachable");
        seed_node(&state, [0xBB; 16], 5, now);
        set_node_path_status(&state, [0xAA; 16], "missing");

        assert_eq!(
            auto_select_node(&state),
            Some([0xBB; 16]),
            "a previously selected relay with a missing path must not keep winning"
        );
    }

    #[test]
    fn live_path_filter_prevents_fallback_to_pathless_relay() {
        let state = make_state();
        let now = now_f64();
        seed_static_node(&state, [0xAA; 16], Some(8), now, "enabled", "reachable");
        seed_node(&state, [0xBB; 16], 1, now);

        let mut live = HashSet::new();
        live.insert([0xBB; 16]);

        assert_eq!(
            auto_select_node_with_live_paths(&state, &live),
            Some([0xBB; 16]),
            "static preference must not override live path evidence"
        );
    }

    #[test]
    fn live_path_promotes_static_bootstrap_node_for_auto_selection() {
        let state = make_state();
        let now = now_f64();
        let sync_hub = sync_hub_hash();
        seed_static_node(&state, sync_hub, None, 0.0, "bootstrap", "unknown");
        seed_node(&state, [0xBB; 16], 1, now);

        let mut live = HashSet::new();
        live.insert(sync_hub);
        live.insert([0xBB; 16]);

        assert_eq!(
            auto_select_node_with_live_paths(&state, &live),
            Some([0xBB; 16]),
            "pathless bootstrap metadata alone must not win"
        );

        promote_static_live_paths(&state, &live);

        assert_eq!(
            auto_select_node_with_live_paths(&state, &live),
            Some(sync_hub),
            "a live path to the bundled sync hub must promote it above fallback nodes"
        );
    }

    #[test]
    fn relay_send_metadata_requires_identity_and_stamp_cost() {
        let state = make_state();
        let node = sync_hub_hash();
        let node_hex = hex::encode(node);
        let remote = Identity::new();
        let mut mgr =
            crate::lxmf::LxmfManager::load_or_create(&state.config.data_root, None).unwrap();

        assert!(!relay_send_metadata_ready(&state, &node));

        mgr.known_identities
            .insert(node_hex, remote.get_public_key());
        assert!(!mgr.propagation_node_ready_for_send(&node));

        mgr.router.set_stamp_cost(node, 0);
        assert!(mgr.propagation_node_ready_for_send(&node));

        *state.lxmf.lock().unwrap() = Some(mgr);
        assert!(relay_send_metadata_ready(&state, &node));
    }

    #[test]
    fn relay_failure_backoff_blocks_selection_until_success() {
        let state = make_state();
        let now = now_f64();
        seed_static_node(&state, [0xAA; 16], Some(1), now, "enabled", "reachable");
        seed_node(&state, [0xBB; 16], 5, now);

        mark_relay_failure(&state, [0xAA; 16], "test_failure");
        assert_eq!(auto_select_node(&state), Some([0xBB; 16]));

        mark_relay_path_success(&state, [0xAA; 16]);
        assert_eq!(auto_select_node(&state), Some([0xAA; 16]));
    }

    #[test]
    fn disabling_static_favor_uses_normal_hop_ranking() {
        let state = make_state();
        set_active_identity_settings(&state, "auto", false);
        let now = now_f64();
        seed_static_node(&state, [0xAA; 16], Some(8), now, "enabled", "reachable");
        seed_node(&state, [0xBB; 16], 1, now);

        assert_eq!(auto_select_node(&state), Some([0xBB; 16]));
    }

    #[test]
    fn disabling_static_favor_turns_sync_hub_priority_off() {
        let state = make_state();
        set_active_identity_settings(&state, "auto", false);
        let now = now_f64();
        let sync_hub = sync_hub_hash();
        let regional = ratspeak_node_1_hash();
        seed_static_node(&state, sync_hub, Some(9), now, "enabled", "reachable");
        seed_static_node(&state, regional, Some(1), now, "enabled", "reachable");

        assert_eq!(auto_select_node(&state), Some(regional));
    }

    #[test]
    fn static_probe_prefers_sync_hub_first() {
        let state = make_state();
        let now = now_f64();
        let sync_hub = hex::decode("deadbeefbadfceeae39c1aceb911e205").unwrap();
        let mut sync_hash = [0u8; 16];
        sync_hash.copy_from_slice(&sync_hub);

        let first = select_static_probe_candidates(&state, StaticProbeKind::Startup, now);
        assert_eq!(first, vec![sync_hash]);
    }

    #[test]
    fn static_probe_candidates_are_serialized_in_auto_favor_mode() {
        let state = make_state();
        let now = now_f64();

        let first = select_static_probe_candidates(&state, StaticProbeKind::Startup, now);
        assert_eq!(first.len(), STATIC_STARTUP_PROBE_BUDGET);
        mark_static_probe_sent(&state, &first, now);

        let second = select_static_probe_candidates(&state, StaticProbeKind::Startup, now + 1.0);
        assert_eq!(
            second.len(),
            0,
            "Auto favor must wait for the active static probe to resolve before probing another Ratspeak node"
        );
    }

    #[test]
    fn static_probe_falls_forward_after_sync_hub_probe_timeout() {
        let state = make_state();
        let now = now_f64();
        let sync_hub = hex::decode("deadbeefbadfceeae39c1aceb911e205").unwrap();
        let mut sync_hash = [0u8; 16];
        sync_hash.copy_from_slice(&sync_hub);
        let regional = hex::decode("111111111111425117677c92c1693b92").unwrap();
        let mut regional_hash = [0u8; 16];
        regional_hash.copy_from_slice(&regional);

        let first = select_static_probe_candidates(&state, StaticProbeKind::Startup, now);
        assert_eq!(first, vec![sync_hash]);
        mark_static_probe_sent(&state, &first, now);

        let after_timeout = select_static_probe_candidates(
            &state,
            StaticProbeKind::Background,
            now + static_probe_timeout() + 1.0,
        );
        assert_eq!(after_timeout, vec![regional_hash]);
    }

    #[test]
    fn auto_failure_lockout_expires_after_window() {
        let state = make_state();
        let now = now_f64();
        seed_node(&state, [0xAA; 16], 1, now);

        let stale = match Instant::now().checked_sub(SYNC_FAILURE_WINDOW + Duration::from_secs(60))
        {
            Some(t) => t,
            // Freshly-booted machine: Instant base too young; skip.
            None => return,
        };
        {
            let mut map = state.auto_failure_counts.lock().unwrap();
            map.insert([0xAA; 16], (SYNC_FAILURE_THRESHOLD + 5, stale));
        }

        assert_eq!(
            auto_select_node(&state),
            Some([0xAA; 16]),
            "expired lockout must not block selection"
        );
    }

    #[test]
    fn auto_select_node_filters_unknown_state() {
        let state = make_state();
        let now = now_f64();
        seed_node_with_state(&state, [0xAA; 16], 1, now, "unknown");
        seed_node_with_state(&state, [0xBB; 16], 5, now, "enabled");
        assert_eq!(
            auto_select_node(&state),
            Some([0xBB; 16]),
            "unknown-state nodes must never win Auto selection"
        );

        let state = make_state();
        let now = now_f64();
        seed_node_with_state(&state, [0xAA; 16], 1, now, "disabled");
        seed_node_with_state(&state, [0xBB; 16], 5, now, "enabled");
        assert_eq!(auto_select_node(&state), Some([0xBB; 16]));

        let state = make_state();
        let now = now_f64();
        seed_node_with_state(&state, [0xCC; 16], 2, now, "known");
        assert_eq!(auto_select_node(&state), Some([0xCC; 16]));
    }

    #[test]
    fn equal_quality_does_not_thrash() {
        let state = make_state();
        let now = now_f64();
        seed_node(&state, [0xAA; 16], 2, now);
        seed_node(&state, [0xBB; 16], 2, now);

        let mut picks: HashMap<[u8; 16], usize> = HashMap::new();
        for _ in 0..20 {
            let pick = auto_select_node(&state).unwrap();
            *picks.entry(pick).or_insert(0) += 1;
        }
        assert_eq!(
            picks.len(),
            1,
            "selection should be stable, got {:?}",
            picks
        );
        assert!(picks.contains_key(&[0xAA; 16]));
    }
}
