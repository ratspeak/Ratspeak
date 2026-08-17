//! Content-free cumulative health counters and consumer-owned loss windows.
//!
//! Health state deliberately contains only counts and observation timestamps.
//! It cannot retain event payloads, classified values, errors, or arbitrary
//! strings.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use serde::Serialize;

use super::schema::RateDomain;

/// Cumulative, process-local health counters for the Activity recorder.
///
/// [`ActivityHealth::new`] returns an [`Arc`] because producers share these
/// counters. Mutation remains available only through named, saturating methods
/// so a counter can neither wrap nor be selected dynamically by a string.
#[derive(Debug, Default)]
pub(crate) struct ActivityHealth {
    ingress_full: AtomicU64,
    rate_limited: AtomicU64,
    oversized_invalid_rejected: AtomicU64,
    count_limit_evicted_events: AtomicU64,
    byte_limit_evicted_events: AtomicU64,
    ipc_failure: AtomicU64,
    replay_gap: AtomicU64,
    coalesced_inputs: AtomicU64,
    worker_recovery: AtomicU64,
    sampled_observations: [LossObservations; RateDomain::COUNT],
    ingress_full_observations: LossObservations,
    invalid_observations: LossObservations,
}

impl ActivityHealth {
    /// Creates zeroed health counters in their shared ownership container.
    #[must_use]
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn increment_ingress_full(&self) {
        self.add_ingress_full(1);
    }

    pub(crate) fn increment_ingress_full_at(&self, observed_unix_ms: u64) {
        self.increment_ingress_full();
        self.ingress_full_observations.note(1, observed_unix_ms);
    }

    pub(crate) fn add_ingress_full(&self, count: u64) {
        saturating_atomic_add(&self.ingress_full, count);
    }

    pub(crate) fn increment_rate_limited(&self) {
        self.add_rate_limited(1);
    }

    pub(crate) fn increment_rate_limited_at(&self, observed_unix_ms: u64, domain: RateDomain) {
        self.increment_rate_limited();
        self.sampled_observations[domain.index()].note(1, observed_unix_ms);
    }

    pub(crate) fn add_rate_limited(&self, count: u64) {
        saturating_atomic_add(&self.rate_limited, count);
    }

    pub(crate) fn increment_oversized_invalid_rejected(&self) {
        self.add_oversized_invalid_rejected(1);
    }

    pub(crate) fn increment_oversized_invalid_rejected_at(&self, observed_unix_ms: u64) {
        self.increment_oversized_invalid_rejected();
        self.invalid_observations.note(1, observed_unix_ms);
    }

    pub(crate) fn add_oversized_invalid_rejected(&self, count: u64) {
        saturating_atomic_add(&self.oversized_invalid_rejected, count);
    }

    #[cfg(test)]
    pub(crate) fn increment_count_limit_evicted_events(&self) {
        self.add_count_limit_evicted_events(1);
    }

    pub(crate) fn add_count_limit_evicted_events(&self, count: u64) {
        saturating_atomic_add(&self.count_limit_evicted_events, count);
    }

    #[cfg(test)]
    pub(crate) fn increment_byte_limit_evicted_events(&self) {
        self.add_byte_limit_evicted_events(1);
    }

    pub(crate) fn add_byte_limit_evicted_events(&self, count: u64) {
        saturating_atomic_add(&self.byte_limit_evicted_events, count);
    }

    pub(crate) fn increment_ipc_failure(&self) {
        self.add_ipc_failure(1);
    }

    pub(crate) fn add_ipc_failure(&self, count: u64) {
        saturating_atomic_add(&self.ipc_failure, count);
    }

    pub(crate) fn increment_replay_gap(&self) {
        self.add_replay_gap(1);
    }

    pub(crate) fn add_replay_gap(&self, count: u64) {
        saturating_atomic_add(&self.replay_gap, count);
    }

    #[cfg(test)]
    pub(crate) fn increment_coalesced_inputs(&self) {
        self.add_coalesced_inputs(1);
    }

    pub(crate) fn add_coalesced_inputs(&self, count: u64) {
        saturating_atomic_add(&self.coalesced_inputs, count);
    }

    pub(crate) fn increment_worker_recovery(&self) {
        self.add_worker_recovery(1);
    }

    pub(crate) fn add_worker_recovery(&self, count: u64) {
        saturating_atomic_add(&self.worker_recovery, count);
    }

    pub(crate) fn take_capture_windows(&self) -> CaptureWindows {
        CaptureWindows {
            sampled: std::array::from_fn(|index| self.sampled_observations[index].take()),
            ingress_full: self.ingress_full_observations.take(),
            invalid: self.invalid_observations.take(),
        }
    }

