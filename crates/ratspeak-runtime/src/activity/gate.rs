//! Lock-free producer admission and synchronous lifecycle barriers.
//!
//! Producer coordination touches only atomics, while admitted producers copy
//! immutable published metadata. Lifecycle control is serialized separately so
//! a stale open cannot race a hard-reset generation change.

#![allow(
    dead_code,
    reason = "some generation-gate inspection remains test-only"
)]

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::schema::CaptureProfile;

const CLOSED_BIT: u64 = 1 << 63;
const READER_MASK: u64 = CLOSED_BIT - 1;
const QUIESCENCE_RECHECK: Duration = Duration::from_millis(10);

#[derive(Clone, Copy)]
struct PublishedAdmission {
    profile: CaptureProfile,
    trace_deadline: Option<Instant>,
}

#[derive(Default)]
struct BarrierState {
    quiescence_waiters: usize,
}

/// Fail-closed lifecycle errors from [`AdmissionGate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GateError {
    NotClosed,
    ReadersActive(u64),
    QuiescenceWaitInProgress,
    GenerationMismatch { expected: u64, actual: u64 },
    GenerationExhausted,
    WaiterCountExhausted,
    UnexpectedTraceDeadline,
    StateChanged,
}

/// A CAS-managed gate whose high bit closes admission and whose remaining bits
/// count producer leases.
///
/// The profile/deadline slot is written only while the gate is closed and has
/// no readers. A Release open publishes it, and successful producer admission
/// performs the matching Acquire before copying it. Hard reset never mutates
/// this slot while an old lease exists.
pub(super) struct AdmissionGate {
    state: AtomicU64,
    generation: AtomicU64,
    published: UnsafeCell<PublishedAdmission>,
    barrier: Mutex<BarrierState>,
    quiescent: Condvar,
}

// SAFETY: `published` is mutated only by `open_if_generation` while `barrier`
// is held and `state == CLOSED_BIT` (closed with zero readers). It is read only
// after a successful Acquire admission has incremented the reader count. The
// gate cannot reopen or write the slot until every such reader releases.
unsafe impl Sync for AdmissionGate {}

impl Default for AdmissionGate {
    fn default() -> Self {
        Self::new()
    }
}

impl AdmissionGate {
    /// Creates a closed gate at generation zero.
    pub(super) fn new() -> Self {
        Self {
            state: AtomicU64::new(CLOSED_BIT),
            generation: AtomicU64::new(0),
            published: UnsafeCell::new(PublishedAdmission {
                profile: CaptureProfile::Normal,
                trace_deadline: None,
            }),
            barrier: Mutex::new(BarrierState::default()),
            quiescent: Condvar::new(),
        }
    }

