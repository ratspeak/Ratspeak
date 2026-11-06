//! I/O-free upload window tracking and caller-accounted airtime pacing.
//!
//! The caller sends chunks returned by [`Uploader::next`], supplies authenticated reports for
//! this transfer, and polls at [`Uploader::wake_at`] even without incoming traffic. These helpers
//! own no payload, Link or firmware verification. Use one monotonic millisecond clock throughout.

use crate::frame::OTA_CHUNK_DATA_MAX;

/// Re-sends of one window without progress before the transfer is given up.
pub const UPLOAD_RETRIES_MAX: u32 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UploadConfig {
    /// Bytes the node expects: the image, or the patch.
    pub total: u32,
    /// The largest chunk the node accepts, from its `ota.begin` reply.
    pub chunk_max: u16,
    /// Chunks in flight before an acknowledgement must return.
    pub window: u16,
    /// How long an acknowledgement may take: a few round trips plus the window's airtime.
    pub ack_timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chunk {
    pub offset: u32,
    pub len: u16,
    /// Ask the node for its next offset.
    pub ack: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadState {
    Sending,
    /// Every byte is acknowledged: finish the transfer.
    Complete,
    /// More than [`UPLOAD_RETRIES_MAX`] acknowledgement deadlines expired without progress.
    Stalled,
}

#[derive(Clone, Copy, Debug)]
pub struct Uploader {
    config: UploadConfig,
    /// The node's next offset as last reported.
    acked: u32,
    /// The next byte to send.
    next: u32,
    /// Chunks sent since the last acknowledgement request.
    since_ack: u16,
    ack_deadline_ms: Option<u64>,
    retries: u32,
}

impl Uploader {
    /// Start/resume from a caller-validated offset. Clamps it to `total`, chunk size to
    /// `1..=OTA_CHUNK_DATA_MAX` and window to at least 1; timeout is used unchanged.
    pub fn new(config: UploadConfig, from: u32) -> Self {
        let chunk_max = config.chunk_max.clamp(1, OTA_CHUNK_DATA_MAX as u16);
        Self {
            config: UploadConfig {
                chunk_max,
                window: config.window.max(1),
                ..config
            },
            acked: from.min(config.total),
            next: from.min(config.total),
            since_ack: 0,
            ack_deadline_ms: None,
            retries: 0,
        }
    }

    /// Current state; timeouts advance only when `next` or `next_with_ack` is called.
    /// Complete means the supplied acknowledgements cover `total`, not verified firmware.
    pub fn state(&self) -> UploadState {
        if self.acked >= self.config.total {
            UploadState::Complete
        } else if self.retries > UPLOAD_RETRIES_MAX {
            UploadState::Stalled
        } else {
            UploadState::Sending
        }
    }

    /// Highest supplied acknowledgement offset, clamped to `total`.
    pub fn acked(&self) -> u32 {
        self.acked
    }

    /// Reserve the next chunk if the window has room, advancing the cursor before actual I/O.
    /// Send the returned bytes before asking again; use gap/timeout handling after a failed send.
    pub fn next(&mut self, now_ms: u64) -> Option<Chunk> {
        self.next_with_ack(now_ms, false)
    }

    /// As [`Self::next`], with an optional forced acknowledgement request before a long wait.
    /// A returned chunk that requests an ack sets a deadline; this neither sends nor waits for it.
    pub fn next_with_ack(&mut self, now_ms: u64, force_ack: bool) -> Option<Chunk> {
        self.expire(now_ms);
        if self.state() != UploadState::Sending || self.next >= self.config.total {
            return None;
        }
        let in_flight = self.next - self.acked;
        let window_bytes = u32::from(self.config.window) * u32::from(self.config.chunk_max);
        if in_flight >= window_bytes {
            return None;
        }
        let len = (self.config.total - self.next).min(u32::from(self.config.chunk_max)) as u16;
        let offset = self.next;
        self.next += u32::from(len);
        self.since_ack += 1;
        // Ask twice per window so the next half goes out while the first is acknowledged, and
        // always on the last chunk and when the window fills.
        let ack = force_ack
            || self.since_ack >= self.config.window.div_ceil(2)
            || self.next == self.config.total
            || self.next - self.acked >= window_bytes;
        if ack {
            self.since_ack = 0;
            self.ack_deadline_ms = Some(now_ms.saturating_add(self.config.ack_timeout_ms));
        }
        Some(Chunk { offset, len, ack })
    }

    /// Apply a caller-validated acknowledgement. Advances up to `total`, even beyond bytes
    /// offered by `next`; bind it to this transfer before calling. Progress resets retries;
    /// older offsets do not move the cursor back. Outstanding bytes retain their deadline.
    pub fn on_ack(&mut self, next_offset: u32, now_ms: u64) {
        let next_offset = next_offset.min(self.config.total);
        if next_offset > self.acked {
            self.acked = next_offset;
            self.retries = 0;
        }
        self.next = self.next.max(self.acked);
        self.ack_deadline_ms = (self.next > self.acked).then(|| {
            self.ack_deadline_ms
                .unwrap_or(now_ms.saturating_add(self.config.ack_timeout_ms))
        });
    }

    /// Apply a caller-validated gap report and rewind offered bytes to at least `acked`.
    /// Older reports cannot reduce acknowledged progress; a rewind clears the current deadline.
    pub fn on_gap(&mut self, next_offset: u32, now_ms: u64) {
        self.on_ack(next_offset, now_ms);
        let next_offset = next_offset.min(self.config.total).max(self.acked);
        if next_offset < self.next {
            self.next = next_offset;
            self.since_ack = 0;
            self.ack_deadline_ms = None;
        }
    }

    /// When to poll [`Self::next`] again with nothing received: the acknowledgement deadline.
    pub fn wake_at(&self) -> Option<u64> {
        self.ack_deadline_ms
    }

    /// No acknowledgement in time: go back to the last confirmed offset.
    fn expire(&mut self, now_ms: u64) {
        if self
            .ack_deadline_ms
            .is_some_and(|deadline| now_ms >= deadline)
        {
            self.ack_deadline_ms = None;
            self.next = self.acked;
            self.since_ack = 0;
            self.retries += 1;
        }
    }
}

/// Token bucket for airtime explicitly charged by the caller. Starts with one hour's budget
/// and refills continuously; zero disables pacing. This is not a rolling-hour limiter or proof
/// of regulatory compliance. It does not include uncharged traffic or per-transmission limits.
#[derive(Clone, Copy, Debug)]
pub struct Pacer {
    budget_ms_per_hour: u64,
    /// Airtime available, scaled by the hour so refills stay exact.
    scaled: u64,
    updated_ms: u64,
}

const HOUR_MS: u64 = 3_600_000;

impl Pacer {
    /// Create a full bucket. `10` means 1%; callers must bound nonzero settings to at most 1000
    /// (100%) and supply monotonic time. The input range is not enforced here.
    pub fn new(duty_tenths_percent: u16, now_ms: u64) -> Self {
        let budget_ms_per_hour = HOUR_MS * u64::from(duty_tenths_percent) / 1_000;
        Self {
            budget_ms_per_hour,
            scaled: budget_ms_per_hour.saturating_mul(HOUR_MS),
            updated_ms: now_ms,
        }
    }

    pub fn unlimited(&self) -> bool {
        self.budget_ms_per_hour == 0
    }

    fn refill(&mut self, now_ms: u64) {
        let elapsed = now_ms.saturating_sub(self.updated_ms);
        self.updated_ms = now_ms;
        self.scaled = self
            .scaled
            .saturating_add(elapsed.saturating_mul(self.budget_ms_per_hour))
            .min(self.budget_ms_per_hour.saturating_mul(HOUR_MS));
    }

    /// Refill and estimate delay until this bucket can cover `airtime_ms`; zero if available.
    /// `None` means the request exceeds a full bucket. This reserves nothing; recheck after
    /// waiting and account for each send with `spend`. Disabled pacing always returns zero.
    pub fn delay_ms(&mut self, airtime_ms: u32, now_ms: u64) -> Option<u64> {
        if self.unlimited() {
            return Some(0);
        }
        if u64::from(airtime_ms) > self.budget_ms_per_hour {
            return None;
        }
        self.refill(now_ms);
        let price = u64::from(airtime_ms).saturating_mul(HOUR_MS);
        Some(
            price
                .saturating_sub(self.scaled)
                .div_ceil(self.budget_ms_per_hour),
        )
    }

    /// Charge airtime after checking `delay_ms`. This never refuses a send: an excessive charge
    /// empties the bucket without retaining debt, so callers must honor the availability check.
    pub fn spend(&mut self, airtime_ms: u32, now_ms: u64) {
        if self.unlimited() {
            return;
        }
        self.refill(now_ms);
        self.scaled = self
            .scaled
            .saturating_sub(u64::from(airtime_ms).saturating_mul(HOUR_MS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(total: u32, window: u16) -> UploadConfig {
        UploadConfig {
            total,
            chunk_max: 100,
            window,
            ack_timeout_ms: 1_000,
        }
    }

    fn drain(uploader: &mut Uploader, now_ms: u64) -> std::vec::Vec<Chunk> {
        core::iter::from_fn(|| uploader.next(now_ms)).collect()
    }

    #[test]
    fn a_window_asks_twice_and_slides_on_each_acknowledgement() {
        let mut uploader = Uploader::new(config(1_050, 4), 0);
        let first = drain(&mut uploader, 0);
        assert_eq!(first.len(), 4);
        assert_eq!(
            first
                .iter()
                .map(|chunk| chunk.ack)
                .collect::<std::vec::Vec<_>>(),
            [false, true, false, true]
        );
        // The first half's acknowledgement opens room for two more; nothing is resent.
        uploader.on_ack(200, 10);
        let more = drain(&mut uploader, 10);
        assert_eq!(
            more.iter()
                .map(|chunk| chunk.offset)
                .collect::<std::vec::Vec<_>>(),
            [400, 500]
        );
        uploader.on_ack(600, 20);
        assert_eq!(drain(&mut uploader, 20).len(), 4);
        uploader.on_ack(1_000, 25);
        let last = drain(&mut uploader, 25);
        assert_eq!((last.len(), last[0].len, last[0].ack), (1, 50, true));
        uploader.on_ack(1_050, 30);
        assert_eq!(uploader.state(), UploadState::Complete);
        assert!(uploader.next(30).is_none());
        assert_eq!(uploader.wake_at(), None);
    }

    #[test]
    fn gaps_and_silence_go_back_to_the_node_offset() {
        let mut uploader = Uploader::new(config(1_000, 4), 300);
        let sent = drain(&mut uploader, 0);
        assert_eq!(sent[0].offset, 300);
        // The node lost the chunk at 400: everything from there goes again.
        uploader.on_gap(400, 5);
        assert_eq!(uploader.next(5).unwrap().offset, 400);
        // No answer at all: after the deadline the window restarts at the last confirmed byte.
        drain(&mut uploader, 5);
        assert!(uploader.next(500).is_none());
        assert_eq!(uploader.wake_at(), Some(1_005));
        assert_eq!(uploader.next(1_005).unwrap().offset, 400);
        let mut timeouts = 1;
        while uploader.state() == UploadState::Sending {
            drain(&mut uploader, 0);
            let deadline = uploader.wake_at().unwrap();
            uploader.next(deadline);
            timeouts += 1;
        }
        assert_eq!(uploader.state(), UploadState::Stalled);
        assert_eq!(timeouts, UPLOAD_RETRIES_MAX + 1);
        assert_eq!(uploader.acked(), 400);
    }

    #[test]
    fn pacing_keeps_an_hourly_duty_cycle_and_refuses_what_never_fits() {
        // 1 % of an hour: 36 s of airtime.
        let mut pacer = Pacer::new(10, 0);
        assert_eq!(pacer.delay_ms(30_000, 0), Some(0));
        pacer.spend(30_000, 0);
        // 6 s left; a 12 s transmission waits for 6 s more to refill at 1 %: 600 s.
        assert_eq!(pacer.delay_ms(12_000, 0), Some(600_000));
        assert_eq!(pacer.delay_ms(12_000, 600_000), Some(0));
        assert_eq!(pacer.delay_ms(40_000, 0), None);
        let mut free = Pacer::new(0, 0);
        assert!(free.unlimited());
        assert_eq!(free.delay_ms(u32::MAX, 0), Some(0));
    }
}
