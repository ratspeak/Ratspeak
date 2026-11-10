//! Per-aspect announce handlers (`lxmf.delivery`, `lxmf.propagation`).
//! Cross-cutting announce work (history, crypto cache, contact-name refresh)
//! still runs in the poll loop.
use std::sync::Arc;
use std::time::Duration;

use rns_identity::destination::Destination;
use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::reticulum::{AnnounceSubscription, ReticulumHandle};
use rns_transport::messages::{AnnounceHandlerEvent, PathTableRpcEntry};

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
    rns_handle: &ReticulumHandle,
    shutdown: ShutdownSignal,
) {
    let Some(mut subscription) = subscribe_with_retry(rns_handle, LXMF_DELIVERY_APP_NAME).await
    else {
        return;
    };

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.wait() => break,
                ev = subscription.recv() => match ev {
                    Some(event) => {
                        process_delivery_announce(&state, event).await;
                        state.request_announce_poll();
                    }
                    None => break,
                },
            }
        }
        close_subscription(&mut subscription, LXMF_DELIVERY_APP_NAME).await;
    });
}

/// Register the lxmf.propagation handler and spawn the per-event processor.
pub async fn spawn_lxmf_propagation_handler(
    state: Arc<AppState>,
    rns_handle: &ReticulumHandle,
    shutdown: ShutdownSignal,
) {
    let Some(mut subscription) = subscribe_with_retry(rns_handle, LXMF_PROPAGATION_APP_NAME).await
    else {
        return;
    };

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.wait() => break,
                ev = subscription.recv() => match ev {
                    Some(event) => {
                        process_propagation_announce(&state, event).await;
                        state.request_announce_poll();
                    }
                    None => break,
                },
            }
        }
        close_subscription(&mut subscription, LXMF_PROPAGATION_APP_NAME).await;
    });
}

/// Register the lxst.telephony handler and map announces onto their associated
/// LXMF peer rows. This keeps the visible peers list service-aware without
/// inserting standalone NomadNet or propagation-node destinations.
pub async fn spawn_lxst_telephony_handler(
    state: Arc<AppState>,
    rns_handle: &ReticulumHandle,
    shutdown: ShutdownSignal,
) {
    let Some(mut subscription) = subscribe_with_retry(rns_handle, LXST_TELEPHONY_ASPECT).await
    else {
        return;
    };

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.wait() => break,
                ev = subscription.recv() => match ev {
                    Some(event) => {
                        process_lxst_telephony_announce(&state, event).await;
                        state.request_announce_poll();
                    }
                    None => break,
                },
            }
        }
        close_subscription(&mut subscription, LXST_TELEPHONY_ASPECT).await;
    });
}

/// Install one exact owned announce subscription, retrying transient startup
/// failures without ever taking aspect-wide deregistration authority.
async fn subscribe_with_retry(
    rns_handle: &ReticulumHandle,
    aspect: &'static str,
) -> Option<AnnounceSubscription> {
    for attempt in 0..REGISTER_ATTEMPTS {
        match rns_handle
            .subscribe_announces_with_capacity(Some(aspect.to_string()), true, HANDLER_CHANNEL_CAP)
            .await
        {
            Ok(subscription) => {
                tracing::debug!(aspect, "announce subscription registered");
                return Some(subscription);
            }
            Err(error) => {
                tracing::warn!(
                    aspect,
                    attempt = attempt + 1,
                    %error,
                    reason = "registration_failed",
                    "announce subscription registration failed; retrying"
                );
                tokio::time::sleep(REGISTER_RETRY_DELAY).await;
            }
        }
    }
    tracing::error!(
        aspect,
        "announce subscription registration: giving up after retries — aspect-driven updates disabled for this session"
    );
    None
}

