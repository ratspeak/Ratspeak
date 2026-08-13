//! LXST voice commands.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;
use tokio::io::AsyncReadExt as _;

use crate::commands::shared::{hex_to_array16, resolve_identity_hash};
use crate::error::{AppError, AppResult};
use crate::helpers::validate_hex;
use crate::state::AppState;

const VOICE_MEMO_START_UNAVAILABLE: &str = "Ratspeak couldn't start recording. Check microphone access and the selected input device, then try again.";

fn voice_memo_start_error(error: String) -> AppError {
    let reason = if error.contains("codec") {
        "codec_start_failed"
    } else if error.contains("microphone") || error.contains("Microphone") {
        "microphone_start_failed"
    } else {
        "recorder_start_failed"
    };
    tracing::warn!(reason, "voice message recorder could not start");
    AppError::service_unavailable(VOICE_MEMO_START_UNAVAILABLE)
}

fn voice_memo_session_error(error: String) -> AppError {
    if error.contains("No matching voice memo recording") {
        AppError::conflict(error)
    } else {
        AppError::service_unavailable(error)
    }
}

#[derive(Deserialize)]
pub struct VoiceCallArgs {
    pub hash: String,
}

#[derive(Deserialize)]
pub struct VoiceSetMicrophoneMutedArgs {
    pub muted: bool,
}

#[derive(Deserialize)]
pub struct VoiceRestartSpeakerArgs {
    pub speakerphone: bool,
}

#[derive(Deserialize)]
pub struct VoiceMemoPauseArgs {
    pub session_id: String,
    pub paused: bool,
}

#[derive(Deserialize)]
pub struct VoiceMemoSessionArgs {
    pub session_id: String,
}

#[derive(Deserialize)]
pub struct VoiceMemoPlaybackLeaseArgs {
    pub lease_id: String,
}

#[derive(Deserialize)]
pub struct VoiceMemoDecodeDataArgs {
    pub data_base64: String,
}

#[derive(Deserialize)]
pub struct VoiceMemoDecodeStoredArgs {
    pub stored_name: String,
}

#[derive(Serialize)]
pub struct VoiceMemoDraftResponse {
    pub session_id: String,
    pub filename: String,
    pub mime: String,
    pub data_base64: String,
    pub size: usize,
    pub duration_ms: u32,
    pub waveform: Vec<u8>,
}

#[derive(Serialize)]
pub struct VoiceMemoPlaybackResponse {
    pub mime: String,
    pub data_base64: String,
    pub duration_ms: u32,
    pub waveform: Vec<u8>,
    pub sample_rate_hz: u32,
    pub channels: u8,
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
    // TODO: Warn or block when the contact has not advertised `lxst.telephony`.

