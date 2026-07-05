//! Shared application state. Narrowest sync primitive per field: `RwLock` for
//! read-heavy caches, `Mutex` for write-heavy maps, `AtomicBool` for single flags.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use ratspeak_core::{Emitter, NativeNotification, NativeNotifier};
use rns_runtime::lifecycle::ShutdownSignal;
use tokio::sync::watch;

use crate::config::DashboardConfig;
use crate::lxmf::LxmfManager;
use crate::rns::RnsManager;

pub use ratspeak_core::types::{
    LrgpMsgMeta, MAX_DISCOVERED_PROPAGATION_NODES, PROPAGATION_NODE_TTL_SECS,
};
pub use ratspeak_db::DbPool;

const INTERFACE_REANNOUNCE_SUPPRESSION_TTL: Duration = Duration::from_secs(120);

/// Uses `std::sync::{Mutex, RwLock}`, not tokio variants. Critical sections
/// must finish before `.await` or run in `spawn_blocking`
/// (`clippy::await_holding_lock` enforces this).
pub struct AppState {
    pub config: DashboardConfig,
    pub db: DbPool,
    pub startup_stage: RwLock<String>,
    pub event_log: Mutex<VecDeque<serde_json::Value>>,
    /// IPC fan-out — concrete impl is `TauriEmitter` in production builds and
    /// a no-op stub in headless tests. Set at construction; never re-assigned.
    pub emitter: Arc<dyn Emitter>,
    pub notifier: Arc<dyn NativeNotifier>,
    /// Keyed by dest_hash hex; IndexMap insertion-order drives FIFO eviction.
    pub announce_history: RwLock<IndexMap<String, serde_json::Value>>,
    pub alerts: Mutex<Vec<serde_json::Value>>,
    pub rns: RwLock<Option<RnsManager>>,
    pub lxmf: Mutex<Option<LxmfManager>>,
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
    pub identity_switch_lock: tokio::sync::Mutex<()>,
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

        Self {
            config,
            db,
            startup_stage: RwLock::new("starting".into()),
            event_log: Mutex::new(VecDeque::with_capacity(200)),
            emitter,
            notifier,
            announce_history: RwLock::new(IndexMap::new()),
            alerts: Mutex::new(Vec::new()),
            rns: RwLock::new(None),
            lxmf: Mutex::new(None),
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
            conversations_broadcast_pending: AtomicBool::new(false),
            last_refresh_request_at: Mutex::new(None),
            last_static_probe_at: Mutex::new(None),
            auto_active_node: RwLock::new(None),
            auto_failure_counts: Mutex::new(HashMap::new()),
            pn_parse_failures: AtomicU64::new(0),
            native_notifications_enabled: AtomicBool::new(initial_notifications_enabled),
            rns_config_lock: Mutex::new(()),
            identity_switch_lock: tokio::sync::Mutex::new(()),
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

    pub fn clear_identity_scoped_runtime_state(&self) {
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
        if let Ok(mut events) = self.event_log.lock() {
            events.clear();
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

    pub fn add_event(&self, event: serde_json::Value) {
        if !self.network_log_enabled.load(Ordering::Relaxed) {
            return;
        }

        let stored = {
            if let Ok(mut log) = self.event_log.lock() {
                if log.len() >= self.config.max_log_entries {
                    log.pop_front();
                }
                log.push_back(event.clone());
                true
            } else {
                false
            }
        };
        if stored {
            self.emit_to_all("event", event);
        }
    }

    pub fn get_recent_events(&self, count: usize) -> Vec<serde_json::Value> {
        if let Ok(log) = self.event_log.lock() {
            let skip = log.len().saturating_sub(count);
            log.iter().skip(skip).cloned().collect()
        } else {
            Vec::new()
        }
    }

    pub fn set_rns(&self, rns: RnsManager) {
        if let Ok(mut r) = self.rns.write() {
            *r = Some(rns);
        }
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
        if let Ok(mut l) = self.lxmf.lock() {
            *l = Some(lxmf);
        }
    }

    /// Levels: essential < standard < detailed.
    pub fn emit_network_event(
        &self,
        event_type: &str,
        message: &str,
        detail: &str,
        event_level: &str,
    ) {
        if !self.network_log_enabled.load(Ordering::Relaxed) {
            return;
        }

        let level_rank = |l: &str| -> u8 {
            match l {
                "essential" => 0,
                "standard" => 1,
                "detailed" => 2,
                _ => 1,
            }
        };

        let configured = self
            .network_log_level
            .read()
            .map(|l| l.clone())
            .unwrap_or_else(|_| "standard".into());

        let event_rank = level_rank(event_level);
        let configured_rank = level_rank(&configured);

        if event_rank > configured_rank {
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        self.emit_to_all(
            "network_event",
            serde_json::json!({
                "type": event_type,
                "message": message,
                "detail": detail,
                "timestamp": now,
                "level": event_level,
            }),
        );
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
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_STATE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct RecordingEmitter {
        events: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl ratspeak_core::Emitter for RecordingEmitter {
        fn emit(&self, event: &str, payload: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push((event.to_string(), payload));
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
    fn event_buffers_are_privacy_gated_by_default() {
        let emitter = Arc::new(RecordingEmitter::default());
        let state = make_state_with_emitter(emitter.clone());

        state.add_event(serde_json::json!({
            "timestamp": 0,
            "category": "system",
            "message": "should not collect",
        }));
        state.emit_network_event("message", "should not emit", "detail", "standard");

        assert!(state.get_recent_events(10).is_empty());
        assert!(emitter.events.lock().unwrap().is_empty());

        state.network_log_enabled.store(true, Ordering::Relaxed);
        state.add_event(serde_json::json!({
            "timestamp": 1,
            "category": "system",
            "message": "collected after opt-in",
        }));
        state.emit_network_event("message", "emitted after opt-in", "detail", "standard");

        assert_eq!(state.get_recent_events(10).len(), 1);
        let events = emitter.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "event");
        assert_eq!(events[1].0, "network_event");
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
