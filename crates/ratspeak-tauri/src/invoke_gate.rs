//! One admission check in front of every IPC command, from in-memory state
//! only. Commands keep their own checks as defense in depth.

use std::sync::Arc;

use tauri::Manager;
use tauri::ipc::Invoke;

use crate::error::AppError;
use crate::helpers;
use crate::state::AppState;

/// Commands that still answer while no identity is active: the shell,
/// platform probes, startup and setup, app settings and identity management.
/// The gate refuses every other command once it knows none is active.
pub(crate) const IDENTITY_FREE: &[&str] = &[
    // Shell.
    "open_external_url",
    "open_support_email",
    "set_window_decorations",
    "save_image_to_photos",
    "request_microphone_permission",
    "open_mobile_app_settings",
    "copy_secret",
    // Platform and runtime probes, read once at page load.
    "api_ble_available",
    "api_ble_peer_available",
    "api_lxmf_limits",
    "voice_status",
    // Startup, setup and reset.
    "api_version",
    "api_startup_progress",
    "api_setup_status",
    "api_setup_complete",
    "api_setup_restart",
    "api_set_foreground",
    "api_factory_reset",
    // App settings.
    "api_app_settings",
    "api_notification_settings",
    "set_appearance",
    "set_native_theme",
    "set_text_scale",
    "set_desktop_notifications",
    "set_notification_preview",
    "set_hide_known_spam_peers",
    "set_paper_messages_enabled",
    "set_close_button_action",
    "set_lxmf_limit_1mb",
    "set_peers_sort",
    "accept_public_channel_consent",
    "set_auto_announce",
    "set_hardware_lock_timeout",
    "set_developer_mode",
    "set_announce_ratspeak_usage",
    "set_android_ble_rnode_auto_resume",
    "set_activity_identity_protection",
    // Identities.
    "api_identity",
    "api_list_identities",
    "api_create_identity",
    "api_import_identity",
    "api_import_identity_base64",
    "api_preview_identity_import_base64",
    "restore_seed_identity",
    "switch_identity",
    "api_activate_identity",
    "api_update_identity",
    "api_delete_identity",
    "unlock_identity",
    "set_identity_passcode",
    "remove_identity_passcode",
    "reveal_identity_mnemonic",
    "api_export_identity_backup_base64",
    "api_export_identity_reticulum_base64",
    "api_export_identity_reticulum_base32",
    "hw_detect",
    "hw_provision_recoverable",
    "hw_provision_hardware_only",
    "hw_import_existing",
    "hw_restore",
    "hw_reset_piv",
    "hw_change_pin",
    "hw_remove",
    "hw_unlock",
    "hw_activate_and_unlock",
];

/// Commands that still answer while the active identity is locked: the shell,
/// platform probes, startup, setup and reset, app settings and identity
/// management. The rest is the locked identity's and waits for its unlock.
pub(crate) const LOCK_FREE: &[&str] = &[
    // Shell.
    "open_external_url",
    "open_support_email",
    "set_window_decorations",
    "save_image_to_photos",
    "request_microphone_permission",
    "open_mobile_app_settings",
    "copy_secret",
    // Platform probes.
    "api_ble_available",
    "api_ble_peer_available",
    // Startup, setup and reset.
    "api_version",
    "api_startup_progress",
    "api_setup_status",
    "api_setup_restart",
    "api_set_foreground",
    "api_factory_reset",
    // App settings.
    "api_app_settings",
    "api_notification_settings",
    "set_appearance",
    "set_native_theme",
    "set_text_scale",
    "set_desktop_notifications",
    "set_notification_preview",
    "set_hide_known_spam_peers",
    "set_paper_messages_enabled",
    "set_close_button_action",
    "set_lxmf_limit_1mb",
    "set_peers_sort",
    "accept_public_channel_consent",
    "set_auto_announce",
    "set_hardware_lock_timeout",
    "set_developer_mode",
    "set_announce_ratspeak_usage",
    "set_android_ble_rnode_auto_resume",
    "set_activity_identity_protection",
    // Identities: unlock, switch, add and remove.
    "api_identity",
    "api_list_identities",
    "api_create_identity",
    "api_import_identity",
    "api_import_identity_base64",
    "api_preview_identity_import_base64",
    "restore_seed_identity",
    "switch_identity",
    "api_activate_identity",
    "api_delete_identity",
    "unlock_identity",
    "set_identity_passcode",
    "remove_identity_passcode",
    "reveal_identity_mnemonic",
    "api_export_identity_backup_base64",
    "api_export_identity_reticulum_base64",
    "api_export_identity_reticulum_base32",
    "hw_detect",
    "hw_provision_recoverable",
    "hw_provision_hardware_only",
    "hw_import_existing",
    "hw_restore",
    "hw_reset_piv",
    "hw_change_pin",
    "hw_remove",
    "hw_unlock",
    "hw_activate_and_unlock",
];

