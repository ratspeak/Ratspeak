//! RRC hub service: hosts a Reticulum Relay Chat hub (`rrc.hub`) so remote
//! clients can connect, join rooms, and relay through this node.
//!
//! Relay traffic is live-only: message bodies, transcripts, and rosters
//! never reach the Ratspeak database. What persists is operator policy — the
//! `channel_hub_*` registry of registered rooms, their grants, and hub-level
//! klines — plus a verify-only digest of each room join key, never the key.
//!
//! Rooms exist only because the hub operator made them; a join naming an
//! unknown room is refused rather than founding one, so no remote peer can
//! write to the operator's registry.
//!
//! Protocol behavior follows the reference daemon (kc1awv/rrcd 0.3.2) except
//! where the fix registry records a deliberate deviation (idempotent re-HELLO
//! is one: a duplicate HELLO re-welcomes without wiping room membership).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, Instant};

use ciborium::value::Value;
use rns_identity::identity::Identity;
use rns_link::link::CloseReason;
use rns_runtime::destination_runtime::{
    DestinationRuntimeOptions, IdentityGatePolicy, RegisteredDestination,
};
use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::link_manager::DestinationAnnounceOptions;
use rns_transport::messages::TransportMessage;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use crate::db;
use crate::rrc;
use crate::state::AppState;
use ratspeak_core::Emitter;

const COMMAND_BUFFER: usize = 16;
/// Client HELLO retries fire at 3s; WELCOME must beat the first retry.
/// CBOR text headers grow with length; two bytes covers every body we emit.
const CBOR_TEXT_HEADER_SLACK: usize = 2;
/// Floor so a pathological room name cannot reduce a notice to nothing.
const MIN_NOTICE_BODY_BYTES: usize = 32;
/// Per-line topic budget in `/list`, which must stay one packet.
const LIST_TOPIC_BYTES: usize = 48;
/// Encoded envelopes ride single link packets; the negotiated floor is 431.
const LINK_PACKET_BUDGET: usize = rns_wire::constants::LINK_MDU;
const HUB_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const DEFAULT_HUB_NAME: &str = "Ratspeak hub";
pub const DEFAULT_PING_INTERVAL_SECS: u64 = 55;
pub const DEFAULT_PING_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone)]
pub struct ChannelHubConfig {
    pub hub_name: String,
    pub greeting: Option<String>,
    /// 0 disables periodic announces; an announce is always sent on start.
    pub announce_interval_secs: u64,
    /// 0 disables hub-driven keepalive PINGs (reference default; ours is on).
    pub ping_interval_secs: u64,
    pub ping_timeout_secs: u64,
    pub max_nick_bytes: usize,
    pub max_room_name_bytes: usize,
    pub max_message_body_bytes: usize,
    pub max_rooms_per_session: usize,
    pub rate_messages_per_minute: usize,
    pub include_member_list: bool,
    pub enable_resource_transfer: bool,
    pub max_resource_bytes: u64,
    /// Server operators (implicit ops in every room). The hosting identity is
    /// always seeded here so the operator can administer through any client.
    pub server_operators: Vec<[u8; 16]>,
    /// Hub-level bans applied at LINKIDENTIFY, editable live via `/kline`.
    pub banned_identities: Vec<[u8; 16]>,
    /// Invite lifetime for `/invite add` (reference default 900s).
    pub invite_timeout_secs: u64,
    /// Drop a registered room nobody has used in this long. 0 disables.
    pub room_registry_prune_after_secs: u64,
    pub room_registry_prune_interval_secs: u64,
    /// Sanity cap on the registry; room creation is already operator-only.
    pub max_registered_rooms: usize,
}

impl Default for ChannelHubConfig {
    fn default() -> Self {
        Self {
            hub_name: DEFAULT_HUB_NAME.to_string(),
            greeting: None,
            announce_interval_secs: 0,
            ping_interval_secs: DEFAULT_PING_INTERVAL_SECS,
            ping_timeout_secs: DEFAULT_PING_TIMEOUT_SECS,
            max_nick_bytes: 32,
            max_room_name_bytes: 64,
            max_message_body_bytes: 350,
            max_rooms_per_session: 32,
            rate_messages_per_minute: 240,
            include_member_list: true,
            enable_resource_transfer: true,
            max_resource_bytes: 256 * 1024,
            server_operators: Vec::new(),
            banned_identities: Vec::new(),
            invite_timeout_secs: 900,
            room_registry_prune_after_secs: 30 * 24 * 3600,
            room_registry_prune_interval_secs: 3600,
            max_registered_rooms: 256,
        }
    }
}

/// Content-free relay counters surfaced by `/stats`.
#[derive(Default)]
struct HubStats {
    joins: u64,
    parts: u64,
    messages_forwarded: u64,
    notices_forwarded: u64,
    actions_forwarded: u64,
    direct_notices: u64,
    rate_limited: u64,
    bad_packets: u64,
    duplicates: u64,
    pings_out: u64,
    pongs_in: u64,
    /// Envelopes the shell could not send. Should stay zero; a nonzero value
    /// means a producer escaped the budget helpers.
    oversize: u64,
}

/// Serializable hub status for IPC and the `channel_hub_snapshot` event.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelHubSnapshot {
    pub running: bool,
    pub hub_name: String,
    pub destination_hash: Option<String>,
    pub sessions: usize,
    pub welcomed_sessions: usize,
    pub rooms: usize,
    pub registered_rooms: usize,
    /// True while a durable registry write is outstanding.
    pub registry_degraded: bool,
    pub announce_interval_secs: u64,
    pub updated_at_ms: u64,
}

