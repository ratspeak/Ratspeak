//! Ratspeak Tauri IPC surface. The only crate in the workspace that depends
//! on `tauri`. Hosts the `#[tauri::command]` handlers, the `TauriEmitter`
//! impl that wraps `AppHandle::emit`, and the `init_core` entry point that
//! wires everything together.

// Holding `std::sync::MutexGuard` / `RwLockGuard` across `.await` breaks
// `Send` bounds or stalls the executor.
#![warn(clippy::await_holding_lock)]

pub mod commands;
pub mod config;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod diagnostic_writer;
pub mod diagnostics;
pub mod emitter;
pub mod error;
pub mod invoke_gate;
pub mod notifier;
pub mod process;
pub mod window_prefs;

// Re-export runtime modules so internal `crate::*` paths in commands keep
// resolving after the runtime/Tauri split.
pub use ratspeak_db as db;
pub use ratspeak_db::static_nodes;
#[cfg(target_os = "ios")]
pub use ratspeak_runtime::platform_ios;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub use ratspeak_runtime::shutdown_ble_peer_for_exit;
#[cfg(feature = "lxst-voice")]
pub use ratspeak_runtime::voice;
#[cfg(feature = "lxst-voice")]
pub use ratspeak_runtime::voice_memo;
pub use ratspeak_runtime::{
    ActivityReset, AnnounceSendDisposition, any_interface_online_cached,
    apply_lxmf_settings_from_state, finish_shutdown, init_rns_lxmf,
    maybe_opportunistic_announce_before_user_send,
    maybe_opportunistic_announce_before_user_send_from_origin, reset_activity_for_shutdown,
    restart_rns_lxmf, send_announce_from_origin, send_announce_from_state,
    send_manual_announce_from_origin, send_manual_announce_from_state,
    send_typed_announce_from_origin, shutdown_rns_lxmf, start_channel_hub_service,
};
pub use ratspeak_runtime::{
    announce, announce_handlers, channel_hub, channels, helpers, identity_prune, lxmf,
    mobile_platform, propagation, rns, rns_config, state,
};

use std::sync::Arc;

use state::AppState;

/// Why the core could not start. The shell then manages no AppState, so no
/// command can run, and the window shows a static screen for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum StartupError {
    /// Another app or process owns this profile.
    ProfileInUse,
    /// The database was written by a newer Ratspeak.
    SchemaNewer { found: i64, supported: i64 },
    /// The profile's storage or database could not be opened.
    Storage,
}

/// A failed start: the screen to show, and the failing step and I/O error kind
/// for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupFailure {
    pub error: StartupError,
    pub step: &'static str,
    pub kind: Option<std::io::ErrorKind>,
}

impl StartupFailure {
    fn at(step: &'static str, error: &(dyn std::error::Error + 'static)) -> Self {
        let io = error.downcast_ref::<std::io::Error>();
        let cause: &(dyn std::error::Error + 'static) = match io.and_then(std::io::Error::get_ref) {
            Some(inner) => inner,
            None => error,
        };
        let error = if cause.is::<ratspeak_runtime::owner_lock::ProfileInUse>() {
            StartupError::ProfileInUse
        } else if let Some(newer) = cause.downcast_ref::<db::NewerSchema>() {
            StartupError::SchemaNewer {
                found: newer.found,
                supported: newer.supported,
            }
        } else {
            StartupError::Storage
        };
        Self {
            error,
            step,
            kind: io.map(std::io::Error::kind),
        }
    }

    /// A step whose blocking task did not finish.
    fn task(step: &'static str) -> Self {
        Self {
            error: StartupError::Storage,
            step,
            kind: None,
        }
    }
}

/// Open the profile (storage, DB pool + schema), AppState, BLE diag relay, then
/// spawn `init_rns_lxmf`. AppHandle is wrapped in a `TauriEmitter` and stashed
/// in AppState before any background task can `emit_to_all`. `default_root`:
/// `data_dir` is the platform's data folder, not one the user named, and is
/// kept private on the desktop.
pub async fn init_core(
    data_dir: std::path::PathBuf,
    default_root: bool,
    app_handle: tauri::AppHandle,
) -> Result<Arc<AppState>, StartupFailure> {
    let (config, db_pool) = open_profile(data_dir.clone(), default_root).await?;
    // Pre-1.1 "!Ratspeak.org-" defaults become Verb Animal before anything loads or announces them.
    match db::spawn_db(db_pool.clone(), |p| {
        db::rename_placeholder_identity_names(&p)
    })
    .await
    {
        Ok(Ok(0)) => {}
        Ok(Ok(renamed)) => tracing::info!(
            renamed,
            reason = "placeholder_names",
            "renamed default identity names"
        ),
        _ => tracing::warn!(
            reason = "placeholder_names",
            "could not rename default identity names"
        ),
    }

    let emitter: Arc<dyn ratspeak_core::Emitter> =
        Arc::new(emitter::TauriEmitter::new(app_handle.clone()));
    let notifier: Arc<dyn ratspeak_core::NativeNotifier> =
        Arc::new(notifier::TauriNotifier::new(app_handle));
    let app_state = Arc::new(AppState::new(config, db_pool, emitter, notifier));
    app_state.set_startup_stage("checking");

    // Relay typed BLE pairing/product events. Raw ble_diag strings are never
    // forwarded across IPC.
    commands::ble::spawn_ble_event_broadcaster(&app_state);

    // Relay AutoInterface events → `auto_unavailable` / `auto_carrier_state`.
    commands::interfaces::spawn_auto_event_broadcaster(&app_state);

    // Saved connections Reticulum detached after a failed verification come back.
    commands::interfaces::spawn_interface_reattacher(&app_state);

    let init_state = app_state.clone();
    let init_data_dir = data_dir.clone();
    tokio::spawn(async move {
        {
            let _identity_lifecycle = init_state.identity_switch_lock.lock().await;
            init_rns_lxmf(Arc::clone(&init_state), init_data_dir).await;
        }
        commands::ble::restore_ble_peer_if_requested(init_state).await;
    });

    Ok(app_state)
}

