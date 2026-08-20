//! Cross-command helpers: transport RPC, interface progress, game persistence,
//! BLE teardown, JSON→MessagePack. All `pub(crate)`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use serde_json::{Value, json};

use crate::db;
use crate::helpers::{active_identity_id, validate_hex};
use crate::state::AppState;

use ratspeak_core::LXMF_DELIVERY_APP_NAME as LXMF_APP_NAME;

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

// Config-scoped interface names whose most recent `add_lora_interface`
// created a brand-new entry. Rollback may only delete these; identity
// switches and reconnects of pre-existing radios must never cross-consume a
// marker. Markers are deliberately short-lived and bounded because a
// successful connection has no rollback consumer.
const FRESH_LORA_ADD_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const MAX_FRESH_LORA_ADDS: usize = 256;
pub(crate) type FreshLoraAddMarker = u64;
type FreshLoraAddKey = (PathBuf, String);

#[derive(Clone, Copy)]
struct FreshLoraAddEntry {
    marker: FreshLoraAddMarker,
    marked_at: std::time::Instant,
}

type FreshLoraAddRegistry = std::collections::HashMap<FreshLoraAddKey, FreshLoraAddEntry>;

static FRESH_LORA_ADDS: std::sync::LazyLock<std::sync::Mutex<FreshLoraAddRegistry>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
static NEXT_FRESH_LORA_ADD_MARKER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

fn prune_fresh_lora_adds(registry: &mut FreshLoraAddRegistry, now: std::time::Instant) {
    registry.retain(|_, entry| now.saturating_duration_since(entry.marked_at) < FRESH_LORA_ADD_TTL);
}

fn mark_lora_add_freshness_in(
    registry: &mut FreshLoraAddRegistry,
    config_dir: &Path,
    name: &str,
    fresh: bool,
    now: std::time::Instant,
) -> Option<FreshLoraAddMarker> {
    prune_fresh_lora_adds(registry, now);
    let key = (config_dir.to_path_buf(), name.to_string());
    if !fresh {
        registry.remove(&key);
        return None;
    }

    if !registry.contains_key(&key) && registry.len() >= MAX_FRESH_LORA_ADDS {
        if let Some(oldest) = registry
            .iter()
            .min_by_key(|(_, entry)| entry.marked_at)
            .map(|(key, _)| key.clone())
        {
            registry.remove(&oldest);
        }
    }
    let marker = NEXT_FRESH_LORA_ADD_MARKER
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .wrapping_add(1);
    registry.insert(
        key,
        FreshLoraAddEntry {
            marker,
            marked_at: now,
        },
    );
    Some(marker)
}

pub(crate) fn mark_lora_add_freshness(
    config_dir: &Path,
    name: &str,
    fresh: bool,
) -> Option<FreshLoraAddMarker> {
    let mut registry = FRESH_LORA_ADDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    mark_lora_add_freshness_in(
        &mut registry,
        config_dir,
        name,
        fresh,
        std::time::Instant::now(),
    )
}

#[cfg_attr(not(feature = "ble"), allow(dead_code))]
pub(crate) fn take_fresh_lora_add(
    config_dir: &Path,
    name: &str,
    expected_marker: FreshLoraAddMarker,
) -> bool {
    let mut registry = FRESH_LORA_ADDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    prune_fresh_lora_adds(&mut registry, std::time::Instant::now());
    let key = (config_dir.to_path_buf(), name.to_string());
    if registry
        .get(&key)
        .is_some_and(|entry| entry.marker == expected_marker)
    {
        registry.remove(&key);
        true
    } else {
        false
    }
}

pub(crate) fn clear_fresh_lora_add_marker(
    state: &AppState,
    config_dir: &Path,
    name: &str,
    marker: FreshLoraAddMarker,
) -> bool {
    with_rns_config_lock(state, || take_fresh_lora_add(config_dir, name, marker))
}

