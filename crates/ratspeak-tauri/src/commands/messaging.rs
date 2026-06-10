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
    AttachmentMessageRequest, DeliveryPreference, DeliveryProfile, MessageSendRequest,
    ReactionSendRequest, ReplyMessageSendRequest,
};
use crate::state::AppState;

const MAX_LXMF_MESSAGE_BYTES: usize = rns_protocol::resource::MAX_RESOURCE_SIZE;

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
        Ok(Err(e)) => {
            tracing::warn!(%dest_hash, ?e, "api_conversation db task failed");
            Err(AppError::internal("Database task failed"))
        }
        Err(_) => {
            tracing::warn!(%dest_hash, "api_conversation timed out after 5s");
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
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "search_messages db task panicked — returning empty");
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

async fn maybe_announce_before_user_send(state: &Arc<AppState>, dest_hash: &str) {
    let _ = crate::maybe_opportunistic_announce_before_user_send(state, dest_hash).await;
}

pub(crate) async fn ensure_propagation_ready_for_send(
    state: &Arc<AppState>,
    dest_hash: &str,
    pref: DeliveryPreference,
    profile: DeliveryProfile,
    client_msg_id: Option<&str>,
) -> AppResult<()> {
    let identity_id = active_identity_id(state);
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
        identity = %identity_id,
        dest = %dest_hash,
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
    let client_msg_id = args.client_msg_id.clone();

    if !validate_hex(&dest_hash, 16, 64) {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Invalid identity hash" }),
        );
        return Err(AppError::bad_request("Invalid identity hash"));
    }
    if content.is_empty() {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Empty message" }),
        );
        return Err(AppError::bad_request("Empty message"));
    }
    validate_delivery_preference(&state, delivery_pref)?;

    resolve_before_send(&state, &dest_hash).await;
    ensure_propagation_ready_for_send(
        &state,
        &dest_hash,
        delivery_pref,
        DeliveryProfile::Message,
        client_msg_id.as_deref(),
    )
    .await?;
    maybe_announce_before_user_send(&state, &dest_hash).await;

    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let ct = content.clone();
    let tt = title.clone();
    let id_c = identity_id.clone();
    let msg_id = tokio::task::spawn_blocking(move || {
        if let Ok(mut lxmf) = st.lxmf.lock() {
            lxmf.as_mut().and_then(|mgr| {
                mgr.send_message_with_preference(MessageSendRequest {
                    dest_hash_hex: &dh,
                    content: &ct,
                    title: &tt,
                    db_pool: &st.db,
                    identity_id: &id_c,
                    preference: delivery_pref,
                    profile: DeliveryProfile::Message,
                })
            })
        } else {
            None
        }
    })
    .await
    .map_err(|_| AppError::internal("send_message task panicked"))?;

    match msg_id {
        Some(id) => {
            state.lxmf_notify.notify_one();
            if let Some(ref cid) = client_msg_id
                && let Ok(mut map) = state.msg_id_map.lock()
            {
                map.insert(id.clone(), cid.clone());
            }
            state.emit_to_all(
                "lxmf_step",
                json!({
                    "step": "sending",
                    "message": "Message queued for delivery",
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                }),
            );
            if state
                .network_log_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                state.emit_network_event(
                    "message",
                    &format!(
                        "Message queued for {}...",
                        &dest_hash[..8.min(dest_hash.len())]
                    ),
                    &dest_hash,
                    "standard",
                );
            }
            broadcast_conversations(Arc::clone(&state));
            Ok(json!({ "msg_id": id, "client_msg_id": client_msg_id }))
        }
        None => {
            state.emit_to_all(
                "lxmf_step",
                json!({ "step": "error", "message": "LXMF not initialized" }),
            );
            if state
                .network_log_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                state.emit_network_event(
                    "error",
                    "Message send failed: LXMF not initialized",
                    "",
                    "essential",
                );
            }
            Err(AppError::lxmf_not_initialized("LXMF not initialized"))
        }
    }
}

