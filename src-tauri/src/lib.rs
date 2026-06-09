mod paths;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use tauri::webview::DownloadEvent;
use tauri::Manager;

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    not(target_os = "macos")
))]
const TRAY_SHOW_ID: &str = "ratspeak_tray_show";
#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    not(target_os = "macos")
))]
const TRAY_QUIT_ID: &str = "ratspeak_tray_quit";

// WKWebView pointer used by iOS network-path + lifecycle JS injection.
// Process-lifetime ObjC object; only passed back to objc_msgSend, never deref'd.
#[cfg(target_os = "ios")]
static WEBVIEW_PTR: std::sync::atomic::AtomicPtr<objc2::runtime::AnyObject> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

// SAFETY: without this init, btleplug's global_adapter() panics on first use
// and panic=abort terminates the app. Also stashes the JavaVM for BLE peer
// advertising, BT Classic RFCOMM, and android_usb.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn JNI_OnLoad(
    vm: jni::JavaVM,
    _reserved: *mut std::ffi::c_void,
) -> jni::sys::jint {
    match vm.get_env() {
        Ok(env) => match btleplug::platform::init(&env) {
            Ok(()) => {
                rns_interface::ble_rnode::mark_btleplug_initialized();
                tracing::debug!("btleplug initialized from JNI_OnLoad");
            }
            Err(e) => {
                tracing::debug!(error = %e, "btleplug init failed from JNI_OnLoad");
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, "failed to get JNI env in JNI_OnLoad");
        }
    }

    if let Ok(vm2) = unsafe { jni::JavaVM::from_raw(vm.get_java_vm_pointer()) } {
        rns_interface::ble_peer::init_android_jvm(vm2);
    }

    if let Ok(vm3) = unsafe { jni::JavaVM::from_raw(vm.get_java_vm_pointer()) } {
        rns_interface::android_usb::init_vm(vm3);
        tracing::debug!("android_usb JVM initialized from JNI_OnLoad");
    }

    jni::sys::JNI_VERSION_1_6
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn diagnostics_enabled() -> bool {
    env_flag("RATSPEAK_DIAGNOSTICS")
}

fn apply_linux_webkit_rendering_workarounds() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }

    let wayland_session = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|value| value.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false);
    if !wayland_session
        || env_flag("RATSPEAK_DISABLE_WEBKIT_DMABUF_WORKAROUND")
        || std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some()
    {
        return false;
    }

    // WebKitGTK's DMA-BUF renderer can create blank webviews on recent
    // Wayland stacks. Ratspeak is not GPU-heavy, so prefer reliable startup.
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    true
}

fn validate_http_url(raw: &str) -> Result<String, String> {
    let parsed = url::Url::parse(raw.trim()).map_err(|_| "Invalid URL".to_string())?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.into()),
        _ => Err("Only http and https links can be opened".into()),
    }
}

#[tauri::command]
fn open_external_url(url: String) -> Result<(), String> {
    let clean = validate_http_url(&url)?;

    #[cfg(target_os = "ios")]
    {
        open_external_url_ios(&clean)
    }

    #[cfg(target_os = "android")]
    {
        let _ = clean;
        Err("Android external links are opened through the native WebView bridge".into())
    }

    #[cfg(all(not(any(target_os = "android", target_os = "ios")), target_os = "macos"))]
    {
        std::process::Command::new("open")
            .arg(&clean)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to open link: {e}"))
    }

    #[cfg(all(not(any(target_os = "android", target_os = "ios")), target_os = "windows"))]
    {
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", &clean])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to open link: {e}"))
    }

    #[cfg(all(
        not(any(target_os = "android", target_os = "ios")),
        not(any(target_os = "macos", target_os = "windows"))
    ))]
    {
        std::process::Command::new("xdg-open")
            .arg(&clean)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to open link: {e}"))
    }
}

#[cfg(target_os = "ios")]
fn open_external_url_ios(url: &str) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use std::ffi::CString;

    unsafe {
        let ns_string_class = AnyClass::get(c"NSString")
            .ok_or_else(|| "NSString class not found".to_string())?;
        let ns_url_class =
            AnyClass::get(c"NSURL").ok_or_else(|| "NSURL class not found".to_string())?;
        let ui_app_class = AnyClass::get(c"UIApplication")
            .ok_or_else(|| "UIApplication class not found".to_string())?;
        let c_url = CString::new(url).map_err(|_| "Invalid URL".to_string())?;
        let ns_string: *mut AnyObject =
            msg_send![ns_string_class, stringWithUTF8String: c_url.as_ptr()];
        let ns_url: *mut AnyObject = msg_send![ns_url_class, URLWithString: ns_string];
        if ns_url.is_null() {
            return Err("Invalid URL".into());
        }
        let app: *mut AnyObject = msg_send![ui_app_class, sharedApplication];
        if app.is_null() {
            return Err("UIApplication unavailable".into());
        }
        let ok: bool = msg_send![app, openURL: ns_url];
        if ok {
            Ok(())
        } else {
            Err("No application can open this link".into())
        }
    }
}

