//! Masked, JavaScript-safe Activity status, batch, and replay contracts.

use std::sync::RwLock;

use serde::Serialize;

use super::health::ActivityHealthSnapshot;
use super::schema::{ACTIVITY_SCHEMA_VERSION, ActivityEventV1, CaptureProfile, DecimalU64};

pub const ACTIVITY_BATCH_MAX_EVENTS: usize = 50;
pub const ACTIVITY_BATCH_MAX_BYTES: usize = 64 * 1024;
pub const ACTIVITY_BATCH_MAX_LATENCY_MS: u64 = 100;
pub const ACTIVITY_REPLAY_MAX_EVENTS: usize = 50;
pub const ACTIVITY_REPLAY_MAX_BYTES: usize = 64 * 1024;
pub const ACTIVITY_REPLAY_MIN_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityCaptureState {
    Off,
    Capturing,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityWorkerState {
    Starting,
    Running,
    Recovering,
    Recovered,
    Unavailable,
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ActivityRecorderError {
    #[error("activity worker is unavailable")]
    WorkerUnavailable,
    #[error("activity control queue is busy")]
    ControlBusy,
    #[error("activity lifecycle transition is invalid")]
    InvalidTransition,
    #[error("activity lifecycle transition was superseded")]
    Superseded,
    #[error("activity generation is exhausted")]
    GenerationExhausted,
    #[error("activity ring could not be initialized")]
    RingUnavailable,
    #[error("activity request limits are invalid")]
    InvalidRequest,
    #[error("activity lifecycle acknowledgement timed out")]
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityPublishError {
    Rejected,
    Unavailable,
}

/// Stage 1B's result-bearing masked batch boundary. Stage 1C adapts this to
/// the core/Tauri emitter without changing worker ownership or retry rules.
pub trait ActivityBatchSink: Send + Sync + 'static {
    fn try_publish(&self, batch: &ActivityBatchV1) -> Result<(), ActivityPublishError>;
}

pub(super) struct NoopBatchSink;

impl ActivityBatchSink for NoopBatchSink {
    fn try_publish(&self, _batch: &ActivityBatchV1) -> Result<(), ActivityPublishError> {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActivityBatchV1 {
    version: u8,
    capture_session: String,
    first_sequence: DecimalU64,
    last_sequence: DecimalU64,
    events: Vec<ActivityEventV1>,
}

impl ActivityBatchV1 {
    pub(super) fn new(capture_session: String, events: Vec<ActivityEventV1>) -> Option<Self> {
        let first_sequence = events.first()?.sequence();
        let last_sequence = events.last()?.sequence();
        Some(Self {
            version: ACTIVITY_SCHEMA_VERSION,
            capture_session,
            first_sequence: DecimalU64::new(first_sequence),
            last_sequence: DecimalU64::new(last_sequence),
            events,
        })
    }

    pub fn capture_session(&self) -> &str {
        &self.capture_session
    }

    pub const fn first_sequence(&self) -> u64 {
        self.first_sequence.get()
    }

    pub const fn last_sequence(&self) -> u64 {
        self.last_sequence.get()
    }

    pub fn events(&self) -> &[ActivityEventV1] {
        &self.events
    }

    /// Exact JSON size for the derived V1 field order without rebuilding and
    /// serializing every event already held by the worker. `event_bytes` is the
    /// sum of each event's independently encoded JSON size.
    pub(super) fn encoded_len_from_parts(
        capture_session: &str,
        first_sequence: u64,
        last_sequence: u64,
        event_bytes: usize,
        event_count: usize,
    ) -> Option<usize> {
        if event_count == 0 {
            return None;
        }
        let capture_session_bytes = serde_json::to_vec(capture_session).ok()?.len();
        let first_sequence_bytes = serde_json::to_vec(&DecimalU64::new(first_sequence))
            .ok()?
            .len();
        let last_sequence_bytes = serde_json::to_vec(&DecimalU64::new(last_sequence))
            .ok()?
            .len();
        [
            br#"{"version":1,"capture_session":"#.len(),
            capture_session_bytes,
            br#","first_sequence":"#.len(),
            first_sequence_bytes,
            br#","last_sequence":"#.len(),
            last_sequence_bytes,
            br#","events":["#.len(),
            event_bytes,
            event_count.saturating_sub(1),
            br#"]}"#.len(),
        ]
        .into_iter()
        .try_fold(0usize, usize::checked_add)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActivityStatusV1 {
    version: u8,
    capture_session: Option<String>,
    state: ActivityCaptureState,
    profile: Option<CaptureProfile>,
    ingress_generation: DecimalU64,
    oldest: Option<DecimalU64>,
    latest: Option<DecimalU64>,
    worker_state: ActivityWorkerState,
    worker_epoch: DecimalU64,
    counters: ActivityHealthSnapshot,
}

impl ActivityStatusV1 {
    pub fn capture_session(&self) -> Option<&str> {
        self.capture_session.as_deref()
    }

    pub const fn state(&self) -> ActivityCaptureState {
        self.state
    }

    pub const fn profile(&self) -> Option<CaptureProfile> {
        self.profile
    }

    pub const fn ingress_generation(&self) -> u64 {
        self.ingress_generation.get()
    }

    pub const fn oldest(&self) -> Option<u64> {
        match self.oldest {
            Some(value) => Some(value.get()),
            None => None,
        }
    }

    pub const fn latest(&self) -> Option<u64> {
        match self.latest {
            Some(value) => Some(value.get()),
            None => None,
        }
    }

    pub const fn worker_state(&self) -> ActivityWorkerState {
        self.worker_state
    }

    pub const fn worker_epoch(&self) -> u64 {
        self.worker_epoch.get()
    }

    pub fn counters(&self) -> &ActivityHealthSnapshot {
        &self.counters
    }
}

#[derive(Clone)]
pub(super) struct StatusFields {
    pub(super) capture_session: Option<String>,
    pub(super) state: ActivityCaptureState,
    pub(super) profile: Option<CaptureProfile>,
    pub(super) ingress_generation: u64,
    pub(super) oldest: Option<u64>,
    pub(super) latest: Option<u64>,
    pub(super) worker_state: ActivityWorkerState,
    pub(super) worker_epoch: u64,
}

pub(super) struct StatusMirror {
    fields: RwLock<StatusFields>,
}

impl StatusMirror {
    pub(super) fn new() -> Self {
        Self {
            fields: RwLock::new(StatusFields {
                capture_session: None,
                state: ActivityCaptureState::Off,
                profile: None,
                ingress_generation: 0,
                oldest: None,
                latest: None,
                worker_state: ActivityWorkerState::Starting,
                worker_epoch: 0,
            }),
        }
    }

    pub(super) fn update(&self, update: impl FnOnce(&mut StatusFields)) {
        let mut fields = self
            .fields
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut fields);
    }

    pub(super) fn snapshot(&self, counters: ActivityHealthSnapshot) -> ActivityStatusV1 {
        let fields = self
            .fields
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        ActivityStatusV1 {
            version: ACTIVITY_SCHEMA_VERSION,
            capture_session: fields.capture_session,
            state: fields.state,
            profile: fields.profile,
            ingress_generation: DecimalU64::new(fields.ingress_generation),
            oldest: fields.oldest.map(DecimalU64::new),
            latest: fields.latest.map(DecimalU64::new),
            worker_state: fields.worker_state,
            worker_epoch: DecimalU64::new(fields.worker_epoch),
            counters,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActivityReplayV1 {
    version: u8,
    capture_session: String,
    events: Vec<ActivityEventV1>,
    oldest: Option<DecimalU64>,
    latest: Option<DecimalU64>,
    next_after: Option<DecimalU64>,
    has_more: bool,
    gap: bool,
    status_counters: ActivityHealthSnapshot,
}

impl ActivityReplayV1 {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        capture_session: String,
        events: Vec<ActivityEventV1>,
        oldest: Option<u64>,
        latest: Option<u64>,
        next_after: Option<u64>,
        has_more: bool,
        gap: bool,
        status_counters: ActivityHealthSnapshot,
    ) -> Self {
        Self {
            version: ACTIVITY_SCHEMA_VERSION,
            capture_session,
            events,
            oldest: oldest.map(DecimalU64::new),
            latest: latest.map(DecimalU64::new),
            next_after: next_after.map(DecimalU64::new),
            has_more,
            gap,
            status_counters,
        }
    }

    pub fn capture_session(&self) -> &str {
        &self.capture_session
    }

    pub fn events(&self) -> &[ActivityEventV1] {
        &self.events
    }

    pub const fn next_after(&self) -> Option<u64> {
        match self.next_after {
            Some(value) => Some(value.get()),
            None => None,
        }
    }

    pub const fn has_more(&self) -> bool {
        self.has_more
    }

    pub const fn gap(&self) -> bool {
        self.gap
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ActivityReplayResultV1 {
    Page { page: ActivityReplayV1 },
    SessionMismatch { status: ActivityStatusV1 },
}
