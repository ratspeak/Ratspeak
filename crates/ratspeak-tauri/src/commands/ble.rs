//! BLE commands.
//!
//! Convention: BLE commands are always registered with the invoke handler;
//! builds without the `ble` feature stub out internally, so the frontend sees
//! a uniform command surface on every platform. This is intentional —
//! contrast with desktop-only `hardware`, which compile-gates the whole
//! module and its registrations behind the `hardware` feature.

use std::sync::Arc;

#[cfg(feature = "ble")]
use bytes::Bytes;
use serde::Deserialize;
#[cfg(any(feature = "ble", test))]
use serde::Serialize;
use serde_json::{Value, json};
use tauri::State;

use ratspeak_runtime::activity::producer::{
    InterfaceClass, InterfaceDegradationReason, InterfaceFailureReason, InterfaceTimeoutReason,
    InterfaceTransition,
};
#[cfg(feature = "ble")]
use rns_interface::rnode::RNodeStartupOptions;

use crate::commands::interface_activity::record_interface_event as record_interface_activity;
#[cfg(any(feature = "ble", test))]
use crate::commands::rnode_readiness::RnodeReadinessFailure;
#[cfg(feature = "ble")]
use crate::commands::rnode_readiness::{await_spawned_rnode_ready, teardown_spawned_rnode_exact};
#[cfg(feature = "ble")]
use crate::commands::shared::{active_rns_config_dir, emit_hub_interfaces, with_rns_config_lock};
use crate::commands::shared::{
    disable_ble_peer_inner, disable_ble_peer_inner_if_expiry, emit_op_status_broadcast,
};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::sanitize_text;
use crate::state::{ActivityRequestFence, AppState};

#[cfg_attr(not(feature = "ble"), allow(dead_code))]
enum BlePeerActivityOutcome {
    Starting,
    Ready,
    Stopped,
    MissingIdentity,
    RuntimeFailed,
    TimedOut,
    PeripheralUnavailable,
}

fn ble_peer_activity_transition(outcome: BlePeerActivityOutcome) -> InterfaceTransition {
    match outcome {
        BlePeerActivityOutcome::Starting => InterfaceTransition::Connecting,
        BlePeerActivityOutcome::Ready => InterfaceTransition::Online,
        BlePeerActivityOutcome::Stopped => InterfaceTransition::Removed,
        BlePeerActivityOutcome::MissingIdentity => InterfaceTransition::Failed {
            reason: InterfaceFailureReason::Configure,
            rollback: None,
        },
        BlePeerActivityOutcome::RuntimeFailed => InterfaceTransition::Failed {
            reason: InterfaceFailureReason::Runtime,
            rollback: None,
        },
        BlePeerActivityOutcome::TimedOut => InterfaceTransition::TimedOut {
            reason: InterfaceTimeoutReason::Setup,
        },
        BlePeerActivityOutcome::PeripheralUnavailable => InterfaceTransition::Degraded {
            reason: InterfaceDegradationReason::PeripheralUnavailable,
        },
    }
}

#[cfg_attr(not(feature = "ble"), allow(dead_code))]
enum BleRnodeActivityOutcome {
    Ready,
    ConfigureFailed,
    ConnectFailed,
    SetupTimedOut,
    PairingTimedOut,
    StartupTimedOut,
    Cancelled,
    RuntimeFailed,
    Removed,
    RemoveFailed,
}

fn ble_rnode_activity_transition(outcome: BleRnodeActivityOutcome) -> InterfaceTransition {
    match outcome {
        BleRnodeActivityOutcome::Ready => InterfaceTransition::Online,
        BleRnodeActivityOutcome::ConfigureFailed => InterfaceTransition::Failed {
            reason: InterfaceFailureReason::Configure,
            rollback: None,
        },
        BleRnodeActivityOutcome::ConnectFailed => InterfaceTransition::Failed {
            reason: InterfaceFailureReason::Connect,
            rollback: None,
        },
        BleRnodeActivityOutcome::SetupTimedOut => InterfaceTransition::TimedOut {
            reason: InterfaceTimeoutReason::Setup,
        },
        BleRnodeActivityOutcome::PairingTimedOut => InterfaceTransition::TimedOut {
            reason: InterfaceTimeoutReason::Pairing,
        },
        BleRnodeActivityOutcome::StartupTimedOut => InterfaceTransition::TimedOut {
            reason: InterfaceTimeoutReason::Startup,
        },
        BleRnodeActivityOutcome::Cancelled => InterfaceTransition::Cancelled,
        BleRnodeActivityOutcome::RuntimeFailed => InterfaceTransition::Failed {
            reason: InterfaceFailureReason::Runtime,
            rollback: None,
        },
        BleRnodeActivityOutcome::Removed => InterfaceTransition::Removed,
        BleRnodeActivityOutcome::RemoveFailed => InterfaceTransition::Failed {
            reason: InterfaceFailureReason::Remove,
            rollback: None,
        },
    }
}

#[cfg(any(feature = "ble", test))]
fn ble_rnode_readiness_failure_feedback(
    failure: RnodeReadinessFailure,
) -> (&'static str, &'static str, BleRnodeActivityOutcome) {
    match failure {
        RnodeReadinessFailure::Timeout => (
            "RNode startup timed out. Check the radio and try again.",
            "startup_timeout",
            BleRnodeActivityOutcome::StartupTimedOut,
        ),
        RnodeReadinessFailure::ShuttingDown
        | RnodeReadinessFailure::Stopped
        | RnodeReadinessFailure::ObservationClosed
        | RnodeReadinessFailure::Unclassified => (
            "RNode did not become ready. Try connecting again.",
            "readiness_failed",
            BleRnodeActivityOutcome::RuntimeFailed,
        ),
    }
}

type BleRnodeRollbackContext = (std::path::PathBuf, String, u64);

#[cfg_attr(not(feature = "ble"), allow(dead_code))]
fn clear_ble_rnode_rollback_context(
    state: &AppState,
    rollback_context: Option<BleRnodeRollbackContext>,
) {
    if let Some((config_dir, name, marker)) = rollback_context {
        crate::commands::shared::clear_fresh_lora_add_marker(state, &config_dir, &name, marker);
    }
}

fn rollback_ble_rnode_context(state: &AppState, rollback_context: Option<BleRnodeRollbackContext>) {
    if let Some((config_dir, name, marker)) = rollback_context {
        let _ = crate::commands::shared::rollback_fresh_lora_add_marker(
            state,
            &config_dir,
            &name,
            marker,
        );
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        crate::commands::shared::emit_hub_interfaces(state, ifaces);
    }
}

fn complete_waiting_ble_rnode_operation(
    completion: Option<crate::state::BleRnodeOperationCompletionSender>,
    result: crate::state::BleRnodeOperationResult,
) -> bool {
    let Some(completion) = completion else {
        return false;
    };
    let _ = completion.send(result);
    true
}

#[cfg(target_os = "android")]
fn disconnect_native_ble_rnode_operation(state: &AppState, activity_operation: &str) {
    state.emit_to_all(
        "ble_rnode_disconnect_native",
        json!({ "activity_operation": activity_operation }),
    );
}

