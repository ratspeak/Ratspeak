//! LRGP commands. `send_game_action` returns a `GameActionResult`;
//! state broadcasts go via `AppHandle::emit`.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::commands::shared::{emit_game_sessions, json_to_rmpv_map};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::{active_lxmf_hash, sanitize_text, validate_hex};
use crate::state::AppState;

fn short_hex(s: &str) -> &str {
    let n = s.len().min(8);
    s.get(..n).unwrap_or("")
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

fn lrgp_payload_is_empty(envelope: &lrgp::envelope::Envelope) -> bool {
    envelope
        .get(lrgp::constants::KEY_PAYLOAD)
        .and_then(lrgp::envelope::map_from_value)
        .map(|payload| payload.is_empty())
        .unwrap_or(true)
}

fn local_lrgp_reject_reason(
    command: &str,
    fallback_text: &str,
    envelope: &lrgp::envelope::Envelope,
) -> Option<&'static str> {
    if command != lrgp::constants::CMD_MOVE || !lrgp_payload_is_empty(envelope) {
        return None;
    }

    let lower = fallback_text.to_ascii_lowercase();
    if lower.contains("not your turn") {
        Some("not_your_turn")
    } else if lower.contains("invalid")
        || lower.contains("illegal")
        || lower.contains("occupied")
        || lower.contains("cell")
    {
        Some("invalid_move")
    } else {
        Some("dispatch_failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn move_envelope(payload: Option<HashMap<String, rmpv::Value>>) -> lrgp::envelope::Envelope {
        lrgp::envelope::pack_envelope("chess", 1, lrgp::constants::CMD_MOVE, "sid", payload, None)
    }

    #[test]
    fn local_reject_maps_empty_wrong_turn_move() {
        let env = move_envelope(Some(HashMap::new()));
        assert_eq!(
            local_lrgp_reject_reason(
                lrgp::constants::CMD_MOVE,
                "[LRGP Chess] Not your turn",
                &env
            ),
            Some("not_your_turn")
        );
    }

    #[test]
    fn local_reject_maps_empty_illegal_move() {
        let env = move_envelope(Some(HashMap::new()));
        assert_eq!(
            local_lrgp_reject_reason(
                lrgp::constants::CMD_MOVE,
                "[LRGP Chess] Illegal move: e2e5",
                &env
            ),
            Some("invalid_move")
        );
    }

    #[test]
    fn local_reject_ignores_non_empty_move_payload() {
        let mut payload = HashMap::new();
        payload.insert("m".into(), rmpv::Value::String("e2e4".into()));
        let env = move_envelope(Some(payload));
        assert_eq!(
            local_lrgp_reject_reason(lrgp::constants::CMD_MOVE, "[LRGP Chess] e2e4", &env),
            None
        );
    }

    #[test]
    fn local_reject_ignores_empty_non_move_payload() {
        let env = lrgp::envelope::pack_envelope(
            "chess",
            1,
            lrgp::constants::CMD_DRAW_OFFER,
            "sid",
            Some(HashMap::new()),
            None,
        );
        assert_eq!(
            local_lrgp_reject_reason(
                lrgp::constants::CMD_DRAW_OFFER,
                "[LRGP Chess] Offered a draw",
                &env
            ),
            None
        );
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
    let dest_hash = sanitize_text(&args.dest_hash, 128);
    let app_id = sanitize_text(&args.app_id, 64);
    let command = sanitize_text(&args.command, 64);
    let session_id = sanitize_text(&args.session_id, 128);
    let delivery_pref =
        crate::commands::messaging::parse_delivery_preference(args.delivery_method.as_deref());

    if !validate_hex(&dest_hash, 16, 64) || app_id.is_empty() || command.is_empty() {
        let payload =
            game_action_result_json(false, &session_id, &command, None, Some("invalid_params"));
        state_arc.emit_to_all("game_action_result", payload.clone());
        return Ok(payload);
    }
    // TODO: Once Ratspeak capability discovery has been deployed long enough,
    // reject or warn for contacts that do not advertise `ratspeak.games`.
    crate::commands::messaging::validate_delivery_preference(&state_arc, delivery_pref)?;

    crate::commands::shared::resolve_before_send(&state_arc, &dest_hash).await;
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
    let snapshot =
        state_arc
            .lrgp_router
            .snapshot_before_outgoing(&app_id, &session_id, &identity_id);

    let dispatch_result = state_arc.lrgp_router.dispatch_outgoing(
        &app_id,
        1,
        &command,
        &session_id,
        &payload,
        &identity_id,
    );

    let (envelope, fallback_text) = match dispatch_result {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(
                target: "ttt_trace",
                step = "send.dispatch_err",
                sid = %short_hex(&session_id),
                command = %command,
                error = %e,
                "dispatch_outgoing returned error"
            );
            let payload = game_action_result_json(
                false,
                &session_id,
                &command,
                None,
                Some("dispatch_failed"),
            );
            state_arc.emit_to_all("game_action_result", payload.clone());
            return Ok(payload);
        }
    };

    if let Some(reason) = local_lrgp_reject_reason(&command, &fallback_text, &envelope) {
        tracing::info!(
            target: "lrgp_trace",
            step = "send.rejected_local",
            sid = %short_hex(&session_id),
            app_id = %app_id,
            command = %command,
            reason,
            fallback = %fallback_text,
            "short-circuiting outgoing LRGP action"
        );

        if let Err(e) =
            state_arc
                .lrgp_router
                .rollback_outgoing(&app_id, &session_id, &identity_id, snapshot)
        {
            tracing::warn!(
                target: "lrgp_trace",
                step = "send.local_reject.rollback_err",
                sid = %short_hex(&session_id),
                app_id = %app_id,
                error = %e,
                "rollback_outgoing failed after local LRGP rejection"
            );
        }

        let payload = game_action_result_json(false, &session_id, &command, None, Some(reason));
        state_arc.emit_to_all("game_action_result", payload.clone());
        return Ok(payload);
    }

    tracing::info!(
        target: "ttt_trace",
        step = "send.dispatch_ok",
        sid = %short_hex(&session_id),
        app_id = %app_id,
        command = %command,
        dest = %short_hex(&dest_hash),
        my = %short_hex(&identity_id),
        "dispatch_outgoing returned envelope"
    );

    // Persist pre-send so DB never drifts from router on failure/crash.
    if let Some(session_state) = state_arc.lrgp_router.with_app(&app_id, |app| {
        app.get_session_state(&session_id, &identity_id)
    }) {
        crate::commands::shared::save_session_from_state(
            &state_arc,
            &session_id,
            &identity_id,
            &app_id,
            &dest_hash,
            &session_state,
            Some("pending"),
        )
        .await;
    }
    emit_game_sessions(&state_arc, &identity_id, Some(&dest_hash)).await;

    let lrgp_fields = lrgp::envelope::pack_lxmf_fields(&envelope);
    // Persisted on the action row so the user-driven "Resend last move" path
    // re-transmits the exact same envelope (including the original nonce)
    // without having to re-dispatch through the LRGP router, which would
    // reject the resend because local state already advanced.
    let envelope_mp = lrgp::envelope::pack_to_bytes(&envelope).ok();

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
        sid = %short_hex(&session_id),
        command = %command,
        msg_id_some = msg_id.is_some(),
        msg_id = %msg_id.as_deref().map(short_hex).unwrap_or(""),
        sender = %short_hex(&sender_hash),
        "LXMF send_message_with_lrgp_fields returned"
    );

    match msg_id {
        Some(id) => {
            state_arc.lxmf_notify.notify_one();
            let pool = state_arc.db.clone();
            let session_for_db = session_id.clone();
            let id_for_db = identity_id.clone();
            let command_for_db = command.clone();
            let payload_for_db = payload_json.clone();
            let sender_for_db = sender_hash.clone();
            let envelope_for_db = envelope_mp.clone();
            let _ = db::spawn_db(pool, move |p| {
                let action_num = db::get_game_action_count(&p, &session_for_db, &id_for_db);
                let action = lrgp::store::Action {
                    session_id: session_for_db.clone(),
                    identity_id: id_for_db.clone(),
                    action_num,
                    command: command_for_db,
                    payload_json: serde_json::to_string(&payload_for_db)
                        .unwrap_or_else(|_| "{}".into()),
                    sender: sender_for_db,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64(),
                };
                db::save_game_action(&p, &action, envelope_for_db.as_deref());
            })
            .await;

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
                    &session_id,
                    &identity_id,
                    &app_id,
                    &dest_hash,
                    &session_state,
                    Some("sending"),
                )
                .await;
                tracing::info!(
                    target: "ttt_trace",
                    step = "send.db_saved_sending",
                    sid = %short_hex(&session_id),
                    command = %command,
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
            let reason = if mgr_ready {
                "send_failed"
            } else {
                "lxmf_not_initialized"
            };

            tracing::warn!(
                target: "ttt_trace",
                step = "send.failed",
                sid = %short_hex(&session_id),
                command = %command,
                mgr_ready,
                "LRGP submit failed \u{2014} rolling back"
            );

            if let Err(e) = state_arc.lrgp_router.rollback_outgoing(
                &app_id,
                &session_id,
                &identity_id,
                snapshot,
            ) {
                tracing::warn!(
                    target: "ttt_trace",
                    step = "send.rollback_err",
                    sid = %short_hex(&session_id),
                    error = %e,
                    "rollback_outgoing failed"
                );
            }

            if let Some(session_state) = state_arc.lrgp_router.with_app(&app_id, |app| {
                app.get_session_state(&session_id, &identity_id)
            }) {
                if session_state.is_empty() {
                    let sid_del = session_id.clone();
                    let id_del = identity_id.clone();
                    let _ = db::spawn_db(state_arc.db.clone(), move |p| {
                        db::delete_game_session(&p, &sid_del, &id_del);
                    })
                    .await;
                } else {
                    crate::commands::shared::save_session_from_state(
                        &state_arc,
                        &session_id,
                        &identity_id,
                        &app_id,
                        &dest_hash,
                        &session_state,
                        Some("failed"),
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
    let session_id = sanitize_text(&session_id, 128);
    let identity_id = active_lxmf_hash(&state);
    let _ = db::spawn_db(state.db.clone(), move |p| {
        db::mark_game_read(&p, &session_id, &identity_id);
    })
    .await;
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
    let _ = db::spawn_db(state_arc.db.clone(), move |p| {
        db::delete_game_session(&p, &sid, &id_c);
    })
    .await;
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
    if !validate_hex(&dest_hash, 16, 64) || app_id.is_empty() {
        return Err(AppError::internal(
            "session row missing contact_hash or app_id",
        ));
    }

    let envelope = lrgp::envelope::unpack_from_bytes(&envelope_mp)
        .map_err(|e| AppError::internal(format!("envelope unpack: {e}")))?;
    let command = lrgp::envelope::value_as_str(envelope.get("c").unwrap_or(&rmpv::Value::Nil))
        .unwrap_or("")
        .to_string();
    let lrgp_fields = lrgp::envelope::pack_lxmf_fields(&envelope);
    let fallback_text = format!("[LRGP {}] {}", app_id, command);

    crate::commands::shared::resolve_before_send(&state_arc, &dest_hash).await;
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
                    &session_id,
                    &identity_id,
                    &app_id,
                    &dest_hash,
                    &session_state,
                    Some("sending"),
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
    let games: Vec<Value> = manifests
        .iter()
        .map(|m| {
            json!({
                "app_id": m.app_id,
                "version": m.version,
                "display_name": m.display_name,
                "icon": m.icon,
                "session_type": m.session_type,
                "max_players": m.max_players,
                "actions": m.actions,
            })
        })
        .collect();
    Ok(json!(games))
}
