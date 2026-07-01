//! Interface discovery + management: presets, serial enum, BLE,
//! connection history, transport toggle, add/remove LoRa/TCP/Auto.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::commands::shared::{
    active_rns_config_dir, emit_hub_interfaces, emit_op_status_broadcast, normalize_transport_mode,
    persisted_transport_mode, with_rns_config_lock,
};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::sanitize_text;
use crate::state::AppState;

const DEFAULT_PEERS_SORT: &str = "last_seen";

fn normalize_peers_sort(sort: &str) -> Option<&'static str> {
    match sort.trim() {
        "name" => Some("name"),
        "hops" => Some("hops"),
        "last_seen" => Some("last_seen"),
        _ => None,
    }
}

fn persisted_peers_sort(state: &AppState) -> String {
    db::get_setting(&state.db, "peers_sort")
        .and_then(|sort| normalize_peers_sort(&sort).map(str::to_string))
        .unwrap_or_else(|| DEFAULT_PEERS_SORT.to_string())
}

#[cfg(all(feature = "ble", target_os = "android"))]
fn android_ble_peer_availability_payload() -> Value {
    match rns_interface::ble_peer::android_ble_peer_availability_json()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).map_err(|e| e.to_string()))
    {
        Ok(value) => value,
        Err(e) => json!({
            "available": true,
            "missing": [],
            "missing_permissions": [],
            "permissions_granted": false,
            "permission_required": false,
            "probe_failed": true,
            "error": e,
        }),
    }
}

/// Result of the shared BLE platform availability probe.
struct BlePlatformProbe {
    available: bool,
    missing: Vec<String>,
    /// iOS CoreBluetooth authorization state (iOS builds only).
    auth_state: Option<&'static str>,
    /// Android runtime permissions still outstanding (Android builds only).
    permission_required: bool,
}

/// Five-way platform dispatch shared by the BLE availability commands: iOS
/// auth-state mapping / Android JNI payload / macOS no-probe / desktop adapter
/// probe / no-`ble`-feature stub. `api_ble_available` keeps its own Android
/// and desktop arms where behavior diverges (hardcoded Android availability,
/// Linux BlueZ hints).
async fn ble_platform_probe() -> BlePlatformProbe {
    #[cfg(all(feature = "ble", target_os = "ios"))]
    {
        let auth = crate::platform_ios::bluetooth_authorization();
        let (available, missing) = match auth {
            "denied" | "restricted" => (
                false,
                vec![
                    "iOS Bluetooth permission denied — open Settings → Ratspeak → Bluetooth"
                        .to_string(),
                ],
            ),
            _ => (true, vec![]),
        };
        return BlePlatformProbe {
            available,
            missing,
            auth_state: Some(auth),
            permission_required: false,
        };
    }

    #[cfg(all(feature = "ble", target_os = "android"))]
    {
        let payload = android_ble_peer_availability_payload();
        let missing = payload
            .get("missing")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        return BlePlatformProbe {
            available: payload
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            missing,
            auth_state: None,
            permission_required: payload
                .get("permission_required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
    }

    // macOS: skip btleplug probe; `Manager::new()` triggers the system
    // Bluetooth permission prompt prematurely.
    #[cfg(all(feature = "ble", target_os = "macos"))]
    return BlePlatformProbe {
        available: true,
        missing: vec![],
        auth_state: None,
        permission_required: false,
    };

    #[cfg(all(
        feature = "ble",
        not(target_os = "ios"),
        not(target_os = "android"),
        not(target_os = "macos")
    ))]
    {
        let (available, missing) = match tokio::time::timeout(
            std::time::Duration::from_secs(3),
            rns_interface::ble_rnode::ble_adapter_present(),
        )
        .await
        {
            Ok(Ok(true)) => (true, vec![]),
            Ok(Ok(false)) => (false, vec!["No BLE adapter found".to_string()]),
            Ok(Err(e)) => (false, vec![e]),
            Err(_) => (false, vec!["BLE check timed out".to_string()]),
        };
        return BlePlatformProbe {
            available,
            missing,
            auth_state: None,
            permission_required: false,
        };
    }

    #[cfg(not(feature = "ble"))]
    BlePlatformProbe {
        available: false,
        missing: vec!["ble feature not compiled".to_string()],
        auth_state: None,
        permission_required: false,
    }
}

#[tauri::command]
pub async fn api_rnode_presets() -> AppResult<Value> {
    serde_json::to_value(ratspeak_core::radio::rnode_catalog())
        .map_err(|e| AppError::internal(format!("RNode preset catalog error: {e}")))
}

#[tauri::command]
pub async fn api_serial_ports() -> AppResult<Value> {
    #[cfg(feature = "serial")]
    {
        let mut ports = Vec::new();
        match serialport::available_ports() {
            Ok(port_list) => {
                for p in port_list {
                    // macOS: hide /dev/tty.* duplicates (we use cu.*).
                    #[cfg(target_os = "macos")]
                    if p.port_name.starts_with("/dev/tty.") {
                        continue;
                    }
                    let (desc, hwid, manufacturer, product, vid, pid) = match &p.port_type {
                        serialport::SerialPortType::UsbPort(usb) => (
                            usb.product.as_deref().unwrap_or(&p.port_name).to_string(),
                            format!("USB VID:PID={:04X}:{:04X}", usb.vid, usb.pid),
                            usb.manufacturer.clone(),
                            usb.product.clone(),
                            Some(usb.vid),
                            Some(usb.pid),
                        ),
                        _ => ("n/a".to_string(), "n/a".to_string(), None, None, None, None),
                    };
                    // Linux: probe-open known RNode VIDs to detect udev permission errors.
                    // VIDs mirror `KNOWN_VIDS` in `rns-interface/src/android_usb.rs`.
                    #[cfg(target_os = "linux")]
                    let perm_denied = {
                        const KNOWN_USB_SERIAL_VIDS: &[u16] = &[
                            0x0403, 0x10C4, 0x1A86, 0x0525, 0x2E8A, 0x303A, 0x239A, 0x1915,
                        ];
                        let known = vid
                            .map(|v| KNOWN_USB_SERIAL_VIDS.contains(&v))
                            .unwrap_or(false);
                        if known {
                            matches!(
                                serialport::new(&p.port_name, 115_200)
                                    .timeout(std::time::Duration::from_millis(50))
                                    .open(),
                                Err(e) if matches!(
                                    e.kind,
                                    serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied),
                                )
                            )
                        } else {
                            false
                        }
                    };
                    #[cfg(not(target_os = "linux"))]
                    let perm_denied = false;

                    ports.push(json!({
                        "device": p.port_name,
                        "description": desc,
                        "hwid": hwid,
                        "manufacturer": manufacturer,
                        "product": product,
                        "vid": vid,
                        "pid": pid,
                        "permission_denied": perm_denied,
                    }));
                }
            }
            Err(_) => {
                #[cfg(unix)]
                for pattern in &["/dev/ttyUSB*", "/dev/ttyACM*", "/dev/cu.*", "/dev/tty.usb*"] {
                    if let Ok(entries) = glob::glob(pattern) {
                        for entry in entries.flatten() {
                            let device = entry.to_string_lossy().to_string();
                            ports.push(json!({
                                "device": device,
                                "description": device,
                                "permission_denied": false,
                            }));
                        }
                    }
                }
            }
        }
        Ok(json!(ports))
    }
    #[cfg(not(feature = "serial"))]
    Ok(json!([]))
}

#[tauri::command]
pub async fn api_ble_available() -> AppResult<Value> {
    // Android: bridge BLE is always present; no probe.
    #[cfg(all(feature = "ble", target_os = "android"))]
    return Ok(json!({"available": true, "missing": [], "install_cmd": ""}));

    // Linux/BSD desktop keeps its own adapter probe: BlueZ-specific hints the
    // shared probe does not produce.
    #[cfg(all(
        feature = "ble",
        not(target_os = "ios"),
        not(target_os = "android"),
        not(target_os = "macos")
    ))]
    match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        rns_interface::ble_rnode::ble_adapter_present(),
    )
    .await
    {
        Ok(Ok(true)) => {
            return Ok(json!({"available": true, "missing": [], "install_cmd": ""}));
        }
        Ok(Ok(false)) => {
            #[cfg(target_os = "linux")]
            return Ok(json!({
                "available": false,
                "missing": [
                    "No BLE adapter found. If your machine has Bluetooth, ensure bluetoothd is running: sudo systemctl start bluetooth"
                ],
                "install_cmd": "",
            }));
            #[cfg(not(target_os = "linux"))]
            return Ok(json!({
                "available": false,
                "missing": ["No BLE adapter found"],
                "install_cmd": "",
            }));
        }
        Ok(Err(e)) => {
            #[cfg(target_os = "linux")]
            {
                let lower = e.to_lowercase();
                let hint = if lower.contains("serviceunknown")
                    || lower.contains("org.bluez")
                    || lower.contains("not provided by any .service")
                {
                    Some("BlueZ daemon not running — try `sudo systemctl start bluetooth`")
                } else if lower.contains("permission") || lower.contains("not authorized") {
                    Some(
                        "BlueZ rejected the request — add your user to the `bluetooth` group (or matching polkit rule) and re-login",
                    )
                } else {
                    None
                };
                let missing = match hint {
                    Some(h) => vec![format!("{e} — {h}")],
                    None => vec![e],
                };
                return Ok(json!({"available": false, "missing": missing, "install_cmd": ""}));
            }
            #[cfg(not(target_os = "linux"))]
            return Ok(json!({"available": false, "missing": [e], "install_cmd": ""}));
        }
        Err(_) => {
            return Ok(
                json!({"available": false, "missing": ["BLE check timed out"], "install_cmd": ""}),
            );
        }
    }

    // Complement of the two arms above: iOS, macOS, and no-`ble` builds match
    // the shared probe exactly.
    #[cfg(any(not(feature = "ble"), target_os = "ios", target_os = "macos"))]
    {
        let probe = ble_platform_probe().await;
        let mut body = json!({
            "available": probe.available,
            "missing": probe.missing,
            "install_cmd": "",
        });
        if let Some(auth) = probe.auth_state {
            body["auth_state"] = json!(auth);
        }
        Ok(body)
    }
}

#[tauri::command]
pub async fn api_ble_scan() -> AppResult<Value> {
    #[cfg(feature = "ble")]
    {
        match tokio::time::timeout(
            std::time::Duration::from_secs(8),
            rns_interface::ble_rnode::scan_ble_devices(5),
        )
        .await
        {
            Ok(Ok(devices)) => Ok(json!({"devices": devices, "available": true, "error": null})),
            Ok(Err(e)) => Ok(json!({"devices": [], "available": true, "error": e})),
            Err(_) => Ok(json!({"devices": [], "available": false, "error": "BLE scan timed out"})),
        }
    }
    #[cfg(not(feature = "ble"))]
    Ok(json!({"devices": [], "available": false, "error": null}))
}

#[tauri::command]
pub async fn api_ble_peer_available() -> AppResult<Value> {
    // Android returns the raw JNI payload: extra permission-detail keys the
    // shared probe does not model.
    #[cfg(all(feature = "ble", target_os = "android"))]
    return Ok(android_ble_peer_availability_payload());

    #[cfg(not(all(feature = "ble", target_os = "android")))]
    {
        let probe = ble_platform_probe().await;
        let mut body = json!({
            "available": probe.available,
            "missing": probe.missing,
        });
        if let Some(auth) = probe.auth_state {
            body["auth_state"] = json!(auth);
        }
        Ok(body)
    }
}

#[tauri::command]
pub async fn api_ble_peer_status(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let enabled = db::spawn_db(state.db.clone(), move |p| {
        let enabled = db::get_setting(&p, "ble_peer_enabled")
            .map(|v| v == "1")
            .unwrap_or(false);
        let expires_at = db::get_setting(&p, "ble_peer_expires_at")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        enabled && (expires_at == 0 || expires_at > now_secs)
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "ble_peer_status db task panicked");
        Default::default()
    });

    let probe = ble_platform_probe().await;

    let peer_count = state
        .ble_peer_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let peer_state = if !enabled {
        "off"
    } else if probe.permission_required {
        "permission_needed"
    } else if !probe.available {
        "unavailable"
    } else if peer_count > 0 {
        "on"
    } else {
        "starting"
    };

    let mut body = json!({
        "enabled": enabled,
        "available": probe.available,
        "missing": probe.missing,
        "state": peer_state,
        "peer_count": peer_count,
    });
    if let Some(a) = probe.auth_state {
        body["auth_state"] = json!(a);
    }
    Ok(body)
}

#[tauri::command]
pub async fn api_connection_history(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let history = db::spawn_db(state.db.clone(), |p| db::get_connection_history(&p, 10))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "connection_history db task panicked");
            Default::default()
        });
    Ok(json!(history))
}

#[tauri::command]
pub async fn api_delete_connection_history(
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> AppResult<Value> {
    db::spawn_db(state.db.clone(), move |p| {
        db::delete_connection_history(&p, id)
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "delete_connection_history db task panicked");
        Default::default()
    });
    Ok(json!(null))
}

#[derive(Deserialize)]
pub struct TransportModeArgs {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_network_type")]
    pub network_type: String,
}

fn default_mode() -> String {
    "off".to_string()
}
fn default_network_type() -> String {
    "unknown".to_string()
}

const PUBLIC_TCP_TRANSPORT_CONNECT_LIMIT_MESSAGE: &str =
    "Disable Transport Mode before connecting to more than 1 public server.";
const PUBLIC_TCP_TRANSPORT_ENABLE_LIMIT_MESSAGE: &str =
    "Transport Mode can't be enabled while connected to more than 1 public server.";

const PUBLIC_TCP_ENDPOINTS: &[(&str, u16, &str)] = &[
    ("1.ratspeak.org", 4141, "ratspeak-ruby"),
    ("2.ratspeak.org", 4242, "ratspeak-emerald"),
    ("rns.ratspeak.org", 4242, "ratspeak-emerald"),
    ("3.ratspeak.org", 4343, "ratspeak-diamond"),
    ("rns.beleth.net", 4242, "beleth"),
    ("rmap.world", 4242, "rmap"),
];

fn normalise_public_tcp_host(host: &str) -> String {
    let mut value = host.trim().to_ascii_lowercase();
    if let Some((_, tail)) = value.split_once("://") {
        value = tail.to_string();
    }
    if let Some((head, _)) = value.split_once('/') {
        value = head.to_string();
    }
    value.trim_end_matches('.').to_string()
}

fn public_tcp_server_id(host: &str, port: u16) -> Option<&'static str> {
    let host = normalise_public_tcp_host(host);
    PUBLIC_TCP_ENDPOINTS
        .iter()
        .find_map(|(public_host, public_port, id)| {
            (host == *public_host && port == *public_port).then_some(*id)
        })
}

fn public_tcp_server_id_from_entry(entry: &Value) -> Option<&'static str> {
    public_tcp_server_id(
        &cfg_str(entry, "target_host")?,
        cfg_u16(entry, "target_port")?,
    )
}

fn push_unique_public_server_id(ids: &mut Vec<&'static str>, id: &'static str) {
    if !ids.contains(&id) {
        ids.push(id);
    }
}

fn projected_enabled_public_tcp_server_ids(
    ifaces: &Value,
    replace_name: Option<&str>,
    candidate: Option<&'static str>,
) -> Vec<&'static str> {
    let mut ids = Vec::new();
    if let Some(entries) = ifaces.get("tcp_client").and_then(Value::as_array) {
        for entry in entries {
            if replace_name
                .is_some_and(|name| entry.get("name").and_then(Value::as_str) == Some(name))
            {
                continue;
            }
            if !cfg_bool_default_true(entry, "enabled") {
                continue;
            }
            if let Some(id) = public_tcp_server_id_from_entry(entry) {
                push_unique_public_server_id(&mut ids, id);
            }
        }
    }
    if let Some(id) = candidate {
        push_unique_public_server_id(&mut ids, id);
    }
    ids
}

fn enabled_public_tcp_server_count(ifaces: &Value) -> usize {
    projected_enabled_public_tcp_server_ids(ifaces, None, None).len()
}

fn auto_transport_base_enabled_for_interfaces(ifaces: &Value, network_type: &str) -> bool {
    transport_auto_network_allows(network_type)
        && has_enabled_non_lora_transport_interface(ifaces)
        && !has_enabled_lora_interface(ifaces)
}

fn transport_auto_network_allows(network_type: &str) -> bool {
    match network_type.trim().to_ascii_lowercase().as_str() {
        "wifi" | "ethernet" => true,
        // Desktop WebViews usually do not expose the active network type.
        // Mobile native network callbacks provide wifi/cellular/none, so keep
        // unknown conservative there.
        "unknown" => !cfg!(any(target_os = "android", target_os = "ios")),
        _ => false,
    }
}

fn interface_group_has_enabled(ifaces: &Value, key: &str) -> bool {
    ifaces
        .get(key)
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| cfg_bool_default_true(entry, "enabled"))
        })
}

fn has_enabled_lora_interface(ifaces: &Value) -> bool {
    interface_group_has_enabled(ifaces, "rnode")
}

fn has_enabled_non_lora_transport_interface(ifaces: &Value) -> bool {
    [
        "auto",
        "tcp_client",
        "tcp_server",
        "backbone_client",
        "backbone_server",
    ]
    .into_iter()
    .any(|key| interface_group_has_enabled(ifaces, key))
}

fn auto_transport_enabled_for_interfaces(ifaces: &Value, network_type: &str) -> bool {
    auto_transport_base_enabled_for_interfaces(ifaces, network_type)
        && enabled_public_tcp_server_count(ifaces) <= 1
}

fn auto_transport_enabled(config_dir: &std::path::Path, network_type: &str) -> bool {
    let ifaces = crate::rns_config::get_all_interfaces(config_dir);
    auto_transport_enabled_for_interfaces(&ifaces, network_type)
}

fn persisted_transport_network_type(state: &AppState) -> String {
    db::get_setting(&state.db, "transport_network_type").unwrap_or_else(default_network_type)
}

fn local_transport_runtime_allowed(state: &AppState) -> bool {
    state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.instance_mode))
        .is_none_or(|mode| mode != rns_runtime::reticulum::InstanceMode::Client)
}

fn configured_transport_enabled_for_interfaces(state: &AppState, ifaces: &Value) -> bool {
    match persisted_transport_mode(state).as_str() {
        "on" => true,
        "auto" => {
            let network_type = persisted_transport_network_type(state);
            auto_transport_enabled_for_interfaces(ifaces, &network_type)
        }
        _ => false,
    }
}

fn enforce_public_tcp_transport_connect_limit(
    state: &AppState,
    ifaces: &Value,
    replace_name: Option<&str>,
    candidate: Option<&'static str>,
) -> AppResult<()> {
    let Some(candidate) = candidate else {
        return Ok(());
    };
    if !configured_transport_enabled_for_interfaces(state, ifaces) {
        return Ok(());
    }
    if projected_enabled_public_tcp_server_ids(ifaces, replace_name, Some(candidate)).len() > 1 {
        return Err(AppError::conflict(
            PUBLIC_TCP_TRANSPORT_CONNECT_LIMIT_MESSAGE,
        ));
    }
    Ok(())
}

