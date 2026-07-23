//! Adapter from the typed Activity batch boundary to Ratspeak's event bus.

use std::sync::Arc;

use ratspeak_core::{EmitError, Emitter};

use super::replay::{ActivityBatchSink, ActivityBatchV1, ActivityPublishError, ActivityStatusV1};
use super::schema::{
    ActivityAttributeKey, ActivityAttributeV1, ActivityEventV1, ActivitySeverity, ActivityValueV1,
};

pub const ACTIVITY_BATCH_EVENT: &str = "activity_batch_v1";
pub const ACTIVITY_STATUS_EVENT: &str = "activity_status_v1";
pub const LEGACY_ACTIVITY_EVENT: &str = "network_event";
const LEGACY_DETAIL_MAX_CHARS: usize = 32;

pub(crate) struct EmitterBatchSink {
    emitter: Arc<dyn Emitter>,
}

impl EmitterBatchSink {
    pub(crate) fn new(emitter: Arc<dyn Emitter>) -> Self {
        Self { emitter }
    }
}

impl ActivityBatchSink for EmitterBatchSink {
    fn try_publish(&self, batch: &ActivityBatchV1) -> Result<(), ActivityPublishError> {
        let payload = serde_json::to_value(batch).map_err(|_| ActivityPublishError::Rejected)?;
        let typed_result = self
            .emitter
            .try_emit(ACTIVITY_BATCH_EVENT, payload)
            .map_err(map_emit_error);

        // Temporary Stage 2 compatibility: the old Activity surface still
        // listens for flattened `network_event` rows. Derive those rows only
        // from the immutable masked event, after classification/redaction and
        // never from a producer's raw inputs. Stage 4 removes this projector.
        for event in batch.events() {
            let Some(legacy) = LegacyActivityProjection::from_masked(event) else {
                continue;
            };
            let payload = match serde_json::to_value(legacy) {
                Ok(payload) => payload,
                Err(_) => continue,
            };
            let _ = self.emitter.try_emit(LEGACY_ACTIVITY_EVENT, payload);
        }

        // Compatibility projection is deliberately best effort and cannot
        // contaminate the typed recorder's IPC-health accounting.
        typed_result
    }

    fn try_publish_status(&self, status: &ActivityStatusV1) -> Result<(), ActivityPublishError> {
        let payload = serde_json::to_value(status).map_err(|_| ActivityPublishError::Rejected)?;
        self.emitter
            .try_emit(ACTIVITY_STATUS_EVENT, payload)
            .map_err(map_emit_error)
    }
}

fn map_emit_error(error: EmitError) -> ActivityPublishError {
    match error {
        EmitError::Rejected => ActivityPublishError::Rejected,
        EmitError::Unavailable => ActivityPublishError::Unavailable,
    }
}

#[derive(serde::Serialize)]
struct LegacyActivityProjection {
    #[serde(rename = "type")]
    event_type: &'static str,
    message: &'static str,
    detail: String,
    timestamp: u64,
    level: &'static str,
    severity: ActivitySeverity,
    capture_session: String,
    sequence: String,
    capture_generation: String,
}

impl LegacyActivityProjection {
    fn from_masked(event: &ActivityEventV1) -> Option<Self> {
        let message = legacy_message(event)?;
        Some(Self {
            event_type: legacy_type(event),
            message,
            detail: legacy_detail(event),
            timestamp: event.timestamp_unix_ms,
            level: legacy_level(event),
            severity: event.severity(),
            capture_session: event.capture_session().to_string(),
            sequence: event.sequence().to_string(),
            capture_generation: event.capture_generation().to_string(),
        })
    }
}

fn legacy_type(event: &ActivityEventV1) -> &'static str {
    if event.severity() == ActivitySeverity::Error {
        return "error";
    }
    let kind = event.kind();
    if kind.starts_with("rns.path.") {
        "path"
    } else if kind.starts_with("rns.announce.") {
        "announce"
    } else if kind.starts_with("interface.") || kind.starts_with("app.runtime.") {
        "interface"
    } else if kind.starts_with("rns.link.") || kind.starts_with("resource.") {
        "link"
    } else if kind.starts_with("lxmf.") {
        "message"
    } else if kind.starts_with("lxst.") {
        "lxst"
    } else {
        // Only allowlisted summaries reach this function; Ratspeak/diagnostic
        // compatibility rows share the old interface/system rail.
        "interface"
    }
}

fn legacy_level(event: &ActivityEventV1) -> &'static str {
    if matches!(
        event.severity(),
        ActivitySeverity::Error | ActivitySeverity::Warning
    ) {
        "essential"
    } else if matches!(
        event.kind(),
        "rns.path.observed"
            | "rns.announce.observed"
            | "rns.packet.sampled"
            | "lxmf.delivery.progress"
    ) {
        "detailed"
    } else {
        "standard"
    }
}

fn legacy_message(event: &ActivityEventV1) -> Option<&'static str> {
    let kind = event.kind();
    if kind.starts_with("interface.") {
        return interface_legacy_message(
            kind,
            attribute_code(event, ActivityAttributeKey::InterfaceClass),
            attribute_code(event, ActivityAttributeKey::Reason),
        );
    }
    Some(match kind {
        // Capture controls already communicate these transitions. Recorder
        // health remains visible because silent loss is never acceptable.
        "diagnostics.capture_started"
        | "diagnostics.capture_stopped"
        | "diagnostics.capture_resumed"
        | "diagnostics.capture_cleared"
        | "diagnostics.profile_changed" => return None,
        "diagnostics.dropped" => "Activity events dropped",
        "diagnostics.evicted" => "Older Activity events removed",
        "diagnostics.worker_recovered" => "Activity recorder recovered",
        "app.runtime.started" => "Ratspeak runtime started",
        "app.runtime.ready" => "Ratspeak runtime ready",
        "app.runtime.unavailable" => "Ratspeak runtime unavailable",
        "app.runtime.stopped" => "Ratspeak runtime stopped",
        "storage.db.failed" => "Local storage unavailable",
        "ipc.failed" => "App event delivery failed",
        "rns.transport.started" => "Reticulum transport started",
        "rns.transport.ready" => "Reticulum transport ready",
        "rns.transport.unavailable" => "Reticulum transport unavailable",
        "rns.transport.stopped" => "Reticulum transport stopped",
        "rns.path.requested" => "Path requested",
        "rns.path.discovered" => "Path discovered",
        "rns.path.observed" => "Path observed",
        "rns.path.timed_out" => "Path request timed out",
        "rns.announce.sent" => "Announce sent",
        "rns.announce.failed" => "Announce failed",
        "rns.announce.held" => "Announce queued",
        "rns.announce.observed" => "Announce observed",
        "rns.announce.ingress_burst_started" => "High announce traffic detected",
        "rns.announce.ingress_burst_cleared" => "Announce traffic returned to normal",
        "rns.announce.suppressed" => "Announce suppressed",
        "rns.security.dropped" => "Network input rejected",
        "rns.packet.sampled" => "Packet observed",
        "rns.link.requested" => "Link requested",
        "rns.link.authenticated" => "Link authenticated",
        "rns.link.identified" => "Link identified",
        "rns.link.stale" => "Link became stale",
        "rns.link.recovered" => "Link recovered",
        "rns.link.closed" => "Link closed",
        "resource.started" => "Resource transfer started",
        "resource.progress" => "Resource transfer progress",
        "resource.succeeded" => "Resource transfer completed",
        "resource.failed" => "Resource transfer failed",
        "lxmf.delivery.queued" => "Message queued",
        "lxmf.delivery.submission_failed" => "Message could not be queued",
        "lxmf.delivery.method_selected" => "Delivery method selected",
        "lxmf.delivery.path_pending" => "Message path pending",
        "lxmf.delivery.link_establishing" => "Establishing delivery link",
        "lxmf.delivery.link_ready" => "Delivery link ready",
        "lxmf.delivery.link_reused" => "Direct link reused",
        "lxmf.delivery.direct_pending" => "Waiting for direct delivery",
        "lxmf.delivery.resource_started" => "Resource transfer advertised",
        "lxmf.delivery.progress" => "Resource transfer progress",
        "lxmf.delivery.awaiting_proof" => "Waiting for delivery proof",
        "lxmf.delivery.delivered" => "Message delivered",
        "lxmf.delivery.rejected" => "Message delivery rejected",
        "lxmf.delivery.deferred" => "Message delivery deferred",
        "lxmf.delivery.retrying" => "Message delivery retrying",
        "lxmf.delivery.failed" => "Message delivery failed",
        "lxmf.propagation.started" => "Storing in Offline Inbox",
        "lxmf.propagation.succeeded" => "Stored in Offline Inbox",
        "lxmf.propagation.failed" => "Offline Inbox delivery failed",
        "lxmf.inbound.accepted" => "Message received",
        "lxmf.inbound.rejected" => "Inbound message rejected",
        "lxst.service.started" => "LXST voice service started",
        "lxst.service.stopped" => "LXST voice service stopped",
        "lxst.service.failed" => "LXST voice service unavailable",
        "lxst.call.path_pending" => "Resolving LXST call path",
        "lxst.call.link_requested" => "LXST call link requested",
        "lxst.call.ringing" => "Incoming LXST call",
        "lxst.call.answered" => "LXST call answered",
        "lxst.call.ended" => "LXST call ended",
        "lxst.call.rejected" => "LXST call rejected",
        "lxst.call.failed" => "LXST call failed",
        "lxst.media.started" => "LXST call media started",
        "lxst.media.stopped" => "LXST call media stopped",
        "lxst.media.warning" => "LXST call media warning",
        _ => return None,
    })
}

