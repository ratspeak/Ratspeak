//! RNS runtime integration: init, path requests and path-table snapshots.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Map, Value, json};

use rns_interface::rnode::RNodeStartupOptions;
#[cfg(all(feature = "ble", target_os = "android"))]
use rns_runtime::interface_factory::BleRNodeInterfaceConfig;
use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::reticulum::{
    self, InitOptions, InstanceMode, ReticulumHandle, StartupRNodeRuntime,
};
use rns_transport::messages::{TransportMessage, TransportQuery, TransportQueryResponse};

use crate::rnode_activity::RNodeActivityRuntimeContext;
use crate::state::{ContinuousInstant, RNodeActivityOrigin};

pub const UI_PATH_TABLE_LIMIT: usize = 500;

/// Ratspeak's BLE RNode startup policy.
///
/// Official firmware leaves Bluetooth disabled after a cold boot unless the
/// host explicitly persists it. Selecting that behavior here keeps automatic
/// saved-interface reconnect product-owned while rsReticulum's generic default
/// and every non-BLE Ratspeak spawn remain wire-compatible.
pub fn ble_rnode_startup_options() -> RNodeStartupOptions {
    RNodeStartupOptions::default().with_persisted_bluetooth_enabled()
}

pub struct RnsManager {
    pub handle: ReticulumHandle,
    pub shutdown: ShutdownSignal,
    /// Exact, observation-only records for RNodes registered while this
    /// runtime initialized. This is a startup snapshot, not current inventory
    /// or teardown authority.
    startup_rnode_runtimes: Vec<StartupRNodeRuntime>,
    /// Exact RNode registrations whose lifecycle is supplied by the driver
    /// observer instead of inferred from generic transport statistics.
    /// Entries are session tombstones and live until this manager is dropped.
    activity_covered_rnodes: HashSet<rns_interface::traits::InterfaceId>,
    activity_session: Option<RNodeActivityOrigin>,
    started: ContinuousInstant,
}

/// What every Ratspeak network start passes to Reticulum.
pub(crate) fn init_options() -> InitOptions {
    let mut options = InitOptions::default();
    // One failed interface is shown as a warning; the others still start.
    options.keep_starting_after_interface_failure = true;
    // An unreachable server costs one connect timeout in all, not one each.
    options.start_interfaces_concurrently = true;
    options
}

impl RnsManager {
    /// Initialize RNS. `is_foreground` is shared with the transport actor and
    /// interfaces for background throttling.
    pub async fn init(
        config_dir: &str,
        socket_dir: Option<std::path::PathBuf>,
        is_foreground: Arc<AtomicBool>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::init_with_policy(
            config_dir,
            socket_dir,
            is_foreground,
            rns_runtime::shared_instance::InstancePolicy::Configured,
        )
        .await
    }

    pub async fn init_with_policy(
        config_dir: &str,
        socket_dir: Option<std::path::PathBuf>,
        is_foreground: Arc<AtomicBool>,
        policy: rns_runtime::shared_instance::InstancePolicy,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let started = ContinuousInstant::now();
        let shutdown = ShutdownSignal::new();
        let handle = reticulum::init_with_policy(
            Some(config_dir),
            socket_dir,
            shutdown.clone(),
            is_foreground,
            init_options(),
            ble_rnode_startup_options(),
            policy,
        )
        .await
        .map_err(|e| format!("RNS init failed: {e}"))?;
        if handle.instance_mode == InstanceMode::Client
            && let Err(error) = handle
                .query_control_result(TransportQuery::GetInterfaceStats)
                .await
        {
            handle.shutdown_and_wait().await;
            return Err(format!("Shared-instance control unavailable ({error}). Configure the other app and its RPC key in Settings › Developer › Reticulum on this device, or set Run by to Ratspeak to use your TCP interfaces.").into());
        }
        let startup_rnode_runtimes = handle.startup_rnode_runtimes();

        tracing::info!(
            "RNS initialized: mode={:?}, interfaces={}",
            handle.instance_mode,
            handle.interface_configs.len(),
        );
        handle
            .enable_on_network_discovery(Arc::new(
                lxmf_core::discovery_stamper::LxmfDiscoveryStamper::default(),
            ))
            .await;

        let activity_covered_rnodes = startup_rnode_runtimes
            .iter()
            .map(|runtime| runtime.interface_id)
            .collect();

        Ok(Self {
            handle,
            shutdown,
            startup_rnode_runtimes,
            activity_covered_rnodes,
            activity_session: None,
            started,
        })
    }

    /// When this runtime began to start, before it restored anything.
    pub fn started(&self) -> ContinuousInstant {
        self.started
    }

    /// Exact startup registrations retained for this runtime session.
    ///
    /// Absence does not prove that no RNode is configured, and these records
    /// must not be used as current inventory or lifecycle authority.
    pub fn startup_rnode_runtimes(&self) -> &[StartupRNodeRuntime] {
        &self.startup_rnode_runtimes
    }

    #[cfg(all(feature = "ble", target_os = "android"))]
    pub fn deferred_android_ble_rnodes(&self) -> Vec<BleRNodeInterfaceConfig> {
        self.handle.deferred_android_ble_rnodes()
    }

    pub(crate) fn bind_rnode_activity_session(&mut self, session: RNodeActivityOrigin) {
        self.activity_session = Some(session);
    }

