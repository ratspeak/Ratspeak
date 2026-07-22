//! Live Reticulum Relay Chat sessions.
//!
//! Channels are intentionally session-scoped: room membership and transcripts
//! exist only while the authenticated Reticulum Link is alive. Nothing in this
//! module writes channel traffic to the Ratspeak database.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::future::pending;
use std::io::Cursor;
use std::sync::{Arc, RwLock};
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

use crate::rrc::{self, Envelope, HubLimits, MessageType, WelcomeInfo};

const COMMAND_BUFFER: usize = 64;
const CONNECT_UPDATE_BUFFER: usize = 32;
const CONNECT_PATH_TIMEOUT: Duration = Duration::from_secs(30);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_NICK_MAX_BYTES: usize = 32;
const DEFAULT_ROOM_MAX_BYTES: usize = 64;
const DEFAULT_MESSAGE_MAX_BYTES: usize = 350;
const TRANSCRIPT_LIMIT: usize = 300;
const NOTICE_LIMIT: usize = 100;
const SEEN_MESSAGE_LIMIT: usize = 2_048;

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
    /// Stable Reticulum identity hash when the hub supplies one. Some hubs omit
    /// optional member lists, in which case only an advisory nickname is known.
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
    pub members: Vec<ChannelMemberSnapshot>,
    /// False means the hub did not advertise the optional JOINED member list;
    /// the visible members are then best-effort live observations only.
    pub members_complete: bool,
    pub transcript: Vec<ChannelTranscriptItem>,
    pub last_error: Option<String>,
}

