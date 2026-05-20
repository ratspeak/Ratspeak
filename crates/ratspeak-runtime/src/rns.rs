//! RNS runtime integration: init + stats queries.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::reticulum::{self, InstanceMode, ReticulumHandle};
use rns_transport::messages::{TransportMessage, TransportQuery, TransportQueryResponse};

pub const UI_PATH_TABLE_LIMIT: usize = 500;

pub struct RnsManager {
    pub handle: ReticulumHandle,
    pub shutdown: ShutdownSignal,
}

impl RnsManager {
    /// Initialize RNS. `is_foreground` is shared with the transport actor and
    /// interfaces for background throttling.
    pub async fn init(
        config_dir: &str,
        socket_dir: Option<std::path::PathBuf>,
        is_foreground: Arc<AtomicBool>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let shutdown = ShutdownSignal::new();
        let handle = reticulum::init(
            Some(config_dir),
            socket_dir,
            shutdown.clone(),
            is_foreground,
        )
        .await
        .map_err(|e| format!("RNS init failed: {e:?}"))?;

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

        Ok(Self { handle, shutdown })
    }

    async fn query(&self, q: TransportQuery) -> Option<TransportQueryResponse> {
        self.handle.query_control(q).await
    }

    pub async fn get_interface_stats(&self) -> Value {
        match self.query(TransportQuery::GetInterfaceStats).await {
            Some(TransportQueryResponse::InterfaceStats(stats)) => {
                let interfaces: Vec<Value> = stats
                    .iter()
                    .map(|s| {
                        json!({
                            "name": s.name,
                            "rxb": s.rx_bytes,
                            "txb": s.tx_bytes,
                            "online": s.online,
                            "bitrate": s.bitrate,
                            "mtu": s.mtu,
                            "mode": s.mode,
                            "role": s.role,
                            "announce_queue": s.announce_queue,
                            "held_announces": s.held_announces,
                            "incoming_announce_frequency": s.incoming_announce_frequency,
                            "outgoing_announce_frequency": s.outgoing_announce_frequency,
                            "incoming_pr_frequency": s.incoming_pr_frequency,
                            "outgoing_pr_frequency": s.outgoing_pr_frequency,
                            "burst_active": s.burst_active,
                            "burst_activated": s.burst_activated,
                            "pr_burst_active": s.pr_burst_active,
                            "pr_burst_activated": s.pr_burst_activated,
                            "announce_rate_target": s.announce_rate_target,
                            "announce_rate_grace": s.announce_rate_grace,
                            "announce_rate_penalty": s.announce_rate_penalty,
                            "announce_cap": s.announce_cap,
                            "ifac_size": s.ifac_size,
                            "tx_drops": s.tx_drops,
                        })
                    })
                    .collect();
                json!({ "interfaces": interfaces })
            }
            _ => json!({ "interfaces": [] }),
        }
    }

    pub async fn get_path_table(&self) -> Vec<Value> {
        match self.query(TransportQuery::GetPathTable).await {
            Some(TransportQueryResponse::PathTable(entries)) => path_table_ui_snapshot(entries).0,
            _ => vec![],
        }
    }

    pub async fn get_rate_table(&self) -> Vec<Value> {
        match self.query(TransportQuery::GetRateTable).await {
            Some(TransportQueryResponse::RateTable(entries)) => entries
                .iter()
                .map(|e| {
                    json!({
                        "hash": hex::encode(e.hash),
                        "rate": e.rate,
                        "last": e.last,
                        "rate_violations": e.rate_violations,
                        "blocked_until": e.blocked_until,
                        "samples": e.timestamps.len(),
                    })
                })
                .collect(),
            _ => vec![],
        }
    }

    pub async fn get_link_count(&self) -> i64 {
        match self.query(TransportQuery::GetLinkCount).await {
            Some(TransportQueryResponse::IntResult(n)) => n,
            _ => 0,
        }
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

    pub async fn build_stats_update(&self) -> Value {
        let interface_stats = self.get_interface_stats().await;
        let (path_table, path_index, path_table_total, path_table_truncated) = match self
            .query(TransportQuery::GetPathTable)
            .await
        {
            Some(TransportQueryResponse::PathTable(entries)) => path_table_stats_snapshot(entries),
            _ => (vec![], Value::Object(Map::new()), 0, false),
        };
        let rate_table = self.get_rate_table().await;
        let link_count = self.get_link_count().await;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        let connected = self.handle.instance_mode == InstanceMode::Client
            || self.handle.instance_mode == InstanceMode::Shared;

        json!({
            "timestamp": now,
            "connected": connected,
            "interface_stats": interface_stats,
            "path_table": path_table,
            "path_index": path_index,
            "path_table_total": path_table_total,
            "path_table_truncated": path_table_truncated,
            "rate_table": rate_table,
            "link_count": link_count,
        })
    }

    pub fn shutdown(&self) {
        self.shutdown.trigger();
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
                "via": e.via.map(hex::encode),
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
                "via": entry.via.map(hex::encode),
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
        PathTableRpcEntry {
            hash,
            timestamp,
            via: None,
            hops: 1,
            expires: timestamp + 60.0,
            interface: "test".to_string(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
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
