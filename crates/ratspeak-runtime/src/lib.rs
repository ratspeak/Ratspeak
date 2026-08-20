//! Ratspeak runtime: RNS + LXMF + LRGP wiring, AppState, async loops.
//! Depends on `ratspeak-core` + `ratspeak-db` plus the protocol crates.
//! Holds zero `tauri::*` — emits go through `ratspeak_core::Emitter`.

// Holding `std::sync::MutexGuard` / `RwLockGuard` across `.await` breaks
// `Send` bounds or stalls the executor.
#![warn(clippy::await_holding_lock)]

pub mod activity;
pub mod announce;
pub mod announce_handlers;
pub mod blackhole;
pub mod channel_hub;
pub mod channels;
#[cfg(feature = "hardware")]
pub mod hardware;
pub mod helpers;
pub mod identity_prune;
pub mod image_attachment;
pub mod lxmf;
pub mod lxmf_persistence;
pub mod messaging;
pub mod mobile_platform;
pub mod propagation;
mod rnode_activity;
pub mod rns;
pub mod rns_config;
pub mod rrc;
pub mod state;
pub mod transport_observation;
pub mod vault;
#[cfg(feature = "lxst-voice")]
pub mod voice;
#[cfg(feature = "lxst-voice")]
pub mod voice_memo;

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
use ratspeak_core::{LXMF_DELIVERY_APP_NAME, LXMF_PROPAGATION_APP_NAME};
use rns_identity::destination::Destination;
use serde_json::{Value, json};

use activity::ActivityRecorderError;
use activity::producer::{self, ProducerEvent};
use announce::{AnnounceAdmission, AnnounceIntent, AnnounceLeadership, AnnounceOrigin};
#[cfg(all(feature = "ble", target_os = "android"))]
use mobile_platform::{NativeBleRnodeDisconnect, NativeBleRnodeRequest};
use state::{ActivityRequestFence, AppState};

pub use rnode_activity::{
    PendingRNodeActivityMonitor, RNodeActivityRuntimeContext, spawn_startup_rnode_activity_monitor,
};
pub use state::RNodeActivityOrigin;

const CHANNEL_BUFFER_SIZE: usize = 64;

// ~150 bytes/entry → ~750 KB ceiling for hub bootstrap bursts.
const ANNOUNCE_HISTORY_CAP: usize = 5_000;
const AUTO_INBOX_READY_RETRY_SECS: f64 = 30.0;
const OPPORTUNISTIC_ANNOUNCE_COOLDOWN: Duration = Duration::from_secs(60);
// Presence construction no longer runs on the requesting IPC future. A queued
// lifecycle may wait cooperatively for a busy LXMF tick, but it never holds the
// manager lock while waiting and still has one bounded terminal deadline.
const ANNOUNCE_LXMF_BUILD_RETRY_WINDOW: Duration = Duration::from_secs(30);
const ANNOUNCE_LXMF_BUILD_RETRY_INTERVAL: Duration = Duration::from_millis(50);
const ANNOUNCE_QUEUE_ADMISSION_WAIT: Duration = Duration::from_secs(1);
const ANNOUNCE_INTERFACE_DISPATCH_WAIT: Duration = Duration::from_secs(5);

#[cfg(all(feature = "ble", target_os = "android"))]
fn interface_mode_name(mode: rns_interface::traits::InterfaceMode) -> &'static str {
    use rns_interface::traits::InterfaceMode;
    match mode {
        InterfaceMode::Full => "full",
        InterfaceMode::PointToPoint => "point_to_point",
        InterfaceMode::AccessPoint => "access_point",
        InterfaceMode::Roaming => "roaming",
        InterfaceMode::Boundary => "boundary",
        InterfaceMode::Gateway => "gateway",
        InterfaceMode::Internal => "internal",
    }
}

#[cfg(all(feature = "ble", target_os = "android"))]
fn start_deferred_android_ble_rnode(
    state: &Arc<AppState>,
    origin: Option<RNodeActivityOrigin>,
    deferred: Vec<rns_runtime::interface_factory::BleRNodeInterfaceConfig>,
) {
    let Some(origin) = origin else {
        return;
    };
    let mut deferred = deferred.into_iter();
    let Some(config) = deferred.next() else {
        return;
    };
    if deferred.next().is_some() {
        state.publish_mobile_hardware_state(
            "ble_rnode",
            "conflict",
            Some("multiple_configured_radios"),
        );
        tracing::warn!(
            reason = "multiple_android_ble_rnodes",
            "only the first configured Android BLE RNode can own the native radio"
        );
    }

    let address = config.port.strip_prefix("ble://").unwrap_or(&config.port);
    let Ok(tcp_port) = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr().map(|address| address.port()))
    else {
        state.publish_mobile_hardware_state("ble_rnode", "failed", Some("bridge_unavailable"));
        return;
    };
    let activity_fence = state.activity_request_fence();
    let activity_operation = state.begin_ble_rnode_activity_operation(activity_fence, None);
    let request = NativeBleRnodeRequest {
        address: address.to_string(),
        tcp_port,
        activity_operation: activity_operation.clone(),
        native_generation: origin.native_generation(),
        name: config.name,
        port: config.port,
        frequency: u64::from(config.frequency),
        bandwidth: u64::from(config.bandwidth),
        spreading_factor: config.spreading_factor,
        coding_rate: config.coding_rate,
        tx_power: config.tx_power,
        mode: Some(interface_mode_name(config.mode).to_string()),
        mode_value: config.mode as u8,
        airtime_limit_short: config.st_alock.map(f64::from),
        airtime_limit_long: config.lt_alock.map(f64::from),
        id_interval: config.id_interval,
        id_callsign: config.id_callsign,
        saved_startup: true,
    };
    if !state
        .mobile_platform_bridge()
        .start_or_replace_ble_rnode(request)
    {
        let _ = state.invalidate_ble_rnode_activity_operation_if_token(&activity_operation);
        state.publish_mobile_hardware_state("ble_rnode", "failed", Some("bridge_unavailable"));
        return;
    }
}

fn accepts_inbound_lxmf_resource(data_size: usize, limit_bytes: usize) -> bool {
    data_size <= limit_bytes
}

fn inbound_resource_admission_bytes(
    data_size: usize,
    total_segments: usize,
    limit_bytes: usize,
) -> Option<usize> {
    if !accepts_inbound_lxmf_resource(data_size, limit_bytes) {
        return None;
    }
    if total_segments <= 1 {
        return Some(data_size);
    }
    let max_segments = limit_bytes.div_ceil(rns_protocol::resource::MAX_EFFICIENT_SIZE);
    if total_segments > max_segments {
        return None;
    }
    Some(limit_bytes.max(rns_protocol::resource::MAX_EFFICIENT_SIZE + 1))
}

fn admit_inbound_lxmf_resource(
    state: &Arc<AppState>,
    link_id: [u8; 16],
    advertisement: &rns_protocol::resource_adv::ResourceAdvertisement,
) -> bool {
    let limit = state.lxmf_delivery_limit_bytes();
    if let Some(admission_bytes) = inbound_resource_admission_bytes(
        advertisement.data_size,
        advertisement.total_segments,
        limit,
    ) {
        match state.admit_inbound_attachment_resource(
            link_id,
            advertisement.resource_hash,
            advertisement.original_hash,
            admission_bytes,
        ) {
            Ok(()) => return true,
            Err(error) => {
                let activity_origin = state.activity_request_fence();
                let reason = match error {
                    state::AttachmentTransferAdmissionError::MemoryPressure => {
                        producer::LxmfInboundRejectionReason::AttachmentMemoryPressure
                    }
                    _ => producer::LxmfInboundRejectionReason::AttachmentBusy,
                };
                record_activity_if_current(state, activity_origin, || {
                    Ok(producer::lxmf_inbound_rejected(
                        producer::LxmfInboundRejected {
                            link: producer::LinkId::new(link_id),
                            encoded_bytes: advertisement.data_size as u64,
                            max_message_bytes: limit as u64,
                            reason,
                        },
                    ))
                });
                tracing::warn!(
                    link = %short_id(&hex::encode(link_id)),
                    encoded_bytes = advertisement.data_size,
                    reason = ?error,
                    "rejected inbound LXMF Resource for bounded memory admission"
                );
                return false;
            }
        }
    }

    let activity_origin = state.activity_request_fence();
    record_activity_if_current(state, activity_origin, || {
        Ok(producer::lxmf_inbound_rejected(
            producer::LxmfInboundRejected {
                link: producer::LinkId::new(link_id),
                encoded_bytes: advertisement.data_size as u64,
                max_message_bytes: limit as u64,
                reason: producer::LxmfInboundRejectionReason::SizeLimit,
            },
        ))
    });
    tracing::warn!(
        link = %short_id(&hex::encode(link_id)),
        encoded_bytes = advertisement.data_size,
        max_message_bytes = limit,
        reason = "size_limit",
        "rejected inbound LXMF Resource advertisement"
    );
    false
}

#[cfg(test)]
mod lxmf_delivery_admission_tests {
    use super::*;

    #[test]
    fn incoming_resource_limit_has_exact_boundaries_and_safe_maximum() {
        assert!(accepts_inbound_lxmf_resource(1_000_000, 1_000_000));
        assert!(!accepts_inbound_lxmf_resource(1_000_001, 1_000_000));
        assert!(accepts_inbound_lxmf_resource(
            state::LXMF_DELIVERY_LIMIT_MAX_BYTES,
            state::LXMF_DELIVERY_LIMIT_MAX_BYTES,
        ));
        assert_eq!(
            inbound_resource_admission_bytes(1_000_000, 1, 1_000_000),
            Some(1_000_000)
        );
        assert_eq!(
            inbound_resource_admission_bytes(
                rns_protocol::resource::MAX_EFFICIENT_SIZE,
                2,
                rns_protocol::resource::MAX_EFFICIENT_SIZE + 1,
            ),
            Some(rns_protocol::resource::MAX_EFFICIENT_SIZE + 1)
        );
        assert_eq!(
            inbound_resource_admission_bytes(
                rns_protocol::resource::MAX_EFFICIENT_SIZE,
                3,
                rns_protocol::resource::MAX_EFFICIENT_SIZE + 1,
            ),
            None
        );
        const {
            assert!(
                state::LXMF_DELIVERY_LIMIT_MAX_BYTES < rns_protocol::resource::MAX_RESOURCE_SIZE
            );
        }
    }
}

fn lxmf_progress_activity_step(
    update: &lxmf::LxmfDeliveryProgressUpdate,
) -> Option<producer::LxmfProgressStep> {
    use lxmf::{LxmfDeliveryProgressKind as Kind, LxmfDeliveryProgressRepresentation as Repr};

    match (update.kind, update.delivery_representation) {
        (Kind::LinkEstablishing, _) => Some(producer::LxmfProgressStep::LinkEstablishing),
        (Kind::LinkEstablished, _) => Some(producer::LxmfProgressStep::LinkReady),
        (Kind::DirectLinkPending, _) => Some(producer::LxmfProgressStep::DirectPending),
        (Kind::DirectLinkReused | Kind::BackchannelLinkReused, _) => {
            Some(producer::LxmfProgressStep::LinkReused)
        }
        (Kind::TransferStarted, Repr::Resource) => {
            Some(producer::LxmfProgressStep::ResourceStarted)
        }
        (Kind::TransferProgress, Repr::Resource) => {
            Some(producer::LxmfProgressStep::ResourceProgress)
        }
        (Kind::AwaitingProof, _) => Some(producer::LxmfProgressStep::AwaitingProof),
        _ => None,
    }
}

fn lxmf_progress_activity_method(
    method: lxmf::LxmfDeliveryProgressMethod,
) -> producer::LxmfDeliveryMethod {
    match method {
        lxmf::LxmfDeliveryProgressMethod::Direct => producer::LxmfDeliveryMethod::Direct,
        lxmf::LxmfDeliveryProgressMethod::PropagationDeposit => {
            producer::LxmfDeliveryMethod::Propagated
        }
    }
}

fn lxmf_progress_supersedes_state(
    updates: &[lxmf::LxmfDeliveryProgressUpdate],
    message_id: &str,
    state: &str,
) -> bool {
    use lxmf::LxmfDeliveryProgressKind as Kind;

    updates.iter().any(|update| {
        if update.msg_id != message_id {
            return false;
        }
        match state {
            "reusing_backchannel" => matches!(
                update.kind,
                Kind::DirectLinkReused | Kind::BackchannelLinkReused
            ),
            "sending_via_link" => matches!(
                update.kind,
                Kind::LinkEstablishing
                    | Kind::LinkEstablished
                    | Kind::DirectLinkPending
                    | Kind::DirectLinkReused
                    | Kind::BackchannelLinkReused
                    | Kind::TransferStarted
                    | Kind::TransferProgress
                    | Kind::AwaitingProof
            ),
            "sent" => matches!(update.kind, Kind::AwaitingProof),
            _ => false,
        }
    })
}

fn record_activity_if_current<F>(state: &AppState, origin: ActivityRequestFence, make: F)
where
    F: FnOnce() -> Result<ProducerEvent, activity::ActivityRejectReason>,
{
    let _ = state
        .activity
        .record_event_fenced(|| state.is_current_activity_origin_fence(origin), make);
}

fn record_lxmf_progress(
    state: &AppState,
    origin: ActivityRequestFence,
    update: &lxmf::LxmfDeliveryProgressUpdate,
) {
    if let Some(step) = lxmf_progress_activity_step(update) {
        record_activity_if_current(state, origin, || {
            let message = producer::MessageId::from_hex(&update.msg_id)?;
            let destination = producer::DestinationHash::from_hex(&update.dest_hash)?;
            let link = update
                .link_id
                .as_deref()
                .map(producer::LinkId::from_hex)
                .transpose()?;
            let percent = update.progress.map(|progress| {
                (progress * 100.0)
                    .round()
                    .clamp(0.0, 100.0)
                    .min(f64::from(u8::MAX)) as u8
            });
            Ok(producer::lxmf_delivery_progress(
                producer::LxmfDeliveryProgress {
                    message,
                    destination,
                    link,
                    method: lxmf_progress_activity_method(update.event_method),
                    step,
                    percent,
                    attempts: update.attempts,
                },
            ))
        });
    }
}

#[cfg(test)]
mod activity_delivery_adapter_tests {
    use super::*;

    fn update(
        kind: lxmf::LxmfDeliveryProgressKind,
        representation: lxmf::LxmfDeliveryProgressRepresentation,
    ) -> lxmf::LxmfDeliveryProgressUpdate {
        lxmf::LxmfDeliveryProgressUpdate {
            msg_id: "11".repeat(32),
            kind,
            event_method: lxmf::LxmfDeliveryProgressMethod::Direct,
            delivery_representation: representation,
            step: "legacy_display_text_must_not_drive_activity",
            method: "legacy_method_text_must_not_drive_activity",
            progress: Some(0.5),
            link_id: Some("22".repeat(16)),
            dest_hash: "33".repeat(16),
            attempts: 1,
            representation: "legacy_representation_text_must_not_drive_activity",
            queued_deliveries: 0,
            in_flight_deliveries: 1,
            reason: Some("peer-controlled prose must stay product-only".into()),
        }
    }

    #[test]
    fn activity_delivery_adapter_consumes_typed_evidence_only() {
        let established = update(
            lxmf::LxmfDeliveryProgressKind::LinkEstablished,
            lxmf::LxmfDeliveryProgressRepresentation::Unknown,
        );
        assert!(matches!(
            lxmf_progress_activity_step(&established),
            Some(producer::LxmfProgressStep::LinkReady)
        ));

        let resource = update(
            lxmf::LxmfDeliveryProgressKind::TransferStarted,
            lxmf::LxmfDeliveryProgressRepresentation::Resource,
        );
        assert!(matches!(
            lxmf_progress_activity_step(&resource),
            Some(producer::LxmfProgressStep::ResourceStarted)
        ));

        let failed = update(
            lxmf::LxmfDeliveryProgressKind::Failed,
            lxmf::LxmfDeliveryProgressRepresentation::Resource,
        );
        assert!(lxmf_progress_activity_step(&failed).is_none());
    }

    #[test]
    fn typed_progress_suppresses_only_the_matching_coarse_state() {
        let established = update(
            lxmf::LxmfDeliveryProgressKind::LinkEstablished,
            lxmf::LxmfDeliveryProgressRepresentation::Unknown,
        );
        assert!(lxmf_progress_supersedes_state(
            std::slice::from_ref(&established),
            &established.msg_id,
            "sending_via_link",
        ));
        assert!(!lxmf_progress_supersedes_state(
            std::slice::from_ref(&established),
            &established.msg_id,
            "routing",
        ));
        assert!(!lxmf_progress_supersedes_state(
            &[established],
            &"44".repeat(32),
            "sending_via_link",
        ));
    }
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
    mgr.router.config.stamp_cost = if enforce && stamp_cost > 0 {
        Some(stamp_cost)
    } else {
        None
    };
    mgr.announce_ratspeak_usage = state.announce_ratspeak_usage_enabled();
    mgr.set_delivery_limit_kb(state.lxmf_delivery_limit_kb());

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
    helpers::diagnostic_short_protocol_id(s).unwrap_or("invalid")
}

fn compact_hash_label(hash: &str) -> String {
    if hash.len() > 12 {
        format!("{}..{}", &hash[..6], &hash[hash.len() - 6..])
    } else {
        hash.to_string()
    }
}

