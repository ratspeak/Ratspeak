//! Bounded response ownership on the existing LXMF inbound task. Waiting for
//! durable announce ordering or transport admission must not block ingestion.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rns_runtime::lifecycle::ShutdownSignal;
use rns_transport::messages::TransportMessage;
use tokio::sync::mpsc::{Sender, error::TrySendError};

use crate::state::AppState;

pub(crate) const RETRY_INTERVAL: Duration = Duration::from_millis(250);
const COALESCED_RETRY: Duration = Duration::from_secs(1);
const REQUEST_LIFETIME: Duration = Duration::from_secs(30);
const MAX_PENDING: usize = 16;

#[derive(Debug)]
pub(crate) enum BuildError {
    Coalesced,
    Busy,
    Failed,
}

struct PendingResponse {
    identity_generation: u64,
    transport: Sender<TransportMessage>,
    runtime_shutdown: ShutdownSignal,
    attached_interface: Option<u64>,
    tag: Option<Vec<u8>>,
    expires: Instant,
    next_attempt: Instant,
    // Once built, these bytes remain immutable through transport pressure.
    message: Option<TransportMessage>,
}

impl PendingResponse {
    fn is_current(&self, state: &AppState) -> bool {
        !self.runtime_shutdown.is_triggered()
            && !self.transport.is_closed()
            && state.current_identity_session_generation() == self.identity_generation
            && state.rns.read().ok().is_some_and(|rns| {
                rns.as_ref().is_some_and(|manager| {
                    manager.handle.transport_tx.same_channel(&self.transport)
                })
            })
    }
}

#[derive(Default)]
pub(crate) struct PendingPathResponses {
    requests: VecDeque<PendingResponse>,
}

impl PendingPathResponses {
    pub(crate) fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    pub(crate) fn admit(
        &mut self,
        state: &Arc<AppState>,
        attached_interface: Option<u64>,
        tag: Option<Vec<u8>>,
        now: Instant,
    ) {
        self.requests
            .retain(|request| now < request.expires && request.is_current(state));
        // A new inbound task can start before its initialization guard is
        // released. Admit its work now and defer service until that transition
        // ends; an Activity fence would incorrectly discard first contact here.
        let identity_generation = state.current_identity_session_generation();
        let Some((transport, runtime_shutdown)) = state.rns.read().ok().and_then(|rns| {
            rns.as_ref().map(|manager| {
                (
                    manager.handle.transport_tx.clone(),
                    manager.handle.shutdown.clone(),
                )
            })
        }) else {
            return;
        };
        // Duplicate work never resets its deadline. Distinct interfaces retain
        // independent ownership, including exact-tag requests on two links.
        if self.requests.iter().any(|request| {
            request.identity_generation == identity_generation
                && request.transport.same_channel(&transport)
                && request.attached_interface == attached_interface
                && request.tag == tag
        }) {
            return;
        }
        if self.requests.len() == MAX_PENDING {
            tracing::warn!(
                reason = "capacity",
                "LXMF path-response request not admitted"
            );
            return;
        }
        self.requests.push_back(PendingResponse {
            identity_generation,
            transport,
            runtime_shutdown,
            attached_interface,
            tag,
            expires: now + REQUEST_LIFETIME,
            next_attempt: now,
            message: None,
        });
    }

