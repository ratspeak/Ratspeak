use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratspeak_core::{NoopEmitter, NoopNotifier};
use ratspeak_runtime::config::DashboardConfig;
use ratspeak_runtime::state::AppState;
use serde_json::{Value, json};

use crate::error::{CliError, CliResult};

const DESKTOP_IDENTIFIER: &str = "org.ratspeak.desktop";
const CLI_IDENTIFIER: &str = "org.ratspeak.cli";

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
    default_cli_data_root()
}

pub fn open_profile(data_root: PathBuf) -> CliResult<Profile> {
    open_profile_with_pool_size(data_root, None)
}

pub fn open_profile_with_pool_size(
    data_root: PathBuf,
    pool_size: Option<u32>,
) -> CliResult<Profile> {
    std::fs::create_dir_all(&data_root)?;
    let config = DashboardConfig::from_env_and_defaults(data_root.clone());
    let db = match pool_size {
        Some(max_size) => ratspeak_db::init_pool_with_max_size(&data_root, max_size),
        None => ratspeak_db::init_pool(&data_root),
    }
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
    let locked = ratspeak_runtime::profile_lock::is_profile_locked(&profile.config.data_dir);
    json!({
        "data_root": profile.data_root,
        "data_dir": profile.config.data_dir,
        "db_path": profile.config.db_path(),
        "profile_lock": {
            "path": lock_path,
            "locked": locked,
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

/// Default data root for the headless CLI/daemon. Deliberately distinct from
/// the desktop app so a bare `ratspeakd`/`ratspeakctl` never co-owns the GUI
/// profile's database, identity, or Reticulum config.
fn default_cli_data_root() -> PathBuf {
    default_data_root_for(CLI_IDENTIFIER)
}

/// Data root of the desktop GUI app. A headless daemon must never silently
/// co-own this profile; `ratspeakd run` refuses it without `--force`.
pub fn desktop_app_data_root() -> PathBuf {
    default_data_root_for(DESKTOP_IDENTIFIER)
}

/// True when `path` resolves to the desktop app's data root.
pub fn is_desktop_app_root(path: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canonical(path) == canonical(&desktop_app_data_root())
}

fn default_data_root_for(identifier: &str) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return home_dir()
            .join("Library")
            .join("Application Support")
            .join(identifier);
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.trim().is_empty() {
                return PathBuf::from(appdata).join(identifier);
            }
        }
        return home_dir().join("AppData").join("Roaming").join(identifier);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.trim().is_empty() {
                return PathBuf::from(xdg).join(identifier);
            }
        }
        home_dir().join(".local").join("share").join(identifier)
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

    #[test]
    fn cli_default_root_is_distinct_from_desktop() {
        let cli = default_cli_data_root();
        let desktop = desktop_app_data_root();
        assert_ne!(cli, desktop, "CLI must not default into the desktop profile");
        assert!(cli.ends_with(CLI_IDENTIFIER));
        assert!(desktop.ends_with(DESKTOP_IDENTIFIER));
    }

    #[test]
    fn desktop_root_detection() {
        assert!(is_desktop_app_root(&desktop_app_data_root()));
        assert!(!is_desktop_app_root(&default_cli_data_root()));
        assert!(!is_desktop_app_root(&PathBuf::from("/tmp/some-bot-profile")));
    }
}
