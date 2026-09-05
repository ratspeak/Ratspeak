//! Native `ratspeak://channel` lifecycle integration.
//!
//! The frontend is not granted the plugin command API and never subscribes to
//! the plugin's URL event. Platform-delivered URLs cross the canonical Rust
//! parser once; only the typed, key-free target is retained in this bounded
//! process-memory inbox until application JavaScript acknowledges presentation.

use std::sync::{Mutex, MutexGuard};

use ratspeak_tauri::commands::channels::{parse_channel_share_target, ChannelShareTarget};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_deep_link::DeepLinkExt;
use url::Url;

const NATIVE_CHANNEL_SHARE_AVAILABLE: &str = "native_channel_share_available";

/// Android's plugin filters incoming Intents independently of the manifest.
/// Supply that filter at runtime: mobile build-time plugin configuration would
/// rewrite platform registration/signing inputs during native builds.
#[cfg(any(target_os = "android", test))]
pub(crate) fn configure_android_runtime(config: &mut tauri::Config) {
    config.plugins.0.insert(
        "deep-link".into(),
        serde_json::json!({
            "mobile": [{ "scheme": ["ratspeak"], "host": "channel" }]
        }),
    );
}

#[derive(Default)]
pub(crate) struct NativeChannelShareInbox {
    state: Mutex<InboxState>,
}

#[derive(Default)]
struct InboxState {
    last_revision: u64,
    pending: Option<PendingShare>,
}

struct PendingShare {
    revision: u64,
    target: ChannelShareTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeChannelShareDelivery {
    // JavaScript numbers cannot represent every u64 exactly. This token is
    // opaque to the frontend and echoed byte-for-byte in the acknowledgment.
    revision: String,
    activity_generation: Option<String>,
    target: ChannelShareTarget,
}

impl NativeChannelShareInbox {
    fn state(&self) -> MutexGuard<'_, InboxState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Accept every valid target in arrival order and retain only the newest.
    ///
    /// Returning `true` means the observable pending value changed. Repeated
    /// delivery of the same URL while it is still pending is coalesced.
    fn accept_payloads<'a>(&self, payloads: impl IntoIterator<Item = &'a str>) -> bool {
        let mut latest = None;
        let mut rejected = 0usize;
        for payload in payloads {
            match parse_channel_share_target(payload) {
                Ok(target) => latest = Some(target),
                Err(_) => rejected = rejected.saturating_add(1),
            }
        }
        if rejected > 0 {
            tracing::debug!(
                rejected,
                reason = "invalid_native_channel_share",
                "ignored non-canonical native channel share"
            );
        }

        let Some(latest) = latest else {
            return false;
        };
        let mut state = self.state();
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.target == latest)
        {
            return false;
        }
        // Never reuse a delivery token, even at exhaustion. Retain any current
        // target instead of letting an old acknowledgment remove a newer one.
        let Some(revision) = state.last_revision.checked_add(1) else {
            return false;
        };
        state.last_revision = revision;
        state.pending = Some(PendingShare {
            revision,
            target: latest,
        });
        tracing::debug!(
            reason = "native_channel_share_accepted",
            "accepted a native channel share"
        );
        true
    }

    fn peek(&self) -> Option<NativeChannelShareDelivery> {
        let state = self.state();
        tracing::debug!(
            pending_present = state.pending.is_some(),
            reason = "native_channel_share_peeked",
            "inspected the pending native channel share"
        );
        state
            .pending
            .as_ref()
            .map(|pending| NativeChannelShareDelivery {
                revision: pending.revision.to_string(),
                activity_generation: None,
                target: pending.target.clone(),
            })
    }

    fn acknowledge(&self, revision: &str) -> bool {
        let mut state = self.state();
        // Exact string equality also rejects non-canonical decimal tokens.
        let accepted = state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.revision.to_string() == revision);
        if accepted {
            state.pending = None;
        }
        accepted
    }
}

fn enqueue_native_channel_shares<R: Runtime>(app: &AppHandle<R>, urls: Vec<Url>) {
    let inbox = app.state::<NativeChannelShareInbox>();
    if inbox.accept_payloads(urls.iter().map(Url::as_str))
        && app.emit(NATIVE_CHANNEL_SHARE_AVAILABLE, ()).is_err()
    {
        tracing::debug!(
            reason = "native_channel_share_event_unavailable",
            "could not notify the WebView about a native channel share"
        );
    }
}

