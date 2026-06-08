//! Tauri-free runtime bootstrap helpers.

use std::path::PathBuf;
use std::sync::Arc;

use ratspeak_core::{Emitter, NativeNotifier};

use crate::config::DashboardConfig;
use crate::state::AppState;

pub async fn init_headless(
    data_root: PathBuf,
    emitter: Arc<dyn Emitter>,
    notifier: Arc<dyn NativeNotifier>,
) -> Result<Arc<AppState>, Box<dyn std::error::Error + Send + Sync>> {
    std::fs::create_dir_all(&data_root)?;
    let config = DashboardConfig::from_env_and_defaults(data_root.clone());

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
    tokio::spawn(async move {
        crate::init_rns_lxmf(init_state, data_root).await;
    });

    Ok(state)
}
