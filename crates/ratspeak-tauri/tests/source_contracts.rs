use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn read_source(path: impl AsRef<Path>) -> std::io::Result<String> {
    fs::read_to_string(path).map(|source| source.replace("\r\n", "\n").replace('\r', "\n"))
}

#[test]
fn privacy_announce_usage_setting_is_wired() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    assert!(index.contains("data-settings-title=\"Privacy\""));
    assert!(index.contains("Privacy related preferences"));
    assert!(index.contains("Announce Ratspeak usage"));
    assert!(index.contains("Let others know you support games, calls, and extra features."));
    assert!(index.contains("id=\"announce-ratspeak-usage-toggle\" checked"));

    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    assert!(settings_js.contains("api_app_settings"));
    assert!(settings_js.contains("set_announce_ratspeak_usage"));
    assert!(settings_js.contains("auto_announce_interval"));
    assert!(settings_js.contains("announce_ratspeak_usage"));

    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    assert!(interfaces_rs.contains("pub async fn api_app_settings"));
    assert!(interfaces_rs.contains("\"auto_announce_interval\""));
    assert!(interfaces_rs.contains("\"announce_ratspeak_usage\""));
    assert!(interfaces_rs.contains("db::set_setting(&p, \"announce_ratspeak_usage\""));

    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    assert!(tauri_lib.contains("api_app_settings"));
    assert!(tauri_lib.contains("set_announce_ratspeak_usage"));

    let system_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/system.rs")).expect("system");
    let reset_body = system_rs
        .split("pub async fn api_reset_database")
        .nth(1)
        .and_then(|tail| tail.split("pub async fn api_identity_reset").next())
        .expect("reset database body");
    assert!(!reset_body.contains("\"settings\""));
}

#[test]
fn ratspeak_capability_marker_drives_name_badge() {
    let root = repo_root();
    let peers_cache_js =
        read_source(root.join("dashboard/static/js/peers_cache.js")).expect("peers cache");
    assert!(peers_cache_js.contains("function ratspeakDisplayNameHtml"));
    assert!(peers_cache_js.contains("ratspeak-name-badge"));
    assert!(peers_cache_js.contains("ratspeak.client"));
    assert!(peers_cache_js.contains("supports_ratspeak"));
    assert!(peers_cache_js.contains("supportsRatspeakFeatures"));

    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    assert!(views_css.contains(".ratspeak-name-badge"));

    let identity_js =
        read_source(root.join("dashboard/static/js/identity.js")).expect("identity js");
    assert!(!identity_js.contains("ratspeak-avatar-glow"));
}

#[test]
fn linux_package_metadata_is_explicit_for_app_stores() {
    let root = repo_root();
    let summary = "Ratspeak: An all-in-one Reticulum & LXMF client in Rust.";
    let homepage = "https://github.com/ratspeak/Ratspeak";
    let metainfo_path = "resources/linux/org.ratspeak.desktop.metainfo.xml";
    let desktop_template_path = "resources/linux/Ratspeak.desktop";

    let cargo_toml = read_source(root.join("src-tauri/Cargo.toml")).expect("tauri Cargo.toml");
    assert!(cargo_toml.contains(&format!("description = \"{summary}\"")));
    assert!(cargo_toml.contains(&format!("homepage = \"{homepage}\"")));
    assert!(cargo_toml.contains(&format!("repository = \"{homepage}\"")));
    assert!(
        !cargo_toml.contains("Ratspeak \u{2014}"),
        "Linux package descriptions must stay ASCII-clean for app-store display"
    );

    let tauri_config = read_source(root.join("src-tauri/tauri.conf.json")).expect("tauri config");
    let tauri_config: serde_json::Value =
        serde_json::from_str(&tauri_config).expect("valid tauri config json");
    let bundle = tauri_config
        .get("bundle")
        .and_then(|value| value.as_object())
        .expect("bundle config");
    assert_eq!(
        bundle.get("publisher").and_then(|value| value.as_str()),
        Some("Ratspeak Contributors")
    );
    assert_eq!(
        bundle.get("homepage").and_then(|value| value.as_str()),
        Some(homepage)
    );
    assert_eq!(
        bundle
            .get("shortDescription")
            .and_then(|value| value.as_str()),
        Some(summary)
    );
    assert_eq!(
        bundle
            .get("longDescription")
            .and_then(|value| value.as_str()),
        Some(homepage)
    );
    assert_eq!(
        bundle.get("category").and_then(|value| value.as_str()),
        Some("SocialNetworking")
    );

    let icons = bundle
        .get("icon")
        .and_then(|value| value.as_array())
        .expect("bundle icons");
    for expected in [
        "icons/32x32.png",
        "icons/64x64.png",
        "icons/128x128.png",
        "icons/icon.png",
    ] {
        assert!(
            icons.iter().any(|value| value.as_str() == Some(expected)),
            "Linux bundles must include {expected} for hicolor/app-store icon lookup"
        );
    }

    let linux = bundle
        .get("linux")
        .and_then(|value| value.as_object())
        .expect("linux bundle config");
    for target in ["deb", "rpm"] {
        let config = linux
            .get(target)
            .and_then(|value| value.as_object())
            .expect("linux package target config");
        assert_eq!(
            config
                .get("desktopTemplate")
                .and_then(|value| value.as_str()),
            Some(desktop_template_path)
        );
        let files = config
            .get("files")
            .and_then(|value| value.as_object())
            .expect("linux package custom files");
        assert_eq!(
            files
                .get("/usr/share/metainfo/org.ratspeak.desktop.metainfo.xml")
                .and_then(|value| value.as_str()),
            Some(metainfo_path)
        );
    }
    let appimage_files = linux
        .get("appimage")
        .and_then(|value| value.get("files"))
        .and_then(|value| value.as_object())
        .expect("appimage custom files");
    assert_eq!(
        appimage_files
            .get("/usr/share/metainfo/org.ratspeak.desktop.metainfo.xml")
            .and_then(|value| value.as_str()),
        Some(metainfo_path)
    );

    let desktop =
        read_source(root.join("src-tauri/resources/linux/Ratspeak.desktop")).expect("desktop");
    assert!(desktop.contains("Name={{name}}"));
    assert!(desktop.contains("Comment={{comment}}"));
    assert!(desktop.contains("Icon={{icon}}"));
    assert!(desktop.contains("Categories={{categories}}Chat;InstantMessaging;"));
    assert!(desktop.contains("StartupNotify=true"));

    let metainfo = read_source(root.join("src-tauri").join(metainfo_path)).expect("metainfo");
    assert!(metainfo.contains("<name>Ratspeak</name>"));
    assert!(metainfo.contains(
        "<summary>Ratspeak: An all-in-one Reticulum &amp; LXMF client in Rust.</summary>"
    ));
    assert!(metainfo.contains("<p>https://github.com/ratspeak/Ratspeak</p>"));
    assert!(metainfo.contains("<developer_name>Ratspeak Contributors</developer_name>"));
    assert!(metainfo.contains("<url type=\"homepage\">https://github.com/ratspeak/Ratspeak</url>"));
    assert!(metainfo.contains("<launchable type=\"desktop-id\">Ratspeak.desktop</launchable>"));
    assert!(metainfo.contains("<icon type=\"stock\">ratspeak</icon>"));
}

#[test]
fn ratspeak_commands_use_current_rns_handle_not_process_singleton() {
    let root = repo_root();
    for rel in [
        "crates/ratspeak-tauri/src/commands/interfaces.rs",
        "crates/ratspeak-tauri/src/commands/ble.rs",
    ] {
        let path = root.join(rel);
        let source = read_source(&path).expect("source file");
        assert!(
            !source.contains("get_instance()"),
            "{} must use AppState.rns so soft restarts do not keep stale handles",
            rel
        );
    }
}

#[test]
fn android_service_is_not_sticky_without_runtime_ownership() {
    let service =
        read_source(repo_root().join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakService.kt",
        ))
        .expect("service source");

    assert!(service.contains("return START_NOT_STICKY"));
    assert!(!service.contains("return START_STICKY"));
}

#[test]
fn game_event_init_does_not_depend_on_missing_network_watcher() {
    let source =
        read_source(repo_root().join("dashboard/static/js/games_tab.js")).expect("js source");

    assert!(source.contains("typeof _startNetworkUnstableWatcher === 'function'"));
    assert!(!source.contains("_gameEventsReady = true;\n        _startNetworkUnstableWatcher();"));
}

