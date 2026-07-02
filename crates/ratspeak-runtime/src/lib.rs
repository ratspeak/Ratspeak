//! Ratspeak runtime: RNS + LXMF + LRGP wiring, AppState, async loops.
//! Depends on `ratspeak-core` + `ratspeak-db` plus the protocol crates.
//! Holds zero `tauri::*` — emits go through `ratspeak_core::Emitter`.

// Holding `std::sync::MutexGuard` / `RwLockGuard` across `.await` breaks
// `Send` bounds or stalls the executor.
#![warn(clippy::await_holding_lock)]

pub mod announce_handlers;
pub mod blackhole;
#[cfg(feature = "hardware")]
pub mod hardware;
pub mod helpers;
pub mod identity_prune;
pub mod lxmf;
pub mod messaging;
pub mod propagation;
pub mod rns;
pub mod rns_config;
pub mod state;
pub mod vault;
#[cfg(feature = "lxst-voice")]
pub mod voice;

#[cfg(target_os = "ios")]
pub mod platform_ios;

// Re-exports so files moved over from the dashboard keep `crate::config`,
// `crate::db`, `crate::static_nodes` paths working without per-file edits.
pub use ratspeak_core::config;
pub use ratspeak_db as db;
pub use ratspeak_db::static_nodes;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rns_identity::destination::Destination;
use serde_json::{Value, json};

use state::AppState;

const CHANNEL_BUFFER_SIZE: usize = 64;

// ~150 bytes/entry → ~750 KB ceiling for hub bootstrap bursts.
const ANNOUNCE_HISTORY_CAP: usize = 5_000;
const AUTO_INBOX_READY_RETRY_SECS: f64 = 30.0;
const OPPORTUNISTIC_ANNOUNCE_COOLDOWN: Duration = Duration::from_secs(60);

fn lxmf_progress_activity_label(step: &str) -> Option<&'static str> {
    match step {
        "link_establishing" => Some("Direct link establishing"),
        "resource_link_ready" => Some("Resource link ready"),
        "resource_advertised" => Some("Resource transfer advertised"),
        "resource_transferring" => Some("Resource transfer sending chunks"),
        "resource_waiting_for_proof" => Some("Resource transfer waiting for proof"),
        _ => None,
    }
}

fn lxmf_progress_activity_detail(update: &lxmf::LxmfDeliveryProgressUpdate) -> String {
    let mut parts = Vec::new();
    if let Some(progress) = update.progress {
        let pct = (progress * 100.0).round().clamp(1.0, 99.0);
        parts.push(format!("{pct:.0}%"));
    }
    parts.push(update.msg_id[..8.min(update.msg_id.len())].to_string());
    if let Some(link_id) = update.link_id.as_deref() {
        parts.push(format!("link {}", &link_id[..8.min(link_id.len())]));
    }
    parts.join(" - ")
}

pub fn telephony_hash_for_identity_hex(identity_hash_hex: &str) -> Option<String> {
    let bytes = hex::decode(identity_hash_hex).ok()?;
    if bytes.len() != 16 {
        return None;
    }
    let mut identity_hash = [0u8; 16];
    identity_hash.copy_from_slice(&bytes);
    Some(hex::encode(Destination::hash_from_name_and_identity(
        db::PEER_SERVICE_LXST_TELEPHONY,
        Some(&identity_hash),
    )))
}

fn inbound_packet_targets_destination(raw: &[u8], destination_hash: [u8; 16]) -> bool {
    rns_wire::header::PacketHeader::unpack(raw)
        .map(|(header, _)| header.destination_hash == destination_hash)
        .unwrap_or(false)
}

pub fn apply_lxmf_settings_from_state(state: &AppState, mgr: &mut lxmf::LxmfManager) {
    let enforce = state
        .enforce_stamps
        .load(std::sync::atomic::Ordering::Relaxed);
    let stamp_cost = state
        .required_stamp_cost
        .load(std::sync::atomic::Ordering::Relaxed);
    mgr.router.set_enforce_stamps(enforce);
    mgr.router.config.stamp_cost = if enforce && stamp_cost > 0 {
        Some(stamp_cost)
    } else {
        None
    };
    mgr.announce_ratspeak_usage = state.announce_ratspeak_usage_enabled();

    let hosting = state
        .propagation_node_hosting_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let pn_cost = state
        .propagation_node_stamp_cost
        .load(std::sync::atomic::Ordering::Relaxed);
    mgr.router.set_propagation_enabled(hosting);
    mgr.router
        .set_stamp_requirements(pn_cost, lxmf_core::constants::PROPAGATION_COST_FLEX);
}

fn short_id(s: &str) -> &str {
    let n = s.len().min(8);
    s.get(..n).unwrap_or("")
}

fn compact_hash_label(hash: &str) -> String {
    if hash.len() > 12 {
        format!("{}..{}", &hash[..6], &hash[hash.len() - 6..])
    } else {
        hash.to_string()
    }
}

pub(crate) fn stable_notification_id(key: &str, offset: i32) -> i32 {
    let mut h: u32 = 0x811c9dc5;
    for b in key.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    offset + ((h >> 1) % 1_000_000) as i32
}

fn local_now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn unix_now_ms() -> u64 {
    unix_secs_to_ms(local_now_ts()).unwrap_or(0)
}

fn unix_secs_to_ms(timestamp: f64) -> Option<u64> {
    if !timestamp.is_finite() || timestamp <= 0.0 {
        return None;
    }
    Some((timestamp * 1000.0).round().clamp(0.0, u64::MAX as f64) as u64)
}

async fn next_chat_observed_timestamp(
    state: &AppState,
    counterpart_hash: &str,
    identity_id: &str,
) -> f64 {
    let observed_at = local_now_ts();
    let counterpart = counterpart_hash.to_string();
    let identity = identity_id.to_string();
    db::spawn_db(state.db.clone(), move |p| {
        db::next_conversation_observed_timestamp(&p, &counterpart, &identity, observed_at)
    })
    .await
    .unwrap_or(observed_at)
}

pub(crate) fn contact_label_from_db(
    pool: &db::DbPool,
    source_hash: &str,
    identity_id: &str,
) -> String {
    if let Some(label) = db::get_contact(pool, source_hash, identity_id).and_then(|c| {
        c.get("display_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    }) {
        return label;
    }

    let hashes = [source_hash.to_string()];
    db::get_peers_by_hashes(pool, &hashes, identity_id)
        .into_iter()
        .find_map(|peer| {
            let display_name = peer.display_name.trim();
            if display_name.is_empty() {
                None
            } else {
                Some(display_name.to_string())
            }
        })
        .unwrap_or_else(|| compact_hash_label(source_hash))
}

fn notification_body(content: &str, has_attachment: bool) -> String {
    let preview: String = content.trim().chars().take(120).collect();
    if !preview.is_empty() {
        preview
    } else if has_attachment {
        "New attachment".to_string()
    } else {
        "New message".to_string()
    }
}

async fn notify_inbound_message_if_background(
    state: &AppState,
    source_hash: &str,
    identity_id: &str,
    content: &str,
    has_attachment: bool,
) {
    if state.is_foreground() || !state.native_notifications_enabled() {
        return;
    }

    let source_for_db = source_hash.to_string();
    let identity_for_db = identity_id.to_string();
    let pool = state.db.clone();
    let label = db::spawn_db(pool, move |p| {
        contact_label_from_db(&p, &source_for_db, &identity_for_db)
    })
    .await
    .unwrap_or_else(|_| compact_hash_label(source_hash));

    state.emit_native_notification(ratspeak_core::NativeNotification::message(
        format!("Message from {label}"),
        notification_body(content, has_attachment),
        format!("lxmf:{source_hash}"),
        stable_notification_id(source_hash, 1_000),
    ));
}

fn game_name(app_id: &str) -> &'static str {
    match app_id {
        "chess" => "chess",
        "tictactoe" | "tic-tac-toe" => "tic-tac-toe",
        _ => "a game",
    }
}

fn notify_game_if_background(
    state: &AppState,
    sender_hash: &str,
    session_id: &str,
    app_id: &str,
    command: &str,
    is_new_session: bool,
) {
    if state.is_foreground() || !state.native_notifications_enabled() {
        return;
    }

    let identity_id = helpers::active_identity_id(state);
    let label = contact_label_from_db(&state.db, sender_hash, &identity_id);
    let game = game_name(app_id);
    let is_challenge = is_new_session
        || command.eq_ignore_ascii_case("challenge")
        || command.eq_ignore_ascii_case("invite");
    let (title, body) = if is_challenge {
        (
            "Game challenge",
            format!("{label} challenged you to {game}"),
        )
    } else if command.eq_ignore_ascii_case("move") {
        ("Game update", format!("{label} made a move in {game}"))
    } else {
        ("Game update", format!("{label} sent a {game} update"))
    };

    state.emit_native_notification(ratspeak_core::NativeNotification::game(
        title,
        body,
        format!("lrgp:{session_id}"),
        stable_notification_id(session_id, 2_000_000),
    ));
}

/// Release the BLE Peer peripheral before exit. Windows requires explicit
/// `StopAdvertising`; process-death leaves a 5-10s ghost advertisement.
/// Does not touch DB / events so next-launch toggle state is preserved.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub async fn shutdown_ble_peer_for_exit() {
    #[cfg(feature = "ble")]
    rns_interface::ble_peer::stop_ble_peer_interface().await;
}

/// Soft-restart: stop all RNS/LXMF tasks, then re-init.
pub async fn restart_rns_lxmf(state: Arc<AppState>) {
    shutdown_rns_lxmf(&state).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    if let Ok(mut sig) = state.session_shutdown.write() {
        *sig = rns_runtime::lifecycle::ShutdownSignal::new();
    }
    state.set_startup_stage("checking");
    let data_dir = state.config.data_root.clone();
    init_rns_lxmf(state, data_dir).await;
}

fn seed_identity_rns_config_from_app_private(
    app_config_dir: &std::path::Path,
    identity_config_dir: &std::path::Path,
) {
    let source = app_config_dir.join("config");
    let target = identity_config_dir.join("config");
    if target.exists() || !source.exists() || app_config_dir == identity_config_dir {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(identity_config_dir) {
        tracing::warn!(
            path = %identity_config_dir.display(),
            error = %e,
            "failed to prepare identity Reticulum config directory"
        );
        return;
    }
    let source_content = match std::fs::read_to_string(&source) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!(
                source = %source.display(),
                error = %e,
                "failed to read app-private Reticulum config for identity seed"
            );
            return;
        }
    };
    let identity_content = rns_config::strip_legacy_default_auto_interface(&source_content);
    if let Err(e) = std::fs::write(&target, identity_content) {
        tracing::warn!(
            source = %source.display(),
            target = %target.display(),
            error = %e,
            "failed to seed identity Reticulum config from app-private config"
        );
    }
}

fn normalize_startup_transport_mode(mode: &str) -> Option<&'static str> {
    match mode.trim() {
        "on" => Some("on"),
        "off" => Some("off"),
        "auto" => Some("auto"),
        _ => None,
    }
}

fn persisted_startup_transport_mode(state: &AppState, config_dir: &std::path::Path) -> String {
    db::get_setting(&state.db, "transport_mode")
        .and_then(|mode| normalize_startup_transport_mode(&mode).map(str::to_string))
        .unwrap_or_else(|| {
            if rns_config::transport_mode_enabled(config_dir) {
                "on".to_string()
            } else {
                "off".to_string()
            }
        })
}

fn persisted_startup_transport_network_type(state: &AppState) -> String {
    db::get_setting(&state.db, "transport_network_type").unwrap_or_else(|| "unknown".to_string())
}

fn startup_cfg_str(entry: &Value, key: &str) -> Option<String> {
    entry.get(key).and_then(Value::as_str).map(str::to_string)
}

fn startup_cfg_u16(entry: &Value, key: &str) -> Option<u16> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<u16>().ok())
}

fn startup_cfg_bool_default_true(entry: &Value, key: &str) -> bool {
    entry
        .get(key)
        .and_then(Value::as_str)
        .map(|s| {
            !matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "false" | "no" | "0" | "off"
            )
        })
        .unwrap_or(true)
}

fn startup_transport_auto_network_allows(network_type: &str) -> bool {
    match network_type.trim().to_ascii_lowercase().as_str() {
        "wifi" | "ethernet" => true,
        "unknown" => !cfg!(any(target_os = "android", target_os = "ios")),
        _ => false,
    }
}

fn startup_interface_group_has_enabled(ifaces: &Value, key: &str) -> bool {
    ifaces
        .get(key)
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| startup_cfg_bool_default_true(entry, "enabled"))
        })
}

fn startup_has_enabled_lora_interface(ifaces: &Value) -> bool {
    startup_interface_group_has_enabled(ifaces, "rnode")
}

fn startup_has_enabled_non_lora_transport_interface(ifaces: &Value) -> bool {
    [
        "auto",
        "tcp_client",
        "tcp_server",
        "backbone_client",
        "backbone_server",
    ]
    .into_iter()
    .any(|key| startup_interface_group_has_enabled(ifaces, key))
}

const STARTUP_PUBLIC_TCP_ENDPOINTS: &[(&str, u16, &str)] = &[
    ("1.ratspeak.org", 4141, "ratspeak-ruby"),
    ("2.ratspeak.org", 4242, "ratspeak-emerald"),
    ("rns.ratspeak.org", 4242, "ratspeak-emerald"),
    ("3.ratspeak.org", 4343, "ratspeak-diamond"),
    ("rns.beleth.net", 4242, "beleth"),
    ("rmap.world", 4242, "rmap"),
];

fn startup_normalise_public_tcp_host(host: &str) -> String {
    let mut value = host.trim().to_ascii_lowercase();
    if let Some((_, tail)) = value.split_once("://") {
        value = tail.to_string();
    }
    if let Some((head, _)) = value.split_once('/') {
        value = head.to_string();
    }
    value.trim_end_matches('.').to_string()
}

fn startup_public_tcp_server_id(host: &str, port: u16) -> Option<&'static str> {
    let host = startup_normalise_public_tcp_host(host);
    STARTUP_PUBLIC_TCP_ENDPOINTS
        .iter()
        .find_map(|(public_host, public_port, id)| {
            (host == *public_host && port == *public_port).then_some(*id)
        })
}

fn startup_public_tcp_server_id_from_entry(entry: &Value) -> Option<&'static str> {
    startup_public_tcp_server_id(
        &startup_cfg_str(entry, "target_host")?,
        startup_cfg_u16(entry, "target_port")?,
    )
}

fn startup_enabled_public_tcp_server_count(ifaces: &Value) -> usize {
    let mut ids = Vec::new();
    if let Some(entries) = ifaces.get("tcp_client").and_then(Value::as_array) {
        for entry in entries {
            if !startup_cfg_bool_default_true(entry, "enabled") {
                continue;
            }
            if let Some(id) = startup_public_tcp_server_id_from_entry(entry)
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
    }
    ids.len()
}

fn startup_auto_transport_enabled_for_interfaces(ifaces: &Value, network_type: &str) -> bool {
    startup_transport_auto_network_allows(network_type)
        && startup_has_enabled_non_lora_transport_interface(ifaces)
        && !startup_has_enabled_lora_interface(ifaces)
        && startup_enabled_public_tcp_server_count(ifaces) <= 1
}

fn reconcile_persisted_transport_mode_for_startup(state: &AppState, config_dir: &std::path::Path) {
    let mode = persisted_startup_transport_mode(state, config_dir);
    let enable = match mode.as_str() {
        "on" => true,
        "auto" => {
            let ifaces = rns_config::get_all_interfaces(config_dir);
            let network_type = persisted_startup_transport_network_type(state);
            startup_auto_transport_enabled_for_interfaces(&ifaces, &network_type)
        }
        _ => false,
    };

    if !rns_config::set_transport_mode(config_dir, enable) {
        tracing::warn!(
            mode = %mode,
            path = %config_dir.join("config").display(),
            "failed to reconcile persisted transport mode before RNS startup"
        );
    }
}

/// Soft-shutdown: stop RNS/LXMF tasks without re-init. App stays open.
pub async fn shutdown_rns_lxmf(state: &Arc<AppState>) {
    // Supersede any pending auto-lock timer for the session being torn down.
    state
        .hw_lock_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    state.emit_to_all("system_status", json!({"status": "stopping"}));
    #[cfg(feature = "lxst-voice")]
    voice::shutdown_voice_service(state).await;
    if let Ok(sig) = state.session_shutdown.read() {
        sig.trigger();
    }
    // Hold a backend-preserving clone of a hardware identity so we can re-lock the
    // token AFTER the signing loops stop — locking earlier would leave a window of
    // failed/garbage signatures.
    let hw_identity = state.lxmf.lock().ok().and_then(|lxmf| {
        lxmf.as_ref()
            .filter(|m| m.is_hardware)
            .map(|m| m.identity.clone())
    });
    let rns_mgr = state.rns.write().ok().and_then(|mut rns| rns.take());
    if let Some(mgr) = rns_mgr {
        teardown_rns_runtime_interfaces(&mgr.handle).await;
        mgr.shutdown();
    }
    // Persist ratchet + peer-key state before dropping the manager.
    if let Ok(lxmf) = state.lxmf.lock()
        && let Some(ref mgr) = *lxmf
    {
        mgr.save_crypto_state();
    }
    if let Ok(mut lxmf) = state.lxmf.lock() {
        *lxmf = None;
    }
    // All signing loops are down — re-lock the token (drops the on-card PIN cache).
    if let Some(id) = hw_identity {
        id.lock();
    }
    state.clear_identity_scoped_runtime_state();
    tokio::time::sleep(Duration::from_millis(300)).await;
    state.set_startup_stage("stopped");
    state.emit_to_all("system_status", json!({"status": "stopped"}));
}

async fn teardown_rns_runtime_interfaces(handle: &rns_runtime::reticulum::ReticulumHandle) {
    let stats = tokio::time::timeout(
        Duration::from_secs(2),
        handle.query_transport(rns_transport::messages::TransportQuery::GetInterfaceStats),
    )
    .await
    .ok()
    .flatten();

    let Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(stats)) = stats else {
        tracing::warn!("RNS shutdown could not enumerate live interfaces before actor stop");
        return;
    };

    for iface in stats {
        // The BLE Peer interface needs its own teardown: the generic path only
        // aborts the read task + deregisters, leaving the peripheral advertising
        // and the mesh loops running (a ghost session against a dead transport)
        // after a soft restart / identity switch / shutdown.
        #[cfg(feature = "ble")]
        if iface.name == "Bluetooth Peer" || iface.name == "BLE Mesh" {
            rns_runtime::reticulum::teardown_ble_peer_interface(handle, iface.id).await;
            continue;
        }
        rns_runtime::reticulum::teardown_interface(handle, iface.id).await;
    }
}

/// Initialize RNS runtime and LXMF manager.
/// Arm the hardware auto-lock timer (no-op unless `hardware_session_timeout` > 0).
/// The timer fires once; it locks the session only if its generation still matches
/// (i.e. the session wasn't switched/unlocked/quit in the meantime).
fn arm_hw_lock_timer(state: &Arc<AppState>) {
    let secs = db::get_setting(&state.db, "hardware_session_timeout")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if secs == 0 {
        return;
    }
    let generation = state.hw_lock_gen.load(std::sync::atomic::Ordering::SeqCst);
    let st = Arc::clone(state);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(secs)).await;
        lock_hardware_session(st, generation).await;
    });
}

/// Auto-lock fired: tear down the session (which re-locks the token) and enter the
/// locked state so the UI prompts for the PIN again.
async fn lock_hardware_session(state: Arc<AppState>, generation: u64) {
    // Serialize with switch/unlock so two teardowns can't race on rns/lxmf.
    let _guard = state.identity_switch_lock.lock().await;
    // Superseded while we waited on the lock (switch / unlock / quit)?
    if state.hw_lock_gen.load(std::sync::atomic::Ordering::SeqCst) != generation {
        return;
    }
    let hash = state.lxmf.lock().ok().and_then(|l| {
        l.as_ref()
            .filter(|m| m.is_hardware)
            .map(|m| m.identity_hash.clone())
    });
    let Some(hash) = hash else { return };
    tracing::info!(%hash, "hardware session auto-lock timeout — locking");
    shutdown_rns_lxmf(&state).await;
    state.set_hw_locked(Some(hash.clone()));
    state.set_startup_stage("hw_locked");
    state.emit_to_all(
        "hardware_locked",
        serde_json::json!({ "hash": hash, "reason": "timeout" }),
    );
}