/// Relay typed BLE pairing/product events. Call once per process.
///
/// The lower layer's raw `ble_diag` strings may include device names,
/// addresses, URIs, and platform errors, so they must not cross Tauri IPC.
#[cfg(feature = "ble")]
pub fn spawn_ble_event_broadcaster(_state: &Arc<AppState>) {
    // Linux-only: BlueZ Agent passkey prompts → frontend modal.
    // `attempt_id` lets the UI dedupe stale prompts.
    #[cfg(all(feature = "ble", target_os = "linux"))]
    {
        let state_pairing = Arc::clone(_state);
        tokio::spawn(async move {
            let mut rx = rns_interface::ble_rnode::subscribe_linux_pairing_prompts();
            loop {
                match rx.recv().await {
                    Ok(prompt) => {
                        state_pairing.emit_to_all(
                            "ble_rnode_passkey_prompt",
                            json!({
                                "device": prompt.device,
                                "attempt_id": prompt.attempt_id,
                            }),
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "passkey prompt relay lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Linux-only: pair-attempt completion → frontend modal dismiss.
        let state_finished = Arc::clone(_state);
        tokio::spawn(async move {
            let mut rx = rns_interface::ble_rnode::subscribe_linux_pairing_finished();
            loop {
                match rx.recv().await {
                    Ok(done) => {
                        state_finished.emit_to_all(
                            "ble_rnode_pairing_finished",
                            json!({
                                "attempt_id": done.attempt_id,
                                "status": done.status,
                            }),
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

#[cfg(not(feature = "ble"))]
pub fn spawn_ble_event_broadcaster(_state: &Arc<AppState>) {}

/// Linux: deliver passkey to bluer Agent. No-op on Apple/Windows (OS dialog).
#[tauri::command]
pub async fn submit_ble_rnode_passkey(_passkey: u32) -> AppResult<Value> {
    #[cfg(all(feature = "ble", target_os = "linux"))]
    {
        if rns_interface::ble_rnode::linux_submit_passkey(_passkey) {
            return Ok(json!({ "ok": true }));
        }
        Err(AppError::not_found(
            "No BLE pairing in progress".to_string(),
        ))
    }
    #[cfg(not(all(feature = "ble", target_os = "linux")))]
    {
        Ok(json!({ "ok": true, "noop": true }))
    }
}

/// Linux: cancel in-flight bonding so bluer rejects fast. No-op elsewhere.
#[tauri::command]
pub async fn cancel_ble_rnode_pairing() -> AppResult<Value> {
    #[cfg(all(feature = "ble", target_os = "linux"))]
    rns_interface::ble_rnode::linux_cancel_pairing();
    Ok(json!({ "ok": true }))
}

#[derive(Deserialize, Default)]
pub struct EnableBlePeerArgs {
    #[serde(default)]
    pub duration: u64,
}

const BLE_PEER_ENABLED_SETTING: &str = "ble_peer_enabled";
const BLE_PEER_EXPIRES_AT_SETTING: &str = "ble_peer_expires_at";
#[cfg(any(feature = "ble", test))]
const BLE_RECENT_DISCONNECTS_SETTING: &str = "ble_recent_disconnects";
#[cfg(any(feature = "ble", test))]
const BLE_RECENT_DISCONNECTS_V2_SETTING: &str = "ble_recent_disconnects_v2";
#[cfg(any(feature = "ble", test))]
const BLE_RECENT_DISCONNECTS_LIMIT: usize = 50;

#[cfg(any(feature = "ble", test))]
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct BleRecentDisconnectRecord {
    address: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    identity_hash: String,
    #[serde(default)]
    disconnected_at: u64,
}

#[cfg(any(feature = "ble", test))]
fn is_valid_identity_hash_hex(value: &str) -> bool {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    hex::decode(value)
        .map(|bytes| bytes.len() == 16 && bytes.iter().any(|b| *b != 0))
        .unwrap_or(false)
}

#[cfg(any(feature = "ble", test))]
fn normalize_ble_address(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(any(feature = "ble", test))]
fn normalize_ble_recent_disconnect_record(
    mut record: BleRecentDisconnectRecord,
) -> Option<BleRecentDisconnectRecord> {
    record.address = normalize_ble_address(&record.address)?;
    record.identity_hash = record.identity_hash.trim().to_ascii_lowercase();
    if !is_valid_identity_hash_hex(&record.identity_hash) {
        record.identity_hash.clear();
    }
    Some(record)
}

#[cfg(any(feature = "ble", test))]
fn ble_recent_disconnect_seed_addresses(
    v2_json: Option<&str>,
    legacy_json: Option<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(records) =
        v2_json.and_then(|v2| serde_json::from_str::<Vec<BleRecentDisconnectRecord>>(v2).ok())
    {
        for record in records {
            if let Some(record) = normalize_ble_recent_disconnect_record(record) {
                if !out.iter().any(|address| address == &record.address) {
                    out.push(record.address);
                }
            }
            if out.len() >= BLE_RECENT_DISCONNECTS_LIMIT {
                return out;
            }
        }
    }

    if let Some(values) =
        legacy_json.and_then(|legacy| serde_json::from_str::<Vec<String>>(legacy).ok())
    {
        for value in values {
            if is_valid_identity_hash_hex(value.trim()) {
                continue;
            }
            if let Some(address) = normalize_ble_address(&value) {
                if !out.iter().any(|existing| existing == &address) {
                    out.push(address);
                }
            }
            if out.len() >= BLE_RECENT_DISCONNECTS_LIMIT {
                break;
            }
        }
    }
    out
}

#[cfg(any(feature = "ble", test))]
fn update_ble_recent_disconnect_records(
    mut records: Vec<BleRecentDisconnectRecord>,
    address: String,
    identity_hash: Option<String>,
    disconnected_at: u64,
) -> Vec<BleRecentDisconnectRecord> {
    let Some(address) = normalize_ble_address(&address) else {
        return records;
    };
    let identity_hash = identity_hash
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| is_valid_identity_hash_hex(value))
        .unwrap_or_default();

    records = records
        .into_iter()
        .filter_map(normalize_ble_recent_disconnect_record)
        .filter(|record| {
            record.address != address
                && (identity_hash.is_empty() || record.identity_hash != identity_hash)
        })
        .collect();

    records.insert(
        0,
        BleRecentDisconnectRecord {
            address,
            identity_hash,
            disconnected_at,
        },
    );
    records.truncate(BLE_RECENT_DISCONNECTS_LIMIT);
    records
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn ble_peer_expires_at_for_duration(duration_secs: u64) -> u64 {
    if duration_secs == 0 {
        0
    } else {
        now_unix_secs().saturating_add(duration_secs)
    }
}

fn ble_peer_remaining_secs(expires_at: u64, now: u64) -> Option<u64> {
    if expires_at == 0 {
        Some(0)
    } else {
        expires_at
            .checked_sub(now)
            .filter(|remaining| *remaining > 0)
    }
}

fn clear_ble_peer_requested_state(state: &Arc<AppState>) {
    let db = state.db.clone();
    tokio::spawn(async move {
        let _ = db::spawn_db(db, |p| {
            db::set_setting(&p, BLE_PEER_ENABLED_SETTING, "0");
            db::set_setting(&p, BLE_PEER_EXPIRES_AT_SETTING, "0");
        })
        .await;
    });
}

#[cfg(feature = "ble")]
fn is_ble_peer_interface_name(name: &str) -> bool {
    name == "Bluetooth Peer" || name == "BLE Mesh"
}

#[cfg(feature = "ble")]
async fn live_ble_peer_interface_id(
    handle: &rns_runtime::reticulum::ReticulumHandle,
) -> Option<u64> {
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if handle
        .transport_tx
        .send(rns_transport::messages::TransportMessage::Rpc {
            query: rns_transport::messages::TransportQuery::GetInterfaceStats,
            response_tx: resp_tx,
        })
        .await
        .is_err()
    {
        return None;
    }
    match resp_rx.await.ok()? {
        rns_transport::messages::TransportQueryResponse::InterfaceStats(stats) => stats
            .into_iter()
            .find(|iface| is_ble_peer_interface_name(&iface.name))
            .map(|iface| iface.id),
        _ => None,
    }
}

#[cfg_attr(not(feature = "ble"), allow(dead_code))]
fn emit_ble_peer_enabled_status(state: &Arc<AppState>) {
    let peer_count = state
        .ble_peer_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let state_name = if peer_count > 0 { "on" } else { "starting" };
    state.emit_to_all(
        "ble_peer_status_changed",
        json!({
            "state": state_name,
            "peer_count": peer_count,
        }),
    );
}

#[cfg_attr(not(feature = "ble"), allow(dead_code))]
async fn persist_ble_peer_requested_state(state: &Arc<AppState>, expires_at: u64) {
    let db = state.db.clone();
    let _ = db::spawn_db(db, move |p| {
        db::set_setting(&p, BLE_PEER_ENABLED_SETTING, "1");
        db::set_setting(&p, BLE_PEER_EXPIRES_AT_SETTING, &expires_at.to_string());
    })
    .await;
    state.emit_to_all("ble_peer_status_update", json!({ "enabled": true }));
    emit_ble_peer_enabled_status(state);
}

#[cfg_attr(not(feature = "ble"), allow(dead_code))]
fn schedule_ble_peer_expiry(
    state: &Arc<AppState>,
    activity_fence: ActivityRequestFence,
    duration_secs: u64,
    expires_at: u64,
) {
    if duration_secs == 0 || expires_at == 0 {
        return;
    }

    let state3: Arc<AppState> = Arc::clone(state);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(duration_secs)).await;
        let changed = disable_ble_peer_inner_if_expiry(&state3, expires_at).await;
        if changed {
            record_interface_activity(
                &state3,
                activity_fence,
                InterfaceClass::BluetoothPeer,
                ble_peer_activity_transition(BlePeerActivityOutcome::Stopped),
                None,
            );
        }
    });
}

#[tauri::command]
pub async fn enable_ble_peer_interface(
    state: State<'_, Arc<AppState>>,
    args: EnableBlePeerArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let activity_fence = state_arc.activity_request_fence();
    let duration_secs = args.duration;
    let expires_at = ble_peer_expires_at_for_duration(duration_secs);

    spawn_enable_ble_peer_task(state_arc, activity_fence, duration_secs, expires_at);
    Ok(json!({ "queued": true }))
}

#[cfg_attr(not(feature = "ble"), allow(unused_variables))]
fn spawn_enable_ble_peer_task(
    state_arc: Arc<AppState>,
    activity_fence: ActivityRequestFence,
    duration_secs: u64,
    expires_at: u64,
) {
    // Mark `ble_peer_enabled=1` only after spawn success.
    tokio::spawn(async move {
        let _enable_guard = state_arc.ble_peer_enable_lock.lock().await;
        let _rns_handle = state_arc
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));

        #[cfg(feature = "ble")]
        if let Some(handle) = _rns_handle {
            if let Some(id) = live_ble_peer_interface_id(&handle).await {
                persist_ble_peer_requested_state(&state_arc, expires_at).await;
                schedule_ble_peer_expiry(&state_arc, activity_fence, duration_secs, expires_at);
                tracing::info!(
                    interface_id = id,
                    duration_secs,
                    expires_at,
                    "Bluetooth Peer enable request reused existing interface"
                );
                emit_op_status_broadcast(
                    &state_arc,
                    "enable_ble_peer",
                    "hub",
                    "Bluetooth Peer already enabled",
                    true,
                    None,
                );
                return;
            }

            // LXMF is source of truth; fall back to DB on startup race.
            let from_lxmf: Option<String> = state_arc
                .lxmf
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|mgr| mgr.identity_hash.clone()));

            let (identity_hash, seed_addresses) = db::spawn_db(state_arc.db.clone(), move |p| {
                let hash_hex = from_lxmf
                    .filter(|h| !h.is_empty())
                    .or_else(|| {
                        db::get_active_identity(&p).and_then(|v| {
                            v.get("hash")
                                .and_then(|s| s.as_str())
                                .map(|s| s.to_string())
                        })
                    })
                    .unwrap_or_default();
                let id = hex::decode(&hash_hex).unwrap_or_default();
                let recent_v2 = db::get_setting(&p, BLE_RECENT_DISCONNECTS_V2_SETTING);
                let recent_legacy = db::get_setting(&p, BLE_RECENT_DISCONNECTS_SETTING);
                let seed = ble_recent_disconnect_seed_addresses(
                    recent_v2.as_deref(),
                    recent_legacy.as_deref(),
                );
                tracing::info!(
                    hash_hex_len = hash_hex.len(),
                    decoded_len = id.len(),
                    seed_address_count = seed.len(),
                    "Bluetooth Peer enable: resolved active identity"
                );
                (id, seed)
            })
            .await
            .expect("db task panicked");

            // Zero/missing identity → Android startAdvertising SecurityException.
            if !rns_interface::ble_peer::is_valid_identity_hash(&identity_hash) {
                let _ = db::spawn_db(state_arc.db.clone(), |p| {
                    db::set_setting(&p, BLE_PEER_ENABLED_SETTING, "0");
                    db::set_setting(&p, BLE_PEER_EXPIRES_AT_SETTING, "0");
                })
                .await;
                state_arc.emit_to_all("ble_peer_status_update", json!({ "enabled": false }));
                emit_op_status_broadcast(
                    &state_arc,
                    "enable_ble_peer",
                    "hub",
                    "Bluetooth Peer requires an active identity",
                    true,
                    Some(
                        "No active identity is configured. Create or select one in Settings → Identity, then try again.",
                    ),
                );
                record_interface_activity(
                    &state_arc,
                    activity_fence,
                    InterfaceClass::BluetoothPeer,
                    ble_peer_activity_transition(BlePeerActivityOutcome::MissingIdentity),
                    None,
                );
                return;
            }

            let (event_tx, mut event_rx) =
                tokio::sync::mpsc::channel::<rns_interface::ble_peer::BlePeerEvent>(1024);

            record_interface_activity(
                &state_arc,
                activity_fence,
                InterfaceClass::BluetoothPeer,
                ble_peer_activity_transition(BlePeerActivityOutcome::Starting),
                None,
            );

            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                rns_runtime::reticulum::spawn_ble_peer_runtime(
                    &handle,
                    "Bluetooth Peer",
                    identity_hash,
                    Some(event_tx),
                    state_arc.foreground_changed.clone(),
                    seed_addresses,
                ),
            )
            .await
            {
                Ok(Ok(_id)) => {
                    state_arc
                        .ble_peer_count
                        .store(0, std::sync::atomic::Ordering::Relaxed);
                    persist_ble_peer_requested_state(&state_arc, expires_at).await;
                    record_interface_activity(
                        &state_arc,
                        activity_fence,
                        InterfaceClass::BluetoothPeer,
                        ble_peer_activity_transition(BlePeerActivityOutcome::Ready),
                        None,
                    );

                    let state_relay: Arc<AppState> = Arc::clone(&state_arc);
                    tokio::spawn(async move {
                        use rns_interface::ble_peer::BlePeerEvent;
                        // Disconnected events lack identity; track per-address.
                        let mut address_to_identity: std::collections::HashMap<String, String> =
                            std::collections::HashMap::new();
                        let mut peripheral_degradation_recorded = false;
                        fn logical_ble_peer_count(
                            address_to_identity: &std::collections::HashMap<String, String>,
                        ) -> usize {
                            let mut identities = std::collections::HashSet::new();
                            let mut unidentified = 0usize;
                            for identity in address_to_identity.values() {
                                if identity.is_empty() {
                                    unidentified += 1;
                                } else {
                                    identities.insert(identity.as_str());
                                }
                            }
                            identities.len() + unidentified
                        }

                        fn store_logical_ble_peer_count(
                            state: &AppState,
                            address_to_identity: &std::collections::HashMap<String, String>,
                        ) -> usize {
                            let peer_count = logical_ble_peer_count(address_to_identity);
                            state
                                .ble_peer_count
                                .store(peer_count, std::sync::atomic::Ordering::Relaxed);
                            // Mirror into AppState so api_ble_peer_status can hand
                            // the current peer rows back after a webview reload.
                            if let Ok(mut peers) = state.ble_peers.lock() {
                                *peers = address_to_identity
                                    .iter()
                                    .map(|(a, i)| (a.clone(), i.clone()))
                                    .collect();
                            }
                            peer_count
                        }

                        fn emit_logical_ble_peer_status(
                            state: &AppState,
                            address_to_identity: &std::collections::HashMap<String, String>,
                        ) {
                            let peer_count =
                                store_logical_ble_peer_count(state, address_to_identity);
                            let state_name = if peer_count > 0 {
                                rns_interface::ble_peer::PeerState::On
                            } else {
                                rns_interface::ble_peer::PeerState::Starting
                            };
                            state.emit_to_all(
                                "ble_peer_status_changed",
                                json!({
                                    "state": state_name,
                                    "peer_count": peer_count,
                                }),
                            );
                        }

                        while let Some(ev) = event_rx.recv().await {
                            match ev {
                                BlePeerEvent::Discovered {
                                    address,
                                    rssi,
                                    protocol,
                                } => {
                                    state_relay.emit_to_all(
                                        "ble_peer_discovered",
                                        json!({
                                            "address": address,
                                            "rssi": rssi,
                                            "protocol": protocol,
                                        }),
                                    );
                                }
                                BlePeerEvent::Connected {
                                    address,
                                    identity_hash,
                                    protocol,
                                } => {
                                    address_to_identity
                                        .insert(address.clone(), identity_hash.clone());
                                    emit_logical_ble_peer_status(
                                        &state_relay,
                                        &address_to_identity,
                                    );
                                    state_relay.emit_to_all(
                                        "ble_peer_connected",
                                        json!({
                                            "address": address,
                                            "identity_hash": identity_hash,
                                            "protocol": protocol,
                                        }),
                                    );
                                }
                                BlePeerEvent::Disconnected { address, reason } => {
                                    let identity_hash = address_to_identity
                                        .remove(&address)
                                        .filter(|value| is_valid_identity_hash_hex(value));
                                    if !address.is_empty() {
                                        let db = state_relay.db.clone();
                                        let address_for_persist = address.clone();
                                        let disconnected_at = now_unix_secs();
                                        tokio::spawn(async move {
                                            let _ = db::spawn_db(db, move |p| {
                                                let records = db::get_setting(
                                                    &p,
                                                    BLE_RECENT_DISCONNECTS_V2_SETTING,
                                                )
                                                .and_then(|v| {
                                                    serde_json::from_str::<
                                                        Vec<BleRecentDisconnectRecord>,
                                                    >(
                                                        &v
                                                    )
                                                    .ok()
                                                })
                                                .unwrap_or_default();
                                                let records = update_ble_recent_disconnect_records(
                                                    records,
                                                    address_for_persist,
                                                    identity_hash,
                                                    disconnected_at,
                                                );
                                                if let Ok(json) = serde_json::to_string(&records) {
                                                    db::set_setting(
                                                        &p,
                                                        BLE_RECENT_DISCONNECTS_V2_SETTING,
                                                        &json,
                                                    );
                                                }
                                                let addresses = records
                                                    .iter()
                                                    .map(|record| record.address.clone())
                                                    .collect::<Vec<_>>();
                                                if let Ok(json) = serde_json::to_string(&addresses)
                                                {
                                                    db::set_setting(
                                                        &p,
                                                        BLE_RECENT_DISCONNECTS_SETTING,
                                                        &json,
                                                    );
                                                }
                                            })
                                            .await;
                                        });
                                    }
                                    emit_logical_ble_peer_status(
                                        &state_relay,
                                        &address_to_identity,
                                    );
                                    state_relay.emit_to_all(
                                        "ble_peer_disconnected",
                                        json!({
                                            "address": address,
                                            "reason": reason,
                                        }),
                                    );
                                }
                                BlePeerEvent::IdentityResolved {
                                    address,
                                    identity_hash,
                                } => {
                                    // Disconnect path persists recent reconnect records from this map.
                                    address_to_identity
                                        .insert(address.clone(), identity_hash.clone());
                                    emit_logical_ble_peer_status(
                                        &state_relay,
                                        &address_to_identity,
                                    );
                                    state_relay.emit_to_all(
                                        "ble_peer_identity_resolved",
                                        json!({
                                            "address": address,
                                            "identity_hash": identity_hash,
                                        }),
                                    );
                                }
                                BlePeerEvent::RssiUpdate { address, rssi } => {
                                    state_relay.emit_to_all(
                                        "ble_peer_rssi",
                                        json!({ "address": address, "rssi": rssi }),
                                    );
                                }
                                BlePeerEvent::PeripheralUnavailable { reason } => {
                                    if !peripheral_degradation_recorded {
                                        peripheral_degradation_recorded = true;
                                        record_interface_activity(
                                            &state_relay,
                                            activity_fence,
                                            InterfaceClass::BluetoothPeer,
                                            ble_peer_activity_transition(
                                                BlePeerActivityOutcome::PeripheralUnavailable,
                                            ),
                                            None,
                                        );
                                    }
                                    state_relay.emit_to_all(
                                        "ble_peer_peripheral_unavailable",
                                        json!({ "reason": reason }),
                                    );
                                }
                                BlePeerEvent::StatusChanged { state, peer_count } => {
                                    state_relay.emit_to_all(
                                        "ble_peer_status_changed",
                                        json!({
                                            "state": state,
                                            "peer_count": peer_count,
                                        }),
                                    );
                                }
                                BlePeerEvent::SubscribeReady { .. } => {
                                    // Kick-announce so the peer learns our identity.
                                    let (packet, transport_tx, dest_hash) = {
                                        let pkt = if let Ok(mut lxmf) = state_relay.lxmf.lock() {
                                            lxmf.as_mut()
                                                .and_then(|mgr| mgr.create_announce_packet().ok())
                                        } else {
                                            None
                                        };
                                        let tx = state_relay.rns.read().ok().and_then(|r| {
                                            r.as_ref().map(|mgr| mgr.handle.transport_tx.clone())
                                        });
                                        let dh = if let Ok(lxmf) = state_relay.lxmf.lock() {
                                            lxmf.as_ref().map(|mgr| mgr.lxmf_dest_hash)
                                        } else {
                                            None
                                        };
                                        (pkt, tx, dh)
                                    };
                                    if let (Some(raw), Some(tx), Some(dh)) =
                                        (packet, transport_tx, dest_hash)
                                    {
                                        tokio::spawn(async move {
                                            match tx
                                                .send(
                                                    rns_transport::messages::TransportMessage::Outbound(
                                                        rns_transport::messages::OutboundRequest {
                                                            raw: Bytes::from(raw),
                                                            destination_hash: dh,
                                                        },
                                                    ),
                                                )
                                                .await
                                            {
                                                Ok(_) => tracing::info!(
                                                    "Bluetooth Peer kick-announce sent on peer subscribe"
                                                ),
                                                Err(_) => tracing::warn!(
                                                    reason = "announce_failed",
                                                    "Bluetooth Peer kick-announce failed"
                                                ),
                                            }
                                        });
                                    } else {
                                        tracing::debug!(
                                            reason = "runtime_not_ready",
                                            "Bluetooth Peer kick-announce skipped (RNS or LXMF not initialized)"
                                        );
                                    }
                                }
                            }
                        }
                        tracing::debug!("BLE peer event relay task exited");
                    });

                    emit_op_status_broadcast(
                        &state_arc,
                        "enable_ble_peer",
                        "hub",
                        "Bluetooth Peer enabled",
                        true,
                        None,
                    );
                    schedule_ble_peer_expiry(&state_arc, activity_fence, duration_secs, expires_at);
                }
                Ok(Err(e)) => {
                    let _ = db::spawn_db(state_arc.db.clone(), |p| {
                        db::set_setting(&p, BLE_PEER_ENABLED_SETTING, "0");
                        db::set_setting(&p, BLE_PEER_EXPIRES_AT_SETTING, "0");
                    })
                    .await;
                    state_arc.emit_to_all("ble_peer_status_update", json!({ "enabled": false }));
                    emit_op_status_broadcast(
                        &state_arc,
                        "enable_ble_peer",
                        "hub",
                        "Bluetooth Peer failed to start",
                        true,
                        Some(&e),
                    );
                    record_interface_activity(
                        &state_arc,
                        activity_fence,
                        InterfaceClass::BluetoothPeer,
                        ble_peer_activity_transition(BlePeerActivityOutcome::RuntimeFailed),
                        None,
                    );
                }
                Err(_) => {
                    // The spawn future was cancelled by the timeout, but it may
                    // have already started advertising + spawned loops (those
                    // are detached, so dropping the future doesn't stop them).
                    // Tear the half-started session down so it can't keep
                    // advertising while the app reports BLE off. Call the
                    // low-level stop directly — disable_ble_peer_inner would
                    // deadlock on the enable lock we hold here.
                    #[cfg(feature = "ble")]
                    rns_interface::ble_peer::stop_ble_peer_interface().await;
                    let _ = db::spawn_db(state_arc.db.clone(), |p| {
                        db::set_setting(&p, BLE_PEER_ENABLED_SETTING, "0");
                        db::set_setting(&p, BLE_PEER_EXPIRES_AT_SETTING, "0");
                    })
                    .await;
                    state_arc.emit_to_all("ble_peer_status_update", json!({ "enabled": false }));
                    emit_op_status_broadcast(
                        &state_arc,
                        "enable_ble_peer",
                        "hub",
                        "Bluetooth Peer timed out",
                        true,
                        Some("Bluetooth Peer spawn timed out; check Bluetooth permissions"),
                    );
                    record_interface_activity(
                        &state_arc,
                        activity_fence,
                        InterfaceClass::BluetoothPeer,
                        ble_peer_activity_transition(BlePeerActivityOutcome::TimedOut),
                        None,
                    );
                }
            }
        } else {
            clear_ble_peer_requested_state(&state_arc);
            state_arc.emit_to_all("ble_peer_status_update", json!({ "enabled": false }));
            emit_op_status_broadcast(
                &state_arc,
                "enable_ble_peer",
                "hub",
                "Bluetooth Peer failed to start",
                true,
                Some("RNS is not initialized yet"),
            );
            record_interface_activity(
                &state_arc,
                activity_fence,
                InterfaceClass::BluetoothPeer,
                ble_peer_activity_transition(BlePeerActivityOutcome::RuntimeFailed),
                None,
            );
        }
        #[cfg(not(feature = "ble"))]
        {
            clear_ble_peer_requested_state(&state_arc);
            state_arc.emit_to_all("ble_peer_status_update", json!({ "enabled": false }));
            emit_op_status_broadcast(
                &state_arc,
                "enable_ble_peer",
                "hub",
                "BLE not available (feature not compiled)",
                true,
                Some("BLE feature not compiled"),
            );
            record_interface_activity(
                &state_arc,
                activity_fence,
                InterfaceClass::BluetoothPeer,
                ble_peer_activity_transition(BlePeerActivityOutcome::RuntimeFailed),
                None,
            );
        }
    });
}

pub(crate) async fn restore_ble_peer_if_requested(state: Arc<AppState>) {
    let (enabled, expires_at) = db::spawn_db(state.db.clone(), |p| {
        let enabled = db::get_setting(&p, BLE_PEER_ENABLED_SETTING)
            .map(|v| v == "1")
            .unwrap_or(false);
        let expires_at = db::get_setting(&p, BLE_PEER_EXPIRES_AT_SETTING)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        (enabled, expires_at)
    })
    .await
    .unwrap_or((false, 0));

    if !enabled {
        return;
    }

    let Some(duration_secs) = ble_peer_remaining_secs(expires_at, now_unix_secs()) else {
        tracing::info!("Bluetooth Peer saved enable request expired before startup restore");
        clear_ble_peer_requested_state(&state);
        state.emit_to_all("ble_peer_status_update", json!({ "enabled": false }));
        return;
    };

    let rns_ready = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|_| ()))
        .is_some();
    if !rns_ready {
        tracing::debug!("Bluetooth Peer restore deferred; RNS is not initialized");
        return;
    }

    tracing::info!(
        duration_secs,
        expires_at,
        "restoring persisted Bluetooth Peer interface request"
    );
    let activity_fence = state.activity_request_fence();
    spawn_enable_ble_peer_task(state, activity_fence, duration_secs, expires_at);
}

#[tauri::command]
pub async fn disable_ble_peer_interface(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let activity_fence = state_arc.activity_request_fence();
    tokio::spawn(async move {
        let changed = disable_ble_peer_inner(&state_arc).await;
        emit_op_status_broadcast(
            &state_arc,
            "disable_ble_peer",
            "hub",
            "Bluetooth Peer disabled",
            true,
            None,
        );
        if changed {
            record_interface_activity(
                &state_arc,
                activity_fence,
                InterfaceClass::BluetoothPeer,
                ble_peer_activity_transition(BlePeerActivityOutcome::Stopped),
                None,
            );
        }
    });
    Ok(json!({ "queued": true }))
}

#[tauri::command]
pub async fn disconnect_ble_peer(
    state: State<'_, Arc<AppState>>,
    address: String,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    if address.is_empty() {
        emit_op_status_broadcast(
            &state_arc,
            "disconnect_ble_peer",
            "hub",
            "Missing peer address",
            true,
            Some("address required"),
        );
        return Err(AppError::bad_request("address required"));
    }
    let address_clone = address.clone();
    tokio::spawn(async move {
        // Cross-platform now (was Android-only, so the UI reported success on
        // desktop/Apple while doing nothing).
        #[cfg(feature = "ble")]
        rns_interface::ble_peer::disconnect_mesh_peer(&address_clone).await;
        #[cfg(not(feature = "ble"))]
        let _ = &address_clone;
        emit_op_status_broadcast(
            &state_arc,
            "disconnect_ble_peer",
            "hub",
            &format!("Disconnect requested for {address}"),
            true,
            None,
        );
    });
    Ok(json!({ "queued": true }))
}

#[tauri::command]
pub async fn scan_ble_mesh_peers(_state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    #[cfg(feature = "ble")]
    {
        match rns_interface::ble_peer::scan_mesh_peers(5).await {
            Ok(peers) => Ok(json!({ "peers": peers })),
            Err(e) => Ok(json!({ "peers": [], "error": e })),
        }
    }
    #[cfg(not(feature = "ble"))]
    Ok(json!({ "peers": [], "error": "ble feature not compiled" }))
}

#[tauri::command]
pub async fn scan_ble_devices(_state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    #[cfg(feature = "ble")]
    {
        match rns_interface::ble_rnode::scan_ble_devices(5).await {
            Ok(devices) => Ok(json!({ "devices": devices })),
            Err(e) => Ok(json!({ "devices": [], "error": e })),
        }
    }
    #[cfg(not(feature = "ble"))]
    Ok(json!({ "devices": [], "error": "ble feature not compiled" }))
}

#[derive(Deserialize)]
pub struct BleRnodeBridgeArgs {
    #[serde(default)]
    pub activity_operation: String,
    pub tcp_port: u16,
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub port: String,
    #[serde(default = "default_freq")]
    pub frequency: u64,
    #[serde(default = "default_bw")]
    pub bandwidth: u64,
    #[serde(default = "default_sf")]
    pub spreading_factor: u8,
    #[serde(default = "default_cr")]
    pub coding_rate: u8,
    #[serde(default = "default_tx")]
    pub tx_power: i8,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub airtime_limit_short: Option<f64>,
    #[serde(default)]
    pub airtime_limit_long: Option<f64>,
}

fn default_name() -> String {
    "LoRa".to_string()
}
fn default_freq() -> u64 {
    915_000_000
}
fn default_bw() -> u64 {
    125_000
}
fn default_sf() -> u8 {
    7
}
fn default_cr() -> u8 {
    5
}
fn default_tx() -> i8 {
    14
}

/// Called once the Kotlin BLE bridge TCP socket accepts KISS framing.
#[tauri::command]
pub async fn ble_rnode_bridge_ready(
    state: State<'_, Arc<AppState>>,
    args: BleRnodeBridgeArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let activity_operation = args.activity_operation.clone();
    let activity_fence = state_arc
        .claim_ble_rnode_activity_operation(&activity_operation)
        .ok_or_else(|| AppError::bad_request("Unknown or expired BLE bridge operation"))?;
    let tcp_port = args.tcp_port;
    let name = sanitize_text(&args.name, 64);
    let port = sanitize_text(&args.port, 256);
    let frequency = args.frequency;
    let bandwidth = args.bandwidth;
    let sf = args.spreading_factor;
    let cr = args.coding_rate;
    let tx = args.tx_power;
    let Some(mode) = crate::rns_config::rnode_interface_mode_value(args.mode.as_deref()) else {
        #[cfg(target_os = "android")]
        disconnect_native_ble_rnode_operation(&state_arc, &activity_operation);
        if let Some((terminal_fence, rollback_context, completion)) = state_arc
            .take_initializing_ble_rnode_activity_operation_with_completion(&activity_operation)
        {
            if !complete_waiting_ble_rnode_operation(
                completion,
                crate::state::BleRnodeOperationResult::Failed(
                    crate::state::BleRnodeOperationFailure::Setup,
                ),
            ) {
                rollback_ble_rnode_context(&state_arc, rollback_context);
                record_interface_activity(
                    &state_arc,
                    terminal_fence,
                    InterfaceClass::RNode,
                    ble_rnode_activity_transition(BleRnodeActivityOutcome::ConfigureFailed),
                    None,
                );
            }
        }
        return Err(AppError::bad_request("Invalid RNode interface mode"));
    };
    // Range-validated at add_lora time; clamp here as belt-and-braces.
    let st_alock = args
        .airtime_limit_short
        .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
        .map(|v| v as f32);
    let lt_alock = args
        .airtime_limit_long
        .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
        .map(|v| v as f32);

    if tcp_port == 0 {
        #[cfg(target_os = "android")]
        disconnect_native_ble_rnode_operation(&state_arc, &activity_operation);
        if let Some((terminal_fence, rollback_context, completion)) = state_arc
            .take_initializing_ble_rnode_activity_operation_with_completion(&activity_operation)
        {
            if !complete_waiting_ble_rnode_operation(
                completion,
                crate::state::BleRnodeOperationResult::Failed(
                    crate::state::BleRnodeOperationFailure::Setup,
                ),
            ) {
                rollback_ble_rnode_context(&state_arc, rollback_context);
                emit_op_status_broadcast(
                    &state_arc,
                    "add_lora",
                    "hub",
                    "Invalid TCP bridge port",
                    true,
                    Some("port=0"),
                );
                record_interface_activity(
                    &state_arc,
                    terminal_fence,
                    InterfaceClass::RNode,
                    ble_rnode_activity_transition(BleRnodeActivityOutcome::ConnectFailed),
                    None,
                );
            }
        }
        return Err(AppError::bad_request("Invalid TCP bridge port"));
    }

    #[cfg(feature = "ble")]
    {
        tokio::spawn(async move {
            // The opaque operation token gates product work independently of
            // Activity capture. Hold the identity transition lock only across
            // the runtime spawn so a consumed token cannot race a teardown.
            let spawn_result = {
                let _identity_guard = state_arc.identity_switch_lock.lock().await;
                if state_arc.current_identity_session_generation()
                    != activity_fence.identity_session_generation()
                {
                    if let Some((_, rollback_context, completion)) = state_arc
                        .take_initializing_ble_rnode_activity_operation_with_completion(
                            &activity_operation,
                        )
                    {
                        if !complete_waiting_ble_rnode_operation(
                            completion,
                            crate::state::BleRnodeOperationResult::Failed(
                                crate::state::BleRnodeOperationFailure::Cancelled,
                            ),
                        ) {
                            rollback_ble_rnode_context(&state_arc, rollback_context);
                        }
                    }
                    #[cfg(target_os = "android")]
                    disconnect_native_ble_rnode_operation(&state_arc, &activity_operation);
                    return;
                }
                emit_op_status_broadcast(
                    &state_arc,
                    "add_lora",
                    "hub",
                    "BLE connected, initializing RNode...",
                    false,
                    None,
                );
                let rnode_context = state_arc.rnode_activity_runtime_context_for_identity(
                    activity_fence.identity_session_generation(),
                );
                match rnode_context {
                    Some(rnode_context) => {
                        let rns = rnode_context.handle().clone();
                        let rnode_activity_origin = rnode_context.origin();
                        let result = rns_runtime::reticulum::spawn_ble_rnode_runtime_native_observed_with_options(
                                &rns,
                                rns_runtime::reticulum::BleRnodeRuntimeArgs {
                                    name: &name,
                                    port: &port,
                                    frequency: frequency as u32,
                                    bandwidth: bandwidth as u32,
                                    spreading_factor: sf,
                                    coding_rate: cr,
                                    tx_power: tx,
                                    mode,
                                    st_alock,
                                    lt_alock,
                                    flow_control: true,
                                },
                                tcp_port,
                                RNodeStartupOptions::require_capability_admission(),
                            )
                            .await
                            .map_err(|error| error.to_string());
                        Some((rns, rnode_activity_origin, result))
                    }
                    None => None,
                }
            };

            match spawn_result {
                Some((rns, rnode_activity_origin, Ok(spawned))) => {
                    // Keep cancellation/replacement responsive while the exact
                    // observer performs its bounded, reconnect-aware wait.
                    let readiness_result = {
                        let readiness =
                            await_spawned_rnode_ready(&state_arc, &spawned, rnode_activity_origin);
                        tokio::pin!(readiness);
                        loop {
                            if !state_arc
                                .is_current_ble_rnode_activity_operation(&activity_operation)
                            {
                                break None;
                            }
                            tokio::select! {
                                result = &mut readiness => break Some(result),
                                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
                            }
                        }
                    };

                    match readiness_result {
                        Some(Ok(pending_monitor)) => {
                            if let Some((terminal_fence, rollback_context, completion)) = state_arc
                                .take_initializing_ble_rnode_activity_operation_with_completion(
                                    &activity_operation,
                                )
                            {
                                if let Some(completion) = completion {
                                    if completion
                                        .send(crate::state::BleRnodeOperationResult::Ready {
                                            interface_id: spawned.interface_id,
                                            monitor: pending_monitor,
                                        })
                                        .is_err()
                                    {
                                        // The lifecycle owner disappeared before accepting the
                                        // single-use seed. Stop the exact runtime; never activate
                                        // an orphaned monitor in this callback.
                                        teardown_spawned_rnode_exact(&rns, &spawned).await;
                                        #[cfg(target_os = "android")]
                                        disconnect_native_ble_rnode_operation(
                                            &state_arc,
                                            &activity_operation,
                                        );
                                    }
                                } else {
                                    clear_ble_rnode_rollback_context(&state_arc, rollback_context);
                                    emit_op_status_broadcast(
                                        &state_arc,
                                        "add_lora",
                                        "hub",
                                        &format!(
                                            "BLE LoRa interface active (#{})",
                                            spawned.interface_id
                                        ),
                                        true,
                                        None,
                                    );
                                    record_interface_activity(
                                        &state_arc,
                                        terminal_fence,
                                        InterfaceClass::RNode,
                                        ble_rnode_activity_transition(
                                            BleRnodeActivityOutcome::Ready,
                                        ),
                                        None,
                                    );
                                    if let Some(pending_monitor) = pending_monitor {
                                        let _ = pending_monitor.activate(Arc::clone(&state_arc));
                                    }
                                }
                            } else {
                                teardown_spawned_rnode_exact(&rns, &spawned).await;
                                #[cfg(target_os = "android")]
                                disconnect_native_ble_rnode_operation(
                                    &state_arc,
                                    &activity_operation,
                                );
                            }
                        }
                        Some(Err(failure)) => {
                            // Claim terminal ownership before any awaited
                            // cleanup. A newer operation can still replace the
                            // claim, in which case this path remains silent.
                            let completion_claimed = state_arc
                                .claim_ble_rnode_activity_operation_completion(&activity_operation)
                                .is_some();
                            teardown_spawned_rnode_exact(&rns, &spawned).await;
                            #[cfg(target_os = "android")]
                            disconnect_native_ble_rnode_operation(&state_arc, &activity_operation);
                            if completion_claimed {
                                if let Some((terminal_fence, rollback_context, completion)) =
                                    state_arc
                                        .take_completing_ble_rnode_activity_operation_with_completion(
                                            &activity_operation,
                                        )
                                {
                                    let operation_failure =
                                        if failure == RnodeReadinessFailure::Timeout {
                                            crate::state::BleRnodeOperationFailure::StartupTimeout
                                        } else {
                                            crate::state::BleRnodeOperationFailure::Readiness
                                        };
                                    if !complete_waiting_ble_rnode_operation(
                                        completion,
                                        crate::state::BleRnodeOperationResult::Failed(
                                            operation_failure,
                                        ),
                                    ) {
                                        let (status, failure_code, activity_outcome) =
                                            ble_rnode_readiness_failure_feedback(failure);
                                        rollback_ble_rnode_context(&state_arc, rollback_context);
                                        emit_op_status_broadcast(
                                            &state_arc,
                                            "add_lora",
                                            "hub",
                                            status,
                                            true,
                                            Some(failure_code),
                                        );
                                        record_interface_activity(
                                            &state_arc,
                                            terminal_fence,
                                            InterfaceClass::RNode,
                                            ble_rnode_activity_transition(activity_outcome),
                                            None,
                                        );
                                    }
                                }
                            }
                        }
                        None => {
                            // Cancellation, replacement, and identity teardown
                            // revoke product authority but not ownership of the
                            // exact runtime this task spawned.
                            teardown_spawned_rnode_exact(&rns, &spawned).await;
                            #[cfg(target_os = "android")]
                            disconnect_native_ble_rnode_operation(&state_arc, &activity_operation);
                        }
                    }
                }
                Some((_rns, _rnode_activity_origin, Err(_error))) => {
                    let terminal = state_arc
                        .take_initializing_ble_rnode_activity_operation_with_completion(
                            &activity_operation,
                        );
                    #[cfg(target_os = "android")]
                    disconnect_native_ble_rnode_operation(&state_arc, &activity_operation);
                    if let Some((terminal_fence, rollback_context, completion)) = terminal {
                        if !complete_waiting_ble_rnode_operation(
                            completion,
                            crate::state::BleRnodeOperationResult::Failed(
                                crate::state::BleRnodeOperationFailure::Connect,
                            ),
                        ) {
                            rollback_ble_rnode_context(&state_arc, rollback_context);
                            emit_op_status_broadcast(
                                &state_arc,
                                "add_lora",
                                "hub",
                                "BLE connected, but the RNode could not start.",
                                true,
                                Some("rnode_start_failed"),
                            );
                            record_interface_activity(
                                &state_arc,
                                terminal_fence,
                                InterfaceClass::RNode,
                                ble_rnode_activity_transition(
                                    BleRnodeActivityOutcome::ConnectFailed,
                                ),
                                None,
                            );
                        }
                    }
                }
                None => {
                    let terminal = state_arc
                        .take_initializing_ble_rnode_activity_operation_with_completion(
                            &activity_operation,
                        );
                    #[cfg(target_os = "android")]
                    disconnect_native_ble_rnode_operation(&state_arc, &activity_operation);
                    if let Some((terminal_fence, rollback_context, completion)) = terminal {
                        if !complete_waiting_ble_rnode_operation(
                            completion,
                            crate::state::BleRnodeOperationResult::Failed(
                                crate::state::BleRnodeOperationFailure::Runtime,
                            ),
                        ) {
                            rollback_ble_rnode_context(&state_arc, rollback_context);
                            emit_op_status_broadcast(
                                &state_arc,
                                "add_lora",
                                "hub",
                                "BLE bridge ready but RNS not running.",
                                true,
                                None,
                            );
                            record_interface_activity(
                                &state_arc,
                                terminal_fence,
                                InterfaceClass::RNode,
                                ble_rnode_activity_transition(
                                    BleRnodeActivityOutcome::RuntimeFailed,
                                ),
                                None,
                            );
                        }
                    }
                }
            }

            let ifaces = crate::rns_config::get_all_interfaces(&active_rns_config_dir(&state_arc));
            emit_hub_interfaces(&state_arc, ifaces);
        });
    }
    #[cfg(not(feature = "ble"))]
    {
        let terminal = state_arc
            .take_initializing_ble_rnode_activity_operation_with_completion(&activity_operation);
        #[cfg(target_os = "android")]
        disconnect_native_ble_rnode_operation(&state_arc, &activity_operation);
        let _ = (
            activity_fence,
            tcp_port,
            name,
            port,
            frequency,
            bandwidth,
            sf,
            cr,
            tx,
            mode,
            st_alock,
            lt_alock,
        );
        if let Some((terminal_fence, rollback_context, completion)) = terminal {
            if !complete_waiting_ble_rnode_operation(
                completion,
                crate::state::BleRnodeOperationResult::Failed(
                    crate::state::BleRnodeOperationFailure::Runtime,
                ),
            ) {
                emit_op_status_broadcast(
                    &state_arc,
                    "add_lora",
                    "hub",
                    "BLE not available (feature not compiled)",
                    true,
                    Some("BLE feature not compiled"),
                );
                rollback_ble_rnode_context(&state_arc, rollback_context);
                record_interface_activity(
                    &state_arc,
                    terminal_fence,
                    InterfaceClass::RNode,
                    ble_rnode_activity_transition(BleRnodeActivityOutcome::RuntimeFailed),
                    None,
                );
            }
        }
    }
    Ok(json!({ "queued": true }))
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BleRnodeNativeFailureCode {
    #[default]
    ConnectFailed,
    BondTimeout,
    SetupTimeout,
}

#[derive(Deserialize)]
pub struct BleRnodeBridgeFailureArgs {
    pub activity_operation: String,
    #[serde(default)]
    failure_code: BleRnodeNativeFailureCode,
}

/// Records only the typed outcome of an Android native bridge failure. The
/// native error remains product feedback in the WebView and never crosses the
/// Activity privacy boundary.
#[tauri::command]
pub async fn ble_rnode_bridge_failed(
    state: State<'_, Arc<AppState>>,
    args: BleRnodeBridgeFailureArgs,
) -> AppResult<Value> {
    let (activity_fence, rollback_context, completion) = state
        .take_pending_ble_rnode_activity_operation_with_completion(&args.activity_operation)
        .ok_or_else(|| AppError::bad_request("Unknown or expired BLE bridge operation"))?;
    let operation_failure = match args.failure_code {
        BleRnodeNativeFailureCode::ConnectFailed => crate::state::BleRnodeOperationFailure::Connect,
        BleRnodeNativeFailureCode::BondTimeout => {
            crate::state::BleRnodeOperationFailure::StartupTimeout
        }
        BleRnodeNativeFailureCode::SetupTimeout => crate::state::BleRnodeOperationFailure::Setup,
    };
    if complete_waiting_ble_rnode_operation(
        completion,
        crate::state::BleRnodeOperationResult::Failed(operation_failure),
    ) {
        return Ok(json!({ "ok": true }));
    }
    rollback_ble_rnode_context(&state, rollback_context);
    record_interface_activity(
        &state,
        activity_fence,
        InterfaceClass::RNode,
        ble_rnode_activity_transition(match args.failure_code {
            BleRnodeNativeFailureCode::ConnectFailed => BleRnodeActivityOutcome::ConnectFailed,
            BleRnodeNativeFailureCode::BondTimeout => BleRnodeActivityOutcome::PairingTimedOut,
            BleRnodeNativeFailureCode::SetupTimeout => BleRnodeActivityOutcome::SetupTimedOut,
        }),
        None,
    );
    Ok(json!({ "ok": true }))
}

/// Aborts an in-flight BLE exchange (the OS dialog may briefly linger).
/// Config rollback only applies to entries the in-flight add created;
/// cancelling a reconnect of a pre-existing radio keeps its config.
#[tauri::command]
pub async fn cancel_ble_connect(state: State<'_, Arc<AppState>>, name: String) -> AppResult<Value> {
    #[cfg(feature = "ble")]
    let state_arc: Arc<AppState> = Arc::clone(&state);
    #[cfg(not(feature = "ble"))]
    let _ = state;
    let name = sanitize_text(&name, 64);
    if name.is_empty() {
        return Err(AppError::bad_request("name required"));
    }

    #[cfg(feature = "ble")]
    {
        let generic_cancelled =
            state_arc.invalidate_rnode_lifecycle_operations_for_names([&name]) > 0;
        let cancelled_operation = state_arc.begin_ble_rnode_activity_cancellation();
        let mut cancellation_token = None;
        if let Some((activity_operation, activity_fence, rollback_context)) = cancelled_operation {
            cancellation_token = Some(activity_operation.clone());
            rollback_ble_rnode_context(&state_arc, rollback_context);
            record_interface_activity(
                &state_arc,
                activity_fence,
                InterfaceClass::RNode,
                ble_rnode_activity_transition(BleRnodeActivityOutcome::Cancelled),
                None,
            );
            #[cfg(target_os = "android")]
            disconnect_native_ble_rnode_operation(&state_arc, &activity_operation);
            #[cfg(not(target_os = "android"))]
            let _ = activity_operation;
        } else if generic_cancelled {
            let activity_fence = state_arc.activity_request_fence();
            record_interface_activity(
                &state_arc,
                activity_fence,
                InterfaceClass::RNode,
                ble_rnode_activity_transition(BleRnodeActivityOutcome::Cancelled),
                None,
            );
            emit_op_status_broadcast(
                &state_arc,
                "add_lora",
                "hub",
                &format!("BLE connect for '{name}' cancelled."),
                true,
                Some("cancelled"),
            );
        }

        // Abort in-flight Linux pair attempt; idempotent.
        #[cfg(target_os = "linux")]
        rns_interface::ble_rnode::linux_cancel_pairing();

        let config_dir = active_rns_config_dir(&state_arc);
        if let Some(cancellation_token) = cancellation_token {
            let name_clone = name.clone();
            tokio::spawn(async move {
                let rns_handle = state_arc
                    .rns
                    .read()
                    .ok()
                    .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));
                let mut interface_id = None;
                if let Some(handle) = rns_handle {
                    if let Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(
                        stats,
                    )) = handle
                        .query_transport(rns_transport::messages::TransportQuery::GetInterfaceStats)
                        .await
                    {
                        for iface in stats {
                            if iface.name == name_clone {
                                interface_id = Some((handle.clone(), iface.id));
                                break;
                            }
                        }
                    }
                }

                if !state_arc.claim_ble_rnode_activity_cancellation(&cancellation_token) {
                    return;
                }
                if let Some((handle, interface_id)) = interface_id {
                    rns_runtime::reticulum::teardown_ble_rnode_interface(&handle, interface_id)
                        .await;
                }
                if state_arc
                    .take_completing_ble_rnode_activity_cancellation(&cancellation_token)
                    .is_none()
                {
                    return;
                }
                let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
                emit_hub_interfaces(&state_arc, ifaces);
                emit_op_status_broadcast(
                    &state_arc,
                    "add_lora",
                    "hub",
                    &format!("BLE connect for '{name_clone}' cancelled."),
                    true,
                    Some("cancelled"),
                );
            });
        }
    }
    #[cfg(not(feature = "ble"))]
    let _ = &name;
    Ok(json!({ "queued": true }))
}

#[tauri::command]
pub async fn disconnect_ble_rnode(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> AppResult<Value> {
    #[cfg(feature = "ble")]
    let state_arc: Arc<AppState> = Arc::clone(&state);
    #[cfg(feature = "ble")]
    let activity_fence = state_arc.activity_request_fence();
    #[cfg(not(feature = "ble"))]
    let _ = state;
    let name = sanitize_text(&name, 64);
    if name.is_empty() {
        return Err(AppError::bad_request("name required"));
    }

    #[cfg(feature = "ble")]
    {
        let config_dir = active_rns_config_dir(&state_arc);
        let (operation_lease, native_ble_disconnect, removal_outcome) =
            with_rns_config_lock(&state_arc, || {
                let operation_lease = state_arc
                    .begin_rnode_lifecycle_operation([&name])
                    .ok_or_else(|| AppError::internal("Failed to begin radio disconnect"))?;
                let native_ble_disconnect = crate::rns_config::get_all_interfaces(&config_dir)
                    .get("rnode")
                    .and_then(Value::as_array)
                    .and_then(|interfaces| {
                        interfaces.iter().find(|interface| {
                            interface.get("name").and_then(Value::as_str) == Some(name.as_str())
                        })
                    })
                    .and_then(|interface| interface.get("port"))
                    .and_then(Value::as_str)
                    .is_some_and(|port| port.starts_with("ble://"));
                let removal_outcome =
                    match crate::rns_config::snapshot_interface_block(&config_dir, &name) {
                        Ok(expected) => {
                            match crate::rns_config::remove_interface_block_if_revision(
                                &config_dir,
                                &expected,
                            ) {
                                crate::rns_config::InterfaceBlockCasOutcome::Applied => {
                                    crate::rns_config::RemoveInterfaceOutcome::Removed
                                }
                                crate::rns_config::InterfaceBlockCasOutcome::NotFound => {
                                    crate::rns_config::RemoveInterfaceOutcome::NotFound
                                }
                                crate::rns_config::InterfaceBlockCasOutcome::Stale
                                | crate::rns_config::InterfaceBlockCasOutcome::WriteFailed => {
                                    crate::rns_config::RemoveInterfaceOutcome::WriteFailed
                                }
                            }
                        }
                        Err(crate::rns_config::InterfaceBlockSnapshotError::NotFound) => {
                            crate::rns_config::RemoveInterfaceOutcome::NotFound
                        }
                        Err(
                            crate::rns_config::InterfaceBlockSnapshotError::Ambiguous
                            | crate::rns_config::InterfaceBlockSnapshotError::ReadFailed,
                        ) => crate::rns_config::RemoveInterfaceOutcome::WriteFailed,
                    };
                Ok::<_, AppError>((operation_lease, native_ble_disconnect, removal_outcome))
            })?;

        if removal_outcome != crate::rns_config::RemoveInterfaceOutcome::Removed {
            if state_arc.is_current_rnode_lifecycle_operation(&operation_lease) {
                match removal_outcome {
                    crate::rns_config::RemoveInterfaceOutcome::NotFound => {
                        emit_op_status_broadcast(
                            &state_arc,
                            "disconnect_ble_rnode",
                            "hub",
                            "BLE LoRa already disconnected",
                            true,
                            None,
                        );
                    }
                    crate::rns_config::RemoveInterfaceOutcome::WriteFailed => {
                        emit_op_status_broadcast(
                            &state_arc,
                            "disconnect_ble_rnode",
                            "hub",
                            "Disconnect failed",
                            true,
                            Some("Config write error"),
                        );
                        record_interface_activity(
                            &state_arc,
                            activity_fence,
                            InterfaceClass::RNode,
                            ble_rnode_activity_transition(BleRnodeActivityOutcome::RemoveFailed),
                            None,
                        );
                    }
                    crate::rns_config::RemoveInterfaceOutcome::Removed => unreachable!(),
                }
                let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
                emit_hub_interfaces(&state_arc, ifaces);
            }
            let _ = state_arc.finish_rnode_lifecycle_operation(&operation_lease);
            return Ok(json!({ "queued": true }));
        }

        if !state_arc.is_current_rnode_lifecycle_operation(&operation_lease) {
            let _ = state_arc.finish_rnode_lifecycle_operation(&operation_lease);
            return Ok(json!({ "queued": true }));
        }
        emit_op_status_broadcast(
            &state_arc,
            "disconnect_ble_rnode",
            "hub",
            "Disconnecting BLE LoRa...",
            false,
            None,
        );
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&state_arc, ifaces);

        tokio::spawn(async move {
            let rns_handle = state_arc
                .rns
                .read()
                .ok()
                .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));
            let mut captured_interface_id = None;
            if let Some(handle) = rns_handle.as_ref() {
                if let Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(
                    stats,
                )) = handle
                    .query_transport(rns_transport::messages::TransportQuery::GetInterfaceStats)
                    .await
                {
                    captured_interface_id = stats
                        .into_iter()
                        .find(|interface| interface.name == name)
                        .map(|interface| interface.id);
                }
            }

            if !state_arc.is_current_rnode_lifecycle_operation(&operation_lease) {
                let _ = state_arc.finish_rnode_lifecycle_operation(&operation_lease);
                return;
            }
            if let (Some(handle), Some(interface_id)) = (rns_handle.as_ref(), captured_interface_id)
            {
                rns_runtime::reticulum::teardown_ble_rnode_interface(handle, interface_id).await;
            }

            let still_owned = with_rns_config_lock(&state_arc, || {
                if !state_arc.is_current_rnode_lifecycle_operation(&operation_lease) {
                    return false;
                }
                #[cfg(target_os = "android")]
                if native_ble_disconnect {
                    // New RNode mutations also begin while this lock is held,
                    // so an older disconnect event is ordered before any
                    // replacement native connect.
                    state_arc.emit_to_all("ble_rnode_disconnect_native", json!({}));
                }
                #[cfg(not(target_os = "android"))]
                let _ = native_ble_disconnect;
                true
            });
            if !still_owned {
                let _ = state_arc.finish_rnode_lifecycle_operation(&operation_lease);
                return;
            }

            if !state_arc.is_current_rnode_lifecycle_operation(&operation_lease) {
                let _ = state_arc.finish_rnode_lifecycle_operation(&operation_lease);
                return;
            }
            emit_op_status_broadcast(
                &state_arc,
                "disconnect_ble_rnode",
                "hub",
                "BLE LoRa disconnected",
                true,
                None,
            );
            record_interface_activity(
                &state_arc,
                activity_fence,
                InterfaceClass::RNode,
                ble_rnode_activity_transition(BleRnodeActivityOutcome::Removed),
                None,
            );
            let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
            emit_hub_interfaces(&state_arc, ifaces);
            let _ = state_arc.finish_rnode_lifecycle_operation(&operation_lease);
        });
    }
    #[cfg(not(feature = "ble"))]
    let _ = &name;
    Ok(json!({ "queued": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ble_peer_activity_outcomes_have_stable_semantics() {
        assert!(matches!(
            ble_peer_activity_transition(BlePeerActivityOutcome::Starting),
            InterfaceTransition::Connecting
        ));
        assert!(matches!(
            ble_peer_activity_transition(BlePeerActivityOutcome::Ready),
            InterfaceTransition::Online
        ));
        assert!(matches!(
            ble_peer_activity_transition(BlePeerActivityOutcome::Stopped),
            InterfaceTransition::Removed
        ));
        assert!(matches!(
            ble_peer_activity_transition(BlePeerActivityOutcome::MissingIdentity),
            InterfaceTransition::Failed {
                reason: InterfaceFailureReason::Configure,
                rollback: None,
            }
        ));
        assert!(matches!(
            ble_peer_activity_transition(BlePeerActivityOutcome::RuntimeFailed),
            InterfaceTransition::Failed {
                reason: InterfaceFailureReason::Runtime,
                rollback: None,
            }
        ));
        assert!(matches!(
            ble_peer_activity_transition(BlePeerActivityOutcome::TimedOut),
            InterfaceTransition::TimedOut {
                reason: InterfaceTimeoutReason::Setup,
            }
        ));
        assert!(matches!(
            ble_peer_activity_transition(BlePeerActivityOutcome::PeripheralUnavailable),
            InterfaceTransition::Degraded {
                reason: InterfaceDegradationReason::PeripheralUnavailable,
            }
        ));
    }

    #[test]
    fn ble_rnode_activity_outcomes_have_stable_semantics() {
        assert!(matches!(
            ble_rnode_activity_transition(BleRnodeActivityOutcome::Ready),
            InterfaceTransition::Online
        ));
        assert!(matches!(
            ble_rnode_activity_transition(BleRnodeActivityOutcome::ConfigureFailed),
            InterfaceTransition::Failed {
                reason: InterfaceFailureReason::Configure,
                rollback: None,
            }
        ));
        assert!(matches!(
            ble_rnode_activity_transition(BleRnodeActivityOutcome::ConnectFailed),
            InterfaceTransition::Failed {
                reason: InterfaceFailureReason::Connect,
                rollback: None,
            }
        ));
        assert!(matches!(
            ble_rnode_activity_transition(BleRnodeActivityOutcome::SetupTimedOut),
            InterfaceTransition::TimedOut {
                reason: InterfaceTimeoutReason::Setup,
            }
        ));
        assert!(matches!(
            ble_rnode_activity_transition(BleRnodeActivityOutcome::PairingTimedOut),
            InterfaceTransition::TimedOut {
                reason: InterfaceTimeoutReason::Pairing,
            }
        ));
        assert!(matches!(
            ble_rnode_activity_transition(BleRnodeActivityOutcome::StartupTimedOut),
            InterfaceTransition::TimedOut {
                reason: InterfaceTimeoutReason::Startup,
            }
        ));
        assert!(matches!(
            ble_rnode_activity_transition(BleRnodeActivityOutcome::Cancelled),
            InterfaceTransition::Cancelled
        ));
        assert!(matches!(
            ble_rnode_activity_transition(BleRnodeActivityOutcome::RuntimeFailed),
            InterfaceTransition::Failed {
                reason: InterfaceFailureReason::Runtime,
                rollback: None,
            }
        ));
        assert!(matches!(
            ble_rnode_activity_transition(BleRnodeActivityOutcome::Removed),
            InterfaceTransition::Removed
        ));
        assert!(matches!(
            ble_rnode_activity_transition(BleRnodeActivityOutcome::RemoveFailed),
            InterfaceTransition::Failed {
                reason: InterfaceFailureReason::Remove,
                rollback: None,
            }
        ));
    }

    #[test]
    fn native_ble_readiness_failures_use_closed_static_feedback() {
        let (status, code, outcome) =
            ble_rnode_readiness_failure_feedback(RnodeReadinessFailure::Timeout);
        assert_eq!(
            status,
            "RNode startup timed out. Check the radio and try again."
        );
        assert_eq!(code, "startup_timeout");
        assert!(matches!(outcome, BleRnodeActivityOutcome::StartupTimedOut));

        for failure in [
            RnodeReadinessFailure::ShuttingDown,
            RnodeReadinessFailure::Stopped,
            RnodeReadinessFailure::ObservationClosed,
            RnodeReadinessFailure::Unclassified,
        ] {
            let (status, code, outcome) = ble_rnode_readiness_failure_feedback(failure);
            assert_eq!(status, "RNode did not become ready. Try connecting again.");
            assert_eq!(code, "readiness_failed");
            assert!(matches!(outcome, BleRnodeActivityOutcome::RuntimeFailed));
        }
    }

    #[test]
    fn native_ble_failure_codes_are_small_typed_allowlist() {
        for (failure_code, expected) in [
            ("connect_failed", BleRnodeNativeFailureCode::ConnectFailed),
            ("bond_timeout", BleRnodeNativeFailureCode::BondTimeout),
            ("setup_timeout", BleRnodeNativeFailureCode::SetupTimeout),
        ] {
            let args: BleRnodeBridgeFailureArgs = serde_json::from_value(json!({
                "activity_operation": "00112233445566778899aabbccddeeff",
                "failure_code": failure_code,
            }))
            .unwrap();
            assert_eq!(args.failure_code, expected);
        }

        let defaulted: BleRnodeBridgeFailureArgs = serde_json::from_value(json!({
            "activity_operation": "00112233445566778899aabbccddeeff",
        }))
        .unwrap();
        assert_eq!(
            defaulted.failure_code,
            BleRnodeNativeFailureCode::ConnectFailed
        );
        assert!(
            serde_json::from_value::<BleRnodeBridgeFailureArgs>(json!({
                "activity_operation": "00112233445566778899aabbccddeeff",
                "failure_code": "raw_native_error",
            }))
            .is_err()
        );
    }

    #[test]
    fn ble_peer_remaining_secs_preserves_always_on() {
        assert_eq!(ble_peer_remaining_secs(0, 100), Some(0));
    }

    #[test]
    fn ble_peer_remaining_secs_drops_expired_timed_request() {
        assert_eq!(ble_peer_remaining_secs(100, 100), None);
        assert_eq!(ble_peer_remaining_secs(99, 100), None);
    }

    #[test]
    fn ble_peer_remaining_secs_keeps_unexpired_timed_request() {
        assert_eq!(ble_peer_remaining_secs(130, 100), Some(30));
    }

    #[test]
    fn ble_recent_disconnect_setting_names_are_stable() {
        assert_eq!(BLE_RECENT_DISCONNECTS_SETTING, "ble_recent_disconnects");
        assert_eq!(
            BLE_RECENT_DISCONNECTS_V2_SETTING,
            "ble_recent_disconnects_v2"
        );
    }

    #[test]
    fn ble_recent_disconnect_seed_addresses_use_v2_records() {
        let v2 = serde_json::to_string(&vec![
            BleRecentDisconnectRecord {
                address: "AA:BB:CC:DD:EE:FF".into(),
                identity_hash: "11111111111111111111111111111111".into(),
                disconnected_at: 10,
            },
            BleRecentDisconnectRecord {
                address: "AA:BB:CC:DD:EE:FF".into(),
                identity_hash: "22222222222222222222222222222222".into(),
                disconnected_at: 9,
            },
            BleRecentDisconnectRecord {
                address: "11:22:33:44:55:66".into(),
                identity_hash: String::new(),
                disconnected_at: 8,
            },
        ])
        .unwrap();
        let seeds = ble_recent_disconnect_seed_addresses(Some(&v2), None);

        assert_eq!(
            seeds,
            vec![
                "AA:BB:CC:DD:EE:FF".to_string(),
                "11:22:33:44:55:66".to_string()
            ]
        );
    }

    #[test]
    fn ble_recent_disconnect_seed_addresses_ignore_legacy_identity_hashes() {
        let legacy = serde_json::to_string(&vec![
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "AA:BB:CC:DD:EE:FF".to_string(),
        ])
        .unwrap();
        let seeds = ble_recent_disconnect_seed_addresses(None, Some(&legacy));

        assert_eq!(seeds, vec!["AA:BB:CC:DD:EE:FF".to_string()]);
    }

    #[test]
    fn ble_recent_disconnect_records_dedupe_address_and_identity() {
        let records = vec![
            BleRecentDisconnectRecord {
                address: "old-address".into(),
                identity_hash: "11111111111111111111111111111111".into(),
                disconnected_at: 1,
            },
            BleRecentDisconnectRecord {
                address: "AA:BB:CC:DD:EE:FF".into(),
                identity_hash: String::new(),
                disconnected_at: 2,
            },
        ];

        let records = update_ble_recent_disconnect_records(
            records,
            "AA:BB:CC:DD:EE:FF".into(),
            Some("11111111111111111111111111111111".into()),
            3,
        );

        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0],
            BleRecentDisconnectRecord {
                address: "AA:BB:CC:DD:EE:FF".into(),
                identity_hash: "11111111111111111111111111111111".into(),
                disconnected_at: 3,
            }
        );
    }
}