/// Resolve identity+path before send; no-ops if transport not ready.
async fn resolve_before_send(state: &AppState, dest_hash: &str) {
    let _ = crate::commands::shared::hydrate_contact_identity_for_send(state, dest_hash).await;

    let tx = state
        .lxmf
        .lock()
        .ok()
        .and_then(|l| l.as_ref().and_then(|mgr| mgr.router.transport_tx.clone()));
    if let Some(ref tx) = tx {
        crate::lxmf::resolve_destination(state, dest_hash, tx).await;
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

    if validate_hex(&dest_hash, 16, 64) {
        resolve_before_send(&state, &dest_hash).await;
        ensure_propagation_ready_for_send(
            &state,
            &dest_hash,
            delivery_pref,
            DeliveryProfile::Message,
            None,
        )
        .await?;
        maybe_announce_before_user_send(&state, &dest_hash).await;
    }

    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let mid = message_id.clone();
    let em = emoji.clone();
    let ac = action.clone();
    let id_c = identity_id.clone();
    let sent = tokio::task::spawn_blocking(move || {
        if let Ok(mut lxmf) = st.lxmf.lock() {
            if let Some(mgr) = lxmf.as_mut() {
                mgr.send_reaction_with_preference(ReactionSendRequest {
                    dest_hash_hex: &dh,
                    message_id: &mid,
                    emoji: &em,
                    action: &ac,
                    db_pool: &st.db,
                    identity_id: &id_c,
                    preference: delivery_pref,
                });
                true
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
    let client_msg_id = args.client_msg_id.clone();

    if !validate_hex(&dest_hash, 16, 64) || content.is_empty() {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Invalid reply" }),
        );
        return Err(AppError::bad_request("Invalid reply"));
    }
    validate_delivery_preference(&state, delivery_pref)?;

    resolve_before_send(&state, &dest_hash).await;
    ensure_propagation_ready_for_send(
        &state,
        &dest_hash,
        delivery_pref,
        DeliveryProfile::Message,
        client_msg_id.as_deref(),
    )
    .await?;
    maybe_announce_before_user_send(&state, &dest_hash).await;

    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let ct = content.clone();
    let id_c = identity_id.clone();
    let reply_id_for_send = wire_reply_to_id.clone();
    let msg_id = tokio::task::spawn_blocking(move || {
        if let Ok(mut lxmf) = st.lxmf.lock() {
            lxmf.as_mut().and_then(|mgr| {
                mgr.send_reply_with_preference(ReplyMessageSendRequest {
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
        } else {
            None
        }
    })
    .await
    .map_err(|_| AppError::internal("send_reply task panicked"))?;

    match msg_id {
        Some(id) => {
            state.lxmf_notify.notify_one();
            if let Some(ref cid) = client_msg_id
                && let Ok(mut map) = state.msg_id_map.lock()
            {
                map.insert(id.clone(), cid.clone());
            }
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
        None => {
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
    let client_msg_id = args.client_msg_id.clone();

    if !validate_hex(&dest_hash, 16, 64) {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Invalid identity hash" }),
        );
        return Err(AppError::bad_request("Invalid identity hash"));
    }
    if content.is_empty() {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Empty message" }),
        );
        return Err(AppError::bad_request("Empty message"));
    }

    validate_delivery_preference(&state, DeliveryPreference::Propagated)?;

    // Propagation still needs the recipient identity for encryption.
    resolve_before_send(&state, &dest_hash).await;
    ensure_propagation_ready_for_send(
        &state,
        &dest_hash,
        DeliveryPreference::Propagated,
        DeliveryProfile::Message,
        client_msg_id.as_deref(),
    )
    .await?;
    maybe_announce_before_user_send(&state, &dest_hash).await;

    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let ct = content.clone();
    let tt = title.clone();
    let id_c = identity_id.clone();
    let msg_id = tokio::task::spawn_blocking(move || {
        if let Ok(mut lxmf) = st.lxmf.lock() {
            lxmf.as_mut().and_then(|mgr| {
                mgr.send_message_with_method(
                    &dh,
                    &ct,
                    &tt,
                    &st.db,
                    &id_c,
                    DeliveryMethod::Propagated,
                )
            })
        } else {
            None
        }
    })
    .await
    .map_err(|_| AppError::internal("send_propagated task panicked"))?;

    match msg_id {
        Some(id) => {
            state.lxmf_notify.notify_one();
            if let Some(ref cid) = client_msg_id
                && let Ok(mut map) = state.msg_id_map.lock()
            {
                map.insert(id.clone(), cid.clone());
            }
            state.emit_to_all(
                "lxmf_step",
                json!({
                    "step": "sending",
                    "message": "Message queued for propagation",
                    "msg_id": id,
                    "client_msg_id": client_msg_id,
                }),
            );
            if state
                .network_log_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                state.emit_network_event(
                    "message",
                    &format!(
                        "Message queued via propagation for {}...",
                        &dest_hash[..8.min(dest_hash.len())]
                    ),
                    &dest_hash,
                    "standard",
                );
            }
            broadcast_conversations(Arc::clone(&state));
            Ok(json!({ "msg_id": id, "client_msg_id": client_msg_id }))
        }
        None => {
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
    let client_msg_id = args.client_msg_id.clone();

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
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Invalid identity hash" }),
        );
        return Err(AppError::bad_request("Invalid identity hash"));
    }
    validate_delivery_preference(&state, delivery_pref)?;
    if file_data_b64.is_empty() {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "No file data provided" }),
        );
        return Err(AppError::bad_request("No file data provided"));
    }
    if base64_decoded_len_upper_bound(file_data_b64.len()).unwrap_or(usize::MAX)
        > rns_protocol::resource::MAX_RESOURCE_SIZE
    {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Attachment exceeds protocol resource limit" }),
        );
        return Err(AppError::bad_request(
            "Attachment exceeds protocol resource limit",
        ));
    }

    let file_bytes = B64.decode(file_data_b64).map_err(|_| {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Invalid base64 file data" }),
        );
        AppError::bad_request("Invalid base64 file data")
    })?;
    if file_bytes.len() > rns_protocol::resource::MAX_RESOURCE_SIZE {
        state.emit_to_all(
            "lxmf_step",
            json!({ "step": "error", "message": "Attachment exceeds protocol resource limit" }),
        );
        return Err(AppError::bad_request(
            "Attachment exceeds protocol resource limit",
        ));
    }

    resolve_before_send(&state, &dest_hash).await;
    ensure_propagation_ready_for_send(
        &state,
        &dest_hash,
        delivery_pref,
        DeliveryProfile::Attachment,
        client_msg_id.as_deref(),
    )
    .await?;
    maybe_announce_before_user_send(&state, &dest_hash).await;

    let identity_id = active_identity_id(&state);
    let st: Arc<AppState> = Arc::clone(&state);
    let dh = dest_hash.clone();
    let ct = content.clone();
    let fn_c = file_name.clone();
    let im = image_mime.clone();
    let id_c = identity_id.clone();
    let msg_id = tokio::task::spawn_blocking(move || {
        if let Ok(mut lxmf) = st.lxmf.lock() {
            if let Some(mgr) = lxmf.as_mut() {
                // Append "[File: …]" so non-attachment clients see the name.
                let msg_content = if ct.is_empty() {
                    format!("[File: {}]", fn_c)
                } else {
                    format!("{}\n[File: {}]", ct, fn_c)
                };
                mgr.send_message_with_attachment_fields_preference(AttachmentMessageRequest {
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
            } else {
                None
            }
        } else {
            None
        }
    })
    .await
    .map_err(|_| AppError::internal("send_attachment task panicked"))?;

    match msg_id {
        Some(id) => {
            if let Some(ref cid) = client_msg_id
                && let Ok(mut map) = state.msg_id_map.lock()
            {
                map.insert(id.clone(), cid.clone());
            }
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
        None => {
            state.emit_to_all(
                "lxmf_step",
                json!({ "step": "error", "message": "LXMF not initialized" }),
            );
            Err(AppError::lxmf_not_initialized("LXMF not initialized"))
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

#[tauri::command]
pub async fn cancel_lxmf_message(
    state: State<'_, Arc<AppState>>,
    args: CancelLxmfMessageArgs,
) -> AppResult<Value> {
    let requested_msg_id = sanitize_text(&args.msg_id, 128);
    let Some((msg_id, client_msg_id)) =
        resolve_lxmf_message_id_for_cancel(&state, &requested_msg_id)
    else {
        return Ok(json!({
            "ok": true,
            "cancelled": false,
            "msg_id": requested_msg_id,
        }));
    };

    let st: Arc<AppState> = Arc::clone(&state);
    let msg_id_for_cancel = msg_id.clone();
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

    let msg_id_for_db = msg_id.clone();
    let identity_for_db = active_identity_id(&state);
    let db_cancelled = db::spawn_db(state.db.clone(), move |p| {
        db::cancel_outbound_message_state(&p, &msg_id_for_db, &identity_for_db)
    })
    .await
    .map_err(|_| AppError::internal("cancel_lxmf_message db task panicked"))?;

    let cancelled = transport_cancelled || db_cancelled;
    if cancelled {
        if let Ok(mut times) = state.message_send_times.lock() {
            times.remove(&msg_id);
        }
        if let Ok(mut map) = state.msg_id_map.lock() {
            map.remove(&msg_id);
        }
        let method = db::get_message_delivery_method(&state.db, &msg_id);
        state.emit_to_all(
            "lxmf_step",
            json!({
                "step": "cancelled",
                "msg_id": msg_id.clone(),
                "client_msg_id": client_msg_id.clone(),
                "method": method,
            }),
        );
        broadcast_conversations(Arc::clone(&state));
        state.lxmf_notify.notify_one();
    }

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
        if let Err(e) = conn.execute(
            "INSERT OR REPLACE INTO hidden_conversations (dest_hash, identity_id) VALUES (?1, ?2)",
            rusqlite::params![dh, id_c],
        ) {
            tracing::warn!(error = %e, "hide_conversation insert failed");
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
    let sanitized: String = stored_name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
        .take(200)
        .collect();
    let file_path = if let Ok(lxmf) = state.lxmf.lock() {
        lxmf.as_ref()
            .and_then(|mgr| mgr.get_received_file(&sanitized))
    } else {
        None
    };
    let path = file_path.ok_or_else(|| AppError::not_found("File not found"))?;
    let data = tokio::fs::read(&path).await.map_err(|e| {
        tracing::warn!(error = %e, "file-download read failed");
        AppError::not_found("File not found")
    })?;
    let mime = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();
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
