//! Live Reticulum Relay Chat sessions.
//!
//! Channels are intentionally session-scoped: room membership and transcripts
//! exist only while the authenticated Reticulum Link is alive. Nothing in this
//! module writes channel traffic to the Ratspeak database.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::pending;
use std::io::Cursor;
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ciborium::value::Value;
use ratspeak_core::Emitter;
use rns_identity::identity::Identity;
use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::link_session::{
    LinkSession, LinkSessionCloseReason, LinkSessionConfig, LinkSessionError, LinkSessionEvent,
};
use rns_transport::messages::{
    AnnounceRpcEntry, TransportMessage, TransportQuery, TransportQueryResponse,
};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use crate::activity::{CorrelationId, producer as activity};
use crate::rrc::{self, Envelope, HubLimits, MessageType, WelcomeInfo};
use crate::state::{ActivityRequestFence, AppState};

const COMMAND_BUFFER: usize = 64;
const CONNECT_UPDATE_BUFFER: usize = 32;
const CONNECT_PATH_TIMEOUT: Duration = Duration::from_secs(30);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(15);
const HUB_GREETING_WINDOW: Duration = Duration::from_secs(30);
const JOIN_CONFIRM_TIMEOUT: Duration = Duration::from_secs(30);
const PART_CONFIRM_TIMEOUT: Duration = Duration::from_secs(15);
const ROOM_TRANSITION_TICK: Duration = Duration::from_secs(1);
const DEFAULT_NICK_MAX_BYTES: usize = 32;
const DEFAULT_ROOM_MAX_BYTES: usize = 64;
const DEFAULT_MESSAGE_MAX_BYTES: usize = 350;
const TRANSCRIPT_LIMIT: usize = 300;
const NOTICE_LIMIT: usize = 100;
const SEEN_MESSAGE_LIMIT: usize = 2_048;

#[derive(Clone)]
struct ChannelsActivity {
    state: Weak<AppState>,
}

