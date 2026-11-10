//! Hosted relay admission. Packet proofs follow durable storage; Resource
//! proofs retain their transport-only meaning.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;
use lxmf_core::propagation_node::{
    PropagationAdmission, PropagationAdmissionOutcome, PropagationAdmissionWaiter, PropagationNode,
    ValidatedOpaquePropagationBlob,
};
use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::link_manager::{InboundLinkPacket, LinkPacketReceipt, LinkPacketRejection};
use rns_runtime::link_manager::{
    LinkManager, LinkManagerAccountingEvent, LinkResourceConclusion, LinkResourceEvent,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use crate::AppState;

const MAX_RESOURCE_BYTES: usize = lxmf_core::admission::MAX_RETAINED_DOWNLOAD_BYTES;
pub(crate) struct ResourceOwner {
    original: [u8; 32],
    _permit: OwnedSemaphorePermit,
}
type ResourceOwners = Arc<Mutex<HashMap<[u8; 16], Arc<ResourceOwner>>>>;
fn resource_slots() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(3)))
}

pub(crate) fn install_resource_admission(
    manager: &mut LinkManager,
    state: &Arc<AppState>,
) -> ResourceOwners {
    let owners: ResourceOwners = Arc::new(Mutex::new(HashMap::new()));
    let accept_owners = owners.clone();
    let state = Arc::downgrade(state);
    manager.set_resource_strategy(rns_runtime::prelude::ResourceStrategy::AcceptApp);
    manager.set_resource_accept_handler(move |link, advertisement| {
        if !state
            .upgrade()
            .is_some_and(|state| state.effective_propagation_hosting())
        {
            return false;
        }
        if crate::inbound_resource_admission_bytes(
            advertisement.data_size,
            advertisement.total_segments,
            MAX_RESOURCE_BYTES,
        )
        .is_none()
        {
            return false;
        }
        let original = advertisement.original_hash;
        let Ok(mut owners) = accept_owners.lock() else {
            return false;
        };
        if let Some(owner) = owners.get(&link) {
            return owner.original == original;
        }
        let Ok(permit) = resource_slots().clone().try_acquire_owned() else {
            return false;
        };
        owners.insert(
            link,
            Arc::new(ResourceOwner {
                original,
                _permit: permit,
            }),
        );
        true
    });
    owners
}

fn worker_slots() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(4)))
}
fn active_hosts() -> &'static Arc<tokio::sync::RwLock<()>> {
    static HOSTS: OnceLock<Arc<tokio::sync::RwLock<()>>> = OnceLock::new();
    HOSTS.get_or_init(|| Arc::new(tokio::sync::RwLock::new(())))
}
pub(crate) async fn quiesce() -> tokio::sync::OwnedRwLockWriteGuard<()> {
    active_hosts().clone().write_owned().await
}

struct GetLease {
    state: Arc<AppState>,
    generation: u64,
    _host: tokio::sync::OwnedRwLockReadGuard<()>,
}
impl GetLease {
    fn acquire(state: &Arc<AppState>, generation: u64) -> Option<Self> {
        let lease = Self {
            state: state.clone(),
            generation,
            _host: active_hosts().clone().try_read_owned().ok()?,
        };
        lease.is_current().then_some(lease)
    }
    fn is_current(&self) -> bool {
        self.state.current_identity_session_generation() == self.generation
            && self.state.effective_propagation_hosting()
            && self
                .state
                .session_shutdown
                .read()
                .is_ok_and(|shutdown| !shutdown.is_triggered())
    }
}

/// The Link loop only captures the already-budgeted request. All planning,
/// native purge barriers and validated file reads run on its bounded worker.
/// The host read lease joins actual work before migration or identity teardown.
pub(crate) fn install_get_handler(
    manager: &mut LinkManager,
    state: &Arc<AppState>,
    node: Arc<Mutex<PropagationNode>>,
    response_limit: usize,
) -> bool {
    let generation = state.current_identity_session_generation();
    let state = Arc::downgrade(state);
    manager.register_deferred_request_handler(
        lxmf_core::constants::MESSAGE_GET_PATH,
        rns_identity::destination::AllowPolicy::AllowAll,
        None,
        false,
        response_limit,
        move |request, responder| {
            let Some(state) = state.upgrade() else {
                return;
            };
            let Some(lease) = GetLease::acquire(&state, generation) else {
                return;
            };
            let Some(identity) = request.remote_identity else {
                return;
            };
            let destination = rns_identity::destination::Destination::hash_from_name_and_identity(
                ratspeak_core::LXMF_DELIVERY_APP_NAME,
                Some(&identity.hash),
            );
            let node = node.clone();
            tokio::task::spawn_blocking(move || {
                if !lease.is_current() || responder.is_cancelled() {
                    return;
                }
                let limit = responder.response_byte_limit();
                let action = match node.lock() {
                    Ok(mut node) => {
                        node.handle_get_request_bounded(&request.data, &destination, limit)
                    }
                    Err(_) => return,
                };
                let (response, _) = action.into_response_with_served_count_bounded(limit);
                if lease.is_current() && !responder.is_cancelled() {
                    drop(
                        responder.respond(rns_runtime::link_manager::RequestOutcome::ReplyValue(
                            response,
                        )),
                    );
                }
            });
        },
    )
}

