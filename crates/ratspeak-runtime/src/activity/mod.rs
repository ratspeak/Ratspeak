//! Typed, privacy-sealed Activity recorder primitives.
//!
//! The only accepted path runs from the public event-specific producer facade
//! through a nonblocking admission gate and supervised single-consumer worker
//! to masked, bounded replay/batch values. It is attached to `AppState`
//! lifecycle resets and exposed through the typed Tauri Activity API.

mod admission;
mod catalog;
mod classified;
mod coalesce;
pub(crate) mod emitter;
mod gate;
mod health;
mod lifecycle;
pub mod producer;
mod pseudonym;
mod query;
mod replay;
mod ring;
mod schema;
mod worker;

pub use classified::{ActivityRejectReason, CorrelationId};
pub use emitter::{ACTIVITY_BATCH_EVENT, ACTIVITY_STATUS_EVENT};
pub use health::ActivityHealthSnapshot;
pub use lifecycle::{ActivityRecordOutcome, ActivityRecorder, TraceCaptureDuration};
pub use query::{
    ActivityDetailResultV1, ActivityIpcResponse, ActivityRevealResultV1, ActivityRevealedValue,
    ActivitySafeCopyResultV1,
};
pub use replay::{
    ACTIVITY_BATCH_MAX_BYTES, ACTIVITY_BATCH_MAX_EVENTS, ACTIVITY_BATCH_MAX_LATENCY_MS,
    ACTIVITY_REPLAY_MAX_BYTES, ACTIVITY_REPLAY_MAX_EVENTS, ACTIVITY_REPLAY_MIN_BYTES,
    ActivityBatchSink, ActivityBatchV1, ActivityCaptureState, ActivityPublishError,
    ActivityRecorderError, ActivityReplayResultV1, ActivityReplayV1, ActivityStatusV1,
    ActivityTraceStateV1, ActivityWorkerState,
};
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
