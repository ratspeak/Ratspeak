//! Live Reticulum Relay Chat sessions.
//!
//! Observed room membership remains session-scoped, while user connection and
//! room intent is durable and explicitly separate. Accepted transcript items
//! enter a bounded client-local append log; they are never routed through the
//! LXMF conversation store or requested as backlog from a constrained hub.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::pending;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ciborium::value::Value;
use ratspeak_core::Emitter;
use rns_identity::{destination::Destination, identity::Identity};
use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::link_session::{
    LinkSession, LinkSessionCloseReason, LinkSessionConfig, LinkSessionError, LinkSessionEvent,
    LinkSessionReceivedResource, LinkSessionResourceOffer,
};
use rns_transport::messages::{
    AnnounceRpcEntry, TransportMessage, TransportQuery, TransportQueryResponse,
};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use zeroize::{Zeroize, Zeroizing};

use crate::activity::{CorrelationId, producer as activity};
use crate::db;
use crate::rrc::{self, Envelope, HubLimits, MessageType, WelcomeInfo};
use crate::state::{ActivityRequestFence, AppState};

const COMMAND_BUFFER: usize = 64;
const CONNECT_UPDATE_BUFFER: usize = 32;
const GREETING_RESOURCE_COMPLETION_BUFFER: usize = 4;
const CONNECT_PATH_TIMEOUT: Duration = Duration::from_secs(30);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(15);
const HUB_GREETING_WINDOW: Duration = Duration::from_secs(30);
const HUB_GREETING_RESOURCE_MAX_BYTES: usize = 16 * 1024;
const HUB_GREETING_RESOURCE_TRANSFER_SLACK: usize = 256;
const HUB_GREETING_RESOURCE_TIMEOUT: Duration = Duration::from_secs(30);
const HUB_GREETING_NOTICE_MDU_SLACK: usize = 64;
const JOIN_CONFIRM_TIMEOUT: Duration = Duration::from_secs(30);
const PART_CONFIRM_TIMEOUT: Duration = Duration::from_secs(15);
const ROOM_TRANSITION_TICK: Duration = Duration::from_secs(1);
const DIRECTORY_REFRESH_TIMEOUT: Duration = Duration::from_secs(15);
const DIRECTORY_REFRESH_COOLDOWN: Duration = Duration::from_secs(5);
const DIRECTORY_MAX_RESPONSE_BYTES: usize = 16 * 1024;
const DIRECTORY_MAX_ROOMS: usize = 256;
const DIRECTORY_MAX_TOPIC_BYTES: usize = 512;
const DEFAULT_NICK_MAX_BYTES: usize = 32;
const DEFAULT_ROOM_MAX_BYTES: usize = 64;
const DEFAULT_MESSAGE_MAX_BYTES: usize = 350;
const LXMF_DELIVERY_ASPECT: &str = "lxmf.delivery";
const TRANSCRIPT_LIMIT: usize = 300;
const NOTICE_LIMIT: usize = 100;
const SEEN_MESSAGE_LIMIT: usize = 2_048;
const RECONNECT_BASE_DELAY: Duration = Duration::from_secs(2);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(5 * 60);
const RECONNECT_STABLE_RESET: Duration = Duration::from_secs(2 * 60);
const RECONNECT_JITTER_PERCENT: u32 = 20;
const AUTO_REJOIN_ROOM_LIMIT: usize = 32;
// History is auxiliary and must not put authenticated Link processing at the
// mercy of a stalled local disk. Together with the DB's per-event input limit,
// this caps queued transcript payload at roughly 8 MiB.
const HISTORY_COMMAND_BUFFER: usize = 128;
const PARTICIPANT_OBSERVATION_QUEUE_LIMIT: usize = 256;
const HISTORY_RETRY_DELAY: Duration = Duration::from_secs(1);
const HISTORY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const HISTORY_BARRIER_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_JOIN_KEY_BYTES: usize = 1_024;
const MAX_SEALED_ROOM_SECRET_BYTES: usize = 4_096;
const ROOM_SECRET_SEAL_SCHEME: &str = "rns_identity";
const ROOM_SECRET_SEAL_VERSION: u32 = 1;
const ROOM_SECRET_MAGIC: &[u8; 8] = b"RSCHKEY\0";
const ROOM_SECRET_FORMAT_VERSION: u8 = 1;
const BAD_ROOM_KEY_ERROR: &str = "bad key (+k)";
const SAVED_ROOM_KEY_REJECTED: &str = "Saved join key was rejected. Enter the current key.";
const ENTERED_ROOM_KEY_REJECTED: &str = "Channel key was rejected. Check it and try again.";
const SAVED_ROOM_KEY_UNAVAILABLE: &str = "Saved join key is unavailable. Enter the current key.";
const ROOM_KEY_REQUIRED: &str = "Channel key required. Enter the current key.";
/// Initial scheduler budget. The hub-keyed model deliberately supports raising
/// this later without pretending the current runtime holds multiple Links.
pub const CHANNELS_CONNECTION_BUDGET: usize = 1;
pub const CHANNELS_SERVICE_MODEL_VERSION: u16 = 3;

