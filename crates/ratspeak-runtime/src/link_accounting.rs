//! Nonblocking demultiplexing of the Link owner's ordered accounting stream.
//!
//! Completed Resource payloads carry their existing memory admission to the
//! consumer. Their queue cannot grow past that budget, and a slow consumer
//! cannot block outbound deadline/proof events in the accounting stream.

use rns_runtime::link_manager::{
    LinkManagerAccountingEvent, LinkResourceConclusion, LinkResourceDirection, LinkResourceEvent,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::lxmf::BackchannelLinkEvent;
use crate::state::AttachmentTransferLease;

pub(crate) struct InboundResourceDelivery {
    pub(crate) data: Vec<u8>,
    pub(crate) link_id: [u8; 16],
    pub(crate) lease: AttachmentTransferLease,
}

pub(crate) fn forward(
    event: LinkManagerAccountingEvent,
    inbound_tx: &UnboundedSender<InboundResourceDelivery>,
    backchannel_tx: &UnboundedSender<BackchannelLinkEvent>,
    mut take_resource: impl FnMut([u8; 16], [u8; 32]) -> Option<AttachmentTransferLease>,
    mut release_link: impl FnMut([u8; 16]),
) {
    let event = match event {
        LinkManagerAccountingEvent::OutboundPacketWait {
            receipt,
            started_at,
            timeout,
            awaiting_admission,
            cancellation,
        } => BackchannelLinkEvent::PacketWait {
            link_id: receipt.link_id,
            packet_hash: receipt.packet_hash,
            started_at,
            timeout,
            awaiting_admission,
            cancellation,
        },
        LinkManagerAccountingEvent::OutboundResourceWait {
            link_id,
            resource_id,
            started_at,
            timeout,
        } => BackchannelLinkEvent::ResourceWait {
            link_id,
            resource_hash: resource_id,
            started_at,
            timeout,
        },
        LinkManagerAccountingEvent::LinkPacketProof(proof) => {
            BackchannelLinkEvent::PacketProof(proof)
        }
        LinkManagerAccountingEvent::ResourceCompletion(completion) => {
            if let Some(lease) = take_resource(completion.link_id, completion.resource_hash) {
                // No cloning, awaiting, or early budget release. A closed
                // receiver drops the envelope/lease; later cleanup events
                // must still run until the session owner shuts the bridge down.
                let _ = inbound_tx.send(InboundResourceDelivery {
                    data: completion.data,
                    link_id: completion.link_id,
                    lease,
                });
            }
            return;
        }
        LinkManagerAccountingEvent::ResourceEvent(LinkResourceEvent::Concluded {
            link_id,
            resource_id,
            direction: LinkResourceDirection::Inbound,
            conclusion,
            ..
        }) if !matches!(conclusion, LinkResourceConclusion::Complete) => {
            drop(take_resource(link_id, resource_id));
            return;
        }
        LinkManagerAccountingEvent::ResourceEvent(LinkResourceEvent::Concluded {
            link_id,
            resource_id,
            direction: LinkResourceDirection::Outbound,
            conclusion,
        }) => BackchannelLinkEvent::ResourceConclusion {
            link_id,
            resource_hash: resource_id,
            conclusion,
        },
        LinkManagerAccountingEvent::LinkClosed { link_id } => {
            release_link(link_id);
            BackchannelLinkEvent::LinkClosed { link_id }
        }
        _ => return,
    };
    let _ = backchannel_tx.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, AttachmentTransferAdmissionError};
    use rns_runtime::link_manager::{LinkPacketProof, LinkPacketSendReceipt, ResourceCompletion};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;

    fn make_state() -> (tempfile::TempDir, Arc<AppState>) {
        let root = tempfile::tempdir().unwrap();
        let config = crate::config::DashboardConfig::from_env_and_defaults(root.path().into());
        let pool = r2d2::Pool::builder()
            .max_size(1)
            .build(r2d2_sqlite::SqliteConnectionManager::memory())
            .unwrap();
        let state = Arc::new(AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        (root, state)
    }

    fn forward_owned(
        state: &AppState,
        event: LinkManagerAccountingEvent,
        inbound_tx: &UnboundedSender<InboundResourceDelivery>,
        backchannel_tx: &UnboundedSender<BackchannelLinkEvent>,
    ) {
        forward(
            event,
            inbound_tx,
            backchannel_tx,
            |link, resource| state.take_inbound_attachment_resource(link, resource),
            |link| {
                state.release_inbound_attachment_link(link);
            },
        );
    }

    fn completion(hash: u8, data: Vec<u8>) -> LinkManagerAccountingEvent {
        LinkManagerAccountingEvent::ResourceCompletion(ResourceCompletion {
            link_id: [1; 16],
            resource_hash: [hash; 32],
            data,
            metadata: None,
        })
    }

    #[test]
    fn stalled_inbound_consumer_does_not_block_original_waits_or_terminal_order() {
        let (_root, state) = make_state();
        let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel();
        let (backchannel_tx, mut backchannel_rx) = mpsc::unbounded_channel();
        let mut allocations = Vec::new();
        // More than the removed intermediate channel's 64 slots. Each payload
        // really owns admission; do not drain until all control events arrive.
        for index in 0..128u8 {
            state
                .admit_inbound_attachment_resource([1; 16], [index; 32], [index; 32], 1024)
                .unwrap();
            let data = vec![index; 1024];
            allocations.push(data.as_ptr());
            forward_owned(
                &state,
                completion(index, data),
                &inbound_tx,
                &backchannel_tx,
            );
        }
        let started_at = Instant::now() - Duration::from_secs(240);
        let timeout = Duration::from_secs(600);
        let events = [
            LinkManagerAccountingEvent::OutboundPacketWait {
                receipt: LinkPacketSendReceipt {
                    link_id: [1; 16],
                    packet_hash: [2; 32],
                },
                started_at,
                timeout,
                awaiting_admission: false,
                cancellation: None,
            },
            LinkManagerAccountingEvent::OutboundResourceWait {
                link_id: [1; 16],
                resource_id: [3; 32],
                started_at,
                timeout,
            },
            LinkManagerAccountingEvent::LinkPacketProof(LinkPacketProof {
                link_id: [1; 16],
                packet_hash: [2; 32],
            }),
            LinkManagerAccountingEvent::ResourceEvent(LinkResourceEvent::Concluded {
                link_id: [1; 16],
                resource_id: [3; 32],
                direction: LinkResourceDirection::Outbound,
                conclusion: LinkResourceConclusion::Complete,
            }),
            LinkManagerAccountingEvent::LinkClosed { link_id: [1; 16] },
        ];
        for event in events {
            forward_owned(&state, event, &inbound_tx, &backchannel_tx);
        }
        assert!(
            matches!(backchannel_rx.try_recv().unwrap(), BackchannelLinkEvent::PacketWait {
            started_at: observed, timeout: duration, awaiting_admission: false, ..
        } if observed == started_at && duration == timeout)
        );
        assert!(
            matches!(backchannel_rx.try_recv().unwrap(), BackchannelLinkEvent::ResourceWait {
            started_at: observed, timeout: duration, ..
        } if observed == started_at && duration == timeout)
        );
        assert!(matches!(
            backchannel_rx.try_recv().unwrap(),
            BackchannelLinkEvent::PacketProof(_)
        ));
        assert!(matches!(
            backchannel_rx.try_recv().unwrap(),
            BackchannelLinkEvent::ResourceConclusion {
                conclusion: LinkResourceConclusion::Complete,
                ..
            }
        ));
        assert!(matches!(
            backchannel_rx.try_recv().unwrap(),
            BackchannelLinkEvent::LinkClosed { .. }
        ));
        assert!(backchannel_rx.try_recv().is_err());
        for (index, allocation) in allocations.into_iter().enumerate() {
            let delivery = inbound_rx.try_recv().unwrap();
            assert_eq!(
                delivery.data.as_ptr(),
                allocation,
                "completion must move, not clone"
            );
            assert_eq!(delivery.data, vec![index as u8; 1024]);
            assert_eq!(delivery.link_id, [1; 16]);
        }
        assert!(inbound_rx.try_recv().is_err());
    }

    #[test]
    fn queued_completion_retains_memory_through_link_close_and_processing() {
        let (_root, state) = make_state();
        let size = rns_protocol::resource::MAX_EFFICIENT_SIZE + 1;
        let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel();
        let (backchannel_tx, _backchannel_rx) = mpsc::unbounded_channel();
        state
            .admit_inbound_attachment_resource([1; 16], [2; 32], [2; 32], size)
            .unwrap();
        forward_owned(
            &state,
            completion(2, vec![4; size]),
            &inbound_tx,
            &backchannel_tx,
        );
        forward_owned(
            &state,
            LinkManagerAccountingEvent::LinkClosed { link_id: [1; 16] },
            &inbound_tx,
            &backchannel_tx,
        );
        assert!(matches!(
            state.reserve_attachment_transfer(size),
            Err(AttachmentTransferAdmissionError::Busy)
        ));
        let processing = inbound_rx.try_recv().unwrap();
        assert!(matches!(
            state.reserve_attachment_transfer(size),
            Err(AttachmentTransferAdmissionError::Busy)
        ));
        drop(processing);
        assert!(state.reserve_attachment_transfer(size).is_ok());

        state
            .admit_inbound_attachment_resource([1; 16], [3; 32], [3; 32], size)
            .unwrap();
        forward_owned(
            &state,
            completion(3, vec![4; size]),
            &inbound_tx,
            &backchannel_tx,
        );
        drop(inbound_rx); // session shutdown drops queued ownership, too
        assert!(state.reserve_attachment_transfer(size).is_ok());
    }

    #[test]
    fn closed_consumer_releases_payload_but_does_not_abandon_later_cleanup() {
        let (_root, state) = make_state();
        let size = rns_protocol::resource::MAX_EFFICIENT_SIZE + 1;
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let (backchannel_tx, mut backchannel_rx) = mpsc::unbounded_channel();
        drop(inbound_rx);
        state
            .admit_inbound_attachment_resource([1; 16], [2; 32], [2; 32], size)
            .unwrap();
        forward_owned(
            &state,
            completion(2, vec![4; size]),
            &inbound_tx,
            &backchannel_tx,
        );
        assert!(state.reserve_attachment_transfer(size).is_ok());
        state
            .admit_inbound_attachment_resource([1; 16], [3; 32], [3; 32], size)
            .unwrap();
        forward_owned(
            &state,
            LinkManagerAccountingEvent::ResourceEvent(LinkResourceEvent::Concluded {
                link_id: [1; 16],
                resource_id: [3; 32],
                direction: LinkResourceDirection::Inbound,
                conclusion: LinkResourceConclusion::Cancelled,
            }),
            &inbound_tx,
            &backchannel_tx,
        );
        assert!(state.reserve_attachment_transfer(size).is_ok());
        forward_owned(
            &state,
            LinkManagerAccountingEvent::LinkClosed { link_id: [1; 16] },
            &inbound_tx,
            &backchannel_tx,
        );
        assert!(matches!(
            backchannel_rx.try_recv().unwrap(),
            BackchannelLinkEvent::LinkClosed { .. }
        ));
    }

    #[test]
    fn another_links_rejection_cannot_release_live_resource_admission() {
        let (_root, state) = make_state();
        let size = rns_protocol::resource::MAX_EFFICIENT_SIZE + 1;
        let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel();
        let (backchannel_tx, _backchannel_rx) = mpsc::unbounded_channel();
        state
            .admit_inbound_attachment_resource([1; 16], [2; 32], [2; 32], size)
            .unwrap();
        forward_owned(
            &state,
            LinkManagerAccountingEvent::ResourceEvent(LinkResourceEvent::Concluded {
                link_id: [9; 16],
                resource_id: [2; 32],
                direction: LinkResourceDirection::Inbound,
                conclusion: LinkResourceConclusion::Rejected,
            }),
            &inbound_tx,
            &backchannel_tx,
        );
        assert!(matches!(
            state.reserve_attachment_transfer(size),
            Err(AttachmentTransferAdmissionError::Busy)
        ));
        forward_owned(
            &state,
            LinkManagerAccountingEvent::ResourceCompletion(ResourceCompletion {
                link_id: [9; 16],
                resource_hash: [2; 32],
                data: vec![9],
                metadata: None,
            }),
            &inbound_tx,
            &backchannel_tx,
        );
        assert!(inbound_rx.try_recv().is_err());
        assert!(matches!(
            state.reserve_attachment_transfer(size),
            Err(AttachmentTransferAdmissionError::Busy)
        ));
        forward_owned(
            &state,
            completion(2, vec![4; size]),
            &inbound_tx,
            &backchannel_tx,
        );
        drop(inbound_rx.try_recv().unwrap());
        assert!(state.reserve_attachment_transfer(size).is_ok());
    }
}
