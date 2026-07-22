//! Tauri-backed `Emitter` impl. Wraps an `AppHandle` and forwards `emit` to
//! the WebView's event bus.

use ratspeak_core::Emitter;

pub struct TauriEmitter {
    handle: tauri::AppHandle,
}

impl TauriEmitter {
    pub fn new(handle: tauri::AppHandle) -> Self {
        Self { handle }
    }
}

impl Emitter for TauriEmitter {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        use tauri::Emitter as _;
        if self.handle.emit(event, &payload).is_err() {
            tracing::warn!(target: "events", reason = "emit_failed", "tauri emit failed");
        }
    }
}