/// Validate a 12-word recovery phrase and derive the 64-byte Reticulum private
/// key (`X25519_prv || Ed25519_seed`) for a SOFTWARE identity. Same BIP-39 scheme
/// as recoverable hardware provisioning, so the restored identity matches the
/// YubiKey-backed one. Hardware-independent — works on every platform.
#[cfg(feature = "seed")]
pub fn derive_identity_key_from_phrase(phrase: &str) -> Result<[u8; 64], String> {
    if !rns_ratkey::seed::validate_mnemonic(phrase) {
        return Err("Invalid recovery phrase — expected 12 valid BIP-39 words".into());
    }
    let derived = rns_ratkey::seed::derive_identity(phrase)
        .map_err(|e| format!("Could not derive identity: {e}"))?;
    let mut key = [0u8; 64];
    key[..32].copy_from_slice(&derived.x25519_secret);
    key[32..].copy_from_slice(&derived.ed25519_seed);
    Ok(key)
}

/// Generate a fresh recoverable identity: a new BIP-39 mnemonic + the 64-byte
/// Reticulum private key derived from it. The caller writes/imports the key as a
/// software identity and stores the mnemonic with the same at-rest protection as
/// the identity key so it can be re-displayed after re-authentication.
#[cfg(feature = "seed")]
pub fn generate_recoverable_key() -> Result<(String, [u8; 64]), String> {
    let mnemonic = rns_ratkey::seed::generate_mnemonic()
        .map_err(|e| format!("Could not generate recovery phrase: {e}"))?;
    let key = derive_identity_key_from_phrase(&mnemonic)?;
    Ok((mnemonic, key))
}

fn has_identity_material(ratspeak_dir: &std::path::Path) -> bool {
    profile_has_identity_material(ratspeak_dir)
        || (ratspeak_dir.join("identities").is_dir()
            && std::fs::read_dir(ratspeak_dir.join("identities"))
                .map(|entries| {
                    entries
                        .flatten()
                        .any(|e| profile_has_identity_material(&e.path()))
                })
                .unwrap_or(false))
}

fn profile_has_identity_material(dir: &std::path::Path) -> bool {
    dir.join("identity").exists()
        || dir.join("identity.enc").exists()
        || dir.join("identity.hwid").exists()
}

fn has_plain_identity_material(ratspeak_dir: &std::path::Path) -> bool {
    ratspeak_dir.join("identity").exists()
        || std::fs::read_dir(ratspeak_dir.join("identities"))
            .map(|entries| {
                entries
                    .flatten()
                    .any(|entry| entry.path().join("identity").exists())
            })
            .unwrap_or(false)
}