fn interface_legacy_message(
    kind: &str,
    class: Option<&str>,
    reason: Option<&str>,
) -> Option<&'static str> {
    let state = match kind {
        "interface.configured" => "configured",
        "interface.connecting" => "connecting",
        "interface.cancelled" => "cancelled",
        "interface.online" => "online",
        "interface.offline" => "offline",
        "interface.degraded" => "degraded",
        "interface.paused" => "paused",
        "interface.removed" => "removed",
        "interface.failed" => "failed",
        "interface.timed_out" => "timed_out",
        _ => return None,
    };
    match state {
        "cancelled" => return Some(interface_cancelled_legacy_message(class)),
        "degraded" => return Some(interface_degraded_legacy_message(class, reason)),
        "failed" => return Some(interface_failure_legacy_message(class, reason)),
        "timed_out" => return Some(interface_timeout_legacy_message(class, reason)),
        _ => {}
    }
    Some(match (class, state) {
        (Some("auto"), "configured") => "Local Network configured",
        (Some("auto"), "connecting") => "Local Network starting",
        (Some("auto"), "online") => "Local Network enabled",
        (Some("auto"), "offline") => "Local Network connectivity interrupted",
        (Some("auto"), "paused") => "Local Network paused",
        (Some("auto"), "removed") => "Local Network disabled",
        (Some("ble_peer"), "configured") => "Bluetooth Peer configured",
        (Some("ble_peer"), "connecting") => "Bluetooth Peer starting",
        (Some("ble_peer"), "online") => "Bluetooth Peer ready",
        (Some("ble_peer"), "offline") => "Bluetooth Peer unavailable",
        (Some("ble_peer"), "paused") => "Bluetooth Peer paused",
        (Some("ble_peer"), "removed") => "Bluetooth Peer stopped",
        (Some("rnode"), "configured") => "LoRa radio configured",
        (Some("rnode"), "connecting") => "LoRa radio connecting",
        (Some("rnode"), "online") => "LoRa radio connected",
        (Some("rnode"), "offline") => "LoRa radio disconnected",
        (Some("rnode"), "paused") => "LoRa radio paused",
        (Some("rnode"), "removed") => "LoRa radio removed",
        (Some("tcp_client"), "configured") => "TCP client configured",
        (Some("tcp_client"), "connecting") => "TCP client connecting",
        (Some("tcp_client"), "online") => "TCP client online",
        (Some("tcp_client"), "offline") => "TCP client offline",
        (Some("tcp_client"), "paused") => "TCP client paused",
        (Some("tcp_client"), "removed") => "TCP client removed",
        (Some("tcp_server"), "configured") => "TCP server configured",
        (Some("tcp_server"), "connecting") => "TCP server starting",
        (Some("tcp_server"), "online") => "TCP server online",
        (Some("tcp_server"), "offline") => "TCP server offline",
        (Some("tcp_server"), "paused") => "TCP server paused",
        (Some("tcp_server"), "removed") => "TCP server removed",
        (Some("backbone_client"), "configured") => "Backbone client configured",
        (Some("backbone_client"), "connecting") => "Backbone client connecting",
        (Some("backbone_client"), "online") => "Backbone client online",
        (Some("backbone_client"), "offline") => "Backbone client offline",
        (Some("backbone_client"), "paused") => "Backbone client paused",
        (Some("backbone_client"), "removed") => "Backbone client removed",
        (Some("backbone_server"), "configured") => "Backbone server configured",
        (Some("backbone_server"), "connecting") => "Backbone server starting",
        (Some("backbone_server"), "online") => "Backbone server online",
        (Some("backbone_server"), "offline") => "Backbone server offline",
        (Some("backbone_server"), "paused") => "Backbone server paused",
        (Some("backbone_server"), "removed") => "Backbone server removed",
        (_, "configured") => "Interface configured",
        (_, "connecting") => "Interface connecting",
        (_, "online") => "Interface online",
        (_, "offline") => "Interface offline",
        (_, "paused") => "Interface paused",
        (_, "removed") => "Interface removed",
        _ => return None,
    })
}

fn interface_cancelled_legacy_message(class: Option<&str>) -> &'static str {
    match class {
        Some("auto") => "Local Network setup cancelled",
        Some("ble_peer") => "Bluetooth Peer setup cancelled",
        Some("rnode") => "LoRa radio setup cancelled",
        Some("tcp_client") => "TCP client connection cancelled",
        Some("tcp_server") => "TCP server setup cancelled",
        Some("backbone_client") => "Backbone client connection cancelled",
        Some("backbone_server") => "Backbone server setup cancelled",
        _ => "Interface setup cancelled",
    }
}

fn interface_degraded_legacy_message(class: Option<&str>, reason: Option<&str>) -> &'static str {
    match (class, reason) {
        (Some("auto"), Some("multicast_unavailable")) => "Local Network discovery is limited",
        (Some("auto"), _) => "Local Network access is limited",
        (Some("ble_peer"), _) => "Bluetooth Peer visibility is limited",
        (Some("rnode"), _) => "LoRa radio connection is limited",
        _ => "Interface availability is limited",
    }
}

fn interface_failure_legacy_message(class: Option<&str>, reason: Option<&str>) -> &'static str {
    match (class, reason) {
        (Some("auto"), Some("configure_failed")) => "Local Network could not be configured",
        (Some("auto"), Some("connect_failed")) => "Local Network could not connect",
        (Some("auto"), Some("listen_failed")) => "Local Network could not accept connections",
        (Some("auto"), Some("remove_failed")) => "Local Network could not be disabled",
        (Some("auto"), Some("resume_failed")) => "Local Network could not resume",
        (Some("auto"), Some("runtime_failed")) => "Local Network encountered an error",
        (Some("auto"), Some("update_failed")) => "Local Network could not be updated",
        (Some("auto"), _) => "Local Network operation failed",
        (Some("ble_peer"), Some("configure_failed")) => "Bluetooth Peer could not be configured",
        (Some("ble_peer"), Some("connect_failed")) => "Bluetooth Peer could not connect",
        (Some("ble_peer"), Some("listen_failed")) => "Bluetooth Peer could not accept connections",
        (Some("ble_peer"), Some("remove_failed")) => "Bluetooth Peer could not stop",
        (Some("ble_peer"), Some("resume_failed")) => "Bluetooth Peer could not resume",
        (Some("ble_peer"), Some("runtime_failed")) => "Bluetooth Peer encountered an error",
        (Some("ble_peer"), Some("update_failed")) => "Bluetooth Peer could not be updated",
        (Some("ble_peer"), _) => "Bluetooth Peer operation failed",
        (Some("rnode"), Some("configure_failed")) => "LoRa radio could not be configured",
        (Some("rnode"), Some("connect_failed")) => "LoRa radio could not connect",
        (Some("rnode"), Some("listen_failed")) => "LoRa radio could not accept connections",
        (Some("rnode"), Some("remove_failed")) => "LoRa radio could not be removed",
        (Some("rnode"), Some("resume_failed")) => "LoRa radio could not resume",
        (Some("rnode"), Some("runtime_failed")) => "LoRa radio encountered an error",
        (Some("rnode"), Some("update_failed")) => "LoRa radio could not be updated",
        (Some("rnode"), _) => "LoRa radio operation failed",
        (Some("tcp_client"), Some("configure_failed")) => "TCP client could not be configured",
        (Some("tcp_client"), Some("connect_failed")) => "TCP client could not connect",
        (Some("tcp_client"), Some("listen_failed")) => "TCP client could not start listening",
        (Some("tcp_client"), Some("remove_failed")) => "TCP client could not be removed",
        (Some("tcp_client"), Some("resume_failed")) => "TCP client could not resume",
        (Some("tcp_client"), Some("runtime_failed")) => "TCP client encountered an error",
        (Some("tcp_client"), Some("update_failed")) => "TCP client could not be updated",
        (Some("tcp_client"), _) => "TCP client operation failed",
        (Some("tcp_server"), Some("configure_failed")) => "TCP server could not be configured",
        (Some("tcp_server"), Some("connect_failed")) => "TCP server could not connect",
        (Some("tcp_server"), Some("listen_failed")) => "TCP server could not start listening",
        (Some("tcp_server"), Some("remove_failed")) => "TCP server could not be removed",
        (Some("tcp_server"), Some("resume_failed")) => "TCP server could not resume",
        (Some("tcp_server"), Some("runtime_failed")) => "TCP server encountered an error",
        (Some("tcp_server"), Some("update_failed")) => "TCP server could not be updated",
        (Some("tcp_server"), _) => "TCP server operation failed",
        (Some("backbone_client"), Some("configure_failed")) => {
            "Backbone client could not be configured"
        }
        (Some("backbone_client"), Some("connect_failed")) => "Backbone client could not connect",
        (Some("backbone_client"), Some("listen_failed")) => {
            "Backbone client could not start listening"
        }
        (Some("backbone_client"), Some("remove_failed")) => "Backbone client could not be removed",
        (Some("backbone_client"), Some("resume_failed")) => "Backbone client could not resume",
        (Some("backbone_client"), Some("runtime_failed")) => "Backbone client encountered an error",
        (Some("backbone_client"), Some("update_failed")) => "Backbone client could not be updated",
        (Some("backbone_client"), _) => "Backbone client operation failed",
        (Some("backbone_server"), Some("configure_failed")) => {
            "Backbone server could not be configured"
        }
        (Some("backbone_server"), Some("connect_failed")) => "Backbone server could not connect",
        (Some("backbone_server"), Some("listen_failed")) => {
            "Backbone server could not start listening"
        }
        (Some("backbone_server"), Some("remove_failed")) => "Backbone server could not be removed",
        (Some("backbone_server"), Some("resume_failed")) => "Backbone server could not resume",
        (Some("backbone_server"), Some("runtime_failed")) => "Backbone server encountered an error",
        (Some("backbone_server"), Some("update_failed")) => "Backbone server could not be updated",
        (Some("backbone_server"), _) => "Backbone server operation failed",
        (_, Some("configure_failed")) => "Interface configuration failed",
        (_, Some("connect_failed")) => "Interface connection failed",
        (_, Some("listen_failed")) => "Interface could not start listening",
        (_, Some("remove_failed")) => "Interface removal failed",
        (_, Some("resume_failed")) => "Interface resume failed",
        (_, Some("runtime_failed")) => "Interface runtime failed",
        (_, Some("update_failed")) => "Interface update failed",
        _ => "Interface operation failed",
    }
}