#[tauri::command]
fn save_image_to_photos(filename: String, mime: String, data_base64: String) -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        save_image_to_photos_ios(&filename, &mime, &data_base64)
    }

    #[cfg(not(target_os = "ios"))]
    {
        let _ = (filename, mime, data_base64);
        Err("Native photo saving is only implemented on iOS through this command".into())
    }
}

#[tauri::command]
async fn request_microphone_permission(_app: tauri::AppHandle) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        use std::time::Duration;

        let (tx, rx) = std::sync::mpsc::channel();
        _app.run_on_main_thread(move || request_microphone_permission_macos(tx))
            .map_err(|e| format!("Could not start microphone permission request: {e}"))?;

        tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(120)))
            .await
            .map_err(|e| format!("Microphone permission task failed: {e}"))?
            .map_err(|_| "Timed out waiting for microphone permission".to_string())?
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(true)
    }
}

#[cfg(target_os = "macos")]
#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

#[cfg(target_os = "macos")]
fn request_microphone_permission_macos(reply: std::sync::mpsc::Sender<Result<bool, String>>) {
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};

    const AV_AUTHORIZATION_STATUS_NOT_DETERMINED: isize = 0;
    const AV_AUTHORIZATION_STATUS_RESTRICTED: isize = 1;
    const AV_AUTHORIZATION_STATUS_DENIED: isize = 2;
    const AV_AUTHORIZATION_STATUS_AUTHORIZED: isize = 3;

    unsafe {
        let Some(capture_device_class) = AnyClass::get(c"AVCaptureDevice") else {
            let _ = reply.send(Err("AVCaptureDevice class not found".to_string()));
            return;
        };
        let Some(ns_string_class) = AnyClass::get(c"NSString") else {
            let _ = reply.send(Err("NSString class not found".to_string()));
            return;
        };
        // AVMediaTypeAudio's NSString value is "soun"; using the value avoids
        // depending on generated AVFoundation bindings for one constant.
        let media_type_audio: *mut AnyObject =
            msg_send![ns_string_class, stringWithUTF8String: c"soun".as_ptr()];
        if media_type_audio.is_null() {
            let _ = reply.send(Err("Could not create AVMediaTypeAudio".into()));
            return;
        }

        let status: isize = msg_send![
            capture_device_class,
            authorizationStatusForMediaType: media_type_audio
        ];
        match status {
            AV_AUTHORIZATION_STATUS_AUTHORIZED => {
                let _ = reply.send(Ok(true));
                return;
            }
            AV_AUTHORIZATION_STATUS_DENIED | AV_AUTHORIZATION_STATUS_RESTRICTED => {
                let _ = reply.send(Ok(false));
                return;
            }
            AV_AUTHORIZATION_STATUS_NOT_DETERMINED => {}
            _ => {
                let _ = reply.send(Ok(false));
                return;
            }
        }

        let completion = RcBlock::new(move |granted: Bool| {
            let _ = reply.send(Ok(granted.as_bool()));
        });
        let _: () = msg_send![
            capture_device_class,
            requestAccessForMediaType: media_type_audio,
            completionHandler: &*completion
        ];
        std::mem::forget(completion);
    }
}

#[cfg(target_os = "ios")]
#[link(name = "Photos", kind = "framework")]
unsafe extern "C" {}

