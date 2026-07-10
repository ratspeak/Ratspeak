//! Per-aspect announce handlers (`lxmf.delivery`, `lxmf.propagation`).
//! Cross-cutting announce work (history, crypto cache, contact-name refresh)
//! still runs in the poll loop.
use std::sync::Arc;
use std::time::Duration;

use rns_identity::destination::Destination;
use rns_runtime::lifecycle::ShutdownSignal;
use rns_transport::messages::{
    AnnounceHandlerEvent, PathTableRpcEntry, TransportMessage, TransportQuery,
    TransportQueryResponse,
};
use serde_json::json;
use tokio::sync::mpsc;

use crate::db;
use crate::state::AppState;
use ratspeak_core::{LXMF_DELIVERY_APP_NAME, LXMF_PROPAGATION_APP_NAME};

const HANDLER_CHANNEL_CAP: usize = 64;
const REGISTER_ATTEMPTS: u32 = 3;
const REGISTER_RETRY_DELAY: Duration = Duration::from_millis(500);
const LXST_TELEPHONY_ASPECT: &str = "lxst.telephony";

/// Register the lxmf.delivery handler and spawn the per-event processor.
pub async fn spawn_lxmf_delivery_handler(
    state: Arc<AppState>,
    transport_tx: mpsc::Sender<TransportMessage>,
    shutdown: ShutdownSignal,
) {
    let (htx, mut hrx) = mpsc::channel::<AnnounceHandlerEvent>(HANDLER_CHANNEL_CAP);
    if !register_with_retry(
        &transport_tx,
        Some(LXMF_DELIVERY_APP_NAME.to_string()),
        true,
        htx,
    )
    .await
    {
        return;
    }

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.wait() => break,
                ev = hrx.recv() => match ev {
                    Some(event) => {
                        process_delivery_announce(&state, event).await;
                        state.request_poll_now();
                    }
                    None => break,
                },
            }
        }
    });
}

/// Register the lxmf.propagation handler and spawn the per-event processor.
pub async fn spawn_lxmf_propagation_handler(
    state: Arc<AppState>,
    transport_tx: mpsc::Sender<TransportMessage>,
    shutdown: ShutdownSignal,
) {
    let (htx, mut hrx) = mpsc::channel::<AnnounceHandlerEvent>(HANDLER_CHANNEL_CAP);
    if !register_with_retry(
        &transport_tx,
        Some(LXMF_PROPAGATION_APP_NAME.to_string()),
        true,
        htx,
    )
    .await
    {
        return;
    }

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.wait() => break,
                ev = hrx.recv() => match ev {
                    Some(event) => {
                        process_propagation_announce(&state, event).await;
                        state.request_poll_now();
                    }
                    None => break,
                },
            }
        }
    });
}

/// Register the lxst.telephony handler and map announces onto their associated
/// LXMF peer rows. This keeps the visible peers list service-aware without
/// inserting standalone NomadNet or propagation-node destinations.
pub async fn spawn_lxst_telephony_handler(
    state: Arc<AppState>,
    transport_tx: mpsc::Sender<TransportMessage>,
    shutdown: ShutdownSignal,
) {
    let (htx, mut hrx) = mpsc::channel::<AnnounceHandlerEvent>(HANDLER_CHANNEL_CAP);
    if !register_with_retry(
        &transport_tx,
        Some(LXST_TELEPHONY_ASPECT.to_string()),
        true,
        htx,
    )
    .await
    {
        return;
    }

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.wait() => break,
                ev = hrx.recv() => match ev {
                    Some(event) => {
                        process_lxst_telephony_announce(&state, event).await;
                        state.request_poll_now();
                    }
                    None => break,
                },
            }
        }
    });
}

/// Send `RegisterAnnounceHandler` to the transport actor with retries to
/// tolerate the startup race before the actor is spawned.
async fn register_with_retry(
    transport_tx: &mpsc::Sender<TransportMessage>,
    aspect_filter: Option<String>,
    receive_path_responses: bool,
    callback_tx: mpsc::Sender<AnnounceHandlerEvent>,
) -> bool {
    for attempt in 0..REGISTER_ATTEMPTS {
        let cb = callback_tx.clone();
        let filter = aspect_filter.clone();
        match transport_tx
            .send(TransportMessage::RegisterAnnounceHandler {
                aspect_filter: filter,
                receive_path_responses,
                callback_tx: cb,
            })
            .await
        {
            Ok(()) => {
                tracing::debug!(
                    aspect = ?aspect_filter,
                    "announce-handler registered"
                );
                return true;
            }
            Err(e) => {
                tracing::warn!(
                    aspect = ?aspect_filter,
                    attempt = attempt + 1,
                    error = %e,
                    "announce-handler register failed; retrying"
                );
                tokio::time::sleep(REGISTER_RETRY_DELAY).await;
            }
        }
    }
    tracing::error!(
        aspect = ?aspect_filter,
        "announce-handler register: giving up after retries — aspect-driven updates disabled for this session"
    );
    false
}

