use std::io::Write;
use std::sync::{Arc, Mutex};

use ratspeak_core::{Emitter, NativeNotification, NativeNotifier, NoopEmitter, NoopNotifier};
use ratspeak_runtime::state::AppState;
use serde_json::{Value, json};

use crate::error::{CliError, CliResult};

pub async fn init_headless_runtime(
    data_root: std::path::PathBuf,
    emit_jsonl: bool,
) -> CliResult<Arc<AppState>> {
    let (emitter, notifier): (Arc<dyn Emitter>, Arc<dyn NativeNotifier>) = if emit_jsonl {
        let sink = Arc::new(JsonlSink::default());
        (sink.clone(), sink)
    } else {
        (Arc::new(NoopEmitter), Arc::new(NoopNotifier))
    };

    ratspeak_runtime::bootstrap::init_headless(data_root, emitter, notifier)
        .await
        .map_err(|e| CliError::failed(format!("failed to initialize Ratspeak runtime: {e}")))
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