fn interface_timeout_legacy_message(class: Option<&str>, reason: Option<&str>) -> &'static str {
    match (class, reason) {
        (Some("auto"), Some("startup_timed_out")) => "Local Network startup timed out",
        (Some("auto"), _) => "Local Network setup timed out",
        (Some("ble_peer"), Some("pairing_timed_out")) => "Bluetooth Peer pairing timed out",
        (Some("ble_peer"), Some("startup_timed_out")) => "Bluetooth Peer startup timed out",
        (Some("ble_peer"), _) => "Bluetooth Peer setup timed out",
        (Some("rnode"), Some("pairing_timed_out")) => "LoRa radio pairing timed out",
        (Some("rnode"), Some("startup_timed_out")) => "LoRa radio startup timed out",
        (Some("rnode"), _) => "LoRa radio setup timed out",
        (_, Some("pairing_timed_out")) => "Interface pairing timed out",
        (_, Some("startup_timed_out")) => "Interface startup timed out",
        _ => "Interface setup timed out",
    }
}

fn legacy_detail(event: &ActivityEventV1) -> String {
    let mut fragments = Vec::with_capacity(2);
    match event.kind() {
        "diagnostics.dropped" => {
            push_unsigned(
                &mut fragments,
                event,
                ActivityAttributeKey::DroppedCount,
                "Dropped ",
                "",
            );
            push_unsigned(
                &mut fragments,
                event,
                ActivityAttributeKey::TimeSpanMs,
                "",
                " ms",
            );
        }
        "diagnostics.evicted" => {
            push_unsigned(
                &mut fragments,
                event,
                ActivityAttributeKey::EvictedCount,
                "Removed ",
                "",
            );
            push_unsigned(
                &mut fragments,
                event,
                ActivityAttributeKey::ByteLength,
                "",
                " bytes",
            );
        }
        "rns.path.requested" => {
            push_ordinal(
                &mut fragments,
                event,
                ActivityAttributeKey::Destination,
                "Destination",
            );
            push_unsigned(
                &mut fragments,
                event,
                ActivityAttributeKey::Count,
                "",
                " destinations",
            );
        }
        "rns.path.discovered" | "rns.path.observed" => {
            push_ordinal(
                &mut fragments,
                event,
                ActivityAttributeKey::Destination,
                "Destination",
            );
            push_unsigned(
                &mut fragments,
                event,
                ActivityAttributeKey::Hops,
                "",
                " hops",
            );
        }
        "rns.announce.sent" => {
            push_code(&mut fragments, event, ActivityAttributeKey::Method);
        }
        "rns.announce.failed" => {
            push_code(&mut fragments, event, ActivityAttributeKey::Reason);
            push_code(&mut fragments, event, ActivityAttributeKey::Method);
        }
        "rns.announce.suppressed" => {
            push_code(&mut fragments, event, ActivityAttributeKey::Reason);
        }
        "rns.announce.held" => {
            push_unsigned(
                &mut fragments,
                event,
                ActivityAttributeKey::QueueCount,
                "",
                " queued",
            );
        }
        "rns.announce.observed" => {
            push_ordinal(
                &mut fragments,
                event,
                ActivityAttributeKey::Destination,
                "Destination",
            );
            push_unsigned(
                &mut fragments,
                event,
                ActivityAttributeKey::Hops,
                "",
                " hops",
            );
        }
        "interface.failed" | "interface.degraded" | "interface.timed_out" => {
            push_code(&mut fragments, event, ActivityAttributeKey::Reason);
            push_code(&mut fragments, event, ActivityAttributeKey::State);
        }
        "lxmf.delivery.queued" => {
            push_ordinal(
                &mut fragments,
                event,
                ActivityAttributeKey::Destination,
                "Destination",
            );
        }
        "lxmf.delivery.submission_failed"
        | "lxmf.delivery.rejected"
        | "lxmf.delivery.failed"
        | "lxmf.propagation.failed" => {
            push_code(&mut fragments, event, ActivityAttributeKey::Reason);
            push_ordinal(
                &mut fragments,
                event,
                ActivityAttributeKey::Destination,
                "Destination",
            );
        }
        "lxmf.delivery.delivered" => {
            push_unsigned(
                &mut fragments,
                event,
                ActivityAttributeKey::RttMs,
                "",
                " ms RTT",
            );
            push_ordinal(
                &mut fragments,
                event,
                ActivityAttributeKey::Message,
                "Message",
            );
        }
        "lxmf.delivery.progress" => {
            push_unsigned(
                &mut fragments,
                event,
                ActivityAttributeKey::Percent,
                "",
                "%",
            );
            push_ordinal(
                &mut fragments,
                event,
                ActivityAttributeKey::Destination,
                "Destination",
            );
        }
        kind if kind.starts_with("lxmf.delivery.") || kind.starts_with("lxmf.propagation.") => {
            push_ordinal(
                &mut fragments,
                event,
                ActivityAttributeKey::Message,
                "Message",
            );
            push_code(&mut fragments, event, ActivityAttributeKey::Method);
        }
        "lxmf.inbound.accepted" | "lxmf.inbound.rejected" => {
            push_ordinal(
                &mut fragments,
                event,
                ActivityAttributeKey::Destination,
                "Peer",
            );
            push_code(
                &mut fragments,
                event,
                if event.kind() == "lxmf.inbound.accepted" {
                    ActivityAttributeKey::Method
                } else {
                    ActivityAttributeKey::Reason
                },
            );
        }
        kind if kind.starts_with("lxst.call.") => {
            push_ordinal(
                &mut fragments,
                event,
                ActivityAttributeKey::Identity,
                "Peer",
            );
            push_code(&mut fragments, event, ActivityAttributeKey::Reason);
        }
        "lxst.media.warning" => {
            push_code(&mut fragments, event, ActivityAttributeKey::Reason);
        }
        _ => {}
    }
    if event.count > 1 {
        let count = format!("×{}", event.count);
        if fragments.is_empty() {
            fragments.push(count);
        } else {
            // Count is never expendable: coalescing must remain visible in the
            // temporary row even when a kind normally contributes two facts.
            fragments.truncate(1);
            let first_budget = LEGACY_DETAIL_MAX_CHARS
                .saturating_sub(" · ".chars().count())
                .saturating_sub(count.chars().count());
            fragments[0] = truncate_fragment(&fragments[0], first_budget);
            fragments.push(count);
        }
    }
    fragments.truncate(2);
    truncate_fragment(&fragments.join(" · "), LEGACY_DETAIL_MAX_CHARS)
}

