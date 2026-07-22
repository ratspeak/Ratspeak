//! Content-free cumulative health counters and consumer-owned loss windows.
//!
//! Health state deliberately contains only counts and observation timestamps.
//! It cannot retain event payloads, classified values, errors, or arbitrary
//! strings.

#![allow(
    dead_code,
    reason = "Stage 1A defines health accounting; Stage 1B wires the recorder"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

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

    pub(crate) fn add_ingress_full(&self, count: u64) {
        saturating_atomic_add(&self.ingress_full, count);
    }

    pub(crate) fn increment_rate_limited(&self) {
        self.add_rate_limited(1);
    }

    pub(crate) fn add_rate_limited(&self, count: u64) {
        saturating_atomic_add(&self.rate_limited, count);
    }

    pub(crate) fn increment_oversized_invalid_rejected(&self) {
        self.add_oversized_invalid_rejected(1);
    }

    pub(crate) fn add_oversized_invalid_rejected(&self, count: u64) {
        saturating_atomic_add(&self.oversized_invalid_rejected, count);
    }

    pub(crate) fn increment_count_limit_evicted_events(&self) {
        self.add_count_limit_evicted_events(1);
    }

    pub(crate) fn add_count_limit_evicted_events(&self, count: u64) {
        saturating_atomic_add(&self.count_limit_evicted_events, count);
    }

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

    pub(crate) fn increment_coalesced_inputs(&self) {
        self.add_coalesced_inputs(1);
    }

    pub(crate) fn add_coalesced_inputs(&self, count: u64) {
        saturating_atomic_add(&self.coalesced_inputs, count);
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
/// Fields are private so only [`ActivityHealth::snapshot`] can construct this
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
}

/// A completed interval of observed losses.
///
/// Timestamps record consumer observation order, not chronological minima and
/// maxima. This remains a numeric-only type so loss accounting cannot carry
/// event content or arbitrary reasons.
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

/// Consumer-owned accumulator for the next visible loss marker.
///
/// This type is intentionally non-atomic: the single Activity consumer owns
/// it. A zero-count observation is ignored and cannot open or extend a window.
#[derive(Debug, Default)]
pub(crate) struct PendingLossWindow {
    count: u64,
    first_observed_unix_ms: Option<u64>,
    last_observed_unix_ms: Option<u64>,
}

