//! Shared application state. Narrowest sync primitive per field: `RwLock` for
//! read-heavy caches, `Mutex` for write-heavy maps, `AtomicBool` for single flags.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use ratspeak_core::{Emitter, NativeNotification, NativeNotifier};
use rns_runtime::lifecycle::ShutdownSignal;
use tokio::sync::watch;

use crate::activity::ActivityRecorder;
use crate::activity::emitter::EmitterBatchSink;
use crate::channels::ChannelsManagerHandle;
use crate::config::DashboardConfig;
use crate::lxmf::LxmfManager;
use crate::rns::RnsManager;

pub use ratspeak_core::types::{
    LrgpMsgMeta, MAX_DISCOVERED_PROPAGATION_NODES, PROPAGATION_NODE_TTL_SECS,
};
pub use ratspeak_db::DbPool;

const INTERFACE_REANNOUNCE_SUPPRESSION_TTL: Duration = Duration::from_secs(120);
// The WebView abandons native connects at 180s. Keep a cleanup margin so its
// typed timeout callback can still consume the token under scheduler delay.
const BLE_RNODE_ACTIVITY_OPERATION_TTL: Duration = Duration::from_secs(240);
// One replacement can spend up to 180s in Android native setup plus 120s in
// protocol readiness on both the proposed and restored runtime. Fifteen minutes
// covers that ten-minute worst case with a five-minute scheduler/cleanup margin
// while still reclaiming abandoned session-local leases.
const RNODE_LIFECYCLE_OPERATION_TTL: Duration = Duration::from_secs(15 * 60);

/// Snapshot used to reject an Activity command that was queued before either
/// an identity transition or a same-identity runtime privacy reset. Callers
/// must capture this before waiting for lifecycle locks and validate it only
/// after acquiring both locks in identity-then-Activity order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivityRequestFence {
    identity_session_generation: u64,
    activity_boundary_generation: u64,
    identity_lock_epoch: u64,
}

/// Opaque ownership token for one installed RNS runtime session.
///
/// The token binds exact RNode observations to both an identity session and a
/// particular RNS manager installation, preventing interface-ID reuse across
/// a soft runtime restart from adopting an older observer.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RNodeActivityOrigin {
    identity_generation: u64,
    rns_generation: u64,
}

impl RNodeActivityOrigin {
    pub(crate) fn new(identity_generation: u64, rns_generation: u64) -> Self {
        Self {
            identity_generation,
            rns_generation,
        }
    }

    pub(crate) fn identity_generation(self) -> u64 {
        self.identity_generation
    }
}

struct BleRnodeActivityOperation {
    token: [u8; 16],
    activity_fence: ActivityRequestFence,
    lifecycle_lease: Option<RNodeLifecycleOperationLease>,
    started_at: Instant,
    phase: BleRnodeActivityOperationPhase,
    rollback_context: Option<BleRnodeRollbackContext>,
    completion: Option<BleRnodeOperationCompletionSender>,
}

struct BleRnodeRollbackContext {
    config_dir: PathBuf,
    name: String,
    marker: u64,
}

/// Closed, snapshot-free failure vocabulary for a native BLE-RNode operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleRnodeOperationFailure {
    Setup,
    Connect,
    StartupTimeout,
    Readiness,
    Runtime,
    Cancelled,
}

/// Terminal result delivered only to an internal native BLE-RNode waiter.
#[derive(Debug)]
pub enum BleRnodeOperationResult {
    Ready {
        interface_id: u64,
        monitor: Option<crate::rnode_activity::PendingRNodeActivityMonitor>,
    },
    Failed(BleRnodeOperationFailure),
}

#[derive(Clone)]
pub struct RNodeLifecycleOperationLease {
    generation: u64,
}

impl std::fmt::Debug for RNodeLifecycleOperationLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RNodeLifecycleOperationLease")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
struct RNodeLifecycleOperationEntry {
    generation: u64,
    started_at: Instant,
}

#[doc(hidden)]
pub type BleRnodeRollback = (PathBuf, String, u64);
#[doc(hidden)]
pub type BleRnodeActivityTake = (ActivityRequestFence, Option<BleRnodeRollback>);
#[doc(hidden)]
pub type BleRnodeOperationCompletionSender = tokio::sync::oneshot::Sender<BleRnodeOperationResult>;
#[doc(hidden)]
pub type BleRnodeOperationCompletionReceiver =
    tokio::sync::oneshot::Receiver<BleRnodeOperationResult>;
#[doc(hidden)]
pub type BleRnodeActivityTakeWithCompletion = (
    ActivityRequestFence,
    Option<BleRnodeRollback>,
    Option<BleRnodeOperationCompletionSender>,
);
#[doc(hidden)]
pub type BleRnodeActivityCancellation = (String, ActivityRequestFence, Option<BleRnodeRollback>);

#[derive(Clone, Copy, Eq, PartialEq)]
enum BleRnodeActivityOperationPhase {
    PendingNative,
    Initializing,
    Completing,
    Cancelling,
    CancellationCompleting,
}

impl ActivityRequestFence {
    pub fn identity_session_generation(self) -> u64 {
        self.identity_session_generation
    }
}

/// Async identity/runtime transition lock with a span epoch. The epoch is odd
/// while any holder owns the lock and advances again when that holder drops,
/// allowing Activity commands to distinguish a request born during a
/// transition from one that was merely queued before a point-in-time reset.
pub struct IdentitySwitchLock {
    inner: tokio::sync::Mutex<()>,
    epoch: AtomicU64,
}

impl IdentitySwitchLock {
    pub fn new() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(()),
            epoch: AtomicU64::new(0),
        }
    }

    pub async fn lock(&self) -> IdentitySwitchGuard<'_> {
        let guard = self.inner.lock().await;
        self.epoch.fetch_add(1, Ordering::SeqCst);
        IdentitySwitchGuard {
            _guard: guard,
            epoch: &self.epoch,
        }
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
}

impl Default for IdentitySwitchLock {
    fn default() -> Self {
        Self::new()
    }
}

pub struct IdentitySwitchGuard<'a> {
    _guard: tokio::sync::MutexGuard<'a, ()>,
    epoch: &'a AtomicU64,
}

