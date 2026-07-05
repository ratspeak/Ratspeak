//! Cross-command helpers: transport RPC, interface progress, game persistence,
//! BLE teardown, JSON→MessagePack. All `pub(crate)`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use serde_json::{Value, json};

use crate::db;
use crate::helpers::{active_identity_id, validate_hex};
use crate::lxmf::resolve_destination;
use crate::state::AppState;

const LXMF_APP_NAME: &str = "lxmf.delivery";

pub(crate) fn transport_sender(
    state: &AppState,
) -> Option<tokio::sync::mpsc::Sender<rns_transport::messages::TransportMessage>> {
    state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.transport_tx.clone()))
}

pub(crate) fn active_rns_config_dir(state: &AppState) -> PathBuf {
    if let Some(config_dir) = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.config_dir.clone()))
    {
        return config_dir;
    }

    if state.config.uses_app_private_rns_config_dir() {
        let active_identity = crate::helpers::active_identity_id(state);
        if !active_identity.is_empty() {
            return state.config.identity_rns_config_dir(&active_identity);
        }
    }

    state.config.rns_config_dir.clone()
}

pub(crate) fn with_rns_config_lock<T>(state: &AppState, f: impl FnOnce() -> T) -> T {
    let _guard = state
        .rns_config_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f()
}

// Interface names whose most recent `add_lora_interface` created a brand-new
// config entry. Connect-failure rollback (`cancel_ble_connect`) may only
// delete these; reconnects of pre-existing radios must survive a failed or
// cancelled connect.
static FRESH_LORA_ADDS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

pub(crate) fn mark_lora_add_freshness(name: &str, fresh: bool) {
    let mut set = FRESH_LORA_ADDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if fresh {
        set.insert(name.to_string());
    } else {
        set.remove(name);
    }
}

#[cfg_attr(not(feature = "ble"), allow(dead_code))]
pub(crate) fn take_fresh_lora_add(name: &str) -> bool {
    FRESH_LORA_ADDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(name)
}

pub(crate) fn remove_stored_file_refs(
    files_dir: &Path,
    file_refs: impl IntoIterator<Item = String>,
) {
    for file_ref in file_refs {
        if file_ref.is_empty() {
            continue;
        }
        let Some(sanitized) = ratspeak_runtime::lxmf::sanitize_stored_file_name(&file_ref) else {
            tracing::warn!(stored_name = %file_ref, "skipping unsafe stored attachment path");
            continue;
        };
        std::fs::remove_file(files_dir.join(sanitized)).ok();
    }
}

pub(crate) async fn transport_query(
    state: &AppState,
    query: rns_transport::messages::TransportQuery,
) -> Option<rns_transport::messages::TransportQueryResponse> {
    let tx = transport_sender(state)?;
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(rns_transport::messages::TransportMessage::Rpc {
        query,
        response_tx: resp_tx,
    })
    .await
    .ok()?;
    resp_rx.await.ok()
}

pub(crate) fn blackhole_reason_display(
    reason: rns_transport::blackhole::BlackholeReason,
    reason_label: Option<&str>,
) -> String {
    reason_label.unwrap_or_else(|| reason.as_str()).to_string()
}

