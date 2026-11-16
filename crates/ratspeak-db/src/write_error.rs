//! Reporting for failed writes whose callers keep a fallback value.

use std::fmt::Display;
use std::sync::OnceLock;

/// A database write that failed. `operation` names the storage function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageWriteError {
    pub operation: &'static str,
    pub message: String,
}

type WriteErrorHook = Box<dyn Fn(StorageWriteError) + Send + Sync>;

static HOOK: OnceLock<WriteErrorHook> = OnceLock::new();

/// Installs the process-wide receiver of failed writes. The first one stays;
/// false when a hook was already installed.
pub fn set_storage_write_error_hook(
    hook: impl Fn(StorageWriteError) + Send + Sync + 'static,
) -> bool {
    HOOK.set(Box::new(hook)).is_ok()
}

/// Logs a failed write and hands it to the installed hook.
pub fn report_write_error(operation: &'static str, error: impl Display) {
    tracing::error!(operation, %error, "database write failed");
    if let Some(hook) = HOOK.get() {
        hook(StorageWriteError {
            operation,
            message: error.to_string(),
        });
    }
}

pub(crate) trait OrReport<T> {
    /// `Result::ok` that reports the error as a failed write.
    fn or_report(self, operation: &'static str) -> Option<T>;
}

impl<T, E: Display> OrReport<T> for Result<T, E> {
    fn or_report(self, operation: &'static str) -> Option<T> {
        self.map_err(|error| report_write_error(operation, error))
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use r2d2_sqlite::SqliteConnectionManager;

    use super::*;
    use crate::db::{
        DbPool, IdentityActivityUpdate, PEER_SERVICE_LXMF_DELIVERY, touch_identity_activity_updates,
    };

    static REPORTED: Mutex<Vec<StorageWriteError>> = Mutex::new(Vec::new());

    #[test]
    fn a_failed_peer_upsert_reaches_the_hook_and_keeps_its_fallback() {
        assert!(set_storage_write_error_hook(|error| {
            REPORTED.lock().unwrap().push(error);
        }));
        let pool: DbPool = r2d2::Pool::builder()
            .max_size(1)
            .build(SqliteConnectionManager::memory())
            .unwrap();
        // The bad profile keyed peers by more than dest_hash.
        pool.get()
            .unwrap()
            .execute_batch(
                "CREATE TABLE identity_activity (
                     dest_hash      TEXT NOT NULL,
                     identity_hash  TEXT NOT NULL DEFAULT '',
                     last_seen      REAL NOT NULL,
                     first_seen     REAL NOT NULL,
                     announce_count INTEGER NOT NULL DEFAULT 1,
                     display_name   TEXT NOT NULL DEFAULT '',
                     status         TEXT NOT NULL DEFAULT '',
                     last_interface TEXT NOT NULL DEFAULT '',
                     services       TEXT NOT NULL DEFAULT '',
                     lxmf_compression_support TEXT NOT NULL DEFAULT '',
                     announce_emitted INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (dest_hash, identity_hash)
                 );",
            )
            .unwrap();

        let touched = touch_identity_activity_updates(
            &pool,
            &[IdentityActivityUpdate {
                dest_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                timestamp: 100.0,
                display_name: Some("Alice".into()),
                status: None,
                last_interface: None,
                identity_hash: Some("11111111111111111111111111111111".into()),
                services: vec![PEER_SERVICE_LXMF_DELIVERY.into()],
                clear_ratspeak_services: false,
                lxmf_compression_support: None,
                announce_emitted: None,
            }],
        );

        assert_eq!(touched, 0);
        let reported = REPORTED.lock().unwrap();
        assert!(
            reported.iter().any(|error| {
                error.operation == "touch_identity_activity_updates"
                    && error.message.contains(
                        "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint",
                    )
            }),
            "{reported:?}"
        );
    }
}
