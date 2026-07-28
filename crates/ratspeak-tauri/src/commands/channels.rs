//! Channels IPC commands. Channel traffic is live session state and is never
//! routed through the LXMF conversation database.

use std::sync::Arc;

use ratspeak_runtime::channels::{
    ChannelRoomPhase, ChannelsError, ChannelsSnapshot, DiscoveredChannelHub,
};
use rns_identity::identity::Identity;
use serde::Deserialize;
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

#[tauri::command]
pub async fn api_channels(state: State<'_, Arc<AppState>>) -> AppResult<ChannelsSnapshot> {
    Ok(state
        .channels_handle()
        .map(|channels| channels.snapshot())
        .unwrap_or_else(ChannelsSnapshot::unavailable))
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
    let room = channels_handle(&state)?
        .join(&args.room, args.key)
        .await
        .map_err(map_error)?;
    Ok(json!({ "room": room, "joining": true }))
}

#[tauri::command]
pub async fn part_channel(
    state: State<'_, Arc<AppState>>,
    args: ChannelRoomArgs,
) -> AppResult<Value> {
    channels_handle(&state)?
        .part(&args.room)
        .await
        .map_err(map_error)?;
    Ok(json!({ "room": args.room, "parting": true }))
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
                    "already_joined": true
                }));
            }
            let room = channels.join(&room, None).await.map_err(map_error)?;
            Ok(json!({
                "accepted": true,
                "local_command": "join",
                "room": room,
                "joining": true
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
                    "already_parting": true
                }));
            }
            channels.part(&room).await.map_err(map_error)?;
            Ok(json!({
                "accepted": true,
                "local_command": "part",
                "room": room,
                "parting": true
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
    Ok(json!({ "room": room_for_result, "removed": removed }))
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
        | ChannelsError::Protocol(_) => AppError::bad_request(error.to_string()),
        ChannelsError::NotConnected
        | ChannelsError::AlreadyConnecting
        | ChannelsError::NotJoined(_)
        | ChannelsError::AlreadyJoining(_)
        | ChannelsError::RoomLimitReached => AppError::conflict(error.to_string()),
        ChannelsError::HubRejected(_) => AppError::new("channel_hub_rejected", error.to_string()),
        ChannelsError::Unavailable | ChannelsError::Transport(_) | ChannelsError::Stopped => {
            AppError::service_unavailable(error.to_string())
        }
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