impl ChannelHubSnapshot {
    pub fn stopped() -> Self {
        Self {
            running: false,
            hub_name: String::new(),
            destination_hash: None,
            sessions: 0,
            welcomed_sessions: 0,
            rooms: 0,
            registered_rooms: 0,
            registry_degraded: false,
            announce_interval_secs: 0,
            updated_at_ms: now_ms(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelHubError {
    #[error("channel hub is not running")]
    Stopped,
    #[error("hub identity is unavailable: {0}")]
    Identity(String),
    #[error("hub destination registration failed: {0}")]
    Registration(String),
    #[error("hub registry is unavailable: {0}")]
    Registry(String),
}

enum HubCommand {
    Status {
        result_tx: oneshot::Sender<ChannelHubSnapshot>,
    },
    Shutdown {
        result_tx: oneshot::Sender<()>,
    },
}

/// Recently seen inbound envelope ids. The reference hub never reads message
/// ids; we drop replays so fan-out cost cannot be amplified by repetition.
const SEEN_ID_LIMIT: usize = 512;

const ROOM_KEY_TAG: &[u8] = b"ratspeak-rrc-roomkey-v1";
const ROOM_KEY_PEPPER_INFO: &[u8] = b"ratspeak-rrc-roomkey-pepper-v1";
/// Short keys are trivially brute-forced if the database ever leaks without
/// the keyfile, and nothing here stretches them.
const MIN_ROOM_KEY_BYTES: usize = 8;

/// A room join key as it is held and stored: salted, peppered, verify-only.
/// The reference keeps plaintext in memory and on disk; we never do.
#[derive(Clone, PartialEq)]
pub(crate) struct RoomKeyDigest {
    pub salt: [u8; 16],
    pub mac: [u8; 32],
    pub pepper_id: [u8; 8],
}

/// Hand-written: `HubSend` derives `Debug`, and a derived one here would put
/// the MAC into any log or panic that renders a send.
impl std::fmt::Debug for RoomKeyDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomKeyDigest(redacted)")
    }
}

/// Hub-local pepper derived from the hub identity, so a stolen database alone
/// cannot brute-force low-entropy room keys.
fn room_key_pepper(identity: &Identity) -> Option<Zeroizing<[u8; 32]>> {
    let private = identity.get_private_key()?;
    let derived = Zeroizing::new(
        rns_crypto::hkdf::hkdf_sha256(32, &*private, None, Some(ROOM_KEY_PEPPER_INFO)).ok()?,
    );
    let mut pepper = Zeroizing::new([0u8; 32]);
    pepper.copy_from_slice(&derived[..32]);
    Some(pepper)
}

/// `pepper_id` is part of the preimage, so a digest written under a different
/// hub identity cannot verify by construction.
fn room_key_preimage(pepper_id: [u8; 8], room: &str, key: &str, salt: &[u8; 16]) -> Vec<u8> {
    let mut preimage = Vec::with_capacity(ROOM_KEY_TAG.len() + 28 + room.len() + key.len());
    preimage.extend_from_slice(ROOM_KEY_TAG);
    preimage.extend_from_slice(salt);
    preimage.extend_from_slice(&pepper_id);
    preimage.extend_from_slice(&(room.len() as u32).to_be_bytes());
    preimage.extend_from_slice(room.as_bytes());
    preimage.extend_from_slice(key.as_bytes());
    preimage
}

fn room_key_digest(
    pepper: &[u8; 32],
    pepper_id: [u8; 8],
    room: &str,
    key: &str,
    salt: [u8; 16],
) -> RoomKeyDigest {
    let mac =
        rns_crypto::hmac::hmac_sha256(pepper, &room_key_preimage(pepper_id, room, key, &salt));
    RoomKeyDigest {
        salt,
        mac,
        pepper_id,
    }
}

fn decode_room_key(row: &db::HubRoomRow) -> Option<RoomKeyDigest> {
    if row.key_mac.is_empty() {
        return None;
    }
    let salt = <[u8; 16]>::try_from(hex::decode(&row.key_salt).ok()?).ok()?;
    let mac = <[u8; 32]>::try_from(hex::decode(&row.key_mac).ok()?).ok()?;
    let pepper_id = <[u8; 8]>::try_from(hex::decode(&row.key_pepper_id).ok()?).ok()?;
    Some(RoomKeyDigest {
        salt,
        mac,
        pepper_id,
    })
}

fn room_key_matches(pepper: &[u8; 32], room: &str, provided: &str, digest: &RoomKeyDigest) -> bool {
    rns_crypto::hmac::hmac_verify(
        pepper,
        &room_key_preimage(digest.pepper_id, room, provided, &digest.salt),
        &digest.mac,
    )
}

/// Per-link token bucket, reference shape: capacity = messages/minute,
/// refill capacity/60 per second, one token per inbound packet of any type.
struct RateBucket {
    tokens: f64,
    refilled_at: Instant,
}

impl RateBucket {
    fn new(now: Instant, per_minute: usize) -> Self {
        Self {
            tokens: per_minute as f64,
            refilled_at: now,
        }
    }

    fn allow(&mut self, now: Instant, per_minute: usize) -> bool {
        if per_minute == 0 {
            return true;
        }
        let capacity = per_minute as f64;
        let elapsed = now.duration_since(self.refilled_at).as_secs_f64();
        self.tokens = (self.tokens + elapsed * capacity / 60.0).min(capacity);
        self.refilled_at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// One authenticated client link. Sessions exist from link establishment and
/// become useful only after LINKIDENTIFY (packets from unidentified links are
/// dropped, reference behavior) and HELLO/WELCOME.
struct HubSession {
    identity: Option<[u8; 16]>,
    welcomed: bool,
    nickname: Option<String>,
    capabilities: BTreeMap<u64, bool>,
    rooms: HashSet<String>,
    established_at: Instant,
    awaiting_pong_since: Option<Instant>,
    rate: RateBucket,
    seen_ids: std::collections::VecDeque<[u8; 8]>,
    seen_set: HashSet<[u8; 8]>,
}

impl HubSession {
    fn new(now: Instant, per_minute: usize) -> Self {
        Self {
            identity: None,
            welcomed: false,
            nickname: None,
            capabilities: BTreeMap::new(),
            rooms: HashSet::new(),
            established_at: now,
            awaiting_pong_since: None,
            rate: RateBucket::new(now, per_minute),
            seen_ids: std::collections::VecDeque::new(),
            seen_set: HashSet::new(),
        }
    }

    fn note_seen(&mut self, id: [u8; 8]) -> bool {
        if self.seen_set.contains(&id) {
            return false;
        }
        self.seen_ids.push_back(id);
        self.seen_set.insert(id);
        if self.seen_ids.len() > SEEN_ID_LIMIT
            && let Some(evicted) = self.seen_ids.pop_front()
        {
            self.seen_set.remove(&evicted);
        }
        true
    }
}

/// Live room state. Everything here dies with the hub session except, later,
/// registry-backed rooms restored at startup.
#[derive(Default)]
struct HubRoom {
    topic: Option<String>,
    key: Option<RoomKeyDigest>,
    ops: HashSet<[u8; 16]>,
    voiced: HashSet<[u8; 16]>,
    bans: HashSet<[u8; 16]>,
    invited: HashMap<[u8; 16], f64>,
    moderated: bool,
    invite_only: bool,
    topic_ops_only: bool,
    no_outside_msgs: bool,
    private: bool,
    registered: bool,
    /// Wall-clock last activity, used only by the registry prune.
    last_used: f64,
    last_used_dirty: bool,
    /// Set when a write for this room failed and must be retried.
    persist_dirty: bool,
    /// Link ids, not identities: two links from one identity both relay.
    members: HashSet<[u8; 16]>,
}

impl HubRoom {
    /// Fixed flag order `i k m n p r t`, `(none)` when clear — NomadNet and
    /// our client render this string, so it is wire format.
    fn mode_string(&self) -> String {
        let mut flags = String::new();
        if self.invite_only {
            flags.push('i');
        }
        if self.key.is_some() {
            flags.push('k');
        }
        if self.moderated {
            flags.push('m');
        }
        if self.no_outside_msgs {
            flags.push('n');
        }
        if self.private {
            flags.push('p');
        }
        if self.registered {
            flags.push('r');
        }
        if self.topic_ops_only {
            flags.push('t');
        }
        if flags.is_empty() {
            "(none)".to_string()
        } else {
            format!("+{flags}")
        }
    }
}

/// Where the lead text of a multi-packet notice goes. Repeating it matters
/// for replies whose clients parse each packet independently.
#[derive(Clone, Copy)]
enum NoticeHeader<'a> {
    None,
    First(&'a str),
    Every(&'a str),
}

/// Hub-side sends the pure core asks the shell to perform.
#[derive(Debug)]
pub(crate) enum HubSend {
    Envelope {
        link_id: [u8; 16],
        envelope: rrc::Envelope,
    },
    Close {
        link_id: [u8; 16],
    },
    /// A durable-write intent. Keeping persistence as an intent lets `HubCore`
    /// stay synchronous and database-free.
    Persist(HubPersist),
}

#[derive(Debug)]
pub(crate) struct HubPersist {
    pub op: db::HubRoomOp,
    /// Link that caused the write; a failure notice goes here and nowhere else.
    pub origin: Option<[u8; 16]>,
    /// Room scope for that notice, so it lands in the right transcript.
    pub room: Option<String>,
}

/// Transport-free hub protocol core: every inbound event mutates state and
/// appends outbound work, so behavior is unit-testable exactly like the
/// reference router.
pub(crate) struct HubCore {
    config: ChannelHubConfig,
    hub_hash: [u8; 16],
    sessions: HashMap<[u8; 16], HubSession>,
    rooms: HashMap<String, HubRoom>,
    /// Last-wins identity→link index for direct notices and command targets.
    by_identity: HashMap<[u8; 16], [u8; 16]>,
    /// Server operators are implicit ops in every room.
    server_ops: HashSet<[u8; 16]>,
    klines: Arc<RwLock<HashSet<[u8; 16]>>>,
    /// Verify-only material for room join keys; never persisted.
    pepper: Zeroizing<[u8; 32]>,
    pepper_id: [u8; 8],
    started_wall: f64,
    klines_dirty: bool,
    pending_removals: HashSet<String>,
    pepper_rotation_notice: Option<String>,
    stats: HubStats,
    started_at: Instant,
}

impl HubCore {
    pub(crate) fn new(
        config: ChannelHubConfig,
        hub_hash: [u8; 16],
        klines: Arc<RwLock<HashSet<[u8; 16]>>>,
        pepper: Zeroizing<[u8; 32]>,
        restored: Vec<db::HubRoomRow>,
    ) -> Self {
        if let Ok(mut set) = klines.write() {
            set.extend(config.banned_identities.iter().copied());
        }
        let server_ops = config.server_operators.iter().copied().collect();
        let mut pepper_id = [0u8; 8];
        pepper_id.copy_from_slice(&hub_hash[..8]);
        let mut core = Self {
            config,
            hub_hash,
            sessions: HashMap::new(),
            rooms: HashMap::new(),
            by_identity: HashMap::new(),
            server_ops,
            klines,
            pepper,
            pepper_id,
            started_wall: now_unix(),
            klines_dirty: false,
            pending_removals: HashSet::new(),
            pepper_rotation_notice: None,
            stats: HubStats::default(),
            started_at: Instant::now(),
        };
        core.restore(restored);
        core
    }

    /// Rebuild live rooms from the registry. Rows are re-validated rather than
    /// trusted: a row that no longer normalizes is dropped from memory but
    /// left on disk, since silently deleting an operator's room over a config
    /// change would be destructive.
    fn restore(&mut self, restored: Vec<db::HubRoomRow>) {
        let now = now_unix();
        let mut key_rotated = 0usize;
        for row in restored {
            let Ok(room_name) = self.norm_room(&row.room_name) else {
                tracing::warn!(reason = "invalid_room_name", "hub registry row skipped");
                continue;
            };
            if room_name != row.room_name {
                tracing::warn!(
                    reason = "unnormalized_room_name",
                    "hub registry row skipped"
                );
                continue;
            }
            let key = decode_room_key(&row);
            if row.key_mac.is_empty() != key.is_none() {
                // Present but unusable: a different hub identity wrote it.
                key_rotated += 1;
            }
            let key = key.filter(|digest| digest.pepper_id == self.pepper_id);
            if !row.key_mac.is_empty() && key.is_none() {
                key_rotated += 1;
            }
            let mut room = HubRoom {
                registered: true,
                topic: (!row.topic.is_empty()).then(|| row.topic.clone()),
                key,
                moderated: row.moderated,
                invite_only: row.invite_only,
                topic_ops_only: row.topic_ops_only,
                no_outside_msgs: row.no_outside_msgs,
                private: row.private,
                last_used: row.last_used,
                ..HubRoom::default()
            };
            for (kind, subject, expires_at) in &row.grants {
                let Some(identity) = hex::decode(subject)
                    .ok()
                    .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
                else {
                    continue;
                };
                match kind.as_str() {
                    "op" => {
                        room.ops.insert(identity);
                    }
                    "voice" => {
                        room.voiced.insert(identity);
                    }
                    "ban" => {
                        room.bans.insert(identity);
                    }
                    "invite" if *expires_at > now => {
                        room.invited.insert(identity, *expires_at);
                    }
                    _ => {}
                }
            }
            self.rooms.insert(room_name, room);
        }
        if key_rotated > 0 {
            // The operator has to be told: +k rooms are now unjoinable for
            // anyone without op status, and a log line is not visible in-app.
            tracing::warn!(
                rooms = key_rotated,
                reason = "hub_key_rotated",
                "room keys cleared after a hub identity change"
            );
            self.pepper_rotation_notice = Some(format!(
                "{key_rotated} room key(s) were cleared because the hub identity changed; set them again with /mode <room> +k"
            ));
        }
    }

    fn session_counts(&self) -> (usize, usize) {
        let welcomed = self
            .sessions
            .values()
            .filter(|session| session.welcomed)
            .count();
        (self.sessions.len(), welcomed)
    }

    fn room_count(&self) -> usize {
        self.rooms.len()
    }

    pub(crate) fn registered_room_count(&self) -> usize {
        self.rooms.values().filter(|room| room.registered).count()
    }

    /// True while any durable write is outstanding, so the operator can be
    /// told the registry is behind rather than silently losing policy.
    pub(crate) fn registry_degraded(&self) -> bool {
        self.klines_dirty
            || !self.pending_removals.is_empty()
            || self.rooms.values().any(|room| room.persist_dirty)
    }

    /// Project a registered room to its durable form and queue the write.
    /// Unregistered rooms are deliberately ephemeral, matching the reference.
    fn persist_room(&mut self, room_name: &str, origin: Option<[u8; 16]>, out: &mut Vec<HubSend>) {
        let now = now_unix();
        let Some(room) = self.rooms.get_mut(room_name) else {
            return;
        };
        if !room.registered {
            return;
        }
        room.invited.retain(|_, expires| *expires > now);
        room.last_used_dirty = false;
        room.persist_dirty = false;
        let key = room.key.clone();
        let mut grants: Vec<(String, String, f64)> = Vec::new();
        for (kind, subjects) in [
            ("op", &room.ops),
            ("voice", &room.voiced),
            ("ban", &room.bans),
        ] {
            grants.extend(
                subjects
                    .iter()
                    .map(|subject| (kind.to_string(), hex::encode(subject), 0.0)),
            );
        }
        grants.extend(
            room.invited
                .iter()
                .map(|(subject, expires)| ("invite".to_string(), hex::encode(subject), *expires)),
        );
        let row = db::HubRoomRow {
            room_name: room_name.to_string(),
            topic: room.topic.clone().unwrap_or_default(),
            key_salt: key
                .as_ref()
                .map(|key| hex::encode(key.salt))
                .unwrap_or_default(),
            key_mac: key
                .as_ref()
                .map(|key| hex::encode(key.mac))
                .unwrap_or_default(),
            key_pepper_id: key
                .as_ref()
                .map(|key| hex::encode(key.pepper_id))
                .unwrap_or_default(),
            moderated: room.moderated,
            invite_only: room.invite_only,
            topic_ops_only: room.topic_ops_only,
            no_outside_msgs: room.no_outside_msgs,
            private: room.private,
            last_used: room.last_used,
            grants,
        };
        out.push(HubSend::Persist(HubPersist {
            op: db::HubRoomOp::Upsert(Box::new(row)),
            origin,
            room: Some(room_name.to_string()),
        }));
    }

    fn persist_klines(&mut self, origin: Option<[u8; 16]>, out: &mut Vec<HubSend>) {
        self.klines_dirty = false;
        let subjects = self
            .klines
            .read()
            .map(|klines| klines.iter().map(hex::encode).collect::<Vec<_>>())
            .unwrap_or_default();
        out.push(HubSend::Persist(HubPersist {
            op: db::HubRoomOp::ReplaceKlines(subjects),
            origin,
            room: None,
        }));
    }

    /// Mark room activity for the prune clock. Deliberately does not write:
    /// a durable round trip per relayed message would be absurd.
    fn touch_room(&mut self, room_name: &str) {
        if let Some(room) = self.rooms.get_mut(room_name)
            && room.registered
        {
            room.last_used = now_unix();
            room.last_used_dirty = true;
        }
    }

    pub(crate) fn flush_dirty_last_used(&mut self, out: &mut Vec<HubSend>) {
        let dirty: Vec<(String, f64)> = self
            .rooms
            .iter_mut()
            .filter(|(_, room)| room.registered && room.last_used_dirty)
            .map(|(name, room)| {
                room.last_used_dirty = false;
                (name.clone(), room.last_used)
            })
            .collect();
        for (room_name, last_used) in dirty {
            out.push(HubSend::Persist(HubPersist {
                op: db::HubRoomOp::Touched {
                    room_name,
                    last_used,
                },
                origin: None,
                room: None,
            }));
        }
    }

    /// Drop registered rooms nobody has used in a long time. Unlike the
    /// reference this runs whether or not a session is connected.
    pub(crate) fn prune_registry(&mut self, now: f64, out: &mut Vec<HubSend>) {
        if self.config.room_registry_prune_after_secs == 0 {
            return;
        }
        // A device with no RTC reports an epoch near zero; pruning on that
        // clock would delete the operator's rooms.
        if now < 1_700_000_000.0 {
            return;
        }
        // Give the clock a chance to settle before the first pass. An
        // interval of 0 means "no startup delay" and is used by tests.
        if self.config.room_registry_prune_interval_secs > 0
            && self.started_at.elapsed()
                < Duration::from_secs(self.config.room_registry_prune_interval_secs)
        {
            return;
        }
        let cutoff = self.config.room_registry_prune_after_secs as f64;
        let mut stale: Vec<String> = Vec::new();
        for (name, room) in self.rooms.iter_mut() {
            if !room.registered || !room.members.is_empty() {
                continue;
            }
            let last_used = if room.last_used <= 0.0 {
                self.started_wall
            } else {
                room.last_used
            };
            // A clock that jumped backwards must not make a room look ancient.
            if last_used > now {
                room.last_used = now;
                room.last_used_dirty = true;
                continue;
            }
            if now - last_used > cutoff {
                stale.push(name.clone());
            }
        }
        for room_name in stale {
            self.rooms.remove(&room_name);
            out.push(HubSend::Persist(HubPersist {
                op: db::HubRoomOp::Removed {
                    room_name: room_name.clone(),
                },
                origin: None,
                room: None,
            }));
        }
        out.push(HubSend::Persist(HubPersist {
            op: db::HubRoomOp::GcInvites { before: now },
            origin: None,
            room: None,
        }));
    }

    /// Record a failed write so the next tick re-emits it from live state.
    /// Re-projecting rather than replaying a stale snapshot keeps the retry
    /// set bounded and always current.
    pub(crate) fn note_persist_failed(&mut self, op: &db::HubRoomOp) {
        match op {
            db::HubRoomOp::Upsert(room) => {
                if let Some(live) = self.rooms.get_mut(&room.room_name) {
                    live.persist_dirty = true;
                }
            }
            db::HubRoomOp::Touched { room_name, .. } => {
                if let Some(live) = self.rooms.get_mut(room_name) {
                    live.last_used_dirty = true;
                }
            }
            db::HubRoomOp::Removed { room_name } => {
                self.pending_removals.insert(room_name.clone());
            }
            db::HubRoomOp::ReplaceKlines(_) => self.klines_dirty = true,
            db::HubRoomOp::GcInvites { .. } => {}
        }
    }

    pub(crate) fn retry_failed_persists(&mut self, out: &mut Vec<HubSend>) {
        let removals: Vec<String> = self.pending_removals.drain().collect();
        for room_name in removals {
            out.push(HubSend::Persist(HubPersist {
                op: db::HubRoomOp::Removed { room_name },
                origin: None,
                room: None,
            }));
        }
        let dirty: Vec<String> = self
            .rooms
            .iter()
            .filter(|(_, room)| room.registered && room.persist_dirty)
            .map(|(name, _)| name.clone())
            .collect();
        for room_name in dirty {
            self.persist_room(&room_name, None, out);
        }
        if self.klines_dirty {
            self.persist_klines(None, out);
        }
    }

    pub(crate) fn note_rate_limited(&mut self) {
        self.stats.rate_limited += 1;
    }

    pub(crate) fn note_bad_packet(&mut self) {
        self.stats.bad_packets += 1;
    }

    pub(crate) fn note_oversize(&mut self, count: usize) {
        self.stats.oversize += count as u64;
    }

    /// Reference room normalization with its exact reply texts.
    fn norm_room(&self, room: &str) -> Result<String, String> {
        let normalized = room.trim().to_lowercase();
        if normalized.is_empty() {
            return Err("room name must not be empty".to_string());
        }
        let bytes = normalized.len();
        if bytes > self.config.max_room_name_bytes {
            return Err(format!(
                "room name too long: {bytes} bytes > {} bytes",
                self.config.max_room_name_bytes
            ));
        }
        Ok(normalized)
    }

    fn is_room_op(&self, room: &HubRoom, identity: [u8; 16]) -> bool {
        self.server_ops.contains(&identity) || room.ops.contains(&identity)
    }

    fn is_voiced(&self, room: &HubRoom, identity: [u8; 16]) -> bool {
        self.is_room_op(room, identity) || room.voiced.contains(&identity)
    }

    fn is_invited(room: &HubRoom, identity: [u8; 16], now_unix: f64) -> bool {
        room.invited
            .get(&identity)
            .is_some_and(|expires| *expires > now_unix)
    }

    /// Same identity still present in `room` through a different link.
    fn identity_still_in_room(
        &self,
        room: &HubRoom,
        identity: [u8; 16],
        excluding: [u8; 16],
    ) -> bool {
        room.members.iter().any(|member| {
            *member != excluding
                && self
                    .sessions
                    .get(member)
                    .is_some_and(|session| session.identity == Some(identity))
        })
    }

    fn roster_identities(&self, room: &HubRoom) -> Vec<[u8; 16]> {
        room.members
            .iter()
            .filter_map(|member| {
                self.sessions
                    .get(member)
                    .and_then(|session| session.identity)
            })
            .collect()
    }

    fn welcome_info(&self) -> rrc::WelcomeInfo {
        let mut capabilities = BTreeMap::new();
        capabilities.insert(rrc::CAP_ACTION, true);
        capabilities.insert(rrc::CAP_DIRECT_NOTICE, true);
        if self.config.enable_resource_transfer {
            capabilities.insert(rrc::CAP_RESOURCE_ENVELOPE, true);
        }
        rrc::WelcomeInfo {
            hub_name: Some(self.config.hub_name.clone()),
            hub_version: Some(HUB_VERSION.to_string()),
            capabilities,
            limits: rrc::HubLimits {
                max_nick_bytes: Some(self.config.max_nick_bytes),
                max_room_name_bytes: Some(self.config.max_room_name_bytes),
                max_message_body_bytes: Some(self.config.max_message_body_bytes),
                max_rooms_per_session: Some(self.config.max_rooms_per_session),
                rate_messages_per_minute: Some(self.config.rate_messages_per_minute),
            },
        }
    }

    /// Encoded size of an envelope of this shape carrying an empty text body.
    /// Measured rather than computed: CBOR header widths shift with room and
    /// nickname length, and a stale constant silently drops packets.
    fn envelope_overhead(&self, message_type: rrc::MessageType, room: Option<&str>) -> usize {
        let mut probe = rrc::Envelope::new(message_type, self.hub_hash);
        probe.room = room.map(str::to_string);
        // An empty *body* rather than no body: without the key the K_BODY byte
        // is unaccounted and a 432-byte packet slips through.
        probe.body = Some(Value::Text(String::new()));
        rrc::encode(&probe).map(|bytes| bytes.len()).unwrap_or(0)
    }

    /// Text bytes that fit in one packet of this shape.
    fn text_body_budget(&self, message_type: rrc::MessageType, room: Option<&str>) -> usize {
        LINK_PACKET_BUDGET
            .saturating_sub(self.envelope_overhead(message_type, room))
            .saturating_sub(CBOR_TEXT_HEADER_SLACK)
    }

    /// Identity hashes that fit in one JOINED roster packet for this room.
    fn roster_chunk_len(&self, room_name: &str) -> usize {
        let overhead = self.envelope_overhead(rrc::MessageType::Joined, Some(room_name));
        // 16 bytes plus a one-byte CBOR header each, minus a byte for the
        // array header widening past 23 entries.
        LINK_PACKET_BUDGET
            .saturating_sub(overhead)
            .saturating_sub(1)
            .checked_div(17)
            .unwrap_or(1)
            .max(1)
    }

    /// Emit a notice that may not fit in one packet. Never drops: it packs on
    /// entry boundaries so a client's per-entry parser cannot see a mangled
    /// half-entry, and falls back to UTF-8 splitting only for a single entry
    /// too long to stand alone.
    fn push_notice_entries(
        &self,
        link_id: [u8; 16],
        header: NoticeHeader<'_>,
        entries: &[String],
        separator: &str,
        room: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        let lead = match header {
            NoticeHeader::None => "",
            NoticeHeader::First(lead) | NoticeHeader::Every(lead) => lead,
        };
        let budget = self
            .text_body_budget(rrc::MessageType::Notice, room)
            .max(MIN_NOTICE_BODY_BYTES);
        let whole = format!("{lead}{}", entries.join(separator));
        if whole.len() <= budget {
            out.push(self.hub_notice(link_id, &whole, room));
            return;
        }

        let repeat_lead = matches!(header, NoticeHeader::Every(_));
        let mut current = lead.to_string();
        let mut current_has_entry = false;
        let mut flush = |text: &mut String, has_entry: &mut bool, out: &mut Vec<HubSend>| {
            if *has_entry {
                out.push(self.hub_notice(link_id, text, room));
            }
            text.clear();
            *has_entry = false;
        };
        for entry in entries {
            let addition = if current_has_entry {
                format!("{separator}{entry}")
            } else {
                entry.clone()
            };
            if current.len() + addition.len() <= budget {
                current.push_str(&addition);
                current_has_entry = true;
                continue;
            }
            flush(&mut current, &mut current_has_entry, out);
            current = if repeat_lead {
                lead.to_string()
            } else {
                String::new()
            };
            if current.len() + entry.len() <= budget {
                current.push_str(entry);
                current_has_entry = true;
            } else {
                // A single entry too large to stand alone: split it rather
                // than drop it, on UTF-8 boundaries.
                for piece in chunk_text(entry, budget.saturating_sub(current.len()).max(1)) {
                    out.push(self.hub_notice(link_id, &format!("{current}{piece}"), room));
                    current = if repeat_lead {
                        lead.to_string()
                    } else {
                        String::new()
                    };
                }
                current_has_entry = false;
            }
        }
        flush(&mut current, &mut current_has_entry, out);
    }

    fn hub_notice(&self, link_id: [u8; 16], text: &str, room: Option<&str>) -> HubSend {
        let mut envelope = rrc::Envelope::new(rrc::MessageType::Notice, self.hub_hash);
        envelope.room = room.map(str::to_string);
        envelope.body = Some(Value::Text(text.to_string()));
        HubSend::Envelope { link_id, envelope }
    }

    fn hub_error(&self, link_id: [u8; 16], text: &str, room: Option<&str>) -> HubSend {
        let mut envelope = rrc::Envelope::new(rrc::MessageType::Error, self.hub_hash);
        envelope.room = room.map(str::to_string);
        envelope.body = Some(Value::Text(text.to_string()));
        HubSend::Envelope { link_id, envelope }
    }

    pub(crate) fn on_link_established(&mut self, link_id: [u8; 16], now: Instant) {
        let per_minute = self.config.rate_messages_per_minute;
        self.sessions
            .entry(link_id)
            .or_insert_with(|| HubSession::new(now, per_minute));
    }

    /// Identification can race establishment through separate event streams;
    /// create-on-first-sight mirrors the rnsh listener ordering fix.
    pub(crate) fn on_link_identified(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        now: Instant,
        out: &mut Vec<HubSend>,
    ) {
        let banned = self
            .klines
            .read()
            .map(|klines| klines.contains(&identity))
            .unwrap_or(false);
        if banned {
            out.push(self.hub_error(link_id, "banned", None));
            out.push(HubSend::Close { link_id });
            self.sessions.remove(&link_id);
            return;
        }
        let per_minute = self.config.rate_messages_per_minute;
        let session = self
            .sessions
            .entry(link_id)
            .or_insert_with(|| HubSession::new(now, per_minute));
        session.identity = Some(identity);
        self.by_identity.insert(identity, link_id);
    }

    /// Reference-exact rate accounting: one token per inbound packet of any
    /// type, charged before decode. Returns false when the packet must be
    /// dropped with a "rate limited" ERROR.
    pub(crate) fn note_packet(&mut self, link_id: [u8; 16], now: Instant) -> bool {
        let per_minute = self.config.rate_messages_per_minute;
        let Some(session) = self.sessions.get_mut(&link_id) else {
            return true;
        };
        if session.identity.is_none() {
            // Unidentified links are dropped before rate accounting.
            return true;
        }
        session.rate.allow(now, per_minute)
    }

    pub(crate) fn on_link_closed(&mut self, link_id: [u8; 16], out: &mut Vec<HubSend>) {
        let Some(session) = self.sessions.remove(&link_id) else {
            return;
        };
        if let Some(identity) = session.identity {
            if self.by_identity.get(&identity) == Some(&link_id) {
                self.by_identity.remove(&identity);
            }
            let rooms: Vec<String> = session.rooms.iter().cloned().collect();
            for room_name in rooms {
                self.remove_member_with_parted(
                    &room_name,
                    link_id,
                    identity,
                    session.nickname.as_deref(),
                    out,
                );
            }
        }
    }

    /// Remove one link from a room, fanning PARTED to the remaining members
    /// unless the same identity is still present through another link.
    fn remove_member_with_parted(
        &mut self,
        room_name: &str,
        link_id: [u8; 16],
        identity: [u8; 16],
        nickname: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        let Some(room) = self.rooms.get_mut(room_name) else {
            return;
        };
        room.members.remove(&link_id);
        let remaining: Vec<[u8; 16]> = room.members.iter().copied().collect();
        let registered = room.registered;
        if remaining.is_empty() && !registered {
            self.rooms.remove(room_name);
        }
        let still_present = self
            .rooms
            .get(room_name)
            .is_some_and(|room| self.identity_still_in_room(room, identity, link_id));
        if still_present {
            return;
        }
        let body = self
            .config
            .include_member_list
            .then(|| rrc::member_list(&[identity]));
        for member in remaining {
            let mut parted = rrc::Envelope::new(rrc::MessageType::Parted, self.hub_hash);
            parted.room = Some(room_name.to_string());
            parted.body = body.clone();
            parted.nickname = nickname.map(str::to_string);
            out.push(HubSend::Envelope {
                link_id: member,
                envelope: parted,
            });
        }
    }

    pub(crate) fn on_envelope(
        &mut self,
        link_id: [u8; 16],
        envelope: rrc::Envelope,
        out: &mut Vec<HubSend>,
    ) {
        let Some(session) = self.sessions.get_mut(&link_id) else {
            return;
        };
        // Packets from unidentified links carry no authenticated source and
        // are dropped without a reply (reference behavior).
        if session.identity.is_none() {
            return;
        }

        // Replayed envelope ids never fan out twice (deliberate deviation:
        // the reference writes ids but never reads them).
        if !session.note_seen(envelope.message_id) {
            self.stats.duplicates += 1;
            return;
        }

        match envelope.message_type {
            rrc::MessageType::Ping => {
                let pong = rrc::Envelope::pong(self.hub_hash, &envelope);
                out.push(HubSend::Envelope {
                    link_id,
                    envelope: pong,
                });
            }
            rrc::MessageType::Pong => {
                session.awaiting_pong_since = None;
                self.stats.pongs_in += 1;
            }
            rrc::MessageType::Hello => self.on_hello(link_id, envelope, out),
            _ if !self.sessions.get(&link_id).is_some_and(|s| s.welcomed) => {
                out.push(self.hub_error(link_id, "send HELLO first", None));
            }
            rrc::MessageType::Join => {
                self.stats.joins += 1;
                self.on_join(link_id, envelope, Instant::now(), out);
            }
            rrc::MessageType::Part => {
                self.stats.parts += 1;
                self.on_part(link_id, envelope, out);
            }
            rrc::MessageType::Message | rrc::MessageType::Notice | rrc::MessageType::Action => {
                self.on_relay(link_id, envelope, out)
            }
            // Resource envelopes land in a later phase; unknown types stay
            // silently forward-compatible.
            _ => {}
        }
    }

    fn on_join(
        &mut self,
        link_id: [u8; 16],
        envelope: rrc::Envelope,
        now: Instant,
        out: &mut Vec<HubSend>,
    ) {
        let Some(identity) = self.session_identity(link_id) else {
            return;
        };
        let Some(room_raw) = envelope.room.as_deref().filter(|room| !room.is_empty()) else {
            out.push(self.hub_error(link_id, "JOIN requires room name", None));
            return;
        };
        let already_joined_count = self
            .sessions
            .get(&link_id)
            .map(|session| session.rooms.len())
            .unwrap_or(0);
        let room_name = match self.norm_room(room_raw) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_error(link_id, &reason, None));
                return;
            }
        };
        let already_member = self
            .sessions
            .get(&link_id)
            .is_some_and(|session| session.rooms.contains(&room_name));
        // Deviation: a re-join of a room this session already occupies is not
        // refused at the limit boundary (the reference counts it and errors).
        if !already_member && already_joined_count >= self.config.max_rooms_per_session {
            out.push(self.hub_error(link_id, "too many rooms", None));
            return;
        }

        // Only the hub operator brings a room into existence. Letting any
        // joiner found one is an unbounded growth vector and would put remote
        // peers in the operator's registry.
        if !self.rooms.contains_key(&room_name) {
            if !self.server_ops.contains(&identity) {
                out.push(self.hub_error(link_id, "no such room", Some(&room_name)));
                return;
            }
            let room = self.rooms.entry(room_name.clone()).or_default();
            room.ops.insert(identity);
        }

        let room = self.rooms.get(&room_name).expect("room exists");
        if room.invite_only {
            let bypass = self.server_ops.contains(&identity)
                || room.ops.contains(&identity)
                || Self::is_invited(room, identity, now_unix());
            if !bypass {
                out.push(self.hub_error(link_id, "invite-only (+i)", Some(&room_name)));
                return;
            }
        }
        if let Some(digest) = self.rooms.get(&room_name).and_then(|room| room.key.clone()) {
            let room = self.rooms.get(&room_name).expect("room exists");
            let bypass = self.server_ops.contains(&identity)
                || room.ops.contains(&identity)
                || Self::is_invited(room, identity, now_unix());
            let provided = envelope.body.as_ref().and_then(|body| match body {
                Value::Text(text) => Some(text.as_str()),
                _ => None,
            });
            let matches = provided.is_some_and(|provided| {
                room_key_matches(&self.pepper, &room_name, provided, &digest)
            });
            if !bypass && !matches {
                out.push(self.hub_error(link_id, "bad key (+k)", Some(&room_name)));
                return;
            }
        }
        if self
            .rooms
            .get(&room_name)
            .is_some_and(|room| room.bans.contains(&identity))
        {
            out.push(self.hub_error(link_id, "banned from room", Some(&room_name)));
            return;
        }

        let room = self.rooms.get_mut(&room_name).expect("room exists");
        let existing_members: Vec<[u8; 16]> = room
            .members
            .iter()
            .copied()
            .filter(|member| *member != link_id)
            .collect();
        room.members.insert(link_id);
        room.invited.remove(&identity);
        drop(room);
        self.touch_room(&room_name);
        let room = self.rooms.get_mut(&room_name).expect("room exists");
        if let Some(session) = self.sessions.get_mut(&link_id) {
            session.rooms.insert(room_name.clone());
        }

        let joiner_nick = self
            .sessions
            .get(&link_id)
            .and_then(|session| session.nickname.clone());
        let fanout_body = self
            .config
            .include_member_list
            .then(|| rrc::member_list(&[identity]));
        for member in existing_members {
            let mut joined = rrc::Envelope::new(rrc::MessageType::Joined, self.hub_hash);
            joined.room = Some(room_name.clone());
            joined.body = fanout_body.clone();
            joined.nickname = joiner_nick.clone();
            out.push(HubSend::Envelope {
                link_id: member,
                envelope: joined,
            });
        }

        if self.config.include_member_list {
            let roster = {
                let room = self.rooms.get(&room_name).expect("room exists");
                self.roster_identities(room)
            };
            self.push_roster_joined(link_id, &room_name, identity, &roster, out);
        } else {
            let mut joined = rrc::Envelope::new(rrc::MessageType::Joined, self.hub_hash);
            joined.room = Some(room_name.clone());
            out.push(HubSend::Envelope {
                link_id,
                envelope: joined,
            });
        }

        out.push(self.room_status_notice(link_id, &room_name));
    }

    /// Send the joiner its roster, split across packets when a large room
    /// would not fit. Truncating instead would report a partial roster as
    /// complete; splitting ends with a complete roster and only the
    /// already-documented "not authoritative" state on the way there.
    fn push_roster_joined(
        &self,
        link_id: [u8; 16],
        room_name: &str,
        joiner: [u8; 16],
        roster: &[[u8; 16]],
        out: &mut Vec<HubSend>,
    ) {
        let per_chunk = self.roster_chunk_len(room_name);
        // The joiner leads chunk 0 so the client can recognise its own join
        // before any continuation arrives.
        let mut ordered: Vec<[u8; 16]> = Vec::with_capacity(roster.len());
        ordered.push(joiner);
        ordered.extend(roster.iter().copied().filter(|member| *member != joiner));

        let mut chunks: Vec<Vec<[u8; 16]>> = ordered
            .chunks(per_chunk)
            .map(<[[u8; 16]]>::to_vec)
            .collect();
        // A lone trailing member reads as a join event, not a roster fragment.
        if chunks.len() > 1 && chunks.last().is_some_and(|last| last.len() == 1) {
            let previous = chunks.len() - 2;
            if let Some(borrowed) = chunks[previous].pop() {
                chunks[previous + 1].insert(0, borrowed);
            }
        }
        for chunk in chunks {
            let mut joined = rrc::Envelope::new(rrc::MessageType::Joined, self.hub_hash);
            joined.room = Some(room_name.to_string());
            joined.body = Some(rrc::member_list(&chunk));
            out.push(HubSend::Envelope {
                link_id,
                envelope: joined,
            });
        }
    }

    /// The join-confirmation NOTICE both NomadNet and our client parse by
    /// exact text — wire format, not prose.
    fn room_status_notice(&self, link_id: [u8; 16], room_name: &str) -> HubSend {
        let room = self.rooms.get(room_name);
        let registered = room.is_some_and(|room| room.registered);
        let mode = room
            .map(|room| room.mode_string())
            .unwrap_or_else(|| "(none)".to_string());
        let topic = room
            .and_then(|room| room.topic.clone())
            .unwrap_or_else(|| "(none)".to_string());
        let reg_txt = if registered {
            "registered"
        } else {
            "unregistered"
        };
        self.hub_notice(
            link_id,
            &format!("room {room_name}: {reg_txt}; mode={mode}; topic={topic}"),
            Some(room_name),
        )
    }

    fn on_part(&mut self, link_id: [u8; 16], envelope: rrc::Envelope, out: &mut Vec<HubSend>) {
        let Some(identity) = self.session_identity(link_id) else {
            return;
        };
        let Some(room_raw) = envelope.room.as_deref().filter(|room| !room.is_empty()) else {
            out.push(self.hub_error(link_id, "PART requires room name", None));
            return;
        };
        let room_name = match self.norm_room(room_raw) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_error(link_id, &reason, None));
                return;
            }
        };
        let nickname = self
            .sessions
            .get(&link_id)
            .and_then(|session| session.nickname.clone());
        if let Some(session) = self.sessions.get_mut(&link_id) {
            session.rooms.remove(&room_name);
        }
        self.remove_member_with_parted(&room_name, link_id, identity, nickname.as_deref(), out);

