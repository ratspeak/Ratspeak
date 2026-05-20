//! Identity CRUD + display-name updates.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;

use crate::commands::shared::{
    active_rns_config_dir, emit_hub_interfaces, remove_stored_file_refs,
};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::{
    active_identity_id, sanitize_announced_display_name, sanitize_text, validate_hex,
};
use crate::state::AppState;

const IDENTITY_BACKUP_FORMAT: &str = "ratspeak.identity.v1";
const LXMF_APP_NAME: &str = "lxmf.delivery";
const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

#[derive(Debug, Deserialize, Serialize)]
struct IdentityBackupV1 {
    format: String,
    kind: String,
    private_key: String,
    identity_hash: String,
    lxmf_hash: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    nickname: String,
    exported_at: f64,
}

#[derive(Debug)]
struct ParsedIdentityImport {
    key_bytes: Vec<u8>,
    format: &'static str,
}

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn base32_value(ch: u8) -> Option<u8> {
    match ch {
        b'A'..=b'Z' => Some(ch - b'A'),
        b'a'..=b'z' => Some(ch - b'a'),
        b'2'..=b'7' => Some(ch - b'2' + 26),
        _ => None,
    }
}

fn base32_decode_text(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits: u8 = 0;
    let mut saw_padding = false;

    for ch in text.bytes() {
        if ch.is_ascii_whitespace() || ch == b'-' {
            continue;
        }
        if ch == b'=' {
            saw_padding = true;
            continue;
        }
        if saw_padding {
            return Err("Invalid base32 private key padding".into());
        }
        let value =
            base32_value(ch).ok_or_else(|| "Invalid base32 private key data".to_string())?;
        buffer = (buffer << 5) | value as u32;
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
            if bits > 0 {
                buffer &= (1 << bits) - 1;
            } else {
                buffer = 0;
            }
        }
    }

    Ok(out)
}

fn base32_encode_padded(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits: u8 = 0;

    for byte in bytes {
        buffer = (buffer << 8) | *byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32_ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
            if bits > 0 {
                buffer &= (1 << bits) - 1;
            } else {
                buffer = 0;
            }
        }
    }
    if bits > 0 {
        out.push(BASE32_ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    while !out.len().is_multiple_of(8) {
        out.push('=');
    }
    out
}

fn parse_private_identity_bytes(bytes: &[u8]) -> Result<ParsedIdentityImport, String> {
    if bytes.len() == 64 {
        return Ok(ParsedIdentityImport {
            key_bytes: bytes.to_vec(),
            format: "raw-private-key",
        });
    }

    let text = std::str::from_utf8(bytes)
        .map_err(|_| "Identity import must be a private identity backup or raw 64-byte key")?
        .trim();
    if text.is_empty() {
        return Err("Identity import is empty".into());
    }

    if let Ok(backup) = serde_json::from_str::<IdentityBackupV1>(text) {
        if backup.format != IDENTITY_BACKUP_FORMAT {
            return Err("Unsupported identity backup format".into());
        }
        if backup.kind != "private" {
            return Err("Public identity backups are not activatable identities".into());
        }
        let key_bytes = B64
            .decode(backup.private_key.trim())
            .map_err(|_| "Invalid identity backup key data")?;
        if key_bytes.len() != 64 {
            return Err("Identity backup key must be exactly 64 bytes".into());
        }
        let identity = Identity::from_private_key(&key_bytes)
            .map_err(|e| format!("Invalid identity backup key: {e}"))?;
        let hash_hex = hex::encode(identity.hash);
        if !backup.identity_hash.is_empty() && backup.identity_hash != hash_hex {
            return Err("Identity backup hash does not match private key".into());
        }
        return Ok(ParsedIdentityImport {
            key_bytes,
            format: IDENTITY_BACKUP_FORMAT,
        });
    }

    if let Ok(key_bytes) = hex::decode(text)
        && key_bytes.len() == 64
    {
        return Ok(ParsedIdentityImport {
            key_bytes,
            format: "hex-private-key",
        });
    }

    if let Ok(key_bytes) = B64.decode(text)
        && key_bytes.len() == 64
    {
        return Ok(ParsedIdentityImport {
            key_bytes,
            format: "base64-private-key",
        });
    }

    if let Ok(key_bytes) = base32_decode_text(text)
        && key_bytes.len() == 64
    {
        return Ok(ParsedIdentityImport {
            key_bytes,
            format: "base32-private-key",
        });
    }

    Err("Identity import must contain a private identity, not a public-only identity".into())
}

fn identity_preview_payload(key_bytes: &[u8], format: &str) -> Result<Value, String> {
    let identity =
        Identity::from_private_key(key_bytes).map_err(|e| format!("Invalid identity key: {e}"))?;
    let lxmf_dest = Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
    Ok(json!({
        "format": format,
        "kind": "private",
        "identity_hash": hex::encode(identity.hash),
        "lxmf_hash": hex::encode(lxmf_dest),
        "activatable": true,
    }))
}

#[tauri::command]
pub async fn api_identity(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let active = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "db task panicked");
            Default::default()
        });
    Ok(match active {
        Some(identity) => json!({
            "exists": true,
            "hash": identity.get("hash"),
            "lxmf_destination": identity.get("lxmf_hash"),
            "display_name": identity.get("display_name").and_then(|v| v.as_str()).unwrap_or(""),
            "nickname": identity.get("nickname").and_then(|v| v.as_str()).unwrap_or(""),
        }),
        None => json!({
            "exists": false,
            "hash": null,
            "lxmf_destination": null,
            "display_name": "",
            "nickname": "",
        }),
    })
}

