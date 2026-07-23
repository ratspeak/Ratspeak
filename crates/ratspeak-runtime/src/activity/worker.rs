//! Supervised single-consumer Activity worker.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use tokio::sync::oneshot;

use super::admission::{LowPermit, LowPermitPool, RateAdmission};
use super::catalog::{self, DiagnosticsDropped, DiagnosticsEvicted, ObservationTime};
use super::classified::{ActivityDraft, DraftContext, ReadyDraft};
use super::coalesce::{CoalesceOutput, PreflushCoalescer};
use super::gate::AdmissionGate;
use super::health::ActivityHealth;
use super::pseudonym::{CapturePrivacy, StoredEventV1};
use super::query::{ActivityDetailResultV1, ActivityRevealResultV1, ActivitySafeCopyResultV1};
use super::replay::{
    ACTIVITY_BATCH_MAX_BYTES, ACTIVITY_BATCH_MAX_EVENTS, ACTIVITY_BATCH_MAX_LATENCY_MS,
    ActivityBatchSink, ActivityBatchV1, ActivityCaptureState, ActivityRecorderError,
    ActivityReplayResultV1, ActivityReplayV1, ActivityStatusV1, ActivityWorkerState, StatusMirror,
};
use super::ring::{ActivityRing, RingError, RingPush};
use super::schema::{ActivityAttributeKey, CaptureProfile, CaptureScope, MAX_ENCODED_EVENT_BYTES};

const WORKER_TICK: Duration = Duration::from_millis(10);
const MAX_INGRESS_BURST_BEFORE_SERVICE: usize = 16;

pub(super) struct IngressDraft {
    pub(super) generation: u64,
    pub(super) profile: CaptureProfile,
    pub(super) draft: ActivityDraft,
    pub(super) low_permit: Option<LowPermit>,
}

pub(super) enum OrderedBarrierKind {
    Stop,
    Clear {
        trace_deadline: Option<Instant>,
    },
    Profile {
        target: CaptureProfile,
        trace_deadline: Option<Instant>,
    },
}

pub(super) struct OrderedBarrier {
    pub(super) expected_generation: u64,
    pub(super) kind: OrderedBarrierKind,
    pub(super) reply: oneshot::Sender<Result<BarrierAck, ActivityRecorderError>>,
}

pub(super) enum IngressItem {
    Draft(IngressDraft),
    Barrier(OrderedBarrier),
}

#[derive(Clone, Copy)]
pub(super) struct BarrierAck {
    pub(super) generation: u64,
    pub(super) profile: CaptureProfile,
    pub(super) trace_deadline: Option<Instant>,
    pub(super) reopen: bool,
    pub(super) worker_epoch: u64,
}

pub(super) enum UrgentCommand {
    Start {
        generation: u64,
        reply: oneshot::Sender<Result<BarrierAck, ActivityRecorderError>>,
    },
    Resume {
        generation: u64,
        reply: oneshot::Sender<Result<BarrierAck, ActivityRecorderError>>,
    },
    HardReset {
        generation: u64,
        reply: oneshot::Sender<Result<(), ActivityRecorderError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), ActivityRecorderError>>,
    },
    #[cfg(test)]
    #[allow(dead_code)]
    InjectFault,
}

pub(super) struct ReplayRequest {
    pub(super) capture_session: String,
    pub(super) after: Option<u64>,
    pub(super) max_events: usize,
    pub(super) max_bytes: usize,
}

pub(super) struct EventQueryRequest {
    pub(super) capture_session: String,
    pub(super) sequence: u64,
}

pub(super) enum QueryCommand {
    Replay {
        request: ReplayRequest,
        reply: oneshot::Sender<Result<ActivityReplayResultV1, ActivityRecorderError>>,
    },
    Detail {
        request: EventQueryRequest,
        reply: oneshot::Sender<Result<ActivityDetailResultV1, ActivityRecorderError>>,
    },
    Reveal {
        request: EventQueryRequest,
        key: ActivityAttributeKey,
        reply: oneshot::Sender<Result<ActivityRevealResultV1, ActivityRecorderError>>,
    },
    SafeCopy {
        request: EventQueryRequest,
        reply: oneshot::Sender<Result<ActivitySafeCopyResultV1, ActivityRecorderError>>,
    },
}

pub(super) struct WorkerShared {
    pub(super) gate: Arc<AdmissionGate>,
    pub(super) rate: Arc<RateAdmission>,
    pub(super) observation_clock: Arc<dyn super::catalog::ActivityClock>,
    pub(super) low_permits: Arc<LowPermitPool>,
    pub(super) health: Arc<ActivityHealth>,
    pub(super) mirror: Arc<StatusMirror>,
    /// Finite Trace deadlines are tagged with their ingress generation so a
    /// stale lifecycle acknowledgement cannot revive an expired session.
    pub(super) trace_deadline: Mutex<Option<(u64, Instant)>>,
    pub(super) available: AtomicBool,
}

impl WorkerShared {
    pub(super) fn status(&self) -> ActivityStatusV1 {
        let generation = self.gate.generation();
        let trace_deadline = self
            .trace_deadline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .and_then(|(stored_generation, deadline)| {
                (stored_generation == generation).then_some(deadline)
            });
        self.mirror.snapshot(self.health.snapshot(), trace_deadline)
    }
}

pub(super) fn spawn_worker(
    shared: Arc<WorkerShared>,
    sink: Arc<dyn ActivityBatchSink>,
    ingress_rx: Receiver<IngressItem>,
    urgent_rx: Receiver<UrgentCommand>,
    query_rx: Receiver<QueryCommand>,
) -> Result<JoinHandle<()>, ActivityRecorderError> {
    std::thread::Builder::new()
        .name("ratspeak-activity".to_string())
        .spawn(move || supervisor(shared, sink, ingress_rx, urgent_rx, query_rx))
        .map_err(|_| ActivityRecorderError::WorkerUnavailable)
}

