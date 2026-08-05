//! LRGP commands. `send_game_action` returns a `GameActionResult`;
//! state broadcasts go via `AppHandle::emit`.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::commands::shared::{SessionStateSave, emit_game_sessions, json_to_rmpv_map};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::{active_lxmf_hash, diagnostic_short_protocol_id, sanitize_text, validate_hex};
use crate::state::AppState;

fn short_hex(s: &str) -> &str {
    diagnostic_short_protocol_id(s).unwrap_or("invalid")
}

fn game_action_result_json(
    ok: bool,
    session_id: &str,
    command: &str,
    msg_id: Option<&str>,
    reason: Option<&str>,
) -> Value {
    json!({
        "ok": ok,
        "session_id": session_id,
        "command": command,
        "msg_id": msg_id,
        "reason": reason,
    })
}

fn game_manifest_json(manifest: &lrgp::app_base::AppManifest) -> Value {
    json!({
        "app_id": manifest.app_id,
        "version": manifest.version,
        "display_name": manifest.display_name,
        "icon": manifest.icon,
        "session_type": manifest.session_type,
        "max_players": manifest.max_players,
        "validation": manifest.validation,
        "actions": manifest.actions,
        "preferred_delivery": manifest.preferred_delivery,
        "ttl": manifest.ttl,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_abbreviation_rejects_noncanonical_game_identifiers() {
        assert_eq!(short_hex("0123456789abcdef0123456789abcdef"), "01234567");
        for malformed in [
            "human-label",
            "0123456789abcdef",
            "0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert_eq!(short_hex(malformed), "invalid");
        }
    }

    #[test]
    fn available_game_manifest_exposes_the_complete_extension_contract() {
        let router = lrgp::router::LrgpRouter::with_builtin_apps();
        let manifests = router.list_apps();
        assert!(!manifests.is_empty());

        for manifest in &manifests {
            let value = game_manifest_json(manifest);
            for key in [
                "app_id",
                "version",
                "display_name",
                "icon",
                "session_type",
                "max_players",
                "validation",
                "actions",
                "preferred_delivery",
                "ttl",
            ] {
                assert!(value.get(key).is_some(), "missing manifest field {key}");
            }
            assert_eq!(
                value.get("app_id").and_then(Value::as_str),
                Some(manifest.app_id.as_str())
            );
        }
    }
}

#[derive(Deserialize)]
pub struct SendGameActionArgs {
    pub dest_hash: String,
    pub app_id: String,
    pub command: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub payload: Option<Value>,
    #[serde(default)]
    pub delivery_method: Option<String>,
}

#[tauri::command]
pub async fn send_game_action(
    state: State<'_, Arc<AppState>>,
    args: SendGameActionArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let activity_origin = state_arc.activity_request_fence();
    let mut dest_hash = sanitize_text(&args.dest_hash, 128);
    let app_id = sanitize_text(&args.app_id, 64);
    let command = sanitize_text(&args.command, 64);
    let mut session_id = sanitize_text(&args.session_id, 128);
    let app_version = state_arc
        .lrgp_router
        .with_app(&app_id, |app| app.version())
        .unwrap_or(1);
    let delivery_pref = if args.delivery_method.is_some() {
        crate::commands::messaging::parse_delivery_preference(args.delivery_method.as_deref())
    } else {
        let manifest_preference = state_arc
            .lrgp_router
            .with_app(&app_id, |app| app.get_delivery_method(&command));
        crate::commands::messaging::parse_delivery_preference(manifest_preference.as_deref())
    };

    if !validate_hex(&dest_hash, 32, 32) || app_id.is_empty() || command.is_empty() {
        let payload =
            game_action_result_json(false, &session_id, &command, None, Some("invalid_params"));
        state_arc.emit_to_all("game_action_result", payload.clone());
        return Ok(payload);
    }
    dest_hash.make_ascii_lowercase();
    // TODO: Once Ratspeak capability discovery has been deployed long enough,
    // reject or warn for contacts that do not advertise `ratspeak.games`.
    crate::commands::messaging::validate_delivery_preference(&state_arc, delivery_pref)?;

    let _ =
        crate::commands::shared::hydrate_contact_identity_for_send(&state_arc, &dest_hash).await;
    crate::commands::messaging::ensure_propagation_ready_for_send(
        &state_arc,
        &dest_hash,
        delivery_pref,
        ratspeak_runtime::lxmf::DeliveryProfile::Lrgp,
        None,
    )
    .await?;

    // LRGP turn/winner fields keyed by LXMF hash.
    let identity_id = active_lxmf_hash(&state_arc);

    // Short-circuit terminal sessions to avoid duplicate envelopes.
    if !session_id.is_empty() {
        let sid = session_id.clone();
        let id_c = identity_id.clone();
        let existing = db::spawn_db(state_arc.db.clone(), move |p| {
            db::get_game_session(&p, &sid, &id_c)
        })
        .await
        .unwrap_or(None);
        if let Some(existing) = existing {
            let status = existing
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if matches!(status.as_str(), "declined" | "completed" | "expired") {
                let payload = game_action_result_json(
                    false,
                    &session_id,
                    &command,
                    None,
                    Some("session_terminal"),
                );
                state_arc.emit_to_all("game_action_result", payload.clone());
                return Ok(payload);
            }
        }
    }

    let payload_json = args.payload.clone().unwrap_or_else(|| json!({}));
    let payload = json_to_rmpv_map(&payload_json);

    // Pre-dispatch snapshot for rollback. `None` = fresh CHALLENGE.
    let snapshot = state_arc
        .lrgp_router
        .snapshot_session(&app_id, &session_id, &identity_id);

    let dispatch_result = state_arc.lrgp_router.dispatch_outgoing_to(
        &app_id,
        app_version,
        &command,
        &session_id,
        &payload,
        &identity_id,
        &dest_hash,
    );

    let prepared = match dispatch_result {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(
                target: "ttt_trace",
                step = "send.dispatch_err",
                reason = "dispatch_failed",
                "dispatch_outgoing returned error"
            );
            let reason = match &error {
                lrgp::errors::LrgpError::UnknownApp(_)
                | lrgp::errors::LrgpError::UnsupportedVersion { .. } => "unsupported_app".into(),
                lrgp::errors::LrgpError::UnauthorizedPeer { .. } => "unauthorized_sender".into(),
                lrgp::errors::LrgpError::SessionExpired(_) => "session_expired".into(),
                lrgp::errors::LrgpError::SessionNotFound(_) => "session_not_found".into(),
                lrgp::errors::LrgpError::SessionExists(_) => "session_exists".into(),
                lrgp::errors::LrgpError::ParticipantRequired => "invalid_params".into(),
                lrgp::errors::LrgpError::IllegalTransition { .. } => "invalid_state".into(),
                lrgp::errors::LrgpError::Validation { code, .. } => code.clone(),
                lrgp::errors::LrgpError::InvalidEnvelope(_)
                | lrgp::errors::LrgpError::EnvelopeTooLarge(_, _)
                | lrgp::errors::LrgpError::UnsupportedAction { .. } => "protocol_error".into(),
                _ => "dispatch_failed".into(),
            };
            if matches!(&error, lrgp::errors::LrgpError::SessionExpired(_))
                && let Some(Some(expired)) = state_arc.lrgp_router.with_app(&app_id, |app| {
                    app.get_session_record(&session_id, &identity_id)
                })
            {
                let _ = db::spawn_db(state_arc.db.clone(), move |pool| {
                    db::save_game_session(&pool, &expired);
                })
                .await;
                emit_game_sessions(&state_arc, &identity_id, Some(&dest_hash)).await;
            }
            let payload =
                game_action_result_json(false, &session_id, &command, None, Some(&reason));
            state_arc.emit_to_all("game_action_result", payload.clone());
            return Ok(payload);
        }
    };
    session_id = prepared.session_id;
    let envelope = prepared.envelope;
    let fallback_text = prepared.fallback_text;

    tracing::info!(
        target: "ttt_trace",
        step = "send.dispatch_ok",
        dest = %short_hex(&dest_hash),
        my = %short_hex(&identity_id),
        "dispatch_outgoing returned envelope"
    );

    let lrgp_fields = match lrgp::envelope::pack_lxmf_fields(&envelope) {
        Ok(fields) => fields,
        Err(_) => {
            let _ = state_arc.lrgp_router.rollback_outgoing(
                &app_id,
                &session_id,
                &identity_id,
                snapshot,
            );
            tracing::error!(
                reason = "field_pack_failed",
                "Validated LRGP envelope failed field packing"
            );
            let payload =
                game_action_result_json(false, &session_id, &command, None, Some("pack_failed"));
            state_arc.emit_to_all("game_action_result", payload.clone());
            return Ok(payload);
        }
    };

    // Commit the locally-applied state and exact resendable envelope as one
    // durable outbox transaction before handing it to LXMF. A process crash
    // can therefore leave either no action, or a complete action that the
    // existing Resend path can recover; it cannot leave a board transition
    // without its wire envelope.
    let envelope_mp = match lrgp::envelope::pack_to_bytes(&envelope) {
        Ok(packed) => packed,
        Err(_) => {
            let _ = state_arc.lrgp_router.rollback_outgoing(
                &app_id,
                &session_id,
                &identity_id,
                snapshot,
            );
            tracing::error!(
                reason = "byte_pack_failed",
                "Validated LRGP envelope failed byte packing"
            );
            let result =
                game_action_result_json(false, &session_id, &command, None, Some("pack_failed"));
            state_arc.emit_to_all("game_action_result", result.clone());
            return Ok(result);
        }
    };
    let mut durable_session = match state_arc
        .lrgp_router
        .with_app(&app_id, |app| {
            app.get_session_record(&session_id, &identity_id)
        })
        .flatten()
    {
        Some(session) => session,
        None => {
            let _ = state_arc.lrgp_router.rollback_outgoing(
                &app_id,
                &session_id,
                &identity_id,
                snapshot,
            );
            let result =
                game_action_result_json(false, &session_id, &command, None, Some("invalid_state"));
            state_arc.emit_to_all("game_action_result", result.clone());
            return Ok(result);
        }
    };
    durable_session.metadata.insert(
        "delivery_state".to_string(),
        serde_json::Value::String("pending".to_string()),
    );
    let action_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let payload_for_history = serde_json::to_string(&payload_json).unwrap_or_else(|_| "{}".into());
    let session_for_outbox = durable_session.clone();
    let command_for_outbox = command.clone();
    let sender_for_outbox = identity_id.clone();
    let envelope_for_outbox = envelope_mp.clone();
    let action_num = db::spawn_db(state_arc.db.clone(), move |pool| {
        db::persist_outbound_game_action(
            &pool,
            &session_for_outbox,
            &command_for_outbox,
            &payload_for_history,
            &sender_for_outbox,
            action_timestamp,
            &envelope_for_outbox,
        )
    })
    .await
    .ok()
    .flatten();
    let Some(action_num) = action_num else {
        let _ =
            state_arc
                .lrgp_router
                .rollback_outgoing(&app_id, &session_id, &identity_id, snapshot);
        tracing::error!(
            session_id,
            app_id,
            command,
            "Failed to commit LRGP durable outbox"
        );
        let result =
            game_action_result_json(false, &session_id, &command, None, Some("storage_failed"));
        state_arc.emit_to_all("game_action_result", result.clone());
        return Ok(result);
    };
    emit_game_sessions(&state_arc, &identity_id, Some(&dest_hash)).await;

    // One blocking task so the lxmf MutexGuard never crosses an .await.
    let st: Arc<AppState> = Arc::clone(&state_arc);
    let dh = dest_hash.clone();
    let ft = fallback_text.clone();
    let fields = lrgp_fields.clone();
    let id_c = identity_id.clone();
    let (msg_id, sender_hash) = tokio::task::spawn_blocking(move || {
        if let Ok(mut lxmf) = st.lxmf.lock() {
            let sender = lxmf
                .as_ref()
                .map(|m| m.lxmf_hash.clone())
                .unwrap_or_default();
            let id = lxmf.as_mut().and_then(|mgr| {
                mgr.send_message_with_lrgp_fields_preference(
                    &dh,
                    &ft,
                    &fields,
                    &st.db,
                    &id_c,
                    delivery_pref,
                )
            });
            (id, sender)
        } else {
            (None, String::new())
        }
    })
    .await
    .unwrap_or((None, String::new()));

    tracing::info!(
        target: "ttt_trace",
        step = "send.lxmf_submitted",
        msg_id_some = msg_id.is_some(),
        msg_id = %msg_id.as_deref().map(short_hex).unwrap_or(""),
        sender = %short_hex(&sender_hash),
        "LXMF send_message_with_lrgp_fields returned"
    );

    match msg_id {
        Some(id) => {
            crate::commands::messaging::schedule_announce_after_user_send_from_origin(
                &state_arc,
                &dest_hash,
                activity_origin,
            );
            state_arc.lxmf_notify.notify_one();
            if let Ok(mut map) = state_arc.lrgp_msg_to_session.lock() {
                map.insert(
                    id.clone(),
                    crate::state::LrgpMsgMeta {
                        session_id: session_id.clone(),
                        identity_id: identity_id.clone(),
                        contact_hash: dest_hash.clone(),
                        app_id: app_id.clone(),
                        sent_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64(),
                    },
                );
            }

            if let Some(session_state) = state_arc.lrgp_router.with_app(&app_id, |app| {
                app.get_session_state(&session_id, &identity_id)
            }) {
                crate::commands::shared::save_session_from_state(
                    &state_arc,
                    SessionStateSave {
                        session_id: &session_id,
                        identity_id: &identity_id,
                        app_id: &app_id,
                        app_version,
                        contact_hash: &dest_hash,
                        session_state: &session_state,
                        delivery_state: Some("sending"),
                    },
                )
                .await;
                tracing::info!(
                    target: "ttt_trace",
                    step = "send.db_saved_sending",
                    "persisted session with delivery_state=sending"
                );
            }

            let payload = game_action_result_json(true, &session_id, &command, Some(&id), None);
            state_arc.emit_to_all("game_action_result", payload.clone());
            emit_game_sessions(&state_arc, &identity_id, Some(&dest_hash)).await;
            Ok(payload)
        }
        None => {
            // Construction-time failure (LXMF not yet initialized, hex decode,
            // dest length, signing). Roll back the router, mark the session
            // failed, surface to UI; the user can use the Resend button to
            // try again. Direct's transport-layer retries
            // (MAX_DELIVERY_ATTEMPTS=5 in lxmf-core) handle wire-loss for
            // sends that *did* reach the router.
            let mgr_ready = state_arc
                .lxmf
                .lock()
                .ok()
                .map(|g| g.is_some())
                .unwrap_or(false);
            let mut reason = if mgr_ready {
                "send_failed"
            } else {
                "lxmf_not_initialized"
            };

            tracing::warn!(
                target: "ttt_trace",
                step = "send.failed",
                mgr_ready,
                reason,
                "LRGP submit failed \u{2014} rolling back"
            );

            let sid_for_rollback = session_id.clone();
            let identity_for_rollback = identity_id.clone();
            let snapshot_for_db = snapshot.clone();
            let db_rolled_back = db::spawn_db(state_arc.db.clone(), move |pool| {
                db::rollback_outbound_game_action(
                    &pool,
                    &sid_for_rollback,
                    &identity_for_rollback,
                    action_num,
                    snapshot_for_db.as_ref(),
                )
            })
            .await
            .unwrap_or(false);

            if db_rolled_back {
                // The action never reached LXMF and both durable and in-memory
                // state are back at the pre-action snapshot. Do not stamp the
                // restored session as failed: there is no matching envelope
                // left to resend, and doing so could make the UI retransmit an
                // older action from this session.
                if state_arc
                    .lrgp_router
                    .rollback_outgoing(&app_id, &session_id, &identity_id, snapshot)
                    .is_err()
                {
                    tracing::warn!(
                        target: "ttt_trace",
                        step = "send.rollback_err",
                        reason = "rollback_failed",
                        "rollback_outgoing failed"
                    );
                }
            } else {
                // Keep router and DB on the same advanced state. The durable
                // envelope remains available to the explicit Resend action,
                // so this is the one failure mode that should be labelled as
                // resendable instead of rolling the board back in the UI.
                tracing::error!(
                    session_id,
                    action_num,
                    "Could not roll back LRGP outbox; retaining resendable pending action"
                );
                reason = "resend_required";
                if let Some(session_state) = state_arc.lrgp_router.with_app(&app_id, |app| {
                    app.get_session_state(&session_id, &identity_id)
                }) && !session_state.is_empty()
                {
                    crate::commands::shared::save_session_from_state(
                        &state_arc,
                        SessionStateSave {
                            session_id: &session_id,
                            identity_id: &identity_id,
                            app_id: &app_id,
                            app_version,
                            contact_hash: &dest_hash,
                            session_state: &session_state,
                            delivery_state: Some("failed"),
                        },
                    )
                    .await;
                }
            }

            emit_game_sessions(&state_arc, &identity_id, Some(&dest_hash)).await;
            let payload = game_action_result_json(false, &session_id, &command, None, Some(reason));
            state_arc.emit_to_all("game_action_result", payload.clone());
            Ok(payload)
        }
    }
}

#[tauri::command]
pub async fn get_active_games(state: State<'_, Arc<AppState>>, hash: String) -> AppResult<Value> {
    let contact_hash = sanitize_text(&hash, 128);
    let identity_id = active_lxmf_hash(&state);
    let id_c = identity_id.clone();
    let ch_c = contact_hash.clone();
    let sessions = db::spawn_db(state.db.clone(), move |p| {
        db::list_game_sessions(&p, &id_c, Some(&ch_c), None)
    })
    .await
    .map_err(|_| AppError::internal("get_active_games db task panicked"))?;
    Ok(json!({ "hash": contact_hash, "games": sessions }))
}

#[tauri::command]
pub async fn get_all_game_sessions(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity_id = active_lxmf_hash(&state);
    let id_c = identity_id.clone();
    let sessions = db::spawn_db(state.db.clone(), move |p| {
        db::list_game_sessions(&p, &id_c, None, None)
    })
    .await
    .map_err(|_| AppError::internal("get_all_game_sessions db task panicked"))?;
    Ok(json!(sessions))
}

#[tauri::command]
pub async fn mark_game_read(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let session_id = sanitize_text(&session_id, 128);
    let identity_id = active_lxmf_hash(&state);
    let identity_id_for_db = identity_id.clone();
    let _ = db::spawn_db(state.db.clone(), move |p| {
        db::mark_game_read(&p, &session_id, &identity_id_for_db);
    })
    .await;
    emit_game_sessions(&state_arc, &identity_id, None).await;
    Ok(json!(null))
}

#[tauri::command]
pub async fn delete_game_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let session_id = sanitize_text(&session_id, 128);
    if session_id.is_empty() {
        return Err(AppError::bad_request("session_id required"));
    }
    let identity_id = active_lxmf_hash(&state_arc);
    let sid = session_id.clone();
    let id_c = identity_id.clone();
    let (outcome, app_id) = db::spawn_db(state_arc.db.clone(), move |p| {
        let Some(session) = db::get_game_session(&p, &sid, &id_c) else {
            return ("not_found", None);
        };
        let status = session.get("status").and_then(Value::as_str).unwrap_or("");
        if !matches!(status, "completed" | "declined" | "expired") {
            return ("active", None);
        }
        let app_id = session
            .get("app_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if db::delete_game_session(&p, &sid, &id_c) {
            ("deleted", app_id)
        } else {
            ("failed", None)
        }
    })
    .await
    .map_err(|_| AppError::internal("delete_game_session db task panicked"))?;
    match outcome {
        "deleted" => {}
        "not_found" => return Err(AppError::not_found("game session not found")),
        "active" => {
            return Err(AppError::conflict(
                "Active and pending games cannot be removed from history",
            ));
        }
        _ => {
            return Err(AppError::database_unavailable(
                "game session could not be removed",
            ));
        }
    }
    if let Some(app_id) = app_id
        && state_arc
            .lrgp_router
            .remove_session(&app_id, &session_id, &identity_id)
            .is_err()
    {
        tracing::warn!(
            reason = "router_remove_failed",
            "Failed to remove live LRGP session"
        );
    }
    state_arc.emit_to_all("game_session_deleted", json!({ "session_id": session_id }));
    Ok(json!({ "session_id": session_id }))
}