/// `lxmf.delivery` per-event processing: activity tracking + peer batch emit.
async fn process_delivery_announce(state: &Arc<AppState>, event: AnnounceHandlerEvent) {
    // Pending-blackhole sweep: the announce already carries an identity hash
    // recovered from the validated payload, so we can escalate any queued
    // network-block requests for this dest immediately. No-op when nothing is
    // queued for this dest hash.
    if let Some(id_hash) = event.identity_hash {
        crate::blackhole::escalate_pending_if_present(state, event.destination_hash, id_hash).await;
    }

    let hash_hex = hex::encode(event.destination_hash);
    let display_name = event
        .app_data
        .as_ref()
        .map(|d| crate::extract_display_name(d))
        .filter(|s| !s.is_empty());
    let status = event
        .app_data
        .as_deref()
        .and_then(crate::lxmf::ratspeak_status_from_app_data);

    let lxmf_compression_support = event
        .app_data
        .as_deref()
        .and_then(crate::lxmf::lxmf_compression_support_db_value_from_app_data)
        .map(str::to_string);

    if let Some(bytes) = event.app_data.as_deref()
        && let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        let changed = mgr.update_lxmf_announce_app_data(
            event.destination_hash,
            rns_identity::name_hash::name_hash(LXMF_DELIVERY_APP_NAME),
            Some(bytes),
        );
        if changed {
            mgr.save_router_state();
        }
    }

    let iface = refresh_lxmf_route_cache_and_lookup_iface(state, event.destination_hash).await;

    let triggered = if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        if let Some(ref public_key) = event.public_key {
            mgr.update_remote_crypto(&hash_hex, public_key, event.ratchet.as_ref());
        }
        mgr.router
            .trigger_outbound_for_delivery_announce(event.destination_hash)
    } else {
        0
    };
    if triggered > 0 {
        state.lxmf_notify.notify_one();
    }

    if should_touch_peer_activity(&event) {
        let identity_hash_hex = event.identity_hash.map(hex::encode);
        let mut services = vec![db::PEER_SERVICE_LXMF_DELIVERY.to_string()];
        if let Some(bytes) = event.app_data.as_deref() {
            services.extend(
                crate::lxmf::ratspeak_capability_services_from_app_data(bytes)
                    .into_iter()
                    .map(str::to_string),
            );
        }

        let pool = state.db.clone();
        let update = db::IdentityActivityUpdate {
            dest_hash: hash_hex.clone(),
            timestamp: now_f64(),
            display_name,
            status,
            last_interface: iface,
            identity_hash: identity_hash_hex,
            services,
            clear_ratspeak_services: true,
            lxmf_compression_support,
        };
        db::spawn_db(pool, move |p| {
            db::touch_identity_activity_updates(&p, &[update]);
        })
        .await
        .expect("db task panicked");
    } else {
        tracing::debug!(
            dest = %hash_hex,
            "lxmf.delivery path response refreshed route data without touching peer last_seen"
        );
    }

    let pool = state.db.clone();
    let hashes = vec![hash_hex];
    let identity_id = crate::helpers::active_identity_id(state);
    let resolved = db::spawn_db(pool, move |p| {
        db::get_peers_by_hashes(&p, &hashes, &identity_id)
    })
    .await
    .unwrap_or_default();
    crate::emit_peers_batch(state, &resolved);
}