/// A peek blocked by Android's native lifecycle is not an empty inbox. Wake
/// the visible frontend again after native readiness/resume, without polling
/// and without holding either the lifecycle authority or the inbox lock.
#[cfg(target_os = "android")]
pub(crate) fn notify_pending(app: &AppHandle) {
    let pending = app
        .state::<NativeChannelShareInbox>()
        .state()
        .pending
        .is_some();
    if pending {
        let _ = app.emit(NATIVE_CHANNEL_SHARE_AVAILABLE, ());
    }
}

/// Register the running-app listener before sampling the cold-start value.
/// The inbox coalesces the harmless overlap if both paths report the same URL.
pub(crate) fn install(app: &mut tauri::App) {
    let listener_app = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        enqueue_native_channel_shares(&listener_app, event.urls());
    });

    match app.deep_link().get_current() {
        Ok(Some(urls)) => enqueue_native_channel_shares(app.handle(), urls),
        Ok(None) => {}
        Err(_) => tracing::debug!(
            reason = "native_channel_share_cold_start_unavailable",
            "could not inspect the native channel-share launch target"
        ),
    }
}

#[tauri::command]
pub(crate) fn peek_native_channel_share(
    inbox: State<'_, NativeChannelShareInbox>,
) -> Option<NativeChannelShareDelivery> {
    #[cfg(target_os = "android")]
    {
        crate::android_lifecycle::with_presentation_owner(None, |generation| {
            inbox.peek().map(|mut delivery| {
                delivery.activity_generation = Some(generation.to_string());
                delivery
            })
        })
        .flatten()
    }
    #[cfg(not(target_os = "android"))]
    inbox.peek()
}

