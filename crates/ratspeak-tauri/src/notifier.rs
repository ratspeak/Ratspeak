use ratspeak_core::{NativeNotification, NativeNotifier};
use tauri_plugin_notification::NotificationExt;

pub struct TauriNotifier {
    handle: tauri::AppHandle,
}

impl TauriNotifier {
    pub fn new(handle: tauri::AppHandle) -> Self {
        Self { handle }
    }
}

impl NativeNotifier for TauriNotifier {
    fn notify(&self, notification: NativeNotification) {
        let NativeNotification {
            kind: _kind,
            title,
            body,
            thread_id,
            notification_id,
        } = notification;
        let state = match self.handle.notification().permission_state() {
            Ok(state) => state,
            Err(_) => {
                tracing::warn!(
                    reason = "permission_check_failed",
                    "notification permission check failed"
                );
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
            // notification to the right view (lxmf:<hash> /
            // lrgp:<session> / channels:<hub>:<hex-room>).
            // Recoverable on Android via the serialized notification payload.
            // TODO(desktop): notify-rust has no tap/action callback, so taps
            // only focus the window; investigate a richer backend later.
            builder = builder.extra("route", thread_id.clone()).group(thread_id);
            #[cfg(target_os = "ios")]
            {
                // notification 2.3.3 drops `extra` when reconstructing an iOS
                // tap, but preserves actionTypeId. Route validation remains in
                // the frontend before this value can cause navigation.
                builder = builder.action_type_id(thread_id);
            }
        }
        #[cfg(target_os = "android")]
        {
            let channel_id = match _kind {
                ratspeak_core::NativeNotificationKind::Message
                | ratspeak_core::NativeNotificationKind::Channel
                | ratspeak_core::NativeNotificationKind::Game => "ratspeak_messages",
                ratspeak_core::NativeNotificationKind::Call => "ratspeak_calls",
            };
            builder = builder.channel_id(channel_id);
        }

        if builder.show().is_err() {
            tracing::warn!(reason = "show_failed", "native notification failed");
        }
    }
}
