//! Exact packet proof ownership, independent of the router's retry queue.
use super::*;
use rns_transport::messages::{OutboundDispatchResult, ReceiptUpdate, TrackedReceiptRegistration};
use tokio::sync::watch;

const MAX_PROOF_ATTEMPTS: usize = 512;
const LATE_PROOF_GRACE: Duration = Duration::from_secs(120);

pub(super) struct ProofOwner {
    pub(super) message: LxMessage,
    attempts: Vec<PacketAttempt>,
}

struct PacketAttempt {
    packet_hash: [u8; 32],
    status: watch::Receiver<ReceiptUpdate>,
    dispatch: Option<oneshot::Receiver<OutboundDispatchResult>>,
    started: Instant,
    lifetime: Duration,
}

impl LxmfManager {
    /// The outer orphan watchdog must not overrule a bounded protocol clock.
    /// Resource transfer limits remain independent; only an active Link's
    /// establishment clock and retained packet proof windows qualify here.
    pub(crate) fn has_bounded_protocol_wait(&self, msg_id: &str) -> bool {
        let Ok(bytes) = hex::decode(msg_id) else {
            return false;
        };
        let Ok(hash) = <[u8; 32]>::try_from(bytes.as_slice()) else {
            return false;
        };
        if self.opportunistic_proofs.get(&hash).is_some_and(|owner| {
            owner.attempts.iter().any(|attempt| {
                attempt.started.elapsed() < attempt.lifetime
                    && matches!(
                        *attempt.status.borrow(),
                        ReceiptUpdate::Sent | ReceiptUpdate::Delivered { .. }
                    )
            })
        }) {
            return true;
        }
        self.link_delivery
            .as_ref()
            .and_then(|delivery| delivery.message_delivery_snapshot(hash))
            .is_some_and(|snapshot| {
                !snapshot.queued && snapshot.delivery_state == DeliveryState::Establishing
            })
    }
    pub(crate) fn take_packet_delivery_rtts(&mut self) -> Vec<(String, Duration)> {
        std::mem::take(&mut self.packet_delivery_rtts)
    }
    pub(super) fn dispatch_opportunistic_packet(
        &mut self,
        message: &LxMessage,
        raw: Vec<u8>,
        public_key: [u8; 64],
    ) -> Result<[u8; 32], &'static str> {
        let Some(tx) = self.router.transport_tx.as_ref() else {
            return Err("transport_unavailable");
        };
        let Some(hash) = message.hash else {
            return Err("missing_message_hash");
        };
        if self
            .opportunistic_proofs
            .values()
            .map(|owner| owner.attempts.len())
            .sum::<usize>()
            >= MAX_PROOF_ATTEMPTS
        {
            return Err("proof_capacity");
        }
        let lifetime = self
            .packet_retry_window(message.destination_hash)
            .saturating_add(LATE_PROOF_GRACE);
        let (full_hash, truncated_hash) =
            rns_wire::hash::packet_hash_pair(&raw, rns_wire::flags::HeaderType::Header1);
        let (status_tx, status) = watch::channel(ReceiptUpdate::Sent);
        let (result_tx, dispatch) = oneshot::channel();
        tx.try_send(TransportMessage::SendPacket {
            request: rns_transport::messages::OutboundRequest {
                raw: Bytes::from(raw),
                destination_hash: message.destination_hash,
            },
            attached_interface: None,
            receipt: Some(TrackedReceiptRegistration {
                truncated_hash,
                full_hash,
                destination_hash: message.destination_hash,
                destination_public_key: public_key,
                timeout: Some(lifetime),
                status_tx,
            }),
            result_tx,
        })
        .map_err(|_| "backpressure")?;
        let owner = self
            .opportunistic_proofs
            .entry(hash)
            .or_insert_with(|| ProofOwner {
                message: message.clone(),
                attempts: Vec::new(),
            });
        owner.message = message.clone();
        owner.attempts.push(PacketAttempt {
            packet_hash: full_hash,
            status,
            dispatch: Some(dispatch),
            started: Instant::now(),
            lifetime,
        });
        Ok(full_hash)
    }

    pub(super) fn last_failed_packet(&self, hash: [u8; 32]) -> Option<[u8; 32]> {
        self.opportunistic_proofs
            .get(&hash)?
            .attempts
            .last()
            .map(|attempt| attempt.packet_hash)
    }

    pub(super) fn poll_opportunistic_proofs(&mut self, results: &mut Vec<(String, &'static str)>) {
        self.packet_delivery_rtts.clear();
        let mut delivered = Vec::new();
        let mut rejected = Vec::new();
        for (hash, owner) in &mut self.opportunistic_proofs {
            let mut completed = None;
            let retry_window = owner
                .attempts
                .last()
                .map(|attempt| attempt.lifetime.saturating_sub(LATE_PROOF_GRACE));
            owner.attempts.retain_mut(|attempt| {
                // Sample terminal proof before expiry or a queued dispatch ACK.
                if let ReceiptUpdate::Delivered { rtt } = *attempt.status.borrow() {
                    completed.get_or_insert(rtt);
                    return true;
                }
                if let Some(dispatch) = &mut attempt.dispatch {
                    match dispatch.try_recv() {
                        Ok(OutboundDispatchResult::Sent) => {
                            attempt.dispatch = None;
                            if let Some(pending) = self
                                .opportunistic_in_flight
                                .get_mut(hash)
                                .filter(|pending| pending.packet_hash == attempt.packet_hash)
                            {
                                let now = SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs_f64();
                                pending.retry_at =
                                    now + retry_window.unwrap_or_default().as_secs_f64();
                                pending.message.next_delivery_attempt = pending.retry_at;
                            }
                        }
                        Err(oneshot::error::TryRecvError::Empty) => {}
                        _ => {
                            rejected.push((*hash, attempt.packet_hash));
                            return false;
                        }
                    }
                }
                if matches!(
                    *attempt.status.borrow(),
                    ReceiptUpdate::TimedOut | ReceiptUpdate::Failed | ReceiptUpdate::Culled
                ) {
                    if let Some(pending) = self
                        .opportunistic_in_flight
                        .get_mut(hash)
                        .filter(|pending| pending.packet_hash == attempt.packet_hash)
                    {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64();
                        pending.retry_at = now;
                        pending.message.next_delivery_attempt = now;
                    }
                }
                // A concluded receipt can no longer prove delivery, but its
                // exact failed-route identity remains useful to the next
                // discovery step. Keep only bounded metadata/message ownership.
                attempt.started.elapsed() < attempt.lifetime
            });
            if let Some(rtt) = completed {
                delivered.push((*hash, rtt));
            }
        }
        for (hash, rtt) in delivered {
            let ephemeral = self.ephemeral_outbound.contains(&hash);
            if self.complete_opportunistic_delivery(&hex::encode(hash)) && !ephemeral {
                results.push((hex::encode(hash), "delivered"));
                self.packet_delivery_rtts.push((hex::encode(hash), rtt));
            }
        }
        for (hash, packet_hash) in rejected {
            // Only the most recent in-flight owner may be refunded. An old
            // receipt failure must not cancel or reschedule a newer attempt.
            if self
                .opportunistic_in_flight
                .get(&hash)
                .is_none_or(|pending| pending.packet_hash != packet_hash)
            {
                continue;
            }
            if let Some(mut pending) = self.opportunistic_in_flight.remove(&hash) {
                pending.message.delivery_attempts =
                    pending.message.delivery_attempts.saturating_sub(1);
                pending.message.next_delivery_attempt = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
                    + 0.5;
                self.queue_router_message(pending.message, "packet interface admission");
                results.push((hex::encode(hash), "routing"));
            }
        }
        self.opportunistic_proofs
            .retain(|_, owner| !owner.attempts.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lxmf::tests::test_manager;

    fn packet() -> (LxmfManager, [u8; 32], mpsc::Receiver<TransportMessage>) {
        let mut mgr = test_manager();
        let dest = [0xDD; 16];
        mgr.known_identities
            .insert(hex::encode(dest), Identity::new().get_public_key());
        mgr.router.set_stamp_cost(dest, 0);
        let (tx, rx) = mpsc::channel(64);
        mgr.router.set_transport(tx);
        let mut message = mgr
            .create_message(
                &hex::encode(dest),
                "probe",
                "",
                DeliveryMethod::Opportunistic,
            )
            .unwrap();
        message.outbound_ticket = None;
        let hash = message.hash.unwrap();
        mgr.router.try_send(message).unwrap();
        mgr.tick();
        (mgr, hash, rx)
    }

    fn accept(rx: &mut mpsc::Receiver<TransportMessage>) -> watch::Sender<ReceiptUpdate> {
        match rx.try_recv().unwrap() {
            TransportMessage::SendPacket {
                receipt: Some(receipt),
                result_tx,
                ..
            } => {
                result_tx.send(OutboundDispatchResult::Sent).unwrap();
                receipt.status_tx
            }
            _ => panic!("expected atomic tracked packet"),
        }
    }

    #[test]
    fn late_proof_completes_queued_retry_once_and_stops_fallback() {
        let (mut mgr, hash, mut rx) = packet();
        let proof = accept(&mut rx);
        mgr.poll_opportunistic_proofs(&mut Vec::new());
        let mut updates = Vec::new();
        mgr.retry_due_opportunistic_deliveries(f64::MAX, &mut updates);
        assert!(mgr.opportunistic_in_flight.is_empty());
        assert!(mgr.has_bounded_protocol_wait(&hex::encode(hash)));
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        mgr.auto_live_fallback.insert(hash);
        proof.send_replace(ReceiptUpdate::Delivered {
            rtt: Duration::from_secs(13),
        });
        mgr.poll_opportunistic_proofs(&mut updates);
        assert_eq!(updates, vec![(hex::encode(hash), "delivered")]);
        assert_eq!(
            mgr.take_packet_delivery_rtts(),
            vec![(hex::encode(hash), Duration::from_secs(13))]
        );
        assert!(mgr.router.pending_outbound.is_empty());
        assert!(!mgr.auto_live_fallback.contains(&hash));
        assert!(mgr.opportunistic_proofs.is_empty());
        assert!(!mgr.has_bounded_protocol_wait(&hex::encode(hash)));
        mgr.poll_opportunistic_proofs(&mut updates);
        assert_eq!(updates.len(), 1);
    }

    #[test]
    fn cancelled_and_retried_same_message_cannot_accept_old_attempt_proof() {
        let (mut mgr, hash, mut rx) = packet();
        let old_proof = accept(&mut rx);
        let mut message = mgr.opportunistic_proofs[&hash].message.clone();
        assert!(mgr.cancel_outbound_message(&hex::encode(hash)));
        message.next_delivery_attempt = 0.0;
        message.delivery_attempts = 0;
        mgr.execute_encrypted_actions(vec![OutboundAction::DeliverOpportunistic {
            dest_hash: message.destination_hash,
            message,
        }]);
        let new_proof = accept(&mut rx);
        old_proof.send_replace(ReceiptUpdate::Delivered {
            rtt: Duration::from_secs(1),
        });
        let mut updates = Vec::new();
        mgr.poll_opportunistic_proofs(&mut updates);
        assert!(updates.is_empty());
        assert!(mgr.opportunistic_in_flight.contains_key(&hash));
        new_proof.send_replace(ReceiptUpdate::Delivered {
            rtt: Duration::from_secs(1),
        });
        mgr.poll_opportunistic_proofs(&mut updates);
        assert_eq!(updates, vec![(hex::encode(hash), "delivered")]);
    }

    #[test]
    fn interface_rejection_refunds_attempt_and_terminal_retention_is_bounded() {
        let (mut mgr, hash, mut rx) = packet();
        if let TransportMessage::SendPacket { result_tx, .. } = rx.try_recv().unwrap() {
            result_tx.send(OutboundDispatchResult::NoInterface).unwrap();
        } else {
            panic!("expected tracked packet");
        }
        mgr.poll_opportunistic_proofs(&mut Vec::new());
        assert_eq!(mgr.router.pending_outbound[0].delivery_attempts, 0);
        assert!(mgr.opportunistic_proofs.is_empty());
        assert!(!mgr.opportunistic_in_flight.contains_key(&hash));

        let (mut mgr, hash, mut rx) = packet();
        let _proof = accept(&mut rx);
        mgr.poll_opportunistic_proofs(&mut Vec::new());
        let attempt = &mut mgr.opportunistic_proofs.get_mut(&hash).unwrap().attempts[0];
        attempt.started = Instant::now() - attempt.lifetime;
        mgr.poll_opportunistic_proofs(&mut Vec::new());
        assert!(mgr.opportunistic_proofs.is_empty());
        assert!(!mgr.complete_opportunistic_delivery(&hex::encode(hash)));
    }

    #[test]
    fn packet_window_includes_slow_first_hop_and_zero_hop_floor() {
        assert_eq!(
            rns_wire::receipt::receipt_timeout_for_route(Duration::from_secs(46), 3),
            Duration::from_secs(64)
        );
        assert_eq!(
            rns_wire::receipt::receipt_timeout_for_route(Duration::from_secs(6), 0),
            Duration::from_secs(6)
        );
        let mgr = test_manager();
        assert_eq!(mgr.packet_retry_window([1; 16]), Duration::from_secs(180));
    }

    #[test]
    fn culled_receipt_retries_promptly_without_losing_exact_failed_route_owner() {
        let (mut mgr, hash, mut rx) = packet();
        let proof = accept(&mut rx);
        mgr.poll_opportunistic_proofs(&mut Vec::new());
        let packet_hash = mgr.opportunistic_in_flight[&hash].packet_hash;
        proof.send_replace(ReceiptUpdate::Culled);
        mgr.poll_opportunistic_proofs(&mut Vec::new());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert!(mgr.opportunistic_in_flight[&hash].retry_at <= now);
        assert_eq!(mgr.last_failed_packet(hash), Some(packet_hash));
        assert!(!mgr.has_bounded_protocol_wait(&hex::encode(hash)));
        assert_eq!(
            mgr.opportunistic_in_flight[&hash].message.delivery_attempts,
            1
        );
    }
}