    /// Attempts to enter without waiting or taking a lock.
    ///
    /// A returned lease keeps the reader count elevated until drop. A
    /// close/reopen ABA releases its tentative count and retries. Closed and
    /// exhausted states return `None`; exhaustion also closes the gate so the
    /// count or generation can never wrap.
    pub(super) fn try_admit(&self) -> Option<AdmissionLease<'_>> {
        self.try_admit_inner(|| {})
    }

    // Keeping the boundary callback inside the state machine makes the
    // generation ABA race deterministic in tests. The production no-op is
    // monomorphized away.
    fn try_admit_inner(
        &self,
        mut after_generation_read: impl FnMut(),
    ) -> Option<AdmissionLease<'_>> {
        'admission: loop {
            let mut state = self.state.load(Ordering::Acquire);
            if state & CLOSED_BIT != 0 {
                return None;
            }

            let admission_generation = self.generation.load(Ordering::Acquire);
            if admission_generation == u64::MAX {
                self.fail_closed();
                return None;
            }
            after_generation_read();

            loop {
                if state & CLOSED_BIT != 0 {
                    return None;
                }

                let readers = state & READER_MASK;
                if readers == READER_MASK {
                    match self.state.compare_exchange_weak(
                        state,
                        state | CLOSED_BIT,
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return None,
                        Err(observed) => {
                            state = observed;
                            continue;
                        }
                    }
                }

                match self.state.compare_exchange_weak(
                    state,
                    state + 1,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(observed) => state = observed,
                }
            }

            let confirmed_generation = self.generation.load(Ordering::Acquire);
            if confirmed_generation != admission_generation {
                self.release_reader();
                if confirmed_generation == u64::MAX {
                    self.fail_closed();
                    return None;
                }
                continue 'admission;
            }

            // SAFETY: the successful Acquire above observes the Release open
            // which published this slot. The confirmed generation rejects a
            // close/reopen ABA, and our reader count prevents another write
            // until the returned lease releases.
            let published = unsafe { *self.published.get() };

            return Some(AdmissionLease {
                gate: self,
                generation: admission_generation,
                profile: published.profile,
                trace_deadline: published.trace_deadline,
            });
        }
    }

    /// Closes admission. Returns `true` only for the open-to-closed transition.
    /// Existing leases remain counted and may complete normally.
    pub(super) fn close(&self) -> bool {
        self.close_atomic()
    }

    /// Blocks an explicit lifecycle barrier until every admitted lease drops.
    /// The gate must already be closed.
    pub(super) fn wait_quiescent(&self) -> Result<(), GateError> {
        let barrier = self.lock_barrier();
        self.wait_quiescent_locked(barrier)
    }

    /// Advances the generation without reopening. The gate must be closed.
    ///
    /// Existing old-generation leases may still be active; callers use
    /// [`Self::wait_quiescent`] when their barrier requires them to drain.
    pub(super) fn advance_generation(&self) -> Result<u64, GateError> {
        let _barrier = self.lock_barrier();
        if !self.is_closed() {
            return Err(GateError::NotClosed);
        }
        self.advance_generation_locked()
    }

    /// Closes admission and advances the generation without waiting for old
    /// leases. This is the preemptive privacy boundary used by hard reset.
    pub(super) fn hard_reset(&self) -> Result<u64, GateError> {
        let _barrier = self.lock_barrier();
        self.close_atomic();
        self.advance_generation_locked()
    }

    /// Publishes profile metadata and reopens only if `expected_generation` is
    /// still current.
    ///
    /// Lifecycle serialization and the expected-generation check make a hard
    /// reset linearize either before this operation (which then fails stale) or
    /// after it (which closes the newly opened gate). It can never reopen after
    /// a concurrent hard reset using a stale generation.
    pub(super) fn open_if_generation(
        &self,
        expected_generation: u64,
        profile: CaptureProfile,
        trace_deadline: Option<Instant>,
    ) -> Result<(), GateError> {
        let barrier = self.lock_barrier();
        if barrier.quiescence_waiters != 0 {
            return Err(GateError::QuiescenceWaitInProgress);
        }
        if profile == CaptureProfile::Normal && trace_deadline.is_some() {
            return Err(GateError::UnexpectedTraceDeadline);
        }

        let state = self.state.load(Ordering::Acquire);
        if state & CLOSED_BIT == 0 {
            return Err(GateError::NotClosed);
        }
        let readers = state & READER_MASK;
        if readers != 0 {
            return Err(GateError::ReadersActive(readers));
        }

        let actual_generation = self.generation.load(Ordering::Acquire);
        if actual_generation == u64::MAX {
            return Err(GateError::GenerationExhausted);
        }
        if actual_generation != expected_generation {
            return Err(GateError::GenerationMismatch {
                expected: expected_generation,
                actual: actual_generation,
            });
        }

        // Prove the zero-reader state with an RMW before touching the non-atomic
        // publication slot. This cannot succeed against a stale zero-reader
        // load if a producer release is still pending in the modification order.
        if self
            .state
            .compare_exchange(CLOSED_BIT, CLOSED_BIT, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.fail_closed();
            return Err(GateError::StateChanged);
        }

        // SAFETY: the barrier mutex excludes other publishers, and the exact
        // CLOSED_BIT RMW above proves no producer can be reading this slot.
        unsafe {
            *self.published.get() = PublishedAdmission {
                profile,
                trace_deadline,
            };
        }

        // The Release publishes the slot. No producer can change a closed
        // state; the barrier mutex excludes open/reset, while a concurrent
        // idempotent close linearizes before this reopen.
        if self
            .state
            .compare_exchange(CLOSED_BIT, 0, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            self.fail_closed();
            return Err(GateError::StateChanged);
        }

        Ok(())
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(super) fn is_closed(&self) -> bool {
        self.state.load(Ordering::Acquire) & CLOSED_BIT != 0
    }

    pub(super) fn active_readers(&self) -> u64 {
        self.state.load(Ordering::Acquire) & READER_MASK
    }

    fn lock_barrier(&self) -> MutexGuard<'_, BarrierState> {
        self.barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn close_atomic(&self) -> bool {
        self.state.fetch_or(CLOSED_BIT, Ordering::AcqRel) & CLOSED_BIT == 0
    }

    fn advance_generation_locked(&self) -> Result<u64, GateError> {
        let current = self.generation.load(Ordering::Acquire);
        let Some(next) = current.checked_add(1) else {
            self.fail_closed();
            return Err(GateError::GenerationExhausted);
        };
        self.generation.store(next, Ordering::Release);
        Ok(next)
    }

    fn wait_quiescent_locked(
        &self,
        mut barrier: MutexGuard<'_, BarrierState>,
    ) -> Result<(), GateError> {
        if !self.is_closed() {
            return Err(GateError::NotClosed);
        }
        let Some(waiters) = barrier.quiescence_waiters.checked_add(1) else {
            return Err(GateError::WaiterCountExhausted);
        };
        barrier.quiescence_waiters = waiters;

        loop {
            if self
                .state
                .compare_exchange(CLOSED_BIT, CLOSED_BIT, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }

            // Lease drop stays lock-free. The short timeout closes the otherwise
            // possible notify-before-wait race without putting a mutex on the
            // producer path; normal completion is woken immediately by notify.
            let (next, _) = self
                .quiescent
                .wait_timeout(barrier, QUIESCENCE_RECHECK)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            barrier = next;
        }

        barrier.quiescence_waiters -= 1;
        Ok(())
    }

    fn fail_closed(&self) {
        self.state.fetch_or(CLOSED_BIT, Ordering::AcqRel);
    }

    fn release_reader(&self) {
        let previous = self.state.fetch_sub(1, Ordering::Release);
        debug_assert_ne!(previous & READER_MASK, 0, "reader count underflow");
        if previous & READER_MASK == 1 {
            self.quiescent.notify_all();
        }
    }
}

