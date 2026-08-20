//! Public, timeless, sealed Activity producer facade.
//!
//! Domain code can select only an event-specific constructor and typed,
//! allowlisted fields. The resulting [`ProducerEvent`] is opaque and carries
//! no timestamp. [`super::ActivityRecorder`] samples its own clock after the
//! capture gate admits the lazy producer closure, then converts the event into
//! a private classified draft.

use super::catalog;
use super::classified::{ActivityDraft, ActivityRejectReason};

pub use super::catalog::{
    AnnounceComponents, AnnounceFailureReason, AnnounceMethod, AnnounceSuppressionReason,
    AppRuntimeTransition, ChannelEnvelopeKind, ChannelJoinEvidence, ChannelMessageToken,
    ChannelNegotiatedCapabilities, ChannelNegotiatedLimits, ChannelRoomFailureReason,
    ChannelRoomToken, ChannelRoomTransition, ChannelSessionCloseReason,
    ChannelSessionFailureReason, ChannelSessionTransition, DeliveryFailureReason, DestinationHash,
    HubModerationAction, HubServiceDegradation, HubSessionCloseReason, HubSessionRejection,
    HubTransition, HubTrustChange, IdentityHash, InboundLxmfMethod, InterfaceClass,
    InterfaceDegradationReason, InterfaceFailureReason, InterfaceRollback, InterfaceTimeoutReason,
    InterfaceTransition, LinkId, LxmfDeliveryMethod, LxmfDeliveryState, LxmfInboundRejectionReason,
    LxmfProgressStep, LxmfSubmissionFailureReason, LxstCallReason, LxstTransition, MessageId,
    PathEvidence, PathRequestMethod, SourceValidation, TcpEndpoint,
};

/// Opaque timeless event accepted by [`super::ActivityRecorder::record_event`].
/// Its representation and variants are deliberately private.
pub struct ProducerEvent(Payload);

enum Payload {
    AppRuntime(AppRuntimeTransition),
    Interface(InterfaceActivity),
    RnsPathRequested(RnsPathRequested),
    RnsPathDiscovered(RnsPathDiscovered),
    RnsPathObserved(RnsPathDiscovered),
    RnsAnnounce {
        transition: catalog::RnsAnnounceTransition,
        interface: Option<InterfaceClass>,
    },
    ChannelsSession(ChannelsSessionActivity),
    ChannelsRoom(ChannelsRoomActivity),
    ChannelsEnvelopeSent(ChannelsEnvelopeActivity),
    ChannelsEnvelopeReceived(ChannelsEnvelopeActivity),
    ChannelsHub(ChannelsHubActivity),
    LxmfDeliveryQueued(LxmfDeliveryQueued),
    LxmfSubmissionFailed(LxmfSubmissionFailed),
    LxmfDeliveryStateChanged(LxmfDeliveryStateChanged),
    LxmfDeliveryProgress(LxmfDeliveryProgress),
    LxmfInboundAccepted(LxmfInboundAccepted),
    LxmfInboundRejected(LxmfInboundRejected),
    LxmfPropagationLimitExceeded(LxmfPropagationLimitExceeded),
    LxmfDeliveryFailed(LxmfDeliveryFailed),
    Lxst(LxstTransition),
}