    pub(crate) fn service(
        &mut self,
        state: &Arc<AppState>,
        session_shutdown: &ShutdownSignal,
        now: Instant,
    ) {
        // One bounded pass gives every admitted interface a turn. No sleep,
        // awaited send, or per-request task can hold up ordinary LXMF ingress.
        for _ in 0..self.requests.len() {
            let Some(mut request) = self.requests.pop_front() else {
                break;
            };
            if session_shutdown.is_triggered() || !request.is_current(state) {
                continue;
            }
            if now >= request.expires {
                tracing::warn!(reason = "expired", "LXMF path-response request expired");
                continue;
            }
            if now < request.next_attempt {
                self.requests.push_back(request);
                continue;
            }
            let lifecycle_epoch = state.identity_switch_lock.epoch();
            if !lifecycle_epoch.is_multiple_of(2) {
                request.next_attempt = now + RETRY_INTERVAL;
                self.requests.push_back(request);
                continue;
            }
            if request.message.is_none() {
                match crate::build_lxmf_path_response_message(
                    state,
                    request.attached_interface,
                    request.tag.as_deref(),
                ) {
                    Ok(message) => request.message = Some(message),
                    Err(BuildError::Coalesced) => {
                        request.next_attempt = now + COALESCED_RETRY;
                        self.requests.push_back(request);
                        continue;
                    }
                    Err(BuildError::Busy) => {
                        request.next_attempt = now + RETRY_INTERVAL;
                        self.requests.push_back(request);
                        continue;
                    }
                    Err(BuildError::Failed) => {
                        tracing::warn!(reason = "build_failed", "LXMF path-response build failed");
                        continue;
                    }
                }
            }
            // Revalidate after taking the LXMF lock/building. The sender never
            // changes. Attached interface IDs are monotonic within its runtime;
            // the transport rejects a removed ID rather than rebinding to its
            // replacement. This is local channel admission, not RF delivery.
            if session_shutdown.is_triggered()
                || !request.is_current(state)
                || now.max(Instant::now()) >= request.expires
            {
                continue;
            }
            // A harmless lifecycle lock span can defer this exact response;
            // replacing identity/runtime revokes it through is_current above.
            if state.identity_switch_lock.epoch() != lifecycle_epoch {
                request.next_attempt = now + RETRY_INTERVAL;
                self.requests.push_back(request);
                continue;
            }
            match request.transport.try_send(request.message.take().unwrap()) {
                Ok(()) => tracing::debug!(
                    attached = request.attached_interface.is_some(),
                    "queued LXMF path-response announce"
                ),
                Err(TrySendError::Full(message)) => {
                    request.message = Some(message);
                    request.next_attempt = now + RETRY_INTERVAL;
                    self.requests.push_back(request);
                }
                Err(TrySendError::Closed(_)) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound_pipeline_tests::{local_dest, message_rows, packed_inbound, pipeline_state};
    use rns_transport::link_messages::{AnnounceRequest, DestinationEvent};

    struct Fixture {
        state: Arc<AppState>,
        outbound: tokio::sync::mpsc::Receiver<TransportMessage>,
        shutdown: ShutdownSignal,
        network: rns_runtime::reticulum::ReticulumHandle,
        _root: tempfile::TempDir,
    }

    impl Fixture {
        async fn new(capacity: usize) -> Self {
            let (state, _) = pipeline_state();
            let root = tempfile::tempdir().unwrap();
            std::fs::write(
                root.path().join("config"),
                "[reticulum]\nshare_instance = No\nenable_transport = No\n[interfaces]\n",
            )
            .unwrap();
            let mut manager = crate::rns::RnsManager::init(
                root.path().to_str().unwrap(),
                Some(root.path().join("cache")),
                Arc::new(std::sync::atomic::AtomicBool::new(true)),
            )
            .await
            .unwrap();
            let network = manager.handle.clone();
            let (tx, outbound) = tokio::sync::mpsc::channel(capacity);
            manager.handle.transport_tx = tx;
            *state.rns.write().unwrap() = Some(manager);
            Self {
                state,
                outbound,
                shutdown: ShutdownSignal::new(),
                network,
                _root: root,
            }
        }

        fn fill_transport(&self) {
            self.state
                .rns
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .handle
                .transport_tx
                .try_send(TransportMessage::Outbound(
                    rns_transport::messages::OutboundRequest {
                        raw: bytes::Bytes::new(),
                        destination_hash: [0; 16],
                    },
                ))
                .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.shutdown.trigger();
            self.network.shutdown.trigger();
        }
    }

    fn raw(message: &TransportMessage) -> &bytes::Bytes {
        match message {
            TransportMessage::OutboundAttached { request, .. }
            | TransportMessage::Outbound(request) => &request.raw,
            _ => panic!("expected path-response packet"),
        }
    }

    #[tokio::test]
    async fn path_response_pressure_retains_exact_bytes_and_duplicate_deadline() {
        let mut f = Fixture::new(1).await;
        f.fill_transport();
        let now = Instant::now();
        let mut owner = PendingPathResponses::default();
        owner.admit(&f.state, Some(7), Some(vec![1; 16]), now);
        owner.service(&f.state, &f.shutdown, now);
        let bytes = raw(owner.requests[0].message.as_ref().unwrap()).clone();
        let expires = owner.requests[0].expires;
        owner.admit(&f.state, Some(7), Some(vec![1; 16]), now + RETRY_INTERVAL);
        assert_eq!(owner.requests.len(), 1);
        assert_eq!(owner.requests[0].expires, expires);
        // Taking a lifecycle lock without changing identity/runtime does not
        // turn retained wire work into a stale Activity command.
        drop(f.state.identity_switch_lock.lock().await);
        f.state.lxmf.lock().unwrap().as_mut().unwrap().display_name = "changed".into();
        owner.service(&f.state, &f.shutdown, now + RETRY_INTERVAL);
        assert_eq!(raw(owner.requests[0].message.as_ref().unwrap()), &bytes);
        f.outbound.try_recv().unwrap();
        owner.service(&f.state, &f.shutdown, now + RETRY_INTERVAL * 2);
        assert!(owner.is_empty());
        assert_eq!(raw(&f.outbound.try_recv().unwrap()), &bytes);
    }

    #[tokio::test]
    async fn path_response_during_startup_guard_is_retained_until_release() {
        let mut f = Fixture::new(1).await;
        let now = Instant::now();
        let mut owner = PendingPathResponses::default();
        let initializing = f.state.identity_switch_lock.lock().await;
        owner.admit(&f.state, Some(7), Some(vec![9; 16]), now);
        owner.service(&f.state, &f.shutdown, now);
        assert_eq!(owner.requests.len(), 1);
        assert!(owner.requests[0].message.is_none());
        assert!(f.outbound.try_recv().is_err());
        drop(initializing);
        owner.service(&f.state, &f.shutdown, now + RETRY_INTERVAL);
        assert!(owner.is_empty());
        assert!(matches!(
            f.outbound.try_recv().unwrap(),
            TransportMessage::OutboundAttached {
                interface_id: 7,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn path_response_same_tag_on_distinct_interfaces_is_independent_exact_replay() {
        let mut f = Fixture::new(4).await;
        let now = Instant::now();
        let mut owner = PendingPathResponses::default();
        for interface in [7, 8] {
            owner.admit(&f.state, Some(interface), Some(vec![2; 16]), now);
        }
        owner.service(&f.state, &f.shutdown, now);
        assert!(owner.is_empty());
        let first = f.outbound.try_recv().unwrap();
        let second = f.outbound.try_recv().unwrap();
        assert_eq!(raw(&first), raw(&second));
        assert!(matches!(
            first,
            TransportMessage::OutboundAttached {
                interface_id: 7,
                ..
            }
        ));
        assert!(matches!(
            second,
            TransportMessage::OutboundAttached {
                interface_id: 8,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn path_response_deadline_capacity_and_busy_manager_are_bounded() {
        let mut f = Fixture::new(1).await;
        let now = Instant::now();
        let mut owner = PendingPathResponses::default();
        for index in 0..MAX_PENDING + 1 {
            owner.admit(&f.state, Some(7), Some(vec![index as u8; 16]), now);
        }
        assert_eq!(owner.requests.len(), MAX_PENDING);
        {
            let _busy = f.state.lxmf.lock().unwrap();
            owner.service(&f.state, &f.shutdown, now);
            assert!(
                owner
                    .requests
                    .iter()
                    .all(|request| request.message.is_none())
            );
        }
        owner.service(&f.state, &f.shutdown, now + REQUEST_LIFETIME);
        assert!(owner.is_empty());
        assert!(f.outbound.try_recv().is_err());
        // An already-built, blocked packet has the same strict deadline.
        f.fill_transport();
        owner.admit(&f.state, Some(7), Some(vec![42; 16]), now);
        owner.service(&f.state, &f.shutdown, now);
        assert!(owner.requests[0].message.is_some());
        f.outbound.try_recv().unwrap();
        owner.service(&f.state, &f.shutdown, now + REQUEST_LIFETIME);
        assert!(owner.is_empty());
        assert!(f.outbound.try_recv().is_err());
    }

    #[tokio::test]
    async fn path_response_retired_identity_runtime_and_session_cannot_send() {
        for retired in 0..4 {
            let mut f = Fixture::new(1).await;
            f.fill_transport();
            let now = Instant::now();
            let mut owner = PendingPathResponses::default();
            owner.admit(&f.state, Some(7), Some(vec![3; 16]), now);
            owner.service(&f.state, &f.shutdown, now);
            assert!(owner.requests[0].message.is_some());
            match retired {
                0 => {
                    f.state.bump_identity_session_generation();
                }
                1 => {
                    let (replacement, _rx) = tokio::sync::mpsc::channel(1);
                    f.state
                        .rns
                        .write()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .handle
                        .transport_tx = replacement;
                }
                2 => f.shutdown.trigger(),
                _ => f.network.shutdown.trigger(),
            }
            f.outbound.try_recv().unwrap();
            owner.service(&f.state, &f.shutdown, now + RETRY_INTERVAL);
            assert!(owner.is_empty());
            assert!(f.outbound.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn path_response_coalesced_build_retries_while_inbound_messages_continue() {
        // Retry the whole ordinary-announce / admission / service setup if it
        // straddles the wall-clock second. No simulated wire timestamp is used.
        let mut deferred_fixture = None;
        for _ in 0..8 {
            let f = Fixture::new(4).await;
            f.state
                .lxmf
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .create_announce_packet()
                .unwrap();
            let mut owner = PendingPathResponses::default();
            owner.admit(&f.state, Some(7), Some(vec![5; 16]), Instant::now());
            owner.service(&f.state, &f.shutdown, Instant::now());
            if owner.requests.len() == 1 && owner.requests[0].message.is_none() {
                deferred_fixture = Some(f);
                break;
            }
        }
        let mut f = deferred_fixture.expect("must exercise the actual ordering deferral");
        // Put that request through the actual task as well; normal ingress
        // must complete before the deferred response can be constructed.
        let (events, rx) = tokio::sync::mpsc::channel(4);
        events
            .send(DestinationEvent::AnnounceRequested(AnnounceRequest {
                app_name: ratspeak_core::LXMF_DELIVERY_APP_NAME.into(),
                path_response: true,
                tag: Some(vec![6; 16]),
                attached_interface: Some(7),
            }))
            .await
            .unwrap();
        let destination = local_dest(&f.state);
        let packed = packed_inbound(destination, [0xEE; 16], "during path discovery");
        let mut packet = vec![0, 0]; // Header1, SINGLE DATA, zero hops
        packet.extend_from_slice(&destination);
        packet.push(0); // context NONE
        packet.extend_from_slice(&packed[16..]);
        events
            .send(DestinationEvent::InboundPacket {
                raw: packet.into(),
                interface_id: 7,
                metrics: Default::default(),
            })
            .await
            .unwrap();
        let worker = tokio::spawn(crate::handle_inbound_lxmf(
            f.state.clone(),
            rx,
            f.shutdown.clone(),
        ));
        tokio::time::timeout(Duration::from_millis(500), async {
            while message_rows(&f.state) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("inbound processing cannot await the response's second");
        let response = tokio::time::timeout(Duration::from_secs(3), f.outbound.recv())
            .await
            .unwrap()
            .unwrap();
        let (header, _) = rns_wire::header::PacketHeader::unpack(raw(&response)).unwrap();
        assert_eq!(
            header.context,
            rns_wire::context::PacketContext::PathResponse
        );
        assert!(matches!(
            response,
            TransportMessage::OutboundAttached {
                interface_id: 7,
                ..
            }
        ));
        f.shutdown.trigger();
        worker.await.unwrap();
    }
}
