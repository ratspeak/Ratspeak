//! System commands: version, startup, lifecycle, unread, restart/shutdown,
//! factory reset, clear-* ops.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::commands::shared::{active_rns_config_dir, remove_stored_file_refs};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::{active_identity_id, sanitize_announced_display_name};
use crate::state::AppState;

fn app_display_version() -> &'static str {
    let tagged_version = option_env!("RATSPEAK_DISPLAY_VERSION")
        .or(option_env!("GITHUB_REF_NAME"))
        .map(|value| value.strip_prefix('v').unwrap_or(value))
        .filter(|value| {
            value
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_digit())
        });

    tagged_version.unwrap_or(env!("CARGO_PKG_VERSION"))
}

#[tauri::command]
pub async fn api_version() -> AppResult<Value> {
    Ok(json!({ "version": app_display_version(), "name": "Ratspeak" }))
}

#[tauri::command]
pub async fn api_startup_progress(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let mut stage = state.get_startup_stage();
    let mut locked = state.hw_locked_hash();
    let mut kind = None;
    if let Some(h) = locked.as_deref() {
        let dir = state.config.data_dir.join("identities").join(h);
        if dir.join("identity.hwid").exists() {
            kind = Some("hardware");
        } else if dir.join("identity.enc").exists() {
            kind = Some("passcode");
        } else {
            // The locked identity was removed locally while the backend was
            // still alive. Do not surface a passcode prompt for an orphaned
            // hardware lock; clear it and let setup/status drive the UI.
            state.set_hw_locked(None);
            state.set_hw_last_error(None);
            locked = None;
            if stage == "hw_locked" {
                state.set_startup_stage("ready");
                stage = "ready".to_string();
            }
        }
    }
    Ok(json!({
        "stage": stage,
        "hw_locked": locked,
        "hw_locked_kind": kind,
    }))
}

#[tauri::command]
pub async fn api_setup_status(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identities = db::spawn_db(state.db.clone(), |p| db::get_all_identities(&p))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "db task panicked");
            Default::default()
        });
    Ok(json!({ "needs_setup": identities.is_empty() }))
}

#[derive(Deserialize)]
pub struct SetupCompleteArgs {
    #[serde(default)]
    pub display_name: Option<String>,
}

#[tauri::command]
pub async fn api_setup_complete(
    state: State<'_, Arc<AppState>>,
    args: SetupCompleteArgs,
) -> AppResult<Value> {
    let display_name = sanitize_announced_display_name(args.display_name.as_deref().unwrap_or(""))
        .map_err(AppError::bad_request)?;

    // Idempotent: api_setup_complete is called twice (create, then "Connect" with
    // a display name). Generate a recoverable (mnemonic-derived) identity only on
    // the FIRST call; later calls load the existing one and just update the name.
    // The mnemonic is returned on the first call and persisted with the same
    // at-rest protection as the identity key for later re-display.
    let existing = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
        .await
        .ok()
        .flatten()
        .and_then(|i| i.get("hash").and_then(|v| v.as_str()).map(str::to_string));

    let (identity_hash, mnemonic) = if let Some(hash) = existing {
        (hash, None)
    } else {
        let (m, key) = match ratspeak_runtime::generate_recoverable_key() {
            Ok(v) => v,
            Err(e) => return Ok(json!({ "ok": false, "error": e })),
        };
        let write_dir = state.config.data_dir.clone();
        let write_db = state.db.clone();
        let write_nick = display_name.clone();
        let write = tokio::task::spawn_blocking(move || {
            crate::lxmf::LxmfManager::import_identity_to_data_dir(
                &write_dir,
                &key,
                &write_nick,
                &write_db,
            )
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|_| AppError::internal("setup write task panicked"))?;
        let (hash, _lxmf) = match write {
            Ok(t) => t,
            Err(e) => {
                return Ok(
                    json!({ "ok": false, "error": format!("Failed to create identity: {e}") }),
                );
            }
        };
        let ih = hash.clone();
        let _ = db::spawn_db(state.db.clone(), move |p| db::set_active_identity(&p, &ih)).await;
        // Persist the phrase so the first identity can re-display it later too.
        let seed_dir = state.config.data_dir.join("identities").join(&hash);
        if let Err(e) = ratspeak_runtime::vault::store_plaintext_seed(&seed_dir, &m) {
            tracing::warn!(error = %e, "could not store recovery-phrase sidecar at setup");
        }
        (hash, Some(m))
    };

    match crate::lxmf::LxmfManager::load_or_create(
        &state.config.data_root,
        Some(&identity_hash),
        None,
    ) {
        Ok(mgr) => {
            let lxmf_hash = mgr.lxmf_hash.clone();
            let dn = if display_name.is_empty() {
                format!("!Ratspeak.org-{}", &lxmf_hash[..6.min(lxmf_hash.len())])
            } else {
                display_name.clone()
            };
            let (ih, lh, dnc) = (identity_hash.clone(), lxmf_hash.clone(), dn.clone());
            db::spawn_db(state.db.clone(), move |p| {
                db::save_identity(&p, &ih, &lh, "Default", &dnc)
            })
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "setup save_identity task panicked");
                Default::default()
            });
            state.set_lxmf(mgr);
            Ok(json!({
                "ok": true,
                "identity_hash": identity_hash,
                "lxmf_hash": lxmf_hash,
                "display_name": dn,
                "mnemonic": mnemonic,
            }))
        }
        Err(e) => Ok(json!({ "ok": false, "error": format!("Failed to load identity: {e}") })),
    }
}

