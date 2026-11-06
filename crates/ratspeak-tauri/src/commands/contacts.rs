//! Contact list + block list reads/writes + transport blackhole controls.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::commands::shared::{
    TransportSender, broadcast_blackhole_update, filter_blackholed_dests, format_contacts_list,
    hex_to_array16, resolve_contact_identity_hash, snapshot_blackhole, snapshot_blackhole_on,
    transport_query, transport_query_on, transport_sender,
};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::{active_identity_id, sanitize_text, validate_hex};
use crate::state::AppState;

#[tauri::command]
pub async fn api_contacts(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity_id = active_identity_id(&state);
    let id_for_db = identity_id.clone();
    let contacts = db::spawn_db(state.db.clone(), move |p| {
        db::get_all_contacts(&p, &id_for_db)
    })
    .await
    .unwrap_or_else(|_| {
        tracing::error!(reason = "task_panicked", "contacts db task panicked");
        Default::default()
    });
    Ok(json!(format_contacts_list(&contacts)))
}

#[tauri::command]
pub async fn api_blocked_contacts(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity_id = active_identity_id(&state);
    let id_for_db = identity_id.clone();
    let blocked = db::spawn_db(state.db.clone(), move |p| {
        db::get_blocked_contacts(&p, &id_for_db)
    })
    .await
    .unwrap_or_else(|_| {
        tracing::error!(
            reason = "task_panicked",
            "blocked-contacts db task panicked"
        );
        Default::default()
    });

    // Build the dest-bytes list from rows, parsing hex once.
    let dest_bytes_list: Vec<[u8; 16]> = blocked
        .iter()
        .filter_map(|r| {
            r.get("hash")
                .and_then(|h| h.as_str())
                .and_then(hex_to_array16)
        })
        .collect();
    // Active blackholes for these dests (transport composes dest→identity→table).
    let mut active_set = filter_blackholed_dests(&state, dest_bytes_list).await;

    // Fallback for persisted blackholes after path/announce cache loss. Once a
    // peer is blackholed, future announces from that identity are dropped, and
    // a restart can leave only the SQLite identity_activity map plus the
    // transport blackhole table. The block-list shield still needs to surface
    // that network-level block so unblock can lift it.
    let blocked_hashes: Vec<String> = blocked
        .iter()
        .filter_map(|r| r.get("hash").and_then(|h| h.as_str()).map(str::to_string))
        .collect();
    let identity_by_dest = {
        let hashes = blocked_hashes.clone();
        db::spawn_db(state.db.clone(), move |p| {
            db::identity_hashes_for_dests(&p, &hashes)
        })
        .await
        .unwrap_or_default()
    };
    if !identity_by_dest.is_empty() {
        let blackholed_ids: std::collections::HashSet<String> = snapshot_blackhole(&state)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| {
                entry
                    .get("hash")
                    .and_then(|h| h.as_str())
                    .map(str::to_string)
            })
            .collect();
        for (dest, identity_hash) in identity_by_dest {
            if blackholed_ids.contains(&identity_hash) {
                active_set.insert(dest);
            }
        }
    }

    // Pending escalations (queued but waiting on first announce).
    let id_for_pending = identity_id.clone();
    let pending_rows = db::spawn_db(state.db.clone(), move |p| {
        db::list_pending_blackholes_for_identity(&p, &id_for_pending)
    })
    .await
    .unwrap_or_default();
    let pending_set: std::collections::HashSet<String> =
        pending_rows.into_iter().map(|r| r.dest_hash).collect();

    let decorated: Vec<Value> = blocked
        .into_iter()
        .map(|mut row| {
            let hash = row
                .get("hash")
                .and_then(|h| h.as_str())
                .unwrap_or("")
                .to_string();
            let is_network_blocked = active_set.contains(&hash);
            let is_blackhole_pending = pending_set.contains(&hash);
            if let Some(obj) = row.as_object_mut() {
                obj.insert("is_network_blocked".to_string(), json!(is_network_blocked));
                obj.insert(
                    "is_blackhole_pending".to_string(),
                    json!(is_blackhole_pending),
                );
            }
            row
        })
        .collect();
    Ok(json!(decorated))
}