pub(crate) fn rollback_fresh_lora_add_marker(
    state: &AppState,
    config_dir: &Path,
    name: &str,
    marker: FreshLoraAddMarker,
) -> Option<crate::rns_config::RemoveInterfaceOutcome> {
    with_rns_config_lock(state, || {
        take_fresh_lora_add(config_dir, name, marker)
            .then(|| crate::rns_config::remove_interface_checked(config_dir, name))
    })
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
            tracing::warn!(
                reason = "unsafe_stored_name",
                "skipping unsafe stored attachment path"
            );
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
        if let Some(rnodes) = obj.get_mut("rnode").and_then(Value::as_array_mut) {
            for rnode in rnodes {
                if let Some(fields) = rnode.as_object_mut() {
                    // Stable USB selectors are private runtime identity. The
                    // WebView only receives the opaque configured port needed
                    // by the existing device picker; recovery is keyed by the
                    // sanitized interface name through a Rust command.
                    fields.remove("usb_vendor_id");
                    fields.remove("usb_product_id");
                    fields.remove("usb_serial_number");
                }
            }
        }
        obj.insert(
            "transport".to_string(),
            json!({
                "mode": mode,
                "enabled": enabled,
                "configured_enabled": configured_enabled,
                "suppressed": suppressed,
            }),
        );
        obj.insert(
            "mobile_hardware".to_string(),
            state.mobile_hardware_state_snapshot(),
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
        tracing::warn!(
            reason = "invalid_public_key",
            "contact identity public key is invalid"
        );
        return false;
    };
    let expected_lxmf =
        Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
    if hex::encode(expected_lxmf) != dest_hash {
        tracing::warn!(
            reason = "destination_mismatch",
            "contact identity public key does not match LXMF destination"
        );
        return false;
    }

    let identity_changed = state.lxmf.lock().ok().and_then(|mut lxmf| {
        lxmf.as_mut()
            .map(|mgr| mgr.update_remote_crypto(&dest_hash, &public_key, None).0)
    });
    if let Some(identity_changed) = identity_changed {
        if identity_changed {
            if let Err(error) = ratspeak_runtime::lxmf_persistence::persist_current_delta(
                state,
                true,
                &[],
                false,
                "contact_hydration",
            )
            .await
            {
                tracing::warn!(%error, "contact identity persistence failed");
            }
        }
        tracing::debug!("hydrated LXMF identity from contact card");
        return true;
    }
    false
}

pub(crate) use ratspeak_core::hex_to_array16;

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

pub(crate) struct SessionStateSave<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) identity_id: &'a str,
    pub(crate) app_id: &'a str,
    pub(crate) app_version: u32,
    pub(crate) contact_hash: &'a str,
    pub(crate) session_state: &'a std::collections::HashMap<String, serde_json::Value>,
    /// `Some` stamps delivery metadata; `None` preserves the existing value.
    pub(crate) delivery_state: Option<&'a str>,
}

