//! Capture-session privacy state and the raw-to-masked sealing boundary.

#![allow(
    dead_code,
    reason = "Stage 1A defines capture privacy; Stage 1B owns its lifecycle"
)]

use std::collections::HashMap;
use std::mem;

use zeroize::Zeroizing;

use super::classified::{
    ActivityRejectReason, DraftValue, NavigationAction, ReadyDraft, ValidatedDraft,
};
use super::schema::{
    ACTIVITY_SCHEMA_VERSION, ActivityAttributeV1, ActivityEventV1, ActivityValueV1, DecimalU64,
    EndpointClass, IdentifierKind, MAX_ENCODED_EVENT_BYTES, MaskedEndpointV1, MaskedIdentifierV1,
    SafeCopyError, SafeCopyEventV1,
};

const PSEUDONYM_CONTEXT: &[u8] = b"ratspeak.activity.pseudonym.v1\0";
const INITIAL_PSEUDONYM_BYTES: usize = 12;
const PSEUDONYM_DOMAIN_COUNT: usize = 14;
const MOBILE_MAX_TRACKED_PSEUDONYMS: usize = 2_048;
const DESKTOP_MAX_TRACKED_PSEUDONYMS: usize = 4_096;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum PseudonymDomain {
    Protocol(IdentifierKind),
    Endpoint(EndpointClass),
}

impl PseudonymDomain {
    const fn class_id(self) -> u8 {
        match self {
            Self::Protocol(_) => 1,
            Self::Endpoint(_) => 2,
        }
    }

    const fn semantic_id(self) -> u8 {
        match self {
            Self::Protocol(kind) => kind.domain_id(),
            Self::Endpoint(class) => class.domain_id(),
        }
    }

    const fn ordinal_slot(self) -> usize {
        match self {
            Self::Protocol(kind) => kind.domain_id() as usize - 1,
            Self::Endpoint(class) => 8 + class.domain_id() as usize - 1,
        }
    }
}

#[derive(Clone)]
struct AssignedPseudonym {
    rendered: String,
    ordinal: Option<u32>,
}

#[derive(Clone, Copy)]
struct TrackedPseudonym {
    prefix: RenderedPrefix,
    ordinal: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct RenderedPrefix {
    bytes: [u8; 32],
    len: u8,
}

impl RenderedPrefix {
    fn new(digest: &[u8; 32], len: usize) -> Self {
        let mut bytes = [0; 32];
        bytes[..len].copy_from_slice(&digest[..len]);
        Self {
            bytes,
            len: len as u8,
        }
    }

    fn render(self) -> String {
        hex::encode(&self.bytes[..usize::from(self.len)])
    }
}

/// Capture-lifetime key and bounded digest-only presentation table. Once the
/// table is full, new values use their deterministic full keyed digest without
/// retaining more state. This type is intentionally non-Clone, non-Debug, and
/// non-Serialize.
pub(crate) struct CapturePrivacy {
    key: Zeroizing<[u8; 32]>,
    capture_session: String,
    by_digest: HashMap<(PseudonymDomain, [u8; 32]), TrackedPseudonym>,
    by_rendered: HashMap<RenderedPrefix, (PseudonymDomain, [u8; 32])>,
    next_ordinal: [u32; PSEUDONYM_DOMAIN_COUNT],
    max_tracked_pseudonyms: usize,
}

const fn platform_max_tracked_pseudonyms() -> usize {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let maximum = MOBILE_MAX_TRACKED_PSEUDONYMS;
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let maximum = DESKTOP_MAX_TRACKED_PSEUDONYMS;
    maximum
}

impl CapturePrivacy {
    pub(crate) fn random() -> Self {
        Self::from_material_with_limit(
            rns_crypto::random::random_32(),
            rns_crypto::random::random_16(),
            platform_max_tracked_pseudonyms(),
        )
    }

    fn from_material(key: [u8; 32], capture_session: [u8; 16]) -> Self {
        Self::from_material_with_limit(key, capture_session, platform_max_tracked_pseudonyms())
    }

    fn from_material_with_limit(
        key: [u8; 32],
        capture_session: [u8; 16],
        max_tracked_pseudonyms: usize,
    ) -> Self {
        Self {
            key: Zeroizing::new(key),
            capture_session: hex::encode(capture_session),
            by_digest: HashMap::new(),
            by_rendered: HashMap::new(),
            next_ordinal: [0; PSEUDONYM_DOMAIN_COUNT],
            max_tracked_pseudonyms,
        }
    }

