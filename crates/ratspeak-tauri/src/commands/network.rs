//! Network commands: announces, alerts, propagation, blackhole, path lookups,
//! announce trigger, log level.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::helpers::{active_identity_id, sanitize_text, validate_hex};
use crate::state::AppState;
use ratspeak_runtime::activity::producer;
use ratspeak_runtime::activity::{
    ActivityCaptureState, ActivityRecorder, ActivityRecorderError, CaptureProfile,
};

#[tauri::command]
pub async fn api_announces(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let announces: Vec<Value> = state
        .announce_history
        .read()
        .map(|a| a.values().cloned().collect())
        .unwrap_or_default();
    Ok(json!(announces))
}

#[tauri::command]
pub async fn api_alerts(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let alerts = state.alerts.lock().map(|a| a.clone()).unwrap_or_default();
    Ok(json!(alerts))
}

#[tauri::command]
pub async fn api_propagation(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    Ok(crate::propagation::get_status_payload(&state))
}

#[derive(Deserialize)]
pub struct PropagationHostingArgs {
    #[serde(default)]
    pub enabled: bool,
    pub stamp_cost: Option<u8>,
}

#[tauri::command]
pub async fn set_propagation_hosting(
    state: State<'_, Arc<AppState>>,
    args: PropagationHostingArgs,
) -> AppResult<Value> {
    let activity_origin = state.activity_request_fence();
    let cost = args.stamp_cost.unwrap_or_else(|| {
        state
            .propagation_node_stamp_cost
            .load(std::sync::atomic::Ordering::Relaxed)
    });
    if cost > 32 {
        return Err(AppError::bad_request("stamp cost must be 0..32"));
    }

    state
        .propagation_node_hosting_enabled
        .store(args.enabled, std::sync::atomic::Ordering::Relaxed);
    state
        .propagation_node_stamp_cost
        .store(cost, std::sync::atomic::Ordering::Relaxed);

    let db = state.db.clone();
    let enabled = args.enabled;
    crate::db::spawn_db(db, move |p| {
        crate::db::set_setting(
            &p,
            "propagation_node_hosting_enabled",
            if enabled { "1" } else { "0" },
        );
        crate::db::set_setting(&p, "propagation_node_stamp_cost", &cost.to_string());
    })
    .await
    .map_err(|_| AppError::internal("set_propagation_hosting db task panicked"))?;

    if let Ok(mut lxmf) = state.lxmf.lock() {
        if let Some(mgr) = lxmf.as_mut() {
            crate::apply_lxmf_settings_from_state(&state, mgr);
        }
    }
    if let Ok(slot) = state.propagation_node.lock() {
        if let Some(node) = slot.as_ref() {
            if let Ok(mut node) = node.lock() {
                node.set_min_stamp_cost(cost);
            }
        }
    }

    if args.enabled {
        crate::send_announce_from_origin(&state, activity_origin).await;
    }
    crate::propagation::emit_propagation_update(&state);
    Ok(crate::propagation::get_status_payload(&state))
}

#[derive(Deserialize)]
pub struct StampSettingsArgs {
    #[serde(default)]
    pub enforce: bool,
    pub required_cost: Option<u8>,
}

#[tauri::command]
pub async fn set_stamp_settings(
    state: State<'_, Arc<AppState>>,
    args: StampSettingsArgs,
) -> AppResult<Value> {
    let activity_origin = state.activity_request_fence();
    let cost = args
        .required_cost
        .unwrap_or(if args.enforce { 8 } else { 0 });
    if cost > 32 {
        return Err(AppError::bad_request("stamp cost must be 0..32"));
    }

    state
        .enforce_stamps
        .store(args.enforce, std::sync::atomic::Ordering::Relaxed);
    state
        .required_stamp_cost
        .store(cost, std::sync::atomic::Ordering::Relaxed);

    let db = state.db.clone();
    let enforce = args.enforce;
    crate::db::spawn_db(db, move |p| {
        crate::db::set_setting(&p, "enforce_stamps", if enforce { "1" } else { "0" });
        crate::db::set_setting(&p, "required_stamp_cost", &cost.to_string());
    })
    .await
    .map_err(|_| AppError::internal("set_stamp_settings db task panicked"))?;

    if let Ok(mut lxmf) = state.lxmf.lock() {
        if let Some(mgr) = lxmf.as_mut() {
            crate::apply_lxmf_settings_from_state(&state, mgr);
        }
    }

    crate::send_announce_from_origin(&state, activity_origin).await;
    let payload = crate::propagation::get_status_payload(&state);
    state.emit_to_all("propagation_update", payload.clone());
    Ok(payload)
}