pub async fn init_rns_lxmf(state: Arc<AppState>, data_dir: std::path::PathBuf) {
    propagation::seed_static_nodes(&state);

    let ratspeak_dir = data_dir.join(".ratspeak");
    let has_identity = has_identity_material(&ratspeak_dir);

    if !has_identity {
        tracing::info!("No identity found — starting in setup mode");
        state.set_startup_stage("ready");
        return;
    }

    state.set_startup_stage("lxmf");
    let preferred_identity_hash = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
        .await
        .expect("db task panicked")
        .and_then(|identity| {
            identity
                .get("hash")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    if preferred_identity_hash.is_none() && !has_plain_identity_material(&ratspeak_dir) {
        tracing::warn!(
            "Identity material exists without an active identity row; returning to setup"
        );
        state.set_startup_stage("ready");
        return;
    }

    // Protected identities need a secret to unlock. If the active identity is
    // hardware or passcode-encrypted and no secret is staged, enter the locked
    // state and wait for `unlock_identity` rather than coming up with no identity.
    let hw_pin = state.take_pending_hw_pin();
    // Detect whether the active identity is protected (needs a secret to unlock):
    // hardware (.hwid → YubiKey PIN) or passcode-encrypted (.enc → passcode).
    let lock_kind = preferred_identity_hash.as_deref().and_then(|h| {
        let dir = data_dir.join(".ratspeak").join("identities").join(h);
        if dir.join("identity.hwid").exists() {
            Some("hardware")
        } else if dir.join("identity.enc").exists() {
            Some("passcode")
        } else {
            None
        }
    });
    let active_is_protected = lock_kind.is_some();
    if active_is_protected && hw_pin.is_none() {
        let hash = preferred_identity_hash.clone().unwrap_or_default();
        let kind = lock_kind.unwrap_or("hardware");
        tracing::info!(%hash, kind, "identity locked — awaiting unlock secret");
        state.set_hw_locked(Some(hash.clone()));
        state.set_startup_stage("hw_locked");
        state.emit_to_all(
            "hardware_locked",
            serde_json::json!({ "hash": hash, "kind": kind, "reason": "secret_required" }),
        );
        return;
    }

    match lxmf::LxmfManager::load_or_create(&data_dir, preferred_identity_hash.as_deref(), hw_pin) {
        Ok(mut mgr) => {
            state.set_hw_locked(None);
            state.set_hw_last_error(None);
            if let Some(preferred) = preferred_identity_hash.as_deref()
                && mgr.identity_hash != preferred
            {
                tracing::error!(
                    loaded = %mgr.identity_hash,
                    active = %preferred,
                    "loaded LXMF identity does not match active identity"
                );
                state.set_startup_stage("error");
                return;
            }

            let active = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
                .await
                .expect("db task panicked");
            if active.is_none() {
                let id_hash = mgr.identity_hash.clone();
                let lxmf_hash = mgr.lxmf_hash.clone();
                // Match the default used by setup + identity creation paths
                // so auto-recovered identities still announce a meaningful name.
                let default_display_name =
                    format!("!Ratspeak.org-{}", &lxmf_hash[..6.min(lxmf_hash.len())]);
                db::spawn_db(state.db.clone(), move |p| {
                    db::save_identity(&p, &id_hash, &lxmf_hash, "Default", &default_display_name);
                })
                .await
                .expect("db task panicked");
                let id_hash_for_set = mgr.identity_hash.clone();
                let set_result = db::spawn_db(state.db.clone(), move |p| {
                    db::set_active_identity(&p, &id_hash_for_set)
                })
                .await
                .expect("db task panicked");
                if let Err(e) = set_result {
                    tracing::error!("Failed to set active identity: {e}");
                }
            }

            if let Some(identity) = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
                .await
                .expect("db task panicked")
            {
                mgr.display_name = identity
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                mgr.status = identity
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }

            apply_lxmf_settings_from_state(&state, &mut mgr);

            // Backfill identity_id on pre-multi-identity rows.
            let id_hash_for_backfill = mgr.identity_hash.clone();
            db::spawn_db(state.db.clone(), move |p| {
                db::backfill_identity_id(&p, &id_hash_for_backfill);
            })
            .await
            .expect("db task panicked");

            // Clear in-flight outbound from previous session.
            let id_hash_for_cleanup = mgr.identity_hash.clone();
            db::spawn_db(state.db.clone(), move |p| {
                db::cleanup_stale_outbound(&p, &id_hash_for_cleanup);
            })
            .await
            .expect("db task panicked");

            let (display_name, status) =
                db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
                    .await
                    .expect("db task panicked")
                    .map(|i| {
                        (
                            i.get("display_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            i.get("status")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        )
                    })
                    .unwrap_or_default();
            state.emit_to_all(
                "lxmf_identity",
                json!({
                    "hash": mgr.lxmf_hash,
                    "identity_hash": mgr.identity_hash,
                    "display_name": display_name,
                    "status": status,
                }),
            );

            let identity_id = helpers::active_identity_id(&state);
            if !identity_id.is_empty() {
                let identity_id_for_contacts = identity_id.clone();
                let contacts = db::spawn_db(state.db.clone(), move |p| {
                    db::get_all_contacts(&p, &identity_id_for_contacts)
                })
                .await
                .expect("db task panicked");
                let contacts_list: Vec<serde_json::Value> = contacts
                    .into_iter()
                    .map(|c| {
                        serde_json::json!({
                            "hash": c.get("dest_hash"),
                            "display_name": c.get("display_name"),
                            "trust": c.get("trust"),
                            "notes": c.get("notes"),
                            "first_seen": c.get("first_seen"),
                            "last_seen": c.get("last_seen"),
                            "services": c.get("services"),
                        })
                    })
                    .collect();
                state.emit_to_all("contacts_update", serde_json::json!(contacts_list));
            }

            state.set_lxmf(mgr);

            // Pre-warm conversations cache so first paint doesn't await DB.
            if let Some(payload) = messaging::build_conversations_payload(&state).await {
                state.emit_to_all("conversations_update", payload);
            } else {
                tracing::warn!("conversations pre-warm failed; tab will fetch on demand");
            }
            tracing::info!("LXMF manager initialized");
            // Protected identities (hardware PIN or software passcode) can auto-lock
            // after an idle timeout (off by default).
            if active_is_protected {
                arm_hw_lock_timer(&state);
            }
            state.request_poll_now();
        }
        Err(e) => {
            let msg = e.to_string();
            tracing::error!("Failed to initialize LXMF: {msg}");
            if active_is_protected {
                let hash = preferred_identity_hash.clone().unwrap_or_default();
                let kind = lock_kind.unwrap_or("hardware");
                state.set_hw_last_error(Some(msg.clone()));
                state.set_hw_locked(Some(hash.clone()));
                state.set_startup_stage("hw_locked");
                state.emit_to_all(
                    "hardware_locked",
                    serde_json::json!({ "hash": hash, "kind": kind, "error": msg }),
                );
                return;
            }
        }
    }

    state.set_startup_stage("rns");
    let active_runtime_identity = state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(|mgr| mgr.identity_hash.clone()));
    let config_dir = if state.config.uses_app_private_rns_config_dir() {
        if let Some(identity_hash) = active_runtime_identity.as_deref() {
            let dir = state.config.identity_rns_config_dir(identity_hash);
            seed_identity_rns_config_from_app_private(&state.config.rns_config_dir, &dir);
            dir
        } else {
            state.config.rns_config_dir.clone()
        }
    } else {
        state.config.rns_config_dir.clone()
    };
    if state.config.uses_app_private_rns_config_dir() {
        match rns_config::ensure_app_private_shared_instance_ports(&config_dir) {
            Ok(rns_config::RatspeakRnsPortConfigChange::Created) => {
                tracing::info!(
                    path = %config_dir.join("config").display(),
                    shared_instance_port = ratspeak_core::config::RATSPEAK_RNS_SHARED_INSTANCE_PORT,
                    instance_control_port = ratspeak_core::config::RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
                    "created Ratspeak app-private Reticulum config"
                );
            }
            Ok(rns_config::RatspeakRnsPortConfigChange::Updated) => {
                tracing::info!(
                    path = %config_dir.join("config").display(),
                    backup = %config_dir.join("config.backup").display(),
                    shared_instance_port = ratspeak_core::config::RATSPEAK_RNS_SHARED_INSTANCE_PORT,
                    instance_control_port = ratspeak_core::config::RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
                    "updated Ratspeak app-private Reticulum shared-instance ports"
                );
            }
            Ok(rns_config::RatspeakRnsPortConfigChange::Unchanged) => {}
            Err(e) => {
                tracing::warn!(
                    path = %config_dir.join("config").display(),
                    error = %e,
                    "failed to prepare Ratspeak app-private Reticulum config"
                );
            }
        }
    }
    reconcile_persisted_transport_mode_for_startup(&state, &config_dir);
    let config_str = config_dir.to_string_lossy().to_string();

    // Android sandbox blocks /tmp — keep UDS under data_dir/cache.
    let socket_dir = active_runtime_identity
        .as_deref()
        .map(|identity_hash| state.config.identity_cache_dir(identity_hash))
        .unwrap_or_else(|| data_dir.join("cache"));
    std::fs::create_dir_all(&socket_dir).ok();
    let socket_dir = Some(socket_dir);

    match rns::RnsManager::init(&config_str, socket_dir, state.is_foreground.clone()).await {
        Ok(rns_mgr) => {
            let registration_info = if let Ok(mut lxmf) = state.lxmf.lock() {
                if let Some(mgr) = lxmf.as_mut() {
                    mgr.router
                        .set_transport(rns_mgr.handle.transport_tx.clone());
                    Some(mgr.lxmf_dest_hash)
                } else {
                    None
                }
            } else {
                None
            };

            // Fan delivery events: link requests + link-addressed → LinkManager,
            // direct packets → LXMF inbound handler.
            let (inbound_rx, lxmf_link_mgr_rx) = if let Some(dest_hash) = registration_info {
                let (delivery_tx, mut delivery_rx) =
                    tokio::sync::mpsc::channel(CHANNEL_BUFFER_SIZE);
                match rns_mgr
                    .handle
                    .transport_tx
                    .send(
                        rns_transport::messages::TransportMessage::RegisterDestination {
                            hash: dest_hash,
                            app_name: "lxmf.delivery".to_string(),
                            delivery_tx: Some(delivery_tx.clone()),
                        },
                    )
                    .await
                {
                    Ok(()) => {
                        tracing::info!(
                            dest = %hex::encode(dest_hash),
                            "LXMF destination registered with transport"
                        );
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "CRITICAL: Failed to register LXMF destination — ALL inbound messages will be lost");
                    }
                }
                if let Ok(mut lxmf) = state.lxmf.lock()
                    && let Some(mgr) = lxmf.as_mut()
                {
                    mgr.delivery_tx = Some(delivery_tx);
                }

                let (pkt_tx, pkt_rx) = tokio::sync::mpsc::channel(CHANNEL_BUFFER_SIZE);
                let (link_tx, link_rx) = tokio::sync::mpsc::channel(CHANNEL_BUFFER_SIZE);
                let dispatch_dest_hash = dest_hash;
                let dispatch_shutdown = state
                    .session_shutdown
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                tokio::spawn(async move {
                    loop {
                        let event = tokio::select! {
                            _ = dispatch_shutdown.wait() => break,
                            ev = delivery_rx.recv() => match ev {
                                Some(e) => e,
                                None => break,
                            },
                        };
                        match &event {
                            rns_transport::link_messages::DestinationEvent::LinkRequest {
                                ..
                            } => {
                                let _ = link_tx.send(event).await;
                            }
                            rns_transport::link_messages::DestinationEvent::InboundPacket {
                                raw,
                                ..
                            } => {
                                // Our-dest = opportunistic delivery; else = link packet.
                                let is_our_dest =
                                    inbound_packet_targets_destination(raw, dispatch_dest_hash);
                                if is_our_dest {
                                    let _ = pkt_tx.send(event).await;
                                } else {
                                    let _ = link_tx.send(event).await;
                                }
                            }
                            _ => {
                                let _ = pkt_tx.send(event).await;
                            }
                        }
                    }
                });
                (Some(pkt_rx), Some(link_rx))
            } else {
                (None, None)
            };

            // Register propagation destination and start inbound propagation LinkManager
            {
                let prop_info = state.lxmf.lock().ok().and_then(|l| {
                    let mgr = l.as_ref()?;
                    let signing_key = mgr.identity.get_signing_key()?;
                    let priv_key = mgr.identity.get_private_key()?;
                    let identity =
                        rns_identity::identity::Identity::from_private_key(&*priv_key).ok()?;
                    Some((mgr.propagation_dest_hash, identity, signing_key))
                });

                if let Some((prop_dest_hash, identity, signing_key)) = prop_info {
                    let (prop_tx, prop_rx) = tokio::sync::mpsc::channel(CHANNEL_BUFFER_SIZE);
                    match rns_mgr
                        .handle
                        .transport_tx
                        .send(
                            rns_transport::messages::TransportMessage::RegisterDestination {
                                hash: prop_dest_hash,
                                app_name: "lxmf.propagation".to_string(),
                                delivery_tx: Some(prop_tx),
                            },
                        )
                        .await
                    {
                        Ok(()) => {
                            tracing::info!(
                                dest = %hex::encode(prop_dest_hash),
                                "propagation destination registered with transport"
                            );
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "failed to register propagation destination");
                        }
                    }

                    let prop_storage = {
                        let lxmf = state.lxmf.lock().ok();
                        lxmf.and_then(|l| {
                            let mgr = l.as_ref()?;
                            let storage_dir = mgr
                                .data_dir
                                .join("identities")
                                .join(&mgr.identity_hash)
                                .join("propagation");
                            Some((storage_dir, prop_dest_hash))
                        })
                    };

                    let prop_node_config = lxmf_core::propagation_node::PropagationNodeConfig {
                        min_stamp_cost: state
                            .propagation_node_stamp_cost
                            .load(std::sync::atomic::Ordering::Relaxed),
                        ..lxmf_core::propagation_node::PropagationNodeConfig::default()
                    };
                    // Captured before the config moves into the node: bounds
                    // the wrapper decode in the deposit loop below.
                    let max_transfer_bytes = prop_node_config.max_storage;

                    let prop_node = if let Some((storage_dir, dest_hash)) = prop_storage {
                        match lxmf_core::propagation_node::PropagationNode::with_storage(
                            prop_node_config.clone(),
                            dest_hash,
                            storage_dir,
                        ) {
                            Ok(node) => {
                                tracing::info!(
                                    messages = node.message_count(),
                                    "propagation node loaded"
                                );
                                node
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "failed to create propagation node with storage, using in-memory");
                                lxmf_core::propagation_node::PropagationNode::new(
                                    prop_node_config.clone(),
                                    dest_hash,
                                )
                            }
                        }
                    } else {
                        lxmf_core::propagation_node::PropagationNode::new(
                            prop_node_config,
                            prop_dest_hash,
                        )
                    };

                    let prop_node = std::sync::Arc::new(std::sync::Mutex::new(prop_node));
                    if let Ok(mut pn) = state.propagation_node.lock() {
                        *pn = Some(prop_node.clone());
                    }

                    let local_identity_hash = identity.hash;
                    let mut link_mgr = rns_runtime::link_manager::LinkManager::with_destination(
                        rns_mgr.handle.transport_tx.clone(),
                        prop_rx,
                        &identity,
                        "lxmf.propagation",
                        Some(signing_key),
                    );

                    let offer_node = prop_node.clone();
                    let get_node = prop_node.clone();
                    let link_identities = link_mgr.link_identities_handle();
                    let prop_hosting_state = state.clone();

                    // Precompute SHA-256(path)[..16] for cheap dispatch.
                    let offer_path_hash = {
                        let h = rns_crypto::sha::sha256(
                            lxmf_core::constants::OFFER_REQUEST_PATH.as_bytes(),
                        );
                        let mut ph = [0u8; 16];
                        ph.copy_from_slice(&h[..16]);
                        ph
                    };
                    let get_path_hash = {
                        let h = rns_crypto::sha::sha256(
                            lxmf_core::constants::MESSAGE_GET_PATH.as_bytes(),
                        );
                        let mut ph = [0u8; 16];
                        ph.copy_from_slice(&h[..16]);
                        ph
                    };

                    link_mgr.set_request_handler(move |link_id, path_hash, data| {
                        if !prop_hosting_state
                            .propagation_node_hosting_enabled
                            .load(std::sync::atomic::Ordering::Relaxed)
                        {
                            return None;
                        }
                        if path_hash == offer_path_hash {
                            if let Ok(mut node) = offer_node.lock() {
                                let remote_identity_hash = link_identities
                                    .lock()
                                    .ok()
                                    .and_then(|ids| ids.get(&link_id).copied());
                                let identity_known = remote_identity_hash.is_some();
                                let peer_hash = remote_identity_hash.unwrap_or([0u8; 16]);
                                Some(node.handle_offer_request(
                                    &data,
                                    lxmf_core::propagation_node::OfferRequestContext {
                                        peer_hash,
                                        identity_known,
                                        is_throttled: false,
                                        access_allowed: true,
                                        local_identity_hash: Some(&local_identity_hash),
                                        remote_identity_hash: remote_identity_hash.as_ref(),
                                    },
                                ))
                            } else {
                                None
                            }
                        } else if path_hash == get_path_hash {
                            let remote_identity_hash = link_identities
                                .lock()
                                .ok()
                                .and_then(|ids| ids.get(&link_id).copied());
                            let client_dest_hash = remote_identity_hash
                                .map(|identity_hash| {
                                    rns_identity::destination::Destination::hash_from_name_and_identity(
                                        "lxmf.delivery",
                                        Some(&identity_hash),
                                    )
                                })
                                .unwrap_or([0u8; 16]);
                            let action = if let Ok(mut node) = get_node.lock() {
                                node.handle_get_request(&data, &client_dest_hash)
                            } else {
                                return None;
                            };
                            // Phase-2 file reads happen here, after the node lock drops.
                            Some(action.into_response())
                        } else {
                            None
                        }
                    });

                    let (pkt_tx, mut pkt_rx) =
                        tokio::sync::mpsc::channel::<(Vec<u8>, [u8; 16])>(CHANNEL_BUFFER_SIZE);
                    link_mgr.set_link_packet_channel(pkt_tx);
                    let (res_tx, mut res_rx) =
                        tokio::sync::mpsc::channel::<(Vec<u8>, [u8; 16])>(CHANNEL_BUFFER_SIZE);
                    link_mgr.set_resource_completed_channel(res_tx);

                    let prop_shutdown = state
                        .session_shutdown
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = prop_shutdown.wait() => {}
                            _ = link_mgr.run() => {}
                        }
                    });

                    // Completed resources on this link = propagation deposits.
                    let store_node = prop_node.clone();
                    let store_state = state.clone();
                    let store_shutdown = state
                        .session_shutdown
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    tokio::spawn(async move {
                        loop {
                            let item = tokio::select! {
                                _ = store_shutdown.wait() => break,
                                item = pkt_rx.recv() => item,
                                item = res_rx.recv() => item,
                            };
                            let Some((data, _link_id)) = item else {
                                break;
                            };
                            let Ok((_timebase, entries)) =
                                lxmf_core::message::LxMessage::unpack_propagation_wrapper_bounded(
                                    &data,
                                    max_transfer_bytes,
                                )
                            else {
                                tracing::warn!("failed to unpack inbound propagation wrapper");
                                continue;
                            };
                            if !store_state
                                .propagation_node_hosting_enabled
                                .load(std::sync::atomic::Ordering::Relaxed)
                            {
                                continue;
                            }
                            if let Ok(mut node) = store_node.lock() {
                                let min_cost = node.min_stamp_cost();
                                let mut accepted = 0usize;
                                let mut rejected = 0usize;
                                for entry in entries {
                                    match lxmf_core::stamper::validate_pn_stamp(&entry, min_cost) {
                                        Some((_tid, lxmf_data, stamp_value, _stamp_data)) => {
                                            if node.accept_propagated_blob(
                                                &lxmf_data,
                                                stamp_value as u8,
                                            ) {
                                                accepted += 1;
                                            }
                                        }
                                        None => rejected += 1,
                                    }
                                }
                                tracing::debug!(
                                    accepted,
                                    rejected,
                                    "processed inbound propagation transfer"
                                );
                            }
                        }
                    });
                }
            }

            // Restore client propagation state. Manual re-applies the stored
            // hash; Auto selects below; Off keeps any stored hash dormant. This
            // is separate from hosted propagation-node enablement.
            let (mode, _) = propagation::read_settings(&state);
            if let Ok(mut lxmf) = state.lxmf.lock()
                && let Some(mgr) = lxmf.as_mut()
            {
                let identity_id = mgr.identity_hash.clone();
                mgr.enable_propagation(
                    mode != propagation::PropagationMode::Off,
                    &state.db,
                    &identity_id,
                );
            }
            if mode == propagation::PropagationMode::Manual {
                let stored_pn = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
                    .await
                    .expect("db task panicked")
                    .and_then(|i| {
                        i.get("propagation_node")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_default();
                if !stored_pn.is_empty()
                    && let Ok(mut lxmf) = state.lxmf.lock()
                    && let Some(mgr) = lxmf.as_mut()
                {
                    let identity_id = mgr.identity_hash.clone();
                    mgr.set_propagation_node(Some(&stored_pn), &state.db, &identity_id);
                    tracing::info!(node = %stored_pn, "restored Manual-mode propagation node from DB");
                }
            }

            // Inbound link-based message handler (decrypted by link session key).
            if let Some(link_rx) = lxmf_link_mgr_rx {
                let link_info = state.lxmf.lock().ok().and_then(|l| {
                    let mgr = l.as_ref()?;
                    // Backend-aware clone; signing_key is None for hardware identities
                    // (link-mode proofs skip, opportunistic delivery still works).
                    let signing_key = mgr.identity.get_signing_key();
                    Some((mgr.lxmf_dest_hash, mgr.identity.clone(), signing_key))
                });

                if let Some((lxmf_dest_hash, identity, signing_key)) = link_info {
                    let mut lxmf_link_mgr =
                        rns_runtime::link_manager::LinkManager::with_destination(
                            rns_mgr.handle.transport_tx.clone(),
                            link_rx,
                            &identity,
                            "lxmf.delivery",
                            signing_key,
                        );

                    let (link_pkt_tx, mut link_pkt_rx) =
                        tokio::sync::mpsc::channel::<(Vec<u8>, [u8; 16])>(CHANNEL_BUFFER_SIZE);
                    let (link_res_tx, mut link_res_rx) =
                        tokio::sync::mpsc::channel::<(Vec<u8>, [u8; 16])>(CHANNEL_BUFFER_SIZE);
                    let (link_command_tx, link_command_rx) = tokio::sync::mpsc::channel::<
                        rns_runtime::link_manager::LinkManagerCommand,
                    >(
                        CHANNEL_BUFFER_SIZE
                    );
                    let (link_identified_tx, link_identified_rx) =
                        tokio::sync::mpsc::channel::<([u8; 16], [u8; 16])>(CHANNEL_BUFFER_SIZE);
                    let (link_closed_tx, link_closed_rx) =
                        tokio::sync::mpsc::channel::<[u8; 16]>(CHANNEL_BUFFER_SIZE);
                    let (link_packet_proof_tx, link_packet_proof_rx) =
                        tokio::sync::mpsc::channel::<rns_runtime::link_manager::LinkPacketProof>(
                            CHANNEL_BUFFER_SIZE,
                        );
                    let (link_resource_proof_tx, link_resource_proof_rx) =
                        tokio::sync::mpsc::channel::<rns_runtime::link_manager::LinkResourceProof>(
                            CHANNEL_BUFFER_SIZE,
                        );
                    lxmf_link_mgr.set_link_packet_channel(link_pkt_tx.clone());
                    lxmf_link_mgr.set_resource_completed_channel(link_res_tx);
                    lxmf_link_mgr.set_link_identified_channel(link_identified_tx);
                    lxmf_link_mgr.set_link_closed_channel(link_closed_tx);
                    lxmf_link_mgr.set_link_packet_proof_channel(link_packet_proof_tx);
                    lxmf_link_mgr.set_outbound_resource_proof_channel(link_resource_proof_tx);

                    if let Ok(mut lxmf) = state.lxmf.lock()
                        && let Some(mgr) = lxmf.as_mut()
                    {
                        mgr.set_lxmf_link_control(
                            link_command_tx,
                            link_pkt_tx.clone(),
                            link_identified_rx,
                            link_closed_rx,
                            link_packet_proof_rx,
                            link_resource_proof_rx,
                        );
                    }

                    let lxmf_link_shutdown = state
                        .session_shutdown
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = lxmf_link_shutdown.wait() => {}
                            _ = lxmf_link_mgr.run_with_commands(link_command_rx) => {}
                        }
                    });

                    let link_inbound_state = state.clone();
                    let link_inbound_shutdown = state
                        .session_shutdown
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    tokio::spawn(async move {
                        loop {
                            let (data, link_id) = tokio::select! {
                                _ = link_inbound_shutdown.wait() => break,
                                item = link_pkt_rx.recv() => match item {
                                    Some((data, link_id)) => (data, link_id),
                                    None => break,
                                },
                                item = link_res_rx.recv() => match item {
                                    Some((data, link_id)) => (data, link_id),
                                    None => break,
                                },
                            };

                            // Link deliveries arrive already decrypted. Payload
                            // is the full LXMF wire format:
                            //   [dest:16][src:16][sig:64][msgpack].
                            handle_decrypted_lxmf(
                                &link_inbound_state,
                                data,
                                InboundLxmfSource::Link {
                                    link_id: Some(link_id),
                                },
                            )
                            .await;
                        }
                    });

                    tracing::info!(
                        dest = %hex::encode(lxmf_dest_hash),
                        "LXMF delivery LinkManager started — accepting link-based messages"
                    );
                }
            }

            // Clone the transport handle before moving rns_mgr into state;
            // the announce handler below still needs it.
            let transport_tx_for_handler = rns_mgr.handle.transport_tx.clone();
            state.set_rns(rns_mgr);
            tracing::info!("RNS runtime initialized");
            #[cfg(feature = "lxst-voice")]
            if let Err(e) = voice::start_voice_service(&state).await {
                tracing::warn!(error = %e, "LXST voice service did not start");
            }

            // LXMF router tick — drains the outbound queue and fires the
            // encrypt/sign pipeline.
            let tick_state = state.clone();
            let tick_shutdown = state
                .session_shutdown
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(500));
                let mut save_counter: u64 = 0;
                let mut timeout_check_counter: u64 = 0;
                let mut next_auto_inbox_ready_check_at = 0.0;
                #[cfg(feature = "mobile-throttle")]
                let mut was_foreground = true;
                loop {
                    tokio::select! {
                        _ = tick_shutdown.wait() => break,
                        _ = interval.tick() => {},
                        _ = tick_state.lxmf_notify.notified() => {},
                    }
                    // Mobile: drop to 2s while backgrounded.
                    #[cfg(feature = "mobile-throttle")]
                    {
                        let is_fg = tick_state.is_foreground();
                        if is_fg != was_foreground {
                            let period = if is_fg {
                                Duration::from_millis(500)
                            } else {
                                Duration::from_secs(2)
                            };
                            interval = tokio::time::interval(period);
                            interval.tick().await;
                            // Defer ratchet cleanup +900s to avoid a large
                            // purge in the first post-resume tick.
                            if is_fg
                                && !was_foreground
                                && let Ok(mut lxmf) = tick_state.lxmf.lock()
                                && let Some(mgr) = lxmf.as_mut()
                            {
                                mgr.mark_foreground_resume();
                            }
                            was_foreground = is_fg;
                        }
                    }
                    let network_available =
                        crate::any_interface_online_cached(&tick_state).unwrap_or(false);
                    let auto_inbox_check_due = if let Ok(lxmf) = tick_state.lxmf.lock() {
                        lxmf.as_ref()
                            .map(|mgr| mgr.auto_propagation_check_due(network_available))
                            .unwrap_or(false)
                    } else {
                        false
                    };
                    let now = local_now_ts();
                    let auto_inbox_download_ready =
                        if auto_inbox_check_due && now >= next_auto_inbox_ready_check_at {
                            let ready = propagation::auto_inbox_download_ready(&tick_state).await;
                            if ready {
                                next_auto_inbox_ready_check_at = 0.0;
                            } else {
                                next_auto_inbox_ready_check_at = now + AUTO_INBOX_READY_RETRY_SECS;
                            }
                            ready
                        } else {
                            false
                        };
                    save_counter = save_counter.wrapping_add(1);
                    let should_save_crypto_state = save_counter.is_multiple_of(600);
                    let tick_state_for_lxmf = tick_state.clone();
                    let tick_result = tokio::task::spawn_blocking(move || {
                        let lock_wait_started = std::time::Instant::now();
                        if let Ok(mut lxmf) = tick_state_for_lxmf.lxmf.lock()
                            && let Some(mgr) = lxmf.as_mut()
                        {
                            let waited = lock_wait_started.elapsed();
                            if waited > Duration::from_secs(1) {
                                tracing::warn!(
                                    waited_ms = waited.as_millis() as u64,
                                    "lxmf tick waited on manager lock"
                                );
                            }
                            let hold_started = std::time::Instant::now();
                            let results = mgr.tick_with_auto_propagation_download_ready(
                                auto_inbox_download_ready,
                            );
                            let tick_held = hold_started.elapsed();
                            if tick_held > Duration::from_secs(1) {
                                tracing::warn!(
                                    held_ms = tick_held.as_millis() as u64,
                                    "lxmf tick held manager lock (tick body)"
                                );
                            }
                            let delivery_progress = mgr.take_delivery_progress_updates();
                            let downloaded = mgr.take_downloaded_propagation_messages();
                            let (
                                completed_deposits,
                                failed_deposits,
                                completed_syncs,
                                failed_syncs,
                            ) = mgr.take_propagation_health();
                            // Persist crypto state every ~5 min (600 × 500ms).
                            if should_save_crypto_state {
                                mgr.save_crypto_state();
                            }
                            (
                                results,
                                delivery_progress,
                                downloaded,
                                completed_deposits,
                                failed_deposits,
                                completed_syncs,
                                failed_syncs,
                            )
                        } else {
                            (
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                            )
                        }
                    })
                    .await;
                    let (
                        results,
                        delivery_progress,
                        downloaded_propagation_messages,
                        completed_propagation_deposits,
                        failed_propagation_deposits,
                        completed_propagation_syncs,
                        failed_propagation_syncs,
                    ) = match tick_result {
                        Ok(result) => result,
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "lxmf tick worker failed; skipping this tick"
                            );
                            (
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                            )
                        }
                    };
                    // Hosted propagation node maintenance on the crypto-save
                    // cadence (~5 min): age-cull, weight cap, orphan cleanup.
                    // Previously never ran — the store only hard-rejected at
                    // the ingest cap once full.
                    if should_save_crypto_state {
                        let hosted_node = tick_state
                            .propagation_node
                            .lock()
                            .ok()
                            .and_then(|guard| guard.clone());
                        if let Some(node) = hosted_node
                            && let Ok(mut node) = node.lock()
                        {
                            node.tick();
                        }
                    }
                    let propagation_deposit_terminal = !completed_propagation_deposits.is_empty()
                        || !failed_propagation_deposits.is_empty();
                    for node in completed_propagation_deposits {
                        propagation::mark_relay_transaction_success(
                            &tick_state,
                            node,
                            "deposit_ok",
                        );
                    }
                    for (node, reason) in failed_propagation_deposits {
                        if network_available {
                            propagation::mark_relay_failure(&tick_state, node, &reason);
                            propagation::reconcile_active_auto_node(&tick_state).await;
                        } else {
                            tracing::info!(
                                node = %hex::encode(node),
                                reason = %reason,
                                "propagation deposit failed while offline; not penalizing relay"
                            );
                        }
                    }
                    for node in completed_propagation_syncs {
                        propagation::mark_relay_transaction_success(&tick_state, node, "sync_ok");
                    }
                    for (node, reason) in failed_propagation_syncs {
                        if network_available {
                            propagation::mark_relay_failure(&tick_state, node, &reason);
                            propagation::reconcile_active_auto_node(&tick_state).await;
                        } else {
                            tracing::info!(
                                node = %hex::encode(node),
                                reason = %reason,
                                "propagation sync failed while offline; not penalizing relay"
                            );
                        }
                    }
                    if propagation_deposit_terminal {
                        propagation::maybe_reselect_auto_after_propagation_idle(&tick_state).await;
                    }
                    // Persist before emit: a successful `lxmf_step` event
                    // must imply the DB has already accepted the transition.
                    // State rows are keyed (id, identity_id); these events come
                    // from the active identity's router.
                    let identity_for_db = if results.is_empty() {
                        String::new()
                    } else {
                        helpers::active_identity_id(&tick_state)
                    };
                    let mut persisted: Vec<(String, &'static str, Option<String>)> =
                        Vec::with_capacity(results.len());
                    for (msg_id, new_state) in &results {
                        let msg_id_for_db = msg_id.clone();
                        let identity_for_db = identity_for_db.clone();
                        let new_state_for_db = new_state.to_string();
                        let delivery_method_for_db =
                            matches!(*new_state, "propagating" | "propagated")
                                .then_some("propagated".to_string());
                        // Same blocking-pool hop also reads the method back
                        // for the emit below.
                        match db::spawn_db(tick_state.db.clone(), move |p| {
                            if let Some(method) = delivery_method_for_db.as_deref() {
                                db::update_message_delivery_method(
                                    &p,
                                    &msg_id_for_db,
                                    &identity_for_db,
                                    method,
                                );
                            }
                            db::update_message_state(
                                &p,
                                &msg_id_for_db,
                                &identity_for_db,
                                &new_state_for_db,
                                None,
                            );
                            db::get_message_delivery_method(&p, &msg_id_for_db, &identity_for_db)
                        })
                        .await
                        {
                            Ok(method) => persisted.push((msg_id.clone(), *new_state, method)),
                            Err(e) => tracing::error!(
                                msg_id = %msg_id,
                                new_state = %new_state,
                                error = %e,
                                "lxmf_tick: persist failed; skipping emit"
                            ),
                        }
                    }
                    for (msg_id, new_state, method) in &persisted {
                        let client_msg_id = tick_state
                            .msg_id_map
                            .lock()
                            .ok()
                            .and_then(|map| map.get(msg_id).cloned());
                        tick_state.emit_to_all(
                            "lxmf_step",
                            json!({
                                "step": new_state,
                                "msg_id": msg_id,
                                "client_msg_id": client_msg_id,
                                "method": method,
                            }),
                        );
                        if tick_state
                            .network_log_enabled
                            .load(std::sync::atomic::Ordering::Relaxed)
                        {
                            let level = if *new_state == "failed" {
                                "essential"
                            } else {
                                "standard"
                            };
                            tick_state.emit_network_event(
                                if *new_state == "failed" {
                                    "error"
                                } else {
                                    "message"
                                },
                                &format!("Message {}", new_state),
                                msg_id,
                                level,
                            );
                        }
                        if *new_state == "sent" {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs_f64();
                            if let Ok(mut times) = tick_state.message_send_times.lock() {
                                times.insert(msg_id.clone(), now);
                            }
                        } else if *new_state == "failed"
                            && let Ok(mut times) = tick_state.message_send_times.lock()
                        {
                            times.remove(msg_id);
                        }

                        // Route delivery-state to originating LRGP session.
                        let lrgp_meta = tick_state
                            .lrgp_msg_to_session
                            .lock()
                            .ok()
                            .and_then(|map| map.get(msg_id).cloned());
                        if let Some(meta) = lrgp_meta {
                            update_game_session_delivery_state(
                                &tick_state,
                                &meta.session_id,
                                &meta.identity_id,
                                &meta.contact_hash,
                                new_state,
                            )
                            .await;
                            if (*new_state == "delivered"
                                || *new_state == "failed"
                                || *new_state == "propagated")
                                && let Ok(mut map) = tick_state.lrgp_msg_to_session.lock()
                            {
                                map.remove(msg_id);
                            }
                        }
                    }

                    for update in delivery_progress {
                        let client_msg_id = tick_state
                            .msg_id_map
                            .lock()
                            .ok()
                            .and_then(|map| map.get(&update.msg_id).cloned());
                        tick_state.emit_to_all(
                            "lxmf_delivery_progress",
                            json!({
                                "step": update.step,
                                "msg_id": update.msg_id,
                                "client_msg_id": client_msg_id,
                                "method": update.method,
                                "progress": update.progress,
                                "link_id": update.link_id,
                                "dest_hash": update.dest_hash,
                                "attempts": update.attempts,
                                "representation": update.representation,
                                "queued_deliveries": update.queued_deliveries,
                                "in_flight_deliveries": update.in_flight_deliveries,
                                "reason": update.reason,
                            }),
                        );
                        if tick_state
                            .network_log_enabled
                            .load(std::sync::atomic::Ordering::Relaxed)
                            && let Some(label) = lxmf_progress_activity_label(update.step)
                        {
                            let detail = lxmf_progress_activity_detail(&update);
                            tick_state.emit_network_event("message", label, &detail, "detailed");
                        }
                    }

                    for data in downloaded_propagation_messages {
                        handle_decrypted_lxmf(&tick_state, data, InboundLxmfSource::Propagated)
                            .await;
                    }

                    // Every ~30s: timeout sweep + evict >1h tracking entries.
                    timeout_check_counter += 1;
                    if timeout_check_counter.is_multiple_of(60) {
                        propagation::reconcile_active_auto_node(&tick_state).await;
                        propagation::probe_static_nodes_background(&tick_state).await;
                        check_message_timeouts(&tick_state).await;
                        sweep_undelivered_game_sessions(&tick_state).await;
                        let cleanup_now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64();
                        let cutoff = cleanup_now - 3600.0;
                        if let Ok(mut times) = tick_state.message_send_times.lock()
                            && times.len() > 200
                        {
                            times.retain(|_, &mut t| t > cutoff);
                        }
                        if let Ok(mut map) = tick_state.msg_id_map.lock()
                            && map.len() > 200
                        {
                            // No timestamps; hard cap only.
                            if map.len() > 1000 {
                                map.clear();
                            }
                        }
                        if let Ok(mut map) = tick_state.lrgp_msg_to_session.lock() {
                            map.retain(|_, meta| meta.sent_at > cutoff);
                        }
                    }
                }
            });

            // Auto-announce loop; wakes on timer or interval change.
            let periodic_state = state.clone();
            let periodic_shutdown = state
                .session_shutdown
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let mut announce_rx = state.announce_interval_rx.clone();
            tokio::spawn(async move {
                loop {
                    let interval_secs = *announce_rx.borrow();
                    if interval_secs == 0 {
                        tokio::select! {
                            _ = periodic_shutdown.wait() => break,
                            _ = announce_rx.changed() => continue,
                        }
                    } else {
                        tokio::select! {
                            _ = periodic_shutdown.wait() => break,
                            _ = announce_rx.changed() => continue,
                            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => {
                                send_announce_from_state(&periodic_state).await;
                            }
                        }
                    }
                }
            });

            let poll_state = state.clone();
            let poll_shutdown = state
                .session_shutdown
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            tokio::spawn(async move {
                poll_stats_loop(poll_state, poll_shutdown).await;
            });

            // Eager stats push after a short delay; lets transport ingest first batch.
            let eager_state = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                push_stats_once(&eager_state).await;
            });

            state.request_poll_now();

            // Per-aspect announce handlers; see `announce_handlers.rs`.
            {
                let shutdown = state
                    .session_shutdown
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                announce_handlers::spawn_lxmf_delivery_handler(
                    state.clone(),
                    transport_tx_for_handler.clone(),
                    shutdown.clone(),
                )
                .await;
                announce_handlers::spawn_lxmf_propagation_handler(
                    state.clone(),
                    transport_tx_for_handler.clone(),
                    shutdown.clone(),
                )
                .await;
                announce_handlers::spawn_lxst_telephony_handler(
                    state.clone(),
                    transport_tx_for_handler,
                    shutdown,
                )
                .await;
            }

            // Auto-mode startup kicker.
            {
                let (mode, favor_static) = propagation::read_settings(&state);
                if mode == propagation::PropagationMode::Auto {
                    if let Some(winner) = propagation::auto_select_node(&state) {
                        propagation::apply_auto_selection(&state, winner).await;
                    }
                    if favor_static && !static_nodes::load().is_empty() {
                        let _ = propagation::refresh_paths(&state, true).await;
                    }
                }
            }

            if let Some(rx) = inbound_rx {
                let inbound_state = state.clone();
                let inbound_shutdown = state
                    .session_shutdown
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                tokio::spawn(async move {
                    handle_inbound_lxmf(inbound_state, rx, inbound_shutdown).await;
                });
            }
        }
        Err(e) => {
            tracing::warn!("Failed to initialize RNS: {e}");
            tracing::warn!("Starting in degraded mode — network features unavailable");
        }
    }

    state.set_startup_stage("ready");
    state.emit_to_all("system_status", json!({"status": "ready"}));
    tracing::info!("Startup complete");
    schedule_startup_auto_announce(state.clone());

    // Schedule identity pruning after ready so it doesn't block cold-start.
    let prune_state = state.clone();
    let prune_shutdown = state
        .session_shutdown
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    identity_prune::spawn_scheduler(prune_state, prune_shutdown);
}