#[tauri::command]
pub(crate) fn ack_native_channel_share(
    inbox: State<'_, NativeChannelShareInbox>,
    revision: String,
    activity_generation: Option<String>,
) -> bool {
    #[cfg(target_os = "android")]
    let accepted = activity_generation.as_deref().is_some_and(|generation| {
        crate::android_lifecycle::with_presentation_owner(Some(generation), |_| {
            inbox.acknowledge(&revision)
        })
        .unwrap_or(false)
    });
    #[cfg(not(target_os = "android"))]
    let accepted = activity_generation.is_none() && inbox.acknowledge(&revision);
    tracing::debug!(
        accepted,
        reason = "native_channel_share_acknowledged",
        "processed a native channel-share presentation acknowledgment"
    );
    accepted
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUB_A: &str = "00112233445566778899aabbccddeeff";
    const HUB_B: &str = "ffeeddccbbaa99887766554433221100";

    #[test]
    fn android_runtime_filter_matches_static_registration_without_build_time_mutation() {
        let mut config: tauri::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert!(!config.plugins.0.contains_key("deep-link"));
        config
            .plugins
            .0
            .insert("unrelated".into(), serde_json::json!({"preserved": true}));
        let before = config.clone();

        configure_android_runtime(&mut config);
        assert_eq!(
            config.plugins.0["deep-link"],
            serde_json::json!({"mobile": [{"scheme": ["ratspeak"], "host": "channel"}]})
        );
        config.plugins.0.remove("deep-link");
        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::to_value(before).unwrap()
        );
        for source in [
            include_str!("../tauri.android.conf.json"),
            include_str!("../tauri.ios.conf.json"),
        ] {
            let platform: serde_json::Value = serde_json::from_str(source).unwrap();
            assert!(platform["plugins"]["deep-link"].is_null());
        }
    }

    fn share(hub: &str, room: &str) -> String {
        format!("ratspeak://channel?v=1&hub={hub}&room={room}")
    }

    #[test]
    fn inbox_keeps_only_the_latest_canonical_target() {
        let inbox = NativeChannelShareInbox::default();
        let first = share(HUB_A, "general");
        let second = share(HUB_B, "field");

        assert!(inbox.accept_payloads([first.as_str(), second.as_str()]));
        let delivery = inbox.peek().expect("latest target");
        assert_eq!(delivery.target.hub_destination_hash, HUB_B);
        assert_eq!(delivery.target.room.as_deref(), Some("field"));
        assert!(inbox.acknowledge(&delivery.revision));
        assert!(inbox.peek().is_none());
    }

    #[test]
    fn invalid_or_key_bearing_urls_never_replace_a_pending_target() {
        let inbox = NativeChannelShareInbox::default();
        let valid = share(HUB_A, "general");
        assert!(inbox.accept_payloads([valid.as_str()]));

        let with_key = format!("{valid}&key=secret");
        assert!(!inbox.accept_payloads([
            "ratspeak://contact?v=1",
            with_key.as_str(),
            "https://channel.invalid/"
        ]));
        assert_eq!(
            inbox
                .peek()
                .expect("original target")
                .target
                .hub_destination_hash,
            HUB_A
        );
    }

    #[test]
    fn repeated_pending_target_coalesces_but_reopens_with_a_new_revision_after_ack() {
        let inbox = NativeChannelShareInbox::default();
        let valid = share(HUB_A, "general");

        assert!(inbox.accept_payloads([valid.as_str()]));
        let first = inbox.peek().unwrap();
        assert!(!inbox.accept_payloads([valid.as_str()]));
        assert_eq!(inbox.peek(), Some(first.clone()));
        assert!(inbox.acknowledge(&first.revision));
        assert!(inbox.accept_payloads([valid.as_str()]));
        let next = inbox.peek().unwrap();
        assert_ne!(first.revision, next.revision);
        assert_eq!(first.target, next.target);
        assert!(!inbox.acknowledge(&first.revision));
        assert_eq!(inbox.peek(), Some(next));
    }

    #[test]
    fn dropped_peek_response_does_not_consume_pending_delivery() {
        let inbox = NativeChannelShareInbox::default();
        let valid = share(HUB_A, "general");
        assert!(inbox.accept_payloads([valid.as_str()]));
        let retired_view = inbox.peek().unwrap();
        assert_eq!(inbox.peek(), Some(retired_view.clone()));
        assert!(inbox.acknowledge(&retired_view.revision));
        assert!(!inbox.acknowledge(&retired_view.revision));
        assert!(inbox.peek().is_none());
    }

    #[test]
    fn stale_or_malformed_ack_never_removes_a_newer_target() {
        let inbox = NativeChannelShareInbox::default();
        let first = share(HUB_A, "general");
        let second = share(HUB_B, "field");
        assert!(inbox.accept_payloads([first.as_str()]));
        let stale = inbox.peek().unwrap();
        assert!(inbox.accept_payloads([second.as_str()]));
        let current = inbox.peek().unwrap();
        for revision in [
            stale.revision.as_str(),
            "",
            "0",
            "02",
            "+2",
            "2 ",
            "18446744073709551616",
        ] {
            assert!(!inbox.acknowledge(revision));
            assert_eq!(inbox.peek(), Some(current.clone()));
        }
        assert!(inbox.acknowledge(&current.revision));
    }

    #[test]
    fn revisions_remain_exact_above_javascript_integer_precision() {
        let inbox = NativeChannelShareInbox::default();
        inbox.state().last_revision = 9_007_199_254_740_992;
        let valid = share(HUB_A, "general");
        assert!(inbox.accept_payloads([valid.as_str()]));
        let delivery = inbox.peek().unwrap();
        assert_eq!(delivery.revision, "9007199254740993");
        assert!(serde_json::to_value(&delivery).unwrap()["revision"].is_string());
        assert!(inbox.acknowledge(&delivery.revision));
    }

    #[test]
    fn exhausted_revisions_never_wrap_or_replace_the_current_target() {
        let inbox = NativeChannelShareInbox::default();
        inbox.state().last_revision = u64::MAX - 1;
        let first = share(HUB_A, "general");
        let next = share(HUB_B, "field");
        assert!(inbox.accept_payloads([first.as_str()]));
        let last = inbox.peek().unwrap();
        assert_eq!(last.revision, u64::MAX.to_string());
        assert!(!inbox.accept_payloads([next.as_str()]));
        assert_eq!(inbox.peek(), Some(last.clone()));
        assert!(inbox.acknowledge(&last.revision));
        assert!(!inbox.accept_payloads([next.as_str()]));
        assert!(inbox.peek().is_none());
    }
}
