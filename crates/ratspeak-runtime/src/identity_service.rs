//! Tauri-free identity onboarding helpers shared by headless frontends.

use std::path::Path;

use serde::Serialize;

use crate::db::{self, DbPool};
use crate::helpers::sanitize_announced_display_name;
use crate::lxmf::LxmfManager;

#[derive(Debug, Clone, Serialize)]
pub struct CreatedIdentity {
    pub hash: String,
    pub lxmf_hash: String,
    pub display_name: String,
    pub nickname: String,
    pub mnemonic: Option<String>,
    pub activated: bool,
    pub seed_stored: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivatedIdentity {
    pub hash: String,
    pub lxmf_hash: String,
    pub display_name: String,
    pub status: String,
    pub requires_runtime_restart: bool,
}

#[cfg(feature = "seed")]
pub fn create_recoverable_identity(
    ratspeak_data_dir: &Path,
    db_pool: &DbPool,
    nickname: Option<&str>,
    activate: bool,
) -> Result<CreatedIdentity, String> {
    let nickname = sanitize_announced_display_name(nickname.unwrap_or(""))?;
    let (mnemonic, key) = crate::generate_recoverable_key()?;
    let (hash, lxmf_hash) =
        LxmfManager::import_identity_to_data_dir(ratspeak_data_dir, &key, &nickname, db_pool)
            .map_err(|e| e.to_string())?;

    let seed_dir = ratspeak_data_dir.join("identities").join(&hash);
    let seed_stored = match crate::vault::store_plaintext_seed(&seed_dir, &mnemonic) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, hash = %hash, "could not store recovery-phrase sidecar");
            false
        }
    };

    let active_missing = db::get_active_identity(db_pool).is_none();
    let activated = if activate || active_missing {
        db::set_active_identity(db_pool, &hash).map_err(|e| format!("activate: {e}"))?;
        true
    } else {
        false
    };

    let identity = db::get_identity(db_pool, &hash);
    let display_name = identity
        .as_ref()
        .and_then(|value| value.get("display_name"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let nickname = identity
        .as_ref()
        .and_then(|value| value.get("nickname"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();

    Ok(CreatedIdentity {
        hash,
        lxmf_hash,
        display_name,
        nickname,
        mnemonic: Some(mnemonic),
        activated,
        seed_stored,
    })
}

pub fn activate_identity(
    ratspeak_data_dir: &Path,
    db_pool: &DbPool,
    hash: &str,
) -> Result<ActivatedIdentity, String> {
    let identity =
        db::get_identity(db_pool, hash).ok_or_else(|| "identity not found".to_string())?;
    let id_dir = ratspeak_data_dir.join("identities").join(hash);
    if !identity_material_exists(&id_dir) {
        return Err("identity file not found".into());
    }

    db::set_active_identity(db_pool, hash).map_err(|e| format!("activate: {e}"))?;

    Ok(ActivatedIdentity {
        hash: hash.to_string(),
        lxmf_hash: identity
            .get("lxmf_hash")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        display_name: identity
            .get("display_name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        status: identity
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        requires_runtime_restart: true,
    })
}

pub fn identity_material_exists(id_dir: &Path) -> bool {
    id_dir.join("identity").exists()
        || id_dir.join("identity.enc").exists()
        || id_dir.join("identity.hwid").exists()
}