    pub(crate) fn capture_session(&self) -> &str {
        &self.capture_session
    }

    pub(crate) fn seal(
        &mut self,
        ready: ReadyDraft,
        sequence: u64,
    ) -> Result<StoredEventV1, ActivityRejectReason> {
        let ValidatedDraft {
            context,
            kind,
            severity,
            direction,
            outcome,
            timestamp_unix_ms,
            elapsed_ms,
            attributes,
            correlation_id,
            parent_sequence,
            coalescing: _,
            count,
            first_timestamp_ms,
            last_timestamp_ms,
        } = ready.0;

        if context.capture_session != self.capture_session {
            return Err(ActivityRejectReason::CaptureContextMismatch);
        }

        let mut masked_attributes = Vec::with_capacity(attributes.len());
        let mut raw_fields = Vec::new();
        for attribute in attributes {
            let key = attribute.key;
            match attribute.value {
                DraftValue::Exact(value) => {
                    masked_attributes.push(ActivityAttributeV1 {
                        key,
                        value: value.wire(),
                    });
                }
                DraftValue::OperationalCode(code) => {
                    masked_attributes.push(ActivityAttributeV1 {
                        key,
                        value: ActivityValueV1::Code(code.to_string()),
                    });
                }
                DraftValue::ProtocolIdentifier { kind, raw } => {
                    let attribute_index = masked_attributes.len();
                    let assigned = self.assign(PseudonymDomain::Protocol(kind), &raw);
                    masked_attributes.push(ActivityAttributeV1 {
                        key,
                        value: ActivityValueV1::Identifier(MaskedIdentifierV1 {
                            kind,
                            pseudonym: assigned.rendered,
                            ordinal: assigned.ordinal,
                        }),
                    });
                    raw_fields.push(StoredRawField {
                        key,
                        attribute_index: Some(attribute_index),
                        value: StoredRawValue::ProtocolIdentifier { kind, raw },
                    });
                }
                DraftValue::SensitiveEndpoint { class, raw } => {
                    let attribute_index = masked_attributes.len();
                    let assigned = self.assign(PseudonymDomain::Endpoint(class), raw.as_bytes());
                    let retained_raw = (context.capture_profile
                        == super::schema::CaptureProfile::Trace)
                        .then_some(raw);
                    masked_attributes.push(ActivityAttributeV1 {
                        key,
                        value: ActivityValueV1::Endpoint(MaskedEndpointV1 { class }),
                    });
                    raw_fields.push(StoredRawField {
                        key,
                        attribute_index: Some(attribute_index),
                        value: StoredRawValue::SensitiveEndpoint {
                            class,
                            pseudonym: assigned.rendered,
                            raw: retained_raw,
                        },
                    });
                }
                DraftValue::OpaqueLocalReference { action, raw } => {
                    raw_fields.push(StoredRawField {
                        key,
                        attribute_index: None,
                        value: StoredRawValue::OpaqueLocalReference { action, raw },
                    });
                }
            }
        }
        masked_attributes.shrink_to_fit();
        raw_fields.shrink_to_fit();

        let mut wire = ActivityEventV1 {
            version: ACTIVITY_SCHEMA_VERSION,
            sequence: DecimalU64::new(sequence),
            capture_session: context.capture_session,
            capture_generation: DecimalU64::new(context.capture_generation),
            timestamp_unix_ms,
            elapsed_ms,
            area: kind.area(),
            kind: kind.code().to_string(),
            severity,
            capture_profile: context.capture_profile,
            direction,
            outcome,
            summary_code: kind.summary_code().to_string(),
            attributes: masked_attributes,
            correlation_id: correlation_id.map(|id| id.wire()),
            parent_sequence: parent_sequence.map(DecimalU64::new),
            count,
            first_timestamp_ms,
            last_timestamp_ms,
        };
        shrink_wire_allocations(&mut wire);
        let encoded_bytes = serde_json::to_vec(&wire)
            .map_err(|_| ActivityRejectReason::EncodedEventTooLarge)?
            .len();
        if encoded_bytes > MAX_ENCODED_EVENT_BYTES {
            return Err(ActivityRejectReason::EncodedEventTooLarge);
        }
        let safe_copy = safe_copy_projection(&wire, &raw_fields)
            .map_err(|_| ActivityRejectReason::EncodedEventTooLarge)?;
        let safe_copy_bytes = serde_json::to_vec(&safe_copy)
            .map_err(|_| ActivityRejectReason::EncodedEventTooLarge)?
            .len();
        if safe_copy_bytes > MAX_ENCODED_EVENT_BYTES {
            return Err(ActivityRejectReason::EncodedEventTooLarge);
        }

        let charged_bytes = charged_bytes(&wire, &raw_fields, raw_fields.capacity());
        Ok(StoredEventV1 {
            wire,
            raw_fields,
            charged_bytes,
        })
    }