#[tauri::command]
pub async fn api_list_identities(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identities = db::spawn_db(state.db.clone(), |p| db::get_all_identities(&p))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "list_identities db task panicked");
            Default::default()
        });
    Ok(json!(identities))
}

#[derive(Deserialize)]
pub struct CreateIdentityArgs {
    #[serde(default)]
    pub nickname: Option<String>,
}

#[tauri::command]
pub async fn api_create_identity(
    state: State<'_, Arc<AppState>>,
    args: CreateIdentityArgs,
) -> AppResult<Value> {
    let nickname = sanitize_announced_display_name(args.nickname.as_deref().unwrap_or(""))
        .map_err(AppError::bad_request)?;

    let st: Arc<AppState> = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        if let Ok(lxmf) = st.lxmf.lock() {
            if let Some(mgr) = lxmf.as_ref() {
                mgr.create_identity(&nickname, &st.db).ok()
            } else {
                None
            }
        } else {
            None
        }
    })
    .await
    .map_err(|_| AppError::internal("create_identity task panicked"))?;

    match result {
        Some((hash, lxmf_hash)) => Ok(json!({ "hash": hash, "lxmf_hash": lxmf_hash })),
        None => Err(AppError::lxmf_not_initialized("LXMF not initialized")),
    }
}

#[derive(Deserialize)]
pub struct ImportIdentityArgs {
    pub key: String,
    #[serde(default)]
    pub nickname: Option<String>,
}

#[tauri::command]
pub async fn api_import_identity(
    state: State<'_, Arc<AppState>>,
    args: ImportIdentityArgs,
) -> AppResult<Value> {
    let key_bytes =
        hex::decode(args.key.trim()).map_err(|_| AppError::bad_request("Invalid hex key data"))?;
    import_identity_shared(state, key_bytes, args.nickname).await
}

#[tauri::command]
pub async fn api_import_identity_base64(
    state: State<'_, Arc<AppState>>,
    args: ImportIdentityArgs,
) -> AppResult<Value> {
    let key_bytes = B64
        .decode(args.key.trim())
        .map_err(|_| AppError::bad_request("Invalid base64 key data"))?;
    import_identity_shared(state, key_bytes, args.nickname).await
}

