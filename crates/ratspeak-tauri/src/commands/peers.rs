//! Peers-list IPC: one-shot snapshot then `peer_updated` / `peer_removed`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tauri::State;

use crate::db;
use crate::error::AppResult;
use crate::state::AppState;

pub const PEER_RECENCY_SECS: f64 = 7.0 * 86400.0;

#[tauri::command]
pub async fn api_get_peers_snapshot(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let cutoff = now - PEER_RECENCY_SECS;
    let pool = state.db.clone();
    let identity_id = crate::helpers::active_identity_id(&state);
    let rows = db::spawn_db(pool, move |p| {
        db::get_peers_snapshot(&p, cutoff, &identity_id)
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "peers-snapshot db task panicked");
        Vec::new()
    });
    let json_rows: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "hash": r.hash,
                "identity_hash": r.identity_hash,
                "telephony_hash": ratspeak_runtime::telephony_hash_for_identity_hex(&r.identity_hash),
                "last_seen": r.last_seen,
                "first_seen": r.first_seen,
                "display_name": r.display_name,
                "is_contact": r.is_contact,
                "last_interface": r.last_interface,
                "services": r.services,
            })
        })
        .collect();
    Ok(json!(json_rows))
}
