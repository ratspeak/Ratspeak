//! Session-local ownership for Ratspeak presence announcements.
//!
//! A presence burst may contain multiple application destinations (LXMF
//! delivery, optional propagation, and optional LXST telephony). Coalescing
//! therefore happens here, before those components are built or queued.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::activity::CorrelationId;

const RECENT_BURST_WINDOW: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AnnounceOrigin {
    Manual,
    Startup,
    Periodic,
    InterfaceOnline,
    Opportunistic,
    IdentityChanged,
    ProfileChanged,
    PropagationChanged,
}

impl AnnounceOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Startup => "startup",
            Self::Periodic => "periodic",
            Self::InterfaceOnline => "interface_online",
            Self::Opportunistic => "opportunistic",
            Self::IdentityChanged => "identity_changed",
            Self::ProfileChanged => "profile_changed",
            Self::PropagationChanged => "propagation_changed",
        }
    }

    /// One-shot semantic triggers must survive a covered lead that later
    /// fails. Manual/repeating triggers can report that terminal failure to
    /// their caller or be submitted again by their normal scheduler.
    const fn retries_after_covered_failure(self) -> bool {
        matches!(
            self,
            Self::InterfaceOnline
                | Self::IdentityChanged
                | Self::ProfileChanged
                | Self::PropagationChanged
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AnnounceSemanticRevision {
    pub identity: u64,
    pub content: u64,
    pub interface: u64,
}

impl AnnounceSemanticRevision {
    fn covers(self, other: Self) -> bool {
        self.identity == other.identity
            && self.content >= other.content
            && self.interface >= other.interface
    }

    fn merge(self, other: Self) -> Self {
        if self.identity != other.identity {
            return if other.identity > self.identity {
                other
            } else {
                self
            };
        }
        Self {
            identity: self.identity,
            content: self.content.max(other.content),
            interface: self.interface.max(other.interface),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnounceIntent {
    pub origin: AnnounceOrigin,
    pub revisions: AnnounceSemanticRevision,
}

#[derive(Clone, Eq, PartialEq)]
pub struct AnnounceLeadership {
    pub correlation_id: u64,
    pub activity_correlation_id: CorrelationId,
    pub revisions: AnnounceSemanticRevision,
    pub origins: Vec<AnnounceOrigin>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnounceAdmission {
    Lead { correlation_id: u64 },
    AlreadyQueued { correlation_id: u64 },
    Deferred { correlation_id: u64 },
}

#[derive(Clone)]
struct PendingBurst {
    correlation_id: u64,
    activity_correlation_id: CorrelationId,
    revisions: AnnounceSemanticRevision,
    origins: HashSet<AnnounceOrigin>,
}

impl PendingBurst {
    fn leadership(&self) -> AnnounceLeadership {
        let mut origins: Vec<_> = self.origins.iter().copied().collect();
        origins.sort_by_key(|origin| origin.as_str());
        AnnounceLeadership {
            correlation_id: self.correlation_id,
            activity_correlation_id: self.activity_correlation_id,
            revisions: self.revisions,
            origins,
        }
    }

    fn merge(&mut self, intent: AnnounceIntent) {
        self.revisions = self.revisions.merge(intent.revisions);
        self.origins.insert(intent.origin);
    }

    fn merge_burst(&mut self, other: PendingBurst) {
        self.revisions = self.revisions.merge(other.revisions);
        self.origins.extend(other.origins);
    }
}

#[derive(Clone)]
struct RecentBurst {
    correlation_id: u64,
    revisions: AnnounceSemanticRevision,
    completed_at: Instant,
}

#[derive(Default)]
pub struct AnnounceCoordinator {
    next_correlation_id: u64,
    in_flight: Option<PendingBurst>,
    follow_up: Option<PendingBurst>,
    retry_after_failure: Option<PendingBurst>,
    recent: Option<RecentBurst>,
}

impl AnnounceCoordinator {
    fn next_correlation_id(&mut self) -> u64 {
        self.next_correlation_id = self.next_correlation_id.wrapping_add(1).max(1);
        self.next_correlation_id
    }

    fn new_burst(&mut self, intent: AnnounceIntent) -> PendingBurst {
        PendingBurst {
            correlation_id: self.next_correlation_id(),
            activity_correlation_id: CorrelationId::random(),
            revisions: intent.revisions,
            origins: HashSet::from([intent.origin]),
        }
    }

    pub fn admit(&mut self, intent: AnnounceIntent, now: Instant) -> AnnounceAdmission {
        if self
            .in_flight
            .as_ref()
            .is_some_and(|burst| burst.revisions.covers(intent.revisions))
        {
            let correlation_id = self
                .in_flight
                .as_ref()
                .expect("covered in-flight burst checked above")
                .correlation_id;
            self.in_flight
                .as_mut()
                .expect("covered in-flight burst checked above")
                .origins
                .insert(intent.origin);
            if intent.origin.retries_after_covered_failure() {
                if let Some(retry) = self.retry_after_failure.as_mut() {
                    retry.merge(intent);
                } else {
                    let retry = self.new_burst(intent);
                    self.retry_after_failure = Some(retry);
                }
            }
            return AnnounceAdmission::AlreadyQueued { correlation_id };
        }

        if self.in_flight.is_some() {
            if let Some(follow_up) = self.follow_up.as_mut() {
                follow_up.merge(intent);
            } else {
                let follow_up = self.new_burst(intent);
                self.follow_up = Some(follow_up);
            }
            return AnnounceAdmission::Deferred {
                correlation_id: self
                    .follow_up
                    .as_ref()
                    .expect("follow-up installed above")
                    .correlation_id,
            };
        }

        if let Some(recent) = self.recent.as_ref() {
            let revisions_covered = recent.revisions.covers(intent.revisions);
            let within_repeat_window =
                now.saturating_duration_since(recent.completed_at) < RECENT_BURST_WINDOW;
            // Interface-online work is semantic, not periodic. A successful
            // burst that already covered this exact interface revision remains
            // authoritative even if a delayed stats poll submits the automatic
            // intent after the short repeat-tap window has elapsed.
            if revisions_covered
                && (within_repeat_window || intent.origin == AnnounceOrigin::InterfaceOnline)
            {
                return AnnounceAdmission::AlreadyQueued {
                    correlation_id: recent.correlation_id,
                };
            }
        }

        let burst = self.new_burst(intent);
        let correlation_id = burst.correlation_id;
        self.in_flight = Some(burst);
        AnnounceAdmission::Lead { correlation_id }
    }

    pub fn leadership(&self, correlation_id: u64) -> Option<AnnounceLeadership> {
        self.in_flight
            .as_ref()
            .filter(|burst| burst.correlation_id == correlation_id)
            .map(PendingBurst::leadership)
    }

    /// Complete the exact current burst and atomically promote at most one
    /// semantic follow-up. Failed bursts are not entered into the recent
    /// success window.
    pub fn finish(
        &mut self,
        correlation_id: u64,
        success: bool,
        now: Instant,
    ) -> Option<AnnounceLeadership> {
        let completed = self
            .in_flight
            .take()
            .filter(|burst| burst.correlation_id == correlation_id)?;
        if success {
            self.recent = Some(RecentBurst {
                correlation_id,
                revisions: completed.revisions,
                completed_at: now,
            });
            self.retry_after_failure = None;
        } else if let Some(retry) = self.retry_after_failure.take() {
            if let Some(follow_up) = self.follow_up.as_mut() {
                follow_up.merge_burst(retry);
            } else {
                self.follow_up = Some(retry);
            }
        }
        if let Some(follow_up) = self.follow_up.take() {
            let leadership = follow_up.leadership();
            self.in_flight = Some(follow_up);
            Some(leadership)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(origin: AnnounceOrigin, content: u64, interface: u64) -> AnnounceIntent {
        AnnounceIntent {
            origin,
            revisions: AnnounceSemanticRevision {
                identity: 7,
                content,
                interface,
            },
        }
    }

    #[test]
    fn rapid_unchanged_requests_share_one_presence_burst() {
        let now = Instant::now();
        let mut coordinator = AnnounceCoordinator::default();
        let AnnounceAdmission::Lead { correlation_id } =
            coordinator.admit(intent(AnnounceOrigin::Manual, 2, 4), now)
        else {
            panic!("first request must lead");
        };
        for origin in [
            AnnounceOrigin::Manual,
            AnnounceOrigin::Periodic,
            AnnounceOrigin::Startup,
            AnnounceOrigin::Opportunistic,
        ] {
            assert_eq!(
                coordinator.admit(intent(origin, 2, 4), now),
                AnnounceAdmission::AlreadyQueued { correlation_id }
            );
        }
        assert!(coordinator.finish(correlation_id, true, now).is_none());
        assert_eq!(
            coordinator.admit(intent(AnnounceOrigin::Manual, 2, 4), now),
            AnnounceAdmission::AlreadyQueued { correlation_id }
        );
    }

    #[test]
    fn newer_semantics_create_exactly_one_follow_up() {
        let now = Instant::now();
        let mut coordinator = AnnounceCoordinator::default();
        let AnnounceAdmission::Lead { correlation_id } =
            coordinator.admit(intent(AnnounceOrigin::Manual, 1, 1), now)
        else {
            panic!("first request must lead");
        };
        let first_follow_up = coordinator.admit(intent(AnnounceOrigin::IdentityChanged, 2, 1), now);
        let second_follow_up =
            coordinator.admit(intent(AnnounceOrigin::InterfaceOnline, 2, 3), now);
        assert!(matches!(
            first_follow_up,
            AnnounceAdmission::Deferred { .. }
        ));
        assert!(matches!(
            second_follow_up,
            AnnounceAdmission::Deferred { .. }
        ));

        let promoted = coordinator
            .finish(correlation_id, true, now)
            .expect("new semantics require one follow-up");
        assert_eq!(promoted.revisions.content, 2);
        assert_eq!(promoted.revisions.interface, 3);
        assert!(promoted.origins.contains(&AnnounceOrigin::IdentityChanged));
        assert!(promoted.origins.contains(&AnnounceOrigin::InterfaceOnline));
        assert!(
            coordinator
                .finish(promoted.correlation_id, true, now)
                .is_none()
        );
    }

    #[test]
    fn failed_burst_is_not_recent_success() {
        let now = Instant::now();
        let mut coordinator = AnnounceCoordinator::default();
        let AnnounceAdmission::Lead { correlation_id } =
            coordinator.admit(intent(AnnounceOrigin::Manual, 1, 1), now)
        else {
            panic!("first request must lead");
        };
        coordinator.finish(correlation_id, false, now);
        assert!(matches!(
            coordinator.admit(intent(AnnounceOrigin::Manual, 1, 1), now),
            AnnounceAdmission::Lead { .. }
        ));
    }

    #[test]
    fn unaccepted_delivery_build_cannot_cover_delayed_interface_intent() {
        let now = Instant::now();
        let mut coordinator = AnnounceCoordinator::default();
        let AnnounceAdmission::Lead { correlation_id } =
            coordinator.admit(intent(AnnounceOrigin::Manual, 2, 4), now)
        else {
            panic!("manual request must lead");
        };
        // Models ratchet construction followed by stale/offline/channel
        // rejection: without actual transport acceptance this is terminally
        // unsuccessful and must not create semantic coverage.
        assert!(coordinator.finish(correlation_id, false, now).is_none());
        assert!(matches!(
            coordinator.admit(
                intent(AnnounceOrigin::InterfaceOnline, 2, 4),
                now + RECENT_BURST_WINDOW + Duration::from_secs(7),
            ),
            AnnounceAdmission::Lead { .. }
        ));
    }

    #[test]
    fn covered_interface_intent_is_retried_once_when_lead_fails() {
        let now = Instant::now();
        let mut coordinator = AnnounceCoordinator::default();
        let AnnounceAdmission::Lead { correlation_id } =
            coordinator.admit(intent(AnnounceOrigin::Manual, 2, 4), now)
        else {
            panic!("manual request must lead");
        };
        assert_eq!(
            coordinator.admit(intent(AnnounceOrigin::InterfaceOnline, 2, 4), now),
            AnnounceAdmission::AlreadyQueued { correlation_id }
        );

        let retry = coordinator
            .finish(correlation_id, false, now)
            .expect("covered one-shot interface intent must survive lead failure");
        assert_ne!(retry.correlation_id, correlation_id);
        assert_eq!(
            retry.revisions,
            intent(AnnounceOrigin::Manual, 2, 4).revisions
        );
        assert_eq!(retry.origins, vec![AnnounceOrigin::InterfaceOnline]);
        assert!(
            coordinator
                .finish(retry.correlation_id, true, now)
                .is_none()
        );
    }

    #[test]
    fn covered_interface_intent_is_discarded_when_lead_succeeds() {
        let now = Instant::now();
        let mut coordinator = AnnounceCoordinator::default();
        let AnnounceAdmission::Lead { correlation_id } =
            coordinator.admit(intent(AnnounceOrigin::Manual, 2, 4), now)
        else {
            panic!("manual request must lead");
        };
        assert_eq!(
            coordinator.admit(intent(AnnounceOrigin::InterfaceOnline, 2, 4), now),
            AnnounceAdmission::AlreadyQueued { correlation_id }
        );
        assert!(coordinator.finish(correlation_id, true, now).is_none());
    }

    #[test]
    fn delayed_interface_intent_is_covered_when_manual_sampled_its_ready_revision() {
        let now = Instant::now();
        let mut coordinator = AnnounceCoordinator::default();
        let AnnounceAdmission::Lead { correlation_id } =
            coordinator.admit(intent(AnnounceOrigin::Manual, 2, 4), now)
        else {
            panic!("manual request must lead");
        };
        assert!(coordinator.finish(correlation_id, true, now).is_none());

        assert_eq!(
            coordinator.admit(
                intent(AnnounceOrigin::InterfaceOnline, 2, 4),
                now + RECENT_BURST_WINDOW + Duration::from_secs(7),
            ),
            AnnounceAdmission::AlreadyQueued { correlation_id }
        );
    }

    #[test]
    fn genuinely_uncovered_interface_revision_gets_one_follow_up() {
        let now = Instant::now();
        let mut coordinator = AnnounceCoordinator::default();
        let AnnounceAdmission::Lead { correlation_id } =
            coordinator.admit(intent(AnnounceOrigin::Manual, 2, 4), now)
        else {
            panic!("manual request must lead");
        };
        let AnnounceAdmission::Deferred {
            correlation_id: follow_up_id,
        } = coordinator.admit(intent(AnnounceOrigin::InterfaceOnline, 2, 5), now)
        else {
            panic!("new interface revision must defer exactly one follow-up");
        };

        let promoted = coordinator
            .finish(correlation_id, true, now)
            .expect("uncovered interface must be promoted");
        assert_eq!(promoted.correlation_id, follow_up_id);
        assert_eq!(promoted.revisions.interface, 5);
        assert!(coordinator.finish(follow_up_id, true, now).is_none());
    }
}