impl ProducerEvent {
    pub(super) fn into_unstamped_draft(self) -> Result<ActivityDraft, ActivityRejectReason> {
        let time = catalog::ObservationTime::unstamped();
        match self.0 {
            Payload::AppRuntime(transition) => Ok(catalog::app_runtime(time, transition)),
            Payload::Interface(input) => catalog::interface_activity(catalog::InterfaceActivity {
                time,
                class: input.class,
                transition: input.transition,
                endpoint: input.endpoint,
            }),
            Payload::RnsPathRequested(input) => {
                catalog::rns_path_requested(catalog::RnsPathRequested {
                    time,
                    destination: input.destination,
                    count: input.count,
                    method: input.method,
                })
            }
            Payload::RnsPathDiscovered(input) => {
                catalog::rns_path_discovered(catalog::RnsPathDiscovered {
                    time,
                    destination: input.destination,
                    hops: input.hops,
                    evidence: input.evidence,
                    endpoint: input.endpoint,
                    correlation_id: input.correlation_id,
                })
            }
            Payload::RnsPathObserved(input) => {
                catalog::rns_path_observed(catalog::RnsPathDiscovered {
                    time,
                    destination: input.destination,
                    hops: input.hops,
                    evidence: input.evidence,
                    endpoint: input.endpoint,
                    correlation_id: input.correlation_id,
                })
            }
            Payload::RnsAnnounce {
                transition,
                interface,
            } => catalog::rns_announce_activity(catalog::RnsAnnounceActivity {
                time,
                transition,
                interface,
            }),
            Payload::ChannelsSession(input) => {
                catalog::channels_session_activity(catalog::ChannelsSessionActivity {
                    time,
                    hub: input.hub,
                    correlation_id: input.correlation_id,
                    transition: input.transition,
                })
            }
            Payload::ChannelsRoom(input) => {
                catalog::channels_room_activity(catalog::ChannelsRoomActivity {
                    time,
                    hub: input.hub,
                    room: input.room,
                    correlation_id: input.correlation_id,
                    transition: input.transition,
                })
            }
            Payload::ChannelsEnvelopeSent(input) => {
                catalog::channels_envelope_sent(catalog::ChannelsEnvelopeActivity {
                    time,
                    hub: input.hub,
                    room: input.room,
                    message: input.message,
                    envelope_kind: input.envelope_kind,
                    encoded_bytes: input.encoded_bytes,
                    validation: input.validation,
                    correlation_id: input.correlation_id,
                })
            }
            Payload::ChannelsEnvelopeReceived(input) => {
                catalog::channels_envelope_received(catalog::ChannelsEnvelopeActivity {
                    time,
                    hub: input.hub,
                    room: input.room,
                    message: input.message,
                    envelope_kind: input.envelope_kind,
                    encoded_bytes: input.encoded_bytes,
                    validation: input.validation,
                    correlation_id: input.correlation_id,
                })
            }
            Payload::ChannelsHub(input) => {
                catalog::channels_hub_activity(catalog::ChannelsHubActivity {
                    time,
                    hub: input.hub,
                    correlation_id: input.correlation_id,
                    transition: input.transition,
                })
            }
            Payload::LxmfDeliveryQueued(input) => {
                catalog::lxmf_delivery_queued(catalog::LxmfDeliveryQueued {
                    time,
                    message: input.message,
                    destination: input.destination,
                    method: input.method,
                })
            }
            Payload::LxmfSubmissionFailed(input) => {
                catalog::lxmf_submission_failed(catalog::LxmfSubmissionFailed {
                    time,
                    destination: input.destination,
                    reason: input.reason,
                })
            }
            Payload::LxmfDeliveryStateChanged(input) => {
                catalog::lxmf_delivery_state_changed(catalog::LxmfDeliveryStateChanged {
                    time,
                    message: input.message,
                    state: input.state,
                    method: input.method,
                    rtt_ms: input.rtt_ms,
                    failure_reason: input.failure_reason,
                })
            }
            Payload::LxmfDeliveryProgress(input) => {
                catalog::lxmf_delivery_progress(catalog::LxmfDeliveryProgress {
                    time,
                    message: input.message,
                    destination: input.destination,
                    link: input.link,
                    method: input.method,
                    step: input.step,
                    percent: input.percent,
                    attempts: input.attempts,
                })
            }
            Payload::LxmfInboundAccepted(input) => {
                catalog::lxmf_inbound_accepted(catalog::LxmfInboundAccepted {
                    time,
                    source: input.source,
                    method: input.method,
                    encoded_bytes: input.encoded_bytes,
                })
            }
            Payload::LxmfInboundRejected(input) => {
                catalog::lxmf_inbound_rejected(catalog::LxmfInboundRejected {
                    time,
                    link: input.link,
                    encoded_bytes: input.encoded_bytes,
                    max_message_bytes: input.max_message_bytes,
                    reason: input.reason,
                })
            }
            Payload::LxmfPropagationLimitExceeded(input) => {
                catalog::lxmf_propagation_limit_exceeded(catalog::LxmfPropagationLimitExceeded {
                    time,
                    message: input.message,
                    encoded_bytes: input.encoded_bytes,
                    max_message_bytes: input.max_message_bytes,
                })
            }
            Payload::LxmfDeliveryFailed(input) => {
                catalog::lxmf_delivery_failed(catalog::LxmfDeliveryFailed {
                    time,
                    message_id: input.message,
                    destination: input.destination,
                    link_id: input.link,
                    reason: input.reason,
                    correlation_id: input.correlation_id,
                })
            }
            Payload::Lxst(transition) => {
                catalog::lxst_activity(catalog::LxstActivity { time, transition })
            }
        }
    }
}

