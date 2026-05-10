//! Ratspeak Tauri IPC surface. The only crate in the workspace that depends
//! on `tauri`. Hosts the `#[tauri::command]` handlers, the `TauriEmitter`
//! impl that wraps `AppHandle::emit`, and the `init_core` entry point that
//! wires everything together.

// Holding `std::sync::MutexGuard` / `RwLockGuard` across `.await` breaks
// `Send` bounds or stalls the executor.
#![warn(clippy::await_holding_lock)]

pub mod commands;
pub mod config;
pub mod emitter;
pub mod error;
pub mod notifier;

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
pub use ratspeak_runtime::{
    announce_handlers, helpers, identity_prune, lxmf, propagation, rns, rns_config, state,
};
pub use ratspeak_runtime::{
    any_interface_online_cached, apply_lxmf_settings_from_state, init_rns_lxmf, restart_rns_lxmf,
    send_announce_from_state, send_manual_announce_from_state, shutdown_rns_lxmf,
};

use std::sync::Arc;

use state::AppState;

/// Init DB pool + schema, AppState, BLE diag relay, then spawn `init_rns_lxmf`.
/// AppHandle is wrapped in a `TauriEmitter` and stashed in AppState before any
/// background task can `emit_to_all`.
pub async fn init_core(
    data_dir: std::path::PathBuf,
    app_handle: tauri::AppHandle,
) -> Result<Arc<AppState>, Box<dyn std::error::Error + Send + Sync>> {
    let config = config::DashboardConfig::from_env_and_defaults(data_dir.clone());

    let data_dir_for_pool = data_dir.clone();
    let db_pool = tokio::task::spawn_blocking(move || db::init_pool(&data_dir_for_pool))
        .await
        .expect("db task panicked")?;
    let pool_for_schema = db_pool.clone();
    db::spawn_db(pool_for_schema, |p| db::init_schema(&p))
        .await
        .expect("db task panicked")?;

    // Bluetooth Peer is never auto-restored; clear any stale flag.
    db::spawn_db(db_pool.clone(), |p| {
        db::set_setting(&p, "ble_peer_enabled", "0")
    })
    .await
    .expect("db task panicked");

    let emitter: Arc<dyn ratspeak_core::Emitter> =
        Arc::new(emitter::TauriEmitter::new(app_handle.clone()));
    let notifier: Arc<dyn ratspeak_core::NativeNotifier> =
        Arc::new(notifier::TauriNotifier::new(app_handle));
    let app_state = Arc::new(AppState::new(config.clone(), db_pool, emitter, notifier));
    app_state.set_startup_stage("checking");

    // Relay BLE diagnostics → `ble_diag` events.
    commands::ble::spawn_ble_diag_broadcaster(&app_state);

    // Relay AutoInterface events → `auto_unavailable` / `auto_carrier_state`.
    commands::interfaces::spawn_auto_event_broadcaster(&app_state);

    let init_state = app_state.clone();
    let init_data_dir = data_dir.clone();
    tokio::spawn(async move {
        init_rns_lxmf(init_state, init_data_dir).await;
    });

    Ok(app_state)
}
