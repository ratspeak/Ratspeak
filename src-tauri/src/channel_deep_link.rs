//! Native `ratspeak://channel` lifecycle integration.
//!
//! The frontend is not granted the plugin command API and never subscribes to
//! the plugin's URL event. Platform-delivered URLs cross the canonical Rust
//! parser once; only the typed, key-free target is retained in this bounded
//! process-memory inbox and consumed by application JavaScript.

use std::sync::{Mutex, MutexGuard};

use ratspeak_tauri::commands::channels::{parse_channel_share_target, ChannelShareTarget};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_deep_link::DeepLinkExt;
use url::Url;

const NATIVE_CHANNEL_SHARE_AVAILABLE: &str = "native_channel_share_available";

#[derive(Default)]
pub(crate) struct NativeChannelShareInbox {
    pending: Mutex<Option<ChannelShareTarget>>,
}

impl NativeChannelShareInbox {
    fn pending(&self) -> MutexGuard<'_, Option<ChannelShareTarget>> {
        self.pending
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
        let mut pending = self.pending();
        if pending.as_ref() == Some(&latest) {
            return false;
        }
        *pending = Some(latest);
        true
    }

    fn take(&self) -> Option<ChannelShareTarget> {
        self.pending().take()
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
pub(crate) fn take_native_channel_share(
    inbox: State<'_, NativeChannelShareInbox>,
) -> Option<ChannelShareTarget> {
    inbox.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUB_A: &str = "00112233445566778899aabbccddeeff";
    const HUB_B: &str = "ffeeddccbbaa99887766554433221100";

    fn share(hub: &str, room: &str) -> String {
        format!("ratspeak://channel?v=1&hub={hub}&room={room}")
    }

    #[test]
    fn inbox_keeps_only_the_latest_canonical_target() {
        let inbox = NativeChannelShareInbox::default();
        let first = share(HUB_A, "general");
        let second = share(HUB_B, "field");

        assert!(inbox.accept_payloads([first.as_str(), second.as_str()]));
        let target = inbox.take().expect("latest target");
        assert_eq!(target.hub_destination_hash, HUB_B);
        assert_eq!(target.room.as_deref(), Some("field"));
        assert!(inbox.take().is_none(), "taking is one-shot");
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
            inbox.take().expect("original target").hub_destination_hash,
            HUB_A
        );
    }

    #[test]
    fn repeated_pending_target_is_coalesced_but_can_be_opened_again_after_take() {
        let inbox = NativeChannelShareInbox::default();
        let valid = share(HUB_A, "general");

        assert!(inbox.accept_payloads([valid.as_str()]));
        assert!(!inbox.accept_payloads([valid.as_str()]));
        assert!(inbox.take().is_some());
        assert!(inbox.accept_payloads([valid.as_str()]));
    }
}
