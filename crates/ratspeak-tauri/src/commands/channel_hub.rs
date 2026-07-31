//! Channel hub (RRC server) IPC commands. Hub relay traffic is live session
//! state. Two things persist: operator configuration in the settings table,
//! and the `channel_hub_*` room registry owned by `ratspeak-runtime`.

use std::sync::Arc;

use ratspeak_runtime::channel_hub::{
    ChannelHubAdminSnapshot, ChannelHubSettings, ChannelHubSnapshot, HubStore,
    channel_hub_hosting_supported,
};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::helpers::sanitize_text;
use crate::state::AppState;

const MAX_HUB_NAME_CHARS: usize = 64;
const MAX_GREETING_CHARS: usize = 512;

#[derive(Debug, Deserialize)]
pub struct ChannelHubConfigArgs {
    #[serde(default)]
    pub hub_name: Option<String>,
    #[serde(default)]
    pub greeting: Option<String>,
    #[serde(default)]
    pub announce_interval_secs: Option<u64>,
    /// Send oversized greetings as a resource, and advertise the capability.
    #[serde(default)]
    pub resource_send: Option<bool>,
    /// Accept inbound resource notices. Off by default.
    #[serde(default)]
    pub resource_accept: Option<bool>,
}

/// Stable read model for the desktop hosting surface. Saved settings remain
/// available while the live service or the entire Reticulum runtime is down.
#[derive(Debug, Serialize)]
pub struct ChannelHubOverview {
    pub supported: bool,
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

async fn overview(
    state: &State<'_, Arc<AppState>>,
    settings: ChannelHubSettings,
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
        if interval != 0 && !(300..=86_400).contains(&interval) {
            return Err(AppError::bad_request(
                "Announce interval must be 0 or between 5 minutes and 24 hours",
            ));
        }
        settings.announce_interval_secs = interval;
    }
    if let Some(enabled) = args.resource_send {
        settings.resource_send_enabled = enabled;
    }
    if let Some(enabled) = args.resource_accept {
        settings.resource_accept_enabled = enabled;
    }
    Ok(settings)
}

#[tauri::command]
pub async fn api_channel_hub(state: State<'_, Arc<AppState>>) -> AppResult<ChannelHubOverview> {
    let _control = state.channel_hub_control_lock.lock().await;
    let settings = load_settings(&state).await?;
    Ok(overview(&state, settings).await)
}

#[tauri::command]
pub async fn api_channel_hub_admin(
    state: State<'_, Arc<AppState>>,
) -> AppResult<ChannelHubAdminSnapshot> {
    ensure_supported()?;
    let _control = state.channel_hub_control_lock.lock().await;
    let (identity_id, operator_identity) = active_operator_identity(&state)?;
    if let Some(hub) = state.channel_hub_handle() {
        return hub.admin_snapshot().await.map_err(|_| {
            AppError::service_unavailable("Channel hub administration is temporarily unavailable")
        });
    }
    let settings = load_settings(&state).await?;
    HubStore::new(state.db.clone(), identity_id)
        .admin_snapshot(settings.runtime_config(), operator_identity)
        .await
        .map_err(AppError::database_unavailable)
}

#[tauri::command]
pub async fn channel_hub_start(state: State<'_, Arc<AppState>>) -> AppResult<ChannelHubOverview> {
    ensure_supported()?;
    let _control = state.channel_hub_control_lock.lock().await;
    let mut settings = load_settings(&state).await?;
    settings.enabled = true;
    persist_settings(&state, &settings).await?;
    let app_state: Arc<AppState> = state.inner().clone();
    if !ratspeak_runtime::start_channel_hub_service(&app_state).await {
        return Err(AppError::service_unavailable(
            "Channel hub requires an active network session",
        ));
    }
    Ok(overview(&state, settings).await)
}

#[tauri::command]
pub async fn channel_hub_stop(state: State<'_, Arc<AppState>>) -> AppResult<ChannelHubOverview> {
    ensure_supported()?;
    let _control = state.channel_hub_control_lock.lock().await;
    let mut settings = load_settings(&state).await?;
    settings.enabled = false;
    persist_settings(&state, &settings).await?;
    if let Some(hub) = state.channel_hub_handle() {
        if !hub.shutdown().await {
            return Err(AppError::service_unavailable(
                "Channel hub is still shutting down",
            ));
        }
        state.take_channel_hub();
    }
    Ok(overview(&state, settings).await)
}

#[tauri::command]
pub async fn channel_hub_set_config(
    state: State<'_, Arc<AppState>>,
    args: ChannelHubConfigArgs,
) -> AppResult<ChannelHubOverview> {
    ensure_supported()?;
    let _control = state.channel_hub_control_lock.lock().await;
    let settings = apply_config_args(load_settings(&state).await?, args)?;
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
    Ok(overview(&state, settings).await)
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
                resource_send: None,
                resource_accept: None,
            },
        )
        .unwrap_err();

        assert_eq!(error.code, "bad_request");
        assert_eq!(
            original.hub_name,
            ratspeak_runtime::channel_hub::DEFAULT_HUB_NAME
        );
    }

    #[test]
    fn partial_config_updates_preserve_unspecified_fields() {
        let original = ChannelHubSettings {
            enabled: true,
            hub_name: "Existing".to_string(),
            greeting: "Hello".to_string(),
            announce_interval_secs: 900,
            resource_send_enabled: true,
            resource_accept_enabled: false,
        };
        let updated = apply_config_args(
            original,
            ChannelHubConfigArgs {
                hub_name: Some("  Mountain relay  ".to_string()),
                greeting: None,
                announce_interval_secs: None,
                resource_send: None,
                resource_accept: Some(true),
            },
        )
        .unwrap();

        assert!(updated.enabled);
        assert_eq!(updated.hub_name, "Mountain relay");
        assert_eq!(updated.greeting, "Hello");
        assert_eq!(updated.announce_interval_secs, 900);
        assert!(updated.resource_send_enabled);
        assert!(updated.resource_accept_enabled);
    }
}
