//! Channel hub (RRC server) IPC commands. Hub relay traffic is live session
//! state. Two things persist: operator configuration in the settings table,
//! and the `channel_hub_*` room registry owned by `ratspeak-runtime`.

use std::sync::Arc;

use ratspeak_runtime::channel_hub::ChannelHubSnapshot;
use serde::Deserialize;
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

fn current_snapshot(state: &State<'_, Arc<AppState>>) -> ChannelHubSnapshot {
    state
        .channel_hub_handle()
        .map(|hub| hub.snapshot())
        .unwrap_or_else(ChannelHubSnapshot::stopped)
}

async fn persist_setting(
    state: &State<'_, Arc<AppState>>,
    key: &'static str,
    value: String,
) -> AppResult<()> {
    let pool = state.db.clone();
    crate::db::spawn_db(pool, move |pool| crate::db::set_setting(&pool, key, &value))
        .await
        .map_err(|_| AppError::internal("channel hub settings task panicked"))?;
    Ok(())
}

/// Restart a running hub so configuration changes take effect. Returns false
/// if it was running and did not come back, which the caller surfaces rather
/// than leaving the operator thinking the hub is still up.
async fn restart_running_hub(state: &State<'_, Arc<AppState>>) -> bool {
    let Some(hub) = state.take_channel_hub() else {
        return true;
    };
    hub.shutdown().await;
    let app_state: Arc<AppState> = state.inner().clone();
    ratspeak_runtime::start_channel_hub_service(&app_state).await
}

#[tauri::command]
pub async fn api_channel_hub(state: State<'_, Arc<AppState>>) -> AppResult<ChannelHubSnapshot> {
    Ok(current_snapshot(&state))
}

#[tauri::command]
pub async fn channel_hub_start(state: State<'_, Arc<AppState>>) -> AppResult<ChannelHubSnapshot> {
    persist_setting(&state, "channel_hub_enabled", "1".to_string()).await?;
    let app_state: Arc<AppState> = state.inner().clone();
    if !ratspeak_runtime::start_channel_hub_service(&app_state).await {
        return Err(AppError::service_unavailable(
            "Channel hub requires an active network session",
        ));
    }
    Ok(current_snapshot(&state))
}

#[tauri::command]
pub async fn channel_hub_stop(state: State<'_, Arc<AppState>>) -> AppResult<ChannelHubSnapshot> {
    persist_setting(&state, "channel_hub_enabled", "0".to_string()).await?;
    if let Some(hub) = state.take_channel_hub() {
        hub.shutdown().await;
    }
    Ok(ChannelHubSnapshot::stopped())
}

#[tauri::command]
pub async fn channel_hub_set_config(
    state: State<'_, Arc<AppState>>,
    args: ChannelHubConfigArgs,
) -> AppResult<ChannelHubSnapshot> {
    if let Some(hub_name) = args.hub_name {
        let hub_name = sanitize_text(&hub_name, MAX_HUB_NAME_CHARS);
        if hub_name.trim().is_empty() {
            return Err(AppError::bad_request("Hub name cannot be empty"));
        }
        persist_setting(&state, "channel_hub_name", hub_name).await?;
    }
    if let Some(greeting) = args.greeting {
        // An empty greeting clears the setting.
        let greeting = sanitize_text(&greeting, MAX_GREETING_CHARS);
        persist_setting(&state, "channel_hub_greeting", greeting).await?;
    }
    if let Some(interval) = args.announce_interval_secs {
        if interval != 0 && !(300..=86_400).contains(&interval) {
            return Err(AppError::bad_request(
                "Announce interval must be 0 or between 5 minutes and 24 hours",
            ));
        }
        persist_setting(
            &state,
            "channel_hub_announce_interval",
            interval.to_string(),
        )
        .await?;
    }
    for (key, value) in [
        ("channel_hub_resource_send", args.resource_send),
        ("channel_hub_resource_accept", args.resource_accept),
    ] {
        if let Some(enabled) = value {
            persist_setting(&state, key, if enabled { "1" } else { "0" }.to_string()).await?;
        }
    }
    if !restart_running_hub(&state).await {
        return Err(AppError::service_unavailable(
            "Channel hub could not restart with the new configuration",
        ));
    }
    Ok(current_snapshot(&state))
}