pub fn app_runtime(transition: AppRuntimeTransition) -> ProducerEvent {
    ProducerEvent(Payload::AppRuntime(transition))
}

pub struct InterfaceActivity {
    pub class: InterfaceClass,
    pub transition: InterfaceTransition,
    pub endpoint: Option<TcpEndpoint>,
}

pub fn interface_activity(input: InterfaceActivity) -> ProducerEvent {
    ProducerEvent(Payload::Interface(input))
}

pub struct RnsPathRequested {
    pub destination: Option<DestinationHash>,
    pub count: Option<u64>,
    pub method: PathRequestMethod,
}

pub fn rns_path_requested(input: RnsPathRequested) -> ProducerEvent {
    ProducerEvent(Payload::RnsPathRequested(input))
}

pub struct RnsPathDiscovered {
    pub destination: DestinationHash,
    pub hops: u8,
    pub evidence: PathEvidence,
    pub endpoint: Option<TcpEndpoint>,
    pub correlation_id: Option<super::CorrelationId>,
}

pub fn rns_path_discovered(input: RnsPathDiscovered) -> ProducerEvent {
    ProducerEvent(Payload::RnsPathDiscovered(input))
}

pub fn rns_path_observed(input: RnsPathDiscovered) -> ProducerEvent {
    ProducerEvent(Payload::RnsPathObserved(input))
}

pub enum RnsAnnounceTransition {
    Queued {
        method: AnnounceMethod,
        components: AnnounceComponents,
        count: u64,
        correlation_id: super::CorrelationId,
    },
    Failed {
        method: AnnounceMethod,
        reason: AnnounceFailureReason,
    },
    Held {
        count: u64,
    },
    IngressBurstStarted,
    IngressBurstCleared,
    Suppressed {
        reason: AnnounceSuppressionReason,
    },
    Observed {
        destination: DestinationHash,
        hops: u8,
    },
}

pub struct RnsAnnounceActivity {
    pub transition: RnsAnnounceTransition,
    pub interface: Option<InterfaceClass>,
}

pub fn rns_announce_activity(input: RnsAnnounceActivity) -> ProducerEvent {
    let transition = match input.transition {
        RnsAnnounceTransition::Queued {
            method,
            components,
            count,
            correlation_id,
        } => catalog::RnsAnnounceTransition::Queued {
            method,
            components,
            count,
            correlation_id,
        },
        RnsAnnounceTransition::Failed { method, reason } => {
            catalog::RnsAnnounceTransition::Failed { method, reason }
        }
        RnsAnnounceTransition::Held { count } => catalog::RnsAnnounceTransition::Held { count },
        RnsAnnounceTransition::IngressBurstStarted => {
            catalog::RnsAnnounceTransition::IngressBurstStarted
        }
        RnsAnnounceTransition::IngressBurstCleared => {
            catalog::RnsAnnounceTransition::IngressBurstCleared
        }
        RnsAnnounceTransition::Suppressed { reason } => {
            catalog::RnsAnnounceTransition::Suppressed { reason }
        }
        RnsAnnounceTransition::Observed { destination, hops } => {
            catalog::RnsAnnounceTransition::Observed { destination, hops }
        }
    };
    ProducerEvent(Payload::RnsAnnounce {
        transition,
        interface: input.interface,
    })
}

pub struct ChannelsSessionActivity {
    pub hub: DestinationHash,
    pub correlation_id: super::CorrelationId,
    pub transition: ChannelSessionTransition,
}

pub fn channels_session_activity(input: ChannelsSessionActivity) -> ProducerEvent {
    ProducerEvent(Payload::ChannelsSession(input))
}

pub struct ChannelsRoomActivity {
    pub hub: DestinationHash,
    pub room: ChannelRoomToken,
    pub correlation_id: super::CorrelationId,
    pub transition: ChannelRoomTransition,
}

pub fn channels_room_activity(input: ChannelsRoomActivity) -> ProducerEvent {
    ProducerEvent(Payload::ChannelsRoom(input))
}

