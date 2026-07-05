use std::io::Write;
use std::sync::{Arc, Mutex};

use ratspeak_core::{Emitter, NativeNotification, NativeNotifier};
use ratspeak_runtime::state::AppState;
use serde_json::{Value, json};

use crate::error::{CliError, CliResult};
use crate::event_store::EventStore;

pub async fn init_headless_runtime(
    data_root: std::path::PathBuf,
    emit_jsonl: bool,
    policy: ratspeak_runtime::bootstrap::HeadlessRnsPolicy,
) -> CliResult<Arc<AppState>> {
    let event_store = EventStore::open(data_root.clone())?;
    let (emitter, notifier): (Arc<dyn Emitter>, Arc<dyn NativeNotifier>) = {
        let sink = Arc::new(HeadlessEventSink::new(event_store, emit_jsonl));
        (sink.clone(), sink)
    };

    ratspeak_runtime::bootstrap::init_headless(data_root, emitter, notifier, policy)
        .await
        .map_err(|e| CliError::failed(format!("failed to initialize Ratspeak runtime: {e}")))
}

struct HeadlessEventSink {
    event_store: Arc<EventStore>,
    emit_jsonl: bool,
    stdout_lock: Mutex<()>,
}

impl HeadlessEventSink {
    fn new(event_store: Arc<EventStore>, emit_jsonl: bool) -> Self {
        Self {
            event_store,
            emit_jsonl,
            stdout_lock: Mutex::new(()),
        }
    }

    fn write(&self, value: Value) {
        if !self.emit_jsonl {
            return;
        }
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

impl Emitter for HeadlessEventSink {
    fn emit(&self, event: &str, payload: Value) {
        match self
            .event_store
            .append_emitter_event(event, payload.clone())
        {
            Ok(record) => self.write(json!(record)),
            Err(error) => {
                tracing::warn!(%error, event, "failed to persist headless event");
                self.write(json!({
                    "type": "event",
                    "event": event,
                    "payload": payload,
                }));
            }
        }
    }
}

impl NativeNotifier for HeadlessEventSink {
    fn notify(&self, notification: NativeNotification) {
        let payload = json!({
            "type": "notification",
            "kind": format!("{:?}", notification.kind),
            "title": notification.title,
            "body": notification.body,
            "thread_id": notification.thread_id,
            "notification_id": notification.notification_id,
        });
        match self.event_store.append_notification(payload.clone()) {
            Ok(record) => self.write(json!(record)),
            Err(error) => {
                tracing::warn!(%error, "failed to persist headless notification");
                self.write(payload);
            }
        }
    }
}
