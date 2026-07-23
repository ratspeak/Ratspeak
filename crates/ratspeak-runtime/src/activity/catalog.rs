//! Sealed, event-specific Activity constructors.
//!
//! Producer modules call functions in this catalog with concrete domain
//! inputs. They cannot select a classification, add an arbitrary attribute,
//! or supply a free-form event/summary code.

#![allow(
    dead_code,
    reason = "the reviewed catalog includes variants reserved for later semantic coverage"
)]

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::classified::{
    ActivityDraft, ActivityRejectReason, ClassifiedEndpoint, CoalescingPolicy, CorrelationId,
    ExactValue, NavigationAction,
};
use super::schema::{
    ActivityAttributeKey, ActivityDirection, ActivityOutcome, ActivitySeverity, EndpointClass,
    IdentifierKind, RateDomain, kinds,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ObservationTime {
    unix_ms: u64,
    elapsed_ms: u64,
}

impl ObservationTime {
    pub(super) const fn new(unix_ms: u64, elapsed_ms: u64) -> Self {
        Self {
            unix_ms,
            elapsed_ms,
        }
    }

    pub(super) const fn unix_ms(self) -> u64 {
        self.unix_ms
    }

    pub(super) const fn elapsed_ms(self) -> u64 {
        self.elapsed_ms
    }

    pub(super) const fn unstamped() -> Self {
        Self::new(0, 0)
    }
}

/// Recorder-owned wall/monotonic observation clock. `ObservationTime` never
/// leaves the private Activity implementation, so domain producers cannot
/// fabricate timestamps or choose another clock domain.
pub(super) trait ActivityClock: Send + Sync {
    fn observe(&self) -> ObservationTime;
}

pub(super) struct SystemActivityClock {
    origin: Instant,
}

impl SystemActivityClock {
    pub(super) fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl ActivityClock for SystemActivityClock {
    fn observe(&self) -> ObservationTime {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let elapsed_ms = self.origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        ObservationTime::new(unix_ms, elapsed_ms)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DestinationHash([u8; 16]);

impl DestinationHash {
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn from_hex(value: &str) -> Result<Self, ActivityRejectReason> {
        decode_fixed_hex(value).map(Self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MessageId([u8; 32]);

impl MessageId {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn from_hex(value: &str) -> Result<Self, ActivityRejectReason> {
        decode_fixed_hex(value).map(Self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LinkId([u8; 16]);

impl LinkId {
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn from_hex(value: &str) -> Result<Self, ActivityRejectReason> {
        decode_fixed_hex(value).map(Self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct IdentityHash([u8; 16]);

impl IdentityHash {
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn from_hex(value: &str) -> Result<Self, ActivityRejectReason> {
        decode_fixed_hex(value).map(Self)
    }
}

fn decode_fixed_hex<const N: usize>(value: &str) -> Result<[u8; N], ActivityRejectReason> {
    if value.len() != N.saturating_mul(2) {
        return Err(ActivityRejectReason::InvalidIdentifier);
    }
    let bytes = hex::decode(value).map_err(|_| ActivityRejectReason::InvalidIdentifier)?;
    bytes
        .try_into()
        .map_err(|_| ActivityRejectReason::InvalidIdentifier)
}

/// Random opaque room-session token assigned by Channels outside Activity. It
/// must never be derived from the human-authored room label.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ChannelRoomToken([u8; 16]);

impl ChannelRoomToken {
    pub fn random() -> Self {
        Self(rns_crypto::random::random_16())
    }

    #[cfg(test)]
    pub(super) const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}

/// Random volatile token assigned to one RRC envelope identifier. The RRC
/// message id is only used as an in-memory lookup key by Channels; Activity
/// receives this unrelated 256-bit token.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ChannelMessageToken([u8; 32]);

impl ChannelMessageToken {
    pub fn random() -> Self {
        Self(rns_crypto::random::random_32())
    }

    #[cfg(test)]
    pub(super) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Random, session-local lookup key into navigation state owned outside
/// Activity. There is no constructor from labels, paths, or arbitrary bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NavigationToken([u8; 16]);

impl NavigationToken {
    pub fn random() -> Self {
        Self(rns_crypto::random::random_16())
    }

    #[cfg(test)]
    pub(super) const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}

/// Validated private TCP endpoint input. It is zeroized when moved into a
/// draft and has no `Debug`, `Clone`, or serialization implementation.
pub struct TcpEndpoint(ClassifiedEndpoint);

impl TcpEndpoint {
    pub fn new(value: String) -> Result<Self, ActivityRejectReason> {
        ClassifiedEndpoint::network(EndpointClass::Tcp, value).map(Self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppRuntimeTransition {
    Started,
    Ready,
    Unavailable,
    Stopped,
}

pub fn app_runtime(time: ObservationTime, transition: AppRuntimeTransition) -> ActivityDraft {
    let (kind, severity, outcome) = match transition {
        AppRuntimeTransition::Started => (
            kinds::APP_RUNTIME_STARTED,
            ActivitySeverity::Info,
            ActivityOutcome::Started,
        ),
        AppRuntimeTransition::Ready => (
            kinds::APP_RUNTIME_READY,
            ActivitySeverity::Info,
            ActivityOutcome::Success,
        ),
        AppRuntimeTransition::Unavailable => (
            kinds::APP_RUNTIME_UNAVAILABLE,
            ActivitySeverity::Error,
            ActivityOutcome::Failed,
        ),
        AppRuntimeTransition::Stopped => (
            kinds::APP_RUNTIME_STOPPED,
            ActivitySeverity::Info,
            ActivityOutcome::Success,
        ),
    };
    ActivityDraft::new(
        kind,
        severity,
        ActivityDirection::Local,
        outcome,
        time.unix_ms,
        time.elapsed_ms,
        CoalescingPolicy::Never,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InterfaceClass {
    Auto,
    BackboneClient,
    BackboneServer,
    BluetoothPeer,
    RNode,
    TcpClient,
    TcpServer,
    Unknown,
}

impl InterfaceClass {
    const fn code(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::BackboneClient => "backbone_client",
            Self::BackboneServer => "backbone_server",
            Self::BluetoothPeer => "ble_peer",
            Self::RNode => "rnode",
            Self::TcpClient => "tcp_client",
            Self::TcpServer => "tcp_server",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InterfaceDegradationReason {
    MulticastUnavailable,
    PeripheralUnavailable,
}

impl InterfaceDegradationReason {
    const fn code(self) -> &'static str {
        match self {
            Self::MulticastUnavailable => "multicast_unavailable",
            Self::PeripheralUnavailable => "peripheral_unavailable",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InterfaceTimeoutReason {
    Setup,
    Pairing,
    Startup,
}

impl InterfaceTimeoutReason {
    const fn code(self) -> &'static str {
        match self {
            Self::Setup => "setup_timed_out",
            Self::Pairing => "pairing_timed_out",
            Self::Startup => "startup_timed_out",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InterfaceFailureReason {
    Configure,
    Connect,
    Listen,
    Remove,
    Resume,
    Runtime,
    Update,
}

impl InterfaceFailureReason {
    const fn code(self) -> &'static str {
        match self {
            Self::Configure => "configure_failed",
            Self::Connect => "connect_failed",
            Self::Listen => "listen_failed",
            Self::Remove => "remove_failed",
            Self::Resume => "resume_failed",
            Self::Runtime => "runtime_failed",
            Self::Update => "update_failed",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InterfaceRollback {
    ConfigRestored,
    RestartFailed,
    WriteFailed,
}

impl InterfaceRollback {
    const fn code(self) -> &'static str {
        match self {
            Self::ConfigRestored => "config_restored",
            Self::RestartFailed => "restart_failed",
            Self::WriteFailed => "write_failed",
        }
    }
}

pub enum InterfaceTransition {
    Configured,
    Connecting,
    Cancelled,
    Online,
    Offline,
    Degraded {
        reason: InterfaceDegradationReason,
    },
    Paused,
    Removed,
    Failed {
        reason: InterfaceFailureReason,
        rollback: Option<InterfaceRollback>,
    },
    TimedOut {
        reason: InterfaceTimeoutReason,
    },
}

pub struct InterfaceActivity {
    pub time: ObservationTime,
    pub class: InterfaceClass,
    pub transition: InterfaceTransition,
    pub endpoint: Option<TcpEndpoint>,
}

pub fn interface_activity(input: InterfaceActivity) -> Result<ActivityDraft, ActivityRejectReason> {
    let (kind, severity, outcome, reason, rollback) = match input.transition {
        InterfaceTransition::Configured => (
            kinds::INTERFACE_CONFIGURED,
            ActivitySeverity::Info,
            ActivityOutcome::Success,
            None,
            None,
        ),
        InterfaceTransition::Connecting => (
            kinds::INTERFACE_CONNECTING,
            ActivitySeverity::Info,
            ActivityOutcome::Started,
            None,
            None,
        ),
        InterfaceTransition::Cancelled => (
            kinds::INTERFACE_CANCELLED,
            ActivitySeverity::Info,
            ActivityOutcome::Success,
            None,
            None,
        ),
        InterfaceTransition::Online => (
            kinds::INTERFACE_ONLINE,
            ActivitySeverity::Info,
            ActivityOutcome::Success,
            None,
            None,
        ),
        InterfaceTransition::Offline => (
            kinds::INTERFACE_OFFLINE,
            ActivitySeverity::Warning,
            ActivityOutcome::Degraded,
            None,
            None,
        ),
        InterfaceTransition::Degraded { reason } => (
            kinds::INTERFACE_DEGRADED,
            ActivitySeverity::Warning,
            ActivityOutcome::Degraded,
            Some(reason.code()),
            None,
        ),
        InterfaceTransition::Paused => (
            kinds::INTERFACE_PAUSED,
            ActivitySeverity::Info,
            ActivityOutcome::Success,
            None,
            None,
        ),
        InterfaceTransition::Removed => (
            kinds::INTERFACE_REMOVED,
            ActivitySeverity::Info,
            ActivityOutcome::Success,
            None,
            None,
        ),
        InterfaceTransition::Failed { reason, rollback } => (
            kinds::INTERFACE_FAILED,
            ActivitySeverity::Error,
            ActivityOutcome::Failed,
            Some(reason.code()),
            rollback,
        ),
        InterfaceTransition::TimedOut { reason } => (
            kinds::INTERFACE_TIMED_OUT,
            ActivitySeverity::Error,
            ActivityOutcome::TimedOut,
            Some(reason.code()),
            None,
        ),
    };
    let mut draft = ActivityDraft::new(
        kind,
        severity,
        ActivityDirection::Local,
        outcome,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .operational_code(ActivityAttributeKey::InterfaceClass, input.class.code())?;
    if let Some(reason) = reason {
        draft = draft.operational_code(ActivityAttributeKey::Reason, reason)?;
    }
    if let Some(rollback) = rollback {
        draft = draft.operational_code(ActivityAttributeKey::State, rollback.code())?;
    }
    if let Some(endpoint) = input.endpoint {
        draft = draft.sensitive_endpoint(ActivityAttributeKey::Endpoint, endpoint.0);
    }
    Ok(draft)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PathRequestMethod {
    Automatic,
    ContactRefresh,
    Manual,
}

impl PathRequestMethod {
    const fn code(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::ContactRefresh => "contact_refresh",
            Self::Manual => "manual",
        }
    }
}

pub struct RnsPathRequested {
    pub time: ObservationTime,
    pub destination: Option<DestinationHash>,
    pub count: Option<u64>,
    pub method: PathRequestMethod,
}

pub fn rns_path_requested(input: RnsPathRequested) -> Result<ActivityDraft, ActivityRejectReason> {
    let mut draft = ActivityDraft::new(
        kinds::RNS_PATH_REQUESTED,
        ActivitySeverity::Info,
        ActivityDirection::Outbound,
        ActivityOutcome::Started,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .operational_code(ActivityAttributeKey::Method, input.method.code())?;
    if let Some(destination) = input.destination {
        draft = draft.protocol_identifier(
            ActivityAttributeKey::Destination,
            IdentifierKind::Destination,
            &destination.0,
        )?;
    }
    if let Some(count) = input.count {
        draft = draft.exact(ActivityAttributeKey::Count, ExactValue::Unsigned(count));
    }
    Ok(draft)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AnnounceMethod {
    InterfaceOnline,
    LxmfDelivery,
    LxstService,
    Manual,
    Startup,
    Transport,
}

impl AnnounceMethod {
    const fn code(self) -> &'static str {
        match self {
            Self::InterfaceOnline => "interface_online",
            Self::LxmfDelivery => "lxmf_delivery",
            Self::LxstService => "lxst_service",
            Self::Manual => "manual",
            Self::Startup => "startup",
            Self::Transport => "transport",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AnnounceFailureReason {
    NoInterfaceTransmission,
    NotReady,
    QueueFailed,
    TransportUnavailable,
}

impl AnnounceFailureReason {
    const fn code(self) -> &'static str {
        match self {
            Self::NoInterfaceTransmission => "no_interface_transmission",
            Self::NotReady => "not_ready",
            Self::QueueFailed => "queue_failed",
            Self::TransportUnavailable => "transport_unavailable",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AnnounceSuppressionReason {
    Cooldown,
    InterfaceRestart,
    RateLimit,
}

impl AnnounceSuppressionReason {
    const fn code(self) -> &'static str {
        match self {
            Self::Cooldown => "cooldown",
            Self::InterfaceRestart => "interface_restart",
            Self::RateLimit => "rate_limit",
        }
    }
}

pub enum RnsAnnounceTransition {
    Sent {
        method: AnnounceMethod,
    },
    Failed {
        method: AnnounceMethod,
        reason: AnnounceFailureReason,
    },
    Held {
        count: u64,
    },
    IngressBurstStarted,
    IngressBurstCleared,
    Suppressed {
        reason: AnnounceSuppressionReason,
    },
    Observed {
        destination: DestinationHash,
        hops: u8,
    },
}

pub struct RnsAnnounceActivity {
    pub time: ObservationTime,
    pub transition: RnsAnnounceTransition,
    pub interface: Option<InterfaceClass>,
}

pub fn rns_announce_activity(
    input: RnsAnnounceActivity,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let (kind, severity, direction, outcome, coalescing) = match input.transition {
        RnsAnnounceTransition::Sent { .. } => (
            kinds::RNS_ANNOUNCE_SENT,
            ActivitySeverity::Info,
            ActivityDirection::Outbound,
            ActivityOutcome::Success,
            CoalescingPolicy::Never,
        ),
        RnsAnnounceTransition::Failed { .. } => (
            kinds::RNS_ANNOUNCE_FAILED,
            ActivitySeverity::Error,
            ActivityDirection::Outbound,
            ActivityOutcome::Failed,
            CoalescingPolicy::Never,
        ),
        RnsAnnounceTransition::Held { .. } => (
            kinds::RNS_ANNOUNCE_HELD,
            ActivitySeverity::Warning,
            ActivityDirection::Local,
            ActivityOutcome::Progress,
            CoalescingPolicy::AdjacentEquivalent,
        ),
        RnsAnnounceTransition::IngressBurstStarted => (
            kinds::RNS_ANNOUNCE_INGRESS_BURST_STARTED,
            ActivitySeverity::Warning,
            ActivityDirection::Inbound,
            ActivityOutcome::Degraded,
            CoalescingPolicy::Never,
        ),
        RnsAnnounceTransition::IngressBurstCleared => (
            kinds::RNS_ANNOUNCE_INGRESS_BURST_CLEARED,
            ActivitySeverity::Info,
            ActivityDirection::Local,
            ActivityOutcome::Success,
            CoalescingPolicy::Never,
        ),
        RnsAnnounceTransition::Suppressed { .. } => (
            kinds::RNS_ANNOUNCE_SUPPRESSED,
            ActivitySeverity::Info,
            ActivityDirection::Local,
            ActivityOutcome::Dropped,
            CoalescingPolicy::Never,
        ),
        RnsAnnounceTransition::Observed { .. } => (
            kinds::RNS_ANNOUNCE_OBSERVED,
            ActivitySeverity::Info,
            ActivityDirection::Inbound,
            ActivityOutcome::Success,
            CoalescingPolicy::AdjacentEquivalent,
        ),
    };
    let mut draft = ActivityDraft::new(
        kind,
        severity,
        direction,
        outcome,
        input.time.unix_ms,
        input.time.elapsed_ms,
        coalescing,
    );
    match input.transition {
        RnsAnnounceTransition::Sent { method } => {
            draft = draft.operational_code(ActivityAttributeKey::Method, method.code())?;
        }
        RnsAnnounceTransition::Failed { method, reason } => {
            draft = draft
                .operational_code(ActivityAttributeKey::Method, method.code())?
                .operational_code(ActivityAttributeKey::Reason, reason.code())?;
        }
        RnsAnnounceTransition::Held { count } => {
            draft = draft.exact(
                ActivityAttributeKey::QueueCount,
                ExactValue::Unsigned(count),
            );
        }
        RnsAnnounceTransition::Suppressed { reason } => {
            draft = draft.operational_code(ActivityAttributeKey::Reason, reason.code())?;
        }
        RnsAnnounceTransition::Observed { destination, hops } => {
            draft = draft
                .protocol_identifier(
                    ActivityAttributeKey::Destination,
                    IdentifierKind::Destination,
                    &destination.0,
                )?
                .exact(
                    ActivityAttributeKey::Hops,
                    ExactValue::Unsigned(u64::from(hops)),
                );
        }
        RnsAnnounceTransition::IngressBurstStarted | RnsAnnounceTransition::IngressBurstCleared => {
        }
    }
    if let Some(interface) = input.interface {
        draft = draft.operational_code(ActivityAttributeKey::InterfaceClass, interface.code())?;
    }
    Ok(draft)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PathEvidence {
    Announce,
    Cached,
    PathResponse,
    Transport,
}

impl PathEvidence {
    const fn code(self) -> &'static str {
        match self {
            Self::Announce => "announce",
            Self::Cached => "cached",
            Self::PathResponse => "path_response",
            Self::Transport => "transport",
        }
    }
}

pub struct RnsPathDiscovered {
    pub time: ObservationTime,
    pub destination: DestinationHash,
    pub hops: u8,
    pub evidence: PathEvidence,
    pub endpoint: Option<TcpEndpoint>,
    pub correlation_id: Option<CorrelationId>,
}

pub fn rns_path_discovered(
    input: RnsPathDiscovered,
) -> Result<ActivityDraft, ActivityRejectReason> {
    rns_path_event(kinds::RNS_PATH_DISCOVERED, input)
}

pub fn rns_path_observed(input: RnsPathDiscovered) -> Result<ActivityDraft, ActivityRejectReason> {
    rns_path_event(kinds::RNS_PATH_OBSERVED, input)
}

fn rns_path_event(
    kind: super::schema::ActivityKindCode,
    input: RnsPathDiscovered,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let mut draft = ActivityDraft::new(
        kind,
        ActivitySeverity::Info,
        ActivityDirection::Inbound,
        ActivityOutcome::Success,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::AdjacentEquivalent,
    )
    .protocol_identifier(
        ActivityAttributeKey::Destination,
        IdentifierKind::Destination,
        &input.destination.0,
    )?
    .exact(
        ActivityAttributeKey::Hops,
        ExactValue::Unsigned(u64::from(input.hops)),
    )
    .operational_code(ActivityAttributeKey::Validation, input.evidence.code())?;
    if let Some(endpoint) = input.endpoint {
        draft = draft.sensitive_endpoint(ActivityAttributeKey::Endpoint, endpoint.0);
    }
    if let Some(correlation_id) = input.correlation_id {
        draft = draft.with_correlation(correlation_id);
    }
    Ok(draft)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChannelEnvelopeKind {
    Action,
    Error,
    Hello,
    Join,
    Joined,
    Message,
    Part,
    Parted,
    Ping,
    Pong,
    Notice,
    Resource,
    Welcome,
}

impl ChannelEnvelopeKind {
    const fn code(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::Error => "error",
            Self::Hello => "hello",
            Self::Join => "join",
            Self::Joined => "joined",
            Self::Message => "message",
            Self::Part => "part",
            Self::Parted => "parted",
            Self::Ping => "ping",
            Self::Pong => "pong",
            Self::Notice => "notice",
            Self::Resource => "resource",
            Self::Welcome => "welcome",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SourceValidation {
    Accepted,
    Duplicate,
    Malformed,
    NonHub,
    Unsupported,
    WrongSource,
}

impl SourceValidation {
    const fn code(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
            Self::Malformed => "malformed",
            Self::NonHub => "non_hub",
            Self::Unsupported => "unsupported",
            Self::WrongSource => "wrong_source",
        }
    }
}

pub struct ChannelsEnvelopeActivity {
    pub time: ObservationTime,
    pub hub: DestinationHash,
    pub room: Option<ChannelRoomToken>,
    pub message: Option<ChannelMessageToken>,
    pub envelope_kind: Option<ChannelEnvelopeKind>,
    pub encoded_bytes: u32,
    pub validation: SourceValidation,
    pub correlation_id: CorrelationId,
}

pub fn channels_envelope_sent(
    input: ChannelsEnvelopeActivity,
) -> Result<ActivityDraft, ActivityRejectReason> {
    channels_envelope(input, ActivityDirection::Outbound)
}

pub fn channels_envelope_received(
    input: ChannelsEnvelopeActivity,
) -> Result<ActivityDraft, ActivityRejectReason> {
    channels_envelope(input, ActivityDirection::Inbound)
}

fn channels_envelope(
    input: ChannelsEnvelopeActivity,
    direction: ActivityDirection,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let (kind, severity, outcome, coalescing, duplicate) = match input.validation {
        SourceValidation::Accepted => (
            if direction == ActivityDirection::Outbound {
                kinds::CHANNELS_ENVELOPE_SENT
            } else {
                kinds::CHANNELS_ENVELOPE_RECEIVED
            },
            ActivitySeverity::Info,
            ActivityOutcome::Success,
            CoalescingPolicy::AdjacentEquivalent,
            false,
        ),
        SourceValidation::Duplicate | SourceValidation::Unsupported => (
            kinds::CHANNELS_ENVELOPE_RECEIVED,
            ActivitySeverity::Info,
            ActivityOutcome::Dropped,
            CoalescingPolicy::Never,
            matches!(input.validation, SourceValidation::Duplicate),
        ),
        SourceValidation::Malformed | SourceValidation::NonHub | SourceValidation::WrongSource => (
            kinds::CHANNELS_ENVELOPE_REJECTED,
            ActivitySeverity::Warning,
            ActivityOutcome::Rejected,
            CoalescingPolicy::Never,
            false,
        ),
    };
    let draft = ActivityDraft::new(
        kind,
        severity,
        direction,
        outcome,
        input.time.unix_ms,
        input.time.elapsed_ms,
        coalescing,
    )
    .protocol_identifier(ActivityAttributeKey::Hub, IdentifierKind::Hub, &input.hub.0)?;
    let mut draft = draft
        .exact(
            ActivityAttributeKey::ByteLength,
            ExactValue::Unsigned(u64::from(input.encoded_bytes)),
        )
        .exact(
            ActivityAttributeKey::Duplicate,
            ExactValue::Boolean(duplicate),
        )
        .operational_code(ActivityAttributeKey::Validation, input.validation.code())?;
    if let Some(room) = input.room {
        draft =
            draft.protocol_identifier(ActivityAttributeKey::Room, IdentifierKind::Room, &room.0)?;
    }
    if let Some(message) = input.message {
        draft = draft.protocol_identifier(
            ActivityAttributeKey::Message,
            IdentifierKind::Message,
            &message.0,
        )?;
    }
    if let Some(envelope_kind) = input.envelope_kind {
        draft = draft.operational_code(ActivityAttributeKey::Method, envelope_kind.code())?;
    }
    Ok(draft.with_correlation(input.correlation_id))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChannelSessionFailureReason {
    AuthenticationFailed,
    HubRejected,
    IdentificationFailed,
    InvalidAnnounce,
    MalformedWelcome,
    PathLookupFailed,
    SendFailed,
    TransportUnavailable,
    UnsupportedVersion,
    WelcomeTimedOut,
    WrongSource,
}

impl ChannelSessionFailureReason {
    const fn code(self) -> &'static str {
        match self {
            Self::AuthenticationFailed => "authentication_failed",
            Self::HubRejected => "hub_rejected",
            Self::IdentificationFailed => "identification_failed",
            Self::InvalidAnnounce => "invalid_announce",
            Self::MalformedWelcome => "malformed_welcome",
            Self::PathLookupFailed => "path_lookup_failed",
            Self::SendFailed => "send_failed",
            Self::TransportUnavailable => "transport_unavailable",
            Self::UnsupportedVersion => "unsupported_version",
            Self::WelcomeTimedOut => "welcome_timed_out",
            Self::WrongSource => "wrong_source",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChannelSessionCloseReason {
    Local,
    Remote,
    SendFailed,
    StreamEnded,
    Timeout,
    TransportUnavailable,
}

impl ChannelSessionCloseReason {
    const fn code(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::SendFailed => "send_failed",
            Self::StreamEnded => "stream_ended",
            Self::Timeout => "timeout",
            Self::TransportUnavailable => "transport_unavailable",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct ChannelNegotiatedCapabilities {
    pub actions: bool,
    pub direct_notices: bool,
    pub resource_envelopes: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct ChannelNegotiatedLimits {
    pub max_nick_bytes: Option<u64>,
    pub max_room_bytes: Option<u64>,
    pub max_message_bytes: Option<u64>,
    pub max_rooms: Option<u64>,
    pub rate_per_minute: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChannelSessionTransition {
    ConnectRequested,
    Cancelled,
    PathRequested,
    PathDiscovered {
        hops: u8,
    },
    PathTimedOut,
    LinkRequested,
    LinkAuthenticated {
        link: LinkId,
    },
    LinkIdentificationSent {
        link: LinkId,
    },
    HelloSent {
        encoded_bytes: u32,
    },
    WelcomeValidated {
        encoded_bytes: u32,
    },
    WelcomeRejected {
        reason: ChannelSessionFailureReason,
    },
    Failed {
        reason: ChannelSessionFailureReason,
    },
    Negotiated {
        protocol_version: u64,
        capabilities: ChannelNegotiatedCapabilities,
        limits: ChannelNegotiatedLimits,
        link_mdu: u64,
    },
    GreetingObserved {
        encoded_bytes: u32,
    },
    Stale,
    Recovered,
    Closed {
        reason: ChannelSessionCloseReason,
    },
}

pub struct ChannelsSessionActivity {
    pub time: ObservationTime,
    pub hub: DestinationHash,
    pub correlation_id: CorrelationId,
    pub transition: ChannelSessionTransition,
}

pub fn channels_session_activity(
    input: ChannelsSessionActivity,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let (kind, severity, direction, outcome, reason) = match input.transition {
        ChannelSessionTransition::ConnectRequested => (
            kinds::CHANNELS_SESSION_CONNECT_REQUESTED,
            ActivitySeverity::Info,
            ActivityDirection::Local,
            ActivityOutcome::Started,
            None,
        ),
        ChannelSessionTransition::Cancelled => (
            kinds::CHANNELS_SESSION_CANCELLED,
            ActivitySeverity::Info,
            ActivityDirection::Local,
            ActivityOutcome::Success,
            None,
        ),
        ChannelSessionTransition::PathRequested => (
            kinds::CHANNELS_SESSION_PATH_REQUESTED,
            ActivitySeverity::Info,
            ActivityDirection::Outbound,
            ActivityOutcome::Started,
            None,
        ),
        ChannelSessionTransition::PathDiscovered { .. } => (
            kinds::CHANNELS_SESSION_PATH_DISCOVERED,
            ActivitySeverity::Info,
            ActivityDirection::Inbound,
            ActivityOutcome::Success,
            None,
        ),
        ChannelSessionTransition::PathTimedOut => (
            kinds::CHANNELS_SESSION_PATH_TIMED_OUT,
            ActivitySeverity::Error,
            ActivityDirection::Local,
            ActivityOutcome::TimedOut,
            None,
        ),
        ChannelSessionTransition::LinkRequested => (
            kinds::CHANNELS_SESSION_LINK_REQUESTED,
            ActivitySeverity::Info,
            ActivityDirection::Outbound,
            ActivityOutcome::Started,
            None,
        ),
        ChannelSessionTransition::LinkAuthenticated { .. } => (
            kinds::CHANNELS_SESSION_LINK_AUTHENTICATED,
            ActivitySeverity::Info,
            ActivityDirection::Inbound,
            ActivityOutcome::Success,
            None,
        ),
        ChannelSessionTransition::LinkIdentificationSent { .. } => (
            kinds::CHANNELS_SESSION_LINK_IDENTIFICATION_SENT,
            ActivitySeverity::Info,
            ActivityDirection::Outbound,
            ActivityOutcome::Success,
            None,
        ),
        ChannelSessionTransition::HelloSent { .. } => (
            kinds::CHANNELS_SESSION_HELLO_SENT,
            ActivitySeverity::Info,
            ActivityDirection::Outbound,
            ActivityOutcome::Success,
            None,
        ),
        ChannelSessionTransition::WelcomeValidated { .. } => (
            kinds::CHANNELS_SESSION_WELCOME_VALIDATED,
            ActivitySeverity::Info,
            ActivityDirection::Inbound,
            ActivityOutcome::Success,
            None,
        ),
        ChannelSessionTransition::WelcomeRejected { reason } => (
            kinds::CHANNELS_SESSION_WELCOME_REJECTED,
            ActivitySeverity::Error,
            ActivityDirection::Inbound,
            ActivityOutcome::Rejected,
            Some(reason.code()),
        ),
        ChannelSessionTransition::Failed { reason } => (
            kinds::CHANNELS_SESSION_FAILED,
            ActivitySeverity::Error,
            ActivityDirection::Local,
            ActivityOutcome::Failed,
            Some(reason.code()),
        ),
        ChannelSessionTransition::Negotiated { .. } => (
            kinds::CHANNELS_SESSION_NEGOTIATED,
            ActivitySeverity::Info,
            ActivityDirection::Inbound,
            ActivityOutcome::Success,
            None,
        ),
        ChannelSessionTransition::GreetingObserved { .. } => (
            kinds::CHANNELS_SESSION_GREETING_OBSERVED,
            ActivitySeverity::Info,
            ActivityDirection::Inbound,
            ActivityOutcome::Success,
            None,
        ),
        ChannelSessionTransition::Stale => (
            kinds::CHANNELS_SESSION_STALE,
            ActivitySeverity::Warning,
            ActivityDirection::Local,
            ActivityOutcome::Degraded,
            None,
        ),
        ChannelSessionTransition::Recovered => (
            kinds::CHANNELS_SESSION_RECOVERED,
            ActivitySeverity::Info,
            ActivityDirection::Local,
            ActivityOutcome::Success,
            None,
        ),
        ChannelSessionTransition::Closed {
            reason: ChannelSessionCloseReason::Local,
        } => (
            kinds::CHANNELS_SESSION_CLOSED,
            ActivitySeverity::Info,
            ActivityDirection::Local,
            ActivityOutcome::Success,
            Some(ChannelSessionCloseReason::Local.code()),
        ),
        ChannelSessionTransition::Closed {
            reason: ChannelSessionCloseReason::Timeout,
        } => (
            kinds::CHANNELS_SESSION_CLOSED,
            ActivitySeverity::Error,
            ActivityDirection::Local,
            ActivityOutcome::TimedOut,
            Some(ChannelSessionCloseReason::Timeout.code()),
        ),
        ChannelSessionTransition::Closed { reason } => (
            kinds::CHANNELS_SESSION_CLOSED,
            ActivitySeverity::Error,
            ActivityDirection::Local,
            ActivityOutcome::Failed,
            Some(reason.code()),
        ),
    };
    let mut draft = ActivityDraft::new(
        kind,
        severity,
        direction,
        outcome,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .protocol_identifier(ActivityAttributeKey::Hub, IdentifierKind::Hub, &input.hub.0)?;
    if let Some(reason) = reason {
        draft = draft.operational_code(ActivityAttributeKey::Reason, reason)?;
    }
    match input.transition {
        ChannelSessionTransition::PathDiscovered { hops } => {
            draft = draft.exact(
                ActivityAttributeKey::Hops,
                ExactValue::Unsigned(u64::from(hops)),
            );
        }
        ChannelSessionTransition::LinkAuthenticated { link }
        | ChannelSessionTransition::LinkIdentificationSent { link } => {
            draft = draft.protocol_identifier(
                ActivityAttributeKey::Link,
                IdentifierKind::Link,
                &link.0,
            )?;
            if matches!(
                input.transition,
                ChannelSessionTransition::LinkIdentificationSent { .. }
            ) {
                draft = draft.operational_code(ActivityAttributeKey::State, "sent")?;
            }
        }
        ChannelSessionTransition::HelloSent { encoded_bytes }
        | ChannelSessionTransition::WelcomeValidated { encoded_bytes }
        | ChannelSessionTransition::GreetingObserved { encoded_bytes } => {
            draft = draft.exact(
                ActivityAttributeKey::ByteLength,
                ExactValue::Unsigned(u64::from(encoded_bytes)),
            );
        }
        ChannelSessionTransition::Negotiated {
            protocol_version,
            capabilities,
            limits,
            link_mdu,
        } => {
            draft = draft
                .exact(
                    ActivityAttributeKey::ProtocolVersion,
                    ExactValue::Unsigned(protocol_version),
                )
                .exact(ActivityAttributeKey::Mdu, ExactValue::Unsigned(link_mdu));
            for (enabled, capability) in [
                (capabilities.actions, "action"),
                (capabilities.direct_notices, "direct_notice"),
                (capabilities.resource_envelopes, "resource_envelope"),
            ] {
                if enabled {
                    draft = draft.operational_code(ActivityAttributeKey::Capability, capability)?;
                }
            }
            for (key, value) in [
                (ActivityAttributeKey::MaxNickBytes, limits.max_nick_bytes),
                (ActivityAttributeKey::MaxRoomBytes, limits.max_room_bytes),
                (
                    ActivityAttributeKey::MaxMessageBytes,
                    limits.max_message_bytes,
                ),
                (ActivityAttributeKey::MaxRooms, limits.max_rooms),
                (ActivityAttributeKey::RatePerMinute, limits.rate_per_minute),
            ] {
                if let Some(value) = value {
                    draft = draft.exact(key, ExactValue::Unsigned(value));
                }
            }
        }
        ChannelSessionTransition::ConnectRequested
        | ChannelSessionTransition::Cancelled
        | ChannelSessionTransition::PathRequested
        | ChannelSessionTransition::PathTimedOut
        | ChannelSessionTransition::LinkRequested
        | ChannelSessionTransition::WelcomeRejected { .. }
        | ChannelSessionTransition::Failed { .. }
        | ChannelSessionTransition::Stale
        | ChannelSessionTransition::Recovered
        | ChannelSessionTransition::Closed { .. } => {}
    }
    Ok(draft.with_correlation(input.correlation_id))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChannelJoinEvidence {
    JoinedRoster,
    RrcdStatusNotice,
}

impl ChannelJoinEvidence {
    const fn code(self) -> &'static str {
        match self {
            Self::JoinedRoster => "joined_roster",
            Self::RrcdStatusNotice => "rrcd_status_notice",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChannelRoomFailureReason {
    HubRejected,
    SendFailed,
    SessionClosed,
}

impl ChannelRoomFailureReason {
    const fn code(self) -> &'static str {
        match self {
            Self::HubRejected => "hub_rejected",
            Self::SendFailed => "send_failed",
            Self::SessionClosed => "session_closed",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChannelRoomTransition {
    JoinRequested,
    Joined { evidence: ChannelJoinEvidence },
    JoinRejected { reason: ChannelRoomFailureReason },
    JoinTimedOut,
    JoinCancelled,
    PartRequested,
    Parted,
    PartRejected { reason: ChannelRoomFailureReason },
    PartTimedOut,
    PartCancelled,
}

pub struct ChannelsRoomActivity {
    pub time: ObservationTime,
    pub hub: DestinationHash,
    pub room: ChannelRoomToken,
    pub correlation_id: CorrelationId,
    pub transition: ChannelRoomTransition,
}

pub fn channels_room_activity(
    input: ChannelsRoomActivity,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let (kind, severity, direction, outcome, reason, evidence) = match input.transition {
        ChannelRoomTransition::JoinRequested => (
            kinds::CHANNELS_ROOM_JOIN_REQUESTED,
            ActivitySeverity::Info,
            ActivityDirection::Outbound,
            ActivityOutcome::Started,
            None,
            None,
        ),
        ChannelRoomTransition::Joined { evidence } => (
            kinds::CHANNELS_ROOM_JOINED,
            ActivitySeverity::Info,
            ActivityDirection::Inbound,
            ActivityOutcome::Success,
            None,
            Some(evidence.code()),
        ),
        ChannelRoomTransition::JoinRejected { reason } => (
            kinds::CHANNELS_ROOM_JOIN_REJECTED,
            ActivitySeverity::Warning,
            ActivityDirection::Inbound,
            ActivityOutcome::Rejected,
            Some(reason.code()),
            None,
        ),
        ChannelRoomTransition::JoinTimedOut => (
            kinds::CHANNELS_ROOM_JOIN_TIMED_OUT,
            ActivitySeverity::Error,
            ActivityDirection::Local,
            ActivityOutcome::TimedOut,
            None,
            None,
        ),
        ChannelRoomTransition::JoinCancelled => (
            kinds::CHANNELS_ROOM_JOIN_CANCELLED,
            ActivitySeverity::Info,
            ActivityDirection::Local,
            ActivityOutcome::Success,
            None,
            None,
        ),
        ChannelRoomTransition::PartRequested => (
            kinds::CHANNELS_ROOM_PART_REQUESTED,
            ActivitySeverity::Info,
            ActivityDirection::Outbound,
            ActivityOutcome::Started,
            None,
            None,
        ),
        ChannelRoomTransition::Parted => (
            kinds::CHANNELS_ROOM_PARTED,
            ActivitySeverity::Info,
            ActivityDirection::Inbound,
            ActivityOutcome::Success,
            None,
            None,
        ),
        ChannelRoomTransition::PartRejected { reason } => (
            kinds::CHANNELS_ROOM_PART_REJECTED,
            ActivitySeverity::Warning,
            ActivityDirection::Inbound,
            ActivityOutcome::Rejected,
            Some(reason.code()),
            None,
        ),
        ChannelRoomTransition::PartTimedOut => (
            kinds::CHANNELS_ROOM_PART_TIMED_OUT,
            ActivitySeverity::Warning,
            ActivityDirection::Local,
            ActivityOutcome::TimedOut,
            None,
            None,
        ),
        ChannelRoomTransition::PartCancelled => (
            kinds::CHANNELS_ROOM_PART_CANCELLED,
            ActivitySeverity::Info,
            ActivityDirection::Local,
            ActivityOutcome::Success,
            None,
            None,
        ),
    };
    let mut draft = ActivityDraft::new(
        kind,
        severity,
        direction,
        outcome,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .protocol_identifier(ActivityAttributeKey::Hub, IdentifierKind::Hub, &input.hub.0)?
    .protocol_identifier(
        ActivityAttributeKey::Room,
        IdentifierKind::Room,
        &input.room.0,
    )?;
    if let Some(reason) = reason {
        draft = draft.operational_code(ActivityAttributeKey::Reason, reason)?;
    }
    if let Some(evidence) = evidence {
        draft = draft.operational_code(ActivityAttributeKey::Validation, evidence)?;
    }
    Ok(draft.with_correlation(input.correlation_id))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LxmfDeliveryMethod {
    Direct,
    Opportunistic,
    Paper,
    Propagated,
}

impl LxmfDeliveryMethod {
    pub fn from_code(value: &str) -> Option<Self> {
        match value {
            "direct" => Some(Self::Direct),
            "opportunistic" => Some(Self::Opportunistic),
            "paper" => Some(Self::Paper),
            "propagated" => Some(Self::Propagated),
            _ => None,
        }
    }

    const fn code(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Opportunistic => "opportunistic",
            Self::Paper => "paper",
            Self::Propagated => "propagated",
        }
    }
}

pub struct LxmfDeliveryQueued {
    pub time: ObservationTime,
    pub message: MessageId,
    pub destination: DestinationHash,
    pub method: LxmfDeliveryMethod,
}

pub fn lxmf_delivery_queued(
    input: LxmfDeliveryQueued,
) -> Result<ActivityDraft, ActivityRejectReason> {
    ActivityDraft::new(
        kinds::LXMF_DELIVERY_QUEUED,
        ActivitySeverity::Info,
        ActivityDirection::Outbound,
        ActivityOutcome::Started,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .protocol_identifier(
        ActivityAttributeKey::Message,
        IdentifierKind::Message,
        &input.message.0,
    )?
    .protocol_identifier(
        ActivityAttributeKey::Destination,
        IdentifierKind::Destination,
        &input.destination.0,
    )?
    .operational_code(ActivityAttributeKey::Method, input.method.code())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LxmfSubmissionFailureReason {
    RouterUnavailable,
    PreparationFailed,
}

impl LxmfSubmissionFailureReason {
    const fn code(self) -> &'static str {
        match self {
            Self::RouterUnavailable => "router_unavailable",
            Self::PreparationFailed => "preparation_failed",
        }
    }
}

pub struct LxmfSubmissionFailed {
    pub time: ObservationTime,
    pub destination: DestinationHash,
    pub reason: LxmfSubmissionFailureReason,
}

pub fn lxmf_submission_failed(
    input: LxmfSubmissionFailed,
) -> Result<ActivityDraft, ActivityRejectReason> {
    ActivityDraft::new(
        kinds::LXMF_DELIVERY_SUBMISSION_FAILED,
        ActivitySeverity::Error,
        ActivityDirection::Outbound,
        ActivityOutcome::Failed,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .protocol_identifier(
        ActivityAttributeKey::Destination,
        IdentifierKind::Destination,
        &input.destination.0,
    )?
    .operational_code(ActivityAttributeKey::Reason, input.reason.code())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LxmfDeliveryState {
    Routing,
    Propagating,
    ReusingBackchannel,
    SendingViaLink,
    Sent,
    Delivered,
    Propagated,
    Rejected,
    Failed,
}

pub struct LxmfDeliveryStateChanged {
    pub time: ObservationTime,
    pub message: MessageId,
    pub state: LxmfDeliveryState,
    pub method: Option<LxmfDeliveryMethod>,
    pub rtt_ms: Option<u64>,
    pub failure_reason: Option<DeliveryFailureReason>,
}

pub fn lxmf_delivery_state_changed(
    input: LxmfDeliveryStateChanged,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let (kind, severity, outcome) = match input.state {
        LxmfDeliveryState::Routing => (
            kinds::LXMF_DELIVERY_PATH_PENDING,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfDeliveryState::Propagating => (
            kinds::LXMF_PROPAGATION_STARTED,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfDeliveryState::ReusingBackchannel => (
            kinds::LXMF_DELIVERY_LINK_REUSED,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfDeliveryState::SendingViaLink => (
            // This persisted router state is overloaded: it also covers work
            // queued behind a pending/busy reusable Link, before any packet or
            // Resource has started. Keep the Activity fact deliberately
            // coarse. Typed progress owns Resource-start facts; a packet-start
            // fact remains deferred until a non-overloaded observer exists.
            kinds::LXMF_DELIVERY_DIRECT_PENDING,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfDeliveryState::Sent => (
            kinds::LXMF_DELIVERY_AWAITING_PROOF,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfDeliveryState::Delivered => (
            kinds::LXMF_DELIVERY_DELIVERED,
            ActivitySeverity::Info,
            ActivityOutcome::Success,
        ),
        LxmfDeliveryState::Propagated => (
            kinds::LXMF_PROPAGATION_SUCCEEDED,
            ActivitySeverity::Info,
            ActivityOutcome::Success,
        ),
        LxmfDeliveryState::Rejected => (
            kinds::LXMF_DELIVERY_REJECTED,
            ActivitySeverity::Error,
            ActivityOutcome::Rejected,
        ),
        LxmfDeliveryState::Failed
            if matches!(input.method, Some(LxmfDeliveryMethod::Propagated)) =>
        {
            (
                kinds::LXMF_PROPAGATION_FAILED,
                ActivitySeverity::Error,
                ActivityOutcome::Failed,
            )
        }
        LxmfDeliveryState::Failed => (
            kinds::LXMF_DELIVERY_FAILED,
            ActivitySeverity::Error,
            ActivityOutcome::Failed,
        ),
    };
    let mut draft = ActivityDraft::new(
        kind,
        severity,
        ActivityDirection::Outbound,
        outcome,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .protocol_identifier(
        ActivityAttributeKey::Message,
        IdentifierKind::Message,
        &input.message.0,
    )?;
    if let Some(method) = input.method {
        draft = draft.operational_code(ActivityAttributeKey::Method, method.code())?;
    }
    if let Some(rtt_ms) = input.rtt_ms {
        draft = draft.exact(ActivityAttributeKey::RttMs, ExactValue::Unsigned(rtt_ms));
    }
    if let Some(reason) = input.failure_reason {
        draft = draft.operational_code(ActivityAttributeKey::Reason, reason.code())?;
    }
    Ok(draft)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LxmfProgressStep {
    LinkEstablishing,
    LinkReady,
    DirectPending,
    LinkReused,
    ResourceStarted,
    ResourceProgress,
    AwaitingProof,
}

pub struct LxmfDeliveryProgress {
    pub time: ObservationTime,
    pub message: MessageId,
    pub destination: DestinationHash,
    pub link: Option<LinkId>,
    pub method: LxmfDeliveryMethod,
    pub step: LxmfProgressStep,
    pub percent: Option<u8>,
    pub attempts: u32,
}

pub fn lxmf_delivery_progress(
    input: LxmfDeliveryProgress,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let (kind, severity, outcome) = match input.step {
        LxmfProgressStep::LinkEstablishing => (
            kinds::LXMF_DELIVERY_LINK_ESTABLISHING,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfProgressStep::LinkReady => (
            kinds::LXMF_DELIVERY_LINK_READY,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfProgressStep::DirectPending => (
            kinds::LXMF_DELIVERY_DIRECT_PENDING,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfProgressStep::LinkReused => (
            kinds::LXMF_DELIVERY_LINK_REUSED,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfProgressStep::ResourceStarted => (
            kinds::LXMF_DELIVERY_RESOURCE_STARTED,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfProgressStep::ResourceProgress => (
            kinds::LXMF_DELIVERY_PROGRESS,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
        LxmfProgressStep::AwaitingProof => (
            kinds::LXMF_DELIVERY_AWAITING_PROOF,
            ActivitySeverity::Info,
            ActivityOutcome::Progress,
        ),
    };
    let mut draft = ActivityDraft::new(
        kind,
        severity,
        ActivityDirection::Outbound,
        outcome,
        input.time.unix_ms,
        input.time.elapsed_ms,
        if matches!(input.step, LxmfProgressStep::ResourceProgress) {
            CoalescingPolicy::AdjacentEquivalent
        } else {
            CoalescingPolicy::Never
        },
    )
    .protocol_identifier(
        ActivityAttributeKey::Message,
        IdentifierKind::Message,
        &input.message.0,
    )?
    .protocol_identifier(
        ActivityAttributeKey::Destination,
        IdentifierKind::Destination,
        &input.destination.0,
    )?
    .operational_code(ActivityAttributeKey::Method, input.method.code())?
    .exact(
        ActivityAttributeKey::Attempts,
        ExactValue::Unsigned(u64::from(input.attempts)),
    );
    if let Some(link) = input.link {
        draft =
            draft.protocol_identifier(ActivityAttributeKey::Link, IdentifierKind::Link, &link.0)?;
    }
    if let Some(percent) = input.percent {
        draft = draft.exact(
            ActivityAttributeKey::Percent,
            ExactValue::Unsigned(u64::from(percent.min(100))),
        );
    }
    Ok(draft)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InboundLxmfMethod {
    Direct,
    Opportunistic,
    Propagated,
}

impl InboundLxmfMethod {
    const fn code(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Opportunistic => "opportunistic",
            Self::Propagated => "propagated",
        }
    }
}

pub struct LxmfInboundAccepted {
    pub time: ObservationTime,
    pub source: DestinationHash,
    pub method: InboundLxmfMethod,
    pub encoded_bytes: u32,
}

pub fn lxmf_inbound_accepted(
    input: LxmfInboundAccepted,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let draft = ActivityDraft::new(
        kinds::LXMF_INBOUND_ACCEPTED,
        ActivitySeverity::Info,
        ActivityDirection::Inbound,
        ActivityOutcome::Success,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .protocol_identifier(
        ActivityAttributeKey::Destination,
        IdentifierKind::Destination,
        &input.source.0,
    )?
    .operational_code(ActivityAttributeKey::Method, input.method.code())?
    .exact(
        ActivityAttributeKey::ByteLength,
        ExactValue::Unsigned(u64::from(input.encoded_bytes)),
    );
    Ok(draft)
}

pub struct LxmfDeliveryFailed {
    pub time: ObservationTime,
    pub message_id: MessageId,
    pub destination: DestinationHash,
    pub link_id: Option<LinkId>,
    pub reason: DeliveryFailureReason,
    pub correlation_id: CorrelationId,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DeliveryFailureReason {
    LinkClosed,
    PathUnavailable,
    ProofTimedOut,
    QueueRejected,
    Rejected,
    ResourceFailed,
    RouterUnavailable,
    TransportFailed,
}

impl DeliveryFailureReason {
    const fn code(self) -> &'static str {
        match self {
            Self::LinkClosed => "link_closed",
            Self::PathUnavailable => "path_unavailable",
            Self::ProofTimedOut => "proof_timed_out",
            Self::QueueRejected => "queue_rejected",
            Self::Rejected => "rejected",
            Self::ResourceFailed => "resource_failed",
            Self::RouterUnavailable => "router_unavailable",
            Self::TransportFailed => "transport_failed",
        }
    }
}

pub fn lxmf_delivery_failed(
    input: LxmfDeliveryFailed,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let mut draft = ActivityDraft::new(
        kinds::LXMF_DELIVERY_FAILED,
        ActivitySeverity::Error,
        ActivityDirection::Outbound,
        ActivityOutcome::Failed,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .protocol_identifier(
        ActivityAttributeKey::Message,
        IdentifierKind::Message,
        &input.message_id.0,
    )?
    .protocol_identifier(
        ActivityAttributeKey::Destination,
        IdentifierKind::Destination,
        &input.destination.0,
    )?
    .operational_code(ActivityAttributeKey::Reason, input.reason.code())?;
    if let Some(link_id) = input.link_id {
        draft = draft.protocol_identifier(
            ActivityAttributeKey::Link,
            IdentifierKind::Link,
            &link_id.0,
        )?;
    }
    Ok(draft.with_correlation(input.correlation_id))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LxstCallReason {
    Busy,
    Rejected,
    Calling,
    Available,
    Ringing,
    Connecting,
    Established,
    LinkFailed,
    ServiceError,
    MediaError,
}

impl LxstCallReason {
    const fn code(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Rejected => "rejected",
            Self::Calling => "calling",
            Self::Available => "available",
            Self::Ringing => "ringing",
            Self::Connecting => "connecting",
            Self::Established => "established",
            Self::LinkFailed => "link_failed",
            Self::ServiceError => "service_error",
            Self::MediaError => "media_error",
        }
    }
}

pub enum LxstTransition {
    ServiceStarted,
    ServiceStopped,
    ServiceFailed {
        reason: LxstCallReason,
    },
    IncomingRinging {
        peer: IdentityHash,
        link: LinkId,
    },
    PathPending {
        peer: IdentityHash,
    },
    LinkRequested {
        peer: IdentityHash,
        link: LinkId,
    },
    Ended {
        link: LinkId,
    },
    Rejected {
        link: LinkId,
    },
    Failed {
        peer: Option<IdentityHash>,
        link: Option<LinkId>,
        reason: LxstCallReason,
    },
    MediaWarning {
        reason: LxstCallReason,
    },
}

pub struct LxstActivity {
    pub time: ObservationTime,
    pub transition: LxstTransition,
}

pub fn lxst_activity(input: LxstActivity) -> Result<ActivityDraft, ActivityRejectReason> {
    let (kind, severity, direction, outcome) = match &input.transition {
        LxstTransition::ServiceStarted => (
            kinds::LXST_SERVICE_STARTED,
            ActivitySeverity::Info,
            ActivityDirection::Local,
            ActivityOutcome::Started,
        ),
        LxstTransition::ServiceStopped => (
            kinds::LXST_SERVICE_STOPPED,
            ActivitySeverity::Info,
            ActivityDirection::Local,
            ActivityOutcome::Success,
        ),
        LxstTransition::ServiceFailed { .. } => (
            kinds::LXST_SERVICE_FAILED,
            ActivitySeverity::Error,
            ActivityDirection::Local,
            ActivityOutcome::Failed,
        ),
        LxstTransition::IncomingRinging { .. } => (
            kinds::LXST_CALL_RINGING,
            ActivitySeverity::Info,
            ActivityDirection::Inbound,
            ActivityOutcome::Started,
        ),
        LxstTransition::PathPending { .. } => (
            kinds::LXST_CALL_PATH_PENDING,
            ActivitySeverity::Info,
            ActivityDirection::Outbound,
            ActivityOutcome::Progress,
        ),
        LxstTransition::LinkRequested { .. } => (
            kinds::LXST_CALL_LINK_REQUESTED,
            ActivitySeverity::Info,
            ActivityDirection::Outbound,
            ActivityOutcome::Started,
        ),
        LxstTransition::Ended { .. } => (
            kinds::LXST_CALL_ENDED,
            ActivitySeverity::Info,
            ActivityDirection::None,
            ActivityOutcome::Success,
        ),
        LxstTransition::Rejected { .. } => (
            kinds::LXST_CALL_REJECTED,
            ActivitySeverity::Warning,
            ActivityDirection::None,
            ActivityOutcome::Rejected,
        ),
        LxstTransition::Failed { .. } => (
            kinds::LXST_CALL_FAILED,
            ActivitySeverity::Error,
            ActivityDirection::None,
            ActivityOutcome::Failed,
        ),
        LxstTransition::MediaWarning { .. } => (
            kinds::LXST_MEDIA_WARNING,
            ActivitySeverity::Warning,
            ActivityDirection::Local,
            ActivityOutcome::Degraded,
        ),
    };
    let mut draft = ActivityDraft::new(
        kind,
        severity,
        direction,
        outcome,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    );
    match input.transition {
        LxstTransition::IncomingRinging { peer, link }
        | LxstTransition::LinkRequested { peer, link } => {
            draft = draft
                .protocol_identifier(
                    ActivityAttributeKey::Identity,
                    IdentifierKind::Peer,
                    &peer.0,
                )?
                .protocol_identifier(ActivityAttributeKey::Link, IdentifierKind::Link, &link.0)?;
        }
        LxstTransition::PathPending { peer } => {
            draft = draft.protocol_identifier(
                ActivityAttributeKey::Identity,
                IdentifierKind::Peer,
                &peer.0,
            )?;
        }
        LxstTransition::Ended { link } => {
            draft = draft.protocol_identifier(
                ActivityAttributeKey::Link,
                IdentifierKind::Link,
                &link.0,
            )?;
        }
        LxstTransition::Rejected { link } => {
            draft = draft
                .protocol_identifier(ActivityAttributeKey::Link, IdentifierKind::Link, &link.0)?
                .operational_code(
                    ActivityAttributeKey::Reason,
                    LxstCallReason::Rejected.code(),
                )?;
        }
        LxstTransition::Failed { peer, link, reason } => {
            if let Some(peer) = peer {
                draft = draft.protocol_identifier(
                    ActivityAttributeKey::Identity,
                    IdentifierKind::Peer,
                    &peer.0,
                )?;
            }
            if let Some(link) = link {
                draft = draft.protocol_identifier(
                    ActivityAttributeKey::Link,
                    IdentifierKind::Link,
                    &link.0,
                )?;
            }
            draft = draft.operational_code(ActivityAttributeKey::Reason, reason.code())?;
        }
        LxstTransition::MediaWarning { reason } => {
            draft = draft.operational_code(ActivityAttributeKey::Reason, reason.code())?;
        }
        LxstTransition::ServiceFailed { reason } => {
            draft = draft.operational_code(ActivityAttributeKey::Reason, reason.code())?;
        }
        LxstTransition::ServiceStarted | LxstTransition::ServiceStopped => {}
    }
    Ok(draft)
}

pub(super) struct DiagnosticsSampled {
    pub time: ObservationTime,
    pub count: u64,
    pub span_ms: u64,
    pub source: RateDomain,
}

pub(super) fn diagnostics_sampled(
    input: DiagnosticsSampled,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let draft = ActivityDraft::new(
        kinds::DIAGNOSTICS_SAMPLED,
        ActivitySeverity::Info,
        ActivityDirection::Local,
        ActivityOutcome::None,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .exact(
        ActivityAttributeKey::SampledCount,
        ExactValue::Unsigned(input.count),
    )
    .operational_code(ActivityAttributeKey::SourceArea, input.source.code())?
    .operational_code(ActivityAttributeKey::Reason, "sustained_rate_limit")?;
    Ok(draft.exact(
        ActivityAttributeKey::TimeSpanMs,
        ExactValue::Unsigned(input.span_ms),
    ))
}

pub struct DiagnosticsDropped {
    pub time: ObservationTime,
    pub count: u64,
    pub span_ms: u64,
}

pub fn diagnostics_dropped(input: DiagnosticsDropped) -> ActivityDraft {
    ActivityDraft::new(
        kinds::DIAGNOSTICS_DROPPED,
        ActivitySeverity::Warning,
        ActivityDirection::Local,
        ActivityOutcome::Dropped,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .exact(
        ActivityAttributeKey::DroppedCount,
        ExactValue::Unsigned(input.count),
    )
    .exact(
        ActivityAttributeKey::TimeSpanMs,
        ExactValue::Unsigned(input.span_ms),
    )
}

pub(super) struct DiagnosticsRejected {
    pub time: ObservationTime,
    pub count: u64,
    pub span_ms: u64,
}

pub(super) fn diagnostics_rejected(input: DiagnosticsRejected) -> ActivityDraft {
    ActivityDraft::new(
        kinds::DIAGNOSTICS_REJECTED,
        ActivitySeverity::Warning,
        ActivityDirection::Local,
        ActivityOutcome::Rejected,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .exact(
        ActivityAttributeKey::RejectedCount,
        ExactValue::Unsigned(input.count),
    )
    .exact(
        ActivityAttributeKey::TimeSpanMs,
        ExactValue::Unsigned(input.span_ms),
    )
}

pub(super) fn diagnostics_capture_started(
    time: ObservationTime,
    profile: super::schema::CaptureProfile,
) -> ActivityDraft {
    diagnostics_profile_boundary(kinds::DIAGNOSTICS_CAPTURE_STARTED, time, profile)
}

pub(super) fn diagnostics_capture_stopped(
    time: ObservationTime,
    profile: super::schema::CaptureProfile,
) -> ActivityDraft {
    diagnostics_profile_boundary(kinds::DIAGNOSTICS_CAPTURE_STOPPED, time, profile)
}

pub(super) fn diagnostics_capture_resumed(time: ObservationTime) -> ActivityDraft {
    diagnostics_profile_boundary(
        kinds::DIAGNOSTICS_CAPTURE_RESUMED,
        time,
        super::schema::CaptureProfile::Normal,
    )
}

pub(super) fn diagnostics_capture_cleared(
    time: ObservationTime,
    profile: super::schema::CaptureProfile,
) -> ActivityDraft {
    diagnostics_profile_boundary(kinds::DIAGNOSTICS_CAPTURE_CLEARED, time, profile)
}

pub(super) fn diagnostics_profile_changed(
    time: ObservationTime,
    profile: super::schema::CaptureProfile,
) -> ActivityDraft {
    diagnostics_profile_boundary(kinds::DIAGNOSTICS_PROFILE_CHANGED, time, profile)
}

fn diagnostics_profile_boundary(
    kind: super::schema::ActivityKindCode,
    time: ObservationTime,
    profile: super::schema::CaptureProfile,
) -> ActivityDraft {
    ActivityDraft::new(
        kind,
        ActivitySeverity::Info,
        ActivityDirection::Local,
        ActivityOutcome::Success,
        time.unix_ms,
        time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .operational_code(ActivityAttributeKey::Profile, profile.code())
    .expect("capture profile codes are compile-time allowlisted")
}

pub(super) struct DiagnosticsEvicted {
    pub(super) time: ObservationTime,
    pub(super) count: u64,
    pub(super) bytes: u64,
    pub(super) span_ms: u64,
}

pub(super) fn diagnostics_evicted(input: DiagnosticsEvicted) -> ActivityDraft {
    ActivityDraft::new(
        kinds::DIAGNOSTICS_EVICTED,
        ActivitySeverity::Warning,
        ActivityDirection::Local,
        ActivityOutcome::Dropped,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .exact(
        ActivityAttributeKey::EvictedCount,
        ExactValue::Unsigned(input.count),
    )
    .exact(
        ActivityAttributeKey::ByteLength,
        ExactValue::Unsigned(input.bytes),
    )
    .exact(
        ActivityAttributeKey::TimeSpanMs,
        ExactValue::Unsigned(input.span_ms),
    )
}

pub(super) fn diagnostics_worker_recovered(time: ObservationTime) -> ActivityDraft {
    ActivityDraft::new(
        kinds::DIAGNOSTICS_WORKER_RECOVERED,
        ActivitySeverity::Warning,
        ActivityDirection::Local,
        ActivityOutcome::Degraded,
        time.unix_ms,
        time.elapsed_ms,
        CoalescingPolicy::Never,
    )
}

pub struct ChannelNavigationReference {
    pub time: ObservationTime,
    pub room: ChannelRoomToken,
    pub navigation_token: NavigationToken,
}

/// Test and future detail-action constructor demonstrating that an opaque
/// navigation reference is retained only in the raw vault and omitted from
/// every masked/copy projection.
pub fn channels_room_joined(
    input: ChannelNavigationReference,
) -> Result<ActivityDraft, ActivityRejectReason> {
    ActivityDraft::new(
        kinds::CHANNELS_ROOM_JOINED,
        ActivitySeverity::Info,
        ActivityDirection::Local,
        ActivityOutcome::Success,
        input.time.unix_ms,
        input.time.elapsed_ms,
        CoalescingPolicy::Never,
    )
    .protocol_identifier(
        ActivityAttributeKey::Room,
        IdentifierKind::Room,
        &input.room.0,
    )?
    .opaque_reference(
        ActivityAttributeKey::Session,
        NavigationAction::Channel,
        &input.navigation_token.0,
    )
}

#[cfg(test)]
pub(super) fn test_network_event(
    timestamp_unix_ms: u64,
    elapsed_ms: u64,
    destination: [u8; 16],
    endpoint: &str,
    coalescing: CoalescingPolicy,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let endpoint = TcpEndpoint::new(endpoint.to_string())?;
    Ok(ActivityDraft::new(
        kinds::RNS_PATH_DISCOVERED,
        ActivitySeverity::Info,
        ActivityDirection::Inbound,
        ActivityOutcome::Success,
        timestamp_unix_ms,
        elapsed_ms,
        coalescing,
    )
    .protocol_identifier(
        ActivityAttributeKey::Destination,
        IdentifierKind::Destination,
        &destination,
    )?
    .sensitive_endpoint(ActivityAttributeKey::Endpoint, endpoint.0))
}

#[cfg(test)]
pub(super) fn test_large_error_event(
    timestamp_unix_ms: u64,
    elapsed_ms: u64,
) -> Result<ActivityDraft, ActivityRejectReason> {
    const LARGE_CODE: &str = concat!(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    let mut draft = ActivityDraft::new(
        kinds::LXMF_DELIVERY_FAILED,
        ActivitySeverity::Error,
        ActivityDirection::Outbound,
        ActivityOutcome::Failed,
        timestamp_unix_ms,
        elapsed_ms,
        CoalescingPolicy::Never,
    );
    for key in [
        ActivityAttributeKey::Validation,
        ActivityAttributeKey::Reason,
        ActivityAttributeKey::State,
        ActivityAttributeKey::Method,
        ActivityAttributeKey::Capability,
        ActivityAttributeKey::Profile,
        ActivityAttributeKey::InterfaceClass,
        ActivityAttributeKey::ProtocolVersion,
        ActivityAttributeKey::Room,
        ActivityAttributeKey::Hub,
    ] {
        draft = draft.operational_code(key, LARGE_CODE)?;
    }
    Ok(draft)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_validated_before_they_can_enter_a_catalog_input() {
        assert!(TcpEndpoint::new("example.net:4242".to_string()).is_ok());
        assert!(matches!(
            TcpEndpoint::new("<script>".to_string()),
            Err(ActivityRejectReason::InvalidEndpoint)
        ));
    }

    #[test]
    fn interface_terminal_and_degradation_reasons_are_closed_typed_codes() {
        for (transition, expected_kind, expected_outcome, expected_reason) in [
            (
                InterfaceTransition::Degraded {
                    reason: InterfaceDegradationReason::MulticastUnavailable,
                },
                "interface.degraded",
                ActivityOutcome::Degraded,
                Some("multicast_unavailable"),
            ),
            (
                InterfaceTransition::Degraded {
                    reason: InterfaceDegradationReason::PeripheralUnavailable,
                },
                "interface.degraded",
                ActivityOutcome::Degraded,
                Some("peripheral_unavailable"),
            ),
            (
                InterfaceTransition::TimedOut {
                    reason: InterfaceTimeoutReason::Setup,
                },
                "interface.timed_out",
                ActivityOutcome::TimedOut,
                Some("setup_timed_out"),
            ),
            (
                InterfaceTransition::TimedOut {
                    reason: InterfaceTimeoutReason::Pairing,
                },
                "interface.timed_out",
                ActivityOutcome::TimedOut,
                Some("pairing_timed_out"),
            ),
            (
                InterfaceTransition::TimedOut {
                    reason: InterfaceTimeoutReason::Startup,
                },
                "interface.timed_out",
                ActivityOutcome::TimedOut,
                Some("startup_timed_out"),
            ),
            (
                InterfaceTransition::Cancelled,
                "interface.cancelled",
                ActivityOutcome::Success,
                None,
            ),
        ] {
            let validated = interface_activity(InterfaceActivity {
                time: ObservationTime::new(1, 1),
                class: InterfaceClass::BluetoothPeer,
                transition,
                endpoint: None,
            })
            .unwrap()
            .validate(super::super::classified::DraftContext {
                capture_session: "11".repeat(16),
                capture_generation: 1,
                capture_profile: super::super::schema::CaptureProfile::Normal,
            })
            .unwrap();

            assert_eq!(validated.kind.code(), expected_kind);
            assert_eq!(validated.outcome, expected_outcome);
            assert_eq!(validated.direction, ActivityDirection::Local);
            assert!(matches!(validated.coalescing, CoalescingPolicy::Never));
            let reason = validated
                .attributes
                .iter()
                .find(|attribute| attribute.key == ActivityAttributeKey::Reason)
                .map(|attribute| match attribute.value {
                    super::super::classified::DraftValue::OperationalCode(code) => code,
                    _ => panic!("reason must remain an operational code"),
                });
            assert_eq!(reason, expected_reason);
        }
    }

    #[test]
    fn channel_inputs_have_no_human_label_or_content_field() {
        let draft = channels_envelope_received(ChannelsEnvelopeActivity {
            time: ObservationTime::new(1, 1),
            hub: DestinationHash::new([1; 16]),
            room: Some(ChannelRoomToken::from_bytes([2; 16])),
            message: Some(ChannelMessageToken::from_bytes([6; 32])),
            envelope_kind: Some(ChannelEnvelopeKind::Message),
            encoded_bytes: 42,
            validation: SourceValidation::Accepted,
            correlation_id: CorrelationId::from_bytes([3; 16]),
        });
        assert!(draft.is_ok());

        let navigation = channels_room_joined(ChannelNavigationReference {
            time: ObservationTime::new(1, 1),
            room: ChannelRoomToken::from_bytes([4; 16]),
            navigation_token: NavigationToken::from_bytes([5; 16]),
        });
        assert!(navigation.is_ok());
    }

    #[test]
    fn channel_source_validation_derives_outcome_duplicate_and_coalescing() {
        for (validation, expected_outcome, expected_duplicate, can_coalesce) in [
            (
                SourceValidation::Accepted,
                ActivityOutcome::Success,
                false,
                true,
            ),
            (
                SourceValidation::Duplicate,
                ActivityOutcome::Dropped,
                true,
                false,
            ),
            (
                SourceValidation::NonHub,
                ActivityOutcome::Rejected,
                false,
                false,
            ),
            (
                SourceValidation::Unsupported,
                ActivityOutcome::Dropped,
                false,
                false,
            ),
            (
                SourceValidation::Malformed,
                ActivityOutcome::Rejected,
                false,
                false,
            ),
            (
                SourceValidation::WrongSource,
                ActivityOutcome::Rejected,
                false,
                false,
            ),
        ] {
            let draft = channels_envelope_received(ChannelsEnvelopeActivity {
                time: ObservationTime::new(1, 1),
                hub: DestinationHash::new([1; 16]),
                room: Some(ChannelRoomToken::from_bytes([2; 16])),
                message: Some(ChannelMessageToken::from_bytes([6; 32])),
                envelope_kind: Some(ChannelEnvelopeKind::Message),
                encoded_bytes: 42,
                validation,
                correlation_id: CorrelationId::from_bytes([3; 16]),
            })
            .unwrap();
            assert_eq!(
                matches!(
                    draft.coalescing_policy(),
                    CoalescingPolicy::AdjacentEquivalent
                ),
                can_coalesce
            );

            let validated = draft
                .validate(super::super::classified::DraftContext {
                    capture_session: "11".repeat(16),
                    capture_generation: 1,
                    capture_profile: super::super::schema::CaptureProfile::Normal,
                })
                .unwrap();
            assert_eq!(validated.outcome, expected_outcome);
            let duplicate = validated
                .attributes
                .iter()
                .find(|attribute| attribute.key == ActivityAttributeKey::Duplicate)
                .expect("catalog always classifies duplicate state");
            assert!(matches!(
                duplicate.value,
                super::super::classified::DraftValue::Exact(ExactValue::Boolean(value))
                    if value == expected_duplicate
            ));
        }
    }

    #[test]
    fn lxmf_typed_progress_uses_specific_nonterminal_kinds() {
        for (step, expected_kind) in [
            (
                LxmfProgressStep::DirectPending,
                "lxmf.delivery.direct_pending",
            ),
            (LxmfProgressStep::LinkReused, "lxmf.delivery.link_reused"),
            (
                LxmfProgressStep::AwaitingProof,
                "lxmf.delivery.awaiting_proof",
            ),
        ] {
            let event = lxmf_delivery_progress(LxmfDeliveryProgress {
                time: ObservationTime::new(4, 4),
                message: MessageId::new([0x51; 32]),
                destination: DestinationHash::new([0x52; 16]),
                link: Some(LinkId::new([0x53; 16])),
                method: LxmfDeliveryMethod::Direct,
                step,
                percent: None,
                attempts: 1,
            })
            .unwrap()
            .validate(super::super::classified::DraftContext {
                capture_session: "11".repeat(16),
                capture_generation: 1,
                capture_profile: super::super::schema::CaptureProfile::Normal,
            })
            .unwrap();
            assert_eq!(event.kind.code(), expected_kind);
        }
    }

    #[test]
    fn failure_catalog_entries_cannot_enable_coalescing() {
        let draft = lxmf_delivery_failed(LxmfDeliveryFailed {
            time: ObservationTime::new(2, 2),
            message_id: MessageId::new([4; 32]),
            destination: DestinationHash::new([5; 16]),
            link_id: Some(LinkId::new([6; 16])),
            reason: DeliveryFailureReason::ProofTimedOut,
            correlation_id: CorrelationId::from_bytes([7; 16]),
        })
        .unwrap();
        assert!(matches!(draft.coalescing_policy(), CoalescingPolicy::Never));
    }
}
