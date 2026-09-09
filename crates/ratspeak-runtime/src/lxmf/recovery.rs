//! Application admission/observation state for transport-owned path recovery.
use super::*;
use rns_transport::messages::{TransportQuery, TransportQueryResponse};
use rns_transport::path_recovery::{PathRecoveryError, PathRecoveryHandle, PathRecoveryOutcome};

const MAX_PENDING_RECOVERIES: usize = 256;
const RECOVERY_WAIT_LIMIT: Duration = Duration::from_secs(10);
const OWNER_RETIREMENT_POLLS: usize = 16;
const OWNER_RETIREMENT_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(super) fn failure_invalidates_route(reason: &str) -> bool {
    matches!(
        reason,
        "link establishment timeout"
            | "link closed"
            | "delivery timeout"
            | "backchannel delivery timeout"
            | "resource advertisement timed out"
            | "resource part requests timed out"
            | "resource proof timed out"
            | "resource transfer timed out"
    )
}

pub(super) struct PendingPathRecovery {
    failed_attempt: Option<Attempt>,
    started_at: Instant,
    reply: Option<oneshot::Receiver<PathRecoveryOutcome>>,
    awaiting_snapshot: Option<Instant>,
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

fn invalidate_attempt(
    handle: &PathRecoveryHandle,
    dest: [u8; 16],
    attempt: Attempt,
) -> Result<oneshot::Receiver<PathRecoveryOutcome>, PathRecoveryError> {
    match attempt {
        Attempt::Packet(packet) => handle.try_invalidate_packet(dest, packet),
        Attempt::Link(link) => handle.try_invalidate_link(dest, link),
    }
}

// The external command deliberately has Python's destination-only semantics.
// The local actor first proves that this exact failed attempt used the unchanged
// route. Only that positive comparison permits an authenticated owner reset.
// There is no compare-and-swap guarantee for the remote table; a concurrently
// learned remote route can be discarded by the legacy operation.
async fn recover_shared_attempt(
    handle: PathRecoveryHandle,
    dest: [u8; 16],
    validation: oneshot::Receiver<PathRecoveryOutcome>,
    reset_owner: impl std::future::Future<Output = Result<(), &'static str>>,
) -> Result<PathRecoveryOutcome, &'static str> {
    let validated = validation.await.map_err(|_| "local_owner_closed")?;
    if validated.path_dropped {
        reset_owner.await?;
    } else if validated.has_path {
        return Ok(validated); // Unknown/consumed/changed attempt: preserve it.
    }
    // This operation's discovery must follow its reset. This does not cancel
    // other callers' pre-existing discovery or make the remote reset atomic.
    loop {
        match handle.try_recover(dest, None) {
            Ok(reply) => {
                let mut outcome = reply.await.map_err(|_| "local_owner_closed")?;
                outcome.path_dropped |= validated.path_dropped;
                return Ok(outcome);
            }
            Err(PathRecoveryError::Full) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(_) => return Err("local_owner_closed"),
        }
    }
}

fn owner_tombstone_pending(
    entries: &[PathTableRpcEntry],
    dest: [u8; 16],
) -> Result<bool, &'static str> {
    match entries.iter().find(|entry| entry.hash == dest) {
        None => Ok(false),
        Some(entry) if entry.timestamp == 0.0 => Ok(true),
        // A legacy owner cannot distinguish newly learned routes from traffic
        // touches. Either ends the tombstone wait, without proving freshness.
        Some(entry) if entry.timestamp.is_finite() && entry.timestamp > 0.0 => Ok(false),
        Some(_) => Err("shared_recovery_invalid_route_observation"),
    }
}

