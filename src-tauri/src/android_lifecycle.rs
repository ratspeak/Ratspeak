//! Retain the process-owned session across Android UI task destruction.
//!
//! Creation runs on Tao's Rust event thread, bound to the exact Activity that
//! requested it. The maintained backend patch owns native lifetime/queue fences;
//! Android's UI thread never waits for a native window build or Wry queue space.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActivityOwner {
    generation: u64,
    activity_id: i32,
    resumed: bool,
    webview_ready: bool,
}

#[derive(Default)]
struct Lifecycle {
    latest_generation: u64,
    activity: Option<ActivityOwner>,
    restore_requested: Option<u64>,
    runtime_ready: bool,
    exiting: bool,
}

enum NativeLifecycleEvent {
    Ready,
    Resumed(u64, bool),
    Detached(u64),
}

impl Lifecycle {
    /// Execute a short, synchronous presentation transaction while the caller
    /// holds lifecycle authority. Never invoke JNI, WebView evaluation, or an
    /// async operation here. Inbox transactions always lock lifecycle first.
    fn with_presentation_owner<T>(
        &self,
        expected_generation: Option<&str>,
        transaction: impl FnOnce(u64) -> T,
    ) -> Option<T> {
        if !self.runtime_ready || self.exiting {
            return None;
        }
        let owner = self
            .activity
            .filter(|owner| owner.resumed && owner.webview_ready)?;
        if expected_generation.is_some_and(|expected| owner.generation.to_string() != expected) {
            return None;
        }
        Some(transaction(owner.generation))
    }

    // The caller owns the lifecycle mutex through publication. submit_lifecycle
    // allocates its transition authority synchronously, without JNI or await.
    fn transition(&mut self, event: NativeLifecycleEvent, publish: impl FnOnce(bool)) {
        let accepted = match event {
            NativeLifecycleEvent::Ready => {
                self.runtime_ready = true;
                true
            }
            NativeLifecycleEvent::Resumed(generation, resumed) => {
                self.set_resumed(generation, resumed)
            }
            NativeLifecycleEvent::Detached(generation) => self.detach(generation),
        };
        if accepted {
            publish(!self.exiting && self.activity.is_some_and(|owner| owner.resumed));
        }
    }
    fn attach(&mut self, generation: u64, activity_id: i32, webview_ready: bool) -> bool {
        if generation == 0 || generation <= self.latest_generation || self.exiting {
            return false;
        }
        self.latest_generation = generation;
        self.activity = Some(ActivityOwner {
            generation,
            activity_id,
            resumed: false,
            webview_ready,
        });
        self.restore_requested = None;
        true
    }

    fn detach(&mut self, generation: u64) -> bool {
        if self
            .activity
            .is_some_and(|owner| owner.generation == generation)
        {
            self.activity = None;
            self.restore_requested = None;
            return true;
        }
        false
    }

    fn set_resumed(&mut self, generation: u64, resumed: bool) -> bool {
        let Some(owner) = self
            .activity
            .as_mut()
            .filter(|owner| owner.generation == generation)
        else {
            return false;
        };
        owner.resumed = resumed;
        !self.exiting
    }

    fn set_webview_ready(&mut self, generation: u64) -> bool {
        let Some(owner) = self
            .activity
            .as_mut()
            .filter(|owner| owner.generation == generation)
        else {
            return false;
        };
        owner.webview_ready = true;
        true
    }

    fn prevent_implicit_exit(&self) -> bool {
        self.runtime_ready && !self.exiting
    }

    fn request_restore(&mut self, window_exists: bool) -> Option<ActivityOwner> {
        if !self.prevent_implicit_exit() || window_exists {
            return None;
        }
        let owner = self.activity?;
        if self.restore_requested == Some(owner.generation) {
            return None;
        }
        self.restore_requested = Some(owner.generation);
        Some(owner)
    }

    fn begin_restore(&mut self, owner: ActivityOwner, window_exists: bool) -> bool {
        if self.restore_requested != Some(owner.generation) {
            return false;
        }
        self.restore_requested = None;
        self.prevent_implicit_exit()
            && !window_exists
            && self.activity.is_some_and(|current| {
                current.generation == owner.generation && current.activity_id == owner.activity_id
            })
    }

    fn abandon_restore(&mut self, generation: u64) {
        if self.restore_requested == Some(generation) {
            self.restore_requested = None;
        }
    }

    fn exit(&mut self) {
        self.exiting = true;
        self.restore_requested = None;
    }
}

#[cfg(target_os = "android")]
mod platform {
    use super::{Lifecycle, NativeLifecycleEvent};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tauri::Manager;