/// Take the profile and open its database. Nothing is reconciled in a profile
/// another process owns, and nothing is written to a newer Ratspeak's database.
async fn open_profile(
    data_dir: std::path::PathBuf,
    default_root: bool,
) -> Result<(config::DashboardConfig, db::DbPool), StartupFailure> {
    // Windows may need to publish a fresh sibling during crash reconciliation.
    // Establish roots before configuration or SQLite can hold live handles.
    let storage_root = data_dir.clone();
    tokio::task::spawn_blocking(move || {
        lxmf::initialize_storage_namespace(&storage_root)?;
        if default_root && cfg!(not(any(target_os = "android", target_os = "ios"))) {
            lxmf::make_private(&storage_root)?;
        }
        Ok::<_, std::io::Error>(())
    })
    .await
    .map_err(|_| StartupFailure::task("storage"))?
    .map_err(|error| StartupFailure::at("storage", &error))?;
    let config = config::DashboardConfig::from_env_and_defaults(data_dir.clone());

    let db_pool = tokio::task::spawn_blocking(move || db::init_pool(&data_dir))
        .await
        .map_err(|_| StartupFailure::task("database_pool"))?
        .map_err(|error| StartupFailure::at("database_pool", &*error))?;
    db::spawn_db(db_pool.clone(), |p| db::init_schema(&p))
        .await
        .map_err(|_| StartupFailure::task("database_schema"))?
        .map_err(|error| StartupFailure::at("database_schema", &*error))?;
    Ok((config, db_pool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_profile_another_process_owns_is_reported_in_use() {
        let root = tempfile::tempdir().unwrap();
        let other = std::fs::File::create(root.path().join(".ratspeak-host.lock")).unwrap();
        rns_identity::persistence::try_lock_exclusive(&other).unwrap();

        let failure = open_profile(root.path().to_path_buf(), false)
            .await
            .unwrap_err();
        assert_eq!(failure.error, StartupError::ProfileInUse);
        assert_eq!(
            (failure.step, failure.kind),
            ("storage", Some(std::io::ErrorKind::ResourceBusy))
        );
        assert!(!root.path().join(".ratspeak").exists());
    }

    #[tokio::test]
    async fn a_database_from_a_newer_build_is_reported_with_both_versions() {
        let root = tempfile::tempdir().unwrap();
        lxmf::initialize_storage_namespace(root.path()).unwrap();
        let pool = db::init_pool(root.path()).unwrap();
        db::init_schema(&pool).unwrap();
        let supported: i64 = pool
            .get()
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        pool.get()
            .unwrap()
            .execute("UPDATE schema_version SET version = ?1", [supported + 1])
            .unwrap();
        drop(pool);

        let failure = open_profile(root.path().to_path_buf(), false)
            .await
            .unwrap_err();
        assert_eq!(
            failure.error,
            StartupError::SchemaNewer {
                found: supported + 1,
                supported
            }
        );
        assert_eq!(failure.step, "storage");
    }

    #[tokio::test]
    async fn a_newer_database_is_refused_before_the_pool_opens_it() {
        let root = tempfile::tempdir().unwrap();
        lxmf::initialize_storage_namespace(root.path()).unwrap();
        db::init_schema(&db::init_pool(root.path()).unwrap()).unwrap();
        let path = root.path().join(".ratspeak/ratspeak.db");
        let supported: i64 = {
            // A newer build that left WAL mode: the pool would convert it back.
            let conn = rusqlite::Connection::open(&path).unwrap();
            let mode: String = conn
                .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
                .unwrap();
            assert_eq!(mode, "delete");
            let supported = conn
                .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
                .unwrap();
            conn.execute("UPDATE schema_version SET version = ?1", [supported + 1])
                .unwrap();
            supported
        };
        let bytes = std::fs::read(&path).unwrap();

        let failure = open_profile(root.path().to_path_buf(), false)
            .await
            .unwrap_err();

        assert!(
            std::fs::read(&path).unwrap() == bytes,
            "the newer database was written"
        );
        assert_eq!(
            failure.error,
            StartupError::SchemaNewer {
                found: supported + 1,
                supported
            }
        );
    }

    // The platform's own data folder is made private; one RATSPEAK_DATA_DIR
    // names keeps the access its owner gave it.
    #[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
    #[tokio::test]
    async fn only_the_default_data_folder_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        for default_root in [true, false] {
            let root = tempfile::tempdir().unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

            open_profile(root.path().to_path_buf(), default_root)
                .await
                .unwrap();

            let mode = std::fs::metadata(root.path()).unwrap().permissions().mode();
            let expected = if default_root { 0o700 } else { 0o755 };
            assert_eq!(mode & 0o777, expected, "default root: {default_root}");
        }
    }

    #[tokio::test]
    async fn storage_that_cannot_hold_a_profile_is_a_storage_error() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("profile");
        std::fs::write(&file, b"not a directory").unwrap();

        let failure = open_profile(file, false).await.unwrap_err();
        assert_eq!(failure.error, StartupError::Storage);
        assert_eq!(
            (failure.step, failure.kind),
            ("storage", Some(std::io::ErrorKind::InvalidInput))
        );
    }
}
