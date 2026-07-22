//! Masked, versioned Activity wire types.
//!
//! Every type in this file is safe to serialize. Raw identifiers, endpoints,
//! navigation references, payloads, and human-authored text deliberately have
//! no representation here.

#![allow(
    dead_code,
    reason = "reviewed wire/catalog variants precede Stage 2 producer migration"
)]

use serde::Serialize;

pub const ACTIVITY_SCHEMA_VERSION: u8 = 1;
pub const MAX_ATTRIBUTES: usize = 32;
pub const MAX_ENCODED_EVENT_BYTES: usize = 4 * 1024;
pub const MAX_STRING_FIELD_BYTES: usize = 256;

/// A `u64` that is always serialized as a decimal string so JavaScript never
/// rounds a sequence, generation, cursor, or cumulative counter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DecimalU64(u64);

impl DecimalU64 {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Serialize for DecimalU64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityArea {
    Network,
    Interfaces,
    Links,
    Messages,
    Channels,
    Calls,
    Apps,
    Ratspeak,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivitySeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureProfile {
    Normal,
    Trace,
}

impl CaptureProfile {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Trace => "trace",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityDirection {
    Local,
    Inbound,
    Outbound,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityOutcome {
    Started,
    Progress,
    Success,
    Degraded,
    Rejected,
    Failed,
    TimedOut,
    Dropped,
    None,
}

impl ActivityOutcome {
    pub(crate) const fn prohibits_coalescing(self) -> bool {
        matches!(
            self,
            Self::Rejected | Self::Failed | Self::TimedOut | Self::Dropped
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityAttributeKey {
    Attempts,
    ByteLength,
    Capability,
    Count,
    Destination,
    DroppedCount,
    Duplicate,
    DurationMs,
    Endpoint,
    EvictedCount,
    Hops,
    Hub,
    Identity,
    InterfaceClass,
    Link,
    Mdu,
    Message,
    Method,
    Percent,
    Profile,
    ProtocolVersion,
    QueueCount,
    Reason,
    Room,
    RttMs,
    Session,
    State,
    TimeSpanMs,
    Validation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierKind {
    Destination,
    Hub,
    Identity,
    Link,
    Message,
    Peer,
    Room,
    Session,
}

impl IdentifierKind {
    pub(crate) const fn domain_id(self) -> u8 {
        match self {
            Self::Destination => 1,
            Self::Hub => 2,
            Self::Identity => 3,
            Self::Link => 4,
            Self::Message => 5,
            Self::Peer => 6,
            Self::Room => 7,
            Self::Session => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointClass {
    Bluetooth,
    Local,
    Serial,
    Tcp,
    Udp,
    Unknown,
}

impl EndpointClass {
    pub(crate) const fn domain_id(self) -> u8 {
        match self {
            Self::Bluetooth => 1,
            Self::Local => 2,
            Self::Serial => 3,
            Self::Tcp => 4,
            Self::Udp => 5,
            Self::Unknown => 6,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct MaskedIdentifierV1 {
    pub(super) kind: IdentifierKind,
    /// Domain-separated, capture-session-scoped keyed pseudonym.
    pub(super) pseudonym: String,
    /// Friendly first-seen ordinal while the bounded presentation table has
    /// capacity. Safe copy uses `pseudonym`, never this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) ordinal: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct MaskedEndpointV1 {
    pub(super) class: EndpointClass,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(super) enum ActivityValueV1 {
    Boolean(bool),
    Code(String),
    Endpoint(MaskedEndpointV1),
    Identifier(MaskedIdentifierV1),
    Signed(i64),
    Unsigned(u64),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct ActivityAttributeV1 {
    pub(super) key: ActivityAttributeKey,
    pub(super) value: ActivityValueV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct SafeCopyIdentifierV1 {
    kind: IdentifierKind,
    pseudonym: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct SafeCopyEndpointV1 {
    class: EndpointClass,
    pseudonym: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum SafeCopyValueV1 {
    Boolean(bool),
    Code(String),
    Endpoint(SafeCopyEndpointV1),
    Identifier(SafeCopyIdentifierV1),
    Signed(i64),
    Unsigned(u64),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct SafeCopyAttributeV1 {
    key: ActivityAttributeKey,
    value: SafeCopyValueV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SafeCopyError {
    MissingEndpointPseudonym,
}

impl SafeCopyAttributeV1 {
    fn from_masked(
        attribute: &ActivityAttributeV1,
        endpoint_pseudonym: Option<&str>,
    ) -> Result<Self, SafeCopyError> {
        let value = match &attribute.value {
            ActivityValueV1::Boolean(value) => SafeCopyValueV1::Boolean(*value),
            ActivityValueV1::Code(value) => SafeCopyValueV1::Code(value.clone()),
            ActivityValueV1::Endpoint(value) => SafeCopyValueV1::Endpoint(SafeCopyEndpointV1 {
                class: value.class,
                pseudonym: endpoint_pseudonym
                    .ok_or(SafeCopyError::MissingEndpointPseudonym)?
                    .to_string(),
            }),
            ActivityValueV1::Identifier(value) => {
                SafeCopyValueV1::Identifier(SafeCopyIdentifierV1 {
                    kind: value.kind,
                    pseudonym: value.pseudonym.clone(),
                })
            }
            ActivityValueV1::Signed(value) => SafeCopyValueV1::Signed(*value),
            ActivityValueV1::Unsigned(value) => SafeCopyValueV1::Unsigned(*value),
        };
        Ok(Self {
            key: attribute.key,
            value,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActivityEventV1 {
    pub(super) version: u8,
    pub(super) sequence: DecimalU64,
    pub(super) capture_session: String,
    pub(super) capture_generation: DecimalU64,
    pub(super) timestamp_unix_ms: u64,
    pub(super) elapsed_ms: u64,
    pub(super) area: ActivityArea,
    pub(super) kind: String,
    pub(super) severity: ActivitySeverity,
    pub(super) capture_profile: CaptureProfile,
    pub(super) direction: ActivityDirection,
    pub(super) outcome: ActivityOutcome,
    pub(super) summary_code: String,
    pub(super) attributes: Vec<ActivityAttributeV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parent_sequence: Option<DecimalU64>,
    pub(super) count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) first_timestamp_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) last_timestamp_ms: Option<u64>,
}

impl ActivityEventV1 {
    pub const fn sequence(&self) -> u64 {
        self.sequence.get()
    }

    pub fn capture_session(&self) -> &str {
        &self.capture_session
    }

    pub const fn capture_generation(&self) -> u64 {
        self.capture_generation.get()
    }

    pub const fn area(&self) -> ActivityArea {
        self.area
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub const fn severity(&self) -> ActivitySeverity {
        self.severity
    }

    pub const fn capture_profile(&self) -> CaptureProfile {
        self.capture_profile
    }

    pub const fn outcome(&self) -> ActivityOutcome {
        self.outcome
    }

    pub(super) fn safe_copy_with_endpoint_pseudonyms<'a>(
        &self,
        mut endpoint_pseudonym: impl FnMut(usize) -> Option<&'a str>,
    ) -> Result<SafeCopyEventV1, SafeCopyError> {
        Ok(SafeCopyEventV1 {
            version: self.version,
            sequence: self.sequence,
            capture_session: self.capture_session.clone(),
            capture_generation: self.capture_generation,
            timestamp_unix_ms: self.timestamp_unix_ms,
            elapsed_ms: self.elapsed_ms,
            area: self.area,
            kind: self.kind.clone(),
            severity: self.severity,
            capture_profile: self.capture_profile,
            direction: self.direction,
            outcome: self.outcome,
            summary_code: self.summary_code.clone(),
            attributes: self
                .attributes
                .iter()
                .enumerate()
                .map(|(index, attribute)| {
                    SafeCopyAttributeV1::from_masked(attribute, endpoint_pseudonym(index))
                })
                .collect::<Result<Vec<_>, _>>()?,
            correlation_id: self.correlation_id.clone(),
            parent_sequence: self.parent_sequence,
            count: self.count,
            first_timestamp_ms: self.first_timestamp_ms,
            last_timestamp_ms: self.last_timestamp_ms,
        })
    }
}

/// A distinct serialization type prevents future safe-copy code from ever
/// accepting the raw-bearing stored event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SafeCopyEventV1 {
    version: u8,
    sequence: DecimalU64,
    capture_session: String,
    capture_generation: DecimalU64,
    timestamp_unix_ms: u64,
    elapsed_ms: u64,
    area: ActivityArea,
    kind: String,
    severity: ActivitySeverity,
    capture_profile: CaptureProfile,
    direction: ActivityDirection,
    outcome: ActivityOutcome,
    summary_code: String,
    attributes: Vec<SafeCopyAttributeV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_sequence: Option<DecimalU64>,
    count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_timestamp_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_timestamp_ms: Option<u64>,
}

/// Allowlisted catalog code. The fields are private so producer modules cannot
/// invent a code, area, or fallback-summary combination.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ActivityKindCode {
    code: &'static str,
    area: ActivityArea,
    summary_code: &'static str,
    capture_scope: CaptureScope,
    rate_domain: RateDomain,
    ambient: bool,
}

impl ActivityKindCode {
    const fn new(
        code: &'static str,
        area: ActivityArea,
        summary_code: &'static str,
        capture_scope: CaptureScope,
        rate_domain: RateDomain,
        ambient: bool,
    ) -> Self {
        Self {
            code,
            area,
            summary_code,
            capture_scope,
            rate_domain,
            ambient,
        }
    }

    pub(super) const fn code(self) -> &'static str {
        self.code
    }

    pub(super) const fn area(self) -> ActivityArea {
        self.area
    }

    pub(super) const fn summary_code(self) -> &'static str {
        self.summary_code
    }

    pub(super) const fn capture_scope(self) -> CaptureScope {
        self.capture_scope
    }

    pub(super) const fn rate_domain(self) -> RateDomain {
        self.rate_domain
    }

    pub(super) const fn ambient(self) -> bool {
        self.ambient
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum CaptureScope {
    Normal,
    TraceOnly,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum RateDomain {
    Network,
    Interfaces,
    Links,
    Messages,
    Channels,
    Calls,
    Apps,
    Ratspeak,
}

impl RateDomain {
    pub(super) const COUNT: usize = 8;

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Network => 0,
            Self::Interfaces => 1,
            Self::Links => 2,
            Self::Messages => 3,
            Self::Channels => 4,
            Self::Calls => 5,
            Self::Apps => 6,
            Self::Ratspeak => 7,
        }
    }

    const fn from_area(area: ActivityArea) -> Self {
        match area {
            ActivityArea::Network => Self::Network,
            ActivityArea::Interfaces => Self::Interfaces,
            ActivityArea::Links => Self::Links,
            ActivityArea::Messages => Self::Messages,
            ActivityArea::Channels => Self::Channels,
            ActivityArea::Calls => Self::Calls,
            ActivityArea::Apps => Self::Apps,
            ActivityArea::Ratspeak => Self::Ratspeak,
        }
    }
}

macro_rules! kind {
    ($name:ident, $code:literal, $area:ident) => {
        pub(in crate::activity) const $name: ActivityKindCode = ActivityKindCode::new(
            $code,
            ActivityArea::$area,
            $code,
            CaptureScope::Normal,
            RateDomain::from_area(ActivityArea::$area),
            false,
        );
    };
}

macro_rules! trace_ambient_kind {
    ($name:ident, $code:literal, $area:ident) => {
        pub(in crate::activity) const $name: ActivityKindCode = ActivityKindCode::new(
            $code,
            ActivityArea::$area,
            $code,
            CaptureScope::TraceOnly,
            RateDomain::from_area(ActivityArea::$area),
            true,
        );
    };
}

/// The central stable-code catalog. Only `activity::catalog` can name this
/// private module; producer modules receive event-specific constructors.
pub(super) mod kinds {
    use super::*;

    kind!(
        DIAGNOSTICS_CAPTURE_STARTED,
        "diagnostics.capture_started",
        Ratspeak
    );
    kind!(
        DIAGNOSTICS_CAPTURE_STOPPED,
        "diagnostics.capture_stopped",
        Ratspeak
    );
    kind!(
        DIAGNOSTICS_CAPTURE_RESUMED,
        "diagnostics.capture_resumed",
        Ratspeak
    );
    kind!(
        DIAGNOSTICS_CAPTURE_CLEARED,
        "diagnostics.capture_cleared",
        Ratspeak
    );
    kind!(
        DIAGNOSTICS_PROFILE_CHANGED,
        "diagnostics.profile_changed",
        Ratspeak
    );
    kind!(DIAGNOSTICS_DROPPED, "diagnostics.dropped", Ratspeak);
    kind!(DIAGNOSTICS_EVICTED, "diagnostics.evicted", Ratspeak);
    kind!(
        DIAGNOSTICS_WORKER_RECOVERED,
        "diagnostics.worker_recovered",
        Ratspeak
    );

    kind!(APP_RUNTIME_STARTED, "app.runtime.started", Ratspeak);
    kind!(APP_RUNTIME_READY, "app.runtime.ready", Ratspeak);
    kind!(APP_RUNTIME_UNAVAILABLE, "app.runtime.unavailable", Ratspeak);
    kind!(APP_RUNTIME_STOPPED, "app.runtime.stopped", Ratspeak);
    kind!(STORAGE_DB_FAILED, "storage.db.failed", Ratspeak);
    kind!(IPC_FAILED, "ipc.failed", Ratspeak);

    kind!(RNS_TRANSPORT_STARTED, "rns.transport.started", Network);
    kind!(RNS_TRANSPORT_READY, "rns.transport.ready", Network);
    kind!(
        RNS_TRANSPORT_UNAVAILABLE,
        "rns.transport.unavailable",
        Network
    );
    kind!(RNS_TRANSPORT_STOPPED, "rns.transport.stopped", Network);
    kind!(RNS_PATH_REQUESTED, "rns.path.requested", Network);
    kind!(RNS_PATH_DISCOVERED, "rns.path.discovered", Network);
    kind!(RNS_PATH_TIMED_OUT, "rns.path.timed_out", Network);
    kind!(RNS_ANNOUNCE_SENT, "rns.announce.sent", Network);
    kind!(RNS_ANNOUNCE_FAILED, "rns.announce.failed", Network);
    kind!(RNS_ANNOUNCE_HELD, "rns.announce.held", Network);
    trace_ambient_kind!(RNS_ANNOUNCE_OBSERVED, "rns.announce.observed", Network);
    kind!(RNS_SECURITY_DROPPED, "rns.security.dropped", Network);
    trace_ambient_kind!(RNS_PACKET_SAMPLED, "rns.packet.sampled", Network);

    kind!(INTERFACE_CONFIGURED, "interface.configured", Interfaces);
    kind!(INTERFACE_CONNECTING, "interface.connecting", Interfaces);
    kind!(INTERFACE_ONLINE, "interface.online", Interfaces);
    kind!(INTERFACE_OFFLINE, "interface.offline", Interfaces);
    kind!(INTERFACE_PAUSED, "interface.paused", Interfaces);
    kind!(INTERFACE_REMOVED, "interface.removed", Interfaces);
    kind!(INTERFACE_FAILED, "interface.failed", Interfaces);

    kind!(RNS_LINK_REQUESTED, "rns.link.requested", Links);
    kind!(RNS_LINK_AUTHENTICATED, "rns.link.authenticated", Links);
    kind!(RNS_LINK_IDENTIFIED, "rns.link.identified", Links);
    kind!(RNS_LINK_STALE, "rns.link.stale", Links);
    kind!(RNS_LINK_RECOVERED, "rns.link.recovered", Links);
    kind!(RNS_LINK_CLOSED, "rns.link.closed", Links);
    kind!(RESOURCE_STARTED, "resource.started", Links);
    kind!(RESOURCE_PROGRESS, "resource.progress", Links);
    kind!(RESOURCE_SUCCEEDED, "resource.succeeded", Links);
    kind!(RESOURCE_FAILED, "resource.failed", Links);

    kind!(LXMF_DELIVERY_QUEUED, "lxmf.delivery.queued", Messages);
    kind!(
        LXMF_DELIVERY_METHOD_SELECTED,
        "lxmf.delivery.method_selected",
        Messages
    );
    kind!(
        LXMF_DELIVERY_PATH_PENDING,
        "lxmf.delivery.path_pending",
        Messages
    );
    kind!(
        LXMF_DELIVERY_LINK_ESTABLISHING,
        "lxmf.delivery.link_establishing",
        Messages
    );
    kind!(
        LXMF_DELIVERY_LINK_REUSED,
        "lxmf.delivery.link_reused",
        Messages
    );
    kind!(
        LXMF_DELIVERY_PACKET_STARTED,
        "lxmf.delivery.packet_started",
        Messages
    );
    kind!(
        LXMF_DELIVERY_RESOURCE_STARTED,
        "lxmf.delivery.resource_started",
        Messages
    );
    kind!(LXMF_DELIVERY_PROGRESS, "lxmf.delivery.progress", Messages);
    kind!(
        LXMF_DELIVERY_AWAITING_PROOF,
        "lxmf.delivery.awaiting_proof",
        Messages
    );
    kind!(LXMF_DELIVERY_DELIVERED, "lxmf.delivery.delivered", Messages);
    kind!(LXMF_DELIVERY_REJECTED, "lxmf.delivery.rejected", Messages);
    kind!(LXMF_DELIVERY_DEFERRED, "lxmf.delivery.deferred", Messages);
    kind!(LXMF_DELIVERY_RETRYING, "lxmf.delivery.retrying", Messages);
    kind!(LXMF_DELIVERY_FAILED, "lxmf.delivery.failed", Messages);
    kind!(
        LXMF_PROPAGATION_STARTED,
        "lxmf.propagation.started",
        Messages
    );
    kind!(
        LXMF_PROPAGATION_SUCCEEDED,
        "lxmf.propagation.succeeded",
        Messages
    );
    kind!(LXMF_PROPAGATION_FAILED, "lxmf.propagation.failed", Messages);
    kind!(LXMF_INBOUND_ACCEPTED, "lxmf.inbound.accepted", Messages);
    kind!(LXMF_INBOUND_REJECTED, "lxmf.inbound.rejected", Messages);

    kind!(
        CHANNELS_SESSION_CONNECT_REQUESTED,
        "channels.session.connect_requested",
        Channels
    );
    kind!(
        CHANNELS_SESSION_CANCELLED,
        "channels.session.cancelled",
        Channels
    );
    kind!(
        CHANNELS_SESSION_PATH_REQUESTED,
        "channels.session.path_requested",
        Channels
    );
    kind!(
        CHANNELS_SESSION_PATH_DISCOVERED,
        "channels.session.path_discovered",
        Channels
    );
    kind!(
        CHANNELS_SESSION_PATH_TIMED_OUT,
        "channels.session.path_timed_out",
        Channels
    );
    kind!(
        CHANNELS_SESSION_LINK_REQUESTED,
        "channels.session.link_requested",
        Channels
    );
    kind!(
        CHANNELS_SESSION_LINK_AUTHENTICATED,
        "channels.session.link_authenticated",
        Channels
    );
    kind!(
        CHANNELS_SESSION_LINK_IDENTIFIED,
        "channels.session.link_identified",
        Channels
    );
    kind!(
        CHANNELS_SESSION_HELLO_SENT,
        "channels.session.hello_sent",
        Channels
    );
    kind!(
        CHANNELS_SESSION_WELCOME_RECEIVED,
        "channels.session.welcome_received",
        Channels
    );
    kind!(
        CHANNELS_SESSION_WELCOME_REJECTED,
        "channels.session.welcome_rejected",
        Channels
    );
    kind!(
        CHANNELS_SESSION_NEGOTIATED,
        "channels.session.negotiated",
        Channels
    );
    kind!(
        CHANNELS_SESSION_GREETING_OBSERVED,
        "channels.session.greeting_observed",
        Channels
    );
    kind!(CHANNELS_SESSION_STALE, "channels.session.stale", Channels);
    kind!(
        CHANNELS_SESSION_RECOVERED,
        "channels.session.recovered",
        Channels
    );
    kind!(CHANNELS_SESSION_CLOSED, "channels.session.closed", Channels);
    kind!(
        CHANNELS_ROOM_JOIN_REQUESTED,
        "channels.room.join_requested",
        Channels
    );
    kind!(CHANNELS_ROOM_JOINED, "channels.room.joined", Channels);
    kind!(
        CHANNELS_ROOM_JOIN_REJECTED,
        "channels.room.join_rejected",
        Channels
    );
    kind!(
        CHANNELS_ROOM_JOIN_TIMED_OUT,
        "channels.room.join_timed_out",
        Channels
    );
    kind!(
        CHANNELS_ROOM_JOIN_CANCELLED,
        "channels.room.join_cancelled",
        Channels
    );
    kind!(
        CHANNELS_ROOM_PART_REQUESTED,
        "channels.room.part_requested",
        Channels
    );
    kind!(CHANNELS_ROOM_PARTED, "channels.room.parted", Channels);
    kind!(
        CHANNELS_ROOM_PART_TIMED_OUT,
        "channels.room.part_timed_out",
        Channels
    );
    kind!(CHANNELS_ENVELOPE_SENT, "channels.envelope.sent", Channels);
    kind!(
        CHANNELS_ENVELOPE_RECEIVED,
        "channels.envelope.received",
        Channels
    );
    kind!(
        CHANNELS_ENVELOPE_REJECTED,
        "channels.envelope.rejected",
        Channels
    );
    trace_ambient_kind!(CHANNELS_HEARTBEAT_PING, "channels.heartbeat.ping", Channels);
    trace_ambient_kind!(CHANNELS_HEARTBEAT_PONG, "channels.heartbeat.pong", Channels);
    kind!(
        CHANNELS_HEARTBEAT_TIMED_OUT,
        "channels.heartbeat.timed_out",
        Channels
    );

    kind!(LXST_SERVICE_STARTED, "lxst.service.started", Calls);
    kind!(LXST_SERVICE_STOPPED, "lxst.service.stopped", Calls);
    kind!(LXST_CALL_RINGING, "lxst.call.ringing", Calls);
    kind!(LXST_CALL_ANSWERED, "lxst.call.answered", Calls);
    kind!(LXST_CALL_ENDED, "lxst.call.ended", Calls);
    kind!(LXST_CALL_REJECTED, "lxst.call.rejected", Calls);
    kind!(LXST_CALL_FAILED, "lxst.call.failed", Calls);
    kind!(LXST_MEDIA_STARTED, "lxst.media.started", Calls);
    kind!(LXST_MEDIA_STOPPED, "lxst.media.stopped", Calls);
    kind!(LXST_MEDIA_WARNING, "lxst.media.warning", Calls);

    kind!(LRGP_ACTION_STARTED, "lrgp.action.started", Apps);
    kind!(LRGP_ACTION_SUCCEEDED, "lrgp.action.succeeded", Apps);
    kind!(LRGP_ACTION_REJECTED, "lrgp.action.rejected", Apps);
    kind!(LRGP_ACTION_FAILED, "lrgp.action.failed", Apps);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_u64_never_serializes_as_a_json_number() {
        assert_eq!(
            serde_json::to_string(&DecimalU64::new(u64::MAX)).unwrap(),
            format!("\"{}\"", u64::MAX)
        );
    }

    #[test]
    fn catalog_derives_area_and_summary_from_one_allowlisted_code() {
        let kind = kinds::CHANNELS_ROOM_JOINED;
        assert_eq!(kind.code(), "channels.room.joined");
        assert_eq!(kind.area(), ActivityArea::Channels);
        assert_eq!(kind.summary_code(), "channels.room.joined");
    }
}
