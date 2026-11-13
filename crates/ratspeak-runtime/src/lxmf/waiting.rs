//! Wakes of waiting messages. A message that could not be sent for a network
//! reason waits in the database, held by nothing here; this takes the oldest
//! of each queue and hands it to the resume path as the same message.

use std::collections::HashMap;

use ratspeak_core::delivery::{OutboundState, Phase, WakeCause, WakeGate, policy};

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
    /// A message was left waiting since the last run.
    pub waiting_written: bool,
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
    /// A message of this identity may wait. While none does, a run reads nothing stored.
    may_wait: bool,
    last_sweep: Option<f64>,
    last_interface_wake: Option<f64>,
    /// Ids of the sends this engine woke and has not seen settle, with their
    /// queue and what woke them.
    woken: HashMap<String, (QueueKey, WakeCause)>,
    /// Wakes in a row of a queue that hearing its recipient prompted and that left it waiting.
    unanswered: HashMap<QueueKey, u32>,
    /// Queues a wake found busy: tried again each tick with the cause it had.
    held: HashMap<QueueKey, (WakeCause, bool)>,
    /// An Offline Inbox node was put in use since the last run.
    pub(super) inbox_selected: bool,
}

impl Wakes {
    /// A message was left waiting, or not.
    pub(super) fn note_waiting(&mut self, waits: bool) {
        self.may_wait |= waits;
    }
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
            waiting_written,
            mut signals,
            reserve,
            release,
        } = ctx;
        self.keep_trying = keep_trying;
        self.wakes.note_waiting(waiting_written);
        let identity = self.identity_hash.clone();
        let mut outcome = WakeOutcome::default();

        // The timer keeps its beat while idle, or the first message to wait would be tried at once.
        let sweep = !within(policy::TIMER_PERIOD_SECS, self.wakes.last_sweep, now);
        if sweep {
            self.wakes.last_sweep = Some(now);
        }
        // A woken send that settled lets the next of its queue go at once.
        let mut advance: HashMap<QueueKey, bool> = HashMap::new();
        for (id, (queue, cause)) in self.wakes.woken.clone() {
            // On its way: there is nothing to read yet.
            if self.still_owned(&id) {
                continue;
            }
            let Ok(stored) = db::outbound_message_state(db, &identity, &id) else {
                continue;
            };
            let state = stored.as_deref().and_then(OutboundState::parse);
            match state.map(OutboundState::phase) {
                Some(Phase::InFlight) => continue,
                // It waits again; its own gate decides when it is tried next.
                Some(Phase::Waiting) => {
                    if cause.gate() == WakeGate::Targeted {
                        *self.wakes.unanswered.entry(queue).or_default() += 1;
                    }
                }
                // Ended, done, deleted, or rewritten: the queue moves on.
                Some(Phase::Ended | Phase::Done) | None => {
                    self.wakes.unanswered.remove(&queue);
                    advance.insert(queue, state == Some(OutboundState::Propagated));
                }
            }
            self.wakes.woken.remove(&id);
        }
        if !self.wakes.may_wait {
            // The start counts as made, and what was heard while nothing waited is not kept.
            self.wakes.started = keep_trying && (self.wakes.started || online);
            self.wakes.inbox_selected = false;
            return outcome;
        }

        if keep_trying && !self.wakes.clamped {
            self.wakes.clamped = true;
            // Before the first sweep, or a clock that was set back would end messages late.
            db::clamp_future_queue_times(db, &identity, now);
        }
        // Expiry needs neither a network nor the setting.
        if sweep {
            outcome.expired = db::expire_waiting(db, &identity, now).unwrap_or_default();
        }
        if !keep_trying {
            // Turned on later, the engine starts over.
            self.wakes.started = false;
            self.wakes.held.clear();
            self.wakes.inbox_selected = false;
            if sweep {
                self.wakes.may_wait = db::has_waiting(db, &identity);
            }
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

        // With each sweep the queues are read too, so that an empty one is seen.
        if signals.is_empty()
            && !inbox_heard
            && advance.is_empty()
            && self.wakes.held.is_empty()
            && !sweep
        {
            return outcome;
        }
        let Ok(queues) = db::waiting_queues(db, &identity) else {
            return outcome;
        };
        self.wakes.may_wait = !queues.is_empty();
        let woken = &self.wakes.woken;
        self.wakes.unanswered.retain(|key, _| {
            queues.iter().any(|queue| {
                [&queue.destination, &queue.preference, &queue.profile]
                    == [&key[0], &key[1], &key[2]]
            }) || woken.values().any(|(queue, _)| queue == key)
        });
        let held = std::mem::take(&mut self.wakes.held);
        // Whether that node can take a deposit is found out in the router, as for any inbox send.
        let inbox_in_use = self.router.outbound_propagation_node.is_some();
        let stamps_idle = self.router.pending_deferred_stamps.is_empty()
            && self.router.active_deferred_stamp.is_none();
        let mut lane = self
            .wakes
            .woken
            .iter()
            .filter(|(id, (queue, _))| {
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
            let unanswered = self.wakes.unanswered.get(&key).copied().unwrap_or(0);
            let rest = policy::gate_secs(cause.gate(), queue.auto_sends, unanswered);
            if now - queue.last_auto_at < rest {
                continue;
            }
            if cause.needs_network() && !online {
                continue;
            }
            let cycle = if after_deposit
                && stored == DeliveryPreference::Auto
                && policy::FOLLOWERS_USE_INBOX_AFTER_DEPOSIT
                && inbox_in_use
            {
                DeliveryPreference::Propagated
            } else {
                stored
            };
            if cycle == DeliveryPreference::Propagated && !inbox_in_use {
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
            self.wakes.woken.insert(id.clone(), (key, cause));
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

    /// Why a new message is saved waiting and not handed to the network: it
    /// takes its turn behind earlier ones of its queue, or it goes by the
    /// Offline Inbox and no node is in use.
    pub(super) fn held_at_send(
        &self,
        db_pool: &DbPool,
        identity_id: &str,
        destination: &str,
        preference: DeliveryPreference,
        profile: DeliveryProfile,
        method: lxmf_core::message_api::DeliveryMethod,
    ) -> Option<DeliveryFailureReason> {
        if !self.keep_trying {
            return None;
        }
        if db::queue_blocks_new_send(
            db_pool,
            identity_id,
            destination,
            preference.as_str(),
            profile.as_str(),
        ) {
            return Some(DeliveryFailureReason::BehindEarlier);
        }
        (method == lxmf_core::message_api::DeliveryMethod::Propagated
            && self.router.outbound_propagation_node.is_none())
        .then(|| self.missing_inbox_reason())
        .filter(|reason| reason.can_wait())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lxmf::tests::{
        failure_code, finish_pending_propagation, known_route, ready_auto_propagation_fallback,
        test_manager,
    };
    use crate::lxmf::{
        AttachmentMessageRequest, LxmfQueuedMessage, LxmfSubmissionFailure, MessageSendRequest,
    };
    use lxmf_core::message_api::DeliveryMethod;
    use r2d2_sqlite::SqliteConnectionManager;
    use rusqlite::params;
    use std::time::{SystemTime, UNIX_EPOCH};

    use DeliveryPreference::{Auto, Direct, Propagated};

    /// Counts each time the database is asked for a connection.
    #[derive(Debug, Default)]
    struct Checkouts(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl r2d2::HandleEvent for Checkouts {
        fn handle_checkout(&self, _: r2d2::event::CheckoutEvent) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// A manager with waiting on, its database, and a clock the test moves.
    struct Bench {
        mgr: LxmfManager,
        pool: DbPool,
        /// How often the database has been used.
        checkouts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        identity: String,
        now: f64,
        keep_trying: bool,
        online: bool,
        /// The tick left a message waiting since the last run.
        written: bool,
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
            let checkouts = std::sync::Arc::default();
            let pool = r2d2::Pool::builder()
                .max_size(2)
                .event_handler(Box::new(Checkouts(std::sync::Arc::clone(&checkouts))))
                .build(SqliteConnectionManager::memory())
                .unwrap();
            db::init_schema(&pool).unwrap();
            let mgr = test_manager();
            let mut bench = Self {
                identity: mgr.identity_hash.clone(),
                mgr,
                pool,
                checkouts,
                now: wall(),
                keep_trying: true,
                online: true,
                written: false,
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
            self.try_send(to, preference, text).unwrap()
        }

        fn try_send(
            &mut self,
            to: &str,
            preference: DeliveryPreference,
            text: &str,
        ) -> Result<LxmfQueuedMessage, LxmfSubmissionFailure> {
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
            if queued.waiting.is_none() {
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
            // As the tick does for a message it leaves waiting.
            self.written = true;
        }

        fn database_uses(&self) -> usize {
            self.checkouts.load(std::sync::atomic::Ordering::Relaxed)
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
                waiting_written: std::mem::take(&mut self.written),
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
        assert_eq!(
            second.waiting.map(DeliveryFailureReason::code),
            Some("behind_earlier")
        );
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
        assert!(third.waiting.is_some());
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
        assert!(fourth.waiting.is_none());
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
        assert!(fresh.waiting.is_none());
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
        assert!(born.waiting.is_none(), "and a new send is not held");
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
        // The start's one look at whether anything waits.
        b.written = db::has_waiting(&b.pool, &identity);
        assert!(b.written);
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
        b.now += 109.0;
        assert!(
            b.wake(&heard).is_empty(),
            "two minutes after one unanswered"
        );
        b.now += 1.0;
        assert_eq!(b.wake(&heard), [(head.clone(), WakeCause::Recipient)]);
        b.sent();
        b.network_failure(&head);

        // The pause grows with every answer to its being heard that came to nothing.
        b.now += 239.0;
        assert!(b.wake(&heard).is_empty());
        b.now += 1.0;
        assert_eq!(b.wake(&heard), [(head.clone(), WakeCause::Recipient)]);
        assert_eq!(b.entry(&head).1, 3);
        assert_eq!(b.entry(&second).1 + b.entry(&third).1, 0);
    }

    #[test]
    fn blind_tries_do_not_slow_the_answer_to_the_recipient_being_heard() {
        let mut b = Bench::new();
        let to = peer(51);
        let id = b.waiting(&to, Auto, "long away");
        let heard = [Bench::heard(&to)];

        // Hours of the timer's tries, each coming to nothing.
        for _ in 0..8 {
            b.now += 3_601.0;
            assert_eq!(b.wake(&[]), [(id.clone(), WakeCause::Timer)]);
            b.sent();
            b.network_failure(&id);
        }
        assert_eq!(b.entry(&id).1, 8);

        // The recipient is heard: a minute after the last try is enough.
        b.now += 59.0;
        assert!(b.wake(&heard).is_empty());
        b.now += 1.0;
        assert_eq!(b.wake(&heard), [(id, WakeCause::Recipient)]);
    }

    #[test]
    fn the_pause_after_an_unanswered_answer_is_kept_by_other_tries_and_ends_with_any_other_outcome()
    {
        let mut b = Bench::new();
        let to = peer(52);
        let head = b.waiting(&to, Auto, "head");
        let next = b.waiting(&to, Auto, "next");
        let heard = [Bench::heard(&to)];
        // Two answers to the recipient being heard come to nothing: four minutes.
        for pause in [0.0, 120.0] {
            b.now += pause;
            assert_eq!(b.wake(&heard), [(head.clone(), WakeCause::Recipient)]);
            b.sent();
            b.network_failure(&head);
        }

        // A try the user asked for that also fails neither adds to the pause nor ends it.
        assert_eq!(
            b.wake(&[Bench::try_now(&to)]),
            [(head.clone(), WakeCause::User)]
        );
        b.sent();
        b.network_failure(&head);
        b.now += 239.0;
        assert!(b.wake(&heard).is_empty());
        b.now += 1.0;
        assert_eq!(b.wake(&heard), [(head.clone(), WakeCause::Recipient)]);
        b.sent();

        // It gets through: the queue's next message is answered within the minute again.
        b.ends(&head, "delivered");
        assert_eq!(b.wake(&[]), [(next.clone(), WakeCause::Queue)]);
        b.sent();
        b.network_failure(&next);
        b.now += 60.0;
        assert_eq!(b.wake(&heard), [(next, WakeCause::Recipient)]);
    }

    #[test]
    fn with_nothing_waiting_a_run_does_not_touch_the_database() {
        let mut b = Bench::new();
        let to = peer(53);
        let every = [
            WakeEvent::InterfaceOnline,
            WakeEvent::Resumed,
            Bench::heard(&to),
            Bench::try_now(&to),
        ];
        b.mgr.client_propagation_enabled = true;
        let uses = b.database_uses();
        for waits in [true, false, true] {
            b.keep_trying = waits;
            b.now += policy::TIMER_PERIOD_SECS + 1.0;
            assert!(b.wake(&every).is_empty());
            b.mgr.set_runtime_propagation_node(Some([0xEE; 16]));
            assert!(b.wake(&[]).is_empty());
        }
        assert_eq!(b.database_uses(), uses, "not a sweep, not a queue read");

        // A message left waiting is work: it is read, and sent when its recipient is heard.
        let id = b.waiting(&to, Auto, "parked");
        assert!(
            b.wake(&[]).is_empty(),
            "nothing from while it did not wait sends it: not the timer, not the node put in use"
        );
        let uses = b.database_uses();
        assert_eq!(
            b.wake(&[Bench::heard(&to)]),
            [(id.clone(), WakeCause::Recipient)]
        );
        assert!(b.database_uses() > uses);
        b.sent();

        // On its way, with nothing else waiting: nothing is read until it settles.
        assert!(b.wake(&[]).is_empty());
        let uses = b.database_uses();
        for _ in 0..3 {
            assert!(b.wake(&[]).is_empty());
        }
        assert_eq!(b.database_uses(), uses);

        // It settles: one look at how, one at the queues, and the engine is idle again.
        b.ends(&id, "delivered");
        let uses = b.database_uses();
        assert!(b.wake(&[]).is_empty());
        assert_eq!(b.database_uses(), uses + 2);
        b.now += policy::TIMER_PERIOD_SECS + 1.0;
        assert!(b.wake(&every).is_empty());
        assert_eq!(b.database_uses(), uses + 2);

        // With waiting off, the next sweep finds that none waits, and that is the last look.
        let ended = b.waiting(&to, Auto, "ended by the switch");
        b.ends(&ended, "failed");
        b.keep_trying = false;
        b.now += policy::TIMER_PERIOD_SECS + 1.0;
        assert!(b.wake(&[]).is_empty());
        let uses = b.database_uses();
        b.now += policy::TIMER_PERIOD_SECS + 1.0;
        assert!(b.wake(&every).is_empty());
        assert_eq!(b.database_uses(), uses);
    }

    #[test]
    fn a_lone_message_keeps_its_pause_while_it_is_on_its_way() {
        let mut b = Bench::new();
        let to = peer(54);
        let id = b.waiting(&to, Auto, "alone in its queue");
        let heard = [Bench::heard(&to)];
        assert_eq!(b.wake(&heard), [(id.clone(), WakeCause::Recipient)]);
        b.sent();
        b.network_failure(&id);
        b.now += 120.0;
        assert_eq!(b.wake(&heard), [(id.clone(), WakeCause::Recipient)]);
        b.sent();

        // Heard again while the try is out: its queue has no waiting message to show.
        assert!(b.wake(&heard).is_empty());
        b.network_failure(&id);
        b.now += 239.0;
        assert!(b.wake(&heard).is_empty(), "two unanswered: four minutes");
        b.now += 1.0;
        assert_eq!(b.wake(&heard), [(id, WakeCause::Recipient)]);
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

        // So does the follower of a deposit once no node is in use.
        b.sent();
        b.network_failure(&last);
        let behind = b.send(&to, Auto, "behind").message_id;
        b.now += 61.0;
        assert_eq!(
            b.wake(&[Bench::heard(&to)]),
            [(last.clone(), WakeCause::Recipient)]
        );
        b.sent();
        b.mgr.set_runtime_propagation_node(None);
        b.ends(&last, "propagated");
        assert_eq!(b.wake(&[]), [(behind.clone(), WakeCause::Queue)]);
        assert_ne!(
            db::get_message_delivery_method(&b.pool, &behind, &b.identity).as_deref(),
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
        assert!(text.waiting.is_none());
        assert!(b.sent().contains(&text.message_id));
        let later_file = b.send_file(&to, "more.txt");
        assert!(later_file.waiting.is_some());

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
    fn an_inbox_message_waits_for_a_node_and_the_router_finds_out_whether_it_can_be_used() {
        let mut b = Bench::new();
        let to = peer(31);
        let node = [0xEE; 16];
        // Sent with a node in use, which then went away.
        b.mgr.client_propagation_enabled = true;
        b.mgr.router.set_outbound_propagation_node(Some(node));
        let id = b.send(&to, Propagated, "by the inbox").message_id;
        b.mgr.router.set_outbound_propagation_node(None);
        b.network_failure(&id);

        assert!(
            b.wake(&[Bench::try_now(&to)]).is_empty(),
            "nowhere to put it"
        );
        assert_eq!(b.entry(&id).1, 0, "and no wake is spent on it");

        // A node in use is enough to try: nothing is known of it yet.
        b.mgr.set_runtime_propagation_node(Some(node));
        assert_eq!(b.wake(&[]), [(id.clone(), WakeCause::Inbox)]);
        assert_eq!(b.sent(), [id.as_str()]);
        assert_eq!(
            b.mgr.router.pending_outbound[0].method,
            DeliveryMethod::Propagated
        );

        // It is given as long there as a send made just now, and then waits again.
        let start = wall();
        let limit = b.mgr.discovery_limit(node).as_secs_f64();
        assert!(b.mgr.run_router_outbound(start).is_empty());
        let hash = <[u8; 32]>::try_from(hex::decode(&id).unwrap()).unwrap();
        assert_eq!(
            b.mgr.run_router_outbound(start + limit),
            [(id.clone(), "failed")]
        );
        assert_eq!(failure_code(&mut b.mgr, hash), Some("inbox_unreachable"));
        b.network_failure(&id);

        // The node being heard again counts as the inbox being reachable.
        b.now += 121.0;
        assert_eq!(b.wake(&[WakeEvent::Heard(node)]), [(id, WakeCause::Inbox)]);
    }

    #[test]
    fn an_offline_inbox_send_with_no_node_in_use_is_saved_waiting() {
        let mut b = Bench::new();
        let to = peer(41);
        // On, with no node found yet.
        b.mgr.client_propagation_enabled = true;

        let held = b.send(&to, Propagated, "nowhere to put it yet");
        assert_eq!(
            held.waiting.map(DeliveryFailureReason::code),
            Some("inbox_unreachable")
        );
        assert_eq!(b.state(&held.message_id), "waiting");
        assert!(b.sent().is_empty(), "it is not handed to the network");
        assert!(b.wake(&[Bench::new_send(&to)]).is_empty());
        assert_eq!(b.entry(&held.message_id).1, 0, "no wake is spent on it");

        let behind = b.send(&to, Propagated, "behind it");
        assert_eq!(
            behind.waiting.map(DeliveryFailureReason::code),
            Some("behind_earlier")
        );
        let live = b.send(&to, Auto, "by the live path");
        assert!(live.waiting.is_none(), "a send that does not start there");
        b.ends(&live.message_id, "delivered");

        b.mgr.set_runtime_propagation_node(Some([0xEE; 16]));
        assert_eq!(
            b.wake(&[]),
            [(held.message_id.clone(), WakeCause::Inbox)],
            "a node coming into use sends it"
        );
        assert_eq!(b.sent(), [held.message_id.as_str()]);

        // Refused as it always was where it cannot wait.
        let mut off = Bench::new();
        assert!(off.try_send(&to, Propagated, "the inbox is off").is_err());
        off.mgr.client_propagation_enabled = true;
        off.keep_trying = false;
        off.run(&[]);
        assert!(off.try_send(&to, Propagated, "waiting is off").is_err());
        assert_eq!(
            db::waiting_queues(&off.pool, &off.identity).unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn an_offline_inbox_send_made_with_the_inbox_out_of_reach_waits_and_deposits_when_it_returns()
     {
        let mut b = Bench::new();
        let recipient = [42; 16];
        let (to, node) = (hex::encode(recipient), [0xEF; 16]);
        let (tx, _transport) = tokio::sync::mpsc::channel(64);
        b.mgr.router.set_transport(tx);
        b.mgr.known_identities.insert(
            to.clone(),
            rns_identity::identity::Identity::new().get_public_key(),
        );
        // In use, and never heard: no key of its own, no route, no terms.
        b.mgr.client_propagation_enabled = true;
        b.mgr.set_runtime_propagation_node(Some(node));

        let sent = b.send(&to, Propagated, "for when the node is back");
        assert!(sent.waiting.is_none(), "it is taken and tried");
        let id = sent.message_id;
        let hash = <[u8; 32]>::try_from(hex::decode(&id).unwrap()).unwrap();
        let start = wall();
        let limit = b.mgr.discovery_limit(node).as_secs_f64();
        assert!(b.mgr.run_router_outbound(start).is_empty());
        assert_eq!(
            b.mgr.run_router_outbound(start + limit),
            [(id.clone(), "failed")]
        );
        assert_eq!(failure_code(&mut b.mgr, hash), Some("inbox_unreachable"));
        b.network_failure(&id);
        assert_eq!(b.state(&id), "waiting");

        let _transport = ready_auto_propagation_fallback(&mut b.mgr, recipient, node);
        assert_eq!(
            b.wake(&[WakeEvent::Heard(node)]),
            [(id.clone(), WakeCause::Inbox)]
        );
        assert_eq!(b.sent(), [id.as_str()]);
        let results = finish_pending_propagation(&mut b.mgr, Vec::new()).await;
        assert_eq!(results, [(id, "propagating")]);
        assert_eq!(b.mgr.in_flight_propagation.get(&hash), Some(&node));
    }

    #[test]
    fn retrying_an_inbox_message_with_no_node_in_use_waits_as_a_new_send_does() {
        let mut b = Bench::new();
        let to = peer(44);
        let id = b.send(&to, Direct, "not delivered").message_id;
        b.ends(&id, "failed");
        let row = db::retryable_outbox_row(&b.pool, &b.identity, &id)
            .unwrap()
            .unwrap();
        let retry = |b: &mut Bench| {
            b.mgr
                .retry_outbound(&b.pool, &b.identity, &row, Propagated)
                .map_err(str::to_owned)
        };

        // Refused where it cannot wait: the Offline Inbox off, or waiting off.
        assert!(retry(&mut b).is_err());
        b.mgr.client_propagation_enabled = true;
        b.keep_trying = false;
        b.run(&[]);
        assert!(retry(&mut b).is_err());
        assert_eq!(b.state(&id), "failed");
        b.keep_trying = true;
        b.run(&[]);

        assert_eq!(retry(&mut b), Ok(DeliveryMethod::Propagated));
        assert_eq!(b.state(&id), "sending", "taken");
        assert!(b.mgr.router.pending_outbound.is_empty());

        // The tick finds it has nowhere to go, and it waits as any such send does.
        let hash = <[u8; 32]>::try_from(hex::decode(&id).unwrap()).unwrap();
        let mut results = Vec::new();
        b.mgr.queue_resumable(wall(), &mut results);
        assert_eq!(results, [(id.clone(), "failed")]);
        assert_eq!(failure_code(&mut b.mgr, hash), Some("inbox_unreachable"));
        b.network_failure(&id);
        assert_eq!(b.entry(&id).3, "propagated");

        b.mgr.set_runtime_propagation_node(Some([0xEE; 16]));
        assert_eq!(b.wake(&[]), [(id.clone(), WakeCause::Inbox)]);
        assert_eq!(b.sent(), [id.as_str()]);
    }

    #[test]
    fn the_inbox_card_and_the_sync_do_not_change() {
        let Bench {
            mut mgr,
            pool,
            identity,
            ..
        } = Bench::new();
        let (recipient, node) = ([43; 16], [0xED; 16]);
        let (tx, _transport) = tokio::sync::mpsc::channel(64);
        mgr.router.set_transport(tx);
        mgr.known_identities.insert(
            hex::encode(recipient),
            rns_identity::identity::Identity::new().get_public_key(),
        );
        mgr.client_propagation_enabled = true;
        mgr.set_runtime_propagation_node(Some(node));
        mgr.last_propagation_check =
            wall() - crate::lxmf::AUTO_PROPAGATION_CHECK_INTERVAL_SECS - 1.0;
        let root = mgr.data_dir.parent().unwrap().to_path_buf();
        let state = crate::state::AppState::new(
            crate::config::DashboardConfig {
                data_dir: root.join(".ratspeak"),
                rns_config_dir: root.join(".ratspeak").join("reticulum"),
                data_root: root,
                rns_config_dir_overridden: false,
                max_log_entries: 200,
            },
            pool.clone(),
            std::sync::Arc::new(ratspeak_core::NoopEmitter),
            std::sync::Arc::new(ratspeak_core::NoopNotifier),
        );
        // What the card shows, and whether the inbox is due to be checked.
        let seen = |mgr: LxmfManager| {
            let due = mgr.auto_propagation_check_due(true);
            *state.lxmf.lock().unwrap() = Some(mgr);
            let card = crate::propagation::card_fingerprint(&state, true);
            (card, due, state.lxmf.lock().unwrap().take().unwrap())
        };
        let (card, due, mut mgr) = seen(mgr);
        assert!(due);

        let id = mgr
            .send_message_with_preference_report(MessageSendRequest {
                dest_hash_hex: &hex::encode(recipient),
                content: "left waiting",
                title: "",
                db_pool: &pool,
                identity_id: &identity,
                preference: Propagated,
                profile: DeliveryProfile::Message,
                renderer: None,
                thread: "",
            })
            .unwrap()
            .message_id;
        let start = wall();
        let limit = mgr.discovery_limit(node).as_secs_f64();
        assert!(mgr.run_router_outbound(start).is_empty());
        assert_eq!(
            mgr.run_router_outbound(start + limit),
            [(id.clone(), "failed")]
        );
        let settled = db::settle_failure(
            &pool,
            &id,
            &identity,
            true,
            true,
            start,
            OutboundState::Failed,
        );
        assert_eq!(
            settled.map(|settled| settled.state),
            Some(OutboundState::Waiting)
        );

        let (card_after, due_after, mgr) = seen(mgr);
        assert_eq!(card_after, card);
        assert_eq!(due_after, due);
        assert!(!mgr.propagated_deposit_pending());
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