#[derive(Deserialize)]
pub struct AddContactArgs {
    pub hash: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// Emit single-row `peers_updated` if visible, else `peer_removed`.
pub(crate) async fn emit_peer_delta_for(state: &Arc<AppState>, dest_hash: &str) {
    let identity_id = crate::helpers::active_identity_id(state);
    let row = read_peer_delta(state, &identity_id, dest_hash).await;
    emit_peer_delta(state, dest_hash, row);
}

async fn read_peer_delta(
    state: &AppState,
    identity_id: &str,
    dest_hash: &str,
) -> Option<db::PeerRow> {
    let key = dest_hash.to_string();
    let identity_id = identity_id.to_string();
    db::spawn_db(state.db.clone(), move |p| {
        db::get_peers_by_hashes(&p, &[key], &identity_id)
    })
    .await
    .unwrap_or_default()
    .into_iter()
    .next()
}

fn emit_peer_delta(state: &AppState, dest_hash: &str, row: Option<db::PeerRow>) {
    if let Some(row) = row {
        state.emit_to_all(
            "peers_updated",
            json!({
                "peers": [{
                    "hash": row.hash,
                    "identity_hash": row.identity_hash,
                    "last_seen": row.last_seen,
                    "first_seen": row.first_seen,
                    "display_name": row.display_name,
                    "profile_status": row.profile_status,
                    "is_contact": row.is_contact,
                    "last_interface": row.last_interface,
                    "services": row.services,
                }]
            }),
        );
    } else {
        state.emit_to_all("peer_removed", json!({ "hash": dest_hash }));
    }
}

#[tauri::command]
pub async fn add_contact(
    state: State<'_, Arc<AppState>>,
    args: AddContactArgs,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&args.hash, 128).to_ascii_lowercase();
    let display_name = args.display_name.as_deref().map(|s| sanitize_text(s, 64));

    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request(
            "Invalid identity hash. Must be 16-64 hex characters (0-9, a-f).",
        ));
    }

    let identity_id = active_identity_id(&state);
    if identity_id.is_empty() {
        return Err(AppError::conflict("no active identity"));
    }
    let dh = dest_hash.clone();
    let dn = display_name.clone();
    let id_c = identity_id.clone();
    let contacts_list = db::spawn_db(state.db.clone(), move |p| {
        let conn = match p.get() {
            Ok(c) => c,
            Err(_) => return Vec::<Value>::new(),
        };
        db::save_contact(&p, &dh, dn.as_deref(), "trusted", &id_c);
        let contacts = db::get_all_contacts_conn(&conn, &id_c);
        format_contacts_list(&contacts)
    })
    .await
    .map_err(|_| AppError::internal("add_contact db task panicked"))?;

    state.emit_to_all("contacts_update", json!(contacts_list));
    state.emit_to_all(
        "contact_added",
        json!({
            "hash": dest_hash,
            "display_name": display_name.clone().unwrap_or_else(|| dest_hash[..12.min(dest_hash.len())].to_string()),
        }),
    );
    emit_peer_delta_for(&state, &dest_hash).await;
    Ok(json!({ "hash": dest_hash, "display_name": display_name }))
}

#[tauri::command]
pub async fn remove_contact(state: State<'_, Arc<AppState>>, hash: String) -> AppResult<Value> {
    let dest_hash = sanitize_text(&hash, 128).to_ascii_lowercase();
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request("Invalid hash for removal."));
    }

    let identity_id = active_identity_id(&state);
    let dh = dest_hash.clone();
    let id_c = identity_id.clone();
    let contacts_list = db::spawn_db(state.db.clone(), move |p| {
        let conn = match p.get() {
            Ok(c) => c,
            Err(_) => return Vec::<Value>::new(),
        };
        conn.execute(
            "DELETE FROM contacts WHERE dest_hash = ?1 AND identity_id = ?2",
            rusqlite::params![dh, id_c],
        )
        .ok();
        let contacts = db::get_all_contacts_conn(&conn, &id_c);
        format_contacts_list(&contacts)
    })
    .await
    .map_err(|_| AppError::internal("remove_contact db task panicked"))?;

    state.emit_to_all("contacts_update", json!(contacts_list));
    emit_peer_delta_for(&state, &dest_hash).await;
    Ok(json!(null))
}

#[derive(Deserialize)]
pub struct BlockContactArgs {
    pub hash: String,
    /// Also blackhole at transport layer (node-global).
    #[serde(default)]
    pub escalate_to_blackhole: bool,
}