    fn assign(&mut self, domain: PseudonymDomain, canonical_value: &[u8]) -> AssignedPseudonym {
        let expected_len = PSEUDONYM_CONTEXT.len() + 10 + canonical_value.len();
        let mut input = Zeroizing::new(Vec::with_capacity(expected_len));
        input.extend_from_slice(PSEUDONYM_CONTEXT);
        input.push(domain.class_id());
        input.push(domain.semantic_id());
        input.extend_from_slice(&(canonical_value.len() as u64).to_be_bytes());
        input.extend_from_slice(canonical_value);
        debug_assert_eq!(input.len(), expected_len);
        let digest = rns_crypto::hmac::hmac_sha256(self.key.as_slice(), &input);
        self.assign_digest(domain, digest)
    }

    fn assign_digest(&mut self, domain: PseudonymDomain, digest: [u8; 32]) -> AssignedPseudonym {
        let digest_key = (domain, digest);
        if let Some(existing) = self.by_digest.get(&digest_key) {
            return AssignedPseudonym {
                rendered: existing.prefix.render(),
                ordinal: Some(existing.ordinal),
            };
        }

        // Once the fixed table is full, the complete keyed digest is already
        // deterministic and collision-maximal. It needs no retained entry and
        // therefore keeps memory bounded for arbitrarily long captures.
        if self.by_digest.len() >= self.max_tracked_pseudonyms {
            return AssignedPseudonym {
                rendered: hex::encode(digest),
                ordinal: None,
            };
        }

        let ordinal_slot = domain.ordinal_slot();
        self.next_ordinal[ordinal_slot] = self.next_ordinal[ordinal_slot].saturating_add(1);
        let ordinal = self.next_ordinal[ordinal_slot];
        let mut rendered_bytes = INITIAL_PSEUDONYM_BYTES;
        let prefix = loop {
            let candidate = RenderedPrefix::new(&digest, rendered_bytes);
            match self.by_rendered.get(&candidate) {
                None => break candidate,
                Some(existing) if existing == &digest_key => break candidate,
                Some(_) if rendered_bytes < digest.len() => rendered_bytes += 1,
                Some(_) => {
                    // A full HMAC collision is computationally infeasible. If
                    // it occurs, treating the digest as the same pseudonym is
                    // safer than retaining raw material to distinguish it.
                    break candidate;
                }
            }
        };
        self.by_rendered.insert(prefix, digest_key);
        self.by_digest
            .insert(digest_key, TrackedPseudonym { prefix, ordinal });
        AssignedPseudonym {
            rendered: prefix.render(),
            ordinal: Some(ordinal),
        }
    }
}

fn shrink_wire_allocations(wire: &mut ActivityEventV1) {
    wire.capture_session.shrink_to_fit();
    wire.kind.shrink_to_fit();
    wire.summary_code.shrink_to_fit();
    if let Some(correlation) = &mut wire.correlation_id {
        correlation.shrink_to_fit();
    }
    for attribute in &mut wire.attributes {
        match &mut attribute.value {
            ActivityValueV1::Code(value) => value.shrink_to_fit(),
            ActivityValueV1::Endpoint(_) => {}
            ActivityValueV1::Identifier(value) => value.pseudonym.shrink_to_fit(),
            ActivityValueV1::Boolean(_)
            | ActivityValueV1::Signed(_)
            | ActivityValueV1::Unsigned(_) => {}
        }
    }
    wire.attributes.shrink_to_fit();
}

fn charged_bytes(
    wire: &ActivityEventV1,
    raw_fields: &[StoredRawField],
    raw_fields_capacity: usize,
) -> usize {
    // The ring's VecDeque backing allocation charges the fixed
    // `StoredEventV1` slots. Each event charges only its owned allocations.
    let mut total = wire
        .capture_session
        .capacity()
        .saturating_add(wire.kind.capacity())
        .saturating_add(wire.summary_code.capacity())
        .saturating_add(wire.correlation_id.as_ref().map_or(0, String::capacity))
        .saturating_add(
            wire.attributes
                .capacity()
                .saturating_mul(mem::size_of::<ActivityAttributeV1>()),
        )
        .saturating_add(raw_fields_capacity.saturating_mul(mem::size_of::<StoredRawField>()));
    for attribute in &wire.attributes {
        total = total.saturating_add(match &attribute.value {
            ActivityValueV1::Code(value) => value.capacity(),
            ActivityValueV1::Endpoint(_) => 0,
            ActivityValueV1::Identifier(value) => value.pseudonym.capacity(),
            ActivityValueV1::Boolean(_)
            | ActivityValueV1::Signed(_)
            | ActivityValueV1::Unsigned(_) => 0,
        });
    }
    for raw_field in raw_fields {
        total = total.saturating_add(raw_field.value.allocated_bytes());
    }
    total
}

fn safe_copy_projection(
    wire: &ActivityEventV1,
    raw_fields: &[StoredRawField],
) -> Result<SafeCopyEventV1, SafeCopyError> {
    wire.safe_copy_with_endpoint_pseudonyms(|index| {
        raw_fields.iter().find_map(|field| {
            if field.attribute_index != Some(index) {
                return None;
            }
            match &field.value {
                StoredRawValue::SensitiveEndpoint { pseudonym, .. } => Some(pseudonym.as_str()),
                StoredRawValue::ProtocolIdentifier { .. }
                | StoredRawValue::OpaqueLocalReference { .. } => None,
            }
        })
    })
}

enum StoredRawValue {
    ProtocolIdentifier {
        kind: IdentifierKind,
        raw: Zeroizing<Box<[u8]>>,
    },
    SensitiveEndpoint {
        class: EndpointClass,
        pseudonym: String,
        raw: Option<Zeroizing<Box<str>>>,
    },
    OpaqueLocalReference {
        action: NavigationAction,
        raw: Zeroizing<Box<[u8]>>,
    },
}

impl StoredRawValue {
    fn allocated_bytes(&self) -> usize {
        match self {
            Self::ProtocolIdentifier { raw, .. } | Self::OpaqueLocalReference { raw, .. } => {
                raw.len()
            }
            Self::SensitiveEndpoint { pseudonym, raw, .. } => pseudonym
                .capacity()
                .saturating_add(raw.as_ref().map_or(0, |value| value.len())),
        }
    }
}

struct StoredRawField {
    key: super::schema::ActivityAttributeKey,
    attribute_index: Option<usize>,
    value: StoredRawValue,
}

/// Immutable raw-value vault entry. It cannot be cloned, debug-formatted, or
/// serialized. Callers can obtain only masked/safe-copy DTOs or an explicit
/// borrowed reveal through a field-specific command path.
pub(crate) struct StoredEventV1 {
    wire: ActivityEventV1,
    raw_fields: Vec<StoredRawField>,
    charged_bytes: usize,
}

impl StoredEventV1 {
    pub(crate) fn sequence(&self) -> u64 {
        self.wire.sequence.get()
    }

