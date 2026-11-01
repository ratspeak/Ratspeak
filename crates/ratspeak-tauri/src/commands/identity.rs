//! Identity CRUD + display-name updates.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ratspeak_runtime::Zeroizing;
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
use crate::state::{ActivityRequestFence, AppState, OwnedIdentitySwitchGuard, OwnedRoutineGuard};

use ratspeak_core::LXMF_DELIVERY_APP_NAME as LXMF_APP_NAME;
use ratspeak_runtime::activity::ActivityRecorderError;
use ratspeak_runtime::vault::{self, VaultJob};

const IDENTITY_BACKUP_FORMAT: &str = "ratspeak.identity.v2";
const LEGACY_IDENTITY_BACKUP_FORMAT: &str = "ratspeak.identity.v1";
const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

fn identity_activity_reset_error(_error: ActivityRecorderError) -> AppError {
    tracing::error!(
        reason = "activity_reset_unacknowledged",
        "identity lifecycle stopped before changing identity state"
    );
    AppError::service_unavailable("Identity change is temporarily unavailable")
}

/// Capture the fence before preparation, then acquire in identity-before-LXMF
/// order. A request queued across reset must fail before persistent mutation.
pub(super) async fn acquire_identity_mutation(
    state: &AppState,
    fence: ActivityRequestFence,
) -> AppResult<Arc<OwnedIdentitySwitchGuard>> {
    let lease = Arc::new(state.identity_switch_lock.lock_owned().await);
    if !state.is_current_activity_request_fence_after_identity_lock(fence) {
        return Err(AppError::conflict(
            "Identity changed while preparing this operation; retry",
        ));
    }
    Ok(lease)
}

/// As `acquire_identity_mutation`, for routine work on the active identity's
/// data: it waits out transitions and is refused if one ran since the request,
/// but invalidates no other request.
pub(super) async fn acquire_identity_routine(
    state: &AppState,
    fence: ActivityRequestFence,
) -> AppResult<OwnedRoutineGuard> {
    let lease = state.identity_switch_lock.lock_routine_owned().await;
    if !state.is_current_activity_origin_fence(fence) {
        return Err(AppError::conflict(
            "Identity changed while preparing this operation; retry",
        ));
    }
    Ok(lease)
}

/// The active identity, refused unless it is the one the caller shows.
fn callers_identity(state: &AppState, caller: Option<&str>) -> AppResult<String> {
    let active = active_identity_id(state);
    if active.is_empty() {
        return Err(AppError::conflict("no active identity"));
    }
    if caller.is_some_and(|caller| caller != active) {
        return Err(AppError::conflict("Identity changed; retry"));
    }
    Ok(active)
}

/// The actual blocking operation retains its lifecycle owner even if the
/// awaiting command is cancelled. The caller retains a separate clone through
/// post-operation publication and response construction.
pub(super) async fn run_identity_mutation<T, F>(
    lease: Arc<OwnedIdentitySwitchGuard>,
    operation: F,
) -> Result<T, tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let _identity_lifecycle = lease;
        operation()
    })
    .await
}

#[derive(Debug, Deserialize, Serialize)]
struct EncryptedIdentityBackupV2 {
    format: String,
    kind: String,
    vault: ratspeak_runtime::vault::EncryptedVault,
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

#[derive(Deserialize, Serialize)]
struct LegacyIdentityBackupV1 {
    format: String,
    kind: String,
    #[serde(
        serialize_with = "serialize_private_key",
        deserialize_with = "deserialize_private_key"
    )]
    private_key: Zeroizing<String>,
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

struct ParsedIdentityImport {
    key_bytes: Zeroizing<Vec<u8>>,
    format: &'static str,
    mnemonic: Option<Zeroizing<String>>,
}

fn serialize_private_key<S>(key: &Zeroizing<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(key.as_str())
}

fn deserialize_private_key<'de, D>(deserializer: D) -> Result<Zeroizing<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Zeroizing::new)
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

fn parse_private_identity_bytes_with_job(
    bytes: &[u8],
    passcode: Option<&str>,
    job: &VaultJob,
) -> Result<ParsedIdentityImport, String> {
    if bytes.len() > vault::MAX_IDENTITY_IMPORT_BYTES {
        return Err("Identity import exceeds the 1 MiB limit".into());
    }
    if let Some(passcode) = passcode {
        vault::validate_passcode(passcode).map_err(|e| e.to_string())?;
    }
    if bytes.len() == 64 {
        return Ok(ParsedIdentityImport {
            key_bytes: Zeroizing::new(bytes.to_vec()),
            format: "raw-private-key",
            mnemonic: None,
        });
    }

    let text = std::str::from_utf8(bytes)
        .map_err(|_| "Identity import must be a private identity backup or raw 64-byte key")?
        .trim();
    if text.is_empty() {
        return Err("Identity import is empty".into());
    }

    if text.starts_with('{') {
        let value = vault::parse_unique_json(text.as_bytes(), vault::MAX_IDENTITY_IMPORT_BYTES)
            .map_err(|e| e.to_string())?;
        let format = value.get("format").and_then(|v| v.as_str()).unwrap_or("");
        if format == IDENTITY_BACKUP_FORMAT {
            let backup = serde_json::from_value::<EncryptedIdentityBackupV2>(value)
                .map_err(|_| "Encrypted identity backup is invalid".to_string())?;
            if backup.kind != "private" {
                return Err("Public identity backups are not activatable identities".into());
            }
            let passcode = passcode
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .ok_or_else(|| "Backup PIN required".to_string())?;
            let unlocked = job
                .decrypt_identity(passcode, &backup.vault)
                .map_err(|e| match e {
                    vault::VaultError::Auth => {
                        "Incorrect backup PIN or corrupt identity backup".to_string()
                    }
                    _ => e.to_string(),
                })?;
            let mnemonic = unlocked.mnemonic().map(|m| Zeroizing::new(m.to_owned()));
            let key_bytes = Zeroizing::new(unlocked.key().to_vec());
            let identity = Identity::from_private_key(&key_bytes)
                .map_err(|e| format!("Invalid identity backup key: {e}"))?;
            let hash_hex = hex::encode(identity.hash);
            if !backup.identity_hash.is_empty() && backup.identity_hash != hash_hex {
                return Err("Identity backup hash does not match private key".into());
            }
            return Ok(ParsedIdentityImport {
                key_bytes,
                format: IDENTITY_BACKUP_FORMAT,
                mnemonic,
            });
        }

        if format == LEGACY_IDENTITY_BACKUP_FORMAT {
            let backup = serde_json::from_value::<LegacyIdentityBackupV1>(value)
                .map_err(|_| "Legacy identity backup is invalid".to_string())?;
            if backup.kind != "private" {
                return Err("Public identity backups are not activatable identities".into());
            }
            let key_bytes = Zeroizing::new(
                B64.decode(backup.private_key.trim())
                    .map_err(|_| "Invalid identity backup key data")?,
            );
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
                format: LEGACY_IDENTITY_BACKUP_FORMAT,
                mnemonic: None,
            });
        }

        return Err("Unsupported identity backup format".into());
    }

    if text.len() <= 128
        && let Some(key_bytes) = hex::decode(text)
            .ok()
            .filter(|key_bytes| key_bytes.len() == 64)
    {
        return Ok(ParsedIdentityImport {
            key_bytes: Zeroizing::new(key_bytes),
            format: "hex-private-key",
            mnemonic: None,
        });
    }

    if text.len() <= 88
        && let Some(key_bytes) = B64
            .decode(text)
            .ok()
            .filter(|key_bytes| key_bytes.len() == 64)
    {
        return Ok(ParsedIdentityImport {
            key_bytes: Zeroizing::new(key_bytes),
            format: "base64-private-key",
            mnemonic: None,
        });
    }

    if text.len() <= 4096 + 104
        && let Some(key_bytes) = base32_decode_text(text)
            .ok()
            .filter(|key_bytes| key_bytes.len() == 64)
    {
        return Ok(ParsedIdentityImport {
            key_bytes: Zeroizing::new(key_bytes),
            format: "base32-private-key",
            mnemonic: None,
        });
    }

    Err("Identity import must contain a private identity, not a public-only identity".into())
}

#[cfg(test)]
fn parse_private_identity_bytes(
    bytes: &[u8],
    passcode: Option<&str>,
) -> Result<ParsedIdentityImport, String> {
    let job = vault::try_acquire_job().map_err(|e| e.to_string())?;
    parse_private_identity_bytes_with_job(bytes, passcode, &job)
}

