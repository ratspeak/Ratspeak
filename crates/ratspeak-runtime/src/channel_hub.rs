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
//! Every reply is sized to one link packet. The greeting alone may exceed
//! that as a resource, and only to a peer that advertised the capability:
//! command replies are parsed per packet by the reference clients, so a
//! resource-delivered one is silently ignored.
//!
//! Inbound resources are off by default and, when enabled, accept exactly one
//! thing: an announced `notice` payload for a room the sender may already
//! relay into. It passes the same gates a packet would, is never dispatched as
//! a command, and its size is checked three times — at the envelope, at the
//! accept closure, and against the bytes actually delivered.
//!
//! Protocol behavior follows the reference daemon (kc1awv/rrcd 0.3.2) except
//! where the fix registry records a deliberate deviation (idempotent re-HELLO
//! is one: a duplicate HELLO re-welcomes without wiping room membership).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, Instant};

use ciborium::value::Value;
use rns_identity::identity::Identity;
use rns_link::link::{CloseReason, ResourceStrategy};
use rns_protocol::resource_adv::ResourceAdvertisement;
use rns_runtime::destination_runtime::{
    DestinationRuntimeOptions, IdentityGatePolicy, RegisteredDestination, ResourceAcceptPolicy,
};
use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::link_manager::{
    DestinationAnnounceOptions, LinkResourceDirection, LinkResourceEvent,
};
use rns_transport::messages::TransportMessage;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use crate::activity::{CorrelationId, producer as activity};
use crate::channels::ChannelsActivity;
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
/// Free a link's outbound resource slot when a transfer never concludes.
/// Longer than the 30s expectation TTL both reference clients apply, so a
/// transfer the peer has already given up on cannot hold the slot.
const OUTBOUND_RESOURCE_TIMEOUT: Duration = Duration::from_secs(40);
const RESOURCE_CYCLE_INTERVAL_SECS: u64 = 10;
/// How long an announced inbound payload stays claimable (reference parity).
const RESOURCE_EXPECTATION_TTL: Duration = Duration::from_secs(30);
/// Advertisements one link may have outstanding at once (reference parity).
const MAX_PENDING_EXPECTATIONS: usize = 8;
/// Concurrent inbound transfers per link. Each one costs reassembly memory
/// bounded by `max_resource_notice_bytes`, so this is the amplification cap.
const MAX_INBOUND_RESOURCES_PER_LINK: usize = 4;
/// Backstop for an inbound transfer that never concludes on either channel.
const INBOUND_RESOURCE_TIMEOUT: Duration = Duration::from_secs(300);
/// The only inbound resource kind we accept. The reference also takes `motd`
/// and `blob`, then discards both: amplification with no interop value.
const RES_KIND_NOTICE: &str = "notice";
/// Throttle/drop counters are reported as one aggregate at this cadence, never
/// per rejected packet: a flooding peer must not be able to drive the event bus.
const THROTTLE_REPORT_INTERVAL: Duration = Duration::from_secs(60);
const HUB_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const DEFAULT_HUB_NAME: &str = "Ratspeak hub";
pub const DEFAULT_PING_INTERVAL_SECS: u64 = 55;
pub const DEFAULT_PING_TIMEOUT_SECS: u64 = 120;
pub const CHANNEL_HUB_SETTING_KEYS: [&str; 6] = [
    "channel_hub_enabled",
    "channel_hub_name",
    "channel_hub_greeting",
    "channel_hub_announce_interval",
    "channel_hub_resource_send",
    "channel_hub_resource_accept",
];

/// Operator-editable hub settings. This is deliberately separate from the
/// live snapshot: saved configuration must remain readable while the network
/// and hub actor are stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelHubSettings {
    pub enabled: bool,
    pub hub_name: String,
    pub greeting: String,
    pub announce_interval_secs: u64,
    pub resource_send_enabled: bool,
    pub resource_accept_enabled: bool,
}

impl Default for ChannelHubSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            hub_name: DEFAULT_HUB_NAME.to_string(),
            greeting: String::new(),
            announce_interval_secs: 0,
            resource_send_enabled: true,
            resource_accept_enabled: false,
        }
    }
}

impl ChannelHubSettings {
    pub fn load(pool: &db::DbPool) -> Result<Self, String> {
        let values = db::get_settings(pool, &CHANNEL_HUB_SETTING_KEYS)?;
        let defaults = Self::default();
        let hub_name = values
            .get("channel_hub_name")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or(&defaults.hub_name)
            .to_string();
        let announce_interval_secs = values
            .get("channel_hub_announce_interval")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value == 0 || (300..=86_400).contains(value))
            .unwrap_or(defaults.announce_interval_secs);

        Ok(Self {
            enabled: values
                .get("channel_hub_enabled")
                .is_some_and(|value| value.trim() == "1"),
            hub_name,
            greeting: values
                .get("channel_hub_greeting")
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            announce_interval_secs,
            resource_send_enabled: values
                .get("channel_hub_resource_send")
                .map(|value| value.trim() == "1")
                .unwrap_or(defaults.resource_send_enabled),
            resource_accept_enabled: values
                .get("channel_hub_resource_accept")
                .map(|value| value.trim() == "1")
                .unwrap_or(defaults.resource_accept_enabled),
        })
    }

    pub fn setting_rows(&self) -> Vec<(String, String)> {
        vec![
            (
                "channel_hub_enabled".to_string(),
                bool_setting(self.enabled),
            ),
            ("channel_hub_name".to_string(), self.hub_name.clone()),
            ("channel_hub_greeting".to_string(), self.greeting.clone()),
            (
                "channel_hub_announce_interval".to_string(),
                self.announce_interval_secs.to_string(),
            ),
            (
                "channel_hub_resource_send".to_string(),
                bool_setting(self.resource_send_enabled),
            ),
            (
                "channel_hub_resource_accept".to_string(),
                bool_setting(self.resource_accept_enabled),
            ),
        ]
    }

    pub fn runtime_config(&self) -> ChannelHubConfig {
        ChannelHubConfig {
            hub_name: self.hub_name.clone(),
            greeting: (!self.greeting.is_empty()).then(|| self.greeting.clone()),
            announce_interval_secs: self.announce_interval_secs,
            resource_send_enabled: self.resource_send_enabled,
            resource_accept_enabled: self.resource_accept_enabled,
            ..ChannelHubConfig::default()
        }
    }
}

fn bool_setting(enabled: bool) -> String {
    if enabled { "1" } else { "0" }.to_string()
}

/// Hosting is a desktop service. Keep this runtime boundary even when the
/// frontend omits the controls so mobile builds cannot be enabled through a
/// stale setting or direct IPC call.
pub const fn channel_hub_hosting_supported() -> bool {
    !cfg!(any(target_os = "android", target_os = "ios"))
}

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
    /// Advertise `CAP_RESOURCE_ENVELOPE` and send an oversized greeting as a
    /// resource. Accepting inbound resources is a separate switch: the two
    /// directions carry very different risk, so they are not one flag.
    pub resource_send_enabled: bool,
    pub resource_accept_enabled: bool,
    /// Trigger ceiling for the outbound resource path. Both reference clients
    /// expire the expectation 30s after the advertisement, so a payload that
    /// cannot conclude inside that window is better chunked than advertised.
    pub max_outbound_resource_bytes: usize,
    /// Inbound ceiling for a resource-delivered NOTICE. A capable client can
    /// inject this much text into a room, fanned to every member, so the
    /// amplification grows linearly with room size if it is raised.
    pub max_resource_notice_bytes: usize,
    /// Absolute protocol ceiling for a resource in either direction.
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
            resource_send_enabled: true,
            resource_accept_enabled: false,
            max_outbound_resource_bytes: 16 * 1024,
            max_resource_notice_bytes: 4096,
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
    /// Inbound resource payloads that survived every gate and fanned out.
    resources_received: u64,
    resource_bytes_received: u64,
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

/// A payload a link has announced but not yet delivered.
#[derive(Clone)]
struct InboundExpectation {
    id: [u8; 8],
    /// The announced byte count. A claim, not evidence: it buys a
    /// matching-size transfer slot and nothing more.
    size: usize,
    sha256: Option<[u8; 32]>,
    encoding: Option<String>,
    /// Normalized room the payload will be relayed into.
    room: String,
    created_at: Instant,
}

#[derive(Default)]
struct AdmissionState {
    enabled: bool,
    max_bytes: usize,
    expectations: HashMap<[u8; 16], Vec<InboundExpectation>>,
    /// Admitted transfers per link, timestamped so a wedged one is swept.
    live: HashMap<[u8; 16], HashMap<[u8; 32], Instant>>,
}

impl AdmissionState {
    fn prune(&mut self, now: Instant) {
        self.expectations.retain(|_, pending| {
            pending.retain(|exp| now.duration_since(exp.created_at) <= RESOURCE_EXPECTATION_TTL);
            !pending.is_empty()
        });
        self.live.retain(|_, live| {
            live.retain(|_, started| now.duration_since(*started) <= INBOUND_RESOURCE_TIMEOUT);
            !live.is_empty()
        });
    }
}

/// Inbound resource gate, shared with the LinkManager task.
///
/// `admit` is called synchronously inside that task for every advertisement,
/// so each method takes the lock for a couple of lookups and one insert and
/// releases it; nothing here is ever held across an await. A poisoned lock
/// fails closed — rejecting is always the safe answer.
pub(crate) struct ResourceAdmission {
    state: RwLock<AdmissionState>,
    /// The closure cannot reach `HubStats`, so the counter lives here and
    /// `/stats` reads it.
    rejected: AtomicU64,
}

impl ResourceAdmission {
    fn new(enabled: bool, max_bytes: usize) -> Self {
        Self {
            state: RwLock::new(AdmissionState {
                enabled,
                max_bytes,
                ..AdmissionState::default()
            }),
            rejected: AtomicU64::new(0),
        }
    }

    /// Record an announced payload. False means the link's pending budget is
    /// full, which the caller answers with the reference ERROR.
    fn expect(&self, link_id: [u8; 16], expectation: InboundExpectation) -> bool {
        let Ok(mut state) = self.state.write() else {
            return false;
        };
        let now = expectation.created_at;
        state.prune(now);
        let pending = state.expectations.entry(link_id).or_default();
        // A repeat announcement of the same id refreshes rather than stacks,
        // so a retrying client cannot exhaust its own budget.
        if let Some(existing) = pending.iter_mut().find(|exp| exp.id == expectation.id) {
            *existing = expectation;
            return true;
        }
        if pending.len() >= MAX_PENDING_EXPECTATIONS {
            return false;
        }
        pending.push(expectation);
        true
    }

    /// The LinkManager-side gate. Order is load-bearing: every cheap structural
    /// refusal runs before the expectation lookup, and the live-set check runs
    /// last so a re-advertisement of an in-flight transfer is never a new slot.
    fn admit(&self, link_id: [u8; 16], adv: &ResourceAdvertisement) -> bool {
        let admitted = self.admit_inner(link_id, adv);
        if !admitted {
            self.rejected.fetch_add(1, Ordering::Relaxed);
        }
        admitted
    }

    fn admit_inner(&self, link_id: [u8; 16], adv: &ResourceAdvertisement) -> bool {
        let Ok(mut state) = self.state.write() else {
            return false;
        };
        if !state.enabled {
            return false;
        }
        // A split transfer announces one segment at a time, so no segment's
        // size matches the announced payload and reassembly is not bounded by
        // our per-advertisement cap.
        if adv.total_segments > 1 || adv.flags.split {
            return false;
        }
        // `total_size = data_size + metadata_size`: a metadata prefix shifts
        // the byte count the expectation matches on, and we never ask for any.
        if adv.flags.has_metadata || adv.metadata_size > 0 {
            return false;
        }
        if adv.data_size == 0 || adv.data_size > state.max_bytes {
            return false;
        }
        let now = Instant::now();
        // Consulted per segment: an already-admitted transfer stays admitted
        // even once its expectation has aged out.
        if state
            .live
            .get(&link_id)
            .is_some_and(|live| live.contains_key(&adv.resource_hash))
        {
            return true;
        }
        state.prune(now);
        let announced = state
            .expectations
            .get(&link_id)
            .is_some_and(|pending| pending.iter().any(|exp| exp.size == adv.data_size));
        if !announced {
            return false;
        }
        let live = state.live.entry(link_id).or_default();
        if live.len() >= MAX_INBOUND_RESOURCES_PER_LINK {
            return false;
        }
        live.insert(adv.resource_hash, now);
        true
    }

    /// Claim the expectation a delivered payload satisfies. Matching is by
    /// real byte count and, when one was announced, real digest.
    fn take_matching(
        &self,
        link_id: [u8; 16],
        size: usize,
        digest: [u8; 32],
    ) -> Option<InboundExpectation> {
        let mut state = self.state.write().ok()?;
        state.prune(Instant::now());
        let pending = state.expectations.get_mut(&link_id)?;
        let index = pending
            .iter()
            .position(|exp| exp.size == size && exp.sha256.is_none_or(|want| want == digest))?;
        Some(pending.remove(index))
    }

    /// Release a live slot. Idempotent: both conclusion channels call it.
    fn retire(&self, link_id: [u8; 16], resource_hash: [u8; 32]) {
        // Recover a poisoned lock here: admission stays fail-closed, but
        // refusing to clean up would strand slots for the hub's lifetime.
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let emptied = match state.live.get_mut(&link_id) {
            Some(live) => {
                live.remove(&resource_hash);
                live.is_empty()
            }
            None => false,
        };
        if emptied {
            state.live.remove(&link_id);
        }
    }

    fn forget_link(&self, link_id: [u8; 16]) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.expectations.remove(&link_id);
        state.live.remove(&link_id);
    }

    fn sweep(&self, now: Instant) {
        self.state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .prune(now);
    }

    fn note_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn pending_count(&self, link_id: [u8; 16]) -> usize {
        self.state
            .read()
            .map(|state| {
                state
                    .expectations
                    .get(&link_id)
                    .map_or(0, |pending| pending.len())
            })
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn live_count(&self, link_id: [u8; 16]) -> usize {
        self.state
            .read()
            .map(|state| state.live.get(&link_id).map_or(0, HashMap::len))
            .unwrap_or(0)
    }
}

/// Effective inbound ceiling: the notice cap, never above the absolute
/// protocol ceiling.
fn inbound_resource_cap(config: &ChannelHubConfig) -> usize {
    config
        .max_resource_notice_bytes
        .min(usize::try_from(config.max_resource_bytes).unwrap_or(usize::MAX))
}

/// Reference ERROR text for a rejected advertisement body. Our codec folds the
/// reference's per-field checks into typed errors; this unfolds them again so
/// a client sees the same string it would from rrcd.
fn resource_envelope_error(error: &rrc::ProtocolError) -> &'static str {
    match error {
        rrc::ProtocolError::MissingField(rrc::RESOURCE_ID)
        | rrc::ProtocolError::InvalidField(rrc::RESOURCE_ID) => "resource envelope missing id",
        rrc::ProtocolError::MissingField(rrc::RESOURCE_KIND)
        | rrc::ProtocolError::InvalidField(rrc::RESOURCE_KIND) => "resource envelope missing kind",
        rrc::ProtocolError::MissingField(rrc::RESOURCE_SIZE)
        | rrc::ProtocolError::InvalidField(rrc::RESOURCE_SIZE) => "resource envelope invalid size",
        rrc::ProtocolError::MissingField(rrc::RESOURCE_SHA256)
        | rrc::ProtocolError::InvalidField(rrc::RESOURCE_SHA256) => {
            "resource envelope invalid sha256"
        }
        _ => "invalid resource envelope body",
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
    /// At most one outbound transfer per link: both reference clients bind an
    /// arriving resource to a pending advertisement by size alone, so two
    /// concurrent same-size transfers cross-match and mislabel each other.
    outbound_resource: Option<OutboundResource>,
    /// Ties every Activity event about this link to one session.
    correlation: CorrelationId,
}