fn apply_transport_runtime_update(
    state: &AppState,
    mode: &str,
    configured_enable: bool,
    config_enable: bool,
) -> Result<Value, String> {
    let runtime_allowed = local_transport_runtime_allowed(state);
    let enable = configured_enable && runtime_allowed;

    let config_dir = active_rns_config_dir(state);
    if !with_rns_config_lock(state, || {
        crate::rns_config::set_transport_mode(&config_dir, config_enable)
    }) {
        return Err("Config write error".to_string());
    }

    if let Some(tx) = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.transport_tx.clone()))
    {
        let _ = tx.try_send(
            rns_transport::messages::TransportMessage::SetTransportEnabled { enabled: enable },
        );
    }

    Ok(json!({
        "mode": mode,
        "enabled": enable,
        "configured_enabled": configured_enable,
        "suppressed": configured_enable && !runtime_allowed,
    }))
}

pub(crate) fn reconcile_auto_transport_after_interface_change(state: &AppState, ifaces: &Value) {
    let mode = persisted_transport_mode(state);
    if mode != "auto" {
        return;
    }

    let network_type = persisted_transport_network_type(state);
    let configured_enable = auto_transport_enabled_for_interfaces(ifaces, &network_type);
    let runtime_allowed = local_transport_runtime_allowed(state);
    let enable = configured_enable && runtime_allowed;
    match apply_transport_runtime_update(state, "auto", configured_enable, enable) {
        Ok(payload) => state.emit_to_all("transport_mode_updated", payload),
        Err(error) => tracing::warn!(error = %error, "failed to reconcile transport mode config"),
    }
}

#[tauri::command]
pub async fn set_transport_mode(
    state: State<'_, Arc<AppState>>,
    args: TransportModeArgs,
) -> AppResult<Value> {
    let mode = normalize_transport_mode(&args.mode)
        .ok_or_else(|| AppError::bad_request("transport mode must be off | auto | on"))?;
    let config_dir = active_rns_config_dir(&state);
    let ifaces = crate::rns_config::get_all_interfaces(&config_dir);

    let would_enable_transport = match mode {
        "on" => true,
        "off" => false,
        "auto" => auto_transport_base_enabled_for_interfaces(&ifaces, &args.network_type),
        _ => false,
    };
    if would_enable_transport && enabled_public_tcp_server_count(&ifaces) > 1 {
        return Err(AppError::conflict(
            PUBLIC_TCP_TRANSPORT_ENABLE_LIMIT_MESSAGE,
        ));
    }
    let configured_enable = match mode {
        "on" => true,
        "off" => false,
        "auto" => auto_transport_enabled_for_interfaces(&ifaces, &args.network_type),
        _ => false,
    };
    let runtime_allowed = local_transport_runtime_allowed(&state);
    let enable = configured_enable && runtime_allowed;

    let mode_for_db = mode.to_string();
    let network_type_for_db = args.network_type.clone();
    db::spawn_db(state.db.clone(), move |p| {
        db::try_set_setting(&p, "transport_mode", &mode_for_db)?;
        db::try_set_setting(&p, "transport_network_type", &network_type_for_db)
    })
    .await
    .map_err(|_| AppError::internal("set_transport_mode db task panicked"))?
    .map_err(|e| AppError::database_unavailable(format!("Failed to save transport mode: {e}")))?;

    let config_enable = if mode == "on" {
        configured_enable
    } else {
        enable
    };
    let payload = apply_transport_runtime_update(&state, mode, configured_enable, config_enable)
        .map_err(AppError::internal)?;
    state.emit_to_all("transport_mode_updated", payload.clone());
    Ok(payload)
}

#[derive(Deserialize)]
pub struct NetworkTypeArgs {
    #[serde(default = "default_network_type")]
    pub network_type: String,
}

#[tauri::command]
pub async fn network_type_changed(
    state: State<'_, Arc<AppState>>,
    args: NetworkTypeArgs,
) -> AppResult<Value> {
    // Android: tear down + respawn AutoInterface on WiFi change because
    // multicast joins are scoped to the NIC's scope_id at creation time.
    #[cfg(target_os = "android")]
    if matches!(args.network_type.as_str(), "wifi" | "ethernet") {
        let st: Arc<AppState> = Arc::clone(&state);
        tokio::spawn(async move {
            respawn_android_auto_interfaces(st).await;
        });
    }

    let network_type_for_db = args.network_type.clone();
    db::spawn_db(state.db.clone(), move |p| {
        db::try_set_setting(&p, "transport_network_type", &network_type_for_db)
    })
    .await
    .map_err(|_| AppError::internal("network_type_changed db task panicked"))?
    .map_err(|e| AppError::database_unavailable(format!("Failed to save network type: {e}")))?;
    let mode = persisted_transport_mode(&state);
    if mode != "auto" {
        return Ok(json!({ "mode": mode, "updated": false }));
    }

    let config_dir = active_rns_config_dir(&state);
    let configured_enable = auto_transport_enabled(&config_dir, &args.network_type);
    let runtime_allowed = local_transport_runtime_allowed(&state);
    let enable = configured_enable && runtime_allowed;
    let payload = apply_transport_runtime_update(&state, "auto", configured_enable, enable)
        .map_err(AppError::internal)?;
    state.emit_to_all("transport_mode_updated", payload.clone());
    Ok(payload)
}

#[cfg(target_os = "android")]
async fn respawn_android_auto_interfaces(state: Arc<AppState>) {
    let auto_configs: Vec<rns_interface::auto::AutoInterfaceConfig> = {
        let config_dir = active_rns_config_dir(&state);
        let v = crate::rns_config::get_all_interfaces(&config_dir);
        v.get("auto")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|e| cfg_bool_default_true(e, "enabled"))
                    .filter_map(auto_runtime_config_from_entry)
                    .collect()
            })
            .unwrap_or_default()
    };

    if auto_configs.is_empty() {
        return;
    }

    let handle = match state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()))
    {
        Some(h) => h,
        None => return,
    };

    for config in auto_configs {
        let name = config.name.clone();
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
            for iface in stats {
                if iface.name == name {
                    rns_runtime::reticulum::teardown_interface(&handle, iface.id).await;
                    break;
                }
            }
        }

        let spawn_res = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            rns_runtime::reticulum::spawn_auto_interface_runtime_with_config(&handle, config),
        )
        .await;
        match spawn_res {
            Ok(Ok(_)) => {
                tracing::info!(
                    interface = %name,
                    "AutoInterface respawned after network change"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    interface = %name,
                    error = %e,
                    "AutoInterface respawn failed after network change"
                );
            }
            Err(_) => {
                tracing::warn!(
                    interface = %name,
                    "AutoInterface respawn timed out after network change"
                );
            }
        }
    }

    let ifaces = crate::rns_config::get_all_interfaces(&active_rns_config_dir(&state));
    emit_hub_interfaces(&state, ifaces);
}

#[tauri::command]
pub async fn set_auto_announce(state: State<'_, Arc<AppState>>, interval: u64) -> AppResult<Value> {
    // 0 disables; otherwise clamp to 15min..48h.
    let interval = if interval == 0 {
        0
    } else {
        interval.clamp(900, 172800)
    };

    db::spawn_db(state.db.clone(), move |p| {
        db::try_set_setting(&p, "auto_announce_interval", &interval.to_string())
    })
    .await
    .map_err(|_| AppError::internal("set_auto_announce db task panicked"))?
    .map_err(|e| {
        AppError::database_unavailable(format!("Failed to save announce interval: {e}"))
    })?;

    let _ = state.announce_interval_tx.send(interval);

    state.emit_to_all("auto_announce_updated", json!({ "interval": interval }));
    tracing::info!("Auto-announce interval set to {interval}s");
    Ok(json!({ "interval": interval }))
}

#[tauri::command]
pub async fn api_app_settings(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let hw_timeout = db::spawn_db(state.db.clone(), |p| {
        db::get_setting(&p, "hardware_session_timeout")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    })
    .await
    .unwrap_or(0);
    Ok(json!({
        "auto_announce_interval": *state.announce_interval_rx.borrow(),
        "announce_ratspeak_usage": state.announce_ratspeak_usage_enabled(),
        "peers_sort": persisted_peers_sort(&state),
        "hardware_session_timeout": hw_timeout,
    }))
}

/// Auto-lock timeout for hardware identities (seconds; 0 = off). Applies on the
/// next unlock/boot — the running session keeps its current timer.
#[tauri::command]
pub async fn set_hardware_lock_timeout(
    state: State<'_, Arc<AppState>>,
    seconds: u64,
) -> AppResult<Value> {
    let seconds = if seconds == 0 {
        0
    } else {
        seconds.clamp(60, 86400)
    };
    db::spawn_db(state.db.clone(), move |p| {
        db::try_set_setting(&p, "hardware_session_timeout", &seconds.to_string())
    })
    .await
    .map_err(|_| AppError::internal("set_hardware_lock_timeout db task panicked"))?
    .map_err(|e| AppError::database_unavailable(format!("Failed to save lock timeout: {e}")))?;

    state.emit_to_all(
        "app_settings_updated",
        json!({ "hardware_session_timeout": seconds }),
    );
    tracing::info!("Hardware auto-lock timeout set to {seconds}s");
    Ok(json!({ "hardware_session_timeout": seconds }))
}

#[tauri::command]
pub async fn set_peers_sort(state: State<'_, Arc<AppState>>, sort: String) -> AppResult<Value> {
    let normalized = normalize_peers_sort(&sort)
        .ok_or_else(|| AppError::bad_request("peers sort must be name | hops | last_seen"))?;
    let persisted = normalized.to_string();

    db::spawn_db(state.db.clone(), move |p| {
        db::try_set_setting(&p, "peers_sort", &persisted)
    })
    .await
    .map_err(|_| AppError::internal("set_peers_sort db task panicked"))?
    .map_err(|e| AppError::database_unavailable(format!("Failed to save peers sort: {e}")))?;

    state.emit_to_all(
        "app_settings_updated",
        json!({
            "auto_announce_interval": *state.announce_interval_rx.borrow(),
            "announce_ratspeak_usage": state.announce_ratspeak_usage_enabled(),
            "peers_sort": normalized,
        }),
    );
    Ok(json!({ "sort": normalized }))
}

#[tauri::command]
pub async fn set_announce_ratspeak_usage(
    state: State<'_, Arc<AppState>>,
    enabled: bool,
) -> AppResult<Value> {
    let persisted = if enabled { "1" } else { "0" };
    db::spawn_db(state.db.clone(), move |p| {
        db::try_set_setting(&p, "announce_ratspeak_usage", persisted)
    })
    .await
    .map_err(|_| AppError::internal("set_announce_ratspeak_usage db task panicked"))?
    .map_err(|e| AppError::database_unavailable(format!("Failed to save privacy setting: {e}")))?;

    state.set_announce_ratspeak_usage_enabled(enabled);
    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        mgr.announce_ratspeak_usage = enabled;
    }

    state.emit_to_all(
        "app_settings_updated",
        json!({
            "auto_announce_interval": *state.announce_interval_rx.borrow(),
            "announce_ratspeak_usage": enabled,
            "peers_sort": persisted_peers_sort(&state),
        }),
    );
    Ok(json!({ "enabled": enabled }))
}

#[tauri::command]
pub async fn api_notification_settings(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    Ok(json!({
        "enabled": state.native_notifications_enabled(),
        "ios_stubbed": cfg!(target_os = "ios"),
    }))
}

#[tauri::command]
pub async fn set_desktop_notifications(
    state: State<'_, Arc<AppState>>,
    enabled: bool,
) -> AppResult<Value> {
    let persisted = if enabled { "1" } else { "0" };
    db::spawn_db(state.db.clone(), move |p| {
        db::try_set_setting(&p, "native_notifications_enabled", persisted)?;
        db::try_set_setting(&p, "desktop_notifications_enabled", persisted)
    })
    .await
    .map_err(|_| AppError::internal("set_desktop_notifications db task panicked"))?
    .map_err(|e| {
        AppError::database_unavailable(format!("Failed to save notification setting: {e}"))
    })?;
    state.set_native_notifications_enabled(enabled);

    state.emit_to_all(
        "desktop_notifications_updated",
        json!({ "enabled": enabled }),
    );
    tracing::info!(
        "Desktop notifications {}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(json!({ "enabled": enabled }))
}

#[derive(Deserialize)]
pub struct AddLoraArgs {
    #[serde(default = "default_lora_name")]
    pub name: String,
    pub port: String,
    #[serde(default)]
    pub region_key: Option<String>,
    #[serde(default)]
    pub preset_key: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub custom_params: bool,
    #[serde(default = "default_frequency")]
    pub frequency: u64,
    #[serde(default = "default_bandwidth")]
    pub bandwidth: u64,
    #[serde(default = "default_sf")]
    pub spreading_factor: u8,
    #[serde(default = "default_cr")]
    pub coding_rate: u8,
    #[serde(default = "default_tx")]
    pub tx_power: i8,
    #[serde(default)]
    pub airtime_limit_short: Option<f64>,
    #[serde(default)]
    pub airtime_limit_long: Option<f64>,
}

fn default_lora_name() -> String {
    "LoRa".to_string()
}
fn default_frequency() -> u64 {
    ratspeak_core::radio::default_rnode_params().frequency
}
fn default_bandwidth() -> u64 {
    ratspeak_core::radio::default_rnode_params().bandwidth
}
fn default_sf() -> u8 {
    ratspeak_core::radio::default_rnode_params().spreading_factor
}
fn default_cr() -> u8 {
    ratspeak_core::radio::default_rnode_params().coding_rate
}
fn default_tx() -> i8 {
    ratspeak_core::radio::default_rnode_params().tx_power
}

fn normalize_lora_interface_mode(mode: Option<&str>) -> AppResult<&'static str> {
    crate::rns_config::normalize_rnode_interface_mode(mode)
        .ok_or_else(|| AppError::bad_request("Invalid RNode interface mode"))
}

fn rnode_runtime_mode(mode: &str) -> rns_interface::traits::InterfaceMode {
    crate::rns_config::rnode_interface_mode_value(Some(mode))
        .unwrap_or(rns_interface::traits::InterfaceMode::Full)
}

const RNODE_TCP_SCHEME: &str = "tcp://";
const RNODE_TCP_DEFAULT_PORT: u16 = 7633;

fn is_rnode_tcp_port(port: &str) -> bool {
    port.get(..RNODE_TCP_SCHEME.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RNODE_TCP_SCHEME))
}

fn normalise_rnode_port(port: &str) -> AppResult<String> {
    if !is_rnode_tcp_port(port) {
        return Ok(port.to_string());
    }
    let endpoint = port
        .get(RNODE_TCP_SCHEME.len()..)
        .ok_or_else(|| AppError::bad_request("Missing RNode TCP host"))?;
    normalise_rnode_tcp_endpoint(endpoint)
        .map(|endpoint| format!("{RNODE_TCP_SCHEME}{endpoint}"))
        .map_err(AppError::bad_request)
}

fn normalise_rnode_tcp_endpoint(endpoint: &str) -> Result<String, String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err("Missing RNode TCP host".to_string());
    }

    if let Some(rest) = endpoint.strip_prefix('[') {
        let Some(closing) = rest.find(']') else {
            return Err("Missing closing ']' in IPv6 TCP host".to_string());
        };
        let host = &rest[..closing];
        validate_rnode_tcp_host(host)?;
        let tail = &rest[closing + 1..];
        let port = if tail.is_empty() {
            RNODE_TCP_DEFAULT_PORT
        } else if let Some(port) = tail.strip_prefix(':') {
            parse_rnode_tcp_port(port)?
        } else {
            return Err("Unexpected text after bracketed TCP host".to_string());
        };
        return Ok(format!("[{host}]:{port}"));
    }

    validate_rnode_tcp_host(endpoint)?;
    let colon_count = endpoint.matches(':').count();
    match colon_count {
        0 => Ok(format!("{endpoint}:{RNODE_TCP_DEFAULT_PORT}")),
        1 => {
            let (host, port) = endpoint
                .rsplit_once(':')
                .expect("colon_count guarantees a separator");
            validate_rnode_tcp_host(host)?;
            Ok(format!("{host}:{}", parse_rnode_tcp_port(port)?))
        }
        _ => Ok(format!("[{endpoint}]:{RNODE_TCP_DEFAULT_PORT}")),
    }
}

fn validate_rnode_tcp_host(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("Missing RNode TCP host".to_string());
    }
    if host
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '/' | '?' | '#' | '[' | ']'))
    {
        return Err("Invalid RNode TCP host".to_string());
    }
    Ok(())
}

fn parse_rnode_tcp_port(port: &str) -> Result<u16, String> {
    if port.is_empty() {
        return Err("Missing RNode TCP port".to_string());
    }
    port.parse::<u16>()
        .map_err(|_| format!("Invalid RNode TCP port: {port}"))
}

#[derive(Debug, Clone, Copy)]
struct ResolvedLoraRadio {
    frequency: u64,
    bandwidth: u64,
    spreading_factor: u8,
    coding_rate: u8,
    tx_power: i8,
    region_key: Option<&'static str>,
    preset_key: Option<&'static str>,
    airtime_limit_short: Option<f64>,
    airtime_limit_long: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct LoraRadioArgs<'a> {
    region_key: Option<&'a str>,
    preset_key: Option<&'a str>,
    custom_params: bool,
    frequency: u64,
    bandwidth: u64,
    spreading_factor: u8,
    coding_rate: u8,
    tx_power: i8,
    airtime_limit_short: Option<f64>,
    airtime_limit_long: Option<f64>,
}

