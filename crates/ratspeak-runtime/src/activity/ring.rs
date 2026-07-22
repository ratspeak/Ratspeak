//! Count- and byte-bounded in-memory Activity ring.

#![allow(
    dead_code,
    reason = "some bounded-ring inspection is currently exercised only by tests"
)]

use std::collections::VecDeque;
use std::mem;

use super::pseudonym::StoredEventV1;
use super::schema::ActivityEventV1;

pub const MOBILE_RING_MAX_EVENTS: usize = 2_000;
pub const MOBILE_RING_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const DESKTOP_RING_MAX_EVENTS: usize = 5_000;
pub const DESKTOP_RING_MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RingLimits {
    max_events: usize,
    max_bytes: usize,
}

impl RingLimits {
    pub(crate) fn new(max_events: usize, max_bytes: usize) -> Result<Self, RingError> {
        if max_events == 0 || max_bytes == 0 {
            return Err(RingError::InvalidLimits);
        }
        Ok(Self {
            max_events,
            max_bytes,
        })
    }

    pub(crate) fn platform_default() -> Self {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        let limits = Self {
            max_events: MOBILE_RING_MAX_EVENTS,
            max_bytes: MOBILE_RING_MAX_BYTES,
        };
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        let limits = Self {
            max_events: DESKTOP_RING_MAX_EVENTS,
            max_bytes: DESKTOP_RING_MAX_BYTES,
        };
        limits
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RingError {
    EventExceedsByteLimit,
    InvalidLimits,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RingPush {
    pub(crate) evicted_for_count_events: u64,
    pub(crate) evicted_for_count_bytes: u64,
    pub(crate) evicted_for_byte_limit_events: u64,
    pub(crate) evicted_for_byte_limit_bytes: u64,
}

pub(crate) struct ActivityRing {
    entries: VecDeque<StoredEventV1>,
    charged_bytes: usize,
    backing_bytes: usize,
    limits: RingLimits,
}

impl ActivityRing {
    pub(crate) fn new(limits: RingLimits) -> Result<Self, RingError> {
        // Preallocate the exact count ceiling so the queue never grows an
        // uncharged spare allocation while capture is running.
        let entries = VecDeque::with_capacity(limits.max_events);
        let backing_bytes = entries
            .capacity()
            .saturating_mul(mem::size_of::<StoredEventV1>());
        if backing_bytes > limits.max_bytes {
            return Err(RingError::InvalidLimits);
        }
        Ok(Self {
            entries,
            charged_bytes: backing_bytes,
            backing_bytes,
            limits,
        })
    }

    pub(crate) fn platform_default() -> Result<Self, RingError> {
        Self::new(RingLimits::platform_default())
    }

    pub(crate) fn push(&mut self, event: StoredEventV1) -> Result<RingPush, RingError> {
        let event_bytes = event.charged_bytes();
        if self.backing_bytes.saturating_add(event_bytes) > self.limits.max_bytes {
            return Err(RingError::EventExceedsByteLimit);
        }

        let mut effect = RingPush::default();
        while self.entries.len() >= self.limits.max_events {
            if let Some(evicted) = self.pop_front() {
                effect.evicted_for_count_events = effect.evicted_for_count_events.saturating_add(1);
                effect.evicted_for_count_bytes = effect
                    .evicted_for_count_bytes
                    .saturating_add(evicted as u64);
            }
        }
        while self.charged_bytes.saturating_add(event_bytes) > self.limits.max_bytes {
            if let Some(evicted) = self.pop_front() {
                effect.evicted_for_byte_limit_events =
                    effect.evicted_for_byte_limit_events.saturating_add(1);
                effect.evicted_for_byte_limit_bytes = effect
                    .evicted_for_byte_limit_bytes
                    .saturating_add(evicted as u64);
            } else {
                return Err(RingError::EventExceedsByteLimit);
            }
        }

        self.charged_bytes = self.charged_bytes.saturating_add(event_bytes);
        self.entries.push_back(event);
        Ok(effect)
    }

    fn pop_front(&mut self) -> Option<usize> {
        self.entries.pop_front().map(|event| {
            let bytes = event.charged_bytes();
            self.charged_bytes = self.charged_bytes.saturating_sub(bytes);
            bytes
        })
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.charged_bytes = self.backing_bytes;
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn charged_bytes(&self) -> usize {
        self.charged_bytes
    }

    pub(crate) fn oldest_sequence(&self) -> Option<u64> {
        self.entries.front().map(StoredEventV1::sequence)
    }

    pub(crate) fn latest_sequence(&self) -> Option<u64> {
        self.entries.back().map(StoredEventV1::sequence)
    }

    /// Masked copy only. No stored/raw handle can escape the ring.
    pub(crate) fn snapshot(&self) -> Vec<ActivityEventV1> {
        self.entries.iter().map(StoredEventV1::masked).collect()
    }

    /// Builds a masked replay page without ever cloning raw vault fields.
    /// The caller supplies already-clamped limits; an individual event is
    /// always permitted because the schema cap is lower than replay's minimum
    /// byte budget.
    pub(crate) fn snapshot_after(
        &self,
        after: Option<u64>,
        max_events: usize,
        max_bytes: usize,
    ) -> Vec<ActivityEventV1> {
        let mut events = Vec::with_capacity(max_events.min(self.entries.len()));
        let mut encoded_bytes = 2usize; // JSON array delimiters.
        for stored in self
            .entries
            .iter()
            .filter(|event| after.is_none_or(|cursor| event.sequence() > cursor))
        {
            if events.len() >= max_events {
                break;
            }
            let event = stored.masked();
            let Ok(event_bytes) = serde_json::to_vec(&event).map(|value| value.len()) else {
                break;
            };
            let separator = usize::from(!events.is_empty());
            if !events.is_empty()
                && encoded_bytes
                    .saturating_add(separator)
                    .saturating_add(event_bytes)
                    > max_bytes
            {
                break;
            }
            encoded_bytes = encoded_bytes
                .saturating_add(separator)
                .saturating_add(event_bytes);
            events.push(event);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalog;
    use super::super::classified::{CoalescingPolicy, DraftContext};
    use super::super::coalesce::{CoalesceOutput, PreflushCoalescer};
    use super::super::pseudonym::CapturePrivacy;
    use super::super::schema::CaptureProfile;
    use super::*;

    fn backing_bytes(max_events: usize) -> usize {
        VecDeque::<StoredEventV1>::with_capacity(max_events)
            .capacity()
            .saturating_mul(mem::size_of::<StoredEventV1>())
    }

    fn stored(privacy: &mut CapturePrivacy, sequence: u64, destination: u8) -> StoredEventV1 {
        stored_with_endpoint(privacy, sequence, destination, "host.example:4242")
    }

    fn stored_with_endpoint(
        privacy: &mut CapturePrivacy,
        sequence: u64,
        destination: u8,
        endpoint: &str,
    ) -> StoredEventV1 {
        stored_with_endpoint_and_profile(
            privacy,
            sequence,
            destination,
            endpoint,
            CaptureProfile::Normal,
        )
    }

    fn stored_with_endpoint_and_profile(
        privacy: &mut CapturePrivacy,
        sequence: u64,
        destination: u8,
        endpoint: &str,
        capture_profile: CaptureProfile,
    ) -> StoredEventV1 {
        let draft = catalog::test_network_event(
            sequence,
            sequence,
            [destination; 16],
            endpoint,
            CoalescingPolicy::Never,
        )
        .unwrap();
        let validated = draft
            .validate(DraftContext {
                capture_session: privacy.capture_session().to_string(),
                capture_generation: 1,
                capture_profile,
            })
            .unwrap();
        let mut coalescer = PreflushCoalescer::default();
        let CoalesceOutput::One(ready) = coalescer.push(validated) else {
            panic!("non-coalescing test event should be immediately ready");
        };
        privacy.seal(ready, sequence).unwrap()
    }

    #[test]
    fn count_pressure_evicts_oldest_with_one_deterministic_cause() {
        let mut privacy = CapturePrivacy::random();
        let sample = stored(&mut privacy, 1, 1);
        let event_bytes = sample.charged_bytes();
        let max_bytes = backing_bytes(2).saturating_add(event_bytes * 4);
        let mut ring = ActivityRing::new(RingLimits::new(2, max_bytes).unwrap()).unwrap();
        ring.push(sample).unwrap();
        ring.push(stored(&mut privacy, 2, 2)).unwrap();
        let effect = ring.push(stored(&mut privacy, 3, 3)).unwrap();

        assert_eq!(effect.evicted_for_count_events, 1);
        assert_eq!(effect.evicted_for_count_bytes, event_bytes as u64);
        assert_eq!(effect.evicted_for_byte_limit_events, 0);
        assert_eq!(ring.oldest_sequence(), Some(2));
        assert_eq!(ring.latest_sequence(), Some(3));
        assert_eq!(ring.len(), 2);
    }

    #[test]
    fn byte_pressure_is_accounted_separately_after_count_pressure() {
        let mut privacy = CapturePrivacy::random();
        let first = stored(&mut privacy, 1, 1);
        let second = stored(&mut privacy, 2, 2);
        let third = stored(&mut privacy, 3, 3);
        let max_bytes = backing_bytes(10)
            .saturating_add(second.charged_bytes())
            .saturating_add(third.charged_bytes());
        let first_bytes = first.charged_bytes();
        let mut ring = ActivityRing::new(RingLimits::new(10, max_bytes).unwrap()).unwrap();
        ring.push(first).unwrap();
        ring.push(second).unwrap();
        let effect = ring.push(third).unwrap();

        assert_eq!(effect.evicted_for_count_events, 0);
        assert_eq!(effect.evicted_for_byte_limit_events, 1);
        assert_eq!(effect.evicted_for_byte_limit_bytes, first_bytes as u64);
        assert_eq!(ring.oldest_sequence(), Some(2));
        assert!(ring.charged_bytes() <= max_bytes);
    }

    #[test]
    fn oversized_insert_is_rejected_without_mutating_the_ring() {
        let mut privacy = CapturePrivacy::random();
        let first = stored(&mut privacy, 1, 1);
        let first_limit = backing_bytes(10).saturating_add(first.charged_bytes());
        let mut ring = ActivityRing::new(RingLimits::new(10, first_limit).unwrap()).unwrap();
        ring.push(first).unwrap();
        let before_bytes = ring.charged_bytes();
        let label = "a".repeat(60);
        let long_endpoint = format!("{label}.{label}.{label}.{label}:1");
        let larger = stored_with_endpoint_and_profile(
            &mut privacy,
            2,
            2,
            &long_endpoint,
            CaptureProfile::Trace,
        );

        assert_eq!(ring.push(larger), Err(RingError::EventExceedsByteLimit));
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.oldest_sequence(), Some(1));
        assert_eq!(ring.charged_bytes(), before_bytes);
    }

    #[test]
    fn snapshot_is_masked_and_clear_releases_every_entry() {
        let mut privacy = CapturePrivacy::random();
        let mut ring = ActivityRing::new(RingLimits::new(10, 64 * 1024).unwrap()).unwrap();
        ring.push(stored(&mut privacy, 1, 0xab)).unwrap();

        let serialized = serde_json::to_string(&ring.snapshot()).unwrap();
        assert!(!serialized.contains(&hex::encode([0xab; 16])));
        assert!(!serialized.contains("host.example:4242"));
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.charged_bytes(), backing_bytes(10));
        assert!(ring.snapshot().is_empty());
    }

    #[test]
    fn invalid_zero_limits_are_rejected() {
        assert_eq!(RingLimits::new(0, 1), Err(RingError::InvalidLimits));
        assert_eq!(RingLimits::new(1, 0), Err(RingError::InvalidLimits));
        let limits = RingLimits::new(100, 1).unwrap();
        assert!(matches!(
            ActivityRing::new(limits),
            Err(RingError::InvalidLimits)
        ));
        assert!(ActivityRing::platform_default().is_ok());
    }
}