#[tauri::command]
pub async fn api_propagation_nodes(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    state.trim_propagation_nodes();
    let static_set = crate::static_nodes::hash_set();
    let nodes: Vec<Value> = state
        .discovered_propagation_nodes
        .lock()
        .map(|registry| {
            registry
                .values()
                .map(|v| {
                    let mut out = v.clone();
                    let is_static = v
                        .get("hash")
                        .and_then(|h| h.as_str())
                        .and_then(|hex_hash| hex::decode(hex_hash).ok())
                        .filter(|b| b.len() == 16)
                        .map(|b| {
                            let mut h = [0u8; 16];
                            h.copy_from_slice(&b);
                            static_set.contains(&h)
                        })
                        .unwrap_or(false);
                    if let Some(obj) = out.as_object_mut() {
                        obj.insert("static".to_string(), json!(is_static));
                    }
                    out
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!(nodes))
}

/// 10s throttle. Returns `{ kind: "throttled" | "offline" | "sent", count? }`.
#[tauri::command]
pub async fn refresh_propagation_nodes(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let outcome = crate::propagation::refresh_paths(&state, false).await;
    Ok(serde_json::to_value(&outcome).unwrap_or(json!({"kind": "sent", "count": 0})))
}

/// `favor_static` only applies when mode = "auto".
#[tauri::command]
pub async fn set_propagation_mode(
    state: State<'_, Arc<AppState>>,
    mode: String,
    #[allow(non_snake_case)] favorStatic: Option<bool>,
) -> AppResult<Value> {
    use crate::propagation::{self, PropagationMode};

    let parsed = PropagationMode::parse(mode.trim())
        .ok_or_else(|| AppError::bad_request("mode must be off | auto | manual"))?;
    let st: Arc<AppState> = Arc::clone(&state);
    let prev_mode = propagation::read_settings(&st).0;
    let favor_static = favorStatic;
    let (mode_now, favor_now) = propagation::persist_settings(&st, parsed, favor_static);

    match parsed {
        PropagationMode::Off => {
            // Immediate client disable: active inbox sync state is dropped;
            // the stored node hash remains dormant for later Auto/Manual use.
            let st_for_off = st.clone();
            let identity_id = crate::helpers::active_identity_id(&st);
            tokio::task::spawn_blocking(move || {
                if let Ok(mut lxmf) = st_for_off.lxmf.lock() {
                    if let Some(mgr) = lxmf.as_mut() {
                        mgr.enable_propagation(false, &st_for_off.db, &identity_id);
                    }
                }
            })
            .await
            .map_err(|_| AppError::internal("set_propagation_mode(off) panicked"))?;
            if let Ok(mut slot) = st.auto_active_node.write() {
                *slot = None;
            }
        }
        PropagationMode::Auto => {
            let st_for_on = st.clone();
            let identity_id = crate::helpers::active_identity_id(&st);
            tokio::task::spawn_blocking(move || {
                if let Ok(mut lxmf) = st_for_on.lxmf.lock() {
                    if let Some(mgr) = lxmf.as_mut() {
                        mgr.enable_propagation(true, &st_for_on.db, &identity_id);
                    }
                }
            })
            .await
            .map_err(|_| AppError::internal("set_propagation_mode(auto) panicked"))?;

            if let Some(winner) = propagation::auto_select_node(&st) {
                propagation::apply_auto_selection(&st, winner).await;
            } else {
                propagation::clear_auto_selection(&st).await;
            }

            // Kick path requests on Auto entry / favor_static toggle.
            let was_already_auto = prev_mode == PropagationMode::Auto;
            let bundle_present = !crate::static_nodes::load().is_empty();
            let needs_kick =
                (!was_already_auto || favor_static.is_some()) && bundle_present && favor_now;
            if needs_kick {
                let _ = propagation::refresh_paths(&st, true).await;
            }
        }
        PropagationMode::Manual => {
            // Re-apply DB-stored hash; (re)creates the propagation client.
            let identity = crate::db::get_active_identity(&st.db);
            let stored_hash = identity
                .as_ref()
                .and_then(|id| id.get("propagation_node").and_then(|h| h.as_str()))
                .map(String::from)
                .unwrap_or_default();
            let identity_id = crate::helpers::active_identity_id(&st);
            let st_for_man = st.clone();
            let stored = stored_hash.clone();
            tokio::task::spawn_blocking(move || {
                if let Ok(mut lxmf) = st_for_man.lxmf.lock() {
                    if let Some(mgr) = lxmf.as_mut() {
                        mgr.enable_propagation(true, &st_for_man.db, &identity_id);
                        if !stored.is_empty() && validate_hex(&stored, 32, 32) {
                            mgr.set_propagation_node(Some(&stored), &st_for_man.db, &identity_id);
                        } else {
                            mgr.set_runtime_propagation_node(None);
                        }
                    }
                }
            })
            .await
            .map_err(|_| AppError::internal("set_propagation_mode(manual) panicked"))?;
            if let Ok(mut slot) = st.auto_active_node.write() {
                *slot = None;
            }
            if !stored_hash.is_empty() && validate_hex(&stored_hash, 32, 32) {
                let bytes = hex::decode(&stored_hash)
                    .map_err(|_| AppError::bad_request("Offline Inbox node hash must be hex"))?;
                let mut node = [0u8; 16];
                node.copy_from_slice(&bytes);
                propagation::request_relay_path(&st, node).await;
            }
        }
    }

    propagation::emit_propagation_update(&st);
    let _ = (mode_now, favor_now);
    Ok(propagation::get_status_payload(&st))
}

#[tauri::command]
pub async fn api_hub_interfaces(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let config_dir = crate::commands::shared::active_rns_config_dir(&state);
    let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
    Ok(crate::commands::shared::hub_interfaces_payload(
        &state, ifaces,
    ))
}

/// Sorted newest-first; empty if transport unreachable.
#[tauri::command]
pub async fn api_network_blackhole(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    use rns_transport::messages::{TransportMessage, TransportQuery, TransportQueryResponse};
    let tx = match state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.transport_tx.clone()))
    {
        Some(t) => t,
        None => return Ok(json!({ "entries": [] })),
    };
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if tx
        .send(TransportMessage::Rpc {
            query: TransportQuery::GetBlackholedIdentities,
            response_tx: resp_tx,
        })
        .await
        .is_err()
    {
        return Ok(json!({ "entries": [] }));
    }
    let entries = match resp_rx.await {
        Ok(TransportQueryResponse::BlackholeList(v)) => v,
        _ => return Ok(json!({ "entries": [] })),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let mut rows: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            let expires_in = e.ttl.map(|t| (e.created + t - now).max(0.0));
            let reason = crate::commands::shared::blackhole_reason_display(
                e.reason,
                e.reason_label.as_deref(),
            );
            json!({
                "hash": rns_crypto::hex_encode(&e.identity_hash),
                "reason": reason,
                "created": e.created,
                "expires_in": expires_in,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b.get("created")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .partial_cmp(&a.get("created").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(json!({ "entries": rows }))
}

/// Returns `has_path: false` with `error` on malformed hash / unreachable / miss.
#[tauri::command]
pub async fn api_path_query(
    state: State<'_, Arc<AppState>>,
    mut dest_hash: String,
) -> AppResult<Value> {
    if !validate_hex(&dest_hash, 16, 64) {
        return Ok(json!({ "has_path": false, "error": "Invalid hash" }));
    }
    dest_hash = dest_hash.to_ascii_lowercase();

    let transport_tx = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.transport_tx.clone()));

    let Some(tx) = transport_tx else {
        return Ok(json!({ "has_path": false, "error": "RNS not initialized" }));
    };

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if tx
        .send(rns_transport::messages::TransportMessage::Rpc {
            query: rns_transport::messages::TransportQuery::GetPathTable,
            response_tx: resp_tx,
        })
        .await
        .is_err()
    {
        return Ok(json!({ "has_path": false, "error": "Transport unreachable" }));
    }

    match resp_rx.await {
        Ok(rns_transport::messages::TransportQueryResponse::PathTable(entries)) => {
            for entry in &entries {
                if hex::encode(entry.hash) == dest_hash {
                    return Ok(json!({
                        "has_path": true,
                        "hops": entry.hops,
                        "interface": entry.interface,
                        "expires": entry.expires,
                        "via": entry.via.map(hex::encode),
                    }));
                }
            }
            let diagnostics = ingress_path_diagnostics(&tx).await;
            Ok(json!({
                "has_path": false,
                "diagnostics": diagnostics,
            }))
        }
        _ => Ok(json!({ "has_path": false, "error": "Unexpected response" })),
    }
}

async fn ingress_path_diagnostics(
    tx: &tokio::sync::mpsc::Sender<rns_transport::messages::TransportMessage>,
) -> Value {
    match query_interface_stats(tx).await {
        Some(entries) => ingress_diagnostics_from_interface_stats(&entries),
        None => empty_ingress_diagnostics(),
    }
}

async fn query_interface_stats(
    tx: &tokio::sync::mpsc::Sender<rns_transport::messages::TransportMessage>,
) -> Option<Vec<rns_transport::messages::InterfaceStatRpcEntry>> {
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if tx
        .send(rns_transport::messages::TransportMessage::Rpc {
            query: rns_transport::messages::TransportQuery::GetInterfaceStats,
            response_tx: resp_tx,
        })
        .await
        .is_err()
    {
        return None;
    }

    match resp_rx.await {
        Ok(rns_transport::messages::TransportQueryResponse::InterfaceStats(entries)) => {
            Some(entries)
        }
        _ => None,
    }
}

fn ingress_diagnostics_from_interface_stats(
    entries: &[rns_transport::messages::InterfaceStatRpcEntry],
) -> Value {
    let held_announces: u64 = entries.iter().map(|e| e.held_announces).sum();
    let ingress_burst_active = entries.iter().any(|e| e.burst_active);
    let pr_burst_active = entries.iter().any(|e| e.pr_burst_active);
    let interfaces_holding_announces: Vec<Value> = entries
        .iter()
        .filter(|e| e.held_announces > 0 || e.burst_active || e.pr_burst_active)
        .map(|e| {
            json!({
                "name": e.name,
                "held_announces": e.held_announces,
                "incoming_announce_frequency": e.incoming_announce_frequency,
                "incoming_pr_frequency": e.incoming_pr_frequency,
                "burst_active": e.burst_active,
                "pr_burst_active": e.pr_burst_active,
            })
        })
        .collect();
    json!({
        "ingress_burst_active": ingress_burst_active,
        "path_response_burst_active": pr_burst_active,
        "held_announces": held_announces,
        "interfaces_holding_announces": interfaces_holding_announces,
    })
}

fn empty_ingress_diagnostics() -> Value {
    json!({
        "ingress_burst_active": false,
        "held_announces": 0,
        "interfaces_holding_announces": [],
    })
}

pub(crate) async fn emit_ingress_diagnostics_snapshot(
    state: &Arc<AppState>,
    expected_fence: crate::state::ActivityRequestFence,
) {
    let transport_tx = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.transport_tx.clone()));
    let Some(tx) = transport_tx else {
        return;
    };
    let Some(entries) = query_interface_stats(&tx).await else {
        return;
    };
    // The transport query may complete after an identity switch. Serialize the
    // Activity records with the switch boundary and reject the stale snapshot
    // rather than publishing old-identity interface data afterward.
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let _activity_control = state.activity_control_lock.lock().await;
    if !state.is_current_activity_request_fence_after_identity_lock(expected_fence) {
        return;
    }
    for entry in entries {
        let burst_active = entry.burst_active;
        let held_announces = entry.held_announces;
        if burst_active {
            state.activity.record_event_fenced(
                || state.is_current_activity_request_fence_after_identity_lock(expected_fence),
                || {
                    Ok(producer::rns_announce_activity(
                        producer::RnsAnnounceActivity {
                            transition: producer::RnsAnnounceTransition::IngressBurstStarted,
                            interface: None,
                        },
                    ))
                },
            );
        }
        if held_announces > 0 {
            state.activity.record_event_fenced(
                || state.is_current_activity_request_fence_after_identity_lock(expected_fence),
                || {
                    Ok(producer::rns_announce_activity(
                        producer::RnsAnnounceActivity {
                            transition: producer::RnsAnnounceTransition::Held {
                                count: held_announces,
                            },
                            interface: None,
                        },
                    ))
                },
            );
        }
    }
}

#[derive(Deserialize)]
pub struct NetworkLogArgs {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub level: Option<String>,
}

#[tauri::command]
pub async fn enable_network_log(
    state: State<'_, Arc<AppState>>,
    args: NetworkLogArgs,
) -> AppResult<Value> {
    let request_fence = state.activity_request_fence();
    let level_was_explicit = args.level.is_some();
    let mut requested_level = match args.level {
        Some(level) => validate_legacy_activity_level(&level)?,
        None => state
            .network_log_level
            .read()
            .map(|level| level.clone())
            .unwrap_or_else(|_| "standard".to_string()),
    };
    let (status, payload) = {
        let _identity_lifecycle = state.identity_switch_lock.lock().await;
        let _activity_control = state.activity_control_lock.lock().await;
        ensure_legacy_activity_request_fence(&state, request_fence)?;
        let (capture_state, capture_profile) = state.activity.capture_state_profile();
        requested_level = normalize_legacy_level_for_state(
            &requested_level,
            level_was_explicit,
            capture_state,
            capture_profile,
        );
        if !args.enabled {
            state
                .network_log_enabled
                .store(false, std::sync::atomic::Ordering::Release);
        }
        let status = reconcile_legacy_activity_capture(
            &state.activity,
            args.enabled,
            legacy_capture_profile(&requested_level),
        )
        .await
        .map_err(map_legacy_activity_error)?;
        let effective_level = effective_legacy_activity_level(&requested_level, &status);
        if let Ok(mut level) = state.network_log_level.write() {
            *level = effective_level.clone();
        }
        state.network_log_enabled.store(
            status.state() == ActivityCaptureState::Capturing,
            std::sync::atomic::Ordering::Release,
        );
        let enabled = status.state() == ActivityCaptureState::Capturing;
        let payload = json!({
            "level": effective_level,
            "enabled": enabled,
            "restart_required": false,
            "identity_generation": request_fence.identity_session_generation().to_string(),
            "activity": status,
        });
        state.emit_to_all("network_log_level_changed", payload.clone());
        (status, payload)
    };
    let enabled = status.state() == ActivityCaptureState::Capturing;

    tracing::debug!(
        "Network logging {}",
        if enabled { "enabled" } else { "disabled" }
    );

    if enabled {
        let diagnostics_fence = state.activity_request_fence();
        emit_ingress_diagnostics_snapshot(state.inner(), diagnostics_fence).await;
    }
    Ok(payload)
}

#[tauri::command]
pub async fn set_network_log_level(
    state: State<'_, Arc<AppState>>,
    level: String,
) -> AppResult<Value> {
    let request_fence = state.activity_request_fence();
    let level = validate_legacy_activity_level(&level)?;
    let (activity, effective_level, payload) = {
        let _identity_lifecycle = state.identity_switch_lock.lock().await;
        let _activity_control = state.activity_control_lock.lock().await;
        ensure_legacy_activity_request_fence(&state, request_fence)?;
        let target = legacy_capture_profile(&level);
        let activity = set_legacy_activity_profile(&state.activity, target)
            .await
            .map_err(map_legacy_activity_error)?;
        let effective_level = effective_legacy_activity_level(&level, &activity);
        if let Ok(mut stored) = state.network_log_level.write() {
            *stored = effective_level.clone();
        }
        let payload = json!({
            "level": effective_level,
            "restart_required": false,
            "identity_generation": request_fence.identity_session_generation().to_string(),
            "activity": activity,
        });
        state.emit_to_all("network_log_level_changed", payload.clone());
        (activity, effective_level, payload)
    };
    tracing::debug!("Network log level set to: {}", effective_level);
    let _ = activity;
    Ok(payload)
}

fn ensure_legacy_activity_request_fence(
    state: &AppState,
    expected: crate::state::ActivityRequestFence,
) -> AppResult<()> {
    if state.is_current_activity_request_fence_after_identity_lock(expected) {
        Ok(())
    } else {
        Err(AppError::conflict(
            "The active session changed before the Activity request could run.",
        ))
    }
}

async fn set_legacy_activity_profile(
    activity: &ActivityRecorder,
    target: CaptureProfile,
) -> Result<ratspeak_runtime::activity::ActivityStatusV1, ActivityRecorderError> {
    let status = activity.status();
    if status.state() == ActivityCaptureState::Capturing && status.profile() != Some(target) {
        activity.set_profile(target, None).await
    } else {
        Ok(status)
    }
}

fn validate_legacy_activity_level(level: &str) -> AppResult<String> {
    let level = sanitize_text(level, 16);
    if !matches!(level.as_str(), "essential" | "standard" | "detailed") {
        return Err(AppError::bad_request("Invalid log level"));
    }
    Ok(level)
}

fn legacy_capture_profile(level: &str) -> CaptureProfile {
    if level == "detailed" {
        CaptureProfile::Trace
    } else {
        CaptureProfile::Normal
    }
}

fn normalize_legacy_level_for_state(
    requested: &str,
    was_explicit: bool,
    state: ActivityCaptureState,
    profile: Option<CaptureProfile>,
) -> String {
    if !was_explicit
        && (state != ActivityCaptureState::Capturing || profile != Some(CaptureProfile::Trace))
    {
        "standard".to_string()
    } else {
        requested.to_string()
    }
}

fn effective_legacy_activity_level(
    requested: &str,
    status: &ratspeak_runtime::activity::ActivityStatusV1,
) -> String {
    if requested == "detailed"
        && (status.state() != ActivityCaptureState::Capturing
            || status.profile() != Some(CaptureProfile::Trace))
    {
        "standard".to_string()
    } else {
        requested.to_string()
    }
}

async fn reconcile_legacy_activity_capture(
    activity: &ActivityRecorder,
    enabled: bool,
    target: CaptureProfile,
) -> Result<ratspeak_runtime::activity::ActivityStatusV1, ActivityRecorderError> {
    let initial = activity.status();
    let initial_state = initial.state();
    if !enabled {
        return if initial_state == ActivityCaptureState::Capturing {
            activity.stop().await
        } else {
            Ok(initial)
        };
    }

    let resumed_or_started = match initial_state {
        ActivityCaptureState::Off => activity.start().await?,
        // Resume is deliberately continuous but always Normal. A historical
        // stopped Trace profile or stale legacy Detailed choice must not
        // silently re-enable Trace.
        ActivityCaptureState::Stopped => return activity.resume().await,
        ActivityCaptureState::Capturing => initial,
    };
    if resumed_or_started.profile() == Some(target) {
        return Ok(resumed_or_started);
    }
    match activity.set_profile(target, None).await {
        Ok(status) => Ok(status),
        Err(error) => {
            match initial_state {
                ActivityCaptureState::Off => {
                    let _ = activity.hard_reset().await;
                }
                ActivityCaptureState::Stopped => {
                    let _ = activity.stop().await;
                }
                ActivityCaptureState::Capturing => {}
            }
            Err(error)
        }
    }
}

fn map_legacy_activity_error(error: ActivityRecorderError) -> AppError {
    match error {
        ActivityRecorderError::InvalidRequest => {
            AppError::bad_request("Invalid Activity capture request")
        }
        ActivityRecorderError::InvalidTransition | ActivityRecorderError::Superseded => {
            AppError::conflict("Activity capture state changed")
        }
        ActivityRecorderError::ControlBusy => {
            AppError::conflict("Activity capture control is busy")
        }
        ActivityRecorderError::WorkerUnavailable
        | ActivityRecorderError::GenerationExhausted
        | ActivityRecorderError::RingUnavailable
        | ActivityRecorderError::TimedOut => {
            AppError::service_unavailable("Activity capture is unavailable")
        }
    }
}

#[cfg(test)]
mod activity_compatibility_tests {
    use ratspeak_runtime::activity::{ActivityReplayResultV1, ActivityTraceStateV1};

    use super::*;

    #[test]
    fn legacy_levels_map_to_the_two_capture_profiles() {
        assert_eq!(legacy_capture_profile("essential"), CaptureProfile::Normal);
        assert_eq!(legacy_capture_profile("standard"), CaptureProfile::Normal);
        assert_eq!(legacy_capture_profile("detailed"), CaptureProfile::Trace);
        assert_eq!(
            normalize_legacy_level_for_state("detailed", false, ActivityCaptureState::Off, None,),
            "standard"
        );
        assert_eq!(
            normalize_legacy_level_for_state("detailed", true, ActivityCaptureState::Off, None,),
            "detailed"
        );
        assert_eq!(
            normalize_legacy_level_for_state(
                "detailed",
                false,
                ActivityCaptureState::Capturing,
                Some(CaptureProfile::Normal),
            ),
            "standard"
        );
        assert_eq!(
            normalize_legacy_level_for_state(
                "detailed",
                false,
                ActivityCaptureState::Capturing,
                Some(CaptureProfile::Trace),
            ),
            "detailed"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enable_stop_resume_and_trace_preserve_the_typed_session() {
        let activity = ActivityRecorder::new();
        let started = reconcile_legacy_activity_capture(&activity, true, CaptureProfile::Normal)
            .await
            .unwrap();
        let session = started.capture_session().unwrap().to_string();
        assert_eq!(started.state(), ActivityCaptureState::Capturing);
        assert_eq!(started.profile(), Some(CaptureProfile::Normal));

        let stopped = reconcile_legacy_activity_capture(&activity, false, CaptureProfile::Normal)
            .await
            .unwrap();
        assert_eq!(stopped.state(), ActivityCaptureState::Stopped);
        assert_eq!(stopped.capture_session(), Some(session.as_str()));
        let ActivityReplayResultV1::Page { page } = activity
            .replay(session.clone(), None, 50, 64 * 1024)
            .await
            .unwrap()
        else {
            panic!("Stop must retain the typed session");
        };
        assert_eq!(page.events().len(), 2);

        let still_stopped = set_legacy_activity_profile(&activity, CaptureProfile::Trace)
            .await
            .unwrap();
        assert_eq!(still_stopped.state(), ActivityCaptureState::Stopped);
        assert_eq!(still_stopped.profile(), Some(CaptureProfile::Normal));

        let resumed = reconcile_legacy_activity_capture(&activity, true, CaptureProfile::Normal)
            .await
            .unwrap();
        assert_eq!(resumed.state(), ActivityCaptureState::Capturing);
        assert_eq!(resumed.capture_session(), Some(session.as_str()));
        assert_eq!(resumed.profile(), Some(CaptureProfile::Normal));

        let traced = set_legacy_activity_profile(&activity, CaptureProfile::Trace)
            .await
            .unwrap();
        assert_eq!(traced.profile(), Some(CaptureProfile::Trace));
        assert!(matches!(
            traced.trace(),
            Some(ActivityTraceStateV1::UntilStopped) | Some(ActivityTraceStateV1::Limited { .. })
        ));

        let stopped_trace =
            reconcile_legacy_activity_capture(&activity, false, CaptureProfile::Trace)
                .await
                .unwrap();
        assert_eq!(stopped_trace.state(), ActivityCaptureState::Stopped);
        assert_eq!(stopped_trace.profile(), Some(CaptureProfile::Trace));
        assert_eq!(stopped_trace.trace(), None);
        assert_eq!(
            effective_legacy_activity_level("detailed", &stopped_trace),
            "standard"
        );

        let resumed_from_trace =
            reconcile_legacy_activity_capture(&activity, true, CaptureProfile::Trace)
                .await
                .unwrap();
        assert_eq!(resumed_from_trace.state(), ActivityCaptureState::Capturing);
        assert_eq!(resumed_from_trace.profile(), Some(CaptureProfile::Normal));
        assert_eq!(resumed_from_trace.trace(), None);
        assert_eq!(
            effective_legacy_activity_level("detailed", &resumed_from_trace),
            "standard"
        );
        activity.shutdown().await.unwrap();
    }
}

#[tauri::command]
pub async fn set_propagation_node(
    state: State<'_, Arc<AppState>>,
    hash: String,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&hash, 128);
    let identity_id = active_identity_id(&state);
    if !dest_hash.is_empty() && !validate_hex(&dest_hash, 32, 32) {
        return Err(AppError::bad_request(
            "Offline Inbox node hash must be 32 hex characters",
        ));
    }
    let runtime_node = if dest_hash.is_empty() {
        None
    } else {
        let bytes = hex::decode(&dest_hash)
            .map_err(|_| AppError::bad_request("Offline Inbox node hash must be hex"))?;
        let mut node = [0u8; 16];
        node.copy_from_slice(&bytes);
        Some(node)
    };
    let mode = crate::propagation::read_settings(&state).0;

    let db = state.db.clone();
    let dh_for_db = dest_hash.clone();
    let id_for_db = identity_id.clone();
    crate::db::spawn_db(db, move |p| {
        crate::db::set_identity_propagation_node(&p, &id_for_db, &dh_for_db)
    })
    .await
    .map_err(|_| AppError::internal("set_propagation_node db task panicked"))?
    .map_err(|e| AppError::internal(format!("Failed to save Offline Inbox node: {e}")))?;

    let st: Arc<AppState> = Arc::clone(&state);
    if mode == crate::propagation::PropagationMode::Manual {
        let path_request_node = runtime_node;
        tokio::task::spawn_blocking(move || {
            if let Ok(mut lxmf) = st.lxmf.lock() {
                if let Some(mgr) = lxmf.as_mut() {
                    mgr.set_runtime_propagation_node(runtime_node);
                }
            }
        })
        .await
        .map_err(|_| AppError::internal("set_propagation_node task panicked"))?;
        if let Ok(mut slot) = state.auto_active_node.write() {
            *slot = None;
        }
        if let Some(node) = path_request_node {
            crate::propagation::request_relay_path(&state, node).await;
        }
    }

    crate::propagation::emit_propagation_update(&state);
    Ok(crate::propagation::get_status_payload(&state))
}

/// Shim around `set_propagation_mode`: `true → "auto"`, `false → "off"`.
#[tauri::command]
pub async fn enable_propagation(
    state: State<'_, Arc<AppState>>,
    enabled: bool,
) -> AppResult<Value> {
    let mode = if enabled { "auto" } else { "off" }.to_string();
    set_propagation_mode(state, mode, None).await
}

#[tauri::command]
pub async fn sync_propagation(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    use lxmf_core::propagation_client::PropagationClientState;

    // Off blocks new client sync starts.
    let (mode, _) = crate::propagation::read_settings(&state);
    if mode == crate::propagation::PropagationMode::Off {
        let result = json!({
            "ok": false,
            "success": false,
            "started": false,
            "downloaded": 0,
            "message": "Offline Inbox is off",
            "error": "Offline Inbox is off",
        });
        state.emit_to_all("propagation_sync_result", result.clone());
        return Ok(result);
    }

    // Run failure handler if last run failed.
    let prev_failed = state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| {
            lxmf.as_ref().and_then(|mgr| {
                mgr.propagation_client
                    .as_ref()
                    .map(|client| client.state() == PropagationClientState::Failed)
            })
        })
        .unwrap_or(false);
    if prev_failed {
        let st: Arc<AppState> = Arc::clone(&state);
        crate::propagation::handle_sync_failure(&st).await;
    }

    let readiness = crate::propagation::ensure_relay_ready_for_send(&state).await;
    let relay_ready = readiness == crate::propagation::RelayReadiness::Ready;

    let result = if let Ok(mut lxmf) = state.lxmf.lock() {
        if let Some(mgr) = lxmf.as_mut() {
            if let Some(ref mut client) = mgr.propagation_client {
                if matches!(
                    client.state(),
                    PropagationClientState::Idle
                        | PropagationClientState::Complete
                        | PropagationClientState::Failed
                ) {
                    if relay_ready {
                        client.start_download();
                    }
                    json!({
                        "ok": true,
                        "success": true,
                        "started": relay_ready,
                        "downloaded": 0,
                        "message": if relay_ready { "Offline Inbox check started" } else { "Offline Inbox path requested" },
                    })
                } else {
                    json!({
                        "ok": true,
                        "success": true,
                        "started": false,
                        "downloaded": 0,
                        "message": format!(
                            "Offline Inbox check already in progress: {:?}",
                            client.state()
                        ),
                    })
                }
            } else {
                json!({
                    "ok": false,
                    "success": false,
                    "started": false,
                    "downloaded": 0,
                    "message": "No Offline Inbox node configured",
                    "error": "No Offline Inbox node configured",
                })
            }
        } else {
            json!({
                "ok": false,
                "success": false,
                "started": false,
                "downloaded": 0,
                "message": "LXMF not initialized",
                "error": "LXMF not initialized",
            })
        }
    } else {
        json!({
            "ok": false,
            "success": false,
            "started": false,
            "downloaded": 0,
            "message": "Lock error",
            "error": "Lock error",
        })
    };
    state.emit_to_all("propagation_sync_result", result.clone());
    crate::propagation::emit_propagation_update(&state);
    Ok(result)
}

#[tauri::command]
pub async fn get_propagation_status(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    Ok(crate::propagation::get_status_payload(&state))
}

async fn live_interface_summary(state: &Arc<AppState>) -> Option<(bool, u64)> {
    let handle = {
        let rns = state.rns.read().ok()?;
        rns.as_ref().map(|mgr| mgr.handle.clone())?
    };
    match handle
        .query_control(rns_transport::messages::TransportQuery::GetInterfaceStats)
        .await
    {
        Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(stats)) => Some((
            stats.iter().any(|iface| iface.online),
            stats.iter().map(|iface| iface.tx_bytes).sum(),
        )),
        _ => None,
    }
}