/// `lxst.telephony` announces carry an identity-level voice destination. The
/// Peers UI is LXMF-address centric, so mirror NomadNet's classification
/// approach and derive the associated `lxmf.delivery` hash from the announced
/// identity.
async fn process_lxst_telephony_announce(state: &Arc<AppState>, event: AnnounceHandlerEvent) {
    let identity_hash = event.identity_hash.or_else(|| {
        event
            .public_key
            .map(|public_key| rns_crypto::sha::truncated_hash(&public_key))
    });
    let Some(identity_hash) = identity_hash else {
        tracing::debug!(
            dest = %hex::encode(event.destination_hash),
            "lxst.telephony announce dropped: no identity hash"
        );
        return;
    };

    let lxmf_dest =
        Destination::hash_from_name_and_identity(LXMF_DELIVERY_APP_NAME, Some(&identity_hash));
    let lxmf_dest_hex = hex::encode(lxmf_dest);
    let identity_hash_hex = hex::encode(identity_hash);
    let iface = refresh_lxmf_route_cache_and_lookup_iface(state, event.destination_hash).await;
    if should_touch_peer_activity(&event) {
        let activity = vec![(lxmf_dest_hex.clone(), now_f64(), None, iface)];

        let pool = state.db.clone();
        let identity_hash_for_db = identity_hash_hex.clone();
        db::spawn_db(pool, move |p| {
            db::touch_identity_activity_for_service(
                &p,
                &activity,
                Some(&identity_hash_for_db),
                db::PEER_SERVICE_LXST_TELEPHONY,
            );
        })
        .await
        .expect("db task panicked");
    } else {
        tracing::debug!(
            dest = %hex::encode(event.destination_hash),
            lxmf_dest = %lxmf_dest_hex,
            "lxst.telephony path response refreshed route data without touching peer last_seen"
        );
    }

    let pool = state.db.clone();
    let hashes = vec![lxmf_dest_hex];
    let identity_id = crate::helpers::active_identity_id(state);
    let resolved = db::spawn_db(pool, move |p| {
        db::get_peers_by_hashes(&p, &hashes, &identity_id)
    })
    .await
    .unwrap_or_default();
    crate::emit_peers_batch(state, &resolved);
}

/// `lxmf.propagation` per-event processing. Drop on parse failure
/// (matches Python `LXMF.py:214`); preserve static badge + region when
/// upgrading an existing static-bundle entry.
async fn process_propagation_announce(state: &Arc<AppState>, event: AnnounceHandlerEvent) {
    use std::sync::atomic::Ordering;

    let hash_hex = hex::encode(event.destination_hash);
    let timestamp = now_f64();

    let pn = match event.app_data.as_ref() {
        None => {
            state.pn_parse_failures.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                dest = %hash_hex,
                reason = "no_app_data",
                "lxmf.propagation announce dropped: no app_data"
            );
            return;
        }
        Some(bytes) => match lxmf_core::handlers::parse_pn_announce_data(bytes) {
            Some(p) => p,
            None => {
                state.pn_parse_failures.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    dest = %hash_hex,
                    reason = "parse_failed",
                    app_data_len = bytes.len(),
                    "lxmf.propagation announce dropped: app_data did not parse as PN format"
                );
                return;
            }
        },
    };

    let display_name_from_announce = event
        .app_data
        .as_ref()
        .and_then(|d| lxmf_core::handlers::pn_name_from_app_data(d))
        .filter(|s| !s.is_empty());

    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        if let Some(ref public_key) = event.public_key {
            mgr.update_remote_crypto(&hash_hex, public_key, event.ratchet.as_ref());
        }
        mgr.router
            .set_stamp_cost(event.destination_hash, pn.stamp_cost);
    }

    let mut entry = json!({
        "hash": hash_hex,
        "hops": event.hops,
        "stamp_cost": pn.stamp_cost,
        "transfer_limit_kb": pn.transfer_limit,
        "last_seen": timestamp,
        "node_state": if pn.node_state { "enabled" } else { "disabled" },
    });

    let inserted = if let Ok(mut registry) = state.discovered_propagation_nodes.lock() {
        let key = hash_hex.clone();
        let existing = registry.get(&key).cloned();
        let static_meta = crate::static_nodes::node_for(&event.destination_hash);

        if let Some(obj) = entry.as_object_mut() {
            let preserved_static = existing
                .as_ref()
                .and_then(|v| v.get("static").and_then(|s| s.as_bool()))
                .unwrap_or_else(|| static_meta.is_some());
            let preserved_region = existing
                .as_ref()
                .and_then(|v| v.get("region").cloned())
                .or_else(|| {
                    static_meta
                        .and_then(|node| node.region.clone())
                        .map(|region| json!(region))
                })
                .unwrap_or(serde_json::Value::Null);
            let preserved_role = existing
                .as_ref()
                .and_then(|v| v.get("role").cloned())
                .or_else(|| {
                    static_meta
                        .and_then(|node| node.role.clone())
                        .map(|role| json!(role))
                })
                .unwrap_or(serde_json::Value::Null);
            let preserved_priority = existing
                .as_ref()
                .and_then(|v| v.get("priority").cloned())
                .or_else(|| static_meta.map(|node| json!(node.priority)))
                .unwrap_or(serde_json::Value::Null);
            let preserved_name = existing
                .as_ref()
                .and_then(|v| v.get("display_name").and_then(|s| s.as_str()))
                .map(String::from)
                .or_else(|| static_meta.map(|node| node.display_name.clone()));
            obj.insert("static".to_string(), json!(preserved_static));
            obj.insert("region".to_string(), preserved_region);
            obj.insert("role".to_string(), preserved_role);
            obj.insert("priority".to_string(), preserved_priority);
            let final_name = display_name_from_announce
                .clone()
                .or(preserved_name)
                .unwrap_or_else(|| format!("Inbox {}", &hash_hex[..8.min(hash_hex.len())]));
            obj.insert("display_name".to_string(), json!(final_name));
        }

        registry.insert(key, entry);
        true
    } else {
        false
    };

    if inserted {
        crate::propagation::mark_relay_path_success(state, event.destination_hash);
        state.trim_propagation_nodes();
        crate::propagation::maybe_reselect_on_announce(state).await;
        let triggered = if let Some(app_data) = event.app_data.as_deref()
            && let Ok(mut lxmf) = state.lxmf.lock()
            && let Some(mgr) = lxmf.as_mut()
        {
            mgr.router
                .trigger_outbound_for_propagation_node_announce(event.destination_hash, app_data)
        } else {
            0
        };
        if triggered > 0 {
            state.lxmf_notify.notify_one();
        }
    }
}

