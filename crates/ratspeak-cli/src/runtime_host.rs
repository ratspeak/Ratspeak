use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ratspeak_core::{Emitter, NativeNotification, NativeNotifier, NoopEmitter, NoopNotifier};
use ratspeak_runtime::config::DashboardConfig;
use ratspeak_runtime::state::AppState;
use serde_json::{Value, json};

use crate::error::{CliError, CliResult};

pub async fn init_headless_runtime(
    data_root: PathBuf,
    emit_jsonl: bool,
) -> CliResult<Arc<AppState>> {
    std::fs::create_dir_all(&data_root)?;
    let config = DashboardConfig::from_env_and_defaults(data_root.clone());

    let pool_root = data_root.clone();
    let db = tokio::task::spawn_blocking(move || ratspeak_db::init_pool(&pool_root))
        .await
        .map_err(|e| CliError::failed(format!("database task panicked: {e}")))?
        .map_err(|e| CliError::failed(format!("failed to open Ratspeak database: {e}")))?;

    let schema_db = db.clone();
    tokio::task::spawn_blocking(move || ratspeak_db::init_schema(&schema_db))
        .await
        .map_err(|e| CliError::failed(format!("schema task panicked: {e}")))?
        .map_err(|e| CliError::failed(format!("failed to initialize Ratspeak schema: {e}")))?;

    let (emitter, notifier): (Arc<dyn Emitter>, Arc<dyn NativeNotifier>) = if emit_jsonl {
        let sink = Arc::new(JsonlSink::default());
        (sink.clone(), sink)
    } else {
        (Arc::new(NoopEmitter), Arc::new(NoopNotifier))
    };

    let state = Arc::new(AppState::new(config, db, emitter, notifier));
    state.set_startup_stage("checking");

    let init_state = state.clone();
    tokio::spawn(async move {
        ratspeak_runtime::init_rns_lxmf(init_state, data_root).await;
    });

    Ok(state)
}

#[derive(Default)]
struct JsonlSink {
    stdout_lock: Mutex<()>,
}

impl JsonlSink {
    fn write(&self, value: Value) {
        let Ok(_guard) = self.stdout_lock.lock() else {
            return;
        };
        let mut stdout = std::io::stdout().lock();
        if serde_json::to_writer(&mut stdout, &value).is_ok() {
            let _ = stdout.write_all(b"\n");
            let _ = stdout.flush();
        }
    }
}

impl Emitter for JsonlSink {
    fn emit(&self, event: &str, payload: Value) {
        self.write(json!({
            "type": "event",
            "event": event,
            "payload": payload,
        }));
    }
}

impl NativeNotifier for JsonlSink {
    fn notify(&self, notification: NativeNotification) {
        self.write(json!({
            "type": "notification",
            "kind": format!("{:?}", notification.kind),
            "title": notification.title,
            "body": notification.body,
            "thread_id": notification.thread_id,
            "notification_id": notification.notification_id,
        }));
    }
}
