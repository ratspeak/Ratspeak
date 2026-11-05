//! Native `lxm://` paper-link delivery.
//!
//! Platform-delivered paper links are held in process memory until the
//! frontend acknowledges presenting them. Nothing is decoded or filed here:
//! the user always reviews a paper message before adding it.

use std::sync::{Mutex, MutexGuard};

use ratspeak_tauri::commands::paper::normalize_paper_uri;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use url::Url;

const NATIVE_PAPER_LINK_AVAILABLE: &str = "native_paper_link_available";

#[derive(Default)]
pub(crate) struct NativePaperLinkInbox {
    pending: Mutex<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct NativePaperLink {
    uri: Option<String>,
}

impl NativePaperLinkInbox {
    fn pending(&self) -> MutexGuard<'_, Option<String>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Retain only the newest paper link; `true` when the pending link changed.
    fn accept_payloads<'a>(&self, payloads: impl IntoIterator<Item = &'a str>) -> bool {
        let Some(latest) = payloads.into_iter().filter_map(normalize_paper_uri).last() else {
            return false;
        };
        let mut pending = self.pending();
        if pending.as_deref() == Some(latest.as_str()) {
            return false;
        }
        *pending = Some(latest);
        tracing::debug!(
            reason = "native_paper_link_accepted",
            "accepted a native paper link"
        );
        true
    }

    fn peek(&self) -> Option<String> {
        self.pending().clone()
    }

    fn acknowledge(&self, uri: &str) -> bool {
        let mut pending = self.pending();
        let accepted = pending.as_deref() == Some(uri);
        if accepted {
            *pending = None;
        }
        accepted
    }
}

pub(crate) fn enqueue<R: Runtime>(app: &AppHandle<R>, urls: &[Url]) {
    let inbox = app.state::<NativePaperLinkInbox>();
    if inbox.accept_payloads(urls.iter().map(Url::as_str))
        && app.emit(NATIVE_PAPER_LINK_AVAILABLE, ()).is_err()
    {
        tracing::debug!(
            reason = "native_paper_link_event_unavailable",
            "could not notify the WebView about a native paper link"
        );
    }
}

/// Re-signal a link held while Android's activity could not present it.
#[cfg(target_os = "android")]
pub(crate) fn notify_pending(app: &AppHandle) {
    let pending = app.state::<NativePaperLinkInbox>().pending().is_some();
    if pending {
        let _ = app.emit(NATIVE_PAPER_LINK_AVAILABLE, ());
    }
}

#[tauri::command]
pub(crate) fn peek_native_paper_link(inbox: State<'_, NativePaperLinkInbox>) -> NativePaperLink {
    #[cfg(target_os = "android")]
    let uri = crate::android_lifecycle::with_presentation_owner(None, |_| inbox.peek()).flatten();
    #[cfg(not(target_os = "android"))]
    let uri = inbox.peek();
    NativePaperLink { uri }
}

#[tauri::command]
pub(crate) fn ack_native_paper_link(inbox: State<'_, NativePaperLinkInbox>, uri: String) -> bool {
    inbox.acknowledge(&uri)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str = "lxm://kGXGpwNEzdOehWmdNJAfuC8WT2iRYSeTRIrP3AmmgveH1x8dYkLYQ";

    #[test]
    fn inbox_keeps_only_the_latest_paper_link_until_acknowledged() {
        let inbox = NativePaperLinkInbox::default();
        let newer = format!("{LINK}xyz");
        assert!(inbox.accept_payloads([LINK, newer.as_str()]));
        assert_eq!(inbox.peek().as_deref(), Some(newer.as_str()));
        assert!(!inbox.accept_payloads([newer.as_str()]));
        assert!(!inbox.acknowledge(LINK));
        assert!(inbox.acknowledge(&newer));
        assert!(inbox.peek().is_none());
    }

    #[test]
    fn non_paper_links_never_replace_a_pending_link() {
        let inbox = NativePaperLinkInbox::default();
        assert!(inbox.accept_payloads([LINK]));
        assert!(!inbox.accept_payloads([
            "ratspeak://channel?v=1&hub=00112233445566778899aabbccddeeff",
            "https://example.invalid/",
            "lxm://",
        ]));
        assert_eq!(inbox.peek().as_deref(), Some(LINK));
    }

    #[test]
    fn url_round_trip_preserves_the_paper_payload() {
        let upper = Url::parse(&LINK.replacen("lxm", "LXM", 1)).unwrap();
        assert_eq!(upper.as_str(), LINK);
        let parsed = Url::parse(LINK).unwrap();
        assert_eq!(parsed.as_str(), LINK);
        let inbox = NativePaperLinkInbox::default();
        assert!(inbox.accept_payloads([parsed.as_str()]));
        assert_eq!(inbox.peek().as_deref(), Some(LINK));
    }
}
