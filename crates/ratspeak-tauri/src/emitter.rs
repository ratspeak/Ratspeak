//! Tauri-backed `Emitter` impl. Wraps an `AppHandle` and forwards `emit` to
//! the WebView's event bus.

use ratspeak_core::{EmitError, Emitter};

pub struct TauriEmitter {
    handle: tauri::AppHandle,
}

impl TauriEmitter {
    pub fn new(handle: tauri::AppHandle) -> Self {
        Self { handle }
    }
}

impl Emitter for TauriEmitter {
    fn try_emit(&self, event: &str, payload: serde_json::Value) -> Result<(), EmitError> {
        use tauri::Emitter as _;
        // Tauri does not distinguish a useful, non-sensitive rejection class
        // here. Keep its source error out of recorder state and logs.
        self.handle
            .emit(event, &payload)
            .map_err(|_| EmitError::Unavailable)
    }

    fn emit(&self, event: &str, payload: serde_json::Value) {
        if self.try_emit(event, payload).is_err() {
            tracing::warn!(target: "events", reason = "emit_failed", "tauri emit failed");
        }
    }
}