struct Job {
    link: [u8; 16],
    data: Bytes,
    receipt: Option<LinkPacketReceipt>,
    _resource: Option<Arc<ResourceOwner>>,
}
enum Outcome {
    Stored,
    InvalidStamp,
    Unavailable,
    Pending(PropagationAdmissionWaiter),
}

fn execute(node: &Arc<Mutex<PropagationNode>>, data: &Bytes) -> Outcome {
    let cost = match node.lock() {
        Ok(node) if node.is_durable() => node.min_stamp_cost(),
        _ => return Outcome::Unavailable,
    };
    let Ok((_, entries)) = lxmf_core::message_api::LxMessage::unpack_propagation_wrapper_owned(
        data.clone(),
        MAX_RESOURCE_BYTES,
    ) else {
        return Outcome::Unavailable;
    };
    // Validate the complete batch before reserving or publishing any entry.
    let mut validated = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(blob) = ValidatedOpaquePropagationBlob::validate(entry, cost) else {
            return Outcome::InvalidStamp;
        };
        validated.push(blob);
    }
    for blob in validated {
        let plan = match node.lock() {
            Ok(mut node) => node.plan_opaque_admission(blob),
            Err(_) => return Outcome::Unavailable,
        };
        let plan = match plan {
            PropagationAdmission::Reserved(plan) => plan,
            PropagationAdmission::CommittedDuplicate(check) => {
                if check.verify().is_err() {
                    return Outcome::Unavailable;
                }
                continue;
            }
            PropagationAdmission::Pending(waiter) => return Outcome::Pending(waiter),
            PropagationAdmission::Rejected => return Outcome::Unavailable,
        };
        let tid = plan.transient_id();
        let size = plan.size();
        match plan.persist() {
            Ok(persisted) => {
                if !node
                    .lock()
                    .is_ok_and(|mut node| node.commit_store_write(persisted))
                {
                    return Outcome::Unavailable;
                }
            }
            Err(error) => {
                if let Ok(mut node) = node.lock() {
                    node.abort_store_write(&tid, size);
                }
                tracing::warn!(%error, "hosted propagation deposit was not durably committed");
                return Outcome::Unavailable;
            }
        }
    }
    Outcome::Stored
}