async fn await_owner_retirement(
    owner: &rns_runtime::reticulum::ReticulumHandle,
    dest: [u8; 16],
) -> Result<(), &'static str> {
    // Python expires to timestamp=0 and culls in a later maintenance turn.
    // Observe this one reset; never repeatedly issue DropPath. The enclosing
    // ten-second deadline and cancellation include every query and delay.
    for attempt in 0..OWNER_RETIREMENT_POLLS {
        let entries = owner
            .path_table(None)
            .await
            .map_err(|_| "shared_recovery_observation_unavailable")?;
        if !owner_tombstone_pending(&entries, dest)? {
            return Ok(());
        }
        if attempt + 1 < OWNER_RETIREMENT_POLLS {
            tokio::time::sleep(OWNER_RETIREMENT_POLL_INTERVAL).await;
        }
    }
    Err("shared_recovery_retirement_unobserved")
}

fn admit_with_owner(
    handle: &PathRecoveryHandle,
    owner: Option<&rns_runtime::reticulum::ReticulumHandle>,
    slots: &std::sync::Arc<tokio::sync::Semaphore>,
    dest: [u8; 16],
    attempt: Option<Attempt>,
) -> Result<oneshot::Receiver<PathRecoveryOutcome>, PathRecoveryError> {
    let (Some(owner), Some(attempt)) = (owner, attempt) else {
        return admit(handle, dest, attempt);
    };
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| PathRecoveryError::Closed)?;
    let permit = slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| PathRecoveryError::Full)?;
    let validation = invalidate_attempt(handle, dest, attempt)?;
    let handle = handle.clone();
    let owner = owner.clone();
    let reset = async move {
        use rns_runtime::reticulum::ControlError;
        use rns_runtime::shared_instance::SharedInstanceState;
        if owner
            .shared_instance_state()
            .is_some_and(|state| state != SharedInstanceState::Ready)
        {
            return Err("shared_owner_not_ready");
        }
        match owner
            .query_control_result(TransportQuery::DropPath { dest })
            .await
        {
            Ok(TransportQueryResponse::Ok) => {}
            Ok(_) => return Err("shared_recovery_unexpected_response"),
            Err(ControlError::RpcAuth) => return Err("shared_recovery_authentication"),
            Err(ControlError::UnsupportedBySharedInstance) => {
                return Err("shared_recovery_unsupported");
            }
            Err(_) => return Err("shared_recovery_unavailable"),
        }
        await_owner_retirement(&owner, dest).await
    };
    Ok(spawn_shared_recovery(
        runtime, handle, dest, validation, reset, permit,
    ))
}