// Each entry: `hash`, `reason`, `created`, `expires_in` (null = permanent),
// `verified` (false means we have no announce backing this identity).
pub(crate) async fn snapshot_blackhole(state: &AppState) -> Vec<Value> {
    use rns_transport::messages::{TransportQuery, TransportQueryResponse};
    let entries = match transport_query(state, TransportQuery::GetBlackholedIdentities).await {
        Some(TransportQueryResponse::BlackholeList(v)) => v,
        _ => return Vec::new(),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    entries
        .into_iter()
        .map(|e| {
            let expires_in = e.ttl.map(|t| (e.created + t - now).max(0.0));
            let reason = blackhole_reason_display(e.reason, e.reason_label.as_deref());
            json!({
                "hash": rns_crypto::hex_encode(&e.identity_hash),
                "reason": reason,
                "created": e.created,
                "expires_in": expires_in,
                "verified": e.verified,
            })
        })
        .collect()
}

/// Resolve a 16-byte hex blob (LXMF dest hash OR identity hash) to the canonical
/// identity hash via rsReticulum's `recent_announces`. Returns `None` when the
/// input is neither a known destination nor a known identity.
pub(crate) async fn resolve_identity_hash(state: &AppState, input: [u8; 16]) -> Option<[u8; 16]> {
    use rns_transport::messages::{TransportQuery, TransportQueryResponse};
    match transport_query(state, TransportQuery::ResolveIdentityHash { input }).await {
        Some(TransportQueryResponse::HashResult(opt)) => opt,
        _ => None,
    }
}

/// Resolve a contact's LXMF destination hash to its Reticulum identity hash.
/// The transport announce cache is preferred, but blackholing deliberately
/// drops paths and future announces, so contact unblock also needs the
/// persistent `identity_activity` mapping learned before the block.
pub(crate) async fn resolve_contact_identity_hash(
    state: &AppState,
    dest_hash_hex: &str,
    input: [u8; 16],
) -> Option<[u8; 16]> {
    if let Some(identity_hash) = resolve_identity_hash(state, input).await {
        return Some(identity_hash);
    }

    let dest = dest_hash_hex.to_string();
    let db = state.db.clone();
    let identity_hex = db::spawn_db(db, move |p| db::identity_hash_for_dest(&p, &dest))
        .await
        .ok()
        .flatten()?;
    hex_to_array16(&identity_hex)
}

/// Batch lookup: which of the given LXMF dest hashes belong to a currently
/// blackholed identity? Returns the set of hex-encoded dest hashes that are
/// blocked at the transport layer. The actor handles the dest→identity→
/// blackhole composition so callers compare dest hashes against dest hashes.
pub(crate) async fn filter_blackholed_dests(
    state: &AppState,
    dests: Vec<[u8; 16]>,
) -> std::collections::HashSet<String> {
    use rns_transport::messages::{TransportQuery, TransportQueryResponse};
    if dests.is_empty() {
        return Default::default();
    }
    match transport_query(state, TransportQuery::FilterBlackholedDests { dests }).await {
        Some(TransportQueryResponse::BlackholedDests(v)) => {
            v.into_iter().map(|d| rns_crypto::hex_encode(&d)).collect()
        }
        _ => Default::default(),
    }
}

/// Broadcast `blackhole_update` after any mutation.
pub(crate) async fn broadcast_blackhole_update(state: &AppState) {
    let entries = snapshot_blackhole(state).await;
    state.emit_to_all("blackhole_update", json!({ "entries": entries }));
}

pub(crate) fn normalize_transport_mode(mode: &str) -> Option<&'static str> {
    match mode.trim() {
        "on" => Some("on"),
        "off" => Some("off"),
        "auto" => Some("auto"),
        _ => None,
    }
}

pub(crate) fn config_transport_enabled(state: &AppState) -> bool {
    let config_dir = active_rns_config_dir(state);
    crate::rns_config::transport_mode_enabled(&config_dir)
}

pub(crate) fn persisted_transport_mode(state: &AppState) -> String {
    db::get_setting(&state.db, "transport_mode")
        .and_then(|mode| normalize_transport_mode(&mode).map(str::to_string))
        .unwrap_or_else(|| {
            if config_transport_enabled(state) {
                "on".to_string()
            } else {
                "off".to_string()
            }
        })
}

pub(crate) fn hub_interfaces_payload(state: &AppState, mut ifaces: Value) -> Value {
    let mode = persisted_transport_mode(state);
    let configured_enabled = config_transport_enabled(state);
    let suppressed = configured_enabled
        && state
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.instance_mode))
            .is_some_and(|mode| mode == rns_runtime::reticulum::InstanceMode::Client);
    let enabled = configured_enabled && !suppressed;

    if let Some(obj) = ifaces.as_object_mut() {
        obj.insert(
            "transport".to_string(),
            json!({
                "mode": mode,
                "enabled": enabled,
                "configured_enabled": configured_enabled,
                "suppressed": suppressed,
            }),
        );
    }
    ifaces
}

pub(crate) fn format_contacts_list(contacts: &[Value]) -> Vec<Value> {
    contacts
        .iter()
        .map(|c| {
            json!({
                "hash": c.get("dest_hash"),
                "display_name": c.get("display_name"),
                "trust": c.get("trust"),
                "notes": c.get("notes"),
                "first_seen": c.get("first_seen"),
                "last_seen": c.get("last_seen"),
                "services": c.get("services"),
            })
        })
        .collect()
}

