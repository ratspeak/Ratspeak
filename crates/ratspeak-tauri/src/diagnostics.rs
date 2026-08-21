//! Process diagnostics policy shared by the Tauri shell and tests.
//!
//! `RUST_LOG` may select levels only within this immutable allowlist. The app
//! links protocol crates into the same process, so an `EnvFilter` alone would
//! otherwise let an opt-in diagnostics session capture dependency paths,
//! endpoints, identifiers, or payload-adjacent error text.

const ALLOWED_TARGET_ROOTS: &[&str] = &[
    "ratspeak",
    "ratspeak_lib",
    "ratspeak_core",
    "ratspeak_db",
    "ratspeak_runtime",
    "ratspeak_tauri",
];

const ALLOWED_EXPLICIT_TARGETS: &[&str] = &["events", "lrgp_trace", "ttt_trace"];

/// Exact lower-layer target whose schema is intentionally safe for local
/// qualification logs. Raw `ble_diag` remains excluded: it can contain a
/// peripheral identifier, display name, URI, or platform error text.
const SAFE_BLE_LIFECYCLE_TARGET: &str = "rns_interface::ble_rnode::lifecycle";

/// Immutable field schema for [`SAFE_BLE_LIFECYCLE_TARGET`]. Values are
/// emitted by rsReticulum from closed enums/static tokens, one
/// process-ephemeral generation number, and characteristic-property booleans.
const SAFE_BLE_LIFECYCLE_FIELDS: &[&str] = &[
    "message",
    "generation",
    "stage",
    "result_class",
    "tx_read",
    "tx_notify",
];

// Metadata-level defense in depth. Values for these field names are never
// visited by a diagnostics subscriber, even if a future callsite is added
// under an otherwise allowed Ratspeak target.
const PROHIBITED_FIELD_NAMES: &[&str] = &[
    "app_id",
    "backup",
    "command",
    "content",
    "endpoint",
    "error",
    "err",
    "event",
    "fallback",
    "file_name",
    "greeting",
    "interface",
    "label",
    "nickname",
    "passcode",
    "path",
    "payload",
    "pin",
    "private_key",
    "public_key",
    "response",
    "secret",
    "session_id",
    "stored",
    "stored_name",
    "title",
    "token",
    "topic",
    "uri",
    "url",
];

/// Whether a tracing event or span target may reach Ratspeak's process-wide
/// diagnostics subscriber.
pub fn target_allowed(target: &str) -> bool {
    target == SAFE_BLE_LIFECYCLE_TARGET
        || ALLOWED_EXPLICIT_TARGETS.contains(&target)
        || ALLOWED_TARGET_ROOTS.iter().any(|root| {
            target == *root
                || target
                    .strip_prefix(root)
                    .is_some_and(|suffix| suffix.starts_with("::"))
        })
}

fn safe_ble_lifecycle_metadata(metadata: &tracing::Metadata<'_>) -> bool {
    if metadata.target() != SAFE_BLE_LIFECYCLE_TARGET {
        return false;
    }

    let fields = metadata.fields();
    fields.len() == SAFE_BLE_LIFECYCLE_FIELDS.len()
        && fields
            .iter()
            .all(|field| SAFE_BLE_LIFECYCLE_FIELDS.contains(&field.name()))
}