fn spawn_shared_recovery(
    runtime: tokio::runtime::Handle,
    handle: PathRecoveryHandle,
    dest: [u8; 16],
    validation: oneshot::Receiver<PathRecoveryOutcome>,
    reset: impl std::future::Future<Output = Result<(), &'static str>> + Send + 'static,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> oneshot::Receiver<PathRecoveryOutcome> {
    let (mut tx, rx) = oneshot::channel();
    runtime.spawn(async move {
        let _permit = permit;
        tokio::select! {
            biased;
            _ = tx.closed() => {}
            result = tokio::time::timeout(RECOVERY_WAIT_LIMIT,
                recover_shared_attempt(handle, dest, validation, reset)) => {
                match result {
                    Ok(Ok(outcome)) => { let _ = tx.send(outcome); }
                    error => {
                        let reason = match error {
                            Ok(Err(reason)) => reason,
                            _ => "shared_recovery_timeout",
                        };
                        tracing::warn!(reason, "bounded shared path recovery failed");
                    }
                }
            }
        }
    });
    rx
}

impl LxmfManager {
    /// Install the active transport's bounded route-recovery owner. Replacing
    /// it cancels old pending operations and invalidates cached observations;
    /// a retired transport can never act on its replacement's path table.
    pub fn set_path_recovery_handle(&mut self, handle: PathRecoveryHandle) {
        self.path_recovery = Some(handle);
        self.shared_recovery_owner = None;
        self.shared_recovery_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        self.pending_path_recoveries.clear();
        self.route_entries.clear();
        self.route_hops.clear();
        self.route_snapshot_started = Instant::now();
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
                pending.reply = admit_with_owner(
                    handle,
                    self.shared_recovery_owner.as_ref(),
                    &self.shared_recovery_slots,
                    dest,
                    failed_attempt,
                )
                .ok();
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
            if let Some(started) = pending.awaiting_snapshot {
                if self.route_snapshot_started > started {
                    self.pending_path_recoveries.remove(&dest);
                }
                continue;
            }
            if pending.reply.is_none() {
                match admit_with_owner(
                    &handle,
                    self.shared_recovery_owner.as_ref(),
                    &self.shared_recovery_slots,
                    dest,
                    pending.failed_attempt,
                ) {
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
                        pending.awaiting_snapshot = Some(Instant::now());
                    } else {
                        self.pending_path_recoveries.remove(&dest);
                    }
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.pending_path_recoveries.remove(&dest);
                    self.path_recovery_refresh_needed = true;
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
    use std::sync::atomic::Ordering;

    #[test]
    fn only_network_failures_authorize_exact_route_invalidation() {
        for reason in [
            "delivery timeout",
            "backchannel delivery timeout",
            "resource advertisement timed out",
            "resource part requests timed out",
            "resource proof timed out",
            "resource transfer timed out",
            "link closed",
        ] {
            assert!(failure_invalidates_route(reason), "{reason}");
        }
        for reason in [
            "transport full",
            "backchannel send command timeout",
            "resource cancelled",
            "resource transfer cancelled",
            "cancelled",
            "rejected",
            "Link endpoint binding failed",
            "shared_recovery_authentication",
        ] {
            assert!(!failure_invalidates_route(reason), "{reason}");
        }
    }

    #[test]
    fn python_reset_tombstone_wait_is_not_a_freshness_claim() {
        let dest = [0x57; 16];
        let mut entry = PathTableRpcEntry {
            hash: dest,
            timestamp: 0.0,
            via: None,
            hops: 1,
            expires: 1000.0,
            interface: "inert owner route".into(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        };
        assert_eq!(owner_tombstone_pending(&[], dest), Ok(false));
        assert_eq!(
            owner_tombstone_pending(std::slice::from_ref(&entry), dest),
            Ok(true)
        );
        entry.timestamp = 12.0;
        assert_eq!(
            owner_tombstone_pending(std::slice::from_ref(&entry), dest),
            Ok(false)
        );
        entry.timestamp = f64::NAN;
        assert!(owner_tombstone_pending(std::slice::from_ref(&entry), dest).is_err());
        entry.timestamp = -1.0;
        assert!(owner_tombstone_pending(std::slice::from_ref(&entry), dest).is_err());
    }

    struct SharedFixture {
        handle: PathRecoveryHandle,
        transport: mpsc::Sender<TransportMessage>,
        task: tokio::task::JoinHandle<()>,
        radio: mpsc::Receiver<bytes::Bytes>,
        dest: [u8; 16],
        packet: [u8; 32],
    }

    impl SharedFixture {
        async fn new() -> Self {
            use rns_transport::{constants::*, messages::*, path_table::PathEntry};
            use rns_wire::flags::*;
            let (mut actor, transport) = rns_transport::actor::TransportActor::new();
            let handle = actor.path_recovery_handle();
            let (radio_tx, mut radio) = mpsc::channel(8);
            actor.interfaces.insert(
                1,
                InterfaceEntry::new(
                    "inert owner IPC".into(),
                    InterfaceMode::Full,
                    InterfaceDirection::bidirectional(),
                    1_000_000,
                    500,
                    radio_tx,
                ),
            );
            let dest = [0xD3; 16];
            let mut path = PathEntry::new(None, 1, 1, InterfaceMode::Full);
            path.packet_hash = Some([0x42; 32]);
            actor.path_table.insert(dest, path);
            let task = tokio::spawn(actor.run());
            let raw = rns_wire::header::PacketHeader {
                flags: PacketFlags {
                    header_type: HeaderType::Header1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    destination_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id: None,
                destination_hash: dest,
                context: rns_wire::context::PacketContext::None,
            }
            .pack();
            let (packet, truncated_hash) =
                rns_wire::hash::packet_hash_pair(&raw, HeaderType::Header1);
            let (status_tx, _status) = tokio::sync::watch::channel(ReceiptUpdate::Sent);
            let (result_tx, result) = oneshot::channel();
            transport
                .send(TransportMessage::SendPacket {
                    request: OutboundRequest {
                        raw: raw.into(),
                        destination_hash: dest,
                    },
                    attached_interface: None,
                    receipt: Some(TrackedReceiptRegistration {
                        truncated_hash,
                        full_hash: packet,
                        destination_hash: dest,
                        destination_public_key: [0; 64],
                        timeout: Some(Duration::from_secs(120)),
                        status_tx,
                    }),
                    result_tx,
                })
                .await
                .unwrap();
            assert_eq!(result.await.unwrap(), OutboundDispatchResult::Sent);
            radio.recv().await.unwrap();
            Self {
                handle,
                transport,
                task,
                radio,
                dest,
                packet,
            }
        }

        fn recover(
            &self,
            reset: impl std::future::Future<Output = Result<(), &'static str>> + Send + 'static,
        ) -> oneshot::Receiver<PathRecoveryOutcome> {
            self.recover_attempt(Attempt::Packet(self.packet), reset)
        }

        fn recover_attempt(
            &self,
            attempt: Attempt,
            reset: impl std::future::Future<Output = Result<(), &'static str>> + Send + 'static,
        ) -> oneshot::Receiver<PathRecoveryOutcome> {
            let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
            spawn_shared_recovery(
                tokio::runtime::Handle::current(),
                self.handle.clone(),
                self.dest,
                invalidate_attempt(&self.handle, self.dest, attempt).unwrap(),
                reset,
                slots.try_acquire_owned().unwrap(),
            )
        }

        async fn observe_link_attempt(&mut self) -> [u8; 16] {
            use rns_wire::flags::*;
            let (link, payload) = rns_link::link::Link::new_initiator(self.dest, 1);
            self.transport
                .send(TransportMessage::RegisterDestination {
                    hash: link.link_id,
                    app_name: "recovery fixture".into(),
                    delivery_tx: None,
                })
                .await
                .unwrap();
            let mut raw = rns_wire::header::PacketHeader {
                flags: PacketFlags {
                    header_type: HeaderType::Header1,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    destination_type: DestinationType::Single,
                    packet_type: PacketType::LinkRequest,
                },
                hops: 0,
                transport_id: None,
                destination_hash: self.dest,
                context: rns_wire::context::PacketContext::None,
            }
            .pack();
            raw.extend_from_slice(&payload);
            self.transport
                .send(TransportMessage::Outbound(
                    rns_transport::messages::OutboundRequest {
                        raw: raw.into(),
                        destination_hash: self.dest,
                    },
                ))
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), self.radio.recv())
                .await
                .unwrap()
                .unwrap();
            link.link_id
        }

        async fn stop(self) {
            self.transport
                .send(TransportMessage::Shutdown)
                .await
                .unwrap();
            self.task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn shared_reset_precedes_discovery_and_consumed_packet_cannot_reset_twice() {
        let mut fixture = SharedFixture::new().await;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let (release, released) = oneshot::channel();
        let reply = fixture.recover(async move {
            observed.fetch_add(1, Ordering::SeqCst);
            released.await.unwrap();
            Ok(())
        });
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            fixture.radio.try_recv().is_err(),
            "no query of the owner's stale cache before reset"
        );
        release.send(()).unwrap();
        let result = reply.await.unwrap();
        assert!(result.path_dropped && result.request_scheduled);
        tokio::time::timeout(Duration::from_secs(2), fixture.radio.recv())
            .await
            .unwrap()
            .unwrap();
        let again = fixture.recover(async { panic!("consumed packet cannot reset owner twice") });
        assert!(!again.await.unwrap().path_dropped);
        fixture.stop().await;
    }

    #[tokio::test]
    async fn shared_auth_failure_does_not_fall_back_to_local_discovery() {
        let mut fixture = SharedFixture::new().await;
        assert!(
            fixture
                .recover(async { Err("shared_recovery_authentication") })
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(1100), fixture.radio.recv())
                .await
                .is_err()
        );
        fixture.stop().await;
    }

    #[tokio::test]
    async fn shared_link_reset_precedes_discovery_and_consumed_attempt_cannot_reset_twice() {
        let mut fixture = SharedFixture::new().await;
        let link = fixture.observe_link_attempt().await;
        let (release, released) = oneshot::channel();
        let (entered, entry) = oneshot::channel();
        let reply = fixture.recover_attempt(Attempt::Link(link), async move {
            entered.send(()).unwrap();
            released.await.unwrap();
            Ok(())
        });
        tokio::time::timeout(Duration::from_secs(2), entry)
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(1100), fixture.radio.recv())
                .await
                .is_err(),
            "Link recovery cannot query the owner's stale cache before reset"
        );
        release.send(()).unwrap();
        let result = reply.await.unwrap();
        assert!(result.path_dropped && result.request_scheduled);
        tokio::time::timeout(Duration::from_secs(2), fixture.radio.recv())
            .await
            .unwrap()
            .unwrap();
        let again = fixture.recover_attempt(Attempt::Link(link), async {
            panic!("consumed Link cannot reset twice")
        });
        assert!(!again.await.unwrap().path_dropped);
        fixture.stop().await;
    }

    #[tokio::test]
    async fn unknown_shared_link_preserves_route_and_link_reset_auth_failure_cannot_discover() {
        let mut fixture = SharedFixture::new().await;
        let unknown = fixture
            .recover_attempt(Attempt::Link([0x98; 16]), async {
                panic!("unknown Link cannot reset")
            })
            .await
            .unwrap();
        assert!(!unknown.path_dropped && unknown.has_path && !unknown.request_scheduled);
        let link = fixture.observe_link_attempt().await;
        assert!(
            fixture
                .recover_attempt(Attempt::Link(link), async {
                    Err("shared_recovery_authentication")
                })
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(1100), fixture.radio.recv())
                .await
                .is_err()
        );
        fixture.stop().await;
    }

    #[tokio::test]
    async fn unknown_packet_preserves_local_route_and_never_resets_owner() {
        let mut fixture = SharedFixture::new().await;
        fixture.packet = [0x99; 32];
        let result = fixture
            .recover(async { panic!("unobserved packet must not reset owner") })
            .await
            .unwrap();
        assert!(!result.path_dropped && result.has_path && !result.request_scheduled);
        assert!(fixture.radio.try_recv().is_err());
        fixture.stop().await;
    }

    #[tokio::test]
    async fn cancelled_shared_recovery_cannot_initiate_reset_or_discovery() {
        let mut fixture = SharedFixture::new().await;
        let reply = fixture.recover(async { panic!("cancelled operation must not reset owner") });
        drop(reply); // Biased cancellation runs before polling either operation.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(fixture.radio.try_recv().is_err());
        // The actor may have completed local validation before observing the
        // cancellation, but no remote side effect or discovery is permitted.
        fixture.stop().await;
    }

    #[tokio::test]
    async fn stalled_shared_reset_expires_without_discovery() {
        let mut fixture = SharedFixture::new().await;
        let result = tokio::time::timeout(
            RECOVERY_WAIT_LIMIT + Duration::from_secs(1),
            fixture.recover(std::future::pending()),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(fixture.radio.try_recv().is_err());
        fixture.stop().await;
    }

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
        let old_poll_started = Instant::now();
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
        mgr.replace_routes_observed_at(&[], old_poll_started);
        mgr.poll_path_recoveries();
        assert!(
            mgr.pending_path_recoveries.contains_key(&dest),
            "a poll begun before recovery cannot unblock retries"
        );
        mgr.replace_route_hops_from_path_table(&[]);
        mgr.poll_path_recoveries();
        assert!(mgr.pending_path_recoveries.is_empty());
        tx.send(TransportMessage::Shutdown).await.unwrap();
        task.await.unwrap();
    }
}
