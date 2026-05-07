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

    let health_js = read_source(root.join("dashboard/static/js/health.js")).expect("health js");
    assert!(health_js.contains("networkAnnounceBtn.dataset.announcePending = '1'"));
    assert!(health_js.contains("networkAnnounceBtn.dataset.announcePending !== '1'"));

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
    assert!(lxmf.contains("sendBtn.addEventListener('pointerdown'"));
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
    assert!(
        !index.contains(r#"id="lxmf-camera-input" accept="image/*,video/*""#),
        "Camera action must request still-image capture instead of the generic media picker"
    );

    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf.contains("function triggerCameraAttachment()"));
    assert!(lxmf.contains("var input = document.getElementById('lxmf-camera-input');"));
    assert!(
        lxmf.contains("{ label: 'Camera', icon: ICON_CAMERA, onSelect: triggerCameraAttachment }")
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
fn mobile_peers_rows_are_larger_and_detail_sheet_expands_progressively() {
    let root = repo_root();
    let peers = read_source(root.join("dashboard/static/js/peers.js")).expect("peers js");
    assert!(peers.contains("_peersRowHeight = window.innerWidth <= 768 ? 54 : 36;"));
    assert!(peers.contains("var avatarSize = window.innerWidth <= 768 ? 42 : 28;"));
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
    assert!(connections.contains("dy < -28"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("css");
    assert!(responsive_css.contains(".peers-row {\n        min-height: 54px;"));
    assert!(
        responsive_css.contains(".peers-row-avatar {\n        width: 42px;\n        height: 42px;")
    );
    assert!(responsive_css.contains(".conn-detail-sheet.conn-detail-sheet--progressive"));
    assert!(responsive_css.contains(
        ".conn-detail-sheet--progressive .conn-detail-sheet-fields {\n    display: none;"
    ));
    assert!(responsive_css.contains(
        ".conn-detail-sheet--progressive.conn-detail-sheet--expanded .conn-detail-sheet-fields"
    ));
}

#[test]
fn lxmf_conversation_rows_use_peer_display_names_when_available() {
    let lxmf = read_source(repo_root().join("dashboard/static/js/lxmf.js")).expect("lxmf js");

    assert!(lxmf.contains("function _conversationNameInfo(hash, payloadName, isContact)"));
    assert!(lxmf.contains("function _conversationPayloadForHash(hash)"));
    assert!(lxmf.contains("var announceName = _lookupAnnounceName(hash);"));
    assert!(lxmf.contains("return { name: _hashFallbackName(hash), isHash: true };"));
    assert!(lxmf.contains("PeersCache.subscribe(_refreshRenderedConversationNames);"));
    assert!(lxmf.contains("_refreshRenderedConversationNames();"));
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
    assert!(lxmf.contains("['contacts-add-fab', 'contacts-add-btn'].forEach(function(id)"));
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
    assert!(nav.contains("localStorage.setItem('ratspeak_more_view', viewId)"));

    let start = nav.find("function initTabSwipe()").expect("initTabSwipe");
    let tail = &nav[start..];
    let end = tail
        .find("\n}\n\nvar _firstRunDismiss")
        .expect("initTabSwipe end");
    let init_tab_swipe = &tail[..end];
    assert!(init_tab_swipe.contains("MOBILE_TAB_SLOTS.indexOf(_mobileTabSlot(currentView))"));
    assert!(init_tab_swipe.contains("_viewForMobileTabSlot(MOBILE_TAB_SLOTS[nextIdx])"));
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
    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces source");

    assert!(index.contains(r#"id="transport-mode-select">OFF</button>"#));
    assert!(settings_js.contains("Enables only on suitable non-LoRa interfaces."));
    assert!(settings_js.contains("function applyTransportModePayload"));
    assert!(
        settings_js.contains(
            "if (ifaces && ifaces.transport) applyTransportModePayload(ifaces.transport);"
        )
    );
    assert!(modals_js.contains("Enables only on suitable non-LoRa interfaces."));
    assert!(modals_js.contains("function applyModalTransportModePayload"));
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

    let connections =
        read_source(root.join("dashboard/static/js/connections.js")).expect("connections");
    assert!(connections.contains("pathCountSummary(data)"));

    let health = read_source(root.join("dashboard/static/js/health.js")).expect("health");
    assert!(health.contains("renderPathTable(data.path_table || [], data)"));
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