fn non_empty_key(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn validate_lora_radio_params(
    frequency: u64,
    bandwidth: u64,
    spreading_factor: u8,
    coding_rate: u8,
    tx_power: i8,
) -> AppResult<()> {
    if !(ratspeak_core::radio::RNODE_FREQUENCY_MIN_HZ
        ..=ratspeak_core::radio::RNODE_FREQUENCY_MAX_HZ)
        .contains(&frequency)
    {
        return Err(AppError::bad_request("Invalid radio frequency"));
    }
    if !(ratspeak_core::radio::RNODE_BANDWIDTH_MIN_HZ
        ..=ratspeak_core::radio::RNODE_BANDWIDTH_MAX_HZ)
        .contains(&bandwidth)
    {
        return Err(AppError::bad_request("Invalid radio bandwidth"));
    }
    if !(ratspeak_core::radio::RNODE_SPREADING_FACTOR_MIN
        ..=ratspeak_core::radio::RNODE_SPREADING_FACTOR_MAX)
        .contains(&spreading_factor)
    {
        return Err(AppError::bad_request("Invalid LoRa spreading factor"));
    }
    if !(ratspeak_core::radio::RNODE_CODING_RATE_MIN..=ratspeak_core::radio::RNODE_CODING_RATE_MAX)
        .contains(&coding_rate)
    {
        return Err(AppError::bad_request("Invalid LoRa coding rate"));
    }
    if !(ratspeak_core::radio::RNODE_TX_POWER_MIN_DBM
        ..=ratspeak_core::radio::RNODE_TX_POWER_MAX_DBM)
        .contains(&tx_power)
    {
        return Err(AppError::bad_request("Invalid LoRa TX power"));
    }
    Ok(())
}

fn validate_airtime_limit(value: Option<f64>, label: &str) -> AppResult<()> {
    if let Some(v) = value
        && !(v.is_finite() && (0.0..=100.0).contains(&v))
    {
        return Err(AppError::bad_request(format!(
            "Invalid {label} airtime limit"
        )));
    }
    Ok(())
}

fn rnode_preset_matches_params(
    preset: &ratspeak_core::radio::RnodePreset,
    bandwidth: u64,
    spreading_factor: u8,
    coding_rate: u8,
    tx_power: i8,
) -> bool {
    preset.bandwidth == bandwidth
        && preset.spreading_factor == spreading_factor
        && preset.coding_rate == coding_rate
        && preset.tx_power == tx_power
}

fn resolve_lora_radio_args(args: LoraRadioArgs<'_>) -> AppResult<ResolvedLoraRadio> {
    let LoraRadioArgs {
        region_key,
        preset_key,
        custom_params,
        frequency,
        bandwidth,
        spreading_factor,
        coding_rate,
        tx_power,
        airtime_limit_short,
        airtime_limit_long,
    } = args;
    validate_airtime_limit(airtime_limit_short, "short-term")?;
    validate_airtime_limit(airtime_limit_long, "long-term")?;
    let region_key = non_empty_key(region_key);
    let preset_key = non_empty_key(preset_key);
    if custom_params {
        validate_lora_radio_params(
            frequency,
            bandwidth,
            spreading_factor,
            coding_rate,
            tx_power,
        )?;

        let resolved_region_key = match region_key {
            Some(key) => {
                let region = ratspeak_core::radio::rnode_region(key)
                    .ok_or_else(|| AppError::bad_request("Invalid radio region"))?;
                if region.min <= frequency && frequency <= region.max {
                    Some(region.key)
                } else {
                    ratspeak_core::radio::infer_rnode_region(frequency)
                }
            }
            None => ratspeak_core::radio::infer_rnode_region(frequency),
        };
        let resolved_preset_key = match preset_key {
            Some(key) => {
                let preset = ratspeak_core::radio::rnode_preset(key)
                    .ok_or_else(|| AppError::bad_request("Invalid radio preset"))?;
                if rnode_preset_matches_params(
                    preset,
                    bandwidth,
                    spreading_factor,
                    coding_rate,
                    tx_power,
                ) {
                    Some(preset.key)
                } else {
                    ratspeak_core::radio::infer_rnode_preset(
                        bandwidth,
                        spreading_factor,
                        coding_rate,
                        tx_power,
                    )
                }
            }
            None => ratspeak_core::radio::infer_rnode_preset(
                bandwidth,
                spreading_factor,
                coding_rate,
                tx_power,
            ),
        };

        return Ok(ResolvedLoraRadio {
            frequency,
            bandwidth,
            spreading_factor,
            coding_rate,
            tx_power,
            region_key: resolved_region_key,
            preset_key: resolved_preset_key,
            airtime_limit_short,
            airtime_limit_long,
        });
    }

    if region_key.is_some() || preset_key.is_some() {
        let region_key = region_key.unwrap_or(ratspeak_core::radio::DEFAULT_RNODE_REGION_KEY);
        let preset_key = preset_key.unwrap_or(ratspeak_core::radio::DEFAULT_RNODE_PRESET_KEY);
        let params = ratspeak_core::radio::resolve_rnode_params(region_key, preset_key)
            .ok_or_else(|| AppError::bad_request("Invalid radio preset or region"))?;
        return Ok(ResolvedLoraRadio {
            frequency: params.frequency,
            bandwidth: params.bandwidth,
            spreading_factor: params.spreading_factor,
            coding_rate: params.coding_rate,
            tx_power: params.tx_power,
            region_key: ratspeak_core::radio::rnode_region(region_key).map(|r| r.key),
            preset_key: ratspeak_core::radio::rnode_preset(preset_key).map(|p| p.key),
            airtime_limit_short,
            airtime_limit_long,
        });
    }

    validate_lora_radio_params(
        frequency,
        bandwidth,
        spreading_factor,
        coding_rate,
        tx_power,
    )?;
    Ok(ResolvedLoraRadio {
        frequency,
        bandwidth,
        spreading_factor,
        coding_rate,
        tx_power,
        region_key: ratspeak_core::radio::infer_rnode_region(frequency),
        preset_key: ratspeak_core::radio::infer_rnode_preset(
            bandwidth,
            spreading_factor,
            coding_rate,
            tx_power,
        ),
        airtime_limit_short,
        airtime_limit_long,
    })
}

#[derive(Clone)]
enum EditableInterfaceConfig {
    RNode {
        name: String,
        port: String,
        mode: String,
        frequency: u64,
        bandwidth: u64,
        spreading_factor: u8,
        coding_rate: u8,
        tx_power: i8,
        airtime_limit_short: Option<f64>,
        airtime_limit_long: Option<f64>,
        public_map: RnodePublicMapSettings,
    },
    TcpClient {
        name: String,
        host: String,
        port: u16,
        ifac: InterfaceIfacSettings,
    },
    TcpServer {
        name: String,
        listen_ip: String,
        listen_port: u16,
    },
    BackboneClient {
        name: String,
        host: String,
        port: u16,
        prefer_ipv6: bool,
        connect_timeout: Option<u64>,
        max_reconnect_tries: Option<usize>,
        i2p_tunneled: bool,
        ifac: InterfaceIfacSettings,
    },
    BackboneServer {
        name: String,
        listen_ip: String,
        listen_port: u16,
        prefer_ipv6: bool,
        device: Option<String>,
    },
}

#[derive(Clone, Default)]
struct RnodePublicMapSettings {
    discoverable: bool,
    latitude: Option<f64>,
    longitude: Option<f64>,
    discovery_name: Option<String>,
}

impl RnodePublicMapSettings {
    fn config_args(&self) -> crate::rns_config::RnodePublicMapArgs<'_> {
        crate::rns_config::RnodePublicMapArgs {
            discoverable: self.discoverable,
            latitude: self.latitude,
            longitude: self.longitude,
            discovery_name: self.discovery_name.as_deref(),
        }
    }
}

enum RnodePublicMapUpdate {
    Preserve,
    Set(RnodePublicMapSettings),
}

impl EditableInterfaceConfig {
    fn name(&self) -> &str {
        match self {
            Self::RNode { name, .. }
            | Self::TcpClient { name, .. }
            | Self::TcpServer { name, .. }
            | Self::BackboneClient { name, .. }
            | Self::BackboneServer { name, .. } => name,
        }
    }

    fn rnode_port(&self) -> Option<&str> {
        match self {
            Self::RNode { port, .. } => Some(port),
            _ => None,
        }
    }
}

fn cfg_str(entry: &Value, key: &str) -> Option<String> {
    entry
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn cfg_non_empty_str(entry: &Value, key: &str) -> Option<String> {
    cfg_str(entry, key).filter(|s| !s.is_empty())
}

fn cfg_u64(entry: &Value, key: &str) -> Option<u64> {
    entry
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
}

fn cfg_u16(entry: &Value, key: &str) -> Option<u16> {
    cfg_u64(entry, key).and_then(|v| u16::try_from(v).ok())
}

fn cfg_u8(entry: &Value, key: &str) -> Option<u8> {
    cfg_u64(entry, key).and_then(|v| u8::try_from(v).ok())
}

fn cfg_i8(entry: &Value, key: &str) -> Option<i8> {
    entry
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<i8>().ok())
}

fn cfg_f64(entry: &Value, key: &str) -> Option<f64> {
    entry
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
}

fn cfg_usize(entry: &Value, key: &str) -> Option<usize> {
    cfg_u64(entry, key).and_then(|v| usize::try_from(v).ok())
}

fn cfg_bool(entry: &Value, key: &str) -> bool {
    entry
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1"))
        .unwrap_or(false)
}

fn cfg_bool_default_true(entry: &Value, key: &str) -> bool {
    entry
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| {
            !matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "false" | "no" | "0" | "off"
            )
        })
        .unwrap_or(true)
}

#[derive(Clone, Debug, Default)]
struct InterfaceIfacSettings {
    network_name: Option<String>,
    passphrase: Option<String>,
    ifac_size: Option<usize>,
}

impl InterfaceIfacSettings {
    fn is_enabled(&self) -> bool {
        self.network_name.as_ref().is_some_and(|s| !s.is_empty())
            || self.passphrase.as_ref().is_some_and(|s| !s.is_empty())
    }

    fn config_args(&self) -> crate::rns_config::InterfaceIfacArgs<'_> {
        crate::rns_config::InterfaceIfacArgs {
            network_name: self.network_name.as_deref(),
            passphrase: self.passphrase.as_deref(),
            ifac_size: self.ifac_size,
        }
    }

    fn runtime_config(&self) -> Option<rns_runtime::reticulum::RuntimeInterfaceIfacConfig> {
        self.is_enabled()
            .then(|| rns_runtime::reticulum::RuntimeInterfaceIfacConfig {
                network_name: self.network_name.clone(),
                passphrase: self.passphrase.clone(),
                ifac_size: self.ifac_size,
            })
    }
}

#[derive(Deserialize, Default)]
struct InterfaceIfacCommandFields {
    #[serde(default)]
    ifac_enabled: Option<bool>,
    #[serde(default)]
    ifac_network_name: Option<String>,
    #[serde(default)]
    ifac_passphrase: Option<String>,
}

fn ifac_settings_from_entry(entry: &Value) -> InterfaceIfacSettings {
    InterfaceIfacSettings {
        network_name: cfg_non_empty_str(entry, "network_name")
            .or_else(|| cfg_non_empty_str(entry, "networkname")),
        passphrase: cfg_non_empty_str(entry, "passphrase")
            .or_else(|| cfg_non_empty_str(entry, "pass_phrase")),
        ifac_size: cfg_usize(entry, "ifac_size"),
    }
}

fn ifac_settings_from_args(
    fields: &InterfaceIfacCommandFields,
    existing: Option<&InterfaceIfacSettings>,
) -> InterfaceIfacSettings {
    match fields.ifac_enabled {
        Some(true) => InterfaceIfacSettings {
            network_name: fields
                .ifac_network_name
                .as_deref()
                .map(|s| sanitize_text(s, 128))
                .filter(|s| !s.is_empty()),
            passphrase: fields
                .ifac_passphrase
                .as_deref()
                .map(|s| sanitize_text(s, 256))
                .filter(|s| !s.is_empty()),
            ifac_size: existing.and_then(|settings| settings.ifac_size),
        },
        Some(false) => InterfaceIfacSettings::default(),
        None => existing.cloned().unwrap_or_default(),
    }
}

fn cfg_rnode_mode(entry: &Value) -> String {
    let raw = cfg_str(entry, "mode").or_else(|| cfg_str(entry, "interface_mode"));
    crate::rns_config::normalize_rnode_interface_mode(raw.as_deref())
        .unwrap_or(crate::rns_config::RNODE_DEFAULT_INTERFACE_MODE)
        .to_string()
}

fn cfg_csv(entry: &Value, key: &str) -> Option<Vec<String>> {
    let values = cfg_str(entry, key)?
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn auto_runtime_config_from_entry(
    entry: &Value,
) -> Option<rns_interface::auto::AutoInterfaceConfig> {
    use std::str::FromStr;

    let discovery_scope = match cfg_str(entry, "discovery_scope") {
        Some(s) => rns_interface::auto::DiscoveryScope::from_str(&s).ok()?,
        None => rns_interface::auto::DiscoveryScope::Link,
    };
    let multicast_address_type = match cfg_str(entry, "multicast_address_type") {
        Some(s) => rns_interface::auto::McastAddrType::from_str(&s).ok()?,
        None => rns_interface::auto::McastAddrType::Temporary,
    };

    Some(rns_interface::auto::AutoInterfaceConfig {
        name: cfg_str(entry, "name").unwrap_or_else(|| "Local Network".to_string()),
        group_id: cfg_str(entry, "group_id")
            .unwrap_or_else(|| rns_interface::auto::DEFAULT_GROUP_ID.to_string()),
        discovery_scope,
        discovery_port: cfg_u16(entry, "discovery_port")
            .unwrap_or(rns_interface::auto::DISCOVERY_PORT),
        data_port: cfg_u16(entry, "data_port").unwrap_or(rns_interface::auto::DATA_PORT),
        multicast_address_type,
        devices: cfg_csv(entry, "devices"),
        ignored_devices: cfg_csv(entry, "ignored_devices").unwrap_or_default(),
        configured_bitrate: cfg_u64(entry, "configured_bitrate"),
        ..rns_interface::auto::AutoInterfaceConfig::default()
    })
}

fn find_config_interface(config_dir: &std::path::Path, group: &str, name: &str) -> Option<Value> {
    crate::rns_config::get_all_interfaces(config_dir)
        .get(group)
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|entry| entry.get("name").and_then(|v| v.as_str()) == Some(name))
                .cloned()
        })
}

fn interface_group_candidates(iface_type: &str) -> &'static [&'static str] {
    match iface_type {
        "rnode" | "lora" => &["rnode"],
        "auto" | "local" => &["auto"],
        "tcp_client" => &["tcp_client"],
        "tcp_server" => &["tcp_server"],
        "backbone_client" => &["backbone_client"],
        "backbone_server" => &["backbone_server"],
        "tcp" => &["tcp_client", "backbone_client"],
        "host" => &["tcp_server", "backbone_server"],
        _ => &[
            "rnode",
            "auto",
            "tcp_client",
            "tcp_server",
            "backbone_client",
            "backbone_server",
        ],
    }
}

fn find_config_interface_with_group(
    config_dir: &std::path::Path,
    iface_type: Option<&str>,
    name: &str,
) -> Option<(String, Value)> {
    let ifaces = crate::rns_config::get_all_interfaces(config_dir);
    let mut groups = Vec::new();
    if let Some(iface_type) = iface_type {
        groups.extend(interface_group_candidates(iface_type).iter().copied());
    }
    for group in [
        "rnode",
        "auto",
        "tcp_client",
        "tcp_server",
        "backbone_client",
        "backbone_server",
    ] {
        if !groups.contains(&group) {
            groups.push(group);
        }
    }

    for group in groups {
        if let Some(entry) = ifaces.get(group).and_then(Value::as_array).and_then(|arr| {
            arr.iter()
                .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
                .cloned()
        }) {
            return Some((group.to_string(), entry));
        }
    }

    None
}

fn rnode_config_from_entry(entry: &Value) -> Option<EditableInterfaceConfig> {
    Some(EditableInterfaceConfig::RNode {
        name: cfg_str(entry, "name")?,
        port: cfg_str(entry, "port")?,
        mode: cfg_rnode_mode(entry),
        frequency: cfg_u64(entry, "frequency").unwrap_or_else(default_frequency),
        bandwidth: cfg_u64(entry, "bandwidth").unwrap_or_else(default_bandwidth),
        spreading_factor: cfg_u8(entry, "spreadingfactor").unwrap_or_else(default_sf),
        coding_rate: cfg_u8(entry, "codingrate").unwrap_or_else(default_cr),
        tx_power: cfg_i8(entry, "txpower").unwrap_or_else(default_tx),
        airtime_limit_short: cfg_f64(entry, "airtime_limit_short"),
        airtime_limit_long: cfg_f64(entry, "airtime_limit_long"),
        public_map: RnodePublicMapSettings {
            discoverable: cfg_bool(entry, "discoverable"),
            latitude: cfg_f64(entry, "latitude"),
            longitude: cfg_f64(entry, "longitude"),
            discovery_name: cfg_non_empty_str(entry, "discovery_name"),
        },
    })
}

fn tcp_client_config_from_entry(entry: &Value) -> Option<EditableInterfaceConfig> {
    Some(EditableInterfaceConfig::TcpClient {
        name: cfg_str(entry, "name")?,
        host: cfg_str(entry, "target_host")?,
        port: cfg_u16(entry, "target_port")?,
        ifac: ifac_settings_from_entry(entry),
    })
}

fn tcp_server_config_from_entry(entry: &Value) -> Option<EditableInterfaceConfig> {
    Some(EditableInterfaceConfig::TcpServer {
        name: cfg_str(entry, "name")?,
        listen_ip: cfg_str(entry, "listen_ip").unwrap_or_else(default_tcp_server_ip),
        listen_port: cfg_u16(entry, "listen_port").unwrap_or_else(default_tcp_server_port),
    })
}

fn backbone_client_config_from_entry(entry: &Value) -> Option<EditableInterfaceConfig> {
    Some(EditableInterfaceConfig::BackboneClient {
        name: cfg_str(entry, "name")?,
        host: cfg_str(entry, "target_host")?,
        port: cfg_u16(entry, "target_port")?,
        prefer_ipv6: cfg_bool(entry, "prefer_ipv6"),
        connect_timeout: cfg_u64(entry, "connect_timeout"),
        max_reconnect_tries: cfg_usize(entry, "max_reconnect_tries"),
        i2p_tunneled: cfg_bool(entry, "i2p_tunneled"),
        ifac: ifac_settings_from_entry(entry),
    })
}

fn backbone_server_config_from_entry(entry: &Value) -> Option<EditableInterfaceConfig> {
    Some(EditableInterfaceConfig::BackboneServer {
        name: cfg_str(entry, "name")?,
        listen_ip: cfg_str(entry, "listen_on")
            .or_else(|| cfg_str(entry, "listen_ip"))
            .unwrap_or_else(default_backbone_listen_ip),
        listen_port: cfg_u16(entry, "listen_port").unwrap_or_else(default_backbone_server_port),
        prefer_ipv6: cfg_bool(entry, "prefer_ipv6"),
        device: cfg_str(entry, "device"),
    })
}

enum ResumableInterfaceConfig {
    Editable(EditableInterfaceConfig),
    Auto(rns_interface::auto::AutoInterfaceConfig),
}

impl ResumableInterfaceConfig {
    fn name(&self) -> &str {
        match self {
            Self::Editable(config) => config.name(),
            Self::Auto(config) => &config.name,
        }
    }

    fn rnode_port(&self) -> Option<&str> {
        match self {
            Self::Editable(config) => config.rnode_port(),
            Self::Auto(_) => None,
        }
    }
}

fn resumable_config_from_entry(group: &str, entry: &Value) -> Option<ResumableInterfaceConfig> {
    match group {
        "rnode" => rnode_config_from_entry(entry).map(ResumableInterfaceConfig::Editable),
        "auto" => auto_runtime_config_from_entry(entry).map(ResumableInterfaceConfig::Auto),
        "tcp_client" => tcp_client_config_from_entry(entry).map(ResumableInterfaceConfig::Editable),
        "tcp_server" => tcp_server_config_from_entry(entry).map(ResumableInterfaceConfig::Editable),
        "backbone_client" => {
            backbone_client_config_from_entry(entry).map(ResumableInterfaceConfig::Editable)
        }
        "backbone_server" => {
            backbone_server_config_from_entry(entry).map(ResumableInterfaceConfig::Editable)
        }
        _ => None,
    }
}

fn runtime_handle(state: &AppState) -> Option<rns_runtime::reticulum::ReticulumHandle> {
    state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()))
}

