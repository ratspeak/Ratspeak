//! Tauri-free runtime bootstrap helpers.

use std::path::PathBuf;
use std::sync::Arc;

use ratspeak_core::{Emitter, NativeNotifier};

use crate::config::DashboardConfig;
use crate::state::AppState;

/// Reticulum instance policy for a headless daemon. Defaults to a Standalone
/// instance (no machine-local rendezvous), which is the isolated default for a
/// CLI bot.
#[derive(Debug, Clone, Default)]
pub struct HeadlessRnsPolicy {
    pub share_instance: bool,
    pub instance_name: Option<String>,
}

pub async fn init_headless(
    data_root: PathBuf,
    emitter: Arc<dyn Emitter>,
    notifier: Arc<dyn NativeNotifier>,
    policy: HeadlessRnsPolicy,
) -> Result<Arc<AppState>, Box<dyn std::error::Error + Send + Sync>> {
    std::fs::create_dir_all(&data_root)?;
    let config = DashboardConfig::from_env_and_defaults(data_root.clone())
        .with_headless_rns_policy(policy.share_instance, policy.instance_name);

    let pool_root = data_root.clone();
    let db = tokio::task::spawn_blocking(move || crate::db::init_pool(&pool_root))
        .await
        .map_err(|e| std::io::Error::other(format!("database task panicked: {e}")))??;

    let schema_db = db.clone();
    tokio::task::spawn_blocking(move || crate::db::init_schema(&schema_db))
        .await
        .map_err(|e| std::io::Error::other(format!("schema task panicked: {e}")))??;

    let state = Arc::new(AppState::new(config, db, emitter, notifier));
    state.set_startup_stage("checking");

    let init_state = state.clone();
    let init_handle = tokio::spawn(async move {
        crate::init_rns_lxmf(init_state, data_root).await;
    });

    // Supervise the detached init: a panic here would otherwise leave the daemon
    // up with the API answering while startup never completes. Surface it and
    // mark the startup stage failed so clients can detect the dead runtime.
    let watch_state = state.clone();
    tokio::spawn(async move {
        if let Err(err) = init_handle.await {
            tracing::error!(error = %err, "Ratspeak runtime init task terminated abnormally");
            watch_state.set_startup_stage("failed");
        }
    });

    Ok(state)
}
