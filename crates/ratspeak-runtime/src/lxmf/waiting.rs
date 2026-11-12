//! Wakes of waiting messages. A message that could not be sent for a network
//! reason waits in the database, held by nothing here; this takes the oldest
//! of each queue and hands it to the resume path as the same message.

use std::collections::HashMap;

use ratspeak_core::delivery::{OutboundState, Phase, WakeCause, policy};

use super::{
    DeliveryFailureReason, DeliveryPreference, DeliveryProfile, LxmfManager, delivery_method_name,
};
use crate::db;
use crate::state::DbPool;

/// Something that may have brought a waiting message's recipient within reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeEvent {
    /// The app came to the foreground.
    Resumed,
    /// An interface came online.
    InterfaceOnline,
    /// An announce of, or a new path to, this destination.
    Heard([u8; 16]),
    /// The user chose Try now on a message to this recipient.
    TryNow([u8; 16]),
    /// The user sent this recipient a message that waits behind earlier ones.
    NewSend([u8; 16]),
}

/// The wakes asked for since the last full tick, one cause a recipient.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WakeSignals {
    all: Option<WakeCause>,
    peers: HashMap<[u8; 16], WakeCause>,
}

impl WakeSignals {
    /// More recipients than this in one tick wake every queue instead.
    const MAX_PEERS: usize = 256;

    pub fn raise(&mut self, event: WakeEvent) {
        match event {
            WakeEvent::Resumed => self.raise_all(WakeCause::Start),
            WakeEvent::InterfaceOnline => self.raise_all(WakeCause::Network),
            WakeEvent::Heard(peer) => self.raise_peer(peer, WakeCause::Recipient),
            WakeEvent::TryNow(peer) => self.raise_peer(peer, WakeCause::User),
            WakeEvent::NewSend(peer) => self.raise_peer(peer, WakeCause::NewSend),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_none() && self.peers.is_empty()
    }

    fn raise_all(&mut self, cause: WakeCause) {
        self.all = Some(self.all.map_or(cause, |held| held.stronger(cause)));
    }

    fn raise_peer(&mut self, peer: [u8; 16], cause: WakeCause) {
        if let Some(held) = self.peers.get_mut(&peer) {
            *held = held.stronger(cause);
        } else if self.peers.len() < Self::MAX_PEERS {
            self.peers.insert(peer, cause);
        } else {
            // Too many to name: every queue is looked at, under the broad gate.
            self.raise_all(WakeCause::Timer);
        }
    }
}

/// What one run of the engine works with. `now` is the steady clock; every
/// wait and gate is measured on it.
pub struct WakeContext<'a> {
    pub db: &'a DbPool,
    pub now: f64,
    pub keep_trying: bool,
    pub online: bool,
    pub signals: WakeSignals,
    /// Grants a woken attachment its transfer budget and keeps it under the id.
    pub reserve: &'a mut dyn FnMut(&str, usize) -> bool,
    /// Gives back a budget granted to a wake that did not happen.
    pub release: &'a mut dyn FnMut(&str),
}

/// A waiting message taken for a new attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Woken {
    pub msg_id: String,
    pub method: &'static str,
    pub cause: WakeCause,
}

