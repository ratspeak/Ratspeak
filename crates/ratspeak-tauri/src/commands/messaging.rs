//! Conversation reads + message send + search + file downloads.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;

use crate::commands::shared::remove_stored_file_refs;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::{active_identity_id, sanitize_text, validate_hex};
use crate::lxmf::{
    AttachmentMessageRequest, DeliveryPreference, DeliveryProfile, LxmfManager, MessageSendRequest,
    ReactionSendRequest, ReplyMessageSendRequest,
};
use crate::state::{
    AppState, LxmfClientSendAdmissionError, LxmfClientSendCancellation,
    LxmfClientSendCancellationProbe, LxmfClientSendGuard,
};
use ratspeak_runtime::activity::producer;

const MAX_LXMF_MESSAGE_BYTES: usize = rns_protocol::resource::MAX_RESOURCE_SIZE;

enum LxmfClientSendAttempt<T> {
    Queued(T),
    Cancelled,
    Failed(producer::LxmfSubmissionFailureReason),
}

fn queue_lxmf_client_send<T>(
    state: &AppState,
    cancellation: Option<&LxmfClientSendCancellationProbe>,
    queue: impl FnOnce(&mut LxmfManager) -> Option<T>,
) -> LxmfClientSendAttempt<T> {
    if cancellation.is_some_and(LxmfClientSendCancellationProbe::is_cancelled) {
        return LxmfClientSendAttempt::Cancelled;
    }
    let send_lock_started = std::time::Instant::now();
    let Ok(mut lxmf) = state.lxmf.lock() else {
        return LxmfClientSendAttempt::Failed(
            producer::LxmfSubmissionFailureReason::RouterUnavailable,
        );
    };
    let waited = send_lock_started.elapsed();
    if waited > std::time::Duration::from_secs(1) {
        tracing::warn!(
            waited_ms = waited.as_millis() as u64,
            "send waited on lxmf manager lock"
        );
    }
    if cancellation.is_some_and(LxmfClientSendCancellationProbe::is_cancelled) {
        return LxmfClientSendAttempt::Cancelled;
    }
    let Some(manager) = lxmf.as_mut() else {
        return LxmfClientSendAttempt::Failed(
            producer::LxmfSubmissionFailureReason::RouterUnavailable,
        );
    };
    queue(manager)
        .map(LxmfClientSendAttempt::Queued)
        .unwrap_or(LxmfClientSendAttempt::Failed(
            producer::LxmfSubmissionFailureReason::PreparationFailed,
        ))
}

fn normalize_lxmf_client_msg_id(raw: Option<&str>) -> AppResult<Option<String>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let client_msg_id = sanitize_text(raw, 128);
    let valid = client_msg_id.len() == 36
        && client_msg_id.starts_with("out_")
        && validate_hex(&client_msg_id[4..], 32, 32);
    if !valid {
        return Err(AppError::bad_request("Invalid client message ID"));
    }
    Ok(Some(client_msg_id))
}

fn begin_lxmf_client_send(
    state: &Arc<AppState>,
    client_msg_id: Option<&String>,
) -> AppResult<Option<LxmfClientSendGuard>> {
    let Some(client_msg_id) = client_msg_id else {
        return Ok(None);
    };
    state
        .begin_lxmf_client_send(client_msg_id.clone())
        .map(Some)
        .map_err(|error| match error {
            LxmfClientSendAdmissionError::Duplicate => {
                AppError::conflict("Message is already being prepared")
            }
            LxmfClientSendAdmissionError::Capacity => {
                AppError::service_unavailable("Too many messages are being prepared")
            }
        })
}

fn emit_prequeue_lxmf_cancellation(state: &AppState, client_msg_id: &str) -> Value {
    state.emit_to_all(
        "lxmf_step",
        json!({
            "step": "cancelled",
            "client_msg_id": client_msg_id,
        }),
    );
    json!({
        "ok": true,
        "cancelled": true,
        "client_msg_id": client_msg_id,
    })
}

fn cancelled_lxmf_client_send_response(
    state: &AppState,
    guard: Option<&LxmfClientSendGuard>,
) -> Option<Value> {
    guard
        .filter(|guard| guard.is_cancelled())
        .map(|guard| emit_prequeue_lxmf_cancellation(state, guard.client_msg_id()))
}

fn activity_lxmf_delivery_method(
    method: lxmf_core::constants::DeliveryMethod,
) -> producer::LxmfDeliveryMethod {
    match method {
        lxmf_core::constants::DeliveryMethod::Direct => producer::LxmfDeliveryMethod::Direct,
        lxmf_core::constants::DeliveryMethod::Opportunistic => {
            producer::LxmfDeliveryMethod::Opportunistic
        }
        lxmf_core::constants::DeliveryMethod::Paper => producer::LxmfDeliveryMethod::Paper,
        lxmf_core::constants::DeliveryMethod::Propagated => {
            producer::LxmfDeliveryMethod::Propagated
        }
    }
}

fn record_lxmf_delivery_queued(
    state: &AppState,
    fence: crate::state::ActivityRequestFence,
    message_id: &str,
    destination_hash: &str,
    method: lxmf_core::constants::DeliveryMethod,
) {
    state.activity.record_event_fenced(
        || state.is_current_activity_origin_fence(fence),
        || {
            let message = producer::MessageId::from_hex(message_id)?;
            let destination = producer::DestinationHash::from_hex(destination_hash)?;
            Ok(producer::lxmf_delivery_queued(
                producer::LxmfDeliveryQueued {
                    message,
                    destination,
                    method: activity_lxmf_delivery_method(method),
                },
            ))
        },
    );
}

fn record_lxmf_submission_failed(
    state: &AppState,
    fence: crate::state::ActivityRequestFence,
    destination_hash: &str,
    reason: producer::LxmfSubmissionFailureReason,
) {
    state.activity.record_event_fenced(
        || state.is_current_activity_origin_fence(fence),
        || {
            let destination = producer::DestinationHash::from_hex(destination_hash)?;
            Ok(producer::lxmf_submission_failed(
                producer::LxmfSubmissionFailed {
                    destination,
                    reason,
                },
            ))
        },
    );
}

fn emit_lxmf_send_error(
    state: &AppState,
    client_msg_id: Option<&str>,
    code: &'static str,
    message: &'static str,
) {
    state.emit_to_all(
        "lxmf_step",
        json!({
            "step": "error",
            "code": code,
            "message": message,
            "client_msg_id": client_msg_id,
        }),
    );
}

fn base64_decoded_len_upper_bound(encoded_len: usize) -> Option<usize> {
    encoded_len.checked_add(3)?.checked_div(4)?.checked_mul(3)
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/bmp" => "bmp",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "application/json" => "json",
        "application/zip" => "zip",
        _ => "",
    }
}

