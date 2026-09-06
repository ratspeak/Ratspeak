//! Application admission/observation state for transport-owned path recovery.
use super::*;
use rns_transport::path_recovery::{PathRecoveryError, PathRecoveryHandle, PathRecoveryOutcome};

const MAX_PENDING_RECOVERIES: usize = 256;
const RECOVERY_WAIT_LIMIT: Duration = Duration::from_secs(10);

pub(super) struct PendingPathRecovery {
    failed_attempt: Option<Attempt>,
    started_at: Instant,
    reply: Option<oneshot::Receiver<PathRecoveryOutcome>>,
    awaiting_snapshot: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Attempt {
    Link([u8; 16]),
    Packet([u8; 32]),
}

fn admit(
    handle: &PathRecoveryHandle,
    dest: [u8; 16],
    attempt: Option<Attempt>,
) -> Result<oneshot::Receiver<PathRecoveryOutcome>, PathRecoveryError> {
    match attempt {
        Some(Attempt::Packet(hash)) => handle.try_recover_packet(dest, hash),
        Some(Attempt::Link(link)) => handle.try_recover(dest, Some(link)),
        None => handle.try_recover(dest, None),
    }
}

impl LxmfManager {
    /// Install the active transport's bounded route-recovery owner. Replacing
    /// it cancels old pending operations and invalidates cached observations;
    /// a retired transport can never act on its replacement's path table.
    pub fn set_path_recovery_handle(&mut self, handle: PathRecoveryHandle) {
        self.path_recovery = Some(handle);
        self.pending_path_recoveries.clear();
        self.route_entries.clear();
        self.route_hops.clear();
        self.path_recovery_refresh_needed = true;
    }

    pub(crate) fn take_path_recovery_refresh(&mut self) -> bool {
        std::mem::take(&mut self.path_recovery_refresh_needed)
    }

    pub(super) fn request_path_recovery(&mut self, dest: [u8; 16], failed_link: Option<[u8; 16]>) {
        self.request_path_attempt_recovery(dest, failed_link.map(Attempt::Link));
    }

    pub(super) fn request_packet_path_recovery(
        &mut self,
        dest: [u8; 16],
        packet: Option<[u8; 32]>,
    ) {
        self.request_path_attempt_recovery(dest, packet.map(Attempt::Packet));
    }

    fn request_path_attempt_recovery(&mut self, dest: [u8; 16], failed_attempt: Option<Attempt>) {
        if self.path_recovery.is_none() {
            // Retained embedding API: callers using only a raw mailbox may
            // discover, but cannot safely invalidate an unobserved route.
            // Never reintroduce unconditional DropPath/interface suppression.
            if let Some(tx) = &self.router.transport_tx {
                let _ = tx.try_send(TransportMessage::RequestPath {
                    destination_hash: dest,
                });
            }
            return;
        }
        if let Some(pending) = self.pending_path_recoveries.get_mut(&dest) {
            if failed_attempt.is_some() && pending.failed_attempt != failed_attempt {
                pending.failed_attempt = failed_attempt;
                pending.reply = None;
                pending.awaiting_snapshot = None;
            }
            return;
        }
        if self.pending_path_recoveries.len() >= MAX_PENDING_RECOVERIES {
            tracing::warn!(
                reason = "recovery_capacity",
                "path recovery admission deferred"
            );
            return;
        }
        self.pending_path_recoveries.insert(
            dest,
            PendingPathRecovery {
                failed_attempt,
                started_at: Instant::now(),
                reply: None,
                awaiting_snapshot: None,
            },
        );
        // Admit only this new destination here. The normal tick polls the
        // bounded inventory once; enqueueing a batch must not rescan it O(n²).
        if let Some(handle) = &self.path_recovery {
            if let Some(pending) = self.pending_path_recoveries.get_mut(&dest) {
                pending.reply = admit(handle, dest, failed_attempt).ok();
            }
        }
    }

