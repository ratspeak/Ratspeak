//! Channels IPC commands. Accepted room transcript items use the bounded local
//! Channels append log and are never routed through the LXMF conversation
//! database or treated as hub-hosted backlog.

use std::sync::Arc;

use ratspeak_runtime::channels::{
    ChannelRoomPhase, ChannelsError, ChannelsSnapshot, DiscoveredChannelHub,
};
use rns_identity::identity::Identity;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::helpers::{active_identity_id, sanitize_text, validate_hex};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ConnectChannelHubArgs {
    pub destination_hash: String,
    pub nickname: String,
}

#[derive(Debug, Deserialize)]
pub struct JoinChannelArgs {
    pub room: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default = "default_true")]
    pub remember_key: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct ChannelRoomArgs {
    pub room: String,
}

#[derive(Debug, Deserialize)]
pub struct SendChannelMessageArgs {
    pub room: String,
    pub text: String,
}

#[derive(Debug, PartialEq, Eq)]
enum LocalComposerCommand {
    Join(Option<String>),
    Part(Option<String>),
}

/// `/join` and `/part` are client navigation conveniences, not RRC hub
/// commands. Keep every other slash command untouched so rrcd-specific
/// commands such as `/list`, `/who`, and `/topic` reach the hub verbatim.
fn parse_local_composer_command(text: &str) -> Option<LocalComposerCommand> {
    let command_line = text.trim().strip_prefix('/')?;
    let verb_end = command_line
        .find(char::is_whitespace)
        .unwrap_or(command_line.len());
    let verb = &command_line[..verb_end];
    let argument = command_line[verb_end..].trim().to_ascii_lowercase();
    let argument = (!argument.is_empty()).then_some(argument);
    if verb.eq_ignore_ascii_case("join") {
        Some(LocalComposerCommand::Join(argument))
    } else if verb.eq_ignore_ascii_case("part") {
        Some(LocalComposerCommand::Part(argument))
    } else {
        None
    }
}

#[derive(Debug, Deserialize)]
pub struct SaveChannelHubArgs {
    pub destination_hash: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub connected: bool,
}

