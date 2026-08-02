//! Channel hub (RRC server) IPC commands. Hub relay traffic is live session
//! state. Two things persist: operator configuration in the settings table,
//! and the `channel_hub_*` room registry owned by `ratspeak-runtime`.

use std::sync::Arc;

use ratspeak_runtime::channel_hub::{
    CHANNEL_HOSTING_ENABLED_KEY, CHANNEL_HOSTING_PREFERENCE_VERSION,
    CHANNEL_HOSTING_PREFERENCE_VERSION_KEY, ChannelHubAdminKeyChange, ChannelHubAdminMutation,
    ChannelHubAdminRoomPolicy, ChannelHubAdminRoomRole, ChannelHubAdminSecret,
    ChannelHubAdminSnapshot, ChannelHubSettings, ChannelHubSnapshot, HubStore,
    channel_hosting_enabled, channel_hub_hosting_supported,
    valid_channel_hub_announce_interval_secs, valid_evidence_retention_secs,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::helpers::sanitize_text;
use crate::state::AppState;

const MAX_HUB_NAME_CHARS: usize = 64;
const MAX_GREETING_CHARS: usize = 512;

/// Complete room policy from the Admin Center. Keeping this non-optional
/// avoids a UI checkbox omission silently preserving stale authority.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelHubAdminRoomPolicyArgs {
    pub invite_only: bool,
    pub moderated: bool,
    pub no_outside_messages: bool,
    pub private: bool,
    pub topic_operators_only: bool,
}

