//! RRC hub service: hosts a Reticulum Relay Chat hub (`rrc.hub`) so remote
//! clients can connect, join rooms, and relay through this node.
//!
//! Relay state is live-only. Nothing in this module writes channel traffic to
//! the Ratspeak database; persisted hub state is limited to operator settings
//! and, later, the registered-room registry. Protocol behavior follows the
//! reference daemon (kc1awv/rrcd 0.3.2) except where the fix registry records
//! a deliberate deviation (idempotent re-HELLO is one: a duplicate HELLO
//! re-welcomes without wiping room membership).

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

use crate::rrc;
use crate::state::AppState;
use ratspeak_core::Emitter;

const COMMAND_BUFFER: usize = 16;
/// Client HELLO retries fire at 3s; WELCOME must beat the first retry.
const GREETING_CHUNK_BYTES: usize = 300;
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
        }
    }
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
}

enum HubCommand {
    Status {
        result_tx: oneshot::Sender<ChannelHubSnapshot>,
    },
    Shutdown {
        result_tx: oneshot::Sender<()>,
    },
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
}

impl HubSession {
    fn new(now: Instant) -> Self {
        Self {
            identity: None,
            welcomed: false,
            nickname: None,
            capabilities: BTreeMap::new(),
            rooms: HashSet::new(),
            established_at: now,
            awaiting_pong_since: None,
        }
    }
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
}

/// Transport-free hub protocol core: every inbound event mutates state and
/// appends outbound work, so behavior is unit-testable exactly like the
/// reference router.
pub(crate) struct HubCore {
    config: ChannelHubConfig,
    hub_hash: [u8; 16],
    sessions: HashMap<[u8; 16], HubSession>,
    klines: Arc<RwLock<HashSet<[u8; 16]>>>,
}

impl HubCore {
    pub(crate) fn new(
        config: ChannelHubConfig,
        hub_hash: [u8; 16],
        klines: Arc<RwLock<HashSet<[u8; 16]>>>,
    ) -> Self {
        Self {
            config,
            hub_hash,
            sessions: HashMap::new(),
            klines,
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
        self.sessions
            .entry(link_id)
            .or_insert_with(|| HubSession::new(now));
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
        let session = self
            .sessions
            .entry(link_id)
            .or_insert_with(|| HubSession::new(now));
        session.identity = Some(identity);
    }

    pub(crate) fn on_link_closed(&mut self, link_id: [u8; 16]) {
        self.sessions.remove(&link_id);
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
            }
            rrc::MessageType::Hello => self.on_hello(link_id, envelope, out),
            _ if !self.sessions.get(&link_id).is_some_and(|s| s.welcomed) => {
                out.push(self.hub_error(link_id, "send HELLO first", None));
            }
            // Room and relay traffic lands in the next phase; unknown and
            // not-yet-implemented types stay silently forward-compatible.
            _ => {}
        }
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
        let first_welcome = !session.welcomed;
        session.welcomed = true;

        let mut welcome = rrc::Envelope::new(rrc::MessageType::Welcome, self.hub_hash);
        welcome.body = Some(welcome_body);
        out.push(HubSend::Envelope {
            link_id,
            envelope: welcome,
        });

        if first_welcome && let Some(greeting) = self.config.greeting.clone() {
            for chunk in chunk_text(&greeting, GREETING_CHUNK_BYTES) {
                out.push(self.hub_notice(link_id, &chunk, None));
            }
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
    identity.to_file(path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
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
    pub async fn start(
        transport_tx: mpsc::Sender<TransportMessage>,
        hub_identity: Identity,
        config: ChannelHubConfig,
        emitter: Arc<dyn Emitter>,
        shutdown: ShutdownSignal,
        _state: Weak<AppState>,
    ) -> Result<Self, ChannelHubError> {
        let klines: Arc<RwLock<HashSet<[u8; 16]>>> = Arc::new(RwLock::new(HashSet::new()));
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
        let core = HubCore::new(config.clone(), hub_identity.hash, klines);
        tokio::spawn(run_hub(
            registration,
            core,
            config,
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
            let _ = tokio::time::timeout(Duration::from_secs(2), result_rx).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_hub(
    mut registration: RegisteredDestination,
    mut core: HubCore,
    config: ChannelHubConfig,
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
                    Some((data, link_id)) => match rrc::decode(&data) {
                        Ok(envelope) => core.on_envelope(link_id, envelope, &mut out),
                        Err(_) => {
                            // Reference replies with the decode error text; we
                            // keep the reply static so nothing inbound echoes.
                            out.push(HubSend::Envelope {
                                link_id,
                                envelope: bad_message_error(&core),
                            });
                        }
                    },
                    None => break,
                }
            }
            closed = registration.events.links_closed.recv() => {
                if let Some(link_id) = closed {
                    core.on_link_closed(link_id);
                }
            }
            _ = ping_tick.tick() => core.ping_cycle(Instant::now(), &mut out),
            _ = announce_tick.tick() => {
                if config.announce_interval_secs > 0
                    && registration.handle.announce(announce_options()).await.is_err()
                {
                    tracing::warn!(reason = "announce_failed", "periodic hub announce failed");
                }
            }
        }
        flush_sends(&registration, out).await;
        publish_snapshot(&snapshot, &emitter, &core, &config, Some(destination_hash));
    }

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

fn bad_message_error(core: &HubCore) -> rrc::Envelope {
    let mut envelope = rrc::Envelope::new(rrc::MessageType::Error, core.hub_hash);
    envelope.body = Some(Value::Text("bad message: invalid envelope".into()));
    envelope
}

async fn flush_sends(registration: &RegisteredDestination, sends: Vec<HubSend>) {
    for send in sends {
        match send {
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
                    tracing::warn!(
                        len = encoded.len(),
                        reason = "over_mdu",
                        "hub envelope dropped"
                    );
                }
                Err(_) => {
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
        rooms: 0,
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

    fn core_with(config: ChannelHubConfig) -> HubCore {
        HubCore::new(config, [0x77; 16], Arc::new(RwLock::new(HashSet::new())))
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
}