async fn teardown_rnode_interface_for_port(
    handle: &rns_runtime::reticulum::ReticulumHandle,
    id: u64,
    port: &str,
) {
    #[cfg(feature = "ble")]
    if port.starts_with("ble://") {
        rns_runtime::reticulum::teardown_ble_rnode_interface(handle, id).await;
        return;
    }

    #[cfg(target_os = "android")]
    if port.starts_with("androidusb://") {
        rns_runtime::reticulum::teardown_android_usb_rnode_interface(handle, id).await;
        return;
    }

    #[cfg(any(feature = "serial", feature = "rnode-tcp"))]
    {
        let _ = port;
        rns_runtime::reticulum::teardown_rnode_interface(handle, id).await;
        return;
    }

    #[allow(unreachable_code)]
    {
        let _ = port;
        rns_runtime::reticulum::teardown_interface(handle, id).await;
    }
}

async fn teardown_live_interface_by_name(
    state: &Arc<AppState>,
    name: &str,
    rnode_port: Option<&str>,
) {
    #[cfg(not(any(
        feature = "ble",
        feature = "serial",
        feature = "rnode-tcp",
        target_os = "android"
    )))]
    let _ = rnode_port;

    #[cfg(target_os = "android")]
    let native_ble_disconnect = rnode_port.is_some_and(|p| p.starts_with("ble://"));

    let Some(handle) = runtime_handle(state) else {
        // Android BLE GATT lives in the Kotlin bridge; without an explicit
        // disconnect the link lingers and the RNode cannot advertise again.
        #[cfg(target_os = "android")]
        if native_ble_disconnect {
            state.emit_to_all("ble_rnode_disconnect_native", json!({}));
        }
        return;
    };
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
        #[cfg(target_os = "android")]
        if native_ble_disconnect {
            state.emit_to_all("ble_rnode_disconnect_native", json!({}));
        }
        return;
    }
    let Ok(rns_transport::messages::TransportQueryResponse::InterfaceStats(stats)) = resp_rx.await
    else {
        #[cfg(target_os = "android")]
        if native_ble_disconnect {
            state.emit_to_all("ble_rnode_disconnect_native", json!({}));
        }
        return;
    };
    for iface in stats {
        if iface.name == name {
            if let Some(port) = rnode_port {
                teardown_rnode_interface_for_port(&handle, iface.id, port).await;
                break;
            } else {
                rns_runtime::reticulum::teardown_interface(&handle, iface.id).await;
                break;
            }
        }
    }

    // Close Android's native GATT link after the Rust RNode driver has sent
    // its normal detach/radio-off sequence through the still-open bridge.
    #[cfg(target_os = "android")]
    if native_ble_disconnect {
        state.emit_to_all("ble_rnode_disconnect_native", json!({}));
    }
}

async fn spawn_editable_interface(
    state: &Arc<AppState>,
    config: &EditableInterfaceConfig,
) -> Result<String, String> {
    let Some(handle) = runtime_handle(state) else {
        return Ok("Config saved (RNS not running)".to_string());
    };

    match config {
        EditableInterfaceConfig::RNode {
            name,
            port,
            mode,
            frequency,
            bandwidth,
            spreading_factor,
            coding_rate,
            tx_power,
            airtime_limit_short,
            airtime_limit_long,
            public_map: _,
        } => {
            #[cfg(all(
                not(feature = "serial"),
                not(feature = "rnode-tcp"),
                not(feature = "ble"),
                not(target_os = "android")
            ))]
            let _ = (
                name,
                frequency,
                bandwidth,
                spreading_factor,
                coding_rate,
                tx_power,
                mode,
                airtime_limit_short,
                airtime_limit_long,
            );

            if port.starts_with("ble://") {
                #[cfg(all(feature = "ble", target_os = "android"))]
                {
                    let tcp_port = std::net::TcpListener::bind("127.0.0.1:0")
                        .and_then(|l| l.local_addr().map(|a| a.port()))
                        .map_err(|e| format!("Failed to reserve BLE bridge port: {e}"))?;
                    let address = port.strip_prefix("ble://").unwrap_or(port);
                    state.emit_to_all(
                        "ble_rnode_connect_native",
                        json!({
                            "address": address,
                            "tcp_port": tcp_port,
                            "name": name,
                            "frequency": frequency,
                            "bandwidth": bandwidth,
                            "spreading_factor": spreading_factor,
                            "coding_rate": coding_rate,
                            "tx_power": tx_power,
                            "mode": mode,
                            "airtime_limit_short": airtime_limit_short,
                            "airtime_limit_long": airtime_limit_long,
                            "rollback_on_error": false,
                        }),
                    );
                    return Ok("Connecting via BLE".to_string());
                }
                #[cfg(all(feature = "ble", not(target_os = "android")))]
                {
                    let (id, _online) = rns_runtime::reticulum::spawn_ble_rnode_runtime(
                        &handle,
                        rns_runtime::reticulum::BleRnodeRuntimeArgs {
                            name,
                            port,
                            frequency: *frequency as u32,
                            bandwidth: *bandwidth as u32,
                            spreading_factor: *spreading_factor,
                            coding_rate: *coding_rate,
                            tx_power: *tx_power,
                            mode: rnode_runtime_mode(mode),
                            st_alock: airtime_limit_short.map(|v| v as f32),
                            lt_alock: airtime_limit_long.map(|v| v as f32),
                            flow_control: true,
                        },
                    )
                    .await?;
                    return Ok(format!("BLE LoRa interface active (#{id})"));
                }
                #[cfg(not(feature = "ble"))]
                {
                    return Err("BLE RNode unsupported on this build".to_string());
                }
            }

            if port.starts_with("androidusb://") {
                #[cfg(target_os = "android")]
                {
                    let device_name = port.strip_prefix("androidusb://").unwrap_or("");
                    if device_name.is_empty() {
                        return Err("Empty USB device name".to_string());
                    }
                    match rns_interface::android_usb::request_usb_permission(device_name).await {
                        Ok(true) => {}
                        Ok(false) => return Err("USB permission denied".to_string()),
                        Err(e) => return Err(format!("USB permission probe failed: {e}")),
                    }
                    let id = rns_runtime::reticulum::spawn_android_usb_rnode_runtime(
                        &handle,
                        name,
                        device_name,
                        *frequency as u32,
                        *bandwidth as u32,
                        *spreading_factor,
                        *coding_rate,
                        *tx_power,
                        rnode_runtime_mode(mode),
                        airtime_limit_short.map(|v| v as f32),
                        airtime_limit_long.map(|v| v as f32),
                        false,
                    )
                    .await?;
                    return Ok(format!("USB LoRa interface active (#{id})"));
                }
                #[cfg(not(target_os = "android"))]
                {
                    return Err("Android USB RNode unsupported on this build".to_string());
                }
            }

            #[cfg(any(feature = "serial", feature = "rnode-tcp"))]
            {
                #[cfg(not(feature = "serial"))]
                if !is_rnode_tcp_port(port) {
                    return Err("Serial RNode unsupported on this build".to_string());
                }

                let (id, _online) = rns_runtime::reticulum::spawn_rnode_runtime(
                    &handle,
                    rns_runtime::reticulum::RnodeRuntimeArgs {
                        name,
                        port,
                        frequency: *frequency as u32,
                        bandwidth: *bandwidth as u32,
                        spreading_factor: *spreading_factor,
                        coding_rate: *coding_rate,
                        tx_power: *tx_power,
                        mode: rnode_runtime_mode(mode),
                        st_alock: airtime_limit_short.map(|v| v as f32),
                        lt_alock: airtime_limit_long.map(|v| v as f32),
                        flow_control: false,
                    },
                )
                .await?;
                if is_rnode_tcp_port(port) {
                    Ok(format!("RNode TCP interface active (#{id})"))
                } else {
                    Ok(format!("RNode interface active (#{id})"))
                }
            }
            #[cfg(not(any(feature = "serial", feature = "rnode-tcp")))]
            {
                if is_rnode_tcp_port(port) {
                    Err("RNode TCP unsupported on this build".to_string())
                } else {
                    Err("Serial RNode unsupported on this build".to_string())
                }
            }
        }
        EditableInterfaceConfig::TcpClient {
            name,
            host,
            port,
            ifac,
        } => {
            let id = rns_runtime::reticulum::spawn_tcp_client_runtime_with_ifac(
                &handle,
                name,
                host,
                *port,
                ifac.runtime_config(),
            )
            .await?;
            Ok(format!("TCP interface active (#{id})"))
        }
        EditableInterfaceConfig::TcpServer {
            name,
            listen_ip,
            listen_port,
        } => {
            let id = rns_runtime::reticulum::spawn_tcp_server_runtime(
                &handle,
                name,
                listen_ip,
                *listen_port,
            )
            .await?;
            Ok(format!("TCP server listening (#{id})"))
        }
        EditableInterfaceConfig::BackboneClient {
            name,
            host,
            port,
            prefer_ipv6,
            connect_timeout,
            max_reconnect_tries,
            i2p_tunneled,
            ifac,
        } => {
            let _ = i2p_tunneled;
            let id = rns_runtime::reticulum::spawn_backbone_client_runtime_with_ifac(
                &handle,
                rns_runtime::reticulum::RuntimeBackboneClientConfig {
                    name,
                    host,
                    port: *port,
                    prefer_ipv6: *prefer_ipv6,
                    connect_timeout: *connect_timeout,
                    max_reconnect_tries: *max_reconnect_tries,
                    ifac: ifac.runtime_config(),
                },
            )
            .await?;
            Ok(format!("Backbone interface active (#{id})"))
        }
        EditableInterfaceConfig::BackboneServer {
            name,
            listen_ip,
            listen_port,
            prefer_ipv6,
            device,
        } => {
            let id = rns_runtime::reticulum::spawn_backbone_server_runtime(
                &handle,
                name,
                listen_ip,
                *listen_port,
                *prefer_ipv6,
                device.as_deref(),
            )
            .await?;
            Ok(format!("Backbone server listening (#{id})"))
        }
    }
}

async fn spawn_resumable_interface(
    state: &Arc<AppState>,
    config: ResumableInterfaceConfig,
) -> Result<String, String> {
    match config {
        ResumableInterfaceConfig::Editable(config) => {
            spawn_editable_interface(state, &config).await
        }
        ResumableInterfaceConfig::Auto(config) => {
            let Some(handle) = runtime_handle(state) else {
                return Ok("Config saved (RNS not running)".to_string());
            };
            let id =
                rns_runtime::reticulum::spawn_auto_interface_runtime_with_config(&handle, config)
                    .await?;
            Ok(format!("Local Network interface active (#{id})"))
        }
    }
}

async fn finish_interface_replace(
    state: Arc<AppState>,
    config_dir: PathBuf,
    operation: &'static str,
    old_config_content: String,
    old_runtime: EditableInterfaceConfig,
    new_runtime: EditableInterfaceConfig,
) {
    let old_name = old_runtime.name().to_string();
    emit_op_status_broadcast(
        &state,
        operation,
        "hub",
        "Restarting interface...",
        false,
        None,
    );
    teardown_live_interface_by_name(&state, &old_name, old_runtime.rnode_port()).await;

    if operation == "update_lora" && matches!(&new_runtime, EditableInterfaceConfig::RNode { .. }) {
        state.suppress_next_interface_reannounce(new_runtime.name());
    }

    match spawn_editable_interface(&state, &new_runtime).await {
        Ok(step) => {
            emit_op_status_broadcast(&state, operation, "hub", &step, true, None);
            if state
                .network_log_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                state.emit_network_event(
                    "interface",
                    &format!("Interface updated: {}", new_runtime.name()),
                    new_runtime.name(),
                    "standard",
                );
            }
        }
        Err(e) => {
            let restored = with_rns_config_lock(&state, || {
                crate::rns_config::write_config(&config_dir, &old_config_content)
            });
            let rollback = if restored {
                match spawn_editable_interface(&state, &old_runtime).await {
                    Ok(step) => format!(" Rolled back: {step}."),
                    Err(re) => format!(" Config restored, but old interface restart failed: {re}."),
                }
            } else {
                " Rollback config write failed.".to_string()
            };
            emit_op_status_broadcast(
                &state,
                operation,
                "hub",
                "Update failed",
                true,
                Some(&format!("{e}.{rollback}")),
            );
            if state
                .network_log_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                state.emit_network_event(
                    "error",
                    &format!("Interface update failed: {}", new_runtime.name()),
                    &format!("{e}.{rollback}"),
                    "essential",
                );
            }
        }
    }

    let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state, ifaces);
}

#[derive(Deserialize)]
pub struct InterfaceLifecycleArgs {
    pub name: String,
    #[serde(default)]
    pub iface_type: Option<String>,
}

#[tauri::command]
pub async fn pause_interface(
    state: State<'_, Arc<AppState>>,
    args: InterfaceLifecycleArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(&args.name, 64);
    let iface_type = args
        .iface_type
        .as_deref()
        .map(|s| sanitize_text(s, 64))
        .filter(|s| !s.is_empty());
    if name.is_empty() {
        return Err(AppError::bad_request("Interface name required"));
    }

    let config_dir = active_rns_config_dir(&state_arc);
    let rnode_port = with_rns_config_lock(&state_arc, || {
        let (group, entry) =
            find_config_interface_with_group(&config_dir, iface_type.as_deref(), &name)
                .ok_or_else(|| AppError::bad_request("Interface not found"))?;
        let rnode_port = (group == "rnode")
            .then(|| cfg_str(&entry, "port"))
            .flatten();
        let config_written = crate::rns_config::set_interface_enabled(&config_dir, &name, false);
        if !config_written {
            return Err(AppError::internal("Config write error"));
        }
        Ok::<_, AppError>(rnode_port)
    })?;

    emit_hub_interfaces(
        &state_arc,
        crate::rns_config::get_all_interfaces(&config_dir),
    );

    let st = Arc::clone(&state_arc);
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let iface_name = name;
        emit_op_status_broadcast(
            &st,
            "pause_interface",
            "hub",
            "Pausing interface...",
            false,
            None,
        );
        teardown_live_interface_by_name(&st, &iface_name, rnode_port.as_deref()).await;
        emit_op_status_broadcast(
            &st,
            "pause_interface",
            "hub",
            "Interface paused",
            true,
            None,
        );
        if st
            .network_log_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            st.emit_network_event(
                "interface",
                &format!("Interface paused: {iface_name}"),
                &iface_name,
                "standard",
            );
        }
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });

    Ok(json!({ "queued": true }))
}

#[tauri::command]
pub async fn resume_interface(
    state: State<'_, Arc<AppState>>,
    args: InterfaceLifecycleArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(&args.name, 64);
    let iface_type = args
        .iface_type
        .as_deref()
        .map(|s| sanitize_text(s, 64))
        .filter(|s| !s.is_empty());
    if name.is_empty() {
        return Err(AppError::bad_request("Interface name required"));
    }

    let config_dir = active_rns_config_dir(&state_arc);
    let runtime = with_rns_config_lock(&state_arc, || {
        let (group, entry) =
            find_config_interface_with_group(&config_dir, iface_type.as_deref(), &name)
                .ok_or_else(|| AppError::bad_request("Interface not found"))?;
        let runtime = resumable_config_from_entry(&group, &entry)
            .ok_or_else(|| AppError::bad_request("Unsupported interface"))?;
        if group == "tcp_client" {
            let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
            enforce_public_tcp_transport_connect_limit(
                &state_arc,
                &ifaces,
                Some(&name),
                public_tcp_server_id_from_entry(&entry),
            )?;
        }
        let config_written = crate::rns_config::set_interface_enabled(&config_dir, &name, true);
        if !config_written {
            return Err(AppError::internal("Config write error"));
        }
        Ok::<_, AppError>(runtime)
    })?;

    emit_hub_interfaces(
        &state_arc,
        crate::rns_config::get_all_interfaces(&config_dir),
    );

    let st = Arc::clone(&state_arc);
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let iface_name = runtime.name().to_string();
        let rnode_port = runtime.rnode_port().map(str::to_string);
        emit_op_status_broadcast(
            &st,
            "resume_interface",
            "hub",
            "Resuming interface...",
            false,
            None,
        );
        teardown_live_interface_by_name(&st, &iface_name, rnode_port.as_deref()).await;
        match spawn_resumable_interface(&st, runtime).await {
            Ok(step) => {
                emit_op_status_broadcast(&st, "resume_interface", "hub", &step, true, None);
                if st
                    .network_log_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    st.emit_network_event(
                        "interface",
                        &format!("Interface resumed: {iface_name}"),
                        &iface_name,
                        "standard",
                    );
                }
            }
            Err(e) => {
                // Failed resume returns to paused; the config entry is kept
                // so the user can retry.
                let _ = with_rns_config_lock(&st, || {
                    crate::rns_config::set_interface_enabled(&config_dir, &iface_name, false)
                });
                emit_op_status_broadcast(
                    &st,
                    "resume_interface",
                    "hub",
                    "Resume failed",
                    true,
                    Some(&e),
                );
                if st
                    .network_log_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    st.emit_network_event(
                        "error",
                        &format!("Interface resume failed: {iface_name}"),
                        &e,
                        "essential",
                    );
                }
            }
        }
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });

    Ok(json!({ "queued": true }))
}

