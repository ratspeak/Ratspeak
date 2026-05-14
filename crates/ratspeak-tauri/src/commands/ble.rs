//! BLE commands; non-`ble` builds keep stub fns for stable registration.

use std::sync::Arc;

#[cfg(feature = "ble")]
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

#[cfg(feature = "ble")]
use crate::commands::shared::{active_rns_config_dir, emit_hub_interfaces, with_rns_config_lock};
use crate::commands::shared::{disable_ble_peer_inner, emit_op_status_broadcast};
#[cfg(feature = "ble")]
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::sanitize_text;
use crate::state::AppState;

/// Relay ble_rnode diagnostics to `ble_diag` events. Call once per process.
#[cfg(feature = "ble")]
pub fn spawn_ble_diag_broadcaster(state: &Arc<AppState>) {
    let state_diag = Arc::clone(state);
    tokio::spawn(async move {
        let mut rx = rns_interface::ble_rnode::subscribe_ble_diag();
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    state_diag.emit_to_all("ble_diag", json!({ "msg": msg }));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Linux-only: BlueZ Agent passkey prompts → frontend modal.
    // `attempt_id` lets the UI dedupe stale prompts.
    #[cfg(all(feature = "ble", target_os = "linux"))]
    {
        let state_pairing = Arc::clone(state);
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
        let state_finished = Arc::clone(state);
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
pub fn spawn_ble_diag_broadcaster(_state: &Arc<AppState>) {}

/// Linux: deliver passkey to bluer Agent. No-op on Apple/Windows (OS dialog).
#[tauri::command]
pub async fn submit_ble_rnode_passkey(_passkey: u32) -> AppResult<Value> {
    #[cfg(all(feature = "ble", target_os = "linux"))]
    {
        if rns_interface::ble_rnode::linux_submit_passkey(_passkey) {
            return Ok(json!({ "ok": true }));
        }
        return Err(AppError::not_found(
            "No BLE pairing in progress".to_string(),
        ));
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

#[tauri::command]
pub async fn enable_ble_peer_interface(
    state: State<'_, Arc<AppState>>,
    args: EnableBlePeerArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    #[allow(unused_variables)]
    let duration_secs = args.duration;

    // Mark `ble_peer_enabled=1` only after spawn success.
    tokio::spawn(async move {
        let _rns_handle = state_arc
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));

        #[cfg(feature = "ble")]
        if let Some(handle) = _rns_handle {
            // LXMF is source of truth; fall back to DB on startup race.
            let from_lxmf: Option<String> = state_arc
                .lxmf
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|mgr| mgr.identity_hash.clone()));

            let (identity_hash, seed_identities) = db::spawn_db(state_arc.db.clone(), move |p| {
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
                let seed: Vec<String> = db::get_setting(&p, "ble_recent_disconnects")
                    .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
                    .unwrap_or_default();
                tracing::info!(
                    hash_hex_len = hash_hex.len(),
                    decoded_len = id.len(),
                    "Bluetooth Peer enable: resolved active identity"
                );
                (id, seed)
            })
            .await
            .expect("db task panicked");

            // Zero/missing identity → Android startAdvertising SecurityException.
            if !rns_interface::ble_peer::is_valid_identity_hash(&identity_hash) {
                let _ = db::spawn_db(state_arc.db.clone(), |p| {
                    db::set_setting(&p, "ble_peer_enabled", "0");
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
                return;
            }

            let (event_tx, mut event_rx) =
                tokio::sync::mpsc::channel::<rns_interface::ble_peer::BlePeerEvent>(1024);

            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                rns_runtime::reticulum::spawn_ble_peer_runtime(
                    &handle,
                    "Bluetooth Peer",
                    identity_hash,
                    Some(event_tx),
                    state_arc.foreground_changed.clone(),
                    seed_identities,
                ),
            )
            .await
            {
                Ok(Ok(_id)) => {
                    let _ = db::spawn_db(state_arc.db.clone(), |p| {
                        db::set_setting(&p, "ble_peer_enabled", "1");
                    })
                    .await;
                    state_arc.emit_to_all("ble_peer_status_update", json!({ "enabled": true }));

                    let state_relay: Arc<AppState> = Arc::clone(&state_arc);
                    tokio::spawn(async move {
                        use rns_interface::ble_peer::BlePeerEvent;
                        // Disconnected events lack identity; track per-address.
                        let mut address_to_identity: std::collections::HashMap<String, String> =
                            std::collections::HashMap::new();
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
                                    state_relay
                                        .ble_peer_count
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    address_to_identity
                                        .insert(address.clone(), identity_hash.clone());
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
                                    let _ = state_relay.ble_peer_count.fetch_update(
                                        std::sync::atomic::Ordering::Relaxed,
                                        std::sync::atomic::Ordering::Relaxed,
                                        |n| Some(n.saturating_sub(1)),
                                    );
                                    if let Some(id_hex) = address_to_identity.remove(&address) {
                                        let db = state_relay.db.clone();
                                        tokio::spawn(async move {
                                            let _ = db::spawn_db(db, move |p| {
                                                let mut list: Vec<String> =
                                                    db::get_setting(&p, "ble_recent_disconnects")
                                                        .and_then(|v| {
                                                            serde_json::from_str::<Vec<String>>(&v)
                                                                .ok()
                                                        })
                                                        .unwrap_or_default();
                                                list.retain(|x| x != &id_hex);
                                                list.insert(0, id_hex);
                                                list.truncate(50);
                                                if let Ok(json) = serde_json::to_string(&list) {
                                                    db::set_setting(
                                                        &p,
                                                        "ble_recent_disconnects",
                                                        &json,
                                                    );
                                                }
                                            })
                                            .await;
                                        });
                                    }
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
                                    // Disconnect path persists ble_recent_disconnects from this map.
                                    address_to_identity
                                        .insert(address.clone(), identity_hash.clone());
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
                                BlePeerEvent::SubscribeReady { address } => {
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
                                                    peer = %address,
                                                    "Bluetooth Peer kick-announce sent on peer subscribe"
                                                ),
                                                Err(e) => tracing::warn!(
                                                    peer = %address,
                                                    error = %e,
                                                    "Bluetooth Peer kick-announce failed"
                                                ),
                                            }
                                        });
                                    } else {
                                        tracing::debug!(
                                            peer = %address,
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

                    // 0 = always on.
                    if duration_secs > 0 {
                        let state3: Arc<AppState> = Arc::clone(&state_arc);
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(duration_secs)).await;
                            let still_enabled = db::spawn_db(state3.db.clone(), |p| {
                                db::get_setting(&p, "ble_peer_enabled")
                                    .map(|v| v == "1")
                                    .unwrap_or(false)
                            })
                            .await
                            .unwrap_or(false);
                            if still_enabled {
                                disable_ble_peer_inner(&state3).await;
                            }
                        });
                    }
                }
                Ok(Err(e)) => {
                    let _ = db::spawn_db(state_arc.db.clone(), |p| {
                        db::set_setting(&p, "ble_peer_enabled", "0");
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
                }
                Err(_) => {
                    let _ = db::spawn_db(state_arc.db.clone(), |p| {
                        db::set_setting(&p, "ble_peer_enabled", "0");
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
                }
            }
        }
        #[cfg(not(feature = "ble"))]
        {
            emit_op_status_broadcast(
                &state_arc,
                "enable_ble_peer",
                "hub",
                "BLE not available (feature not compiled)",
                true,
                Some("BLE feature not compiled"),
            );
        }
    });
    Ok(json!({ "queued": true }))
}

#[tauri::command]
pub async fn disable_ble_peer_interface(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    tokio::spawn(async move {
        disable_ble_peer_inner(&state_arc).await;
        emit_op_status_broadcast(
            &state_arc,
            "disable_ble_peer",
            "hub",
            "Bluetooth Peer disabled",
            true,
            None,
        );
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
        #[cfg(all(feature = "ble", target_os = "android"))]
        {
            let addr = address_clone.clone();
            let _ = tokio::task::spawn_blocking(move || {
                rns_interface::ble_peer::disconnect_android_peer(&addr);
            })
            .await;
        }
        #[cfg(not(all(feature = "ble", target_os = "android")))]
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
    let tcp_port = args.tcp_port;
    let name = sanitize_text(&args.name, 64);
    let port = sanitize_text(&args.port, 256);
    let frequency = args.frequency;
    let bandwidth = args.bandwidth;
    let sf = args.spreading_factor;
    let cr = args.coding_rate;
    let tx = args.tx_power;

    if tcp_port == 0 {
        emit_op_status_broadcast(
            &state_arc,
            "add_lora",
            "hub",
            "Invalid TCP bridge port",
            true,
            Some("port=0"),
        );
        return Err(AppError::bad_request("Invalid TCP bridge port"));
    }

    #[cfg(feature = "ble")]
    {
        let name_for_status = name.clone();
        tokio::spawn(async move {
            emit_op_status_broadcast(
                &state_arc,
                "add_lora",
                "hub",
                "BLE connected, initializing RNode...",
                false,
                None,
            );

            let rns = state_arc
                .rns
                .read()
                .ok()
                .and_then(|guard| guard.as_ref().map(|mgr| mgr.handle.clone()));
            if let Some(rns) = rns {
                match rns_runtime::reticulum::spawn_ble_rnode_runtime_native(
                    &rns,
                    rns_runtime::reticulum::BleRnodeRuntimeArgs {
                        name: &name,
                        port: &port,
                        frequency: frequency as u32,
                        bandwidth: bandwidth as u32,
                        spreading_factor: sf,
                        coding_rate: cr,
                        tx_power: tx,
                    },
                    tcp_port,
                )
                .await
                {
                    Ok((id, online)) => {
                        // Wait for first RNode detect/init response.
                        let start = std::time::Instant::now();
                        let timeout = std::time::Duration::from_secs(120);
                        loop {
                            if online.load(std::sync::atomic::Ordering::SeqCst) {
                                emit_op_status_broadcast(
                                    &state_arc,
                                    "add_lora",
                                    "hub",
                                    &format!("BLE LoRa interface active (#{id})"),
                                    true,
                                    None,
                                );
                                break;
                            }
                            if start.elapsed() > timeout {
                                emit_op_status_broadcast(
                                    &state_arc,
                                    "add_lora",
                                    "hub",
                                    &format!("RNode init timed out for '{name_for_status}'."),
                                    true,
                                    Some("init_timeout"),
                                );
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                    }
                    Err(e) => {
                        emit_op_status_broadcast(
                            &state_arc,
                            "add_lora",
                            "hub",
                            &format!("BLE bridge ready but RNode init failed: {e}"),
                            true,
                            Some(&e),
                        );
                    }
                }
            } else {
                emit_op_status_broadcast(
                    &state_arc,
                    "add_lora",
                    "hub",
                    "BLE bridge ready but RNS not running.",
                    true,
                    None,
                );
            }

            let ifaces = crate::rns_config::get_all_interfaces(&active_rns_config_dir(&state_arc));
            emit_hub_interfaces(&state_arc, ifaces);
        });
    }
    #[cfg(not(feature = "ble"))]
    {
        let _ = (tcp_port, name, port, frequency, bandwidth, sf, cr, tx);
        emit_op_status_broadcast(
            &state_arc,
            "add_lora",
            "hub",
            "BLE not available (feature not compiled)",
            true,
            Some("BLE feature not compiled"),
        );
    }
    Ok(json!({ "queued": true }))
}

/// Aborts in-flight iOS SMP exchange (the OS dialog may briefly linger).
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
        // Abort in-flight Linux pair attempt; idempotent.
        #[cfg(target_os = "linux")]
        rns_interface::ble_rnode::linux_cancel_pairing();

        let config_dir = active_rns_config_dir(&state_arc);
        let name_clone = name.clone();
        tokio::spawn(async move {
            let rns_handle = state_arc
                .rns
                .read()
                .ok()
                .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));
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
                    && let Ok(rns_transport::messages::TransportQueryResponse::InterfaceStats(
                        stats,
                    )) = resp_rx.await
                {
                    for iface in stats {
                        if iface.name == name_clone {
                            rns_runtime::reticulum::teardown_interface(&handle, iface.id).await;
                            break;
                        }
                    }
                }
            }

            let _ = with_rns_config_lock(&state_arc, || {
                crate::rns_config::remove_interface(&config_dir, &name_clone)
            });
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
    #[cfg(not(feature = "ble"))]
    let _ = state;
    let name = sanitize_text(&name, 64);

    #[cfg(feature = "ble")]
    {
        let config_dir = active_rns_config_dir(&state_arc);
        let name_clone = name.clone();
        tokio::spawn(async move {
            emit_op_status_broadcast(
                &state_arc,
                "disconnect_ble_rnode",
                "hub",
                "Disconnecting BLE LoRa...",
                false,
                None,
            );

            let rns_handle = state_arc
                .rns
                .read()
                .ok()
                .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));
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
                    && let Ok(rns_transport::messages::TransportQueryResponse::InterfaceStats(
                        stats,
                    )) = resp_rx.await
                {
                    for iface in stats {
                        if iface.name == name_clone {
                            rns_runtime::reticulum::teardown_interface(&handle, iface.id).await;
                            break;
                        }
                    }
                }
            }

            if with_rns_config_lock(&state_arc, || {
                crate::rns_config::remove_interface(&config_dir, &name_clone)
            }) {
                emit_op_status_broadcast(
                    &state_arc,
                    "disconnect_ble_rnode",
                    "hub",
                    "BLE LoRa disconnected",
                    true,
                    None,
                );
            } else {
                emit_op_status_broadcast(
                    &state_arc,
                    "disconnect_ble_rnode",
                    "hub",
                    "Disconnect failed",
                    true,
                    Some("Config write error"),
                );
            }

            let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
            emit_hub_interfaces(&state_arc, ifaces);
        });
    }
    #[cfg(not(feature = "ble"))]
    let _ = &name;
    Ok(json!({ "queued": true }))
}
