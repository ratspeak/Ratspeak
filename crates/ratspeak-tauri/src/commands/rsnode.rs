//! rsNode firmware updates from a node's conversation. The `.rsota` arrives through attachment
//! staging; the transfer itself runs in `ratspeak_runtime::rsnode::update`.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::helpers::{sanitize_text, validate_hex};
use crate::state::AppState;
use ratspeak_runtime::rsnode::update::{self, StartError};

/// Largest `.rsota` accepted: a manifest and a full flash slot.
const RSOTA_MAX_BYTES: usize = 8 * 1024 * 1024;

fn contact_hash(value: &str) -> AppResult<String> {
    let hash = sanitize_text(value, 64).to_ascii_lowercase();
    if !validate_hex(&hash, 32, 32) {
        return Err(AppError::bad_request("Invalid contact"));
    }
    Ok(hash)
}

#[derive(Deserialize)]
pub struct RsnodeUpdateStartArgs {
    pub dest_hash: String,
    /// A completed attachment stage holding the `.rsota`.
    pub stage_token: String,
}

#[tauri::command]
pub async fn rsnode_update_start(
    state: State<'_, Arc<AppState>>,
    args: RsnodeUpdateStartArgs,
) -> AppResult<Value> {
    let contact = contact_hash(&args.dest_hash)?;
    let staged = state
        .take_completed_attachment_staging(&args.stage_token)
        .ok_or_else(|| AppError::not_found("The update file is no longer staged"))?;
    if staged.declared_size > RSOTA_MAX_BYTES {
        return Err(AppError::bad_request(
            "This file is too large for an rsNode update.",
        ));
    }
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(&staged.path))
        .await
        .map_err(|_| AppError::internal("update file read panicked"))?
        .map_err(|_| {
            AppError::new(
                "attachment_storage_failed",
                "Could not read the update file",
            )
        })?;
    update::start(&state, &contact, bytes)
        .await
        .map_err(|error| match error {
            StartError::Invalid(message) => AppError::bad_request(message),
            StartError::Conflict(message) => AppError::conflict(message),
            StartError::Unavailable(message) => AppError::service_unavailable(message),
        })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn rsnode_update_cancel(
    state: State<'_, Arc<AppState>>,
    dest_hash: String,
) -> AppResult<Value> {
    let contact = contact_hash(&dest_hash)?;
    Ok(json!({ "cancelled": update::cancel(&state, &contact) }))
}

/// The running update to this node, if any, as its last progress event.
#[tauri::command(rename_all = "snake_case")]
pub async fn rsnode_update_state(
    state: State<'_, Arc<AppState>>,
    dest_hash: String,
) -> AppResult<Value> {
    let contact = contact_hash(&dest_hash)?;
    Ok(update::current(&state, &contact).unwrap_or(Value::Null))
}