    pub(crate) fn masked(&self) -> ActivityEventV1 {
        self.wire.clone()
    }

    pub(crate) fn safe_copy(&self) -> Result<SafeCopyEventV1, SafeCopyError> {
        safe_copy_projection(&self.wire, &self.raw_fields)
    }

    pub(crate) fn charged_bytes(&self) -> usize {
        self.charged_bytes
    }

    pub(crate) fn reveal_identifier(
        &self,
        key: super::schema::ActivityAttributeKey,
    ) -> Option<RevealedIdentifierRef<'_>> {
        self.raw_fields
            .iter()
            .find(|field| field.key == key)
            .and_then(|field| match &field.value {
                StoredRawValue::ProtocolIdentifier { kind, raw } => {
                    Some(RevealedIdentifierRef { kind: *kind, raw })
                }
                StoredRawValue::SensitiveEndpoint { .. }
                | StoredRawValue::OpaqueLocalReference { .. } => None,
            })
    }

    /// Sensitive endpoints can be revealed only for events captured in Trace.
    pub(crate) fn reveal_endpoint(
        &self,
        key: super::schema::ActivityAttributeKey,
    ) -> Option<RevealedEndpointRef<'_>> {
        if self.wire.capture_profile != super::schema::CaptureProfile::Trace {
            return None;
        }
        self.raw_fields
            .iter()
            .find(|field| field.key == key)
            .and_then(|field| match &field.value {
                StoredRawValue::SensitiveEndpoint {
                    class,
                    raw: Some(raw),
                    ..
                } => Some(RevealedEndpointRef { class: *class, raw }),
                StoredRawValue::SensitiveEndpoint { raw: None, .. } => None,
                StoredRawValue::ProtocolIdentifier { .. }
                | StoredRawValue::OpaqueLocalReference { .. } => None,
            })
    }

    /// Navigation references are resolved only by the internal action path.
    /// They never share the reveal/copy surface.
    pub(crate) fn resolve_navigation(
        &self,
        key: super::schema::ActivityAttributeKey,
        expected_action: NavigationAction,
    ) -> Option<&[u8]> {
        self.raw_fields
            .iter()
            .find(|field| field.key == key)
            .and_then(|field| match &field.value {
                StoredRawValue::OpaqueLocalReference { action, raw }
                    if *action == expected_action =>
                {
                    Some(&raw[..])
                }
                StoredRawValue::ProtocolIdentifier { .. }
                | StoredRawValue::SensitiveEndpoint { .. }
                | StoredRawValue::OpaqueLocalReference { .. } => None,
            })
    }
}