fn ensure_filename_extension(name: &str, mime: &str, fallback_stem: &str) -> String {
    let mut clean = sanitize_text(name, 200)
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_' || *c == ' ')
        .collect::<String>()
        .trim()
        .to_string();
    if clean.is_empty() {
        clean = fallback_stem.to_string();
    }
    let has_ext = clean
        .rsplit_once('.')
        .map(|(_, ext)| {
            !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
        .unwrap_or(false);
    if has_ext {
        return clean;
    }
    let ext = extension_for_mime(mime);
    if ext.is_empty() {
        clean
    } else {
        format!("{clean}.{ext}")
    }
}

fn sanitize_message_content(value: &str) -> AppResult<String> {
    let trimmed = value.trim();
    if trimmed.len() > MAX_LXMF_MESSAGE_BYTES {
        return Err(AppError::bad_request(
            "Message exceeds protocol resource limit",
        ));
    }
    Ok(trimmed.to_string())
}

#[tauri::command]
pub async fn api_conversation(
    state: State<'_, Arc<AppState>>,
    dest_hash: String,
) -> AppResult<Value> {
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request("Invalid identity hash"));
    }
    let identity_id = active_identity_id(&state);
    let dest_for_db = dest_hash.clone();
    let id_for_db = identity_id.clone();
    // 5s cap: WAL checkpoint / post-resume tick can stall the DB.
    let fetch = db::spawn_db(state.db.clone(), move |p| {
        db::get_conversation(&p, &dest_for_db, &id_for_db, 100)
    });
    match tokio::time::timeout(Duration::from_secs(5), fetch).await {
        Ok(Ok(messages)) => Ok(json!(messages)),
        Ok(Err(_)) => {
            tracing::warn!(reason = "db_task_failed", "api_conversation db task failed");
            Err(AppError::internal("Database task failed"))
        }
        Err(_) => {
            tracing::warn!(reason = "timeout", "api_conversation timed out after 5s");
            Err(AppError::service_unavailable(
                "Database temporarily unavailable",
            ))
        }
    }
}

pub(crate) use ratspeak_runtime::messaging::{
    broadcast_conversations, build_conversations_payload,
};

#[tauri::command]
pub async fn api_lxmf_conversations(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    build_conversations_payload(&state)
        .await
        .ok_or_else(|| AppError::service_unavailable("Database temporarily unavailable"))
}

#[tauri::command]
pub async fn api_search_messages(
    state: State<'_, Arc<AppState>>,
    q: Option<String>,
) -> AppResult<Value> {
    let query = q.unwrap_or_default();
    let query = query.trim();
    if query.len() < 2 {
        return Ok(json!([]));
    }
    let identity_id = active_identity_id(&state);
    let query_str = query.to_string();
    let id_for_db = identity_id.clone();
    let results = db::spawn_db(state.db.clone(), move |p| {
        db::search_messages(&p, &query_str, &id_for_db, 50)
    })
    .await
    .unwrap_or_else(|_| {
        tracing::error!(
            reason = "task_panicked",
            "search_messages db task panicked — returning empty"
        );
        Default::default()
    });
    Ok(json!(results))
}

#[derive(Deserialize)]
pub struct SendLxmfArgs {
    pub dest_hash: String,
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub delivery_method: Option<String>,
    /// Echoed back in `lxmf_step` so the optimistic UI row reconciles.
    #[serde(default)]
    pub client_msg_id: Option<String>,
}

#[derive(Deserialize)]
pub struct CancelLxmfMessageArgs {
    pub msg_id: String,
}

pub(crate) fn parse_delivery_preference(value: Option<&str>) -> DeliveryPreference {
    DeliveryPreference::parse(value)
}

pub(crate) fn propagation_node_configured(state: &AppState) -> bool {
    let (mode, _) = crate::propagation::read_settings(state);
    match mode {
        crate::propagation::PropagationMode::Off => false,
        crate::propagation::PropagationMode::Auto => state
            .auto_active_node
            .read()
            .ok()
            .and_then(|node| *node)
            .is_some(),
        crate::propagation::PropagationMode::Manual => state
            .lxmf
            .lock()
            .ok()
            .and_then(|l| l.as_ref().map(|m| m.configured_propagation_node.is_some()))
            .unwrap_or(false),
    }
}

pub(crate) fn validate_delivery_preference(
    state: &AppState,
    pref: DeliveryPreference,
) -> AppResult<()> {
    if pref == DeliveryPreference::Propagated {
        let (mode, _) = crate::propagation::read_settings(state);
        if mode == crate::propagation::PropagationMode::Off {
            return Err(AppError::conflict("Offline Inbox is off."));
        }
        if mode == crate::propagation::PropagationMode::Manual
            && !propagation_node_configured(state)
        {
            return Err(AppError::conflict(
                "No Offline Inbox node configured. Set one in Settings > Network first.",
            ));
        }
    }
    Ok(())
}

fn destination_identity_known(state: &AppState, dest_hash: &str) -> bool {
    state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(|mgr| mgr.is_destination_known(dest_hash)))
        .unwrap_or(false)
}

pub(crate) fn schedule_announce_after_user_send_from_origin(
    state: &Arc<AppState>,
    dest_hash: &str,
    activity_fence: crate::state::ActivityRequestFence,
) {
    let state = Arc::clone(state);
    let dest_hash = dest_hash.to_string();
    tokio::spawn(async move {
        let _ = crate::maybe_opportunistic_announce_before_user_send_from_origin(
            &state,
            &dest_hash,
            activity_fence,
        )
        .await;
    });
}

