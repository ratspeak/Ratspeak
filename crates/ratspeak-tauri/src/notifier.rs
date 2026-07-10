use ratspeak_core::{NativeNotification, NativeNotifier};
#[cfg(not(target_os = "ios"))]
use tauri_plugin_notification::NotificationExt;

pub struct TauriNotifier {
    // iOS: unused until the notification stub in `notify` is lifted.
    #[cfg_attr(target_os = "ios", allow(dead_code))]
    handle: tauri::AppHandle,
}

impl TauriNotifier {
    pub fn new(handle: tauri::AppHandle) -> Self {
        Self { handle }
    }
}

impl NativeNotifier for TauriNotifier {
    fn notify(&self, notification: NativeNotification) {
        #[cfg(target_os = "ios")]
        {
            let _ = notification;
            // TODO(iOS release): enable after App Store/TestFlight signing and
            // notification entitlement review are finalized. When unstubbed, also
            // wire the `route` extra for tap deep-linking (see non-iOS branch).
            tracing::debug!("iOS native notifications are stubbed until release signing is ready");
            return;
        }

        #[cfg(not(target_os = "ios"))]
        {
            let NativeNotification {
                kind: _kind,
                title,
                body,
                thread_id,
                notification_id,
            } = notification;
            let state = match self.handle.notification().permission_state() {
                Ok(state) => state,
                Err(e) => {
                    tracing::warn!(error = %e, "notification permission check failed");
                    return;
                }
            };
            if state != tauri_plugin_notification::PermissionState::Granted {
                tracing::debug!(?state, "native notification skipped without permission");
                return;
            }

            let mut builder = self
                .handle
                .notification()
                .builder()
                .title(title)
                .body(body)
                .auto_cancel();

            if let Some(id) = notification_id {
                builder = builder.id(id);
            }
            if let Some(thread_id) = thread_id {
                // `route` lets the frontend `onAction` handler deep-link a tapped
                // notification to the right view (lxmf:<hash> / lrgp:<session>).
                // Recoverable on Android via the serialized notification payload.
                // TODO(desktop): notify-rust has no tap/action callback, so taps
                // only focus the window; investigate a richer backend later.
                builder = builder.extra("route", thread_id.clone()).group(thread_id);
            }
            #[cfg(target_os = "android")]
            {
                let channel_id = match _kind {
                    ratspeak_core::NativeNotificationKind::Message
                    | ratspeak_core::NativeNotificationKind::Game => "ratspeak_messages",
                    ratspeak_core::NativeNotificationKind::Call => "ratspeak_calls",
                };
                builder = builder.channel_id(channel_id);
            }

            if let Err(e) = builder.show() {
                tracing::warn!(error = %e, "native notification failed");
            }
        }
    }
}
