//! IPC abstraction. The runtime emits events through this trait so it stays
//! free of any `tauri` dependency. The concrete `TauriEmitter` lives in
//! `ratspeak-tauri` and wraps `AppHandle::emit`.

use serde_json::Value;

/// Non-sensitive failure classification for an event-bus enqueue attempt.
///
/// Variants deliberately carry no source error or payload context so callers
/// can report recorder health without retaining application data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EmitError {
    #[error("event emit was rejected")]
    Rejected,
    #[error("event emitter is unavailable")]
    Unavailable,
}

pub trait Emitter: Send + Sync {
    /// Attempts to enqueue an event for broadcast.
    fn try_emit(&self, event: &str, payload: Value) -> Result<(), EmitError>;

    /// Best-effort compatibility adapter for existing callers.
    fn emit(&self, event: &str, payload: Value) {
        let _ = self.try_emit(event, payload);
    }
}

/// Drops every emit. Useful for headless tests where there's no IPC peer.
pub struct NoopEmitter;

impl Emitter for NoopEmitter {
    fn try_emit(&self, _event: &str, _payload: Value) -> Result<(), EmitError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct RejectingEmitter {
        attempts: AtomicUsize,
    }

    impl Emitter for RejectingEmitter {
        fn try_emit(&self, _event: &str, _payload: Value) -> Result<(), EmitError> {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            Err(EmitError::Rejected)
        }
    }

    #[test]
    fn noop_try_emit_succeeds() {
        assert_eq!(NoopEmitter.try_emit("ignored", Value::Null), Ok(()));
    }

    #[test]
    fn best_effort_adapter_attempts_and_suppresses_failure() {
        let emitter = RejectingEmitter {
            attempts: AtomicUsize::new(0),
        };

        emitter.emit("test", Value::Null);

        assert_eq!(emitter.attempts.load(Ordering::Relaxed), 1);
    }
}