impl From<ChannelHubAdminRoomPolicyArgs> for ChannelHubAdminRoomPolicy {
    fn from(value: ChannelHubAdminRoomPolicyArgs) -> Self {
        Self {
            invite_only: value.invite_only,
            moderated: value.moderated,
            no_outside_messages: value.no_outside_messages,
            private: value.private,
            topic_operators_only: value.topic_operators_only,
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelHubAdminRoomRoleArgs {
    Operator,
    Voice,
}

impl From<ChannelHubAdminRoomRoleArgs> for ChannelHubAdminRoomRole {
    fn from(value: ChannelHubAdminRoomRoleArgs) -> Self {
        match value {
            ChannelHubAdminRoomRoleArgs::Operator => Self::Operator,
            ChannelHubAdminRoomRoleArgs::Voice => Self::Voice,
        }
    }
}

/// Tagged owner intents. Deliberately no `Debug`: create and update can carry
/// a plaintext join key that must become `Zeroizing` runtime input immediately.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChannelHubAdminMutationArgs {
    CreateChannel {
        room: String,
        #[serde(default)]
        topic: Option<String>,
        policy: ChannelHubAdminRoomPolicyArgs,
        #[serde(default)]
        join_key: Option<String>,
    },
    UpdateChannel {
        room: String,
        /// Complete topic value; an empty string explicitly clears it.
        topic: String,
        policy: ChannelHubAdminRoomPolicyArgs,
        #[serde(default)]
        join_key: Option<String>,
        #[serde(default)]
        clear_join_key: bool,
    },
    UnregisterChannel {
        room: String,
    },
    SetRoomRole {
        room: String,
        target_identity: String,
        role: ChannelHubAdminRoomRoleArgs,
        enabled: bool,
    },
    SetRoomBan {
        room: String,
        target_identity: String,
        banned: bool,
    },
    SetInvitation {
        room: String,
        target_identity: String,
        invited: bool,
    },
    Kick {
        room: String,
        target_identity: String,
    },
    SetHubBan {
        target_identity: String,
        banned: bool,
    },
}

#[derive(Debug, Deserialize)]
pub struct ChannelHubConfigArgs {
    #[serde(default)]
    pub hub_name: Option<String>,
    #[serde(default)]
    pub greeting: Option<String>,
    #[serde(default)]
    pub announce_interval_secs: Option<u64>,
    /// Memory-only operator context. 0 disables it; otherwise whole hours
    /// between one and 24 are accepted.
    #[serde(default)]
    pub recent_activity_retention_secs: Option<u64>,
}

/// Stable read model for the desktop hosting surface. Saved settings remain
/// available while the live service or the entire Reticulum runtime is down.
#[derive(Debug, Serialize)]
pub struct ChannelHubOverview {
    pub supported: bool,
    /// Explicit opt-in for the operator UI and hosting command surface.
    pub hosting_enabled: bool,
    /// True once this Ratspeak identity has created a dedicated hub identity.
    pub created: bool,
    /// Stable public address, available even while the hub is stopped.
    pub destination_hash: Option<String>,
    pub settings: ChannelHubSettings,
    pub status: ChannelHubSnapshot,
}

async fn current_snapshot(state: &State<'_, Arc<AppState>>) -> ChannelHubSnapshot {
    let Some(hub) = state.channel_hub_handle() else {
        return ChannelHubSnapshot::stopped();
    };
    // `ChannelHubHandle::start` returns after registration and task spawn. A
    // direct lock snapshot can still be the pre-task `stopped()` seed, racing
    // the first emitted event and making a successful Start response look
    // offline. The command round-trip is ordered behind initial publication.
    hub.status()
        .await
        .unwrap_or_else(|_| ChannelHubSnapshot::stopped())
}

fn ensure_supported() -> AppResult<()> {
    if channel_hub_hosting_supported() {
        Ok(())
    } else {
        Err(AppError::new(
            "unsupported_platform",
            "Channel hub hosting is available on desktop",
        ))
    }
}

async fn load_settings(state: &State<'_, Arc<AppState>>) -> AppResult<ChannelHubSettings> {
    let pool = state.db.clone();
    crate::db::spawn_db(pool, move |pool| ChannelHubSettings::load(&pool))
        .await
        .map_err(|_| AppError::internal("channel hub settings task panicked"))?
        .map_err(AppError::database_unavailable)
}

async fn load_hosting_enabled(state: &State<'_, Arc<AppState>>) -> AppResult<bool> {
    let pool = state.db.clone();
    crate::db::spawn_db(pool, move |pool| channel_hosting_enabled(&pool))
        .await
        .map_err(|_| AppError::internal("channel hosting preference task panicked"))
}

async fn ensure_hosting_enabled(state: &State<'_, Arc<AppState>>) -> AppResult<()> {
    if load_hosting_enabled(state).await? {
        Ok(())
    } else {
        Err(AppError::bad_request(
            "Turn on Channel hosting in Settings first",
        ))
    }
}

async fn persist_settings(
    state: &State<'_, Arc<AppState>>,
    settings: &ChannelHubSettings,
) -> AppResult<()> {
    let pool = state.db.clone();
    let values = settings.setting_rows();
    crate::db::spawn_db(pool, move |pool| {
        crate::db::try_set_settings(&pool, &values)
    })
    .await
    .map_err(|_| AppError::internal("channel hub settings task panicked"))?
    .map_err(AppError::database_unavailable)
}

async fn shutdown_channel_hub(state: &State<'_, Arc<AppState>>) -> AppResult<()> {
    let Some(hub) = state.channel_hub_handle() else {
        return Ok(());
    };
    if !hub.shutdown().await {
        return Err(AppError::service_unavailable(
            "Channel hub is still shutting down",
        ));
    }
    state.take_channel_hub();
    Ok(())
}

fn active_operator_identity(state: &State<'_, Arc<AppState>>) -> AppResult<(String, [u8; 16])> {
    let identity_id = crate::helpers::active_identity_id(state);
    if !crate::helpers::validate_hex(&identity_id, 32, 32) {
        return Err(AppError::service_unavailable(
            "Channel hub administration requires an active identity",
        ));
    }
    let bytes = hex::decode(&identity_id)
        .ok()
        .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
        .ok_or_else(|| AppError::internal("active identity hash is invalid"))?;
    Ok((identity_id, bytes))
}

fn admin_target_identity(value: &str) -> AppResult<[u8; 16]> {
    if !crate::helpers::validate_hex(value, 32, 32) {
        return Err(AppError::bad_request(
            "Target identity must be a complete 32-character identity hash",
        ));
    }
    hex::decode(value)
        .ok()
        .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
        .ok_or_else(|| AppError::bad_request("Target identity hash is invalid"))
}

fn admin_mutation(args: ChannelHubAdminMutationArgs) -> AppResult<ChannelHubAdminMutation> {
    Ok(match args {
        ChannelHubAdminMutationArgs::CreateChannel {
            room,
            topic,
            policy,
            join_key,
        } => ChannelHubAdminMutation::CreateChannel {
            room,
            topic,
            policy: policy.into(),
            join_key: join_key.map(ChannelHubAdminSecret::new),
        },
        ChannelHubAdminMutationArgs::UpdateChannel {
            room,
            topic,
            policy,
            join_key,
            clear_join_key,
        } => {
            // Wrap before checking the mutually-exclusive controls so even a
            // rejected key is zeroized on this path.
            let join_key = join_key.map(ChannelHubAdminSecret::new);
            if join_key.is_some() && clear_join_key {
                return Err(AppError::bad_request(
                    "A join key cannot be set and cleared in the same change",
                ));
            }
            let join_key = match (join_key, clear_join_key) {
                (Some(key), false) => ChannelHubAdminKeyChange::Set(key),
                (None, true) => ChannelHubAdminKeyChange::Clear,
                (None, false) => ChannelHubAdminKeyChange::Keep,
                (Some(_), true) => unreachable!("conflict returned above"),
            };
            ChannelHubAdminMutation::UpdateChannel {
                room,
                topic: Some(topic),
                policy: policy.into(),
                join_key,
            }
        }
        ChannelHubAdminMutationArgs::UnregisterChannel { room } => {
            ChannelHubAdminMutation::UnregisterChannel { room }
        }
        ChannelHubAdminMutationArgs::SetRoomRole {
            room,
            target_identity,
            role,
            enabled,
        } => ChannelHubAdminMutation::SetRoomRole {
            room,
            target_identity: admin_target_identity(&target_identity)?,
            role: role.into(),
            enabled,
        },
        ChannelHubAdminMutationArgs::SetRoomBan {
            room,
            target_identity,
            banned,
        } => ChannelHubAdminMutation::SetRoomBan {
            room,
            target_identity: admin_target_identity(&target_identity)?,
            banned,
        },
        ChannelHubAdminMutationArgs::SetInvitation {
            room,
            target_identity,
            invited,
        } => ChannelHubAdminMutation::SetInvitation {
            room,
            target_identity: admin_target_identity(&target_identity)?,
            invited,
        },
        ChannelHubAdminMutationArgs::Kick {
            room,
            target_identity,
        } => ChannelHubAdminMutation::Kick {
            room,
            target_identity: admin_target_identity(&target_identity)?,
        },
        ChannelHubAdminMutationArgs::SetHubBan {
            target_identity,
            banned,
        } => ChannelHubAdminMutation::SetHubBan {
            target_identity: admin_target_identity(&target_identity)?,
            banned,
        },
    })
}

async fn overview(
    state: &State<'_, Arc<AppState>>,
    settings: ChannelHubSettings,
    hosting_enabled: bool,
) -> ChannelHubOverview {
    let status = current_snapshot(state).await;
    let identity_id = crate::helpers::active_identity_id(state);
    let identity_path =
        ratspeak_runtime::channel_hub::hub_identity_path(&state.config.data_dir, &identity_id);
    let created = !identity_id.is_empty() && identity_path.is_file();
    let destination_hash = status.destination_hash.clone().or_else(|| {
        created
            .then(|| {
                ratspeak_runtime::channel_hub::existing_hub_destination_hash(&identity_path)
                    .ok()
                    .flatten()
            })
            .flatten()
    });
    ChannelHubOverview {
        supported: channel_hub_hosting_supported(),
        hosting_enabled,
        created,
        destination_hash,
        settings,
        status,
    }
}

fn apply_config_args(
    mut settings: ChannelHubSettings,
    args: ChannelHubConfigArgs,
) -> AppResult<ChannelHubSettings> {
    if let Some(hub_name) = args.hub_name {
        let hub_name = sanitize_text(&hub_name, MAX_HUB_NAME_CHARS);
        if hub_name.is_empty() {
            return Err(AppError::bad_request("Hub name cannot be empty"));
        }
        settings.hub_name = hub_name;
    }
    if let Some(greeting) = args.greeting {
        settings.greeting = sanitize_text(&greeting, MAX_GREETING_CHARS);
    }
    if let Some(interval) = args.announce_interval_secs {
        if !valid_channel_hub_announce_interval_secs(interval) {
            return Err(AppError::bad_request(
                "Announce interval must be 15 minutes, 30 minutes, 1 hour, 12 hours, or 24 hours",
            ));
        }
        settings.announce_interval_secs = interval;
    }
    if let Some(retention) = args.recent_activity_retention_secs {
        if !valid_evidence_retention_secs(retention) {
            return Err(AppError::bad_request(
                "Recent activity must be off or between 1 and 24 whole hours",
            ));
        }
        settings.recent_activity_retention_secs = retention;
    }
    Ok(settings)
}

#[tauri::command]
pub async fn api_channel_hub(state: State<'_, Arc<AppState>>) -> AppResult<ChannelHubOverview> {
    let _control = state.channel_hub_control_lock.lock().await;
    let settings = load_settings(&state).await?;
    let hosting_enabled = load_hosting_enabled(&state).await?;
    Ok(overview(&state, settings, hosting_enabled).await)
}

#[tauri::command]
pub async fn api_channel_hub_admin(
    state: State<'_, Arc<AppState>>,
) -> AppResult<ChannelHubAdminSnapshot> {
    ensure_supported()?;
    let _control = state.channel_hub_control_lock.lock().await;
    let settings = load_settings(&state).await?;
    ensure_hosting_enabled(&state).await?;
    let (identity_id, operator_identity) = active_operator_identity(&state)?;
    if let Some(hub) = state.channel_hub_handle() {
        return hub.admin_snapshot().await.map_err(|_| {
            AppError::service_unavailable("Channel hub administration is temporarily unavailable")
        });
    }
    HubStore::new(state.db.clone(), identity_id)
        .admin_snapshot(settings.runtime_config(), operator_identity)
        .await
        .map_err(AppError::database_unavailable)
}

#[tauri::command]
pub async fn channel_hub_admin_mutate(
    state: State<'_, Arc<AppState>>,
    args: ChannelHubAdminMutationArgs,
) -> AppResult<ChannelHubAdminSnapshot> {
    ensure_supported()?;
    let _control = state.channel_hub_control_lock.lock().await;
    // Convert first so any join key becomes zeroizing input even when the
    // preference, identity, or live actor checks below reject the request.
    let mutation = admin_mutation(args)?;
    ensure_hosting_enabled(&state).await?;
    let (_, actor_identity) = active_operator_identity(&state)?;
    let hub = state.channel_hub_handle().ok_or_else(|| {
        AppError::service_unavailable("Start the channel hub before making administrative changes")
    })?;
    hub.admin_mutate(actor_identity, mutation)
        .await
        .map_err(|error| AppError::new(error.code(), error.message()))
}

#[tauri::command]
pub async fn channel_hub_start(state: State<'_, Arc<AppState>>) -> AppResult<ChannelHubOverview> {
    ensure_supported()?;
    let _control = state.channel_hub_control_lock.lock().await;
    let mut settings = load_settings(&state).await?;
    ensure_hosting_enabled(&state).await?;
    settings.enabled = true;
    persist_settings(&state, &settings).await?;
    let app_state: Arc<AppState> = state.inner().clone();
    if !ratspeak_runtime::start_channel_hub_service(&app_state).await {
        return Err(AppError::service_unavailable(
            "Channel hub requires an active network session",
        ));
    }
    Ok(overview(&state, settings, true).await)
}

#[tauri::command]
pub async fn channel_hub_stop(state: State<'_, Arc<AppState>>) -> AppResult<ChannelHubOverview> {
    ensure_supported()?;
    let _control = state.channel_hub_control_lock.lock().await;
    let mut settings = load_settings(&state).await?;
    let hosting_enabled = load_hosting_enabled(&state).await?;
    settings.enabled = false;
    persist_settings(&state, &settings).await?;
    shutdown_channel_hub(&state).await?;
    Ok(overview(&state, settings, hosting_enabled).await)
}

/// Master opt-in for hub hosting. Disabling it also stops the live service so
/// the UI can never hide a hub that is still relaying traffic. Hub identity,
/// configuration, and channel policy remain available for a later opt-in.
#[tauri::command]
pub async fn set_channel_hosting_enabled(
    state: State<'_, Arc<AppState>>,
    enabled: bool,
) -> AppResult<ChannelHubOverview> {
    if enabled {
        ensure_supported()?;
    }
    let _control = state.channel_hub_control_lock.lock().await;
    let mut settings = load_settings(&state).await?;
    if !enabled {
        // Keep the preference On until teardown is acknowledged. The UI must
        // never claim hosting is Off while a relay actor may still be live.
        shutdown_channel_hub(&state).await?;
        settings.enabled = false;
    }

    let mut values = if enabled {
        Vec::new()
    } else {
        settings.setting_rows()
    };
    values.push((
        CHANNEL_HOSTING_ENABLED_KEY.to_string(),
        if enabled { "1" } else { "0" }.to_string(),
    ));
    values.push((
        CHANNEL_HOSTING_PREFERENCE_VERSION_KEY.to_string(),
        CHANNEL_HOSTING_PREFERENCE_VERSION.to_string(),
    ));
    let pool = state.db.clone();
    crate::db::spawn_db(pool, move |pool| {
        crate::db::try_set_settings(&pool, &values)
    })
    .await
    .map_err(|_| AppError::internal("channel hosting preference task panicked"))?
    .map_err(AppError::database_unavailable)?;

    state.emit_to_all(
        "app_settings_updated",
        json!({ "channel_hosting_enabled": enabled }),
    );
    Ok(overview(&state, settings, enabled).await)
}

#[tauri::command]
pub async fn channel_hub_set_config(
    state: State<'_, Arc<AppState>>,
    args: ChannelHubConfigArgs,
) -> AppResult<ChannelHubOverview> {
    ensure_supported()?;
    let _control = state.channel_hub_control_lock.lock().await;
    let current = load_settings(&state).await?;
    ensure_hosting_enabled(&state).await?;
    let settings = apply_config_args(current, args)?;
    persist_settings(&state, &settings).await?;

    if let Some(hub) = state.channel_hub_handle() {
        if !hub.shutdown().await {
            return Err(AppError::service_unavailable(
                "Channel hub is still shutting down",
            ));
        }
        state.take_channel_hub();
        let app_state: Arc<AppState> = state.inner().clone();
        if !ratspeak_runtime::start_channel_hub_service(&app_state).await {
            return Err(AppError::service_unavailable(
                "Changes were saved, but the channel hub could not restart",
            ));
        }
    }
    Ok(overview(&state, settings, true).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_updates_are_validated_before_producing_a_candidate() {
        let original = ChannelHubSettings::default();
        let error = apply_config_args(
            original.clone(),
            ChannelHubConfigArgs {
                hub_name: Some("New name".to_string()),
                greeting: None,
                announce_interval_secs: Some(42),
                recent_activity_retention_secs: None,
            },
        )
        .unwrap_err();

        assert_eq!(error.code, "bad_request");
        assert_eq!(
            original.hub_name,
            ratspeak_runtime::channel_hub::DEFAULT_HUB_NAME
        );

        let error = apply_config_args(
            original,
            ChannelHubConfigArgs {
                hub_name: None,
                greeting: None,
                announce_interval_secs: None,
                recent_activity_retention_secs: Some(900),
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "bad_request");
    }

    #[test]
    fn partial_config_updates_preserve_unspecified_fields() {
        let original = ChannelHubSettings {
            enabled: true,
            hub_name: "Existing".to_string(),
            greeting: "Hello".to_string(),
            announce_interval_secs: 900,
            recent_activity_retention_secs: 3600,
        };
        let updated = apply_config_args(
            original,
            ChannelHubConfigArgs {
                hub_name: Some("  Mountain relay  ".to_string()),
                greeting: None,
                announce_interval_secs: Some(43_200),
                recent_activity_retention_secs: Some(21_600),
            },
        )
        .unwrap();

        assert!(updated.enabled);
        assert_eq!(updated.hub_name, "Mountain relay");
        assert_eq!(updated.greeting, "Hello");
        assert_eq!(updated.announce_interval_secs, 43_200);
        assert_eq!(updated.recent_activity_retention_secs, 21_600);
    }

    fn room_policy_args() -> ChannelHubAdminRoomPolicyArgs {
        ChannelHubAdminRoomPolicyArgs {
            invite_only: false,
            moderated: false,
            no_outside_messages: true,
            private: false,
            topic_operators_only: true,
        }
    }

    #[test]
    fn admin_mutation_conversion_requires_full_identity_hashes() {
        let error = match admin_mutation(ChannelHubAdminMutationArgs::Kick {
            room: "lobby".to_string(),
            target_identity: "aabbcc".to_string(),
        }) {
            Err(error) => error,
            Ok(_) => panic!("a partial identity hash must be rejected"),
        };
        assert_eq!(error.code, "bad_request");

        let mutation = admin_mutation(ChannelHubAdminMutationArgs::SetRoomRole {
            room: "lobby".to_string(),
            target_identity: "BB".repeat(16),
            role: ChannelHubAdminRoomRoleArgs::Voice,
            enabled: true,
        })
        .unwrap();
        let ChannelHubAdminMutation::SetRoomRole {
            target_identity,
            role,
            ..
        } = mutation
        else {
            panic!("expected a room-role mutation");
        };
        assert_eq!(target_identity, [0xBB; 16]);
        assert!(matches!(role, ChannelHubAdminRoomRole::Voice));
    }

    #[test]
    fn admin_update_key_controls_are_explicit_and_mutually_exclusive() {
        let error = match admin_mutation(ChannelHubAdminMutationArgs::UpdateChannel {
            room: "vault".to_string(),
            topic: String::new(),
            policy: room_policy_args(),
            join_key: Some("secret-key".to_string()),
            clear_join_key: true,
        }) {
            Err(error) => error,
            Ok(_) => panic!("set and clear must be mutually exclusive"),
        };
        assert_eq!(error.code, "bad_request");

        let mutation = admin_mutation(ChannelHubAdminMutationArgs::UpdateChannel {
            room: "vault".to_string(),
            topic: String::new(),
            policy: room_policy_args(),
            join_key: None,
            clear_join_key: false,
        })
        .unwrap();
        let ChannelHubAdminMutation::UpdateChannel {
            topic, join_key, ..
        } = mutation
        else {
            panic!("expected an update mutation");
        };
        assert_eq!(topic.as_deref(), Some(""));
        assert!(matches!(join_key, ChannelHubAdminKeyChange::Keep));
    }

    #[test]
    fn admin_update_deserialization_rejects_partial_or_unknown_policy() {
        let missing_topic = serde_json::json!({
            "action": "update_channel",
            "room": "vault",
            "policy": {
                "invite_only": false,
                "moderated": false,
                "no_outside_messages": true,
                "private": false,
                "topic_operators_only": true
            }
        });
        assert!(
            serde_json::from_value::<ChannelHubAdminMutationArgs>(missing_topic).is_err(),
            "an omitted complete topic must not silently clear it"
        );

        let unknown_policy = serde_json::json!({
            "action": "create_channel",
            "room": "vault",
            "policy": {
                "invite_only": false,
                "moderated": false,
                "no_outside_messages": true,
                "private": false,
                "topic_operators_only": true,
                "future_mode": true
            }
        });
        assert!(
            serde_json::from_value::<ChannelHubAdminMutationArgs>(unknown_policy).is_err(),
            "unknown authority fields require an explicit model update"
        );
    }
}