// Snapshot ordering crosses two asynchronous delivery paths: direct Tauri
// command responses and live `channels_snapshot` events. A process-local
// generation separates manager lifetimes (notably identity switches), while
// each manager's revision orders state within that lifetime. Wall time remains
// diagnostic only: two mutations can share a millisecond and clocks can move.
static NEXT_CHANNELS_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_channels_generation() -> u64 {
    NEXT_CHANNELS_GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn lxmf_destination_hash(identity_hash: [u8; 16]) -> String {
    hex::encode(Destination::hash_from_name_and_identity(
        LXMF_DELIVERY_ASPECT,
        Some(&identity_hash),
    ))
}

/// Derive the canonical LXMF delivery destination used by Ratspeak avatars
/// from a Reticulum identity hash supplied by an authenticated RRC Link.
pub fn lxmf_destination_hash_from_identity_hex(identity_hash: &str) -> Option<String> {
    let bytes = hex::decode(identity_hash).ok()?;
    let identity_hash: [u8; 16] = bytes.try_into().ok()?;
    Some(lxmf_destination_hash(identity_hash))
}

/// Fenced Activity recorder shared with the hub service: both sides of
/// Channels record through the same origin-fence logic rather than two copies.
#[derive(Clone)]
pub(crate) struct ChannelsActivity {
    state: Weak<AppState>,
}

impl ChannelsActivity {
    pub(crate) fn new(state: Weak<AppState>) -> Self {
        Self { state }
    }

    fn capture_fence(&self) -> Option<ActivityRequestFence> {
        self.state
            .upgrade()
            .map(|state| state.activity_request_fence())
    }

    fn record_fenced<F>(&self, fence: Option<ActivityRequestFence>, make: F)
    where
        F: FnOnce() -> activity::ProducerEvent,
    {
        let (Some(state), Some(fence)) = (self.state.upgrade(), fence) else {
            return;
        };
        let validation_state = Arc::clone(&state);
        let _ = state.activity.record_event_fenced(
            move || validation_state.is_current_activity_origin_fence(fence),
            move || Ok(make()),
        );
    }

    pub(crate) fn record_spontaneous<F>(&self, make: F)
    where
        F: FnOnce() -> activity::ProducerEvent,
    {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let fence = state.activity_request_fence();
        let validation_state = Arc::clone(&state);
        let _ = state.activity.record_event_fenced(
            move || validation_state.is_current_activity_origin_fence(fence),
            move || Ok(make()),
        );
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelsPhase {
    Unavailable,
    #[default]
    Offline,
    Resolving,
    Connecting,
    AwaitingWelcome,
    Reconnecting,
    Active,
    Stale,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRoomPhase {
    Joining,
    Joined,
    Parting,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelItemKind {
    Message,
    Notice,
    Action,
    Join,
    Part,
    Error,
    System,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelHubLimitsSnapshot {
    pub max_nick_bytes: Option<usize>,
    pub max_room_name_bytes: Option<usize>,
    pub max_message_body_bytes: Option<usize>,
    pub max_rooms_per_session: Option<usize>,
    pub rate_messages_per_minute: Option<usize>,
}

impl From<&HubLimits> for ChannelHubLimitsSnapshot {
    fn from(limits: &HubLimits) -> Self {
        Self {
            max_nick_bytes: limits.max_nick_bytes,
            max_room_name_bytes: limits.max_room_name_bytes,
            max_message_body_bytes: limits.max_message_body_bytes,
            max_rooms_per_session: limits.max_rooms_per_session,
            rate_messages_per_minute: limits.rate_messages_per_minute,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelHubCapabilitiesSnapshot {
    pub actions: bool,
    pub direct_notices: bool,
    /// The hub negotiated bounded Resource envelopes. Ratspeak currently
    /// accepts only authenticated roomless `motd` guidance under a 16 KiB cap.
    pub resource_envelopes: bool,
    /// The hub can retain a short identity-bound `+i` rejoin grant. This says
    /// nothing about `+k`; a reconnect still needs the room key.
    pub rejoin_grace: bool,
}

impl From<&WelcomeInfo> for ChannelHubCapabilitiesSnapshot {
    fn from(welcome: &WelcomeInfo) -> Self {
        Self {
            actions: welcome
                .capabilities
                .get(&rrc::CAP_ACTION)
                .copied()
                .unwrap_or(false),
            direct_notices: welcome
                .capabilities
                .get(&rrc::CAP_DIRECT_NOTICE)
                .copied()
                .unwrap_or(false),
            resource_envelopes: welcome
                .capabilities
                .get(&rrc::CAP_RESOURCE_ENVELOPE)
                .copied()
                .unwrap_or(false),
            rejoin_grace: welcome
                .capabilities
                .get(&rrc::CAP_REJOIN_GRACE)
                .copied()
                .unwrap_or(false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelHubSnapshot {
    pub destination_hash: String,
    pub identity_hash: Option<String>,
    pub announced_name: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub hops: Option<u8>,
    pub link_mdu: Option<usize>,
    pub connected_at_ms: Option<u64>,
    pub capabilities: ChannelHubCapabilitiesSnapshot,
    pub limits: ChannelHubLimitsSnapshot,
}

impl ChannelHubSnapshot {
    fn pending(destination_hash: [u8; 16]) -> Self {
        Self {
            destination_hash: hex::encode(destination_hash),
            identity_hash: None,
            announced_name: None,
            name: None,
            version: None,
            hops: None,
            link_mdu: None,
            connected_at_ms: None,
            capabilities: ChannelHubCapabilitiesSnapshot::default(),
            limits: ChannelHubLimitsSnapshot::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelMemberSnapshot {
    /// Stable Reticulum identity hash from a hub roster or the reported source
    /// of observed room content. Some hubs omit both, leaving only a nickname.
    pub identity_hash: Option<String>,
    /// Canonical `lxmf.delivery` destination derived from `identity_hash`.
    pub lxmf_hash: Option<String>,
    pub nickname: Option<String>,
    pub is_self: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelTranscriptItem {
    pub id: String,
    pub kind: ChannelItemKind,
    pub timestamp_ms: u64,
    pub source_hash: Option<String>,
    /// Canonical `lxmf.delivery` destination derived from `source_hash`.
    pub source_lxmf_hash: Option<String>,
    pub nickname: Option<String>,
    pub text: String,
    pub ours: bool,
    pub mentioned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelHubGreetingDelivery {
    /// One or more roomless NOTICE packets. RRC does not frame a NOTICE burst,
    /// so this is useful guidance but not proof that every configured byte
    /// arrived.
    Notice,
    /// A bounded Resource whose announced and delivered bytes were validated.
    Resource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelHubGreetingCompleteness {
    /// The transfer supplied an exact byte count and completed validation.
    Complete,
    /// Packet fallback has no protocol-level final-fragment marker.
    Unframed,
}

/// Authenticated Link-scoped hub guidance, deliberately separate from room
/// transcript and local history. It is neither a room message nor a claim that
/// the client possesses a formally versioned rules document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelHubGreetingSnapshot {
    pub text: String,
    pub received_at_ms: u64,
    pub source_hash: String,
    pub delivery: ChannelHubGreetingDelivery,
    pub completeness: ChannelHubGreetingCompleteness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelRoomSnapshot {
    pub name: String,
    pub phase: ChannelRoomPhase,
    /// Wall-clock time when the current room transition began. This lets the
    /// UI explain an in-flight JOIN/PART without persisting session state.
    pub phase_started_at_ms: u64,
    pub members: Vec<ChannelMemberSnapshot>,
    /// False means the hub did not advertise the optional JOINED member list;
    /// the visible members are then best-effort live observations only.
    pub members_complete: bool,
    /// Hub-local room metadata reported after JOIN. These fields are advisory:
    /// RRC itself does not standardize room registration, modes, or topics.
    pub registered: Option<bool>,
    pub modes: Option<String>,
    pub topic: Option<String>,
    pub transcript: Vec<ChannelTranscriptItem>,
    pub last_error: Option<String>,
}

impl ChannelRoomSnapshot {
    fn joining(name: String) -> Self {
        Self {
            name,
            phase: ChannelRoomPhase::Joining,
            phase_started_at_ms: now_ms(),
            members: Vec::new(),
            members_complete: false,
            registered: None,
            modes: None,
            topic: None,
            transcript: Vec::new(),
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelsDurabilityPhase {
    #[default]
    Loading,
    Ready,
    Degraded,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRecoveryPhase {
    #[default]
    Idle,
    Scheduled,
    Connecting,
    Rejoining,
    Blocked,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelHubRecoverySnapshot {
    pub phase: ChannelRecoveryPhase,
    /// Consecutive failed attempts or short-lived sessions.
    pub attempt: u32,
    pub next_attempt_at_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelsDurabilitySnapshot {
    pub phase: ChannelsDurabilityPhase,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelsHistoryPhase {
    #[default]
    Loading,
    Ready,
    Pending,
    Degraded,
    Unavailable,
}

/// Health of the independent local append log. It is deliberately separate
/// from bookmark durability: a bookmark can save while transcript writes are
/// retrying, and neither status is proof of live hub membership.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelsHistorySnapshot {
    pub phase: ChannelsHistoryPhase,
    pub pending_events: usize,
    pub dropped_events: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelDesiredRoomSnapshot {
    pub name: String,
    pub joined: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelHubDesiredSnapshot {
    /// User intent, not proof that a Link currently exists.
    pub connected: bool,
    pub nickname: Option<String>,
    pub rooms: Vec<ChannelDesiredRoomSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelDurableRoomSnapshot {
    pub name: String,
    pub added_at_ms: u64,
    pub last_joined_at_ms: u64,
    pub desired_joined: bool,
    pub join_key_required: bool,
    /// Availability only. Ciphertext and seal metadata never cross IPC.
    pub has_stored_join_key: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelHubDurableSnapshot {
    pub saved: bool,
    pub label: String,
    pub nickname: String,
    pub added_at_ms: u64,
    pub last_connected_at_ms: u64,
    pub rooms: Vec<ChannelDurableRoomSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelObservedRoomSnapshot {
    pub name: String,
    pub phase: ChannelRoomPhase,
    pub member_count: usize,
    pub members_complete: bool,
    pub registered: Option<bool>,
    pub modes: Option<String>,
    pub topic: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRoomDirectoryPhase {
    #[default]
    Idle,
    Loading,
    Ready,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelDirectoryRoomSnapshot {
    pub name: String,
    pub topic: Option<String>,
}

/// A Link-scoped interpretation of the reference `/list` NOTICE. It is
/// observation only: never a bookmark, membership claim, or hub backlog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelRoomDirectorySnapshot {
    pub phase: ChannelRoomDirectoryPhase,
    pub rooms: Vec<ChannelDirectoryRoomSnapshot>,
    /// False when a constrained hub explicitly reports that entries were
    /// omitted from its single-packet compatibility response.
    pub complete: bool,
    pub omitted_count: usize,
    pub refreshed_at_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelHubObservedSnapshot {
    /// Network observation, never a desired-state alias.
    pub phase: ChannelsPhase,
    pub nickname: Option<String>,
    pub hub: ChannelHubSnapshot,
    pub rooms: Vec<ChannelObservedRoomSnapshot>,
    pub directory: ChannelRoomDirectorySnapshot,
    /// Link-scoped authenticated welcome/guidance for this exact hub.
    pub greeting: Option<ChannelHubGreetingSnapshot>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelClientHubStateSnapshot {
    pub destination_hash: String,
    pub desired: ChannelHubDesiredSnapshot,
    pub observed: Option<ChannelHubObservedSnapshot>,
    pub durable: ChannelHubDurableSnapshot,
    pub recovery: ChannelHubRecoverySnapshot,
}

impl ChannelClientHubStateSnapshot {
    fn new(destination_hash: String) -> Self {
        Self {
            destination_hash,
            desired: ChannelHubDesiredSnapshot::default(),
            observed: None,
            durable: ChannelHubDurableSnapshot::default(),
            recovery: ChannelHubRecoverySnapshot::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelsSnapshot {
    pub protocol_version: &'static str,
    pub service_model_version: u16,
    /// Process-local manager lifetime. Higher generations supersede every
    /// snapshot emitted by a retired manager.
    pub generation: u64,
    /// Strictly increasing state version within one manager generation.
    pub revision: u64,
    /// Hard scheduler ceiling, separate from how many hubs are remembered.
    pub connection_budget: usize,
    /// The hub the user wants the scheduler to service. It survives an
    /// unexpected Link close; explicit disconnect clears it.
    pub selected_hub_destination: Option<String>,
    /// Hub-keyed desired/observed/durable service state. The legacy live
    /// projection below remains during the frontend migration.
    pub hubs: Vec<ChannelClientHubStateSnapshot>,
    pub durability: ChannelsDurabilitySnapshot,
    pub history: ChannelsHistorySnapshot,
    pub phase: ChannelsPhase,
    pub nickname: Option<String>,
    pub hub: Option<ChannelHubSnapshot>,
    pub rooms: Vec<ChannelRoomSnapshot>,
    /// Public rooms currently advertised by the authenticated hub. This is
    /// cleared with the Link and is intentionally never persisted.
    pub directory: ChannelRoomDirectorySnapshot,
    /// Authenticated roomless hub guidance after WELCOME. Resource delivery is
    /// complete; NOTICE fallback remains explicitly unframed.
    pub hub_greeting: Option<ChannelHubGreetingSnapshot>,
    pub notices: Vec<ChannelTranscriptItem>,
    pub last_error: Option<String>,
    pub updated_at_ms: u64,
}

impl ChannelsSnapshot {
    pub fn offline() -> Self {
        Self {
            protocol_version: "0.1.3",
            service_model_version: CHANNELS_SERVICE_MODEL_VERSION,
            generation: 0,
            revision: 0,
            connection_budget: CHANNELS_CONNECTION_BUDGET,
            selected_hub_destination: None,
            hubs: Vec::new(),
            durability: ChannelsDurabilitySnapshot {
                phase: ChannelsDurabilityPhase::Ready,
                last_error: None,
            },
            history: ChannelsHistorySnapshot {
                phase: ChannelsHistoryPhase::Unavailable,
                ..ChannelsHistorySnapshot::default()
            },
            phase: ChannelsPhase::Offline,
            nickname: None,
            hub: None,
            rooms: Vec::new(),
            directory: ChannelRoomDirectorySnapshot::default(),
            hub_greeting: None,
            notices: Vec::new(),
            last_error: None,
            updated_at_ms: now_ms(),
        }
    }

    pub fn unavailable() -> Self {
        Self {
            phase: ChannelsPhase::Unavailable,
            last_error: Some("Reticulum is not ready".into()),
            ..Self::offline()
        }
    }

    fn for_manager(generation: u64) -> Self {
        Self {
            generation,
            revision: 1,
            durability: ChannelsDurabilitySnapshot::default(),
            history: ChannelsHistorySnapshot::default(),
            ..Self::offline()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiscoveredChannelHub {
    pub destination_hash: String,
    pub identity_hash: Option<String>,
    pub announced_name: Option<String>,
    pub hops: u8,
    pub last_seen: f64,
    pub is_path_response: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelsError {
    #[error("Channels runtime is unavailable")]
    Unavailable,
    #[error("channel hub destination must be a 32-character hexadecimal hash")]
    InvalidDestination,
    #[error("not connected to a channel hub")]
    NotConnected,
    #[error("the channel hub is still connecting")]
    AlreadyConnecting,
    #[error("not joined to room {0}")]
    NotJoined(String),
    #[error("room {0} is already being joined")]
    AlreadyJoining(String),
    #[error("message must not be empty")]
    EmptyMessage,
    #[error("message exceeds the hub's {0}-byte limit")]
    MessageTooLong(usize),
    #[error("channel key exceeds the {0}-byte client limit")]
    JoinKeyTooLong(usize),
    #[error("saved join key for {0} is unavailable; enter the current key")]
    SavedJoinKeyUnavailable(String),
    #[error("the hub's room limit has been reached")]
    RoomLimitReached,
    #[error("channel hub rejected the session: {0}")]
    HubRejected(String),
    #[error("channel protocol error: {0}")]
    Protocol(String),
    #[error("channel transport error: {0}")]
    Transport(String),
    #[error("local channel history is not ready")]
    LocalHistoryUnavailable,
    #[error("Channels runtime stopped")]
    Stopped,
}

impl From<rrc::ProtocolError> for ChannelsError {
    fn from(error: rrc::ProtocolError) -> Self {
        Self::Protocol(error.to_string())
    }
}

impl From<LinkSessionError> for ChannelsError {
    fn from(error: LinkSessionError) -> Self {
        Self::Transport(error.to_string())
    }
}

enum ChannelsCommand {
    Discover {
        result_tx: oneshot::Sender<Result<Vec<DiscoveredChannelHub>, ChannelsError>>,
    },
    Connect {
        destination_hash: [u8; 16],
        nickname: String,
        known_hub: Option<KnownHubTarget>,
        activity_fence: Option<ActivityRequestFence>,
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    Disconnect {
        activity_fence: Option<ActivityRequestFence>,
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    /// The local identity was renamed. RRC carries nicknames inline rather
    /// than as a rename verb, so adopting the new name here is enough for the
    /// hub and every room member to see it on the next envelope.
    IdentityRenamed {
        previous: String,
        current: String,
    },
    Join {
        room: String,
        key: Option<Zeroizing<String>>,
        remember_key: bool,
        activity_fence: Option<ActivityRequestFence>,
        result_tx: oneshot::Sender<Result<String, ChannelsError>>,
    },
    Part {
        room: String,
        activity_fence: Option<ActivityRequestFence>,
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    Send {
        room: String,
        text: String,
        activity_fence: Option<ActivityRequestFence>,
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    RefreshDirectory {
        activity_fence: Option<ActivityRequestFence>,
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    RefreshDurable {
        result_tx: oneshot::Sender<()>,
    },
    FlushHistory {
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    Shutdown {
        activity_fence: Option<ActivityRequestFence>,
        result_tx: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
struct KnownHubTarget {
    public_key: [u8; 64],
    identity_hash: [u8; 16],
    announced_name: Option<String>,
    hops: u8,
}

#[derive(Clone)]
struct ChannelsStore {
    pool: db::DbPool,
    identity_id: String,
}

struct DurableChannelsState {
    hubs: Vec<db::SavedChannelHub>,
    rooms: Vec<db::SavedChannelRoom>,
    secrets: Vec<db::StoredChannelRoomSecret>,
}

type StoredRoomSecrets = BTreeMap<(String, String), db::StoredChannelRoomSecret>;

struct ChannelsManagerInput {
    transport_tx: mpsc::Sender<TransportMessage>,
    identity: Identity,
    emitter: Arc<dyn Emitter>,
    shutdown: ShutdownSignal,
    command_rx: mpsc::Receiver<ChannelsCommand>,
    snapshot: Arc<RwLock<ChannelsSnapshot>>,
    activity: ChannelsActivity,
    store: Option<ChannelsStore>,
    app_state: Weak<AppState>,
}

impl ChannelsStore {
    fn new(pool: db::DbPool, identity_id: String) -> Self {
        Self { pool, identity_id }
    }

    async fn load(&self) -> Result<DurableChannelsState, String> {
        let pool = self.pool.clone();
        let identity_id = self.identity_id.clone();
        db::spawn_db(pool, move |pool| load_durable_channels(&pool, &identity_id))
            .await
            .map_err(|_| "Channels state database task panicked".to_string())?
    }

    async fn set_hub_desired(
        &self,
        destination_hash: String,
        nickname: String,
        desired: bool,
    ) -> Result<DurableChannelsState, String> {
        let pool = self.pool.clone();
        let identity_id = self.identity_id.clone();
        db::spawn_db(pool, move |pool| {
            db::set_channel_hub_desired(
                &pool,
                &identity_id,
                &destination_hash,
                &nickname,
                desired,
            )?;
            load_durable_channels(&pool, &identity_id)
        })
        .await
        .map_err(|_| "Channels state database task panicked".to_string())?
    }

    async fn set_room_desired(
        &self,
        destination_hash: String,
        room: String,
        desired: bool,
    ) -> Result<DurableChannelsState, String> {
        let pool = self.pool.clone();
        let identity_id = self.identity_id.clone();
        db::spawn_db(pool, move |pool| {
            db::set_channel_room_desired(&pool, &identity_id, &destination_hash, &room, desired)?;
            load_durable_channels(&pool, &identity_id)
        })
        .await
        .map_err(|_| "Channels state database task panicked".to_string())?
    }

    async fn save_room_secret(
        &self,
        destination_hash: String,
        room: String,
        ciphertext: Vec<u8>,
    ) -> Result<DurableChannelsState, String> {
        let pool = self.pool.clone();
        let identity_id = self.identity_id.clone();
        db::spawn_db(pool, move |pool| {
            db::save_channel_room_secret(
                &pool,
                &identity_id,
                &destination_hash,
                &room,
                ROOM_SECRET_SEAL_SCHEME,
                ROOM_SECRET_SEAL_VERSION,
                &ciphertext,
            )?;
            load_durable_channels(&pool, &identity_id)
        })
        .await
        .map_err(|_| "Channels state database task panicked".to_string())?
    }

    async fn remove_room_secret(
        &self,
        destination_hash: String,
        room: String,
    ) -> Result<DurableChannelsState, String> {
        let pool = self.pool.clone();
        let identity_id = self.identity_id.clone();
        db::spawn_db(pool, move |pool| {
            db::remove_channel_room_secret(&pool, &identity_id, &destination_hash, &room, true)?;
            load_durable_channels(&pool, &identity_id)
        })
        .await
        .map_err(|_| "Channels state database task panicked".to_string())?
    }

    async fn mark_room_key_required(
        &self,
        destination_hash: String,
        room: String,
    ) -> Result<DurableChannelsState, String> {
        let pool = self.pool.clone();
        let identity_id = self.identity_id.clone();
        db::spawn_db(pool, move |pool| {
            db::mark_channel_room_key_required(&pool, &identity_id, &destination_hash, &room)?;
            load_durable_channels(&pool, &identity_id)
        })
        .await
        .map_err(|_| "Channels state database task panicked".to_string())?
    }

    async fn note_connected(
        &self,
        destination_hash: String,
        label: String,
        nickname: String,
    ) -> Result<DurableChannelsState, String> {
        let pool = self.pool.clone();
        let identity_id = self.identity_id.clone();
        db::spawn_db(pool, move |pool| {
            db::save_channel_hub(
                &pool,
                &identity_id,
                &destination_hash,
                &label,
                &nickname,
                true,
            )?;
            load_durable_channels(&pool, &identity_id)
        })
        .await
        .map_err(|_| "Channels state database task panicked".to_string())?
    }

    async fn append_history(
        &self,
        events: Arc<Vec<db::NewChannelHistoryEvent>>,
    ) -> Result<db::ChannelHistoryAppendOutcome, String> {
        let pool = self.pool.clone();
        let identity_id = self.identity_id.clone();
        db::spawn_db(pool, move |pool| {
            db::append_channel_history_events(&pool, &identity_id, events.as_slice())
        })
        .await
        .map_err(|_| "Channels history database task panicked".to_string())?
    }

    async fn remember_participants(
        &self,
        observations: Arc<Vec<db::NewChannelParticipantObservation>>,
    ) -> Result<usize, String> {
        let pool = self.pool.clone();
        let identity_id = self.identity_id.clone();
        db::spawn_db(pool, move |pool| {
            db::remember_channel_participants(&pool, &identity_id, observations.as_slice())
        })
        .await
        .map_err(|_| "Channels participant database task panicked".to_string())?
    }

    async fn prune_history(&self) -> Result<usize, String> {
        let pool = self.pool.clone();
        db::spawn_db(pool, move |pool| db::prune_expired_channel_history(&pool))
            .await
            .map_err(|_| "Channels history database task panicked".to_string())?
    }

    async fn unread_summary(&self) -> Result<db::ChannelUnreadSummary, String> {
        let pool = self.pool.clone();
        let identity_id = self.identity_id.clone();
        db::spawn_db(pool, move |pool| {
            db::get_channel_unread_summary(&pool, &identity_id)
        })
        .await
        .map_err(|_| "Channels unread database task panicked".to_string())?
    }
}

fn load_durable_channels(
    pool: &db::DbPool,
    identity_id: &str,
) -> Result<DurableChannelsState, String> {
    Ok(DurableChannelsState {
        hubs: db::list_saved_channel_hubs(pool, identity_id)?,
        rooms: db::list_saved_channel_rooms_for_identity(pool, identity_id)?,
        secrets: db::list_channel_room_secrets_for_identity(pool, identity_id)?,
    })
}

const HISTORY_TEMPORARILY_UNAVAILABLE: &str =
    "Local channel history is temporarily unavailable. New activity will retry automatically.";
const HISTORY_EVENTS_DROPPED: &str =
    "Some recent channel activity could not be saved to local history.";

enum ChannelHistoryCommand {
    Append(db::NewChannelHistoryEvent),
    Observe(db::NewChannelParticipantObservation),
    Barrier { result_tx: oneshot::Sender<()> },
    Shutdown { result_tx: oneshot::Sender<()> },
}

struct ChannelHistoryWorkerContext {
    status: Arc<RwLock<ChannelsHistorySnapshot>>,
    stopping: Arc<AtomicBool>,
    snapshot: Arc<RwLock<ChannelsSnapshot>>,
    emitter: Arc<dyn Emitter>,
    app_state: Weak<AppState>,
    identity_generation: Option<u64>,
}

#[derive(Clone)]
struct ChannelHistoryWriter {
    command_tx: Option<mpsc::Sender<ChannelHistoryCommand>>,
    identity_id: String,
    status: Arc<RwLock<ChannelsHistorySnapshot>>,
    stopping: Arc<AtomicBool>,
}

impl ChannelHistoryWriter {
    fn start(
        store: Option<ChannelsStore>,
        snapshot: Arc<RwLock<ChannelsSnapshot>>,
        emitter: Arc<dyn Emitter>,
        app_state: Weak<AppState>,
    ) -> Self {
        let Some(store) = store else {
            let status = ChannelsHistorySnapshot {
                phase: ChannelsHistoryPhase::Unavailable,
                ..ChannelsHistorySnapshot::default()
            };
            mutate_snapshot_if_changed(&snapshot, |state| {
                if state.history == status {
                    return false;
                }
                state.history = status.clone();
                true
            });
            return Self {
                command_tx: None,
                identity_id: String::new(),
                status: Arc::new(RwLock::new(status)),
                stopping: Arc::new(AtomicBool::new(true)),
            };
        };
        let identity_id = store.identity_id.clone();
        let status = Arc::new(RwLock::new(ChannelsHistorySnapshot::default()));
        let stopping = Arc::new(AtomicBool::new(false));
        let (command_tx, command_rx) = mpsc::channel(HISTORY_COMMAND_BUFFER);
        let identity_generation = app_state
            .upgrade()
            .map(|state| state.current_identity_session_generation());
        tokio::spawn(run_channel_history_writer(
            store,
            command_rx,
            ChannelHistoryWorkerContext {
                status: status.clone(),
                stopping: stopping.clone(),
                snapshot,
                emitter,
                app_state,
                identity_generation,
            },
        ));
        Self {
            command_tx: Some(command_tx),
            identity_id,
            status,
            stopping,
        }
    }

    /// Move newly accepted transcript items into the bounded writer ingress.
    /// `try_send` is intentional: a busy local database must never block the
    /// authenticated Link event loop or grow memory without a ceiling.
    fn enqueue(&self, events: &mut VecDeque<db::NewChannelHistoryEvent>) -> bool {
        if events.is_empty() {
            return false;
        }
        let Some(command_tx) = self.command_tx.as_ref() else {
            events.clear();
            return false;
        };
        // Hold the status lock while commands become visible to the worker.
        // Otherwise a very fast database write can finish and subtract from
        // zero before this producer increments `pending_events`, stranding the
        // projection in Pending forever.
        let Ok(mut status) = self.status.write() else {
            events.clear();
            return false;
        };
        let mut pending = 0usize;
        let mut dropped = 0u64;
        for event in events.drain(..) {
            if db::validate_channel_history_event(&self.identity_id, &event).is_err() {
                dropped = dropped.saturating_add(1);
                continue;
            }
            match command_tx.try_send(ChannelHistoryCommand::Append(event)) {
                Ok(()) => pending = pending.saturating_add(1),
                Err(_) => dropped = dropped.saturating_add(1),
            }
        }
        if pending == 0 && dropped == 0 {
            return false;
        }
        status.pending_events = status.pending_events.saturating_add(pending);
        if dropped > 0 {
            status.dropped_events = status.dropped_events.saturating_add(dropped);
            status.phase = ChannelsHistoryPhase::Degraded;
            status.last_error = Some(HISTORY_EVENTS_DROPPED.into());
        } else if status.phase != ChannelsHistoryPhase::Degraded {
            status.phase = ChannelsHistoryPhase::Pending;
            status.last_error = None;
        }
        true
    }

    /// Queue canonical participant identities without treating them as
    /// transcript activity. If the bounded writer is momentarily full, keep
    /// the unsent observation in the equally bounded session queue so a later
    /// event-loop pass can retry it.
    fn enqueue_participants(
        &self,
        observations: &mut VecDeque<db::NewChannelParticipantObservation>,
    ) -> bool {
        if observations.is_empty() {
            return false;
        }
        let Some(command_tx) = self.command_tx.as_ref() else {
            observations.clear();
            return false;
        };
        let mut queued = false;
        while let Some(observation) = observations.pop_front() {
            if db::validate_channel_participant_observation(&self.identity_id, &observation)
                .is_err()
            {
                continue;
            }
            match command_tx.try_send(ChannelHistoryCommand::Observe(observation)) {
                Ok(()) => queued = true,
                Err(mpsc::error::TrySendError::Full(ChannelHistoryCommand::Observe(
                    observation,
                ))) => {
                    observations.push_front(observation);
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    observations.clear();
                    break;
                }
                Err(mpsc::error::TrySendError::Full(_)) => unreachable!(),
            }
        }
        queued
    }

    fn project(&self, snapshot: &Arc<RwLock<ChannelsSnapshot>>) -> bool {
        let Some(status) = self.status.read().ok().map(|status| status.clone()) else {
            return false;
        };
        mutate_snapshot_if_changed(snapshot, |state| {
            if state.history == status {
                return false;
            }
            state.history = status;
            true
        })
    }

    async fn barrier(&self) -> Result<(), ChannelsError> {
        let Some(command_tx) = self.command_tx.as_ref() else {
            return Err(ChannelsError::LocalHistoryUnavailable);
        };
        let (result_tx, result_rx) = oneshot::channel();
        tokio::time::timeout(HISTORY_BARRIER_TIMEOUT, async {
            command_tx
                .send(ChannelHistoryCommand::Barrier { result_tx })
                .await
                .map_err(|_| ChannelsError::Stopped)?;
            result_rx.await.map_err(|_| ChannelsError::Stopped)
        })
        .await
        .map_err(|_| ChannelsError::LocalHistoryUnavailable)?
    }

    async fn shutdown(&self) {
        let Some(command_tx) = self.command_tx.as_ref() else {
            return;
        };
        self.stopping.store(true, Ordering::Release);
        let (result_tx, result_rx) = oneshot::channel();
        let shutdown = async {
            command_tx
                .send(ChannelHistoryCommand::Shutdown { result_tx })
                .await
                .map_err(|_| ())?;
            result_rx.await.map_err(|_| ())
        };
        let _ = tokio::time::timeout(HISTORY_SHUTDOWN_TIMEOUT, shutdown).await;
    }
}

fn enqueue_session_persistence(writer: &ChannelHistoryWriter, session: &mut ActiveSession) -> bool {
    let history_changed = writer.enqueue(&mut session.history_events);
    writer.enqueue_participants(&mut session.participant_observations);
    history_changed
}

fn publish_channel_history_status(
    status: &Arc<RwLock<ChannelsHistorySnapshot>>,
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    emitter: &Arc<dyn Emitter>,
) {
    let Some(status) = status.read().ok().map(|status| status.clone()) else {
        return;
    };
    if mutate_snapshot_if_changed(snapshot, |state| {
        if state.history == status {
            return false;
        }
        state.history = status;
        true
    }) {
        emit_snapshot(emitter, snapshot);
    }
}

fn finish_channel_history_batch(status: &Arc<RwLock<ChannelsHistorySnapshot>>, completed: usize) {
    let Ok(mut status) = status.write() else {
        return;
    };
    status.pending_events = status.pending_events.saturating_sub(completed);
    if status.dropped_events > 0 {
        status.phase = ChannelsHistoryPhase::Degraded;
        status.last_error = Some(HISTORY_EVENTS_DROPPED.into());
    } else if status.pending_events > 0 {
        status.phase = ChannelsHistoryPhase::Pending;
        status.last_error = None;
    } else {
        status.phase = ChannelsHistoryPhase::Ready;
        status.last_error = None;
    }
}

fn channel_writer_origin_is_current(
    app_state: &Weak<AppState>,
    identity_generation: Option<u64>,
) -> bool {
    match (app_state.upgrade(), identity_generation) {
        (Some(state), Some(generation)) => {
            state.current_identity_session_generation() == generation
        }
        // Headless tests intentionally start the writer without AppState.
        (None, None) => true,
        _ => false,
    }
}

fn emit_channel_unread_summary(
    emitter: &Arc<dyn Emitter>,
    app_state: &Weak<AppState>,
    identity_generation: Option<u64>,
    summary: &db::ChannelUnreadSummary,
) {
    if !channel_writer_origin_is_current(app_state, identity_generation) {
        return;
    }
    match serde_json::to_value(summary) {
        Ok(payload) => emitter.emit("channels_unread", payload),
        Err(_) => tracing::warn!(
            reason = "serialization_failed",
            "failed to serialize Channels unread summary"
        ),
    }
}

fn channel_notification_text(value: &str, fallback: &str, limit: usize) -> String {
    let inspected: String = value
        .chars()
        .take(limit.saturating_mul(4).max(limit))
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let collapsed = inspected.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated: String = collapsed.chars().take(limit).collect();
    if truncated.is_empty() {
        fallback.into()
    } else {
        truncated
    }
}

fn notify_committed_channel_events(
    app_state: &Weak<AppState>,
    identity_generation: Option<u64>,
    batch: &[db::NewChannelHistoryEvent],
    outcome: &db::ChannelHistoryAppendOutcome,
    summary: &db::ChannelUnreadSummary,
) {
    let Some(state) = app_state.upgrade() else {
        return;
    };
    if identity_generation
        .is_none_or(|generation| state.current_identity_session_generation() != generation)
        || state.is_foreground()
        || !state.native_notifications_enabled()
    {
        return;
    }

    // One replacement notification per room avoids a burst when a recovered
    // Link delivers several committed events in one writer batch.
    let mut selected = BTreeMap::<(String, String), usize>::new();
    for inserted in &outcome.inserted_events {
        let Some(event) = batch.get(inserted.batch_index) else {
            continue;
        };
        if event.ours
            || !matches!(
                event.kind,
                db::ChannelHistoryKind::Message
                    | db::ChannelHistoryKind::Notice
                    | db::ChannelHistoryKind::Action
            )
        {
            continue;
        }
        let Some(room) = summary.rooms.iter().find(|room| {
            room.hub_destination_hash == event.hub_destination_hash
                && room.room_name == event.room_name
        }) else {
            // A concurrent read barrier may already have consumed it.
            continue;
        };
        if room.unread_count == 0 {
            continue;
        }
        let allowed = match room.notification_level {
            db::ChannelRoomNotificationLevel::All => true,
            db::ChannelRoomNotificationLevel::Mentions => event.mentioned,
            db::ChannelRoomNotificationLevel::Mute => false,
        };
        if allowed {
            selected.insert(
                (event.hub_destination_hash.clone(), event.room_name.clone()),
                inserted.batch_index,
            );
        }
    }

    for ((hub_destination_hash, room_name), batch_index) in selected {
        let Some(event) = batch.get(batch_index) else {
            continue;
        };
        let sender = channel_notification_text(
            event.nickname.as_deref().unwrap_or(""),
            if event.kind == db::ChannelHistoryKind::Notice {
                "Channel hub"
            } else {
                "Someone"
            },
            48,
        );
        let room_label = channel_notification_text(&room_name, "channel", 48);
        let title = if event.mentioned {
            format!("{sender} mentioned you in {room_label}")
        } else {
            format!("{sender} in {room_label}")
        };
        let body = channel_notification_text(&event.text, "New channel activity", 120);
        let encoded_room = hex::encode(room_name.as_bytes());
        let route = format!("channels:{hub_destination_hash}:{encoded_room}");
        let notification_key = format!("{hub_destination_hash}:{encoded_room}");
        state.emit_native_notification(ratspeak_core::NativeNotification::channel(
            title,
            body,
            route,
            crate::stable_notification_id(&notification_key, 4_000_000),
        ));
    }
}

async fn run_channel_history_writer(
    store: ChannelsStore,
    mut command_rx: mpsc::Receiver<ChannelHistoryCommand>,
    context: ChannelHistoryWorkerContext,
) {
    let ChannelHistoryWorkerContext {
        status,
        stopping,
        snapshot,
        emitter,
        app_state,
        identity_generation,
    } = context;

    match store.prune_history().await {
        Ok(_) => finish_channel_history_batch(&status, 0),
        Err(_) => {
            if let Ok(mut status) = status.write() {
                status.phase = ChannelsHistoryPhase::Degraded;
                status.last_error = Some(HISTORY_TEMPORARILY_UNAVAILABLE.into());
            }
        }
    }
    publish_channel_history_status(&status, &snapshot, &emitter);
    if let Ok(summary) = store.unread_summary().await {
        emit_channel_unread_summary(&emitter, &app_state, identity_generation, &summary);
    }

    let mut deferred = None;
    loop {
        let command = match deferred.take() {
            Some(command) => Some(command),
            None => command_rx.recv().await,
        };
        let Some(command) = command else {
            break;
        };
        match command {
            ChannelHistoryCommand::Append(first) => {
                let mut batch = Vec::with_capacity(db::CHANNEL_HISTORY_MAX_APPEND_BATCH);
                batch.push(first);
                while batch.len() < db::CHANNEL_HISTORY_MAX_APPEND_BATCH {
                    match command_rx.try_recv() {
                        Ok(ChannelHistoryCommand::Append(event)) => batch.push(event),
                        Ok(command) => {
                            deferred = Some(command);
                            break;
                        }
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => break,
                    }
                }

                let batch = Arc::new(batch);
                let batch_len = batch.len();
                let mut failure_reported = false;
                loop {
                    match store.append_history(batch.clone()).await {
                        Ok(outcome) => {
                            if failure_reported {
                                tracing::info!(
                                    reason = "write_recovered",
                                    "local Channels history writes recovered"
                                );
                            }
                            if let Ok(summary) = store.unread_summary().await {
                                emit_channel_unread_summary(
                                    &emitter,
                                    &app_state,
                                    identity_generation,
                                    &summary,
                                );
                                notify_committed_channel_events(
                                    &app_state,
                                    identity_generation,
                                    batch.as_slice(),
                                    &outcome,
                                    &summary,
                                );
                            }
                            finish_channel_history_batch(&status, batch_len);
                            publish_channel_history_status(&status, &snapshot, &emitter);
                            break;
                        }
                        Err(_) => {
                            if stopping.load(Ordering::Acquire) {
                                return;
                            }
                            if !failure_reported {
                                tracing::warn!(
                                    reason = "write_failed",
                                    "local Channels history write will retry"
                                );
                                failure_reported = true;
                            }
                            if let Ok(mut status) = status.write() {
                                status.phase = ChannelsHistoryPhase::Degraded;
                                if status.dropped_events == 0 {
                                    status.last_error =
                                        Some(HISTORY_TEMPORARILY_UNAVAILABLE.into());
                                }
                            }
                            publish_channel_history_status(&status, &snapshot, &emitter);
                            tokio::time::sleep(HISTORY_RETRY_DELAY).await;
                        }
                    }
                }
            }
            ChannelHistoryCommand::Observe(first) => {
                let mut batch = Vec::with_capacity(db::CHANNEL_PARTICIPANT_MAX_OBSERVATION_BATCH);
                batch.push(first);
                while batch.len() < db::CHANNEL_PARTICIPANT_MAX_OBSERVATION_BATCH {
                    match command_rx.try_recv() {
                        Ok(ChannelHistoryCommand::Observe(observation)) => batch.push(observation),
                        Ok(command) => {
                            deferred = Some(command);
                            break;
                        }
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => break,
                    }
                }

                let batch = Arc::new(batch);
                let mut failure_reported = false;
                loop {
                    match store.remember_participants(batch.clone()).await {
                        Ok(_) => {
                            if failure_reported {
                                tracing::info!(
                                    reason = "participant_write_recovered",
                                    "local Channels participant writes recovered"
                                );
                            }
                            break;
                        }
                        Err(_) => {
                            if stopping.load(Ordering::Acquire) {
                                return;
                            }
                            if !failure_reported {
                                tracing::warn!(
                                    reason = "participant_write_failed",
                                    "local Channels participant write will retry"
                                );
                                failure_reported = true;
                            }
                            tokio::time::sleep(HISTORY_RETRY_DELAY).await;
                        }
                    }
                }
            }
            ChannelHistoryCommand::Barrier { result_tx } => {
                let _ = result_tx.send(());
            }
            ChannelHistoryCommand::Shutdown { result_tx } => {
                let _ = result_tx.send(());
                break;
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum RoomSecretError {
    #[error("invalid room-key secret context")]
    InvalidContext,
    #[error("room-key identity seal failed")]
    SealFailed,
}

fn seal_room_key(
    identity: &Identity,
    destination_hash: [u8; 16],
    room: &str,
    key: &str,
) -> Result<Vec<u8>, RoomSecretError> {
    let room_bytes = room.as_bytes();
    let key_bytes = key.as_bytes();
    if room_bytes.is_empty()
        || room_bytes.len() > u16::MAX as usize
        || key_bytes.is_empty()
        || key_bytes.len() > MAX_JOIN_KEY_BYTES
    {
        return Err(RoomSecretError::InvalidContext);
    }

    let mut plaintext = Zeroizing::new(Vec::with_capacity(
        ROOM_SECRET_MAGIC.len() + 1 + 16 + 16 + 2 + 2 + room_bytes.len() + key_bytes.len(),
    ));
    plaintext.extend_from_slice(ROOM_SECRET_MAGIC);
    plaintext.push(ROOM_SECRET_FORMAT_VERSION);
    plaintext.extend_from_slice(&identity.hash);
    plaintext.extend_from_slice(&destination_hash);
    plaintext.extend_from_slice(&(room_bytes.len() as u16).to_be_bytes());
    plaintext.extend_from_slice(&(key_bytes.len() as u16).to_be_bytes());
    plaintext.extend_from_slice(room_bytes);
    plaintext.extend_from_slice(key_bytes);

    let ciphertext = identity
        .encrypt(&plaintext, None)
        .map_err(|_| RoomSecretError::SealFailed)?;
    if ciphertext.is_empty() || ciphertext.len() > MAX_SEALED_ROOM_SECRET_BYTES {
        return Err(RoomSecretError::SealFailed);
    }
    Ok(ciphertext)
}

fn unseal_room_key(
    identity: &Identity,
    destination_hash: [u8; 16],
    room: &str,
    secret: &db::StoredChannelRoomSecret,
) -> Result<Zeroizing<String>, RoomSecretError> {
    if secret.seal_scheme != ROOM_SECRET_SEAL_SCHEME
        || secret.seal_version != ROOM_SECRET_SEAL_VERSION
        || secret.ciphertext.is_empty()
        || secret.ciphertext.len() > MAX_SEALED_ROOM_SECRET_BYTES
    {
        return Err(RoomSecretError::InvalidContext);
    }
    let plaintext = Zeroizing::new(
        identity
            .decrypt(&secret.ciphertext, None, false)
            .map_err(|_| RoomSecretError::SealFailed)?,
    );
    const HEADER_LEN: usize = 8 + 1 + 16 + 16 + 2 + 2;
    if plaintext.len() < HEADER_LEN
        || &plaintext[..8] != ROOM_SECRET_MAGIC
        || plaintext[8] != ROOM_SECRET_FORMAT_VERSION
        || plaintext[9..25] != identity.hash
        || plaintext[25..41] != destination_hash
    {
        return Err(RoomSecretError::InvalidContext);
    }
    let room_len = u16::from_be_bytes([plaintext[41], plaintext[42]]) as usize;
    let key_len = u16::from_be_bytes([plaintext[43], plaintext[44]]) as usize;
    let expected_len = HEADER_LEN
        .checked_add(room_len)
        .and_then(|length| length.checked_add(key_len))
        .ok_or(RoomSecretError::InvalidContext)?;
    if expected_len != plaintext.len()
        || room_len == 0
        || key_len == 0
        || key_len > MAX_JOIN_KEY_BYTES
    {
        return Err(RoomSecretError::InvalidContext);
    }
    let room_end = HEADER_LEN + room_len;
    if &plaintext[HEADER_LEN..room_end] != room.as_bytes() {
        return Err(RoomSecretError::InvalidContext);
    }
    let key =
        std::str::from_utf8(&plaintext[room_end..]).map_err(|_| RoomSecretError::InvalidContext)?;
    Ok(Zeroizing::new(key.to_string()))
}

#[derive(Clone)]
pub struct ChannelsManagerHandle {
    command_tx: mpsc::Sender<ChannelsCommand>,
    snapshot: Arc<RwLock<ChannelsSnapshot>>,
    activity: ChannelsActivity,
}

impl ChannelsManagerHandle {
    pub fn start(
        transport_tx: mpsc::Sender<TransportMessage>,
        identity: Identity,
        emitter: Arc<dyn Emitter>,
        shutdown: ShutdownSignal,
        state: Weak<AppState>,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_BUFFER);
        let snapshot = Arc::new(RwLock::new(ChannelsSnapshot::for_manager(
            next_channels_generation(),
        )));
        let store = state
            .upgrade()
            .map(|state| ChannelsStore::new(state.db.clone(), hex::encode(identity.hash)));
        let activity = ChannelsActivity::new(state.clone());
        tokio::spawn(run_manager(ChannelsManagerInput {
            transport_tx,
            identity,
            emitter,
            shutdown,
            command_rx,
            snapshot: snapshot.clone(),
            activity: activity.clone(),
            store,
            app_state: state,
        }));
        Self {
            command_tx,
            snapshot,
            activity,
        }
    }

    pub fn snapshot(&self) -> ChannelsSnapshot {
        self.snapshot
            .read()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|_| ChannelsSnapshot::unavailable())
    }

    pub async fn discover_hubs(&self) -> Result<Vec<DiscoveredChannelHub>, ChannelsError> {
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Discover { result_tx })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    pub async fn connect(
        &self,
        destination_hash: &str,
        nickname: &str,
    ) -> Result<(), ChannelsError> {
        let destination_hash = parse_destination_hash(destination_hash)?;
        let nickname = rrc::normalize_nickname(nickname, DEFAULT_NICK_MAX_BYTES)?;
        self.connect_target(destination_hash, nickname, None).await
    }

    /// Connect to an already-authenticated hub identity without relying on a
    /// locally cached announce. Used for a hub hosted by this same Ratspeak
    /// process, whose outbound announce is not required to re-enter the cache.
    pub async fn connect_known(
        &self,
        destination_hash: &str,
        nickname: &str,
        public_key: [u8; 64],
        announced_name: Option<String>,
    ) -> Result<(), ChannelsError> {
        let destination_hash = parse_destination_hash(destination_hash)?;
        let nickname = rrc::normalize_nickname(nickname, DEFAULT_NICK_MAX_BYTES)?;
        let identity = Identity::from_public_key(&public_key)
            .map_err(|error| ChannelsError::Protocol(error.to_string()))?;
        let expected =
            Destination::hash_from_name_and_identity(rrc::RRC_HUB_ASPECT, Some(&identity.hash));
        if expected != destination_hash {
            return Err(ChannelsError::Protocol(
                "channel hub identity does not match its destination".into(),
            ));
        }
        self.connect_target(
            destination_hash,
            nickname,
            Some(KnownHubTarget {
                public_key,
                identity_hash: identity.hash,
                announced_name,
                hops: 1,
            }),
        )
        .await
    }

    async fn connect_target(
        &self,
        destination_hash: [u8; 16],
        nickname: String,
        known_hub: Option<KnownHubTarget>,
    ) -> Result<(), ChannelsError> {
        let activity_fence = self.activity.capture_fence();
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Connect {
                destination_hash,
                nickname,
                known_hub,
                activity_fence,
                result_tx,
            })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    pub async fn disconnect(&self) -> Result<(), ChannelsError> {
        let activity_fence = self.activity.capture_fence();
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Disconnect {
                activity_fence,
                result_tx,
            })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    pub async fn join(&self, room: &str, key: Option<String>) -> Result<String, ChannelsError> {
        self.join_with_key_policy(room, key, false).await
    }

    pub async fn join_with_key_policy(
        &self,
        room: &str,
        key: Option<String>,
        remember_key: bool,
    ) -> Result<String, ChannelsError> {
        if key
            .as_ref()
            .is_some_and(|key| key.len() > MAX_JOIN_KEY_BYTES)
        {
            return Err(ChannelsError::JoinKeyTooLong(MAX_JOIN_KEY_BYTES));
        }
        let activity_fence = self.activity.capture_fence();
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Join {
                room: room.to_string(),
                key: key.map(Zeroizing::new),
                remember_key,
                activity_fence,
                result_tx,
            })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    pub async fn part(&self, room: &str) -> Result<(), ChannelsError> {
        let activity_fence = self.activity.capture_fence();
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Part {
                room: room.to_string(),
                activity_fence,
                result_tx,
            })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    pub async fn send(&self, room: &str, text: &str) -> Result<(), ChannelsError> {
        let activity_fence = self.activity.capture_fence();
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Send {
                room: room.to_string(),
                text: text.to_string(),
                activity_fence,
                result_tx,
            })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    /// Request the reference-compatible public room list. The response is
    /// interpreted into Link-scoped observation; it is not persisted and does
    /// not imply that any listed room has been joined.
    pub async fn refresh_directory(&self) -> Result<(), ChannelsError> {
        let activity_fence = self.activity.capture_fence();
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::RefreshDirectory {
                activity_fence,
                result_tx,
            })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    /// Reconcile bookmark changes made through the existing IPC surface into
    /// the unified service snapshot. Best-effort callers can ignore a stopped
    /// manager; a fresh manager reloads the same identity-scoped rows.
    pub async fn refresh_durable(&self) {
        let (result_tx, result_rx) = oneshot::channel();
        if self
            .command_tx
            .send(ChannelsCommand::RefreshDurable { result_tx })
            .await
            .is_ok()
        {
            let _ = result_rx.await;
        }
    }

    /// FIFO barrier for durable read transitions. Every transcript event the
    /// manager accepted before this command is committed (or the command
    /// fails) before the caller may advance a room cursor.
    pub async fn flush_history(&self) -> Result<(), ChannelsError> {
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::FlushHistory { result_tx })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    /// Adopt a renamed identity for the live session, if it is still using the
    /// superseded name. Best-effort: a stopped manager has nothing to leak.
    pub async fn identity_renamed(&self, previous: &str, current: &str) {
        let _ = self
            .command_tx
            .send(ChannelsCommand::IdentityRenamed {
                previous: previous.to_string(),
                current: current.to_string(),
            })
            .await;
    }

    pub async fn shutdown(&self) {
        let activity_fence = self.activity.capture_fence();
        let (result_tx, result_rx) = oneshot::channel();
        if self
            .command_tx
            .send(ChannelsCommand::Shutdown {
                activity_fence,
                result_tx,
            })
            .await
            .is_ok()
        {
            let _ = tokio::time::timeout(Duration::from_secs(2), result_rx).await;
        }
    }
}

#[derive(Clone, Copy)]
struct SessionActivityContext {
    hub: activity::DestinationHash,
    correlation_id: CorrelationId,
    origin: Option<ActivityRequestFence>,
}

#[derive(Clone, Copy)]
struct RoomOperationContext {
    correlation_id: CorrelationId,
    origin: Option<ActivityRequestFence>,
}

struct RoomActivityContext {
    token: activity::ChannelRoomToken,
    join: Option<RoomOperationContext>,
    part: Option<RoomOperationContext>,
}

enum PendingJoinSecret {
    UserRemember(Zeroizing<String>),
    UserEphemeral,
    Stored,
}

enum JoinSecretInput {
    None,
    User {
        key: Zeroizing<String>,
        remember: bool,
    },
    Stored(Zeroizing<String>),
}

enum RoomSecretAction {
    Persist {
        room: String,
        key: Zeroizing<String>,
    },
    ForgetRequired {
        room: String,
    },
    ForgetRejected {
        room: String,
    },
    MarkRequired {
        room: String,
    },
}

#[derive(Clone)]
struct HubGreetingResourceExpectation {
    announcement_id: [u8; 8],
    size: usize,
    sha256: Option<[u8; 32]>,
    encoding: Option<String>,
    encoded_bytes: u32,
    created_at: Instant,
}

struct HubGreetingResourceInFlight {
    resource_id: [u8; 32],
    started_at: Instant,
}

struct HubGreetingResourceCompletion {
    link_id: [u8; 16],
    resource_id: [u8; 32],
    expectation: HubGreetingResourceExpectation,
    result: Result<LinkSessionReceivedResource, String>,
}

struct ActiveSession {
    handle: rns_runtime::link_session::LinkSessionHandle,
    events: mpsc::Receiver<LinkSessionEvent>,
    resource_offers: mpsc::Receiver<LinkSessionResourceOffer>,
    resource_offers_open: bool,
    connected_at: Instant,
    source: [u8; 16],
    destination_hash: [u8; 16],
    hub_identity: [u8; 16],
    nickname: String,
    supports_action: bool,
    supports_resources: bool,
    limits: HubLimits,
    rooms: BTreeMap<String, ChannelRoomSnapshot>,
    directory: ChannelRoomDirectorySnapshot,
    directory_request_deadline: Option<Instant>,
    directory_last_requested_at: Option<Instant>,
    hub_greeting: Option<ChannelHubGreetingSnapshot>,
    hub_greeting_deadline: Instant,
    hub_greeting_notice_may_continue: bool,
    greeting_resource_expectation: Option<HubGreetingResourceExpectation>,
    greeting_resource_in_flight: Option<HubGreetingResourceInFlight>,
    notices: VecDeque<ChannelTranscriptItem>,
    seen_ids: HashSet<[u8; 8]>,
    seen_order: VecDeque<[u8; 8]>,
    message_tokens: HashMap<[u8; 8], activity::ChannelMessageToken>,
    message_token_order: VecDeque<[u8; 8]>,
    room_activity: BTreeMap<String, RoomActivityContext>,
    auto_rejoin_queue: VecDeque<String>,
    auto_rejoining: HashSet<String>,
    pending_join_secrets: BTreeMap<String, PendingJoinSecret>,
    room_secret_actions: VecDeque<RoomSecretAction>,
    history_events: VecDeque<db::NewChannelHistoryEvent>,
    participant_observations: VecDeque<db::NewChannelParticipantObservation>,
    connect_origin: ConnectOrigin,
    activity: SessionActivityContext,
}

impl ActiveSession {
    fn closed_transition(
        &self,
        reason: activity::ChannelSessionCloseReason,
    ) -> activity::ChannelSessionTransition {
        activity::ChannelSessionTransition::Closed {
            reason,
            link: Some(activity::LinkId::new(self.handle.link_id())),
            duration_ms: Some(
                self.connected_at
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            ),
        }
    }

    fn remember(&mut self, message_id: [u8; 8]) -> bool {
        if !self.seen_ids.insert(message_id) {
            return false;
        }
        self.seen_order.push_back(message_id);
        while self.seen_order.len() > SEEN_MESSAGE_LIMIT {
            if let Some(oldest) = self.seen_order.pop_front() {
                self.seen_ids.remove(&oldest);
            }
        }
        true
    }

    fn message_token(&mut self, message_id: [u8; 8]) -> activity::ChannelMessageToken {
        if let Some(token) = self.message_tokens.get(&message_id) {
            return *token;
        }
        let token = activity::ChannelMessageToken::random();
        self.message_tokens.insert(message_id, token);
        self.message_token_order.push_back(message_id);
        while self.message_token_order.len() > SEEN_MESSAGE_LIMIT {
            if let Some(oldest) = self.message_token_order.pop_front() {
                self.message_tokens.remove(&oldest);
            }
        }
        token
    }

    fn room_token(&self, room: Option<&str>) -> Option<activity::ChannelRoomToken> {
        room.and_then(|room| self.room_activity.get(room).map(|context| context.token))
    }

    fn envelope_correlation(&self, envelope: &Envelope) -> CorrelationId {
        let operation = envelope.room.as_deref().and_then(|room| {
            self.room_activity
                .get(room)
                .and_then(|context| match envelope.message_type {
                    MessageType::Join | MessageType::Joined => context.join,
                    MessageType::Part | MessageType::Parted => context.part,
                    _ => None,
                })
        });
        operation
            .map(|operation| operation.correlation_id)
            .unwrap_or(self.activity.correlation_id)
    }
}

struct ConnectedSession {
    session: LinkSession,
    destination_hash: [u8; 16],
    hub_identity: [u8; 16],
    announced_name: Option<String>,
    hops: u8,
    nickname: String,
    welcome: WelcomeInfo,
    buffered: Vec<(Envelope, u32)>,
    activity: SessionActivityContext,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ConnectOrigin {
    #[default]
    User,
    Recovery,
}

#[derive(Clone, Copy)]
enum ConnectFailure {
    PathTimedOut,
    WelcomeRejected(activity::ChannelSessionFailureReason),
    Failed(activity::ChannelSessionFailureReason),
}

#[derive(Default)]
struct ReconnectController {
    failure_streak: u32,
    deadline: Option<Instant>,
    next_attempt_at_ms: Option<u64>,
    last_error: Option<String>,
    blocked: bool,
}

impl ReconnectController {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn schedule_immediate(&mut self) {
        self.deadline = Some(Instant::now());
        self.next_attempt_at_ms = Some(now_ms());
        self.last_error = None;
        self.blocked = false;
    }

    fn schedule_failure(&mut self, error: String, jitter: u16) -> Duration {
        self.failure_streak = self.failure_streak.saturating_add(1);
        let delay = reconnect_delay(self.failure_streak, jitter);
        self.deadline = Some(Instant::now() + delay);
        self.next_attempt_at_ms =
            Some(now_ms().saturating_add(delay.as_millis().try_into().unwrap_or(u64::MAX)));
        self.last_error = Some(error);
        self.blocked = false;
        delay
    }

    fn block(&mut self, error: String) {
        self.deadline = None;
        self.next_attempt_at_ms = None;
        self.last_error = Some(error);
        self.blocked = true;
    }

    fn begin_attempt(&mut self) {
        self.deadline = None;
        self.next_attempt_at_ms = None;
        self.blocked = false;
    }

    fn note_session_ended(&mut self, connected_for: Duration) {
        if connected_for >= RECONNECT_STABLE_RESET {
            self.failure_streak = 0;
        }
    }
}

fn reconnect_delay(attempt: u32, jitter: u16) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    let multiplier = 1u32 << exponent;
    let base = RECONNECT_BASE_DELAY
        .saturating_mul(multiplier)
        .min(RECONNECT_MAX_DELAY);
    let spread = RECONNECT_JITTER_PERCENT * 2;
    let jitter_percent =
        100 - RECONNECT_JITTER_PERCENT + (u32::from(jitter) * spread / u32::from(u16::MAX));
    Duration::from_millis(
        base.as_millis()
            .saturating_mul(u128::from(jitter_percent))
            .saturating_div(100)
            .min(RECONNECT_MAX_DELAY.as_millis())
            .try_into()
            .unwrap_or(u64::MAX),
    )
}

fn reconnect_jitter() -> u16 {
    let bytes = rns_crypto::random::random_bytes(2);
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn retryable_connect_failure(failure: ConnectFailure) -> bool {
    match failure {
        ConnectFailure::PathTimedOut => true,
        ConnectFailure::Failed(reason) | ConnectFailure::WelcomeRejected(reason) => matches!(
            reason,
            activity::ChannelSessionFailureReason::AuthenticationFailed
                | activity::ChannelSessionFailureReason::PathLookupFailed
                | activity::ChannelSessionFailureReason::SendFailed
                | activity::ChannelSessionFailureReason::TransportUnavailable
                | activity::ChannelSessionFailureReason::WelcomeTimedOut
        ),
    }
}

struct ConnectAttemptError {
    product: ChannelsError,
    activity: ConnectFailure,
}

fn record_session_origin(
    recorder: &ChannelsActivity,
    context: SessionActivityContext,
    transition: activity::ChannelSessionTransition,
) {
    recorder.record_fenced(context.origin, move || {
        activity::channels_session_activity(activity::ChannelsSessionActivity {
            hub: context.hub,
            correlation_id: context.correlation_id,
            transition,
        })
    });
}

fn record_session_command(
    recorder: &ChannelsActivity,
    context: SessionActivityContext,
    command_fence: Option<ActivityRequestFence>,
    transition: activity::ChannelSessionTransition,
) {
    recorder.record_fenced(command_fence, move || {
        activity::channels_session_activity(activity::ChannelsSessionActivity {
            hub: context.hub,
            correlation_id: context.correlation_id,
            transition,
        })
    });
}

fn record_session_spontaneous(
    recorder: &ChannelsActivity,
    context: SessionActivityContext,
    transition: activity::ChannelSessionTransition,
) {
    recorder.record_spontaneous(move || {
        activity::channels_session_activity(activity::ChannelsSessionActivity {
            hub: context.hub,
            correlation_id: context.correlation_id,
            transition,
        })
    });
}

fn record_room_operation(
    recorder: &ChannelsActivity,
    session: SessionActivityContext,
    room: activity::ChannelRoomToken,
    operation: RoomOperationContext,
    transition: activity::ChannelRoomTransition,
) {
    recorder.record_fenced(operation.origin, move || {
        activity::channels_room_activity(activity::ChannelsRoomActivity {
            hub: session.hub,
            room,
            correlation_id: operation.correlation_id,
            transition,
        })
    });
}

fn record_room_spontaneous(
    recorder: &ChannelsActivity,
    session: SessionActivityContext,
    room: activity::ChannelRoomToken,
    correlation_id: CorrelationId,
    transition: activity::ChannelRoomTransition,
) {
    recorder.record_spontaneous(move || {
        activity::channels_room_activity(activity::ChannelsRoomActivity {
            hub: session.hub,
            room,
            correlation_id,
            transition,
        })
    });
}

fn record_pending_room_cancellations(
    recorder: &ChannelsActivity,
    session: &ActiveSession,
    command_fence: Option<ActivityRequestFence>,
) {
    for context in session.room_activity.values() {
        if let Some(join) = context.join {
            record_room_operation(
                recorder,
                session.activity,
                context.token,
                RoomOperationContext {
                    origin: command_fence,
                    ..join
                },
                activity::ChannelRoomTransition::JoinCancelled,
            );
        }
        if let Some(part) = context.part {
            record_room_operation(
                recorder,
                session.activity,
                context.token,
                RoomOperationContext {
                    origin: command_fence,
                    ..part
                },
                activity::ChannelRoomTransition::PartCancelled,
            );
        }
    }
}

fn record_lost_room_operations(recorder: &ChannelsActivity, session: &ActiveSession) {
    for context in session.room_activity.values() {
        if let Some(join) = context.join {
            record_room_spontaneous(
                recorder,
                session.activity,
                context.token,
                join.correlation_id,
                activity::ChannelRoomTransition::JoinRejected {
                    reason: activity::ChannelRoomFailureReason::SessionClosed,
                },
            );
        }
        if let Some(part) = context.part {
            record_room_spontaneous(
                recorder,
                session.activity,
                context.token,
                part.correlation_id,
                activity::ChannelRoomTransition::PartRejected {
                    reason: activity::ChannelRoomFailureReason::SessionClosed,
                },
            );
        }
    }
}

enum ConnectUpdate {
    SessionActivity {
        attempt: u64,
        transition: activity::ChannelSessionTransition,
    },
    EnvelopeActivity {
        attempt: u64,
        outbound: bool,
        message: Option<activity::ChannelMessageToken>,
        envelope_kind: Option<activity::ChannelEnvelopeKind>,
        encoded_bytes: u32,
        validation: activity::SourceValidation,
    },
    Discovered {
        attempt: u64,
        hub_identity: [u8; 16],
        announced_name: Option<String>,
        hops: u8,
    },
    AwaitingWelcome {
        attempt: u64,
        link_mdu: usize,
    },
    Ready {
        attempt: u64,
        connected: Box<ConnectedSession>,
    },
    Failed {
        attempt: u64,
        error: ChannelsError,
        failure: ConnectFailure,
    },
}

struct ConnectAttemptInput {
    attempt: u64,
    transport_tx: mpsc::Sender<TransportMessage>,
    identity: Identity,
    destination_hash: [u8; 16],
    nickname: String,
    known_hub: Option<KnownHubTarget>,
    update_tx: mpsc::Sender<ConnectUpdate>,
    cancel_rx: oneshot::Receiver<()>,
    activity_context: SessionActivityContext,
}

struct ConnectToHubInput {
    attempt: u64,
    transport_tx: mpsc::Sender<TransportMessage>,
    identity: Identity,
    destination_hash: [u8; 16],
    nickname: String,
    known_hub: Option<KnownHubTarget>,
    update_tx: mpsc::Sender<ConnectUpdate>,
    activity_context: SessionActivityContext,
}

struct ConnectUpdateContext<'a> {
    current_attempt: u64,
    connect_cancel: &'a mut Option<oneshot::Sender<()>>,
    active: &'a mut Option<ActiveSession>,
    snapshot: &'a Arc<RwLock<ChannelsSnapshot>>,
    emitter: &'a Arc<dyn Emitter>,
    activity_recorder: &'a ChannelsActivity,
    pending_connect_activity: &'a mut Option<SessionActivityContext>,
    source: [u8; 16],
    store: Option<&'a ChannelsStore>,
    stored_secrets: &'a mut StoredRoomSecrets,
}

fn connect_update_attempt(update: &ConnectUpdate) -> u64 {
    match update {
        ConnectUpdate::SessionActivity { attempt, .. }
        | ConnectUpdate::EnvelopeActivity { attempt, .. }
        | ConnectUpdate::Discovered { attempt, .. }
        | ConnectUpdate::AwaitingWelcome { attempt, .. }
        | ConnectUpdate::Ready { attempt, .. }
        | ConnectUpdate::Failed { attempt, .. } => *attempt,
    }
}

async fn run_manager(input: ChannelsManagerInput) {
    let ChannelsManagerInput {
        transport_tx,
        identity,
        emitter,
        shutdown,
        mut command_rx,
        snapshot,
        activity,
        store,
        app_state,
    } = input;
    let source = identity.hash;
    let (connect_update_tx, mut connect_update_rx) = mpsc::channel(CONNECT_UPDATE_BUFFER);
    let (greeting_resource_completion_tx, mut greeting_resource_completion_rx) =
        mpsc::channel(GREETING_RESOURCE_COMPLETION_BUFFER);
    let mut active: Option<ActiveSession> = None;
    let mut attempt: u64 = 0;
    let mut connect_cancel: Option<oneshot::Sender<()>> = None;
    let mut pending_connect_activity: Option<SessionActivityContext> = None;
    let mut connect_origin = ConnectOrigin::User;
    let mut reconnect = ReconnectController::default();
    let mut stored_secrets = StoredRoomSecrets::new();
    let mut room_transition_tick = tokio::time::interval(ROOM_TRANSITION_TICK);
    room_transition_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let history = ChannelHistoryWriter::start(
        store.clone(),
        snapshot.clone(),
        emitter.clone(),
        app_state.clone(),
    );

    match &store {
        Some(store) => apply_store_result(&snapshot, &mut stored_secrets, store.load().await),
        None => mutate_snapshot(&snapshot, |state| {
            state.durability = ChannelsDurabilitySnapshot {
                phase: ChannelsDurabilityPhase::Ready,
                last_error: None,
            };
        }),
    }
    if snapshot
        .read()
        .ok()
        .is_some_and(|state| state.selected_hub_destination.is_some())
    {
        reconnect.schedule_immediate();
        project_reconnect(&snapshot, &reconnect, ChannelRecoveryPhase::Scheduled);
    }
    emit_snapshot(&emitter, &snapshot);

    loop {
        tokio::select! {
            _ = shutdown.wait() => {
                invalidate_connect_attempt(&mut attempt);
                if let Some(context) = pending_connect_activity.take() {
                    record_session_spontaneous(
                        &activity,
                        context,
                        activity::ChannelSessionTransition::Cancelled,
                    );
                }
                cancel_connection(&mut connect_cancel);
                if let Some(session) = active.as_ref() {
                    record_session_spontaneous(
                        &activity,
                        session.activity,
                        session.closed_transition(activity::ChannelSessionCloseReason::Local),
                    );
                }
                if let Some(session) = active.as_mut() {
                    enqueue_session_persistence(&history, session);
                    history.project(&snapshot);
                }
                history.shutdown().await;
                close_active(&mut active).await;
                mutate_snapshot(&snapshot, clear_observed_snapshot);
                emit_snapshot(&emitter, &snapshot);
                break;
            }
            _ = wait_for_reconnect(reconnect.deadline),
                if active.is_none() && connect_cancel.is_none() =>
            {
                let (destination_hash, nickname) =
                    match desired_connection_target(&snapshot) {
                        Ok(Some(target)) => target,
                        Ok(None) => {
                            reconnect.clear();
                            continue;
                        }
                        Err(error) => {
                            reconnect.block(error);
                            project_reconnect(
                                &snapshot,
                                &reconnect,
                                ChannelRecoveryPhase::Blocked,
                            );
                            emit_snapshot(&emitter, &snapshot);
                            continue;
                        }
                    };
                let this_attempt = invalidate_connect_attempt(&mut attempt);
                let activity_context = SessionActivityContext {
                    hub: activity::DestinationHash::new(destination_hash),
                    correlation_id: CorrelationId::random(),
                    origin: None,
                };
                record_session_origin(
                    &activity,
                    activity_context,
                    activity::ChannelSessionTransition::ConnectRequested,
                );
                pending_connect_activity = Some(activity_context);
                connect_origin = ConnectOrigin::Recovery;
                reconnect.begin_attempt();
                let destination_text = hex::encode(destination_hash);
                mutate_snapshot(&snapshot, |state| {
                    clear_observed_snapshot(state);
                    state.phase = ChannelsPhase::Reconnecting;
                    state.nickname = Some(nickname.clone());
                    state.hub = Some(ChannelHubSnapshot::pending(destination_hash));
                    project_reconnect_state(
                        state,
                        &destination_text,
                        &reconnect,
                        ChannelRecoveryPhase::Connecting,
                    );
                });
                emit_snapshot(&emitter, &snapshot);
                let known_hub = known_owned_hub_target(&app_state, destination_hash);
                let (cancel_tx, cancel_rx) = oneshot::channel();
                connect_cancel = Some(cancel_tx);
                tokio::spawn(run_connect_attempt(ConnectAttemptInput {
                    attempt: this_attempt,
                    transport_tx: transport_tx.clone(),
                    identity: identity.clone(),
                    destination_hash,
                    nickname,
                    known_hub,
                    update_tx: connect_update_tx.clone(),
                    cancel_rx,
                    activity_context,
                }));
            }
            command = command_rx.recv() => {
                let Some(command) = command else {
                    invalidate_connect_attempt(&mut attempt);
                    if let Some(context) = pending_connect_activity.take() {
                        record_session_spontaneous(
                            &activity,
                            context,
                            activity::ChannelSessionTransition::Cancelled,
                        );
                    }
                    cancel_connection(&mut connect_cancel);
                    if let Some(session) = active.as_ref() {
                        record_session_spontaneous(
                            &activity,
                            session.activity,
                            session.closed_transition(activity::ChannelSessionCloseReason::Local),
                        );
                    }
                    if let Some(session) = active.as_mut() {
                        enqueue_session_persistence(&history, session);
                        history.project(&snapshot);
                    }
                    history.shutdown().await;
                    close_active(&mut active).await;
                    break;
                };
                match command {
                    ChannelsCommand::Discover { result_tx } => {
                        let _ = result_tx.send(discover_hubs(&transport_tx).await);
                    }
                    ChannelsCommand::Connect {
                        destination_hash,
                        nickname,
                        known_hub,
                        activity_fence,
                        result_tx,
                    } => {
                        let phase = snapshot.read().ok().map(|s| s.phase);
                        if matches!(phase, Some(ChannelsPhase::Resolving | ChannelsPhase::Connecting | ChannelsPhase::AwaitingWelcome)) {
                            let _ = result_tx.send(Err(ChannelsError::AlreadyConnecting));
                            continue;
                        }
                        reconnect.clear();
                        connect_origin = ConnectOrigin::User;
                        let this_attempt = invalidate_connect_attempt(&mut attempt);
                        cancel_connection(&mut connect_cancel);
                        if let Some(session) = active.as_ref() {
                            record_session_command(
                                &activity,
                                session.activity,
                                activity_fence,
                                session.closed_transition(activity::ChannelSessionCloseReason::Local),
                            );
                        }
                        close_active(&mut active).await;
                        let activity_context = SessionActivityContext {
                            hub: activity::DestinationHash::new(destination_hash),
                            correlation_id: CorrelationId::random(),
                            origin: activity_fence,
                        };
                        record_session_origin(
                            &activity,
                            activity_context,
                            activity::ChannelSessionTransition::ConnectRequested,
                        );
                        pending_connect_activity = Some(activity_context);
                        let destination_text = hex::encode(destination_hash);
                        mutate_snapshot(&snapshot, |state| {
                            clear_observed_snapshot(state);
                            set_desired_hub(state, &destination_text, &nickname, true);
                            state.phase = ChannelsPhase::Resolving;
                            state.nickname = Some(nickname.clone());
                            state.hub = Some(ChannelHubSnapshot::pending(destination_hash));
                            project_reconnect_state(
                                state,
                                &destination_text,
                                &reconnect,
                                ChannelRecoveryPhase::Connecting,
                            );
                        });
                        emit_snapshot(&emitter, &snapshot);
                        if let Some(store) = &store {
                            apply_store_result(
                                &snapshot,
                                &mut stored_secrets,
                                store
                                    .set_hub_desired(
                                        destination_text,
                                        nickname.clone(),
                                        true,
                                    )
                                    .await,
                            );
                            emit_snapshot(&emitter, &snapshot);
                        }

                        let (cancel_tx, cancel_rx) = oneshot::channel();
                        connect_cancel = Some(cancel_tx);
                        let known_hub = known_hub
                            .or_else(|| known_owned_hub_target(&app_state, destination_hash));
                        tokio::spawn(run_connect_attempt(ConnectAttemptInput {
                            attempt: this_attempt,
                            transport_tx: transport_tx.clone(),
                            identity: identity.clone(),
                            destination_hash,
                            nickname,
                            known_hub,
                            update_tx: connect_update_tx.clone(),
                            cancel_rx,
                            activity_context,
                        }));
                        let _ = result_tx.send(Ok(()));
                    }
                    ChannelsCommand::Disconnect {
                        activity_fence,
                        result_tx,
                    } => {
                        reconnect.clear();
                        invalidate_connect_attempt(&mut attempt);
                        if let Some(context) = pending_connect_activity.take() {
                            record_session_command(
                                &activity,
                                context,
                                activity_fence,
                                activity::ChannelSessionTransition::Cancelled,
                            );
                        }
                        cancel_connection(&mut connect_cancel);
                        if let Some(session) = active.as_ref() {
                            record_pending_room_cancellations(
                                &activity,
                                session,
                                activity_fence,
                            );
                            record_session_command(
                                &activity,
                                session.activity,
                                activity_fence,
                                session.closed_transition(activity::ChannelSessionCloseReason::Local),
                            );
                        }
                        let desired_target = snapshot.read().ok().and_then(|state| {
                            let destination = state.selected_hub_destination.clone()?;
                            let nickname = state
                                .hubs
                                .iter()
                                .find(|hub| hub.destination_hash == destination)
                                .and_then(|hub| hub.desired.nickname.clone())
                                .unwrap_or_default();
                            Some((destination, nickname))
                        });
                        close_active(&mut active).await;
                        mutate_snapshot(&snapshot, |state| {
                            if let Some((destination, nickname)) = desired_target.as_ref() {
                                set_desired_hub(state, destination, nickname, false);
                                project_reconnect_state(
                                    state,
                                    destination,
                                    &reconnect,
                                    ChannelRecoveryPhase::Idle,
                                );
                            }
                            clear_observed_snapshot(state);
                        });
                        emit_snapshot(&emitter, &snapshot);
                        if let (Some(store), Some((destination, nickname))) =
                            (&store, desired_target)
                        {
                            apply_store_result(
                                &snapshot,
                                &mut stored_secrets,
                                store
                                    .set_hub_desired(destination, nickname, false)
                                    .await,
                            );
                            emit_snapshot(&emitter, &snapshot);
                        }
                        let _ = result_tx.send(Ok(()));
                    }
                    ChannelsCommand::Join {
                        room,
                        key,
                        remember_key,
                        activity_fence,
                        result_tx,
                    } => {
                        let Some(session) = active.as_ref() else {
                            let _ = result_tx.send(Err(ChannelsError::NotConnected));
                            continue;
                        };
                        let max_room = session
                            .limits
                            .max_room_name_bytes
                            .unwrap_or(DEFAULT_ROOM_MAX_BYTES);
                        let room = match rrc::normalize_room(&room, max_room) {
                            Ok(room) => room,
                            Err(error) => {
                                let _ = result_tx.send(Err(error.into()));
                                continue;
                            }
                        };
                        let destination_hash = session.destination_hash;
                        let destination = hex::encode(destination_hash);
                        let secret = match key {
                            Some(key) if !key.is_empty() => JoinSecretInput::User {
                                key,
                                remember: remember_key,
                            },
                            _ => {
                                let secret_id = (destination.clone(), room.clone());
                                match stored_secrets.get(&secret_id) {
                                    Some(stored) => match unseal_room_key(
                                        &identity,
                                        destination_hash,
                                        &room,
                                        stored,
                                    ) {
                                        Ok(key) => JoinSecretInput::Stored(key),
                                        Err(_) => {
                                            stored_secrets.remove(&secret_id);
                                            set_room_secret_status(
                                                &snapshot,
                                                &destination,
                                                &room,
                                                false,
                                                true,
                                            );
                                            if let Some(store) = &store {
                                                apply_store_result(
                                                    &snapshot,
                                                    &mut stored_secrets,
                                                    store
                                                        .remove_room_secret(
                                                            destination,
                                                            room.clone(),
                                                        )
                                                        .await,
                                                );
                                            }
                                            emit_snapshot(&emitter, &snapshot);
                                            let _ = result_tx.send(Err(
                                                ChannelsError::SavedJoinKeyUnavailable(room),
                                            ));
                                            continue;
                                        }
                                    },
                                    None => JoinSecretInput::None,
                                }
                            }
                        };
                        let result = join_room(
                            active.as_mut(),
                            &snapshot,
                            &emitter,
                            &activity,
                            room,
                            secret,
                            activity_fence,
                        ).await;
                        if let Ok(joined_room) = result.as_ref()
                            && let Some(session) = active.as_ref()
                        {
                            let destination = hex::encode(session.destination_hash);
                            mutate_snapshot(&snapshot, |state| {
                                set_desired_room(state, &destination, joined_room, true)
                            });
                            if let Some(store) = &store {
                                apply_store_result(
                                    &snapshot,
                                    &mut stored_secrets,
                                    store
                                        .set_room_desired(
                                            destination,
                                            joined_room.clone(),
                                            true,
                                        )
                                        .await,
                                );
                            }
                            emit_snapshot(&emitter, &snapshot);
                        }
                        let _ = result_tx.send(result);
                    }
                    ChannelsCommand::Part {
                        room,
                        activity_fence,
                        result_tx,
                    } => {
                        let desired_room = active.as_ref().and_then(|session| {
                            rrc::normalize_room(
                                &room,
                                session
                                    .limits
                                    .max_room_name_bytes
                                    .unwrap_or(DEFAULT_ROOM_MAX_BYTES),
                            )
                            .ok()
                        });
                        let result = part_room(
                            active.as_mut(),
                            &snapshot,
                            &emitter,
                            &activity,
                            room,
                            activity_fence,
                        ).await;
                        if result.is_ok()
                            && let (Some(room), Some(session)) =
                                (desired_room, active.as_ref())
                        {
                            let destination = hex::encode(session.destination_hash);
                            mutate_snapshot(&snapshot, |state| {
                                set_desired_room(state, &destination, &room, false)
                            });
                            if let Some(store) = &store {
                                apply_store_result(
                                    &snapshot,
                                    &mut stored_secrets,
                                    store
                                        .set_room_desired(destination, room, false)
                                        .await,
                                );
                            }
                            emit_snapshot(&emitter, &snapshot);
                        }
                        let _ = result_tx.send(result);
                    }
                    ChannelsCommand::Send {
                        room,
                        text,
                        activity_fence,
                        result_tx,
                    } => {
                        let result =
                            send_room_text(active.as_mut(), &activity, room, text, activity_fence)
                                .await;
                        let _ = result_tx.send(result);
                    }
                    ChannelsCommand::RefreshDirectory {
                        activity_fence,
                        result_tx,
                    } => {
                        let result = refresh_room_directory(
                            active.as_mut(),
                            &activity,
                            activity_fence,
                        )
                        .await;
                        if let Some(session) = active.as_ref() {
                            sync_session_snapshot(session, &snapshot);
                            emit_snapshot(&emitter, &snapshot);
                        }
                        let _ = result_tx.send(result);
                    }
                    ChannelsCommand::IdentityRenamed { previous, current } => {
                        let mut adopted_target = None;
                        if let Some(session) = active.as_mut()
                            && let Some(adopted) = adopt_renamed_nickname(
                                &session.nickname,
                                &previous,
                                &current,
                                session
                                    .limits
                                    .max_nick_bytes
                                    .unwrap_or(DEFAULT_NICK_MAX_BYTES),
                            )
                        {
                            session.nickname = adopted.clone();
                            adopted_target = Some((hex::encode(session.destination_hash), adopted.clone()));
                            mutate_snapshot(&snapshot, |snapshot| {
                                snapshot.nickname = Some(adopted);
                            });
                            emit_snapshot(&emitter, &snapshot);
                        }
                        if let Some((destination, adopted)) = adopted_target {
                            mutate_snapshot(&snapshot, |state| {
                                set_desired_hub(state, &destination, &adopted, true)
                            });
                            if let Some(store) = &store {
                                apply_store_result(
                                    &snapshot,
                                    &mut stored_secrets,
                                    store
                                        .set_hub_desired(destination, adopted, true)
                                        .await,
                                );
                            }
                            emit_snapshot(&emitter, &snapshot);
                        } else {
                            // The rename transaction may have retired a saved
                            // default nickname even without a live session.
                            if let Some(store) = &store {
                                apply_store_result(
                                    &snapshot,
                                    &mut stored_secrets,
                                    store.load().await,
                                );
                                emit_snapshot(&emitter, &snapshot);
                            }
                        }
                    }
                    ChannelsCommand::RefreshDurable { result_tx } => {
                        if let Some(store) = &store {
                            apply_store_result(
                                &snapshot,
                                &mut stored_secrets,
                                store.load().await,
                            );
                        } else {
                            mutate_snapshot(&snapshot, |state| {
                                state.durability = ChannelsDurabilitySnapshot {
                                    phase: ChannelsDurabilityPhase::Ready,
                                    last_error: None,
                                };
                            });
                        }
                        let selected = snapshot
                            .read()
                            .ok()
                            .and_then(|state| state.selected_hub_destination.clone());
                        if selected.is_none() {
                            reconnect.clear();
                        } else if active.is_none()
                            && connect_cancel.is_none()
                            && reconnect.deadline.is_none()
                            && !reconnect.blocked
                        {
                            reconnect.schedule_immediate();
                            project_reconnect(
                                &snapshot,
                                &reconnect,
                                ChannelRecoveryPhase::Scheduled,
                            );
                        }
                        emit_snapshot(&emitter, &snapshot);
                        let _ = result_tx.send(());
                    }
                    ChannelsCommand::FlushHistory { result_tx } => {
                        if let Some(session) = active.as_mut()
                            && enqueue_session_persistence(&history, session)
                        {
                            history.project(&snapshot);
                            emit_snapshot(&emitter, &snapshot);
                        }
                        let _ = result_tx.send(history.barrier().await);
                    }
                    ChannelsCommand::Shutdown {
                        activity_fence,
                        result_tx,
                    } => {
                        invalidate_connect_attempt(&mut attempt);
                        if let Some(context) = pending_connect_activity.take() {
                            record_session_command(
                                &activity,
                                context,
                                activity_fence,
                                activity::ChannelSessionTransition::Cancelled,
                            );
                        }
                        cancel_connection(&mut connect_cancel);
                        if let Some(session) = active.as_ref() {
                            record_pending_room_cancellations(
                                &activity,
                                session,
                                activity_fence,
                            );
                            record_session_command(
                                &activity,
                                session.activity,
                                activity_fence,
                                session.closed_transition(activity::ChannelSessionCloseReason::Local),
                            );
                        }
                        if let Some(session) = active.as_mut() {
                            enqueue_session_persistence(&history, session);
                            history.project(&snapshot);
                        }
                        history.shutdown().await;
                        close_active(&mut active).await;
                        mutate_snapshot(&snapshot, clear_observed_snapshot);
                        emit_snapshot(&emitter, &snapshot);
                        let _ = result_tx.send(());
                        break;
                    }
                }
            }
            update = connect_update_rx.recv() => {
                let Some(update) = update else { continue; };
                let update_attempt = connect_update_attempt(&update);
                let failed = if update_attempt == attempt {
                    match &update {
                        ConnectUpdate::Failed { error, failure, .. } => {
                            Some((*failure, error.to_string()))
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                let ready = update_attempt == attempt
                    && matches!(&update, ConnectUpdate::Ready { .. });
                handle_connect_update(
                    update,
                    ConnectUpdateContext {
                        current_attempt: attempt,
                        connect_cancel: &mut connect_cancel,
                        active: &mut active,
                        snapshot: &snapshot,
                        emitter: &emitter,
                        activity_recorder: &activity,
                        pending_connect_activity: &mut pending_connect_activity,
                        source,
                        store: store.as_ref(),
                        stored_secrets: &mut stored_secrets,
                    },
                ).await;
                if ready {
                    reconnect.deadline = None;
                    reconnect.next_attempt_at_ms = None;
                    reconnect.last_error = None;
                    reconnect.blocked = false;
                    if let Some(session) = active.as_mut() {
                        session.connect_origin = connect_origin;
                        prepare_auto_rejoin(session, &snapshot);
                        let phase = if session.auto_rejoin_queue.is_empty() {
                            ChannelRecoveryPhase::Idle
                        } else {
                            ChannelRecoveryPhase::Rejoining
                        };
                        project_reconnect(&snapshot, &reconnect, phase);
                        let _ = drive_auto_rejoin(
                            session,
                            &snapshot,
                            &emitter,
                            &activity,
                            &identity,
                            &mut stored_secrets,
                            store.as_ref(),
                        ).await;
                    }
                    emit_snapshot(&emitter, &snapshot);
                } else if let Some((failure, error)) = failed {
                    if selected_hub_is_desired(&snapshot) {
                        if retryable_connect_failure(failure) {
                            reconnect.schedule_failure(error, reconnect_jitter());
                            mutate_snapshot(&snapshot, |state| {
                                state.phase = ChannelsPhase::Reconnecting;
                                project_selected_reconnect_state(
                                    state,
                                    &reconnect,
                                    ChannelRecoveryPhase::Scheduled,
                                );
                            });
                        } else {
                            reconnect.block(error);
                            project_reconnect(
                                &snapshot,
                                &reconnect,
                                ChannelRecoveryPhase::Blocked,
                            );
                        }
                        emit_snapshot(&emitter, &snapshot);
                    } else {
                        reconnect.clear();
                    }
                }
            }
            _ = room_transition_tick.tick() => {
                if let Some(active) = active.as_mut() {
                    let now = Instant::now();
                    let mut changed = expire_room_transitions(
                        &mut active.rooms,
                        &mut active.room_activity,
                        active.activity,
                        &activity,
                        now_ms(),
                    );
                    changed |= expire_directory_request(active, now);
                    expire_hub_greeting_resource_state(active, now);
                    active.auto_rejoining.retain(|room| {
                        active
                            .rooms
                            .get(room)
                            .is_some_and(|room| room.phase == ChannelRoomPhase::Joining)
                    });
                    active.pending_join_secrets.retain(|room, _| {
                        active
                            .rooms
                            .get(room)
                            .is_some_and(|room| room.phase == ChannelRoomPhase::Joining)
                    });
                    changed |= drive_auto_rejoin(
                        active,
                        &snapshot,
                        &emitter,
                        &activity,
                        &identity,
                        &mut stored_secrets,
                        store.as_ref(),
                    ).await;
                    if active.auto_rejoin_queue.is_empty()
                        && active.auto_rejoining.is_empty()
                    {
                        changed |= project_reconnect(
                            &snapshot,
                            &reconnect,
                            ChannelRecoveryPhase::Idle,
                        );
                    }
                    if enqueue_session_persistence(&history, active) {
                        changed |= history.project(&snapshot);
                    }
                    if changed {
                        sync_session_snapshot(active, &snapshot);
                        emit_snapshot(&emitter, &snapshot);
                    }
                }
            }
            active_input = receive_active_session_input(active.as_mut()) => {
                match active_input {
                    ActiveSessionInput::Event(event) => match event {
                    Some(event) => {
                        let outcome = handle_link_event(
                            active.as_mut().expect("active branch"),
                            &activity,
                            event,
                        ).await;
                        if let Some(session) = active.as_mut() {
                            enqueue_session_persistence(&history, session);
                            history.project(&snapshot);
                        }
                        match outcome {
                            LinkEventOutcome::Keep => {
                                apply_room_secret_actions(
                                    active.as_mut().expect("active session"),
                                    &identity,
                                    store.as_ref(),
                                    &mut stored_secrets,
                                    &snapshot,
                                )
                                .await;
                                sync_session_snapshot(active.as_ref().expect("active session"), &snapshot);
                            }
                            LinkEventOutcome::Stale => {
                                let session = active.as_ref().expect("active session");
                                record_session_spontaneous(
                                    &activity,
                                    session.activity,
                                    activity::ChannelSessionTransition::Stale,
                                );
                                sync_session_snapshot(active.as_ref().expect("active session"), &snapshot);
                                mutate_snapshot(&snapshot, |state| state.phase = ChannelsPhase::Stale);
                            }
                            LinkEventOutcome::Recovered => {
                                let session = active.as_ref().expect("active session");
                                record_session_spontaneous(
                                    &activity,
                                    session.activity,
                                    activity::ChannelSessionTransition::Recovered,
                                );
                                sync_session_snapshot(active.as_ref().expect("active session"), &snapshot);
                                mutate_snapshot(&snapshot, |state| {
                                    state.phase = ChannelsPhase::Active;
                                    state.last_error = None;
                                });
                            }
                            LinkEventOutcome::Closed {
                                product_reason,
                                activity_reason,
                            } => {
                                let session = active.as_ref().expect("active session");
                                let connected_for = session.connected_at.elapsed();
                                record_lost_room_operations(&activity, session);
                                record_session_spontaneous(
                                    &activity,
                                    session.activity,
                                    session.closed_transition(activity_reason),
                                );
                                active = None;
                                let should_reconnect = selected_hub_is_desired(&snapshot);
                                if should_reconnect {
                                    reconnect.note_session_ended(connected_for);
                                    reconnect.schedule_failure(
                                        product_reason.clone(),
                                        reconnect_jitter(),
                                    );
                                }
                                mutate_snapshot(&snapshot, |state| {
                                    state.phase = if should_reconnect {
                                        ChannelsPhase::Reconnecting
                                    } else {
                                        ChannelsPhase::Error
                                    };
                                    // The nickname belongs to the dead session;
                                    // keeping it would re-offer a name the
                                    // identity may since have retired.
                                    state.nickname = None;
                                    state.rooms.clear();
                                    state.directory = ChannelRoomDirectorySnapshot::default();
                                    state.hub_greeting = None;
                                    state.notices.clear();
                                    state.last_error = Some(product_reason);
                                    if should_reconnect {
                                        project_selected_reconnect_state(
                                            state,
                                            &reconnect,
                                            ChannelRecoveryPhase::Scheduled,
                                        );
                                    }
                                });
                            }
                        }
                        emit_snapshot(&emitter, &snapshot);
                    }
                    None => {
                        let connected_for = active
                            .as_ref()
                            .map(|session| session.connected_at.elapsed())
                            .unwrap_or_default();
                        if let Some(session) = active.as_ref() {
                            record_lost_room_operations(&activity, session);
                            record_session_spontaneous(
                                &activity,
                                session.activity,
                                session.closed_transition(
                                    activity::ChannelSessionCloseReason::StreamEnded,
                                ),
                            );
                        }
                        active = None;
                        let should_reconnect = selected_hub_is_desired(&snapshot);
                        if should_reconnect {
                            reconnect.note_session_ended(connected_for);
                            reconnect.schedule_failure(
                                "Channel link closed".into(),
                                reconnect_jitter(),
                            );
                        }
                        mutate_snapshot(&snapshot, |state| {
                            state.phase = if should_reconnect {
                                ChannelsPhase::Reconnecting
                            } else {
                                ChannelsPhase::Error
                            };
                            state.nickname = None;
                            state.rooms.clear();
                            state.directory = ChannelRoomDirectorySnapshot::default();
                            state.hub_greeting = None;
                            state.notices.clear();
                            state.last_error = Some("Channel link closed".into());
                            if should_reconnect {
                                project_selected_reconnect_state(
                                    state,
                                    &reconnect,
                                    ChannelRecoveryPhase::Scheduled,
                                );
                            }
                        });
                        emit_snapshot(&emitter, &snapshot);
                    }
                    },
                    ActiveSessionInput::ResourceOffer(Some(offer)) => {
                        handle_hub_greeting_resource_offer(
                            active.as_mut().expect("active resource branch"),
                            offer,
                            &greeting_resource_completion_tx,
                        )
                        .await;
                    }
                    ActiveSessionInput::ResourceOffer(None) => {
                        if let Some(active) = active.as_mut() {
                            active.resource_offers_open = false;
                        }
                    }
                }
            }
            completion = greeting_resource_completion_rx.recv() => {
                if let Some(completion) = completion
                    && let Some(active) = active.as_mut()
                    && apply_hub_greeting_resource_completion(active, &activity, completion)
                {
                    sync_session_snapshot(active, &snapshot);
                    emit_snapshot(&emitter, &snapshot);
                }
            }
        }
    }
}

async fn run_connect_attempt(input: ConnectAttemptInput) {
    let ConnectAttemptInput {
        attempt,
        transport_tx,
        identity,
        destination_hash,
        nickname,
        known_hub,
        update_tx,
        cancel_rx,
        activity_context,
    } = input;
    let connect = connect_to_hub(ConnectToHubInput {
        attempt,
        transport_tx,
        identity,
        destination_hash,
        nickname,
        known_hub,
        update_tx: update_tx.clone(),
        activity_context,
    });
    tokio::select! {
        _ = cancel_rx => {}
        result = connect => {
            let update = match result {
                Ok(connected) => ConnectUpdate::Ready { attempt, connected: Box::new(connected) },
                Err(error) => ConnectUpdate::Failed {
                    attempt,
                    error: error.product,
                    failure: error.activity,
                },
            };
            let _ = update_tx.send(update).await;
        }
    }
}

async fn connect_to_hub(input: ConnectToHubInput) -> Result<ConnectedSession, ConnectAttemptError> {
    let ConnectToHubInput {
        attempt,
        transport_tx,
        identity,
        destination_hash,
        nickname,
        known_hub,
        update_tx,
        activity_context,
    } = input;
    send_session_activity_update(
        &update_tx,
        attempt,
        activity::ChannelSessionTransition::PathRequested,
    )
    .await;
    let (public_key, hub_identity, announced_name, hops) = if let Some(known) = known_hub {
        (
            known.public_key,
            known.identity_hash,
            known.announced_name,
            known.hops.max(1),
        )
    } else {
        let announce = rns_runtime::link_session::discover_destination(
            &transport_tx,
            destination_hash,
            CONNECT_PATH_TIMEOUT,
        )
        .await
        .map_err(path_connect_error)?;
        let public_key = announce.public_key.ok_or_else(|| ConnectAttemptError {
            product: ChannelsError::Transport("channel hub announce has no public key".into()),
            activity: ConnectFailure::Failed(
                activity::ChannelSessionFailureReason::InvalidAnnounce,
            ),
        })?;
        let hub_identity = Identity::from_public_key(&public_key)
            .map_err(|error| ConnectAttemptError {
                product: ChannelsError::Transport(error.to_string()),
                activity: ConnectFailure::Failed(
                    activity::ChannelSessionFailureReason::InvalidAnnounce,
                ),
            })?
            .hash;
        (
            public_key,
            hub_identity,
            parse_announce_hub_name(announce.app_data.as_deref()),
            announce.hops.max(1),
        )
    };
    let _ = update_tx
        .send(ConnectUpdate::Discovered {
            attempt,
            hub_identity,
            announced_name: announced_name.clone(),
            hops,
        })
        .await;

    send_session_activity_update(
        &update_tx,
        attempt,
        activity::ChannelSessionTransition::LinkRequested,
    )
    .await;
    let mut session = LinkSession::connect(
        transport_tx,
        identity.clone(),
        LinkSessionConfig {
            destination_hash,
            remote_public_key: public_key,
            hops,
            establishment_timeout: CONNECT_PATH_TIMEOUT,
            client_label: "ratspeak.channels".into(),
            identify: true,
            track_phy_stats: false,
        },
    )
    .await
    .map_err(link_connect_error)?;
    let link = activity::LinkId::new(session.handle.link_id());
    send_session_activity_update(
        &update_tx,
        attempt,
        activity::ChannelSessionTransition::LinkAuthenticated { link },
    )
    .await;
    send_session_activity_update(
        &update_tx,
        attempt,
        activity::ChannelSessionTransition::LinkIdentificationSent { link },
    )
    .await;

    let hello = Envelope::hello(identity.hash, &nickname, env!("CARGO_PKG_VERSION"));
    let hello_bytes = rrc::encode(&hello).map_err(|error| ConnectAttemptError {
        product: ChannelsError::from(error),
        activity: ConnectFailure::Failed(activity::ChannelSessionFailureReason::SendFailed),
    })?;
    session
        .handle
        .send_packet(hello_bytes.clone())
        .await
        .map_err(send_connect_error)?;
    send_connect_envelope_update(
        &update_tx,
        attempt,
        true,
        Some(activity::ChannelMessageToken::random()),
        Some(activity::ChannelEnvelopeKind::Hello),
        hello_bytes.len(),
        activity::SourceValidation::Accepted,
    )
    .await;
    send_session_activity_update(
        &update_tx,
        attempt,
        activity::ChannelSessionTransition::HelloSent {
            encoded_bytes: bounded_encoded_len(hello_bytes.len()),
        },
    )
    .await;
    let _ = update_tx
        .send(ConnectUpdate::AwaitingWelcome {
            attempt,
            link_mdu: session.handle.mdu(),
        })
        .await;

    let mut buffered = Vec::new();
    let wait_for_welcome = async {
        while let Some(event) = session.events.recv().await {
            match event {
                LinkSessionEvent::Packet { data, .. } => {
                    let envelope = match rrc::decode(&data) {
                        Ok(envelope) => envelope,
                        Err(error) => {
                            let unsupported =
                                matches!(error, rrc::ProtocolError::UnsupportedVersion(_));
                            let validation = if unsupported {
                                activity::SourceValidation::Unsupported
                            } else {
                                activity::SourceValidation::Malformed
                            };
                            send_connect_envelope_update(
                                &update_tx,
                                attempt,
                                false,
                                None,
                                None,
                                data.len(),
                                validation,
                            )
                            .await;
                            tracing::debug!(
                                reason = "decode_failed",
                                "ignoring malformed pre-WELCOME channel envelope"
                            );
                            if unsupported {
                                return Err(ConnectAttemptError {
                                    product: ChannelsError::Protocol(
                                        "channel hub uses an unsupported RRC version".into(),
                                    ),
                                    activity: ConnectFailure::WelcomeRejected(
                                        activity::ChannelSessionFailureReason::UnsupportedVersion,
                                    ),
                                });
                            }
                            continue;
                        }
                    };
                    let envelope_kind = channel_envelope_kind(envelope.message_type);
                    let message_token = activity::ChannelMessageToken::random();
                    let must_be_hub = matches!(
                        envelope.message_type,
                        MessageType::Welcome | MessageType::Ping | MessageType::Error
                    );
                    if must_be_hub && envelope.source != hub_identity {
                        send_connect_envelope_update(
                            &update_tx,
                            attempt,
                            false,
                            Some(message_token),
                            envelope_kind,
                            data.len(),
                            activity::SourceValidation::NonHub,
                        )
                        .await;
                        if envelope.message_type == MessageType::Welcome {
                            return Err(ConnectAttemptError {
                                product: ChannelsError::Protocol(
                                    "WELCOME source does not match the authenticated hub".into(),
                                ),
                                activity: ConnectFailure::WelcomeRejected(
                                    activity::ChannelSessionFailureReason::WrongSource,
                                ),
                            });
                        }
                        continue;
                    }
                    match envelope.message_type {
                        MessageType::Welcome => {
                            let welcome = rrc::parse_welcome(&envelope);
                            let max_nick = welcome
                                .limits
                                .max_nick_bytes
                                .unwrap_or(DEFAULT_NICK_MAX_BYTES);
                            rrc::normalize_nickname(&nickname, max_nick).map_err(|error| {
                                ConnectAttemptError {
                                    product: ChannelsError::from(error),
                                    activity: ConnectFailure::WelcomeRejected(
                                        activity::ChannelSessionFailureReason::MalformedWelcome,
                                    ),
                                }
                            })?;
                            send_connect_envelope_update(
                                &update_tx,
                                attempt,
                                false,
                                Some(message_token),
                                envelope_kind,
                                data.len(),
                                activity::SourceValidation::Accepted,
                            )
                            .await;
                            send_session_activity_update(
                                &update_tx,
                                attempt,
                                activity::ChannelSessionTransition::WelcomeValidated {
                                    encoded_bytes: bounded_encoded_len(data.len()),
                                },
                            )
                            .await;
                            send_session_activity_update(
                                &update_tx,
                                attempt,
                                activity::ChannelSessionTransition::Negotiated {
                                    protocol_version: envelope.version,
                                    capabilities: activity::ChannelNegotiatedCapabilities {
                                        actions: welcome
                                            .capabilities
                                            .get(&rrc::CAP_ACTION)
                                            .copied()
                                            .unwrap_or(false),
                                        direct_notices: welcome
                                            .capabilities
                                            .get(&rrc::CAP_DIRECT_NOTICE)
                                            .copied()
                                            .unwrap_or(false),
                                        resource_envelopes: welcome
                                            .capabilities
                                            .get(&rrc::CAP_RESOURCE_ENVELOPE)
                                            .copied()
                                            .unwrap_or(false),
                                    },
                                    limits: negotiated_limits(&welcome.limits),
                                    link_mdu: session.handle.mdu() as u64,
                                },
                            )
                            .await;
                            return Ok(welcome);
                        }
                        MessageType::Ping => {
                            send_connect_envelope_update(
                                &update_tx,
                                attempt,
                                false,
                                Some(message_token),
                                envelope_kind,
                                data.len(),
                                activity::SourceValidation::Accepted,
                            )
                            .await;
                            let pong = Envelope::pong(identity.hash, &envelope);
                            let pong_bytes =
                                rrc::encode(&pong).map_err(|error| ConnectAttemptError {
                                    product: ChannelsError::from(error),
                                    activity: ConnectFailure::Failed(
                                        activity::ChannelSessionFailureReason::SendFailed,
                                    ),
                                })?;
                            session
                                .handle
                                .send_packet(pong_bytes.clone())
                                .await
                                .map_err(send_connect_error)?;
                            send_connect_envelope_update(
                                &update_tx,
                                attempt,
                                true,
                                Some(activity::ChannelMessageToken::random()),
                                Some(activity::ChannelEnvelopeKind::Pong),
                                pong_bytes.len(),
                                activity::SourceValidation::Accepted,
                            )
                            .await;
                        }
                        MessageType::Error => {
                            send_connect_envelope_update(
                                &update_tx,
                                attempt,
                                false,
                                Some(message_token),
                                envelope_kind,
                                data.len(),
                                activity::SourceValidation::Accepted,
                            )
                            .await;
                            return Err(ConnectAttemptError {
                                product: ChannelsError::HubRejected(
                                    rrc::text_body(&envelope)
                                        .unwrap_or("connection rejected")
                                        .to_string(),
                                ),
                                activity: ConnectFailure::WelcomeRejected(
                                    activity::ChannelSessionFailureReason::HubRejected,
                                ),
                            });
                        }
                        MessageType::ResourceEnvelope | MessageType::Unknown(_) => {
                            send_connect_envelope_update(
                                &update_tx,
                                attempt,
                                false,
                                Some(message_token),
                                envelope_kind,
                                data.len(),
                                activity::SourceValidation::Unsupported,
                            )
                            .await;
                        }
                        MessageType::Notice => {
                            send_connect_envelope_update(
                                &update_tx,
                                attempt,
                                false,
                                Some(message_token),
                                envelope_kind,
                                data.len(),
                                activity::SourceValidation::Accepted,
                            )
                            .await;
                            buffered.push((envelope, bounded_encoded_len(data.len())));
                        }
                        _ => {
                            send_connect_envelope_update(
                                &update_tx,
                                attempt,
                                false,
                                Some(message_token),
                                envelope_kind,
                                data.len(),
                                activity::SourceValidation::Accepted,
                            )
                            .await;
                        }
                    }
                }
                LinkSessionEvent::Closed { reason } => {
                    send_session_activity_update(
                        &update_tx,
                        attempt,
                        activity::ChannelSessionTransition::Closed {
                            reason: channel_close_reason(reason),
                            link: Some(link),
                            duration_ms: None,
                        },
                    )
                    .await;
                    return Err(ConnectAttemptError {
                        product: ChannelsError::Transport(format!(
                            "link closed before WELCOME ({})",
                            close_reason_label(reason)
                        )),
                        activity: ConnectFailure::Failed(
                            activity::ChannelSessionFailureReason::TransportUnavailable,
                        ),
                    });
                }
                LinkSessionEvent::Stale => {
                    send_session_activity_update(
                        &update_tx,
                        attempt,
                        activity::ChannelSessionTransition::Stale,
                    )
                    .await;
                    return Err(ConnectAttemptError {
                        product: ChannelsError::Transport(
                            "channel link became stale before WELCOME".into(),
                        ),
                        activity: ConnectFailure::Failed(
                            activity::ChannelSessionFailureReason::TransportUnavailable,
                        ),
                    });
                }
                LinkSessionEvent::Recovered
                | LinkSessionEvent::PacketDelivered { .. }
                | LinkSessionEvent::RequestConcluded { .. }
                | LinkSessionEvent::ResourceStarted { .. }
                | LinkSessionEvent::ResourceProgress { .. }
                | LinkSessionEvent::ResourceConcluded { .. } => {}
            }
        }
        Err(ConnectAttemptError {
            product: ChannelsError::Transport("channel link closed before WELCOME".into()),
            activity: ConnectFailure::Failed(
                activity::ChannelSessionFailureReason::TransportUnavailable,
            ),
        })
    };

    let welcome = tokio::time::timeout(WELCOME_TIMEOUT, wait_for_welcome)
        .await
        .map_err(|_| ConnectAttemptError {
            product: ChannelsError::Transport("timed out waiting for WELCOME".into()),
            activity: ConnectFailure::WelcomeRejected(
                activity::ChannelSessionFailureReason::WelcomeTimedOut,
            ),
        })??;

    Ok(ConnectedSession {
        session,
        destination_hash,
        hub_identity,
        announced_name,
        hops,
        nickname,
        welcome,
        buffered,
        activity: activity_context,
    })
}

async fn send_session_activity_update(
    update_tx: &mpsc::Sender<ConnectUpdate>,
    attempt: u64,
    transition: activity::ChannelSessionTransition,
) {
    let _ = update_tx
        .send(ConnectUpdate::SessionActivity {
            attempt,
            transition,
        })
        .await;
}

async fn send_connect_envelope_update(
    update_tx: &mpsc::Sender<ConnectUpdate>,
    attempt: u64,
    outbound: bool,
    message: Option<activity::ChannelMessageToken>,
    envelope_kind: Option<activity::ChannelEnvelopeKind>,
    encoded_bytes: usize,
    validation: activity::SourceValidation,
) {
    let _ = update_tx
        .send(ConnectUpdate::EnvelopeActivity {
            attempt,
            outbound,
            message,
            envelope_kind,
            encoded_bytes: bounded_encoded_len(encoded_bytes),
            validation,
        })
        .await;
}

fn bounded_encoded_len(encoded_bytes: usize) -> u32 {
    encoded_bytes.min(u32::MAX as usize) as u32
}

fn path_connect_error(error: LinkSessionError) -> ConnectAttemptError {
    let failure = if matches!(error, LinkSessionError::Timeout(_)) {
        ConnectFailure::PathTimedOut
    } else {
        ConnectFailure::Failed(activity::ChannelSessionFailureReason::PathLookupFailed)
    };
    ConnectAttemptError {
        product: ChannelsError::from(error),
        activity: failure,
    }
}

fn link_connect_error(error: LinkSessionError) -> ConnectAttemptError {
    let reason = match error {
        LinkSessionError::ProofInvalid(_)
        | LinkSessionError::HandshakeFailed(_)
        | LinkSessionError::Timeout(_) => {
            activity::ChannelSessionFailureReason::AuthenticationFailed
        }
        LinkSessionError::IdentificationUnavailable => {
            activity::ChannelSessionFailureReason::IdentificationFailed
        }
        LinkSessionError::PublicKeyUnavailable => {
            activity::ChannelSessionFailureReason::InvalidAnnounce
        }
        LinkSessionError::TransportUnavailable | LinkSessionError::SessionClosed => {
            activity::ChannelSessionFailureReason::TransportUnavailable
        }
        LinkSessionError::LinkCrypto
        | LinkSessionError::LinkNotActive
        | LinkSessionError::PayloadTooLarge { .. }
        | LinkSessionError::RequestRequiresResource { .. }
        | LinkSessionError::RequestResourceFailed(_)
        | LinkSessionError::TooManyPendingRequests => {
            activity::ChannelSessionFailureReason::SendFailed
        }
    };
    ConnectAttemptError {
        product: ChannelsError::from(error),
        activity: ConnectFailure::Failed(reason),
    }
}

fn send_connect_error(error: LinkSessionError) -> ConnectAttemptError {
    ConnectAttemptError {
        product: ChannelsError::from(error),
        activity: ConnectFailure::Failed(activity::ChannelSessionFailureReason::SendFailed),
    }
}

fn negotiated_limits(limits: &HubLimits) -> activity::ChannelNegotiatedLimits {
    activity::ChannelNegotiatedLimits {
        max_nick_bytes: limits.max_nick_bytes.map(|value| value as u64),
        max_room_bytes: limits.max_room_name_bytes.map(|value| value as u64),
        max_message_bytes: limits.max_message_body_bytes.map(|value| value as u64),
        max_rooms: limits.max_rooms_per_session.map(|value| value as u64),
        rate_per_minute: limits.rate_messages_per_minute.map(|value| value as u64),
    }
}

fn channel_envelope_kind(message_type: MessageType) -> Option<activity::ChannelEnvelopeKind> {
    Some(match message_type {
        MessageType::Hello => activity::ChannelEnvelopeKind::Hello,
        MessageType::Welcome => activity::ChannelEnvelopeKind::Welcome,
        MessageType::Join => activity::ChannelEnvelopeKind::Join,
        MessageType::Joined => activity::ChannelEnvelopeKind::Joined,
        MessageType::Part => activity::ChannelEnvelopeKind::Part,
        MessageType::Parted => activity::ChannelEnvelopeKind::Parted,
        MessageType::Message => activity::ChannelEnvelopeKind::Message,
        MessageType::Notice => activity::ChannelEnvelopeKind::Notice,
        MessageType::Action => activity::ChannelEnvelopeKind::Action,
        MessageType::Ping => activity::ChannelEnvelopeKind::Ping,
        MessageType::Pong => activity::ChannelEnvelopeKind::Pong,
        MessageType::Error => activity::ChannelEnvelopeKind::Error,
        MessageType::ResourceEnvelope => activity::ChannelEnvelopeKind::Resource,
        MessageType::Unknown(_) => return None,
    })
}

async fn handle_connect_update(update: ConnectUpdate, context: ConnectUpdateContext<'_>) {
    let ConnectUpdateContext {
        current_attempt,
        connect_cancel,
        active,
        snapshot,
        emitter,
        activity_recorder,
        pending_connect_activity,
        source,
        store,
        stored_secrets,
    } = context;
    let update_attempt = connect_update_attempt(&update);
    if update_attempt != current_attempt {
        if let ConnectUpdate::Ready { connected, .. } = update {
            connected.session.handle.close().await;
        }
        return;
    }

    match update {
        ConnectUpdate::SessionActivity { transition, .. } => {
            if let Some(context) = *pending_connect_activity {
                record_session_origin(activity_recorder, context, transition);
            }
            return;
        }
        ConnectUpdate::EnvelopeActivity {
            outbound,
            message,
            envelope_kind,
            encoded_bytes,
            validation,
            ..
        } => {
            if let Some(context) = *pending_connect_activity {
                activity_recorder.record_fenced(context.origin, move || {
                    let input = activity::ChannelsEnvelopeActivity {
                        hub: context.hub,
                        room: None,
                        message,
                        envelope_kind,
                        encoded_bytes,
                        validation,
                        correlation_id: context.correlation_id,
                    };
                    if outbound {
                        activity::channels_envelope_sent(input)
                    } else {
                        activity::channels_envelope_received(input)
                    }
                });
            }
            return;
        }
        ConnectUpdate::Discovered {
            hub_identity,
            announced_name,
            hops,
            ..
        } => {
            if let Some(context) = *pending_connect_activity {
                record_session_origin(
                    activity_recorder,
                    context,
                    activity::ChannelSessionTransition::PathDiscovered { hops },
                );
            }
            mutate_snapshot(snapshot, |state| {
                state.phase = ChannelsPhase::Connecting;
                if let Some(hub) = state.hub.as_mut() {
                    hub.identity_hash = Some(hex::encode(hub_identity));
                    hub.announced_name = announced_name;
                    hub.hops = Some(hops);
                }
            });
        }
        ConnectUpdate::AwaitingWelcome { link_mdu, .. } => {
            mutate_snapshot(snapshot, |state| {
                state.phase = ChannelsPhase::AwaitingWelcome;
                if let Some(hub) = state.hub.as_mut() {
                    hub.link_mdu = Some(link_mdu);
                }
            });
        }
        ConnectUpdate::Failed { error, failure, .. } => {
            *connect_cancel = None;
            if let Some(context) = pending_connect_activity.take() {
                let transition = match failure {
                    ConnectFailure::PathTimedOut => {
                        activity::ChannelSessionTransition::PathTimedOut
                    }
                    ConnectFailure::WelcomeRejected(reason) => {
                        activity::ChannelSessionTransition::WelcomeRejected { reason }
                    }
                    ConnectFailure::Failed(reason) => {
                        activity::ChannelSessionTransition::Failed { reason }
                    }
                };
                record_session_origin(activity_recorder, context, transition);
            }
            mutate_snapshot(snapshot, |state| {
                state.phase = ChannelsPhase::Error;
                state.nickname = None;
                state.rooms.clear();
                state.directory = ChannelRoomDirectorySnapshot::default();
                state.hub_greeting = None;
                state.notices.clear();
                state.last_error = Some(error.to_string());
            });
        }
        ConnectUpdate::Ready { connected, .. } => {
            *connect_cancel = None;
            *pending_connect_activity = None;
            let ConnectedSession {
                session,
                destination_hash,
                hub_identity,
                announced_name,
                hops,
                nickname,
                welcome,
                buffered,
                activity,
            } = *connected;
            let capabilities = ChannelHubCapabilitiesSnapshot::from(&welcome);
            let limits_snapshot = ChannelHubLimitsSnapshot::from(&welcome.limits);
            let destination_text = hex::encode(destination_hash);
            let durable_label = welcome
                .hub_name
                .clone()
                .or_else(|| announced_name.clone())
                .unwrap_or_default();
            let LinkSession {
                handle,
                events,
                resource_offers,
            } = session;
            let mut live = ActiveSession {
                handle,
                events,
                resource_offers,
                resource_offers_open: true,
                connected_at: Instant::now(),
                source,
                destination_hash,
                hub_identity,
                nickname: nickname.clone(),
                supports_action: capabilities.actions,
                supports_resources: capabilities.resource_envelopes,
                limits: welcome.limits.clone(),
                rooms: BTreeMap::new(),
                directory: ChannelRoomDirectorySnapshot::default(),
                directory_request_deadline: None,
                directory_last_requested_at: None,
                hub_greeting: None,
                hub_greeting_deadline: Instant::now() + HUB_GREETING_WINDOW,
                hub_greeting_notice_may_continue: false,
                greeting_resource_expectation: None,
                greeting_resource_in_flight: None,
                notices: VecDeque::new(),
                seen_ids: HashSet::new(),
                seen_order: VecDeque::new(),
                message_tokens: HashMap::new(),
                message_token_order: VecDeque::new(),
                room_activity: BTreeMap::new(),
                auto_rejoin_queue: VecDeque::new(),
                auto_rejoining: HashSet::new(),
                pending_join_secrets: BTreeMap::new(),
                room_secret_actions: VecDeque::new(),
                history_events: VecDeque::new(),
                participant_observations: VecDeque::new(),
                connect_origin: ConnectOrigin::User,
                activity,
            };
            for (envelope, encoded_bytes) in buffered {
                let _ =
                    handle_envelope(&mut live, activity_recorder, envelope, encoded_bytes).await;
            }
            mutate_snapshot(snapshot, |state| {
                clear_observed_snapshot(state);
                state.phase = ChannelsPhase::Active;
                state.nickname = Some(nickname.clone());
                state.hub = Some(ChannelHubSnapshot {
                    destination_hash: destination_text.clone(),
                    identity_hash: Some(hex::encode(hub_identity)),
                    announced_name: announced_name.clone(),
                    name: welcome.hub_name,
                    version: welcome.hub_version,
                    hops: Some(hops),
                    link_mdu: Some(live.handle.mdu()),
                    connected_at_ms: Some(now_ms()),
                    capabilities,
                    limits: limits_snapshot,
                });
            });
            sync_session_snapshot(&live, snapshot);
            *active = Some(live);
            if let Some(store) = store {
                apply_store_result(
                    snapshot,
                    stored_secrets,
                    store
                        .note_connected(destination_text, durable_label, nickname)
                        .await,
                );
            }
        }
    }
    emit_snapshot(emitter, snapshot);
}

async fn join_room(
    active: Option<&mut ActiveSession>,
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    emitter: &Arc<dyn Emitter>,
    activity_recorder: &ChannelsActivity,
    room: String,
    secret: JoinSecretInput,
    activity_fence: Option<ActivityRequestFence>,
) -> Result<String, ChannelsError> {
    let active = active.ok_or(ChannelsError::NotConnected)?;
    let max_room = active
        .limits
        .max_room_name_bytes
        .unwrap_or(DEFAULT_ROOM_MAX_BYTES);
    let room = rrc::normalize_room(&room, max_room)?;
    if let Some(existing) = active.rooms.get(&room) {
        match existing.phase {
            ChannelRoomPhase::Joined => return Ok(room),
            ChannelRoomPhase::Joining | ChannelRoomPhase::Parting => {
                return Err(ChannelsError::AlreadyJoining(room));
            }
            // A hub ERROR leaves the failed room visible for explanation, but
            // the next explicit join must be a real retry on the wire.
            ChannelRoomPhase::Error => {
                active.rooms.remove(&room);
                active.room_activity.remove(&room);
                active.pending_join_secrets.remove(&room);
            }
        }
    }
    if let Some(pending) = active
        .rooms
        .values()
        .find(|candidate| candidate.phase == ChannelRoomPhase::Joining)
    {
        return Err(ChannelsError::AlreadyJoining(pending.name.clone()));
    }
    if active.limits.max_rooms_per_session.is_some_and(|limit| {
        active
            .rooms
            .values()
            .filter(|room| room.phase != ChannelRoomPhase::Error)
            .count()
            >= limit
    }) {
        return Err(ChannelsError::RoomLimitReached);
    }

    let room_token = activity::ChannelRoomToken::random();
    let operation = RoomOperationContext {
        correlation_id: CorrelationId::random(),
        origin: activity_fence,
    };
    active
        .rooms
        .insert(room.clone(), ChannelRoomSnapshot::joining(room.clone()));
    active.room_activity.insert(
        room.clone(),
        RoomActivityContext {
            token: room_token,
            join: Some(operation),
            part: None,
        },
    );
    record_room_operation(
        activity_recorder,
        active.activity,
        room_token,
        operation,
        activity::ChannelRoomTransition::JoinRequested,
    );
    let mut envelope =
        Envelope::room_command(MessageType::Join, active.source, &room, &active.nickname);
    let key = match &secret {
        JoinSecretInput::None => None,
        JoinSecretInput::User { key, .. } | JoinSecretInput::Stored(key) => Some(key.as_str()),
    };
    if let Some(key) = key {
        if key.len() > MAX_JOIN_KEY_BYTES {
            active.rooms.remove(&room);
            active.room_activity.remove(&room);
            return Err(ChannelsError::JoinKeyTooLong(MAX_JOIN_KEY_BYTES));
        }
        envelope.body = Some(Value::Text(key.to_string()));
    }
    let send_result =
        send_active_envelope(active, activity_recorder, &envelope, activity_fence).await;
    if let Some(Value::Text(key)) = envelope.body.as_mut() {
        key.zeroize();
    }
    if let Err(error) = send_result {
        active.rooms.remove(&room);
        active.room_activity.remove(&room);
        record_room_operation(
            activity_recorder,
            active.activity,
            room_token,
            operation,
            activity::ChannelRoomTransition::JoinRejected {
                reason: activity::ChannelRoomFailureReason::SendFailed,
            },
        );
        return Err(error);
    }
    match secret {
        JoinSecretInput::None => {}
        JoinSecretInput::User {
            key,
            remember: true,
        } => {
            active
                .pending_join_secrets
                .insert(room.clone(), PendingJoinSecret::UserRemember(key));
        }
        JoinSecretInput::User {
            remember: false, ..
        } => {
            active
                .pending_join_secrets
                .insert(room.clone(), PendingJoinSecret::UserEphemeral);
        }
        JoinSecretInput::Stored(_) => {
            active
                .pending_join_secrets
                .insert(room.clone(), PendingJoinSecret::Stored);
        }
    }
    sync_session_snapshot(active, snapshot);
    emit_snapshot(emitter, snapshot);
    Ok(room)
}

async fn part_room(
    active: Option<&mut ActiveSession>,
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    emitter: &Arc<dyn Emitter>,
    activity_recorder: &ChannelsActivity,
    room: String,
    activity_fence: Option<ActivityRequestFence>,
) -> Result<(), ChannelsError> {
    let active = active.ok_or(ChannelsError::NotConnected)?;
    let max_room = active
        .limits
        .max_room_name_bytes
        .unwrap_or(DEFAULT_ROOM_MAX_BYTES);
    let room = rrc::normalize_room(&room, max_room)?;
    active.pending_join_secrets.remove(&room);
    let prior = active
        .rooms
        .get(&room)
        .map(|room| room.phase)
        .ok_or_else(|| ChannelsError::NotJoined(room.clone()))?;
    let room_token = active
        .room_activity
        .get(&room)
        .map(|context| context.token)
        .unwrap_or_else(activity::ChannelRoomToken::random);
    if prior == ChannelRoomPhase::Joining
        && let Some(join) = active
            .room_activity
            .get_mut(&room)
            .and_then(|context| context.join.take())
    {
        record_room_operation(
            activity_recorder,
            active.activity,
            room_token,
            RoomOperationContext {
                origin: activity_fence,
                ..join
            },
            activity::ChannelRoomTransition::JoinCancelled,
        );
    }
    let operation = RoomOperationContext {
        correlation_id: CorrelationId::random(),
        origin: activity_fence,
    };
    active
        .room_activity
        .entry(room.clone())
        .and_modify(|context| context.part = Some(operation))
        .or_insert(RoomActivityContext {
            token: room_token,
            join: None,
            part: Some(operation),
        });
    record_room_operation(
        activity_recorder,
        active.activity,
        room_token,
        operation,
        activity::ChannelRoomTransition::PartRequested,
    );
    if let Some(room_state) = active.rooms.get_mut(&room) {
        room_state.phase = ChannelRoomPhase::Parting;
        room_state.phase_started_at_ms = now_ms();
        room_state.last_error = None;
    }
    let envelope =
        Envelope::room_command(MessageType::Part, active.source, &room, &active.nickname);
    if let Err(error) =
        send_active_envelope(active, activity_recorder, &envelope, activity_fence).await
    {
        if let Some(room_state) = active.rooms.get_mut(&room) {
            room_state.phase = prior;
        }
        if let Some(context) = active.room_activity.get_mut(&room) {
            context.part = None;
        }
        record_room_operation(
            activity_recorder,
            active.activity,
            room_token,
            operation,
            activity::ChannelRoomTransition::PartRejected {
                reason: activity::ChannelRoomFailureReason::SendFailed,
            },
        );
        return Err(error);
    }
    sync_session_snapshot(active, snapshot);
    emit_snapshot(emitter, snapshot);
    Ok(())
}

async fn send_room_text(
    active: Option<&mut ActiveSession>,
    activity_recorder: &ChannelsActivity,
    room: String,
    text: String,
    activity_fence: Option<ActivityRequestFence>,
) -> Result<(), ChannelsError> {
    let active = active.ok_or(ChannelsError::NotConnected)?;
    if text.trim().is_empty() {
        return Err(ChannelsError::EmptyMessage);
    }
    let max_room = active
        .limits
        .max_room_name_bytes
        .unwrap_or(DEFAULT_ROOM_MAX_BYTES);
    let room = rrc::normalize_room(&room, max_room)?;
    if !active
        .rooms
        .get(&room)
        .is_some_and(|room| room.phase == ChannelRoomPhase::Joined)
    {
        return Err(ChannelsError::NotJoined(room));
    }

    let (message_type, body) = if active.supports_action {
        text.strip_prefix("/me ")
            .filter(|action| !action.trim().is_empty())
            .map(|action| (MessageType::Action, action.to_string()))
            .unwrap_or((MessageType::Message, text))
    } else {
        (MessageType::Message, text)
    };
    let max_message = active
        .limits
        .max_message_body_bytes
        .unwrap_or(DEFAULT_MESSAGE_MAX_BYTES);
    if body.len() > max_message {
        return Err(ChannelsError::MessageTooLong(max_message));
    }
    let envelope = Envelope::room_text(message_type, active.source, &room, &active.nickname, &body);
    send_active_envelope(active, activity_recorder, &envelope, activity_fence)
        .await
        .map(|_| ())
}

async fn refresh_room_directory(
    active: Option<&mut ActiveSession>,
    activity_recorder: &ChannelsActivity,
    activity_fence: Option<ActivityRequestFence>,
) -> Result<(), ChannelsError> {
    let active = active.ok_or(ChannelsError::NotConnected)?;
    let now = Instant::now();
    if active.directory_request_deadline.is_some() {
        return Ok(());
    }
    if active
        .directory_last_requested_at
        .is_some_and(|last| now.saturating_duration_since(last) < DIRECTORY_REFRESH_COOLDOWN)
    {
        return Ok(());
    }

    let mut envelope = Envelope::new(MessageType::Message, active.source);
    envelope.nickname = Some(active.nickname.clone());
    envelope.body = Some(Value::Text("/list".into()));
    active.directory_last_requested_at = Some(now);
    match send_active_envelope(active, activity_recorder, &envelope, activity_fence).await {
        Ok(_) => {
            active.directory.phase = ChannelRoomDirectoryPhase::Loading;
            active.directory.last_error = None;
            active.directory_request_deadline = Some(now + DIRECTORY_REFRESH_TIMEOUT);
            Ok(())
        }
        Err(error) => {
            active.directory.phase = ChannelRoomDirectoryPhase::Error;
            active.directory.last_error =
                Some("Could not request public channels from this hub".into());
            active.directory_request_deadline = None;
            Err(error)
        }
    }
}

fn expire_directory_request(active: &mut ActiveSession, now: Instant) -> bool {
    if active
        .directory_request_deadline
        .is_none_or(|deadline| deadline > now)
    {
        return false;
    }
    active.directory_request_deadline = None;
    active.directory.phase = ChannelRoomDirectoryPhase::Error;
    active.directory.last_error = Some("The hub did not answer the public channel request".into());
    true
}

enum LinkEventOutcome {
    Keep,
    Stale,
    Recovered,
    Closed {
        product_reason: String,
        activity_reason: activity::ChannelSessionCloseReason,
    },
}

enum ActiveSessionInput {
    Event(Option<LinkSessionEvent>),
    ResourceOffer(Option<LinkSessionResourceOffer>),
}

async fn receive_active_session_input(active: Option<&mut ActiveSession>) -> ActiveSessionInput {
    let Some(active) = active else {
        return pending::<ActiveSessionInput>().await;
    };
    if !active.resource_offers_open {
        return ActiveSessionInput::Event(active.events.recv().await);
    }
    let events = &mut active.events;
    let resource_offers = &mut active.resource_offers;
    // The Link actor emits the application packet before it emits the
    // advertisement offer. Prefer that packet when both are ready so the
    // authenticated RESOURCE_ENVELOPE installs the admission expectation
    // before the transfer asks for a decision.
    tokio::select! {
        biased;
        event = events.recv() => ActiveSessionInput::Event(event),
        offer = resource_offers.recv() => ActiveSessionInput::ResourceOffer(offer),
    }
}

fn accept_hub_greeting_resource_envelope(
    active: &mut ActiveSession,
    envelope: &Envelope,
    encoded_bytes: u32,
) -> activity::SourceValidation {
    active.hub_greeting_notice_may_continue = false;
    if !active.supports_resources
        || Instant::now() > active.hub_greeting_deadline
        || active.greeting_resource_in_flight.is_some()
    {
        return activity::SourceValidation::Unsupported;
    }
    let expectation = match hub_greeting_resource_expectation(envelope, encoded_bytes) {
        Ok(expectation) => expectation,
        Err(validation) => return validation,
    };
    if active
        .greeting_resource_expectation
        .as_ref()
        .is_some_and(|pending| pending.announcement_id != expectation.announcement_id)
    {
        return activity::SourceValidation::Unsupported;
    }
    active.greeting_resource_expectation = Some(expectation);
    activity::SourceValidation::Accepted
}

fn hub_greeting_resource_expectation(
    envelope: &Envelope,
    encoded_bytes: u32,
) -> Result<HubGreetingResourceExpectation, activity::SourceValidation> {
    if envelope.room.is_some() {
        return Err(activity::SourceValidation::Unsupported);
    }
    let body = rrc::parse_resource_envelope(envelope).map_err(|error| {
        tracing::debug!(
            reason = %error,
            "ignoring malformed channel hub greeting resource envelope"
        );
        activity::SourceValidation::Malformed
    })?;
    let size = usize::try_from(body.size).map_err(|_| activity::SourceValidation::Unsupported)?;
    if body.kind != "motd"
        || size > HUB_GREETING_RESOURCE_MAX_BYTES
        || body
            .encoding
            .as_deref()
            .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("utf-8"))
    {
        return Err(activity::SourceValidation::Unsupported);
    }
    Ok(HubGreetingResourceExpectation {
        announcement_id: body.id,
        size,
        sha256: body.sha256,
        encoding: body.encoding,
        encoded_bytes,
        created_at: Instant::now(),
    })
}

fn greeting_resource_offer_matches(
    active: &ActiveSession,
    offer: &LinkSessionResourceOffer,
    expectation: &HubGreetingResourceExpectation,
) -> bool {
    let now = Instant::now();
    offer.link_id() == active.handle.link_id()
        && active.greeting_resource_in_flight.is_none()
        && now <= active.hub_greeting_deadline
        && now.duration_since(expectation.created_at) <= HUB_GREETING_RESOURCE_TIMEOUT
        && offer.data_size() == expectation.size
        && offer.transfer_size() > 0
        && offer.transfer_size()
            <= expectation
                .size
                .saturating_add(HUB_GREETING_RESOURCE_TRANSFER_SLACK)
        && offer.total_segments() == 1
        && offer.request_id().is_none()
        && !offer.is_request()
        && !offer.is_response()
}

async fn reject_greeting_resource_offer(offer: LinkSessionResourceOffer, reason: &'static str) {
    if offer.reject().await.is_err() {
        tracing::debug!(reason, "failed to reject channel Resource offer");
    }
}

async fn handle_hub_greeting_resource_offer(
    active: &mut ActiveSession,
    offer: LinkSessionResourceOffer,
    completion_tx: &mpsc::Sender<HubGreetingResourceCompletion>,
) {
    let Some(expectation) = active.greeting_resource_expectation.as_ref() else {
        reject_greeting_resource_offer(offer, "no_authenticated_motd_expectation").await;
        return;
    };
    if !greeting_resource_offer_matches(active, &offer, expectation) {
        reject_greeting_resource_offer(offer, "motd_offer_failed_admission").await;
        return;
    }

    let expectation = active
        .greeting_resource_expectation
        .take()
        .expect("validated greeting resource expectation");
    let link_id = offer.link_id();
    let resource_id = offer.resource_id();
    match offer.accept().await {
        Ok(resource) => {
            active.greeting_resource_in_flight = Some(HubGreetingResourceInFlight {
                resource_id,
                started_at: Instant::now(),
            });
            let completion_tx = completion_tx.clone();
            tokio::spawn(async move {
                let result = resource
                    .concluded()
                    .await
                    .map_err(|error| error.to_string());
                let _ = completion_tx
                    .send(HubGreetingResourceCompletion {
                        link_id,
                        resource_id,
                        expectation,
                        result,
                    })
                    .await;
            });
        }
        Err(_) => {
            tracing::debug!("failed to accept authenticated channel hub greeting Resource");
            if expectation.created_at.elapsed() <= HUB_GREETING_RESOURCE_TIMEOUT {
                active.greeting_resource_expectation = Some(expectation);
            }
        }
    }
}

fn apply_hub_greeting_resource_completion(
    active: &mut ActiveSession,
    activity_recorder: &ChannelsActivity,
    completion: HubGreetingResourceCompletion,
) -> bool {
    let Some(in_flight) = active.greeting_resource_in_flight.as_ref() else {
        return false;
    };
    if completion.link_id != active.handle.link_id()
        || completion.resource_id != in_flight.resource_id
    {
        return false;
    }
    let timed_out = in_flight.started_at.elapsed() > HUB_GREETING_RESOURCE_TIMEOUT;
    active.greeting_resource_in_flight = None;
    if timed_out {
        return false;
    }
    let received = match completion.result {
        Ok(received) => received,
        Err(error) => {
            tracing::debug!(
                announcement_id = %hex::encode(completion.expectation.announcement_id),
                reason = %error,
                "channel hub greeting Resource did not conclude"
            );
            return false;
        }
    };
    if received.link_id != completion.link_id
        || received.resource_id != completion.resource_id
        || received.data.len() != completion.expectation.size
        || received.data.is_empty()
        || received.data.len() > HUB_GREETING_RESOURCE_MAX_BYTES
        || received.metadata.is_some()
        || received.total_segments != 1
        || received.request_id.is_some()
        || received.is_request
        || received.is_response
    {
        tracing::debug!(
            announcement_id = %hex::encode(completion.expectation.announcement_id),
            "channel hub greeting Resource conclusion failed structural validation"
        );
        return false;
    }
    let digest = rns_crypto::sha::sha256(&received.data);
    if completion
        .expectation
        .sha256
        .is_some_and(|expected| expected != digest)
    {
        tracing::debug!(
            announcement_id = %hex::encode(completion.expectation.announcement_id),
            "channel hub greeting Resource digest did not match its envelope"
        );
        return false;
    }
    if completion
        .expectation
        .encoding
        .as_deref()
        .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("utf-8"))
    {
        return false;
    }
    let Ok(text) = String::from_utf8(received.data) else {
        tracing::debug!(
            announcement_id = %hex::encode(completion.expectation.announcement_id),
            "channel hub greeting Resource is not valid UTF-8"
        );
        return false;
    };
    active.hub_greeting_notice_may_continue = false;
    active.greeting_resource_expectation = None;
    active.hub_greeting = Some(ChannelHubGreetingSnapshot {
        text,
        received_at_ms: now_ms(),
        source_hash: hex::encode(active.hub_identity),
        delivery: ChannelHubGreetingDelivery::Resource,
        completeness: ChannelHubGreetingCompleteness::Complete,
    });
    record_session_spontaneous(
        activity_recorder,
        active.activity,
        activity::ChannelSessionTransition::GreetingObserved {
            encoded_bytes: completion.expectation.encoded_bytes,
        },
    );
    true
}

fn expire_hub_greeting_resource_state(active: &mut ActiveSession, now: Instant) {
    if active
        .greeting_resource_expectation
        .as_ref()
        .is_some_and(|pending| {
            now > active.hub_greeting_deadline
                || now.duration_since(pending.created_at) > HUB_GREETING_RESOURCE_TIMEOUT
        })
    {
        active.greeting_resource_expectation = None;
    }
    if active
        .greeting_resource_in_flight
        .as_ref()
        .is_some_and(|in_flight| {
            now.duration_since(in_flight.started_at) > HUB_GREETING_RESOURCE_TIMEOUT
        })
    {
        active.greeting_resource_in_flight = None;
    }
    if now > active.hub_greeting_deadline {
        active.hub_greeting_notice_may_continue = false;
    }
}

async fn handle_link_event(
    active: &mut ActiveSession,
    activity_recorder: &ChannelsActivity,
    event: LinkSessionEvent,
) -> LinkEventOutcome {
    match event {
        LinkSessionEvent::Packet { data, .. } => match rrc::decode(&data) {
            Ok(envelope) => {
                handle_envelope(
                    active,
                    activity_recorder,
                    envelope,
                    bounded_encoded_len(data.len()),
                )
                .await;
                LinkEventOutcome::Keep
            }
            Err(error) => {
                let context = active.activity;
                let validation = if matches!(error, rrc::ProtocolError::UnsupportedVersion(_)) {
                    activity::SourceValidation::Unsupported
                } else {
                    activity::SourceValidation::Malformed
                };
                let encoded_bytes = bounded_encoded_len(data.len());
                activity_recorder.record_spontaneous(move || {
                    activity::channels_envelope_received(activity::ChannelsEnvelopeActivity {
                        hub: context.hub,
                        room: None,
                        message: None,
                        envelope_kind: None,
                        encoded_bytes,
                        validation,
                        correlation_id: context.correlation_id,
                    })
                });
                tracing::debug!(
                    reason = "decode_failed",
                    "ignoring malformed channel envelope"
                );
                LinkEventOutcome::Keep
            }
        },
        LinkSessionEvent::PacketDelivered { .. }
        | LinkSessionEvent::RequestConcluded { .. }
        | LinkSessionEvent::ResourceStarted { .. }
        | LinkSessionEvent::ResourceProgress { .. }
        | LinkSessionEvent::ResourceConcluded { .. } => LinkEventOutcome::Keep,
        LinkSessionEvent::Stale => LinkEventOutcome::Stale,
        LinkSessionEvent::Recovered => LinkEventOutcome::Recovered,
        LinkSessionEvent::Closed { reason } => {
            tracing::info!(reason = close_reason_label(reason), "channel Link closed");
            LinkEventOutcome::Closed {
                product_reason: format!(
                    "Channel hub disconnected ({})",
                    close_reason_label(reason)
                ),
                activity_reason: channel_close_reason(reason),
            }
        }
    }
}

async fn handle_envelope(
    active: &mut ActiveSession,
    activity_recorder: &ChannelsActivity,
    mut envelope: Envelope,
    encoded_bytes: u32,
) {
    let message = active.message_token(envelope.message_id);
    let room = active.room_token(envelope.room.as_deref());
    let envelope_kind = channel_envelope_kind(envelope.message_type);
    let correlation_id = active.envelope_correlation(&envelope);
    let session = active.activity;
    if matches!(
        envelope.message_type,
        MessageType::Welcome
            | MessageType::Joined
            | MessageType::Parted
            | MessageType::Ping
            | MessageType::Error
            | MessageType::ResourceEnvelope
    ) && envelope.source != active.hub_identity
    {
        activity_recorder.record_spontaneous(move || {
            activity::channels_envelope_received(activity::ChannelsEnvelopeActivity {
                hub: session.hub,
                room,
                message: Some(message),
                envelope_kind,
                encoded_bytes,
                validation: activity::SourceValidation::NonHub,
                correlation_id,
            })
        });
        tracing::debug!(
            message_type = ?envelope.message_type,
            reason = "unauthenticated_control_source",
            "ignoring channel control envelope not authored by the authenticated hub"
        );
        return;
    }
    envelope.timestamp_ms = rrc::sanitize_display_timestamp_ms(envelope.timestamp_ms, now_ms());
    if !active.remember(envelope.message_id) {
        activity_recorder.record_spontaneous(move || {
            activity::channels_envelope_received(activity::ChannelsEnvelopeActivity {
                hub: session.hub,
                room,
                message: Some(message),
                envelope_kind,
                encoded_bytes,
                validation: activity::SourceValidation::Duplicate,
                correlation_id,
            })
        });
        return;
    }
    if matches!(envelope.message_type, MessageType::Unknown(_)) {
        activity_recorder.record_spontaneous(move || {
            activity::channels_envelope_received(activity::ChannelsEnvelopeActivity {
                hub: session.hub,
                room,
                message: Some(message),
                envelope_kind,
                encoded_bytes,
                validation: activity::SourceValidation::Unsupported,
                correlation_id,
            })
        });
        return;
    }
    if envelope.message_type == MessageType::ResourceEnvelope {
        let validation = accept_hub_greeting_resource_envelope(active, &envelope, encoded_bytes);
        activity_recorder.record_spontaneous(move || {
            activity::channels_envelope_received(activity::ChannelsEnvelopeActivity {
                hub: session.hub,
                room,
                message: Some(message),
                envelope_kind,
                encoded_bytes,
                validation,
                correlation_id,
            })
        });
        return;
    }
    let preserves_notice_burst =
        matches!(envelope.message_type, MessageType::Ping | MessageType::Pong)
            || (envelope.message_type == MessageType::Notice
                && envelope.room.is_none()
                && envelope.source == active.hub_identity);
    if !preserves_notice_burst {
        active.hub_greeting_notice_may_continue = false;
    }
    activity_recorder.record_spontaneous(move || {
        activity::channels_envelope_received(activity::ChannelsEnvelopeActivity {
            hub: session.hub,
            room,
            message: Some(message),
            envelope_kind,
            encoded_bytes,
            validation: activity::SourceValidation::Accepted,
            correlation_id,
        })
    });
    if envelope.message_type == MessageType::Ping {
        let pong = Envelope::pong(active.source, &envelope);
        // A single application-heartbeat send error is not authoritative
        // evidence that the Reticulum Link has ended. The Link actor owns
        // recovery and closure; it will emit Recovered, Closed, or end its
        // event stream. Closing here used to turn a recoverable stale window
        // into an immediate product-level "Channel link send failed".
        if let Err(error) = send_active_envelope_spontaneous(active, activity_recorder, &pong).await
        {
            tracing::warn!(
                reason = %error,
                "failed to answer an authenticated channel heartbeat"
            );
        }
    }

    match envelope.message_type {
        MessageType::Ping | MessageType::Pong | MessageType::Hello => {}
        MessageType::Welcome => {
            let welcome = rrc::parse_welcome(&envelope);
            active.supports_action = welcome
                .capabilities
                .get(&rrc::CAP_ACTION)
                .copied()
                .unwrap_or(active.supports_action);
            active.supports_resources = welcome
                .capabilities
                .get(&rrc::CAP_RESOURCE_ENVELOPE)
                .copied()
                .unwrap_or(active.supports_resources);
            if welcome.limits != HubLimits::default() {
                active.limits = welcome.limits;
            }
        }
        MessageType::Join => {}
        MessageType::Joined => {
            let confirmation = join_confirmation_context(active, &envelope);
            apply_joined(active, &envelope);
            record_join_confirmation(
                active,
                activity_recorder,
                confirmation,
                activity::ChannelJoinEvidence::JoinedRoster,
            );
        }
        MessageType::Part => {}
        MessageType::Parted => {
            let confirmation = part_confirmation_context(active, &envelope);
            apply_parted(active, &envelope);
            record_part_confirmation(active, activity_recorder, confirmation);
        }
        MessageType::Message | MessageType::Notice | MessageType::Action => {
            if envelope.message_type == MessageType::Notice {
                match apply_room_directory_notice(active, &envelope) {
                    RoomDirectoryNoticeHandling::AppliedConsumed => {
                        active.hub_greeting_notice_may_continue = false;
                    }
                    RoomDirectoryNoticeHandling::AppliedVisible => {
                        append_content(active, activity_recorder, &envelope, encoded_bytes);
                    }
                    RoomDirectoryNoticeHandling::NotDirectory => {
                        let confirmation = join_confirmation_context(active, &envelope);
                        if apply_rrcd_room_status_notice(active, &envelope) {
                            record_join_confirmation(
                                active,
                                activity_recorder,
                                confirmation,
                                activity::ChannelJoinEvidence::RrcdStatusNotice,
                            );
                        } else {
                            append_content(active, activity_recorder, &envelope, encoded_bytes)
                        }
                    }
                }
            } else {
                append_content(active, activity_recorder, &envelope, encoded_bytes)
            }
        }
        MessageType::Error => {
            if !apply_room_directory_error(active, &envelope) {
                append_error(active, activity_recorder, &envelope);
            }
        }
        MessageType::ResourceEnvelope | MessageType::Unknown(_) => {}
    }
}

#[derive(Clone)]
struct RoomConfirmationContext {
    room: String,
    token: activity::ChannelRoomToken,
    operation: Option<RoomOperationContext>,
    prior_phase: ChannelRoomPhase,
}

fn normalized_envelope_room(active: &ActiveSession, envelope: &Envelope) -> Option<String> {
    rrc::normalize_room(
        envelope.room.as_deref()?,
        active
            .limits
            .max_room_name_bytes
            .unwrap_or(DEFAULT_ROOM_MAX_BYTES),
    )
    .ok()
}

fn join_confirmation_context(
    active: &ActiveSession,
    envelope: &Envelope,
) -> Option<RoomConfirmationContext> {
    let room = normalized_envelope_room(active, envelope)?;
    let prior_phase = active.rooms.get(&room)?.phase;
    let context = active.room_activity.get(&room)?;
    Some(RoomConfirmationContext {
        room,
        token: context.token,
        operation: context.join,
        prior_phase,
    })
}

fn part_confirmation_context(
    active: &ActiveSession,
    envelope: &Envelope,
) -> Option<RoomConfirmationContext> {
    let room = normalized_envelope_room(active, envelope)?;
    let prior_phase = active.rooms.get(&room)?.phase;
    let context = active.room_activity.get(&room)?;
    Some(RoomConfirmationContext {
        room,
        token: context.token,
        operation: context.part,
        prior_phase,
    })
}

fn record_join_confirmation(
    active: &mut ActiveSession,
    recorder: &ChannelsActivity,
    confirmation: Option<RoomConfirmationContext>,
    evidence: activity::ChannelJoinEvidence,
) {
    let Some(confirmation) = confirmation else {
        return;
    };
    if confirmation.prior_phase == ChannelRoomPhase::Joined
        || !active
            .rooms
            .get(&confirmation.room)
            .is_some_and(|room| room.phase == ChannelRoomPhase::Joined)
    {
        return;
    }
    if let Some(context) = active.room_activity.get_mut(&confirmation.room) {
        context.join = None;
    }
    complete_pending_join_secret(active, &confirmation.room);
    if let Some(operation) = confirmation.operation {
        record_room_operation(
            recorder,
            active.activity,
            confirmation.token,
            operation,
            activity::ChannelRoomTransition::Joined { evidence },
        );
    } else {
        record_room_spontaneous(
            recorder,
            active.activity,
            confirmation.token,
            active.activity.correlation_id,
            activity::ChannelRoomTransition::Joined { evidence },
        );
    }
}

fn complete_pending_join_secret(active: &mut ActiveSession, room: &str) {
    let Some(pending) = active.pending_join_secrets.remove(room) else {
        return;
    };
    let action = match pending {
        PendingJoinSecret::UserRemember(key) => RoomSecretAction::Persist {
            room: room.to_string(),
            key,
        },
        PendingJoinSecret::UserEphemeral => RoomSecretAction::ForgetRequired {
            room: room.to_string(),
        },
        PendingJoinSecret::Stored => return,
    };
    active.room_secret_actions.push_back(action);
}

fn record_part_confirmation(
    active: &mut ActiveSession,
    recorder: &ChannelsActivity,
    confirmation: Option<RoomConfirmationContext>,
) {
    let Some(confirmation) = confirmation else {
        return;
    };
    if confirmation.prior_phase != ChannelRoomPhase::Parting {
        return;
    }
    active.room_activity.remove(&confirmation.room);
    if let Some(operation) = confirmation.operation {
        record_room_operation(
            recorder,
            active.activity,
            confirmation.token,
            operation,
            activity::ChannelRoomTransition::Parted,
        );
    }
}

fn apply_joined(active: &mut ActiveSession, envelope: &Envelope) {
    let Some(room_name) = envelope.room.as_deref() else {
        return;
    };
    let Ok(room_name) = rrc::normalize_room(
        room_name,
        active
            .limits
            .max_room_name_bytes
            .unwrap_or(DEFAULT_ROOM_MAX_BYTES),
    ) else {
        return;
    };
    let reconnecting_room = active.auto_rejoining.remove(&room_name)
        && active.connect_origin == ConnectOrigin::Recovery;
    let Some(room) = active.rooms.get_mut(&room_name) else {
        return;
    };
    if room.phase == ChannelRoomPhase::Parting {
        return;
    }
    let room_was_joined = room.phase == ChannelRoomPhase::Joined;
    let identities = rrc::member_identities(envelope);
    let identity_count = identities.len();
    let includes_self = identities.contains(&active.source);
    let single_identity_hash = (identity_count == 1).then(|| hex::encode(identities[0]));
    let single_lxmf_hash = (identity_count == 1).then(|| lxmf_destination_hash(identities[0]));
    let mut single_member_inserted = false;
    let mut nickname_member_inserted = false;
    let confirming_self = room.phase == ChannelRoomPhase::Joining
        || (room.phase == ChannelRoomPhase::Error && includes_self);
    // rrcd tracks room membership by Link, not by identity. When another Link
    // joins with our identity, the existing Link receives JOINED [self] as an
    // ordinary fanout. Treat that packet as an idempotent delta; it is not an
    // authoritative roster and must not erase members already visible here.
    let same_identity_delta = room_was_joined && identity_count == 1 && includes_self;
    let replaces_roster = (confirming_self || includes_self) && !same_identity_delta;
    if room.phase == ChannelRoomPhase::Error && !confirming_self {
        return;
    }
    room.phase = ChannelRoomPhase::Joined;
    room.phase_started_at_ms = now_ms();
    room.last_error = None;

    if !identities.is_empty() {
        if replaces_roster {
            room.members.clear();
        }
        let single_member_nickname = (identities.len() == 1)
            .then(|| envelope.nickname.clone())
            .flatten();
        for identity in identities {
            let member_nickname = if identity == active.source {
                Some(active.nickname.clone())
            } else {
                single_member_nickname.clone()
            };
            let inserted = upsert_member(
                &mut room.members,
                Some(identity),
                member_nickname.clone(),
                identity == active.source,
            );
            queue_participant_observation(
                &mut active.participant_observations,
                active.destination_hash,
                active.source,
                active.hub_identity,
                &room_name,
                identity,
                member_nickname,
            );
            if identity_count == 1 {
                single_member_inserted = inserted;
            }
        }
        if replaces_roster {
            room.members_complete = true;
        } else if identity_count > 1 {
            // A continuation chunk proves only that these members are here;
            // until a full roster includes us, keep the completeness claim
            // conservative. A one-member fanout is a complete delta and does
            // not invalidate a roster that was already complete.
            room.members_complete = false;
        }
    } else if confirming_self {
        upsert_member(
            &mut room.members,
            Some(active.source),
            Some(active.nickname.clone()),
            true,
        );
        room.members_complete = false;
    } else if let Some(nickname) = envelope.nickname.clone() {
        nickname_member_inserted = upsert_member(&mut room.members, None, Some(nickname), false);
    }

    let nickname = if confirming_self {
        Some(active.nickname.clone())
    } else {
        envelope.nickname.clone()
    };
    let join_already_visible = confirming_self && self_join_transition_visible(room);
    // A multi-identity JOINED that does not include us is a roster fragment,
    // never a join event: hubs split large rosters across packets, and
    // treating a continuation as an arrival invents "A member joined" lines.
    let nickname_only_join = room_was_joined
        && identity_count == 0
        && nickname_member_inserted
        && envelope
            .nickname
            .as_ref()
            .is_some_and(|nick| !nick.is_empty());
    let is_join_event = confirming_self
        || (identity_count == 1 && !includes_self && single_member_inserted)
        || nickname_only_join;
    if !join_already_visible && is_join_event && !(reconnecting_room && confirming_self) {
        let mut item = transcript_item(
            envelope,
            ChannelItemKind::Join,
            nickname.clone(),
            if confirming_self {
                "You joined".into()
            } else {
                format!("{} joined", nickname.unwrap_or_else(|| "A member".into()))
            },
            confirming_self,
        );
        item.source_hash = if confirming_self {
            Some(hex::encode(active.source))
        } else if identity_count == 1 {
            single_identity_hash
        } else {
            None
        };
        item.source_lxmf_hash = if confirming_self {
            Some(lxmf_destination_hash(active.source))
        } else if identity_count == 1 {
            single_lxmf_hash
        } else {
            None
        };
        append_room_item(
            &mut active.history_events,
            active.destination_hash,
            room,
            item,
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedRoomDirectoryNotice {
    NotDirectory,
    Malformed,
    Directory {
        rooms: Vec<ChannelDirectoryRoomSnapshot>,
        omitted_count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomDirectoryNoticeHandling {
    NotDirectory,
    AppliedVisible,
    AppliedConsumed,
}

fn parse_directory_omitted_marker(line: &str) -> Option<usize> {
    line.strip_prefix("(+")?
        .strip_suffix(" more)")?
        .parse()
        .ok()
        .filter(|count| *count > 0)
}

/// Interpret the byte-compatible `/list` response already used by rrcd and
/// NomadNet. Exact framing keeps ordinary roomless greetings out of the
/// directory, while source and room checks prevent peers from forging it.
fn parse_room_directory_notice(
    envelope: &Envelope,
    hub_identity: [u8; 16],
    max_room_bytes: usize,
) -> ParsedRoomDirectoryNotice {
    if envelope.source != hub_identity || envelope.room.is_some() {
        return ParsedRoomDirectoryNotice::NotDirectory;
    }
    let Some(text) = rrc::text_body(envelope) else {
        return ParsedRoomDirectoryNotice::NotDirectory;
    };
    let text = text.trim();
    if text == "No public rooms registered" {
        return ParsedRoomDirectoryNotice::Directory {
            rooms: Vec::new(),
            omitted_count: 0,
        };
    }

    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("Registered public rooms:") {
        return ParsedRoomDirectoryNotice::NotDirectory;
    }
    if text.len() > DIRECTORY_MAX_RESPONSE_BYTES {
        return ParsedRoomDirectoryNotice::Malformed;
    }

    let lines: Vec<&str> = lines
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut rooms = BTreeMap::<String, Option<String>>::new();
    let mut omitted_count = 0usize;
    for (index, line) in lines.iter().copied().enumerate() {
        if let Some(omitted) = parse_directory_omitted_marker(line) {
            if index + 1 != lines.len() {
                return ParsedRoomDirectoryNotice::Malformed;
            }
            omitted_count = omitted;
            continue;
        }
        if line.starts_with("(+") && line.ends_with(" more)") {
            return ParsedRoomDirectoryNotice::Malformed;
        }
        if line.chars().any(char::is_control) {
            return ParsedRoomDirectoryNotice::Malformed;
        }
        let (name, topic) = match line.split_once(" - ") {
            Some((name, topic)) => (name, Some(topic.trim())),
            None => (line, None),
        };
        let Ok(name) = rrc::normalize_room(name, max_room_bytes) else {
            return ParsedRoomDirectoryNotice::Malformed;
        };
        if name.chars().any(char::is_control) {
            return ParsedRoomDirectoryNotice::Malformed;
        }
        let topic = topic.filter(|topic| !topic.is_empty()).map(str::to_string);
        if topic.as_ref().is_some_and(|topic| {
            topic.len() > DIRECTORY_MAX_TOPIC_BYTES || topic.chars().any(char::is_control)
        }) {
            return ParsedRoomDirectoryNotice::Malformed;
        }
        rooms.entry(name).or_insert(topic);
        if rooms.len() > DIRECTORY_MAX_ROOMS {
            return ParsedRoomDirectoryNotice::Malformed;
        }
    }

    ParsedRoomDirectoryNotice::Directory {
        rooms: rooms
            .into_iter()
            .map(|(name, topic)| ChannelDirectoryRoomSnapshot { name, topic })
            .collect(),
        omitted_count,
    }
}

fn apply_room_directory_notice(
    active: &mut ActiveSession,
    envelope: &Envelope,
) -> RoomDirectoryNoticeHandling {
    let pending = active.directory_request_deadline.is_some();
    let parsed = parse_room_directory_notice(
        envelope,
        active.hub_identity,
        active
            .limits
            .max_room_name_bytes
            .unwrap_or(DEFAULT_ROOM_MAX_BYTES),
    );
    match parsed {
        ParsedRoomDirectoryNotice::NotDirectory => RoomDirectoryNoticeHandling::NotDirectory,
        ParsedRoomDirectoryNotice::Malformed if !pending => {
            RoomDirectoryNoticeHandling::AppliedVisible
        }
        ParsedRoomDirectoryNotice::Malformed => {
            active.directory_request_deadline = None;
            active.directory.phase = ChannelRoomDirectoryPhase::Error;
            active.directory.last_error =
                Some("The hub returned an invalid public channel list".into());
            RoomDirectoryNoticeHandling::AppliedConsumed
        }
        ParsedRoomDirectoryNotice::Directory {
            rooms,
            omitted_count,
        } => {
            active.directory = ChannelRoomDirectorySnapshot {
                phase: ChannelRoomDirectoryPhase::Ready,
                rooms,
                complete: omitted_count == 0,
                omitted_count,
                refreshed_at_ms: Some(now_ms()),
                last_error: None,
            };
            active.directory_request_deadline = None;
            if pending {
                RoomDirectoryNoticeHandling::AppliedConsumed
            } else {
                RoomDirectoryNoticeHandling::AppliedVisible
            }
        }
    }
}

fn apply_room_directory_error(active: &mut ActiveSession, envelope: &Envelope) -> bool {
    if active.directory_request_deadline.is_none()
        || envelope.source != active.hub_identity
        || envelope.room.is_some()
    {
        return false;
    }
    active.directory_request_deadline = None;
    active.directory.phase = ChannelRoomDirectoryPhase::Error;
    active.directory.last_error = Some("The hub rejected the public channel request".into());
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RrcdRoomStatus {
    registered: bool,
    modes: Option<String>,
    topic: Option<String>,
}

fn parse_rrcd_room_status(room_name: &str, text: &str) -> Option<RrcdRoomStatus> {
    let status = text.strip_prefix(&format!("room {room_name}: "))?;
    let (registration, details) = status.split_once("; mode=")?;
    let registered = match registration.trim() {
        "registered" => true,
        "unregistered" => false,
        _ => return None,
    };
    let (modes, topic) = details.split_once("; topic=")?;
    let modes = modes.trim();
    let topic = topic.trim();
    Some(RrcdRoomStatus {
        registered,
        modes: (!modes.is_empty()).then(|| modes.to_string()),
        topic: (!topic.is_empty() && topic != "(none)").then(|| topic.to_string()),
    })
}

/// `rrcd` sends this room-scoped status NOTICE after it has accepted JOIN,
/// added the client to the room, and queued JOINED. A JOINED roster can exceed
/// a constrained Link MDU on populated rooms, while this short NOTICE still
/// arrives. Consume the authenticated metadata instead of exposing raw mode
/// flags in chat, and use it as a fallback confirmation when JOINED is absent.
fn apply_rrcd_room_status_notice(active: &mut ActiveSession, envelope: &Envelope) -> bool {
    if envelope.source != active.hub_identity {
        return false;
    }
    let Some(room_name) = envelope.room.as_deref() else {
        return false;
    };
    let Ok(room_name) = rrc::normalize_room(
        room_name,
        active
            .limits
            .max_room_name_bytes
            .unwrap_or(DEFAULT_ROOM_MAX_BYTES),
    ) else {
        return false;
    };
    let Some(text) = rrc::text_body(envelope) else {
        return false;
    };
    let Some(status) = parse_rrcd_room_status(&room_name, text) else {
        return false;
    };
    let reconnecting_room = active.auto_rejoining.remove(&room_name)
        && active.connect_origin == ConnectOrigin::Recovery;
    let Some(room) = active.rooms.get_mut(&room_name) else {
        return false;
    };
    room.registered = Some(status.registered);
    room.modes = status.modes;
    room.topic = status.topic;

    if matches!(
        room.phase,
        ChannelRoomPhase::Joining | ChannelRoomPhase::Error
    ) {
        room.phase = ChannelRoomPhase::Joined;
        room.phase_started_at_ms = now_ms();
        room.last_error = None;
        room.members_complete = false;
        upsert_member(
            &mut room.members,
            Some(active.source),
            Some(active.nickname.clone()),
            true,
        );
        if !reconnecting_room && !self_join_transition_visible(room) {
            let item = ChannelTranscriptItem {
                id: format!("{}-joined", hex::encode(envelope.message_id)),
                kind: ChannelItemKind::Join,
                timestamp_ms: envelope.timestamp_ms,
                source_hash: Some(hex::encode(active.source)),
                source_lxmf_hash: Some(lxmf_destination_hash(active.source)),
                nickname: Some(active.nickname.clone()),
                text: "You joined".into(),
                ours: true,
                mentioned: false,
            };
            append_room_item(
                &mut active.history_events,
                active.destination_hash,
                room,
                item,
            );
        }
    }
    true
}

fn self_join_transition_visible(room: &ChannelRoomSnapshot) -> bool {
    room.transcript
        .iter()
        .any(|item| item.ours && item.kind == ChannelItemKind::Join)
}

fn apply_parted(active: &mut ActiveSession, envelope: &Envelope) {
    let Some(room_name) = envelope.room.as_deref() else {
        return;
    };
    let Ok(room_name) = rrc::normalize_room(
        room_name,
        active
            .limits
            .max_room_name_bytes
            .unwrap_or(DEFAULT_ROOM_MAX_BYTES),
    ) else {
        return;
    };
    let own_part = active
        .rooms
        .get(&room_name)
        .is_some_and(|room| room.phase == ChannelRoomPhase::Parting);
    if own_part {
        active.rooms.remove(&room_name);
        return;
    }
    let Some(room) = active.rooms.get_mut(&room_name) else {
        return;
    };
    let identities = rrc::member_identities(envelope);
    let observed_nickname = (identities.len() == 1)
        .then(|| envelope.nickname.clone())
        .flatten();
    for identity in identities.iter().copied() {
        queue_participant_observation(
            &mut active.participant_observations,
            active.destination_hash,
            active.source,
            active.hub_identity,
            &room_name,
            identity,
            observed_nickname.clone(),
        );
    }
    if !identities.is_empty() {
        room.members.retain(|member| {
            member
                .identity_hash
                .as_deref()
                .and_then(|hash| hex::decode(hash).ok())
                .is_none_or(|identity| !identities.iter().any(|left| left.as_slice() == identity))
        });
    } else if let Some(nickname) = envelope.nickname.as_deref()
        && let Some(index) = room
            .members
            .iter()
            .position(|member| member.nickname.as_deref() == Some(nickname) && !member.is_self)
    {
        room.members.remove(index);
    }
    let nickname = envelope.nickname.clone();
    let mut item = transcript_item(
        envelope,
        ChannelItemKind::Part,
        nickname.clone(),
        format!("{} left", nickname.unwrap_or_else(|| "A member".into())),
        false,
    );
    item.source_hash = (identities.len() == 1).then(|| hex::encode(identities[0]));
    item.source_lxmf_hash = (identities.len() == 1).then(|| lxmf_destination_hash(identities[0]));
    append_room_item(
        &mut active.history_events,
        active.destination_hash,
        room,
        item,
    );
}

fn mention_word_continuation(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-')
}

fn contains_exact_mention(text: &str, target: &str) -> bool {
    if target.is_empty() {
        return false;
    }
    let text = text.to_lowercase();
    let needle = format!("@{}", target.to_lowercase());
    let mut offset = 0usize;
    while let Some(found) = text[offset..].find(&needle) {
        let start = offset.saturating_add(found);
        let end = start.saturating_add(needle.len());
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if before.is_none_or(|ch| !mention_word_continuation(ch))
            && after.is_none_or(|ch| !mention_word_continuation(ch))
        {
            return true;
        }
        // The marker is ASCII, so advancing one byte stays on a UTF-8
        // boundary and still permits overlapping candidate searches.
        offset = start.saturating_add(1);
    }
    false
}

fn channel_text_mentions(text: &str, nickname: &str, identity_hash: [u8; 16]) -> bool {
    contains_exact_mention(text, nickname)
        || contains_exact_mention(text, &hex::encode(identity_hash))
}

fn append_content(
    active: &mut ActiveSession,
    activity_recorder: &ChannelsActivity,
    envelope: &Envelope,
    encoded_bytes: u32,
) {
    let Some(text) = rrc::text_body(envelope) else {
        return;
    };
    let kind = match envelope.message_type {
        MessageType::Notice => ChannelItemKind::Notice,
        MessageType::Action => ChannelItemKind::Action,
        _ => ChannelItemKind::Message,
    };
    let mentioned = envelope.source != active.source
        && matches!(
            envelope.message_type,
            MessageType::Message | MessageType::Action
        )
        && channel_text_mentions(text, &active.nickname, active.source);
    let mut item = transcript_item(
        envelope,
        kind,
        envelope.nickname.clone(),
        text.to_string(),
        envelope.source == active.source,
    );
    item.mentioned = mentioned;
    if let Some(room_name) = envelope.room.as_deref()
        && let Some(room) = active.rooms.get_mut(room_name)
    {
        // RRC content received over the authenticated hub Link is a live,
        // hub-attested observation of its source, not an independently signed
        // peer claim. Once membership is confirmed, a room message/action is
        // still useful evidence for the best-effort visible member list.
        if room.phase == ChannelRoomPhase::Joined
            && matches!(
                envelope.message_type,
                MessageType::Message | MessageType::Action
            )
            && envelope.source != active.hub_identity
        {
            let max_nick = active
                .limits
                .max_nick_bytes
                .unwrap_or(DEFAULT_NICK_MAX_BYTES);
            let nickname = envelope
                .nickname
                .as_deref()
                .and_then(|value| rrc::normalize_nickname(value, max_nick).ok());
            let inserted = upsert_member(
                &mut room.members,
                Some(envelope.source),
                nickname.clone(),
                envelope.source == active.source,
            );
            queue_participant_observation(
                &mut active.participant_observations,
                active.destination_hash,
                active.source,
                active.hub_identity,
                room_name,
                envelope.source,
                nickname,
            );
            if inserted {
                room.members_complete = false;
            }
        }
        append_room_item(
            &mut active.history_events,
            active.destination_hash,
            room,
            item,
        );
    } else if envelope.message_type == MessageType::Notice {
        let within_greeting_window = Instant::now() <= active.hub_greeting_deadline;
        let authenticated_roomless_notice =
            envelope.source == active.hub_identity && within_greeting_window;
        let continuing_unframed_greeting = active.hub_greeting_notice_may_continue
            && active
                .hub_greeting
                .as_ref()
                .is_some_and(|greeting| greeting.delivery == ChannelHubGreetingDelivery::Notice);
        if authenticated_roomless_notice
            && active.greeting_resource_in_flight.is_none()
            && (active.hub_greeting.is_none() || continuing_unframed_greeting)
        {
            // A NOTICE fallback has no final-fragment marker. Coalesce only
            // when the previous packet was close enough to the Link MDU to
            // plausibly be a reference-hub chunk; otherwise the next NOTICE
            // remains an ordinary hub notice.
            let first_fragment = active.hub_greeting.is_none();
            if first_fragment {
                active.greeting_resource_expectation = None;
                active.hub_greeting = Some(ChannelHubGreetingSnapshot {
                    text: text.to_string(),
                    received_at_ms: now_ms(),
                    source_hash: hex::encode(active.hub_identity),
                    delivery: ChannelHubGreetingDelivery::Notice,
                    completeness: ChannelHubGreetingCompleteness::Unframed,
                });
                record_session_spontaneous(
                    activity_recorder,
                    active.activity,
                    activity::ChannelSessionTransition::GreetingObserved { encoded_bytes },
                );
            } else if let Some(greeting) = active.hub_greeting.as_mut() {
                greeting.text.push_str(text);
            }
            active.hub_greeting_notice_may_continue = (encoded_bytes as usize)
                .saturating_add(HUB_GREETING_NOTICE_MDU_SLACK)
                >= active.handle.mdu();
        } else {
            active.hub_greeting_notice_may_continue = false;
            append_bounded(&mut active.notices, item, NOTICE_LIMIT);
        }
    }
}

fn append_error(
    active: &mut ActiveSession,
    activity_recorder: &ChannelsActivity,
    envelope: &Envelope,
) {
    let hub_text = rrc::text_body(envelope).unwrap_or("Channel hub reported an error");
    let explicit_room = envelope.room.as_deref().and_then(|room| {
        rrc::normalize_room(
            room,
            active
                .limits
                .max_room_name_bytes
                .unwrap_or(DEFAULT_ROOM_MAX_BYTES),
        )
        .ok()
    });
    let inferred_room = if explicit_room.is_none() {
        let mut joining = active
            .rooms
            .values()
            .filter(|room| room.phase == ChannelRoomPhase::Joining);
        let first = joining.next().map(|room| room.name.clone());
        if joining.next().is_none() {
            first
        } else {
            None
        }
    } else {
        None
    };
    if let Some(room_name) = explicit_room.or(inferred_room) {
        active.auto_rejoining.remove(&room_name);
        let Some(prior_phase) = active.rooms.get(&room_name).map(|room| room.phase) else {
            let item = transcript_item(
                envelope,
                ChannelItemKind::Error,
                None,
                hub_text.to_string(),
                false,
            );
            append_bounded(&mut active.notices, item, NOTICE_LIMIT);
            return;
        };
        let pending = if prior_phase == ChannelRoomPhase::Joining {
            active.pending_join_secrets.remove(&room_name)
        } else {
            None
        };
        let bad_room_key =
            prior_phase == ChannelRoomPhase::Joining && hub_text == BAD_ROOM_KEY_ERROR;
        let saved_key_rejected =
            bad_room_key && matches!(pending.as_ref(), Some(PendingJoinSecret::Stored));
        if bad_room_key {
            let action = if saved_key_rejected {
                RoomSecretAction::ForgetRejected {
                    room: room_name.clone(),
                }
            } else {
                RoomSecretAction::MarkRequired {
                    room: room_name.clone(),
                }
            };
            active.room_secret_actions.push_back(action);
        }
        let product_text = if saved_key_rejected {
            SAVED_ROOM_KEY_REJECTED
        } else if bad_room_key && pending.is_some() {
            ENTERED_ROOM_KEY_REJECTED
        } else if bad_room_key {
            ROOM_KEY_REQUIRED
        } else {
            hub_text
        };
        let item = transcript_item(
            envelope,
            ChannelItemKind::Error,
            None,
            product_text.to_string(),
            false,
        );
        let room = active
            .rooms
            .get_mut(&room_name)
            .expect("room phase was read above");
        if room.phase == ChannelRoomPhase::Joining {
            room.phase = ChannelRoomPhase::Error;
            room.phase_started_at_ms = now_ms();
            room.last_error = Some(product_text.to_string());
        } else if room.phase == ChannelRoomPhase::Parting {
            room.phase = ChannelRoomPhase::Joined;
            room.phase_started_at_ms = now_ms();
            room.last_error = Some(product_text.to_string());
        }
        append_room_item(
            &mut active.history_events,
            active.destination_hash,
            room,
            item,
        );
        let operation = active
            .room_activity
            .get_mut(&room_name)
            .and_then(|context| {
                let operation = match prior_phase {
                    ChannelRoomPhase::Joining => context.join.take(),
                    ChannelRoomPhase::Parting => context.part.take(),
                    ChannelRoomPhase::Joined | ChannelRoomPhase::Error => None,
                };
                operation.map(|operation| (context.token, operation))
            });
        if let Some((room_token, operation)) = operation {
            let transition = match prior_phase {
                ChannelRoomPhase::Joining => activity::ChannelRoomTransition::JoinRejected {
                    reason: activity::ChannelRoomFailureReason::HubRejected,
                },
                ChannelRoomPhase::Parting => activity::ChannelRoomTransition::PartRejected {
                    reason: activity::ChannelRoomFailureReason::HubRejected,
                },
                ChannelRoomPhase::Joined | ChannelRoomPhase::Error => return,
            };
            record_room_operation(
                activity_recorder,
                active.activity,
                room_token,
                operation,
                transition,
            );
        }
    } else {
        let item = transcript_item(
            envelope,
            ChannelItemKind::Error,
            None,
            hub_text.to_string(),
            false,
        );
        append_bounded(&mut active.notices, item, NOTICE_LIMIT);
    }
}

fn expire_room_transitions(
    rooms: &mut BTreeMap<String, ChannelRoomSnapshot>,
    room_activity: &mut BTreeMap<String, RoomActivityContext>,
    session_activity: SessionActivityContext,
    activity_recorder: &ChannelsActivity,
    timestamp_ms: u64,
) -> bool {
    let join_timeout_ms = JOIN_CONFIRM_TIMEOUT.as_millis() as u64;
    let part_timeout_ms = PART_CONFIRM_TIMEOUT.as_millis() as u64;
    let mut changed = false;
    let mut joined_out = Vec::new();
    let mut parted_out = Vec::new();

    for (name, room) in rooms.iter_mut() {
        let elapsed = timestamp_ms.saturating_sub(room.phase_started_at_ms);
        match room.phase {
            ChannelRoomPhase::Joining if elapsed >= join_timeout_ms => {
                room.phase = ChannelRoomPhase::Error;
                room.phase_started_at_ms = timestamp_ms;
                room.last_error = Some(
                    "No confirmation arrived from the hub. Try joining again or leave this channel."
                        .into(),
                );
                joined_out.push(name.clone());
                changed = true;
            }
            ChannelRoomPhase::Parting if elapsed >= part_timeout_ms => {
                parted_out.push(name.clone());
            }
            ChannelRoomPhase::Joining
            | ChannelRoomPhase::Joined
            | ChannelRoomPhase::Parting
            | ChannelRoomPhase::Error => {}
        }
    }
    for name in joined_out {
        if let Some((token, operation)) = room_activity.get_mut(&name).and_then(|context| {
            context
                .join
                .take()
                .map(|operation| (context.token, operation))
        }) {
            record_room_operation(
                activity_recorder,
                session_activity,
                token,
                operation,
                activity::ChannelRoomTransition::JoinTimedOut,
            );
        }
    }
    for name in parted_out {
        rooms.remove(&name);
        if let Some(context) = room_activity.remove(&name)
            && let Some(operation) = context.part
        {
            record_room_operation(
                activity_recorder,
                session_activity,
                context.token,
                operation,
                activity::ChannelRoomTransition::PartTimedOut,
            );
        }
        changed = true;
    }
    changed
}

fn transcript_item(
    envelope: &Envelope,
    kind: ChannelItemKind,
    nickname: Option<String>,
    text: String,
    ours: bool,
) -> ChannelTranscriptItem {
    ChannelTranscriptItem {
        id: hex::encode(envelope.message_id),
        kind,
        timestamp_ms: envelope.timestamp_ms,
        source_hash: Some(hex::encode(envelope.source)),
        source_lxmf_hash: Some(lxmf_destination_hash(envelope.source)),
        nickname,
        text,
        ours,
        mentioned: false,
    }
}

fn append_room_item(
    history_events: &mut VecDeque<db::NewChannelHistoryEvent>,
    destination_hash: [u8; 16],
    room: &mut ChannelRoomSnapshot,
    item: ChannelTranscriptItem,
) {
    let kind = match item.kind {
        ChannelItemKind::Message => db::ChannelHistoryKind::Message,
        ChannelItemKind::Notice => db::ChannelHistoryKind::Notice,
        ChannelItemKind::Action => db::ChannelHistoryKind::Action,
        ChannelItemKind::Join => db::ChannelHistoryKind::Join,
        ChannelItemKind::Part => db::ChannelHistoryKind::Part,
        ChannelItemKind::Error => db::ChannelHistoryKind::Error,
        ChannelItemKind::System => db::ChannelHistoryKind::System,
    };
    history_events.push_back(db::NewChannelHistoryEvent {
        hub_destination_hash: hex::encode(destination_hash),
        room_name: room.name.clone(),
        event_id: item.id.clone(),
        kind,
        timestamp_ms: item.timestamp_ms,
        source_hash: item.source_hash.clone(),
        nickname: item.nickname.clone(),
        text: item.text.clone(),
        ours: item.ours,
        mentioned: item.mentioned,
    });
    room.transcript.push(item);
    if room.transcript.len() > TRANSCRIPT_LIMIT {
        room.transcript
            .drain(..room.transcript.len().saturating_sub(TRANSCRIPT_LIMIT));
    }
}

fn append_bounded<T>(items: &mut VecDeque<T>, item: T, limit: usize) {
    items.push_back(item);
    while items.len() > limit {
        items.pop_front();
    }
}

fn queue_participant_observation(
    observations: &mut VecDeque<db::NewChannelParticipantObservation>,
    destination_hash: [u8; 16],
    local_identity: [u8; 16],
    hub_identity: [u8; 16],
    room_name: &str,
    participant_identity: [u8; 16],
    nickname: Option<String>,
) {
    if participant_identity == local_identity || participant_identity == hub_identity {
        return;
    }
    let hub_destination_hash = hex::encode(destination_hash);
    let identity_hash = hex::encode(participant_identity);
    let nickname = nickname.and_then(|nickname| {
        let nickname = nickname.trim();
        (!nickname.is_empty()).then(|| nickname.to_string())
    });
    if let Some(existing) = observations.iter_mut().find(|observation| {
        observation.hub_destination_hash == hub_destination_hash
            && observation.room_name == room_name
            && observation.identity_hash == identity_hash
    }) {
        if nickname.is_some() {
            existing.nickname = nickname;
        }
        return;
    }
    append_bounded(
        observations,
        db::NewChannelParticipantObservation {
            hub_destination_hash,
            room_name: room_name.to_string(),
            identity_hash,
            nickname,
        },
        PARTICIPANT_OBSERVATION_QUEUE_LIMIT,
    );
}

fn upsert_member(
    members: &mut Vec<ChannelMemberSnapshot>,
    identity: Option<[u8; 16]>,
    nickname: Option<String>,
    is_self: bool,
) -> bool {
    let identity_hash = identity.map(hex::encode);
    let lxmf_hash = identity.map(lxmf_destination_hash);
    let existing_index = identity_hash
        .as_deref()
        .and_then(|hash| {
            members
                .iter()
                .position(|member| member.identity_hash.as_deref() == Some(hash))
        })
        .or_else(|| {
            nickname.as_deref().and_then(|nick| {
                members.iter().position(|member| {
                    member.identity_hash.is_none() && member.nickname.as_deref() == Some(nick)
                })
            })
        });
    if let Some(existing) = existing_index.and_then(|index| members.get_mut(index)) {
        if existing.identity_hash.is_none() {
            existing.identity_hash = identity_hash;
        }
        if existing.lxmf_hash.is_none() {
            existing.lxmf_hash = lxmf_hash;
        }
        if nickname.is_some() {
            existing.nickname = nickname;
        }
        existing.is_self |= is_self;
        false
    } else {
        members.push(ChannelMemberSnapshot {
            identity_hash,
            lxmf_hash,
            nickname,
            is_self,
        });
        true
    }
}

async fn send_active_envelope(
    active: &mut ActiveSession,
    activity_recorder: &ChannelsActivity,
    envelope: &Envelope,
    activity_fence: Option<ActivityRequestFence>,
) -> Result<rns_runtime::link_session::LinkSessionPacketReceipt, ChannelsError> {
    send_active_envelope_inner(active, activity_recorder, envelope, Some(activity_fence)).await
}

async fn send_active_envelope_spontaneous(
    active: &mut ActiveSession,
    activity_recorder: &ChannelsActivity,
    envelope: &Envelope,
) -> Result<rns_runtime::link_session::LinkSessionPacketReceipt, ChannelsError> {
    send_active_envelope_inner(active, activity_recorder, envelope, None).await
}

async fn send_active_envelope_inner(
    active: &mut ActiveSession,
    activity_recorder: &ChannelsActivity,
    envelope: &Envelope,
    command_fence: Option<Option<ActivityRequestFence>>,
) -> Result<rns_runtime::link_session::LinkSessionPacketReceipt, ChannelsError> {
    let encoded = rrc::encode(envelope)?;
    let encoded_bytes = bounded_encoded_len(encoded.len());
    let receipt = active.handle.send_packet(encoded).await?;
    let message = active.message_token(envelope.message_id);
    let room = active.room_token(envelope.room.as_deref());
    let envelope_kind = channel_envelope_kind(envelope.message_type);
    let correlation_id = active.envelope_correlation(envelope);
    let session = active.activity;
    let make = move || {
        activity::channels_envelope_sent(activity::ChannelsEnvelopeActivity {
            hub: session.hub,
            room,
            message: Some(message),
            envelope_kind,
            encoded_bytes,
            validation: activity::SourceValidation::Accepted,
            correlation_id,
        })
    };
    if let Some(fence) = command_fence {
        activity_recorder.record_fenced(fence, make);
    } else {
        activity_recorder.record_spontaneous(make);
    }
    Ok(receipt)
}

async fn discover_hubs(
    transport_tx: &mpsc::Sender<TransportMessage>,
) -> Result<Vec<DiscoveredChannelHub>, ChannelsError> {
    let entries = query_announces(transport_tx).await?;
    let aspect_hash = rns_identity::name_hash::name_hash(rrc::RRC_HUB_ASPECT);
    Ok(entries
        .into_iter()
        .filter(|entry| entry.name_hash == aspect_hash)
        .map(discovered_hub_from_announce)
        .collect())
}

async fn query_announces(
    transport_tx: &mpsc::Sender<TransportMessage>,
) -> Result<Vec<AnnounceRpcEntry>, ChannelsError> {
    let (response_tx, response_rx) = oneshot::channel();
    transport_tx
        .send(TransportMessage::Rpc {
            query: TransportQuery::GetRecentAnnounces,
            response_tx,
        })
        .await
        .map_err(|_| ChannelsError::Unavailable)?;
    match response_rx.await.map_err(|_| ChannelsError::Unavailable)? {
        TransportQueryResponse::Announces(entries) => Ok(entries),
        _ => Err(ChannelsError::Transport(
            "unexpected announce query response".into(),
        )),
    }
}

fn discovered_hub_from_announce(entry: AnnounceRpcEntry) -> DiscoveredChannelHub {
    let identity_hash = entry.public_key.and_then(|public_key| {
        Identity::from_public_key(&public_key)
            .ok()
            .map(|identity| hex::encode(identity.hash))
    });
    DiscoveredChannelHub {
        destination_hash: hex::encode(entry.dest_hash),
        identity_hash,
        announced_name: parse_announce_hub_name(entry.app_data.as_deref()),
        hops: entry.hops,
        last_seen: entry.timestamp,
        is_path_response: entry.is_path_response,
    }
}

fn parse_announce_hub_name(app_data: Option<&[u8]>) -> Option<String> {
    let data = app_data?;
    let value: Value = ciborium::de::from_reader(Cursor::new(data)).ok()?;
    let Value::Map(fields) = value else {
        return None;
    };
    fields
        .into_iter()
        .find_map(|(key, value)| match (key, value) {
            (Value::Text(key), Value::Text(name)) if key == "hub" && !name.trim().is_empty() => {
                Some(name)
            }
            _ => None,
        })
}

fn parse_destination_hash(value: &str) -> Result<[u8; 16], ChannelsError> {
    let bytes = hex::decode(value.trim()).map_err(|_| ChannelsError::InvalidDestination)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| ChannelsError::InvalidDestination)
}

fn sync_session_snapshot(active: &ActiveSession, snapshot: &Arc<RwLock<ChannelsSnapshot>>) {
    mutate_snapshot(snapshot, |state| {
        state.nickname = Some(active.nickname.clone());
        state.rooms = active.rooms.values().cloned().collect();
        state.directory = active.directory.clone();
        state.hub_greeting = active.hub_greeting.clone();
        state.notices = active.notices.iter().cloned().collect();
    });
}

fn clear_observed_snapshot(state: &mut ChannelsSnapshot) {
    state.phase = ChannelsPhase::Offline;
    state.nickname = None;
    state.hub = None;
    state.rooms.clear();
    state.directory = ChannelRoomDirectorySnapshot::default();
    state.hub_greeting = None;
    state.notices.clear();
    state.last_error = None;
}

async fn wait_for_reconnect(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => pending::<()>().await,
    }
}

fn selected_hub_is_desired(snapshot: &Arc<RwLock<ChannelsSnapshot>>) -> bool {
    snapshot.read().ok().is_some_and(|state| {
        let Some(selected) = state.selected_hub_destination.as_deref() else {
            return false;
        };
        state
            .hubs
            .iter()
            .any(|hub| hub.destination_hash == selected && hub.desired.connected)
    })
}

fn desired_connection_target(
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
) -> Result<Option<([u8; 16], String)>, String> {
    let state = snapshot
        .read()
        .map_err(|_| "Channels state is unavailable".to_string())?;
    let Some(destination) = state.selected_hub_destination.as_deref() else {
        return Ok(None);
    };
    let destination_hash =
        parse_destination_hash(destination).map_err(|error| error.to_string())?;
    let nickname = state
        .hubs
        .iter()
        .find(|hub| hub.destination_hash == destination)
        .and_then(|hub| hub.desired.nickname.as_deref())
        .ok_or_else(|| "Saved channel nickname is unavailable".to_string())?;
    let nickname = rrc::normalize_nickname(nickname, DEFAULT_NICK_MAX_BYTES)
        .map_err(|_| "Saved channel nickname is invalid; reconnect manually".to_string())?;
    Ok(Some((destination_hash, nickname)))
}

fn known_owned_hub_target(
    state: &Weak<AppState>,
    destination_hash: [u8; 16],
) -> Option<KnownHubTarget> {
    let state = state.upgrade()?;
    let hub = state.channel_hub_handle()?;
    let status = hub.snapshot();
    let expected = hex::encode(destination_hash);
    if !status.running || status.destination_hash.as_deref() != Some(expected.as_str()) {
        return None;
    }

    let public_key = hub.public_key();
    let identity_hash = Identity::from_public_key(&public_key).ok()?.hash;
    Some(KnownHubTarget {
        public_key,
        identity_hash,
        announced_name: (!status.hub_name.is_empty()).then_some(status.hub_name),
        hops: 1,
    })
}

fn project_reconnect_state(
    state: &mut ChannelsSnapshot,
    destination_hash: &str,
    reconnect: &ReconnectController,
    phase: ChannelRecoveryPhase,
) {
    client_hub_mut(state, destination_hash).recovery = reconnect_snapshot(reconnect, phase);
}

fn reconnect_snapshot(
    reconnect: &ReconnectController,
    phase: ChannelRecoveryPhase,
) -> ChannelHubRecoverySnapshot {
    ChannelHubRecoverySnapshot {
        phase,
        attempt: if phase == ChannelRecoveryPhase::Idle {
            0
        } else {
            reconnect.failure_streak
        },
        next_attempt_at_ms: if phase == ChannelRecoveryPhase::Idle {
            None
        } else {
            reconnect.next_attempt_at_ms
        },
        last_error: if phase == ChannelRecoveryPhase::Idle {
            None
        } else {
            reconnect.last_error.clone()
        },
    }
}

fn project_selected_reconnect_state(
    state: &mut ChannelsSnapshot,
    reconnect: &ReconnectController,
    phase: ChannelRecoveryPhase,
) {
    if let Some(destination) = state.selected_hub_destination.clone() {
        project_reconnect_state(state, &destination, reconnect, phase);
    }
}

fn project_reconnect(
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    reconnect: &ReconnectController,
    phase: ChannelRecoveryPhase,
) -> bool {
    let recovery = reconnect_snapshot(reconnect, phase);
    let should_project = snapshot.read().ok().is_some_and(|state| {
        let Some(destination) = state.selected_hub_destination.as_deref() else {
            return false;
        };
        state
            .hubs
            .iter()
            .find(|hub| hub.destination_hash == destination)
            .is_none_or(|hub| hub.recovery != recovery)
    });
    if !should_project {
        return false;
    }
    mutate_snapshot(snapshot, |state| {
        project_selected_reconnect_state(state, reconnect, phase)
    });
    true
}

fn prepare_auto_rejoin(active: &mut ActiveSession, snapshot: &Arc<RwLock<ChannelsSnapshot>>) {
    let destination = hex::encode(active.destination_hash);
    let desired_rooms = snapshot
        .read()
        .ok()
        .and_then(|state| {
            state
                .hubs
                .iter()
                .find(|hub| hub.destination_hash == destination)
                .map(|hub| {
                    hub.desired
                        .rooms
                        .iter()
                        .filter(|room| room.joined)
                        .map(|room| room.name.clone())
                        .collect::<Vec<_>>()
                })
        })
        .unwrap_or_default();
    let negotiated_limit = active
        .limits
        .max_rooms_per_session
        .unwrap_or(AUTO_REJOIN_ROOM_LIMIT);
    let limit = negotiated_limit.min(AUTO_REJOIN_ROOM_LIMIT);
    active.auto_rejoin_queue = desired_rooms.into_iter().take(limit).collect();
    active.auto_rejoining.clear();
}

async fn drive_auto_rejoin(
    active: &mut ActiveSession,
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    emitter: &Arc<dyn Emitter>,
    activity_recorder: &ChannelsActivity,
    identity: &Identity,
    stored_secrets: &mut StoredRoomSecrets,
    store: Option<&ChannelsStore>,
) -> bool {
    if active
        .rooms
        .values()
        .any(|room| room.phase == ChannelRoomPhase::Joining)
    {
        return false;
    }
    while let Some(room) = active.auto_rejoin_queue.pop_front() {
        if active
            .rooms
            .get(&room)
            .is_some_and(|state| state.phase == ChannelRoomPhase::Joined)
        {
            continue;
        }
        let destination = hex::encode(active.destination_hash);
        let secret_id = (destination.clone(), room.clone());
        let secret = match stored_secrets.get(&secret_id) {
            Some(stored) => match unseal_room_key(identity, active.destination_hash, &room, stored)
            {
                Ok(key) => JoinSecretInput::Stored(key),
                Err(_) => {
                    stored_secrets.remove(&secret_id);
                    set_room_secret_status(snapshot, &destination, &room, false, true);
                    if let Some(store) = store {
                        apply_store_result(
                            snapshot,
                            stored_secrets,
                            store
                                .remove_room_secret(destination.clone(), room.clone())
                                .await,
                        );
                    }
                    insert_auto_rejoin_error(active, room, SAVED_ROOM_KEY_UNAVAILABLE);
                    return true;
                }
            },
            None if room_key_required(snapshot, &destination, &room) => {
                insert_auto_rejoin_error(active, room, ROOM_KEY_REQUIRED);
                return true;
            }
            None => JoinSecretInput::None,
        };
        match join_room(
            Some(active),
            snapshot,
            emitter,
            activity_recorder,
            room.clone(),
            secret,
            None,
        )
        .await
        {
            Ok(room) => {
                active.auto_rejoining.insert(room);
                return true;
            }
            Err(error) => {
                let mut failed = ChannelRoomSnapshot::joining(room.clone());
                failed.phase = ChannelRoomPhase::Error;
                failed.last_error = Some(format!("Automatic rejoin failed: {error}"));
                active.rooms.insert(room, failed);
                return true;
            }
        }
    }
    false
}

fn insert_auto_rejoin_error(active: &mut ActiveSession, room: String, error: &str) {
    let mut failed = ChannelRoomSnapshot::joining(room.clone());
    failed.phase = ChannelRoomPhase::Error;
    failed.last_error = Some(error.to_string());
    active.rooms.insert(room, failed);
}

async fn apply_room_secret_actions(
    active: &mut ActiveSession,
    identity: &Identity,
    store: Option<&ChannelsStore>,
    stored_secrets: &mut StoredRoomSecrets,
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
) {
    let destination_hash = active.destination_hash;
    let destination = hex::encode(destination_hash);
    while let Some(action) = active.room_secret_actions.pop_front() {
        match action {
            RoomSecretAction::Persist { room, key } => {
                let secret_id = (destination.clone(), room.clone());
                let ciphertext = match seal_room_key(identity, destination_hash, &room, &key) {
                    Ok(ciphertext) => ciphertext,
                    Err(_) => {
                        stored_secrets.remove(&secret_id);
                        set_room_secret_status(snapshot, &destination, &room, false, true);
                        mutate_snapshot(snapshot, mark_durability_degraded);
                        continue;
                    }
                };
                let Some(store) = store else {
                    stored_secrets.remove(&secret_id);
                    set_room_secret_status(snapshot, &destination, &room, false, true);
                    mutate_snapshot(snapshot, mark_durability_degraded);
                    continue;
                };
                let result = store
                    .save_room_secret(destination.clone(), room.clone(), ciphertext)
                    .await;
                if result.is_err() {
                    stored_secrets.remove(&secret_id);
                    set_room_secret_status(snapshot, &destination, &room, false, true);
                }
                apply_store_result(snapshot, stored_secrets, result);
            }
            RoomSecretAction::ForgetRequired { room }
            | RoomSecretAction::ForgetRejected { room } => {
                stored_secrets.remove(&(destination.clone(), room.clone()));
                set_room_secret_status(snapshot, &destination, &room, false, true);
                if let Some(store) = store {
                    apply_store_result(
                        snapshot,
                        stored_secrets,
                        store
                            .remove_room_secret(destination.clone(), room.clone())
                            .await,
                    );
                }
            }
            RoomSecretAction::MarkRequired { room } => {
                let has_stored_join_key =
                    stored_secrets.contains_key(&(destination.clone(), room.clone()));
                set_room_secret_status(snapshot, &destination, &room, has_stored_join_key, true);
                if let Some(store) = store {
                    apply_store_result(
                        snapshot,
                        stored_secrets,
                        store
                            .mark_room_key_required(destination.clone(), room.clone())
                            .await,
                    );
                }
            }
        }
    }
}

fn room_key_required(
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    destination_hash: &str,
    room_name: &str,
) -> bool {
    snapshot.read().ok().is_some_and(|state| {
        state
            .hubs
            .iter()
            .find(|hub| hub.destination_hash == destination_hash)
            .and_then(|hub| hub.durable.rooms.iter().find(|room| room.name == room_name))
            .is_some_and(|room| room.join_key_required)
    })
}

fn set_room_secret_status(
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    destination_hash: &str,
    room_name: &str,
    has_stored_join_key: bool,
    join_key_required: bool,
) {
    mutate_snapshot_if_changed(snapshot, |state| {
        let Some(room) = state
            .hubs
            .iter_mut()
            .find(|hub| hub.destination_hash == destination_hash)
            .and_then(|hub| {
                hub.durable
                    .rooms
                    .iter_mut()
                    .find(|room| room.name == room_name)
            })
        else {
            return false;
        };
        if room.has_stored_join_key == has_stored_join_key
            && room.join_key_required == join_key_required
        {
            return false;
        }
        room.has_stored_join_key = has_stored_join_key;
        room.join_key_required = join_key_required;
        true
    });
}

fn client_hub_mut<'a>(
    state: &'a mut ChannelsSnapshot,
    destination_hash: &str,
) -> &'a mut ChannelClientHubStateSnapshot {
    if let Some(index) = state
        .hubs
        .iter()
        .position(|hub| hub.destination_hash == destination_hash)
    {
        return &mut state.hubs[index];
    }
    state.hubs.push(ChannelClientHubStateSnapshot::new(
        destination_hash.to_string(),
    ));
    state.hubs.last_mut().expect("hub state was inserted")
}

fn set_desired_hub(
    state: &mut ChannelsSnapshot,
    destination_hash: &str,
    nickname: &str,
    connected: bool,
) {
    if connected {
        for hub in &mut state.hubs {
            hub.desired.connected = false;
            hub.recovery = ChannelHubRecoverySnapshot::default();
        }
        state.selected_hub_destination = Some(destination_hash.to_string());
    } else if state.selected_hub_destination.as_deref() == Some(destination_hash) {
        state.selected_hub_destination = None;
    }
    let hub = client_hub_mut(state, destination_hash);
    hub.desired.connected = connected;
    if !nickname.is_empty() {
        hub.desired.nickname = Some(nickname.to_string());
    }
}

fn set_desired_room(
    state: &mut ChannelsSnapshot,
    destination_hash: &str,
    room_name: &str,
    joined: bool,
) {
    let hub = client_hub_mut(state, destination_hash);
    if let Some(room) = hub
        .desired
        .rooms
        .iter_mut()
        .find(|room| room.name == room_name)
    {
        room.joined = joined;
    } else {
        hub.desired.rooms.push(ChannelDesiredRoomSnapshot {
            name: room_name.to_string(),
            joined,
        });
    }
    hub.desired
        .rooms
        .sort_by(|left, right| left.name.cmp(&right.name));
}

fn timestamp_ms_from_seconds(seconds: f64) -> u64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * 1000.0).min(u64::MAX as f64) as u64
}

fn apply_durable_channels(
    state: &mut ChannelsSnapshot,
    hubs: Vec<db::SavedChannelHub>,
    rooms: Vec<db::SavedChannelRoom>,
    secrets: &StoredRoomSecrets,
) {
    for hub in &mut state.hubs {
        hub.durable = ChannelHubDurableSnapshot::default();
        hub.desired = ChannelHubDesiredSnapshot::default();
    }
    state.selected_hub_destination = None;

    for saved in hubs {
        let destination_hash = saved.destination_hash.clone();
        let hub = client_hub_mut(state, &destination_hash);
        hub.durable = ChannelHubDurableSnapshot {
            saved: true,
            label: saved.label,
            nickname: saved.nickname.clone(),
            added_at_ms: timestamp_ms_from_seconds(saved.added_at),
            last_connected_at_ms: timestamp_ms_from_seconds(saved.last_connected),
            rooms: Vec::new(),
        };
        hub.desired.connected = saved.desired_connected;
        hub.desired.nickname = (!saved.nickname.is_empty()).then_some(saved.nickname);
        if saved.desired_connected {
            state.selected_hub_destination = Some(destination_hash);
        }
    }

    for saved in rooms {
        let destination_hash = saved.hub_destination_hash;
        let room_name = saved.room_name;
        let has_stored_join_key =
            secrets.contains_key(&(destination_hash.clone(), room_name.clone()));
        let hub = client_hub_mut(state, &destination_hash);
        hub.durable.rooms.push(ChannelDurableRoomSnapshot {
            name: room_name.clone(),
            added_at_ms: timestamp_ms_from_seconds(saved.added_at),
            last_joined_at_ms: timestamp_ms_from_seconds(saved.last_joined),
            desired_joined: saved.desired_joined,
            join_key_required: saved.join_key_required,
            has_stored_join_key,
        });
        set_desired_room(state, &destination_hash, &room_name, saved.desired_joined);
    }
    for hub in &mut state.hubs {
        hub.durable.rooms.sort_by(|left, right| {
            right
                .last_joined_at_ms
                .cmp(&left.last_joined_at_ms)
                .then_with(|| left.name.cmp(&right.name))
        });
        if !hub.desired.connected {
            hub.recovery = ChannelHubRecoverySnapshot::default();
        }
    }
    state.durability = ChannelsDurabilitySnapshot {
        phase: ChannelsDurabilityPhase::Ready,
        last_error: None,
    };
}

fn mark_durability_degraded(state: &mut ChannelsSnapshot) {
    state.durability = ChannelsDurabilitySnapshot {
        phase: ChannelsDurabilityPhase::Degraded,
        last_error: Some("Channels preferences could not be saved".to_string()),
    };
}

fn apply_store_result(
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    stored_secrets: &mut StoredRoomSecrets,
    result: Result<DurableChannelsState, String>,
) {
    match result {
        Ok(durable) => {
            let DurableChannelsState {
                hubs,
                rooms,
                secrets,
            } = durable;
            let loaded = secrets
                .into_iter()
                .map(|secret| {
                    (
                        (
                            secret.hub_destination_hash.clone(),
                            secret.room_name.clone(),
                        ),
                        secret,
                    )
                })
                .collect();
            mutate_snapshot(snapshot, |state| {
                apply_durable_channels(state, hubs, rooms, &loaded)
            });
            *stored_secrets = loaded;
        }
        Err(error) => {
            tracing::warn!(reason = %error, "failed to load or save durable Channels state");
            mutate_snapshot(snapshot, mark_durability_degraded);
        }
    }
}

fn sync_hub_service_projection(state: &mut ChannelsSnapshot) {
    for hub in &mut state.hubs {
        hub.observed = None;
    }
    if let Some(observed_hub) = state.hub.clone() {
        let observed = ChannelHubObservedSnapshot {
            phase: state.phase,
            nickname: state.nickname.clone(),
            hub: observed_hub.clone(),
            rooms: state
                .rooms
                .iter()
                .map(|room| ChannelObservedRoomSnapshot {
                    name: room.name.clone(),
                    phase: room.phase,
                    member_count: room.members.len(),
                    members_complete: room.members_complete,
                    registered: room.registered,
                    modes: room.modes.clone(),
                    topic: room.topic.clone(),
                    last_error: room.last_error.clone(),
                })
                .collect(),
            directory: state.directory.clone(),
            greeting: state.hub_greeting.clone(),
            last_error: state.last_error.clone(),
        };
        client_hub_mut(state, &observed_hub.destination_hash).observed = Some(observed);
    }
    state.hubs.retain(|hub| {
        hub.observed.is_some()
            || hub.durable.saved
            || hub.desired.connected
            || !hub.desired.rooms.is_empty()
    });
    let selected = state.selected_hub_destination.as_deref();
    state.hubs.sort_by(|left, right| {
        let left_selected = selected == Some(left.destination_hash.as_str());
        let right_selected = selected == Some(right.destination_hash.as_str());
        right_selected
            .cmp(&left_selected)
            .then_with(|| {
                right
                    .durable
                    .last_connected_at_ms
                    .cmp(&left.durable.last_connected_at_ms)
            })
            .then_with(|| left.destination_hash.cmp(&right.destination_hash))
    });
}

/// Decide whether a live session should adopt a renamed identity's name.
///
/// A session still carrying the superseded name adopts the new one, so the
/// hub and every room member see it on the next envelope. A nickname the user
/// deliberately chose for this session differs from the old identity name and
/// is left alone.
fn adopt_renamed_nickname(
    session_nickname: &str,
    previous: &str,
    current: &str,
    max_bytes: usize,
) -> Option<String> {
    if previous.is_empty() || session_nickname != previous {
        return None;
    }
    let adopted = rrc::normalize_nickname(current, max_bytes).ok()?;
    (adopted != session_nickname).then_some(adopted)
}

fn mutate_snapshot(
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    mutate: impl FnOnce(&mut ChannelsSnapshot),
) {
    if let Ok(mut snapshot) = snapshot.write() {
        let generation = snapshot.generation;
        let revision = snapshot.revision;
        mutate(&mut snapshot);
        sync_hub_service_projection(&mut snapshot);
        // Mutations sometimes replace the whole value with `offline()` first;
        // the ordering identity belongs to the manager, never the replacement.
        snapshot.generation = generation;
        snapshot.revision = revision.saturating_add(1);
        snapshot.updated_at_ms = now_ms();
    }
}

fn mutate_snapshot_if_changed(
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    mutate: impl FnOnce(&mut ChannelsSnapshot) -> bool,
) -> bool {
    let Ok(mut snapshot) = snapshot.write() else {
        return false;
    };
    if !mutate(&mut snapshot) {
        return false;
    }
    sync_hub_service_projection(&mut snapshot);
    snapshot.revision = snapshot.revision.saturating_add(1);
    snapshot.updated_at_ms = now_ms();
    true
}

#[cfg(test)]
fn replace_snapshot(snapshot: &Arc<RwLock<ChannelsSnapshot>>, replacement: ChannelsSnapshot) {
    if let Ok(mut snapshot) = snapshot.write() {
        let generation = snapshot.generation;
        let revision = snapshot.revision.saturating_add(1);
        *snapshot = replacement;
        sync_hub_service_projection(&mut snapshot);
        snapshot.generation = generation;
        snapshot.revision = revision;
        snapshot.updated_at_ms = now_ms();
    }
}

fn emit_snapshot(emitter: &Arc<dyn Emitter>, snapshot: &Arc<RwLock<ChannelsSnapshot>>) {
    let Some(snapshot) = snapshot.read().ok().map(|snapshot| snapshot.clone()) else {
        return;
    };
    match serde_json::to_value(snapshot) {
        Ok(payload) => emitter.emit("channels_snapshot", payload),
        Err(_) => tracing::warn!(
            reason = "serialization_failed",
            "failed to serialize Channels snapshot"
        ),
    }
}

fn cancel_connection(cancel: &mut Option<oneshot::Sender<()>>) {
    if let Some(cancel) = cancel.take() {
        let _ = cancel.send(());
    }
}

fn invalidate_connect_attempt(attempt: &mut u64) -> u64 {
    *attempt = attempt.wrapping_add(1);
    *attempt
}

async fn close_active(active: &mut Option<ActiveSession>) {
    if let Some(active) = active.take() {
        active.handle.close().await;
    }
}

fn close_reason_label(reason: LinkSessionCloseReason) -> &'static str {
    match reason {
        LinkSessionCloseReason::Local => "local",
        LinkSessionCloseReason::Remote => "remote",
        LinkSessionCloseReason::Timeout => "timeout",
        LinkSessionCloseReason::TransportUnavailable => "transport unavailable",
    }
}

fn channel_close_reason(reason: LinkSessionCloseReason) -> activity::ChannelSessionCloseReason {
    match reason {
        LinkSessionCloseReason::Local => activity::ChannelSessionCloseReason::Local,
        LinkSessionCloseReason::Remote => activity::ChannelSessionCloseReason::Remote,
        LinkSessionCloseReason::Timeout => activity::ChannelSessionCloseReason::Timeout,
        LinkSessionCloseReason::TransportUnavailable => {
            activity::ChannelSessionCloseReason::TransportUnavailable
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use r2d2_sqlite::SqliteConnectionManager;
    use ratspeak_core::{NativeNotification, NativeNotificationKind, NativeNotifier};
    use rns_identity::destination::Destination;
    use rns_link::link::{CloseReason, Link};
    use rns_transport::actor::TransportActor;
    use rns_transport::link_messages::DestinationEvent;
    use rns_transport::messages::OutboundRequest;
    use std::sync::Mutex;

    use crate::channel_hub::{ChannelHubConfig, ChannelHubHandle, HubStore};

    #[derive(Default)]
    struct RecordingNotifier {
        seen: Mutex<Vec<NativeNotification>>,
    }

    impl NativeNotifier for RecordingNotifier {
        fn notify(&self, notification: NativeNotification) {
            self.seen.lock().unwrap().push(notification);
        }
    }

    #[test]
    fn hub_greeting_resource_envelopes_are_narrowly_admitted() {
        let mut envelope = Envelope::new(MessageType::ResourceEnvelope, [0x44; 16]);
        envelope.body = Some(rrc::resource_envelope_body(&rrc::ResourceEnvelopeBody {
            id: [0x11; 8],
            kind: "motd".into(),
            size: 1_024,
            sha256: Some([0x22; 32]),
            encoding: Some("UTF-8".into()),
        }));
        let admitted = match hub_greeting_resource_expectation(&envelope, 91) {
            Ok(admitted) => admitted,
            Err(_) => panic!("valid authenticated motd envelope should be admitted"),
        };
        assert_eq!(admitted.announcement_id, [0x11; 8]);
        assert_eq!(admitted.size, 1_024);
        assert_eq!(admitted.sha256, Some([0x22; 32]));
        assert_eq!(admitted.encoded_bytes, 91);

        envelope.room = Some("general".into());
        assert!(matches!(
            hub_greeting_resource_expectation(&envelope, 91),
            Err(activity::SourceValidation::Unsupported)
        ));
        envelope.room = None;

        for body in [
            rrc::ResourceEnvelopeBody {
                id: [0x11; 8],
                kind: "notice".into(),
                size: 1_024,
                sha256: None,
                encoding: Some("utf-8".into()),
            },
            rrc::ResourceEnvelopeBody {
                id: [0x11; 8],
                kind: "motd".into(),
                size: (HUB_GREETING_RESOURCE_MAX_BYTES as u64) + 1,
                sha256: None,
                encoding: Some("utf-8".into()),
            },
            rrc::ResourceEnvelopeBody {
                id: [0x11; 8],
                kind: "motd".into(),
                size: 1_024,
                sha256: None,
                encoding: Some("gzip".into()),
            },
        ] {
            envelope.body = Some(rrc::resource_envelope_body(&body));
            assert!(matches!(
                hub_greeting_resource_expectation(&envelope, 91),
                Err(activity::SourceValidation::Unsupported)
            ));
        }

        envelope.body = Some(Value::Text("not a resource envelope".into()));
        assert!(matches!(
            hub_greeting_resource_expectation(&envelope, 91),
            Err(activity::SourceValidation::Malformed)
        ));
    }

    #[test]
    fn snapshot_order_is_monotonic_and_survives_whole_value_replacement() {
        let generation = next_channels_generation();
        let snapshot = Arc::new(RwLock::new(ChannelsSnapshot::for_manager(generation)));
        let initial = snapshot.read().unwrap().clone();

        mutate_snapshot(&snapshot, |state| state.phase = ChannelsPhase::Resolving);
        let resolving = snapshot.read().unwrap().clone();
        assert_eq!(resolving.generation, generation);
        assert_eq!(resolving.revision, initial.revision + 1);

        // This mirrors the connect/ready paths, which intentionally build a
        // fresh offline value before filling in live state.
        mutate_snapshot(&snapshot, |state| {
            *state = ChannelsSnapshot::offline();
            state.phase = ChannelsPhase::Active;
        });
        let active = snapshot.read().unwrap().clone();
        assert_eq!(active.generation, generation);
        assert_eq!(active.revision, resolving.revision + 1);

        replace_snapshot(&snapshot, ChannelsSnapshot::offline());
        let offline = snapshot.read().unwrap().clone();
        assert_eq!(offline.generation, generation);
        assert_eq!(offline.revision, active.revision + 1);
        assert_eq!(offline.phase, ChannelsPhase::Offline);
    }

    #[tokio::test]
    async fn later_channel_managers_receive_newer_generations() {
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        let first = ChannelsManagerHandle::start(
            transport_tx.clone(),
            Identity::new(),
            Arc::new(ratspeak_core::NoopEmitter),
            ShutdownSignal::new(),
            Weak::new(),
        );
        let first_generation = first.snapshot().generation;
        first.shutdown().await;

        let second = ChannelsManagerHandle::start(
            transport_tx,
            Identity::new(),
            Arc::new(ratspeak_core::NoopEmitter),
            ShutdownSignal::new(),
            Weak::new(),
        );
        let second_generation = second.snapshot().generation;
        assert!(second_generation > first_generation);
        second.shutdown().await;
    }

    #[test]
    fn snapshot_order_is_part_of_the_serialized_ipc_contract() {
        let snapshot = ChannelsSnapshot::for_manager(42);
        let value = serde_json::to_value(snapshot).unwrap();
        assert_eq!(value["generation"], 42);
        assert_eq!(value["revision"], 1);
        assert_eq!(
            value["service_model_version"],
            CHANNELS_SERVICE_MODEL_VERSION
        );
        assert_eq!(value["connection_budget"], CHANNELS_CONNECTION_BUDGET);
        assert_eq!(value["history"]["phase"], "loading");
        assert_eq!(value["history"]["pending_events"], 0);
        assert_eq!(value["directory"]["phase"], "idle");
        assert_eq!(value["directory"]["complete"], false);
    }

    fn history_test_event(id: &str) -> db::NewChannelHistoryEvent {
        db::NewChannelHistoryEvent {
            hub_destination_hash: "11".repeat(16),
            room_name: "general".into(),
            event_id: id.into(),
            kind: db::ChannelHistoryKind::Message,
            timestamp_ms: now_ms(),
            source_hash: Some("22".repeat(16)),
            nickname: Some("Field Rat".into()),
            text: format!("signal {id}"),
            ours: false,
            mentioned: false,
        }
    }

    #[tokio::test]
    async fn history_writer_persists_without_blocking_the_link_actor() {
        let manager = SqliteConnectionManager::memory()
            .with_init(|connection| connection.execute_batch("PRAGMA foreign_keys=ON;"));
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        db::init_schema(&pool).unwrap();
        let identity_id = "aa".repeat(16);
        db::save_identity(&pool, &identity_id, "", "A", "A");
        let snapshot = Arc::new(RwLock::new(ChannelsSnapshot::for_manager(1)));
        let writer = ChannelHistoryWriter::start(
            Some(ChannelsStore::new(pool.clone(), identity_id.clone())),
            snapshot.clone(),
            Arc::new(ratspeak_core::NoopEmitter),
            Weak::new(),
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while writer
                .status
                .read()
                .is_ok_and(|status| status.phase != ChannelsHistoryPhase::Ready)
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();

        let mut event = history_test_event("event-1");
        event.mentioned = true;
        let mut pending = VecDeque::from([event]);
        assert!(writer.enqueue(&mut pending));
        assert!(pending.is_empty());
        writer.project(&snapshot);
        assert_eq!(snapshot.read().unwrap().history.pending_events, 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let done = writer.status.read().is_ok_and(|status| {
                    status.phase == ChannelsHistoryPhase::Ready && status.pending_events == 0
                });
                if done {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        writer.barrier().await.unwrap();

        let page =
            db::list_channel_history(&pool, &identity_id, &"11".repeat(16), "general", None, 10)
                .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].event_id, "event-1");
        assert!(page.items[0].mentioned);
        let unread = db::get_channel_unread_summary(&pool, &identity_id).unwrap();
        assert_eq!(unread.unread_total, 1);
        assert_eq!(unread.mention_total, 1);
        writer.shutdown().await;
    }

    #[test]
    fn committed_channel_notifications_are_policy_fenced_and_room_coalesced() {
        let identity = Identity::new();
        let notifier = Arc::new(RecordingNotifier::default());
        let state = channels_test_state_with_notifier(&identity, notifier.clone());
        state.set_native_notifications_enabled(true);
        state.is_foreground.store(false, Ordering::Release);
        let generation = state.current_identity_session_generation();
        let weak = Arc::downgrade(&state);

        let mut plain = history_test_event("plain");
        plain.nickname = Some("Scout".into());
        let mut mention = history_test_event("mention");
        mention.nickname = Some("Scout".into());
        mention.text = "@Field Rat check in".into();
        mention.mentioned = true;
        let batch = vec![plain, mention];
        let outcome = db::ChannelHistoryAppendOutcome {
            inserted: 2,
            duplicates: 0,
            pruned: 0,
            latest_sequence: Some("2".into()),
            inserted_events: vec![
                db::ChannelHistoryInsertedEvent {
                    batch_index: 0,
                    sequence: "1".into(),
                },
                db::ChannelHistoryInsertedEvent {
                    batch_index: 1,
                    sequence: "2".into(),
                },
            ],
        };
        let summary = db::ChannelUnreadSummary {
            rooms: vec![db::ChannelRoomUnread {
                hub_destination_hash: "11".repeat(16),
                room_name: "general".into(),
                unread_count: 2,
                mention_count: 1,
                notification_level: db::ChannelRoomNotificationLevel::Mentions,
            }],
            unread_total: 2,
            mention_total: 1,
            attention_total: 1,
        };

        notify_committed_channel_events(&weak, Some(generation), &batch, &outcome, &summary);
        let seen = notifier.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "a batch coalesces to one room alert");
        assert_eq!(seen[0].kind, NativeNotificationKind::Channel);
        assert!(seen[0].title.contains("mentioned you in general"));
        let expected_route = format!("channels:{}:{}", "11".repeat(16), hex::encode("general"));
        assert_eq!(seen[0].thread_id.as_deref(), Some(expected_route.as_str()),);
        drop(seen);

        let duplicate = db::ChannelHistoryAppendOutcome {
            duplicates: 2,
            ..db::ChannelHistoryAppendOutcome::default()
        };
        notify_committed_channel_events(&weak, Some(generation), &batch, &duplicate, &summary);
        assert_eq!(
            notifier.seen.lock().unwrap().len(),
            1,
            "deduplicated retries never replay notifications"
        );

        let mut muted = summary.clone();
        muted.rooms[0].notification_level = db::ChannelRoomNotificationLevel::Mute;
        notify_committed_channel_events(&weak, Some(generation), &batch, &outcome, &muted);
        notify_committed_channel_events(
            &weak,
            Some(generation.wrapping_add(1)),
            &batch,
            &outcome,
            &summary,
        );
        assert_eq!(
            notifier.seen.lock().unwrap().len(),
            1,
            "mute and stale identity generations suppress alerts"
        );
    }

    #[test]
    fn history_ingress_is_bounded_and_reports_irrecoverable_loss() {
        let (command_tx, _command_rx) = mpsc::channel(1);
        let status = Arc::new(RwLock::new(ChannelsHistorySnapshot {
            phase: ChannelsHistoryPhase::Ready,
            ..ChannelsHistorySnapshot::default()
        }));
        let writer = ChannelHistoryWriter {
            command_tx: Some(command_tx),
            identity_id: "aa".repeat(16),
            status: status.clone(),
            stopping: Arc::new(AtomicBool::new(false)),
        };
        let mut events = VecDeque::from([
            history_test_event("event-1"),
            history_test_event("event-2"),
            history_test_event("event-3"),
        ]);
        assert!(writer.enqueue(&mut events));
        let status = status.read().unwrap();
        assert_eq!(status.pending_events, 1);
        assert_eq!(status.dropped_events, 2);
        assert_eq!(status.phase, ChannelsHistoryPhase::Degraded);
        assert_eq!(status.last_error.as_deref(), Some(HISTORY_EVENTS_DROPPED));
    }

    #[test]
    fn hub_service_model_separates_desired_observed_and_durable_state() {
        let destination = "00112233445566778899aabbccddeeff";
        let mut snapshot = ChannelsSnapshot::for_manager(42);
        let secrets = StoredRoomSecrets::from([(
            (destination.into(), "general".into()),
            db::StoredChannelRoomSecret {
                hub_destination_hash: destination.into(),
                room_name: "general".into(),
                seal_scheme: ROOM_SECRET_SEAL_SCHEME.into(),
                seal_version: ROOM_SECRET_SEAL_VERSION,
                ciphertext: vec![1, 2, 3],
                updated_at: 5.0,
            },
        )]);
        apply_durable_channels(
            &mut snapshot,
            vec![db::SavedChannelHub {
                destination_hash: destination.into(),
                label: "Mountain relay".into(),
                nickname: "Field Rat".into(),
                added_at: 1.0,
                last_connected: 2.0,
                desired_connected: true,
            }],
            vec![db::SavedChannelRoom {
                hub_destination_hash: destination.into(),
                room_name: "general".into(),
                added_at: 3.0,
                last_joined: 4.0,
                desired_joined: true,
                join_key_required: true,
            }],
            &secrets,
        );
        snapshot.phase = ChannelsPhase::Resolving;
        snapshot.nickname = Some("Field Rat".into());
        snapshot.hub = Some(ChannelHubSnapshot::pending(
            parse_destination_hash(destination).unwrap(),
        ));
        snapshot.directory = ChannelRoomDirectorySnapshot {
            phase: ChannelRoomDirectoryPhase::Ready,
            rooms: vec![ChannelDirectoryRoomSnapshot {
                name: "lobby".into(),
                topic: Some("Public coordination".into()),
            }],
            complete: true,
            omitted_count: 0,
            refreshed_at_ms: Some(9),
            last_error: None,
        };
        sync_hub_service_projection(&mut snapshot);

        assert_eq!(
            snapshot.selected_hub_destination.as_deref(),
            Some(destination)
        );
        let hub = &snapshot.hubs[0];
        assert!(hub.desired.connected);
        assert_eq!(hub.desired.rooms[0].name, "general");
        assert!(hub.desired.rooms[0].joined);
        assert!(hub.durable.saved);
        assert_eq!(hub.durable.label, "Mountain relay");
        assert_eq!(hub.durable.added_at_ms, 1_000);
        assert!(hub.durable.rooms[0].join_key_required);
        assert!(hub.durable.rooms[0].has_stored_join_key);
        assert_eq!(
            hub.observed.as_ref().map(|observed| observed.phase),
            Some(ChannelsPhase::Resolving)
        );
        assert_eq!(
            hub.observed
                .as_ref()
                .map(|observed| observed.directory.rooms[0].name.as_str()),
            Some("lobby")
        );

        // Losing observation does not rewrite user intent or its durable copy.
        clear_observed_snapshot(&mut snapshot);
        sync_hub_service_projection(&mut snapshot);
        let hub = &snapshot.hubs[0];
        assert!(hub.observed.is_none());
        assert!(hub.desired.connected && hub.desired.rooms[0].joined);
        assert!(hub.durable.saved && hub.durable.rooms[0].desired_joined);

        // An explicit disconnect changes only connection desire; remembered
        // room intent remains available when the user selects this hub again.
        set_desired_hub(&mut snapshot, destination, "Field Rat", false);
        sync_hub_service_projection(&mut snapshot);
        assert!(snapshot.selected_hub_destination.is_none());
        assert!(!snapshot.hubs[0].desired.connected);
        assert!(snapshot.hubs[0].desired.rooms[0].joined);
    }

    #[test]
    fn selecting_one_hub_preserves_other_hub_room_intent() {
        let mut snapshot = ChannelsSnapshot::for_manager(1);
        set_desired_hub(&mut snapshot, "aa", "alpha", true);
        set_desired_room(&mut snapshot, "aa", "general", true);
        set_desired_hub(&mut snapshot, "bb", "bravo", true);
        sync_hub_service_projection(&mut snapshot);

        assert_eq!(snapshot.connection_budget, 1);
        assert_eq!(snapshot.selected_hub_destination.as_deref(), Some("bb"));
        assert_eq!(
            snapshot
                .hubs
                .iter()
                .filter(|hub| hub.desired.connected)
                .count(),
            1
        );
        let first = snapshot
            .hubs
            .iter()
            .find(|hub| hub.destination_hash == "aa")
            .unwrap();
        assert!(!first.desired.connected);
        assert!(first.desired.rooms[0].joined);
    }

    #[test]
    fn room_keys_round_trip_only_for_the_bound_identity_hub_and_room() {
        let identity = Identity::new();
        let other_identity = Identity::new();
        let destination = [0x42; 16];
        let other_destination = [0x24; 16];
        let key = "field key with spaces";
        let ciphertext = seal_room_key(&identity, destination, "general", key).unwrap();
        assert!(
            !ciphertext
                .windows(key.len())
                .any(|window| window == key.as_bytes()),
            "recoverable storage must never contain the plaintext key"
        );
        let stored = db::StoredChannelRoomSecret {
            hub_destination_hash: hex::encode(destination),
            room_name: "general".into(),
            seal_scheme: ROOM_SECRET_SEAL_SCHEME.into(),
            seal_version: ROOM_SECRET_SEAL_VERSION,
            ciphertext: ciphertext.clone(),
            updated_at: 1.0,
        };

        assert_eq!(
            unseal_room_key(&identity, destination, "general", &stored)
                .unwrap()
                .as_str(),
            key
        );
        assert!(unseal_room_key(&other_identity, destination, "general", &stored).is_err());
        assert!(unseal_room_key(&identity, other_destination, "general", &stored).is_err());
        assert!(unseal_room_key(&identity, destination, "other-room", &stored).is_err());

        let mut tampered = stored.clone();
        let last = tampered.ciphertext.len() - 1;
        tampered.ciphertext[last] ^= 0x80;
        assert!(unseal_room_key(&identity, destination, "general", &tampered).is_err());

        let mut wrong_scheme = stored;
        wrong_scheme.seal_scheme = "unknown".into();
        assert!(unseal_room_key(&identity, destination, "general", &wrong_scheme).is_err());
    }

    #[test]
    fn service_snapshot_exposes_key_availability_but_never_seal_material() {
        let destination = "00112233445566778899aabbccddeeff";
        let durable = DurableChannelsState {
            hubs: vec![db::SavedChannelHub {
                destination_hash: destination.into(),
                label: "Relay".into(),
                nickname: "Field Rat".into(),
                added_at: 1.0,
                last_connected: 2.0,
                desired_connected: true,
            }],
            rooms: vec![db::SavedChannelRoom {
                hub_destination_hash: destination.into(),
                room_name: "general".into(),
                added_at: 1.0,
                last_joined: 2.0,
                desired_joined: true,
                join_key_required: true,
            }],
            secrets: vec![db::StoredChannelRoomSecret {
                hub_destination_hash: destination.into(),
                room_name: "general".into(),
                seal_scheme: ROOM_SECRET_SEAL_SCHEME.into(),
                seal_version: ROOM_SECRET_SEAL_VERSION,
                ciphertext: b"opaque-ciphertext-marker".to_vec(),
                updated_at: 1.0,
            }],
        };
        let snapshot = Arc::new(RwLock::new(ChannelsSnapshot::for_manager(1)));
        let mut secrets = StoredRoomSecrets::new();
        apply_store_result(&snapshot, &mut secrets, Ok(durable));

        let serialized = serde_json::to_string(&*snapshot.read().unwrap()).unwrap();
        assert!(serialized.contains("\"has_stored_join_key\":true"));
        assert!(!serialized.contains("opaque-ciphertext-marker"));
        assert!(!serialized.contains(ROOM_SECRET_SEAL_SCHEME));
        assert!(!serialized.contains("seal_version"));
    }

    #[test]
    fn reconnect_backoff_is_jittered_bounded_and_stability_aware() {
        assert_eq!(reconnect_delay(1, u16::MAX / 2 + 1), RECONNECT_BASE_DELAY);
        assert_eq!(
            reconnect_delay(2, u16::MAX / 2 + 1),
            RECONNECT_BASE_DELAY * 2
        );
        assert_eq!(
            reconnect_delay(1, 0),
            Duration::from_millis(
                RECONNECT_BASE_DELAY.as_millis() as u64 * u64::from(100 - RECONNECT_JITTER_PERCENT)
                    / 100
            )
        );
        assert!(
            reconnect_delay(u32::MAX, u16::MAX) <= RECONNECT_MAX_DELAY,
            "jitter must never raise a retry above the resource ceiling"
        );

        let mut reconnect = ReconnectController {
            failure_streak: 7,
            ..ReconnectController::default()
        };
        reconnect.note_session_ended(RECONNECT_STABLE_RESET - Duration::from_millis(1));
        assert_eq!(
            reconnect.failure_streak, 7,
            "a flapping Link keeps its retry penalty"
        );
        reconnect.note_session_ended(RECONNECT_STABLE_RESET);
        assert_eq!(
            reconnect.failure_streak, 0,
            "a stable Link earns a fresh retry budget"
        );
    }

    #[test]
    fn reconnect_projection_changes_once_and_idle_clears_public_attempts() {
        let destination = "11".repeat(16);
        let mut initial = ChannelsSnapshot::offline();
        set_desired_hub(&mut initial, &destination, "Field Rat", true);
        let snapshot = Arc::new(RwLock::new(initial));
        let reconnect = ReconnectController {
            failure_streak: 3,
            next_attempt_at_ms: Some(42),
            last_error: Some("link closed".into()),
            ..ReconnectController::default()
        };

        let initial_revision = snapshot.read().unwrap().revision;
        assert!(project_reconnect(
            &snapshot,
            &reconnect,
            ChannelRecoveryPhase::Scheduled
        ));
        let scheduled_revision = snapshot.read().unwrap().revision;
        assert_eq!(scheduled_revision, initial_revision + 1);
        assert!(!project_reconnect(
            &snapshot,
            &reconnect,
            ChannelRecoveryPhase::Scheduled
        ));
        assert_eq!(snapshot.read().unwrap().revision, scheduled_revision);

        assert!(project_reconnect(
            &snapshot,
            &reconnect,
            ChannelRecoveryPhase::Idle
        ));
        let state = snapshot.read().unwrap();
        let recovery = &state.hubs[0].recovery;
        assert_eq!(recovery.phase, ChannelRecoveryPhase::Idle);
        assert_eq!(recovery.attempt, 0);
        assert_eq!(recovery.next_attempt_at_ms, None);
        assert_eq!(recovery.last_error, None);
    }

    #[test]
    fn reconnect_retries_transport_failures_but_blocks_protocol_or_policy_failures() {
        use activity::ChannelSessionFailureReason as Reason;

        for failure in [
            ConnectFailure::PathTimedOut,
            ConnectFailure::Failed(Reason::PathLookupFailed),
            ConnectFailure::Failed(Reason::TransportUnavailable),
            ConnectFailure::Failed(Reason::SendFailed),
            ConnectFailure::WelcomeRejected(Reason::WelcomeTimedOut),
            ConnectFailure::Failed(Reason::AuthenticationFailed),
        ] {
            assert!(retryable_connect_failure(failure));
        }
        for reason in [
            Reason::HubRejected,
            Reason::IdentificationFailed,
            Reason::InvalidAnnounce,
            Reason::MalformedWelcome,
            Reason::UnsupportedVersion,
            Reason::WrongSource,
        ] {
            assert!(!retryable_connect_failure(ConnectFailure::Failed(reason)));
        }
    }

    #[tokio::test]
    async fn channels_store_round_trips_scheduler_state() {
        let manager = SqliteConnectionManager::memory()
            .with_init(|connection| connection.execute_batch("PRAGMA foreign_keys=ON;"));
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        db::init_schema(&pool).unwrap();
        db::save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        let store = ChannelsStore::new(pool, "identity-a".into());

        store
            .set_hub_desired("aa".into(), "alpha".into(), true)
            .await
            .unwrap();
        let durable = store
            .set_room_desired("aa".into(), "general".into(), true)
            .await
            .unwrap();
        assert!(durable.hubs[0].desired_connected);
        assert!(durable.rooms[0].desired_joined);

        let durable = store
            .set_hub_desired("bb".into(), "bravo".into(), true)
            .await
            .unwrap();
        assert_eq!(
            durable
                .hubs
                .iter()
                .filter(|hub| hub.desired_connected)
                .count(),
            1
        );
        assert!(
            durable
                .rooms
                .iter()
                .any(|room| room.room_name == "general" && room.desired_joined)
        );

        let snapshot = Arc::new(RwLock::new(ChannelsSnapshot::for_manager(1)));
        let mut stored_secrets = StoredRoomSecrets::new();
        apply_store_result(&snapshot, &mut stored_secrets, Ok(durable));
        assert_eq!(
            snapshot.read().unwrap().selected_hub_destination.as_deref(),
            Some("bb")
        );
        db::remove_channel_hub(&store.pool, "identity-a", "bb").unwrap();
        apply_store_result(&snapshot, &mut stored_secrets, store.load().await);
        let reconciled = snapshot.read().unwrap();
        assert!(
            reconciled.selected_hub_destination.is_none(),
            "removing durable state must not leave reconnect intent only in memory"
        );
        assert!(
            reconciled
                .hubs
                .iter()
                .all(|hub| hub.destination_hash != "bb"),
            "a removed unobserved hub must leave the unified service model"
        );
    }

    #[test]
    fn parses_reference_hub_announce_and_filters_name() {
        let app_data = {
            let value = Value::Map(vec![
                (Value::Text("proto".into()), Value::Text("rrc".into())),
                (Value::Text("v".into()), Value::Integer(1.into())),
                (
                    Value::Text("hub".into()),
                    Value::Text("Mountain relay".into()),
                ),
            ]);
            let mut encoded = Vec::new();
            ciborium::ser::into_writer(&value, &mut encoded).unwrap();
            encoded
        };
        assert_eq!(
            parse_announce_hub_name(Some(&app_data)).as_deref(),
            Some("Mountain relay")
        );
    }

    #[test]
    fn destination_hash_requires_exact_reticulum_length() {
        assert_eq!(
            parse_destination_hash("00112233445566778899aabbccddeeff").unwrap(),
            hex::decode("00112233445566778899aabbccddeeff")
                .unwrap()
                .as_slice()
        );
        assert!(parse_destination_hash("0011").is_err());
        assert!(parse_destination_hash("zz112233445566778899aabbccddeeff").is_err());
    }

    #[tokio::test]
    async fn a_known_hub_identity_must_match_its_destination() {
        let client_identity = Identity::new();
        let hub_identity = Identity::new();
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        let manager = ChannelsManagerHandle::start(
            transport_tx,
            client_identity,
            Arc::new(ratspeak_core::NoopEmitter),
            ShutdownSignal::new(),
            Weak::new(),
        );

        let error = manager
            .connect_known(
                &hex::encode([0x44; 16]),
                "Field Rat",
                hub_identity.get_public_key(),
                Some("Local hub".into()),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ChannelsError::Protocol(_)));
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn a_known_local_hub_does_not_require_its_own_announce_cache_entry() {
        let client_identity = Identity::new();
        let hub_identity = Identity::new();
        let hub_destination =
            Destination::hash_from_name_and_identity(rrc::RRC_HUB_ASPECT, Some(&hub_identity.hash));
        let (transport_tx, mut transport_rx) = mpsc::channel::<TransportMessage>(16);
        let manager = ChannelsManagerHandle::start(
            transport_tx,
            client_identity,
            Arc::new(ratspeak_core::NoopEmitter),
            ShutdownSignal::new(),
            Weak::new(),
        );

        manager
            .connect_known(
                &hex::encode(hub_destination),
                "Field Rat",
                hub_identity.get_public_key(),
                Some("Local hub".into()),
            )
            .await
            .unwrap();
        let desired = manager.snapshot();
        assert_eq!(
            desired.selected_hub_destination.as_deref(),
            Some(hex::encode(hub_destination).as_str())
        );
        assert!(desired.hubs[0].desired.connected);

        match timeout_transport(&mut transport_rx).await {
            TransportMessage::RegisterDestination { .. } => {}
            other => panic!("known local hub unexpectedly queried discovery: {other:?}"),
        }
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn hosted_hub_and_client_share_one_runtime_through_an_authenticated_link() {
        let greeting =
            "Welcome to the local test hub.\nRead the field rules before transmitting.\n"
                .repeat(16);
        assert!(greeting.len() > 512);
        let (actor, transport_tx) = TransportActor::new();
        let actor_task = tokio::spawn(actor.run());
        let pool = r2d2::Pool::builder()
            .max_size(1)
            .build(SqliteConnectionManager::memory())
            .unwrap();
        crate::db::init_schema(&pool).unwrap();
        let hub_identity = Identity::new();
        let hub_public_key = hub_identity.get_public_key();
        let destination_hash = hex::encode(Destination::hash_from_name_and_identity(
            rrc::RRC_HUB_ASPECT,
            Some(&hub_identity.hash),
        ));
        let client_identity = Identity::new();
        let client_hash = client_identity.hash;
        let emitter: Arc<dyn ratspeak_core::Emitter> = Arc::new(ratspeak_core::NoopEmitter);
        let hub = ChannelHubHandle::start(
            transport_tx.clone(),
            hub_identity,
            ChannelHubConfig {
                hub_name: "Local test hub".into(),
                greeting: Some(greeting.clone()),
                ping_interval_secs: 1,
                ..ChannelHubConfig::default()
            },
            client_hash,
            HubStore::new(pool, hex::encode(client_hash)),
            emitter.clone(),
            ShutdownSignal::new(),
            Weak::new(),
        )
        .await
        .unwrap();
        let manager = ChannelsManagerHandle::start(
            transport_tx.clone(),
            client_identity,
            emitter,
            ShutdownSignal::new(),
            Weak::new(),
        );

        manager
            .connect_known(
                &destination_hash,
                "Field Rat",
                hub_public_key,
                Some("Local test hub".into()),
            )
            .await
            .unwrap();
        let active = wait_snapshot(&manager, |snapshot| {
            snapshot.phase == ChannelsPhase::Active
                && snapshot.hub_greeting.as_ref().is_some_and(|observed| {
                    observed.delivery == ChannelHubGreetingDelivery::Resource
                })
        })
        .await;
        assert_eq!(
            active.hub.as_ref().and_then(|hub| hub.name.as_deref()),
            Some("Local test hub")
        );
        let service_hub = active
            .hubs
            .iter()
            .find(|hub| hub.destination_hash == destination_hash)
            .unwrap();
        assert!(service_hub.desired.connected);
        assert_eq!(
            service_hub.observed.as_ref().map(|observed| observed.phase),
            Some(ChannelsPhase::Active)
        );
        let observed_greeting = active.hub_greeting.as_ref().unwrap();
        assert_eq!(observed_greeting.text, greeting);
        assert_eq!(
            observed_greeting.completeness,
            ChannelHubGreetingCompleteness::Complete
        );
        assert_eq!(
            service_hub
                .observed
                .as_ref()
                .and_then(|observed| observed.greeting.as_ref()),
            Some(observed_greeting)
        );

        // Let the otherwise-idle same-runtime session complete at least one
        // application PING/PONG cycle before joining. The production report
        // surfaced only after a long idle period; this keeps that lifecycle
        // boundary in the product regression without making the test slow.
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        let after_idle = manager.snapshot();
        assert!(after_idle.revision > active.revision);

        manager.join("general", None).await.unwrap();
        assert!(
            manager
                .snapshot()
                .hubs
                .iter()
                .find(|hub| hub.destination_hash == destination_hash)
                .unwrap()
                .desired
                .rooms
                .iter()
                .any(|room| room.name == "general" && room.joined)
        );
        let joined = wait_snapshot(&manager, |snapshot| {
            snapshot
                .rooms
                .iter()
                .any(|room| room.name == "general" && room.phase == ChannelRoomPhase::Joined)
        })
        .await;
        assert!(joined.rooms[0].members.iter().any(|member| member.is_self));

        manager.disconnect().await.unwrap();
        let disconnected = manager.snapshot();
        let service_hub = disconnected
            .hubs
            .iter()
            .find(|hub| hub.destination_hash == destination_hash)
            .unwrap();
        assert!(!service_hub.desired.connected);
        assert!(service_hub.observed.is_none());
        assert!(
            service_hub
                .desired
                .rooms
                .iter()
                .any(|room| room.name == "general" && room.joined),
            "disconnecting a hub does not mean leaving its rooms"
        );

        manager.shutdown().await;
        assert!(hub.shutdown().await);
        transport_tx.send(TransportMessage::Shutdown).await.unwrap();
        actor_task.await.unwrap();
    }

    #[test]
    fn parses_rrcd_room_status_without_exposing_raw_protocol_copy() {
        assert_eq!(
            parse_rrcd_room_status(
                "general",
                "room general: registered; mode=+nrt; topic=(none)"
            ),
            Some(RrcdRoomStatus {
                registered: true,
                modes: Some("+nrt".into()),
                topic: None,
            })
        );
        assert_eq!(
            parse_rrcd_room_status(
                "field-team",
                "room field-team: unregistered; mode=+n; topic=Field coordination"
            ),
            Some(RrcdRoomStatus {
                registered: false,
                modes: Some("+n".into()),
                topic: Some("Field coordination".into()),
            })
        );
        assert!(parse_rrcd_room_status("general", "Welcome to general").is_none());
        assert!(
            parse_rrcd_room_status(
                "general",
                "room another-room: registered; mode=+nrt; topic=(none)"
            )
            .is_none()
        );
    }

    fn directory_notice(source: [u8; 16], text: &str) -> Envelope {
        let mut envelope = Envelope::new(MessageType::Notice, source);
        envelope.body = Some(Value::Text(text.into()));
        envelope
    }

    #[test]
    fn parses_reference_room_directory_as_bounded_canonical_observation() {
        let hub = [7u8; 16];
        let parsed = parse_room_directory_notice(
            &directory_notice(
                hub,
                "Registered public rooms:\n  General - Field coordination\n  #Town Square\n  Почен - Привет\n  GENERAL - duplicate is ignored",
            ),
            hub,
            64,
        );
        assert_eq!(
            parsed,
            ParsedRoomDirectoryNotice::Directory {
                rooms: vec![
                    ChannelDirectoryRoomSnapshot {
                        name: "#town square".into(),
                        topic: None,
                    },
                    ChannelDirectoryRoomSnapshot {
                        name: "general".into(),
                        topic: Some("Field coordination".into()),
                    },
                    ChannelDirectoryRoomSnapshot {
                        name: "почен".into(),
                        topic: Some("Привет".into()),
                    },
                ],
                omitted_count: 0,
            }
        );
        assert_eq!(
            parse_room_directory_notice(
                &directory_notice(hub, "No public rooms registered"),
                hub,
                64,
            ),
            ParsedRoomDirectoryNotice::Directory {
                rooms: Vec::new(),
                omitted_count: 0,
            }
        );
    }

    #[test]
    fn room_directory_preserves_honest_single_packet_truncation() {
        let hub = [8u8; 16];
        assert_eq!(
            parse_room_directory_notice(
                &directory_notice(
                    hub,
                    "Registered public rooms:\n  alpha - First\n  bravo\n  (+17 more)"
                ),
                hub,
                64,
            ),
            ParsedRoomDirectoryNotice::Directory {
                rooms: vec![
                    ChannelDirectoryRoomSnapshot {
                        name: "alpha".into(),
                        topic: Some("First".into()),
                    },
                    ChannelDirectoryRoomSnapshot {
                        name: "bravo".into(),
                        topic: None,
                    },
                ],
                omitted_count: 17,
            }
        );
    }

    #[test]
    fn room_directory_requires_authenticated_roomless_exact_framing() {
        let hub = [9u8; 16];
        let peer = [10u8; 16];
        let text = "Registered public rooms:\n  general";
        assert_eq!(
            parse_room_directory_notice(&directory_notice(peer, text), hub, 64),
            ParsedRoomDirectoryNotice::NotDirectory
        );
        let mut room_scoped = directory_notice(hub, text);
        room_scoped.room = Some("general".into());
        assert_eq!(
            parse_room_directory_notice(&room_scoped, hub, 64),
            ParsedRoomDirectoryNotice::NotDirectory
        );
        assert_eq!(
            parse_room_directory_notice(
                &directory_notice(hub, "Registered public rooms\n  general"),
                hub,
                64,
            ),
            ParsedRoomDirectoryNotice::NotDirectory
        );
        assert_eq!(
            parse_room_directory_notice(
                &directory_notice(
                    hub,
                    "Registered public rooms:\n  general\n  (+2 more)\n  hidden"
                ),
                hub,
                64,
            ),
            ParsedRoomDirectoryNotice::Malformed
        );
        assert_eq!(
            parse_room_directory_notice(
                &directory_notice(hub, "Registered public rooms:\n  room - bad\u{0007}topic"),
                hub,
                64,
            ),
            ParsedRoomDirectoryNotice::Malformed
        );
        assert_eq!(
            parse_room_directory_notice(
                &directory_notice(hub, "Registered public rooms:\n  too-long"),
                hub,
                4,
            ),
            ParsedRoomDirectoryNotice::Malformed
        );
    }

    #[test]
    fn a_renamed_identity_is_adopted_only_when_the_session_still_uses_the_old_name() {
        // The reported leak: a session connected under the old identity name
        // keeps stamping it on every envelope until it adopts the new one.
        assert_eq!(
            adopt_renamed_nickname("Old Name", "Old Name", "New Name", 32).as_deref(),
            Some("New Name")
        );

        // A deliberate per-session alias is not the identity name and must
        // survive a rename untouched.
        assert_eq!(
            adopt_renamed_nickname("Radio Rat", "Old Name", "New Name", 32),
            None
        );

        // Nothing to retire, no change, and hub limits still apply.
        assert_eq!(adopt_renamed_nickname("Old Name", "", "New Name", 32), None);
        assert_eq!(
            adopt_renamed_nickname("Same", "Same", "Same", 32),
            None,
            "an unchanged name is not a rename"
        );
        assert_eq!(
            adopt_renamed_nickname("Old Name", "Old Name", "A Very Long Replacement", 8),
            None,
            "a name the hub would reject is never adopted"
        );
    }

    #[test]
    fn mentions_are_exact_literal_case_insensitive_and_identity_addressable() {
        let identity = [0xab; 16];
        assert!(channel_text_mentions(
            "Signal for @Field Rat.",
            "field rat",
            identity
        ));
        assert!(channel_text_mentions(
            "(@FieLD rAT) check in",
            "Field Rat",
            identity
        ));
        assert!(channel_text_mentions("hello @学习", "学习", identity));
        assert!(channel_text_mentions(
            "literal @Field.Rat works",
            "Field.Rat",
            identity
        ));
        assert!(channel_text_mentions(
            &format!("identity ping @{}", hex::encode(identity)),
            "someone else",
            identity
        ));

        assert!(!channel_text_mentions(
            "@Field Rattle",
            "Field Rat",
            identity
        ));
        assert!(!channel_text_mentions(
            "mail@Field Rat",
            "Field Rat",
            identity
        ));
        assert!(!channel_text_mentions(
            "@Field Rat-team",
            "Field Rat",
            identity
        ));
        assert!(!channel_text_mentions(
            &format!("@{}a", hex::encode(identity)),
            "someone else",
            identity
        ));
        assert!(!channel_text_mentions(
            "plain traffic",
            "Field Rat",
            identity
        ));
    }

    #[test]
    fn live_transcripts_are_strictly_bounded() {
        let mut room = ChannelRoomSnapshot::joining("field team".into());
        let mut history = VecDeque::new();
        for index in 0..(TRANSCRIPT_LIMIT + 20) {
            append_room_item(
                &mut history,
                [0x11; 16],
                &mut room,
                ChannelTranscriptItem {
                    id: index.to_string(),
                    kind: ChannelItemKind::Message,
                    timestamp_ms: index as u64,
                    source_hash: None,
                    source_lxmf_hash: None,
                    nickname: None,
                    text: "signal".into(),
                    ours: false,
                    mentioned: false,
                },
            );
        }
        assert_eq!(room.transcript.len(), TRANSCRIPT_LIMIT);
        assert_eq!(room.transcript.first().unwrap().id, "20");
        assert_eq!(
            history.len(),
            TRANSCRIPT_LIMIT + 20,
            "accepted items leave the small live window for the bounded writer"
        );
        let oldest = history.front().unwrap();
        assert_eq!(oldest.room_name, "field team");
        assert_eq!(oldest.hub_destination_hash, "11".repeat(16));
    }

    #[test]
    fn observed_member_upsert_promotes_nickname_only_rows_without_duplicates() {
        let mut members = vec![ChannelMemberSnapshot {
            identity_hash: None,
            lxmf_hash: None,
            nickname: Some("Field Rat".into()),
            is_self: false,
        }];
        let identity = [0x42; 16];
        let identity_hash = hex::encode(identity);

        assert!(!upsert_member(
            &mut members,
            Some(identity),
            Some("Field Rat".into()),
            false,
        ));
        assert_eq!(members.len(), 1);
        assert_eq!(
            members[0].identity_hash.as_deref(),
            Some(identity_hash.as_str())
        );
        let expected_lxmf_hash = lxmf_destination_hash(identity);
        assert_eq!(
            members[0].lxmf_hash.as_deref(),
            Some(expected_lxmf_hash.as_str())
        );
        assert_eq!(
            lxmf_destination_hash_from_identity_hex(&identity_hash).as_deref(),
            Some(expected_lxmf_hash.as_str())
        );
        assert!(lxmf_destination_hash_from_identity_hex("not-an-identity").is_none());

        assert!(!upsert_member(
            &mut members,
            Some(identity),
            Some("Field Rat 2".into()),
            false,
        ));
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].nickname.as_deref(), Some("Field Rat 2"));
    }

    #[test]
    fn room_transitions_finish_instead_of_sticking_forever() {
        let mut rooms = BTreeMap::new();
        let mut room_activity = BTreeMap::new();
        let activity_recorder = ChannelsActivity::new(Weak::new());
        let session_activity = SessionActivityContext {
            hub: activity::DestinationHash::new([0x11; 16]),
            correlation_id: CorrelationId::random(),
            origin: None,
        };
        let mut joining = ChannelRoomSnapshot::joining("general".into());
        joining.phase_started_at_ms = 1_000;
        rooms.insert(joining.name.clone(), joining);

        assert!(expire_room_transitions(
            &mut rooms,
            &mut room_activity,
            session_activity,
            &activity_recorder,
            1_000 + JOIN_CONFIRM_TIMEOUT.as_millis() as u64
        ));
        assert_eq!(rooms["general"].phase, ChannelRoomPhase::Error);
        assert!(rooms["general"].last_error.is_some());

        rooms.get_mut("general").unwrap().phase = ChannelRoomPhase::Parting;
        rooms.get_mut("general").unwrap().phase_started_at_ms = 50_000;
        assert!(expire_room_transitions(
            &mut rooms,
            &mut room_activity,
            session_activity,
            &activity_recorder,
            50_000 + PART_CONFIRM_TIMEOUT.as_millis() as u64
        ));
        assert!(rooms.is_empty());
    }

    #[tokio::test]
    async fn heartbeat_send_failure_defers_session_end_to_link_actor() {
        let client_identity = Identity::new();
        let hub_identity = Identity::new();
        let hub_signing = hub_identity.get_signing_key().unwrap();
        let hub_public = hub_identity.get_public_key();
        let hub_destination =
            Destination::hash_from_name_and_identity(rrc::RRC_HUB_ASPECT, Some(&hub_identity.hash));

        // Capacity one makes the failure deterministic: the proof for the
        // inbound PING fills the transport queue before the automatic PONG is
        // submitted. Dropping the receiver then rejects the PONG inside the
        // Link actor.
        let (transport_tx, mut transport_rx) = mpsc::channel::<TransportMessage>(1);
        let manager = ChannelsManagerHandle::start(
            transport_tx,
            client_identity.clone(),
            Arc::new(ratspeak_core::NoopEmitter),
            ShutdownSignal::new(),
            Weak::new(),
        );

        manager
            .connect(&hex::encode(hub_destination), "Field Rat")
            .await
            .unwrap();

        let response_tx = match timeout_transport(&mut transport_rx).await {
            TransportMessage::Rpc {
                query: TransportQuery::GetRecentAnnounces,
                response_tx,
            } => response_tx,
            other => panic!("expected announce query, got {other:?}"),
        };
        response_tx
            .send(TransportQueryResponse::Announces(vec![AnnounceRpcEntry {
                dest_hash: hub_destination,
                hops: 1,
                app_data: Some(reference_announce_data("Test relay")),
                timestamp: 1_700_000_000.0,
                public_key: Some(hub_public),
                ratchet: None,
                name_hash: rns_identity::name_hash::name_hash(rrc::RRC_HUB_ASPECT),
                is_path_response: false,
                retained: false,
            }]))
            .unwrap();

        let delivery_tx = match timeout_transport(&mut transport_rx).await {
            TransportMessage::RegisterDestination {
                delivery_tx: Some(delivery_tx),
                ..
            } => delivery_tx,
            other => panic!("expected Link destination registration, got {other:?}"),
        };
        let request = next_outbound(&mut transport_rx).await;
        let (_, request_offset) = rns_wire::header::PacketHeader::unpack(&request.raw).unwrap();
        let (mut responder, link_proof) = Link::new_responder(
            &request.raw[request_offset..],
            &hub_signing,
            hub_destination,
            1,
        )
        .unwrap();
        send_link_packet(
            &delivery_tx,
            responder.link_id,
            rns_wire::flags::PacketType::Proof,
            rns_wire::context::PacketContext::None,
            &link_proof,
        )
        .await;

        let lrrtt = next_attached_outbound(&mut transport_rx).await;
        let (_, lrrtt_offset) = rns_wire::header::PacketHeader::unpack(&lrrtt.raw).unwrap();
        responder
            .receive_rtt_packet(&lrrtt.raw[lrrtt_offset..])
            .unwrap();
        let identify = next_attached_outbound(&mut transport_rx).await;
        let (_, identify_offset) = rns_wire::header::PacketHeader::unpack(&identify.raw).unwrap();
        responder
            .handle_identification(&identify.raw[identify_offset..])
            .unwrap();

        let hello = receive_client_envelope(
            &mut transport_rx,
            &delivery_tx,
            &mut responder,
            &hub_signing,
        )
        .await;
        assert_eq!(hello.message_type, MessageType::Hello);

        let welcome = Envelope::new(MessageType::Welcome, hub_identity.hash);
        send_server_envelope(&delivery_tx, &mut responder, &welcome).await;
        wait_snapshot(&manager, |snapshot| snapshot.phase == ChannelsPhase::Active).await;

        let mut ping = Envelope::new(MessageType::Ping, hub_identity.hash);
        ping.body = Some(Value::Float(42.5));
        send_server_envelope(&delivery_tx, &mut responder, &ping).await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(transport_rx);

        let closed = wait_snapshot(&manager, |snapshot| {
            snapshot.phase == ChannelsPhase::Reconnecting
        })
        .await;
        let reason = closed.last_error.expect("closed session reason");
        assert!(
            reason.contains("transport unavailable"),
            "the Link actor must own the authoritative close reason: {reason}"
        );
        assert_ne!(reason, "Channel link send failed");
        let recovery = &closed.hubs[0].recovery;
        assert_eq!(recovery.phase, ChannelRecoveryPhase::Scheduled);
        assert_eq!(recovery.attempt, 1);
        assert!(recovery.next_attempt_at_ms.is_some());

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn an_unexpected_close_reconnects_and_sequentially_rejoins_desired_rooms() {
        let client_identity = Identity::new();
        let hub_identity = Identity::new();
        let hub_destination =
            Destination::hash_from_name_and_identity(rrc::RRC_HUB_ASPECT, Some(&hub_identity.hash));
        let (transport_tx, mut transport_rx) = mpsc::channel::<TransportMessage>(32);
        let manager = ChannelsManagerHandle::start(
            transport_tx,
            client_identity.clone(),
            Arc::new(ratspeak_core::NoopEmitter),
            ShutdownSignal::new(),
            Weak::new(),
        );

        manager
            .connect(&hex::encode(hub_destination), "Field Rat")
            .await
            .unwrap();
        let (first_delivery, mut first_responder) = accept_test_hub_session(
            &mut transport_rx,
            &client_identity,
            &hub_identity,
            hub_destination,
        )
        .await;
        wait_snapshot(&manager, |snapshot| snapshot.phase == ChannelsPhase::Active).await;

        let joined_for = |room_name: &str| {
            let mut joined = Envelope::new(MessageType::Joined, hub_identity.hash);
            joined.room = Some(room_name.into());
            joined.body = Some(Value::Array(vec![Value::Bytes(
                client_identity.hash.to_vec(),
            )]));
            joined
        };
        for room_name in ["general", "field"] {
            manager.join(room_name, None).await.unwrap();
            let join = receive_client_envelope(
                &mut transport_rx,
                &first_delivery,
                &mut first_responder,
                &hub_identity.get_signing_key().unwrap(),
            )
            .await;
            assert_eq!(join.message_type, MessageType::Join);
            assert_eq!(join.room.as_deref(), Some(room_name));
            send_server_envelope(
                &first_delivery,
                &mut first_responder,
                &joined_for(room_name),
            )
            .await;
            wait_snapshot(&manager, |snapshot| {
                snapshot
                    .rooms
                    .iter()
                    .any(|room| room.name == room_name && room.phase == ChannelRoomPhase::Joined)
            })
            .await;
        }

        let teardown = first_responder
            .teardown(CloseReason::DestinationClosed)
            .unwrap();
        send_link_packet(
            &first_delivery,
            first_responder.link_id,
            rns_wire::flags::PacketType::Data,
            rns_wire::context::PacketContext::LinkClose,
            &teardown,
        )
        .await;
        let recovering = wait_snapshot(&manager, |snapshot| {
            snapshot.phase == ChannelsPhase::Reconnecting
        })
        .await;
        assert!(recovering.hubs[0].desired.connected);
        assert!(
            recovering.hubs[0]
                .desired
                .rooms
                .iter()
                .filter(|room| room.joined)
                .count()
                == 2
        );

        let (second_delivery, mut second_responder) = accept_test_hub_session(
            &mut transport_rx,
            &client_identity,
            &hub_identity,
            hub_destination,
        )
        .await;
        let first_rejoin = receive_client_envelope(
            &mut transport_rx,
            &second_delivery,
            &mut second_responder,
            &hub_identity.get_signing_key().unwrap(),
        )
        .await;
        assert_eq!(first_rejoin.message_type, MessageType::Join);
        assert_eq!(first_rejoin.room.as_deref(), Some("field"));
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                receive_client_envelope(
                    &mut transport_rx,
                    &second_delivery,
                    &mut second_responder,
                    &hub_identity.get_signing_key().unwrap(),
                ),
            )
            .await
            .is_err(),
            "the next desired room must wait for hub confirmation"
        );
        send_server_envelope(
            &second_delivery,
            &mut second_responder,
            &joined_for("field"),
        )
        .await;

        let second_rejoin = receive_client_envelope(
            &mut transport_rx,
            &second_delivery,
            &mut second_responder,
            &hub_identity.get_signing_key().unwrap(),
        )
        .await;
        assert_eq!(second_rejoin.message_type, MessageType::Join);
        assert_eq!(second_rejoin.room.as_deref(), Some("general"));
        send_server_envelope(
            &second_delivery,
            &mut second_responder,
            &joined_for("general"),
        )
        .await;
        let recovered = wait_snapshot(&manager, |snapshot| {
            snapshot.hubs[0].recovery.phase == ChannelRecoveryPhase::Idle
                && ["field", "general"].iter().all(|room_name| {
                    snapshot.rooms.iter().any(|room| {
                        room.name == *room_name && room.phase == ChannelRoomPhase::Joined
                    })
                })
        })
        .await;
        assert_eq!(recovered.hubs[0].recovery.phase, ChannelRecoveryPhase::Idle);
        assert!(
            recovered
                .rooms
                .iter()
                .all(|room| room.transcript.is_empty())
        );

        manager.disconnect().await.unwrap();
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn confirmed_room_key_is_replayed_then_forgotten_after_authenticated_rejection() {
        let client_identity = Identity::new();
        let identity_id = hex::encode(client_identity.hash);
        let hub_identity = Identity::new();
        let hub_destination =
            Destination::hash_from_name_and_identity(rrc::RRC_HUB_ASPECT, Some(&hub_identity.hash));
        let hub_destination_hex = hex::encode(hub_destination);
        let state = channels_test_state(&client_identity);
        let (transport_tx, mut transport_rx) = mpsc::channel::<TransportMessage>(64);
        let manager = ChannelsManagerHandle::start(
            transport_tx,
            client_identity.clone(),
            Arc::new(ratspeak_core::NoopEmitter),
            ShutdownSignal::new(),
            Arc::downgrade(&state),
        );

        manager
            .connect(&hub_destination_hex, "Field Rat")
            .await
            .unwrap();
        let (first_delivery, mut first_responder) = accept_test_hub_session(
            &mut transport_rx,
            &client_identity,
            &hub_identity,
            hub_destination,
        )
        .await;
        wait_snapshot(&manager, |snapshot| snapshot.phase == ChannelsPhase::Active).await;

        let room = "locked";
        let join_key = "correct field key";
        manager
            .join_with_key_policy(room, Some(join_key.into()), true)
            .await
            .unwrap();
        let first_join = receive_client_envelope(
            &mut transport_rx,
            &first_delivery,
            &mut first_responder,
            &hub_identity.get_signing_key().unwrap(),
        )
        .await;
        assert_eq!(first_join.message_type, MessageType::Join);
        assert_eq!(first_join.room.as_deref(), Some(room));
        assert_eq!(rrc::text_body(&first_join), Some(join_key));
        assert!(
            db::list_channel_room_secrets_for_identity(&state.db, &identity_id)
                .unwrap()
                .is_empty(),
            "sending JOIN must not persist an unconfirmed key"
        );

        let mut joined = Envelope::new(MessageType::Joined, hub_identity.hash);
        joined.room = Some(room.into());
        joined.body = Some(Value::Array(vec![Value::Bytes(
            client_identity.hash.to_vec(),
        )]));
        send_server_envelope(&first_delivery, &mut first_responder, &joined).await;
        let confirmed = wait_snapshot(&manager, |snapshot| {
            snapshot
                .rooms
                .iter()
                .any(|room| room.name == "locked" && room.phase == ChannelRoomPhase::Joined)
                && snapshot.hubs.iter().any(|hub| {
                    hub.destination_hash == hub_destination_hex
                        && hub.durable.rooms.iter().any(|room| {
                            room.name == "locked"
                                && room.join_key_required
                                && room.has_stored_join_key
                        })
                })
                && snapshot.history.phase == ChannelsHistoryPhase::Ready
                && snapshot.history.pending_events == 0
        })
        .await;
        assert_eq!(confirmed.durability.phase, ChannelsDurabilityPhase::Ready);
        let stored = db::list_channel_room_secrets_for_identity(&state.db, &identity_id).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(
            !stored[0]
                .ciphertext
                .windows(join_key.len())
                .any(|window| window == join_key.as_bytes()),
            "the database must contain only identity-sealed ciphertext"
        );
        let history = db::list_channel_history(
            &state.db,
            &identity_id,
            &hub_destination_hex,
            room,
            None,
            10,
        )
        .unwrap();
        assert!(
            history.items.iter().any(|item| {
                item.event_id == hex::encode(joined.message_id)
                    && item.kind == db::ChannelHistoryKind::Join
                    && item.text == "You joined"
            }),
            "the same authenticated JOIN confirmation must enter local history"
        );

        let invited_room = "invited";
        manager.join(invited_room, None).await.unwrap();
        let invited_join = receive_client_envelope(
            &mut transport_rx,
            &first_delivery,
            &mut first_responder,
            &hub_identity.get_signing_key().unwrap(),
        )
        .await;
        assert_eq!(invited_join.message_type, MessageType::Join);
        assert_eq!(invited_join.room.as_deref(), Some(invited_room));
        assert_eq!(rrc::text_body(&invited_join), None);
        let mut invited_joined = Envelope::new(MessageType::Joined, hub_identity.hash);
        invited_joined.room = Some(invited_room.into());
        invited_joined.body = Some(Value::Array(vec![Value::Bytes(
            client_identity.hash.to_vec(),
        )]));
        send_server_envelope(&first_delivery, &mut first_responder, &invited_joined).await;
        wait_snapshot(&manager, |snapshot| {
            snapshot
                .rooms
                .iter()
                .any(|room| room.name == "invited" && room.phase == ChannelRoomPhase::Joined)
        })
        .await;

        close_test_hub_session(&first_delivery, &mut first_responder).await;
        wait_snapshot(&manager, |snapshot| {
            snapshot.phase == ChannelsPhase::Reconnecting
        })
        .await;
        let (second_delivery, mut second_responder) = accept_test_hub_session(
            &mut transport_rx,
            &client_identity,
            &hub_identity,
            hub_destination,
        )
        .await;
        let invited_rejoin = receive_client_envelope(
            &mut transport_rx,
            &second_delivery,
            &mut second_responder,
            &hub_identity.get_signing_key().unwrap(),
        )
        .await;
        assert_eq!(invited_rejoin.message_type, MessageType::Join);
        assert_eq!(invited_rejoin.room.as_deref(), Some(invited_room));
        assert_eq!(rrc::text_body(&invited_rejoin), None);
        let mut invited_rejoined = Envelope::new(MessageType::Joined, hub_identity.hash);
        invited_rejoined.room = Some(invited_room.into());
        invited_rejoined.body = Some(Value::Array(vec![Value::Bytes(
            client_identity.hash.to_vec(),
        )]));
        send_server_envelope(&second_delivery, &mut second_responder, &invited_rejoined).await;
        let replayed_join = receive_client_envelope(
            &mut transport_rx,
            &second_delivery,
            &mut second_responder,
            &hub_identity.get_signing_key().unwrap(),
        )
        .await;
        assert_eq!(replayed_join.message_type, MessageType::Join);
        assert_eq!(replayed_join.room.as_deref(), Some(room));
        assert_eq!(rrc::text_body(&replayed_join), Some(join_key));

        let mut forged_rejection = Envelope::new(MessageType::Error, [0x77; 16]);
        forged_rejection.room = Some(room.into());
        forged_rejection.body = Some(Value::Text(BAD_ROOM_KEY_ERROR.into()));
        send_server_envelope(&second_delivery, &mut second_responder, &forged_rejection).await;
        next_outbound_with_context(
            &mut transport_rx,
            rns_wire::context::PacketContext::LinkProof,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            manager
                .snapshot()
                .rooms
                .iter()
                .any(|room| { room.name == "locked" && room.phase == ChannelRoomPhase::Joining }),
            "a non-hub control source cannot reject a pending join"
        );
        assert_eq!(
            db::list_channel_room_secrets_for_identity(&state.db, &identity_id)
                .unwrap()
                .len(),
            1,
            "a forged bad-key error cannot delete recoverable ciphertext"
        );

        let mut rejected = Envelope::new(MessageType::Error, hub_identity.hash);
        rejected.room = Some(room.into());
        rejected.body = Some(Value::Text(BAD_ROOM_KEY_ERROR.into()));
        send_server_envelope(&second_delivery, &mut second_responder, &rejected).await;
        let rejected_snapshot = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.iter().any(|room| {
                room.name == "locked"
                    && room.phase == ChannelRoomPhase::Error
                    && room.last_error.as_deref() == Some(SAVED_ROOM_KEY_REJECTED)
            }) && snapshot.hubs.iter().any(|hub| {
                hub.destination_hash == hub_destination_hex
                    && hub.durable.rooms.iter().any(|room| {
                        room.name == "locked" && room.join_key_required && !room.has_stored_join_key
                    })
            })
        })
        .await;
        assert_eq!(
            rejected_snapshot
                .rooms
                .iter()
                .find(|room| room.name == "locked")
                .and_then(|room| room.last_error.as_deref()),
            Some(SAVED_ROOM_KEY_REJECTED)
        );
        assert!(
            db::list_channel_room_secrets_for_identity(&state.db, &identity_id)
                .unwrap()
                .is_empty(),
            "an authenticated bad-key rejection must remove replayable ciphertext"
        );
        assert!(
            rejected_snapshot.hubs.iter().any(|hub| {
                hub.destination_hash == hub_destination_hex
                    && hub
                        .desired
                        .rooms
                        .iter()
                        .any(|room| room.name == "locked" && room.joined)
            }),
            "bad-key recovery must preserve the user's room intent"
        );

        close_test_hub_session(&second_delivery, &mut second_responder).await;
        wait_snapshot(&manager, |snapshot| {
            snapshot.phase == ChannelsPhase::Reconnecting
        })
        .await;
        let (third_delivery, mut third_responder) = accept_test_hub_session(
            &mut transport_rx,
            &client_identity,
            &hub_identity,
            hub_destination,
        )
        .await;
        let invited_keyless_rejoin = receive_client_envelope(
            &mut transport_rx,
            &third_delivery,
            &mut third_responder,
            &hub_identity.get_signing_key().unwrap(),
        )
        .await;
        assert_eq!(invited_keyless_rejoin.message_type, MessageType::Join);
        assert_eq!(invited_keyless_rejoin.room.as_deref(), Some(invited_room));
        assert_eq!(rrc::text_body(&invited_keyless_rejoin), None);
        let mut invited_key_required = Envelope::new(MessageType::Error, hub_identity.hash);
        invited_key_required.room = Some(invited_room.into());
        invited_key_required.body = Some(Value::Text(BAD_ROOM_KEY_ERROR.into()));
        send_server_envelope(&third_delivery, &mut third_responder, &invited_key_required).await;
        wait_snapshot(&manager, |snapshot| {
            ["invited", "locked"].iter().all(|room_name| {
                snapshot.rooms.iter().any(|room| {
                    room.name == *room_name
                        && room.phase == ChannelRoomPhase::Error
                        && room.last_error.as_deref() == Some(ROOM_KEY_REQUIRED)
                })
            }) && snapshot.hubs.iter().any(|hub| {
                hub.destination_hash == hub_destination_hex
                    && hub.durable.rooms.iter().any(|room| {
                        room.name == "invited"
                            && room.join_key_required
                            && !room.has_stored_join_key
                    })
            })
        })
        .await;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(250),
                receive_client_envelope(
                    &mut transport_rx,
                    &third_delivery,
                    &mut third_responder,
                    &hub_identity.get_signing_key().unwrap(),
                ),
            )
            .await
            .is_err(),
            "known key-required rooms without ciphertext must not loop keyless JOINs"
        );

        let replacement_key = "rotated field key";
        manager
            .join_with_key_policy(room, Some(replacement_key.into()), false)
            .await
            .unwrap();
        let replacement_join = receive_client_envelope(
            &mut transport_rx,
            &third_delivery,
            &mut third_responder,
            &hub_identity.get_signing_key().unwrap(),
        )
        .await;
        assert_eq!(replacement_join.message_type, MessageType::Join);
        assert_eq!(replacement_join.room.as_deref(), Some(room));
        assert_eq!(rrc::text_body(&replacement_join), Some(replacement_key));
        let mut replacement_joined = Envelope::new(MessageType::Joined, hub_identity.hash);
        replacement_joined.room = Some(room.into());
        replacement_joined.body = Some(Value::Array(vec![Value::Bytes(
            client_identity.hash.to_vec(),
        )]));
        send_server_envelope(&third_delivery, &mut third_responder, &replacement_joined).await;
        wait_snapshot(&manager, |snapshot| {
            snapshot
                .rooms
                .iter()
                .any(|room| room.name == "locked" && room.phase == ChannelRoomPhase::Joined)
                && snapshot.hubs.iter().any(|hub| {
                    hub.destination_hash == hub_destination_hex
                        && hub.durable.rooms.iter().any(|room| {
                            room.name == "locked"
                                && room.join_key_required
                                && !room.has_stored_join_key
                        })
                })
        })
        .await;
        assert!(
            db::list_channel_room_secrets_for_identity(&state.db, &identity_id)
                .unwrap()
                .is_empty(),
            "opting out must keep a confirmed replacement key session-only"
        );

        manager.disconnect().await.unwrap();
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn authenticated_session_runs_welcome_room_message_ping_and_part() {
        let client_identity = Identity::new();
        let identity_id = hex::encode(client_identity.hash);
        let state = channels_test_state(&client_identity);
        let hub_identity = Identity::new();
        let hub_signing = hub_identity.get_signing_key().unwrap();
        let hub_public = hub_identity.get_public_key();
        let hub_destination =
            Destination::hash_from_name_and_identity(rrc::RRC_HUB_ASPECT, Some(&hub_identity.hash));
        let hub_destination_hex = hex::encode(hub_destination);
        let (transport_tx, mut transport_rx) = mpsc::channel::<TransportMessage>(128);
        let manager = ChannelsManagerHandle::start(
            transport_tx,
            client_identity.clone(),
            Arc::new(ratspeak_core::NoopEmitter),
            ShutdownSignal::new(),
            Arc::downgrade(&state),
        );

        manager
            .connect(&hub_destination_hex, "Field Rat")
            .await
            .unwrap();

        let response_tx = match timeout_transport(&mut transport_rx).await {
            TransportMessage::Rpc {
                query: TransportQuery::GetRecentAnnounces,
                response_tx,
            } => response_tx,
            other => panic!("expected announce query, got {other:?}"),
        };
        let announce_data = reference_announce_data("Test relay");
        response_tx
            .send(TransportQueryResponse::Announces(vec![AnnounceRpcEntry {
                dest_hash: hub_destination,
                hops: 1,
                app_data: Some(announce_data),
                timestamp: 1_700_000_000.0,
                public_key: Some(hub_public),
                ratchet: None,
                name_hash: rns_identity::name_hash::name_hash(rrc::RRC_HUB_ASPECT),
                is_path_response: false,
                retained: false,
            }]))
            .unwrap();

        let delivery_tx = match timeout_transport(&mut transport_rx).await {
            TransportMessage::RegisterDestination {
                delivery_tx: Some(delivery_tx),
                ..
            } => delivery_tx,
            other => panic!("expected Link destination registration, got {other:?}"),
        };
        let request = next_outbound(&mut transport_rx).await;
        let (request_header, request_offset) =
            rns_wire::header::PacketHeader::unpack(&request.raw).unwrap();
        assert_eq!(
            request_header.flags.packet_type,
            rns_wire::flags::PacketType::LinkRequest
        );
        let (mut responder, link_proof) = Link::new_responder(
            &request.raw[request_offset..],
            &hub_signing,
            hub_destination,
            1,
        )
        .unwrap();
        send_link_packet(
            &delivery_tx,
            responder.link_id,
            rns_wire::flags::PacketType::Proof,
            rns_wire::context::PacketContext::None,
            &link_proof,
        )
        .await;

        let lrrtt = next_attached_outbound(&mut transport_rx).await;
        let (lrrtt_header, lrrtt_offset) =
            rns_wire::header::PacketHeader::unpack(&lrrtt.raw).unwrap();
        assert_eq!(
            lrrtt_header.context,
            rns_wire::context::PacketContext::Lrrtt
        );
        responder
            .receive_rtt_packet(&lrrtt.raw[lrrtt_offset..])
            .unwrap();

        let identify = next_attached_outbound(&mut transport_rx).await;
        let (identify_header, identify_offset) =
            rns_wire::header::PacketHeader::unpack(&identify.raw).unwrap();
        assert_eq!(
            identify_header.context,
            rns_wire::context::PacketContext::LinkIdentify
        );
        assert_eq!(
            responder
                .handle_identification(&identify.raw[identify_offset..])
                .unwrap(),
            client_identity.get_public_key()
        );

        let hello = receive_client_envelope(
            &mut transport_rx,
            &delivery_tx,
            &mut responder,
            &hub_signing,
        )
        .await;
        assert_eq!(hello.message_type, MessageType::Hello);
        assert_eq!(hello.nickname.as_deref(), Some("Field Rat"));
        assert_eq!(hello.source, client_identity.hash);

        let mut welcome = Envelope::new(MessageType::Welcome, hub_identity.hash);
        welcome.body = Some(Value::Map(vec![
            (
                Value::Integer(rrc::WELCOME_HUB_NAME.into()),
                Value::Text("Test relay".into()),
            ),
            (
                Value::Integer(rrc::WELCOME_HUB_VERSION.into()),
                Value::Text("0.1.3".into()),
            ),
            (
                Value::Integer(rrc::WELCOME_CAPABILITIES.into()),
                Value::Map(vec![(
                    Value::Integer(rrc::CAP_ACTION.into()),
                    Value::Bool(true),
                )]),
            ),
            (
                Value::Integer(rrc::WELCOME_LIMITS.into()),
                Value::Map(vec![
                    (
                        Value::Integer(rrc::LIMIT_MAX_NICK_BYTES.into()),
                        Value::Integer(32.into()),
                    ),
                    (
                        Value::Integer(rrc::LIMIT_MAX_ROOM_NAME_BYTES.into()),
                        Value::Integer(64.into()),
                    ),
                    (
                        Value::Integer(rrc::LIMIT_MAX_MESSAGE_BODY_BYTES.into()),
                        Value::Integer(350.into()),
                    ),
                    (
                        Value::Integer(rrc::LIMIT_MAX_ROOMS_PER_SESSION.into()),
                        Value::Integer(8.into()),
                    ),
                ]),
            ),
        ]));
        send_server_envelope(&delivery_tx, &mut responder, &welcome).await;

        let connected =
            wait_snapshot(&manager, |snapshot| snapshot.phase == ChannelsPhase::Active).await;
        assert_eq!(
            connected.hub.as_ref().and_then(|hub| hub.name.as_deref()),
            Some("Test relay")
        );
        assert!(
            connected
                .hub
                .as_ref()
                .is_some_and(|hub| hub.capabilities.actions)
        );

        manager.refresh_directory().await.unwrap();
        let list_request = receive_client_envelope(
            &mut transport_rx,
            &delivery_tx,
            &mut responder,
            &hub_signing,
        )
        .await;
        assert_eq!(list_request.message_type, MessageType::Message);
        assert_eq!(list_request.room, None);
        assert_eq!(list_request.nickname.as_deref(), Some("Field Rat"));
        assert_eq!(rrc::text_body(&list_request), Some("/list"));
        assert_eq!(
            manager.snapshot().directory.phase,
            ChannelRoomDirectoryPhase::Loading
        );

        let directory_notice = directory_notice(
            hub_identity.hash,
            "Registered public rooms:\n  field team - Field coordination\n  lobby\n  (+3 more)",
        );
        send_server_envelope(&delivery_tx, &mut responder, &directory_notice).await;
        let directory_snapshot = wait_snapshot(&manager, |snapshot| {
            snapshot.directory.phase == ChannelRoomDirectoryPhase::Ready
        })
        .await;
        assert!(
            directory_snapshot.rooms.is_empty(),
            "listing never joins a room"
        );
        assert_eq!(directory_snapshot.directory.rooms.len(), 2);
        assert_eq!(directory_snapshot.directory.rooms[0].name, "field team");
        assert_eq!(
            directory_snapshot.directory.rooms[0].topic.as_deref(),
            Some("Field coordination")
        );
        assert!(!directory_snapshot.directory.complete);
        assert_eq!(directory_snapshot.directory.omitted_count, 3);
        assert!(
            directory_snapshot.notices.is_empty(),
            "the response to an internal refresh is structured, not chat copy"
        );
        assert_eq!(
            directory_snapshot
                .hubs
                .first()
                .and_then(|hub| hub.observed.as_ref())
                .map(|observed| observed.directory.clone()),
            Some(directory_snapshot.directory.clone())
        );

        let mut greeting = Envelope::new(MessageType::Notice, hub_identity.hash);
        greeting.body = Some(Value::Text(
            "Welcome to the test hub. /join general for the main room.".into(),
        ));
        send_server_envelope(&delivery_tx, &mut responder, &greeting).await;
        let greeting_snapshot = wait_snapshot(&manager, |snapshot| {
            snapshot.hub_greeting.as_ref().is_some_and(|greeting| {
                greeting.text == "Welcome to the test hub. /join general for the main room."
            })
        })
        .await;
        assert!(greeting_snapshot.notices.is_empty());
        let observed_greeting = greeting_snapshot.hub_greeting.as_ref().unwrap();
        assert_eq!(
            observed_greeting.delivery,
            ChannelHubGreetingDelivery::Notice
        );
        assert_eq!(
            observed_greeting.completeness,
            ChannelHubGreetingCompleteness::Unframed
        );
        assert_eq!(
            greeting_snapshot
                .hubs
                .first()
                .and_then(|hub| hub.observed.as_ref())
                .and_then(|observed| observed.greeting.as_ref()),
            Some(observed_greeting)
        );

        let mut hub_notice = Envelope::new(MessageType::Notice, hub_identity.hash);
        hub_notice.body = Some(Value::Text("Maintenance window at 04:00".into()));
        send_server_envelope(&delivery_tx, &mut responder, &hub_notice).await;
        let notice_snapshot = wait_snapshot(&manager, |snapshot| {
            snapshot
                .notices
                .iter()
                .any(|item| item.text == "Maintenance window at 04:00")
        })
        .await;
        assert_eq!(
            notice_snapshot
                .hub_greeting
                .as_ref()
                .map(|item| item.text.as_str()),
            Some("Welcome to the test hub. /join general for the main room.")
        );

        assert_eq!(
            manager.join(" Field Team ", None).await.unwrap(),
            "field team"
        );
        let join = receive_client_envelope(
            &mut transport_rx,
            &delivery_tx,
            &mut responder,
            &hub_signing,
        )
        .await;
        assert_eq!(join.message_type, MessageType::Join);
        assert_eq!(join.room.as_deref(), Some("field team"));

        let mut joined = Envelope::new(MessageType::Joined, hub_identity.hash);
        joined.room = Some("field team".into());
        joined.body = Some(Value::Array(vec![Value::Bytes(
            client_identity.hash.to_vec(),
        )]));
        send_server_envelope(&delivery_tx, &mut responder, &joined).await;
        let joined_snapshot = wait_snapshot(&manager, |snapshot| {
            snapshot
                .rooms
                .first()
                .is_some_and(|room| room.phase == ChannelRoomPhase::Joined)
        })
        .await;
        assert_eq!(joined_snapshot.rooms[0].name, "field team");
        assert!(joined_snapshot.rooms[0].members[0].is_self);

        manager.send("field team", "signal check").await.unwrap();
        let message = receive_client_envelope(
            &mut transport_rx,
            &delivery_tx,
            &mut responder,
            &hub_signing,
        )
        .await;
        assert_eq!(message.message_type, MessageType::Message);
        assert_eq!(rrc::text_body(&message), Some("signal check"));
        send_server_envelope(&delivery_tx, &mut responder, &message).await;
        let echoed = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.transcript
                    .iter()
                    .any(|item| item.text == "signal check" && item.ours)
            })
        })
        .await;
        assert!(
            echoed.rooms[0]
                .transcript
                .iter()
                .any(|item| item.text == "signal check" && item.ours)
        );

        let remote_identity = Identity::new();
        let mut mention = Envelope::new(MessageType::Message, remote_identity.hash);
        mention.room = Some("field team".into());
        mention.nickname = Some("Scout".into());
        mention.body = Some(Value::Text("Copy, @FIELD RAT.".into()));
        send_server_envelope(&delivery_tx, &mut responder, &mention).await;
        let mentioned = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.transcript
                    .iter()
                    .any(|item| item.id == hex::encode(mention.message_id) && item.mentioned)
            })
        })
        .await;
        assert!(
            mentioned.rooms[0]
                .transcript
                .iter()
                .any(|item| { item.text == "Copy, @FIELD RAT." && item.mentioned && !item.ours })
        );

        let mut ping = Envelope::new(MessageType::Ping, hub_identity.hash);
        ping.body = Some(Value::Bytes(vec![1, 2, 3, 4]));
        send_server_envelope(&delivery_tx, &mut responder, &ping).await;
        let pong = receive_client_envelope(
            &mut transport_rx,
            &delivery_tx,
            &mut responder,
            &hub_signing,
        )
        .await;
        assert_eq!(pong.message_type, MessageType::Pong);
        assert_eq!(pong.body, ping.body);

        manager.part("field team").await.unwrap();
        let part = receive_client_envelope(
            &mut transport_rx,
            &delivery_tx,
            &mut responder,
            &hub_signing,
        )
        .await;
        assert_eq!(part.message_type, MessageType::Part);
        let mut parted = Envelope::new(MessageType::Parted, hub_identity.hash);
        parted.room = Some("field team".into());
        parted.body = Some(Value::Array(vec![Value::Bytes(
            client_identity.hash.to_vec(),
        )]));
        send_server_envelope(&delivery_tx, &mut responder, &parted).await;
        wait_snapshot(&manager, |snapshot| snapshot.rooms.is_empty()).await;

        // rrcd emits this documented room status NOTICE only after accepting
        // JOIN. It is the compatibility confirmation when a populated room's
        // optional JOINED roster is too large for the Link MDU.
        assert_eq!(manager.join("general", None).await.unwrap(), "general");
        let join = receive_client_envelope(
            &mut transport_rx,
            &delivery_tx,
            &mut responder,
            &hub_signing,
        )
        .await;
        assert_eq!(join.message_type, MessageType::Join);
        let mut room_status = Envelope::new(MessageType::Notice, hub_identity.hash);
        room_status.room = Some("general".into());
        room_status.body = Some(Value::Text(
            "room general: registered; mode=+nrt; topic=(none)".into(),
        ));
        send_server_envelope(&delivery_tx, &mut responder, &room_status).await;
        let fallback_joined = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.name == "general" && room.phase == ChannelRoomPhase::Joined
            })
        })
        .await;
        assert!(!fallback_joined.rooms[0].members_complete);
        assert_eq!(fallback_joined.rooms[0].registered, Some(true));
        assert_eq!(fallback_joined.rooms[0].modes.as_deref(), Some("+nrt"));
        assert_eq!(fallback_joined.rooms[0].topic, None);
        assert!(
            fallback_joined.rooms[0]
                .members
                .iter()
                .any(|member| member.is_self)
        );
        assert!(
            fallback_joined.rooms[0]
                .transcript
                .iter()
                .any(|item| item.kind == ChannelItemKind::Join && item.ours)
        );
        assert!(
            fallback_joined.rooms[0]
                .transcript
                .iter()
                .all(|item| !item.text.starts_with("room general: registered;"))
        );

        // A nickname-only or one-member JOINED/PARTED fanout is human room
        // activity, not a roster refresh. Preserve it in the bounded
        // transcript so clients can render membership changes with messages.
        let mut member_joined = Envelope::new(MessageType::Joined, hub_identity.hash);
        member_joined.room = Some("general".into());
        member_joined.nickname = Some("v6z".into());
        send_server_envelope(&delivery_tx, &mut responder, &member_joined).await;
        let member_visible = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.members
                    .iter()
                    .any(|member| member.nickname.as_deref() == Some("v6z"))
                    && room.transcript.iter().any(|item| {
                        item.kind == ChannelItemKind::Join
                            && !item.ours
                            && item.nickname.as_deref() == Some("v6z")
                            && item.source_hash.is_none()
                            && item.text == "v6z joined"
                    })
            })
        })
        .await;
        assert_eq!(member_visible.rooms[0].members.len(), 2);

        let mut member_parted = Envelope::new(MessageType::Parted, hub_identity.hash);
        member_parted.room = Some("general".into());
        member_parted.nickname = Some("v6z".into());
        send_server_envelope(&delivery_tx, &mut responder, &member_parted).await;
        let member_left = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                !room
                    .members
                    .iter()
                    .any(|member| member.nickname.as_deref() == Some("v6z"))
                    && room.transcript.iter().any(|item| {
                        item.kind == ChannelItemKind::Part
                            && item.nickname.as_deref() == Some("v6z")
                            && item.source_hash.is_none()
                            && item.text == "v6z left"
                    })
            })
        })
        .await;
        assert_eq!(member_left.rooms[0].members.len(), 1);

        let identified_member = [0x45; 16];
        let identified_hash = hex::encode(identified_member);
        let mut identified_joined = Envelope::new(MessageType::Joined, hub_identity.hash);
        identified_joined.room = Some("general".into());
        identified_joined.nickname = Some("Ada".into());
        identified_joined.body = Some(Value::Array(vec![Value::Bytes(identified_member.to_vec())]));
        send_server_envelope(&delivery_tx, &mut responder, &identified_joined).await;
        let identified_visible = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.transcript.iter().any(|item| {
                    item.kind == ChannelItemKind::Join
                        && item.nickname.as_deref() == Some("Ada")
                        && item.source_hash.as_deref() == Some(identified_hash.as_str())
                })
            })
        })
        .await;

        // rrcd keys room membership by Link. A second Link using this same
        // identity makes the existing Link receive JOINED [self]. That is an
        // idempotent fanout, not an authoritative one-member roster.
        let partial_members_before = identified_visible.rooms[0].members.clone();
        let partial_join_count = identified_visible.rooms[0]
            .transcript
            .iter()
            .filter(|item| item.kind == ChannelItemKind::Join)
            .count();
        let mut same_identity_joined = Envelope::new(MessageType::Joined, hub_identity.hash);
        same_identity_joined.room = Some("general".into());
        same_identity_joined.nickname = Some("Other device".into());
        same_identity_joined.body = Some(Value::Array(vec![Value::Bytes(
            client_identity.hash.to_vec(),
        )]));
        send_server_envelope(&delivery_tx, &mut responder, &same_identity_joined).await;
        let mut partial_barrier = Envelope::new(MessageType::Notice, hub_identity.hash);
        partial_barrier.room = Some("general".into());
        partial_barrier.body = Some(Value::Text("partial roster barrier".into()));
        let partial_barrier_id = hex::encode(partial_barrier.message_id);
        send_server_envelope(&delivery_tx, &mut responder, &partial_barrier).await;
        let partial_after_same_identity = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.transcript
                    .iter()
                    .any(|item| item.id == partial_barrier_id)
            })
        })
        .await;
        assert_eq!(
            partial_after_same_identity.rooms[0].members,
            partial_members_before
        );
        assert!(!partial_after_same_identity.rooms[0].members_complete);
        assert_eq!(
            partial_after_same_identity.rooms[0]
                .members
                .iter()
                .filter(|member| member.is_self)
                .count(),
            1
        );
        assert_eq!(
            partial_after_same_identity.rooms[0]
                .members
                .iter()
                .find(|member| member.is_self)
                .and_then(|member| member.nickname.as_deref()),
            Some("Field Rat")
        );
        assert_eq!(
            partial_after_same_identity.rooms[0]
                .transcript
                .iter()
                .filter(|item| item.kind == ChannelItemKind::Join)
                .count(),
            partial_join_count
        );

        let mut duplicate_joined = Envelope::new(MessageType::Joined, hub_identity.hash);
        duplicate_joined.room = Some("general".into());
        duplicate_joined.nickname = Some("Ada refreshed".into());
        duplicate_joined.body = Some(Value::Array(vec![Value::Bytes(identified_member.to_vec())]));
        send_server_envelope(&delivery_tx, &mut responder, &duplicate_joined).await;
        let duplicate_observed = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.members.iter().any(|member| {
                    member.identity_hash.as_deref() == Some(identified_hash.as_str())
                        && member.nickname.as_deref() == Some("Ada refreshed")
                })
            })
        })
        .await;
        assert_eq!(
            duplicate_observed.rooms[0]
                .transcript
                .iter()
                .filter(|item| {
                    item.kind == ChannelItemKind::Join
                        && item.source_hash.as_deref() == Some(identified_hash.as_str())
                })
                .count(),
            1,
            "repeated JOINED for an already visible member is not a second arrival"
        );

        let mut identified_parted = Envelope::new(MessageType::Parted, hub_identity.hash);
        identified_parted.room = Some("general".into());
        identified_parted.nickname = Some("Ada".into());
        identified_parted.body = Some(Value::Array(vec![Value::Bytes(identified_member.to_vec())]));
        send_server_envelope(&delivery_tx, &mut responder, &identified_parted).await;
        wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.transcript.iter().any(|item| {
                    item.kind == ChannelItemKind::Part
                        && item.nickname.as_deref() == Some("Ada")
                        && item.source_hash.as_deref() == Some(identified_hash.as_str())
                })
            })
        })
        .await;

        // A room message is live evidence that its hub-reported source is
        // present even when the optional JOINED roster was not delivered.
        // ACTION updates the same observed member instead of duplicating it,
        // and a later authenticated PARTED removes that observation.
        let observed_identity = [0x43; 16];
        let observed_hash = hex::encode(observed_identity);
        let mut observed_message = Envelope::room_text(
            MessageType::Message,
            observed_identity,
            "general",
            "Observer",
            "checking in",
        );
        observed_message.timestamp_ms = u64::MAX;
        let observed_message_id = hex::encode(observed_message.message_id);
        send_server_envelope(&delivery_tx, &mut responder, &observed_message).await;
        let observed = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.members.iter().any(|member| {
                    member.identity_hash.as_deref() == Some(observed_hash.as_str())
                        && member.nickname.as_deref() == Some("Observer")
                })
            })
        })
        .await;
        assert!(!observed.rooms[0].members_complete);
        assert!(observed.rooms[0].transcript.iter().any(|item| {
            item.id == observed_message_id && item.timestamp_ms <= rrc::MAX_DISPLAY_TIMESTAMP_MS
        }));

        let mut observed_action = Envelope::room_text(
            MessageType::Action,
            observed_identity,
            "general",
            "Observer",
            "waves",
        );
        observed_action.timestamp_ms = u64::MAX;
        let observed_action_id = hex::encode(observed_action.message_id);
        send_server_envelope(&delivery_tx, &mut responder, &observed_action).await;
        let updated_observation = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.transcript
                    .iter()
                    .any(|item| item.kind == ChannelItemKind::Action && item.text == "waves")
            })
        })
        .await;
        assert_eq!(
            updated_observation.rooms[0]
                .members
                .iter()
                .filter(|member| {
                    member.identity_hash.as_deref() == Some(observed_hash.as_str())
                })
                .count(),
            1
        );
        assert!(updated_observation.rooms[0].transcript.iter().any(|item| {
            item.id == observed_action_id && item.timestamp_ms <= rrc::MAX_DISPLAY_TIMESTAMP_MS
        }));

        let persisted = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let page = db::list_channel_history(
                    &state.db,
                    &identity_id,
                    &hub_destination_hex,
                    "general",
                    None,
                    64,
                )
                .unwrap();
                if [observed_message_id.as_str(), observed_action_id.as_str()]
                    .iter()
                    .all(|event_id| page.items.iter().any(|item| item.event_id == *event_id))
                {
                    break page;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("remote message and action history timed out");
        for event_id in [&observed_message_id, &observed_action_id] {
            let item = persisted
                .items
                .iter()
                .find(|item| item.event_id == *event_id)
                .expect("sanitized remote event persisted");
            assert!(item.timestamp_ms <= rrc::MAX_DISPLAY_TIMESTAMP_MS);
        }
        let history_ready = wait_snapshot(&manager, |snapshot| {
            snapshot.history.phase == ChannelsHistoryPhase::Ready
                && snapshot.history.pending_events == 0
        })
        .await;
        assert_eq!(history_ready.history.dropped_events, 0);

        let mut observed_parted = Envelope::new(MessageType::Parted, hub_identity.hash);
        observed_parted.room = Some("general".into());
        observed_parted.nickname = Some("Observer".into());
        observed_parted.body = Some(Value::Array(vec![Value::Bytes(observed_identity.to_vec())]));
        send_server_envelope(&delivery_tx, &mut responder, &observed_parted).await;
        let observation_removed = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                !room
                    .members
                    .iter()
                    .any(|member| member.identity_hash.as_deref() == Some(observed_hash.as_str()))
            })
        })
        .await;
        assert_eq!(observation_removed.rooms[0].members.len(), 1);

        // If the larger JOINED roster arrives after the fallback notice, it
        // completes the member list without duplicating the local join event.
        let mut late_joined = Envelope::new(MessageType::Joined, hub_identity.hash);
        late_joined.room = Some("general".into());
        late_joined.body = Some(Value::Array(vec![
            Value::Bytes(client_identity.hash.to_vec()),
            Value::Bytes(vec![0x42; 16]),
        ]));
        send_server_envelope(&delivery_tx, &mut responder, &late_joined).await;
        let completed_roster = wait_snapshot(&manager, |snapshot| {
            snapshot
                .rooms
                .first()
                .is_some_and(|room| room.members_complete && room.members.len() == 2)
        })
        .await;
        assert_eq!(
            completed_roster.rooms[0]
                .transcript
                .iter()
                .filter(|item| item.kind == ChannelItemKind::Join && item.ours)
                .count(),
            1
        );
        manager.flush_history().await.unwrap();
        let roster_only_hash = hex::encode([0x42; 16]);
        let remembered =
            db::list_channel_participants(&state.db, &identity_id, &hub_destination_hex, "general")
                .unwrap();
        assert!(
            remembered.participants.iter().any(|participant| {
                participant.identity_hash.as_deref() == Some(roster_only_hash.as_str())
            }),
            "an identified roster member remains available even without an individual JOIN transcript row"
        );

        let complete_members_before = completed_roster.rooms[0].members.clone();
        let complete_join_count = completed_roster.rooms[0]
            .transcript
            .iter()
            .filter(|item| item.kind == ChannelItemKind::Join)
            .count();
        let mut same_identity_joined = Envelope::new(MessageType::Joined, hub_identity.hash);
        same_identity_joined.room = Some("general".into());
        same_identity_joined.nickname = Some("Other device".into());
        same_identity_joined.body = Some(Value::Array(vec![Value::Bytes(
            client_identity.hash.to_vec(),
        )]));
        send_server_envelope(&delivery_tx, &mut responder, &same_identity_joined).await;
        let mut complete_barrier = Envelope::new(MessageType::Notice, hub_identity.hash);
        complete_barrier.room = Some("general".into());
        complete_barrier.body = Some(Value::Text("complete roster barrier".into()));
        let complete_barrier_id = hex::encode(complete_barrier.message_id);
        send_server_envelope(&delivery_tx, &mut responder, &complete_barrier).await;
        let complete_after_same_identity = wait_snapshot(&manager, |snapshot| {
            snapshot.rooms.first().is_some_and(|room| {
                room.transcript
                    .iter()
                    .any(|item| item.id == complete_barrier_id)
            })
        })
        .await;
        assert_eq!(
            complete_after_same_identity.rooms[0].members,
            complete_members_before
        );
        assert!(complete_after_same_identity.rooms[0].members_complete);
        assert_eq!(
            complete_after_same_identity.rooms[0]
                .members
                .iter()
                .filter(|member| member.is_self)
                .count(),
            1
        );
        assert_eq!(
            complete_after_same_identity.rooms[0]
                .members
                .iter()
                .find(|member| member.is_self)
                .and_then(|member| member.nickname.as_deref()),
            Some("Field Rat")
        );
        assert_eq!(
            complete_after_same_identity.rooms[0]
                .transcript
                .iter()
                .filter(|item| item.kind == ChannelItemKind::Join)
                .count(),
            complete_join_count
        );

        manager.part("general").await.unwrap();
        let part = receive_client_envelope(
            &mut transport_rx,
            &delivery_tx,
            &mut responder,
            &hub_signing,
        )
        .await;
        assert_eq!(part.message_type, MessageType::Part);
        let mut parted = Envelope::new(MessageType::Parted, hub_identity.hash);
        parted.room = Some("general".into());
        parted.body = Some(Value::Array(vec![Value::Bytes(
            client_identity.hash.to_vec(),
        )]));
        send_server_envelope(&delivery_tx, &mut responder, &parted).await;
        wait_snapshot(&manager, |snapshot| snapshot.rooms.is_empty()).await;

        manager.disconnect().await.unwrap();
        let close = next_outbound_with_context(
            &mut transport_rx,
            rns_wire::context::PacketContext::LinkClose,
        )
        .await;
        let (_, close_offset) = rns_wire::header::PacketHeader::unpack(&close.raw).unwrap();
        assert!(responder.receive_teardown(&close.raw[close_offset..]));
        assert_eq!(manager.snapshot().phase, ChannelsPhase::Offline);
        manager.shutdown().await;
    }

    fn reference_announce_data(name: &str) -> Vec<u8> {
        let value = Value::Map(vec![
            (Value::Text("proto".into()), Value::Text("rrc".into())),
            (Value::Text("v".into()), Value::Integer(1.into())),
            (Value::Text("hub".into()), Value::Text(name.into())),
        ]);
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&value, &mut encoded).unwrap();
        encoded
    }

    async fn timeout_transport(rx: &mut mpsc::Receiver<TransportMessage>) -> TransportMessage {
        timeout_transport_within(rx, Duration::from_secs(3)).await
    }

    async fn timeout_transport_within(
        rx: &mut mpsc::Receiver<TransportMessage>,
        duration: Duration,
    ) -> TransportMessage {
        tokio::time::timeout(duration, rx.recv())
            .await
            .expect("transport message timed out")
            .expect("transport channel closed")
    }

    async fn next_outbound(rx: &mut mpsc::Receiver<TransportMessage>) -> OutboundRequest {
        loop {
            if let TransportMessage::Outbound(request) = timeout_transport(rx).await {
                return request;
            }
        }
    }

    async fn next_attached_outbound(rx: &mut mpsc::Receiver<TransportMessage>) -> OutboundRequest {
        loop {
            if let TransportMessage::OutboundAttached {
                request,
                interface_id: 1,
            } = timeout_transport(rx).await
            {
                return request;
            }
        }
    }

    async fn next_outbound_with_context(
        rx: &mut mpsc::Receiver<TransportMessage>,
        wanted: rns_wire::context::PacketContext,
    ) -> OutboundRequest {
        loop {
            let request = next_attached_outbound(rx).await;
            if rns_wire::header::PacketHeader::unpack(&request.raw)
                .is_ok_and(|(header, _)| header.context == wanted)
            {
                return request;
            }
        }
    }

    async fn receive_client_envelope(
        rx: &mut mpsc::Receiver<TransportMessage>,
        delivery_tx: &mpsc::Sender<DestinationEvent>,
        responder: &mut Link,
        signing_key: &rns_crypto::ed25519::Ed25519PrivateKey,
    ) -> Envelope {
        loop {
            let request = next_attached_outbound(rx).await;
            let Ok((header, offset)) = rns_wire::header::PacketHeader::unpack(&request.raw) else {
                continue;
            };
            if header.flags.packet_type != rns_wire::flags::PacketType::Data
                || header.context != rns_wire::context::PacketContext::None
            {
                continue;
            }
            let plaintext = responder.decrypt(&request.raw[offset..]).unwrap();
            let packet_hash = rns_wire::hash::packet_hash(&request.raw, header.flags.header_type);
            let proof = responder.prove_packet(&packet_hash, signing_key).unwrap();
            send_link_packet(
                delivery_tx,
                responder.link_id,
                rns_wire::flags::PacketType::Proof,
                rns_wire::context::PacketContext::LinkProof,
                &proof,
            )
            .await;
            return rrc::decode(&plaintext).unwrap();
        }
    }

    async fn accept_test_hub_session(
        transport_rx: &mut mpsc::Receiver<TransportMessage>,
        client_identity: &Identity,
        hub_identity: &Identity,
        hub_destination: [u8; 16],
    ) -> (mpsc::Sender<DestinationEvent>, Link) {
        let response_tx = loop {
            match timeout_transport_within(transport_rx, Duration::from_secs(6)).await {
                TransportMessage::Rpc {
                    query: TransportQuery::GetRecentAnnounces,
                    response_tx,
                } => break response_tx,
                _ => continue,
            }
        };
        response_tx
            .send(TransportQueryResponse::Announces(vec![AnnounceRpcEntry {
                dest_hash: hub_destination,
                hops: 1,
                app_data: Some(reference_announce_data("Recovery relay")),
                timestamp: 1_700_000_000.0,
                public_key: Some(hub_identity.get_public_key()),
                ratchet: None,
                name_hash: rns_identity::name_hash::name_hash(rrc::RRC_HUB_ASPECT),
                is_path_response: false,
                retained: false,
            }]))
            .unwrap();
        let delivery_tx = loop {
            match timeout_transport(transport_rx).await {
                TransportMessage::RegisterDestination {
                    delivery_tx: Some(delivery_tx),
                    ..
                } => break delivery_tx,
                _ => continue,
            }
        };
        let request = next_outbound(transport_rx).await;
        let (_, request_offset) = rns_wire::header::PacketHeader::unpack(&request.raw).unwrap();
        let signing = hub_identity.get_signing_key().unwrap();
        let (mut responder, link_proof) =
            Link::new_responder(&request.raw[request_offset..], &signing, hub_destination, 1)
                .unwrap();
        send_link_packet(
            &delivery_tx,
            responder.link_id,
            rns_wire::flags::PacketType::Proof,
            rns_wire::context::PacketContext::None,
            &link_proof,
        )
        .await;
        let lrrtt = next_attached_outbound(transport_rx).await;
        let (_, lrrtt_offset) = rns_wire::header::PacketHeader::unpack(&lrrtt.raw).unwrap();
        responder
            .receive_rtt_packet(&lrrtt.raw[lrrtt_offset..])
            .unwrap();
        let identify = next_attached_outbound(transport_rx).await;
        let (_, identify_offset) = rns_wire::header::PacketHeader::unpack(&identify.raw).unwrap();
        assert_eq!(
            responder
                .handle_identification(&identify.raw[identify_offset..])
                .unwrap(),
            client_identity.get_public_key()
        );
        let hello =
            receive_client_envelope(transport_rx, &delivery_tx, &mut responder, &signing).await;
        assert_eq!(hello.message_type, MessageType::Hello);
        let mut welcome = Envelope::new(MessageType::Welcome, hub_identity.hash);
        welcome.body = Some(Value::Map(vec![(
            Value::Integer(rrc::WELCOME_HUB_NAME.into()),
            Value::Text("Recovery relay".into()),
        )]));
        send_server_envelope(&delivery_tx, &mut responder, &welcome).await;
        (delivery_tx, responder)
    }

    async fn send_server_envelope(
        delivery_tx: &mpsc::Sender<DestinationEvent>,
        responder: &mut Link,
        envelope: &Envelope,
    ) {
        let encoded = rrc::encode(envelope).unwrap();
        let encrypted = responder.encrypt(&encoded).unwrap();
        send_link_packet(
            delivery_tx,
            responder.link_id,
            rns_wire::flags::PacketType::Data,
            rns_wire::context::PacketContext::None,
            &encrypted,
        )
        .await;
    }

    async fn close_test_hub_session(
        delivery_tx: &mpsc::Sender<DestinationEvent>,
        responder: &mut Link,
    ) {
        let teardown = responder.teardown(CloseReason::DestinationClosed).unwrap();
        send_link_packet(
            delivery_tx,
            responder.link_id,
            rns_wire::flags::PacketType::Data,
            rns_wire::context::PacketContext::LinkClose,
            &teardown,
        )
        .await;
    }

    async fn send_link_packet(
        delivery_tx: &mpsc::Sender<DestinationEvent>,
        link_id: [u8; 16],
        packet_type: rns_wire::flags::PacketType,
        context: rns_wire::context::PacketContext,
        body: &[u8],
    ) {
        let header = rns_wire::header::PacketHeader {
            flags: rns_wire::flags::PacketFlags {
                header_type: rns_wire::flags::HeaderType::Header1,
                context_flag: false,
                transport_type: rns_wire::flags::TransportType::Broadcast,
                destination_type: rns_wire::flags::DestinationType::Link,
                packet_type,
            },
            hops: 0,
            transport_id: None,
            destination_hash: link_id,
            context,
        };
        let mut raw = header.pack();
        raw.extend_from_slice(body);
        delivery_tx
            .send(DestinationEvent::InboundPacket {
                raw: Bytes::from(raw),
                interface_id: 1,
                metrics: Default::default(),
            })
            .await
            .unwrap();
    }

    fn channels_test_state_with_notifier(
        identity: &Identity,
        notifier: Arc<dyn NativeNotifier>,
    ) -> Arc<AppState> {
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-channels-key-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manager = SqliteConnectionManager::memory()
            .with_init(|connection| connection.execute_batch("PRAGMA foreign_keys=ON;"));
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        db::init_schema(&pool).unwrap();
        let identity_id = hex::encode(identity.hash);
        db::save_identity(&pool, &identity_id, &identity_id, "Field Rat", "Field Rat");
        db::set_active_identity(&pool, &identity_id).unwrap();
        Arc::new(AppState::new(
            crate::config::DashboardConfig::from_env_and_defaults(tmp),
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            notifier,
        ))
    }

    fn channels_test_state(identity: &Identity) -> Arc<AppState> {
        channels_test_state_with_notifier(identity, Arc::new(ratspeak_core::NoopNotifier))
    }

    async fn wait_snapshot(
        manager: &ChannelsManagerHandle,
        predicate: impl Fn(&ChannelsSnapshot) -> bool,
    ) -> ChannelsSnapshot {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = manager.snapshot();
                if predicate(&snapshot) {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("snapshot state timed out")
    }
}