/// RAII proof that a producer was admitted under one capture generation and
/// its Release-opened profile metadata.
pub(super) struct AdmissionLease<'a> {
    gate: &'a AdmissionGate,
    generation: u64,
    profile: CaptureProfile,
    trace_deadline: Option<Instant>,
}

impl AdmissionLease<'_> {
    pub(super) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) const fn profile(&self) -> CaptureProfile {
        self.profile
    }

    pub(super) const fn trace_deadline(&self) -> Option<Instant> {
        self.trace_deadline
    }
}

impl Drop for AdmissionLease<'_> {
    fn drop(&mut self) {
        self.gate.release_reader();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;

    use super::*;

    #[test]
    fn gate_starts_closed_and_named_queries_are_exact() {
        let gate = AdmissionGate::new();
        assert!(gate.is_closed());
        assert_eq!(gate.generation(), 0);
        assert_eq!(gate.active_readers(), 0);
        assert!(gate.try_admit().is_none());
    }

    #[test]
    fn lease_copies_generation_profile_and_monotonic_deadline() {
        let gate = AdmissionGate::new();
        let deadline = Instant::now() + Duration::from_secs(60);
        gate.open_if_generation(0, CaptureProfile::Trace, Some(deadline))
            .unwrap();

        let lease = gate.try_admit().expect("open gate should admit");
        assert_eq!(lease.generation(), 0);
        assert_eq!(lease.profile(), CaptureProfile::Trace);
        assert_eq!(lease.trace_deadline(), Some(deadline));
        assert_eq!(gate.active_readers(), 1);
        drop(lease);
        assert_eq!(gate.active_readers(), 0);
    }

    #[test]
    fn normal_profile_rejects_a_trace_deadline_and_stays_closed() {
        let gate = AdmissionGate::new();
        let error = gate
            .open_if_generation(
                0,
                CaptureProfile::Normal,
                Some(Instant::now() + Duration::from_secs(1)),
            )
            .unwrap_err();
        assert_eq!(error, GateError::UnexpectedTraceDeadline);
        assert!(gate.is_closed());

        gate.open_if_generation(0, CaptureProfile::Trace, None)
            .unwrap();
        let lease = gate.try_admit().unwrap();
        assert_eq!(lease.profile(), CaptureProfile::Trace);
        assert_eq!(lease.trace_deadline(), None);
    }

    #[test]
    fn close_is_idempotent_prevents_entries_and_waits_for_existing_lease() {
        let gate = Arc::new(AdmissionGate::new());
        gate.open_if_generation(0, CaptureProfile::Normal, None)
            .unwrap();

        let (admitted_tx, admitted_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let producer_gate = Arc::clone(&gate);
        let producer = thread::spawn(move || {
            let lease = producer_gate.try_admit().expect("producer should enter");
            admitted_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(lease);
        });
        admitted_rx.recv().unwrap();

        assert!(gate.close());
        assert!(!gate.close());
        assert!(gate.try_admit().is_none());

        let (done_tx, done_rx) = mpsc::sync_channel(0);
        let waiter_gate = Arc::clone(&gate);
        let waiter = thread::spawn(move || {
            waiter_gate.wait_quiescent().unwrap();
            done_tx.send(()).unwrap();
        });
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        release_tx.send(()).unwrap();
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("last lease should wake the quiescence waiter");
        producer.join().unwrap();
        waiter.join().unwrap();
        assert_eq!(gate.active_readers(), 0);
    }

    #[test]
    fn wait_quiescent_refuses_an_open_gate() {
        let gate = AdmissionGate::new();
        gate.open_if_generation(0, CaptureProfile::Normal, None)
            .unwrap();
        assert_eq!(gate.wait_quiescent(), Err(GateError::NotClosed));
        assert_eq!(gate.advance_generation(), Err(GateError::NotClosed));
    }

    #[test]
    fn hard_reset_closes_and_advances_without_waiting_for_reader() {
        let gate = Arc::new(AdmissionGate::new());
        gate.open_if_generation(0, CaptureProfile::Normal, None)
            .unwrap();

        let (admitted_tx, admitted_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let producer_gate = Arc::clone(&gate);
        let producer = thread::spawn(move || {
            let lease = producer_gate.try_admit().unwrap();
            assert_eq!(lease.generation(), 0);
            admitted_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(lease);
        });
        admitted_rx.recv().unwrap();

        let (reset_tx, reset_rx) = mpsc::sync_channel(0);
        let reset_gate = Arc::clone(&gate);
        let reset = thread::spawn(move || reset_tx.send(reset_gate.hard_reset()).unwrap());
        assert_eq!(
            reset_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("hard reset must not wait for the reader"),
            Ok(1)
        );
        assert!(gate.is_closed());
        assert_eq!(gate.generation(), 1);
        assert_eq!(gate.active_readers(), 1);

        release_tx.send(()).unwrap();
        gate.wait_quiescent().unwrap();
        producer.join().unwrap();
        reset.join().unwrap();
    }

    #[test]
    fn stale_open_after_hard_reset_is_rejected() {
        let gate = AdmissionGate::new();
        assert_eq!(gate.hard_reset(), Ok(1));
        assert_eq!(
            gate.open_if_generation(0, CaptureProfile::Normal, None),
            Err(GateError::GenerationMismatch {
                expected: 0,
                actual: 1,
            })
        );
        assert!(gate.is_closed());
    }

    #[test]
    fn close_reopen_aba_releases_tentative_reader_and_retries() {
        let gate = Arc::new(AdmissionGate::new());
        let old_deadline = Instant::now() + Duration::from_secs(60);
        gate.open_if_generation(0, CaptureProfile::Trace, Some(old_deadline))
            .unwrap();

        let (generation_read_tx, generation_read_rx) = mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = mpsc::sync_channel(0);
        let (observed_tx, observed_rx) = mpsc::sync_channel(0);
        let producer_gate = Arc::clone(&gate);
        let producer = thread::spawn(move || {
            let mut first_attempt = true;
            let lease = producer_gate
                .try_admit_inner(|| {
                    if first_attempt {
                        first_attempt = false;
                        generation_read_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                    }
                })
                .expect("the retried admission should enter the new generation");
            observed_tx
                .send((lease.generation(), lease.profile(), lease.trace_deadline()))
                .unwrap();
        });

        generation_read_rx.recv().unwrap();
        assert!(gate.close());
        gate.wait_quiescent().unwrap();
        assert_eq!(gate.advance_generation(), Ok(1));
        gate.open_if_generation(1, CaptureProfile::Normal, None)
            .unwrap();
        resume_tx.send(()).unwrap();

        assert_eq!(
            observed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            (1, CaptureProfile::Normal, None)
        );
        producer.join().unwrap();
        assert_eq!(gate.active_readers(), 0);
    }

    #[test]
    fn racing_open_and_hard_reset_never_finishes_stale_and_open() {
        for _ in 0..128 {
            let gate = Arc::new(AdmissionGate::new());
            let start = Arc::new(Barrier::new(3));

            let open_gate = Arc::clone(&gate);
            let open_start = Arc::clone(&start);
            let open = thread::spawn(move || {
                open_start.wait();
                open_gate.open_if_generation(0, CaptureProfile::Trace, None)
            });

            let reset_gate = Arc::clone(&gate);
            let reset_start = Arc::clone(&start);
            let reset = thread::spawn(move || {
                reset_start.wait();
                reset_gate.hard_reset()
            });

            start.wait();
            let open_result = open.join().unwrap();
            assert_eq!(reset.join().unwrap(), Ok(1));
            assert!(matches!(
                open_result,
                Ok(()) | Err(GateError::GenerationMismatch { .. })
            ));
            assert_eq!(gate.generation(), 1);
            assert!(gate.is_closed(), "stale opener won after hard reset");
            assert!(gate.try_admit().is_none());
        }
    }

    #[test]
    fn release_open_acquire_admission_publishes_metadata_across_threads() {
        const ROUNDS: u64 = 128;

        let gate = Arc::new(AdmissionGate::new());
        let base = Instant::now() + Duration::from_secs(60);
        let (observed_tx, observed_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let producer_gate = Arc::clone(&gate);
        let producer = thread::spawn(move || {
            for _ in 0..ROUNDS {
                let lease = loop {
                    if let Some(lease) = producer_gate.try_admit() {
                        break lease;
                    }
                    thread::yield_now();
                };
                observed_tx
                    .send((lease.generation(), lease.profile(), lease.trace_deadline()))
                    .unwrap();
                release_rx.recv().unwrap();
                drop(lease);
            }
        });

        for generation in 0..ROUNDS {
            let profile = if generation % 2 == 0 {
                CaptureProfile::Normal
            } else {
                CaptureProfile::Trace
            };
            let deadline = (profile == CaptureProfile::Trace)
                .then_some(base + Duration::from_nanos(generation));
            gate.open_if_generation(generation, profile, deadline)
                .unwrap();

            assert_eq!(observed_rx.recv().unwrap(), (generation, profile, deadline));
            gate.close();
            release_tx.send(()).unwrap();
            gate.wait_quiescent().unwrap();
            if generation + 1 < ROUNDS {
                assert_eq!(gate.advance_generation(), Ok(generation + 1));
            }
        }

        producer.join().unwrap();
    }

    #[test]
    fn reader_count_exhaustion_closes_without_wrapping() {
        let gate = AdmissionGate::new();
        gate.state.store(READER_MASK, Ordering::Relaxed);

        assert!(gate.try_admit().is_none());
        assert!(gate.is_closed());
        assert_eq!(gate.active_readers(), READER_MASK);
        assert_eq!(gate.state.load(Ordering::Relaxed), CLOSED_BIT | READER_MASK);
    }

    #[test]
    fn generation_exhaustion_never_wraps_or_reopens() {
        let gate = AdmissionGate::new();
        gate.generation.store(u64::MAX, Ordering::Relaxed);

        assert_eq!(
            gate.open_if_generation(u64::MAX, CaptureProfile::Normal, None),
            Err(GateError::GenerationExhausted)
        );
        assert_eq!(
            gate.advance_generation(),
            Err(GateError::GenerationExhausted)
        );
        assert_eq!(gate.hard_reset(), Err(GateError::GenerationExhausted));
        assert_eq!(gate.generation(), u64::MAX);
        assert!(gate.is_closed());
        assert!(gate.try_admit().is_none());
    }

    #[test]
    fn generation_advances_only_closed_and_expected_generation_controls_open() {
        let gate = AdmissionGate::new();
        gate.open_if_generation(0, CaptureProfile::Normal, None)
            .unwrap();
        assert_eq!(gate.advance_generation(), Err(GateError::NotClosed));

        gate.close();
        gate.wait_quiescent().unwrap();
        assert_eq!(gate.advance_generation(), Ok(1));
        assert_eq!(
            gate.open_if_generation(0, CaptureProfile::Normal, None),
            Err(GateError::GenerationMismatch {
                expected: 0,
                actual: 1,
            })
        );
        gate.open_if_generation(1, CaptureProfile::Normal, None)
            .unwrap();
        assert_eq!(gate.try_admit().unwrap().generation(), 1);
    }
}