impl PendingLossWindow {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            count: 0,
            first_observed_unix_ms: None,
            last_observed_unix_ms: None,
        }
    }

    #[must_use]
    pub(crate) const fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[must_use]
    pub(crate) const fn count(&self) -> u64 {
        self.count
    }

    #[must_use]
    pub(crate) const fn first_observed_unix_ms(&self) -> Option<u64> {
        self.first_observed_unix_ms
    }

    #[must_use]
    pub(crate) const fn last_observed_unix_ms(&self) -> Option<u64> {
        self.last_observed_unix_ms
    }

    /// Adds `count` losses observed at `observed_unix_ms`.
    ///
    /// The count saturates at `u64::MAX`. First and last timestamps follow call
    /// order even if the wall clock moves backwards between observations.
    pub(crate) fn note(&mut self, count: u64, observed_unix_ms: u64) {
        if count == 0 {
            return;
        }

        if self.is_empty() {
            self.first_observed_unix_ms = Some(observed_unix_ms);
        }
        self.count = self.count.saturating_add(count);
        self.last_observed_unix_ms = Some(observed_unix_ms);
    }

    /// Returns the accumulated window and atomically clears this owned state.
    pub(crate) fn take(&mut self) -> Option<LossWindow> {
        if self.is_empty() {
            return None;
        }

        let count = self.count;
        let first_observed_unix_ms = self
            .first_observed_unix_ms
            .expect("non-empty loss window has a first observation");
        let last_observed_unix_ms = self
            .last_observed_unix_ms
            .expect("non-empty loss window has a last observation");
        self.clear();

        Some(LossWindow {
            count,
            first_observed_unix_ms,
            last_observed_unix_ms,
        })
    }

    /// Discards all pending loss observations.
    pub(crate) fn clear(&mut self) {
        self.count = 0;
        self.first_observed_unix_ms = None;
        self.last_observed_unix_ms = None;
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

        let snapshot = health.snapshot();
        assert_eq!(snapshot.ingress_full(), "3");
        assert_eq!(snapshot.rate_limited(), "4");
        assert_eq!(snapshot.oversized_invalid_rejected(), "5");
        assert_eq!(snapshot.count_limit_evicted_events(), "6");
        assert_eq!(snapshot.byte_limit_evicted_events(), "7");
        assert_eq!(snapshot.ipc_failure(), "8");
        assert_eq!(snapshot.replay_gap(), "9");
        assert_eq!(snapshot.coalesced_inputs(), "10");
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

        let snapshot = health.snapshot();
        assert_eq!(snapshot.ingress_full(), "0");
        assert_eq!(snapshot.rate_limited(), "0");
        assert_eq!(snapshot.oversized_invalid_rejected(), "0");
        assert_eq!(snapshot.count_limit_evicted_events(), "0");
        assert_eq!(snapshot.byte_limit_evicted_events(), "0");
        assert_eq!(snapshot.ipc_failure(), "0");
        assert_eq!(snapshot.replay_gap(), "0");
        assert_eq!(snapshot.coalesced_inputs(), "0");
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
            })
        );
    }

    #[test]
    fn loss_window_uses_observation_order_not_timestamp_sorting() {
        let mut pending = PendingLossWindow::new();

        pending.note(2, 500);
        pending.note(3, 100);
        pending.note(4, 300);

        assert!(!pending.is_empty());
        assert_eq!(pending.count(), 9);
        assert_eq!(pending.first_observed_unix_ms(), Some(500));
        assert_eq!(pending.last_observed_unix_ms(), Some(300));

        let window = pending
            .take()
            .expect("observations should produce a window");
        assert_eq!(window.count(), 9);
        assert_eq!(window.first_observed_unix_ms(), 500);
        assert_eq!(window.last_observed_unix_ms(), 300);
    }

    #[test]
    fn zero_loss_note_does_not_open_or_extend_a_window() {
        let mut pending = PendingLossWindow::new();
        pending.note(0, 100);
        assert!(pending.is_empty());
        assert_eq!(pending.first_observed_unix_ms(), None);
        assert_eq!(pending.last_observed_unix_ms(), None);

        pending.note(1, 200);
        pending.note(0, 300);
        assert_eq!(pending.count(), 1);
        assert_eq!(pending.first_observed_unix_ms(), Some(200));
        assert_eq!(pending.last_observed_unix_ms(), Some(200));
    }

    #[test]
    fn loss_count_saturates_while_last_observation_keeps_advancing() {
        let mut pending = PendingLossWindow::new();
        pending.note(u64::MAX, 10);
        pending.note(1, 20);

        let window = pending
            .take()
            .expect("observations should produce a window");
        assert_eq!(window.count(), u64::MAX);
        assert_eq!(window.first_observed_unix_ms(), 10);
        assert_eq!(window.last_observed_unix_ms(), 20);
    }

    #[test]
    fn take_returns_window_then_resets_for_the_next_interval() {
        let mut pending = PendingLossWindow::new();
        pending.note(2, 10);

        assert_eq!(
            pending.take(),
            Some(LossWindow {
                count: 2,
                first_observed_unix_ms: 10,
                last_observed_unix_ms: 10,
            })
        );
        assert!(pending.is_empty());
        assert_eq!(pending.count(), 0);
        assert_eq!(pending.first_observed_unix_ms(), None);
        assert_eq!(pending.last_observed_unix_ms(), None);
        assert_eq!(pending.take(), None);

        pending.note(3, 50);
        assert_eq!(
            pending.take(),
            Some(LossWindow {
                count: 3,
                first_observed_unix_ms: 50,
                last_observed_unix_ms: 50,
            })
        );
    }

    #[test]
    fn clear_discards_all_pending_observations() {
        let mut pending = PendingLossWindow::new();
        pending.note(7, 700);
        pending.note(8, 800);
        pending.clear();

        assert!(pending.is_empty());
        assert_eq!(pending.count(), 0);
        assert_eq!(pending.first_observed_unix_ms(), None);
        assert_eq!(pending.last_observed_unix_ms(), None);
        assert_eq!(pending.take(), None);
    }
}