fn should_reannounce_for_interface_online(
    online: bool,
    suppressed: bool,
    auto_announce_interval: u64,
    cooldown_elapsed: bool,
) -> bool {
    online && !suppressed && auto_announce_interval > 0 && cooldown_elapsed
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
    let trimmed = content.trim();
    let lower = trimmed.to_ascii_lowercase();
    if has_attachment && lower.starts_with("[file:") && trimmed.ends_with(']') {
        return "New attachment".to_string();
    }
    let without_fallback = trimmed
        .rfind("\n[File:")
        .map(|index| &trimmed[..index])
        .unwrap_or(trimmed)
        .trim();
    let preview: String = without_fallback.chars().take(120).collect();
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
    if !state.should_surface_native_notification() {
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

fn game_name(state: &AppState, app_id: &str) -> String {
    state
        .lrgp_router
        .with_app(app_id, |app| app.manifest().display_name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "a game".to_string())
}

fn notify_game_if_background(
    state: &AppState,
    sender_hash: &str,
    session_id: &str,
    app_id: &str,
    command: &str,
    is_new_session: bool,
) {
    if !state.should_surface_native_notification() {
        return;
    }

    let identity_id = helpers::active_identity_id(state);
    let label = contact_label_from_db(&state.db, sender_hash, &identity_id);
    let game = game_name(state, app_id);
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

/// Soft-restart: serialize identity lifecycle, stop RNS/LXMF, then re-init.
/// Activity reset failure leaves the current protocol runtime untouched.
pub async fn restart_rns_lxmf(state: Arc<AppState>) -> Result<(), ActivityRecorderError> {
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    shutdown_rns_lxmf(&state).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    if let Ok(mut sig) = state.session_shutdown.write() {
        *sig = rns_runtime::lifecycle::ShutdownSignal::new();
    }
    state.set_startup_stage("checking");
    let data_dir = state.config.data_root.clone();
    init_rns_lxmf(Arc::clone(&state), data_dir).await;
    Ok(())
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
    if std::fs::create_dir_all(identity_config_dir).is_err() {
        tracing::warn!(
            reason = "create_directory",
            "failed to prepare identity Reticulum config directory"
        );
        return;
    }
    let source_content = match std::fs::read_to_string(&source) {
        Ok(content) => content,
        Err(_) => {
            tracing::warn!(
                reason = "read_config",
                "failed to read app-private Reticulum config for identity seed"
            );
            return;
        }
    };
    let identity_content = rns_config::strip_legacy_default_auto_interface(&source_content);
    if std::fs::write(&target, identity_content).is_err() {
        tracing::warn!(
            reason = "write_config",
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
            if let Some(id) =
                startup_public_tcp_server_id_from_entry(entry).filter(|id| !ids.contains(id))
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
        tracing::warn!(mode = %mode, "failed to reconcile persisted transport mode before RNS startup");
    }
}

/// Soft-shutdown: stop RNS/LXMF tasks without re-init. App stays open.
/// Activity reset must acknowledge before any protocol teardown begins.
pub async fn shutdown_rns_lxmf(state: &Arc<AppState>) -> Result<(), ActivityRecorderError> {
    {
        let _activity_control = state.activity_control_lock.lock().await;
        state.bump_activity_boundary_generation();
        state
            .network_log_enabled
            .store(false, std::sync::atomic::Ordering::Release);
        if let Ok(mut level) = state.network_log_level.write() {
            *level = "standard".to_string();
        }
        let status = state.activity.hard_reset().await?;
        // The worker's status notification makes recovery observable. This
        // second boundary gives frontend privacy teardown one acknowledged,
        // cross-runtime fence for the temporary one-shot legacy broadcasts.
        state.emit_to_all(
            "activity_boundary_v1",
            json!({
                "version": 1,
                "kind": "hard_reset",
                "identity_generation": state.current_identity_session_generation().to_string(),
                "capture_generation": status.ingress_generation().to_string(),
                "status": status,
            }),
        );
    }

    // Supersede any pending auto-lock timer for the session being torn down.
    state
        .hw_lock_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    state.emit_to_all("system_status", json!({"status": "stopping"}));
    #[cfg(feature = "lxst-voice")]
    voice_memo::cancel_recording(state).await.ok();
    #[cfg(feature = "lxst-voice")]
    voice::shutdown_voice_service_for_runtime_teardown(state).await;
    if let Some(channels) = state.take_channels() {
        channels.shutdown().await;
    }
    {
        let _hub_control = state.channel_hub_control_lock.lock().await;
        if let Some(channel_hub) = state.channel_hub_handle() {
            if channel_hub.shutdown().await {
                state.take_channel_hub();
            } else {
                tracing::warn!(
                    reason = "shutdown_unacknowledged",
                    "channel hub teardown did not complete before runtime shutdown"
                );
            }
        }
    }
    if let Ok(sig) = state.session_shutdown.read() {
        sig.trigger();
    }
    #[cfg(all(feature = "ble", target_os = "android"))]
    let _ = state
        .mobile_platform_bridge()
        .disconnect_ble_rnode(NativeBleRnodeDisconnect::Current);
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
        mgr.shutdown().await;
    }
    // Drain serialized delta writes and persist one final immutable
    // identity/router snapshot before dropping the manager. Received ratchets
    // and the delivery ring are already durable at their mutation seams.
    if let Err(error) = crate::lxmf_persistence::persist_current_checkpoint(state, "shutdown").await
    {
        tracing::warn!(%error, "shutdown LXMF checkpoint failed");
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
    Ok(())
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
    tracing::info!(identity = %short_id(&hash), "hardware session auto-lock timeout — locking");
    if let Err(error) = shutdown_rns_lxmf(&state).await {
        tracing::error!(
            %error,
            identity = %short_id(&hash),
            "hardware session auto-lock aborted because Activity reset failed"
        );
        return;
    }
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

/// Start the RRC hub service for the active identity. The hub identity is a
/// dedicated per-identity keypair stored beside the identity's runtime state,
/// never the operator's chat identity. Requires a running RNS session.
pub async fn start_channel_hub_service(state: &Arc<AppState>) -> bool {
    if !channel_hub::channel_hub_hosting_supported() {
        tracing::warn!(reason = "unsupported_platform", "channel hub not started");
        return false;
    }
    let hub_settings = match channel_hub::ChannelHubSettings::load(&state.db) {
        Ok(settings) => settings,
        Err(_) => {
            tracing::warn!(reason = "settings_unavailable", "channel hub not started");
            return false;
        }
    };
    if !hub_settings.enabled || !channel_hub::channel_hosting_enabled(&state.db) {
        tracing::info!(reason = "hosting_disabled", "channel hub not started");
        return false;
    }
    if let Some(existing) = state.channel_hub_handle() {
        if existing.snapshot().running {
            return true;
        }
        // A timed-out shutdown keeps its handle in the slot specifically so a
        // replacement cannot overlap it. Once its shared snapshot says
        // stopped, the stale handle is safe to retire.
        state.take_channel_hub();
    }
    let transport_tx = state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.transport_tx.clone()));
    let Some(transport_tx) = transport_tx else {
        tracing::warn!(reason = "rns_unavailable", "channel hub not started");
        return false;
    };
    let shutdown = state
        .session_shutdown
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let active_identity = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
        .await
        .expect("db task panicked")
        .and_then(|identity| {
            identity
                .get("hash")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(identity_hash) =
        active_identity.filter(|hash| helpers::is_protocol_hash_16(hash.as_str()))
    else {
        tracing::warn!(reason = "no_active_identity", "channel hub not started");
        return false;
    };
    // Always resolve from the configured data dir: taking a caller-supplied
    // root gave boot and the Start button different keyfiles, and so different
    // hub destination hashes for the same hub.
    let identity_path = channel_hub::hub_identity_path(&state.config.data_dir, &identity_hash);
    let hub_identity = match channel_hub::load_or_create_hub_identity(&identity_path) {
        Ok(identity) => identity,
        Err(_) => {
            tracing::warn!(reason = "identity_unavailable", "channel hub not started");
            return false;
        }
    };
    let operator_identity = match hex::decode(&identity_hash)
        .ok()
        .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
    {
        Some(hash) => hash,
        None => {
            tracing::warn!(reason = "invalid_operator_hash", "channel hub not started");
            return false;
        }
    };
    let config = hub_settings.runtime_config();
    match channel_hub::ChannelHubHandle::start(
        transport_tx,
        hub_identity,
        config,
        operator_identity,
        channel_hub::HubStore::new(state.db.clone(), identity_hash.clone()),
        state.emitter.clone(),
        shutdown,
        Arc::downgrade(state),
    )
    .await
    {
        Ok(handle) => {
            state.set_channel_hub(handle);
            tracing::info!("Channel hub service initialized");
            true
        }
        Err(_) => {
            tracing::warn!(reason = "start_failed", "channel hub did not start");
            false
        }
    }
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
    if preferred_identity_hash
        .as_deref()
        .is_some_and(|hash| !helpers::is_protocol_hash_16(hash))
    {
        tracing::error!(
            reason = "invalid_identifier",
            "active identity row contains an invalid identity hash"
        );
        state.set_startup_stage("error");
        return;
    }
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
        tracing::info!(identity = %short_id(&hash), kind, "identity locked — awaiting unlock secret");
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
            if let Some(preferred) = preferred_identity_hash
                .as_deref()
                .filter(|preferred| mgr.identity_hash != *preferred)
            {
                tracing::error!(
                    loaded = %short_id(&mgr.identity_hash),
                    active = %short_id(preferred),
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
                if set_result.is_err() {
                    tracing::error!(
                        reason = "set_active_failed",
                        "Failed to set active identity"
                    );
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
            tracing::error!(
                reason = "initialization_failed",
                "Failed to initialize LXMF"
            );
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
                    shared_instance_port = ratspeak_core::config::RATSPEAK_RNS_SHARED_INSTANCE_PORT,
                    instance_control_port =
                        ratspeak_core::config::RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
                    "created Ratspeak app-private Reticulum config"
                );
            }
            Ok(rns_config::RatspeakRnsPortConfigChange::Updated) => {
                tracing::info!(
                    shared_instance_port = ratspeak_core::config::RATSPEAK_RNS_SHARED_INSTANCE_PORT,
                    instance_control_port =
                        ratspeak_core::config::RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
                    "updated Ratspeak app-private Reticulum shared-instance ports"
                );
            }
            Ok(rns_config::RatspeakRnsPortConfigChange::Unchanged) => {}
            Err(_) => {
                tracing::warn!(
                    reason = "prepare_config",
                    "failed to prepare Ratspeak app-private Reticulum config"
                );
            }
        }
    }
    reconcile_persisted_transport_mode_for_startup(&state, &config_dir);
    #[cfg(target_os = "android")]
    enforce_android_single_ble_rnode_for_startup(&state, &config_dir);
    #[cfg(target_os = "android")]
    migrate_android_usb_selectors_for_startup(&state, &config_dir).await;
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
                            app_name: LXMF_DELIVERY_APP_NAME.to_string(),
                            delivery_tx: Some(delivery_tx.clone()),
                        },
                    )
                    .await
                {
                    Ok(()) => {
                        tracing::info!(
                            dest = %short_id(&hex::encode(dest_hash)),
                            "LXMF destination registered with transport"
                        );
                    }
                    Err(_) => {
                        tracing::error!(
                            reason = "registration_failed",
                            "CRITICAL: Failed to register LXMF destination — ALL inbound messages will be lost"
                        );
                    }
                }
                let (opportunistic_proof_tx, opportunistic_proof_rx) =
                    tokio::sync::mpsc::unbounded_channel();
                if let Ok(mut lxmf) = state.lxmf.lock() {
                    if let Some(mgr) = lxmf.as_mut() {
                        mgr.delivery_tx = Some(delivery_tx);
                        mgr.set_opportunistic_proof_sender(opportunistic_proof_tx);
                    }
                }

                let (pkt_tx, pkt_rx) = tokio::sync::mpsc::channel(CHANNEL_BUFFER_SIZE);
                let (link_tx, link_rx) = tokio::sync::mpsc::channel(CHANNEL_BUFFER_SIZE);
                let dispatch_dest_hash = dest_hash;
                let dispatch_shutdown = state
                    .session_shutdown
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                tokio::spawn(handle_lxmf_delivery_proofs(
                    state.clone(),
                    opportunistic_proof_rx,
                    dispatch_shutdown.clone(),
                ));
                tokio::spawn(async move {
                    loop {
                        let event = tokio::select! {
                            biased;
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
                                app_name: LXMF_PROPAGATION_APP_NAME.to_string(),
                                delivery_tx: Some(prop_tx),
                            },
                        )
                        .await
                    {
                        Ok(()) => {
                            tracing::info!(
                                dest = %short_id(&hex::encode(prop_dest_hash)),
                                "propagation destination registered with transport"
                            );
                        }
                        Err(_) => {
                            tracing::error!(
                                reason = "registration_failed",
                                "failed to register propagation destination"
                            );
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
                            Err(_) => {
                                tracing::warn!(
                                    reason = "storage_unavailable",
                                    "failed to create propagation node with storage, using in-memory"
                                );
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
                        LXMF_PROPAGATION_APP_NAME,
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
                                        LXMF_DELIVERY_APP_NAME,
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
                        tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
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
                            biased;
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
                                biased;
                                _ = store_shutdown.wait() => break,
                                item = pkt_rx.recv() => item,
                                item = res_rx.recv() => item,
                            };
                            let Some((data, _link_id)) = item else {
                                break;
                            };
                            let Ok((_timebase, entries)) =
                                lxmf_core::message_api::LxMessage::unpack_propagation_wrapper_bounded(
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
            if let Ok(mut lxmf) = state.lxmf.lock() {
                if let Some(mgr) = lxmf.as_mut() {
                    let identity_id = mgr.identity_hash.clone();
                    mgr.enable_propagation(
                        mode != propagation::PropagationMode::Off,
                        &state.db,
                        &identity_id,
                    );
                }
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
                if !stored_pn.is_empty() {
                    if let Ok(mut lxmf) = state.lxmf.lock() {
                        if let Some(mgr) = lxmf.as_mut() {
                            let identity_id = mgr.identity_hash.clone();
                            if mgr.set_propagation_node(Some(&stored_pn), &state.db, &identity_id) {
                                tracing::info!(
                                    node = %short_id(&stored_pn),
                                    "restored Manual-mode propagation node from DB"
                                );
                            }
                        }
                    }
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
                            LXMF_DELIVERY_APP_NAME,
                            signing_key,
                        );
                    let admission_state = state.clone();
                    lxmf_link_mgr
                        .set_resource_strategy(rns_runtime::prelude::ResourceStrategy::AcceptApp);
                    lxmf_link_mgr.set_resource_accept_handler(move |link_id, advertisement| {
                        admit_inbound_lxmf_resource(&admission_state, link_id, advertisement)
                    });
                    let lxmf_link_identities = lxmf_link_mgr.link_identities_handle();

                    let (link_pkt_tx, mut link_pkt_rx) =
                        tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
                    let (link_res_tx, mut link_res_rx) =
                        tokio::sync::mpsc::channel::<(Vec<u8>, [u8; 16])>(CHANNEL_BUFFER_SIZE);
                    let (link_accounting_tx, mut link_accounting_rx) =
                        tokio::sync::mpsc::unbounded_channel::<
                            rns_runtime::link_manager::LinkManagerAccountingEvent,
                        >();
                    let (link_command_tx, link_command_rx) = tokio::sync::mpsc::channel::<
                        rns_runtime::link_manager::LinkManagerCommand,
                    >(
                        CHANNEL_BUFFER_SIZE
                    );
                    let (link_identified_tx, link_identified_rx) =
                        tokio::sync::mpsc::channel::<([u8; 16], [u8; 16])>(CHANNEL_BUFFER_SIZE);
                    let (backchannel_event_tx, backchannel_event_rx) =
                        tokio::sync::mpsc::unbounded_channel::<lxmf::BackchannelLinkEvent>();
                    lxmf_link_mgr.set_link_packet_channel(link_pkt_tx.clone());
                    // Use the single-owner accounting stream instead of also
                    // installing the legacy completion channel. Installing
                    // both would clone every completed Resource Vec.
                    lxmf_link_mgr.set_accounting_event_channel(link_accounting_tx);
                    lxmf_link_mgr.set_link_identified_channel(link_identified_tx);

                    let direct_admission_state = state.clone();
                    let direct_conclusion_state = state.clone();
                    if let Ok(mut lxmf) = state.lxmf.lock() {
                        if let Some(mgr) = lxmf.as_mut() {
                            mgr.set_direct_inbound_resource_handlers(
                                move |link_id, advertisement| {
                                    admit_inbound_lxmf_resource(
                                        &direct_admission_state,
                                        link_id,
                                        advertisement,
                                    )
                                },
                                move |_link_id, resource_id| {
                                    direct_conclusion_state
                                        .complete_inbound_attachment_resource(resource_id);
                                },
                            );
                            mgr.set_lxmf_link_control(
                                link_command_tx,
                                link_pkt_tx.clone(),
                                link_identified_rx,
                                backchannel_event_rx,
                            );
                        }
                    }

                    let accounting_state = state.clone();
                    let accounting_shutdown = state
                        .session_shutdown
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone();
                    tokio::spawn(async move {
                        use rns_runtime::link_manager::{
                            LinkManagerAccountingEvent, LinkResourceConclusion,
                            LinkResourceDirection, LinkResourceEvent,
                        };
                        loop {
                            let event = tokio::select! {
                                biased;
                                _ = accounting_shutdown.wait() => break,
                                event = link_accounting_rx.recv() => match event {
                                    Some(event) => event,
                                    None => break,
                                },
                            };
                            match event {
                                LinkManagerAccountingEvent::LinkPacketProof(proof) => {
                                    let _ = backchannel_event_tx
                                        .send(lxmf::BackchannelLinkEvent::PacketProof(proof));
                                }
                                LinkManagerAccountingEvent::ResourceCompletion(completion) => {
                                    accounting_state.complete_inbound_attachment_resource(
                                        completion.resource_hash,
                                    );
                                    if link_res_tx
                                        .send((completion.data, completion.link_id))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                LinkManagerAccountingEvent::ResourceEvent(
                                    LinkResourceEvent::Concluded {
                                        resource_id,
                                        direction: LinkResourceDirection::Inbound,
                                        conclusion,
                                        ..
                                    },
                                ) if !matches!(conclusion, LinkResourceConclusion::Complete) => {
                                    accounting_state
                                        .complete_inbound_attachment_resource(resource_id);
                                }
                                LinkManagerAccountingEvent::ResourceEvent(
                                    LinkResourceEvent::Concluded {
                                        link_id,
                                        resource_id,
                                        direction: LinkResourceDirection::Outbound,
                                        conclusion,
                                    },
                                ) => {
                                    let _ = backchannel_event_tx.send(
                                        lxmf::BackchannelLinkEvent::ResourceConclusion {
                                            link_id,
                                            resource_hash: resource_id,
                                            conclusion,
                                        },
                                    );
                                }
                                LinkManagerAccountingEvent::LinkClosed { link_id } => {
                                    accounting_state.release_inbound_attachment_link(link_id);
                                    let _ = backchannel_event_tx
                                        .send(lxmf::BackchannelLinkEvent::LinkClosed { link_id });
                                }
                                _ => {}
                            }
                        }
                    });

                    let lxmf_link_shutdown = state
                        .session_shutdown
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            biased;
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
                                biased;
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
                            // Sample after the receive so a newly spawned task
                            // does not retain an odd transition fence while it
                            // waits. The immediate shutdown check prevents an
                            // old task selected before reset from borrowing the
                            // replacement session's fence.
                            let activity_origin = link_inbound_state.activity_request_fence();
                            if link_inbound_shutdown.is_triggered() {
                                break;
                            }

                            // Link deliveries arrive already decrypted. Payload
                            // is the full LXMF wire format:
                            //   [dest:16][src:16][sig:64][msgpack].
                            handle_decrypted_lxmf_from_origin(
                                &link_inbound_state,
                                data,
                                InboundLxmfSource::Link {
                                    link_id: Some(link_id),
                                    remote_identity_hash: lxmf_link_identities
                                        .lock()
                                        .ok()
                                        .and_then(|identities| identities.get(&link_id).copied()),
                                },
                                activity_origin,
                            )
                            .await;
                        }
                    });

                    tracing::info!(
                        dest = %short_id(&hex::encode(lxmf_dest_hash)),
                        "LXMF delivery LinkManager started — accepting link-based messages"
                    );
                }
            }

            // Retain one runtime handle clone before moving the manager into
            // state; exact announce subscriptions below are created through
            // this handle and own their registrations until shutdown.
            let announce_handle = rns_mgr.handle.clone();
            let channels_identity = state
                .lxmf
                .lock()
                .ok()
                .and_then(|lxmf| lxmf.as_ref().map(|manager| manager.identity.clone()));
            let channels_shutdown = state
                .session_shutdown
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let channels_transport = rns_mgr.handle.transport_tx.clone();
            let startup_rnode_activity = rns_mgr.startup_rnode_runtimes().to_vec();
            #[cfg(all(feature = "ble", target_os = "android"))]
            let deferred_android_ble_rnodes = rns_mgr.deferred_android_ble_rnodes();
            let rnode_activity_origin = state.set_rns(rns_mgr);
            if let Some(origin) = rnode_activity_origin {
                for runtime in startup_rnode_activity {
                    if !state.cover_rnode_activity_interface(runtime.interface_id, origin) {
                        continue;
                    }
                    rnode_activity::spawn_startup_rnode_activity_monitor(
                        state.clone(),
                        runtime.observer,
                        origin,
                    );
                }
            }
            #[cfg(all(feature = "ble", target_os = "android"))]
            {
                state.wait_for_mobile_platform_bridge().await;
                start_deferred_android_ble_rnode(
                    &state,
                    rnode_activity_origin,
                    deferred_android_ble_rnodes,
                );
            }
            if let Some(identity) = channels_identity {
                state.set_channels(channels::ChannelsManagerHandle::start(
                    channels_transport,
                    identity,
                    state.emitter.clone(),
                    channels_shutdown,
                    Arc::downgrade(&state),
                ));
                tracing::info!("Channels runtime initialized");
            }
            if channel_hub::channel_hub_hosting_supported() {
                let _hub_control = state.channel_hub_control_lock.lock().await;
                if channel_hub::ChannelHubSettings::load(&state.db).is_ok_and(|settings| {
                    settings.enabled && channel_hub::channel_hosting_enabled(&state.db)
                }) {
                    start_channel_hub_service(&state).await;
                }
            }
            tracing::info!("RNS runtime initialized");
            #[cfg(feature = "lxst-voice")]
            if voice::start_voice_service(&state).await.is_err() {
                tracing::warn!(
                    reason = "startup_failed",
                    "LXST voice service did not start"
                );
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
                        biased;
                        _ = tick_shutdown.wait() => break,
                        _ = interval.tick() => {},
                        _ = tick_state.lxmf_notify.notified() => {},
                    }
                    let tick_activity_origin = tick_state.activity_request_fence();
                    if tick_shutdown.is_triggered() {
                        break;
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
                            if is_fg && !was_foreground {
                                if let Ok(mut lxmf) = tick_state.lxmf.lock() {
                                    if let Some(mgr) = lxmf.as_mut() {
                                        mgr.mark_foreground_resume();
                                    }
                                }
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
                        let empty_result = || {
                            (
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                            )
                        };
                        let lock_wait_started = std::time::Instant::now();
                        let Ok(mut lxmf) = tick_state_for_lxmf.lxmf.lock() else {
                            return empty_result();
                        };
                        let Some(mgr) = lxmf.as_mut() else {
                            return empty_result();
                        };
                        let waited = lock_wait_started.elapsed();
                        if waited > Duration::from_secs(1) {
                            tracing::warn!(
                                waited_ms = waited.as_millis() as u64,
                                "lxmf tick waited on manager lock"
                            );
                        }
                        let hold_started = std::time::Instant::now();
                        let results = mgr
                            .tick_with_auto_propagation_download_ready(auto_inbox_download_ready);
                        let tick_held = hold_started.elapsed();
                        if tick_held > Duration::from_secs(1) {
                            tracing::warn!(
                                held_ms = tick_held.as_millis() as u64,
                                "lxmf tick held manager lock (tick body)"
                            );
                        }
                        let delivery_progress = mgr.take_delivery_progress_updates();
                        let delivery_failures = mgr.take_delivery_failure_updates();
                        let downloaded = mgr.take_downloaded_propagation_messages();
                        let (completed_deposits, failed_deposits, completed_syncs, failed_syncs) =
                            mgr.take_propagation_health();
                        let expired_received_ratchets = mgr.take_expired_received_ratchets();
                        (
                            results,
                            delivery_progress,
                            delivery_failures,
                            downloaded,
                            completed_deposits,
                            failed_deposits,
                            completed_syncs,
                            failed_syncs,
                            expired_received_ratchets,
                        )
                    })
                    .await;
                    let (
                        results,
                        delivery_progress,
                        delivery_failures,
                        downloaded_propagation_messages,
                        completed_propagation_deposits,
                        failed_propagation_deposits,
                        completed_propagation_syncs,
                        failed_propagation_syncs,
                        expired_received_ratchets,
                    ) = match tick_result {
                        Ok(result) => result,
                        Err(_) => {
                            tracing::error!(
                                reason = "worker_failed",
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
                                Vec::new(),
                                Vec::new(),
                            )
                        }
                    };
                    if !expired_received_ratchets.is_empty() {
                        let cleanup_state = tick_state.clone();
                        tokio::spawn(async move {
                            if let Err(error) =
                                crate::lxmf_persistence::delete_expired_received_ratchets(
                                    &cleanup_state,
                                    &expired_received_ratchets,
                                )
                                .await
                            {
                                tracing::warn!(%error, "received-ratchet cleanup failed");
                            }
                        });
                    }
                    // Capture and persist the current identity/router checkpoint
                    // outside the protocol-manager lock. Changed received
                    // ratchets are already write-through deltas, so the
                    // periodic pass never replays thousands of unchanged files.
                    if should_save_crypto_state {
                        let checkpoint_state = tick_state.clone();
                        tokio::spawn(async move {
                            if let Err(error) = crate::lxmf_persistence::persist_current_checkpoint(
                                &checkpoint_state,
                                "periodic",
                            )
                            .await
                            {
                                tracing::warn!(
                                    %error,
                                    "periodic LXMF checkpoint failed"
                                );
                            }
                        });
                    }
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
                        if let Some(node) = hosted_node {
                            if let Ok(mut node) = node.lock() {
                                node.tick();
                            }
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
                                node = %short_id(&hex::encode(node)),
                                reason = "offline",
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
                                node = %short_id(&hex::encode(node)),
                                reason = "offline",
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
                    let mut persisted: Vec<(
                        String,
                        &'static str,
                        Option<String>,
                        Option<lxmf::LxmfDeliveryFailureUpdate>,
                    )> = Vec::with_capacity(results.len());
                    for (msg_id, new_state) in &results {
                        if matches!(
                            *new_state,
                            "delivered" | "propagated" | "rejected" | "failed"
                        ) {
                            tick_state.release_attachment_delivery_lease(msg_id);
                        }
                        let failure = delivery_failures
                            .iter()
                            .find(|failure| failure.msg_id == *msg_id)
                            .cloned();
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
                            let updated = db::update_message_state(
                                &p,
                                &msg_id_for_db,
                                &identity_for_db,
                                &new_state_for_db,
                                None,
                            );
                            let method = db::get_message_delivery_method(
                                &p,
                                &msg_id_for_db,
                                &identity_for_db,
                            );
                            (updated, method)
                        })
                        .await
                        {
                            Ok((true, method)) => {
                                persisted.push((msg_id.clone(), *new_state, method, failure))
                            }
                            Ok((false, _)) => tracing::debug!(
                                msg_id = %short_id(msg_id),
                                new_state = %new_state,
                                reason = "terminal_state_preserved",
                                "lxmf_tick: suppressed a late state regression"
                            ),
                            Err(_) => tracing::error!(
                                msg_id = %short_id(msg_id),
                                new_state = %new_state,
                                reason = "persist_failed",
                                "lxmf_tick: persist failed; skipping emit"
                            ),
                        }
                    }
                    for (msg_id, new_state, method, failure) in &persisted {
                        let client_msg_id = tick_state
                            .msg_id_map
                            .lock()
                            .ok()
                            .and_then(|map| map.get(msg_id).cloned());
                        let step_payload = if let Some(failure) = failure {
                            json!({
                                "step": "error",
                                "code": failure.code,
                                "message": "Message exceeds propagation node limit",
                                "actual_bytes": failure.actual_bytes,
                                "limit_bytes": failure.limit_bytes,
                                "msg_id": msg_id,
                                "client_msg_id": client_msg_id,
                                "method": method,
                            })
                        } else {
                            json!({
                                "step": new_state,
                                "msg_id": msg_id,
                                "client_msg_id": client_msg_id,
                                "method": method,
                            })
                        };
                        tick_state.emit_to_all("lxmf_step", step_payload);
                        let activity_state = match *new_state {
                            "routing" => Some(producer::LxmfDeliveryState::Routing),
                            "propagating" => Some(producer::LxmfDeliveryState::Propagating),
                            "reusing_backchannel" => {
                                Some(producer::LxmfDeliveryState::ReusingBackchannel)
                            }
                            "sending_via_link" => Some(producer::LxmfDeliveryState::SendingViaLink),
                            "sent" => Some(producer::LxmfDeliveryState::Sent),
                            "delivered" => Some(producer::LxmfDeliveryState::Delivered),
                            "propagated" => Some(producer::LxmfDeliveryState::Propagated),
                            "rejected" => Some(producer::LxmfDeliveryState::Rejected),
                            "failed" => Some(producer::LxmfDeliveryState::Failed),
                            _ => None,
                        };
                        if let Some(activity_state) = activity_state.filter(|_| {
                            !lxmf_progress_supersedes_state(&delivery_progress, msg_id, new_state)
                        }) {
                            record_activity_if_current(&tick_state, tick_activity_origin, || {
                                let message = producer::MessageId::from_hex(msg_id)?;
                                if let Some(failure) = failure {
                                    return Ok(producer::lxmf_propagation_limit_exceeded(
                                        producer::LxmfPropagationLimitExceeded {
                                            message,
                                            encoded_bytes: failure.actual_bytes as u64,
                                            max_message_bytes: failure.limit_bytes as u64,
                                        },
                                    ));
                                }
                                let method = method
                                    .as_deref()
                                    .and_then(producer::LxmfDeliveryMethod::from_code);
                                Ok(producer::lxmf_delivery_state_changed(
                                    producer::LxmfDeliveryStateChanged {
                                        message,
                                        state: activity_state,
                                        method,
                                        rtt_ms: None,
                                        failure_reason: match activity_state {
                                            producer::LxmfDeliveryState::Rejected => {
                                                Some(producer::DeliveryFailureReason::Rejected)
                                            }
                                            producer::LxmfDeliveryState::Failed => Some(
                                                producer::DeliveryFailureReason::TransportFailed,
                                            ),
                                            _ => None,
                                        },
                                    },
                                ))
                            });
                        }
                        update_message_delivery_timeout(&tick_state, msg_id, new_state);

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
                            if *new_state == "delivered"
                                || *new_state == "failed"
                                || *new_state == "propagated"
                            {
                                if let Ok(mut map) = tick_state.lrgp_msg_to_session.lock() {
                                    map.remove(msg_id);
                                }
                            }
                        }
                    }

                    for update in delivery_progress {
                        // Progress can be the only observable output when a
                        // recovered/reused Link already owns the delivery.
                        // Start the same bounded clock as ordinary state
                        // updates so a stalled Resource can never remain
                        // pending indefinitely.
                        update_message_delivery_timeout(&tick_state, &update.msg_id, update.step);
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
                        record_lxmf_progress(&tick_state, tick_activity_origin, &update);
                    }

                    for data in downloaded_propagation_messages {
                        handle_decrypted_lxmf_from_origin(
                            &tick_state,
                            data,
                            InboundLxmfSource::Propagated,
                            tick_activity_origin,
                        )
                        .await;
                    }

                    // Check delivery deadlines every ~5s. Resource progress can
                    // otherwise cross its three-minute deadline just after a
                    // coarse maintenance pass and remain apparently pending
                    // for another 30 seconds.
                    timeout_check_counter += 1;
                    if timeout_check_counter.is_multiple_of(10) {
                        check_message_timeouts(&tick_state, tick_activity_origin).await;
                    }
                    // Every ~30s: slower discovery maintenance and retention.
                    if timeout_check_counter.is_multiple_of(60) {
                        propagation::reconcile_active_auto_node(&tick_state).await;
                        propagation::probe_static_nodes_background(&tick_state).await;
                        sweep_stale_game_deliveries(&tick_state).await;
                        let cleanup_now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64();
                        let cutoff = cleanup_now - 3600.0;
                        if let Some(mut times) = tick_state
                            .message_send_times
                            .lock()
                            .ok()
                            .filter(|times| times.len() > 200)
                        {
                            times.retain(|_, &mut t| t > cutoff);
                        }
                        if let Some(mut map) = tick_state
                            .msg_id_map
                            .lock()
                            .ok()
                            .filter(|map| map.len() > 200)
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
                            biased;
                            _ = periodic_shutdown.wait() => break,
                            _ = announce_rx.changed() => continue,
                        }
                    } else {
                        tokio::select! {
                            biased;
                            _ = periodic_shutdown.wait() => break,
                            _ = announce_rx.changed() => continue,
                            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => {
                                let activity_origin =
                                    periodic_state.activity_request_fence();
                                if periodic_shutdown.is_triggered() {
                                    break;
                                }
                                send_typed_announce_from_origin(
                                    &periodic_state,
                                    AnnounceOrigin::Periodic,
                                    activity_origin,
                                )
                                .await;
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
            let poll_activity_origin = state.activity_request_fence();
            tokio::spawn(async move {
                poll_stats_loop(poll_state, poll_shutdown, poll_activity_origin).await;
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
                    &announce_handle,
                    shutdown.clone(),
                )
                .await;
                announce_handlers::spawn_lxmf_propagation_handler(
                    state.clone(),
                    &announce_handle,
                    shutdown.clone(),
                )
                .await;
                announce_handlers::spawn_lxst_telephony_handler(
                    state.clone(),
                    &announce_handle,
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
        Err(_) => {
            tracing::warn!(reason = "initialization_failed", "Failed to initialize RNS");
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

#[cfg(target_os = "android")]
fn enforce_android_single_ble_rnode_for_startup(state: &AppState, config_dir: &std::path::Path) {
    let disabled = {
        let _config_guard = state
            .rns_config_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut enabled =
            rns_config::enabled_rnode_names_with_port_prefix(config_dir, "ble://").into_iter();
        let _selected = enabled.next();
        enabled
            .filter(|name| rns_config::set_interface_enabled(config_dir, name, false))
            .count()
    };
    if disabled > 0 {
        state.publish_mobile_hardware_state(
            "ble_rnode",
            "conflict",
            Some("multiple_configured_radios"),
        );
        tracing::warn!(
            disabled,
            reason = "multiple_android_ble_rnodes",
            "paused additional Android BLE RNodes to preserve one native radio owner"
        );
    }
}

#[cfg(target_os = "android")]
async fn migrate_android_usb_selectors_for_startup(state: &AppState, config_dir: &std::path::Path) {
    let candidates = {
        let _config_guard = state
            .rns_config_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        rns_config::android_usb_selector_migration_candidates(config_dir)
    };
    for candidate in candidates {
        let requested = rns_interface::android_usb::AndroidUsbDeviceSelector {
            device_name: candidate.device_name.clone(),
            vendor_id: candidate.vendor_id,
            product_id: candidate.product_id,
            serial_number: candidate.serial_number.clone(),
        };
        let Ok(resolved) =
            rns_interface::android_usb::resolve_android_usb_device_selector(&requested).await
        else {
            continue;
        };
        let (Some(vendor_id), Some(product_id)) = (resolved.vendor_id, resolved.product_id) else {
            continue;
        };
        let outcome = {
            let _config_guard = state
                .rns_config_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            rns_config::apply_android_usb_selector_migration(
                config_dir,
                &candidate,
                vendor_id,
                product_id,
                resolved.serial_number.as_deref(),
            )
        };
        match outcome {
            rns_config::InterfaceBlockCasOutcome::Applied => tracing::info!(
                interface = candidate.name,
                "saved stable Android USB radio identity"
            ),
            rns_config::InterfaceBlockCasOutcome::Stale => tracing::info!(
                interface = candidate.name,
                "skipped Android USB identity migration after concurrent config change"
            ),
            rns_config::InterfaceBlockCasOutcome::NotFound
            | rns_config::InterfaceBlockCasOutcome::WriteFailed => tracing::warn!(
                interface = candidate.name,
                reason = "selector_persist_failed",
                "could not save Android USB radio identity"
            ),
        }
    }
}

fn interface_stats_have_online_egress(interfaces: &[Value]) -> bool {
    interfaces.iter().any(|interface| {
        if !interface
            .get("online")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return false;
        }

        // The app-private shared-instance listener and its accepted local
        // clients are process plumbing, not paths to another Reticulum node.
        // A SharedInstancePeer is different: it reaches an external daemon
        // and therefore is a valid egress path.
        !matches!(
            interface.get("role").and_then(Value::as_str),
            Some("shared_server" | "local_client")
        )
    })
}

#[cfg(test)]
mod interface_availability_tests {
    use super::interface_stats_have_online_egress;
    use serde_json::json;

    #[test]
    fn internal_shared_listener_is_not_network_egress() {
        let interfaces = vec![
            json!({"online": true, "role": "shared_server"}),
            json!({"online": true, "role": "local_client"}),
            json!({"online": false, "role": "normal"}),
        ];

        assert!(!interface_stats_have_online_egress(&interfaces));
    }

    #[test]
    fn physical_and_shared_instance_peer_interfaces_are_network_egress() {
        assert!(interface_stats_have_online_egress(&[json!({
            "online": true,
            "role": "normal",
        })]));
        assert!(interface_stats_have_online_egress(&[json!({
            "online": true,
            "role": "shared_instance_peer",
        })]));
    }

    #[test]
    fn missing_legacy_role_remains_eligible() {
        assert!(interface_stats_have_online_egress(&[json!({
            "online": true,
        })]));
    }
}

/// `None` until the first poll completes; callers should allow the attempt.
pub fn any_interface_online_cached(state: &AppState) -> Option<bool> {
    let guard = state.last_stats.read().ok()?;
    let stats = guard.as_ref()?;
    let arr = stats
        .get("interface_stats")?
        .get("interfaces")?
        .as_array()?;
    Some(interface_stats_have_online_egress(arr))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AnnounceSendDisposition {
    Queued,
    AlreadyQueued,
    #[default]
    Deferred,
    Failed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnnounceSendReport {
    pub packets: usize,
    pub queued: usize,
    pub failed: usize,
    pub disposition: AnnounceSendDisposition,
    pub correlation_id: u64,
    lxmf_delivery_queued: bool,
    propagation_queued: bool,
    lxst_queued: bool,
}

struct AnnounceBurstExecution {
    report: AnnounceSendReport,
    activity_recorded: bool,
}

pub async fn send_announce_from_state(state: &Arc<AppState>) -> AnnounceSendReport {
    let activity_origin = state.activity_request_fence();
    submit_announce_intent(state, AnnounceOrigin::Periodic, true, activity_origin).await
}

pub async fn send_manual_announce_from_state(state: &Arc<AppState>) -> AnnounceSendReport {
    let activity_origin = state.activity_request_fence();
    submit_announce_intent(state, AnnounceOrigin::Manual, false, activity_origin).await
}

pub async fn send_manual_announce_from_origin(
    state: &Arc<AppState>,
    activity_origin: ActivityRequestFence,
) -> AnnounceSendReport {
    submit_announce_intent(state, AnnounceOrigin::Manual, false, activity_origin).await
}

pub async fn send_announce_from_origin(
    state: &Arc<AppState>,
    activity_origin: ActivityRequestFence,
) -> AnnounceSendReport {
    submit_announce_intent(state, AnnounceOrigin::Periodic, true, activity_origin).await
}

pub async fn send_typed_announce_from_origin(
    state: &Arc<AppState>,
    origin: AnnounceOrigin,
    activity_origin: ActivityRequestFence,
) -> AnnounceSendReport {
    submit_announce_intent(state, origin, true, activity_origin).await
}

pub async fn maybe_opportunistic_announce_before_user_send(
    state: &Arc<AppState>,
    dest_hash: &str,
) -> AnnounceSendReport {
    let activity_origin = state.activity_request_fence();
    maybe_opportunistic_announce_before_user_send_from_origin(state, dest_hash, activity_origin)
        .await
}

pub async fn maybe_opportunistic_announce_before_user_send_from_origin(
    state: &Arc<AppState>,
    dest_hash: &str,
    activity_origin: ActivityRequestFence,
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
    let announce_report =
        send_typed_announce_from_origin(state, AnnounceOrigin::Opportunistic, activity_origin)
            .await;
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
                biased;
                _ = shutdown.wait() => return,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
            // Startup is scheduled while the identity transition fence can be
            // odd. Sample only after this wake, then reject an old task whose
            // shutdown raced the selected timer before doing any work.
            let activity_origin = state.activity_request_fence();
            if shutdown.is_triggered() {
                return;
            }

            if *state.announce_interval_rx.borrow() == 0 {
                return;
            }

            if matches!(any_interface_online_cached(&state), Some(true)) {
                let report = send_typed_announce_from_origin(
                    &state,
                    AnnounceOrigin::Startup,
                    activity_origin,
                )
                .await;
                if !matches!(report.disposition, AnnounceSendDisposition::Failed) {
                    tracing::info!(
                        correlation_id = report.correlation_id,
                        disposition = ?report.disposition,
                        "startup auto-announce admitted"
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

async fn submit_announce_intent(
    state: &Arc<AppState>,
    origin: AnnounceOrigin,
    require_cached_online: bool,
    activity_origin: ActivityRequestFence,
) -> AnnounceSendReport {
    let intent = AnnounceIntent {
        origin,
        revisions: state.announce_semantic_revision(),
    };
    let admission = match state.announce_coordinator.lock() {
        Ok(mut coordinator) => coordinator.admit(intent, Instant::now()),
        Err(_) => {
            return AnnounceSendReport {
                failed: 1,
                disposition: AnnounceSendDisposition::Failed,
                ..AnnounceSendReport::default()
            };
        }
    };

    let correlation_id = match admission {
        AnnounceAdmission::AlreadyQueued { correlation_id } => {
            tracing::info!(
                correlation_id,
                origin = origin.as_str(),
                "presence announce request already covered"
            );
            return AnnounceSendReport {
                disposition: AnnounceSendDisposition::AlreadyQueued,
                correlation_id,
                ..AnnounceSendReport::default()
            };
        }
        AnnounceAdmission::Deferred { correlation_id } => {
            tracing::info!(
                correlation_id,
                origin = origin.as_str(),
                "presence announce request deferred to semantic follow-up"
            );
            return AnnounceSendReport {
                disposition: AnnounceSendDisposition::Deferred,
                correlation_id,
                ..AnnounceSendReport::default()
            };
        }
        AnnounceAdmission::Lead { correlation_id } => correlation_id,
    };

    // Admission is the synchronous boundary. Packet construction, transport
    // queueing, semantic follow-ups and Activity all belong to this one
    // background lifecycle owner; an IPC caller must never wait for LXMF.
    let lifecycle_state = Arc::clone(state);
    tokio::spawn(async move {
        run_announce_lifecycle(
            lifecycle_state,
            correlation_id,
            require_cached_online,
            activity_origin,
        )
        .await;
    });

    AnnounceSendReport {
        disposition: AnnounceSendDisposition::Queued,
        correlation_id,
        ..AnnounceSendReport::default()
    }
}

async fn run_announce_lifecycle(
    state: Arc<AppState>,
    correlation_id: u64,
    require_cached_online: bool,
    activity_origin: ActivityRequestFence,
) {
    let mut current_correlation_id = correlation_id;
    let mut current_activity_origin = activity_origin;
    loop {
        let leadership = state
            .announce_coordinator
            .lock()
            .ok()
            .and_then(|coordinator| coordinator.leadership(current_correlation_id));
        let Some(leadership) = leadership else {
            tracing::warn!(
                correlation_id = current_correlation_id,
                "presence announce lifecycle lost coordinator leadership"
            );
            return;
        };

        let execution = if state.is_current_activity_origin_fence(current_activity_origin)
            && leadership.revisions.identity == state.current_identity_session_generation()
        {
            execute_announce_burst(
                &state,
                require_cached_online,
                current_activity_origin,
                &leadership,
            )
            .await
        } else {
            tracing::info!(
                correlation_id = leadership.correlation_id,
                "stale presence announce suppressed"
            );
            AnnounceBurstExecution {
                report: AnnounceSendReport {
                    disposition: AnnounceSendDisposition::Failed,
                    correlation_id: leadership.correlation_id,
                    ..AnnounceSendReport::default()
                },
                activity_recorded: false,
            }
        };
        let report = execution.report;
        // Capture origins merged while the builder/transport work was in
        // progress before recording the lifecycle's single public entry.
        let completed_leadership = state
            .announce_coordinator
            .lock()
            .ok()
            .and_then(|coordinator| coordinator.leadership(leadership.correlation_id))
            .unwrap_or(leadership);
        if !execution.activity_recorded {
            record_presence_lifecycle_activity(
                &state,
                current_activity_origin,
                &completed_leadership,
                &report,
            );
        }

        let success = matches!(report.disposition, AnnounceSendDisposition::Queued);
        let follow_up = state
            .announce_coordinator
            .lock()
            .ok()
            .and_then(|mut coordinator| {
                coordinator.finish(completed_leadership.correlation_id, success, Instant::now())
            });
        let Some(follow_up) = follow_up else {
            break;
        };

        // Delivery ratchets intentionally coalesce identical wall-clock
        // announce material. A semantic follow-up must cross that boundary
        // before building its new complete presence bundle.
        tokio::time::sleep(Duration::from_millis(1_050)).await;
        current_correlation_id = follow_up.correlation_id;
        current_activity_origin = state.activity_request_fence();
    }
}

fn announce_activity_method(origins: &[AnnounceOrigin]) -> producer::AnnounceMethod {
    if origins.len() != 1 {
        return producer::AnnounceMethod::Coordinated;
    }
    match origins[0] {
        AnnounceOrigin::Manual => producer::AnnounceMethod::Manual,
        AnnounceOrigin::Startup => producer::AnnounceMethod::Startup,
        AnnounceOrigin::Periodic => producer::AnnounceMethod::Periodic,
        AnnounceOrigin::InterfaceOnline => producer::AnnounceMethod::InterfaceOnline,
        AnnounceOrigin::Opportunistic => producer::AnnounceMethod::Opportunistic,
        AnnounceOrigin::IdentityChanged => producer::AnnounceMethod::IdentityChanged,
        AnnounceOrigin::ProfileChanged => producer::AnnounceMethod::ProfileChanged,
        AnnounceOrigin::PropagationChanged => producer::AnnounceMethod::PropagationChanged,
    }
}

fn announce_activity_components(report: &AnnounceSendReport) -> producer::AnnounceComponents {
    match (report.propagation_queued, report.lxst_queued) {
        (false, false) => producer::AnnounceComponents::LxmfDelivery,
        (false, true) => producer::AnnounceComponents::LxmfDeliveryAndLxst,
        (true, false) => producer::AnnounceComponents::LxmfDeliveryAndPropagation,
        (true, true) => producer::AnnounceComponents::LxmfDeliveryPropagationAndLxst,
    }
}

fn record_presence_lifecycle_activity(
    state: &AppState,
    activity_origin: ActivityRequestFence,
    leadership: &AnnounceLeadership,
    report: &AnnounceSendReport,
) {
    let Some(event) = presence_lifecycle_activity_event(leadership, report) else {
        return;
    };
    record_activity_if_current(state, activity_origin, || Ok(event));
}

/// Record while `identity_switch_lock` is held. This is the final wire-send
/// ownership seam: the same fence that authorizes transport admission also
/// authorizes the one correlated Activity result before the lock is released.
fn record_presence_lifecycle_activity_after_identity_lock(
    state: &AppState,
    activity_origin: ActivityRequestFence,
    leadership: &AnnounceLeadership,
    report: &AnnounceSendReport,
) {
    let Some(event) = presence_lifecycle_activity_event(leadership, report) else {
        return;
    };
    let _ = state.activity.record_event_fenced(
        || state.is_current_activity_request_fence_after_identity_lock(activity_origin),
        || Ok(event),
    );
}

fn record_current_presence_lifecycle_activity_after_identity_lock(
    state: &AppState,
    activity_origin: ActivityRequestFence,
    leadership: &AnnounceLeadership,
    report: &AnnounceSendReport,
) {
    let completed_leadership = state
        .announce_coordinator
        .lock()
        .ok()
        .and_then(|coordinator| coordinator.leadership(leadership.correlation_id))
        .unwrap_or_else(|| leadership.clone());
    record_presence_lifecycle_activity_after_identity_lock(
        state,
        activity_origin,
        &completed_leadership,
        report,
    );
}

fn presence_lifecycle_activity_event(
    leadership: &AnnounceLeadership,
    report: &AnnounceSendReport,
) -> Option<ProducerEvent> {
    if report.disposition == AnnounceSendDisposition::AlreadyQueued {
        return None;
    }
    let transition = if report.disposition == AnnounceSendDisposition::Queued
        && report.lxmf_delivery_queued
        && report.failed == 0
    {
        producer::RnsAnnounceTransition::Queued {
            method: announce_activity_method(&leadership.origins),
            components: announce_activity_components(report),
            count: report.queued as u64,
            correlation_id: leadership.activity_correlation_id,
        }
    } else {
        producer::RnsAnnounceTransition::Failed {
            method: announce_activity_method(&leadership.origins),
            reason: if report.failed > 0 {
                producer::AnnounceFailureReason::QueueFailed
            } else {
                producer::AnnounceFailureReason::NotReady
            },
        }
    };
    Some(producer::rns_announce_activity(
        producer::RnsAnnounceActivity {
            transition,
            interface: None,
        },
    ))
}

type PresenceAnnouncePacket = ([u8; 16], Vec<u8>, bool);

enum PresencePacketBuildAttempt {
    Built {
        packets: Vec<PresenceAnnouncePacket>,
        delivery_coalesced: bool,
        delivery_failed: bool,
    },
    Busy,
    Poisoned,
}

fn try_build_presence_announce_packets(
    state: &AppState,
    correlation_id: u64,
) -> PresencePacketBuildAttempt {
    let mut lxmf = match state.lxmf.try_lock() {
        Ok(lxmf) => lxmf,
        Err(std::sync::TryLockError::WouldBlock) => return PresencePacketBuildAttempt::Busy,
        Err(std::sync::TryLockError::Poisoned(_)) => {
            return PresencePacketBuildAttempt::Poisoned;
        }
    };
    let mut packets = Vec::new();
    let mut delivery_coalesced = false;
    let mut delivery_failed = false;
    if let Some(mgr) = lxmf.as_mut() {
        let propagation_packet = if state
            .propagation_node_hosting_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            match mgr.create_propagation_announce_packet() {
                Ok(raw) => Some((mgr.propagation_dest_hash, raw, false)),
                Err(error) => {
                    delivery_failed = true;
                    tracing::warn!(
                        correlation_id,
                        %error,
                        "propagation announce component build failed"
                    );
                    None
                }
            }
        } else {
            None
        };
        // Build the delivery component last because its ratchet owns
        // wall-clock coalescing state. A failed sibling build must not consume
        // that ratchet when no bundle can be admitted.
        if !delivery_failed {
            match mgr.create_coordinated_announce_packet() {
                Ok(raw) => {
                    packets.push((mgr.lxmf_dest_hash, raw, true));
                    packets.extend(propagation_packet);
                }
                Err(lxmf::CoordinatedDeliveryAnnounceError::Coalesced) => {
                    delivery_coalesced = true;
                }
                Err(lxmf::CoordinatedDeliveryAnnounceError::Failed(error)) => {
                    delivery_failed = true;
                    tracing::warn!(
                        correlation_id,
                        %error,
                        "LXMF delivery announce component build failed"
                    );
                }
            }
        }
    }
    PresencePacketBuildAttempt::Built {
        packets,
        delivery_coalesced,
        delivery_failed,
    }
}

async fn execute_announce_burst(
    state: &AppState,
    require_cached_online: bool,
    activity_origin: ActivityRequestFence,
    leadership: &AnnounceLeadership,
) -> AnnounceBurstExecution {
    let mut report = AnnounceSendReport {
        correlation_id: leadership.correlation_id,
        ..AnnounceSendReport::default()
    };
    let origin_set = leadership
        .origins
        .iter()
        .map(|origin| origin.as_str())
        .collect::<Vec<_>>()
        .join(",");
    tracing::info!(
        correlation_id = leadership.correlation_id,
        origins = %origin_set,
        identity_revision = leadership.revisions.identity,
        content_revision = leadership.revisions.content,
        interface_revision = leadership.revisions.interface,
        "presence announce burst started"
    );
    if require_cached_online && matches!(any_interface_online_cached(state), Some(false)) {
        tracing::warn!("announce skipped: no interfaces online");
        report.failed = 1;
        report.disposition = AnnounceSendDisposition::Failed;
        return AnnounceBurstExecution {
            report,
            activity_recorded: false,
        };
    }
    let lock_deadline = std::time::Instant::now() + ANNOUNCE_LXMF_BUILD_RETRY_WINDOW;
    let (packets, delivery_failed) = loop {
        if !state.is_current_activity_origin_fence(activity_origin) {
            tracing::info!(
                correlation_id = leadership.correlation_id,
                "stale presence announce suppressed while waiting for LXMF"
            );
            break (Vec::new(), true);
        }
        match try_build_presence_announce_packets(state, leadership.correlation_id) {
            PresencePacketBuildAttempt::Built {
                packets,
                delivery_coalesced: false,
                delivery_failed,
            } => break (packets, delivery_failed),
            PresencePacketBuildAttempt::Built {
                delivery_coalesced: true,
                ..
            } if std::time::Instant::now() < lock_deadline => {
                // The delivery ratchet commits before returning packet bytes.
                // Coalescing therefore proves only a same-second build, never
                // prior transport acceptance. Retry into the next wall-clock
                // interval instead of promoting it to false success.
                tokio::time::sleep(ANNOUNCE_LXMF_BUILD_RETRY_INTERVAL).await;
            }
            PresencePacketBuildAttempt::Built {
                delivery_coalesced: true,
                ..
            } => {
                tracing::warn!(
                    correlation_id = leadership.correlation_id,
                    "presence announce retry window expired on delivery ratchet coalescing"
                );
                break (Vec::new(), true);
            }
            PresencePacketBuildAttempt::Busy if std::time::Instant::now() < lock_deadline => {
                tokio::time::sleep(ANNOUNCE_LXMF_BUILD_RETRY_INTERVAL).await;
            }
            PresencePacketBuildAttempt::Busy => {
                tracing::warn!(
                    correlation_id = leadership.correlation_id,
                    "presence announce retry window expired while the LXMF manager was busy"
                );
                break (Vec::new(), true);
            }
            PresencePacketBuildAttempt::Poisoned => {
                tracing::warn!(
                    correlation_id = leadership.correlation_id,
                    "presence announce failed because the LXMF manager lock is unavailable"
                );
                break (Vec::new(), true);
            }
        }
    };
    report.packets = packets.len();

    if delivery_failed || !packets.iter().any(|(_, _, is_delivery)| *is_delivery) {
        report.failed = 1;
        report.disposition = AnnounceSendDisposition::Failed;
        return AnnounceBurstExecution {
            report,
            activity_recorded: false,
        };
    }

    // Bind the prepared bytes and their final transport admission to the same
    // exact identity/runtime lifecycle span. This covers same-identity RNS
    // replacement as well as identity switches, and the guard remains held
    // through the bounded channel admissions and correlated Activity record.
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    if !state.is_current_activity_request_fence_after_identity_lock(activity_origin) {
        tracing::info!(
            correlation_id = leadership.correlation_id,
            "stale presence announce suppressed before transport admission"
        );
        report.failed = 1;
        report.disposition = AnnounceSendDisposition::Failed;
        return AnnounceBurstExecution {
            report,
            activity_recorded: false,
        };
    }
    if matches!(any_interface_online_cached(state), Some(false)) {
        tracing::warn!(
            correlation_id = leadership.correlation_id,
            "announce skipped after retry because no interface remains online"
        );
        report.failed = 1;
        report.disposition = AnnounceSendDisposition::Failed;
        record_current_presence_lifecycle_activity_after_identity_lock(
            state,
            activity_origin,
            leadership,
            &report,
        );
        return AnnounceBurstExecution {
            report,
            activity_recorded: true,
        };
    }
    let transport_tx = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.transport_tx.clone()));
    let Some(tx) = transport_tx else {
        report.failed = 1;
        report.disposition = AnnounceSendDisposition::Failed;
        record_current_presence_lifecycle_activity_after_identity_lock(
            state,
            activity_origin,
            leadership,
            &report,
        );
        return AnnounceBurstExecution {
            report,
            activity_recorded: true,
        };
    };

    for (destination_hash, raw, is_lxmf_delivery) in packets {
        let packet_len = raw.len();
        let fingerprint = hex::encode(&rns_crypto::sha::sha256(&raw)[..6]);
        let (dispatch_tx, dispatch_rx) = tokio::sync::oneshot::channel();
        let admitted = tokio::time::timeout(
            ANNOUNCE_QUEUE_ADMISSION_WAIT,
            tx.send(rns_transport::messages::TransportMessage::SendPacket {
                request: rns_transport::messages::OutboundRequest {
                    raw: Bytes::from(raw),
                    destination_hash,
                },
                attached_interface: None,
                receipt: None,
                result_tx: dispatch_tx,
            }),
        )
        .await;
        let dispatch = if matches!(admitted, Ok(Ok(()))) {
            tokio::time::timeout(ANNOUNCE_INTERFACE_DISPATCH_WAIT, dispatch_rx)
                .await
                .ok()
                .and_then(Result::ok)
        } else {
            None
        };
        match dispatch {
            Some(rns_transport::messages::OutboundDispatchResult::Sent) => {
                report.queued += 1;
                if is_lxmf_delivery {
                    report.lxmf_delivery_queued = true;
                    state
                        .last_lxmf_delivery_announce_at_ms
                        .store(unix_now_ms(), Ordering::Relaxed);
                } else {
                    report.propagation_queued = true;
                }
                tracing::info!(
                    correlation_id = leadership.correlation_id,
                    dest = %short_id(&hex::encode(destination_hash)),
                    packet_len,
                    packet_fingerprint = %fingerprint,
                    "announce accepted by Reticulum interface layer"
                );
            }
            Some(rns_transport::messages::OutboundDispatchResult::NoInterface) => {
                report.failed += 1;
                tracing::warn!(
                    correlation_id = leadership.correlation_id,
                    reason = "no_interface",
                    dest = %short_id(&hex::encode(destination_hash)),
                    packet_len,
                    packet_fingerprint = %fingerprint,
                    "No Reticulum interface accepted announce"
                );
                if is_lxmf_delivery {
                    break;
                }
            }
            Some(rns_transport::messages::OutboundDispatchResult::ReceiptCollision) | None => {
                report.failed += 1;
                tracing::warn!(
                    correlation_id = leadership.correlation_id,
                    reason = "dispatch_failed",
                    dest = %short_id(&hex::encode(destination_hash)),
                    packet_len,
                    packet_fingerprint = %fingerprint,
                    "Failed to dispatch announce to Reticulum interface layer"
                );
                if is_lxmf_delivery {
                    // No sibling presence component may be admitted without
                    // the delivery component that binds it to this identity.
                    break;
                }
            }
        }
    }

    #[cfg(feature = "lxst-voice")]
    if report.lxmf_delivery_queued {
        match tokio::time::timeout(
            ANNOUNCE_QUEUE_ADMISSION_WAIT,
            voice::announce_if_running(state),
        )
        .await
        {
            Ok(Ok(true)) => {
                report.packets += 1;
                report.queued += 1;
                report.lxst_queued = true;
                tracing::info!("LXST telephony announce queued");
            }
            Ok(Ok(false)) => {
                tracing::debug!("LXST telephony announce skipped: voice service is not running");
            }
            Ok(Err(_)) | Err(_) => {
                report.packets += 1;
                report.failed += 1;
                tracing::warn!(
                    reason = "queue_failed",
                    "Failed to queue LXST telephony announce"
                );
            }
        }
    } else {
        tracing::debug!(
            correlation_id = leadership.correlation_id,
            "LXST telephony announce suppressed because delivery was not admitted"
        );
    }

    report.disposition = if report.queued > 0 && report.failed == 0 {
        AnnounceSendDisposition::Queued
    } else {
        AnnounceSendDisposition::Failed
    };
    tracing::info!(
        correlation_id = leadership.correlation_id,
        packets = report.packets,
        queued = report.queued,
        failed = report.failed,
        disposition = ?report.disposition,
        "presence announce burst completed"
    );
    record_current_presence_lifecycle_activity_after_identity_lock(
        state,
        activity_origin,
        leadership,
        &report,
    );
    AnnounceBurstExecution {
        report,
        activity_recorded: true,
    }
}

// FIELD_FILE_ATTACHMENTS 0x05 = msgpack `[[filename, bytes], …]`.
// FIELD_IMAGE            0x06 = msgpack `[format, bytes]` (`png`, `webp`, ...).
// FIELD_AUDIO            0x07 = msgpack `[mode, bytes]`.
struct ExtractedAttachment {
    file_name: String,
    stored_name: String,
    is_image: bool,
}

fn extract_and_save_attachment(
    state: &AppState,
    msg: &lxmf_core::message_api::LxMessage,
) -> Option<ExtractedAttachment> {
    if let Ok(Some((file_name, file_data))) = msg.first_file_attachment() {
        if let Ok(mut lxmf) = state.lxmf.lock() {
            if let Some(mgr) = lxmf.as_mut() {
                let stored = match mgr.save_attachment(&file_name, file_data) {
                    Ok(stored) => stored,
                    Err(error) => {
                        tracing::warn!(
                            error_kind = ?error.kind(),
                            size = file_data.len(),
                            kind = "file",
                            "failed to persist inbound attachment"
                        );
                        return Some(ExtractedAttachment {
                            file_name,
                            stored_name: db::ATTACHMENT_UNAVAILABLE_STORED_NAME.to_string(),
                            is_image: false,
                        });
                    }
                };
                tracing::info!(
                    size = file_data.len(),
                    kind = "file",
                    "extracted inbound attachment"
                );
                return Some(ExtractedAttachment {
                    file_name,
                    stored_name: stored,
                    is_image: false,
                });
            }
        }
    }

    if let Ok(Some((mime_type, image_data))) = msg.image_attachment() {
        let ext = mime_type.rsplit('/').next().unwrap_or("png");
        let file_name = format!("image.{ext}");
        if let Ok(mut lxmf) = state.lxmf.lock() {
            if let Some(mgr) = lxmf.as_mut() {
                let stored = match mgr.save_attachment(&file_name, image_data) {
                    Ok(stored) => stored,
                    Err(error) => {
                        tracing::warn!(
                            error_kind = ?error.kind(),
                            size = image_data.len(),
                            kind = "image",
                            "failed to persist inbound attachment"
                        );
                        return Some(ExtractedAttachment {
                            file_name,
                            stored_name: db::ATTACHMENT_UNAVAILABLE_STORED_NAME.to_string(),
                            is_image: true,
                        });
                    }
                };
                tracing::info!(
                    size = image_data.len(),
                    kind = "image",
                    "extracted inbound attachment"
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

#[derive(Debug, Clone)]
struct ExtractedAudio {
    mode: u8,
    stored_name: String,
    supported: bool,
}

fn extracted_audio_json(audio: &ExtractedAudio) -> Value {
    if audio.stored_name == db::ATTACHMENT_UNAVAILABLE_STORED_NAME {
        json!({
            "mode": audio.mode,
            "supported": false,
            "unavailable": true,
        })
    } else {
        json!({
            "mode": audio.mode,
            "stored_name": audio.stored_name,
            "supported": audio.supported,
        })
    }
}

/// Extract a structurally valid native LXMF audio field after the shared
/// authentication, policy and deduplication gates. Media errors are local to
/// this optional field: they never reject the enclosing message or its proof.
fn extract_and_save_audio(
    state: &AppState,
    msg: &lxmf_core::message_api::LxMessage,
) -> Option<ExtractedAudio> {
    let audio = match msg.audio_field() {
        Ok(audio) => audio?,
        Err(error) => {
            tracing::warn!(%error, "ignoring malformed inbound LXMF audio field");
            return None;
        }
    };
    let mode = audio.mode;
    let unavailable = || ExtractedAudio {
        mode,
        stored_name: db::ATTACHMENT_UNAVAILABLE_STORED_NAME.to_string(),
        supported: false,
    };
    if audio.bytes.len() > lxmf::MAX_AUDIO_FIELD_BYTES {
        tracing::warn!(
            mode,
            size = audio.bytes.len(),
            max_size = lxmf::MAX_AUDIO_FIELD_BYTES,
            "inbound LXMF audio exceeds persistence limit"
        );
        return Some(unavailable());
    }

    let is_ogg_opus = mode == lxmf_core::constants::AM_OPUS_OGG;
    #[cfg(feature = "lxst-voice")]
    if is_ogg_opus {
        if let Err(error) = voice_memo::inspect_voice_memo(audio.bytes) {
            tracing::warn!(%error, size = audio.bytes.len(), "inbound Ogg/Opus audio is invalid");
            return Some(unavailable());
        }
    }

    let file_name = if is_ogg_opus {
        lxmf::AUDIO_MESSAGE_FILE_NAME.to_string()
    } else {
        format!("Audio message {mode:02x}.bin")
    };
    let stored_name = state.lxmf.lock().ok().and_then(|mut lxmf| {
        lxmf.as_mut()
            .and_then(|mgr| mgr.save_attachment(&file_name, audio.bytes).ok())
    });
    match stored_name {
        Some(stored_name) => {
            tracing::info!(
                mode,
                size = audio.bytes.len(),
                "extracted inbound LXMF audio"
            );
            Some(ExtractedAudio {
                mode,
                stored_name,
                supported: is_ogg_opus && cfg!(feature = "lxst-voice"),
            })
        }
        None => {
            tracing::warn!(
                mode,
                size = audio.bytes.len(),
                "failed to persist inbound LXMF audio"
            );
            Some(unavailable())
        }
    }
}

fn remove_inbound_media_after_persistence_failure(
    state: &AppState,
    attachment: Option<&ExtractedAttachment>,
    audio: Option<&ExtractedAudio>,
) {
    let files_dir = state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(lxmf::LxmfManager::files_dir));
    let Some(files_dir) = files_dir else {
        return;
    };
    let stored_names = attachment
        .map(|attachment| attachment.stored_name.as_str())
        .into_iter()
        .chain(audio.map(|audio| audio.stored_name.as_str()));
    for stored_name in stored_names {
        if stored_name == db::ATTACHMENT_UNAVAILABLE_STORED_NAME {
            continue;
        }
        if let Some(sanitized) = lxmf::sanitize_stored_file_name(stored_name) {
            let _ = std::fs::remove_file(files_dir.join(sanitized));
        }
    }
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

/// Build the transport message that answers a path request for our own LXMF
/// delivery destination: a `PathResponse`-context announce carrying our
/// identity + path, routed back out the interface the request arrived on
/// (`OutboundAttached`), or broadcast (`Outbound`) when that interface is
/// unknown. Returns `None` if the LXMF manager isn't ready or the announce
/// can't be built. Split from the send so the routing/context choice is
/// unit-testable without a live transport.
fn build_lxmf_path_response_message(
    state: &Arc<AppState>,
    attached_interface: Option<u64>,
    tag: Option<&[u8]>,
) -> Option<rns_transport::messages::TransportMessage> {
    // Build under the lxmf lock (sync), then drop it before returning.
    let (raw, dest_hash) = match state.lxmf.lock() {
        Ok(mut guard) => {
            guard
                .as_mut()
                .and_then(|mgr| match mgr.create_path_response_announce_packet(tag) {
                    Ok(raw) => Some((raw, mgr.lxmf_dest_hash)),
                    Err(_) => {
                        tracing::warn!(
                            reason = "build_failed",
                            "failed to build LXMF path-response announce"
                        );
                        None
                    }
                })
        }
        Err(_) => None,
    }?;

    let request = rns_transport::messages::OutboundRequest {
        raw: Bytes::from(raw),
        destination_hash: dest_hash,
    };
    Some(match attached_interface {
        Some(interface_id) => rns_transport::messages::TransportMessage::OutboundAttached {
            request,
            interface_id,
        },
        None => rns_transport::messages::TransportMessage::Outbound(request),
    })
}

/// Emit a path-response announce for our LXMF delivery destination on the
/// interface a path request arrived on. The transport delegates this to us
/// because it doesn't hold our identity keys; answering is what lets a peer
/// that never announced learn our identity + path on first contact.
async fn answer_lxmf_path_request(
    state: &Arc<AppState>,
    attached_interface: Option<u64>,
    tag: Option<Vec<u8>>,
) {
    let Some(message) = build_lxmf_path_response_message(state, attached_interface, tag.as_deref())
    else {
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

    if tx.send(message).await.is_err() {
        tracing::warn!(
            reason = "queue_failed",
            "failed to queue LXMF path-response announce"
        );
    } else {
        tracing::debug!(
            attached = attached_interface.is_some(),
            "answered LXMF path request with path-response announce"
        );
    }
}

/// Handle authenticated Opportunistic proofs on their dedicated per-session
/// channel. They bypass ordinary bounded destination ingress entirely, while
/// sharing the same terminal completion path as legacy destination events.
async fn handle_lxmf_delivery_proofs(
    state: Arc<AppState>,
    mut proof_rx: tokio::sync::mpsc::UnboundedReceiver<
        rns_transport::link_messages::DestinationEvent,
    >,
    shutdown: rns_runtime::lifecycle::ShutdownSignal,
) {
    use rns_transport::link_messages::DestinationEvent;

    loop {
        let event = tokio::select! {
            biased;
            _ = shutdown.wait() => break,
            event = proof_rx.recv() => match event {
                Some(event) => event,
                None => break,
            },
        };
        let activity_origin = state.activity_request_fence();
        if shutdown.is_triggered() {
            break;
        }
        if let DestinationEvent::DeliveryProof { msg_id, rtt } = event {
            complete_authenticated_lxmf_delivery_proof(&state, &msg_id, rtt, activity_origin).await;
        } else {
            tracing::warn!(
                reason = "unexpected_event",
                "ignored non-proof event on dedicated LXMF proof channel"
            );
        }
    }
}

async fn complete_authenticated_lxmf_delivery_proof(
    state: &Arc<AppState>,
    msg_id: &str,
    rtt: Option<Duration>,
    activity_origin: ActivityRequestFence,
) {
    // Reticulum has already authenticated this proof. Rejoin it with the
    // retained Opportunistic LXMF message so callbacks and ticket
    // last-delivery accounting advance only on actual delivery.
    let completed = state
        .lxmf
        .lock()
        .ok()
        .and_then(|mut lxmf| {
            lxmf.as_mut()
                .map(|manager| manager.complete_opportunistic_delivery(msg_id))
        })
        .unwrap_or(false);
    if !completed {
        tracing::debug!(
            msg_id = %short_id(msg_id),
            "ignored delivery proof without a matching in-flight Opportunistic message"
        );
        return;
    }
    let rtt_ms = rtt.map(|d| d.as_secs_f64() * 1000.0);
    let msg_id_for_db = msg_id.to_string();
    let identity_for_db = helpers::active_identity_id(state);
    // One hop: flip the state and read the method back for the emit.
    let (updated, method) = db::spawn_db(state.db.clone(), move |p| {
        let updated =
            db::update_message_state(&p, &msg_id_for_db, &identity_for_db, "delivered", rtt_ms);
        let method = db::get_message_delivery_method(&p, &msg_id_for_db, &identity_for_db);
        (updated, method)
    })
    .await
    .expect("db task panicked");
    if !updated {
        tracing::debug!(
            msg_id = %short_id(msg_id),
            reason = "terminal_state_preserved",
            "suppressed a late delivery proof state regression"
        );
        return;
    }
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
    tracing::info!(msg_id = %short_id(msg_id), rtt_ms = ?rtt_ms, "message delivery confirmed");
    record_activity_if_current(state, activity_origin, || {
        let message = producer::MessageId::from_hex(msg_id)?;
        let method = method
            .as_deref()
            .and_then(producer::LxmfDeliveryMethod::from_code);
        let rtt_ms = rtt_ms.map(|value| {
            value
                .round()
                .clamp(0.0, u64::MAX as f64)
                .min(u64::MAX as f64) as u64
        });
        Ok(producer::lxmf_delivery_state_changed(
            producer::LxmfDeliveryStateChanged {
                message,
                state: producer::LxmfDeliveryState::Delivered,
                method,
                rtt_ms,
                failure_reason: None,
            },
        ))
    });
    // Proofs do not necessarily reappear in the LXMF manager's polled state
    // changes. Complete the originating game action here as well, so its UI
    // cannot remain stuck on "Sending" after a valid proof.
    let lrgp_meta = state
        .lrgp_msg_to_session
        .lock()
        .ok()
        .and_then(|mut map| map.remove(msg_id));
    if let Some(meta) = lrgp_meta {
        update_game_session_delivery_state(
            state,
            &meta.session_id,
            &meta.identity_id,
            &meta.contact_hash,
            "delivered",
        )
        .await;
    }
}

/// Handle inbound LXMF messages delivered by the transport actor.
async fn handle_inbound_lxmf(
    state: Arc<AppState>,
    mut rx: tokio::sync::mpsc::Receiver<rns_transport::link_messages::DestinationEvent>,
    shutdown: rns_runtime::lifecycle::ShutdownSignal,
) {
    use rns_transport::link_messages::DestinationEvent;

    loop {
        let event = tokio::select! {
            biased;
            _ = shutdown.wait() => break,
            ev = rx.recv() => match ev {
                Some(e) => e,
                None => break,
            },
        };
        // Sample after receive so a new task can cross an in-progress identity
        // transition while idle. If an old task selected this payload before
        // reset, its old shutdown signal is now triggered and the payload is
        // discarded before any side effect can borrow the replacement fence.
        let activity_origin = state.activity_request_fence();
        if shutdown.is_triggered() {
            break;
        }
        if let DestinationEvent::DeliveryProof { msg_id, rtt } = &event {
            complete_authenticated_lxmf_delivery_proof(&state, msg_id, *rtt, activity_origin).await;
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
                answer_lxmf_path_request(&state, req.attached_interface, req.tag.clone()).await;
            }
            continue;
        }

        let raw = match event {
            DestinationEvent::InboundPacket { raw, .. } => raw,
            _ => continue,
        };

        let (header, data_offset) = match rns_wire::header::PacketHeader::unpack(&raw) {
            Ok(h) => h,
            Err(_) => {
                tracing::warn!(
                    reason = "header_parse_failed",
                    "Inbound packet header parse failed"
                );
                continue;
            }
        };
        let lxmf_payload = &raw[data_offset..];
        let dest_hash = header.destination_hash;

        tracing::info!(
            payload_len = lxmf_payload.len(),
            dest = %short_id(&hex::encode(dest_hash)),
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
        let msg = match lxmf_core::message_api::LxMessage::unpack(&lxmf_data) {
            Ok(m) => m,
            Err(_) => {
                tracing::warn!(
                    reason = "unpack_failed",
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
            activity_origin,
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
    Link {
        link_id: Option<[u8; 16]>,
        /// Identity authenticated by LINKIDENTIFY for this exact link. The
        /// LXMF source destination still has to derive from this identity.
        remote_identity_hash: Option<[u8; 16]>,
    },
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

    fn activity_method(&self) -> producer::InboundLxmfMethod {
        match self {
            InboundLxmfSource::Opportunistic { .. } => producer::InboundLxmfMethod::Opportunistic,
            InboundLxmfSource::Link { .. } => producer::InboundLxmfMethod::Direct,
            InboundLxmfSource::Propagated => producer::InboundLxmfMethod::Propagated,
        }
    }
}

/// LRGP participant binding is only meaningful when `sender_hash` came from
/// an authenticated transport identity. A valid LXMF signature is sufficient
/// for every delivery method. A LINKIDENTIFY-authenticated direct link is also
/// sufficient when the message's LXMF delivery destination derives from that
/// exact remote identity.
fn lrgp_sender_authenticated(
    source: &InboundLxmfSource,
    source_hash: &[u8; 16],
    signature_valid: Option<bool>,
) -> bool {
    if signature_valid == Some(true) {
        return true;
    }
    let InboundLxmfSource::Link {
        remote_identity_hash: Some(identity_hash),
        ..
    } = source
    else {
        return false;
    };
    Destination::hash_from_name_and_identity(LXMF_DELIVERY_APP_NAME, Some(identity_hash))
        == *source_hash
}

/// Stamp PoW gate (T1-9): applies to every inbound source. Runs after
/// signature validation and before the delivery-proof ACK; ticket-store
/// entries bypass via `validate_stamp_with_tickets`.
fn inbound_stamp_allowed(state: &AppState, msg: &lxmf_core::message_api::LxMessage) -> bool {
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
            from = %short_id(&hex::encode(msg.source_hash)),
            required_cost,
            has_stamp = msg.stamp.is_some(),
            "inbound message REJECTED: stamp missing or PoW invalid (enforce_stamps=true)"
        );
    }
    stamp_ok
}

/// Blackholed-source drop gate: inbound LXMs whose source resolves to a
/// blackholed identity are dropped before any processing
/// (LXMRouter.py:1739-1741 at 1.0.1). Fail-open like Python's recall-gated
/// check (LXMessage.py:803-805): unknown sources, a missing transport, and
/// query failures all pass.
async fn inbound_source_blackholed(
    source_identity: Option<[u8; 16]>,
    transport_tx: Option<tokio::sync::mpsc::Sender<rns_transport::messages::TransportMessage>>,
) -> bool {
    let (Some(hash), Some(tx)) = (source_identity, transport_tx) else {
        return false;
    };
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    if tx
        .send(rns_transport::messages::TransportMessage::Rpc {
            query: rns_transport::messages::TransportQuery::IsBlackholed { hash },
            response_tx,
        })
        .await
        .is_err()
    {
        return false;
    }
    matches!(
        tokio::time::timeout(Duration::from_millis(100), response_rx).await,
        Ok(Ok(
            rns_transport::messages::TransportQueryResponse::BoolResult(true)
        ))
    )
}

async fn enqueue_lxmf_delivery_proof(
    transport_tx: &tokio::sync::mpsc::Sender<rns_transport::messages::TransportMessage>,
    proof_raw: Vec<u8>,
    destination_hash: [u8; 16],
) -> bool {
    transport_tx
        .send(rns_transport::messages::TransportMessage::Outbound(
            rns_transport::messages::OutboundRequest {
                raw: Bytes::from(proof_raw),
                destination_hash,
            },
        ))
        .await
        .is_ok()
}

/// Pre-decrypted inbound entry: `data` = [dest:16][src:16][sig:64][msgpack].
async fn handle_decrypted_lxmf_from_origin(
    state: &Arc<AppState>,
    data: Vec<u8>,
    source: InboundLxmfSource,
    activity_origin: ActivityRequestFence,
) {
    if let InboundLxmfSource::Link { link_id, .. } = &source {
        let limit = state.lxmf_delivery_limit_bytes();
        if data.len() > limit {
            if let Some(link_id) = link_id {
                record_activity_if_current(state, activity_origin, || {
                    Ok(producer::lxmf_inbound_rejected(
                        producer::LxmfInboundRejected {
                            link: producer::LinkId::new(*link_id),
                            encoded_bytes: data.len() as u64,
                            max_message_bytes: limit as u64,
                            reason: producer::LxmfInboundRejectionReason::SizeLimit,
                        },
                    ))
                });
            }
            tracing::warn!(
                data_len = data.len(),
                max_message_bytes = limit,
                reason = "reassembled_size_limit",
                "rejected reassembled inbound LXMF Resource"
            );
            return;
        }
    }
    let msg = match lxmf_core::message_api::LxMessage::unpack(&data) {
        Ok(m) => m,
        Err(_) => {
            tracing::warn!(
                data_len = data.len(),
                source = source.label(),
                reason = "unpack_failed",
                "inbound LXMF unpack failed"
            );
            return;
        }
    };
    process_inbound_lxmf(state, msg, &data, source, activity_origin).await;
}

#[cfg(test)]
async fn handle_decrypted_lxmf(state: &Arc<AppState>, data: Vec<u8>, source: InboundLxmfSource) {
    handle_decrypted_lxmf_from_origin(state, data, source, state.activity_request_fence()).await;
}

/// The one inbound LXMF pipeline. `fallback_id_material` is the unpacked
/// wire material; its hash is the msg-id fallback when the message carries
/// no hash — deterministic across sender retries so dedupe still works
/// (the old paths used the ciphertext hash / a fresh uuid4, both of which
/// made every retry look new).
async fn process_inbound_lxmf(
    state: &Arc<AppState>,
    mut msg: lxmf_core::message_api::LxMessage,
    fallback_id_material: &[u8],
    source: InboundLxmfSource,
    activity_origin: ActivityRequestFence,
) {
    let source_hash = hex::encode(msg.source_hash);
    let dest_hash = hex::encode(msg.destination_hash);
    let inbound_method = source.activity_method();

    tracing::info!(
        from = %short_id(&source_hash),
        len = msg.content.len(),
        source = source.label(),
        "inbound LXMF message received"
    );
    let (source_identity, blackhole_tx) = state
        .lxmf
        .lock()
        .ok()
        .and_then(|l| {
            l.as_ref().map(|mgr| {
                (
                    mgr.recall_identity_hash(&msg.source_hash),
                    mgr.router.transport_tx.clone(),
                )
            })
        })
        .unwrap_or((None, None));
    if inbound_source_blackholed(source_identity, blackhole_tx).await {
        tracing::debug!(from = %short_id(&source_hash), "Dropping LXM from blackholed identity");
        return;
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

    if sig_valid == Some(true) {
        if let Ok(mut lxmf) = state.lxmf.lock() {
            if let Some(mgr) = lxmf.as_mut() {
                if mgr.router.learn_ticket_from_inbound(&msg) {
                    tracing::debug!(
                        from = %short_id(&hex::encode(msg.source_hash)),
                        "stored signed inbound ticket for future stamp bypass"
                    );
                }
            }
        }
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
        if let Some((proof_raw, tx, proof_hdr)) = proof_and_tx.and_then(|(proof_raw, tx)| {
            rns_wire::header::PacketHeader::unpack(&proof_raw)
                .ok()
                .map(|(proof_hdr, _)| (proof_raw, tx, proof_hdr))
        }) {
            if enqueue_lxmf_delivery_proof(&tx, proof_raw, proof_hdr.destination_hash).await {
                tracing::debug!("sent delivery proof for inbound message");
            } else {
                tracing::warn!(
                    reason = "transport_closed",
                    "could not enqueue delivery proof for inbound message"
                );
            }
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
        tracing::debug!(msg_id = %short_id(&msg_id), identity_id = %short_id(&identity_id), "inbound LXMF duplicate — skipping");
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
        tracing::debug!(from = %short_id(&source_hash), "inbound message from blocked user — discarding");
        return;
    }

    if let InboundLxmfSource::Link {
        link_id: Some(link_id),
        ..
    } = source
    {
        let local_destination_matches = hex::decode(&lxmf_id)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .is_some_and(|local_dest: [u8; 16]| local_dest == msg.destination_hash);
        if local_destination_matches {
            if let Ok(mut lxmf) = state.lxmf.lock() {
                if let Some(mgr) = lxmf.as_mut() {
                    mgr.note_pending_direct_backchannel(msg.source_hash, link_id);
                }
            }
            tracing::debug!(
                from = %short_id(&source_hash),
                link_id = %short_id(&hex::encode(link_id)),
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
    let lrgp_sender_authenticated = lrgp_sender_authenticated(&source, &msg.source_hash, sig_valid);
    if !matches!(
        chat_extension,
        Some(lxmf::RatspeakChatExtension::Reply { .. })
    ) && try_handle_inbound_lrgp(
        state,
        &msg,
        &source_hash,
        &lxmf_id,
        lrgp_sender_authenticated,
    )
    .await
    {
        return;
    }

    let received_at = next_chat_observed_timestamp(state, &source_hash, &identity_id).await;
    let attachment_file = extract_and_save_attachment(state, &msg);
    let audio_file = extract_and_save_audio(state, &msg);
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
        let audio_for_save = audio_file.clone();
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
        let save_result = db::spawn_db(state.db.clone(), move |p| {
            db::try_save_message_with_audio(
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
                audio_for_save.as_ref().map(|audio| audio.mode),
                audio_for_save
                    .as_ref()
                    .map(|audio| audio.stored_name.as_str())
                    .unwrap_or(""),
            )
        })
        .await
        .expect("db task panicked");
        if let Err(error) = save_result {
            tracing::warn!(%error, "failed to persist inbound LXMF message");
            remove_inbound_media_after_persistence_failure(
                state,
                attachment_file.as_ref(),
                audio_file.as_ref(),
            );
            return;
        }
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
    // "Accepted" is intentionally recorded only after every authentication,
    // policy, duplicate, extension-routing, and persistence gate succeeds.
    // The typed producer receives no message content, display name, or raw
    // wire material.
    record_activity_if_current(state, activity_origin, || {
        Ok(producer::lxmf_inbound_accepted(
            producer::LxmfInboundAccepted {
                source: producer::DestinationHash::new(msg.source_hash),
                method: inbound_method,
                encoded_bytes: fallback_id_material.len().min(u32::MAX as usize) as u32,
            },
        ))
    });
    notify_inbound_message_if_background(
        state,
        &source_hash,
        &identity_id,
        &msg.content,
        attachment_file.is_some() || audio_file.is_some(),
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
    if let Some(ref audio) = audio_file {
        event_data
            .as_object_mut()
            .unwrap()
            .insert("audio".to_string(), extracted_audio_json(audio));
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
        Err(_) => tracing::error!(
            reason = "refresh_failed",
            "contacts refresh after inbound message failed"
        ),
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
        Err(_) => {
            tracing::error!(
                reason = "refresh_failed",
                "unread-total refresh after inbound message failed"
            )
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
        crate::transport_observation::authoritative_path_table(&handle),
        handle.query_control(rns_transport::messages::TransportQuery::GetLinkCount),
    );

    let iface_stats = match iface_result {
        Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(s)) => {
            state.interface_stats_payload(&s)
        }
        _ => json!({ "interfaces": [] }),
    };

    let (path_table, path_index, path_table_total, path_table_truncated) = match path_result {
        Some(entries) => {
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
    if let Ok(mut lxmf) = state.lxmf.lock() {
        if let Some(mgr) = lxmf.as_mut() {
            mgr.replace_route_hops_from_path_table(entries);
        }
    }
}

// Debounce for eager `poll_now` wakes.
const POLL_NOW_COOLDOWN: Duration = Duration::from_millis(750);

/// Minimal, content-free observations retained until the poll's identity and
/// Activity generation fences have been revalidated. Keeping domain facts
/// here avoids constructing classified drafts (or sampling the Activity
/// clock) for stale poll work.
enum PollActivityObservation {
    InterfaceState { online: bool },
    AnnounceSuppressed,
    AnnounceIngressBurst { active: bool },
    AnnouncesHeld { count: u64 },
    PathObserved { destination: [u8; 16], hops: u8 },
    AnnounceObserved { destination: [u8; 16], hops: u8 },
}

fn observe_polled_interface_state(
    previous: &mut std::collections::HashMap<u64, bool>,
    interface_id: u64,
    online: bool,
    covered_by_exact_rnode_observer: bool,
) -> (bool, bool) {
    let changed = previous.insert(interface_id, online) != Some(online);
    (changed, changed && !covered_by_exact_rnode_observer)
}

#[cfg(test)]
mod rnode_poll_activity_tests {
    use super::observe_polled_interface_state;
    use std::collections::HashMap;

    #[test]
    fn exact_covered_id_is_suppressed_without_hiding_another_same_name_interface() {
        let mut previous = HashMap::new();

        // Names deliberately do not participate in this state machine. Two
        // stats rows may share a display name while their exact IDs remain
        // independently observed.
        assert_eq!(
            observe_polled_interface_state(&mut previous, 41, true, true),
            (true, false)
        );
        assert_eq!(
            observe_polled_interface_state(&mut previous, 42, true, false),
            (true, true)
        );
        assert_eq!(
            observe_polled_interface_state(&mut previous, 41, false, true),
            (true, false)
        );
        assert_eq!(
            observe_polled_interface_state(&mut previous, 42, false, false),
            (true, true)
        );
        assert_eq!(
            observe_polled_interface_state(&mut previous, 42, false, false),
            (false, false)
        );
    }
}

fn record_poll_activity(
    state: &AppState,
    origin: ActivityRequestFence,
    observation: PollActivityObservation,
) {
    record_activity_if_current(state, origin, || match observation {
        PollActivityObservation::InterfaceState { online } => {
            Ok(producer::interface_activity(producer::InterfaceActivity {
                class: producer::InterfaceClass::Unknown,
                transition: if online {
                    producer::InterfaceTransition::Online
                } else {
                    producer::InterfaceTransition::Offline
                },
                endpoint: None,
            }))
        }
        PollActivityObservation::AnnounceSuppressed => Ok(producer::rns_announce_activity(
            producer::RnsAnnounceActivity {
                transition: producer::RnsAnnounceTransition::Suppressed {
                    reason: producer::AnnounceSuppressionReason::InterfaceRestart,
                },
                interface: None,
            },
        )),
        PollActivityObservation::AnnounceIngressBurst { active } => Ok(
            producer::rns_announce_activity(producer::RnsAnnounceActivity {
                transition: if active {
                    producer::RnsAnnounceTransition::IngressBurstStarted
                } else {
                    producer::RnsAnnounceTransition::IngressBurstCleared
                },
                interface: None,
            }),
        ),
        PollActivityObservation::AnnouncesHeld { count } => Ok(producer::rns_announce_activity(
            producer::RnsAnnounceActivity {
                transition: producer::RnsAnnounceTransition::Held { count },
                interface: None,
            },
        )),
        PollActivityObservation::PathObserved { destination, hops } => {
            Ok(producer::rns_path_observed(producer::RnsPathDiscovered {
                destination: producer::DestinationHash::new(destination),
                hops,
                evidence: producer::PathEvidence::Transport,
                endpoint: None,
                correlation_id: None,
            }))
        }
        PollActivityObservation::AnnounceObserved { destination, hops } => Ok(
            producer::rns_announce_activity(producer::RnsAnnounceActivity {
                transition: producer::RnsAnnounceTransition::Observed {
                    destination: producer::DestinationHash::new(destination),
                    hops,
                },
                interface: None,
            }),
        ),
    });
}

// Always emits, including backgrounded — first paint on resume.
async fn poll_stats_loop(
    state: Arc<AppState>,
    shutdown: rns_runtime::lifecycle::ShutdownSignal,
    runtime_activity_origin: ActivityRequestFence,
) {
    // A runtime task may not be scheduled until after its owning session has
    // already shut down. Reject that late start before emitting compatibility
    // or typed facts, and retain the origin captured by the spawning session.
    if shutdown.is_triggered() {
        return;
    }
    let mut interval = tokio::time::interval(Duration::from_millis(2500));

    record_activity_if_current(&state, runtime_activity_origin, || {
        Ok(producer::app_runtime(
            producer::AppRuntimeTransition::Started,
        ))
    });

    let mut prev_online: std::collections::HashMap<u64, bool> = std::collections::HashMap::new();
    let mut prev_ingress_burst: std::collections::HashMap<u64, bool> =
        std::collections::HashMap::new();
    let mut prev_held_announces: std::collections::HashMap<u64, u64> =
        std::collections::HashMap::new();
    let mut last_interface_announce = std::time::Instant::now();

    #[cfg(feature = "mobile-throttle")]
    let mut was_foreground = true;
    let mut last_poll_at = std::time::Instant::now()
        .checked_sub(POLL_NOW_COOLDOWN)
        .unwrap_or_else(std::time::Instant::now);

    loop {
        tokio::select! {
            biased;
            _ = shutdown.wait() => break,
            _ = interval.tick() => {}
            _ = state.poll_now.notified() => {
                if last_poll_at.elapsed() < POLL_NOW_COOLDOWN {
                    continue;
                }
                interval.reset();
            }
        }
        let poll_activity_origin = state.activity_request_fence();
        if shutdown.is_triggered() {
            break;
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
        let mut activity_observations = Vec::new();
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
                crate::transport_observation::authoritative_path_table(&handle),
                handle.query_control(rns_transport::messages::TransportQuery::GetLinkCount),
                handle.query_transport(rns_transport::messages::TransportQuery::GetRecentAnnounces),
            );

            let iface_stats = match iface_result {
                Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(s)) => {
                    for iface in &s {
                        let name = iface.name.as_str();
                        let online = state.effective_interface_online(iface.id, iface.online);
                        let burst_active = iface.burst_active;
                        let held_announces = iface.held_announces;
                        let key = iface.id;
                        let rnode_activity_covered =
                            state.is_rnode_activity_interface_covered(iface.id);
                        let (state_changed, emit_generic_state) = observe_polled_interface_state(
                            &mut prev_online,
                            key,
                            online,
                            rnode_activity_covered,
                        );
                        if state_changed {
                            if emit_generic_state {
                                activity_observations
                                    .push(PollActivityObservation::InterfaceState { online });
                            }
                            let reannounce_suppressed =
                                online && state.take_interface_reannounce_suppression(name);
                            if reannounce_suppressed {
                                last_interface_announce = Instant::now();
                                tracing::info!(
                                    "interface re-announce suppressed after config restart"
                                );
                                activity_observations
                                    .push(PollActivityObservation::AnnounceSuppressed);
                            }
                            // Re-announce on interface up; governed by the
                            // user's announce schedule and 30-second cooldown.
                            if should_reannounce_for_interface_online(
                                online,
                                reannounce_suppressed,
                                *state.announce_interval_rx.borrow(),
                                last_interface_announce.elapsed() >= Duration::from_secs(30),
                            ) {
                                last_interface_announce = std::time::Instant::now();
                                // RNode readiness advances this revision at its
                                // exact Ready boundary before publishing stats.
                                // Generic interfaces have no narrower signal,
                                // so their first online observation owns it.
                                if !rnode_activity_covered {
                                    state.bump_announce_interface_revision();
                                }
                                let announce_state = state.clone();
                                let announce_activity_origin = poll_activity_origin;
                                tokio::spawn(async move {
                                    tokio::time::sleep(Duration::from_secs(2)).await;
                                    let _ = send_typed_announce_from_origin(
                                        &announce_state,
                                        AnnounceOrigin::InterfaceOnline,
                                        announce_activity_origin,
                                    )
                                    .await;
                                });
                            }
                        }
                        let prev_burst = prev_ingress_burst.get(&key).copied();
                        if prev_burst.is_some_and(|was_bursting| was_bursting != burst_active) {
                            activity_observations.push(
                                PollActivityObservation::AnnounceIngressBurst {
                                    active: burst_active,
                                },
                            );
                        }
                        let prev_held = prev_held_announces.get(&key).copied().unwrap_or(0);
                        if held_announces > 0 && prev_held == 0 {
                            activity_observations.push(PollActivityObservation::AnnouncesHeld {
                                count: held_announces,
                            });
                        }
                        prev_ingress_burst.insert(key, burst_active);
                        prev_held_announces.insert(key, held_announces);
                    }

                    state.interface_stats_payload(&s)
                }
                _ => json!({ "interfaces": [] }),
            };

            let (path_table, path_index, path_table_total, path_table_truncated) = match path_result
            {
                Some(entries) => {
                    cache_lxmf_route_hops_from_path_table(&state, &entries);
                    let path_activity_ready = state
                        .path_activity_baselined
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let hashes: std::collections::HashSet<String> =
                        entries.iter().map(|e| hex::encode(e.hash)).collect();

                    let newly_reachable: Vec<([u8; 16], u8)> = if path_activity_ready {
                        if let Ok(cached) = state.known_path_hashes.lock() {
                            entries
                                .iter()
                                .filter(|entry| !cached.contains(&hex::encode(entry.hash)))
                                .map(|entry| (entry.hash, entry.hops))
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

                    for (destination, hops) in newly_reachable {
                        activity_observations
                            .push(PollActivityObservation::PathObserved { destination, hops });
                    }

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
                let mut identities_changed = false;
                let mut router_changed = false;
                let mut changed_ratchet_hashes = Vec::new();
                // Aspect-agnostic: crypto cache, announce_history, contact-name refresh.
                if let Ok(mut lxmf) = state.lxmf.lock() {
                    if let Some(mgr) = lxmf.as_mut() {
                        for a in &announces {
                            let dest_hex = hex::encode(a.dest_hash);
                            tracing::debug!(
                                dest = %short_id(&dest_hex),
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
                                if ratchet_changed {
                                    changed_ratchet_hashes.push(dest_hex.clone());
                                }
                                if is_new {
                                    tracing::debug!(
                                        dest = %short_id(&dest_hex),
                                        has_ratchet = a.ratchet.is_some(),
                                        "new remote identity cached from announce"
                                    );
                                }
                            }
                            router_changed |= mgr.update_lxmf_announce_app_data(
                                a.dest_hash,
                                a.name_hash,
                                a.app_data.as_deref(),
                            );
                        }
                    }
                }
                if identities_changed || !changed_ratchet_hashes.is_empty() || router_changed {
                    let persistence_state = state.clone();
                    tokio::spawn(async move {
                        if let Err(error) = crate::lxmf_persistence::persist_current_delta(
                            &persistence_state,
                            identities_changed,
                            &changed_ratchet_hashes,
                            router_changed,
                            "announce_ingress",
                        )
                        .await
                        {
                            tracing::warn!(
                                %error,
                                "announce-derived LXMF persistence failed"
                            );
                        }
                    });
                }

                if let Ok(mut history) = state.announce_history.write() {
                    let current_announce_hashes: std::collections::HashSet<String> =
                        announces.iter().map(|a| hex::encode(a.dest_hash)).collect();
                    if let Some(mut seen) = state
                        .seen_announce_hashes
                        .lock()
                        .ok()
                        .filter(|seen| seen.len() > 50_000)
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
                            } else if let Some(identity_hash) = a
                                .public_key
                                .as_ref()
                                .filter(|_| a.name_hash == lxst_telephony_name_hash)
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
                            activity_observations.push(PollActivityObservation::AnnounceObserved {
                                destination: a.dest_hash,
                                hops: a.hops,
                            });
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
                    let triggered = match state.lxmf.lock() {
                        Ok(mut lxmf) => lxmf.as_mut().map_or(0, |mgr| {
                            delivery_trigger_hashes
                                .iter()
                                .map(|dest| {
                                    mgr.router.trigger_outbound_for_delivery_announce(*dest)
                                })
                                .sum::<usize>()
                        }),
                        Err(_) => 0,
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

        // Activity is emitted only after the same final identity barrier used
        // for the stats snapshot. Each record also rechecks the independent
        // Activity reset fence immediately before recorder admission.
        for observation in activity_observations {
            record_poll_activity(&state, poll_activity_origin, observation);
        }

        state.set_last_stats(stats.clone());
        // Emit even when suspended; freshest snapshot is queued for resume.
        state.emit_to_all("stats_update", stats);

        last_poll_at = std::time::Instant::now();
    }
}

// LXMF send → "failed" if no delivery proof within this window.
const MESSAGE_TIMEOUT_SECS: f64 = 180.0;

fn lxmf_step_starts_delivery_timeout(step: &str) -> bool {
    matches!(
        step,
        "sent"
            | "routing"
            | "propagating"
            | "resolving"
            | "link_establishing"
            | "sending_via_link"
            | "resource_link_ready"
            | "resource_advertised"
            | "resource_transferring"
            | "resource_waiting_for_proof"
            | "reusing_direct_link"
            | "reusing_backchannel"
    )
}

fn lxmf_step_ends_delivery_timeout(step: &str) -> bool {
    matches!(
        step,
        "delivered" | "propagated" | "failed" | "cancelled" | "rejected" | "timeout"
    )
}

fn update_message_delivery_timeout(state: &AppState, msg_id: &str, step: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    if let Ok(mut times) = state.message_send_times.lock() {
        update_message_delivery_timeout_at(&mut times, msg_id, step, now);
    }
}

fn update_message_delivery_timeout_at(
    times: &mut std::collections::HashMap<String, f64>,
    msg_id: &str,
    step: &str,
    now: f64,
) {
    if lxmf_step_starts_delivery_timeout(step) {
        times.entry(msg_id.to_string()).or_insert(now);
    } else if lxmf_step_ends_delivery_timeout(step) {
        times.remove(msg_id);
    }
}

#[cfg(test)]
mod delivery_timeout_policy_tests {
    use super::{
        lxmf_step_ends_delivery_timeout, lxmf_step_starts_delivery_timeout,
        update_message_delivery_timeout_at,
    };

    #[test]
    fn direct_link_setup_starts_the_bounded_delivery_clock() {
        for step in [
            "routing",
            "link_establishing",
            "sending_via_link",
            "resource_waiting_for_proof",
            "sent",
        ] {
            assert!(lxmf_step_starts_delivery_timeout(step), "{step}");
        }
    }

    #[test]
    fn every_terminal_outcome_retires_the_delivery_clock() {
        for step in [
            "delivered",
            "propagated",
            "failed",
            "cancelled",
            "rejected",
            "timeout",
        ] {
            assert!(lxmf_step_ends_delivery_timeout(step), "{step}");
        }
    }

    #[test]
    fn progress_only_resource_transfer_owns_one_bounded_clock() {
        let mut times = std::collections::HashMap::new();

        update_message_delivery_timeout_at(
            &mut times,
            "resource-message",
            "resource_advertised",
            10.0,
        );
        update_message_delivery_timeout_at(
            &mut times,
            "resource-message",
            "resource_transferring",
            20.0,
        );
        assert_eq!(times.get("resource-message"), Some(&10.0));

        update_message_delivery_timeout_at(&mut times, "resource-message", "delivered", 30.0);
        assert!(!times.contains_key("resource-message"));
    }
}

// The process-local LRGP message-to-session map is intentionally ephemeral.
// After a restart, use the same proof timeout as ordinary Direct messages to
// recover a durable action left in flight and expose its preserved envelope
// through Resend.
const LRGP_RECOVERY_TIMEOUT_SECS: f64 = MESSAGE_TIMEOUT_SECS;

async fn check_message_timeouts(state: &AppState, activity_origin: ActivityRequestFence) {
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

    // One blocking-pool hop for the whole sweep. Only rows that win the
    // one-way terminal-state race may emit timeout or cancel live owners.
    let identity_id = helpers::active_identity_id(state);
    let ids_for_db = timed_out.clone();
    let transitioned = db::spawn_db(state.db.clone(), move |p| {
        ids_for_db
            .iter()
            .filter_map(|msg_id| {
                if db::update_message_state(&p, msg_id, &identity_id, "timeout", None) {
                    Some((
                        msg_id.clone(),
                        db::get_message_delivery_method(&p, msg_id, &identity_id),
                    ))
                } else {
                    None
                }
            })
            .collect::<Vec<(String, Option<String>)>>()
    })
    .await
    .unwrap_or_default();

    // Stop every remaining local retry/Link/Resource owner after timeout is
    // durable. This cannot recall a packet already handed to the network.
    if let Ok(mut lxmf) = state.lxmf.lock() {
        if let Some(manager) = lxmf.as_mut() {
            for (msg_id, _) in &transitioned {
                let _ = manager.cancel_outbound_message(msg_id);
            }
        }
    }

    for (msg_id, method) in transitioned {
        state.release_attachment_delivery_lease(&msg_id);
        let client_msg_id = state
            .msg_id_map
            .lock()
            .ok()
            .and_then(|mut map| map.remove(&msg_id));
        state.emit_to_all(
            "lxmf_step",
            json!({
                "step": "timeout",
                "msg_id": msg_id,
                "client_msg_id": client_msg_id,
                "reason": "timeout",
                "method": method,
            }),
        );
        tracing::debug!(msg_id = %short_id(&msg_id), timeout_secs = MESSAGE_TIMEOUT_SECS, "Message timed out");
        record_activity_if_current(state, activity_origin, || {
            let message = producer::MessageId::from_hex(&msg_id)?;
            let method = method
                .as_deref()
                .and_then(producer::LxmfDeliveryMethod::from_code);
            Ok(producer::lxmf_delivery_state_changed(
                producer::LxmfDeliveryStateChanged {
                    message,
                    state: producer::LxmfDeliveryState::Failed,
                    method,
                    rtt_ms: None,
                    failure_reason: Some(producer::DeliveryFailureReason::ProofTimedOut),
                },
            ))
        });
        // Keep LRGP delivery state coupled to the same timeout policy as
        // ordinary Direct messages. Without this, a game action that reached
        // "sent" but never produced a proof stayed pending forever and never
        // exposed its exact durable envelope through the Resend UI.
        let lrgp_meta = state
            .lrgp_msg_to_session
            .lock()
            .ok()
            .and_then(|mut map| map.remove(&msg_id));
        if let Some(meta) = lrgp_meta {
            update_game_session_delivery_state(
                state,
                &meta.session_id,
                &meta.identity_id,
                &meta.contact_hash,
                "failed",
            )
            .await;
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
    // A transport rejection is terminal for this send attempt and must expose
    // the same exact-envelope Resend path as a timeout/failure.
    let ns = match new_state {
        "rejected" | "undelivered" => "failed",
        state => state,
    }
    .to_string();
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

fn game_delivery_state_is_in_flight(state: &str) -> bool {
    matches!(
        state,
        "pending"
            | "sending"
            | "routing"
            | "link_establishing"
            | "sending_via_link"
            | "reusing_direct_link"
            | "reusing_backchannel"
            | "propagating"
            | "sent"
    )
}

async fn sweep_stale_game_deliveries(state: &AppState) {
    let pool = state.db.clone();
    let candidates: Vec<(String, String, String)> = db::spawn_db(pool, |p| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let cutoff = now - LRGP_RECOVERY_TIMEOUT_SECS;
        let conn = match p.get() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT s.session_id, s.identity_id, s.contact_hash, s.metadata
             FROM app_sessions s
             WHERE s.last_action_at < ?1
               AND EXISTS (
                 SELECT 1 FROM app_actions a
                 WHERE a.session_id = s.session_id
                   AND a.identity_id = s.identity_id
                   AND a.action_num = (
                     SELECT MAX(latest.action_num) FROM app_actions latest
                     WHERE latest.session_id = s.session_id
                       AND latest.identity_id = s.identity_id
                   )
                   AND a.sender = s.identity_id
                   AND a.envelope_mp IS NOT NULL
               )",
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
                let meta: serde_json::Value = serde_json::from_str(meta_json).unwrap_or(json!({}));
                let ds = meta
                    .get("delivery_state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                game_delivery_state_is_in_flight(ds)
            })
            .map(|(sid, iid, ch, _)| (sid, iid, ch))
            .collect()
    })
    .await
    .unwrap_or_default();

    for (sid, iid, ch) in candidates {
        update_game_session_delivery_state(state, &sid, &iid, &ch, "failed").await;
        tracing::info!("Recovered stale LRGP delivery after restart — Resend is now available");
    }

    // App manifests own pending/active TTL policy. Asking each registered app
    // for its records applies that policy in memory; mirror only newly-expired
    // transitions into durable state so restarts and the UI see the same truth.
    let expired: Vec<lrgp::session::Session> = state
        .lrgp_router
        .list_apps()
        .into_iter()
        .flat_map(|manifest| {
            state
                .lrgp_router
                .list_sessions(&manifest.app_id, None)
                .unwrap_or_default()
        })
        .filter(|session| session.status == lrgp::constants::STATUS_EXPIRED)
        .collect();
    if expired.is_empty() {
        return;
    }

    let changed_identities = db::spawn_db(state.db.clone(), move |pool| {
        let mut changed = std::collections::HashSet::new();
        for session in expired {
            let already_expired =
                db::get_game_session(&pool, &session.session_id, &session.identity_id)
                    .and_then(|row| {
                        row.get("status")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .is_some_and(|status| status == lrgp::constants::STATUS_EXPIRED);
            if !already_expired {
                changed.insert(session.identity_id.clone());
                db::save_game_session(&pool, &session);
            }
        }
        changed
    })
    .await
    .unwrap_or_default();

    let active_identity = state
        .lxmf
        .lock()
        .ok()
        .and_then(|manager| manager.as_ref().map(|manager| manager.lxmf_hash.clone()));
    if let Some(identity_id) =
        active_identity.filter(|identity_id| changed_identities.contains(identity_id))
    {
        let all = db::spawn_db(state.db.clone(), move |pool| {
            db::list_game_sessions(&pool, &identity_id, None, None)
        })
        .await
        .unwrap_or_default();
        state.emit_to_all("all_game_sessions", all.into());
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
    if let Some(name) = rmpv::decode::read_value(&mut cursor)
        .ok()
        .and_then(|value| extract_name_from_msgpack(&value))
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

struct LrgpErrorReply<'a> {
    destination: &'a str,
    identity_id: &'a str,
    app_id: &'a str,
    app_version: u32,
    session_id: &'a str,
    rejected_command: &'a str,
    code: &'a str,
    message: &'a str,
}

fn send_lrgp_error_best_effort(state: &AppState, reply: LrgpErrorReply<'_>) {
    let LrgpErrorReply {
        destination,
        identity_id,
        app_id,
        app_version,
        session_id,
        rejected_command,
        code,
        message,
    } = reply;
    if rejected_command == lrgp::protocol::CMD_ERROR || app_id.is_empty() || session_id.is_empty() {
        return;
    }

    let payload = std::collections::HashMap::from([
        (
            "code".to_string(),
            rmpv::Value::String(code.to_string().into()),
        ),
        (
            "msg".to_string(),
            rmpv::Value::String(message.to_string().into()),
        ),
        (
            "ref".to_string(),
            rmpv::Value::String(rejected_command.to_string().into()),
        ),
    ]);
    let Ok(envelope) = lrgp::protocol::pack_envelope(
        app_id,
        app_version,
        lrgp::protocol::CMD_ERROR,
        session_id,
        Some(payload),
        None,
    ) else {
        return;
    };
    if lrgp::protocol::validate_envelope_size(&envelope).is_err() {
        return;
    }
    let Ok(fields) = lrgp::protocol::pack_lxmf_fields(&envelope) else {
        return;
    };
    let fallback = format!("[LRGP] Action rejected: {message}");
    let queued = state
        .lxmf
        .lock()
        .ok()
        .and_then(|mut manager| {
            manager.as_mut().and_then(|manager| {
                manager.send_message_with_lrgp_fields_preference(
                    destination,
                    &fallback,
                    &fields,
                    &state.db,
                    identity_id,
                    lxmf::DeliveryPreference::Opportunistic,
                )
            })
        })
        .is_some();
    if queued {
        state.lxmf_notify.notify_one();
    }
}

// Returns true if the envelope was LRGP (dispatched); false → fall through.
async fn try_handle_inbound_lrgp(
    state: &AppState,
    msg: &lxmf_core::message_api::LxMessage,
    sender_hash: &str,
    identity_id: &str,
    sender_authenticated: bool,
) -> bool {
    let mut rmpv_fields: std::collections::HashMap<u8, rmpv::Value> =
        std::collections::HashMap::new();
    for (&key, bytes) in &msg.fields {
        let mut cursor = std::io::Cursor::new(bytes);
        if let Some(value) = rmpv::decode::read_value(&mut cursor)
            .ok()
            .filter(|_| cursor.position() as usize == bytes.len())
        {
            rmpv_fields.insert(key, value);
        } else if let Ok(s) = std::str::from_utf8(bytes) {
            rmpv_fields.insert(key, rmpv::Value::String(s.into()));
        } else {
            rmpv_fields.insert(key, rmpv::Value::Binary(bytes.clone()));
        }
    }

    let has_lrgp_marker = matches!(
        rmpv_fields.get(&lrgp::protocol::FIELD_CUSTOM_TYPE),
        Some(rmpv::Value::String(value))
            if value.as_str() == Some(lrgp::protocol::PROTOCOL_TYPE)
    );
    if !has_lrgp_marker {
        return false;
    }
    if !sender_authenticated {
        // LRGP binds a session to the sender identity supplied by its
        // transport; it cannot authenticate that string itself. Never let an
        // unsigned/unknown LRGP marker fall through into ordinary chat.
        tracing::warn!(
            from = %short_id(sender_hash),
            "Dropping LRGP envelope without an authenticated LXMF sender"
        );
        return true;
    }

    let envelope = match lrgp::protocol::unpack_envelope(&rmpv_fields) {
        Ok(Some(env)) => env,
        Ok(None) => return false,
        Err(_) => {
            // A message explicitly marked as LRGP must never leak into the
            // ordinary chat transcript merely because its envelope is bad.
            tracing::warn!(
                from = %short_id(sender_hash),
                reason = "invalid_envelope",
                "Dropping malformed LRGP envelope"
            );
            return true;
        }
    };

    tracing::info!(from = %short_id(sender_hash), "Inbound LRGP game message received");

    let session_id = envelope
        .get(lrgp::protocol::KEY_SESSION)
        .and_then(lrgp::protocol::value_as_str)
        .unwrap_or("")
        .to_string();
    let app_ver = envelope
        .get(lrgp::protocol::KEY_APP)
        .and_then(lrgp::protocol::value_as_str)
        .unwrap_or("");
    let (app_id, app_version) = lrgp::protocol::parse_app_version(app_ver)
        .map(|(id, version)| (id.to_string(), version))
        .unwrap_or_default();
    let command = envelope
        .get(lrgp::protocol::KEY_COMMAND)
        .and_then(lrgp::protocol::value_as_str)
        .unwrap_or("")
        .to_string();

    // The router's process-local nonce cache protects the hot path. Retaining
    // accepted envelopes on action rows lets us compare their protocol nonces
    // and extend that guarantee across application restarts.
    let envelope_mp = match lrgp::protocol::pack_to_bytes(&envelope) {
        Ok(bytes) => bytes,
        Err(_) => {
            tracing::warn!(
                reason = "reencode_failed",
                "Dropping LRGP envelope that cannot be encoded"
            );
            return true;
        }
    };
    let durable_nonce: [u8; lrgp::protocol::NONCE_BYTES] = envelope
        .get(lrgp::protocol::KEY_NONCE)
        .and_then(|value| match value {
            rmpv::Value::Binary(bytes) => bytes.as_slice().try_into().ok(),
            _ => None,
        })
        .expect("unpack_envelope validated the LRGP nonce");
    let sid = session_id.clone();
    let iid = identity_id.to_string();
    let nonce_for_db = durable_nonce;
    let replayed_after_restart = db::spawn_db(state.db.clone(), move |pool| {
        db::has_game_nonce(&pool, &sid, &iid, &nonce_for_db)
    })
    .await
    .unwrap_or(false);
    if replayed_after_restart {
        tracing::debug!(session_id, command, "Dropping durable LRGP replay");
        return true;
    }

    // Keep a router snapshot until durable storage commits. The LRGP app
    // handlers are intentionally in-memory, so accepting an action in the
    // router and then losing the database write would otherwise split the two
    // sources of state for the remainder of the process.
    let router_snapshot = state
        .lrgp_router
        .snapshot_session(&app_id, &session_id, identity_id);

    let result = match state
        .lrgp_router
        .dispatch_incoming(&envelope, sender_hash, identity_id)
    {
        Ok(lrgp::app_base::IncomingDispatch::Applied(result)) => result,
        Ok(lrgp::app_base::IncomingDispatch::Replay) => {
            tracing::debug!(session_id, command, "Dropping in-process LRGP replay");
            return true;
        }
        Ok(lrgp::app_base::IncomingDispatch::RemoteError(error)) => {
            // Remote protocol errors are accepted LRGP actions even though
            // they do not mutate game state. Persist their nonce/action before
            // surfacing them so a transport replay after restart cannot show
            // the same rejection again.
            let error_payload = json!({
                "code": error.code.clone(),
                "msg": error.message.clone(),
                "ref": error.reference.clone(),
            });
            let persisted = {
                let sid = error.session_id.clone();
                let iid = identity_id.to_string();
                let sender = sender_hash.to_string();
                let packed = envelope_mp.clone();
                let payload = serde_json::to_string(&error_payload).unwrap_or_else(|_| "{}".into());
                let message_timestamp = msg.timestamp;
                db::spawn_db(state.db.clone(), move |pool| {
                    db::persist_inbound_game_action(
                        &pool,
                        &sid,
                        &iid,
                        lrgp::protocol::CMD_ERROR,
                        &payload,
                        &sender,
                        message_timestamp,
                        &packed,
                        None,
                    )
                })
                .await
                .ok()
                .flatten()
                .is_some()
            };
            if !persisted {
                // Remote errors consume a replay nonce but do not mutate app
                // state. If the durable action record fails, release only
                // that nonce and do not surface the error: the authenticated
                // peer may retransmit the exact envelope after storage
                // recovers, at which point it can be committed and shown
                // exactly once.
                state.lrgp_router.forget_incoming_nonce(
                    identity_id,
                    &error.session_id,
                    &durable_nonce,
                );
                tracing::warn!(
                    session = %short_id(&error.session_id),
                    "Remote LRGP error was not surfaced because durable replay state failed"
                );
                state.emit_to_all(
                    "game_action_received",
                    json!({
                        "session_id": error.session_id,
                        "app_id": error.app_id,
                        "command": command,
                        "from": sender_hash,
                        "applied": false,
                        "reason": "storage_failed",
                    }),
                );
                return true;
            } else {
                let iid = identity_id.to_string();
                let all = db::spawn_db(state.db.clone(), move |pool| {
                    db::list_game_sessions(&pool, &iid, None, None)
                })
                .await
                .unwrap_or_default();
                state.emit_to_all("all_game_sessions", all.into());
            }
            tracing::warn!(
                session = %short_id(&error.session_id),
                code = %error_payload["code"].as_str().unwrap_or("protocol_error"),
                action_kind = %error_payload["ref"].as_str().unwrap_or("action"),
                "Remote LRGP peer reported a protocol error"
            );
            state.emit_to_all(
                "game_protocol_error",
                json!({
                    "session_id": error.session_id,
                    "app_id": error.app_id,
                    "from": sender_hash,
                    "code": error_payload["code"],
                    "message": error_payload["msg"],
                    "ref": error_payload["ref"],
                }),
            );
            state.emit_to_all(
                "game_action_received",
                json!({
                    "session_id": session_id,
                    "app_id": app_id,
                    "command": command,
                    "from": sender_hash,
                    "applied": false,
                    "remote_error": true,
                }),
            );
            return true;
        }
        Err(error) => {
            tracing::warn!(
                target: "ttt_trace",
                step = "inbound.dispatched",
                valid = false,
                from = %short_id(sender_hash),
                reason = "dispatch_failed",
                "dispatch_incoming returned error"
            );
            let (code, public_message) = match &error {
                lrgp::protocol::LrgpError::UnknownApp(_) => ("unsupported_app", "Game unavailable"),
                lrgp::protocol::LrgpError::SessionExpired(_) => {
                    ("session_expired", "Game session expired")
                }
                lrgp::protocol::LrgpError::UnauthorizedPeer { .. } => {
                    ("unauthorized_sender", "Sender is not part of this game")
                }
                _ => ("protocol_error", "Action rejected"),
            };
            if matches!(&error, lrgp::protocol::LrgpError::SessionExpired(_)) {
                if let Some(Some(expired)) = state.lrgp_router.with_app(&app_id, |app| {
                    app.get_session_record(&session_id, identity_id)
                }) {
                    let _ = db::spawn_db(state.db.clone(), move |pool| {
                        db::save_game_session(&pool, &expired);
                    })
                    .await;
                }
            }
            send_lrgp_error_best_effort(
                state,
                LrgpErrorReply {
                    destination: sender_hash,
                    identity_id,
                    app_id: &app_id,
                    app_version,
                    session_id: &session_id,
                    rejected_command: &command,
                    code,
                    message: public_message,
                },
            );
            // Envelope parsed as LRGP; do not fall through to chat.
            return true;
        }
    };

    // Empty session_id can't address app_sessions PK; drop without DB write.
    if session_id.is_empty() {
        tracing::warn!(
            target: "ttt_trace",
            step = "inbound.empty_sid_rejected",
            from = %short_id(sender_hash),
            my = %short_id(identity_id),
            reason = "empty_session_id",
            "dropping inbound LRGP envelope with empty session_id"
        );
        return true;
    }

    tracing::info!(
        target: "ttt_trace",
        step = "inbound.dispatched",
        valid = true,
        from = %short_id(sender_hash),
        my = %short_id(identity_id),
        has_session = result.session.is_some(),
        has_emit = result.emit.is_some(),
        has_error = result.error.is_some(),
        "dispatch_incoming ok"
    );

    if let Some(error) = result.error.as_ref() {
        let code = error
            .get("code")
            .and_then(|value| value.as_str())
            .unwrap_or("protocol_error");
        let message = error
            .get("msg")
            .and_then(|value| value.as_str())
            .unwrap_or("Action rejected");
        tracing::warn!(
            session_id,
            command,
            code,
            "Rejected inbound LRGP action without mutating durable state"
        );
        send_lrgp_error_best_effort(
            state,
            LrgpErrorReply {
                destination: sender_hash,
                identity_id,
                app_id: &app_id,
                app_version,
                session_id: &session_id,
                rejected_command: &command,
                code,
                message,
            },
        );
        state.emit_to_all(
            "game_action_received",
            json!({
                "session_id": session_id,
                "app_id": app_id,
                "command": command,
                "from": sender_hash,
                "applied": false,
                "error": error,
            }),
        );
        return true;
    }

    let payload_json = result
        .emit
        .as_ref()
        .map(|e| serde_json::to_value(e).unwrap_or(json!({})))
        .unwrap_or(json!({}));
    let persisted_session = state
        .lrgp_router
        .with_app(&app_id, |app| {
            app.get_session_record(&session_id, identity_id)
        })
        .flatten();

    // Action allocation, the canonical session snapshot, and unread state are
    // committed together. A failed/duplicate commit rolls the in-memory app
    // back to its pre-dispatch snapshot before anything reaches the UI.
    let persisted = {
        let session_id = session_id.clone();
        let identity_id = identity_id.to_string();
        let sender_hash = sender_hash.to_string();
        let command = command.clone();
        let envelope_mp = envelope_mp.clone();
        let timestamp = msg.timestamp;
        let payload_json = serde_json::to_string(&payload_json).unwrap_or_else(|_| "{}".into());
        let session = persisted_session.clone();
        db::spawn_db(state.db.clone(), move |p| {
            let had_session = db::persist_inbound_game_action(
                &p,
                &session_id,
                &identity_id,
                &command,
                &payload_json,
                &sender_hash,
                timestamp,
                &envelope_mp,
                session.as_ref(),
            )?;

            let sessions = db::list_game_sessions(&p, &identity_id, Some(&sender_hash), None);
            let all = db::list_game_sessions(&p, &identity_id, None, None);
            Some((had_session, sessions, all))
        })
        .await
        .ok()
        .flatten()
    };

    let Some((had_session, sessions, all)) = persisted else {
        if state
            .lrgp_router
            .rollback_incoming(
                &app_id,
                &session_id,
                identity_id,
                &durable_nonce,
                router_snapshot,
            )
            .is_err()
        {
            tracing::error!(session_id, app_id, "Failed to roll back LRGP router state");
        }
        tracing::error!(
            session_id,
            app_id,
            command,
            "Rejected LRGP action because durable persistence did not commit"
        );
        state.emit_to_all(
            "game_action_received",
            json!({
                "session_id": session_id,
                "app_id": app_id,
                "command": command,
                "from": sender_hash,
                "applied": false,
                "reason": "storage_failed",
            }),
        );
        return true;
    };

    notify_game_if_background(
        state,
        sender_hash,
        &session_id,
        &app_id,
        &command,
        !had_session,
    );

    state.emit_to_all(
        "active_games",
        json!({"hash": sender_hash, "games": sessions}),
    );
    tracing::info!(
        target: "ttt_trace",
        step = "inbound.emitted_all",
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
            "applied": true,
        }),
    );

    true
}

#[cfg(test)]
mod packet_dispatch_tests {
    use super::*;

    #[test]
    fn lrgp_requires_a_signature_or_matching_identified_link() {
        let remote_identity = [0x42; 16];
        let remote_lxmf = Destination::hash_from_name_and_identity(
            LXMF_DELIVERY_APP_NAME,
            Some(&remote_identity),
        );
        let propagated = InboundLxmfSource::Propagated;
        assert!(lrgp_sender_authenticated(
            &propagated,
            &[0x99; 16],
            Some(true)
        ));
        assert!(!lrgp_sender_authenticated(&propagated, &remote_lxmf, None));

        let identified_link = InboundLxmfSource::Link {
            link_id: Some([0x11; 16]),
            remote_identity_hash: Some(remote_identity),
        };
        assert!(lrgp_sender_authenticated(
            &identified_link,
            &remote_lxmf,
            None
        ));
        assert!(!lrgp_sender_authenticated(
            &identified_link,
            &[0x99; 16],
            None
        ));
    }

    #[test]
    fn stale_game_recovery_only_targets_in_flight_delivery_states() {
        for state in [
            "pending",
            "sending",
            "routing",
            "link_establishing",
            "sending_via_link",
            "reusing_direct_link",
            "reusing_backchannel",
            "propagating",
            "sent",
        ] {
            assert!(game_delivery_state_is_in_flight(state), "{state}");
        }
        for state in [
            "",
            "delivered",
            "propagated",
            "failed",
            "rejected",
            "undelivered",
        ] {
            assert!(!game_delivery_state_is_in_flight(state), "{state}");
        }
    }

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
        fn try_emit(
            &self,
            event: &str,
            payload: serde_json::Value,
        ) -> Result<(), ratspeak_core::EmitError> {
            self.events
                .lock()
                .unwrap()
                .push((event.to_string(), payload));
            Ok(())
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

    #[tokio::test]
    async fn dedicated_opportunistic_proof_bypasses_destination_ingress_backpressure() {
        use rns_transport::link_messages::DestinationEvent;

        let (state, emitter) = pipeline_state();
        let (proof_tx, proof_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ordinary_tx, _ordinary_rx) = tokio::sync::mpsc::channel(1);
        let shutdown = rns_runtime::lifecycle::ShutdownSignal::new();
        ordinary_tx
            .send(DestinationEvent::DeliveryProof {
                msg_id: "11".repeat(32),
                rtt: None,
            })
            .await
            .unwrap();
        let task = tokio::spawn(handle_lxmf_delivery_proofs(state, proof_rx, shutdown));
        proof_tx
            .send(DestinationEvent::DeliveryProof {
                msg_id: "22".repeat(32),
                rtt: Some(Duration::from_millis(5)),
            })
            .unwrap();
        drop(proof_tx);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("dedicated proof handler must not wait on ordinary ingress")
            .unwrap();
        assert!(matches!(
            ordinary_tx.try_send(DestinationEvent::DeliveryProof {
                msg_id: "33".repeat(32),
                rtt: None,
            }),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_))
        ));
        assert_eq!(emitter.count("lxmf_step"), 0);
    }

    #[tokio::test]
    async fn receiver_delivery_proof_waits_for_transport_capacity() {
        let (transport_tx, mut transport_rx) = tokio::sync::mpsc::channel(1);
        transport_tx
            .send(rns_transport::messages::TransportMessage::RequestPath {
                destination_hash: [0x31; 16],
            })
            .await
            .unwrap();
        let proof_destination = [0x32; 16];
        let enqueue_tx = transport_tx.clone();
        let task = tokio::spawn(async move {
            enqueue_lxmf_delivery_proof(&enqueue_tx, vec![0xAA; 64], proof_destination).await
        });
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "proof enqueue must wait instead of drop"
        );

        assert!(matches!(
            transport_rx.recv().await,
            Some(rns_transport::messages::TransportMessage::RequestPath { .. })
        ));
        assert!(task.await.unwrap());
        match transport_rx.recv().await.unwrap() {
            rns_transport::messages::TransportMessage::Outbound(request) => {
                assert_eq!(request.destination_hash, proof_destination);
                assert_eq!(request.raw.as_ref(), &[0xAA; 64]);
            }
            _ => panic!("expected retained delivery proof outbound"),
        }
    }

    #[tokio::test]
    async fn unmatched_opportunistic_proof_does_not_emit_delivery() {
        let (state, emitter) = pipeline_state();
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let shutdown = rns_runtime::lifecycle::ShutdownSignal::new();
        let task = tokio::spawn(handle_inbound_lxmf(state, rx, shutdown));

        tx.send(
            rns_transport::link_messages::DestinationEvent::DeliveryProof {
                msg_id: "ab".repeat(32),
                rtt: Some(std::time::Duration::from_millis(25)),
            },
        )
        .await
        .unwrap();
        drop(tx);
        task.await.unwrap();

        assert_eq!(emitter.count("lxmf_step"), 0);
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
        let mut msg = lxmf_core::message_api::LxMessage::new(
            dest,
            src,
            "",
            content,
            lxmf_core::message_api::DeliveryMethod::Direct,
        );
        // Unsigned-by-unknown-sender: verify returns None and the message is
        // still delivered, so tests don't need real peer keys.
        msg.signature = Some([0u8; 64]);
        msg.pack().unwrap()
    }

    fn packed_inbound_with_audio(
        dest: [u8; 16],
        src: [u8; 16],
        mode: u8,
        audio_bytes: &[u8],
    ) -> Vec<u8> {
        let mut msg = lxmf_core::message_api::LxMessage::new(
            dest,
            src,
            "",
            "Voice message",
            lxmf_core::message_api::DeliveryMethod::Direct,
        );
        msg.set_audio_field(mode, audio_bytes).unwrap();
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

    // Regression guard for commit 4e7a71e: a path request for our own LXMF
    // destination must be answered with a PathResponse-context announce routed
    // back out the arrival interface. Before the fix the AnnounceRequested event
    // fell through to `_ => continue` and never produced a reply, so a peer that
    // never announced could not reach us on first contact.
    #[test]
    fn path_response_message_targets_arrival_interface() {
        let (state, _emitter) = pipeline_state();
        let dest = local_dest(&state);

        let tag = [0xA5; 16];
        let message = build_lxmf_path_response_message(&state, Some(7), Some(&tag))
            .expect("path-response message built");

        // Must be OutboundAttached on the arrival interface, NOT a broadcast —
        // upstream RNS answers a local path request only on that interface.
        let request = match message {
            rns_transport::messages::TransportMessage::OutboundAttached {
                request,
                interface_id,
            } => {
                assert_eq!(
                    interface_id, 7,
                    "answer must go back out the arrival interface"
                );
                request
            }
            other => panic!("expected OutboundAttached, got {other:?}"),
        };
        assert_eq!(
            request.destination_hash, dest,
            "answer is for our own LXMF destination"
        );

        let (header, _) = rns_wire::header::PacketHeader::unpack(&request.raw)
            .expect("unpack path-response announce");
        assert_eq!(
            header.flags.packet_type,
            rns_wire::flags::PacketType::Announce
        );
        assert_eq!(
            header.context,
            rns_wire::context::PacketContext::PathResponse,
            "must be a PathResponse announce, not a plain broadcast announce"
        );
        assert_eq!(header.destination_hash, dest);
    }

    // With no arrival interface the answer falls back to a broadcast Outbound
    // (still a PathResponse announce). Guards the Some/None routing branch.
    #[test]
    fn path_response_message_without_interface_broadcasts() {
        let (state, _emitter) = pipeline_state();
        let dest = local_dest(&state);

        let message = build_lxmf_path_response_message(&state, None, None)
            .expect("path-response message built");

        let request = match message {
            rns_transport::messages::TransportMessage::Outbound(request) => request,
            other => panic!("expected Outbound broadcast, got {other:?}"),
        };
        assert_eq!(request.destination_hash, dest);

        let (header, _) = rns_wire::header::PacketHeader::unpack(&request.raw)
            .expect("unpack path-response announce");
        assert_eq!(
            header.context,
            rns_wire::context::PacketContext::PathResponse
        );
    }

    #[test]
    fn path_response_message_preserves_tag_for_exact_replay() {
        let (state, _emitter) = pipeline_state();
        let tag = [0x5A; 16];

        let first = build_lxmf_path_response_message(&state, Some(3), Some(&tag))
            .expect("first path response");
        let second = build_lxmf_path_response_message(&state, Some(3), Some(&tag))
            .expect("cached path response");
        let raw = |message| match message {
            rns_transport::messages::TransportMessage::OutboundAttached { request, .. } => {
                request.raw
            }
            other => panic!("expected attached path response, got {other:?}"),
        };

        assert_eq!(
            raw(second),
            raw(first),
            "discarding AnnounceRequest.tag would defeat Reticulum path-response deduplication"
        );
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
    async fn unknown_audio_mode_is_bounded_first_class_media_not_an_attachment() {
        let (state, emitter) = pipeline_state();
        let data =
            packed_inbound_with_audio(local_dest(&state), [0xED; 16], 0xfe, b"future audio codec");

        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;

        let identity = local_identity(&state);
        let conversation = db::get_conversation(&state.db, &hex::encode([0xED; 16]), &identity, 10);
        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation[0]["audio"]["mode"], 0xfe);
        assert_eq!(conversation[0]["audio"]["supported"], false);
        assert!(conversation[0]["audio"]["stored_name"].is_string());
        assert!(conversation[0]["attachments"].is_null());
        assert!(conversation[0]["image"].is_null());

        let event = emitter
            .events
            .lock()
            .unwrap()
            .iter()
            .find(|(name, _)| name == "lxmf_message")
            .map(|(_, payload)| payload.clone())
            .unwrap();
        assert_eq!(event["audio"]["mode"], 0xfe);
        assert_eq!(event["audio"]["supported"], false);
        assert!(event.get("attachments").is_none());
    }

    #[tokio::test]
    async fn malformed_audio_does_not_reject_or_hide_the_enclosing_message() {
        let (state, emitter) = pipeline_state();
        let mut msg = lxmf_core::message_api::LxMessage::new(
            local_dest(&state),
            [0xEC; 16],
            "",
            "text survives malformed media",
            lxmf_core::message_api::DeliveryMethod::Direct,
        );
        msg.set_msgpack_field(
            lxmf_core::constants::FIELD_AUDIO,
            vec![0x93, lxmf_core::constants::AM_OPUS_OGG, 0xc4, 0x00, 0xc0],
        )
        .unwrap();
        msg.signature = Some([0u8; 64]);
        let data = msg.pack().unwrap();

        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;

        let identity = local_identity(&state);
        let conversation = db::get_conversation(&state.db, &hex::encode([0xEC; 16]), &identity, 10);
        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation[0]["content"], "text survives malformed media");
        assert!(conversation[0]["audio"].is_null());
        assert_eq!(emitter.count("lxmf_message"), 1);
    }

    #[tokio::test]
    async fn oversized_audio_retains_message_and_mode_without_persisting_bytes() {
        let (state, _emitter) = pipeline_state();
        let audio = vec![0x55; lxmf::MAX_AUDIO_FIELD_BYTES + 1];
        let data = packed_inbound_with_audio(local_dest(&state), [0xEB; 16], 0xfe, &audio);

        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;

        let identity = local_identity(&state);
        let conversation = db::get_conversation(&state.db, &hex::encode([0xEB; 16]), &identity, 10);
        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation[0]["audio"]["mode"], 0xfe);
        assert_eq!(conversation[0]["audio"]["unavailable"], true);
        assert!(conversation[0]["audio"].get("stored_name").is_none());
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

    /// Fake transport actor answering `IsBlackholed` with a fixed verdict.
    fn spawn_blackhole_transport(
        blackholed: bool,
    ) -> tokio::sync::mpsc::Sender<rns_transport::messages::TransportMessage> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if let rns_transport::messages::TransportMessage::Rpc { query, response_tx } =
                    message
                {
                    if matches!(
                        query,
                        rns_transport::messages::TransportQuery::IsBlackholed { .. }
                    ) {
                        let _ = response_tx.send(
                            rns_transport::messages::TransportQueryResponse::BoolResult(blackholed),
                        );
                    }
                }
            }
        });
        tx
    }

    fn set_blackhole_transport(
        state: &AppState,
        tx: Option<tokio::sync::mpsc::Sender<rns_transport::messages::TransportMessage>>,
    ) {
        state
            .lxmf
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .router
            .transport_tx = tx;
    }

    /// Register `src` as a known identity so the gate's recall resolves and
    /// signatures verify (x25519 half is irrelevant to both).
    fn register_source_identity(
        state: &AppState,
        src: [u8; 16],
        signing: &rns_crypto::ed25519::Ed25519PrivateKey,
    ) {
        let mut pub_key = [0u8; 64];
        pub_key[32..].copy_from_slice(&signing.public_key().to_bytes());
        state
            .lxmf
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .known_identities
            .insert(hex::encode(src), pub_key);
    }

    fn packed_signed_inbound(
        dest: [u8; 16],
        src: [u8; 16],
        content: &str,
        signing: &rns_crypto::ed25519::Ed25519PrivateKey,
    ) -> Vec<u8> {
        let mut msg = lxmf_core::message_api::LxMessage::new(
            dest,
            src,
            "",
            content,
            lxmf_core::message_api::DeliveryMethod::Direct,
        );
        msg.sign(signing).unwrap();
        msg.pack().unwrap()
    }

    /// Blackholed-source gate (LXMRouter.py:1739-1741): a resolvable source
    /// with a blackholed identity is dropped; the same peer delivers once the
    /// transport says not-blackholed.
    #[tokio::test]
    async fn blackholed_source_is_dropped() {
        let (state, emitter) = pipeline_state();
        let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
        let src = [0xBB; 16];
        register_source_identity(&state, src, &signing);

        set_blackhole_transport(&state, Some(spawn_blackhole_transport(true)));
        let data = packed_signed_inbound(local_dest(&state), src, "should vanish", &signing);
        handle_decrypted_lxmf(
            &state,
            data,
            InboundLxmfSource::Link {
                link_id: None,
                remote_identity_hash: None,
            },
        )
        .await;
        assert_eq!(message_rows(&state), 0, "blackholed source must be dropped");
        assert_eq!(emitter.count("lxmf_message"), 0);

        set_blackhole_transport(&state, Some(spawn_blackhole_transport(false)));
        let data = packed_signed_inbound(local_dest(&state), src, "now allowed", &signing);
        handle_decrypted_lxmf(
            &state,
            data,
            InboundLxmfSource::Link {
                link_id: None,
                remote_identity_hash: None,
            },
        )
        .await;
        assert_eq!(message_rows(&state), 1, "non-blackholed source delivers");
    }

    /// Unknown sources pass the gate without a transport query, mirroring
    /// Python's recall-gated check (LXMessage.py:803-805).
    #[tokio::test]
    async fn unknown_source_passes_blackhole_gate() {
        let (state, _emitter) = pipeline_state();
        set_blackhole_transport(&state, Some(spawn_blackhole_transport(true)));

        let data = packed_inbound(local_dest(&state), [0xEE; 16], "unknown sender");
        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;

        assert_eq!(message_rows(&state), 1);
    }

    /// Query failure fails open: a dead transport channel and a dropped
    /// response both deliver instead of dropping.
    #[tokio::test]
    async fn blackhole_query_failure_fails_open() {
        let (state, _emitter) = pipeline_state();
        let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
        let src = [0xBB; 16];
        register_source_identity(&state, src, &signing);

        // Transport channel closed.
        let (dead_tx, dead_rx) =
            tokio::sync::mpsc::channel::<rns_transport::messages::TransportMessage>(1);
        drop(dead_rx);
        set_blackhole_transport(&state, Some(dead_tx));
        let data = packed_signed_inbound(local_dest(&state), src, "channel closed", &signing);
        handle_decrypted_lxmf(
            &state,
            data,
            InboundLxmfSource::Link {
                link_id: None,
                remote_identity_hash: None,
            },
        )
        .await;
        assert_eq!(message_rows(&state), 1);

        // Query accepted but the response never arrives.
        let (mute_tx, mut mute_rx) =
            tokio::sync::mpsc::channel::<rns_transport::messages::TransportMessage>(1);
        tokio::spawn(async move { while mute_rx.recv().await.is_some() {} });
        set_blackhole_transport(&state, Some(mute_tx));
        let data = packed_signed_inbound(local_dest(&state), src, "response dropped", &signing);
        handle_decrypted_lxmf(
            &state,
            data,
            InboundLxmfSource::Link {
                link_id: None,
                remote_identity_hash: None,
            },
        )
        .await;
        assert_eq!(message_rows(&state), 2);
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
        handle_decrypted_lxmf(
            &state,
            link_data,
            InboundLxmfSource::Link {
                link_id: None,
                remote_identity_hash: None,
            },
        )
        .await;

        let prop_data = packed_inbound(dest, [0xE2; 16], "via propagation");
        handle_decrypted_lxmf(&state, prop_data, InboundLxmfSource::Propagated).await;

        let opp_data = packed_inbound(dest, [0xE3; 16], "via opportunistic");
        let msg = lxmf_core::message_api::LxMessage::unpack(&opp_data).unwrap();
        process_inbound_lxmf(
            &state,
            msg,
            &opp_data,
            InboundLxmfSource::Opportunistic { raw: Bytes::new() },
            state.activity_request_fence(),
        )
        .await;

        assert_eq!(message_rows(&state), 0, "all unstamped sources rejected");
        assert_eq!(emitter.count("lxmf_message"), 0);

        // Enforcement off again: the same wire bytes deliver.
        state
            .enforce_stamps
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let data = packed_inbound(dest, [0xE1; 16], "via link");
        handle_decrypted_lxmf(
            &state,
            data,
            InboundLxmfSource::Link {
                link_id: None,
                remote_identity_hash: None,
            },
        )
        .await;
        assert_eq!(message_rows(&state), 1);
    }

    /// The Link source persists like Propagated (shared pipeline).
    #[tokio::test]
    async fn link_source_persists_and_emits() {
        let (state, emitter) = pipeline_state();
        let data = packed_inbound(local_dest(&state), [0xEE; 16], "direct hello");

        handle_decrypted_lxmf(
            &state,
            data,
            InboundLxmfSource::Link {
                link_id: None,
                remote_identity_hash: None,
            },
        )
        .await;

        assert_eq!(message_rows(&state), 1);
        assert_eq!(emitter.count("lxmf_message"), 1);
    }

    #[tokio::test]
    async fn reaction_routes_to_reaction_store_not_conversation() {
        let (state, emitter) = pipeline_state();
        let target_id = hex::encode([0xAB; 32]);

        let mut msg = lxmf_core::message_api::LxMessage::new(
            local_dest(&state),
            [0xEE; 16],
            "",
            "",
            lxmf_core::message_api::DeliveryMethod::Direct,
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

    /// A standard-only peer's FIELD_REACTION (native msgpack dict, no
    /// 0xFB/0xFC envelope) routes to the reaction store like ours.
    #[tokio::test]
    async fn standard_field_reaction_routes_to_reaction_store() {
        let (state, emitter) = pipeline_state();
        let target_hash = [0xAB; 32];

        let mut msg = lxmf_core::message_api::LxMessage::new(
            local_dest(&state),
            [0xEE; 16],
            "",
            "Reacted to your message with \u{1F44D}.",
            lxmf_core::message_api::DeliveryMethod::Direct,
        );
        let dict_value = rmpv::Value::Map(vec![
            (
                rmpv::Value::from(lxmf_core::constants::REACTION_TO as u64),
                rmpv::Value::Binary(target_hash.to_vec()),
            ),
            (
                rmpv::Value::from(lxmf_core::constants::REACTION_CONTENT as u64),
                rmpv::Value::Binary("\u{1F44D}".as_bytes().to_vec()),
            ),
        ]);
        let mut dict = Vec::new();
        rmpv::encode::write_value(&mut dict, &dict_value).unwrap();
        msg.set_msgpack_field(lxmf_core::constants::FIELD_REACTION, dict)
            .unwrap();
        msg.signature = Some([0u8; 64]);
        let data = msg.pack().unwrap();

        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;

        assert_eq!(reaction_rows(&state), 1, "standard reaction recorded");
        assert_eq!(message_rows(&state), 0, "reactions never hit the chat log");
        assert_eq!(emitter.count("reaction_update"), 1);
        let stored: String = state
            .db
            .get()
            .unwrap()
            .query_row("SELECT message_id FROM reactions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, hex::encode(target_hash), "keys on full-hash hex id");
    }

    /// A standard-only peer's FIELD_REPLY_TO/FIELD_REPLY_QUOTE (e.g. MeshChatX
    /// replies) persists as reply metadata on the saved message.
    #[tokio::test]
    async fn standard_field_reply_persists_reply_metadata() {
        let (state, emitter) = pipeline_state();
        let target_hash = [0xCD; 32];

        let mut msg = lxmf_core::message_api::LxMessage::new(
            local_dest(&state),
            [0xEE; 16],
            "",
            "standard reply",
            lxmf_core::message_api::DeliveryMethod::Direct,
        );
        msg.set_field(lxmf_core::constants::FIELD_REPLY_TO, target_hash.to_vec());
        msg.set_field(lxmf_core::constants::FIELD_REPLY_QUOTE, b"quoted".to_vec());
        msg.signature = Some([0u8; 64]);
        let data = msg.pack().unwrap();

        handle_decrypted_lxmf(
            &state,
            data,
            InboundLxmfSource::Link {
                link_id: None,
                remote_identity_hash: None,
            },
        )
        .await;

        assert_eq!(message_rows(&state), 1);
        assert_eq!(emitter.count("lxmf_message"), 1);
        let (reply_to, preview): (String, String) = state
            .db
            .get()
            .unwrap()
            .query_row(
                "SELECT reply_to_id, reply_to_preview FROM messages",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(reply_to, hex::encode(target_hash));
        assert_eq!(preview, "quoted");
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

    #[test]
    fn attachment_notifications_hide_wire_fallback_without_legacy_media_inference() {
        assert_eq!(
            notification_body("[File: field-notes.pdf]", true),
            "New attachment"
        );
        assert_eq!(
            notification_body("Please review\n[File: field-notes.pdf]", true),
            "Please review"
        );
    }

    #[tokio::test]
    async fn inbound_message_notifies_only_when_backgrounded_and_enabled() {
        let notifier = Arc::new(RecordingNotifier::default());
        let state = make_state(notifier.clone());
        state
            .is_foreground
            .store(false, std::sync::atomic::Ordering::Relaxed);
        state.set_notification_foreground(false);
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
        state.set_notification_foreground(true);
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
        state.set_notification_foreground(false);
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
    fn native_notification_owner_follows_immediate_platform_visibility() {
        let notifier = Arc::new(RecordingNotifier::default());
        let state = make_state(notifier.clone());
        let notification = ratspeak_core::NativeNotification::message(
            "Message from Alice",
            "hello",
            "lxmf:abcd1234abcd1234",
            42,
        );

        // AppState starts foregrounded. Even a caller that forgets its own
        // early lifecycle check cannot leak an OS notification.
        state.emit_native_notification(notification.clone());
        assert!(notifier.notifications.lock().unwrap().is_empty());

        // A platform background edge owns notification attention immediately,
        // even while the slower transport lifecycle state is still foreground.
        state.set_notification_foreground(false);
        state.emit_native_notification(notification.clone());
        assert_eq!(notifier.notifications.lock().unwrap().len(), 1);

        // The inverse edge suppresses immediately as the app becomes visible.
        state.set_notification_foreground(true);
        state.emit_native_notification(notification);
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
        state.set_notification_foreground(false);
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
    fn game_notifications_use_registered_manifest_names() {
        let state = make_state(Arc::new(RecordingNotifier::default()));
        assert_eq!(game_name(&state, "ttt"), "Tic-Tac-Toe");
        assert_eq!(game_name(&state, "chess"), "Chess");
        assert_eq!(game_name(&state, "four_in_a_row"), "Four in a Row");
        assert_eq!(game_name(&state, "future-game"), "a game");
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

    #[test]
    fn interface_reannounce_respects_never_suppression_and_cooldown() {
        assert!(!should_reannounce_for_interface_online(
            true, false, 0, true
        ));
        assert!(!should_reannounce_for_interface_online(
            true, true, 1800, true
        ));
        assert!(!should_reannounce_for_interface_online(
            true, false, 1800, false
        ));
        assert!(!should_reannounce_for_interface_online(
            false, false, 1800, true
        ));
        assert!(should_reannounce_for_interface_online(
            true, false, 1800, true
        ));
    }
}