    /// Takes an exact, JavaScript-safe copy of every cumulative counter.
    ///
    /// Each atomic is sampled independently. The returned decimal strings
    /// preserve all `u64` values without a lossy conversion through a
    /// JavaScript `Number`.
    #[must_use]
    pub(crate) fn snapshot(&self) -> ActivityHealthSnapshot {
        ActivityHealthSnapshot {
            ingress_full: decimal_snapshot(&self.ingress_full),
            rate_limited: decimal_snapshot(&self.rate_limited),
            oversized_invalid_rejected: decimal_snapshot(&self.oversized_invalid_rejected),
            count_limit_evicted_events: decimal_snapshot(&self.count_limit_evicted_events),
            byte_limit_evicted_events: decimal_snapshot(&self.byte_limit_evicted_events),
            ipc_failure: decimal_snapshot(&self.ipc_failure),
            replay_gap: decimal_snapshot(&self.replay_gap),
            coalesced_inputs: decimal_snapshot(&self.coalesced_inputs),
            worker_recovery: decimal_snapshot(&self.worker_recovery),
        }
    }
}

fn saturating_atomic_add(counter: &AtomicU64, count: u64) {
    if count == 0 {
        return;
    }

    // These counters do not publish or guard any other state, so relaxed
    // ordering is sufficient. `fetch_update` retries the saturating operation
    // against the latest observed value and therefore cannot lose increments.
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        (current != u64::MAX).then(|| current.saturating_add(count))
    });
}

fn decimal_snapshot(counter: &AtomicU64) -> String {
    counter.load(Ordering::Relaxed).to_string()
}

/// Serializable Activity health with exact decimal-string counter values.
///
/// Fields are private so only `ActivityHealth::snapshot` can construct this
/// wire-safe representation. Named accessors support native inspection without
/// exposing a generic lookup surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivityHealthSnapshot {
    ingress_full: String,
    rate_limited: String,
    oversized_invalid_rejected: String,
    count_limit_evicted_events: String,
    byte_limit_evicted_events: String,
    ipc_failure: String,
    replay_gap: String,
    coalesced_inputs: String,
    worker_recovery: String,
}

impl ActivityHealthSnapshot {
    #[must_use]
    pub fn ingress_full(&self) -> &str {
        &self.ingress_full
    }

    #[must_use]
    pub fn rate_limited(&self) -> &str {
        &self.rate_limited
    }

    #[must_use]
    pub fn oversized_invalid_rejected(&self) -> &str {
        &self.oversized_invalid_rejected
    }

    #[must_use]
    pub fn count_limit_evicted_events(&self) -> &str {
        &self.count_limit_evicted_events
    }

    #[must_use]
    pub fn byte_limit_evicted_events(&self) -> &str {
        &self.byte_limit_evicted_events
    }

    #[must_use]
    pub fn ipc_failure(&self) -> &str {
        &self.ipc_failure
    }

    #[must_use]
    pub fn replay_gap(&self) -> &str {
        &self.replay_gap
    }

    #[must_use]
    pub fn coalesced_inputs(&self) -> &str {
        &self.coalesced_inputs
    }

    #[must_use]
    pub fn worker_recovery(&self) -> &str {
        &self.worker_recovery
    }
}

const NO_LOSS_TIMESTAMP: u64 = u64::MAX;

#[derive(Debug)]
struct LossObservationSlot {
    readers: AtomicUsize,
    count: AtomicU64,
    first_unix_ms: AtomicU64,
    last_unix_ms: AtomicU64,
}

impl LossObservationSlot {
    const fn new() -> Self {
        Self {
            readers: AtomicUsize::new(0),
            count: AtomicU64::new(0),
            first_unix_ms: AtomicU64::new(NO_LOSS_TIMESTAMP),
            last_unix_ms: AtomicU64::new(0),
        }
    }

    fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.first_unix_ms
            .store(NO_LOSS_TIMESTAMP, Ordering::Relaxed);
        self.last_unix_ms.store(0, Ordering::Relaxed);
    }
}

/// Double-buffered numeric-only observation window. Loss producers never wait:
/// they briefly enter the currently published slot, revalidate it, and update
/// atomics. The single consumer rotates slots and waits only on its own worker
/// thread before draining the now-inactive slot.
#[derive(Debug)]
struct LossObservations {
    active_epoch: AtomicU64,
    slots: [LossObservationSlot; 2],
}

impl Default for LossObservations {
    fn default() -> Self {
        Self {
            active_epoch: AtomicU64::new(0),
            slots: [LossObservationSlot::new(), LossObservationSlot::new()],
        }
    }
}