pub(crate) fn emit_hub_interfaces(state: &AppState, ifaces: serde_json::Value) {
    crate::commands::interfaces::reconcile_auto_transport_after_interface_change(state, &ifaces);
    let ifaces = hub_interfaces_payload(state, ifaces);
    state.set_last_hub_interfaces(ifaces.clone());
    state.emit_to_all("hub_interfaces_update", ifaces);
}

pub(crate) async fn hydrate_contact_identity_for_send(state: &AppState, dest_hash: &str) -> bool {
    let dest_hash = dest_hash.trim().to_ascii_lowercase();
    if !validate_hex(&dest_hash, 32, 32) {
        return false;
    }

    if state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| {
            lxmf.as_ref()
                .map(|mgr| mgr.is_destination_known(&dest_hash))
        })
        .unwrap_or(false)
    {
        return true;
    }

    let identity_id = active_identity_id(state);
    let dest_for_db = dest_hash.clone();
    let contact = match db::spawn_db(state.db.clone(), move |p| {
        db::get_contact(&p, &dest_for_db, &identity_id)
    })
    .await
    {
        Ok(Some(contact)) => contact,
        _ => return false,
    };

    let Some(pubkey_hex) = contact
        .get("identity_pubkey")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| validate_hex(s, 128, 128))
    else {
        return false;
    };
    let Ok(pubkey_bytes) = hex::decode(pubkey_hex) else {
        return false;
    };
    if pubkey_bytes.len() != 64 {
        return false;
    }
    let mut public_key = [0u8; 64];
    public_key.copy_from_slice(&pubkey_bytes);

    let Ok(identity) = Identity::from_public_key(&public_key) else {
        tracing::warn!(dest = %dest_hash, "contact identity public key is invalid");
        return false;
    };
    let expected_lxmf =
        Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
    if hex::encode(expected_lxmf) != dest_hash {
        tracing::warn!(
            dest = %dest_hash,
            expected = %hex::encode(expected_lxmf),
            "contact identity public key does not match LXMF destination"
        );
        return false;
    }

    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        mgr.update_remote_crypto(&dest_hash, &public_key, None);
        mgr.save_crypto_state();
        tracing::debug!(dest = %dest_hash, "hydrated LXMF identity from contact card");
        return true;
    }
    false
}

// Extracts transport_tx then calls resolve_destination outside the lock
// (clippy::await_holding_lock). Failure does not block sending.
pub(crate) async fn resolve_before_send(state: &AppState, dest_hash: &str) {
    let _ = hydrate_contact_identity_for_send(state, dest_hash).await;

    let transport_tx = {
        if let Ok(lxmf) = state.lxmf.lock() {
            lxmf.as_ref()
                .and_then(|mgr| mgr.router.transport_tx.clone())
        } else {
            None
        }
    };

    if let Some(tx) = transport_tx
        && !resolve_destination(state, dest_hash, &tx).await
    {
        tracing::warn!(dest = %dest_hash, "could not resolve destination, sending anyway");
    }
}

pub(crate) fn hex_to_array16(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        let hi = u8::from_str_radix(&s[i * 2..i * 2 + 1], 16).ok()?;
        let lo = u8::from_str_radix(&s[i * 2 + 1..i * 2 + 2], 16).ok()?;
        *byte = (hi << 4) | lo;
    }
    Some(out)
}

pub(crate) fn json_to_rmpv_map(v: &Value) -> std::collections::HashMap<String, rmpv::Value> {
    let mut map = std::collections::HashMap::new();
    if let Some(obj) = v.as_object() {
        for (key, val) in obj {
            map.insert(key.clone(), json_to_rmpv(val));
        }
    }
    map
}

fn json_to_rmpv(v: &Value) -> rmpv::Value {
    match v {
        Value::Null => rmpv::Value::Nil,
        Value::Bool(b) => rmpv::Value::Boolean(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                rmpv::Value::Integer(i.into())
            } else if let Some(u) = n.as_u64() {
                rmpv::Value::Integer(u.into())
            } else if let Some(f) = n.as_f64() {
                rmpv::Value::F64(f)
            } else {
                rmpv::Value::Nil
            }
        }
        Value::String(s) => rmpv::Value::String(s.as_str().into()),
        Value::Array(arr) => rmpv::Value::Array(arr.iter().map(json_to_rmpv).collect()),
        Value::Object(obj) => {
            let pairs: Vec<(rmpv::Value, rmpv::Value)> = obj
                .iter()
                .map(|(k, v)| (rmpv::Value::String(k.as_str().into()), json_to_rmpv(v)))
                .collect();
            rmpv::Value::Map(pairs)
        }
    }
}