/// `None` until the first poll completes; callers should allow the attempt.
pub fn any_interface_online_cached(state: &AppState) -> Option<bool> {
    let guard = state.last_stats.read().ok()?;
    let stats = guard.as_ref()?;
    let arr = stats
        .get("interface_stats")?
        .get("interfaces")?
        .as_array()?;
    Some(
        arr.iter()
            .any(|i| i.get("online").and_then(|o| o.as_bool()).unwrap_or(false)),
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnnounceSendReport {
    pub packets: usize,
    pub queued: usize,
    pub failed: usize,
}

pub async fn send_announce_from_state(state: &AppState) -> AnnounceSendReport {
    send_announce_from_state_inner(state, true).await
}

pub async fn send_manual_announce_from_state(state: &AppState) -> AnnounceSendReport {
    send_announce_from_state_inner(state, false).await
}

pub async fn maybe_opportunistic_announce_before_user_send(
    state: &AppState,
    dest_hash: &str,
) -> AnnounceSendReport {
    let report = AnnounceSendReport::default();

    if *state.announce_interval_rx.borrow() == 0 {
        return report;
    }
    if hex::decode(dest_hash)
        .ok()
        .is_none_or(|bytes| bytes.len() != 16)
    {
        return report;
    }
    if !matches!(any_interface_online_cached(state), Some(true)) {
        return report;
    }

    let rns_ready = state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|_| ()))
        .is_some();
    if !rns_ready {
        return report;
    }
    let lxmf_ready = state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(|_| ()))
        .is_some();
    if !lxmf_ready {
        return report;
    }

    let hash_for_db = dest_hash.to_string();
    let first_seen = db::spawn_db(state.db.clone(), move |p| {
        db::get_identity_activity_first_seen(&p, &hash_for_db)
    })
    .await
    .unwrap_or(None);
    let Some(peer_first_seen_ms) = first_seen.and_then(unix_secs_to_ms) else {
        return report;
    };

    let last_announce_ms = state
        .last_lxmf_delivery_announce_at_ms
        .load(Ordering::Relaxed);
    if last_announce_ms >= peer_first_seen_ms {
        return report;
    }

    if !claim_opportunistic_announce(state, dest_hash) {
        return report;
    }
    let announce_report = send_announce_from_state(state).await;
    release_opportunistic_announce(state, dest_hash);
    announce_report
}

fn claim_opportunistic_announce(state: &AppState, dest_hash: &str) -> bool {
    let now = Instant::now();
    let mut last = match state.last_opportunistic_announce_at.lock() {
        Ok(last) => last,
        Err(_) => return false,
    };
    if last
        .as_ref()
        .is_some_and(|instant| now.duration_since(*instant) < OPPORTUNISTIC_ANNOUNCE_COOLDOWN)
    {
        return false;
    }
    let mut inflight = match state.opportunistic_announce_inflight.lock() {
        Ok(inflight) => inflight,
        Err(_) => return false,
    };
    if !inflight.insert(dest_hash.to_string()) {
        return false;
    }
    *last = Some(now);
    true
}

fn release_opportunistic_announce(state: &AppState, dest_hash: &str) {
    if let Ok(mut inflight) = state.opportunistic_announce_inflight.lock() {
        inflight.remove(dest_hash);
    }
}

fn schedule_startup_auto_announce(state: Arc<AppState>) {
    if *state.announce_interval_rx.borrow() == 0 {
        return;
    }

    let shutdown = state
        .session_shutdown
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    tokio::spawn(async move {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        state.poll_now.notify_one();

        loop {
            tokio::select! {
                _ = shutdown.wait() => return,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }

            if *state.announce_interval_rx.borrow() == 0 {
                return;
            }

            if matches!(any_interface_online_cached(&state), Some(true)) {
                let report = send_announce_from_state(&state).await;
                if report.queued > 0 {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    state.add_event(json!({
                        "timestamp": ts,
                        "category": "system",
                        "message": "Startup auto-announce queued",
                    }));
                    tracing::info!(
                        packets = report.packets,
                        queued = report.queued,
                        failed = report.failed,
                        "startup auto-announce queued"
                    );
                }
                return;
            }

            if std::time::Instant::now() >= deadline {
                tracing::debug!("startup auto-announce skipped: no online interface observed");
                return;
            }

            state.poll_now.notify_one();
        }
    });
}

async fn send_announce_from_state_inner(
    state: &AppState,
    require_cached_online: bool,
) -> AnnounceSendReport {
    let mut report = AnnounceSendReport::default();
    if require_cached_online && matches!(any_interface_online_cached(state), Some(false)) {
        tracing::warn!("announce skipped: no interfaces online");
        return report;
    }
    let (packets, transport_tx) = {
        let mut packets: Vec<([u8; 16], Vec<u8>, &'static str, bool)> = Vec::new();
        let lock_wait_started = std::time::Instant::now();
        if let Ok(mut lxmf) = state.lxmf.lock()
            && let Some(mgr) = lxmf.as_mut()
        {
            let waited = lock_wait_started.elapsed();
            if waited > Duration::from_secs(1) {
                tracing::warn!(
                    waited_ms = waited.as_millis() as u64,
                    "announce waited on lxmf manager lock"
                );
            }
            if let Ok(raw) = mgr.create_announce_packet() {
                packets.push((
                    mgr.lxmf_dest_hash,
                    raw,
                    "Identity announced on all interfaces",
                    true,
                ));
            }
            if state
                .propagation_node_hosting_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
                && let Ok(raw) = mgr.create_propagation_announce_packet()
            {
                packets.push((
                    mgr.propagation_dest_hash,
                    raw,
                    "Propagation node announced on all interfaces",
                    false,
                ));
            }
        }
        let tx = state
            .rns
            .read()
            .ok()
            .and_then(|r| r.as_ref().map(|mgr| mgr.handle.transport_tx.clone()));
        (packets, tx)
    };
    report.packets = packets.len();

    let Some(tx) = transport_tx else {
        return report;
    };

    for (destination_hash, raw, log_message, is_lxmf_delivery) in packets {
        match tx
            .send(rns_transport::messages::TransportMessage::Outbound(
                rns_transport::messages::OutboundRequest {
                    raw: Bytes::from(raw),
                    destination_hash,
                },
            ))
            .await
        {
            Ok(_) => {
                report.queued += 1;
                if is_lxmf_delivery {
                    state
                        .last_lxmf_delivery_announce_at_ms
                        .store(unix_now_ms(), Ordering::Relaxed);
                }
                tracing::info!(dest = %hex::encode(destination_hash), "announce sent");
                if state
                    .network_log_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    state.emit_network_event("announce", log_message, "", "detailed");
                }
            }
            Err(e) => {
                report.failed += 1;
                tracing::warn!("Failed to send announce: {e}");
                if state
                    .network_log_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    state.emit_network_event(
                        "error",
                        &format!("Announce failed: {e}"),
                        "",
                        "essential",
                    );
                }
            }
        }
    }

    #[cfg(feature = "lxst-voice")]
    match voice::announce_if_running(state).await {
        Ok(true) => {
            report.packets += 1;
            report.queued += 1;
            tracing::info!("LXST telephony announce queued");
            if state
                .network_log_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                state.emit_network_event(
                    "announce",
                    "LXST telephony announced on all interfaces",
                    "",
                    "detailed",
                );
            }
        }
        Ok(false) => {
            tracing::debug!("LXST telephony announce skipped: voice service is not running");
        }
        Err(e) => {
            report.packets += 1;
            report.failed += 1;
            tracing::warn!("Failed to queue LXST telephony announce: {e}");
            if state
                .network_log_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                state.emit_network_event(
                    "error",
                    &format!("LXST telephony announce failed: {e}"),
                    "",
                    "essential",
                );
            }
        }
    }

    report
}

// FIELD_FILE_ATTACHMENTS 0x05 = msgpack `[[filename, bytes], …]`.
// FIELD_IMAGE            0x06 = msgpack `[format, bytes]` (`png`, `webp`, ...).
struct ExtractedAttachment {
    file_name: String,
    stored_name: String,
    is_image: bool,
}

fn extract_and_save_attachment(
    state: &AppState,
    msg: &lxmf_core::message::LxMessage,
) -> Option<ExtractedAttachment> {
    if let Some(field_bytes) = msg.get_field(lxmf_core::constants::FIELD_FILE_ATTACHMENTS) {
        let mut cursor = std::io::Cursor::new(field_bytes);
        if let Ok(rmpv::Value::Array(attachments)) = rmpv::decode::read_value(&mut cursor)
            && let Some(rmpv::Value::Array(pair)) = attachments.first()
            && pair.len() >= 2
        {
            let file_name = match &pair[0] {
                rmpv::Value::Binary(b) => String::from_utf8_lossy(b).to_string(),
                rmpv::Value::String(s) => s.as_str().unwrap_or("attachment").to_string(),
                _ => "attachment".to_string(),
            };
            let file_data = match &pair[1] {
                rmpv::Value::Binary(b) => b.as_slice(),
                _ => return None,
            };
            if let Ok(mut lxmf) = state.lxmf.lock()
                && let Some(mgr) = lxmf.as_mut()
            {
                let stored = mgr.save_attachment(&file_name, file_data);
                tracing::info!(
                    file_name = %file_name,
                    stored = %stored,
                    size = file_data.len(),
                    "extracted inbound file attachment from FIELD_FILE_ATTACHMENTS"
                );
                return Some(ExtractedAttachment {
                    file_name,
                    stored_name: stored,
                    is_image: false,
                });
            }
        }
    }

    if let Some(field_bytes) = msg.get_field(lxmf_core::constants::FIELD_IMAGE) {
        let mut cursor = std::io::Cursor::new(field_bytes);
        if let Ok(rmpv::Value::Array(pair)) = rmpv::decode::read_value(&mut cursor)
            && pair.len() >= 2
        {
            let mime_type = match &pair[0] {
                rmpv::Value::Binary(b) => String::from_utf8_lossy(b).to_string(),
                rmpv::Value::String(s) => s.as_str().unwrap_or("image/png").to_string(),
                _ => "image/png".to_string(),
            };
            let image_data = match &pair[1] {
                rmpv::Value::Binary(b) => b.as_slice(),
                _ => return None,
            };
            let ext = mime_type.rsplit('/').next().unwrap_or("png");
            let file_name = format!("image.{ext}");
            if let Ok(mut lxmf) = state.lxmf.lock()
                && let Some(mgr) = lxmf.as_mut()
            {
                let stored = mgr.save_attachment(&file_name, image_data);
                tracing::info!(
                    mime_type = %mime_type,
                    stored = %stored,
                    size = image_data.len(),
                    "extracted inbound image from FIELD_IMAGE"
                );
                return Some(ExtractedAttachment {
                    file_name,
                    stored_name: stored,
                    is_image: true,
                });
            }
        }
    }

    None
}

fn decode_lxmf_ticket_field(ticket_data: &[u8]) -> Option<([u8; 16], f64)> {
    let value = rmpv::decode::read_value(&mut &ticket_data[..]).ok()?;
    let arr = value.as_array()?;
    if arr.len() < 2 {
        return None;
    }

    let parse_token = |value: &rmpv::Value| -> Option<[u8; 16]> {
        let bytes = value.as_slice()?;
        if bytes.len() != 16 {
            return None;
        }
        let mut token = [0u8; 16];
        token.copy_from_slice(bytes);
        Some(token)
    };

    // Python emits `[expires_f64, token:16]`; older Ratspeak builds emitted
    // `[token:16, expires_f64]`. Accept both on inbound.
    if let (Some(expires), Some(token)) = (arr[0].as_f64(), parse_token(&arr[1])) {
        return Some((token, expires));
    }
    if let (Some(token), Some(expires)) = (parse_token(&arr[0]), arr[1].as_f64()) {
        return Some((token, expires));
    }
    None
}

fn clamp_chat_field(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Inbound reactions are peer-controlled and rendered in the UI. Reject
/// markup-dangerous and control characters outright instead of trusting
/// every render site to escape (the renderer escapes too — defense in depth).
fn sanitize_reaction_emoji(value: &str) -> Option<String> {
    let emoji = clamp_chat_field(value, 16);
    if emoji.is_empty()
        || emoji
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '&' | '"' | '\''))
    {
        return None;
    }
    Some(emoji)
}

fn inbound_reply_fields(ext: Option<&lxmf::RatspeakChatExtension>) -> (String, String) {
    match ext {
        Some(lxmf::RatspeakChatExtension::Reply {
            target, preview, ..
        }) => (
            clamp_chat_field(target, 128),
            clamp_chat_field(preview, 200),
        ),
        _ => (String::new(), String::new()),
    }
}

async fn apply_inbound_ratspeak_reaction(
    state: &AppState,
    source_hash: &str,
    identity_id: &str,
    target: &str,
    emoji: &str,
    action: &str,
) {
    let target = clamp_chat_field(target, 128);
    let Some(emoji) = sanitize_reaction_emoji(emoji) else {
        return;
    };
    if target.is_empty() {
        return;
    }
    let action = if action == "remove" {
        "remove".to_string()
    } else {
        "add".to_string()
    };
    let sender = source_hash.to_string();
    let identity_id = identity_id.to_string();
    let target_for_db = target.clone();
    let emoji_for_db = emoji.clone();
    let reactions = db::spawn_db(state.db.clone(), move |p| {
        if action == "remove" {
            db::remove_reaction(&p, &target_for_db, &sender, &emoji_for_db, &identity_id);
        } else {
            db::save_reaction(&p, &target_for_db, &sender, &emoji_for_db, &identity_id);
        }
        db::get_reactions_for_message(&p, &target_for_db, &identity_id)
    })
    .await
    .unwrap_or_default();

    state.emit_to_all(
        "reaction_update",
        json!({
            "message_id": target,
            "reactions": reactions,
        }),
    );
}

/// Handle inbound LXMF messages delivered by the transport actor.
/// Emit a path-response announce for our LXMF delivery destination on the
/// interface a path request arrived on. The transport delegates this to us
/// because it doesn't hold our identity keys; answering is what lets a peer
/// that never announced learn our identity + path on first contact.
async fn answer_lxmf_path_request(state: &Arc<AppState>, attached_interface: Option<u64>) {
    // Build under the lxmf lock (sync), then drop it before the async send.
    let built =
        match state.lxmf.lock() {
            Ok(mut guard) => guard.as_mut().and_then(|mgr| {
                match mgr.create_path_response_announce_packet() {
                    Ok(raw) => Some((raw, mgr.lxmf_dest_hash)),
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to build LXMF path-response announce");
                        None
                    }
                }
            }),
            Err(_) => None,
        };
    let Some((raw, dest_hash)) = built else {
        return;
    };

    let Some(tx) = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.transport_tx.clone()))
    else {
        return;
    };

    let request = rns_transport::messages::OutboundRequest {
        raw: Bytes::from(raw),
        destination_hash: dest_hash,
    };
    let message = match attached_interface {
        Some(interface_id) => rns_transport::messages::TransportMessage::OutboundAttached {
            request,
            interface_id,
        },
        None => rns_transport::messages::TransportMessage::Outbound(request),
    };
    if let Err(e) = tx.send(message).await {
        tracing::warn!(error = %e, "failed to queue LXMF path-response announce");
    } else {
        tracing::debug!(
            attached_interface = ?attached_interface,
            "answered LXMF path request with path-response announce"
        );
    }
}