/// Apply both the immutable target boundary and the structured-field privacy
/// boundary before a subscriber is allowed to record an event or span.
pub fn metadata_allowed(metadata: &tracing::Metadata<'_>) -> bool {
    let target_is_allowed = if metadata.target() == SAFE_BLE_LIFECYCLE_TARGET {
        safe_ble_lifecycle_metadata(metadata)
    } else {
        target_allowed(metadata.target())
    };

    target_is_allowed
        && metadata
            .fields()
            .iter()
            .all(|field| !PROHIBITED_FIELD_NAMES.contains(&field.name()))
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{metadata_allowed, target_allowed};

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("diagnostic test writer").extend(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn prohibited_value(visits: &AtomicUsize) -> &'static str {
        visits.fetch_add(1, Ordering::SeqCst);
        "side-effect-canary"
    }

    #[test]
    fn accepts_only_reviewed_ratspeak_roots_and_explicit_targets() {
        for target in [
            "ratspeak",
            "ratspeak_lib::mobile",
            "ratspeak_core::emitter",
            "ratspeak_db::db",
            "ratspeak_runtime::channels",
            "ratspeak_tauri::commands::network",
            "events",
            "lrgp_trace",
            "ttt_trace",
            "rns_interface::ble_rnode::lifecycle",
        ] {
            assert!(target_allowed(target), "expected {target} to be allowed");
        }
    }

    #[test]
    fn rejects_dependency_and_prefix_lookalike_targets() {
        for target in [
            "rns_interface",
            "rns_interface::ble_rnode",
            "rns_interface::ble_rnode::lifecycle::nested",
            "rns_transport",
            "lxmf_core::router",
            "lxst_telephony",
            "lrgp",
            "ble_diag",
            "tokio",
            "ratspeak_runtime_evil",
            "ratspeak_libextra",
            "events::nested",
        ] {
            assert!(!target_allowed(target), "expected {target} to be rejected");
        }
    }

    #[test]
    fn ble_lifecycle_target_requires_the_exact_reviewed_field_schema() {
        use tracing_subscriber::filter::filter_fn;
        use tracing_subscriber::layer::SubscriberExt;

        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let subscriber = tracing_subscriber::registry()
            .with(filter_fn(metadata_allowed))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .with_writer(move || writer.clone()),
            );

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "rns_interface::ble_rnode::lifecycle",
                generation = 2_u64,
                stage = "subscribe",
                result_class = "target_disconnected",
                tx_read = false,
                tx_notify = true,
                "BLE RNode generation lifecycle"
            );
            tracing::info!(
                target: "rns_interface::ble_rnode::lifecycle",
                generation = 2_u64,
                stage = "subscribe",
                result_class = "stage_failed",
                tx_read = false,
                tx_notify = true,
                peripheral_id = "identifier-canary",
                "hostile schema extension"
            );
            tracing::info!(
                target: "rns_interface::ble_rnode::lifecycle",
                generation = 2_u64,
                stage = "subscribe",
                result_class = "stage_failed",
                tx_read = false,
                "incomplete schema"
            );
        });

        let output = String::from_utf8(captured.lock().expect("captured output").clone())
            .expect("UTF-8 diagnostics output");
        assert!(output.contains("target_disconnected"));
        assert!(!output.contains("identifier-canary"));
        assert!(!output.contains("hostile schema extension"));
        assert!(!output.contains("incomplete schema"));
    }

    #[test]
    fn hostile_env_filter_cannot_reenable_excluded_targets_or_spans() {
        use tracing_subscriber::EnvFilter;
        use tracing_subscriber::filter::filter_fn;
        use tracing_subscriber::layer::SubscriberExt;

        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new(
                "trace,rns_interface=trace,lxmf_core=trace,ble_diag=trace",
            ))
            .with(filter_fn(metadata_allowed))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .with_writer(move || writer.clone()),
            );

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "ratspeak_tauri::diagnostics",
                marker = "allowed-marker",
                "allowed diagnostics event"
            );
            tracing::error!(
                target: "rns_interface",
                secret = "dependency-canary",
                "excluded dependency event"
            );
            let span = tracing::info_span!(
                target: "lxmf_core::router",
                "excluded_dependency_span",
                secret = "span-canary"
            );
            let _entered = span.enter();
            tracing::warn!(
                target: "ble_diag",
                endpoint = "ble://endpoint-canary",
                "excluded explicit target"
            );
        });

        let output = String::from_utf8(captured.lock().expect("captured output").clone())
            .expect("UTF-8 diagnostics output");
        assert!(output.contains("allowed-marker"));
        for canary in ["dependency-canary", "span-canary", "endpoint-canary"] {
            assert!(!output.contains(canary), "excluded target leaked {canary}");
        }
    }

    #[test]
    fn prohibited_structured_fields_are_rejected_before_values_are_visited() {
        use tracing_subscriber::EnvFilter;
        use tracing_subscriber::filter::filter_fn;
        use tracing_subscriber::layer::SubscriberExt;

        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new("trace"))
            .with(filter_fn(metadata_allowed))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .with_writer(move || writer.clone()),
            );

        let prohibited_value_visits = AtomicUsize::new(0);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "ratspeak_tauri::diagnostics",
                marker = "safe-marker",
                "safe diagnostics event"
            );
            tracing::info!(target: "ratspeak_runtime", app_id = "app-id-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", backup = "backup-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", command = "command-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", content = "content-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", endpoint = "endpoint-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", error = "error-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", err = "err-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", event = "event-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", fallback = "fallback-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", file_name = "filename-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", greeting = "greeting-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", interface = "interface-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", label = "label-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", nickname = "nickname-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", passcode = "passcode-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", path = "path-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", payload = "payload-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", pin = "pin-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", private_key = "private-key-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", public_key = "public-key-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", response = "response-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", secret = "secret-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", session_id = "session-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", stored = "stored-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", stored_name = "stored-name-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", title = "title-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", token = "token-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", topic = "topic-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", uri = "uri-canary", "blocked");
            tracing::info!(target: "ratspeak_runtime", url = "url-canary", "blocked");
            tracing::debug!(target: "ratspeak_runtime", content = "debug-content-canary", "blocked");
            tracing::trace!(target: "ratspeak_runtime", content = "trace-content-canary", "blocked");
            tracing::info!(
                target: "ratspeak_runtime",
                content = prohibited_value(&prohibited_value_visits),
                "blocked without evaluating its value"
            );
        });

        let output = String::from_utf8(captured.lock().expect("captured output").clone())
            .expect("UTF-8 diagnostics output");
        assert!(output.contains("safe-marker"));
        for canary in [
            "app-id-canary",
            "backup-canary",
            "debug-content-canary",
            "content-canary",
            "err-canary",
            "event-canary",
            "fallback-canary",
            "filename-canary",
            "greeting-canary",
            "interface-canary",
            "label-canary",
            "nickname-canary",
            "passcode-canary",
            "path-canary",
            "endpoint-canary",
            "command-canary",
            "session-canary",
            "error-canary",
            "payload-canary",
            "pin-canary",
            "private-key-canary",
            "public-key-canary",
            "response-canary",
            "topic-canary",
            "secret-canary",
            "stored-canary",
            "stored-name-canary",
            "title-canary",
            "token-canary",
            "trace-content-canary",
            "uri-canary",
            "url-canary",
            "side-effect-canary",
        ] {
            assert!(!output.contains(canary), "prohibited field leaked {canary}");
        }
        assert_eq!(
            prohibited_value_visits.load(Ordering::SeqCst),
            0,
            "metadata filtering must reject the callsite before evaluating field expressions"
        );
    }
}