async fn import_identity_shared(
    state: State<'_, Arc<AppState>>,
    key_bytes: Vec<u8>,
    nickname: Option<String>,
) -> AppResult<Value> {
    let nickname = sanitize_announced_display_name(nickname.as_deref().unwrap_or(""))
        .map_err(AppError::bad_request)?;
    let parsed = parse_private_identity_bytes(&key_bytes).map_err(AppError::bad_request)?;
    let format = parsed.format;
    let parsed_identity = Identity::from_private_key(&parsed.key_bytes)
        .map_err(|e| AppError::bad_request(format!("Invalid identity key: {e}")))?;
    let import_hash = hex::encode(parsed_identity.hash);
    let hash_for_duplicate_check = import_hash.clone();
    let already_exists = db::spawn_db(state.db.clone(), move |p| {
        db::get_identity(&p, &hash_for_duplicate_check).is_some()
    })
    .await
    .map_err(|_| AppError::internal("identity duplicate check db task panicked"))?;
    if already_exists {
        return Err(AppError::conflict("Identity already exists"));
    }
    let st: Arc<AppState> = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        if let Ok(lxmf) = st.lxmf.lock() {
            if let Some(mgr) = lxmf.as_ref() {
                mgr.import_identity(&parsed.key_bytes, &nickname, &st.db)
                    .map_err(|e| e.to_string())
            } else {
                crate::lxmf::LxmfManager::import_identity_to_data_dir(
                    &st.config.data_dir,
                    &parsed.key_bytes,
                    &nickname,
                    &st.db,
                )
                .map_err(|e| e.to_string())
            }
        } else {
            Err("Lock error".into())
        }
    })
    .await
    .map_err(|_| AppError::internal("import_identity task panicked"))?;

    match result {
        Ok((hash, lxmf_hash)) => {
            let active_missing = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
                .await
                .map_err(|_| AppError::internal("active identity db task panicked"))?
                .is_none();
            if active_missing {
                let hash_for_active = hash.clone();
                db::spawn_db(state.db.clone(), move |p| {
                    db::set_active_identity(&p, &hash_for_active)
                })
                .await
                .map_err(|_| AppError::internal("activate imported identity task panicked"))?
                .map_err(|e| AppError::internal(format!("Failed to activate import: {e}")))?;
            }
            Ok(json!({
                "hash": hash,
                "lxmf_hash": lxmf_hash,
                "format": format,
                "activated": active_missing,
            }))
        }
        Err(e) => Err(AppError::bad_request(e)),
    }
}

#[tauri::command]
pub async fn api_preview_identity_import_base64(args: ImportIdentityArgs) -> AppResult<Value> {
    let bytes = B64
        .decode(args.key.trim())
        .map_err(|_| AppError::bad_request("Invalid base64 identity data"))?;
    let parsed = parse_private_identity_bytes(&bytes).map_err(AppError::bad_request)?;
    identity_preview_payload(&parsed.key_bytes, parsed.format).map_err(AppError::bad_request)
}

