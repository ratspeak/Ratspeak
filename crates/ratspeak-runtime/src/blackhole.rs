//! Pending-blackhole escalation triggered by inbound announces.
//!
//! When the user requests "Block on the network" but we have not yet seen the
//! contact's announce, the LXMF dest hash cannot be resolved to an identity
//! hash — so we cannot call `BlackholeIdentity` immediately. The block intent
//! is persisted in `pending_blackholes` and replayed here on first sighting:
//! the announce-handler already has `event.identity_hash` populated, so no
//! second RPC is needed.

use std::sync::Arc;

use rns_transport::messages::{TransportMessage, TransportQuery, TransportQueryResponse};
use serde_json::json;

use crate::db;
use crate::state::AppState;

/// Send a `BlackholeIdentity` RPC and return the response, or `None` if the
/// transport actor is not running yet. Mirrors the helper in the tauri layer
/// but works against `AppState` directly so we can call it from the runtime.
async fn blackhole_identity(
    state: &AppState,
    identity_hash: [u8; 16],
    ttl: Option<f64>,
    reason_label: Option<String>,
) -> Option<TransportQueryResponse> {
    let tx = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.transport_tx.clone()))?;
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(TransportMessage::Rpc {
        query: TransportQuery::BlackholeIdentity {
            hash: identity_hash,
            ttl,
            reason: rns_transport::blackhole::BlackholeReason::Manual,
            reason_label,
        },
        response_tx: resp_tx,
    })
    .await
    .ok()?;
    resp_rx.await.ok()
}

/// On every `lxmf.delivery` announce, sweep `pending_blackholes` for the
/// announced dest hash. Each match is escalated using the identity hash
/// recovered from the announce (no extra RPC). Successful escalations clear
/// the pending row and emit `blackhole_promoted` so the UI can swap the
/// "pending" badge for the active "network blocked" pill.
pub async fn escalate_pending_if_present(
    state: &Arc<AppState>,
    dest_hash: [u8; 16],
    identity_hash: [u8; 16],
) {
    let dest_hex = hex::encode(dest_hash);
    let dest_for_query = dest_hex.clone();
    let pending = db::spawn_db(state.db.clone(), move |p| {
        db::list_pending_blackholes_by_dest(&p, &dest_for_query)
    })
    .await
    .unwrap_or_default();

    if pending.is_empty() {
        return;
    }

    let mut any_promoted = false;
    for row in pending {
        let resp = blackhole_identity(
            state,
            identity_hash,
            row.ttl_seconds,
            row.reason_label.clone(),
        )
        .await;
        if !matches!(resp, Some(TransportQueryResponse::Ok)) {
            tracing::warn!(
                dest = %crate::short_id(&row.dest_hash),
                identity_id = %crate::short_id(&row.identity_id),
                "pending blackhole escalation failed; row left in queue"
            );
            continue;
        }
        let id_for_db = row.identity_id.clone();
        let dest_for_db = row.dest_hash.clone();
        db::spawn_db(state.db.clone(), move |p| {
            db::clear_pending_blackhole(&p, &dest_for_db, &id_for_db);
        })
        .await
        .ok();
        state.emit_to_all(
            "blackhole_promoted",
            json!({
                "dest_hash": row.dest_hash,
                "identity_hash": hex::encode(identity_hash),
                "identity_id": row.identity_id,
            }),
        );
        any_promoted = true;
    }

    if any_promoted {
        // Surface a generic refresh so the system-wide blackhole list redraws
        // (active pill + verified flag now reflect this entry).
        state.emit_to_all("blackhole_update", json!({ "promoted": dest_hex }));
    }
}
