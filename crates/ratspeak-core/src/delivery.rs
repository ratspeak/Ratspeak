//! The states of an outbound message. Every list of them, in SQL, in the
//! runtime, and in the dashboard, is derived from this table.

/// A value `messages.state` holds for an outbound message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutboundState {
    Sending,
    Routing,
    Propagating,
    LinkEstablishing,
    SendingViaLink,
    ReusingDirectLink,
    ReusingBackchannel,
    Sent,
    Delivered,
    Propagated,
    Failed,
    Cancelled,
    Rejected,
    Timeout,
    Paper,
    /// Not sent for a network reason; sent again when the recipient may be reachable.
    Waiting,
}

/// Where a state stands in a message's send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// On its way.
    InFlight,
    /// Held by nothing, until a wake sends it again or its time runs out.
    Waiting,
    /// Stopped undelivered; Retry sends it again.
    Ended,
    /// Settled; only a proof of delivery can still replace some of these.
    Done,
}

/// Transfer steps reported with delivery progress; they are never stored.
pub const PROGRESS_STEPS: [&str; 4] = [
    "resource_link_ready",
    "resource_advertised",
    "resource_transferring",
    "resource_waiting_for_proof",
];

impl OutboundState {
    pub const ALL: [Self; 16] = [
        Self::Sending,
        Self::Routing,
        Self::Propagating,
        Self::LinkEstablishing,
        Self::SendingViaLink,
        Self::ReusingDirectLink,
        Self::ReusingBackchannel,
        Self::Sent,
        Self::Delivered,
        Self::Propagated,
        Self::Failed,
        Self::Cancelled,
        Self::Rejected,
        Self::Timeout,
        Self::Paper,
        Self::Waiting,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sending => "sending",
            Self::Routing => "routing",
            Self::Propagating => "propagating",
            Self::LinkEstablishing => "link_establishing",
            Self::SendingViaLink => "sending_via_link",
            Self::ReusingDirectLink => "reusing_direct_link",
            Self::ReusingBackchannel => "reusing_backchannel",
            Self::Sent => "sent",
            Self::Delivered => "delivered",
            Self::Propagated => "propagated",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Rejected => "rejected",
            Self::Timeout => "timeout",
            Self::Paper => "paper",
            Self::Waiting => "waiting",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == value)
    }

    pub const fn phase(self) -> Phase {
        match self {
            Self::Sending
            | Self::Routing
            | Self::Propagating
            | Self::LinkEstablishing
            | Self::SendingViaLink
            | Self::ReusingDirectLink
            | Self::ReusingBackchannel
            | Self::Sent => Phase::InFlight,
            Self::Waiting => Phase::Waiting,
            Self::Failed | Self::Cancelled | Self::Timeout => Phase::Ended,
            Self::Delivered | Self::Propagated | Self::Rejected | Self::Paper => Phase::Done,
        }
    }

    /// No later step of the send replaces it.
    pub const fn is_final(self) -> bool {
        matches!(self.phase(), Phase::Ended | Phase::Done)
    }

    /// A proof of delivery replaces it: a message that arrived was never undelivered.
    pub const fn yields_to_proof(self) -> bool {
        !matches!(self, Self::Delivered | Self::Rejected | Self::Paper)
    }

    /// The message has left the queue, so the undelivered watchdog times it.
    pub const fn starts_delivery_clock(self) -> bool {
        matches!(self.phase(), Phase::InFlight) && !matches!(self, Self::Sending)
    }

    /// Nothing is in the air any more, so there is nothing for the watchdog to time.
    pub const fn ends_delivery_clock(self) -> bool {
        self.is_final() || matches!(self, Self::Waiting)
    }

    /// Nothing can send it again, so its outbox entry goes.
    pub const fn clears_outbox(self) -> bool {
        matches!(self, Self::Delivered | Self::Propagated | Self::Rejected)
    }

    /// A copy of the message left this device. The attempt keeps showing Sent
    /// through its later steps; a wake or Retry starts an attempt without one.
    pub const fn sends_a_copy(self) -> bool {
        matches!(self, Self::Sent)
    }
}

/// The states `include` accepts as an SQL list, `('a', 'b')`.
pub fn sql_list(include: impl Fn(OutboundState) -> bool) -> String {
    let names = OutboundState::ALL
        .into_iter()
        .filter(|state| include(*state))
        .map(|state| format!("'{}'", state.as_str()))
        .collect::<Vec<_>>();
    format!("({})", names.join(", "))
}

/// The numbers and switches that decide how long and how a message waits.
pub mod policy {
    /// How long after its send a message may wait before it ends undelivered.
    pub const WAIT_LIMIT_SECS: f64 = 86_400.0;