pub(crate) struct RevealedIdentifierRef<'a> {
    pub(crate) kind: IdentifierKind,
    pub(crate) raw: &'a [u8],
}

pub(crate) struct RevealedEndpointRef<'a> {
    pub(crate) class: EndpointClass,
    pub(crate) raw: &'a str,
}

#[cfg(test)]
mod tests {
    use super::super::catalog;
    use super::super::classified::{CoalescingPolicy, DraftContext};
    use super::super::coalesce::{CoalesceOutput, PreflushCoalescer};
    use super::super::schema::{ActivityAttributeKey, CaptureProfile};
    use super::*;

    fn privacy(key: u8, session: u8) -> CapturePrivacy {
        CapturePrivacy::from_material([key; 32], [session; 16])
    }

    fn ready(privacy: &CapturePrivacy, destination: [u8; 16], endpoint: &str) -> ReadyDraft {
        ready_with_profile(privacy, destination, endpoint, CaptureProfile::Normal)
    }

    fn ready_with_profile(
        privacy: &CapturePrivacy,
        destination: [u8; 16],
        endpoint: &str,
        capture_profile: CaptureProfile,
    ) -> ReadyDraft {
        let draft =
            catalog::test_network_event(100, 20, destination, endpoint, CoalescingPolicy::Never)
                .unwrap();
        let validated = draft
            .validate(DraftContext {
                capture_session: privacy.capture_session().to_string(),
                capture_generation: 1,
                capture_profile,
            })
            .unwrap();
        let mut coalescer = PreflushCoalescer::default();
        match coalescer.push(validated) {
            CoalesceOutput::One(ready) => ready,
            CoalesceOutput::Held => coalescer.flush().unwrap(),
            CoalesceOutput::Merged { .. } | CoalesceOutput::Two(_, _) => {
                panic!("single draft cannot merge or produce two outputs")
            }
        }
    }

    #[test]
    fn pseudonyms_are_stable_per_key_domain_and_rotate_with_the_key() {
        let mut first = privacy(1, 1);
        let first_ready = ready(&first, [7; 16], "example.net:4242");
        let first_event = first.seal(first_ready, 1).unwrap();
        let first_wire = first_event.masked();
        let first_peer = match &first_wire.attributes[0].value {
            ActivityValueV1::Identifier(value) => value.pseudonym.clone(),
            _ => panic!("expected identifier"),
        };

        let second_ready = ready(&first, [7; 16], "example.net:4242");
        let second_event = first.seal(second_ready, 2).unwrap();
        let second_peer = match &second_event.masked().attributes[0].value {
            ActivityValueV1::Identifier(value) => value.pseudonym.clone(),
            _ => panic!("expected identifier"),
        };
        assert_eq!(first_peer, second_peer);

        let mut rotated = privacy(2, 2);
        let rotated_ready = ready(&rotated, [7; 16], "example.net:4242");
        let rotated_event = rotated.seal(rotated_ready, 1).unwrap();
        let rotated_peer = match &rotated_event.masked().attributes[0].value {
            ActivityValueV1::Identifier(value) => value.pseudonym.clone(),
            _ => panic!("expected identifier"),
        };
        assert_ne!(first_peer, rotated_peer);
    }

