use std::path::PathBuf;
use std::sync::Arc;

use ratspeak_core::{NoopEmitter, NoopNotifier};
use ratspeak_runtime::config::DashboardConfig;
use ratspeak_runtime::state::AppState;
use serde_json::{Value, json};

use crate::error::{CliError, CliResult};

const DESKTOP_IDENTIFIER: &str = "org.ratspeak.desktop";

#[derive(Clone)]
pub struct Profile {
    pub data_root: PathBuf,
    pub config: DashboardConfig,
    pub db: ratspeak_db::DbPool,
}

pub fn resolve_data_root(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    if let Ok(value) = std::env::var("RATSPEAK_DATA_DIR") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    default_desktop_data_root()
}

pub fn open_profile(data_root: PathBuf) -> CliResult<Profile> {
    std::fs::create_dir_all(&data_root)?;
    let config = DashboardConfig::from_env_and_defaults(data_root.clone());
    let db = ratspeak_db::init_pool(&data_root)
        .map_err(|e| CliError::failed(format!("failed to open Ratspeak database: {e}")))?;
    ratspeak_db::init_schema(&db)
        .map_err(|e| CliError::failed(format!("failed to initialize Ratspeak schema: {e}")))?;
    Ok(Profile {
        data_root,
        config,
        db,
    })
}

pub fn offline_state(profile: &Profile) -> Arc<AppState> {
    Arc::new(AppState::new(
        profile.config.clone(),
        profile.db.clone(),
        Arc::new(NoopEmitter),
        Arc::new(NoopNotifier),
    ))
}

pub fn active_identity_id(profile: &Profile, override_hash: Option<String>) -> String {
    override_hash.unwrap_or_else(|| {
        ratspeak_db::get_active_identity(&profile.db)
            .and_then(|id| {
                id.get("hash")
                    .and_then(|hash| hash.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default()
    })
}

pub fn profile_summary(profile: &Profile) -> Value {
    let active_identity = ratspeak_db::get_active_identity(&profile.db);
    let identities = ratspeak_db::get_all_identities(&profile.db);
    let db_stats = ratspeak_db::get_database_stats(&profile.db);
    let lock_path = ratspeak_runtime::profile_lock::lock_path(&profile.config.data_dir);
    let lock_info = ratspeak_runtime::profile_lock::read_profile_lock(&profile.config.data_dir);
    json!({
        "data_root": profile.data_root,
        "data_dir": profile.config.data_dir,
        "db_path": profile.config.db_path(),
        "profile_lock": {
            "path": lock_path,
            "locked": lock_info.is_some(),
            "owner": lock_info,
        },
        "rns_config_dir": profile.config.rns_config_dir,
        "rns_config_dir_overridden": profile.config.rns_config_dir_overridden,
        "uses_app_private_rns_config_dir": profile.config.uses_app_private_rns_config_dir(),
        "active_identity": active_identity,
        "identity_count": identities.len(),
        "database": db_stats,
    })
}

fn default_desktop_data_root() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return home_dir()
            .join("Library")
            .join("Application Support")
            .join(DESKTOP_IDENTIFIER);
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.trim().is_empty() {
                return PathBuf::from(appdata).join(DESKTOP_IDENTIFIER);
            }
        }
        return home_dir()
            .join("AppData")
            .join("Roaming")
            .join(DESKTOP_IDENTIFIER);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.trim().is_empty() {
                return PathBuf::from(xdg).join(DESKTOP_IDENTIFIER);
            }
        }
        home_dir()
            .join(".local")
            .join("share")
            .join(DESKTOP_IDENTIFIER)
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_data_root_wins() {
        let path = PathBuf::from("/tmp/ratspeak-explicit");
        assert_eq!(resolve_data_root(Some(path.clone())), path);
    }
}