    let app_state = state.inner().clone();
    // Calls and voice memos share the platform capture device. Enforce the
    // handoff natively as well as in the WebView so a stale client cannot
    // leave an unseen recorder competing for the microphone.
    crate::voice::reserve_call_audio(&app_state);
    if let Err(error) = crate::voice_memo::cancel_recording(&app_state).await {
        crate::voice::release_call_audio(&app_state);
        return Err(AppError::service_unavailable(error));
    }
    let mut result = match crate::voice::call_identity(&app_state, remote_identity).await {
        Ok(result) => result,
        Err(error) => {
            crate::voice::release_call_audio(&app_state);
            return Err(AppError::service_unavailable(error));
        }
    };
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
    crate::voice::reserve_call_audio(&app_state);
    if let Err(error) = crate::voice_memo::cancel_recording(&app_state).await {
        crate::voice::release_call_audio(&app_state);
        return Err(AppError::service_unavailable(error));
    }
    match crate::voice::answer(&app_state).await {
        Ok(result) => Ok(result),
        Err(error) => {
            crate::voice::release_call_audio(&app_state);
            Err(AppError::service_unavailable(error))
        }
    }
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

#[tauri::command]
pub async fn voice_set_microphone_muted(
    state: State<'_, Arc<AppState>>,
    args: VoiceSetMicrophoneMutedArgs,
) -> AppResult<Value> {
    crate::voice::set_microphone_muted(&state, args.muted).map_err(AppError::service_unavailable)
}

#[tauri::command]
pub async fn voice_restart_speaker(
    state: State<'_, Arc<AppState>>,
    args: VoiceRestartSpeakerArgs,
) -> AppResult<Value> {
    crate::voice::restart_speaker(&state, args.speakerphone)
        .await
        .map_err(AppError::service_unavailable)
}

#[tauri::command]
pub async fn voice_memo_start(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    crate::voice_memo::start_recording(state.inner())
        .await
        .map(|status| {
            serde_json::to_value(status).unwrap_or_else(|_| json!({ "state": "recording" }))
        })
        .map_err(voice_memo_start_error)
}

#[tauri::command]
pub async fn voice_memo_status(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    Ok(
        serde_json::to_value(crate::voice_memo::recording_status(&state))
            .unwrap_or_else(|_| json!({ "state": "idle" })),
    )
}

#[tauri::command]
pub async fn voice_memo_pause(
    state: State<'_, Arc<AppState>>,
    args: VoiceMemoPauseArgs,
) -> AppResult<Value> {
    let session_id = crate::voice_memo::parse_recording_session_id(&args.session_id)
        .ok_or_else(|| AppError::bad_request("Voice message recording session is invalid"))?;
    crate::voice_memo::set_paused(&state, session_id, args.paused)
        .await
        .map(|status| serde_json::to_value(status).unwrap_or_else(|_| json!({})))
        .map_err(voice_memo_session_error)
}

#[tauri::command]
pub async fn voice_memo_stop(
    state: State<'_, Arc<AppState>>,
    args: VoiceMemoSessionArgs,
) -> AppResult<VoiceMemoDraftResponse> {
    let session_id = crate::voice_memo::parse_recording_session_id(&args.session_id)
        .ok_or_else(|| AppError::bad_request("Voice message recording session is invalid"))?;
    let draft = crate::voice_memo::stop_recording(&state, session_id)
        .await
        .map_err(voice_memo_session_error)?;
    let size = draft.data.len();
    Ok(VoiceMemoDraftResponse {
        session_id: args.session_id,
        filename: crate::voice_memo::VOICE_MEMO_FILENAME.to_string(),
        mime: crate::voice_memo::VOICE_MEMO_MIME.to_string(),
        data_base64: B64.encode(&draft.data),
        size,
        duration_ms: draft.duration_ms,
        waveform: draft.waveform,
    })
}

#[tauri::command]
pub async fn voice_memo_cancel(
    state: State<'_, Arc<AppState>>,
    args: VoiceMemoSessionArgs,
) -> AppResult<Value> {
    let session_id = crate::voice_memo::parse_recording_session_id(&args.session_id)
        .ok_or_else(|| AppError::bad_request("Voice message recording session is invalid"))?;
    crate::voice_memo::cancel_recording_session(&state, session_id)
        .await
        .map_err(voice_memo_session_error)?;
    Ok(json!({ "ok": true, "session_id": args.session_id }))
}

#[tauri::command]
pub async fn voice_memo_playback_session_start(
    state: State<'_, Arc<AppState>>,
) -> AppResult<Value> {
    let lease_id = crate::voice_memo::start_playback_session(&state)
        .await
        .map_err(|error| {
            if error.contains("using audio") || error.contains("being recorded") {
                AppError::conflict(error)
            } else {
                AppError::service_unavailable(error)
            }
        })?;
    Ok(json!({
        "ok": true,
        "lease_id": crate::voice_memo::format_playback_lease_id(lease_id),
    }))
}

#[tauri::command]
pub async fn voice_memo_playback_session_stop(
    state: State<'_, Arc<AppState>>,
    args: VoiceMemoPlaybackLeaseArgs,
) -> AppResult<Value> {
    let lease_id = crate::voice_memo::parse_playback_lease_id(&args.lease_id)
        .ok_or_else(|| AppError::bad_request("Voice message playback lease is invalid"))?;
    let released = crate::voice_memo::stop_playback_session(&state, lease_id).await;
    Ok(json!({ "ok": true, "released": released, "lease_id": args.lease_id }))
}

#[tauri::command]
pub async fn voice_memo_decode_data(
    state: State<'_, Arc<AppState>>,
    args: VoiceMemoDecodeDataArgs,
) -> AppResult<VoiceMemoPlaybackResponse> {
    let max_encoded = crate::voice_memo::VOICE_MEMO_MAX_CONTAINER_BYTES
        .div_ceil(3)
        .saturating_mul(4);
    if args.data_base64.len() > max_encoded {
        return Err(AppError::bad_request("Voice memo data is too large"));
    }
    let data = B64
        .decode(args.data_base64)
        .map_err(|_| AppError::bad_request("Voice memo data is not valid base64"))?;
    ensure_voice_memo_container_size(data.len())?;
    decode_voice_memo_response(&state, data).await
}

#[tauri::command]
pub async fn voice_memo_decode_stored(
    state: State<'_, Arc<AppState>>,
    args: VoiceMemoDecodeStoredArgs,
) -> AppResult<VoiceMemoPlaybackResponse> {
    let path = state
        .lxmf
        .lock()
        .ok()
        .and_then(|manager| {
            manager
                .as_ref()
                .and_then(|manager| manager.get_received_file(&args.stored_name))
        })
        .ok_or_else(|| AppError::not_found("Voice memo not found"))?;
    let data = read_bounded_voice_memo(path).await?;
    decode_voice_memo_response(&state, data).await
}

#[tauri::command]
pub async fn voice_memo_inspect_stored(
    state: State<'_, Arc<AppState>>,
    args: VoiceMemoDecodeStoredArgs,
) -> AppResult<Value> {
    let path = state
        .lxmf
        .lock()
        .ok()
        .and_then(|manager| {
            manager
                .as_ref()
                .and_then(|manager| manager.get_received_file(&args.stored_name))
        })
        .ok_or_else(|| AppError::not_found("Voice memo not found"))?;
    let data = read_bounded_voice_memo(path).await?;
    let metadata =
        tokio::task::spawn_blocking(move || crate::voice_memo::inspect_voice_memo(&data))
            .await
            .map_err(|_| AppError::internal("Voice memo inspector task panicked"))?
            .map_err(AppError::bad_request)?;
    Ok(serde_json::to_value(metadata).unwrap_or_else(|_| json!({})))
}

fn ensure_voice_memo_container_size(size: usize) -> AppResult<()> {
    if size > crate::voice_memo::VOICE_MEMO_MAX_CONTAINER_BYTES {
        return Err(AppError::bad_request("Voice memo data is too large"));
    }
    Ok(())
}

async fn read_bounded_voice_memo(path: std::path::PathBuf) -> AppResult<Vec<u8>> {
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| AppError::not_found("Voice memo not found"))?;
    let size = usize::try_from(metadata.len())
        .map_err(|_| AppError::bad_request("Voice memo data is too large"))?;
    ensure_voice_memo_container_size(size)?;
    let mut data = Vec::with_capacity(size);
    tokio::fs::File::open(path)
        .await
        .map_err(|_| AppError::not_found("Voice memo not found"))?
        .take((crate::voice_memo::VOICE_MEMO_MAX_CONTAINER_BYTES as u64) + 1)
        .read_to_end(&mut data)
        .await
        .map_err(|_| AppError::not_found("Voice memo not found"))?;
    // Bound the read as well as checking metadata, since the private file could
    // have been replaced or grown between those operations.
    ensure_voice_memo_container_size(data.len())?;
    Ok(data)
}

