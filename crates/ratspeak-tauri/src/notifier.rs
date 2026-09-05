use ratspeak_core::{NativeNotification, NativeNotifier};
#[cfg(not(target_os = "android"))]
use tauri_plugin_notification::NotificationExt;

pub struct TauriNotifier {
    #[cfg(not(target_os = "android"))]
    handle: tauri::AppHandle,
}

impl TauriNotifier {
    pub fn new(handle: tauri::AppHandle) -> Self {
        #[cfg(target_os = "android")]
        {
            let _ = handle;
            Self {}
        }
        #[cfg(not(target_os = "android"))]
        Self { handle }
    }
}

impl NativeNotifier for TauriNotifier {
    fn notify(&self, notification: NativeNotification) {
        #[cfg(target_os = "android")]
        android::notify(notification);
        #[cfg(not(target_os = "android"))]
        self.notify_with_plugin(notification);
    }
}

#[cfg(not(target_os = "android"))]
impl TauriNotifier {
    fn notify_with_plugin(&self, notification: NativeNotification) {
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

        #[cfg(target_os = "ios")]
        {
            // The plugin exposes iOS sounds as names rather than exposing
            // `UNNotificationSound.default` directly. `default` intentionally
            // names no bundled custom file, so UserNotifications follows its
            // documented missing-file fallback and plays the system default.
            // Without a sound object, iOS shows the banner silently. The OS
            // still owns Sounds, Haptics, silent-mode and Focus decisions.
            builder = builder.sound("default");
        }

        if let Some(id) = notification_id {
            builder = builder.id(id);
        }
        if let Some(thread_id) = thread_id {
            // `route` lets the frontend `onAction` handler deep-link a tapped
            // notification to the right view (lxmf:<hash> /
            // lrgp:<session> / channels:<hub>:<hex-room>).
            // Android uses its separate Application-context delivery adapter.
            // TODO(desktop): notify-rust has no tap/action callback, so taps
            // only focus the window; investigate a richer backend later.
            builder = builder
                .extra("route", thread_id.clone())
                .group(thread_id.clone());
            #[cfg(target_os = "ios")]
            {
                // notification 2.3.3 drops `extra` when reconstructing an iOS
                // tap, but preserves actionTypeId. Route validation remains in
                // the frontend before this value can cause navigation.
                builder = builder.action_type_id(thread_id);
            }
        }
        if builder.show().is_err() {
            tracing::warn!(reason = "show_failed", "native notification failed");
        }
    }
}

#[cfg(any(target_os = "android", test))]
fn android_notification_payload(notification: &NativeNotification) -> Result<String, &'static str> {
    if notification
        .thread_id
        .as_deref()
        .is_some_and(|route| !valid_android_route(route))
    {
        return Err("invalid_route");
    }
    let channel_id = match notification.kind {
        ratspeak_core::NativeNotificationKind::Message
        | ratspeak_core::NativeNotificationKind::Channel
        | ratspeak_core::NativeNotificationKind::Game => "ratspeak_messages",
        ratspeak_core::NativeNotificationKind::Call => "ratspeak_calls",
    };
    // Normal notification text is already abbreviated by the runtime's privacy
    // policy. These outer bounds also protect the JNI/Binder boundary against
    // an unexpectedly large local contact name without altering that policy.
    let title = notification.title.chars().take(512).collect::<String>();
    let body = notification.body.chars().take(2048).collect::<String>();
    let payload = serde_json::json!({
        "title": title,
        "body": body,
        "route": notification.thread_id,
        "id": notification.notification_id,
        "channel": channel_id,
    })
    .to_string();
    if payload.len() > 16 * 1024 {
        return Err("payload_too_large");
    }
    Ok(payload)
}