#[tauri::command]
pub async fn add_lora_interface(
    state: State<'_, Arc<AppState>>,
    args: AddLoraArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(&args.name, 64);
    let port = normalise_rnode_port(&sanitize_text(&args.port, 256))?;
    let radio = resolve_lora_radio_args(LoraRadioArgs {
        region_key: args.region_key.as_deref(),
        preset_key: args.preset_key.as_deref(),
        custom_params: args.custom_params,
        frequency: args.frequency,
        bandwidth: args.bandwidth,
        spreading_factor: args.spreading_factor,
        coding_rate: args.coding_rate,
        tx_power: args.tx_power,
        airtime_limit_short: args.airtime_limit_short,
        airtime_limit_long: args.airtime_limit_long,
    })?;
    let mode = normalize_lora_interface_mode(args.mode.as_deref())?;
    let runtime_mode = rnode_runtime_mode(mode);

    let config_dir = active_rns_config_dir(&state_arc);
    emit_op_status_broadcast(
        &state_arc,
        "add_lora",
        "hub",
        "Writing config...",
        false,
        None,
    );

    let (fresh_add, existing_rnode_port, config_written) = with_rns_config_lock(&state_arc, || {
        // add_rnode_interface upserts by name; only entries this add creates
        // may be rolled back (deleted) on connect failure or cancel.
        let fresh_add = find_config_interface_with_group(&config_dir, None, &name).is_none();
        let existing_rnode_port = find_config_interface(&config_dir, "rnode", &name)
            .and_then(|entry| rnode_config_from_entry(&entry))
            .and_then(|config| config.rnode_port().map(str::to_string));
        let config_written = crate::rns_config::add_rnode_interface(
            &config_dir,
            crate::rns_config::RnodeInterfaceArgs {
                name: &name,
                port: &port,
                mode: Some(mode),
                frequency: radio.frequency,
                bandwidth: radio.bandwidth,
                spreading_factor: radio.spreading_factor,
                coding_rate: radio.coding_rate,
                tx_power: radio.tx_power,
                region_key: radio.region_key,
                preset_key: radio.preset_key,
                airtime_limit_short: radio.airtime_limit_short,
                airtime_limit_long: radio.airtime_limit_long,
                public_map: crate::rns_config::RnodePublicMapArgs::default(),
            },
        );
        (fresh_add, existing_rnode_port, config_written)
    });
    #[cfg(not(any(feature = "ble", target_os = "android")))]
    let _ = &existing_rnode_port;

    if !config_written {
        emit_op_status_broadcast(
            &state_arc,
            "add_lora",
            "hub",
            "Failed to write config",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }
    crate::commands::shared::mark_lora_add_freshness(&name, fresh_add);

    // USB-OTG: factory skips `androidusb://` on restart; user re-adds.
    #[cfg(target_os = "android")]
    if port.starts_with("androidusb://") {
        let device_name = port.strip_prefix("androidusb://").unwrap_or("").to_string();
        if device_name.is_empty() {
            emit_op_status_broadcast(
                &state_arc,
                "add_lora",
                "hub",
                "Invalid USB device name",
                true,
                Some("Empty device"),
            );
            return Err(AppError::bad_request("Empty USB device name"));
        }
        let st = Arc::clone(&state_arc);
        let iface_name = name.clone();
        let config_dir = config_dir.clone();
        let existing_rnode_port = existing_rnode_port.clone();
        tokio::spawn(async move {
            teardown_rnode_handoff_broadcast(&st, "ble://", "BLE").await;
            teardown_live_interface_by_name(&st, &iface_name, existing_rnode_port.as_deref()).await;

            emit_op_status_broadcast(
                &st,
                "add_lora",
                "hub",
                "Requesting USB permission...",
                false,
                None,
            );
            match rns_interface::android_usb::request_usb_permission(&device_name).await {
                Ok(true) => {}
                Ok(false) => {
                    emit_op_status_broadcast(
                        &st,
                        "add_lora",
                        "hub",
                        "USB permission not granted for device",
                        true,
                        Some("Permission denied"),
                    );
                    return;
                }
                Err(e) => {
                    emit_op_status_broadcast(
                        &st,
                        "add_lora",
                        "hub",
                        &format!("USB permission probe failed: {e}"),
                        true,
                        Some("JNI error"),
                    );
                    return;
                }
            }

            if let Some(rns) = runtime_handle(&st) {
                emit_op_status_broadcast(
                    &st,
                    "add_lora",
                    "hub",
                    "Opening USB serial...",
                    false,
                    None,
                );
                match rns_runtime::reticulum::spawn_android_usb_rnode_runtime(
                    &rns,
                    &iface_name,
                    &device_name,
                    radio.frequency as u32,
                    radio.bandwidth as u32,
                    radio.spreading_factor,
                    radio.coding_rate,
                    radio.tx_power,
                    runtime_mode,
                    radio.airtime_limit_short.map(|v| v as f32),
                    radio.airtime_limit_long.map(|v| v as f32),
                    false,
                )
                .await
                {
                    Ok(id) => {
                        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
                        emit_hub_interfaces(&st, ifaces);
                        emit_op_status_broadcast(
                            &st,
                            "add_lora",
                            "hub",
                            &format!("USB LoRa interface active (#{id})"),
                            true,
                            None,
                        );
                    }
                    Err(e) => {
                        emit_op_status_broadcast(
                            &st,
                            "add_lora",
                            "hub",
                            &format!("USB interface spawn failed: {e}"),
                            true,
                            Some("Spawn error"),
                        );
                    }
                }
            } else {
                emit_op_status_broadcast(
                    &st,
                    "add_lora",
                    "hub",
                    "Reticulum runtime not ready — retry after startup",
                    true,
                    Some("Runtime not ready"),
                );
            }
        });
        return Ok(json!({ "deferred": true, "transport": "androidusb" }));
    }

    #[cfg(feature = "ble")]
    if port.starts_with("ble://") {
        let st = Arc::clone(&state_arc);
        let name = name.clone();
        let port_str = port.clone();

        // Android: native Kotlin BLE bridge handles GATT. Emit
        // `ble_rnode_connect_native`; frontend invokes `ble_rnode_bridge_ready`
        // once the TCP bridge socket is up.
        #[cfg(target_os = "android")]
        {
            let tcp_port = match std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|l| l.local_addr().map(|a| a.port()))
            {
                Ok(p) => p,
                Err(e) => {
                    emit_op_status_broadcast(
                        &st,
                        "add_lora",
                        "hub",
                        "BLE setup failed",
                        true,
                        Some(&format!("Failed to reserve BLE bridge port: {e}")),
                    );
                    return Err(AppError::internal("BLE bridge port reserve failed"));
                }
            };

            let ble_address = port_str
                .strip_prefix("ble://")
                .unwrap_or(&port_str)
                .to_string();
            let st_a = Arc::clone(&st);
            let name_a = name.clone();
            let existing_rnode_port = existing_rnode_port.clone();
            tokio::spawn(async move {
                teardown_rnode_handoff_broadcast(&st_a, "androidusb://", "USB").await;
                teardown_live_interface_by_name(&st_a, &name_a, existing_rnode_port.as_deref())
                    .await;
                st_a.emit_to_all(
                    "ble_rnode_connect_native",
                    json!({
                        "address": ble_address,
                        "tcp_port": tcp_port,
                        "name": name_a,
                        "frequency": radio.frequency,
                        "bandwidth": radio.bandwidth,
                        "spreading_factor": radio.spreading_factor,
                        "coding_rate": radio.coding_rate,
                        "tx_power": radio.tx_power,
                        "mode": mode,
                        "airtime_limit_short": radio.airtime_limit_short,
                        "airtime_limit_long": radio.airtime_limit_long,
                        "rollback_on_error": fresh_add,
                    }),
                );
                emit_op_status_broadcast(
                    &st_a,
                    "add_lora",
                    "hub",
                    "Connecting via BLE...",
                    false,
                    None,
                );
            });
            return Ok(json!({ "deferred": true, "transport": "ble-android" }));
        }

        #[cfg(not(target_os = "android"))]
        {
            let name_for_status = name.clone();
            let config_dir = config_dir.clone();
            let existing_rnode_port = existing_rnode_port.clone();
            tokio::spawn(async move {
                emit_op_status_broadcast(
                    &st,
                    "add_lora",
                    "hub",
                    "Connecting via Bluetooth…",
                    false,
                    None,
                );

                if let Some(rns) = runtime_handle(&st) {
                    teardown_live_interface_by_name(&st, &name, existing_rnode_port.as_deref())
                        .await;
                    match rns_runtime::reticulum::spawn_ble_rnode_runtime(
                        &rns,
                        rns_runtime::reticulum::BleRnodeRuntimeArgs {
                            name: &name,
                            port: &port_str,
                            frequency: radio.frequency as u32,
                            bandwidth: radio.bandwidth as u32,
                            spreading_factor: radio.spreading_factor,
                            coding_rate: radio.coding_rate,
                            tx_power: radio.tx_power,
                            mode: runtime_mode,
                            st_alock: radio.airtime_limit_short.map(|v| v as f32),
                            lt_alock: radio.airtime_limit_long.map(|v| v as f32),
                            flow_control: true,
                        },
                    )
                    .await
                    {
                        Ok((id, online)) => {
                            emit_op_status_broadcast(
                                &st,
                                "add_lora",
                                "hub",
                                "Pair the radio when prompted — passkey is on the RNode display",
                                false,
                                None,
                            );
                            let start = std::time::Instant::now();
                            let timeout = std::time::Duration::from_secs(120);
                            loop {
                                if online.load(std::sync::atomic::Ordering::SeqCst) {
                                    emit_op_status_broadcast(
                                        &st,
                                        "add_lora",
                                        "hub",
                                        &format!("BLE LoRa interface active (#{id})"),
                                        true,
                                        None,
                                    );
                                    break;
                                }
                                if start.elapsed() > timeout {
                                    // Rollback only entries this add created;
                                    // pre-existing radios stay configured.
                                    if fresh_add {
                                        let _ = with_rns_config_lock(&st, || {
                                            crate::rns_config::remove_interface(
                                                &config_dir,
                                                &name_for_status,
                                            )
                                        });
                                        let ifaces =
                                            crate::rns_config::get_all_interfaces(&config_dir);
                                        emit_hub_interfaces(&st, ifaces);
                                    }
                                    emit_op_status_broadcast(
                                        &st,
                                        "add_lora",
                                        "hub",
                                        &format!(
                                            "BLE pairing timed out for '{name_for_status}'. Check that the RNode is in pairing mode and retry."
                                        ),
                                        true,
                                        Some("pairing_timeout"),
                                    );
                                    break;
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            }
                        }
                        Err(e) => {
                            emit_op_status_broadcast(
                                &st,
                                "add_lora",
                                "hub",
                                &format!("Config saved. BLE connect failed: {e}"),
                                true,
                                Some(&e),
                            );
                        }
                    }
                } else {
                    emit_op_status_broadcast(
                        &st,
                        "add_lora",
                        "hub",
                        "Config saved. BLE connect deferred (RNS not ready).",
                        true,
                        None,
                    );
                }

                let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
                emit_hub_interfaces(&st, ifaces);
            });
            return Ok(json!({ "deferred": true, "transport": "ble" }));
        }
    }

    #[cfg(any(feature = "serial", feature = "rnode-tcp"))]
    {
        let st = Arc::clone(&state_arc);
        let name_owned = name.clone();
        let port_str = port.clone();
        let is_tcp = is_rnode_tcp_port(&port_str);
        let config_dir = config_dir.clone();
        let existing_rnode_port = existing_rnode_port.clone();
        tokio::spawn(async move {
            #[cfg(not(feature = "serial"))]
            if !is_tcp {
                emit_op_status_broadcast(
                    &st,
                    "add_lora",
                    "hub",
                    "Serial RNode unsupported on this build",
                    true,
                    Some("serial feature not compiled"),
                );
                let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
                emit_hub_interfaces(&st, ifaces);
                return;
            }

            emit_op_status_broadcast(
                &st,
                "add_lora",
                "hub",
                if is_tcp {
                    "Connecting to RNode TCP endpoint..."
                } else {
                    "Opening serial port..."
                },
                false,
                None,
            );

            if let Some(rns) = runtime_handle(&st) {
                teardown_live_interface_by_name(&st, &name_owned, existing_rnode_port.as_deref())
                    .await;
                match rns_runtime::reticulum::spawn_rnode_runtime(
                    &rns,
                    rns_runtime::reticulum::RnodeRuntimeArgs {
                        name: &name_owned,
                        port: &port_str,
                        frequency: radio.frequency as u32,
                        bandwidth: radio.bandwidth as u32,
                        spreading_factor: radio.spreading_factor,
                        coding_rate: radio.coding_rate,
                        tx_power: radio.tx_power,
                        mode: runtime_mode,
                        st_alock: radio.airtime_limit_short.map(|v| v as f32),
                        lt_alock: radio.airtime_limit_long.map(|v| v as f32),
                        flow_control: false,
                    },
                )
                .await
                {
                    Ok((id, _online)) => {
                        let step = if is_tcp {
                            format!("RNode TCP interface active (#{id})")
                        } else {
                            format!("RNode interface active (#{id})")
                        };
                        emit_op_status_broadcast(&st, "add_lora", "hub", &step, true, None);
                    }
                    Err(e) => {
                        let step = if is_tcp {
                            format!("Config saved. RNode TCP connect failed: {e}")
                        } else {
                            format!("Config saved. Serial open failed: {e}")
                        };
                        emit_op_status_broadcast(&st, "add_lora", "hub", &step, true, Some(&e));
                    }
                }
            } else {
                emit_op_status_broadcast(
                    &st,
                    "add_lora",
                    "hub",
                    if is_tcp {
                        "Config saved. RNode TCP connect deferred (RNS not ready)."
                    } else {
                        "Config saved. Serial open deferred (RNS not ready)."
                    },
                    true,
                    None,
                );
            }

            let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
            emit_hub_interfaces(&st, ifaces);
        });
        Ok(
            json!({ "deferred": true, "transport": if is_rnode_tcp_port(&port) { "tcp" } else { "serial" } }),
        )
    }

    #[cfg(not(any(feature = "serial", feature = "rnode-tcp")))]
    {
        emit_op_status_broadcast(
            &state_arc,
            "add_lora",
            "hub",
            if is_rnode_tcp_port(&port) {
                "RNode TCP unsupported on this build"
            } else {
                "Serial RNode unsupported on this build"
            },
            true,
            Some("rnode feature not compiled"),
        );
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&state_arc, ifaces);
        Ok(json!({ "ok": false }))
    }
}

#[derive(Deserialize)]
pub struct UpdateLoraArgs {
    pub old_name: String,
    #[serde(default = "default_lora_name")]
    pub name: String,
    pub port: String,
    #[serde(default)]
    pub region_key: Option<String>,
    #[serde(default)]
    pub preset_key: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub custom_params: bool,
    #[serde(default = "default_frequency")]
    pub frequency: u64,
    #[serde(default = "default_bandwidth")]
    pub bandwidth: u64,
    #[serde(default = "default_sf")]
    pub spreading_factor: u8,
    #[serde(default = "default_cr")]
    pub coding_rate: u8,
    #[serde(default = "default_tx")]
    pub tx_power: i8,
    #[serde(default)]
    pub airtime_limit_short: Option<f64>,
    #[serde(default)]
    pub airtime_limit_long: Option<f64>,
    #[serde(default)]
    pub public_map: Option<UpdateLoraPublicMapArgs>,
}

#[derive(Deserialize)]
pub struct UpdateLoraPublicMapArgs {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
}

async fn resolve_rnode_public_map_update(
    state: &Arc<AppState>,
    args: Option<&UpdateLoraPublicMapArgs>,
) -> AppResult<RnodePublicMapUpdate> {
    let Some(args) = args else {
        return Ok(RnodePublicMapUpdate::Preserve);
    };
    if !args.enabled {
        return Ok(RnodePublicMapUpdate::Set(RnodePublicMapSettings::default()));
    }

    let latitude = args
        .latitude
        .filter(|v| v.is_finite())
        .ok_or_else(|| AppError::bad_request("Add a location before enabling public map."))?;
    if !(-90.0..=90.0).contains(&latitude) {
        return Err(AppError::bad_request(
            "Latitude must be between -90 and 90.",
        ));
    }
    let longitude = args
        .longitude
        .filter(|v| v.is_finite())
        .ok_or_else(|| AppError::bad_request("Add a location before enabling public map."))?;
    if !(-180.0..=180.0).contains(&longitude) {
        return Err(AppError::bad_request(
            "Longitude must be between -180 and 180.",
        ));
    }

    let display_name = active_identity_display_name_for_public_map(state).await?;
    Ok(RnodePublicMapUpdate::Set(RnodePublicMapSettings {
        discoverable: true,
        latitude: Some(latitude),
        longitude: Some(longitude),
        discovery_name: Some(display_name),
    }))
}

async fn active_identity_display_name_for_public_map(state: &Arc<AppState>) -> AppResult<String> {
    let active = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
        .await
        .map_err(|_| AppError::internal("active identity db task panicked"))?;
    let display_name = active
        .as_ref()
        .and_then(|identity| identity.get("display_name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if display_name.is_empty() {
        return Err(AppError::bad_request(
            "Set an identity display name before enabling public map.",
        ));
    }
    if display_name
        .chars()
        .any(|c| c == '\r' || c == '\n' || c == '\0' || c == '#')
    {
        return Err(AppError::bad_request(
            "Identity display name contains unsupported characters.",
        ));
    }
    Ok(display_name.to_string())
}

#[tauri::command]
pub async fn update_lora_interface(
    state: State<'_, Arc<AppState>>,
    args: UpdateLoraArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let old_name = sanitize_text(&args.old_name, 64);
    let name = sanitize_text(&args.name, 64);
    let port = normalise_rnode_port(&sanitize_text(&args.port, 256))?;
    if old_name.is_empty() || name.is_empty() || port.is_empty() {
        emit_op_status_broadcast(
            &state_arc,
            "update_lora",
            "hub",
            "Invalid parameters",
            true,
            Some("Name and device required"),
        );
        return Err(AppError::bad_request("Name and device required"));
    }
    let radio = resolve_lora_radio_args(LoraRadioArgs {
        region_key: args.region_key.as_deref(),
        preset_key: args.preset_key.as_deref(),
        custom_params: args.custom_params,
        frequency: args.frequency,
        bandwidth: args.bandwidth,
        spreading_factor: args.spreading_factor,
        coding_rate: args.coding_rate,
        tx_power: args.tx_power,
        airtime_limit_short: args.airtime_limit_short,
        airtime_limit_long: args.airtime_limit_long,
    })?;
    let mode = normalize_lora_interface_mode(args.mode.as_deref())?;
    let public_map_update =
        resolve_rnode_public_map_update(&state_arc, args.public_map.as_ref()).await?;

    let config_dir = active_rns_config_dir(&state_arc);
    let (old_runtime, old_config_content, config_written) =
        with_rns_config_lock(&state_arc, || {
            let old_entry = find_config_interface(&config_dir, "rnode", &old_name)
                .ok_or_else(|| AppError::bad_request("Interface not found"))?;
            let old_runtime = rnode_config_from_entry(&old_entry)
                .ok_or_else(|| AppError::bad_request("Invalid radio config"))?;
            let public_map = match &public_map_update {
                RnodePublicMapUpdate::Preserve => match &old_runtime {
                    EditableInterfaceConfig::RNode { public_map, .. } => public_map.clone(),
                    _ => RnodePublicMapSettings::default(),
                },
                RnodePublicMapUpdate::Set(public_map) => public_map.clone(),
            };
            let old_config_content =
                crate::rns_config::read_config(&config_dir).unwrap_or_default();
            let config_written = crate::rns_config::update_rnode_interface(
                &config_dir,
                &old_name,
                crate::rns_config::RnodeInterfaceArgs {
                    name: &name,
                    port: &port,
                    mode: Some(mode),
                    frequency: radio.frequency,
                    bandwidth: radio.bandwidth,
                    spreading_factor: radio.spreading_factor,
                    coding_rate: radio.coding_rate,
                    tx_power: radio.tx_power,
                    region_key: radio.region_key,
                    preset_key: radio.preset_key,
                    airtime_limit_short: radio.airtime_limit_short,
                    airtime_limit_long: radio.airtime_limit_long,
                    public_map: public_map.config_args(),
                },
            );
            Ok::<_, AppError>((old_runtime, old_config_content, config_written))
        })?;

    if !config_written {
        emit_op_status_broadcast(
            &state_arc,
            "update_lora",
            "hub",
            "Failed to write config",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let new_runtime = EditableInterfaceConfig::RNode {
        name: name.clone(),
        port,
        mode: mode.to_string(),
        frequency: radio.frequency,
        bandwidth: radio.bandwidth,
        spreading_factor: radio.spreading_factor,
        coding_rate: radio.coding_rate,
        tx_power: radio.tx_power,
        airtime_limit_short: radio.airtime_limit_short,
        airtime_limit_long: radio.airtime_limit_long,
        public_map: match public_map_update {
            RnodePublicMapUpdate::Preserve => match &old_runtime {
                EditableInterfaceConfig::RNode { public_map, .. } => public_map.clone(),
                _ => RnodePublicMapSettings::default(),
            },
            RnodePublicMapUpdate::Set(public_map) => public_map,
        },
    };
    emit_hub_interfaces(
        &state_arc,
        crate::rns_config::get_all_interfaces(&config_dir),
    );
    tokio::spawn(finish_interface_replace(
        Arc::clone(&state_arc),
        config_dir.clone(),
        "update_lora",
        old_config_content,
        old_runtime,
        new_runtime,
    ));
    Ok(json!({ "queued": true, "iface_name": name }))
}

/// BLE↔USB handoff: tear down the old side before adding the new transport.
#[cfg(target_os = "android")]
async fn teardown_rnode_handoff_broadcast(
    state: &Arc<AppState>,
    other_prefix: &str,
    friendly: &str,
) {
    let config_dir = active_rns_config_dir(state);
    let names = crate::rns_config::rnode_names_with_port_prefix(&config_dir, other_prefix);
    if names.is_empty() {
        return;
    }

    let rns_handle = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));

    for name in &names {
        emit_op_status_broadcast(
            state,
            "add_lora",
            "hub",
            &format!("Disconnecting {friendly} radio '{name}'..."),
            false,
            None,
        );
        if let Some(ref handle) = rns_handle {
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
                for iface in stats {
                    if &iface.name == name {
                        teardown_rnode_interface_for_port(handle, iface.id, other_prefix).await;
                        break;
                    }
                }
            }
        }
        let _ = with_rns_config_lock(state, || {
            crate::rns_config::remove_interface(&config_dir, name)
        });
        if state
            .network_log_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            state.emit_network_event(
                "interface",
                &format!("{friendly} RNode '{name}' disconnected (switching transport)"),
                name,
                "standard",
            );
        }
    }

    let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(state, ifaces);
}