/// The identity a block or unblock is for, with its runtime's transport and
/// identity session, read together under the lifecycle lock. A switch waits
/// for this, and one asked for before a switch is refused; the network
/// queries then run off the lock, on that runtime's transport.
async fn contact_change_identity(
    state: &AppState,
    request_fence: crate::state::ActivityRequestFence,
) -> AppResult<(String, Option<TransportSender>, u64)> {
    let _identity = super::identity::acquire_identity_routine(state, request_fence).await?;
    Ok((
        active_identity_id(state),
        transport_sender(state),
        state.current_identity_session_generation(),
    ))
}

#[tauri::command]
pub async fn block_contact(
    state: State<'_, Arc<AppState>>,
    args: BlockContactArgs,
) -> AppResult<Value> {
    let request_fence = state.activity_request_fence();
    let dest_hash = sanitize_text(&args.hash, 128).to_ascii_lowercase();
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request("Invalid hash for blocking."));
    }
    let (identity_id, transport, generation) =
        contact_change_identity(&state, request_fence).await?;
    if identity_id.is_empty() {
        return Err(AppError::conflict("no active identity"));
    }
    let dh = dest_hash.clone();
    let id_c = identity_id.clone();
    let result = db::spawn_db(state.db.clone(), move |p| {
        let conn = p.get().ok()?;
        let display_name: String = conn
            .query_row(
                "SELECT display_name FROM contacts WHERE dest_hash = ?1 AND identity_id = ?2",
                rusqlite::params![dh, id_c],
                |row| row.get(0),
            )
            .unwrap_or_default();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        conn.execute(
            "INSERT OR REPLACE INTO blocked_contacts (dest_hash, identity_id, display_name, blocked_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![dh, id_c, display_name, now],
        ).ok();
        conn.execute(
            "DELETE FROM contacts WHERE dest_hash = ?1 AND identity_id = ?2",
            rusqlite::params![dh, id_c],
        )
        .ok();

        let contacts = db::get_all_contacts_conn(&conn, &id_c);
        let contacts_list = format_contacts_list(&contacts);
        drop(conn);
        // An earlier unblock's network lift must not undo this block.
        db::clear_pending_network_lift(&p, &id_c, &dh);
        Some((display_name, contacts_list))
    })
    .await
    .map_err(|_| AppError::internal("block_contact db task panicked"))?;

    let (display_name, contacts_list) =
        result.ok_or_else(|| AppError::database_unavailable("Contact DB unavailable"))?;

    // Manual reason + permanent TTL.
    //
    // The user typed an LXMF dest hash; the transport blackhole keys on
    // identity hash. Resolve via rsReticulum's recent_announces. If we have
    // never seen an announce for this contact, queue the request and let the
    // announce-handler escalate on first sighting.
    let mut blackholed = false;
    let mut blackhole_pending = false;
    let mut blackhole_entries = None;
    if let Some(input_bytes) = hex_to_array16(&dest_hash).filter(|_| args.escalate_to_blackhole) {
        use rns_transport::messages::{TransportQuery, TransportQueryResponse};
        if let Some(identity_hash) =
            resolve_contact_identity_hash(&state, transport.as_ref(), &dest_hash, input_bytes).await
        {
            let resp = transport_query_on(
                transport.as_ref(),
                TransportQuery::BlackholeIdentity {
                    hash: identity_hash,
                    ttl: None,
                    reason: rns_transport::blackhole::BlackholeReason::Manual,
                    reason_label: None,
                },
            )
            .await;
            blackholed = matches!(resp, Some(TransportQueryResponse::Ok));
            if blackholed {
                // Clear any leftover pending row from a prior attempt.
                let dest_c = dest_hash.clone();
                let id_c = identity_id.clone();
                db::spawn_db(state.db.clone(), move |p| {
                    db::clear_pending_blackhole(&p, &dest_c, &id_c)
                })
                .await
                .ok();
                blackhole_entries = snapshot_blackhole_on(transport.as_ref()).await;
            } else if resp.is_none() {
                // Its runtime stopped first: the network block waits for that
                // identity's next sighting of the contact.
                let dest_c = dest_hash.clone();
                let id_c = identity_id.clone();
                blackhole_pending = db::spawn_db(state.db.clone(), move |p| {
                    db::enqueue_pending_blackhole(&p, &dest_c, &id_c, None, None)
                })
                .await
                .unwrap_or(false);
            }
        } else {
            let dest_c = dest_hash.clone();
            let id_c = identity_id.clone();
            blackhole_pending = db::spawn_db(state.db.clone(), move |p| {
                db::enqueue_pending_blackhole(&p, &dest_c, &id_c, None, None)
            })
            .await
            .unwrap_or(false);
        }
    }

    // The block holds for its identity; once a switch has landed, the next
    // session's window hears nothing of it.
    let _identity = state.identity_switch_lock.lock_routine().await;
    if state.current_identity_session_generation() == generation {
        if let Some(entries) = blackhole_entries {
            state.emit_to_all("blackhole_update", json!({ "entries": entries }));
        }
        state.emit_to_all("contacts_update", json!(contacts_list));
        state.emit_to_all(
            "contact_blocked",
            json!({
                "ok": true,
                "hash": dest_hash,
                "display_name": display_name,
                "blackholed": blackholed,
                "blackhole_pending": blackhole_pending,
            }),
        );
        state.emit_to_all("peer_removed", json!({ "hash": dest_hash }));
        crate::commands::messaging::broadcast_conversations(Arc::clone(&state));
    }
    Ok(json!({
        "hash": dest_hash,
        "display_name": display_name,
        "blackholed": blackholed,
        "blackhole_pending": blackhole_pending,
    }))
}