fn supervisor(
    shared: Arc<WorkerShared>,
    sink: Arc<dyn ActivityBatchSink>,
    ingress_rx: Receiver<IngressItem>,
    urgent_rx: Receiver<UrgentCommand>,
    query_rx: Receiver<QueryCommand>,
) {
    let mut worker_epoch = 1u64;
    let mut recovery_marker_pending = false;
    shared.available.store(true, Ordering::Release);
    shared.mirror.update(|status| {
        status.worker_state = ActivityWorkerState::Running;
        status.worker_epoch = worker_epoch;
    });

    loop {
        let exit = catch_unwind(AssertUnwindSafe(|| {
            WorkerCore::new(
                Arc::clone(&shared),
                Arc::clone(&sink),
                ingress_rx.clone(),
                urgent_rx.clone(),
                query_rx.clone(),
                worker_epoch,
                recovery_marker_pending,
            )
            .run()
        }));

        match exit {
            Ok(WorkerExit::Shutdown) | Ok(WorkerExit::Disconnected) => {
                shared.available.store(false, Ordering::Release);
                shared.mirror.update(|status| {
                    status.worker_state = ActivityWorkerState::Shutdown;
                    status.capture_session = None;
                    status.state = ActivityCaptureState::Off;
                    status.profile = None;
                    status.oldest = None;
                    status.latest = None;
                });
                publish_status(&shared, sink.as_ref());
                return;
            }
            Err(_) => {
                shared.available.store(false, Ordering::Release);
                shared.health.increment_worker_recovery();
                shared.mirror.update(|status| {
                    status.worker_state = ActivityWorkerState::Recovering;
                    status.capture_session = None;
                    status.state = ActivityCaptureState::Off;
                    status.profile = None;
                    status.oldest = None;
                    status.latest = None;
                });
                publish_status(&shared, sink.as_ref());

                let generation = match shared.gate.hard_reset() {
                    Ok(generation) => generation,
                    Err(_) => {
                        shared.mirror.update(|status| {
                            status.worker_state = ActivityWorkerState::Unavailable;
                        });
                        publish_status(&shared, sink.as_ref());
                        reject_all(
                            &ingress_rx,
                            &urgent_rx,
                            &query_rx,
                            ActivityRecorderError::WorkerUnavailable,
                        );
                        return;
                    }
                };
                *shared
                    .trace_deadline
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                let _ = shared.gate.wait_quiescent();
                let _ = shared.health.take_loss_window();
                reject_all(
                    &ingress_rx,
                    &urgent_rx,
                    &query_rx,
                    ActivityRecorderError::WorkerUnavailable,
                );

                worker_epoch = match worker_epoch.checked_add(1) {
                    Some(epoch) => epoch,
                    None => {
                        shared.mirror.update(|status| {
                            status.worker_state = ActivityWorkerState::Unavailable;
                            status.ingress_generation = generation;
                        });
                        publish_status(&shared, sink.as_ref());
                        return;
                    }
                };
                recovery_marker_pending = true;
                shared.available.store(true, Ordering::Release);
                shared.mirror.update(|status| {
                    status.worker_state = ActivityWorkerState::Recovered;
                    status.worker_epoch = worker_epoch;
                    status.ingress_generation = generation;
                });
                publish_status(&shared, sink.as_ref());
            }
        }
    }
}

