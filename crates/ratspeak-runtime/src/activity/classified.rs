//! Raw-bearing Activity drafts and their validation boundary.
//!
//! Nothing in this file implements `Serialize`, `Debug`, or `Clone`. The only
//! serializable projection is constructed later by `CapturePrivacy`.

#![allow(
    dead_code,
    reason = "reviewed draft variants include later semantic coverage"
)]

use std::fmt;

use zeroize::Zeroizing;

use super::schema::{
    ACTIVITY_SCHEMA_VERSION, ActivityAttributeKey, ActivityAttributeV1, ActivityDirection,
    ActivityEventV1, ActivityKindCode, ActivityOutcome, ActivitySeverity, ActivityValueV1,
    CaptureProfile, DecimalU64, EndpointClass, IdentifierKind, MAX_ATTRIBUTES,
    MAX_ENCODED_EVENT_BYTES, MAX_STRING_FIELD_BYTES, MaskedEndpointV1, MaskedIdentifierV1,
};

const MAX_WIRE_PSEUDONYM: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
const MAX_WIRE_TOKEN: &str = "ffffffffffffffffffffffffffffffff";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityRejectReason {
    CaptureContextMismatch,
    EncodedEventTooLarge,
    InvalidEndpoint,
    InvalidIdentifier,
    InvalidOperationalCode,
    TooManyAttributes,
}

impl fmt::Display for ActivityRejectReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CaptureContextMismatch => "capture_context_mismatch",
            Self::EncodedEventTooLarge => "encoded_event_too_large",
            Self::InvalidEndpoint => "invalid_endpoint",
            Self::InvalidIdentifier => "invalid_identifier",
            Self::InvalidOperationalCode => "invalid_operational_code",
            Self::TooManyAttributes => "too_many_attributes",
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoalescingPolicy {
    Never,
    AdjacentEquivalent,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigationAction {
    Call,
    Channel,
    Conversation,
    Interface,
    Peer,
}

/// Canonical, fixed-allocation endpoint owned by the classified draft path.
/// Source and intermediate strings are zeroized before this value is returned.
pub(super) struct ClassifiedEndpoint {
    class: EndpointClass,
    raw: Zeroizing<Box<str>>,
}

impl ClassifiedEndpoint {
    pub(super) fn network(
        class: EndpointClass,
        value: String,
    ) -> Result<Self, ActivityRejectReason> {
        let source = Zeroizing::new(value);
        if !matches!(class, EndpointClass::Tcp | EndpointClass::Udp) {
            return Err(ActivityRejectReason::InvalidEndpoint);
        }

        let canonical = Zeroizing::new(canonicalize_network_endpoint(source.as_str())?);
        let raw = Zeroizing::new(Box::<str>::from(canonical.as_str()));
        Ok(Self { class, raw })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ExactValue {
    Boolean(bool),
    Signed(i64),
    Unsigned(u64),
}

impl ExactValue {
    pub(super) fn wire(self) -> ActivityValueV1 {
        match self {
            Self::Boolean(value) => ActivityValueV1::Boolean(value),
            Self::Signed(value) => ActivityValueV1::Signed(value),
            Self::Unsigned(value) => ActivityValueV1::Unsigned(value),
        }
    }
}

impl NavigationAction {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Call => "open_call",
            Self::Channel => "open_channel",
            Self::Conversation => "open_conversation",
            Self::Interface => "open_interface",
            Self::Peer => "open_peer",
        }
    }
}

/// An opaque operation token. It is random session state, not a protocol
/// identifier, and can therefore be included in masked projections.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CorrelationId([u8; 16]);

impl CorrelationId {
    pub fn random() -> Self {
        Self(rns_crypto::random::random_16())
    }

    #[cfg(test)]
    pub(super) const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub(super) fn wire(self) -> String {
        hex::encode(self.0)
    }
}

pub(super) struct DraftContext {
    pub(super) capture_session: String,
    pub(super) capture_generation: u64,
    pub(super) capture_profile: CaptureProfile,
}

pub(super) enum DraftValue {
    Exact(ExactValue),
    OperationalCode(&'static str),
    ProtocolIdentifier {
        kind: IdentifierKind,
        raw: Zeroizing<Box<[u8]>>,
    },
    SensitiveEndpoint {
        class: EndpointClass,
        raw: Zeroizing<Box<str>>,
    },
    OpaqueLocalReference {
        action: NavigationAction,
        raw: Zeroizing<Box<[u8]>>,
    },
}

impl PartialEq for DraftValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Exact(left), Self::Exact(right)) => left == right,
            (Self::OperationalCode(left), Self::OperationalCode(right)) => left == right,
            (
                Self::ProtocolIdentifier {
                    kind: left_kind,
                    raw: left_raw,
                },
                Self::ProtocolIdentifier {
                    kind: right_kind,
                    raw: right_raw,
                },
            ) => left_kind == right_kind && left_raw[..] == right_raw[..],
            (
                Self::SensitiveEndpoint {
                    class: left_class,
                    raw: left_raw,
                },
                Self::SensitiveEndpoint {
                    class: right_class,
                    raw: right_raw,
                },
            ) => left_class == right_class && left_raw[..] == right_raw[..],
            (
                Self::OpaqueLocalReference {
                    action: left_action,
                    raw: left_raw,
                },
                Self::OpaqueLocalReference {
                    action: right_action,
                    raw: right_raw,
                },
            ) => left_action == right_action && left_raw[..] == right_raw[..],
            _ => false,
        }
    }
}