pub(crate) async fn ensure_propagation_ready_for_send(
    state: &Arc<AppState>,
    dest_hash: &str,
    pref: DeliveryPreference,
    profile: DeliveryProfile,
    client_msg_id: Option<&str>,
) -> AppResult<()> {
    let st = Arc::clone(state);
    let dh = dest_hash.to_string();
    let method = tokio::task::spawn_blocking(move || {
        st.lxmf
            .lock()
            .ok()
            .and_then(|lxmf| {
                lxmf.as_ref()
                    .map(|mgr| mgr.pick_delivery_method(&st.db, &dh, pref, profile))
            })
            .unwrap_or(lxmf_core::constants::DeliveryMethod::Direct)
    })
    .await
    .map_err(|_| AppError::internal("delivery-method preflight task panicked"))?;

    if method != lxmf_core::constants::DeliveryMethod::Propagated {
        return Ok(());
    }

    let readiness = crate::propagation::ensure_relay_ready_for_send(state).await;
    if readiness == crate::propagation::RelayReadiness::Ready
        && destination_identity_known(state, dest_hash)
    {
        return Ok(());
    }

    let message = if readiness == crate::propagation::RelayReadiness::Ready {
        "Recipient identity key is not known yet. Scan or import their contact card, or wait for their LXMF announce before using Offline Inbox."
    } else {
        match readiness {
            crate::propagation::RelayReadiness::Offline => {
                "Network is offline. Offline Inbox will be checked again when an interface is online."
            }
            crate::propagation::RelayReadiness::Waiting => {
                "No reachable Offline Inbox is available yet. Ratspeak is looking for one."
            }
            crate::propagation::RelayReadiness::Unavailable => {
                "No Offline Inbox node is configured. Check Settings > Network."
            }
            crate::propagation::RelayReadiness::Ready => unreachable!(),
        }
    };

    state.emit_to_all(
        "lxmf_step",
        json!({
            "step": "error",
            "message": message,
            "client_msg_id": client_msg_id,
        }),
    );
    tracing::warn!(
        ?readiness,
        "propagation send held until a reachable Offline Inbox is available"
    );
    Err(AppError::conflict(message))
}

#[tauri::command]
#[tracing::instrument(
    level = "debug",
    name = "command.send_lxmf_message",
    skip_all,
    fields(
        dest_hash_len = args.dest_hash.len(),
        content_len = args.content.len(),
        has_title = args.title.is_some(),
    ),
)]
pub async fn send_lxmf_message(
    state: State<'_, Arc<AppState>>,
    args: SendLxmfArgs,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&args.dest_hash, 128);
    let content = sanitize_message_content(&args.content)?;
    let title = sanitize_text(args.title.as_deref().unwrap_or(""), 256);
    let delivery_pref = parse_delivery_preference(args.delivery_method.as_deref());
    let client_msg_id = normalize_lxmf_client_msg_id(args.client_msg_id.as_deref())?;

    if !validate_hex(&dest_hash, 16, 64) {
        emit_lxmf_send_error(
            &state,
            client_msg_id.as_deref(),
            "invalid_destination",
            "Invalid identity hash",
        );
        return Err(AppError::new(
            "invalid_destination",
            "Invalid identity hash",
        ));
    }
    if content.is_empty() {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Empty message" }),
        );
        return Err(AppError::bad_request("Empty message"));
    }
    validate_delivery_preference(&state, delivery_pref)?;
    let state_arc = Arc::clone(&state);
    let client_send = begin_lxmf_client_send(&state_arc, client_msg_id.as_ref())?;

    let activity_fence = state.activity_request_fence();
    let _ = crate::commands::shared::hydrate_contact_identity_for_send(&state, &dest_hash).await;
    if let Some(response) = cancelled_lxmf_client_send_response(&state, client_send.as_ref()) {
        return Ok(response);
    }
    let propagation_readiness = ensure_propagation_ready_for_send(
        &state,
        &dest_hash,
        delivery_pref,
        DeliveryProfile::Message,
        client_msg_id.as_deref(),
    )
    .await;
    if let Some(response) = cancelled_lxmf_client_send_response(&state, client_send.as_ref()) {
        return Ok(response);
    }
    propagation_readiness?;
    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let ct = content.clone();
    let tt = title.clone();
    let id_c = identity_id.clone();
    let cancellation = client_send
        .as_ref()
        .map(LxmfClientSendGuard::cancellation_probe);
    let send_result = tokio::task::spawn_blocking(move || {
        queue_lxmf_client_send(&st, cancellation.as_ref(), |manager| {
            manager
                .send_message_with_preference_report(MessageSendRequest {
                    dest_hash_hex: &dh,
                    content: &ct,
                    title: &tt,
                    db_pool: &st.db,
                    identity_id: &id_c,
                    preference: delivery_pref,
                    profile: DeliveryProfile::Message,
                })
                .ok()
        })
    })
    .await
    .map_err(|_| AppError::internal("send_message task panicked"))?;

    match send_result {
        LxmfClientSendAttempt::Queued(queued) => {
            let id = queued.message_id;
            if finalize_lxmf_client_send(&state_arc, client_send.as_ref(), &id).await? {
                return Ok(json!({
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                    "cancelled": true,
                }));
            }
            schedule_announce_after_user_send_from_origin(&state, &dest_hash, activity_fence);
            record_lxmf_delivery_queued(&state, activity_fence, &id, &dest_hash, queued.method);
            state.lxmf_notify.notify_one();
            state.emit_to_all(
                "lxmf_step",
                json!({
                    "step": "sending",
                    "message": "Message queued for delivery",
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                }),
            );
            broadcast_conversations(Arc::clone(&state));
            Ok(json!({ "msg_id": id, "client_msg_id": client_msg_id }))
        }
        LxmfClientSendAttempt::Cancelled => Ok(emit_prequeue_lxmf_cancellation(
            &state,
            client_msg_id.as_deref().unwrap_or_default(),
        )),
        LxmfClientSendAttempt::Failed(reason) => {
            record_lxmf_submission_failed(&state, activity_fence, &dest_hash, reason);
            let (message, error) = match reason {
                producer::LxmfSubmissionFailureReason::RouterUnavailable => (
                    "LXMF not initialized",
                    AppError::lxmf_not_initialized("LXMF not initialized"),
                ),
                producer::LxmfSubmissionFailureReason::PreparationFailed => (
                    "Message could not be queued",
                    AppError::internal("Message could not be queued"),
                ),
            };
            state.emit_to_all("lxmf_step", json!({ "step": "error", "message": message }));
            Err(error)
        }
    }
}

#[derive(Deserialize)]
pub struct SendReactionArgs {
    pub dest_hash: String,
    pub message_id: String,
    pub emoji: String,
    #[serde(default = "default_reaction_action")]
    pub action: String,
    #[serde(default)]
    pub delivery_method: Option<String>,
}

fn default_reaction_action() -> String {
    "add".to_string()
}