struct OutboundResource {
    /// None until the transport reports one; the slot is held either way.
    resource_hash: Option<[u8; 32]>,
    started_at: Instant,
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
            outbound_resource: None,
            correlation: CorrelationId::random(),
        }
    }

    /// Whether the peer advertised resource support in its HELLO. No client
    /// name sniffing: our own client does not advertise it and drops type 50,
    /// so a resource sent there stalls until the transport cancels it.
    fn supports_resources(&self) -> bool {
        self.capabilities
            .get(&rrc::CAP_RESOURCE_ENVELOPE)
            .copied()
            .unwrap_or(false)
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

/// Resource payload bytes. Hand-written `Debug`: `HubSend` derives one, and a
/// derived field would spill the whole body into any log that renders a send.
pub(crate) struct ResourcePayload(pub Vec<u8>);

impl std::fmt::Debug for ResourcePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ResourcePayload({} bytes)", self.0.len())
    }
}

/// Hub-side sends the pure core asks the shell to perform.
#[derive(Debug)]
pub(crate) enum HubSend {
    Envelope {
        link_id: [u8; 16],
        envelope: rrc::Envelope,
    },
    /// An advertisement packet plus the bytes it announces. The shell sends
    /// the envelope first and the resource second, never `send_link_payload`:
    /// its implicit size switch is not the envelope/resource pairing.
    Resource {
        link_id: [u8; 16],
        envelope: rrc::Envelope,
        payload: ResourcePayload,
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

/// Activity intents the shell drains and records. Deliberately no `Debug` and
/// deliberately no `String`: rooms are opaque tokens, peers are hashes bound
/// for the pseudonym boundary, and nicknames, topics, keys and message bodies
/// have no representation here at all.
pub(crate) enum HubEvent {
    ServiceStarted {
        correlation: CorrelationId,
    },
    ServiceStopped {
        correlation: CorrelationId,
    },
    ServiceDegraded {
        correlation: CorrelationId,
        reason: activity::HubServiceDegradation,
        count: u64,
    },
    SessionOpened {
        correlation: CorrelationId,
        link: [u8; 16],
        peer: [u8; 16],
    },
    SessionRejected {
        correlation: CorrelationId,
        link: [u8; 16],
        reason: activity::HubSessionRejection,
    },
    SessionClosed {
        correlation: CorrelationId,
        link: [u8; 16],
        reason: activity::HubSessionCloseReason,
        duration_ms: u64,
    },
    RoomJoined {
        correlation: CorrelationId,
        link: [u8; 16],
        room: activity::ChannelRoomToken,
        members: u64,
    },
    RoomParted {
        correlation: CorrelationId,
        link: [u8; 16],
        room: activity::ChannelRoomToken,
        members: u64,
    },
    RoomModerated {
        correlation: CorrelationId,
        link: [u8; 16],
        room: activity::ChannelRoomToken,
        action: activity::HubModerationAction,
    },
    TrustChanged {
        correlation: CorrelationId,
        link: [u8; 16],
        change: activity::HubTrustChange,
    },
    RelayForwarded {
        correlation: CorrelationId,
        room: activity::ChannelRoomToken,
        method: activity::ChannelEnvelopeKind,
        encoded_bytes: u64,
        recipients: u64,
    },
    RelayThrottled {
        correlation: CorrelationId,
        rejected: u64,
        dropped: u64,
        span_ms: u64,
    },
}

/// Lower a hub event onto the sealed Activity catalog. The correlation rides
/// with the event because the session that owns it may already be gone by the
/// time the shell drains.
fn hub_activity_transition(event: HubEvent) -> (CorrelationId, activity::HubTransition) {
    match event {
        HubEvent::ServiceStarted { correlation } => {
            (correlation, activity::HubTransition::ServiceStarted)
        }
        HubEvent::ServiceStopped { correlation } => {
            (correlation, activity::HubTransition::ServiceStopped)
        }
        HubEvent::ServiceDegraded {
            correlation,
            reason,
            count,
        } => (
            correlation,
            activity::HubTransition::ServiceDegraded { reason, count },
        ),
        HubEvent::SessionOpened {
            correlation,
            link,
            peer,
        } => (
            correlation,
            activity::HubTransition::SessionOpened {
                link: activity::LinkId::new(link),
                peer: activity::IdentityHash::new(peer),
            },
        ),
        HubEvent::SessionRejected {
            correlation,
            link,
            reason,
        } => (
            correlation,
            activity::HubTransition::SessionRejected {
                link: activity::LinkId::new(link),
                reason,
            },
        ),
        HubEvent::SessionClosed {
            correlation,
            link,
            reason,
            duration_ms,
        } => (
            correlation,
            activity::HubTransition::SessionClosed {
                link: activity::LinkId::new(link),
                reason,
                duration_ms,
            },
        ),
        HubEvent::RoomJoined {
            correlation,
            link,
            room,
            members,
        } => (
            correlation,
            activity::HubTransition::RoomJoined {
                link: activity::LinkId::new(link),
                room,
                members,
            },
        ),
        HubEvent::RoomParted {
            correlation,
            link,
            room,
            members,
        } => (
            correlation,
            activity::HubTransition::RoomParted {
                link: activity::LinkId::new(link),
                room,
                members,
            },
        ),
        HubEvent::RoomModerated {
            correlation,
            link,
            room,
            action,
        } => (
            correlation,
            activity::HubTransition::RoomModerated {
                link: activity::LinkId::new(link),
                room,
                action,
            },
        ),
        HubEvent::TrustChanged {
            correlation,
            link,
            change,
        } => (
            correlation,
            activity::HubTransition::TrustChanged {
                link: activity::LinkId::new(link),
                change,
            },
        ),
        HubEvent::RelayForwarded {
            correlation,
            room,
            method,
            encoded_bytes,
            recipients,
        } => (
            correlation,
            activity::HubTransition::RelayForwarded {
                room,
                method,
                encoded_bytes,
                recipients,
            },
        ),
        HubEvent::RelayThrottled {
            correlation,
            rejected,
            dropped,
            span_ms,
        } => (
            correlation,
            activity::HubTransition::RelayThrottled {
                rejected,
                dropped,
                span_ms,
            },
        ),
    }
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
    /// Shared with the LinkManager's accept closure; see `ResourceAdmission`.
    admission: Arc<ResourceAdmission>,
    /// Verify-only material for room join keys; never persisted.
    pepper: Zeroizing<[u8; 32]>,
    pepper_id: [u8; 8],
    started_wall: f64,
    klines_dirty: bool,
    pending_removals: HashSet<String>,
    pepper_rotation_notice: Option<String>,
    stats: HubStats,
    started_at: Instant,
    /// Opaque per-room Activity tokens, minted on demand and dropped with the
    /// room. Never derived from the room label.
    room_tokens: HashMap<String, activity::ChannelRoomToken>,
    /// Correlation for events that belong to the hub run rather than a session.
    hub_correlation: CorrelationId,
    events: Vec<HubEvent>,
    throttle_reported_at: Instant,
    throttle_baseline: (u64, u64),
}

impl HubCore {
    pub(crate) fn new(
        config: ChannelHubConfig,
        hub_hash: [u8; 16],
        klines: Arc<RwLock<HashSet<[u8; 16]>>>,
        admission: Arc<ResourceAdmission>,
        pepper: Zeroizing<[u8; 32]>,
        restored: Vec<db::HubRoomRow>,
    ) -> Self {
        if let Ok(mut set) = klines.write() {
            set.extend(config.banned_identities.iter().copied());
        }
        let server_ops = config.server_operators.iter().copied().collect();
        let mut pepper_id = [0u8; 8];
        pepper_id.copy_from_slice(&hub_hash[..8]);
        let started_at = Instant::now();
        let mut core = Self {
            config,
            hub_hash,
            sessions: HashMap::new(),
            rooms: HashMap::new(),
            by_identity: HashMap::new(),
            server_ops,
            klines,
            admission,
            pepper,
            pepper_id,
            started_wall: now_unix(),
            klines_dirty: false,
            pending_removals: HashSet::new(),
            pepper_rotation_notice: None,
            stats: HubStats::default(),
            started_at,
            room_tokens: HashMap::new(),
            hub_correlation: CorrelationId::random(),
            events: Vec::new(),
            throttle_reported_at: started_at,
            throttle_baseline: (0, 0),
        };
        core.restore(restored);
        core
    }

    /// Drain Activity intents for the shell to record. Kept off the `HubSend`
    /// path so `HubSend` can keep its derived `Debug` and the eight
    /// `out: &mut Vec<HubSend>` signatures stay untouched.
    pub(crate) fn drain_events(&mut self) -> Vec<HubEvent> {
        std::mem::take(&mut self.events)
    }

    /// Opaque token for a room, stable while the room exists.
    fn room_token(&mut self, room_name: &str) -> activity::ChannelRoomToken {
        if let Some(token) = self.room_tokens.get(room_name) {
            return *token;
        }
        let token = activity::ChannelRoomToken::random();
        self.room_tokens.insert(room_name.to_string(), token);
        token
    }

    /// Correlation for anything scoped to one link, falling back to the hub
    /// run so an event is never lost because its session already went away.
    fn link_correlation(&self, link_id: [u8; 16]) -> CorrelationId {
        self.sessions
            .get(&link_id)
            .map(|session| session.correlation)
            .unwrap_or(self.hub_correlation)
    }

    fn note_moderated(
        &mut self,
        link_id: [u8; 16],
        room_name: &str,
        action: activity::HubModerationAction,
    ) {
        let correlation = self.link_correlation(link_id);
        let room = self.room_token(room_name);
        self.events.push(HubEvent::RoomModerated {
            correlation,
            link: link_id,
            room,
            action,
        });
    }

    fn note_relayed(
        &mut self,
        link_id: [u8; 16],
        room_name: &str,
        method: activity::ChannelEnvelopeKind,
        encoded_bytes: usize,
        recipients: usize,
    ) {
        let correlation = self.link_correlation(link_id);
        let room = self.room_token(room_name);
        self.events.push(HubEvent::RelayForwarded {
            correlation,
            room,
            method,
            encoded_bytes: encoded_bytes as u64,
            recipients: recipients as u64,
        });
    }

    pub(crate) fn note_service_started(&mut self) {
        let correlation = self.hub_correlation;
        self.events.push(HubEvent::ServiceStarted { correlation });
    }

    /// Teardown: every live session ends with the service, then the service
    /// itself. Bounded by the connected link count.
    pub(crate) fn note_service_stopped(&mut self) {
        let closing: Vec<(CorrelationId, [u8; 16], u64)> = self
            .sessions
            .iter()
            .map(|(link_id, session)| {
                (
                    session.correlation,
                    *link_id,
                    session
                        .established_at
                        .elapsed()
                        .as_millis()
                        .min(u128::from(u64::MAX)) as u64,
                )
            })
            .collect();
        for (correlation, link, duration_ms) in closing {
            self.events.push(HubEvent::SessionClosed {
                correlation,
                link,
                reason: activity::HubSessionCloseReason::ServiceStopped,
                duration_ms,
            });
        }
        let correlation = self.hub_correlation;
        self.events.push(HubEvent::ServiceStopped { correlation });
    }

    pub(crate) fn note_service_degraded(
        &mut self,
        reason: activity::HubServiceDegradation,
        count: usize,
    ) {
        if count == 0 {
            return;
        }
        let correlation = self.hub_correlation;
        self.events.push(HubEvent::ServiceDegraded {
            correlation,
            reason,
            count: count as u64,
        });
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
            self.room_tokens.remove(&room_name);
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
        self.note_service_degraded(activity::HubServiceDegradation::EnvelopeOversize, count);
    }

    pub(crate) fn note_send_failed(&mut self, count: usize) {
        self.note_service_degraded(activity::HubServiceDegradation::SendFailed, count);
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
        // The advertisement follows the send flag: no reference client checks
        // the hub's capability before sending, so tying it to accept would
        // advertise nothing useful and hide what we can actually do.
        if self.config.resource_send_enabled {
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

    /// The greeting is the only hub reply that may travel as a resource.
    /// Command replies must not: NomadNet parses `/list` and `/who` in its
    /// NOTICE handler alone, so a resource-delivered reply never updates its
    /// room list or member set and silently suppresses the next genuine one.
    /// A resource-delivered notice is display-only text, which is exactly what
    /// a greeting is.
    fn push_greeting(&mut self, link_id: [u8; 16], greeting: &str, out: &mut Vec<HubSend>) {
        let budget = self
            .text_body_budget(rrc::MessageType::Notice, None)
            .max(MIN_NOTICE_BODY_BYTES);
        // The threshold is "does not fit one link packet". The reference's
        // 512 bytes sits above its own MDU and carries no interop meaning.
        if greeting.len() > budget && self.push_greeting_resource(link_id, greeting, out) {
            return;
        }
        let entries = [greeting.to_string()];
        self.push_notice_entries(link_id, NoticeHeader::None, &entries, "", None, out);
    }

    /// Advertise then transfer, at most one at a time per link. Returns false
    /// when the caller must fall back to chunking; nothing is dropped here.
    fn push_greeting_resource(
        &mut self,
        link_id: [u8; 16],
        greeting: &str,
        out: &mut Vec<HubSend>,
    ) -> bool {
        if !self.config.resource_send_enabled {
            return false;
        }
        let payload = greeting.as_bytes().to_vec();
        // Above the trigger ceiling the client's 30s expectation TTL is the
        // binding constraint, and chunking always lands where a slow transfer
        // would be discarded.
        if payload.len() > self.config.max_outbound_resource_bytes
            || payload.len() as u64 > self.config.max_resource_bytes
        {
            return false;
        }
        let hub_hash = self.hub_hash;
        let Some(session) = self.sessions.get_mut(&link_id) else {
            return false;
        };
        if !session.supports_resources() || session.outbound_resource.is_some() {
            return false;
        }
        session.outbound_resource = Some(OutboundResource {
            resource_hash: None,
            started_at: Instant::now(),
        });
        let mut envelope = rrc::Envelope::new(rrc::MessageType::ResourceEnvelope, hub_hash);
        envelope.body = Some(rrc::resource_envelope_body(&rrc::ResourceEnvelopeBody {
            id: envelope.message_id,
            kind: "motd".to_string(),
            size: payload.len() as u64,
            sha256: Some(rns_crypto::sha::sha256(&payload)),
            encoding: Some("utf-8".to_string()),
        }));
        out.push(HubSend::Resource {
            link_id,
            envelope,
            payload: ResourcePayload(payload),
        });
        true
    }

    /// Shell feedback after a start attempt. `None` means the advertisement or
    /// the transfer start failed, so the slot is released on the same pass
    /// rather than waiting for the timeout sweep.
    pub(crate) fn on_resource_started(
        &mut self,
        link_id: [u8; 16],
        resource_hash: Option<[u8; 32]>,
    ) {
        let Some(session) = self.sessions.get_mut(&link_id) else {
            return;
        };
        match resource_hash {
            Some(hash) => {
                if let Some(outbound) = session.outbound_resource.as_mut() {
                    outbound.resource_hash = Some(hash);
                }
            }
            None => session.outbound_resource = None,
        }
    }

    /// Release the slot when the transport concludes an outbound transfer.
    /// Cancelled and failed conclusions free the link exactly like a complete
    /// one — the peer is not going to receive it either way.
    pub(crate) fn on_outbound_resource_concluded(
        &mut self,
        link_id: [u8; 16],
        resource_hash: [u8; 32],
    ) {
        let Some(session) = self.sessions.get_mut(&link_id) else {
            return;
        };
        let ours = session.outbound_resource.as_ref().is_some_and(|outbound| {
            outbound
                .resource_hash
                .is_none_or(|hash| hash == resource_hash)
        });
        if ours {
            session.outbound_resource = None;
        }
    }

    /// Free a slot whose transfer never concluded. `DestinationHandle` has no
    /// per-resource cancel, so the transport keeps retrying independently;
    /// this only stops one wedged transfer from blocking the link forever.
    pub(crate) fn resource_cycle(&mut self, now: Instant) {
        for session in self.sessions.values_mut() {
            let expired = session.outbound_resource.as_ref().is_some_and(|outbound| {
                now.duration_since(outbound.started_at) > OUTBOUND_RESOURCE_TIMEOUT
            });
            if expired {
                session.outbound_resource = None;
            }
        }
        self.admission.sweep(now);
    }

    fn inbound_resource_cap(&self) -> usize {
        inbound_resource_cap(&self.config)
    }

    /// Inbound RESOURCE_ENVELOPE: the announce half of announce-then-transfer.
    /// Everything in it is attacker input, so the announced size only buys a
    /// matching-size transfer slot; the delivered bytes are re-checked in full.
    fn on_resource_envelope(
        &mut self,
        link_id: [u8; 16],
        envelope: rrc::Envelope,
        out: &mut Vec<HubSend>,
    ) {
        let raw_room = envelope.room.clone();
        let raw = raw_room.as_deref();
        if !self.config.resource_accept_enabled {
            self.admission.note_rejected();
            out.push(self.hub_error_echo(link_id, "resource transfer disabled", raw));
            return;
        }
        let Some(identity) = self.session_identity(link_id) else {
            return;
        };
        let body = match rrc::parse_resource_envelope(&envelope) {
            Ok(body) => body,
            Err(error) => {
                self.admission.note_rejected();
                out.push(self.hub_error_echo(link_id, resource_envelope_error(&error), raw));
                return;
            }
        };
        if body.kind != RES_KIND_NOTICE {
            self.admission.note_rejected();
            out.push(self.hub_error_echo(link_id, "unsupported resource kind", raw));
            return;
        }
        let cap = self.inbound_resource_cap();
        if body.size > cap as u64 {
            self.admission.note_rejected();
            out.push(self.hub_error_echo(
                link_id,
                &format!("resource too large: {} > {cap}", body.size),
                raw,
            ));
            return;
        }
        // A roomless notice has nowhere to fan out, so accepting the transfer
        // would be pure amplification.
        let Some(room_raw) = raw.filter(|room| !room.is_empty()) else {
            self.admission.note_rejected();
            out.push(self.hub_error_echo(link_id, "notice resource requires room name", None));
            return;
        };
        let room_name = match self.norm_room(room_raw) {
            Ok(room_name) => room_name,
            Err(reason) => {
                self.admission.note_rejected();
                out.push(self.hub_error_echo(link_id, &reason, raw));
                return;
            }
        };
        // Gate before the transfer as well as after it: a peer who could not
        // relay this as a packet does not get to spend the link on it first.
        if !self.relay_gate(link_id, identity, &room_name, out) {
            self.admission.note_rejected();
            return;
        }
        let expectation = InboundExpectation {
            id: body.id,
            size: body.size as usize,
            sha256: body.sha256,
            encoding: body.encoding,
            room: room_name,
            created_at: Instant::now(),
        };
        if !self.admission.expect(link_id, expectation) {
            self.admission.note_rejected();
            out.push(self.hub_error_echo(link_id, "too many pending resource expectations", raw));
        }
    }

    /// A delivered inbound payload. The size cap is enforced here for the third
    /// time — envelope, accept closure, and now the bytes actually in hand,
    /// which are the only ones that were ever evidence.
    pub(crate) fn on_resource_completed(
        &mut self,
        link_id: [u8; 16],
        resource_hash: [u8; 32],
        data: Vec<u8>,
        has_metadata: bool,
        out: &mut Vec<HubSend>,
    ) {
        // Retire first and unconditionally: `resource_completions` and
        // `resource_events` are independent bounded channels, so either may be
        // the only one that arrives.
        self.admission.retire(link_id, resource_hash);
        if !self.config.resource_accept_enabled {
            return;
        }
        let Some(identity) = self.session_identity(link_id) else {
            return;
        };
        if has_metadata || data.is_empty() || data.len() > self.inbound_resource_cap() {
            self.admission.note_rejected();
            return;
        }
        let digest = rns_crypto::sha::sha256(&data);
        let Some(expectation) = self.admission.take_matching(link_id, data.len(), digest) else {
            // No expectation matches these real bytes: an unannounced payload,
            // a size that did not hold up, or a digest that did not.
            self.admission.note_rejected();
            return;
        };
        // Nothing here transcodes; an encoding we cannot read is refused
        // rather than guessed at.
        let readable = expectation
            .encoding
            .as_deref()
            .is_none_or(|encoding| encoding.eq_ignore_ascii_case("utf-8"));
        let Some(text) = readable.then(|| String::from_utf8(data).ok()).flatten() else {
            self.admission.note_rejected();
            return;
        };
        // A 4 KiB `/kick` is exactly what must not happen: command dispatch is
        // a packet path, and a resource never reaches it.
        if text.trim_start().starts_with('/') {
            self.admission.note_rejected();
            out.push(self.hub_error_echo(
                link_id,
                "commands must be sent as a message, not a resource",
                Some(&expectation.room),
            ));
            return;
        }
        // Re-gate: the advertisement may be seconds old, and a ban or `+m` set
        // in the meantime has to win.
        if !self.relay_gate(link_id, identity, &expectation.room, out) {
            self.admission.note_rejected();
            return;
        }
        self.fan_out_resource_notice(link_id, identity, &expectation.room, &text, out);
    }

    /// Release the slot a concluded inbound transfer held. Idempotent with the
    /// completion path.
    pub(crate) fn on_inbound_resource_concluded(
        &mut self,
        link_id: [u8; 16],
        resource_hash: [u8; 32],
    ) {
        self.admission.retire(link_id, resource_hash);
    }

    /// Text bytes that fit one NOTICE packet relayed as `source` into `room`.
    fn relayed_notice_budget(
        &self,
        source: [u8; 16],
        room_name: &str,
        nickname: Option<&str>,
    ) -> usize {
        let mut probe = rrc::Envelope::new(rrc::MessageType::Notice, source);
        probe.room = Some(room_name.to_string());
        probe.nickname = nickname.map(str::to_string);
        probe.body = Some(Value::Text(String::new()));
        let overhead = rrc::encode(&probe)
            .map(|bytes| bytes.len())
            .unwrap_or(LINK_PACKET_BUDGET);
        LINK_PACKET_BUDGET
            .saturating_sub(overhead)
            .saturating_sub(CBOR_TEXT_HEADER_SLACK)
    }

    /// Fan a resource-delivered notice into the room as ordinary packets, split
    /// to the link budget. The sender is echoed, exactly like packet relay.
    fn fan_out_resource_notice(
        &mut self,
        link_id: [u8; 16],
        identity: [u8; 16],
        room_name: &str,
        text: &str,
        out: &mut Vec<HubSend>,
    ) {
        let mut nickname = self
            .sessions
            .get(&link_id)
            .and_then(|session| session.nickname.clone())
            .and_then(|nickname| {
                rrc::normalize_nickname(&nickname, self.config.max_nick_bytes).ok()
            });
        let mut budget = self.relayed_notice_budget(identity, room_name, nickname.as_deref());
        if budget < MIN_NOTICE_BODY_BYTES && nickname.is_some() {
            // Same trade as packet relay: both clients cache nicks by source,
            // so the hint is what gets dropped, never the text.
            nickname = None;
            budget = self.relayed_notice_budget(identity, room_name, None);
        }
        let chunks = chunk_text(text, budget.max(MIN_NOTICE_BODY_BYTES));

        // One token per relayed chunk, on top of the one the envelope packet
        // already cost. The reference bypasses the bucket for payloads
        // entirely, which makes a resource the cheapest way to flood a room.
        let now = Instant::now();
        let mut allowed = true;
        for _ in 0..chunks.len() {
            allowed &= self.note_packet(link_id, now);
        }
        if !allowed {
            self.stats.rate_limited += 1;
            out.push(self.hub_error_echo(link_id, "rate limited", Some(room_name)));
            return;
        }

        let Some(room) = self.rooms.get(room_name) else {
            return;
        };
        let members: Vec<[u8; 16]> = room.members.iter().copied().collect();
        self.stats.notices_forwarded += 1;
        self.stats.resources_received += 1;
        self.stats.resource_bytes_received += text.len() as u64;
        let mut relayed_bytes = 0usize;
        for chunk in chunks {
            let mut notice = rrc::Envelope::new(rrc::MessageType::Notice, identity);
            notice.room = Some(room_name.to_string());
            notice.nickname = nickname.clone();
            notice.body = Some(Value::Text(chunk));
            relayed_bytes += rrc::encode(&notice).map(|bytes| bytes.len()).unwrap_or(0);
            for member in &members {
                out.push(HubSend::Envelope {
                    link_id: *member,
                    envelope: notice.clone(),
                });
            }
        }
        // One event for the whole payload rather than one per chunk: the
        // chunking is our MDU concern, not something the operator relays.
        self.note_relayed(
            link_id,
            room_name,
            activity::ChannelEnvelopeKind::Notice,
            relayed_bytes,
            members.len(),
        );
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

    /// An ERROR echoing a room name that came straight off the wire. The echo
    /// lets the client scope the message, but the room is attacker-sized, so
    /// drop it rather than hand the shell a packet it must discard.
    fn hub_error_echo(&self, link_id: [u8; 16], text: &str, room: Option<&str>) -> HubSend {
        let send = self.hub_error(link_id, text, room);
        let fits = match &send {
            HubSend::Envelope { envelope, .. } => {
                rrc::encode(envelope).is_ok_and(|bytes| bytes.len() <= LINK_PACKET_BUDGET)
            }
            _ => false,
        };
        if fits {
            send
        } else {
            self.hub_error(link_id, text, None)
        }
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
            // Defense in depth only: `identity_gate` closes a klined link
            // inside the LinkManager before identification ever reaches here,
            // so this branch records nothing.
            out.push(self.hub_error(link_id, "banned", None));
            out.push(HubSend::Close { link_id });
            self.sessions.remove(&link_id);
            self.admission.forget_link(link_id);
            return;
        }
        let per_minute = self.config.rate_messages_per_minute;
        let session = self
            .sessions
            .entry(link_id)
            .or_insert_with(|| HubSession::new(now, per_minute));
        let first = session.identity.is_none();
        session.identity = Some(identity);
        let correlation = session.correlation;
        self.by_identity.insert(identity, link_id);
        if first {
            self.events.push(HubEvent::SessionOpened {
                correlation,
                link: link_id,
                peer: identity,
            });
        }
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
        self.close_session(link_id, activity::HubSessionCloseReason::Remote, out);
    }

    /// Tear down one session and its room memberships. The reason is Activity
    /// only; the wire behavior is identical however the link went away.
    fn close_session(
        &mut self,
        link_id: [u8; 16],
        reason: activity::HubSessionCloseReason,
        out: &mut Vec<HubSend>,
    ) {
        // Expectations and live slots die with the link, whether or not it ever
        // carried a session: nothing about them survives a reconnect.
        self.admission.forget_link(link_id);
        let Some(session) = self.sessions.get(&link_id) else {
            return;
        };
        let identity = session.identity;
        let nickname = session.nickname.clone();
        let rooms: Vec<String> = session.rooms.iter().cloned().collect();
        self.events.push(HubEvent::SessionClosed {
            correlation: session.correlation,
            link: link_id,
            reason,
            duration_ms: session
                .established_at
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
        });
        if let Some(identity) = identity {
            if self.by_identity.get(&identity) == Some(&link_id) {
                self.by_identity.remove(&identity);
            }
            for room_name in rooms {
                self.remove_member_with_parted(
                    &room_name,
                    link_id,
                    identity,
                    nickname.as_deref(),
                    out,
                );
            }
        }
        // Dropped last so the departure events it produces still resolve the
        // session's correlation rather than falling back to the hub run.
        self.sessions.remove(&link_id);
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
        if !self.rooms.contains_key(room_name) {
            return;
        }
        // Minted before the `rooms` borrow: `room_token` needs `&mut self`.
        let correlation = self.link_correlation(link_id);
        let token = self.room_token(room_name);
        let Some(room) = self.rooms.get_mut(room_name) else {
            return;
        };
        let was_member = room.members.remove(&link_id);
        let remaining: Vec<[u8; 16]> = room.members.iter().copied().collect();
        let registered = room.registered;
        if remaining.is_empty() && !registered {
            self.rooms.remove(room_name);
            self.room_tokens.remove(room_name);
        }
        if was_member {
            self.events.push(HubEvent::RoomParted {
                correlation,
                link: link_id,
                room: token,
                members: remaining.len() as u64,
            });
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
            // Deliberately after the welcome guard: the reference dispatches
            // type 50 ahead of it, which lets an unwelcomed peer register
            // expectations and start transfers.
            rrc::MessageType::ResourceEnvelope => self.on_resource_envelope(link_id, envelope, out),
            // Unknown types stay silently forward-compatible.
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

        let correlation = self.link_correlation(link_id);
        let token = self.room_token(&room_name);
        let members = self
            .rooms
            .get(&room_name)
            .map(|room| room.members.len())
            .unwrap_or(0) as u64;
        self.events.push(HubEvent::RoomJoined {
            correlation,
            link: link_id,
            room: token,
            members,
        });
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

    /// Reference gate order for anything relayed into a room, shared by packet
    /// relay and by a resource-delivered notice. False means the caller must
    /// stop; the ERROR is already queued.
    ///
    /// The order is wire behavior, not style: `+n` is checked *only* on the
    /// non-member branch and nested inside the room lookup, so a non-member
    /// naming a room that does not exist gets `no such room` rather than `+n`.
    fn relay_gate(
        &self,
        link_id: [u8; 16],
        identity: [u8; 16],
        room_name: &str,
        out: &mut Vec<HubSend>,
    ) -> bool {
        let is_member = self
            .sessions
            .get(&link_id)
            .is_some_and(|session| session.rooms.contains(room_name));
        if !is_member {
            let Some(room) = self.rooms.get(room_name) else {
                out.push(self.hub_error(link_id, "no such room", Some(room_name)));
                return false;
            };
            if room.no_outside_msgs {
                out.push(self.hub_error(link_id, "no outside messages (+n)", Some(room_name)));
                return false;
            }
        }
        if let Some(room) = self.rooms.get(room_name) {
            if room.bans.contains(&identity) {
                out.push(self.hub_error(link_id, "banned from room", Some(room_name)));
                return false;
            }
            if room.moderated && !self.is_voiced(room, identity) {
                out.push(self.hub_error(link_id, "room is moderated (+m)", Some(room_name)));
                return false;
            }
        }
        true
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

        if !self.relay_gate(link_id, identity, &room_name, out) {
            return;
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

        let method = match envelope.message_type {
            rrc::MessageType::Message => {
                self.stats.messages_forwarded += 1;
                activity::ChannelEnvelopeKind::Message
            }
            rrc::MessageType::Action => {
                self.stats.actions_forwarded += 1;
                activity::ChannelEnvelopeKind::Action
            }
            _ => {
                self.stats.notices_forwarded += 1;
                activity::ChannelEnvelopeKind::Notice
            }
        };

        let Some(room) = self.rooms.get(&room_name) else {
            return;
        };
        let members: Vec<[u8; 16]> = room.members.iter().copied().collect();
        for member in members.iter().copied() {
            out.push(HubSend::Envelope {
                link_id: member,
                envelope: envelope.clone(),
            });
        }
        self.note_relayed(link_id, &room_name, method, encoded, members.len());
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
        self.note_moderated(link_id, &room_name, activity::HubModerationAction::Kick);
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
            let correlation = self.link_correlation(link_id);
            self.events.push(HubEvent::TrustChanged {
                correlation,
                link: link_id,
                change: activity::HubTrustChange::KlineAdded,
            });
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
                self.close_session(target_link, activity::HubSessionCloseReason::Kicked, out);
            }
        } else {
            let removed = self
                .klines
                .write()
                .map(|mut klines| klines.remove(&target))
                .unwrap_or(false);
            if removed {
                self.persist_klines(Some(link_id), out);
                let correlation = self.link_correlation(link_id);
                self.events.push(HubEvent::TrustChanged {
                    correlation,
                    link: link_id,
                    change: activity::HubTrustChange::KlineRemoved,
                });
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
        self.note_moderated(link_id, &room_name, activity::HubModerationAction::Register);
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
        // Recorded before the room can disappear: the token dies with it.
        self.note_moderated(
            link_id,
            &room_name,
            activity::HubModerationAction::Unregister,
        );
        if empty {
            self.rooms.remove(&room_name);
            self.room_tokens.remove(&room_name);
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
        self.note_moderated(link_id, &room_name, activity::HubModerationAction::Topic);
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
                self.note_moderated(link_id, &room_name, activity::HubModerationAction::Mode);
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
                self.note_moderated(link_id, &room_name, activity::HubModerationAction::Mode);
            }
            "-k" => {
                let room = self.rooms.get_mut(&room_name).expect("room just ensured");
                room.key = None;
                self.broadcast_mode(&room_name, out);
                self.note_moderated(link_id, &room_name, activity::HubModerationAction::Mode);
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
                let action = match flag.as_str() {
                    "+o" => activity::HubModerationAction::Op,
                    "-o" => activity::HubModerationAction::Deop,
                    "+v" => activity::HubModerationAction::Voice,
                    _ => activity::HubModerationAction::Devoice,
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
                self.note_moderated(link_id, &room_name, action);
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
        let (text, action) = match verb {
            "op" => {
                room.ops.insert(target);
                (
                    format!("op granted in {room_name}"),
                    activity::HubModerationAction::Op,
                )
            }
            "deop" => {
                if self.server_ops.contains(&target) {
                    out.push(self.hub_notice(link_id, "cannot deop a server operator", raw_room));
                    return;
                }
                room.ops.remove(&target);
                (
                    format!("op removed in {room_name}"),
                    activity::HubModerationAction::Deop,
                )
            }
            "voice" => {
                room.voiced.insert(target);
                (
                    format!("voice granted in {room_name}"),
                    activity::HubModerationAction::Voice,
                )
            }
            _ => {
                room.voiced.remove(&target);
                (
                    format!("voice removed in {room_name}"),
                    activity::HubModerationAction::Devoice,
                )
            }
        };
        self.persist_room(&room_name, Some(link_id), out);
        self.note_moderated(link_id, &room_name, action);
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
            self.note_moderated(link_id, &room_name, activity::HubModerationAction::Ban);
            out.push(self.hub_notice(link_id, &format!("ban added in {room_name}"), raw_room));
        } else {
            if let Some(room) = self.rooms.get_mut(&room_name) {
                room.bans.remove(&target);
            }
            self.persist_room(&room_name, Some(link_id), out);
            self.note_moderated(link_id, &room_name, activity::HubModerationAction::Unban);
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
            self.note_moderated(link_id, &room_name, activity::HubModerationAction::Invite);
            out.push(self.hub_notice(link_id, &confirmation, raw_room));
        } else {
            if let Some(room) = self.rooms.get_mut(&room_name) {
                room.invited.remove(&target);
            }
            self.persist_room(&room_name, Some(link_id), out);
            self.note_moderated(link_id, &room_name, activity::HubModerationAction::Uninvite);
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
            format!(
                "resources: in={} bytes={} rejected={}",
                self.stats.resources_received,
                self.stats.resource_bytes_received,
                self.admission.rejected()
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
            let correlation = self.link_correlation(link_id);
            self.events.push(HubEvent::SessionRejected {
                correlation,
                link: link_id,
                reason: activity::HubSessionRejection::WelcomeUnsendable,
            });
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
            self.push_greeting(link_id, &greeting, out);
        }
    }

    /// Hub-driven keepalive: stamp and PING idle welcomed sessions, tear down
    /// links whose PONG never arrived, and reap links that never completed a
    /// handshake within the timeout window.
    pub(crate) fn ping_cycle(&mut self, now: Instant, out: &mut Vec<HubSend>) {
        // Reaping and the throttle report run whether or not hub-driven
        // keepalive is configured: turning PINGs off must not leave half-open
        // links alive forever or silence the drop counters.
        self.report_throttle(now);
        let timeout = Duration::from_secs(self.config.ping_timeout_secs.max(1));
        let keepalive = self.config.ping_interval_secs > 0;
        let mut dead: Vec<([u8; 16], activity::HubSessionCloseReason)> = Vec::new();
        for (link_id, session) in self.sessions.iter_mut() {
            if !session.welcomed {
                if now.duration_since(session.established_at) > timeout {
                    dead.push((*link_id, activity::HubSessionCloseReason::HandshakeTimeout));
                }
                continue;
            }
            if !keepalive {
                continue;
            }
            match session.awaiting_pong_since {
                Some(since) if now.duration_since(since) > timeout => {
                    dead.push((*link_id, activity::HubSessionCloseReason::PingTimeout))
                }
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
        for (link_id, reason) in dead {
            // Full teardown here, not just the session record: we tore the link
            // down ourselves, so waiting for a transport close event would
            // leave the peer in every room it joined — rosters would list a
            // dead link forever and the room could never empty.
            self.close_session(link_id, reason, out);
            out.push(HubSend::Close { link_id });
        }
    }

    /// One aggregate per window, and only when something was actually shed.
    fn report_throttle(&mut self, now: Instant) {
        let span = now.duration_since(self.throttle_reported_at);
        if span < THROTTLE_REPORT_INTERVAL {
            return;
        }
        let rejected = self
            .stats
            .rate_limited
            .saturating_sub(self.throttle_baseline.0);
        let dropped = self
            .stats
            .bad_packets
            .saturating_add(self.stats.duplicates)
            .saturating_sub(self.throttle_baseline.1);
        self.throttle_reported_at = now;
        self.throttle_baseline = (
            self.stats.rate_limited,
            self.stats.bad_packets + self.stats.duplicates,
        );
        if rejected == 0 && dropped == 0 {
            return;
        }
        let correlation = self.hub_correlation;
        self.events.push(HubEvent::RelayThrottled {
            correlation,
            rejected,
            dropped,
            span_ms: span.as_millis().min(u128::from(u64::MAX)) as u64,
        });
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
        state: Weak<AppState>,
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
        let admission = Arc::new(ResourceAdmission::new(
            config.resource_accept_enabled,
            inbound_resource_cap(&config),
        ));
        let gate_admission = admission.clone();
        let options = DestinationRuntimeOptions {
            accepts_links: true,
            default_app_data: Some(hub_announce_app_data(&config.hub_name)),
            identity_gate: Some(IdentityGatePolicy::new(move |_link_id, identity| {
                gate_klines
                    .read()
                    .map(|klines| !klines.contains(&identity))
                    .unwrap_or(true)
            })),
            // Both halves are required to accept anything: `AcceptApp` without
            // a policy rejects every advertisement, and a policy is never
            // consulted under `AcceptNone`. Off means off at the strategy.
            resource_strategy: if config.resource_accept_enabled {
                ResourceStrategy::AcceptApp
            } else {
                ResourceStrategy::AcceptNone
            },
            resource_accept: Some(ResourceAcceptPolicy::new(move |link_id, advertisement| {
                gate_admission.admit(link_id, advertisement)
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
        let core = HubCore::new(
            config.clone(),
            hub_identity.hash,
            klines,
            admission,
            pepper,
            restored,
        );
        tokio::spawn(run_hub(
            registration,
            core,
            config,
            store,
            emitter,
            shutdown,
            command_rx,
            snapshot.clone(),
            ChannelsActivity::new(state),
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

    /// Stop the service and wait until its registry tail is flushed and the
    /// Reticulum destination has been deregistered. A false result means the
    /// actor did not acknowledge the complete teardown inside the budget; a
    /// caller must not immediately register a replacement over it.
    pub async fn shutdown(&self) -> bool {
        let (result_tx, result_rx) = oneshot::channel();
        if self
            .command_tx
            .send(HubCommand::Shutdown { result_tx })
            .await
            .is_err()
        {
            return false;
        }
        // The task flushes outstanding registry writes and closes the
        // destination before it acks, so this budget covers slow disk and
        // transport teardown, not just a channel round trip.
        matches!(
            tokio::time::timeout(Duration::from_secs(10), result_rx).await,
            Ok(Ok(()))
        )
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
    recorder: ChannelsActivity,
) {
    let mut shutdown_ack = None;
    let destination_hash = registration.handle.destination_hash();
    let activity_hub = activity::DestinationHash::new(destination_hash);
    core.note_service_started();
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
        core.note_service_degraded(activity::HubServiceDegradation::Announce, 1);
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
    // Deliberately not folded into `ping_cycle`, which early-returns when
    // hub-driven keepalive is disabled.
    let mut resource_tick =
        tokio::time::interval(Duration::from_secs(RESOURCE_CYCLE_INTERVAL_SECS));
    resource_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    resource_tick.reset();

    publish_snapshot(&snapshot, &emitter, &core, &config, Some(destination_hash));
    record_hub_events(&recorder, activity_hub, core.drain_events());
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
                        shutdown_ack = Some(result_tx);
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
            // Outbound completion arrives here as `Concluded`; the separate
            // `resource_proofs` stream carries the same fact and stays
            // undrained rather than gaining a dead arm.
            event = registration.events.resource_events.recv() => {
                if let Some(LinkResourceEvent::Concluded {
                    link_id,
                    resource_id,
                    direction,
                    ..
                }) = event
                {
                    match direction {
                        LinkResourceDirection::Outbound => {
                            core.on_outbound_resource_concluded(link_id, resource_id)
                        }
                        // This and `resource_completions` are independent
                        // bounded channels; both retire the slot, idempotently,
                        // so a drop on either one cannot strand it.
                        LinkResourceDirection::Inbound => {
                            core.on_inbound_resource_concluded(link_id, resource_id)
                        }
                    }
                }
            }
            completion = registration.events.resource_completions.recv() => {
                if let Some(completion) = completion {
                    core.on_resource_completed(
                        completion.link_id,
                        completion.resource_hash,
                        completion.data,
                        completion.metadata.is_some(),
                        &mut out,
                    );
                }
            }
            _ = resource_tick.tick() => core.resource_cycle(Instant::now()),
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
                    core.note_service_degraded(activity::HubServiceDegradation::Announce, 1);
                }
            }
        }
        flush_sends(&registration, &store, &mut core, out).await;
        record_hub_events(&recorder, activity_hub, core.drain_events());
        publish_snapshot(&snapshot, &emitter, &core, &config, Some(destination_hash));
    }

    // Land any outstanding writes before the task ends: a restart reloads the
    // registry immediately and would otherwise race the old task's tail.
    let mut final_out = Vec::new();
    core.flush_dirty_last_used(&mut final_out);
    core.retry_failed_persists(&mut final_out);
    flush_sends(&registration, &store, &mut core, final_out).await;
    core.note_service_stopped();
    record_hub_events(&recorder, activity_hub, core.drain_events());

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
    if let Some(result_tx) = shutdown_ack {
        let _ = result_tx.send(());
    }
    tracing::info!("channel hub stopped");
}

/// Every hub event is network-originated, so each one takes a fresh capture
/// fence at record time rather than inheriting a request's.
fn record_hub_events(
    recorder: &ChannelsActivity,
    hub: activity::DestinationHash,
    events: Vec<HubEvent>,
) {
    for event in events {
        let (correlation_id, transition) = hub_activity_transition(event);
        recorder.record_spontaneous(move || {
            activity::channels_hub_activity(activity::ChannelsHubActivity {
                hub,
                correlation_id,
                transition,
            })
        });
    }
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
    let mut send_failed = 0usize;
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
                        send_failed += 1;
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
            HubSend::Resource {
                link_id,
                envelope,
                payload,
            } => {
                let started = match rrc::encode(&envelope) {
                    Ok(encoded) if encoded.len() <= LINK_PACKET_BUDGET => {
                        // Advertise first: both reference clients bind an
                        // arriving resource to a pending expectation, and the
                        // TTL on that expectation starts at the envelope.
                        if registration
                            .handle
                            .send_link_packet(link_id, encoded)
                            .await
                            .is_err()
                        {
                            send_failed += 1;
                            tracing::debug!(reason = "send_failed", "hub resource envelope failed");
                            None
                        } else {
                            // No auto-compression: the advertisement announces
                            // the exact byte count the client matches on.
                            match registration
                                .handle
                                .send_link_resource(link_id, payload.0, false)
                                .await
                            {
                                Ok(receipt) => Some(receipt.resource_hash),
                                Err(_) => {
                                    send_failed += 1;
                                    tracing::warn!(
                                        reason = "resource_start_failed",
                                        "hub resource transfer did not start"
                                    );
                                    None
                                }
                            }
                        }
                    }
                    _ => {
                        oversize += 1;
                        tracing::warn!(
                            reason = "resource_envelope_unsendable",
                            "hub resource advertisement dropped"
                        );
                        None
                    }
                };
                core.on_resource_started(link_id, started);
            }
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
    if send_failed > 0 {
        core.note_send_failed(send_failed);
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

    fn settings_pool() -> db::DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        db::init_schema(&pool).unwrap();
        pool
    }

    #[test]
    fn hub_settings_are_readable_without_a_live_service() {
        let pool = settings_pool();
        let defaults = ChannelHubSettings::load(&pool).unwrap();
        assert!(!defaults.enabled);
        assert_eq!(defaults.hub_name, DEFAULT_HUB_NAME);
        assert!(defaults.greeting.is_empty());
        assert!(defaults.resource_send_enabled);
        assert!(!defaults.resource_accept_enabled);

        let configured = ChannelHubSettings {
            enabled: true,
            hub_name: "Mountain relay".to_string(),
            greeting: "Welcome".to_string(),
            announce_interval_secs: 900,
            resource_send_enabled: false,
            resource_accept_enabled: true,
        };
        db::try_set_settings(&pool, &configured.setting_rows()).unwrap();
        assert_eq!(ChannelHubSettings::load(&pool).unwrap(), configured);
    }

    #[test]
    fn corrupt_hub_settings_fall_back_to_safe_defaults() {
        let pool = settings_pool();
        db::try_set_settings(
            &pool,
            &[
                ("channel_hub_enabled".to_string(), "yes".to_string()),
                ("channel_hub_name".to_string(), "   ".to_string()),
                (
                    "channel_hub_announce_interval".to_string(),
                    "42".to_string(),
                ),
                ("channel_hub_resource_accept".to_string(), "yes".to_string()),
            ],
        )
        .unwrap();

        let settings = ChannelHubSettings::load(&pool).unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.hub_name, DEFAULT_HUB_NAME);
        assert_eq!(settings.announce_interval_secs, 0);
        assert!(!settings.resource_accept_enabled);
    }

    /// ID_A is always a server operator, mirroring production: `start` seeds
    /// the hosting identity into `server_operators`. Rooms only exist because
    /// an operator made them, so most fixtures create through ID_A.
    fn core_with(mut config: ChannelHubConfig) -> HubCore {
        if !config.server_operators.contains(&ID_A) {
            config.server_operators.push(ID_A);
        }
        let admission = Arc::new(ResourceAdmission::new(
            config.resource_accept_enabled,
            inbound_resource_cap(&config),
        ));
        HubCore::new(
            config,
            [0x77; 16],
            Arc::new(RwLock::new(HashSet::new())),
            admission,
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
    /// A resource advertisement is an ordinary packet and is held to the same
    /// budget; only the payload behind it escapes the MDU.
    fn assert_all_sendable(out: &[HubSend]) {
        for send in out {
            let envelope = match send {
                HubSend::Envelope { envelope, .. } | HubSend::Resource { envelope, .. } => envelope,
                _ => continue,
            };
            let encoded = rrc::encode(envelope).expect("hub envelopes must encode");
            assert!(
                encoded.len() <= LINK_PACKET_BUDGET,
                "{:?} encoded to {} bytes, over the {LINK_PACKET_BUDGET} budget",
                envelope.message_type,
                encoded.len()
            );
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
            Arc::new(ResourceAdmission::new(false, 4096)),
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

    /// A HELLO from a peer that advertises resource support. NomadNet and
    /// rrc-gui both do; our own client deliberately does not.
    fn capable_hello(identity: [u8; 16], nick: &str) -> rrc::Envelope {
        let mut hello = rrc::Envelope::hello(identity, nick, "1");
        hello.body = Some(rrc::integer_map(vec![
            (rrc::HELLO_CLIENT_NAME, Value::Text("nomadnet".into())),
            (rrc::HELLO_CLIENT_VERSION, Value::Text("1.2.3".into())),
            (
                rrc::HELLO_CAPABILITIES,
                rrc::integer_map(vec![(rrc::CAP_RESOURCE_ENVELOPE, Value::Bool(true))]),
            ),
        ]));
        hello
    }

    /// Returns the WELCOME pass so a caller can inspect the greeting sends.
    fn welcomed_capable_session(
        core: &mut HubCore,
        link_id: [u8; 16],
        identity: [u8; 16],
        nick: &str,
    ) -> Vec<HubSend> {
        let now = Instant::now();
        core.on_link_established(link_id, now);
        let mut out = Vec::new();
        core.on_link_identified(link_id, identity, now, &mut out);
        out.clear();
        core.on_envelope(link_id, capable_hello(identity, nick), &mut out);
        out
    }

    fn resource_sends(out: &[HubSend]) -> Vec<(&rrc::Envelope, &ResourcePayload)> {
        out.iter()
            .filter_map(|send| match send {
                HubSend::Resource {
                    envelope, payload, ..
                } => Some((envelope, payload)),
                _ => None,
            })
            .collect()
    }

    fn notice_texts(out: &[HubSend]) -> Vec<String> {
        out.iter()
            .filter_map(|send| match send {
                HubSend::Envelope { envelope, .. }
                    if envelope.message_type == rrc::MessageType::Notice =>
                {
                    rrc::text_body(envelope).map(str::to_string)
                }
                _ => None,
            })
            .collect()
    }

    fn greeting_core(greeting: &str, config: ChannelHubConfig) -> HubCore {
        core_with(ChannelHubConfig {
            greeting: Some(greeting.to_string()),
            ..config
        })
    }

    #[test]
    fn greeting_uses_a_resource_only_for_capable_peers() {
        let greeting = "m".repeat(900);
        let mut core = greeting_core(&greeting, ChannelHubConfig::default());
        let out = welcomed_capable_session(&mut core, LINK_A, ID_A, "nomad");

        let resources = resource_sends(&out);
        assert_eq!(resources.len(), 1, "one advertisement, one transfer");
        let (envelope, payload) = resources[0];
        assert_eq!(envelope.message_type, rrc::MessageType::ResourceEnvelope);
        assert_eq!(envelope.room, None, "the greeting is roomless");
        let body = rrc::parse_resource_envelope(envelope).expect("advertisement parses");
        assert_eq!(body.kind, "motd");
        assert_eq!(body.encoding.as_deref(), Some("utf-8"));
        assert_eq!(body.size, greeting.len() as u64);
        assert_eq!(body.size as usize, payload.0.len());
        assert_eq!(body.sha256, Some(rns_crypto::sha::sha256(&payload.0)));
        assert_eq!(payload.0, greeting.as_bytes());
        // The advertisement rides an ordinary link packet.
        assert_all_sendable(&out);
        // The greeting travels once: no notice carries it as well.
        assert!(notice_texts(&out).is_empty());
        // A derived Debug on the payload would put the body in every log.
        assert!(!format!("{:?}", out).contains("mmmm"));
        assert_eq!(
            format!("{:?}", ResourcePayload(vec![0u8; 3])),
            "ResourcePayload(3 bytes)"
        );

        // The same greeting to a peer that never advertised the capability
        // chunks into notices instead, losing nothing.
        let mut core = greeting_core(&greeting, ChannelHubConfig::default());
        let mut out = Vec::new();
        let now = Instant::now();
        core.on_link_established(LINK_B, now);
        core.on_link_identified(LINK_B, ID_B, now, &mut out);
        out.clear();
        core.on_envelope(LINK_B, rrc::Envelope::hello(ID_B, "rat", "1"), &mut out);
        assert!(resource_sends(&out).is_empty());
        let chunks = notice_texts(&out);
        assert!(chunks.len() > 1, "900 bytes cannot be one notice");
        assert_eq!(chunks.concat(), greeting);
        assert_all_sendable(&out);
        assert!(core.sessions[&LINK_B].outbound_resource.is_none());

        // A greeting that fits one packet stays one notice even for a capable
        // peer: the threshold is the packet budget, not a byte constant.
        let mut core = greeting_core("hello mesh", ChannelHubConfig::default());
        let out = welcomed_capable_session(&mut core, LINK_A, ID_A, "nomad");
        assert!(resource_sends(&out).is_empty());
        assert_eq!(notice_texts(&out), vec!["hello mesh".to_string()]);
    }

    #[test]
    fn command_output_never_takes_the_resource_path() {
        let config = ChannelHubConfig {
            server_operators: vec![ID_A],
            ..ChannelHubConfig::default()
        };
        let mut core = core_with(config);
        welcomed_capable_session(&mut core, LINK_A, ID_A, "nomad");
        assert!(core.sessions[&LINK_A].supports_resources());
        join(&mut core, LINK_A, ID_A, "lobby");
        for index in 0..30u8 {
            let link = [0xA0, index, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, index];
            welcomed_session(&mut core, link, [index; 16], &format!("member{index:02}"));
            join(&mut core, link, [index; 16], "lobby");
        }
        run_command(&mut core, LINK_A, ID_A, "/register lobby");
        for index in 0..30u8 {
            core.rooms
                .get_mut("lobby")
                .unwrap()
                .bans
                .insert([0xC0, index, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, index]);
        }

        // NomadNet parses /list and /who in its NOTICE handler only, so a
        // resource-delivered reply never updates its room list or member set
        // and suppresses the next genuine reply.
        for command in ["/who lobby", "/list", "/ban lobby list", "/stats"] {
            let out = run_command(&mut core, LINK_A, ID_A, command);
            assert!(!out.is_empty(), "{command} produced no reply");
            assert!(
                resource_sends(&out).is_empty(),
                "{command} must never become a resource"
            );
            assert_all_sendable(&out);
        }

        // Structural, not incidental: the chunker itself has no resource
        // branch, so even a headerless multi-packet reply stays notices.
        let entries: Vec<String> = (0..40)
            .map(|index| format!("entry-{index:03}-{}", "z".repeat(20)))
            .collect();
        let mut out = Vec::new();
        core.push_notice_entries(LINK_A, NoticeHeader::None, &entries, ", ", None, &mut out);
        assert!(out.len() > 1);
        assert!(resource_sends(&out).is_empty());
    }

    #[test]
    fn outbound_resource_ceiling_falls_back_to_chunking() {
        let greeting = "g".repeat(2048);
        let mut core = greeting_core(
            &greeting,
            ChannelHubConfig {
                max_outbound_resource_bytes: 1024,
                ..ChannelHubConfig::default()
            },
        );
        let out = welcomed_capable_session(&mut core, LINK_A, ID_A, "nomad");

        // Past the ceiling the client's 30s expectation TTL is the binding
        // constraint; chunking always lands where a slow transfer is dropped.
        assert!(resource_sends(&out).is_empty());
        let chunks = notice_texts(&out);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), greeting);
        assert!(
            core.sessions[&LINK_A].outbound_resource.is_none(),
            "a refused start must not hold the slot"
        );
    }

    #[test]
    fn outbound_resources_serialize_per_link() {
        let greeting = "g".repeat(900);
        let mut core = greeting_core(&greeting, ChannelHubConfig::default());
        let out = welcomed_capable_session(&mut core, LINK_A, ID_A, "nomad");
        assert_eq!(resource_sends(&out).len(), 1);
        core.on_resource_started(LINK_A, Some([0x09; 32]));

        // Both reference clients bind an arriving resource to a pending
        // expectation by size alone, so a second live transfer would
        // cross-match. It falls back to chunking rather than advertising.
        let mut out = Vec::new();
        core.push_greeting(LINK_A, &greeting, &mut out);
        assert!(resource_sends(&out).is_empty());
        assert_eq!(notice_texts(&out).concat(), greeting);

        // Another transfer's conclusion must not free this slot.
        core.on_outbound_resource_concluded(LINK_A, [0x22; 32]);
        assert!(core.sessions[&LINK_A].outbound_resource.is_some());

        core.on_outbound_resource_concluded(LINK_A, [0x09; 32]);
        assert!(core.sessions[&LINK_A].outbound_resource.is_none());
        let mut out = Vec::new();
        core.push_greeting(LINK_A, &greeting, &mut out);
        assert_eq!(resource_sends(&out).len(), 1);
    }

    #[test]
    fn outbound_start_failure_releases_the_slot() {
        let greeting = "g".repeat(900);
        let mut core = greeting_core(&greeting, ChannelHubConfig::default());
        let out = welcomed_capable_session(&mut core, LINK_A, ID_A, "nomad");
        assert_eq!(resource_sends(&out).len(), 1);
        assert!(core.sessions[&LINK_A].outbound_resource.is_some());

        // The shell reports a failed advertisement or start as `None`, which
        // releases the slot on the same pass rather than after the sweep.
        core.on_resource_started(LINK_A, None);
        assert!(core.sessions[&LINK_A].outbound_resource.is_none());
        let mut out = Vec::new();
        core.push_greeting(LINK_A, &greeting, &mut out);
        assert_eq!(resource_sends(&out).len(), 1);
    }

    #[test]
    fn resource_cycle_times_out_a_wedged_outbound_transfer() {
        let greeting = "g".repeat(900);
        let mut core = greeting_core(&greeting, ChannelHubConfig::default());
        welcomed_capable_session(&mut core, LINK_A, ID_A, "nomad");
        core.on_resource_started(LINK_A, Some([0x07; 32]));

        let now = Instant::now();
        core.resource_cycle(now);
        assert!(
            core.sessions[&LINK_A].outbound_resource.is_some(),
            "a transfer still inside the window keeps its slot"
        );
        core.resource_cycle(now + OUTBOUND_RESOURCE_TIMEOUT + Duration::from_secs(1));
        assert!(core.sessions[&LINK_A].outbound_resource.is_none());
    }

    #[test]
    fn disabling_resource_send_withdraws_the_capability_and_chunks() {
        assert!(
            !ChannelHubConfig::default().resource_accept_enabled,
            "inbound acceptance stays off by default"
        );
        let greeting = "g".repeat(900);
        let mut core = greeting_core(
            &greeting,
            ChannelHubConfig {
                resource_send_enabled: false,
                ..ChannelHubConfig::default()
            },
        );
        let out = welcomed_capable_session(&mut core, LINK_A, ID_A, "nomad");

        let welcome = first_envelope(&out);
        assert_eq!(welcome.message_type, rrc::MessageType::Welcome);
        assert_eq!(
            rrc::parse_welcome(welcome)
                .capabilities
                .get(&rrc::CAP_RESOURCE_ENVELOPE),
            None,
            "the advertisement follows the send flag"
        );
        assert!(resource_sends(&out).is_empty());
        assert_eq!(notice_texts(&out).concat(), greeting);
    }

    fn accepting_config() -> ChannelHubConfig {
        ChannelHubConfig {
            resource_accept_enabled: true,
            ..ChannelHubConfig::default()
        }
    }

    /// A RESOURCE_ENVELOPE shaped the way a client sends one.
    fn resource_envelope(
        id: [u8; 8],
        kind: &str,
        size: u64,
        sha256: Option<[u8; 32]>,
        room: Option<&str>,
    ) -> rrc::Envelope {
        let mut envelope = rrc::Envelope::new(rrc::MessageType::ResourceEnvelope, ID_B);
        envelope.room = room.map(str::to_string);
        envelope.body = Some(rrc::resource_envelope_body(&rrc::ResourceEnvelopeBody {
            id,
            kind: kind.to_string(),
            size,
            sha256,
            encoding: None,
        }));
        envelope
    }

    /// Announce `payload` so a later completion has something to match.
    fn announce(
        core: &mut HubCore,
        link_id: [u8; 16],
        id: [u8; 8],
        payload: &[u8],
        room: &str,
    ) -> Vec<HubSend> {
        let mut out = Vec::new();
        core.on_envelope(
            link_id,
            resource_envelope(
                id,
                "notice",
                payload.len() as u64,
                Some(rns_crypto::sha::sha256(payload)),
                Some(room),
            ),
            &mut out,
        );
        out
    }

    /// A single-segment, metadata-free advertisement: the only shape we admit.
    fn advertisement(size: usize, resource_hash: [u8; 32]) -> ResourceAdvertisement {
        ResourceAdvertisement::new(
            size + 64,
            size,
            1,
            resource_hash,
            vec![0u8; 4],
            rns_protocol::resource::ResourceFlags::default(),
            &[],
            rns_wire::constants::LINK_MDU,
        )
    }

    fn error_texts(out: &[HubSend], target: [u8; 16]) -> Vec<String> {
        sends_to(out, target)
            .into_iter()
            .filter(|envelope| envelope.message_type == rrc::MessageType::Error)
            .filter_map(|envelope| rrc::text_body(envelope).map(str::to_string))
            .collect()
    }

    fn relayed_texts(out: &[HubSend], target: [u8; 16]) -> Vec<String> {
        sends_to(out, target)
            .into_iter()
            .filter(|envelope| envelope.message_type == rrc::MessageType::Notice)
            .filter_map(|envelope| rrc::text_body(envelope).map(str::to_string))
            .collect()
    }

    /// Operator ID_A owns "lobby"; ID_B is a welcomed member of it.
    fn resource_room(config: ChannelHubConfig) -> HubCore {
        let mut core = core_with(config);
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");
        core
    }

    #[test]
    fn resource_envelope_requires_welcome() {
        let (mut core, link_id) = identified_core(accepting_config());
        let mut out = Vec::new();
        core.on_envelope(
            link_id,
            resource_envelope([0x11; 8], "notice", 64, None, Some("lobby")),
            &mut out,
        );
        // The reference dispatches type 50 ahead of its welcome guard, which
        // lets an unwelcomed peer register expectations and start transfers.
        assert_eq!(error_texts(&out, link_id), vec!["send HELLO first"]);
        assert_eq!(core.admission.pending_count(link_id), 0);
        assert!(
            !core
                .admission
                .admit(link_id, &advertisement(64, [0x01; 32]))
        );

        // The same envelope after HELLO and a join registers.
        let mut core = resource_room(accepting_config());
        let out = announce(&mut core, LINK_B, [0x11; 8], &[0u8; 64], "lobby");
        assert!(error_texts(&out, LINK_B).is_empty());
        assert_eq!(core.admission.pending_count(LINK_B), 1);
    }

    #[test]
    fn resource_envelope_validation_matches_reference_texts() {
        let mut off = resource_room(ChannelHubConfig::default());
        let mut out = Vec::new();
        off.on_envelope(
            LINK_B,
            resource_envelope([0x01; 8], "notice", 64, None, Some("Lobby")),
            &mut out,
        );
        assert_eq!(
            error_texts(&out, LINK_B),
            vec!["resource transfer disabled"]
        );
        // The raw room rides back so the client can scope the message.
        assert_eq!(sends_to(&out, LINK_B)[0].room.as_deref(), Some("Lobby"));
        assert_eq!(off.admission.pending_count(LINK_B), 0);

        let mut core = resource_room(accepting_config());
        let refuse = |core: &mut HubCore, envelope: rrc::Envelope| -> String {
            let mut out = Vec::new();
            core.on_envelope(LINK_B, envelope, &mut out);
            let texts = error_texts(&out, LINK_B);
            assert_eq!(texts.len(), 1, "exactly one refusal");
            assert_eq!(core.admission.pending_count(LINK_B), 0);
            texts.into_iter().next().unwrap()
        };

        let mut not_a_map = rrc::Envelope::new(rrc::MessageType::ResourceEnvelope, ID_B);
        not_a_map.room = Some("lobby".into());
        not_a_map.body = Some(Value::Text("nope".into()));
        assert_eq!(
            refuse(&mut core, not_a_map),
            "invalid resource envelope body"
        );

        let field = |fields: Vec<(u64, Value)>| {
            let mut envelope = rrc::Envelope::new(rrc::MessageType::ResourceEnvelope, ID_B);
            envelope.room = Some("lobby".into());
            envelope.body = Some(rrc::integer_map(fields));
            envelope
        };
        assert_eq!(
            refuse(
                &mut core,
                field(vec![
                    (rrc::RESOURCE_KIND, Value::Text("notice".into())),
                    (rrc::RESOURCE_SIZE, Value::Integer(64.into())),
                ])
            ),
            "resource envelope missing id"
        );
        assert_eq!(
            refuse(
                &mut core,
                field(vec![
                    (rrc::RESOURCE_ID, Value::Bytes(vec![0x02; 8])),
                    (rrc::RESOURCE_SIZE, Value::Integer(64.into())),
                ])
            ),
            "resource envelope missing kind"
        );
        assert_eq!(
            refuse(
                &mut core,
                field(vec![
                    (rrc::RESOURCE_ID, Value::Bytes(vec![0x03; 8])),
                    (rrc::RESOURCE_KIND, Value::Text("notice".into())),
                ])
            ),
            "resource envelope invalid size"
        );
        assert_eq!(
            refuse(
                &mut core,
                resource_envelope([0x04; 8], "notice", 0, None, Some("lobby"))
            ),
            "resource envelope invalid size"
        );
        assert_eq!(
            refuse(
                &mut core,
                field(vec![
                    (rrc::RESOURCE_ID, Value::Bytes(vec![0x05; 8])),
                    (rrc::RESOURCE_KIND, Value::Text("notice".into())),
                    (rrc::RESOURCE_SIZE, Value::Integer(64.into())),
                    (rrc::RESOURCE_SHA256, Value::Bytes(vec![0xAA; 7])),
                ])
            ),
            "resource envelope invalid sha256"
        );

        // The reference accepts motd/blob and then discards them: pure
        // amplification, so they never buy a transfer here.
        for kind in ["motd", "blob", "notice2", ""] {
            assert_eq!(
                refuse(
                    &mut core,
                    resource_envelope([0x06; 8], kind, 64, None, Some("lobby"))
                ),
                "unsupported resource kind",
                "kind {kind:?} must not be accepted"
            );
        }

        assert_eq!(
            refuse(
                &mut core,
                resource_envelope([0x07; 8], "notice", 5000, None, Some("lobby"))
            ),
            "resource too large: 5000 > 4096"
        );
        assert_eq!(
            refuse(
                &mut core,
                resource_envelope([0x08; 8], "notice", 64, None, None)
            ),
            "notice resource requires room name"
        );
        assert_eq!(
            refuse(
                &mut core,
                resource_envelope([0x09; 8], "notice", 64, None, Some(&"r".repeat(65)))
            ),
            "room name too long: 65 bytes > 64 bytes"
        );
    }

    #[test]
    fn resource_expectation_budget_and_ttl() {
        let mut core = resource_room(accepting_config());
        for index in 0..MAX_PENDING_EXPECTATIONS {
            let out = announce(
                &mut core,
                LINK_B,
                [index as u8; 8],
                &vec![0u8; 64 + index],
                "lobby",
            );
            assert!(error_texts(&out, LINK_B).is_empty());
        }
        assert_eq!(
            core.admission.pending_count(LINK_B),
            MAX_PENDING_EXPECTATIONS
        );

        let out = announce(&mut core, LINK_B, [0xEE; 8], &[0u8; 4000], "lobby");
        assert_eq!(
            error_texts(&out, LINK_B),
            vec!["too many pending resource expectations"]
        );

        // A repeat announcement of a live id refreshes rather than stacks, so a
        // retrying client cannot exhaust its own budget.
        let out = announce(&mut core, LINK_B, [0x00; 8], &[0u8; 128], "lobby");
        assert!(error_texts(&out, LINK_B).is_empty());
        assert_eq!(
            core.admission.pending_count(LINK_B),
            MAX_PENDING_EXPECTATIONS
        );

        assert!(
            core.admission
                .admit(LINK_B, &advertisement(128, [0x77; 32]))
        );
        core.admission.retire(LINK_B, [0x77; 32]);

        // Past the TTL the announcement no longer buys a transfer.
        core.resource_cycle(Instant::now() + RESOURCE_EXPECTATION_TTL + Duration::from_secs(1));
        assert_eq!(core.admission.pending_count(LINK_B), 0);
        assert!(
            !core
                .admission
                .admit(LINK_B, &advertisement(128, [0x78; 32]))
        );
    }

    #[test]
    fn resource_admission_refuses_unannounced_oversized_split_and_metadata() {
        let mut core = resource_room(accepting_config());
        let payload = vec![0x41u8; 100];
        announce(&mut core, LINK_B, [0x01; 8], &payload, "lobby");

        assert!(
            !core.admission.admit(LINK_B, &advertisement(99, [0x01; 32])),
            "a size nobody announced is refused"
        );
        assert!(
            !core
                .admission
                .admit(LINK_B, &advertisement(4097, [0x03; 32]))
        );

        // Isolated from the announced-size check: an expectation the envelope
        // path could never produce still cannot buy an over-cap or empty
        // transfer, because the closure re-checks the cap itself.
        let tight = ResourceAdmission::new(true, 128);
        for size in [0usize, 200] {
            assert!(tight.expect(
                LINK_B,
                InboundExpectation {
                    id: [size as u8; 8],
                    size,
                    sha256: None,
                    encoding: None,
                    room: "lobby".into(),
                    created_at: Instant::now(),
                },
            ));
            assert!(
                !tight.admit(LINK_B, &advertisement(size, [size as u8; 32])),
                "an announced {size}-byte payload is still refused by the cap"
            );
        }
        // The cap itself is admissible: the refusal is `>`, not `>=`.
        assert!(tight.expect(
            LINK_B,
            InboundExpectation {
                id: [0x99; 8],
                size: 128,
                sha256: None,
                encoding: None,
                room: "lobby".into(),
                created_at: Instant::now(),
            },
        ));
        assert!(tight.admit(LINK_B, &advertisement(128, [0x99; 32])));

        let mut split = advertisement(100, [0x04; 32]);
        split.flags.split = true;
        assert!(!core.admission.admit(LINK_B, &split));
        let mut segmented = advertisement(100, [0x05; 32]);
        segmented.total_segments = 2;
        assert!(!core.admission.admit(LINK_B, &segmented));
        // total_size = data_size + metadata_size, so metadata shifts the byte
        // count the expectation matches on.
        let mut with_metadata = advertisement(100, [0x06; 32]);
        with_metadata.flags.has_metadata = true;
        assert!(!core.admission.admit(LINK_B, &with_metadata));
        let mut prefixed = advertisement(100, [0x07; 32]);
        prefixed.metadata_size = 8;
        assert!(!core.admission.admit(LINK_B, &prefixed));
        assert_eq!(core.admission.live_count(LINK_B), 0);

        assert!(
            core.admission
                .admit(LINK_B, &advertisement(100, [0x10; 32]))
        );
        assert_eq!(core.admission.live_count(LINK_B), 1);
        // Re-advertisement of a live transfer is not a second slot: the policy
        // is consulted again per segment.
        assert!(
            core.admission
                .admit(LINK_B, &advertisement(100, [0x10; 32]))
        );
        assert_eq!(core.admission.live_count(LINK_B), 1);

        for index in 1..MAX_INBOUND_RESOURCES_PER_LINK as u8 {
            assert!(
                core.admission
                    .admit(LINK_B, &advertisement(100, [0x10 + index; 32]))
            );
        }
        assert_eq!(
            core.admission.live_count(LINK_B),
            MAX_INBOUND_RESOURCES_PER_LINK
        );
        assert!(
            !core
                .admission
                .admit(LINK_B, &advertisement(100, [0x20; 32])),
            "the fifth concurrent transfer on one link is refused"
        );
        // Another link is unaffected, and has announced nothing of its own.
        assert!(
            !core
                .admission
                .admit(LINK_A, &advertisement(100, [0x30; 32]))
        );

        // With acceptance off nothing is admitted, announced or not.
        let disabled = ResourceAdmission::new(false, 4096);
        assert!(!disabled.admit(LINK_B, &advertisement(100, [0x40; 32])));
        assert_eq!(disabled.rejected(), 1);
    }

    #[test]
    fn inbound_resource_notice_passes_relay_gates() {
        let mut core = resource_room(accepting_config());
        let link_c = [0x03; 16];
        let id_c = [0xCC; 16];
        welcomed_session(&mut core, link_c, id_c, "gamma");

        core.rooms.get_mut("lobby").unwrap().no_outside_msgs = true;
        let out = announce(&mut core, link_c, [0x01; 8], &[0u8; 64], "lobby");
        assert_eq!(error_texts(&out, link_c), vec!["no outside messages (+n)"]);
        assert_eq!(core.admission.pending_count(link_c), 0);

        let out = announce(&mut core, link_c, [0x02; 8], &[0u8; 64], "ghost");
        assert_eq!(error_texts(&out, link_c), vec!["no such room"]);
        core.rooms.get_mut("lobby").unwrap().no_outside_msgs = false;

        core.rooms.get_mut("lobby").unwrap().bans.insert(ID_B);
        let out = announce(&mut core, LINK_B, [0x03; 8], &[0u8; 64], "lobby");
        assert_eq!(error_texts(&out, LINK_B), vec!["banned from room"]);
        core.rooms.get_mut("lobby").unwrap().bans.remove(&ID_B);

        core.rooms.get_mut("lobby").unwrap().moderated = true;
        let out = announce(&mut core, LINK_B, [0x04; 8], &[0u8; 64], "lobby");
        assert_eq!(error_texts(&out, LINK_B), vec!["room is moderated (+m)"]);
        core.rooms.get_mut("lobby").unwrap().moderated = false;

        // Gated again at completion: the advertisement is seconds old and a ban
        // set in the meantime has to win.
        let payload = b"a legitimate broadcast".to_vec();
        announce(&mut core, LINK_B, [0x05; 8], &payload, "lobby");
        core.rooms.get_mut("lobby").unwrap().bans.insert(ID_B);
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x55; 32], payload.clone(), false, &mut out);
        assert_eq!(error_texts(&out, LINK_B), vec!["banned from room"]);
        assert!(relayed_texts(&out, LINK_A).is_empty());
        core.rooms.get_mut("lobby").unwrap().bans.remove(&ID_B);

        // Happy path: one payload too big for a packet, fanned to every member
        // including the sender, losing nothing.
        let payload = "long notice ".repeat(100).into_bytes();
        announce(&mut core, LINK_B, [0x06; 8], &payload, "lobby");
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x66; 32], payload.clone(), false, &mut out);
        assert!(error_texts(&out, LINK_B).is_empty());
        let to_a = relayed_texts(&out, LINK_A);
        let to_b = relayed_texts(&out, LINK_B);
        assert!(to_a.len() > 1, "1200 bytes cannot ride one packet");
        assert_eq!(
            to_a, to_b,
            "the sender is echoed, exactly like packet relay"
        );
        assert_eq!(
            to_a.concat().into_bytes(),
            payload,
            "every byte arrives, in order"
        );
        assert!(
            relayed_texts(&out, link_c).is_empty(),
            "non-members get nothing"
        );
        for envelope in sends_to(&out, LINK_A) {
            assert_eq!(envelope.source, ID_B, "the relayed source is the sender");
            assert_eq!(envelope.room.as_deref(), Some("lobby"));
            assert_eq!(envelope.nickname.as_deref(), Some("beta"));
        }
        assert_all_sendable(&out);
        // The expectation is consumed, so the same bytes cannot be replayed.
        assert_eq!(core.admission.pending_count(LINK_B), 0);
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x66; 32], payload, false, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn inbound_resource_notice_charges_rate_per_chunk() {
        let mut core = resource_room(accepting_config());
        let payload = "x".repeat(1200).into_bytes();
        announce(&mut core, LINK_B, [0x01; 8], &payload, "lobby");
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x01; 32], payload.clone(), false, &mut out);
        let chunks = relayed_texts(&out, LINK_B).len();
        assert!(chunks >= 4);

        // One token per relayed chunk, on top of the token the envelope packet
        // already cost. The reference bypasses the bucket entirely here.
        let mut core = resource_room(ChannelHubConfig {
            rate_messages_per_minute: chunks,
            ..accepting_config()
        });
        announce(&mut core, LINK_B, [0x02; 8], &payload, "lobby");
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x02; 32], payload.clone(), false, &mut out);
        assert_eq!(relayed_texts(&out, LINK_B).len(), chunks);
        assert!(
            !core.note_packet(LINK_B, Instant::now()),
            "the fan-out drained the bucket"
        );

        let mut core = resource_room(ChannelHubConfig {
            rate_messages_per_minute: chunks - 1,
            ..accepting_config()
        });
        announce(&mut core, LINK_B, [0x03; 8], &payload, "lobby");
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x03; 32], payload, false, &mut out);
        assert_eq!(error_texts(&out, LINK_B), vec!["rate limited"]);
        assert!(
            relayed_texts(&out, LINK_A).is_empty(),
            "a payload that cannot be paid for is never partly relayed"
        );
        assert_eq!(core.stats.rate_limited, 1);
    }

    #[test]
    fn inbound_resource_rejects_sha_mismatch_oversize_and_bad_utf8() {
        let mut core = resource_room(accepting_config());
        let payload = b"the announced payload".to_vec();
        announce(&mut core, LINK_B, [0x01; 8], &payload, "lobby");

        let forged = b"a different payload!!".to_vec();
        assert_eq!(forged.len(), payload.len(), "same size, different bytes");
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x01; 32], forged, false, &mut out);
        assert!(out.is_empty(), "a digest that does not hold up is dropped");
        assert_eq!(
            core.admission.pending_count(LINK_B),
            1,
            "the genuine expectation is not burned by a forgery"
        );

        // `data_size` is unvalidated attacker input, so a legal claim followed
        // by oversized bytes is caught on the bytes.
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x02; 32], vec![0x41; 5000], false, &mut out);
        assert!(out.is_empty());

        // Metadata never reaches a payload we asked for.
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x03; 32], payload.clone(), true, &mut out);
        assert!(out.is_empty());

        // Unannounced entirely.
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x04; 32], b"surprise".to_vec(), false, &mut out);
        assert!(out.is_empty());

        // Bytes that are not text at all.
        let invalid = vec![0xF0, 0x9F, 0x92, 0xA9, 0xFF, 0xFE];
        announce(&mut core, LINK_B, [0x05; 8], &invalid, "lobby");
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x05; 32], invalid, false, &mut out);
        assert!(out.is_empty(), "a notice that is not UTF-8 is dropped");

        assert_eq!(core.stats.resources_received, 0);
        assert!(core.admission.rejected() >= 5);

        // The genuine payload still relays.
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x01; 32], payload.clone(), false, &mut out);
        assert_eq!(
            relayed_texts(&out, LINK_A),
            vec![String::from_utf8(payload).unwrap()]
        );
    }

    #[test]
    fn inbound_resource_refuses_slash_commands() {
        let mut core = resource_room(accepting_config());
        assert!(core.rooms["lobby"].members.contains(&LINK_B));
        // A 4 KiB /kick is exactly what a resource must never be able to do.
        let payload = format!("/kick lobby beta {}", "x".repeat(4000)).into_bytes();
        announce(&mut core, LINK_A, [0x01; 8], &payload, "lobby");
        let mut out = Vec::new();
        core.on_resource_completed(LINK_A, [0x01; 32], payload, false, &mut out);
        assert_eq!(
            error_texts(&out, LINK_A),
            vec!["commands must be sent as a message, not a resource"]
        );
        assert_eq!(sends_to(&out, LINK_A)[0].room.as_deref(), Some("lobby"));
        assert!(
            core.rooms["lobby"].members.contains(&LINK_B),
            "the command was refused, not executed"
        );
        assert!(relayed_texts(&out, LINK_B).is_empty(), "and never relayed");

        // Leading whitespace does not smuggle one through.
        let payload = b"   /stats".to_vec();
        announce(&mut core, LINK_A, [0x02; 8], &payload, "lobby");
        let mut out = Vec::new();
        core.on_resource_completed(LINK_A, [0x02; 32], payload, false, &mut out);
        assert_eq!(
            error_texts(&out, LINK_A),
            vec!["commands must be sent as a message, not a resource"]
        );
    }

    #[test]
    fn live_slot_retires_on_completion_as_well_as_conclusion() {
        let mut core = resource_room(accepting_config());
        let payload = b"a relayed notice".to_vec();

        announce(&mut core, LINK_B, [0x01; 8], &payload, "lobby");
        assert!(
            core.admission
                .admit(LINK_B, &advertisement(payload.len(), [0x01; 32]))
        );
        assert_eq!(core.admission.live_count(LINK_B), 1);
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x01; 32], payload.clone(), false, &mut out);
        assert_eq!(
            core.admission.live_count(LINK_B),
            0,
            "the completion channel retires the slot"
        );

        // `resource_events` and `resource_completions` are independent bounded
        // channels; either one alone must free the slot.
        announce(&mut core, LINK_B, [0x02; 8], &payload, "lobby");
        assert!(
            core.admission
                .admit(LINK_B, &advertisement(payload.len(), [0x02; 32]))
        );
        core.on_inbound_resource_concluded(LINK_B, [0x02; 32]);
        assert_eq!(core.admission.live_count(LINK_B), 0);
        // Idempotent: the other channel's copy changes nothing.
        core.on_inbound_resource_concluded(LINK_B, [0x02; 32]);
        assert_eq!(core.admission.live_count(LINK_B), 0);

        // A transfer that concludes on neither channel is swept.
        announce(&mut core, LINK_B, [0x03; 8], &payload, "lobby");
        assert!(
            core.admission
                .admit(LINK_B, &advertisement(payload.len(), [0x03; 32]))
        );
        let now = Instant::now();
        core.resource_cycle(now);
        assert_eq!(core.admission.live_count(LINK_B), 1);
        core.resource_cycle(now + INBOUND_RESOURCE_TIMEOUT + Duration::from_secs(1));
        assert_eq!(core.admission.live_count(LINK_B), 0);
    }

    #[test]
    fn link_close_clears_resource_state() {
        let mut core = resource_room(accepting_config());
        announce(&mut core, LINK_B, [0x01; 8], &[0u8; 64], "lobby");
        assert!(core.admission.admit(LINK_B, &advertisement(64, [0x01; 32])));
        assert_eq!(core.admission.pending_count(LINK_B), 1);
        assert_eq!(core.admission.live_count(LINK_B), 1);

        let mut out = Vec::new();
        core.on_link_closed(LINK_B, &mut out);
        assert_eq!(core.admission.pending_count(LINK_B), 0);
        assert_eq!(core.admission.live_count(LINK_B), 0);
        assert!(
            !core.admission.admit(LINK_B, &advertisement(64, [0x02; 32])),
            "nothing about a closed link survives a reconnect"
        );

        // A klined identity is torn down at identify; its state goes too.
        announce(&mut core, LINK_A, [0x02; 8], &[0u8; 64], "lobby");
        assert!(core.admission.admit(LINK_A, &advertisement(64, [0x03; 32])));
        core.klines.write().unwrap().insert(ID_A);
        let mut out = Vec::new();
        core.on_link_identified(LINK_A, ID_A, Instant::now(), &mut out);
        assert_eq!(error_texts(&out, LINK_A), vec!["banned"]);
        assert_eq!(core.admission.pending_count(LINK_A), 0);
        assert_eq!(core.admission.live_count(LINK_A), 0);
    }

    #[test]
    fn stats_counts_resource_transfers() {
        let mut core = resource_room(accepting_config());
        let stats_line = |core: &mut HubCore| -> String {
            let out = run_command(core, LINK_A, ID_A, "/stats");
            let text = sends_to(&out, LINK_A)
                .into_iter()
                .filter_map(rrc::text_body)
                .collect::<Vec<_>>()
                .join("");
            text.lines()
                .find(|line| line.starts_with("resources: "))
                .map(str::to_string)
                .expect("a resources line")
        };
        assert_eq!(stats_line(&mut core), "resources: in=0 bytes=0 rejected=0");

        let payload = b"counted once".to_vec();
        announce(&mut core, LINK_B, [0x01; 8], &payload, "lobby");
        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x01; 32], payload, false, &mut out);
        assert_eq!(stats_line(&mut core), "resources: in=1 bytes=12 rejected=0");

        // The accept closure cannot reach HubStats, so its refusals are counted
        // inside the admission gate and read here.
        assert!(
            !core
                .admission
                .admit(LINK_B, &advertisement(999, [0x02; 32]))
        );
        assert_eq!(stats_line(&mut core), "resources: in=1 bytes=12 rejected=1");
    }

    /// Lower drained events exactly as the shell does, so these tests assert on
    /// what Activity receives rather than on the intermediate enum.
    fn lowered(core: &mut HubCore) -> Vec<(CorrelationId, activity::HubTransition)> {
        core.drain_events()
            .into_iter()
            .map(hub_activity_transition)
            .collect()
    }

    /// `kind:reason[:count]`. Restating the codes here is the point: it pins
    /// the vocabulary the dashboard renders against a silent rename.
    fn describe(transition: &activity::HubTransition) -> String {
        match transition {
            activity::HubTransition::ServiceStarted => "service.started".to_string(),
            activity::HubTransition::ServiceStopped => "service.stopped".to_string(),
            activity::HubTransition::ServiceDegraded { reason, count } => format!(
                "service.degraded:{}:{count}",
                match reason {
                    activity::HubServiceDegradation::Announce => "announce",
                    activity::HubServiceDegradation::EnvelopeOversize => "envelope_oversize",
                    activity::HubServiceDegradation::SendFailed => "send_failed",
                }
            ),
            activity::HubTransition::SessionOpened { .. } => "session.opened".to_string(),
            activity::HubTransition::SessionRejected { reason, .. } => format!(
                "session.rejected:{}",
                match reason {
                    activity::HubSessionRejection::WelcomeUnsendable => "welcome_unsendable",
                }
            ),
            activity::HubTransition::SessionClosed { reason, .. } => format!(
                "session.closed:{}",
                match reason {
                    activity::HubSessionCloseReason::Remote => "remote",
                    activity::HubSessionCloseReason::PingTimeout => "ping_timed_out",
                    activity::HubSessionCloseReason::HandshakeTimeout => "handshake_timed_out",
                    activity::HubSessionCloseReason::Kicked => "kicked",
                    activity::HubSessionCloseReason::ServiceStopped => "service_stopped",
                }
            ),
            activity::HubTransition::RoomJoined { members, .. } => {
                format!("room.joined:{members}")
            }
            activity::HubTransition::RoomParted { members, .. } => {
                format!("room.parted:{members}")
            }
            activity::HubTransition::RoomModerated { action, .. } => format!(
                "room.moderated:{}",
                match action {
                    activity::HubModerationAction::Register => "register",
                    activity::HubModerationAction::Unregister => "unregister",
                    activity::HubModerationAction::Topic => "topic",
                    activity::HubModerationAction::Mode => "mode",
                    activity::HubModerationAction::Op => "op",
                    activity::HubModerationAction::Deop => "deop",
                    activity::HubModerationAction::Voice => "voice",
                    activity::HubModerationAction::Devoice => "devoice",
                    activity::HubModerationAction::Ban => "ban",
                    activity::HubModerationAction::Unban => "unban",
                    activity::HubModerationAction::Kick => "kick",
                    activity::HubModerationAction::Invite => "invite",
                    activity::HubModerationAction::Uninvite => "uninvite",
                }
            ),
            activity::HubTransition::TrustChanged { change, .. } => format!(
                "trust.changed:{}",
                match change {
                    activity::HubTrustChange::KlineAdded => "kline_added",
                    activity::HubTrustChange::KlineRemoved => "kline_removed",
                }
            ),
            activity::HubTransition::RelayForwarded {
                method, recipients, ..
            } => format!(
                "relay.forwarded:{}:{recipients}",
                match method {
                    activity::ChannelEnvelopeKind::Message => "message",
                    activity::ChannelEnvelopeKind::Notice => "notice",
                    activity::ChannelEnvelopeKind::Action => "action",
                    _ => "other",
                }
            ),
            activity::HubTransition::RelayThrottled {
                rejected, dropped, ..
            } => format!("relay.throttled:{rejected}:{dropped}"),
        }
    }

    fn described(core: &mut HubCore) -> Vec<String> {
        lowered(core)
            .iter()
            .map(|(_, transition)| describe(transition))
            .collect()
    }

    fn room_tokens_of(core: &mut HubCore) -> Vec<activity::ChannelRoomToken> {
        lowered(core)
            .into_iter()
            .filter_map(|(_, transition)| match transition {
                activity::HubTransition::RoomJoined { room, .. }
                | activity::HubTransition::RoomParted { room, .. }
                | activity::HubTransition::RoomModerated { room, .. }
                | activity::HubTransition::RelayForwarded { room, .. } => Some(room),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn hub_activity_events_are_tokenized_and_stable() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_A, ID_A, "annex");
        let first = room_tokens_of(&mut core);
        assert_eq!(first.len(), 2);
        let lobby = first[0];
        let annex = first[1];
        assert!(lobby != annex, "two rooms never share a token");

        join(&mut core, LINK_B, ID_B, "lobby");
        let again = room_tokens_of(&mut core);
        assert!(again == vec![lobby], "a live room keeps its token");

        // Random, never derived from the label: a second hub disagrees about
        // the same room name.
        let mut elsewhere = op_core();
        join(&mut elsewhere, LINK_A, ID_A, "lobby");
        assert!(
            room_tokens_of(&mut elsewhere)[0] != lobby,
            "a token derived from the room name would collide across hubs"
        );

        // Removal site 1: the last member leaves an unregistered room.
        assert_eq!(core.room_tokens.len(), 2);
        for link in [LINK_A, LINK_B] {
            let mut part = rrc::Envelope::new(rrc::MessageType::Part, ID_A);
            part.room = Some("lobby".to_string());
            core.on_envelope(link, part, &mut Vec::new());
        }
        assert!(!core.rooms.contains_key("lobby"));
        assert!(!core.room_tokens.contains_key("lobby"));

        // Removal site 2: /unregister drops an empty registered room.
        run_command(&mut core, LINK_A, ID_A, "/register annex");
        let mut part = rrc::Envelope::new(rrc::MessageType::Part, ID_A);
        part.room = Some("annex".to_string());
        core.on_envelope(LINK_A, part, &mut Vec::new());
        assert!(
            core.room_tokens.contains_key("annex"),
            "registered rooms live on"
        );
        run_command(&mut core, LINK_A, ID_A, "/unregister annex");
        assert!(!core.rooms.contains_key("annex"));
        assert!(!core.room_tokens.contains_key("annex"));

        // Removal site 3: the registry prune.
        let mut pruned = core_with(ChannelHubConfig {
            room_registry_prune_after_secs: 10,
            room_registry_prune_interval_secs: 0,
            ..ChannelHubConfig::default()
        });
        welcomed_session(&mut pruned, LINK_A, ID_A, "alpha");
        join(&mut pruned, LINK_A, ID_A, "idle");
        run_command(&mut pruned, LINK_A, ID_A, "/register idle");
        let mut part = rrc::Envelope::new(rrc::MessageType::Part, ID_A);
        part.room = Some("idle".to_string());
        pruned.on_envelope(LINK_A, part, &mut Vec::new());
        pruned.rooms.get_mut("idle").unwrap().last_used = 1_800_000_000.0;
        assert!(pruned.room_tokens.contains_key("idle"));
        pruned.prune_registry(1_800_000_100.0, &mut Vec::new());
        assert!(!pruned.rooms.contains_key("idle"));
        assert!(
            !pruned.room_tokens.contains_key("idle"),
            "a pruned room must not leave its token behind"
        );
    }

    #[test]
    fn session_events_share_one_correlation_per_link() {
        let mut core = op_core();
        let opened = lowered(&mut core);
        assert_eq!(
            opened
                .iter()
                .filter(|(_, transition)| matches!(
                    transition,
                    activity::HubTransition::SessionOpened { .. }
                ))
                .count(),
            2,
            "one open per identified link"
        );
        let correlation_a = opened[0].0;
        assert!(correlation_a != opened[1].0, "sessions never share a token");
        match opened[0].1 {
            activity::HubTransition::SessionOpened { link, peer } => {
                assert!(link == activity::LinkId::new(LINK_A));
                assert!(peer == activity::IdentityHash::new(ID_A));
            }
            _ => panic!("expected session.opened"),
        }

        // Re-identification of a live link is not a second session.
        core.on_link_identified(LINK_A, ID_A, Instant::now(), &mut Vec::new());
        assert!(described(&mut core).is_empty());

        join(&mut core, LINK_A, ID_A, "lobby");
        let joined = lowered(&mut core);
        assert_eq!(joined.len(), 1);
        assert!(
            joined[0].0 == correlation_a,
            "room events ride the session correlation"
        );

        let mut out = Vec::new();
        core.on_link_closed(LINK_A, &mut out);
        let closed = lowered(&mut core);
        assert_eq!(
            closed.iter().map(|(_, t)| describe(t)).collect::<Vec<_>>(),
            vec!["session.closed:remote", "room.parted:0"]
        );
        assert!(closed.iter().all(|(id, _)| *id == correlation_a));

        // A reconnect on the same link id is a new session.
        core.on_link_established(LINK_A, Instant::now());
        core.on_link_identified(LINK_A, ID_A, Instant::now(), &mut Vec::new());
        let reopened = lowered(&mut core);
        assert_eq!(reopened.len(), 1);
        assert!(reopened[0].0 != correlation_a);
    }

    #[test]
    fn a_rejected_welcome_is_recorded_and_never_opens_a_session() {
        let (mut core, link_id) = identified_core(ChannelHubConfig {
            hub_name: "h".repeat(4096),
            ..ChannelHubConfig::default()
        });
        assert_eq!(described(&mut core), vec!["session.opened"]);
        core.on_envelope(
            link_id,
            rrc::Envelope::hello(ID_A, "rat", "1"),
            &mut Vec::new(),
        );
        assert_eq!(
            described(&mut core),
            vec!["session.rejected:welcome_unsendable"]
        );

        // A WELCOME that fits records nothing: the open already said it.
        let (mut core, link_id) = identified_core(ChannelHubConfig::default());
        lowered(&mut core);
        core.on_envelope(
            link_id,
            rrc::Envelope::hello(ID_A, "rat", "1"),
            &mut Vec::new(),
        );
        assert!(described(&mut core).is_empty());
    }

    #[test]
    fn a_reaped_link_leaves_no_ghost_membership() {
        // We tear the link down ourselves, so nothing else will do the
        // membership teardown. Leaving it to the transport's close event meant
        // rosters listed a dead link forever and the room never emptied.
        let config = ChannelHubConfig {
            ping_interval_secs: 1,
            ping_timeout_secs: 1,
            ..ChannelHubConfig::default()
        };
        let mut core = core_with(config);
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        welcomed_session(&mut core, LINK_B, ID_B, "beta");
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");
        assert_eq!(core.rooms.get("lobby").unwrap().members.len(), 2);

        let start = Instant::now();
        let mut out = Vec::new();
        core.ping_cycle(start, &mut out);
        out.clear();
        // A answers; B stays silent, so only B is reaped.
        let mut pong = rrc::Envelope::new(rrc::MessageType::Pong, ID_A);
        pong.body = Some(Value::Bytes(vec![0x01]));
        core.on_envelope(LINK_A, pong, &mut out);
        out.clear();
        core.ping_cycle(start + Duration::from_secs(30), &mut out);

        assert!(!core.sessions.contains_key(&LINK_B));
        assert!(
            !core.rooms.get("lobby").unwrap().members.contains(&LINK_B),
            "a reaped link must leave the rooms it joined"
        );
        assert!(
            !core.by_identity.contains_key(&ID_B),
            "a reaped link must not keep resolving as a command target"
        );
        // The survivor is told, exactly as for any other departure.
        let parted = sends_to(&out, LINK_A);
        assert!(
            parted
                .iter()
                .any(|env| env.message_type == rrc::MessageType::Parted
                    && rrc::member_identities(env) == vec![ID_B]),
            "remaining members must see the departure"
        );

        // And the room can now empty and be reclaimed.
        let mut part = rrc::Envelope::new(rrc::MessageType::Part, ID_A);
        part.room = Some("lobby".to_string());
        let mut out = Vec::new();
        core.on_envelope(LINK_A, part, &mut out);
        assert!(
            !core.rooms.contains_key("lobby"),
            "an unregistered room with no live members is reclaimed"
        );
    }

    #[test]
    fn handshake_reaping_survives_disabled_keepalive() {
        let config = ChannelHubConfig {
            ping_interval_secs: 0,
            ping_timeout_secs: 10,
            ..ChannelHubConfig::default()
        };
        let mut core = core_with(config);
        let now = Instant::now();
        core.on_link_established(LINK_A, now);
        core.on_link_identified(LINK_A, ID_A, now, &mut Vec::new());
        lowered(&mut core);

        let mut out = Vec::new();
        core.ping_cycle(now + Duration::from_secs(5), &mut out);
        assert!(out.is_empty(), "a fresh handshake is not reaped");
        assert!(core.sessions.contains_key(&LINK_A));

        let mut out = Vec::new();
        core.ping_cycle(now + Duration::from_secs(11), &mut out);
        assert!(
            matches!(out.as_slice(), [HubSend::Close { link_id }] if *link_id == LINK_A),
            "keepalive off must still reap a half-open link"
        );
        assert!(!core.sessions.contains_key(&LINK_A));
        assert_eq!(
            described(&mut core),
            vec!["session.closed:handshake_timed_out"]
        );

        // With keepalive on, an unanswered PING is the other reason.
        let mut core = core_with(ChannelHubConfig {
            ping_interval_secs: 1,
            ping_timeout_secs: 10,
            ..ChannelHubConfig::default()
        });
        let now = Instant::now();
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        lowered(&mut core);
        let mut out = Vec::new();
        core.ping_cycle(now, &mut out);
        assert_eq!(sends_to(&out, LINK_A).len(), 1, "a PING goes out");
        let mut out = Vec::new();
        core.ping_cycle(now + Duration::from_secs(11), &mut out);
        assert_eq!(described(&mut core), vec!["session.closed:ping_timed_out"]);
    }

    #[test]
    fn throttle_reports_are_bounded_and_survive_disabled_keepalive() {
        let mut core = core_with(ChannelHubConfig {
            ping_interval_secs: 0,
            ..ChannelHubConfig::default()
        });
        welcomed_session(&mut core, LINK_A, ID_A, "alpha");
        lowered(&mut core);
        let start = Instant::now();
        core.note_rate_limited();
        core.note_bad_packet();

        let mut out = Vec::new();
        core.ping_cycle(start + Duration::from_secs(30), &mut out);
        assert!(
            described(&mut core).is_empty(),
            "at most one report per minute"
        );
        assert!(out.is_empty(), "keepalive off sends no PING");

        core.ping_cycle(start + Duration::from_secs(61), &mut out);
        let reported = lowered(&mut core);
        assert_eq!(
            reported
                .iter()
                .map(|(_, t)| describe(t))
                .collect::<Vec<_>>(),
            vec!["relay.throttled:1:1"]
        );
        match reported[0].1 {
            activity::HubTransition::RelayThrottled { span_ms, .. } => {
                assert!(span_ms >= 60_000, "the window is reported, not guessed");
            }
            _ => panic!("expected relay.throttled"),
        }

        // A quiet window reports nothing at all.
        core.ping_cycle(start + Duration::from_secs(130), &mut out);
        assert!(described(&mut core).is_empty());

        // Counters are deltas, not totals.
        core.note_rate_limited();
        core.ping_cycle(start + Duration::from_secs(200), &mut out);
        assert_eq!(described(&mut core), vec!["relay.throttled:1:0"]);
        assert!(out.is_empty(), "keepalive off still sends no PING");
    }

    #[test]
    fn every_operator_command_names_its_verb_and_nothing_else() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");
        lowered(&mut core);

        // A refused command changes nothing, so it records nothing.
        run_command(&mut core, LINK_B, ID_B, "/register lobby");
        assert!(described(&mut core).is_empty());
        run_command(&mut core, LINK_A, ID_A, "/topic lobby");
        assert!(
            described(&mut core).is_empty(),
            "reading a topic is not moderation"
        );

        for (command, expected) in [
            ("/register lobby", "room.moderated:register"),
            ("/topic lobby hello", "room.moderated:topic"),
            ("/mode lobby +m", "room.moderated:mode"),
            ("/mode lobby +k hunter2secret", "room.moderated:mode"),
            ("/mode lobby -k", "room.moderated:mode"),
            ("/mode lobby +o beta", "room.moderated:op"),
            ("/mode lobby -o beta", "room.moderated:deop"),
            ("/mode lobby +v beta", "room.moderated:voice"),
            ("/mode lobby -v beta", "room.moderated:devoice"),
            ("/op lobby beta", "room.moderated:op"),
            ("/deop lobby beta", "room.moderated:deop"),
            ("/voice lobby beta", "room.moderated:voice"),
            ("/devoice lobby beta", "room.moderated:devoice"),
            ("/invite lobby add beta", "room.moderated:invite"),
            ("/invite lobby del beta", "room.moderated:uninvite"),
            ("/kick lobby beta", "room.moderated:kick"),
        ] {
            run_command(&mut core, LINK_A, ID_A, command);
            let events = described(&mut core);
            assert!(
                events.contains(&expected.to_string()),
                "`{command}` recorded {events:?}"
            );
        }

        join(&mut core, LINK_B, ID_B, "lobby");
        lowered(&mut core);
        for (command, expected) in [
            ("/ban lobby add beta", "room.moderated:ban"),
            (
                "/ban lobby del bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "room.moderated:unban",
            ),
            ("/unregister lobby", "room.moderated:unregister"),
        ] {
            run_command(&mut core, LINK_A, ID_A, command);
            let events = described(&mut core);
            assert!(
                events.contains(&expected.to_string()),
                "`{command}` recorded {events:?}"
            );
        }
    }

    #[test]
    fn kline_changes_are_recorded_and_close_the_session_as_kicked() {
        let mut core = op_core();
        lowered(&mut core);

        run_command(&mut core, LINK_A, ID_A, "/kline add beta");
        assert_eq!(
            described(&mut core),
            vec!["trust.changed:kline_added", "session.closed:kicked"]
        );

        let subject = hex::encode(ID_B);
        run_command(&mut core, LINK_A, ID_A, &format!("/kline del {subject}"));
        assert_eq!(described(&mut core), vec!["trust.changed:kline_removed"]);

        // Removing a kline that was never there is not a trust change.
        run_command(&mut core, LINK_A, ID_A, &format!("/kline del {subject}"));
        assert!(described(&mut core).is_empty());
    }

    #[test]
    fn relay_events_count_recipients_and_bytes() {
        let mut core = op_core();
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");
        lowered(&mut core);

        let mut message = rrc::Envelope::new(rrc::MessageType::Message, ID_B);
        message.room = Some("lobby".to_string());
        message.body = Some(Value::Text("hello".to_string()));
        let mut out = Vec::new();
        core.on_envelope(LINK_B, message, &mut out);
        let relayed = sends_to(&out, LINK_A);
        assert_eq!(relayed.len(), 1);
        let wire = rrc::encode(relayed[0]).expect("relayed envelope encodes");
        let events = lowered(&mut core);
        assert_eq!(
            events.iter().map(|(_, t)| describe(t)).collect::<Vec<_>>(),
            vec!["relay.forwarded:message:2"],
            "the sender is echoed, so both members count"
        );
        match events[0].1 {
            activity::HubTransition::RelayForwarded { encoded_bytes, .. } => {
                assert_eq!(encoded_bytes, wire.len() as u64);
            }
            _ => panic!("expected relay.forwarded"),
        }

        // A refused relay forwards nothing and records nothing.
        run_command(&mut core, LINK_A, ID_A, "/ban lobby add beta");
        lowered(&mut core);
        let mut message = rrc::Envelope::new(rrc::MessageType::Message, ID_B);
        message.room = Some("lobby".to_string());
        message.body = Some(Value::Text("blocked".to_string()));
        let mut out = Vec::new();
        core.on_envelope(LINK_B, message, &mut out);
        assert_eq!(error_texts(&out, LINK_B), vec!["banned from room"]);
        assert!(described(&mut core).is_empty());
    }

    #[test]
    fn a_resource_notice_records_one_relay_for_the_whole_payload() {
        let mut core = resource_room(accepting_config());
        lowered(&mut core);
        let payload = vec![b'r'; 1_200];
        announce(&mut core, LINK_B, [0x31; 8], &payload, "lobby");
        assert!(
            described(&mut core).is_empty(),
            "an announcement is not a relay"
        );

        let mut out = Vec::new();
        core.on_resource_completed(LINK_B, [0x31; 32], payload, false, &mut out);
        let chunks = relayed_texts(&out, LINK_A);
        assert!(chunks.len() > 1, "1200 bytes needs more than one packet");
        let bytes: usize = sends_to(&out, LINK_A)
            .into_iter()
            .map(|envelope| rrc::encode(envelope).expect("encodes").len())
            .sum();
        let events = lowered(&mut core);
        assert_eq!(
            events.iter().map(|(_, t)| describe(t)).collect::<Vec<_>>(),
            vec!["relay.forwarded:notice:2"],
            "chunking is our MDU concern, not a second relay"
        );
        match events[0].1 {
            activity::HubTransition::RelayForwarded { encoded_bytes, .. } => {
                assert_eq!(encoded_bytes, bytes as u64);
            }
            _ => panic!("expected relay.forwarded"),
        }
    }

    #[test]
    fn room_events_follow_membership_not_traffic() {
        let mut core = op_core();
        lowered(&mut core);
        join(&mut core, LINK_A, ID_A, "lobby");
        join(&mut core, LINK_B, ID_B, "lobby");
        assert_eq!(described(&mut core), vec!["room.joined:1", "room.joined:2"]);

        // A PART naming a room that does not exist touches no membership.
        let mut part = rrc::Envelope::new(rrc::MessageType::Part, ID_B);
        part.room = Some("nowhere".to_string());
        core.on_envelope(LINK_B, part, &mut Vec::new());
        assert!(described(&mut core).is_empty());

        let part_lobby = |core: &mut HubCore| {
            let mut part = rrc::Envelope::new(rrc::MessageType::Part, ID_B);
            part.room = Some("lobby".to_string());
            core.on_envelope(LINK_B, part, &mut Vec::new());
        };
        part_lobby(&mut core);
        assert_eq!(described(&mut core), vec!["room.parted:1"]);

        // A repeat PART is not a second departure.
        part_lobby(&mut core);
        assert!(described(&mut core).is_empty());
    }

    #[test]
    fn service_events_bracket_the_run_and_report_degradation() {
        let mut core = op_core();
        let sessions = lowered(&mut core);
        let session_correlations: Vec<CorrelationId> =
            sessions.into_iter().map(|(id, _)| id).collect();

        core.note_service_started();
        core.note_service_degraded(activity::HubServiceDegradation::Announce, 1);
        core.note_oversize(2);
        core.note_send_failed(3);
        // A zero count is not a degradation.
        core.note_service_degraded(activity::HubServiceDegradation::SendFailed, 0);
        core.note_service_stopped();

        let events = lowered(&mut core);
        let mut described: Vec<String> = events.iter().map(|(_, t)| describe(t)).collect();
        described.sort();
        assert_eq!(
            described,
            vec![
                "service.degraded:announce:1",
                "service.degraded:envelope_oversize:2",
                "service.degraded:send_failed:3",
                "service.started",
                "service.stopped",
                "session.closed:service_stopped",
                "session.closed:service_stopped",
            ]
        );

        let run_correlations: Vec<CorrelationId> = events
            .iter()
            .filter(|(_, transition)| {
                !matches!(transition, activity::HubTransition::SessionClosed { .. })
            })
            .map(|(id, _)| *id)
            .collect();
        assert!(
            run_correlations.iter().all(|id| *id == run_correlations[0]),
            "service events share one correlation per hub run"
        );
        assert!(
            session_correlations
                .iter()
                .all(|id| *id != run_correlations[0]),
            "the hub run is not one of its sessions"
        );
    }
}
