//! Privacy-safe Activity projection for exact runtime-owned RNode observers.

use std::sync::Arc;

use rns_interface::rnode::{
    RNodeCapabilityState, RNodeRuntimePhase, RNodeRuntimeReason, RNodeRuntimeSnapshot,
};
use rns_runtime::reticulum::{RNodeRuntimeObserver, ReticulumHandle};

use crate::activity::producer;
use crate::state::{AppState, RNodeActivityOrigin};

/// One installed RNS handle paired with its unforgeable Activity origin.
/// Commands must spawn through this handle and retain `origin()` until the
/// exact interface registration is covered.
pub struct RNodeActivityRuntimeContext {
    handle: ReticulumHandle,
    origin: RNodeActivityOrigin,
}

impl RNodeActivityRuntimeContext {
    pub(crate) fn new(handle: ReticulumHandle, origin: RNodeActivityOrigin) -> Self {
        Self { handle, origin }
    }

    pub fn handle(&self) -> &ReticulumHandle {
        &self.handle
    }

    pub fn origin(&self) -> RNodeActivityOrigin {
        self.origin
    }
}

/// Single-use activation seed for one exact Ready RNode observation.
///
/// It is deliberately not Clone: one successful product operation may start
/// at most one Activity monitor, including when ownership crosses a native
/// completion channel.
pub struct PendingRNodeActivityMonitor {
    observer: RNodeRuntimeObserver,
    ready_snapshot: Arc<RNodeRuntimeSnapshot>,
    origin: RNodeActivityOrigin,
}

impl std::fmt::Debug for PendingRNodeActivityMonitor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingRNodeActivityMonitor")
            .finish_non_exhaustive()
    }
}

impl PendingRNodeActivityMonitor {
    pub fn new(
        observer: RNodeRuntimeObserver,
        ready_snapshot: Arc<RNodeRuntimeSnapshot>,
        origin: RNodeActivityOrigin,
    ) -> Self {
        Self {
            observer,
            ready_snapshot,
            origin,
        }
    }

    pub fn interface_id(&self) -> rns_interface::traits::InterfaceId {
        self.observer.interface_id()
    }

