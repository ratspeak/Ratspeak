//! Hardware (YubiKey/Nitrokey PIV) identity commands — thin wrappers over
//! `ratspeak_runtime::hardware`. Card I/O is blocking, so each runs on a
//! blocking task. Only compiled with the `hardware` feature.

use std::sync::Arc;

use serde_json::Value;
use tauri::State;

use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::{sanitize_announced_display_name, validate_hex};
use crate::state::AppState;

fn to_value<T: serde::Serialize>(v: T) -> AppResult<Value> {
    serde_json::to_value(v).map_err(|e| AppError::internal(e.to_string()))
}

fn check_piv_code(label: &str, value: &str) -> AppResult<()> {
    if value.len() < 6 || value.len() > 8 {
        return Err(AppError::bad_request(format!(
            "{label} must be 6-8 characters"
        )));
    }
    Ok(())
}

fn check_pin(pin: &str) -> AppResult<()> {
    check_piv_code("PIN", pin)
}

fn clean_nickname(nickname: &str) -> AppResult<String> {
    sanitize_announced_display_name(nickname).map_err(AppError::bad_request)
}

#[tauri::command]
pub async fn hw_detect(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let data_dir = state.config.data_dir.clone();
    let d = tokio::task::spawn_blocking(move || ratspeak_runtime::hardware::detect(&data_dir))
        .await
        .map_err(|_| AppError::internal("hw detect task panicked"))?;
    to_value(d)
}

#[tauri::command]
pub async fn hw_provision_recoverable(
    state: State<'_, Arc<AppState>>,
    pin: String,
    current_pin: Option<String>,
    nickname: String,
    force: bool,
) -> AppResult<Value> {
    check_pin(&pin)?;
    if let Some(current_pin) = current_pin.as_deref() {
        check_pin(current_pin)?;
    }
    let nickname = clean_nickname(&nickname)?;
    let data_dir = state.config.data_dir.clone();
    let db = state.db.clone();
    let res = tokio::task::spawn_blocking(move || {
        ratspeak_runtime::hardware::provision_recoverable(
            &data_dir,
            &db,
            &pin,
            current_pin.as_deref(),
            &nickname,
            force,
        )
    })
    .await
    .map_err(|_| AppError::internal("provision task panicked"))?
    .map_err(AppError::bad_request)?;
    to_value(res)
}

#[tauri::command]
pub async fn hw_provision_hardware_only(
    state: State<'_, Arc<AppState>>,
    pin: String,
    current_pin: Option<String>,
    nickname: String,
    force: bool,
) -> AppResult<Value> {
    check_pin(&pin)?;
    if let Some(current_pin) = current_pin.as_deref() {
        check_pin(current_pin)?;
    }
    let nickname = clean_nickname(&nickname)?;
    let data_dir = state.config.data_dir.clone();
    let db = state.db.clone();
    let res = tokio::task::spawn_blocking(move || {
        ratspeak_runtime::hardware::provision_hardware_only(
            &data_dir,
            &db,
            &pin,
            current_pin.as_deref(),
            &nickname,
            force,
        )
    })
    .await
    .map_err(|_| AppError::internal("provision task panicked"))?
    .map_err(AppError::bad_request)?;
    to_value(res)
}

#[tauri::command]
pub async fn hw_import_existing(
    state: State<'_, Arc<AppState>>,
    nickname: String,
) -> AppResult<Value> {
    let nickname = clean_nickname(&nickname)?;
    let data_dir = state.config.data_dir.clone();
    let db = state.db.clone();
    let res = tokio::task::spawn_blocking(move || {
        ratspeak_runtime::hardware::import_existing(&data_dir, &db, &nickname)
    })
    .await
    .map_err(|_| AppError::internal("import task panicked"))?
    .map_err(AppError::bad_request)?;
    to_value(res)
}

#[tauri::command]
pub async fn hw_restore(
    state: State<'_, Arc<AppState>>,
    phrase: String,
    pin: String,
    current_pin: Option<String>,
    nickname: String,
    force: bool,
) -> AppResult<Value> {
    check_pin(&pin)?;
    if let Some(current_pin) = current_pin.as_deref() {
        check_pin(current_pin)?;
    }
    let nickname = clean_nickname(&nickname)?;
    let data_dir = state.config.data_dir.clone();
    let db = state.db.clone();
    let res = tokio::task::spawn_blocking(move || {
        ratspeak_runtime::hardware::restore(
            &data_dir,
            &db,
            &phrase,
            &pin,
            current_pin.as_deref(),
            &nickname,
            force,
        )
    })
    .await
    .map_err(|_| AppError::internal("restore task panicked"))?
    .map_err(AppError::bad_request)?;
    to_value(res)
}

/// Stage the PIN the user already entered during first-run hardware setup so
/// the setup restart can load the just-provisioned identity once. The runtime
/// consumes and clears this value via `take_pending_hw_pin`.
#[tauri::command]
pub async fn hw_stage_unlock(state: State<'_, Arc<AppState>>, pin: String) -> AppResult<Value> {
    check_pin(&pin)?;
    state.set_pending_hw_pin(Some(pin));
    to_value(serde_json::json!({ "ok": true }))
}

#[tauri::command]
pub async fn hw_reset_piv() -> AppResult<Value> {
    tokio::task::spawn_blocking(ratspeak_runtime::hardware::reset_piv_application)
        .await
        .map_err(|_| AppError::internal("reset PIV task panicked"))?
        .map_err(AppError::bad_request)?;
    to_value(serde_json::json!({ "ok": true }))
}