async fn handle_inbound_lxmf(
    state: Arc<AppState>,
    mut rx: tokio::sync::mpsc::Receiver<rns_transport::link_messages::DestinationEvent>,
    shutdown: rns_runtime::lifecycle::ShutdownSignal,
) {
    use rns_transport::link_messages::DestinationEvent;

    loop {
        let event = tokio::select! {
            _ = shutdown.wait() => break,
            ev = rx.recv() => match ev {
                Some(e) => e,
                None => break,
            },
        };
        if let DestinationEvent::DeliveryProof {
            ref msg_id,
            ref rtt,
        } = event
        {
            let rtt_ms = rtt.map(|d| d.as_secs_f64() * 1000.0);
            let msg_id_for_db = msg_id.clone();
            let identity_for_db = helpers::active_identity_id(&state);
            // One hop: flip the state and read the method back for the emit.
            let method = db::spawn_db(state.db.clone(), move |p| {
                db::update_message_state(&p, &msg_id_for_db, &identity_for_db, "delivered", rtt_ms);
                db::get_message_delivery_method(&p, &msg_id_for_db, &identity_for_db)
            })
            .await
            .expect("db task panicked");
            if let Ok(mut times) = state.message_send_times.lock() {
                times.remove(msg_id);
            }
            let client_msg_id = state
                .msg_id_map
                .lock()
                .ok()
                .and_then(|mut map| map.remove(msg_id));
            state.emit_to_all(
                "lxmf_step",
                json!({
                    "step": "delivered",
                    "msg_id": msg_id,
                    "client_msg_id": client_msg_id,
                    "rtt_ms": rtt_ms,
                    "method": method,
                }),
            );
            tracing::info!(msg_id = %msg_id, rtt_ms = ?rtt_ms, "message delivery confirmed");
            if state
                .network_log_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                let rtt_label = rtt_ms
                    .map(|r| format!(" ({:.0}ms RTT)", r))
                    .unwrap_or_default();
                state.emit_network_event(
                    "message",
                    &format!("Message delivered{}", rtt_label),
                    msg_id,
                    "standard",
                );
            }
            continue;
        }

        // A path request arrived for our LXMF destination. The transport can't
        // answer it itself (it doesn't hold our keys), so it asks us to. Reply
        // with a path-response announce carrying our identity + path, so a peer
        // that has never announced can still reach us on first contact.
        // Previously this event fell through to `_ => continue` and was
        // dropped, so we never answered path requests and replies to us stalled
        // until we announced.
        if let DestinationEvent::AnnounceRequested(ref req) = event {
            if req.path_response {
                answer_lxmf_path_request(&state, req.attached_interface).await;
            }
            continue;
        }

        let raw = match event {
            DestinationEvent::InboundPacket { raw, .. } => raw,
            _ => continue,
        };

        let (header, data_offset) = match rns_wire::header::PacketHeader::unpack(&raw) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("Inbound packet header parse failed: {e}");
                continue;
            }
        };
        let lxmf_payload = &raw[data_offset..];
        let dest_hash = header.destination_hash;

        tracing::info!(
            payload_len = lxmf_payload.len(),
            dest = %hex::encode(dest_hash),
            "attempting LXMF decrypt"
        );
        let decrypted = state
            .lxmf
            .lock()
            .ok()
            .and_then(|l| l.as_ref().and_then(|mgr| mgr.decrypt_inbound(lxmf_payload)));
        tracing::info!(
            decrypted = decrypted.is_some(),
            decrypted_len = decrypted.as_ref().map(|d| d.len()),
            "LXMF decrypt result"
        );

        // Opportunistic LXMF omits dest_hash from the body (it's in the
        // RNS header). unpack() needs [dest_hash:16][src_hash:16][sig:64][msgpack];
        // re-prepend it here. Falls back to the plaintext-broadcast layout
        // when decryption didn't apply.
        let body: &[u8] = decrypted.as_deref().unwrap_or(lxmf_payload);
        let mut lxmf_data = Vec::with_capacity(16 + body.len());
        lxmf_data.extend_from_slice(&dest_hash);
        lxmf_data.extend_from_slice(body);
        let msg = match lxmf_core::message::LxMessage::unpack(&lxmf_data) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    decrypted = decrypted.is_some(),
                    "inbound LXMF unpack failed — dropping"
                );
                continue;
            }
        };

        process_inbound_lxmf(
            &state,
            msg,
            &lxmf_data,
            InboundLxmfSource::Opportunistic { raw },
        )
        .await;
    }

    tracing::warn!("Inbound LXMF handler channel closed");
}

/// Where an inbound LXMF message entered. Source only drives the
/// source-specific steps (delivery proof, backchannel note, last-heard
/// touch, log labels); everything else is the shared pipeline.
enum InboundLxmfSource {
    /// Opportunistic single-packet delivery; `raw` is the RNS packet the
    /// delivery proof is derived from.
    Opportunistic { raw: Bytes },
    /// Link-delivered (direct); the link is noted for backchannel reuse.
    Link { link_id: Option<[u8; 16]> },
    /// Downloaded from a propagation node.
    Propagated,
}

impl InboundLxmfSource {
    fn label(&self) -> &'static str {
        match self {
            InboundLxmfSource::Opportunistic { .. } => "opportunistic",
            InboundLxmfSource::Link { .. } => "link",
            InboundLxmfSource::Propagated => "propagated",
        }
    }

    /// Propagated messages say nothing about the sender being reachable now.
    fn marks_sender_seen(&self) -> bool {
        !matches!(self, InboundLxmfSource::Propagated)
    }
}

/// Stamp PoW gate (T1-9): applies to every inbound source. Runs after
/// signature validation and before the delivery-proof ACK; ticket-store
/// entries bypass via `validate_stamp_with_tickets`.
fn inbound_stamp_allowed(state: &AppState, msg: &lxmf_core::message::LxMessage) -> bool {
    if !state
        .enforce_stamps
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return true;
    }
    let required_cost = state
        .required_stamp_cost
        .load(std::sync::atomic::Ordering::Relaxed);
    if required_cost == 0 {
        return true;
    }
    let stamp_ok = match (msg.stamp.as_deref(), msg.message_id.or(msg.hash)) {
        (Some(stamp), Some(message_id)) => state
            .lxmf
            .lock()
            .ok()
            .and_then(|l| {
                l.as_ref().map(|mgr| {
                    mgr.router.validate_stamp_with_tickets(
                        &message_id,
                        stamp,
                        required_cost,
                        &msg.source_hash,
                    )
                })
            })
            .unwrap_or(false),
        _ => false,
    };
    if !stamp_ok {
        tracing::warn!(
            from = %hex::encode(msg.source_hash),
            required_cost,
            has_stamp = msg.stamp.is_some(),
            "inbound message REJECTED: stamp missing or PoW invalid (enforce_stamps=true)"
        );
    }
    stamp_ok
}

/// Pre-decrypted inbound entry: `data` = [dest:16][src:16][sig:64][msgpack].
async fn handle_decrypted_lxmf(state: &Arc<AppState>, data: Vec<u8>, source: InboundLxmfSource) {
    let msg = match lxmf_core::message::LxMessage::unpack(&data) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                error = %e,
                data_len = data.len(),
                source = source.label(),
                "inbound LXMF unpack failed"
            );
            return;
        }
    };
    process_inbound_lxmf(state, msg, &data, source).await;
}

/// The one inbound LXMF pipeline. `fallback_id_material` is the unpacked
/// wire material; its hash is the msg-id fallback when the message carries
/// no hash — deterministic across sender retries so dedupe still works
/// (the old paths used the ciphertext hash / a fresh uuid4, both of which
/// made every retry look new).
async fn process_inbound_lxmf(
    state: &Arc<AppState>,
    mut msg: lxmf_core::message::LxMessage,
    fallback_id_material: &[u8],
    source: InboundLxmfSource,
) {
    let source_hash = hex::encode(msg.source_hash);
    let dest_hash = hex::encode(msg.destination_hash);

    tracing::info!(
        from = %source_hash,
        title = %msg.title,
        len = msg.content.len(),
        source = source.label(),
        "inbound LXMF message received"
    );
    if state
        .network_log_enabled
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        state.emit_network_event(
            "message",
            &format!(
                "Message received from {} ({})",
                &source_hash[..8.min(source_hash.len())],
                source.label()
            ),
            &source_hash,
            "standard",
        );
    }

    let sig_valid = state.lxmf.lock().ok().and_then(|mut l| {
        l.as_mut()
            .and_then(|mgr| mgr.verify_inbound_signature(&mut msg))
    });
    match sig_valid {
        Some(true) => tracing::debug!("inbound signature validated"),
        Some(false) => {
            tracing::warn!("inbound signature INVALID — dropping message");
            return;
        }
        None => tracing::debug!("sender unknown — signature not validated"),
    }

    if !inbound_stamp_allowed(state, &msg) {
        return;
    }

    // FIELD_TICKET 0x0C: `[expires_f64, token:16]` for stamp bypass.
    if let Some(ticket_data) = msg.fields.get(&lxmf_core::constants::FIELD_TICKET)
        && let Some((token, expires)) = decode_lxmf_ticket_field(ticket_data)
        && let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        mgr.router.ticket_store.add(lxmf_core::ticket::Ticket::new(
            token,
            msg.source_hash,
            expires,
        ));
        tracing::debug!(
            from = %hex::encode(msg.source_hash),
            "stored inbound ticket for future stamp bypass"
        );
    }

    // Opportunistic ACK; runs before the blocked check on purpose so a
    // blocked sender doesn't learn anything from a missing proof.
    if let InboundLxmfSource::Opportunistic { ref raw } = source {
        let proof_and_tx = state.lxmf.lock().ok().and_then(|l| {
            let mgr = l.as_ref()?;
            let proof = mgr.create_delivery_proof(raw)?;
            let tx = mgr.router.transport_tx.clone()?;
            Some((proof, tx))
        });
        if let Some((proof_raw, tx)) = proof_and_tx
            && let Ok((proof_hdr, _)) = rns_wire::header::PacketHeader::unpack(&proof_raw)
        {
            let _ = tx.try_send(rns_transport::messages::TransportMessage::Outbound(
                rns_transport::messages::OutboundRequest {
                    raw: Bytes::from(proof_raw),
                    destination_hash: proof_hdr.destination_hash,
                },
            ));
            tracing::debug!("sent delivery proof for inbound message");
        }
    }

    // Active identity comes from the running LXMF manager for every source
    // (the old opportunistic path re-read the DB; the manager IS the active
    // identity and inbound traffic only exists while it runs).
    let (identity_id, lxmf_id) = state
        .lxmf
        .lock()
        .ok()
        .and_then(|l| {
            l.as_ref()
                .map(|m| (m.identity_hash.clone(), m.lxmf_hash.clone()))
        })
        .unwrap_or_default();
    if identity_id.is_empty() {
        tracing::warn!("No active LXMF identity — dropping inbound message");
        return;
    }

    let msg_id = msg
        .hash
        .map(hex::encode)
        .unwrap_or_else(|| hex::encode(rns_crypto::sha::sha256(fallback_id_material)));

    // Senders retry on missing proofs; duplicates are scoped to the local
    // identity so two Ratspeak identities can hold the same LXMF hash.
    let msg_id_for_exists = msg_id.clone();
    let identity_id_for_exists = identity_id.clone();
    let already_exists = db::spawn_db(state.db.clone(), move |p| {
        db::message_exists_for_identity(&p, &msg_id_for_exists, &identity_id_for_exists)
    })
    .await
    .expect("db task panicked");
    if already_exists {
        tracing::debug!(msg_id = %msg_id, identity_id = %identity_id, "inbound LXMF duplicate — skipping");
        return;
    }

    // Blocked senders silently discarded; any source-level ACK already
    // happened so we don't leak a "missing proof" signal.
    let source_hash_for_blocked = source_hash.clone();
    let identity_id_for_blocked = identity_id.clone();
    let blocked = db::spawn_db(state.db.clone(), move |p| {
        db::is_blocked(&p, &source_hash_for_blocked, &identity_id_for_blocked)
    })
    .await
    .expect("db task panicked");
    if blocked {
        tracing::debug!(from = %source_hash, "inbound message from blocked user — discarding");
        return;
    }

    if let InboundLxmfSource::Link {
        link_id: Some(link_id),
    } = source
    {
        let local_destination_matches = hex::decode(&lxmf_id)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .is_some_and(|local_dest: [u8; 16]| local_dest == msg.destination_hash);
        if local_destination_matches
            && let Ok(mut lxmf) = state.lxmf.lock()
            && let Some(mgr) = lxmf.as_mut()
        {
            mgr.note_pending_direct_backchannel(msg.source_hash, link_id);
            tracing::debug!(
                from = %source_hash,
                link_id = %hex::encode(link_id),
                "Direct LXMF payload received; waiting for LINKIDENTIFY before backchannel reuse"
            );
        }
    }

    if source.marks_sender_seen() {
        touch_peer_last_heard(state, &source_hash).await;
    }

    let chat_extension = lxmf::decode_ratspeak_chat_extension(&msg);
    if let Some(lxmf::RatspeakChatExtension::Reaction {
        target,
        emoji,
        action,
    }) = chat_extension.as_ref()
    {
        apply_inbound_ratspeak_reaction(state, &source_hash, &identity_id, target, emoji, action)
            .await;
        return;
    }

    // LRGP tunnels over LXMF; don't surface in conversation UI.
    if !matches!(
        chat_extension,
        Some(lxmf::RatspeakChatExtension::Reply { .. })
    ) && try_handle_inbound_lrgp(state, &msg, &source_hash, &lxmf_id).await
    {
        return;
    }

    let received_at = next_chat_observed_timestamp(state, &source_hash, &identity_id).await;
    let attachment_file = extract_and_save_attachment(state, &msg);
    let (reply_to_id, reply_to_preview) = inbound_reply_fields(chat_extension.as_ref());
    {
        let msg_id_for_save = msg_id.clone();
        let source_hash_for_save = source_hash.clone();
        let dest_hash_for_save = dest_hash.clone();
        let content_for_save = msg.content.clone();
        let title_for_save = msg.title.clone();
        let timestamp_for_save = received_at;
        let identity_id_for_save = identity_id.clone();
        let reply_to_id_for_save = reply_to_id.clone();
        let reply_to_preview_for_save = reply_to_preview.clone();
        let (att_name, att_stored, img_name, img_stored) = match attachment_file.as_ref() {
            Some(a) if a.is_image => (
                String::new(),
                String::new(),
                a.file_name.clone(),
                a.stored_name.clone(),
            ),
            Some(a) => (
                a.file_name.clone(),
                a.stored_name.clone(),
                String::new(),
                String::new(),
            ),
            None => (String::new(), String::new(), String::new(), String::new()),
        };
        db::spawn_db(state.db.clone(), move |p| {
            db::save_message(
                &p,
                &msg_id_for_save,
                &source_hash_for_save,
                &dest_hash_for_save,
                &content_for_save,
                &title_for_save,
                timestamp_for_save,
                "received",
                "inbound",
                &identity_id_for_save,
                &att_name,
                &att_stored,
                &img_name,
                &img_stored,
                &reply_to_id_for_save,
                &reply_to_preview_for_save,
                None,
            );
        })
        .await
        .expect("db task panicked");
    }
    {
        // Inbound message un-hides the conversation.
        let source_hash_for_unhide = source_hash.clone();
        let identity_id_for_unhide = identity_id.clone();
        db::spawn_db(state.db.clone(), move |p| {
            db::unhide_conversation(&p, &source_hash_for_unhide, &identity_id_for_unhide);
        })
        .await
        .expect("db task panicked");
    }
    notify_inbound_message_if_background(
        state,
        &source_hash,
        &identity_id,
        &msg.content,
        attachment_file.is_some(),
    )
    .await;

    let source_display_name = contact_label_from_db(&state.db, &source_hash, &identity_id);

    // Frontend expects nested `image` / `attachments` matching history rows.
    let mut event_data = json!({
        "id": msg_id,
        "source": source_hash,
        "source_display_name": source_display_name,
        "destination": dest_hash,
        "content": msg.content,
        "title": msg.title,
        "timestamp": received_at,
        "state": "received",
        "direction": "inbound",
        "reply_to_id": reply_to_id,
        "reply_to_preview": reply_to_preview,
    });
    if let Some(ref att) = attachment_file {
        let obj = event_data.as_object_mut().unwrap();
        if att.is_image {
            obj.insert(
                "image".to_string(),
                json!({ "stored_name": att.stored_name, "filename": att.file_name }),
            );
        } else {
            obj.insert(
                "attachments".to_string(),
                json!([{ "filename": att.file_name, "stored_name": att.stored_name }]),
            );
        }
    }
    state.emit_to_all("lxmf_message", event_data);
    messaging::broadcast_conversations(Arc::clone(state));

    // Post-emit UI refresh failures only mean stale sidebar counts.
    let identity_id_for_contacts = identity_id.clone();
    match db::spawn_db(state.db.clone(), move |p| {
        db::get_all_contacts(&p, &identity_id_for_contacts)
    })
    .await
    {
        Ok(contacts) => {
            let contacts_list: Vec<serde_json::Value> = contacts
                .into_iter()
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
                .collect();
            state.emit_to_all("contacts_update", contacts_list.into());
        }
        Err(e) => tracing::error!(error = %e, "contacts refresh after inbound message failed"),
    }

    let identity_id_for_counts = identity_id.clone();
    match db::spawn_db(state.db.clone(), move |p| {
        db::get_all_unread_counts(&p, &identity_id_for_counts)
    })
    .await
    {
        Ok(counts) => {
            let total: i64 = counts.values().sum();
            state.emit_to_all("unread_total", json!({"count": total}));
        }
        Err(e) => {
            tracing::error!(error = %e, "unread-total refresh after inbound message failed")
        }
    }
}

// Single stats fetch + emit; used for eager post-init push.
async fn push_stats_once(state: &AppState) {
    let handle = {
        let rns = state.rns.read().ok();
        rns.as_ref()
            .and_then(|r| r.as_ref())
            .map(|mgr| mgr.handle.clone())
    };

    let Some(handle) = handle else {
        return;
    };
    let mode = handle.instance_mode;

    let (iface_result, path_result, link_result) = tokio::join!(
        handle.query_control(rns_transport::messages::TransportQuery::GetInterfaceStats),
        handle.query_control(rns_transport::messages::TransportQuery::GetPathTable),
        handle.query_control(rns_transport::messages::TransportQuery::GetLinkCount),
    );

    let iface_stats = match iface_result {
        Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(s)) => {
            let interfaces: Vec<serde_json::Value> = s
                .iter()
                .map(|e| {
                    json!({
                        "name": e.name, "rxb": e.rx_bytes, "txb": e.tx_bytes,
                        "online": e.online, "bitrate": e.bitrate, "mtu": e.mtu, "mode": e.mode,
                        "role": e.role,
                        "announce_queue": e.announce_queue,
                        "held_announces": e.held_announces,
                        "incoming_announce_frequency": e.incoming_announce_frequency,
                        "outgoing_announce_frequency": e.outgoing_announce_frequency,
                        "incoming_pr_frequency": e.incoming_pr_frequency,
                        "outgoing_pr_frequency": e.outgoing_pr_frequency,
                        "burst_active": e.burst_active,
                        "burst_activated": e.burst_activated,
                        "pr_burst_active": e.pr_burst_active,
                        "pr_burst_activated": e.pr_burst_activated,
                        "announce_rate_target": e.announce_rate_target,
                        "announce_rate_grace": e.announce_rate_grace,
                        "announce_rate_penalty": e.announce_rate_penalty,
                        "announce_cap": e.announce_cap,
                        "ifac_size": e.ifac_size,
                        "tx_drops": e.tx_drops,
                    })
                })
                .collect();
            json!({ "interfaces": interfaces })
        }
        _ => json!({ "interfaces": [] }),
    };

    let (path_table, path_index, path_table_total, path_table_truncated) = match path_result {
        Some(rns_transport::messages::TransportQueryResponse::PathTable(entries)) => {
            cache_lxmf_route_hops_from_path_table(state, &entries);
            crate::rns::path_table_stats_snapshot(entries)
        }
        _ => (
            vec![],
            serde_json::Value::Object(serde_json::Map::new()),
            0,
            false,
        ),
    };

    let link_count = match link_result {
        Some(rns_transport::messages::TransportQueryResponse::IntResult(n)) => n,
        _ => 0,
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let any_online = iface_stats
        .get("interfaces")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .any(|i| i.get("online").and_then(|o| o.as_bool()).unwrap_or(false))
        })
        .unwrap_or(false);

    let connected = any_online
        && (mode == rns_runtime::reticulum::InstanceMode::Client
            || mode == rns_runtime::reticulum::InstanceMode::Shared);

    let stats = json!({
        "timestamp": now,
        "connected": connected,
        "interface_stats": iface_stats,
        "path_table": path_table,
        "path_index": path_index,
        "path_table_total": path_table_total,
        "path_table_truncated": path_table_truncated,
        "rate_table": [],
        "link_count": link_count,
    });

    state.set_last_stats(stats.clone());
    state.emit_to_all("stats_update", stats);
}