#[tauri::command]
pub async fn send_reaction(
    state: State<'_, Arc<AppState>>,
    args: SendReactionArgs,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&args.dest_hash, 128);
    let message_id = sanitize_text(&args.message_id, 128);
    let emoji = sanitize_text(&args.emoji, 16);
    let action = sanitize_text(&args.action, 16);
    let delivery_pref = parse_delivery_preference(args.delivery_method.as_deref());

    if message_id.is_empty() || emoji.is_empty() {
        return Err(AppError::bad_request("Missing message_id or emoji"));
    }
    validate_delivery_preference(&state, delivery_pref)?;
    let activity_fence = state.activity_request_fence();

    if validate_hex(&dest_hash, 16, 64) {
        let _ =
            crate::commands::shared::hydrate_contact_identity_for_send(&state, &dest_hash).await;
        ensure_propagation_ready_for_send(
            &state,
            &dest_hash,
            delivery_pref,
            DeliveryProfile::Message,
            None,
        )
        .await?;
    }

    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let mid = message_id.clone();
    let em = emoji.clone();
    let ac = action.clone();
    let id_c = identity_id.clone();
    let sent = tokio::task::spawn_blocking(move || {
        let send_lock_started = std::time::Instant::now();
        if let Ok(mut lxmf) = st.lxmf.lock() {
            let waited = send_lock_started.elapsed();
            if waited > std::time::Duration::from_secs(1) {
                tracing::warn!(
                    waited_ms = waited.as_millis() as u64,
                    "send waited on lxmf manager lock"
                );
            }
            if let Some(mgr) = lxmf.as_mut() {
                mgr.send_reaction_with_preference(ReactionSendRequest {
                    dest_hash_hex: &dh,
                    message_id: &mid,
                    emoji: &em,
                    action: &ac,
                    db_pool: &st.db,
                    identity_id: &id_c,
                    preference: delivery_pref,
                })
            } else {
                false
            }
        } else {
            false
        }
    })
    .await
    .unwrap_or(false);
    if sent {
        schedule_announce_after_user_send_from_origin(&state, &dest_hash, activity_fence);
        state.lxmf_notify.notify_one();
        let mid_for_db = message_id.clone();
        let id_for_db = identity_id.clone();
        let reactions = db::spawn_db(state.db.clone(), move |p| {
            db::get_reactions_for_message(&p, &mid_for_db, &id_for_db)
        })
        .await
        .unwrap_or_default();
        state.emit_to_all(
            "reaction_update",
            json!({
                "message_id": message_id,
                "reactions": reactions,
            }),
        );
        Ok(json!(null))
    } else {
        Err(AppError::lxmf_not_initialized("LXMF not initialized"))
    }
}

#[derive(Deserialize)]
pub struct SendReplyArgs {
    pub dest_hash: String,
    pub content: String,
    #[serde(default)]
    pub reply_to_id: Option<String>,
    #[serde(default)]
    pub reply_to_preview: Option<String>,
    #[serde(default)]
    pub delivery_method: Option<String>,
    #[serde(default)]
    pub client_msg_id: Option<String>,
}

#[tauri::command]
pub async fn send_lxmf_reply(
    state: State<'_, Arc<AppState>>,
    args: SendReplyArgs,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&args.dest_hash, 128);
    let content = sanitize_message_content(&args.content)?;
    let reply_to_id = sanitize_text(args.reply_to_id.as_deref().unwrap_or(""), 128);
    let reply_to_preview = sanitize_text(args.reply_to_preview.as_deref().unwrap_or(""), 200);
    let wire_reply_to_id = state
        .msg_id_map
        .lock()
        .ok()
        .and_then(|map| {
            map.iter()
                .find(|(_, client_id)| client_id.as_str() == reply_to_id)
                .map(|(msg_id, _)| msg_id.clone())
        })
        .unwrap_or_else(|| reply_to_id.clone());
    let delivery_pref = parse_delivery_preference(args.delivery_method.as_deref());
    let client_msg_id = normalize_lxmf_client_msg_id(args.client_msg_id.as_deref())?;

    if !validate_hex(&dest_hash, 16, 64) || content.is_empty() {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Invalid reply" }),
        );
        return Err(AppError::bad_request("Invalid reply"));
    }
    validate_delivery_preference(&state, delivery_pref)?;
    let state_arc = Arc::clone(&state);
    let client_send = begin_lxmf_client_send(&state_arc, client_msg_id.as_ref())?;
    let activity_fence = state.activity_request_fence();

    let _ = crate::commands::shared::hydrate_contact_identity_for_send(&state, &dest_hash).await;
    if let Some(response) = cancelled_lxmf_client_send_response(&state, client_send.as_ref()) {
        return Ok(response);
    }
    let propagation_readiness = ensure_propagation_ready_for_send(
        &state,
        &dest_hash,
        delivery_pref,
        DeliveryProfile::Message,
        client_msg_id.as_deref(),
    )
    .await;
    if let Some(response) = cancelled_lxmf_client_send_response(&state, client_send.as_ref()) {
        return Ok(response);
    }
    propagation_readiness?;
    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let ct = content.clone();
    let id_c = identity_id.clone();
    let reply_id_for_send = wire_reply_to_id.clone();
    let cancellation = client_send
        .as_ref()
        .map(LxmfClientSendGuard::cancellation_probe);
    let msg_id = tokio::task::spawn_blocking(move || {
        queue_lxmf_client_send(&st, cancellation.as_ref(), |manager| {
            manager.send_reply_with_preference(ReplyMessageSendRequest {
                dest_hash_hex: &dh,
                content: &ct,
                title: "",
                reply_to_id: &reply_id_for_send,
                reply_to_preview: &reply_to_preview,
                db_pool: &st.db,
                identity_id: &id_c,
                preference: delivery_pref,
                profile: DeliveryProfile::Message,
            })
        })
    })
    .await
    .map_err(|_| AppError::internal("send_reply task panicked"))?;

    match msg_id {
        LxmfClientSendAttempt::Queued(id) => {
            if finalize_lxmf_client_send(&state_arc, client_send.as_ref(), &id).await? {
                return Ok(json!({
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                    "cancelled": true,
                }));
            }
            schedule_announce_after_user_send_from_origin(&state, &dest_hash, activity_fence);
            state.lxmf_notify.notify_one();
            state.emit_to_all(
                "lxmf_step",
                json!({
                    "step": "sending",
                    "message": "Reply queued for delivery",
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                }),
            );
            broadcast_conversations(Arc::clone(&state));
            Ok(json!({ "msg_id": id, "client_msg_id": client_msg_id }))
        }
        LxmfClientSendAttempt::Cancelled => Ok(emit_prequeue_lxmf_cancellation(
            &state,
            client_msg_id.as_deref().unwrap_or_default(),
        )),
        LxmfClientSendAttempt::Failed(_) => {
            state.emit_to_all(
                "lxmf_step",
                json!({ "step": "error", "message": "LXMF not initialized" }),
            );
            Err(AppError::lxmf_not_initialized("LXMF not initialized"))
        }
    }
}

#[derive(Deserialize)]
pub struct SendPropagatedArgs {
    pub dest_hash: String,
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub client_msg_id: Option<String>,
}