async fn decode_voice_memo_response(
    state: &AppState,
    data: Vec<u8>,
) -> AppResult<VoiceMemoPlaybackResponse> {
    ensure_voice_memo_container_size(data.len())?;
    let _decode = state.voice_memo_decode_lock.lock().await;
    let playback = tokio::task::spawn_blocking(move || crate::voice_memo::decode_voice_memo(&data))
        .await
        .map_err(|_| AppError::internal("Voice memo decoder task panicked"))?
        .map_err(AppError::bad_request)?;
    Ok(VoiceMemoPlaybackResponse {
        mime: "audio/wav".to_string(),
        data_base64: B64.encode(&playback.wav_data),
        duration_ms: playback.duration_ms,
        waveform: playback.waveform,
        sample_rate_hz: playback.sample_rate_hz,
        channels: playback.channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_memo_start_errors_do_not_expose_audio_backend_diagnostics() {
        let error = voice_memo_start_error(
            "Failed to read microphone configuration: backend-specific CoreAudio failure"
                .to_string(),
        );

        assert_eq!(error.code, "service_unavailable");
        assert_eq!(error.message, VOICE_MEMO_START_UNAVAILABLE);
        assert!(!error.message.contains("CoreAudio"));
        assert!(!error.message.contains("backend"));
    }

    #[test]
    fn voice_memo_container_limit_rejects_only_oversize() {
        assert!(
            ensure_voice_memo_container_size(crate::voice_memo::VOICE_MEMO_MAX_CONTAINER_BYTES)
                .is_ok()
        );
        let error =
            ensure_voice_memo_container_size(crate::voice_memo::VOICE_MEMO_MAX_CONTAINER_BYTES + 1)
                .expect_err("oversized memo must be rejected before decode");
        assert_eq!(error.code, "bad_request");
    }

    #[tokio::test]
    async fn stored_voice_memo_is_rejected_from_metadata_before_read() {
        let path = std::env::temp_dir().join(format!(
            "ratspeak-voice-oversize-{}-{}.lxvm",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let file = std::fs::File::create(&path).expect("create sparse fixture");
        file.set_len((crate::voice_memo::VOICE_MEMO_MAX_CONTAINER_BYTES + 1) as u64)
            .expect("set sparse fixture length");
        drop(file);

        let error = read_bounded_voice_memo(path.clone())
            .await
            .expect_err("metadata size must reject the file before reading it");
        assert_eq!(error.code, "bad_request");
        std::fs::remove_file(path).expect("remove sparse fixture");
    }
}
