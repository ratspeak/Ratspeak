//! Typed, privacy-sealed Activity recorder primitives.
//!
//! Stage 1B owns the only accepted path from sealed catalog drafts through a
//! nonblocking admission gate and supervised single-consumer worker to masked,
//! bounded replay/batch values. It is attached to `AppState` lifecycle resets;
//! domain-producer migration and the public Tauri API arrive in later stages.

mod admission;
pub(crate) mod catalog;
mod classified;
mod coalesce;
pub(crate) mod emitter;
mod gate;
mod health;
mod lifecycle;
mod pseudonym;
mod query;
mod replay;
mod ring;
mod schema;
mod worker;

pub use classified::{ActivityDraft, ActivityRejectReason, CorrelationId};
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