fn bounded_encoded_input(text: &str, encoded_limit: usize) -> AppResult<&str> {
    if text.len() > encoded_limit + vault::MAX_OUTER_WHITESPACE_BYTES {
        return Err(AppError::bad_request("Identity import exceeds size limit"));
    }
    let text = text.trim();
    if text.len() > encoded_limit {
        return Err(AppError::bad_request("Identity import exceeds size limit"));
    }
    Ok(text)
}
fn decode_identity_base64(text: &str) -> AppResult<Zeroizing<Vec<u8>>> {
    let bytes = Zeroizing::new(
        B64.decode(bounded_encoded_input(text, vault::MAX_BASE64_IMPORT_BYTES)?)
            .map_err(|_| AppError::bad_request("Invalid base64 identity data"))?,
    );
    if bytes.len() > vault::MAX_IDENTITY_IMPORT_BYTES {
        return Err(AppError::bad_request(
            "Identity import exceeds the 1 MiB limit",
        ));
    }
    Ok(bytes)
}
async fn parse_identity_import_async(
    bytes: Zeroizing<Vec<u8>>,
    passcode: Option<Zeroizing<String>>,
) -> AppResult<ParsedIdentityImport> {
    if bytes.len() > vault::MAX_IDENTITY_IMPORT_BYTES {
        return Err(AppError::bad_request(
            "Identity import exceeds the 1 MiB limit",
        ));
    }
    if let Some(p) = &passcode {
        vault::validate_passcode(p).map_err(|e| AppError::bad_request(e.to_string()))?;
    }
    let job = vault::try_acquire_job().map_err(|e| AppError::service_unavailable(e.to_string()))?;
    tokio::task::spawn_blocking(move || {
        parse_private_identity_bytes_with_job(&bytes, passcode.as_deref().map(String::as_str), &job)
    })
    .await
    .map_err(|_| AppError::internal("identity parsing task panicked"))?
    .map_err(AppError::bad_request)
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
        .unwrap_or_else(|_| {
            tracing::error!(reason = "task_panicked", "db task panicked");
            Ok(None)
        })
        .ok()
        .flatten();
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
        .unwrap_or_else(|_| {
            tracing::error!(reason = "task_panicked", "list_identities db task panicked");
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
    let request_fence = state.activity_request_fence();
    let nickname = sanitize_announced_display_name(args.nickname.as_deref().unwrap_or(""))
        .map_err(AppError::bad_request)?;

    // New identities are recoverable: derived from a fresh BIP-39 mnemonic, which
    // we return once for the user to back up. Reuses the import path for the
    // duplicate-check, on-disk write, and activation.
    let (mnemonic, key) =
        ratspeak_runtime::generate_recoverable_key().map_err(AppError::internal)?;
    let mut resp = import_identity_request(
        Arc::clone(&state),
        Zeroizing::new(key.to_vec()),
        Some(nickname),
        None,
        Some(mnemonic.clone()),
        request_fence,
    )
    .await?;
    if let Some(obj) = resp.as_object_mut() {
        obj.insert("mnemonic".to_string(), json!(mnemonic.as_str()));
    }
    Ok(resp)
}

#[derive(Deserialize)]
pub struct ImportIdentityArgs {
    pub key: String,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub passcode: Option<String>,
}

#[tauri::command]
pub async fn api_import_identity(
    state: State<'_, Arc<AppState>>,
    args: ImportIdentityArgs,
) -> AppResult<Value> {
    let key = Zeroizing::new(args.key);
    let key_bytes = Zeroizing::new(
        hex::decode(bounded_encoded_input(&key, vault::MAX_HEX_IMPORT_BYTES)?)
            .map_err(|_| AppError::bad_request("Invalid hex key data"))?,
    );
    import_identity_shared(state, key_bytes, args.nickname).await
}

#[tauri::command]
pub async fn api_import_identity_base64(
    state: State<'_, Arc<AppState>>,
    args: ImportIdentityArgs,
) -> AppResult<Value> {
    let key_bytes = decode_identity_base64(&Zeroizing::new(args.key))?;
    import_identity_shared_with_passcode(
        state,
        key_bytes,
        args.nickname,
        args.passcode.map(Zeroizing::new),
    )
    .await
}

#[cfg(feature = "seed")]
#[derive(Deserialize)]
pub struct RestoreSeedArgs {
    pub phrase: String,
    #[serde(default)]
    pub nickname: Option<String>,
}

/// Restore a *recoverable* identity's 12-word phrase as a SOFTWARE identity (no
/// hardware). Same derivation as the recoverable YubiKey scheme, so the restored
/// Reticulum identity/address matches. Cross-platform (the one hardware-related
/// path that works on mobile).
#[cfg(feature = "seed")]
#[tauri::command]
pub async fn restore_seed_identity(
    state: State<'_, Arc<AppState>>,
    args: RestoreSeedArgs,
) -> AppResult<Value> {
    let request_fence = state.activity_request_fence();
    let phrase = Zeroizing::new(args.phrase);
    let phrase = Zeroizing::new(phrase.trim().to_owned());
    let key = ratspeak_runtime::derive_identity_key_from_phrase(&phrase)
        .map_err(AppError::bad_request)?;
    import_identity_request(
        Arc::clone(&state),
        Zeroizing::new(key.to_vec()),
        args.nickname,
        None,
        Some(phrase),
        request_fence,
    )
    .await
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
    let (passcode, current) = (
        Zeroizing::new(args.passcode),
        args.current.map(Zeroizing::new),
    );
    if passcode.len() < 6 || passcode.len() > vault::MAX_PASSCODE_BYTES {
        return Err(AppError::bad_request("PIN must be at least 6 characters"));
    }
    let generation = state.current_identity_session_generation();
    let id_dir = state.config.data_dir.join("identities").join(&args.hash);
    let job = vault::try_acquire_job().map_err(|e| AppError::service_unavailable(e.to_string()))?;
    let (job, prepared) = tokio::task::spawn_blocking(move || {
        job.prepare_protection(&id_dir, &passcode, current.as_deref().map(String::as_str))
            .map(|prepared| (job, prepared))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| AppError::internal("set_passcode task panicked"))?
    .map_err(AppError::bad_request)?;
    let identity_lifecycle = state.identity_switch_lock.lock_owned().await;
    if generation != state.current_identity_session_generation() {
        return Err(AppError::conflict(
            "Identity changed while preparing protection; retry",
        ));
    }
    let worker_state = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let _identity_lifecycle = identity_lifecycle;
        job.commit_protection(prepared)?;
        // The open active identity locks once the set time has passed from now.
        if args.hash == active_identity_id(&worker_state) && worker_state.hw_locked_hash().is_none()
        {
            ratspeak_runtime::arm_auto_lock(&worker_state, &args.hash, "passcode", false);
        }
        Ok(())
    })
    .await
    .map_err(|_| AppError::internal("vault commit task panicked"))?
    .map_err(|e: vault::VaultError| AppError::bad_request(e.to_string()))?;
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
    let generation = state.current_identity_session_generation();
    let id_dir = state.config.data_dir.join("identities").join(&args.hash);
    let passcode = Zeroizing::new(args.passcode);
    let job = vault::try_acquire_job().map_err(|e| AppError::service_unavailable(e.to_string()))?;
    let (job, prepared) = tokio::task::spawn_blocking(move || {
        job.prepare_unprotection(&id_dir, &passcode)
            .map(|prepared| (job, prepared))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| AppError::internal("remove_passcode task panicked"))?
    .map_err(AppError::bad_request)?;
    let identity_lifecycle = state.identity_switch_lock.lock_owned().await;
    if generation != state.current_identity_session_generation() {
        return Err(AppError::conflict(
            "Identity changed while preparing unprotection; retry",
        ));
    }
    let worker_state = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let _identity_lifecycle = identity_lifecycle;
        job.commit_unprotection(prepared)?;
        // Without a passcode the active identity no longer locks.
        if args.hash == active_identity_id(&worker_state) {
            worker_state.disarm_auto_lock();
        }
        Ok(())
    })
    .await
    .map_err(|_| AppError::internal("vault commit task panicked"))?
    .map_err(|e: vault::VaultError| AppError::bad_request(e.to_string()))?;
    Ok(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct RevealMnemonicArgs {
    pub hash: String,
    /// Required when the identity is passcode-protected (phrase in the vault).
    #[serde(default)]
    pub passcode: Option<String>,
    /// Required when it is not.
    #[serde(default)]
    pub confirmed: Option<bool>,
}

/// Re-display the active software identity's 12-word recovery phrase: from
/// its vault with its passcode (Argon2 → spawn_blocking), or from the
/// plaintext sidecar after a confirmation. Hardware identities have no stored
/// phrase.
#[tauri::command]
pub async fn reveal_identity_mnemonic(
    state: State<'_, Arc<AppState>>,
    args: RevealMnemonicArgs,
) -> AppResult<Value> {
    if !validate_hex(&args.hash, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    if args.hash != active_identity_id(&state) {
        return Err(AppError::forbidden(
            "Only the active identity's recovery phrase can be shown",
        ));
    }
    let passcode = release_passcode(&state, &args.hash, args.passcode, args.confirmed)?;
    let id_dir = state.config.data_dir.join("identities").join(&args.hash);
    let job = vault::try_acquire_job().map_err(|e| AppError::service_unavailable(e.to_string()))?;
    let phrase = tokio::task::spawn_blocking(move || {
        job.reveal_mnemonic(&id_dir, passcode.as_deref().map(String::as_str))
    })
    .await
    .map_err(|_| AppError::internal("reveal_mnemonic task panicked"))?
    .map_err(key_release_error)?;
    Ok(json!({ "mnemonic": phrase.as_str() }))
}

fn protected_identity_kind(state: &AppState, hash: &str) -> Option<&'static str> {
    let id_dir = state.config.data_dir.join("identities").join(hash);
    if id_dir.join("identity.hwid").exists() {
        Some("hardware")
    } else if id_dir.join("identity.enc").exists() {
        Some("passcode")
    } else {
        None
    }
}

/// A key, phrase or access file of `hash`'s is released with the identity's
/// passcode when it has one, otherwise only after an explicit confirmation: a
/// hardware PIN would need the token. Returns the passcode to open its vault.
pub(crate) fn release_passcode(
    state: &AppState,
    hash: &str,
    passcode: Option<String>,
    confirmed: Option<bool>,
) -> AppResult<Option<Zeroizing<String>>> {
    let passcode = passcode.map(Zeroizing::new);
    if protected_identity_kind(state, hash) == Some("passcode") {
        return match passcode.filter(|passcode| !passcode.is_empty()) {
            Some(passcode) => Ok(Some(passcode)),
            None => Err(AppError::unauthorized("Enter this identity's PIN")),
        };
    }
    if confirmed == Some(true) {
        Ok(None)
    } else {
        Err(AppError::bad_request(
            "Confirm before releasing this identity's keys",
        ))
    }
}

/// `hash`'s key and stored phrase, opened as an unlock opens them.
fn open_identity_files(
    job: &VaultJob,
    id_dir: &std::path::Path,
    hash: &str,
    passcode: Option<&str>,
) -> Result<(vault::UnlockedVault, Identity), vault::VaultError> {
    let unlocked = job.open_identity(id_dir, passcode)?;
    let identity = Identity::from_private_key(unlocked.key())
        .map_err(|_| vault::VaultError::InvalidFormat("private identity".into()))?;
    if hex::encode(identity.hash) != hash {
        return Err(vault::VaultError::InvalidFormat(
            "vault identity does not match selected profile".into(),
        ));
    }
    Ok((unlocked, identity))
}

fn key_release_error(error: vault::VaultError) -> AppError {
    match error {
        vault::VaultError::Auth => AppError::unauthorized("Incorrect PIN"),
        error => AppError::bad_request(error.to_string()),
    }
}

/// Checks `passcode` against `hash`'s vault.
pub(crate) async fn verify_identity_passcode(
    state: &AppState,
    hash: &str,
    passcode: Zeroizing<String>,
) -> AppResult<()> {
    let id_dir = state.config.data_dir.join("identities").join(hash);
    let hash = hash.to_owned();
    let job = vault::try_acquire_job().map_err(|e| AppError::service_unavailable(e.to_string()))?;
    tokio::task::spawn_blocking(move || {
        open_identity_files(&job, &id_dir, &hash, Some(&passcode)).map(drop)
    })
    .await
    .map_err(|_| AppError::internal("passcode check task panicked"))?
    .map_err(key_release_error)
}

fn check_unlock_secret(kind: Option<&str>, secret: &str) -> AppResult<()> {
    if secret.len() < 6 {
        return Err(AppError::bad_request("PIN must be at least 6 characters"));
    }
    if kind == Some("hardware") && secret.len() > 8 {
        return Err(AppError::bad_request("PIN must be 6-8 characters"));
    }
    if kind != Some("hardware") && secret.len() > vault::MAX_PASSCODE_BYTES {
        return Err(AppError::bad_request("Passcode exceeds 1024 UTF-8 bytes"));
    }
    Ok(())
}

fn parse_remaining_attempts(msg: &str) -> Option<u8> {
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

pub(crate) async fn unlock_protected_identity(
    state: Arc<AppState>,
    secret: String,
) -> AppResult<Value> {
    let secret = Zeroizing::new(secret);
    let generation = state.current_identity_session_generation();
    // The secret is staged for the locked identity alone.
    let expected_hash = state
        .hw_locked_hash()
        .ok_or_else(|| AppError::bad_request("No locked identity"))?;
    let kind = protected_identity_kind(&state, &expected_hash);
    check_unlock_secret(kind, &secret)?;

    // Validate the candidate before stopping a usable runtime. The job permit
    // lives in the actual worker if this awaiting command is cancelled.
    let unlocked = if kind == Some("passcode") {
        let hash = expected_hash.clone();
        let id_dir = state.config.data_dir.join("identities").join(&hash);
        let passcode = secret.clone();
        let job =
            vault::try_acquire_job().map_err(|e| AppError::service_unavailable(e.to_string()))?;
        let unlocked = tokio::task::spawn_blocking(move || {
            open_identity_files(&job, &id_dir, &hash, Some(&passcode)).map(|(_, identity)| identity)
        })
        .await
        .map_err(|_| AppError::internal("identity unlock task panicked"))?;
        match unlocked {
            Ok(identity) => Some(identity),
            Err(vault::VaultError::Auth) => {
                return Ok(
                    json!({"ok":false,"kind":"passcode","error":"Incorrect PIN or corrupt identity vault","locked":false,"remaining":null}),
                );
            }
            Err(error) => return Err(AppError::bad_request(error.to_string())),
        }
    } else {
        None
    };
    let _guard = state.identity_switch_lock.lock().await;
    if generation != state.current_identity_session_generation()
        || state.hw_locked_hash().as_deref() != Some(expected_hash.as_str())
    {
        return Err(AppError::conflict(
            "Selected identity changed during unlock; retry",
        ));
    }

    crate::shutdown_rns_lxmf(&state)
        .await
        .map_err(identity_activity_reset_error)?;
    state.clear_identity_scoped_runtime_state();
    state.set_hw_last_error(None);
    if let Some(identity) = unlocked {
        if !state.retain_unlocked_identity(identity, &expected_hash, generation) {
            return Err(AppError::conflict(
                "Selected identity changed during unlock",
            ));
        }
        state.clear_pending_hw_pin();
    } else {
        state.stage_pending_hw_pin(&expected_hash, secret);
    }
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
    let unlocked = loaded_identity.as_deref() == Some(expected_hash.as_str());
    if unlocked {
        state.set_hw_locked(None);
        state.set_hw_last_error(None);
        return Ok(json!({
            "ok": true,
            "kind": kind.unwrap_or("unknown"),
        }));
    }

    let msg = state.take_hw_last_error().unwrap_or_else(|| match kind {
        Some("passcode") => "Could not unlock the identity.".to_string(),
        _ => "Could not unlock the hardware identity.".to_string(),
    });
    state.set_hw_locked(Some(expected_hash));
    state.set_startup_stage("hw_locked");
    let locked = kind == Some("hardware") && msg.contains("PIN locked");
    Ok(json!({
        "ok": false,
        "kind": kind.unwrap_or("unknown"),
        "error": msg,
        "locked": locked,
        "remaining": parse_remaining_attempts(&msg),
    }))
}

/// Unlock the active protected identity. For software identities, `secret` is
/// the at-rest encryption PIN; for hardware identities, it is the token PIN.
#[tauri::command]
pub async fn unlock_identity(state: State<'_, Arc<AppState>>, secret: String) -> AppResult<Value> {
    unlock_protected_identity(Arc::clone(&state), secret).await
}

async fn import_identity_shared(
    state: State<'_, Arc<AppState>>,
    key_bytes: Zeroizing<Vec<u8>>,
    nickname: Option<String>,
) -> AppResult<Value> {
    import_identity_shared_with_passcode(state, key_bytes, nickname, None).await
}

async fn import_identity_shared_with_passcode(
    state: State<'_, Arc<AppState>>,
    key_bytes: Zeroizing<Vec<u8>>,
    nickname: Option<String>,
    passcode: Option<Zeroizing<String>>,
) -> AppResult<Value> {
    let request_fence = state.activity_request_fence();
    import_identity_request(
        Arc::clone(&state),
        key_bytes,
        nickname,
        passcode,
        None,
        request_fence,
    )
    .await
}

async fn import_identity_request(
    state: Arc<AppState>,
    key_bytes: Zeroizing<Vec<u8>>,
    nickname: Option<String>,
    passcode: Option<Zeroizing<String>>,
    explicit_mnemonic: Option<Zeroizing<String>>,
    request_fence: ActivityRequestFence,
) -> AppResult<Value> {
    import_identity_after_parse(
        state,
        nickname,
        explicit_mnemonic,
        request_fence,
        parse_identity_import_async(key_bytes, passcode),
        persist_parsed_identity_import,
    )
    .await
}

/// A failed identity creation, as the window shows it. A phrase that could
/// not be stored has a code of its own: no identity was created without it.
pub(crate) fn identity_import_error(error: crate::lxmf::IdentityImportError) -> AppError {
    match error {
        crate::lxmf::IdentityImportError::Phrase(error) => {
            tracing::error!(%error, "identity not created: its recovery phrase was not stored");
            AppError::new(
                "phrase_not_saved",
                "Ratspeak couldn't save the recovery phrase, so it didn't create the identity. \
                 Make sure this device has free storage, then try again.",
            )
        }
        error => AppError::bad_request(error.to_string()),
    }
}

fn persist_parsed_identity_import(
    state: Arc<AppState>,
    parsed: ParsedIdentityImport,
    nickname: String,
    mnemonic: Option<Zeroizing<String>>,
) -> AppResult<(String, String)> {
    // A recoverable identity is created with its stored phrase or not at all.
    let job = mnemonic
        .as_ref()
        .map(|_| vault::try_acquire_job())
        .transpose()
        .map_err(|e| AppError::service_unavailable(e.to_string()))?;
    crate::lxmf::LxmfManager::import_identity_with_phrase(
        &state.config.data_dir,
        &parsed.key_bytes,
        mnemonic.as_deref().map(String::as_str).zip(job.as_ref()),
        &nickname,
        &state.db,
    )
    .map_err(identity_import_error)
}

// The production-used seam lets regression tests pause preparation or the
// actual persistent worker without requiring a WebView, KDF timing or a card.
async fn import_identity_after_parse<P, W>(
    state: Arc<AppState>,
    nickname: Option<String>,
    explicit_mnemonic: Option<Zeroizing<String>>,
    request_fence: ActivityRequestFence,
    parse: P,
    persist: W,
) -> AppResult<Value>
where
    P: std::future::Future<Output = AppResult<ParsedIdentityImport>>,
    W: FnOnce(
            Arc<AppState>,
            ParsedIdentityImport,
            String,
            Option<Zeroizing<String>>,
        ) -> AppResult<(String, String)>
        + Send
        + 'static,
{
    let nickname = sanitize_announced_display_name(nickname.as_deref().unwrap_or(""))
        .map_err(AppError::bad_request)?;
    // Bounded parsing/KDF retains its own permit outside the lifecycle lock.
    let parsed = parse.await?;
    let identity_lifecycle = acquire_identity_mutation(&state, request_fence).await?;
    let format = parsed.format;
    let mnemonic = explicit_mnemonic.or_else(|| parsed.mnemonic.clone());
    let parsed_identity = Identity::from_private_key(&parsed.key_bytes)
        .map_err(|error| AppError::bad_request(format!("Invalid identity key: {error}")))?;
    let import_hash = hex::encode(parsed_identity.hash);
    let (already_exists, active) = db::spawn_db_with_lease(
        state.db.clone(),
        Arc::clone(&identity_lifecycle),
        move |pool| {
            (
                db::get_identity(&pool, &import_hash).is_some(),
                db::get_active_identity(&pool),
            )
        },
    )
    .await
    .map_err(|_| AppError::internal("identity duplicate check db task panicked"))?;
    // Read before anything is written: a failed read is not "none active", and
    // activating the import on it would switch identity.
    let active_missing = active.map_err(AppError::database_unavailable)?.is_none();
    if already_exists {
        return Err(AppError::conflict("Identity already exists"));
    }
    let worker_state = Arc::clone(&state);
    let (hash, lxmf_hash) = run_identity_mutation(Arc::clone(&identity_lifecycle), move || {
        persist(worker_state, parsed, nickname, mnemonic)
    })
    .await
    .map_err(|_| AppError::internal("import_identity task panicked"))??;

    // Reuse this exact lifecycle owner. A nested acquisition would deadlock,
    // while releasing it here would allow reset to erase the just-imported row.
    if active_missing {
        let reset = crate::reset_activity_for_shutdown(&state)
            .await
            .map_err(identity_activity_reset_error)?;
        // An activation: nothing begun before it may land in this identity.
        state.bump_identity_session_generation();
        crate::finish_shutdown(&state, &reset).await;
        let hash_for_active = hash.clone();
        db::spawn_db_with_lease(
            state.db.clone(),
            Arc::clone(&identity_lifecycle),
            move |pool| db::set_active_identity(&pool, &hash_for_active),
        )
        .await
        .map_err(|_| AppError::internal("activate imported identity task panicked"))?
        .map_err(|error| AppError::internal(format!("Failed to activate import: {error}")))?;
    }
    Ok(json!({
        "hash": hash,
        "lxmf_hash": lxmf_hash,
        "format": format,
        "activated": active_missing,
    }))
}

#[tauri::command]
pub async fn api_preview_identity_import_base64(args: ImportIdentityArgs) -> AppResult<Value> {
    let bytes = decode_identity_base64(&Zeroizing::new(args.key))?;
    let parsed = parse_identity_import_async(bytes, args.passcode.map(Zeroizing::new)).await?;
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

/// Runs `export` on `hash`'s private key and stored recovery phrase, under the
/// one vault job, opened as an unlock opens them: a passcode identity's from
/// its vault with its passcode, so a locked one exports too; any other's from
/// its files once confirmed. A hardware key never leaves its token.
async fn export_identity<T: Send + 'static>(
    state: &AppState,
    hash: &str,
    passcode: Option<String>,
    confirmed: Option<bool>,
    export: impl FnOnce(&VaultJob, &vault::UnlockedVault) -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    if !validate_hex(hash, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }
    let id_dir = state.config.data_dir.join("identities").join(hash);
    if id_dir.join("identity.hwid").exists() {
        return Err(AppError::bad_request(
            "Hardware-backed identity: the private key lives on the token and cannot be exported",
        ));
    }
    if !identity_material_exists(&id_dir) {
        return Err(AppError::not_found("Identity file not found"));
    }
    let passcode = release_passcode(state, hash, passcode, confirmed)?;
    let hash = hash.to_owned();
    let job = vault::try_acquire_job().map_err(|e| AppError::service_unavailable(e.to_string()))?;
    tokio::task::spawn_blocking(move || {
        let (unlocked, _) = open_identity_files(
            &job,
            &id_dir,
            &hash,
            passcode.as_deref().map(String::as_str),
        )
        .map_err(key_release_error)?;
        export(&job, &unlocked)
    })
    .await
    .map_err(|_| AppError::internal("identity export task panicked"))?
}

fn identity_export_hashes(key_bytes: &[u8]) -> AppResult<(String, String)> {
    let identity =
        Identity::from_private_key(key_bytes).map_err(|_| AppError::bad_request("Invalid key"))?;
    let identity_hash = hex::encode(identity.hash);
    let lxmf_dest = Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
    Ok((identity_hash, hex::encode(lxmf_dest)))
}

/// The raw private key. Takes the identity's passcode when it has one,
/// otherwise `confirmed`.
#[tauri::command]
pub async fn api_export_identity_reticulum_base64(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
    passcode: Option<String>,
    confirmed: Option<bool>,
) -> AppResult<Value> {
    export_identity(&state, &hash_hex, passcode, confirmed, |_, identity| {
        let (identity_hash, lxmf_hash) = identity_export_hashes(identity.key())?;
        Ok(json!({
            "data_base64": B64.encode(identity.key()),
            "file_name": format!("{}-reticulum-identity.identity", &identity_hash[..16]),
            "identity_hash": identity_hash,
            "lxmf_hash": lxmf_hash,
            "format": "reticulum.raw-private-key",
        }))
    })
    .await
}

/// The private key as base32 text. Takes the identity's passcode when it has
/// one, otherwise `confirmed`.
#[tauri::command]
pub async fn api_export_identity_reticulum_base32(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
    passcode: Option<String>,
    confirmed: Option<bool>,
) -> AppResult<Value> {
    export_identity(&state, &hash_hex, passcode, confirmed, |_, identity| {
        let (identity_hash, lxmf_hash) = identity_export_hashes(identity.key())?;
        let text = Zeroizing::new(base32_encode_padded(identity.key()));
        Ok(json!({
            "data_base64": B64.encode(text.as_bytes()),
            "text": text.as_str(),
            "file_name": format!("{}-reticulum-identity-key-base32.txt", &identity_hash[..16]),
            "identity_hash": identity_hash,
            "lxmf_hash": lxmf_hash,
            "format": "reticulum.base32-private-key",
        }))
    })
    .await
}

/// A backup sealed with `passcode`, the backup PIN. Like the raw exports it
/// takes the identity's own passcode (`identity_passcode`) when it has one,
/// otherwise `confirmed`.
#[tauri::command]
pub async fn api_export_identity_backup_base64(
    state: State<'_, Arc<AppState>>,
    hash_hex: String,
    passcode: String,
    identity_passcode: Option<String>,
    confirmed: Option<bool>,
) -> AppResult<Value> {
    let passcode = Zeroizing::new(passcode);
    let passcode = Zeroizing::new(passcode.trim().to_owned());
    if passcode.len() < 6 || passcode.len() > vault::MAX_PASSCODE_BYTES {
        return Err(AppError::bad_request(
            "Backup PIN must be at least 6 characters",
        ));
    }
    if !validate_hex(&hash_hex, 16, 128) {
        return Err(AppError::bad_request("Invalid hash"));
    }

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

    // Argon2id KDF — off the async worker like the other vault paths.
    let (vault, identity_hash, lxmf_hash) = export_identity(
        &state,
        &hash_hex,
        identity_passcode,
        confirmed,
        move |job, identity| {
            let (identity_hash, lxmf_hash) = identity_export_hashes(identity.key())?;
            let vault = job
                .encrypt_identity(&passcode, identity.key(), identity.mnemonic())
                .map_err(|e| {
                    AppError::internal(format!("failed to encrypt identity backup: {e}"))
                })?;
            Ok((vault, identity_hash, lxmf_hash))
        },
    )
    .await?;

    let backup = EncryptedIdentityBackupV2 {
        format: IDENTITY_BACKUP_FORMAT.to_string(),
        kind: "private".to_string(),
        vault,
        identity_hash: identity_hash.clone(),
        lxmf_hash: lxmf_hash.clone(),
        display_name,
        status,
        nickname,
        exported_at: now_ts(),
    };
    let bytes = db::admission::bounded_json(&backup, vault::MAX_IDENTITY_IMPORT_BYTES)
        .map_err(|_| {
            AppError::bad_request("Identity backup metadata exceeds the 1 MiB export limit")
        })?
        .into_bytes();

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
    result.map_err(|_| {
        tracing::error!(reason = "update_failed", "update_identity failed");
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
    let identity_lifecycle = Arc::new(state.identity_switch_lock.lock_owned().await);
    let active = active_identity_id(&state);
    let runtime_identity = state
        .lxmf
        .lock()
        .ok()
        .and_then(|lxmf| lxmf.as_ref().map(|mgr| mgr.identity_hash.clone()));
    if active == hash_hex || runtime_identity.as_deref() == Some(hash_hex.as_str()) {
        return Err(AppError::bad_request("Cannot delete active identity"));
    }
    super::shared::forget_shared_text_drafts(&state, Some(hash_hex.clone())).await?;
    let file_refs = if cascade.unwrap_or(false) {
        let hash_for_refs = hash_hex.clone();
        db::spawn_db_with_lease(
            state.db.clone(),
            Arc::clone(&identity_lifecycle),
            move |p| db::get_identity_file_refs(&p, &hash_for_refs),
        )
        .await
        .map_err(|_| AppError::internal("identity file refs db task panicked"))?
    } else {
        Vec::new()
    };
    let hash_for_db = hash_hex.clone();
    let cascade = cascade.unwrap_or(false);
    let result = db::spawn_db_with_lease(
        state.db.clone(),
        Arc::clone(&identity_lifecycle),
        move |p| db::delete_identity(&p, &hash_for_db, cascade),
    )
    .await
    .map_err(|_| AppError::internal("delete_identity db task panicked"))?;
    result.map_err(|e| AppError::internal(format!("Failed to delete: {e}")))?;
    if cascade && !file_refs.is_empty() {
        remove_stored_file_refs(&state.config.files_dir(), file_refs);
    }
    crate::lxmf::LxmfManager::purge_identity_profile(&state.config.data_root, &hash_hex, cascade)
        .map_err(|e| AppError::internal(format!("Failed to remove identity files: {e}")))?;
    state.release_retained_identity_except(&active);
    state.forget_local_identity_public_key(&hash_hex);
    ratspeak_runtime::network_ownership::forget_deleted_profile(&state, &hash_hex)
        .await
        .map_err(AppError::internal)?;
    Ok(json!(null))
}

#[derive(Deserialize)]
pub struct DisplayNameArgs {
    #[serde(default)]
    pub display_name: Option<String>,
    /// The identity the caller shows; the rename is refused if another is active.
    #[serde(default)]
    pub identity_id: Option<String>,
}

/// A switch under way. One that does not finish closes with
/// `identity_switch_failed`, so no window is left showing it.
struct SwitchUnderWay {
    state: Arc<AppState>,
    target: String,
    error: Option<String>,
    finished: bool,
}

impl SwitchUnderWay {
    fn begin(state: &Arc<AppState>, target: &str, generation: u64) -> Self {
        state.emit_to_all(
            "identity_switching",
            json!({
                "hash": target,
                "generation": generation.to_string(),
            }),
        );
        Self {
            state: Arc::clone(state),
            target: target.to_owned(),
            error: None,
            finished: false,
        }
    }

    /// Call just before announcing the new identity.
    fn finish(mut self) {
        self.finished = true;
    }

    /// End the switch with `error`, which its closing event carries.
    fn fail(mut self, error: AppError) -> AppError {
        self.error = Some(error.message.clone());
        error
    }
}

impl Drop for SwitchUnderWay {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let error = self
            .error
            .take()
            .unwrap_or_else(|| "The identity switch was interrupted.".to_string());
        emit_switch_failed(&self.state, &self.target, &error);
    }
}

/// A switch to `target` that did not finish, in the session that followed it.
pub(super) fn emit_switch_failed(state: &AppState, target: &str, error: &str) {
    state.emit_to_all(
        "identity_switch_failed",
        json!({
            "hash": target,
            "generation": state.current_identity_session_generation().to_string(),
            "error": error,
        }),
    );
}

/// Bring the previous identity back up after a failed switch to `target`.
/// With none before it, no identity is left active without a runtime.
pub(super) async fn resume_previous(state: &Arc<AppState>, previous: Option<String>, target: &str) {
    let Some(old_hash) = previous else {
        let target = target.to_owned();
        let deactivated = db::spawn_db(state.db.clone(), move |p| {
            db::deactivate_identity(&p, &target)
        })
        .await;
        // Still active, it would come back at the next start without a runtime.
        if !matches!(deactivated, Ok(Ok(()))) {
            tracing::error!(
                reason = "deactivate_failed",
                "could not leave the failed identity inactive"
            );
            state.set_startup_stage("error");
            return;
        }
        state.set_startup_stage("ready");
        return;
    };
    let reactivated = db::spawn_db(state.db.clone(), move |p| {
        db::set_active_identity(&p, &old_hash)
    })
    .await;
    // With the failed identity still active, a start would only open it again.
    if !matches!(reactivated, Ok(Ok(()))) {
        tracing::error!(
            reason = "reactivate_failed",
            "could not bring the previous identity back"
        );
        state.set_startup_stage("error");
        return;
    }
    if let Ok(mut sig) = state.session_shutdown.write() {
        *sig = rns_runtime::lifecycle::ShutdownSignal::new();
    }
    state.set_startup_stage("checking");
    crate::init_rns_lxmf(Arc::clone(state), state.config.data_root.clone()).await;
    crate::commands::ble::restore_ble_peer_if_requested(Arc::clone(state)).await;
}

async fn mark_identity_used(state: &AppState, hash: &str) {
    let hash = hash.to_owned();
    let marked = db::spawn_db(state.db.clone(), move |p| db::mark_identity_used(&p, &hash)).await;
    if !matches!(marked, Ok(Ok(()))) {
        tracing::warn!(
            reason = "mark_used_failed",
            "could not count the identity's use"
        );
    }
}

async fn switch_identity_session(state: Arc<AppState>, hash_hex: String) -> AppResult<Value> {
    let lifecycle = state.identity_switch_lock.lock_owned().await;
    switch_identity_session_held(state, hash_hex, &lifecycle).await
}

/// The switch, inside a transition its caller holds: a removal re-activates
/// through it without taking the lock again.
async fn switch_identity_session_held(
    state: Arc<AppState>,
    hash_hex: String,
    _lifecycle: &OwnedIdentitySwitchGuard,
) -> AppResult<Value> {
    let _auto_ownership = state.auto_interface_lock.lock().await;

    let hash_for_lookup = hash_hex.clone();
    let target = db::spawn_db(state.db.clone(), move |p| {
        db::get_identity(&p, &hash_for_lookup)
    })
    .await
    .map_err(|_| {
        tracing::error!(reason = "task_panicked", "identity lookup task panicked");
        AppError::internal("db task panicked")
    })?
    .ok_or_else(|| AppError::not_found("Identity not found"))?;

    let id_dir = state.config.data_dir.join("identities").join(&hash_hex);
    // Hardware identities store `.hwid`; passcode-protected software identities
    // store `identity.enc` until unlocked.
    if !identity_material_exists(&id_dir) {
        return Err(AppError::not_found("Identity file not found"));
    }

    // Without the previous identity a failed switch could not restore it.
    let previous_active = db::spawn_db(state.db.clone(), |p| db::get_active_identity(&p))
        .await
        .map_err(|_| AppError::internal("db task panicked"))?
        .map_err(AppError::database_unavailable)?
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

    // Only the reset can refuse, and it comes first: a refused one changes
    // nothing, and once it is acknowledged the previous runtime always stops.
    let reset = crate::reset_activity_for_shutdown(&state)
        .await
        .map_err(identity_activity_reset_error)?;
    let generation = state.bump_identity_session_generation();
    state.withdraw_native_notifications();
    let switching = SwitchUnderWay::begin(&state, &hash_hex, generation);
    crate::finish_shutdown(&state, &reset).await;
    state.clear_identity_scoped_runtime_state();

    // Counted as its use only once the identity opens.
    let hash_for_db = hash_hex.clone();
    let activation = match db::spawn_db(state.db.clone(), move |p| {
        db::activate_identity(&p, &hash_for_db)
    })
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(AppError::internal(format!("Failed to activate: {e}"))),
        Err(_) => {
            tracing::error!(reason = "task_panicked", "activate db task panicked");
            Err(AppError::internal("db task panicked"))
        }
    };
    if let Err(error) = activation {
        // The runtime is down: bring the previous identity back, in a session
        // of its own.
        state.bump_identity_session_generation();
        resume_previous(&state, previous_active, &hash_hex).await;
        return Err(switching.fail(error));
    }

    if let Ok(mut sig) = state.session_shutdown.write() {
        *sig = rns_runtime::lifecycle::ShutdownSignal::new();
    }
    state.set_startup_stage("checking");
    crate::init_rns_lxmf(Arc::clone(&state), state.config.data_root.clone()).await;
    crate::commands::ble::restore_ble_peer_if_requested(Arc::clone(&state)).await;

    // Switching to a protected identity comes up locked (awaiting PIN) — a valid
    // intermediate state, not a failed switch. Keep it active and let the unlock
    // prompt (driven by the hardware_locked event) take over; do not roll back.
    if state.hw_locked_hash().as_deref() == Some(hash_hex.as_str()) {
        mark_identity_used(&state, &hash_hex).await;
        state.release_retained_identity_except(&hash_hex);
        let payload = json!({
            "hash": hash_hex,
            "locked": true,
            "generation": generation.to_string(),
        });
        switching.finish();
        state.emit_to_all("identity_switched", payload.clone());
        return Ok(payload);
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
            generation,
            reason = "identity_mismatch",
            "identity switch failed to load target; rolling back"
        );
        // Target failed to load (e.g. a hardware key that was re-provisioned or
        // unplugged). Restore the previous identity + its runtime so the session
        // is not left with a dead active row and no LXMF. The restored session
        // gets its own generation; it never reuses the failed target's. The
        // switch's reset still covers this teardown.
        state.bump_identity_session_generation();
        crate::finish_shutdown(&state, &reset).await;
        state.clear_identity_scoped_runtime_state();
        resume_previous(&state, previous_active, &hash_hex).await;
        return Err(switching.fail(AppError::bad_request(
            "Couldn't load this identity's key. If it's a hardware (YubiKey) identity, \
             the key may be unplugged or was re-provisioned.",
        )));
    }

    mark_identity_used(&state, &hash_hex).await;
    // Switched away for good: the previous identity's unlock goes. A failed
    // switch kept it to bring that identity back as it was.
    state.release_retained_identity_except(&hash_hex);
    let payload = json!({
        "hash": hash_hex,
        "lxmf_hash": loaded_lxmf,
        "display_name": loaded_display,
        "status": loaded_status,
        "generation": generation.to_string(),
    });
    let ifaces = crate::rns_config::get_all_interfaces(&active_rns_config_dir(&state));
    emit_hub_interfaces(&state, ifaces);
    switching.finish();
    state.emit_to_all("identity_switched", payload.clone());
    state.request_poll_now();
    Ok(payload)
}

/// After the live identity is removed the most recently used one takes over,
/// as at a cold start. The caller holds the lifecycle lock as a transition.
#[cfg(feature = "hardware")]
pub(super) async fn activate_most_recent_identity(
    state: &Arc<AppState>,
    lifecycle: &OwnedIdentitySwitchGuard,
) {
    let data_dir = state.config.data_dir.clone();
    let next = db::spawn_db(state.db.clone(), move |p| {
        ratspeak_runtime::most_recent_identity(&p, &data_dir)
    })
    .await;
    match next {
        Ok(Ok(Some(hash))) => {
            if let Err(error) =
                switch_identity_session_held(Arc::clone(state), hash, lifecycle).await
            {
                tracing::warn!(
                    code = %error.code,
                    "the most recently used identity could not take over"
                );
            }
        }
        Ok(Ok(None)) => state.set_startup_stage("ready"),
        Ok(Err(_)) | Err(_) => {
            tracing::error!(
                reason = "identity_read_failed",
                "could not choose an identity to take over"
            );
            state.set_startup_stage("error");
        }
    }
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
    let activity_origin = state.activity_request_fence();
    let display_name = sanitize_announced_display_name(args.display_name.as_deref().unwrap_or(""))
        .map_err(AppError::bad_request)?;
    if display_name.is_empty() {
        return Err(AppError::bad_request("display_name required"));
    }
    // A switch lands before or after the check, the rename and its write,
    // never between: the name stays with the identity the caller showed.
    let lifecycle = acquire_identity_routine(&state, activity_origin).await?;
    let identity_id = callers_identity(&state, args.identity_id.as_deref())?;
    // The mgr may be absent on a startup race: DB-only then.
    let updated_in_memory = {
        let mut guard = state
            .lxmf
            .lock()
            .map_err(|_| AppError::internal("lxmf state lock poisoned"))?;
        match guard.as_mut() {
            Some(mgr) if mgr.identity_hash == identity_id => {
                mgr.set_display_name(&display_name);
                // With the rename, so nothing built under the old name is sent.
                state.bump_announce_content_revision();
                true
            }
            Some(_) => return Err(AppError::conflict("Identity changed; retry")),
            None => false,
        }
    };

    let id = identity_id.clone();
    let dn = display_name.clone();
    // The rename and the saved-hub sweep commit together, so a failure leaves
    // the old name intact everywhere and the retry re-runs the sweep.
    let rename = db::spawn_db(state.db.clone(), move |p| {
        db::rename_identity_and_retire_alias(&p, &id, &dn)
    })
    .await
    .map_err(|_| AppError::internal("failed to save display name"))?
    .map_err(|_| {
        tracing::error!(reason = "update_failed", "display_name: rename failed");
        AppError::internal("failed to save display name")
    })?;
    let channels = state.channels_handle();
    drop(lifecycle);

    // A live channels session snapshots its nickname at connect, so without
    // this it would keep stamping the superseded name on every outbound
    // envelope for the rest of the session.
    if !rename.previous_name.is_empty()
        && rename.previous_name != display_name
        && let Some(channels) = channels
    {
        channels
            .identity_renamed(&rename.previous_name, &display_name)
            .await;
    }

    if updated_in_memory {
        crate::send_typed_announce_from_origin(
            &state,
            crate::announce::AnnounceOrigin::IdentityChanged,
            activity_origin,
        )
        .await;
    }

    Ok(json!({ "display_name": display_name }))
}

#[tauri::command]
pub async fn set_identity_status(
    state: State<'_, Arc<AppState>>,
    status: Option<String>,
    identity_id: Option<String>,
) -> AppResult<Value> {
    let activity_origin = state.activity_request_fence();
    let status = sanitize_announced_status(status.as_deref().unwrap_or(""))
        .map_err(AppError::bad_request)?;
    // As a rename: one routine span for the check, the change and its write.
    let lifecycle = acquire_identity_routine(&state, activity_origin).await?;
    let identity_id = callers_identity(&state, identity_id.as_deref())?;
    // The mgr may be absent on a startup race: DB-only then.
    let identity_payload: Option<Value> = {
        let mut guard = state
            .lxmf
            .lock()
            .map_err(|_| AppError::internal("lxmf state lock poisoned"))?;
        match guard.as_mut() {
            Some(mgr) if mgr.identity_hash == identity_id => {
                mgr.set_status(&status);
                state.bump_announce_content_revision();
                Some(json!({
                    "hash": mgr.lxmf_hash.clone(),
                    "identity_hash": mgr.identity_hash.clone(),
                    "display_name": mgr.display_name.clone(),
                    "status": status.clone(),
                }))
            }
            Some(_) => return Err(AppError::conflict("Identity changed; retry")),
            None => None,
        }
    };

    let id = identity_id.clone();
    let saved_status = status.clone();
    db::spawn_db(state.db.clone(), move |p| {
        db::update_identity_status(&p, &id, &saved_status)
    })
    .await
    .map_err(|_| AppError::internal("failed to save status"))?
    .map_err(|_| {
        tracing::error!(
            reason = "update_failed",
            "status: update_identity_status failed"
        );
        AppError::internal("failed to save status")
    })?;

    if let Some(payload) = identity_payload {
        state.emit_to_all("lxmf_identity", payload);
        drop(lifecycle);
        crate::send_typed_announce_from_origin(
            &state,
            crate::announce::AnnounceOrigin::IdentityChanged,
            activity_origin,
        )
        .await;
    }

    Ok(json!({ "status": status }))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(in crate::commands) static VAULT_TEST_SERIAL: std::sync::Mutex<()> =
        std::sync::Mutex::new(());

    static TEMP_IDENTITY_COMMAND_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn mutation_state() -> (tempfile::TempDir, Arc<AppState>) {
        let root = tempfile::tempdir().unwrap();
        crate::lxmf::initialize_storage_namespace(root.path()).unwrap();
        let config = crate::config::DashboardConfig::from_env_and_defaults(root.path().into());
        let pool = db::init_pool(root.path()).unwrap();
        db::init_schema(&pool).unwrap();
        let state = Arc::new(AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        (root, state)
    }

    const ACTIVE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// ACTIVE and OTHER exist, ACTIVE is active. One pooled connection, so
    /// holding it fails every read for 200 ms.
    fn single_connection_state() -> (tempfile::TempDir, Arc<AppState>) {
        let root = tempfile::tempdir().unwrap();
        crate::lxmf::initialize_storage_namespace(root.path()).unwrap();
        let config = crate::config::DashboardConfig::from_env_and_defaults(root.path().into());
        let manager = r2d2_sqlite::SqliteConnectionManager::file(config.db_path())
            .with_init(|c| c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=30000;"));
        let pool = r2d2::Pool::builder()
            .max_size(1)
            .connection_timeout(std::time::Duration::from_millis(200))
            .build_unchecked(manager);
        // Open the connection without the 200 ms read bound, which a loaded runner can miss.
        drop(
            pool.get_timeout(std::time::Duration::from_secs(30))
                .unwrap(),
        );
        db::init_schema(&pool).unwrap();
        for hash in [ACTIVE, OTHER] {
            db::save_identity(&pool, hash, &hash[..16], "", "");
        }
        db::set_active_identity(&pool, ACTIVE).unwrap();
        let state = Arc::new(AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        (root, state)
    }

    fn active_hash(state: &AppState) -> Option<String> {
        state
            .db
            .get()
            .unwrap()
            .query_row(
                "SELECT hash FROM identities WHERE is_active = 1",
                [],
                |row| row.get(0),
            )
            .ok()
    }

    fn identity_dirs(state: &AppState) -> Vec<String> {
        let mut dirs: Vec<String> = std::fs::read_dir(state.config.data_dir.join("identities"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        dirs
    }

    #[tokio::test]
    async fn import_refuses_when_the_active_identity_cannot_be_read() {
        let (_root, state) = single_connection_state();
        let boundary = state.current_activity_boundary_generation();
        let dirs = identity_dirs(&state);
        let held = state.db.get().unwrap();
        let result = import_identity_after_parse(
            Arc::clone(&state),
            None,
            None,
            state.activity_request_fence(),
            std::future::ready(Ok(parsed_key(private_key_bytes(), None))),
            persist_parsed_identity_import,
        )
        .await;
        drop(held);
        assert_eq!(result.unwrap_err().code, "database_unavailable");
        assert_eq!(identity_dirs(&state), dirs, "no identity was created");
        assert_eq!(db::get_all_identities(&state.db).len(), 2);
        assert_eq!(active_hash(&state).as_deref(), Some(ACTIVE));
        assert_eq!(
            state.current_activity_boundary_generation(),
            boundary,
            "nothing was shut down"
        );
    }

    #[tokio::test]
    async fn switch_refuses_when_the_active_identity_cannot_be_read() {
        let (_root, state) = single_connection_state();
        let target = state.config.data_dir.join("identities").join(OTHER);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("identity.enc"), b"sealed").unwrap();
        // The target lookup reads another row first, so the active row itself
        // is made unreadable instead of holding the only connection.
        state
            .db
            .get()
            .unwrap()
            .execute(
                "UPDATE identities SET created_at = 'unreadable' WHERE hash = ?1",
                [ACTIVE],
            )
            .unwrap();
        assert!(db::get_active_identity(&state.db).is_err());
        let boundary = state.current_activity_boundary_generation();
        let generation = state.current_identity_session_generation();
        let result = switch_identity_session(Arc::clone(&state), OTHER.to_string()).await;
        assert_eq!(result.unwrap_err().code, "database_unavailable");
        assert_eq!(active_hash(&state).as_deref(), Some(ACTIVE));
        assert_eq!(state.current_activity_boundary_generation(), boundary);
        assert_eq!(state.current_identity_session_generation(), generation);
    }

    #[derive(Default)]
    struct Recorder {
        events: std::sync::Mutex<Vec<(String, Value)>>,
    }

    impl ratspeak_core::Emitter for Recorder {
        fn try_emit(&self, event: &str, payload: Value) -> Result<(), ratspeak_core::EmitError> {
            self.events
                .lock()
                .unwrap()
                .push((event.to_owned(), payload));
            Ok(())
        }
    }

    impl Recorder {
        fn last(&self, name: &str) -> Option<Value> {
            let events = self.events.lock().unwrap();
            events
                .iter()
                .rev()
                .find(|(event, _)| event == name)
                .map(|(_, payload)| payload.clone())
        }

        async fn wait_for(&self, name: &str) -> Value {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some(payload) = self.last(name) {
                    return payload;
                }
                assert!(tokio::time::Instant::now() < deadline, "no {name}");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    }

    /// No identity, with events recorded and a mock app holding the state, so
    /// commands can take their `State`.
    fn recorded_state() -> (
        tempfile::TempDir,
        Arc<AppState>,
        Arc<Recorder>,
        tauri::App<tauri::test::MockRuntime>,
    ) {
        let root = tempfile::tempdir().unwrap();
        crate::lxmf::initialize_storage_namespace(root.path()).unwrap();
        let config = crate::config::DashboardConfig::from_env_and_defaults(root.path().into());
        let pool = db::init_pool(root.path()).unwrap();
        db::init_schema(&pool).unwrap();
        let recorder = Arc::new(Recorder::default());
        let state = Arc::new(AppState::new(
            config,
            pool,
            Arc::clone(&recorder) as Arc<dyn ratspeak_core::Emitter>,
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        let app = tauri::test::mock_builder()
            .manage(Arc::clone(&state))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        (root, state, recorder, app)
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    /// Start an import whose preparation pauses until `release` is sent.
    fn paused_import(
        state: &Arc<AppState>,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<AppResult<Value>>,
    ) {
        let request_fence = state.activity_request_fence();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let import = tokio::spawn(import_identity_after_parse(
            Arc::clone(state),
            None,
            None,
            request_fence,
            async move {
                ready_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok(parsed_key(private_key_bytes(), None))
            },
            persist_parsed_identity_import,
        ));
        (ready_rx, release_tx, import)
    }

    // What the window does all the time must not fail an identity mutation.
    #[test]
    fn identity_mutation_survives_routine_work_during_preparation() {
        use tauri::Manager;
        let (_root, state, recorder, app) = recorded_state();
        test_runtime().block_on(async {
            let (ready, release, import) = paused_import(&state);
            ready.await.unwrap();
            super::super::activity::activity_status(app.state())
                .await
                .unwrap();
            let _ = super::super::channels::api_channel_unread(app.state()).await;
            let edge = state.begin_foreground_transition();
            super::super::system::apply_foreground_transition(Arc::clone(&state), true, edge)
                .await
                .unwrap();
            let unblock = super::super::contacts::UnblockContactArgs {
                hash: "ab".repeat(16),
                also_remove_blackhole: false,
            };
            super::super::contacts::unblock_contact(app.state(), unblock)
                .await
                .unwrap();
            // The unblock re-sends the conversation list under the lifecycle lock.
            recorder.wait_for("conversations_update").await;
            release.send(()).unwrap();
            let imported = import.await.unwrap().unwrap();
            let hash = imported["hash"].as_str().unwrap();
            assert!(db::get_identity(&state.db, hash).is_some());
        });
    }

    // A guard, which passes even when every lifecycle holder is a transition:
    // a privacy reset during preparation must still refuse.
    #[test]
    fn identity_mutation_refused_after_a_privacy_reset_during_preparation() {
        use tauri::Manager;
        let (_root, state, _recorder, app) = recorded_state();
        test_runtime().block_on(async {
            let (ready, release, import) = paused_import(&state);
            ready.await.unwrap();
            let _ = super::super::activity::activity_clear(app.state()).await;
            release.send(()).unwrap();
            assert_eq!(import.await.unwrap().unwrap_err().code, "conflict");
            assert!(db::get_all_identities(&state.db).is_empty());
        });
    }

    #[tokio::test]
    async fn import_with_none_active_starts_a_new_identity_session() {
        let (_root, state) = mutation_state();
        for activates in [true, false] {
            let generation = state.current_identity_session_generation();
            let imported = import_identity_after_parse(
                Arc::clone(&state),
                None,
                None,
                state.activity_request_fence(),
                std::future::ready(Ok(parsed_key(private_key_bytes(), None))),
                persist_parsed_identity_import,
            )
            .await
            .unwrap();
            assert_eq!(imported["activated"], activates);
            assert_eq!(
                state.current_identity_session_generation() > generation,
                activates,
                "only the import that activates starts a session"
            );
        }
    }

    fn parsed_key(key_bytes: Vec<u8>, mnemonic: Option<Zeroizing<String>>) -> ParsedIdentityImport {
        ParsedIdentityImport {
            key_bytes: Zeroizing::new(key_bytes),
            format: "test-private-key",
            mnemonic,
        }
    }

    // Vault admission is process-wide, so the seed worker's tests hold the
    // vault serial from the start, not from inside the timed wait.
    #[test]
    #[cfg(feature = "seed")]
    fn identity_mutation_cancelled_import_keeps_reset_blocked_until_seed_worker_finishes() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        test_runtime().block_on(async {
            let (_root, state) = mutation_state();
            let (phrase, key) = ratspeak_runtime::generate_recoverable_key().unwrap();
            let identity = Identity::from_private_key(&key[..]).unwrap();
            let hash = hex::encode(identity.hash);
            let request_fence = state.activity_request_fence();
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let worker_state = Arc::clone(&state);
            let import = tokio::spawn(async move {
                import_identity_after_parse(
                    worker_state,
                    None,
                    Some(phrase),
                    request_fence,
                    std::future::ready(Ok(parsed_key(key.to_vec(), None))),
                    move |state, parsed, nickname, mnemonic| {
                        ready_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        persist_parsed_identity_import(state, parsed, nickname, mnemonic)
                    },
                )
                .await
            });
            ready_rx.await.unwrap();
            import.abort();
            assert!(import.await.unwrap_err().is_cancelled());
            let reset_state = Arc::clone(&state);
            let reset_hash = hash.clone();
            let mut reset = tokio::spawn(async move {
                let _lifecycle = reset_state.identity_switch_lock.lock_owned().await;
                crate::lxmf::clear_identity_storage_namespace(&reset_state.config.data_dir)
                    .unwrap();
                db::delete_identity(&reset_state.db, &reset_hash, true).unwrap();
            });
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(40), &mut reset)
                    .await
                    .is_err()
            );
            release_tx.send(()).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), reset)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !state
                    .config
                    .data_dir
                    .join("identities")
                    .join(&hash)
                    .exists()
            );
            assert!(db::get_identity(&state.db, &hash).is_none());
            assert!(
                db::get_active_identity(&state.db).unwrap().is_none(),
                "cancelled import must not later activate or resurrect its row"
            );
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identity_mutation_reset_during_preparation_rejects_before_files() {
        let (_root, state) = mutation_state();
        let request_fence = state.activity_request_fence();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let import_state = Arc::clone(&state);
        let import = tokio::spawn(async move {
            import_identity_after_parse(
                import_state,
                None,
                None,
                request_fence,
                async move {
                    ready_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(parsed_key(private_key_bytes(), None))
                },
                persist_parsed_identity_import,
            )
            .await
        });
        ready_rx.await.unwrap();
        {
            let _lifecycle = state.identity_switch_lock.lock_owned().await;
            crate::lxmf::clear_identity_storage_namespace(&state.config.data_dir).unwrap();
        }
        release_tx.send(()).unwrap();
        let error = import.await.unwrap().unwrap_err();
        assert_eq!(error.code, "conflict");
        assert_eq!(
            std::fs::read_dir(state.config.data_dir.join("identities"))
                .unwrap()
                .count(),
            0
        );
        assert!(db::get_all_identities(&state.db).is_empty());
        assert!(db::get_active_identity(&state.db).unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identity_mutation_waiting_across_reset_rejects_before_files() {
        let (_root, state) = mutation_state();
        let request_fence = state.activity_request_fence();
        let reset = state.identity_switch_lock.lock_owned().await;
        let import_state = Arc::clone(&state);
        let mut import = tokio::spawn(async move {
            import_identity_after_parse(
                import_state,
                None,
                None,
                request_fence,
                std::future::ready(Ok(parsed_key(private_key_bytes(), None))),
                persist_parsed_identity_import,
            )
            .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(40), &mut import)
                .await
                .is_err()
        );
        crate::lxmf::clear_identity_storage_namespace(&state.config.data_dir).unwrap();
        drop(reset);
        assert_eq!(import.await.unwrap().unwrap_err().code, "conflict");
        assert_eq!(
            std::fs::read_dir(state.config.data_dir.join("identities"))
                .unwrap()
                .count(),
            0
        );
        assert!(db::get_all_identities(&state.db).is_empty());
    }

    #[test]
    #[cfg(feature = "seed")]
    fn identity_mutation_explicit_and_backup_seeds_commit_with_first_activation() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        test_runtime().block_on(async {
            for explicit in [true, false] {
                let (_root, state) = mutation_state();
                let (phrase, key) = ratspeak_runtime::generate_recoverable_key().unwrap();
                let identity = Identity::from_private_key(&key[..]).unwrap();
                let hash = hex::encode(identity.hash);
                let parsed = parsed_key(key.to_vec(), (!explicit).then_some(phrase.clone()));
                let value = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    import_identity_after_parse(
                        Arc::clone(&state),
                        None,
                        explicit.then_some(phrase),
                        state.activity_request_fence(),
                        std::future::ready(Ok(parsed)),
                        persist_parsed_identity_import,
                    ),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(value["hash"], hash);
                assert_eq!(value["activated"], true);
                assert!(
                    state
                        .config
                        .data_dir
                        .join("identities")
                        .join(&hash)
                        .join("identity.seed")
                        .is_file()
                );
                assert!(db::get_identity(&state.db, &hash).is_some());
                assert_eq!(
                    db::get_active_identity(&state.db).unwrap().unwrap()["hash"],
                    hash
                );
            }
        });
    }

    /// Asserts that no part of `hash` was created: no row, no activation and
    /// no key, vault or phrase file.
    fn assert_no_identity(state: &AppState, hash: &str) {
        assert!(db::get_identity(&state.db, hash).is_none(), "listed");
        assert!(
            db::get_active_identity(&state.db).unwrap().is_none(),
            "active"
        );
        let profile = state.config.data_dir.join("identities").join(hash);
        assert!(!identity_material_exists(&profile), "key left behind");
        assert!(
            !profile.join("identity.seed").is_file(),
            "phrase left behind"
        );
    }

    async fn import_recoverable(
        state: &Arc<AppState>,
        key: &[u8],
        phrase: &Zeroizing<String>,
    ) -> AppResult<Value> {
        import_identity_after_parse(
            Arc::clone(state),
            None,
            Some(phrase.clone()),
            state.activity_request_fence(),
            std::future::ready(Ok(parsed_key(key.to_vec(), None))),
            persist_parsed_identity_import,
        )
        .await
    }

    // A recoverable identity is created with its stored phrase or not at
    // all: while another identity encryption holds the vault, nothing is.
    #[test]
    #[cfg(feature = "seed")]
    fn a_recoverable_identity_is_not_created_while_the_vault_is_busy() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        test_runtime().block_on(async {
            let (_root, state) = mutation_state();
            let (phrase, key) = ratspeak_runtime::generate_recoverable_key().unwrap();
            let hash = hex::encode(Identity::from_private_key(&key[..]).unwrap().hash);
            let busy = vault::try_acquire_job().unwrap();
            let refused = import_recoverable(&state, &key[..], &phrase).await;
            assert_eq!(refused.unwrap_err().code, "service_unavailable");
            assert_no_identity(&state, &hash);

            drop(busy);
            let created = import_recoverable(&state, &key[..], &phrase).await.unwrap();
            assert_eq!(
                (&created["hash"], &created["activated"]),
                (&json!(hash), &json!(true))
            );
            let profile = state.config.data_dir.join("identities").join(&hash);
            assert_eq!(vault::reveal_mnemonic(&profile, None).unwrap(), phrase);
        });
    }

    // A phrase that cannot be written leaves no identity behind, and the same
    // identity is created with its phrase once it can be.
    #[test]
    #[cfg(feature = "seed")]
    fn a_recoverable_identity_is_not_created_when_its_phrase_cannot_be_written() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        test_runtime().block_on(async {
            let (_root, state) = mutation_state();
            let (phrase, key) = ratspeak_runtime::generate_recoverable_key().unwrap();
            let hash = hex::encode(Identity::from_private_key(&key[..]).unwrap().hash);
            let profile = state.config.data_dir.join("identities").join(&hash);
            let blocked = profile.join("identity.seed");
            std::fs::create_dir_all(blocked.join("occupied")).unwrap();
            let failed = import_recoverable(&state, &key[..], &phrase).await;
            assert_eq!(failed.unwrap_err().code, "phrase_not_saved");
            assert_no_identity(&state, &hash);
            assert!(
                blocked.join("occupied").is_dir(),
                "only its own files are removed"
            );

            std::fs::remove_dir_all(&blocked).unwrap();
            let created = import_recoverable(&state, &key[..], &phrase).await.unwrap();
            assert_eq!(created["activated"], true);
            assert_eq!(vault::reveal_mnemonic(&profile, None).unwrap(), phrase);
        });
    }

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
        let _serial = VAULT_TEST_SERIAL.lock().unwrap();
        let key = private_key_bytes();
        let parsed = parse_private_identity_bytes(&key, None).unwrap();
        assert_eq!(parsed.format, "raw-private-key");
        assert_eq!(*parsed.key_bytes, key);
    }

    #[test]
    fn parse_private_identity_accepts_encrypted_backup_envelope() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap();
        let key = private_key_bytes();
        let preview = identity_preview_payload(&key, "test").unwrap();
        let key_array: [u8; 64] = key.as_slice().try_into().unwrap();
        let vault =
            ratspeak_runtime::vault::encrypt_identity("123456", &key_array, Some("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")).unwrap();
        let backup = EncryptedIdentityBackupV2 {
            format: IDENTITY_BACKUP_FORMAT.to_string(),
            kind: "private".to_string(),
            vault,
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

        let parsed = parse_private_identity_bytes(&encoded, Some("123456")).unwrap();
        assert_eq!(parsed.format, IDENTITY_BACKUP_FORMAT);
        assert_eq!(*parsed.key_bytes, key);
        assert!(parsed.mnemonic.is_some());
    }

    #[test]
    fn parse_private_identity_rejects_encrypted_backup_without_pin() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap();
        let key = private_key_bytes();
        let preview = identity_preview_payload(&key, "test").unwrap();
        let key_array: [u8; 64] = key.as_slice().try_into().unwrap();
        let vault = ratspeak_runtime::vault::encrypt_key("123456", &key_array).unwrap();
        let backup = EncryptedIdentityBackupV2 {
            format: IDENTITY_BACKUP_FORMAT.to_string(),
            kind: "private".to_string(),
            vault,
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
            display_name: String::new(),
            status: String::new(),
            nickname: String::new(),
            exported_at: 1.0,
        };
        let encoded = serde_json::to_vec(&backup).unwrap();

        let err = parse_private_identity_bytes(&encoded, None)
            .err()
            .expect("refused");
        assert!(err.contains("Backup PIN required"));
    }

    #[test]
    fn legacy_backup_private_key_keeps_its_json_string_shape() {
        let value = json!({
            "format": LEGACY_IDENTITY_BACKUP_FORMAT,
            "kind": "private",
            "private_key": "cHJpdmF0ZSBrZXk=",
            "identity_hash": "identity",
            "lxmf_hash": "destination",
            "display_name": "Name",
            "status": "Away",
            "nickname": "Alias",
            "exported_at": 1.0,
        });
        let backup: LegacyIdentityBackupV1 = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(backup.private_key.as_str(), "cHJpdmF0ZSBrZXk=");
        assert_eq!(serde_json::to_value(&backup).unwrap(), value);

        let mut invalid = value;
        invalid["private_key"] = json!([1, 2, 3]);
        assert!(serde_json::from_value::<LegacyIdentityBackupV1>(invalid).is_err());
    }

    #[test]
    fn parse_private_identity_accepts_legacy_backup_envelope() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap();
        let key = private_key_bytes();
        let preview = identity_preview_payload(&key, "test").unwrap();
        let backup = LegacyIdentityBackupV1 {
            format: LEGACY_IDENTITY_BACKUP_FORMAT.to_string(),
            kind: "private".to_string(),
            private_key: Zeroizing::new(B64.encode(&key)),
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

        let parsed = parse_private_identity_bytes(&encoded, None).unwrap();
        assert_eq!(parsed.format, LEGACY_IDENTITY_BACKUP_FORMAT);
        assert_eq!(*parsed.key_bytes, key);
    }

    #[test]
    fn parse_private_identity_accepts_reticulum_base32_text() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap();
        let key = private_key_bytes();
        let encoded = base32_encode_padded(&key);

        let parsed = parse_private_identity_bytes(encoded.as_bytes(), None).unwrap();
        assert_eq!(parsed.format, "base32-private-key");
        assert_eq!(*parsed.key_bytes, key);
    }

    #[test]
    fn base32_identity_text_roundtrips_with_spacing() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap();
        let key = private_key_bytes();
        let encoded = base32_encode_padded(&key).to_lowercase();
        let wrapped = encoded
            .as_bytes()
            .chunks(8)
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect::<Vec<_>>()
            .join(" ");

        let parsed = parse_private_identity_bytes(wrapped.as_bytes(), None).unwrap();
        assert_eq!(parsed.format, "base32-private-key");
        assert_eq!(*parsed.key_bytes, key);
    }

    #[test]
    fn parse_private_identity_rejects_public_backup() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap();
        let key = private_key_bytes();
        let backup = LegacyIdentityBackupV1 {
            format: LEGACY_IDENTITY_BACKUP_FORMAT.to_string(),
            kind: "public".to_string(),
            private_key: Zeroizing::new(B64.encode(&key)),
            identity_hash: String::new(),
            lxmf_hash: String::new(),
            display_name: String::new(),
            status: String::new(),
            nickname: String::new(),
            exported_at: 1.0,
        };
        let encoded = serde_json::to_vec(&backup).unwrap();

        let err = parse_private_identity_bytes(&encoded, None)
            .err()
            .expect("refused");
        assert!(err.contains("Public identity backups are not activatable"));
    }

    #[test]
    fn parse_private_identity_rejects_hash_mismatch() {
        let _serial = VAULT_TEST_SERIAL.lock().unwrap();
        let key = private_key_bytes();
        let backup = LegacyIdentityBackupV1 {
            format: LEGACY_IDENTITY_BACKUP_FORMAT.to_string(),
            kind: "private".to_string(),
            private_key: Zeroizing::new(B64.encode(&key)),
            identity_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            lxmf_hash: String::new(),
            display_name: String::new(),
            status: String::new(),
            nickname: String::new(),
            exported_at: 1.0,
        };
        let encoded = serde_json::to_vec(&backup).unwrap();

        let err = parse_private_identity_bytes(&encoded, None)
            .err()
            .expect("refused");
        assert!(err.contains("hash does not match"));
    }
}