fn cache_lxmf_route_hops_from_path_table(
    state: &AppState,
    entries: &[rns_transport::messages::PathTableRpcEntry],
) {
    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        mgr.replace_route_hops_from_path_table(entries);
    }
}

// Debounce for eager `poll_now` wakes.
const POLL_NOW_COOLDOWN: Duration = Duration::from_millis(750);

// Always emits, including backgrounded — first paint on resume.
async fn poll_stats_loop(state: Arc<AppState>, shutdown: rns_runtime::lifecycle::ShutdownSignal) {
    let mut interval = tokio::time::interval(Duration::from_millis(2500));

    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    state.add_event(json!({
        "timestamp": now_ts,
        "category": "system",
        "message": "Ratspeak dashboard started",
    }));
    state.emit_network_event("interface", "Ratspeak dashboard started", "", "essential");

    let mut prev_online: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut prev_ingress_burst: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    let mut prev_held_announces: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    let mut last_interface_announce = std::time::Instant::now();

    #[cfg(feature = "mobile-throttle")]
    let mut was_foreground = true;
    let mut last_poll_at = std::time::Instant::now()
        .checked_sub(POLL_NOW_COOLDOWN)
        .unwrap_or_else(std::time::Instant::now);

    loop {
        tokio::select! {
            _ = shutdown.wait() => break,
            _ = interval.tick() => {}
            _ = state.poll_now.notified() => {
                if last_poll_at.elapsed() < POLL_NOW_COOLDOWN {
                    continue;
                }
                interval.reset();
            }
        }

        // Mobile: drop to 15s while backgrounded.
        #[cfg(feature = "mobile-throttle")]
        {
            let is_fg = state.is_foreground();
            if is_fg != was_foreground {
                let new_period = if is_fg {
                    Duration::from_millis(2500)
                } else {
                    Duration::from_secs(15)
                };
                interval = tokio::time::interval(new_period);
                interval.tick().await;
                was_foreground = is_fg;
            }
        }

        let poll_generation = state
            .identity_session_generation
            .load(std::sync::atomic::Ordering::SeqCst);
        let handle = {
            let rns = state.rns.read().ok();
            rns.as_ref()
                .and_then(|r| r.as_ref())
                .map(|mgr| mgr.handle.clone())
        };

        let Some(handle) = handle else {
            continue;
        };
        let mode = handle.instance_mode;

        // Python-parity control surfaces proxy to the shared instance in
        // client mode; recent announces stay local dashboard state.
        let stats = {
            let (iface_result, path_result, link_result, announce_result) = tokio::join!(
                handle.query_control(rns_transport::messages::TransportQuery::GetInterfaceStats),
                handle.query_control(rns_transport::messages::TransportQuery::GetPathTable),
                handle.query_control(rns_transport::messages::TransportQuery::GetLinkCount),
                handle.query_transport(rns_transport::messages::TransportQuery::GetRecentAnnounces),
            );

            let iface_stats = match iface_result {
                Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(s)) => {
                    let interfaces: Vec<serde_json::Value> = s.iter().map(|e| {
                        json!({
                            "name": e.name, "rxb": e.rx_bytes, "txb": e.tx_bytes,
                            "online": e.online, "bitrate": e.bitrate, "mtu": e.mtu, "mode": e.mode,
                            "role": e.role,
                            "announce_queue": e.announce_queue,
                            "held_announces": e.held_announces,
                            "incoming_announce_frequency": e.incoming_announce_frequency,
                            "outgoing_announce_frequency": e.outgoing_announce_frequency,
                            "incoming_pr_frequency": e.incoming_pr_frequency,
                            "outgoing_pr_frequency": e.outgoing_pr_frequency,
                            "burst_active": e.burst_active,
                            "burst_activated": e.burst_activated,
                            "pr_burst_active": e.pr_burst_active,
                            "pr_burst_activated": e.pr_burst_activated,
                            "announce_rate_target": e.announce_rate_target,
                            "announce_rate_grace": e.announce_rate_grace,
                            "announce_rate_penalty": e.announce_rate_penalty,
                            "announce_cap": e.announce_cap,
                            "ifac_size": e.ifac_size,
                            "tx_drops": e.tx_drops,
                        })
                    }).collect();
                    json!({ "interfaces": interfaces })
                }
                _ => json!({ "interfaces": [] }),
            };

            if let Some(ifaces) = iface_stats.get("interfaces").and_then(|v| v.as_array()) {
                let ev_ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                for iface in ifaces {
                    let name = iface["name"].as_str().unwrap_or("unknown");
                    let online = iface["online"].as_bool().unwrap_or(false);
                    let burst_active = iface["burst_active"].as_bool().unwrap_or(false);
                    let held_announces = iface["held_announces"].as_u64().unwrap_or(0);
                    let key = name.to_string();
                    let prev = prev_online.get(&key).copied();
                    if prev != Some(online) {
                        let msg = if online {
                            format!("{} connected", name)
                        } else {
                            format!("{} disconnected", name)
                        };
                        state.add_event(json!({
                            "timestamp": ev_ts,
                            "category": "interface",
                            "message": msg,
                        }));
                        if state
                            .network_log_enabled
                            .load(std::sync::atomic::Ordering::Relaxed)
                        {
                            let net_level = if online { "standard" } else { "essential" };
                            state.emit_network_event("interface", &msg, name, net_level);
                        }
                        let reannounce_suppressed =
                            online && state.take_interface_reannounce_suppression(name);
                        if reannounce_suppressed {
                            last_interface_announce = Instant::now();
                            tracing::info!(
                                interface = %name,
                                "interface re-announce suppressed after config restart"
                            );
                            state.add_event(json!({
                                "timestamp": ev_ts,
                                "category": "system",
                                "message": format!("Skipped re-announce after {name} restarted"),
                            }));
                            if state
                                .network_log_enabled
                                .load(std::sync::atomic::Ordering::Relaxed)
                            {
                                state.emit_network_event(
                                    "announce",
                                    "Skipped re-announce after interface restarted",
                                    name,
                                    "detailed",
                                );
                            }
                        }
                        // Re-announce on interface up; gated by auto-announce + 30s cooldown.
                        let auto_announce_on = *state.announce_interval_rx.borrow() > 0;
                        if online
                            && !reannounce_suppressed
                            && auto_announce_on
                            && last_interface_announce.elapsed() >= Duration::from_secs(30)
                        {
                            last_interface_announce = std::time::Instant::now();
                            let announce_state = state.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                send_announce_from_state(&announce_state).await;
                                let ev_ts = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                announce_state.add_event(serde_json::json!({
                                    "timestamp": ev_ts,
                                    "category": "system",
                                    "message": "Re-announced after interface connected",
                                }));
                                if announce_state
                                    .network_log_enabled
                                    .load(std::sync::atomic::Ordering::Relaxed)
                                {
                                    announce_state.emit_network_event(
                                        "announce",
                                        "Re-announced after interface connected",
                                        "",
                                        "detailed",
                                    );
                                }
                            });
                        }
                    }
                    let prev_burst = prev_ingress_burst.get(&key).copied();
                    if let Some(was_bursting) = prev_burst
                        && was_bursting != burst_active
                        && state
                            .network_log_enabled
                            .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        let msg = if burst_active {
                            format!(
                                "{} ingress burst active; passive announces may be held",
                                name
                            )
                        } else {
                            format!("{} ingress burst cleared", name)
                        };
                        state.emit_network_event("announce", &msg, name, "standard");
                    }
                    let prev_held = prev_held_announces.get(&key).copied().unwrap_or(0);
                    if held_announces > 0
                        && prev_held == 0
                        && state
                            .network_log_enabled
                            .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        let msg = format!(
                            "{} holding {} passive announce{} during ingress burst",
                            name,
                            held_announces,
                            if held_announces == 1 { "" } else { "s" }
                        );
                        state.emit_network_event("announce", &msg, name, "standard");
                    }
                    prev_online.insert(key, online);
                    prev_ingress_burst.insert(name.to_string(), burst_active);
                    prev_held_announces.insert(name.to_string(), held_announces);
                }
            }

            let (path_table, path_index, path_table_total, path_table_truncated) = match path_result
            {
                Some(rns_transport::messages::TransportQueryResponse::PathTable(entries)) => {
                    cache_lxmf_route_hops_from_path_table(&state, &entries);
                    let path_activity_ready = state
                        .path_activity_baselined
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let hashes: std::collections::HashSet<String> =
                        entries.iter().map(|e| hex::encode(e.hash)).collect();

                    let newly_reachable: Vec<String> = if path_activity_ready {
                        if let Ok(cached) = state.known_path_hashes.lock() {
                            hashes
                                .iter()
                                .filter(|h| !cached.contains(*h))
                                .cloned()
                                .collect()
                        } else {
                            Vec::new()
                        }
                    } else {
                        Vec::new()
                    };

                    if let Ok(mut cached) = state.known_path_hashes.lock() {
                        *cached = hashes;
                    }
                    if !path_activity_ready && !entries.is_empty() {
                        state
                            .path_activity_baselined
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }

                    if !newly_reachable.is_empty()
                        && state
                            .network_log_enabled
                            .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        for dest in &newly_reachable {
                            state.emit_network_event(
                                "path",
                                &format!("Path discovered to {}...", &dest[..8.min(dest.len())]),
                                dest,
                                "standard",
                            );
                        }
                    }

                    // Auto-resend disabled: would flood on reconnect. Manual only.
                    let _ = &newly_reachable;

                    crate::rns::path_table_stats_snapshot(entries)
                }
                _ => (
                    vec![],
                    serde_json::Value::Object(serde_json::Map::new()),
                    0,
                    false,
                ),
            };

            let link_count = match link_result {
                Some(rns_transport::messages::TransportQueryResponse::IntResult(n)) => n,
                _ => 0,
            };

            if let Some(rns_transport::messages::TransportQueryResponse::Announces(announces)) =
                announce_result
            {
                let announce_activity_ready = state
                    .announce_activity_baselined
                    .load(std::sync::atomic::Ordering::Relaxed);
                let lxmf_delivery_name_hash =
                    rns_identity::name_hash::name_hash(db::PEER_SERVICE_LXMF_DELIVERY);
                let lxst_telephony_name_hash =
                    rns_identity::name_hash::name_hash(db::PEER_SERVICE_LXST_TELEPHONY);
                let mut peer_activity_updates: Vec<db::IdentityActivityUpdate> = Vec::new();
                let mut peer_activity_hashes: Vec<String> = Vec::new();
                let mut delivery_trigger_hashes: Vec<[u8; 16]> = Vec::new();
                // Aspect-agnostic: crypto cache, announce_history, contact-name refresh.
                if let Ok(mut lxmf) = state.lxmf.lock()
                    && let Some(mgr) = lxmf.as_mut()
                {
                    let mut identities_changed = false;
                    let mut router_changed = false;
                    let mut changed_ratchets: Vec<(
                        String,
                        rns_identity::ratchet::ReceivedRatchet,
                    )> = Vec::new();
                    for a in &announces {
                        let dest_hex = hex::encode(a.dest_hash);
                        tracing::debug!(
                            dest = %dest_hex,
                            has_pk = a.public_key.is_some(),
                            has_ratchet = a.ratchet.is_some(),
                            hops = a.hops,
                            "processing announce entry"
                        );
                        if let Some(ref pk) = a.public_key {
                            let is_new = !mgr.known_identities.contains_key(&dest_hex);
                            let (id_changed, ratchet_changed) =
                                mgr.update_remote_crypto(&dest_hex, pk, a.ratchet.as_ref());
                            identities_changed |= id_changed;
                            if ratchet_changed
                                && let Some(rr) = mgr.received_ratchets.get(&dest_hex)
                            {
                                changed_ratchets.push((dest_hex.clone(), *rr));
                            }
                            if is_new {
                                tracing::debug!(
                                    dest = %dest_hex,
                                    has_ratchet = a.ratchet.is_some(),
                                    "new remote identity cached from announce"
                                );
                                if announce_activity_ready
                                    && state
                                        .network_log_enabled
                                        .load(std::sync::atomic::Ordering::Relaxed)
                                {
                                    state.emit_network_event(
                                        "announce",
                                        &format!(
                                            "New identity discovered: {}...",
                                            &dest_hex[..8.min(dest_hex.len())]
                                        ),
                                        &dest_hex,
                                        "standard",
                                    );
                                }
                            }
                        }
                        router_changed |= mgr.update_lxmf_announce_app_data(
                            a.dest_hash,
                            a.name_hash,
                            a.app_data.as_deref(),
                        );
                    }
                    // Persist only announce-derived deltas, off the poll
                    // loop; the ring and full rewrites stay on the
                    // rotation/periodic/shutdown saves. Stamp costs persist
                    // per batch like Python's delivery announce handler.
                    if router_changed {
                        mgr.save_router_state();
                    }
                    if identities_changed || !changed_ratchets.is_empty() {
                        let ratchet_dir = mgr.ratchets_dir();
                        let ki_blob = identities_changed.then(|| mgr.known_identities_blob());
                        tracing::debug!(
                            known_identities = mgr.known_identities.len(),
                            changed_ratchets = changed_ratchets.len(),
                            router_state_changed = router_changed,
                            "announce-derived crypto state persisted"
                        );
                        tokio::task::spawn_blocking(move || {
                            let received_dir = ratchet_dir.join("received");
                            std::fs::create_dir_all(&received_dir).ok();
                            for (hash_hex, rr) in &changed_ratchets {
                                let path = received_dir.join(format!("{hash_hex}.ratchet"));
                                if let Err(e) = rr.save(&path) {
                                    tracing::warn!("Failed to persist received ratchet: {e}");
                                }
                            }
                            if let Some(blob) = ki_blob {
                                let ki_path = ratchet_dir.join("known_identities");
                                if let Err(e) =
                                    rns_identity::persistence::atomic_write(&ki_path, &blob)
                                {
                                    tracing::warn!("Failed to save known identities: {e}");
                                }
                            }
                        });
                    }
                }

                if let Ok(mut history) = state.announce_history.write() {
                    let current_announce_hashes: std::collections::HashSet<String> =
                        announces.iter().map(|a| hex::encode(a.dest_hash)).collect();
                    if let Ok(mut seen) = state.seen_announce_hashes.lock()
                        && seen.len() > 50_000
                    {
                        if current_announce_hashes.is_empty() {
                            seen.clear();
                        } else {
                            seen.retain(|hash| current_announce_hashes.contains(hash));
                        }
                    }
                    for a in &announces {
                        let hash_hex = hex::encode(a.dest_hash);
                        let display_name = a
                            .app_data
                            .as_ref()
                            .map(|d| extract_display_name(d))
                            .unwrap_or_default();
                        let status = a
                            .app_data
                            .as_deref()
                            .and_then(crate::lxmf::ratspeak_status_from_app_data);
                        let previous_timestamp = history
                            .get(&hash_hex)
                            .and_then(|existing| existing.get("timestamp"))
                            .and_then(|ts| ts.as_f64());
                        let announce_timestamp_changed = previous_timestamp
                            .map(|prev| a.timestamp > prev + 0.001)
                            .unwrap_or(true);
                        let is_new = if let Ok(mut seen) = state.seen_announce_hashes.lock() {
                            seen.insert(hash_hex.clone())
                        } else {
                            false
                        };
                        if announce_timestamp_changed && !a.is_path_response {
                            if a.name_hash == lxmf_delivery_name_hash {
                                let mut services = vec![db::PEER_SERVICE_LXMF_DELIVERY.to_string()];
                                let lxmf_compression_support = a
                                    .app_data
                                    .as_deref()
                                    .and_then(
                                        crate::lxmf::lxmf_compression_support_db_value_from_app_data,
                                    )
                                    .map(str::to_string);
                                if let Some(app_data) = a.app_data.as_deref() {
                                    services.extend(
                                        crate::lxmf::ratspeak_capability_services_from_app_data(
                                            app_data,
                                        )
                                        .into_iter()
                                        .map(str::to_string),
                                    );
                                }
                                peer_activity_updates.push(db::IdentityActivityUpdate {
                                    dest_hash: hash_hex.clone(),
                                    timestamp: a.timestamp,
                                    display_name: if display_name.is_empty() {
                                        None
                                    } else {
                                        Some(display_name.clone())
                                    },
                                    status: status.clone(),
                                    last_interface: None,
                                    identity_hash: a
                                        .public_key
                                        .as_ref()
                                        .map(|pk| hex::encode(rns_crypto::sha::truncated_hash(pk))),
                                    services,
                                    clear_ratspeak_services: true,
                                    lxmf_compression_support,
                                });
                                peer_activity_hashes.push(hash_hex.clone());
                                delivery_trigger_hashes.push(a.dest_hash);
                            } else if a.name_hash == lxst_telephony_name_hash
                                && let Some(identity_hash) = a
                                    .public_key
                                    .as_ref()
                                    .map(|pk| rns_crypto::sha::truncated_hash(pk))
                            {
                                let lxmf_dest = Destination::hash_from_name_and_identity(
                                    db::PEER_SERVICE_LXMF_DELIVERY,
                                    Some(&identity_hash),
                                );
                                let lxmf_dest_hex = hex::encode(lxmf_dest);
                                peer_activity_updates.push(db::IdentityActivityUpdate {
                                    dest_hash: lxmf_dest_hex.clone(),
                                    timestamp: a.timestamp,
                                    display_name: None,
                                    status: None,
                                    last_interface: None,
                                    identity_hash: Some(hex::encode(identity_hash)),
                                    services: vec![db::PEER_SERVICE_LXST_TELEPHONY.to_string()],
                                    clear_ratspeak_services: false,
                                    lxmf_compression_support: None,
                                });
                                peer_activity_hashes.push(lxmf_dest_hex);
                            }
                        }
                        if let Some(existing) = history.get_mut(&hash_hex) {
                            if !display_name.is_empty() {
                                existing["display_name"] = json!(display_name);
                            }
                            if let Some(status) = status.clone() {
                                existing["status"] = json!(status);
                            }
                            existing["timestamp"] = json!(a.timestamp);
                            existing["hops"] = json!(a.hops);
                        } else {
                            if history.len() >= ANNOUNCE_HISTORY_CAP {
                                history.shift_remove_index(0);
                            }
                            history.insert(
                                hash_hex.clone(),
                                json!({
                                    "hash": hash_hex.clone(),
                                    "display_name": display_name.clone(),
                                    "status": status.clone().unwrap_or_default(),
                                    "timestamp": a.timestamp,
                                    "hops": a.hops,
                                }),
                            );
                        }
                        if announce_activity_ready && is_new {
                            state.emit_to_all(
                                "announce_received",
                                json!({
                                    "hash": hash_hex,
                                    "display_name": display_name,
                                    "status": status.unwrap_or_default(),
                                    "timestamp": a.timestamp,
                                    "hops": a.hops,
                                }),
                            );
                            let announce_label = if display_name.is_empty() {
                                if hash_hex.len() > 12 {
                                    format!(
                                        "{}::{}",
                                        &hash_hex[..6],
                                        &hash_hex[hash_hex.len() - 6..]
                                    )
                                } else {
                                    hash_hex.clone()
                                }
                            } else if display_name.chars().count() > 20 {
                                let truncated: String = display_name.chars().take(18).collect();
                                format!("{}..", truncated)
                            } else {
                                display_name.clone()
                            };
                            state.add_event(json!({
                                "timestamp": a.timestamp as u64,
                                "category": "announce_summary",
                                "message": format!("{} is {} hops away",
                                    announce_label, a.hops),
                            }));
                            if state
                                .network_log_enabled
                                .load(std::sync::atomic::Ordering::Relaxed)
                            {
                                state.emit_network_event(
                                    "announce",
                                    &format!("{} is {} hops away", announce_label, a.hops),
                                    &hash_hex,
                                    "detailed",
                                );
                            }
                        }
                    }
                    if !announce_activity_ready && !announces.is_empty() {
                        state
                            .announce_activity_baselined
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    // Belt-and-braces: a large batch could push us past the
                    // per-insert cap if we were already just below it.
                    while history.len() > ANNOUNCE_HISTORY_CAP {
                        history.shift_remove_index(0);
                    }
                }
                if !delivery_trigger_hashes.is_empty() {
                    delivery_trigger_hashes.sort();
                    delivery_trigger_hashes.dedup();
                    let triggered = if let Ok(mut lxmf) = state.lxmf.lock()
                        && let Some(mgr) = lxmf.as_mut()
                    {
                        delivery_trigger_hashes
                            .iter()
                            .map(|dest| mgr.router.trigger_outbound_for_delivery_announce(*dest))
                            .sum::<usize>()
                    } else {
                        0
                    };
                    if triggered > 0 {
                        state.lxmf_notify.notify_one();
                    }
                }
                if !peer_activity_updates.is_empty() {
                    peer_activity_hashes.sort();
                    peer_activity_hashes.dedup();
                    let pool = state.db.clone();
                    let identity_id = crate::helpers::active_identity_id(&state);
                    let rows = db::spawn_db(pool, move |p| {
                        db::touch_identity_activity_updates(&p, &peer_activity_updates);
                        db::get_peers_by_hashes(&p, &peer_activity_hashes, &identity_id)
                    })
                    .await
                    .unwrap_or_default();
                    emit_peers_batch(&state, &rows);
                }

                // Peers who messaged us before they announced have no real
                // name in contacts yet; refresh names when announces arrive.
                let announce_identity_id =
                    db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
                        .await
                        .expect("db task panicked")
                        .and_then(|id| id.get("hash").and_then(|h| h.as_str()).map(String::from))
                        .unwrap_or_default();
                if !announce_identity_id.is_empty() {
                    let mut contacts_changed = false;
                    for a in &announces {
                        let display_name = a
                            .app_data
                            .as_ref()
                            .map(|d| extract_display_name(d))
                            .unwrap_or_default();
                        if !display_name.is_empty() {
                            let dest_hex = hex::encode(a.dest_hash);
                            let display_name_for_db = display_name.clone();
                            let announce_id_for_db = announce_identity_id.clone();
                            let updated = db::spawn_db(state.db.clone(), move |p| {
                                db::update_contact_name_from_announce(
                                    &p,
                                    &dest_hex,
                                    &display_name_for_db,
                                    &announce_id_for_db,
                                )
                            })
                            .await
                            .expect("db task panicked");
                            if updated {
                                contacts_changed = true;
                            }
                        }
                    }
                    if contacts_changed {
                        let announce_id_for_contacts = announce_identity_id.clone();
                        let contacts = db::spawn_db(state.db.clone(), move |p| {
                            db::get_all_contacts(&p, &announce_id_for_contacts)
                        })
                        .await
                        .expect("db task panicked");
                        let contacts_list: Vec<serde_json::Value> = contacts
                            .into_iter()
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
                            .collect();
                        state.emit_to_all("contacts_update", serde_json::json!(contacts_list));
                    }
                }
            }

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();

            let any_online = iface_stats
                .get("interfaces")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .any(|i| i.get("online").and_then(|o| o.as_bool()).unwrap_or(false))
                })
                .unwrap_or(false);

            let connected = any_online
                && (mode == rns_runtime::reticulum::InstanceMode::Client
                    || mode == rns_runtime::reticulum::InstanceMode::Shared);

            json!({
                "timestamp": now,
                "connected": connected,
                "interface_stats": iface_stats,
                "path_table": path_table,
                "path_index": path_index,
                "path_table_total": path_table_total,
                "path_table_truncated": path_table_truncated,
                "rate_table": [],
                "link_count": link_count,
            })
        };

        if state
            .identity_session_generation
            .load(std::sync::atomic::Ordering::SeqCst)
            != poll_generation
        {
            continue;
        }

        state.set_last_stats(stats.clone());
        // Emit even when suspended; freshest snapshot is queued for resume.
        state.emit_to_all("stats_update", stats);

        last_poll_at = std::time::Instant::now();
    }
}