impl Drop for IdentitySwitchGuard<'_> {
    fn drop(&mut self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

fn prune_rnode_lifecycle_operations(
    operations: &mut HashMap<String, RNodeLifecycleOperationEntry>,
    now: Instant,
) {
    operations.retain(|_, operation| {
        now.saturating_duration_since(operation.started_at) <= RNODE_LIFECYCLE_OPERATION_TTL
    });
}

fn next_rnode_lifecycle_operation_generation(counter: &AtomicU64) -> u64 {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .expect("RNode lifecycle generation exhausted");
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

fn next_rnode_activity_session_generation(counter: &AtomicU64) -> u64 {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .expect("RNode Activity session generation exhausted");
        match counter.compare_exchange_weak(current, next, Ordering::SeqCst, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

/// Uses `std::sync::{Mutex, RwLock}`, not tokio variants. Critical sections
/// must finish before `.await` or run in `spawn_blocking`
/// (`clippy::await_holding_lock` enforces this).
pub struct AppState {
    pub config: DashboardConfig,
    pub db: DbPool,
    pub startup_stage: RwLock<String>,
    /// IPC fan-out — concrete impl is `TauriEmitter` in production builds and
    /// a no-op stub in headless tests. Set at construction; never re-assigned.
    pub emitter: Arc<dyn Emitter>,
    /// Process-lived typed Activity recorder. Capture begins Off; identity and
    /// app lifecycle barriers explicitly purge its session-owned privacy state.
    pub activity: ActivityRecorder,
    /// Serializes typed and legacy Activity lifecycle commands with runtime
    /// hard-reset reconciliation. Read-only replay/detail queries use the
    /// recorder's own privacy barrier instead.
    pub activity_control_lock: tokio::sync::Mutex<()>,
    /// Invalidates WebView lifecycle requests that were queued before any
    /// acknowledged runtime/identity hard reset, including same-identity soft
    /// restarts that do not advance `identity_session_generation`.
    activity_boundary_generation: AtomicU64,
    pub notifier: Arc<dyn NativeNotifier>,
    /// Keyed by dest_hash hex; IndexMap insertion-order drives FIFO eviction.
    pub announce_history: RwLock<IndexMap<String, serde_json::Value>>,
    pub alerts: Mutex<Vec<serde_json::Value>>,
    pub rns: RwLock<Option<RnsManager>>,
    /// Process-monotonic source for opaque installed-RNS ownership tokens.
    /// Identity-scoped resets deliberately do not rewind it.
    rnode_activity_session_generation: AtomicU64,
    /// Session-scoped live Channels runtime. It is torn down before RNS so an
    /// active hub Link can send its closing packet cleanly.
    pub channels: RwLock<Option<ChannelsManagerHandle>>,
    /// Session-scoped RRC hub service. Same teardown ordering as `channels`:
    /// before RNS, so client links receive their teardown packets.
    pub channel_hub: RwLock<Option<crate::channel_hub::ChannelHubHandle>>,
    /// Serializes hub start, stop, configuration restart, and identity/runtime
    /// teardown. The handle slot alone cannot make check-then-register atomic.
    pub channel_hub_control_lock: tokio::sync::Mutex<()>,
    pub lxmf: Mutex<Option<LxmfManager>>,
    /// Public identity keys captured when a local identity is unlocked. Contact
    /// card export is read-only and must not queue behind the LXMF router lock.
    local_identity_public_keys: RwLock<HashMap<String, [u8; 64]>>,
    #[cfg(feature = "lxst-voice")]
    pub lxst_voice: Mutex<Option<crate::voice::LxstVoiceServiceHandle>>,
    #[cfg(feature = "lxst-voice")]
    pub lxst_rejected_call_attempts: Mutex<HashMap<String, (u32, Instant)>>,
    pub known_path_hashes: Mutex<std::collections::HashSet<String>>,
    /// False until the first non-empty path-table snapshot has seeded
    /// `known_path_hashes`; prevents restored paths from flooding Activity.
    pub path_activity_baselined: AtomicBool,
    pub lrgp_router: lrgp::router::LrgpRouter,
    pub message_send_times: Mutex<HashMap<String, f64>>,
    pub seen_announce_hashes: Mutex<std::collections::HashSet<String>>,
    /// False until the first non-empty announce snapshot has seeded
    /// `seen_announce_hashes`; prevents cached announces from replaying as live.
    pub announce_activity_baselined: AtomicBool,
    pub msg_id_map: Mutex<HashMap<String, String>>,
    /// LRGP msg_id → originating session for delivery-state routing.
    pub lrgp_msg_to_session: Mutex<HashMap<String, LrgpMsgMeta>>,
    pub session_shutdown: RwLock<ShutdownSignal>,
    pub is_foreground: Arc<AtomicBool>,
    /// Monotonic ticket used to discard a stale asynchronous foreground
    /// transition after a newer background/foreground edge has arrived.
    foreground_transition_generation: AtomicU64,
    /// Edge-trigger wake for long-sleeping background loops.
    pub foreground_changed: Arc<tokio::sync::Notify>,
    pub propagation_node: Mutex<Option<Arc<Mutex<lxmf_core::propagation_node::PropagationNode>>>>,
    pub last_stats: RwLock<Option<serde_json::Value>>,
    pub last_hub_interfaces: RwLock<Option<serde_json::Value>>,
    pub lxmf_notify: Arc<tokio::sync::Notify>,
    pub discovered_propagation_nodes: Mutex<HashMap<String, serde_json::Value>>,
    pub network_log_enabled: AtomicBool,
    /// One of "essential" | "standard" | "detailed".
    pub network_log_level: RwLock<String>,
    /// Auto-announce interval in seconds (0 = disabled).
    pub announce_interval_tx: watch::Sender<u64>,
    pub announce_interval_rx: watch::Receiver<u64>,
    /// If true, delivery announces include Ratspeak capability metadata.
    pub announce_ratspeak_usage: AtomicBool,
    /// Eager-wake for the stats poll loop; loop has 750ms debounce cooldown.
    pub poll_now: Arc<tokio::sync::Notify>,
    /// Live BLE-peer count, driven by `BlePeerEvent::Connected/Disconnected`.
    pub ble_peer_count: AtomicUsize,
    /// Live connected BLE-peer set (address → identity hash, empty if not yet
    /// resolved). Snapshot source so the peer rows survive a webview reload —
    /// the per-event list otherwise lives only in the relay task.
    pub ble_peers: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    /// If true, inbound LXMF without a stamp meeting `required_stamp_cost`
    /// are dropped before delivery-proof + storage.
    pub enforce_stamps: AtomicBool,
    /// 0 disables enforcement even if `enforce_stamps` is set.
    pub required_stamp_cost: AtomicU8,
    /// If true, this identity announces and serves its `lxmf.propagation` node.
    pub propagation_node_hosting_enabled: AtomicBool,
    pub propagation_node_stamp_cost: AtomicU8,
    /// Unix milliseconds when this identity last queued its LXMF delivery
    /// announce. Used to decide if a newly seen peer might not know our name.
    pub last_lxmf_delivery_announce_at_ms: AtomicU64,
    /// Session-local global throttle for announce-before-send nudges.
    pub last_opportunistic_announce_at: Mutex<Option<Instant>>,
    /// Peers currently covered by an in-flight opportunistic announce attempt.
    pub opportunistic_announce_inflight: Mutex<HashSet<String>>,
    /// One-shot interface-up re-announce suppression keyed by interface name.
    pub interface_reannounce_suppression: Mutex<HashMap<String, Instant>>,
    /// One-shot bridge from an Android native BLE-RNode request back to the
    /// operation and Activity origin that launched it. Product-side bridge
    /// admission uses only this opaque token and the identity generation;
    /// Activity capture state never gates the legitimate connect operation.
    ble_rnode_activity_operation: Mutex<Option<BleRnodeActivityOperation>>,
    /// Exact session-local ownership for asynchronous RNode add/update/resume
    /// work. Every name participating in one operation points at the same
    /// opaque generation, so a rename transaction owns both old and new names.
    /// This is deliberately independent from Activity and native BLE bridge
    /// admission; neither subsystem can grant lifecycle authority to another.
    rnode_lifecycle_operations: Mutex<HashMap<String, RNodeLifecycleOperationEntry>>,
    /// Process-monotonic source for opaque RNode lifecycle generations. Identity
    /// teardown clears active ownership but deliberately never resets this value,
    /// so a stale lease cannot become current again within this process.
    rnode_lifecycle_generation: AtomicU64,
    /// Coalesces conversation-list broadcasts; spawned task debounces 100ms.
    pub conversations_broadcast_pending: AtomicBool,
    /// 10s session-local throttle on Refresh button. `None` = never throttled.
    pub last_refresh_request_at: Mutex<Option<Instant>>,
    /// Low-rate background probing throttle for bundled Ratspeak relays.
    pub last_static_probe_at: Mutex<Option<Instant>>,
    /// In-memory mirror of the active identity's Auto-picked PN.
    pub auto_active_node: RwLock<Option<[u8; 16]>>,
    /// Per-node failure counter for the 3-strikes-within-30-min Auto drop.
    pub auto_failure_counts: Mutex<HashMap<[u8; 16], (u32, Instant)>>,
    /// Lifetime count of `lxmf.propagation` announces with unparseable app_data.
    pub pn_parse_failures: AtomicU64,
    pub native_notifications_enabled: AtomicBool,
    /// Serializes read-modify-write edits to the active Reticulum config file.
    pub rns_config_lock: Mutex<()>,
    pub identity_switch_lock: IdentitySwitchLock,
    pub ble_peer_enable_lock: tokio::sync::Mutex<()>,
    pub identity_session_generation: AtomicU64,
    /// Secret handed to the next protected-identity load (hardware PIN or
    /// software passcode, consumed by `init_rns_lxmf`). Never persisted.
    pub hw_pending_pin: Mutex<Option<String>>,
    /// Hash of a protected identity that is active but locked (awaiting PIN).
    pub hw_locked: RwLock<Option<String>>,
    /// Last protected-identity unlock failure message.
    pub hw_last_error: Mutex<Option<String>>,
    /// Bumped on every session teardown; an auto-lock timer no-ops if its captured
    /// generation no longer matches (i.e. the session was switched/unlocked/quit).
    pub hw_lock_gen: AtomicU64,
    /// Read-through cache for the active identity's (hash, lxmf_hash),
    /// stamped with `db::identity_generation()` so identity-table writes
    /// invalidate it. Keeps hot async paths off sync DB reads.
    pub active_identity_cache: Mutex<Option<CachedActiveIdentity>>,
}

/// (identity-table generation, active identity (hash, lxmf_hash) at that
/// generation — `None` when no identity is active).
pub type CachedActiveIdentity = (u64, Option<(String, String)>);

impl AppState {
    pub fn new(
        config: DashboardConfig,
        db: DbPool,
        emitter: Arc<dyn Emitter>,
        notifier: Arc<dyn NativeNotifier>,
    ) -> Self {
        let lrgp_router = lrgp::router::LrgpRouter::new();
        lrgp_router.register(Box::new(lrgp::apps::tictactoe::TicTacToeApp::new()));
        lrgp_router.register(Box::new(lrgp::apps::chess::ChessApp::new()));
        for session in crate::db::load_game_sessions(&db) {
            let app_id = session.app_id.clone();
            let session_id = session.session_id.clone();
            let identity_id = session.identity_id.clone();
            if lrgp_router.restore_session(session).is_err() {
                tracing::warn!(
                    reason = "invalid_durable_session",
                    "Skipping invalid durable LRGP session during hydration"
                );
                continue;
            }

            // `restore_session` applies the app's TTL policy. Mirror an expiry
            // transition immediately so the UI and live engine cannot diverge
            // after a restart.
            if let Some(Some(restored)) = lrgp_router.with_app(&app_id, |app| {
                app.get_session_record(&session_id, &identity_id)
            }) {
                crate::db::save_game_session(&db, &restored);
            }
        }

        let initial_interval = crate::db::get_setting(&db, "auto_announce_interval")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1800);
        let (announce_interval_tx, announce_interval_rx) = watch::channel(initial_interval);
        let initial_announce_ratspeak_usage =
            crate::db::get_setting(&db, "announce_ratspeak_usage")
                .and_then(|v| v.parse::<u8>().ok())
                .map(|v| v != 0)
                .unwrap_or(true);

        let initial_enforce_stamps = crate::db::get_setting(&db, "enforce_stamps")
            .and_then(|v| v.parse::<u8>().ok())
            .map(|v| v != 0)
            .unwrap_or(false);
        let initial_required_stamp_cost = crate::db::get_setting(&db, "required_stamp_cost")
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(0);
        let initial_prop_node_hosting =
            crate::db::get_setting(&db, "propagation_node_hosting_enabled")
                .and_then(|v| v.parse::<u8>().ok())
                .map(|v| v != 0)
                .unwrap_or(false);
        let initial_prop_node_stamp_cost =
            crate::db::get_setting(&db, "propagation_node_stamp_cost")
                .and_then(|v| v.parse::<u8>().ok())
                .unwrap_or(16);
        let initial_notifications_enabled =
            crate::db::get_setting(&db, "native_notifications_enabled")
                .or_else(|| crate::db::get_setting(&db, "desktop_notifications_enabled"))
                .and_then(|v| v.parse::<u8>().ok())
                .map(|v| v != 0)
                .unwrap_or(true);
        let activity = ActivityRecorder::with_batch_sink(Arc::new(EmitterBatchSink::new(
            Arc::clone(&emitter),
        )));

        Self {
            config,
            db,
            startup_stage: RwLock::new("starting".into()),
            emitter,
            activity,
            activity_control_lock: tokio::sync::Mutex::new(()),
            activity_boundary_generation: AtomicU64::new(0),
            notifier,
            announce_history: RwLock::new(IndexMap::new()),
            alerts: Mutex::new(Vec::new()),
            rns: RwLock::new(None),
            rnode_activity_session_generation: AtomicU64::new(0),
            channels: RwLock::new(None),
            channel_hub: RwLock::new(None),
            channel_hub_control_lock: tokio::sync::Mutex::new(()),
            lxmf: Mutex::new(None),
            local_identity_public_keys: RwLock::new(HashMap::new()),
            #[cfg(feature = "lxst-voice")]
            lxst_voice: Mutex::new(None),
            #[cfg(feature = "lxst-voice")]
            lxst_rejected_call_attempts: Mutex::new(HashMap::new()),
            known_path_hashes: Mutex::new(std::collections::HashSet::new()),
            path_activity_baselined: AtomicBool::new(false),
            lrgp_router,
            message_send_times: Mutex::new(HashMap::new()),
            seen_announce_hashes: Mutex::new(std::collections::HashSet::new()),
            announce_activity_baselined: AtomicBool::new(false),
            msg_id_map: Mutex::new(HashMap::new()),
            lrgp_msg_to_session: Mutex::new(HashMap::new()),
            session_shutdown: RwLock::new(ShutdownSignal::new()),
            is_foreground: Arc::new(AtomicBool::new(true)),
            foreground_transition_generation: AtomicU64::new(0),
            foreground_changed: Arc::new(tokio::sync::Notify::new()),
            propagation_node: Mutex::new(None),
            last_stats: RwLock::new(None),
            last_hub_interfaces: RwLock::new(None),
            lxmf_notify: Arc::new(tokio::sync::Notify::new()),
            discovered_propagation_nodes: Mutex::new(HashMap::new()),
            network_log_enabled: AtomicBool::new(false),
            network_log_level: RwLock::new("standard".into()),
            announce_interval_tx,
            announce_interval_rx,
            announce_ratspeak_usage: AtomicBool::new(initial_announce_ratspeak_usage),
            poll_now: Arc::new(tokio::sync::Notify::new()),
            ble_peer_count: AtomicUsize::new(0),
            ble_peers: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            enforce_stamps: AtomicBool::new(initial_enforce_stamps),
            required_stamp_cost: AtomicU8::new(initial_required_stamp_cost),
            propagation_node_hosting_enabled: AtomicBool::new(initial_prop_node_hosting),
            propagation_node_stamp_cost: AtomicU8::new(initial_prop_node_stamp_cost),
            last_lxmf_delivery_announce_at_ms: AtomicU64::new(0),
            last_opportunistic_announce_at: Mutex::new(None),
            opportunistic_announce_inflight: Mutex::new(HashSet::new()),
            interface_reannounce_suppression: Mutex::new(HashMap::new()),
            ble_rnode_activity_operation: Mutex::new(None),
            rnode_lifecycle_operations: Mutex::new(HashMap::new()),
            rnode_lifecycle_generation: AtomicU64::new(0),
            conversations_broadcast_pending: AtomicBool::new(false),
            last_refresh_request_at: Mutex::new(None),
            last_static_probe_at: Mutex::new(None),
            auto_active_node: RwLock::new(None),
            auto_failure_counts: Mutex::new(HashMap::new()),
            pn_parse_failures: AtomicU64::new(0),
            native_notifications_enabled: AtomicBool::new(initial_notifications_enabled),
            rns_config_lock: Mutex::new(()),
            identity_switch_lock: IdentitySwitchLock::new(),
            ble_peer_enable_lock: tokio::sync::Mutex::new(()),
            identity_session_generation: AtomicU64::new(0),
            hw_pending_pin: Mutex::new(None),
            hw_locked: RwLock::new(None),
            hw_last_error: Mutex::new(None),
            hw_lock_gen: AtomicU64::new(0),
            active_identity_cache: Mutex::new(None),
        }
    }

    /// Take the PIN staged for the next hardware-identity load (one-shot).
    pub fn take_pending_hw_pin(&self) -> Option<String> {
        self.hw_pending_pin.lock().ok().and_then(|mut p| p.take())
    }

    pub fn set_pending_hw_pin(&self, pin: Option<String>) {
        if let Ok(mut p) = self.hw_pending_pin.lock() {
            *p = pin;
        }
    }

    pub fn set_hw_locked(&self, hash: Option<String>) {
        if let Ok(mut h) = self.hw_locked.write() {
            *h = hash;
        }
    }

    pub fn hw_locked_hash(&self) -> Option<String> {
        self.hw_locked.read().ok().and_then(|h| h.clone())
    }

    pub fn set_hw_last_error(&self, e: Option<String>) {
        if let Ok(mut x) = self.hw_last_error.lock() {
            *x = e;
        }
    }

    pub fn take_hw_last_error(&self) -> Option<String> {
        self.hw_last_error.lock().ok().and_then(|mut x| x.take())
    }

    pub fn request_poll_now(&self) {
        self.poll_now.notify_one();
    }

    pub fn is_foreground(&self) -> bool {
        self.is_foreground.load(Ordering::Relaxed)
    }

    pub fn begin_foreground_transition(&self) -> u64 {
        self.foreground_transition_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    pub fn is_current_foreground_transition(&self, generation: u64) -> bool {
        self.foreground_transition_generation
            .load(Ordering::Acquire)
            == generation
    }

    pub fn native_notifications_enabled(&self) -> bool {
        self.native_notifications_enabled.load(Ordering::Relaxed)
    }

    pub fn set_native_notifications_enabled(&self, enabled: bool) {
        self.native_notifications_enabled
            .store(enabled, Ordering::Relaxed);
    }

    pub fn announce_ratspeak_usage_enabled(&self) -> bool {
        self.announce_ratspeak_usage.load(Ordering::Relaxed)
    }

    pub fn set_announce_ratspeak_usage_enabled(&self, enabled: bool) {
        self.announce_ratspeak_usage
            .store(enabled, Ordering::Relaxed);
    }

    pub fn bump_identity_session_generation(&self) -> u64 {
        self.identity_session_generation
            .fetch_add(1, Ordering::SeqCst)
            + 1
    }

    pub fn current_identity_session_generation(&self) -> u64 {
        self.identity_session_generation.load(Ordering::SeqCst)
    }

    pub fn current_activity_boundary_generation(&self) -> u64 {
        self.activity_boundary_generation.load(Ordering::SeqCst)
    }

    pub fn activity_request_fence(&self) -> ActivityRequestFence {
        self.activity_request_fence_after_epoch(|| {})
    }

    fn activity_request_fence_after_epoch(
        &self,
        after_epoch: impl FnOnce(),
    ) -> ActivityRequestFence {
        // The lock epoch is the temporal linearization point and must be read
        // first. If any lock span begins or ends after this sample, validation
        // observes more than the request's own single acquisition and rejects
        // the work. Loading it last would allow a snapshot to straddle a
        // transition release while retaining the transition's new generations.
        let identity_lock_epoch = self.identity_switch_lock.epoch();
        after_epoch();
        let identity_session_generation = self.current_identity_session_generation();
        let activity_boundary_generation = self.current_activity_boundary_generation();
        ActivityRequestFence {
            identity_session_generation,
            activity_boundary_generation,
            identity_lock_epoch,
        }
    }

    /// Validate only while the caller owns `identity_switch_lock`. A request
    /// born while another transition held the lock observes at least a release
    /// plus this acquisition and therefore cannot match the single expected
    /// epoch advance.
    pub fn is_current_activity_request_fence_after_identity_lock(
        &self,
        fence: ActivityRequestFence,
    ) -> bool {
        self.current_identity_session_generation() == fence.identity_session_generation
            && self.current_activity_boundary_generation() == fence.activity_boundary_generation
            && fence
                .identity_lock_epoch
                .checked_add(1)
                .is_some_and(|expected| self.identity_switch_lock.epoch() == expected)
    }

    /// Lock-free completion fence for async diagnostic producers. Capture the
    /// fence before starting an operation and pass this validation through
    /// `ActivityRecorder::record_event_fenced`. The recorder invokes it only
    /// after acquiring an admission lease, so a transition either makes the
    /// origin stale first or waits for/purges the already-admitted draft.
    pub fn is_current_activity_origin_fence(&self, fence: ActivityRequestFence) -> bool {
        let current_epoch = self.identity_switch_lock.epoch();
        fence.identity_lock_epoch.is_multiple_of(2)
            && current_epoch == fence.identity_lock_epoch
            && self.current_identity_session_generation() == fence.identity_session_generation
            && self.current_activity_boundary_generation() == fence.activity_boundary_generation
    }

    pub fn bump_activity_boundary_generation(&self) -> u64 {
        self.activity_boundary_generation
            .fetch_add(1, Ordering::SeqCst)
            + 1
    }

    pub fn suppress_next_interface_reannounce(&self, name: &str) {
        if name.is_empty() {
            return;
        }
        if let Ok(mut suppressions) = self.interface_reannounce_suppression.lock() {
            suppressions.insert(name.to_string(), Instant::now());
        }
    }

    pub fn take_interface_reannounce_suppression(&self, name: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        let now = Instant::now();
        let Ok(mut suppressions) = self.interface_reannounce_suppression.lock() else {
            return false;
        };
        suppressions.retain(|_, marked| {
            now.duration_since(*marked) <= INTERFACE_REANNOUNCE_SUPPRESSION_TTL
        });
        suppressions.remove(name).is_some()
    }

    /// Begin one exact session-local RNode lifecycle operation.
    ///
    /// A rename should pass both its old and proposed names. Starting work for
    /// any name atomically invalidates every name owned by each overlapping
    /// older operation. The returned lease keeps its process-monotonic
    /// generation private and grants authority only through the methods below.
    pub fn begin_rnode_lifecycle_operation<I, S>(
        &self,
        names: I,
    ) -> Option<RNodeLifecycleOperationLease>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut owned_names = HashSet::new();
        for name in names {
            let name = name.as_ref();
            if name.is_empty() {
                return None;
            }
            owned_names.insert(name.to_string());
        }
        if owned_names.is_empty() {
            return None;
        }

        let now = Instant::now();
        let mut operations = self
            .rnode_lifecycle_operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_rnode_lifecycle_operations(&mut operations, now);

        let displaced = owned_names
            .iter()
            .filter_map(|name| operations.get(name).map(|operation| operation.generation))
            .collect::<HashSet<_>>();
        if !displaced.is_empty() {
            operations.retain(|_, operation| !displaced.contains(&operation.generation));
        }

        let generation =
            next_rnode_lifecycle_operation_generation(&self.rnode_lifecycle_generation);
        let operation = RNodeLifecycleOperationEntry {
            generation,
            started_at: now,
        };
        for name in owned_names {
            operations.insert(name, operation);
        }
        Some(RNodeLifecycleOperationLease { generation })
    }

    /// Return whether this exact operation still owns all of its names.
    pub fn is_current_rnode_lifecycle_operation(
        &self,
        lease: &RNodeLifecycleOperationLease,
    ) -> bool {
        let now = Instant::now();
        let mut operations = self
            .rnode_lifecycle_operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_rnode_lifecycle_operations(&mut operations, now);
        operations
            .values()
            .any(|operation| operation.generation == lease.generation)
    }

    /// Finish only the operation represented by `lease`.
    ///
    /// A stale completion cannot remove a replacement operation even when it
    /// owns one or more of the same names.
    pub fn finish_rnode_lifecycle_operation(&self, lease: &RNodeLifecycleOperationLease) -> bool {
        let now = Instant::now();
        let mut operations = self
            .rnode_lifecycle_operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_rnode_lifecycle_operations(&mut operations, now);
        let before = operations.len();
        operations.retain(|_, operation| operation.generation != lease.generation);
        operations.len() != before
    }

    /// Invalidate every active lifecycle operation owning any supplied name.
    ///
    /// Pause and remove flows use this before tearing down a runtime. The
    /// return value is the number of distinct operations invalidated.
    pub fn invalidate_rnode_lifecycle_operations_for_names<I, S>(&self, names: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let now = Instant::now();
        let mut operations = self
            .rnode_lifecycle_operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_rnode_lifecycle_operations(&mut operations, now);

        let invalidated = names
            .into_iter()
            .filter_map(|name| {
                operations
                    .get(name.as_ref())
                    .map(|operation| operation.generation)
            })
            .collect::<HashSet<_>>();
        if !invalidated.is_empty() {
            operations.retain(|_, operation| !invalidated.contains(&operation.generation));
        }
        invalidated.len()
    }

    pub fn begin_ble_rnode_activity_operation(
        &self,
        fence: ActivityRequestFence,
        rollback_context: Option<BleRnodeRollback>,
    ) -> String {
        self.begin_ble_rnode_activity_operation_inner(fence, rollback_context, None, None)
    }

    /// Begin a native BLE operation coupled to one exact RNode lifecycle
    /// generation. Superseding the lifecycle operation also revokes bridge
    /// phase transitions before they can publish a stale ready result.
    pub fn begin_ble_rnode_activity_operation_owned(
        &self,
        fence: ActivityRequestFence,
        rollback_context: Option<BleRnodeRollback>,
        lifecycle_lease: &RNodeLifecycleOperationLease,
    ) -> String {
        self.begin_ble_rnode_activity_operation_inner(
            fence,
            rollback_context,
            None,
            Some(lifecycle_lease.clone()),
        )
    }

    /// Begin a native BLE-RNode operation whose terminal protocol result can
    /// be awaited by an internal update or resume transaction.
    pub fn begin_ble_rnode_activity_operation_with_completion(
        &self,
        fence: ActivityRequestFence,
        rollback_context: Option<BleRnodeRollback>,
    ) -> (String, BleRnodeOperationCompletionReceiver) {
        self.begin_ble_rnode_activity_operation_with_completion_inner(fence, rollback_context, None)
    }

    pub fn begin_ble_rnode_activity_operation_with_completion_owned(
        &self,
        fence: ActivityRequestFence,
        rollback_context: Option<BleRnodeRollback>,
        lifecycle_lease: &RNodeLifecycleOperationLease,
    ) -> (String, BleRnodeOperationCompletionReceiver) {
        self.begin_ble_rnode_activity_operation_with_completion_inner(
            fence,
            rollback_context,
            Some(lifecycle_lease.clone()),
        )
    }

    fn begin_ble_rnode_activity_operation_with_completion_inner(
        &self,
        fence: ActivityRequestFence,
        rollback_context: Option<BleRnodeRollback>,
        lifecycle_lease: Option<RNodeLifecycleOperationLease>,
    ) -> (String, BleRnodeOperationCompletionReceiver) {
        let (completion, receiver) = tokio::sync::oneshot::channel();
        (
            self.begin_ble_rnode_activity_operation_inner(
                fence,
                rollback_context,
                Some(completion),
                lifecycle_lease,
            ),
            receiver,
        )
    }

    fn begin_ble_rnode_activity_operation_inner(
        &self,
        fence: ActivityRequestFence,
        rollback_context: Option<BleRnodeRollback>,
        completion: Option<BleRnodeOperationCompletionSender>,
        lifecycle_lease: Option<RNodeLifecycleOperationLease>,
    ) -> String {
        let mut token = rns_crypto::random::random_16();
        let lifecycle_generation = lifecycle_lease.as_ref().map(|lease| lease.generation);
        let mut pending = self
            .ble_rnode_activity_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending
            .as_ref()
            .is_some_and(|previous| previous.token == token)
        {
            token[0] ^= 1;
        }
        let displaced = pending.replace(BleRnodeActivityOperation {
            token,
            activity_fence: fence,
            lifecycle_lease,
            started_at: Instant::now(),
            phase: BleRnodeActivityOperationPhase::PendingNative,
            rollback_context: rollback_context.map(|(config_dir, name, marker)| {
                BleRnodeRollbackContext {
                    config_dir,
                    name,
                    marker,
                }
            }),
            completion,
        });
        drop(pending);

        let displaced_lease = displaced
            .as_ref()
            .and_then(|operation| operation.lifecycle_lease.clone());
        if let Some(displaced_lease) = displaced_lease
            && Some(displaced_lease.generation) != lifecycle_generation
        {
            // Native BLE setup is process-global. Replacing its token also
            // revokes a distinct product transaction before dropping its
            // completion sender. The displaced caller therefore cannot wake,
            // still observe ownership, and roll back over the new operation.
            let _ = self.finish_rnode_lifecycle_operation(&displaced_lease);
        }
        drop(displaced);
        hex::encode(token)
    }

    fn ble_rnode_lifecycle_is_current(&self, operation: &BleRnodeActivityOperation) -> bool {
        operation
            .lifecycle_lease
            .as_ref()
            .is_none_or(|lease| self.is_current_rnode_lifecycle_operation(lease))
    }

    pub fn is_current_ble_rnode_activity_operation(&self, token_hex: &str) -> bool {
        let Ok(token) = hex::decode(token_hex).and_then(|bytes| {
            <[u8; 16]>::try_from(bytes).map_err(|_| hex::FromHexError::InvalidStringLength)
        }) else {
            return false;
        };
        let mut pending = self
            .ble_rnode_activity_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.as_ref().is_some_and(|operation| {
            operation.started_at.elapsed() > BLE_RNODE_ACTIVITY_OPERATION_TTL
        }) {
            *pending = None;
            return false;
        }
        pending.as_ref().is_some_and(|operation| {
            operation.token == token
                && self.ble_rnode_lifecycle_is_current(operation)
                && matches!(
                    operation.phase,
                    BleRnodeActivityOperationPhase::PendingNative
                        | BleRnodeActivityOperationPhase::Initializing
                        | BleRnodeActivityOperationPhase::Completing
                )
        })
    }

    /// Atomically turns the current BLE-RNode operation into a cancellation
    /// lease. A replacement operation overwrites this lease under the same
    /// mutex, so delayed teardown can prove that it still belongs to the
    /// operation the user cancelled.
    pub fn begin_ble_rnode_activity_cancellation(&self) -> Option<BleRnodeActivityCancellation> {
        let mut pending = self
            .ble_rnode_activity_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.as_ref().is_some_and(|operation| {
            operation.started_at.elapsed() > BLE_RNODE_ACTIVITY_OPERATION_TTL
        }) {
            *pending = None;
            return None;
        }
        let operation = pending.as_mut()?;
        if matches!(
            operation.phase,
            BleRnodeActivityOperationPhase::Cancelling
                | BleRnodeActivityOperationPhase::CancellationCompleting
        ) {
            return None;
        }
        operation.phase = BleRnodeActivityOperationPhase::Cancelling;
        operation.started_at = Instant::now();
        let rollback_context = operation
            .rollback_context
            .take()
            .map(|context| (context.config_dir, context.name, context.marker));
        Some((
            hex::encode(operation.token),
            operation.activity_fence,
            rollback_context,
        ))
    }

    /// Claims a cancellation only after the caller has captured the exact
    /// runtime interface id it intends to tear down. If another operation has
    /// started in the meantime, its replacement of the lease makes this fail.
    pub fn claim_ble_rnode_activity_cancellation(&self, token_hex: &str) -> bool {
        let Ok(token) = hex::decode(token_hex).and_then(|bytes| {
            <[u8; 16]>::try_from(bytes).map_err(|_| hex::FromHexError::InvalidStringLength)
        }) else {
            return false;
        };
        let mut pending = self
            .ble_rnode_activity_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.as_ref().is_some_and(|operation| {
            operation.started_at.elapsed() > BLE_RNODE_ACTIVITY_OPERATION_TTL
        }) {
            *pending = None;
            return false;
        }
        let Some(operation) = pending.as_mut() else {
            return false;
        };
        if operation.token != token || operation.phase != BleRnodeActivityOperationPhase::Cancelling
        {
            return false;
        }
        operation.phase = BleRnodeActivityOperationPhase::CancellationCompleting;
        operation.started_at = Instant::now();
        true
    }

    pub fn take_completing_ble_rnode_activity_cancellation(
        &self,
        token_hex: &str,
    ) -> Option<ActivityRequestFence> {
        self.take_ble_rnode_activity_operation_in_phase_with_completion(
            token_hex,
            BleRnodeActivityOperationPhase::CancellationCompleting,
        )
        .map(|(fence, _, _)| fence)
    }

    pub fn claim_ble_rnode_activity_operation(
        &self,
        token_hex: &str,
    ) -> Option<ActivityRequestFence> {
        let token: [u8; 16] = hex::decode(token_hex).ok()?.try_into().ok()?;
        let mut pending = self
            .ble_rnode_activity_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.as_ref().is_some_and(|operation| {
            operation.started_at.elapsed() > BLE_RNODE_ACTIVITY_OPERATION_TTL
        }) {
            *pending = None;
            return None;
        }
        let operation = pending.as_mut()?;
        if operation.token != token
            || operation.phase != BleRnodeActivityOperationPhase::PendingNative
            || !self.ble_rnode_lifecycle_is_current(operation)
        {
            return None;
        }
        operation.phase = BleRnodeActivityOperationPhase::Initializing;
        operation.started_at = Instant::now();
        Some(operation.activity_fence)
    }

    pub fn take_pending_ble_rnode_activity_operation(
        &self,
        token_hex: &str,
    ) -> Option<BleRnodeActivityTake> {
        self.take_pending_ble_rnode_activity_operation_with_completion(token_hex)
            .map(|(fence, rollback_context, _completion)| (fence, rollback_context))
    }

    pub fn take_pending_ble_rnode_activity_operation_with_completion(
        &self,
        token_hex: &str,
    ) -> Option<BleRnodeActivityTakeWithCompletion> {
        self.take_ble_rnode_activity_operation_in_phase_with_completion(
            token_hex,
            BleRnodeActivityOperationPhase::PendingNative,
        )
    }

    pub fn take_initializing_ble_rnode_activity_operation(
        &self,
        token_hex: &str,
    ) -> Option<BleRnodeActivityTake> {
        self.take_initializing_ble_rnode_activity_operation_with_completion(token_hex)
            .map(|(fence, rollback_context, _completion)| (fence, rollback_context))
    }

    pub fn take_initializing_ble_rnode_activity_operation_with_completion(
        &self,
        token_hex: &str,
    ) -> Option<BleRnodeActivityTakeWithCompletion> {
        self.take_ble_rnode_activity_operation_in_phase_with_completion(
            token_hex,
            BleRnodeActivityOperationPhase::Initializing,
        )
    }

    pub fn claim_ble_rnode_activity_operation_completion(
        &self,
        token_hex: &str,
    ) -> Option<ActivityRequestFence> {
        let token: [u8; 16] = hex::decode(token_hex).ok()?.try_into().ok()?;
        let mut pending = self
            .ble_rnode_activity_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.as_ref().is_some_and(|operation| {
            operation.started_at.elapsed() > BLE_RNODE_ACTIVITY_OPERATION_TTL
        }) {
            *pending = None;
            return None;
        }
        let operation = pending.as_mut()?;
        if operation.token != token
            || operation.phase != BleRnodeActivityOperationPhase::Initializing
            || !self.ble_rnode_lifecycle_is_current(operation)
        {
            return None;
        }
        operation.phase = BleRnodeActivityOperationPhase::Completing;
        operation.started_at = Instant::now();
        Some(operation.activity_fence)
    }

    pub fn take_completing_ble_rnode_activity_operation(
        &self,
        token_hex: &str,
    ) -> Option<BleRnodeActivityTake> {
        self.take_completing_ble_rnode_activity_operation_with_completion(token_hex)
            .map(|(fence, rollback_context, _completion)| (fence, rollback_context))
    }

    pub fn take_completing_ble_rnode_activity_operation_with_completion(
        &self,
        token_hex: &str,
    ) -> Option<BleRnodeActivityTakeWithCompletion> {
        self.take_ble_rnode_activity_operation_in_phase_with_completion(
            token_hex,
            BleRnodeActivityOperationPhase::Completing,
        )
    }

    fn take_ble_rnode_activity_operation_in_phase_with_completion(
        &self,
        token_hex: &str,
        expected_phase: BleRnodeActivityOperationPhase,
    ) -> Option<BleRnodeActivityTakeWithCompletion> {
        let token: [u8; 16] = hex::decode(token_hex).ok()?.try_into().ok()?;
        let mut pending = self
            .ble_rnode_activity_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.as_ref().is_some_and(|operation| {
            operation.started_at.elapsed() > BLE_RNODE_ACTIVITY_OPERATION_TTL
        }) {
            *pending = None;
            return None;
        }
        let cancellation_phase = matches!(
            expected_phase,
            BleRnodeActivityOperationPhase::Cancelling
                | BleRnodeActivityOperationPhase::CancellationCompleting
        );
        if pending.as_ref().is_some_and(|operation| {
            operation.token == token
                && operation.phase == expected_phase
                && (cancellation_phase || self.ble_rnode_lifecycle_is_current(operation))
        }) {
            pending.take().map(|operation| {
                let rollback_context = operation
                    .rollback_context
                    .map(|context| (context.config_dir, context.name, context.marker));
                (
                    operation.activity_fence,
                    rollback_context,
                    operation.completion,
                )
            })
        } else {
            None
        }
    }

    pub fn invalidate_ble_rnode_activity_operation(&self) -> Option<BleRnodeActivityCancellation> {
        let mut pending = self
            .ble_rnode_activity_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.as_ref().is_some_and(|operation| {
            matches!(
                operation.phase,
                BleRnodeActivityOperationPhase::Cancelling
                    | BleRnodeActivityOperationPhase::CancellationCompleting
            )
        }) {
            *pending = None;
            return None;
        }
        let operation = pending.take()?;
        (operation.started_at.elapsed() <= BLE_RNODE_ACTIVITY_OPERATION_TTL).then(|| {
            let rollback_context = operation
                .rollback_context
                .map(|context| (context.config_dir, context.name, context.marker));
            (
                hex::encode(operation.token),
                operation.activity_fence,
                rollback_context,
            )
        })
    }

    /// Invalidate only the native BLE operation identified by `token_hex`.
    ///
    /// Lifecycle watchers use this to revoke their own bridge operation after
    /// a same-name replacement. A delayed watcher can never cancel the newer
    /// global BLE operation that replaced its token.
    pub fn invalidate_ble_rnode_activity_operation_if_token(
        &self,
        token_hex: &str,
    ) -> Option<BleRnodeActivityCancellation> {
        let token: [u8; 16] = hex::decode(token_hex).ok()?.try_into().ok()?;
        let mut pending = self
            .ble_rnode_activity_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let operation = pending.as_ref()?;
        if operation.token != token
            || matches!(
                operation.phase,
                BleRnodeActivityOperationPhase::Cancelling
                    | BleRnodeActivityOperationPhase::CancellationCompleting
            )
        {
            return None;
        }
        let operation = pending.take()?;
        (operation.started_at.elapsed() <= BLE_RNODE_ACTIVITY_OPERATION_TTL).then(|| {
            let rollback_context = operation
                .rollback_context
                .map(|context| (context.config_dir, context.name, context.marker));
            (
                hex::encode(operation.token),
                operation.activity_fence,
                rollback_context,
            )
        })
    }

    pub fn clear_identity_scoped_runtime_state(&self) {
        if let Ok(mut channels) = self.channels.write() {
            *channels = None;
        }
        if let Ok(mut channel_hub) = self.channel_hub.write() {
            *channel_hub = None;
        }
        if let Ok(mut known) = self.known_path_hashes.lock() {
            known.clear();
        }
        self.path_activity_baselined.store(false, Ordering::Relaxed);
        if let Ok(mut history) = self.announce_history.write() {
            history.clear();
        }
        if let Ok(mut alerts) = self.alerts.lock() {
            alerts.clear();
        }
        if let Ok(mut seen) = self.seen_announce_hashes.lock() {
            seen.clear();
        }
        self.announce_activity_baselined
            .store(false, Ordering::Relaxed);
        if let Ok(mut times) = self.message_send_times.lock() {
            times.clear();
        }
        if let Ok(mut map) = self.msg_id_map.lock() {
            map.clear();
        }
        if let Ok(mut sessions) = self.lrgp_msg_to_session.lock() {
            sessions.clear();
        }
        if let Ok(mut pn) = self.propagation_node.lock() {
            *pn = None;
        }
        if let Ok(mut stats) = self.last_stats.write() {
            *stats = None;
        }
        if let Ok(mut hub) = self.last_hub_interfaces.write() {
            *hub = None;
        }
        if let Ok(mut nodes) = self.discovered_propagation_nodes.lock() {
            nodes.clear();
        }
        if let Ok(mut node) = self.auto_active_node.write() {
            *node = None;
        }
        if let Ok(mut failures) = self.auto_failure_counts.lock() {
            failures.clear();
        }
        self.last_lxmf_delivery_announce_at_ms
            .store(0, Ordering::Relaxed);
        if let Ok(mut last) = self.last_opportunistic_announce_at.lock() {
            *last = None;
        }
        if let Ok(mut inflight) = self.opportunistic_announce_inflight.lock() {
            inflight.clear();
        }
        if let Ok(mut suppressions) = self.interface_reannounce_suppression.lock() {
            suppressions.clear();
        }
        if let Ok(mut pending) = self.ble_rnode_activity_operation.lock() {
            *pending = None;
        }
        if let Ok(mut operations) = self.rnode_lifecycle_operations.lock() {
            operations.clear();
        }
        if let Ok(mut last) = self.last_refresh_request_at.lock() {
            *last = None;
        }
        if let Ok(mut last) = self.last_static_probe_at.lock() {
            *last = None;
        }
    }

    pub fn emit_native_notification(&self, notification: NativeNotification) {
        if self.native_notifications_enabled() {
            self.notifier.notify(notification);
        }
    }

    pub fn set_startup_stage(&self, stage: &str) {
        if let Ok(mut s) = self.startup_stage.write() {
            *s = stage.to_string();
        }
    }

    pub fn get_startup_stage(&self) -> String {
        self.startup_stage
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|_| "unknown".into())
    }

    /// Best-effort broadcast; never panics on torn-down WebView.
    pub fn emit_to_all(&self, event: &str, data: serde_json::Value) {
        self.emitter.emit(event, data);
    }

    pub fn set_rns(&self, mut rns: RnsManager) -> Option<RNodeActivityOrigin> {
        let origin = RNodeActivityOrigin::new(
            self.current_identity_session_generation(),
            next_rnode_activity_session_generation(&self.rnode_activity_session_generation),
        );
        rns.bind_rnode_activity_session(origin);
        if let Ok(mut r) = self.rns.write() {
            *r = Some(rns);
            Some(origin)
        } else {
            None
        }
    }

    /// Capture the current RNS handle and opaque ownership origin under one
    /// manager read lock. RNode commands must spawn through this context so a
    /// soft RNS restart cannot rebase later coverage onto a replacement.
    pub fn rnode_activity_runtime_context_for_identity(
        &self,
        origin_identity_generation: u64,
    ) -> Option<crate::rnode_activity::RNodeActivityRuntimeContext> {
        if self.current_identity_session_generation() != origin_identity_generation {
            return None;
        }
        let Ok(rns) = self.rns.read() else {
            return None;
        };
        if self.current_identity_session_generation() != origin_identity_generation {
            return None;
        }
        rns.as_ref()?
            .rnode_activity_runtime_context(origin_identity_generation)
    }

    /// Cover one exact registration only if `origin` still owns the installed
    /// RNS manager. Coverage is a tombstone until that manager is torn down.
    pub fn cover_rnode_activity_interface(
        &self,
        interface_id: rns_interface::traits::InterfaceId,
        origin: RNodeActivityOrigin,
    ) -> bool {
        if self.current_identity_session_generation() != origin.identity_generation() {
            return false;
        }
        let Ok(mut rns) = self.rns.write() else {
            return false;
        };
        if self.current_identity_session_generation() != origin.identity_generation() {
            return false;
        }
        rns.as_mut()
            .is_some_and(|rns| rns.cover_rnode_activity_interface(interface_id, origin))
    }

    pub(crate) fn owns_rnode_activity_observation(
        &self,
        interface_id: rns_interface::traits::InterfaceId,
        origin: RNodeActivityOrigin,
    ) -> bool {
        if self.current_identity_session_generation() != origin.identity_generation() {
            return false;
        }
        self.rns
            .read()
            .ok()
            .and_then(|rns| {
                rns.as_ref().map(|rns| {
                    rns.owns_rnode_activity_session(origin)
                        && rns.is_rnode_activity_interface_covered(
                            interface_id,
                            origin.identity_generation(),
                        )
                })
            })
            .unwrap_or(false)
    }

    pub(crate) fn is_rnode_activity_interface_covered(
        &self,
        interface_id: rns_interface::traits::InterfaceId,
    ) -> bool {
        let identity_generation = self.current_identity_session_generation();
        self.rns
            .read()
            .ok()
            .and_then(|rns| {
                rns.as_ref().map(|rns| {
                    rns.is_rnode_activity_interface_covered(interface_id, identity_generation)
                })
            })
            .unwrap_or(false)
    }

    pub fn set_channels(&self, channels: ChannelsManagerHandle) {
        if let Ok(mut current) = self.channels.write() {
            *current = Some(channels);
        }
    }

    pub fn channels_handle(&self) -> Option<ChannelsManagerHandle> {
        self.channels
            .read()
            .ok()
            .and_then(|channels| channels.clone())
    }

    pub fn take_channels(&self) -> Option<ChannelsManagerHandle> {
        self.channels
            .write()
            .ok()
            .and_then(|mut channels| channels.take())
    }

    pub fn set_channel_hub(&self, hub: crate::channel_hub::ChannelHubHandle) {
        if let Ok(mut current) = self.channel_hub.write() {
            *current = Some(hub);
        }
    }

    pub fn channel_hub_handle(&self) -> Option<crate::channel_hub::ChannelHubHandle> {
        self.channel_hub.read().ok().and_then(|hub| hub.clone())
    }

    pub fn take_channel_hub(&self) -> Option<crate::channel_hub::ChannelHubHandle> {
        self.channel_hub.write().ok().and_then(|mut hub| hub.take())
    }

    pub fn set_last_stats(&self, stats: serde_json::Value) {
        if let Ok(mut s) = self.last_stats.write() {
            *s = Some(stats);
        }
    }

    pub fn set_last_hub_interfaces(&self, interfaces: serde_json::Value) {
        if let Ok(mut s) = self.last_hub_interfaces.write() {
            *s = Some(interfaces);
        }
    }

    pub fn set_lxmf(&self, lxmf: LxmfManager) {
        let identity_hash = lxmf.identity_hash.clone();
        let public_key = lxmf.identity.get_public_key();
        if let Ok(mut keys) = self.local_identity_public_keys.write() {
            keys.insert(identity_hash, public_key);
        }
        if let Ok(mut l) = self.lxmf.lock() {
            *l = Some(lxmf);
        }
    }

    pub fn local_identity_public_key(&self, identity_hash: &str) -> Option<[u8; 64]> {
        self.local_identity_public_keys
            .read()
            .ok()
            .and_then(|keys| keys.get(identity_hash).copied())
    }

    pub fn forget_local_identity_public_key(&self, identity_hash: &str) {
        if let Ok(mut keys) = self.local_identity_public_keys.write() {
            keys.remove(identity_hash);
        }
    }

    /// Re-anchor send times to "now" so post-suspend resumes don't fail every
    /// in-flight send on the first tick.
    pub fn reset_message_send_times_on_resume(&self) -> usize {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let Ok(mut times) = self.message_send_times.lock() else {
            return 0;
        };
        let count = times.len();
        if count > 0 {
            for v in times.values_mut() {
                *v = now;
            }
        }
        count
    }

    pub fn trim_propagation_nodes(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let Ok(mut nodes) = self.discovered_propagation_nodes.lock() else {
            return;
        };

        nodes.retain(|_, v| {
            if v.get("static").and_then(|s| s.as_bool()).unwrap_or(false) {
                return true;
            }

            v.get("last_seen")
                .and_then(json_number_as_f64)
                .map(|t| t > 0.0 && now - t < PROPAGATION_NODE_TTL_SECS as f64)
                .unwrap_or(false)
        });

        if nodes.len() > MAX_DISCOVERED_PROPAGATION_NODES {
            let to_drop = nodes.len() - MAX_DISCOVERED_PROPAGATION_NODES;
            let mut entries: Vec<(String, bool, u64)> = nodes
                .iter()
                .map(|(k, v)| {
                    let is_static = v.get("static").and_then(|s| s.as_bool()).unwrap_or(false);
                    let ts = v
                        .get("last_seen")
                        .and_then(json_number_as_f64)
                        .unwrap_or(0.0)
                        .max(0.0) as u64;
                    (k.clone(), is_static, ts)
                })
                .collect();
            entries.sort_by_key(|(_, is_static, t)| (*is_static, *t));
            for (key, _, _) in entries.into_iter().take(to_drop) {
                nodes.remove(&key);
            }
        }
    }
}

fn json_number_as_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_u64().map(|v| v as f64))
        .or_else(|| value.as_i64().map(|v| v as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DashboardConfig;
    use r2d2_sqlite::SqliteConnectionManager;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static TEMP_STATE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct RecordingEmitter {
        events: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl ratspeak_core::Emitter for RecordingEmitter {
        fn try_emit(
            &self,
            event: &str,
            payload: serde_json::Value,
        ) -> Result<(), ratspeak_core::EmitError> {
            self.events
                .lock()
                .unwrap()
                .push((event.to_string(), payload));
            Ok(())
        }
    }

    struct FailingEmitter;

    impl ratspeak_core::Emitter for FailingEmitter {
        fn try_emit(
            &self,
            _event: &str,
            _payload: serde_json::Value,
        ) -> Result<(), ratspeak_core::EmitError> {
            Err(ratspeak_core::EmitError::Unavailable)
        }
    }

    fn make_state_with_emitter(emitter: Arc<dyn ratspeak_core::Emitter>) -> AppState {
        let unique = TEMP_STATE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-state-test-{}-{}-{unique}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = DashboardConfig::from_env_and_defaults(tmp);
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
        AppState::new(config, pool, emitter, Arc::new(ratspeak_core::NoopNotifier))
    }

    fn make_state() -> AppState {
        make_state_with_emitter(Arc::new(ratspeak_core::NoopEmitter))
    }

    #[test]
    fn set_lxmf_snapshots_public_identity_key_outside_router_lock() {
        let state = make_state();
        let data_root = state.config.data_root.clone();
        let manager = crate::lxmf::LxmfManager::load_or_create(&data_root, None, None).unwrap();
        let identity_hash = manager.identity_hash.clone();
        let public_key = manager.identity.get_public_key();
        state.set_lxmf(manager);

        let router_guard = state.lxmf.lock().unwrap();
        assert_eq!(
            state.local_identity_public_key(&identity_hash),
            Some(public_key)
        );

        drop(router_guard);
        std::fs::remove_dir_all(data_root).ok();
    }

    #[tokio::test]
    async fn activity_request_fence_is_invalidated_by_both_privacy_boundaries() {
        let state = make_state();
        let initial = state.activity_request_fence();
        let initial_guard = state.identity_switch_lock.lock().await;
        assert!(state.is_current_activity_request_fence_after_identity_lock(initial));

        state.bump_activity_boundary_generation();
        assert!(!state.is_current_activity_request_fence_after_identity_lock(initial));
        drop(initial_guard);

        let after_runtime_reset = state.activity_request_fence();
        let identity_guard = state.identity_switch_lock.lock().await;
        assert!(state.is_current_activity_request_fence_after_identity_lock(after_runtime_reset));

        state.bump_identity_session_generation();
        assert!(!state.is_current_activity_request_fence_after_identity_lock(after_runtime_reset));
        drop(identity_guard);
    }

    #[tokio::test]
    async fn request_born_during_identity_lock_span_is_rejected_afterward() {
        let state = make_state();
        let transition = state.identity_switch_lock.lock().await;
        let born_during_transition = state.activity_request_fence();
        drop(transition);

        let command = state.identity_switch_lock.lock().await;
        assert!(
            !state.is_current_activity_request_fence_after_identity_lock(born_during_transition)
        );
        drop(command);
    }

    #[tokio::test]
    async fn request_snapshot_straddling_a_transition_release_is_rejected() {
        let state = make_state();
        let fence = state.activity_request_fence_after_epoch(|| {
            // Deterministically model one complete intervening lock span at
            // the exact hook after the snapshot's epoch linearization point.
            state
                .identity_switch_lock
                .epoch
                .fetch_add(2, Ordering::SeqCst);
        });

        let command = state.identity_switch_lock.lock().await;
        assert!(!state.is_current_activity_request_fence_after_identity_lock(fence));
        drop(command);
    }

    #[test]
    fn async_producer_origin_fence_rejects_completed_privacy_boundaries() {
        let state = make_state();
        let fence = state.activity_request_fence();
        assert!(state.is_current_activity_origin_fence(fence));

        state.bump_activity_boundary_generation();
        assert!(!state.is_current_activity_origin_fence(fence));

        let next = state.activity_request_fence();
        assert!(state.is_current_activity_origin_fence(next));
        state.bump_identity_session_generation();
        assert!(!state.is_current_activity_origin_fence(next));
    }

    #[tokio::test]
    async fn async_producer_fence_born_inside_transition_never_becomes_current() {
        let state = make_state();
        let transition = state.identity_switch_lock.lock().await;
        let fence = state.activity_request_fence();
        assert!(!state.is_current_activity_origin_fence(fence));
        drop(transition);
        assert!(!state.is_current_activity_origin_fence(fence));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fenced_record_closes_the_origin_check_to_new_capture_admission_race() {
        let state = make_state();
        state.activity.start().await.unwrap();
        let stale_origin = state.activity_request_fence();

        // This is the old unsafe precheck. Deterministically place a complete
        // same-identity reset and new capture between it and recorder
        // admission, modeling a producer descheduled at exactly that point.
        assert!(state.is_current_activity_origin_fence(stale_origin));
        state.bump_activity_boundary_generation();
        state.activity.hard_reset().await.unwrap();
        let new_session = state
            .activity
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();

        let built = AtomicBool::new(false);
        let outcome = state.activity.record_event_fenced(
            || state.is_current_activity_origin_fence(stale_origin),
            || {
                built.store(true, Ordering::Relaxed);
                Ok(crate::activity::producer::app_runtime(
                    crate::activity::producer::AppRuntimeTransition::Ready,
                ))
            },
        );
        assert_eq!(
            outcome,
            crate::activity::ActivityRecordOutcome::StaleGeneration
        );
        assert!(!built.load(Ordering::Relaxed));

        let crate::activity::ActivityReplayResultV1::Page { page } = state
            .activity
            .replay(new_session, None, 50, 64 * 1024)
            .await
            .unwrap()
        else {
            panic!("new capture should replay");
        };
        assert!(
            page.events()
                .iter()
                .all(|event| event.kind() != "app.runtime.ready")
        );
        state.activity.shutdown().await.unwrap();
    }

    #[test]
    fn interface_reannounce_suppression_is_one_shot() {
        let state = make_state();

        assert!(!state.take_interface_reannounce_suppression("LoRa"));
        state.suppress_next_interface_reannounce("LoRa");

        assert!(state.take_interface_reannounce_suppression("LoRa"));
        assert!(!state.take_interface_reannounce_suppression("LoRa"));
    }

    #[test]
    fn stale_interface_reannounce_suppression_expires() {
        let state = make_state();
        {
            let mut suppressions = state.interface_reannounce_suppression.lock().unwrap();
            suppressions.insert(
                "LoRa".to_string(),
                Instant::now() - INTERFACE_REANNOUNCE_SUPPRESSION_TTL - Duration::from_secs(1),
            );
        }

        assert!(!state.take_interface_reannounce_suppression("LoRa"));
        assert!(
            state
                .interface_reannounce_suppression
                .lock()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rnode_lifecycle_operation_replaces_same_name_owner() {
        let state = make_state();
        let first = state
            .begin_rnode_lifecycle_operation(["LoRa"])
            .expect("first operation");
        let second = state
            .begin_rnode_lifecycle_operation(["LoRa"])
            .expect("replacement operation");

        assert!(!state.is_current_rnode_lifecycle_operation(&first));
        assert!(state.is_current_rnode_lifecycle_operation(&second));
    }

    #[test]
    fn rnode_lifecycle_multi_name_overlap_invalidates_entire_older_lease() {
        let state = make_state();
        let rename = state
            .begin_rnode_lifecycle_operation(["Old Radio", "New Radio"])
            .expect("rename operation");
        let unrelated = state
            .begin_rnode_lifecycle_operation(["Other Radio"])
            .expect("unrelated operation");
        let overlapping = state
            .begin_rnode_lifecycle_operation(["New Radio", "Third Radio"])
            .expect("overlapping operation");

        assert!(!state.is_current_rnode_lifecycle_operation(&rename));
        assert!(state.is_current_rnode_lifecycle_operation(&unrelated));
        assert!(state.is_current_rnode_lifecycle_operation(&overlapping));
        let operations = state.rnode_lifecycle_operations.lock().unwrap();
        assert!(!operations.contains_key("Old Radio"));
        assert_eq!(
            operations.get("New Radio").unwrap().generation,
            overlapping.generation
        );
        assert_eq!(
            operations.get("Third Radio").unwrap().generation,
            overlapping.generation
        );
    }

    #[test]
    fn rnode_lifecycle_generations_are_monotonic_across_runtime_clear() {
        let state = make_state();
        let first = state
            .begin_rnode_lifecycle_operation(["First Radio"])
            .expect("first operation");

        state.clear_identity_scoped_runtime_state();

        let second = state
            .begin_rnode_lifecycle_operation(["Second Radio"])
            .expect("second operation");
        assert!(second.generation > first.generation);
        assert!(!state.is_current_rnode_lifecycle_operation(&first));
        assert!(state.is_current_rnode_lifecycle_operation(&second));
    }

    #[test]
    fn rnode_lifecycle_ttl_covers_native_setup_readiness_and_rollback() {
        let one_native_attempt = Duration::from_secs(180 + 120);
        assert_eq!(RNODE_LIFECYCLE_OPERATION_TTL, Duration::from_secs(15 * 60));
        assert!(RNODE_LIFECYCLE_OPERATION_TTL > one_native_attempt * 2);
    }

    #[test]
    fn stale_rnode_lifecycle_finish_does_not_delete_replacement() {
        let state = make_state();
        let first = state
            .begin_rnode_lifecycle_operation(["LoRa"])
            .expect("first operation");
        let replacement = state
            .begin_rnode_lifecycle_operation(["LoRa"])
            .expect("replacement operation");

        assert!(!state.finish_rnode_lifecycle_operation(&first));
        assert!(state.is_current_rnode_lifecycle_operation(&replacement));
        assert!(state.finish_rnode_lifecycle_operation(&replacement));
        assert!(!state.is_current_rnode_lifecycle_operation(&replacement));
    }

    #[test]
    fn rnode_lifecycle_invalidation_removes_all_names_for_matching_operations() {
        let state = make_state();
        let rename = state
            .begin_rnode_lifecycle_operation(["Old Radio", "New Radio"])
            .expect("rename operation");
        let unrelated = state
            .begin_rnode_lifecycle_operation(["Other Radio"])
            .expect("unrelated operation");

        assert_eq!(
            state.invalidate_rnode_lifecycle_operations_for_names(["New Radio", "Missing"]),
            1
        );
        assert!(!state.is_current_rnode_lifecycle_operation(&rename));
        assert!(state.is_current_rnode_lifecycle_operation(&unrelated));
        assert_eq!(
            state.invalidate_rnode_lifecycle_operations_for_names(["Other Radio"]),
            1
        );
        assert!(!state.is_current_rnode_lifecycle_operation(&unrelated));
    }

    #[test]
    fn rnode_lifecycle_operation_expires_and_is_pruned() {
        let state = make_state();
        let lease = state
            .begin_rnode_lifecycle_operation(["LoRa"])
            .expect("operation");
        {
            let mut operations = state.rnode_lifecycle_operations.lock().unwrap();
            operations.get_mut("LoRa").unwrap().started_at =
                Instant::now() - RNODE_LIFECYCLE_OPERATION_TTL - Duration::from_secs(1);
        }

        assert!(!state.is_current_rnode_lifecycle_operation(&lease));
        assert!(state.rnode_lifecycle_operations.lock().unwrap().is_empty());
    }

    #[test]
    fn identity_runtime_clear_invalidates_rnode_lifecycle_operations() {
        let state = make_state();
        let lease = state
            .begin_rnode_lifecycle_operation(["Old Radio", "New Radio"])
            .expect("rename operation");

        state.clear_identity_scoped_runtime_state();

        assert!(!state.is_current_rnode_lifecycle_operation(&lease));
        assert!(state.rnode_lifecycle_operations.lock().unwrap().is_empty());
    }

    #[test]
    fn ble_rnode_activity_operation_is_one_shot_and_replacement_safe() {
        let state = make_state();
        let first_fence = state.activity_request_fence();
        let first = state.begin_ble_rnode_activity_operation(first_fence, None);
        let second_fence = state.activity_request_fence();
        let second = state.begin_ble_rnode_activity_operation(second_fence, None);

        assert_eq!(first.len(), 32);
        assert_eq!(second.len(), 32);
        assert_ne!(first, second);
        assert!(!state.is_current_ble_rnode_activity_operation(&first));
        assert!(state.is_current_ble_rnode_activity_operation(&second));
        assert_eq!(state.claim_ble_rnode_activity_operation(&first), None);
        assert_eq!(
            state.claim_ble_rnode_activity_operation(&second),
            Some(second_fence)
        );
        assert_eq!(state.claim_ble_rnode_activity_operation(&second), None);
        assert!(state.is_current_ble_rnode_activity_operation(&second));
        assert_eq!(
            state.take_initializing_ble_rnode_activity_operation(&second),
            Some((second_fence, None))
        );
        assert!(!state.is_current_ble_rnode_activity_operation(&second));
        assert_eq!(
            state.take_initializing_ble_rnode_activity_operation(&second),
            None
        );
    }

    #[tokio::test]
    async fn ble_rnode_completion_reports_ready_with_exact_interface_id() {
        let state = make_state();
        let fence = state.activity_request_fence();
        let (token, receiver) =
            state.begin_ble_rnode_activity_operation_with_completion(fence, None);
        assert_eq!(
            state.claim_ble_rnode_activity_operation(&token),
            Some(fence)
        );

        let (terminal_fence, rollback_context, completion) = state
            .take_initializing_ble_rnode_activity_operation_with_completion(&token)
            .expect("exact initializing operation");
        assert_eq!(terminal_fence, fence);
        assert_eq!(rollback_context, None);
        completion
            .expect("completion sender")
            .send(BleRnodeOperationResult::Ready {
                interface_id: 73,
                monitor: None,
            })
            .expect("waiting receiver");

        assert!(matches!(
            receiver.await.expect("typed completion"),
            BleRnodeOperationResult::Ready {
                interface_id: 73,
                monitor: None,
            }
        ));
    }

    #[tokio::test]
    async fn ble_rnode_completion_reports_closed_failure_type() {
        let state = make_state();
        let fence = state.activity_request_fence();
        let (token, receiver) =
            state.begin_ble_rnode_activity_operation_with_completion(fence, None);
        assert_eq!(
            state.claim_ble_rnode_activity_operation(&token),
            Some(fence)
        );
        assert_eq!(
            state.claim_ble_rnode_activity_operation_completion(&token),
            Some(fence)
        );

        let (_, _, completion) = state
            .take_completing_ble_rnode_activity_operation_with_completion(&token)
            .expect("exact completing operation");
        completion
            .expect("completion sender")
            .send(BleRnodeOperationResult::Failed(
                BleRnodeOperationFailure::Readiness,
            ))
            .expect("waiting receiver");

        assert!(matches!(
            receiver.await.expect("typed completion"),
            BleRnodeOperationResult::Failed(BleRnodeOperationFailure::Readiness)
        ));
    }

    #[tokio::test]
    async fn replacement_and_identity_clear_close_ble_rnode_completion_receivers() {
        let state = make_state();
        let (first, first_receiver) = state.begin_ble_rnode_activity_operation_with_completion(
            state.activity_request_fence(),
            None,
        );
        let (second, second_receiver) = state.begin_ble_rnode_activity_operation_with_completion(
            state.activity_request_fence(),
            None,
        );
        assert_ne!(first, second);
        assert!(first_receiver.await.is_err());

        state.clear_identity_scoped_runtime_state();

        assert!(second_receiver.await.is_err());
    }

    #[test]
    fn exact_ble_rnode_invalidation_cannot_cancel_replacement_token() {
        let state = make_state();
        let first = state.begin_ble_rnode_activity_operation(state.activity_request_fence(), None);
        assert!(
            state
                .invalidate_ble_rnode_activity_operation_if_token(&first)
                .is_some()
        );

        let replacement =
            state.begin_ble_rnode_activity_operation(state.activity_request_fence(), None);
        assert!(
            state
                .invalidate_ble_rnode_activity_operation_if_token(&first)
                .is_none()
        );
        assert!(state.is_current_ble_rnode_activity_operation(&replacement));
        assert!(
            state
                .invalidate_ble_rnode_activity_operation_if_token(&replacement)
                .is_some()
        );
    }

    #[tokio::test]
    async fn owned_ble_rnode_operation_is_revoked_with_lifecycle_generation() {
        let state = make_state();
        let lifecycle = state
            .begin_rnode_lifecycle_operation(["Radio"])
            .expect("lifecycle lease");
        let fence = state.activity_request_fence();
        let (token, receiver) =
            state.begin_ble_rnode_activity_operation_with_completion_owned(fence, None, &lifecycle);
        assert!(state.is_current_ble_rnode_activity_operation(&token));

        let replacement = state
            .begin_rnode_lifecycle_operation(["Radio"])
            .expect("replacement lifecycle lease");
        assert!(!state.is_current_ble_rnode_activity_operation(&token));
        assert_eq!(state.claim_ble_rnode_activity_operation(&token), None);
        assert!(
            state
                .invalidate_ble_rnode_activity_operation_if_token(&token)
                .is_some()
        );
        assert!(receiver.await.is_err());
        assert!(state.is_current_rnode_lifecycle_operation(&replacement));
    }

    #[tokio::test]
    async fn global_ble_replacement_revokes_only_displaced_lifecycle() {
        let state = Arc::new(make_state());
        let first_lease = state
            .begin_rnode_lifecycle_operation(["Radio A"])
            .expect("first lifecycle");
        let (_, first_receiver) = state.begin_ble_rnode_activity_operation_with_completion_owned(
            state.activity_request_fence(),
            None,
            &first_lease,
        );
        let displaced_state = Arc::clone(&state);
        let displaced_lease = first_lease.clone();
        let displaced_observer = tokio::spawn(async move {
            assert!(first_receiver.await.is_err());
            displaced_state.is_current_rnode_lifecycle_operation(&displaced_lease)
        });

        let second_lease = state
            .begin_rnode_lifecycle_operation(["Radio B"])
            .expect("second lifecycle");
        let (second_token, _) = state.begin_ble_rnode_activity_operation_with_completion_owned(
            state.activity_request_fence(),
            None,
            &second_lease,
        );

        assert!(
            !displaced_observer
                .await
                .expect("displaced receiver observer")
        );
        assert!(!state.is_current_rnode_lifecycle_operation(&first_lease));
        assert!(state.is_current_rnode_lifecycle_operation(&second_lease));
        assert!(state.is_current_ble_rnode_activity_operation(&second_token));

        let replacement_same_lease = state.begin_ble_rnode_activity_operation_owned(
            state.activity_request_fence(),
            None,
            &second_lease,
        );
        assert!(state.is_current_rnode_lifecycle_operation(&second_lease));
        assert!(state.is_current_ble_rnode_activity_operation(&replacement_same_lease));
    }

    #[tokio::test]
    async fn legacy_ble_rnode_take_deliberately_closes_completion_receiver() {
        let state = make_state();
        let fence = state.activity_request_fence();
        let (token, receiver) =
            state.begin_ble_rnode_activity_operation_with_completion(fence, None);

        assert_eq!(
            state.take_pending_ble_rnode_activity_operation(&token),
            Some((fence, None))
        );
        assert!(receiver.await.is_err());
    }

    #[test]
    fn ble_rnode_activity_operation_can_be_cancelled_and_expires() {
        let state = make_state();
        let cancelled_fence = state.activity_request_fence();
        let cancelled = state.begin_ble_rnode_activity_operation(cancelled_fence, None);

        assert_eq!(
            state.invalidate_ble_rnode_activity_operation(),
            Some((cancelled.clone(), cancelled_fence, None))
        );
        assert!(!state.is_current_ble_rnode_activity_operation(&cancelled));
        assert_eq!(state.invalidate_ble_rnode_activity_operation(), None);

        let initializing_fence = state.activity_request_fence();
        let initializing = state.begin_ble_rnode_activity_operation(initializing_fence, None);
        assert_eq!(
            state.claim_ble_rnode_activity_operation(&initializing),
            Some(initializing_fence)
        );
        assert_eq!(
            state.invalidate_ble_rnode_activity_operation(),
            Some((initializing.clone(), initializing_fence, None))
        );
        assert_eq!(
            state.take_initializing_ble_rnode_activity_operation(&initializing),
            None
        );

        let expired =
            state.begin_ble_rnode_activity_operation(state.activity_request_fence(), None);
        {
            let mut pending = state
                .ble_rnode_activity_operation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending.as_mut().expect("pending operation").started_at =
                Instant::now() - BLE_RNODE_ACTIVITY_OPERATION_TTL - Duration::from_secs(1);
        }
        assert!(!state.is_current_ble_rnode_activity_operation(&expired));
        assert_eq!(
            state.take_pending_ble_rnode_activity_operation(&expired),
            None
        );
    }

    #[test]
    fn identity_runtime_clear_invalidates_ble_rnode_activity_operation() {
        let state = make_state();
        let token = state.begin_ble_rnode_activity_operation(state.activity_request_fence(), None);

        state.clear_identity_scoped_runtime_state();

        assert_eq!(
            state.take_pending_ble_rnode_activity_operation(&token),
            None
        );
    }

    #[test]
    fn activity_boundary_change_makes_ble_rnode_operation_origin_stale() {
        let state = make_state();
        let origin = state.activity_request_fence();
        let token = state.begin_ble_rnode_activity_operation(origin, None);

        state.bump_activity_boundary_generation();

        let (recovered, _) = state
            .take_pending_ble_rnode_activity_operation(&token)
            .expect("valid one-shot token");
        assert_eq!(recovered, origin);
        assert!(!state.is_current_activity_origin_fence(recovered));
    }

    #[test]
    fn stale_ble_failure_cannot_take_newer_rollback_context() {
        let state = make_state();
        let first_fence = state.activity_request_fence();
        let first = state.begin_ble_rnode_activity_operation(
            first_fence,
            Some((PathBuf::from("/identity-a"), "LoRa".to_string(), 11)),
        );
        let second_fence = state.activity_request_fence();
        let second = state.begin_ble_rnode_activity_operation(
            second_fence,
            Some((PathBuf::from("/identity-b"), "LoRa".to_string(), 22)),
        );

        assert_eq!(
            state.take_pending_ble_rnode_activity_operation(&first),
            None
        );
        assert_eq!(
            state.take_pending_ble_rnode_activity_operation(&second),
            Some((
                second_fence,
                Some((PathBuf::from("/identity-b"), "LoRa".to_string(), 22))
            ))
        );
    }

    #[test]
    fn ble_completion_ownership_is_lost_when_new_operation_replaces_it() {
        let state = make_state();
        let first_fence = state.activity_request_fence();
        let first = state.begin_ble_rnode_activity_operation(first_fence, None);
        assert_eq!(
            state.claim_ble_rnode_activity_operation(&first),
            Some(first_fence)
        );
        assert_eq!(
            state.claim_ble_rnode_activity_operation_completion(&first),
            Some(first_fence)
        );
        assert!(state.is_current_ble_rnode_activity_operation(&first));

        let second = state.begin_ble_rnode_activity_operation(state.activity_request_fence(), None);

        assert_eq!(
            state.take_completing_ble_rnode_activity_operation(&first),
            None
        );
        assert!(state.is_current_ble_rnode_activity_operation(&second));
    }

    #[test]
    fn ble_cancellation_lease_cannot_claim_a_replacement_operation() {
        let state = make_state();
        let first_fence = state.activity_request_fence();
        let first = state.begin_ble_rnode_activity_operation(
            first_fence,
            Some((PathBuf::from("/identity-a"), "LoRa".to_string(), 11)),
        );
        let (cancel_token, cancel_fence, rollback_context) = state
            .begin_ble_rnode_activity_cancellation()
            .expect("current operation should become a cancellation lease");
        assert_eq!(cancel_token, first);
        assert_eq!(cancel_fence, first_fence);
        assert_eq!(
            rollback_context,
            Some((PathBuf::from("/identity-a"), "LoRa".to_string(), 11))
        );
        assert!(!state.is_current_ble_rnode_activity_operation(&first));

        let second = state.begin_ble_rnode_activity_operation(
            state.activity_request_fence(),
            Some((PathBuf::from("/identity-b"), "LoRa".to_string(), 22)),
        );

        assert!(!state.claim_ble_rnode_activity_cancellation(&cancel_token));
        assert_eq!(
            state.take_completing_ble_rnode_activity_cancellation(&cancel_token),
            None
        );
        assert!(state.is_current_ble_rnode_activity_operation(&second));
    }

    #[test]
    fn ble_claimed_cancellation_suppresses_terminal_after_replacement() {
        let state = make_state();
        let first_fence = state.activity_request_fence();
        let first = state.begin_ble_rnode_activity_operation(first_fence, None);
        let (cancel_token, _, _) = state
            .begin_ble_rnode_activity_cancellation()
            .expect("current operation should become a cancellation lease");
        assert_eq!(cancel_token, first);
        assert!(state.claim_ble_rnode_activity_cancellation(&cancel_token));

        let second = state.begin_ble_rnode_activity_operation(state.activity_request_fence(), None);

        assert_eq!(
            state.take_completing_ble_rnode_activity_cancellation(&cancel_token),
            None
        );
        assert!(state.is_current_ble_rnode_activity_operation(&second));
    }

    #[test]
    fn ble_cancellation_terminal_requires_the_exact_claimed_lease() {
        let state = make_state();
        let fence = state.activity_request_fence();
        let token = state.begin_ble_rnode_activity_operation(fence, None);
        let (cancel_token, _, _) = state
            .begin_ble_rnode_activity_cancellation()
            .expect("current operation should become a cancellation lease");
        assert_eq!(cancel_token, token);
        assert!(state.claim_ble_rnode_activity_cancellation(&cancel_token));
        assert_eq!(
            state.take_completing_ble_rnode_activity_cancellation(&cancel_token),
            Some(fence)
        );
        assert_eq!(
            state.take_completing_ble_rnode_activity_cancellation(&cancel_token),
            None
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activity_batches_use_the_result_bearing_app_emitter() {
        let emitter = Arc::new(RecordingEmitter::default());
        let state = make_state_with_emitter(emitter.clone());
        let started = state.activity.start().await.unwrap();
        let session = started.capture_session().unwrap().to_string();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if emitter
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(event, _)| event == crate::activity::ACTIVITY_BATCH_EVENT)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the typed batch should reach the app emitter");

        {
            let events = emitter.events.lock().unwrap();
            let payload = &events
                .iter()
                .find(|(event, _)| event == crate::activity::ACTIVITY_BATCH_EVENT)
                .unwrap()
                .1;
            assert_eq!(payload["version"], 1);
            assert_eq!(payload["capture_session"], session);
            assert_eq!(payload["events"][0]["kind"], "diagnostics.capture_started");
            let status = events
                .iter()
                .find(|(event, _)| event == crate::activity::ACTIVITY_STATUS_EVENT)
                .expect("the lifecycle acknowledgement should also publish canonical status");
            assert_eq!(status.1["capture_session"], session);
            assert_eq!(status.1["state"], "capturing");
        }
        state.activity.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn emitter_failure_is_counted_while_the_batch_remains_replayable() {
        let state = make_state_with_emitter(Arc::new(FailingEmitter));
        let session = state
            .activity
            .start()
            .await
            .unwrap()
            .capture_session()
            .unwrap()
            .to_string();
        tokio::time::timeout(Duration::from_secs(2), async {
            while state.activity.status().counters().ipc_failure() == "0" {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("a rejected typed batch should increment IPC health");

        let crate::activity::ActivityReplayResultV1::Page { page } = state
            .activity
            .replay(session, None, 50, 64 * 1024)
            .await
            .unwrap()
        else {
            panic!("the failed publish should remain replayable");
        };
        assert_eq!(page.events().len(), 1);
        assert_eq!(page.events()[0].kind(), "diagnostics.capture_started");
        state.activity.shutdown().await.unwrap();
    }

    #[test]
    fn newer_foreground_transition_supersedes_an_awaiting_resume() {
        let state = make_state();
        let pending_resume = state.begin_foreground_transition();
        let background = state.begin_foreground_transition();

        assert!(!state.is_current_foreground_transition(pending_resume));
        assert!(state.is_current_foreground_transition(background));
    }

    #[test]
    fn trim_propagation_nodes_evicts_expired() {
        let state = make_state();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        {
            let mut nodes = state.discovered_propagation_nodes.lock().unwrap();
            nodes.insert("fresh".into(), serde_json::json!({ "last_seen": now }));
            nodes.insert(
                "stale".into(),
                serde_json::json!({ "last_seen": now - PROPAGATION_NODE_TTL_SECS - 60 }),
            );
            nodes.insert("missing_ts".into(), serde_json::json!({}));
        }
        state.trim_propagation_nodes();
        let nodes = state.discovered_propagation_nodes.lock().unwrap();
        assert!(nodes.contains_key("fresh"));
        assert!(!nodes.contains_key("stale"));
        assert!(!nodes.contains_key("missing_ts"));
    }

    #[test]
    fn trim_propagation_nodes_keeps_float_timestamps_from_announces() {
        let state = make_state();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        {
            let mut nodes = state.discovered_propagation_nodes.lock().unwrap();
            nodes.insert("fresh".into(), serde_json::json!({ "last_seen": now }));
            nodes.insert(
                "stale".into(),
                serde_json::json!({ "last_seen": now - PROPAGATION_NODE_TTL_SECS as f64 - 60.0 }),
            );
        }
        state.trim_propagation_nodes();
        let nodes = state.discovered_propagation_nodes.lock().unwrap();
        assert!(nodes.contains_key("fresh"));
        assert!(!nodes.contains_key("stale"));
    }

    #[test]
    fn trim_propagation_nodes_keeps_static_placeholders() {
        let state = make_state();
        {
            let mut nodes = state.discovered_propagation_nodes.lock().unwrap();
            nodes.insert(
                "static".into(),
                serde_json::json!({ "last_seen": 0.0, "static": true }),
            );
            nodes.insert("unknown".into(), serde_json::json!({ "last_seen": 0.0 }));
        }
        state.trim_propagation_nodes();
        let nodes = state.discovered_propagation_nodes.lock().unwrap();
        assert!(nodes.contains_key("static"));
        assert!(!nodes.contains_key("unknown"));
    }

    #[test]
    fn trim_propagation_nodes_caps_size() {
        let state = make_state();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        {
            let mut nodes = state.discovered_propagation_nodes.lock().unwrap();
            for i in 0..(MAX_DISCOVERED_PROPAGATION_NODES + 50) {
                nodes.insert(
                    format!("node_{i:04}"),
                    serde_json::json!({ "last_seen": now - 100 + i as u64 }),
                );
            }
        }
        state.trim_propagation_nodes();
        let nodes = state.discovered_propagation_nodes.lock().unwrap();
        assert_eq!(nodes.len(), MAX_DISCOVERED_PROPAGATION_NODES);
        for i in 0..50 {
            assert!(
                !nodes.contains_key(&format!("node_{i:04}")),
                "node_{i:04} should be evicted"
            );
        }
        for i in
            (MAX_DISCOVERED_PROPAGATION_NODES + 50 - 50)..(MAX_DISCOVERED_PROPAGATION_NODES + 50)
        {
            assert!(
                nodes.contains_key(&format!("node_{i:04}")),
                "node_{i:04} should remain"
            );
        }
    }

    #[test]
    fn reset_message_send_times_on_resume_advances_stale_timestamps() {
        let state = make_state();
        let ancient = 1.0_f64;
        {
            let mut times = state.message_send_times.lock().unwrap();
            times.insert("msg-a".into(), ancient);
            times.insert("msg-b".into(), ancient);
            times.insert("msg-c".into(), ancient);
        }

        let reset = state.reset_message_send_times_on_resume();
        assert_eq!(reset, 3);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let times = state.message_send_times.lock().unwrap();
        for (k, v) in times.iter() {
            assert!(
                *v > ancient,
                "{k}: expected reset ({v}) > ancient ({ancient})"
            );
            assert!(
                (now - *v).abs() < 5.0,
                "{k}: expected reset ({v}) within 5s of now ({now})"
            );
        }
    }

    #[test]
    fn reset_message_send_times_on_resume_noop_when_empty() {
        let state = make_state();
        assert_eq!(state.reset_message_send_times_on_resume(), 0);
        assert!(state.message_send_times.lock().unwrap().is_empty());
    }
}