    static LIFECYCLE: Mutex<Lifecycle> = Mutex::new(Lifecycle {
        latest_generation: 0,
        activity: None,
        restore_requested: None,
        runtime_ready: false,
        exiting: false,
    });
    static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
    static BRIDGE: OnceLock<AndroidBridge> = OnceLock::new();
    static ACTIVITY_TARGET: Mutex<
        Option<(
            u64,
            tauri::tao::platform::android::prelude::ActivityWindowTarget,
        )>,
    > = Mutex::new(None);

    struct AndroidBridge {
        vm: jni::JavaVM,
        class: jni::objects::GlobalRef,
    }
    fn lifecycle() -> MutexGuard<'static, Lifecycle> {
        LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn with_presentation_owner<T>(
        expected_generation: Option<&str>,
        transaction: impl FnOnce(u64) -> T,
    ) -> Option<T> {
        lifecycle().with_presentation_owner(expected_generation, transaction)
    }

    fn notify_presentation_ready() {
        let ready = lifecycle().with_presentation_owner(None, |_| ()).is_some();
        if ready {
            // The guard above is gone before any event delivery/inbox lock.
            if let Some(app) = APP.get() {
                crate::channel_deep_link::notify_pending(app);
                crate::text_share::notify_pending();
            }
        }
    }

