//! Lock-free pre-ingress rate admission and the FIFO's reserved-tail permit.

use std::array;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use super::schema::{ActivitySeverity, CaptureProfile, RateDomain};

pub(super) const INGRESS_CAPACITY: usize = 1_024;
pub(super) const RESERVED_PRIORITY_SLOTS: usize = 64;
pub(super) const LOW_PRIORITY_LIMIT: usize = INGRESS_CAPACITY - RESERVED_PRIORITY_SLOTS;

const NANOS_PER_SECOND: u64 = 1_000_000_000;
const NORMAL_RATE_PER_SECOND: u64 = 50;
const TRACE_RATE_PER_SECOND: u64 = 100;
const AMBIENT_RATE_PER_SECOND: u64 = 5;

pub(super) trait MonotonicClock: Send + Sync {
    fn now_tick(&self) -> u64;
}

pub(super) struct ProcessClock {
    origin: Instant,
}

impl ProcessClock {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
        })
    }
}

impl MonotonicClock for ProcessClock {
    fn now_tick(&self) -> u64 {
        self.origin.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
    }
}

/// A lock-free Generic Cell Rate Algorithm bucket. Its burst tolerance is
/// token-bucket equivalent: `capacity` events may pass at one instant, then
/// one token replenishes per interval.
struct GcraBucket {
    theoretical_arrival: AtomicU64,
    interval: u64,
    burst_window: u64,
}

impl GcraBucket {
    fn per_second(rate: u64, capacity: u64) -> Self {
        debug_assert!(rate > 0);
        debug_assert!(capacity > 0);
        let interval = NANOS_PER_SECOND / rate;
        Self {
            theoretical_arrival: AtomicU64::new(0),
            interval,
            burst_window: interval.saturating_mul(capacity),
        }
    }

    fn reset(&self, now: u64) {
        self.theoretical_arrival.store(now, Ordering::Relaxed);
    }

    fn try_take(&self, now: u64) -> bool {
        let mut observed = self.theoretical_arrival.load(Ordering::Relaxed);
        loop {
            let base = observed.max(now);
            let Some(next) = base.checked_add(self.interval) else {
                return false;
            };
            let deadline = now.saturating_add(self.burst_window);
            if next > deadline {
                return false;
            }
            match self.theoretical_arrival.compare_exchange_weak(
                observed,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => observed = actual,
            }
        }
    }
}

struct ProfileBuckets {
    global: GcraBucket,
    domains: [GcraBucket; RateDomain::COUNT],
}

impl ProfileBuckets {
    fn per_second(rate: u64) -> Self {
        Self {
            global: GcraBucket::per_second(rate, rate),
            domains: array::from_fn(|_| GcraBucket::per_second(rate, rate)),
        }
    }

    fn reset(&self, now: u64) {
        self.global.reset(now);
        for domain in &self.domains {
            domain.reset(now);
        }
    }

    fn try_take(&self, now: u64, domain: RateDomain) -> bool {
        // A failed domain admission does not consume a global token. The
        // reverse race can conservatively consume a domain token if another
        // thread wins the global CAS; no unsafe refund is attempted.
        self.domains[domain.index()].try_take(now) && self.global.try_take(now)
    }
}

pub(super) struct RateAdmission {
    clock: Arc<dyn MonotonicClock>,
    normal: ProfileBuckets,
    trace: ProfileBuckets,
    ambient: [GcraBucket; RateDomain::COUNT],
}

impl RateAdmission {
    pub(super) fn new(clock: Arc<dyn MonotonicClock>) -> Self {
        Self {
            clock,
            normal: ProfileBuckets::per_second(NORMAL_RATE_PER_SECOND),
            trace: ProfileBuckets::per_second(TRACE_RATE_PER_SECOND),
            ambient: array::from_fn(|_| {
                GcraBucket::per_second(AMBIENT_RATE_PER_SECOND, AMBIENT_RATE_PER_SECOND)
            }),
        }
    }

    pub(super) fn reset(&self, profile: CaptureProfile) {
        let now = self.clock.now_tick();
        match profile {
            CaptureProfile::Normal => self.normal.reset(now),
            CaptureProfile::Trace => self.trace.reset(now),
        }
        for ambient in &self.ambient {
            ambient.reset(now);
        }
    }

    pub(super) fn allow(
        &self,
        profile: CaptureProfile,
        severity: ActivitySeverity,
        domain: RateDomain,
        ambient: bool,
    ) -> bool {
        if severity == ActivitySeverity::Error {
            return true;
        }
        let now = self.clock.now_tick();
        if ambient && !self.ambient[domain.index()].try_take(now) {
            return false;
        }
        match profile {
            CaptureProfile::Normal => self.normal.try_take(now, domain),
            CaptureProfile::Trace => self.trace.try_take(now, domain),
        }
    }
}