/// `delivery_state = Some` stamps metadata; `None` preserves existing.
pub(crate) async fn save_session_from_state(
    state: &AppState,
    session_id: &str,
    identity_id: &str,
    app_id: &str,
    contact_hash: &str,
    session_state: &std::collections::HashMap<String, serde_json::Value>,
    delivery_state: Option<&str>,
) {
    // Empty session_id is unaddressable; bail loudly.
    if session_id.is_empty() {
        tracing::warn!(
            target: "ttt_trace",
            step = "save_session.empty_sid_rejected",
            app_id = %app_id,
            identity_id = %identity_id,
            contact_hash = %contact_hash,
            delivery_state = ?delivery_state,
            "refusing to persist app_session with empty session_id"
        );
        return;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let status = session_state
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending");
    let initiator = session_state
        .get("initiator")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Unwrap nested "metadata" so DB column has flat fields.
    let mut metadata_map: std::collections::HashMap<String, serde_json::Value> = session_state
        .get("metadata")
        .and_then(|v| v.as_object())
        .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();

    if let Some(ds) = delivery_state {
        metadata_map.insert("delivery_state".to_string(), json!(ds));
    }

    let session = lrgp::session::Session {
        session_id: session_id.to_string(),
        identity_id: identity_id.to_string(),
        app_id: app_id.to_string(),
        app_version: 1,
        contact_hash: contact_hash.to_string(),
        initiator: initiator.to_string(),
        status: status.to_string(),
        metadata: metadata_map,
        unread: 0,
        created_at: session_state
            .get("created_at")
            .and_then(|v| v.as_f64())
            .unwrap_or(now),
        updated_at: now,
        last_action_at: now,
    };
    let _ = db::spawn_db(state.db.clone(), move |p| {
        db::save_game_session(&p, &session);
    })
    .await;
}

pub(crate) async fn emit_game_sessions(
    state: &AppState,
    identity_id: &str,
    contact_hash: Option<&str>,
) {
    let id_c = identity_id.to_string();
    let ch_c = contact_hash.map(|s| s.to_string());
    let (per_contact, all) = db::spawn_db(state.db.clone(), move |p| {
        let per = ch_c
            .as_deref()
            .map(|ch| db::list_game_sessions(&p, &id_c, Some(ch), None));
        let all = db::list_game_sessions(&p, &id_c, None, None);
        (per, all)
    })
    .await
    .expect("db task panicked");

    if let (Some(sessions), Some(ch)) = (per_contact, contact_hash) {
        state.emit_to_all("active_games", json!({ "hash": ch, "games": sessions }));
    }
    state.emit_to_all("all_game_sessions", json!(all));
}

pub(crate) fn emit_op_status_broadcast(
    state: &AppState,
    operation: &str,
    node: &str,
    step: &str,
    done: bool,
    error: Option<&str>,
) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    state.emit_to_all(
        "node_operation_status",
        json!({
            "operation": operation,
            "node": node,
            "step": step,
            "done": done,
            "error": error,
            "timestamp": ts,
        }),
    );
}