#[derive(Deserialize)]
pub struct UnblockContactArgs {
    pub hash: String,
    /// Also lift transport-layer blackhole.
    #[serde(default)]
    pub also_remove_blackhole: bool,
}

#[tauri::command]
pub async fn unblock_contact(
    state: State<'_, Arc<AppState>>,
    args: UnblockContactArgs,
) -> AppResult<Value> {
    let request_fence = state.activity_request_fence();
    let dest_hash = sanitize_text(&args.hash, 128).to_ascii_lowercase();
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request("Invalid hash for unblocking."));
    }
    let (identity_id, transport, generation) =
        contact_change_identity(&state, request_fence).await?;
    let dh = dest_hash.clone();
    let id_c = identity_id.clone();
    let contacts_list = db::spawn_db(state.db.clone(), move |p| {
        let conn = match p.get() {
            Ok(c) => c,
            Err(_) => return Vec::<Value>::new(),
        };
        conn.execute(
            "DELETE FROM blocked_contacts WHERE dest_hash = ?1 AND identity_id = ?2",
            rusqlite::params![dh, id_c],
        )
        .ok();
        let contacts = db::get_all_contacts_conn(&conn, &id_c);
        format_contacts_list(&contacts)
    })
    .await
    .map_err(|_| AppError::internal("unblock_contact db task panicked"))?;

    let mut pending_cleared = false;
    let mut lift = None;
    let mut blackhole_entries = None;
    let lift_input = hex_to_array16(&dest_hash).filter(|_| args.also_remove_blackhole);
    if let Some(input_bytes) = lift_input {
        // Always clear the pending row first so the announce-handler retry
        // does not re-escalate after we just lifted.
        let dest_c = dest_hash.clone();
        let id_c = identity_id.clone();
        pending_cleared = db::spawn_db(state.db.clone(), move |p| {
            db::clear_pending_blackhole(&p, &dest_c, &id_c)
        })
        .await
        .unwrap_or(false);
        lift = ratspeak_runtime::blackhole::lift_network_block(
            &state,
            transport.as_ref(),
            &dest_hash,
            input_bytes,
        )
        .await;
        if lift == Some(true) || pending_cleared {
            blackhole_entries = snapshot_blackhole_on(transport.as_ref()).await;
        }
    }
    let peer = read_peer_delta(&state, &identity_id, &dest_hash).await;

    if let Some(input_bytes) = lift_input.filter(|_| lift.is_none()) {
        // Its runtime stopped first. While the identity is still the active
        // one, lift through its live runtime, read with it under the lock;
        // otherwise once one is up. The lift itself runs off the lock.
        let live = {
            let _identity = state.identity_switch_lock.lock_routine().await;
            (active_identity_id(&state) == identity_id)
                .then(|| transport_sender(&state))
                .flatten()
        };
        // Pending until it lands: a block made from now on cancels it like any
        // pending lift, and one made since keeps its network block.
        let dest_c = dest_hash.clone();
        let id_c = identity_id.clone();
        let wanted = db::spawn_db(state.db.clone(), move |p| {
            db::queue_pending_network_lift(&p, &id_c, &dest_c);
            let blocked = db::is_blocked(&p, &dest_c, &id_c);
            if blocked {
                db::clear_pending_network_lift(&p, &id_c, &dest_c);
            }
            !blocked
        })
        .await
        .unwrap_or(false);
        if let Some(live) = live.filter(|_| wanted) {
            lift = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                ratspeak_runtime::blackhole::lift_network_block(
                    &state,
                    Some(&live),
                    &dest_hash,
                    input_bytes,
                ),
            )
            .await
            .ok()
            .flatten();
            if lift.is_some() {
                let dest_c = dest_hash.clone();
                let id_c = identity_id.clone();
                db::spawn_db(state.db.clone(), move |p| {
                    db::clear_pending_network_lift(&p, &id_c, &dest_c)
                })
                .await
                .ok();
            }
        }
    }
    let unblackholed = lift == Some(true);
    // As a block: nothing reaches the next session's window.
    let _identity = state.identity_switch_lock.lock_routine().await;
    if state.current_identity_session_generation() == generation {
        if let Some(entries) = blackhole_entries {
            state.emit_to_all("blackhole_update", json!({ "entries": entries }));
        }
        state.emit_to_all("contacts_update", json!(contacts_list));
        state.emit_to_all(
            "contact_unblocked",
            json!({
                "ok": true,
                "hash": dest_hash,
                "unblackholed": unblackholed,
                "pending_cleared": pending_cleared,
            }),
        );
        emit_peer_delta(&state, &dest_hash, peer);
        crate::commands::messaging::broadcast_conversations(Arc::clone(&state));
    }
    Ok(json!({
        "hash": dest_hash,
        "unblackholed": unblackholed,
        "pending_cleared": pending_cleared,
    }))
}