#[tauri::command]
pub async fn api_setup_restart(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    state.set_startup_stage("checking");
    if let Ok(mut sig) = state.session_shutdown.write() {
        *sig = rns_runtime::lifecycle::ShutdownSignal::new();
    }
    let data_dir = state.config.data_root.clone();
    let st: Arc<AppState> = Arc::clone(&state);
    tokio::spawn(async move {
        crate::init_rns_lxmf(Arc::clone(&st), data_dir).await;
        crate::commands::ble::restore_ble_peer_if_requested(st).await;
    });
    Ok(json!({ "message": "Initializing..." }))
}

#[derive(Deserialize)]
pub struct SetForegroundArgs {
    #[serde(default)]
    pub foreground: Option<bool>,
}

#[tauri::command]
#[tracing::instrument(
    level = "debug",
    name = "command.api_set_foreground",
    skip_all,
    fields(foreground = args.foreground),
)]
pub async fn api_set_foreground(
    state: State<'_, Arc<AppState>>,
    args: SetForegroundArgs,
) -> AppResult<Value> {
    let fg = args.foreground.unwrap_or(true);
    set_foreground_state(&state, fg);
    Ok(json!({ "foreground": fg }))
}

pub fn set_foreground_state(state: &Arc<AppState>, fg: bool) {
    let was = state
        .is_foreground
        .swap(fg, std::sync::atomic::Ordering::Relaxed);
    if fg != was {
        state.foreground_changed.notify_waiters();
    }
    if fg && !was {
        tracing::info!("lifecycle: foreground resume — waking LXMF ticker + stats poll");
        state.request_poll_now();
        state.lxmf_notify.notify_one();
        // Reset send-time clock to avoid false timeouts after iOS suspend.
        let reset_count = state.reset_message_send_times_on_resume();
        if reset_count > 0 {
            tracing::info!(
                count = reset_count,
                "lifecycle: reset {} in-flight message timeout timers on resume",
                reset_count
            );
        }
        // Apple BLE: prune stale discovery state on resume.
        #[cfg(all(feature = "ble", any(target_os = "ios", target_os = "macos")))]
        rns_interface::ble_central_apple_connect::on_app_did_become_active();
    } else if !fg && was {
        tracing::info!("lifecycle: background — throttling begins");
        #[cfg(all(feature = "ble", any(target_os = "ios", target_os = "macos")))]
        rns_interface::ble_central_apple_connect::on_app_will_resign_active();
    }
}

#[tauri::command]
pub async fn api_unread_count(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity_id = active_identity_id(&state);
    let (total, senders): (i64, Vec<Value>) = if !identity_id.is_empty() {
        let id_for_db = identity_id.clone();
        let rows = db::spawn_db(state.db.clone(), move |p| {
            db::get_unread_breakdown(&p, &id_for_db)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "unread-count db task panicked");
            Default::default()
        });
        let total: i64 = rows.iter().map(|(_, _, c, _, _)| *c).sum();
        let senders = rows
            .into_iter()
            .map(|(hash, name, count, preview, ts)| {
                let short_preview: String = preview.chars().take(120).collect();
                json!({
                    "hash": hash,
                    "display_name": name,
                    "count": count,
                    "preview": short_preview,
                    "timestamp": ts,
                })
            })
            .collect();
        (total, senders)
    } else {
        (0, Vec::new())
    };
    let fg = state.is_foreground();
    let ble_peers = state
        .ble_peer_count
        .load(std::sync::atomic::Ordering::Relaxed);
    Ok(json!({
        "count": total,
        "foreground": fg,
        "ble_peer_count": ble_peers,
        "senders": senders,
    }))
}