fn reject_all(
    ingress_rx: &Receiver<IngressItem>,
    urgent_rx: &Receiver<UrgentCommand>,
    query_rx: &Receiver<QueryCommand>,
    error: ActivityRecorderError,
) {
    loop {
        match ingress_rx.try_recv() {
            Ok(IngressItem::Draft(_)) => {}
            Ok(IngressItem::Barrier(barrier)) => {
                let _ = barrier.reply.send(Err(error));
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    while let Ok(command) = urgent_rx.try_recv() {
        reject_urgent(command, error);
    }
    while let Ok(query) = query_rx.try_recv() {
        reject_query(query, error);
    }
}

fn publish_status(shared: &WorkerShared, sink: &dyn ActivityBatchSink) {
    if sink.try_publish_status(&shared.status()).is_err() {
        shared.health.increment_ipc_failure();
    }
}

fn reject_urgent(command: UrgentCommand, error: ActivityRecorderError) {
    match command {
        UrgentCommand::Start { reply, .. } | UrgentCommand::Resume { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        UrgentCommand::HardReset { reply, .. } | UrgentCommand::Shutdown { reply } => {
            let _ = reply.send(Err(error));
        }
        #[cfg(test)]
        UrgentCommand::InjectFault => {}
    }
}

fn reject_query(query: QueryCommand, error: ActivityRecorderError) {
    match query {
        QueryCommand::Replay { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        QueryCommand::Detail { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        QueryCommand::Reveal { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        QueryCommand::SafeCopy { reply, .. } => {
            let _ = reply.send(Err(error));
        }
    }
}

enum WorkerExit {
    Shutdown,
    Disconnected,
}

struct WorkerCore {
    shared: Arc<WorkerShared>,
    sink: Arc<dyn ActivityBatchSink>,
    ingress_rx: Receiver<IngressItem>,
    urgent_rx: Receiver<UrgentCommand>,
    query_rx: Receiver<QueryCommand>,
    tick_rx: Receiver<Instant>,
    session: Option<SessionCore>,
    worker_epoch: u64,
    recovery_marker_pending: bool,
}

impl WorkerCore {
    #[allow(clippy::too_many_arguments)]
    fn new(
        shared: Arc<WorkerShared>,
        sink: Arc<dyn ActivityBatchSink>,
        ingress_rx: Receiver<IngressItem>,
        urgent_rx: Receiver<UrgentCommand>,
        query_rx: Receiver<QueryCommand>,
        worker_epoch: u64,
        recovery_marker_pending: bool,
    ) -> Self {
        Self {
            shared,
            sink,
            ingress_rx,
            urgent_rx,
            query_rx,
            tick_rx: crossbeam_channel::tick(WORKER_TICK),
            session: None,
            worker_epoch,
            recovery_marker_pending,
        }
    }

    fn run(mut self) -> WorkerExit {
        let mut ingress_burst = 0usize;
        loop {
            match self.urgent_rx.try_recv() {
                Ok(command) => {
                    if self.handle_urgent(command) {
                        return WorkerExit::Shutdown;
                    }
                    continue;
                }
                Err(TryRecvError::Disconnected) => return WorkerExit::Disconnected,
                Err(TryRecvError::Empty) => {}
            }

            if ingress_burst >= MAX_INGRESS_BURST_BEFORE_SERVICE {
                let mut serviced = false;
                if let Ok(query) = self.query_rx.try_recv() {
                    self.handle_query(query);
                    serviced = true;
                }
                if self.tick_rx.try_recv().is_ok() {
                    self.on_tick();
                    serviced = true;
                }
                ingress_burst = 0;
                if serviced {
                    continue;
                }
            }

            crossbeam_channel::select_biased! {
                recv(self.urgent_rx) -> command => match command {
                    Ok(command) => {
                        if self.handle_urgent(command) {
                            return WorkerExit::Shutdown;
                        }
                    }
                    Err(_) => return WorkerExit::Disconnected,
                },
                recv(self.ingress_rx) -> item => match item {
                    Ok(item) => {
                        self.handle_ingress(item);
                        ingress_burst = ingress_burst.saturating_add(1);
                    },
                    Err(_) => return WorkerExit::Disconnected,
                },
                recv(self.query_rx) -> query => match query {
                    Ok(query) => {
                        self.handle_query(query);
                        ingress_burst = 0;
                    },
                    Err(_) => return WorkerExit::Disconnected,
                },
                recv(self.tick_rx) -> _ => {
                    self.on_tick();
                    ingress_burst = 0;
                },
            }
        }
    }

    fn handle_urgent(&mut self, command: UrgentCommand) -> bool {
        match command {
            UrgentCommand::Start { generation, reply } => {
                let result = self.start(generation);
                let _ = reply.send(result);
                false
            }
            UrgentCommand::Resume { generation, reply } => {
                let result = self.resume(generation);
                let _ = reply.send(result);
                false
            }
            UrgentCommand::HardReset { generation, reply } => {
                let result = self.hard_reset(generation);
                let _ = reply.send(result);
                false
            }
            UrgentCommand::Shutdown { reply } => {
                self.session = None;
                *self
                    .shared
                    .trace_deadline
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                let _ = reply.send(Ok(()));
                true
            }
            #[cfg(test)]
            UrgentCommand::InjectFault => panic!("injected activity worker fault"),
        }
    }

    fn start(&mut self, generation: u64) -> Result<BarrierAck, ActivityRecorderError> {
        if self.session.is_some() || self.shared.gate.generation() != generation {
            return Err(if self.session.is_some() {
                ActivityRecorderError::InvalidTransition
            } else {
                ActivityRecorderError::Superseded
            });
        }
        let privacy = CapturePrivacy::random();
        let ring =
            ActivityRing::platform_default().map_err(|_| ActivityRecorderError::RingUnavailable)?;
        let _ = self.shared.health.take_loss_window();
        let mut session = SessionCore::new(privacy, ring, generation, CaptureProfile::Normal);
        let now = self.observation_time();
        session.commit_control(
            catalog::diagnostics_capture_started(now, CaptureProfile::Normal),
            &self.shared,
            self.sink.as_ref(),
        )?;
        if self.recovery_marker_pending {
            session.commit_control(
                catalog::diagnostics_worker_recovered(self.observation_time()),
                &self.shared,
                self.sink.as_ref(),
            )?;
            self.recovery_marker_pending = false;
        }
        self.session = Some(session);
        self.publish_session(ActivityCaptureState::Capturing);
        publish_status(&self.shared, self.sink.as_ref());
        Ok(self.open_ack(generation, CaptureProfile::Normal, None, true))
    }

    fn resume(&mut self, generation: u64) -> Result<BarrierAck, ActivityRecorderError> {
        if self.shared.gate.generation() != generation {
            return Err(ActivityRecorderError::Superseded);
        }
        let now = self.observation_time();
        let session = self
            .session
            .as_mut()
            .ok_or(ActivityRecorderError::InvalidTransition)?;
        if session.state != ActivityCaptureState::Stopped {
            return Err(ActivityRecorderError::InvalidTransition);
        }
        session.generation = generation;
        session.profile = CaptureProfile::Normal;
        session.state = ActivityCaptureState::Capturing;
        session.commit_control(
            catalog::diagnostics_capture_resumed(now),
            &self.shared,
            self.sink.as_ref(),
        )?;
        self.publish_session(ActivityCaptureState::Capturing);
        publish_status(&self.shared, self.sink.as_ref());
        Ok(self.open_ack(generation, CaptureProfile::Normal, None, true))
    }

    fn hard_reset(&mut self, generation: u64) -> Result<(), ActivityRecorderError> {
        if self.shared.gate.generation() != generation || !self.shared.gate.is_closed() {
            return Err(ActivityRecorderError::Superseded);
        }
        self.session = None;
        *self
            .shared
            .trace_deadline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let _ = self.shared.health.take_loss_window();
        self.drain_ingress(ActivityRecorderError::Superseded);
        self.drain_queries(ActivityRecorderError::Superseded);
        self.shared.mirror.update(|status| {
            status.capture_session = None;
            status.state = ActivityCaptureState::Off;
            status.profile = None;
            status.ingress_generation = generation;
            status.oldest = None;
            status.latest = None;
            status.worker_state = ActivityWorkerState::Running;
            status.worker_epoch = self.worker_epoch;
        });
        publish_status(&self.shared, self.sink.as_ref());
        Ok(())
    }

    fn drain_ingress(&self, error: ActivityRecorderError) {
        loop {
            match self.ingress_rx.try_recv() {
                Ok(IngressItem::Draft(_)) => {}
                Ok(IngressItem::Barrier(barrier)) => {
                    let _ = barrier.reply.send(Err(error));
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    fn drain_queries(&self, error: ActivityRecorderError) {
        while let Ok(query) = self.query_rx.try_recv() {
            reject_query(query, error);
        }
    }

    fn handle_ingress(&mut self, item: IngressItem) {
        match item {
            IngressItem::Draft(mut envelope) => {
                // Release the 960-slot admission permit as soon as the channel
                // position itself is no longer occupied.
                drop(envelope.low_permit.take());
                self.process_draft(envelope);
            }
            IngressItem::Barrier(barrier) => {
                let result = self.process_barrier(&barrier);
                let _ = barrier.reply.send(result);
            }
        }
    }

    fn process_draft(&mut self, envelope: IngressDraft) {
        let now = self.observation_time();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.state != ActivityCaptureState::Capturing
            || envelope.generation != session.generation
            || envelope.profile != session.profile
        {
            return;
        }
        if envelope.draft.capture_scope() == CaptureScope::TraceOnly
            && session.profile != CaptureProfile::Trace
        {
            return;
        }
        session.flush_health_markers(now, &self.shared, self.sink.as_ref());
        let context = DraftContext {
            capture_session: session.privacy.capture_session().to_string(),
            capture_generation: envelope.generation,
            capture_profile: envelope.profile,
        };
        let validated = match envelope.draft.validate(context) {
            Ok(validated) => validated,
            Err(_) => {
                self.shared
                    .health
                    .increment_oversized_invalid_rejected_at(now.unix_ms());
                return;
            }
        };
        session.coalescer_since.get_or_insert_with(Instant::now);
        match session.coalescer.push(validated) {
            CoalesceOutput::Held => {}
            CoalesceOutput::Merged { absorbed } => {
                self.shared.health.add_coalesced_inputs(u64::from(absorbed));
            }
            CoalesceOutput::One(ready) => {
                session.coalescer_since = (!session.coalescer.is_empty()).then_some(Instant::now());
                let _ = session.commit_ready(ready, true, true, &self.shared, self.sink.as_ref());
                session.flush_health_markers(now, &self.shared, self.sink.as_ref());
            }
            CoalesceOutput::Two(first, second) => {
                session.coalescer_since = None;
                let _ = session.commit_ready(first, true, true, &self.shared, self.sink.as_ref());
                session.flush_health_markers(now, &self.shared, self.sink.as_ref());
                session.flush_batch_if_full(&self.shared, self.sink.as_ref());
                let _ = session.commit_ready(second, true, true, &self.shared, self.sink.as_ref());
                session.flush_health_markers(now, &self.shared, self.sink.as_ref());
            }
        }
        session.flush_batch_if_full(&self.shared, self.sink.as_ref());
        self.publish_session(ActivityCaptureState::Capturing);
    }

    fn process_barrier(
        &mut self,
        barrier: &OrderedBarrier,
    ) -> Result<BarrierAck, ActivityRecorderError> {
        if self.shared.gate.generation() != barrier.expected_generation
            || self
                .session
                .as_ref()
                .is_none_or(|session| session.generation != barrier.expected_generation)
        {
            return Err(ActivityRecorderError::Superseded);
        }
        match barrier.kind {
            OrderedBarrierKind::Stop => self.stop(barrier.expected_generation),
            OrderedBarrierKind::Clear { trace_deadline } => {
                self.clear(barrier.expected_generation, trace_deadline)
            }
            OrderedBarrierKind::Profile {
                target,
                trace_deadline,
            } => self.profile(barrier.expected_generation, target, trace_deadline),
        }
    }

    fn stop(&mut self, generation: u64) -> Result<BarrierAck, ActivityRecorderError> {
        let now = self.observation_time();
        let session = self
            .session
            .as_mut()
            .ok_or(ActivityRecorderError::InvalidTransition)?;
        if session.state != ActivityCaptureState::Capturing {
            return Err(ActivityRecorderError::InvalidTransition);
        }
        session.flush_coalescer(&self.shared, self.sink.as_ref())?;
        session.flush_health_markers(now, &self.shared, self.sink.as_ref());
        session.commit_control(
            catalog::diagnostics_capture_stopped(now, session.profile),
            &self.shared,
            self.sink.as_ref(),
        )?;
        session.flush_batch(&self.shared, self.sink.as_ref());
        session.state = ActivityCaptureState::Stopped;
        let profile = session.profile;
        self.publish_session(ActivityCaptureState::Stopped);
        publish_status(&self.shared, self.sink.as_ref());
        Ok(self.open_ack(generation, profile, None, false))
    }

    fn clear(
        &mut self,
        expected_generation: u64,
        trace_deadline: Option<Instant>,
    ) -> Result<BarrierAck, ActivityRecorderError> {
        let (state, profile) = self
            .session
            .as_ref()
            .map(|session| (session.state, session.profile))
            .ok_or(ActivityRecorderError::InvalidTransition)?;
        if !matches!(
            state,
            ActivityCaptureState::Capturing | ActivityCaptureState::Stopped
        ) {
            return Err(ActivityRecorderError::InvalidTransition);
        }
        if self.shared.gate.generation() != expected_generation {
            return Err(ActivityRecorderError::Superseded);
        }
        if profile == CaptureProfile::Normal && trace_deadline.is_some() {
            return Err(ActivityRecorderError::InvalidTransition);
        }
        let generation = self
            .shared
            .gate
            .advance_generation()
            .map_err(map_gate_error)?;
        let now = self.observation_time();
        let session = self.session.as_mut().expect("checked above");
        session.discard_pending();
        let _ = self.shared.health.take_loss_window();
        session.ring.clear();
        session.generation = generation;
        session.commit_control(
            catalog::diagnostics_capture_cleared(now, session.profile),
            &self.shared,
            self.sink.as_ref(),
        )?;
        let reopen = state == ActivityCaptureState::Capturing;
        self.publish_session(state);
        publish_status(&self.shared, self.sink.as_ref());
        Ok(self.open_ack(generation, profile, trace_deadline, reopen))
    }

    fn profile(
        &mut self,
        expected_generation: u64,
        target: CaptureProfile,
        trace_deadline: Option<Instant>,
    ) -> Result<BarrierAck, ActivityRecorderError> {
        if target == CaptureProfile::Normal && trace_deadline.is_some() {
            return Err(ActivityRecorderError::InvalidTransition);
        }
        let current_state = self
            .session
            .as_ref()
            .map(|session| session.state)
            .ok_or(ActivityRecorderError::InvalidTransition)?;
        if current_state != ActivityCaptureState::Capturing {
            return Err(ActivityRecorderError::InvalidTransition);
        }
        if self.shared.gate.generation() != expected_generation {
            return Err(ActivityRecorderError::Superseded);
        }
        {
            let preflush_time = self.observation_time();
            let session = self.session.as_mut().expect("checked above");
            session.flush_coalescer(&self.shared, self.sink.as_ref())?;
            session.flush_health_markers(preflush_time, &self.shared, self.sink.as_ref());
            session.flush_batch(&self.shared, self.sink.as_ref());
        }
        let generation = self
            .shared
            .gate
            .advance_generation()
            .map_err(map_gate_error)?;
        let now = self.observation_time();
        let session = self.session.as_mut().expect("checked above");
        session.generation = generation;
        session.profile = target;
        session.commit_control(
            catalog::diagnostics_profile_changed(now, target),
            &self.shared,
            self.sink.as_ref(),
        )?;
        self.publish_session(ActivityCaptureState::Capturing);
        publish_status(&self.shared, self.sink.as_ref());
        Ok(self.open_ack(generation, target, trace_deadline, true))
    }

    fn handle_query(&mut self, query: QueryCommand) {
        match query {
            QueryCommand::Replay { request, reply } => {
                let result = self.replay(request);
                let _ = reply.send(result);
            }
            QueryCommand::Detail { request, reply } => {
                let result = self.detail(&request);
                let _ = reply.send(result);
            }
            QueryCommand::Reveal {
                request,
                key,
                reply,
            } => {
                let result = self.reveal(&request, key);
                let _ = reply.send(result);
            }
            QueryCommand::SafeCopy { request, reply } => {
                let result = self.safe_copy(&request);
                let _ = reply.send(result);
            }
        }
    }

    fn event_lookup<'a>(&'a self, request: &EventQueryRequest) -> EventLookup<'a> {
        let Some(session) = self.session.as_ref() else {
            return EventLookup::SessionMismatch;
        };
        if request.capture_session != session.privacy.capture_session() {
            return EventLookup::SessionMismatch;
        }
        session
            .ring
            .get(request.sequence)
            .map_or(EventLookup::NotFound, EventLookup::Found)
    }

    fn detail(
        &self,
        request: &EventQueryRequest,
    ) -> Result<ActivityDetailResultV1, ActivityRecorderError> {
        Ok(match self.event_lookup(request) {
            EventLookup::Found(event) => ActivityDetailResultV1::found(event.masked()),
            EventLookup::NotFound => ActivityDetailResultV1::not_found(),
            EventLookup::SessionMismatch => {
                ActivityDetailResultV1::session_mismatch(self.shared.status())
            }
        })
    }

    fn reveal(
        &self,
        request: &EventQueryRequest,
        key: ActivityAttributeKey,
    ) -> Result<ActivityRevealResultV1, ActivityRecorderError> {
        Ok(match self.event_lookup(request) {
            EventLookup::Found(event) => {
                if let Some(identifier) = event.reveal_identifier(key) {
                    ActivityRevealResultV1::identifier(
                        key,
                        identifier.kind,
                        hex::encode(identifier.raw),
                    )
                } else if let Some(endpoint) = event.reveal_endpoint(key) {
                    ActivityRevealResultV1::endpoint(key, endpoint.class, endpoint.raw.to_string())
                } else {
                    ActivityRevealResultV1::not_revealable()
                }
            }
            EventLookup::NotFound => ActivityRevealResultV1::not_found(),
            EventLookup::SessionMismatch => {
                ActivityRevealResultV1::session_mismatch(self.shared.status())
            }
        })
    }

    fn safe_copy(
        &self,
        request: &EventQueryRequest,
    ) -> Result<ActivitySafeCopyResultV1, ActivityRecorderError> {
        match self.event_lookup(request) {
            EventLookup::Found(event) => {
                let copy = event
                    .safe_copy()
                    .map_err(|_| ActivityRecorderError::RingUnavailable)?;
                let json = serde_json::to_string(&copy)
                    .map_err(|_| ActivityRecorderError::RingUnavailable)?;
                Ok(ActivitySafeCopyResultV1::found(json))
            }
            EventLookup::NotFound => Ok(ActivitySafeCopyResultV1::not_found()),
            EventLookup::SessionMismatch => Ok(ActivitySafeCopyResultV1::session_mismatch(
                self.shared.status(),
            )),
        }
    }

    fn replay(
        &mut self,
        request: ReplayRequest,
    ) -> Result<ActivityReplayResultV1, ActivityRecorderError> {
        let Some(session) = self.session.as_ref() else {
            return Ok(ActivityReplayResultV1::SessionMismatch {
                status: self.shared.status(),
            });
        };
        if request.capture_session != session.privacy.capture_session() {
            return Ok(ActivityReplayResultV1::SessionMismatch {
                status: self.shared.status(),
            });
        }
        let oldest = session.ring.oldest_sequence();
        let latest = session.ring.latest_sequence();
        let gap = request
            .after
            .is_some_and(|after| oldest.is_some_and(|oldest| after < oldest.saturating_sub(1)));
        if gap {
            self.shared.health.increment_replay_gap();
        }
        let effective_after = if gap {
            oldest.and_then(|value| value.checked_sub(1))
        } else {
            request.after
        };
        let mut events =
            session
                .ring
                .snapshot_after(effective_after, request.max_events, request.max_bytes);
        loop {
            let next_after = events
                .last()
                .map(|event| event.sequence())
                .or(effective_after);
            let has_more =
                latest.is_some_and(|latest| next_after.is_some_and(|next| latest > next));
            let page = ActivityReplayV1::new(
                session.privacy.capture_session().to_string(),
                events.clone(),
                oldest,
                latest,
                next_after,
                has_more,
                gap,
                self.shared.health.snapshot(),
            );
            if serde_json::to_vec(&page)
                .map(|encoded| encoded.len() <= request.max_bytes)
                .unwrap_or(false)
            {
                return Ok(ActivityReplayResultV1::Page { page });
            }
            if events.pop().is_none() {
                return Err(ActivityRecorderError::InvalidRequest);
            }
        }
    }

    fn on_tick(&mut self) {
        let now_instant = Instant::now();
        let now = self.observation_time();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.state != ActivityCaptureState::Capturing {
            return;
        }
        let coalescer_due = session.coalescer_since.is_some_and(|since| {
            now_instant.duration_since(since)
                >= Duration::from_millis(ACTIVITY_BATCH_MAX_LATENCY_MS)
        });
        if coalescer_due {
            let _ = session.flush_coalescer(&self.shared, self.sink.as_ref());
        }
        session.flush_health_markers(now, &self.shared, self.sink.as_ref());
        session.flush_batch_if_full(&self.shared, self.sink.as_ref());
        if coalescer_due {
            // A just-flushed coalescer has already held its event for the full
            // latency budget; do not start a second 100 ms batch timer.
            session.flush_batch(&self.shared, self.sink.as_ref());
        }
        if session.batch_started.is_some_and(|since| {
            now_instant.duration_since(since)
                >= Duration::from_millis(ACTIVITY_BATCH_MAX_LATENCY_MS)
        }) {
            session.flush_batch(&self.shared, self.sink.as_ref());
        }
        let state = session.state;
        self.publish_session(state);
    }

    fn observation_time(&self) -> ObservationTime {
        self.shared.observation_clock.observe()
    }

    fn open_ack(
        &self,
        generation: u64,
        profile: CaptureProfile,
        trace_deadline: Option<Instant>,
        reopen: bool,
    ) -> BarrierAck {
        BarrierAck {
            generation,
            profile,
            trace_deadline,
            reopen,
            worker_epoch: self.worker_epoch,
        }
    }

    fn publish_session(&self, state: ActivityCaptureState) {
        if let Some(session) = &self.session {
            self.shared.mirror.update(|status| {
                status.capture_session = Some(session.privacy.capture_session().to_string());
                status.state = state;
                status.profile = Some(session.profile);
                status.ingress_generation = session.generation;
                status.oldest = session.ring.oldest_sequence();
                status.latest = session.ring.latest_sequence();
                status.worker_state = ActivityWorkerState::Running;
                status.worker_epoch = self.worker_epoch;
            });
        }
    }
}

struct SessionCore {
    privacy: CapturePrivacy,
    ring: ActivityRing,
    state: ActivityCaptureState,
    profile: CaptureProfile,
    generation: u64,
    next_sequence: Option<u64>,
    coalescer: PreflushCoalescer,
    coalescer_since: Option<Instant>,
    pending_batch: Vec<super::schema::ActivityEventV1>,
    pending_batch_event_bytes: usize,
    batch_started: Option<Instant>,
    pending_evicted_count: u64,
    pending_evicted_bytes: u64,
    pending_evicted_first_unix_ms: Option<u64>,
    pending_evicted_last_unix_ms: Option<u64>,
}

impl SessionCore {
    fn new(
        privacy: CapturePrivacy,
        ring: ActivityRing,
        generation: u64,
        profile: CaptureProfile,
    ) -> Self {
        Self {
            privacy,
            ring,
            state: ActivityCaptureState::Capturing,
            profile,
            generation,
            next_sequence: Some(1),
            coalescer: PreflushCoalescer::default(),
            coalescer_since: None,
            pending_batch: Vec::with_capacity(ACTIVITY_BATCH_MAX_EVENTS),
            pending_batch_event_bytes: 0,
            batch_started: None,
            pending_evicted_count: 0,
            pending_evicted_bytes: 0,
            pending_evicted_first_unix_ms: None,
            pending_evicted_last_unix_ms: None,
        }
    }

    fn commit_control(
        &mut self,
        draft: ActivityDraft,
        shared: &WorkerShared,
        sink: &dyn ActivityBatchSink,
    ) -> Result<(), ActivityRecorderError> {
        let validated = draft
            .validate(DraftContext {
                capture_session: self.privacy.capture_session().to_string(),
                capture_generation: self.generation,
                capture_profile: self.profile,
            })
            .map_err(|_| ActivityRecorderError::RingUnavailable)?;
        self.commit_ready(ReadyDraft(validated), false, true, shared, sink)
    }

    fn commit_health_marker(
        &mut self,
        draft: ActivityDraft,
        shared: &WorkerShared,
        sink: &dyn ActivityBatchSink,
    ) -> Result<(), ActivityRecorderError> {
        let validated = draft
            .validate(DraftContext {
                capture_session: self.privacy.capture_session().to_string(),
                capture_generation: self.generation,
                capture_profile: self.profile,
            })
            .map_err(|_| ActivityRecorderError::RingUnavailable)?;
        self.commit_ready(ReadyDraft(validated), false, false, shared, sink)
    }

    fn commit_ready(
        &mut self,
        ready: ReadyDraft,
        track_eviction_marker: bool,
        reserve_health_marker: bool,
        shared: &WorkerShared,
        sink: &dyn ActivityBatchSink,
    ) -> Result<(), ActivityRecorderError> {
        let sequence = self
            .next_sequence
            .ok_or(ActivityRecorderError::GenerationExhausted)?;
        let stored = self
            .privacy
            .seal(ready, sequence)
            .map_err(|_| ActivityRecorderError::RingUnavailable)?;
        let masked = stored.masked();
        let observed_unix_ms = masked.timestamp_unix_ms;
        let effect = self.ring.push(stored).map_err(map_ring_error)?;
        self.next_sequence = sequence.checked_add(1);
        self.account_eviction(effect, track_eviction_marker, observed_unix_ms, shared);
        self.append_batch(masked, reserve_health_marker, shared, sink);
        Ok(())
    }

    fn account_eviction(
        &mut self,
        effect: RingPush,
        track_marker: bool,
        observed_unix_ms: u64,
        shared: &WorkerShared,
    ) {
        let count_events = effect.evicted_for_count_events;
        let byte_events = effect.evicted_for_byte_limit_events;
        shared.health.add_count_limit_evicted_events(count_events);
        shared.health.add_byte_limit_evicted_events(byte_events);
        let evicted_count = count_events.saturating_add(byte_events);
        if track_marker && evicted_count > 0 {
            self.pending_evicted_count = self.pending_evicted_count.saturating_add(evicted_count);
            self.pending_evicted_bytes = self
                .pending_evicted_bytes
                .saturating_add(effect.evicted_for_count_bytes)
                .saturating_add(effect.evicted_for_byte_limit_bytes);
            self.pending_evicted_first_unix_ms = Some(
                self.pending_evicted_first_unix_ms
                    .map_or(observed_unix_ms, |first| first.min(observed_unix_ms)),
            );
            self.pending_evicted_last_unix_ms = Some(
                self.pending_evicted_last_unix_ms
                    .map_or(observed_unix_ms, |last| last.max(observed_unix_ms)),
            );
        }
    }

    fn append_batch(
        &mut self,
        event: super::schema::ActivityEventV1,
        reserve_health_marker: bool,
        shared: &WorkerShared,
        sink: &dyn ActivityBatchSink,
    ) {
        let Some(event_bytes) = serde_json::to_vec(&event).ok().map(|encoded| encoded.len()) else {
            shared.health.increment_ipc_failure();
            return;
        };
        let marker_slots = usize::from(reserve_health_marker);
        let exceeds_count = self
            .pending_batch
            .len()
            .saturating_add(1)
            .saturating_add(marker_slots)
            > ACTIVITY_BATCH_MAX_EVENTS;
        let projected_event_bytes = self.pending_batch_event_bytes.saturating_add(event_bytes);
        let projected_count = self.pending_batch.len().saturating_add(1);
        let first_sequence = self
            .pending_batch
            .first()
            .map_or(event.sequence(), super::schema::ActivityEventV1::sequence);
        let reserve_bytes = if reserve_health_marker {
            MAX_ENCODED_EVENT_BYTES.saturating_add(1)
        } else {
            0
        };
        let exceeds_bytes = ActivityBatchV1::encoded_len_from_parts(
            self.privacy.capture_session(),
            first_sequence,
            event.sequence(),
            projected_event_bytes,
            projected_count,
        )
        .is_none_or(|encoded| encoded.saturating_add(reserve_bytes) > ACTIVITY_BATCH_MAX_BYTES);
        if !self.pending_batch.is_empty() && (exceeds_count || exceeds_bytes) {
            self.flush_batch(shared, sink);
        }
        self.batch_started.get_or_insert_with(Instant::now);
        self.pending_batch_event_bytes = self.pending_batch_event_bytes.saturating_add(event_bytes);
        self.pending_batch.push(event);
    }

    fn flush_batch_if_full(&mut self, shared: &WorkerShared, sink: &dyn ActivityBatchSink) {
        let should_flush = self.pending_batch.len() >= ACTIVITY_BATCH_MAX_EVENTS
            || self
                .batch_encoded_len()
                .is_some_and(|encoded| encoded >= ACTIVITY_BATCH_MAX_BYTES);
        if should_flush {
            self.flush_batch(shared, sink);
        }
    }

    fn flush_batch(&mut self, shared: &WorkerShared, sink: &dyn ActivityBatchSink) {
        if self.pending_batch.is_empty() {
            self.batch_started = None;
            return;
        }
        let events = std::mem::take(&mut self.pending_batch);
        self.pending_batch = Vec::with_capacity(ACTIVITY_BATCH_MAX_EVENTS);
        self.pending_batch_event_bytes = 0;
        self.batch_started = None;
        let Some(batch) = ActivityBatchV1::new(self.privacy.capture_session().to_string(), events)
        else {
            return;
        };
        if sink.try_publish(&batch).is_err() {
            shared.health.increment_ipc_failure();
        }
    }

    fn flush_coalescer(
        &mut self,
        shared: &WorkerShared,
        sink: &dyn ActivityBatchSink,
    ) -> Result<(), ActivityRecorderError> {
        self.coalescer_since = None;
        if let Some(ready) = self.coalescer.flush() {
            self.commit_ready(ready, true, true, shared, sink)?;
        }
        Ok(())
    }

    fn flush_health_markers(
        &mut self,
        now: ObservationTime,
        shared: &WorkerShared,
        sink: &dyn ActivityBatchSink,
    ) {
        if let Some(loss) = shared.health.take_loss_window() {
            let span_ms = loss
                .last_observed_unix_ms()
                .abs_diff(loss.first_observed_unix_ms());
            let _ = self.commit_health_marker(
                catalog::diagnostics_dropped(DiagnosticsDropped {
                    time: now,
                    count: loss.count(),
                    span_ms,
                }),
                shared,
                sink,
            );
        }
        if self.pending_evicted_count > 0 {
            let count = std::mem::take(&mut self.pending_evicted_count);
            let bytes = std::mem::take(&mut self.pending_evicted_bytes);
            let span_ms = match (
                self.pending_evicted_first_unix_ms.take(),
                self.pending_evicted_last_unix_ms.take(),
            ) {
                (Some(first), Some(last)) => last.abs_diff(first),
                _ => 0,
            };
            let _ = self.commit_health_marker(
                catalog::diagnostics_evicted(DiagnosticsEvicted {
                    time: now,
                    count,
                    bytes,
                    span_ms,
                }),
                shared,
                sink,
            );
        }
    }

    fn discard_pending(&mut self) {
        self.coalescer.clear();
        self.coalescer_since = None;
        self.pending_batch.clear();
        self.pending_batch_event_bytes = 0;
        self.batch_started = None;
        self.pending_evicted_count = 0;
        self.pending_evicted_bytes = 0;
        self.pending_evicted_first_unix_ms = None;
        self.pending_evicted_last_unix_ms = None;
    }

    fn batch_encoded_len(&self) -> Option<usize> {
        ActivityBatchV1::encoded_len_from_parts(
            self.privacy.capture_session(),
            self.pending_batch.first()?.sequence(),
            self.pending_batch.last()?.sequence(),
            self.pending_batch_event_bytes,
            self.pending_batch.len(),
        )
    }
}

enum EventLookup<'a> {
    Found(&'a StoredEventV1),
    NotFound,
    SessionMismatch,
}

fn map_ring_error(error: RingError) -> ActivityRecorderError {
    match error {
        RingError::EventExceedsByteLimit | RingError::InvalidLimits => {
            ActivityRecorderError::RingUnavailable
        }
    }
}

fn map_gate_error(error: super::gate::GateError) -> ActivityRecorderError {
    match error {
        super::gate::GateError::GenerationExhausted => ActivityRecorderError::GenerationExhausted,
        super::gate::GateError::GenerationMismatch { .. }
        | super::gate::GateError::StateChanged => ActivityRecorderError::Superseded,
        super::gate::GateError::NotClosed
        | super::gate::GateError::ReadersActive(_)
        | super::gate::GateError::QuiescenceWaitInProgress
        | super::gate::GateError::WaiterCountExhausted
        | super::gate::GateError::UnexpectedTraceDeadline => {
            ActivityRecorderError::InvalidTransition
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::super::admission::{
        INGRESS_CAPACITY, LOW_PRIORITY_LIMIT, ProcessClock, RESERVED_PRIORITY_SLOTS,
    };
    use super::super::replay::ActivityPublishError;
    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        batches: Mutex<Vec<ActivityBatchV1>>,
    }

    impl ActivityBatchSink for RecordingSink {
        fn try_publish(&self, batch: &ActivityBatchV1) -> Result<(), ActivityPublishError> {
            self.batches
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(batch.clone());
            Ok(())
        }
    }

    fn test_shared() -> WorkerShared {
        WorkerShared {
            gate: Arc::new(AdmissionGate::new()),
            rate: Arc::new(RateAdmission::new(ProcessClock::new())),
            observation_clock: Arc::new(super::catalog::SystemActivityClock::new()),
            low_permits: LowPermitPool::new(),
            health: ActivityHealth::new(),
            mirror: Arc::new(StatusMirror::new()),
            trace_deadline: Mutex::new(None),
            available: AtomicBool::new(true),
        }
    }

    #[test]
    fn pending_loss_marker_is_in_the_next_deliverable_batch() {
        let shared = test_shared();
        let sink = RecordingSink::default();
        let mut session = SessionCore::new(
            CapturePrivacy::random(),
            ActivityRing::platform_default().unwrap(),
            1,
            CaptureProfile::Normal,
        );
        for offset in 0..49 {
            session
                .commit_control(
                    catalog::diagnostics_capture_started(
                        ObservationTime::new(offset, offset),
                        CaptureProfile::Normal,
                    ),
                    &shared,
                    &sink,
                )
                .unwrap();
        }
        assert_eq!(session.pending_batch.len(), 49);
        shared.health.increment_ingress_full_at(1_000);

        session.flush_health_markers(ObservationTime::new(1_001, 1_001), &shared, &sink);
        session.flush_batch_if_full(&shared, &sink);

        let batches = sink
            .batches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].events().len(), ACTIVITY_BATCH_MAX_EVENTS);
        assert!(
            batches[0]
                .events()
                .iter()
                .any(|event| event.kind() == "diagnostics.dropped")
        );
        assert!(serde_json::to_vec(&batches[0]).unwrap().len() <= ACTIVITY_BATCH_MAX_BYTES);
    }

    #[test]
    fn eviction_marker_serializes_aggregate_count_bytes_and_time_span() {
        let shared = test_shared();
        let sink = RecordingSink::default();
        let mut session = SessionCore::new(
            CapturePrivacy::random(),
            ActivityRing::platform_default().unwrap(),
            1,
            CaptureProfile::Normal,
        );
        session.account_eviction(
            RingPush {
                evicted_for_count_events: 1,
                evicted_for_count_bytes: 11,
                ..RingPush::default()
            },
            true,
            1_042,
            &shared,
        );
        session.account_eviction(
            RingPush {
                evicted_for_byte_limit_events: 2,
                evicted_for_byte_limit_bytes: 31,
                ..RingPush::default()
            },
            true,
            1_000,
            &shared,
        );

        session.flush_health_markers(ObservationTime::new(1_100, 1_100), &shared, &sink);
        session.flush_batch(&shared, &sink);

        let batches = sink
            .batches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let evicted = batches
            .iter()
            .flat_map(ActivityBatchV1::events)
            .find(|event| event.kind() == "diagnostics.evicted")
            .expect("evictions should produce a marker");
        let evicted_json = serde_json::to_value(evicted).unwrap();
        let unsigned_attribute = |key: &str| {
            evicted_json["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|attribute| attribute["key"] == key)
                .and_then(|attribute| attribute["value"]["value"].as_u64())
                .unwrap()
        };
        assert_eq!(unsigned_attribute("evicted_count"), 3);
        assert_eq!(unsigned_attribute("byte_length"), 42);
        assert_eq!(unsigned_attribute("time_span_ms"), 42);
        assert_eq!(shared.health.snapshot().count_limit_evicted_events(), "1");
        assert_eq!(shared.health.snapshot().byte_limit_evicted_events(), "2");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replay_is_serviced_after_a_bounded_ingress_burst() {
        const QUEUED_EVENTS: usize = 64;

        let shared = Arc::new(test_shared());
        let sink: Arc<dyn ActivityBatchSink> = Arc::new(RecordingSink::default());
        let (ingress_tx, ingress_rx) = crossbeam_channel::bounded(INGRESS_CAPACITY);
        let (urgent_tx, urgent_rx) = crossbeam_channel::bounded(1);
        let (query_tx, query_rx) = crossbeam_channel::bounded(1);
        let privacy = CapturePrivacy::random();
        let capture_session = privacy.capture_session().to_string();
        let mut worker = WorkerCore::new(
            Arc::clone(&shared),
            sink,
            ingress_rx,
            urgent_rx,
            query_rx,
            1,
            false,
        );
        worker.session = Some(SessionCore::new(
            privacy,
            ActivityRing::platform_default().unwrap(),
            1,
            CaptureProfile::Normal,
        ));
        for offset in 0..QUEUED_EVENTS {
            assert!(
                ingress_tx
                    .try_send(IngressItem::Draft(IngressDraft {
                        generation: 1,
                        profile: CaptureProfile::Normal,
                        draft: catalog::test_large_error_event(offset as u64, offset as u64)
                            .unwrap(),
                        low_permit: None,
                    }))
                    .is_ok(),
                "the deterministic ingress backlog should fit"
            );
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        assert!(
            query_tx
                .try_send(QueryCommand::Replay {
                    request: ReplayRequest {
                        capture_session,
                        after: None,
                        max_events: ACTIVITY_BATCH_MAX_EVENTS,
                        max_bytes: ACTIVITY_BATCH_MAX_BYTES,
                    },
                    reply: reply_tx,
                })
                .is_ok(),
            "the replay query should fit"
        );

        let worker_thread = std::thread::spawn(move || worker.run());
        let replay = tokio::time::timeout(Duration::from_secs(2), reply_rx)
            .await
            .expect("replay must not starve behind a continuously ready ingress queue")
            .expect("worker should return the replay result")
            .expect("the matching session should replay");
        let ActivityReplayResultV1::Page { page } = replay else {
            panic!("the matching session should produce a page");
        };
        assert_eq!(page.events().len(), MAX_INGRESS_BURST_BEFORE_SERVICE);
        assert_eq!(
            page.events()
                .iter()
                .map(super::super::schema::ActivityEventV1::sequence)
                .collect::<Vec<_>>(),
            (1..=MAX_INGRESS_BURST_BEFORE_SERVICE as u64).collect::<Vec<_>>()
        );

        drop(urgent_tx);
        drop(ingress_tx);
        drop(query_tx);
        tokio::task::spawn_blocking(move || worker_thread.join().unwrap())
            .await
            .unwrap();
    }

    #[test]
    fn warning_and_error_envelopes_can_use_the_reserved_fifo_tail() {
        let permits = LowPermitPool::new();
        let (tx, rx) = crossbeam_channel::bounded(INGRESS_CAPACITY);
        for offset in 0..LOW_PRIORITY_LIMIT as u64 {
            assert!(
                tx.try_send(IngressItem::Draft(IngressDraft {
                    generation: 1,
                    profile: CaptureProfile::Normal,
                    draft: catalog::diagnostics_capture_started(
                        ObservationTime::new(offset, offset),
                        CaptureProfile::Normal,
                    ),
                    low_permit: Some(permits.try_acquire().expect("low-priority slot should fit")),
                }))
                .is_ok(),
                "the low-priority partition should fit"
            );
        }
        assert!(permits.try_acquire().is_none());

        for offset in 0..RESERVED_PRIORITY_SLOTS / 2 {
            assert!(
                tx.try_send(IngressItem::Draft(IngressDraft {
                    generation: 1,
                    profile: CaptureProfile::Normal,
                    draft: catalog::diagnostics_dropped(DiagnosticsDropped {
                        time: ObservationTime::new(offset as u64, offset as u64),
                        count: 1,
                        span_ms: 0,
                    }),
                    low_permit: None,
                }))
                .is_ok(),
                "warning should enter the reserved tail"
            );
        }
        for offset in 0..RESERVED_PRIORITY_SLOTS / 2 {
            assert!(
                tx.try_send(IngressItem::Draft(IngressDraft {
                    generation: 1,
                    profile: CaptureProfile::Normal,
                    draft: catalog::test_large_error_event(offset as u64, offset as u64).unwrap(),
                    low_permit: None,
                }))
                .is_ok(),
                "error should enter the reserved tail"
            );
        }
        assert_eq!(rx.len(), INGRESS_CAPACITY);
        assert!(matches!(
            tx.try_send(IngressItem::Draft(IngressDraft {
                generation: 1,
                profile: CaptureProfile::Normal,
                draft: catalog::diagnostics_dropped(DiagnosticsDropped {
                    time: ObservationTime::new(99, 99),
                    count: 1,
                    span_ms: 0,
                }),
                low_permit: None,
            })),
            Err(crossbeam_channel::TrySendError::Full(_))
        ));
        drop(rx);
    }
}
