//! Ratspeak runtime: RNS + LXMF + LRGP wiring, AppState, async loops.
//! Depends on `ratspeak-core` + `ratspeak-db` plus the protocol crates.
//! Holds zero `tauri::*` — emits go through `ratspeak_core::Emitter`.

// Holding `std::sync::MutexGuard` / `RwLockGuard` across `.await` breaks
// `Send` bounds or stalls the executor.
#![warn(clippy::await_holding_lock)]

pub mod activity;
pub mod announce;
pub mod announce_handlers;
pub mod announce_pacing;
mod announce_stamps;
pub mod blackhole;
pub mod channel_hub;
pub mod channels;
#[cfg(feature = "hardware")]
pub mod hardware;
pub mod helpers;
mod hosted_propagation;
pub mod identity_prune;
pub mod image_attachment;
mod inbound_admission;
pub mod interface_detach;
pub mod interface_lifecycle;
mod link_accounting;
pub mod lxmf;
pub mod lxmf_persistence;
mod markdown_plain;
pub mod messaging;
pub mod mobile_platform;
pub mod network_migration;
pub mod network_ownership;
mod network_secrets;
mod outbox;
pub mod owner_lock;
pub mod paper;
mod path_response;
pub mod propagation;
pub mod propagation_recovery;
mod rnode_activity;
pub mod rnode_status;
pub mod rns;
pub mod rns_config;
pub mod rrc;
pub mod rsnode;
pub mod state;
mod storage_errors;
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
/// The wrapper the staged unlock secret and other held secrets use.
pub use zeroize::Zeroizing;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytes::Bytes;
use ratspeak_core::delivery::{AfterFailure, OutboundState, PROGRESS_STEPS};
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
const ANNOUNCE_STAMP_POLL_INTERVAL: Duration = Duration::from_millis(250);
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

/// The kind of connection the path to `destination` used at the last stats
/// poll. Takes the LXMF lock, so never call it with that lock held.
fn path_interface_class(state: &AppState, destination: &str) -> Option<producer::InterfaceClass> {
    let destination: [u8; 16] = hex::decode(destination).ok()?.try_into().ok()?;
    let name = state
        .lxmf
        .lock()
        .ok()?
        .as_ref()?
        .route_entry(&destination)?
        .interface
        .clone();
    let stats = state.last_stats.read().ok()?;
    interface_class_on_path(stats.as_ref()?, &name)
}

fn interface_class_on_path(stats: &Value, name: &str) -> Option<producer::InterfaceClass> {
    use producer::InterfaceClass::*;
    let rows = stats["interface_stats"]["interfaces"].as_array()?;
    let row = |name: &str| rows.iter().find(|row| row["name"].as_str() == Some(name));
    let mut row = row(name)?;
    // A client a listener spawned takes the listener's kind.
    if let Some(parent) = row["parent"].as_str() {
        row = rows
            .iter()
            .find(|candidate| candidate["name"].as_str() == Some(parent))?;
    }
    match row["type"].as_str() {
        Some("TCPClientInterface") => Some(TcpClient),
        Some("TCPServerInterface") => Some(TcpServer),
        Some("BackboneClientInterface") => Some(BackboneClient),
        Some("BackboneInterface") => Some(BackboneServer),
        Some("RNodeInterface" | "RNodeMultiInterface" | "RNodeSubInterface") => Some(RNode),
        Some("AutoInterface" | "AutoInterfacePeer") => Some(Auto),
        None if matches!(row["name"].as_str(), Some("Bluetooth Peer" | "BLE Mesh")) => {
            Some(BluetoothPeer)
        }
        _ => None,
    }
}