impl Eq for DraftValue {}

pub(super) struct DraftAttribute {
    pub(super) key: ActivityAttributeKey,
    pub(super) value: DraftValue,
}

impl PartialEq for DraftAttribute {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.value == other.value
    }
}

impl Eq for DraftAttribute {}

pub(crate) struct ActivityDraft {
    kind: ActivityKindCode,
    severity: ActivitySeverity,
    direction: ActivityDirection,
    outcome: ActivityOutcome,
    timestamp_unix_ms: u64,
    elapsed_ms: u64,
    attributes: Vec<DraftAttribute>,
    correlation_id: Option<CorrelationId>,
    parent_sequence: Option<u64>,
    coalescing: CoalescingPolicy,
}

impl ActivityDraft {
    pub(super) fn new(
        kind: ActivityKindCode,
        severity: ActivitySeverity,
        direction: ActivityDirection,
        outcome: ActivityOutcome,
        timestamp_unix_ms: u64,
        elapsed_ms: u64,
        coalescing: CoalescingPolicy,
    ) -> Self {
        Self {
            kind,
            severity,
            direction,
            outcome,
            timestamp_unix_ms,
            elapsed_ms,
            attributes: Vec::new(),
            correlation_id: None,
            parent_sequence: None,
            coalescing,
        }
    }

    pub(super) fn with_correlation(mut self, correlation_id: CorrelationId) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    pub(super) fn with_parent_sequence(mut self, parent_sequence: u64) -> Self {
        self.parent_sequence = Some(parent_sequence);
        self
    }

    pub(super) const fn area(&self) -> super::schema::ActivityArea {
        self.kind.area()
    }

    pub(super) const fn severity(&self) -> ActivitySeverity {
        self.severity
    }

    pub(super) const fn capture_scope(&self) -> super::schema::CaptureScope {
        self.kind.capture_scope()
    }

    pub(super) fn stamp(&mut self, time: super::catalog::ObservationTime) {
        self.timestamp_unix_ms = time.unix_ms();
        self.elapsed_ms = time.elapsed_ms();
    }

    pub(super) const fn rate_domain(&self) -> super::schema::RateDomain {
        self.kind.rate_domain()
    }

    pub(super) const fn is_ambient(&self) -> bool {
        self.kind.ambient()
    }

    #[cfg(test)]
    pub(super) const fn coalescing_policy(&self) -> CoalescingPolicy {
        self.coalescing
    }

    pub(super) fn exact(mut self, key: ActivityAttributeKey, value: ExactValue) -> Self {
        self.attributes.push(DraftAttribute {
            key,
            value: DraftValue::Exact(value),
        });
        self
    }

    pub(super) fn operational_code(
        mut self,
        key: ActivityAttributeKey,
        code: &'static str,
    ) -> Result<Self, ActivityRejectReason> {
        validate_operational_code(code)?;
        self.attributes.push(DraftAttribute {
            key,
            value: DraftValue::OperationalCode(code),
        });
        Ok(self)
    }