#[tauri::command]
pub async fn api_system_restart(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let st: Arc<AppState> = Arc::clone(&state);
    tokio::spawn(async move {
        crate::restart_rns_lxmf(Arc::clone(&st)).await;
        crate::commands::ble::restore_ble_peer_if_requested(st).await;
    });
    Ok(json!({ "message": "Restarting..." }))
}

#[tauri::command]
pub async fn api_system_shutdown(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let st: Arc<AppState> = Arc::clone(&state);
    tokio::spawn(async move {
        crate::shutdown_rns_lxmf(&st).await;
    });
    Ok(json!({ "message": "Shutting down..." }))
}

#[tauri::command]
pub async fn api_database_stats(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let stats = db::spawn_db(state.db.clone(), |p| db::get_database_stats(&p))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "database_stats db task panicked");
            Default::default()
        });
    Ok(stats)
}

fn clear_cached_path_stats(state: &AppState) {
    let stats = {
        let mut guard = match state.last_stats.write() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let Some(stats) = guard.as_mut() else {
            return;
        };
        if let Some(obj) = stats.as_object_mut() {
            obj.insert("path_table".to_string(), json!([]));
            obj.insert("path_index".to_string(), json!({}));
            obj.insert("path_table_total".to_string(), json!(0));
            obj.insert("path_table_truncated".to_string(), json!(false));
        }
        stats.clone()
    };
    state.emit_to_all("stats_update", stats);
}

#[tauri::command]
pub async fn api_clear_paths(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let handle = {
        let rns = state.rns.read().ok();
        rns.as_ref()
            .and_then(|r| r.as_ref())
            .map(|mgr| mgr.handle.clone())
    };

    let mut cleared = 0i64;
    if let Some(handle) = handle {
        if let Some(rns_transport::messages::TransportQueryResponse::IntResult(n)) = handle
            .query_control(rns_transport::messages::TransportQuery::DropPathTable)
            .await
        {
            cleared += n;
        }
        if handle.instance_mode == rns_runtime::reticulum::InstanceMode::Client
            && let Some(rns_transport::messages::TransportQueryResponse::IntResult(n)) = handle
                .query_transport(rns_transport::messages::TransportQuery::DropPathTable)
                .await
        {
            cleared += n;
        }
        if let Some(rns_transport::messages::TransportQueryResponse::PathTable(entries)) = handle
            .query_control(rns_transport::messages::TransportQuery::GetPathTable)
            .await
        {
            for entry in entries {
                let _ = handle
                    .query_control(rns_transport::messages::TransportQuery::DropPath {
                        dest: entry.hash,
                    })
                    .await;
                cleared += 1;
            }
        }
    }

    if let Ok(mut paths) = state.known_path_hashes.lock() {
        paths.clear();
    }
    state
        .path_activity_baselined
        .store(false, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        mgr.replace_route_hops_from_path_table(&[]);
    }
    clear_cached_path_stats(&state);
    state.emit_to_all("paths_cleared", json!({ "cleared": cleared }));
    state.request_poll_now();

    Ok(json!({ "cleared": cleared }))
}

#[tauri::command]
pub async fn api_clear_announces(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let handle = {
        let rns = state.rns.read().ok();
        rns.as_ref()
            .and_then(|r| r.as_ref())
            .map(|mgr| mgr.handle.clone())
    };

    let mut recent_cleared = 0i64;
    if let Some(handle) = handle {
        if let Some(rns_transport::messages::TransportQueryResponse::IntResult(n)) = handle
            .query_transport(rns_transport::messages::TransportQuery::DropRecentAnnounces)
            .await
        {
            recent_cleared += n;
        }
        if handle.instance_mode == rns_runtime::reticulum::InstanceMode::Client
            && let Some(rns_transport::messages::TransportQueryResponse::IntResult(n)) = handle
                .query_control(rns_transport::messages::TransportQuery::DropRecentAnnounces)
                .await
        {
            recent_cleared += n;
        }
        let _ = handle
            .query_control(rns_transport::messages::TransportQuery::DropAnnounceQueues)
            .await;
    }

    if let Ok(mut announces) = state.announce_history.write() {
        announces.clear();
    }
    if let Ok(mut seen) = state.seen_announce_hashes.lock() {
        seen.clear();
    }
    state
        .announce_activity_baselined
        .store(false, std::sync::atomic::Ordering::Relaxed);

    let peers_cleared = db::spawn_db(state.db.clone(), |p| {
        db::clear_discovered_identity_activity(&p)
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "clear_announces db task panicked");
        0
    });
    state.emit_to_all(
        "announces_cleared",
        json!({
            "recent_cleared": recent_cleared,
            "peers_cleared": peers_cleared,
        }),
    );
    state.request_poll_now();

    Ok(json!({
        "recent_cleared": recent_cleared,
        "peers_cleared": peers_cleared,
    }))
}

