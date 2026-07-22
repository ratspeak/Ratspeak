//! Cheap producer handle and acknowledged Activity lifecycle controller.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{SendTimeoutError, Sender, TrySendError};
use tokio::sync::oneshot;

use super::admission::{INGRESS_CAPACITY, LowPermitPool, ProcessClock, RateAdmission};
use super::classified::{ActivityDraft, ActivityRejectReason};
use super::gate::{AdmissionGate, GateError};
use super::health::ActivityHealth;
use super::replay::{
    ACTIVITY_REPLAY_MAX_BYTES, ACTIVITY_REPLAY_MAX_EVENTS, ACTIVITY_REPLAY_MIN_BYTES,
    ActivityBatchSink, ActivityRecorderError, ActivityReplayResultV1, ActivityStatusV1,
    NoopBatchSink, StatusMirror,
};
use super::schema::{ActivitySeverity, CaptureProfile, CaptureScope};
use super::worker::{
    BarrierAck, IngressDraft, IngressItem, OrderedBarrier, OrderedBarrierKind, QueryCommand,
    ReplayRequest, UrgentCommand, WorkerShared, spawn_worker,
};

const URGENT_CAPACITY: usize = 16;
const QUERY_CAPACITY: usize = 16;
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(5);
const MOBILE_DEFAULT_TRACE_DURATION: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityRecordOutcome {
    Accepted,
    CaptureOff,
    ProfileFiltered,
    TraceExpired,
    RateLimited,
    IngressFull,
    StaleGeneration,
    Rejected(ActivityRejectReason),
    WorkerUnavailable,
}

/// Explicit Trace lifetime override. `None` at the profile API boundary means
/// the platform default; this keeps mobile's user-selected Until stopped
/// distinct from its default ten-minute deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceCaptureDuration {
    UntilStopped,
    Limited(Duration),
}

struct RecorderInner {
    shared: Arc<WorkerShared>,
    ingress_tx: Sender<IngressItem>,
    urgent_tx: Sender<UrgentCommand>,
    query_tx: Sender<QueryCommand>,
    lifecycle_lock: tokio::sync::Mutex<()>,
    replay_lock: tokio::sync::RwLock<()>,
    join: Mutex<Option<JoinHandle<()>>>,
    shutdown: AtomicBool,
}

/// Cloneable Activity producer/controller. Producers call only [`Self::record`]
/// and never await; lifecycle and replay operations are explicitly async.
#[derive(Clone)]
pub struct ActivityRecorder {
    inner: Arc<RecorderInner>,
}

impl Default for ActivityRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityRecorder {
    pub fn new() -> Self {
        Self::with_batch_sink(Arc::new(NoopBatchSink))
    }

    pub fn with_batch_sink(sink: Arc<dyn ActivityBatchSink>) -> Self {
        let gate = Arc::new(AdmissionGate::new());
        let health = ActivityHealth::new();
        let mirror = Arc::new(StatusMirror::new());
        let shared = Arc::new(WorkerShared {
            gate,
            rate: Arc::new(RateAdmission::new(ProcessClock::new())),
            low_permits: LowPermitPool::new(),
            health,
            mirror,
            trace_deadline: Mutex::new(None),
            available: AtomicBool::new(false),
        });
        let (ingress_tx, ingress_rx) = crossbeam_channel::bounded(INGRESS_CAPACITY);
        let (urgent_tx, urgent_rx) = crossbeam_channel::bounded(URGENT_CAPACITY);
        let (query_tx, query_rx) = crossbeam_channel::bounded(QUERY_CAPACITY);
        let join = spawn_worker(Arc::clone(&shared), sink, ingress_rx, urgent_rx, query_rx).ok();
        if join.is_none() {
            shared.mirror.update(|status| {
                status.worker_state = super::replay::ActivityWorkerState::Unavailable;
            });
        }
        Self {
            inner: Arc::new(RecorderInner {
                shared,
                ingress_tx,
                urgent_tx,
                query_tx,
                lifecycle_lock: tokio::sync::Mutex::new(()),
                replay_lock: tokio::sync::RwLock::new(()),
                join: Mutex::new(join),
                shutdown: AtomicBool::new(false),
            }),
        }
    }

    /// Nonblocking lazy producer path. A closed gate returns before invoking
    /// `make`; producer code cannot select its own priority, capture scope, or
    /// rate domain because all three come from the sealed catalog draft.
    pub fn record<F>(&self, make: F) -> ActivityRecordOutcome
    where
        F: FnOnce() -> Result<ActivityDraft, ActivityRejectReason>,
    {
        if !self.inner.shared.available.load(Ordering::Acquire) {
            return ActivityRecordOutcome::WorkerUnavailable;
        }
        let Some(lease) = self.inner.shared.gate.try_admit() else {
            return ActivityRecordOutcome::CaptureOff;
        };
        let generation = lease.generation();
        let profile = lease.profile();
        if profile == CaptureProfile::Trace
            && lease
                .trace_deadline()
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.inner.shared.gate.close();
            drop(lease);
            return ActivityRecordOutcome::TraceExpired;
        }
        if self.inner.shared.gate.generation() != generation {
            drop(lease);
            return ActivityRecordOutcome::StaleGeneration;
        }

        let draft = match make() {
            Ok(draft) => draft,
            Err(error) => {
                self.inner
                    .shared
                    .health
                    .increment_oversized_invalid_rejected_at(loss_observation_unix_ms());
                return ActivityRecordOutcome::Rejected(error);
            }
        };
        if draft.capture_scope() == CaptureScope::TraceOnly && profile != CaptureProfile::Trace {
            return ActivityRecordOutcome::ProfileFiltered;
        }
        if !self.inner.shared.rate.allow(
            profile,
            draft.severity(),
            draft.rate_domain(),
            draft.is_ambient(),
        ) {
            self.inner
                .shared
                .health
                .increment_rate_limited_at(loss_observation_unix_ms());
            return ActivityRecordOutcome::RateLimited;
        }
        if self.inner.shared.gate.generation() != generation {
            return ActivityRecordOutcome::StaleGeneration;
        }