    /// A stop for a runaway only: more wakes than the gates give one message in its 24 hours.
    pub const WAKE_CAP: u32 = 200;

    /// Whether a message sent with `preference` waits; each waits with its own method.
    pub fn method_waits(preference: &str) -> bool {
        matches!(
            preference,
            "auto" | "opportunistic" | "direct" | "propagated"
        )
    }

    /// The most woken text messages in the air at once, so fresh sends keep
    /// the links. Attachments take the transfer budget instead.
    pub const LANE_LIMIT: usize = 2;

    /// How often waiting messages are checked with no other prompt, and
    /// expired ones ended.
    pub const TIMER_PERIOD_SECS: f64 = 300.0;

    /// An interface coming online wakes at most this often: links flap.
    pub const INTERFACE_WAKE_MIN_SECS: f64 = 120.0;

    /// Turning waiting off ends what waits at once, so nothing is sent later unasked.
    pub const OFF_ENDS_WAITING: bool = true;

    /// After a waiting Auto message was stored in the Offline Inbox, the next
    /// of its queue goes straight there: the live path just failed for it.
    pub const FOLLOWERS_USE_INBOX_AFTER_DEPOSIT: bool = true;

    /// How long a message rests after a wake before `gate` lets it be woken
    /// again. `wakes` counts every wake of it; `unanswered` the wakes in a
    /// row that hearing its recipient prompted and that left it waiting, so
    /// blind tries never slow the answer to the recipient being heard.
    pub fn gate_secs(gate: super::WakeGate, wakes: u32, unanswered: u32) -> f64 {
        let doubled = |base: f64, times: u32| base * 2f64.powi(times.min(16) as i32);
        match gate {
            super::WakeGate::Open => 0.0,
            super::WakeGate::Targeted => doubled(60.0, unanswered).min(900.0),
            super::WakeGate::Broad => doubled(30.0, wakes).min(3_600.0),
        }
    }
}

/// What prompted a wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WakeCause {
    /// The user chose Try now.
    User,
    /// The user sent another message to the recipient.
    NewSend,
    /// An earlier message of the queue finished.
    Queue,
    /// The recipient was heard: an announce or a path.
    Recipient,
    /// The Offline Inbox became usable.
    Inbox,
    /// Ratspeak started or came to the foreground.
    Start,
    /// An interface came online.
    Network,
    /// The periodic check.
    Timer,
}

/// How a cause is held back from waking a message that was just woken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeGate {
    /// Not at all: the user asked, or the queue moved on.
    Open,
    /// Briefly: something was heard of this recipient or the inbox.
    Targeted,
    /// With a growing pause: nothing says this recipient is nearer.
    Broad,
}

impl WakeCause {
    /// Strongest first: where several causes meet, the first of these wins.
    pub const ALL: [Self; 8] = [
        Self::User,
        Self::NewSend,
        Self::Queue,
        Self::Recipient,
        Self::Inbox,
        Self::Start,
        Self::Network,
        Self::Timer,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::User => "wake_user",
            Self::NewSend => "wake_new_send",
            Self::Queue => "wake_queue",
            Self::Recipient => "wake_recipient",
            Self::Inbox => "wake_inbox",
            Self::Start => "wake_start",
            Self::Network => "wake_network",
            Self::Timer => "wake_timer",
        }
    }

    pub const fn gate(self) -> WakeGate {
        match self {
            Self::User | Self::NewSend | Self::Queue => WakeGate::Open,
            Self::Recipient | Self::Inbox => WakeGate::Targeted,
            Self::Start | Self::Network | Self::Timer => WakeGate::Broad,
        }
    }

    /// Whether the cause is acted on only with an interface online; what the
    /// user asks for is tried regardless.
    pub const fn needs_network(self) -> bool {
        !matches!(self, Self::User | Self::NewSend)
    }

    /// The stronger of two causes that meet on one queue.
    pub fn stronger(self, other: Self) -> Self {
        let rank = |cause: Self| Self::ALL.iter().position(|each| *each == cause);
        if rank(other) < rank(self) {
            other
        } else {
            self
        }
    }
}

/// What the outbox holds about a message's send.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutboxClock {
    /// When the user last sent the message.
    pub queued_at: f64,
    /// How many times it was woken since.
    pub auto_sends: u32,
    pub method_waits: bool,
}

/// A send that did not go through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Failure {
    /// The reason is the network's.
    pub can_wait: bool,
    /// The user's setting.
    pub keep_trying: bool,
    pub now: f64,
    /// `None` for a message nothing can send again.
    pub entry: Option<OutboxClock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFailure {
    Wait,
    Fail,
    /// Its time to wait has run out.
    FailExpired,
    /// It was woken as often as a message may be.
    FailExhausted,
}