    pub(super) fn protocol_identifier(
        mut self,
        key: ActivityAttributeKey,
        kind: IdentifierKind,
        raw: &[u8],
    ) -> Result<Self, ActivityRejectReason> {
        validate_identifier(kind, raw)?;
        self.attributes.push(DraftAttribute {
            key,
            value: DraftValue::ProtocolIdentifier {
                kind,
                raw: Zeroizing::new(Box::<[u8]>::from(raw)),
            },
        });
        Ok(self)
    }

    pub(super) fn sensitive_endpoint(
        mut self,
        key: ActivityAttributeKey,
        endpoint: ClassifiedEndpoint,
    ) -> Self {
        self.attributes.push(DraftAttribute {
            key,
            value: DraftValue::SensitiveEndpoint {
                class: endpoint.class,
                raw: endpoint.raw,
            },
        });
        self
    }

    pub(super) fn opaque_reference(
        mut self,
        key: ActivityAttributeKey,
        action: NavigationAction,
        raw: &[u8],
    ) -> Result<Self, ActivityRejectReason> {
        if raw.is_empty() || raw.len() > 64 {
            return Err(ActivityRejectReason::InvalidIdentifier);
        }
        self.attributes.push(DraftAttribute {
            key,
            value: DraftValue::OpaqueLocalReference {
                action,
                raw: Zeroizing::new(Box::<[u8]>::from(raw)),
            },
        });
        Ok(self)
    }

    pub(super) fn validate(
        self,
        context: DraftContext,
    ) -> Result<ValidatedDraft, ActivityRejectReason> {
        if self.attributes.len() > MAX_ATTRIBUTES {
            return Err(ActivityRejectReason::TooManyAttributes);
        }

        let conservative_attributes = self
            .attributes
            .iter()
            .filter_map(conservative_projection)
            .collect();
        let conservative = ActivityEventV1 {
            version: ACTIVITY_SCHEMA_VERSION,
            sequence: DecimalU64::new(u64::MAX),
            capture_session: context.capture_session.clone(),
            capture_generation: DecimalU64::new(u64::MAX),
            timestamp_unix_ms: u64::MAX,
            elapsed_ms: u64::MAX,
            area: self.kind.area(),
            kind: self.kind.code().to_string(),
            severity: self.severity,
            capture_profile: context.capture_profile,
            direction: self.direction,
            outcome: self.outcome,
            summary_code: self.kind.summary_code().to_string(),
            attributes: conservative_attributes,
            correlation_id: self
                .correlation_id
                .map(CorrelationId::wire)
                .or_else(|| Some(MAX_WIRE_TOKEN.to_string())),
            parent_sequence: Some(DecimalU64::new(u64::MAX)),
            count: u32::MAX,
            first_timestamp_ms: Some(u64::MAX),
            last_timestamp_ms: Some(u64::MAX),
        };
        let encoded_wire_bytes = serde_json::to_vec(&conservative)
            .map_err(|_| ActivityRejectReason::EncodedEventTooLarge)?
            .len();
        let conservative_copy = conservative
            .safe_copy_with_endpoint_pseudonyms(|_| Some(MAX_WIRE_PSEUDONYM))
            .map_err(|_| ActivityRejectReason::EncodedEventTooLarge)?;
        let encoded_copy_bytes = serde_json::to_vec(&conservative_copy)
            .map_err(|_| ActivityRejectReason::EncodedEventTooLarge)?
            .len();
        if encoded_wire_bytes.max(encoded_copy_bytes) > MAX_ENCODED_EVENT_BYTES {
            return Err(ActivityRejectReason::EncodedEventTooLarge);
        }

        Ok(ValidatedDraft {
            context,
            kind: self.kind,
            severity: self.severity,
            direction: self.direction,
            outcome: self.outcome,
            timestamp_unix_ms: self.timestamp_unix_ms,
            elapsed_ms: self.elapsed_ms,
            attributes: self.attributes,
            correlation_id: self.correlation_id,
            parent_sequence: self.parent_sequence,
            coalescing: self.coalescing,
            count: 1,
            first_timestamp_ms: None,
            last_timestamp_ms: None,
        })
    }
}

