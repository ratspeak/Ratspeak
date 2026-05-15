//! Network commands: announces, alerts, propagation, blackhole, path lookups,
//! announce trigger, log level.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::helpers::{active_identity_id, sanitize_text, validate_hex};
use crate::state::AppState;

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

    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        crate::apply_lxmf_settings_from_state(&state, mgr);
    }
    if let Ok(slot) = state.propagation_node.lock()
        && let Some(node) = slot.as_ref()
        && let Ok(mut node) = node.lock()
    {
        node.set_min_stamp_cost(cost);
    }

    if args.enabled {
        crate::send_announce_from_state(&state).await;
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

    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        crate::apply_lxmf_settings_from_state(&state, mgr);
    }

    crate::send_announce_from_state(&state).await;
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
                if let Ok(mut lxmf) = st_for_off.lxmf.lock()
                    && let Some(mgr) = lxmf.as_mut()
                {
                    mgr.enable_propagation(false, &st_for_off.db, &identity_id);
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
                if let Ok(mut lxmf) = st_for_on.lxmf.lock()
                    && let Some(mgr) = lxmf.as_mut()
                {
                    mgr.enable_propagation(true, &st_for_on.db, &identity_id);
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
                if let Ok(mut lxmf) = st_for_man.lxmf.lock()
                    && let Some(mgr) = lxmf.as_mut()
                {
                    mgr.enable_propagation(true, &st_for_man.db, &identity_id);
                    if !stored.is_empty() && validate_hex(&stored, 32, 32) {
                        mgr.set_propagation_node(Some(&stored), &st_for_man.db, &identity_id);
                    } else {
                        mgr.set_runtime_propagation_node(None);
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
    dest_hash: String,
) -> AppResult<Value> {
    if !validate_hex(&dest_hash, 16, 64) {
        return Ok(json!({ "has_path": false, "error": "Invalid hash" }));
    }

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
            Ok(json!({ "has_path": false }))
        }
        _ => Ok(json!({ "has_path": false, "error": "Unexpected response" })),
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
    state
        .network_log_enabled
        .store(args.enabled, std::sync::atomic::Ordering::Relaxed);
    if !args.enabled
        && let Ok(mut log) = state.event_log.lock()
    {
        log.clear();
    }

    if let Some(level) = args.level.as_deref() {
        let valid = matches!(level, "essential" | "standard" | "detailed");
        if valid && let Ok(mut l) = state.network_log_level.write() {
            *l = level.to_string();
        }
    }

    tracing::debug!(
        "Network logging {}",
        if args.enabled { "enabled" } else { "disabled" }
    );

    let level_out = state
        .network_log_level
        .read()
        .map(|l| l.clone())
        .unwrap_or_else(|_| "standard".into());
    let payload = json!({
        "level": level_out,
        "enabled": args.enabled,
        "restart_required": false,
    });
    state.emit_to_all("network_log_level_changed", payload.clone());
    Ok(payload)
}

#[tauri::command]
pub async fn set_network_log_level(
    state: State<'_, Arc<AppState>>,
    level: String,
) -> AppResult<Value> {
    let level = sanitize_text(&level, 16);
    let valid = matches!(level.as_str(), "essential" | "standard" | "detailed");
    if !valid {
        return Err(AppError::bad_request("Invalid log level"));
    }
    if let Ok(mut l) = state.network_log_level.write() {
        *l = level.clone();
    }
    tracing::debug!("Network log level set to: {}", level);
    let payload = json!({ "level": level, "restart_required": false });
    state.emit_to_all("network_log_level_changed", payload.clone());
    Ok(payload)
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
            if let Ok(mut lxmf) = st.lxmf.lock()
                && let Some(mgr) = lxmf.as_mut()
            {
                mgr.set_runtime_propagation_node(runtime_node);
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
    let prev_failed = if let Ok(lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_ref()
        && let Some(ref client) = mgr.propagation_client
    {
        client.state == PropagationClientState::Failed
    } else {
        false
    };
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
                    client.state,
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
                        "message": format!("Offline Inbox check already in progress: {:?}", client.state),
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

#[tauri::command]
pub async fn trigger_announce(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let ready = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|_| ()))
        .is_some()
        && state
            .lxmf
            .lock()
            .ok()
            .and_then(|l| l.as_ref().map(|_| ()))
            .is_some();
    if !ready {
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
        state.emit_to_all(
            "announce_triggered",
            json!({ "success": false, "error": "no_interfaces" }),
        );
        return Ok(json!(null));
    }
    if state
        .network_log_enabled
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        state.emit_network_event("announce", "Manual announce triggered", "", "detailed");
    }

    let before_tx = before_summary.map(|(_, tx)| tx);
    let mut report = crate::send_manual_announce_from_state(&state).await;
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
            report = crate::send_manual_announce_from_state(&state).await;
            tokio::time::sleep(std::time::Duration::from_millis(450)).await;
            sent_bytes = live_interface_summary(&state)
                .await
                .map(|(_, tx)| tx.saturating_sub(start_tx));
        }
    }

    if report.queued == 0 {
        state.emit_to_all(
            "announce_triggered",
            json!({ "success": false, "error": "not_ready" }),
        );
        return Ok(json!({ "success": false, "error": "not_ready" }));
    }

    if sent_bytes == Some(0) {
        tracing::warn!("manual announce queued but no interface transmitted bytes");
        state.emit_to_all(
            "announce_triggered",
            json!({ "success": false, "error": "not_sent", "retried": retried }),
        );
        return Ok(json!({ "success": false, "error": "not_sent", "retried": retried }));
    }

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

    if success
        && state
            .network_log_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    {
        state.emit_network_event(
            "path",
            &format!("Path requested for {}", &dest_hex[..8.min(dest_hex.len())]),
            &dest_hex,
            "detailed",
        );
    }
    Ok(json!({ "hash": dest_hex, "success": success }))
}

#[tauri::command]
pub async fn request_all_paths(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
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
    if state
        .network_log_enabled
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        state.emit_network_event(
            "path",
            &format!("Requested paths for {} contacts", count),
            "",
            "detailed",
        );
    }
    Ok(json!({ "count": count, "success": true }))
}