#[tauri::command]
pub async fn api_clear_messages(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity_id = active_identity_id(&state);
    let id_for_db = identity_id.clone();
    let file_refs = db::spawn_db(state.db.clone(), move |p| {
        db::clear_all_messages(&p, &id_for_db)
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "clear_messages db task panicked");
        Default::default()
    });
    remove_stored_file_refs(&state.config.files_dir(), file_refs);
    Ok(json!(null))
}

#[tauri::command]
pub async fn api_clear_contacts(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity_id = active_identity_id(&state);
    let id1 = identity_id.clone();
    db::spawn_db(state.db.clone(), move |p| db::clear_all_contacts(&p, &id1))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "clear_contacts db task panicked");
            Default::default()
        });
    let id2 = identity_id;
    db::spawn_db(state.db.clone(), move |p| db::clear_all_blocked(&p, &id2))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "clear_blocked db task panicked");
            Default::default()
        });
    Ok(json!(null))
}

#[tauri::command]
#[tracing::instrument(level = "debug", name = "command.api_reset_database", skip_all)]
pub async fn api_reset_database(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity_id = active_identity_id(&state);
    let id1 = identity_id.clone();
    let file_refs = db::spawn_db(state.db.clone(), move |p| db::clear_all_messages(&p, &id1))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "reset_database clear_messages panicked");
            Default::default()
        });
    remove_stored_file_refs(&state.config.files_dir(), file_refs);
    let id2 = identity_id.clone();
    db::spawn_db(state.db.clone(), move |p| db::clear_all_contacts(&p, &id2))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "reset_database clear_contacts panicked");
            Default::default()
        });
    let id3 = identity_id;
    db::spawn_db(state.db.clone(), move |p| db::clear_all_blocked(&p, &id3))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "reset_database clear_blocked panicked");
            Default::default()
        });
    Ok(json!(null))
}

#[tauri::command]
#[tracing::instrument(level = "debug", name = "command.api_identity_reset", skip_all)]
pub async fn api_identity_reset(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity_id = active_identity_id(&state);

    #[cfg(feature = "lxst-voice")]
    {
        let app_state = state.inner().clone();
        crate::voice::shutdown_voice_service(&app_state).await;
    }

    if !identity_id.is_empty() {
        let id1 = identity_id.clone();
        let file_refs = db::spawn_db(state.db.clone(), move |p| db::clear_all_messages(&p, &id1))
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "identity_reset clear_messages panicked");
                Default::default()
            });
        remove_stored_file_refs(&state.config.files_dir(), file_refs);
        let id2 = identity_id.clone();
        db::spawn_db(state.db.clone(), move |p| db::clear_all_contacts(&p, &id2))
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "identity_reset clear_contacts panicked");
                Default::default()
            });
        let id3 = identity_id.clone();
        let del_res = db::spawn_db(state.db.clone(), move |p| {
            db::delete_identity(&p, &id3, true)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "identity_reset delete panicked");
            Err(format!("db task panicked: {e}"))
        });
        if let Err(e) = del_res {
            tracing::error!("Failed to delete identity during reset: {e}");
        }
    }

    let reset_data_dir = state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(|m| m.data_dir.clone()));
    if let Some(data_dir) = reset_data_dir {
        let _ = tokio::task::spawn_blocking(move || {
            std::fs::remove_dir_all(data_dir.join("identities")).ok();
            std::fs::remove_dir_all(data_dir.join("ratchets")).ok();
            std::fs::remove_file(data_dir.join("identity")).ok();
        })
        .await;
    }

    if let Ok(mut lxmf) = state.lxmf.lock() {
        *lxmf = None;
    }

    state.set_startup_stage("ready");
    state.emit_to_all("identity_reset", json!({ "restarting": true }));

    Ok(json!({ "message": "Identity deleted. Returning to setup..." }))
}

#[tauri::command]
pub async fn dismiss_alert(state: State<'_, Arc<AppState>>, index: i64) -> AppResult<Value> {
    if index < 0 {
        return Err(AppError::bad_request("invalid index"));
    }
    let mut alerts = state
        .alerts
        .lock()
        .map_err(|_| AppError::internal("alert store unavailable"))?;
    let idx = index as usize;
    if idx >= alerts.len() {
        return Err(AppError::bad_request("index out of range"));
    }
    alerts.remove(idx);
    Ok(json!(null))
}