pub struct ChannelsEnvelopeActivity {
    pub hub: DestinationHash,
    pub room: Option<ChannelRoomToken>,
    pub message: Option<ChannelMessageToken>,
    pub envelope_kind: Option<ChannelEnvelopeKind>,
    pub encoded_bytes: u32,
    pub validation: SourceValidation,
    pub correlation_id: super::CorrelationId,
}

pub fn channels_envelope_sent(input: ChannelsEnvelopeActivity) -> ProducerEvent {
    ProducerEvent(Payload::ChannelsEnvelopeSent(input))
}

pub fn channels_envelope_received(input: ChannelsEnvelopeActivity) -> ProducerEvent {
    ProducerEvent(Payload::ChannelsEnvelopeReceived(input))
}

pub struct ChannelsHubActivity {
    pub hub: DestinationHash,
    pub correlation_id: super::CorrelationId,
    pub transition: HubTransition,
}

pub fn channels_hub_activity(input: ChannelsHubActivity) -> ProducerEvent {
    ProducerEvent(Payload::ChannelsHub(input))
}

pub struct LxmfDeliveryQueued {
    pub message: MessageId,
    pub destination: DestinationHash,
    pub method: LxmfDeliveryMethod,
}

pub fn lxmf_delivery_queued(input: LxmfDeliveryQueued) -> ProducerEvent {
    ProducerEvent(Payload::LxmfDeliveryQueued(input))
}

pub struct LxmfSubmissionFailed {
    pub destination: DestinationHash,
    pub reason: LxmfSubmissionFailureReason,
}

pub fn lxmf_submission_failed(input: LxmfSubmissionFailed) -> ProducerEvent {
    ProducerEvent(Payload::LxmfSubmissionFailed(input))
}

pub struct LxmfDeliveryStateChanged {
    pub message: MessageId,
    pub state: LxmfDeliveryState,
    pub method: Option<LxmfDeliveryMethod>,
    pub rtt_ms: Option<u64>,
    pub failure_reason: Option<DeliveryFailureReason>,
}

pub fn lxmf_delivery_state_changed(input: LxmfDeliveryStateChanged) -> ProducerEvent {
    ProducerEvent(Payload::LxmfDeliveryStateChanged(input))
}

pub struct LxmfDeliveryProgress {
    pub message: MessageId,
    pub destination: DestinationHash,
    pub link: Option<LinkId>,
    pub method: LxmfDeliveryMethod,
    pub step: LxmfProgressStep,
    pub percent: Option<u8>,
    pub attempts: u32,
}

pub fn lxmf_delivery_progress(input: LxmfDeliveryProgress) -> ProducerEvent {
    ProducerEvent(Payload::LxmfDeliveryProgress(input))
}

pub struct LxmfInboundAccepted {
    pub source: DestinationHash,
    pub method: InboundLxmfMethod,
    pub encoded_bytes: u32,
}

pub fn lxmf_inbound_accepted(input: LxmfInboundAccepted) -> ProducerEvent {
    ProducerEvent(Payload::LxmfInboundAccepted(input))
}

pub struct LxmfInboundRejected {
    pub link: LinkId,
    pub encoded_bytes: u64,
    pub max_message_bytes: u64,
    pub reason: catalog::LxmfInboundRejectionReason,
}

pub fn lxmf_inbound_rejected(input: LxmfInboundRejected) -> ProducerEvent {
    ProducerEvent(Payload::LxmfInboundRejected(input))
}

pub struct LxmfPropagationLimitExceeded {
    pub message: MessageId,
    pub encoded_bytes: u64,
    pub max_message_bytes: u64,
}

pub fn lxmf_propagation_limit_exceeded(input: LxmfPropagationLimitExceeded) -> ProducerEvent {
    ProducerEvent(Payload::LxmfPropagationLimitExceeded(input))
}

pub struct LxmfDeliveryFailed {
    pub message: MessageId,
    pub destination: DestinationHash,
    pub link: Option<LinkId>,
    pub reason: DeliveryFailureReason,
    pub correlation_id: super::CorrelationId,
}

pub fn lxmf_delivery_failed(input: LxmfDeliveryFailed) -> ProducerEvent {
    ProducerEvent(Payload::LxmfDeliveryFailed(input))
}

pub fn lxst_activity(transition: LxstTransition) -> ProducerEvent {
    ProducerEvent(Payload::Lxst(transition))
}