    pub fn activate(self, state: Arc<AppState>) -> bool {
        spawn_ready_rnode_activity_monitor_for_origin(
            state,
            self.observer,
            self.ready_snapshot,
            self.origin,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RNodeActivitySignal {
    Offline,
    Online,
    CapabilityUnverified,
    CapabilityRejected,
    RuntimeFailed,
}

#[derive(Clone, Copy)]
struct RNodeActivityObservation {
    phase: RNodeRuntimePhase,
    connection_generation: u64,
    capability: RNodeCapabilityState,
    reason: Option<RNodeRuntimeReason>,
}

impl From<&RNodeRuntimeSnapshot> for RNodeActivityObservation {
    fn from(snapshot: &RNodeRuntimeSnapshot) -> Self {
        Self {
            phase: snapshot.phase,
            connection_generation: snapshot.connection_generation,
            capability: snapshot.capability,
            reason: snapshot.reason,
        }
    }
}

struct RNodeActivityReducer {
    previous_phase: RNodeRuntimePhase,
    previous_generation: u64,
    previous_capability: RNodeCapabilityState,
    loss_open: bool,
    expected_shutdown: bool,
    terminal_emitted: bool,
}

impl RNodeActivityReducer {
    fn from_ready(snapshot: &RNodeRuntimeSnapshot) -> Option<Self> {
        Self::from_ready_observation(snapshot.into())
    }

    fn from_ready_observation(snapshot: RNodeActivityObservation) -> Option<Self> {
        (snapshot.phase == RNodeRuntimePhase::Ready).then_some(Self {
            previous_phase: snapshot.phase,
            previous_generation: snapshot.connection_generation,
            previous_capability: snapshot.capability,
            loss_open: false,
            expected_shutdown: false,
            terminal_emitted: false,
        })
    }

    fn baseline_signals(&self, include_ready: bool) -> Vec<RNodeActivitySignal> {
        let mut signals = Vec::with_capacity(2);
        if include_ready {
            signals.push(RNodeActivitySignal::Online);
        }
        if self.previous_capability == RNodeCapabilityState::Unverified {
            signals.push(RNodeActivitySignal::CapabilityUnverified);
        }
        signals
    }

    fn observe(&mut self, snapshot: RNodeActivityObservation) -> Vec<RNodeActivitySignal> {
        if self.terminal_emitted {
            self.remember(snapshot);
            return Vec::new();
        }

        if is_expected_shutdown(snapshot.reason) {
            self.expected_shutdown = true;
            self.remember(snapshot);
            return Vec::new();
        }

        let mut signals = Vec::with_capacity(3);
        if snapshot.phase == RNodeRuntimePhase::Stopped {
            if !self.expected_shutdown {
                if self.previous_phase == RNodeRuntimePhase::Ready && !self.loss_open {
                    signals.push(RNodeActivitySignal::Offline);
                    self.loss_open = true;
                }
                signals.push(
                    if snapshot.reason == Some(RNodeRuntimeReason::CapabilityAdmissionRejected) {
                        RNodeActivitySignal::CapabilityRejected
                    } else {
                        RNodeActivitySignal::RuntimeFailed
                    },
                );
                self.terminal_emitted = true;
            }
            self.remember(snapshot);
            return signals;
        }

        let capability_became_unverified = snapshot.capability == RNodeCapabilityState::Unverified
            && self.previous_capability != RNodeCapabilityState::Unverified;

        if snapshot.phase == RNodeRuntimePhase::Ready {
            if self.loss_open {
                signals.push(RNodeActivitySignal::Online);
                self.loss_open = false;
            } else if self.previous_phase == RNodeRuntimePhase::Ready
                && snapshot.connection_generation != self.previous_generation
            {
                // A watch channel may coalesce every intermediate reconnect
                // snapshot. Preserve the one loss/recovery pair implied by a
                // changed ready generation without inventing attempt chatter.
                signals.push(RNodeActivitySignal::Offline);
                signals.push(RNodeActivitySignal::Online);
            } else if self.previous_phase != RNodeRuntimePhase::Ready {
                signals.push(RNodeActivitySignal::Online);
            }
        } else if self.previous_phase == RNodeRuntimePhase::Ready && !self.loss_open {
            signals.push(RNodeActivitySignal::Offline);
            self.loss_open = true;
        }
        if capability_became_unverified {
            signals.push(RNodeActivitySignal::CapabilityUnverified);
        }

        self.remember(snapshot);
        signals
    }

    fn publisher_closed(&mut self) -> Vec<RNodeActivitySignal> {
        if self.expected_shutdown || self.terminal_emitted {
            return Vec::new();
        }
        let mut signals = Vec::with_capacity(2);
        if self.previous_phase == RNodeRuntimePhase::Ready && !self.loss_open {
            signals.push(RNodeActivitySignal::Offline);
            self.loss_open = true;
        }
        signals.push(RNodeActivitySignal::RuntimeFailed);
        self.terminal_emitted = true;
        signals
    }

    fn remember(&mut self, snapshot: RNodeActivityObservation) {
        self.previous_phase = snapshot.phase;
        self.previous_generation = snapshot.connection_generation;
        self.previous_capability = snapshot.capability;
    }
}

fn is_expected_shutdown(reason: Option<RNodeRuntimeReason>) -> bool {
    matches!(reason, Some(RNodeRuntimeReason::StopRequested))
}

/// Start observation of one exact RNode after its first protocol-ready
/// snapshot. This grants no lifecycle or reconnect authority.
///
/// Returns `false` if the baseline is not Ready, the identity or installed RNS
/// session changed, or the exact interface was not covered before readiness
/// waiting began.
fn spawn_ready_rnode_activity_monitor_for_origin(
    state: Arc<AppState>,
    observer: RNodeRuntimeObserver,
    ready_snapshot: Arc<RNodeRuntimeSnapshot>,
    origin: RNodeActivityOrigin,
) -> bool {
    let Some(reducer) = RNodeActivityReducer::from_ready(&ready_snapshot) else {
        return false;
    };
    let interface_id = observer.interface_id();
    if !state.owns_rnode_activity_observation(interface_id, origin) {
        return false;
    }
    let shutdown = state
        .session_shutdown
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();

    tokio::spawn(async move {
        run_ready_rnode_activity_monitor(
            state,
            observer,
            reducer,
            origin,
            interface_id,
            shutdown,
            false,
        )
        .await;
    });
    true
}

pub fn spawn_startup_rnode_activity_monitor(
    state: Arc<AppState>,
    observer: RNodeRuntimeObserver,
    origin: RNodeActivityOrigin,
) {
    let identity_generation = origin.identity_generation();
    let interface_id = observer.interface_id();
    let shutdown = state
        .session_shutdown
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    tokio::spawn(async move {
        if !await_identity_lifecycle_release(&state, &shutdown, identity_generation).await {
            return;
        }
        let current = observer.snapshot();
        if current.phase == RNodeRuntimePhase::Ready {
            let Some(reducer) = RNodeActivityReducer::from_ready(&current) else {
                return;
            };
            if !state.owns_rnode_activity_observation(interface_id, origin) {
                return;
            }
            run_ready_rnode_activity_monitor(
                state,
                observer,
                reducer,
                origin,
                interface_id,
                shutdown,
                true,
            )
            .await;
            return;
        }
        let readiness = tokio::select! {
            biased;
            _ = shutdown.wait() => return,
            readiness = observer.await_ready(std::time::Duration::MAX) => readiness,
        };
        if !await_identity_lifecycle_release(&state, &shutdown, identity_generation).await {
            return;
        }
        match readiness {
            Ok(ready) => {
                let Some(reducer) = RNodeActivityReducer::from_ready(&ready) else {
                    return;
                };
                if !state.owns_rnode_activity_observation(interface_id, origin) {
                    return;
                }
                run_ready_rnode_activity_monitor(
                    state,
                    observer,
                    reducer,
                    origin,
                    interface_id,
                    shutdown,
                    true,
                )
                .await;
            }
            Err(error) => {
                let last = match error {
                    rns_runtime::reticulum::RNodeReadinessError::Timeout { last }
                    | rns_runtime::reticulum::RNodeReadinessError::ShuttingDown { last }
                    | rns_runtime::reticulum::RNodeReadinessError::Stopped { last }
                    | rns_runtime::reticulum::RNodeReadinessError::ObservationClosed { last } => {
                        last
                    }
                    _ => return,
                };
                if !is_expected_shutdown(last.reason) {
                    let signal =
                        if last.reason == Some(RNodeRuntimeReason::CapabilityAdmissionRejected) {
                            RNodeActivitySignal::CapabilityRejected
                        } else {
                            RNodeActivitySignal::RuntimeFailed
                        };
                    let _ = emit_signals_if_current(&state, origin, interface_id, vec![signal]);
                }
            }
        }
    });
}

async fn run_ready_rnode_activity_monitor(
    state: Arc<AppState>,
    mut observer: RNodeRuntimeObserver,
    mut reducer: RNodeActivityReducer,
    origin: RNodeActivityOrigin,
    interface_id: rns_interface::traits::InterfaceId,
    shutdown: rns_runtime::lifecycle::ShutdownSignal,
    include_ready_baseline: bool,
) {
    if !await_identity_lifecycle_release(&state, &shutdown, origin.identity_generation()).await {
        return;
    }
    if !state.set_rnode_product_readiness(interface_id, origin, true) {
        return;
    }
    if !emit_signals_if_current(
        &state,
        origin,
        interface_id,
        reducer.baseline_signals(include_ready_baseline),
    ) {
        return;
    }

    loop {
        let changed = tokio::select! {
            biased;
            _ = shutdown.wait() => return,
            changed = observer.changed() => changed,
        };
        let publisher_closed = changed.is_none();
        let product_ready = changed
            .as_ref()
            .is_some_and(|snapshot| snapshot.phase == RNodeRuntimePhase::Ready);
        if !state.set_rnode_product_readiness(interface_id, origin, product_ready) {
            return;
        }
        let signals = match changed.as_ref() {
            Some(snapshot) => reducer.observe(snapshot.as_ref().into()),
            None => reducer.publisher_closed(),
        };
        if product_ready
            && signals.contains(&RNodeActivitySignal::Online)
            && state.owns_rnode_activity_observation(interface_id, origin)
        {
            // Keep reconnect semantics in the same revision stream as first
            // Ready publication. A coalesced Ready-generation change still
            // produces exactly one Online signal and therefore one bump.
            state.bump_announce_interface_revision();
        }
        if !emit_signals_if_current(&state, origin, interface_id, signals) || publisher_closed {
            return;
        }
    }
}

async fn await_identity_lifecycle_release(
    state: &AppState,
    shutdown: &rns_runtime::lifecycle::ShutdownSignal,
    identity_generation: u64,
) -> bool {
    loop {
        let observed_epoch = state.identity_switch_lock.epoch();
        if observed_epoch.is_multiple_of(2) {
            if state.current_identity_session_generation() != identity_generation {
                return false;
            }
            if state.identity_switch_lock.epoch() == observed_epoch {
                return true;
            }
            continue;
        }
        let guard = tokio::select! {
            biased;
            _ = shutdown.wait() => return false,
            guard = state.identity_switch_lock.lock() => guard,
        };
        let current = state.current_identity_session_generation() == identity_generation;
        drop(guard);
        return current && state.current_identity_session_generation() == identity_generation;
    }
}

fn emit_signals_if_current(
    state: &AppState,
    origin: RNodeActivityOrigin,
    interface_id: rns_interface::traits::InterfaceId,
    signals: Vec<RNodeActivitySignal>,
) -> bool {
    if !state.owns_rnode_activity_observation(interface_id, origin) {
        return false;
    }
    for signal in signals {
        if !state.owns_rnode_activity_observation(interface_id, origin) {
            return false;
        }
        let fence = state.activity_request_fence();
        if fence.identity_session_generation() != origin.identity_generation() {
            return false;
        }
        let _ = state.activity.record_event_fenced(
            || {
                state.is_current_activity_origin_fence(fence)
                    && state.owns_rnode_activity_observation(interface_id, origin)
            },
            || Ok(rnode_activity_event(signal)),
        );
    }
    true
}

fn rnode_activity_event(signal: RNodeActivitySignal) -> producer::ProducerEvent {
    let transition = match signal {
        RNodeActivitySignal::Offline => producer::InterfaceTransition::Offline,
        RNodeActivitySignal::Online => producer::InterfaceTransition::Online,
        RNodeActivitySignal::CapabilityUnverified => producer::InterfaceTransition::Degraded {
            reason: producer::InterfaceDegradationReason::CapabilityUnverified,
        },
        RNodeActivitySignal::CapabilityRejected => producer::InterfaceTransition::Failed {
            reason: producer::InterfaceFailureReason::CapabilityRejected,
            rollback: None,
        },
        RNodeActivitySignal::RuntimeFailed => producer::InterfaceTransition::Failed {
            reason: producer::InterfaceFailureReason::Runtime,
            rollback: None,
        },
    };
    producer::interface_activity(producer::InterfaceActivity {
        class: producer::InterfaceClass::RNode,
        transition,
        endpoint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn observation(
        phase: RNodeRuntimePhase,
        generation: u64,
        capability: RNodeCapabilityState,
        reason: Option<RNodeRuntimeReason>,
    ) -> RNodeActivityObservation {
        RNodeActivityObservation {
            phase,
            connection_generation: generation,
            capability,
            reason,
        }
    }

    fn reducer() -> RNodeActivityReducer {
        RNodeActivityReducer::from_ready_observation(observation(
            RNodeRuntimePhase::Ready,
            1,
            RNodeCapabilityState::Verified,
            None,
        ))
        .unwrap()
    }

    fn test_state() -> AppState {
        let manager = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        crate::db::init_schema(&pool).unwrap();
        AppState::new(
            crate::config::DashboardConfig::from_env_and_defaults(
                std::env::temp_dir().join("ratspeak-rnode-activity-fence-test"),
            ),
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        )
    }

    #[test]
    fn loss_episode_ignores_retry_churn_and_recovers_once() {
        let mut reducer = reducer();
        assert_eq!(
            reducer.observe(observation(
                RNodeRuntimePhase::ReconnectBackoff,
                0,
                RNodeCapabilityState::NotRequested,
                Some(RNodeRuntimeReason::ConnectionLost),
            )),
            vec![RNodeActivitySignal::Offline]
        );
        assert!(
            reducer
                .observe(observation(
                    RNodeRuntimePhase::Connecting,
                    0,
                    RNodeCapabilityState::NotRequested,
                    Some(RNodeRuntimeReason::ConnectionAttemptFailed),
                ))
                .is_empty()
        );
        assert_eq!(
            reducer.observe(observation(
                RNodeRuntimePhase::Ready,
                2,
                RNodeCapabilityState::Verified,
                None,
            )),
            vec![RNodeActivitySignal::Online]
        );
    }

    #[test]
    fn coalesced_ready_generation_synthesizes_one_loss_and_recovery() {
        let mut reducer = reducer();
        assert_eq!(
            reducer.observe(observation(
                RNodeRuntimePhase::Ready,
                2,
                RNodeCapabilityState::Verified,
                None,
            )),
            vec![RNodeActivitySignal::Offline, RNodeActivitySignal::Online]
        );
        assert!(
            reducer
                .observe(observation(
                    RNodeRuntimePhase::Ready,
                    2,
                    RNodeCapabilityState::Verified,
                    None,
                ))
                .is_empty()
        );
    }

    #[test]
    fn unexpected_terminal_is_reported_once_but_requested_stop_is_silent() {
        let mut failed = reducer();
        assert_eq!(
            failed.observe(observation(
                RNodeRuntimePhase::Stopped,
                0,
                RNodeCapabilityState::NotRequested,
                Some(RNodeRuntimeReason::DriverTerminated),
            )),
            vec![
                RNodeActivitySignal::Offline,
                RNodeActivitySignal::RuntimeFailed
            ]
        );
        assert!(failed.publisher_closed().is_empty());

        let mut stopped = reducer();
        assert!(
            stopped
                .observe(observation(
                    RNodeRuntimePhase::ShuttingDown,
                    1,
                    RNodeCapabilityState::Verified,
                    Some(RNodeRuntimeReason::StopRequested),
                ))
                .is_empty()
        );
        assert!(
            stopped
                .observe(observation(
                    RNodeRuntimePhase::Stopped,
                    0,
                    RNodeCapabilityState::NotRequested,
                    Some(RNodeRuntimeReason::StopRequested),
                ))
                .is_empty()
        );
        assert!(stopped.publisher_closed().is_empty());
    }

    #[test]
    fn transport_consumer_close_is_not_mistaken_for_requested_shutdown() {
        let mut reducer = reducer();
        assert_eq!(
            reducer.observe(observation(
                RNodeRuntimePhase::Stopped,
                0,
                RNodeCapabilityState::NotRequested,
                Some(RNodeRuntimeReason::TransportConsumerClosed),
            )),
            vec![
                RNodeActivitySignal::Offline,
                RNodeActivitySignal::RuntimeFailed
            ]
        );
    }

    #[test]
    fn capability_projection_is_closed_and_has_no_verified_chatter() {
        let verified = reducer();
        assert!(verified.baseline_signals(false).is_empty());
        assert_eq!(
            verified.baseline_signals(true),
            vec![RNodeActivitySignal::Online]
        );

        let unverified_ready = observation(
            RNodeRuntimePhase::Ready,
            1,
            RNodeCapabilityState::Unverified,
            None,
        );
        let mut unverified_reducer =
            RNodeActivityReducer::from_ready_observation(unverified_ready).unwrap();
        assert_eq!(
            unverified_reducer.baseline_signals(false),
            vec![RNodeActivitySignal::CapabilityUnverified]
        );
        assert_eq!(
            unverified_reducer.baseline_signals(true),
            vec![
                RNodeActivitySignal::Online,
                RNodeActivitySignal::CapabilityUnverified
            ]
        );
        assert!(unverified_reducer.observe(unverified_ready).is_empty());
        assert!(
            unverified_reducer
                .observe(observation(
                    RNodeRuntimePhase::Ready,
                    1,
                    RNodeCapabilityState::Verified,
                    None,
                ))
                .is_empty()
        );
        assert_eq!(
            unverified_reducer.observe(unverified_ready),
            vec![RNodeActivitySignal::CapabilityUnverified]
        );

        let mut rejected = reducer();
        assert_eq!(
            rejected.observe(observation(
                RNodeRuntimePhase::Stopped,
                0,
                RNodeCapabilityState::NotRequested,
                Some(RNodeRuntimeReason::CapabilityAdmissionRejected),
            )),
            vec![
                RNodeActivitySignal::Offline,
                RNodeActivitySignal::CapabilityRejected
            ]
        );
    }

    #[tokio::test]
    async fn stale_identity_generation_stops_emission_before_activity_admission() {
        let state = test_state();
        let identity_generation = state.current_identity_session_generation();
        let origin = RNodeActivityOrigin::new(identity_generation, 1);
        state.bump_identity_session_generation();
        assert!(!emit_signals_if_current(
            &state,
            origin,
            73,
            vec![RNodeActivitySignal::Offline]
        ));
    }

    #[tokio::test]
    async fn startup_barrier_waits_for_identity_transition_and_revalidates_origin() {
        let state = Arc::new(test_state());
        let shutdown = rns_runtime::lifecycle::ShutdownSignal::new();
        let origin = state.current_identity_session_generation();
        let guard = state.identity_switch_lock.lock().await;
        let task_state = Arc::clone(&state);
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            await_identity_lifecycle_release(&task_state, &task_shutdown, origin).await
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        state.bump_identity_session_generation();
        drop(guard);
        assert!(!task.await.unwrap());
    }

    #[tokio::test]
    async fn stable_startup_barrier_does_not_create_an_identity_epoch() {
        let state = test_state();
        let shutdown = rns_runtime::lifecycle::ShutdownSignal::new();
        let origin = state.current_identity_session_generation();
        let before = state.identity_switch_lock.epoch();
        assert_eq!(before % 2, 0);
        assert!(await_identity_lifecycle_release(&state, &shutdown, origin).await);
        assert_eq!(state.identity_switch_lock.epoch(), before);
    }

    #[tokio::test]
    async fn soft_rns_restart_rejects_an_old_origin_even_when_identity_is_unchanged() {
        async fn manager(root: &std::path::Path) -> crate::rns::RnsManager {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(
                root.join("config"),
                "[reticulum]\nshare_instance = No\nenable_transport = No\n\n[interfaces]\n",
            )
            .unwrap();
            crate::rns::RnsManager::init(
                root.to_str().unwrap(),
                Some(root.join("cache")),
                Arc::new(std::sync::atomic::AtomicBool::new(true)),
            )
            .await
            .unwrap()
        }

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ratspeak-rnode-activity-origin-{}-{nonce}",
            std::process::id()
        ));
        let state = test_state();

        let first = manager(&root.join("first")).await;
        let first_origin = state.set_rns(first).unwrap();
        let first_context = state
            .rnode_activity_runtime_context_for_identity(
                state.current_identity_session_generation(),
            )
            .unwrap();
        assert!(first_context.origin() == first_origin);
        assert!(state.cover_rnode_activity_interface(71, first_origin));
        assert!(state.set_rnode_product_readiness(71, first_origin, true));
        assert!(!state.effective_interface_online(71, false));
        assert!(state.effective_interface_online(71, true));
        state.set_last_stats(serde_json::json!({"session": "first"}));

        let old = state.rns.write().unwrap().take().unwrap();
        old.shutdown().await;
        let second = manager(&root.join("second")).await;
        let second_origin = state.set_rns(second).unwrap();
        assert!(first_origin != second_origin);
        assert!(state.last_stats.read().unwrap().is_none());
        assert!(!state.effective_interface_online(71, false));
        assert!(!state.set_rnode_product_readiness(71, first_origin, true));
        assert!(!state.cover_rnode_activity_interface(72, first_origin));
        assert!(state.cover_rnode_activity_interface(72, second_origin));

        let current = state.rns.write().unwrap().take().unwrap();
        current.shutdown().await;
        std::fs::remove_dir_all(root).ok();
    }
}