/// Wraps the generated command handler in the admission check. A core that
/// failed to start answers no command at all; its window is static.
pub fn gated<R: tauri::Runtime>(
    commands: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke| {
        let webview = invoke.message.webview_ref();
        let refused = if webview.try_state::<crate::StartupError>().is_some() {
            Some(AppError::new("startup_error", "Ratspeak could not start"))
        } else {
            webview
                .try_state::<Arc<AppState>>()
                .and_then(|state| refusal(&state, invoke.message.command()))
        };
        match refused {
            Some(error) => {
                invoke.resolver.reject(error);
                true
            }
            None => commands(invoke),
        }
    }
}

/// Why `command` must not run now, if it must not.
fn refusal(state: &AppState, command: &str) -> Option<AppError> {
    if !IDENTITY_FREE.contains(&command) && helpers::active_identity_known_absent(state) {
        return Some(AppError::conflict("no active identity"));
    }
    if !LOCK_FREE.contains(&command) && state.hw_locked_hash().is_some() {
        return Some(AppError::new("locked", "Unlock this identity first."));
    }
    None
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tauri::test::{
        INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
    };

    #[test]
    fn setup_admission_allows_first_run_and_rejects_a_locked_identity() {
        let root = tempfile::tempdir().unwrap();
        crate::lxmf::initialize_storage_namespace(root.path()).unwrap();
        let config = crate::config::DashboardConfig::from_env_and_defaults(root.path().into());
        let pool = crate::db::init_pool(root.path()).unwrap();
        crate::db::init_schema(&pool).unwrap();
        let state = crate::state::AppState::new(
            config,
            pool,
            std::sync::Arc::new(ratspeak_core::NoopEmitter),
            std::sync::Arc::new(ratspeak_core::NoopNotifier),
        );
        assert!(super::refusal(&state, "api_setup_complete").is_none());

        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        crate::db::save_identity(
            &state.db,
            hash,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "Personal",
            "Alice",
        );
        crate::db::set_active_identity(&state.db, hash).unwrap();
        assert!(super::refusal(&state, "api_setup_complete").is_none());

        state.set_hw_locked(Some(hash.to_owned()));
        assert_eq!(
            super::refusal(&state, "api_setup_complete").unwrap().code,
            "locked"
        );
    }

    fn invoke(window: &tauri::WebviewWindow<MockRuntime>, command: &str) -> Result<Value, Value> {
        get_ipc_response(
            window,
            tauri::webview::InvokeRequest {
                cmd: command.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(any(windows, target_os = "android")) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(json!({})),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.into(),
            },
        )
        .map(|response| response.deserialize().unwrap())
    }

    fn window(failed: bool) -> (tauri::App<MockRuntime>, tauri::WebviewWindow<MockRuntime>) {
        let builder = mock_builder();
        let builder = if failed {
            builder.manage(crate::StartupError::Storage)
        } else {
            builder
        };
        let app = builder
            .invoke_handler(super::gated(tauri::generate_handler![
                crate::commands::system::api_version
            ]))
            .build(mock_context(noop_assets()))
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        (app, window)
    }

    #[test]
    fn a_failed_start_refuses_even_commands_that_need_no_state() {
        let (_app, started) = window(false);
        assert_eq!(invoke(&started, "api_version").unwrap()["name"], "Ratspeak");

        let (_app, failed) = window(true);
        let refused = invoke(&failed, "api_version").unwrap_err();
        assert_eq!(refused["code"], "startup_error");
    }
}