async fn close_subscription(subscription: &mut AnnounceSubscription, aspect: &'static str) {
    let dropped_events = subscription.dropped_events();
    if dropped_events > 0 {
        tracing::warn!(
            aspect,
            dropped_events,
            "announce subscription omitted events under backpressure"
        );
    }
    if let Err(error) = subscription.close().await {
        tracing::warn!(aspect, %error, "failed to close announce subscription");
    }
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

    // The announce payload is already validated by Reticulum. Mutate all
    // related in-memory state in one short manager critical section, then
    // persist its immutable delta through the serialized writer.
    let (identity_changed, ratchet_changed, router_changed) = state
        .lxmf
        .lock()
        .ok()
        .and_then(|mut lxmf| {
            lxmf.as_mut().map(|mgr| {
                let (identity_changed, ratchet_changed) = event
                    .public_key
                    .as_ref()
                    .map(|public_key| {
                        mgr.update_remote_crypto(&hash_hex, public_key, event.ratchet.as_ref())
                    })
                    .unwrap_or((false, false));
                let router_changed = event.app_data.as_deref().is_some_and(|bytes| {
                    mgr.update_lxmf_announce_app_data(
                        event.destination_hash,
                        rns_identity::name_hash::name_hash(LXMF_DELIVERY_APP_NAME),
                        Some(bytes),
                    )
                });
                (identity_changed, ratchet_changed, router_changed)
            })
        })
        .unwrap_or((false, false, false));
    if identity_changed || ratchet_changed || router_changed {
        let ratchet_hashes = if ratchet_changed {
            vec![hash_hex.clone()]
        } else {
            Vec::new()
        };
        if let Err(error) = crate::lxmf_persistence::persist_current_delta(
            state,
            identity_changed,
            &ratchet_hashes,
            router_changed,
            "delivery_announce",
        )
        .await
        {
            tracing::warn!(%error, "delivery announce persistence failed");
        }
    }

    let iface = refresh_lxmf_route_cache_and_lookup_iface(state, event.destination_hash).await;

    let triggered = match state.lxmf.lock() {
        Ok(mut lxmf) => lxmf
            .as_mut()
            .filter(|mgr| mgr.has_live_direct_route(event.destination_hash, now_f64()))
            .map_or(0, |mgr| {
                mgr.router
                    .trigger_outbound_for_delivery_announce(event.destination_hash)
            }),
        Err(_) => 0,
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
            dest = %crate::short_id(&hash_hex),
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
            dest = %crate::short_id(&hex::encode(event.destination_hash)),
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
            dest = %crate::short_id(&hex::encode(event.destination_hash)),
            lxmf_dest = %crate::short_id(&lxmf_dest_hex),
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
                dest = %crate::short_id(&hash_hex),
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
                    dest = %crate::short_id(&hash_hex),
                    reason = "parse_failed",
                    app_data_len = bytes.len(),
                    "lxmf.propagation announce dropped: app_data did not parse as PN format"
                );
                return;
            }
        },
    };

    let (identity_changed, ratchet_changed, router_changed) = state
        .lxmf
        .lock()
        .ok()
        .and_then(|mut lxmf| {
            lxmf.as_mut().map(|mgr| {
                let (identity_changed, ratchet_changed) = event
                    .public_key
                    .as_ref()
                    .map(|public_key| {
                        mgr.update_remote_crypto(&hash_hex, public_key, event.ratchet.as_ref())
                    })
                    .unwrap_or((false, false));
                let router_changed = mgr.update_lxmf_announce_app_data(
                    event.destination_hash,
                    rns_identity::name_hash::name_hash(LXMF_PROPAGATION_APP_NAME),
                    event.app_data.as_deref(),
                );
                (identity_changed, ratchet_changed, router_changed)
            })
        })
        .unwrap_or((false, false, false));
    if identity_changed || ratchet_changed || router_changed {
        let ratchet_hashes = if ratchet_changed {
            vec![hash_hex.clone()]
        } else {
            Vec::new()
        };
        if let Err(error) = crate::lxmf_persistence::persist_current_delta(
            state,
            identity_changed,
            &ratchet_hashes,
            router_changed,
            "propagation_announce",
        )
        .await
        {
            tracing::warn!(%error, "propagation announce persistence failed");
        }
    }

    let inserted = if let (Some(app_data), Ok(mut registry)) = (
        event.app_data.as_deref(),
        state.discovered_propagation_nodes.lock(),
    ) {
        crate::propagation::insert_heard_node(
            &mut registry,
            event.destination_hash,
            &pn,
            app_data,
            Some(event.hops),
            timestamp,
        );
        true
    } else {
        false
    };

    if inserted {
        if let Some(app_data) = event.app_data.clone() {
            let heard = db::HeardPropagationNode {
                dest_hash: hash_hex.clone(),
                app_data,
                hops: Some(event.hops),
                last_seen: timestamp,
            };
            let _ = db::spawn_db(state.db.clone(), move |p| {
                db::save_propagation_node(&p, &heard)
            })
            .await;
        }
        crate::propagation::mark_relay_path_success(state, event.destination_hash);
        state.trim_propagation_nodes();
        crate::propagation::maybe_reselect_on_announce(state).await;
        let triggered = match (event.app_data.as_deref(), state.lxmf.lock()) {
            (Some(app_data), Ok(mut lxmf)) => lxmf.as_mut().map_or(0, |mgr| {
                mgr.router.trigger_outbound_for_propagation_node_announce(
                    event.destination_hash,
                    app_data,
                )
            }),
            _ => 0,
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
    let entries = crate::transport_observation::local_path_table(&tx).await?;
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

    #[test]
    fn owned_subscription_aspects_are_complete_and_distinct() {
        let aspects = [
            LXMF_DELIVERY_APP_NAME,
            LXMF_PROPAGATION_APP_NAME,
            LXST_TELEPHONY_ASPECT,
        ];
        assert_eq!(aspects.len(), 3);
        assert_ne!(aspects[0], aspects[1]);
        assert_ne!(aspects[0], aspects[2]);
        assert_ne!(aspects[1], aspects[2]);
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

    fn state_on(pool: &db::DbPool) -> Arc<AppState> {
        let tmp = tempfile::tempdir().unwrap().keep();
        crate::lxmf::initialize_storage_namespace(&tmp).unwrap();
        Arc::new(AppState::new(
            ratspeak_core::config::DashboardConfig::from_env_and_defaults(tmp),
            pool.clone(),
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ))
    }

    fn inbox_app_data(name: &str) -> Vec<u8> {
        use rmpv::Value;
        let announce = Value::Array(vec![
            Value::Boolean(false),
            Value::from(1_700_000_000),
            Value::Boolean(true),
            Value::from(256),
            Value::from(10_240),
            Value::Array(vec![Value::from(16), Value::from(3), Value::from(18)]),
            Value::Map(vec![(
                Value::from(1),
                Value::Binary(name.as_bytes().to_vec()),
            )]),
        ]);
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, &announce).unwrap();
        bytes
    }

    #[tokio::test]
    async fn an_announce_keeps_what_using_the_node_taught_us() {
        let pool = r2d2::Pool::builder()
            .max_size(1)
            .build(r2d2_sqlite::SqliteConnectionManager::memory())
            .unwrap();
        db::init_schema(&pool).unwrap();
        let state = state_on(&pool);
        let node = [0x3c; 16];
        let node_hex = hex::encode(node);
        let announce = |name: &str, hops| AnnounceHandlerEvent {
            destination_hash: node,
            hops,
            app_data: Some(inbox_app_data(name)),
            ..event_with_path_response(false)
        };
        process_propagation_announce(&state, announce("Old name", 3)).await;
        crate::propagation::mark_relay_transaction_success(&state, node, "sync_ok");
        crate::propagation::mark_relay_transaction_success(&state, node, "deposit_ok");
        let before = state.discovered_propagation_nodes.lock().unwrap()[&node_hex].clone();
        let before_fingerprint = crate::propagation::card_fingerprint(&state, true);

        process_propagation_announce(&state, announce("New name", 1)).await;
        let after = state.discovered_propagation_nodes.lock().unwrap()[&node_hex].clone();
        assert_eq!(after["display_name"], "New name");
        assert_eq!(after["hops"], 1);
        for kept in ["last_sync_success", "last_deposit_success"] {
            assert!(
                after[kept].as_f64().is_some(),
                "{kept} survives an announce"
            );
            assert_eq!(after[kept], before[kept], "{kept}");
        }
        assert!(
            after["last_success"].as_f64() >= before["last_success"].as_f64(),
            "an announce shows the node is reachable"
        );

        *state.auto_active_node.write().unwrap() = Some(node);
        let status = crate::propagation::get_status_payload(&state);
        assert_eq!(
            status["active_node"]["last_sync_success"], after["last_sync_success"],
            "the card reads the selected node's history from the status"
        );
        assert!(status.get("network_available").is_some());
        assert_ne!(
            crate::propagation::card_fingerprint(&state, true),
            before_fingerprint,
            "a newly heard node changes what the card shows"
        );
    }

    #[tokio::test]
    async fn a_heard_inbox_node_is_listed_again_after_a_restart() {
        let pool = r2d2::Pool::builder()
            .max_size(1)
            .build(r2d2_sqlite::SqliteConnectionManager::memory())
            .unwrap();
        db::init_schema(&pool).unwrap();
        let node = [0x5a; 16];
        let node_hex = hex::encode(node);
        let before = state_on(&pool);
        process_propagation_announce(
            &before,
            AnnounceHandlerEvent {
                destination_hash: node,
                hops: 2,
                app_data: Some(inbox_app_data("Home inbox")),
                ..event_with_path_response(false)
            },
        )
        .await;
        let heard = before.discovered_propagation_nodes.lock().unwrap()[&node_hex].clone();

        let stale = [0x6b; 16];
        let window = crate::state::PROPAGATION_NODE_TTL_SECS as f64;
        let bundled = crate::static_nodes::load()
            .first()
            .map(|node| node.hash)
            .expect("the app ships bundled inbox nodes");
        for (hash, last_seen) in [(stale, now_f64() - window - 60.0), (bundled, now_f64())] {
            db::save_propagation_node(
                &pool,
                &db::HeardPropagationNode {
                    dest_hash: hex::encode(hash),
                    app_data: inbox_app_data("Elsewhere"),
                    hops: Some(1),
                    last_seen,
                },
            );
        }

        let after = state_on(&pool);
        crate::propagation::seed_static_nodes(&after);
        crate::propagation::restore_heard_nodes(&after).await;
        let registry = after.discovered_propagation_nodes.lock().unwrap();
        let restored = &registry[&node_hex];
        for field in [
            "display_name",
            "hops",
            "last_seen",
            "stamp_cost",
            "transfer_limit_kb",
        ] {
            assert_eq!(restored[field], heard[field], "{field}");
        }
        assert_eq!(restored["display_name"], "Home inbox");
        assert!(
            !registry.contains_key(&hex::encode(stale)),
            "a node silent past the discovery window stays gone"
        );
        assert_ne!(
            registry[&hex::encode(bundled)]["display_name"],
            "Elsewhere",
            "bundled nodes keep their own entry"
        );
        drop(registry);
        assert_eq!(
            db::load_propagation_nodes(&pool, 0.0, 10)
                .unwrap()
                .iter()
                .map(|node| node.dest_hash.as_str())
                .filter(|hash| *hash == hex::encode(stale))
                .count(),
            0,
            "the stale row is deleted"
        );
    }
}