/// What a run did; the tick tells the dashboard and Activity once the manager
/// lock is released.
#[derive(Debug, Default)]
pub struct WakeOutcome {
    pub woken: Vec<Woken>,
    /// Waiting messages whose time ran out; they are Not delivered now.
    pub expired: Vec<db::ExpiredWait>,
    /// Steps of woken sends that ended before they left, for the tick's results.
    pub failed: Vec<(String, &'static str)>,
}

/// Recipient, the method the user chose, and text or attachment.
type QueueKey = [String; 3];

/// The engine's memory. A restart loses it; order, gates, and counts are in
/// the database, so nothing here has to survive.
#[derive(Default)]
pub(super) struct Wakes {
    /// The wake at start has been made.
    started: bool,
    /// Times set ahead of the clock have been brought back.
    clamped: bool,
    last_sweep: Option<f64>,
    last_interface_wake: Option<f64>,
    /// Ids of the sends this engine woke and has not seen settle, with their queue.
    woken: HashMap<String, QueueKey>,
    /// Queues a wake found busy: tried again each tick with the cause it had.
    held: HashMap<QueueKey, (WakeCause, bool)>,
    /// An Offline Inbox node was put in use since the last run.
    pub(super) inbox_selected: bool,
}

fn within(period: f64, since: Option<f64>, now: f64) -> bool {
    // A clock set back reads as a negative age: then the period is over too.
    since.is_some_and(|since| (0.0..period).contains(&(now - since)))
}

impl LxmfManager {
    /// End waiting messages whose time ran out and, with waiting on, send
    /// again the oldest message of each queue that a signal reached. Called
    /// on every full tick, under the manager lock, before the tick's own work.
    pub fn run_wakes(&mut self, ctx: WakeContext<'_>) -> WakeOutcome {
        let WakeContext {
            db,
            now,
            keep_trying,
            online,
            mut signals,
            reserve,
            release,
        } = ctx;
        self.keep_trying = keep_trying;
        let identity = self.identity_hash.clone();
        let mut outcome = WakeOutcome::default();

        if keep_trying && !self.wakes.clamped {
            self.wakes.clamped = true;
            // Before the first sweep, or a clock that was set back would end messages late.
            db::clamp_future_queue_times(db, &identity, now);
        }
        // Expiry needs neither a network nor the setting.
        let sweep = !within(policy::TIMER_PERIOD_SECS, self.wakes.last_sweep, now);
        if sweep {
            self.wakes.last_sweep = Some(now);
            outcome.expired = db::expire_waiting(db, &identity, now).unwrap_or_default();
        }
        if !keep_trying {
            // Turned on later, the engine starts over.
            self.wakes.started = false;
            self.wakes.held.clear();
            self.wakes.inbox_selected = false;
            return outcome;
        }

        // Links flap; an interface coming up wakes the queues once in a while.
        if signals.all == Some(WakeCause::Network) {
            if within(
                policy::INTERFACE_WAKE_MIN_SECS,
                self.wakes.last_interface_wake,
                now,
            ) {
                signals.all = None;
            } else {
                self.wakes.last_interface_wake = Some(now);
            }
        }
        if online && !self.wakes.started {
            self.wakes.started = true;
            signals.raise_all(WakeCause::Start);
        } else if online && sweep {
            signals.raise_all(WakeCause::Timer);
        }
        let inbox_heard = std::mem::take(&mut self.wakes.inbox_selected)
            || self
                .router
                .outbound_propagation_node
                .is_some_and(|node| signals.peers.contains_key(&node));

        // A woken send that settled lets the next of its queue go at once.
        let mut advance: HashMap<QueueKey, bool> = HashMap::new();
        for (id, queue) in self.wakes.woken.clone() {
            let Ok(stored) = db::outbound_message_state(db, &identity, &id) else {
                continue;
            };
            let state = stored.as_deref().and_then(OutboundState::parse);
            match state.map(OutboundState::phase) {
                Some(Phase::InFlight) => continue,
                // It waits again; its own gate decides when it is tried next.
                Some(Phase::Waiting) => {}
                // Ended, done, deleted, or rewritten: the queue moves on.
                Some(Phase::Ended | Phase::Done) | None => {
                    advance.insert(queue, state == Some(OutboundState::Propagated));
                }
            }
            self.wakes.woken.remove(&id);
        }

        if signals.is_empty() && !inbox_heard && advance.is_empty() && self.wakes.held.is_empty() {
            return outcome;
        }
        let Ok(queues) = db::waiting_queues(db, &identity) else {
            return outcome;
        };
        let held = std::mem::take(&mut self.wakes.held);
        let inbox_ready = self.inbox_ready();
        let stamps_idle = self.router.pending_deferred_stamps.is_empty()
            && self.router.active_deferred_stamp.is_none();
        let mut lane = self
            .wakes
            .woken
            .iter()
            .filter(|(id, queue)| {
                queue[2] != DeliveryProfile::Attachment.as_str() && self.still_owned(id)
            })
            .count();

        for queue in queues {
            let key: QueueKey = [
                queue.destination.clone(),
                queue.preference.clone(),
                queue.profile.clone(),
            ];
            let peer = hex::decode(&queue.destination)
                .ok()
                .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok());
            let stored = DeliveryPreference::parse(Some(&queue.preference));
            let uses_inbox = matches!(
                stored,
                DeliveryPreference::Auto | DeliveryPreference::Propagated
            );
            // The strongest cause that reached this queue, and whether it is to go by the inbox.
            let reached = [
                advance
                    .get(&key)
                    .map(|after_deposit| (WakeCause::Queue, *after_deposit)),
                held.get(&key).copied(),
                peer.and_then(|peer| signals.peers.get(&peer))
                    .map(|cause| (*cause, false)),
                (inbox_heard && uses_inbox).then_some((WakeCause::Inbox, false)),
                signals.all.map(|cause| (cause, false)),
            ]
            .into_iter()
            .flatten()
            .reduce(|strongest, next| {
                if strongest.0.stronger(next.0) == strongest.0 {
                    strongest
                } else {
                    next
                }
            });
            let Some((cause, after_deposit)) = reached else {
                continue;
            };
            let id = &queue.head.message_id;

            // One message of a queue in the air at a time: order, and one probe a wake.
            if queue
                .in_flight
                .iter()
                .chain([id])
                .any(|each| self.still_owned(each))
            {
                self.wakes.held.insert(key, (cause, after_deposit));
                continue;
            }
            if now - queue.last_auto_at < policy::gate_secs(cause.gate(), queue.auto_sends) {
                continue;
            }
            if cause.needs_network() && !online {
                continue;
            }
            let cycle = if after_deposit
                && stored == DeliveryPreference::Auto
                && policy::FOLLOWERS_USE_INBOX_AFTER_DEPOSIT
                && inbox_ready
            {
                DeliveryPreference::Propagated
            } else {
                stored
            };
            if cycle == DeliveryPreference::Propagated && !inbox_ready {
                continue;
            }
            let attachment = queue.profile == DeliveryProfile::Attachment.as_str();
            // Fresh sends come first: few woken texts at once, and none while a stamp is computed.
            if !stamps_idle || (!attachment && lane >= policy::LANE_LIMIT) {
                self.wakes.held.insert(key, (cause, after_deposit));
                continue;
            }
            let budget = attachment
                .then(|| Self::outbox_send_size(&queue.head))
                .flatten();
            if let Some(bytes) = budget
                && !reserve(id, bytes)
            {
                self.wakes.held.insert(key, (cause, after_deposit));
                continue;
            }

            let profile =
                DeliveryProfile::parse(&queue.profile).unwrap_or(DeliveryProfile::Message);
            let method =
                delivery_method_name(self.delivery_method_for(&queue.destination, cycle, profile));
            if !db::claim_waiting(db, &identity, id, now, method) {
                if budget.is_some() {
                    release(id);
                }
                continue;
            }
            // Its settling, however it ends, is what lets the next of its queue go.
            self.wakes.woken.insert(id.clone(), key);
            match self.resumable_send(&queue.head, cycle, db) {
                Some(send) => {
                    if !attachment {
                        lane += 1;
                    }
                    self.resumable.push(send);
                    outcome.woken.push(Woken {
                        msg_id: id.clone(),
                        method,
                        cause,
                    });
                }
                // Claimed first: only a message on its way can end, and this one must.
                None => {
                    if budget.is_some() {
                        release(id);
                    }
                    match hex::decode(id).ok().and_then(|id| id.try_into().ok()) {
                        Some(hash) => self.fail_send(
                            hash,
                            DeliveryFailureReason::PreparationFailed,
                            &mut outcome.failed,
                        ),
                        None => outcome.failed.push((id.clone(), "failed")),
                    }
                }
            }
        }
        outcome
    }

    /// Whether a send that starts at the Offline Inbox can go now.
    fn inbox_ready(&self) -> bool {
        // Routes expire by the wall clock.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        self.client_propagation_enabled
            && self.router.outbound_propagation_node.is_some_and(|node| {
                self.known_identities.contains_key(&hex::encode(node))
                    && self.direct_route_entry(node, now).is_some()
            })
    }