#[cfg(target_os = "ios")]
fn save_image_to_photos_ios(_filename: &str, mime: &str, data_base64: &str) -> Result<(), String> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as B64;
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use std::ptr;

    if !mime.to_ascii_lowercase().starts_with("image/") {
        return Err("Only images can be saved to Photos".into());
    }
    let bytes = B64
        .decode(data_base64)
        .map_err(|_| "Invalid image data".to_string())?;

    unsafe {
        let ns_data_class =
            AnyClass::get(c"NSData").ok_or_else(|| "NSData class not found".to_string())?;
        let ui_image_class =
            AnyClass::get(c"UIImage").ok_or_else(|| "UIImage class not found".to_string())?;
        let data: *mut AnyObject =
            msg_send![ns_data_class, dataWithBytes: bytes.as_ptr(), length: bytes.len()];
        if data.is_null() {
            return Err("Could not decode image data".into());
        }
        let image: *mut AnyObject = msg_send![ui_image_class, imageWithData: data];
        if image.is_null() {
            return Err("Could not create image".into());
        }

        let photo_library_class = AnyClass::get(c"PHPhotoLibrary")
            .ok_or_else(|| "PHPhotoLibrary class not found".to_string())?;
        let asset_request_class = AnyClass::get(c"PHAssetChangeRequest")
            .ok_or_else(|| "PHAssetChangeRequest class not found".to_string())?;
        let library: *mut AnyObject = msg_send![photo_library_class, sharedPhotoLibrary];
        if library.is_null() {
            return Err("Photo library unavailable".into());
        }

        let changes = RcBlock::new(move || {
            let _: *mut AnyObject =
                msg_send![asset_request_class, creationRequestForAssetFromImage: image];
        });
        let mut error: *mut AnyObject = ptr::null_mut();
        let ok: bool = msg_send![
            library,
            performChangesAndWait: &*changes,
            error: &mut error
        ];
        if !ok {
            return Err(if error.is_null() {
                "Could not save image to Photos".into()
            } else {
                "Photo library denied or failed the save".into()
            });
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn diagnostic_file_enabled() -> bool {
    diagnostics_enabled() && env_flag("RATSPEAK_DIAGNOSTIC_FILE")
}

// Silent by default. Source/dev support diagnostics require
// RATSPEAK_DIAGNOSTICS=1, and desktop file logs additionally require
// RATSPEAK_DIAGNOSTIC_FILE=1. RUST_LOG only selects the filter after opt-in.
fn init_tracing() {
    if !diagnostics_enabled() {
        return;
    }

    use tracing_subscriber::EnvFilter;

    #[cfg(any(target_os = "ios", target_os = "android"))]
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    #[cfg(target_os = "ios")]
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(tracing_oslog::OsLogger::new("org.ratspeak.ios", "default"))
            .try_init();
    }

    #[cfg(target_os = "android")]
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        match tracing_android::layer("RatspeakRust") {
            Ok(layer) => {
                let _ = tracing_subscriber::registry()
                    .with(filter)
                    .with(layer)
                    .try_init();
            }
            Err(_) => {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_target(false)
                    .with_ansi(false)
                    .try_init();
            }
        }
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        if diagnostic_file_enabled() {
            let log_dir = dirs::data_local_dir()
                .map(|d| d.join("Ratspeak").join("logs"))
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let _ = std::fs::create_dir_all(&log_dir);
            let file_appender = tracing_appender::rolling::daily(&log_dir, "ratspeak.log");

            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        .with_ansi(true)
                        .with_writer(std::io::stderr),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(true)
                        .with_ansi(false)
                        .with_writer(file_appender),
                )
                .try_init();
        } else {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(true)
                .with_writer(std::io::stderr)
                .try_init();
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let linux_webkit_dmabuf_workaround = apply_linux_webkit_rendering_workarounds();
    init_tracing();
    if linux_webkit_dmabuf_workaround {
        tracing::debug!("disabled WebKitGTK DMA-BUF renderer for Linux Wayland startup");
    }

    let builder = tauri::Builder::default().plugin(tauri_plugin_notification::init());

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        show_main_window(app);
    }));

    // Mobile haptics bridge — navigator.vibrate is a no-op in WKWebView so
    // iOS needs UIImpactFeedbackGenerator via this plugin.
    #[cfg(any(target_os = "ios", target_os = "android"))]
    let builder = {
        let b = builder.plugin(tauri_plugin_haptics::init());
        tracing::info!("[haptics] tauri-plugin-haptics registered");
        b
    };

    builder
        .invoke_handler(tauri::generate_handler![
            open_external_url,
            save_image_to_photos,
            request_microphone_permission,
            ratspeak_tauri::commands::system::api_version,
            ratspeak_tauri::commands::system::api_startup_progress,
            ratspeak_tauri::commands::system::api_setup_status,
            ratspeak_tauri::commands::identity::api_identity,
            ratspeak_tauri::commands::network::api_announces,
            ratspeak_tauri::commands::network::api_alerts,
            ratspeak_tauri::commands::network::api_propagation,
            ratspeak_tauri::commands::network::api_propagation_nodes,
            ratspeak_tauri::commands::network::api_hub_interfaces,
            ratspeak_tauri::commands::messaging::api_conversation,
            ratspeak_tauri::commands::messaging::api_lxmf_conversations,
            ratspeak_tauri::commands::messaging::api_search_messages,
            ratspeak_tauri::commands::messaging::api_files,
            ratspeak_tauri::commands::messaging::api_lxmf_limits,
            ratspeak_tauri::commands::messaging::api_file_download,
            ratspeak_tauri::commands::messaging::send_lxmf_message,
            ratspeak_tauri::commands::messaging::send_reaction,
            ratspeak_tauri::commands::messaging::send_lxmf_reply,
            ratspeak_tauri::commands::messaging::send_lxmf_propagated,
            ratspeak_tauri::commands::messaging::send_lxmf_with_attachment,
            ratspeak_tauri::commands::messaging::cancel_lxmf_message,
            ratspeak_tauri::commands::messaging::get_conversation,
            ratspeak_tauri::commands::messaging::mark_read,
            ratspeak_tauri::commands::messaging::hide_conversation,
            ratspeak_tauri::commands::messaging::delete_conversation,
            ratspeak_tauri::commands::contacts::api_contacts,
            ratspeak_tauri::commands::contacts::api_blocked_contacts,
            ratspeak_tauri::commands::peers::api_get_peers_snapshot,
            ratspeak_tauri::commands::contact_card::api_contact_card,
            ratspeak_tauri::commands::contact_card::api_preview_contact_card,
            ratspeak_tauri::commands::contact_card::import_contact_card,
            ratspeak_tauri::commands::contacts::add_contact,
            ratspeak_tauri::commands::contacts::remove_contact,
            ratspeak_tauri::commands::contacts::block_contact,
            ratspeak_tauri::commands::contacts::unblock_contact,
            ratspeak_tauri::commands::contacts::get_blackhole,
            ratspeak_tauri::commands::contacts::clear_system_blackholes,
            ratspeak_tauri::commands::contacts::purge_unverified_blackholes,
            ratspeak_tauri::commands::contacts::check_contact_status,
            ratspeak_tauri::commands::system::dismiss_alert,
            ratspeak_tauri::commands::network::enable_network_log,
            ratspeak_tauri::commands::network::set_network_log_level,
            ratspeak_tauri::commands::network::set_propagation_node,
            ratspeak_tauri::commands::network::set_propagation_mode,
            ratspeak_tauri::commands::network::set_propagation_hosting,
            ratspeak_tauri::commands::network::set_stamp_settings,
            ratspeak_tauri::commands::network::refresh_propagation_nodes,
            ratspeak_tauri::commands::network::enable_propagation,
            ratspeak_tauri::commands::network::sync_propagation,
            ratspeak_tauri::commands::network::get_propagation_status,
            ratspeak_tauri::commands::network::trigger_announce,
            ratspeak_tauri::commands::network::request_path,
            ratspeak_tauri::commands::network::request_all_paths,
            ratspeak_tauri::commands::identity::switch_identity,
            ratspeak_tauri::commands::interfaces::set_transport_mode,
            ratspeak_tauri::commands::interfaces::network_type_changed,
            ratspeak_tauri::commands::interfaces::set_auto_announce,
            ratspeak_tauri::commands::interfaces::api_app_settings,
            ratspeak_tauri::commands::agents::api_agents,
            ratspeak_tauri::commands::agents::api_agent,
            ratspeak_tauri::commands::agents::api_agent_connection_bundle,
            ratspeak_tauri::commands::agents::create_agent,
            ratspeak_tauri::commands::agents::set_agent_grant,
            ratspeak_tauri::commands::agents::revoke_agent,
            ratspeak_tauri::commands::agents::rotate_agent_token,
            ratspeak_tauri::commands::agents::api_agent_policy,
            ratspeak_tauri::commands::agents::api_agent_policy_defaults,
            ratspeak_tauri::commands::agents::set_agent_policy,
            ratspeak_tauri::commands::agents::api_agent_approvals,
            ratspeak_tauri::commands::agents::api_agent_approval,
            ratspeak_tauri::commands::agents::api_agent_file_inspection,
            ratspeak_tauri::commands::agents::approve_agent_action,
            ratspeak_tauri::commands::agents::reject_agent_action,
            ratspeak_tauri::commands::agents::cancel_agent_action,
            ratspeak_tauri::commands::agents::execute_agent_action,
            ratspeak_tauri::commands::agents::expire_agent_actions,
            ratspeak_tauri::commands::agents::api_agent_audit,
            ratspeak_tauri::commands::interfaces::set_peers_sort,
            ratspeak_tauri::commands::interfaces::set_hardware_lock_timeout,
            ratspeak_tauri::commands::interfaces::set_announce_ratspeak_usage,
            ratspeak_tauri::commands::interfaces::api_notification_settings,
            ratspeak_tauri::commands::interfaces::set_desktop_notifications,
            ratspeak_tauri::commands::interfaces::add_lora_interface,
            ratspeak_tauri::commands::interfaces::update_lora_interface,
            ratspeak_tauri::commands::interfaces::remove_lora_interface,
            ratspeak_tauri::commands::interfaces::pause_interface,
            ratspeak_tauri::commands::interfaces::resume_interface,
            ratspeak_tauri::commands::interfaces::enable_auto_interface,
            ratspeak_tauri::commands::interfaces::disable_auto_interface,
            ratspeak_tauri::commands::interfaces::api_list_network_interfaces,
            ratspeak_tauri::commands::interfaces::add_tcp_connection,
            ratspeak_tauri::commands::interfaces::update_tcp_connection,
            ratspeak_tauri::commands::interfaces::remove_tcp_connection,
            ratspeak_tauri::commands::interfaces::add_tcp_server,
            ratspeak_tauri::commands::interfaces::update_tcp_server,
            ratspeak_tauri::commands::interfaces::remove_tcp_server,
            ratspeak_tauri::commands::interfaces::add_backbone_connection,
            ratspeak_tauri::commands::interfaces::update_backbone_connection,
            ratspeak_tauri::commands::interfaces::remove_backbone_connection,
            ratspeak_tauri::commands::interfaces::add_backbone_server,
            ratspeak_tauri::commands::interfaces::update_backbone_server,
            ratspeak_tauri::commands::interfaces::remove_backbone_server,
            ratspeak_tauri::commands::interfaces::api_rnode_presets,
            ratspeak_tauri::commands::interfaces::api_serial_ports,
            ratspeak_tauri::commands::interfaces::api_ble_available,
            ratspeak_tauri::commands::interfaces::api_ble_scan,
            ratspeak_tauri::commands::interfaces::api_ble_peer_available,
            ratspeak_tauri::commands::interfaces::api_ble_peer_status,
            ratspeak_tauri::commands::interfaces::api_connection_history,
            ratspeak_tauri::commands::interfaces::api_delete_connection_history,
            ratspeak_tauri::commands::network::api_network_blackhole,
            ratspeak_tauri::commands::network::api_path_query,
            ratspeak_tauri::commands::identity::api_list_identities,
            ratspeak_tauri::commands::identity::api_create_identity,
            ratspeak_tauri::commands::identity::api_import_identity,
            ratspeak_tauri::commands::identity::api_import_identity_base64,
            // `seed` is enabled on every target (desktop via `hardware`, mobile explicitly).
            ratspeak_tauri::commands::identity::restore_seed_identity,
            ratspeak_tauri::commands::identity::set_identity_passcode,
            ratspeak_tauri::commands::identity::remove_identity_passcode,
            ratspeak_tauri::commands::identity::reveal_identity_mnemonic,
            ratspeak_tauri::commands::identity::api_preview_identity_import_base64,
            ratspeak_tauri::commands::identity::api_activate_identity,
            ratspeak_tauri::commands::identity::api_export_identity,
            ratspeak_tauri::commands::identity::api_export_identity_base64,
            ratspeak_tauri::commands::identity::api_export_identity_backup_base64,
            ratspeak_tauri::commands::identity::api_export_identity_reticulum_base64,
            ratspeak_tauri::commands::identity::api_export_identity_reticulum_base32,
            ratspeak_tauri::commands::identity::api_update_identity,
            ratspeak_tauri::commands::identity::api_delete_identity,
            ratspeak_tauri::commands::identity::api_set_display_name,
            ratspeak_tauri::commands::identity::set_identity_status,
            ratspeak_tauri::commands::system::api_setup_complete,
            ratspeak_tauri::commands::system::api_setup_restart,
            ratspeak_tauri::commands::system::api_set_foreground,
            ratspeak_tauri::commands::system::api_unread_count,
            ratspeak_tauri::commands::system::api_system_restart,
            ratspeak_tauri::commands::system::api_system_shutdown,
            ratspeak_tauri::commands::system::api_database_stats,
            ratspeak_tauri::commands::system::api_clear_paths,
            ratspeak_tauri::commands::system::api_clear_announces,
            ratspeak_tauri::commands::system::api_clear_messages,
            ratspeak_tauri::commands::system::api_clear_contacts,
            ratspeak_tauri::commands::system::api_reset_database,
            ratspeak_tauri::commands::system::api_identity_reset,
            ratspeak_tauri::commands::system::api_factory_reset,
            ratspeak_tauri::commands::games::send_game_action,
            ratspeak_tauri::commands::games::resend_last_game_action,
            ratspeak_tauri::commands::games::get_active_games,
            ratspeak_tauri::commands::games::get_all_game_sessions,
            ratspeak_tauri::commands::games::mark_game_read,
            ratspeak_tauri::commands::games::delete_game_session,
            ratspeak_tauri::commands::games::get_game_session_detail,
            ratspeak_tauri::commands::games::get_available_games,
            ratspeak_tauri::commands::ble::enable_ble_peer_interface,
            ratspeak_tauri::commands::ble::disable_ble_peer_interface,
            ratspeak_tauri::commands::ble::disconnect_ble_peer,
            ratspeak_tauri::commands::ble::scan_ble_mesh_peers,
            ratspeak_tauri::commands::ble::scan_ble_devices,
            ratspeak_tauri::commands::ble::ble_rnode_bridge_ready,
            ratspeak_tauri::commands::ble::cancel_ble_connect,
            ratspeak_tauri::commands::ble::disconnect_ble_rnode,
            ratspeak_tauri::commands::ble::submit_ble_rnode_passkey,
            ratspeak_tauri::commands::ble::cancel_ble_rnode_pairing,
            #[cfg(feature = "lxst-voice")]
            ratspeak_tauri::commands::voice::voice_start_service,
            #[cfg(feature = "lxst-voice")]
            ratspeak_tauri::commands::voice::voice_stop_service,
            #[cfg(feature = "lxst-voice")]
            ratspeak_tauri::commands::voice::voice_status,
            #[cfg(feature = "lxst-voice")]
            ratspeak_tauri::commands::voice::voice_call,
            #[cfg(feature = "lxst-voice")]
            ratspeak_tauri::commands::voice::voice_answer,
            #[cfg(feature = "lxst-voice")]
            ratspeak_tauri::commands::voice::voice_reject,
            #[cfg(feature = "lxst-voice")]
            ratspeak_tauri::commands::voice::voice_hangup,
            #[cfg(feature = "lxst-voice")]
            ratspeak_tauri::commands::voice::voice_set_microphone_muted,
            #[cfg(feature = "lxst-voice")]
            ratspeak_tauri::commands::voice::voice_restart_speaker,
            // Hardware (PIV) identity commands — desktop only (pcsc).
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_detect,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_provision_recoverable,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_provision_hardware_only,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_import_existing,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_restore,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_stage_unlock,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_reset_piv,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_change_pin,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_remove,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_unlock,
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            ratspeak_tauri::commands::hardware::hw_activate_and_unlock,
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            let data_dir = paths::resolve_data_dir(&handle);
            std::fs::create_dir_all(&data_dir).ok();
            tracing::debug!(path = %data_dir.display(), "resolved Ratspeak data directory");

            // setup() has no Tokio runtime; leak one for process-lifetime tasks.
            let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
            let tauri_handle = handle.clone();
            let state = rt.block_on(async move {
                ratspeak_tauri::init_core(data_dir, tauri_handle)
                    .await
                    .expect("Failed to start Ratspeak core")
            });
            std::mem::forget(rt);

            app.manage(state);

            // Programmatic window construction so we can attach on_download.
            let platform_script = if cfg!(any(target_os = "android", target_os = "ios")) {
                "window.__RATSPEAK_MOBILE__ = true;"
            } else {
                "window.__RATSPEAK_DESKTOP__ = true;"
            };
            let diagnostics_script = if diagnostics_enabled() {
                "window.__RATSPEAK_DIAGNOSTICS__ = true;"
            } else {
                ""
            };
            let initialization_script = format!("{platform_script}{diagnostics_script}");
            let window = tauri::WebviewWindowBuilder::new(
                &handle,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .initialization_script(initialization_script);

            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            let window = window
                .title("Ratspeak")
                .inner_size(1200.0, 800.0)
                .min_inner_size(800.0, 600.0)
                .disable_drag_drop_handler();

            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            let window = window.on_download(|_webview, event| {
                match event {
                    DownloadEvent::Requested { url, destination } => {
                        let filename = url
                            .path_segments()
                            .and_then(|mut s| s.next_back())
                            .unwrap_or("download")
                            .to_string();

                        // Strip "1693824532000_doc.pdf" → "doc.pdf".
                        let clean_name = filename
                            .find('_')
                            .map(|pos| &filename[pos + 1..])
                            .unwrap_or(&filename);

                        if let Some(path) =
                            rfd::FileDialog::new().set_file_name(clean_name).save_file()
                        {
                            *destination = path;
                            true
                        } else {
                            false
                        }
                    }
                    DownloadEvent::Finished { url, success, .. } => {
                        if !success {
                            tracing::debug!(%url, "download failed");
                        }
                        true
                    }
                    _ => true,
                }
            });

            let _window = window.build()?;

            #[cfg(all(
                not(any(target_os = "android", target_os = "ios")),
                not(target_os = "macos")
            ))]
            install_desktop_tray(app)?;

            // iOS edge-to-edge: env(safe-area-inset-*) + viewport-fit=cover
            // drive the layout; disable WKWebView's automatic inset adjustment.
            #[cfg(target_os = "ios")]
            {
                // Swizzle WKContentView.inputAccessoryView → nil so the keyboard
                // sits flush under the compose bar (no prev/next/done toolbar).
                unsafe {
                    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
                    use std::ffi::{c_char, CStr};

                    let class_name = CStr::from_bytes_with_nul_unchecked(b"WKContentView\0");
                    if let Some(cls) = AnyClass::get(class_name) {
                        let sel = objc2::sel!(inputAccessoryView);

                        unsafe extern "C-unwind" fn nil_input_accessory(
                            _this: *mut AnyObject,
                            _cmd: Sel,
                        ) -> *const AnyObject {
                            std::ptr::null()
                        }

                        let imp: Imp = std::mem::transmute(
                            nil_input_accessory
                                as unsafe extern "C-unwind" fn(
                                    *mut AnyObject,
                                    Sel,
                                )
                                    -> *const AnyObject,
                        );
                        // ObjC type encoding: returns @, takes self (@), selector (:).
                        let types = b"@@:\0".as_ptr() as *const c_char;
                        objc2::ffi::class_replaceMethod(
                            cls as *const AnyClass as *mut AnyClass,
                            sel,
                            imp,
                            types,
                        );
                    }
                }

                // Stash the WKWebView pointer for nw_path_monitor + lifecycle
                // JS injection without plumbing the Tauri handle through.
                let _ = _window.with_webview(|webview| unsafe {
                    use objc2::rc::Retained;
                    use objc2::runtime::AnyObject;
                    use objc2_ui_kit::UIScrollView;
                    use objc2_ui_kit::UIScrollViewContentInsetAdjustmentBehavior;

                    let wk_webview_ptr = webview.inner() as *mut AnyObject;
                    WEBVIEW_PTR.store(wk_webview_ptr, std::sync::atomic::Ordering::Release);

                    let wk_webview: &AnyObject = &*(wk_webview_ptr as *const AnyObject);
                    let scroll_view: Retained<UIScrollView> =
                        objc2::msg_send![wk_webview, scrollView];
                    scroll_view.setContentInsetAdjustmentBehavior(
                        UIScrollViewContentInsetAdjustmentBehavior::Never,
                    );
                });

                // NSNotificationCenter UIApplicationDidEnterBackground /
                // DidBecomeActive — fires before the WKWebView JS event loop,
                // so is_foreground stays accurate during paused fetches.
                unsafe { register_ios_lifecycle_observers() };

                // nw_path_monitor for wifi↔cellular handoff (WKWebView lacks
                // navigator.connection).
                unsafe { register_ios_network_observer() };
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Ratspeak")
        .run(|app_handle, event| {
            #[cfg(any(target_os = "ios", target_os = "android"))]
            let _ = (&app_handle, &event);

            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            match event {
                tauri::RunEvent::WindowEvent {
                    label,
                    event: tauri::WindowEvent::CloseRequested { api, .. },
                    ..
                } if label == "main" => {
                    api.prevent_close();
                    set_desktop_foreground(app_handle, false);
                    if let Some(window) = app_handle.get_webview_window("main") {
                        let _ = window.hide();
                    }
                }
                tauri::RunEvent::ExitRequested { .. } => {
                    shutdown_desktop_core_for_exit(app_handle);
                }
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Reopen {
                    has_visible_windows,
                    ..
                } if !has_visible_windows => {
                    show_main_window(app_handle);
                }
                _ => {}
            }
        });
}

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    not(target_os = "macos")
))]
fn install_desktop_tray(app: &mut tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let show = MenuItem::with_id(app, TRAY_SHOW_ID, "Show Ratspeak", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, TRAY_QUIT_ID, "Quit Ratspeak", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut tray = TrayIconBuilder::with_id("ratspeak")
        .menu(&menu)
        .tooltip("Ratspeak")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            if event.id() == TRAY_SHOW_ID {
                show_main_window(app);
            } else if event.id() == TRAY_QUIT_ID {
                app.exit(0);
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }
    tray.build(app)?;
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    set_desktop_foreground(app, true);
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn set_desktop_foreground(app: &tauri::AppHandle, foreground: bool) {
    if let Some(state) = app.try_state::<std::sync::Arc<ratspeak_tauri::state::AppState>>() {
        ratspeak_tauri::commands::system::set_foreground_state(state.inner(), foreground);
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn shutdown_desktop_core_for_exit(app: &tauri::AppHandle) {
    tauri::async_runtime::block_on(async {
        if let Some(state) = app.try_state::<std::sync::Arc<ratspeak_tauri::state::AppState>>() {
            let state = std::sync::Arc::clone(state.inner());
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                ratspeak_tauri::shutdown_rns_lxmf(&state),
            )
            .await;
        }

        // Release the WinRT GattServiceProvider before exit so Windows does
        // not leave a short-lived stale BLE advertisement behind.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            ratspeak_tauri::shutdown_ble_peer_for_exit(),
        )
        .await;
    });
}

/// Drive foreground/background transitions through `api_set_foreground` so the
/// Rust core is the single entry point for lifecycle changes.
#[cfg(target_os = "ios")]
fn post_ios_lifecycle(foreground: bool) {
    let js = format!(
        "if (typeof RS !== 'undefined' && RS.invoke) {{ \
         RS.invoke('api_set_foreground', {{ args: {{ foreground: {foreground} }} }}).catch(function() {{}}); }}"
    );
    inject_js(&js);
}

#[cfg(target_os = "ios")]
fn inject_js(js: &str) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use std::ffi::CString;
    use std::sync::atomic::Ordering;

    let webview = WEBVIEW_PTR.load(Ordering::Acquire);
    if webview.is_null() {
        return;
    }
    let cstring = match CString::new(js) {
        Ok(s) => s,
        Err(_) => return,
    };

    unsafe {
        let ns_string_class = match AnyClass::get(c"NSString") {
            Some(c) => c,
            None => return,
        };
        let js_nsstring: *mut AnyObject =
            msg_send![ns_string_class, stringWithUTF8String: cstring.as_ptr()];
        if js_nsstring.is_null() {
            return;
        }

        // SAFETY: WKWebView is main-thread-only; both callers
        // (nw_path_monitor main queue, NSNotificationCenter main run loop)
        // already run on the main thread.
        let _: () = msg_send![
            webview,
            evaluateJavaScript: js_nsstring,
            completionHandler: std::ptr::null::<AnyObject>(),
        ];
    }
}

#[cfg(target_os = "ios")]
unsafe fn register_ios_lifecycle_observers() {
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use std::ffi::CStr;
    use std::ptr;

    let nc_class = match AnyClass::get(c"NSNotificationCenter") {
        Some(c) => c,
        None => {
            tracing::debug!("NSNotificationCenter class not found");
            return;
        }
    };
    let center: *mut AnyObject = msg_send![nc_class, defaultCenter];
    if center.is_null() {
        return;
    }

    let ns_string_class = match AnyClass::get(c"NSString") {
        Some(c) => c,
        None => return,
    };

    let make_name = |name: &CStr| -> *mut AnyObject {
        msg_send![ns_string_class, stringWithUTF8String: name.as_ptr()]
    };

    let bg_name = make_name(c"UIApplicationDidEnterBackgroundNotification");
    let fg_name = make_name(c"UIApplicationDidBecomeActiveNotification");
    if bg_name.is_null() || fg_name.is_null() {
        return;
    }

    // NSNotificationCenter retains the block; RcBlock can drop at end of fn.
    let bg_block = RcBlock::new(|_n: *mut AnyObject| post_ios_lifecycle(false));
    let fg_block = RcBlock::new(|_n: *mut AnyObject| post_ios_lifecycle(true));

    // Observers live for process lifetime; token discarded.
    let _: *mut AnyObject = msg_send![
        center,
        addObserverForName: bg_name,
        object: ptr::null::<AnyObject>(),
        queue: ptr::null::<AnyObject>(),
        usingBlock: &*bg_block,
    ];
    let _: *mut AnyObject = msg_send![
        center,
        addObserverForName: fg_name,
        object: ptr::null::<AnyObject>(),
        queue: ptr::null::<AnyObject>(),
        usingBlock: &*fg_block,
    ];
}

// NWPathMonitor is Swift-only (no ObjC class); bind the C ABI directly.
// nw_path_monitor_t / nw_path_t are opaque libdispatch-backed handles.
#[cfg(target_os = "ios")]
#[link(name = "Network", kind = "framework")]
extern "C" {
    fn nw_path_monitor_create() -> *mut std::ffi::c_void;
    fn nw_path_monitor_set_update_handler(
        monitor: *mut std::ffi::c_void,
        handler: *const std::ffi::c_void,
    );
    fn nw_path_monitor_set_queue(monitor: *mut std::ffi::c_void, queue: *mut std::ffi::c_void);
    fn nw_path_monitor_start(monitor: *mut std::ffi::c_void);
    fn nw_path_get_status(path: *mut std::ffi::c_void) -> i32;
    fn nw_path_uses_interface_type(path: *mut std::ffi::c_void, interface_type: i32) -> bool;
}

// dispatch_get_main_queue() expands to &_dispatch_main_q.
#[cfg(target_os = "ios")]
extern "C" {
    static _dispatch_main_q: u8;
}

// NWPath.h / NWInterface.h enum values.
#[cfg(target_os = "ios")]
const NW_PATH_STATUS_SATISFIED: i32 = 1;
#[cfg(target_os = "ios")]
const NW_INTERFACE_TYPE_WIFI: i32 = 1;
#[cfg(target_os = "ios")]
const NW_INTERFACE_TYPE_CELLULAR: i32 = 2;
#[cfg(target_os = "ios")]
const NW_INTERFACE_TYPE_WIRED: i32 = 3;

#[cfg(target_os = "ios")]
unsafe fn classify_path(path: *mut std::ffi::c_void) -> &'static str {
    let status = nw_path_get_status(path);
    if status != NW_PATH_STATUS_SATISFIED {
        return "none";
    }
    if nw_path_uses_interface_type(path, NW_INTERFACE_TYPE_WIFI) {
        return "wifi";
    }
    if nw_path_uses_interface_type(path, NW_INTERFACE_TYPE_CELLULAR) {
        return "cellular";
    }
    if nw_path_uses_interface_type(path, NW_INTERFACE_TYPE_WIRED) {
        return "ethernet";
    }
    "unknown"
}