#[tauri::command]
pub async fn send_lxmf_propagated(
    state: State<'_, Arc<AppState>>,
    args: SendPropagatedArgs,
) -> AppResult<Value> {
    use lxmf_core::constants::DeliveryMethod;

    let dest_hash = sanitize_text(&args.dest_hash, 128);
    let content = sanitize_message_content(&args.content)?;
    let title = sanitize_text(args.title.as_deref().unwrap_or(""), 200);
    let client_msg_id = normalize_lxmf_client_msg_id(args.client_msg_id.as_deref())?;

    if !validate_hex(&dest_hash, 16, 64) {
        emit_lxmf_send_error(
            &state,
            client_msg_id.as_deref(),
            "invalid_destination",
            "Invalid identity hash",
        );
        return Err(AppError::new(
            "invalid_destination",
            "Invalid identity hash",
        ));
    }
    if content.is_empty() {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Empty message" }),
        );
        return Err(AppError::bad_request("Empty message"));
    }

    validate_delivery_preference(&state, DeliveryPreference::Propagated)?;
    let state_arc = Arc::clone(&state);
    let client_send = begin_lxmf_client_send(&state_arc, client_msg_id.as_ref())?;

    let activity_fence = state.activity_request_fence();
    // Propagation still needs the recipient identity for encryption.
    let _ = crate::commands::shared::hydrate_contact_identity_for_send(&state, &dest_hash).await;
    if let Some(response) = cancelled_lxmf_client_send_response(&state, client_send.as_ref()) {
        return Ok(response);
    }
    let propagation_readiness = ensure_propagation_ready_for_send(
        &state,
        &dest_hash,
        DeliveryPreference::Propagated,
        DeliveryProfile::Message,
        client_msg_id.as_deref(),
    )
    .await;
    if let Some(response) = cancelled_lxmf_client_send_response(&state, client_send.as_ref()) {
        return Ok(response);
    }
    propagation_readiness?;
    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let ct = content.clone();
    let tt = title.clone();
    let id_c = identity_id.clone();
    let cancellation = client_send
        .as_ref()
        .map(LxmfClientSendGuard::cancellation_probe);
    let msg_id = tokio::task::spawn_blocking(move || {
        queue_lxmf_client_send(&st, cancellation.as_ref(), |manager| {
            manager.send_message_with_method(
                &dh,
                &ct,
                &tt,
                &st.db,
                &id_c,
                DeliveryMethod::Propagated,
            )
        })
    })
    .await
    .map_err(|_| AppError::internal("send_propagated task panicked"))?;

    match msg_id {
        LxmfClientSendAttempt::Queued(id) => {
            if finalize_lxmf_client_send(&state_arc, client_send.as_ref(), &id).await? {
                return Ok(json!({
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                    "cancelled": true,
                }));
            }
            schedule_announce_after_user_send_from_origin(&state, &dest_hash, activity_fence);
            record_lxmf_delivery_queued(
                &state,
                activity_fence,
                &id,
                &dest_hash,
                DeliveryMethod::Propagated,
            );
            state.lxmf_notify.notify_one();
            state.emit_to_all(
                "lxmf_step",
                json!({
                    "step": "sending",
                    "message": "Message queued for propagation",
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                }),
            );
            broadcast_conversations(Arc::clone(&state));
            Ok(json!({ "msg_id": id, "client_msg_id": client_msg_id }))
        }
        LxmfClientSendAttempt::Cancelled => Ok(emit_prequeue_lxmf_cancellation(
            &state,
            client_msg_id.as_deref().unwrap_or_default(),
        )),
        LxmfClientSendAttempt::Failed(_) => {
            state.emit_to_all(
                "lxmf_step",
                json!({
                    "step": "error",
                    "message": "LXMF not initialized",
                    "client_msg_id": client_msg_id,
                }),
            );
            Err(AppError::lxmf_not_initialized("LXMF not initialized"))
        }
    }
}

#[derive(Deserialize)]
pub struct SendWithAttachmentArgs {
    pub dest_hash: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub delivery_method: Option<String>,
    #[serde(default)]
    pub image_data: Option<String>,
    #[serde(default)]
    pub image_mime: Option<String>,
    #[serde(default)]
    pub file_data: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub client_msg_id: Option<String>,
}