    pub(crate) fn rnode_activity_runtime_context(
        &self,
        identity_generation: u64,
    ) -> Option<RNodeActivityRuntimeContext> {
        let origin = self.activity_session?;
        if origin.identity_generation() != identity_generation {
            return None;
        }
        Some(RNodeActivityRuntimeContext::new(
            self.handle.clone(),
            origin,
        ))
    }

    pub(crate) fn cover_rnode_activity_interface(
        &mut self,
        interface_id: rns_interface::traits::InterfaceId,
        origin: RNodeActivityOrigin,
    ) -> bool {
        if self.activity_session != Some(origin) {
            return false;
        }
        self.activity_covered_rnodes.insert(interface_id);
        true
    }

    pub(crate) fn is_rnode_activity_interface_covered(
        &self,
        interface_id: rns_interface::traits::InterfaceId,
        identity_generation: u64,
    ) -> bool {
        self.activity_session
            .is_some_and(|session| session.identity_generation() == identity_generation)
            && self.activity_covered_rnodes.contains(&interface_id)
    }

    pub(crate) fn owns_rnode_activity_session(&self, session: RNodeActivityOrigin) -> bool {
        self.activity_session == Some(session)
    }

    async fn query(&self, q: TransportQuery) -> Option<TransportQueryResponse> {
        self.handle.query_control(q).await
    }

    pub async fn request_path(&self, dest_hash: [u8; 16]) {
        let _ = self
            .handle
            .transport_tx
            .send(TransportMessage::RequestPath {
                destination_hash: dest_hash,
            })
            .await;
    }

    pub async fn drop_path(&self, dest_hash: [u8; 16]) {
        let _ = self
            .query(TransportQuery::DropPath { dest: dest_hash })
            .await;
    }

    pub async fn shutdown(&self) {
        self.handle.shutdown_and_wait().await;
    }
}

pub fn path_table_ui_snapshot(
    mut entries: Vec<rns_transport::messages::PathTableRpcEntry>,
) -> (Vec<Value>, usize, bool) {
    let total = entries.len();
    if total > UI_PATH_TABLE_LIMIT {
        entries.sort_by(|a, b| {
            b.timestamp
                .partial_cmp(&a.timestamp)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        entries.truncate(UI_PATH_TABLE_LIMIT);
    }
    let truncated = total > entries.len();
    let rows = entries
        .iter()
        .map(|e| {
            json!({
                "hash": hex::encode(e.hash),
                "via": e.via.filter(|via| *via != e.hash).map(hex::encode),
                "hops": e.hops,
                "expires": e.expires,
                "timestamp": e.timestamp,
                "interface": e.interface,
            })
        })
        .collect();
    (rows, total, truncated)
}

pub fn path_table_stats_snapshot(
    entries: Vec<rns_transport::messages::PathTableRpcEntry>,
) -> (Vec<Value>, Value, usize, bool) {
    let mut path_index = Map::with_capacity(entries.len());
    for entry in &entries {
        path_index.insert(
            hex::encode(entry.hash),
            json!({
                "via": entry.via.filter(|via| *via != entry.hash).map(hex::encode),
                "hops": entry.hops,
                "expires": entry.expires,
                "timestamp": entry.timestamp,
                "interface": entry.interface,
            }),
        );
    }

    let (rows, total, truncated) = path_table_ui_snapshot(entries);
    (rows, Value::Object(path_index), total, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rns_transport::messages::PathTableRpcEntry;

    fn path_entry(index: usize, timestamp: f64) -> PathTableRpcEntry {
        let mut hash = [0_u8; 16];
        hash[..8].copy_from_slice(&(index as u64).to_be_bytes());
        {
            let mut entry = PathTableRpcEntry::default();
            entry.hash = hash;
            entry.timestamp = timestamp;
            entry.via = None;
            entry.hops = 1;
            entry.expires = timestamp + 60.0;
            entry.value = None;
            entry.used = 0.0;
            entry.lr_cost = None;
            entry.interface = "test".to_string();
            entry.interface_id = 1;
            entry.interface_mode = rns_transport::constants::InterfaceMode::Full;
            entry.interface_role = rns_transport::messages::InterfaceRole::Normal;
            entry
        }
    }

    #[test]
    fn path_table_ui_snapshot_caps_newest_first_and_reports_total() {
        let entries: Vec<_> = (0..(UI_PATH_TABLE_LIMIT + 3))
            .map(|i| path_entry(i, i as f64))
            .collect();

        let (rows, total, truncated) = path_table_ui_snapshot(entries);

        assert_eq!(total, UI_PATH_TABLE_LIMIT + 3);
        assert!(truncated);
        assert_eq!(rows.len(), UI_PATH_TABLE_LIMIT);
        assert_eq!(
            rows[0].get("timestamp").and_then(|v| v.as_f64()),
            Some((UI_PATH_TABLE_LIMIT + 2) as f64)
        );
    }

    #[test]
    fn path_table_stats_snapshot_keeps_uncapped_peer_lookup() {
        let entries: Vec<_> = (0..(UI_PATH_TABLE_LIMIT + 3))
            .map(|i| path_entry(i, i as f64))
            .collect();

        let (rows, path_index, total, truncated) = path_table_stats_snapshot(entries);
        let index = path_index.as_object().expect("path index object");

        assert_eq!(total, UI_PATH_TABLE_LIMIT + 3);
        assert!(truncated);
        assert_eq!(rows.len(), UI_PATH_TABLE_LIMIT);
        assert_eq!(index.len(), UI_PATH_TABLE_LIMIT + 3);
        assert!(index.contains_key(&hex::encode([0_u8; 16])));
    }
}