async fn refresh_lxmf_route_cache_and_lookup_iface(
    state: &Arc<AppState>,
    dest: [u8; 16],
) -> Option<String> {
    let tx = {
        let rns = state.rns.read().ok()?;
        rns.as_ref().map(|mgr| mgr.handle.transport_tx.clone())?
    };
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if tx
        .send(TransportMessage::Rpc {
            query: TransportQuery::GetPathTable,
            response_tx: resp_tx,
        })
        .await
        .is_err()
    {
        return None;
    }
    let entries = match resp_rx.await {
        Ok(TransportQueryResponse::PathTable(e)) => e,
        _ => return None,
    };
    refresh_lxmf_route_cache_from_path_table(state, &entries);
    entries.iter().find(|e| e.hash == dest).and_then(|e| {
        if e.interface.is_empty() {
            None
        } else {
            Some(e.interface.clone())
        }
    })
}

fn refresh_lxmf_route_cache_from_path_table(state: &Arc<AppState>, entries: &[PathTableRpcEntry]) {
    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        mgr.replace_route_hops_from_path_table(entries);
    }
}

fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn should_touch_peer_activity(event: &AnnounceHandlerEvent) -> bool {
    !event.is_path_response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lxmf_handler_registration_opts_into_path_responses() {
        for aspect in ["lxmf.delivery", "lxmf.propagation", LXST_TELEPHONY_ASPECT] {
            let (transport_tx, mut transport_rx) = mpsc::channel::<TransportMessage>(1);
            let (callback_tx, _callback_rx) = mpsc::channel::<AnnounceHandlerEvent>(1);

            assert!(
                register_with_retry(&transport_tx, Some(aspect.to_string()), true, callback_tx)
                    .await
            );

            let msg = transport_rx.recv().await.expect("registration message");
            match msg {
                TransportMessage::RegisterAnnounceHandler {
                    aspect_filter,
                    receive_path_responses,
                    ..
                } => {
                    assert_eq!(aspect_filter.as_deref(), Some(aspect));
                    assert!(receive_path_responses);
                }
                other => panic!("unexpected transport message: {other:?}"),
            }
        }
    }

    fn event_with_path_response(is_path_response: bool) -> AnnounceHandlerEvent {
        AnnounceHandlerEvent {
            destination_hash: [0x11; 16],
            identity_hash: Some([0x22; 16]),
            announce_packet_hash: [0x33; 32],
            is_path_response,
            hops: 10,
            app_data: None,
            public_key: None,
            ratchet: None,
            name_hash: [0x44; 10],
        }
    }

    #[test]
    fn path_responses_do_not_touch_peer_activity() {
        assert!(!should_touch_peer_activity(&event_with_path_response(true)));
        assert!(should_touch_peer_activity(&event_with_path_response(false)));
    }
}