impl LossObservations {
    fn note(&self, count: u64, observed_unix_ms: u64) {
        if count == 0 {
            return;
        }
        loop {
            let epoch = self.active_epoch.load(Ordering::Acquire);
            let slot = &self.slots[(epoch & 1) as usize];
            if slot
                .readers
                .fetch_update(Ordering::Acquire, Ordering::Relaxed, |readers| {
                    readers.checked_add(1)
                })
                .is_err()
            {
                return;
            }
            if self.active_epoch.load(Ordering::Acquire) != epoch {
                slot.readers.fetch_sub(1, Ordering::Release);
                continue;
            }

            saturating_atomic_add(&slot.count, count);
            slot.first_unix_ms
                .fetch_min(observed_unix_ms, Ordering::Relaxed);
            slot.last_unix_ms
                .fetch_max(observed_unix_ms, Ordering::Relaxed);
            slot.readers.fetch_sub(1, Ordering::Release);
            return;
        }
    }

    fn take(&self) -> Option<LossWindow> {
        let old_epoch = self.active_epoch.load(Ordering::Acquire);
        let old = (old_epoch & 1) as usize;
        if self.slots[old].count.load(Ordering::Acquire) == 0 {
            return None;
        }
        let next_epoch = old_epoch.checked_add(1)?;
        let next = (next_epoch & 1) as usize;
        let next_slot = &self.slots[next];
        next_slot.reset();
        // The Activity worker is the sole consumer. Publishing an exact epoch
        // makes stale-slot fencing explicit even when the same physical slot
        // becomes active again after two rotations.
        self.active_epoch.store(next_epoch, Ordering::Release);

        let old_slot = &self.slots[old];
        while old_slot.readers.load(Ordering::Acquire) != 0 {
            std::hint::spin_loop();
            std::thread::yield_now();
        }
        let count = old_slot.count.swap(0, Ordering::AcqRel);
        let first_observed_unix_ms = old_slot
            .first_unix_ms
            .swap(NO_LOSS_TIMESTAMP, Ordering::AcqRel);
        let last_observed_unix_ms = old_slot.last_unix_ms.swap(0, Ordering::AcqRel);
        if count == 0 {
            return None;
        }
        let first_observed_unix_ms = if first_observed_unix_ms == NO_LOSS_TIMESTAMP {
            last_observed_unix_ms
        } else {
            first_observed_unix_ms
        };
        Some(LossWindow {
            count,
            first_observed_unix_ms,
            last_observed_unix_ms,
        })
    }
}

/// A completed interval of observed losses.
///
/// Timestamps are the chronological minimum and maximum producer observation
/// times in the completed window. This remains a numeric-only type so loss
/// accounting cannot carry event content or arbitrary reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LossWindow {
    count: u64,
    first_observed_unix_ms: u64,
    last_observed_unix_ms: u64,
}

impl LossWindow {
    #[must_use]
    pub(crate) const fn count(&self) -> u64 {
        self.count
    }

    #[must_use]
    pub(crate) const fn first_observed_unix_ms(&self) -> u64 {
        self.first_observed_unix_ms
    }

    #[must_use]
    pub(crate) const fn last_observed_unix_ms(&self) -> u64 {
        self.last_observed_unix_ms
    }
}

/// Exact, payload-free summaries drained by the Activity worker.
///
/// Rate-limited observations are kept per fixed domain so the UI can say what
/// was summarized. Recorder pressure and invalid drafts remain separate
/// because those are genuine capture failures rather than intentional
/// sampling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureWindows {
    sampled: [Option<LossWindow>; RateDomain::COUNT],
    ingress_full: Option<LossWindow>,
    invalid: Option<LossWindow>,
}

impl CaptureWindows {
    pub(crate) fn sampled(&self, domain: RateDomain) -> Option<LossWindow> {
        self.sampled[domain.index()]
    }

    pub(crate) const fn ingress_full(&self) -> Option<LossWindow> {
        self.ingress_full
    }

