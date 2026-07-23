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
    AnnounceFailureReason, AnnounceMethod, AnnounceSuppressionReason, AppRuntimeTransition,
    DeliveryFailureReason, DestinationHash, IdentityHash, InboundLxmfMethod, InterfaceClass,
    InterfaceDegradationReason, InterfaceFailureReason, InterfaceRollback, InterfaceTimeoutReason,
    InterfaceTransition, LinkId, LxmfDeliveryMethod, LxmfDeliveryState, LxmfProgressStep,
    LxmfSubmissionFailureReason, LxstCallReason, LxstTransition, MessageId, PathEvidence,
    PathRequestMethod, TcpEndpoint,
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
    LxmfDeliveryQueued(LxmfDeliveryQueued),
    LxmfSubmissionFailed(LxmfSubmissionFailed),
    LxmfDeliveryStateChanged(LxmfDeliveryStateChanged),
    LxmfDeliveryProgress(LxmfDeliveryProgress),
    LxmfInboundAccepted(LxmfInboundAccepted),
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
    Sent {
        method: AnnounceMethod,
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
        RnsAnnounceTransition::Sent { method } => catalog::RnsAnnounceTransition::Sent { method },
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