#[cfg(any(target_os = "android", test))]
fn valid_android_route(route: &str) -> bool {
    fn lowercase_hex(value: &str, length: usize) -> bool {
        value.len() == length
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
    let Some((kind, value)) = route.split_once(':') else {
        return false;
    };
    match kind {
        "lxmf" | "lxst" => lowercase_hex(value, 32),
        "lrgp" => lrgp::protocol::is_valid_session_id(value),
        "channels" => {
            let Some((hub, room)) = value.split_once(':') else {
                return false;
            };
            if !lowercase_hex(hub, 32)
                || room.is_empty()
                || room.len() > 512
                || !room.len().is_multiple_of(2)
                || !lowercase_hex(room, room.len())
            {
                return false;
            }
            let Ok(bytes) = hex::decode(room) else {
                return false;
            };
            let Ok(room) = std::str::from_utf8(&bytes) else {
                return false;
            };
            // Match the frontend's ECMAScript String.trim boundary exactly.
            // Rust's Unicode trim differs for U+0085 and U+FEFF.
            room.trim_matches(|character| {
                matches!(character,
                    '\u{9}'..='\u{d}' | '\u{20}' | '\u{a0}' | '\u{1680}' |
                    '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' |
                    '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
            }) == room
                && !room.is_empty()
                && !room
                    .chars()
                    .any(|character| character <= '\u{1f}' || character == '\u{7f}')
        }
        _ => false,
    }
}

#[cfg(target_os = "android")]
mod android {
    use super::{NativeNotification, android_notification_payload};
    use jni::objects::{GlobalRef, JClass, JObject, JValue};
    use std::sync::OnceLock;

    struct ApplicationBridge {
        vm: jni::JavaVM,
        application: GlobalRef,
        class: GlobalRef,
    }

    static BRIDGE: OnceLock<ApplicationBridge> = OnceLock::new();

    // Registered by Kotlin after its application bootstrap loads the native
    // library. Strong global refs own only Application and the class, never an
    // Activity/WebView. Every delivery can therefore run after task removal.
    #[unsafe(no_mangle)]
    extern "system" fn Java_org_ratspeak_android_RatspeakNotifications_nativeInitialize(
        env: jni::JNIEnv,
        class: JClass,
        context: JObject,
    ) {
        if BRIDGE.get().is_some() {
            return;
        }
        let registered = (|| -> jni::errors::Result<()> {
            let application = env
                .call_method(
                    context,
                    "getApplicationContext",
                    "()Landroid/content/Context;",
                    &[],
                )?
                .l()?;
            if application.is_null() {
                return Err(jni::errors::Error::NullPtr("Application context"));
            }
            let application = env.auto_local(application);
            let bridge = ApplicationBridge {
                vm: env.get_java_vm()?,
                application: env.new_global_ref(application.as_obj())?,
                class: env.new_global_ref(class)?,
            };
            let _ = BRIDGE.set(bridge);
            Ok(())
        })();
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        if registered.is_err() {
            tracing::warn!(
                reason = "notification_bridge_init_failed",
                "Android notification bridge unavailable"
            );
        }
    }

    pub(super) fn notify(notification: NativeNotification) {
        let payload = match android_notification_payload(&notification) {
            Ok(payload) => payload,
            Err(reason) => {
                tracing::warn!(reason, "Android notification payload rejected");
                return;
            }
        };
        let Some(bridge) = BRIDGE.get() else {
            tracing::warn!(
                reason = "notification_bridge_unavailable",
                "Android notification skipped"
            );
            return;
        };
        let Ok(env) = bridge.vm.attach_current_thread() else {
            tracing::warn!(
                reason = "notification_thread_attach_failed",
                "Android notification failed"
            );
            return;
        };
        let result = (|| -> jni::errors::Result<i32> {
            let payload = env.auto_local(JObject::from(env.new_string(payload)?));
            env.call_static_method(
                JClass::from(bridge.class.as_obj()),
                "show",
                "(Landroid/content/Context;Ljava/lang/String;)I",
                &[
                    JValue::Object(bridge.application.as_obj()),
                    JValue::Object(payload.as_obj()),
                ],
            )?
            .i()
        })();
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        match result {
            Ok(0) => {}
            Ok(1) => tracing::debug!(reason = "permission_denied", "Android notification skipped"),
            Ok(2) => tracing::warn!(reason = "invalid_payload", "Android notification rejected"),
            _ => tracing::warn!(
                reason = "notification_show_failed",
                "Android notification failed"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_payload_preserves_notification_policy_and_stable_identity() {
        let route = format!("lxmf:{}", "ab".repeat(16));
        let original =
            NativeNotification::message("Message from Alice", "Private preview", &route, 1250);
        let payload: serde_json::Value =
            serde_json::from_str(&android_notification_payload(&original).unwrap()).unwrap();
        assert_eq!(payload["title"], original.title);
        assert_eq!(payload["body"], original.body);
        assert_eq!(payload["route"], route);
        assert_eq!(payload["id"], 1250);
        assert_eq!(payload["channel"], "ratspeak_messages");
        let call = NativeNotification::call(
            "Incoming call",
            "Tap to open Ratspeak",
            format!("lxst:{}", "11".repeat(16)),
            3_000_005,
        );
        let payload: serde_json::Value =
            serde_json::from_str(&android_notification_payload(&call).unwrap()).unwrap();
        assert_eq!(payload["channel"], "ratspeak_calls");
    }

    #[test]
    fn android_routes_reject_noncanonical_targets_and_invalid_channel_utf8() {
        let hub = "ab".repeat(16);
        for route in [
            format!("lxmf:{hub}"),
            format!("lxst:{hub}"),
            "lrgp:0123456789abcdef".into(),
            format!("channels:{hub}:{}", hex::encode("field café")),
            format!("channels:{hub}:{}", hex::encode("\u{85}field")),
        ] {
            assert!(valid_android_route(&route), "{route}");
        }
        for route in [
            "https://example.invalid".into(),
            "lrgp:game-7".into(),
            format!("lxmf:{}", hub.to_uppercase()),
            format!("channels:{hub}:ff"),
            format!("channels:{hub}:0a"),
            format!("channels:{hub}:2061"),
            format!("channels:{hub}:{}", hex::encode("\u{feff}field")),
            format!("channels:{hub}:{}", "61".repeat(257)),
        ] {
            assert!(!valid_android_route(&route), "{route}");
        }
    }

    #[test]
    fn android_payload_bounds_unicode_without_splitting_characters() {
        let message = NativeNotification::message(
            "😀".repeat(1000),
            "é".repeat(5000),
            format!("lxmf:{}", "ab".repeat(16)),
            1001,
        );
        let payload = android_notification_payload(&message).unwrap();
        assert!(payload.len() <= 16 * 1024);
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["title"].as_str().unwrap().chars().count(), 512);
        assert_eq!(payload["body"].as_str().unwrap().chars().count(), 2048);
    }
}