fn truncate_fragment(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    value
        .chars()
        .take(max_chars.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

fn attribute(event: &ActivityEventV1, key: ActivityAttributeKey) -> Option<&ActivityAttributeV1> {
    event
        .attributes
        .iter()
        .find(|attribute| attribute.key == key)
}

fn attribute_code(event: &ActivityEventV1, key: ActivityAttributeKey) -> Option<&str> {
    match &attribute(event, key)?.value {
        ActivityValueV1::Code(value) => Some(value.as_str()),
        _ => None,
    }
}

fn push_ordinal(
    fragments: &mut Vec<String>,
    event: &ActivityEventV1,
    key: ActivityAttributeKey,
    label: &str,
) {
    if fragments.len() >= 2 {
        return;
    }
    let Some(ActivityAttributeV1 {
        value: ActivityValueV1::Identifier(value),
        ..
    }) = attribute(event, key)
    else {
        return;
    };
    // A full masked digest is still too long for the legacy row. If the
    // bounded ordinal table is exhausted, omit the token entirely.
    if let Some(ordinal) = value.ordinal {
        fragments.push(format!("{label} {ordinal}"));
    }
}

fn push_unsigned(
    fragments: &mut Vec<String>,
    event: &ActivityEventV1,
    key: ActivityAttributeKey,
    prefix: &str,
    suffix: &str,
) {
    if fragments.len() >= 2 {
        return;
    }
    let Some(ActivityAttributeV1 {
        value: ActivityValueV1::Unsigned(value),
        ..
    }) = attribute(event, key)
    else {
        return;
    };
    fragments.push(format!("{prefix}{value}{suffix}"));
}

fn push_code(fragments: &mut Vec<String>, event: &ActivityEventV1, key: ActivityAttributeKey) {
    if fragments.len() >= 2 {
        return;
    }
    if let Some(label) = attribute_code(event, key).and_then(friendly_code) {
        fragments.push(label.to_string());
    }
}

fn friendly_code(value: &str) -> Option<&'static str> {
    Some(match value {
        "automatic" => "Automatic",
        "contact_refresh" => "Contact refresh",
        "manual" => "Manual",
        "startup" => "Startup",
        "transport" => "Transport",
        "interface_online" => "Interface online",
        "lxmf_delivery" => "LXMF delivery",
        "lxst_service" => "LXST service",
        "direct" => "Direct",
        "opportunistic" => "Opportunistic",
        "paper" => "Paper",
        "propagated" => "Propagated",
        "configure_failed" => "Configuration failed",
        "connect_failed" => "Connection failed",
        "listen_failed" => "Listen failed",
        "remove_failed" => "Removal failed",
        "resume_failed" => "Resume failed",
        "runtime_failed" => "Runtime failed",
        "update_failed" => "Update failed",
        "multicast_unavailable" => "Peer discovery unavailable",
        "peripheral_unavailable" => "Incoming connections unavailable",
        "setup_timed_out" => "Setup timed out",
        "pairing_timed_out" => "Pairing timed out",
        "startup_timed_out" => "Startup timed out",
        "config_restored" => "Config restored",
        "restart_failed" => "Restart failed",
        "write_failed" => "Rollback write failed",
        "no_interface_transmission" => "No interface transmission",
        "not_ready" => "Not ready",
        "queue_failed" => "Queue failed",
        "transport_unavailable" => "Transport unavailable",
        "cooldown" => "Cooldown",
        "interface_restart" => "Interface restart",
        "rate_limit" => "Rate limited",
        "router_unavailable" => "Router unavailable",
        "preparation_failed" => "Message preparation failed",
        "queue_rejected" => "Queue rejected",
        "link_closed" => "Link closed",
        "path_unavailable" => "Path unavailable",
        "proof_timed_out" => "Proof timed out",
        "resource_failed" => "Resource failed",
        "transport_failed" => "Transport failed",
        "busy" => "Busy",
        "rejected" => "Rejected",
        "calling" => "Calling",
        "available" => "Available",
        "ringing" => "Ringing",
        "connecting" => "Connecting",
        "established" => "Established",
        "link_failed" => "Link failed",
        "service_error" => "Service error",
        "media_error" => "Media error",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::Value;

    use super::super::classified::{CorrelationId, DraftContext, ReadyDraft};
    use super::super::lifecycle::{ActivityRecordOutcome, ActivityRecorder};
    use super::super::producer;
    use super::super::pseudonym::CapturePrivacy;
    use super::super::replay::ActivityReplayResultV1;
    use super::super::schema::{
        ActivityArea, ActivityDirection, ActivityOutcome, CaptureProfile, CaptureScope,
    };
    use super::*;

    #[derive(Default)]
    struct RecordingEmitter {
        events: Mutex<Vec<(String, Value)>>,
        reject_typed: AtomicBool,
        reject_legacy: AtomicBool,
        legacy_attempts: AtomicUsize,
    }

    impl RecordingEmitter {
        fn payloads(&self, event: &str) -> Vec<Value> {
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .filter_map(|(name, payload)| (name == event).then_some(payload.clone()))
                .collect()
        }

        fn all_payload_text(&self) -> String {
            serde_json::to_string(
                &*self
                    .events
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )
            .unwrap()
        }
    }

    impl Emitter for RecordingEmitter {
        fn try_emit(&self, event: &str, payload: Value) -> Result<(), EmitError> {
            if event == ACTIVITY_BATCH_EVENT && self.reject_typed.load(Ordering::Relaxed) {
                return Err(EmitError::Rejected);
            }
            if event == LEGACY_ACTIVITY_EVENT {
                self.legacy_attempts.fetch_add(1, Ordering::Relaxed);
                if self.reject_legacy.load(Ordering::Relaxed) {
                    return Err(EmitError::Rejected);
                }
            }
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((event.to_string(), payload));
            Ok(())
        }
    }

    async fn wait_until(mut predicate: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !predicate() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("condition should become true");
    }

    struct ProducerProjectionCase {
        event: producer::ProducerEvent,
        kind: &'static str,
        area: ActivityArea,
        severity: ActivitySeverity,
        scope: CaptureScope,
        attribute_keys: &'static [ActivityAttributeKey],
        legacy_type: &'static str,
        legacy_message: &'static str,
        legacy_level: &'static str,
    }

    macro_rules! projection_case {
        (
            $event:expr,
            $kind:literal,
            $area:ident,
            $severity:ident,
            $scope:ident,
            [$($attribute:ident),* $(,)?],
            $legacy_type:literal,
            $legacy_message:literal,
            $legacy_level:literal
        ) => {
            ProducerProjectionCase {
                event: $event,
                kind: $kind,
                area: ActivityArea::$area,
                severity: ActivitySeverity::$severity,
                scope: CaptureScope::$scope,
                attribute_keys: &[$(ActivityAttributeKey::$attribute),*],
                legacy_type: $legacy_type,
                legacy_message: $legacy_message,
                legacy_level: $legacy_level,
            }
        };
    }

    fn destination(value: u8) -> producer::DestinationHash {
        producer::DestinationHash::new([value; 16])
    }

    fn message(value: u8) -> producer::MessageId {
        producer::MessageId::new([value; 32])
    }

    fn link(value: u8) -> producer::LinkId {
        producer::LinkId::new([value; 16])
    }

    fn identity(value: u8) -> producer::IdentityHash {
        producer::IdentityHash::new([value; 16])
    }

    fn expected_direction(kind: &str) -> ActivityDirection {
        match kind {
            "app.runtime.started"
            | "app.runtime.ready"
            | "app.runtime.unavailable"
            | "app.runtime.stopped"
            | "interface.configured"
            | "interface.connecting"
            | "interface.cancelled"
            | "interface.online"
            | "interface.offline"
            | "interface.degraded"
            | "interface.paused"
            | "interface.removed"
            | "interface.failed"
            | "interface.timed_out"
            | "rns.announce.held"
            | "rns.announce.ingress_burst_cleared"
            | "rns.announce.suppressed"
            | "lxst.service.started"
            | "lxst.service.stopped"
            | "lxst.service.failed"
            | "lxst.media.warning" => ActivityDirection::Local,
            "rns.path.requested"
            | "rns.announce.sent"
            | "rns.announce.failed"
            | "lxmf.delivery.queued"
            | "lxmf.delivery.submission_failed"
            | "lxmf.delivery.path_pending"
            | "lxmf.delivery.link_establishing"
            | "lxmf.delivery.link_ready"
            | "lxmf.delivery.link_reused"
            | "lxmf.delivery.direct_pending"
            | "lxmf.delivery.resource_started"
            | "lxmf.delivery.progress"
            | "lxmf.delivery.awaiting_proof"
            | "lxmf.delivery.delivered"
            | "lxmf.delivery.rejected"
            | "lxmf.delivery.failed"
            | "lxmf.propagation.started"
            | "lxmf.propagation.succeeded"
            | "lxmf.propagation.failed"
            | "lxst.call.path_pending"
            | "lxst.call.link_requested" => ActivityDirection::Outbound,
            "rns.path.discovered"
            | "rns.path.observed"
            | "rns.announce.ingress_burst_started"
            | "rns.announce.observed"
            | "lxmf.inbound.accepted"
            | "lxst.call.ringing" => ActivityDirection::Inbound,
            "lxst.call.ended" | "lxst.call.rejected" | "lxst.call.failed" => {
                ActivityDirection::None
            }
            _ => panic!("missing expected direction for {kind}"),
        }
    }

    fn expected_outcome(kind: &str) -> ActivityOutcome {
        match kind {
            "app.runtime.started"
            | "interface.connecting"
            | "rns.path.requested"
            | "lxmf.delivery.queued"
            | "lxst.service.started"
            | "lxst.call.ringing"
            | "lxst.call.link_requested" => ActivityOutcome::Started,
            "rns.announce.held"
            | "lxmf.delivery.path_pending"
            | "lxmf.delivery.link_establishing"
            | "lxmf.delivery.link_ready"
            | "lxmf.delivery.link_reused"
            | "lxmf.delivery.direct_pending"
            | "lxmf.delivery.resource_started"
            | "lxmf.delivery.progress"
            | "lxmf.delivery.awaiting_proof"
            | "lxmf.propagation.started"
            | "lxst.call.path_pending" => ActivityOutcome::Progress,
            "app.runtime.ready"
            | "app.runtime.stopped"
            | "interface.configured"
            | "interface.cancelled"
            | "interface.online"
            | "interface.paused"
            | "interface.removed"
            | "rns.path.discovered"
            | "rns.path.observed"
            | "rns.announce.sent"
            | "rns.announce.ingress_burst_cleared"
            | "rns.announce.observed"
            | "lxmf.delivery.delivered"
            | "lxmf.propagation.succeeded"
            | "lxmf.inbound.accepted"
            | "lxst.service.stopped"
            | "lxst.call.ended" => ActivityOutcome::Success,
            "interface.offline"
            | "interface.degraded"
            | "rns.announce.ingress_burst_started"
            | "lxst.media.warning" => ActivityOutcome::Degraded,
            "rns.announce.suppressed" => ActivityOutcome::Dropped,
            "lxmf.delivery.rejected" | "lxst.call.rejected" => ActivityOutcome::Rejected,
            "app.runtime.unavailable"
            | "interface.failed"
            | "rns.announce.failed"
            | "lxmf.delivery.submission_failed"
            | "lxmf.delivery.failed"
            | "lxmf.propagation.failed"
            | "lxst.service.failed"
            | "lxst.call.failed" => ActivityOutcome::Failed,
            "interface.timed_out" => ActivityOutcome::TimedOut,
            _ => panic!("missing expected outcome for {kind}"),
        }
    }

    #[test]
    fn interface_failure_compatibility_copy_tracks_the_operation_reason() {
        for (class, reason, expected) in [
            (
                Some("auto"),
                Some("configure_failed"),
                "Local Network could not be configured",
            ),
            (
                Some("auto"),
                Some("connect_failed"),
                "Local Network could not connect",
            ),
            (
                Some("auto"),
                Some("runtime_failed"),
                "Local Network encountered an error",
            ),
            (
                Some("auto"),
                Some("remove_failed"),
                "Local Network could not be disabled",
            ),
            (
                Some("auto"),
                Some("resume_failed"),
                "Local Network could not resume",
            ),
            (
                Some("auto"),
                Some("update_failed"),
                "Local Network could not be updated",
            ),
            (
                Some("ble_peer"),
                Some("configure_failed"),
                "Bluetooth Peer could not be configured",
            ),
            (
                Some("ble_peer"),
                Some("connect_failed"),
                "Bluetooth Peer could not connect",
            ),
            (
                Some("ble_peer"),
                Some("runtime_failed"),
                "Bluetooth Peer encountered an error",
            ),
            (
                Some("ble_peer"),
                Some("remove_failed"),
                "Bluetooth Peer could not stop",
            ),
            (
                Some("ble_peer"),
                Some("resume_failed"),
                "Bluetooth Peer could not resume",
            ),
            (
                Some("ble_peer"),
                Some("update_failed"),
                "Bluetooth Peer could not be updated",
            ),
            (
                Some("rnode"),
                Some("configure_failed"),
                "LoRa radio could not be configured",
            ),
            (
                Some("rnode"),
                Some("connect_failed"),
                "LoRa radio could not connect",
            ),
            (
                Some("rnode"),
                Some("runtime_failed"),
                "LoRa radio encountered an error",
            ),
            (
                Some("rnode"),
                Some("remove_failed"),
                "LoRa radio could not be removed",
            ),
            (
                Some("rnode"),
                Some("resume_failed"),
                "LoRa radio could not resume",
            ),
            (
                Some("rnode"),
                Some("update_failed"),
                "LoRa radio could not be updated",
            ),
        ] {
            assert_eq!(
                interface_legacy_message("interface.failed", class, reason),
                Some(expected)
            );
        }
    }

    #[test]
    fn interface_terminal_and_degradation_copy_is_closed_and_product_facing() {
        assert_eq!(
            interface_legacy_message("interface.cancelled", Some("auto"), None),
            Some("Local Network setup cancelled")
        );
        assert_eq!(
            interface_legacy_message("interface.cancelled", Some("ble_peer"), None),
            Some("Bluetooth Peer setup cancelled")
        );
        assert_eq!(
            interface_legacy_message("interface.cancelled", Some("rnode"), None),
            Some("LoRa radio setup cancelled")
        );
        assert_eq!(
            interface_legacy_message(
                "interface.timed_out",
                Some("auto"),
                Some("startup_timed_out"),
            ),
            Some("Local Network startup timed out")
        );
        assert_eq!(
            interface_legacy_message(
                "interface.timed_out",
                Some("ble_peer"),
                Some("setup_timed_out"),
            ),
            Some("Bluetooth Peer setup timed out")
        );
        assert_eq!(
            interface_legacy_message(
                "interface.timed_out",
                Some("rnode"),
                Some("pairing_timed_out"),
            ),
            Some("LoRa radio pairing timed out")
        );
        assert_eq!(
            interface_legacy_message(
                "interface.degraded",
                Some("auto"),
                Some("multicast_unavailable"),
            ),
            Some("Local Network discovery is limited")
        );
        assert_eq!(
            interface_legacy_message(
                "interface.degraded",
                Some("ble_peer"),
                Some("peripheral_unavailable"),
            ),
            Some("Bluetooth Peer visibility is limited")
        );
        assert_eq!(
            friendly_code("peripheral_unavailable"),
            Some("Incoming connections unavailable")
        );
    }

    #[test]
    fn real_stage_2a_producers_preserve_typed_and_legacy_parity() {
        use producer::{
            AnnounceFailureReason, AnnounceMethod, AnnounceSuppressionReason, AppRuntimeTransition,
            DeliveryFailureReason, InboundLxmfMethod, InterfaceClass, InterfaceFailureReason,
            InterfaceRollback, InterfaceTransition, LxmfDeliveryMethod, LxmfDeliveryState,
            LxmfProgressStep, LxmfSubmissionFailureReason, LxstCallReason, LxstTransition,
            PathEvidence, PathRequestMethod,
        };

        let cases = vec![
            projection_case!(
                producer::app_runtime(AppRuntimeTransition::Started),
                "app.runtime.started",
                Ratspeak,
                Info,
                Normal,
                [],
                "interface",
                "Ratspeak runtime started",
                "standard"
            ),
            projection_case!(
                producer::app_runtime(AppRuntimeTransition::Ready),
                "app.runtime.ready",
                Ratspeak,
                Info,
                Normal,
                [],
                "interface",
                "Ratspeak runtime ready",
                "standard"
            ),
            projection_case!(
                producer::app_runtime(AppRuntimeTransition::Unavailable),
                "app.runtime.unavailable",
                Ratspeak,
                Error,
                Normal,
                [],
                "error",
                "Ratspeak runtime unavailable",
                "essential"
            ),
            projection_case!(
                producer::app_runtime(AppRuntimeTransition::Stopped),
                "app.runtime.stopped",
                Ratspeak,
                Info,
                Normal,
                [],
                "interface",
                "Ratspeak runtime stopped",
                "standard"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::TcpClient,
                    transition: InterfaceTransition::Configured,
                    endpoint: Some(
                        producer::TcpEndpoint::new("node.example:4242".to_string()).unwrap(),
                    ),
                }),
                "interface.configured",
                Interfaces,
                Info,
                Normal,
                [InterfaceClass, Endpoint],
                "interface",
                "TCP client configured",
                "standard"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::BackboneClient,
                    transition: InterfaceTransition::Connecting,
                    endpoint: Some(
                        producer::TcpEndpoint::new("node.example:4243".to_string()).unwrap(),
                    ),
                }),
                "interface.connecting",
                Interfaces,
                Info,
                Normal,
                [InterfaceClass, Endpoint],
                "interface",
                "Backbone client connecting",
                "standard"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::BluetoothPeer,
                    transition: InterfaceTransition::Cancelled,
                    endpoint: None,
                }),
                "interface.cancelled",
                Interfaces,
                Info,
                Normal,
                [InterfaceClass],
                "interface",
                "Bluetooth Peer setup cancelled",
                "standard"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::Auto,
                    transition: InterfaceTransition::Online,
                    endpoint: None,
                }),
                "interface.online",
                Interfaces,
                Info,
                Normal,
                [InterfaceClass],
                "interface",
                "Local Network enabled",
                "standard"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::RNode,
                    transition: InterfaceTransition::Offline,
                    endpoint: None,
                }),
                "interface.offline",
                Interfaces,
                Warning,
                Normal,
                [InterfaceClass],
                "interface",
                "LoRa radio disconnected",
                "essential"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::BluetoothPeer,
                    transition: InterfaceTransition::Degraded {
                        reason: producer::InterfaceDegradationReason::PeripheralUnavailable,
                    },
                    endpoint: None,
                }),
                "interface.degraded",
                Interfaces,
                Warning,
                Normal,
                [InterfaceClass, Reason],
                "interface",
                "Bluetooth Peer visibility is limited",
                "essential"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::Auto,
                    transition: InterfaceTransition::Degraded {
                        reason: producer::InterfaceDegradationReason::MulticastUnavailable,
                    },
                    endpoint: None,
                }),
                "interface.degraded",
                Interfaces,
                Warning,
                Normal,
                [InterfaceClass, Reason],
                "interface",
                "Local Network discovery is limited",
                "essential"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::BackboneClient,
                    transition: InterfaceTransition::Paused,
                    endpoint: None,
                }),
                "interface.paused",
                Interfaces,
                Info,
                Normal,
                [InterfaceClass],
                "interface",
                "Backbone client paused",
                "standard"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::BackboneServer,
                    transition: InterfaceTransition::Removed,
                    endpoint: None,
                }),
                "interface.removed",
                Interfaces,
                Info,
                Normal,
                [InterfaceClass],
                "interface",
                "Backbone server removed",
                "standard"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::TcpServer,
                    transition: InterfaceTransition::Failed {
                        reason: InterfaceFailureReason::Listen,
                        rollback: Some(InterfaceRollback::ConfigRestored),
                    },
                    endpoint: None,
                }),
                "interface.failed",
                Interfaces,
                Error,
                Normal,
                [InterfaceClass, Reason, State],
                "error",
                "TCP server could not start listening",
                "essential"
            ),
            projection_case!(
                producer::interface_activity(producer::InterfaceActivity {
                    class: InterfaceClass::RNode,
                    transition: InterfaceTransition::TimedOut {
                        reason: producer::InterfaceTimeoutReason::Pairing,
                    },
                    endpoint: None,
                }),
                "interface.timed_out",
                Interfaces,
                Error,
                Normal,
                [InterfaceClass, Reason],
                "error",
                "LoRa radio pairing timed out",
                "essential"
            ),
            projection_case!(
                producer::rns_path_requested(producer::RnsPathRequested {
                    destination: Some(destination(1)),
                    count: Some(1),
                    method: PathRequestMethod::Manual,
                }),
                "rns.path.requested",
                Network,
                Info,
                Normal,
                [Method, Destination, Count],
                "path",
                "Path requested",
                "standard"
            ),
            projection_case!(
                producer::rns_path_discovered(producer::RnsPathDiscovered {
                    destination: destination(2),
                    hops: 3,
                    evidence: PathEvidence::PathResponse,
                    endpoint: Some(
                        producer::TcpEndpoint::new("relay.example:4242".to_string()).unwrap(),
                    ),
                    correlation_id: Some(CorrelationId::from_bytes([2; 16])),
                }),
                "rns.path.discovered",
                Network,
                Info,
                Normal,
                [Destination, Hops, Validation, Endpoint],
                "path",
                "Path discovered",
                "standard"
            ),
            projection_case!(
                producer::rns_path_observed(producer::RnsPathDiscovered {
                    destination: destination(3),
                    hops: 4,
                    evidence: PathEvidence::Announce,
                    endpoint: None,
                    correlation_id: None,
                }),
                "rns.path.observed",
                Network,
                Info,
                TraceOnly,
                [Destination, Hops, Validation],
                "path",
                "Path observed",
                "detailed"
            ),
            projection_case!(
                producer::rns_announce_activity(producer::RnsAnnounceActivity {
                    transition: producer::RnsAnnounceTransition::Sent {
                        method: AnnounceMethod::Manual,
                    },
                    interface: Some(InterfaceClass::Auto),
                }),
                "rns.announce.sent",
                Network,
                Info,
                Normal,
                [Method, InterfaceClass],
                "announce",
                "Announce sent",
                "standard"
            ),
            projection_case!(
                producer::rns_announce_activity(producer::RnsAnnounceActivity {
                    transition: producer::RnsAnnounceTransition::Sent {
                        method: AnnounceMethod::Startup,
                    },
                    interface: None,
                }),
                "rns.announce.sent",
                Network,
                Info,
                Normal,
                [Method],
                "announce",
                "Announce sent",
                "standard"
            ),
            projection_case!(
                producer::rns_announce_activity(producer::RnsAnnounceActivity {
                    transition: producer::RnsAnnounceTransition::Failed {
                        method: AnnounceMethod::Transport,
                        reason: AnnounceFailureReason::TransportUnavailable,
                    },
                    interface: Some(InterfaceClass::Unknown),
                }),
                "rns.announce.failed",
                Network,
                Error,
                Normal,
                [Method, Reason, InterfaceClass],
                "error",
                "Announce failed",
                "essential"
            ),
            projection_case!(
                producer::rns_announce_activity(producer::RnsAnnounceActivity {
                    transition: producer::RnsAnnounceTransition::Held { count: 2 },
                    interface: None,
                }),
                "rns.announce.held",
                Network,
                Warning,
                Normal,
                [QueueCount],
                "announce",
                "Announce queued",
                "essential"
            ),
            projection_case!(
                producer::rns_announce_activity(producer::RnsAnnounceActivity {
                    transition: producer::RnsAnnounceTransition::IngressBurstStarted,
                    interface: None,
                }),
                "rns.announce.ingress_burst_started",
                Network,
                Warning,
                Normal,
                [],
                "announce",
                "High announce traffic detected",
                "essential"
            ),
            projection_case!(
                producer::rns_announce_activity(producer::RnsAnnounceActivity {
                    transition: producer::RnsAnnounceTransition::IngressBurstCleared,
                    interface: None,
                }),
                "rns.announce.ingress_burst_cleared",
                Network,
                Info,
                Normal,
                [],
                "announce",
                "Announce traffic returned to normal",
                "standard"
            ),
            projection_case!(
                producer::rns_announce_activity(producer::RnsAnnounceActivity {
                    transition: producer::RnsAnnounceTransition::Suppressed {
                        reason: AnnounceSuppressionReason::RateLimit,
                    },
                    interface: None,
                }),
                "rns.announce.suppressed",
                Network,
                Info,
                Normal,
                [Reason],
                "announce",
                "Announce suppressed",
                "standard"
            ),
            projection_case!(
                producer::rns_announce_activity(producer::RnsAnnounceActivity {
                    transition: producer::RnsAnnounceTransition::Observed {
                        destination: destination(4),
                        hops: 5,
                    },
                    interface: None,
                }),
                "rns.announce.observed",
                Network,
                Info,
                TraceOnly,
                [Destination, Hops],
                "announce",
                "Announce observed",
                "detailed"
            ),
            projection_case!(
                producer::lxmf_delivery_queued(producer::LxmfDeliveryQueued {
                    message: message(1),
                    destination: destination(5),
                    method: LxmfDeliveryMethod::Direct,
                }),
                "lxmf.delivery.queued",
                Messages,
                Info,
                Normal,
                [Message, Destination, Method],
                "message",
                "Message queued",
                "standard"
            ),
            projection_case!(
                producer::lxmf_submission_failed(producer::LxmfSubmissionFailed {
                    destination: destination(6),
                    reason: LxmfSubmissionFailureReason::RouterUnavailable,
                }),
                "lxmf.delivery.submission_failed",
                Messages,
                Error,
                Normal,
                [Destination, Reason],
                "error",
                "Message could not be queued",
                "essential"
            ),
            projection_case!(
                producer::lxmf_delivery_state_changed(producer::LxmfDeliveryStateChanged {
                    message: message(2),
                    state: LxmfDeliveryState::Routing,
                    method: None,
                    rtt_ms: None,
                    failure_reason: None,
                }),
                "lxmf.delivery.path_pending",
                Messages,
                Info,
                Normal,
                [Message],
                "message",
                "Message path pending",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_state_changed(producer::LxmfDeliveryStateChanged {
                    message: message(3),
                    state: LxmfDeliveryState::Propagating,
                    method: Some(LxmfDeliveryMethod::Propagated),
                    rtt_ms: None,
                    failure_reason: None,
                }),
                "lxmf.propagation.started",
                Messages,
                Info,
                Normal,
                [Message, Method],
                "message",
                "Storing in Offline Inbox",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_state_changed(producer::LxmfDeliveryStateChanged {
                    message: message(4),
                    state: LxmfDeliveryState::ReusingBackchannel,
                    method: Some(LxmfDeliveryMethod::Direct),
                    rtt_ms: None,
                    failure_reason: None,
                }),
                "lxmf.delivery.link_reused",
                Messages,
                Info,
                Normal,
                [Message, Method],
                "message",
                "Direct link reused",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_state_changed(producer::LxmfDeliveryStateChanged {
                    message: message(5),
                    state: LxmfDeliveryState::SendingViaLink,
                    method: Some(LxmfDeliveryMethod::Direct),
                    rtt_ms: None,
                    failure_reason: None,
                }),
                "lxmf.delivery.direct_pending",
                Messages,
                Info,
                Normal,
                [Message, Method],
                "message",
                "Waiting for direct delivery",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_state_changed(producer::LxmfDeliveryStateChanged {
                    message: message(6),
                    state: LxmfDeliveryState::Sent,
                    method: Some(LxmfDeliveryMethod::Direct),
                    rtt_ms: None,
                    failure_reason: None,
                }),
                "lxmf.delivery.awaiting_proof",
                Messages,
                Info,
                Normal,
                [Message, Method],
                "message",
                "Waiting for delivery proof",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_state_changed(producer::LxmfDeliveryStateChanged {
                    message: message(7),
                    state: LxmfDeliveryState::Delivered,
                    method: Some(LxmfDeliveryMethod::Direct),
                    rtt_ms: Some(42),
                    failure_reason: None,
                }),
                "lxmf.delivery.delivered",
                Messages,
                Info,
                Normal,
                [Message, Method, RttMs],
                "message",
                "Message delivered",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_state_changed(producer::LxmfDeliveryStateChanged {
                    message: message(8),
                    state: LxmfDeliveryState::Propagated,
                    method: Some(LxmfDeliveryMethod::Propagated),
                    rtt_ms: None,
                    failure_reason: None,
                }),
                "lxmf.propagation.succeeded",
                Messages,
                Info,
                Normal,
                [Message, Method],
                "message",
                "Stored in Offline Inbox",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_state_changed(producer::LxmfDeliveryStateChanged {
                    message: message(9),
                    state: LxmfDeliveryState::Rejected,
                    method: Some(LxmfDeliveryMethod::Direct),
                    rtt_ms: None,
                    failure_reason: Some(DeliveryFailureReason::Rejected),
                }),
                "lxmf.delivery.rejected",
                Messages,
                Error,
                Normal,
                [Message, Method, Reason],
                "error",
                "Message delivery rejected",
                "essential"
            ),
            projection_case!(
                producer::lxmf_delivery_state_changed(producer::LxmfDeliveryStateChanged {
                    message: message(10),
                    state: LxmfDeliveryState::Failed,
                    method: Some(LxmfDeliveryMethod::Direct),
                    rtt_ms: None,
                    failure_reason: Some(DeliveryFailureReason::TransportFailed),
                }),
                "lxmf.delivery.failed",
                Messages,
                Error,
                Normal,
                [Message, Method, Reason],
                "error",
                "Message delivery failed",
                "essential"
            ),
            projection_case!(
                producer::lxmf_delivery_state_changed(producer::LxmfDeliveryStateChanged {
                    message: message(11),
                    state: LxmfDeliveryState::Failed,
                    method: Some(LxmfDeliveryMethod::Propagated),
                    rtt_ms: None,
                    failure_reason: Some(DeliveryFailureReason::TransportFailed),
                }),
                "lxmf.propagation.failed",
                Messages,
                Error,
                Normal,
                [Message, Method, Reason],
                "error",
                "Offline Inbox delivery failed",
                "essential"
            ),
            projection_case!(
                producer::lxmf_delivery_progress(producer::LxmfDeliveryProgress {
                    message: message(12),
                    destination: destination(7),
                    link: None,
                    method: LxmfDeliveryMethod::Direct,
                    step: LxmfProgressStep::LinkEstablishing,
                    percent: None,
                    attempts: 1,
                }),
                "lxmf.delivery.link_establishing",
                Messages,
                Info,
                Normal,
                [Message, Destination, Method, Attempts],
                "message",
                "Establishing delivery link",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_progress(producer::LxmfDeliveryProgress {
                    message: message(13),
                    destination: destination(8),
                    link: Some(link(1)),
                    method: LxmfDeliveryMethod::Direct,
                    step: LxmfProgressStep::LinkReady,
                    percent: None,
                    attempts: 1,
                }),
                "lxmf.delivery.link_ready",
                Messages,
                Info,
                Normal,
                [Message, Destination, Method, Attempts, Link],
                "message",
                "Delivery link ready",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_progress(producer::LxmfDeliveryProgress {
                    message: message(14),
                    destination: destination(9),
                    link: Some(link(2)),
                    method: LxmfDeliveryMethod::Direct,
                    step: LxmfProgressStep::ResourceStarted,
                    percent: None,
                    attempts: 2,
                }),
                "lxmf.delivery.resource_started",
                Messages,
                Info,
                Normal,
                [Message, Destination, Method, Attempts, Link],
                "message",
                "Resource transfer advertised",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_progress(producer::LxmfDeliveryProgress {
                    message: message(15),
                    destination: destination(10),
                    link: Some(link(3)),
                    method: LxmfDeliveryMethod::Direct,
                    step: LxmfProgressStep::ResourceProgress,
                    percent: Some(64),
                    attempts: 2,
                }),
                "lxmf.delivery.progress",
                Messages,
                Info,
                TraceOnly,
                [Message, Destination, Method, Attempts, Link, Percent],
                "message",
                "Resource transfer progress",
                "detailed"
            ),
            projection_case!(
                producer::lxmf_inbound_accepted(producer::LxmfInboundAccepted {
                    source: destination(11),
                    method: InboundLxmfMethod::Opportunistic,
                    encoded_bytes: 128,
                }),
                "lxmf.inbound.accepted",
                Messages,
                Info,
                Normal,
                [Destination, Method, ByteLength],
                "message",
                "Message received",
                "standard"
            ),
            projection_case!(
                producer::lxmf_delivery_failed(producer::LxmfDeliveryFailed {
                    message: message(16),
                    destination: destination(12),
                    link: Some(link(4)),
                    reason: DeliveryFailureReason::ProofTimedOut,
                    correlation_id: CorrelationId::from_bytes([4; 16]),
                }),
                "lxmf.delivery.failed",
                Messages,
                Error,
                Normal,
                [Message, Destination, Reason, Link],
                "error",
                "Message delivery failed",
                "essential"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::ServiceStarted),
                "lxst.service.started",
                Calls,
                Info,
                Normal,
                [],
                "lxst",
                "LXST voice service started",
                "standard"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::ServiceStopped),
                "lxst.service.stopped",
                Calls,
                Info,
                Normal,
                [],
                "lxst",
                "LXST voice service stopped",
                "standard"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::ServiceFailed {
                    reason: LxstCallReason::ServiceError,
                }),
                "lxst.service.failed",
                Calls,
                Error,
                Normal,
                [Reason],
                "error",
                "LXST voice service unavailable",
                "essential"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::IncomingRinging {
                    peer: identity(1),
                    link: link(5),
                }),
                "lxst.call.ringing",
                Calls,
                Info,
                Normal,
                [Identity, Link],
                "lxst",
                "Incoming LXST call",
                "standard"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::PathPending { peer: identity(2) }),
                "lxst.call.path_pending",
                Calls,
                Info,
                Normal,
                [Identity],
                "lxst",
                "Resolving LXST call path",
                "standard"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::LinkRequested {
                    peer: identity(3),
                    link: link(6),
                }),
                "lxst.call.link_requested",
                Calls,
                Info,
                Normal,
                [Identity, Link],
                "lxst",
                "LXST call link requested",
                "standard"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::Ended { link: link(7) }),
                "lxst.call.ended",
                Calls,
                Info,
                Normal,
                [Link],
                "lxst",
                "LXST call ended",
                "standard"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::Rejected { link: link(8) }),
                "lxst.call.rejected",
                Calls,
                Warning,
                Normal,
                [Link, Reason],
                "lxst",
                "LXST call rejected",
                "essential"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::Failed {
                    peer: None,
                    link: Some(link(9)),
                    reason: LxstCallReason::Busy,
                }),
                "lxst.call.failed",
                Calls,
                Error,
                Normal,
                [Link, Reason],
                "error",
                "LXST call failed",
                "essential"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::Failed {
                    peer: Some(identity(4)),
                    link: None,
                    reason: LxstCallReason::ServiceError,
                }),
                "lxst.call.failed",
                Calls,
                Error,
                Normal,
                [Identity, Reason],
                "error",
                "LXST call failed",
                "essential"
            ),
            projection_case!(
                producer::lxst_activity(LxstTransition::MediaWarning {
                    reason: LxstCallReason::MediaError,
                }),
                "lxst.media.warning",
                Calls,
                Warning,
                Normal,
                [Reason],
                "lxst",
                "LXST call media warning",
                "essential"
            ),
        ];

        for (index, case) in cases.into_iter().enumerate() {
            let draft = case
                .event
                .into_unstamped_draft()
                .unwrap_or_else(|error| panic!("{} rejected: {error}", case.kind));
            assert_eq!(draft.area(), case.area, "{} area", case.kind);
            assert_eq!(draft.severity(), case.severity, "{} severity", case.kind);
            assert!(
                draft.capture_scope() == case.scope,
                "{} capture scope",
                case.kind
            );

            let profile = match case.scope {
                CaptureScope::Normal => CaptureProfile::Normal,
                CaptureScope::TraceOnly => CaptureProfile::Trace,
            };
            let mut privacy = CapturePrivacy::random();
            let validated = draft
                .validate(DraftContext {
                    capture_session: privacy.capture_session().to_string(),
                    capture_generation: 1,
                    capture_profile: profile,
                })
                .unwrap_or_else(|error| panic!("{} validation failed: {error}", case.kind));
            let masked = privacy
                .seal(ReadyDraft(validated), index as u64 + 1)
                .unwrap_or_else(|error| panic!("{} sealing failed: {error}", case.kind))
                .masked();

            assert_eq!(masked.kind(), case.kind);
            assert_eq!(masked.area(), case.area, "{} masked area", case.kind);
            assert_eq!(
                masked.severity(),
                case.severity,
                "{} masked severity",
                case.kind
            );
            assert_eq!(
                masked.direction,
                expected_direction(case.kind),
                "{} direction",
                case.kind
            );
            assert_eq!(
                masked.outcome(),
                expected_outcome(case.kind),
                "{} outcome",
                case.kind
            );
            assert_eq!(
                masked.capture_profile(),
                profile,
                "{} capture profile",
                case.kind
            );
            let actual_keys = masked
                .attributes
                .iter()
                .map(|attribute| attribute.key)
                .collect::<Vec<_>>();
            assert_eq!(
                actual_keys, case.attribute_keys,
                "{} attribute contract",
                case.kind
            );

            let legacy = LegacyActivityProjection::from_masked(&masked)
                .unwrap_or_else(|| panic!("{} missing compatibility row", case.kind));
            assert_eq!(legacy.event_type, case.legacy_type, "{} type", case.kind);
            assert_eq!(legacy.message, case.legacy_message, "{} message", case.kind);
            assert_eq!(legacy.level, case.legacy_level, "{} level", case.kind);
            assert!(
                legacy.detail.chars().count() <= LEGACY_DETAIL_MAX_CHARS,
                "{} detail width",
                case.kind
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compatibility_rows_are_allowlisted_masked_and_session_addressable() {
        let emitter = Arc::new(RecordingEmitter::default());
        let recorder =
            ActivityRecorder::with_batch_sink(Arc::new(EmitterBatchSink::new(emitter.clone())));
        let session = recorder
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();
        wait_until(|| !emitter.payloads(ACTIVITY_BATCH_EVENT).is_empty()).await;
        assert!(emitter.payloads(LEGACY_ACTIVITY_EVENT).is_empty());

        let raw_destination = [0xab; 16];
        assert_eq!(
            recorder.record_event(|| {
                Ok(producer::rns_path_requested(producer::RnsPathRequested {
                    destination: Some(producer::DestinationHash::new(raw_destination)),
                    count: Some(3),
                    method: producer::PathRequestMethod::Manual,
                }))
            }),
            ActivityRecordOutcome::Accepted
        );
        let raw_endpoint = "private-node.example:4242";
        assert_eq!(
            recorder.record_event(|| {
                Ok(producer::interface_activity(producer::InterfaceActivity {
                    class: producer::InterfaceClass::TcpClient,
                    transition: producer::InterfaceTransition::Configured,
                    endpoint: Some(producer::TcpEndpoint::new(raw_endpoint.to_string())?),
                }))
            }),
            ActivityRecordOutcome::Accepted
        );
        assert_eq!(
            recorder.record_event(|| {
                Ok(producer::lxst_activity(
                    producer::LxstTransition::ServiceStarted,
                ))
            }),
            ActivityRecordOutcome::Accepted
        );
        wait_until(|| emitter.payloads(LEGACY_ACTIVITY_EVENT).len() >= 3).await;

        let rows = emitter.payloads(LEGACY_ACTIVITY_EVENT);
        let path = rows
            .iter()
            .find(|row| row["message"] == "Path requested")
            .unwrap();
        assert_eq!(path["type"], "path");
        assert_eq!(path["level"], "standard");
        assert_eq!(path["severity"], "info");
        assert_eq!(path["capture_session"], session);
        assert!(path["sequence"].as_str().is_some());
        assert!(path["capture_generation"].as_str().is_some());
        assert!(
            path["detail"]
                .as_str()
                .is_some_and(|detail| detail.chars().count() <= LEGACY_DETAIL_MAX_CHARS)
        );
        let lxst = rows
            .iter()
            .find(|row| row["message"] == "LXST voice service started")
            .unwrap();
        assert_eq!(lxst["type"], "lxst");

        let payload_text = emitter.all_payload_text();
        assert!(!payload_text.contains(&hex::encode(raw_destination)));
        assert!(!payload_text.contains(raw_endpoint));

        let ActivityReplayResultV1::Page { page } =
            recorder.replay(session, None, 50, 64 * 1024).await.unwrap()
        else {
            panic!("active session should replay");
        };
        let mut fixture = page
            .events()
            .iter()
            .find(|event| event.kind() == "rns.path.requested")
            .unwrap()
            .clone();
        fixture.count = 7;
        let counted = LegacyActivityProjection::from_masked(&fixture).unwrap();
        assert!(counted.detail.contains("×7"));
        assert!(counted.detail.chars().count() <= LEGACY_DETAIL_MAX_CHARS);
        assert!(counted.detail.matches(" · ").count() <= 1);
        fixture.count = 1;

        // Every kind migrated in Stage 2A has an intentional static summary;
        // unknown producer prose can never fall through into compatibility.
        for kind in [
            "app.runtime.started",
            "interface.configured",
            "interface.connecting",
            "interface.cancelled",
            "interface.online",
            "interface.offline",
            "interface.degraded",
            "interface.paused",
            "interface.removed",
            "interface.failed",
            "interface.timed_out",
            "rns.path.requested",
            "rns.path.discovered",
            "rns.path.observed",
            "rns.announce.sent",
            "rns.announce.failed",
            "rns.announce.held",
            "rns.announce.observed",
            "rns.announce.ingress_burst_started",
            "rns.announce.ingress_burst_cleared",
            "rns.announce.suppressed",
            "lxmf.delivery.queued",
            "lxmf.delivery.submission_failed",
            "lxmf.delivery.method_selected",
            "lxmf.delivery.path_pending",
            "lxmf.delivery.link_establishing",
            "lxmf.delivery.link_ready",
            "lxmf.delivery.link_reused",
            "lxmf.delivery.direct_pending",
            "lxmf.delivery.resource_started",
            "lxmf.delivery.progress",
            "lxmf.delivery.awaiting_proof",
            "lxmf.delivery.delivered",
            "lxmf.delivery.rejected",
            "lxmf.delivery.failed",
            "lxmf.propagation.started",
            "lxmf.propagation.succeeded",
            "lxmf.propagation.failed",
            "lxmf.inbound.accepted",
            "lxst.service.started",
            "lxst.service.stopped",
            "lxst.service.failed",
            "lxst.call.path_pending",
            "lxst.call.link_requested",
            "lxst.call.ringing",
            "lxst.call.ended",
            "lxst.call.failed",
            "lxst.media.warning",
        ] {
            fixture.kind = kind.to_string();
            assert!(
                LegacyActivityProjection::from_masked(&fixture).is_some(),
                "missing compatibility projection for {kind}"
            );
        }

        fixture.kind = "diagnostics.capture_started".to_string();
        assert!(LegacyActivityProjection::from_masked(&fixture).is_none());
        fixture.kind = "unreviewed.free_form".to_string();
        assert!(LegacyActivityProjection::from_masked(&fixture).is_none());

        fixture.kind = "app.runtime.ready".to_string();
        fixture.attributes.clear();
        fixture.count = 3;
        let coalesced =
            serde_json::to_value(LegacyActivityProjection::from_masked(&fixture).unwrap()).unwrap();
        assert_eq!(coalesced["detail"], "×3");

        fixture.kind = "rns.path.observed".to_string();
        fixture.severity = ActivitySeverity::Info;
        let ambient = LegacyActivityProjection::from_masked(&fixture).unwrap();
        assert_eq!(ambient.level, "detailed");
        fixture.kind = "rns.announce.failed".to_string();
        fixture.severity = ActivitySeverity::Error;
        let failed = LegacyActivityProjection::from_masked(&fixture).unwrap();
        assert_eq!(failed.event_type, "error");
        assert_eq!(failed.level, "essential");

        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_compatibility_rows_do_not_count_as_typed_ipc_failures() {
        let emitter = Arc::new(RecordingEmitter::default());
        emitter.reject_legacy.store(true, Ordering::Relaxed);
        let recorder =
            ActivityRecorder::with_batch_sink(Arc::new(EmitterBatchSink::new(emitter.clone())));
        recorder.start().await.unwrap();
        assert_eq!(
            recorder.record_event(|| {
                Ok(producer::app_runtime(producer::AppRuntimeTransition::Ready))
            }),
            ActivityRecordOutcome::Accepted
        );
        wait_until(|| emitter.legacy_attempts.load(Ordering::Relaxed) > 0).await;
        assert_eq!(recorder.status().counters().ipc_failure(), "0");
        assert!(emitter.payloads(ACTIVITY_BATCH_EVENT).iter().any(|batch| {
            batch["events"].as_array().is_some_and(|events| {
                events
                    .iter()
                    .any(|event| event["kind"] == "app.runtime.ready")
            })
        }));
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_typed_batches_still_count_when_compatibility_succeeds() {
        let emitter = Arc::new(RecordingEmitter::default());
        let recorder =
            ActivityRecorder::with_batch_sink(Arc::new(EmitterBatchSink::new(emitter.clone())));
        recorder.start().await.unwrap();
        emitter.reject_typed.store(true, Ordering::Relaxed);
        assert_eq!(
            recorder.record_event(|| {
                Ok(producer::app_runtime(producer::AppRuntimeTransition::Ready))
            }),
            ActivityRecordOutcome::Accepted
        );
        wait_until(|| {
            recorder.status().counters().ipc_failure() != "0"
                && emitter
                    .payloads(LEGACY_ACTIVITY_EVENT)
                    .iter()
                    .any(|row| row["message"] == "Ratspeak runtime ready")
        })
        .await;
        recorder.shutdown().await.unwrap();
    }
}