    /// Whether a new message must wait its turn behind earlier ones of its queue.
    pub(super) fn waits_behind_earlier(
        &self,
        db_pool: &DbPool,
        identity_id: &str,
        destination: &str,
        preference: DeliveryPreference,
        profile: DeliveryProfile,
    ) -> bool {
        self.keep_trying
            && db::queue_blocks_new_send(
                db_pool,
                identity_id,
                destination,
                preference.as_str(),
                profile.as_str(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lxmf::tests::{known_route, test_manager};
    use crate::lxmf::{AttachmentMessageRequest, LxmfQueuedMessage, MessageSendRequest};
    use lxmf_core::message_api::DeliveryMethod;
    use r2d2_sqlite::SqliteConnectionManager;
    use rusqlite::params;
    use std::time::{SystemTime, UNIX_EPOCH};

    use DeliveryPreference::{Auto, Direct, Propagated};

    /// A manager with waiting on, its database, and a clock the test moves.
    struct Bench {
        mgr: LxmfManager,
        pool: DbPool,
        identity: String,
        now: f64,
        keep_trying: bool,
        online: bool,
        grant_budget: bool,
        reserved: Vec<(String, usize)>,
        released: Vec<String>,
    }

    fn peer(n: u8) -> String {
        hex::encode([n; 16])
    }

    fn wall() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    }

    impl Bench {
        fn new() -> Self {
            let pool = r2d2::Pool::builder()
                .max_size(2)
                .build(SqliteConnectionManager::memory())
                .unwrap();
            db::init_schema(&pool).unwrap();
            let mgr = test_manager();
            let mut bench = Self {
                identity: mgr.identity_hash.clone(),
                mgr,
                pool,
                now: wall(),
                keep_trying: true,
                online: true,
                grant_budget: true,
                reserved: Vec::new(),
                released: Vec::new(),
            };
            // The wake at start is made before anything waits.
            assert!(bench.wake(&[]).is_empty());
            bench
        }

        fn send(
            &mut self,
            to: &str,
            preference: DeliveryPreference,
            text: &str,
        ) -> LxmfQueuedMessage {
            self.mgr
                .send_message_with_preference_report(MessageSendRequest {
                    dest_hash_hex: to,
                    content: text,
                    title: "",
                    db_pool: &self.pool,
                    identity_id: &self.identity,
                    preference,
                    profile: DeliveryProfile::Message,
                    renderer: None,
                    thread: "",
                })
                .unwrap()
        }

        fn send_file(&mut self, to: &str, name: &str) -> LxmfQueuedMessage {
            self.mgr
                .send_message_with_attachment_fields_preference_report(AttachmentMessageRequest {
                    dest_hash_hex: to,
                    content: "",
                    title: "",
                    file_name: name,
                    file_bytes: name.as_bytes(),
                    staged_path: None,
                    is_image: false,
                    image_mime: "",
                    db_pool: &self.pool,
                    identity_id: &self.identity,
                    preference: Auto,
                    renderer: None,
                    thread: "",
                })
                .unwrap()
        }

        /// A message that waits: its first send failed for a network reason,
        /// or it was held behind an earlier one that waits.
        fn waiting(&mut self, to: &str, preference: DeliveryPreference, text: &str) -> String {
            let queued = self.send(to, preference, text);
            if !queued.waiting {
                self.network_failure(&queued.message_id);
            }
            queued.message_id
        }

        /// The network gives up on `id`, as the tick records it.
        fn network_failure(&mut self, id: &str) {
            self.mgr.cancel_outbound_message(id);
            let settled = db::settle_failure(
                &self.pool,
                id,
                &self.identity,
                true,
                true,
                self.now,
                OutboundState::Failed,
            );
            assert_eq!(
                settled.map(|settled| settled.state),
                Some(OutboundState::Waiting),
                "{id}"
            );
        }

        /// `id` ends as `state`, and nothing holds it any more.
        fn ends(&mut self, id: &str, state: &str) {
            self.mgr.cancel_outbound_message(id);
            assert!(
                db::update_message_state(&self.pool, id, &self.identity, state, None),
                "{id} {state}"
            );
        }

        fn run(&mut self, events: &[WakeEvent]) -> WakeOutcome {
            let mut signals = WakeSignals::default();
            for event in events {
                signals.raise(*event);
            }
            let grant = self.grant_budget;
            let (reserved, released) = (&mut self.reserved, &mut self.released);
            self.mgr.run_wakes(WakeContext {
                db: &self.pool,
                now: self.now,
                keep_trying: self.keep_trying,
                online: self.online,
                signals,
                reserve: &mut |id, bytes| {
                    reserved.push((id.to_string(), bytes));
                    grant
                },
                release: &mut |id| released.push(id.to_string()),
            })
        }

        /// One run of the engine; the woken ids with what woke them.
        fn wake(&mut self, events: &[WakeEvent]) -> Vec<(String, WakeCause)> {
            let outcome = self.run(events);
            assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
            outcome
                .woken
                .into_iter()
                .map(|woken| (woken.msg_id, woken.cause))
                .collect()
        }

        /// The tick's own work on what was woken; the ids the router then holds.
        fn sent(&mut self) -> Vec<String> {
            let mut results = Vec::new();
            self.mgr.queue_resumable(wall(), &mut results);
            assert!(results.is_empty(), "{results:?}");
            self.mgr
                .router
                .pending_outbound
                .iter()
                .map(|message| hex::encode(message.message_id()))
                .collect()
        }

        fn state(&self, id: &str) -> String {
            db::outbound_message_state(&self.pool, &self.identity, id)
                .unwrap()
                .unwrap()
        }

        /// `(queued_at, auto_sends, last_auto_at, preference)` of an outbox entry.
        fn entry(&self, id: &str) -> (f64, u32, f64, String) {
            self.pool
                .get()
                .unwrap()
                .query_row(
                    "SELECT queued_at, auto_sends, last_auto_at, preference
                     FROM lxmf_outbox WHERE message_id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap()
        }

        /// Give `id` a send time `age` seconds before the bench's clock.
        fn sent_ago(&self, id: &str, age: f64) {
            self.pool
                .get()
                .unwrap()
                .execute(
                    "UPDATE lxmf_outbox SET queued_at = ?1 WHERE message_id = ?2",
                    params![self.now - age, id],
                )
                .unwrap();
        }

        fn sql(&self, sql: &str, id: &str) {
            self.pool.get().unwrap().execute(sql, params![id]).unwrap();
        }

        fn heard(to: &str) -> WakeEvent {
            WakeEvent::Heard(hex::decode(to).unwrap().try_into().unwrap())
        }

        fn try_now(to: &str) -> WakeEvent {
            WakeEvent::TryNow(hex::decode(to).unwrap().try_into().unwrap())
        }

        fn new_send(to: &str) -> WakeEvent {
            WakeEvent::NewSend(hex::decode(to).unwrap().try_into().unwrap())
        }

        /// An Offline Inbox node in use, with its identity and a route to it.
        fn usable_inbox(&mut self) -> [u8; 16] {
            let node = [0xEE; 16];
            self.mgr.client_propagation_enabled = true;
            self.mgr.known_identities.insert(
                hex::encode(node),
                rns_identity::identity::Identity::new().get_public_key(),
            );
            known_route(&mut self.mgr, node);
            self.mgr.set_runtime_propagation_node(Some(node));
            node
        }
    }

    #[test]
    fn an_unreachable_recipient_does_not_delay_another() {
        let mut b = Bench::new();
        let (away, near) = (peer(1), peer(2));
        let a1 = b.waiting(&away, Auto, "a1");
        let a2 = b.waiting(&away, Auto, "a2");
        let n1 = b.waiting(&near, Auto, "n1");
        let n2 = b.waiting(&near, Auto, "n2");

        // One probe a recipient, the oldest message of each.
        assert_eq!(
            b.wake(&[WakeEvent::InterfaceOnline]),
            [
                (a1.clone(), WakeCause::Network),
                (n1.clone(), WakeCause::Network)
            ]
        );
        assert_eq!(b.sent(), [a1.clone(), n1.clone()]);

        b.network_failure(&a1);
        b.ends(&n1, "delivered");
        assert_eq!(b.wake(&[]), [(n2.clone(), WakeCause::Queue)]);
        assert_eq!(b.sent(), [n2.as_str()]);
        b.ends(&n2, "delivered");
        assert!(b.wake(&[]).is_empty());

        assert_eq!(b.state(&a1), "waiting");
        assert_eq!(b.state(&a2), "waiting");
        assert_eq!(b.entry(&a2).1, 0, "the one behind was never tried");
    }

    #[test]
    fn a_later_message_joins_behind_and_arrives_in_order() {
        let mut b = Bench::new();
        let to = peer(3);
        let first = b.waiting(&to, Auto, "first");

        let second = b.send(&to, Auto, "second");
        assert!(second.waiting);
        assert_eq!(b.state(&second.message_id), "waiting");
        assert!(b.sent().is_empty(), "it is not handed to the network");
        assert!(
            !b.mgr
                .auto_live_fallback
                .contains(&<[u8; 32]>::try_from(hex::decode(&second.message_id).unwrap()).unwrap())
        );

        // The new send wakes the queue, and the queue sends its oldest.
        assert_eq!(
            b.wake(&[Bench::new_send(&to)]),
            [(first.clone(), WakeCause::NewSend)]
        );
        assert_eq!(b.sent(), [first.as_str()]);

        // A wake's send on its way holds the queue as a waiting message does.
        let third = b.send(&to, Auto, "third");
        assert!(third.waiting);
        assert!(b.wake(&[Bench::new_send(&to)]).is_empty(), "one at a time");

        // The queue moves on by itself; the send that asked for it stays the cause on record.
        b.ends(&first, "delivered");
        assert_eq!(
            b.wake(&[]),
            [(second.message_id.clone(), WakeCause::NewSend)]
        );
        assert_eq!(b.sent(), [second.message_id.as_str()]);
        b.ends(&second.message_id, "delivered");
        assert_eq!(b.wake(&[]), [(third.message_id.clone(), WakeCause::Queue)]);
        assert_eq!(b.sent(), [third.message_id.as_str()]);
        b.ends(&third.message_id, "delivered");

        // With nothing waiting, a send goes out as it always has.
        let fourth = b.send(&to, Auto, "fourth");
        assert!(!fourth.waiting);
        assert_eq!(b.state(&fourth.message_id), "sending");
        assert_eq!(b.sent(), [fourth.message_id]);
    }

    #[test]
    fn a_rejected_head_leaves_and_the_next_goes_in_the_same_wake() {
        let mut b = Bench::new();
        let to = peer(4);
        let rejected = b.waiting(&to, Direct, "rejected");
        let unreadable = b.waiting(&to, Direct, "unreadable");
        let refused = b.waiting(&to, Direct, "refused");
        let last = b.waiting(&to, Direct, "last");
        b.sql(
            "UPDATE lxmf_outbox SET message = x'00' WHERE message_id = ?1",
            &unreadable,
        );

        assert_eq!(
            b.wake(&[Bench::try_now(&to)]),
            [(rejected.clone(), WakeCause::User)]
        );
        b.sent();
        b.ends(&rejected, "rejected");

        // Its record cannot be read: it is taken, fails for good, and does not block.
        let outcome = b.run(&[]);
        assert!(outcome.woken.is_empty());
        assert_eq!(outcome.failed, [(unreadable.clone(), "failed")]);
        let reason = b.mgr.take_failure_reasons()[&unreadable];
        assert_eq!(reason.code(), "preparation_failed");
        assert_eq!(b.state(&unreadable), "sending");
        let settled = db::settle_failure(
            &b.pool,
            &unreadable,
            &b.identity,
            reason.can_wait(),
            true,
            b.now,
            OutboundState::Failed,
        )
        .unwrap();
        assert_eq!(settled.state, OutboundState::Failed);

        assert_eq!(b.wake(&[]), [(refused.clone(), WakeCause::Queue)]);
        b.sent();
        // A failure that is not the network's ends it, and the queue moves on.
        b.mgr.cancel_outbound_message(&refused);
        let ended = db::settle_failure(
            &b.pool,
            &refused,
            &b.identity,
            false,
            true,
            b.now,
            OutboundState::Failed,
        );
        assert_eq!(ended.map(|ended| ended.state), Some(OutboundState::Failed));
        assert_eq!(b.wake(&[]), [(last.clone(), WakeCause::Queue)]);
        assert_eq!(b.sent(), [last]);
    }

    #[test]
    fn a_flapping_interface_makes_one_wake() {
        let mut b = Bench::new();
        let (first, later) = (peer(5), peer(6));
        let woken = b.waiting(&first, Auto, "woken by the first edge");
        assert_eq!(
            b.wake(&[WakeEvent::InterfaceOnline]),
            [(woken.clone(), WakeCause::Network)]
        );
        b.sent();
        b.network_failure(&woken);

        // A message that began to wait during the flap is not sent on its next edge.
        b.now += 10.0;
        let parked = b.waiting(&later, Auto, "parked during the flap");
        for _ in 0..3 {
            b.now += 5.0;
            assert!(b.wake(&[WakeEvent::InterfaceOnline]).is_empty());
        }
        assert_eq!(b.entry(&parked).1, 0);

        // Once the interface has stayed quiet long enough, an edge counts again.
        b.now += policy::INTERFACE_WAKE_MIN_SECS;
        let again = b.wake(&[WakeEvent::InterfaceOnline]);
        assert!(again.contains(&(parked, WakeCause::Network)), "{again:?}");
    }

    #[test]
    fn two_triggers_at_once_send_one_copy() {
        let mut b = Bench::new();
        let to = peer(7);
        let id = b.waiting(&to, Auto, "once");
        let every_trigger = [
            WakeEvent::InterfaceOnline,
            WakeEvent::Resumed,
            Bench::heard(&to),
            Bench::new_send(&to),
            Bench::try_now(&to),
        ];
        assert_eq!(b.wake(&every_trigger), [(id.clone(), WakeCause::User)]);
        assert!(
            b.wake(&every_trigger).is_empty(),
            "taken, before it is queued"
        );
        assert!(
            !db::claim_waiting(&b.pool, &b.identity, &id, b.now, "direct"),
            "a claim finds it taken"
        );
        assert_eq!(b.sent(), [id.as_str()]);
        assert!(
            b.wake(&every_trigger).is_empty(),
            "and while it is on its way"
        );
        assert_eq!(b.sent(), [id.as_str()], "one copy");
        assert_eq!(b.entry(&id).1, 1, "one wake");
        assert_eq!(b.state(&id), "sending");
    }

    #[test]
    fn a_reconnect_with_a_large_queue_does_not_delay_a_fresh_send() {
        let mut b = Bench::new();
        let parked = (10..20)
            .map(|n| b.waiting(&peer(n), Auto, "parked"))
            .collect::<Vec<_>>();

        let woken = b.wake(&[WakeEvent::InterfaceOnline]);
        assert_eq!(
            woken,
            [
                (parked[0].clone(), WakeCause::Network),
                (parked[1].clone(), WakeCause::Network)
            ],
            "the oldest two; the rest keep the links free"
        );
        assert_eq!(b.sent().len(), policy::LANE_LIMIT);

        // A message sent now goes straight out.
        let fresh = b.send(&peer(30), Auto, "fresh");
        assert!(!fresh.waiting);
        assert_eq!(b.state(&fresh.message_id), "sending");
        assert!(b.sent().contains(&fresh.message_id));

        // The rest follow as the woken ones settle, with no new signal.
        assert!(b.wake(&[]).is_empty());
        b.ends(&parked[0], "delivered");
        assert_eq!(b.wake(&[]), [(parked[2].clone(), WakeCause::Network)]);
        b.network_failure(&parked[1]);
        assert_eq!(b.wake(&[]), [(parked[3].clone(), WakeCause::Network)]);
        assert_eq!(
            parked.iter().filter(|id| b.state(id) == "sending").count(),
            policy::LANE_LIMIT
        );
    }

    #[test]
    fn messages_expire_with_no_network() {
        let mut b = Bench::new();
        let to = peer(8);
        let old = b.waiting(&to, Auto, "old");
        let newer = b.waiting(&to, Auto, "newer");
        b.sent_ago(&old, policy::WAIT_LIMIT_SECS);
        b.sent_ago(&newer, policy::WAIT_LIMIT_SECS - 3_600.0);
        b.online = false;

        b.now += policy::TIMER_PERIOD_SECS;
        let outcome = b.run(&[]);
        assert!(outcome.woken.is_empty());
        assert_eq!(
            outcome.expired,
            [db::ExpiredWait {
                msg_id: old.clone(),
                destination: to.clone(),
                delivery_method: db::get_message_delivery_method(&b.pool, &old, &b.identity),
            }]
        );
        assert_eq!(b.state(&old), "failed");
        assert_eq!(b.state(&newer), "waiting");
        assert!(b.run(&[]).expired.is_empty(), "ended once");

        // With waiting turned off the sweep still runs, and nothing else does.
        b.keep_trying = false;
        b.online = true;
        b.now += 3_600.0;
        let outcome = b.run(&[Bench::try_now(&to), WakeEvent::InterfaceOnline]);
        assert!(outcome.woken.is_empty());
        assert_eq!(outcome.expired.len(), 1);
        assert_eq!(b.state(&newer), "failed");
    }

    #[test]
    fn with_waiting_off_a_signal_wakes_nothing() {
        let mut b = Bench::new();
        let to = peer(9);
        let id = b.waiting(&to, Auto, "parked");
        b.sql(
            "UPDATE lxmf_outbox SET queued_at = queued_at + 30.0 WHERE message_id = ?1",
            &id,
        );
        let ahead = b.entry(&id).0;
        b.keep_trying = false;
        let outcome = b.run(&[Bench::try_now(&to), WakeEvent::InterfaceOnline]);
        assert!(outcome.woken.is_empty() && outcome.expired.is_empty());
        assert_eq!(b.state(&id), "waiting");
        assert_eq!(b.entry(&id).0, ahead, "no time is rewritten");
        let born = b.send(&to, Auto, "next");
        assert!(!born.waiting, "and a new send is not held");
        b.ends(&born.message_id, "delivered");

        // Turned on again, the engine starts over: the wake at start reaches it.
        b.keep_trying = true;
        assert_eq!(b.wake(&[]), [(id, WakeCause::Start)]);
    }

    #[test]
    fn a_restart_mid_queue_continues_in_order_and_resends_nothing_settled() {
        let mut b = Bench::new();
        let to = peer(11);
        let done = b.waiting(&to, Direct, "done");
        let flying = b.waiting(&to, Direct, "flying");
        let behind = b.waiting(&to, Direct, "behind");
        assert_eq!(b.wake(&[Bench::try_now(&to)]).len(), 1);
        b.sent();
        b.ends(&done, "delivered");
        assert_eq!(b.wake(&[]), [(flying.clone(), WakeCause::Queue)]);
        b.sent();

        // The app stops with the second on its way.
        let root = b.mgr.data_dir.parent().unwrap().to_path_buf();
        let identity = b.identity.clone();
        let stopped = std::mem::replace(&mut b.mgr, test_manager());
        drop(stopped);
        crate::lxmf::initialize_storage_namespace(&root).unwrap();
        b.mgr = LxmfManager::load_or_create(&root, Some(&identity), None).unwrap();
        assert_eq!(
            db::return_woken_to_waiting(&b.pool, &identity),
            [flying.as_str()]
        );
        let rows = db::outbox_rows(&b.pool, &identity).unwrap();
        assert!(rows.is_empty(), "nothing is sent by the start itself");
        assert_eq!(b.mgr.resume_outbox(rows, &b.pool), 0);

        // The start's wake respects the pause after the last attempt.
        b.now += 5.0;
        assert!(b.wake(&[]).is_empty());
        assert!(b.sent().is_empty());
        assert_eq!(b.state(&behind), "waiting");

        b.now += policy::TIMER_PERIOD_SECS;
        assert_eq!(b.wake(&[]), [(flying.clone(), WakeCause::Timer)]);
        assert_eq!(
            b.sent(),
            [flying.as_str()],
            "the delivered one is not sent again"
        );
        b.ends(&flying, "delivered");
        assert_eq!(b.wake(&[]), [(behind.clone(), WakeCause::Queue)]);
        assert_eq!(b.sent(), [behind]);
    }

    #[test]
    fn one_probe_per_queue_while_the_recipient_is_away() {
        let mut b = Bench::new();
        let to = peer(12);
        let head = b.waiting(&to, Auto, "head");
        let second = b.waiting(&to, Auto, "second");
        let third = b.waiting(&to, Auto, "third");
        let heard = [Bench::heard(&to)];

        assert_eq!(b.wake(&heard), [(head.clone(), WakeCause::Recipient)]);
        assert_eq!(b.sent(), [head.as_str()]);
        assert!(b.wake(&heard).is_empty(), "the probe is still out");
        assert!(b.sent().len() == 1);
        b.network_failure(&head);

        // Heard again at once: the head rests, and the next is not sent in its place.
        b.now += 10.0;
        assert!(b.wake(&heard).is_empty());
        b.now += 49.0;
        assert!(b.wake(&heard).is_empty(), "a minute, not less");
        b.now += 1.0;
        assert_eq!(b.wake(&heard), [(head.clone(), WakeCause::Recipient)]);
        b.sent();
        b.network_failure(&head);

        // The pause grows with every wake.
        b.now += 119.0;
        assert!(b.wake(&heard).is_empty());
        b.now += 1.0;
        assert_eq!(b.wake(&heard), [(head.clone(), WakeCause::Recipient)]);
        assert_eq!(b.entry(&head).1, 3);
        assert_eq!(b.entry(&second).1 + b.entry(&third).1, 0);
    }

    #[test]
    fn a_message_that_keeps_failing_ends_at_the_cap() {
        let mut b = Bench::new();
        let to = peer(13);
        let id = b.waiting(&to, Auto, "poison");
        let next = b.waiting(&to, Auto, "next");
        b.pool
            .get()
            .unwrap()
            .execute(
                "UPDATE lxmf_outbox SET auto_sends = ?1 WHERE message_id = ?2",
                params![policy::WAKE_CAP - 1, id],
            )
            .unwrap();

        assert_eq!(
            b.wake(&[Bench::try_now(&to)]),
            [(id.clone(), WakeCause::User)]
        );
        assert_eq!(
            b.entry(&id).1,
            policy::WAKE_CAP,
            "counted before it is sent"
        );
        b.sent();
        b.mgr.cancel_outbound_message(&id);
        let settled = db::settle_failure(
            &b.pool,
            &id,
            &b.identity,
            true,
            true,
            b.now,
            OutboundState::Failed,
        )
        .unwrap();
        assert_eq!(
            (settled.state, settled.verdict),
            (
                OutboundState::Failed,
                ratspeak_core::delivery::AfterFailure::FailExhausted
            )
        );
        assert_eq!(
            b.wake(&[]),
            [(next, WakeCause::Queue)],
            "and the queue moves on"
        );
    }

    #[test]
    fn switching_identity_leaves_the_other_queue_alone_and_its_clock_running() {
        let mut b = Bench::new();
        let to = peer(14);
        let mine = b.waiting(&to, Auto, "mine");
        for (id, age) in [
            ("theirs", 60.0),
            ("theirs-old", policy::WAIT_LIMIT_SECS + 60.0),
        ] {
            db::save_outbound_message(
                &b.pool,
                &db::OutboundMessage {
                    msg_id: id,
                    identity_id: "another identity",
                    source: "them",
                    destination: &to,
                    waiting: true,
                    resend: db::Resend {
                        preference: "auto",
                        profile: "message",
                        message: b"parts",
                    },
                    ..db::OutboundMessage::default()
                },
            )
            .unwrap();
            b.pool
                .get()
                .unwrap()
                .execute(
                    "UPDATE lxmf_outbox SET queued_at = ?1 WHERE message_id = ?2",
                    params![b.now - age, id],
                )
                .unwrap();
        }

        b.now += policy::TIMER_PERIOD_SECS;
        let outcome = b.run(&[Bench::try_now(&to), WakeEvent::InterfaceOnline]);
        assert_eq!(
            outcome
                .woken
                .iter()
                .map(|woken| woken.msg_id.as_str())
                .collect::<Vec<_>>(),
            [mine.as_str()]
        );
        assert!(
            outcome.expired.is_empty(),
            "its time is counted when it is next active"
        );
        for id in ["theirs", "theirs-old"] {
            let state = db::outbound_message_state(&b.pool, "another identity", id);
            assert_eq!(state, Ok(Some("waiting".into())), "{id}");
            assert_eq!(b.entry(id).1, 0, "{id}");
        }
        // It ends by its own clock, as soon as that identity runs.
        let expired = db::expire_waiting(&b.pool, "another identity", b.now).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].msg_id, "theirs-old");
    }

    #[test]
    fn an_auto_queue_behind_a_deposit_goes_straight_to_the_inbox() {
        let mut b = Bench::new();
        let to = peer(15);
        let deposited = b.waiting(&to, Auto, "deposited");
        let follower = b.waiting(&to, Auto, "follower");
        let last = b.waiting(&to, Auto, "last");
        b.usable_inbox();

        // The inbox coming into use is itself a wake; the head takes its usual cycle.
        assert_eq!(b.wake(&[]), [(deposited.clone(), WakeCause::Inbox)]);
        assert_ne!(
            db::get_message_delivery_method(&b.pool, &deposited, &b.identity).as_deref(),
            Some("propagated"),
            "live first"
        );
        b.sent();
        b.ends(&deposited, "propagated");

        assert_eq!(b.wake(&[]), [(follower.clone(), WakeCause::Queue)]);
        assert_eq!(
            db::get_message_delivery_method(&b.pool, &follower, &b.identity).as_deref(),
            Some("propagated")
        );
        assert_eq!(
            b.entry(&follower).3,
            "auto",
            "the method the user chose stays"
        );
        b.sent();
        let queued = b
            .mgr
            .router
            .pending_outbound
            .iter()
            .find(|message| hex::encode(message.message_id()) == follower)
            .expect("the follower is queued");
        assert_eq!(queued.method, DeliveryMethod::Propagated);

        // A follower of one that was delivered live takes its usual cycle again.
        b.ends(&follower, "delivered");
        assert_eq!(b.wake(&[]), [(last.clone(), WakeCause::Queue)]);
        assert_ne!(
            db::get_message_delivery_method(&b.pool, &last, &b.identity).as_deref(),
            Some("propagated")
        );
    }

    #[test]
    fn text_is_not_held_behind_an_attachment() {
        let mut b = Bench::new();
        let to = peer(16);
        let file = b.send_file(&to, "notes.txt").message_id;
        b.network_failure(&file);

        // A waiting file does not hold a new text; it does hold a new file.
        let text = b.send(&to, Auto, "text");
        assert!(!text.waiting);
        assert!(b.sent().contains(&text.message_id));
        let later_file = b.send_file(&to, "more.txt");
        assert!(later_file.waiting);

        // Text that waits and a file that waits are woken side by side.
        b.network_failure(&text.message_id);
        let woken = b.wake(&[Bench::try_now(&to)]);
        assert_eq!(
            woken,
            [
                (file.clone(), WakeCause::User),
                (text.message_id.clone(), WakeCause::User)
            ]
        );
        b.sent();

        // A file on its way holds neither a later text nor the text lane.
        b.ends(&text.message_id, "delivered");
        let next_text = b.waiting(&to, Auto, "next text");
        let other = b.waiting(&peer(17), Auto, "to someone else");
        let another = b.waiting(&peer(18), Auto, "and another");
        assert_eq!(b.state(&file), "sending");
        let woken = b
            .wake(&[
                Bench::try_now(&to),
                Bench::try_now(&peer(17)),
                Bench::try_now(&peer(18)),
            ])
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert_eq!(
            woken,
            [next_text, other],
            "two texts, with the file still out"
        );
        assert_eq!(b.state(&another), "waiting");
    }

    #[test]
    fn a_wake_sends_the_same_message_by_its_own_method() {
        let mut b = Bench::new();
        let to = peer(19);
        let id = b.send(&to, Direct, "the same").message_id;
        let wire = b.mgr.router.pending_outbound[0].wire_bytes().to_vec();
        b.network_failure(&id);
        let (queued_at, _, _, _) = b.entry(&id);

        b.now += 300.0;
        assert_eq!(
            b.wake(&[Bench::try_now(&to)]),
            [(id.clone(), WakeCause::User)]
        );
        assert_eq!(b.sent(), [id.as_str()]);
        let queued = &b.mgr.router.pending_outbound[0];
        assert_eq!(queued.method, DeliveryMethod::Direct);
        assert_eq!(
            queued.wire_bytes(),
            wire,
            "the same signed message, with its first timestamp"
        );
        assert_eq!(
            b.entry(&id),
            (queued_at, 1, b.now, "direct".to_string()),
            "its send time and method stay; the wake is counted"
        );
        assert_eq!(b.state(&id), "sending");
    }

    #[test]
    fn a_waiting_auto_message_keeps_its_inbox_fallback() {
        let mut b = Bench::new();
        let to = peer(20);
        let id = b.waiting(&to, Auto, "falls back");
        let hash = <[u8; 32]>::try_from(hex::decode(&id).unwrap()).unwrap();
        assert!(!b.mgr.auto_live_fallback.contains(&hash));

        b.wake(&[Bench::try_now(&to)]);
        b.sent();
        assert!(
            b.mgr.auto_live_fallback.contains(&hash),
            "the woken send may still go to the Offline Inbox when the live path ends"
        );

        // A message sent Direct is never moved to the inbox.
        let direct = b.waiting(&peer(21), Direct, "direct only");
        b.wake(&[Bench::try_now(&peer(21))]);
        b.sent();
        let direct = <[u8; 32]>::try_from(hex::decode(&direct).unwrap()).unwrap();
        assert!(!b.mgr.auto_live_fallback.contains(&direct));
    }

    #[test]
    fn a_message_still_held_is_not_woken() {
        let mut b = Bench::new();
        let to = peer(22);
        let id = b.waiting(&to, Auto, "held");
        let hash = <[u8; 32]>::try_from(hex::decode(&id).unwrap()).unwrap();
        let try_now = [Bench::try_now(&to)];

        // An earlier packet of it can still be proven.
        b.mgr
            .deferred_failures
            .insert(hash, DeliveryFailureReason::PathUnavailable);
        assert!(b.wake(&try_now).is_empty());
        assert_eq!(b.state(&id), "waiting");
        b.mgr.deferred_failures.remove(&hash);
        assert_eq!(
            b.wake(&[]),
            [(id.clone(), WakeCause::User)],
            "asked for once, it goes as soon as nothing holds it"
        );
        b.sent();
        b.network_failure(&id);

        // An ordinary send of its queue is still on its way.
        b.now += 3_600.0;
        let fresh = b.send(&peer(23), Auto, "fresh");
        b.sql(
            "UPDATE messages SET destination = (SELECT destination FROM messages WHERE id = ?1)
             WHERE id = (SELECT id FROM messages ORDER BY rowid DESC LIMIT 1)",
            &id,
        );
        assert!(
            b.wake(&try_now).is_empty(),
            "one message of a queue at a time"
        );
        b.ends(&fresh.message_id, "delivered");
        assert_eq!(b.wake(&[]), [(id, WakeCause::User)]);
    }

    #[test]
    fn a_path_or_an_announce_for_the_recipient_wakes_it_with_interfaces_up() {
        let mut b = Bench::new();
        let (heard, silent) = (peer(24), peer(25));
        let reached = b.waiting(&heard, Auto, "reached");
        let parked = b.waiting(&silent, Auto, "parked");

        // No interface came up; the recipient was heard.
        assert_eq!(
            b.wake(&[Bench::heard(&heard)]),
            [(reached, WakeCause::Recipient)]
        );
        assert_eq!(b.state(&parked), "waiting");
        assert_eq!(b.entry(&parked).1, 0);
    }

    #[test]
    fn try_now_and_a_new_send_need_no_network_and_nothing_else_goes_without_one() {
        let mut b = Bench::new();
        b.online = false;
        let ids = (26..31)
            .map(|n| b.waiting(&peer(n), Auto, "offline"))
            .collect::<Vec<_>>();
        assert!(
            b.wake(&[
                WakeEvent::InterfaceOnline,
                WakeEvent::Resumed,
                Bench::heard(&peer(26))
            ])
            .is_empty()
        );
        assert_eq!(
            b.wake(&[Bench::try_now(&peer(27)), Bench::new_send(&peer(28))]),
            [
                (ids[1].clone(), WakeCause::User),
                (ids[2].clone(), WakeCause::NewSend)
            ]
        );
    }

    #[test]
    fn an_inbox_message_waits_for_a_usable_inbox() {
        let mut b = Bench::new();
        let to = peer(31);
        // Sent with a node in use, which then went away.
        b.mgr.client_propagation_enabled = true;
        b.mgr.router.set_outbound_propagation_node(Some([0xEE; 16]));
        let id = b.send(&to, Propagated, "by the inbox").message_id;
        b.mgr.router.set_outbound_propagation_node(None);
        b.network_failure(&id);

        assert!(
            b.wake(&[Bench::try_now(&to)]).is_empty(),
            "nowhere to put it"
        );
        assert_eq!(b.entry(&id).1, 0, "and no wake is spent on it");

        let node = b.usable_inbox();
        assert_eq!(b.wake(&[]), [(id.clone(), WakeCause::Inbox)]);
        b.sent();
        b.network_failure(&id);

        // The node being heard again counts as the inbox being reachable.
        b.now += 61.0;
        assert_eq!(b.wake(&[WakeEvent::Heard(node)]), [(id, WakeCause::Inbox)]);
    }

    #[test]
    fn a_wake_waits_for_an_idle_stamp_queue() {
        let mut b = Bench::new();
        let (costly, to) = (peer(32), peer(33));
        let costly_bytes: [u8; 16] = hex::decode(&costly).unwrap().try_into().unwrap();
        let id = b.waiting(&to, Auto, "parked");

        // A fresh send is having its stamp computed: it comes first.
        b.mgr.router.set_stamp_cost(costly_bytes, 1.0);
        let fresh = b.send(&costly, Direct, "needs a stamp").message_id;
        let fresh_hash = <[u8; 32]>::try_from(hex::decode(&fresh).unwrap()).unwrap();
        assert!(
            b.mgr
                .router
                .pending_deferred_stamps
                .contains_key(&fresh_hash)
        );
        assert!(b.mgr.still_owned(&fresh), "a stamp in the making is held");
        assert!(b.wake(&[Bench::try_now(&to)]).is_empty());
        assert_eq!(b.state(&id), "waiting");

        b.mgr.cancel_outbound_message(&fresh);
        assert!(!b.mgr.still_owned(&fresh));
        assert_eq!(b.wake(&[]), [(id, WakeCause::User)]);
    }

    #[test]
    fn a_waiting_attachment_takes_the_budget() {
        let mut b = Bench::new();
        let to = peer(34);
        let file = b.send_file(&to, "report.pdf").message_id;
        b.network_failure(&file);
        let row = db::waiting_queues(&b.pool, &b.identity)
            .unwrap()
            .remove(0)
            .head;
        let size = LxmfManager::outbox_send_size(&row).unwrap();

        // No budget: it stays waiting, and is asked for again each tick.
        b.grant_budget = false;
        assert!(b.wake(&[Bench::try_now(&to)]).is_empty());
        assert!(b.wake(&[]).is_empty());
        assert_eq!(b.reserved, [(file.clone(), size), (file.clone(), size)]);
        assert_eq!(b.state(&file), "waiting");
        assert_eq!(b.entry(&file).1, 0);

        b.grant_budget = true;
        assert_eq!(b.wake(&[]), [(file.clone(), WakeCause::User)]);
        assert_eq!(b.reserved.len(), 3);
        assert!(
            b.released.is_empty(),
            "it holds its budget until it settles"
        );
        assert_eq!(b.sent(), [file]);

        // Text asks for none.
        let text = b.waiting(&peer(35), Auto, "text");
        b.wake(&[Bench::try_now(&peer(35))]);
        assert!(b.reserved.iter().all(|(id, _)| *id != text));
    }

    #[test]
    fn signals_keep_the_strongest_cause_and_overflow_to_every_queue() {
        let mut signals = WakeSignals::default();
        assert!(signals.is_empty());
        let peer = [7; 16];
        signals.raise(WakeEvent::Heard(peer));
        signals.raise(WakeEvent::TryNow(peer));
        signals.raise(WakeEvent::Heard(peer));
        assert_eq!(signals.peers.get(&peer), Some(&WakeCause::User));
        signals.raise(WakeEvent::InterfaceOnline);
        signals.raise(WakeEvent::Resumed);
        signals.raise(WakeEvent::InterfaceOnline);
        assert_eq!(signals.all, Some(WakeCause::Start));

        let mut flood = WakeSignals::default();
        for n in 0..=WakeSignals::MAX_PEERS as u32 {
            let mut hash = [0; 16];
            hash[..4].copy_from_slice(&n.to_be_bytes());
            flood.raise(WakeEvent::Heard(hash));
        }
        assert_eq!(flood.peers.len(), WakeSignals::MAX_PEERS);
        assert_eq!(
            flood.all,
            Some(WakeCause::Timer),
            "too many to name: every queue is looked at"
        );
    }
}