/// Activity for a game action this device sent.
pub fn record_game_action(
    state: &AppState,
    origin: ActivityRequestFence,
    msg_id: &str,
    meta: &crate::state::LrgpMsgMeta,
    stage: producer::LrgpActionStage,
) {
    record_activity_if_current(state, origin, || {
        Ok(producer::lrgp_action(producer::LrgpAction {
            message: producer::MessageId::from_hex(msg_id)?,
            destination: producer::DestinationHash::from_hex(&meta.contact_hash)?,
            stage,
            app: producer::lrgp_app_code(&meta.app_id),
            command: producer::lrgp_command_code(&meta.command),
        }))
    });
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

#[cfg(test)]
mod path_interface_tests {
    use super::*;

    fn stats() -> Value {
        json!({
            "interface_stats": { "interfaces": [
                { "name": "Ruby", "type": "TCPClientInterface", "parent": null },
                { "name": "Hub", "type": "TCPServerInterface", "parent": null },
                { "name": "Client on Hub", "type": "TCPClientInterface", "parent": "Hub" },
                { "name": "Bluetooth Peer", "type": null, "parent": null },
                { "name": "Pipe", "type": "PipeInterface", "parent": null },
            ] },
        })
    }

    #[test]
    fn a_path_names_its_kind_of_connection() {
        use producer::InterfaceClass::*;
        let class = |iface| interface_class_on_path(&stats(), iface);
        assert!(matches!(class("Ruby"), Some(TcpClient)));
        assert!(
            matches!(class("Client on Hub"), Some(TcpServer)),
            "a spawned client is its listener's"
        );
        assert!(matches!(class("Bluetooth Peer"), Some(BluetoothPeer)));
        assert!(class("Pipe").is_none(), "no kind to name");
        assert!(class("Gone").is_none(), "an interface no longer running");
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
        Some(f64::from(stamp_cost))
    } else {
        None
    };
    mgr.announce_ratspeak_usage = state.announce_ratspeak_usage_enabled();
    mgr.set_delivery_limit_kb(state.lxmf_delivery_limit_kb());
    mgr.keep_trying = state.lxmf_keep_trying_enabled();

    let hosting = state.effective_propagation_hosting();
    let pn_cost = state
        .propagation_node_stamp_cost
        .load(std::sync::atomic::Ordering::Relaxed);
    mgr.router.set_propagation_enabled(hosting);
    mgr.router.set_stamp_requirements(
        f64::from(pn_cost),
        lxmf_core::constants::PROPAGATION_COST_FLEX,
    );
}

/// Path responses must advertise the same live node policy as our ordinary
/// propagation announces. An empty response replaces Python peers' cached
/// app_data and prevents them from determining the required propagation cost.
fn install_propagation_announce_metadata(
    manager: &mut rns_runtime::link_manager::LinkManager,
    state: &Arc<AppState>,
    destination_hash: [u8; 16],
) {
    let shutdown = state
        .session_shutdown
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let state = Arc::downgrade(state);
    if let Some(destination) = manager.destination_mut() {
        destination.set_default_app_data(rns_identity::destination::DefaultAppData::Dynamic(
            Box::new(move || {
                if shutdown.is_triggered() {
                    return None;
                }
                let state = state.upgrade()?;
                if !state.effective_propagation_hosting() {
                    return None;
                }
                let owner = state.lxmf.lock().ok()?;
                let owner = owner
                    .as_ref()
                    .filter(|owner| owner.propagation_dest_hash == destination_hash)?;
                Some(owner.router.get_propagation_node_app_data())
            }),
        ));
    }
}

/// Keep the validated propagation stamp in canonical node storage. The node
/// strips this stamp when serving a client, including after a process restart.
/// `None` rejects invalid proof data; `Some(false)` is a valid but unaccepted
/// deposit (for example a duplicate or a storage limit).
#[cfg(test)]
fn accept_hosted_propagation_deposit(
    node: &mut lxmf_core::propagation_node::PropagationNode,
    entry: &[u8],
) -> Option<bool> {
    let (_transient_id, lxmf_data, stamp_value, stamp_data) =
        lxmf_core::stamper::validate_pn_stamp(entry, node.min_stamp_cost())?;
    Some(node.accept_stamped_propagated_blob(&lxmf_data, &stamp_data, stamp_value))
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

/// The label `contact_label_from_db` gives, read off the lifecycle lock.
pub(crate) async fn read_contact_label(
    state: &AppState,
    source_hash: &str,
    identity_id: &str,
) -> String {
    let source = source_hash.to_string();
    let identity = identity_id.to_string();
    db::spawn_db(state.db.clone(), move |p| {
        contact_label_from_db(&p, &source, &identity)
    })
    .await
    .unwrap_or_else(|_| compact_hash_label(source_hash))
}

pub(crate) fn contact_label_from_db(
    pool: &db::DbPool,
    source_hash: &str,
    identity_id: &str,
) -> String {
    contact_name_from_db(pool, source_hash, identity_id)
        .unwrap_or_else(|| compact_hash_label(source_hash))
}

/// Contact name, else announced name.
pub(crate) fn contact_name_from_db(
    pool: &db::DbPool,
    source_hash: &str,
    identity_id: &str,
) -> Option<String> {
    if let Some(label) = db::get_contact(pool, source_hash, identity_id).and_then(|c| {
        c.get("display_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    }) {
        return Some(label);
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
}

fn notification_body(content: &str, renderer: Option<u8>, has_attachment: bool) -> String {
    let plain;
    let content = if renderer == Some(lxmf_core::constants::RENDERER_MARKDOWN) {
        // The preview keeps 120 chars; bound the stripping work to a short head.
        let head = (0..=content.len().min(4096))
            .rev()
            .find(|&end| content.is_char_boundary(end))
            .map_or("", |end| &content[..end]);
        plain = markdown_plain::markdown_to_plain_text(head);
        plain.as_str()
    } else {
        content
    };
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

/// Call while the message's session is current and its row is stored.
pub(crate) fn notify_inbound_message_if_background(
    state: &AppState,
    source_hash: &str,
    label: &str,
    content: &str,
    renderer: Option<u8>,
    has_attachment: bool,
) {
    if !state.should_surface_native_notification() {
        return;
    }

    state.emit_native_notification(ratspeak_core::NativeNotification::message(
        format!("Message from {label}"),
        notification_body(content, renderer, has_attachment),
        format!("lxmf:{source_hash}"),
        stable_notification_id(source_hash, 1_000),
    ));
}

/// A game action's alert, raised once its session's publication is due.
/// `label` names the sender from the receiver's contacts, read off the lock.
pub(crate) struct GameAlert {
    pub(crate) label: String,
    pub(crate) sender_hash: String,
    pub(crate) session_id: String,
    pub(crate) app_id: String,
    pub(crate) command: String,
    pub(crate) is_new_session: bool,
}

/// What an inbound game action shows. Gathered under the game transaction
/// and published after it, in identity-then-game lock order.
#[derive(Default)]
struct LrgpOutcome {
    events: Vec<(&'static str, Value)>,
    alert: Option<GameAlert>,
}

impl LrgpOutcome {
    fn emit(&mut self, event: &'static str, payload: Value) {
        self.events.push((event, payload));
    }

    /// Names the alert's sender, off the lifecycle lock.
    async fn label_alert(&mut self, state: &AppState, identity_id: &str) {
        if let Some(alert) = self.alert.as_mut() {
            alert.label = read_contact_label(state, &alert.sender_hash, identity_id).await;
        }
    }

    fn publish(self, state: &AppState) {
        if let Some(alert) = &self.alert {
            notify_game_if_background(state, alert);
        }
        for (event, payload) in self.events {
            state.emit_to_all(event, payload);
        }
    }
}

fn game_name(state: &AppState, app_id: &str) -> String {
    state
        .lrgp_router
        .with_app(app_id, |app| app.manifest().display_name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "a game".to_string())
}

fn notify_game_if_background(state: &AppState, alert: &GameAlert) {
    if !state.should_surface_native_notification() {
        return;
    }

    let label = &alert.label;
    let game = game_name(state, &alert.app_id);
    let command = alert.command.as_str();
    let session_id = alert.session_id.as_str();
    let is_challenge = alert.is_new_session
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

/// Counts a soft restart for as long as it runs.
struct RestartInFlight<'a>(&'a std::sync::atomic::AtomicUsize);

impl<'a> RestartInFlight<'a> {
    fn begin(counter: &'a std::sync::atomic::AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}

impl Drop for RestartInFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Soft-restart: serialize identity lifecycle, stop RNS/LXMF, then re-init.
/// Activity reset failure leaves the current protocol runtime untouched.
pub async fn restart_rns_lxmf(state: Arc<AppState>) -> Result<(), ActivityRecorderError> {
    // Sends wait for a restart instead of failing in its gap.
    let _in_flight = RestartInFlight::begin(&state.runtime_restarts_in_flight);
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let _auto_ownership = state.auto_interface_lock.lock().await;
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

/// A panic under the LXMF manager lock leaves it poisoned and the manager
/// half-updated, so every later send fails until the app restarts. Clear the
/// poison and rebuild the session once; a panic that keeps coming back ends
/// with messaging stopped rather than a restart loop.
pub fn recover_lxmf_after_panic(state: &Arc<AppState>, cause: &'static str) {
    use state::LxmfRecoveryClaim;
    let claim = state.lxmf_panic_recovery.claim();
    if claim == LxmfRecoveryClaim::AlreadyRunning {
        return;
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        // The panic hook has logged where it happened.
        state.lxmf.clear_poison();
        if claim == LxmfRecoveryClaim::Restart {
            tracing::error!(cause, "messaging panicked; restarting it");
            if let Err(error) = restart_rns_lxmf(Arc::clone(&state)).await {
                tracing::error!(%error, "messaging restart after a panic was refused");
            }
        } else {
            tracing::error!(
                cause,
                "messaging panicked again after repeated restarts; stopping it"
            );
            if let Err(error) = shutdown_rns_lxmf(&state).await {
                tracing::error!(%error, "messaging stop after repeated panics was refused");
            }
            state.set_startup_stage("error");
            state.emit_to_all(
                "system_status",
                json!({"status": "error", "message": "Messaging stopped after repeated failures. Restart Ratspeak."}),
            );
        }
        state.lxmf_panic_recovery.finish();
    });
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
                .any(crate::rns_config::interface_entry_enabled)
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

/// The public servers, by endpoint: the Transport Mode limit counts them, and
/// the dashboard's Connect sheet lists the same ones.
const PUBLIC_TCP_ENDPOINTS: &[(&str, u16, &str)] = &[
    ("1.ratspeak.org", 4141, "ratspeak-ruby"),
    ("2.ratspeak.org", 4242, "ratspeak-emerald"),
    ("rns.ratspeak.org", 4242, "ratspeak-emerald"),
    ("3.ratspeak.org", 4343, "ratspeak-diamond"),
    ("rns.not-a-number.io", 4242, "apollo-rns"),
    ("rns.beleth.net", 4242, "beleth"),
    ("rns.michmesh.net", 7822, "michmesh"),
    ("rns.one-big.network", 4242, "one-big-network"),
    ("rmap.world", 4242, "rmap"),
    ("swiss-rns.cl0ud.ch", 4242, "swiss-rns"),
    ("rns.utn.lol", 4965, "utn-oregon"),
    ("reticulum.washmesh.net", 7242, "washmesh"),
];

/// The interface groups whose entries connect out to a `target_host`.
const PUBLIC_TCP_CLIENT_GROUPS: [&str; 2] = ["tcp_client", "backbone_client"];

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

/// The public server at `host:port`, when it is one.
pub fn public_tcp_server_id(host: &str, port: u16) -> Option<&'static str> {
    let host = normalise_public_tcp_host(host);
    PUBLIC_TCP_ENDPOINTS
        .iter()
        .find_map(|(public_host, public_port, id)| {
            (host == *public_host && port == *public_port).then_some(*id)
        })
}

/// The public server a TCP or Backbone client entry connects to.
pub fn public_tcp_server_id_from_entry(entry: &Value) -> Option<&'static str> {
    public_tcp_server_id(
        &startup_cfg_str(entry, "target_host")?,
        startup_cfg_u16(entry, "target_port")?,
    )
}

/// The public servers the enabled entries of `ifaces` connect to, each once
/// however many entries of either type reach it, with the entry
/// `replace_name` left out and `candidate` added.
pub fn projected_enabled_public_tcp_server_ids(
    ifaces: &Value,
    replace_name: Option<&str>,
    candidate: Option<&'static str>,
) -> Vec<&'static str> {
    let mut ids = Vec::new();
    let entries = PUBLIC_TCP_CLIENT_GROUPS
        .into_iter()
        .filter_map(|group| ifaces.get(group).and_then(Value::as_array))
        .flatten()
        .filter(|entry| {
            replace_name.is_none_or(|name| entry.get("name").and_then(Value::as_str) != Some(name))
                && crate::rns_config::interface_entry_enabled(entry)
        });
    for id in entries
        .filter_map(public_tcp_server_id_from_entry)
        .chain(candidate)
    {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

fn startup_auto_transport_enabled_for_interfaces(ifaces: &Value, network_type: &str) -> bool {
    startup_transport_auto_network_allows(network_type)
        && startup_has_enabled_non_lora_transport_interface(ifaces)
        && !startup_has_enabled_lora_interface(ifaces)
        && projected_enabled_public_tcp_server_ids(ifaces, None, None).len() <= 1
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
    shutdown_rns_lxmf_inner(state, true).await
}

pub(crate) async fn shutdown_rns_lxmf_inner(
    state: &Arc<AppState>,
    relock_identity: bool,
) -> Result<(), ActivityRecorderError> {
    let reset = reset_activity_for_shutdown(state).await?;
    finish_shutdown_inner(state, &reset, relock_identity).await;
    Ok(())
}

/// An acknowledged Activity reset, which a shutdown begins with.
#[must_use = "finish the shutdown it began"]
pub struct ActivityReset {
    status: activity::ActivityStatusV1,
}

/// The user's Activity choice; Normal capture runs until they stop it.
pub const ACTIVITY_CAPTURE_SETTING: &str = "activity_capture";

/// Starts Normal capture for a new runtime session unless the user stopped
/// Activity. A shutdown resets capture, so every session starts it again.
pub async fn start_activity_unless_stopped(state: &AppState) {
    let stopped = db::spawn_db(state.db.clone(), |p| {
        db::get_setting(&p, ACTIVITY_CAPTURE_SETTING)
    })
    .await
    .ok()
    .flatten()
    .is_some_and(|choice| choice == "off");
    if stopped {
        return;
    }
    let _activity_control = state.activity_control_lock.lock().await;
    if state.activity.status().state() == activity::ActivityCaptureState::Off {
        match state.activity.start().await {
            Ok(_) => tracing::info!("Activity capture started"),
            Err(error) => tracing::warn!(?error, "Activity capture did not start"),
        }
    }
}

/// The first step of a shutdown and the only one that can be refused, which
/// leaves the runtime as it was. An activation resets before it starts its
/// session, so a runtime never outlives the session it ran in.
pub async fn reset_activity_for_shutdown(
    state: &AppState,
) -> Result<ActivityReset, ActivityRecorderError> {
    let _activity_control = state.activity_control_lock.lock().await;
    state.bump_activity_boundary_generation();
    state
        .network_log_enabled
        .store(false, std::sync::atomic::Ordering::Release);
    if let Ok(mut level) = state.network_log_level.write() {
        *level = "standard".to_string();
    }
    let status = state.activity.hard_reset().await?;
    Ok(ActivityReset { status })
}

/// The rest of the shutdown `reset` began. Nothing here is refused.
pub async fn finish_shutdown(state: &Arc<AppState>, reset: &ActivityReset) {
    finish_shutdown_inner(state, reset, true).await;
}

async fn finish_shutdown_inner(
    state: &Arc<AppState>,
    reset: &ActivityReset,
    relock_identity: bool,
) {
    {
        let _activity_control = state.activity_control_lock.lock().await;
        // The worker's status notification makes recovery observable. This
        // second boundary gives frontend privacy teardown one acknowledged,
        // cross-runtime fence for the temporary one-shot legacy broadcasts.
        state.emit_to_all(
            "activity_boundary_v1",
            json!({
                "version": 1,
                "kind": "hard_reset",
                "identity_generation": state.current_identity_session_generation().to_string(),
                "capture_generation": reset.status.ingress_generation().to_string(),
                "status": reset.status,
            }),
        );
    }

    state.emit_to_all("system_status", json!({"status": "stopping"}));
    // A poisoned manager lock would make every step below skip the manager.
    state.lxmf.clear_poison();
    #[cfg(feature = "lxst-voice")]
    voice_memo::cancel_recording(state).await.ok();
    #[cfg(feature = "lxst-voice")]
    voice::shutdown_voice_service(state).await;
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
    let outbound_workers = state
        .lxmf
        .lock()
        .ok()
        .and_then(|mut manager| {
            let manager = manager.as_mut()?;
            manager.pause_download_admission();
            Some(manager.cancel_and_take_propagation_workers())
        })
        .unwrap_or_default();
    for worker in outbound_workers {
        if let Err(error) = worker.await {
            tracing::warn!(%error, "outbound preparation worker stopped");
        }
    }
    let _download_jobs = inbound_admission::quiesce().await;
    let _hosted_jobs = hosted_propagation::quiesce().await;
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
    state.lxmf.clear_poison();
    *state.lxmf.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // All signing loops are down — re-lock the token (drops the on-card PIN cache).
    if let Some(id) = hw_identity.filter(|_| relock_identity) {
        id.lock();
    }
    state.clear_identity_scoped_runtime_state();
    tokio::time::sleep(Duration::from_millis(300)).await;
    state.set_startup_stage("stopped");
    state.emit_to_all("system_status", json!({"status": "stopped"}));
}

/// Ask Reticulum which ratchet it holds for each destination
/// (`RNS.Identity.get_ratchet`). A destination it gave no answer for is left out.
pub(crate) async fn recall_ratchets(
    state: &AppState,
    destinations: Vec<[u8; 16]>,
) -> std::collections::HashMap<[u8; 16], Option<[u8; 32]>> {
    let mut answers = std::collections::HashMap::new();
    let handle = state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.clone()));
    let Some(handle) = handle else {
        return answers;
    };
    for dest in destinations {
        let query = rns_transport::messages::TransportQuery::RecallDestination { dest };
        if let Ok(Some(rns_transport::messages::TransportQueryResponse::RecalledDestination(
            recalled,
        ))) =
            tokio::time::timeout(Duration::from_millis(250), handle.query_transport(query)).await
        {
            answers.insert(dest, recalled.and_then(|recalled| recalled.ratchet));
        }
    }
    answers
}

/// Before a tick, give the manager Reticulum's current ratchet for every
/// destination a pending message may be encrypted for, as `Destination.encrypt`
/// reads it at each send. Reticulum alone keeps received ratchets.
async fn refresh_outbound_ratchets(state: &AppState) {
    let pending = state.lxmf.lock().ok().and_then(|lxmf| {
        lxmf.as_ref()
            .map(|mgr| (mgr.identity_hash.clone(), mgr.outbound_ratchet_targets()))
    });
    let Some((identity, targets)) = pending else {
        return;
    };
    let answers = recall_ratchets(state, targets).await;
    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
        && mgr.identity_hash == identity
    {
        mgr.set_outbound_ratchets(answers);
    }
}

/// Retain each destination the router delivered to in Reticulum, through the
/// shared instance in client mode, as Python's router does (LXMRouter.py).
async fn forward_destination_retains(state: &AppState) {
    let Some(handle) = state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.clone()))
    else {
        return;
    };
    let retains = state
        .lxmf
        .lock()
        .ok()
        .and_then(|mut lxmf| {
            lxmf.as_mut()
                .map(|mgr| mgr.router.take_destination_retains())
        })
        .unwrap_or_default();
    // A runtime still current after the drain is the drained manager's own.
    if retains.is_empty() || !is_current_network_runtime(state, &handle) {
        return;
    }
    for dest in retains {
        let query = rns_transport::messages::TransportQuery::RetainDestination { dest };
        let retained =
            tokio::time::timeout(Duration::from_millis(250), handle.query_control(query)).await;
        if !matches!(
            retained,
            Ok(Some(
                rns_transport::messages::TransportQueryResponse::BoolResult(true)
            ))
        ) {
            tracing::debug!(
                dest = %short_id(&hex::encode(dest)),
                "Reticulum did not retain a delivered destination"
            );
        }
    }
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

/// The auto-lock setting, `hardware_session_timeout` in seconds; zero is off.
fn auto_lock_timeout(state: &AppState) -> Duration {
    Duration::from_secs(
        db::get_setting(&state.db, "hardware_session_timeout")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0),
    )
}

/// How long an armed auto-lock waits, at most, before it reads the boot clock
/// again: a lock that fell due while the system slept happens within this
/// long of waking.
const AUTO_LOCK_RECHECK: Duration = Duration::from_secs(5);

/// `hardware` or `passcode` for a protected identity in `data_dir`.
fn protection_kind(data_dir: &std::path::Path, hash: &str) -> Option<&'static str> {
    let dir = data_dir.join("identities").join(hash);
    if dir.join("identity.hwid").exists() {
        Some("hardware")
    } else if dir.join("identity.enc").exists() {
        Some("passcode")
    } else {
        None
    }
}

/// Arm protected identity `hash`'s auto-lock, with the timeout set now, and
/// watch it on a task of its own. A reload from its retained unlock keeps
/// the deadline it had.
pub fn arm_auto_lock(
    state: &Arc<AppState>,
    hash: &str,
    kind: &'static str,
    from_retained_unlock: bool,
) {
    let generation =
        state.arm_auto_lock(hash, kind, auto_lock_timeout(state), from_retained_unlock);
    tokio::spawn(watch_auto_lock(Arc::downgrade(state), generation));
}

/// Carries out the auto-lock armed at `generation` once it falls due. It
/// waits for no network or tick: it sleeps at most `AUTO_LOCK_RECHECK` at a
/// time and re-reads the boot clock, so time the system slept counts. It
/// ends once another lock is armed or none is.
async fn watch_auto_lock(state: std::sync::Weak<AppState>, generation: u64) {
    loop {
        let wait = {
            let Some(state) = state.upgrade() else {
                return;
            };
            let Some(lock) = state.armed_auto_lock(generation) else {
                return;
            };
            if lock.timeout.is_zero() {
                return;
            }
            let now = rns_link::clock::ContinuousInstant::now();
            if lock.due_at(now) {
                // One that cannot go ahead yet stays armed for the next check.
                lock_protected_session(state, lock).await;
                AUTO_LOCK_RECHECK
            } else {
                lock.remaining_at(now).min(AUTO_LOCK_RECHECK)
            }
        };
        tokio::time::sleep(wait).await;
    }
}

/// Auto-lock fell due: refuse the identity's data at once, tear down its
/// session (which re-locks a token), drop the unlock, and wait for its PIN
/// or passcode again. An active identity whose network has stopped locks
/// too. One that cannot lock yet keeps its lock armed.
async fn lock_protected_session(state: Arc<AppState>, lock: state::AutoLock) {
    // Serialize with switch/unlock so two teardowns can't race on rns/lxmf.
    let _guard = state.identity_switch_lock.lock().await;
    // Re-armed or ended while we waited on the lock (switch, unlock, passcode)?
    if state.armed_auto_lock(lock.generation).is_none() {
        return;
    }
    let hash = lock.hash;
    let loaded = state
        .lxmf
        .lock()
        .ok()
        .and_then(|l| l.as_ref().map(|m| m.identity_hash.clone()));
    let current = match loaded {
        Some(loaded) => loaded == hash,
        None => helpers::active_identity_id(&state) == hash,
    };
    let Some(kind) = protection_kind(&state.config.data_dir, &hash).filter(|_| current) else {
        state.disarm_auto_lock();
        return;
    };
    tracing::info!(identity = %short_id(&hash), kind, "auto-lock timeout — locking");
    let reset = match reset_activity_for_shutdown(&state).await {
        Ok(reset) => reset,
        Err(error) => {
            tracing::error!(
                %error,
                identity = %short_id(&hash),
                "auto-lock deferred because Activity reset failed"
            );
            return;
        }
    };
    // Nothing begun before the lock may land behind it, and none of the
    // identity's data is answered from here on.
    state.bump_identity_session_generation();
    state.set_hw_locked(Some(hash.clone()));
    state.disarm_auto_lock();
    state.withdraw_native_notifications();
    finish_shutdown(&state, &reset).await;
    state.release_retained_identity_except("");
    state.clear_pending_hw_pin();
    state.set_startup_stage("hw_locked");
    state.emit_to_all(
        "hardware_locked",
        serde_json::json!({ "hash": hash, "kind": kind, "reason": "timeout" }),
    );
}

/// Validate a 12-word recovery phrase and derive the 64-byte Reticulum private
/// key (`X25519_prv || Ed25519_seed`) for a SOFTWARE identity. Same BIP-39 scheme
/// as recoverable hardware provisioning, so the restored identity matches the
/// YubiKey-backed one. Hardware-independent — works on every platform.
#[cfg(feature = "seed")]
pub fn derive_identity_key_from_phrase(phrase: &str) -> Result<Zeroizing<[u8; 64]>, String> {
    if !rns_ratkey::seed::validate_mnemonic(phrase) {
        return Err("Invalid recovery phrase — expected 12 valid BIP-39 words".into());
    }
    let derived = rns_ratkey::seed::derive_identity(phrase)
        .map_err(|e| format!("Could not derive identity: {e}"))?;
    let mut key = Zeroizing::new([0u8; 64]);
    key[..32].copy_from_slice(&derived.x25519_secret);
    key[32..].copy_from_slice(&derived.ed25519_seed);
    Ok(key)
}

/// Generate a fresh recoverable identity: a new BIP-39 mnemonic + the 64-byte
/// Reticulum private key derived from it. The caller writes/imports the key as a
/// software identity and stores the mnemonic with the same at-rest protection as
/// the identity key so it can be re-displayed after re-authentication.
#[cfg(feature = "seed")]
pub fn generate_recoverable_key() -> Result<(Zeroizing<String>, Zeroizing<[u8; 64]>), String> {
    let mnemonic = Zeroizing::new(
        rns_ratkey::seed::generate_mnemonic()
            .map_err(|e| format!("Could not generate recovery phrase: {e}"))?,
    );
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

/// The most recently used identity that can open.
pub fn most_recent_identity(
    pool: &db::DbPool,
    data_dir: &std::path::Path,
) -> Result<Option<String>, String> {
    let identities = data_dir.join("identities");
    Ok(db::identity_hashes_by_last_use(pool)?
        .into_iter()
        .find(|hash| {
            helpers::is_protocol_hash_16(hash) && profile_opens(&identities.join(hash), hash)
        }))
}

/// A protected identity opens to its lock sheet; a plain one only when its key
/// loads as that identity.
fn profile_opens(dir: &std::path::Path, hash: &str) -> bool {
    if dir.join("identity.hwid").exists() || dir.join("identity.enc").exists() {
        return true;
    }
    lxmf::bounded_plain_identity_from_file(&dir.join("identity"))
        .is_ok_and(|identity| hex::encode(identity.hash) == hash)
}

/// No active row: the most recently used identity becomes active, as after a
/// removal. A protected one then opens to its lock sheet.
async fn activate_most_recent_identity(state: &Arc<AppState>) -> Result<Option<String>, ()> {
    let data_dir = state.config.data_dir.clone();
    let activated = db::spawn_db(state.db.clone(), move |pool| {
        let Some(hash) = most_recent_identity(&pool, &data_dir)? else {
            return Ok(None);
        };
        db::set_active_identity(&pool, &hash)?;
        Ok::<_, String>(Some(hash))
    })
    .await;
    match activated {
        Ok(Ok(hash)) => {
            if hash.is_some() {
                state.bump_identity_session_generation();
            }
            Ok(hash)
        }
        Ok(Err(_)) | Err(_) => {
            tracing::error!(
                reason = "activate_failed",
                "could not activate the most recently used identity"
            );
            state.set_startup_stage("error");
            Err(())
        }
    }
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
    let Ok(active_identity) = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
        .await
        .expect("db task panicked")
    else {
        tracing::error!(reason = "query_failed", "channel hub not started");
        return false;
    };
    let active_identity = active_identity.and_then(|identity| {
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

/// A failed read is not "no active identity": startup would then pick
/// another identity. It ends startup in the error stage instead.
async fn read_active_identity_for_startup(state: &AppState) -> Result<Option<Value>, ()> {
    db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
        .await
        .expect("db task panicked")
        .map_err(|_| {
            tracing::error!(
                reason = "query_failed",
                "active identity unreadable at startup"
            );
            state.set_startup_stage("error");
        })
}

pub type StartupFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// Type-erased so the start-up tree compiles once: a concrete future is
/// compiled again in every crate that awaits it. Boxing also keeps the large
/// state machine off callers' stacks.
pub fn init_rns_lxmf(state: Arc<AppState>, data_dir: std::path::PathBuf) -> StartupFuture {
    Box::pin(init_rns_lxmf_inner(state, data_dir))
}

#[cfg(test)]
#[test]
fn startup_future_stays_type_erased() {
    let _: fn(Arc<AppState>, std::path::PathBuf) -> StartupFuture = init_rns_lxmf;
}

async fn init_rns_lxmf_inner(state: Arc<AppState>, data_dir: std::path::PathBuf) {
    // A staged secret serves this load only, and only the identity it was
    // staged for.
    let staged_secret = state.take_pending_hw_pin();
    state
        .network_session
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .error = None;
    start_activity_unless_stopped(&state).await;
    propagation::seed_static_nodes(&state);
    propagation::restore_heard_nodes(&state).await;

    let ratspeak_dir = data_dir.join(".ratspeak");
    let has_identity = has_identity_material(&ratspeak_dir);

    if !has_identity {
        tracing::info!("No identity found — starting in setup mode");
        state.set_startup_stage("ready");
        return;
    }

    state.set_startup_stage("lxmf");
    let Ok(preferred_identity) = read_active_identity_for_startup(&state).await else {
        return;
    };
    let mut preferred_identity_hash = preferred_identity.and_then(|identity| {
        identity
            .get("hash")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });
    if preferred_identity_hash.is_none() {
        let Ok(recent) = activate_most_recent_identity(&state).await else {
            return;
        };
        preferred_identity_hash = recent;
    }
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
    let hw_pin = staged_secret.and_then(|(hash, secret)| {
        (preferred_identity_hash.as_deref() == Some(hash.as_str())).then_some(secret)
    });
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
    let retained_identity = state
        .network_session
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .retained_identity
        .clone()
        .filter(|(identity, _)| {
            Some(hex::encode(identity.hash)).as_deref() == preferred_identity_hash.as_deref()
        });
    if active_is_protected && hw_pin.is_none() && retained_identity.is_none() {
        let hash = preferred_identity_hash.clone().unwrap_or_default();
        let kind = lock_kind.unwrap_or("hardware");
        tracing::info!(identity = %short_id(&hash), kind, "identity locked — awaiting unlock secret");
        state.disarm_auto_lock();
        state.set_hw_locked(Some(hash.clone()));
        state.set_startup_stage("hw_locked");
        state.emit_to_all(
            "hardware_locked",
            serde_json::json!({ "hash": hash, "kind": kind, "reason": "secret_required" }),
        );
        return;
    }

    let generation = state.current_identity_session_generation();
    let vault_job = if lock_kind == Some("passcode") && retained_identity.is_none() {
        match vault::try_acquire_job() {
            Ok(job) => Some(job),
            Err(error) => {
                state.disarm_auto_lock();
                state.set_hw_last_error(Some(error.to_string()));
                state.set_hw_locked(preferred_identity_hash.clone());
                state.set_startup_stage("hw_locked");
                return;
            }
        }
    } else {
        None
    };
    let from_retained_unlock = retained_identity.is_some();
    let load_dir = data_dir.clone();
    let load_hash = preferred_identity_hash.clone();
    let loaded = tokio::task::spawn_blocking(move || match retained_identity {
        Some((identity, hardware)) => {
            lxmf::LxmfManager::from_loaded_identity(&load_dir, identity, hardware)
        }
        None => lxmf::LxmfManager::load_or_create_with_vault_job(
            &load_dir,
            load_hash.as_deref(),
            hw_pin.as_deref().map(String::as_str),
            vault_job.as_ref(),
        ),
    })
    .await
    .unwrap_or_else(|_| Err("identity loading task panicked".into()));
    if state.current_identity_session_generation() != generation {
        return;
    }
    match loaded {
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
            // The stamps start as soon as the identity is loaded, before its network;
            // one already made, during setup say, is loaded instead.
            if announce_stamps::configure_for_identity(&state, &mgr.identity_hash) {
                mgr.load_stored_destination_stamps();
                let mut stamped = vec![mgr.delivery_destination()];
                if state
                    .propagation_node_hosting_enabled
                    .load(Ordering::Relaxed)
                {
                    stamped.push(mgr.propagation_destination());
                }
                announce_stamps::start_for_identity(
                    &state,
                    &mgr.identity_hash,
                    &mgr.identity,
                    &stamped,
                );
            }

            let Ok(active) = read_active_identity_for_startup(&state).await else {
                return;
            };
            if active.is_none() {
                // An activation: nothing begun before it may land in this identity.
                state.bump_identity_session_generation();
                let id_hash = mgr.identity_hash.clone();
                let lxmf_hash = mgr.lxmf_hash.clone();
                // Match the default used by setup + identity creation paths
                // so auto-recovered identities still announce a meaningful name.
                let default_display_name = ratspeak_core::default_display_name(&lxmf_hash);
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

            if let Ok(Some(identity)) =
                db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
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

            // The outbox's unfinished sends go out again; the previous run's
            // other in-flight sends fail.
            let mgr = resume_outbox(&state, mgr).await;
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
                    .ok()
                    .flatten()
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
                let contacts_list: Vec<serde_json::Value> = contact_list_payload(contacts);
                state.emit_to_all("contacts_update", serde_json::json!(contacts_list));
            }

            state.set_lxmf(mgr);
            rsnode::update::resume_pending(&state);

            // Pre-warm conversations cache so first paint doesn't await DB.
            if let Some(payload) = messaging::build_conversations_payload(&state).await {
                state.emit_to_all("conversations_update", payload);
            } else {
                tracing::warn!("conversations pre-warm failed; tab will fetch on demand");
            }
            tracing::info!("LXMF manager initialized");
            // Protected identities (hardware PIN or software passcode) lock once
            // the auto-lock timeout has passed since their unlock (off by default).
            match lock_kind.zip(preferred_identity_hash.as_deref()) {
                Some((kind, hash)) => arm_auto_lock(&state, hash, kind, from_retained_unlock),
                None => state.disarm_auto_lock(),
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
                state.disarm_auto_lock();
                state.set_hw_last_error(Some(msg.clone()));
                state.set_hw_locked(Some(hash.clone()));
                state.set_startup_stage("hw_locked");
                state.emit_to_all(
                    "hardware_locked",
                    serde_json::json!({ "hash": hash, "kind": kind, "error": msg }),
                );
                return;
            }
            // Without its identity the network would run as a ready app
            // that can't message.
            state.set_startup_stage("error");
            return;
        }
    }

    state.set_startup_stage("rns");
    let active_runtime_identity = state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(|mgr| mgr.identity_hash.clone()));
    let config_dir = rns_config_dir_for_start(&state, active_runtime_identity.as_deref());
    // Earlier releases' received ratchets are unused now; removing thousands
    // of them stays off the start's path, and an interrupted removal resumes
    // at the next start.
    if let Some(identity_hash) = active_runtime_identity.clone() {
        let data_dir = state.config.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(error) = lxmf::remove_earlier_received_ratchets(&data_dir, &identity_hash) {
                tracing::warn!(%error, "could not remove earlier received ratchets");
            }
        });
    }
    if state.config.uses_app_private_rns_config_dir() {
        match rns_config::ensure_app_private_shared_instance(&config_dir) {
            Ok(rns_config::RatspeakRnsPortConfigChange::Created) => {
                tracing::info!(
                    shared_instance_port = ratspeak_core::config::RATSPEAK_RNS_SHARED_INSTANCE_PORT,
                    instance_control_port =
                        ratspeak_core::config::RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
                    "created isolated Ratspeak app-private Reticulum config"
                );
            }
            Ok(rns_config::RatspeakRnsPortConfigChange::Updated) => {
                tracing::info!(
                    shared_instance_port = ratspeak_core::config::RATSPEAK_RNS_SHARED_INSTANCE_PORT,
                    instance_control_port =
                        ratspeak_core::config::RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
                    "updated Ratspeak app-private Reticulum shared-instance isolation"
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
        match rns_config::enable_unflagged_interfaces(&config_dir) {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "enabled interfaces that had no enable setting"),
            Err(_) => tracing::warn!(
                reason = "enable_interfaces",
                "failed to write enable settings into Ratspeak interfaces"
            ),
        }
        // The network identity first: the discovery sources always accept its network.
        if rns_config::apply_network_identity(&config_dir).is_err() {
            tracing::warn!(
                reason = "network_identity",
                "failed to write the network identity into the Reticulum config"
            );
        }
        if state.seed_network_settings(&config_dir)
            && rns_config::apply_network_settings(&config_dir, &state.network_settings()).is_err()
        {
            tracing::warn!(
                reason = "network_settings",
                "failed to write network settings into the Reticulum config"
            );
        }
    }
    let policy = match network_ownership::startup_policy(&state, &config_dir).await {
        Ok(policy) => policy,
        Err(error) => {
            network_ownership::fail_startup(&state, error);
            return;
        }
    };
    if !matches!(
        policy,
        rns_runtime::shared_instance::InstancePolicy::SharedClient(_)
    ) && state.config.uses_app_private_rns_config_dir()
    {
        reconcile_persisted_transport_mode_for_startup(&state, &config_dir);
        #[cfg(target_os = "android")]
        enforce_android_single_ble_rnode_for_startup(&state, &config_dir);
        #[cfg(target_os = "android")]
        migrate_android_usb_selectors_for_startup(&state, &config_dir).await;
    }
    let config_str = config_dir.to_string_lossy().to_string();

    // Android sandbox blocks /tmp — keep UDS under data_dir/cache.
    let socket_dir = active_runtime_identity
        .as_deref()
        .map(|identity_hash| state.config.identity_cache_dir(identity_hash))
        .unwrap_or_else(|| data_dir.join("cache"));
    std::fs::create_dir_all(&socket_dir).ok();
    let socket_dir = Some(socket_dir);

    // Reticulum imports the earlier releases' files as it starts; the migrating screen watches.
    let migration = network_migration::begin(&state, &config_dir);
    let started = rns::RnsManager::init_with_policy(
        &config_str,
        socket_dir,
        state.is_foreground.clone(),
        policy,
    )
    .await;
    if let Some(migration) = migration {
        migration.finish(
            started
                .as_ref()
                .map(|_| ())
                .map_err(|error| error.to_string()),
        );
    }
    match started {
        Ok(rns_mgr) => {
            let registration_info = if let Ok(mut lxmf) = state.lxmf.lock() {
                if let Some(mgr) = lxmf.as_mut() {
                    mgr.router
                        .set_transport(rns_mgr.handle.transport_tx.clone());
                    mgr.load_stored_destination_stamps();
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
                if let Ok(mut lxmf) = state.lxmf.lock()
                    && let Some(mgr) = lxmf.as_mut()
                {
                    mgr.delivery_tx = Some(delivery_tx);
                    mgr.set_path_recovery_handle(rns_mgr.handle.path_recovery_handle());
                    mgr.set_delivery_timing_owner(rns_mgr.handle.clone());
                    mgr.set_opportunistic_proof_sender(opportunistic_proof_tx);
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

            // Open durable storage before exposing the propagation service.
            'propagation_host: {
                let prop_info = state.lxmf.lock().ok().and_then(|l| {
                    let mgr = l.as_ref()?;
                    let signing_key = mgr.identity.get_signing_key()?;
                    let priv_key = mgr.identity.get_private_key()?;
                    let identity =
                        rns_identity::identity::Identity::from_private_key(&*priv_key).ok()?;
                    Some((mgr.propagation_dest_hash, identity, signing_key))
                });

                if let Some((prop_dest_hash, identity, signing_key)) = prop_info {
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
                        min_stamp_cost: f64::from(
                            state
                                .propagation_node_stamp_cost
                                .load(std::sync::atomic::Ordering::Relaxed),
                        ),
                        ..lxmf_core::propagation_node::PropagationNodeConfig::default()
                    };

                    let generation = state.current_identity_session_generation();
                    let identity_id = hex::encode(identity.hash);
                    state.set_propagation_host_readiness(
                        identity_id.clone(),
                        generation,
                        state::PropagationHostReadiness::Opening,
                        None,
                    );
                    let Some((storage_dir, dest_hash)) = prop_storage else {
                        state.set_propagation_host_readiness(
                            identity_id,
                            generation,
                            state::PropagationHostReadiness::Unavailable,
                            Some("Storage path unavailable".into()),
                        );
                        break 'propagation_host;
                    };
                    let result = tokio::task::spawn_blocking(move || {
                        let node = lxmf_core::propagation_node::PropagationNode::with_storage(
                            prop_node_config,
                            dest_hash,
                            storage_dir.clone(),
                        );
                        let summary =
                            lxmf_core::propagation_store_v2::quarantine_summary(&storage_dir).ok();
                        (node, summary)
                    })
                    .await;
                    if let Ok((_, summary)) = &result {
                        state.set_propagation_quarantine_summary(generation, summary.clone());
                    }
                    let prop_node = match result {
                        Ok((Ok(node), _)) => node,
                        error => {
                            let migration = matches!(&error, Ok((Err(error), _)) if lxmf_core::propagation_store_v2::is_migration_required(error));
                            let readiness = if migration {
                                state::PropagationHostReadiness::MigrationRequired
                            } else {
                                state::PropagationHostReadiness::Unavailable
                            };
                            state.set_propagation_host_readiness(
                                identity_id,
                                generation,
                                readiness,
                                Some(
                                    if migration {
                                        "Relay storage requires an offline migration"
                                    } else {
                                        "Relay storage could not be opened or verified"
                                    }
                                    .into(),
                                ),
                            );
                            tracing::warn!(
                                migration,
                                "propagation hosting unavailable; ordinary messaging remains active"
                            );
                            break 'propagation_host;
                        }
                    };
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
                            state.set_propagation_host_readiness(
                                identity_id,
                                generation,
                                state::PropagationHostReadiness::Unavailable,
                                Some("Transport unavailable".into()),
                            );
                            break 'propagation_host;
                        }
                    }

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
                    if let Some(destination) = link_mgr.destination_mut() {
                        destination.set_link_cost(lxmf_core::constants::PROPAGATION_LINK_COST);
                    }
                    install_propagation_announce_metadata(&mut link_mgr, &state, prop_dest_hash);

                    let offer_node = prop_node.clone();
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

                    link_mgr.set_request_handler(move |link_id, path_hash, data| {
                        if !prop_hosting_state.effective_propagation_hosting() {
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
                        } else {
                            None
                        }
                    });

                    link_mgr.set_link_endpoint_dispatch_handle(
                        rns_mgr.handle.link_endpoint_dispatch_handle(),
                    );
                    let get_limit = state
                        .lxmf
                        .lock()
                        .ok()
                        .and_then(|owner| {
                            let kb = owner.as_ref()?.router.config.propagation_limit_kb.as_f64();
                            // A float cast saturates; NaN and negative limits become 0.
                            Some(
                                ((kb * lxmf_core::constants::BYTES_PER_KILOBYTE as f64) as usize)
                                    .min(64 * 1024 * 1024),
                            )
                        })
                        .unwrap_or(0);
                    if !hosted_propagation::install_get_handler(
                        &mut link_mgr,
                        &state,
                        prop_node.clone(),
                        get_limit,
                    ) {
                        state.set_propagation_host_readiness(
                            identity_id,
                            generation,
                            state::PropagationHostReadiness::Unavailable,
                            Some("Propagation transfer budget is invalid".into()),
                        );
                        break 'propagation_host;
                    }

                    let (pkt_tx, pkt_rx) = tokio::sync::mpsc::channel(256);
                    link_mgr
                        .set_inbound_link_packet_channel(pkt_tx)
                        .expect("new hosted manager uses a typed packet channel");
                    link_mgr
                        .set_ordinary_packet_proof_strategy(
                            rns_identity::destination::ProofStrategy::ProveApp,
                        )
                        .expect("hosted manager has exact dispatch and a typed packet channel");
                    let (accounting_tx, accounting_rx) = tokio::sync::mpsc::unbounded_channel();
                    link_mgr.set_accounting_event_channel(accounting_tx);
                    let resource_owners =
                        hosted_propagation::install_resource_admission(&mut link_mgr, &state);
                    if !state.set_propagation_host_readiness(
                        identity_id,
                        generation,
                        state::PropagationHostReadiness::Ready,
                        None,
                    ) {
                        break 'propagation_host;
                    }
                    if let Ok(mut manager) = state.lxmf.lock()
                        && let Some(manager) = manager.as_mut()
                    {
                        apply_lxmf_settings_from_state(&state, manager);
                    }
                    let prop_shutdown = state
                        .session_shutdown
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let store_shutdown = prop_shutdown.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            biased;
                            _ = prop_shutdown.wait() => {}
                            _ = link_mgr.run() => {}
                        }
                    });
                    tokio::spawn(hosted_propagation::run(
                        state.clone(),
                        prop_node,
                        pkt_rx,
                        accounting_rx,
                        resource_owners,
                        store_shutdown,
                    ));
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
                    .ok()
                    .flatten()
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
                    if mgr.set_propagation_node(Some(&stored_pn), &state.db, &identity_id) {
                        tracing::info!(
                            node = %short_id(&stored_pn),
                            "restored Manual-mode propagation node from DB"
                        );
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
                    if let Some(destination) = lxmf_link_mgr.destination_mut() {
                        destination.set_link_cost(lxmf_core::constants::DELIVERY_LINK_COST);
                    }
                    lxmf_link_mgr.set_link_endpoint_dispatch_handle(
                        rns_mgr.handle.link_endpoint_dispatch_handle(),
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
                    let (link_res_tx, mut link_res_rx) = tokio::sync::mpsc::unbounded_channel::<
                        link_accounting::InboundResourceDelivery,
                    >();
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
                    lxmf_link_mgr
                        .set_ordinary_packet_proof_strategy(
                            rns_identity::destination::ProofStrategy::ProveAll,
                        )
                        .expect("Direct proofs do not require deferred admission");
                    lxmf_link_mgr
                        .set_link_packet_channel(link_pkt_tx.clone())
                        .expect("Direct channel uses automatic proofs");
                    // Use the single-owner accounting stream instead of also
                    // installing the legacy completion channel. Installing
                    // both would clone every completed Resource Vec.
                    lxmf_link_mgr.set_accounting_event_channel(link_accounting_tx);
                    lxmf_link_mgr.set_link_identified_channel(link_identified_tx);

                    let direct_admission_state = state.clone();
                    let direct_conclusion_state = state.clone();
                    let direct_completion_state = state.clone();
                    let direct_completion_tx = link_res_tx.clone();
                    if let Ok(mut lxmf) = state.lxmf.lock()
                        && let Some(mgr) = lxmf.as_mut()
                    {
                        mgr.set_direct_inbound_resource_handlers(
                            move |link_id, advertisement| {
                                admit_inbound_lxmf_resource(
                                    &direct_admission_state,
                                    link_id,
                                    advertisement,
                                )
                            },
                            move |link_id, resource_id| {
                                drop(
                                    direct_conclusion_state
                                        .take_inbound_attachment_resource(link_id, resource_id),
                                );
                            },
                        );
                        mgr.set_direct_inbound_resource_completion_handler(
                            move |link_id, resource_id, data| {
                                if let Some(lease) = direct_completion_state
                                    .take_inbound_attachment_resource(link_id, resource_id)
                                {
                                    let _ = direct_completion_tx.send(
                                        link_accounting::InboundResourceDelivery {
                                            data,
                                            link_id,
                                            lease,
                                        },
                                    );
                                }
                            },
                        );
                        mgr.set_lxmf_link_control(
                            link_command_tx,
                            link_pkt_tx.clone(),
                            link_identified_rx,
                            backchannel_event_rx,
                        );
                    }

                    let accounting_state = state.clone();
                    let accounting_shutdown = state
                        .session_shutdown
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone();
                    tokio::spawn(async move {
                        loop {
                            let event = tokio::select! {
                                biased;
                                _ = accounting_shutdown.wait() => break,
                                event = link_accounting_rx.recv() => match event {
                                    Some(event) => event,
                                    None => break,
                                },
                            };
                            link_accounting::forward(
                                event,
                                &link_res_tx,
                                &backchannel_event_tx,
                                |link_id, resource_id| {
                                    accounting_state
                                        .take_inbound_attachment_resource(link_id, resource_id)
                                },
                                |link_id| {
                                    accounting_state.release_inbound_attachment_link(link_id);
                                },
                            );
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
                            // Fair packet/Resource receive prevents a packet
                            // stream from starving completed attachments. The
                            // shutdown check below still fences received work.
                            let (data, link_id, resource_lease) = tokio::select! {
                                _ = link_inbound_shutdown.wait() => break,
                                item = link_pkt_rx.recv() => match item {
                                    Some((data, link_id)) => (data, link_id, None),
                                    None => break,
                                },
                                item = link_res_rx.recv() => match item {
                                    Some(delivery) => (delivery.data, delivery.link_id, Some(delivery.lease)),
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
                                link_inbound_shutdown.clone(),
                            )
                            .await;
                            // Keep completed attachment memory admitted through
                            // decoding, validation, and durable message handling.
                            drop(resource_lease);
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
            let detach_handle = rns_mgr.handle.clone();
            let detach_shutdown = rns_mgr.shutdown.clone();
            let startup_rnode_activity = rns_mgr.startup_rnode_runtimes().to_vec();
            #[cfg(all(feature = "ble", target_os = "android"))]
            let deferred_android_ble_rnodes = rns_mgr.deferred_android_ble_rnodes();
            let rnode_activity_origin = state.set_rns(rns_mgr);
            // Reticulum 1.6.1 detaches a hub that fails verification or is blackholed.
            interface_detach::install(&state, &detach_handle, detach_shutdown).await;
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
            if voice::start_voice_service_for_runtime_start(&state)
                .await
                .is_err()
            {
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
                let mut inbox_card = serde_json::Value::Null;
                let mut timeout_check_counter: u64 = 0;
                let mut next_auto_inbox_ready_check_at = 0.0;
                let mut download_coordinator = inbound_admission::DownloadCoordinator::default();
                #[cfg(feature = "mobile-throttle")]
                let mut was_foreground = true;
                loop {
                    let delivery_only = tokio::select! {
                        biased;
                        _ = tick_shutdown.wait() => break,
                        _ = interval.tick() => false,
                        _ = tick_state.lxmf_notify.notified() => false,
                        _ = std::future::poll_fn(|cx| {
                            tick_state.lxmf.lock().ok().and_then(|mut guard| {
                                guard.as_mut().map(|mgr| mgr.poll_delivery_ready(cx))
                            }).unwrap_or(std::task::Poll::Pending)
                        }) => true,
                    };
                    let dispatch_started = std::time::Instant::now();
                    let tick_activity_origin = tick_state.activity_request_fence();
                    if tick_shutdown.is_triggered() {
                        break;
                    }
                    // A panic under the manager lock, here or in a command.
                    if tick_state.lxmf.is_poisoned() {
                        recover_lxmf_after_panic(&tick_state, "poisoned manager lock");
                    }
                    if tick_state.lxmf_panic_recovery.in_flight() {
                        // The manager may be half-updated: leave it alone until the restart.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
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
                            was_foreground = is_fg;
                        }
                    }
                    if !delivery_only {
                        download_coordinator.synchronize(&tick_state).await;
                        refresh_outbound_ratchets(&tick_state).await;
                    }
                    let network_available =
                        crate::any_interface_online_cached(&tick_state).unwrap_or(false);
                    let auto_inbox_check_due = if delivery_only {
                        false
                    } else if let Ok(lxmf) = tick_state.lxmf.lock() {
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
                    if !delivery_only {
                        save_counter = save_counter.wrapping_add(1);
                    }
                    let should_save_crypto_state =
                        !delivery_only && save_counter.is_multiple_of(600);
                    let tick_state_for_lxmf = tick_state.clone();
                    let tick_result = tokio::task::spawn_blocking(move || {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                            let empty_result = || {
                                (
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    std::collections::HashMap::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    lxmf::WakeOutcome::default(),
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
                            let mut wakes = lxmf::WakeOutcome::default();
                            if !delivery_only {
                                admit_resumes_within_budget(&tick_state_for_lxmf, mgr);
                                wakes = run_wakes(&tick_state_for_lxmf, mgr);
                            }
                            let mut results = std::mem::take(&mut wakes.failed);
                            results.extend(if delivery_only {
                                mgr.process_delivery_events()
                            } else {
                                mgr.tick_with_auto_propagation_download_ready(
                                    auto_inbox_download_ready,
                                )
                            });
                            if delivery_only
                                && dispatch_started.elapsed() > Duration::from_millis(100)
                            {
                                tracing::debug!(
                                    target: "ratspeak_runtime::delivery_dispatch",
                                    wait_ms = dispatch_started.elapsed().as_millis() as u64,
                                    "Delivery wake processing delayed"
                                );
                            }
                            if mgr.take_path_recovery_refresh() {
                                tick_state_for_lxmf.poll_now.notify_one();
                            }
                            let tick_held = hold_started.elapsed();
                            if tick_held > Duration::from_secs(1) {
                                tracing::warn!(
                                    held_ms = tick_held.as_millis() as u64,
                                    "lxmf tick held manager lock (tick body)"
                                );
                            }
                            let delivery_progress = mgr.take_delivery_progress_updates();
                            let delivery_failures = mgr.take_delivery_failure_updates();
                            let failure_reasons = mgr.take_failure_reasons();
                            let downloaded = mgr.take_downloaded_propagation_messages();
                            let (
                                completed_deposits,
                                failed_deposits,
                                completed_syncs,
                                failed_syncs,
                            ) = mgr.take_propagation_health();
                            (
                                results,
                                delivery_progress,
                                delivery_failures,
                                failure_reasons,
                                downloaded,
                                completed_deposits,
                                failed_deposits,
                                completed_syncs,
                                failed_syncs,
                                mgr.take_packet_delivery_rtts(),
                                wakes,
                            )
                        }))
                    })
                    .await;
                    let (
                        results,
                        delivery_progress,
                        delivery_failures,
                        failure_reasons,
                        downloaded_propagation_messages,
                        completed_propagation_deposits,
                        failed_propagation_deposits,
                        completed_propagation_syncs,
                        failed_propagation_syncs,
                        packet_delivery_rtts,
                        wakes,
                    ) = match tick_result {
                        Ok(Ok(result)) => result,
                        Ok(Err(_)) | Err(_) => {
                            recover_lxmf_after_panic(&tick_state, "tick panic");
                            (
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                std::collections::HashMap::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                lxmf::WakeOutcome::default(),
                            )
                        }
                    };
                    apply_wake_outcome(&tick_state, tick_activity_origin, &wakes);
                    let packet_delivery_rtts = packet_delivery_rtts
                        .into_iter()
                        .collect::<std::collections::HashMap<_, _>>();
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
                    if !delivery_only {
                        let card = propagation::card_fingerprint(&tick_state, network_available);
                        if card != inbox_card {
                            inbox_card = card;
                            propagation::emit_propagation_update(&tick_state);
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
                    // (msg_id, state, method, destination, failure, reason)
                    type PersistedStep = (
                        String,
                        &'static str,
                        Option<String>,
                        Option<String>,
                        Option<lxmf::LxmfDeliveryFailureUpdate>,
                        Option<producer::DeliveryFailureReason>,
                        Option<String>,
                    );
                    let mut persisted: Vec<PersistedStep> = Vec::with_capacity(results.len());
                    let keep_trying = tick_state.lxmf_keep_trying_enabled();
                    let settled_at = tick_state.steady_now();
                    for (msg_id, new_state) in &results {
                        let rtt_ms = (*new_state == "delivered")
                            .then(|| packet_delivery_rtts.get(msg_id))
                            .flatten()
                            .map(|rtt| rtt.as_secs_f64() * 1000.0);
                        if OutboundState::parse(new_state).is_some_and(OutboundState::is_final) {
                            tick_state.release_attachment_delivery_lease(msg_id);
                        }
                        let failure = delivery_failures
                            .iter()
                            .find(|failure| failure.msg_id == *msg_id)
                            .cloned();
                        let msg_id_for_db = msg_id.clone();
                        let identity_for_db = identity_for_db.clone();
                        let state_for_db = Arc::clone(&tick_state);
                        let step = *new_state;
                        let failure_reason = (step == "failed").then(|| {
                            failure_reasons
                                .get(msg_id)
                                .copied()
                                .unwrap_or(producer::DeliveryFailureReason::TransportFailed)
                        });
                        let delivery_method_for_db = polled_delivery_method_override(
                            new_state,
                            packet_delivery_rtts.contains_key(msg_id),
                            delivery_progress
                                .iter()
                                .rev()
                                .find(|update| update.msg_id == *msg_id)
                                .map(|update| update.event_method),
                        )
                        .map(str::to_owned);
                        // A game move has no message row: its session holds
                        // its delivery state and guards it against regressions.
                        if tick_state
                            .lrgp_msg_to_session
                            .lock()
                            .is_ok_and(|map| map.contains_key(msg_id))
                        {
                            persisted.push((
                                msg_id.clone(),
                                step,
                                delivery_method_for_db,
                                None,
                                failure,
                                failure_reason,
                                None,
                            ));
                            continue;
                        }
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
                            // A failed send waits or ends; every other step is written as it is.
                            let written = match failure_reason {
                                Some(reason) => db::settle_failure(
                                    &p,
                                    &msg_id_for_db,
                                    &identity_for_db,
                                    reason.can_wait(),
                                    keep_trying,
                                    settled_at,
                                    OutboundState::Failed,
                                )
                                .map(|settled| {
                                    (
                                        settled.state.as_str(),
                                        // Names the chat for a Not delivered notice.
                                        Some(settled.destination),
                                        Some(failure_reason_after(reason, settled.verdict)),
                                    )
                                }),
                                None => db::update_message_state(
                                    &p,
                                    &msg_id_for_db,
                                    &identity_for_db,
                                    step,
                                    rtt_ms,
                                )
                                .then_some((step, None, None)),
                            };
                            let method = db::get_message_delivery_method(
                                &p,
                                &msg_id_for_db,
                                &identity_for_db,
                            );
                            let recipient = if matches!(written, Some(("delivered", ..))) {
                                recipient_of_proof(
                                    &state_for_db,
                                    &p,
                                    &identity_for_db,
                                    &msg_id_for_db,
                                )
                            } else {
                                None
                            };
                            (written, method, recipient)
                        })
                        .await
                        {
                            Ok((Some((state, destination, reason)), method, recipient)) => {
                                if state == OutboundState::Waiting.as_str() {
                                    tick_state.note_waiting_row();
                                }
                                persisted.push((
                                    msg_id.clone(),
                                    state,
                                    method,
                                    destination,
                                    failure,
                                    reason,
                                    recipient,
                                ))
                            }
                            Ok((None, _, _)) => tracing::debug!(
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
                    for (msg_id, new_state, method, destination, failure, reason, recipient) in
                        &persisted
                    {
                        let rtt_ms = (*new_state == "delivered")
                            .then(|| packet_delivery_rtts.get(msg_id))
                            .flatten()
                            .map(|rtt| rtt.as_secs_f64() * 1000.0);
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
                                "rtt_ms": rtt_ms,
                                "destination": destination,
                                "reason": reason.map(producer::DeliveryFailureReason::code),
                            })
                        };
                        tick_state.emit_to_all("lxmf_step", step_payload);
                        let activity_state =
                            OutboundState::parse(new_state).and_then(step_activity_state);
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
                                        rtt_ms: rtt_ms.map(|value| {
                                            value.round().clamp(0.0, u64::MAX as f64) as u64
                                        }),
                                        failure_reason: match activity_state {
                                            producer::LxmfDeliveryState::Rejected => {
                                                Some(producer::DeliveryFailureReason::Rejected)
                                            }
                                            producer::LxmfDeliveryState::Failed
                                            | producer::LxmfDeliveryState::Waiting => *reason,
                                            _ => None,
                                        },
                                        interface: recipient.as_deref().and_then(|recipient| {
                                            path_interface_class(&tick_state, recipient)
                                        }),
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
                            let outcome = match *new_state {
                                "delivered" | "propagated" => {
                                    Some(producer::LrgpActionStage::Succeeded)
                                }
                                "failed" | "rejected" | "undelivered" => {
                                    Some(producer::LrgpActionStage::Failed)
                                }
                                _ => None,
                            };
                            if let Some(stage) = outcome {
                                record_game_action(
                                    &tick_state,
                                    tick_activity_origin,
                                    msg_id,
                                    &meta,
                                    stage,
                                );
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
                        inbound_admission::admit_download(&tick_state, data, tick_activity_origin)
                            .await;
                    }
                    forward_destination_retains(&tick_state).await;

                    if delivery_only {
                        // Drain in bounded turns, giving shutdown and unrelated work a chance.
                        tokio::task::yield_now().await;
                        continue;
                    }

                    // Check orphan deadlines every ~5s in foreground (~20s in
                    // background), after the protocol managers have ticked.
                    // Healthy slow Resource progress owns its finite protocol
                    // clock; this absolute ceiling only cleans up unowned work.
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

            // Auto-announce loop; any announce restarts the wait, as Sideband does.
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
                        let wait = periodic_announce_wait(interval_secs, jitter_unit());
                        tokio::select! {
                            biased;
                            _ = periodic_shutdown.wait() => break,
                            _ = announce_rx.changed() => continue,
                            _ = periodic_state.presence_announced.notified() => continue,
                            _ = tokio::time::sleep(wait) => {
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
        Err(error) => {
            tracing::warn!(reason = "initialization_failed", "Failed to initialize RNS");
            network_ownership::fail_startup(&state, error.to_string());
            return;
        }
    }

    state.set_startup_stage("ready");
    state.emit_to_all("system_status", json!({"status": "ready"}));
    tracing::info!("Startup complete");
    schedule_startup_auto_announce(state.clone());
    // Unblocks whose runtime stopped before they lifted their network block
    // lift it through this one.
    if let Some(identity) = state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(|mgr| mgr.identity_hash.clone()))
    {
        let lift_state = state.clone();
        tokio::spawn(async move {
            blackhole::apply_pending_network_lifts(&lift_state, &identity).await;
        });
    }

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

const ANNOUNCE_SENT_EVENT: &str = "announce_sent";

/// The Reticulum config directory a network start uses: this identity's own,
/// seeded from the app-private one, when the profile keeps one per identity.
fn rns_config_dir_for_start(state: &AppState, identity_hash: Option<&str>) -> std::path::PathBuf {
    if state.config.uses_app_private_rns_config_dir()
        && let Some(identity_hash) = identity_hash
    {
        let dir = state.config.identity_rns_config_dir(identity_hash);
        seed_identity_rns_config_from_app_private(&state.config.rns_config_dir, &dir);
        return dir;
    }
    state.config.rns_config_dir.clone()
}

/// Start a new identity's announce stamp the moment it exists, before its
/// network does, so the first start usually finds it made.
pub fn start_announce_stamp_for_new_identity(
    state: &Arc<AppState>,
    identity_hash: &str,
    private_key: &[u8],
) {
    let Ok(identity) = rns_identity::identity::Identity::from_private_key(private_key) else {
        return;
    };
    let Ok(delivery) = rns_identity::destination::Destination::new(
        Some(&identity),
        rns_identity::destination::Direction::In,
        rns_identity::destination::DestType::Single,
        LXMF_DELIVERY_APP_NAME,
    ) else {
        return;
    };
    if announce_stamps::configure_for_identity(state, identity_hash) {
        announce_stamps::start_for_identity(state, identity_hash, &identity, &[&delivery]);
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
        if success && report.lxmf_delivery_queued {
            state.presence_announced.notify_waiters();
        }
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

/// Fresh installs announce hourly: transport nodes rebroadcast one announce
/// per destination an hour (Reticulum's `announce_rate_target`), and more
/// often only gets this node rate-limited at its hubs.
pub const DEFAULT_AUTO_ANNOUNCE_INTERVAL_SECS: u64 = 3600;

/// The interval plus up to half again, so nodes started together drift apart
/// and the wait never undercuts the hubs' hourly target. `unit` is in [0, 1).
fn periodic_announce_wait(interval_secs: u64, unit: f64) -> Duration {
    let interval = Duration::from_secs(interval_secs);
    interval + interval.mul_f64(unit.clamp(0.0, 1.0) / 2.0)
}

fn jitter_unit() -> f64 {
    let bytes = rns_crypto::random::random_bytes(4);
    f64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        / (f64::from(u32::MAX) + 1.0)
}

#[cfg(test)]
#[test]
fn periodic_announces_wait_the_interval_plus_up_to_half_again() {
    assert_eq!(periodic_announce_wait(3600, 0.0), Duration::from_secs(3600));
    assert_eq!(periodic_announce_wait(3600, 0.5), Duration::from_secs(4500));
    assert!(periodic_announce_wait(3600, 0.999_999) < Duration::from_secs(5400));
    for _ in 0..100 {
        assert!((0.0..1.0).contains(&jitter_unit()));
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

fn record_current_presence_lifecycle_activity(
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
    record_presence_lifecycle_activity(state, activity_origin, &completed_leadership, report);
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
    /// A destination stamp is still being generated off the runtime.
    StampPending,
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
        let propagation_packet = if state.effective_propagation_hosting() {
            match mgr.create_propagation_announce_packet() {
                Ok(raw) => Some((mgr.propagation_dest_hash, raw, false)),
                Err(lxmf::AnnounceBuildError::StampPending) => {
                    return PresencePacketBuildAttempt::StampPending;
                }
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
                Err(lxmf::AnnounceBuildError::Coalesced) => {
                    delivery_coalesced = true;
                }
                Err(lxmf::AnnounceBuildError::StampPending) => {
                    return PresencePacketBuildAttempt::StampPending;
                }
                Err(lxmf::AnnounceBuildError::TooLarge(size)) => {
                    delivery_failed = true;
                    tracing::warn!(correlation_id, size, "LXMF delivery announce too large");
                    state.warn_announce_too_large();
                }
                Err(lxmf::AnnounceBuildError::Failed(error)) => {
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

/// Admit one presence component in a routine lifecycle span, unless a
/// transition began or ended since `activity_origin`. Sending invalidates no
/// other request, and callers await interface dispatch after the span, so
/// inbound display, alerts and deletes never queue behind a slow interface.
async fn admit_in_lifecycle<F, T>(
    state: &AppState,
    activity_origin: ActivityRequestFence,
    admission: impl FnOnce() -> F,
) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    let _lifecycle = state.identity_switch_lock.lock_routine().await;
    if !state.is_current_activity_origin_fence(activity_origin) {
        return None;
    }
    Some(admission().await)
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
    let mut lock_deadline = std::time::Instant::now() + ANNOUNCE_LXMF_BUILD_RETRY_WINDOW;
    let mut stamp_wait_logged = false;
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
            PresencePacketBuildAttempt::StampPending => {
                // Python parks the announce until its stamp is ready; so does
                // this lifecycle, which runs off the caller's task.
                if !stamp_wait_logged {
                    stamp_wait_logged = true;
                    tracing::info!(
                        correlation_id = leadership.correlation_id,
                        "presence announce waiting for destination stamp generation"
                    );
                }
                tokio::time::sleep(ANNOUNCE_STAMP_POLL_INTERVAL).await;
                lock_deadline = std::time::Instant::now() + ANNOUNCE_LXMF_BUILD_RETRY_WINDOW;
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

    if matches!(any_interface_online_cached(state), Some(false)) {
        tracing::warn!(
            correlation_id = leadership.correlation_id,
            "announce skipped after retry because no interface remains online"
        );
        report.failed = 1;
        report.disposition = AnnounceSendDisposition::Failed;
        record_current_presence_lifecycle_activity(state, activity_origin, leadership, &report);
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
        record_current_presence_lifecycle_activity(state, activity_origin, leadership, &report);
        return AnnounceBurstExecution {
            report,
            activity_recorded: true,
        };
    };

    for (destination_hash, raw, is_lxmf_delivery) in packets {
        let packet_len = raw.len();
        let fingerprint = hex::encode(&rns_crypto::sha::sha256(&raw)[..6]);
        let (dispatch_tx, dispatch_rx) = tokio::sync::oneshot::channel();
        let packet = rns_transport::messages::TransportMessage::SendPacket {
            request: rns_transport::messages::OutboundRequest {
                raw: Bytes::from(raw),
                destination_hash,
            },
            attached_interface: None,
            receipt: None,
            result_tx: dispatch_tx,
        };
        let Some(admitted) = admit_in_lifecycle(state, activity_origin, || {
            tokio::time::timeout(ANNOUNCE_QUEUE_ADMISSION_WAIT, tx.send(packet))
        })
        .await
        else {
            tracing::info!(
                correlation_id = leadership.correlation_id,
                "stale presence announce suppressed before transport admission"
            );
            report.failed += 1;
            report.disposition = AnnounceSendDisposition::Failed;
            return AnnounceBurstExecution {
                report,
                activity_recorded: false,
            };
        };
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
                    let sent_ms = unix_now_ms();
                    state
                        .last_lxmf_delivery_announce_at_ms
                        .store(sent_ms, Ordering::Relaxed);
                    // The window shows an announce as sent only now, not when it was queued.
                    state.emitter.emit(
                        ANNOUNCE_SENT_EVENT,
                        json!({ "at": sent_ms as f64 / 1000.0 }),
                    );
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
        match admit_in_lifecycle(state, activity_origin, || {
            tokio::time::timeout(
                ANNOUNCE_QUEUE_ADMISSION_WAIT,
                voice::announce_if_running(state),
            )
        })
        .await
        {
            Some(Ok(Ok(true))) => {
                report.packets += 1;
                report.queued += 1;
                report.lxst_queued = true;
                tracing::info!("LXST telephony announce queued");
            }
            Some(Ok(Ok(false))) => {
                tracing::debug!("LXST telephony announce skipped: voice service is not running");
            }
            Some(Ok(Err(_)) | Err(_)) | None => {
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
    record_current_presence_lifecycle_activity(state, activity_origin, leadership, &report);
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
    files_dir: &std::path::Path,
    msg: &lxmf_core::message_api::LxMessage,
) -> Option<ExtractedAttachment> {
    if let Ok(Some((file_name, file_data))) = msg.first_file_attachment() {
        let stored = match lxmf::save_attachment_in(files_dir, &file_name, file_data) {
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

    if let Ok(Some((mime_type, image_data))) = msg.image_attachment() {
        let ext = mime_type.rsplit('/').next().unwrap_or("png");
        let file_name = format!("image.{ext}");
        let stored = match lxmf::save_attachment_in(files_dir, &file_name, image_data) {
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

    // A declared but malformed field is user media, not service traffic.
    // Keep a non-playable/unavailable representation instead of a blank bubble.
    use lxmf_core::constants::{FIELD_FILE_ATTACHMENTS, FIELD_IMAGE};
    if msg.fields().contains(FIELD_IMAGE) || msg.fields().contains(FIELD_FILE_ATTACHMENTS) {
        let is_image = msg.fields().contains(FIELD_IMAGE);
        return Some(ExtractedAttachment {
            file_name: if is_image { "Image" } else { "Attachment" }.into(),
            stored_name: db::ATTACHMENT_UNAVAILABLE_STORED_NAME.into(),
            is_image,
        });
    }
    None
}

fn extracted_attachment_json(attachment: &ExtractedAttachment) -> Value {
    if attachment.stored_name == db::ATTACHMENT_UNAVAILABLE_STORED_NAME {
        json!({"filename": attachment.file_name, "unavailable": true})
    } else {
        json!({"filename": attachment.file_name, "stored_name": attachment.stored_name})
    }
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
    files_dir: &std::path::Path,
    msg: &lxmf_core::message_api::LxMessage,
) -> Option<ExtractedAudio> {
    let audio = match msg.audio_field() {
        Ok(audio) => audio?,
        Err(error) => {
            tracing::warn!(%error, "unavailable malformed inbound LXMF audio field");
            return Some(ExtractedAudio {
                mode: lxmf_core::constants::AM_CUSTOM,
                stored_name: db::ATTACHMENT_UNAVAILABLE_STORED_NAME.into(),
                supported: false,
            });
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
    let is_compact = mode == lxmf_core::constants::AM_CODEC2_700C;
    #[cfg(feature = "lxst-voice")]
    if is_ogg_opus || is_compact {
        let valid = voice_memo::MemoFormat::from_mode(mode)
            .and_then(|format| voice_memo::inspect_voice_memo_format(audio.bytes, format));
        if let Err(error) = valid {
            tracing::warn!(%error, mode, size = audio.bytes.len(), "inbound voice audio is invalid");
            return Some(unavailable());
        }
    }

    let file_name = if is_ogg_opus {
        lxmf::AUDIO_MESSAGE_FILE_NAME.to_string()
    } else if is_compact {
        lxmf::COMPACT_AUDIO_FILE_NAME.to_string()
    } else {
        format!("Audio message {mode:02x}.bin")
    };
    let stored_name = lxmf::save_attachment_in(files_dir, &file_name, audio.bytes).ok();
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
                supported: (is_ogg_opus || is_compact) && cfg!(feature = "lxst-voice"),
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
    files_dir: &std::path::Path,
    attachment: Option<&ExtractedAttachment>,
    audio: Option<&ExtractedAudio>,
) {
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
    fence: &InboundFence,
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

    fence
        .publish(state, || {
            state.emit_to_all(
                "reaction_update",
                json!({
                    "message_id": target,
                    "reactions": reactions,
                }),
            );
        })
        .await;
}

/// Build a signed response without erasing the durable ordering deferral.
/// Queue ownership and retries live in the inbound task's bounded owner.
fn build_lxmf_path_response_message(
    state: &Arc<AppState>,
    attached_interface: Option<u64>,
    tag: Option<&[u8]>,
) -> Result<rns_transport::messages::TransportMessage, path_response::BuildError> {
    use lxmf::AnnounceBuildError;
    use path_response::BuildError;

    let mut guard = match state.lxmf.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::WouldBlock) => return Err(BuildError::Busy),
        Err(std::sync::TryLockError::Poisoned(_)) => return Err(BuildError::Failed),
    };
    let mgr = guard.as_mut().ok_or(BuildError::Failed)?;
    let raw = mgr
        .create_deferred_path_response_announce_packet(tag)
        .map_err(|error| match error {
            AnnounceBuildError::Coalesced => BuildError::Coalesced,
            AnnounceBuildError::StampPending => BuildError::Busy,
            AnnounceBuildError::TooLarge(_) => {
                state.warn_announce_too_large();
                BuildError::Failed
            }
            AnnounceBuildError::Failed(_) => BuildError::Failed,
        })?;
    let request = rns_transport::messages::OutboundRequest {
        raw: Bytes::from(raw),
        destination_hash: mgr.lxmf_dest_hash,
    };
    Ok(match attached_interface {
        Some(interface_id) => rns_transport::messages::TransportMessage::OutboundAttached {
            request,
            interface_id,
        },
        None => rns_transport::messages::TransportMessage::Outbound(request),
    })
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

/// A proof of delivery shows its recipient is in reach: what else waits for it is tried.
/// A delivered message's recipient, woken from waiting when Keep trying is on.
fn recipient_of_proof(
    state: &AppState,
    pool: &db::DbPool,
    identity_id: &str,
    msg_id: &str,
) -> Option<String> {
    let recipient = db::get_outbound_message_destination(pool, msg_id, identity_id)?;
    if state.lxmf_keep_trying_enabled()
        && let Some(hash) = ratspeak_core::hex_to_array16(&recipient)
    {
        state.wake_waiting(lxmf::WakeEvent::Heard(hash));
    }
    Some(recipient)
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
    // One hop: flip the state and read the method and recipient back for the emit.
    let state_for_db = Arc::clone(state);
    let (updated, method, recipient) = db::spawn_db(state.db.clone(), move |p| {
        let updated =
            db::update_message_state(&p, &msg_id_for_db, &identity_for_db, "delivered", rtt_ms);
        let method = db::get_message_delivery_method(&p, &msg_id_for_db, &identity_for_db);
        let recipient = updated
            .then(|| recipient_of_proof(&state_for_db, &p, &identity_for_db, &msg_id_for_db))
            .flatten();
        (updated, method, recipient)
    })
    .await
    .expect("db task panicked");
    // A game move has no message row; its session takes the proof.
    let lrgp_meta = state
        .lrgp_msg_to_session
        .lock()
        .ok()
        .and_then(|mut map| map.remove(msg_id));
    if !updated && lrgp_meta.is_none() {
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
                interface: recipient
                    .as_deref()
                    .and_then(|recipient| path_interface_class(state, recipient)),
            },
        ))
    });
    // Proofs do not necessarily reappear in the LXMF manager's polled state
    // changes. Complete the originating game action here as well, so its UI
    // cannot remain stuck on "Sending" after a valid proof.
    if let Some(meta) = lrgp_meta {
        record_game_action(
            state,
            activity_origin,
            msg_id,
            &meta,
            producer::LrgpActionStage::Succeeded,
        );
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

    let mut path_responses = path_response::PendingPathResponses::default();
    let mut response_tick = tokio::time::interval(path_response::RETRY_INTERVAL);
    response_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let event = tokio::select! {
            biased;
            _ = shutdown.wait() => break,
            _ = response_tick.tick(), if !path_responses.is_empty() => {
                path_responses.service(&state, &shutdown, Instant::now());
                continue;
            },
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
                path_responses.admit(
                    &state,
                    req.attached_interface,
                    req.tag.clone(),
                    Instant::now(),
                );
                path_responses.service(&state, &shutdown, Instant::now());
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
        let decrypted = state.lxmf.lock().ok().and_then(|l| {
            l.as_ref()
                .filter(|mgr| mgr.lxmf_dest_hash == dest_hash)
                .and_then(|mgr| mgr.decrypt_inbound(lxmf_payload))
        });
        tracing::info!(
            decrypted = decrypted.is_some(),
            decrypted_len = decrypted.as_ref().map(|d| d.len()),
            "LXMF decrypt result"
        );

        // Opportunistic LXMF omits dest_hash from the body (it's in the
        // RNS header). unpack() needs [dest_hash:16][src_hash:16][sig:64][msgpack];
        // re-prepend it only after authenticated decryption succeeds.
        let Some(body) = decrypted.as_deref() else {
            tracing::warn!(reason = "decrypt_failed", "encrypted LXMF input rejected");
            continue;
        };
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
            lxmf_data.len(),
            InboundLxmfSource::Opportunistic { raw },
            activity_origin,
            shutdown.clone(),
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
    /// Filed by the user from an `lxm://` paper URI.
    Paper,
}

impl InboundLxmfSource {
    fn label(&self) -> &'static str {
        match self {
            InboundLxmfSource::Opportunistic { .. } => "opportunistic",
            InboundLxmfSource::Link { .. } => "link",
            InboundLxmfSource::Propagated => "propagated",
            InboundLxmfSource::Paper => "paper",
        }
    }

    /// Stored and paper messages say nothing about the sender being reachable now.
    fn marks_sender_seen(&self) -> bool {
        !matches!(
            self,
            InboundLxmfSource::Propagated | InboundLxmfSource::Paper
        )
    }

    fn activity_method(&self) -> producer::InboundLxmfMethod {
        match self {
            InboundLxmfSource::Opportunistic { .. } => producer::InboundLxmfMethod::Opportunistic,
            InboundLxmfSource::Link { .. } => producer::InboundLxmfMethod::Direct,
            InboundLxmfSource::Propagated => producer::InboundLxmfMethod::Propagated,
            InboundLxmfSource::Paper => producer::InboundLxmfMethod::Paper,
        }
    }

    /// The stored `delivery_method` of an inbound row; only paper is recorded.
    fn stored_delivery_method(&self) -> Option<&'static str> {
        matches!(self, InboundLxmfSource::Paper).then_some("paper")
    }
}

/// What the inbound pipeline did with one message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum InboundOutcome {
    NotForUs,
    Blackholed,
    SignatureInvalid,
    StampRejected,
    Duplicate,
    Blocked,
    /// Handled without a chat row: reaction, LRGP, or service envelope.
    Consumed,
    SaveFailed,
    Stored {
        timestamp: f64,
    },
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
fn inbound_stamp_allowed(
    state: &AppState,
    identity_id: &str,
    msg: &lxmf_core::message_api::LxMessage,
) -> bool {
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
    let required_cost = f64::from(required_cost);
    let stamp_ok = match (
        msg.recipient_stamp().map(|stamp| stamp.as_bytes()),
        Some(msg.message_id()),
    ) {
        (Some(stamp), Some(message_id)) => with_receiver(state, identity_id, |mgr| {
            mgr.router.validate_stamp_with_tickets(
                &message_id,
                stamp,
                required_cost,
                &msg.source_hash(),
            )
        })
        // Once the receiver's manager is gone its tickets are too, but proof
        // of work still counts: a switch alone never loses a message.
        .unwrap_or_else(|| {
            <&[u8; 32]>::try_from(stamp).is_ok_and(|stamp| {
                lxmf_core::stamper::validate_stamp(
                    &message_id,
                    stamp,
                    required_cost,
                    lxmf_core::constants::STAMP_WORKBLOCK_EXPAND_ROUNDS,
                )
            })
        }),
        _ => false,
    };
    if !stamp_ok {
        tracing::warn!(
            from = %short_id(&hex::encode(msg.source_hash())),
            required_cost,
            has_stamp = msg.recipient_stamp().is_some(),
            "inbound message REJECTED: stamp missing or PoW invalid (enforce_stamps=true)"
        );
    }
    stamp_ok
}

/// Blackholed-source drop gate: inbound LXMs whose source resolves to a
/// blackholed identity are dropped before any processing
/// (LXMRouter.py at 1.0.1). Fail-open like Python's recall-gated
/// check (LXMessage.py): unknown sources, a missing transport, and
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
    session: rns_runtime::lifecycle::ShutdownSignal,
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
    let encoded_bytes = data.len();
    let limits = lxmf_core::message_api::MessageDecodeLimits::default();
    let msg = match lxmf_core::message_api::LxMessage::unpack_owned(data.into(), limits) {
        Ok(m) => m,
        Err(_) => {
            tracing::warn!(
                data_len = encoded_bytes,
                source = source.label(),
                reason = "unpack_failed",
                "inbound LXMF unpack failed"
            );
            return;
        }
    };
    process_inbound_lxmf(state, msg, encoded_bytes, source, activity_origin, session).await;
}

#[cfg(test)]
async fn handle_decrypted_lxmf(state: &Arc<AppState>, data: Vec<u8>, source: InboundLxmfSource) {
    let session = state
        .session_shutdown
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    handle_decrypted_lxmf_from_origin(state, data, source, state.activity_request_fence(), session)
        .await;
}

/// Presentation policy, not LXMF validity. Field presence preserves malformed
/// or unsupported declared media as unavailable; opaque metadata alone never
/// invents a user message. Recognized replies retain their quoted context.
fn inbound_has_chat_content(
    msg: &lxmf_core::message_api::LxMessage,
    extension: Option<&lxmf::RatspeakChatExtension>,
) -> bool {
    use lxmf_core::constants::{FIELD_AUDIO, FIELD_FILE_ATTACHMENTS, FIELD_IMAGE};
    !msg.content_text().trim().is_empty()
        || !msg.title_text().trim().is_empty()
        || [FIELD_FILE_ATTACHMENTS, FIELD_IMAGE, FIELD_AUDIO]
            .iter()
            .any(|field| msg.fields().contains(*field))
        || matches!(extension, Some(lxmf::RatspeakChatExtension::Reply { .. }))
}

/// The local identity a message is for: the live one when its processing
/// began. What the message changes stays with it even when a switch replaces
/// the live identity meanwhile, and its manager steps run only while it is
/// still live.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InboundReceiver {
    identity_id: String,
    lxmf_id: String,
}

/// What the receiver's manager knew of the sender when processing began.
struct InboundSender {
    identity: Option<[u8; 16]>,
    public_key: Option<[u8; 64]>,
    transport: Option<tokio::sync::mpsc::Sender<rns_transport::messages::TransportMessage>>,
}

impl InboundReceiver {
    /// The live identity, when it owns `msg`'s destination and still runs in
    /// `fence`'s sessions. A runtime session ends before its manager goes, so
    /// the manager seen under its lock is that session's.
    fn live(
        state: &AppState,
        msg: &lxmf_core::message_api::LxMessage,
        fence: &InboundFence,
    ) -> Option<(Self, InboundSender)> {
        let lxmf = state.lxmf.lock().ok()?;
        let mgr = lxmf.as_ref().filter(|mgr| {
            mgr.lxmf_dest_hash == msg.destination_hash() && fence.is_current(state)
        })?;
        let source = msg.source_hash();
        Some((
            Self {
                identity_id: mgr.identity_hash.clone(),
                lxmf_id: mgr.lxmf_hash.clone(),
            },
            InboundSender {
                identity: mgr.recall_identity_hash(&source),
                public_key: mgr.inbound_source_public_key(&source),
                transport: mgr.router.transport_tx.clone(),
            },
        ))
    }
}

/// Runs `f` on the live manager only while it is `identity_id`'s.
pub(crate) fn with_receiver<T>(
    state: &AppState,
    identity_id: &str,
    f: impl FnOnce(&mut lxmf::LxmfManager) -> T,
) -> Option<T> {
    let mut lxmf = state.lxmf.lock().ok()?;
    let mgr = lxmf
        .as_mut()
        .filter(|mgr| mgr.identity_hash == identity_id)?;
    Some(f(mgr))
}

/// The identity session a message began in and the runtime session it came
/// through. Nothing it shows reaches a window once either has ended.
#[derive(Clone)]
struct InboundFence {
    generation: u64,
    session: rns_runtime::lifecycle::ShutdownSignal,
}

impl InboundFence {
    fn new(
        activity_origin: ActivityRequestFence,
        session: rns_runtime::lifecycle::ShutdownSignal,
    ) -> Self {
        Self {
            generation: activity_origin.identity_session_generation(),
            session,
        }
    }

    fn is_current(&self, state: &AppState) -> bool {
        !self.session.is_triggered()
            && state.current_identity_session_generation() == self.generation
    }

    /// Runs `publish` under the routine lock while both sessions are current,
    /// so no transition lands between the check and what it emits.
    async fn publish(&self, state: &AppState, publish: impl FnOnce()) {
        let _lifecycle = state.identity_switch_lock.lock_routine().await;
        if self.is_current(state) {
            publish();
        }
    }
}

/// A stored message's sender label, the lists it changed, and whether its row
/// was still there. Read off the lifecycle lock, after the list revision was
/// taken: only a bulk delete moves it, and only then is the row read again,
/// under the lock (`inbound_message_stored`).
pub(crate) struct InboundLists {
    pub(crate) label: String,
    /// The message's conversation once the sender has several; else `None`.
    pub(crate) conversation: Option<String>,
    pub(crate) contacts: Vec<Value>,
    pub(crate) unread: i64,
    pub(crate) stored: bool,
    /// The sender's conversation is muted: no alert.
    pub(crate) muted: bool,
}

impl InboundLists {
    /// Alert sender: "Name · Conversation" when the sender has several.
    pub(crate) fn alert_label(&self) -> String {
        match &self.conversation {
            Some(conversation) => format!("{} · {conversation}", self.label),
            None => self.label.clone(),
        }
    }
}

pub(crate) async fn read_inbound_lists(
    state: &AppState,
    source_hash: &str,
    thread: &str,
    identity_id: &str,
    msg_id: &str,
) -> Option<InboundLists> {
    let source = source_hash.to_string();
    let thread = thread.to_string();
    let identity = identity_id.to_string();
    let msg_id = msg_id.to_string();
    db::spawn_db(state.db.clone(), move |p| InboundLists {
        label: contact_label_from_db(&p, &source, &identity),
        conversation: db::conversation_thread_display_label(&p, &identity, &source, &thread),
        contacts: contact_list_payload(db::get_all_contacts(&p, &identity)),
        unread: db::alerting_unread_total(&p, &identity),
        stored: db::message_exists_for_identity(&p, &msg_id, &identity),
        muted: db::is_conversation_muted(&p, &source, &identity),
    })
    .await
    .ok()
}

/// The one contact payload every list and event carries.
pub fn contact_payload(c: &Value) -> Value {
    json!({
        "hash": c.get("dest_hash"),
        "display_name": c.get("display_name"),
        "trust": c.get("trust"),
        "notes": c.get("notes"),
        "first_seen": c.get("first_seen"),
        "last_seen": c.get("last_seen"),
        "services": c.get("services"),
        "kind": c.get("kind"),
        "caps": c.get("caps"),
    })
}

pub fn contact_list_payload(contacts: Vec<Value>) -> Vec<Value> {
    contacts.iter().map(contact_payload).collect()
}

/// Whether `msg_id` is still stored: a bulk delete that committed after the
/// lists were read removed it, and it must not reach a window after that.
async fn inbound_message_stored(state: &AppState, msg_id: &str, identity_id: &str) -> bool {
    let msg_id = msg_id.to_string();
    let identity_id = identity_id.to_string();
    db::spawn_db(state.db.clone(), move |p| {
        db::message_exists_for_identity(&p, &msg_id, &identity_id)
    })
    .await
    .unwrap_or(false)
}

/// The one inbound LXMF pipeline. `encoded_bytes` is the unpacked wire's
/// length, for Activity.
async fn process_inbound_lxmf(
    state: &Arc<AppState>,
    msg: lxmf_core::message_api::LxMessage,
    encoded_bytes: usize,
    source: InboundLxmfSource,
    activity_origin: ActivityRequestFence,
    session: rns_runtime::lifecycle::ShutdownSignal,
) -> InboundOutcome {
    let source_hash = hex::encode(msg.source_hash());
    let dest_hash = hex::encode(msg.destination_hash());
    let inbound_method = source.activity_method();
    let fence = InboundFence::new(activity_origin, session);
    // Link and propagated deliveries name their destination only inside the
    // message; one for no live local identity is dropped.
    let Some((receiver, sender)) = InboundReceiver::live(state, &msg, &fence) else {
        tracing::warn!(
            reason = "not_for_us",
            source = source.label(),
            "inbound LXMF for no live local identity — dropping"
        );
        return InboundOutcome::NotForUs;
    };

    tracing::info!(
        from = %short_id(&source_hash),
        len = msg.content_bytes().len(),
        source = source.label(),
        "inbound LXMF message received"
    );
    if inbound_source_blackholed(sender.identity, sender.transport).await {
        tracing::debug!(from = %short_id(&source_hash), "Dropping LXM from blackholed identity");
        return InboundOutcome::Blackholed;
    }

    let verification = sender.public_key.as_ref().map(|key| msg.verify(key));
    let sig_valid = verification.as_ref().map(Result::is_ok);
    match sig_valid {
        Some(true) => tracing::debug!("inbound signature validated"),
        Some(false) => {
            tracing::warn!("inbound signature INVALID — dropping message");
            return InboundOutcome::SignatureInvalid;
        }
        None => tracing::debug!("sender unknown — signature not validated"),
    }

    // Paper messages carry no stamp requirement (LXMRouter.lxmf_propagation).
    if !matches!(source, InboundLxmfSource::Paper)
        && !inbound_stamp_allowed(state, &receiver.identity_id, &msg)
    {
        return InboundOutcome::StampRejected;
    }

    if let Some(Ok(verified)) = verification.as_ref()
        && with_receiver(state, &receiver.identity_id, |mgr| {
            mgr.router.learn_ticket_from_inbound(verified)
        }) == Some(true)
    {
        tracing::debug!(
            from = %short_id(&hex::encode(msg.source_hash())),
            "stored signed inbound ticket for future stamp bypass"
        );
    }

    // Opportunistic ACK; runs before the blocked check on purpose so a
    // blocked sender doesn't learn anything from a missing proof. Only the
    // live receiver proves; a sender without a proof sends again.
    if let InboundLxmfSource::Opportunistic { ref raw } = source {
        let proof_and_tx = with_receiver(state, &receiver.identity_id, |mgr| {
            let proof = mgr.create_delivery_proof(raw)?;
            let tx = mgr.router.transport_tx.clone()?;
            Some((proof, tx))
        })
        .flatten();
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

    let identity_id = receiver.identity_id.clone();
    let msg_id = hex::encode(msg.message_id());

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
        return InboundOutcome::Duplicate;
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
        return InboundOutcome::Blocked;
    }

    request_unknown_sender_path(state, msg.source_hash(), sender.public_key.is_some());

    if let InboundLxmfSource::Link {
        link_id: Some(link_id),
        ..
    } = source
    {
        with_receiver(state, &identity_id, |mgr| {
            mgr.note_pending_direct_backchannel(msg.source_hash(), link_id);
        });
        tracing::debug!(
            from = %short_id(&source_hash),
            link_id = %short_id(&hex::encode(link_id)),
            "Direct LXMF payload received; waiting for LINKIDENTIFY before backchannel reuse"
        );
    }

    if source.marks_sender_seen() {
        touch_peer_last_heard(state, &fence, &identity_id, &source_hash).await;
    }

    let chat_extension = lxmf::decode_ratspeak_chat_extension(&msg);
    if let Some(lxmf::RatspeakChatExtension::Reaction {
        target,
        emoji,
        action,
    }) = chat_extension.as_ref()
    {
        apply_inbound_ratspeak_reaction(
            state,
            &fence,
            &source_hash,
            &identity_id,
            target,
            emoji,
            action,
        )
        .await;
        return InboundOutcome::Consumed;
    }

    // LRGP tunnels over LXMF; don't surface in conversation UI.
    let lrgp_sender_authenticated =
        lrgp_sender_authenticated(&source, &msg.source_hash(), sig_valid);
    if !matches!(
        chat_extension,
        Some(lxmf::RatspeakChatExtension::Reply { .. })
    ) && let Some(mut outcome) = try_handle_inbound_lrgp(
        state,
        &msg,
        &source_hash,
        &receiver,
        lrgp_sender_authenticated,
    )
    .await
    {
        // Published once its game transaction is released, in identity-then-
        // game lock order.
        outcome.label_alert(state, &identity_id).await;
        fence.publish(state, || outcome.publish(state)).await;
        return InboundOutcome::Consumed;
    }

    // LXMF is also a service envelope (telemetry, commands, tickets, future
    // extensions). Protocol acceptance/proofs and signed ticket learning above
    // must not depend on whether this client has anything to show in a chat.
    // No service command is executed here. Replaying an ignored envelope has
    // no chat side effects, so it needs neither a chat row nor an unbounded
    // second message-id cache. Ticket storage is already keyed/idempotent.
    if !inbound_has_chat_content(&msg, chat_extension.as_ref()) {
        tracing::debug!(
            reason = "no_chat_content",
            "Consumed non-chat LXMF envelope"
        );
        return InboundOutcome::Consumed;
    }

    let received_at = next_chat_observed_timestamp(state, &source_hash, &identity_id).await;
    let (reply_to_id, reply_to_preview) = inbound_reply_fields(chat_extension.as_ref());
    let renderer = lxmf::message_renderer(&msg);
    let thread = lxmf::message_thread(&msg);
    let delivery_method = source.stored_delivery_method();
    // Only a verified sender's results are kept; they alone mark it an rsNode.
    let results = (sig_valid == Some(true))
        .then(|| msg.get_field(lxmf_core::constants::FIELD_RESULTS))
        .flatten()
        .and_then(|field| rsnode::decode_results(field.encoded()));
    // The receiver's media is written to its own files with the row that
    // names it, in one blocking task off the manager lock.
    let saved = {
        let config = state.config.clone();
        let thread = thread.clone();
        let msg_id = msg_id.clone();
        let source_hash = source_hash.clone();
        let dest_hash = dest_hash.clone();
        let identity_id = identity_id.clone();
        let reply_to_id = reply_to_id.clone();
        let reply_to_preview = reply_to_preview.clone();
        let results = results.clone();
        db::spawn_db(state.db.clone(), move |p| {
            let files_dir = config.identity_files_dir(&identity_id);
            let attachment = extract_and_save_attachment(&files_dir, &msg);
            let audio = extract_and_save_audio(&files_dir, &msg);
            let (att_name, att_stored, img_name, img_stored) = match attachment.as_ref() {
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
            let saved = db::try_save_message_with_audio(
                &p,
                &msg_id,
                &source_hash,
                &dest_hash,
                &msg.content_text(),
                &msg.title_text(),
                received_at,
                "received",
                "inbound",
                &identity_id,
                &att_name,
                &att_stored,
                &img_name,
                &img_stored,
                &reply_to_id,
                &reply_to_preview,
                delivery_method,
                audio.as_ref().map(|audio| audio.mode),
                audio
                    .as_ref()
                    .map(|audio| audio.stored_name.as_str())
                    .unwrap_or(""),
                renderer,
                &thread,
            );
            if saved.is_err() {
                remove_inbound_media_after_persistence_failure(
                    &files_dir,
                    attachment.as_ref(),
                    audio.as_ref(),
                );
            } else {
                // Inbound message un-hides the conversation.
                db::unhide_conversation(&p, &source_hash, &identity_id);
                if let Some(results) = results.as_ref() {
                    db::save_message_results(&p, &msg_id, &identity_id, results);
                    if rsnode::marks_rsnode(results) {
                        db::mark_contact_rsnode_device(&p, &source_hash, &identity_id);
                    }
                }
            }
            (msg, attachment, audio, saved)
        })
        .await
        .expect("db task panicked")
    };
    let (msg, attachment_file, audio_file, save_result) = saved;
    if let Err(error) = save_result {
        tracing::warn!(%error, "failed to persist inbound LXMF message");
        return InboundOutcome::SaveFailed;
    }
    // A message that came live shows its sender is in reach.
    if source.marks_sender_seen() {
        state.wake_waiting(lxmf::WakeEvent::Heard(msg.source_hash()));
    }
    // "Accepted" is intentionally recorded only after every authentication,
    // policy, duplicate, extension-routing, and persistence gate succeeds.
    // The typed producer receives no message content, display name, or raw
    // wire material.
    record_activity_if_current(state, activity_origin, || {
        Ok(producer::lxmf_inbound_accepted(
            producer::LxmfInboundAccepted {
                source: producer::DestinationHash::new(msg.source_hash()),
                method: inbound_method,
                encoded_bytes: encoded_bytes.min(u32::MAX as usize) as u32,
                // A message from the Offline Inbox or on paper didn't come over the sender's path.
                interface: matches!(
                    inbound_method,
                    producer::InboundLxmfMethod::Direct
                        | producer::InboundLxmfMethod::Opportunistic
                )
                .then(|| path_interface_class(state, &source_hash))
                .flatten(),
            },
        ))
    });

    let revision = state.conversation_list_revision();
    let lists = read_inbound_lists(state, &source_hash, &thread, &identity_id, &msg_id).await;

    let source_display_name = lists.as_ref().map_or_else(
        || compact_hash_label(&source_hash),
        |lists| lists.label.clone(),
    );
    let notification_label = lists
        .as_ref()
        .map_or_else(|| source_display_name.clone(), InboundLists::alert_label);

    // Frontend expects nested `image` / `attachments` matching history rows.
    let mut event_data = json!({
        "id": msg_id,
        "source": source_hash,
        "source_display_name": source_display_name,
        "destination": dest_hash,
        "content": msg.content_text(),
        "title": msg.title_text(),
        "timestamp": received_at,
        "state": "received",
        "direction": "inbound",
        "reply_to_id": reply_to_id,
        "reply_to_preview": reply_to_preview,
        "renderer": renderer,
        "delivery_method": delivery_method,
        "thread": thread,
        "results": results,
    });
    if let Some(ref att) = attachment_file {
        let obj = event_data.as_object_mut().unwrap();
        if att.is_image {
            obj.insert("image".to_string(), extracted_attachment_json(att));
        } else {
            obj.insert(
                "attachments".to_string(),
                json!([extracted_attachment_json(att)]),
            );
        }
    }
    if let Some(ref audio) = audio_file {
        event_data
            .as_object_mut()
            .unwrap()
            .insert("audio".to_string(), extracted_audio_json(audio));
    }

    let stored_outcome = InboundOutcome::Stored {
        timestamp: received_at,
    };
    let _lifecycle = state.identity_switch_lock.lock_routine().await;
    if !fence.is_current(state) {
        return stored_outcome;
    }
    let current = state.conversation_list_revision() == revision;
    let stored = match &lists {
        Some(lists) if current => lists.stored,
        _ => inbound_message_stored(state, &msg_id, &identity_id).await,
    };
    if !stored {
        return stored_outcome;
    }
    if !lists.as_ref().is_some_and(|lists| lists.muted) {
        notify_inbound_message_if_background(
            state,
            &source_hash,
            &notification_label,
            &messaging::message_display_text(&msg.title_text(), &msg.content_text()),
            renderer,
            attachment_file.is_some() || audio_file.is_some(),
        );
    }
    state.emit_to_all("lxmf_message", event_data);
    messaging::broadcast_conversations(Arc::clone(state));
    match lists {
        Some(lists) if current => {
            state.emit_to_all("contacts_update", lists.contacts.into());
            state.emit_to_all("unread_total", json!({"count": lists.unread}));
        }
        Some(_) => {}
        // Post-emit UI refresh failures only mean stale sidebar counts.
        None => tracing::error!(
            reason = "refresh_failed",
            "list refresh after inbound message failed"
        ),
    }
    stored_outcome
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

    let path_observation_started = Instant::now();
    let (iface_result, path_result, link_result) = tokio::join!(
        handle.query_control(rns_transport::messages::TransportQuery::GetInterfaceStats),
        crate::transport_observation::authoritative_path_table(&handle),
        handle.query_control(rns_transport::messages::TransportQuery::GetLinkCount),
    );

    if !is_current_network_runtime(state, &handle) {
        return;
    }

    let iface_stats = match iface_result {
        Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(s)) => {
            state.publish_local_network_active(local_network_running(&s));
            state.interface_stats_payload(&s)
        }
        _ => json!({ "interfaces": [] }),
    };

    let (path_table, path_index_delta, path_index_revision, path_table_total, path_table_truncated) =
        match path_result {
            Some(entries) => {
                let (fold, revision) =
                    fold_routes_from_path_table(state, &entries, path_observation_started);
                let (rows, total, truncated) = crate::rns::path_table_ui_snapshot(entries);
                (
                    rows,
                    fold.and_then(|fold| fold.delta),
                    revision,
                    total,
                    truncated,
                )
            }
            _ => (vec![], None, path_index_revision(state), 0, false),
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

    let connected = any_online && network_ownership::shared_connection_ready(&handle);

    let stats = json!({
        "timestamp": now,
        "connected": connected,
        "last_announce_at": last_announce_at_secs(state),
        "network_ownership": network_ownership::snapshot(state),
        "interface_stats": iface_stats,
        "path_table": path_table,
        "path_index_revision": path_index_revision,
        "path_table_total": path_table_total,
        "path_table_truncated": path_table_truncated,
        "rate_table": [],
        "link_count": link_count,
    });

    state.set_last_stats(stats.clone());
    if let Some(delta) = path_index_delta {
        state.emit_to_all("path_index_delta", delta);
    }
    state.emit_to_all("stats_update", stats);
}

/// Unix seconds of this identity's last LXMF delivery announce, or null before the first.
fn last_announce_at_secs(state: &AppState) -> Value {
    match state
        .last_lxmf_delivery_announce_at_ms
        .load(Ordering::Relaxed)
    {
        0 => Value::Null,
        ms => json!(ms as f64 / 1000.0),
    }
}

// Ownership can change without changing identity. Fence observations by the
// actual transport channel, not only the profile generation.
fn is_current_network_runtime(
    state: &AppState,
    handle: &rns_runtime::reticulum::ReticulumHandle,
) -> bool {
    state.rns.read().ok().is_some_and(|rns| {
        rns.as_ref().is_some_and(|manager| {
            manager
                .handle
                .transport_tx
                .same_channel(&handle.transport_tx)
        })
    }) && !handle.shutdown.is_triggered()
}

/// Whether Local Network runs: an AutoInterface, or a peer one spawned.
fn local_network_running(stats: &[rns_transport::messages::InterfaceStatRpcEntry]) -> bool {
    stats.iter().any(|entry| {
        entry.identity.as_ref().is_some_and(|identity| {
            matches!(
                identity.class_name.as_str(),
                "AutoInterface" | "AutoInterfacePeer"
            )
        })
    })
}

/// Fold the path table into LXMF's routes, and return the fold, if the
/// manager took it, with the path index's revision.
fn fold_routes_from_path_table(
    state: &AppState,
    entries: &[rns_transport::messages::PathTableRpcEntry],
    started: Instant,
) -> (Option<lxmf::RouteFold>, u64) {
    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        let fold = mgr.replace_routes_observed_at(entries, started);
        return (fold, mgr.path_index_revision());
    }
    (None, 0)
}

pub fn path_index_revision(state: &AppState) -> u64 {
    state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(lxmf::LxmfManager::path_index_revision))
        .unwrap_or(0)
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
    AnnouncePacing(announce_pacing::AnnouncePacingChange),
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
        PollActivityObservation::AnnouncePacing(change) => Ok(announce_pacing_activity(change)),
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

fn announce_pacing_activity(change: announce_pacing::AnnouncePacingChange) -> ProducerEvent {
    use announce_pacing::AnnouncePacingChange::{Ended, Started};
    let transition = match change {
        Started(interface) => producer::RnsAnnounceTransition::IngressBurstStarted {
            connection: producer::ConnectionName::new(interface.name).ok(),
        },
        Ended { interface, ago } => producer::RnsAnnounceTransition::IngressBurstCleared {
            connection: producer::ConnectionName::new(interface.name).ok(),
            stopped_before: ago,
        },
    };
    producer::rns_announce_activity(producer::RnsAnnounceActivity {
        transition,
        interface: None,
    })
}

/// Runs `publish` under the lifecycle lock while `generation` is still the
/// identity session: what a poll read under one never reaches the next, even
/// when the next comes up locked and sends no lists of its own.
async fn publish_in_session(state: &AppState, generation: u64, publish: impl FnOnce()) {
    let _lifecycle = state.identity_switch_lock.lock_routine().await;
    if state.current_identity_session_generation() == generation {
        publish();
    }
}

// Polls in the background too; a resume wake refreshes the UI at once.
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
    let mut pacing = announce_pacing::AnnouncePacing::default();
    let mut last_interface_announce = std::time::Instant::now();
    // Announces already applied to the LXMF caches, by received timestamp.
    let mut processed_announces: std::collections::HashMap<[u8; 16], f64> =
        std::collections::HashMap::new();
    // Announces already applied to the peer list and history: the transport's
    // whole cache, never the capped history, or every announce past the cap
    // reads as new on every poll.
    let mut applied_announces: std::collections::HashMap<[u8; 16], f64> =
        std::collections::HashMap::new();
    let mut processed_generation = None;
    let mut restored_announces = RestoredAnnounces::default();
    // A backgrounded poll skips the UI emit; resume must not wait out the cooldown.
    let mut stats_emit_skipped = false;

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
                if last_poll_at.elapsed() < POLL_NOW_COOLDOWN
                    && !(stats_emit_skipped && state.is_foreground())
                {
                    continue;
                }
                interval.reset();
            }
        }
        let poll_activity_origin = state.activity_request_fence();
        if shutdown.is_triggered() {
            break;
        }

        // Drop to 15s while backgrounded (desktop: window hidden or in the tray).
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
                .map(|mgr| (mgr.handle.clone(), mgr.started()))
        };

        let Some((handle, network_started)) = handle else {
            continue;
        };

        // Python-parity control surfaces proxy to the shared instance in
        // client mode; recent announces stay local dashboard state.
        let (stats, path_index_delta) = {
            let path_observation_started = Instant::now();
            let (iface_result, path_result, link_result, announce_result) = tokio::join!(
                handle.query_control(rns_transport::messages::TransportQuery::GetInterfaceStats),
                crate::transport_observation::authoritative_path_table(&handle),
                handle.query_control(rns_transport::messages::TransportQuery::GetLinkCount),
                handle.query_transport(rns_transport::messages::TransportQuery::GetRecentAnnounces),
            );

            if !is_current_network_runtime(&state, &handle) {
                continue;
            }

            let iface_stats = match iface_result {
                Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(s)) => {
                    state.publish_local_network_active(local_network_running(&s));
                    for iface in &s {
                        let name = iface.name.as_str();
                        let online = state.effective_interface_online(iface.id, iface.online);
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
                            if online {
                                state.wake_waiting(lxmf::WakeEvent::InterfaceOnline);
                            }
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
                    }
                    for change in pacing.observe(&s, rns_link::clock::ContinuousInstant::now()) {
                        tracing::debug!(interface = change.key(), "announce pacing changed");
                        activity_observations.push(PollActivityObservation::AnnouncePacing(change));
                    }

                    state.interface_stats_payload(&s)
                }
                _ => json!({ "interfaces": [] }),
            };

            let (
                path_table,
                path_index_delta,
                path_index_revision,
                path_table_total,
                path_table_truncated,
            ) = match path_result {
                Some(entries) => {
                    let (fold, revision) =
                        fold_routes_from_path_table(&state, &entries, path_observation_started);
                    let mut delta = None;
                    if let Some(fold) = fold {
                        let path_activity_ready = state
                            .path_activity_baselined
                            .load(std::sync::atomic::Ordering::Relaxed);
                        if path_activity_ready {
                            for (destination, hops) in fold.added {
                                state.wake_waiting(lxmf::WakeEvent::Heard(destination));
                                activity_observations.push(PollActivityObservation::PathObserved {
                                    destination,
                                    hops,
                                });
                            }
                        } else if !entries.is_empty() {
                            state
                                .path_activity_baselined
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        delta = fold.delta;
                    }
                    let (rows, total, truncated) = crate::rns::path_table_ui_snapshot(entries);
                    (rows, delta, revision, total, truncated)
                }
                _ => (vec![], None, path_index_revision(&state), 0, false),
            };

            let link_count = match link_result {
                Some(rns_transport::messages::TransportQueryResponse::IntResult(n)) => n,
                _ => 0,
            };

            if let Some(rns_transport::messages::TransportQueryResponse::Announces(mut announces)) =
                announce_result
            {
                let local_identity = state
                    .lxmf
                    .lock()
                    .ok()
                    .and_then(|lxmf| lxmf.as_ref().map(|mgr| mgr.identity.hash));
                // Transport remembers this node's own announces; they are not peers.
                announces.retain(|a| !is_own_announce(a, local_identity));
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
                // Aspect-agnostic: crypto cache, announce_history, contact-name refresh.
                if processed_generation != Some(poll_generation) {
                    processed_announces.clear();
                    applied_announces.clear();
                    processed_generation = Some(poll_generation);
                    restored_announces = RestoredAnnounces::from_listing(
                        &announces,
                        network_started.elapsed(),
                        local_now_ts(),
                    );
                }
                if let Ok(mut lxmf) = state.lxmf.lock()
                    && let Some(mgr) = lxmf.as_mut()
                {
                    for a in &announces {
                        let dest_hex = hex::encode(a.dest_hash);
                        let unchanged =
                            !announce_is_newer(&processed_announces, &a.dest_hash, a.timestamp);
                        if unchanged
                            && (a.public_key.is_none()
                                || mgr.known_identities.contains_key(&dest_hex))
                        {
                            continue;
                        }
                        tracing::debug!(
                            dest = %short_id(&dest_hex),
                            has_pk = a.public_key.is_some(),
                            has_ratchet = a.ratchet.is_some(),
                            hops = ?crate::transport_observation::announce_hops(a),
                            "processing announce entry"
                        );
                        if let Some(ref pk) = a.public_key {
                            let is_new = !mgr.known_identities.contains_key(&dest_hex);
                            identities_changed |= mgr.update_remote_identity(&dest_hex, pk);
                            if is_new {
                                tracing::debug!(
                                    dest = %short_id(&dest_hex),
                                    has_ratchet = a.ratchet.is_some(),
                                    "new remote identity cached from announce"
                                );
                            }
                        }
                        // `Identity.recall_app_data` serves a restored
                        // destination too; its aspect is read from its hash.
                        if let Some(name_hash) = crate::transport_observation::announce_name_hash(
                            a,
                            &[LXMF_DELIVERY_APP_NAME, LXMF_PROPAGATION_APP_NAME],
                        ) {
                            let app_data = a.app_data.as_deref();
                            router_changed |= if restored_announces.heard_now(a) {
                                mgr.update_lxmf_announce_app_data(a.dest_hash, name_hash, app_data)
                            } else {
                                mgr.recall_lxmf_announce_app_data(
                                    a.dest_hash,
                                    name_hash,
                                    app_data,
                                    a.timestamp,
                                )
                            };
                        }
                    }
                    processed_announces = announces
                        .iter()
                        .map(|a| (a.dest_hash, a.timestamp))
                        .collect();
                    mgr.prune_compression_support(&announces);
                }
                if identities_changed || router_changed {
                    crate::lxmf_persistence::schedule_announce_persistence(&state, router_changed);
                }

                if let Ok(mut history) = state.announce_history.write()
                    && let Ok(mut seen) = state.seen_announce_hashes.lock()
                {
                    // Forget destinations the announce cache dropped, amortized.
                    if seen.len() > announces.len().saturating_mul(2).max(1024) {
                        let current: std::collections::HashSet<[u8; 16]> =
                            announces.iter().map(|a| a.dest_hash).collect();
                        seen.retain(|hash| current.contains(hash));
                    }
                    for a in &announces {
                        let announce_timestamp_changed =
                            announce_is_newer(&applied_announces, &a.dest_hash, a.timestamp);
                        // Nothing to show or record for an announce already applied.
                        if !announce_timestamp_changed && seen.contains(&a.dest_hash) {
                            continue;
                        }
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
                        // Null, not zero, for a destination restored without them.
                        let announce_hops = crate::transport_observation::announce_hops(a);
                        let is_new = seen.insert(a.dest_hash);
                        // A restored row is not an announce heard now: peers stay
                        // as the database has them.
                        if restored_announces.touches_peer(&applied_announces, a) {
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
                                    announce_emitted: None,
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
                                    announce_emitted: None,
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
                            existing["hops"] = json!(announce_hops);
                        } else if announce_timestamp_changed {
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
                                    "hops": announce_hops,
                                }),
                            );
                        }
                        if announce_activity_ready
                            && is_new
                            && let Some(hops) = announce_hops
                        {
                            state.emit_to_all(
                                "announce_received",
                                json!({
                                    "hash": hash_hex,
                                    "display_name": display_name,
                                    "status": status.unwrap_or_default(),
                                    "timestamp": a.timestamp,
                                    "hops": hops,
                                }),
                            );
                            activity_observations.push(PollActivityObservation::AnnounceObserved {
                                destination: a.dest_hash,
                                hops,
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
                    applied_announces = announces
                        .iter()
                        .map(|a| (a.dest_hash, a.timestamp))
                        .collect();
                }
                if !delivery_trigger_hashes.is_empty() {
                    delivery_trigger_hashes.sort();
                    delivery_trigger_hashes.dedup();
                    for recipient in &delivery_trigger_hashes {
                        state.wake_waiting(lxmf::WakeEvent::Heard(*recipient));
                    }
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
                    publish_in_session(&state, poll_generation, || emit_peers_batch(&state, &rows))
                        .await;
                }

                // Peers who messaged us before they announced have no real
                // name in contacts yet; name them from their announces.
                let announce_identity_id = crate::helpers::active_identity_id(&state);
                let unnamed = if announce_identity_id.is_empty() {
                    Vec::new()
                } else {
                    let identity_id = announce_identity_id.clone();
                    db::spawn_db(state.db.clone(), move |p| {
                        db::unnamed_contact_hashes(&p, &identity_id)
                    })
                    .await
                    .unwrap_or_default()
                };
                let announced_names: Vec<(String, String)> = if unnamed.is_empty() {
                    Vec::new()
                } else {
                    let unnamed: std::collections::HashSet<String> = unnamed.into_iter().collect();
                    announces
                        .iter()
                        .filter_map(|a| {
                            let hash = hex::encode(a.dest_hash);
                            if !unnamed.contains(&hash) {
                                return None;
                            }
                            let name = extract_display_name(a.app_data.as_ref()?);
                            (!name.is_empty()).then_some((hash, name))
                        })
                        .collect()
                };
                if !announced_names.is_empty() {
                    let announce_id_for_db = announce_identity_id.clone();
                    let contacts_changed = db::spawn_db(state.db.clone(), move |p| {
                        db::fill_unnamed_contact_names(&p, &announce_id_for_db, &announced_names)
                    })
                    .await
                    .expect("db task panicked");
                    if contacts_changed {
                        let announce_id_for_contacts = announce_identity_id.clone();
                        let contacts = db::spawn_db(state.db.clone(), move |p| {
                            db::get_all_contacts(&p, &announce_id_for_contacts)
                        })
                        .await
                        .expect("db task panicked");
                        let contacts_list: Vec<serde_json::Value> = contact_list_payload(contacts);
                        publish_in_session(&state, poll_generation, || {
                            state.emit_to_all("contacts_update", serde_json::json!(contacts_list))
                        })
                        .await;
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

            let connected = any_online && network_ownership::shared_connection_ready(&handle);

            let stats = json!({
                "timestamp": now,
                "connected": connected,
                "last_announce_at": last_announce_at_secs(&state),
                "network_ownership": network_ownership::snapshot(&state),
                "interface_stats": iface_stats,
                "path_table": path_table,
                "path_index_revision": path_index_revision,
                "path_table_total": path_table_total,
                "path_table_truncated": path_table_truncated,
                "rate_table": [],
                "link_count": link_count,
            });
            (stats, path_index_delta)
        };

        if state
            .identity_session_generation
            .load(std::sync::atomic::Ordering::SeqCst)
            != poll_generation
            || !is_current_network_runtime(&state, &handle)
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
        // A backgrounded WebView has no reader; resume polls and emits at once.
        // A delta it misses is caught by the revision in the next stats.
        let emit = state.is_foreground();
        if emit {
            if let Some(delta) = path_index_delta {
                state.emit_to_all("path_index_delta", delta);
            }
            state.emit_to_all("stats_update", stats);
        }
        stats_emit_skipped = !emit;

        last_poll_at = std::time::Instant::now();
    }

    // No runtime, no Local Network.
    state.publish_local_network_active(false);

    // Spells still open end with polling, so each has its end row.
    for change in pacing.close_all(rns_link::clock::ContinuousInstant::now()) {
        record_poll_activity(
            &state,
            runtime_activity_origin,
            PollActivityObservation::AnnouncePacing(change),
        );
    }
}

/// What stopping a send found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoppedSend {
    /// The manager held the message and let go of it.
    pub live_owner_stopped: bool,
    /// Its row now says cancelled.
    pub row_marked_stopped: bool,
    /// It only waited: nothing of it was in the air on this attempt.
    pub only_waiting: bool,
    pub delivery_method: Option<String>,
}

/// Stop `msg_id`: the manager lets go of it and its row is marked cancelled.
/// Both happen under the manager lock, where a wake also claims, so a
/// waiting message is stopped or sent again and never both.
pub fn stop_outbound_message(state: &AppState, msg_id: &str, identity_id: &str) -> StoppedSend {
    let mut lxmf = state.lxmf.lock().ok();
    let live_owner_stopped = lxmf
        .as_mut()
        .and_then(|lxmf| lxmf.as_mut())
        .is_some_and(|manager| manager.cancel_outbound_message(msg_id));
    let waited = db::outbound_message_state(&state.db, identity_id, msg_id)
        .is_ok_and(|stored| stored.as_deref() == Some(OutboundState::Waiting.as_str()));
    let row_marked_stopped = db::cancel_outbound_message_state(&state.db, msg_id, identity_id);
    let delivery_method = db::get_message_delivery_method(&state.db, msg_id, identity_id);
    drop(lxmf);
    StoppedSend {
        live_owner_stopped,
        row_marked_stopped,
        only_waiting: waited && row_marked_stopped && !live_owner_stopped,
        delivery_method,
    }
}

/// Turn waiting on or off for every consumer at once. Off ends every
/// identity's waiting messages as Not delivered; the active identity's are
/// told to the dashboard with a reason that shows no notice. Returns how many ended.
pub fn set_lxmf_keep_trying(state: &AppState, enabled: bool) -> Result<usize, String> {
    // Under the manager lock, where a wake claims: none is claimed while they end.
    let mut lxmf = state.lxmf.lock().ok();
    state.set_lxmf_keep_trying_enabled(enabled);
    if let Some(manager) = lxmf.as_mut().and_then(|lxmf| lxmf.as_mut()) {
        manager.keep_trying = enabled;
    }
    let ended = if enabled || !ratspeak_core::delivery::policy::OFF_ENDS_WAITING {
        Vec::new()
    } else {
        drop(state.take_wake_signals());
        db::fail_all_waiting(&state.db)?
    };
    drop(lxmf);
    if ended.is_empty() {
        return Ok(0);
    }
    let active = helpers::active_identity_id(state);
    for (_, msg_id) in ended.iter().filter(|(identity, _)| *identity == active) {
        state.emit_to_all(
            "lxmf_step",
            json!({
                "step": "failed",
                "msg_id": msg_id,
                "reason": "keep_trying_off",
            }),
        );
    }
    Ok(ended.len())
}

/// Run the wake engine; the caller holds the manager lock and applies the
/// outcome once it is released.
fn run_wakes(state: &Arc<AppState>, mgr: &mut lxmf::LxmfManager) -> lxmf::WakeOutcome {
    mgr.run_wakes(lxmf::WakeContext {
        db: &state.db,
        now: state.steady_now(),
        keep_trying: state.lxmf_keep_trying_enabled(),
        online: any_interface_online_cached(state) == Some(true),
        waiting_written: state.take_waiting_written(),
        signals: state.take_wake_signals(),
        reserve: &mut |msg_id, bytes| match state.reserve_attachment_transfer(bytes) {
            Ok(lease) => {
                state.hold_attachment_delivery_lease(msg_id.to_string(), lease);
                true
            }
            Err(_) => false,
        },
        release: &mut |msg_id| {
            state.release_attachment_delivery_lease(msg_id);
        },
    })
}

/// Tell the dashboard and Activity what the wake engine did: each new attempt
/// with what prompted it, and each message whose time to wait ran out.
fn apply_wake_outcome(
    state: &AppState,
    activity_origin: ActivityRequestFence,
    wakes: &lxmf::WakeOutcome,
) {
    for woken in &wakes.woken {
        // Nothing may hold a woken send: the watchdog ends one that goes nowhere.
        if let Ok(mut times) = state.message_send_times.lock() {
            times.insert(woken.msg_id.clone(), local_now_ts());
        }
        state.emit_to_all(
            "lxmf_step",
            json!({
                "step": "sending",
                "msg_id": woken.msg_id,
                "method": woken.method,
                "wake": woken.cause.code(),
            }),
        );
        record_activity_if_current(state, activity_origin, || {
            Ok(producer::lxmf_delivery_retrying(
                producer::LxmfDeliveryRetrying {
                    message: producer::MessageId::from_hex(&woken.msg_id)?,
                    method: producer::LxmfDeliveryMethod::from_code(woken.method),
                    cause: woken.cause,
                },
            ))
        });
    }
    for expired in &wakes.expired {
        let reason = producer::DeliveryFailureReason::WaitExpired;
        state.emit_to_all(
            "lxmf_step",
            json!({
                "step": "failed",
                "msg_id": expired.msg_id,
                "method": expired.delivery_method,
                "destination": expired.destination,
                "reason": reason.code(),
            }),
        );
        record_activity_if_current(state, activity_origin, || {
            Ok(producer::lxmf_delivery_state_changed(
                producer::LxmfDeliveryStateChanged {
                    message: producer::MessageId::from_hex(&expired.msg_id)?,
                    state: producer::LxmfDeliveryState::Failed,
                    method: expired
                        .delivery_method
                        .as_deref()
                        .and_then(producer::LxmfDeliveryMethod::from_code),
                    rtt_ms: None,
                    failure_reason: Some(reason),
                    interface: None,
                },
            ))
        });
    }
}

/// Resumed attachments load under the same transfer budget as fresh sends;
/// each holds its lease until it settles, as a fresh send does.
fn admit_resumes_within_budget(state: &Arc<AppState>, mgr: &mut lxmf::LxmfManager) {
    mgr.admit_resumes_within_budget(
        |msg_id, size| match state.reserve_attachment_transfer(size) {
            Ok(lease) => {
                state.hold_attachment_delivery_lease(msg_id.to_string(), lease);
                true
            }
            Err(_) => false,
        },
    );
}

/// Queue the outbox's unfinished sends again; the tick sends them, and fails
/// the message of a record that cannot be read.
async fn resume_outbox(state: &Arc<AppState>, mgr: lxmf::LxmfManager) -> lxmf::LxmfManager {
    let identity_id = mgr.identity_hash.clone();
    let own_address = mgr.lxmf_hash.clone();
    let rows = match db::spawn_db(state.db.clone(), move |p| {
        // Nothing carries a message to one's own address: one still unsettled is delivered.
        let to_self = db::deliver_messages_to_self(&p, &identity_id, &own_address);
        // A wake's unfinished send waits for a wake again; resumed, it would go out in a burst.
        let returned = db::return_woken_to_waiting(&p, &identity_id);
        let waiting = db::has_waiting(&p, &identity_id);
        (
            to_self,
            returned,
            waiting,
            db::outbox_rows(&p, &identity_id),
        )
    })
    .await
    {
        Ok((to_self, returned, waiting, Ok(rows))) => {
            for msg_id in to_self {
                state.emit_to_all(
                    "lxmf_step",
                    json!({ "step": "delivered", "msg_id": msg_id }),
                );
            }
            for msg_id in returned {
                state.emit_to_all("lxmf_step", json!({ "step": "waiting", "msg_id": msg_id }));
            }
            // The one look at start; from here the engine is told of each message left waiting.
            if waiting {
                state.note_waiting_row();
            }
            rows
        }
        _ => {
            tracing::warn!(reason = "outbox_unreadable", "outbox not read at startup");
            return mgr;
        }
    };
    if rows.is_empty() {
        return mgr;
    }
    let total = rows.len();
    let pool = state.db.clone();
    let (mgr, unreadable) = tokio::task::spawn_blocking(move || {
        let mut mgr = mgr;
        let unreadable = mgr.resume_outbox(rows, &pool);
        (mgr, unreadable)
    })
    .await
    .expect("outbox resume task panicked");
    tracing::info!(resumed = total - unreadable, unreadable, "outbox resumed");
    mgr
}

// Orphan/send watchdog. Actual bounded packet/Link/Resource clocks take
// precedence; queued, expired and abandoned owners retain this ceiling.
const MESSAGE_TIMEOUT_SECS: f64 = 180.0;

/// The reason a send ended with: the verdict's when it ended the wait, else its own.
fn failure_reason_after(
    reason: producer::DeliveryFailureReason,
    verdict: AfterFailure,
) -> producer::DeliveryFailureReason {
    match verdict {
        AfterFailure::Wait | AfterFailure::Fail => reason,
        AfterFailure::FailExpired => producer::DeliveryFailureReason::WaitExpired,
        AfterFailure::FailExhausted => producer::DeliveryFailureReason::AttemptsExhausted,
    }
}

/// The Activity entry a step of the tick makes, if it makes one.
fn step_activity_state(step: OutboundState) -> Option<producer::LxmfDeliveryState> {
    match step {
        OutboundState::Routing => Some(producer::LxmfDeliveryState::Routing),
        OutboundState::Propagating => Some(producer::LxmfDeliveryState::Propagating),
        OutboundState::ReusingBackchannel => Some(producer::LxmfDeliveryState::ReusingBackchannel),
        OutboundState::SendingViaLink => Some(producer::LxmfDeliveryState::SendingViaLink),
        OutboundState::Sent => Some(producer::LxmfDeliveryState::Sent),
        OutboundState::Delivered => Some(producer::LxmfDeliveryState::Delivered),
        OutboundState::Propagated => Some(producer::LxmfDeliveryState::Propagated),
        OutboundState::Rejected => Some(producer::LxmfDeliveryState::Rejected),
        OutboundState::Failed => Some(producer::LxmfDeliveryState::Failed),
        OutboundState::Sending
        | OutboundState::LinkEstablishing
        | OutboundState::ReusingDirectLink
        | OutboundState::Cancelled
        | OutboundState::Timeout
        | OutboundState::Paper => None,
        OutboundState::Waiting => Some(producer::LxmfDeliveryState::Waiting),
    }
}

fn polled_delivery_method_override(
    step: &str,
    has_packet_proof: bool,
    evidence: Option<lxmf::LxmfDeliveryProgressMethod>,
) -> Option<&'static str> {
    match step {
        "sent" => Some("opportunistic"),
        "delivered" if has_packet_proof => Some("opportunistic"),
        "propagating" | "propagated" => Some("propagated"),
        _ => evidence.map(|method| match method {
            lxmf::LxmfDeliveryProgressMethod::Direct => "direct",
            lxmf::LxmfDeliveryProgressMethod::PropagationDeposit => "propagated",
        }),
    }
}

fn lxmf_step_starts_delivery_timeout(step: &str) -> bool {
    PROGRESS_STEPS.contains(&step)
        || OutboundState::parse(step).is_some_and(OutboundState::starts_delivery_clock)
}

fn lxmf_step_ends_delivery_timeout(step: &str) -> bool {
    OutboundState::parse(step).is_some_and(OutboundState::ends_delivery_clock)
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
    fn late_packet_proof_records_actual_method_even_after_propagation_fallback() {
        assert_eq!(
            super::polled_delivery_method_override("delivered", true, None),
            Some("opportunistic")
        );
        assert_eq!(
            super::polled_delivery_method_override("delivered", false, None),
            None
        );
        assert_eq!(
            super::polled_delivery_method_override("propagated", false, None),
            Some("propagated")
        );
    }

    #[test]
    fn late_selected_method_uses_actual_dispatch_evidence() {
        use super::lxmf::LxmfDeliveryProgressMethod;
        assert_eq!(
            super::polled_delivery_method_override("sent", false, None),
            Some("opportunistic")
        );
        assert_eq!(
            super::polled_delivery_method_override(
                "link_establishing",
                false,
                Some(LxmfDeliveryProgressMethod::Direct)
            ),
            Some("direct")
        );
        assert_eq!(
            super::polled_delivery_method_override(
                "link_establishing",
                false,
                Some(LxmfDeliveryProgressMethod::PropagationDeposit)
            ),
            Some("propagated")
        );
        assert_eq!(
            super::polled_delivery_method_override("routing", false, None),
            None
        );
        assert_eq!(
            super::polled_delivery_method_override(
                "delivered",
                true,
                Some(LxmfDeliveryProgressMethod::PropagationDeposit)
            ),
            Some("opportunistic")
        );
    }

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
            "waiting",
        ] {
            assert!(lxmf_step_ends_delivery_timeout(step), "{step}");
        }
    }

    #[test]
    fn waiting_retires_the_delivery_clock() {
        let mut times = std::collections::HashMap::new();
        update_message_delivery_timeout_at(&mut times, "m", "routing", 10.0);
        assert_eq!(times.get("m"), Some(&10.0));
        update_message_delivery_timeout_at(&mut times, "m", "waiting", 20.0);
        assert!(
            times.is_empty(),
            "nothing is in the air for the watchdog to time"
        );
        assert!(!lxmf_step_starts_delivery_timeout("waiting"));
    }

    #[test]
    fn the_verdict_names_why_a_wait_ended() {
        use crate::activity::producer::DeliveryFailureReason as Reason;
        use ratspeak_core::delivery::AfterFailure;
        for (verdict, code) in [
            (AfterFailure::Wait, "path_unavailable"),
            (AfterFailure::Fail, "path_unavailable"),
            (AfterFailure::FailExpired, "wait_expired"),
            (AfterFailure::FailExhausted, "attempts_exhausted"),
        ] {
            assert_eq!(
                super::failure_reason_after(Reason::PathUnavailable, verdict).code(),
                code
            );
        }
    }

    #[test]
    fn the_clock_and_activity_lists_equal_the_literals_they_replaced() {
        use crate::activity::producer::LxmfDeliveryState as Entry;
        use ratspeak_core::delivery::{OutboundState, PROGRESS_STEPS};
        let started = [
            "sent",
            "routing",
            "propagating",
            "resolving",
            "link_establishing",
            "sending_via_link",
            "resource_link_ready",
            "resource_advertised",
            "resource_transferring",
            "resource_waiting_for_proof",
            "reusing_direct_link",
            "reusing_backchannel",
        ];
        let ended = [
            "delivered",
            "propagated",
            "failed",
            "cancelled",
            "rejected",
            "timeout",
        ];
        let lease_released = ["delivered", "propagated", "rejected", "failed"];
        // Every step the manager's results carry.
        let manager_steps = [
            "routing",
            "propagating",
            "link_establishing",
            "sending_via_link",
            "reusing_direct_link",
            "reusing_backchannel",
            "sent",
            "delivered",
            "propagated",
            "rejected",
            "failed",
        ];
        let names = OutboundState::ALL
            .map(OutboundState::as_str)
            .into_iter()
            .chain(PROGRESS_STEPS)
            .chain(["resolving", "unknown", ""]);
        for name in names {
            // Nothing emits `resolving`, and `paper` is never a step.
            assert_eq!(
                lxmf_step_starts_delivery_timeout(name),
                started.contains(&name) && name != "resolving",
                "{name}"
            );
            assert_eq!(
                lxmf_step_ends_delivery_timeout(name),
                ended.contains(&name) || matches!(name, "paper" | "waiting"),
                "{name}"
            );
            let entry = match name {
                "routing" => Some(Entry::Routing),
                "propagating" => Some(Entry::Propagating),
                "reusing_backchannel" => Some(Entry::ReusingBackchannel),
                "sending_via_link" => Some(Entry::SendingViaLink),
                "sent" => Some(Entry::Sent),
                "delivered" => Some(Entry::Delivered),
                "propagated" => Some(Entry::Propagated),
                "rejected" => Some(Entry::Rejected),
                "failed" => Some(Entry::Failed),
                "waiting" => Some(Entry::Waiting),
                _ => None,
            };
            assert!(
                OutboundState::parse(name).and_then(super::step_activity_state) == entry,
                "{name}"
            );
        }
        for step in manager_steps {
            assert_eq!(
                OutboundState::parse(step).is_some_and(OutboundState::is_final),
                lease_released.contains(&step),
                "{step}"
            );
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

    let candidates: Vec<String> = if let Ok(times) = state.message_send_times.lock() {
        times
            .iter()
            .filter(|(_, send_time)| now - **send_time > MESSAGE_TIMEOUT_SECS)
            .map(|(id, _)| id.clone())
            .collect()
    } else {
        Vec::new()
    };
    let protected = bounded_protocol_waits(state, &candidates);
    let timed_out = candidates
        .into_iter()
        .filter(|id| !protected.contains(id))
        .collect::<Vec<_>>();
    if let Ok(mut times) = state.message_send_times.lock() {
        for id in &timed_out {
            times.remove(id);
        }
    }
    if timed_out.is_empty() {
        return;
    }
    // Game moves have no message row; their sessions settle them.
    let timed_out_moves: Vec<(String, crate::state::LrgpMsgMeta)> = state
        .lrgp_msg_to_session
        .lock()
        .map(|mut map| {
            timed_out
                .iter()
                .filter_map(|id| map.remove(id).map(|meta| (id.clone(), meta)))
                .collect()
        })
        .unwrap_or_default();
    if !timed_out_moves.is_empty() {
        if let Ok(mut lxmf) = state.lxmf.lock()
            && let Some(manager) = lxmf.as_mut()
        {
            for (msg_id, _) in &timed_out_moves {
                let _ = manager.cancel_outbound_message(msg_id);
            }
        }
        for (msg_id, meta) in &timed_out_moves {
            tracing::debug!(msg_id = %short_id(msg_id), "Game move timed out");
            record_game_action(
                state,
                activity_origin,
                msg_id,
                meta,
                producer::LrgpActionStage::Failed,
            );
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

    // One blocking-pool hop for the whole sweep. Only rows that win the
    // one-way terminal-state race may emit timeout or cancel live owners.
    let identity_id = helpers::active_identity_id(state);
    let ids_for_db = timed_out.clone();
    let keep_trying = state.lxmf_keep_trying_enabled();
    let settled_at = state.steady_now();
    // A message nothing holds any more got no confirmation in time.
    let orphaned = producer::DeliveryFailureReason::ProofTimedOut;
    let transitioned = db::spawn_db(state.db.clone(), move |p| {
        ids_for_db
            .iter()
            .filter_map(|msg_id| {
                let settled = db::settle_failure(
                    &p,
                    msg_id,
                    &identity_id,
                    orphaned.can_wait(),
                    keep_trying,
                    settled_at,
                    OutboundState::Timeout,
                )?;
                Some((
                    msg_id.clone(),
                    db::get_message_delivery_method(&p, msg_id, &identity_id),
                    settled,
                ))
            })
            .collect::<Vec<(String, Option<String>, db::Settled)>>()
    })
    .await
    .unwrap_or_default();

    // Stop every remaining local retry/Link/Resource owner after timeout is
    // durable. This cannot recall a packet already handed to the network.
    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(manager) = lxmf.as_mut()
    {
        for (msg_id, _, _) in &transitioned {
            let _ = manager.cancel_outbound_message(msg_id);
        }
    }

    for (msg_id, method, settled) in transitioned {
        state.release_attachment_delivery_lease(&msg_id);
        let client_msg_id = state
            .msg_id_map
            .lock()
            .ok()
            .and_then(|mut map| map.remove(&msg_id));
        let reason = failure_reason_after(orphaned, settled.verdict);
        if settled.state == OutboundState::Waiting {
            state.note_waiting_row();
        }
        let step = if settled.state == OutboundState::Waiting {
            json!({
                "step": "waiting",
                "msg_id": msg_id,
                "client_msg_id": client_msg_id,
                "reason": reason.code(),
                "method": method,
                "destination": settled.destination,
            })
        } else {
            json!({
                "step": "timeout",
                "msg_id": msg_id,
                "client_msg_id": client_msg_id,
                "reason": match settled.verdict {
                    AfterFailure::FailExpired | AfterFailure::FailExhausted => reason.code(),
                    AfterFailure::Fail | AfterFailure::Wait => "timeout",
                },
                "method": method,
                "destination": settled.destination,
            })
        };
        state.emit_to_all("lxmf_step", step);
        tracing::debug!(msg_id = %short_id(&msg_id), timeout_secs = MESSAGE_TIMEOUT_SECS, state = settled.state.as_str(), "Message timed out");
        record_activity_if_current(state, activity_origin, || {
            let message = producer::MessageId::from_hex(&msg_id)?;
            let method = method
                .as_deref()
                .and_then(producer::LxmfDeliveryMethod::from_code);
            Ok(producer::lxmf_delivery_state_changed(
                producer::LxmfDeliveryStateChanged {
                    message,
                    state: if settled.state == OutboundState::Waiting {
                        producer::LxmfDeliveryState::Waiting
                    } else {
                        producer::LxmfDeliveryState::Failed
                    },
                    method,
                    rtt_ms: None,
                    failure_reason: Some(reason),
                    interface: None,
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
        if matches!(current.as_str(), "delivered" | "failed" | "undelivered")
            || game_move_keeps_sent(&current, &ns)
        {
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

/// A move that went out stays "sent" while its copy is retried; storing it, a proof, or a failure moves it on.
fn game_move_keeps_sent(current: &str, next: &str) -> bool {
    current == "sent" && next != "propagating" && game_delivery_state_is_in_flight(next)
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

fn bounded_protocol_waits(state: &AppState, ids: &[String]) -> std::collections::HashSet<String> {
    state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| {
            lxmf.as_ref().map(|manager| {
                ids.iter()
                    .filter(|id| manager.still_owned(id))
                    .cloned()
                    .collect()
            })
        })
        .unwrap_or_default()
}

async fn sweep_stale_game_deliveries(state: &AppState) {
    let _game_transaction = Arc::new(state.lrgp_transaction.clone().lock_owned().await);
    // A move LXMF still owns settles through its own delivery or timeout; a
    // failure here would stick even if the move is then delivered.
    let protected_sessions: std::collections::HashSet<(String, String)> = state
        .lrgp_msg_to_session
        .lock()
        .map(|map| {
            map.values()
                .map(|meta| (meta.session_id.clone(), meta.identity_id.clone()))
                .collect()
        })
        .unwrap_or_default();
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
        if protected_sessions.contains(&(sid.clone(), iid.clone())) {
            continue;
        }
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

    let changed_identities = db::spawn_db_with_lease(
        state.db.clone(),
        Arc::clone(&_game_transaction),
        move |pool| {
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
        },
    )
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
        let all = db::spawn_db_with_lease(
            state.db.clone(),
            Arc::clone(&_game_transaction),
            move |pool| db::list_game_sessions(&pool, &identity_id, None, None),
        )
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
                "signal": state.peer_signal_json(&r.hash),
            })
        })
        .collect();
    state.emit_to_all("peers_updated", json!({ "peers": arr }));
}

const SENDER_PATH_REQUEST_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Asks once for the path of a sender whose announce never reached us, as
/// NomadNet does for a new conversation. A hub answers from its cache even
/// while it rate-limits the announce, and the path response carries the
/// sender's name and status. Paced per sender.
fn request_unknown_sender_path(state: &Arc<AppState>, source: [u8; 16], identity_known: bool) {
    let Some(tx) = state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.transport_tx.clone()))
    else {
        return;
    };
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let source_hex = hex::encode(source);
        if identity_known {
            let hash = source_hex.clone();
            let stored = db::spawn_db(state.db.clone(), move |p| {
                db::has_stored_announce(&p, &hash)
            })
            .await
            .unwrap_or(true);
            if stored {
                return;
            }
        }
        {
            let Ok(mut asked) = state.sender_path_requests.lock() else {
                return;
            };
            let now = Instant::now();
            if asked
                .get(&source)
                .is_some_and(|at| now.duration_since(*at) < SENDER_PATH_REQUEST_INTERVAL)
            {
                return;
            }
            asked.retain(|_, at| now.duration_since(*at) < SENDER_PATH_REQUEST_INTERVAL);
            asked.insert(source, now);
        }
        tracing::debug!(
            from = %short_id(&source_hex),
            identity_known,
            "requesting the path of a sender with no stored announce"
        );
        let _ = tx
            .send(rns_transport::messages::TransportMessage::RequestPath {
                destination_hash: source,
            })
            .await;
    });
}

async fn touch_peer_last_heard(
    state: &AppState,
    fence: &InboundFence,
    identity_id: &str,
    source_hash: &str,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let hash = source_hash.to_string();
    let identity_id = identity_id.to_string();
    let rows = db::spawn_db(state.db.clone(), move |p| {
        db::touch_identity_last_heard(&p, &hash, now);
        db::get_peers_by_hashes(&p, &[hash], &identity_id)
    })
    .await
    .unwrap_or_default();
    fence
        .publish(state, || emit_peers_batch(state, &rows))
        .await;
}

fn is_own_announce(
    announce: &rns_transport::messages::AnnounceRpcEntry,
    local_identity: Option<[u8; 16]>,
) -> bool {
    local_identity.is_some_and(|local| {
        announce
            .public_key
            .is_some_and(|key| rns_crypto::sha::truncated_hash(&key) == local)
    })
}

// Three wire shapes: UTF-8 string, msgpack BIN/STR (NomadNet),
// msgpack fixarray(1)[bin8(name)] (rsdeck/ratcom).
/// Whether an announce's `timestamp` is newer than the one applied for `dest`.
fn announce_is_newer(
    applied: &std::collections::HashMap<[u8; 16], f64>,
    dest: &[u8; 16],
    timestamp: f64,
) -> bool {
    applied
        .get(dest)
        .is_none_or(|seen| timestamp > seen + 0.001)
}

/// The entries a poll generation's first listing holds that the network
/// runtime did not record since it started: rows restored from storage. One
/// is heard once Reticulum records it again, past the timestamp it came with.
#[derive(Default)]
struct RestoredAnnounces {
    timestamps: std::collections::HashMap<[u8; 16], f64>,
}

impl RestoredAnnounces {
    /// `running` is the runtime's age on the boot clock, so a wall-clock step
    /// since it started cannot stretch the span over restored timestamps; a
    /// restored row stamped ahead of `now` stays restored.
    fn from_listing(
        announces: &[rns_transport::messages::AnnounceRpcEntry],
        running: Duration,
        now: f64,
    ) -> Self {
        let recorded = (now - running.as_secs_f64())..=now;
        Self {
            timestamps: announces
                .iter()
                .filter(|a| !recorded.contains(&a.timestamp))
                .map(|a| (a.dest_hash, a.timestamp))
                .collect(),
        }
    }

    /// Heard during this run. A row restored without its facts never was.
    fn heard_now(&self, a: &rns_transport::messages::AnnounceRpcEntry) -> bool {
        a.facts_known && announce_is_newer(&self.timestamps, &a.dest_hash, a.timestamp)
    }

    /// A hearing that touches the peer and wakes what waits for it: past what
    /// the last poll applied, heard during this run, not a path response.
    fn touches_peer(
        &self,
        applied: &std::collections::HashMap<[u8; 16], f64>,
        a: &rns_transport::messages::AnnounceRpcEntry,
    ) -> bool {
        announce_is_newer(applied, &a.dest_hash, a.timestamp)
            && self.heard_now(a)
            && !a.is_path_response
    }
}

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
    /// The receiving identity: only its live manager answers.
    receiver: &'a str,
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
        receiver,
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
    let queued = with_receiver(state, receiver, |manager| {
        manager.send_message_with_lrgp_fields_preference(
            destination,
            &fallback,
            &fields,
            &state.db,
            identity_id,
            lxmf::DeliveryPreference::Opportunistic,
        )
    })
    .flatten()
    .is_some();
    if queued {
        state.lxmf_notify.notify_one();
    }
}

// What an LRGP envelope shows once handled; `None` → not LRGP, fall through.
async fn try_handle_inbound_lrgp(
    state: &AppState,
    msg: &lxmf_core::message_api::LxMessage,
    sender_hash: &str,
    receiver: &InboundReceiver,
    sender_authenticated: bool,
) -> Option<LrgpOutcome> {
    // Game sessions belong to the receiver's LXMF destination.
    let identity_id = receiver.lxmf_id.as_str();
    let mut outcome = LrgpOutcome::default();
    let envelope = match inbound_admission::decode_game(msg, sender_authenticated) {
        Ok(Some(envelope)) => envelope,
        Ok(None) => return None,
        Err(reason) => {
            tracing::warn!(?reason, from = %short_id(sender_hash), "Dropping invalid LRGP envelope");
            return Some(outcome);
        }
    };
    let _game_transaction = Arc::new(state.lrgp_transaction.clone().lock_owned().await);

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
            return Some(outcome);
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
    let replayed_after_restart = db::spawn_db_with_lease(
        state.db.clone(),
        Arc::clone(&_game_transaction),
        move |pool| db::has_game_nonce(&pool, &sid, &iid, &nonce_for_db),
    )
    .await
    .unwrap_or(false);
    if replayed_after_restart {
        tracing::debug!(session_id, command, "Dropping durable LRGP replay");
        return Some(outcome);
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
            return Some(outcome);
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
                let message_timestamp = msg.timestamp();
                db::spawn_db_with_lease(
                    state.db.clone(),
                    Arc::clone(&_game_transaction),
                    move |pool| {
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
                    },
                )
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
                outcome.emit(
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
                return Some(outcome);
            } else {
                let iid = identity_id.to_string();
                let all = db::spawn_db_with_lease(
                    state.db.clone(),
                    Arc::clone(&_game_transaction),
                    move |pool| db::list_game_sessions(&pool, &iid, None, None),
                )
                .await
                .unwrap_or_default();
                outcome.emit("all_game_sessions", all.into());
            }
            tracing::warn!(
                session = %short_id(&error.session_id),
                code = %error_payload["code"].as_str().unwrap_or("protocol_error"),
                action_kind = %error_payload["ref"].as_str().unwrap_or("action"),
                "Remote LRGP peer reported a protocol error"
            );
            outcome.emit(
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
            outcome.emit(
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
            return Some(outcome);
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
            if matches!(&error, lrgp::protocol::LrgpError::SessionExpired(_))
                && let Some(Some(expired)) = state.lrgp_router.with_app(&app_id, |app| {
                    app.get_session_record(&session_id, identity_id)
                })
            {
                let _ = db::spawn_db_with_lease(
                    state.db.clone(),
                    Arc::clone(&_game_transaction),
                    move |pool| {
                        db::save_game_session(&pool, &expired);
                    },
                )
                .await;
            }
            send_lrgp_error_best_effort(
                state,
                LrgpErrorReply {
                    receiver: &receiver.identity_id,
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
            return Some(outcome);
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
        return Some(outcome);
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
                receiver: &receiver.identity_id,
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
        outcome.emit(
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
        return Some(outcome);
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
        let timestamp = msg.timestamp();
        let payload_json = serde_json::to_string(&payload_json).unwrap_or_else(|_| "{}".into());
        let session = persisted_session.clone();
        db::spawn_db_with_lease(state.db.clone(), Arc::clone(&_game_transaction), move |p| {
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
        outcome.emit(
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
        return Some(outcome);
    };

    outcome.alert = Some(GameAlert {
        label: String::new(),
        sender_hash: sender_hash.to_string(),
        session_id: session_id.clone(),
        app_id: app_id.clone(),
        command: command.clone(),
        is_new_session: !had_session,
    });

    outcome.emit(
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
    outcome.emit("all_game_sessions", all.into());

    // Positive per-action signal so the frontend can force-redraw the active
    // board even if the bulk `all_game_sessions` payload looks identical.
    outcome.emit(
        "game_action_received",
        json!({
            "session_id": session_id,
            "app_id": app_id,
            "command": command,
            "from": sender_hash,
            "applied": true,
        }),
    );

    Some(outcome)
}

#[cfg(test)]
mod announce_ingest_tests {
    use super::*;

    #[test]
    fn announces_past_the_history_cap_are_not_new_again() {
        let applied: std::collections::HashMap<[u8; 16], f64> = (0..(ANNOUNCE_HISTORY_CAP + 1000)
            as u32)
            .map(|n| {
                let mut dest = [0u8; 16];
                dest[..4].copy_from_slice(&n.to_be_bytes());
                (dest, 100.0)
            })
            .collect();
        assert!(
            applied
                .keys()
                .all(|dest| !announce_is_newer(&applied, dest, 100.0))
        );
        let first = *applied.keys().next().unwrap();
        assert!(announce_is_newer(&applied, &first, 101.0));
        assert!(announce_is_newer(&applied, &[0xff; 16], 1.0));
    }

    fn heard(dest: u8, timestamp: f64) -> rns_transport::messages::AnnounceRpcEntry {
        let mut entry = rns_transport::messages::AnnounceRpcEntry::default();
        entry.dest_hash = [dest; 16];
        entry.timestamp = timestamp;
        entry.facts_known = true;
        entry
    }

    fn applied(
        listing: &[rns_transport::messages::AnnounceRpcEntry],
    ) -> std::collections::HashMap<[u8; 16], f64> {
        listing.iter().map(|a| (a.dest_hash, a.timestamp)).collect()
    }

    // Each start lists every remembered peer: only what this run hears again
    // touches it, wakes what waits for it, or refreshes its costs.
    #[test]
    fn a_restored_announce_is_heard_only_once_recorded_again() {
        let started = 1_000_000.0;
        let restored = heard(1, started - 86_400.0);
        let heard_since_start = heard(2, started + 2.0);
        let mut without_facts = heard(3, started + 1.0);
        without_facts.facts_known = false;
        let ahead = heard(4, started + 3_600.0);
        let first = vec![
            restored.clone(),
            heard_since_start.clone(),
            without_facts.clone(),
            ahead.clone(),
        ];
        let start = RestoredAnnounces::from_listing(&first, Duration::from_secs(5), started + 5.0);
        let nothing_applied = std::collections::HashMap::new();
        for remembered in [&restored, &without_facts, &ahead] {
            assert!(!start.heard_now(remembered));
            assert!(!start.touches_peer(&nothing_applied, remembered));
        }
        assert!(start.heard_now(&heard_since_start));
        assert!(start.touches_peer(&nothing_applied, &heard_since_start));

        let reheard = heard(1, started + 60.0);
        let new_peer = heard(5, started + 61.0);
        let mut second = first.clone();
        second[0] = reheard.clone();
        second.push(new_peer.clone());
        for live in [&reheard, &new_peer] {
            assert!(start.heard_now(live));
            assert!(start.touches_peer(&applied(&first), live));
            assert!(!start.touches_peer(&applied(&second), live), "heard once");
        }
        let mut path_response = heard(6, started + 62.0);
        path_response.is_path_response = true;
        assert!(start.heard_now(&path_response));
        assert!(!start.touches_peer(&applied(&second), &path_response));

        let restarted =
            RestoredAnnounces::from_listing(&second, Duration::from_secs(5), started + 900.0);
        let nothing_applied = std::collections::HashMap::new();
        for entry in &second {
            assert!(!restarted.heard_now(entry));
            assert!(!restarted.touches_peer(&nothing_applied, entry));
        }
    }

    #[test]
    fn a_wall_clock_step_does_not_make_restored_announces_heard() {
        // The runtime started 30 s ago on a clock a day behind; the first poll
        // reads the corrected one, which a wall-clock start mark would not.
        let now = 2_000_000.0;
        let restored = heard(1, now - 3_600.0);
        let heard_since_step = heard(2, now - 1.0);
        let listing = [restored.clone(), heard_since_step.clone()];
        let start = RestoredAnnounces::from_listing(&listing, Duration::from_secs(30), now);
        assert!(!start.heard_now(&restored));
        assert!(start.heard_now(&heard_since_step));
    }

    #[test]
    fn own_announces_are_not_ingested_as_peers() {
        let local = rns_identity::identity::Identity::new();
        let remote = rns_identity::identity::Identity::new();
        let entry = |public_key: Option<[u8; 64]>| {
            let mut entry = rns_transport::messages::AnnounceRpcEntry::default();
            entry.dest_hash = [0x11; 16];
            entry.hops = 1;
            entry.app_data = None;
            entry.timestamp = 1.0;
            entry.public_key = public_key;
            entry.ratchet = None;
            entry.name_hash = [0; 10];
            entry.is_path_response = false;
            entry.retained = false;
            entry
        };
        let own = entry(Some(local.get_public_key()));
        assert!(is_own_announce(&own, Some(local.hash)));
        assert!(!is_own_announce(&own, None));
        assert!(!is_own_announce(
            &entry(Some(remote.get_public_key())),
            Some(local.hash)
        ));
        assert!(!is_own_announce(&entry(None), Some(local.hash)));
    }
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
    fn a_sent_move_never_shows_sending_again() {
        for next in ["pending", "sending", "routing", "link_establishing", "sent"] {
            assert!(game_move_keeps_sent("sent", next), "{next}");
        }
        for next in [
            "propagating",
            "propagated",
            "delivered",
            "failed",
            "rejected",
        ] {
            assert!(!game_move_keeps_sent("sent", next), "{next}");
        }
        assert!(!game_move_keeps_sent("sending", "routing"));
        assert!(!game_move_keeps_sent("pending", "sent"));
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

    include!("inbound_content_tests.rs");

    #[derive(Default)]
    pub(super) struct RecordingEmitter {
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
        pub(super) fn payloads(&self, name: &str) -> Vec<serde_json::Value> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|(event, _)| event == name)
                .map(|(_, payload)| payload.clone())
                .collect()
        }

        pub(super) fn count(&self, name: &str) -> usize {
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

    pub(super) fn pipeline_state() -> (Arc<AppState>, Arc<RecordingEmitter>) {
        let unique = TEMP_PIPELINE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "ratspeak-inbound-pipeline-{}-{}-{unique}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        crate::lxmf::initialize_storage_namespace(&root).unwrap();
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

    pub(super) fn local_dest(state: &AppState) -> [u8; 16] {
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

    #[tokio::test]
    async fn propagation_path_responses_preserve_current_signed_node_metadata() {
        let _stamps = crate::announce_stamps::test_policy::unstamped().await;
        use rns_transport::link_messages::{AnnounceRequest, DestinationEvent};
        use rns_transport::messages::TransportMessage;

        fn metadata(
            raw: &[u8],
            expected_hash: &[u8; 16],
        ) -> lxmf_core::handlers::PropagationNodeAnnounceData {
            let (header, offset) = rns_wire::header::PacketHeader::unpack(raw).unwrap();
            assert_eq!(header.destination_hash, *expected_hash);
            assert!(
                !header.flags.context_flag,
                "PN announcements never carry delivery ratchets"
            );
            let announce = rns_identity::announce::AnnounceData::unpack(
                &raw[offset..],
                header.flags.context_flag,
            )
            .unwrap();
            announce
                .validate(expected_hash)
                .expect("genuine signed PN announcement");
            lxmf_core::handlers::parse_pn_announce_data(announce.app_data.as_deref().unwrap())
                .unwrap()
        }

        let (state, _) = pipeline_state();
        let (transport_tx, mut outbound) = tokio::sync::mpsc::channel(16);
        let (events, event_rx) = tokio::sync::mpsc::channel(16);
        let (mut manager, destination) = {
            let owner = state.lxmf.lock().unwrap();
            let owner = owner.as_ref().unwrap();
            (
                rns_runtime::link_manager::LinkManager::with_destination(
                    transport_tx,
                    event_rx,
                    &owner.identity,
                    LXMF_PROPAGATION_APP_NAME,
                    owner.identity.get_signing_key(),
                ),
                owner.propagation_dest_hash,
            )
        };
        install_propagation_announce_metadata(&mut manager, &state, destination);
        let worker = tokio::spawn(manager.run());
        let mut first_response = None;

        for (index, (hosting, ready, static_only, cost, limit)) in [
            (true, true, false, 13, 256),
            (false, true, false, 17, 512),
            (true, true, true, 21, 1024),
            (true, true, false, 23, 2048),
            (true, false, false, 23, 2048),
        ]
        .into_iter()
        .enumerate()
        {
            state
                .propagation_node_hosting_enabled
                .store(hosting, Ordering::Relaxed);
            state
                .propagation_node_stamp_cost
                .store(cost, Ordering::Relaxed);
            let identity = state
                .lxmf
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .identity_hash
                .clone();
            assert!(state.set_propagation_host_readiness(
                identity,
                state.current_identity_session_generation(),
                if ready {
                    crate::state::PropagationHostReadiness::Ready
                } else {
                    crate::state::PropagationHostReadiness::Unavailable
                },
                None
            ));
            let ordinary = {
                let mut owner = state.lxmf.lock().unwrap();
                let owner = owner.as_mut().unwrap();
                apply_lxmf_settings_from_state(&state, owner);
                owner.router.config.propagation_limit_kb = (limit as u64).into();
                owner.router.config.sync_limit_kb = ((limit * 2) as u64).into();
                owner.router.config.ext.from_static_only = static_only;
                owner.router.config.ext.name = Some(format!("current node {index}"));
                owner.create_propagation_announce_packet().unwrap()
            };
            events
                .send(DestinationEvent::AnnounceRequested(AnnounceRequest {
                    app_name: LXMF_PROPAGATION_APP_NAME.into(),
                    path_response: true,
                    tag: Some(vec![index as u8 + 1; 16]),
                    attached_interface: Some(17),
                }))
                .await
                .unwrap();
            let message = tokio::time::timeout(Duration::from_secs(2), outbound.recv())
                .await
                .unwrap()
                .unwrap();
            let TransportMessage::OutboundAttached {
                request,
                interface_id,
            } = message
            else {
                panic!("path response must use the requesting interface");
            };
            assert_eq!(interface_id, 17);
            let (header, _) = rns_wire::header::PacketHeader::unpack(&request.raw).unwrap();
            assert_eq!(
                header.context,
                rns_wire::context::PacketContext::PathResponse
            );
            let broadcast = metadata(&ordinary, &destination);
            if !hosting || !ready {
                let (header, offset) =
                    rns_wire::header::PacketHeader::unpack(&request.raw).unwrap();
                let announce = rns_identity::announce::AnnounceData::unpack(
                    &request.raw[offset..],
                    header.flags.context_flag,
                )
                .unwrap();
                announce.validate(&destination).unwrap();
                assert!(
                    announce
                        .app_data
                        .as_deref()
                        .is_none_or(|data| data.is_empty()),
                    "unavailable hosting must not advertise active node policy"
                );
                assert!(!broadcast.node_state);
                continue;
            }
            let response = metadata(&request.raw, &destination);
            assert_eq!(response.node_state, hosting && ready && !static_only);
            assert_eq!(response.stamp_cost, f64::from(cost));
            assert_eq!(response.transfer_limit.as_f64(), limit as f64);
            assert_eq!(response.sync_limit.as_f64(), (limit * 2) as f64);
            assert_eq!(
                response.metadata.get(&lxmf_core::constants::PN_META_NAME),
                Some(&format!("current node {index}").into_bytes())
            );
            assert_eq!(response.node_state, broadcast.node_state);
            assert_eq!(response.stamp_cost, broadcast.stamp_cost);
            assert_eq!(response.stamp_flex, broadcast.stamp_flex);
            assert_eq!(response.peering_cost, broadcast.peering_cost);
            assert_eq!(response.transfer_limit, broadcast.transfer_limit);
            assert_eq!(response.sync_limit, broadcast.sync_limit);
            assert_eq!(response.metadata, broadcast.metadata);
            first_response.get_or_insert(request.raw);
        }

        // Preserve the destination's protocol-level duplicate-tag cache: a
        // repeated request reuses its exact signature, not a new policy epoch.
        events
            .send(DestinationEvent::AnnounceRequested(AnnounceRequest {
                app_name: LXMF_PROPAGATION_APP_NAME.into(),
                path_response: true,
                tag: Some(vec![1; 16]),
                attached_interface: Some(17),
            }))
            .await
            .unwrap();
        let TransportMessage::OutboundAttached { request, .. } =
            tokio::time::timeout(Duration::from_secs(2), outbound.recv())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("repeated attached path response");
        };
        assert_eq!(Some(request.raw), first_response);
        drop(events);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn hosted_propagation_deposit_retains_validated_stamp_and_survives_restart() {
        use lxmf_core::propagation_node::{PropagationNode, PropagationNodeConfig};
        use rmpv::Value;

        let storage = tempfile::tempdir().unwrap();
        let recipient = rns_identity::identity::Identity::new();
        let destination =
            Destination::hash_from_name_and_identity(LXMF_DELIVERY_APP_NAME, Some(&recipient.hash));
        // The node must treat destination+encrypted data as opaque; only the
        // actual recipient can authenticate/decrypt it after client retrieval.
        let plaintext = b"authenticated private body retained across PN restart".repeat(8);
        let mut blob = destination.to_vec();
        blob.extend(recipient.encrypt(&plaintext, None).unwrap());
        let transient_id = rns_crypto::sha::full_hash(&blob);
        let (stamp, expanded_value) = lxmf_core::stamper::generate_stamp(
            &transient_id,
            1.0,
            lxmf_core::constants::STAMP_WORKBLOCK_EXPAND_ROUNDS_PN,
            &lxmf_core::stamper::StampControl::default(),
        )
        .unwrap()
        .expect("real low-cost propagation stamp");
        let mut entry = blob.clone();
        entry.extend(stamp);
        let config = PropagationNodeConfig {
            min_stamp_cost: 1.0,
            ..Default::default()
        };
        let mut node = PropagationNode::with_storage(
            config.clone(),
            [0xA1; 16],
            storage.path().join("messages"),
        )
        .unwrap();
        assert_eq!(
            accept_hosted_propagation_deposit(&mut node, &entry),
            Some(true)
        );
        assert_eq!(
            accept_hosted_propagation_deposit(&mut node, &entry),
            Some(false)
        );
        assert_eq!(accept_hosted_propagation_deposit(&mut node, &[0; 16]), None);
        assert_eq!(node.message_count(), 1);
        let files = std::fs::read_dir(storage.path().join("messages.v2").join("objects"))
            .unwrap()
            .map(|file| file.unwrap().path())
            .filter(|path| {
                !path.to_string_lossy().ends_with(".entry")
                    && !path.to_string_lossy().ends_with(".active")
            })
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 1);
        assert_eq!(
            std::fs::read(&files[0]).unwrap(),
            entry,
            "preserve exact validated stamp, not just ciphertext"
        );

        let request = Value::Array(vec![
            Value::Array(vec![Value::Binary(transient_id.to_vec())]),
            Value::Array(vec![]),
        ]);
        let mut request_bytes = Vec::new();
        rmpv::encode::write_value(&mut request_bytes, &request).unwrap();
        for restarted in [false, true] {
            if restarted {
                drop(node);
                node = PropagationNode::with_storage(
                    config.clone(),
                    [0xA1; 16],
                    storage.path().join("messages"),
                )
                .unwrap();
            }
            assert_eq!(node.message_count(), 1);
            let response = node
                .handle_get_request(&request_bytes, &destination)
                .into_response();
            let value = rmpv::decode::read_value(&mut response.as_slice()).unwrap();
            let messages = value.as_array().unwrap();
            assert_eq!(messages.len(), 1);
            let retrieved = messages[0].as_slice().unwrap();
            assert_eq!(
                retrieved, blob,
                "exact opaque data before/after restart, without node stamp"
            );
            assert_eq!(
                recipient.decrypt(&retrieved[16..], None, false).unwrap(),
                plaintext
            );
        }

        // Either workblock form passes (LXMF 1.3.1): refuse above both values.
        let stamp_value = lxmf_core::stamper::validate_pn_stamp(&entry, 0.0)
            .unwrap()
            .2
            .max(expanded_value);
        assert!(stamp_value < lxmf_core::stamper::STAMP_COST_CAP);
        node.set_min_stamp_cost(stamp_value + 1.0 / 256.0);
        assert_eq!(accept_hosted_propagation_deposit(&mut node, &entry), None);
        let mut limited = PropagationNode::with_storage(
            PropagationNodeConfig {
                max_message_size: blob.len(),
                min_stamp_cost: 1.0,
                ..Default::default()
            },
            [0xA2; 16],
            storage.path().join("limited"),
        )
        .unwrap();
        assert_eq!(
            accept_hosted_propagation_deposit(&mut limited, &entry),
            Some(false),
            "storage limit includes the canonical stamp bytes"
        );
        assert_eq!(limited.message_count(), 0);
    }

    #[test]
    fn propagation_default_metadata_does_not_retain_or_cross_session_owners() {
        let (state, _) = pipeline_state();
        let (transport_tx, _outbound) = tokio::sync::mpsc::channel(4);
        let (_events, event_rx) = tokio::sync::mpsc::channel(4);
        let (mut manager, destination) = {
            let owner = state.lxmf.lock().unwrap();
            let owner = owner.as_ref().unwrap();
            (
                rns_runtime::link_manager::LinkManager::with_destination(
                    transport_tx,
                    event_rx,
                    &owner.identity,
                    LXMF_PROPAGATION_APP_NAME,
                    owner.identity.get_signing_key(),
                ),
                owner.propagation_dest_hash,
            )
        };
        state
            .propagation_node_hosting_enabled
            .store(true, Ordering::Relaxed);
        let identity = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .identity_hash
            .clone();
        assert!(state.set_propagation_host_readiness(
            identity,
            state.current_identity_session_generation(),
            crate::state::PropagationHostReadiness::Ready,
            None
        ));
        let owners = Arc::strong_count(&state);
        install_propagation_announce_metadata(&mut manager, &state, destination);
        assert_eq!(
            Arc::strong_count(&state),
            owners,
            "no state/worker ownership cycle"
        );
        assert!(manager.destination().unwrap().resolve_app_data().is_some());
        let original = state.lxmf.lock().unwrap().take();
        assert!(manager.destination().unwrap().resolve_app_data().is_none());
        let (replacement, _) = pipeline_state();
        *state.lxmf.lock().unwrap() = replacement.lxmf.lock().unwrap().take();
        assert!(manager.destination().unwrap().resolve_app_data().is_none());
        *state.lxmf.lock().unwrap() = original;
        assert!(manager.destination().unwrap().resolve_app_data().is_some());
        state.session_shutdown.read().unwrap().trigger();
        assert!(manager.destination().unwrap().resolve_app_data().is_none());
        *state.session_shutdown.write().unwrap() = rns_runtime::lifecycle::ShutdownSignal::new();
        assert!(
            manager.destination().unwrap().resolve_app_data().is_none(),
            "old worker cannot revive on a new session"
        );
        drop(state);
        assert!(manager.destination().unwrap().resolve_app_data().is_none());
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

    pub(super) fn packed_inbound(dest: [u8; 16], src: [u8; 16], content: &str) -> Vec<u8> {
        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            src,
            "",
            content,
            lxmf_core::message_api::DeliveryMethod::Direct,
        )
        .unwrap();
        // Signed by an unknown sender: verify returns None and the message is
        // still delivered, so tests don't need real peer keys.
        let msg = msg
            .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(
                &[7; 32],
            ))
            .unwrap()
            .finalize(None)
            .unwrap();
        msg.pack().unwrap()
    }

    fn packed_inbound_with_audio(
        dest: [u8; 16],
        src: [u8; 16],
        mode: u8,
        audio_bytes: &[u8],
    ) -> Vec<u8> {
        let mut msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            src,
            "",
            "Voice message",
            lxmf_core::message_api::DeliveryMethod::Direct,
        )
        .unwrap();
        msg.set_audio_field(mode, audio_bytes).unwrap();
        let msg = msg
            .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(
                &[7; 32],
            ))
            .unwrap()
            .finalize(None)
            .unwrap();
        msg.pack().unwrap()
    }

    pub(super) fn message_rows(state: &AppState) -> i64 {
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
        let _stamps = crate::announce_stamps::test_policy::blocking_unstamped();
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
        let _stamps = crate::announce_stamps::test_policy::blocking_unstamped();
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
        let _stamps = crate::announce_stamps::test_policy::blocking_unstamped();
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

    #[cfg(feature = "lxst-voice")]
    #[tokio::test]
    async fn compact_audio_pipeline_keeps_metadata_and_rejects_malformed_media_only() {
        for (length, valid) in [
            (4, true),
            (1500, true),
            (3000, true),
            (3, false),
            (3004, false),
        ] {
            let (state, emitter) = pipeline_state();
            let bytes = vec![0x42; length];
            let data = packed_inbound_with_audio(local_dest(&state), [0xED; 16], 3, &bytes);
            handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;
            let identity = local_identity(&state);
            let rows = db::get_conversation(&state.db, &hex::encode([0xED; 16]), &identity, 10);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["content"], "Voice message");
            assert_eq!(rows[0]["audio"]["mode"], 3);
            assert_eq!(rows[0]["audio"]["supported"], valid);
            assert_eq!(emitter.count("lxmf_message"), 1);
            if valid {
                let name = rows[0]["audio"]["stored_name"].as_str().unwrap();
                let path = state
                    .lxmf
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .get_received_file(name)
                    .unwrap();
                assert_eq!(std::fs::read(path).unwrap(), bytes);
                assert_eq!(
                    db::stored_audio_mode(&state.db, &identity, name).unwrap(),
                    3
                );
            } else {
                assert_eq!(rows[0]["audio"]["unavailable"], true);
            }
        }
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
        let mut msg = lxmf_core::message::LxMessageDraft::new(
            local_dest(&state),
            [0xEC; 16],
            "",
            "text survives malformed media",
            lxmf_core::message_api::DeliveryMethod::Direct,
        )
        .unwrap();
        msg.set_msgpack_field(
            lxmf_core::constants::FIELD_AUDIO,
            vec![0x93, lxmf_core::constants::AM_OPUS_OGG, 0xc4, 0x00, 0xc0],
        )
        .unwrap();
        let msg = msg
            .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(
                &[7; 32],
            ))
            .unwrap()
            .finalize(None)
            .unwrap();
        let data = msg.pack().unwrap();

        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;

        let identity = local_identity(&state);
        let conversation = db::get_conversation(&state.db, &hex::encode([0xEC; 16]), &identity, 10);
        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation[0]["content"], "text survives malformed media");
        assert_eq!(conversation[0]["audio"]["unavailable"], true);
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
                    && matches!(
                        query,
                        rns_transport::messages::TransportQuery::IsBlackholed { .. }
                    )
                {
                    let _ = response_tx.send(
                        rns_transport::messages::TransportQueryResponse::BoolResult(blackholed),
                    );
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

    /// Register a complete identity and derive its LXMF source address. The
    /// verified capability binds both public-key halves to this exact source.
    fn register_source_identity(
        state: &AppState,
        signing: &rns_crypto::ed25519::Ed25519PrivateKey,
    ) -> [u8; 16] {
        let mut pub_key = [0u8; 64];
        pub_key[..32].copy_from_slice(
            &rns_crypto::x25519::X25519PrivateKey::from_bytes(&[9; 32])
                .public_key()
                .to_bytes(),
        );
        pub_key[32..].copy_from_slice(&signing.public_key().to_bytes());
        let identity = rns_crypto::sha::truncated_hash(&pub_key);
        let src = rns_identity::destination::Destination::hash_from_name_and_identity(
            "lxmf.delivery",
            Some(&identity),
        );
        state
            .lxmf
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .known_identities
            .insert(hex::encode(src), pub_key);
        src
    }

    fn packed_signed_inbound(
        dest: [u8; 16],
        src: [u8; 16],
        content: &str,
        signing: &rns_crypto::ed25519::Ed25519PrivateKey,
    ) -> Vec<u8> {
        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            src,
            "",
            content,
            lxmf_core::message_api::DeliveryMethod::Direct,
        )
        .unwrap();
        let msg = msg.sign(signing).unwrap().finalize(None).unwrap();
        msg.pack().unwrap()
    }

    /// Blackholed-source gate (LXMRouter.py): a resolvable source
    /// with a blackholed identity is dropped; the same peer delivers once the
    /// transport says not-blackholed.
    #[tokio::test]
    async fn blackholed_source_is_dropped() {
        let (state, emitter) = pipeline_state();
        let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
        let src = register_source_identity(&state, &signing);

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
    /// Python's recall-gated check (LXMessage.py).
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
        let src = register_source_identity(&state, &signing);

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
        let session = state.session_shutdown.read().unwrap().clone();
        process_inbound_lxmf(
            &state,
            msg,
            opp_data.len(),
            InboundLxmfSource::Opportunistic { raw: Bytes::new() },
            state.activity_request_fence(),
            session,
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

    #[tokio::test]
    async fn a_message_that_came_live_wakes_what_waits_for_its_sender() {
        let (state, _emitter) = pipeline_state();
        state
            .lxmf_keep_trying
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (stored, live) = ([0xD1; 16], [0xD2; 16]);

        // From the Offline Inbox it may be hours old.
        let data = packed_inbound(local_dest(&state), stored, "left for later");
        handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;
        assert_eq!(message_rows(&state), 1);
        assert!(state.take_wake_signals().is_empty());

        let data = packed_inbound(local_dest(&state), live, "here now");
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
        let mut heard = lxmf::WakeSignals::default();
        heard.raise(lxmf::WakeEvent::Heard(live));
        assert_eq!(state.take_wake_signals(), heard);
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

        let mut msg = lxmf_core::message::LxMessageDraft::new(
            local_dest(&state),
            [0xEE; 16],
            "",
            "",
            lxmf_core::message_api::DeliveryMethod::Direct,
        )
        .unwrap();
        for (field_id, bytes) in
            lxmf::ratspeak_chat_custom_fields(&lxmf::RatspeakChatExtension::Reaction {
                target: target_id.clone(),
                emoji: "\u{1F44D}".to_string(),
                action: "add".to_string(),
            })
            .unwrap()
        {
            if field_id == lxmf_core::constants::FIELD_REACTION {
                msg.set_msgpack_field(field_id, bytes).unwrap();
            } else {
                msg.set_field(field_id, bytes).unwrap();
            }
        }
        let msg = msg
            .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(
                &[7; 32],
            ))
            .unwrap()
            .finalize(None)
            .unwrap();
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

        let mut msg = lxmf_core::message::LxMessageDraft::new(
            local_dest(&state),
            [0xEE; 16],
            "",
            "Reacted to your message with \u{1F44D}.",
            lxmf_core::message_api::DeliveryMethod::Direct,
        )
        .unwrap();
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
        let msg = msg
            .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(
                &[7; 32],
            ))
            .unwrap()
            .finalize(None)
            .unwrap();
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

        let mut msg = lxmf_core::message::LxMessageDraft::new(
            local_dest(&state),
            [0xEE; 16],
            "",
            "standard reply",
            lxmf_core::message_api::DeliveryMethod::Direct,
        )
        .unwrap();
        msg.set_field(lxmf_core::constants::FIELD_REPLY_TO, target_hash.to_vec())
            .unwrap();
        msg.set_field(lxmf_core::constants::FIELD_REPLY_QUOTE, b"quoted".to_vec())
            .unwrap();
        let msg = msg
            .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(
                &[7; 32],
            ))
            .unwrap()
            .finalize(None)
            .unwrap();
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
mod identity_session_tests {
    use super::*;
    use rns_link::clock::ContinuousInstant;

    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<(String, Value)>>);

    impl ratspeak_core::Emitter for Recorder {
        fn try_emit(&self, event: &str, payload: Value) -> Result<(), ratspeak_core::EmitError> {
            self.0.lock().unwrap().push((event.to_owned(), payload));
            Ok(())
        }
    }

    #[derive(Default)]
    struct Withdrawals(std::sync::atomic::AtomicUsize);

    impl ratspeak_core::NativeNotifier for Withdrawals {
        fn notify(&self, _notification: ratspeak_core::NativeNotification) {}
        fn withdraw_all(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct Session {
        _root: tempfile::TempDir,
        state: Arc<AppState>,
        hash: String,
    }

    /// An active identity loaded as `kind`, with that protection on disk
    /// unless it is `plain`, and its auto-lock armed.
    fn armed_session(
        kind: &'static str,
        emitter: Arc<dyn ratspeak_core::Emitter>,
        notifier: Arc<dyn ratspeak_core::NativeNotifier>,
    ) -> (Session, state::AutoLock) {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().to_path_buf();
        lxmf::initialize_storage_namespace(&data_root).unwrap();
        let config = crate::config::DashboardConfig {
            data_root: data_root.clone(),
            data_dir: data_root.join(".ratspeak"),
            rns_config_dir: data_root.join("rns"),
            rns_config_dir_overridden: true,
            max_log_entries: 20,
        };
        let pool = db::init_pool(&data_root).unwrap();
        db::init_schema(&pool).unwrap();
        let state = Arc::new(AppState::new(config, pool, emitter, notifier));
        let identity = rns_identity::identity::Identity::new();
        let hash = hex::encode(identity.hash);
        if kind == "passcode" {
            let generation = state.current_identity_session_generation();
            assert!(state.retain_unlocked_identity(identity.clone(), &hash, generation));
        }
        let manager =
            lxmf::LxmfManager::from_loaded_identity(&data_root, identity, kind == "hardware")
                .unwrap();
        let profile = state.config.data_dir.join("identities").join(&hash);
        match kind {
            "hardware" => std::fs::write(profile.join("identity.hwid"), b"{}").unwrap(),
            "passcode" => std::fs::write(profile.join("identity.enc"), b"{}").unwrap(),
            _ => {}
        }
        db::save_identity(&state.db, &hash, &manager.lxmf_hash, "A", "A");
        db::set_active_identity(&state.db, &hash).unwrap();
        state.set_lxmf(manager);
        let lock_kind = if kind == "hardware" {
            "hardware"
        } else {
            "passcode"
        };
        state.arm_auto_lock(&hash, lock_kind, Duration::from_secs(60), false);
        let lock = state.auto_lock.lock().unwrap().clone().unwrap();
        (
            Session {
                _root: root,
                state,
                hash,
            },
            lock,
        )
    }

    fn last_lock_event(recorder: &Recorder) -> Value {
        let events = recorder.0.lock().unwrap();
        events
            .iter()
            .rev()
            .find(|(event, _)| event == "hardware_locked")
            .map(|(_, payload)| payload.clone())
            .expect("no lock event")
    }

    // Locking ends the session: nothing begun under it may land behind the
    // lock, and its alerts go. A passcode identity locks as a hardware one
    // does, and its unlock goes with the session. The lock is private, so
    // this stays in the library's tests.
    #[tokio::test]
    async fn auto_lock_locks_either_kind_and_starts_a_new_identity_session() {
        let _hosts = crate::hosted_propagation::get_tests::GET_TEST_LOCK
            .lock()
            .await;
        for kind in ["hardware", "passcode"] {
            let recorder = Arc::new(Recorder::default());
            let withdrawals = Arc::new(Withdrawals::default());
            let (session, lock) = armed_session(kind, recorder.clone(), withdrawals.clone());
            let (state, hash) = (&session.state, &session.hash);
            let generation = state.current_identity_session_generation();

            lock_protected_session(Arc::clone(state), lock).await;

            assert_eq!(
                state.hw_locked_hash().as_deref(),
                Some(hash.as_str()),
                "{kind}"
            );
            assert!(
                state.current_identity_session_generation() > generation,
                "{kind}"
            );
            assert!(
                state
                    .network_session
                    .read()
                    .unwrap()
                    .retained_identity
                    .is_none(),
                "{kind}: the unlock outlived the lock"
            );
            assert!(state.auto_lock.lock().unwrap().is_none(), "{kind}");
            assert_eq!(
                withdrawals.0.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "{kind}: the locked identity's alerts stayed"
            );
            let locked = last_lock_event(&recorder);
            assert_eq!(locked["kind"], kind);
            assert_eq!(locked["reason"], "timeout");
        }
    }

    /// Records, with each event, which identity the state held locked then.
    #[derive(Default)]
    struct LockWitness {
        state: std::sync::OnceLock<std::sync::Weak<AppState>>,
        seen: std::sync::Mutex<Vec<(String, Value, Option<String>)>>,
    }

    impl ratspeak_core::Emitter for LockWitness {
        fn try_emit(&self, event: &str, payload: Value) -> Result<(), ratspeak_core::EmitError> {
            let locked = self
                .state
                .get()
                .and_then(std::sync::Weak::upgrade)
                .and_then(|state| state.hw_locked_hash());
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).push((
                event.to_owned(),
                payload,
                locked,
            ));
            Ok(())
        }
    }

    // A lock refuses the identity's data from its start: the teardown, which
    // waits on the network, never runs with the identity still open.
    #[tokio::test]
    async fn an_auto_lock_refuses_the_identitys_data_before_its_teardown() {
        let _hosts = crate::hosted_propagation::get_tests::GET_TEST_LOCK
            .lock()
            .await;
        let witness = Arc::new(LockWitness::default());
        let (session, lock) = armed_session(
            "passcode",
            witness.clone(),
            Arc::new(Withdrawals::default()),
        );
        witness.state.set(Arc::downgrade(&session.state)).unwrap();

        lock_protected_session(Arc::clone(&session.state), lock).await;

        let seen = witness.seen.lock().unwrap();
        let (_, _, locked) = seen
            .iter()
            .find(|(event, payload, _)| event == "system_status" && payload["status"] == "stopping")
            .expect("no teardown");
        assert_eq!(locked.as_deref(), Some(session.hash.as_str()));
    }

    // The network stopping leaves the identity active, and it still locks.
    #[tokio::test]
    async fn an_auto_lock_locks_an_active_identity_whose_network_stopped() {
        let _hosts = crate::hosted_propagation::get_tests::GET_TEST_LOCK
            .lock()
            .await;
        let recorder = Arc::new(Recorder::default());
        let (session, lock) = armed_session(
            "passcode",
            recorder.clone(),
            Arc::new(Withdrawals::default()),
        );
        let state = &session.state;
        shutdown_rns_lxmf(state).await.unwrap();
        assert!(state.lxmf.lock().unwrap().is_none());

        lock_protected_session(Arc::clone(state), lock).await;

        assert_eq!(
            state.hw_locked_hash().as_deref(),
            Some(session.hash.as_str())
        );
        assert!(
            state
                .network_session
                .read()
                .unwrap()
                .retained_identity
                .is_none(),
            "the unlock outlived the lock"
        );
        assert_eq!(last_lock_event(&recorder)["reason"], "timeout");
    }

    // An identity that lost its passcode before its lock fell due stays open,
    // and its lock goes.
    #[tokio::test]
    async fn an_auto_lock_never_locks_an_identity_without_a_passcode_or_key() {
        let (session, lock) = armed_session(
            "plain",
            Arc::new(Recorder::default()),
            Arc::new(Withdrawals::default()),
        );
        let state = &session.state;

        lock_protected_session(Arc::clone(state), lock).await;

        assert!(state.hw_locked_hash().is_none());
        assert!(state.lxmf.lock().unwrap().is_some(), "its session ended");
        assert!(
            state.auto_lock.lock().unwrap().is_none(),
            "its lock stayed armed"
        );
    }

    // The deadline runs from the unlock on the suspend-counting clock, so time
    // the system slept counts toward it.
    #[test]
    fn an_auto_lock_falls_due_once_its_timeout_has_passed_since_the_unlock() {
        let unlocked_at = ContinuousInstant::now();
        let lock = |timeout: u64| state::AutoLock {
            hash: "ab".repeat(16),
            kind: "passcode",
            unlocked_at,
            timeout: Duration::from_secs(timeout),
            generation: 0,
        };
        let quarter_hour = lock(15 * 60);
        assert!(!quarter_hour.due_at(unlocked_at + Duration::from_secs(15 * 60 - 1)));
        assert!(quarter_hour.due_at(unlocked_at + Duration::from_secs(15 * 60)));
        // A lid closed for eight hours.
        assert!(quarter_hour.due_at(unlocked_at + Duration::from_secs(8 * 3600)));
        assert!(!quarter_hour.due_at(unlocked_at.checked_sub(Duration::from_secs(1)).unwrap()));
        // Off.
        assert!(!lock(0).due_at(unlocked_at + Duration::from_secs(8 * 3600)));
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
        crate::lxmf::initialize_storage_namespace(&root).unwrap();
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

    // A Backbone client reaches the same public servers a TCP client does.
    #[test]
    fn startup_transport_auto_counts_a_public_server_saved_as_backbone() {
        for (ruby_type, emerald_type, emerald_host, transport) in [
            (
                "TCPClientInterface",
                "BackboneInterface",
                "2.ratspeak.org",
                false,
            ),
            (
                "BackboneInterface",
                "BackboneInterface",
                "rns.ratspeak.org",
                false,
            ),
            // One server saved both ways is one server.
            (
                "TCPClientInterface",
                "BackboneInterface",
                "1.ratspeak.org:4141",
                true,
            ),
        ] {
            let root = temp_root("public-limit-backbone");
            let state = state_for_root(root.clone());
            let config_dir = state.config.rns_config_dir.clone();
            let (host, port) = emerald_host
                .split_once(':')
                .unwrap_or((emerald_host, "4242"));
            rns_config::write_config(
                &config_dir,
                &format!(
                    "[reticulum]\nenable_transport = True\n\n[interfaces]\n\
                     [[Ruby]]\n\
                     type = {ruby_type}\n\
                     enabled = true\n\
                     target_host = 1.ratspeak.org\n\
                     target_port = 4141\n\
                     [[Second]]\n\
                     type = {emerald_type}\n\
                     enabled = true\n\
                     target_host = {host}\n\
                     target_port = {port}\n"
                ),
            );
            db::set_setting(&state.db, "transport_mode", "auto");
            db::set_setting(&state.db, "transport_network_type", "wifi");

            reconcile_persisted_transport_mode_for_startup(&state, &config_dir);

            assert_eq!(
                rns_config::transport_mode_enabled(&config_dir),
                transport,
                "{ruby_type} + {emerald_type} to {emerald_host}"
            );
            std::fs::remove_dir_all(root).ok();
        }
    }

    #[test]
    fn public_servers_include_community_presets_on_their_own_ports() {
        assert_eq!(
            public_tcp_server_id("RNS.MichMesh.net", 7822),
            Some("michmesh")
        );
        assert_eq!(public_tcp_server_id("rns.michmesh.net", 4242), None);
        let count =
            |ifaces: &Value| projected_enabled_public_tcp_server_ids(ifaces, None, None).len();
        let ifaces = serde_json::json!({
            "tcp_client": [
                { "name": "UTN", "target_host": "rns.utn.lol", "target_port": "4965", "enabled": true },
                { "name": "Washmesh", "target_host": "reticulum.washmesh.net", "target_port": "7242", "enabled": true }
            ]
        });
        assert_eq!(count(&ifaces), 2);
    }

    #[test]
    fn a_public_server_counts_once_however_it_is_saved() {
        let ids = projected_enabled_public_tcp_server_ids;
        let ifaces = serde_json::json!({
            "tcp_client": [
                { "name": "Ruby", "target_host": "1.ratspeak.org", "target_port": "4141", "enabled": true },
                { "name": "Emerald", "target_host": "2.ratspeak.org", "target_port": "4242", "enabled": true }
            ],
            "backbone_client": [
                { "name": "Backbone to Emerald", "target_host": "RNS.Ratspeak.org.", "target_port": "4242", "enabled": true },
                { "name": "Backbone to RMAP", "target_host": "rmap.world", "target_port": "4242", "enabled": true },
                { "name": "Paused", "target_host": "rns.utn.lol", "target_port": "4965", "enabled": false },
                { "name": "Private", "target_host": "hub.example", "target_port": "4242", "enabled": true }
            ],
            // A server listens; it connects to nothing.
            "backbone_server": [
                { "name": "Listener", "target_host": "3.ratspeak.org", "target_port": "4343", "enabled": true }
            ]
        });
        assert_eq!(
            ids(&ifaces, None, None),
            ["ratspeak-ruby", "ratspeak-emerald", "rmap"]
        );
        assert_eq!(
            ids(&ifaces, Some("Backbone to RMAP"), None),
            ["ratspeak-ruby", "ratspeak-emerald"],
            "an entry being replaced is left out, whichever type it is"
        );
        assert_eq!(
            ids(&ifaces, Some("Emerald"), None),
            ["ratspeak-ruby", "ratspeak-emerald", "rmap"],
            "its Backbone copy still reaches Emerald"
        );
        assert_eq!(
            ids(&ifaces, Some("Backbone to RMAP"), Some("ratspeak-emerald")),
            ["ratspeak-ruby", "ratspeak-emerald"]
        );
        assert_eq!(
            ids(&ifaces, None, Some("washmesh")),
            ["ratspeak-ruby", "ratspeak-emerald", "rmap", "washmesh"]
        );

        let backbone_only = serde_json::json!({
            "backbone_client": [
                { "name": "A", "target_host": "1.ratspeak.org", "target_port": "4141", "enabled": true },
                { "name": "B", "target_host": "3.ratspeak.org", "target_port": "4343", "enabled": true }
            ]
        });
        assert_eq!(ids(&backbone_only, None, None).len(), 2);
        assert_eq!(
            public_tcp_server_id_from_entry(&backbone_only["backbone_client"][1]),
            Some("ratspeak-diamond")
        );
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
mod activity_default_tests {
    use super::*;
    use activity::ActivityCaptureState;

    fn state() -> Arc<AppState> {
        let tmp = tempfile::tempdir().unwrap().keep();
        lxmf::initialize_storage_namespace(&tmp).unwrap();
        let pool = r2d2::Pool::builder()
            .max_size(1)
            .build(r2d2_sqlite::SqliteConnectionManager::memory())
            .unwrap();
        db::init_schema(&pool).unwrap();
        Arc::new(AppState::new(
            ratspeak_core::config::DashboardConfig::from_env_and_defaults(tmp),
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activity_runs_by_default_and_again_after_a_runtime_reset() {
        let state = state();
        start_activity_unless_stopped(&state).await;
        assert_eq!(
            state.activity.status().state(),
            ActivityCaptureState::Capturing
        );
        let profile = state.activity.status().profile();
        assert!(
            matches!(profile, Some(activity::CaptureProfile::Normal)),
            "the default is Normal, never Trace"
        );

        let _session_ended = reset_activity_for_shutdown(&state).await.unwrap();
        assert_eq!(state.activity.status().state(), ActivityCaptureState::Off);
        start_activity_unless_stopped(&state).await;
        assert_eq!(
            state.activity.status().state(),
            ActivityCaptureState::Capturing
        );
        state.activity.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stopped_activity_log_stays_stopped() {
        let state = state();
        db::set_setting(&state.db, ACTIVITY_CAPTURE_SETTING, "off");
        start_activity_unless_stopped(&state).await;
        assert_eq!(state.activity.status().state(), ActivityCaptureState::Off);
        state.activity.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod outbox_budget_tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn state_and_manager() -> (Arc<AppState>, lxmf::LxmfManager) {
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-outbox-budget-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        lxmf::initialize_storage_namespace(&tmp).unwrap();
        let pool = r2d2::Pool::builder()
            .max_size(2)
            .build(SqliteConnectionManager::memory())
            .unwrap();
        db::init_schema(&pool).unwrap();
        let config = ratspeak_core::config::DashboardConfig::from_env_and_defaults(tmp.clone());
        let state = Arc::new(AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        let mgr = lxmf::LxmfManager::load_or_create(&tmp, None, None).unwrap();
        (state, mgr)
    }

    fn send_large(state: &AppState, mgr: &mut lxmf::LxmfManager, seed: u8) -> String {
        let bytes = (0..1_200_000u32)
            .map(|i| (i.wrapping_mul(131) as u8) ^ seed)
            .collect::<Vec<_>>();
        let identity_id = mgr.identity_hash.clone();
        mgr.send_message_with_attachment_fields_preference_report(lxmf::AttachmentMessageRequest {
            dest_hash_hex: &"d1".repeat(16),
            content: "",
            title: "",
            file_name: "video.mp4",
            file_bytes: bytes,
            staged_path: None,
            is_image: false,
            image_mime: "",
            db_pool: &state.db,
            identity_id: &identity_id,
            preference: lxmf::DeliveryPreference::Direct,
            renderer: None,
            thread: "",
        })
        .unwrap()
        .message_id
    }

    #[test]
    fn large_resumed_attachments_load_one_at_a_time() {
        let (state, mut mgr) = state_and_manager();
        let first = send_large(&state, &mut mgr, 1);
        let second = send_large(&state, &mut mgr, 2);
        let root = mgr.data_dir.parent().unwrap().to_path_buf();
        let identity_id = mgr.identity_hash.clone();
        drop(mgr);
        let mut mgr = lxmf::LxmfManager::load_or_create(&root, Some(&identity_id), None).unwrap();
        let rows = db::outbox_rows(&state.db, &identity_id).unwrap();
        assert_eq!(mgr.resume_outbox(rows, &state.db), 0);
        assert_eq!(mgr.resumes_awaiting_budget(), 2);

        admit_resumes_within_budget(&state, &mut mgr);
        assert_eq!(
            mgr.resumes_awaiting_budget(),
            1,
            "the large lane loads one file"
        );
        admit_resumes_within_budget(&state, &mut mgr);
        assert_eq!(
            mgr.resumes_awaiting_budget(),
            1,
            "the next waits for that one to settle"
        );

        assert!(state.release_attachment_delivery_lease(&first));
        admit_resumes_within_budget(&state, &mut mgr);
        assert_eq!(mgr.resumes_awaiting_budget(), 0);
        assert!(
            state.release_attachment_delivery_lease(&second),
            "it holds its own lease"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreadable_outbox_record_fails_with_an_event() {
        let (state, mut mgr) = state_and_manager();
        let identity_id = mgr.identity_hash.clone();
        let dest = "d2".repeat(16);
        let msg_id = mgr
            .send_message_with_preference_report(lxmf::MessageSendRequest {
                dest_hash_hex: &dest,
                content: "its record is damaged",
                title: "",
                db_pool: &state.db,
                identity_id: &identity_id,
                preference: lxmf::DeliveryPreference::Direct,
                profile: lxmf::DeliveryProfile::Message,
                renderer: None,
                thread: "",
            })
            .unwrap()
            .message_id;
        state
            .db
            .get()
            .unwrap()
            .execute(
                "UPDATE lxmf_outbox SET message = x'00' WHERE message_id = ?1",
                rusqlite::params![msg_id],
            )
            .unwrap();
        let root = mgr.data_dir.parent().unwrap().to_path_buf();
        drop(mgr);
        let mgr = lxmf::LxmfManager::load_or_create(&root, Some(&identity_id), None).unwrap();

        let mut mgr = resume_outbox(&state, mgr).await;
        assert_eq!(
            db::get_conversation(&state.db, &dest, &identity_id, 10)[0]["state"],
            "sending",
            "the resume writes nothing; the tick's step is the event"
        );
        assert_eq!(mgr.tick(), [(msg_id.clone(), "failed")]);
        assert_eq!(
            mgr.take_failure_reasons()
                .get(&msg_id)
                .map(|reason| reason.code()),
            Some("preparation_failed")
        );
    }

    // Earlier builds kept a message to one's own address on its way and sent it again at every start.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_message_to_ones_own_address_left_unsettled_is_delivered_at_start() {
        let (state, mut mgr) = state_and_manager();
        let identity_id = mgr.identity_hash.clone();
        let me = mgr.lxmf_hash.clone();
        let peer = "d3".repeat(16);
        let mut send = |to: &str, text: &str| {
            mgr.send_message_with_preference_report(lxmf::MessageSendRequest {
                dest_hash_hex: to,
                content: text,
                title: "",
                db_pool: &state.db,
                identity_id: &identity_id,
                preference: lxmf::DeliveryPreference::Direct,
                profile: lxmf::DeliveryProfile::Message,
                renderer: None,
                thread: "",
            })
            .unwrap()
            .message_id
        };
        // As they were left: rewritten by the copy that came back, waiting, on its way, ended.
        let left = ["received", "waiting", "routing", "failed"];
        let to_self = left.map(|was| send(&me, was));
        let to_peer = send(&peer, "to someone else");
        for (id, was) in to_self.iter().zip(left) {
            state
                .db
                .get()
                .unwrap()
                .execute(
                    "UPDATE messages SET state = ?1 WHERE id = ?2",
                    rusqlite::params![was, id],
                )
                .unwrap();
        }
        let root = mgr.data_dir.parent().unwrap().to_path_buf();
        drop(mgr);
        let mgr = lxmf::LxmfManager::load_or_create(&root, Some(&identity_id), None).unwrap();

        let mut mgr = resume_outbox(&state, mgr).await;
        let stored = |id: &str| {
            db::outbound_message_state(&state.db, &identity_id, id)
                .unwrap()
                .unwrap()
        };
        assert_eq!(
            to_self.each_ref().map(|id| stored(id)),
            ["delivered", "delivered", "delivered", "failed"],
            "what ended stays as the user saw it"
        );
        assert!(!db::has_waiting(&state.db, &identity_id));
        let resumed = db::outbox_rows(&state.db, &identity_id).unwrap();
        assert_eq!(
            resumed
                .iter()
                .map(|row| &row.message_id)
                .collect::<Vec<_>>(),
            [&to_peer],
            "only the message to someone else is sent again"
        );
        assert_eq!(
            mgr.tick(),
            [(to_peer, "routing")],
            "nothing to its own address reached the queue"
        );
        assert_eq!(
            db::get_conversation(&state.db, &me, &identity_id, 10).len(),
            4
        );
    }
}

#[cfg(test)]
mod waiting_send_tests {
    use super::inbound_pipeline_tests::{RecordingEmitter, pipeline_state};
    use super::*;

    const PEER: &str = "d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d3";

    /// A running identity, and a way to queue a message from it.
    fn session() -> (Arc<AppState>, Arc<RecordingEmitter>, String) {
        let (state, emitter) = pipeline_state();
        let identity = {
            let lxmf = state.lxmf.lock().unwrap();
            let manager = lxmf.as_ref().unwrap();
            db::save_identity(
                &state.db,
                &manager.identity_hash,
                &manager.lxmf_hash,
                "A",
                "A",
            );
            manager.identity_hash.clone()
        };
        db::set_active_identity(&state.db, &identity).unwrap();
        (state, emitter, identity)
    }

    fn send(state: &AppState, identity: &str, content: &str) -> String {
        state
            .lxmf
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .send_message_with_preference_report(lxmf::MessageSendRequest {
                dest_hash_hex: PEER,
                content,
                title: "",
                db_pool: &state.db,
                identity_id: identity,
                preference: lxmf::DeliveryPreference::Direct,
                profile: lxmf::DeliveryProfile::Message,
                renderer: None,
                thread: "",
            })
            .unwrap()
            .message_id
    }

    fn stored_state(state: &AppState, identity: &str, msg_id: &str) -> String {
        db::get_conversation(&state.db, PEER, identity, 50)
            .into_iter()
            .find(|message| message["id"] == msg_id)
            .expect("the message has a row")["state"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn set_stored(state: &AppState, msg_id: &str, sql: &str) {
        state
            .db
            .get()
            .unwrap()
            .execute(sql, rusqlite::params![msg_id])
            .unwrap();
    }

    /// Leave `msg_id` with nothing holding it, three minutes after its first step.
    fn orphan(state: &AppState, msg_id: &str) {
        assert!(
            state
                .lxmf
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .cancel_outbound_message(msg_id)
        );
        let long_ago = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            - MESSAGE_TIMEOUT_SECS
            - 1.0;
        state
            .message_send_times
            .lock()
            .unwrap()
            .insert(msg_id.to_owned(), long_ago);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_orphan_waits_like_any_network_failure() {
        let (state, emitter, identity) = session();
        let fence = state.activity_request_fence();

        // With waiting off it ends as it always has.
        state.set_lxmf_keep_trying_enabled(false);
        let ended = send(&state, &identity, "ends");
        orphan(&state, &ended);
        check_message_timeouts(&state, fence).await;
        assert_eq!(stored_state(&state, &identity, &ended), "timeout");

        state.lxmf_keep_trying.store(true, Ordering::Relaxed);
        let parked = send(&state, &identity, "waits");
        orphan(&state, &parked);
        check_message_timeouts(&state, fence).await;
        assert_eq!(stored_state(&state, &identity, &parked), "waiting");
        assert!(state.message_send_times.lock().unwrap().is_empty());

        let steps = emitter.payloads("lxmf_step");
        let step = |msg_id: &str| {
            steps
                .iter()
                .filter(|step| step["msg_id"] == msg_id)
                .map(|step| {
                    (
                        step["step"].as_str().unwrap().to_owned(),
                        step["reason"].as_str().unwrap().to_owned(),
                        step["destination"].as_str().unwrap().to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            step(&ended),
            [("timeout".to_owned(), "timeout".to_owned(), PEER.to_owned())]
        );
        assert_eq!(
            step(&parked),
            [(
                "waiting".to_owned(),
                "proof_timed_out".to_owned(),
                PEER.to_owned()
            )]
        );

        // Examined again, a waiting message stays as it is and says nothing.
        orphan_clock_only(&state, &parked);
        check_message_timeouts(&state, fence).await;
        assert_eq!(stored_state(&state, &identity, &parked), "waiting");
        assert_eq!(emitter.payloads("lxmf_step").len(), steps.len());
    }

    fn orphan_clock_only(state: &AppState, msg_id: &str) {
        state
            .message_send_times
            .lock()
            .unwrap()
            .insert(msg_id.to_owned(), 0.0);
    }

    fn peer_hash() -> [u8; 16] {
        hex::decode(PEER).unwrap().try_into().unwrap()
    }

    /// One pass of the engine as the tick makes it: under the lock, then told.
    fn wake(state: &Arc<AppState>) -> lxmf::WakeOutcome {
        let outcome = {
            let mut lxmf = state.lxmf.lock().unwrap();
            run_wakes(state, lxmf.as_mut().unwrap())
        };
        apply_wake_outcome(state, state.activity_request_fence(), &outcome);
        outcome
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wake_tells_the_dashboard_and_starts_the_clock() {
        let (state, emitter, identity) = session();
        state.lxmf_keep_trying.store(true, Ordering::Relaxed);
        let id = send(&state, &identity, "tried again");
        orphan(&state, &id);
        check_message_timeouts(&state, state.activity_request_fence()).await;
        assert_eq!(stored_state(&state, &identity, &id), "waiting");
        assert!(state.message_send_times.lock().unwrap().is_empty());
        assert!(wake(&state).woken.is_empty(), "nothing asked for it yet");

        // A signal raised anywhere reaches the engine at the next tick, once.
        state.wake_waiting(lxmf::WakeEvent::TryNow(peer_hash()));
        let outcome = wake(&state);
        assert_eq!(outcome.woken.len(), 1);
        assert_eq!(stored_state(&state, &identity, &id), "sending");
        assert!(
            state.message_send_times.lock().unwrap().contains_key(&id),
            "the watchdog can end a woken send that goes nowhere"
        );
        let step = emitter
            .payloads("lxmf_step")
            .into_iter()
            .find(|step| step["step"] == "sending")
            .expect("the new attempt is announced");
        assert_eq!(step["msg_id"], id.as_str());
        assert_eq!(step["wake"], "wake_user");
        assert_eq!(step["method"], "direct");
        assert_eq!(state.take_wake_signals(), lxmf::WakeSignals::default());
        assert!(wake(&state).woken.is_empty(), "one signal, one wake");
    }

    #[test]
    fn a_proof_of_delivery_wakes_what_else_waits_for_that_recipient() {
        let (state, _emitter, identity) = session();
        let id = send(&state, &identity, "proven");

        state.set_lxmf_keep_trying_enabled(false);
        assert_eq!(
            recipient_of_proof(&state, &state.db, &identity, &id).as_deref(),
            Some(hex::encode(peer_hash()).as_str())
        );
        assert!(state.take_wake_signals().is_empty(), "waiting is off");

        state.lxmf_keep_trying.store(true, Ordering::Relaxed);
        assert_eq!(
            recipient_of_proof(&state, &state.db, &identity, &"ee".repeat(32)),
            None
        );
        assert!(
            state.take_wake_signals().is_empty(),
            "not a message of ours"
        );
        recipient_of_proof(&state, &state.db, &identity, &id);
        let mut heard = lxmf::WakeSignals::default();
        heard.raise(lxmf::WakeEvent::Heard(peer_hash()));
        assert_eq!(state.take_wake_signals(), heard);
    }

    fn manager_waits(state: &AppState) -> bool {
        state.lxmf.lock().unwrap().as_ref().unwrap().keep_trying
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn turning_it_off_ends_waiting_messages_at_once_without_a_notice() {
        let (state, emitter, identity) = session();
        let fence = state.activity_request_fence();
        let on_its_way = send(&state, &identity, "on its way");
        let mine = send(&state, &identity, "waits");
        orphan(&state, &mine);
        check_message_timeouts(&state, fence).await;
        assert_eq!(stored_state(&state, &identity, &mine), "waiting");
        // Another identity's, from when it was the active one.
        let (other, theirs) = ("77".repeat(16), "9a".repeat(32));
        db::save_identity(&state.db, &other, &"78".repeat(16), "B", "B");
        db::save_outbound_message(
            &state.db,
            &db::OutboundMessage {
                msg_id: &theirs,
                identity_id: &other,
                source: "them",
                destination: PEER,
                waiting: true,
                resend: db::Resend {
                    preference: "auto",
                    profile: "message",
                    message: b"parts",
                },
                ..db::OutboundMessage::default()
            },
        )
        .unwrap();
        state.wake_waiting(lxmf::WakeEvent::TryNow(peer_hash()));
        let told = emitter.count("lxmf_step");

        assert_eq!(set_lxmf_keep_trying(&state, false), Ok(2));
        assert!(!state.lxmf_keep_trying_enabled());
        assert!(!manager_waits(&state), "a send made now is not held");
        assert!(state.take_wake_signals().is_empty());
        assert_eq!(stored_state(&state, &identity, &mine), "failed");
        assert_eq!(
            db::outbound_message_state(&state.db, &other, &theirs).unwrap(),
            Some("failed".into()),
            "every identity's, not only the one in use"
        );
        assert_eq!(stored_state(&state, &identity, &on_its_way), "sending");
        assert_eq!(
            emitter.payloads("lxmf_step")[told..],
            [json!({ "step": "failed", "msg_id": mine, "reason": "keep_trying_off" })],
            "one step, for the identity in use, naming no chat to give notice of"
        );
        assert!(
            db::retryable_outbox_row(&state.db, &identity, &mine)
                .unwrap()
                .is_some(),
            "it can be sent again by hand"
        );

        // On again takes effect at once; what ended stays ended.
        assert_eq!(set_lxmf_keep_trying(&state, true), Ok(0));
        assert!(state.lxmf_keep_trying_enabled());
        assert!(manager_waits(&state));
        assert!(wake(&state).woken.is_empty());
        assert_eq!(stored_state(&state, &identity, &mine), "failed");
        orphan(&state, &on_its_way);
        check_message_timeouts(&state, fence).await;
        assert_eq!(stored_state(&state, &identity, &on_its_way), "waiting");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_update_from_1_0_44_starts_with_waiting_on_and_leaves_ended_messages_ended() {
        let (old, _emitter, identity) = session();
        let ended = ["failed", "timeout", "cancelled"].map(|state| {
            let id = send(&old, &identity, state);
            orphan(&old, &id);
            set_stored(
                &old,
                &id,
                &format!("UPDATE messages SET state = '{state}' WHERE id = ?1"),
            );
            (id, state)
        });
        let on_its_way = send(&old, &identity, "on its way");
        // As 1.0.44 left it: the schema before waiting, sent long ago, no choice stored.
        old.db
            .get()
            .unwrap()
            .execute_batch(
                "UPDATE lxmf_outbox SET queued_at = queued_at - 200000.0;
                 ALTER TABLE lxmf_outbox DROP COLUMN auto_sends;
                 ALTER TABLE lxmf_outbox DROP COLUMN last_auto_at;
                 UPDATE schema_version SET version = 51;",
            )
            .unwrap();
        assert_eq!(db::get_setting(&old.db, "lxmf_keep_trying"), None);

        db::init_schema(&old.db).unwrap();
        let state = Arc::new(AppState::new(
            old.config.clone(),
            old.db.clone(),
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        *state.lxmf.lock().unwrap() = old.lxmf.lock().unwrap().take();
        *state.last_stats.write().unwrap() =
            Some(json!({ "interface_stats": { "interfaces": [{ "online": true }] } }));
        assert_eq!(any_interface_online_cached(&state), Some(true));
        assert!(state.lxmf_keep_trying_enabled(), "on without being asked");

        // The start: nothing woken by an earlier run, then the engine's first pass.
        assert!(db::return_woken_to_waiting(&state.db, &identity).is_empty());
        let outcome = wake(&state);
        assert!(outcome.woken.is_empty() && outcome.expired.is_empty());
        assert!(outcome.failed.is_empty());
        assert!(manager_waits(&state));
        for (id, was) in &ended {
            assert_eq!(stored_state(&state, &identity, id), *was);
            assert!(
                db::retryable_outbox_row(&state.db, &identity, id)
                    .unwrap()
                    .is_some(),
                "{was} keeps Retry"
            );
        }
        assert_eq!(stored_state(&state, &identity, &on_its_way), "sending");
        assert!(db::waiting_queues(&state.db, &identity).unwrap().is_empty());
        assert_eq!(
            db::outbox_rows(&state.db, &identity)
                .unwrap()
                .iter()
                .map(|row| row.message_id.as_str())
                .collect::<Vec<_>>(),
            [on_its_way.as_str()],
            "what was on its way is sent again, as before"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wait_that_ran_out_ends_with_its_notice() {
        let (state, emitter, identity) = session();
        state.lxmf_keep_trying.store(true, Ordering::Relaxed);
        let id = send(&state, &identity, "a day old");
        orphan(&state, &id);
        check_message_timeouts(&state, state.activity_request_fence()).await;
        set_stored(
            &state,
            &id,
            "UPDATE lxmf_outbox SET queued_at = queued_at - 86400.0 WHERE message_id = ?1",
        );

        // With waiting turned off since, the sweep is all that runs.
        state.lxmf_keep_trying.store(false, Ordering::Relaxed);
        let outcome = wake(&state);
        assert!(outcome.woken.is_empty());
        assert_eq!(outcome.expired.len(), 1);
        assert_eq!(stored_state(&state, &identity, &id), "failed");
        let step = emitter
            .payloads("lxmf_step")
            .into_iter()
            .find(|step| step["step"] == "failed")
            .expect("the end of the wait is announced");
        assert_eq!(step["msg_id"], id.as_str());
        assert_eq!(step["reason"], "wait_expired");
        assert_eq!(step["destination"], PEER, "the notice names the chat");
    }

    #[test]
    fn stop_trying_and_a_wake_cannot_both_win() {
        let root = tempfile::tempdir().unwrap().keep();
        lxmf::initialize_storage_namespace(&root).unwrap();
        let pool = db::init_pool(&root).unwrap();
        db::init_schema(&pool).unwrap();
        let state = Arc::new(AppState::new(
            ratspeak_core::config::DashboardConfig::from_env_and_defaults(root.clone()),
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        let mgr = lxmf::LxmfManager::load_or_create(&root, None, None).unwrap();
        let identity = mgr.identity_hash.clone();
        *state.lxmf.lock().unwrap() = Some(mgr);
        state.lxmf_keep_trying.store(true, Ordering::Relaxed);
        let park = |content: &str| {
            let id = send(&state, &identity, content);
            let mut lxmf = state.lxmf.lock().unwrap();
            lxmf.as_mut().unwrap().cancel_outbound_message(&id);
            set_stored(
                &state,
                &id,
                "UPDATE messages SET state = 'waiting' WHERE id = ?1",
            );
            // As the tick says of a message it leaves waiting.
            state.note_waiting_row();
            id
        };
        let held = |id: &str| state.lxmf.lock().unwrap().as_ref().unwrap().still_owned(id);
        assert!(wake(&state).woken.is_empty());

        // Stopped first: it only waited, and a wake finds nothing to send.
        let stopped_first = park("stopped first");
        let stopped = stop_outbound_message(&state, &stopped_first, &identity);
        assert!(stopped.only_waiting && stopped.row_marked_stopped && !stopped.live_owner_stopped);
        state.wake_waiting(lxmf::WakeEvent::TryNow(peer_hash()));
        assert!(wake(&state).woken.is_empty());
        assert_eq!(stored_state(&state, &identity, &stopped_first), "cancelled");
        assert!(!held(&stopped_first));

        // Woken first: the stop takes the attempt back out of the manager.
        let woken_first = park("woken first");
        state.wake_waiting(lxmf::WakeEvent::TryNow(peer_hash()));
        assert_eq!(wake(&state).woken.len(), 1);
        assert!(held(&woken_first));
        let stopped = stop_outbound_message(&state, &woken_first, &identity);
        assert!(stopped.live_owner_stopped && stopped.row_marked_stopped && !stopped.only_waiting);
        assert_eq!(stored_state(&state, &identity, &woken_first), "cancelled");
        assert!(!held(&woken_first), "nothing of it is sent");

        // The stop writes its row under the manager lock. With the database
        // held, a stop that let go of the manager cannot have finished: the
        // manager must stay locked for as long as the row is unwritten.
        let racing = park("racing");
        state.wake_waiting(lxmf::WakeEvent::TryNow(peer_hash()));
        assert_eq!(wake(&state).woken.len(), 1);
        let writer = state.db.get().unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let stopper = {
            let (state, id, identity) = (Arc::clone(&state), racing.clone(), identity.clone());
            std::thread::spawn(move || stop_outbound_message(&state, &id, &identity))
        };
        let mut locked_for = 0;
        while locked_for < 20 {
            match state.lxmf.try_lock() {
                Err(_) => locked_for += 1,
                Ok(lxmf) => {
                    assert!(
                        lxmf.as_ref().unwrap().still_owned(&racing),
                        "the manager let go of the message and its row is not written yet: a wake could send it"
                    );
                    locked_for = 0;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        writer.execute_batch("COMMIT").unwrap();
        drop(writer);
        let stopped = stopper.join().unwrap();
        assert!(stopped.live_owner_stopped && stopped.row_marked_stopped);
        assert_eq!(stored_state(&state, &identity, &racing), "cancelled");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_resume_leaves_waiting_and_woken_sends_to_a_wake() {
        let (state, emitter, identity) = session();
        let parked = send(&state, &identity, "parked");
        let woken = send(&state, &identity, "woken");
        let fresh = send(&state, &identity, "fresh");
        set_stored(
            &state,
            &parked,
            "UPDATE messages SET state = 'waiting' WHERE id = ?1",
        );
        set_stored(
            &state,
            &woken,
            "UPDATE lxmf_outbox SET auto_sends = 1 WHERE message_id = ?1",
        );

        let mgr = state.lxmf.lock().unwrap().take().unwrap();
        let root = mgr.data_dir.parent().unwrap().to_path_buf();
        drop(mgr);
        let mgr = lxmf::LxmfManager::load_or_create(&root, Some(&identity), None).unwrap();
        let mut mgr = resume_outbox(&state, mgr).await;

        assert!(
            state.take_waiting_written(),
            "the start tells the engine that messages wait"
        );
        assert_eq!(stored_state(&state, &identity, &parked), "waiting");
        assert_eq!(stored_state(&state, &identity, &woken), "waiting");
        assert_eq!(stored_state(&state, &identity, &fresh), "sending");
        assert_eq!(
            emitter.payloads("lxmf_step"),
            [json!({ "step": "waiting", "msg_id": woken })],
            "the one whose state changed says so"
        );
        // Only what the resume took can be withdrawn from the manager.
        assert!(!mgr.cancel_outbound_message(&parked));
        assert!(!mgr.cancel_outbound_message(&woken));
        assert!(mgr.cancel_outbound_message(&fresh));
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
        fn withdraw_all(&self) {}
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

    fn announce_state() -> Arc<AppState> {
        let state = Arc::new(make_state(Arc::new(RecordingNotifier::default())));
        let unique = TEMP_NOTIFICATION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-announce-fence-test-{}-{unique}",
            std::process::id()
        ));
        crate::lxmf::initialize_storage_namespace(&tmp).unwrap();
        *state.lxmf.lock().unwrap() =
            Some(crate::lxmf::LxmfManager::load_or_create(&tmp, None, None).unwrap());
        state
    }

    // Setup makes the stamp as the identity is created; its first network start
    // loads that one and has nothing left to make.
    #[tokio::test]
    async fn a_new_identitys_stamp_is_made_at_creation_and_loaded_by_its_start() {
        let _stamps = crate::announce_stamps::test_policy::unstamped().await;
        let state = Arc::new(make_state(Arc::new(RecordingNotifier::default())));
        let identity = rns_identity::identity::Identity::new();
        let key = identity.get_private_key().unwrap();
        let hash = hex::encode(identity.hash);
        // A low value keeps the search short in a debug build.
        let dir = rns_config_dir_for_start(&state, Some(&hash));
        std::fs::write(
            dir.join("config"),
            "[stamps]\n  destination_value = 8\n  destination_cost = 6\n",
        )
        .unwrap();

        start_announce_stamp_for_new_identity(&state, &hash, &key[..]);
        assert_eq!(state.announce_stamp_status()["state"], "working");
        let deadline = Instant::now() + Duration::from_secs(20);
        while state.announce_stamp_status()["state"] != "ready" {
            assert!(Instant::now() < deadline, "the stamp was made");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let mut delivery = rns_identity::destination::Destination::new(
            Some(&identity),
            rns_identity::destination::Direction::In,
            rns_identity::destination::DestType::Single,
            LXMF_DELIVERY_APP_NAME,
        )
        .unwrap();
        rns_runtime::destination_stamper::load_stored_stamp(&mut delivery, &identity);
        assert!(
            delivery.destination_stamp().is_some(),
            "saved where the start loads it"
        );
        assert!(rns_runtime::destination_stamper::prepare(&delivery, &identity).is_none());
        crate::announce_stamps::test_policy::unstamp();
    }

    fn manual_leadership(state: &AppState) -> crate::announce::AnnounceLeadership {
        crate::announce::AnnounceLeadership {
            correlation_id: 1,
            activity_correlation_id: crate::activity::CorrelationId::random(),
            revisions: state.announce_semantic_revision(),
            origins: vec![crate::announce::AnnounceOrigin::Manual],
        }
    }

    // A burst past its fence fails later here for want of a transport, and
    // records its result; a stale one is suppressed without a record.
    #[tokio::test]
    async fn only_a_transition_suppresses_an_announce_and_sending_suppresses_none() {
        let _stamps = crate::announce_stamps::test_policy::unstamped().await;
        let state = announce_state();
        let origin = state.activity_request_fence();
        drop(crate::propagation_recovery::begin_download_deletion(Arc::clone(&state)).await);
        let bystander = state.activity_request_fence();
        let burst = execute_announce_burst(&state, false, origin, &manual_leadership(&state)).await;
        assert!(
            burst.activity_recorded,
            "routine work suppressed the announce"
        );
        assert!(
            state.is_current_activity_origin_fence(bystander),
            "sending the announce invalidated another request"
        );

        let state = announce_state();
        let origin = state.activity_request_fence();
        drop(state.identity_switch_lock.lock().await);
        let burst = execute_announce_burst(&state, false, origin, &manual_leadership(&state)).await;
        assert!(
            !burst.activity_recorded,
            "a transition must suppress the announce"
        );
    }

    // A burst that really sends: its dispatch waits outside the lifecycle
    // lock, so routine work goes on meanwhile, and it invalidates no request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_sending_burst_waits_for_dispatch_outside_the_lock_and_invalidates_nothing() {
        let _stamps = crate::announce_stamps::test_policy::unstamped().await;
        use rns_transport::messages::{OutboundDispatchResult, TransportMessage};
        let state = announce_state();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("config"),
            "[reticulum]\nshare_instance = No\nenable_transport = No\n[interfaces]\n",
        )
        .unwrap();
        let mut network = crate::rns::RnsManager::init(
            root.path().to_str().unwrap(),
            Some(root.path().join("cache")),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
        .await
        .unwrap();
        crate::announce_stamps::test_policy::unstamp();
        let (tx, mut packets) = tokio::sync::mpsc::channel(4);
        network.handle.transport_tx = tx;
        let network_shutdown = network.handle.shutdown.clone();
        *state.rns.write().unwrap() = Some(network);

        let origin = state.activity_request_fence();
        let bystander = state.activity_request_fence();
        let burst = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                execute_announce_burst(&state, false, origin, &manual_leadership(&state)).await
            }
        });
        let Some(TransportMessage::SendPacket { result_tx, .. }) = packets.recv().await else {
            panic!("the burst sent nothing");
        };
        assert!(
            tokio::time::timeout(
                Duration::from_secs(1),
                state.identity_switch_lock.lock_routine()
            )
            .await
            .is_ok(),
            "the burst held the lifecycle lock across a pending dispatch"
        );
        let _ = result_tx.send(OutboundDispatchResult::Sent);
        let answer = tokio::spawn(async move {
            while let Some(message) = packets.recv().await {
                if let TransportMessage::SendPacket { result_tx, .. } = message {
                    let _ = result_tx.send(OutboundDispatchResult::Sent);
                }
            }
        });
        let burst = burst.await.unwrap();
        assert!(burst.report.lxmf_delivery_queued, "the burst did not send");
        assert!(
            state.is_current_activity_origin_fence(bystander),
            "sending the announce invalidated another request"
        );
        network_shutdown.trigger();
        state.rns.write().unwrap().take();
        answer.abort();
    }

    // Python's router retains every delivered or propagated recipient.
    #[tokio::test]
    async fn delivered_and_propagated_recipients_are_retained_in_reticulum() {
        let _stamps = crate::announce_stamps::test_policy::unstamped().await;
        use lxmf_core::message::LxMessageDraft;
        use lxmf_core::message_api::DeliveryMethod;
        use rns_transport::messages::{TransportMessage, TransportQuery, TransportQueryResponse};
        let state = announce_state();
        let (delivered, propagated) = ([0xD1; 16], [0xD2; 16]);
        {
            let mut lxmf = state.lxmf.lock().unwrap();
            let mgr = lxmf.as_mut().unwrap();
            let key = mgr.identity.get_signing_key().unwrap();
            let source = mgr.lxmf_dest_hash;
            let message = |dest| {
                LxMessageDraft::new(dest, source, "", "hi", DeliveryMethod::Direct)
                    .unwrap()
                    .sign(&key)
                    .unwrap()
                    .finalize(None)
                    .unwrap()
            };
            mgr.router.complete_outbound_message(message(delivered));
            let deposited = message(propagated);
            let id = deposited.message_id();
            mgr.router.try_send(deposited).unwrap();
            assert!(mgr.router.mark_outbound_propagated(&id));
        }

        // Without a runtime the retains stay queued for the next one.
        forward_destination_retains(&state).await;

        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("config"),
            "[reticulum]\nshare_instance = No\nenable_transport = No\n[interfaces]\n",
        )
        .unwrap();
        let mut network = crate::rns::RnsManager::init(
            root.path().to_str().unwrap(),
            Some(root.path().join("cache")),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
        .await
        .unwrap();
        crate::announce_stamps::test_policy::unstamp();
        let (tx, mut queries) = tokio::sync::mpsc::channel(4);
        network.handle.transport_tx = tx;
        let network_shutdown = network.handle.shutdown.clone();
        *state.rns.write().unwrap() = Some(network);
        let retained = Arc::new(Mutex::new(Vec::new()));
        let answer = tokio::spawn({
            let retained = Arc::clone(&retained);
            async move {
                while let Some(message) = queries.recv().await {
                    if let TransportMessage::Rpc {
                        query: TransportQuery::RetainDestination { dest },
                        response_tx,
                    } = message
                    {
                        retained.lock().unwrap().push(dest);
                        let _ = response_tx.send(TransportQueryResponse::BoolResult(true));
                    }
                }
            }
        });

        forward_destination_retains(&state).await;
        forward_destination_retains(&state).await;

        assert_eq!(*retained.lock().unwrap(), vec![delivered, propagated]);
        network_shutdown.trigger();
        state.rns.write().unwrap().take();
        answer.abort();
    }

    #[test]
    fn attachment_notifications_hide_wire_fallback_without_legacy_media_inference() {
        assert_eq!(
            notification_body("[File: field-notes.pdf]", None, true),
            "New attachment"
        );
        assert_eq!(
            notification_body("Please review\n[File: field-notes.pdf]", None, true),
            "Please review"
        );
    }

    #[test]
    fn markdown_notifications_are_plain_text_only_for_the_markdown_renderer() {
        let body = "## Plan\n**ship** it, see [notes](https://x.org) ||twist||";
        assert_eq!(
            notification_body(body, Some(lxmf_core::constants::RENDERER_MARKDOWN), false),
            "Plan\nship it, see notes [Spoiler]"
        );
        assert_eq!(notification_body(body, None, false), body);
        assert_eq!(
            notification_body(body, Some(lxmf_core::constants::RENDERER_MICRON), false),
            body
        );
        assert_eq!(
            notification_body(
                "```\n```",
                Some(lxmf_core::constants::RENDERER_MARKDOWN),
                true
            ),
            "New attachment"
        );
    }

    #[test]
    fn inbound_message_notifies_only_when_backgrounded_and_enabled() {
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
            &contact_label_from_db(&state.db, "abcd1234abcd1234", "identity-a"),
            "hello from mesh",
            None,
            false,
        );

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
            &contact_label_from_db(&state.db, "abcd1234abcd1234", "identity-a"),
            "foreground",
            None,
            false,
        );
        assert_eq!(notifier.notifications.lock().unwrap().len(), 1);

        state
            .is_foreground
            .store(false, std::sync::atomic::Ordering::Relaxed);
        state.set_notification_foreground(false);
        state.set_native_notifications_enabled(false);
        notify_inbound_message_if_background(
            &state,
            "abcd1234abcd1234",
            &contact_label_from_db(&state.db, "abcd1234abcd1234", "identity-a"),
            "disabled",
            None,
            false,
        );
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

    // What alerts show is one stored setting, applied where every alert
    // passes; the route stays.
    #[test]
    fn every_alert_shows_what_the_stored_preview_allows() {
        for (preview, title, body) in [
            ("name_only", "Message from Alice", "New message"),
            ("neither", "Ratspeak", "New message"),
        ] {
            let notifier = Arc::new(RecordingNotifier::default());
            let unique = TEMP_NOTIFICATION_COUNTER.fetch_add(1, Ordering::Relaxed);
            let config = DashboardConfig::from_env_and_defaults(std::env::temp_dir().join(
                format!("ratspeak-preview-test-{}-{unique}", std::process::id()),
            ));
            let pool = r2d2::Pool::builder()
                .max_size(2)
                .build(SqliteConnectionManager::memory())
                .unwrap();
            db::init_schema(&pool).unwrap();
            db::set_setting(&pool, "notification_preview", preview);
            let state = AppState::new(
                config,
                pool,
                Arc::new(ratspeak_core::NoopEmitter),
                notifier.clone(),
            );
            state.set_notification_foreground(false);

            state.emit_native_notification(ratspeak_core::NativeNotification::message(
                "Message from Alice",
                "meet at noon",
                "lxmf:abcd1234abcd1234",
                42,
            ));

            let shown = notifier.notifications.lock().unwrap().pop().expect(preview);
            assert_eq!(
                (shown.title.as_str(), shown.body.as_str()),
                (title, body),
                "{preview}"
            );
            assert_eq!(shown.thread_id.as_deref(), Some("lxmf:abcd1234abcd1234"));
        }
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

        let alert = GameAlert {
            label: contact_label_from_db(&state.db, "feedfacefeedface", "identity-a"),
            sender_hash: "feedfacefeedface".into(),
            session_id: "session-1".into(),
            app_id: "chess".into(),
            command: "move".into(),
            is_new_session: false,
        };
        notify_game_if_background(&state, &alert);
        notify_game_if_background(&state, &alert);

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
    fn stats_report_the_last_announce_in_seconds_and_null_before_one() {
        let state = make_state(Arc::new(RecordingNotifier::default()));
        assert_eq!(last_announce_at_secs(&state), Value::Null);
        state
            .last_lxmf_delivery_announce_at_ms
            .store(1_700_000_000_500, Ordering::Relaxed);
        assert_eq!(last_announce_at_secs(&state), json!(1_700_000_000.5));
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
