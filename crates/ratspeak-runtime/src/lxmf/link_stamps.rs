//! Link request stamps for Ratspeak's own LXMF Links, searched off the tick
//! as Python grinds deferred keys on a thread (RNS/Link.py:310-382@1.6.0).
use super::*;
use lxmf_core::router::LinkDeliveryPlan;
use rns_link::handshake::EphemeralKeys;
use rns_link::stamp::LinkStampError;
use std::sync::atomic::{AtomicBool, Ordering};

type KeySearchReply = oneshot::Receiver<Result<EphemeralKeys, LinkStampError>>;

#[derive(Default)]
pub(super) struct LinkKeySearches {
    pending: HashMap<[u8; 16], Search>,
    ready: HashMap<[u8; 16], EphemeralKeys>,
    /// The node whose keys the inbox client holds for its next Link.
    download_keys_given: Option<[u8; 16]>,
    /// The peer whose keys the sync task holds for its next Link.
    sync_keys_given: Option<[u8; 16]>,
}

struct Search {
    start: Option<DeferredStart>,
    reply: KeySearchReply,
    _cancel: CancelOnDrop,
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// The Link delivery to open once its keys are found.
pub(super) enum DeferredStart {
    Direct {
        message_id: [u8; 32],
        hops: u8,
    },
    Propagated {
        message_id: [u8; 32],
        hops: u8,
        packed: Vec<u8>,
    },
    Download,
    Sync,
}

impl DeferredStart {
    fn message_id(&self) -> Option<[u8; 32]> {
        match self {
            Self::Direct { message_id, .. } | Self::Propagated { message_id, .. } => {
                Some(*message_id)
            }
            Self::Download | Self::Sync => None,
        }
    }
}

impl LinkKeySearches {
    pub(super) fn searching(&self, dest: &[u8; 16]) -> bool {
        self.pending.contains_key(dest)
    }

    /// Record what to open when the search for `dest` ends. A message start
    /// takes precedence over a download, which searches again afterwards.
    pub(super) fn defer(&mut self, dest: [u8; 16], start: DeferredStart) {
        if let Some(search) = self.pending.get_mut(&dest)
            && (search.start.is_none()
                || !matches!(start, DeferredStart::Download | DeferredStart::Sync))
        {
            search.start = Some(start);
        }
    }

    /// Drop the searches carrying `message_id`, cancelling their grind.
    pub(super) fn cancel_message(&mut self, message_id: &[u8; 32]) {
        self.pending.retain(|_, search| {
            search
                .start
                .as_ref()
                .is_none_or(|start| start.message_id().as_ref() != Some(message_id))
        });
    }
}

impl LxmfManager {
    pub(super) fn opens_direct_link(&self, dest: [u8; 16]) -> bool {
        self.link_delivery
            .as_ref()
            .and_then(|ld| ld.direct_link_snapshot(dest))
            .is_none_or(|snapshot| snapshot.idle_expired)
    }

    pub(super) fn opens_propagation_link(&self, node: [u8; 16]) -> bool {
        self.link_delivery
            .as_ref()
            .and_then(|ld| ld.propagation_link_snapshot(node))
            .is_none_or(|snapshot| snapshot.link_state == rns_link::link::LinkState::Closed)
    }

