//! LXST voice commands.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::commands::shared::{hex_to_array16, resolve_identity_hash};
use crate::error::{AppError, AppResult};
use crate::helpers::validate_hex;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct VoiceCallArgs {
    pub hash: String,
}

#[tauri::command]
pub async fn voice_start_service(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let app_state = state.inner().clone();
    crate::voice::start_voice_service(&app_state)
        .await
        .map_err(AppError::service_unavailable)?;
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub async fn voice_stop_service(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let app_state = state.inner().clone();
    crate::voice::shutdown_voice_service(&app_state).await;
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub async fn voice_status(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    Ok(crate::voice::voice_status(&state))
}

#[tauri::command]
pub async fn voice_call(state: State<'_, Arc<AppState>>, args: VoiceCallArgs) -> AppResult<Value> {
    let hash = args.hash.trim().to_ascii_lowercase();
    if !validate_hex(&hash, 32, 32) {
        return Err(AppError::bad_request(
            "Voice calls require a 16-byte contact or identity hash",
        ));
    }

    let input = hex_to_array16(&hash)
        .ok_or_else(|| AppError::bad_request("Voice calls require a 16-byte hash"))?;
    let remote_identity = resolve_identity_hash(&state, input).await.unwrap_or(input);

    let app_state = state.inner().clone();
    let mut result = crate::voice::call_identity(&app_state, remote_identity)
        .await
        .map_err(AppError::service_unavailable)?;
    if let Some(obj) = result.as_object_mut() {
        obj.insert("requested_hash".to_string(), json!(hash));
        obj.insert(
            "resolved_identity".to_string(),
            json!(hex::encode(remote_identity)),
        );
        obj.insert(
            "hash_was_resolved".to_string(),
            json!(remote_identity != input),
        );
    }
    Ok(result)
}

#[tauri::command]
pub async fn voice_answer(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let app_state = state.inner().clone();
    crate::voice::answer(&app_state)
        .await
        .map_err(AppError::service_unavailable)
}

#[tauri::command]
pub async fn voice_reject(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let app_state = state.inner().clone();
    crate::voice::reject(&app_state)
        .await
        .map_err(AppError::service_unavailable)
}

#[tauri::command]
pub async fn voice_hangup(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let app_state = state.inner().clone();
    crate::voice::hangup(&app_state)
        .await
        .map_err(AppError::service_unavailable)
}