        // The actor always receives PARTED, membership or not (reference).
        let mut parted = rrc::Envelope::new(rrc::MessageType::Parted, self.hub_hash);
        parted.room = Some(room_name);
        parted.body = self
            .config
            .include_member_list
            .then(|| rrc::member_list(&[identity]));
        out.push(HubSend::Envelope {
            link_id,
            envelope: parted,
        });
    }

    fn session_identity(&self, link_id: [u8; 16]) -> Option<[u8; 16]> {
        self.sessions
            .get(&link_id)
            .and_then(|session| session.identity)
    }

    /// MSG/NOTICE/ACTION relay: slash interception first, then the reference
    /// gate order, then in-place source/room/nick rewrite and member fan-out
    /// (sender included, envelope id and timestamp preserved).
    fn on_relay(&mut self, link_id: [u8; 16], mut envelope: rrc::Envelope, out: &mut Vec<HubSend>) {
        let Some(identity) = self.session_identity(link_id) else {
            return;
        };
        let is_action = envelope.message_type == rrc::MessageType::Action;
        if !is_action
            && let Some(text) = rrc::text_body(&envelope)
            && text.trim_start().starts_with('/')
        {
            if !self.handle_slash_command(link_id, identity, &envelope, out) {
                let room = envelope.room.clone();
                out.push(self.hub_error(link_id, "unrecognized command", room.as_deref()));
            }
            return;
        }

        if matches!(
            envelope.message_type,
            rrc::MessageType::Message | rrc::MessageType::Action
        ) {
            if envelope.room.as_deref().is_none_or(str::is_empty) {
                out.push(self.hub_error(link_id, "message requires room name", None));
                return;
            }
            if let Some(text) = rrc::text_body(&envelope) {
                let body_bytes = text.len();
                if body_bytes > self.config.max_message_body_bytes {
                    out.push(self.hub_error(
                        link_id,
                        &format!(
                            "message too large: {body_bytes} bytes > {} bytes",
                            self.config.max_message_body_bytes
                        ),
                        None,
                    ));
                    return;
                }
            }
        } else {
            // NOTICE: direct when a destination is present; roomless room
            // notices are silently dropped (reference).
            if envelope.destination.is_some() {
                self.on_direct_notice(link_id, identity, envelope, out);
                return;
            }
            if envelope.room.as_deref().is_none_or(str::is_empty) {
                return;
            }
        }

        let room_name = match self.norm_room(envelope.room.as_deref().unwrap_or_default()) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_error(link_id, &reason, None));
                return;
            }
        };

        let is_member = self
            .sessions
            .get(&link_id)
            .is_some_and(|session| session.rooms.contains(&room_name));
        if !is_member {
            let Some(room) = self.rooms.get(&room_name) else {
                out.push(self.hub_error(link_id, "no such room", Some(&room_name)));
                return;
            };
            if room.no_outside_msgs {
                out.push(self.hub_error(link_id, "no outside messages (+n)", Some(&room_name)));
                return;
            }
        }
        if let Some(room) = self.rooms.get(&room_name) {
            if room.bans.contains(&identity) {
                out.push(self.hub_error(link_id, "banned from room", Some(&room_name)));
                return;
            }
            if room.moderated && !self.is_voiced(room, identity) {
                out.push(self.hub_error(link_id, "room is moderated (+m)", Some(&room_name)));
                return;
            }
        }

        envelope.source = identity;
        envelope.room = Some(room_name.clone());
        self.rewrite_relay_nickname(link_id, &mut envelope);

        // The rewritten envelope carries the room and the advisory nickname,
        // so a message inside the advertised body limit can still overflow a
        // link packet. Drop the nickname first: both reference clients cache
        // nicks by source, so the message survives and only the hint is lost.
        let mut encoded = rrc::encode(&envelope).map(|bytes| bytes.len()).unwrap_or(0);
        if encoded > LINK_PACKET_BUDGET && envelope.nickname.is_some() {
            envelope.nickname = None;
            encoded = rrc::encode(&envelope).map(|bytes| bytes.len()).unwrap_or(0);
        }
        if encoded > LINK_PACKET_BUDGET {
            out.push(self.hub_error(
                link_id,
                &format!(
                    "message too large for {room_name}: {encoded} bytes > {LINK_PACKET_BUDGET} bytes"
                ),
                Some(&room_name),
            ));
            return;
        }

        match envelope.message_type {
            rrc::MessageType::Message => self.stats.messages_forwarded += 1,
            rrc::MessageType::Action => self.stats.actions_forwarded += 1,
            _ => self.stats.notices_forwarded += 1,
        }

        let Some(room) = self.rooms.get(&room_name) else {
            return;
        };
        for member in room.members.iter().copied() {
            out.push(HubSend::Envelope {
                link_id: member,
                envelope: envelope.clone(),
            });
        }
    }

    /// Inbound nick updates the session when valid, is stripped when invalid,
    /// and the stored session nick rides along when absent (reference rules).
    fn rewrite_relay_nickname(&mut self, link_id: [u8; 16], envelope: &mut rrc::Envelope) {
        let max_bytes = self.config.max_nick_bytes;
        let Some(session) = self.sessions.get_mut(&link_id) else {
            return;
        };
        match envelope.nickname.as_deref() {
            Some(nickname) => match rrc::normalize_nickname(nickname, max_bytes) {
                Ok(normalized) => {
                    if session.nickname.as_deref() != Some(normalized.as_str()) {
                        session.nickname = Some(normalized.clone());
                    }
                    envelope.nickname = Some(normalized);
                }
                Err(_) => envelope.nickname = None,
            },
            None => {
                envelope.nickname = session
                    .nickname
                    .as_deref()
                    .and_then(|nickname| rrc::normalize_nickname(nickname, max_bytes).ok());
            }
        }
    }

    fn on_direct_notice(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        mut envelope: rrc::Envelope,
        out: &mut Vec<HubSend>,
    ) {
        if envelope.room.is_some() {
            out.push(self.hub_error(link_id, "direct notice must not include room", None));
            return;
        }
        let Some(destination) = envelope.destination else {
            out.push(self.hub_error(link_id, "direct notice requires destination identity", None));
            return;
        };
        let Some(target_link) = self.by_identity.get(&destination).copied() else {
            out.push(self.hub_error(link_id, "destination not connected", None));
            return;
        };
        envelope.source = identity;
        self.rewrite_relay_nickname(link_id, &mut envelope);
        let mut encoded = rrc::encode(&envelope).map(|bytes| bytes.len()).unwrap_or(0);
        if encoded > LINK_PACKET_BUDGET && envelope.nickname.is_some() {
            envelope.nickname = None;
            encoded = rrc::encode(&envelope).map(|bytes| bytes.len()).unwrap_or(0);
        }
        if encoded > LINK_PACKET_BUDGET {
            out.push(self.hub_error(
                link_id,
                &format!("message too large: {encoded} bytes > {LINK_PACKET_BUDGET} bytes"),
                None,
            ));
            return;
        }
        self.stats.direct_notices += 1;
        out.push(HubSend::Envelope {
            link_id: target_link,
            envelope,
        });
    }

    /// Operator command dispatch, reference-faithful in verbs, permissions,
    /// and reply texts. Returns false only for unrecognized verbs, which the
    /// relay path answers with the reference "unrecognized command" ERROR.
    fn handle_slash_command(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        envelope: &rrc::Envelope,
        out: &mut Vec<HubSend>,
    ) -> bool {
        let Some(text) = rrc::text_body(envelope) else {
            return false;
        };
        let raw_room = envelope.room.clone();
        let parts: Vec<String> = text.trim()[1..]
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let Some(verb) = parts.first().map(|verb| verb.to_lowercase()) else {
            return false;
        };
        let args = &parts[1..];
        let raw = raw_room.as_deref();
        match verb.as_str() {
            "list" => self.cmd_list(link_id, identity, out),
            "who" | "names" => self.cmd_who(link_id, identity, args, raw, out),
            "kick" => self.cmd_kick(link_id, identity, args, raw, out),
            "kline" => self.cmd_kline(link_id, identity, args, out),
            "register" => self.cmd_register(link_id, identity, args, raw, out),
            "unregister" => self.cmd_unregister(link_id, identity, args, raw, out),
            "topic" => self.cmd_topic(link_id, identity, args, raw, out),
            "mode" => self.cmd_mode(link_id, identity, args, raw, out),
            "op" | "deop" | "voice" | "devoice" => {
                self.cmd_grant(link_id, identity, &verb, args, raw, out)
            }
            "ban" => self.cmd_ban(link_id, identity, args, raw, out),
            "invite" => self.cmd_invite(link_id, identity, args, raw, out),
            "stats" => self.cmd_stats(link_id, identity, out),
            "reload" => self.cmd_reload(link_id, identity, out),
            _ => return false,
        }
        true
    }

    /// Resolve a command target the reference way: an all-hex token of at
    /// least six characters matches identity-hash prefixes, anything else is
    /// a case-insensitive nick lookup over identified sessions.
    fn resolve_target(&self, token: &str) -> Result<[u8; 16], String> {
        let lowered = token.to_lowercase();
        let is_hex = lowered.len() >= 6 && lowered.chars().all(|ch| ch.is_ascii_hexdigit());
        let mut matches: Vec<([u8; 16], Option<String>)> = Vec::new();
        for session in self.sessions.values() {
            let Some(identity) = session.identity else {
                continue;
            };
            let matched = if is_hex {
                hex::encode(identity).starts_with(&lowered)
            } else {
                session
                    .nickname
                    .as_deref()
                    .is_some_and(|nickname| nickname.eq_ignore_ascii_case(token))
            };
            if matched && !matches.iter().any(|(existing, _)| *existing == identity) {
                matches.push((identity, session.nickname.clone()));
            }
        }
        match matches.len() {
            0 => Err(format!("target '{token}' not found")),
            1 => Ok(matches[0].0),
            _ => {
                let mut lines = vec![format!(
                    "ambiguous: '{token}' matches {} identities:",
                    matches.len()
                )];
                for (identity, nickname) in &matches {
                    let nick_str = match nickname {
                        Some(nick) => format!("nick='{nick}'"),
                        None => "(no nick)".to_string(),
                    };
                    lines.push(format!("  - {} {nick_str}", hex::encode(identity)));
                }
                lines.push("Use full or longer identity hash to disambiguate.".to_string());
                Err(lines.join("\n"))
            }
        }
    }

    /// Resolve a command target, accepting a full identity hash for someone
    /// who is not connected. Grants outlive sessions now, so an operator must
    /// be able to lift a ban on an identity that is not currently online.
    fn resolve_target_or_hash(&self, token: &str) -> Result<[u8; 16], String> {
        if token.len() == 32 && token.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return hex::decode(token.to_lowercase())
                .ok()
                .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
                .ok_or_else(|| format!("bad identity hash: {token}"));
        }
        self.resolve_target(token)
    }

    fn cmd_list(&mut self, link_id: [u8; 16], identity: [u8; 16], out: &mut Vec<HubSend>) {
        let server_op = self.server_ops.contains(&identity);
        let mut rooms: Vec<(&String, &HubRoom)> = self
            .rooms
            .iter()
            .filter(|(_, room)| room.registered && (server_op || !room.private))
            .collect();
        rooms.sort_by(|a, b| a.0.cmp(b.0));
        if rooms.is_empty() {
            out.push(self.hub_notice(link_id, "No public rooms registered", None));
            return;
        }
        // Deliberately one packet, never chunked: NomadNet REPLACES its room
        // list per parsed notice, so a continuation chunk would leave only the
        // last one, and a headerless chunk is read as an MOTD instead.
        // Truncating with a visible count is the honest failure.
        let budget = self
            .text_body_budget(rrc::MessageType::Notice, None)
            .max(MIN_NOTICE_BODY_BYTES);
        let mut text = "Registered public rooms:".to_string();
        let total = rooms.len();
        let mut shown = 0usize;
        for (name, room) in rooms {
            let topic = room.topic.as_deref().map(clip_topic);
            let line = match topic {
                Some(topic) => format!("\n  {name} - {topic}"),
                None => format!("\n  {name}"),
            };
            // Leave room for the trailing count if more remain.
            let remaining = total - shown - 1;
            let tail = if remaining > 0 {
                format!("\n  (+{remaining} more)").len()
            } else {
                0
            };
            if text.len() + line.len() + tail > budget {
                break;
            }
            text.push_str(&line);
            shown += 1;
        }
        if shown < total {
            text.push_str(&format!("\n  (+{} more)", total - shown));
        }
        out.push(self.hub_notice(link_id, &text, None));
    }

    fn cmd_who(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        args: &[String],
        raw_room: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        let target = args.first().map(String::as_str).or(raw_room);
        let Some(target) = target else {
            out.push(self.hub_notice(link_id, "usage: /who [room]", None));
            return;
        };
        let room_name = match self.norm_room(target) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &format!("bad room: {reason}"), None));
                return;
            }
        };
        if self
            .rooms
            .get(&room_name)
            .is_some_and(|room| room.private && !self.server_ops.contains(&identity))
        {
            out.push(self.hub_notice(link_id, &format!("room {room_name} is private"), None));
            return;
        }
        let mut members: Vec<String> = Vec::new();
        if let Some(room) = self.rooms.get(&room_name) {
            let mut entries: Vec<(Option<String>, [u8; 16])> = room
                .members
                .iter()
                .filter_map(|member| self.sessions.get(member))
                .filter_map(|session| {
                    session
                        .identity
                        .map(|identity| (session.nickname.clone(), identity))
                })
                .collect();
            entries.sort_by_key(|(_, identity)| *identity);
            entries.dedup_by_key(|(_, identity)| *identity);
            for (nickname, identity) in entries {
                let ident = hex::encode(identity);
                match nickname {
                    Some(nick) => members.push(format!("{nick} ({})", &ident[..12])),
                    None => members.push(ident),
                }
            }
        }
        if members.is_empty() {
            members.push("(none)".to_string());
        }
        self.push_notice_entries(
            link_id,
            NoticeHeader::Every(&format!("members in {room_name}: ")),
            &members,
            ", ",
            None,
            out,
        );
    }

    fn cmd_kick(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        args: &[String],
        raw_room: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        let [room_arg, token] = args else {
            out.push(self.hub_notice(link_id, "usage: /kick <room> <nick|hashprefix>", None));
            return;
        };
        let room_name = match self.norm_room(room_arg) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &format!("bad room: {reason}"), raw_room));
                return;
            }
        };
        let authorized = self
            .rooms
            .get(&room_name)
            .is_some_and(|room| self.is_room_op(room, identity));
        if !authorized {
            out.push(self.hub_error(link_id, "not authorized", None));
            return;
        }
        let target = match self.resolve_target(token) {
            Ok(target) => target,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &reason, raw_room));
                return;
            }
        };
        let target_links: Vec<[u8; 16]> = self
            .rooms
            .get(&room_name)
            .map(|room| {
                room.members
                    .iter()
                    .copied()
                    .filter(|member| {
                        self.sessions
                            .get(member)
                            .is_some_and(|session| session.identity == Some(target))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if target_links.is_empty() {
            out.push(self.hub_notice(link_id, "target not in room", raw_room));
            return;
        }
        for target_link in target_links {
            let nickname = self
                .sessions
                .get(&target_link)
                .and_then(|session| session.nickname.clone());
            if let Some(session) = self.sessions.get_mut(&target_link) {
                session.rooms.remove(&room_name);
            }
            // Deviation: kicked members produce a PARTED fan-out so rosters
            // stay accurate (the reference silently drops them).
            self.remove_member_with_parted(
                &room_name,
                target_link,
                target,
                nickname.as_deref(),
                out,
            );
            out.push(self.hub_error(
                target_link,
                &format!("kicked from {room_name}"),
                Some(&room_name),
            ));
        }
        out.push(self.hub_notice(
            link_id,
            &format!("kicked {token} from {room_name}"),
            raw_room,
        ));
    }

    fn cmd_kline(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        args: &[String],
        out: &mut Vec<HubSend>,
    ) {
        if !self.server_ops.contains(&identity) {
            out.push(self.hub_error(link_id, "not authorized", None));
            return;
        }
        const USAGE: &str = "usage: /kline add|del|list [nick|hashprefix|hash]";
        let Some(op) = args.first().map(|op| op.to_lowercase()) else {
            out.push(self.hub_notice(link_id, USAGE, None));
            return;
        };
        if op == "list" {
            let mut items: Vec<String> = self
                .klines
                .read()
                .map(|klines| klines.iter().map(hex::encode).collect())
                .unwrap_or_default();
            items.sort();
            if items.is_empty() {
                items.push("(none)".to_string());
            }
            self.push_notice_entries(
                link_id,
                NoticeHeader::First("klines: "),
                &items,
                ", ",
                None,
                out,
            );
            return;
        }
        if op != "add" && op != "del" {
            out.push(self.hub_notice(link_id, USAGE, None));
            return;
        }
        let Some(token) = args.get(1) else {
            out.push(self.hub_notice(
                link_id,
                &format!("usage: /kline {op} <nick|hashprefix|hash>"),
                None,
            ));
            return;
        };
        let target = match self.resolve_target_or_hash(token) {
            Ok(target) => target,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &reason, None));
                return;
            }
        };
        if op == "add" {
            if let Ok(mut klines) = self.klines.write() {
                klines.insert(target);
            }
            self.persist_klines(Some(link_id), out);
            out.push(self.hub_notice(
                link_id,
                &format!("kline added for {}", hex::encode(target)),
                None,
            ));
            // A connected target is disconnected immediately (reference).
            let links: Vec<[u8; 16]> = self
                .sessions
                .iter()
                .filter(|(_, session)| session.identity == Some(target))
                .map(|(link, _)| *link)
                .collect();
            for target_link in links {
                out.push(self.hub_error(target_link, "banned", None));
                out.push(HubSend::Close {
                    link_id: target_link,
                });
                self.on_link_closed(target_link, out);
            }
        } else {
            let removed = self
                .klines
                .write()
                .map(|mut klines| klines.remove(&target))
                .unwrap_or(false);
            if removed {
                self.persist_klines(Some(link_id), out);
            }
            let text = if removed {
                format!("kline removed for {}", hex::encode(target))
            } else {
                format!("not klined: {}", hex::encode(target))
            };
            out.push(self.hub_notice(link_id, &text, None));
        }
    }

    fn cmd_register(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        args: &[String],
        raw_room: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        let Some(room_arg) = args.first() else {
            out.push(self.hub_notice(link_id, "usage: /register <room>", None));
            return;
        };
        let room_name = match self.norm_room(room_arg) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &format!("bad room: {reason}"), None));
                return;
            }
        };
        let present = self
            .sessions
            .get(&link_id)
            .is_some_and(|session| session.rooms.contains(&room_name));
        if !present {
            out.push(self.hub_notice(
                link_id,
                "must be present in the room to register it",
                raw_room,
            ));
            return;
        }
        let Some(room) = self.rooms.get_mut(&room_name) else {
            return;
        };
        if !self.server_ops.contains(&identity) {
            out.push(self.hub_error(
                link_id,
                "only a server operator can register",
                Some(&room_name),
            ));
            return;
        }
        if self.registered_room_count() >= self.config.max_registered_rooms {
            out.push(self.hub_notice(link_id, "registry is full", raw_room));
            return;
        }
        let room = self.rooms.get_mut(&room_name).expect("room exists");
        room.registered = true;
        room.no_outside_msgs = true;
        room.topic_ops_only = true;
        room.ops.insert(identity);
        room.last_used = now_unix();
        self.persist_room(&room_name, Some(link_id), out);
        out.push(self.hub_notice(link_id, &format!("registered room {room_name}"), raw_room));
    }

    fn cmd_unregister(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        args: &[String],
        raw_room: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        let Some(room_arg) = args.first() else {
            out.push(self.hub_notice(link_id, "usage: /unregister <room>", None));
            return;
        };
        let room_name = match self.norm_room(room_arg) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &format!("bad room: {reason}"), None));
                return;
            }
        };
        let Some(room) = self.rooms.get_mut(&room_name) else {
            out.push(self.hub_notice(
                link_id,
                &format!("room {room_name} is not registered"),
                raw_room,
            ));
            return;
        };
        if !room.registered {
            out.push(self.hub_notice(
                link_id,
                &format!("room {room_name} is not registered"),
                raw_room,
            ));
            return;
        }
        if !self.server_ops.contains(&identity) {
            out.push(self.hub_error(
                link_id,
                "only a server operator can unregister",
                Some(&room_name),
            ));
            return;
        }
        room.registered = false;
        let empty = room.members.is_empty();
        if empty {
            self.rooms.remove(&room_name);
        }
        out.push(HubSend::Persist(HubPersist {
            op: db::HubRoomOp::Removed {
                room_name: room_name.clone(),
            },
            origin: Some(link_id),
            room: Some(room_name.clone()),
        }));
        out.push(self.hub_notice(link_id, &format!("unregistered room {room_name}"), raw_room));
    }

    fn cmd_topic(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        args: &[String],
        raw_room: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        let Some(room_arg) = args.first() else {
            out.push(self.hub_notice(link_id, "usage: /topic <room> [topic]", None));
            return;
        };
        let room_name = match self.norm_room(room_arg) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &format!("bad room: {reason}"), None));
                return;
            }
        };
        let Some(room) = self.rooms.get(&room_name) else {
            out.push(self.hub_error(link_id, "no such room", Some(&room_name)));
            return;
        };
        if args.len() == 1 {
            let topic = room.topic.clone().unwrap_or_else(|| "(none)".to_string());
            out.push(self.hub_notice(
                link_id,
                &format!("topic for {room_name}: {topic}"),
                raw_room,
            ));
            return;
        }
        let authorized = {
            let room = self.rooms.get(&room_name).expect("room just ensured");
            self.is_room_op(room, identity) || !room.topic_ops_only
        };
        if !authorized {
            out.push(self.hub_error(link_id, "not authorized (+t)", Some(&room_name)));
            return;
        }
        let topic = args[1..].join(" ").trim().to_string();
        let room = self.rooms.get_mut(&room_name).expect("room just ensured");
        room.topic = (!topic.is_empty()).then(|| topic.clone());
        let display = if topic.is_empty() {
            "(cleared)".to_string()
        } else {
            topic
        };
        let members: Vec<[u8; 16]> = room.members.iter().copied().collect();
        for member in members {
            out.push(self.hub_notice(
                member,
                &format!("topic for {room_name} is now: {display}"),
                Some(&room_name),
            ));
        }
        self.persist_room(&room_name, Some(link_id), out);
    }

    fn broadcast_mode(&self, room_name: &str, out: &mut Vec<HubSend>) {
        let Some(room) = self.rooms.get(room_name) else {
            return;
        };
        let text = format!("mode for {room_name} is now: {}", room.mode_string());
        for member in room.members.iter().copied() {
            out.push(self.hub_notice(member, &text, Some(room_name)));
        }
    }

    fn cmd_mode(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        args: &[String],
        raw_room: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        const USAGE: &str = "usage: /mode <room> (+m|-m|+i|-i|+t|-t|+n|-n|+p|-p|+k|-k|+r|-r) [key] | /mode <room> (+o|-o|+v|-v) <nick|hashprefix|hash>";
        let (Some(room_arg), Some(flag)) = (args.first(), args.get(1)) else {
            out.push(self.hub_notice(link_id, USAGE, None));
            return;
        };
        let room_name = match self.norm_room(room_arg) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &format!("bad room: {reason}"), None));
                return;
            }
        };
        let Some(room) = self.rooms.get(&room_name) else {
            out.push(self.hub_error(link_id, "no such room", Some(&room_name)));
            return;
        };
        if !self.is_room_op(room, identity) {
            out.push(self.hub_error(link_id, "not authorized", None));
            return;
        }
        match flag.as_str() {
            "+m" | "-m" | "+i" | "-i" | "+t" | "-t" | "+n" | "-n" | "+p" | "-p" => {
                let enable = flag.starts_with('+');
                let room = self.rooms.get_mut(&room_name).expect("room just ensured");
                match &flag[1..] {
                    "m" => room.moderated = enable,
                    "i" => room.invite_only = enable,
                    "t" => room.topic_ops_only = enable,
                    "n" => room.no_outside_msgs = enable,
                    _ => room.private = enable,
                }
                self.broadcast_mode(&room_name, out);
                self.persist_room(&room_name, Some(link_id), out);
            }
            "+k" => {
                if args.len() < 3 {
                    out.push(self.hub_notice(link_id, "usage: /mode <room> +k <key>", raw_room));
                    return;
                }
                let key = Zeroizing::new(args[2..].join(" ").trim().to_string());
                if key.is_empty() {
                    out.push(self.hub_notice(link_id, "key must not be empty", raw_room));
                    return;
                }
                // The setter collapses whitespace while the JOIN gate reads the
                // body verbatim, so a spaced key could never be matched.
                if args[2..].len() > 1 || key.chars().any(char::is_whitespace) {
                    out.push(self.hub_notice(link_id, "key must not contain spaces", raw_room));
                    return;
                }
                if key.len() < MIN_ROOM_KEY_BYTES {
                    out.push(self.hub_notice(link_id, "key must be at least 8 bytes", raw_room));
                    return;
                }
                let digest = room_key_digest(
                    &self.pepper,
                    self.pepper_id,
                    &room_name,
                    &key,
                    rns_crypto::random::random_16(),
                );
                let room = self.rooms.get_mut(&room_name).expect("room exists");
                room.key = Some(digest);
                self.broadcast_mode(&room_name, out);
                self.persist_room(&room_name, Some(link_id), out);
            }
            "-k" => {
                let room = self.rooms.get_mut(&room_name).expect("room just ensured");
                room.key = None;
                self.broadcast_mode(&room_name, out);
            }
            "+r" | "-r" => {
                out.push(self.hub_notice(
                    link_id,
                    "use /register or /unregister to change +r",
                    raw_room,
                ));
            }
            "+o" | "-o" | "+v" | "-v" => {
                let Some(token) = args.get(2) else {
                    out.push(self.hub_notice(
                        link_id,
                        "usage: /mode <room> (+o|-o|+v|-v) <nick|hashprefix|hash>",
                        raw_room,
                    ));
                    return;
                };
                let target = match self.resolve_target_or_hash(token) {
                    Ok(target) => target,
                    Err(reason) => {
                        out.push(self.hub_notice(link_id, &reason, raw_room));
                        return;
                    }
                };
                let room = self.rooms.get_mut(&room_name).expect("room just ensured");
                match flag.as_str() {
                    "+o" => {
                        room.ops.insert(target);
                    }
                    "-o" => {
                        if self.server_ops.contains(&target) {
                            out.push(self.hub_notice(
                                link_id,
                                "cannot deop a server operator",
                                raw_room,
                            ));
                            return;
                        }
                        room.ops.remove(&target);
                    }
                    "+v" => {
                        room.voiced.insert(target);
                    }
                    _ => {
                        room.voiced.remove(&target);
                    }
                }
                let text = format!(
                    "mode for {room_name} is now: {flag} {}",
                    &hex::encode(target)[..12]
                );
                let members: Vec<[u8; 16]> = self
                    .rooms
                    .get(&room_name)
                    .map(|room| room.members.iter().copied().collect())
                    .unwrap_or_default();
                for member in members {
                    out.push(self.hub_notice(member, &text, Some(&room_name)));
                }
                self.persist_room(&room_name, Some(link_id), out);
            }
            _ => {
                out.push(self.hub_notice(
                    link_id,
                    "supported modes: +m -m +i -i +k -k +t -t +n -n +p -p +r -r +o -o +v -v",
                    raw_room,
                ));
            }
        }
    }

    fn cmd_grant(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        verb: &str,
        args: &[String],
        raw_room: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        let (Some(room_arg), Some(token)) = (args.first(), args.get(1)) else {
            out.push(self.hub_notice(
                link_id,
                &format!("usage: /{verb} <room> <nick|hashprefix|hash>"),
                None,
            ));
            return;
        };
        let room_name = match self.norm_room(room_arg) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &format!("bad room: {reason}"), None));
                return;
            }
        };
        let Some(room) = self.rooms.get(&room_name) else {
            out.push(self.hub_error(link_id, "no such room", Some(&room_name)));
            return;
        };
        if !self.is_room_op(room, identity) {
            out.push(self.hub_error(link_id, "not authorized", None));
            return;
        }
        let target = match self.resolve_target_or_hash(token) {
            Ok(target) => target,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &reason, raw_room));
                return;
            }
        };
        let room = self.rooms.get_mut(&room_name).expect("room just ensured");
        let text = match verb {
            "op" => {
                room.ops.insert(target);
                format!("op granted in {room_name}")
            }
            "deop" => {
                if self.server_ops.contains(&target) {
                    out.push(self.hub_notice(link_id, "cannot deop a server operator", raw_room));
                    return;
                }
                room.ops.remove(&target);
                format!("op removed in {room_name}")
            }
            "voice" => {
                room.voiced.insert(target);
                format!("voice granted in {room_name}")
            }
            _ => {
                room.voiced.remove(&target);
                format!("voice removed in {room_name}")
            }
        };
        self.persist_room(&room_name, Some(link_id), out);
        out.push(self.hub_notice(link_id, &text, raw_room));
    }

    fn cmd_ban(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        args: &[String],
        raw_room: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        const USAGE: &str = "usage: /ban <room> add|del|list [nick|hashprefix|hash]";
        let (Some(room_arg), Some(op)) = (args.first(), args.get(1).map(|op| op.to_lowercase()))
        else {
            out.push(self.hub_notice(link_id, USAGE, None));
            return;
        };
        let room_name = match self.norm_room(room_arg) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &format!("bad room: {reason}"), None));
                return;
            }
        };
        if op == "list" {
            let mut items: Vec<String> = self
                .rooms
                .get(&room_name)
                .map(|room| room.bans.iter().map(hex::encode).collect())
                .unwrap_or_default();
            items.sort();
            if items.is_empty() {
                out.push(self.hub_notice(link_id, &format!("no bans in {room_name}"), raw_room));
                return;
            }
            self.push_notice_entries(
                link_id,
                NoticeHeader::First(&format!("bans in {room_name}: ")),
                &items,
                ", ",
                raw_room,
                out,
            );
            return;
        }
        if op != "add" && op != "del" {
            out.push(self.hub_notice(link_id, USAGE, None));
            return;
        }
        let Some(room) = self.rooms.get(&room_name) else {
            out.push(self.hub_error(link_id, "no such room", Some(&room_name)));
            return;
        };
        if !self.is_room_op(room, identity) {
            out.push(self.hub_error(link_id, "not authorized", None));
            return;
        }
        let Some(token) = args.get(2) else {
            out.push(self.hub_notice(link_id, USAGE, None));
            return;
        };
        let target = match self.resolve_target_or_hash(token) {
            Ok(target) => target,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &reason, raw_room));
                return;
            }
        };
        if op == "add" {
            self.rooms
                .get_mut(&room_name)
                .expect("room just ensured")
                .bans
                .insert(target);
            let target_links: Vec<[u8; 16]> = self
                .rooms
                .get(&room_name)
                .map(|room| {
                    room.members
                        .iter()
                        .copied()
                        .filter(|member| {
                            self.sessions
                                .get(member)
                                .is_some_and(|session| session.identity == Some(target))
                        })
                        .collect()
                })
                .unwrap_or_default();
            for target_link in target_links {
                let nickname = self
                    .sessions
                    .get(&target_link)
                    .and_then(|session| session.nickname.clone());
                if let Some(session) = self.sessions.get_mut(&target_link) {
                    session.rooms.remove(&room_name);
                }
                // Deviation: banned members leave with a PARTED fan-out so
                // rosters stay accurate (the reference drops them silently).
                self.remove_member_with_parted(
                    &room_name,
                    target_link,
                    target,
                    nickname.as_deref(),
                    out,
                );
                out.push(self.hub_error(
                    target_link,
                    &format!("banned from {room_name}"),
                    Some(&room_name),
                ));
            }
            self.persist_room(&room_name, Some(link_id), out);
            out.push(self.hub_notice(link_id, &format!("ban added in {room_name}"), raw_room));
        } else {
            if let Some(room) = self.rooms.get_mut(&room_name) {
                room.bans.remove(&target);
            }
            self.persist_room(&room_name, Some(link_id), out);
            out.push(self.hub_notice(link_id, &format!("ban removed in {room_name}"), raw_room));
        }
    }

    fn cmd_invite(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        args: &[String],
        raw_room: Option<&str>,
        out: &mut Vec<HubSend>,
    ) {
        const USAGE: &str = "usage: /invite <room> add|del|list [nick|hashprefix|hash]";
        let (Some(room_arg), Some(op)) = (args.first(), args.get(1).map(|op| op.to_lowercase()))
        else {
            out.push(self.hub_notice(link_id, USAGE, None));
            return;
        };
        let room_name = match self.norm_room(room_arg) {
            Ok(room_name) => room_name,
            Err(reason) => {
                out.push(self.hub_notice(link_id, &format!("bad room: {reason}"), None));
                return;
            }
        };
        let Some(room) = self.rooms.get(&room_name) else {
            out.push(self.hub_error(link_id, "no such room", Some(&room_name)));
            return;
        };
        if !self.is_room_op(room, identity) {
            out.push(self.hub_error(link_id, "not authorized", None));
            return;
        }
        let now = now_unix();
        if op == "list" {
            let mut items: Vec<String> = self
                .rooms
                .get(&room_name)
                .map(|room| {
                    room.invited
                        .iter()
                        .filter(|(_, expires)| **expires > now)
                        .map(|(identity, expires)| {
                            format!(
                                "{} expires_in={}s",
                                hex::encode(identity),
                                (*expires - now).max(0.0) as u64
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            items.sort();
            if items.is_empty() {
                items.push("(none)".to_string());
            }
            self.push_notice_entries(
                link_id,
                NoticeHeader::First(&format!("invites in {room_name}: ")),
                &items,
                ", ",
                raw_room,
                out,
            );
            return;
        }
        if op != "add" && op != "del" {
            out.push(self.hub_notice(link_id, USAGE, None));
            return;
        }
        let Some(token) = args.get(2) else {
            out.push(self.hub_notice(
                link_id,
                &format!("usage: /invite {room_name} {op} <nick|hashprefix|hash>"),
                raw_room,
            ));
            return;
        };
        let target = match self.resolve_target_or_hash(token) {
            Ok(target) => target,
            Err(reason) => {
                out.push(self.hub_error(link_id, &format!("invite failed: {reason}"), None));
                return;
            }
        };
        if op == "add" {
            let ttl = Duration::from_secs(self.config.invite_timeout_secs.max(1));
            let (gated, keyed) = self
                .rooms
                .get(&room_name)
                .map(|room| (room.invite_only || room.key.is_some(), room.key.is_some()))
                .unwrap_or((false, false));
            // Only record a grant when there is a gate to bypass; an ungated
            // room needs no invite and would accrue dead rows.
            if gated {
                self.rooms
                    .get_mut(&room_name)
                    .expect("room exists")
                    .invited
                    .insert(target, now + ttl.as_secs_f64());
            }
            if let Some(target_link) = self.by_identity.get(&target).copied() {
                let text = if keyed {
                    format!(
                        "You have been invited to join {room_name}. This invite allows joining without the key (+k)."
                    )
                } else {
                    format!("You have been invited to join {room_name}.")
                };
                out.push(self.hub_notice(target_link, &text, Some(&room_name)));
            }
            let confirmation = if gated {
                format!(
                    "invite added in {room_name} (expires in {}s)",
                    ttl.as_secs()
                )
            } else {
                format!("invite sent to {token} for {room_name}")
            };
            if gated {
                self.persist_room(&room_name, Some(link_id), out);
            }
            out.push(self.hub_notice(link_id, &confirmation, raw_room));
        } else {
            if let Some(room) = self.rooms.get_mut(&room_name) {
                room.invited.remove(&target);
            }
            self.persist_room(&room_name, Some(link_id), out);
            out.push(self.hub_notice(link_id, &format!("invite removed in {room_name}"), raw_room));
        }
    }

    fn cmd_stats(&mut self, link_id: [u8; 16], identity: [u8; 16], out: &mut Vec<HubSend>) {
        if !self.server_ops.contains(&identity) {
            out.push(self.hub_error(link_id, "not authorized", None));
            return;
        }
        let (sessions, welcomed) = self.session_counts();
        let registered = self.rooms.values().filter(|room| room.registered).count();
        let klines = self.klines.read().map(|set| set.len()).unwrap_or(0);
        // Newline-joined on purpose: the reference emits one unbroken line.
        let text = [
            format!("hub {} v{HUB_VERSION}", self.config.hub_name),
            format!("uptime: {}s", self.started_at.elapsed().as_secs()),
            format!("sessions: {sessions} ({welcomed} welcomed)"),
            format!("rooms: {} ({registered} registered)", self.rooms.len()),
            format!(
                "registry: {}",
                if self.registry_degraded() {
                    "degraded"
                } else {
                    "ok"
                }
            ),
            format!(
                "relay: msgs={} notices={} actions={} direct={}",
                self.stats.messages_forwarded,
                self.stats.notices_forwarded,
                self.stats.actions_forwarded,
                self.stats.direct_notices
            ),
            format!(
                "control: joins={} parts={} pings_out={} pongs_in={}",
                self.stats.joins, self.stats.parts, self.stats.pings_out, self.stats.pongs_in
            ),
            format!(
                "dropped: rate_limited={} bad_packets={} duplicates={} oversize={}",
                self.stats.rate_limited,
                self.stats.bad_packets,
                self.stats.duplicates,
                self.stats.oversize
            ),
            format!("klines: {klines}"),
        ]
        .to_vec();
        self.push_notice_entries(link_id, NoticeHeader::None, &text, "\n", None, out);
    }

    fn cmd_reload(&mut self, link_id: [u8; 16], identity: [u8; 16], out: &mut Vec<HubSend>) {
        if !self.server_ops.contains(&identity) {
            out.push(self.hub_error(link_id, "not authorized", None));
            return;
        }
        // Deviation: hub configuration is managed through the app; the IPC
        // config path restarts the service to apply changes.
        out.push(self.hub_notice(
            link_id,
            "configuration is managed from the Ratspeak app settings",
            None,
        ));
    }

    /// HELLO always answers with WELCOME. Unlike the reference hub, a repeat
    /// HELLO from the same identity keeps session rooms: client HELLO retries
    /// race slow WELCOMEs, and a reset there silently wipes memberships.
    fn on_hello(&mut self, link_id: [u8; 16], envelope: rrc::Envelope, out: &mut Vec<HubSend>) {
        let capabilities = rrc::hello_capabilities(&envelope);
        let nickname = envelope.nickname.as_deref().and_then(|nickname| {
            rrc::normalize_nickname(nickname, self.config.max_nick_bytes).ok()
        });
        let welcome_body = rrc::welcome_body(&self.welcome_info());
        let Some(session) = self.sessions.get_mut(&link_id) else {
            return;
        };
        session.capabilities = capabilities;
        if nickname.is_some() {
            session.nickname = nickname;
        }
        let mut welcome = rrc::Envelope::new(rrc::MessageType::Welcome, self.hub_hash);
        welcome.body = Some(welcome_body);
        // The reference marks the session welcomed and then silently drops an
        // oversized WELCOME, leaving the client waiting forever. Only claim
        // the session is welcomed if the packet can actually go out.
        let sendable = rrc::encode(&welcome).is_ok_and(|bytes| bytes.len() <= LINK_PACKET_BUDGET);
        if !sendable {
            tracing::error!(reason = "welcome_over_mdu", "hub WELCOME does not fit");
            out.push(self.hub_error(link_id, "hub configuration error", None));
            return;
        }
        let first_welcome = !session.welcomed;
        if let Some(session) = self.sessions.get_mut(&link_id) {
            session.welcomed = true;
        }
        out.push(HubSend::Envelope {
            link_id,
            envelope: welcome,
        });

        if first_welcome && let Some(greeting) = self.config.greeting.clone() {
            self.push_notice_entries(
                link_id,
                NoticeHeader::None,
                std::slice::from_ref(&greeting),
                "",
                None,
                out,
            );
        }
    }

    /// Hub-driven keepalive: stamp and PING idle welcomed sessions, tear down
    /// links whose PONG never arrived, and reap links that never completed a
    /// handshake within the timeout window.
    pub(crate) fn ping_cycle(&mut self, now: Instant, out: &mut Vec<HubSend>) {
        if self.config.ping_interval_secs == 0 {
            return;
        }
        let timeout = Duration::from_secs(self.config.ping_timeout_secs.max(1));
        let mut dead = Vec::new();
        for (link_id, session) in self.sessions.iter_mut() {
            if !session.welcomed {
                if now.duration_since(session.established_at) > timeout {
                    dead.push(*link_id);
                }
                continue;
            }
            match session.awaiting_pong_since {
                Some(since) if now.duration_since(since) > timeout => dead.push(*link_id),
                Some(_) => {}
                None => {
                    session.awaiting_pong_since = Some(now);
                    self.stats.pings_out += 1;
                    let mut ping = rrc::Envelope::new(rrc::MessageType::Ping, self.hub_hash);
                    ping.body = Some(Value::Bytes(now_ms().to_be_bytes().to_vec()));
                    out.push(HubSend::Envelope {
                        link_id: *link_id,
                        envelope: ping,
                    });
                }
            }
        }
        for link_id in dead {
            self.sessions.remove(&link_id);
            out.push(HubSend::Close { link_id });
        }
    }
}

/// Clip a topic for the single-packet `/list` reply, on a char boundary.
fn clip_topic(topic: &str) -> String {
    if topic.len() <= LIST_TOPIC_BYTES {
        return topic.to_string();
    }
    let cut = (0..=LIST_TOPIC_BYTES)
        .rev()
        .find(|index| topic.is_char_boundary(*index))
        .unwrap_or(0);
    format!("{}…", &topic[..cut])
}

/// Split human text into UTF-8-boundary chunks of at most `max_bytes`.
fn chunk_text(text: &str, max_bytes: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if current.len() + ch.len_utf8() > max_bytes && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Wall clock, used wherever a value has to mean the same thing in memory and
/// on disk (invite expiry, room last-used).
fn now_unix() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// rrcd announces `{"proto": "rrc", "v": 1, "hub": <name>}` — the one RRC
/// structure keyed by text, and the only announce shape rrc-gui discovers.
pub fn hub_announce_app_data(hub_name: &str) -> Vec<u8> {
    let value = Value::Map(vec![
        (Value::Text("proto".into()), Value::Text("rrc".into())),
        (Value::Text("v".into()), Value::Integer(1.into())),
        (Value::Text("hub".into()), Value::Text(hub_name.into())),
    ]);
    let mut encoded = Vec::new();
    if ciborium::ser::into_writer(&value, &mut encoded).is_err() {
        encoded.clear();
    }
    encoded
}

/// Load the per-identity hub identity, generating and persisting one on first
/// start. The hub identity is deliberately distinct from the operator's chat
/// identity: joiners learn the hub hash, not the operator hash.
pub fn load_or_create_hub_identity(path: &std::path::Path) -> Result<Identity, String> {
    if path.exists() {
        return Identity::from_file(path).map_err(|error| error.to_string());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let identity = Identity::new();
    // to_file goes through atomic_write, which already creates 0600 and
    // fsyncs with a read-back verify.
    identity.to_file(path).map_err(|error| error.to_string())?;
    Ok(identity)
}

#[derive(Clone)]
pub struct ChannelHubHandle {
    command_tx: mpsc::Sender<HubCommand>,
    snapshot: Arc<RwLock<ChannelHubSnapshot>>,
}

impl ChannelHubHandle {
    /// Register the hub destination and spawn the service loop. Fails fast if
    /// the destination cannot be registered, so callers can surface the error.
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        transport_tx: mpsc::Sender<TransportMessage>,
        hub_identity: Identity,
        mut config: ChannelHubConfig,
        operator_identity: [u8; 16],
        store: HubStore,
        emitter: Arc<dyn Emitter>,
        shutdown: ShutdownSignal,
        _state: Weak<AppState>,
    ) -> Result<Self, ChannelHubError> {
        // The operator administers the hub through the normal client, so their
        // chat identity is always a server operator.
        if !config.server_operators.contains(&operator_identity) {
            config.server_operators.push(operator_identity);
        }
        // Load before registering. Booting with an empty registry would turn
        // every restored +i/+k/banned room into an open one, so a load failure
        // must stop the hub rather than start it wide open.
        let (restored, restored_klines) = store
            .load()
            .await
            .map_err(|error| ChannelHubError::Registry(error.to_string()))?;
        // Seed the kline set before the destination exists, otherwise a banned
        // identity can establish a link in the window before HubCore fills it.
        let mut seeded: HashSet<[u8; 16]> = config.banned_identities.iter().copied().collect();
        seeded.extend(restored_klines.iter().filter_map(|hex| {
            hex::decode(hex)
                .ok()
                .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
        }));
        let klines: Arc<RwLock<HashSet<[u8; 16]>>> = Arc::new(RwLock::new(seeded));
        let gate_klines = klines.clone();
        let options = DestinationRuntimeOptions {
            accepts_links: true,
            default_app_data: Some(hub_announce_app_data(&config.hub_name)),
            identity_gate: Some(IdentityGatePolicy::new(move |_link_id, identity| {
                gate_klines
                    .read()
                    .map(|klines| !klines.contains(&identity))
                    .unwrap_or(true)
            })),
            ..DestinationRuntimeOptions::default()
        };
        let registration = RegisteredDestination::register(
            transport_tx,
            hub_identity.clone(),
            rrc::RRC_HUB_ASPECT,
            options,
        )
        .await
        .map_err(|error| ChannelHubError::Registration(error.to_string()))?;

        let (command_tx, command_rx) = mpsc::channel(COMMAND_BUFFER);
        let snapshot = Arc::new(RwLock::new(ChannelHubSnapshot::stopped()));
        let pepper = room_key_pepper(&hub_identity).ok_or_else(|| {
            ChannelHubError::Identity("hub identity has no private key".to_string())
        })?;
        let core = HubCore::new(config.clone(), hub_identity.hash, klines, pepper, restored);
        tokio::spawn(run_hub(
            registration,
            core,
            config,
            store,
            emitter,
            shutdown,
            command_rx,
            snapshot.clone(),
        ));
        Ok(Self {
            command_tx,
            snapshot,
        })
    }

    pub fn snapshot(&self) -> ChannelHubSnapshot {
        self.snapshot
            .read()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|_| ChannelHubSnapshot::stopped())
    }

    pub async fn status(&self) -> Result<ChannelHubSnapshot, ChannelHubError> {
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(HubCommand::Status { result_tx })
            .await
            .map_err(|_| ChannelHubError::Stopped)?;
        result_rx.await.map_err(|_| ChannelHubError::Stopped)
    }

    pub async fn shutdown(&self) {
        let (result_tx, result_rx) = oneshot::channel();
        if self
            .command_tx
            .send(HubCommand::Shutdown { result_tx })
            .await
            .is_ok()
        {
            // The task flushes outstanding registry writes before it acks, so
            // this budget covers a slow disk, not just a channel round trip.
            let _ = tokio::time::timeout(Duration::from_secs(10), result_rx).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_hub(
    mut registration: RegisteredDestination,
    mut core: HubCore,
    config: ChannelHubConfig,
    store: HubStore,
    emitter: Arc<dyn Emitter>,
    shutdown: ShutdownSignal,
    mut command_rx: mpsc::Receiver<HubCommand>,
    snapshot: Arc<RwLock<ChannelHubSnapshot>>,
) {
    let destination_hash = registration.handle.destination_hash();
    let announce_options = || DestinationAnnounceOptions {
        app_data: Some(hub_announce_app_data(&config.hub_name)),
        ..DestinationAnnounceOptions::default()
    };
    if registration
        .handle
        .announce(announce_options())
        .await
        .is_err()
    {
        tracing::warn!(reason = "announce_failed", "channel hub announce failed");
    }

    let announce_period = if config.announce_interval_secs > 0 {
        Duration::from_secs(config.announce_interval_secs)
    } else {
        // Effectively never; announces stay manual/start-only.
        Duration::from_secs(u32::MAX as u64)
    };
    let mut announce_tick = tokio::time::interval(announce_period);
    announce_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    announce_tick.reset();
    let ping_period = Duration::from_secs(config.ping_interval_secs.max(1));
    let mut ping_tick = tokio::time::interval(ping_period);
    ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut prune_tick = tokio::time::interval(Duration::from_secs(
        config.room_registry_prune_interval_secs.max(1),
    ));
    prune_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    prune_tick.reset();

    publish_snapshot(&snapshot, &emitter, &core, &config, Some(destination_hash));
    tracing::info!(
        dest = %crate::short_id(&hex::encode(destination_hash)),
        "channel hub started"
    );

    loop {
        let mut out = Vec::new();
        tokio::select! {
            biased;
            _ = shutdown.wait() => break,
            command = command_rx.recv() => {
                match command {
                    Some(HubCommand::Status { result_tx }) => {
                        let _ = result_tx.send(current_snapshot(&core, &config, Some(destination_hash)));
                        continue;
                    }
                    Some(HubCommand::Shutdown { result_tx }) => {
                        let _ = result_tx.send(());
                        break;
                    }
                    None => break,
                }
            }
            established = registration.events.links_established.recv() => {
                match established {
                    Some(link_id) => core.on_link_established(link_id, Instant::now()),
                    None => break,
                }
            }
            identified = registration.events.links_identified.recv() => {
                if let Some((link_id, identity)) = identified {
                    core.on_link_identified(link_id, identity, Instant::now(), &mut out);
                }
            }
            packet = registration.events.link_packets.recv() => {
                match packet {
                    Some((data, link_id)) => {
                        // One token per inbound packet, charged before decode
                        // (reference accounting).
                        if !core.note_packet(link_id, Instant::now()) {
                            out.push(HubSend::Envelope {
                                link_id,
                                envelope: shell_error(&core, "rate limited"),
                            });
                            core.note_rate_limited();
                        } else {
                            match rrc::decode(&data) {
                                Ok(envelope) => core.on_envelope(link_id, envelope, &mut out),
                                Err(_) => {
                                    // Reference replies with the decode error
                                    // text; ours stays static so nothing
                                    // inbound echoes back out.
                                    core.note_bad_packet();
                                    out.push(HubSend::Envelope {
                                        link_id,
                                        envelope: shell_error(&core, "bad message: invalid envelope"),
                                    });
                                }
                            }
                        }
                    }
                    None => break,
                }
            }
            closed = registration.events.links_closed.recv() => {
                if let Some(link_id) = closed {
                    core.on_link_closed(link_id, &mut out);
                }
            }
            _ = ping_tick.tick() => core.ping_cycle(Instant::now(), &mut out),
            _ = prune_tick.tick() => {
                core.prune_registry(now_unix(), &mut out);
                core.flush_dirty_last_used(&mut out);
                core.retry_failed_persists(&mut out);
            }
            _ = announce_tick.tick() => {
                if config.announce_interval_secs > 0
                    && registration.handle.announce(announce_options()).await.is_err()
                {
                    tracing::warn!(reason = "announce_failed", "periodic hub announce failed");
                }
            }
        }
        flush_sends(&registration, &store, &mut core, out).await;
        publish_snapshot(&snapshot, &emitter, &core, &config, Some(destination_hash));
    }

    // Land any outstanding writes before the task ends: a restart reloads the
    // registry immediately and would otherwise race the old task's tail.
    let mut final_out = Vec::new();
    core.flush_dirty_last_used(&mut final_out);
    core.retry_failed_persists(&mut final_out);
    flush_sends(&registration, &store, &mut core, final_out).await;

    if let Ok(mut current) = snapshot.write() {
        *current = ChannelHubSnapshot::stopped();
    }
    let _ = emitter.try_emit(
        "channel_hub_snapshot",
        serde_json::to_value(ChannelHubSnapshot::stopped()).unwrap_or_default(),
    );
    if registration.close().await.is_err() {
        tracing::warn!(reason = "close_failed", "channel hub deregistration failed");
    }
    tracing::info!("channel hub stopped");
}

fn shell_error(core: &HubCore, text: &str) -> rrc::Envelope {
    let mut envelope = rrc::Envelope::new(rrc::MessageType::Error, core.hub_hash);
    envelope.body = Some(Value::Text(text.to_string()));
    envelope
}

/// Durable side of the hub registry. The sole hex boundary: `HubCore` speaks
/// bytes, the database stores hex.
#[derive(Clone)]
pub struct HubStore {
    pool: db::DbPool,
    identity_id: String,
}

impl HubStore {
    pub fn new(pool: db::DbPool, identity_id: String) -> Self {
        Self { pool, identity_id }
    }

    async fn load(&self) -> Result<(Vec<db::HubRoomRow>, Vec<String>), String> {
        let pool = self.pool.clone();
        let identity = self.identity_id.clone();
        db::spawn_db(pool, move |pool| {
            let rooms = db::list_hub_rooms(&pool, &identity)?;
            let klines = db::list_hub_klines(&pool, &identity)?;
            Ok::<_, String>((rooms, klines))
        })
        .await
        .map_err(|_| "registry load task panicked".to_string())?
    }

    /// Apply a whole batch in one transaction, in order. Ordering matters:
    /// two writes to the same room must not reorder, which is exactly what a
    /// task-per-op would risk.
    async fn apply_batch(&self, ops: Vec<db::HubRoomOp>) -> Result<(), String> {
        if ops.is_empty() {
            return Ok(());
        }
        let pool = self.pool.clone();
        let identity = self.identity_id.clone();
        db::spawn_db(pool, move |pool| db::apply_hub_ops(&pool, &identity, &ops))
            .await
            .map_err(|_| "registry write task panicked".to_string())?
    }
}

/// Apply durable writes first, then send. An operator confirmation must not
/// go out ahead of the write it is confirming.
async fn flush_sends(
    registration: &RegisteredDestination,
    store: &HubStore,
    core: &mut HubCore,
    sends: Vec<HubSend>,
) {
    let mut ops: Vec<db::HubRoomOp> = Vec::new();
    let mut failures: Vec<(Option<[u8; 16]>, Option<String>)> = Vec::new();
    let mut envelopes: Vec<HubSend> = Vec::with_capacity(sends.len());
    for send in sends {
        match send {
            HubSend::Persist(persist) => {
                ops.push(persist.op);
                failures.push((persist.origin, persist.room));
            }
            other => envelopes.push(other),
        }
    }

    if !ops.is_empty() && store.apply_batch(ops.clone()).await.is_err() {
        tracing::warn!(
            reason = "registry_write_failed",
            ops = ops.len(),
            "hub registry write failed"
        );
        for op in &ops {
            core.note_persist_failed(op);
        }
        // One static notice per originating link; the database error text is
        // never echoed outward.
        let mut told: HashSet<[u8; 16]> = HashSet::new();
        for (origin, room) in failures {
            if let Some(origin) = origin
                && told.insert(origin)
            {
                envelopes.insert(
                    0,
                    HubSend::Envelope {
                        link_id: origin,
                        envelope: {
                            let mut envelope =
                                rrc::Envelope::new(rrc::MessageType::Notice, core.hub_hash);
                            envelope.room = room;
                            envelope.body =
                                Some(Value::Text("room config persist failed".to_string()));
                            envelope
                        },
                    },
                );
            }
        }
    }

    let mut oversize = 0usize;
    for send in envelopes {
        match send {
            HubSend::Persist(_) => unreachable!("persist ops are partitioned out"),
            HubSend::Envelope { link_id, envelope } => match rrc::encode(&envelope) {
                Ok(encoded) if encoded.len() <= LINK_PACKET_BUDGET => {
                    if registration
                        .handle
                        .send_link_packet(link_id, encoded)
                        .await
                        .is_err()
                    {
                        tracing::debug!(reason = "send_failed", "hub envelope send failed");
                    }
                }
                Ok(encoded) => {
                    // Every producer sizes itself; reaching here is a bug, so
                    // surface it on /stats rather than only in a log.
                    oversize += 1;
                    tracing::warn!(
                        len = encoded.len(),
                        reason = "over_mdu",
                        "hub envelope dropped"
                    );
                }
                Err(_) => {
                    oversize += 1;
                    tracing::warn!(reason = "encode_failed", "hub envelope dropped");
                }
            },
            HubSend::Close { link_id } => {
                if registration
                    .handle
                    .close_link(link_id, CloseReason::DestinationClosed, true)
                    .await
                    .is_err()
                {
                    tracing::debug!(reason = "close_failed", "hub link close failed");
                }
            }
        }
    }
    if oversize > 0 {
        core.note_oversize(oversize);
    }
}

fn current_snapshot(
    core: &HubCore,
    config: &ChannelHubConfig,
    destination_hash: Option<[u8; 16]>,
) -> ChannelHubSnapshot {
    let (sessions, welcomed) = core.session_counts();
    ChannelHubSnapshot {
        running: true,
        hub_name: config.hub_name.clone(),
        destination_hash: destination_hash.map(hex::encode),
        sessions,
        welcomed_sessions: welcomed,
        rooms: core.room_count(),
        registered_rooms: core.registered_room_count(),
        registry_degraded: core.registry_degraded(),
        announce_interval_secs: config.announce_interval_secs,
        updated_at_ms: now_ms(),
    }
}

fn publish_snapshot(
    snapshot: &Arc<RwLock<ChannelHubSnapshot>>,
    emitter: &Arc<dyn Emitter>,
    core: &HubCore,
    config: &ChannelHubConfig,
    destination_hash: Option<[u8; 16]>,
) {
    let next = current_snapshot(core, config, destination_hash);
    let changed = snapshot
        .read()
        .map(|current| {
            current.running != next.running
                || current.sessions != next.sessions
                || current.welcomed_sessions != next.welcomed_sessions
                || current.rooms != next.rooms
                || current.registered_rooms != next.registered_rooms
                || current.registry_degraded != next.registry_degraded
        })
        .unwrap_or(true);
    if let Ok(mut current) = snapshot.write() {
        *current = next.clone();
    }
    if changed {
        let _ = emitter.try_emit(
            "channel_hub_snapshot",
            serde_json::to_value(next).unwrap_or_default(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ID_A is always a server operator, mirroring production: `start` seeds
    /// the hosting identity into `server_operators`. Rooms only exist because
    /// an operator made them, so most fixtures create through ID_A.
    fn core_with(mut config: ChannelHubConfig) -> HubCore {
        if !config.server_operators.contains(&ID_A) {
            config.server_operators.push(ID_A);
        }
        HubCore::new(
            config,
            [0x77; 16],
            Arc::new(RwLock::new(HashSet::new())),
            Zeroizing::new([0x5A; 32]),
            Vec::new(),
        )
    }

    fn identified_core(config: ChannelHubConfig) -> (HubCore, [u8; 16]) {
        let mut core = core_with(config);
        let link_id = [0x01; 16];
        let now = Instant::now();
        core.on_link_established(link_id, now);
        let mut out = Vec::new();
        core.on_link_identified(link_id, [0xAA; 16], now, &mut out);
        assert!(out.is_empty());
        (core, link_id)
    }

    fn first_envelope(out: &[HubSend]) -> &rrc::Envelope {
        match out.first().expect("at least one send") {
            HubSend::Envelope { envelope, .. } => envelope,
            other => panic!("expected an envelope send, got {other:?}"),
        }
    }

    #[test]
    fn hello_welcomes_with_configured_limits_and_capabilities() {
        let (mut core, link_id) = identified_core(ChannelHubConfig::default());
        let hello = rrc::Envelope::hello([0xAA; 16], "rat", "1.0.0");
        let mut out = Vec::new();
        core.on_envelope(link_id, hello, &mut out);

        let welcome = first_envelope(&out);
        assert_eq!(welcome.message_type, rrc::MessageType::Welcome);
        assert_eq!(welcome.source, [0x77; 16]);
        let info = rrc::parse_welcome(welcome);
        assert_eq!(info.hub_name.as_deref(), Some(DEFAULT_HUB_NAME));
        assert_eq!(info.limits.max_message_body_bytes, Some(350));
        assert_eq!(info.capabilities.get(&rrc::CAP_ACTION), Some(&true));
        assert_eq!(
            info.capabilities.get(&rrc::CAP_RESOURCE_ENVELOPE),
            Some(&true)
        );
    }

    #[test]
    fn duplicate_hello_rewelcomes_without_wiping_rooms() {
        let (mut core, link_id) = identified_core(ChannelHubConfig::default());
        let mut out = Vec::new();
        core.on_envelope(
            link_id,
            rrc::Envelope::hello([0xAA; 16], "rat", "1"),
            &mut out,
        );
        core.sessions
            .get_mut(&link_id)
            .unwrap()
            .rooms
            .insert("lobby".into());

        out.clear();
        core.on_envelope(
            link_id,
            rrc::Envelope::hello([0xAA; 16], "rat", "1"),
            &mut out,
        );
        assert_eq!(first_envelope(&out).message_type, rrc::MessageType::Welcome);
        assert!(core.sessions[&link_id].rooms.contains("lobby"));
    }

    #[test]
    fn greeting_is_sent_once_as_roomless_notices() {
        let config = ChannelHubConfig {
            greeting: Some("hello mesh".into()),
            ..ChannelHubConfig::default()
        };
        let (mut core, link_id) = identified_core(config);
        let mut out = Vec::new();
        core.on_envelope(
            link_id,
            rrc::Envelope::hello([0xAA; 16], "rat", "1"),
            &mut out,
        );
        assert_eq!(out.len(), 2);
        let HubSend::Envelope { envelope, .. } = &out[1] else {
            panic!("expected greeting notice");
        };
        assert_eq!(envelope.message_type, rrc::MessageType::Notice);
        assert_eq!(envelope.room, None);
        assert_eq!(rrc::text_body(envelope), Some("hello mesh"));

        out.clear();
        core.on_envelope(
            link_id,
            rrc::Envelope::hello([0xAA; 16], "rat", "1"),
            &mut out,
        );
        assert_eq!(out.len(), 1, "greeting must not repeat on re-HELLO");
    }

    #[test]
    fn pre_welcome_traffic_gets_hello_first_but_ping_pong_flows() {
        let (mut core, link_id) = identified_core(ChannelHubConfig::default());
        let mut out = Vec::new();
        let message =
            rrc::Envelope::room_text(rrc::MessageType::Message, [0xAA; 16], "lobby", "rat", "hi");
        core.on_envelope(link_id, message, &mut out);
        assert_eq!(
            rrc::text_body(first_envelope(&out)),
            Some("send HELLO first")
        );

        out.clear();
        let mut ping = rrc::Envelope::new(rrc::MessageType::Ping, [0xAA; 16]);
        ping.body = Some(Value::Bytes(vec![0x01, 0x02]));
        core.on_envelope(link_id, ping, &mut out);
        let pong = first_envelope(&out);
        assert_eq!(pong.message_type, rrc::MessageType::Pong);
        assert_eq!(pong.body, Some(Value::Bytes(vec![0x01, 0x02])));
    }

    #[test]
    fn unidentified_links_are_ignored_and_klines_reject_at_identify() {
        let mut core = core_with(ChannelHubConfig::default());
        let link_id = [0x02; 16];
        core.on_link_established(link_id, Instant::now());
        let mut out = Vec::new();
        core.on_envelope(
            link_id,
            rrc::Envelope::hello([0xBB; 16], "rat", "1"),
            &mut out,
        );
        assert!(out.is_empty(), "unidentified links get no replies");

        core.klines.write().unwrap().insert([0xBB; 16]);
        core.on_link_identified(link_id, [0xBB; 16], Instant::now(), &mut out);
        assert_eq!(rrc::text_body(first_envelope(&out)), Some("banned"));
        assert!(matches!(out[1], HubSend::Close { .. }));
        assert!(!core.sessions.contains_key(&link_id));
    }

    #[test]
    fn ping_cycle_stamps_then_reaps_silent_sessions() {
        let config = ChannelHubConfig {
            ping_interval_secs: 1,
            ping_timeout_secs: 1,
            ..ChannelHubConfig::default()
        };
        let (mut core, link_id) = identified_core(config);
        let mut out = Vec::new();
        core.on_envelope(
            link_id,
            rrc::Envelope::hello([0xAA; 16], "rat", "1"),
            &mut out,
        );

        out.clear();
        let start = Instant::now();
        core.ping_cycle(start, &mut out);
        assert_eq!(first_envelope(&out).message_type, rrc::MessageType::Ping);

        // PONG clears the pending stamp; the next cycle pings again.
        out.clear();
        let mut pong = rrc::Envelope::new(rrc::MessageType::Pong, [0xAA; 16]);
        pong.body = Some(Value::Bytes(vec![0x00]));
        core.on_envelope(link_id, pong, &mut out);
        core.ping_cycle(start + Duration::from_secs(2), &mut out);
        assert_eq!(first_envelope(&out).message_type, rrc::MessageType::Ping);

        // No PONG this time: the follow-up cycle reaps the link.
        out.clear();
        core.ping_cycle(start + Duration::from_secs(10), &mut out);
        assert!(matches!(out[0], HubSend::Close { .. }));
        assert!(core.sessions.is_empty());
    }

    #[test]
    fn announce_app_data_matches_reference_shape() {
        let encoded = hub_announce_app_data("Test Hub");
        let value: Value = ciborium::de::from_reader(encoded.as_slice()).unwrap();
        let Value::Map(entries) = value else {
            panic!("announce app data must be a map");
        };
        let hub = entries
            .iter()
            .find(|(key, _)| matches!(key, Value::Text(text) if text == "hub"))
            .map(|(_, value)| value.clone());
        assert_eq!(hub, Some(Value::Text("Test Hub".into())));
    }

    #[test]
    fn chunk_text_respects_utf8_boundaries() {
        let chunks = chunk_text("学习学习学习", 7);
        assert!(chunks.iter().all(|chunk| chunk.len() <= 7));
        assert_eq!(chunks.concat(), "学习学习学习");
    }

    const LINK_A: [u8; 16] = [0x01; 16];
    const LINK_B: [u8; 16] = [0x02; 16];
    const ID_A: [u8; 16] = [0xAA; 16];
    const ID_B: [u8; 16] = [0xBB; 16];

    fn welcomed_session(core: &mut HubCore, link_id: [u8; 16], identity: [u8; 16], nick: &str) {
        let now = Instant::now();
        core.on_link_established(link_id, now);
        let mut out = Vec::new();
        core.on_link_identified(link_id, identity, now, &mut out);
        core.on_envelope(link_id, rrc::Envelope::hello(identity, nick, "1"), &mut out);
    }

    fn join(core: &mut HubCore, link_id: [u8; 16], identity: [u8; 16], room: &str) -> Vec<HubSend> {
        let mut out = Vec::new();
        let mut envelope = rrc::Envelope::new(rrc::MessageType::Join, identity);
        envelope.room = Some(room.to_string());
        core.on_envelope(link_id, envelope, &mut out);
        out
    }

    fn sends_to(out: &[HubSend], target: [u8; 16]) -> Vec<&rrc::Envelope> {
        out.iter()
            .filter_map(|send| match send {
                HubSend::Envelope { link_id, envelope } if *link_id == target => Some(envelope),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn join_confirms_with_roster_status_notice_and_single_element_fanout() {
        let mut core = core_with(ChannelHubConfig::default());
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");

        let out = join(&mut core, LINK_A, ID_A, " Lobby ");
        let to_a = sends_to(&out, LINK_A);
        assert_eq!(to_a.len(), 2);
        assert_eq!(to_a[0].message_type, rrc::MessageType::Joined);
        assert_eq!(to_a[0].room.as_deref(), Some("lobby"));
        assert_eq!(rrc::member_identities(to_a[0]), vec![ID_A]);
        assert_eq!(
            rrc::text_body(to_a[1]),
            Some("room lobby: unregistered; mode=(none); topic=(none)")
        );
        assert_eq!(to_a[1].room.as_deref(), Some("lobby"));

        let out = join(&mut core, LINK_B, ID_B, "lobby");
        let fanout = sends_to(&out, LINK_A);
        assert_eq!(fanout.len(), 1);
        assert_eq!(rrc::member_identities(fanout[0]), vec![ID_B]);
        assert_eq!(fanout[0].nickname.as_deref(), Some("beta"));
        let roster = sends_to(&out, LINK_B);
        let mut members = rrc::member_identities(roster[0]);
        members.sort();
        assert_eq!(members, vec![ID_A, ID_B]);
    }

    #[test]
    fn join_gates_follow_reference_order_and_texts() {
        let mut core = core_with(ChannelHubConfig::default());
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        join(&mut core, LINK_A, ID_A, "vault");
        {
            let room = core.rooms.get_mut("vault").unwrap();
            room.invite_only = true;
            room.key = Some(room_key_digest(
                &[0x5A; 32],
                [0x77; 8],
                "vault",
                "sesame99",
                [0x11; 16],
            ));
        }

        let out = join(&mut core, LINK_B, ID_B, "vault");
        let error = sends_to(&out, LINK_B)[0];
        assert_eq!(rrc::text_body(error), Some("invite-only (+i)"));
        assert_eq!(error.room.as_deref(), Some("vault"));

        // An invite bypasses both +i and +k, and is consumed by the join.
        core.rooms
            .get_mut("vault")
            .unwrap()
            .invited
            .insert(ID_B, now_unix() + 60.0);
        let out = join(&mut core, LINK_B, ID_B, "vault");
        assert_eq!(
            sends_to(&out, LINK_B)[0].message_type,
            rrc::MessageType::Joined
        );
        assert!(!core.rooms.get("vault").unwrap().invited.contains_key(&ID_B));

        // Banned identities are refused even with the key.
        let mut part = rrc::Envelope::new(rrc::MessageType::Part, ID_B);
        part.room = Some("vault".to_string());
        let mut out = Vec::new();
        core.on_envelope(LINK_B, part, &mut out);
        core.rooms.get_mut("vault").unwrap().invite_only = false;
        core.rooms.get_mut("vault").unwrap().bans.insert(ID_B);
        let mut envelope = rrc::Envelope::new(rrc::MessageType::Join, ID_B);
        envelope.room = Some("vault".to_string());
        envelope.body = Some(Value::Text("sesame99".to_string()));
        let mut out = Vec::new();
        core.on_envelope(LINK_B, envelope, &mut out);
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("banned from room")
        );

        // Wrong key text, once unbanned.
        core.rooms.get_mut("vault").unwrap().bans.clear();
        let mut envelope = rrc::Envelope::new(rrc::MessageType::Join, ID_B);
        envelope.room = Some("vault".to_string());
        envelope.body = Some(Value::Text("wrong".to_string()));
        let mut out = Vec::new();
        core.on_envelope(LINK_B, envelope, &mut out);
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("bad key (+k)")
        );
    }

    #[test]
    fn join_limit_skips_rooms_this_session_already_occupies() {
        let config = ChannelHubConfig {
            max_rooms_per_session: 1,
            ..ChannelHubConfig::default()
        };
        let mut core = core_with(config);
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        join(&mut core, LINK_A, ID_A, "lobby");

        // Re-joining the same room is not refused at the limit (deviation).
        let out = join(&mut core, LINK_A, ID_A, "lobby");
        assert_eq!(
            sends_to(&out, LINK_A)[0].message_type,
            rrc::MessageType::Joined
        );

        let out = join(&mut core, LINK_A, ID_A, "second");
        let error = sends_to(&out, LINK_A)[0];
        assert_eq!(rrc::text_body(error), Some("too many rooms"));
        assert_eq!(error.room, None);
    }

    #[test]
    fn part_suppresses_fanout_while_identity_remains_on_another_link() {
        let mut core = core_with(ChannelHubConfig::default());
        let link_a2 = [0x03; 16];
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        welcomed_session(&mut core, link_a2, ID_A, "alpha");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, link_a2, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");

        let mut part = rrc::Envelope::new(rrc::MessageType::Part, ID_A);
        part.room = Some("lobby".to_string());
        let mut out = Vec::new();
        core.on_envelope(LINK_A, part, &mut out);
        assert!(
            sends_to(&out, LINK_B).is_empty(),
            "identity still present via the second link"
        );
        assert_eq!(
            sends_to(&out, LINK_A)[0].message_type,
            rrc::MessageType::Parted
        );

        let mut part = rrc::Envelope::new(rrc::MessageType::Part, ID_A);
        part.room = Some("lobby".to_string());
        let mut out = Vec::new();
        core.on_envelope(link_a2, part, &mut out);
        let to_b = sends_to(&out, LINK_B);
        assert_eq!(to_b.len(), 1);
        assert_eq!(rrc::member_identities(to_b[0]), vec![ID_A]);
        assert_eq!(to_b[0].nickname.as_deref(), Some("alpha"));
    }

    #[test]
    fn relay_echoes_to_sender_and_rewrites_source_room_and_nick() {
        let mut core = core_with(ChannelHubConfig::default());
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");

        let mut message = rrc::Envelope::new(rrc::MessageType::Message, [0xEE; 16]);
        message.room = Some("Lobby".to_string());
        message.body = Some(Value::Text("hi".to_string()));
        let original_id = message.message_id;
        let mut out = Vec::new();
        core.on_envelope(LINK_B, message, &mut out);

        let to_a = sends_to(&out, LINK_A);
        let echo = sends_to(&out, LINK_B);
        assert_eq!(to_a.len(), 1);
        assert_eq!(echo.len(), 1, "sender must receive its own echo");
        for forwarded in [to_a[0], echo[0]] {
            assert_eq!(forwarded.source, ID_B, "source is the authenticated peer");
            assert_eq!(forwarded.room.as_deref(), Some("lobby"));
            assert_eq!(forwarded.message_id, original_id);
            assert_eq!(forwarded.nickname.as_deref(), Some("beta"));
        }
    }

    #[test]
    fn relay_gate_errors_use_reference_texts() {
        let config = ChannelHubConfig {
            max_message_body_bytes: 10,
            ..ChannelHubConfig::default()
        };
        let mut core = core_with(config);
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        join(&mut core, LINK_A, ID_A, "lobby");

        let send_msg = |core: &mut HubCore, link: [u8; 16], room: &str, text: &str| {
            let mut msg = rrc::Envelope::new(rrc::MessageType::Message, [0x00; 16]);
            msg.room = Some(room.to_string());
            msg.body = Some(Value::Text(text.to_string()));
            let mut out = Vec::new();
            core.on_envelope(link, msg, &mut out);
            out
        };

        let out = send_msg(&mut core, LINK_B, "ghost", "hi");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("no such room")
        );

        core.rooms.get_mut("lobby").unwrap().no_outside_msgs = true;
        let out = send_msg(&mut core, LINK_B, "lobby", "hi");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("no outside messages (+n)")
        );

        join(&mut core, LINK_B, ID_B, "lobby");
        core.rooms.get_mut("lobby").unwrap().moderated = true;
        let out = send_msg(&mut core, LINK_B, "lobby", "hi");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("room is moderated (+m)")
        );

        core.rooms.get_mut("lobby").unwrap().moderated = false;
        let out = send_msg(&mut core, LINK_B, "lobby", "this is far too long");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("message too large: 20 bytes > 10 bytes")
        );
    }

    #[test]
    fn notices_drop_roomless_and_direct_notices_route_exactly_once() {
        let mut core = core_with(ChannelHubConfig::default());
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");

        let mut roomless = rrc::Envelope::new(rrc::MessageType::Notice, ID_A);
        roomless.body = Some(Value::Text("hello".to_string()));
        let mut out = Vec::new();
        core.on_envelope(LINK_A, roomless, &mut out);
        assert!(out.is_empty(), "roomless notices drop silently");

        let mut direct = rrc::Envelope::new(rrc::MessageType::Notice, ID_A);
        direct.body = Some(Value::Text("psst".to_string()));
        direct.destination = Some([0xCC; 16]);
        let mut out = Vec::new();
        core.on_envelope(LINK_A, direct, &mut out);
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("destination not connected")
        );

        let mut direct = rrc::Envelope::new(rrc::MessageType::Notice, ID_A);
        direct.body = Some(Value::Text("psst".to_string()));
        direct.destination = Some(ID_B);
        direct.room = Some("lobby".to_string());
        let mut out = Vec::new();
        core.on_envelope(LINK_A, direct, &mut out);
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("direct notice must not include room")
        );

        let mut direct = rrc::Envelope::new(rrc::MessageType::Notice, ID_A);
        direct.body = Some(Value::Text("psst".to_string()));
        direct.destination = Some(ID_B);
        let mut out = Vec::new();
        core.on_envelope(LINK_A, direct, &mut out);
        let delivered = sends_to(&out, LINK_B);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].source, ID_A);
        assert_eq!(delivered[0].destination, Some(ID_B));
        assert!(
            sends_to(&out, LINK_A).is_empty(),
            "no echo for direct notices"
        );
    }

    #[test]
    fn slash_commands_reply_unrecognized_with_raw_room() {
        let mut core = core_with(ChannelHubConfig::default());
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        let mut msg = rrc::Envelope::new(rrc::MessageType::Message, ID_A);
        msg.room = Some("Lobby".to_string());
        msg.body = Some(Value::Text("/frobnicate now".to_string()));
        let mut out = Vec::new();
        core.on_envelope(LINK_A, msg, &mut out);
        let error = sends_to(&out, LINK_A)[0];
        assert_eq!(rrc::text_body(error), Some("unrecognized command"));
        assert_eq!(error.room.as_deref(), Some("Lobby"));
    }

    fn op_core() -> HubCore {
        // ID_A is a server operator, so it is an implicit op everywhere.
        let config = ChannelHubConfig {
            server_operators: vec![ID_A],
            ..ChannelHubConfig::default()
        };
        let mut core = core_with(config);
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        core
    }

    fn run_command(core: &mut HubCore, link: [u8; 16], id: [u8; 16], text: &str) -> Vec<HubSend> {
        let mut msg = rrc::Envelope::new(rrc::MessageType::Message, id);
        msg.room = Some("lobby".to_string());
        msg.body = Some(Value::Text(text.to_string()));
        let mut out = Vec::new();
        core.on_envelope(link, msg, &mut out);
        out
    }

    #[test]
    fn who_and_list_replies_match_reference_wire_text() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");

        let out = run_command(&mut core, LINK_A, ID_A, "/who lobby");
        let who = sends_to(&out, LINK_A)[0];
        assert_eq!(who.room, None);
        let body = rrc::text_body(who).unwrap();
        assert!(body.starts_with("members in lobby: "));
        assert!(body.contains("alpha (") && body.contains("beta ("));

        let out = run_command(&mut core, LINK_A, ID_A, "/list");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("No public rooms registered")
        );

        // Register the room, set a topic, and it appears in /list.
        run_command(&mut core, LINK_A, ID_A, "/topic lobby hello there");
        run_command(&mut core, LINK_A, ID_A, "/register lobby");
        let out = run_command(&mut core, LINK_A, ID_A, "/list");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("Registered public rooms:\n  lobby - hello there")
        );
    }

    fn persist_ops(out: &[HubSend]) -> Vec<&db::HubRoomOp> {
        out.iter()
            .filter_map(|send| match send {
                HubSend::Persist(persist) => Some(&persist.op),
                _ => None,
            })
            .collect()
    }

    fn upserted<'a>(out: &'a [HubSend], room: &str) -> Option<&'a db::HubRoomRow> {
        persist_ops(out).into_iter().find_map(|op| match op {
            db::HubRoomOp::Upsert(row) if row.room_name == room => Some(&**row),
            _ => None,
        })
    }

    /// Every envelope a batch would put on the wire must fit a link packet.
    fn assert_all_sendable(out: &[HubSend]) {
        for send in out {
            if let HubSend::Envelope { envelope, .. } = send {
                let encoded = rrc::encode(envelope).expect("hub envelopes must encode");
                assert!(
                    encoded.len() <= LINK_PACKET_BUDGET,
                    "{:?} encoded to {} bytes, over the {LINK_PACKET_BUDGET} budget",
                    envelope.message_type,
                    encoded.len()
                );
            }
        }
    }

    #[test]
    fn a_max_size_message_relays_and_sheds_the_nickname_to_fit() {
        // A 350-byte body plus a 32-byte nickname overflows a link packet; the
        // nickname is advisory and both reference clients cache nicks by
        // source, so shedding it saves the message.
        let mut core = op_core();
        let room = "lobby";
        welcomed_session(&mut core, LINK_B, ID_B, &"n".repeat(32));
        join(&mut core, LINK_A, ID_A, room);
        join(&mut core, LINK_B, ID_B, room);

        let mut message = rrc::Envelope::new(rrc::MessageType::Message, ID_B);
        message.room = Some(room.to_string());
        message.nickname = Some("n".repeat(32));
        message.body = Some(Value::Text("m".repeat(350)));
        let mut out = Vec::new();
        core.on_envelope(LINK_B, message, &mut out);

        let relayed = sends_to(&out, LINK_A);
        assert_eq!(relayed.len(), 1, "the message must be relayed, not dropped");
        assert_all_sendable(&out);
        assert_eq!(rrc::text_body(relayed[0]).map(str::len), Some(350));
    }

    #[test]
    fn a_message_that_cannot_fit_is_refused_with_a_reason() {
        // In a 64-byte room a 350-byte body cannot fit a 431-byte packet at
        // all. The reference silently dropped it; we say so instead. The
        // advertised limit stays 350 for rrcd parity.
        let mut core = op_core();
        let room = "r".repeat(64);
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        join(&mut core, LINK_A, ID_A, &room);
        join(&mut core, LINK_B, ID_B, &room);

        let mut message = rrc::Envelope::new(rrc::MessageType::Message, ID_B);
        message.room = Some(room.clone());
        message.body = Some(Value::Text("m".repeat(350)));
        let mut out = Vec::new();
        core.on_envelope(LINK_B, message, &mut out);

        assert!(sends_to(&out, LINK_A).is_empty(), "nothing is fanned out");
        let error = sends_to(&out, LINK_B)[0];
        assert_eq!(error.message_type, rrc::MessageType::Error);
        assert!(
            rrc::text_body(error)
                .is_some_and(|text| text.starts_with(&format!("message too large for {room}"))),
            "the sender must learn why"
        );
        assert_eq!(error.room.as_deref(), Some(room.as_str()));
        assert_all_sendable(&out);
    }

    #[test]
    fn a_large_roster_is_split_rather_than_dropped_or_truncated() {
        let mut core = op_core();
        let room = "r".repeat(64);
        join(&mut core, LINK_A, ID_A, &room);
        // Fill the room past what one packet can carry.
        let mut expected = vec![ID_A];
        for index in 0..40u8 {
            let link = [
                0xF0 | (index >> 4),
                index,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                index,
            ];
            let identity = [index; 16];
            welcomed_session(&mut core, link, identity, "m");
            join(&mut core, link, identity, &room);
            expected.push(identity);
        }

        let out = join(&mut core, LINK_A, ID_A, &room);
        assert_all_sendable(&out);
        let joined: Vec<&rrc::Envelope> = sends_to(&out, LINK_A)
            .into_iter()
            .filter(|env| env.message_type == rrc::MessageType::Joined)
            .collect();
        assert!(joined.len() > 1, "a 41-member roster needs several packets");
        // No chunk may be a single identity: clients read that as a join.
        for chunk in &joined {
            assert!(rrc::member_identities(chunk).len() > 1);
        }
        let mut seen: Vec<[u8; 16]> = joined
            .iter()
            .flat_map(|env| rrc::member_identities(env))
            .collect();
        seen.sort();
        seen.dedup();
        expected.sort();
        expected.dedup();
        assert_eq!(seen, expected, "the roster must be complete across chunks");
        assert_eq!(
            rrc::member_identities(joined[0]).first(),
            Some(&ID_A),
            "the joiner leads chunk 0 so it can recognise its own join"
        );
    }

    #[test]
    fn who_repeats_its_prefix_on_every_packet() {
        let mut core = op_core();
        let room = "lobby";
        join(&mut core, LINK_A, ID_A, room);
        for index in 0..30u8 {
            let link = [0xA0, index, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, index];
            welcomed_session(&mut core, link, [index; 16], &format!("member{index:02}"));
            join(&mut core, link, [index; 16], room);
        }

        let out = run_command(&mut core, LINK_A, ID_A, "/who lobby");
        assert_all_sendable(&out);
        let notices: Vec<&rrc::Envelope> = sends_to(&out, LINK_A);
        assert!(notices.len() > 1, "30 members exceed one packet");
        for notice in &notices {
            // NomadNet parses each packet independently and requires the
            // prefix; a continuation without it is discarded.
            assert!(
                rrc::text_body(notice).is_some_and(|text| text.starts_with("members in lobby: ")),
                "every /who packet must carry the prefix"
            );
        }
    }

    #[test]
    fn list_stays_one_packet_and_says_what_it_dropped() {
        let mut core = op_core();
        for index in 0..40u8 {
            let room = format!("room-{index:02}-{}", "x".repeat(20));
            join(&mut core, LINK_A, ID_A, &room);
            run_command(&mut core, LINK_A, ID_A, &format!("/register {room}"));
            run_command(
                &mut core,
                LINK_A,
                ID_A,
                &format!("/topic {room} {}", "t".repeat(120)),
            );
        }

        let out = run_command(&mut core, LINK_A, ID_A, "/list");
        assert_all_sendable(&out);
        let notices = sends_to(&out, LINK_A);
        // A continuation chunk would clobber NomadNet's room list and MOTD.
        assert_eq!(notices.len(), 1, "/list must never be chunked");
        let text = rrc::text_body(notices[0]).unwrap();
        assert!(text.starts_with("Registered public rooms:"));
        assert!(text.contains("more)"), "truncation must be visible: {text}");
    }

    #[test]
    fn oversized_replies_are_split_not_dropped() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        run_command(&mut core, LINK_A, ID_A, "/register lobby");
        // Ban a large number of identities so the list cannot fit one packet.
        for index in 0..30u8 {
            core.rooms
                .get_mut("lobby")
                .unwrap()
                .bans
                .insert([index; 16]);
        }
        let out = run_command(&mut core, LINK_A, ID_A, "/ban lobby list");
        assert_all_sendable(&out);
        let text: String = sends_to(&out, LINK_A)
            .iter()
            .filter_map(|env| rrc::text_body(env))
            .collect::<Vec<_>>()
            .join("");
        for index in 0..30u8 {
            assert!(
                text.contains(&hex::encode([index; 16])),
                "every ban must survive the split"
            );
        }
    }

    #[test]
    fn a_welcome_that_cannot_be_sent_never_marks_the_session_welcomed() {
        let config = ChannelHubConfig {
            // Far past anything the IPC layer allows, but the hub must not
            // hang a client if it ever happens.
            hub_name: "h".repeat(4096),
            ..ChannelHubConfig::default()
        };
        let (mut core, link_id) = identified_core(config);
        let mut out = Vec::new();
        core.on_envelope(link_id, rrc::Envelope::hello(ID_A, "rat", "1"), &mut out);

        assert_all_sendable(&out);
        assert!(
            !core.sessions[&link_id].welcomed,
            "a session is only welcomed when the WELCOME can actually go out"
        );
        assert_eq!(
            rrc::text_body(first_envelope(&out)),
            Some("hub configuration error")
        );
    }

    #[test]
    fn durable_grants_accept_a_full_hash_for_an_offline_identity() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        run_command(&mut core, LINK_A, ID_A, "/register lobby");
        // Nobody is connected under this identity, so a nickname lookup could
        // never find it — but a ban outlives the session that earned it.
        let offline = [0xEE; 16];
        let token = hex::encode(offline);

        let out = run_command(&mut core, LINK_A, ID_A, &format!("/ban lobby add {token}"));
        assert!(
            sends_to(&out, LINK_A)
                .iter()
                .any(|env| rrc::text_body(env) == Some("ban added in lobby"))
        );
        assert!(core.rooms.get("lobby").unwrap().bans.contains(&offline));

        // And it can be lifted again without the identity coming back online.
        let out = run_command(&mut core, LINK_A, ID_A, &format!("/ban lobby del {token}"));
        assert!(
            sends_to(&out, LINK_A)
                .iter()
                .any(|env| rrc::text_body(env) == Some("ban removed in lobby"))
        );
        assert!(!core.rooms.get("lobby").unwrap().bans.contains(&offline));

        // An op grant works the same way.
        run_command(&mut core, LINK_A, ID_A, &format!("/op lobby {token}"));
        assert!(core.rooms.get("lobby").unwrap().ops.contains(&offline));

        // A malformed hash is still rejected.
        let out = run_command(&mut core, LINK_A, ID_A, "/ban lobby add zz00");
        assert!(
            sends_to(&out, LINK_A)
                .iter()
                .any(|env| rrc::text_body(env) == Some("target 'zz00' not found"))
        );
    }

    #[test]
    fn only_registered_rooms_are_persisted() {
        let mut core = op_core();
        let out = join(&mut core, LINK_A, ID_A, "lobby");
        assert!(
            persist_ops(&out).is_empty(),
            "an unregistered room is ephemeral"
        );

        let out = run_command(&mut core, LINK_A, ID_A, "/topic lobby hi");
        assert!(
            persist_ops(&out).is_empty(),
            "editing an unregistered room still persists nothing"
        );

        let out = run_command(&mut core, LINK_A, ID_A, "/register lobby");
        assert!(upserted(&out, "lobby").is_some());
    }

    #[test]
    fn a_registered_room_projects_its_policy_and_grants() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "vault");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        run_command(&mut core, LINK_A, ID_A, "/register vault");
        run_command(&mut core, LINK_A, ID_A, "/mode vault +p");
        run_command(&mut core, LINK_A, ID_A, "/mode vault +k open-sesame");
        run_command(&mut core, LINK_A, ID_A, "/voice vault beta");
        let out = run_command(&mut core, LINK_A, ID_A, "/ban vault add beta");

        let row = upserted(&out, "vault").expect("ban persists the room");
        // +p must survive a restart; the reference loses it.
        assert!(row.private);
        // /register forces these two on.
        assert!(row.no_outside_msgs && row.topic_ops_only);
        assert!(!row.key_mac.is_empty() && !row.key_salt.is_empty());
        let kinds: Vec<&str> = row.grants.iter().map(|(k, _, _)| k.as_str()).collect();
        assert!(kinds.contains(&"op"), "the operator's grant is recorded");
        assert!(kinds.contains(&"voice") && kinds.contains(&"ban"));
    }

    #[test]
    fn unregister_removes_the_room_from_the_registry() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        run_command(&mut core, LINK_A, ID_A, "/register lobby");
        let out = run_command(&mut core, LINK_A, ID_A, "/unregister lobby");
        assert!(matches!(
            persist_ops(&out).first(),
            Some(db::HubRoomOp::Removed { room_name }) if room_name == "lobby"
        ));
    }

    #[test]
    fn klines_persist_the_whole_set() {
        let mut core = op_core();
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        let out = run_command(&mut core, LINK_A, ID_A, "/kline add beta");
        let Some(db::HubRoomOp::ReplaceKlines(subjects)) = persist_ops(&out).first().copied()
        else {
            panic!("expected a kline write");
        };
        assert_eq!(subjects, &vec![hex::encode(ID_B)]);
    }

    #[test]
    fn a_failed_write_is_retried_from_live_state() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        let out = run_command(&mut core, LINK_A, ID_A, "/register lobby");
        assert!(!core.registry_degraded());

        for op in persist_ops(&out) {
            core.note_persist_failed(op);
        }
        assert!(
            core.registry_degraded(),
            "the operator must be able to see this"
        );

        // The retry re-projects from live state rather than replaying the
        // failed snapshot, so a change made meanwhile is carried along.
        core.rooms.get_mut("lobby").unwrap().topic = Some("recovered".to_string());
        let mut retry = Vec::new();
        core.retry_failed_persists(&mut retry);
        let row = upserted(&retry, "lobby").expect("the failed room is retried");
        assert_eq!(row.topic, "recovered");
        assert!(!core.registry_degraded(), "the retry clears the flag");
    }

    #[test]
    fn prune_drops_only_idle_registered_rooms_and_respects_the_clock() {
        let config = ChannelHubConfig {
            room_registry_prune_after_secs: 10,
            room_registry_prune_interval_secs: 0,
            ..ChannelHubConfig::default()
        };
        let mut core = core_with(config);
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        join(&mut core, LINK_A, ID_A, "occupied");
        run_command(&mut core, LINK_A, ID_A, "/register occupied");
        join(&mut core, LINK_A, ID_A, "idle");
        run_command(&mut core, LINK_A, ID_A, "/register idle");
        // Vacate the second room.
        let mut part = rrc::Envelope::new(rrc::MessageType::Part, ID_A);
        part.room = Some("idle".to_string());
        let mut out = Vec::new();
        core.on_envelope(LINK_A, part, &mut out);
        core.rooms.get_mut("idle").unwrap().last_used = 1_800_000_000.0;

        // A device with no RTC must never prune.
        let mut out = Vec::new();
        core.prune_registry(1_000.0, &mut out);
        assert!(out.is_empty(), "an implausible clock disables pruning");
        assert!(core.rooms.contains_key("idle"));

        // A clock that jumped backwards must not age a room out either.
        let mut out = Vec::new();
        core.prune_registry(1_799_999_000.0, &mut out);
        assert!(core.rooms.contains_key("idle"));

        let mut out = Vec::new();
        core.prune_registry(1_800_000_100.0, &mut out);
        assert!(!core.rooms.contains_key("idle"), "the idle room is pruned");
        assert!(
            core.rooms.contains_key("occupied"),
            "an occupied room is never pruned"
        );
        assert!(
            persist_ops(&out).iter().any(
                |op| matches!(op, db::HubRoomOp::Removed { room_name } if room_name == "idle")
            )
        );
        assert!(
            persist_ops(&out)
                .iter()
                .any(|op| matches!(op, db::HubRoomOp::GcInvites { .. })),
            "expired invites are collected on the same tick"
        );
    }

    #[test]
    fn a_restored_registry_rebuilds_rooms_and_drops_foreign_keys() {
        let row = db::HubRoomRow {
            room_name: "vault".into(),
            topic: "ops".into(),
            key_salt: hex::encode([0x11; 16]),
            key_mac: hex::encode([0x22; 32]),
            // Written under a different hub identity.
            key_pepper_id: hex::encode([0x99; 8]),
            moderated: true,
            invite_only: true,
            topic_ops_only: true,
            no_outside_msgs: true,
            private: true,
            last_used: 1_800_000_000.0,
            grants: vec![
                ("op".into(), hex::encode(ID_B), 0.0),
                ("ban".into(), hex::encode([0xCC; 16]), 0.0),
                ("invite".into(), hex::encode([0xDD; 16]), 0.0),
            ],
        };
        let bad = db::HubRoomRow {
            room_name: "NotNormalized".into(),
            ..row.clone()
        };
        let core = HubCore::new(
            ChannelHubConfig::default(),
            [0x77; 16],
            Arc::new(RwLock::new(HashSet::new())),
            Zeroizing::new([0x5A; 32]),
            vec![row, bad],
        );

        let room = core.rooms.get("vault").expect("the room is restored");
        assert!(room.registered && room.private && room.invite_only);
        assert!(room.members.is_empty(), "membership never persists");
        assert!(room.ops.contains(&ID_B) && room.bans.contains(&[0xCC; 16]));
        assert!(room.invited.is_empty(), "an expired invite is not restored");
        // The key was written under another hub identity, so it is unusable
        // and must be dropped rather than left permanently unmatchable.
        assert!(room.key.is_none());
        assert!(
            core.pepper_rotation_notice.is_some(),
            "the operator is told"
        );
        assert!(
            !core.rooms.contains_key("NotNormalized"),
            "a row that no longer normalizes is skipped"
        );
    }

    #[test]
    fn join_key_verifies_against_the_digest_and_is_never_stored() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "vault");
        run_command(&mut core, LINK_A, ID_A, "/mode vault +k open-sesame");

        let digest = core.rooms.get("vault").unwrap().key.clone().unwrap();
        // The plaintext is not recoverable from anything we keep.
        let stored = format!("{digest:?}");
        assert!(!stored.contains("open-sesame"));
        assert_eq!(stored, "RoomKeyDigest(redacted)");

        assert!(room_key_matches(
            &[0x5A; 32],
            "vault",
            "open-sesame",
            &digest
        ));
        assert!(!room_key_matches(
            &[0x5A; 32],
            "vault",
            "open-sesamf",
            &digest
        ));
        // The room name is bound into the preimage, so a digest cannot be
        // lifted from one room to another.
        assert!(!room_key_matches(
            &[0x5A; 32],
            "lobby",
            "open-sesame",
            &digest
        ));
        // A different hub identity cannot verify it either.
        assert!(!room_key_matches(
            &[0x11; 32],
            "vault",
            "open-sesame",
            &digest
        ));

        // And the gate accepts the real key while refusing a wrong one.
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        let mut envelope = rrc::Envelope::new(rrc::MessageType::Join, ID_B);
        envelope.room = Some("vault".to_string());
        envelope.body = Some(Value::Text("nope".to_string()));
        let mut out = Vec::new();
        core.on_envelope(LINK_B, envelope, &mut out);
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("bad key (+k)")
        );

        let mut envelope = rrc::Envelope::new(rrc::MessageType::Join, ID_B);
        envelope.room = Some("vault".to_string());
        envelope.body = Some(Value::Text("open-sesame".to_string()));
        let mut out = Vec::new();
        core.on_envelope(LINK_B, envelope, &mut out);
        assert_eq!(
            sends_to(&out, LINK_B)[0].message_type,
            rrc::MessageType::Joined
        );
    }

    #[test]
    fn weak_or_unmatchable_keys_are_refused_at_the_setter() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "vault");

        // The JOIN gate reads the body verbatim while the setter collapses
        // whitespace, so a spaced key could never be matched.
        let out = run_command(&mut core, LINK_A, ID_A, "/mode vault +k two words");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("key must not contain spaces")
        );
        assert!(core.rooms.get("vault").unwrap().key.is_none());

        let out = run_command(&mut core, LINK_A, ID_A, "/mode vault +k short");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("key must be at least 8 bytes")
        );
        assert!(core.rooms.get("vault").unwrap().key.is_none());

        // -k clears it again.
        run_command(&mut core, LINK_A, ID_A, "/mode vault +k longenough");
        assert!(core.rooms.get("vault").unwrap().key.is_some());
        run_command(&mut core, LINK_A, ID_A, "/mode vault -k");
        assert!(core.rooms.get("vault").unwrap().key.is_none());
    }

    #[test]
    fn the_key_never_appears_in_any_reply_or_mode_string() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "vault");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        run_command(&mut core, LINK_A, ID_A, "/mode vault +k open-sesame");

        assert_eq!(core.rooms.get("vault").unwrap().mode_string(), "+k");

        // Every reply that mentions the room must expose the flag only.
        let mut rendered = String::new();
        for command in [
            "/mode vault +i",
            "/topic vault",
            "/who vault",
            "/list",
            "/stats",
            "/invite vault add beta",
            "/ban vault list",
        ] {
            for send in run_command(&mut core, LINK_A, ID_A, command) {
                if let HubSend::Envelope { envelope, .. } = send
                    && let Some(text) = rrc::text_body(&envelope)
                {
                    rendered.push_str(text);
                    rendered.push('\n');
                }
            }
        }
        // The room-status notice on join is the other room-describing reply.
        for send in join(&mut core, LINK_B, ID_B, "vault") {
            if let HubSend::Envelope { envelope, .. } = send
                && let Some(text) = rrc::text_body(&envelope)
            {
                rendered.push_str(text);
                rendered.push('\n');
            }
        }
        assert!(
            !rendered.contains("open-sesame"),
            "a room key leaked into a reply: {rendered}"
        );
        assert!(rendered.contains("mode=+ik") || rendered.contains("+ik"));
    }

    #[test]
    fn join_of_an_unknown_room_is_refused_for_non_operators() {
        let mut core = op_core();
        let out = join(&mut core, LINK_B, ID_B, "ghost");
        let error = sends_to(&out, LINK_B)[0];
        assert_eq!(error.message_type, rrc::MessageType::Error);
        assert_eq!(rrc::text_body(error), Some("no such room"));
        // NomadNet rolls a pending join back only when the ERROR names the
        // room, and it matches on the normalized name.
        assert_eq!(error.room.as_deref(), Some("ghost"));
        assert!(
            core.rooms.is_empty(),
            "a refused join must not create a room"
        );
    }

    #[test]
    fn operator_join_creates_the_room_and_others_may_then_join() {
        let mut core = op_core();
        let out = join(&mut core, LINK_A, ID_A, " Lobby ");
        assert_eq!(
            sends_to(&out, LINK_A)[0].message_type,
            rrc::MessageType::Joined
        );
        assert!(core.rooms.contains_key("lobby"));
        assert!(core.rooms.get("lobby").unwrap().ops.contains(&ID_A));

        let out = join(&mut core, LINK_B, ID_B, "lobby");
        assert_eq!(
            sends_to(&out, LINK_B)[0].message_type,
            rrc::MessageType::Joined
        );
    }

    #[test]
    fn commands_never_create_rooms() {
        // Every command that touches a room used to call entry().or_default()
        // before authorizing, so any peer could grow the hub without limit.
        let mut core = op_core();
        for command in [
            "/topic ghost",
            "/topic ghost a new topic",
            "/mode ghost +m",
            "/op ghost beta",
            "/voice ghost beta",
            "/ban ghost add beta",
            "/invite ghost add beta",
        ] {
            let out = run_command(&mut core, LINK_B, ID_B, command);
            assert_eq!(
                rrc::text_body(sends_to(&out, LINK_B)[0]),
                Some("no such room"),
                "{command} must not create a room"
            );
            assert!(core.rooms.is_empty(), "{command} created a room");
        }

        // The operator is refused too: creation is a JOIN/register concern.
        for command in ["/topic ghost hi", "/mode ghost +m"] {
            let out = run_command(&mut core, LINK_A, ID_A, command);
            assert_eq!(
                rrc::text_body(sends_to(&out, LINK_A)[0]),
                Some("no such room")
            );
            assert!(core.rooms.is_empty());
        }
    }

    #[test]
    fn invite_grants_are_recorded_only_for_gated_rooms() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "open");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");

        // An ungated room needs no invite, so no grant row is kept.
        let out = run_command(&mut core, LINK_A, ID_A, "/invite open add beta");
        assert!(core.rooms.get("open").unwrap().invited.is_empty());
        assert!(
            sends_to(&out, LINK_A)
                .iter()
                .any(|env| rrc::text_body(env) == Some("invite sent to beta for open"))
        );

        // Gating the room makes the invite meaningful, and it is recorded.
        run_command(&mut core, LINK_A, ID_A, "/mode open +i");
        run_command(&mut core, LINK_A, ID_A, "/invite open add beta");
        assert!(core.rooms.get("open").unwrap().invited.contains_key(&ID_B));
    }

    #[test]
    fn register_forces_modes_and_requires_a_server_operator() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");
        // ID_B is a plain member, not a server operator.
        let out = run_command(&mut core, LINK_B, ID_B, "/register lobby");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("only a server operator can register")
        );

        let out = run_command(&mut core, LINK_A, ID_A, "/register lobby");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("registered room lobby")
        );
        let room = core.rooms.get("lobby").unwrap();
        assert!(room.registered && room.no_outside_msgs && room.topic_ops_only);
        assert_eq!(room.mode_string(), "+nrt");
    }

    #[test]
    fn mode_changes_broadcast_and_reject_unauthorized() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");

        // ID_A is a server operator, so it is an implicit op in every room.
        let out = run_command(&mut core, LINK_A, ID_A, "/mode lobby +m");
        assert_eq!(sends_to(&out, LINK_A).len(), 1);
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("mode for lobby is now: +m")
        );
        assert!(core.rooms.get("lobby").unwrap().moderated);

        let out = run_command(&mut core, LINK_A, ID_A, "/mode lobby +r");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("use /register or /unregister to change +r")
        );

        // +k sets a key; a later keyless join is refused.
        run_command(&mut core, LINK_A, ID_A, "/mode lobby +k s3cret99");
        let digest = core.rooms.get("lobby").unwrap().key.clone().unwrap();
        assert!(room_key_matches(&[0x5A; 32], "lobby", "s3cret99", &digest));
        assert!(!room_key_matches(
            &[0x5A; 32],
            "lobby",
            "wrong-key",
            &digest
        ));
    }

    #[test]
    fn op_voice_grants_and_a_server_operator_cannot_be_deopped() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");
        let out = run_command(&mut core, LINK_A, ID_A, "/op lobby beta");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("op granted in lobby")
        );
        assert!(core.rooms.get("lobby").unwrap().ops.contains(&ID_B));

        // A granted op can be removed...
        let out = run_command(&mut core, LINK_A, ID_A, "/deop lobby beta");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("op removed in lobby")
        );
        assert!(!core.rooms.get("lobby").unwrap().ops.contains(&ID_B));

        // ...but a server operator's authority is not room-scoped, so it
        // cannot be dropped from inside a room.
        let out = run_command(&mut core, LINK_A, ID_A, "/deop lobby alpha");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("cannot deop a server operator")
        );

        let out = run_command(&mut core, LINK_A, ID_A, "/voice lobby beta");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("voice granted in lobby")
        );
    }

    #[test]
    fn kick_and_ban_remove_members_with_parted_and_texts() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");

        let out = run_command(&mut core, LINK_A, ID_A, "/kick lobby beta");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("kicked from lobby")
        );
        // Deviation: remaining member ID_A sees a PARTED for the kicked peer.
        assert!(
            sends_to(&out, LINK_A)
                .iter()
                .any(|env| env.message_type == rrc::MessageType::Parted)
        );
        assert!(
            sends_to(&out, LINK_A)
                .iter()
                .any(|env| rrc::text_body(env) == Some("kicked beta from lobby"))
        );
        assert!(!core.rooms.get("lobby").unwrap().members.contains(&LINK_B));

        join(&mut core, LINK_B, ID_B, "lobby");
        let out = run_command(&mut core, LINK_A, ID_A, "/ban lobby add beta");
        assert!(
            sends_to(&out, LINK_B)
                .iter()
                .any(|env| rrc::text_body(env) == Some("banned from lobby"))
        );
        assert!(core.rooms.get("lobby").unwrap().bans.contains(&ID_B));
        assert!(
            sends_to(&out, LINK_A)
                .iter()
                .any(|env| rrc::text_body(env) == Some("ban added in lobby"))
        );
    }

    #[test]
    fn kline_disconnects_and_blocks_reconnect() {
        let mut core = op_core();
        let out = run_command(&mut core, LINK_A, ID_A, "/kline add beta");
        assert!(
            sends_to(&out, LINK_A).iter().any(|env| rrc::text_body(env)
                == Some(&format!("kline added for {}", hex::encode(ID_B))))
        );
        // ID_B's link is torn down and its session removed.
        assert!(
            out.iter()
                .any(|send| matches!(send, HubSend::Close { link_id } if *link_id == LINK_B))
        );
        assert!(!core.sessions.contains_key(&LINK_B));
        assert!(core.klines.read().unwrap().contains(&ID_B));

        // A fresh identify for the klined identity is rejected at the gate.
        let mut out = Vec::new();
        core.on_link_established(LINK_B, Instant::now());
        core.on_link_identified(LINK_B, ID_B, Instant::now(), &mut out);
        assert_eq!(rrc::text_body(sends_to(&out, LINK_B)[0]), Some("banned"));
    }

    #[test]
    fn invite_grants_bypass_and_targets_get_notified() {
        let config = ChannelHubConfig {
            server_operators: vec![ID_A],
            invite_timeout_secs: 900,
            ..ChannelHubConfig::default()
        };
        let mut core = core_with(config);
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        join(&mut core, LINK_A, ID_A, "lobby");
        run_command(&mut core, LINK_A, ID_A, "/mode lobby +i");

        let out = run_command(&mut core, LINK_A, ID_A, "/invite lobby add beta");
        assert!(
            sends_to(&out, LINK_B)
                .iter()
                .any(|env| rrc::text_body(env) == Some("You have been invited to join lobby."))
        );
        assert!(sends_to(&out, LINK_A).iter().any(|env| {
            rrc::text_body(env)
                .is_some_and(|body| body.starts_with("invite added in lobby (expires in 900s)"))
        }));
        assert!(core.rooms.get("lobby").unwrap().invited.contains_key(&ID_B));
    }

    #[test]
    fn stats_and_reload_require_server_op() {
        let mut core = op_core();
        let out = run_command(&mut core, LINK_B, ID_B, "/stats");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_B)[0]),
            Some("not authorized")
        );

        let out = run_command(&mut core, LINK_A, ID_A, "/stats");
        let stats = rrc::text_body(sends_to(&out, LINK_A)[0]).unwrap();
        assert!(stats.contains("sessions: 2"));
        assert!(stats.contains("rooms:"));

        let out = run_command(&mut core, LINK_A, ID_A, "/reload");
        assert!(
            rrc::text_body(sends_to(&out, LINK_A)[0])
                .is_some_and(|body| body.contains("Ratspeak app settings"))
        );
    }

    #[test]
    fn target_resolution_disambiguates_and_reports_missing() {
        let mut core = op_core();
        // The room must exist and the caller be an op before target resolution
        // runs; ID_A is a server op, so joining suffices.
        join(&mut core, LINK_A, ID_A, "lobby");
        // Two sessions share the "dup" nick; a nick lookup is ambiguous.
        let link_c = [0x04; 16];
        welcomed_session(&mut core, link_c, [0xCC; 16], "dup");
        core.sessions.get_mut(&LINK_B).unwrap().nickname = Some("dup".to_string());
        core.by_identity.insert([0xCC; 16], link_c);

        let out = run_command(&mut core, LINK_A, ID_A, "/kick lobby dup");
        assert!(
            rrc::text_body(sends_to(&out, LINK_A)[0])
                .is_some_and(|body| body.starts_with("ambiguous: 'dup' matches 2 identities:"))
        );

        let out = run_command(&mut core, LINK_A, ID_A, "/kick lobby ghost");
        assert_eq!(
            rrc::text_body(sends_to(&out, LINK_A)[0]),
            Some("target 'ghost' not found")
        );
    }

    #[test]
    fn rate_bucket_and_dedup_guard_the_relay() {
        let config = ChannelHubConfig {
            rate_messages_per_minute: 2,
            ..ChannelHubConfig::default()
        };
        let mut core = core_with(config);
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        let now = Instant::now();
        // The HELLO already consumed nothing (note_packet is shell-driven);
        // two packets pass, the third is refused until refill.
        assert!(core.note_packet(LINK_A, now));
        assert!(core.note_packet(LINK_A, now));
        assert!(!core.note_packet(LINK_A, now));
        assert!(core.note_packet(LINK_A, now + Duration::from_secs(31)));

        join(&mut core, LINK_A, ID_A, "lobby");
        let mut msg = rrc::Envelope::new(rrc::MessageType::Message, ID_A);
        msg.room = Some("lobby".to_string());
        msg.body = Some(Value::Text("once".to_string()));
        let mut out = Vec::new();
        core.on_envelope(LINK_A, msg.clone(), &mut out);
        assert_eq!(sends_to(&out, LINK_A).len(), 1);
        let mut out = Vec::new();
        core.on_envelope(LINK_A, msg, &mut out);
        assert!(out.is_empty(), "replayed envelope ids never fan out twice");
    }
}