// LXMF send → "failed" if no delivery proof within this window.
const MESSAGE_TIMEOUT_SECS: f64 = 180.0;

// LRGP sessions with no delivery proof eventually flip to "undelivered", but
// propagated challenges can sit on a relay for the normal LXMF expiry window.
const LRGP_UNDELIVERED_TIMEOUT_SECS: f64 = lxmf_core::constants::MESSAGE_EXPIRY as f64;

async fn check_message_timeouts(state: &AppState) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let timed_out: Vec<String> = if let Ok(mut times) = state.message_send_times.lock() {
        let expired: Vec<String> = times
            .iter()
            .filter(|(_, send_time)| now - **send_time > MESSAGE_TIMEOUT_SECS)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            times.remove(id);
        }
        expired
    } else {
        Vec::new()
    };
    if timed_out.is_empty() {
        return;
    }

    // One blocking-pool hop for the whole sweep: state flips + method reads.
    let identity_id = helpers::active_identity_id(state);
    let ids_for_db = timed_out.clone();
    let methods = db::spawn_db(state.db.clone(), move |p| {
        ids_for_db
            .iter()
            .map(|msg_id| {
                db::update_message_state(&p, msg_id, &identity_id, "failed", None);
                db::get_message_delivery_method(&p, msg_id, &identity_id)
            })
            .collect::<Vec<Option<String>>>()
    })
    .await
    .unwrap_or_default();

    for (msg_id, method) in timed_out.iter().zip(methods) {
        let client_msg_id = state
            .msg_id_map
            .lock()
            .ok()
            .and_then(|mut map| map.remove(msg_id));
        state.emit_to_all(
            "lxmf_step",
            json!({
                "step": "failed",
                "msg_id": msg_id,
                "client_msg_id": client_msg_id,
                "reason": "timeout",
                "method": method,
            }),
        );
        tracing::debug!(msg_id = %msg_id, "Message timed out after {}s", MESSAGE_TIMEOUT_SECS);
        if state
            .network_log_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            state.emit_network_event(
                "error",
                &format!("Message delivery timed out after {}s", MESSAGE_TIMEOUT_SECS),
                msg_id,
                "essential",
            );
        }
    }
}

// Monotonic: never overwrite delivered/failed/undelivered.
async fn update_game_session_delivery_state(
    state: &AppState,
    session_id: &str,
    identity_id: &str,
    contact_hash: &str,
    new_state: &str,
) {
    let sid = session_id.to_string();
    let iid = identity_id.to_string();
    let ns = new_state.to_string();
    let pool = state.db.clone();
    let updated = db::spawn_db(pool, move |p| {
        let session = db::get_game_session(&p, &sid, &iid)?;
        let mut metadata = session
            .get("metadata")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let current = metadata
            .get("delivery_state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if matches!(current.as_str(), "delivered" | "failed" | "undelivered") {
            return None;
        }
        metadata.insert("delivery_state".to_string(), json!(ns));
        let metadata_json = serde_json::to_string(&serde_json::Value::Object(metadata))
            .unwrap_or_else(|_| "{}".into());
        let conn = p.get().ok()?;
        conn.execute(
            "UPDATE app_sessions SET metadata = ?1, updated_at = ?2 WHERE session_id = ?3 AND identity_id = ?4",
            rusqlite::params![
                metadata_json,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64(),
                sid,
                iid,
            ],
        ).ok()?;
        Some(())
    })
    .await
    .ok()
    .flatten();

    if updated.is_some() {
        let iid = identity_id.to_string();
        let ch = contact_hash.to_string();
        let (per_contact, all) = db::spawn_db(state.db.clone(), move |p| {
            let per = db::list_game_sessions(&p, &iid, Some(&ch), None);
            let all = db::list_game_sessions(&p, &iid, None, None);
            (per, all)
        })
        .await
        .unwrap_or_else(|_| (Vec::new(), Vec::new()));
        state.emit_to_all(
            "active_games",
            json!({"hash": contact_hash, "games": per_contact}),
        );
        state.emit_to_all("all_game_sessions", all.into());
    }
}

async fn sweep_undelivered_game_sessions(state: &AppState) {
    let pool = state.db.clone();
    let candidates: Vec<(String, String, String)> = db::spawn_db(pool, |p| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let cutoff = now - LRGP_UNDELIVERED_TIMEOUT_SECS;
        let conn = match p.get() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT session_id, identity_id, contact_hash, metadata FROM app_sessions WHERE status = 'pending' AND initiator = identity_id AND created_at < ?1",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map(rusqlite::params![cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3).unwrap_or_else(|_| "{}".into()),
            ))
        });
        let Ok(rows) = rows else { return Vec::new() };
        rows.filter_map(Result::ok)
            .filter(|(_, _, _, meta_json)| {
                let meta: serde_json::Value =
                    serde_json::from_str(meta_json).unwrap_or(json!({}));
                let ds = meta
                    .get("delivery_state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                !matches!(ds, "delivered" | "undelivered")
            })
            .map(|(sid, iid, ch, _)| (sid, iid, ch))
            .collect()
    })
    .await
    .unwrap_or_default();

    for (sid, iid, ch) in candidates {
        update_game_session_delivery_state(state, &sid, &iid, &ch, "undelivered").await;
        tracing::info!(
            session_id = %sid,
            "LRGP session timed out without delivery proof — marked undelivered"
        );
    }
}

// Batches per-peer updates into one emit per poll: per-peer emits drained
// the JNI global-ref table (cap 51,200) on Android in ~10 min and SIGABRT'd.
pub(crate) fn emit_peers_batch(state: &AppState, rows: &[db::PeerRow]) {
    if rows.is_empty() {
        return;
    }
    let arr: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "hash": r.hash,
                "identity_hash": r.identity_hash,
                "telephony_hash": telephony_hash_for_identity_hex(&r.identity_hash),
                "last_seen": r.last_seen,
                "first_seen": r.first_seen,
                "display_name": r.display_name,
                "profile_status": r.profile_status,
                "is_contact": r.is_contact,
                "last_interface": r.last_interface,
                "services": r.services,
            })
        })
        .collect();
    state.emit_to_all("peers_updated", json!({ "peers": arr }));
}

async fn touch_peer_last_heard(state: &AppState, source_hash: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let hash = source_hash.to_string();
    let identity_id = helpers::active_identity_id(state);
    let rows = db::spawn_db(state.db.clone(), move |p| {
        db::touch_identity_last_heard(&p, &hash, now);
        db::get_peers_by_hashes(&p, &[hash], &identity_id)
    })
    .await
    .unwrap_or_default();
    emit_peers_batch(state, &rows);
}

// Three wire shapes: UTF-8 string, msgpack BIN/STR (NomadNet),
// msgpack fixarray(1)[bin8(name)] (rsdeck/ratcom).
pub(crate) fn extract_display_name(data: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(data) {
        return s.to_string();
    }
    let mut cursor = std::io::Cursor::new(data);
    if let Ok(value) = rmpv::decode::read_value(&mut cursor)
        && let Some(name) = extract_name_from_msgpack(&value)
    {
        return name;
    }
    String::new()
}

fn extract_name_from_msgpack(value: &rmpv::Value) -> Option<String> {
    match value {
        rmpv::Value::String(s) => s.as_str().map(|s| s.to_string()),
        rmpv::Value::Binary(b) => std::str::from_utf8(b).ok().map(|s| s.to_string()),
        rmpv::Value::Array(arr) => arr.iter().find_map(extract_name_from_msgpack),
        _ => None,
    }
}

// Returns true if the envelope was LRGP (dispatched); false → fall through.
async fn try_handle_inbound_lrgp(
    state: &AppState,
    msg: &lxmf_core::message::LxMessage,
    sender_hash: &str,
    identity_id: &str,
) -> bool {
    let mut rmpv_fields: std::collections::HashMap<u8, rmpv::Value> =
        std::collections::HashMap::new();
    for (&key, bytes) in &msg.fields {
        let mut cursor = std::io::Cursor::new(bytes);
        if let Ok(value) = rmpv::decode::read_value(&mut cursor) {
            rmpv_fields.insert(key, value);
        } else if let Ok(s) = std::str::from_utf8(bytes) {
            rmpv_fields.insert(key, rmpv::Value::String(s.into()));
        } else {
            rmpv_fields.insert(key, rmpv::Value::Binary(bytes.clone()));
        }
    }

    let envelope = match lrgp::envelope::unpack_envelope(&rmpv_fields) {
        Ok(Some(env)) => env,
        _ => return false,
    };

    tracing::info!(
        from = %sender_hash,
        "Inbound LRGP game message received"
    );

    let result = match state
        .lrgp_router
        .dispatch_incoming(&envelope, sender_hash, identity_id)
    {
        Ok(r) => r,
        Err(e) => {
            let sid_early = envelope
                .get("s")
                .and_then(|v| lrgp::envelope::value_as_str(v))
                .unwrap_or("");
            let cmd_early = envelope
                .get("c")
                .and_then(|v| lrgp::envelope::value_as_str(v))
                .unwrap_or("");
            tracing::warn!(
                target: "ttt_trace",
                step = "inbound.dispatched",
                valid = false,
                sid = %short_id(sid_early),
                command = %cmd_early,
                from = %short_id(sender_hash),
                err = %e,
                "dispatch_incoming returned error"
            );
            tracing::warn!("LRGP dispatch error: {e}");
            // Envelope parsed as LRGP; do not fall through to chat.
            return true;
        }
    };

    let session_id = envelope
        .get("s")
        .and_then(|v| lrgp::envelope::value_as_str(v))
        .unwrap_or("")
        .to_string();
    let app_ver = envelope
        .get("a")
        .and_then(|v| lrgp::envelope::value_as_str(v))
        .unwrap_or("");
    let app_id = lrgp::envelope::parse_app_version(app_ver)
        .map(|(id, _)| id.to_string())
        .unwrap_or_default();
    let command = envelope
        .get("c")
        .and_then(|v| lrgp::envelope::value_as_str(v))
        .unwrap_or("")
        .to_string();

    // Empty session_id can't address app_sessions PK; drop without DB write.
    if session_id.is_empty() {
        tracing::warn!(
            target: "ttt_trace",
            step = "inbound.empty_sid_rejected",
            app_id = %app_id,
            command = %command,
            from = %short_id(sender_hash),
            my = %short_id(identity_id),
            "dropping inbound LRGP envelope with empty session_id"
        );
        return true;
    }

    tracing::info!(
        target: "ttt_trace",
        step = "inbound.dispatched",
        valid = true,
        sid = %short_id(&session_id),
        command = %command,
        app_id = %app_id,
        from = %short_id(sender_hash),
        my = %short_id(identity_id),
        has_session = result.session.is_some(),
        has_emit = result.emit.is_some(),
        has_error = result.error.is_some(),
        "dispatch_incoming ok"
    );

    let payload_json = result
        .emit
        .as_ref()
        .map(|e| serde_json::to_value(e).unwrap_or(json!({})))
        .unwrap_or(json!({}));

    // The whole persistence sequence is one blocking-pool hop; previously
    // these ~7 sequential sync calls all ran on the async worker.
    let (had_session, sessions, all) = {
        let session_id = session_id.clone();
        let identity_id = identity_id.to_string();
        let sender_hash = sender_hash.to_string();
        let app_id = app_id.clone();
        let command = command.clone();
        let session_data = result.session.clone();
        let timestamp = msg.timestamp;
        db::spawn_db(state.db.clone(), move |p| {
            let had_session = db::get_game_session(&p, &session_id, &identity_id).is_some();

            let action_num = db::get_game_action_count(&p, &session_id, &identity_id);
            let action = lrgp::store::Action {
                session_id: session_id.clone(),
                identity_id: identity_id.clone(),
                action_num,
                command: command.clone(),
                payload_json: serde_json::to_string(&payload_json)
                    .unwrap_or_else(|_| "{}".into()),
                sender: sender_hash.clone(),
                timestamp,
            };
            db::save_game_action(&p, &action, None);

            if let Some(ref session_data) = session_data {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();

                let status = session_data
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("pending");
                let initiator = session_data
                    .get("initiator")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&sender_hash);

                // Unwrap nested "metadata" so DB has flat fields the frontend reads.
                let metadata_map: std::collections::HashMap<String, serde_json::Value> =
                    session_data
                        .get("metadata")
                        .and_then(|v| v.as_object())
                        .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                        .unwrap_or_default();

                // Bump unread relative to the persisted row so repeat actions
                // accumulate instead of clobbering each other to 1.
                let unread = db::get_game_session(&p, &session_id, &identity_id)
                    .as_ref()
                    .and_then(|row| row.get("unread").and_then(|v| v.as_i64()))
                    .unwrap_or(0)
                    + 1;

                let session = lrgp::session::Session {
                    session_id: session_id.clone(),
                    identity_id: identity_id.clone(),
                    app_id: app_id.clone(),
                    app_version: 1,
                    contact_hash: sender_hash.clone(),
                    initiator: initiator.to_string(),
                    status: status.to_string(),
                    metadata: metadata_map,
                    unread,
                    created_at: session_data
                        .get("created_at")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(now),
                    updated_at: now,
                    last_action_at: now,
                };
                db::save_game_session(&p, &session);
            } else if let Some(existing) = db::get_game_session(&p, &session_id, &identity_id) {
                let unread = existing.get("unread").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
                if let Ok(conn) = p.get() {
                    conn.execute(
                        "UPDATE app_sessions SET unread = ?1, last_action_at = ?2 WHERE session_id = ?3 AND identity_id = ?4",
                        rusqlite::params![unread, timestamp, session_id, identity_id],
                    ).ok();
                }
            }

            let sessions = db::list_game_sessions(&p, &identity_id, Some(&sender_hash), None);
            let all = db::list_game_sessions(&p, &identity_id, None, None);
            (had_session, sessions, all)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "LRGP inbound persistence task panicked");
            (false, Vec::new(), Vec::new())
        })
    };

    if result.error.is_none() {
        notify_game_if_background(
            state,
            sender_hash,
            &session_id,
            &app_id,
            &command,
            !had_session,
        );
    }

    state.emit_to_all(
        "active_games",
        json!({"hash": sender_hash, "games": sessions}),
    );
    tracing::info!(
        target: "ttt_trace",
        step = "inbound.emitted_all",
        sid = %short_id(&session_id),
        command = %command,
        from = %short_id(sender_hash),
        total_sessions = all.len(),
        "emitting all_game_sessions + active_games after inbound"
    );
    state.emit_to_all("all_game_sessions", all.into());

    // Positive per-action signal so the frontend can force-redraw the active
    // board even if the bulk `all_game_sessions` payload looks identical.
    state.emit_to_all(
        "game_action_received",
        json!({
            "session_id": session_id,
            "app_id": app_id,
            "command": command,
            "from": sender_hash,
            "applied": result.error.is_none(),
        }),
    );

    true
}

#[cfg(test)]
mod packet_dispatch_tests {
    use super::*;

    fn raw_packet(
        header_type: rns_wire::flags::HeaderType,
        transport_id: Option<[u8; 16]>,
        destination_hash: [u8; 16],
    ) -> Vec<u8> {
        let header = rns_wire::header::PacketHeader {
            flags: rns_wire::flags::PacketFlags {
                header_type,
                context_flag: false,
                transport_type: match header_type {
                    rns_wire::flags::HeaderType::Header1 => {
                        rns_wire::flags::TransportType::Broadcast
                    }
                    rns_wire::flags::HeaderType::Header2 => {
                        rns_wire::flags::TransportType::Transport
                    }
                },
                destination_type: rns_wire::flags::DestinationType::Single,
                packet_type: rns_wire::flags::PacketType::Data,
            },
            hops: 0,
            transport_id,
            destination_hash,
            context: rns_wire::context::PacketContext::None,
        };
        let mut raw = header.pack();
        raw.extend_from_slice(b"payload");
        raw
    }