pub(crate) async fn save_session_from_state(state: &AppState, save: SessionStateSave<'_>) {
    let SessionStateSave {
        session_id,
        identity_id,
        app_id,
        app_version,
        contact_hash,
        session_state,
        delivery_state,
    } = save;
    // Empty session_id is unaddressable; bail loudly.
    if session_id.is_empty() {
        tracing::warn!(
            target: "ttt_trace",
            step = "save_session.empty_sid_rejected",
            reason = "empty_session_id",
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
        app_version,
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

pub(crate) async fn disable_ble_peer_inner(state: &Arc<AppState>) -> bool {
    // Serialize against enable: without this a rapid toggle (or an expiry
    // firing mid-enable) races the spawn, leaving either a zombie "enabled"
    // interface or a torn-down new session. The enable task holds the same
    // lock for its whole duration, so this waits for any in-flight enable.
    let _enable_guard = state.ble_peer_enable_lock.lock().await;
    disable_ble_peer_inner_locked(state).await
}

pub(crate) async fn disable_ble_peer_inner_if_expiry(
    state: &Arc<AppState>,
    expected_expires_at: u64,
) -> bool {
    let _enable_guard = state.ble_peer_enable_lock.lock().await;
    let still_this_request = db::spawn_db(state.db.clone(), move |p| {
        let enabled = db::get_setting(&p, "ble_peer_enabled").is_some_and(|value| value == "1");
        let current_expires_at = db::get_setting(&p, "ble_peer_expires_at")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        enabled && current_expires_at == expected_expires_at
    })
    .await
    .unwrap_or(false);
    if !still_this_request {
        return false;
    }
    disable_ble_peer_inner_locked(state).await
}

async fn disable_ble_peer_inner_locked(state: &Arc<AppState>) -> bool {
    tracing::info!("disable_ble_peer_inner: start");
    let was_requested = db::spawn_db(state.db.clone(), |p| {
        db::get_setting(&p, "ble_peer_enabled").is_some_and(|enabled| enabled == "1")
    })
    .await
    .unwrap_or(false);
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
    let mut had_live_interface = false;
    if let Some(handle) = rns_handle {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let stats = if handle
            .transport_tx
            .send(rns_transport::messages::TransportMessage::Rpc {
                query: rns_transport::messages::TransportQuery::GetInterfaceStats,
                response_tx: resp_tx,
            })
            .await
            .is_ok()
        {
            match resp_rx.await {
                Ok(rns_transport::messages::TransportQueryResponse::InterfaceStats(stats)) => {
                    Some(stats)
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(stats) = stats {
            #[cfg(feature = "ble")]
            let mut torn_down = false;
            let iface_count = stats.len();
            tracing::info!(
                iface_count,
                "disable_ble_peer_inner: searching for Bluetooth Peer interface"
            );
            for iface in stats {
                if iface.name == "Bluetooth Peer" || iface.name == "BLE Mesh" {
                    had_live_interface = true;
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
    was_requested || had_live_interface
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
        let first_identity = tempfile::tempdir().unwrap();
        let second_identity = tempfile::tempdir().unwrap();

        // Fresh add: rollback allowed exactly once.
        let fresh_marker =
            mark_lora_add_freshness(first_identity.path(), "Marker Radio Fresh", true).unwrap();
        assert!(take_fresh_lora_add(
            first_identity.path(),
            "Marker Radio Fresh",
            fresh_marker,
        ));
        assert!(!take_fresh_lora_add(
            first_identity.path(),
            "Marker Radio Fresh",
            fresh_marker,
        ));

        // Re-add of an existing entry clears any stale fresh marker, so a
        // failed reconnect never deletes pre-existing config.
        let stale_marker =
            mark_lora_add_freshness(first_identity.path(), "Marker Radio Existing", true).unwrap();
        mark_lora_add_freshness(first_identity.path(), "Marker Radio Existing", false);
        assert!(!take_fresh_lora_add(
            first_identity.path(),
            "Marker Radio Existing",
            stale_marker,
        ));

        // Resume/cancel paths that never captured an exact marker are not
        // deletable.
        assert!(!take_fresh_lora_add(
            first_identity.path(),
            "Marker Radio Never Added",
            u64::MAX,
        ));

        // Identical interface names in different identity config directories
        // cannot consume or clear each other's rollback authorization.
        let first_marker =
            mark_lora_add_freshness(first_identity.path(), "Shared Radio Name", true).unwrap();
        mark_lora_add_freshness(second_identity.path(), "Shared Radio Name", false);
        assert!(!take_fresh_lora_add(
            second_identity.path(),
            "Shared Radio Name",
            first_marker,
        ));
        assert!(take_fresh_lora_add(
            first_identity.path(),
            "Shared Radio Name",
            first_marker,
        ));

        // A same-name replacement in one config gets a new version. A's late
        // success/failure cannot clear or consume B's rollback authorization.
        let marker_a =
            mark_lora_add_freshness(first_identity.path(), "Versioned Radio", true).unwrap();
        let marker_b =
            mark_lora_add_freshness(first_identity.path(), "Versioned Radio", true).unwrap();
        assert!(!take_fresh_lora_add(
            first_identity.path(),
            "Versioned Radio",
            marker_a,
        ));
        assert!(take_fresh_lora_add(
            first_identity.path(),
            "Versioned Radio",
            marker_b,
        ));
    }

    #[test]
    fn fresh_lora_add_registry_expires_and_stays_bounded() {
        let now = std::time::Instant::now();
        let mut registry = FreshLoraAddRegistry::new();
        registry.insert(
            (
                PathBuf::from("/expired-config"),
                "Expired Radio".to_string(),
            ),
            FreshLoraAddEntry {
                marker: 1,
                marked_at: now,
            },
        );
        prune_fresh_lora_adds(
            &mut registry,
            now + FRESH_LORA_ADD_TTL + std::time::Duration::from_secs(1),
        );
        assert!(registry.is_empty());

        for index in 0..(MAX_FRESH_LORA_ADDS + 20) {
            let _ = mark_lora_add_freshness_in(
                &mut registry,
                Path::new("/bounded-config"),
                &format!("Radio {index}"),
                true,
                now,
            );
        }
        assert_eq!(registry.len(), MAX_FRESH_LORA_ADDS);
    }

    #[test]
    fn fresh_lora_success_and_edit_clear_delete_authorization() {
        let temp = tempfile::tempdir().unwrap();
        let config = DashboardConfig::from_env_and_defaults(temp.path().to_path_buf());
        let state = state_for_config(config);
        let config_dir = temp.path().join("rns");
        crate::rns_config::write_config(
            &config_dir,
            "[interfaces]\n  [[Shared Radio]]\n    type = RNodeInterface\n    port = ble://AA:BB:CC:DD:EE:FF\n",
        );

        let success_marker = with_rns_config_lock(&state, || {
            mark_lora_add_freshness(&config_dir, "Shared Radio", true).unwrap()
        });
        assert!(clear_fresh_lora_add_marker(
            &state,
            &config_dir,
            "Shared Radio",
            success_marker,
        ));
        assert!(
            rollback_fresh_lora_add_marker(&state, &config_dir, "Shared Radio", success_marker,)
                .is_none()
        );

        let edit_marker = with_rns_config_lock(&state, || {
            let marker = mark_lora_add_freshness(&config_dir, "Shared Radio", true).unwrap();
            let _ = mark_lora_add_freshness(&config_dir, "Shared Radio", false);
            marker
        });
        assert!(
            rollback_fresh_lora_add_marker(&state, &config_dir, "Shared Radio", edit_marker)
                .is_none()
        );
        assert!(
            crate::rns_config::read_config(&config_dir)
                .unwrap()
                .contains("[[Shared Radio]]")
        );
    }

    #[test]
    fn stale_same_name_marker_cannot_delete_replacement_config() {
        let temp = tempfile::tempdir().unwrap();
        let config = DashboardConfig::from_env_and_defaults(temp.path().to_path_buf());
        let state = state_for_config(config);
        let config_dir = temp.path().join("rns");
        crate::rns_config::write_config(
            &config_dir,
            "[interfaces]\n  [[Shared Radio]]\n    type = RNodeInterface\n    port = ble://AA:BB:CC:DD:EE:FF\n",
        );

        let marker_a = with_rns_config_lock(&state, || {
            mark_lora_add_freshness(&config_dir, "Shared Radio", true).unwrap()
        });
        let marker_b = with_rns_config_lock(&state, || {
            mark_lora_add_freshness(&config_dir, "Shared Radio", true).unwrap()
        });

        assert!(
            rollback_fresh_lora_add_marker(&state, &config_dir, "Shared Radio", marker_a).is_none()
        );
        assert!(
            crate::rns_config::read_config(&config_dir)
                .unwrap()
                .contains("[[Shared Radio]]")
        );
        assert!(clear_fresh_lora_add_marker(
            &state,
            &config_dir,
            "Shared Radio",
            marker_b,
        ));
    }

    #[tokio::test]
    async fn ble_peer_expiry_only_disables_the_exact_requested_generation() {
        let temp = tempfile::tempdir().unwrap();
        let config = DashboardConfig::from_env_and_defaults(temp.path().to_path_buf());
        let state = Arc::new(state_for_config(config));
        db::set_setting(&state.db, "ble_peer_enabled", "1");
        db::set_setting(&state.db, "ble_peer_expires_at", "200");

        assert!(!disable_ble_peer_inner_if_expiry(&state, 100).await);
        assert_eq!(
            db::get_setting(&state.db, "ble_peer_enabled").as_deref(),
            Some("1")
        );
        assert_eq!(
            db::get_setting(&state.db, "ble_peer_expires_at").as_deref(),
            Some("200")
        );

        assert!(disable_ble_peer_inner_if_expiry(&state, 200).await);
        assert_eq!(
            db::get_setting(&state.db, "ble_peer_enabled").as_deref(),
            Some("0")
        );
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
