//! Surfaces failed database writes to the UI: one `storage_error` per
//! operation per session, with details the user can copy.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use ratspeak_core::Emitter;
use ratspeak_db::StorageWriteError;

pub(crate) const STORAGE_ERROR_EVENT: &str = "storage_error";
const STORAGE_ERROR_MESSAGE: &str = "Ratspeak couldn't save some changes.";

struct StorageErrorReporter {
    emitter: RwLock<Option<Arc<dyn Emitter>>>,
    reported: Mutex<BTreeSet<&'static str>>,
}

impl StorageErrorReporter {
    const fn new() -> Self {
        Self {
            emitter: RwLock::new(None),
            reported: Mutex::new(BTreeSet::new()),
        }
    }

    /// Starts a session on `emitter`.
    fn attach(&self, emitter: Arc<dyn Emitter>) {
        *self.emitter.write().unwrap_or_else(PoisonError::into_inner) = Some(emitter);
        self.reported
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    fn report(&self, error: &StorageWriteError, unix_secs: u64) {
        if !self
            .reported
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(error.operation)
        {
            return;
        }
        let emitter = self
            .emitter
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(emitter) = emitter {
            emitter.emit(STORAGE_ERROR_EVENT, payload(error, unix_secs));
        }
    }
}

static REPORTER: StorageErrorReporter = StorageErrorReporter::new();

/// Routes failed writes to `emitter`; a later call starts a new session.
pub(crate) fn install(emitter: Arc<dyn Emitter>) {
    REPORTER.attach(emitter);
    ratspeak_db::set_storage_write_error_hook(|error| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        REPORTER.report(&error, now);
    });
}

fn payload(error: &StorageWriteError, unix_secs: u64) -> serde_json::Value {
    let details = format!(
        "{STORAGE_ERROR_MESSAGE}\nOperation: {}\nError: {}\nVersion: {}\nTime: {}",
        error.operation,
        error.message,
        env!("CARGO_PKG_VERSION"),
        utc_timestamp(unix_secs),
    );
    serde_json::json!({
        "message": STORAGE_ERROR_MESSAGE,
        "operation": error.operation,
        "details": details,
    })
}

fn utc_timestamp(unix_secs: u64) -> String {
    let days = i64::try_from(unix_secs / 86_400).unwrap_or(0);
    let secs = unix_secs % 86_400;
    // Civil date from days since 1970-01-01 (Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        secs / 3_600,
        secs / 60 % 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use ratspeak_core::EmitError;
    use serde_json::Value;

    use super::*;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<(String, Value)>>);

    impl Emitter for Recorder {
        fn try_emit(&self, event: &str, payload: Value) -> Result<(), EmitError> {
            self.0.lock().unwrap().push((event.to_owned(), payload));
            Ok(())
        }
    }

    fn failure(operation: &'static str) -> StorageWriteError {
        StorageWriteError {
            operation,
            message: "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint"
                .into(),
        }
    }

    #[test]
    fn each_operation_reaches_the_ui_once_per_session() {
        let reporter = StorageErrorReporter::new();
        let first = Arc::new(Recorder::default());
        reporter.attach(first.clone());

        reporter.report(&failure("touch_identity_activity_updates"), 1_763_089_965);
        reporter.report(&failure("touch_identity_activity_updates"), 1_763_089_966);
        reporter.report(&failure("save_reaction"), 1_763_089_967);

        let events = first.0.lock().unwrap();
        assert_eq!(events.len(), 2);
        let (event, payload) = &events[0];
        assert_eq!(event, STORAGE_ERROR_EVENT);
        assert_eq!(payload["message"], "Ratspeak couldn't save some changes.");
        assert_eq!(payload["operation"], "touch_identity_activity_updates");
        assert_eq!(
            payload["details"],
            format!(
                "Ratspeak couldn't save some changes.\n\
                 Operation: touch_identity_activity_updates\n\
                 Error: ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint\n\
                 Version: {}\n\
                 Time: 2025-11-14 03:12:45 UTC",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert_eq!(events[1].1["operation"], "save_reaction");
        drop(events);

        let second = Arc::new(Recorder::default());
        reporter.attach(second.clone());
        reporter.report(&failure("touch_identity_activity_updates"), 1_763_089_968);
        assert_eq!(second.0.lock().unwrap().len(), 1);
        assert_eq!(first.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn timestamps_are_utc_calendar_dates() {
        assert_eq!(utc_timestamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc_timestamp(1_798_761_599), "2026-12-31 23:59:59 UTC");
    }
}