/// Same shape as `blackhole_update` broadcast.
#[tauri::command]
pub async fn get_blackhole(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let entries = snapshot_blackhole(&state).await.unwrap_or_default();
    Ok(json!({ "entries": entries }))
}

/// Flushes every entry whose reason is not `Manual`.
#[tauri::command]
pub async fn clear_system_blackholes(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    use rns_transport::messages::{TransportQuery, TransportQueryResponse};
    let resp = transport_query(&state, TransportQuery::ClearSystemBlackholes).await;
    let cleared = match resp {
        Some(TransportQueryResponse::IntResult(n)) => n,
        _ => 0,
    };
    if cleared > 0 {
        broadcast_blackhole_update(&state).await;
    }
    Ok(json!({ "cleared": cleared }))
}

#[tauri::command]
pub async fn check_contact_status(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity_id = active_identity_id(&state);
    let known_hashes = state
        .known_path_hashes
        .lock()
        .map(|h| h.clone())
        .unwrap_or_default();
    let st: Arc<AppState> = Arc::clone(&state);
    let id_c = identity_id.clone();
    let status = tokio::task::spawn_blocking(move || {
        if let Ok(lxmf) = st.lxmf.lock() {
            lxmf.as_ref()
                .map(|mgr| mgr.check_contacts_identity_status(&st.db, &id_c, &known_hashes))
        } else {
            None
        }
    })
    .await
    .unwrap_or(None);
    Ok(status.unwrap_or(json!({})))
}

/// Drop every Manual blackhole entry whose identity hash is not currently
/// backed by a known announce. Useful after pre-fix builds populated the
/// table with LXMF-dest-hash bytes that can never match an announcer.
/// Returns `{ "purged": n }`. May also remove legit-but-unseen entries —
/// frontends should warn the user before invoking.
#[tauri::command]
pub async fn purge_unverified_blackholes(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    use rns_transport::messages::{TransportQuery, TransportQueryResponse};
    let resp = transport_query(&state, TransportQuery::PurgeUnverifiedBlackholes).await;
    let purged = match resp {
        Some(TransportQueryResponse::IntResult(n)) => n,
        _ => 0,
    };
    if purged > 0 {
        broadcast_blackhole_update(&state).await;
    }
    Ok(json!({ "purged": purged }))
}
