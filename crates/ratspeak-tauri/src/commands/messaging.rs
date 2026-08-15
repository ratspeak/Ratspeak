//! Conversation reads + message send + search + file downloads.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::commands::shared::remove_stored_file_refs;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::{active_identity_id, sanitize_text, validate_hex};
#[cfg(feature = "lxst-voice")]
use crate::lxmf::AudioMessageRequest;
use crate::lxmf::{
    AttachmentMessageRequest, DeliveryPreference, DeliveryProfile, LxmfManager,
    LxmfSubmissionFailure, MessageSendRequest, ReactionSendRequest, ReplyMessageSendRequest,
};
use crate::state::{
    AppState, AttachmentTransferAdmissionError, AttachmentTransferLease,
    ImageAttachmentStagingError, LxmfClientSendAdmissionError, LxmfClientSendCancellation,
    LxmfClientSendCancellationProbe, LxmfClientSendGuard, StagedAttachment,
};
use ratspeak_runtime::activity::producer;
use ratspeak_runtime::image_attachment::{
    ImageAttachmentDisposition, ImageAttachmentError, ImageSizeProfile, inspect_image_attachment,
    prepare_image_attachment, unavailable_image_inspection,
};

const MAX_LXMF_MESSAGE_BYTES: usize = rns_protocol::resource::MAX_RESOURCE_SIZE;
const ATTACHMENT_IPC_CHUNK_BYTES: usize = 256 * 1024;
const LEGACY_BASE64_ATTACHMENT_MAX_BYTES: usize = 1_000_000;
const INLINE_IMAGE_MAX_PIXELS: u64 = 16_000_000;
const INLINE_IMAGE_MAX_DIMENSION: u32 = 8192;

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

