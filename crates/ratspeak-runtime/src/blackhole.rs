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

/// Send a `BlackholeIdentity` RPC to `transport` and return the response, or
/// `None` if that runtime's transport is gone.
async fn blackhole_identity(
    transport: &tokio::sync::mpsc::Sender<TransportMessage>,
    identity_hash: [u8; 16],
    ttl: Option<f64>,
    reason_label: Option<String>,
) -> Option<TransportQueryResponse> {
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    transport
        .send(TransportMessage::Rpc {
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

/// A runtime's transport, captured with the identity it runs.
pub type TransportSender = tokio::sync::mpsc::Sender<TransportMessage>;

/// One query to a runtime's transport; `None` once that runtime is gone.
pub async fn query_transport(
    transport: Option<&TransportSender>,
    query: TransportQuery,
) -> Option<TransportQueryResponse> {
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    transport?
        .send(TransportMessage::Rpc {
            query,
            response_tx: resp_tx,
        })
        .await
        .ok()?;
    resp_rx.await.ok()
}

/// Resolve a 16-byte LXMF destination or identity hash to the canonical
/// identity hash from the transport's recent announces.
pub async fn resolve_identity_hash(
    transport: Option<&TransportSender>,
    input: [u8; 16],
) -> Option<[u8; 16]> {
    match query_transport(transport, TransportQuery::ResolveIdentityHash { input }).await {
        Some(TransportQueryResponse::HashResult(hash)) => hash,
        _ => None,
    }
}

/// Resolve a contact's LXMF destination hash to its Reticulum identity hash.
/// The transport announce cache is preferred, but blackholing deliberately
/// drops paths and future announces, so contact unblock also needs the
/// persistent `identity_activity` mapping learned before the block.
pub async fn resolve_contact_identity_hash(
    state: &AppState,
    transport: Option<&TransportSender>,
    dest_hex: &str,
    input: [u8; 16],
) -> Option<[u8; 16]> {
    if let Some(identity_hash) = resolve_identity_hash(transport, input).await {
        return Some(identity_hash);
    }
    let dest = dest_hex.to_string();
    let identity_hex = db::spawn_db(state.db.clone(), move |p| {
        db::identity_hash_for_dest(&p, &dest)
    })
    .await
    .ok()
    .flatten()?;
    hex::decode(identity_hex).ok()?.try_into().ok()
}

/// Lift `dest_hex`'s network block through `transport`, by its identity and by
/// the raw destination older builds stored. Whether anything was lifted, or
/// `None` if that runtime answered nothing.
pub async fn lift_network_block(
    state: &AppState,
    transport: Option<&TransportSender>,
    dest_hex: &str,
    input: [u8; 16],
) -> Option<bool> {
    let lifted = |response: &Option<TransportQueryResponse>| {
        matches!(
            response,
            Some(TransportQueryResponse::OptBoolResult(Some(true)))
        )
    };
    let by_identity = match resolve_contact_identity_hash(state, transport, dest_hex, input).await {
        Some(hash) => {
            query_transport(transport, TransportQuery::UnblackholeIdentity { hash }).await
        }
        None => None,
    };
    // Older builds stored the LXMF destination bytes as if they were an
    // identity hash; removing that too is a no-op when nothing is there.
    let legacy = query_transport(
        transport,
        TransportQuery::UnblackholeIdentity { hash: input },
    )
    .await;
    (by_identity.is_some() || legacy.is_some()).then(|| lifted(&by_identity) || lifted(&legacy))
}

/// Lifts the network blocks `identity_id` was asked to remove while its
/// runtime was gone, through the runtime now up for it.
pub async fn apply_pending_network_lifts(state: &AppState, identity_id: &str) {
    let generation = state.current_identity_session_generation();
    if crate::helpers::active_identity_id(state) != identity_id {
        return;
    }
    let transport = state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.transport_tx.clone()));
    let identity = identity_id.to_string();
    let lifts = db::spawn_db(state.db.clone(), move |p| {
        db::pending_network_lifts(&p, &identity)
    })
    .await
    .unwrap_or_default();
    for dest in lifts {
        if state.current_identity_session_generation() != generation {
            return;
        }
        let Some(input) = hex::decode(&dest)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
        else {
            continue;
        };
        // A block made since the list was read keeps its network block.
        let identity = identity_id.to_string();
        let contact = dest.clone();
        let wanted = db::spawn_db(state.db.clone(), move |p| {
            db::pending_network_lifts(&p, &identity).contains(&contact)
                && !db::is_blocked(&p, &contact, &identity)
        })
        .await
        .unwrap_or(false);
        if !wanted {
            continue;
        }
        if lift_network_block(state, transport.as_ref(), &dest, input)
            .await
            .is_some()
        {
            let identity = identity_id.to_string();
            db::spawn_db(state.db.clone(), move |p| {
                db::clear_pending_network_lift(&p, &identity, &dest)
            })
            .await
            .ok();
        }
    }
}

/// On every `lxmf.delivery` announce, sweep `pending_blackholes` for the
/// announced dest hash. Each match is escalated using the identity hash
/// recovered from the announce (no extra RPC). Successful escalations clear
/// the pending row and emit `blackhole_promoted` so the UI can swap the
/// "pending" badge for the active "network blocked" pill.
///
/// Network blocks are per identity: only the live identity's requests go, to
/// the transport of the runtime it runs in. Another identity's wait for its
/// own runtime, and a switch mid-sweep leaves the rest for theirs.
pub async fn escalate_pending_if_present(
    state: &Arc<AppState>,
    dest_hash: [u8; 16],
    identity_hash: [u8; 16],
) {
    let generation = state.current_identity_session_generation();
    let Some((identity_id, transport)) = state.lxmf.lock().ok().and_then(|lxmf| {
        let mgr = lxmf.as_ref()?;
        Some((mgr.identity_hash.clone(), mgr.router.transport_tx.clone()?))
    }) else {
        return;
    };
    let dest_hex = hex::encode(dest_hash);
    let dest_for_query = dest_hex.clone();
    let pending = db::spawn_db(state.db.clone(), move |p| {
        db::list_pending_blackholes_by_dest(&p, &dest_for_query)
            .into_iter()
            .filter(|row| row.identity_id == identity_id)
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();

    let mut any_promoted = false;
    for row in pending {
        if state.current_identity_session_generation() != generation {
            break;
        }
        let resp = blackhole_identity(
            &transport,
            identity_hash,
            row.ttl_seconds,
            row.reason_label.clone(),
        )
        .await;
        if !matches!(
            resp,
            Some(TransportQueryResponse::OptBoolResult(Some(true) | None))
        ) {
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
        let _lifecycle = state.identity_switch_lock.lock_routine().await;
        if state.current_identity_session_generation() != generation {
            return;
        }
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
        let _lifecycle = state.identity_switch_lock.lock_routine().await;
        if state.current_identity_session_generation() == generation {
            // Surface a generic refresh so the system-wide blackhole list
            // redraws (active pill + verified flag now reflect this entry).
            state.emit_to_all("blackhole_update", json!({ "promoted": dest_hex }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound_pipeline_tests::pipeline_state;
    use rns_transport::messages::TransportMessage;

    // An announce escalates only the live identity's pending network
    // block, through the transport captured with that identity. Another
    // identity's block waits for its own runtime.
    #[tokio::test]
    async fn only_the_live_identitys_pending_blocks_escalate() {
        let (state, _) = pipeline_state();
        let live = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .identity_hash
            .clone();
        let other = "cd".repeat(16);
        let dest = [0xAB; 16];
        let dest_hex = hex::encode(dest);
        for owner in [&live, &other] {
            assert!(db::enqueue_pending_blackhole(
                &state.db, &dest_hex, owner, None, None
            ));
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        state
            .lxmf
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .router
            .transport_tx = Some(tx);
        let rpcs = tokio::spawn(async move {
            let mut count = 0;
            while let Some(message) = rx.recv().await {
                if let TransportMessage::Rpc { response_tx, .. } = message {
                    count += 1;
                    let _ = response_tx.send(TransportQueryResponse::OptBoolResult(Some(true)));
                }
            }
            count
        });
        escalate_pending_if_present(&state, dest, [0x42; 16]).await;
        state
            .lxmf
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .router
            .transport_tx = None;
        assert_eq!(
            rpcs.await.unwrap(),
            1,
            "one network block, the live identity's"
        );
        let left: Vec<_> = db::list_pending_blackholes_by_dest(&state.db, &dest_hex)
            .into_iter()
            .map(|row| row.identity_id)
            .collect();
        assert_eq!(left, [other]);
    }
}