    pub(crate) fn on_run_event(app: &tauri::AppHandle, event: &tauri::RunEvent) {
        match event {
            tauri::RunEvent::Ready => {
                let _ = APP.set(app.clone());
                // Activity attachment/resume can precede Tauri Ready.
                lifecycle().transition(
                    NativeLifecycleEvent::Ready,
                    crate::mobile_native::submit_lifecycle,
                );
                notify_presentation_ready();
                request_restore(app);
            }
            tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } => {
                if lifecycle().prevent_implicit_exit() {
                    api.prevent_exit();
                    request_restore(app);
                    tracing::info!(
                        reason = "android_task_detached",
                        "retained Android session after task removal"
                    );
                }
            }
            tauri::RunEvent::ExitRequested { code: Some(_), .. } | tauri::RunEvent::Exit => {
                lifecycle().exit()
            }
            _ => {}
        }
    }

    fn queue_restore_check() {
        let Some(app) = APP.get() else { return };
        let restore_app = app.clone();
        if app
            .run_on_main_thread(move || request_restore(&restore_app))
            .is_err()
        {
            tracing::warn!(
                reason = "android_restore_dispatch_failed",
                "could not schedule Android window restoration"
            );
        }
    }

    /// Only the Rust runtime event thread creates native windows. JNI calls
    /// enqueue this work without waiting or holding lifecycle locks across it.
    fn request_restore(app: &tauri::AppHandle) {
        let window_exists = app.get_webview_window("main").is_some();
        let requested = {
            let mut state = lifecycle();
            let requested = state.request_restore(window_exists);
            tracing::debug!(
                generation = state.latest_generation,
                window_exists,
                activity_present = state.activity.is_some(),
                requested = requested.is_some(),
                reason = "android_restore_check",
                "checked Android window restoration"
            );
            requested
        };
        let Some(owner) = requested else {
            return;
        };
        let restore_app = app.clone();
        let generation = owner.generation;
        if app
            .run_on_main_thread(move || {
                if !lifecycle()
                    .begin_restore(owner, restore_app.get_webview_window("main").is_some())
                {
                    return;
                }
                let target = ACTIVITY_TARGET
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref()
                    .filter(|(target_generation, _)| *target_generation == generation)
                    .map(|(_, target)| target.clone());
                let Some(target) = target else {
                    tracing::warn!(
                        generation,
                        reason = "android_restore_target_missing",
                        "captured Android window target is unavailable"
                    );
                    report_restore_failure(generation);
                    return;
                };
                let result = tauri::tao::platform::android::prelude::with_activity(target, || {
                    crate::main_window_builder(&restore_app).build()
                });
                match result {
                    Ok(_) => tracing::info!(
                        generation,
                        reason = "android_window_restore_created",
                        "created replacement Android window; awaiting native WebView readiness"
                    ),
                    Err(error) => {
                        let error_category = match error {
                            tauri::Error::Runtime(_) => "native_runtime",
                            tauri::Error::WindowLabelAlreadyExists(_) => "window_label_conflict",
                            tauri::Error::WebviewLabelAlreadyExists(_) => "webview_label_conflict",
                            tauri::Error::InvalidWindowHandle => "invalid_native_handle",
                            tauri::Error::Jni(_) => "jni",
                            _ => "window_configuration",
                        };
                        tracing::warn!(
                            generation,
                            error_category,
                            reason = "android_window_restore_failed",
                            "could not restore Android window"
                        );
                        report_restore_failure(generation);
                    }
                }
            })
            .is_err()
        {
            lifecycle().abandon_restore(generation);
            report_restore_failure(generation);
        }
    }

    fn report_restore_failure(generation: u64) {
        if !lifecycle()
            .activity
            .is_some_and(|owner| owner.generation == generation && !owner.webview_ready)
        {
            return;
        }
        let Some(bridge) = BRIDGE.get() else { return };
        let Ok(env) = bridge.vm.attach_current_thread() else {
            return;
        };
        let _ = env.call_static_method(
            jni::objects::JClass::from(bridge.class.as_obj()),
            "showActivityRestoreFailure",
            "(J)V",
            &[jni::objects::JValue::Long(generation as i64)],
        );
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_org_ratspeak_android_RatspeakNativeBridge_nativeActivityAttached(
        env: jni::JNIEnv,
        class: jni::objects::JClass,
        generation: jni::sys::jlong,
        activity_id: jni::sys::jint,
        webview_ready: jni::sys::jboolean,
    ) {
        let Ok(generation) = u64::try_from(generation) else {
            return;
        };
        if BRIDGE.get().is_none() {
            if let (Ok(vm), Ok(class)) = (env.get_java_vm(), env.new_global_ref(class)) {
                let _ = BRIDGE.set(AndroidBridge { vm, class });
            } else if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
        }
        // Capture the owned native target here on Android's main thread, not
        // later by integer ID after another Activity could reuse that ID.
        let target = tauri::tao::platform::android::prelude::capture_activity(activity_id);
        tracing::debug!(
            generation,
            activity_id,
            target_present = target.is_some(),
            webview_ready = webview_ready != 0,
            reason = "android_activity_attach",
            "attached Android Activity generation"
        );
        let mut state = lifecycle();
        if state.attach(generation, activity_id, webview_ready != 0) {
            *ACTIVITY_TARGET
                .lock()
                .unwrap_or_else(|error| error.into_inner()) =
                target.map(|target| (generation, target));
            drop(state);
            queue_restore_check();
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_org_ratspeak_android_RatspeakNativeBridge_nativeActivityDetached(
        _env: jni::JNIEnv,
        _class: jni::objects::JClass,
        generation: jni::sys::jlong,
    ) {
        if let Ok(generation) = u64::try_from(generation) {
            let mut state = lifecycle();
            if state
                .activity
                .is_some_and(|owner| owner.generation == generation)
            {
                ACTIVITY_TARGET
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
            }
            state.transition(
                NativeLifecycleEvent::Detached(generation),
                crate::mobile_native::submit_lifecycle,
            );
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_org_ratspeak_android_RatspeakNativeBridge_nativeActivityResumed(
        _env: jni::JNIEnv,
        _class: jni::objects::JClass,
        generation: jni::sys::jlong,
        resumed: jni::sys::jboolean,
    ) {
        if let Ok(generation) = u64::try_from(generation) {
            lifecycle().transition(
                NativeLifecycleEvent::Resumed(generation, resumed != 0),
                crate::mobile_native::submit_lifecycle,
            );
            if resumed != 0 {
                notify_presentation_ready();
            }
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_org_ratspeak_android_RatspeakNativeBridge_nativeActivityWebviewReady(
        _env: jni::JNIEnv,
        _class: jni::objects::JClass,
        generation: jni::sys::jlong,
    ) {
        if let Ok(generation) = u64::try_from(generation) {
            let accepted = lifecycle().set_webview_ready(generation);
            if accepted {
                tracing::info!(
                    generation,
                    reason = "android_webview_ready",
                    "Android Activity has a native WebView"
                );
                notify_presentation_ready();
            }
        }
    }
}

#[cfg(target_os = "android")]
pub(crate) use platform::{on_run_event, with_presentation_owner};

#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> Lifecycle {
        Lifecycle {
            runtime_ready: true,
            ..Default::default()
        }
    }

    #[test]
    fn presentation_lease_requires_current_resumed_native_ready_owner() {
        let mut state = ready();
        assert_eq!(state.with_presentation_owner(None, |_| true), None);
        assert!(state.attach(1, 100, false));
        assert_eq!(state.with_presentation_owner(None, |_| true), None);
        assert!(state.set_resumed(1, true));
        assert_eq!(state.with_presentation_owner(None, |_| true), None);
        assert!(state.set_webview_ready(1));
        assert_eq!(
            state.with_presentation_owner(None, |generation| generation),
            Some(1)
        );
        assert_eq!(
            state.with_presentation_owner(Some("1"), |_| true),
            Some(true)
        );
        for invalid in ["", "01", "+1", "2"] {
            assert_eq!(state.with_presentation_owner(Some(invalid), |_| true), None);
        }
        assert!(state.set_resumed(1, false));
        assert_eq!(state.with_presentation_owner(Some("1"), |_| true), None);
        assert!(state.set_resumed(1, true));
        state.exit();
        assert_eq!(state.with_presentation_owner(Some("1"), |_| true), None);
    }

    #[test]
    fn retiring_view_ack_cannot_run_after_replacement_lease_is_ready() {
        let mut state = ready();
        assert!(state.attach(1, 100, true));
        assert!(state.set_resumed(1, true));
        let old_lease = state
            .with_presentation_owner(None, |generation| generation.to_string())
            .unwrap();
        assert!(state.detach(1));
        assert!(state.attach(2, 200, true));
        assert!(state.set_resumed(2, true));
        let mut pending = true;
        assert_eq!(
            state.with_presentation_owner(Some(&old_lease), |_| pending = false),
            None
        );
        assert!(pending, "stale ACK transaction must never reach the inbox");
        assert_eq!(
            state.with_presentation_owner(Some("2"), |_| pending = false),
            Some(())
        );
        assert!(!pending);
    }

    #[test]
    fn early_attach_and_resume_survive_until_ready() {
        let mut state = Lifecycle::default();
        assert!(state.attach(1, 123, false));
        assert!(state.set_resumed(1, true));
        assert!(state.request_restore(false).is_none());
        state.runtime_ready = true;
        assert!(state.activity.unwrap().resumed);
        let owner = state.request_restore(false).unwrap();
        assert!(state.begin_restore(owner, false));
    }

    #[test]
    fn task_removal_retains_session_and_reopens_only_the_new_activity() {
        let mut state = ready();
        state.attach(1, 123, true);
        assert!(state.detach(1));
        assert!(state.prevent_implicit_exit());
        assert!(state.request_restore(false).is_none());
        state.attach(2, 456, false);
        assert!(state.request_restore(true).is_none());
        let owner = state.request_restore(false).unwrap();
        assert_eq!(owner.activity_id, 456);
        assert!(state.begin_restore(owner, false));
        assert!(!state.begin_restore(owner, false));
    }

    #[test]
    fn configuration_recreation_keeps_the_existing_window() {
        let mut state = ready();
        state.attach(1, 123, true);
        state.detach(1);
        state.attach(2, 123, true);
        assert!(state.request_restore(true).is_none());
    }

    #[test]
    fn stale_generation_cannot_detach_background_or_ack_the_replacement() {
        let mut state = ready();
        state.attach(1, 123, false);
        let old = state.request_restore(false).unwrap();
        state.attach(2, 123, false);
        assert!(!state.attach(1, 123, false));
        assert!(!state.attach(0, 123, false));
        assert!(state.set_resumed(2, true));
        assert!(!state.detach(1));
        assert!(!state.set_resumed(1, false));
        assert!(!state.set_webview_ready(1));
        let current = state.request_restore(false).unwrap();
        assert!(!state.begin_restore(old, false));
        assert!(state.begin_restore(current, false));
        assert!(state.activity.unwrap().resumed);
    }

    #[test]
    fn failed_dispatch_is_retryable_and_duplicate_requests_coalesce() {
        let mut state = ready();
        state.attach(1, 123, false);
        assert!(state.request_restore(false).is_some());
        assert!(state.request_restore(false).is_none());
        state.abandon_restore(2);
        assert!(state.request_restore(false).is_none());
        state.abandon_restore(1);
        assert!(state.request_restore(false).is_some());
    }

    #[test]
    fn explicit_exit_is_honored_and_cancels_restoration() {
        let mut state = ready();
        state.attach(1, 123, false);
        let owner = state.request_restore(false).unwrap();
        state.exit();
        assert!(!state.prevent_implicit_exit());
        assert!(!state.begin_restore(owner, false));
        assert!(!state.attach(2, 456, false));
        assert!(!state.set_resumed(1, true));
        assert!(state.request_restore(false).is_none());
    }

    #[test]
    fn publication_holds_authority_lock_and_ready_replays_latest_native_edge() {
        let state = std::sync::Mutex::new(Lifecycle::default());
        state.lock().unwrap().attach(1, 123, false);
        let mut published = Vec::new();
        for event in [
            NativeLifecycleEvent::Resumed(1, true),
            NativeLifecycleEvent::Resumed(1, false),
            NativeLifecycleEvent::Ready,
        ] {
            state.lock().unwrap().transition(event, |foreground| {
                assert!(matches!(
                    state.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ));
                published.push(foreground);
            });
        }
        assert_eq!(published, vec![true, false, false]);
        state
            .lock()
            .unwrap()
            .transition(NativeLifecycleEvent::Detached(1), |foreground| {
                assert!(!foreground)
            });
    }
}