    pub(crate) const fn invalid(&self) -> Option<LossWindow> {
        self.invalid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_increment_and_add_methods_update_every_counter() {
        let health = ActivityHealth::new();

        health.increment_ingress_full();
        health.add_ingress_full(2);
        health.increment_rate_limited();
        health.add_rate_limited(3);
        health.increment_oversized_invalid_rejected();
        health.add_oversized_invalid_rejected(4);
        health.increment_count_limit_evicted_events();
        health.add_count_limit_evicted_events(5);
        health.increment_byte_limit_evicted_events();
        health.add_byte_limit_evicted_events(6);
        health.increment_ipc_failure();
        health.add_ipc_failure(7);
        health.increment_replay_gap();
        health.add_replay_gap(8);
        health.increment_coalesced_inputs();
        health.add_coalesced_inputs(9);
        health.increment_worker_recovery();
        health.add_worker_recovery(10);

        let snapshot = health.snapshot();
        assert_eq!(snapshot.ingress_full(), "3");
        assert_eq!(snapshot.rate_limited(), "4");
        assert_eq!(snapshot.oversized_invalid_rejected(), "5");
        assert_eq!(snapshot.count_limit_evicted_events(), "6");
        assert_eq!(snapshot.byte_limit_evicted_events(), "7");
        assert_eq!(snapshot.ipc_failure(), "8");
        assert_eq!(snapshot.replay_gap(), "9");
        assert_eq!(snapshot.coalesced_inputs(), "10");
        assert_eq!(snapshot.worker_recovery(), "11");
    }

    #[test]
    fn zero_adds_leave_every_counter_unchanged() {
        let health = ActivityHealth::new();

        health.add_ingress_full(0);
        health.add_rate_limited(0);
        health.add_oversized_invalid_rejected(0);
        health.add_count_limit_evicted_events(0);
        health.add_byte_limit_evicted_events(0);
        health.add_ipc_failure(0);
        health.add_replay_gap(0);
        health.add_coalesced_inputs(0);
        health.add_worker_recovery(0);

        let snapshot = health.snapshot();
        assert_eq!(snapshot.ingress_full(), "0");
        assert_eq!(snapshot.rate_limited(), "0");
        assert_eq!(snapshot.oversized_invalid_rejected(), "0");
        assert_eq!(snapshot.count_limit_evicted_events(), "0");
        assert_eq!(snapshot.byte_limit_evicted_events(), "0");
        assert_eq!(snapshot.ipc_failure(), "0");
        assert_eq!(snapshot.replay_gap(), "0");
        assert_eq!(snapshot.coalesced_inputs(), "0");
        assert_eq!(snapshot.worker_recovery(), "0");
    }

    #[test]
    fn every_counter_saturates_instead_of_wrapping() {
        let health = ActivityHealth::new();

        health.add_ingress_full(u64::MAX);
        health.increment_ingress_full();
        health.add_rate_limited(u64::MAX - 1);
        health.add_rate_limited(9);
        health.add_oversized_invalid_rejected(u64::MAX);
        health.increment_oversized_invalid_rejected();
        health.add_count_limit_evicted_events(u64::MAX - 2);
        health.add_count_limit_evicted_events(10);
        health.add_byte_limit_evicted_events(u64::MAX);
        health.increment_byte_limit_evicted_events();
        health.add_ipc_failure(u64::MAX - 3);
        health.add_ipc_failure(11);
        health.add_replay_gap(u64::MAX);
        health.increment_replay_gap();
        health.add_coalesced_inputs(u64::MAX - 4);
        health.add_coalesced_inputs(12);
        health.add_worker_recovery(u64::MAX);
        health.increment_worker_recovery();

        let snapshot = health.snapshot();
        let max = u64::MAX.to_string();
        assert_eq!(snapshot.ingress_full(), max);
        assert_eq!(snapshot.rate_limited(), max);
        assert_eq!(snapshot.oversized_invalid_rejected(), max);
        assert_eq!(snapshot.count_limit_evicted_events(), max);
        assert_eq!(snapshot.byte_limit_evicted_events(), max);
        assert_eq!(snapshot.ipc_failure(), max);
        assert_eq!(snapshot.replay_gap(), max);
        assert_eq!(snapshot.coalesced_inputs(), max);
        assert_eq!(snapshot.worker_recovery(), max);
    }

    #[test]
    fn arc_clones_share_exact_atomic_updates() {
        let health = ActivityHealth::new();
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let health = Arc::clone(&health);
                std::thread::spawn(move || {
                    for _ in 0..2_500 {
                        health.increment_ingress_full();
                    }
                })
            })
            .collect();

        for worker in workers {
            worker.join().expect("health update worker should finish");
        }