#[test]
fn games_new_sheet_uses_shared_mobile_bottom_sheet_width() {
    let root = repo_root();
    let games_js = read_source(root.join("dashboard/static/js/games_tab.js")).expect("games js");
    assert!(games_js.contains(r#"class="bottom-sheet games-new-dialog""#));

    let games_css = read_source(root.join("dashboard/static/css/11-games.css")).expect("games css");
    assert!(games_css.contains(
        "@media (min-width: 769px) {\n    .bottom-sheet.open.games-new-dialog {\n        width: min(520px, 92vw);\n    }\n}"
    ));
    assert!(
        !games_css
            .contains("\n.bottom-sheet.open.games-new-dialog {\n    width: min(520px, 92vw);\n}"),
        "games new sheet width must not override the shared mobile bottom-sheet left/right layout"
    );

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("css");
    assert!(responsive_css.contains(
        ".bottom-sheet {\n        position: fixed;\n        bottom: 0;\n        left: 0;\n        right: 0;"
    ));
}

#[test]
fn games_view_uses_standard_dark_mode_surfaces() {
    let games_css =
        read_source(repo_root().join("dashboard/static/css/11-games.css")).expect("games css");

    assert!(games_css.contains(
        "[data-theme=\"dark\"] .games-layout {\n    background: var(--surface-workspace);\n}"
    ));
    assert!(games_css.contains(
        "[data-theme=\"dark\"] .games-sidebar,\n[data-theme=\"dark\"] .games-detail {\n    background: var(--surface-panel);\n}"
    ));
    assert!(games_css.contains(
        "[data-theme=\"dark\"] .games-detail-header {\n    background: var(--surface-panel);\n}"
    ));
}

#[test]
fn process_diagnostics_are_explicit_opt_in() {
    let source = read_source(repo_root().join("src-tauri/src/lib.rs")).expect("app shell");

    assert!(source.contains("fn diagnostics_enabled()"));
    assert!(source.contains("env_flag(\"RATSPEAK_DIAGNOSTICS\")"));
    assert!(source.contains("if !diagnostics_enabled()"));
    assert!(source.contains("fn diagnostic_file_enabled()"));
    assert!(source.contains("RATSPEAK_DIAGNOSTIC_FILE"));
    assert!(!source.contains("const DEFAULT_FILTER"));
}

#[test]
fn app_sources_do_not_write_direct_stdout_or_stderr_logs() {
    let root = repo_root();
    let mut files = Vec::new();
    for rel in [
        "src-tauri/src",
        "crates/ratspeak-core/src",
        "crates/ratspeak-db/src",
        "crates/ratspeak-runtime/src",
        "crates/ratspeak-tauri/src",
    ] {
        collect_files(&root.join(rel), &mut files);
    }

    for path in files {
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let source = read_source(&path).expect("source file");
        let rel = path.strip_prefix(&root).unwrap_or(&path).display();
        assert!(
            !source.contains("println!("),
            "{rel} must not print to stdout"
        );
        assert!(
            !source.contains("eprintln!("),
            "{rel} must not print to stderr"
        );
    }
}

#[test]
fn frontend_console_output_is_silent_by_default() {
    let root = repo_root();
    let mut files = Vec::new();
    collect_files(&root.join("dashboard/static/js"), &mut files);

    for path in files {
        if path.extension().and_then(|e| e.to_str()) != Some("js") {
            continue;
        }
        let source = read_source(&path).expect("frontend source");
        let rel = path.strip_prefix(&root).unwrap_or(&path).display();
        assert!(
            !source.contains("console."),
            "{rel} must route diagnostics through RS.diag"
        );
    }
}

#[test]
fn frontend_ipc_waits_and_connect_errors_are_visible() {
    let root = repo_root();
    let state_js = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    assert!(state_js.contains("function _rsWaitForInvoke()"));
    assert!(state_js.contains("err.code = 'ipc_unavailable'"));

    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    assert!(modals_js.contains("function _handleConnectInvokeError"));
    assert!(modals_js.contains("function _handleInterfaceButtonError"));
    let start = modals_js.find("function submitConnection()").unwrap();
    let end = modals_js.find("function openHostModal").unwrap();
    let submit_connection = &modals_js[start..end];
    assert!(
        !submit_connection.contains("catch(function() {})"),
        "TCP connect submit must not swallow IPC/backend failures"
    );
    for disallowed in [
        "RS.invoke(loraCommand, { args: loraArgs }).catch(function() {})",
        "RS.invoke('enable_ble_peer_interface', { args: { duration: parseInt(duration, 10) } }).catch(function() {})",
        "RS.invoke('disconnect_ble_peer', { address: address }).catch(function() {})",
        "RS.invoke(event, invokeArgs).catch(function() {})",
    ] {
        assert!(
            !modals_js.contains(disallowed),
            "interface actions must not swallow IPC/backend failures"
        );
    }

    for checked_invoke in [
        "RS.invoke(editContext ? 'update_tcp_server' : 'add_tcp_server'",
        "RS.invoke(editContext ? 'update_backbone_server' : 'add_backbone_server'",
    ] {
        let idx = modals_js.find(checked_invoke).unwrap();
        let tail = &modals_js[idx..idx + 180.min(modals_js.len() - idx)];
        assert!(
            !tail.contains("catch(function() {})"),
            "interface server submit must surface IPC/backend failures"
        );
    }

    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    for disallowed in [
        "RS.invoke('disconnect_ble_rnode', { name: iface.name }).catch(function() {})",
        "RS.invoke('set_transport_mode', { args: { mode: mode, network_type: networkType } }).catch(function() {})",
        "RS.invoke('set_auto_announce', { interval: interval }).catch(function() {})",
        "RS.invoke('trigger_announce').catch(function() {})",
    ] {
        assert!(
            !settings_js.contains(disallowed),
            "settings interface actions must not swallow IPC/backend failures"
        );
    }
    assert!(settings_js.contains("data.error === 'not_sent'"));
    assert!(settings_js.contains("delete networkBtn.dataset.announcePending"));
    assert!(
        settings_js.contains("var ANNOUNCE_COOLDOWN = 5000;"),
        "manual announce cooldown should only prevent rapid repeat taps"
    );

    let health_js = read_source(root.join("dashboard/static/js/health.js")).expect("health js");
    assert!(health_js.contains("networkAnnounceBtn.dataset.announcePending = '1'"));
    assert!(health_js.contains("networkAnnounceBtn.dataset.announcePending !== '1'"));
    assert!(health_js.contains("function interfaceStatsWithoutAutoPeerDoubleCount"));
    assert!(health_js.contains("AutoInterfacePeer["));

    let connections_js =
        read_source(root.join("dashboard/static/js/connections.js")).expect("connections js");
    assert!(connections_js.contains("interfaceStatsTotals(ifaces)"));

    let network_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/network.rs"))
        .expect("network command source");
    assert!(network_rs.contains("send_manual_announce_from_state"));
    assert!(network_rs.contains("\"not_sent\""));
}

#[test]
fn interface_add_flows_cannot_be_misclassified_as_edits() {
    let root = repo_root();
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    assert!(
        settings_js
            .contains("connAddTcp.addEventListener('click', function() { openConnectModal(); });")
    );
    assert!(!settings_js.contains("connAddTcp.addEventListener('click', openConnectModal);"));

    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    assert!(modals_js.contains("function _normaliseConnectEditContext(editContext)"));
    assert!(modals_js.contains("function _normaliseHostEditContext(editContext, ifaceType)"));
    assert!(modals_js.contains("_connectEditContext = _normaliseConnectEditContext(editContext);"));
    assert!(
        modals_js.contains("var editContext = _normaliseConnectEditContext(_connectEditContext);")
    );
    assert!(
        modals_js
            .contains("_hostEditContext = _normaliseHostEditContext(editContext, 'tcp_server');")
    );
    assert!(modals_js.contains(
        "_backboneHostEditContext = _normaliseHostEditContext(editContext, 'backbone_server');"
    ));

    let quick_start = modals_js
        .find("function quickConnect(")
        .expect("quickConnect");
    let quick_tail = &modals_js[quick_start..];
    let quick_end = quick_tail
        .find("\n}\n\nvar _connectTimeout")
        .expect("quickConnect end");
    let quick_connect = &quick_tail[..quick_end];
    assert!(quick_connect.contains("_connectEditContext = null;"));
    assert!(quick_connect.contains("submitConnection();"));

    assert!(!modals_js.contains("_connectEditContext.oldName"));
    assert!(!modals_js.contains("_hostEditContext.oldName"));
    assert!(!modals_js.contains("_backboneHostEditContext.oldName"));
}

#[test]
fn rnode_radio_catalog_has_single_runtime_source() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    let core_radio =
        read_source(root.join("crates/ratspeak-core/src/radio.rs")).expect("radio source");

    assert!(core_radio.contains("pub const RNODE_PRESETS"));
    assert!(core_radio.contains("pub const RNODE_REGIONS"));
    assert!(core_radio.contains("uhf_433"));
    assert!(modals_js.contains("RS.invoke('api_rnode_presets')"));
    assert!(modals_js.contains("function _rnodeParseFrequencyHz"));
    assert!(modals_js.contains("function _rnodeFormatScaledValue"));
    assert!(modals_js.contains("return _rnodeFormatScaledValue(freq, 1000000, 6, 3);"));
    assert!(modals_js.contains("return _rnodeFormatScaledValue(bw, 1000, 3, 0);"));
    assert!(
        modals_js.contains("loraArgs.frequency = radioSettings.frequency")
            || modals_js.contains("frequency: radioSettings.frequency")
    );
    assert!(modals_js.contains("loraArgs.custom_params = true"));
    assert!(index.contains(r#"id="rnode-frequency""#));
    assert!(index.contains(r#"id="rnode-advanced""#));
    assert!(!modals_js.contains("var RNODE_PRESETS = {"));
    assert!(!modals_js.contains("var RNODE_REGIONS = {"));
    assert!(!index.contains("<option value=\"americas\""));
    assert!(!index.contains("<option value=\"medium_fast\""));
}

#[test]
fn conversation_row_swipe_uses_delete_choice_without_tab_navigation() {
    let root = repo_root();
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf.contains("delegated: '.conv-row'"));
    assert!(lxmf.contains("showConversationDeleteDialog(hash, name)"));
    assert!(!lxmf.contains("_swipeHideConversation("));
    assert!(!lxmf.contains("Conversation hidden"));

    let nav = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    assert!(nav.contains("e.target.closest('.conv-row, .conv-swipe-delete')"));

    let messaging_css =
        read_source(root.join("dashboard/static/css/09-messaging.css")).expect("css");
    assert!(messaging_css.contains("touch-action: pan-y;"));
}

#[test]
fn empty_ghost_conversations_are_removed_when_leaving_chat_detail() {
    let root = repo_root();
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    let view_stack =
        read_source(root.join("dashboard/static/js/view_stack.js")).expect("view stack js");

    assert!(lxmf.contains("function _ensureGhostRow(hash)"));
    assert!(lxmf.contains("row.dataset.ghost = 'true';"));
    assert!(lxmf.contains("function _onChatDetailExit()"));
    assert!(lxmf.contains("function _conversationHasVisibleMessages()"));
    assert!(lxmf.contains("function _mergeOptimisticConversation(convos)"));
    assert!(lxmf.contains(
        "if (!_ghostConversationHash || _ghostConversationHash !== exitingHash) return;"
    ));
    assert!(lxmf.contains("if (_conversationHasVisibleMessages())"));
    assert!(lxmf.contains("_removeGhostRow();"));
    assert!(lxmf.contains("cacheDel(exitingHash);"));
    assert!(lxmf.contains("lxmfActiveContact = null;"));
    assert!(lxmf.contains("lxmfConversation = [];"));
    assert!(lxmf.contains("convos = _mergeOptimisticConversation(convos);"));
    assert!(lxmf.contains("_renderConversationsFromCache(lxmfConversations || []);"));
    assert!(view_stack.contains("popped.viewId === 'chat-detail'"));
    assert!(view_stack.contains("typeof _onChatDetailExit === 'function'"));
    assert!(view_stack.contains("_onChatDetailExit(popped);"));
}

#[test]
fn message_composer_send_preserves_preexisting_focus_state() {
    let root = repo_root();
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    let start = lxmf
        .find("function sendLxmfMessage(")
        .expect("send function");
    let tail = &lxmf[start..];
    let end = tail
        .find("\nfunction triggerFileAttachment")
        .expect("send function end");
    let send_function = &tail[..end];

    assert!(lxmf.contains("function _captureLxmfSendFocusState()"));
    assert!(lxmf.contains("function _consumeLxmfSendFocusState(input)"));
    assert!(lxmf.contains("function _finishLxmfComposerSend(input, shouldRestoreFocus)"));
    // Send button uses split touchstart/mousedown handlers with non-passive
    // preventDefault to keep the soft keyboard up while the long-press timer
    // runs. Both wire `_captureLxmfSendFocusState` so the existing focus-
    // restore pathway in sendLxmfMessage stays valid.
    assert!(lxmf.contains("sendBtn.addEventListener('touchstart'"));
    assert!(lxmf.contains("sendBtn.addEventListener('mousedown'"));
    assert!(lxmf.contains("_captureLxmfSendFocusState();"));
    assert!(
        send_function
            .contains("var shouldRestoreComposerFocus = _consumeLxmfSendFocusState(input);")
    );
    assert!(send_function.contains("_finishLxmfComposerSend(input, shouldRestoreComposerFocus);"));
    assert!(
        !send_function.contains("input.focus();"),
        "send must not unconditionally focus the composer after a button send"
    );

    let messaging_css =
        read_source(root.join("dashboard/static/css/09-messaging.css")).expect("css");
    assert!(messaging_css.contains("overflow-y: auto;"));
    assert!(messaging_css.contains("scrollbar-width: none;"));
    assert!(messaging_css.contains("-webkit-appearance: none;"));
    assert!(messaging_css.contains(".lxmf-compose textarea::-webkit-scrollbar"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("css");
    assert!(responsive_css.contains("overflow-y: auto;"));
    assert!(responsive_css.contains("scrollbar-width: none;"));
    assert!(responsive_css.contains("-webkit-appearance: none;"));
}

#[test]
fn conversation_view_scrolls_to_recent_messages_without_yanking_history() {
    let lxmf = read_source(repo_root().join("dashboard/static/js/lxmf.js")).expect("lxmf js");

    assert!(lxmf.contains("function _wireLxmfMessageScroll(container)"));
    assert!(lxmf.contains("function _captureLxmfMessageScrollState(container)"));
    assert!(lxmf.contains("function _scheduleLxmfScrollToBottom(container)"));
    assert!(
        lxmf.contains("function _applyLxmfMessageScrollAfterRender(container, state, options)")
    );
    assert!(lxmf.contains("function _watchLxmfImagesForBottomPin(container, shouldPin)"));
    assert!(lxmf.contains("container.querySelectorAll('img').forEach(function(img)"));
    assert!(lxmf.contains("img.addEventListener('load', function()"));
    assert!(lxmf.contains("renderConversation({ forceScrollBottom: true });"));
    assert!(lxmf.contains("renderConversation({ stickToBottom: true });"));
    assert!(
        !lxmf.contains("setTimeout(function() { msgEl.scrollTop = msgEl.scrollHeight; }, 50)"),
        "conversation scrolling must use the central settled-bottom policy"
    );
}

#[test]
fn message_camera_and_photo_attachment_flow_is_native_and_previewed() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    assert!(index.contains(r#"id="lxmf-camera-input" accept="image/*" capture="environment""#));
    assert!(index.contains(r#"id="lxmf-video-input" accept="video/*" capture="environment""#));
    assert!(
        !index.contains(r#"id="lxmf-camera-input" accept="image/*,video/*""#),
        "Camera action must request still-image capture instead of the generic media picker"
    );

    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf.contains("function triggerCameraAttachment()"));
    assert!(lxmf.contains("function triggerVideoAttachment()"));
    assert!(lxmf.contains("var input = document.getElementById('lxmf-camera-input');"));
    assert!(lxmf.contains("var input = document.getElementById('lxmf-video-input');"));
    assert!(
        lxmf.contains("{ label: 'Camera', icon: ICON_CAMERA, onSelect: triggerCameraAttachment }")
    );
    assert!(
        lxmf.contains("{ label: 'Video', icon: ICON_VIDEO, onSelect: triggerVideoAttachment }")
    );
    assert!(lxmf.contains("function _pendingAttachmentName(file)"));
    assert!(lxmf.contains("pending-file-thumbnail"));
    assert!(lxmf.contains(
        "src=\"data:' + escapeHtml(lxmfPendingFile.mime) + ';base64,' + lxmfPendingFile.data"
    ));
    assert!(lxmf.contains("container.classList.toggle('pending-file-has-image', isImage);"));

    let messaging_css =
        read_source(root.join("dashboard/static/css/09-messaging.css")).expect("css");
    assert!(messaging_css.contains("#lxmf-pending-file.file-transfer-info"));
    assert!(messaging_css.contains(".pending-file-thumbnail img"));
    assert!(messaging_css.contains("object-fit: cover;"));
    assert!(messaging_css.contains(".pending-file-copy"));
}

#[test]
fn message_media_viewer_links_and_native_saves_are_wired() {
    let root = repo_root();
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf.contains("function linkifyMessageText(text)"));
    assert!(lxmf.contains("class=\"rs-message-link\""));
    assert!(lxmf.contains("function openImageViewer(img)"));
    assert!(lxmf.contains("lightbox-zoomable"));
    assert!(lxmf.contains("RS.saveDownloadedFile(file, { preferPhotos: true })"));
    assert!(lxmf.contains("Saved to photos!"));
    assert!(lxmf.contains("function _compensateImageLoadScroll(container, img, before)"));

    let state_js = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    assert!(state_js.contains("saveImageToPhotos"));
    assert!(state_js.contains("saveFileDocument"));
    assert!(state_js.contains("data_base64: result.data_base64 || ''"));
    assert!(state_js.contains("window.RS.openExternalUrl"));
    assert!(state_js.contains("open_external_url"));

    let nav_js = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    assert!(nav_js.contains("RS.closeImageViewer"));

    let messaging_css =
        read_source(root.join("dashboard/static/css/09-messaging.css")).expect("css");
    assert!(messaging_css.contains(".lxmf-msg.msg-has-image"));
    assert!(messaging_css.contains(".image-viewer"));
    assert!(messaging_css.contains(".rs-message-link"));

    let android_activity = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("android main activity");
    assert!(android_activity.contains("fun saveImageToPhotos("));
    assert!(android_activity.contains("MediaStore.Images.Media.RELATIVE_PATH"));
    assert!(android_activity.contains("Pictures/Ratspeak"));
    assert!(android_activity.contains("fun saveFileDocument("));
    assert!(android_activity.contains("fun openExternalUrl(url: String): Boolean"));

    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    assert!(tauri_lib.contains("fn open_external_url(url: String)"));
    assert!(tauri_lib.contains("fn save_image_to_photos("));
    assert!(tauri_lib.contains("performChangesAndWait"));
    assert!(tauri_lib.contains("PHAssetChangeRequest"));
}

#[test]
fn voice_and_capture_paths_preflight_media_permissions() {
    let root = repo_root();
    let manifest = read_source(root.join("src-tauri/gen/android/app/src/main/AndroidManifest.xml"))
        .expect("android manifest");
    assert!(manifest.contains("android.permission.CAMERA"));
    assert!(manifest.contains("android.permission.RECORD_AUDIO"));
    assert!(manifest.contains("android.permission.WAKE_LOCK"));
    assert!(manifest.contains("android.hardware.camera.any"));
    assert!(manifest.contains("android.hardware.microphone"));

    let activity = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("main activity");
    assert!(activity.contains("MEDIA_PERMISSION_REQUEST_CODE"));
    assert!(activity.contains("fun hasMediaPermissions(audio: Boolean, camera: Boolean): Boolean"));
    assert!(activity.contains(
        "fun requestMediaPermissions(audio: Boolean, camera: Boolean, requestId: String)"
    ));
    assert!(activity.contains("window._onAndroidMediaPermissionResult"));
    assert!(activity.contains("mediaPlaybackRequiresUserGesture = false"));
    assert!(activity.contains("fun playCallRingtone(mode: String)"));
    assert!(activity.contains("fun stopCallRingtone()"));
    assert!(activity.contains("fun startCallAudioRoute(role: String)"));
    assert!(activity.contains("fun stopCallAudioRoute()"));
    assert!(activity.contains("requestCallAudioFocus()"));
    assert!(activity.contains("fun playCallRingtone(mode: String): Boolean"));
    assert!(activity.contains("runOnMainForBoolean"));
    assert!(activity.contains("AUDIOFOCUS_REQUEST_GRANTED"));
    assert!(activity.contains("AudioManager.STREAM_RING"));
    assert!(activity.contains("AudioAttributes.USAGE_VOICE_COMMUNICATION"));
    assert!(activity.contains("volumeControlStream = AudioManager.STREAM_VOICE_CALL"));
    assert!(activity.contains("syncCallProximityWakeLock(preferEarpiece)"));
    assert!(activity.contains("PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK"));
    assert!(activity.contains("isWakeLockLevelSupported"));
    assert!(activity.contains("PowerManager.RELEASE_FLAG_WAIT_FOR_NO_PROXIMITY"));
    assert!(activity.contains("callAudioRouteName = routeName"));
    assert!(activity.contains("AudioAttributes.USAGE_VOICE_COMMUNICATION_SIGNALLING"));
    assert!(activity.contains("AudioAttributes.USAGE_NOTIFICATION_RINGTONE"));
    assert!(activity.contains("audioManager.setCommunicationDevice(route)"));

    let voice_audio = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakVoiceAudio.kt",
    ))
    .expect("android voice audio");
    assert!(voice_audio.contains("object RatspeakVoiceAudio"));
    assert!(voice_audio.contains("AudioAttributes.USAGE_VOICE_COMMUNICATION"));
    assert!(voice_audio.contains("AudioAttributes.CONTENT_TYPE_SPEECH"));
    assert!(voice_audio.contains("AudioFormat.ENCODING_PCM_FLOAT"));
    assert!(voice_audio.contains("AudioFormat.ENCODING_PCM_16BIT"));
    assert!(voice_audio.contains("AudioTrack.MODE_STREAM"));
    assert!(voice_audio.contains("AudioTrack.WRITE_NON_BLOCKING"));
    assert!(voice_audio.contains("fun lastError(): String"));

    let state_js = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    assert!(state_js.contains("window.RS.mediaPermissions"));
    assert!(state_js.contains("window.RS.audioPlayback"));
    assert!(state_js.contains("window.RatspeakAndroid.requestMediaPermissions"));
    assert!(state_js.contains("navigator.mediaDevices.getUserMedia"));

    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf.contains("function _voiceEnsureMicrophonePermission()"));
    assert!(lxmf.contains("isTauriDesktop())"));
    assert!(lxmf.contains("function _voiceEnsurePlaybackReady()"));
    assert!(lxmf.contains("function _voiceAfterNextPaint()"));
    assert!(lxmf.contains("function _voiceSetOptimisticOutgoing(hash)"));
    assert!(lxmf.contains("function _voiceBlockMobileNavigation(ms)"));
    assert!(lxmf.contains("var dialToken = ++_voiceDialToken;"));
    assert!(lxmf.contains(
        "return _voiceAfterNextPaint().then(_voiceEnsurePlaybackReady).then(_voiceEnsureMicrophonePermission)"
    ));
    assert!(lxmf.contains("RS.ringtones.sync(lxstVoiceState)"));
    assert!(lxmf.contains("RS.ringtones.setHandlers({ onOutgoingTimeout"));
    assert!(lxmf.contains("function _voiceSyncNativeAudioRoute(force)"));
    assert!(lxmf.contains("window.RatspeakAndroid.startCallAudioRoute"));
    assert!(lxmf.contains("lxstVoiceState.speakerphone ? 'speaker' : 'earpiece'"));
    assert!(lxmf.contains("function _voiceToggleMute()"));
    assert!(lxmf.contains("function _voiceToggleSpeaker()"));
    assert!(lxmf.contains("function _voicePrimeNativeCallRoute()"));
    assert!(lxmf.contains("_voiceNativeAudioRouteLastSyncAt"));
    assert!(lxmf.contains("voice_set_microphone_muted"));
    assert!(lxmf.contains("voice_restart_speaker"));
    assert!(lxmf.contains("function _voicePeerLookupHash(call)"));
    assert!(
        lxmf.contains("if (call.role === 'outgoing' && lxstVoiceState.lastDialHash) return lxstVoiceState.lastDialHash;")
    );
    assert!(lxmf.contains("function _voicePeerSurfaceTitle(call)"));
    assert!(lxmf.contains("return _voicePeerName(call);"));
    assert!(lxmf.contains("remote_lxmf_destination"));
    assert!(lxmf.contains("lxst-incoming-call-address"));
    assert!(lxmf.contains("data.type === 'outgoing_pending'"));
    assert!(lxmf.contains("data.type === 'outgoing_failed'"));
    assert!(lxmf.contains("case 'available': return 'Calling';"));
    assert!(lxmf.contains(
        "var canShow = lxstVoiceState.available && !!lxmfActiveContact && !activeMatches && !incomingMatches;"
    ));
    assert!(lxmf.contains("_ensureAttachmentMediaPermission({ camera: true })"));
    assert!(lxmf.contains("_ensureAttachmentMediaPermission({ camera: true, audio: true })"));

    let voice_rs =
        read_source(root.join("crates/ratspeak-runtime/src/voice.rs")).expect("voice rs");
    assert!(voice_rs.contains("remote_lxmf_destination"));
    assert!(voice_rs.contains("fn lxmf_destination_for_identity(identity_hash: [u8; 16])"));
    assert!(voice_rs.contains("const VOICE_CONTACTS_ONLY_NOTICE"));
    assert!(voice_rs.contains("const VOICE_REJECTED_CALL_BLACKHOLE_THRESHOLD: u32 = 10"));
    assert!(voice_rs.contains("fn spawn_contacts_only_notice("));
    assert!(voice_rs.contains("fn cached_zero_hop_path("));
    assert!(voice_rs.contains("suppressed_call_links.insert(link_id);"));
    assert!(voice_rs.contains("TransportQuery::IsBlackholed"));
    assert!(voice_rs.contains("BlackholeReason::RateLimit"));
    assert!(voice_rs.contains("send_ephemeral_opportunistic_message"));
    assert!(voice_rs.contains("pub async fn announce_if_running(state: &AppState)"));
    assert!(voice_rs.contains("static VOICE_MICROPHONE_MUTED: AtomicBool"));
    assert!(voice_rs.contains("pub fn set_microphone_muted("));
    assert!(voice_rs.contains("enum VoiceAudioControl"));
    assert!(voice_rs.contains("RestartSpeaker { speakerphone: bool }"));
    assert!(voice_rs.contains("async fn restart_speaker("));
    assert!(voice_rs.contains("TelephonyControl::StopOpusStream"));
    assert!(voice_rs.contains("start_microphone_side("));
    assert!(voice_rs.contains("start_android_speaker_side("));
    assert!(voice_rs.contains("RatspeakVoiceAudio.write"));
    assert!(voice_rs.contains("retry_missing_audio("));
    assert!(voice_rs.contains("const VOICE_OUTPUT_GAIN"));
    assert!(voice_rs.contains("const VOICE_NOISE_GATE_OPEN_RMS"));
    assert!(voice_rs.contains("fn update_noise_gate("));
    assert!(voice_rs.contains("fn frame_rms("));
    assert!(voice_rs.contains("fn apply_voice_output_leveling("));
    assert!(voice_rs.contains("builder.clear_pending_audio();"));
    assert!(voice_rs.contains("\"microphone_muted\": microphone_muted()"));
    assert!(voice_rs.contains("TelephonyControl::Announce"));
    assert!(voice_rs.contains("TelephonyServiceEvent::OutgoingCallPending"));
    assert!(voice_rs.contains("TelephonyServiceEvent::OutgoingCallFailed"));
    assert!(voice_rs.contains("state.emit_network_event(\"lxst\""));

    let runtime_rs =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime lib");
    assert!(runtime_rs.contains("voice::announce_if_running(state).await"));
    assert!(runtime_rs.contains("LXST telephony announced on all interfaces"));

    let ringtone_js =
        read_source(root.join("dashboard/static/js/voice_ringtones.js")).expect("ringtone js");
    assert!(ringtone_js.contains("var RATSPEAK_RINGTONE_LOOP_MS = 3200"));
    assert!(ringtone_js.contains("var RATSPEAK_RINGTONE_E5_HZ = 659.255114"));
    assert!(ringtone_js.contains("var RATSPEAK_RINGTONE_G5_HZ = 783.990872"));
    assert!(ringtone_js.contains("var RATSPEAK_RINGTONE_B5_HZ = 987.766603"));
    assert!(ringtone_js.contains("var RATSPEAK_RINGTONE_INCOMING_NOTES = ["));
    assert!(ringtone_js.contains(
        "{ startMs: 300, freqHz: RATSPEAK_RINGTONE_B5_HZ, durationMs: 168, gain: 1.00 }"
    ));
    assert!(ringtone_js.contains("var RATSPEAK_RINGTONE_OUTGOING_NOTES = ["));
    assert!(ringtone_js.contains(
        "{ startMs: 1560, freqHz: RATSPEAK_RINGTONE_G5_HZ, durationMs: 96, gain: 0.68 }"
    ));
    assert!(ringtone_js.contains("var RATSPEAK_RINGTONE_INCOMING_GAIN = 0.36"));
    assert!(ringtone_js.contains("var RATSPEAK_RINGTONE_OUTGOING_GAIN = 0.18"));
    assert!(ringtone_js.contains("var RATSPEAK_RINGTONE_INCOMING_GLIDE_CENTS = 7.0"));
    assert!(ringtone_js.contains("var RATSPEAK_RINGTONE_OUTGOING_GLIDE_CENTS = -4.0"));
    assert!(ringtone_js.contains("ctx.createBuffer(1, sampleCount, sampleRate)"));
    assert!(ringtone_js.contains("source.loop = true"));
    assert!(ringtone_js.contains("var OUTGOING_TIMEOUT_MS = 25000"));
    assert!(ringtone_js.contains("playCallRingtone"));
    assert!(ringtone_js.contains("stopCallRingtone"));
    assert!(ringtone_js.contains("if (started === false)"));
    assert!(ringtone_js.contains("playTimeoutCue();"));
    assert!(ringtone_js.contains("active.status !== 'established'"));
    assert!(activity.contains("private const val CALL_RINGTONE_LOOP_MS = 3200L"));
    assert!(activity.contains("private const val CALL_RINGTONE_E5_HZ = 659.255114"));
    assert!(activity.contains("private const val CALL_RINGTONE_G5_HZ = 783.990872"));
    assert!(activity.contains("private const val CALL_RINGTONE_B5_HZ = 987.766603"));
    assert!(activity.contains(
        "CALL_RINGTONE_INCOMING_START_MS = longArrayOf(0L, 150L, 300L, 780L, 920L, 1070L)"
    ));
    assert!(
        activity.contains("CALL_RINGTONE_OUTGOING_START_MS = longArrayOf(0L, 180L, 1560L, 1710L)")
    );
    assert!(activity.contains("private const val CALL_RINGTONE_INCOMING_VOLUME = 0.36"));
    assert!(activity.contains("private const val CALL_RINGTONE_OUTGOING_VOLUME = 0.18"));
    assert!(
        activity.contains(
            "private val CALL_RINGTONE_INCOMING_PARTIALS = doubleArrayOf(0.74, 0.18, 0.08)"
        )
    );
    assert!(
        activity.contains(
            "private val CALL_RINGTONE_OUTGOING_PARTIALS = doubleArrayOf(0.80, 0.15, 0.05)"
        )
    );
    assert!(activity.contains("private fun raisedCosine(progress: Double): Double"));
    assert!(activity.contains("track.setLoopPoints(0, frameCount, -1)"));

    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    assert!(index.contains("id=\"lxst-call-global-mute-btn\""));
    assert!(index.contains("id=\"lxst-call-global-speaker-btn\""));
    assert!(index.contains("id=\"lxst-call-mute-btn\""));
    assert!(index.contains("id=\"lxst-call-speaker-btn\""));
    let ringtone_pos = index
        .find("/static/js/voice_ringtones.js")
        .expect("ringtone script");
    let lxmf_pos = index.find("/static/js/lxmf.js").expect("lxmf script");
    assert!(ringtone_pos < lxmf_pos);

    let activity_js =
        read_source(root.join("dashboard/static/js/activity.js")).expect("activity js");
    assert!(activity_js.contains("lxst: true"));
    assert!(activity_js.contains("lxst: 'LXST'"));
}

#[test]
fn active_call_surface_is_passive_and_shows_elapsed_duration() {
    let root = repo_root();
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf.contains("function _voiceElapsedLabel()"));
    assert!(lxmf.contains("function _voiceGlobalStatusLabel(active)"));
    assert!(lxmf.contains("return 'Active call' + (elapsed ? ' - ' + elapsed : '');"));
    assert!(lxmf.contains("if (audioIssue) return audioIssue;"));
    assert!(lxmf.contains("Math.max(1"));
    assert!(lxmf.contains("minutes + ':' + (seconds < 10 ? '0' : '') + seconds"));
    assert!(lxmf.contains("function _voiceCallSurfaceAvatarHtml(call, size)"));
    assert!(lxmf.contains("identityAvatar(info.avatarHash || info.address || '', size)"));
    assert!(lxmf.contains("name === 'speaker-on'"));
    assert!(lxmf.contains("lxstVoiceState.speakerphone ? 'speaker-on' : 'speaker'"));
    assert!(lxmf.contains("function _voiceWireHangupProximity(surfaceId, hangupId)"));
    assert!(
        lxmf.contains(
            "_voiceWireHangupProximity('lxst-call-global', 'lxst-call-global-hangup-btn')"
        )
    );
    assert!(!lxmf.contains("function _voiceWireCallSurfaceNavigation(id)"));
    assert!(!lxmf.contains("_voiceOpenActiveConversation();"));

    let messaging_css =
        read_source(root.join("dashboard/static/css/09-messaging.css")).expect("css");
    assert!(messaging_css.contains("cursor: default;"));
    assert!(messaging_css.contains("min-height: 78px;"));
    assert!(messaging_css.contains(".lxst-call-action::before"));
    assert!(messaging_css.contains(".lxst-call-strip-controls"));
    assert!(messaging_css.contains("flex-direction: column;"));
    assert!(messaging_css.contains(".lxst-call-toggle.is-muted::after"));
    assert!(messaging_css.contains(".lxst-call-toggle.is-on"));
    assert!(!messaging_css.contains("box-shadow: inset 0 0 0 1px var(--border-light);"));
    assert!(messaging_css.contains(".lxst-call-strip-title"));
    assert!(messaging_css.contains("overflow-wrap: anywhere;"));
    assert!(messaging_css.contains(".lxst-incoming-call-address"));
    assert!(messaging_css.contains("word-break: break-all;"));
}

#[test]
fn settings_version_display_uses_package_version_api() {
    let root = repo_root();
    let system_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/system.rs")).expect("system rs");
    assert!(system_rs.contains("env!(\"CARGO_PKG_VERSION\")"));
    assert!(system_rs.contains("RATSPEAK_DISPLAY_VERSION"));
    assert!(system_rs.contains("GITHUB_REF_NAME"));
    assert!(system_rs.contains("strip_prefix('v')"));
    assert!(!system_rs.contains("\"version\": \"1.0.13\""));

    let index = read_source(root.join("dashboard/index.html")).expect("index");
    assert!(index.contains("id=\"settings-version-sidebar\""));
    assert!(index.contains("id=\"settings-version-system\""));

    let settings_js = read_source(root.join("dashboard/static/js/settings.js")).expect("settings");
    assert!(settings_js.contains("function renderSettingsVersion()"));
    assert!(settings_js.contains("RS.invoke('api_version')"));
    assert!(settings_js.contains("name + ' v.' + version"));

    let nav_js = read_source(root.join("dashboard/static/js/nav.js")).expect("nav");
    assert!(nav_js.contains("id=\"about-modal-version\""));
    assert!(nav_js.contains("RS.invoke('api_version')"));
    assert!(!nav_js.contains("v1.0.7"));

    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    assert!(views_css.contains(".settings-sidebar-version"));
    assert!(views_css.contains(".settings-version-system"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    assert!(responsive_css.contains(".settings-version-system"));
    assert!(responsive_css.contains("text-align: center;"));
}

#[test]
fn mobile_settings_use_section_drilldown_instead_of_stacked_panels() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let nav_js = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");

    assert!(index.contains("class=\"settings-nav-desc\""));
    assert!(index.contains("id=\"settings-mobile-back-btn\""));
    assert!(index.contains("id=\"settings-mobile-detail-title\""));
    assert!(settings_js.contains("function _settingsMobileModeActive()"));
    assert!(settings_js.contains("showMobileDetail: _settingsMobileModeActive()"));
    assert!(settings_js.contains("function showSettingsMobileSectionIndex(opts)"));
    assert!(settings_js.contains("function isSettingsMobileDetailActive()"));
    assert!(settings_js.contains("settings-mobile-detail-active"));
    assert!(nav_js.contains("function _settingsDetailSwipeActive()"));
    assert!(nav_js.contains("function initSettingsDetailSwipeBack()"));
    assert!(nav_js.contains("if (_settingsDetailSwipeActive()) return true;"));
    assert!(nav_js.contains("RS.viewStack.depth() > 1) return true;"));
    assert!(nav_js.contains("showSettingsMobileSectionIndex();"));
    assert!(nav_js.contains("initSettingsDetailSwipeBack();"));
    assert!(views_css.contains(".settings-nav-desc,"));
    assert!(
        responsive_css
            .contains(".settings-page:not(.settings-mobile-detail-active) .settings-detail-pane")
    );
    assert!(
        responsive_css.contains(".settings-detail-mode .settings-panel.settings-panel-selected")
    );
    assert!(responsive_css.contains(".settings-row-label {\n        font-size: 16px;"));
}

#[test]
fn mobile_primary_lists_share_readable_row_scale() {
    let root = repo_root();
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");

    assert!(responsive_css.contains("--mobile-list-avatar-size: 44px;"));
    assert!(responsive_css.contains("--mobile-list-min-height: 58px;"));
    assert!(responsive_css.contains("--mobile-list-title-size: 16px;"));
    assert!(responsive_css.contains("--mobile-list-detail-size: 14px;"));
    assert!(responsive_css.contains("--mobile-list-meta-size: 13px;"));
    assert!(responsive_css.contains(
        ".conv-row,\n    .contacts-row,\n    .identity-list-item,\n    .games-session-row"
    ));
    assert!(responsive_css.contains(
        ".conv-avatar-wrap,\n    .conv-avatar,\n    .contacts-avatar,\n    .identity-list-avatar"
    ));
    assert!(responsive_css.contains(
        ".conv-name,\n    .contacts-row-name,\n    .identity-list-name,\n    .games-session-name"
    ));
    assert!(responsive_css.contains(".conn-section-label,\n    .conn-iface-name"));
    assert!(responsive_css.contains(".conn-iface-empty,"));
    assert!(responsive_css.contains(".activity-empty,"));
    assert!(
        responsive_css
            .contains(".games-session-icon {\n        width: var(--mobile-list-icon-size);")
    );
    assert!(
        responsive_css
            .contains(".conn-card-label {\n        font-size: var(--mobile-list-title-size);")
    );
    assert!(responsive_css.contains(".activity-level-btn,\n    .activity-filter-chip"));
    assert!(responsive_css.contains("font-size: var(--mobile-list-meta-size);"));
    assert!(
        responsive_css.contains(
            ".pulse-throughput-value {\n        font-size: var(--mobile-list-detail-size);"
        )
    );
    assert!(responsive_css.contains(".pulse-announce-btn {\n        min-height: 38px;"));
    assert!(responsive_css.contains(".pulse-announce-btn svg {\n        width: 16px;"));
    assert!(responsive_css.contains(".contacts-standalone .contacts-row-hash"));
    assert!(responsive_css.contains(".games-session-game {\n        display: none;"));
    assert!(responsive_css.contains(".peers-list-scroll,\n    #lxmf-conversations-list,"));
    assert!(responsive_css.contains(".dashboard-peers-scroll,"));
    assert!(responsive_css.contains(".peers-list-scroll::-webkit-scrollbar,"));
    assert!(responsive_css.contains(".dashboard-peers-scroll::-webkit-scrollbar,"));
    assert!(responsive_css.contains(".conn-group-header {\n        font-size: 13px;"));
    assert!(responsive_css.contains(".system-action-label,"));
    assert!(responsive_css.contains(".system-subsection-title,"));
    assert!(responsive_css.contains(".relay-card-header,"));
    assert!(responsive_css.contains(".relay-card-details,"));
    assert!(responsive_css.contains(".propagation-section-desc,"));
    assert!(responsive_css.contains("--mobile-list-avatar-size: 42px;"));
}

#[test]
fn mobile_peers_toolbar_uses_search_plus_icon_sort_only() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    let peers_js = read_source(root.join("dashboard/static/js/peers.js")).expect("peers js");
    let lxmf_js = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");

    assert!(!index.contains("id=\"peers-filter-pills\""));
    assert!(!index.contains("data-filter=\"reachable\""));
    assert!(!peers_js.contains("peersFilter"));
    assert!(peers_js.contains("return 'Local';"));
    assert!(index.contains("class=\"peers-sort-icon\""));
    assert!(!index.contains("<span>Peers</span>"));
    assert!(!index.contains("<span>Messages</span>"));
    assert!(!index.contains("<span>Contacts</span>"));
    assert!(!index.contains("<span>More</span>"));
    assert!(responsive_css.contains(".peers-toolbar {\n        padding:"));
    assert!(responsive_css.contains(".peers-toolbar { flex-wrap: nowrap; }"));
    assert!(responsive_css.contains(".peers-sort-label {\n        display: none;"));
    assert!(
        responsive_css
            .contains(".peers-sort-dropdown .toolbar-dropdown-btn {\n        width: 44px;")
    );
    assert!(responsive_css.contains("background: var(--input-bg);"));
    assert!(
        responsive_css
            .contains(".peers-sort-dropdown .toolbar-dropdown-item {\n        min-height: 48px;")
    );
    assert!(
        responsive_css.contains(".bottom-bar-item span:not(.bottom-bar-badge) { display: none; }")
    );
    assert!(responsive_css.contains("height: calc(62px + var(--sab));"));
    assert!(responsive_css.contains("padding-bottom: calc(62px + var(--sab));"));
    assert!(responsive_css.contains(".bottom-bar-item svg {\n        width: 26px;"));
    assert!(responsive_css.contains("right: calc(50% - 18px);"));

    assert!(!index.contains("id=\"header-mobile-hash\""));
    assert!(responsive_css.contains(".header-mobile-avatar {\n        width: 36px;"));
    assert!(responsive_css.contains(".header-mobile-name {\n        font-size: var(--text-xl);"));
    assert!(lxmf_js.contains("identityAvatar(hash, 36)"));
    assert!(settings_js.contains("identityAvatar(hash, 36)"));
}

#[test]
fn contact_detail_sheet_centers_identity_and_separates_primary_actions() {
    let root = repo_root();
    let lxmf_js = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");

    let hash_row = lxmf_js.find("contact-detail-hash-row").expect("hash row");
    let primary_actions = lxmf_js
        .find("contact-detail-primary-actions")
        .expect("primary actions");
    let fields = lxmf_js.find("contact-detail-fields").expect("fields");
    let danger_actions = lxmf_js
        .find("contact-detail-danger-actions")
        .expect("danger actions");
    assert!(hash_row < primary_actions);
    assert!(primary_actions < fields);
    assert!(fields < danger_actions);

    assert!(views_css.contains(".contact-detail-avatar {\n    display: flex;"));
    assert!(views_css.contains("margin: var(--space-4) auto 0;"));
    assert!(views_css.contains(".contact-detail-avatar svg,"));
    assert!(views_css.contains(".contact-detail-primary-actions"));
    assert!(views_css.contains(".contact-detail-danger-actions"));
}

#[test]
fn mobile_peers_rows_are_larger_and_detail_sheet_expands_progressively() {
    let root = repo_root();
    let peers = read_source(root.join("dashboard/static/js/peers.js")).expect("peers js");
    assert!(peers.contains("_peersRowHeight = window.innerWidth <= 768 ? 58 : 36;"));
    assert!(peers.contains("var avatarSize = window.innerWidth <= 768 ? 44 : 28;"));
    assert!(peers.contains("showConnectionDetailSheet(hash, { progressive: true });"));

    let connections =
        read_source(root.join("dashboard/static/js/connections.js")).expect("connections js");
    assert!(connections.contains("function showConnectionDetailSheet(hash, options)"));
    assert!(connections.contains("Swipe up for more info"));
    assert!(connections.contains("function expandConnectionDetailSheet()"));
    assert!(connections.contains("function wireConnectionDetailExpansion(sheet)"));
    assert!(
        connections
            .contains("sheet.classList.toggle('conn-detail-sheet--progressive', progressive);")
    );
    assert!(connections.contains(
        "sheet.classList.toggle('conn-detail-sheet--compact', progressive && !addActionHtml);"
    ));
    assert!(connections.contains(
        "sheet.classList.toggle('conn-detail-sheet--with-add', progressive && !!addActionHtml);"
    ));
    assert!(connections.contains(
        "sheet.classList.remove('conn-detail-sheet--progressive', 'conn-detail-sheet--expanded', 'conn-detail-sheet--compact', 'conn-detail-sheet--with-add');"
    ));
    assert!(connections.contains("dy < -28"));
    let sheet_start = connections
        .find("function showConnectionDetailSheet")
        .expect("connection detail sheet renderer");
    let sheet_tail = &connections[sheet_start..];
    let sheet_end = sheet_tail
        .find("function expandConnectionDetailSheet")
        .expect("connection detail sheet renderer end");
    let sheet_source = &sheet_tail[..sheet_end];
    assert!(sheet_source.contains("identityAvatar(contact.hash, 64)"));
    assert!(sheet_source.contains("conn-detail-sheet-identity"));
    assert!(sheet_source.contains("conn-detail-sheet-header-actions"));
    assert!(sheet_source.contains("id=\"conn-sheet-more-btn\""));
    assert!(sheet_source.contains("actionPopover(this"));
    assert!(sheet_source.contains("label: 'Block'"));
    assert!(sheet_source.contains("function confirmBlockPeer(h)"));
    assert!(!sheet_source.contains("id=\"conn-sheet-block-btn\""));
    assert!(sheet_source.contains("conn-detail-sheet-primary-actions entity-action-grid"));
    assert!(sheet_source.contains("conn-detail-sheet-secondary-actions entity-action-grid"));
    assert!(sheet_source.contains("<span>Message route</span><strong>"));
    assert!(sheet_source.contains("<span>Call route</span><strong>"));
    assert!(!sheet_source.contains("<span>Hops</span><strong>"));
    assert!(!sheet_source.contains("<span>Route</span>"));
    assert!(!sheet_source.contains("<span>Via</span>"));
    assert!(sheet_source.contains("TODO(dev-mode): expose next-hop/via"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("css");
    assert!(responsive_css.contains(".peers-row {\n        min-height: 58px;"));
    assert!(
        responsive_css.contains(".peers-row-avatar {\n        width: 44px;\n        height: 44px;")
    );
    assert!(responsive_css.contains(".conn-detail-sheet.conn-detail-sheet--progressive"));
    assert!(responsive_css.contains(".conn-detail-sheet-identity"));
    assert!(responsive_css.contains(".conn-detail-sheet-avatar"));
    assert!(responsive_css.contains(".conn-detail-sheet-header-actions"));
    assert!(responsive_css.contains(".conn-detail-sheet-icon-btn"));
    assert!(responsive_css.contains(".conn-detail-sheet-primary-actions"));
    assert!(responsive_css.contains(".conn-detail-sheet-secondary-actions"));
    assert!(
        responsive_css.contains(
            ".conn-detail-sheet.conn-detail-sheet--progressive.conn-detail-sheet--compact"
        )
    );
    assert!(
        responsive_css.contains(
            ".conn-detail-sheet.conn-detail-sheet--progressive.conn-detail-sheet--with-add"
        )
    );
    assert!(responsive_css.contains(".conn-detail-sheet--compact .conn-detail-sheet-expand-hint"));
    assert!(responsive_css.contains(".conn-detail-sheet--with-add .conn-detail-sheet-expand-hint"));
    assert!(responsive_css.contains(".conn-detail-sheet {\n    max-width: 100vw;"));
    assert!(
        responsive_css.contains(".conn-detail-sheet--compact .conn-detail-sheet-primary-actions")
    );
    assert!(
        responsive_css
            .contains(".conn-detail-sheet--compact .conn-detail-sheet-actions .entity-action-btn")
    );
    assert!(responsive_css.contains(
        ".conn-detail-sheet-secondary-actions {\n    grid-template-columns: minmax(0, 1fr);"
    ));
    assert!(responsive_css.contains("overflow-x: hidden;"));
    assert!(responsive_css.contains("grid-template-areas: \"avatar title actions\";"));
    assert!(responsive_css.contains("grid-template-columns: 64px minmax(0, 1fr) auto;"));
    assert!(responsive_css.contains("min-height: 60px;"));
    assert!(responsive_css.contains(".conn-detail-sheet-expand-hint {\n    appearance: none;"));
    assert!(!responsive_css.contains("margin-top: auto;"));
    assert!(responsive_css.contains(
        ".conn-detail-sheet--progressive .conn-detail-sheet-fields {\n    display: none;"
    ));
    assert!(responsive_css.contains(
        ".conn-detail-sheet--progressive.conn-detail-sheet--expanded .conn-detail-sheet-fields"
    ));
}

#[test]
fn peers_avatars_are_circle_cropped_like_contacts() {
    let root = repo_root();
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    assert!(views_css.contains(
        ".peers-row-avatar {\n    width: 28px;\n    height: 28px;\n    border-radius: var(--radius-full);"
    ));
    assert!(views_css.contains(
        ".peers-detail-avatar {\n    width: 64px;\n    height: 64px;\n    border-radius: var(--radius-full);"
    ));
    assert!(views_css.contains("clip-path: circle(50% at 50% 50%);"));
    assert!(views_css.contains(
        ".contacts-avatar {\n    flex-shrink: 0;\n    width: 40px;\n    height: 40px;\n    border-radius: var(--radius-full);"
    ));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    assert!(responsive_css.contains(
        ".peers-row-avatar {\n        width: 44px;\n        height: 44px;\n        border-radius: var(--radius-full);"
    ));
    assert!(
        !responsive_css.contains(
            ".peers-row-avatar {\n        width: 44px;\n        height: 44px;\n        border-radius: var(--radius-lg);"
        ),
        "mobile peers avatars must not override contact-style circle cropping"
    );
}

#[test]
fn identity_avatars_are_circle_cropped_everywhere() {
    let root = repo_root();
    let identity_js =
        read_source(root.join("dashboard/static/js/identity.js")).expect("identity js");
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");

    assert!(
        !identity_js.contains("<clipPath id="),
        "cached avatar SVGs must not reuse DOM clip-path ids"
    );
    assert!(identity_js.contains("clip-path:circle(50% at 50% 50%)"));
    assert!(identity_js.contains("<circle cx=\""));
    assert!(views_css.contains(
        ".identity-avatar {\n    flex-shrink: 0;\n    border-radius: var(--radius-full);"
    ));
    assert!(views_css.contains(
        ".identity-list-avatar {\n    flex-shrink: 0;\n    width: 32px;\n    height: 32px;\n    border-radius: var(--radius-full);"
    ));
    assert!(views_css.contains(
        ".identity-detail-avatar {\n    width: 72px;\n    height: 72px;\n    border-radius: var(--radius-full);"
    ));
    assert!(views_css.contains(
        ".settings-identity-avatar {\n    flex-shrink: 0;\n    border-radius: var(--radius-full);"
    ));
    assert!(views_css.contains(
        ".identity-summary-avatar {\n    flex-shrink: 0;\n    border-radius: var(--radius-full);"
    ));
    assert!(responsive_css.contains(".identity-list-avatar,\n    .identity-list-avatar svg,"));
}

#[test]
fn lxmf_conversation_rows_use_peer_display_names_when_available() {
    let lxmf = read_source(repo_root().join("dashboard/static/js/lxmf.js")).expect("lxmf js");

    assert!(lxmf.contains("function _conversationNameInfo(hash, payloadName, isContact)"));
    assert!(lxmf.contains("function _conversationPayloadForHash(hash)"));
    assert!(lxmf.contains("var announceName = _lookupAnnounceName(hash);"));
    assert!(lxmf.contains("return { name: _hashFallbackName(hash), isHash: true };"));
    assert!(lxmf.contains("PeersCache.subscribe(function()"));
    assert!(lxmf.contains("_refreshRenderedConversationNames();"));
    assert!(lxmf.contains("renderVoiceUi();"));
    assert!(lxmf.contains("var payload = _conversationPayloadForHash(hash);"));
    assert!(lxmf.contains("_conversationNameInfo(c.hash, c.display_name, c.is_contact);"));
    assert!(lxmf.contains("_conversationNameInfo(lxmfActiveContact, null, false);"));
    assert!(lxmf.contains("nameEl.classList.toggle('is-hash', !!info.isHash);"));

    let render_start = lxmf
        .find("function _renderConversationsFromCache(convos)")
        .expect("conversation renderer");
    let render_tail = &lxmf[render_start..];
    let render_end = render_tail
        .find("\nfunction renderContactList")
        .expect("conversation renderer end");
    let render_fn = &render_tail[..render_end];
    assert!(
        !render_fn.contains("c.display_name || (c.is_contact ? 'Anonymous'"),
        "conversation list must not bypass peer display-name lookup"
    );
}

#[test]
fn contacts_tab_is_first_class_on_desktop_and_shows_full_addresses() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    assert!(index.contains(r##"class="nav-item" data-view="contacts" href="#contacts""##));
    assert!(index.contains(r#"class="contacts-standalone-header""#));
    assert!(index.contains(r#"id="contacts-count""#));
    assert!(index.contains(r#"id="contacts-add-btn""#));
    assert!(!index.contains(r#"id="dashboard-contacts-search""#));

    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf.contains("function normalizeContactRecord(c)"));
    assert!(lxmf.contains("var hash = c.hash || c.dest_hash || '';"));
    assert!(lxmf.contains("lxmfContacts = normalizeContactList(data);"));
    assert!(!lxmf.contains("dashboard-contacts-search"));

    let start = lxmf
        .find("function renderStandaloneContactList()")
        .expect("standalone contacts renderer");
    let tail = &lxmf[start..];
    let end = tail
        .find("\nfunction renderNetworkContactList")
        .expect("standalone contacts renderer end");
    let renderer = &tail[..end];
    assert!(
        renderer.contains("'<span class=\"contacts-row-hash\">' + escapeHtml(c.hash) + '</span>'")
    );
    assert!(lxmf.contains("function openAddContactPrompt(trigger)"));
    assert!(lxmf.contains("RS.gestures.bindViewFabClick('contacts-add-fab', function()"));
    assert!(
        !renderer.contains("shortHash(c.hash"),
        "standalone Contacts tab must not shorten LXMF addresses"
    );

    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    assert!(views_css.contains(".contacts-standalone .contacts-row-hash"));
    assert!(views_css.contains("overflow-wrap: anywhere;"));
    assert!(views_css.contains("max-width: none;"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    assert!(responsive_css.contains(".contacts-add-btn"));
    assert!(responsive_css.contains("display: none;"));
}

#[test]
fn contact_card_qr_flow_exports_public_key_and_imports_known_identity() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    let identity = read_source(root.join("dashboard/static/js/identity.js")).expect("identity js");
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    let settings = read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let contact_card_js =
        read_source(root.join("dashboard/static/js/contact_card.js")).expect("contact card js");
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    let tauri_build = read_source(root.join("src-tauri/build.rs")).expect("tauri build script");
    let contact_card_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/contact_card.rs"))
            .expect("contact card command");
    let lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    let db = read_source(root.join("crates/ratspeak-db/src/db.rs")).expect("db");

    assert!(index.contains(r#"/static/js/contact_card.js"#));
    assert!(identity.contains("Share Contact Card"));
    assert!(identity.contains("openIdentityShareScreen(identityHash)"));
    assert!(settings.contains("function openActiveIdentityContactCard()"));
    assert!(settings.contains("openIdentityShareScreen(identityHash);"));
    assert!(settings.contains("mobileId.addEventListener('keydown'"));
    assert!(index.contains("id=\"header-mobile-identity\" title=\"Share contact card\""));
    assert!(index.contains("id=\"header-identity-pill\" title=\"Share contact card\""));
    let header_mobile_start = index
        .find("id=\"header-mobile-identity\"")
        .expect("mobile identity header");
    let header_mobile_tail = &index[header_mobile_start..];
    let header_mobile_end = header_mobile_tail
        .find("</div>\n    </div>\n    <div class=\"header-right\">")
        .expect("mobile identity header end");
    assert!(!header_mobile_tail[..header_mobile_end].contains("header-identity-chevron"));
    assert!(lxmf.contains("openContactAddOptions(trigger)"));
    assert!(lxmf.contains("openAddContactPrompt(document.getElementById('contacts-add-fab'))"));

    assert!(contact_card_js.contains("BarcodeDetector"));
    assert!(contact_card_js.contains("RS.mediaPermissions.ensure({ camera: true })"));
    assert!(contact_card_js.contains("RS.invoke('api_preview_contact_card'"));
    assert!(contact_card_js.contains("RS.invoke('import_contact_card'"));
    assert!(contact_card_js.contains("renderQrCanvas(canvas, card.payload || '')"));
    assert!(contact_card_js.contains("function QrContactCard(text)"));
    assert!(contact_card_js.contains("var VERSION = 13;"));
    assert!(contact_card_js.contains("var ERROR_CORRECTION_FORMAT_BITS = 3;"));
    assert!(contact_card_js.contains("var BYTE_COUNT_BITS = VERSION >= 10 ? 16 : 8;"));
    assert!(
        contact_card_js
            .contains("var DATA_BLOCK_SIZES = [20, 20, 20, 20, 20, 20, 20, 20, 21, 21, 21, 21];")
    );
    assert!(contact_card_js.contains("function drawVersionBits()"));
    assert!(contact_card_js.contains("0x1f25"));
    assert!(contact_card_js.contains("drawVersionBits();"));
    assert!(contact_card_js.contains("moduleFallsBehindLogo"));
    assert!(contact_card_js.contains("var logoSize = pixels * 0.155;"));
    assert!(contact_card_js.contains("var logoClearSize = logoSize * 1.18;"));
    assert!(
        contact_card_js
            .contains("drawRatspeakLogo(ctx, pixels / 2, pixels / 2, logoSize, qrSurface)")
    );
    assert!(contact_card_js.contains("var scanCanvas = document.createElement('canvas')"));
    assert!(contact_card_js.contains("scanCtx.drawImage(video"));
    assert!(contact_card_js.contains("detector.detect(scanCanvas)"));
    assert!(contact_card_js.contains("RATSPEAK_MARK_PATHS"));
    assert!(contact_card_js.contains("drawOfficialRatspeakMark"));
    assert!(contact_card_js.contains("new Path2D(RATSPEAK_MARK_PATHS[i])"));
    assert!(contact_card_js.contains("'<span>Copy</span></button>'"));
    assert!(contact_card_js.contains(r#"<circle cx="9" cy="7" r="4"/>"#));
    assert!(
        !contact_card_js.contains("M12 21s7-4.35"),
        "address contact action should use a peer/person icon, not a map pin"
    );
    assert!(!contact_card_js.contains("Share Card"));
    assert!(!contact_card_js.contains("contact-share-card"));
    assert!(!contact_card_js.contains("contact-scan-check"));
    assert!(contact_card_js.contains("function showContactAddDial"));
    assert!(
        contact_card_js.contains("isMobileContactFlow() && showContactAddDial(trigger, items)")
    );
    assert!(views_css.contains(".contact-share-sheet"));
    assert!(views_css.contains(".contact-scan-sheet"));
    assert!(views_css.contains("top: 50%;\n    left: 50%;\n    height: auto;"));
    assert!(views_css.contains("transform: translate(-50%, calc(-50% + 12px)) scale(0.98);"));
    assert!(views_css.contains("transform: translate(-50%, -50%) scale(1);"));
    assert!(views_css.contains(".contact-share-qr-shell"));
    assert!(views_css.contains(".contact-scan-camera-wrap"));
    assert!(views_css.contains(".contact-scan-avatar {\n    width: 72px;\n    height: 72px;\n    border-radius: var(--radius-full);"));
    assert!(views_css.contains(".contact-scan-avatar canvas"));
    assert!(
        !views_css.contains(".contact-scan-check"),
        "scan preview should lead with the peer avatar, not a separate success check"
    );
    assert!(views_css.contains("overflow-wrap: anywhere;"));
    assert!(responsive_css.contains(
        ".fab-dial-btn svg {\n        display: block;\n        width: 22px;\n        height: 22px;"
    ));
    assert!(responsive_css.contains(".view-fab.dial-open"));
    assert!(tauri_build.contains("build_dashboard_css();"));
    assert!(tauri_build.contains(r#""10-views.css""#));
    assert!(tauri_build.contains(r#""13-responsive.css""#));

    assert!(contact_card_rs.contains(r#"const CONTACT_CARD_PREFIX: &str = "RSCP1:""#));
    assert!(contact_card_rs.contains("Identity::from_public_key(&public_key)"));
    assert!(contact_card_rs.contains("Destination::hash_from_name_and_identity(LXMF_APP_NAME"));
    assert!(
        contact_card_rs.contains("mgr.update_remote_crypto(&dest_hash, &card.public_key, None)")
    );
    assert!(contact_card_rs.contains("mgr.save_crypto_state()"));
    assert!(contact_card_rs.contains("save_contact_with_identity_pubkey"));
    assert!(db.contains("pub fn save_contact_with_identity_pubkey"));

    assert!(lib.contains("commands::contact_card::api_contact_card"));
    assert!(lib.contains("commands::contact_card::api_preview_contact_card"));
    assert!(lib.contains("commands::contact_card::import_contact_card"));
}

#[test]
fn mobile_contacts_tab_keeps_desktop_header_out_of_search_flow() {
    let root = repo_root();
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    assert!(views_css.contains(".contacts-standalone-toolbar .conn-search-input"));
    assert!(views_css.contains("flex: 1 1 auto;"));
    assert!(views_css.contains("min-width: 0;"));
    assert!(views_css.contains("margin: 0;"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    assert!(responsive_css.contains(".contacts-standalone {\n        max-width: none;"));
    assert!(responsive_css.contains(".contacts-standalone-header {\n        display: none;"));
    assert!(responsive_css.contains(".contacts-standalone-toolbar #contacts-search"));
    assert!(responsive_css.contains("width: 100%;"));
    assert!(responsive_css.contains("margin: 0;"));
}

#[test]
fn mobile_tab_swipe_uses_bottom_bar_slots_without_view_slide_animation() {
    let nav = read_source(repo_root().join("dashboard/static/js/nav.js")).expect("nav js");
    assert!(nav.contains("var MOBILE_TAB_SLOTS = ['peers', 'message', 'contacts', 'more'];"));
    assert!(nav.contains("function _mobileTabSlot(viewId)"));
    assert!(nav.contains("function _viewForMobileTabSlot(slot)"));
    assert!(nav.contains("function blockMobileNavigation(ms)"));
    assert!(nav.contains("window.RS.blockMobileNavigation = blockMobileNavigation;"));
    assert!(
        nav.contains("if (_isMobileNavigationBlocked()) {\n                e.stopPropagation();")
    );
    assert!(nav.contains("localStorage.setItem('ratspeak_more_view', viewId)"));

    let start = nav.find("function initTabSwipe()").expect("initTabSwipe");
    let tail = &nav[start..];
    let end = tail
        .find("\n}\n\nvar FIRST_RUN_ANNOUNCE_HINT_KEY")
        .expect("initTabSwipe end");
    let init_tab_swipe = &tail[..end];
    assert!(init_tab_swipe.contains("MOBILE_TAB_SLOTS.indexOf(_mobileTabSlot(currentView))"));
    assert!(init_tab_swipe.contains("_viewForMobileTabSlot(MOBILE_TAB_SLOTS[nextIdx])"));
    assert!(init_tab_swipe.contains("if (_isMobileNavigationBlocked()) return true;"));
    assert!(init_tab_swipe.contains("switchView(targetView);"));
    assert!(
        !init_tab_swipe.contains("transition:"),
        "bottom-tab swipes should switch slots directly instead of overlapping full-screen slide animations"
    );
    assert!(
        !init_tab_swipe.contains("TAB_VIEWS[nextIdx]"),
        "More destinations must collapse to the More bottom-bar slot for swipe math"
    );
}

#[test]
fn mobile_haptics_use_tauri_plugin_commands_and_semantic_feedback() {
    let root = repo_root();
    let state_js = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    let nav = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let index_html = read_source(root.join("dashboard/index.html")).expect("dashboard html");
    let gestures = read_source(root.join("dashboard/static/js/gestures.js")).expect("gestures js");
    let constants =
        read_source(root.join("dashboard/static/js/constants.js")).expect("constants js");
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    let games = read_source(root.join("dashboard/static/js/games_tab.js")).expect("games js");
    let mut js_files = Vec::new();
    collect_files(&root.join("dashboard/static/js"), &mut js_files);

    assert!(state_js.contains("impactFeedback: 'impact_feedback'"));
    assert!(state_js.contains("notificationFeedback: 'notification_feedback'"));
    assert!(state_js.contains("selectionFeedback: 'selection_feedback'"));
    assert!(state_js.contains("'plugin:haptics|'"));
    assert!(nav.contains("case 'success':"));
    assert!(nav.contains("case 'warning':"));
    assert!(nav.contains("case 'error':"));
    assert!(nav.contains("step.kind === 'impact'    ? 'impact_feedback'"));
    assert!(nav.contains("step.kind === 'notify'    ? 'notification_feedback'"));
    assert!(nav.contains("'selection_feedback'"));
    assert!(!nav.contains("{ payload: step.payload }"));
    assert!(nav.contains("var HAPTICS_STORAGE_KEY = 'rs-haptics-enabled';"));
    assert!(nav.contains("if (!getHapticsEnabled()) return;"));
    assert!(settings_js.contains("function initHapticsToggle()"));
    assert!(index_html.contains("data-settings-title=\"General\""));
    assert!(index_html.contains("id=\"haptics-enabled-toggle\""));
    assert!(
        !index_html.contains("id=\"haptics-enabled-toggle\" checked"),
        "haptics should default off"
    );
    assert!(gestures.contains("if (typeof haptic === 'function') haptic(name);"));
    assert!(gestures.contains("G.bindViewFabClick = function(target, handler, opts)"));
    assert!(gestures.contains("RIPPLE_HAPTIC_SELECTORS"));
    assert!(constants.contains("RIPPLE_HAPTIC_SELECTORS"));
    assert!(lxmf.contains("function _voiceHaptic(name)"));
    assert!(lxmf.contains("_voiceHaptic('success')"));
    assert!(lxmf.contains("_voiceHaptic('warning')"));
    assert!(lxmf.contains("RS.gestures.bindViewFabClick(mainFab"));
    assert!(lxmf.contains("RS.gestures.bindViewFabClick('contacts-add-fab'"));
    assert!(games.contains("RS.gestures.bindViewFabClick('games-fab-btn'"));

    for path in js_files
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "js"))
    {
        let source = read_source(path).expect("js source");
        assert!(
            !source.contains("haptic(["),
            "{} should use semantic haptic names, not vibration arrays",
            path.display()
        );
        for digit in '0'..='9' {
            let needle = format!("haptic({digit}");
            assert!(
                !source.contains(&needle),
                "{} should use semantic haptic names, not raw durations",
                path.display()
            );
        }
    }
}

#[test]
fn message_actions_use_mobile_long_press_and_action_state() {
    let root = repo_root();
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    let messaging_css =
        read_source(root.join("dashboard/static/css/09-messaging.css")).expect("messaging css");
    let nav = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    let runtime =
        read_source(root.join("crates/ratspeak-runtime/src/lxmf.rs")).expect("runtime lxmf");
    let inbound =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime lib");
    let messaging = read_source(root.join("crates/ratspeak-tauri/src/commands/messaging.rs"))
        .expect("messaging command");

    assert!(lxmf.contains("RS.gestures.attachLongPress(bubble"));
    assert!(lxmf.contains("window.RS.closeMessageActionMenu"));
    assert!(lxmf.contains("function _copyToClipboardFallback(text)"));
    assert!(lxmf.contains("function _optimisticApplyReaction"));
    assert!(lxmf.contains("showToast(ok ? 'Message copied'"));
    assert!(messaging_css.contains(".lxmf-messages.msg-action-mode .msg-row"));
    assert!(messaging_css.contains(".msg-row.msg-action-selected .lxmf-msg"));
    assert!(nav.contains("RS.closeMessageActionMenu()"));

    assert!(runtime.contains("RATSPEAK_CHAT_CUSTOM_TYPE"));
    assert!(runtime.contains("ratspeak.chat.v1"));
    assert!(runtime.contains("decode_ratspeak_chat_extension"));
    assert!(runtime.contains("reaction_fallback_text"));
    assert!(inbound.contains("apply_inbound_ratspeak_reaction"));
    assert!(inbound.contains("\"reply_to_id\": reply_to_id"));
    assert!(inbound.contains("\"reaction_update\""));
    assert!(messaging.contains("\"reaction_update\""));
}

#[test]
fn first_run_announce_hint_waits_for_online_mobile_interface() {
    let root = repo_root();
    let nav = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    let events = read_source(root.join("dashboard/static/js/tauri_events.js")).expect("events js");
    let settings = read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let system =
        read_source(root.join("crates/ratspeak-tauri/src/commands/system.rs")).expect("system rs");
    let runtime = read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime rs");
    let rns_config =
        read_source(root.join("crates/ratspeak-runtime/src/rns_config.rs")).expect("rns config");
    let animations =
        read_source(root.join("dashboard/static/css/12-animations.css")).expect("animations css");

    assert!(nav.contains("Tap and hold to announce"));
    assert!(nav.contains("first-run-hint-svg"));
    assert!(nav.contains("<rect x=\"4\" y=\"16\" width=\"16\" height=\"4.5\" rx=\"2.25\"/>"));
    assert!(!nav.contains("<path d=\"M2 12 7 2l5 10-5 10z\""));
    assert!(nav.contains("function _firstRunMobileEligible()"));
    assert!(nav.contains("if (window.__RATSPEAK_DESKTOP__) return false;"));
    assert!(nav.contains("window.__RATSPEAK_MOBILE__ === true"));
    assert!(nav.contains("function updateFirstRunInterfaceHintGate(data)"));
    assert!(nav.contains("_firstRunConfiguredInterfaceCount(data) > 0"));
    assert!(nav.contains("_firstRunHasConfiguredInterface !== true"));
    assert!(nav.contains("_anyInterfaceOnline !== true"));
    assert!(nav.contains("if (opts.persist) _setFirstRunHintDone();"));
    assert!(nav.contains("if (opts.auto) _firstRunHintAutoHiddenThisSession = true;"));
    assert!(nav.contains("scheduleFirstRunTooltip(2000);"));
    assert!(
        events
            .contains("if (_anyInterfaceOnline && typeof scheduleFirstRunTooltip === 'function')")
    );
    assert!(events.contains("updateFirstRunInterfaceHintGate(data)"));
    assert!(settings.contains("clearFirstRunAnnounceHintDone"));
    assert!(system.contains("app_private_rns_config_dir"));
    assert!(system.contains("remove app-private Reticulum config"));
    assert!(runtime.contains("strip_legacy_default_auto_interface(&source_content)"));
    assert!(rns_config.contains("pub fn strip_legacy_default_auto_interface"));
    assert!(animations.contains("bottom: calc(62px + var(--sab, 0px) + 20px);"));
    assert!(animations.contains("background: var(--surface-sheet);"));
    assert!(animations.contains(".first-run-hint-icon"));
    assert!(animations.contains("background: var(--accent-a12);"));
}

#[test]
fn identity_management_is_first_class_tab() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    assert!(index.contains(r#"data-view="identity""#));
    assert!(index.contains(r#"id="view-identity""#));
    assert!(index.contains(r#"id="identity-import-btn""#));
    assert!(index.contains(r#"id="setup-import-identity-btn""#));
    assert!(index.contains("application/json,application/octet-stream,text/plain"));
    assert!(index.contains("title=\"Import identity\""));
    assert!(index.contains(r#"<path d="M7 10l5 5 5-5"/>"#));
    assert!(!index.contains("Import identity backup"));
    assert!(!index.contains(r#"<path d="M7 8l5-5 5 5"/>"#));

    let nav = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    assert!(nav.contains("'identity'"));
    assert!(nav.contains("var DEFAULT_MORE_VIEW = 'identity';"));
    assert!(!nav.contains("'identity': 'settings'"));

    let identity_js =
        read_source(root.join("dashboard/static/js/identity.js")).expect("identity js");
    assert!(identity_js.contains("api_preview_identity_import_base64"));
    assert!(identity_js.contains("api_export_identity_backup_base64"));
    assert!(identity_js.contains("api_export_identity_reticulum_base64"));
    assert!(identity_js.contains("api_export_identity_reticulum_base32"));
    assert!(identity_js.contains("Export Private Identity"));
    assert!(identity_js.contains("function exportIdentityBackup(hash)"));
    assert!(identity_js.contains(r#"<path d="M7 14l5-5 5 5"/>"#));
    assert!(!identity_js.contains(r#"<path d="M7 16l5 5 5-5"/>"#));
    assert!(identity_js.contains("function identityImportFormatChoices()"));
    assert!(identity_js.contains("function identityExportFormatChoices()"));
    assert!(identity_js.contains("Reticulum Identity Key"));
    assert!(identity_js.contains("Reticulum Base32 Key"));
    assert!(identity_js.contains("reticulum-base32"));
    assert!(!identity_js.contains("NomadNet"));
    assert!(!identity_js.contains("Sideband"));
    assert!(identity_js.contains("function resetPendingIdentityImport()"));
    assert!(identity_js.contains("fileInput.addEventListener('cancel'"));
    assert!(identity_js.contains("function openIdentityBackupWithAndroid()"));
    assert!(identity_js.contains("window.RatspeakAndroid.importIdentityBackup();"));
    assert!(
        identity_js.contains(
            "function handleImportBackupPayload(fileName, fileSize, b64, expectedFormat)"
        )
    );
    assert!(identity_js.contains("var fromSetup = !!window._identityImportFromSetup;"));
    assert!(identity_js.contains("var activateHtml = fromSetup ? ''"));
    assert!(identity_js.contains("completeSetupAfterIdentityImport(data);"));
    assert!(identity_js.contains("Choose Reticulum Identity Key import"));
    assert!(identity_js.contains("Choose Ratspeak Identity Backup import"));
    assert!(identity_js.contains("mimeType: 'application/octet-stream'"));
    assert!(identity_js.contains("function saveIdentityBackupWithAndroid(fileName, backupBase64)"));
    assert!(
        identity_js
            .contains("function saveIdentityDocumentWithAndroid(fileName, dataBase64, mimeType)")
    );
    assert!(
        identity_js
            .contains("window.RatspeakAndroid.exportIdentityBackup(fileName, backupBase64);")
    );
    assert!(
        identity_js.contains("window.RatspeakAndroid.saveIdentityDocument(fileName, dataBase64")
    );
    assert!(!identity_js.contains("Identity backup ready"));
    assert!(!identity_js.contains("Export Backup"));
    assert!(identity_js.contains("function openIdentityActions(hash)"));
    assert!(identity_js.contains("function deleteIdentityByHash(hash)"));

    let dialogs_js = read_source(root.join("dashboard/static/js/dialogs.js")).expect("dialogs js");
    assert!(dialogs_js.contains("built.sheet.addEventListener('keydown'"));
    assert!(!dialogs_js.contains("built.overlay.addEventListener('keydown'"));
    assert!(dialogs_js.contains("btn.appendChild(hint);"));

    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    assert!(index.contains("Identity Management"));
    assert!(index.contains("Identity Detail"));
    assert!(!index.contains("id=\"identity-export-btn\""));
    assert!(!index.contains("identity-panel-actions"));

    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    assert!(views_css.contains(".identity-page-header"));
    assert!(views_css.contains(".identity-management-grid"));
    assert!(views_css.contains(".identity-detail-hero"));
    assert!(views_css.contains(".identity-address-row"));
    assert!(views_css.contains(".identity-detail-actions"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    assert!(responsive_css.contains(".identity-toolbar-btn span"));
    assert!(responsive_css.contains("display: none;"));

    let modals_css =
        read_source(root.join("dashboard/static/css/08-modals.css")).expect("modals css");
    assert!(modals_css.contains(".rs-dialog-choice"));
    assert!(modals_css.contains("flex-direction: column;"));
    assert!(modals_css.contains(".rs-dialog-choice-hint"));

    let android_activity = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("android main activity");
    assert!(
        android_activity
            .contains("fun exportIdentityBackup(fileName: String, backupBase64: String)")
    );
    assert!(android_activity.contains(
        "fun saveIdentityDocument(fileName: String, dataBase64: String, mimeType: String)"
    ));
    assert!(android_activity.contains("fun importIdentityBackup()"));
    assert!(android_activity.contains("Intent.ACTION_CREATE_DOCUMENT"));
    assert!(android_activity.contains("?: \"application/octet-stream\""));
    assert!(android_activity.contains("launchIdentityDocumentSave(safeName, bytes, mimeType)"));
    assert!(android_activity.contains("Intent.ACTION_OPEN_DOCUMENT"));
    assert!(android_activity.contains("contentResolver.openOutputStream(uri)"));
    assert!(android_activity.contains("contentResolver.openInputStream(uri)"));
    assert!(android_activity.contains("MAX_IDENTITY_IMPORT_BYTES"));
    assert!(android_activity.contains("_onAndroidIdentityExportResult"));
    assert!(android_activity.contains("_onAndroidIdentityImportResult"));

    let setup_js = read_source(root.join("dashboard/static/js/setup.js")).expect("setup js");
    assert!(setup_js.contains("function completeSetupAfterIdentityImport()"));
    assert!(setup_js.contains("runConnectingProgress();"));

    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    assert!(tauri_lib.contains("api_export_identity_reticulum_base64"));
    assert!(tauri_lib.contains("api_export_identity_reticulum_base32"));

    let identity_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/identity.rs"))
        .expect("identity command source");
    assert!(identity_rs.contains("identity duplicate check db task panicked"));
    assert!(identity_rs.contains("Identity already exists"));
    assert!(identity_rs.contains("base32-private-key"));
    assert!(identity_rs.contains("api_export_identity_reticulum_base64"));
    assert!(identity_rs.contains("api_export_identity_reticulum_base32"));
}

#[test]
fn network_activity_opt_in_is_session_local() {
    let source =
        read_source(repo_root().join("dashboard/static/js/activity.js")).expect("activity js");

    assert!(source.contains("localStorage.removeItem('rs-activity-enabled')"));
    assert!(!source.contains("localStorage.setItem('rs-activity-enabled'"));
    assert!(source.contains("enabled: false, level: activityLevel"));
}

#[test]
fn transport_mode_defaults_and_auto_policy_are_explicit() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    let ui_shared_js =
        read_source(root.join("dashboard/static/js/ui_shared.js")).expect("ui shared js");
    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces source");

    assert!(index.contains(r#"id="transport-mode-select">OFF</button>"#));
    assert!(ui_shared_js.contains("Enables only on suitable non-LoRa interfaces."));
    assert!(ui_shared_js.contains("RS.ui.applyTransportModePayload"));
    assert!(ui_shared_js.contains("RS.ui.openTransportModeChoice"));
    assert!(ui_shared_js.contains("RS.ui.bindTransportChoice"));
    assert!(settings_js.contains("function applyTransportModePayload"));
    assert!(settings_js.contains("RS.ui.applyTransportModePayload"));
    assert!(settings_js.contains("RS.ui.bindTransportChoice"));
    assert!(
        settings_js.contains(
            "if (ifaces && ifaces.transport) applyTransportModePayload(ifaces.transport);"
        )
    );
    assert!(modals_js.contains("function applyModalTransportModePayload"));
    assert!(modals_js.contains("RS.ui.applyTransportModePayload"));
    assert!(modals_js.contains("RS.ui.bindTransportChoice"));
    assert!(modals_js.contains(
        "if (ifaces && ifaces.transport) applyModalTransportModePayload(ifaces.transport);"
    ));
    assert!(!settings_js.contains("Disables when on cellular or using LoRa."));
    assert!(!modals_js.contains("Disables when on cellular or using LoRa."));

    assert!(interfaces_rs.contains(r#""off".to_string()"#));
    assert!(interfaces_rs.contains("auto_transport_enabled_for_interfaces"));
    assert!(interfaces_rs.contains("has_enabled_non_lora_transport_interface"));
    assert!(interfaces_rs.contains("reconcile_auto_transport_after_interface_change"));
    assert!(interfaces_rs.contains("transport_network_type"));
    assert!(interfaces_rs.contains("configured_enabled"));
    assert!(interfaces_rs.contains("suppressed"));
    assert!(interfaces_rs.contains("InstanceMode::Client"));

    let shared_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/shared.rs"))
        .expect("shared source");
    let network_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/network.rs"))
        .expect("network source");
    assert!(shared_rs.contains("hub_interfaces_payload"));
    assert!(shared_rs.contains("\"transport\".to_string()"));
    assert!(shared_rs.contains("reconcile_auto_transport_after_interface_change"));
    assert!(network_rs.contains("hub_interfaces_payload"));
}

#[test]
fn android_logcat_output_is_privacy_gated() {
    let root = repo_root();
    let android_root = root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android");
    let mut files = Vec::new();
    collect_files(&android_root, &mut files);

    for path in files {
        if path.extension().and_then(|e| e.to_str()) != Some("kt") {
            continue;
        }
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string()
            .replace('\\', "/");
        if rel.ends_with("RatspeakDiagnostics.kt") || rel.ends_with("generated/Logger.kt") {
            continue;
        }
        let source = read_source(&path).expect("kotlin source");
        assert!(
            !source.contains("import android.util.Log"),
            "{rel} must use the gated package-local Log shim"
        );
    }

    let generated_logger =
        read_source(android_root.join("generated/Logger.kt")).expect("generated logger");
    assert!(generated_logger.contains("return RatspeakDiagnostics.enabled()"));

    let gradle = read_source(root.join("src-tauri/gen/android/app/build.gradle.kts"))
        .expect("android app gradle");
    assert!(gradle.contains("patchTauriGeneratedLogger"));
    assert!(gradle.contains("return BuildConfig.DEBUG"));
    assert!(gradle.contains("return RatspeakDiagnostics.enabled()"));
    assert!(gradle.contains("dependsOn(patchTauriGeneratedLogger)"));
    assert!(gradle.contains("finalizedBy(patchTauriGeneratedLogger)"));
    assert!(gradle.contains("outputs.upToDateWhen { false }"));
}

#[test]
fn apple_generated_native_sources_do_not_emit_direct_logs() {
    let root = repo_root();
    let apple_sources = root.join("src-tauri/gen/apple/Sources");
    let mut files = Vec::new();
    collect_files(&apple_sources, &mut files);

    for path in files {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "swift" | "m" | "mm" | "h") {
            continue;
        }
        let source = read_source(&path).expect("apple native source");
        let rel = path.strip_prefix(&root).unwrap_or(&path).display();
        for disallowed in ["NSLog(", "os_log(", "OSLog(", "print("] {
            assert!(
                !source.contains(disallowed),
                "{rel} must not emit direct native logs"
            );
        }
    }
}

#[test]
fn peer_reachability_uses_uncapped_path_index() {
    let root = repo_root();
    let state = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    assert!(state.contains("function pathCountSummary"));
    assert!(state.contains("path_table_total"));

    let runtime = read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime");
    assert!(runtime.contains("\"path_index\": path_index"));
    assert!(runtime.contains("path_table_stats_snapshot(entries)"));

    let rns = read_source(root.join("crates/ratspeak-runtime/src/rns.rs")).expect("rns");
    assert!(rns.contains("pub fn path_table_stats_snapshot"));
    assert!(rns.contains("let mut path_index = Map::with_capacity(entries.len())"));
    assert!(rns.contains("path_table_ui_snapshot(entries)"));

    let peers = read_source(root.join("dashboard/static/js/peers_cache.js")).expect("peers cache");
    assert!(peers.contains("lastStats.path_index"));
    assert!(peers.contains("pathLookup[h] = pathIndex[h]"));
    assert!(peers.contains("else if (pathTable)"));
    assert!(peers.contains("function pathInfo(hash, service, pathLookup, nowSecs)"));
    assert!(peers.contains("function primaryRouteInfo(messageInfo, voiceInfo)"));
    assert!(peers.contains("entry.telephony_hash"));
    assert!(peers.contains("message_route_label: messageInfo.route_label"));
    assert!(peers.contains("voice_route_label: voiceInfo.route_label"));
    assert!(peers.contains("route_service: primaryInfo.service"));

    let connections =
        read_source(root.join("dashboard/static/js/connections.js")).expect("connections");
    assert!(connections.contains("pathCountSummary(data)"));

    let health = read_source(root.join("dashboard/static/js/health.js")).expect("health");
    assert!(health.contains("renderPathTable(data.path_table || [], data)"));
}

#[test]
fn peer_spammer_names_are_ui_suppressed_not_user_blocked() {
    let root = repo_root();
    let peers = read_source(root.join("dashboard/static/js/peers_cache.js")).expect("peers cache");
    assert!(peers.contains("function _isSuppressedPeerDisplayName(displayName)"));
    assert!(peers.contains("/meshtastic/i.test(name)"));
    assert!(peers.contains("/^![a-f0-9]{8}$/i.test(name)"));
    assert!(peers.contains("if (_isSuppressedPeerEntry(_cache[h])) continue;"));
    assert!(peers.contains("return _isSuppressedPeerEntry(entry) ? null : entry;"));

    let settings = read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    assert!(
        !settings.contains("_isSuppressedPeerDisplayName"),
        "automatic spammer suppression must not appear in the user block list"
    );
}

#[test]
fn peers_are_filtered_to_ratspeak_actionable_services() {
    let root = repo_root();
    let peers = read_source(root.join("dashboard/static/js/peers_cache.js")).expect("peers cache");
    assert!(peers.contains("function _hasSupportedPeerService(entry)"));
    assert!(peers.contains("services.indexOf('lxmf.delivery') !== -1"));
    assert!(peers.contains("services.indexOf('lxst.telephony') !== -1"));
    assert!(peers.contains("telephony_hash"));
    assert!(peers.contains("supports_lxst_call"));

    let db = read_source(root.join("crates/ratspeak-db/src/db.rs")).expect("db");
    assert!(db.contains("pub const PEER_SERVICE_LXMF_DELIVERY: &str = \"lxmf.delivery\";"));
    assert!(db.contains("pub const PEER_SERVICE_LXST_TELEPHONY: &str = \"lxst.telephony\";"));
    assert!(db.contains("fn peer_service_filter_sql(column: &str) -> String"));

    let handlers = read_source(root.join("crates/ratspeak-runtime/src/announce_handlers.rs"))
        .expect("handlers");
    assert!(handlers.contains("pub async fn spawn_lxst_telephony_handler"));
    assert!(handlers.contains("const LXST_TELEPHONY_ASPECT: &str = \"lxst.telephony\";"));
    assert!(handlers.contains("Destination::hash_from_name_and_identity(\"lxmf.delivery\""));
    assert!(handlers.contains("db::PEER_SERVICE_LXST_TELEPHONY"));

    let runtime = read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime");
    assert!(runtime.contains("pub fn telephony_hash_for_identity_hex"));
    assert!(
        runtime.contains("\"telephony_hash\": telephony_hash_for_identity_hex(&r.identity_hash)")
    );

    let tauri_peers =
        read_source(root.join("crates/ratspeak-tauri/src/commands/peers.rs")).expect("peers cmd");
    assert!(tauri_peers.contains(
        "\"telephony_hash\": ratspeak_runtime::telephony_hash_for_identity_hex(&r.identity_hash)"
    ));
}

#[test]
fn network_view_hides_shared_instance_internal_interfaces() {
    let health = read_source(repo_root().join("dashboard/static/js/health.js")).expect("health js");
    assert!(health.contains(
        "role === 'local_client' || role === 'shared_instance_peer' || role === 'shared_server'"
    ));
    assert!(health.contains(
        "role === 'shared_instance_peer' || role === 'shared_server' || role === 'local_client'"
    ));
    assert!(!health.contains("if (role === 'shared_server') return 'host';"));
    assert!(!health.contains("if (role === 'shared_instance_peer') return 'tcp';"));
}

#[test]
fn propagated_send_paths_run_relay_readiness_preflight() {
    let root = repo_root();
    let propagation = read_source(root.join("crates/ratspeak-runtime/src/propagation.rs"))
        .expect("propagation source");
    assert!(propagation.contains("Stops active client sync state"));
    assert!(!propagation.contains("In-flight sync drains"));

    let messaging = read_source(root.join("crates/ratspeak-tauri/src/commands/messaging.rs"))
        .expect("messaging commands");
    let shared = read_source(root.join("crates/ratspeak-tauri/src/commands/shared.rs"))
        .expect("shared command helpers");
    let announce_handlers =
        read_source(root.join("crates/ratspeak-runtime/src/announce_handlers.rs"))
            .expect("announce handlers");
    for fn_name in [
        "send_lxmf_message",
        "send_reaction",
        "send_lxmf_reply",
        "send_lxmf_propagated",
        "send_lxmf_with_attachment",
    ] {
        let marker = format!("pub async fn {fn_name}");
        let start = messaging.find(&marker).expect("send function exists");
        let rest = &messaging[start..];
        let next = rest.find("\n#[tauri::command]").unwrap_or(rest.len());
        let body = &rest[..next];
        assert!(
            body.contains("ensure_propagation_ready_for_send("),
            "{fn_name} must not bypass propagation relay readiness checks"
        );
    }
    assert!(messaging.contains("destination_identity_known(state, dest_hash)"));
    assert!(messaging.contains("Recipient identity key is not known yet"));
    assert!(shared.contains("hydrate_contact_identity_for_send"));
    assert!(shared.contains("db::get_contact(&p, &dest_for_db, &identity_id)"));
    assert!(shared.contains("mgr.update_remote_crypto(&dest_hash, &public_key, None)"));
    assert!(
        announce_handlers
            .contains("trigger_outbound_for_delivery_announce(event.destination_hash)")
    );
    assert!(announce_handlers.contains("trigger_outbound_for_propagation_node_announce("));
    assert!(announce_handlers.contains("state.lxmf_notify.notify_one()"));

    let games = read_source(root.join("crates/ratspeak-tauri/src/commands/games.rs"))
        .expect("game commands");
    for fn_name in ["send_game_action", "resend_last_game_action"] {
        let marker = format!("pub async fn {fn_name}");
        let start = games.find(&marker).expect("game send function exists");
        let rest = &games[start..];
        let next = rest.find("\n#[tauri::command]").unwrap_or(rest.len());
        let body = &rest[..next];
        assert!(
            body.contains("ensure_propagation_ready_for_send("),
            "{fn_name} must not bypass propagation relay readiness checks"
        );
    }
}

#[test]
fn offline_inbox_auto_settings_use_ratspeak_node_preference() {
    let root = repo_root();
    let propagation_js =
        read_source(root.join("dashboard/static/js/propagation.js")).expect("propagation js");
    let settings_html = read_source(root.join("dashboard/index.html")).expect("dashboard html");
    let network_commands = read_source(root.join("crates/ratspeak-tauri/src/commands/network.rs"))
        .expect("network commands");

    assert!(propagation_js.contains("args.favorStatic = !!opts.favor_static"));
    assert!(network_commands.contains("favorStatic: Option<bool>"));
    assert!(propagation_js.contains("Auto selected"));
    assert!(propagation_js.contains("if (mode === 'manual')"));
    assert!(propagation_js.contains("Propagation address<br>"));
    assert!(!propagation_js.contains("Connecting..."));
    assert!(!settings_html.contains("Relay Node"));
    assert!(settings_html.contains("Offline Inbox"));
    assert!(propagation_js.contains("beginRelayRefresh(RELAY_REFRESH_WATCHDOG_MS)"));
    assert!(propagation_js.contains("finishRelayRefresh()"));
    assert!(propagation_js.contains("clearRelayRefreshWatchdog()"));
    assert!(network_commands.contains("propagation::request_relay_path(&st, node).await"));
    assert!(
        network_commands.contains("crate::propagation::request_relay_path(&state, node).await")
    );
    assert!(network_commands.contains("ensure_relay_ready_for_send(&state).await"));
}

#[test]
fn lxmf_tick_runs_blocking_work_off_async_runtime() {
    let root = repo_root();
    let runtime =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime source");
    let lxmf = read_source(root.join("crates/ratspeak-runtime/src/lxmf.rs")).expect("lxmf source");

    assert!(runtime.contains("tokio::task::spawn_blocking(move ||"));
    assert!(runtime.contains("mgr.tick_with_auto_propagation_download_ready("));
    assert!(runtime.contains("lxmf tick worker failed; skipping this tick"));
    assert!(lxmf.contains("OutboundAction::Failed(message) | OutboundAction::Expired(message)"));
    assert!(lxmf.contains("expired_or_attempt_exhausted_outbound_surfaces_failed_state"));
}

#[test]
fn bundled_ratspeak_propagation_nodes_are_destination_hashes_with_sync_hub_priority() {
    let root = repo_root();
    let nodes = read_source(root.join("crates/ratspeak-db/nodes.json")).expect("nodes json");
    let propagation = read_source(root.join("crates/ratspeak-runtime/src/propagation.rs"))
        .expect("propagation source");
    let announce_handlers =
        read_source(root.join("crates/ratspeak-runtime/src/announce_handlers.rs"))
            .expect("announce handlers");

    assert!(nodes.contains("deadbeefbadfceeae39c1aceb911e205"));
    assert!(nodes.contains("\"role\": \"sync_hub\""));
    assert!(nodes.contains("\"priority\": 0"));
    assert!(propagation.contains("registry_static_priority(favor_static && is_static"));
    assert!(propagation.contains("favor_static_prefers_sync_hub_over_lower_hop_static_node"));
    assert!(propagation.contains("static_probe_goal_satisfied_by_active"));
    assert!(
        propagation.contains("secondary_ratspeak_node_does_not_stop_sync_hub_background_probe")
    );
    assert!(propagation.contains("const STATIC_STARTUP_PROBE_BUDGET: usize = 1"));
    assert!(propagation.contains("static_probe_prefers_sync_hub_first"));
    assert!(announce_handlers.contains("let hash_hex = hex::encode(event.destination_hash);"));
    assert!(announce_handlers.contains("mgr.router"));
    assert!(announce_handlers.contains("set_stamp_cost(event.destination_hash"));
}