#[tauri::command]
pub async fn remove_lora_interface(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(&name, 64);
    tokio::spawn(async move {
        let config_dir = active_rns_config_dir(&state_arc);

        let port = {
            let all = crate::rns_config::get_all_interfaces(&config_dir);
            all.get("rnode")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|entry| {
                        let n = entry.get("name").and_then(|v| v.as_str())?;
                        if n == name {
                            entry
                                .get("port")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_default()
        };

        #[cfg(target_os = "android")]
        let native_ble_disconnect = port.starts_with("ble://");

        let rns_handle = state_arc
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));

        if let Some(ref handle) = rns_handle {
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
                for iface in stats {
                    if iface.name == name {
                        teardown_rnode_interface_for_port(handle, iface.id, &port).await;
                        break;
                    }
                }
            }
        }

        #[cfg(target_os = "android")]
        if native_ble_disconnect {
            state_arc.emit_to_all("ble_rnode_disconnect_native", json!({}));
        }

        if with_rns_config_lock(&state_arc, || {
            crate::rns_config::remove_interface(&config_dir, &name)
        }) {
            emit_op_status_broadcast(
                &state_arc,
                "remove_lora",
                "hub",
                "Connection removed.",
                true,
                None,
            );
            if state_arc
                .network_log_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                state_arc.emit_network_event(
                    "interface",
                    &format!("LoRa interface removed: {}", name),
                    &name,
                    "standard",
                );
            }
            let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
            emit_hub_interfaces(&state_arc, ifaces);
        } else {
            emit_op_status_broadcast(
                &state_arc,
                "remove_lora",
                "hub",
                "Failed",
                true,
                Some("Config write error"),
            );
            if state_arc
                .network_log_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                state_arc.emit_network_event(
                    "error",
                    &format!("Failed to remove LoRa interface: {}", name),
                    &name,
                    "essential",
                );
            }
        }
    });
    Ok(json!({ "queued": true }))
}

#[tauri::command]
pub async fn enable_auto_interface(
    state: State<'_, Arc<AppState>>,
    #[allow(non_snake_case)] name: Option<String>,
    options: Option<crate::rns_config::AutoInterfaceOptions>,
) -> AppResult<Value> {
    use std::str::FromStr;

    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(name.as_deref().unwrap_or("Local Network"), 64);
    let config_dir = active_rns_config_dir(&state_arc);
    let opts = options.unwrap_or_default();

    // Validate before writing config to avoid half-written entries.
    if let Some(scope) = opts.discovery_scope.as_deref() {
        rns_interface::auto::DiscoveryScope::from_str(scope)
            .map_err(|e| AppError::bad_request(format!("Invalid discovery_scope: {e}")))?;
    }
    if let Some(t) = opts.multicast_address_type.as_deref() {
        rns_interface::auto::McastAddrType::from_str(t)
            .map_err(|e| AppError::bad_request(format!("Invalid multicast_address_type: {e}")))?;
    }
    if let Some(g) = opts.group_id.as_deref() {
        if g.is_empty() || g.len() > 63 {
            return Err(AppError::bad_request("group_id must be 1-63 characters"));
        }
        if !g
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(AppError::bad_request(
                "group_id may contain only [A-Za-z0-9_-]",
            ));
        }
    }
    if matches!(opts.discovery_port, Some(0)) || matches!(opts.data_port, Some(0)) {
        return Err(AppError::bad_request(
            "discovery_port and data_port must be 1-65535",
        ));
    }
    if let (Some(d), Some(p)) = (opts.discovery_port, opts.data_port)
        && d == p
    {
        return Err(AppError::bad_request(
            "discovery_port and data_port must differ",
        ));
    }

    if !with_rns_config_lock(&state_arc, || {
        crate::rns_config::add_auto_interface(&config_dir, &name, &opts)
    }) {
        emit_op_status_broadcast(
            &state_arc,
            "enable_auto",
            "hub",
            "Failed",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let ifaces_now = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state_arc, ifaces_now);

    let group_id = opts
        .group_id
        .clone()
        .unwrap_or_else(|| rns_interface::auto::DEFAULT_GROUP_ID.to_string());
    let discovery_scope = opts
        .discovery_scope
        .as_deref()
        .map(|s| rns_interface::auto::DiscoveryScope::from_str(s).unwrap())
        .unwrap_or(rns_interface::auto::DiscoveryScope::Link);
    let multicast_address_type = opts
        .multicast_address_type
        .as_deref()
        .map(|s| rns_interface::auto::McastAddrType::from_str(s).unwrap())
        .unwrap_or(rns_interface::auto::McastAddrType::Temporary);
    let discovery_port = opts
        .discovery_port
        .unwrap_or(rns_interface::auto::DISCOVERY_PORT);
    let data_port = opts.data_port.unwrap_or(rns_interface::auto::DATA_PORT);
    let runtime_config = rns_interface::auto::AutoInterfaceConfig {
        name: name.clone(),
        group_id,
        discovery_scope,
        discovery_port,
        data_port,
        multicast_address_type,
        devices: opts.devices.clone(),
        ignored_devices: opts.ignored_devices.clone().unwrap_or_default(),
        configured_bitrate: opts.configured_bitrate,
        ..rns_interface::auto::AutoInterfaceConfig::default()
    };

    let st = Arc::clone(&state_arc);
    let iface_name = name.clone();
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let rns_handle = st
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));
        if let Some(handle) = rns_handle {
            teardown_live_interface_by_name(&st, &iface_name, None).await;
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                rns_runtime::reticulum::spawn_auto_interface_runtime_with_config(
                    &handle,
                    runtime_config,
                ),
            )
            .await
            {
                Ok(Ok(_id)) => {
                    emit_op_status_broadcast(
                        &st,
                        "enable_auto",
                        "hub",
                        "Local Network enabled",
                        true,
                        None,
                    );
                    if st
                        .network_log_enabled
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        st.emit_network_event(
                            "interface",
                            "Local Network interface enabled",
                            &iface_name,
                            "standard",
                        );
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(interface = %iface_name, error = %e, "AutoInterface spawn failed");
                    // Roll back config write on spawn failure.
                    let _ = with_rns_config_lock(&st, || {
                        crate::rns_config::remove_interface(&config_dir, &iface_name)
                    });
                    emit_op_status_broadcast(
                        &st,
                        "enable_auto",
                        "hub",
                        "Spawn failed",
                        true,
                        Some(&e),
                    );
                }
                Err(_) => {
                    tracing::warn!(interface = %iface_name, "AutoInterface spawn timed out");
                    let _ = with_rns_config_lock(&st, || {
                        crate::rns_config::remove_interface(&config_dir, &iface_name)
                    });
                    emit_op_status_broadcast(
                        &st,
                        "enable_auto",
                        "hub",
                        "Spawn timed out",
                        true,
                        Some("Local Network spawn timed out; check network permissions"),
                    );
                }
            }
        } else {
            emit_op_status_broadcast(
                &st,
                "enable_auto",
                "hub",
                "Config saved (RNS not running)",
                true,
                None,
            );
        }
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });
    Ok(json!({ "queued": true }))
}

#[tauri::command]
pub async fn disable_auto_interface(
    state: State<'_, Arc<AppState>>,
    name: Option<String>,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let config_dir = active_rns_config_dir(&state_arc);
    let names = name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| vec![sanitize_text(s, 64)])
        .unwrap_or_else(|| crate::rns_config::auto_interface_names(&config_dir));

    if !names.is_empty()
        && !with_rns_config_lock(&state_arc, || {
            crate::rns_config::remove_interfaces(&config_dir, &names)
        })
    {
        emit_op_status_broadcast(
            &state_arc,
            "disable_auto",
            "hub",
            "Failed",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let ifaces_now = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state_arc, ifaces_now);

    let st = Arc::clone(&state_arc);
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        if let Some(handle) = st
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()))
        {
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
                for iface in stats {
                    if names.iter().any(|name| name == &iface.name) {
                        rns_runtime::reticulum::teardown_interface(&handle, iface.id).await;
                    }
                }
            }
        }
        emit_op_status_broadcast(
            &st,
            "disable_auto",
            "hub",
            "Local Network disabled",
            true,
            None,
        );
        if st
            .network_log_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            st.emit_network_event(
                "interface",
                "Local Network interface disabled",
                &names.join(", "),
                "standard",
            );
        }
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });
    Ok(json!({ "queued": true }))
}

/// Relay `AutoInterfaceEvent`s as `auto_unavailable` / `auto_carrier_state`.
/// Call once at startup.
pub fn spawn_auto_event_broadcaster(state: &Arc<AppState>) {
    let state_auto = Arc::clone(state);
    tokio::spawn(async move {
        let mut rx = rns_interface::auto::subscribe_auto_events();
        loop {
            match rx.recv().await {
                Ok(rns_interface::auto::AutoInterfaceEvent::JoinFailed {
                    interface_name,
                    ifname,
                    reason,
                }) => {
                    state_auto.emit_to_all(
                        "auto_unavailable",
                        json!({
                            "interface": interface_name,
                            "nic": ifname,
                            "reason": reason,
                            "platform": std::env::consts::OS,
                        }),
                    );
                }
                Ok(rns_interface::auto::AutoInterfaceEvent::CarrierState {
                    interface_name,
                    ifname,
                    ok,
                    reason,
                }) => {
                    state_auto.emit_to_all(
                        "auto_carrier_state",
                        json!({
                            "interface": interface_name,
                            "nic": ifname,
                            "ok": ok,
                            "reason": reason,
                            "platform": std::env::consts::OS,
                        }),
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Returns `[{name, addr_v4, addr_v6_link_local, is_up, is_loopback}]`.
#[tauri::command]
pub async fn api_list_network_interfaces() -> AppResult<Value> {
    let interfaces = rns_interface::auto::list_network_interfaces().map_err(AppError::internal)?;
    Ok(json!({ "interfaces": interfaces }))
}

#[derive(Deserialize)]
pub struct TcpConnectionArgs {
    pub host: String,
    pub port: i64,
    #[serde(default = "default_tcp_name")]
    pub name: String,
    #[serde(flatten)]
    ifac: InterfaceIfacCommandFields,
}

fn default_tcp_name() -> String {
    "TCP".to_string()
}

#[tauri::command]
pub async fn add_tcp_connection(
    state: State<'_, Arc<AppState>>,
    args: TcpConnectionArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let host = sanitize_text(&args.host, 256);
    let port = args.port;
    let name = sanitize_text(&args.name, 64);
    let ifac = ifac_settings_from_args(&args.ifac, None);

    if host.is_empty() || !(1..=65535).contains(&port) {
        emit_op_status_broadcast(
            &state_arc,
            "add_tcp",
            "hub",
            "Invalid parameters",
            true,
            Some("Host and port required"),
        );
        return Err(AppError::bad_request("Host and port required"));
    }

    let iface_name = if name.is_empty() || name == default_tcp_name() {
        format!("{}:{}", host, port)
    } else {
        name.clone()
    };

    let config_dir = active_rns_config_dir(&state_arc);
    let candidate_public_server = public_tcp_server_id(&host, port as u16);
    if !with_rns_config_lock(&state_arc, || {
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        enforce_public_tcp_transport_connect_limit(
            &state_arc,
            &ifaces,
            Some(&iface_name),
            candidate_public_server,
        )?;
        if crate::rns_config::add_tcp_client_with_ifac(
            &config_dir,
            &iface_name,
            &host,
            port as u16,
            ifac.config_args(),
        ) {
            Ok::<_, AppError>(true)
        } else {
            Ok(false)
        }
    })? {
        emit_op_status_broadcast(
            &state_arc,
            "add_tcp",
            "hub",
            "Failed to save config",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let host_for_db = host.clone();
    let name_for_db = name.clone();
    let _ = db::spawn_db(state_arc.db.clone(), move |p| {
        db::save_connection_history(&p, &host_for_db, port, &name_for_db);
    })
    .await;

    let ifaces_now = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state_arc, ifaces_now);

    let st = Arc::clone(&state_arc);
    let host_clone = host.clone();
    let iface_name_clone = iface_name.clone();
    let ifac_clone = ifac.clone();
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let rns_handle = st
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));
        if let Some(handle) = rns_handle {
            teardown_live_interface_by_name(&st, &iface_name_clone, None).await;
            match rns_runtime::reticulum::spawn_tcp_client_runtime_with_ifac(
                &handle,
                &iface_name_clone,
                &host_clone,
                port as u16,
                ifac_clone.runtime_config(),
            )
            .await
            {
                Ok(_id) => {
                    emit_op_status_broadcast(&st, "add_tcp", "hub", "Connected", true, None);
                    if st
                        .network_log_enabled
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        st.emit_network_event(
                            "interface",
                            &format!("TCP connected to {}:{}", host_clone, port),
                            &iface_name_clone,
                            "standard",
                        );
                    }
                }
                Err(e) => {
                    emit_op_status_broadcast(
                        &st,
                        "add_tcp",
                        "hub",
                        "Config saved, connect failed",
                        true,
                        Some(&e),
                    );
                    if st
                        .network_log_enabled
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        st.emit_network_event(
                            "error",
                            &format!("TCP connect failed: {}:{}", host_clone, port),
                            &e,
                            "essential",
                        );
                    }
                }
            }
        } else {
            emit_op_status_broadcast(
                &st,
                "add_tcp",
                "hub",
                "Config saved (RNS not running)",
                true,
                None,
            );
        }
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });
    Ok(json!({ "queued": true, "iface_name": iface_name }))
}

#[derive(Deserialize)]
pub struct UpdateTcpConnectionArgs {
    pub old_name: String,
    pub host: String,
    pub port: i64,
    #[serde(default = "default_tcp_name")]
    pub name: String,
    #[serde(flatten)]
    ifac: InterfaceIfacCommandFields,
}

#[tauri::command]
pub async fn update_tcp_connection(
    state: State<'_, Arc<AppState>>,
    args: UpdateTcpConnectionArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let old_name = sanitize_text(&args.old_name, 64);
    let host = sanitize_text(&args.host, 256);
    let port = args.port;
    let raw_name = sanitize_text(&args.name, 64);
    if old_name.is_empty() || host.is_empty() || !(1..=65535).contains(&port) {
        emit_op_status_broadcast(
            &state_arc,
            "update_tcp",
            "hub",
            "Invalid parameters",
            true,
            Some("Host and port required"),
        );
        return Err(AppError::bad_request("Host and port required"));
    }
    let name = if raw_name.is_empty() || raw_name == default_tcp_name() {
        format!("{}:{}", host, port)
    } else {
        raw_name
    };

    let config_dir = active_rns_config_dir(&state_arc);

    let candidate_public_server = public_tcp_server_id(&host, port as u16);
    let (old_runtime, old_config_content, config_written, ifac) =
        with_rns_config_lock(&state_arc, || {
            let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
            let old_entry = find_config_interface(&config_dir, "tcp_client", &old_name)
                .ok_or_else(|| AppError::bad_request("Interface not found"))?;
            let old_runtime = tcp_client_config_from_entry(&old_entry)
                .ok_or_else(|| AppError::bad_request("Invalid TCP config"))?;
            let old_ifac = ifac_settings_from_entry(&old_entry);
            let ifac = ifac_settings_from_args(&args.ifac, Some(&old_ifac));
            enforce_public_tcp_transport_connect_limit(
                &state_arc,
                &ifaces,
                Some(&old_name),
                candidate_public_server,
            )?;
            let old_config_content =
                crate::rns_config::read_config(&config_dir).unwrap_or_default();
            let config_written = crate::rns_config::update_tcp_client_with_ifac(
                &config_dir,
                &old_name,
                &name,
                &host,
                port as u16,
                ifac.config_args(),
            );
            Ok::<_, AppError>((old_runtime, old_config_content, config_written, ifac))
        })?;

    if !config_written {
        emit_op_status_broadcast(
            &state_arc,
            "update_tcp",
            "hub",
            "Failed to write config",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let host_for_db = host.clone();
    let name_for_db = name.clone();
    let _ = db::spawn_db(state_arc.db.clone(), move |p| {
        db::save_connection_history(&p, &host_for_db, port, &name_for_db);
    })
    .await;

    let new_runtime = EditableInterfaceConfig::TcpClient {
        name: name.clone(),
        host,
        port: port as u16,
        ifac,
    };
    emit_hub_interfaces(
        &state_arc,
        crate::rns_config::get_all_interfaces(&config_dir),
    );
    tokio::spawn(finish_interface_replace(
        Arc::clone(&state_arc),
        config_dir.clone(),
        "update_tcp",
        old_config_content,
        old_runtime,
        new_runtime,
    ));
    Ok(json!({ "queued": true, "iface_name": name }))
}

#[tauri::command]
pub async fn remove_tcp_connection(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(&name, 64);
    let config_dir = active_rns_config_dir(&state_arc);

    if !with_rns_config_lock(&state_arc, || {
        crate::rns_config::remove_interface(&config_dir, &name)
    }) {
        emit_op_status_broadcast(
            &state_arc,
            "remove_tcp",
            "hub",
            "Failed",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let ifaces_now = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state_arc, ifaces_now);

    let st = Arc::clone(&state_arc);
    let name2 = name.clone();
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let rns_handle = st
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
                && let Ok(rns_transport::messages::TransportQueryResponse::InterfaceStats(stats)) =
                    resp_rx.await
            {
                for iface in stats {
                    if iface.name == name2 {
                        rns_runtime::reticulum::teardown_interface(&handle, iface.id).await;
                        break;
                    }
                }
            }
        }
        emit_op_status_broadcast(&st, "remove_tcp", "hub", "Connection removed.", true, None);
        if st
            .network_log_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            st.emit_network_event(
                "interface",
                &format!("TCP interface removed: {}", name2),
                &name2,
                "standard",
            );
        }
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });
    Ok(json!({ "queued": true }))
}

#[derive(Deserialize)]
pub struct TcpServerArgs {
    #[serde(default = "default_tcp_server_name")]
    pub name: String,
    #[serde(default = "default_tcp_server_port")]
    pub listen_port: u16,
    #[serde(default = "default_tcp_server_ip")]
    pub listen_ip: String,
}

fn default_tcp_server_name() -> String {
    "TCP Server".to_string()
}
fn default_tcp_server_port() -> u16 {
    4242
}
fn default_tcp_server_ip() -> String {
    "0.0.0.0".to_string()
}

#[tauri::command]
pub async fn add_tcp_server(
    state: State<'_, Arc<AppState>>,
    args: TcpServerArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(&args.name, 64);
    let listen_ip = sanitize_text(&args.listen_ip, 64);
    let listen_port = args.listen_port;

    let config_dir = active_rns_config_dir(&state_arc);
    if !with_rns_config_lock(&state_arc, || {
        crate::rns_config::add_tcp_server(&config_dir, &name, listen_port, &listen_ip)
    }) {
        emit_op_status_broadcast(
            &state_arc,
            "add_server",
            "hub",
            "Failed",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let ifaces_now = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state_arc, ifaces_now);

    let st = Arc::clone(&state_arc);
    let name_clone = name.clone();
    let listen_ip_clone = listen_ip.clone();
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let rns_handle = st
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));
        if let Some(handle) = rns_handle {
            teardown_live_interface_by_name(&st, &name_clone, None).await;
            match rns_runtime::reticulum::spawn_tcp_server_runtime(
                &handle,
                &name_clone,
                &listen_ip_clone,
                listen_port,
            )
            .await
            {
                Ok(_id) => {
                    emit_op_status_broadcast(&st, "add_server", "hub", "Started", true, None);
                    if st
                        .network_log_enabled
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        st.emit_network_event(
                            "interface",
                            &format!(
                                "TCP server listening on {}:{}",
                                listen_ip_clone, listen_port
                            ),
                            &name_clone,
                            "standard",
                        );
                    }
                }
                Err(e) => {
                    let _ = with_rns_config_lock(&st, || {
                        crate::rns_config::remove_interface(&config_dir, &name_clone)
                    });
                    emit_op_status_broadcast(
                        &st,
                        "add_server",
                        "hub",
                        "Failed to start",
                        true,
                        Some(&e),
                    );
                    if st
                        .network_log_enabled
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        st.emit_network_event(
                            "error",
                            &format!("TCP server failed on {}:{}", listen_ip_clone, listen_port),
                            &e,
                            "essential",
                        );
                    }
                }
            }
        } else {
            emit_op_status_broadcast(
                &st,
                "add_server",
                "hub",
                "Config saved (RNS not running)",
                true,
                None,
            );
        }
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });
    Ok(json!({ "queued": true, "iface_name": name }))
}