/// At most 960 low-priority envelopes can hold one of these permits. The
/// permit moves through the channel with its draft and releases immediately
/// after receive or on any failed send/drop path.
pub(super) struct LowPermitPool {
    in_use: AtomicUsize,
}

impl LowPermitPool {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            in_use: AtomicUsize::new(0),
        })
    }

    pub(super) fn try_acquire(self: &Arc<Self>) -> Option<LowPermit> {
        let mut observed = self.in_use.load(Ordering::Relaxed);
        loop {
            if observed >= LOW_PRIORITY_LIMIT {
                return None;
            }
            match self.in_use.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(LowPermit {
                        pool: Arc::clone(self),
                    });
                }
                Err(actual) => observed = actual,
            }
        }
    }

    #[cfg(test)]
    fn in_use(&self) -> usize {
        self.in_use.load(Ordering::Relaxed)
    }
}

pub(super) struct LowPermit {
    pool: Arc<LowPermitPool>,
}

impl Drop for LowPermit {
    fn drop(&mut self) {
        let previous = self.pool.in_use.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 0, "low-priority permit count underflow");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;

    use super::*;

    #[derive(Default)]
    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn advance(&self, nanos: u64) {
            self.0.fetch_add(nanos, Ordering::Relaxed);
        }
    }

    impl MonotonicClock for FakeClock {
        fn now_tick(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn normal_and_trace_bursts_are_exact_and_replenish() {
        let clock = Arc::new(FakeClock::default());
        let rate = RateAdmission::new(clock.clone());
        rate.reset(CaptureProfile::Normal);
        for _ in 0..NORMAL_RATE_PER_SECOND {
            assert!(rate.allow(
                CaptureProfile::Normal,
                ActivitySeverity::Info,
                RateDomain::Network,
                false
            ));
        }
        assert!(!rate.allow(
            CaptureProfile::Normal,
            ActivitySeverity::Info,
            RateDomain::Network,
            false
        ));
        clock.advance(NANOS_PER_SECOND / NORMAL_RATE_PER_SECOND);
        assert!(rate.allow(
            CaptureProfile::Normal,
            ActivitySeverity::Info,
            RateDomain::Network,
            false
        ));

        rate.reset(CaptureProfile::Trace);
        for _ in 0..TRACE_RATE_PER_SECOND {
            assert!(rate.allow(
                CaptureProfile::Trace,
                ActivitySeverity::Warning,
                RateDomain::Channels,
                false
            ));
        }
        assert!(!rate.allow(
            CaptureProfile::Trace,
            ActivitySeverity::Warning,
            RateDomain::Channels,
            false
        ));
    }

    #[test]
    fn ambient_bucket_is_five_per_second_and_errors_bypass_every_bucket() {
        let clock = Arc::new(FakeClock::default());
        let rate = RateAdmission::new(clock);
        rate.reset(CaptureProfile::Trace);
        for _ in 0..AMBIENT_RATE_PER_SECOND {
            assert!(rate.allow(
                CaptureProfile::Trace,
                ActivitySeverity::Info,
                RateDomain::Network,
                true
            ));
        }
        assert!(!rate.allow(
            CaptureProfile::Trace,
            ActivitySeverity::Info,
            RateDomain::Network,
            true
        ));
        for _ in 0..2_000 {
            assert!(rate.allow(
                CaptureProfile::Normal,
                ActivitySeverity::Error,
                RateDomain::Network,
                true
            ));
        }
    }

    #[test]
    fn concurrent_low_permits_never_enter_the_reserved_tail() {
        let pool = LowPermitPool::new();
        const WORKERS: usize = 32;
        const PER_WORKER: usize = LOW_PRIORITY_LIMIT / WORKERS;
        let start = Arc::new(Barrier::new(WORKERS + 1));
        let mut workers = Vec::with_capacity(WORKERS);
        for _ in 0..WORKERS {
            let pool = Arc::clone(&pool);
            let start = Arc::clone(&start);
            workers.push(thread::spawn(move || {
                let permits: Vec<_> = (0..PER_WORKER)
                    .map(|_| pool.try_acquire().expect("first 960 must fit"))
                    .collect();
                start.wait();
                permits
            }));
        }
        start.wait();
        assert_eq!(pool.in_use(), LOW_PRIORITY_LIMIT);
        assert!(pool.try_acquire().is_none());
        for worker in workers {
            drop(worker.join().expect("permit worker should finish"));
        }
        assert_eq!(pool.in_use(), 0);
    }
}