/// Whether a failed send waits to be sent again or ends undelivered.
pub fn after_failure(failure: &Failure) -> AfterFailure {
    if !failure.can_wait || !failure.keep_trying {
        return AfterFailure::Fail;
    }
    let Some(entry) = failure.entry.filter(|entry| entry.method_waits) else {
        return AfterFailure::Fail;
    };
    if wait_expired(entry.queued_at, failure.now) {
        AfterFailure::FailExpired
    } else if entry.auto_sends >= policy::WAKE_CAP {
        AfterFailure::FailExhausted
    } else {
        AfterFailure::Wait
    }
}

/// Whether a message queued at `queued_at` has waited its full time by `now`.
pub fn wait_expired(queued_at: f64, now: f64) -> bool {
    now - queued_at >= policy::WAIT_LIMIT_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_one_name_and_parses_back() {
        let names = OutboundState::ALL
            .iter()
            .map(|state| state.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(names.len(), OutboundState::ALL.len());
        for state in OutboundState::ALL {
            assert_eq!(OutboundState::parse(state.as_str()), Some(state));
        }
        for name in [
            "",
            "unknown",
            "received",
            "Sending",
            "resource_transferring",
        ] {
            assert_eq!(OutboundState::parse(name), None, "{name}");
        }
        for step in PROGRESS_STEPS {
            assert_eq!(OutboundState::parse(step), None, "{step} is never stored");
        }
    }

    #[test]
    fn each_class_holds_exactly_its_states() {
        use OutboundState::*;
        let class = |include: fn(OutboundState) -> bool| {
            OutboundState::ALL
                .into_iter()
                .filter(|state| include(*state))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            class(|state| state.phase() == Phase::InFlight),
            [
                Sending,
                Routing,
                Propagating,
                LinkEstablishing,
                SendingViaLink,
                ReusingDirectLink,
                ReusingBackchannel,
                Sent
            ]
        );
        assert_eq!(class(|state| state.phase() == Phase::Waiting), [Waiting]);
        assert_eq!(
            class(|state| state.phase() == Phase::Ended),
            [Failed, Cancelled, Timeout]
        );
        assert_eq!(
            class(|state| state.phase() == Phase::Done),
            [Delivered, Propagated, Rejected, Paper]
        );
        assert_eq!(
            class(OutboundState::is_final),
            [
                Delivered, Propagated, Failed, Cancelled, Rejected, Timeout, Paper
            ]
        );
        assert_eq!(
            class(|state| !state.yields_to_proof()),
            [Delivered, Rejected, Paper]
        );
        assert_eq!(
            class(OutboundState::starts_delivery_clock),
            [
                Routing,
                Propagating,
                LinkEstablishing,
                SendingViaLink,
                ReusingDirectLink,
                ReusingBackchannel,
                Sent
            ]
        );
        assert_eq!(
            class(OutboundState::ends_delivery_clock),
            [
                Delivered, Propagated, Failed, Cancelled, Rejected, Timeout, Paper, Waiting
            ]
        );
        assert_eq!(
            class(OutboundState::clears_outbox),
            [Delivered, Propagated, Rejected]
        );
        assert_eq!(class(OutboundState::sends_a_copy), [Sent]);
    }

    #[test]
    fn a_failed_send_waits_only_when_every_check_allows_it() {
        use AfterFailure::*;
        const NOW: f64 = 1_000_000.0;
        let fresh = OutboxClock {
            queued_at: NOW - 60.0,
            auto_sends: 0,
            method_waits: true,
        };
        let expired = OutboxClock {
            queued_at: NOW - policy::WAIT_LIMIT_SECS,
            ..fresh
        };
        let spent = OutboxClock {
            auto_sends: policy::WAKE_CAP,
            ..fresh
        };
        let failure = |can_wait, keep_trying, entry| Failure {
            can_wait,
            keep_trying,
            now: NOW,
            entry,
        };
        // One row per check, in the order the checks run; each later row passes the earlier ones.
        let rows = [
            (
                failure(false, true, Some(fresh)),
                Fail,
                "the reason is not the network's",
            ),
            (
                failure(true, false, Some(fresh)),
                Fail,
                "the setting is off",
            ),
            (failure(true, true, None), Fail, "nothing can send it again"),
            (
                failure(
                    true,
                    true,
                    Some(OutboxClock {
                        method_waits: false,
                        ..fresh
                    }),
                ),
                Fail,
                "its method does not wait",
            ),
            (
                failure(true, true, Some(expired)),
                FailExpired,
                "its time ran out",
            ),
            (
                failure(true, true, Some(spent)),
                FailExhausted,
                "it was woken too often",
            ),
            (failure(true, true, Some(fresh)), Wait, "it waits"),
        ];
        for (failure, verdict, why) in rows {
            assert_eq!(after_failure(&failure), verdict, "{why}");
        }

        // An earlier check wins over a later one.
        let expired_and_spent = OutboxClock {
            auto_sends: policy::WAKE_CAP,
            ..expired
        };
        assert_eq!(
            after_failure(&failure(true, true, Some(expired_and_spent))),
            FailExpired
        );
        for (can_wait, keep_trying) in [(false, true), (true, false)] {
            assert_eq!(
                after_failure(&failure(can_wait, keep_trying, Some(expired_and_spent))),
                Fail,
                "an expired wait is only named for a message that would have waited"
            );
        }
        let unwaiting_and_expired = OutboxClock {
            method_waits: false,
            ..expired
        };
        assert_eq!(
            after_failure(&failure(true, true, Some(unwaiting_and_expired))),
            Fail
        );

        // The limits are reached, not passed.
        let almost = |entry: OutboxClock| after_failure(&failure(true, true, Some(entry)));
        assert_eq!(
            almost(OutboxClock {
                queued_at: NOW - policy::WAIT_LIMIT_SECS + 0.001,
                ..fresh
            }),
            Wait
        );
        assert_eq!(
            almost(OutboxClock {
                auto_sends: policy::WAKE_CAP - 1,
                ..fresh
            }),
            Wait
        );
    }

    #[test]
    fn a_wake_is_gated_by_what_prompted_it() {
        use WakeCause::*;
        let codes = WakeCause::ALL.map(WakeCause::code);
        assert_eq!(
            codes,
            [
                "wake_user",
                "wake_new_send",
                "wake_queue",
                "wake_recipient",
                "wake_inbox",
                "wake_start",
                "wake_network",
                "wake_timer"
            ]
        );
        for (cause, gate, needs_network) in [
            (User, WakeGate::Open, false),
            (NewSend, WakeGate::Open, false),
            (Queue, WakeGate::Open, true),
            (Recipient, WakeGate::Targeted, true),
            (Inbox, WakeGate::Targeted, true),
            (Start, WakeGate::Broad, true),
            (Network, WakeGate::Broad, true),
            (Timer, WakeGate::Broad, true),
        ] {
            assert_eq!(cause.gate(), gate, "{}", cause.code());
            assert_eq!(cause.needs_network(), needs_network, "{}", cause.code());
        }
        assert_eq!(Timer.stronger(Recipient), Recipient);
        assert_eq!(Recipient.stronger(Timer), Recipient);
        assert_eq!(Recipient.stronger(User), User);
        assert_eq!(Start.stronger(Network), Start);

        // (count, targeted pause after that many unanswered, broad pause after that many wakes)
        for (count, targeted, broad) in [
            (0, 60.0, 30.0),
            (1, 120.0, 60.0),
            (2, 240.0, 120.0),
            (3, 480.0, 240.0),
            (4, 900.0, 480.0),
            (5, 900.0, 960.0),
            (7, 900.0, 3_600.0),
            (policy::WAKE_CAP - 1, 900.0, 3_600.0),
        ] {
            assert_eq!(policy::gate_secs(WakeGate::Open, count, count), 0.0);
            // Each gate counts its own tries and not the other's.
            for other in [0, policy::WAKE_CAP] {
                assert_eq!(
                    policy::gate_secs(WakeGate::Targeted, other, count),
                    targeted,
                    "{count}"
                );
                assert_eq!(
                    policy::gate_secs(WakeGate::Broad, count, other),
                    broad,
                    "{count}"
                );
            }
        }
        assert_eq!(policy::TIMER_PERIOD_SECS, 300.0);
        // More than the gates give one message in its 24 hours: about 100
        // answers to its recipient being heard, and the timer's 30.
        let (hour, day) = (3_600.0, policy::WAIT_LIMIT_SECS);
        assert!(f64::from(policy::WAKE_CAP) > day / 900.0 + day / hour + 10.0);
    }

    #[test]
    fn every_send_method_waits_with_its_own_method() {
        for preference in ["auto", "opportunistic", "direct", "propagated"] {
            assert!(policy::method_waits(preference), "{preference}");
        }
        assert!(!policy::method_waits("paper"));
        assert!(!policy::method_waits(""));
    }

    #[test]
    fn an_sql_list_is_quoted_and_in_table_order() {
        assert_eq!(
            sql_list(|state| state.phase() == Phase::Ended),
            "('failed', 'cancelled', 'timeout')"
        );
    }
}