    /// Keys for a new Link to `dest`: `None` while a search runs (record what
    /// to open with [`LinkKeySearches::defer`]), `Some(None)` when the request
    /// is not stamped.
    pub(super) fn link_request_keys(&mut self, dest: [u8; 16]) -> Option<Option<EphemeralKeys>> {
        if self.link_keys.searching(&dest) {
            return None;
        }
        if let Some(keys) = self.link_keys.ready.remove(&dest) {
            return Some(Some(keys));
        }
        let policy = rns_runtime::link_stamp::link_stamp_policy();
        // Identity.recall_link_cost: the cost the destination last announced.
        let recalled = self
            .route_entries
            .get(&dest)
            .and_then(|entry| entry.lr_cost);
        let Some(cost) = policy.request_cost(recalled) else {
            return Some(None);
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let mut ctl = policy.control(cost);
        ctl.cancel = cancel.clone();
        let (reply_tx, reply) = oneshot::channel();
        // A failed spawn drops the sender, which reads as a failed search.
        let _ = std::thread::Builder::new()
            .name("link-request-stamp".into())
            .spawn(move || {
                let _ = reply_tx.send(rns_link::stamp::generate_stamped_ephemeral_keys(
                    &dest, cost, &ctl,
                ));
            });
        tracing::debug!(
            dest = %crate::short_id(&hex::encode(dest)),
            cost,
            "starting link request stamp search"
        );
        self.link_keys.pending.insert(
            dest,
            Search {
                start: None,
                reply,
                _cancel: CancelOnDrop(cancel),
            },
        );
        None
    }

    /// Hand found keys for `dest` to the Link delivery manager; `false` while
    /// a search runs.
    pub(super) fn prepare_delivery_link_keys(&mut self, dest: [u8; 16]) -> bool {
        match self.link_request_keys(dest) {
            None => false,
            Some(None) => true,
            Some(Some(keys)) => {
                if self.ensure_link_delivery_manager()
                    && let Some(ld) = self.link_delivery.as_mut()
                {
                    ld.provide_link_request_keys(dest, keys);
                }
                true
            }
        }
    }

    /// Before the inbox client links to `node`, search its keys; `false` holds
    /// the client back while the search runs.
    pub(super) fn prepare_download_link_keys(&mut self, node: [u8; 16]) -> bool {
        let linking = self.propagation_client.as_ref().is_some_and(|client| {
            client.state() == lxmf_core::propagation_client::PropagationClientState::PathRequested
        });
        if !linking {
            self.link_keys.download_keys_given = None;
            return true;
        }
        if self.link_keys.download_keys_given == Some(node) {
            return true;
        }
        match self.link_request_keys(node) {
            None => {
                self.link_keys.defer(node, DeferredStart::Download);
                false
            }
            Some(None) => true,
            Some(Some(keys)) => {
                if let Some(client) = self.propagation_client.as_mut() {
                    client.provide_link_request_keys(node, keys);
                    self.link_keys.download_keys_given = Some(node);
                }
                true
            }
        }
    }

    /// Before the sync task links to `node`, search its keys; `false` holds
    /// the task back while the search runs.
    pub(super) fn prepare_sync_link_keys(&mut self) -> bool {
        let Some(node) = self.propagation_sync.as_ref().and_then(|sync| {
            (sync.state == lxmf_core::propagation_sync::SyncTaskState::PathRequested)
                .then(|| sync.node_dest_hash())
                .flatten()
        }) else {
            self.link_keys.sync_keys_given = None;
            return true;
        };
        if self.link_keys.sync_keys_given == Some(node) {
            return true;
        }
        match self.link_request_keys(node) {
            None => {
                self.link_keys.defer(node, DeferredStart::Sync);
                false
            }
            Some(None) => true,
            Some(Some(keys)) => {
                if let Some(sync) = self.propagation_sync.as_mut() {
                    sync.provide_link_request_keys(node, keys);
                    self.link_keys.sync_keys_given = Some(node);
                }
                true
            }
        }
    }

    fn still_linking(&self, node: [u8; 16], start: &DeferredStart) -> bool {
        match start {
            DeferredStart::Download => {
                self.router.outbound_propagation_node == Some(node)
                    && self.propagation_client.as_ref().is_some_and(|client| {
                        client.state()
                            == lxmf_core::propagation_client::PropagationClientState::PathRequested
                    })
            }
            DeferredStart::Sync => self.propagation_sync.as_ref().is_some_and(|sync| {
                sync.state == lxmf_core::propagation_sync::SyncTaskState::PathRequested
                    && sync.node_dest_hash() == Some(node)
            }),
            DeferredStart::Direct { .. } | DeferredStart::Propagated { .. } => true,
        }
    }

    /// Open the Links whose keys were found; a failed search closes its
    /// never-activated Link as Python's `__stamp_failed` does.
    pub(super) fn poll_link_key_searches(
        &mut self,
        now: f64,
        results: &mut Vec<(String, &'static str)>,
    ) {
        let finished = self
            .link_keys
            .pending
            .iter_mut()
            .filter_map(|(dest, search)| match search.reply.try_recv() {
                Err(oneshot::error::TryRecvError::Empty) => None,
                Ok(outcome) => Some((*dest, outcome.ok())),
                Err(oneshot::error::TryRecvError::Closed) => Some((*dest, None)),
            })
            .collect::<Vec<_>>();
        for (dest, keys) in finished {
            let Some(start) = self
                .link_keys
                .pending
                .remove(&dest)
                .and_then(|mut search| search.start.take())
            else {
                continue;
            };
            let Some(message_id) = start.message_id() else {
                match keys {
                    // Keys belong to the Link being opened; nobody is opening it any more.
                    Some(keys) if self.still_linking(dest, &start) => {
                        self.link_keys.ready.insert(dest, keys);
                    }
                    Some(_) => {}
                    None if matches!(start, DeferredStart::Sync) => {
                        tracing::info!(
                            node = %crate::short_id(&hex::encode(dest)),
                            "link request stamp generation failed for peer sync"
                        );
                        if let Some(sync) = self.propagation_sync.as_mut() {
                            sync.state = lxmf_core::propagation_sync::SyncTaskState::Failed;
                        }
                    }
                    None => {
                        tracing::info!(
                            node = %crate::short_id(&hex::encode(dest)),
                            "link request stamp generation failed for message download"
                        );
                        if let Some(client) = self.propagation_client.as_mut() {
                            client.cancel_download();
                            self.failed_propagation_syncs
                                .push((dest, "sync_failed".to_string()));
                        }
                    }
                }
                continue;
            };
            let Some(message) = self
                .router
                .pending_outbound
                .iter()
                .find(|message| {
                    message.message_id() == message_id
                        && message.state != lxmf_core::constants::MessageState::Cancelled
                })
                .cloned()
            else {
                continue;
            };
            let Some(keys) = keys else {
                tracing::info!(
                    dest = %crate::short_id(&hex::encode(dest)),
                    "link request stamp generation failed"
                );
                let medium = self.outbound_medium.clone();
                let actions = self
                    .router
                    .outbound_link_closed(&message_id, false, &medium, now);
                results.extend(self.execute_encrypted_actions(actions));
                continue;
            };
            self.link_keys.ready.insert(dest, keys);
            match start {
                DeferredStart::Direct { hops, .. } => self.deliver_direct_over_link(
                    message,
                    dest,
                    LinkDeliveryPlan::StartLink { hops },
                    results,
                ),
                DeferredStart::Download | DeferredStart::Sync => {}
                DeferredStart::Propagated { hops, packed, .. } => {
                    self.ready_propagation.insert(message_id, packed);
                    self.deliver_propagated_over_link(
                        message,
                        dest,
                        LinkDeliveryPlan::StartLink { hops },
                        results,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lxmf::tests::test_manager;

    #[test]
    fn keys_found_after_the_client_stopped_linking_are_dropped() {
        let mut mgr = test_manager();
        for (node, start) in [
            ([1; 16], DeferredStart::Download),
            ([2; 16], DeferredStart::Sync),
        ] {
            let (reply_tx, reply) = oneshot::channel();
            assert!(reply_tx.send(Ok(EphemeralKeys::generate())).is_ok());
            mgr.link_keys.pending.insert(
                node,
                Search {
                    start: Some(start),
                    reply,
                    _cancel: CancelOnDrop(Arc::new(AtomicBool::new(false))),
                },
            );
        }
        mgr.poll_link_key_searches(0.0, &mut Vec::new());
        assert!(mgr.link_keys.pending.is_empty());
        assert!(mgr.link_keys.ready.is_empty());
    }
}