    #[test]
    fn inbound_packet_targets_header1_destination() {
        let destination_hash = [0x11; 16];
        let raw = raw_packet(rns_wire::flags::HeaderType::Header1, None, destination_hash);

        assert!(inbound_packet_targets_destination(&raw, destination_hash));
    }

    #[test]
    fn inbound_packet_targets_header2_final_destination() {
        let transport_id = [0x22; 16];
        let destination_hash = [0x33; 16];
        let raw = raw_packet(
            rns_wire::flags::HeaderType::Header2,
            Some(transport_id),
            destination_hash,
        );

        assert!(inbound_packet_targets_destination(&raw, destination_hash));
    }

    #[test]
    fn inbound_packet_does_not_match_header2_transport_id_as_destination() {
        let transport_id = [0x44; 16];
        let destination_hash = [0x55; 16];
        let raw = raw_packet(
            rns_wire::flags::HeaderType::Header2,
            Some(transport_id),
            destination_hash,
        );

        assert!(!inbound_packet_targets_destination(&raw, transport_id));
    }
}

#[cfg(test)]
mod inbound_pipeline_tests {
    use super::*;
    use crate::lxmf::LxmfManager;
    use r2d2_sqlite::SqliteConnectionManager;
    use ratspeak_core::config::DashboardConfig;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_PIPELINE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct RecordingEmitter {
        events: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl ratspeak_core::Emitter for RecordingEmitter {
        fn emit(&self, event: &str, payload: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push((event.to_string(), payload));
        }
    }

    impl RecordingEmitter {
        fn count(&self, name: &str) -> usize {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|(event, _)| event == name)
                .count()
        }
    }

    fn pipeline_state() -> (Arc<AppState>, Arc<RecordingEmitter>) {
        let unique = TEMP_PIPELINE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "ratspeak-inbound-pipeline-{}-{}-{unique}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data_dir = root.join(".ratspeak");
        let rns_config_dir = data_dir.join("reticulum");
        std::fs::create_dir_all(&rns_config_dir).unwrap();
        let manager = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        db::init_schema(&pool).unwrap();
        let emitter = Arc::new(RecordingEmitter::default());
        let state = AppState::new(
            DashboardConfig {
                data_root: root.clone(),
                data_dir,
                rns_config_dir,
                rns_config_dir_overridden: false,
                max_log_entries: 200,
            },
            pool,
            emitter.clone(),
            Arc::new(ratspeak_core::NoopNotifier),
        );
        let mgr = LxmfManager::load_or_create(&root, None, None).unwrap();
        *state.lxmf.lock().unwrap() = Some(mgr);
        (Arc::new(state), emitter)
    }

    fn local_dest(state: &AppState) -> [u8; 16] {
        let hex_hash = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .lxmf_hash
            .clone();
        hex::decode(hex_hash).unwrap().try_into().unwrap()
    }

    fn local_identity(state: &AppState) -> String {
        state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .identity_hash
            .clone()
    }

    fn packed_inbound(dest: [u8; 16], src: [u8; 16], content: &str) -> Vec<u8> {
        let mut msg = lxmf_core::message::LxMessage::new(
            dest,
            src,
            "",
            content,
            lxmf_core::constants::DeliveryMethod::Direct,
        );
        // Unsigned-by-unknown-sender: verify returns None and the message is
        // still delivered, so tests don't need real peer keys.
        msg.signature = Some([0u8; 64]);
        msg.pack().unwrap()
    }

    fn message_rows(state: &AppState) -> i64 {
        state
            .db
            .get()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap()
    }

    fn reaction_rows(state: &AppState) -> i64 {
        state
            .db
            .get()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM reactions", [], |row| row.get(0))
            .unwrap()
    }

    #[tokio::test]
    async fn inbound_message_persists_and_emits() {
        let (state, emitter) = pipeline_state();
        let data = packed_inbound(local_dest(&state), [0xEE; 16], "hello");

        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;

        assert_eq!(message_rows(&state), 1);
        assert_eq!(emitter.count("lxmf_message"), 1);
        assert!(emitter.count("contacts_update") >= 1);
        assert!(emitter.count("unread_total") >= 1);
    }

    #[tokio::test]
    async fn duplicate_inbound_is_skipped() {
        let (state, emitter) = pipeline_state();
        let data = packed_inbound(local_dest(&state), [0xEE; 16], "once");

        handle_decrypted_lxmf(&state, data.clone(), InboundLxmfSource::Propagated).await;
        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;

        assert_eq!(message_rows(&state), 1, "sender retry must dedupe");
        assert_eq!(emitter.count("lxmf_message"), 1);
    }

    #[tokio::test]
    async fn blocked_sender_is_discarded() {
        let (state, emitter) = pipeline_state();
        let src = [0xEE; 16];
        db::block_contact(
            &state.db,
            &hex::encode(src),
            "blocked peer",
            &local_identity(&state),
        );
        let data = packed_inbound(local_dest(&state), src, "should vanish");

        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;

        assert_eq!(message_rows(&state), 0);
        assert_eq!(emitter.count("lxmf_message"), 0);
    }

    /// T1-9: with enforce_stamps on, an unstamped message is rejected on
    /// EVERY inbound source — the old link/propagated path skipped the check.
    #[tokio::test]
    async fn unstamped_message_rejected_on_all_sources_when_enforced() {
        let (state, emitter) = pipeline_state();
        state
            .enforce_stamps
            .store(true, std::sync::atomic::Ordering::Relaxed);
        state
            .required_stamp_cost
            .store(8, std::sync::atomic::Ordering::Relaxed);

        let dest = local_dest(&state);
        let link_data = packed_inbound(dest, [0xE1; 16], "via link");
        handle_decrypted_lxmf(&state, link_data, InboundLxmfSource::Link { link_id: None }).await;

        let prop_data = packed_inbound(dest, [0xE2; 16], "via propagation");
        handle_decrypted_lxmf(&state, prop_data, InboundLxmfSource::Propagated).await;

        let opp_data = packed_inbound(dest, [0xE3; 16], "via opportunistic");
        let msg = lxmf_core::message::LxMessage::unpack(&opp_data).unwrap();
        process_inbound_lxmf(
            &state,
            msg,
            &opp_data,
            InboundLxmfSource::Opportunistic { raw: Bytes::new() },
        )
        .await;

        assert_eq!(message_rows(&state), 0, "all unstamped sources rejected");
        assert_eq!(emitter.count("lxmf_message"), 0);

        // Enforcement off again: the same wire bytes deliver.
        state
            .enforce_stamps
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let data = packed_inbound(dest, [0xE1; 16], "via link");
        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Link { link_id: None }).await;
        assert_eq!(message_rows(&state), 1);
    }

    /// The Link source persists like Propagated (shared pipeline).
    #[tokio::test]
    async fn link_source_persists_and_emits() {
        let (state, emitter) = pipeline_state();
        let data = packed_inbound(local_dest(&state), [0xEE; 16], "direct hello");

        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Link { link_id: None }).await;

        assert_eq!(message_rows(&state), 1);
        assert_eq!(emitter.count("lxmf_message"), 1);
    }

    #[tokio::test]
    async fn reaction_routes_to_reaction_store_not_conversation() {
        let (state, emitter) = pipeline_state();
        let target_id = hex::encode([0xAB; 32]);

        let mut msg = lxmf_core::message::LxMessage::new(
            local_dest(&state),
            [0xEE; 16],
            "",
            "",
            lxmf_core::constants::DeliveryMethod::Direct,
        );
        for (field_id, bytes) in
            lxmf::ratspeak_chat_custom_fields(&lxmf::RatspeakChatExtension::Reaction {
                target: target_id.clone(),
                emoji: "\u{1F44D}".to_string(),
                action: "add".to_string(),
            })
            .unwrap()
        {
            msg.fields.insert(field_id, bytes);
        }
        msg.signature = Some([0u8; 64]);
        let data = msg.pack().unwrap();

        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;

        assert_eq!(reaction_rows(&state), 1, "reaction recorded");
        assert_eq!(message_rows(&state), 0, "reactions never hit the chat log");
        assert_eq!(emitter.count("reaction_update"), 1);
        assert_eq!(emitter.count("lxmf_message"), 0);
    }
}

#[cfg(test)]
mod reaction_sanitizer_tests {
    use super::*;

    /// T0-5: peer-controlled reactions are rendered in the UI — markup and
    /// control characters must be rejected at ingest.
    #[test]
    fn rejects_markup_and_control_characters() {
        assert_eq!(sanitize_reaction_emoji("<b>x</b>"), None);
        assert_eq!(sanitize_reaction_emoji("a&b"), None);
        assert_eq!(sanitize_reaction_emoji("\"quote\""), None);
        assert_eq!(sanitize_reaction_emoji("it's"), None);
        assert_eq!(sanitize_reaction_emoji("a\nb"), None);
        assert_eq!(sanitize_reaction_emoji("\u{7f}"), None);
        assert_eq!(sanitize_reaction_emoji(""), None);
    }

    #[test]
    fn accepts_plausible_reactions_and_clamps_length() {
        assert_eq!(
            sanitize_reaction_emoji("\u{1F44D}").as_deref(),
            Some("\u{1F44D}")
        );
        assert_eq!(sanitize_reaction_emoji("+1").as_deref(), Some("+1"));
        let long = "x".repeat(40);
        assert_eq!(
            sanitize_reaction_emoji(&long).as_deref(),
            Some("xxxxxxxxxxxxxxxx")
        );
    }
}

#[cfg(test)]
mod identity_material_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_IDENTITY_MATERIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_ratspeak_dir(tag: &str) -> std::path::PathBuf {
        let n = TEMP_IDENTITY_MATERIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ratspeak-identity-material-{tag}-{}-{n}",
            std::process::id()
        ));
        let ratspeak_dir = dir.join(".ratspeak");
        std::fs::create_dir_all(&ratspeak_dir).unwrap();
        ratspeak_dir
    }

    #[test]
    fn encrypted_root_identity_counts_as_identity_material() {
        let dir = temp_ratspeak_dir("root-enc");
        std::fs::write(dir.join("identity.enc"), b"{}").unwrap();
        assert!(has_identity_material(&dir));
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn encrypted_profile_identity_counts_as_identity_material() {
        let dir = temp_ratspeak_dir("profile-enc");
        let profile_dir = dir.join("identities").join("abcdef");
        std::fs::create_dir_all(&profile_dir).unwrap();
        std::fs::write(profile_dir.join("identity.enc"), b"{}").unwrap();
        assert!(has_identity_material(&dir));
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn hardware_profile_identity_counts_as_identity_material() {
        let dir = temp_ratspeak_dir("profile-hwid");
        let profile_dir = dir
            .join("identities")
            .join("df3b53016f50e4ce7c2c90c97486977c");
        std::fs::create_dir_all(&profile_dir).unwrap();
        std::fs::write(profile_dir.join("identity.hwid"), b"{}").unwrap();
        assert!(has_identity_material(&dir));
        assert!(!has_plain_identity_material(&dir));
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }
}

#[cfg(test)]
mod transport_startup_tests {
    use super::*;
    use crate::config::DashboardConfig;
    use r2d2_sqlite::SqliteConnectionManager;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_TRANSPORT_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let n = TEMP_TRANSPORT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "ratspeak-transport-startup-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn memory_pool() -> ratspeak_db::DbPool {
        let manager = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        db::init_schema(&pool).unwrap();
        pool
    }

    fn state_for_root(root: std::path::PathBuf) -> AppState {
        let data_dir = root.join(".ratspeak");
        let rns_config_dir = data_dir.join("reticulum");
        std::fs::create_dir_all(&rns_config_dir).unwrap();
        AppState::new(
            DashboardConfig {
                data_root: root,
                data_dir,
                rns_config_dir,
                rns_config_dir_overridden: false,
                max_log_entries: 200,
            },
            memory_pool(),
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        )
    }

    #[test]
    fn startup_transport_on_rewrites_saved_config_before_rns_init() {
        let root = temp_root("on");
        let state = state_for_root(root.clone());
        let config_dir = state.config.rns_config_dir.clone();
        rns_config::write_config(
            &config_dir,
            "[reticulum]\nenable_transport = False\n\n[interfaces]\n",
        );
        db::set_setting(&state.db, "transport_mode", "on");

        reconcile_persisted_transport_mode_for_startup(&state, &config_dir);

        assert!(rns_config::transport_mode_enabled(&config_dir));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn startup_transport_auto_recomputes_from_saved_network_and_interfaces() {
        let root = temp_root("auto");
        let state = state_for_root(root.clone());
        let config_dir = state.config.rns_config_dir.clone();
        rns_config::write_config(
            &config_dir,
            "[reticulum]\nenable_transport = False\n\n[interfaces]\n\
             [[Local Network]]\n\
             type = AutoInterface\n\
             enabled = true\n",
        );
        db::set_setting(&state.db, "transport_mode", "auto");
        db::set_setting(&state.db, "transport_network_type", "wifi");

        reconcile_persisted_transport_mode_for_startup(&state, &config_dir);

        assert!(rns_config::transport_mode_enabled(&config_dir));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn startup_transport_auto_keeps_public_tcp_limit() {
        let root = temp_root("public-limit");
        let state = state_for_root(root.clone());
        let config_dir = state.config.rns_config_dir.clone();
        rns_config::write_config(
            &config_dir,
            "[reticulum]\nenable_transport = True\n\n[interfaces]\n\
             [[Ruby]]\n\
             type = TCPClientInterface\n\
             enabled = true\n\
             target_host = 1.ratspeak.org\n\
             target_port = 4141\n\
             [[Emerald]]\n\
             type = TCPClientInterface\n\
             enabled = true\n\
             target_host = 2.ratspeak.org\n\
             target_port = 4242\n",
        );
        db::set_setting(&state.db, "transport_mode", "auto");
        db::set_setting(&state.db, "transport_network_type", "wifi");

        reconcile_persisted_transport_mode_for_startup(&state, &config_dir);

        assert!(!rns_config::transport_mode_enabled(&config_dir));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn startup_transport_without_db_preserves_existing_enabled_config() {
        let root = temp_root("config-fallback");
        let state = state_for_root(root.clone());
        let config_dir = state.config.rns_config_dir.clone();
        rns_config::write_config(
            &config_dir,
            "[reticulum]\nenable_transport = True\n\n[interfaces]\n",
        );

        reconcile_persisted_transport_mode_for_startup(&state, &config_dir);

        assert!(rns_config::transport_mode_enabled(&config_dir));
        std::fs::remove_dir_all(root).ok();
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;
    use crate::config::DashboardConfig;
    use r2d2_sqlite::SqliteConnectionManager;
    use ratspeak_core::{NativeNotification, NativeNotifier};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_NOTIFICATION_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct RecordingNotifier {
        notifications: Mutex<Vec<NativeNotification>>,
    }

    impl NativeNotifier for RecordingNotifier {
        fn notify(&self, notification: NativeNotification) {
            self.notifications.lock().unwrap().push(notification);
        }
    }

    fn make_state(notifier: Arc<RecordingNotifier>) -> AppState {
        let unique = TEMP_NOTIFICATION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-notification-test-{}-{}-{unique}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = DashboardConfig::from_env_and_defaults(tmp);
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(mgr).unwrap();
        db::init_schema(&pool).unwrap();
        AppState::new(config, pool, Arc::new(ratspeak_core::NoopEmitter), notifier)
    }

    #[tokio::test]
    async fn inbound_message_notifies_only_when_backgrounded_and_enabled() {
        let notifier = Arc::new(RecordingNotifier::default());
        let state = make_state(notifier.clone());
        state
            .is_foreground
            .store(false, std::sync::atomic::Ordering::Relaxed);
        db::save_contact(
            &state.db,
            "abcd1234abcd1234",
            Some("Alice"),
            "trusted",
            "identity-a",
        );

        notify_inbound_message_if_background(
            &state,
            "abcd1234abcd1234",
            "identity-a",
            "hello from mesh",
            false,
        )
        .await;

        let seen = notifier.notifications.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].title, "Message from Alice");
        assert_eq!(seen[0].body, "hello from mesh");

        state
            .is_foreground
            .store(true, std::sync::atomic::Ordering::Relaxed);
        notify_inbound_message_if_background(
            &state,
            "abcd1234abcd1234",
            "identity-a",
            "foreground",
            false,
        )
        .await;
        assert_eq!(notifier.notifications.lock().unwrap().len(), 1);

        state
            .is_foreground
            .store(false, std::sync::atomic::Ordering::Relaxed);
        state.set_native_notifications_enabled(false);
        notify_inbound_message_if_background(
            &state,
            "abcd1234abcd1234",
            "identity-a",
            "disabled",
            false,
        )
        .await;
        assert_eq!(notifier.notifications.lock().unwrap().len(), 1);
    }

    #[test]
    fn notification_label_uses_announce_display_name_without_contact() {
        let notifier = Arc::new(RecordingNotifier::default());
        let state = make_state(notifier);
        db::touch_identity_activity(
            &state.db,
            &[(
                "abcd1234abcd1234".to_string(),
                1.0,
                Some("Mesh Alice".to_string()),
                Some("if0".to_string()),
            )],
        );

        assert_eq!(
            contact_label_from_db(&state.db, "abcd1234abcd1234", "identity-a"),
            "Mesh Alice"
        );
    }

    #[test]
    fn game_notification_uses_session_stable_id() {
        let notifier = Arc::new(RecordingNotifier::default());
        let state = make_state(notifier.clone());
        state
            .is_foreground
            .store(false, std::sync::atomic::Ordering::Relaxed);
        db::save_identity(&state.db, "identity-a", "lxmf-a", "Me", "Me");
        db::set_active_identity(&state.db, "identity-a").unwrap();
        db::save_contact(
            &state.db,
            "feedfacefeedface",
            Some("Rook"),
            "trusted",
            "identity-a",
        );

        notify_game_if_background(
            &state,
            "feedfacefeedface",
            "session-1",
            "chess",
            "move",
            false,
        );
        notify_game_if_background(
            &state,
            "feedfacefeedface",
            "session-1",
            "chess",
            "move",
            false,
        );

        let seen = notifier.notifications.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].notification_id, seen[1].notification_id);
        assert_eq!(seen[0].title, "Game update");
        assert!(seen[0].body.contains("Rook"));
    }

    #[test]
    fn opportunistic_announce_timestamps_use_unix_milliseconds() {
        assert_eq!(unix_secs_to_ms(1.234), Some(1234));
        assert_eq!(unix_secs_to_ms(0.0), None);
        assert_eq!(unix_secs_to_ms(f64::NAN), None);
    }

    #[test]
    fn opportunistic_announce_claim_is_session_throttled() {
        let notifier = Arc::new(RecordingNotifier::default());
        let state = make_state(notifier);

        assert!(claim_opportunistic_announce(&state, "alice"));
        assert!(!claim_opportunistic_announce(&state, "alice"));
        release_opportunistic_announce(&state, "alice");
        assert!(!claim_opportunistic_announce(&state, "bob"));

        *state.last_opportunistic_announce_at.lock().unwrap() = Some(
            std::time::Instant::now() - OPPORTUNISTIC_ANNOUNCE_COOLDOWN - Duration::from_secs(1),
        );
        assert!(claim_opportunistic_announce(&state, "bob"));
    }

    #[test]
    fn identity_scoped_state_clears_opportunistic_announce_suppression() {
        let notifier = Arc::new(RecordingNotifier::default());
        let state = make_state(notifier);
        state
            .last_lxmf_delivery_announce_at_ms
            .store(1234, Ordering::Relaxed);
        *state.last_opportunistic_announce_at.lock().unwrap() = Some(std::time::Instant::now());
        state
            .opportunistic_announce_inflight
            .lock()
            .unwrap()
            .insert("alice".into());

        state.clear_identity_scoped_runtime_state();

        assert_eq!(
            state
                .last_lxmf_delivery_announce_at_ms
                .load(Ordering::Relaxed),
            0
        );
        assert!(
            state
                .last_opportunistic_announce_at
                .lock()
                .unwrap()
                .is_none()
        );
        assert!(
            state
                .opportunistic_announce_inflight
                .lock()
                .unwrap()
                .is_empty()
        );
    }
}