#[tauri::command]
#[tracing::instrument(level = "debug", name = "command.api_factory_reset", skip_all)]
pub async fn api_factory_reset(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    // Capture config_dir before shutdown wipes RNS.
    let rns_config_dir = active_rns_config_dir(&state);
    let app_private_rns_config_dir = state
        .config
        .uses_app_private_rns_config_dir()
        .then(|| state.config.rns_config_dir.clone());

    #[cfg(feature = "lxst-voice")]
    {
        let app_state = state.inner().clone();
        crate::voice::shutdown_voice_service(&app_state).await;
    }

    // Drop LXMF without save_crypto_state (would rewrite the dir we delete).
    if let Ok(mut lxmf) = state.lxmf.lock() {
        let _ = lxmf.take();
    }

    state.emit_to_all("system_status", json!({ "status": "stopping" }));
    if let Ok(sig) = state.session_shutdown.read() {
        sig.trigger();
    }
    if let Ok(mut rns) = state.rns.write()
        && let Some(mgr) = rns.take()
    {
        mgr.shutdown();
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    db::spawn_db(state.db.clone(), |pool| {
        if let Ok(conn) = pool.get() {
            let tables = [
                "messages",
                "contacts",
                "identities",
                "settings",
                "connection_history",
                "reactions",
                "games",
                "app_sessions",
                "app_actions",
                "hidden_conversations",
                "blocked_contacts",
                "identity_activity",
            ];
            for table in &tables {
                let _ = conn.execute(&format!("DELETE FROM {}", table), []);
            }
            let _ = conn.execute(
                "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
                [],
            );
            let _ = conn.execute("VACUUM", []);
        }
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "factory_reset db wipe panicked");
        Default::default()
    });

    // Bulk fs wipe; keep off the runtime.
    let data_dir = state.config.data_dir.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let identities_dir = data_dir.join("identities");
        if identities_dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&identities_dir)
        {
            tracing::warn!("Factory reset: failed to remove identities dir: {e}");
        }
        let ratchet_dir = data_dir.join("ratchets");
        if ratchet_dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&ratchet_dir)
        {
            tracing::warn!("Factory reset: failed to remove ratchets dir: {e}");
        }
        let files_dir = data_dir.join("files");
        if files_dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&files_dir)
        {
            tracing::warn!("Factory reset: failed to remove files dir: {e}");
        }
        if let Some(rns_dir) = app_private_rns_config_dir
            && rns_dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&rns_dir)
        {
            tracing::warn!(
                "Factory reset: failed to remove app-private Reticulum config {}: {e}",
                rns_dir.display()
            );
        }
        if let Ok(entries) = std::fs::read_dir(&data_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "key")
                    && let Err(e) = std::fs::remove_file(&path)
                {
                    tracing::warn!(
                        "Factory reset: failed to remove key file {}: {e}",
                        path.display()
                    );
                }
                if path.file_name().is_some_and(|n| n == "identity")
                    && let Err(e) = std::fs::remove_file(&path)
                {
                    tracing::warn!("Factory reset: failed to remove identity file: {e}");
                }
            }
        }
    })
    .await;

    let storage_dir = rns_config_dir.join("storage");
    if storage_dir.exists()
        && let Err(e) = std::fs::remove_dir_all(&storage_dir)
    {
        tracing::warn!("Factory reset: failed to remove RNS storage dir: {e}");
    }

    if let Ok(mut log) = state.event_log.lock() {
        log.clear();
    }
    if let Ok(mut announces) = state.announce_history.write() {
        announces.clear();
    }
    if let Ok(mut alerts) = state.alerts.lock() {
        alerts.clear();
    }
    if let Ok(mut paths) = state.known_path_hashes.lock() {
        paths.clear();
    }
    state
        .path_activity_baselined
        .store(false, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut seen) = state.seen_announce_hashes.lock() {
        seen.clear();
    }
    state
        .announce_activity_baselined
        .store(false, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut times) = state.message_send_times.lock() {
        times.clear();
    }
    if let Ok(mut map) = state.msg_id_map.lock() {
        map.clear();
    }
    if let Ok(mut sig) = state.session_shutdown.write() {
        *sig = rns_runtime::lifecycle::ShutdownSignal::new();
    }
    state.set_startup_stage("ready");
    state.emit_to_all("system_status", json!({ "status": "stopped" }));
    state.emit_to_all("identity_reset", json!({ "restarting": true }));

    Ok(json!({ "message": "Factory reset complete. Returning to setup..." }))
}