#[tauri::command]
pub async fn send_lxmf_with_attachment(
    state: State<'_, Arc<AppState>>,
    args: SendWithAttachmentArgs,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&args.dest_hash, 128);
    let content = sanitize_message_content(args.content.as_deref().unwrap_or(""))?;
    let delivery_pref = parse_delivery_preference(args.delivery_method.as_deref());
    let client_msg_id = normalize_lxmf_client_msg_id(args.client_msg_id.as_deref())?;

    let is_image = args.image_data.as_deref().is_some_and(|s| !s.is_empty());
    let image_mime = if is_image {
        sanitize_text(args.image_mime.as_deref().unwrap_or("image/png"), 200)
    } else {
        String::new()
    };
    let file_name = if is_image {
        ensure_filename_extension(
            args.file_name.as_deref().unwrap_or("image"),
            &image_mime,
            "image",
        )
    } else {
        ensure_filename_extension(
            args.file_name.as_deref().unwrap_or("attachment"),
            "",
            "attachment",
        )
    };
    let file_data_b64: &str = if is_image {
        args.image_data.as_deref().unwrap_or("")
    } else {
        args.file_data.as_deref().unwrap_or("")
    };

    if !validate_hex(&dest_hash, 16, 64) {
        emit_lxmf_send_error(
            &state,
            client_msg_id.as_deref(),
            "invalid_destination",
            "Invalid identity hash",
        );
        return Err(AppError::new(
            "invalid_destination",
            "Invalid identity hash",
        ));
    }
    validate_delivery_preference(&state, delivery_pref)?;
    if file_data_b64.is_empty() {
        emit_lxmf_send_error(
            &state,
            client_msg_id.as_deref(),
            "attachment_missing",
            "No file data provided",
        );
        return Err(AppError::new("attachment_missing", "No file data provided"));
    }
    if base64_decoded_len_upper_bound(file_data_b64.len()).unwrap_or(usize::MAX)
        > rns_protocol::resource::MAX_RESOURCE_SIZE
    {
        emit_lxmf_send_error(
            &state,
            client_msg_id.as_deref(),
            "attachment_too_large",
            "Attachment exceeds protocol resource limit",
        );
        return Err(AppError::new(
            "attachment_too_large",
            "Attachment exceeds protocol resource limit",
        ));
    }
    let state_arc = Arc::clone(&state);
    let client_send = begin_lxmf_client_send(&state_arc, client_msg_id.as_ref())?;

    let file_bytes = B64.decode(file_data_b64).map_err(|_| {
        emit_lxmf_send_error(
            &state,
            client_msg_id.as_deref(),
            "attachment_invalid",
            "Invalid base64 file data",
        );
        AppError::new("attachment_invalid", "Invalid base64 file data")
    })?;
    if file_bytes.len() > rns_protocol::resource::MAX_RESOURCE_SIZE {
        emit_lxmf_send_error(
            &state,
            client_msg_id.as_deref(),
            "attachment_too_large",
            "Attachment exceeds protocol resource limit",
        );
        return Err(AppError::new(
            "attachment_too_large",
            "Attachment exceeds protocol resource limit",
        ));
    }

    let activity_fence = state.activity_request_fence();
    let _ = crate::commands::shared::hydrate_contact_identity_for_send(&state, &dest_hash).await;
    if let Some(response) = cancelled_lxmf_client_send_response(&state, client_send.as_ref()) {
        return Ok(response);
    }
    let propagation_readiness = ensure_propagation_ready_for_send(
        &state,
        &dest_hash,
        delivery_pref,
        DeliveryProfile::Attachment,
        client_msg_id.as_deref(),
    )
    .await;
    if let Some(response) = cancelled_lxmf_client_send_response(&state, client_send.as_ref()) {
        return Ok(response);
    }
    propagation_readiness?;
    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let ct = content.clone();
    let fn_c = file_name.clone();
    let im = image_mime.clone();
    let id_c = identity_id.clone();
    let cancellation = client_send
        .as_ref()
        .map(LxmfClientSendGuard::cancellation_probe);
    let send_result = tokio::task::spawn_blocking(move || {
        // Append "[File: …]" so non-attachment clients see the name.
        let msg_content = if ct.is_empty() {
            format!("[File: {}]", fn_c)
        } else {
            format!("{}\n[File: {}]", ct, fn_c)
        };
        queue_lxmf_client_send(&st, cancellation.as_ref(), |manager| {
            manager
                .send_message_with_attachment_fields_preference_report(AttachmentMessageRequest {
                    dest_hash_hex: &dh,
                    content: &msg_content,
                    title: "",
                    file_name: &fn_c,
                    file_bytes: &file_bytes,
                    is_image,
                    image_mime: &im,
                    db_pool: &st.db,
                    identity_id: &id_c,
                    preference: delivery_pref,
                })
                .ok()
        })
    })
    .await
    .map_err(|_| AppError::internal("send_attachment task panicked"))?;

    match send_result {
        LxmfClientSendAttempt::Queued(queued) => {
            let id = queued.message_id;
            if finalize_lxmf_client_send(&state_arc, client_send.as_ref(), &id).await? {
                return Ok(json!({
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                    "cancelled": true,
                }));
            }
            schedule_announce_after_user_send_from_origin(&state, &dest_hash, activity_fence);
            record_lxmf_delivery_queued(&state, activity_fence, &id, &dest_hash, queued.method);
            state.emit_to_all(
                "lxmf_step",
                json!({
                    "step": "sending",
                    "message": "Message with attachment queued for delivery",
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                }),
            );
            broadcast_conversations(Arc::clone(&state));
            state.lxmf_notify.notify_one();
            Ok(json!({ "msg_id": id, "client_msg_id": client_msg_id }))
        }
        LxmfClientSendAttempt::Cancelled => Ok(emit_prequeue_lxmf_cancellation(
            &state,
            client_msg_id.as_deref().unwrap_or_default(),
        )),
        LxmfClientSendAttempt::Failed(reason) => {
            record_lxmf_submission_failed(&state, activity_fence, &dest_hash, reason);
            let (code, message, error) = match reason {
                producer::LxmfSubmissionFailureReason::RouterUnavailable => (
                    "lxmf_not_initialized",
                    "LXMF not initialized",
                    AppError::lxmf_not_initialized("LXMF not initialized"),
                ),
                producer::LxmfSubmissionFailureReason::PreparationFailed => (
                    "lxmf_preparation_failed",
                    "Attachment could not be queued",
                    AppError::new("lxmf_preparation_failed", "Attachment could not be queued"),
                ),
            };
            emit_lxmf_send_error(&state, client_msg_id.as_deref(), code, message);
            Err(error)
        }
    }
}

fn resolve_lxmf_message_id_for_cancel(
    state: &AppState,
    msg_id: &str,
) -> Option<(String, Option<String>)> {
    if validate_hex(msg_id, 64, 64) {
        let client_msg_id = state
            .msg_id_map
            .lock()
            .ok()
            .and_then(|map| map.get(msg_id).cloned());
        return Some((msg_id.to_string(), client_msg_id));
    }

    state.msg_id_map.lock().ok().and_then(|map| {
        map.iter()
            .find(|(_, client_id)| client_id.as_str() == msg_id)
            .map(|(server_id, client_id)| (server_id.clone(), Some(client_id.clone())))
    })
}

async fn cancel_canonical_lxmf_message(
    state: &Arc<AppState>,
    msg_id: &str,
    client_msg_id: Option<&str>,
) -> AppResult<bool> {
    let activity_fence = state.activity_request_fence();
    let st = Arc::clone(state);
    let msg_id_for_cancel = msg_id.to_string();
    let transport_cancelled = tokio::task::spawn_blocking(move || {
        st.lxmf
            .lock()
            .ok()
            .and_then(|mut lxmf| {
                lxmf.as_mut()
                    .map(|mgr| mgr.cancel_outbound_message(&msg_id_for_cancel))
            })
            .unwrap_or(false)
    })
    .await
    .map_err(|_| AppError::internal("cancel_lxmf_message task panicked"))?;

    let msg_id_for_db = msg_id.to_string();
    let identity_for_db = active_identity_id(state);
    let (db_cancelled, method) = db::spawn_db(state.db.clone(), move |p| {
        let db_cancelled = db::cancel_outbound_message_state(&p, &msg_id_for_db, &identity_for_db);
        let method = db::get_message_delivery_method(&p, &msg_id_for_db, &identity_for_db);
        (db_cancelled, method)
    })
    .await
    .map_err(|_| AppError::internal("cancel_lxmf_message db task panicked"))?;

    let cancelled = transport_cancelled || db_cancelled;
    if cancelled {
        if let Ok(mut times) = state.message_send_times.lock() {
            times.remove(msg_id);
        }
        if let Ok(mut map) = state.msg_id_map.lock() {
            map.remove(msg_id);
        }
        state.emit_to_all(
            "lxmf_step",
            json!({
                "step": "cancelled",
                "msg_id": msg_id,
                "client_msg_id": client_msg_id,
                "method": method.clone(),
            }),
        );
        state.activity.record_event_fenced(
            || state.is_current_activity_origin_fence(activity_fence),
            || {
                let message = producer::MessageId::from_hex(msg_id)?;
                let method = method
                    .as_deref()
                    .and_then(producer::LxmfDeliveryMethod::from_code);
                Ok(producer::lxmf_delivery_state_changed(
                    producer::LxmfDeliveryStateChanged {
                        message,
                        state: producer::LxmfDeliveryState::Cancelled,
                        method,
                        rtt_ms: None,
                        failure_reason: None,
                    },
                ))
            },
        );
        broadcast_conversations(Arc::clone(state));
        state.lxmf_notify.notify_one();
    }
    Ok(cancelled)
}