impl ChannelsActivity {
    fn new(state: Weak<AppState>) -> Self {
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

    fn record_spontaneous<F>(&self, make: F)
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
    /// Kept visible for compatibility reporting. Ratspeak does not advertise
    /// or accept the resource-envelope extension in the first Channels beta.
    pub resource_envelopes: bool,
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
    pub nickname: Option<String>,
    pub is_self: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelTranscriptItem {
    pub id: String,
    pub kind: ChannelItemKind,
    pub timestamp_ms: u64,
    pub source_hash: Option<String>,
    pub nickname: Option<String>,
    pub text: String,
    pub ours: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelsSnapshot {
    pub protocol_version: &'static str,
    pub phase: ChannelsPhase,
    pub nickname: Option<String>,
    pub hub: Option<ChannelHubSnapshot>,
    pub rooms: Vec<ChannelRoomSnapshot>,
    /// The first authenticated roomless hub NOTICE after WELCOME. `rrcd`
    /// delivers its configured greeting this way, so keep it in hub context
    /// instead of merging it into every room transcript.
    pub hub_greeting: Option<ChannelTranscriptItem>,
    pub notices: Vec<ChannelTranscriptItem>,
    pub last_error: Option<String>,
    pub updated_at_ms: u64,
}

impl ChannelsSnapshot {
    pub fn offline() -> Self {
        Self {
            protocol_version: "0.1.3",
            phase: ChannelsPhase::Offline,
            nickname: None,
            hub: None,
            rooms: Vec::new(),
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
    #[error("the hub's room limit has been reached")]
    RoomLimitReached,
    #[error("channel hub rejected the session: {0}")]
    HubRejected(String),
    #[error("channel protocol error: {0}")]
    Protocol(String),
    #[error("channel transport error: {0}")]
    Transport(String),
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
        activity_fence: Option<ActivityRequestFence>,
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    Disconnect {
        activity_fence: Option<ActivityRequestFence>,
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    Join {
        room: String,
        key: Option<String>,
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
    Shutdown {
        activity_fence: Option<ActivityRequestFence>,
        result_tx: oneshot::Sender<()>,
    },
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
        let snapshot = Arc::new(RwLock::new(ChannelsSnapshot::offline()));
        let activity = ChannelsActivity::new(state);
        tokio::spawn(run_manager(
            transport_tx,
            identity,
            emitter,
            shutdown,
            command_rx,
            snapshot.clone(),
            activity.clone(),
        ));
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
        let activity_fence = self.activity.capture_fence();
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Connect {
                destination_hash,
                nickname,
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
        let activity_fence = self.activity.capture_fence();
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Join {
                room: room.to_string(),
                key,
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

struct ActiveSession {
    handle: rns_runtime::link_session::LinkSessionHandle,
    events: mpsc::Receiver<LinkSessionEvent>,
    source: [u8; 16],
    hub_identity: [u8; 16],
    nickname: String,
    supports_action: bool,
    limits: HubLimits,
    rooms: BTreeMap<String, ChannelRoomSnapshot>,
    hub_greeting: Option<ChannelTranscriptItem>,
    hub_greeting_deadline_ms: u64,
    notices: VecDeque<ChannelTranscriptItem>,
    seen_ids: HashSet<[u8; 8]>,
    seen_order: VecDeque<[u8; 8]>,
    message_tokens: HashMap<[u8; 8], activity::ChannelMessageToken>,
    message_token_order: VecDeque<[u8; 8]>,
    room_activity: BTreeMap<String, RoomActivityContext>,
    activity: SessionActivityContext,
}

impl ActiveSession {
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

#[derive(Clone, Copy)]
enum ConnectFailure {
    PathTimedOut,
    WelcomeRejected(activity::ChannelSessionFailureReason),
    Failed(activity::ChannelSessionFailureReason),
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
    update_tx: mpsc::Sender<ConnectUpdate>,
    cancel_rx: oneshot::Receiver<()>,
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
}

async fn run_manager(
    transport_tx: mpsc::Sender<TransportMessage>,
    identity: Identity,
    emitter: Arc<dyn Emitter>,
    shutdown: ShutdownSignal,
    mut command_rx: mpsc::Receiver<ChannelsCommand>,
    snapshot: Arc<RwLock<ChannelsSnapshot>>,
    activity: ChannelsActivity,
) {
    let source = identity.hash;
    let (connect_update_tx, mut connect_update_rx) = mpsc::channel(CONNECT_UPDATE_BUFFER);
    let mut active: Option<ActiveSession> = None;
    let mut attempt: u64 = 0;
    let mut connect_cancel: Option<oneshot::Sender<()>> = None;
    let mut pending_connect_activity: Option<SessionActivityContext> = None;
    let mut room_transition_tick = tokio::time::interval(ROOM_TRANSITION_TICK);
    room_transition_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

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
                        activity::ChannelSessionTransition::Closed {
                            reason: activity::ChannelSessionCloseReason::Local,
                        },
                    );
                }
                close_active(&mut active).await;
                replace_snapshot(&snapshot, ChannelsSnapshot::offline());
                emit_snapshot(&emitter, &snapshot);
                break;
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
                            activity::ChannelSessionTransition::Closed {
                                reason: activity::ChannelSessionCloseReason::Local,
                            },
                        );
                    }
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
                        activity_fence,
                        result_tx,
                    } => {
                        let phase = snapshot.read().ok().map(|s| s.phase);
                        if matches!(phase, Some(ChannelsPhase::Resolving | ChannelsPhase::Connecting | ChannelsPhase::AwaitingWelcome)) {
                            let _ = result_tx.send(Err(ChannelsError::AlreadyConnecting));
                            continue;
                        }
                        let this_attempt = invalidate_connect_attempt(&mut attempt);
                        cancel_connection(&mut connect_cancel);
                        if let Some(session) = active.as_ref() {
                            record_session_command(
                                &activity,
                                session.activity,
                                activity_fence,
                                activity::ChannelSessionTransition::Closed {
                                    reason: activity::ChannelSessionCloseReason::Local,
                                },
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
                        mutate_snapshot(&snapshot, |state| {
                            *state = ChannelsSnapshot::offline();
                            state.phase = ChannelsPhase::Resolving;
                            state.nickname = Some(nickname.clone());
                            state.hub = Some(ChannelHubSnapshot::pending(destination_hash));
                        });
                        emit_snapshot(&emitter, &snapshot);

                        let (cancel_tx, cancel_rx) = oneshot::channel();
                        connect_cancel = Some(cancel_tx);
                        tokio::spawn(run_connect_attempt(ConnectAttemptInput {
                            attempt: this_attempt,
                            transport_tx: transport_tx.clone(),
                            identity: identity.clone(),
                            destination_hash,
                            nickname,
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
                                activity::ChannelSessionTransition::Closed {
                                    reason: activity::ChannelSessionCloseReason::Local,
                                },
                            );
                        }
                        close_active(&mut active).await;
                        replace_snapshot(&snapshot, ChannelsSnapshot::offline());
                        emit_snapshot(&emitter, &snapshot);
                        let _ = result_tx.send(Ok(()));
                    }
                    ChannelsCommand::Join {
                        room,
                        key,
                        activity_fence,
                        result_tx,
                    } => {
                        let result = join_room(
                            active.as_mut(),
                            &snapshot,
                            &emitter,
                            &activity,
                            room,
                            key,
                            activity_fence,
                        ).await;
                        let _ = result_tx.send(result);
                    }
                    ChannelsCommand::Part {
                        room,
                        activity_fence,
                        result_tx,
                    } => {
                        let result = part_room(
                            active.as_mut(),
                            &snapshot,
                            &emitter,
                            &activity,
                            room,
                            activity_fence,
                        ).await;
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
                                activity::ChannelSessionTransition::Closed {
                                    reason: activity::ChannelSessionCloseReason::Local,
                                },
                            );
                        }
                        close_active(&mut active).await;
                        replace_snapshot(&snapshot, ChannelsSnapshot::offline());
                        emit_snapshot(&emitter, &snapshot);
                        let _ = result_tx.send(());
                        break;
                    }
                }
            }
            update = connect_update_rx.recv() => {
                let Some(update) = update else { continue; };
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
                    },
                ).await;
            }
            _ = room_transition_tick.tick() => {
                if let Some(active) = active.as_mut()
                    && expire_room_transitions(
                        &mut active.rooms,
                        &mut active.room_activity,
                        active.activity,
                        &activity,
                        now_ms(),
                    )
                {
                    sync_session_snapshot(active, &snapshot);
                    emit_snapshot(&emitter, &snapshot);
                }
            }
            event = async {
                match active.as_mut() {
                    Some(session) => session.events.recv().await,
                    None => pending::<Option<LinkSessionEvent>>().await,
                }
            } => {
                match event {
                    Some(event) => {
                        let outcome = handle_link_event(
                            active.as_mut().expect("active branch"),
                            &activity,
                            event,
                        ).await;
                        match outcome {
                            LinkEventOutcome::Keep => {
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
                                record_lost_room_operations(&activity, session);
                                record_session_spontaneous(
                                    &activity,
                                    session.activity,
                                    activity::ChannelSessionTransition::Closed {
                                        reason: activity_reason,
                                    },
                                );
                                active = None;
                                mutate_snapshot(&snapshot, |state| {
                                    state.phase = ChannelsPhase::Error;
                                    state.rooms.clear();
                                    state.hub_greeting = None;
                                    state.notices.clear();
                                    state.last_error = Some(product_reason);
                                });
                            }
                        }
                        emit_snapshot(&emitter, &snapshot);
                    }
                    None => {
                        if let Some(session) = active.as_ref() {
                            record_lost_room_operations(&activity, session);
                            record_session_spontaneous(
                                &activity,
                                session.activity,
                                activity::ChannelSessionTransition::Closed {
                                    reason: activity::ChannelSessionCloseReason::StreamEnded,
                                },
                            );
                        }
                        active = None;
                        mutate_snapshot(&snapshot, |state| {
                            state.phase = ChannelsPhase::Error;
                            state.rooms.clear();
                            state.hub_greeting = None;
                            state.notices.clear();
                            state.last_error = Some("Channel link closed".into());
                        });
                        emit_snapshot(&emitter, &snapshot);
                    }
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
        update_tx,
        cancel_rx,
        activity_context,
    } = input;
    let connect = connect_to_hub(
        attempt,
        transport_tx,
        identity,
        destination_hash,
        nickname,
        update_tx.clone(),
        activity_context,
    );
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

async fn connect_to_hub(
    attempt: u64,
    transport_tx: mpsc::Sender<TransportMessage>,
    identity: Identity,
    destination_hash: [u8; 16],
    nickname: String,
    update_tx: mpsc::Sender<ConnectUpdate>,
    activity_context: SessionActivityContext,
) -> Result<ConnectedSession, ConnectAttemptError> {
    send_session_activity_update(
        &update_tx,
        attempt,
        activity::ChannelSessionTransition::PathRequested,
    )
    .await;
    let announce = rns_runtime::link_session::discover_destination(
        &transport_tx,
        destination_hash,
        CONNECT_PATH_TIMEOUT,
    )
    .await
    .map_err(path_connect_error)?;
    let public_key = announce.public_key.ok_or_else(|| ConnectAttemptError {
        product: ChannelsError::Transport("channel hub announce has no public key".into()),
        activity: ConnectFailure::Failed(activity::ChannelSessionFailureReason::InvalidAnnounce),
    })?;
    let hub_identity = Identity::from_public_key(&public_key)
        .map_err(|error| ConnectAttemptError {
            product: ChannelsError::Transport(error.to_string()),
            activity: ConnectFailure::Failed(
                activity::ChannelSessionFailureReason::InvalidAnnounce,
            ),
        })?
        .hash;
    let announced_name = parse_announce_hub_name(announce.app_data.as_deref());
    let hops = announce.hops.max(1);
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
                LinkSessionEvent::Recovered | LinkSessionEvent::PacketDelivered { .. } => {}
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
        | LinkSessionError::PayloadTooLarge { .. } => {
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
    } = context;
    let update_attempt = match &update {
        ConnectUpdate::SessionActivity { attempt, .. }
        | ConnectUpdate::EnvelopeActivity { attempt, .. }
        | ConnectUpdate::Discovered { attempt, .. }
        | ConnectUpdate::AwaitingWelcome { attempt, .. }
        | ConnectUpdate::Ready { attempt, .. }
        | ConnectUpdate::Failed { attempt, .. } => *attempt,
    };
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
                state.rooms.clear();
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
            let mut live = ActiveSession {
                handle: session.handle,
                events: session.events,
                source,
                hub_identity,
                nickname: nickname.clone(),
                supports_action: capabilities.actions,
                limits: welcome.limits.clone(),
                rooms: BTreeMap::new(),
                hub_greeting: None,
                hub_greeting_deadline_ms: now_ms()
                    .saturating_add(HUB_GREETING_WINDOW.as_millis() as u64),
                notices: VecDeque::new(),
                seen_ids: HashSet::new(),
                seen_order: VecDeque::new(),
                message_tokens: HashMap::new(),
                message_token_order: VecDeque::new(),
                room_activity: BTreeMap::new(),
                activity,
            };
            for (envelope, encoded_bytes) in buffered {
                let _ =
                    handle_envelope(&mut live, activity_recorder, envelope, encoded_bytes).await;
            }
            mutate_snapshot(snapshot, |state| {
                *state = ChannelsSnapshot::offline();
                state.phase = ChannelsPhase::Active;
                state.nickname = Some(nickname);
                state.hub = Some(ChannelHubSnapshot {
                    destination_hash: hex::encode(destination_hash),
                    identity_hash: Some(hex::encode(hub_identity)),
                    announced_name,
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
    key: Option<String>,
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
    if active
        .limits
        .max_rooms_per_session
        .is_some_and(|limit| active.rooms.len() >= limit)
    {
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
    if let Some(key) = key.filter(|key| !key.is_empty()) {
        envelope.body = Some(Value::Text(key));
    }
    if let Err(error) =
        send_active_envelope(active, activity_recorder, &envelope, activity_fence).await
    {
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

enum LinkEventOutcome {
    Keep,
    Stale,
    Recovered,
    Closed {
        product_reason: String,
        activity_reason: activity::ChannelSessionCloseReason,
    },
}

async fn handle_link_event(
    active: &mut ActiveSession,
    activity_recorder: &ChannelsActivity,
    event: LinkSessionEvent,
) -> LinkEventOutcome {
    match event {
        LinkSessionEvent::Packet { data, .. } => match rrc::decode(&data) {
            Ok(envelope) => {
                if handle_envelope(
                    active,
                    activity_recorder,
                    envelope,
                    bounded_encoded_len(data.len()),
                )
                .await
                {
                    LinkEventOutcome::Keep
                } else {
                    LinkEventOutcome::Closed {
                        product_reason: "Channel link send failed".into(),
                        activity_reason: activity::ChannelSessionCloseReason::SendFailed,
                    }
                }
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
        LinkSessionEvent::PacketDelivered { .. } => LinkEventOutcome::Keep,
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
    envelope: Envelope,
    encoded_bytes: u32,
) -> bool {
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
        return true;
    }
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
        return true;
    }
    if matches!(
        envelope.message_type,
        MessageType::ResourceEnvelope | MessageType::Unknown(_)
    ) {
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
        return true;
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
        if send_active_envelope_spontaneous(active, activity_recorder, &pong)
            .await
            .is_err()
        {
            return false;
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
            } else {
                append_content(active, activity_recorder, &envelope, encoded_bytes)
            }
        }
        MessageType::Error => append_error(active, activity_recorder, &envelope),
        MessageType::ResourceEnvelope | MessageType::Unknown(_) => {}
    }
    true
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
    let Some(room) = active.rooms.get_mut(&room_name) else {
        return;
    };
    if room.phase == ChannelRoomPhase::Parting {
        return;
    }
    let identities = rrc::member_identities(envelope);
    let includes_self = identities.contains(&active.source);
    let joining_self = room.phase == ChannelRoomPhase::Joining || includes_self;
    if room.phase == ChannelRoomPhase::Error && !joining_self {
        return;
    }
    room.phase = ChannelRoomPhase::Joined;
    room.phase_started_at_ms = now_ms();
    room.last_error = None;

    if !identities.is_empty() {
        if joining_self || includes_self {
            room.members.clear();
        }
        let single_member_nickname = (identities.len() == 1)
            .then(|| envelope.nickname.clone())
            .flatten();
        for identity in identities {
            upsert_member(
                &mut room.members,
                Some(identity),
                if identity == active.source {
                    Some(active.nickname.clone())
                } else {
                    single_member_nickname.clone()
                },
                identity == active.source,
            );
        }
        room.members_complete = joining_self || includes_self;
    } else if joining_self {
        upsert_member(
            &mut room.members,
            Some(active.source),
            Some(active.nickname.clone()),
            true,
        );
        room.members_complete = false;
    } else if let Some(nickname) = envelope.nickname.clone() {
        upsert_member(&mut room.members, None, Some(nickname), false);
    }

    let nickname = if joining_self {
        Some(active.nickname.clone())
    } else {
        envelope.nickname.clone()
    };
    let join_already_visible = joining_self
        && room
            .transcript
            .iter()
            .any(|item| item.kind == ChannelItemKind::Join && item.ours);
    if !join_already_visible {
        append_room_item(
            room,
            transcript_item(
                envelope,
                ChannelItemKind::Join,
                nickname.clone(),
                if joining_self {
                    "You joined".into()
                } else {
                    format!("{} joined", nickname.unwrap_or_else(|| "A member".into()))
                },
                joining_self,
            ),
        );
    }
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
        if !room
            .transcript
            .iter()
            .any(|item| item.kind == ChannelItemKind::Join && item.ours)
        {
            append_room_item(
                room,
                ChannelTranscriptItem {
                    id: format!("{}-joined", hex::encode(envelope.message_id)),
                    kind: ChannelItemKind::Join,
                    timestamp_ms: envelope.timestamp_ms,
                    source_hash: Some(hex::encode(active.source)),
                    nickname: Some(active.nickname.clone()),
                    text: "You joined".into(),
                    ours: true,
                },
            );
        }
    }
    true
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
    append_room_item(
        room,
        transcript_item(
            envelope,
            ChannelItemKind::Part,
            nickname.clone(),
            format!("{} left", nickname.unwrap_or_else(|| "A member".into())),
            false,
        ),
    );
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
    let item = transcript_item(
        envelope,
        kind,
        envelope.nickname.clone(),
        text.to_string(),
        envelope.source == active.source,
    );
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
                nickname,
                envelope.source == active.source,
            );
            if inserted {
                room.members_complete = false;
            }
        }
        append_room_item(room, item);
    } else if envelope.message_type == MessageType::Notice {
        if active.hub_greeting.is_none()
            && envelope.source == active.hub_identity
            && now_ms() <= active.hub_greeting_deadline_ms
        {
            active.hub_greeting = Some(item);
            record_session_spontaneous(
                activity_recorder,
                active.activity,
                activity::ChannelSessionTransition::GreetingObserved { encoded_bytes },
            );
        } else {
            append_bounded(&mut active.notices, item, NOTICE_LIMIT);
        }
    }
}

fn append_error(
    active: &mut ActiveSession,
    activity_recorder: &ChannelsActivity,
    envelope: &Envelope,
) {
    let text = rrc::text_body(envelope).unwrap_or("Channel hub reported an error");
    let item = transcript_item(
        envelope,
        ChannelItemKind::Error,
        None,
        text.to_string(),
        false,
    );
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
    if let Some(room_name) = explicit_room.or(inferred_room)
        && let Some(room) = active.rooms.get_mut(&room_name)
    {
        let prior_phase = room.phase;
        if room.phase == ChannelRoomPhase::Joining {
            room.phase = ChannelRoomPhase::Error;
            room.phase_started_at_ms = now_ms();
            room.last_error = Some(text.to_string());
        } else if room.phase == ChannelRoomPhase::Parting {
            room.phase = ChannelRoomPhase::Joined;
            room.phase_started_at_ms = now_ms();
            room.last_error = Some(text.to_string());
        }
        append_room_item(room, item);
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
        nickname,
        text,
        ours,
    }
}

fn append_room_item(room: &mut ChannelRoomSnapshot, item: ChannelTranscriptItem) {
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

fn upsert_member(
    members: &mut Vec<ChannelMemberSnapshot>,
    identity: Option<[u8; 16]>,
    nickname: Option<String>,
    is_self: bool,
) -> bool {
    let identity_hash = identity.map(hex::encode);
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
        if nickname.is_some() {
            existing.nickname = nickname;
        }
        existing.is_self |= is_self;
        false
    } else {
        members.push(ChannelMemberSnapshot {
            identity_hash,
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
        state.hub_greeting = active.hub_greeting.clone();
        state.notices = active.notices.iter().cloned().collect();
    });
}

fn mutate_snapshot(
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    mutate: impl FnOnce(&mut ChannelsSnapshot),
) {
    if let Ok(mut snapshot) = snapshot.write() {
        mutate(&mut snapshot);
        snapshot.updated_at_ms = now_ms();
    }
}

fn replace_snapshot(snapshot: &Arc<RwLock<ChannelsSnapshot>>, replacement: ChannelsSnapshot) {
    if let Ok(mut snapshot) = snapshot.write() {
        *snapshot = replacement;
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
    use rns_identity::destination::Destination;
    use rns_link::link::Link;
    use rns_transport::link_messages::DestinationEvent;
    use rns_transport::messages::OutboundRequest;

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

    #[test]
    fn live_transcripts_are_strictly_bounded() {
        let mut room = ChannelRoomSnapshot::joining("field team".into());
        for index in 0..(TRANSCRIPT_LIMIT + 20) {
            room.transcript.push(ChannelTranscriptItem {
                id: index.to_string(),
                kind: ChannelItemKind::Message,
                timestamp_ms: index as u64,
                source_hash: None,
                nickname: None,
                text: "signal".into(),
                ours: false,
            });
            if room.transcript.len() > TRANSCRIPT_LIMIT {
                room.transcript.remove(0);
            }
        }
        assert_eq!(room.transcript.len(), TRANSCRIPT_LIMIT);
        assert_eq!(room.transcript.first().unwrap().id, "20");
    }

    #[test]
    fn observed_member_upsert_promotes_nickname_only_rows_without_duplicates() {
        let mut members = vec![ChannelMemberSnapshot {
            identity_hash: None,
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
    async fn authenticated_session_runs_welcome_room_message_ping_and_part() {
        let client_identity = Identity::new();
        let hub_identity = Identity::new();
        let hub_signing = hub_identity.get_signing_key().unwrap();
        let hub_public = hub_identity.get_public_key();
        let hub_destination =
            Destination::hash_from_name_and_identity(rrc::RRC_HUB_ASPECT, Some(&hub_identity.hash));
        let (transport_tx, mut transport_rx) = mpsc::channel::<TransportMessage>(128);
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

        let mut greeting = Envelope::new(MessageType::Notice, hub_identity.hash);
        greeting.body = Some(Value::Text(
            "Welcome to the test hub. /join general for the main room.".into(),
        ));
        send_server_envelope(&delivery_tx, &mut responder, &greeting).await;
        let greeting_snapshot = wait_snapshot(&manager, |snapshot| {
            snapshot.hub_greeting.as_ref().is_some_and(|item| {
                item.text == "Welcome to the test hub. /join general for the main room."
            })
        })
        .await;
        assert!(greeting_snapshot.notices.is_empty());

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
        observed_message.timestamp_ms = now_ms();
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

        let observed_action = Envelope::room_text(
            MessageType::Action,
            observed_identity,
            "general",
            "Observer",
            "waves",
        );
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
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
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
            })
            .await
            .unwrap();
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