#[derive(Deserialize)]
pub struct UpdateTcpServerArgs {
    pub old_name: String,
    #[serde(default = "default_tcp_server_name")]
    pub name: String,
    #[serde(default = "default_tcp_server_port")]
    pub listen_port: u16,
    #[serde(default = "default_tcp_server_ip")]
    pub listen_ip: String,
}

#[tauri::command]
pub async fn update_tcp_server(
    state: State<'_, Arc<AppState>>,
    args: UpdateTcpServerArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let old_name = sanitize_text(&args.old_name, 64);
    let name = sanitize_text(&args.name, 64);
    let listen_ip = sanitize_text(&args.listen_ip, 64);
    if old_name.is_empty() || name.is_empty() {
        emit_op_status_broadcast(
            &state_arc,
            "update_server",
            "hub",
            "Invalid parameters",
            true,
            Some("Name required"),
        );
        return Err(AppError::bad_request("Name required"));
    }

    let config_dir = active_rns_config_dir(&state_arc);
    let (old_runtime, old_config_content, config_written) =
        with_rns_config_lock(&state_arc, || {
            let old_entry = find_config_interface(&config_dir, "tcp_server", &old_name)
                .ok_or_else(|| AppError::bad_request("Interface not found"))?;
            let old_runtime = tcp_server_config_from_entry(&old_entry)
                .ok_or_else(|| AppError::bad_request("Invalid TCP server config"))?;
            let old_config_content =
                crate::rns_config::read_config(&config_dir).unwrap_or_default();
            let config_written = crate::rns_config::update_tcp_server(
                &config_dir,
                &old_name,
                &name,
                args.listen_port,
                &listen_ip,
            );
            Ok::<_, AppError>((old_runtime, old_config_content, config_written))
        })?;

    if !config_written {
        emit_op_status_broadcast(
            &state_arc,
            "update_server",
            "hub",
            "Failed to write config",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let new_runtime = EditableInterfaceConfig::TcpServer {
        name: name.clone(),
        listen_ip,
        listen_port: args.listen_port,
    };
    emit_hub_interfaces(
        &state_arc,
        crate::rns_config::get_all_interfaces(&config_dir),
    );
    tokio::spawn(finish_interface_replace(
        Arc::clone(&state_arc),
        config_dir.clone(),
        "update_server",
        old_config_content,
        old_runtime,
        new_runtime,
    ));
    Ok(json!({ "queued": true, "iface_name": name }))
}

#[tauri::command]
pub async fn remove_tcp_server(state: State<'_, Arc<AppState>>, name: String) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(&name, 64);
    let config_dir = active_rns_config_dir(&state_arc);

    if !with_rns_config_lock(&state_arc, || {
        crate::rns_config::remove_interface(&config_dir, &name)
    }) {
        emit_op_status_broadcast(
            &state_arc,
            "remove_server",
            "hub",
            "Failed",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let ifaces_now = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state_arc, ifaces_now);

    let st = Arc::clone(&state_arc);
    let name2 = name.clone();
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let rns_handle = st
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
                && let Ok(rns_transport::messages::TransportQueryResponse::InterfaceStats(stats)) =
                    resp_rx.await
            {
                for iface in stats {
                    if iface.name == name2 {
                        rns_runtime::reticulum::teardown_interface(&handle, iface.id).await;
                        break;
                    }
                }
            }
        }
        emit_op_status_broadcast(
            &st,
            "remove_server",
            "hub",
            "Connection removed.",
            true,
            None,
        );
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });
    Ok(json!({ "queued": true }))
}

// Backbone (HDLC-over-TCP). `target_host` presence selects client vs server.

fn default_backbone_client_name() -> String {
    "Backbone".to_string()
}
fn default_backbone_server_name() -> String {
    "Backbone Server".to_string()
}
fn default_backbone_listen_ip() -> String {
    "0.0.0.0".to_string()
}
fn default_backbone_server_port() -> u16 {
    4242
}

#[derive(Deserialize)]
pub struct BackboneConnectionArgs {
    pub host: String,
    pub port: i64,
    #[serde(default = "default_backbone_client_name")]
    pub name: String,
    #[serde(default)]
    pub prefer_ipv6: bool,
    #[serde(default)]
    pub connect_timeout: Option<u64>,
    #[serde(default)]
    pub max_reconnect_tries: Option<usize>,
    #[serde(default)]
    pub i2p_tunneled: bool,
    #[serde(flatten)]
    ifac: InterfaceIfacCommandFields,
}

#[tauri::command]
pub async fn add_backbone_connection(
    state: State<'_, Arc<AppState>>,
    args: BackboneConnectionArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let host = sanitize_text(&args.host, 256);
    let port = args.port;
    let raw_name = sanitize_text(&args.name, 64);
    let ifac = ifac_settings_from_args(&args.ifac, None);

    if host.is_empty() || !(1..=65535).contains(&port) {
        emit_op_status_broadcast(
            &state_arc,
            "add_backbone",
            "hub",
            "Invalid parameters",
            true,
            Some("Host and port required"),
        );
        return Err(AppError::bad_request("Host and port required"));
    }

    let iface_name = if raw_name.is_empty() || raw_name == default_backbone_client_name() {
        format!("Backbone to {}:{}", host, port)
    } else {
        raw_name
    };

    let config_dir = active_rns_config_dir(&state_arc);
    if !with_rns_config_lock(&state_arc, || {
        crate::rns_config::add_backbone_client(
            &config_dir,
            crate::rns_config::BackboneClientArgs {
                name: &iface_name,
                host: &host,
                port: port as u16,
                prefer_ipv6: args.prefer_ipv6,
                connect_timeout: args.connect_timeout,
                max_reconnect_tries: args.max_reconnect_tries,
                i2p_tunneled: args.i2p_tunneled,
                ifac: ifac.config_args(),
            },
        )
    }) {
        emit_op_status_broadcast(
            &state_arc,
            "add_backbone",
            "hub",
            "Failed to save config",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let ifaces_now = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state_arc, ifaces_now);

    let st = Arc::clone(&state_arc);
    let host_clone = host.clone();
    let iface_name_clone = iface_name.clone();
    let prefer_ipv6 = args.prefer_ipv6;
    let connect_timeout = args.connect_timeout;
    let max_reconnect_tries = args.max_reconnect_tries;
    let ifac_clone = ifac.clone();
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let rns_handle = st
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));
        if let Some(handle) = rns_handle {
            teardown_live_interface_by_name(&st, &iface_name_clone, None).await;
            match rns_runtime::reticulum::spawn_backbone_client_runtime_with_ifac(
                &handle,
                rns_runtime::reticulum::RuntimeBackboneClientConfig {
                    name: &iface_name_clone,
                    host: &host_clone,
                    port: port as u16,
                    prefer_ipv6,
                    connect_timeout,
                    max_reconnect_tries,
                    ifac: ifac_clone.runtime_config(),
                },
            )
            .await
            {
                Ok(_id) => {
                    emit_op_status_broadcast(&st, "add_backbone", "hub", "Connected", true, None);
                    if st
                        .network_log_enabled
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        st.emit_network_event(
                            "interface",
                            &format!("Backbone connected to {}:{}", host_clone, port),
                            &iface_name_clone,
                            "standard",
                        );
                    }
                }
                Err(e) => {
                    emit_op_status_broadcast(
                        &st,
                        "add_backbone",
                        "hub",
                        "Config saved, connect failed",
                        true,
                        Some(&e),
                    );
                    if st
                        .network_log_enabled
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        st.emit_network_event(
                            "error",
                            &format!("Backbone connect failed: {}:{}", host_clone, port),
                            &e,
                            "essential",
                        );
                    }
                }
            }
        } else {
            emit_op_status_broadcast(
                &st,
                "add_backbone",
                "hub",
                "Config saved (RNS not running)",
                true,
                None,
            );
        }
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });
    Ok(json!({ "queued": true, "iface_name": iface_name }))
}

#[derive(Deserialize)]
pub struct UpdateBackboneConnectionArgs {
    pub old_name: String,
    pub host: String,
    pub port: i64,
    #[serde(default = "default_backbone_client_name")]
    pub name: String,
    #[serde(default)]
    pub prefer_ipv6: bool,
    #[serde(default)]
    pub connect_timeout: Option<u64>,
    #[serde(default)]
    pub max_reconnect_tries: Option<usize>,
    #[serde(default)]
    pub i2p_tunneled: bool,
    #[serde(flatten)]
    ifac: InterfaceIfacCommandFields,
}

#[tauri::command]
pub async fn update_backbone_connection(
    state: State<'_, Arc<AppState>>,
    args: UpdateBackboneConnectionArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let old_name = sanitize_text(&args.old_name, 64);
    let host = sanitize_text(&args.host, 256);
    let port = args.port;
    let raw_name = sanitize_text(&args.name, 64);
    if old_name.is_empty() || host.is_empty() || !(1..=65535).contains(&port) {
        emit_op_status_broadcast(
            &state_arc,
            "update_backbone",
            "hub",
            "Invalid parameters",
            true,
            Some("Host and port required"),
        );
        return Err(AppError::bad_request("Host and port required"));
    }
    let name = if raw_name.is_empty() || raw_name == default_backbone_client_name() {
        format!("Backbone to {}:{}", host, port)
    } else {
        raw_name
    };

    let config_dir = active_rns_config_dir(&state_arc);
    let (old_runtime, old_config_content, config_written, ifac) =
        with_rns_config_lock(&state_arc, || {
            let old_entry = find_config_interface(&config_dir, "backbone_client", &old_name)
                .ok_or_else(|| AppError::bad_request("Interface not found"))?;
            let old_runtime = backbone_client_config_from_entry(&old_entry)
                .ok_or_else(|| AppError::bad_request("Invalid Backbone config"))?;
            let old_ifac = ifac_settings_from_entry(&old_entry);
            let ifac = ifac_settings_from_args(&args.ifac, Some(&old_ifac));
            let old_config_content =
                crate::rns_config::read_config(&config_dir).unwrap_or_default();
            let config_written = crate::rns_config::update_backbone_client(
                &config_dir,
                &old_name,
                crate::rns_config::BackboneClientArgs {
                    name: &name,
                    host: &host,
                    port: port as u16,
                    prefer_ipv6: args.prefer_ipv6,
                    connect_timeout: args.connect_timeout,
                    max_reconnect_tries: args.max_reconnect_tries,
                    i2p_tunneled: args.i2p_tunneled,
                    ifac: ifac.config_args(),
                },
            );
            Ok::<_, AppError>((old_runtime, old_config_content, config_written, ifac))
        })?;

    if !config_written {
        emit_op_status_broadcast(
            &state_arc,
            "update_backbone",
            "hub",
            "Failed to write config",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let new_runtime = EditableInterfaceConfig::BackboneClient {
        name: name.clone(),
        host,
        port: port as u16,
        prefer_ipv6: args.prefer_ipv6,
        connect_timeout: args.connect_timeout,
        max_reconnect_tries: args.max_reconnect_tries,
        i2p_tunneled: args.i2p_tunneled,
        ifac,
    };
    emit_hub_interfaces(
        &state_arc,
        crate::rns_config::get_all_interfaces(&config_dir),
    );
    tokio::spawn(finish_interface_replace(
        Arc::clone(&state_arc),
        config_dir.clone(),
        "update_backbone",
        old_config_content,
        old_runtime,
        new_runtime,
    ));
    Ok(json!({ "queued": true, "iface_name": name }))
}

#[tauri::command]
pub async fn remove_backbone_connection(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(&name, 64);
    let config_dir = active_rns_config_dir(&state_arc);

    if !with_rns_config_lock(&state_arc, || {
        crate::rns_config::remove_interface(&config_dir, &name)
    }) {
        emit_op_status_broadcast(
            &state_arc,
            "remove_backbone",
            "hub",
            "Failed",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let ifaces_now = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state_arc, ifaces_now);

    let st = Arc::clone(&state_arc);
    let name2 = name.clone();
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let rns_handle = st
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
                && let Ok(rns_transport::messages::TransportQueryResponse::InterfaceStats(stats)) =
                    resp_rx.await
            {
                for iface in stats {
                    if iface.name == name2 {
                        rns_runtime::reticulum::teardown_interface(&handle, iface.id).await;
                        break;
                    }
                }
            }
        }
        emit_op_status_broadcast(
            &st,
            "remove_backbone",
            "hub",
            "Connection removed.",
            true,
            None,
        );
        if st
            .network_log_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            st.emit_network_event(
                "interface",
                &format!("Backbone interface removed: {}", name2),
                &name2,
                "standard",
            );
        }
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });
    Ok(json!({ "queued": true }))
}

#[derive(Deserialize)]
pub struct BackboneServerArgs {
    #[serde(default = "default_backbone_server_name")]
    pub name: String,
    #[serde(default = "default_backbone_server_port")]
    pub listen_port: u16,
    #[serde(default = "default_backbone_listen_ip")]
    pub listen_ip: String,
    #[serde(default)]
    pub prefer_ipv6: bool,
    #[serde(default)]
    pub device: Option<String>,
}

#[tauri::command]
pub async fn add_backbone_server(
    state: State<'_, Arc<AppState>>,
    args: BackboneServerArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(&args.name, 64);
    let listen_ip = sanitize_text(&args.listen_ip, 64);
    let listen_port = args.listen_port;
    let device = args
        .device
        .as_deref()
        .map(|s| sanitize_text(s, 64))
        .filter(|s| !s.is_empty());

    let config_dir = active_rns_config_dir(&state_arc);
    if !with_rns_config_lock(&state_arc, || {
        crate::rns_config::add_backbone_server(
            &config_dir,
            &name,
            listen_port,
            &listen_ip,
            args.prefer_ipv6,
            device.as_deref(),
        )
    }) {
        emit_op_status_broadcast(
            &state_arc,
            "add_backbone_server",
            "hub",
            "Failed",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let ifaces_now = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state_arc, ifaces_now);

    let st = Arc::clone(&state_arc);
    let name_clone = name.clone();
    let listen_ip_clone = listen_ip.clone();
    let device_clone = device.clone();
    let prefer_ipv6 = args.prefer_ipv6;
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let rns_handle = st
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()));
        if let Some(handle) = rns_handle {
            teardown_live_interface_by_name(&st, &name_clone, None).await;
            match rns_runtime::reticulum::spawn_backbone_server_runtime(
                &handle,
                &name_clone,
                &listen_ip_clone,
                listen_port,
                prefer_ipv6,
                device_clone.as_deref(),
            )
            .await
            {
                Ok(_id) => {
                    emit_op_status_broadcast(
                        &st,
                        "add_backbone_server",
                        "hub",
                        "Started",
                        true,
                        None,
                    );
                    if st
                        .network_log_enabled
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        st.emit_network_event(
                            "interface",
                            &format!(
                                "Backbone server listening on {}:{}",
                                listen_ip_clone, listen_port
                            ),
                            &name_clone,
                            "standard",
                        );
                    }
                }
                Err(e) => {
                    let _ = with_rns_config_lock(&st, || {
                        crate::rns_config::remove_interface(&config_dir, &name_clone)
                    });
                    emit_op_status_broadcast(
                        &st,
                        "add_backbone_server",
                        "hub",
                        "Failed to start",
                        true,
                        Some(&e),
                    );
                    if st
                        .network_log_enabled
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        st.emit_network_event(
                            "error",
                            &format!(
                                "Backbone server failed on {}:{}",
                                listen_ip_clone, listen_port
                            ),
                            &e,
                            "essential",
                        );
                    }
                }
            }
        } else {
            emit_op_status_broadcast(
                &st,
                "add_backbone_server",
                "hub",
                "Config saved (RNS not running)",
                true,
                None,
            );
        }
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });
    Ok(json!({ "queued": true, "iface_name": name }))
}

#[derive(Deserialize)]
pub struct UpdateBackboneServerArgs {
    pub old_name: String,
    #[serde(default = "default_backbone_server_name")]
    pub name: String,
    #[serde(default = "default_backbone_server_port")]
    pub listen_port: u16,
    #[serde(default = "default_backbone_listen_ip")]
    pub listen_ip: String,
    #[serde(default)]
    pub prefer_ipv6: bool,
    pub device: Option<String>,
}