    #[test]
    fn deterministic_prefix_collision_extends_only_the_new_token() {
        let mut privacy = privacy(3, 3);
        let mut first_digest = [0x11; 32];
        let mut second_digest = [0x11; 32];
        first_digest[31] = 0xaa;
        second_digest[31] = 0xbb;

        let first = privacy.assign_digest(
            PseudonymDomain::Protocol(IdentifierKind::Peer),
            first_digest,
        );
        let second = privacy.assign_digest(
            PseudonymDomain::Protocol(IdentifierKind::Peer),
            second_digest,
        );
        assert_eq!(first.rendered.len(), INITIAL_PSEUDONYM_BYTES * 2);
        assert!(second.rendered.len() > first.rendered.len());
        assert_eq!(first.ordinal, Some(1));
        assert_eq!(second.ordinal, Some(2));
        assert_eq!(
            privacy
                .assign_digest(
                    PseudonymDomain::Protocol(IdentifierKind::Peer),
                    first_digest,
                )
                .rendered,
            first.rendered
        );
    }

    #[test]
    fn digest_identity_and_ordinals_remain_domain_scoped() {
        let mut privacy = privacy(9, 9);
        let digest = [0x44; 32];
        let protocol =
            privacy.assign_digest(PseudonymDomain::Protocol(IdentifierKind::Peer), digest);
        let endpoint = privacy.assign_digest(PseudonymDomain::Endpoint(EndpointClass::Tcp), digest);

        assert_eq!(protocol.ordinal, Some(1));
        assert_eq!(endpoint.ordinal, Some(1));
        assert_ne!(protocol.rendered, endpoint.rendered);
        assert_eq!(
            privacy
                .assign_digest(PseudonymDomain::Endpoint(EndpointClass::Tcp), digest)
                .rendered,
            endpoint.rendered
        );
    }

    #[test]
    fn pseudonym_tracking_stays_bounded_across_one_hundred_thousand_unique_values() {
        let mut privacy = CapturePrivacy::from_material_with_limit([0x55; 32], [0x66; 16], 32);
        let domain = PseudonymDomain::Protocol(IdentifierKind::Peer);
        let mut overflow_digest = [0; 32];
        let mut saturated_capacities = None;

        for value in 0_u64..100_000 {
            let mut digest = [0; 32];
            digest[24..].copy_from_slice(&value.to_be_bytes());
            let assigned = privacy.assign_digest(domain, digest);
            if value >= 32 {
                assert_eq!(assigned.ordinal, None);
                assert_eq!(assigned.rendered.len(), 64);
            }
            if value == 31 {
                saturated_capacities =
                    Some((privacy.by_digest.capacity(), privacy.by_rendered.capacity()));
            }
            if value == 99_999 {
                overflow_digest = digest;
            }
        }

        assert_eq!(privacy.by_digest.len(), 32);
        assert_eq!(privacy.by_rendered.len(), 32);
        assert_eq!(
            (privacy.by_digest.capacity(), privacy.by_rendered.capacity()),
            saturated_capacities.unwrap()
        );
        assert_eq!(privacy.next_ordinal[domain.ordinal_slot()], 32);
        let repeated = privacy.assign_digest(domain, overflow_digest);
        assert_eq!(repeated.ordinal, None);
        assert_eq!(repeated.rendered, hex::encode(overflow_digest));
        assert_eq!(privacy.by_digest.len(), 32);
    }

