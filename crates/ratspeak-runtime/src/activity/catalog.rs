//! Sealed, event-specific Activity constructors.
//!
//! Producer modules call functions in this catalog with concrete domain
//! inputs. They cannot select a classification, add an arbitrary attribute,
//! or supply a free-form event/summary code.

#![allow(
    dead_code,
    reason = "Stage 1A seals the producer catalog before Stage 2 migrates producers"
)]

use super::classified::{
    ActivityDraft, ActivityRejectReason, ClassifiedEndpoint, CoalescingPolicy, CorrelationId,
    ExactValue, NavigationAction,
};
use super::schema::{
    ActivityAttributeKey, ActivityDirection, ActivityOutcome, ActivitySeverity, EndpointClass,
    IdentifierKind, kinds,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ObservationTime {
    unix_ms: u64,
    elapsed_ms: u64,
}

impl ObservationTime {
    pub const fn new(unix_ms: u64, elapsed_ms: u64) -> Self {
        Self {
            unix_ms,
            elapsed_ms,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DestinationHash([u8; 16]);

impl DestinationHash {
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MessageId([u8; 32]);

impl MessageId {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LinkId([u8; 16]);

impl LinkId {
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
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
    let mut draft = ActivityDraft::new(
        kinds::RNS_PATH_DISCOVERED,
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
    Join,
    Message,
    Part,
    Ping,
    Pong,
    Status,
}

impl ChannelEnvelopeKind {
    const fn code(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::Join => "join",
            Self::Message => "message",
            Self::Part => "part",
            Self::Ping => "ping",
            Self::Pong => "pong",
            Self::Status => "status",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SourceValidation {
    Accepted,
    Duplicate,
    NonHub,
    WrongSource,
}

impl SourceValidation {
    const fn code(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
            Self::NonHub => "non_hub",
            Self::WrongSource => "wrong_source",
        }
    }
}

pub struct ChannelsEnvelopeReceived {
    pub time: ObservationTime,
    pub hub: DestinationHash,
    pub room: ChannelRoomToken,
    pub envelope_kind: ChannelEnvelopeKind,
    pub encoded_bytes: u32,
    pub validation: SourceValidation,
    pub correlation_id: CorrelationId,
}

pub fn channels_envelope_received(
    input: ChannelsEnvelopeReceived,
) -> Result<ActivityDraft, ActivityRejectReason> {
    let (kind, severity, outcome, coalescing, duplicate) = match input.validation {
        SourceValidation::Accepted => (
            kinds::CHANNELS_ENVELOPE_RECEIVED,
            ActivitySeverity::Info,
            ActivityOutcome::Success,
            CoalescingPolicy::AdjacentEquivalent,
            false,
        ),
        SourceValidation::Duplicate => (
            kinds::CHANNELS_ENVELOPE_RECEIVED,
            ActivitySeverity::Info,
            ActivityOutcome::Dropped,
            CoalescingPolicy::Never,
            true,
        ),
        SourceValidation::NonHub | SourceValidation::WrongSource => (
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
        ActivityDirection::Inbound,
        outcome,
        input.time.unix_ms,
        input.time.elapsed_ms,
        coalescing,
    )
    .protocol_identifier(ActivityAttributeKey::Hub, IdentifierKind::Hub, &input.hub.0)?
    .protocol_identifier(
        ActivityAttributeKey::Room,
        IdentifierKind::Room,
        &input.room.0,
    )?
    .operational_code(ActivityAttributeKey::Method, input.envelope_kind.code())?;
    let draft = draft
        .exact(
            ActivityAttributeKey::ByteLength,
            ExactValue::Unsigned(u64::from(input.encoded_bytes)),
        )
        .exact(
            ActivityAttributeKey::Duplicate,
            ExactValue::Boolean(duplicate),
        )
        .operational_code(ActivityAttributeKey::Validation, input.validation.code())?;
    Ok(draft.with_correlation(input.correlation_id))
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
    ResourceFailed,
}

impl DeliveryFailureReason {
    const fn code(self) -> &'static str {
        match self {
            Self::LinkClosed => "link_closed",
            Self::PathUnavailable => "path_unavailable",
            Self::ProofTimedOut => "proof_timed_out",
            Self::QueueRejected => "queue_rejected",
            Self::ResourceFailed => "resource_failed",
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
    fn channel_inputs_have_no_human_label_or_content_field() {
        let draft = channels_envelope_received(ChannelsEnvelopeReceived {
            time: ObservationTime::new(1, 1),
            hub: DestinationHash::new([1; 16]),
            room: ChannelRoomToken::from_bytes([2; 16]),
            envelope_kind: ChannelEnvelopeKind::Message,
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
                SourceValidation::WrongSource,
                ActivityOutcome::Rejected,
                false,
                false,
            ),
        ] {
            let draft = channels_envelope_received(ChannelsEnvelopeReceived {
                time: ObservationTime::new(1, 1),
                hub: DestinationHash::new([1; 16]),
                room: ChannelRoomToken::from_bytes([2; 16]),
                envelope_kind: ChannelEnvelopeKind::Message,
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