/// Four actual blocking workers, FIFO per Link and round-robin between Links.
/// All queue and worker owners retain RNS receipt or Resource admission leases.
pub(crate) async fn run(
    state: Arc<AppState>,
    node: Arc<Mutex<PropagationNode>>,
    mut packets: mpsc::Receiver<InboundLinkPacket>,
    mut resources: mpsc::UnboundedReceiver<LinkManagerAccountingEvent>,
    owners: ResourceOwners,
    shutdown: ShutdownSignal,
) {
    let _host = active_hosts().clone().read_owned().await;
    if shutdown.is_triggered() {
        return;
    }
    let generation = state.current_identity_session_generation();
    let mut queues: HashMap<[u8; 16], VecDeque<Job>> = HashMap::new();
    let mut ready = VecDeque::new();
    let mut workers = tokio::task::JoinSet::new();
    let mut pending = tokio::task::JoinSet::new();
    let mut stopping = false;
    loop {
        while !stopping && workers.len() < 4 {
            let Ok(permit) = worker_slots().clone().try_acquire_owned() else {
                break;
            };
            let Some(link) = ready.pop_front() else {
                break;
            };
            let Some(queue) = queues.get_mut(&link) else {
                continue;
            };
            let Some(job) = queue.pop_front() else {
                continue;
            };
            if queue.is_empty() {
                queues.remove(&link);
            } else {
                ready.push_back(link);
            }
            let node = node.clone();
            let state = state.clone();
            workers.spawn_blocking(move || {
                let _worker = permit;
                let outcome = if state.current_identity_session_generation() == generation
                    && state.effective_propagation_hosting()
                {
                    execute(&node, &job.data)
                } else {
                    Outcome::Unavailable
                };
                (job, outcome)
            });
        }
        if stopping && workers.is_empty() {
            break;
        }
        let job = tokio::select! {
            biased;
            _ = shutdown.wait(), if !stopping => { stopping=true; queues.clear(); ready.clear(); pending.abort_all(); if let Ok(mut owners)=owners.lock(){owners.clear();} None },
            finished = workers.join_next(), if !workers.is_empty() => {
                if let Some(Ok((job, outcome))) = finished {
                    let outcome = if let Outcome::Pending(waiter) = outcome {
                        if !stopping {
                            pending.spawn(async move { let result=waiter.wait().await; (job,result) });
                        }
                        continue;
                    } else { outcome };
                    if !stopping && !shutdown.is_triggered() && state.current_identity_session_generation()==generation && state.effective_propagation_hosting()
                        && let Some(receipt)=job.receipt
                    {
                        match outcome {
                            Outcome::Stored => { drop(receipt.accept()); }
                            Outcome::InvalidStamp => { drop(receipt.reject_and_close(LinkPacketRejection::InvalidPropagationStamp)); }
                            Outcome::Unavailable => { drop(receipt.close_without_proof()); }
                            Outcome::Pending(_) => unreachable!("waiter moved into pending set"),
                        }
                    }
                    // The payload lease stays alive through resolution and
                    // drops only now, after the actual storage worker exited.
                }
                None
            },
            item = pending.join_next(), if !pending.is_empty() && !stopping => {
                match item {
                    Some(Ok((job,PropagationAdmissionOutcome::Committed))) => Some(job),
                    Some(Ok((job,PropagationAdmissionOutcome::Failed))) => {
                        if let Some(receipt)=job.receipt { drop(receipt.close_without_proof()); }
                        None
                    },
                    _ => None,
                }
            },
            _ = worker_slots().acquire(), if !stopping && !ready.is_empty() && workers.len()<4 => None,
            packet=packets.recv(), if !stopping => match packet {
                Some(packet) => { let link=packet.link_id(); let (data,receipt)=packet.into_parts(); Some(Job {link,data,receipt,_resource:None}) },
                None => { stopping=true; queues.clear(); ready.clear(); None },
            },
            event=resources.recv(), if !stopping => match event {
                Some(LinkManagerAccountingEvent::ResourceCompletion(completion)) => {
                    let owner=owners.lock().ok().and_then(|mut owners|owners.remove(&completion.link_id));
                    owner.map(|owner|Job {link:completion.link_id,data:Bytes::from(completion.data),receipt:None,_resource:Some(owner)})
                },
                Some(LinkManagerAccountingEvent::LinkClosed {link_id}) => {if let Ok(mut owners)=owners.lock(){owners.remove(&link_id);}None},
                Some(LinkManagerAccountingEvent::ResourceEvent(LinkResourceEvent::Concluded {link_id,conclusion,direction:rns_runtime::link_manager::LinkResourceDirection::Inbound,..})) => {
                    if !matches!(conclusion,LinkResourceConclusion::Complete) && let Ok(mut owners)=owners.lock(){owners.remove(&link_id);} None
                },
                Some(_) => None,
                None => {stopping=true;queues.clear();ready.clear();None},
            },
        };
        if let Some(job) = job {
            let queue = queues.entry(job.link).or_default();
            if queue.is_empty() {
                ready.push_back(job.link);
            }
            queue.push_back(job);
        }
    }
}

#[cfg(test)]
pub(crate) mod get_tests {
    use super::*;
    use std::sync::atomic::Ordering;
    /// Also held by the library's tests that tear a session down: a teardown
    /// quiesces every host, and these tests wait that out only so long.
    pub(crate) static GET_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    fn ready_state() -> Arc<AppState> {
        let (state, _) = crate::inbound_pipeline_tests::pipeline_state();
        state
            .propagation_node_hosting_enabled
            .store(true, Ordering::Relaxed);
        state.set_propagation_host_readiness(
            "fixture".into(),
            state.current_identity_session_generation(),
            crate::state::PropagationHostReadiness::Ready,
            None,
        );
        state
    }
    /// A runtime shutdown in another test briefly quiesces every host.
    async fn lease(state: &Arc<AppState>, generation: u64) -> GetLease {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(lease) = GetLease::acquire(state, generation) {
                return lease;
            }
            assert!(tokio::time::Instant::now() < deadline, "no host lease");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn hosted_get_quiescence_joins_cancelled_blocking_work_and_invalidates_generation() {
        let _test = GET_TEST_LOCK.lock().await;
        let state = ready_state();
        let lease = lease(&state, state.current_identity_session_generation()).await;
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            assert!(
                !lease.is_current(),
                "old generation cannot publish response"
            );
            drop(lease);
            finished_tx.send(()).unwrap();
        });
        started_rx.await.unwrap();
        worker.abort(); // Already-running native work must keep its lifecycle lease.
        state.bump_identity_session_generation();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), quiesce())
                .await
                .is_err()
        );
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        release_tx.send(()).unwrap();
        finished_rx.await.unwrap();
        let _stopped = tokio::time::timeout(std::time::Duration::from_secs(5), quiesce())
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn hosted_get_requires_current_ready_enabled_host() {
        let _test = GET_TEST_LOCK.lock().await;
        let state = ready_state();
        let old = state.current_identity_session_generation();
        drop(lease(&state, old).await);
        state
            .propagation_node_hosting_enabled
            .store(false, Ordering::Relaxed);
        assert!(GetLease::acquire(&state, old).is_none());
        state
            .propagation_node_hosting_enabled
            .store(true, Ordering::Relaxed);
        state.bump_identity_session_generation();
        assert!(GetLease::acquire(&state, old).is_none());
        assert!(GetLease::acquire(&state, state.current_identity_session_generation()).is_none());
    }
}