#[derive(Debug, Deserialize)]
pub struct ChannelHubArgs {
    pub destination_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct SavedChannelRoomsArgs {
    pub hub_destination_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct SaveChannelRoomArgs {
    pub hub_destination_hash: String,
    pub room: String,
    #[serde(default)]
    pub joined: bool,
}

#[derive(Debug, Deserialize)]
pub struct ChannelHistoryArgs {
    pub hub_destination_hash: String,
    pub room: String,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct MarkChannelRoomReadArgs {
    pub hub_destination_hash: String,
    pub room: String,
    pub through: String,
}

#[derive(Debug, Deserialize)]
pub struct SetChannelRoomNotificationLevelArgs {
    pub hub_destination_hash: String,
    pub room: String,
    pub notification_level: crate::db::ChannelRoomNotificationLevel,
}

#[derive(Debug, Serialize)]
pub struct ChannelRoomStateUpdate {
    pub room: crate::db::ChannelRoomReadState,
    pub unread: crate::db::ChannelUnreadSummary,
}

#[tauri::command]
pub async fn api_channels(state: State<'_, Arc<AppState>>) -> AppResult<ChannelsSnapshot> {
    Ok(state
        .channels_handle()
        .map(|channels| channels.snapshot())
        .unwrap_or_else(ChannelsSnapshot::unavailable))
}

#[tauri::command]
pub async fn api_channel_history(
    state: State<'_, Arc<AppState>>,
    args: ChannelHistoryArgs,
) -> AppResult<crate::db::ChannelHistoryPage> {
    let identity_id = require_identity(&state)?;
    let destination_hash = clean_destination_hash(&args.hub_destination_hash)?;
    let room = ratspeak_runtime::rrc::normalize_room(
        &args.room,
        crate::db::CHANNEL_HISTORY_MAX_ROOM_BYTES,
    )
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    if args.before.is_some() && args.after.is_some() {
        return Err(AppError::bad_request(
            "Channel history accepts either before or after, not both",
        ));
    }
    crate::db::validate_channel_history_cursor(args.before.as_deref())
        .map_err(AppError::bad_request)?;
    if let Some(after) = args.after.as_deref() {
        crate::db::validate_channel_history_after_cursor(after).map_err(AppError::bad_request)?;
    }
    let limit = args
        .limit
        .unwrap_or(crate::db::CHANNEL_HISTORY_DEFAULT_PAGE_SIZE);
    if limit == 0 || limit > crate::db::CHANNEL_HISTORY_MAX_PAGE_SIZE {
        return Err(AppError::bad_request(format!(
            "Channel history page size must be between 1 and {}",
            crate::db::CHANNEL_HISTORY_MAX_PAGE_SIZE
        )));
    }
    let before = args.before;
    let after = args.after;
    let prune_expired = before.is_none() && after.is_none();
    let pool = state.db.clone();
    crate::db::spawn_db(pool, move |pool| {
        if prune_expired {
            crate::db::prune_expired_channel_history(&pool)?;
        }
        match after.as_deref() {
            Some(after) => crate::db::list_channel_history_after(
                &pool,
                &identity_id,
                &destination_hash,
                &room,
                after,
                limit,
            ),
            None => crate::db::list_channel_history(
                &pool,
                &identity_id,
                &destination_hash,
                &room,
                before.as_deref(),
                limit,
            ),
        }
    })
    .await
    .map_err(|_| AppError::internal("channel history database task panicked"))?
    .map_err(AppError::database_unavailable)
}

#[tauri::command]
pub async fn api_channel_unread(
    state: State<'_, Arc<AppState>>,
) -> AppResult<crate::db::ChannelUnreadSummary> {
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let identity_id = require_identity(&state)?;
    let pool = state.db.clone();
    crate::db::spawn_db(pool, move |pool| {
        crate::db::get_channel_unread_summary(&pool, &identity_id)
    })
    .await
    .map_err(|_| AppError::internal("channel unread database task panicked"))?
    .map_err(AppError::database_unavailable)
}

#[tauri::command]
pub async fn mark_channel_room_read(
    state: State<'_, Arc<AppState>>,
    args: MarkChannelRoomReadArgs,
) -> AppResult<ChannelRoomStateUpdate> {
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let identity_id = require_identity(&state)?;
    let destination_hash = clean_destination_hash(&args.hub_destination_hash)?;
    let room = ratspeak_runtime::rrc::normalize_room(
        &args.room,
        crate::db::CHANNEL_HISTORY_MAX_ROOM_BYTES,
    )
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    crate::db::validate_channel_history_after_cursor(&args.through)
        .map_err(AppError::bad_request)?;

    // The manager command and history-writer barrier are FIFO. Every event
    // accepted before this read request is either committed first or this
    // command fails; events accepted later receive a greater sequence.
    if let Some(channels) = state.channels_handle() {
        channels.flush_history().await.map_err(map_error)?;
    }

    let through = args.through;
    let pool = state.db.clone();
    let update = crate::db::spawn_db(pool, move |pool| {
        let room_state = crate::db::mark_channel_room_read(
            &pool,
            &identity_id,
            &destination_hash,
            &room,
            &through,
        )?;
        let unread = crate::db::get_channel_unread_summary(&pool, &identity_id)?;
        Ok::<_, String>(ChannelRoomStateUpdate {
            room: room_state,
            unread,
        })
    })
    .await
    .map_err(|_| AppError::internal("channel read-state database task panicked"))?
    .map_err(AppError::database_unavailable)?;
    if let Ok(payload) = serde_json::to_value(&update.unread) {
        state.emit_to_all("channels_unread", payload);
    }
    Ok(update)
}

#[tauri::command]
pub async fn set_channel_room_notification_level(
    state: State<'_, Arc<AppState>>,
    args: SetChannelRoomNotificationLevelArgs,
) -> AppResult<ChannelRoomStateUpdate> {
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let identity_id = require_identity(&state)?;
    let destination_hash = clean_destination_hash(&args.hub_destination_hash)?;
    let room = ratspeak_runtime::rrc::normalize_room(
        &args.room,
        crate::db::CHANNEL_HISTORY_MAX_ROOM_BYTES,
    )
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    let notification_level = args.notification_level;
    let pool = state.db.clone();
    let update = crate::db::spawn_db(pool, move |pool| {
        let room_state = crate::db::set_channel_room_notification_level(
            &pool,
            &identity_id,
            &destination_hash,
            &room,
            notification_level,
        )?;
        let unread = crate::db::get_channel_unread_summary(&pool, &identity_id)?;
        Ok::<_, String>(ChannelRoomStateUpdate {
            room: room_state,
            unread,
        })
    })
    .await
    .map_err(|_| AppError::internal("channel notification-state database task panicked"))?
    .map_err(AppError::database_unavailable)?;
    if let Ok(payload) = serde_json::to_value(&update.unread) {
        state.emit_to_all("channels_unread", payload);
    }
    Ok(update)
}

#[tauri::command]
pub async fn discover_channel_hubs(
    state: State<'_, Arc<AppState>>,
) -> AppResult<Vec<DiscoveredChannelHub>> {
    channels_handle(&state)?
        .discover_hubs()
        .await
        .map_err(map_error)
}

#[tauri::command]
pub async fn refresh_channel_directory(
    state: State<'_, Arc<AppState>>,
) -> AppResult<ChannelsSnapshot> {
    let channels = channels_handle(&state)?;
    channels.refresh_directory().await.map_err(map_error)?;
    Ok(channels.snapshot())
}

#[tauri::command]
pub async fn connect_channel_hub(
    state: State<'_, Arc<AppState>>,
    args: ConnectChannelHubArgs,
) -> AppResult<ChannelsSnapshot> {
    let channels = channels_handle(&state)?;
    let destination_hash = clean_destination_hash(&args.destination_hash)?;
    let owned_hub = state.channel_hub_handle().and_then(|hub| {
        let status = hub.snapshot();
        (status.running && status.destination_hash.as_deref() == Some(destination_hash.as_str()))
            .then_some(status)
    });
    if let Some(status) = owned_hub {
        let identity_id = require_identity(&state)?;
        let identity_path =
            ratspeak_runtime::channel_hub::hub_identity_path(&state.config.data_dir, &identity_id);
        let identity = Identity::from_file(&identity_path).map_err(|_| {
            AppError::service_unavailable("Your channel hub identity is unavailable")
        })?;
        channels
            .connect_known(
                &destination_hash,
                &args.nickname,
                identity.get_public_key(),
                Some(status.hub_name),
            )
            .await
            .map_err(map_error)?;
    } else {
        channels
            .connect(&destination_hash, &args.nickname)
            .await
            .map_err(map_error)?;
    }
    Ok(channels.snapshot())
}

#[tauri::command]
pub async fn disconnect_channel_hub(
    state: State<'_, Arc<AppState>>,
) -> AppResult<ChannelsSnapshot> {
    let channels = channels_handle(&state)?;
    channels.disconnect().await.map_err(map_error)?;
    Ok(channels.snapshot())
}

#[tauri::command]
pub async fn join_channel(
    state: State<'_, Arc<AppState>>,
    args: JoinChannelArgs,
) -> AppResult<Value> {
    let channels = channels_handle(&state)?;
    let room = channels
        .join_with_key_policy(&args.room, args.key, args.remember_key)
        .await
        .map_err(map_error)?;
    Ok(json!({
        "room": room,
        "joining": true,
        "snapshot": channels.snapshot()
    }))
}

#[tauri::command]
pub async fn part_channel(
    state: State<'_, Arc<AppState>>,
    args: ChannelRoomArgs,
) -> AppResult<Value> {
    let channels = channels_handle(&state)?;
    channels.part(&args.room).await.map_err(map_error)?;
    Ok(json!({
        "room": args.room,
        "parting": true,
        "snapshot": channels.snapshot()
    }))
}

#[tauri::command]
pub async fn send_channel_message(
    state: State<'_, Arc<AppState>>,
    args: SendChannelMessageArgs,
) -> AppResult<Value> {
    let channels = channels_handle(&state)?;
    match parse_local_composer_command(&args.text) {
        Some(LocalComposerCommand::Join(None)) => {
            Err(AppError::bad_request("Use /join <channel>."))
        }
        Some(LocalComposerCommand::Join(Some(requested_room))) => {
            let snapshot = channels.snapshot();
            let max_room_bytes = snapshot
                .hub
                .as_ref()
                .and_then(|hub| hub.limits.max_room_name_bytes)
                .unwrap_or(64);
            let room = ratspeak_runtime::rrc::normalize_room(&requested_room, max_room_bytes)
                .map_err(|error| AppError::bad_request(error.to_string()))?;
            if snapshot.rooms.iter().any(|candidate| {
                candidate.name == room && candidate.phase == ChannelRoomPhase::Joined
            }) {
                return Ok(json!({
                    "accepted": true,
                    "local_command": "join",
                    "room": room,
                    "already_joined": true,
                    "snapshot": snapshot
                }));
            }
            let room = channels.join(&room, None).await.map_err(map_error)?;
            Ok(json!({
                "accepted": true,
                "local_command": "join",
                "room": room,
                "joining": true,
                "snapshot": channels.snapshot()
            }))
        }
        Some(LocalComposerCommand::Part(requested_room)) => {
            let snapshot = channels.snapshot();
            let max_room_bytes = snapshot
                .hub
                .as_ref()
                .and_then(|hub| hub.limits.max_room_name_bytes)
                .unwrap_or(64);
            let requested_room = requested_room.unwrap_or(args.room);
            let room = ratspeak_runtime::rrc::normalize_room(&requested_room, max_room_bytes)
                .map_err(|error| AppError::bad_request(error.to_string()))?;
            if snapshot.rooms.iter().any(|candidate| {
                candidate.name == room && candidate.phase == ChannelRoomPhase::Parting
            }) {
                return Ok(json!({
                    "accepted": true,
                    "local_command": "part",
                    "room": room,
                    "already_parting": true,
                    "snapshot": snapshot
                }));
            }
            channels.part(&room).await.map_err(map_error)?;
            Ok(json!({
                "accepted": true,
                "local_command": "part",
                "room": room,
                "parting": true,
                "snapshot": channels.snapshot()
            }))
        }
        None => {
            channels
                .send(&args.room, &args.text)
                .await
                .map_err(map_error)?;
            Ok(json!({ "accepted": true }))
        }
    }
}

#[tauri::command]
pub async fn api_saved_channel_hubs(
    state: State<'_, Arc<AppState>>,
) -> AppResult<Vec<crate::db::SavedChannelHub>> {
    let identity_id = require_identity(&state)?;
    let pool = state.db.clone();
    crate::db::spawn_db(pool, move |pool| {
        crate::db::list_saved_channel_hubs(&pool, &identity_id)
    })
    .await
    .map_err(|_| AppError::internal("saved channel hubs database task panicked"))?
    .map_err(AppError::database_unavailable)
}

#[tauri::command]
pub async fn save_channel_hub(
    state: State<'_, Arc<AppState>>,
    args: SaveChannelHubArgs,
) -> AppResult<Value> {
    let identity_id = require_identity(&state)?;
    let destination_hash = clean_destination_hash(&args.destination_hash)?;
    let label = sanitize_text(&args.label.replace(['\0', '\r', '\n'], " "), 80);
    let nickname = ratspeak_runtime::rrc::normalize_nickname(&args.nickname, 32)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let connected = args.connected;
    let pool = state.db.clone();
    let destination_for_result = destination_hash.clone();
    crate::db::spawn_db(pool, move |pool| {
        crate::db::save_channel_hub(
            &pool,
            &identity_id,
            &destination_hash,
            &label,
            &nickname,
            connected,
        )
    })
    .await
    .map_err(|_| AppError::internal("save channel hub database task panicked"))?
    .map_err(AppError::database_unavailable)?;
    refresh_channels_durable(&state).await;
    Ok(json!({ "destination_hash": destination_for_result, "saved": true }))
}

#[tauri::command]
pub async fn remove_saved_channel_hub(
    state: State<'_, Arc<AppState>>,
    args: ChannelHubArgs,
) -> AppResult<Value> {
    let identity_id = require_identity(&state)?;
    let destination_hash = clean_destination_hash(&args.destination_hash)?;
    let pool = state.db.clone();
    let destination_for_result = destination_hash.clone();
    let removed = crate::db::spawn_db(pool, move |pool| {
        crate::db::remove_channel_hub(&pool, &identity_id, &destination_hash)
    })
    .await
    .map_err(|_| AppError::internal("remove channel hub database task panicked"))?
    .map_err(AppError::database_unavailable)?;
    refresh_channels_durable(&state).await;
    Ok(json!({ "destination_hash": destination_for_result, "removed": removed }))
}

#[tauri::command]
pub async fn api_saved_channel_rooms(
    state: State<'_, Arc<AppState>>,
    args: SavedChannelRoomsArgs,
) -> AppResult<Vec<crate::db::SavedChannelRoom>> {
    let identity_id = require_identity(&state)?;
    let destination_hash = clean_destination_hash(&args.hub_destination_hash)?;
    let pool = state.db.clone();
    crate::db::spawn_db(pool, move |pool| {
        crate::db::list_saved_channel_rooms(&pool, &identity_id, &destination_hash)
    })
    .await
    .map_err(|_| AppError::internal("saved channel rooms database task panicked"))?
    .map_err(AppError::database_unavailable)
}

/// Return the union of remembered rooms and retained local history for the
/// offline Channels browser. History outlives bookmarks by design and must not
/// become unreachable when a user forgets a hub.
#[tauri::command]
pub async fn api_channel_room_index(
    state: State<'_, Arc<AppState>>,
) -> AppResult<Vec<crate::db::ChannelRoomIndexEntry>> {
    let identity_id = require_identity(&state)?;
    let pool = state.db.clone();
    crate::db::spawn_db(pool, move |pool| {
        crate::db::list_channel_room_index(&pool, &identity_id)
    })
    .await
    .map_err(|_| AppError::internal("channel room index database task panicked"))?
    .map_err(AppError::database_unavailable)
}

#[tauri::command]
pub async fn save_channel_room(
    state: State<'_, Arc<AppState>>,
    args: SaveChannelRoomArgs,
) -> AppResult<Value> {
    let identity_id = require_identity(&state)?;
    let destination_hash = clean_destination_hash(&args.hub_destination_hash)?;
    let room = ratspeak_runtime::rrc::normalize_room(&args.room, 64)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let joined = args.joined;
    let pool = state.db.clone();
    let room_for_result = room.clone();
    crate::db::spawn_db(pool, move |pool| {
        crate::db::save_channel_room(&pool, &identity_id, &destination_hash, &room, joined)
    })
    .await
    .map_err(|_| AppError::internal("save channel room database task panicked"))?
    .map_err(AppError::database_unavailable)?;
    refresh_channels_durable(&state).await;
    Ok(json!({ "room": room_for_result, "saved": true }))
}

#[tauri::command]
pub async fn remove_saved_channel_room(
    state: State<'_, Arc<AppState>>,
    args: SaveChannelRoomArgs,
) -> AppResult<Value> {
    let identity_id = require_identity(&state)?;
    let destination_hash = clean_destination_hash(&args.hub_destination_hash)?;
    let room = ratspeak_runtime::rrc::normalize_room(&args.room, 64)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let pool = state.db.clone();
    let room_for_result = room.clone();
    let removed = crate::db::spawn_db(pool, move |pool| {
        crate::db::remove_channel_room(&pool, &identity_id, &destination_hash, &room)
    })
    .await
    .map_err(|_| AppError::internal("remove channel room database task panicked"))?
    .map_err(AppError::database_unavailable)?;
    refresh_channels_durable(&state).await;
    Ok(json!({ "room": room_for_result, "removed": removed }))
}

async fn refresh_channels_durable(state: &AppState) {
    if let Some(channels) = state.channels_handle() {
        channels.refresh_durable().await;
    }
}

fn channels_handle(
    state: &AppState,
) -> AppResult<ratspeak_runtime::channels::ChannelsManagerHandle> {
    state.channels_handle().ok_or_else(|| {
        AppError::service_unavailable("Channels will be available when Reticulum is ready")
    })
}

fn require_identity(state: &AppState) -> AppResult<String> {
    let identity_id = active_identity_id(state);
    if identity_id.is_empty() {
        Err(AppError::service_unavailable(
            "An unlocked identity is required for Channels",
        ))
    } else {
        Ok(identity_id)
    }
}

fn clean_destination_hash(value: &str) -> AppResult<String> {
    let destination_hash = value.trim().to_ascii_lowercase();
    if !validate_hex(&destination_hash, 32, 32) {
        return Err(AppError::bad_request(
            "channel hub destination must be a 32-character hexadecimal hash",
        ));
    }
    Ok(destination_hash)
}

fn map_error(error: ChannelsError) -> AppError {
    match error {
        ChannelsError::InvalidDestination
        | ChannelsError::EmptyMessage
        | ChannelsError::MessageTooLong(_)
        | ChannelsError::JoinKeyTooLong(_)
        | ChannelsError::Protocol(_) => AppError::bad_request(error.to_string()),
        ChannelsError::NotConnected
        | ChannelsError::AlreadyConnecting
        | ChannelsError::NotJoined(_)
        | ChannelsError::AlreadyJoining(_)
        | ChannelsError::RoomLimitReached => AppError::conflict(error.to_string()),
        ChannelsError::SavedJoinKeyUnavailable(_) => {
            AppError::new("channel_key_required", error.to_string())
        }
        ChannelsError::HubRejected(_) => AppError::new("channel_hub_rejected", error.to_string()),
        ChannelsError::Unavailable
        | ChannelsError::Transport(_)
        | ChannelsError::LocalHistoryUnavailable
        | ChannelsError::Stopped => AppError::service_unavailable(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_input_errors_are_stable_bad_requests() {
        let error = map_error(ChannelsError::InvalidDestination);
        assert_eq!(error.code, "bad_request");
    }

    #[test]
    fn join_keys_default_to_confirmed_identity_sealed_storage() {
        let args: JoinChannelArgs =
            serde_json::from_value(json!({ "room": "general", "key": "field-pass" })).unwrap();
        assert!(args.remember_key);
        let opted_out: JoinChannelArgs = serde_json::from_value(json!({
            "room": "general",
            "key": "field-pass",
            "remember_key": false
        }))
        .unwrap();
        assert!(!opted_out.remember_key);
    }

    #[test]
    fn channel_history_cursors_remain_opaque_strings() {
        let defaults: ChannelHistoryArgs = serde_json::from_value(json!({
            "hub_destination_hash": "00112233445566778899aabbccddeeff",
            "room": "general"
        }))
        .unwrap();
        assert_eq!(defaults.before, None);
        assert_eq!(defaults.after, None);
        assert_eq!(defaults.limit, None);

        let paged: ChannelHistoryArgs = serde_json::from_value(json!({
            "hub_destination_hash": "00112233445566778899aabbccddeeff",
            "room": "general",
            "before": "9007199254740993",
            "after": null,
            "limit": 200
        }))
        .unwrap();
        assert_eq!(paged.before.as_deref(), Some("9007199254740993"));
        assert_eq!(paged.after, None);
        assert_eq!(paged.limit, Some(200));

        let forward: ChannelHistoryArgs = serde_json::from_value(json!({
            "hub_destination_hash": "00112233445566778899aabbccddeeff",
            "room": "general",
            "after": "0"
        }))
        .unwrap();
        assert_eq!(forward.before, None);
        assert_eq!(forward.after.as_deref(), Some("0"));
    }

    #[test]
    fn session_state_errors_are_conflicts() {
        let error = map_error(ChannelsError::NotConnected);
        assert_eq!(error.code, "conflict");
    }

    #[test]
    fn composer_routes_only_client_navigation_commands_locally() {
        assert_eq!(
            parse_local_composer_command(" /JOIN Field Team "),
            Some(LocalComposerCommand::Join(Some("field team".into())))
        );
        assert_eq!(
            parse_local_composer_command("/part"),
            Some(LocalComposerCommand::Part(None))
        );
        assert_eq!(
            parse_local_composer_command("/part General"),
            Some(LocalComposerCommand::Part(Some("general".into())))
        );
        for forwarded in [
            "/list",
            "/who general",
            "/topic general Field work",
            "/mode general +m",
            "/me waves",
            "ordinary message",
        ] {
            assert_eq!(parse_local_composer_command(forwarded), None);
        }
    }
}