impl ChannelRoomSnapshot {
    fn joining(name: String) -> Self {
        Self {
            name,
            phase: ChannelRoomPhase::Joining,
            members: Vec::new(),
            members_complete: false,
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
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    Disconnect {
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    Join {
        room: String,
        key: Option<String>,
        result_tx: oneshot::Sender<Result<String, ChannelsError>>,
    },
    Part {
        room: String,
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    Send {
        room: String,
        text: String,
        result_tx: oneshot::Sender<Result<(), ChannelsError>>,
    },
    Shutdown {
        result_tx: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct ChannelsManagerHandle {
    command_tx: mpsc::Sender<ChannelsCommand>,
    snapshot: Arc<RwLock<ChannelsSnapshot>>,
}

impl ChannelsManagerHandle {
    pub fn start(
        transport_tx: mpsc::Sender<TransportMessage>,
        identity: Identity,
        emitter: Arc<dyn Emitter>,
        shutdown: ShutdownSignal,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_BUFFER);
        let snapshot = Arc::new(RwLock::new(ChannelsSnapshot::offline()));
        tokio::spawn(run_manager(
            transport_tx,
            identity,
            emitter,
            shutdown,
            command_rx,
            snapshot.clone(),
        ));
        Self {
            command_tx,
            snapshot,
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
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Connect {
                destination_hash,
                nickname,
                result_tx,
            })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    pub async fn disconnect(&self) -> Result<(), ChannelsError> {
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Disconnect { result_tx })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    pub async fn join(&self, room: &str, key: Option<String>) -> Result<String, ChannelsError> {
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Join {
                room: room.to_string(),
                key,
                result_tx,
            })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    pub async fn part(&self, room: &str) -> Result<(), ChannelsError> {
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Part {
                room: room.to_string(),
                result_tx,
            })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    pub async fn send(&self, room: &str, text: &str) -> Result<(), ChannelsError> {
        let (result_tx, result_rx) = oneshot::channel();
        self.command_tx
            .send(ChannelsCommand::Send {
                room: room.to_string(),
                text: text.to_string(),
                result_tx,
            })
            .await
            .map_err(|_| ChannelsError::Stopped)?;
        result_rx.await.map_err(|_| ChannelsError::Stopped)?
    }

    pub async fn shutdown(&self) {
        let (result_tx, result_rx) = oneshot::channel();
        if self
            .command_tx
            .send(ChannelsCommand::Shutdown { result_tx })
            .await
            .is_ok()
        {
            let _ = tokio::time::timeout(Duration::from_secs(2), result_rx).await;
        }
    }
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
    notices: VecDeque<ChannelTranscriptItem>,
    seen_ids: HashSet<[u8; 8]>,
    seen_order: VecDeque<[u8; 8]>,
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
}

struct ConnectedSession {
    session: LinkSession,
    destination_hash: [u8; 16],
    hub_identity: [u8; 16],
    announced_name: Option<String>,
    hops: u8,
    nickname: String,
    welcome: WelcomeInfo,
    buffered: Vec<Envelope>,
}

enum ConnectUpdate {
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
    },
}

async fn run_manager(
    transport_tx: mpsc::Sender<TransportMessage>,
    identity: Identity,
    emitter: Arc<dyn Emitter>,
    shutdown: ShutdownSignal,
    mut command_rx: mpsc::Receiver<ChannelsCommand>,
    snapshot: Arc<RwLock<ChannelsSnapshot>>,
) {
    let source = identity.hash;
    let (connect_update_tx, mut connect_update_rx) = mpsc::channel(CONNECT_UPDATE_BUFFER);
    let mut active: Option<ActiveSession> = None;
    let mut attempt: u64 = 0;
    let mut connect_cancel: Option<oneshot::Sender<()>> = None;

    loop {
        tokio::select! {
            _ = shutdown.wait() => {
                cancel_connection(&mut connect_cancel);
                close_active(&mut active).await;
                replace_snapshot(&snapshot, ChannelsSnapshot::offline());
                emit_snapshot(&emitter, &snapshot);
                break;
            }
            command = command_rx.recv() => {
                let Some(command) = command else {
                    cancel_connection(&mut connect_cancel);
                    close_active(&mut active).await;
                    break;
                };
                match command {
                    ChannelsCommand::Discover { result_tx } => {
                        let _ = result_tx.send(discover_hubs(&transport_tx).await);
                    }
                    ChannelsCommand::Connect { destination_hash, nickname, result_tx } => {
                        let phase = snapshot.read().ok().map(|s| s.phase);
                        if matches!(phase, Some(ChannelsPhase::Resolving | ChannelsPhase::Connecting | ChannelsPhase::AwaitingWelcome)) {
                            let _ = result_tx.send(Err(ChannelsError::AlreadyConnecting));
                            continue;
                        }
                        cancel_connection(&mut connect_cancel);
                        close_active(&mut active).await;
                        attempt = attempt.wrapping_add(1);
                        let this_attempt = attempt;
                        mutate_snapshot(&snapshot, |state| {
                            *state = ChannelsSnapshot::offline();
                            state.phase = ChannelsPhase::Resolving;
                            state.nickname = Some(nickname.clone());
                            state.hub = Some(ChannelHubSnapshot::pending(destination_hash));
                        });
                        emit_snapshot(&emitter, &snapshot);

                        let (cancel_tx, cancel_rx) = oneshot::channel();
                        connect_cancel = Some(cancel_tx);
                        tokio::spawn(run_connect_attempt(
                            this_attempt,
                            transport_tx.clone(),
                            identity.clone(),
                            destination_hash,
                            nickname,
                            connect_update_tx.clone(),
                            cancel_rx,
                        ));
                        let _ = result_tx.send(Ok(()));
                    }
                    ChannelsCommand::Disconnect { result_tx } => {
                        cancel_connection(&mut connect_cancel);
                        close_active(&mut active).await;
                        replace_snapshot(&snapshot, ChannelsSnapshot::offline());
                        emit_snapshot(&emitter, &snapshot);
                        let _ = result_tx.send(Ok(()));
                    }
                    ChannelsCommand::Join { room, key, result_tx } => {
                        let result = join_room(active.as_mut(), &snapshot, &emitter, room, key).await;
                        let _ = result_tx.send(result);
                    }
                    ChannelsCommand::Part { room, result_tx } => {
                        let result = part_room(active.as_mut(), &snapshot, &emitter, room).await;
                        let _ = result_tx.send(result);
                    }
                    ChannelsCommand::Send { room, text, result_tx } => {
                        let result = send_room_text(active.as_mut(), room, text).await;
                        let _ = result_tx.send(result);
                    }
                    ChannelsCommand::Shutdown { result_tx } => {
                        cancel_connection(&mut connect_cancel);
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
                    attempt,
                    &mut connect_cancel,
                    &mut active,
                    &snapshot,
                    &emitter,
                    source,
                ).await;
            }
            event = async {
                match active.as_mut() {
                    Some(session) => session.events.recv().await,
                    None => pending::<Option<LinkSessionEvent>>().await,
                }
            } => {
                match event {
                    Some(event) => {
                        let outcome = handle_link_event(active.as_mut().expect("active branch"), event).await;
                        match outcome {
                            LinkEventOutcome::Keep => {
                                sync_session_snapshot(active.as_ref().expect("active session"), &snapshot);
                            }
                            LinkEventOutcome::Stale => {
                                sync_session_snapshot(active.as_ref().expect("active session"), &snapshot);
                                mutate_snapshot(&snapshot, |state| state.phase = ChannelsPhase::Stale);
                            }
                            LinkEventOutcome::Recovered => {
                                sync_session_snapshot(active.as_ref().expect("active session"), &snapshot);
                                mutate_snapshot(&snapshot, |state| {
                                    state.phase = ChannelsPhase::Active;
                                    state.last_error = None;
                                });
                            }
                            LinkEventOutcome::Closed(reason) => {
                                active = None;
                                mutate_snapshot(&snapshot, |state| {
                                    state.phase = ChannelsPhase::Error;
                                    state.rooms.clear();
                                    state.notices.clear();
                                    state.last_error = Some(reason);
                                });
                            }
                        }
                        emit_snapshot(&emitter, &snapshot);
                    }
                    None => {
                        active = None;
                        mutate_snapshot(&snapshot, |state| {
                            state.phase = ChannelsPhase::Error;
                            state.rooms.clear();
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

async fn run_connect_attempt(
    attempt: u64,
    transport_tx: mpsc::Sender<TransportMessage>,
    identity: Identity,
    destination_hash: [u8; 16],
    nickname: String,
    update_tx: mpsc::Sender<ConnectUpdate>,
    cancel_rx: oneshot::Receiver<()>,
) {
    let connect = connect_to_hub(
        attempt,
        transport_tx,
        identity,
        destination_hash,
        nickname,
        update_tx.clone(),
    );
    tokio::select! {
        _ = cancel_rx => {}
        result = connect => {
            let update = match result {
                Ok(connected) => ConnectUpdate::Ready { attempt, connected: Box::new(connected) },
                Err(error) => ConnectUpdate::Failed { attempt, error },
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
) -> Result<ConnectedSession, ChannelsError> {
    let announce = rns_runtime::link_session::discover_destination(
        &transport_tx,
        destination_hash,
        CONNECT_PATH_TIMEOUT,
    )
    .await?;
    let public_key = announce
        .public_key
        .ok_or_else(|| ChannelsError::Transport("channel hub announce has no public key".into()))?;
    let hub_identity = Identity::from_public_key(&public_key)
        .map_err(|error| ChannelsError::Transport(error.to_string()))?
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
    .await?;

    let hello = Envelope::hello(identity.hash, &nickname, env!("CARGO_PKG_VERSION"));
    session.handle.send_packet(rrc::encode(&hello)?).await?;
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
                            tracing::debug!(error = %error, "ignoring malformed pre-WELCOME channel envelope");
                            continue;
                        }
                    };
                    match envelope.message_type {
                        MessageType::Welcome => {
                            if envelope.source != hub_identity {
                                return Err(ChannelsError::Protocol(
                                    "WELCOME source does not match the authenticated hub".into(),
                                ));
                            }
                            let welcome = rrc::parse_welcome(&envelope);
                            let max_nick = welcome
                                .limits
                                .max_nick_bytes
                                .unwrap_or(DEFAULT_NICK_MAX_BYTES);
                            rrc::normalize_nickname(&nickname, max_nick)?;
                            return Ok(welcome);
                        }
                        MessageType::Ping => {
                            let pong = Envelope::pong(identity.hash, &envelope);
                            session.handle.send_packet(rrc::encode(&pong)?).await?;
                        }
                        MessageType::Error => {
                            return Err(ChannelsError::HubRejected(
                                rrc::text_body(&envelope)
                                    .unwrap_or("connection rejected")
                                    .to_string(),
                            ));
                        }
                        MessageType::Notice => buffered.push(envelope),
                        _ => {}
                    }
                }
                LinkSessionEvent::Closed { reason } => {
                    return Err(ChannelsError::Transport(format!(
                        "link closed before WELCOME ({})",
                        close_reason_label(reason)
                    )));
                }
                LinkSessionEvent::Stale => {
                    return Err(ChannelsError::Transport(
                        "channel link became stale before WELCOME".into(),
                    ));
                }
                LinkSessionEvent::Recovered | LinkSessionEvent::PacketDelivered { .. } => {}
            }
        }
        Err(ChannelsError::Transport(
            "channel link closed before WELCOME".into(),
        ))
    };

    let welcome = tokio::time::timeout(WELCOME_TIMEOUT, wait_for_welcome)
        .await
        .map_err(|_| ChannelsError::Transport("timed out waiting for WELCOME".into()))??;

    Ok(ConnectedSession {
        session,
        destination_hash,
        hub_identity,
        announced_name,
        hops,
        nickname,
        welcome,
        buffered,
    })
}

async fn handle_connect_update(
    update: ConnectUpdate,
    current_attempt: u64,
    connect_cancel: &mut Option<oneshot::Sender<()>>,
    active: &mut Option<ActiveSession>,
    snapshot: &Arc<RwLock<ChannelsSnapshot>>,
    emitter: &Arc<dyn Emitter>,
    source: [u8; 16],
) {
    let update_attempt = match &update {
        ConnectUpdate::Discovered { attempt, .. }
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
        ConnectUpdate::Discovered {
            hub_identity,
            announced_name,
            hops,
            ..
        } => {
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
        ConnectUpdate::Failed { error, .. } => {
            *connect_cancel = None;
            mutate_snapshot(snapshot, |state| {
                state.phase = ChannelsPhase::Error;
                state.rooms.clear();
                state.notices.clear();
                state.last_error = Some(error.to_string());
            });
        }
        ConnectUpdate::Ready { connected, .. } => {
            *connect_cancel = None;
            let ConnectedSession {
                session,
                destination_hash,
                hub_identity,
                announced_name,
                hops,
                nickname,
                welcome,
                buffered,
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
                notices: VecDeque::new(),
                seen_ids: HashSet::new(),
                seen_order: VecDeque::new(),
            };
            for envelope in buffered {
                let _ = handle_envelope(&mut live, envelope).await;
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
    room: String,
    key: Option<String>,
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
            }
        }
    }
    if active
        .limits
        .max_rooms_per_session
        .is_some_and(|limit| active.rooms.len() >= limit)
    {
        return Err(ChannelsError::RoomLimitReached);
    }

    active
        .rooms
        .insert(room.clone(), ChannelRoomSnapshot::joining(room.clone()));
    let mut envelope =
        Envelope::room_command(MessageType::Join, active.source, &room, &active.nickname);
    if let Some(key) = key.filter(|key| !key.is_empty()) {
        envelope.body = Some(Value::Text(key));
    }
    if let Err(error) = send_envelope(&active.handle, &envelope).await {
        active.rooms.remove(&room);
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
    room: String,
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
    if let Some(room_state) = active.rooms.get_mut(&room) {
        room_state.phase = ChannelRoomPhase::Parting;
        room_state.last_error = None;
    }
    let envelope =
        Envelope::room_command(MessageType::Part, active.source, &room, &active.nickname);
    if let Err(error) = send_envelope(&active.handle, &envelope).await {
        if let Some(room_state) = active.rooms.get_mut(&room) {
            room_state.phase = prior;
        }
        return Err(error);
    }
    sync_session_snapshot(active, snapshot);
    emit_snapshot(emitter, snapshot);
    Ok(())
}

async fn send_room_text(
    active: Option<&mut ActiveSession>,
    room: String,
    text: String,
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
    send_envelope(&active.handle, &envelope).await.map(|_| ())
}

enum LinkEventOutcome {
    Keep,
    Stale,
    Recovered,
    Closed(String),
}

async fn handle_link_event(
    active: &mut ActiveSession,
    event: LinkSessionEvent,
) -> LinkEventOutcome {
    match event {
        LinkSessionEvent::Packet { data, .. } => match rrc::decode(&data) {
            Ok(envelope) => {
                if handle_envelope(active, envelope).await {
                    LinkEventOutcome::Keep
                } else {
                    LinkEventOutcome::Closed("Channel link send failed".into())
                }
            }
            Err(error) => {
                tracing::debug!(error = %error, "ignoring malformed channel envelope");
                LinkEventOutcome::Keep
            }
        },
        LinkSessionEvent::PacketDelivered { .. } => LinkEventOutcome::Keep,
        LinkSessionEvent::Stale => LinkEventOutcome::Stale,
        LinkSessionEvent::Recovered => LinkEventOutcome::Recovered,
        LinkSessionEvent::Closed { reason } => {
            tracing::info!(reason = close_reason_label(reason), "channel Link closed");
            LinkEventOutcome::Closed(format!(
                "Channel hub disconnected ({})",
                close_reason_label(reason)
            ))
        }
    }
}

async fn handle_envelope(active: &mut ActiveSession, envelope: Envelope) -> bool {
    if matches!(
        envelope.message_type,
        MessageType::Welcome
            | MessageType::Joined
            | MessageType::Parted
            | MessageType::Ping
            | MessageType::Error
    ) && envelope.source != active.hub_identity
    {
        tracing::debug!(
            message_type = ?envelope.message_type,
            source = %hex::encode(envelope.source),
            "ignoring channel control envelope not authored by the authenticated hub"
        );
        return true;
    }
    if envelope.message_type == MessageType::Ping {
        let pong = Envelope::pong(active.source, &envelope);
        if send_envelope(&active.handle, &pong).await.is_err() {
            return false;
        }
    }
    if !active.remember(envelope.message_id) {
        return true;
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
        MessageType::Joined => apply_joined(active, &envelope),
        MessageType::Part => {}
        MessageType::Parted => apply_parted(active, &envelope),
        MessageType::Message | MessageType::Notice | MessageType::Action => {
            append_content(active, &envelope)
        }
        MessageType::Error => append_error(active, &envelope),
        MessageType::ResourceEnvelope | MessageType::Unknown(_) => {}
    }
    true
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
    let joining_self = room.phase == ChannelRoomPhase::Joining;
    room.phase = ChannelRoomPhase::Joined;
    room.last_error = None;

    let identities = rrc::member_identities(envelope);
    if !identities.is_empty() {
        if joining_self {
            room.members.clear();
        }
        for identity in identities {
            upsert_member(
                &mut room.members,
                Some(identity),
                if identity == active.source {
                    Some(active.nickname.clone())
                } else {
                    envelope.nickname.clone()
                },
                identity == active.source,
            );
        }
        room.members_complete = joining_self;
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

fn append_content(active: &mut ActiveSession, envelope: &Envelope) {
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
        append_room_item(room, item);
    } else if envelope.message_type == MessageType::Notice {
        append_bounded(&mut active.notices, item, NOTICE_LIMIT);
    }
}

fn append_error(active: &mut ActiveSession, envelope: &Envelope) {
    let text = rrc::text_body(envelope).unwrap_or("Channel hub reported an error");
    let item = transcript_item(
        envelope,
        ChannelItemKind::Error,
        None,
        text.to_string(),
        false,
    );
    if let Some(room_name) = envelope.room.as_deref()
        && let Some(room) = active.rooms.get_mut(room_name)
    {
        if room.phase == ChannelRoomPhase::Joining {
            room.phase = ChannelRoomPhase::Error;
            room.last_error = Some(text.to_string());
        } else if room.phase == ChannelRoomPhase::Parting {
            room.phase = ChannelRoomPhase::Joined;
            room.last_error = Some(text.to_string());
        }
        append_room_item(room, item);
    } else {
        append_bounded(&mut active.notices, item, NOTICE_LIMIT);
    }
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
) {
    let identity_hash = identity.map(hex::encode);
    let existing_index = if let Some(hash) = identity_hash.as_deref() {
        members
            .iter()
            .position(|member| member.identity_hash.as_deref() == Some(hash))
    } else {
        nickname.as_deref().and_then(|nick| {
            members.iter().position(|member| {
                member.identity_hash.is_none() && member.nickname.as_deref() == Some(nick)
            })
        })
    };
    if let Some(existing) = existing_index.and_then(|index| members.get_mut(index)) {
        if nickname.is_some() {
            existing.nickname = nickname;
        }
        existing.is_self |= is_self;
    } else {
        members.push(ChannelMemberSnapshot {
            identity_hash,
            nickname,
            is_self,
        });
    }
}

async fn send_envelope(
    handle: &rns_runtime::link_session::LinkSessionHandle,
    envelope: &Envelope,
) -> Result<rns_runtime::link_session::LinkSessionPacketReceipt, ChannelsError> {
    Ok(handle.send_packet(rrc::encode(envelope)?).await?)
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
        Err(error) => tracing::warn!(error = %error, "failed to serialize Channels snapshot"),
    }
}

fn cancel_connection(cancel: &mut Option<oneshot::Sender<()>>) {
    if let Some(cancel) = cancel.take() {
        let _ = cancel.send(());
    }
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

        let lrrtt = next_outbound(&mut transport_rx).await;
        let (lrrtt_header, lrrtt_offset) =
            rns_wire::header::PacketHeader::unpack(&lrrtt.raw).unwrap();
        assert_eq!(
            lrrtt_header.context,
            rns_wire::context::PacketContext::Lrrtt
        );
        responder
            .receive_rtt_packet(&lrrtt.raw[lrrtt_offset..])
            .unwrap();

        let identify = next_outbound(&mut transport_rx).await;
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

    async fn next_outbound_with_context(
        rx: &mut mpsc::Receiver<TransportMessage>,
        wanted: rns_wire::context::PacketContext,
    ) -> OutboundRequest {
        loop {
            let request = next_outbound(rx).await;
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
            let request = next_outbound(rx).await;
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