#[cfg(target_os = "ios")]
fn inject_network_type_change_js(network_type: &str) {
    // typeof guard covers the early-boot window before state.js loads.
    let js = format!(
        "if (typeof RS !== 'undefined' && RS.invoke) {{ \
         RS.invoke('network_type_changed', {{ args: {{ network_type: '{network_type}' }} }}).catch(function() {{}}); }}"
    );
    inject_js(&js);
}

#[cfg(target_os = "ios")]
unsafe fn register_ios_network_observer() {
    use block2::RcBlock;

    let monitor = nw_path_monitor_create();
    if monitor.is_null() {
        tracing::debug!("nw_path_monitor_create returned nil");
        return;
    }

    // Network.framework retains the block; RcBlock can drop at end of fn.
    let block = RcBlock::new(|path: *mut std::ffi::c_void| {
        if path.is_null() {
            return;
        }
        let network_type = unsafe { classify_path(path) };
        inject_network_type_change_js(network_type);
    });
    nw_path_monitor_set_update_handler(monitor, &*block as *const _ as *const std::ffi::c_void);

    // Main queue so evaluateJavaScript runs on the main thread directly.
    let main_queue = std::ptr::addr_of!(_dispatch_main_q) as *mut std::ffi::c_void;
    nw_path_monitor_set_queue(monitor, main_queue);

    nw_path_monitor_start(monitor);
    // Dispatch queue retains the monitor; no nw_release needed.
}