#[tauri::command]
pub async fn update_backbone_server(
    state: State<'_, Arc<AppState>>,
    args: UpdateBackboneServerArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let old_name = sanitize_text(&args.old_name, 64);
    let name = sanitize_text(&args.name, 64);
    let listen_ip = sanitize_text(&args.listen_ip, 64);
    let device = args
        .device
        .as_deref()
        .map(|s| sanitize_text(s, 64))
        .filter(|s| !s.is_empty());
    if old_name.is_empty() || name.is_empty() {
        emit_op_status_broadcast(
            &state_arc,
            "update_backbone_server",
            "hub",
            "Invalid parameters",
            true,
            Some("Name required"),
        );
        return Err(AppError::bad_request("Name required"));
    }

    let config_dir = active_rns_config_dir(&state_arc);
    let (old_runtime, old_config_content, config_written) =
        with_rns_config_lock(&state_arc, || {
            let old_entry = find_config_interface(&config_dir, "backbone_server", &old_name)
                .ok_or_else(|| AppError::bad_request("Interface not found"))?;
            let old_runtime = backbone_server_config_from_entry(&old_entry)
                .ok_or_else(|| AppError::bad_request("Invalid Backbone server config"))?;
            let old_config_content =
                crate::rns_config::read_config(&config_dir).unwrap_or_default();
            let config_written = crate::rns_config::update_backbone_server(
                &config_dir,
                &old_name,
                &name,
                args.listen_port,
                &listen_ip,
                args.prefer_ipv6,
                device.as_deref(),
            );
            Ok::<_, AppError>((old_runtime, old_config_content, config_written))
        })?;

    if !config_written {
        emit_op_status_broadcast(
            &state_arc,
            "update_backbone_server",
            "hub",
            "Failed to write config",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let new_runtime = EditableInterfaceConfig::BackboneServer {
        name: name.clone(),
        listen_ip,
        listen_port: args.listen_port,
        prefer_ipv6: args.prefer_ipv6,
        device,
    };
    emit_hub_interfaces(
        &state_arc,
        crate::rns_config::get_all_interfaces(&config_dir),
    );
    tokio::spawn(finish_interface_replace(
        Arc::clone(&state_arc),
        config_dir.clone(),
        "update_backbone_server",
        old_config_content,
        old_runtime,
        new_runtime,
    ));
    Ok(json!({ "queued": true, "iface_name": name }))
}

#[tauri::command]
pub async fn remove_backbone_server(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let name = sanitize_text(&name, 64);
    let config_dir = active_rns_config_dir(&state_arc);

    if !with_rns_config_lock(&state_arc, || {
        crate::rns_config::remove_interface(&config_dir, &name)
    }) {
        emit_op_status_broadcast(
            &state_arc,
            "remove_backbone_server",
            "hub",
            "Failed",
            true,
            Some("Config write error"),
        );
        return Err(AppError::internal("Config write error"));
    }

    let ifaces_now = crate::rns_config::get_all_interfaces(&config_dir);
    emit_hub_interfaces(&state_arc, ifaces_now);

    let st = Arc::clone(&state_arc);
    let name2 = name.clone();
    let config_dir = config_dir.clone();
    tokio::spawn(async move {
        let rns_handle = st
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
                && let Ok(rns_transport::messages::TransportQueryResponse::InterfaceStats(stats)) =
                    resp_rx.await
            {
                for iface in stats {
                    if iface.name == name2 {
                        rns_runtime::reticulum::teardown_interface(&handle, iface.id).await;
                        break;
                    }
                }
            }
        }
        emit_op_status_broadcast(
            &st,
            "remove_backbone_server",
            "hub",
            "Connection removed.",
            true,
            None,
        );
        let ifaces = crate::rns_config::get_all_interfaces(&config_dir);
        emit_hub_interfaces(&st, ifaces);
    });
    Ok(json!({ "queued": true }))
}

#[cfg(test)]
mod backbone_args_tests {
    use super::*;

    #[test]
    fn backbone_connection_args_defaults() {
        let v = serde_json::from_value::<BackboneConnectionArgs>(serde_json::json!({
            "host": "h", "port": 1
        }))
        .unwrap();
        assert_eq!(v.name, "Backbone");
        assert!(!v.prefer_ipv6);
        assert!(v.connect_timeout.is_none());
        assert!(v.max_reconnect_tries.is_none());
        assert!(!v.i2p_tunneled);
    }

    #[test]
    fn backbone_server_args_defaults() {
        let v = serde_json::from_value::<BackboneServerArgs>(serde_json::json!({})).unwrap();
        assert_eq!(v.listen_ip, "0.0.0.0");
        assert_eq!(v.listen_port, 4242);
        assert_eq!(v.name, "Backbone Server");
        assert!(!v.prefer_ipv6);
        assert!(v.device.is_none());
    }

    #[test]
    fn auto_runtime_config_from_entry_preserves_saved_options() {
        let entry = serde_json::json!({
            "name": "Field LAN",
            "type": "AutoInterface",
            "enabled": "yes",
            "group_id": "field",
            "discovery_scope": "site",
            "discovery_port": "30000",
            "data_port": "30001",
            "multicast_address_type": "permanent",
            "devices": "wlan0, eth0",
            "ignored_devices": "utun0, awdl0",
            "configured_bitrate": "42000000"
        });

        assert!(cfg_bool_default_true(&entry, "enabled"));
        let cfg = auto_runtime_config_from_entry(&entry).expect("auto config");
        assert_eq!(cfg.name, "Field LAN");
        assert_eq!(cfg.group_id, "field");
        assert_eq!(
            cfg.discovery_scope,
            rns_interface::auto::DiscoveryScope::Site
        );
        assert_eq!(cfg.discovery_port, 30_000);
        assert_eq!(cfg.data_port, 30_001);
        assert_eq!(
            cfg.multicast_address_type,
            rns_interface::auto::McastAddrType::Permanent
        );
        assert_eq!(
            cfg.devices,
            Some(vec!["wlan0".to_string(), "eth0".to_string()])
        );
        assert_eq!(
            cfg.ignored_devices,
            vec!["utun0".to_string(), "awdl0".to_string()]
        );
        assert_eq!(cfg.configured_bitrate, Some(42_000_000));
    }

    #[test]
    fn auto_runtime_config_from_entry_uses_python_parity_defaults() {
        let entry = serde_json::json!({
            "name": "Default Interface",
            "type": "AutoInterface"
        });

        assert!(cfg_bool_default_true(&entry, "enabled"));
        let cfg = auto_runtime_config_from_entry(&entry).expect("auto config");
        assert_eq!(cfg.name, "Default Interface");
        assert_eq!(cfg.group_id, rns_interface::auto::DEFAULT_GROUP_ID);
        assert_eq!(
            cfg.discovery_scope,
            rns_interface::auto::DiscoveryScope::Link
        );
        assert_eq!(cfg.discovery_port, rns_interface::auto::DISCOVERY_PORT);
        assert_eq!(cfg.data_port, rns_interface::auto::DATA_PORT);
        assert_eq!(
            cfg.multicast_address_type,
            rns_interface::auto::McastAddrType::Temporary
        );
        assert!(cfg.devices.is_none());
        assert!(cfg.ignored_devices.is_empty());
        assert!(cfg.configured_bitrate.is_none());
    }

    #[test]
    fn transport_mode_default_is_off() {
        assert_eq!(default_mode(), "off");
    }

    #[test]
    fn auto_transport_requires_enabled_non_lora_without_enabled_lora() {
        let ifaces = serde_json::json!({
            "rnode": [
                { "name": "Disabled LoRa", "type": "RNodeInterface", "enabled": "false" }
            ],
            "auto": [
                { "name": "LAN", "type": "AutoInterface", "enabled": "true" }
            ],
            "tcp_client": [],
            "tcp_server": [],
            "backbone_client": [],
            "backbone_server": []
        });

        assert!(auto_transport_enabled_for_interfaces(&ifaces, "wifi"));
        assert!(!auto_transport_enabled_for_interfaces(&ifaces, "cellular"));

        let ifaces_with_lora = serde_json::json!({
            "rnode": [
                { "name": "LoRa", "type": "RNodeInterface", "enabled": "true" }
            ],
            "auto": [
                { "name": "LAN", "type": "AutoInterface", "enabled": "true" }
            ],
            "tcp_client": [],
            "tcp_server": [],
            "backbone_client": [],
            "backbone_server": []
        });

        assert!(!auto_transport_enabled_for_interfaces(
            &ifaces_with_lora,
            "wifi"
        ));

        let ifaces_without_non_lora = serde_json::json!({
            "rnode": [],
            "auto": [],
            "tcp_client": [],
            "tcp_server": [],
            "backbone_client": [],
            "backbone_server": []
        });

        assert!(!auto_transport_enabled_for_interfaces(
            &ifaces_without_non_lora,
            "wifi"
        ));
    }

    #[test]
    fn public_tcp_servers_are_canonicalised_for_transport_limits() {
        assert_eq!(
            public_tcp_server_id("RNS.RATSPEAK.ORG.", 4242),
            Some("ratspeak-emerald")
        );
        assert_eq!(
            public_tcp_server_id("https://2.ratspeak.org/", 4242),
            Some("ratspeak-emerald")
        );
        assert_eq!(public_tcp_server_id("example.net", 4242), None);

        let alias_pair = serde_json::json!({
            "tcp_client": [
                { "name": "Emerald 2", "target_host": "2.ratspeak.org", "target_port": "4242", "enabled": "true" },
                { "name": "Emerald RNS", "target_host": "rns.ratspeak.org", "target_port": "4242", "enabled": "true" }
            ]
        });
        assert_eq!(enabled_public_tcp_server_count(&alias_pair), 1);

        let multiple_public = serde_json::json!({
            "tcp_client": [
                { "name": "Ruby", "target_host": "1.ratspeak.org", "target_port": "4141", "enabled": "true" },
                { "name": "Emerald", "target_host": "2.ratspeak.org", "target_port": "4242", "enabled": "true" },
                { "name": "Paused Diamond", "target_host": "3.ratspeak.org", "target_port": "4343", "enabled": "false" }
            ]
        });
        assert_eq!(enabled_public_tcp_server_count(&multiple_public), 2);
        assert_eq!(
            projected_enabled_public_tcp_server_ids(
                &multiple_public,
                Some("Ruby"),
                Some("ratspeak-diamond")
            )
            .len(),
            2
        );
    }

    #[test]
    fn auto_transport_refuses_multiple_enabled_public_tcp_servers() {
        let ifaces = serde_json::json!({
            "rnode": [],
            "auto": [],
            "tcp_client": [
                { "name": "Ruby", "type": "TCPClientInterface", "target_host": "1.ratspeak.org", "target_port": "4141", "enabled": "true" },
                { "name": "Emerald", "type": "TCPClientInterface", "target_host": "2.ratspeak.org", "target_port": "4242", "enabled": "true" }
            ],
            "tcp_server": [],
            "backbone_client": [],
            "backbone_server": []
        });

        assert!(auto_transport_base_enabled_for_interfaces(&ifaces, "wifi"));
        assert!(!auto_transport_enabled_for_interfaces(&ifaces, "wifi"));
    }

    #[test]
    fn rnode_tcp_ports_normalise_to_config_urls() {
        assert_eq!(
            normalise_rnode_port("tcp://192.168.1.50").unwrap(),
            "tcp://192.168.1.50:7633"
        );
        assert_eq!(
            normalise_rnode_port("TCP://rnode.local:9000").unwrap(),
            "tcp://rnode.local:9000"
        );
        assert_eq!(
            normalise_rnode_port("tcp://[2001:db8::1]").unwrap(),
            "tcp://[2001:db8::1]:7633"
        );
        assert_eq!(
            normalise_rnode_port("tcp://2001:db8::1").unwrap(),
            "tcp://[2001:db8::1]:7633"
        );
    }

    #[test]
    fn rnode_tcp_ports_reject_invalid_endpoints() {
        assert!(normalise_rnode_port("tcp://").is_err());
        assert!(normalise_rnode_port("tcp://rnode.local:").is_err());
        assert!(normalise_rnode_port("tcp://rnode.local:notaport").is_err());
        assert!(normalise_rnode_port("tcp://bad host:7633").is_err());
        assert!(normalise_rnode_port("tcp://[2001:db8::1").is_err());
    }

    #[tokio::test]
    async fn rnode_preset_api_comes_from_core_catalog() {
        let value = api_rnode_presets().await.expect("catalog");
        assert_eq!(
            value.get("default_region").and_then(Value::as_str),
            Some(ratspeak_core::radio::DEFAULT_RNODE_REGION_KEY)
        );
        assert_eq!(
            value.get("default_preset").and_then(Value::as_str),
            Some(ratspeak_core::radio::DEFAULT_RNODE_PRESET_KEY)
        );
        assert_eq!(
            value
                .get("presets")
                .and_then(Value::as_array)
                .and_then(|presets| presets.first())
                .and_then(|preset| preset.get("key"))
                .and_then(Value::as_str),
            Some(ratspeak_core::radio::DEFAULT_RNODE_PRESET_KEY)
        );
        assert_eq!(
            value.get("frequency_min").and_then(Value::as_u64),
            Some(ratspeak_core::radio::RNODE_FREQUENCY_MIN_HZ)
        );
        assert!(
            value
                .get("regions")
                .and_then(Value::as_array)
                .is_some_and(|regions| regions
                    .iter()
                    .any(|region| region.get("key").and_then(Value::as_str) == Some("uhf_433")))
        );
    }

    #[test]
    fn keyed_lora_args_resolve_and_validate_server_side() {
        let radio = resolve_lora_radio_args(LoraRadioArgs {
            region_key: Some("europe"),
            preset_key: Some("long_moderate"),
            custom_params: false,
            frequency: 1,
            bandwidth: 1,
            spreading_factor: 5,
            coding_rate: 5,
            tx_power: 0,
            airtime_limit_short: None,
            airtime_limit_long: None,
        })
        .expect("keyed catalog params");

        assert_eq!(radio.frequency, 868_000_000);
        assert_eq!(radio.bandwidth, 125_000);
        assert_eq!(radio.spreading_factor, 11);
        assert_eq!(radio.coding_rate, 8);
        assert_eq!(radio.tx_power, 22);
        assert_eq!(radio.region_key, Some("europe"));
        assert_eq!(radio.preset_key, Some("long_moderate"));

        assert!(
            resolve_lora_radio_args(LoraRadioArgs {
                region_key: Some("invalid"),
                preset_key: Some("medium_fast"),
                custom_params: false,
                frequency: 1,
                bandwidth: 1,
                spreading_factor: 5,
                coding_rate: 5,
                tx_power: 0,
                airtime_limit_short: None,
                airtime_limit_long: None,
            })
            .is_err()
        );
        assert!(
            resolve_lora_radio_args(LoraRadioArgs {
                region_key: None,
                preset_key: None,
                custom_params: false,
                frequency: 0,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 5,
                tx_power: 17,
                airtime_limit_short: None,
                airtime_limit_long: None,
            })
            .is_err()
        );
        assert!(
            resolve_lora_radio_args(LoraRadioArgs {
                region_key: None,
                preset_key: None,
                custom_params: false,
                frequency: 915_000_000,
                bandwidth: 250_000,
                spreading_factor: 13,
                coding_rate: 5,
                tx_power: 17,
                airtime_limit_short: None,
                airtime_limit_long: None,
            })
            .is_err()
        );
    }

    #[test]
    fn custom_lora_args_preserve_numeric_radio_params() {
        let radio = resolve_lora_radio_args(LoraRadioArgs {
            region_key: Some("americas"),
            preset_key: Some("long_fast"),
            custom_params: true,
            frequency: 915_250_000,
            bandwidth: 250_000,
            spreading_factor: 11,
            coding_rate: 5,
            tx_power: 22,
            airtime_limit_short: None,
            airtime_limit_long: None,
        })
        .expect("custom frequency with catalog preset");

        assert_eq!(radio.frequency, 915_250_000);
        assert_eq!(radio.bandwidth, 250_000);
        assert_eq!(radio.spreading_factor, 11);
        assert_eq!(radio.coding_rate, 5);
        assert_eq!(radio.tx_power, 22);
        assert_eq!(radio.region_key, Some("americas"));
        assert_eq!(radio.preset_key, Some("long_fast"));
    }

    #[test]
    fn custom_lora_args_support_433_band_and_advanced_params() {
        let radio = resolve_lora_radio_args(LoraRadioArgs {
            region_key: Some("uhf_433"),
            preset_key: Some("medium_fast"),
            custom_params: true,
            frequency: 433_000_000,
            bandwidth: 125_000,
            spreading_factor: 10,
            coding_rate: 6,
            tx_power: 17,
            airtime_limit_short: None,
            airtime_limit_long: None,
        })
        .expect("433 MHz custom params");

        assert_eq!(radio.frequency, 433_000_000);
        assert_eq!(radio.bandwidth, 125_000);
        assert_eq!(radio.spreading_factor, 10);
        assert_eq!(radio.coding_rate, 6);
        assert_eq!(radio.tx_power, 17);
        assert_eq!(radio.region_key, Some("uhf_433"));
        assert_eq!(radio.preset_key, None);
    }

    #[test]
    fn lora_args_validate_airtime_limits() {
        let base = LoraRadioArgs {
            region_key: Some("americas"),
            preset_key: Some("medium_fast"),
            custom_params: false,
            frequency: 915_000_000,
            bandwidth: 250_000,
            spreading_factor: 9,
            coding_rate: 5,
            tx_power: 17,
            airtime_limit_short: Some(33.0),
            airtime_limit_long: Some(3.3),
        };

        let radio = resolve_lora_radio_args(base).expect("valid airtime limits");
        assert_eq!(radio.airtime_limit_short, Some(33.0));
        assert_eq!(radio.airtime_limit_long, Some(3.3));

        assert!(
            resolve_lora_radio_args(LoraRadioArgs {
                airtime_limit_short: Some(100.5),
                ..base
            })
            .is_err()
        );
        assert!(
            resolve_lora_radio_args(LoraRadioArgs {
                airtime_limit_long: Some(-0.1),
                ..base
            })
            .is_err()
        );
        assert!(
            resolve_lora_radio_args(LoraRadioArgs {
                airtime_limit_short: Some(f64::NAN),
                ..base
            })
            .is_err()
        );
    }
}

#[cfg(test)]
mod ble_probe_tests {
    use super::*;

    #[cfg(not(feature = "ble"))]
    #[tokio::test]
    async fn ble_probe_without_feature_reports_stub() {
        let probe = ble_platform_probe().await;
        assert!(!probe.available);
        assert_eq!(probe.missing, vec!["ble feature not compiled".to_string()]);
        assert_eq!(probe.auth_state, None);
        assert!(!probe.permission_required);
    }

    #[cfg(all(feature = "ble", target_os = "macos"))]
    #[tokio::test]
    async fn ble_probe_macos_skips_probe_and_reports_available() {
        let probe = ble_platform_probe().await;
        assert!(probe.available);
        assert!(probe.missing.is_empty());
        assert_eq!(probe.auth_state, None);
        assert!(!probe.permission_required);
    }
}