    #[test]
    fn raw_values_never_enter_masked_or_safe_copy_serialization() {
        let destination = [0xab; 16];
        let endpoint = "private.example:4242";
        let mut privacy = privacy(4, 4);
        let event_ready = ready(&privacy, destination, endpoint);
        let stored = privacy.seal(event_ready, 9).unwrap();

        let masked_event = stored.masked();
        let masked = serde_json::to_string(&masked_event).unwrap();
        let safe_copy = serde_json::to_string(&stored.safe_copy().unwrap()).unwrap();
        assert!(masked.contains("\"ordinal\""));
        assert!(!safe_copy.contains("\"ordinal\""));
        assert!(safe_copy.contains("\"pseudonym\""));
        let masked_json = serde_json::to_value(&masked_event).unwrap();
        let endpoint_summary = &masked_json["attributes"][1]["value"]["value"];
        assert_eq!(endpoint_summary["class"], "tcp");
        assert!(endpoint_summary.get("pseudonym").is_none());
        for serialized in [masked, safe_copy] {
            assert!(!serialized.contains(endpoint));
            assert!(!serialized.contains(&hex::encode(destination)));
            assert!(!serialized.contains("<script>"));
            assert!(!serialized.contains('\n'));
        }
        let identifier = stored
            .reveal_identifier(ActivityAttributeKey::Destination)
            .unwrap();
        assert_eq!(identifier.kind, IdentifierKind::Destination);
        assert_eq!(identifier.raw, destination);
        assert!(
            stored
                .reveal_endpoint(ActivityAttributeKey::Endpoint)
                .is_none()
        );

        let trace_ready =
            ready_with_profile(&privacy, destination, endpoint, CaptureProfile::Trace);
        let trace_stored = privacy.seal(trace_ready, 10).unwrap();
        let revealed_endpoint = trace_stored
            .reveal_endpoint(ActivityAttributeKey::Endpoint)
            .unwrap();
        assert_eq!(revealed_endpoint.class, EndpointClass::Tcp);
        assert_eq!(revealed_endpoint.raw, endpoint);
    }

    #[test]
    fn opaque_navigation_references_are_omitted_from_both_safe_projections() {
        let navigation_bytes = [0xcd; 16];
        let mut privacy = privacy(8, 8);
        let draft = catalog::channels_room_joined(catalog::ChannelNavigationReference {
            time: catalog::ObservationTime::new(1, 1),
            room: catalog::ChannelRoomToken::from_bytes([3; 16]),
            navigation_token: catalog::NavigationToken::from_bytes(navigation_bytes),
        })
        .unwrap();
        let validated = draft
            .validate(DraftContext {
                capture_session: privacy.capture_session().to_string(),
                capture_generation: 1,
                capture_profile: CaptureProfile::Normal,
            })
            .unwrap();
        let mut coalescer = PreflushCoalescer::default();
        let CoalesceOutput::One(ready) = coalescer.push(validated) else {
            panic!("joined event is non-coalescing");
        };
        let stored = privacy.seal(ready, 1).unwrap();

        for serialized in [
            serde_json::to_string(&stored.masked()).unwrap(),
            serde_json::to_string(&stored.safe_copy().unwrap()).unwrap(),
        ] {
            assert!(!serialized.contains(&hex::encode(navigation_bytes)));
            assert!(!serialized.contains("open_channel"));
        }
        assert_eq!(
            stored.resolve_navigation(ActivityAttributeKey::Session, NavigationAction::Channel),
            Some(navigation_bytes.as_slice())
        );
        assert!(
            stored
                .resolve_navigation(ActivityAttributeKey::Session, NavigationAction::Peer)
                .is_none()
        );
        assert!(
            stored
                .reveal_identifier(ActivityAttributeKey::Session)
                .is_none()
        );
        assert!(
            stored
                .reveal_endpoint(ActivityAttributeKey::Session)
                .is_none()
        );
    }

    #[test]
    fn sealing_rejects_a_capture_context_from_another_session() {
        let first = privacy(5, 5);
        let mut second = privacy(6, 6);
        assert!(matches!(
            second.seal(ready(&first, [1; 16], "host:1"), 1),
            Err(ActivityRejectReason::CaptureContextMismatch)
        ));
    }

    #[test]
    fn charged_bytes_include_raw_allocations_not_only_masked_json() {
        let mut privacy = privacy(7, 7);
        let short_ready = ready_with_profile(&privacy, [2; 16], "a:1", CaptureProfile::Trace);
        let short = privacy.seal(short_ready, 1).unwrap();
        let label = "a".repeat(60);
        let long_endpoint = format!("{label}.{label}.{label}.{label}:1");
        let long_ready =
            ready_with_profile(&privacy, [3; 16], &long_endpoint, CaptureProfile::Trace);
        let long = privacy.seal(long_ready, 2).unwrap();
        assert!(long.charged_bytes() > short.charged_bytes() + 200);

        let normal_ready = ready(&privacy, [4; 16], &long_endpoint);
        let normal = privacy.seal(normal_ready, 3).unwrap();
        assert!(long.charged_bytes() >= normal.charged_bytes() + long_endpoint.len());
    }
}