pub(crate) fn normalize_lxmf_client_msg_id(raw: Option<&str>) -> AppResult<Option<String>> {
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
            "may_have_left_device": false,
        }),
    );
    json!({
        "ok": true,
        "cancelled": true,
        "client_msg_id": client_msg_id,
        "may_have_left_device": false,
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
    let dest_hash = sanitize_text(&args.dest_hash, 128).to_ascii_lowercase();
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
                producer::LxmfSubmissionFailureReason::AttachmentBusy => (
                    "Another attachment transfer is already active",
                    AppError::conflict("Another attachment transfer is already active"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentMemoryPressure => (
                    "Attachment transfers are paused while memory recovers",
                    AppError::conflict("Attachment transfers are paused while memory recovers"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentTooLarge => (
                    "Attachment exceeds the configured receive limit",
                    AppError::bad_request("Attachment exceeds the configured receive limit"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentEnvelopeTooLarge => (
                    "Attachment and message metadata exceed the protocol resource limit",
                    AppError::bad_request(
                        "Attachment and message metadata exceed the protocol resource limit",
                    ),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentStorageFailed => (
                    "Attachment storage is unavailable",
                    AppError::internal("Attachment storage is unavailable"),
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
    let dest_hash = sanitize_text(&args.dest_hash, 128).to_ascii_lowercase();
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
    let dest_hash = sanitize_text(&args.dest_hash, 128).to_ascii_lowercase();
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

    let dest_hash = sanitize_text(&args.dest_hash, 128).to_ascii_lowercase();
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

enum AttachmentLeaseOwner {
    Direct { lease: AttachmentTransferLease },
    Staged { staged: StagedAttachment },
}

impl AttachmentLeaseOwner {
    fn into_transfer_lease(self) -> AttachmentTransferLease {
        match self {
            Self::Direct { lease } => lease,
            Self::Staged { staged } => staged.into_transfer_lease(),
        }
    }

    fn staged_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Direct { .. } => None,
            Self::Staged { staged } => Some(&staged.path),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn queue_prepared_attachment(
    state: Arc<AppState>,
    dest_hash: String,
    content: String,
    delivery_pref: DeliveryPreference,
    client_msg_id: Option<String>,
    file_name: String,
    file_bytes: Vec<u8>,
    is_image: bool,
    image_mime: String,
    attachment_owner: AttachmentLeaseOwner,
) -> AppResult<Value> {
    let client_send = begin_lxmf_client_send(&state, client_msg_id.as_ref())?;
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
    let st = Arc::clone(&state);
    let dh = dest_hash.clone();
    let ct = content.clone();
    let fn_c = file_name.clone();
    let im = image_mime.clone();
    let cancellation = client_send
        .as_ref()
        .map(LxmfClientSendGuard::cancellation_probe);
    let staged_path = attachment_owner
        .staged_path()
        .map(std::path::Path::to_path_buf);
    let (send_result, attachment_owner) = tokio::task::spawn_blocking(move || {
        let msg_content = if ct.is_empty() {
            format!("[File: {}]", fn_c)
        } else {
            format!("{}\n[File: {}]", ct, fn_c)
        };
        let attempt = queue_lxmf_client_send(&st, cancellation.as_ref(), |manager| {
            Some(
                manager.send_message_with_attachment_fields_preference_report(
                    AttachmentMessageRequest {
                        dest_hash_hex: &dh,
                        content: &msg_content,
                        title: "",
                        file_name: &fn_c,
                        file_bytes: &file_bytes,
                        staged_path: staged_path.as_deref(),
                        is_image,
                        image_mime: &im,
                        db_pool: &st.db,
                        identity_id: &identity_id,
                        preference: delivery_pref,
                    },
                ),
            )
        });
        (attempt, attachment_owner)
    })
    .await
    .map_err(|_| AppError::internal("send_attachment task panicked"))?;

    match send_result {
        LxmfClientSendAttempt::Queued(Ok(queued)) => {
            let id = queued.message_id;
            state
                .hold_attachment_delivery_lease(id.clone(), attachment_owner.into_transfer_lease());
            if finalize_lxmf_client_send(&state, client_send.as_ref(), &id).await? {
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
        LxmfClientSendAttempt::Queued(Err(LxmfSubmissionFailure::ResourceLimitExceeded {
            actual_bytes,
            limit_bytes,
        })) => {
            record_lxmf_submission_failed(
                &state,
                activity_fence,
                &dest_hash,
                producer::LxmfSubmissionFailureReason::AttachmentEnvelopeTooLarge,
            );
            emit_lxmf_send_error(
                &state,
                client_msg_id.as_deref(),
                "attachment_envelope_too_large",
                "Attachment and message metadata exceed the protocol resource limit",
            );
            Err(AppError::new(
                "attachment_envelope_too_large",
                format!(
                    "Attachment and message metadata use {actual_bytes} bytes; the protocol limit is {limit_bytes} bytes"
                ),
            ))
        }
        LxmfClientSendAttempt::Queued(Err(LxmfSubmissionFailure::PreparationFailed)) => {
            record_lxmf_submission_failed(
                &state,
                activity_fence,
                &dest_hash,
                producer::LxmfSubmissionFailureReason::PreparationFailed,
            );
            emit_lxmf_send_error(
                &state,
                client_msg_id.as_deref(),
                "lxmf_preparation_failed",
                "Attachment could not be queued",
            );
            Err(AppError::new(
                "lxmf_preparation_failed",
                "Attachment could not be queued",
            ))
        }
        LxmfClientSendAttempt::Queued(Err(LxmfSubmissionFailure::StorageFailed)) => {
            record_lxmf_submission_failed(
                &state,
                activity_fence,
                &dest_hash,
                producer::LxmfSubmissionFailureReason::AttachmentStorageFailed,
            );
            emit_lxmf_send_error(
                &state,
                client_msg_id.as_deref(),
                "attachment_storage_failed",
                "Attachment storage is unavailable",
            );
            Err(AppError::new(
                "attachment_storage_failed",
                "Attachment storage is unavailable",
            ))
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
                producer::LxmfSubmissionFailureReason::AttachmentBusy => (
                    "attachment_busy",
                    "Another attachment transfer is already active",
                    AppError::conflict("Another attachment transfer is already active"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentMemoryPressure => (
                    "attachment_memory_pressure",
                    "Attachment transfers are paused while memory recovers",
                    AppError::conflict("Attachment transfers are paused while memory recovers"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentTooLarge => (
                    "attachment_too_large",
                    "Attachment exceeds the configured receive limit",
                    AppError::bad_request("Attachment exceeds the configured receive limit"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentEnvelopeTooLarge => (
                    "attachment_envelope_too_large",
                    "Attachment and message metadata exceed the protocol resource limit",
                    AppError::bad_request(
                        "Attachment and message metadata exceed the protocol resource limit",
                    ),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentStorageFailed => (
                    "attachment_storage_failed",
                    "Attachment storage is unavailable",
                    AppError::internal("Attachment storage is unavailable"),
                ),
            };
            emit_lxmf_send_error(&state, client_msg_id.as_deref(), code, message);
            Err(error)
        }
    }
}

#[cfg(feature = "lxst-voice")]
pub(crate) async fn queue_prepared_audio(
    state: Arc<AppState>,
    dest_hash: String,
    delivery_pref: DeliveryPreference,
    client_msg_id: Option<String>,
    audio_bytes: Vec<u8>,
    staged: StagedAttachment,
) -> AppResult<Value> {
    let client_send = begin_lxmf_client_send(&state, client_msg_id.as_ref())?;
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
    let st = Arc::clone(&state);
    let dh = dest_hash.clone();
    let cancellation = client_send
        .as_ref()
        .map(LxmfClientSendGuard::cancellation_probe);
    let staged_path = staged.path.clone();
    let (send_result, staged) = tokio::task::spawn_blocking(move || {
        let attempt = queue_lxmf_client_send(&st, cancellation.as_ref(), |manager| {
            Some(
                manager.send_audio_message_with_preference_report(AudioMessageRequest {
                    dest_hash_hex: &dh,
                    content: "Voice message",
                    title: "",
                    audio_bytes: &audio_bytes,
                    staged_path: Some(&staged_path),
                    db_pool: &st.db,
                    identity_id: &identity_id,
                    preference: delivery_pref,
                }),
            )
        });
        (attempt, staged)
    })
    .await
    .map_err(|_| AppError::internal("send_audio task panicked"))?;

    match send_result {
        LxmfClientSendAttempt::Queued(Ok(queued)) => {
            let id = queued.message_id;
            state.hold_attachment_delivery_lease(id.clone(), staged.into_transfer_lease());
            if finalize_lxmf_client_send(&state, client_send.as_ref(), &id).await? {
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
                    "message": "Voice message queued for delivery",
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                }),
            );
            broadcast_conversations(Arc::clone(&state));
            state.lxmf_notify.notify_one();
            Ok(json!({ "msg_id": id, "client_msg_id": client_msg_id }))
        }
        LxmfClientSendAttempt::Queued(Err(LxmfSubmissionFailure::ResourceLimitExceeded {
            actual_bytes,
            limit_bytes,
        })) => {
            record_lxmf_submission_failed(
                &state,
                activity_fence,
                &dest_hash,
                producer::LxmfSubmissionFailureReason::AttachmentEnvelopeTooLarge,
            );
            emit_lxmf_send_error(
                &state,
                client_msg_id.as_deref(),
                "audio_envelope_too_large",
                "Voice message exceeds the protocol resource limit",
            );
            Err(AppError::new(
                "audio_envelope_too_large",
                format!(
                    "Voice message uses {actual_bytes} bytes; the protocol limit is {limit_bytes} bytes"
                ),
            ))
        }
        LxmfClientSendAttempt::Queued(Err(LxmfSubmissionFailure::PreparationFailed)) => {
            record_lxmf_submission_failed(
                &state,
                activity_fence,
                &dest_hash,
                producer::LxmfSubmissionFailureReason::PreparationFailed,
            );
            emit_lxmf_send_error(
                &state,
                client_msg_id.as_deref(),
                "audio_invalid",
                "Voice message could not be queued",
            );
            Err(AppError::new(
                "audio_invalid",
                "Voice message could not be queued",
            ))
        }
        LxmfClientSendAttempt::Queued(Err(LxmfSubmissionFailure::StorageFailed)) => {
            record_lxmf_submission_failed(
                &state,
                activity_fence,
                &dest_hash,
                producer::LxmfSubmissionFailureReason::AttachmentStorageFailed,
            );
            emit_lxmf_send_error(
                &state,
                client_msg_id.as_deref(),
                "audio_storage_failed",
                "Voice message storage is unavailable",
            );
            Err(AppError::new(
                "audio_storage_failed",
                "Voice message storage is unavailable",
            ))
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
                    "audio_invalid",
                    "Voice message could not be queued",
                    AppError::new("audio_invalid", "Voice message could not be queued"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentBusy => (
                    "attachment_busy",
                    "Another media transfer is already active",
                    AppError::conflict("Another media transfer is already active"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentMemoryPressure => (
                    "attachment_memory_pressure",
                    "Media transfers are paused while memory recovers",
                    AppError::conflict("Media transfers are paused while memory recovers"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentTooLarge => (
                    "audio_too_large",
                    "Voice message exceeds the supported size",
                    AppError::bad_request("Voice message exceeds the supported size"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentEnvelopeTooLarge => (
                    "audio_envelope_too_large",
                    "Voice message exceeds the protocol resource limit",
                    AppError::bad_request("Voice message exceeds the protocol resource limit"),
                ),
                producer::LxmfSubmissionFailureReason::AttachmentStorageFailed => (
                    "audio_storage_failed",
                    "Voice message storage is unavailable",
                    AppError::internal("Voice message storage is unavailable"),
                ),
            };
            emit_lxmf_send_error(&state, client_msg_id.as_deref(), code, message);
            Err(error)
        }
    }
}

#[tauri::command]
pub async fn send_lxmf_with_attachment(
    state: State<'_, Arc<AppState>>,
    args: SendWithAttachmentArgs,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&args.dest_hash, 128).to_ascii_lowercase();
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
        > LEGACY_BASE64_ATTACHMENT_MAX_BYTES
    {
        emit_lxmf_send_error(
            &state,
            client_msg_id.as_deref(),
            "attachment_too_large",
            "Legacy attachment input is limited to 1 MB; use staged transfer",
        );
        return Err(AppError::new(
            "attachment_too_large",
            "Legacy attachment input is limited to 1 MB; use staged transfer",
        ));
    }
    let state_arc = Arc::clone(&state);

    let decoded_upper_bound = base64_decoded_len_upper_bound(file_data_b64.len())
        .ok_or_else(|| AppError::new("attachment_too_large", "Attachment is too large"))?;
    let attachment_lease = state_arc
        .reserve_attachment_transfer(decoded_upper_bound)
        .map_err(|error| {
            let (code, message) = match error {
                AttachmentTransferAdmissionError::Busy => (
                    "attachment_busy",
                    "Another large attachment is being prepared",
                ),
                AttachmentTransferAdmissionError::MemoryPressure => (
                    "attachment_memory_pressure",
                    "Attachment memory budget is currently full",
                ),
                AttachmentTransferAdmissionError::TooLarge => (
                    "attachment_too_large",
                    "Attachment exceeds the supported limit",
                ),
                AttachmentTransferAdmissionError::Storage => (
                    "attachment_storage_failed",
                    "Could not create private attachment staging",
                ),
            };
            emit_lxmf_send_error(&state, client_msg_id.as_deref(), code, message);
            AppError::new(code, message)
        })?;

    let file_bytes = B64.decode(file_data_b64).map_err(|_| {
        emit_lxmf_send_error(
            &state,
            client_msg_id.as_deref(),
            "attachment_invalid",
            "Invalid base64 file data",
        );
        AppError::new("attachment_invalid", "Invalid base64 file data")
    })?;
    if file_bytes.len() > LEGACY_BASE64_ATTACHMENT_MAX_BYTES {
        emit_lxmf_send_error(
            &state,
            client_msg_id.as_deref(),
            "attachment_too_large",
            "Legacy attachment input is limited to 1 MB; use staged transfer",
        );
        return Err(AppError::new(
            "attachment_too_large",
            "Legacy attachment input is limited to 1 MB; use staged transfer",
        ));
    }

    queue_prepared_attachment(
        state_arc,
        dest_hash,
        content,
        delivery_pref,
        client_msg_id,
        file_name,
        file_bytes,
        is_image,
        image_mime,
        AttachmentLeaseOwner::Direct {
            lease: attachment_lease,
        },
    )
    .await
}

#[derive(Deserialize)]
pub struct BeginAttachmentStageArgs {
    pub file_name: String,
    pub mime: String,
    pub declared_size: usize,
    #[serde(default)]
    pub dest_hash: Option<String>,
    #[serde(default)]
    pub is_image: bool,
}

fn validate_outbound_attachment_size(declared_size: usize) -> AppResult<()> {
    if declared_size == 0 || declared_size > ratspeak_runtime::state::LXMF_DELIVERY_LIMIT_MAX_BYTES
    {
        return Err(AppError::new(
            "attachment_too_large",
            "Attachment exceeds the supported limit",
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn begin_attachment_stage(
    state: State<'_, Arc<AppState>>,
    args: BeginAttachmentStageArgs,
) -> AppResult<Value> {
    validate_outbound_attachment_size(args.declared_size)?;
    let activity_fence = state.activity_request_fence();
    let destination = args
        .dest_hash
        .as_deref()
        .map(|value| sanitize_text(value, 128))
        .filter(|value| validate_hex(value, 16, 64));
    let mime = sanitize_text(&args.mime, 200);
    let file_name = ensure_filename_extension(
        &args.file_name,
        if args.is_image { &mime } else { "" },
        if args.is_image { "image" } else { "attachment" },
    );
    let token = Arc::clone(&state)
        .begin_attachment_staging(file_name, mime, args.declared_size, args.is_image)
        .map_err(|error| {
            let (reason, app_error) = match error {
                AttachmentTransferAdmissionError::Busy => (
                    producer::LxmfSubmissionFailureReason::AttachmentBusy,
                    AppError::conflict("Another large attachment is being prepared"),
                ),
                AttachmentTransferAdmissionError::MemoryPressure => (
                    producer::LxmfSubmissionFailureReason::AttachmentMemoryPressure,
                    AppError::service_unavailable("Attachment memory budget is currently full"),
                ),
                AttachmentTransferAdmissionError::TooLarge => (
                    producer::LxmfSubmissionFailureReason::AttachmentTooLarge,
                    AppError::new(
                        "attachment_too_large",
                        "Attachment exceeds the supported limit",
                    ),
                ),
                AttachmentTransferAdmissionError::Storage => (
                    producer::LxmfSubmissionFailureReason::AttachmentStorageFailed,
                    AppError::new(
                        "attachment_storage_failed",
                        "Could not create private attachment staging",
                    ),
                ),
            };
            if let Some(destination) = destination.as_deref() {
                record_lxmf_submission_failed(&state, activity_fence, destination, reason);
            }
            app_error
        })?;
    Ok(json!({
        "token": token,
        "chunk_bytes": ATTACHMENT_IPC_CHUNK_BYTES,
    }))
}

#[derive(Deserialize)]
pub struct AppendAttachmentStageArgs {
    pub token: String,
    pub offset: usize,
    pub data_base64: String,
}

#[tauri::command]
pub async fn append_attachment_stage(
    state: State<'_, Arc<AppState>>,
    args: AppendAttachmentStageArgs,
) -> AppResult<Value> {
    let decoded_bound = base64_decoded_len_upper_bound(args.data_base64.len())
        .ok_or_else(|| AppError::bad_request("Invalid attachment chunk"))?;
    if decoded_bound > ATTACHMENT_IPC_CHUNK_BYTES {
        return Err(AppError::bad_request("Attachment chunk is too large"));
    }
    let bytes = B64
        .decode(args.data_base64)
        .map_err(|_| AppError::bad_request("Invalid attachment chunk"))?;
    if bytes.is_empty() || bytes.len() > ATTACHMENT_IPC_CHUNK_BYTES {
        return Err(AppError::bad_request("Invalid attachment chunk"));
    }
    let st = Arc::clone(&state);
    let token = args.token;
    let written = tokio::task::spawn_blocking(move || {
        st.append_attachment_staging(&token, args.offset, &bytes)
    })
    .await
    .map_err(|_| AppError::internal("attachment staging task panicked"))?
    .map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => AppError::not_found("Attachment staging expired"),
        std::io::ErrorKind::InvalidInput => AppError::bad_request("Invalid attachment chunk"),
        _ => AppError::new("attachment_storage_failed", "Could not stage attachment"),
    })?;
    Ok(json!({ "written": written }))
}

#[tauri::command]
pub async fn cancel_attachment_stage(
    state: State<'_, Arc<AppState>>,
    token: String,
) -> AppResult<Value> {
    let st = Arc::clone(&state);
    let removed = tokio::task::spawn_blocking(move || st.cancel_attachment_staging(&token))
        .await
        .unwrap_or(false);
    Ok(json!({ "cancelled": removed }))
}

fn map_image_staging_error(error: ImageAttachmentStagingError) -> AppError {
    match error {
        ImageAttachmentStagingError::NotFound => AppError::not_found("Image staging expired"),
        ImageAttachmentStagingError::InvalidState => {
            AppError::conflict("Image staging is not ready")
        }
        ImageAttachmentStagingError::Admission(error) => match error {
            AttachmentTransferAdmissionError::Busy => {
                AppError::conflict("Another large attachment is being prepared")
            }
            AttachmentTransferAdmissionError::MemoryPressure => {
                AppError::service_unavailable("Attachment memory budget is currently full")
            }
            AttachmentTransferAdmissionError::TooLarge => AppError::new(
                "attachment_too_large",
                "Prepared image exceeds the supported attachment limit",
            ),
            AttachmentTransferAdmissionError::Storage => AppError::new(
                "attachment_storage_failed",
                "Could not update private attachment staging",
            ),
        },
    }
}

#[derive(Deserialize)]
pub struct ImageAttachmentStageArgs {
    pub token: String,
}

#[tauri::command]
pub async fn inspect_image_attachment_stage(
    state: State<'_, Arc<AppState>>,
    args: ImageAttachmentStageArgs,
) -> AppResult<Value> {
    let _preparation = state.image_preparation_lock.lock().await;
    let snapshot = state
        .inspect_staged_image_attachment(&args.token)
        .map_err(map_image_staging_error)?;
    let source_size = snapshot.source_size;
    let inspection = tokio::task::spawn_blocking(move || inspect_image_attachment(&snapshot.path))
        .await
        .map_err(|_| AppError::internal("image inspection task panicked"))?;
    let inspection = match inspection {
        Ok(inspection) => inspection,
        Err(ImageAttachmentError::TooLarge) => {
            unavailable_image_inspection(source_size, ImageAttachmentDisposition::TooLarge)
        }
        Err(ImageAttachmentError::Io(_)) => {
            return Err(AppError::new(
                "attachment_storage_failed",
                "Could not inspect private image staging",
            ));
        }
        Err(_) => {
            unavailable_image_inspection(source_size, ImageAttachmentDisposition::Unsupported)
        }
    };
    Ok(json!(inspection))
}

#[derive(Deserialize)]
pub struct PrepareImageAttachmentStageArgs {
    pub token: String,
    pub profile: ImageSizeProfile,
}

#[tauri::command]
pub async fn prepare_image_attachment_stage(
    state: State<'_, Arc<AppState>>,
    args: PrepareImageAttachmentStageArgs,
) -> AppResult<Value> {
    let _preparation = state.image_preparation_lock.lock().await;
    let snapshot = state
        .begin_staged_image_preparation(&args.token)
        .map_err(map_image_staging_error)?;
    let output_path = snapshot.path.with_file_name(format!(
        "{}.prepared.{}",
        args.token, snapshot.preparation_revision
    ));
    let source_path = snapshot.path.clone();
    let source_name = snapshot.file_name.clone();
    let profile = args.profile;
    let prepared_path = output_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        prepare_image_attachment(
            &source_path,
            &prepared_path,
            &source_name,
            profile,
            ratspeak_runtime::state::LXMF_DELIVERY_LIMIT_MAX_BYTES,
        )
    })
    .await
    .map_err(|_| AppError::internal("image preparation task panicked"));

    let prepared = match result {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => {
            let _ = tokio::fs::remove_file(&output_path).await;
            state.abort_staged_image_preparation(&args.token, snapshot.preparation_revision);
            return Err(match error {
                ImageAttachmentError::Animated => AppError::new(
                    "attachment_image_animated",
                    "Animated images can be sent as files",
                ),
                ImageAttachmentError::TooLarge => AppError::new(
                    "attachment_image_too_large",
                    "This image is too large to resize safely",
                ),
                ImageAttachmentError::CannotMeetProfile => AppError::new(
                    "attachment_image_profile_failed",
                    "Could not prepare the selected photo size",
                ),
                ImageAttachmentError::OutputTooLarge => AppError::new(
                    "attachment_too_large",
                    "Prepared image exceeds the supported attachment limit",
                ),
                ImageAttachmentError::Io(_) => AppError::new(
                    "attachment_storage_failed",
                    "Could not prepare private image staging",
                ),
                ImageAttachmentError::Unsupported | ImageAttachmentError::Codec(_) => {
                    AppError::new(
                        "attachment_image_unsupported",
                        "This image format can be sent as a file",
                    )
                }
            });
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&output_path).await;
            state.abort_staged_image_preparation(&args.token, snapshot.preparation_revision);
            return Err(error);
        }
    };

    let finish_state = Arc::clone(&state);
    let finish_token = args.token.clone();
    let finish_path = prepared.path.clone();
    let finish_name = prepared.file_name.clone();
    let finish_mime = prepared.mime.to_string();
    let finish_size = prepared.size;
    let finish_revision = snapshot.preparation_revision;
    let finish_result = tokio::task::spawn_blocking(move || {
        finish_state.finish_staged_image_preparation(
            &finish_token,
            finish_revision,
            finish_path,
            finish_name,
            finish_mime,
            finish_size,
        )
    })
    .await;
    let finish_result = match finish_result {
        Ok(result) => result,
        Err(_) => {
            let _ = tokio::fs::remove_file(&prepared.path).await;
            state.abort_staged_image_preparation(&args.token, snapshot.preparation_revision);
            return Err(AppError::internal("image staging task panicked"));
        }
    };
    if let Err(error) = finish_result {
        let _ = tokio::fs::remove_file(&prepared.path).await;
        state.abort_staged_image_preparation(&args.token, snapshot.preparation_revision);
        return Err(map_image_staging_error(error));
    }

    Ok(json!({
        "token": args.token,
        "file_name": prepared.file_name,
        "mime": prepared.mime,
        "size": prepared.size,
        "width": prepared.width,
        "height": prepared.height,
        "profile": prepared.profile,
        "preview_mime": prepared.preview_mime,
        "preview_base64": B64.encode(prepared.preview_bytes),
    }))
}

#[tauri::command]
pub async fn mark_image_attachment_stage_as_file(
    state: State<'_, Arc<AppState>>,
    args: ImageAttachmentStageArgs,
) -> AppResult<Value> {
    state
        .mark_staged_image_as_file(&args.token)
        .map_err(map_image_staging_error)?;
    Ok(json!({ "token": args.token, "as_file": true }))
}

#[derive(Deserialize)]
pub struct SendStagedAttachmentArgs {
    pub dest_hash: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub delivery_method: Option<String>,
    pub staging_token: String,
    #[serde(default)]
    pub client_msg_id: Option<String>,
}

#[tauri::command]
pub async fn send_lxmf_with_staged_attachment(
    state: State<'_, Arc<AppState>>,
    args: SendStagedAttachmentArgs,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&args.dest_hash, 128).to_ascii_lowercase();
    let content = sanitize_message_content(args.content.as_deref().unwrap_or(""))?;
    let delivery_pref = parse_delivery_preference(args.delivery_method.as_deref());
    let client_msg_id = normalize_lxmf_client_msg_id(args.client_msg_id.as_deref())?;
    if !validate_hex(&dest_hash, 16, 64) {
        return Err(AppError::new(
            "invalid_destination",
            "Invalid identity hash",
        ));
    }
    validate_delivery_preference(&state, delivery_pref)?;

    let staged = state
        .take_completed_attachment_staging(&args.staging_token)
        .ok_or_else(|| AppError::bad_request("Attachment staging is incomplete or expired"))?;
    if staged.is_image && inspect_inline_image(&staged.path).await.is_none() {
        let _ = tokio::fs::remove_file(&staged.path).await;
        return Err(AppError::new(
            "attachment_image_unsafe",
            "Image dimensions or format are not safe for inline display",
        ));
    }
    let file_bytes = match tokio::fs::read(&staged.path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = tokio::fs::remove_file(&staged.path).await;
            tracing::warn!(
                error_kind = ?error.kind(),
                "failed to read private attachment staging file"
            );
            return Err(AppError::new(
                "attachment_storage_failed",
                "Could not read staged attachment",
            ));
        }
    };
    if file_bytes.len() != staged.declared_size {
        return Err(AppError::new(
            "attachment_storage_failed",
            "Staged attachment length changed",
        ));
    }
    let file_name = staged.file_name.clone();
    let image_mime = if staged.is_image {
        staged.mime.clone()
    } else {
        String::new()
    };
    let is_image = staged.is_image;
    queue_prepared_attachment(
        Arc::clone(&state),
        dest_hash,
        content,
        delivery_pref,
        client_msg_id,
        file_name,
        file_bytes,
        is_image,
        image_mime,
        AttachmentLeaseOwner::Staged { staged },
    )
    .await
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
        state.release_attachment_delivery_lease(msg_id);
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
                "may_have_left_device": true,
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
                    "may_have_left_device": false,
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
        "may_have_left_device": cancelled,
    }))
}

/// Marks inbound read; returns latest 100 + aggregate unread count.
#[tauri::command]
pub async fn get_conversation(state: State<'_, Arc<AppState>>, hash: String) -> AppResult<Value> {
    let dest_hash = sanitize_text(&hash, 128).to_ascii_lowercase();
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
    let dest_hash = sanitize_text(&hash, 128).to_ascii_lowercase();
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
    let dest_hash = sanitize_text(&hash, 128).to_ascii_lowercase();
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
    let dest_hash = sanitize_text(&hash, 128).to_ascii_lowercase();
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
        "max_attachment_bytes": ratspeak_runtime::state::LXMF_DELIVERY_LIMIT_MAX_BYTES,
        "image_size_prompt_bytes": ratspeak_runtime::image_attachment::IMAGE_SIZE_PROMPT_BYTES,
        "max_message_bytes": MAX_LXMF_MESSAGE_BYTES,
        "efficient_resource_bytes": rns_protocol::resource::MAX_EFFICIENT_SIZE,
        "default_propagation_limit_kb": lxmf_core::constants::PROPAGATION_LIMIT,
        "propagation_transfer_limit_kb": propagation_transfer_limit_kb,
    }))
}

#[derive(Serialize)]
pub struct FileMetadata {
    pub mime: String,
    pub filename: String,
    pub size: u64,
    pub chunk_bytes: usize,
    pub inline_image: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_height: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InlineImageInfo {
    width: u32,
    height: u32,
    mime: &'static str,
}

fn bounded_inline_image_info(data: &[u8]) -> Option<InlineImageInfo> {
    let dimensions = if data.len() >= 24 && &data[..8] == b"\x89PNG\r\n\x1a\n" {
        Some((
            u32::from_be_bytes(data[16..20].try_into().ok()?),
            u32::from_be_bytes(data[20..24].try_into().ok()?),
            "image/png",
        ))
    } else if data.len() >= 10 && matches!(&data[..6], b"GIF87a" | b"GIF89a") {
        Some((
            u16::from_le_bytes(data[6..8].try_into().ok()?) as u32,
            u16::from_le_bytes(data[8..10].try_into().ok()?) as u32,
            "image/gif",
        ))
    } else if data.len() >= 26 && &data[..2] == b"BM" {
        let width = i32::from_le_bytes(data[18..22].try_into().ok()?).unsigned_abs();
        let height = i32::from_le_bytes(data[22..26].try_into().ok()?).unsigned_abs();
        Some((width, height, "image/bmp"))
    } else if data.len() >= 30 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        match &data[12..16] {
            b"VP8X" => Some((
                1 + u32::from_le_bytes([data[24], data[25], data[26], 0]),
                1 + u32::from_le_bytes([data[27], data[28], data[29], 0]),
                "image/webp",
            )),
            b"VP8L" if data.len() >= 25 && data[20] == 0x2f => Some((
                1 + u32::from(data[21]) + ((u32::from(data[22]) & 0x3f) << 8),
                1 + (u32::from(data[22]) >> 6)
                    + (u32::from(data[23]) << 2)
                    + ((u32::from(data[24]) & 0x0f) << 10),
                "image/webp",
            )),
            b"VP8 " if data.len() >= 30 && data[23..26] == [0x9d, 0x01, 0x2a] => Some((
                u32::from(u16::from_le_bytes([data[26], data[27]]) & 0x3fff),
                u32::from(u16::from_le_bytes([data[28], data[29]]) & 0x3fff),
                "image/webp",
            )),
            _ => None,
        }
    } else if data.len() >= 4 && data[..2] == [0xff, 0xd8] {
        jpeg_dimensions(data).map(|(width, height)| (width, height, "image/jpeg"))
    } else {
        None
    }?;

    let (width, height, mime) = dimensions;
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    (width > 0
        && height > 0
        && width <= INLINE_IMAGE_MAX_DIMENSION
        && height <= INLINE_IMAGE_MAX_DIMENSION
        && pixels <= INLINE_IMAGE_MAX_PIXELS)
        .then_some(InlineImageInfo {
            width,
            height,
            mime,
        })
}

fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let mut offset = 2usize;
    while offset.checked_add(4)? <= data.len() {
        if data[offset] != 0xff {
            offset += 1;
            continue;
        }
        while offset < data.len() && data[offset] == 0xff {
            offset += 1;
        }
        let marker = *data.get(offset)?;
        offset += 1;
        if matches!(marker, 0xd8 | 0xd9) || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = u16::from_be_bytes(data.get(offset..offset + 2)?.try_into().ok()?) as usize;
        if length < 2 || offset.checked_add(length)? > data.len() {
            return None;
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            let height = u16::from_be_bytes(data.get(offset + 3..offset + 5)?.try_into().ok()?);
            let width = u16::from_be_bytes(data.get(offset + 5..offset + 7)?.try_into().ok()?);
            return Some((u32::from(width), u32::from(height)));
        }
        offset += length;
    }
    None
}

async fn inspect_inline_image(path: &std::path::Path) -> Option<InlineImageInfo> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let size = file
        .metadata()
        .await
        .ok()?
        .len()
        .min(ATTACHMENT_IPC_CHUNK_BYTES as u64) as usize;
    let mut header = vec![0u8; size];
    file.read_exact(&mut header).await.ok()?;
    bounded_inline_image_info(&header)
}

fn clean_download_filename(path: &std::path::Path) -> String {
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());
    filename
        .find('_')
        .map(|prefix| filename[prefix + 1..].to_string())
        .unwrap_or(filename)
}

fn download_mime(path: &std::path::Path) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

fn received_file_path(state: &AppState, stored_name: &str) -> AppResult<std::path::PathBuf> {
    let file_path = if let Ok(lxmf) = state.lxmf.lock() {
        lxmf.as_ref()
            .and_then(|manager| manager.get_received_file(stored_name))
    } else {
        None
    };
    file_path.ok_or_else(|| AppError::not_found("File not found"))
}

pub fn received_file_export(
    state: &AppState,
    stored_name: &str,
) -> AppResult<(std::path::PathBuf, String, String)> {
    let path = received_file_path(state, stored_name)?;
    let filename = clean_download_filename(&path);
    let mime = download_mime(&path);
    Ok((path, filename, mime))
}

#[tauri::command]
pub async fn api_file_metadata(
    state: State<'_, Arc<AppState>>,
    stored_name: String,
) -> AppResult<FileMetadata> {
    let path = received_file_path(&state, &stored_name)?;
    let size = tokio::fs::metadata(&path)
        .await
        .map_err(|_| AppError::not_found("File not found"))?
        .len();
    let image = inspect_inline_image(&path).await;
    Ok(FileMetadata {
        mime: image.map_or_else(|| download_mime(&path), |info| info.mime.to_string()),
        filename: clean_download_filename(&path),
        size,
        chunk_bytes: ATTACHMENT_IPC_CHUNK_BYTES,
        inline_image: image.is_some(),
        image_width: image.map(|info| info.width),
        image_height: image.map(|info| info.height),
    })
}

/// Read one bounded raw attachment chunk. The stored-name sanitizer and exact
/// metadata length keep the WebView from turning this into a generic file API.
#[tauri::command]
pub async fn api_file_read_chunk(
    state: State<'_, Arc<AppState>>,
    stored_name: String,
    offset: u64,
    length: usize,
) -> AppResult<tauri::ipc::Response> {
    if length == 0 || length > ATTACHMENT_IPC_CHUNK_BYTES {
        return Err(AppError::bad_request("Invalid attachment chunk length"));
    }
    let path = received_file_path(&state, &stored_name)?;
    let file_size = tokio::fs::metadata(&path)
        .await
        .map_err(|_| AppError::not_found("File not found"))?
        .len();
    let end = offset
        .checked_add(length as u64)
        .ok_or_else(|| AppError::bad_request("Invalid attachment chunk range"))?;
    if offset >= file_size || end > file_size {
        return Err(AppError::bad_request("Invalid attachment chunk range"));
    }

    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| AppError::not_found("File not found"))?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|_| AppError::not_found("File not found"))?;
    let mut bytes = vec![0u8; length];
    file.read_exact(&mut bytes)
        .await
        .map_err(|_| AppError::not_found("File not found"))?;
    Ok(tauri::ipc::Response::new(bytes))
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
    fn staged_outbound_attachments_enforce_the_product_limit_at_command_boundary() {
        assert!(validate_outbound_attachment_size(1).is_ok());
        assert!(
            validate_outbound_attachment_size(
                ratspeak_runtime::state::LXMF_DELIVERY_LIMIT_MAX_BYTES
            )
            .is_ok()
        );
        for invalid in [
            0,
            ratspeak_runtime::state::LXMF_DELIVERY_LIMIT_MAX_BYTES + 1,
        ] {
            assert_eq!(
                validate_outbound_attachment_size(invalid)
                    .expect_err("outbound attachment size must be rejected")
                    .code,
                "attachment_too_large"
            );
        }
    }

    #[test]
    fn inline_image_header_policy_accepts_bounded_png_and_rejects_pixel_bomb() {
        fn png_header(width: u32, height: u32) -> Vec<u8> {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            bytes.extend_from_slice(&[0, 0, 0, 13]);
            bytes.extend_from_slice(b"IHDR");
            bytes.extend_from_slice(&width.to_be_bytes());
            bytes.extend_from_slice(&height.to_be_bytes());
            bytes
        }

        assert_eq!(
            bounded_inline_image_info(&png_header(4000, 3000)),
            Some(InlineImageInfo {
                width: 4000,
                height: 3000,
                mime: "image/png",
            })
        );
        assert_eq!(bounded_inline_image_info(&png_header(8192, 8192)), None);
        assert_eq!(bounded_inline_image_info(&png_header(9000, 1)), None);
    }

    #[test]
    fn inline_image_header_policy_rejects_svg_and_false_mime_content() {
        assert_eq!(
            bounded_inline_image_info(b"<svg width='10' height='10'></svg>"),
            None
        );
        assert_eq!(bounded_inline_image_info(b"not really a png"), None);
        assert_eq!(bounded_inline_image_info(b"\x89PNG\r\n\x1a\n"), None);
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