pub(crate) async fn disable_ble_peer_inner(state: &Arc<AppState>) {
    // Serialize against enable: without this a rapid toggle (or an expiry
    // firing mid-enable) races the spawn, leaving either a zombie "enabled"
    // interface or a torn-down new session. The enable task holds the same
    // lock for its whole duration, so this waits for any in-flight enable.
    let _enable_guard = state.ble_peer_enable_lock.lock().await;
    tracing::info!("disable_ble_peer_inner: start");
    let _ = db::spawn_db(state.db.clone(), |p| {
        db::set_setting(&p, "ble_peer_enabled", "0");
        db::set_setting(&p, "ble_peer_expires_at", "0");
    })
    .await;
    state.emit_to_all("ble_peer_status_update", json!({ "enabled": false }));
    state
        .ble_peer_count
        .store(0, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut peers) = state.ble_peers.lock() {
        peers.clear();
    }
    state.emit_to_all(
        "ble_peer_status_changed",
        json!({ "state": "off", "peer_count": 0 }),
    );

    let rns_handle = {
        state
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()))
    };
    if let Some(handle) = rns_handle {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        if handle
            .transport_tx
            .send(rns_transport::messages::TransportMessage::Rpc {
                query: rns_transport::messages::TransportQuery::GetInterfaceStats,
                response_tx: resp_tx,
            })
            .await
            .is_ok()
            && let Ok(rns_transport::messages::TransportQueryResponse::InterfaceStats(stats)) =
                resp_rx.await
        {
            #[cfg(feature = "ble")]
            let mut torn_down = false;
            let iface_count = stats.len();
            tracing::info!(
                iface_count,
                "disable_ble_peer_inner: searching for Bluetooth Peer interface"
            );
            for iface in stats {
                if iface.name == "Bluetooth Peer" || iface.name == "BLE Mesh" {
                    tracing::info!(
                        id = iface.id,
                        "disable_ble_peer_inner: tearing down Bluetooth Peer interface"
                    );
                    #[cfg(feature = "ble")]
                    {
                        rns_runtime::reticulum::teardown_ble_peer_interface(&handle, iface.id)
                            .await;
                        torn_down = true;
                    }
                    #[cfg(not(feature = "ble"))]
                    {
                        rns_runtime::reticulum::teardown_interface(&handle, iface.id).await;
                    }
                    break;
                }
            }

            #[cfg(feature = "ble")]
            if !torn_down {
                tracing::info!(
                    "disable_ble_peer_inner: no live interface, forcing stop_ble_peer_interface"
                );
                rns_interface::ble_peer::stop_ble_peer_interface().await;
            }
        } else {
            tracing::warn!(
                "disable_ble_peer_inner: failed to query interface stats, forcing stop_ble_peer_interface"
            );
            #[cfg(feature = "ble")]
            rns_interface::ble_peer::stop_ble_peer_interface().await;
        }
    } else {
        tracing::info!("disable_ble_peer_inner: no RNS runtime, clearing BLE state");
        #[cfg(feature = "ble")]
        rns_interface::ble_peer::stop_ble_peer_interface().await;
    }
    tracing::info!("disable_ble_peer_inner: done");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DashboardConfig;
    use rns_transport::blackhole::BlackholeReason;
    use std::sync::Arc;

    fn memory_pool() -> ratspeak_db::DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        ratspeak_db::init_schema(&pool).unwrap();
        pool
    }

    fn state_for_config(config: DashboardConfig) -> AppState {
        AppState::new(
            config,
            memory_pool(),
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        )
    }

    #[test]
    fn fresh_lora_add_marker_gates_rollback_and_consumes_once() {
        // Fresh add: rollback allowed exactly once.
        mark_lora_add_freshness("Marker Radio Fresh", true);
        assert!(take_fresh_lora_add("Marker Radio Fresh"));
        assert!(!take_fresh_lora_add("Marker Radio Fresh"));

        // Re-add of an existing entry clears any stale fresh marker, so a
        // failed reconnect never deletes pre-existing config.
        mark_lora_add_freshness("Marker Radio Existing", true);
        mark_lora_add_freshness("Marker Radio Existing", false);
        assert!(!take_fresh_lora_add("Marker Radio Existing"));

        // Resume/cancel paths that never went through add are not deletable.
        assert!(!take_fresh_lora_add("Marker Radio Never Added"));
    }

    #[test]
    fn blackhole_reason_display_prefers_custom_label() {
        assert_eq!(
            blackhole_reason_display(BlackholeReason::Manual, Some("operator note")),
            "operator note"
        );
        assert_eq!(
            blackhole_reason_display(BlackholeReason::RateLimit, None),
            "rate_limit"
        );
    }

    #[test]
    fn active_rns_config_dir_uses_active_identity_before_runtime_starts() {
        let temp = tempfile::tempdir().unwrap();
        let config = DashboardConfig::from_env_and_defaults(temp.path().to_path_buf());
        let state = state_for_config(config.clone());
        let identity_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        ratspeak_db::save_identity(
            &state.db,
            identity_hash,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "Default",
            "Default",
        );
        ratspeak_db::set_active_identity(&state.db, identity_hash).unwrap();

        let active_dir = active_rns_config_dir(&state);

        assert_eq!(active_dir, config.identity_rns_config_dir(identity_hash));
        assert!(active_dir.exists());
        assert_ne!(active_dir, config.rns_config_dir);
    }

    #[test]
    fn active_rns_config_dir_respects_explicit_override_before_runtime_starts() {
        let temp = tempfile::tempdir().unwrap();
        let override_dir = temp.path().join("custom-reticulum");
        let config = DashboardConfig {
            data_root: temp.path().to_path_buf(),
            data_dir: temp.path().join(".ratspeak"),
            rns_config_dir: override_dir.clone(),
            rns_config_dir_overridden: true,
            max_log_entries: 200,
            rns_share_instance: true,
            rns_instance_name: None,
            rns_derive_ports: false,
            rns_seed_default_interface: false,
        };
        let state = state_for_config(config);
        let identity_hash = "cccccccccccccccccccccccccccccccc";
        ratspeak_db::save_identity(
            &state.db,
            identity_hash,
            "dddddddddddddddddddddddddddddddd",
            "Default",
            "Default",
        );
        ratspeak_db::set_active_identity(&state.db, identity_hash).unwrap();

        assert_eq!(active_rns_config_dir(&state), override_dir);
    }

    #[test]
    fn transport_payload_falls_back_to_enabled_config_when_db_setting_missing() {
        let temp = tempfile::tempdir().unwrap();
        let config = DashboardConfig::from_env_and_defaults(temp.path().to_path_buf());
        let state = state_for_config(config);
        let identity_hash = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        ratspeak_db::save_identity(
            &state.db,
            identity_hash,
            "ffffffffffffffffffffffffffffffff",
            "Default",
            "Default",
        );
        ratspeak_db::set_active_identity(&state.db, identity_hash).unwrap();

        let config_dir = active_rns_config_dir(&state);
        crate::rns_config::write_config(
            &config_dir,
            "[reticulum]\nenable_transport = True\n\n[interfaces]\n",
        );

        let payload = hub_interfaces_payload(&state, json!({}));
        let transport = payload.get("transport").expect("transport payload");
        assert_eq!(transport.get("mode").and_then(Value::as_str), Some("on"));
        assert_eq!(
            transport.get("configured_enabled").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            transport.get("enabled").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn interface_write_targets_active_identity_config_before_runtime_starts() {
        let temp = tempfile::tempdir().unwrap();
        let config = DashboardConfig::from_env_and_defaults(temp.path().to_path_buf());
        let state = state_for_config(config.clone());
        let identity_hash = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        ratspeak_db::save_identity(
            &state.db,
            identity_hash,
            "ffffffffffffffffffffffffffffffff",
            "Default",
            "Default",
        );
        ratspeak_db::set_active_identity(&state.db, identity_hash).unwrap();

        let config_dir = active_rns_config_dir(&state);
        assert!(with_rns_config_lock(&state, || {
            crate::rns_config::add_auto_interface(
                &config_dir,
                "Local Network",
                &crate::rns_config::AutoInterfaceOptions::default(),
            )
        }));

        let identity_config = crate::rns_config::read_config(&config_dir).unwrap();
        assert!(identity_config.contains("[[Local Network]]"));
        assert!(crate::rns_config::read_config(&config.rns_config_dir).is_none());
    }

    #[test]
    fn rns_config_lock_serializes_concurrent_interface_writes() {
        let temp = tempfile::tempdir().unwrap();
        let config = DashboardConfig::from_env_and_defaults(temp.path().to_path_buf());
        let state = Arc::new(state_for_config(config));
        let config_dir = active_rns_config_dir(&state);
        crate::rns_config::write_config(
            &config_dir,
            "[reticulum]\n  enable_transport = False\n\n[interfaces]\n",
        );

        let mut handles = Vec::new();
        for idx in 0..8 {
            let state = Arc::clone(&state);
            let config_dir = config_dir.clone();
            handles.push(std::thread::spawn(move || {
                let name = format!("TCP {idx}");
                let host = format!("node{idx}.example");
                let port = 4000 + idx as u16;
                assert!(with_rns_config_lock(&state, || {
                    crate::rns_config::add_tcp_client(&config_dir, &name, &host, port)
                }));
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let content = crate::rns_config::read_config(&config_dir).unwrap();
        for idx in 0..8 {
            assert!(content.contains(&format!("[[TCP {idx}]]")));
            assert!(content.contains(&format!("target_host = node{idx}.example")));
        }
    }
}
