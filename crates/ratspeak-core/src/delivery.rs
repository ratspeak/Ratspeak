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
}

/// Where a state stands in a message's send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// On its way.
    InFlight,
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
    pub const ALL: [Self; 15] = [
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

    pub const fn ends_delivery_clock(self) -> bool {
        self.is_final()
    }

    /// Nothing can send it again, so its outbox entry goes.
    pub const fn clears_outbox(self) -> bool {
        matches!(self, Self::Delivered | Self::Propagated | Self::Rejected)
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
                Delivered, Propagated, Failed, Cancelled, Rejected, Timeout, Paper
            ]
        );
        assert_eq!(
            class(OutboundState::clears_outbox),
            [Delivered, Propagated, Rejected]
        );
    }

    #[test]
    fn an_sql_list_is_quoted_and_in_table_order() {
        assert_eq!(
            sql_list(|state| state.phase() == Phase::Ended),
            "('failed', 'cancelled', 'timeout')"
        );
    }
}