async fn finalize_lxmf_client_send(
    state: &Arc<AppState>,
    guard: Option<&LxmfClientSendGuard>,
    canonical_msg_id: &str,
) -> AppResult<bool> {
    let Some(guard) = guard else {
        return Ok(false);
    };
    if let Ok(mut map) = state.msg_id_map.lock() {
        map.insert(
            canonical_msg_id.to_string(),
            guard.client_msg_id().to_string(),
        );
    }
    if !guard.publish_canonical(canonical_msg_id) {
        return Ok(false);
    }

    let cancelled =
        cancel_canonical_lxmf_message(state, canonical_msg_id, Some(guard.client_msg_id())).await?;
    if !cancelled {
        return Err(AppError::internal(
            "Queued message could not be cancelled before delivery",
        ));
    }
    Ok(true)
}

#[tauri::command]
pub async fn cancel_lxmf_message(
    state: State<'_, Arc<AppState>>,
    args: CancelLxmfMessageArgs,
) -> AppResult<Value> {
    let requested_msg_id = sanitize_text(&args.msg_id, 128);
    if requested_msg_id.is_empty() {
        return Err(AppError::bad_request("Missing message ID"));
    }

    let (msg_id, client_msg_id) = match state.cancel_lxmf_client_send(&requested_msg_id) {
        LxmfClientSendCancellation::Preparing => {
            return Ok(emit_prequeue_lxmf_cancellation(&state, &requested_msg_id));
        }
        LxmfClientSendCancellation::Queued { canonical_msg_id } => {
            (canonical_msg_id, Some(requested_msg_id.clone()))
        }
        LxmfClientSendCancellation::NotFound => {
            let Some(resolved) = resolve_lxmf_message_id_for_cancel(&state, &requested_msg_id)
            else {
                return Ok(json!({
                    "ok": true,
                    "cancelled": false,
                    "msg_id": requested_msg_id,
                }));
            };
            resolved
        }
    };
    let cancelled =
        cancel_canonical_lxmf_message(&state, &msg_id, client_msg_id.as_deref()).await?;

    Ok(json!({
        "ok": true,
        "cancelled": cancelled,
        "msg_id": msg_id,
        "client_msg_id": client_msg_id,
    }))
}

/// Marks inbound read; returns latest 100 + aggregate unread count.
#[tauri::command]
pub async fn get_conversation(state: State<'_, Arc<AppState>>, hash: String) -> AppResult<Value> {
    let dest_hash = sanitize_text(&hash, 128);
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request("Invalid identity hash"));
    }
    let identity_id = active_identity_id(&state);
    let dh = dest_hash.clone();
    let id_c = identity_id.clone();
    let (messages, unread_total) = db::spawn_db(state.db.clone(), move |p| {
        db::mark_read(&p, &dh, &id_c);
        let messages = db::get_conversation(&p, &dh, &id_c, 100);
        let total = if let Ok(conn) = p.get() {
            let counts = db::get_all_unread_counts_conn(&conn, &id_c);
            counts.values().sum::<i64>()
        } else {
            0
        };
        (messages, total)
    })
    .await
    .map_err(|_| AppError::internal("get_conversation db task panicked"))?;

    state.emit_to_all("unread_total", json!({ "count": unread_total }));
    broadcast_conversations(Arc::clone(&state));
    Ok(json!({ "hash": dest_hash, "messages": messages, "unread_total": unread_total }))
}

#[tauri::command]
pub async fn mark_read(state: State<'_, Arc<AppState>>, hash: String) -> AppResult<Value> {
    let dest_hash = sanitize_text(&hash, 128);
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request("Invalid identity hash"));
    }
    let identity_id = active_identity_id(&state);
    let dh = dest_hash.clone();
    let id_c = identity_id.clone();
    let total = db::spawn_db(state.db.clone(), move |p| {
        let conn = match p.get() {
            Ok(c) => c,
            Err(_) => return 0i64,
        };
        conn.execute(
            "UPDATE messages SET state = 'read' WHERE source = ?1 AND direction = 'inbound' AND state != 'read' AND identity_id = ?2",
            rusqlite::params![dh, id_c],
        ).ok();
        let counts = db::get_all_unread_counts_conn(&conn, &id_c);
        counts.values().sum::<i64>()
    })
    .await
    .map_err(|_| AppError::internal("mark_read db task panicked"))?;

    state.emit_to_all("unread_total", json!({ "count": total }));
    broadcast_conversations(Arc::clone(&state));
    Ok(json!({ "unread_total": total }))
}

#[tauri::command]
pub async fn hide_conversation(state: State<'_, Arc<AppState>>, hash: String) -> AppResult<Value> {
    let dest_hash = sanitize_text(&hash, 128);
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request("Invalid identity hash"));
    }
    let identity_id = active_identity_id(&state);
    let dh = dest_hash.clone();
    let id_c = identity_id.clone();
    let total = db::spawn_db(state.db.clone(), move |p| {
        let conn = match p.get() {
            Ok(c) => c,
            Err(_) => return 0i64,
        };
        if conn.execute(
            "INSERT OR REPLACE INTO hidden_conversations (dest_hash, identity_id) VALUES (?1, ?2)",
            rusqlite::params![dh, id_c],
        ).is_err() {
            tracing::warn!(reason = "insert_failed", "hide_conversation insert failed");
        }
        let counts = db::get_all_unread_counts_conn(&conn, &id_c);
        counts.values().sum::<i64>()
    })
    .await
    .map_err(|_| AppError::internal("hide_conversation db task panicked"))?;

    state.emit_to_all(
        "conversation_hidden",
        json!({ "ok": true, "hash": dest_hash }),
    );
    state.emit_to_all("unread_total", json!({ "count": total }));
    broadcast_conversations(Arc::clone(&state));
    Ok(json!({ "hash": dest_hash, "unread_total": total }))
}