        assert_eq!(health.snapshot().ingress_full(), "10000");
    }

    #[test]
    fn snapshot_serializes_every_u64_as_an_exact_decimal_string() {
        let health = ActivityHealth::new();
        health.add_ingress_full(9_007_199_254_740_993);
        health.add_rate_limited(u64::MAX);
        health.add_oversized_invalid_rejected(2);
        health.add_count_limit_evicted_events(3);
        health.add_byte_limit_evicted_events(4);
        health.add_ipc_failure(5);
        health.add_replay_gap(6);
        health.add_coalesced_inputs(7);
        health.add_worker_recovery(8);

        assert_eq!(
            serde_json::to_value(health.snapshot()).expect("snapshot should serialize"),
            serde_json::json!({
                "ingress_full": "9007199254740993",
                "rate_limited": "18446744073709551615",
                "oversized_invalid_rejected": "2",
                "count_limit_evicted_events": "3",
                "byte_limit_evicted_events": "4",
                "ipc_failure": "5",
                "replay_gap": "6",
                "coalesced_inputs": "7",
                "worker_recovery": "8",
            })
        );
    }

    #[test]
    fn loss_window_uses_chronological_timestamp_bounds() {
        let observations = LossObservations::default();
        observations.note(2, 500);
        observations.note(3, 100);
        observations.note(4, 300);

        let window = observations
            .take()
            .expect("observations should produce a window");
        assert_eq!(window.count(), 9);
        assert_eq!(window.first_observed_unix_ms(), 100);
        assert_eq!(window.last_observed_unix_ms(), 500);
        assert_eq!(observations.take(), None);
    }

    #[test]
    fn zero_loss_note_does_not_open_or_extend_a_window() {
        let observations = LossObservations::default();
        observations.note(0, 100);
        assert_eq!(observations.take(), None);

        observations.note(1, 200);
        observations.note(0, 300);
        assert_eq!(
            observations.take(),
            Some(LossWindow {
                count: 1,
                first_observed_unix_ms: 200,
                last_observed_unix_ms: 200,
            })
        );
    }

    #[test]
    fn loss_count_saturates_while_last_observation_keeps_advancing() {
        let observations = LossObservations::default();
        observations.note(u64::MAX, 10);
        observations.note(1, 20);

        let window = observations
            .take()
            .expect("observations should produce a window");
        assert_eq!(window.count(), u64::MAX);
        assert_eq!(window.first_observed_unix_ms(), 10);
        assert_eq!(window.last_observed_unix_ms(), 20);
    }

    #[test]
    fn take_rotates_then_accepts_a_distinct_next_interval() {
        let observations = LossObservations::default();
        observations.note(2, 10);

        assert_eq!(
            observations.take(),
            Some(LossWindow {
                count: 2,
                first_observed_unix_ms: 10,
                last_observed_unix_ms: 10,
            })
        );
        assert_eq!(observations.take(), None);

        observations.note(3, 50);
        assert_eq!(
            observations.take(),
            Some(LossWindow {
                count: 3,
                first_observed_unix_ms: 50,
                last_observed_unix_ms: 50,
            })
        );
    }

    #[test]
    fn concurrent_loss_observations_are_not_lost() {
        let health = ActivityHealth::new();
        let workers: Vec<_> = (0..4u64)
            .map(|worker| {
                let health = Arc::clone(&health);
                std::thread::spawn(move || {
                    for _ in 0..2_500 {
                        health.increment_ingress_full_at(1_000 + worker);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("loss producer should finish");
        }

        let windows = health.take_capture_windows();
        let window = windows
            .ingress_full()
            .expect("concurrent observations should produce a window");
        assert_eq!(window.count(), 10_000);
        assert_eq!(window.first_observed_unix_ms(), 1_000);
        assert_eq!(window.last_observed_unix_ms(), 1_003);
        assert_eq!(health.snapshot().ingress_full(), "10000");
        assert_eq!(health.take_capture_windows().ingress_full(), None);
    }

    #[test]
    fn sampled_windows_remain_separate_by_domain_and_from_capture_failures() {
        let health = ActivityHealth::new();
        health.increment_rate_limited_at(100, RateDomain::Network);
        health.increment_rate_limited_at(125, RateDomain::Network);
        health.increment_rate_limited_at(150, RateDomain::Channels);
        health.increment_ingress_full_at(175);
        health.increment_oversized_invalid_rejected_at(200);

        let windows = health.take_capture_windows();
        let network = windows
            .sampled(RateDomain::Network)
            .expect("network samples should be summarized");
        assert_eq!(network.count(), 2);
        assert_eq!(network.first_observed_unix_ms(), 100);
        assert_eq!(network.last_observed_unix_ms(), 125);
        assert_eq!(
            windows
                .sampled(RateDomain::Channels)
                .expect("channel samples should be summarized")
                .count(),
            1
        );
        assert_eq!(
            windows
                .ingress_full()
                .expect("ingress pressure should remain distinct")
                .count(),
            1
        );
        assert_eq!(
            windows
                .invalid()
                .expect("invalid drafts should remain distinct")
                .count(),
            1
        );
    }
}
