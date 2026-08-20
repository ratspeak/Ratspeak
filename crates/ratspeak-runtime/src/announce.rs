//! Session-local ownership for Ratspeak presence announcements.
//!
//! A presence burst may contain multiple application destinations (LXMF
//! delivery, optional propagation, and optional LXST telephony). Coalescing
//! therefore happens here, before those components are built or queued.

use std::collections::HashSet;
use std::time::{Duration, Instant};

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnounceLeadership {
    pub correlation_id: u64,
    pub revisions: AnnounceSemanticRevision,
    pub origins: Vec<AnnounceOrigin>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnounceAdmission {
    Lead { correlation_id: u64 },
    AlreadyQueued { correlation_id: u64 },
    Deferred { correlation_id: u64 },
}

#[derive(Clone, Debug)]
struct PendingBurst {
    correlation_id: u64,
    revisions: AnnounceSemanticRevision,
    origins: HashSet<AnnounceOrigin>,
}

impl PendingBurst {
    fn leadership(&self) -> AnnounceLeadership {
        let mut origins: Vec<_> = self.origins.iter().copied().collect();
        origins.sort_by_key(|origin| origin.as_str());
        AnnounceLeadership {
            correlation_id: self.correlation_id,
            revisions: self.revisions,
            origins,
        }
    }

    fn merge(&mut self, intent: AnnounceIntent) {
        self.revisions = self.revisions.merge(intent.revisions);
        self.origins.insert(intent.origin);
    }
}

#[derive(Clone, Debug)]
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
            revisions: intent.revisions,
            origins: HashSet::from([intent.origin]),
        }
    }

    pub fn admit(&mut self, intent: AnnounceIntent, now: Instant) -> AnnounceAdmission {
        if let Some(in_flight) = self.in_flight.as_mut() {
            if in_flight.revisions.covers(intent.revisions) {
                in_flight.origins.insert(intent.origin);
                return AnnounceAdmission::AlreadyQueued {
                    correlation_id: in_flight.correlation_id,
                };
            }

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
            if now.saturating_duration_since(recent.completed_at) < RECENT_BURST_WINDOW
                && recent.revisions.covers(intent.revisions)
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
}