#[tauri::command]
pub async fn get_game_session_detail(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> AppResult<Value> {
    let session_id = sanitize_text(&session_id, 128);
    let identity_id = active_lxmf_hash(&state);
    let sid = session_id.clone();
    let id_c = identity_id.clone();
    let (session, actions) = db::spawn_db(state.db.clone(), move |p| {
        let session = db::get_game_session(&p, &sid, &id_c);
        let actions = db::get_game_actions(&p, &sid, &id_c);
        (session, actions)
    })
    .await
    .map_err(|_| AppError::internal("game_session_detail db task panicked"))?;
    Ok(json!({ "session": session, "actions": actions }))
}

#[derive(Deserialize)]
pub struct ResendLastGameActionArgs {
    pub session_id: String,
    #[serde(default)]
    pub delivery_method: Option<String>,
}

/// User-driven retransmit of the active identity's most recent outbound action
/// in this session. Re-sends the same envelope (preserved on the action row)
/// rather than re-dispatching through the LRGP router — re-dispatch would be
/// rejected as `not_your_turn` because local game state already advanced.
///
/// Idempotency: at the wire level the recipient's LRGP nonce dedup
/// (`lrgp::dedup`) catches duplicates within ~10 minutes. Beyond that window
/// the chess/tictactoe app layer's move-number sequencing rejects already-
/// applied moves.
#[tauri::command]
pub async fn resend_last_game_action(
    state: State<'_, Arc<AppState>>,
    args: ResendLastGameActionArgs,
) -> AppResult<Value> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    let activity_origin = state_arc.activity_request_fence();
    let session_id = sanitize_text(&args.session_id, 128);
    let delivery_pref =
        crate::commands::messaging::parse_delivery_preference(args.delivery_method.as_deref());
    if session_id.is_empty() {
        return Err(AppError::bad_request("session_id required"));
    }
    crate::commands::messaging::validate_delivery_preference(&state_arc, delivery_pref)?;
    let identity_id = active_lxmf_hash(&state_arc);

    let sid = session_id.clone();
    let iid = identity_id.clone();
    let (session, envelope_mp) = db::spawn_db(state_arc.db.clone(), move |p| {
        let session = db::get_game_session(&p, &sid, &iid);
        let env = db::get_last_outbound_envelope_for_session(&p, &sid, &iid);
        (session, env)
    })
    .await
    .map_err(|_| AppError::internal("resend_last_game_action db task panicked"))?;

    let session = session.ok_or_else(|| AppError::not_found("session not found"))?;
    let envelope_mp = envelope_mp
        .ok_or_else(|| AppError::not_found("no outbound envelope persisted for this session"))?;

    let dest_hash = session
        .get("contact_hash")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let app_id = session
        .get("app_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let app_version = session
        .get("app_version")
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or(1);
    if !validate_hex(&dest_hash, 32, 32) || app_id.is_empty() {
        return Err(AppError::internal(
            "session row missing contact_hash or app_id",
        ));
    }

    let envelope = lrgp::envelope::unpack_from_bytes(&envelope_mp)
        .map_err(|e| AppError::internal(format!("envelope unpack: {e}")))?;
    let command = lrgp::envelope::value_as_str(envelope.get("c").unwrap_or(&rmpv::Value::Nil))
        .unwrap_or("")
        .to_string();
    let lrgp_fields = lrgp::envelope::pack_lxmf_fields(&envelope)
        .map_err(|e| AppError::internal(format!("envelope field packing: {e}")))?;
    let fallback_text = format!("[LRGP {}] {}", app_id, command);

    let _ =
        crate::commands::shared::hydrate_contact_identity_for_send(&state_arc, &dest_hash).await;
    crate::commands::messaging::ensure_propagation_ready_for_send(
        &state_arc,
        &dest_hash,
        delivery_pref,
        ratspeak_runtime::lxmf::DeliveryProfile::Lrgp,
        None,
    )
    .await?;

    let st: Arc<AppState> = Arc::clone(&state_arc);
    let dh = dest_hash.clone();
    let iid_for_send = identity_id.clone();
    let msg_id: Option<String> = tokio::task::spawn_blocking(move || {
        if let Ok(mut lxmf) = st.lxmf.lock() {
            lxmf.as_mut().and_then(|mgr| {
                mgr.send_message_with_lrgp_fields_preference(
                    &dh,
                    &fallback_text,
                    &lrgp_fields,
                    &st.db,
                    &iid_for_send,
                    delivery_pref,
                )
            })
        } else {
            None
        }
    })
    .await
    .unwrap_or(None);

    match msg_id {
        Some(id) => {
            crate::commands::messaging::schedule_announce_after_user_send_from_origin(
                &state_arc,
                &dest_hash,
                activity_origin,
            );
            state_arc.lxmf_notify.notify_one();
            if let Ok(mut map) = state_arc.lrgp_msg_to_session.lock() {
                map.insert(
                    id.clone(),
                    crate::state::LrgpMsgMeta {
                        session_id: session_id.clone(),
                        identity_id: identity_id.clone(),
                        contact_hash: dest_hash.clone(),
                        app_id: app_id.clone(),
                        sent_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64(),
                    },
                );
            }
            if let Some(session_state) = state_arc.lrgp_router.with_app(&app_id, |app| {
                app.get_session_state(&session_id, &identity_id)
            }) {
                crate::commands::shared::save_session_from_state(
                    &state_arc,
                    SessionStateSave {
                        session_id: &session_id,
                        identity_id: &identity_id,
                        app_id: &app_id,
                        app_version,
                        contact_hash: &dest_hash,
                        session_state: &session_state,
                        delivery_state: Some("sending"),
                    },
                )
                .await;
            }
            emit_game_sessions(&state_arc, &identity_id, Some(&dest_hash)).await;
            let payload = game_action_result_json(true, &session_id, &command, Some(&id), None);
            state_arc.emit_to_all("game_action_result", payload.clone());
            Ok(payload)
        }
        None => {
            let payload =
                game_action_result_json(false, &session_id, &command, None, Some("send_failed"));
            state_arc.emit_to_all("game_action_result", payload.clone());
            Ok(payload)
        }
    }
}

#[tauri::command]
pub async fn get_available_games(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let manifests = state.lrgp_router.list_apps();
    let games: Vec<Value> = manifests.iter().map(game_manifest_json).collect();
    Ok(json!(games))
}