fn conservative_projection(attribute: &DraftAttribute) -> Option<ActivityAttributeV1> {
    let value = match &attribute.value {
        DraftValue::Exact(value) => value.wire(),
        DraftValue::OperationalCode(code) => ActivityValueV1::Code((*code).to_string()),
        DraftValue::ProtocolIdentifier { kind, .. } => {
            ActivityValueV1::Identifier(MaskedIdentifierV1 {
                kind: *kind,
                pseudonym: MAX_WIRE_PSEUDONYM.to_string(),
                ordinal: Some(u32::MAX),
            })
        }
        DraftValue::SensitiveEndpoint { class, .. } => {
            ActivityValueV1::Endpoint(MaskedEndpointV1 { class: *class })
        }
        DraftValue::OpaqueLocalReference { .. } => return None,
    };
    Some(ActivityAttributeV1 {
        key: attribute.key,
        value,
    })
}

fn validate_operational_code(code: &str) -> Result<(), ActivityRejectReason> {
    if code.is_empty()
        || code.len() > MAX_STRING_FIELD_BYTES
        || !code.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        return Err(ActivityRejectReason::InvalidOperationalCode);
    }
    Ok(())
}

fn validate_identifier(kind: IdentifierKind, raw: &[u8]) -> Result<(), ActivityRejectReason> {
    let expected = match kind {
        IdentifierKind::Message => 32,
        IdentifierKind::Destination
        | IdentifierKind::Hub
        | IdentifierKind::Identity
        | IdentifierKind::Link
        | IdentifierKind::Peer
        | IdentifierKind::Room
        | IdentifierKind::Session => 16,
    };
    if raw.len() != expected {
        return Err(ActivityRejectReason::InvalidIdentifier);
    }
    Ok(())
}

pub(super) fn canonicalize_network_endpoint(raw: &str) -> Result<String, ActivityRejectReason> {
    if raw.is_empty()
        || raw.len() > MAX_STRING_FIELD_BYTES
        || !raw.is_ascii()
        || raw.chars().any(|character| {
            character.is_control()
                || is_bidi_control(character)
                || matches!(character, '<' | '>' | '&' | '\'' | '"' | '`')
        })
    {
        return Err(ActivityRejectReason::InvalidEndpoint);
    }

    if let Ok(socket) = raw.parse::<std::net::SocketAddr>() {
        return Ok(socket.to_string());
    }

    let (host, port) = raw
        .rsplit_once(':')
        .ok_or(ActivityRejectReason::InvalidEndpoint)?;
    let port = port
        .parse::<u16>()
        .map_err(|_| ActivityRejectReason::InvalidEndpoint)?;
    if host.is_empty() || host.len() > 253 {
        return Err(ActivityRejectReason::InvalidEndpoint);
    }
    let canonical_host = Zeroizing::new(host.to_ascii_lowercase());
    if canonical_host.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return Err(ActivityRejectReason::InvalidEndpoint);
    }
    Ok(format!("{}:{port}", canonical_host.as_str()))
}

fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

pub(super) struct ValidatedDraft {
    pub(super) context: DraftContext,
    pub(super) kind: ActivityKindCode,
    pub(super) severity: ActivitySeverity,
    pub(super) direction: ActivityDirection,
    pub(super) outcome: ActivityOutcome,
    pub(super) timestamp_unix_ms: u64,
    pub(super) elapsed_ms: u64,
    pub(super) attributes: Vec<DraftAttribute>,
    pub(super) correlation_id: Option<CorrelationId>,
    pub(super) parent_sequence: Option<u64>,
    pub(super) coalescing: CoalescingPolicy,
    pub(super) count: u32,
    pub(super) first_timestamp_ms: Option<u64>,
    pub(super) last_timestamp_ms: Option<u64>,
}

impl ValidatedDraft {
    pub(super) fn can_coalesce_with(&self, newer: &Self) -> bool {
        self.coalescing == CoalescingPolicy::AdjacentEquivalent
            && newer.coalescing == CoalescingPolicy::AdjacentEquivalent
            && self.severity != ActivitySeverity::Error
            && newer.severity != ActivitySeverity::Error
            && !self.outcome.prohibits_coalescing()
            && !newer.outcome.prohibits_coalescing()
            && self.count < u32::MAX
            && self.context.capture_session == newer.context.capture_session
            && self.context.capture_generation == newer.context.capture_generation
            && self.context.capture_profile == newer.context.capture_profile
            && self.kind == newer.kind
            && self.severity == newer.severity
            && self.direction == newer.direction
            && self.outcome == newer.outcome
            && self.attributes == newer.attributes
            && self.correlation_id == newer.correlation_id
            && self.parent_sequence == newer.parent_sequence
    }

