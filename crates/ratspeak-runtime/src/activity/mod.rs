//! Typed, privacy-sealed Activity recorder primitives.
//!
//! Stage 1A intentionally contains no worker, Tauri command, `AppState`
//! integration, or producer migration. It defines the only accepted path from
//! sealed catalog drafts to masked wire values and a bounded raw-value vault.

pub(crate) mod catalog;
mod classified;
mod coalesce;
mod health;
mod pseudonym;
mod ring;
mod schema;

pub use classified::{ActivityRejectReason, CorrelationId};
pub use health::ActivityHealthSnapshot;
pub use schema::{
    ACTIVITY_SCHEMA_VERSION, ActivityArea, ActivityAttributeKey, ActivityDirection,
    ActivityEventV1, ActivityOutcome, ActivitySeverity, CaptureProfile, DecimalU64, EndpointClass,
    IdentifierKind, MAX_ATTRIBUTES, MAX_ENCODED_EVENT_BYTES, MAX_STRING_FIELD_BYTES,
    SafeCopyEventV1,
};

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[test]
    fn schema_caps_are_the_normative_stage_one_values() {
        assert_eq!(ACTIVITY_SCHEMA_VERSION, 1);
        assert_eq!(MAX_ATTRIBUTES, 32);
        assert_eq!(MAX_ENCODED_EVENT_BYTES, 4 * 1024);
        assert_eq!(MAX_STRING_FIELD_BYTES, 256);
    }
}
