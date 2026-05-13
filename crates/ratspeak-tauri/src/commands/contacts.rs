//! Contact list + block list reads/writes + transport blackhole controls.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::commands::shared::{
    broadcast_blackhole_update, filter_blackholed_dests, format_contacts_list, hex_to_array16,
    resolve_contact_identity_hash, snapshot_blackhole, transport_query,
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
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "contacts db task panicked");
        Default::default()
    });
    let result: Vec<Value> = contacts
        .into_iter()
        .map(|c| {
            json!({
                "hash": c.get("dest_hash"),
                "display_name": c.get("display_name"),
                "trust": c.get("trust"),
                "notes": c.get("notes"),
                "first_seen": c.get("first_seen"),
                "last_seen": c.get("last_seen"),
            })
        })
        .collect();
    Ok(json!(result))
}

#[tauri::command]
pub async fn api_blocked_contacts(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity_id = active_identity_id(&state);
    let id_for_db = identity_id.clone();
    let blocked = db::spawn_db(state.db.clone(), move |p| {
        db::get_blocked_contacts(&p, &id_for_db)
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "blocked-contacts db task panicked");
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
async fn emit_peer_delta_for(state: &Arc<AppState>, dest_hash: &str) {
    let pool = state.db.clone();
    let key = dest_hash.to_string();
    let identity_id = crate::helpers::active_identity_id(state);
    let resolved = db::spawn_db(pool, move |p| {
        db::get_peers_by_hashes(&p, &[key], &identity_id)
    })
    .await
    .unwrap_or_default();
    if let Some(row) = resolved.into_iter().next() {
        state.emit_to_all(
            "peers_updated",
            json!({
                "peers": [{
                    "hash": row.hash,
                    "identity_hash": row.identity_hash,
                    "last_seen": row.last_seen,
                    "first_seen": row.first_seen,
                    "display_name": row.display_name,
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
    let dest_hash = sanitize_text(&args.hash, 128);
    let display_name = args.display_name.as_deref().map(|s| sanitize_text(s, 64));

    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request(
            "Invalid identity hash. Must be 16-64 hex characters (0-9, a-f).",
        ));
    }

    let identity_id = active_identity_id(&state);
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
    let dest_hash = sanitize_text(&hash, 128);
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

#[tauri::command]
pub async fn block_contact(
    state: State<'_, Arc<AppState>>,
    args: BlockContactArgs,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&args.hash, 128);
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request("Invalid hash for blocking."));
    }

    let identity_id = active_identity_id(&state);
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
    if args.escalate_to_blackhole
        && let Some(input_bytes) = hex_to_array16(&dest_hash)
    {
        use rns_transport::messages::{TransportQuery, TransportQueryResponse};
        if let Some(identity_hash) =
            resolve_contact_identity_hash(&state, &dest_hash, input_bytes).await
        {
            let resp = transport_query(
                &state,
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
                broadcast_blackhole_update(&state).await;
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
    let dest_hash = sanitize_text(&args.hash, 128);
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request("Invalid hash for unblocking."));
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
            "DELETE FROM blocked_contacts WHERE dest_hash = ?1 AND identity_id = ?2",
            rusqlite::params![dh, id_c],
        )
        .ok();
        let contacts = db::get_all_contacts_conn(&conn, &id_c);
        format_contacts_list(&contacts)
    })
    .await
    .map_err(|_| AppError::internal("unblock_contact db task panicked"))?;

    let mut unblackholed = false;
    let mut pending_cleared = false;
    if args.also_remove_blackhole
        && let Some(input_bytes) = hex_to_array16(&dest_hash)
    {
        use rns_transport::messages::{TransportQuery, TransportQueryResponse};

        // Always clear the pending row first so the announce-handler retry
        // does not re-escalate after we just lifted.
        let dest_c = dest_hash.clone();
        let id_c = identity_id.clone();
        pending_cleared = db::spawn_db(state.db.clone(), move |p| {
            db::clear_pending_blackhole(&p, &dest_c, &id_c)
        })
        .await
        .unwrap_or(false);

        if let Some(identity_hash) =
            resolve_contact_identity_hash(&state, &dest_hash, input_bytes).await
        {
            let resp = transport_query(
                &state,
                TransportQuery::UnblackholeIdentity {
                    hash: identity_hash,
                },
            )
            .await;
            unblackholed = matches!(resp, Some(TransportQueryResponse::BoolResult(true)));
        }

        // Legacy cleanup: pre-fix builds stored the LXMF dest-hash bytes as if
        // they were an identity hash. Try removing under the raw input too —
        // harmless no-op when no such entry exists.
        let legacy_resp = transport_query(
            &state,
            TransportQuery::UnblackholeIdentity { hash: input_bytes },
        )
        .await;
        unblackholed =
            unblackholed || matches!(legacy_resp, Some(TransportQueryResponse::BoolResult(true)));

        if unblackholed || pending_cleared {
            broadcast_blackhole_update(&state).await;
        }
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
    emit_peer_delta_for(&state, &dest_hash).await;
    crate::commands::messaging::broadcast_conversations(Arc::clone(&state));
    Ok(json!({
        "hash": dest_hash,
        "unblackholed": unblackholed,
        "pending_cleared": pending_cleared,
    }))
}

/// Same shape as `blackhole_update` broadcast.
#[tauri::command]
pub async fn get_blackhole(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let entries = snapshot_blackhole(&state).await;
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