    pub(super) fn poll_path_recoveries(&mut self) {
        let Some(handle) = self.path_recovery.clone() else {
            return;
        };
        let destinations = self
            .pending_path_recoveries
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for dest in destinations {
            let pending = self
                .pending_path_recoveries
                .get_mut(&dest)
                .expect("pending recovery");
            if pending.started_at.elapsed() >= RECOVERY_WAIT_LIMIT {
                self.pending_path_recoveries.remove(&dest);
                self.route_entries.remove(&dest);
                self.route_hops.remove(&dest);
                self.path_recovery_refresh_needed = true;
                tracing::warn!(
                    reason = "recovery_wait_timeout",
                    "path recovery observation timed out"
                );
                continue;
            }
            if let Some(revision) = pending.awaiting_snapshot {
                if revision != self.route_snapshot_revision {
                    self.pending_path_recoveries.remove(&dest);
                }
                continue;
            }
            if pending.reply.is_none() {
                match admit(&handle, dest, pending.failed_attempt) {
                    Ok(reply) => pending.reply = Some(reply),
                    Err(PathRecoveryError::Full) => continue,
                    Err(_) => {
                        self.pending_path_recoveries.remove(&dest);
                        continue;
                    }
                }
            }
            match pending
                .reply
                .as_mut()
                .expect("admitted recovery")
                .try_recv()
            {
                Ok(result) => {
                    self.path_recovery_refresh_needed = true;
                    if result.path_dropped || !result.has_path {
                        // These are only app observations. The atomic actor
                        // operation already preserved any newer real route.
                        self.route_entries.remove(&dest);
                        self.route_hops.remove(&dest);
                        pending.reply = None;
                        pending.awaiting_snapshot = Some(self.route_snapshot_revision);
                    } else {
                        self.pending_path_recoveries.remove(&dest);
                    }
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.pending_path_recoveries.remove(&dest);
                }
            }
        }
    }

    pub(super) fn hold_messages_for_path_recovery(&mut self, now: f64) {
        // Discovery retries retain upstream's bounded attempt policy. Local
        // admission/observation wait, however, is not another delivery attempt.
        for message in &mut self.router.pending_outbound {
            if self
                .pending_path_recoveries
                .contains_key(&message.destination_hash)
                && message.next_delivery_attempt <= now
            {
                message.next_delivery_attempt = now + 0.5;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lxmf::tests::test_manager;

    #[test]
    fn recovery_backpressure_does_not_spend_delivery_attempts_and_wait_is_bounded() {
        let (mut actor, _tx) = rns_transport::actor::TransportActor::new();
        let handle = actor.path_recovery_handle();
        let mut occupied = Vec::new();
        while let Ok(reply) = handle.try_recover([0xAA; 16], None) {
            occupied.push(reply);
        }
        let mut mgr = test_manager();
        mgr.set_path_recovery_handle(handle);
        let dest = [0xDD; 16];
        let mut message = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "",
            "probe",
            DeliveryMethod::Direct,
        );
        message.delivery_attempts = 1;
        message.next_delivery_attempt = 0.0;
        mgr.router.pending_outbound.push(message);
        mgr.request_path_recovery(dest, Some([0x11; 16]));
        for now in [10.0, 20.0, 30.0] {
            mgr.poll_path_recoveries();
            mgr.hold_messages_for_path_recovery(now);
            assert_eq!(mgr.router.pending_outbound[0].delivery_attempts, 1);
            assert!(mgr.router.pending_outbound[0].next_delivery_attempt > now);
        }
        mgr.pending_path_recoveries
            .get_mut(&dest)
            .unwrap()
            .started_at = Instant::now() - RECOVERY_WAIT_LIMIT;
        mgr.poll_path_recoveries();
        assert!(mgr.pending_path_recoveries.is_empty());
        assert!(mgr.take_path_recovery_refresh());
    }

    #[test]
    fn pending_recovery_inventory_is_bounded_and_transport_replacement_cancels_it() {
        let (mut actor, _tx) = rns_transport::actor::TransportActor::new();
        let mut mgr = test_manager();
        mgr.set_path_recovery_handle(actor.path_recovery_handle());
        for value in 0..MAX_PENDING_RECOVERIES + 1 {
            let mut dest = [0; 16];
            dest[..8].copy_from_slice(&(value as u64).to_le_bytes());
            mgr.request_path_recovery(dest, None);
        }
        assert_eq!(mgr.pending_path_recoveries.len(), MAX_PENDING_RECOVERIES);
        mgr.set_path_recovery_handle(actor.path_recovery_handle());
        assert!(mgr.pending_path_recoveries.is_empty());
    }

    #[tokio::test]
    async fn recovery_completion_waits_for_a_new_authoritative_snapshot() {
        let (mut actor, tx) = rns_transport::actor::TransportActor::new();
        let mut mgr = test_manager();
        mgr.set_path_recovery_handle(actor.path_recovery_handle());
        let task = tokio::spawn(actor.run());
        let dest = [0xDD; 16];
        mgr.request_path_recovery(dest, None);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                mgr.poll_path_recoveries();
                if mgr
                    .pending_path_recoveries
                    .get(&dest)
                    .is_some_and(|entry| entry.awaiting_snapshot.is_some())
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(mgr.take_path_recovery_refresh());
        assert!(mgr.pending_path_recoveries.contains_key(&dest));
        mgr.replace_route_hops_from_path_table(&[]);
        mgr.poll_path_recoveries();
        assert!(mgr.pending_path_recoveries.is_empty());
        tx.send(TransportMessage::Shutdown).await.unwrap();
        task.await.unwrap();
    }
}
