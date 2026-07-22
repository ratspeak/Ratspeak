//! Adjacent-only coalescing before sequence allocation or IPC flush.

#![allow(
    dead_code,
    reason = "Stage 1A defines preflush coalescing; Stage 1B wires its worker"
)]

use super::classified::{ReadyDraft, ValidatedDraft};

pub(crate) enum CoalesceOutput {
    Held,
    Merged { absorbed: u32 },
    One(ReadyDraft),
    Two(ReadyDraft, ReadyDraft),
}

#[derive(Default)]
pub(crate) struct PreflushCoalescer {
    pending: Option<ValidatedDraft>,
}

impl PreflushCoalescer {
    pub(crate) fn push(&mut self, draft: ValidatedDraft) -> CoalesceOutput {
        let Some(mut pending) = self.pending.take() else {
            if draft.can_coalesce_with(&draft) {
                self.pending = Some(draft);
                return CoalesceOutput::Held;
            }
            return CoalesceOutput::One(ReadyDraft(draft));
        };

        if pending.can_coalesce_with(&draft) {
            let absorbed = draft.count;
            pending.absorb(draft);
            self.pending = Some(pending);
            return CoalesceOutput::Merged { absorbed };
        }

        if draft.can_coalesce_with(&draft) {
            self.pending = Some(draft);
            CoalesceOutput::One(ReadyDraft(pending))
        } else {
            CoalesceOutput::Two(ReadyDraft(pending), ReadyDraft(draft))
        }
    }

    pub(crate) fn flush(&mut self) -> Option<ReadyDraft> {
        self.pending.take().map(ReadyDraft)
    }

    pub(crate) fn clear(&mut self) {
        self.pending = None;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalog;
    use super::super::classified::{ActivityDraft, CoalescingPolicy, CorrelationId, DraftContext};
    use super::super::schema::{
        ActivityDirection, ActivityOutcome, ActivitySeverity, CaptureProfile, kinds,
    };
    use super::*;

    fn validated(timestamp: u64, destination: u8, profile: CaptureProfile) -> ValidatedDraft {
        catalog::test_network_event(
            timestamp,
            timestamp,
            [destination; 16],
            "host:4242",
            CoalescingPolicy::AdjacentEquivalent,
        )
        .unwrap()
        .validate(DraftContext {
            capture_session: "11".repeat(16),
            capture_generation: 1,
            capture_profile: profile,
        })
        .unwrap()
    }

    fn failure(timestamp: u64) -> ValidatedDraft {
        ActivityDraft::new(
            kinds::LXMF_DELIVERY_FAILED,
            ActivitySeverity::Error,
            ActivityDirection::Outbound,
            ActivityOutcome::Failed,
            timestamp,
            timestamp,
            CoalescingPolicy::Never,
        )
        .with_correlation(CorrelationId::from_bytes([8; 16]))
        .validate(DraftContext {
            capture_session: "11".repeat(16),
            capture_generation: 1,
            capture_profile: CaptureProfile::Normal,
        })
        .unwrap()
    }

    #[test]
    fn only_adjacent_equivalent_drafts_merge_before_sequence_allocation() {
        let mut coalescer = PreflushCoalescer::default();
        assert!(matches!(
            coalescer.push(validated(100, 1, CaptureProfile::Normal)),
            CoalesceOutput::Held
        ));
        assert!(matches!(
            coalescer.push(validated(90, 1, CaptureProfile::Normal)),
            CoalesceOutput::Merged { absorbed: 1 }
        ));
        let ready = coalescer.flush().unwrap().0;
        assert_eq!(ready.count, 2);
        assert_eq!(ready.timestamp_unix_ms, 100);
        assert_eq!(ready.first_timestamp_ms, Some(100));
        assert_eq!(
            ready.last_timestamp_ms,
            Some(90),
            "last means last observed, not wall-clock maximum"
        );
    }

    #[test]
    fn an_intervening_event_breaks_equivalence() {
        let mut coalescer = PreflushCoalescer::default();
        assert!(matches!(
            coalescer.push(validated(1, 1, CaptureProfile::Normal)),
            CoalesceOutput::Held
        ));
        let output = coalescer.push(validated(2, 2, CaptureProfile::Normal));
        let CoalesceOutput::One(first) = output else {
            panic!("first adjacent event should be released");
        };
        assert_eq!(first.0.timestamp_unix_ms, 1);
        assert_eq!(coalescer.flush().unwrap().0.timestamp_unix_ms, 2);
    }

    #[test]
    fn failures_are_never_delayed_or_coalesced() {
        let mut coalescer = PreflushCoalescer::default();
        assert!(matches!(
            coalescer.push(validated(1, 1, CaptureProfile::Normal)),
            CoalesceOutput::Held
        ));
        let CoalesceOutput::Two(first, error) = coalescer.push(failure(2)) else {
            panic!("pending normal event and failure must both be released in order");
        };
        assert_eq!(first.0.timestamp_unix_ms, 1);
        assert_eq!(error.0.timestamp_unix_ms, 2);
        assert!(coalescer.is_empty());

        assert!(matches!(coalescer.push(failure(3)), CoalesceOutput::One(_)));
        assert!(coalescer.is_empty());
    }

    #[test]
    fn capture_profile_is_part_of_the_equivalence_key() {
        let mut coalescer = PreflushCoalescer::default();
        assert!(matches!(
            coalescer.push(validated(1, 1, CaptureProfile::Normal)),
            CoalesceOutput::Held
        ));
        assert!(matches!(
            coalescer.push(validated(2, 1, CaptureProfile::Trace)),
            CoalesceOutput::One(_)
        ));
        assert_eq!(
            coalescer.flush().unwrap().0.context.capture_profile,
            CaptureProfile::Trace
        );
    }

    #[test]
    fn clear_drops_the_pending_raw_bearing_draft() {
        let mut coalescer = PreflushCoalescer::default();
        assert!(matches!(
            coalescer.push(validated(1, 1, CaptureProfile::Normal)),
            CoalesceOutput::Held
        ));
        coalescer.clear();
        assert!(coalescer.is_empty());
        assert!(coalescer.flush().is_none());
    }
}