fn record_manual_announce_outcome(
    state: &AppState,
    fence: crate::state::ActivityRequestFence,
    failure: Option<producer::AnnounceFailureReason>,
) {
    state.activity.record_event_fenced(
        || state.is_current_activity_origin_fence(fence),
        || {
            let transition = match failure {
                Some(reason) => producer::RnsAnnounceTransition::Failed {
                    method: producer::AnnounceMethod::Manual,
                    reason,
                },
                None => producer::RnsAnnounceTransition::Sent {
                    method: producer::AnnounceMethod::Manual,
                },
            };
            Ok(producer::rns_announce_activity(
                producer::RnsAnnounceActivity {
                    transition,
                    interface: None,
                },
            ))
        },
    );
}

#[tauri::command]
pub async fn trigger_announce(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let activity_fence = state.activity_request_fence();
    let rns_ready = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|_| ()))
        .is_some();
    let lxmf_ready = rns_ready
        && state
            .lxmf
            .lock()
            .ok()
            .and_then(|l| l.as_ref().map(|_| ()))
            .is_some();
    if !rns_ready || !lxmf_ready {
        record_manual_announce_outcome(
            &state,
            activity_fence,
            Some(if rns_ready {
                producer::AnnounceFailureReason::NotReady
            } else {
                producer::AnnounceFailureReason::TransportUnavailable
            }),
        );
        state.emit_to_all(
            "announce_triggered",
            json!({ "success": false, "error": "RNS or LXMF not initialized" }),
        );
        return Err(AppError::service_unavailable("RNS or LXMF not initialized"));
    }

    let before_summary = live_interface_summary(&state).await;
    let online = before_summary
        .map(|(online, _)| online)
        .or_else(|| crate::any_interface_online_cached(&state));
    if matches!(online, Some(false)) {
        tracing::warn!("manual announce skipped: no interfaces online");
        record_manual_announce_outcome(
            &state,
            activity_fence,
            Some(producer::AnnounceFailureReason::NoInterfaceTransmission),
        );
        state.emit_to_all(
            "announce_triggered",
            json!({ "success": false, "error": "no_interfaces" }),
        );
        return Ok(json!(null));
    }

    let before_tx = before_summary.map(|(_, tx)| tx);
    let mut report = crate::send_manual_announce_from_origin(&state, activity_fence).await;
    let mut retried = false;
    let mut sent_bytes = None;

    if let Some(start_tx) = before_tx {
        tokio::time::sleep(std::time::Duration::from_millis(450)).await;
        sent_bytes = live_interface_summary(&state)
            .await
            .map(|(_, tx)| tx.saturating_sub(start_tx));

        if report.queued > 0 && sent_bytes == Some(0) {
            retried = true;
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            report = crate::send_manual_announce_from_origin(&state, activity_fence).await;
            tokio::time::sleep(std::time::Duration::from_millis(450)).await;
            sent_bytes = live_interface_summary(&state)
                .await
                .map(|(_, tx)| tx.saturating_sub(start_tx));
        }
    }

    if report.queued == 0 {
        let failure = if report.packets == 0 {
            producer::AnnounceFailureReason::NotReady
        } else if report.failed > 0 {
            producer::AnnounceFailureReason::QueueFailed
        } else {
            producer::AnnounceFailureReason::TransportUnavailable
        };
        record_manual_announce_outcome(&state, activity_fence, Some(failure));
        state.emit_to_all(
            "announce_triggered",
            json!({ "success": false, "error": "not_ready" }),
        );
        return Ok(json!({ "success": false, "error": "not_ready" }));
    }

    if sent_bytes == Some(0) {
        tracing::warn!("manual announce queued but no interface transmitted bytes");
        record_manual_announce_outcome(
            &state,
            activity_fence,
            Some(producer::AnnounceFailureReason::NoInterfaceTransmission),
        );
        state.emit_to_all(
            "announce_triggered",
            json!({ "success": false, "error": "not_sent", "retried": retried }),
        );
        return Ok(json!({ "success": false, "error": "not_sent", "retried": retried }));
    }

    record_manual_announce_outcome(&state, activity_fence, None);
    state.emit_to_all(
        "announce_triggered",
        json!({ "success": true, "retried": retried, "sent_bytes": sent_bytes }),
    );
    Ok(json!({ "success": true, "retried": retried, "sent_bytes": sent_bytes }))
}

