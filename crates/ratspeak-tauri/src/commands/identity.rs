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
    active_identity_id, sanitize_announced_display_name, sanitize_announced_status, sanitize_text,
    validate_hex,
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
    status: String,
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

fn identity_material_exists(id_dir: &std::path::Path) -> bool {
    id_dir.join("identity").exists()
        || id_dir.join("identity.enc").exists()
        || id_dir.join("identity.hwid").exists()
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
    let is_hardware = state
        .lxmf
        .lock()
        .ok()
        .and_then(|l| l.as_ref().map(|m| m.is_hardware))
        .unwrap_or(false);
    Ok(match active {
        Some(identity) => json!({
            "exists": true,
            "hash": identity.get("hash"),
            "lxmf_destination": identity.get("lxmf_hash"),
            "display_name": identity.get("display_name").and_then(|v| v.as_str()).unwrap_or(""),
            "status": identity.get("status").and_then(|v| v.as_str()).unwrap_or(""),
            "nickname": identity.get("nickname").and_then(|v| v.as_str()).unwrap_or(""),
            "is_hardware": is_hardware,
        }),
        None => json!({
            "exists": false,
            "hash": null,
            "lxmf_destination": null,
            "display_name": "",
            "status": "",
            "nickname": "",
            "is_hardware": false,
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
    // Tag each row hardware-backed if its on-disk artifact is a `.hwid`, and
    // expose its PIV serial so the UI can badge it / detect an absent key.
    let dir = state.config.data_dir.join("identities");
    let mut value = json!(identities);
    if let Some(rows) = value.as_array_mut() {
        for row in rows.iter_mut() {
            let id_dir = row
                .get("hash")
                .and_then(|v| v.as_str())
                .map(|h| dir.join(h));
            let hwid_path = id_dir.as_ref().map(|d| d.join("identity.hwid"));
            let is_hw = hwid_path.as_ref().map(|p| p.exists()).unwrap_or(false);
            let serial = is_hw
                .then(|| hwid_path.and_then(|p| read_hwid_serial(&p)))
                .flatten();
            // Software identity sealed with a passcode (at-rest encrypted)?
            let passcode_protected = id_dir
                .as_ref()
                .map(|d| d.join("identity.enc").exists())
                .unwrap_or(false);
            // Does it have a recovery phrase we can re-display (sidecar or sealed)?
            let has_mnemonic = id_dir
                .as_ref()
                .map(|d| ratspeak_runtime::vault::has_stored_mnemonic(d))
                .unwrap_or(false);
            if let Some(obj) = row.as_object_mut() {
                obj.insert("is_hardware".to_string(), json!(is_hw));
                obj.insert("passcode_protected".to_string(), json!(passcode_protected));
                obj.insert("has_mnemonic".to_string(), json!(has_mnemonic));
                if let Some(serial) = serial {
                    obj.insert("hw_serial".to_string(), json!(serial));
                }
            }
        }
    }
    Ok(value)
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

    // New identities are recoverable: derived from a fresh BIP-39 mnemonic, which
    // we return once for the user to back up. Reuses the import path for the
    // duplicate-check, on-disk write, and activation.
    let identities_dir = state.config.data_dir.join("identities");
    let (mnemonic, key) =
        ratspeak_runtime::generate_recoverable_key().map_err(AppError::internal)?;
    let mut resp = import_identity_shared(state, key.to_vec(), Some(nickname)).await?;
    // Persist the phrase (software identity) so it can be re-displayed later.
    if let Some(hash) = resp.get("hash").and_then(|v| v.as_str()) {
        if let Err(e) =
            ratspeak_runtime::vault::store_plaintext_seed(&identities_dir.join(hash), &mnemonic)
        {
            tracing::warn!(error = %e, "could not store recovery-phrase sidecar");
        }
    }
    if let Some(obj) = resp.as_object_mut() {
        obj.insert("mnemonic".to_string(), json!(mnemonic));
    }
    Ok(resp)
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

#[cfg(feature = "seed")]
#[derive(Deserialize)]
pub struct RestoreSeedArgs {
    pub phrase: String,
    #[serde(default)]
    pub nickname: Option<String>,
}

/// Restore a *recoverable* identity's 24-word phrase as a SOFTWARE identity (no
/// hardware). Same derivation as the recoverable YubiKey scheme, so the restored
/// Reticulum identity/address matches. Cross-platform (the one hardware-related
/// path that works on mobile).
#[cfg(feature = "seed")]
#[tauri::command]
pub async fn restore_seed_identity(
    state: State<'_, Arc<AppState>>,
    args: RestoreSeedArgs,
) -> AppResult<Value> {
    let phrase = args.phrase.trim().to_string();
    let identities_dir = state.config.data_dir.join("identities");
    let key = ratspeak_runtime::derive_identity_key_from_phrase(&phrase)
        .map_err(AppError::bad_request)?;
    let resp = import_identity_shared(state, key.to_vec(), args.nickname).await?;
    // Persist the phrase so a restored identity can re-display it later too.
    if let Some(hash) = resp.get("hash").and_then(|v| v.as_str()) {
        if let Err(e) =
            ratspeak_runtime::vault::store_plaintext_seed(&identities_dir.join(hash), &phrase)
        {
            tracing::warn!(error = %e, "could not store recovery-phrase sidecar");
        }
    }
    Ok(resp)
}

#[derive(Deserialize)]
pub struct SetPasscodeArgs {
    pub hash: String,
    pub passcode: String,
    /// Required only when changing an existing passcode.
    #[serde(default)]
    pub current: Option<String>,
}

/// Add or change a passcode on a software identity (at-rest encryption). The
/// identity is sealed on disk; the next launch will prompt for the passcode.
#[tauri::command]
pub async fn set_identity_passcode(
    state: State<'_, Arc<AppState>>,
    args: SetPasscodeArgs,
) -> AppResult<Value> {
    if !validate_hex(&args.hash, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    if args.passcode.len() < 6 || args.passcode.len() > 128 {
        return Err(AppError::bad_request(
            "Passcode must be at least 6 characters",
        ));
    }
    let id_dir = state.config.data_dir.join("identities").join(&args.hash);
    let (passcode, current) = (args.passcode, args.current);
    tokio::task::spawn_blocking(move || {
        ratspeak_runtime::vault::protect_identity(&id_dir, &passcode, current.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| AppError::internal("set_passcode task panicked"))?
    .map_err(AppError::bad_request)?;
    Ok(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct RemovePasscodeArgs {
    pub hash: String,
    pub passcode: String,
}

/// Remove a passcode (decrypt the identity back to a plaintext key file).
#[tauri::command]
pub async fn remove_identity_passcode(
    state: State<'_, Arc<AppState>>,
    args: RemovePasscodeArgs,
) -> AppResult<Value> {
    if !validate_hex(&args.hash, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    let id_dir = state.config.data_dir.join("identities").join(&args.hash);
    let passcode = args.passcode;
    tokio::task::spawn_blocking(move || {
        ratspeak_runtime::vault::unprotect_identity(&id_dir, &passcode).map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| AppError::internal("remove_passcode task panicked"))?
    .map_err(AppError::bad_request)?;
    Ok(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct RevealMnemonicArgs {
    pub hash: String,
    /// Required only when the identity is passcode-protected (phrase in the vault).
    #[serde(default)]
    pub passcode: Option<String>,
}

/// Re-display a software identity's 24-word recovery phrase. Reads the plaintext
/// sidecar, or decrypts it from the vault (Argon2 → spawn_blocking) when the
/// identity is passcode-protected. Hardware identities have no stored phrase.
#[tauri::command]
pub async fn reveal_identity_mnemonic(
    state: State<'_, Arc<AppState>>,
    args: RevealMnemonicArgs,
) -> AppResult<Value> {
    if !validate_hex(&args.hash, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    let id_dir = state.config.data_dir.join("identities").join(&args.hash);
    let passcode = args.passcode;
    let phrase = tokio::task::spawn_blocking(move || {
        ratspeak_runtime::vault::reveal_mnemonic(&id_dir, passcode.as_deref())
            .map(|z| z.as_str().to_string())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| AppError::internal("reveal_mnemonic task panicked"))?
    .map_err(AppError::bad_request)?;
    Ok(json!({ "mnemonic": phrase }))
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
    if is_hardware_identity(&state, &hash_hex) {
        return Err(AppError::bad_request(
            "Hardware-backed identity: the private key lives on the token and cannot be exported",
        ));
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

/// Pull the PIV `serial` out of a `.hwid` (TOML) without a ratkey dependency,
/// so the always-compiled list path works in non-`hardware` builds too.
fn read_hwid_serial(path: &std::path::Path) -> Option<u64> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.trim().strip_prefix("serial") {
            let rest = rest.trim_start_matches([' ', '=']).trim();
            if let Ok(n) = rest.parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

/// A hardware-backed identity stores a `.hwid` instead of a private-key file.
fn is_hardware_identity(state: &AppState, hash_hex: &str) -> bool {
    state
        .config
        .data_dir
        .join("identities")
        .join(hash_hex)
        .join("identity.hwid")
        .exists()
}

pub(crate) fn export_identity_key_bytes(state: &AppState, hash_hex: &str) -> AppResult<Vec<u8>> {
    if !validate_hex(hash_hex, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    if is_hardware_identity(state, hash_hex) {
        return Err(AppError::bad_request(
            "Hardware-backed identity: the private key lives on the token and cannot be exported",
        ));
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
    let status = identity_row
        .get("status")
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
        status,
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

    let id_dir = state.config.data_dir.join("identities").join(&hash_hex);
    // Hardware identities store `.hwid`; passcode-protected software identities
    // store `identity.enc` until unlocked.
    if !identity_material_exists(&id_dir) {
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
            "status": target.get("status").and_then(|v| v.as_str()).unwrap_or(""),
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

    // Switching to a hardware identity comes up locked (awaiting PIN) — a valid
    // intermediate state, not a failed switch. Keep it active and let the unlock
    // prompt (driven by the hardware_locked event) take over; do not roll back.
    if state.hw_locked_hash().as_deref() == Some(hash_hex.as_str()) {
        state.emit_to_all(
            "identity_switched",
            json!({ "hash": hash_hex, "locked": true }),
        );
        return Ok(json!({ "hash": hash_hex, "locked": true }));
    }

    let (loaded_identity, loaded_lxmf, loaded_display, loaded_status) = {
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
                        mgr.status.clone(),
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
            "identity switch failed to load target; rolling back"
        );
        // Target failed to load (e.g. a hardware key that was re-provisioned or
        // unplugged). Restore the previous identity + its runtime so the session
        // is not left with a dead active row and no LXMF.
        crate::shutdown_rns_lxmf(&state).await;
        state.clear_identity_scoped_runtime_state();
        if let Some(old_hash) = previous_active.clone() {
            let old_for_db = old_hash.clone();
            let _ = db::spawn_db(state.db.clone(), move |p| {
                db::set_active_identity(&p, &old_for_db)
            })
            .await;
            if let Ok(mut sig) = state.session_shutdown.write() {
                *sig = rns_runtime::lifecycle::ShutdownSignal::new();
            }
            state.set_startup_stage("checking");
            crate::init_rns_lxmf(Arc::clone(&state), state.config.data_root.clone()).await;
        } else {
            state.set_startup_stage("ready");
        }
        return Err(AppError::bad_request(
            "Couldn't load this identity's key. If it's a hardware (YubiKey) identity, \
             the key may be unplugged or was re-provisioned.",
        ));
    }

    let payload = json!({
        "hash": hash_hex,
        "lxmf_hash": loaded_lxmf,
        "display_name": loaded_display,
        "status": loaded_status,
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

#[tauri::command]
pub async fn set_identity_status(
    state: State<'_, Arc<AppState>>,
    status: Option<String>,
) -> AppResult<Value> {
    let status = sanitize_announced_status(status.as_deref().unwrap_or(""))
        .map_err(AppError::bad_request)?;
    let identity_id = active_identity_id(&state);
    if identity_id.is_empty() {
        return Err(AppError::conflict("no active identity"));
    }

    let mut identity_payload: Option<Value> = None;
    let updated_in_memory = {
        let mut guard = state
            .lxmf
            .lock()
            .map_err(|_| AppError::internal("lxmf state lock poisoned"))?;
        match guard.as_mut() {
            Some(mgr) => {
                mgr.update_status(&status, &state.db, &identity_id)
                    .map_err(|e| {
                        tracing::error!(error = %e, "status: update_identity_status failed");
                        AppError::internal("failed to save status")
                    })?;
                identity_payload = Some(json!({
                    "hash": mgr.lxmf_hash.clone(),
                    "identity_hash": mgr.identity_hash.clone(),
                    "display_name": mgr.display_name.clone(),
                    "status": status.clone(),
                }));
                true
            }
            None => false,
        }
    };

    if !updated_in_memory {
        let id = identity_id.clone();
        let saved_status = status.clone();
        db::spawn_db(state.db.clone(), move |p| {
            db::update_identity_status(&p, &id, &saved_status)
        })
        .await
        .map_err(|_| AppError::internal("failed to save status"))?
        .map_err(|e| {
            tracing::error!(error = %e, "status: update_identity_status failed (no lxmf)");
            AppError::internal("failed to save status")
        })?;
    }

    if let Some(payload) = identity_payload {
        state.emit_to_all("lxmf_identity", payload);
        crate::send_announce_from_state(&state).await;
    }

    Ok(json!({ "status": status }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_IDENTITY_COMMAND_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn private_key_bytes() -> Vec<u8> {
        let identity = Identity::new();
        identity.get_private_key().unwrap().to_vec()
    }

    fn temp_identity_dir(tag: &str) -> std::path::PathBuf {
        let n = TEMP_IDENTITY_COMMAND_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ratspeak-identity-command-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn identity_material_exists_accepts_encrypted_identity() {
        let dir = temp_identity_dir("enc");
        std::fs::write(dir.join("identity.enc"), b"{}").unwrap();
        assert!(identity_material_exists(&dir));
        std::fs::remove_dir_all(&dir).ok();
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
            status: "Away".to_string(),
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
            status: String::new(),
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
            status: String::new(),
            nickname: String::new(),
            exported_at: 1.0,
        };
        let encoded = serde_json::to_vec(&backup).unwrap();

        let err = parse_private_identity_bytes(&encoded).unwrap_err();
        assert!(err.contains("hash does not match"));
    }
}