        let low_permit = if draft.severity() == ActivitySeverity::Info {
            match self.inner.shared.low_permits.try_acquire() {
                Some(permit) => Some(permit),
                None => {
                    self.inner
                        .shared
                        .health
                        .increment_ingress_full_at(loss_observation_unix_ms());
                    return ActivityRecordOutcome::IngressFull;
                }
            }
        } else {
            None
        };
        let envelope = IngressItem::Draft(IngressDraft {
            generation,
            profile,
            draft,
            low_permit,
        });
        match self.inner.ingress_tx.try_send(envelope) {
            Ok(()) => ActivityRecordOutcome::Accepted,
            Err(TrySendError::Full(_)) => {
                self.inner
                    .shared
                    .health
                    .increment_ingress_full_at(loss_observation_unix_ms());
                ActivityRecordOutcome::IngressFull
            }
            Err(TrySendError::Disconnected(_)) => ActivityRecordOutcome::WorkerUnavailable,
        }
    }

    pub fn status(&self) -> ActivityStatusV1 {
        self.inner.shared.status()
    }

    pub async fn start(&self) -> Result<ActivityStatusV1, ActivityRecorderError> {
        let _lifecycle = self.lock_lifecycle().await?;
        if self.status().state() != super::replay::ActivityCaptureState::Off {
            return Err(ActivityRecorderError::InvalidTransition);
        }
        self.inner.shared.gate.close();
        self.wait_quiescent().await?;
        let generation = self
            .inner
            .shared
            .gate
            .advance_generation()
            .map_err(map_gate_error)?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_urgent(UrgentCommand::Start {
            generation,
            reply: reply_tx,
        })
        .await?;
        let ack = await_ack(reply_rx).await??;
        self.apply_open_ack(ack)?;
        Ok(self.status())
    }

    pub async fn stop(&self) -> Result<ActivityStatusV1, ActivityRecorderError> {
        let _lifecycle = self.lock_lifecycle().await?;
        self.inner.shared.gate.close();
        self.wait_quiescent().await?;
        let expected_generation = self.inner.shared.gate.generation();
        let ack = self
            .send_ordered(OrderedBarrierKind::Stop, expected_generation)
            .await?;
        if ack.reopen {
            return Err(ActivityRecorderError::InvalidTransition);
        }
        self.set_trace_deadline(None);
        Ok(self.status())
    }

    pub async fn resume(&self) -> Result<ActivityStatusV1, ActivityRecorderError> {
        let _lifecycle = self.lock_lifecycle().await?;
        if self.status().state() != super::replay::ActivityCaptureState::Stopped {
            return Err(ActivityRecorderError::InvalidTransition);
        }
        self.inner.shared.gate.close();
        self.wait_quiescent().await?;
        let generation = self
            .inner
            .shared
            .gate
            .advance_generation()
            .map_err(map_gate_error)?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_urgent(UrgentCommand::Resume {
            generation,
            reply: reply_tx,
        })
        .await?;
        let ack = await_ack(reply_rx).await??;
        self.apply_open_ack(ack)?;
        Ok(self.status())
    }

    pub async fn clear(&self) -> Result<ActivityStatusV1, ActivityRecorderError> {
        let _lifecycle = self.lock_lifecycle().await?;
        self.inner.shared.gate.close();
        self.wait_quiescent().await?;
        let expected_generation = self.inner.shared.gate.generation();
        let trace_deadline = self.trace_deadline();
        let ack = self
            .send_ordered(
                OrderedBarrierKind::Clear { trace_deadline },
                expected_generation,
            )
            .await?;
        if ack.reopen {
            self.apply_open_ack(ack)?;
        } else {
            self.set_trace_deadline(None);
        }
        Ok(self.status())
    }

    /// Changes future capture only. `None` uses the platform default: ten
    /// minutes on mobile and Until stopped on desktop.
    pub async fn set_profile(
        &self,
        target: CaptureProfile,
        trace_duration: Option<TraceCaptureDuration>,
    ) -> Result<ActivityStatusV1, ActivityRecorderError> {
        let deadline = trace_deadline(target, trace_duration)?;
        let _lifecycle = self.lock_lifecycle().await?;
        self.inner.shared.gate.close();
        self.wait_quiescent().await?;
        let expected_generation = self.inner.shared.gate.generation();
        let ack = self
            .send_ordered(
                OrderedBarrierKind::Profile {
                    target,
                    trace_deadline: deadline,
                },
                expected_generation,
            )
            .await?;
        self.apply_open_ack(ack)?;
        self.schedule_trace_expiry(deadline);
        Ok(self.status())
    }

    /// Foreground integration calls this before waking any producer loops.
    pub async fn expire_trace_if_due(&self) -> Result<ActivityStatusV1, ActivityRecorderError> {
        let deadline = self.trace_deadline();
        if deadline.is_none_or(|deadline| Instant::now() < deadline) {
            return Ok(self.status());
        }
        let _lifecycle = self.lock_lifecycle().await?;
        let deadline = self.trace_deadline();
        if deadline.is_none_or(|deadline| Instant::now() < deadline) {
            return Ok(self.status());
        }
        self.inner.shared.gate.close();
        self.wait_quiescent().await?;
        let expected_generation = self.inner.shared.gate.generation();
        let ack = self
            .send_ordered(
                OrderedBarrierKind::Profile {
                    target: CaptureProfile::Normal,
                    trace_deadline: None,
                },
                expected_generation,
            )
            .await?;
        self.apply_open_ack(ack)?;
        Ok(self.status())
    }

    /// Preemptively closes and rotates ingress generation before waiting for
    /// any ordinary lifecycle operation. Success means the worker has dropped
    /// every privacy-bearing session value and purged pending output.
    pub async fn hard_reset(&self) -> Result<ActivityStatusV1, ActivityRecorderError> {
        let generation = self
            .inner
            .shared
            .gate
            .hard_reset()
            .map_err(map_gate_error)?;
        self.set_trace_deadline(None);
        let _lifecycle = self.lock_lifecycle().await?;
        self.wait_quiescent().await?;
        let _replay = tokio::time::timeout(LIFECYCLE_TIMEOUT, self.inner.replay_lock.write())
            .await
            .map_err(|_| ActivityRecorderError::TimedOut)?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_urgent(UrgentCommand::HardReset {
            generation,
            reply: reply_tx,
        })
        .await?;
        await_ack(reply_rx).await??;
        self.set_trace_deadline(None);
        Ok(self.status())
    }

    pub async fn replay(
        &self,
        capture_session: String,
        after: Option<u64>,
        max_events: usize,
        max_bytes: usize,
    ) -> Result<ActivityReplayResultV1, ActivityRecorderError> {
        if !is_capture_session_token(&capture_session)
            || max_events == 0
            || max_bytes < ACTIVITY_REPLAY_MIN_BYTES
        {
            return Err(ActivityRecorderError::InvalidRequest);
        }
        let _replay = tokio::time::timeout(LIFECYCLE_TIMEOUT, self.inner.replay_lock.read())
            .await
            .map_err(|_| ActivityRecorderError::TimedOut)?;
        let request = ReplayRequest {
            capture_session,
            after,
            max_events: max_events.min(ACTIVITY_REPLAY_MAX_EVENTS),
            max_bytes: max_bytes.min(ACTIVITY_REPLAY_MAX_BYTES),
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        match self.inner.query_tx.try_send(QueryCommand::Replay {
            request,
            reply: reply_tx,
        }) {
            Ok(()) => await_ack(reply_rx).await?,
            Err(TrySendError::Full(_)) => Err(ActivityRecorderError::ControlBusy),
            Err(TrySendError::Disconnected(_)) => Err(ActivityRecorderError::WorkerUnavailable),
        }
    }

    /// App-exit-only finalizer. Identity/runtime transitions use
    /// [`Self::hard_reset`] and leave the process worker available.
    pub async fn shutdown(&self) -> Result<(), ActivityRecorderError> {
        if self.inner.shutdown.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let result = async {
            self.hard_reset().await?;
            let _lifecycle = self.lock_lifecycle().await?;
            let (reply_tx, reply_rx) = oneshot::channel();
            self.send_urgent(UrgentCommand::Shutdown { reply: reply_tx })
                .await?;
            await_ack(reply_rx).await??;
            let join = self
                .inner
                .join
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            if let Some(join) = join {
                let joined = tokio::task::spawn_blocking(move || join.join())
                    .await
                    .map_err(|_| ActivityRecorderError::WorkerUnavailable)?;
                if joined.is_err() {
                    return Err(ActivityRecorderError::WorkerUnavailable);
                }
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            self.inner.shutdown.store(false, Ordering::Release);
        }
        result
    }

    async fn lock_lifecycle(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>, ActivityRecorderError> {
        tokio::time::timeout(LIFECYCLE_TIMEOUT, self.inner.lifecycle_lock.lock())
            .await
            .map_err(|_| ActivityRecorderError::TimedOut)
    }

    async fn wait_quiescent(&self) -> Result<(), ActivityRecorderError> {
        let gate = Arc::clone(&self.inner.shared.gate);
        tokio::time::timeout(
            LIFECYCLE_TIMEOUT,
            tokio::task::spawn_blocking(move || gate.wait_quiescent()),
        )
        .await
        .map_err(|_| ActivityRecorderError::TimedOut)?
        .map_err(|_| ActivityRecorderError::WorkerUnavailable)?
        .map_err(map_gate_error)
    }

    async fn send_ordered(
        &self,
        kind: OrderedBarrierKind,
        expected_generation: u64,
    ) -> Result<BarrierAck, ActivityRecorderError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let item = IngressItem::Barrier(OrderedBarrier {
            expected_generation,
            kind,
            reply: reply_tx,
        });
        let sender = self.inner.ingress_tx.clone();
        let sent = tokio::task::spawn_blocking(move || {
            sender
                .send_timeout(item, LIFECYCLE_TIMEOUT)
                .map_err(|error| match error {
                    SendTimeoutError::Timeout(_) => ActivityRecorderError::TimedOut,
                    SendTimeoutError::Disconnected(_) => ActivityRecorderError::WorkerUnavailable,
                })
        })
        .await
        .map_err(|_| ActivityRecorderError::WorkerUnavailable)?;
        match sent {
            Ok(()) => await_ack(reply_rx).await?,
            Err(error) => Err(error),
        }
    }

    async fn send_urgent(&self, command: UrgentCommand) -> Result<(), ActivityRecorderError> {
        let sender = self.inner.urgent_tx.clone();
        let sent = tokio::task::spawn_blocking(move || {
            sender
                .send_timeout(command, LIFECYCLE_TIMEOUT)
                .map_err(|error| match error {
                    SendTimeoutError::Timeout(_) => ActivityRecorderError::TimedOut,
                    SendTimeoutError::Disconnected(_) => ActivityRecorderError::WorkerUnavailable,
                })
        })
        .await
        .map_err(|_| ActivityRecorderError::WorkerUnavailable)?;
        match sent {
            Ok(()) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn apply_open_ack(&self, ack: BarrierAck) -> Result<(), ActivityRecorderError> {
        if !ack.reopen || self.status().worker_epoch() != ack.worker_epoch {
            return Err(ActivityRecorderError::Superseded);
        }
        self.inner.shared.rate.reset(ack.profile);
        self.inner
            .shared
            .gate
            .open_if_generation(ack.generation, ack.profile, ack.trace_deadline)
            .map_err(map_gate_error)?;
        self.set_trace_deadline_for_generation(ack.generation, ack.trace_deadline);
        Ok(())
    }

    fn trace_deadline(&self) -> Option<Instant> {
        let generation = self.inner.shared.gate.generation();
        self.inner
            .shared
            .trace_deadline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .and_then(|(stored_generation, deadline)| {
                (stored_generation == generation).then_some(deadline)
            })
    }

    fn set_trace_deadline(&self, deadline: Option<Instant>) {
        self.set_trace_deadline_for_generation(self.inner.shared.gate.generation(), deadline);
    }

    fn set_trace_deadline_for_generation(&self, generation: u64, deadline: Option<Instant>) {
        *self
            .inner
            .shared
            .trace_deadline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            deadline.map(|deadline| (generation, deadline));
    }

    fn schedule_trace_expiry(&self, deadline: Option<Instant>) {
        let Some(deadline) = deadline else { return };
        let recorder = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            let _ = recorder.expire_trace_if_due().await;
        });
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) async fn inject_worker_fault(&self) -> Result<(), ActivityRecorderError> {
        self.send_urgent(UrgentCommand::InjectFault).await
    }
}

fn is_capture_session_token(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn loss_observation_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

async fn await_ack<T>(receiver: oneshot::Receiver<T>) -> Result<T, ActivityRecorderError> {
    tokio::time::timeout(LIFECYCLE_TIMEOUT, receiver)
        .await
        .map_err(|_| ActivityRecorderError::TimedOut)?
        .map_err(|_| ActivityRecorderError::WorkerUnavailable)
}

fn trace_deadline(
    profile: CaptureProfile,
    requested: Option<TraceCaptureDuration>,
) -> Result<Option<Instant>, ActivityRecorderError> {
    if profile == CaptureProfile::Normal {
        return if requested.is_none() {
            Ok(None)
        } else {
            Err(ActivityRecorderError::InvalidRequest)
        };
    }
    if requested.is_some_and(
        |duration| matches!(duration, TraceCaptureDuration::Limited(value) if value.is_zero()),
    ) {
        return Err(ActivityRecorderError::InvalidRequest);
    }
    let duration = match requested {
        Some(TraceCaptureDuration::UntilStopped) => None,
        Some(TraceCaptureDuration::Limited(duration)) => Some(duration),
        None => cfg!(any(target_os = "android", target_os = "ios"))
            .then_some(MOBILE_DEFAULT_TRACE_DURATION),
    };
    duration
        .map(|duration| {
            Instant::now()
                .checked_add(duration)
                .ok_or(ActivityRecorderError::InvalidRequest)
        })
        .transpose()
}

fn map_gate_error(error: GateError) -> ActivityRecorderError {
    match error {
        GateError::GenerationExhausted => ActivityRecorderError::GenerationExhausted,
        GateError::GenerationMismatch { .. } | GateError::StateChanged => {
            ActivityRecorderError::Superseded
        }
        GateError::NotClosed
        | GateError::ReadersActive(_)
        | GateError::QuiescenceWaitInProgress
        | GateError::WaiterCountExhausted
        | GateError::UnexpectedTraceDeadline => ActivityRecorderError::InvalidTransition,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use super::super::catalog::{
        self, DeliveryFailureReason, DestinationHash, LxmfDeliveryFailed, MessageId,
        ObservationTime,
    };
    use super::super::classified::{CoalescingPolicy, CorrelationId};
    use super::super::replay::{
        ACTIVITY_BATCH_MAX_BYTES, ACTIVITY_BATCH_MAX_EVENTS, ActivityBatchV1, ActivityCaptureState,
        ActivityPublishError, ActivityReplayResultV1, ActivityWorkerState,
    };
    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        batches: Mutex<Vec<ActivityBatchV1>>,
        fail: AtomicBool,
    }

    impl RecordingSink {
        fn batches(&self) -> Vec<ActivityBatchV1> {
            self.batches
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl ActivityBatchSink for RecordingSink {
        fn try_publish(&self, batch: &ActivityBatchV1) -> Result<(), ActivityPublishError> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(ActivityPublishError::Unavailable);
            }
            self.batches
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(batch.clone());
            Ok(())
        }
    }

    fn normal_draft(value: u8) -> Result<ActivityDraft, ActivityRejectReason> {
        catalog::test_network_event(
            u64::from(value),
            u64::from(value),
            [value; 16],
            "example.net:4242",
            CoalescingPolicy::Never,
        )
    }

    fn coalescible_draft(value: u8) -> Result<ActivityDraft, ActivityRejectReason> {
        catalog::test_network_event(
            u64::from(value),
            u64::from(value),
            [value; 16],
            "example.net:4242",
            CoalescingPolicy::AdjacentEquivalent,
        )
    }

    fn error_draft(value: u8) -> Result<ActivityDraft, ActivityRejectReason> {
        catalog::lxmf_delivery_failed(LxmfDeliveryFailed {
            time: ObservationTime::new(u64::from(value), u64::from(value)),
            message_id: MessageId::new([value; 32]),
            destination: DestinationHash::new([value; 16]),
            link_id: None,
            reason: DeliveryFailureReason::LinkClosed,
            correlation_id: CorrelationId::from_bytes([value; 16]),
        })
    }

    fn large_error_draft(value: u8) -> Result<ActivityDraft, ActivityRejectReason> {
        catalog::test_large_error_event(u64::from(value), u64::from(value))
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

    #[test]
    fn closed_fast_path_never_evaluates_the_lazy_closure() {
        let recorder = ActivityRecorder::new();
        let evaluated = AtomicBool::new(false);
        let outcome = recorder.record(|| {
            evaluated.store(true, Ordering::Relaxed);
            normal_draft(1)
        });
        assert!(matches!(
            outcome,
            ActivityRecordOutcome::CaptureOff | ActivityRecordOutcome::WorkerUnavailable
        ));
        assert!(!evaluated.load(Ordering::Relaxed));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_drains_fifo_flushes_and_replay_retains_the_session() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = ActivityRecorder::with_batch_sink(sink.clone());
        let started = recorder.start().await.unwrap();
        let capture_session = started.capture_session().unwrap().to_string();
        assert_eq!(
            recorder.record(|| normal_draft(7)),
            ActivityRecordOutcome::Accepted
        );

        let stopped = recorder.stop().await.unwrap();
        assert_eq!(stopped.state(), ActivityCaptureState::Stopped);
        assert_eq!(stopped.capture_session(), Some(capture_session.as_str()));
        let batches = sink.batches();
        let kinds: Vec<_> = batches
            .iter()
            .flat_map(|batch| batch.events())
            .map(|event| event.kind())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "diagnostics.capture_started",
                "rns.path.discovered",
                "diagnostics.capture_stopped"
            ]
        );

        let replay = recorder
            .replay(capture_session, None, 50, 64 * 1024)
            .await
            .unwrap();
        let ActivityReplayResultV1::Page { page } = replay else {
            panic!("same session should replay");
        };
        assert_eq!(page.events().len(), 3);
        assert_eq!(page.next_after(), Some(3));
        assert!(!page.has_more());
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_waits_for_an_admitted_producer_and_orders_its_event_before_the_boundary() {
        let recorder = ActivityRecorder::new();
        let session = recorder
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let producer_recorder = recorder.clone();
        let producer = std::thread::spawn(move || {
            producer_recorder.record(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                error_draft(77)
            })
        });
        tokio::task::spawn_blocking(move || {
            entered_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("the producer should hold an admission lease")
        })
        .await
        .unwrap();
        assert_eq!(recorder.inner.shared.gate.active_readers(), 1);

        let stop_recorder = recorder.clone();
        let stop = tokio::spawn(async move { stop_recorder.stop().await });
        wait_until(|| recorder.inner.shared.gate.is_closed()).await;
        assert_eq!(recorder.inner.shared.gate.active_readers(), 1);
        assert!(
            !stop.is_finished(),
            "Stop must wait for the admitted producer"
        );
        assert_eq!(
            recorder.record(|| panic!("a closed gate must stay lazy")),
            ActivityRecordOutcome::CaptureOff
        );

        release_tx.send(()).unwrap();
        let producer_outcome = tokio::task::spawn_blocking(move || producer.join().unwrap())
            .await
            .unwrap();
        assert_eq!(producer_outcome, ActivityRecordOutcome::Accepted);
        let stopped = tokio::time::timeout(Duration::from_secs(2), stop)
            .await
            .expect("Stop should finish after the producer releases its lease")
            .unwrap()
            .unwrap();
        assert_eq!(stopped.state(), ActivityCaptureState::Stopped);

        let ActivityReplayResultV1::Page { page } =
            recorder.replay(session, None, 50, 64 * 1024).await.unwrap()
        else {
            panic!("the stopped session should replay");
        };
        let kinds: Vec<_> = page.events().iter().map(|event| event.kind()).collect();
        assert_eq!(
            kinds,
            vec![
                "diagnostics.capture_started",
                "lxmf.delivery.failed",
                "diagnostics.capture_stopped"
            ]
        );
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resume_preserves_session_and_sequence_but_forces_normal() {
        let recorder = ActivityRecorder::new();
        let started = recorder.start().await.unwrap();
        let session = started.capture_session().unwrap().to_string();
        recorder
            .set_profile(
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::Limited(Duration::from_secs(30))),
            )
            .await
            .unwrap();
        recorder.stop().await.unwrap();
        let resumed = recorder.resume().await.unwrap();
        assert_eq!(resumed.capture_session(), Some(session.as_str()));
        assert_eq!(resumed.profile(), Some(CaptureProfile::Normal));
        assert_eq!(resumed.state(), ActivityCaptureState::Capturing);
        assert_eq!(
            recorder.record(|| normal_draft(8)),
            ActivityRecordOutcome::Accepted
        );
        recorder.stop().await.unwrap();
        let ActivityReplayResultV1::Page { page } =
            recorder.replay(session, None, 50, 64 * 1024).await.unwrap()
        else {
            panic!("same session should replay");
        };
        let resumed_position = page
            .events()
            .iter()
            .position(|event| event.kind() == "diagnostics.capture_resumed")
            .unwrap();
        let record_position = page
            .events()
            .iter()
            .position(|event| event.kind() == "rns.path.discovered")
            .unwrap();
        assert!(resumed_position < record_position);
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clear_discards_pending_batch_and_creates_a_sequence_gap() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = ActivityRecorder::with_batch_sink(sink.clone());
        let started = recorder.start().await.unwrap();
        let session = started.capture_session().unwrap().to_string();
        assert_eq!(
            recorder.record(|| normal_draft(9)),
            ActivityRecordOutcome::Accepted
        );
        let cleared = recorder.clear().await.unwrap();
        assert_eq!(cleared.capture_session(), Some(session.as_str()));
        assert_eq!(cleared.oldest(), Some(3));
        recorder.stop().await.unwrap();

        let kinds: Vec<String> = sink
            .batches()
            .iter()
            .flat_map(|batch| batch.events())
            .map(|event| event.kind().to_string())
            .collect();
        assert!(
            !kinds
                .iter()
                .any(|kind| kind == "diagnostics.capture_started")
        );
        assert!(!kinds.iter().any(|kind| kind == "rns.path.discovered"));
        assert!(
            kinds
                .iter()
                .any(|kind| kind == "diagnostics.capture_cleared")
        );

        let ActivityReplayResultV1::Page { page } = recorder
            .replay(session, Some(0), 50, 64 * 1024)
            .await
            .unwrap()
        else {
            panic!("same session should replay");
        };
        assert!(page.gap());
        assert_eq!(page.events().first().unwrap().sequence(), 3);
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn producer_closes_expired_trace_before_evaluating_the_closure() {
        let recorder = ActivityRecorder::new();
        recorder.start().await.unwrap();
        recorder
            .set_profile(
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::Limited(Duration::from_millis(25))),
            )
            .await
            .unwrap();
        std::thread::sleep(Duration::from_millis(35));

        let evaluated = AtomicBool::new(false);
        let outcome = recorder.record(|| {
            evaluated.store(true, Ordering::Relaxed);
            normal_draft(10)
        });
        assert_eq!(outcome, ActivityRecordOutcome::TraceExpired);
        assert!(!evaluated.load(Ordering::Relaxed));
        let status = recorder.expire_trace_if_due().await.unwrap();
        assert_eq!(status.profile(), Some(CaptureProfile::Normal));
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clear_preserves_a_finite_trace_deadline() {
        let recorder = ActivityRecorder::new();
        recorder.start().await.unwrap();
        recorder
            .set_profile(
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::Limited(Duration::from_millis(35))),
            )
            .await
            .unwrap();
        let cleared = recorder.clear().await.unwrap();
        assert_eq!(cleared.profile(), Some(CaptureProfile::Trace));

        tokio::time::sleep(Duration::from_millis(50)).await;
        let expired = recorder.expire_trace_if_due().await.unwrap();
        assert_eq!(expired.profile(), Some(CaptureProfile::Normal));
        recorder.shutdown().await.unwrap();
    }

    #[test]
    fn explicit_until_stopped_is_distinct_from_the_platform_trace_default() {
        assert_eq!(
            trace_deadline(
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::UntilStopped),
            ),
            Ok(None)
        );
        assert!(matches!(
            trace_deadline(
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::Limited(Duration::ZERO)),
            ),
            Err(ActivityRecorderError::InvalidRequest)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_open_ack_cannot_restore_a_trace_deadline() {
        let recorder = ActivityRecorder::new();
        recorder.start().await.unwrap();
        recorder
            .set_profile(
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::Limited(Duration::from_secs(30))),
            )
            .await
            .unwrap();
        recorder.inner.shared.gate.close();
        recorder.wait_quiescent().await.unwrap();
        let stale_generation = recorder.inner.shared.gate.generation();
        let stale_ack = BarrierAck {
            generation: stale_generation,
            profile: CaptureProfile::Trace,
            trace_deadline: Some(Instant::now() + Duration::from_secs(30)),
            reopen: true,
            worker_epoch: recorder.status().worker_epoch(),
        };
        recorder.inner.shared.gate.advance_generation().unwrap();
        recorder.set_trace_deadline(None);

        assert_eq!(
            recorder.apply_open_ack(stale_ack),
            Err(ActivityRecorderError::Superseded)
        );
        assert_eq!(recorder.trace_deadline(), None);
        recorder.hard_reset().await.unwrap();
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hard_reset_purges_pending_output_and_rotates_the_session() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = ActivityRecorder::with_batch_sink(sink.clone());
        let first = recorder
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();
        assert_eq!(
            recorder.record(|| normal_draft(11)),
            ActivityRecordOutcome::Accepted
        );
        let reset = recorder.hard_reset().await.unwrap();
        assert_eq!(reset.state(), ActivityCaptureState::Off);
        assert!(reset.capture_session().is_none());
        assert!(sink.batches().is_empty());
        let ActivityReplayResultV1::SessionMismatch { status } = recorder
            .replay(first.clone(), None, 50, 64 * 1024)
            .await
            .unwrap()
        else {
            panic!("a reset session must not remain replayable");
        };
        assert!(status.capture_session().is_none());
        assert_eq!(status.state(), ActivityCaptureState::Off);

        let second = recorder
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();
        assert_ne!(first, second);
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hard_reset_ack_waits_for_in_flight_replay_admission() {
        let recorder = ActivityRecorder::new();
        recorder.start().await.unwrap();
        let old_generation = recorder.status().ingress_generation();
        let replay_guard = recorder.inner.replay_lock.read().await;
        let reset_recorder = recorder.clone();
        let reset = tokio::spawn(async move { reset_recorder.hard_reset().await });

        wait_until(|| recorder.inner.shared.gate.generation() > old_generation).await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!reset.is_finished());

        drop(replay_guard);
        let status = reset.await.unwrap().unwrap();
        assert_eq!(status.state(), ActivityCaptureState::Off);
        assert!(status.capture_session().is_none());
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ordered_barrier_cannot_adopt_a_preemptive_reset_generation() {
        let recorder = ActivityRecorder::new();
        recorder.start().await.unwrap();
        recorder.inner.shared.gate.close();
        recorder.wait_quiescent().await.unwrap();
        let reset_generation = recorder.inner.shared.gate.advance_generation().unwrap();

        let result = recorder
            .send_ordered(OrderedBarrierKind::Stop, reset_generation)
            .await;
        assert_eq!(result.err(), Some(ActivityRecorderError::Superseded));
        recorder.hard_reset().await.unwrap();
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fifty_event_threshold_flushes_an_immutable_bounded_batch() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = ActivityRecorder::with_batch_sink(sink.clone());
        recorder.start().await.unwrap();
        for value in 0..49u8 {
            assert_eq!(
                recorder.record(|| error_draft(value)),
                ActivityRecordOutcome::Accepted
            );
        }
        wait_until(|| !sink.batches().is_empty()).await;
        let first = sink.batches().remove(0);
        assert!(first.events().len() <= 50);
        assert!(serde_json::to_vec(&first).unwrap().len() <= 64 * 1024);
        assert_eq!(first.first_sequence(), 1);
        assert_eq!(
            first.last_sequence(),
            first.first_sequence() + first.events().len() as u64 - 1
        );
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn near_limit_events_form_exact_contiguous_byte_bounded_batches() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = ActivityRecorder::with_batch_sink(sink.clone());
        recorder.start().await.unwrap();
        for value in 0..30u8 {
            assert_eq!(
                recorder.record(|| large_error_draft(value)),
                ActivityRecordOutcome::Accepted
            );
        }
        recorder.stop().await.unwrap();

        let batches = sink.batches();
        assert!(
            batches.len() >= 2,
            "the byte ceiling should split the events"
        );
        let mut sequences = Vec::new();
        let mut saw_large_batch = false;
        for batch in &batches {
            let encoded = serde_json::to_vec(batch).unwrap();
            assert!(encoded.len() <= ACTIVITY_BATCH_MAX_BYTES);
            assert!(batch.events().len() <= ACTIVITY_BATCH_MAX_EVENTS);
            saw_large_batch |= encoded.len() > 32 * 1024;

            let event_bytes = batch
                .events()
                .iter()
                .map(|event| serde_json::to_vec(event).unwrap().len())
                .sum();
            assert_eq!(
                ActivityBatchV1::encoded_len_from_parts(
                    batch.capture_session(),
                    batch.first_sequence(),
                    batch.last_sequence(),
                    event_bytes,
                    batch.events().len(),
                ),
                Some(encoded.len())
            );
            sequences.extend(batch.events().iter().map(|event| event.sequence()));
        }
        assert!(saw_large_batch);
        assert_eq!(sequences, (1..=32).collect::<Vec<_>>());
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_batch_flushes_within_the_latency_budget() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = ActivityRecorder::with_batch_sink(sink.clone());
        recorder.start().await.unwrap();

        tokio::time::timeout(Duration::from_millis(500), async {
            while sink.batches().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the 100 ms worker flush should publish without more input");

        let batches = sink.batches();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].events().len(), 1);
        assert_eq!(batches[0].events()[0].kind(), "diagnostics.capture_started");
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn coalescer_does_not_start_a_second_batch_latency_window() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = ActivityRecorder::with_batch_sink(sink.clone());
        recorder.start().await.unwrap();
        wait_until(|| !sink.batches().is_empty()).await;
        let initial_batches = sink.batches().len();

        assert_eq!(
            recorder.record(|| coalescible_draft(41)),
            ActivityRecordOutcome::Accepted
        );
        tokio::time::timeout(Duration::from_millis(180), async {
            while sink.batches().len() == initial_batches {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("a coalesced event must share the original 100 ms budget");

        let kinds: Vec<String> = sink
            .batches()
            .iter()
            .skip(initial_batches)
            .flat_map(|batch| batch.events())
            .map(|event| event.kind().to_string())
            .collect();
        assert!(kinds.iter().any(|kind| kind == "rns.path.discovered"));
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_emitted_event_is_never_mutated_by_later_coalescing() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = ActivityRecorder::with_batch_sink(sink.clone());
        recorder.start().await.unwrap();
        wait_until(|| !sink.batches().is_empty()).await;

        assert_eq!(
            recorder.record(|| coalescible_draft(42)),
            ActivityRecordOutcome::Accepted
        );
        wait_until(|| {
            sink.batches()
                .iter()
                .flat_map(|batch| batch.events())
                .any(|event| event.kind() == "rns.path.discovered")
        })
        .await;

        assert_eq!(
            recorder.record(|| coalescible_draft(42)),
            ActivityRecordOutcome::Accepted
        );
        recorder.stop().await.unwrap();

        let path_events: Vec<_> = sink
            .batches()
            .iter()
            .flat_map(|batch| batch.events().iter().cloned())
            .filter(|event| event.kind() == "rns.path.discovered")
            .collect();
        assert_eq!(path_events.len(), 2);
        assert!(path_events[0].sequence() < path_events[1].sequence());
        for event in path_events {
            assert_eq!(serde_json::to_value(event).unwrap()["count"], 1);
        }
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mismatched_replay_session_returns_current_safe_status() {
        let recorder = ActivityRecorder::new();
        let current = recorder.start().await.unwrap();
        let current_session = current.capture_session().unwrap().to_string();
        let mut foreign_session = current_session.clone().into_bytes();
        foreign_session[0] = if foreign_session[0] == b'0' {
            b'1'
        } else {
            b'0'
        };
        let foreign_session = String::from_utf8(foreign_session).unwrap();

        let result = recorder
            .replay(foreign_session, None, 50, 64 * 1024)
            .await
            .unwrap();
        let ActivityReplayResultV1::SessionMismatch { status } = result else {
            panic!("a foreign capture session must not receive replay data");
        };
        assert_eq!(status.capture_session(), Some(current_session.as_str()));
        assert_eq!(status.state(), ActivityCaptureState::Capturing);
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sink_failure_is_counted_while_ring_replay_remains_available() {
        let sink = Arc::new(RecordingSink::default());
        sink.fail.store(true, Ordering::Relaxed);
        let recorder = ActivityRecorder::with_batch_sink(sink);
        let session = recorder
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();
        recorder.stop().await.unwrap();
        assert_eq!(recorder.status().counters().ipc_failure(), "1");
        let ActivityReplayResultV1::Page { page } =
            recorder.replay(session, None, 50, 64 * 1024).await.unwrap()
        else {
            panic!("failed publish must still replay");
        };
        assert_eq!(page.events().len(), 2);
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_ack_is_the_final_publish_after_a_queued_loss_marker() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = ActivityRecorder::with_batch_sink(sink.clone());
        recorder.start().await.unwrap();
        recorder
            .inner
            .shared
            .health
            .increment_rate_limited_at(1_000);
        recorder
            .inner
            .shared
            .health
            .increment_ingress_full_at(1_042);

        let stopped = recorder.stop().await.unwrap();
        assert_eq!(stopped.counters().rate_limited(), "1");
        assert_eq!(stopped.counters().ingress_full(), "1");
        let batches_at_ack = sink.batches();
        let dropped = batches_at_ack
            .iter()
            .flat_map(|batch| batch.events())
            .find(|event| event.kind() == "diagnostics.dropped")
            .expect("Stop must publish the pending loss marker");
        let dropped_json = serde_json::to_value(dropped).unwrap();
        let unsigned_attribute = |key: &str| {
            dropped_json["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|attribute| attribute["key"] == key)
                .and_then(|attribute| attribute["value"]["value"].as_u64())
                .unwrap()
        };
        assert_eq!(unsigned_attribute("dropped_count"), 2);
        assert_eq!(unsigned_attribute("time_span_ms"), 42);
        let final_kind = batches_at_ack
            .last()
            .and_then(|batch| batch.events().last())
            .map(|event| event.kind());
        assert_eq!(final_kind, Some("diagnostics.capture_stopped"));
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(sink.batches().len(), batches_at_ack.len());
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervised_fault_drops_old_privacy_and_restarts_closed() {
        let recorder = ActivityRecorder::new();
        let first = recorder
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();
        recorder.inject_worker_fault().await.unwrap();
        wait_until(|| {
            matches!(
                recorder.status().worker_state(),
                ActivityWorkerState::Recovered | ActivityWorkerState::Unavailable
            )
        })
        .await;
        let recovered = recorder.status();
        assert_eq!(recovered.worker_state(), ActivityWorkerState::Recovered);
        assert_eq!(recovered.state(), ActivityCaptureState::Off);
        assert!(recovered.capture_session().is_none());
        assert_eq!(recovered.counters().worker_recovery(), "1");

        let second = recorder
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();
        assert_ne!(first, second);
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervised_fault_discards_the_trace_deadline_side_state() {
        let recorder = ActivityRecorder::new();
        recorder.start().await.unwrap();
        recorder
            .set_profile(
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::Limited(Duration::from_secs(30))),
            )
            .await
            .unwrap();
        assert!(recorder.trace_deadline().is_some());

        recorder.inject_worker_fault().await.unwrap();
        wait_until(|| recorder.status().worker_state() == ActivityWorkerState::Recovered).await;
        assert_eq!(recorder.trace_deadline(), None);
        let status = recorder.expire_trace_if_due().await.unwrap();
        assert_eq!(status.state(), ActivityCaptureState::Off);
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_error_producers_receive_fifo_observation_sequences() {
        let recorder = ActivityRecorder::new();
        let session = recorder
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();
        let accepted = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for worker in 0..8u8 {
            let recorder = recorder.clone();
            let accepted = Arc::clone(&accepted);
            workers.push(std::thread::spawn(move || {
                for offset in 0..8u8 {
                    if recorder.record(|| error_draft(worker * 8 + offset))
                        == ActivityRecordOutcome::Accepted
                    {
                        accepted.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        recorder.stop().await.unwrap();
        assert_eq!(accepted.load(Ordering::Relaxed), 64);
        let ActivityReplayResultV1::Page { page: first } = recorder
            .replay(session.clone(), None, 50, 64 * 1024)
            .await
            .unwrap()
        else {
            panic!("same session should replay");
        };
        let ActivityReplayResultV1::Page { page: second } = recorder
            .replay(session, first.next_after(), 50, 64 * 1024)
            .await
            .unwrap()
        else {
            panic!("second page should replay");
        };
        let sequences: Vec<_> = first
            .events()
            .iter()
            .chain(second.events())
            .map(|event| event.sequence())
            .collect();
        assert_eq!(sequences, (1..=66).collect::<Vec<_>>());
        recorder.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hundred_thousand_event_flood_stays_bounded_and_reports_exact_loss() {
        const ATTEMPTS: usize = 100_000;

        let sink = Arc::new(RecordingSink::default());
        let recorder = ActivityRecorder::with_batch_sink(sink.clone());
        let session = recorder
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();
        let producer_recorder = recorder.clone();
        let producer = std::thread::spawn(move || {
            let mut counts = [0u64; 4];
            for index in 0..ATTEMPTS {
                let slot = match producer_recorder.record(|| error_draft((index % 251) as u8)) {
                    ActivityRecordOutcome::Accepted => 0,
                    ActivityRecordOutcome::IngressFull => 1,
                    ActivityRecordOutcome::CaptureOff => 2,
                    ActivityRecordOutcome::StaleGeneration => 3,
                    outcome => panic!("unexpected flood outcome: {outcome:?}"),
                };
                counts[slot] += 1;
            }
            counts
        });
        let [accepted, ingress_full, capture_off, stale_generation] =
            tokio::task::spawn_blocking(move || producer.join().unwrap())
                .await
                .unwrap();

        let replay = tokio::time::timeout(
            Duration::from_secs(5),
            recorder.replay(session, None, 50, 64 * 1024),
        )
        .await
        .expect("Replay must remain responsive with a flooded ingress FIFO")
        .unwrap();
        assert!(matches!(replay, ActivityReplayResultV1::Page { .. }));
        let stopped = tokio::time::timeout(Duration::from_secs(5), recorder.stop())
            .await
            .expect("Stop must drain the flooded ingress FIFO")
            .unwrap();
        assert_eq!(
            accepted + ingress_full + capture_off + stale_generation,
            ATTEMPTS as u64
        );
        assert!(
            ingress_full > 0,
            "the stress run must exercise loss accounting"
        );
        let counted_full = stopped.counters().ingress_full().parse::<u64>().unwrap();
        assert_eq!(counted_full, ingress_full);
        let retained_span = stopped
            .oldest()
            .zip(stopped.latest())
            .map(|(oldest, latest)| latest.saturating_sub(oldest).saturating_add(1))
            .unwrap_or(0);
        assert!(retained_span <= 5_000);
        assert!(
            sink.batches()
                .iter()
                .flat_map(|batch| batch.events())
                .any(|event| event.kind() == "diagnostics.dropped")
        );
        recorder.shutdown().await.unwrap();
    }
}