#[tauri::command]
pub async fn delete_conversation(
    state: State<'_, Arc<AppState>>,
    hash: String,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&hash, 128);
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::bad_request("Invalid identity hash"));
    }
    let identity_id = active_identity_id(&state);

    // One blocking task so the lxmf MutexGuard never crosses an .await.
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let id_c = identity_id.clone();
    let total = tokio::task::spawn_blocking(move || {
        let mut file_refs = Vec::new();
        if let Ok(lxmf) = st.lxmf.lock() {
            if let Some(mgr) = lxmf.as_ref() {
                mgr.delete_conversation(&dh, &st.db, &id_c);
            } else {
                file_refs = db::delete_conversation(&st.db, &dh, &id_c);
            }
        } else {
            file_refs = db::delete_conversation(&st.db, &dh, &id_c);
        }
        if !file_refs.is_empty() {
            remove_stored_file_refs(&st.config.files_dir(), file_refs);
        }
        if let Ok(conn) = st.db.get() {
            let counts = db::get_all_unread_counts_conn(&conn, &id_c);
            counts.values().sum::<i64>()
        } else {
            0i64
        }
    })
    .await
    .unwrap_or(0);

    state.emit_to_all(
        "conversation_deleted",
        json!({ "ok": true, "hash": dest_hash }),
    );
    state.emit_to_all("unread_total", json!({ "count": total }));
    broadcast_conversations(Arc::clone(&state));
    Ok(json!({ "hash": dest_hash, "unread_total": total }))
}

#[tauri::command]
pub async fn api_files(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let files = if let Ok(lxmf) = state.lxmf.lock() {
        lxmf.as_ref()
            .map(|mgr| mgr.list_received_files())
            .unwrap_or_default()
    } else {
        vec![]
    };
    Ok(json!(files))
}

#[tauri::command]
pub async fn api_lxmf_limits(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let node_hex = state.lxmf.lock().ok().and_then(|l| {
        l.as_ref()
            .and_then(|m| m.configured_propagation_node.map(hex::encode))
    });
    let propagation_transfer_limit_kb = node_hex.as_ref().and_then(|h| {
        state
            .discovered_propagation_nodes
            .lock()
            .ok()
            .and_then(|nodes| nodes.get(h).cloned())
            .and_then(|n| n.get("transfer_limit_kb").and_then(|v| v.as_f64()))
    });
    Ok(json!({
        "max_attachment_bytes": rns_protocol::resource::MAX_RESOURCE_SIZE,
        "max_message_bytes": MAX_LXMF_MESSAGE_BYTES,
        "efficient_resource_bytes": rns_protocol::resource::MAX_EFFICIENT_SIZE,
        "default_propagation_limit_kb": lxmf_core::constants::PROPAGATION_LIMIT,
        "propagation_transfer_limit_kb": propagation_transfer_limit_kb,
    }))
}

#[derive(Serialize)]
pub struct FileDownload {
    pub mime: String,
    pub filename: String,
    /// Base64 (Tauri JSON IPC encodes Vec<u8> as number array; 6× the wire).
    pub data_base64: String,
}

#[tauri::command]
pub async fn api_file_download(
    state: State<'_, Arc<AppState>>,
    stored_name: String,
) -> AppResult<FileDownload> {
    // get_received_file applies the shared stored-filename sanitizer.
    let file_path = if let Ok(lxmf) = state.lxmf.lock() {
        lxmf.as_ref()
            .and_then(|mgr| mgr.get_received_file(&stored_name))
    } else {
        None
    };
    let path = file_path.ok_or_else(|| AppError::not_found("File not found"))?;
    let data = tokio::fs::read(&path).await.map_err(|_| {
        tracing::warn!(reason = "read_failed", "file-download read failed");
        AppError::not_found("File not found")
    })?;
    let mime = if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("lxvm"))
    {
        "audio/x-lxst-voice-memo".to_string()
    } else {
        mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string()
    };
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());
    // Strip the `<ts>_` storage prefix.
    let clean = filename
        .find('_')
        .map(|p| filename[p + 1..].to_string())
        .unwrap_or(filename);
    Ok(FileDownload {
        mime,
        filename: clean,
        data_base64: B64.encode(&data),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DashboardConfig;
    use crate::db::init_schema;
    use r2d2_sqlite::SqliteConnectionManager;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_MESSAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_state_with_schema() -> Arc<AppState> {
        let unique = TEMP_MESSAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-msg-test-{}-{}-{unique}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = DashboardConfig::from_env_and_defaults(tmp);
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(mgr).unwrap();
        init_schema(&pool).expect("init_schema");
        Arc::new(AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ))
    }

    #[test]
    fn base64_decode_upper_bound_rejects_protocol_oversize_before_decode() {
        let encoded_len = (rns_protocol::resource::MAX_RESOURCE_SIZE / 3 + 1) * 4;
        assert!(
            base64_decoded_len_upper_bound(encoded_len).unwrap()
                > rns_protocol::resource::MAX_RESOURCE_SIZE
        );
        assert_eq!(base64_decoded_len_upper_bound(4), Some(3));
    }

    #[test]
    fn attachment_filenames_keep_or_gain_expected_extensions() {
        assert_eq!(
            ensure_filename_extension("screen", "image/png", "image"),
            "screen.png"
        );
        assert_eq!(
            ensure_filename_extension("screen.jpg", "image/png", "image"),
            "screen.jpg"
        );
        assert_eq!(
            ensure_filename_extension("", "image/jpeg", "image"),
            "image.jpg"
        );
        assert_eq!(
            ensure_filename_extension("archive", "", "attachment"),
            "archive"
        );
    }

    #[test]
    fn optimistic_message_ids_use_the_bounded_native_namespace() {
        let valid = format!("out_{}", "a".repeat(32));
        assert_eq!(
            normalize_lxmf_client_msg_id(Some(&valid)).unwrap(),
            Some(valid)
        );
        assert!(normalize_lxmf_client_msg_id(None).unwrap().is_none());
        for invalid in [
            "",
            "out_short",
            "out_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(normalize_lxmf_client_msg_id(Some(invalid)).is_err());
        }
    }

    /// Catches column-name drift between inline SQL and schema in `db.rs`.
    #[tokio::test]
    async fn build_conversations_payload_succeeds_with_hidden_and_blocked_rows() {
        let state = make_state_with_schema();
        {
            let conn = state.db.get().unwrap();
            conn.execute(
                "INSERT INTO hidden_conversations (dest_hash, identity_id, hidden_at) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params!["aaaa", "me", 0.0],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO blocked_contacts (dest_hash, identity_id, blocked_at) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params!["bbbb", "me", 0.0],
            )
            .unwrap();
        }
        let payload = build_conversations_payload(&state)
            .await
            .expect("build_conversations_payload should succeed against the real schema");
        assert!(payload.is_array(), "payload should be a JSON array");
    }
}