#[tauri::command]
pub async fn hw_change_pin(
    state: State<'_, Arc<AppState>>,
    hash: String,
    current_pin: String,
    new_pin: String,
) -> AppResult<Value> {
    if !validate_hex(&hash, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    check_piv_code("Current PIN", &current_pin)?;
    check_piv_code("New PIN", &new_pin)?;
    let data_dir = state.config.data_dir.clone();
    tokio::task::spawn_blocking(move || {
        ratspeak_runtime::hardware::change_pin(&data_dir, &hash, &current_pin, &new_pin)
    })
    .await
    .map_err(|_| AppError::internal("change PIN task panicked"))?
    .map_err(AppError::bad_request)?;
    to_value(serde_json::json!({ "ok": true }))
}

#[tauri::command]
pub async fn hw_remove(state: State<'_, Arc<AppState>>, hash: String) -> AppResult<Value> {
    if !validate_hex(&hash, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    let state = Arc::clone(&state);
    let was_locked = state.hw_locked_hash().as_deref() == Some(hash.as_str());
    let was_loaded = state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(|mgr| mgr.identity_hash.clone()))
        .as_deref()
        == Some(hash.as_str());
    if was_loaded {
        crate::shutdown_rns_lxmf(&state).await;
        state.clear_identity_scoped_runtime_state();
    }
    let data_dir = state.config.data_dir.clone();
    let db = state.db.clone();
    let hash_for_remove = hash.clone();
    tokio::task::spawn_blocking(move || {
        ratspeak_runtime::hardware::remove(&data_dir, &db, &hash_for_remove)
    })
    .await
    .map_err(|_| AppError::internal("remove task panicked"))?
    .map_err(AppError::bad_request)?;
    let remaining = db::spawn_db(state.db.clone(), |p| db::get_all_identities(&p).len())
        .await
        .map_err(|_| AppError::internal("identity count db task panicked"))?;
    if was_locked || was_loaded {
        state.set_hw_locked(None);
        state.set_hw_last_error(None);
        state.set_pending_hw_pin(None);
        if remaining == 0 {
            state.set_startup_stage("ready");
        }
    }
    to_value(serde_json::json!({
        "removed": true,
        "needs_setup": remaining == 0,
    }))
}

/// Unlock the active hardware identity with the user's PIN. Uniformly tears down
/// and re-initializes (full reload — the backend Arc is shared into the RNS link
/// manager), then reports success or a structured failure (wrong PIN with
/// remaining attempts, or PIN-blocked).
#[tauri::command]
pub async fn hw_unlock(state: State<'_, Arc<AppState>>, pin: String) -> AppResult<Value> {
    crate::commands::identity::unlock_protected_identity(Arc::clone(&state), pin).await
}

/// First-run hardware setup completion needs a stronger guarantee than
/// `hw_stage_unlock` + async restart: activate this exact hardware identity,
/// unlock it with the provided PIN, and only report success if that identity is
/// what the runtime loaded.
#[tauri::command]
pub async fn hw_activate_and_unlock(
    state: State<'_, Arc<AppState>>,
    hash: String,
    pin: String,
) -> AppResult<Value> {
    if !validate_hex(&hash, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    check_pin(&pin)?;
    let state = Arc::clone(&state);
    let _guard = state.identity_switch_lock.lock().await;

    let hash_for_lookup = hash.clone();
    let exists = db::spawn_db(state.db.clone(), move |p| {
        db::get_identity(&p, &hash_for_lookup).is_some()
    })
    .await
    .map_err(|_| AppError::internal("identity lookup db task panicked"))?;
    if !exists {
        return Err(AppError::not_found("Identity not found"));
    }

    let id_dir = state.config.data_dir.join("identities").join(&hash);
    if !id_dir.join("identity.hwid").exists() {
        return Err(AppError::bad_request(
            "Identity is not a hardware key identity",
        ));
    }

    let hash_for_active = hash.clone();
    db::spawn_db(state.db.clone(), move |p| {
        db::set_active_identity(&p, &hash_for_active)
    })
    .await
    .map_err(|_| AppError::internal("activate identity db task panicked"))?
    .map_err(|e| AppError::internal(format!("Failed to activate hardware identity: {e}")))?;

    crate::shutdown_rns_lxmf(&state).await;
    state.clear_identity_scoped_runtime_state();
    state.set_hw_last_error(None);
    state.set_hw_locked(None);
    state.set_pending_hw_pin(Some(pin));
    if let Ok(mut sig) = state.session_shutdown.write() {
        *sig = rns_runtime::lifecycle::ShutdownSignal::new();
    }
    state.set_startup_stage("checking");
    crate::init_rns_lxmf(Arc::clone(&state), state.config.data_root.clone()).await;
    crate::commands::ble::restore_ble_peer_if_requested(Arc::clone(&state)).await;

    let loaded_identity = state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(|mgr| mgr.identity_hash.clone()));
    if loaded_identity.as_deref() == Some(hash.as_str()) {
        state.set_hw_locked(None);
        state.set_hw_last_error(None);
        return to_value(serde_json::json!({
            "ok": true,
            "hash": hash,
        }));
    }

    let msg = state
        .take_hw_last_error()
        .unwrap_or_else(|| match loaded_identity {
            Some(other) => format!("Hardware unlock loaded a different identity ({other})."),
            None => "Could not unlock the hardware identity.".to_string(),
        });
    state.set_hw_locked(Some(hash.clone()));
    state.set_startup_stage("hw_locked");
    let locked = msg.contains("PIN locked");
    to_value(serde_json::json!({
        "ok": false,
        "hash": hash,
        "error": msg,
        "locked": locked,
        "remaining": parse_remaining(&msg),
    }))
}

/// Pull N from RatkeyError::PinFailed's "(N attempts remaining)" Display.
fn parse_remaining(msg: &str) -> Option<u8> {
    let idx = msg.find(" attempts remaining")?;
    msg[..idx]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .parse()
        .ok()
}