#[tauri::command]
pub async fn request_path(state: State<'_, Arc<AppState>>, hash: String) -> AppResult<Value> {
    let dest_hex = sanitize_text(&hash, 128);
    if !validate_hex(&dest_hex, 16, 64) {
        return Err(AppError::bad_request("Invalid hash"));
    }

    let bytes = hex::decode(&dest_hex).map_err(|_| AppError::bad_request("Invalid hash"))?;
    if bytes.len() != 16 {
        return Err(AppError::bad_request("Invalid hash"));
    }
    let mut dest = [0u8; 16];
    dest.copy_from_slice(&bytes);

    let activity_fence = state.activity_request_fence();
    let success = if let Ok(rns) = state.rns.read() {
        if let Some(mgr) = rns.as_ref() {
            mgr.handle
                .transport_tx
                .try_send(rns_transport::messages::TransportMessage::RequestPath {
                    destination_hash: dest,
                })
                .is_ok()
        } else {
            false
        }
    } else {
        false
    };

    if success {
        state.activity.record_event_fenced(
            || state.is_current_activity_origin_fence(activity_fence),
            || {
                let destination = producer::DestinationHash::from_hex(&dest_hex)?;
                Ok(producer::rns_path_requested(producer::RnsPathRequested {
                    destination: Some(destination),
                    count: None,
                    method: producer::PathRequestMethod::Manual,
                }))
            },
        );
    }
    Ok(json!({ "hash": dest_hex, "success": success }))
}

#[tauri::command]
pub async fn request_all_paths(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let activity_fence = state.activity_request_fence();
    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let id_c = identity_id.clone();
    let count = tokio::task::spawn_blocking(move || {
        if let Ok(lxmf) = st.lxmf.lock() {
            lxmf.as_ref()
                .map(|mgr| mgr.request_all_paths(&st.db, &id_c))
                .unwrap_or(0)
        } else {
            0
        }
    })
    .await
    .unwrap_or(0);
    state.activity.record_event_fenced(
        || state.is_current_activity_origin_fence(activity_fence),
        || {
            Ok(producer::rns_path_requested(producer::RnsPathRequested {
                destination: None,
                count: Some(count as u64),
                method: producer::PathRequestMethod::ContactRefresh,
            }))
        },
    );
    Ok(json!({ "count": count, "success": true }))
}