    pub(super) fn absorb(&mut self, newer: Self) {
        if self.first_timestamp_ms.is_none() {
            self.first_timestamp_ms = Some(self.timestamp_unix_ms);
        }
        self.last_timestamp_ms = Some(newer.timestamp_unix_ms);
        self.count = self.count.saturating_add(newer.count);
    }
}

pub(super) struct ReadyDraft(pub(super) ValidatedDraft);

#[cfg(test)]
mod tests {
    use super::super::schema::kinds;
    use super::*;

    fn context() -> DraftContext {
        DraftContext {
            capture_session: "11".repeat(16),
            capture_generation: 7,
            capture_profile: CaptureProfile::Normal,
        }
    }

    fn base_draft() -> ActivityDraft {
        ActivityDraft::new(
            kinds::RNS_PATH_DISCOVERED,
            ActivitySeverity::Info,
            ActivityDirection::Inbound,
            ActivityOutcome::Success,
            10,
            5,
            CoalescingPolicy::AdjacentEquivalent,
        )
    }

    #[test]
    fn operational_codes_reject_html_unicode_and_controls() {
        for canary in ["<script>", "hello world", "path\nfound", "safe\u{202e}code"] {
            assert_eq!(
                validate_operational_code(canary),
                Err(ActivityRejectReason::InvalidOperationalCode)
            );
        }
        assert!(validate_operational_code("joined_roster").is_ok());
    }

    #[test]
    fn sensitive_endpoints_are_byte_bounded_and_reject_markup_controls_and_bidi() {
        assert_eq!(
            canonicalize_network_endpoint("TCP.Example:4242").unwrap(),
            "tcp.example:4242"
        );
        assert_eq!(
            canonicalize_network_endpoint(&"é".repeat(129)),
            Err(ActivityRejectReason::InvalidEndpoint)
        );
        for canary in [
            "host\nname:1",
            "<img>:1",
            "host\u{202e}name:1",
            "höst:1",
            "secret-body",
            "bad_label:1",
        ] {
            assert_eq!(
                canonicalize_network_endpoint(canary),
                Err(ActivityRejectReason::InvalidEndpoint)
            );
        }
    }

    #[test]
    fn identifier_domains_enforce_concrete_lengths() {
        assert!(validate_identifier(IdentifierKind::Destination, &[1; 16]).is_ok());
        assert!(validate_identifier(IdentifierKind::Message, &[2; 32]).is_ok());
        assert_eq!(
            validate_identifier(IdentifierKind::Destination, &[1; 15]),
            Err(ActivityRejectReason::InvalidIdentifier)
        );
    }

    #[test]
    fn attribute_and_wire_caps_are_enforced_before_sequence_allocation() {
        let mut too_many = base_draft();
        for _ in 0..=MAX_ATTRIBUTES {
            too_many = too_many.exact(ActivityAttributeKey::Count, ExactValue::Unsigned(1));
        }
        assert!(matches!(
            too_many.validate(context()),
            Err(ActivityRejectReason::TooManyAttributes)
        ));

        let mut too_large = base_draft();
        for _ in 0..MAX_ATTRIBUTES {
            too_large = too_large
                .protocol_identifier(
                    ActivityAttributeKey::Message,
                    IdentifierKind::Message,
                    &[9; 32],
                )
                .unwrap();
        }
        assert!(matches!(
            too_large.validate(context()),
            Err(ActivityRejectReason::EncodedEventTooLarge)
        ));
    }

    #[test]
    fn endpoint_safe_copy_expansion_is_capped_before_sequence_allocation() {
        let mut draft = base_draft();
        for index in 0..MAX_ATTRIBUTES {
            let endpoint = ClassifiedEndpoint::network(
                EndpointClass::Tcp,
                format!("host{index}.example:4242"),
            )
            .unwrap();
            draft = draft.sensitive_endpoint(ActivityAttributeKey::Endpoint, endpoint);
        }

        assert!(matches!(
            draft.validate(context()),
            Err(ActivityRejectReason::EncodedEventTooLarge)
        ));
    }

    #[test]
    fn correlation_and_parent_are_opaque_typed_values() {
        let validated = base_draft()
            .with_correlation(CorrelationId::from_bytes([7; 16]))
            .with_parent_sequence(44)
            .validate(context())
            .unwrap();
        assert_eq!(validated.correlation_id.unwrap().wire(), "07".repeat(16));
        assert_eq!(validated.parent_sequence, Some(44));
    }
}