#[tauri::command]
pub async fn api_activate_identity(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
) -> AppResult<Value> {
    if !validate_hex(&hash_hex, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    switch_identity_session(Arc::clone(&state), hash_hex).await
}

/// Existence check; the actual bytes ship via `api_export_identity_base64`.
#[tauri::command]
pub async fn api_export_identity(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
) -> AppResult<Value> {
    if !validate_hex(&hash_hex, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    let exists = state
        .lxmf
        .lock()
        .ok()
        .and_then(|l| l.as_ref().and_then(|mgr| mgr.export_identity(&hash_hex)))
        .is_some();
    if exists {
        Ok(json!({ "message": "Use export-base64 endpoint for key data" }))
    } else {
        Err(AppError::not_found("Identity file not found"))
    }
}

#[tauri::command]
pub async fn api_export_identity_base64(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
) -> AppResult<Value> {
    if !validate_hex(&hash_hex, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    let key_bytes = state
        .lxmf
        .lock()
        .ok()
        .and_then(|l| l.as_ref().and_then(|mgr| mgr.export_identity(&hash_hex)));
    match key_bytes {
        Some(bytes) => Ok(json!({ "key": B64.encode(&bytes) })),
        None => Err(AppError::not_found("Identity file not found")),
    }
}

pub(crate) fn export_identity_key_bytes(state: &AppState, hash_hex: &str) -> AppResult<Vec<u8>> {
    if !validate_hex(hash_hex, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    let key_bytes = state
        .lxmf
        .lock()
        .ok()
        .and_then(|l| l.as_ref().and_then(|mgr| mgr.export_identity(hash_hex)))
        .ok_or_else(|| AppError::not_found("Identity file not found"))?;

    let identity =
        Identity::from_private_key(&key_bytes).map_err(|_| AppError::bad_request("Invalid key"))?;
    let identity_hash = hex::encode(identity.hash);
    if identity_hash != hash_hex {
        return Err(AppError::conflict("Identity file hash mismatch"));
    }
    Ok(key_bytes)
}

fn identity_export_hashes(key_bytes: &[u8]) -> AppResult<(String, String)> {
    let identity =
        Identity::from_private_key(key_bytes).map_err(|_| AppError::bad_request("Invalid key"))?;
    let identity_hash = hex::encode(identity.hash);
    let lxmf_dest = Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
    Ok((identity_hash, hex::encode(lxmf_dest)))
}

#[tauri::command]
pub async fn api_export_identity_reticulum_base64(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
) -> AppResult<Value> {
    let key_bytes = export_identity_key_bytes(&state, &hash_hex)?;
    let (identity_hash, lxmf_hash) = identity_export_hashes(&key_bytes)?;
    Ok(json!({
        "data_base64": B64.encode(&key_bytes),
        "file_name": format!("{}-reticulum-identity.identity", &identity_hash[..16]),
        "identity_hash": identity_hash,
        "lxmf_hash": lxmf_hash,
        "format": "reticulum.raw-private-key",
    }))
}

#[tauri::command]
pub async fn api_export_identity_reticulum_base32(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
) -> AppResult<Value> {
    let key_bytes = export_identity_key_bytes(&state, &hash_hex)?;
    let (identity_hash, lxmf_hash) = identity_export_hashes(&key_bytes)?;
    let text = base32_encode_padded(&key_bytes);
    Ok(json!({
        "data_base64": B64.encode(text.as_bytes()),
        "text": text,
        "file_name": format!("{}-reticulum-identity-key-base32.txt", &identity_hash[..16]),
        "identity_hash": identity_hash,
        "lxmf_hash": lxmf_hash,
        "format": "reticulum.base32-private-key",
    }))
}

#[tauri::command]
pub async fn api_export_identity_backup_base64(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
) -> AppResult<Value> {
    let key_bytes = export_identity_key_bytes(&state, &hash_hex)?;
    let (identity_hash, lxmf_hash) = identity_export_hashes(&key_bytes)?;

    let row_hash = hash_hex.clone();
    let identity_row = db::spawn_db(state.db.clone(), move |p| db::get_identity(&p, &row_hash))
        .await
        .map_err(|_| AppError::internal("identity metadata db task panicked"))?
        .unwrap_or_default();
    let display_name = identity_row
        .get("display_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let nickname = identity_row
        .get("nickname")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let backup = IdentityBackupV1 {
        format: IDENTITY_BACKUP_FORMAT.to_string(),
        kind: "private".to_string(),
        private_key: B64.encode(&key_bytes),
        identity_hash: identity_hash.clone(),
        lxmf_hash: lxmf_hash.clone(),
        display_name,
        nickname,
        exported_at: now_ts(),
    };
    let bytes = serde_json::to_vec_pretty(&backup)
        .map_err(|_| AppError::internal("failed to encode identity backup"))?;

    Ok(json!({
        "backup_base64": B64.encode(&bytes),
        "file_name": format!("{}-ratspeak-identity.rsi", &identity_hash[..16]),
        "identity_hash": identity_hash,
        "lxmf_hash": lxmf_hash,
        "format": IDENTITY_BACKUP_FORMAT,
    }))
}

#[derive(Deserialize)]
pub struct UpdateIdentityArgs {
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[tauri::command]
pub async fn api_update_identity(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
    args: UpdateIdentityArgs,
) -> AppResult<Value> {
    if !validate_hex(&hash_hex, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    let nickname = args
        .nickname
        .as_deref()
        .map(sanitize_announced_display_name)
        .transpose()
        .map_err(AppError::bad_request)?;
    let display_name = args
        .display_name
        .as_deref()
        .map(sanitize_announced_display_name)
        .transpose()
        .map_err(AppError::bad_request)?;
    let hash_for_db = hash_hex.clone();
    let nick_for_db = nickname.clone();
    let dn_for_db = display_name.clone();
    let result = db::spawn_db(state.db.clone(), move |p| {
        db::update_identity(
            &p,
            &hash_for_db,
            nick_for_db.as_deref(),
            dn_for_db.as_deref(),
        )
    })
    .await
    .map_err(|_| AppError::internal("update_identity db task panicked"))?;
    result.map_err(|e| {
        tracing::error!(error = %e, "update_identity failed");
        AppError::internal("failed to update identity")
    })?;
    Ok(json!(null))
}

#[tauri::command]
pub async fn api_delete_identity(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
    #[allow(non_snake_case)] cascade: Option<bool>,
) -> AppResult<Value> {
    if !validate_hex(&hash_hex, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    let active = active_identity_id(&state);
    if active == hash_hex {
        return Err(AppError::bad_request("Cannot delete active identity"));
    }
    let file_refs = if cascade.unwrap_or(false) {
        let hash_for_refs = hash_hex.clone();
        db::spawn_db(state.db.clone(), move |p| {
            db::get_identity_file_refs(&p, &hash_for_refs)
        })
        .await
        .map_err(|_| AppError::internal("identity file refs db task panicked"))?
    } else {
        Vec::new()
    };
    let hash_for_db = hash_hex.clone();
    let cascade = cascade.unwrap_or(false);
    let result = db::spawn_db(state.db.clone(), move |p| {
        db::delete_identity(&p, &hash_for_db, cascade)
    })
    .await
    .map_err(|_| AppError::internal("delete_identity db task panicked"))?;
    result.map_err(|e| AppError::internal(format!("Failed to delete: {e}")))?;
    if cascade && !file_refs.is_empty() {
        remove_stored_file_refs(&state.config.files_dir(), file_refs);
    }
    crate::lxmf::LxmfManager::purge_identity_profile(&state.config.data_root, &hash_hex, cascade)
        .map_err(|e| AppError::internal(format!("Failed to remove identity files: {e}")))?;
    Ok(json!(null))
}

#[derive(Deserialize)]
pub struct DisplayNameArgs {
    #[serde(default)]
    pub display_name: Option<String>,
}

async fn switch_identity_session(state: Arc<AppState>, hash_hex: String) -> AppResult<Value> {
    let _switch_guard = state.identity_switch_lock.lock().await;

    let hash_for_lookup = hash_hex.clone();
    let target = db::spawn_db(state.db.clone(), move |p| {
        db::get_identity(&p, &hash_for_lookup)
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "identity lookup task panicked");
        AppError::internal("db task panicked")
    })?
    .ok_or_else(|| AppError::not_found("Identity not found"))?;

    let id_file = state
        .config
        .data_dir
        .join("identities")
        .join(&hash_hex)
        .join("identity");
    if !id_file.exists() {
        return Err(AppError::not_found("Identity file not found"));
    }

    let previous_active = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
        .await
        .map_err(|_| AppError::internal("db task panicked"))?
        .and_then(|identity| {
            identity
                .get("hash")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    let runtime_identity = {
        state
            .lxmf
            .lock()
            .ok()
            .and_then(|lxmf| lxmf.as_ref().map(|mgr| mgr.identity_hash.clone()))
    };
    if previous_active.as_deref() == Some(hash_hex.as_str())
        && runtime_identity.as_deref() == Some(hash_hex.as_str())
    {
        let payload = json!({
            "hash": hash_hex,
            "lxmf_hash": target.get("lxmf_hash").and_then(|v| v.as_str()).unwrap_or(""),
            "display_name": target.get("display_name").and_then(|v| v.as_str()).unwrap_or(""),
        });
        return Ok(payload);
    }

    let generation = state.bump_identity_session_generation();
    state.emit_to_all(
        "identity_switching",
        json!({
            "hash": hash_hex,
            "generation": generation,
        }),
    );

    crate::shutdown_rns_lxmf(&state).await;
    state.clear_identity_scoped_runtime_state();

    let hash_for_db = hash_hex.clone();
    let set_result = db::spawn_db(state.db.clone(), move |p| {
        db::set_active_identity(&p, &hash_for_db)
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "activate db task panicked");
        AppError::internal("db task panicked")
    })?;
    if let Err(e) = set_result {
        if let Some(old_hash) = previous_active {
            let _ = db::spawn_db(state.db.clone(), move |p| {
                db::set_active_identity(&p, &old_hash)
            })
            .await;
        }
        return Err(AppError::internal(format!("Failed to activate: {e}")));
    }

    if let Ok(mut sig) = state.session_shutdown.write() {
        *sig = rns_runtime::lifecycle::ShutdownSignal::new();
    }
    state.set_startup_stage("checking");
    crate::init_rns_lxmf(Arc::clone(&state), state.config.data_root.clone()).await;

    let (loaded_identity, loaded_lxmf, loaded_display) = {
        state
            .lxmf
            .lock()
            .ok()
            .and_then(|lxmf| {
                lxmf.as_ref().map(|mgr| {
                    (
                        mgr.identity_hash.clone(),
                        mgr.lxmf_hash.clone(),
                        mgr.display_name.clone(),
                    )
                })
            })
            .unwrap_or_default()
    };
    if loaded_identity != hash_hex {
        tracing::error!(
            requested = %hash_hex,
            loaded = %loaded_identity,
            generation,
            "identity session switch loaded the wrong identity"
        );
        return Err(AppError::internal(
            "Identity switch did not activate requested identity",
        ));
    }

    let payload = json!({
        "hash": hash_hex,
        "lxmf_hash": loaded_lxmf,
        "display_name": loaded_display,
        "generation": generation,
    });
    let ifaces = crate::rns_config::get_all_interfaces(&active_rns_config_dir(&state));
    emit_hub_interfaces(&state, ifaces);
    state.emit_to_all("identity_switched", payload.clone());
    state.request_poll_now();
    Ok(payload)
}

#[tauri::command]
pub async fn switch_identity(state: State<'_, Arc<AppState>>, hash: String) -> AppResult<Value> {
    let hash_hex = sanitize_text(&hash, 128);
    if !validate_hex(&hash_hex, 16, 128) {
        return Err(AppError::bad_request("Invalid identity hash"));
    }
    switch_identity_session(Arc::clone(&state), hash_hex).await
}

#[tauri::command]
pub async fn api_set_display_name(
    state: State<'_, Arc<AppState>>,
    args: DisplayNameArgs,
) -> AppResult<Value> {
    let display_name = sanitize_announced_display_name(args.display_name.as_deref().unwrap_or(""))
        .map_err(AppError::bad_request)?;
    if display_name.is_empty() {
        return Err(AppError::bad_request("display_name required"));
    }
    let identity_id = active_identity_id(&state);
    if identity_id.is_empty() {
        return Err(AppError::conflict("no active identity"));
    }

    // Prefer in-memory LXMF mgr; fall back to DB-only on startup race.
    let updated_in_memory = {
        let mut guard = state
            .lxmf
            .lock()
            .map_err(|_| AppError::internal("lxmf state lock poisoned"))?;
        match guard.as_mut() {
            Some(mgr) => {
                mgr.update_display_name(&display_name, &state.db, &identity_id)
                    .map_err(|e| {
                        tracing::error!(error = %e, "display_name: update_identity failed");
                        AppError::internal("failed to save display name")
                    })?;
                true
            }
            None => false,
        }
    };

    if !updated_in_memory {
        let id = identity_id.clone();
        let dn = display_name.clone();
        db::spawn_db(state.db.clone(), move |p| {
            db::update_identity(&p, &id, None, Some(&dn))
        })
        .await
        .map_err(|_| AppError::internal("failed to save display name"))?
        .map_err(|e| {
            tracing::error!(error = %e, "display_name: update_identity failed (no lxmf)");
            AppError::internal("failed to save display name")
        })?;
    }

    if updated_in_memory {
        crate::send_announce_from_state(&state).await;
    }

    Ok(json!({ "display_name": display_name }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_key_bytes() -> Vec<u8> {
        let identity = Identity::new();
        identity.get_private_key().unwrap().to_vec()
    }

    #[test]
    fn parse_private_identity_accepts_raw_key() {
        let key = private_key_bytes();
        let parsed = parse_private_identity_bytes(&key).unwrap();
        assert_eq!(parsed.format, "raw-private-key");
        assert_eq!(parsed.key_bytes, key);
    }

    #[test]
    fn parse_private_identity_accepts_backup_envelope() {
        let key = private_key_bytes();
        let preview = identity_preview_payload(&key, "test").unwrap();
        let backup = IdentityBackupV1 {
            format: IDENTITY_BACKUP_FORMAT.to_string(),
            kind: "private".to_string(),
            private_key: B64.encode(&key),
            identity_hash: preview
                .get("identity_hash")
                .and_then(|v| v.as_str())
                .unwrap()
                .to_string(),
            lxmf_hash: preview
                .get("lxmf_hash")
                .and_then(|v| v.as_str())
                .unwrap()
                .to_string(),
            display_name: "Sir".to_string(),
            nickname: "Sir".to_string(),
            exported_at: 1.0,
        };
        let encoded = serde_json::to_vec(&backup).unwrap();

        let parsed = parse_private_identity_bytes(&encoded).unwrap();
        assert_eq!(parsed.format, IDENTITY_BACKUP_FORMAT);
        assert_eq!(parsed.key_bytes, key);
    }

    #[test]
    fn parse_private_identity_accepts_reticulum_base32_text() {
        let key = private_key_bytes();
        let encoded = base32_encode_padded(&key);

        let parsed = parse_private_identity_bytes(encoded.as_bytes()).unwrap();
        assert_eq!(parsed.format, "base32-private-key");
        assert_eq!(parsed.key_bytes, key);
    }

    #[test]
    fn base32_identity_text_roundtrips_with_spacing() {
        let key = private_key_bytes();
        let encoded = base32_encode_padded(&key).to_lowercase();
        let wrapped = encoded
            .as_bytes()
            .chunks(8)
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect::<Vec<_>>()
            .join(" ");

        let parsed = parse_private_identity_bytes(wrapped.as_bytes()).unwrap();
        assert_eq!(parsed.format, "base32-private-key");
        assert_eq!(parsed.key_bytes, key);
    }

    #[test]
    fn parse_private_identity_rejects_public_backup() {
        let key = private_key_bytes();
        let backup = IdentityBackupV1 {
            format: IDENTITY_BACKUP_FORMAT.to_string(),
            kind: "public".to_string(),
            private_key: B64.encode(&key),
            identity_hash: String::new(),
            lxmf_hash: String::new(),
            display_name: String::new(),
            nickname: String::new(),
            exported_at: 1.0,
        };
        let encoded = serde_json::to_vec(&backup).unwrap();

        let err = parse_private_identity_bytes(&encoded).unwrap_err();
        assert!(err.contains("Public identity backups are not activatable"));
    }

    #[test]
    fn parse_private_identity_rejects_hash_mismatch() {
        let key = private_key_bytes();
        let backup = IdentityBackupV1 {
            format: IDENTITY_BACKUP_FORMAT.to_string(),
            kind: "private".to_string(),
            private_key: B64.encode(&key),
            identity_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            lxmf_hash: String::new(),
            display_name: String::new(),
            nickname: String::new(),
            exported_at: 1.0,
        };
        let encoded = serde_json::to_vec(&backup).unwrap();

        let err = parse_private_identity_bytes(&encoded).unwrap_err();
        assert!(err.contains("hash does not match"));
    }
}
