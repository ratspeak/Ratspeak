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

fn rust_struct_literal_blocks<'a>(source: &'a str, marker: &str) -> Vec<&'a str> {
    source
        .match_indices(marker)
        .map(|(idx, _)| {
            let tail = &source[idx..];
            let start = tail.find('{').expect("struct literal start");
            let mut depth = 0usize;
            for (offset, ch) in tail[start..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return &tail[..start + offset + 1];
                        }
                    }
                    _ => {}
                }
            }
            panic!("unterminated struct literal for {marker}");
        })
        .collect()
}

fn rust_call_blocks<'a>(source: &'a str, call_path: &str) -> Vec<&'a str> {
    let marker = format!("{call_path}(");
    source
        .match_indices(&marker)
        .map(|(idx, _)| {
            let tail = &source[idx..];
            let start = call_path.len();
            let mut depth = 0usize;
            for (offset, ch) in tail[start..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return &tail[..start + offset + 1];
                        }
                    }
                    _ => {}
                }
            }
            panic!("unterminated function call for {call_path}");
        })
        .collect()
}

fn rust_function_block<'a>(source: &'a str, function_name: &str) -> &'a str {
    let marker = format!("fn {function_name}(");
    let index = source
        .find(&marker)
        .unwrap_or_else(|| panic!("missing function {function_name}"));
    let tail = &source[index..];
    let start = tail.find('{').expect("function body start");
    let mut depth = 0usize;
    for (offset, ch) in tail[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return &tail[..start + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function {function_name}");
}

#[test]
fn channels_are_live_only_and_wired_across_runtime_ipc_and_responsive_ui() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let channels_js =
        read_source(root.join("dashboard/static/js/channels.js")).expect("channels js");
    let channels_css =
        read_source(root.join("dashboard/static/css/09-channels.css")).expect("channels css");
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    let nav_js = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    let build_css = read_source(root.join("dashboard/build-css.sh")).expect("css build script");
    let runtime = read_source(root.join("crates/ratspeak-runtime/src/channels.rs"))
        .expect("channels runtime");
    let commands = read_source(root.join("crates/ratspeak-tauri/src/commands/channels.rs"))
        .expect("channels commands");
    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    let tauri_build = read_source(root.join("src-tauri/build.rs")).expect("tauri build script");
    let db = read_source(root.join("crates/ratspeak-db/src/db.rs")).expect("database source");

    assert!(index.contains("data-view=\"channels\""));
    assert!(index.contains("id=\"view-channels\""));
    assert!(index.contains("/static/js/channels.js"));
    assert!(index.contains("id=\"channel-message-input\""));
    assert!(!index.contains("Group conversations"));
    assert!(!index.contains("channels-sidebar-subtitle"));
    assert!(index.contains("Available hubs"));
    assert!(index.contains("id=\"channel-owned-hub\""));
    assert!(index.contains("id=\"channel-owned-hub-manage\""));
    assert!(!index.contains("channels-refresh-btn"));
    assert!(index.contains("id=\"channel-members-back\""));
    assert!(!index.contains("Messages are not saved and disappear when this session ends."));
    assert!(!index.contains("id=\"channel-session-banner\""));
    assert!(channels_js.contains("hub relays and can read channel messages"));
    assert!(channels_js.contains("Keys are sent over the authenticated Link and are never saved"));
    assert!(channels_js.contains("RS.listen('channels_snapshot'"));
    assert!(channels_js.contains("RS.listen('announce_received'"));
    assert!(channels_js.contains("function _channelsBuildHubMark"));
    assert!(channels_js.contains("function _channelsHubDistance"));
    assert!(channels_js.contains("function channelsConnectToHub"));
    assert!(channels_js.contains("channelHubOwnDestinationHash"));
    assert!(!channels_js.contains("hub.nearby ? 'Nearby' : 'Recent'"));
    assert!(channels_js.contains("TextEncoder"));
    assert!(channels_js.contains("channel-transition-card"));
    assert!(channels_js.contains("Messages unlock when the hub confirms your membership."));
    assert!(channels_js.contains("dataset.channelAction = 'retry-room'"));
    assert!(channels_js.contains("case 'joined': return 'Live';"));
    assert_eq!(channels_js.matches("return 'Live'").count(), 1);
    assert!(channels_js.contains("function _channelsIdentityTone"));
    assert!(!channels_js.contains("function _channelsInitials"));
    assert!(channels_css.contains(".channel-hub-row-mark"));
    assert!(channels_css.contains(".channel-hub-row-distance"));
    assert!(channels_js.contains("function _channelsBuildHubNotice"));
    assert!(channels_js.contains("function _channelsBuildHubGreeting"));
    assert!(channels_js.contains("function _channelsGroupPresenceEvents"));
    assert!(channels_js.contains("function _channelsBuildPresenceGroup"));
    assert!(channels_js.contains("function _channelsRenderMemberDetail"));
    assert!(channels_js.contains("function _channelsApplyComposerTypingPolicy"));
    assert!(channels_js.contains("function _channelsHandleComposerBeforeInput"));
    assert!(channels_js.contains("event.inputType !== 'insertReplacementText'"));
    assert!(
        channels_js.contains("_channelsApplyComposerTypingPolicy(input, useMobileTypingDefaults)")
    );
    assert!(channels_js.contains("PeersCache.enriched()"));
    assert!(channels_js.contains("services.indexOf('lxmf.delivery')"));
    assert!(channels_js.contains("disableAutoCorrect(roomInput)"));
    assert!(channels_js.contains("You\\u2019re already in "));
    assert!(channels_js.contains("result.local_command"));
    assert!(responsive_css.contains(".channels-layout.view-channel-detail"));
    assert!(responsive_css.contains("body.view-channel-detail .bottom-bar"));
    assert!(responsive_css.contains("calc(56px + var(--sat))"));
    assert!(responsive_css.contains("body.view-channel-detail .main-content"));
    assert!(responsive_css.contains(".channels-layout.room-live.members-open"));
    assert!(channels_css.contains(".channel-members-scrim"));
    assert!(channels_css.contains(".channels-layout:not(.room-live)"));
    assert!(channels_css.contains(".channel-transition-rail"));
    assert!(channels_css.contains(".channel-hub-greeting"));
    assert!(channels_css.contains(".channel-presence-summary"));
    assert!(channels_css.contains(".channel-member-detail-fields"));
    assert!(channels_js.contains("layout.classList.remove('members-open')"));
    assert!(build_css.contains("09-channels.css"));
    assert!(tauri_build.contains(r#""09-channels.css""#));

    assert!(nav_js.contains("var MOBILE_TAB_SLOTS = ['peers', 'message', 'contacts', 'more'];"));
    assert!(nav_js.contains("if (viewId === 'channels') return 'message';"));
    assert!(!nav_js.contains("MOBILE_TAB_SLOTS = ['peers', 'message', 'channels'"));

    assert!(runtime.contains("Nothing in this"));
    assert!(runtime.contains("module writes channel traffic to the Ratspeak database."));
    assert!(runtime.contains("TRANSCRIPT_LIMIT"));
    assert!(runtime.contains("JOIN_CONFIRM_TIMEOUT"));
    assert!(runtime.contains("apply_rrcd_room_status_notice"));
    assert!(runtime.contains("parse_rrcd_room_status"));
    assert!(runtime.contains("hub-attested observation of its source"));
    assert!(runtime.contains("room.members_complete = false"));
    assert!(runtime.contains("pub hub_greeting: Option<ChannelTranscriptItem>"));
    assert!(runtime.contains("WELCOME source does not match the authenticated hub"));
    assert!(commands.contains("fn parse_local_composer_command"));
    assert!(runtime.contains("pub async fn connect_known"));
    assert!(runtime.contains("channel hub identity does not match its destination"));
    assert!(commands.contains("Identity::from_file(&identity_path)"));
    assert!(commands.contains(".connect_known("));
    assert!(commands.contains("LocalComposerCommand::Join"));
    assert!(commands.contains("LocalComposerCommand::Part"));
    assert!(commands.contains("\"/list\""));
    for command in [
        "api_channels",
        "discover_channel_hubs",
        "connect_channel_hub",
        "disconnect_channel_hub",
        "join_channel",
        "part_channel",
        "send_channel_message",
        "api_saved_channel_hubs",
        "api_saved_channel_rooms",
    ] {
        assert!(commands.contains(&format!("fn {command}")));
        assert!(tauri_lib.contains(command));
    }

    let channel_schema = db
        .split("CREATE TABLE IF NOT EXISTS channel_hubs")
        .nth(1)
        .and_then(|tail| {
            tail.split("CREATE INDEX IF NOT EXISTS idx_channel_rooms")
                .next()
        })
        .expect("channel bookmark schema");
    assert!(!channel_schema.contains("message_body"));
    assert!(!channel_schema.contains("transcript"));
    assert!(!channel_schema.contains("member_hash"));
}

/// The hub persists operator policy and nothing else, creates rooms only for
/// the operator, and never stores a join key. Each assertion below stands for
/// a deliberate divergence from rrcd recorded in the fix registry; losing one
/// silently would be a privacy or availability regression, not a style change.
#[test]
fn channel_hub_persists_policy_only_and_gates_room_creation() {
    let root = repo_root();
    let hub = read_source(root.join("crates/ratspeak-runtime/src/channel_hub.rs"))
        .expect("channel hub runtime");
    let runtime =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime lifecycle");
    let state =
        read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("runtime state");
    let commands = read_source(root.join("crates/ratspeak-tauri/src/commands/channel_hub.rs"))
        .expect("channel hub commands");
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let hub_ui =
        read_source(root.join("dashboard/static/js/channel_hub.js")).expect("channel hub frontend");
    let channels_css =
        read_source(root.join("dashboard/static/css/09-channels.css")).expect("channels css");
    let db = read_source(root.join("crates/ratspeak-db/src/db.rs")).expect("database source");
    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");

    // The module doc is the human-readable half of the same contract.
    assert!(hub.contains("Relay traffic is live-only"));
    assert!(hub.contains("never reach the Ratspeak database"));

    // Durable hub state is policy. No traffic, no rosters, no nicknames.
    let hub_schema = db
        .split("CREATE TABLE IF NOT EXISTS channel_hub_rooms")
        .nth(1)
        .and_then(|tail| {
            tail.split("CREATE INDEX IF NOT EXISTS idx_contacts_identity")
                .next()
        })
        .expect("hub registry schema");
    for forbidden in [
        "message_body",
        "transcript",
        "member_hash",
        "nickname",
        "members",
        "body",
    ] {
        assert!(
            !hub_schema.contains(forbidden),
            "hub registry must not store `{forbidden}`"
        );
    }
    // The join key is only ever a verifiable digest.
    assert!(hub_schema.contains("key_mac"));
    assert!(!hub_schema.contains("key_plain"));
    assert!(hub.contains("fn room_key_matches"));
    assert!(hub.contains("hmac_verify"), "key checks stay constant time");
    assert!(
        !hub.contains("room.key = Some(key)"),
        "a plaintext join key must never be stored"
    );

    // Room creation is operator-only; commands look rooms up, never create.
    assert!(hub.contains("if !self.server_ops.contains(&identity) {"));
    assert!(
        hub.matches("entry(room_name.clone()).or_default()").count() == 1,
        "only the operator-gated JOIN path may create a room"
    );
    assert!(
        !hub.contains("room.founder"),
        "founder authority is not durable"
    );

    // Registry tables are wiped with the identity that owns them.
    for table in [
        "channel_hub_rooms",
        "channel_hub_grants",
        "channel_hub_klines",
    ] {
        assert!(
            db.contains(&format!("DELETE FROM {table} WHERE identity_id = ?1")),
            "{table} must cascade with its identity"
        );
        assert!(db.contains(&format!("\"{table}\",")), "{table} must reset");
    }

    // Every reply is measured against the packet budget rather than guessed.
    assert!(hub.contains("fn text_body_budget"));
    assert!(hub.contains("fn roster_chunk_len"));
    assert!(hub.contains("fn push_notice_entries"));
    assert!(
        hub.contains("NoticeHeader::Every(&format!(\"members in {room_name}: \"))"),
        "/who repeats its prefix; NomadNet parses each packet independently"
    );

    // Activity carries hub policy, never hub content. Asserted on the event
    // declaration itself: the mapping function only ever sees minted values.
    let hub_events = hub
        .split("pub(crate) enum HubEvent {")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n").next())
        .expect("hub activity event enum");
    for forbidden in ["String", "room_name", "topic", "nickname", "body", "text"] {
        assert!(
            !hub_events.contains(forbidden),
            "hub Activity events must not be able to carry `{forbidden}`"
        );
    }
    assert!(
        hub.contains("ChannelRoomToken::random()"),
        "room tokens are random, never derived from the room label"
    );

    // The IPC surface stays registered unconditionally.
    for command in [
        "channel_hub::api_channel_hub",
        "channel_hub::channel_hub_start",
        "channel_hub::channel_hub_stop",
        "channel_hub::channel_hub_set_config",
    ] {
        assert!(
            tauri_lib.contains(command),
            "{command} must stay registered"
        );
    }

    // Hosting is a desktop capability even though status IPC remains
    // available everywhere for one stable frontend contract.
    assert!(hub.contains("pub const fn channel_hub_hosting_supported()"));
    assert!(hub.contains("target_os = \"android\", target_os = \"ios\""));
    assert_eq!(
        commands.matches("ensure_supported()?;").count(),
        3,
        "every mutating hub command must reject mobile hosting"
    );
    assert!(runtime.contains("channel_hub_hosting_supported()"));

    // Desktop hosting is discoverable from the existing Channels add action,
    // but stays separate from client session state and obeys backend support.
    assert!(index.contains("/static/js/channel_hub.js"));
    assert!(hub_ui.contains("if (!overview || !overview.supported)"));
    assert!(hub_ui.contains("RS.invoke('api_channel_hub')"));
    assert!(hub_ui.contains("RS.invoke('channel_hub_start')"));
    assert!(hub_ui.contains("RS.invoke('channel_hub_stop')"));
    assert!(hub_ui.contains("RS.invoke('channel_hub_set_config'"));
    assert!(hub_ui.contains("RS.listen('channel_hub_snapshot'"));
    assert!(hub_ui.contains("function channelHubRenderHome"));
    assert!(hub_ui.contains("function channelHubOpenOwnHub"));
    assert!(hub_ui.contains("overview.created"));
    assert!(hub_ui.contains("Some channel changes are still waiting to be saved."));
    assert!(hub_ui.contains("copyAddress.hidden = !destination"));
    assert!(channels_css.contains(".channel-host-admin-sheet"));
    assert!(channels_css.contains(".channel-host-registry-warning"));
    assert!(channels_css.contains(".channel-host-copy-btn[hidden]"));
    assert!(channels_css.contains(".channel-owned-hub-card"));

    // Configuration reads independently of live state, writes as one SQLite
    // transaction, and serializes every lifecycle mutation.
    assert!(commands.contains("pub struct ChannelHubOverview"));
    assert!(commands.contains("pub created: bool"));
    assert!(commands.contains("pub destination_hash: Option<String>"));
    assert!(runtime.contains("channel_hub::hub_identity_path"));
    assert!(commands.contains("ChannelHubSettings::load"));
    assert!(commands.contains("try_set_settings"));
    assert!(commands.contains("hub.status()"));
    assert!(commands.contains("let status = current_snapshot(state).await"));
    assert!(commands.contains("existing_hub_destination_hash(&identity_path)"));
    assert!(db.contains("pub fn try_set_settings"));
    assert!(db.contains("let transaction = conn.transaction()"));
    assert!(state.contains("pub channel_hub_control_lock: tokio::sync::Mutex<()>"));
    for command in [
        "pub async fn channel_hub_start",
        "pub async fn channel_hub_stop",
        "pub async fn channel_hub_set_config",
    ] {
        let body = commands
            .split(command)
            .nth(1)
            .expect("hub lifecycle command");
        assert!(body.contains("channel_hub_control_lock.lock().await"));
    }
}

#[test]
fn channel_hub_shutdown_acknowledges_complete_teardown() {
    let root = repo_root();
    let hub = read_source(root.join("crates/ratspeak-runtime/src/channel_hub.rs"))
        .expect("channel hub runtime");
    let run_hub = hub
        .split("async fn run_hub(")
        .nth(1)
        .and_then(|tail| tail.split("/// Every hub event").next())
        .expect("hub service loop");

    let captures_ack = run_hub
        .find("shutdown_ack = Some(result_tx)")
        .expect("shutdown request captures its acknowledgement");
    let final_flush = run_hub
        .find("core.flush_dirty_last_used(&mut final_out)")
        .expect("final registry flush");
    let closes_destination = run_hub
        .find("registration.close().await")
        .expect("destination teardown");
    let sends_ack = run_hub
        .find("if let Some(result_tx) = shutdown_ack")
        .expect("completed teardown acknowledgement");

    assert!(captures_ack < final_flush);
    assert!(final_flush < closes_destination);
    assert!(closes_destination < sends_ack);
}

#[test]
fn activity_lxmf_progress_is_typed_and_content_free() {
    let root = repo_root();
    let runtime = read_source(root.join("crates/ratspeak-runtime/src/lib.rs"))
        .expect("runtime Activity adapter");
    let lxmf = read_source(root.join("crates/ratspeak-runtime/src/lxmf.rs")).expect("LXMF adapter");

    let progress_adapter = runtime
        .split("fn lxmf_progress_activity_step(")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("typed LXMF Activity adapter");
    assert!(progress_adapter.contains("update.kind"));
    assert!(progress_adapter.contains("update.event_method"));
    assert!(progress_adapter.contains("update.delivery_representation"));
    assert!(!progress_adapter.contains("update.step"));
    assert!(!progress_adapter.contains("update.reason"));
    assert!(!progress_adapter.contains("from_code(update.method)"));
    assert!(runtime.contains("lxmf_progress_supersedes_state"));

    for typed_field in [
        "pub kind: LxmfDeliveryProgressKind",
        "pub event_method: LxmfDeliveryProgressMethod",
        "pub delivery_representation: LxmfDeliveryProgressRepresentation",
    ] {
        assert!(lxmf.contains(typed_field));
    }
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
    assert!(interfaces_rs.contains("db::try_set_setting(&p, \"announce_ratspeak_usage\""));

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
fn ble_rnode_runtime_spawns_enable_flow_control() {
    let root = repo_root();
    let ble_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/ble.rs")).expect("ble commands");
    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");

    let native_blocks = rust_struct_literal_blocks(&ble_rs, "BleRnodeRuntimeArgs");
    assert_eq!(
        native_blocks.len(),
        1,
        "Android native BLE RNode should have one runtime-args block"
    );
    for block in native_blocks {
        assert!(
            block.contains("flow_control: true"),
            "Android native BLE RNode runtime args must opt into RNode CMD_READY flow control:\n{block}"
        );
    }

    let interface_blocks = rust_struct_literal_blocks(&interfaces_rs, "BleRnodeRuntimeArgs");
    assert_eq!(
        interface_blocks.len(),
        2,
        "interface commands should have editable/add BLE RNode runtime-args blocks"
    );
    for block in interface_blocks {
        assert!(
            block.contains("flow_control: true"),
            "BLE RNode runtime args must opt into RNode CMD_READY flow control:\n{block}"
        );
    }
}

#[test]
fn all_rnode_creation_paths_require_strict_capability_admission() {
    let root = repo_root();
    let runtime_rs =
        read_source(root.join("crates/ratspeak-runtime/src/rns.rs")).expect("RNS runtime");
    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    let ble_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/ble.rs")).expect("BLE commands");
    let strict_option = "RNodeStartupOptions::require_capability_admission()";

    let assert_strict_calls = |source: &str, call_path: &str, expected: usize| {
        let calls = rust_call_blocks(source, call_path);
        assert_eq!(
            calls.len(),
            expected,
            "unexpected exact call count for {call_path}"
        );
        for call in calls {
            assert!(
                call.contains(strict_option),
                "{call_path} must require capability admission:\n{call}"
            );
        }
    };

    assert_strict_calls(
        &runtime_rs,
        "reticulum::init_with_options_and_rnode_startup_options",
        1,
    );
    let configured_startup = rust_call_blocks(
        &runtime_rs,
        "reticulum::init_with_options_and_rnode_startup_options",
    );
    assert!(configured_startup[0].contains("InitOptions::default()"));
    assert_strict_calls(
        &interfaces_rs,
        "rns_runtime::reticulum::spawn_ble_rnode_runtime_observed_with_options",
        2,
    );
    assert_strict_calls(
        &interfaces_rs,
        "rns_runtime::reticulum::spawn_android_usb_rnode_runtime_observed_with_options",
        2,
    );
    assert_strict_calls(
        &interfaces_rs,
        "rns_runtime::reticulum::spawn_rnode_runtime_observed_with_options",
        2,
    );
    assert_strict_calls(
        &ble_rs,
        "rns_runtime::reticulum::spawn_ble_rnode_runtime_native_observed_with_options",
        1,
    );

    assert_eq!(runtime_rs.matches(strict_option).count(), 1);
    assert_eq!(interfaces_rs.matches(strict_option).count(), 6);
    assert_eq!(ble_rs.matches(strict_option).count(), 1);

    for legacy_call in ["reticulum::init", "reticulum::init_with_options"] {
        assert!(
            rust_call_blocks(&runtime_rs, legacy_call).is_empty(),
            "legacy RNode startup call remains: {legacy_call}"
        );
    }
    for legacy_call in [
        "rns_runtime::reticulum::spawn_ble_rnode_runtime_observed",
        "rns_runtime::reticulum::spawn_android_usb_rnode_runtime_observed",
        "rns_runtime::reticulum::spawn_rnode_runtime_observed",
    ] {
        assert!(
            rust_call_blocks(&interfaces_rs, legacy_call).is_empty(),
            "legacy RNode spawn call remains: {legacy_call}"
        );
    }
    assert!(
        rust_call_blocks(
            &ble_rs,
            "rns_runtime::reticulum::spawn_ble_rnode_runtime_native_observed",
        )
        .is_empty(),
        "legacy native BLE RNode spawn call remains"
    );
}

#[test]
fn dynamic_rnode_activity_monitors_are_exact_covered_and_ownership_gated() {
    let root = repo_root();
    let readiness = read_source(root.join("crates/ratspeak-tauri/src/commands/rnode_readiness.rs"))
        .expect("RNode readiness adapter");
    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    let ble =
        read_source(root.join("crates/ratspeak-tauri/src/commands/ble.rs")).expect("BLE commands");
    let runtime_monitor = read_source(root.join("crates/ratspeak-runtime/src/rnode_activity.rs"))
        .expect("RNode Activity monitor");
    let runtime_state =
        read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("runtime state");

    let pending_declaration = runtime_monitor
        .find("pub struct PendingRNodeActivityMonitor")
        .expect("single-use pending monitor seed");
    let pending_attributes = runtime_monitor[..pending_declaration]
        .rsplit("\n\n")
        .next()
        .expect("pending monitor attributes");
    assert!(!pending_attributes.contains("#[derive"));
    assert!(runtime_monitor.contains("origin: RNodeActivityOrigin"));
    let runtime_activation = rust_function_block(&runtime_monitor, "activate");
    assert!(runtime_activation.contains("self.origin"));
    assert!(
        runtime_state
            .contains("monitor: Option<crate::rnode_activity::PendingRNodeActivityMonitor>")
    );
    assert!(!runtime_state.contains(
        "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\npub enum BleRnodeOperationResult"
    ));

    let shared_wait = rust_function_block(&readiness, "await_spawned_rnode_ready");
    let cover = shared_wait
        .find("state.cover_rnode_activity_interface(spawned.interface_id, origin)")
        .expect("exact RNode poll coverage");
    let readiness_wait = shared_wait
        .find(".await_ready(RNODE_READINESS_TIMEOUT)")
        .expect("exact observer readiness wait");
    assert!(cover < readiness_wait, "coverage must precede readiness");
    assert!(shared_wait.contains("covered.then(||"));
    assert!(shared_wait.contains(
        "PendingRNodeActivityMonitor::new(spawned.observer.clone(), ready_snapshot, origin)"
    ));

    let owned_wait = rust_function_block(&interfaces, "await_owned_rnode_ready");
    assert!(owned_wait.contains("origin: RNodeActivityOrigin"));
    assert!(owned_wait.contains("await_spawned_rnode_ready(state, spawned, origin)"));

    let editable = rust_function_block(&interfaces, "spawn_editable_interface");
    let add = rust_function_block(&interfaces, "add_lora_interface");
    let editable_contexts =
        rust_call_blocks(editable, "rnode_activity_runtime_context_for_identity");
    assert_eq!(editable_contexts.len(), 1);
    assert!(editable_contexts[0].contains("activity_fence.identity_session_generation()"));
    assert!(editable.contains("(context.handle().clone(), context.origin())"));
    let add_contexts = rust_call_blocks(add, "rnode_activity_runtime_context_for_identity");
    assert_eq!(add_contexts.len(), 3);
    for context in add_contexts {
        assert!(context.contains("activity_fence.identity_session_generation()"));
    }
    for (source, expected_waits) in [(editable, 3usize), (add, 3usize)] {
        assert_eq!(
            source.matches("await_owned_rnode_ready(").count(),
            expected_waits
        );
        for call in rust_call_blocks(source, "await_owned_rnode_ready") {
            assert!(call.contains("rnode_activity_origin"));
        }
        for call_path in [
            "rns_runtime::reticulum::spawn_ble_rnode_runtime_observed_with_options",
            "rns_runtime::reticulum::spawn_android_usb_rnode_runtime_observed_with_options",
            "rns_runtime::reticulum::spawn_rnode_runtime_observed_with_options",
        ] {
            assert_eq!(
                rust_call_blocks(source, call_path).len(),
                1,
                "{call_path} must occur exactly once in each dynamic creation matrix"
            );
        }
    }
    assert_eq!(
        editable
            .matches("InterfaceSpawnOutcome::started_rnode(")
            .count(),
        5
    );
    assert!(interfaces.contains("rnode_activity_monitor: Option<PendingRNodeActivityMonitor>"));
    assert!(editable.contains("BleRnodeOperationResult::Ready {"));
    assert!(editable.contains("interface_id,\n                            monitor,"));
    assert!(editable.contains("InterfaceSpawnOutcome::started_rnode("));

    let replace = rust_function_block(&interfaces, "finish_rnode_interface_replace");
    assert_eq!(replace.matches(".activate(Arc::clone(&state))").count(), 2);
    assert!(replace.matches("finish_rnode_lifecycle_operation").count() >= 2);
    let resume = rust_function_block(&interfaces, "resume_interface");
    assert_eq!(resume.matches(".activate(Arc::clone(&st))").count(), 1);
    assert_eq!(add.matches(".activate(Arc::clone(&st))").count(), 3);
    for (source, activation_marker, finish_marker) in [
        (
            replace,
            ".activate(Arc::clone(&state))",
            "finish_rnode_lifecycle_operation(&operation_lease)",
        ),
        (
            resume,
            ".activate(Arc::clone(&st))",
            "finish_rnode_lifecycle_operation(lease)",
        ),
        (
            add,
            ".activate(Arc::clone(&st))",
            "finish_rnode_lifecycle_operation(&operation_lease)",
        ),
    ] {
        for (activation, _) in source.match_indices(activation_marker) {
            let nearby = &source[activation.saturating_sub(240)..activation];
            assert!(
                nearby.contains(finish_marker),
                "monitor activation must immediately follow lifecycle ownership"
            );
        }
    }

    let bridge = rust_function_block(&ble, "ble_rnode_bridge_ready");
    assert_eq!(
        rust_call_blocks(
            bridge,
            "rns_runtime::reticulum::spawn_ble_rnode_runtime_native_observed_with_options"
        )
        .len(),
        1
    );
    let native_contexts = rust_call_blocks(bridge, "rnode_activity_runtime_context_for_identity");
    assert_eq!(native_contexts.len(), 1);
    assert!(native_contexts[0].contains("activity_fence.identity_session_generation()"));
    assert!(bridge.contains("let rnode_activity_origin = rnode_context.origin()"));
    let native_wait = rust_call_blocks(bridge, "await_spawned_rnode_ready");
    assert_eq!(native_wait.len(), 1);
    assert!(native_wait[0].contains("rnode_activity_origin"));
    let ready_branch = bridge
        .split("Some(Ok(pending_monitor)) => {")
        .nth(1)
        .and_then(|tail| tail.split("Some(Err(failure)) => {").next())
        .expect("native BLE ready branch");
    let completion_take = ready_branch
        .find("take_initializing_ble_rnode_activity_operation_with_completion")
        .expect("native BLE terminal ownership take");
    let completion_branch = ready_branch
        .split("if let Some(completion) = completion {")
        .nth(1)
        .and_then(|tail| tail.split("} else {").next())
        .expect("completion-bearing native BLE path");
    assert!(completion_branch.contains("BleRnodeOperationResult::Ready {"));
    assert!(completion_branch.contains("monitor: pending_monitor"));
    assert!(completion_branch.contains(".is_err()"));
    assert!(completion_branch.contains("teardown_spawned_rnode_exact"));
    assert!(!completion_branch.contains(".activate("));
    let direct_branch = ready_branch
        .split("} else {")
        .nth(1)
        .expect("direct native BLE terminal path");
    let direct_activation = direct_branch
        .find("pending_monitor.activate(Arc::clone(&state_arc))")
        .expect("owned direct native BLE monitor activation");
    assert!(completion_take < direct_activation);
}

#[test]
fn android_ble_rnode_bridge_retries_writes_and_fallback_detaches() {
    let root = repo_root();
    let gatt =
        read_source(root.join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakBleGatt.kt",
        ))
        .expect("android BLE GATT bridge");

    assert!(gatt.contains("private val RNODE_DETACH_FRAME = byteArrayOf("));
    assert!(gatt.contains("0xC0.toByte(), 0x06, 0x00, 0xC0.toByte()"));
    assert!(gatt.contains("0xC0.toByte(), 0x0A, 0xFF.toByte(), 0xC0.toByte()"));
    assert!(gatt.contains("private const val BLE_WRITE_REJECT_TIMEOUT_MS"));
    assert!(gatt.contains("private fun enqueueBleWriteLocked("));
    assert!(gatt.contains("attempts++"));
    assert!(gatt.contains("Thread.sleep(BLE_WRITE_REJECT_RETRY_MS)"));
    assert!(gatt.contains("Thread.sleep(BLE_WRITE_PACING_MS)"));
    assert!(gatt.contains("observeRustDetachBytes(readBuf, off, end)"));
    assert!(gatt.contains("sendRnodeDetachFallbackIfNeeded(\"TCP bridge closing\")"));
    assert!(gatt.contains("if (rustDetachObserved.get()) return"));
}

#[test]
fn rnode_config_edit_suppresses_next_interface_reannounce() {
    let root = repo_root();
    let state_rs =
        read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("runtime state");
    let runtime_rs =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime lib");
    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");

    assert!(state_rs.contains("interface_reannounce_suppression"));
    assert!(state_rs.contains("suppress_next_interface_reannounce"));
    assert!(state_rs.contains("take_interface_reannounce_suppression"));
    assert!(state_rs.contains("INTERFACE_REANNOUNCE_SUPPRESSION_TTL"));

    assert!(runtime_rs.contains("take_interface_reannounce_suppression(name)"));
    assert!(runtime_rs.contains("!reannounce_suppressed"));
    assert!(runtime_rs.contains("PollActivityObservation::AnnounceSuppressed"));
    assert!(runtime_rs.contains("AnnounceSuppressionReason::InterfaceRestart"));

    assert!(interfaces_rs.contains("operation == \"update_lora\""));
    assert!(interfaces_rs.contains("matches!(&new_runtime, EditableInterfaceConfig::RNode"));
    assert!(interfaces_rs.contains("suppress_next_interface_reannounce(new_runtime.name())"));
}

#[test]
fn rnode_public_map_is_edit_only_and_privacy_gated() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    let rns_config_rs =
        read_source(root.join("crates/ratspeak-runtime/src/rns_config.rs")).expect("rns config");
    let manifest = read_source(root.join("src-tauri/gen/android/app/src/main/AndroidManifest.xml"))
        .expect("android manifest");
    let android_main = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("android main activity");
    let android_web_chrome = read_source(
        root.join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/generated/RustWebChromeClient.kt",
        ),
    )
    .expect("android generated web chrome client");
    let android_gradle = read_source(root.join("src-tauri/gen/android/app/build.gradle.kts"))
        .expect("android gradle");

    assert!(index.contains(r#"id="rnode-public-map-section" style="display:none;""#));
    assert!(index.contains(r#"id="rnode-public-map-enabled""#));
    assert!(index.contains(r#"class="rnode-setting-row""#));
    assert!(index.contains(r#"class="prop-toggle rnode-public-map-toggle""#));
    assert!(index.contains("Display on public map"));
    assert!(index.contains(r#"id="rnode-public-map-latitude""#));
    assert!(index.contains(r#"id="rnode-public-map-longitude""#));
    assert!(!index.contains("rnode-device-summary"));
    assert!(!index.contains("rnode-public-map-state"));
    assert!(!index.contains("id=\"rnode-public-map-help\""));
    assert!(!index.contains("id=\"rnode-public-map-toggle-wrap\""));
    assert!(!index.contains("rnode-public-map-height"));
    let advanced_idx = index
        .find(r#"id="rnode-advanced""#)
        .expect("advanced details");
    let public_map_idx = index
        .find(r#"id="rnode-public-map-section""#)
        .expect("public map section");
    let submit_idx = index
        .find(r#"id="rnode-submit-btn""#)
        .expect("submit button");
    assert!(advanced_idx < public_map_idx);
    assert!(public_map_idx < submit_idx);

    let warning = "This node's approximate location data will be broadcast publicly. The location will be your current approximate location, and only change again if you update it. Location is never live tracked.";
    assert!(modals_js.contains(warning));
    assert!(modals_js.contains("title: 'Display on public map?'"));
    assert!(modals_js.contains("confirmText: 'Enable'"));
    assert!(!modals_js.contains("_rnodeSetPublicMapState"));
    assert!(modals_js.contains("Requesting current approximate location..."));
    assert!(modals_js.contains("navigator.geolocation.getCurrentPosition"));
    assert!(modals_js.contains("_rnodeResetPublicMap();"));
    assert!(modals_js.contains("_rnodeLoadPublicMap(editIface);"));
    assert!(modals_js.contains("loraArgs.public_map = publicMapSettings"));
    assert!(modals_js.contains("Math.round(value * 1000) / 1000"));

    assert!(interfaces_rs.contains("pub public_map: Option<UpdateLoraPublicMapArgs>"));
    assert!(interfaces_rs.contains("resolve_rnode_public_map_update"));
    assert!(interfaces_rs.contains("Set an identity display name before enabling public map."));
    assert!(interfaces_rs.contains("discovery_name: Some(display_name)"));

    assert!(rns_config_rs.contains("pub struct RnodePublicMapArgs"));
    assert!(rns_config_rs.contains("discoverable = yes"));
    assert!(rns_config_rs.contains("latitude = {latitude}"));
    assert!(rns_config_rs.contains("longitude = {longitude}"));
    assert!(rns_config_rs.contains("discovery_name = {discovery_name}"));
    assert!(!rns_config_rs.contains("height = {"));

    assert!(manifest.contains(r#"android.permission.ACCESS_FINE_LOCATION" />"#));
    assert!(manifest.contains(r#"android.permission.ACCESS_COARSE_LOCATION" />"#));
    assert!(!android_main.contains("RustWebChromeClient(this)"));
    assert!(!android_main.contains("RatspeakWebChromeClient"));
    assert!(android_web_chrome.contains("override fun onGeolocationPermissionsShowPrompt("));
    assert!(android_web_chrome.contains(
        "val coarseLocationPermission = arrayOf(Manifest.permission.ACCESS_COARSE_LOCATION)"
    ));
    assert!(
        android_web_chrome
            .contains("PermissionHelper.hasPermissions(activity, coarseLocationPermission)")
    );
    assert!(android_web_chrome.contains("callback.invoke(origin, true, false)"));
    assert!(
        android_web_chrome
            .contains("onGeolocationPermissionsShowPrompt: coarse permission already granted")
    );
    assert!(
        android_gradle.contains(
            "Tauri RustWebChromeClient.kt coarse geolocation permission patch is missing"
        )
    );
}

#[test]
fn peers_sort_preference_defaults_to_last_seen_and_persists() {
    let root = repo_root();

    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    assert!(index.contains(
        r#"<button class="toolbar-dropdown-item" data-sort="name">Alphabetical</button>"#
    ));
    assert!(index.contains(
        r#"<button class="toolbar-dropdown-item active" data-sort="last_seen">Last Seen</button>"#
    ));

    let peers_js = read_source(root.join("dashboard/static/js/peers.js")).expect("peers js");
    assert!(peers_js.contains("var PEERS_SORT_DEFAULT = 'last_seen';"));
    assert!(peers_js.contains("function hydratePeersSortPreference()"));
    assert!(peers_js.contains("RS.invoke('api_app_settings')"));
    assert!(peers_js.contains("RS.invoke('set_peers_sort', { sort: peersSort })"));
    assert!(peers_js.contains("RS.listen('app_settings_updated'"));

    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    assert!(interfaces_rs.contains("const DEFAULT_PEERS_SORT: &str = \"last_seen\";"));
    assert!(interfaces_rs.contains("pub async fn set_peers_sort"));
    assert!(interfaces_rs.contains("\"peers_sort\": persisted_peers_sort(&state)"));
    assert!(interfaces_rs.contains("db::try_set_setting(&p, \"peers_sort\", &persisted)"));

    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    assert!(tauri_lib.contains("set_peers_sort"));
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
fn frontend_shared_helpers_are_adopted() {
    let root = repo_root();
    // T2-4: one sheet shell, one peer-group builder, toast-surfaced actions.
    let constants_js =
        read_source(root.join("dashboard/static/js/constants.js")).expect("constants js");
    assert!(constants_js.contains("RS.sheetShell = {"));
    assert!(constants_js.contains("RS.invokeOrToast = function"));
    assert!(constants_js.contains("RS.buildPeerGroupItems = function"));

    for file in [
        "dashboard/static/js/dialogs.js",
        "dashboard/static/js/contact_card.js",
        "dashboard/static/js/games_tab.js",
    ] {
        let source = read_source(root.join(file)).expect(file);
        assert!(
            source.contains("RS.sheetShell."),
            "{file} must use the shared sheet shell"
        );
    }
    for file in [
        "dashboard/static/js/peers.js",
        "dashboard/static/js/connections.js",
    ] {
        let source = read_source(root.join(file)).expect(file);
        assert!(
            source.contains("RS.buildPeerGroupItems("),
            "{file} must use the shared peer grouping"
        );
    }

    // User-initiated contact/block actions surface failures, never the silent
    // `RS.invoke('add_contact'...).catch(function() {})` pattern.
    for file in [
        "dashboard/static/js/lxmf.js",
        "dashboard/static/js/peers.js",
        "dashboard/static/js/health.js",
        "dashboard/static/js/contact_card.js",
    ] {
        let source = read_source(root.join(file)).expect(file);
        for action in ["add_contact", "remove_contact", "block_contact"] {
            assert!(
                !source.contains(&format!("RS.invoke('{action}'")),
                "{file}: {action} must go through RS.invokeOrToast"
            );
        }
    }

    // Portable CSS minify (BSD `sed -i ''` silently no-ops on GNU sed).
    let build_css = read_source(root.join("dashboard/build-css.sh")).expect("build-css.sh");
    assert!(!build_css.contains("sed -i ''"));
    assert!(build_css.contains("perl -0777 -pi"));

    // Peer-controlled data URL is escaped at the image render site.
    let lxmf_js = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf_js.contains("escapeHtml(msg.image.data_url)"));
}

#[test]
fn contact_list_renders_are_gated() {
    let root = repo_root();
    // T2-3: visibility gates per owning view + content-hash dedupe.
    let lxmf_js = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf_js.contains("function _gateHidden"));
    assert!(lxmf_js.contains("function _gateClean"));
    assert!(lxmf_js.contains("_gateHidden('view-message'"));
    assert!(lxmf_js.contains("_gateHidden('view-contacts'"));
    assert!(lxmf_js.contains("_gateHidden('view-dashboard'"));
    // Reactions map resets on conversation switch.
    assert!(lxmf_js.contains("_msgReactions = {};"));

    let connections_js =
        read_source(root.join("dashboard/static/js/connections.js")).expect("connections js");
    assert!(connections_js.contains("if (container._rsLastHtml === mobileHtml) return;"));

    // Message view heals gated skips on activation.
    let nav_js = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    assert!(nav_js.contains("Heal renders skipped while this view was hidden."));
}

#[test]
fn reaction_emoji_is_escaped_and_validated() {
    let root = repo_root();
    // Render site escapes the peer-controlled emoji text (T0-5).
    let lxmf_js = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf_js.contains("escapeHtml(emoji) + (count > 1"));

    // Runtime rejects markup/control characters at ingest.
    let runtime_rs = read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime");
    assert!(runtime_rs.contains("fn sanitize_reaction_emoji"));
    assert!(runtime_rs.contains("let Some(emoji) = sanitize_reaction_emoji(emoji)"));
}

#[test]
fn profile_status_frontend_contract_is_wired() {
    let root = repo_root();
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    assert!(settings_js.contains("var PROFILE_STATUS_MAX_BYTES = 50;"));
    assert!(settings_js.contains("function profileStatusFromPayload"));
    assert!(settings_js.contains("function ensureProfileStatusElements"));
    assert!(settings_js.contains("'header-mobile-status'"));
    assert!(settings_js.contains("'sidebar-identity-status'"));
    assert!(settings_js.contains("'msg-profile-status'"));
    assert!(settings_js.contains("Set a status"));
    assert!(settings_js.contains("profile_status"));
    assert!(settings_js.contains("function trimProfileStatusToByteLimit"));
    assert!(settings_js.contains("function openIdentityStatusEditor"));
    assert!(settings_js.contains("RS.invoke('set_identity_status', { status: nextStatus })"));
    assert!(settings_js.contains("counter.textContent = bytes + '/' + PROFILE_STATUS_MAX_BYTES;"));

    let lxmf_js = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf_js.contains("var statusEl = document.getElementById('msg-profile-status');"));
    assert!(lxmf_js.contains("syncActiveProfileStatusFromPayload(data);"));
    assert!(lxmf_js.contains("peer.profile_status"));
    assert!(lxmf_js.contains("profileStatus ? (activity + ' \\u00b7 ' + profileStatus)"));

    let peers_cache_js =
        read_source(root.join("dashboard/static/js/peers_cache.js")).expect("peers cache js");
    assert!(peers_cache_js.contains("function ratspeakProfileStatusText"));
    assert!(peers_cache_js.contains("profile_status: typeof r.profile_status === 'string'"));
    assert!(peers_cache_js.contains("existing.profile_status = n.profile_status"));

    let peers_js = read_source(root.join("dashboard/static/js/peers.js")).expect("peers js");
    assert!(peers_js.contains("class=\"peers-row-status\""));
    assert!(peers_js.contains("statusRowHeight"));
    assert!(peers_js.contains("_peerListMetrics"));

    let health_js = read_source(root.join("dashboard/static/js/health.js")).expect("health js");
    assert!(health_js.contains("class=\"dashboard-peers-status\""));
    assert!(health_js.contains("ratspeakProfileStatusText(p)"));

    let identity_js =
        read_source(root.join("dashboard/static/js/identity.js")).expect("identity js");
    assert!(identity_js.contains("profileStatusFromPayload(_activeIdent)"));

    let layout_css =
        read_source(root.join("dashboard/static/css/04-layout.css")).expect("layout css");
    assert!(layout_css.contains(".profile-status-text"));
    assert!(layout_css.contains(".profile-status-empty"));

    let modals_css =
        read_source(root.join("dashboard/static/css/08-modals.css")).expect("modals css");
    assert!(modals_css.contains(".profile-status-input"));
    assert!(modals_css.contains(".profile-status-counter.at-limit"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    assert!(responsive_css.contains(".header-mobile-status"));

    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    assert!(views_css.contains(".peers-row-status"));
    assert!(views_css.contains(".dashboard-peers-status"));
    assert!(views_css.contains(".dashboard-peers-row.has-profile-status"));
    assert!(!views_css.contains("calc(var(--type-row-meta-size)"));
    assert!(!views_css.contains("calc(var(--text-xs)"));
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
fn notifications_use_canonical_names_and_ignore_watched_game_unread() {
    let root = repo_root();

    let games_js = read_source(root.join("dashboard/static/js/games_tab.js")).expect("games js");
    assert!(games_js.contains("function _isViewingSession(sessionId)"));
    assert!(games_js.contains("function _markSessionReadLocal(sessionId, options)"));
    assert!(games_js.contains("_markViewedSessionRead({ render: false });"));
    assert!(
        games_js
            .contains("_markSessionReadLocal(data.session_id, { render: false, force: true });")
    );
    assert!(games_js.contains(
        "if (_allSessions[i].unread > 0 && !_isViewingSession(_allSessions[i].game_id)) total++;"
    ));

    let games_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/games.rs")).expect("games rs");
    assert!(games_rs.contains("emit_game_sessions(&state_arc, &identity_id, None).await;"));

    let lxmf_js = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf_js.contains("function _messageSourceName(msg)"));
    assert!(lxmf_js.contains("msg.source_display_name"));
    assert!(lxmf_js.contains("var fromLabel = _messageSourceName(msg);"));
    assert!(lxmf_js.contains("var notifFrom = _messageSourceName(msg);"));

    let runtime_rs =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime lib");
    assert!(runtime_rs.contains("\"source_display_name\": source_display_name"));
    assert!(runtime_rs.contains("db::get_peers_by_hashes(pool, &hashes, identity_id)"));
    assert!(
        !runtime_rs.contains("downloaded from relay"),
        "background Offline Inbox downloads must rely on per-message notifications"
    );
}

#[test]
fn games_new_sheet_uses_shared_mobile_bottom_sheet_width() {
    let root = repo_root();
    let games_js = read_source(root.join("dashboard/static/js/games_tab.js")).expect("games js");
    assert!(games_js.contains("sheetClass: 'bottom-sheet games-new-dialog'"));
    assert!(games_js.contains("rs-dialog-cancel games-sheet-cancel-btn"));
    assert!(games_js.contains("rs-dialog-confirm games-sheet-send-btn"));

    let games_css = read_source(root.join("dashboard/static/css/11-games.css")).expect("games css");
    assert!(games_css.contains(
        "@media (min-width: 769px) {\n    .bottom-sheet.open.games-new-dialog {\n        width: min(520px, 92vw);\n    }\n}"
    ));
    assert!(!games_css.contains(".games-sheet-send-btn {\n    border: 1px solid var(--accent);"));
    assert!(
        !games_css
            .contains(".games-sheet-cancel-btn {\n    border: 1px solid var(--border-control);")
    );
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
    let policy = read_source(repo_root().join("crates/ratspeak-tauri/src/diagnostics.rs"))
        .expect("diagnostics target policy");
    let core =
        read_source(repo_root().join("crates/ratspeak-tauri/src/lib.rs")).expect("tauri core");
    let ble = read_source(repo_root().join("crates/ratspeak-tauri/src/commands/ble.rs"))
        .expect("BLE commands");
    let events = read_source(repo_root().join("dashboard/static/js/tauri_events.js"))
        .expect("frontend event listeners");

    assert!(source.contains("fn diagnostics_enabled()"));
    assert!(source.contains("env_flag(\"RATSPEAK_DIAGNOSTICS\")"));
    assert!(source.contains("if !diagnostics_enabled()"));
    assert!(source.contains("fn diagnostic_file_enabled()"));
    assert!(source.contains("RATSPEAK_DIAGNOSTIC_FILE"));
    assert!(!source.contains("const DEFAULT_FILTER"));
    assert!(source.contains("fn diagnostic_metadata_allowed("));
    assert_eq!(
        source
            .matches(".with(filter_fn(diagnostic_metadata_allowed))")
            .count(),
        6,
        "every platform subscriber path must intersect EnvFilter with the immutable target policy"
    );
    assert!(policy.contains("pub fn target_allowed(target: &str) -> bool"));
    assert!(policy.contains("pub fn metadata_allowed(metadata: &tracing::Metadata<'_>) -> bool"));
    assert!(policy.contains("PROHIBITED_FIELD_NAMES"));
    for denied in ["rns_interface", "lxmf_core::router", "ble_diag"] {
        assert!(policy.contains(denied));
    }
    assert!(core.contains("spawn_ble_event_broadcaster(&app_state)"));
    assert!(!core.contains("spawn_ble_diag_broadcaster"));
    assert!(!ble.contains("subscribe_ble_diag"));
    assert!(!events.contains("RS.listen('ble_diag'"));
}

#[test]
fn semantic_ble_and_auto_activity_adapters_preserve_the_privacy_boundary() {
    let root = repo_root();
    let ble =
        read_source(root.join("crates/ratspeak-tauri/src/commands/ble.rs")).expect("BLE commands");
    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interface commands");
    let adapter =
        read_source(root.join("crates/ratspeak-tauri/src/commands/interface_activity.rs"))
            .expect("interface Activity adapter");
    let state =
        read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("runtime state");
    let events = read_source(root.join("dashboard/static/js/tauri_events.js"))
        .expect("frontend event listeners");
    let shell = read_source(root.join("src-tauri/src/lib.rs")).expect("command registration");

    assert!(adapter.contains("record_event_fenced("));
    assert!(adapter.contains("is_current_activity_origin_fence(fence)"));
    assert!(!adapter.contains("reason: String"));
    assert!(!adapter.contains("address"));
    assert!(!adapter.contains("device"));
    assert!(!adapter.contains("ifname"));

    assert!(ble.contains("ble_peer_activity_transition("));
    assert!(ble.contains("ble_rnode_activity_transition("));
    assert!(ble.contains("InterfaceDegradationReason::PeripheralUnavailable"));
    assert!(ble.contains("let mut peripheral_degradation_recorded = false;"));
    assert!(ble.contains("activity_fence: ActivityRequestFence"));
    assert!(!ble.contains("subscribe_ble_diag"));
    assert!(!ble.contains(".record_event("));

    let auto_relay = interfaces
        .split("pub fn spawn_auto_event_broadcaster")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub async fn api_list_network_interfaces")
                .next()
        })
        .expect("AutoInterface product relay");
    assert!(auto_relay.contains("AutoInterfaceEvent::JoinFailed"));
    assert!(auto_relay.contains("AutoInterfaceEvent::CarrierState"));
    assert!(auto_relay.contains("emit_to_all("));
    assert!(!auto_relay.contains("record_interface_activity"));
    assert!(!auto_relay.contains("activity.record"));

    assert!(state.contains("rns_crypto::random::random_16()"));
    assert!(state.contains("claim_ble_rnode_activity_operation"));
    assert!(state.contains("take_pending_ble_rnode_activity_operation"));
    assert!(state.contains("take_initializing_ble_rnode_activity_operation"));
    assert!(state.contains("BleRnodeActivityOperationPhase::Initializing"));
    assert!(state.contains("BleRnodeActivityOperationPhase::Completing"));
    assert!(state.contains("rollback_context: Option<BleRnodeRollbackContext>"));
    assert!(state.contains("pending.take().map(|operation| {"));
    assert!(state.contains("*pending = None;"));
    assert!(shell.contains("commands::ble::ble_rnode_bridge_failed"));

    let native_failure = events
        .split("RS.invoke('ble_rnode_bridge_failed'")
        .nth(1)
        .and_then(|tail| tail.split("var errRaw").next())
        .expect("typed native bridge failure callback");
    assert!(native_failure.contains("activity_operation"));
    assert!(native_failure.contains("failure_code: nativeFailureCode"));
    assert!(!native_failure.contains("result.error"));
    assert!(!native_failure.contains("cancel_ble_connect"));

    let bridge_failure = ble
        .split("pub async fn ble_rnode_bridge_failed")
        .nth(1)
        .and_then(|tail| tail.split("pub async fn cancel_ble_connect").next())
        .expect("typed native bridge failure command");
    let token_accept = bridge_failure
        .find("take_pending_ble_rnode_activity_operation")
        .expect("exact operation acceptance");
    let rollback = bridge_failure
        .find("rollback_ble_rnode_context")
        .expect("backend-owned failure rollback");
    assert!(
        token_accept < rollback,
        "a stale rejected failure must not reach config rollback"
    );

    for product_event in [
        "ble_peer_discovered",
        "ble_peer_connected",
        "ble_peer_disconnected",
        "ble_peer_peripheral_unavailable",
        "auto_unavailable",
        "auto_carrier_state",
        "ble_rnode_passkey_prompt",
        "ble_rnode_pairing_finished",
        "ble_rnode_connect_native",
        "ble_rnode_disconnect_native",
    ] {
        assert!(
            ble.contains(product_event)
                || interfaces.contains(product_event)
                || events.contains(product_event),
            "dedicated product stream {product_event} must remain"
        );
    }
}

#[test]
fn android_ble_operation_nonce_is_round_tripped_scoped_and_watchdog_owned() {
    let root = repo_root();
    let state =
        read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("runtime state");
    let ble =
        read_source(root.join("crates/ratspeak-tauri/src/commands/ble.rs")).expect("BLE commands");
    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interface commands");
    let events = read_source(root.join("dashboard/static/js/tauri_events.js"))
        .expect("frontend event listeners");
    let main_activity = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("Android MainActivity");
    let gatt =
        read_source(root.join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakBleGatt.kt",
        ))
        .expect("Android GATT bridge");

    assert!(
        state.contains("BLE_RNODE_ACTIVITY_OPERATION_TTL: Duration = Duration::from_secs(240)")
    );
    assert!(state.contains("BleRnodeActivityOperationPhase::PendingNative"));
    assert!(state.contains("BleRnodeActivityOperationPhase::Initializing"));
    assert!(state.contains("BleRnodeActivityOperationPhase::Completing"));
    assert!(state.contains("claim_ble_rnode_activity_operation"));
    assert!(state.contains("claim_ble_rnode_activity_operation_completion"));
    assert!(state.contains("take_pending_ble_rnode_activity_operation"));
    assert!(state.contains("take_initializing_ble_rnode_activity_operation"));
    assert!(state.contains("take_completing_ble_rnode_activity_operation"));
    assert!(state.contains("rollback_context: Option<BleRnodeRollbackContext>"));
    assert!(state.contains("lifecycle_lease: Option<RNodeLifecycleOperationLease>"));
    assert!(state.contains("begin_ble_rnode_activity_operation_owned"));
    assert!(state.contains("begin_ble_rnode_activity_operation_with_completion_owned"));
    assert!(state.contains("invalidate_ble_rnode_activity_operation_if_token"));
    assert!(state.contains("ble_completion_ownership_is_lost_when_new_operation_replaces_it"));
    assert!(state.contains("stale_ble_failure_cannot_take_newer_rollback_context"));

    assert!(ble.contains("claim_ble_rnode_activity_operation(&activity_operation)"));
    assert!(ble.contains("take_initializing_ble_rnode_activity_operation"));
    assert!(ble.contains("claim_ble_rnode_activity_operation_completion"));
    assert!(ble.contains("take_completing_ble_rnode_activity_operation"));
    assert!(ble.contains("disconnect_native_ble_rnode_operation"));
    assert!(ble.contains("json!({ \"activity_operation\": activity_operation })"));
    assert_eq!(
        rust_call_blocks(
            &ble,
            "rns_runtime::reticulum::spawn_ble_rnode_runtime_native_observed_with_options",
        )
        .len(),
        1
    );
    let readiness_calls = rust_call_blocks(&ble, "await_spawned_rnode_ready");
    assert_eq!(readiness_calls.len(), 1);
    assert!(readiness_calls[0].contains("rnode_activity_origin"));
    assert!(ble.contains("teardown_spawned_rnode_exact(&rns, &spawned)"));
    assert!(!ble.contains("online.load(std::sync::atomic::Ordering::SeqCst)"));

    let mut native_emissions = 0usize;
    let mut remainder = interfaces.as_str();
    while let Some(index) = remainder.find("\"ble_rnode_connect_native\"") {
        native_emissions += 1;
        let tail = &remainder[index..];
        let nearby = &tail[..tail.len().min(1600)];
        assert!(
            nearby.contains("\"activity_operation\": activity_operation"),
            "every Android BLE reconnect/resume emission must carry its operation token"
        );
        remainder = &tail["\"ble_rnode_connect_native\"".len()..];
    }
    assert!(
        native_emissions >= 2,
        "add and reconnect/resume paths must both emit native BLE work"
    );
    assert!(interfaces.contains("schedule_android_ble_rnode_operation_watchdog"));
    assert!(interfaces.contains("couple_android_ble_operation_to_rnode_lease"));
    assert!(interfaces.contains("begin_ble_rnode_activity_operation_owned"));
    assert!(interfaces.contains("begin_ble_rnode_activity_operation_with_completion_owned"));
    assert!(interfaces.contains("Duration::from_secs(180)"));
    assert!(interfaces.contains("take_pending_ble_rnode_activity_operation"));
    assert!(interfaces.contains("rollback_fresh_lora_add_marker"));
    assert!(interfaces.contains("RnodeActivityOutcome::SetupTimedOut"));
    assert!(interfaces.contains("Some(\"setup_timeout\")"));

    assert!(events.contains("result.activity_operation !== activityOperation"));
    assert!(events.contains("progress.activity_operation !== activityOperation"));
    assert!(events.contains("_BLE_RNODE_NATIVE_TIMEOUT_MS = 180000"));
    assert!(events.contains("disconnectBleDeviceForOperation(activityOperation)"));
    assert!(events.contains("data.tcp_port,\n            activityOperation"));
    assert!(events.contains("failure_code: 'setup_timeout'"));
    assert!(!events.contains("rollbackOnly"));
    assert!(!events.contains("cancel_ble_connect"));

    let bridge_ready = ble
        .split("pub async fn ble_rnode_bridge_ready")
        .nth(1)
        .and_then(|tail| tail.split("pub struct BleRnodeBridgeFailureArgs").next())
        .expect("Android BLE bridge-ready command");
    let readiness_failure = bridge_ready
        .split("Some(Err(failure)) => {")
        .nth(1)
        .and_then(|tail| tail.split("None => {").next())
        .expect("RNode readiness failure completion");
    let completion_claim = readiness_failure
        .find("claim_ble_rnode_activity_operation_completion")
        .expect("initialization completion claim");
    let teardown = readiness_failure
        .find("teardown_spawned_rnode_exact")
        .expect("exact readiness-failure teardown");
    let completion_take = readiness_failure
        .find("take_completing_ble_rnode_activity_operation")
        .expect("exact completion take");
    assert!(completion_claim < teardown && teardown < completion_take);
    assert!(bridge_ready.contains("clear_ble_rnode_rollback_context"));

    assert!(main_activity.contains(
        "fun connectBleDevice(address: String, localPort: Int, activityOperation: String)"
    ));
    assert!(main_activity.contains(".put(\"activity_operation\", activityOperation)"));
    assert!(
        main_activity.contains("fun disconnectBleDeviceForOperation(activityOperation: String)")
    );
    assert!(main_activity.contains("if (bleConnectOperation != activityOperation)"));
    assert!(main_activity.contains(".put(\"failure_code\", \"connect_failed\")"));
    assert!(main_activity.contains("error.contains(RatspeakBleGatt.ERR_BOND_TIMEOUT)"));
    assert!(main_activity.contains("\"bond_timeout\""));
    assert!(
        main_activity
            .contains("window.RatspeakAndroid.disconnectBleDeviceForOperation($operationJson)")
    );
    assert!(gatt.contains("fun attachWebView(webView: WebView?, operation: String)"));
    assert!(gatt.contains(".put(\"activity_operation\", activityOperation)"));
    assert!(gatt.contains("const val ERR_BOND_TIMEOUT = \"ERR_BOND_TIMEOUT\""));
    assert!(gatt.contains("$ERR_PAIRING_MODE $ERR_BOND_TIMEOUT Bonding timed out"));
    assert!(ble.contains("enum BleRnodeNativeFailureCode"));
    assert!(ble.contains("BondTimeout"));
    assert!(ble.contains("SetupTimeout"));
    assert!(!ble.contains("pub timed_out: bool"));
}

#[test]
fn interface_command_lifecycles_use_origin_fences_truthful_terminals_and_scoped_auto_events() {
    let root = repo_root();
    let ble =
        read_source(root.join("crates/ratspeak-tauri/src/commands/ble.rs")).expect("BLE commands");
    let shared =
        read_source(root.join("crates/ratspeak-tauri/src/commands/shared.rs")).expect("shared");
    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interface commands");
    let rns_config =
        read_source(root.join("crates/ratspeak-runtime/src/rns_config.rs")).expect("rns config");

    let expiry = ble
        .split("fn schedule_ble_peer_expiry")
        .nth(1)
        .and_then(|tail| tail.split("#[tauri::command]").next())
        .expect("BLE Peer expiry scheduler");
    assert!(expiry.contains("activity_fence: ActivityRequestFence"));
    assert!(!expiry.contains("activity_request_fence()"));
    assert!(
        expiry
            .contains("let changed = disable_ble_peer_inner_if_expiry(&state3, expires_at).await")
    );
    assert!(ble.contains("InterfaceTimeoutReason::Setup"));
    assert!(ble.contains("if changed {\n            record_interface_activity"));
    assert!(shared.contains("pub(crate) async fn disable_ble_peer_inner"));
    assert!(shared.contains("pub(crate) async fn disable_ble_peer_inner_if_expiry"));
    assert!(shared.contains("was_requested || had_live_interface"));
    assert!(rns_config.contains("pub enum RemoveInterfaceOutcome"));
    assert!(rns_config.contains("pub fn remove_interface_checked"));
    assert!(rns_config.contains("if !removed {"));
    assert!(rns_config.contains("RemoveInterfaceOutcome::NotFound"));

    let cancel_ble = ble
        .split("pub async fn cancel_ble_connect")
        .nth(1)
        .and_then(|tail| tail.split("pub async fn disconnect_ble_rnode").next())
        .expect("BLE cancellation command");
    assert!(!cancel_ble.contains("rollback_only"));
    let cancellation_lease = cancel_ble
        .find("state_arc.begin_ble_rnode_activity_cancellation()")
        .expect("user cancellation lease");
    let exact_rollback = cancel_ble
        .find("rollback_ble_rnode_context")
        .expect("exact Android cancellation rollback");
    assert!(
        !cancel_ble.contains("rollback_current_fresh_lora_add"),
        "cancellation without an exact operation owner must not delete a same-name replacement"
    );
    let teardown_spawn = cancel_ble
        .find("tokio::spawn")
        .expect("awaited runtime teardown spawn");
    let teardown_claim = cancel_ble
        .find("claim_ble_rnode_activity_cancellation")
        .expect("exact cancellation teardown claim");
    let runtime_teardown = cancel_ble
        .find("teardown_ble_rnode_interface")
        .expect("exact-id runtime teardown");
    let terminal_take = cancel_ble
        .find("take_completing_ble_rnode_activity_cancellation")
        .expect("post-await cancellation terminal take");
    let terminal_status = terminal_take
        + cancel_ble[terminal_take..]
            .find("BLE connect for")
            .expect("post-teardown cancellation terminal status");
    assert!(
        cancellation_lease < exact_rollback
            && exact_rollback < teardown_spawn
            && teardown_spawn < teardown_claim
            && teardown_claim < runtime_teardown
            && runtime_teardown < terminal_take
            && terminal_take < terminal_status,
        "cancellation must own exact rollback, teardown, and terminal publication"
    );
    assert!(shared.contains("const FRESH_LORA_ADD_TTL"));
    assert!(shared.contains("const MAX_FRESH_LORA_ADDS"));
    assert!(shared.contains("type FreshLoraAddKey = (PathBuf, String)"));
    assert!(shared.contains("struct FreshLoraAddEntry"));
    assert!(shared.contains("entry.marker == expected_marker"));
    assert!(shared.contains("rollback_fresh_lora_add_marker"));
    assert!(!shared.contains("rollback_current_fresh_lora_add"));
    assert!(shared.contains("fresh_lora_success_and_edit_clear_delete_authorization"));
    assert!(shared.contains("stale_same_name_marker_cannot_delete_replacement_config"));
    let atomic_rollback = shared
        .split("pub(crate) fn rollback_fresh_lora_add_marker")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) fn remove_stored_file_refs").next())
        .expect("atomic versioned fresh-add rollback");
    assert!(atomic_rollback.contains("with_rns_config_lock"));
    assert!(atomic_rollback.contains("take_fresh_lora_add"));
    assert!(atomic_rollback.contains("remove_interface_checked"));

    let disconnect_ble = ble
        .split("pub async fn disconnect_ble_rnode")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("BLE RNode disconnect command");
    assert!(disconnect_ble.contains("with_rns_config_lock"));
    assert!(disconnect_ble.contains("begin_rnode_lifecycle_operation"));
    assert!(disconnect_ble.contains("snapshot_interface_block"));
    assert!(disconnect_ble.contains("remove_interface_block_if_revision"));
    assert!(disconnect_ble.contains("captured_interface_id"));
    assert!(disconnect_ble.contains("is_current_rnode_lifecycle_operation"));
    assert!(disconnect_ble.contains("RemoveInterfaceOutcome::Removed"));
    assert!(disconnect_ble.contains("RemoveInterfaceOutcome::NotFound"));
    assert!(disconnect_ble.contains("RemoveInterfaceOutcome::WriteFailed"));

    let add_lora = interfaces
        .split("pub async fn add_lora_interface")
        .nth(1)
        .and_then(|tail| tail.split("pub struct UpdateLoraArgs").next())
        .expect("add_lora command");
    for outcome in [
        "RnodeActivityOutcome::Configured",
        "RnodeActivityOutcome::Connecting",
        "RnodeActivityOutcome::Online",
        "RnodeActivityOutcome::ConfigureFailed",
        "RnodeActivityOutcome::ConnectFailed",
        "RnodeActivityOutcome::PairingTimedOut",
        "RnodeActivityOutcome::StartupTimedOut",
        "RnodeActivityOutcome::RuntimeFailed",
    ] {
        assert!(
            add_lora.contains(outcome),
            "add_lora transport matrix is missing {outcome}"
        );
    }
    assert_eq!(
        rust_call_blocks(
            add_lora,
            "rns_runtime::reticulum::spawn_ble_rnode_runtime_observed_with_options",
        )
        .len(),
        1
    );
    assert_eq!(
        rust_call_blocks(
            add_lora,
            "rns_runtime::reticulum::spawn_rnode_runtime_observed_with_options",
        )
        .len(),
        1
    );
    assert_eq!(
        rust_call_blocks(
            add_lora,
            "rns_runtime::reticulum::spawn_android_usb_rnode_runtime_observed_with_options",
        )
        .len(),
        1
    );
    assert!(add_lora.contains("await_owned_rnode_ready"));
    assert!(!add_lora.contains("online.load(std::sync::atomic::Ordering::SeqCst)"));
    assert!(interfaces.contains("teardown_spawned_rnode_exact(handle, spawned)"));
    let freshness_transaction = add_lora
        .split(
            "let (operation_lease, fresh_marker, existing_rnode_port, handoff_targets, config_written)",
        )
        .nth(1)
        .and_then(|tail| tail.split("let fresh_add = fresh_marker.is_some()").next())
        .expect("fresh BLE add config transaction");
    assert!(freshness_transaction.contains("with_rns_config_lock"));
    assert!(freshness_transaction.contains("begin_rnode_lifecycle_operation"));
    let stale_clear = freshness_transaction
        .find("mark_lora_add_freshness(&config_dir, &name, false)")
        .expect("stale marker clear");
    let config_write = freshness_transaction
        .find("crate::rns_config::add_rnode_interface")
        .expect("RNode config write");
    let marker_install = freshness_transaction
        .rfind("mark_lora_add_freshness(&config_dir, &name, fresh_add)")
        .expect("versioned marker install");
    assert!(stale_clear < config_write && config_write < marker_install);
    assert!(add_lora.contains("clear_fresh_lora_add_marker"));
    assert!(add_lora.contains("rollback_fresh_lora_add_marker"));

    let update_lora = interfaces
        .split("pub async fn update_lora_interface")
        .nth(1)
        .and_then(|tail| {
            tail.split("async fn teardown_rnode_handoff_broadcast")
                .next()
        })
        .expect("update_lora command");
    assert!(update_lora.contains("mark_lora_add_freshness"));
    assert!(update_lora.contains("&old_name"));
    assert!(update_lora.contains("&name"));

    let handoff = interfaces
        .split("async fn teardown_rnode_handoff_broadcast")
        .nth(1)
        .and_then(|tail| tail.split("pub async fn remove_lora_interface").next())
        .expect("Android RNode handoff");
    assert!(handoff.contains("teardown_live_interface_by_name"));
    assert!(handoff.contains("is_current_rnode_lifecycle_operation"));
    assert!(handoff.contains("remove_interface_block_if_revision"));
    assert!(handoff.contains("InterfaceBlockCasOutcome::NotFound => {}"));
    assert!(handoff.contains("InterfaceBlockCasOutcome::WriteFailed"));
    assert!(handoff.contains("return false;"));

    let remove_lora = interfaces
        .split("pub async fn remove_lora_interface")
        .nth(1)
        .and_then(|tail| tail.split("pub async fn enable_auto_interface").next())
        .expect("RNode removal command");
    assert!(remove_lora.contains("with_rns_config_lock"));
    assert!(remove_lora.contains("begin_rnode_lifecycle_operation"));
    assert!(remove_lora.contains("snapshot_interface_block"));
    assert!(remove_lora.contains("remove_interface_block_if_revision"));
    assert!(remove_lora.contains("teardown_live_interface_by_name"));
    assert!(remove_lora.contains("InterfaceBlockCasOutcome::Applied"));
    assert!(remove_lora.contains("InterfaceBlockCasOutcome::NotFound"));
    assert!(remove_lora.contains("InterfaceBlockCasOutcome::WriteFailed"));

    let enable_auto = interfaces
        .split("pub async fn enable_auto_interface")
        .nth(1)
        .and_then(|tail| tail.split("pub async fn disable_auto_interface").next())
        .expect("enable_auto command");
    let subscribe = enable_auto
        .find("let mut initial_auto_events = rns_interface::auto::subscribe_auto_events()")
        .expect("command-scoped Auto subscriber");
    let spawn = enable_auto
        .find("spawn_auto_interface_runtime_with_config")
        .expect("Auto spawn");
    assert!(
        subscribe < spawn,
        "Auto must subscribe before its owned spawn"
    );
    assert!(enable_auto.contains("drain_initial_auto_join_failure"));
    assert!(enable_auto.contains("AutoActivityOutcome::MulticastUnavailable"));
    assert!(enable_auto.contains("AutoActivityOutcome::TimedOut"));
}

#[test]
fn diagnostic_file_writer_is_bounded_nonblocking_and_lifetime_scoped() {
    let root = repo_root();
    let shell = read_source(root.join("src-tauri/src/lib.rs")).expect("app shell");
    let writer = read_source(root.join("crates/ratspeak-tauri/src/diagnostic_writer.rs"))
        .expect("bounded diagnostic writer");

    assert!(!shell.contains("tracing_appender::rolling::daily"));
    assert!(!shell.contains("PathBuf::from(\".\")"));
    assert!(shell.contains("diagnostic_writer::DiagnosticFileRuntime::start("));
    assert!(shell.contains("let mut tracing_guard = init_tracing();"));
    assert!(
        shell.contains("file: Option<ratspeak_tauri::diagnostic_writer::DiagnosticFileRuntime>")
    );
    assert!(shell.contains("app.manage(dropped);"));
    assert!(shell.contains("tracing_guard.shutdown();"));

    for contract in [
        "pub const ACTIVE_LOG_NAME: &str = \"ratspeak.log\";",
        "pub const ARCHIVE_COUNT: usize = 4;",
        "pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;",
        "pub const MAX_RECORD_BYTES: usize = 16 * 1024;",
        "pub const WRITER_QUEUE_RECORDS: usize = 2_048;",
        "mpsc::sync_channel(queue_records)",
        "try_send(WorkerMessage::Record(record))",
        "WorkerMessage::Shutdown",
        "pub fn dropped_counter(&self) -> DroppedLogLines",
        "pub fn shutdown(mut self) -> io::Result<()>",
        "metadata.file_type().is_symlink()",
        "metadata_is_reparse_point",
        "options.create_new(create_new)",
        "options.custom_flags(libc::O_NOFOLLOW)",
        "options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)",
    ] {
        assert!(
            writer.contains(contract),
            "missing writer contract {contract:?}"
        );
    }
    assert!(!writer.contains("read_dir("));
    assert!(!writer.contains("glob("));
}

#[test]
fn process_diagnostics_sources_do_not_reintroduce_sensitive_trace_fields() {
    let root = repo_root();
    let mut files = Vec::new();
    collect_files(&root.join("crates"), &mut files);
    collect_files(&root.join("src-tauri/src"), &mut files);

    let forbidden = [
        "app_id = %",
        "backup = %",
        "command = %",
        "content = %",
        "endpoint = %",
        "error = %",
        "error = ?",
        "err = %",
        "err = ?",
        "event = %",
        "fallback = %",
        "file_name = %",
        "greeting = %",
        "interface = %",
        "label = %",
        "nickname = %",
        "path = %",
        "payload = %",
        "response = ?",
        "secret = %",
        "session_id = %",
        "stored = %",
        "stored_name = %",
        "title = %",
        "token = %",
        "topic = %",
        "uri = %",
        "url = %",
    ];

    for path in files.into_iter().filter(|path| {
        path.extension().and_then(|extension| extension.to_str()) == Some("rs")
            && !path
                .components()
                .any(|component| component.as_os_str() == "tests")
    }) {
        let source = read_source(&path).expect("Rust source");
        for pattern in forbidden {
            assert!(
                !source.contains(pattern),
                "{} contains prohibited diagnostics field pattern {pattern:?}",
                path.display()
            );
        }
        for shorthand in [
            "tracing::trace!(%hash",
            "tracing::debug!(%hash",
            "tracing::info!(%hash",
            "tracing::warn!(%hash",
            "tracing::error!(%hash",
            "tracing::trace!(%dest_hash",
            "tracing::debug!(%dest_hash",
            "tracing::info!(%dest_hash",
            "tracing::warn!(%dest_hash",
            "tracing::error!(%dest_hash",
        ] {
            assert!(
                !source.contains(shorthand),
                "{} contains unreviewed full-identifier shorthand {shorthand:?}",
                path.display()
            );
        }
    }
}

#[test]
fn linux_wayland_webkit_startup_keeps_blank_window_workaround() {
    let source = read_source(repo_root().join("src-tauri/src/lib.rs")).expect("app shell");

    assert!(source.contains("fn apply_linux_webkit_rendering_workarounds()"));
    assert!(source.contains("WEBKIT_DISABLE_DMABUF_RENDERER"));
    assert!(source.contains("RATSPEAK_DISABLE_WEBKIT_DMABUF_WORKAROUND"));

    // WEBKIT_DISABLE_DMABUF_RENDERER only selects the legacy renderer on
    // WebKitGTK < 2.46; on 2.46+ it disables hardware acceleration outright
    // (gray windows on smithay compositors / NixOS). The workaround must stay
    // version-gated on the runtime WebKit.
    assert!(source.contains("fn should_disable_webkit_dmabuf("));
    assert!(source.contains("webkit_get_major_version"));
    assert!(source.contains("webkit_version < (2, 46)"));

    // Session detection lives in the shared window_prefs module.
    let prefs = read_source(repo_root().join("crates/ratspeak-tauri/src/window_prefs.rs"))
        .expect("window prefs");
    assert!(prefs.contains("WAYLAND_DISPLAY"));
    assert!(prefs.contains("XDG_SESSION_TYPE"));

    let workaround_pos = source
        .find("let linux_webkit_dmabuf_workaround = apply_linux_webkit_rendering_workarounds();")
        .expect("workaround applied at process startup");
    let tracing_pos = source
        .find("let mut tracing_guard = init_tracing();")
        .expect("tracing initialization");
    let builder_pos = source
        .find("tauri::Builder::default()")
        .expect("tauri builder construction");
    let build_pos = source
        .find(".build(tauri::generate_context!())")
        .expect("tauri app build");
    let run_pos = source.find("app.run(").expect("tauri app run");
    assert!(
        workaround_pos < builder_pos
            && builder_pos < build_pos
            && build_pos < tracing_pos
            && tracing_pos < run_pos,
        "apply the WebKit environment workaround before build, but initialize file tracing only after single-instance build and before run"
    );

    // --webview-diag must exit before any webview/env mutation side effects.
    let diag_pos = source
        .find("\"--webview-diag\"")
        .expect("webview diagnostics flag");
    assert!(
        diag_pos < workaround_pos,
        "--webview-diag must be handled before the workaround/builder run"
    );
}

#[test]
fn linux_window_decorations_preference_is_wired_end_to_end() {
    let root = repo_root();
    let shell = read_source(root.join("src-tauri/src/lib.rs")).expect("app shell");
    let prefs =
        read_source(root.join("crates/ratspeak-tauri/src/window_prefs.rs")).expect("window prefs");
    let commands = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let index = read_source(root.join("dashboard/index.html")).expect("index html");

    // Resolver: explicit on/off override; auto only reacts to tiling Wayland.
    assert!(prefs.contains("fn resolve_window_decorations"));
    assert!(prefs.contains("SWAYSOCK"));
    assert!(prefs.contains("NIRI_SOCKET"));
    assert!(prefs.contains("HYPRLAND_INSTANCE_SIGNATURE"));

    // Shell: preference read before the window is built, applied at builder
    // time, and adjustable live via the command.
    assert!(shell.contains("window_decorations"));
    assert!(shell.contains(".decorations(decorations)"));
    assert!(shell.contains("fn set_window_decorations("));
    assert!(shell.contains("set_window_decorations,"));

    // Devtools must be reachable in release builds for field diagnostics.
    assert!(shell.contains(".devtools(diagnostics_enabled() || developer_mode)"));

    // Settings surface: payload field + frontend control.
    assert!(commands.contains("\"window_decorations\": window_decorations"));
    assert!(settings_js.contains("set_window_decorations"));
    assert!(settings_js.contains("initWindowDecorationsToggle"));
    assert!(index.contains("settings-window-decorations-auto"));
    assert!(index.contains("settings-window-decorations-off"));
}

#[test]
fn modal_action_footers_use_shared_dialog_buttons() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    let identity_js =
        read_source(root.join("dashboard/static/js/identity.js")).expect("identity js");
    let modals_css =
        read_source(root.join("dashboard/static/css/08-modals.css")).expect("modals css");

    let identity_modal = index
        .split(r#"id="identity-modal""#)
        .nth(1)
        .and_then(|tail| tail.split(r#"id="identity-file-input""#).next())
        .expect("identity modal markup");
    assert!(identity_modal.contains(r#"class="bottom-sheet-footer""#));
    assert!(identity_modal.contains(r#"class="rs-dialog-cancel" id="identity-modal-cancel""#));
    assert!(identity_modal.contains(r#"class="rs-dialog-confirm" id="identity-modal-confirm""#));
    assert!(!identity_modal.contains("u-flex gap-4"));
    assert!(!identity_modal.contains("nr-btn flex-1"));
    assert!(!identity_modal.contains("nr-btn nr-btn-ghost flex-1"));

    assert!(identity_js.contains("var confirmClasses = 'rs-dialog-confirm';"));
    assert!(identity_js.contains("confirmClasses += ' rs-dialog-danger';"));
    assert!(!identity_js.contains("confirmBtn.className = confirmClass || 'nr-btn'"));

    assert!(modals_css.contains(".bottom-sheet-footer {\n    display: flex;\n    justify-content: flex-end;\n    flex-wrap: wrap;"));
    assert!(modals_css.contains("min-width: 96px;"));
    assert!(modals_css.contains(".rs-dialog-cancel:disabled,"));
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
        let mut source = read_source(&path).expect("source file");
        // Normalize separators so the carve-out below matches on Windows too.
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string()
            .replace('\\', "/");

        // Sole sanctioned stderr exception: `--webview-diag` prints the
        // rendering environment by design — it must work without the tracing
        // opt-in, before any subscriber exists. Everything else stays on
        // tracing. The excised region must still never touch stdout.
        if rel.ends_with("src-tauri/src/lib.rs") {
            let start = source
                .find("fn print_webview_diagnostics()")
                .expect("webview diagnostics printer present");
            let end = source[start..]
                .find("\nfn ")
                .map(|offset| start + offset)
                .expect("function following the diagnostics printer");
            // `eprintln!(` contains the `println!(` substring; strip stderr
            // prints first so only genuine stdout prints can trip this.
            let diag_without_stderr = source[start..end].replace("eprintln!(", "");
            assert!(
                !diag_without_stderr.contains("println!("),
                "print_webview_diagnostics must write to stderr, not stdout"
            );
            source.replace_range(start..end, "");
        }

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
fn ble_peer_network_rows_are_identity_deduped() {
    let root = repo_root();
    let health_js = read_source(root.join("dashboard/static/js/health.js")).expect("health js");
    assert!(health_js.contains("function _bleVisiblePeersFromCache()"));
    assert!(health_js.contains("var byIdentity = {};"));
    assert!(health_js.contains("peer.addresses = group.addresses.slice();"));
    assert!(health_js.contains("window._bleVisiblePeersFromCache = _bleVisiblePeersFromCache;"));
    assert!(health_js.contains("data-peer-addresses"));

    let tauri_events =
        read_source(root.join("dashboard/static/js/tauri_events.js")).expect("tauri events js");
    assert!(tauri_events.contains("return window._bleVisiblePeersFromCache().length;"));
    assert!(tauri_events.contains("peerCount === 0 && typeof window._blePeerCount === 'number'"));

    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    assert!(modals_js.contains("getAttribute('data-peer-addresses')"));
    assert!(modals_js.contains("addresses.forEach(function(address)"));
}

#[test]
fn ble_peer_requested_state_survives_restart_when_valid() {
    let root = repo_root();
    let tauri_lib =
        read_source(root.join("crates/ratspeak-tauri/src/lib.rs")).expect("tauri lib source");
    assert!(!tauri_lib.contains("Bluetooth Peer is never auto-restored"));
    assert!(tauri_lib.contains("commands::ble::restore_ble_peer_if_requested(init_state).await"));

    let ble_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/ble.rs")).expect("ble source");
    assert!(ble_rs.contains("const BLE_PEER_EXPIRES_AT_SETTING"));
    assert!(ble_rs.contains("pub(crate) async fn restore_ble_peer_if_requested"));
    assert!(ble_rs.contains("let _enable_guard = state_arc.ble_peer_enable_lock.lock().await;"));
    assert!(ble_rs.contains("async fn live_ble_peer_interface_id"));
    assert!(ble_rs.contains("Bluetooth Peer already enabled"));
    assert!(ble_rs.contains("let activity_fence = state.activity_request_fence();"));
    assert!(
        ble_rs.contains(
            "spawn_enable_ble_peer_task(state, activity_fence, duration_secs, expires_at);"
        )
    );
    assert!(ble_rs.contains("const BLE_RECENT_DISCONNECTS_V2_SETTING"));
    assert!(ble_rs.contains("ble_recent_disconnect_seed_addresses"));
    assert!(ble_rs.contains("update_ble_recent_disconnect_records"));
    assert!(ble_rs.contains("seed_addresses"));
    assert!(ble_rs.contains("PeerState::Starting"));
    assert!(ble_rs.contains("emit_ble_peer_enabled_status"));
    assert!(ble_rs.contains("emit_logical_ble_peer_status"));

    let state_rs =
        read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("state source");
    assert!(state_rs.contains("pub ble_peer_enable_lock: tokio::sync::Mutex<()>"));

    let shared_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/shared.rs"))
        .expect("shared source");
    assert!(shared_rs.contains("current_expires_at == expected_expires_at"));
    assert!(shared_rs.contains("ble_peer_expiry_only_disables_the_exact_requested_generation"));
    assert!(shared_rs.contains("db::set_setting(&p, \"ble_peer_expires_at\", \"0\");"));
    assert!(shared_rs.contains("\"ble_peer_status_changed\""));

    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces source");
    assert!(interfaces_rs.contains("\"state\": peer_state"));
    assert!(interfaces_rs.contains("\"peer_count\": peer_count"));
    assert!(interfaces_rs.contains("fn android_ble_peer_availability_payload"));
    assert!(interfaces_rs.contains("android_ble_peer_availability_json"));
    assert!(interfaces_rs.contains("\"probe_failed\": true"));
    assert!(interfaces_rs.contains("permission_required"));
    assert!(interfaces_rs.contains(
        "#[cfg(all(feature = \"ble\", target_os = \"android\"))]\n    return Ok(android_ble_peer_availability_payload());"
    ));

    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    assert!(settings_js.contains("window._blePeerState = data.state"));
    assert!(settings_js.contains("window._blePeerCount = data.peer_count"));

    let android_availability = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakBleAvailability.kt",
    ))
    .expect("android BLE availability source");
    assert!(android_availability.contains("object RatspeakBleAvailability"));
    assert!(android_availability.contains("BLUETOOTH_SCAN"));
    assert!(android_availability.contains("BLUETOOTH_CONNECT"));
    assert!(android_availability.contains("BLUETOOTH_ADVERTISE"));
    assert!(android_availability.contains("bluetoothLeScanner"));
    assert!(android_availability.contains("probe_failed"));
    assert!(android_availability.contains("permission_required"));

    let android_activity = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("android main activity");
    assert!(!android_activity.contains("fun startBlePeerMode"));
    assert!(!android_activity.contains("fun stopBlePeerMode"));
    assert!(!android_activity.contains("fun connectToBlePeer"));
    assert!(!android_activity.contains("fun disconnectBlePeer"));
    assert!(!android_activity.contains("fun scanForBlePeers"));

    let proguard = read_source(root.join("src-tauri/gen/android/app/proguard-rules.pro"))
        .expect("android proguard rules");
    assert!(proguard.contains("-keep class org.ratspeak.android.RatspeakBleAvailability"));
}

#[test]
fn android_ble_gatt_close_targets_captured_connection() {
    let root = repo_root();
    let gatt =
        read_source(root.join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakBleGatt.kt",
        ))
        .expect("android BLE GATT source");

    assert!(gatt.contains("val gattToClose = gatt"));
    assert!(gatt.contains("val txCharToDisable = txChar"));
    assert!(
        gatt.contains("gattToClose?.setCharacteristicNotification(it, false)"),
        "notification teardown should target the captured GATT handle"
    );
    assert!(gatt.contains("gattToClose?.disconnect()"));
    assert!(gatt.contains("gattToClose?.close()"));
    assert!(
        gatt.contains("if (gatt === gattToClose) {\n                gatt = null\n            }"),
        "delayed close must not clear a newer active GATT handle"
    );
    assert!(
        !gatt.contains("try { gatt?.close() }"),
        "delayed close must not dereference the mutable global GATT handle"
    );
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
    assert!(network_rs.contains("send_manual_announce_from_origin"));
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
    assert!(modals_js.contains("var INTERFACE_SHEET_ICONS = {"));
    assert!(modals_js.contains("function setBottomSheetTitleWithIcon(titleEl, title, iconType)"));
    assert!(modals_js.contains("function interfaceSheetIconTypeForInterface(ifaceType)"));
    assert!(modals_js.contains("_connectEditContext = _normaliseConnectEditContext(editContext);"));
    assert!(modals_js.contains("setBottomSheetTitleWithIcon(titleEl, editIface ? 'Edit LoRa Device' : 'Add LoRa Device', 'lora');"));
    assert!(modals_js.contains("setBottomSheetTitleWithIcon(\n        titleEl,\n        isEdit ? 'Edit Connection' : 'Connect to Network',"));
    assert!(modals_js.contains(
        "setBottomSheetTitleWithIcon(titleEl, isEdit ? 'Edit Host' : 'Host Network', 'host');"
    ));
    assert!(modals_js.contains("setBottomSheetTitleWithIcon(\n        titleEl,\n        isEdit ? 'Edit Backbone Server' : 'Host Backbone Server',"));
    assert!(modals_js.contains("titleIcon: interfaceSheetIcon('local')"));
    assert!(modals_js.contains("titleIcon: interfaceSheetIcon('ble')"));
    let dialogs_js = read_source(root.join("dashboard/static/js/dialogs.js")).expect("dialogs js");
    assert!(dialogs_js.contains("titleIcon: opts.titleIcon || ''"));
    assert!(dialogs_js.contains("titleIconType: opts.titleIconType || ''"));
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
fn tcp_public_connect_sheet_uses_curated_public_servers() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    assert!(index.contains("id=\"connect-tab-public\""));
    assert!(index.contains("id=\"connect-tab-custom\""));
    assert!(index.contains("id=\"public-server-list\""));
    assert!(index.contains("id=\"connect-name-field\" style=\"display:none;\""));

    let public_panel = index
        .split("id=\"connect-public-panel\"")
        .nth(1)
        .and_then(|tail| tail.split("id=\"connect-custom-panel\"").next())
        .expect("public connect panel");
    for hidden_endpoint in [
        "1.ratspeak.org",
        "2.ratspeak.org",
        "3.ratspeak.org",
        "rns.beleth.net",
        "rmap.world",
    ] {
        assert!(
            !public_panel.contains(hidden_endpoint),
            "public sheet shell should not render endpoint {hidden_endpoint}; JS cards expose friendly names"
        );
    }

    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    for expected in [
        "Ruby",
        "1.ratspeak.org",
        "4141",
        "Emerald",
        "2.ratspeak.org",
        "rns.ratspeak.org",
        "4242",
        "Diamond",
        "3.ratspeak.org",
        "4343",
        "Beleth",
        "rns.beleth.net",
        "RMAP",
        "rmap.world",
    ] {
        assert!(
            modals_js.contains(expected),
            "missing public TCP server token {expected}"
        );
    }
    assert!(modals_js.contains("function _isPublicTcpServer(host, port)"));
    assert!(modals_js.contains("function _publicServerMatchesEndpoint(server, host, port)"));
    assert!(modals_js.contains("aliases: [{ host: 'rns.ratspeak.org', port: 4242 }]"));
    assert!(modals_js.contains("tags: ['OFFICIAL']"));
    assert!(modals_js.contains("tags: ['UNOFFICIAL']"));
    assert!(!modals_js.contains("tags: ['Ratspeak', 'Public']"));
    assert!(!modals_js.contains("tags: ['Community', 'Public']"));
    assert!(modals_js.contains("var PUBLIC_SERVER_ARROW_ICON"));
    assert!(modals_js.contains("var PUBLIC_SERVER_CHECK_ICON"));
    assert!(modals_js.contains("var PUBLIC_SERVER_GEM_ICON"));
    assert!(modals_js.contains("function _publicServerMarkHtml(server)"));
    assert!(modals_js.contains("return !_isPublicTcpServer(entry.host, entry.port);"));
    assert!(modals_js.contains("quickConnect(server.host, server.port, server.name"));
    assert!(modals_js.contains("if (bbCheckbox && opts.publicServer) bbCheckbox.checked = false;"));

    let modals_css =
        read_source(root.join("dashboard/static/css/08-modals.css")).expect("modals css");
    assert!(modals_css.contains(".sheet-segmented-tabs"));
    assert!(modals_css.contains(".public-server-card--ruby"));
    assert!(modals_css.contains(".public-server-card--emerald"));
    assert!(modals_css.contains(".public-server-card--diamond"));
    assert!(modals_css.contains(".public-server-card--beleth"));
    assert!(modals_css.contains(".public-server-card--rmap"));
    assert!(modals_css.contains("grid-template-columns: 34px minmax(0, 1fr) 38px"));
    assert!(modals_css.contains("gap: var(--space-6);"));
    assert!(modals_css.contains(".public-server-mark--gem"));
    assert!(modals_css.contains("stroke-linejoin: round;"));
    assert!(modals_css.contains(".public-server-action svg"));
}

#[test]
fn ifac_is_available_ungated_on_client_and_server_sheets() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    for expected in [
        // Backbone stays a developer-mode experiment; IFAC does not.
        "id=\"connect-backbone-row\" style=\"display:none;\"",
        "id=\"connect-use-ifac\"",
        "id=\"connect-ifac-network-name\"",
        "id=\"connect-ifac-passphrase\"",
        "id=\"connect-ifac-size\"",
        "id=\"host-use-ifac\"",
        "id=\"host-ifac-network-name\"",
        "id=\"host-ifac-passphrase\"",
        "id=\"host-ifac-size\"",
        "id=\"backbone-host-use-ifac\"",
        "id=\"backbone-host-ifac-network-name\"",
        "id=\"backbone-host-ifac-passphrase\"",
        "id=\"backbone-host-ifac-size\"",
    ] {
        assert!(index.contains(expected), "missing IFAC UI token {expected}");
    }

    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    assert!(modals_js.contains("function _syncConnectAdvancedVisibility()"));
    assert!(modals_js.contains("if (bbRow) bbRow.style.display = dev ? '' : 'none';"));
    // IFAC row is always visible; only the size override is dev-gated.
    assert!(modals_js.contains("if (ifacRow) ifacRow.style.display = '';"));
    assert!(modals_js.contains(
        "if (sizeField) sizeField.style.display = _developerModeEnabled() ? '' : 'none';"
    ));
    assert!(modals_js.contains("function _ifacSyncFields(prefix)"));
    assert!(modals_js.contains("args.ifac_enabled = v.ifac_enabled;"));
    assert!(modals_js.contains("args.ifac_network_name = v.ifac_network_name;"));
    assert!(modals_js.contains("args.ifac_passphrase = v.ifac_passphrase;"));
    assert!(modals_js.contains("Enter an IFAC network name or passphrase"));
    assert!(modals_js.contains("_ifacGuardEmpty('connect')"));
    assert!(modals_js.contains("_ifacGuardEmpty('host')"));
    assert!(modals_js.contains("_ifacGuardEmpty('backbone-host')"));
    assert!(modals_js.contains("_ifacPopulate('connect', iface)"));
    assert!(modals_js.contains("_ifacPopulate('host', iface)"));
    assert!(modals_js.contains("_ifacPopulate('backbone-host', iface)"));
    assert!(modals_js.contains("_ifacApplyArgs('host', {"));
    assert!(modals_js.contains("_ifacApplyArgs('backbone-host', {"));
    assert!(modals_js.contains(
        "window.addEventListener('ratspeak-developer-mode-changed', _syncConnectAdvancedVisibility);"
    ));
    assert!(modals_js.contains("if (ifacCheckbox) ifacCheckbox.checked = false;"));
    assert!(modals_js.contains("if (ifacNetworkName) ifacNetworkName.value = '';"));
    assert!(modals_js.contains("if (ifacPassphrase) ifacPassphrase.value = '';"));
    assert!(modals_js.contains("if (ifacSize) ifacSize.value = '';"));

    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    assert!(interfaces_rs.contains("struct InterfaceIfacCommandFields"));
    assert!(interfaces_rs.contains("ifac_enabled: Option<bool>"));
    assert!(interfaces_rs.contains("ifac_size: Option<usize>"));
    assert!(interfaces_rs.contains("ifac_settings_from_args(&args.ifac, None)"));
    assert!(interfaces_rs.contains("ifac_settings_from_args(&args.ifac, Some(&old_ifac))"));
    assert!(interfaces_rs.contains("ifac_settings_from_args(&args.ifac, Some(&existing_ifac))"));
    assert!(interfaces_rs.contains("spawn_tcp_client_runtime_with_ifac"));
    assert!(interfaces_rs.contains("spawn_backbone_client_runtime_with_ifac"));
    assert!(interfaces_rs.contains("spawn_tcp_server_runtime_with_ifac"));
    assert!(interfaces_rs.contains("spawn_backbone_server_runtime_with_ifac"));

    let rns_config =
        read_source(root.join("crates/ratspeak-runtime/src/rns_config.rs")).expect("rns config");
    assert!(rns_config.contains("pub struct InterfaceIfacArgs"));
    assert!(rns_config.contains("network_name = {network_name}"));
    assert!(rns_config.contains("passphrase = {passphrase}"));
    assert!(rns_config.contains("ifac_size = {ifac_size}"));
    assert!(rns_config.contains("add_tcp_client_with_ifac"));
    assert!(rns_config.contains("update_tcp_client_with_ifac"));
    assert!(rns_config.contains("add_tcp_server_with_ifac"));
    assert!(rns_config.contains("update_tcp_server_with_ifac"));
    assert!(rns_config.contains("add_backbone_server_with_ifac"));
    assert!(rns_config.contains("update_backbone_server_with_ifac"));
}

#[test]
fn developer_mode_persists_in_sqlite_not_only_localstorage() {
    let root = repo_root();
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    assert!(settings_js.contains("RS.invoke('set_developer_mode'"));
    assert!(settings_js.contains("function adoptDeveloperModeFromBackend(enabled)"));
    assert!(settings_js.contains("adoptDeveloperModeFromBackend(data.developer_mode)"));

    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    assert!(interfaces_rs.contains("pub async fn set_developer_mode"));
    assert!(interfaces_rs.contains("\"developer_mode_enabled\""));
    assert!(interfaces_rs.contains("\"developer_mode\": developer_mode"));

    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("src-tauri lib");
    assert!(tauri_lib.contains("ratspeak_tauri::commands::interfaces::set_developer_mode"));
}

#[test]
fn interface_pause_resume_is_config_backed_and_visible() {
    let root = repo_root();

    let health_js = read_source(root.join("dashboard/static/js/health.js")).expect("health js");
    assert!(health_js.contains("Pause Interface"));
    assert!(health_js.contains("Resume Interface"));
    assert!(health_js.contains("label: 'Rename'"));
    assert!(health_js.contains("pause_interface"));
    assert!(health_js.contains("resume_interface"));
    assert!(health_js.contains("conn-iface-pill-paused"));
    assert!(!health_js.contains("Display Name"));
    assert!(!health_js.contains("dangerDivider"));

    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    assert!(modals_js.contains("name: name || (host + ':' + port)"));
    assert!(!modals_js.contains("'TCP to ' + host + ':' + port"));

    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    assert!(interfaces_rs.contains("pub async fn pause_interface"));
    assert!(interfaces_rs.contains("pub async fn resume_interface"));
    assert!(
        interfaces_rs
            .contains("crate::rns_config::set_interface_enabled(&config_dir, &name, false)")
    );
    assert!(
        interfaces_rs
            .contains("crate::rns_config::set_interface_enabled(&config_dir, &name, true)")
    );
    assert!(interfaces_rs.contains("teardown_live_interface_by_name(&st, &iface_name"));
    assert!(!interfaces_rs.contains("format!(\"TCP to {}:{}\""));

    let rns_config_rs =
        read_source(root.join("crates/ratspeak-runtime/src/rns_config.rs")).expect("rns config");
    assert!(rns_config_rs.contains("pub fn set_interface_enabled"));
    assert!(rns_config_rs.contains("key == \"enabled\" || key == \"interface_enabled\""));

    let app_shell = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    assert!(app_shell.contains("ratspeak_tauri::commands::interfaces::pause_interface"));
    assert!(app_shell.contains("ratspeak_tauri::commands::interfaces::resume_interface"));
}

#[test]
fn failed_lora_reconnects_keep_persisted_interface_config() {
    let root = repo_root();

    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    // Resume/update reconnects never request frontend rollback; only an add
    // that created the entry may (`fresh_add`). A hardcoded `true` regresses
    // re-adds of existing radios into config deletion on connect failure.
    assert!(interfaces_rs.contains("\"rollback_on_error\": false,"));
    assert!(interfaces_rs.contains("\"rollback_on_error\": fresh_add,"));
    assert!(!interfaces_rs.contains("\"rollback_on_error\": true"));
    assert!(
        interfaces_rs
            .contains("find_config_interface_with_group(&config_dir, None, &name).is_none()")
    );
    assert!(interfaces_rs.contains("mark_lora_add_freshness(&config_dir, &name, fresh_add)"));
    // Desktop pairing-timeout rollback consumes only the exact versioned
    // marker installed by its own config transaction.
    assert!(interfaces_rs.contains("if let Some(marker) = fresh_marker"));
    assert!(interfaces_rs.contains("rollback_fresh_lora_add_marker"));
    assert!(interfaces_rs.contains("clear_fresh_lora_add_marker"));
    // Failed resume flips the entry back to paused instead of deleting it or
    // leaving a dead enabled config.
    assert!(interfaces_rs.contains("set_interface_enabled_if_revision"));
    assert!(interfaces_rs.contains("InterfaceBlockCasOutcome::Applied"));
    assert!(interfaces_rs.contains("newer settings were left untouched"));

    let shared_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/shared.rs"))
        .expect("shared commands");
    assert!(shared_rs.contains("pub(crate) fn mark_lora_add_freshness"));
    assert!(shared_rs.contains("pub(crate) fn take_fresh_lora_add"));
    assert!(shared_rs.contains("type FreshLoraAddKey = (PathBuf, String)"));
    assert!(shared_rs.contains("struct FreshLoraAddEntry"));
    assert!(shared_rs.contains("entry.marker == expected_marker"));
    assert!(shared_rs.contains("const FRESH_LORA_ADD_TTL"));
    assert!(shared_rs.contains("const MAX_FRESH_LORA_ADDS"));

    let ble_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/ble.rs")).expect("ble commands");
    assert!(ble_rs.contains("rollback_ble_rnode_context"));
    assert!(ble_rs.contains("clear_ble_rnode_rollback_context"));

    // Native failure cleanup is exact-token backend work; the frontend never
    // chains a delayed cancellation command. User cancellation still reaches
    // edit reconnects, whose config transaction clears any old marker.
    let events_js =
        read_source(root.join("dashboard/static/js/tauri_events.js")).expect("tauri events js");
    assert!(!events_js.contains("cancel_ble_connect"));
    assert!(!events_js.contains("rollbackOnly"));
    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    assert!(
        modals_js
            .contains("RS.invoke('cancel_ble_connect', { name: bleName }).catch(function() {});")
    );
    assert!(!modals_js.contains(
        "if (!isEdit) RS.invoke('cancel_ble_connect', { name: bleName }).catch(function() {});"
    ));
}

#[test]
fn rnode_radio_catalog_has_single_runtime_source() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    let tauri_events_js =
        read_source(root.join("dashboard/static/js/tauri_events.js")).expect("tauri events js");
    let core_radio =
        read_source(root.join("crates/ratspeak-core/src/radio.rs")).expect("radio source");
    let rns_config_rs =
        read_source(root.join("crates/ratspeak-runtime/src/rns_config.rs")).expect("rns config");
    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces source");
    let ble_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/ble.rs")).expect("ble source");
    let rns_runtime_rs =
        read_source(root.join("../rsReticulum/crates/rns-runtime/src/reticulum.rs"))
            .expect("rns runtime source");

    assert!(core_radio.contains("pub const RNODE_PRESETS"));
    assert!(core_radio.contains("pub const RNODE_REGIONS"));
    assert!(core_radio.contains("uhf_433"));
    assert!(modals_js.contains("RS.invoke('api_rnode_presets')"));
    assert!(modals_js.contains("function _rnodeParseFrequencyHz"));
    assert!(modals_js.contains("function _rnodeFormatScaledValue"));
    assert!(modals_js.contains("return _rnodeFormatScaledValue(freq, 1000000, 6, 3);"));
    assert!(modals_js.contains("return _rnodeFormatScaledValue(bw, 1000, 3, 0);"));
    assert!(modals_js.contains("var RNODE_TCP_DEFAULT_PORT = 7633;"));
    assert!(modals_js.contains("function _normaliseRnodeTcpEndpoint(raw)"));
    assert!(modals_js.contains("if (_rnodeIsTcpPort(port)) return 'tcp';"));
    assert!(modals_js.contains("setRnodeConnectionType('tcp')"));
    assert!(modals_js.contains("function _rnodeNormaliseInterfaceMode(mode)"));
    assert!(modals_js.contains("mode: _rnodeReadInterfaceMode()"));
    assert!(modals_js.contains("window.ratspeakDeveloperModeEnabled()"));
    assert!(modals_js.contains("built.sheet.classList.add('local-network-sheet')"));
    assert!(
        modals_js.contains("loraArgs.frequency = radioSettings.frequency")
            || modals_js.contains("frequency: radioSettings.frequency")
    );
    assert!(modals_js.contains("loraArgs.custom_params = true"));
    assert!(index.contains(r#"id="rnode-frequency""#));
    assert!(index.contains(r#"id="rnode-advanced""#));
    assert!(index.contains(r#"id="rnode-toggle-tcp""#));
    assert!(index.contains(r#"id="rnode-tcp-endpoint""#));
    assert!(index.contains(r#"id="rnode-mode-field" style="display:none;""#));
    assert!(index.contains(r#"id="rnode-interface-mode""#));
    assert!(index.contains(r#"<option value="full">Full</option>"#));
    assert!(index.contains(r#"<option value="gateway">Gateway</option>"#));
    assert!(index.contains(r#"<option value="access_point">Access Point (AP)</option>"#));
    assert!(index.contains(r#"<option value="boundary">Boundary</option>"#));
    assert!(index.contains(r#"<option value="roaming">Roaming</option>"#));
    assert!(index.contains("Mode affects routing and announce propagation."));
    assert!(rns_config_rs.contains(
        r#"pub const RNODE_INTERFACE_MODES: &[&str] =
    &["full", "gateway", "access_point", "boundary", "roaming"];"#
    ));
    assert!(rns_config_rs.contains("pub fn normalize_rnode_interface_mode"));
    assert!(rns_config_rs.contains("\"gateway\" | \"gw\" => Some(\"gateway\")"));
    assert!(
        rns_config_rs.contains("\"access_point\" | \"accesspoint\" | \"access point\" | \"ap\"")
    );
    assert!(rns_config_rs.contains("mode = {mode}"));
    assert!(!rns_config_rs.contains("\"point_to_point\" => Some(\"point_to_point\")"));
    assert!(interfaces_rs.contains("pub mode: Option<String>"));
    assert!(
        interfaces_rs.contains("let mode = normalize_lora_interface_mode(args.mode.as_deref())?;")
    );
    assert!(interfaces_rs.contains("mode: Some(mode)"));
    assert!(interfaces_rs.contains("mode: runtime_mode"));
    assert!(interfaces_rs.contains("fn cfg_rnode_mode(entry: &Value) -> String"));
    assert!(
        interfaces_rs
            .contains("cfg_str(entry, \"mode\").or_else(|| cfg_str(entry, \"interface_mode\"))")
    );
    assert!(interfaces_rs.contains("\"mode\": mode"));
    assert!(ble_rs.contains("pub mode: Option<String>"));
    assert!(ble_rs.contains("rnode_interface_mode_value(args.mode.as_deref())"));
    assert!(ble_rs.contains("mode,"));
    assert!(tauri_events_js.contains("mode: data.mode"));
    assert!(rns_runtime_rs.contains("pub mode: rns_interface::traits::InterfaceMode"));
    assert!(rns_runtime_rs.matches("config.mode = mode;").count() >= 4);
    let tauri_cargo =
        read_source(root.join("crates/ratspeak-tauri/Cargo.toml")).expect("tauri cargo");
    assert!(tauri_cargo.contains("rnode-tcp = [\"ratspeak-runtime/rnode-tcp\""));
    let app_cargo = read_source(root.join("src-tauri/Cargo.toml")).expect("app cargo");
    assert!(app_cargo.contains(r#"features = ["ble", "rnode-tcp", "mobile-throttle", "seed"]"#));
    assert!(!modals_js.contains("var RNODE_PRESETS = {"));
    assert!(!modals_js.contains("var RNODE_REGIONS = {"));
    assert!(!index.contains("<option value=\"americas\""));
    assert!(!index.contains("<option value=\"medium_fast\""));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    assert!(responsive_css.contains(".bottom-sheet .modal-field label"));
    assert!(responsive_css.contains(".bottom-sheet .rs-dialog-field-label"));
    assert!(responsive_css.contains(".bottom-sheet .sheet-segmented-tabs button"));
    assert!(responsive_css.contains(".bottom-sheet .rs-dialog-choice-hint"));
    assert!(responsive_css.contains(".bottom-sheet .rs-dialog-checkbox-label"));
    assert!(responsive_css.contains(".bottom-sheet .hub-iface-detail"));
    assert!(responsive_css.contains("#connect-modal .connect-tab-toggle button"));
    assert!(responsive_css.contains("#connect-modal .quick-connect-btn"));
    assert!(responsive_css.contains("#connect-modal .quick-connect-detail"));
    assert!(responsive_css.contains("#connect-modal .public-server-name"));
    assert!(responsive_css.contains("#connect-modal .public-server-tag"));
    assert!(responsive_css.contains("#rnode-modal .rnode-pairing-tip"));
    assert!(responsive_css.contains("#rnode-modal .rnode-frequency-unit"));
    assert!(responsive_css.contains("#rnode-modal .ble-device-meta"));
    assert!(responsive_css.contains(".bottom-sheet .rs-dialog-field-help"));
    assert!(responsive_css.contains("font-size: var(--mobile-list-detail-size);"));
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
    let nav = read_source(repo_root().join("dashboard/static/js/nav.js")).expect("nav js");

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
    assert!(nav.contains("function _chatMessagesNearBottomForKeyboard()"));
    assert!(nav.contains("function _pinChatMessagesToBottomForKeyboard()"));
    assert!(nav.contains("_waitingForKeyboard = _chatMessagesNearBottomForKeyboard();"));
    assert!(nav.contains(
        "document.documentElement.classList.contains('keyboard-open') && _chatMessagesNearBottomForKeyboard()"
    ));
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
    assert!(lxmf.contains("function _stripImageMetadataForShare(file)"));
    assert!(lxmf.contains("ctx.drawImage(decoded.source"));
    assert!(lxmf.contains("metadata_stripped: true"));
    assert!(lxmf.contains("Could not remove image metadata; image not attached"));
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
    assert!(lxmf.contains("function _wireImageViewerSwipeDismiss(viewer, stage, img)"));
    assert!(lxmf.contains("viewer.classList.toggle('is-zoomed', zoomed);"));
    assert!(lxmf.contains("Math.abs(dy) > 64"));
    assert!(lxmf.contains("if (e.target === stage) closeImageViewer();"));
    assert!(lxmf.contains("function _canCopyDownloadedImages()"));
    assert!(lxmf.contains("if (typeof isAndroid === 'function' && isAndroid()) return false;"));
    assert!(lxmf.contains("function _syncImageViewerActions(viewer)"));
    assert!(lxmf.contains("copyBtn.hidden = !canCopy;"));
    assert!(lxmf.contains("_saveDownloadedMediaFile(file, { preferPhotos: true })"));
    assert!(lxmf.contains("Saved to photos!"));
    assert!(lxmf.contains("function _compensateImageLoadScroll(container, img, before)"));
    assert!(lxmf.contains("function _messageHasTransferPayload(msg)"));
    assert!(lxmf.contains("function _messageCanCancelSend(msg)"));
    assert!(lxmf.contains("function _messageCanCancelTransfer(msg)"));
    assert!(
        lxmf.contains("if (msg.state === 'sent') return _messageDeliveryMethod(msg) === 'direct';")
    );
    assert!(lxmf.contains("function _messageTransferPayloadSize(msg)"));
    assert!(lxmf.contains("function _messageShowsTransferPercent(msg)"));
    assert!(lxmf.contains("lxmfLimits.efficient_resource_bytes || 1048575"));
    assert!(lxmf.contains("if (!_messageShowsTransferPercent(msg)) return null;"));
    assert!(lxmf.contains("if (!_messageCanCancelSend(msg)) return '';"));
    assert!(lxmf.contains("aria-label=\"Cancel send\">Cancel</button>"));
    assert!(lxmf.contains("canCancelSend ? _messageInlineCancelHtml(msg) : '<span class=\"msg-time\">' + time + '</span>'"));

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
    assert!(messaging_css.contains("max-width: min(560px, 75%);"));
    assert!(messaging_css.contains(".image-viewer-img.is-dragging"));
    assert!(messaging_css.contains("touch-action: pan-x pinch-zoom;"));
    assert!(messaging_css.contains(".image-viewer.is-zoomed .image-viewer-stage"));
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
    assert!(state_js.contains("function _rsDesktopMicrophonePermission(audio)"));
    assert!(state_js.contains("RS.invoke('request_microphone_permission')"));
    assert!(state_js.contains("navigator.mediaDevices.getUserMedia"));

    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf.contains("function _voiceEnsureMicrophonePermission()"));
    let voice_mic_permission = lxmf
        .split("function _voiceEnsureMicrophonePermission()")
        .nth(1)
        .and_then(|tail| tail.split("function _voiceEnsurePlaybackReady()").next())
        .expect("voice microphone permission function");
    assert!(!voice_mic_permission.contains("isTauriDesktop"));
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

    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    assert!(tauri_lib.contains("async fn request_microphone_permission(_app: tauri::AppHandle)"));
    assert!(tauri_lib.contains("fn request_microphone_permission_macos("));
    assert!(tauri_lib.contains("AVCaptureDevice"));
    assert!(tauri_lib.contains("requestAccessForMediaType"));
    assert!(tauri_lib.contains("_app.run_on_main_thread"));
    assert!(tauri_lib.contains("request_microphone_permission,"));

    let mac_info_plist = read_source(root.join("src-tauri/Info.plist")).expect("mac info plist");
    assert!(mac_info_plist.contains("NSMicrophoneUsageDescription"));
    let tauri_conf = read_source(root.join("src-tauri/tauri.conf.json")).expect("tauri conf");
    assert!(tauri_conf.contains(r#""signingIdentity": "-""#));
    assert!(tauri_conf.contains(r#""entitlements": "Entitlements.plist""#));
    let mac_entitlements =
        read_source(root.join("src-tauri/Entitlements.plist")).expect("mac entitlements");
    assert!(mac_entitlements.contains("com.apple.security.device.audio-input"));
    let release_macos = read_source(root.join(".github/workflows/release-macos.yml"))
        .expect("mac release workflow");
    assert!(release_macos.contains(r#""entitlements":"Entitlements.plist""#));

    let voice_rs =
        read_source(root.join("crates/ratspeak-runtime/src/voice.rs")).expect("voice rs");
    assert!(voice_rs.contains("fn notify_incoming_call_if_background("));
    assert!(voice_rs.contains("NativeNotification::call("));
    assert!(voice_rs.contains("Incoming call from {label}"));
    assert!(voice_rs.contains("crate::stable_notification_id(&link_hex, 3_000_000)"));
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
    assert!(voice_rs.contains("fn record_lxst_activity("));
    assert!(voice_rs.contains("producer::lxst_activity(transition)"));

    let runtime_rs =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime lib");
    assert!(runtime_rs.contains("voice::announce_if_running(state).await"));
    assert!(runtime_rs.contains("producer::AnnounceMethod::LxstService"));

    let notification_rs =
        read_source(root.join("crates/ratspeak-core/src/notification.rs")).expect("notification");
    assert!(notification_rs.contains("NativeNotificationKind::Call"));
    assert!(notification_rs.contains("pub fn call("));

    let notifier_rs =
        read_source(root.join("crates/ratspeak-tauri/src/notifier.rs")).expect("notifier");
    assert!(notifier_rs.contains("NativeNotificationKind::Call => \"ratspeak_calls\""));

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
    assert!(activity_js.contains("calls: 'Calls'"));
    assert!(activity_js.contains("'lxst.call': 'Call'"));
    assert!(activity_js.contains("'lxst.media': 'Call media'"));

    let service =
        read_source(root.join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakService.kt",
        ))
        .expect("android service");
    assert!(service.contains("CALL_CHANNEL_ID = \"ratspeak_calls\""));
    assert!(service.contains("createCallNotificationChannel()"));
    assert!(service.contains("NotificationManager.IMPORTANCE_HIGH"));
    assert!(service.contains("lockscreenVisibility = Notification.VISIBILITY_PUBLIC"));
}

#[test]
fn apple_bluetooth_permission_copy_is_current_and_aligned() {
    let root = repo_root();
    let expected = "Ratspeak uses Bluetooth to connect to hardware nodes and other Bluetooth peers when enabled.";
    for relative in [
        "src-tauri/Info.plist",
        "src-tauri/gen/apple/ratspeak_iOS/Info.plist",
    ] {
        let plist = read_source(root.join(relative)).expect("Apple Info.plist");
        assert_eq!(plist.matches(expected).count(), 2, "{relative}");
        assert!(!plist.contains("Ratlow mesh devices"), "{relative}");
    }
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
    let version_file = read_source(root.join("VERSION")).expect("display version");
    assert_eq!(version_file.trim(), "1.0.25");

    let system_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/system.rs")).expect("system rs");
    assert!(system_rs.contains("env!(\"CARGO_PKG_VERSION\")"));
    assert!(system_rs.contains("RATSPEAK_DISPLAY_VERSION"));
    assert!(system_rs.contains("GITHUB_REF_NAME"));
    assert!(system_rs.contains("strip_prefix('v')"));
    assert!(!system_rs.contains("\"version\": \"1.0.13\""));

    let tauri_build =
        read_source(root.join("crates/ratspeak-tauri/build.rs")).expect("tauri crate build");
    assert!(tauri_build.contains("../../VERSION"));
    assert!(tauri_build.contains("cargo::rustc-env=RATSPEAK_DISPLAY_VERSION"));

    let index = read_source(root.join("dashboard/index.html")).expect("index");
    assert!(index.contains("id=\"settings-version-sidebar\""));
    assert!(index.contains("id=\"settings-version-system\""));
    assert!(index.contains("class=\"system-data-tip\""));
    assert!(
        index.contains("Click and hold the send button on a message to choose its delivery type.")
    );
    assert!(!index.contains("Tap and hold the send button in Messages"));
    let settings_sidebar = index
        .split("class=\"settings-sidebar-panel\"")
        .nth(1)
        .and_then(|tail| tail.split("class=\"settings-detail-pane\"").next())
        .expect("settings sidebar");
    assert!(settings_sidebar.contains("class=\"system-data-tip\""));
    let sidebar_version = settings_sidebar
        .find("id=\"settings-version-sidebar\"")
        .expect("sidebar version");
    let sidebar_tip = settings_sidebar
        .find("class=\"system-data-tip\"")
        .expect("settings tip");
    assert!(sidebar_version < sidebar_tip);
    let system_panel = index
        .split("id=\"panel-settings-system\"")
        .nth(1)
        .and_then(|tail| tail.split("id=\"settings-version-system\"").next())
        .expect("system panel");
    assert!(!system_panel.contains("class=\"system-data-tip\""));

    let settings_js = read_source(root.join("dashboard/static/js/settings.js")).expect("settings");
    assert!(settings_js.contains("function renderSettingsVersion()"));
    assert!(settings_js.contains("RS.invoke('api_version')"));
    assert!(settings_js.contains("name + ' v.' + version"));
    assert!(settings_js.contains("RATSPEAK_RELEASE_LATEST_URL"));
    assert!(settings_js.contains("https://api.github.com/repos/ratspeak/Ratspeak/releases/latest"));
    assert!(settings_js.contains("function promptRatspeakUpdateCheck"));
    assert!(settings_js.contains("title: 'Check for updates?'"));
    assert!(settings_js.contains("confirmText: 'Yes'"));
    assert!(settings_js.contains("cancelText: 'No'"));
    assert!(settings_js.contains("function checkRatspeakUpdate"));
    assert!(settings_js.contains("function _settingsVersionSuffixRank"));
    assert!(settings_js.contains("replace(/(\\d)-([a-z]+)$/i, '$1$2')"));
    assert!(settings_js.contains("fetch(RATSPEAK_RELEASE_LATEST_URL"));
    assert!(settings_js.contains("Update available!"));
    assert!(settings_js.contains("Up to date!"));
    assert!(settings_js.contains("For privacy reasons, we do not currently auto-update"));

    let nav_js = read_source(root.join("dashboard/static/js/nav.js")).expect("nav");
    assert!(nav_js.contains("id=\"about-modal-version\""));
    assert!(nav_js.contains("RS.invoke('api_version')"));
    assert!(!nav_js.contains("v1.0.7"));

    let dialogs_js = read_source(root.join("dashboard/static/js/dialogs.js")).expect("dialogs");
    assert!(dialogs_js.contains("function rsAlert(opts)"));

    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    assert!(views_css.contains(".settings-sidebar-version"));
    assert!(views_css.contains(".settings-version-system"));
    assert!(views_css.contains(".settings-update-check-btn"));
    let forms_css = read_source(root.join("dashboard/static/css/06-forms.css")).expect("forms css");
    assert!(forms_css.contains(".system-data-tip"));
    assert!(forms_css.contains(".system-data-tip-icon"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    assert!(responsive_css.contains(".settings-version-system"));
    assert!(responsive_css.contains("text-align: center;"));

    let tauri_conf = read_source(root.join("src-tauri/tauri.conf.json")).expect("tauri conf");
    assert!(
        tauri_conf.contains("connect-src 'self' ipc: http://ipc.localhost https://api.github.com")
    );
    assert!(tauri_conf.contains(r#""versionCode": 1000029"#));

    let android_gradle = read_source(root.join("src-tauri/gen/android/app/build.gradle.kts"))
        .expect("android gradle");
    assert!(android_gradle.contains("fun ratspeakDisplayVersionName()"));
    assert!(android_gradle.contains("../../../VERSION"));
    assert!(android_gradle.contains("versionName = ratspeakDisplayVersionName()"));
}

#[test]
fn settings_information_architecture_groups_one_off_settings() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");

    assert!(!index.contains(r#"data-settings-panel="panel-settings-blocked""#));
    assert!(!index.contains(r#"id="panel-settings-blocked""#));
    assert!(!index.contains(r#"<span class="settings-nav-label">Blocked Users</span>"#));
    assert!(!index.contains(r#"data-settings-panel="panel-settings-notifications""#));
    assert!(!index.contains(r#"id="panel-settings-notifications""#));
    assert!(!index.contains(r#"id="settings-nav-notifications""#));

    let general_panel = index
        .split(r#"id="panel-settings-general""#)
        .nth(1)
        .and_then(|tail| tail.split(r#"id="panel-settings-identity""#).next())
        .expect("general settings panel");
    assert!(
        general_panel.contains(r#"<span class="settings-row-label">Desktop Notifications</span>"#)
    );
    assert!(general_panel.contains(r#"id="settings-row-notifications""#));
    assert!(general_panel.contains(r#"id="desktop-notifications-toggle""#));
    assert!(general_panel.contains(r#"<span class="settings-row-label">Block List</span>"#));
    assert!(general_panel.contains(
        r#"class="selector-badge selector-badge-no-caret" id="settings-blocked-count">Manage</button>"#
    ));

    let identity_panel = index
        .split(r#"id="panel-settings-identity""#)
        .nth(1)
        .and_then(|tail| tail.split(r#"id="panel-settings-privacy""#).next())
        .expect("identity settings panel");
    assert!(identity_panel.contains(r#"<span class="settings-row-label">Status</span>"#));
    assert!(identity_panel.contains(r#"id="settings-identity-status-desc""#));
    assert!(identity_panel.contains(r#"id="settings-edit-status-btn""#));
    assert!(identity_panel.contains(r#"id="settings-clear-status-btn" disabled"#));
    assert!(identity_panel.contains(
        r#"class="selector-badge selector-badge-no-caret" id="settings-manage-identities-btn">Manage</button>"#
    ));
    assert!(identity_panel.contains(
        r#"class="selector-badge selector-badge-no-caret" id="settings-backup-identity-btn">Export</button>"#
    ));
    assert!(identity_panel.contains(r#"<span class="settings-row-label">Backup Identity</span>"#));
    assert!(
        !identity_panel.contains(r#"<span class="settings-row-label">View Recovery Phrase</span>"#)
    );
    assert!(identity_panel.contains(
        r#"class="selector-badge selector-badge-no-caret" id="settings-view-recovery-phrase-btn">View</button>"#
    ));
    assert!(
        identity_panel
            .contains(r#"<span class="settings-row-label">Hardware Key Auto-Lock</span>"#)
    );
    assert!(identity_panel.contains(r#"id="hw-lock-row""#));

    let network_panel = index
        .split(r#"id="panel-settings-network""#)
        .nth(1)
        .and_then(|tail| tail.split(r#"id="panel-settings-offline-inbox""#).next())
        .expect("network settings panel");
    assert!(network_panel.contains(r#"<span class="settings-row-label">Transport Mode</span>"#));
    assert!(network_panel.contains(r#"<span class="settings-row-label">Auto-Announce</span>"#));
    assert!(!network_panel.contains("Hardware Key Auto-Lock"));

    assert!(
        settings_js
            .contains("var _notifRow = document.getElementById('settings-row-notifications');")
    );
    assert!(!settings_js.contains("document.getElementById('panel-settings-notifications')"));
    assert!(settings_js.contains("function syncSettingsIdentityStatus()"));
    assert!(settings_js.contains("function clearActiveIdentityStatus()"));
    assert!(settings_js.contains("saveIdentityStatus('')"));
    assert!(settings_js.contains("openIdentityStatusEditor()"));
    assert!(
        settings_js.contains("setActiveProfileStatus(savedStatus === null ? '' : savedStatus);")
    );

    assert!(views_css.contains(".settings-row-actions"));
    assert!(views_css.contains(".selector-badge-no-caret::after"));
    assert!(responsive_css.contains(".settings-row-actions"));
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
fn settings_system_panel_has_developer_mode_and_reset_group() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");

    assert!(
        index.contains(
            r#"data-settings-panel="panel-settings-system" data-settings-title="System""#
        )
    );
    assert!(index.contains(r#"<span class="settings-nav-label">System</span>"#));
    assert!(!index.contains(r#"<span class="settings-nav-label">System Data</span>"#));
    assert!(index.contains(r#"<div class="panel-header">System</div>"#));
    assert!(index.contains(r#"<div class="settings-panel-section-title">System</div>"#));
    assert!(index.contains(r#"<span class="settings-row-label">Developer Mode</span>"#));
    assert!(index.contains(r#"role="radiogroup" aria-label="Developer Mode""#));
    assert!(index.contains(r#"type="radio" name="settings-developer-mode" id="settings-developer-mode-off" value="off" checked"#));
    assert!(index.contains(
        r#"type="radio" name="settings-developer-mode" id="settings-developer-mode-on" value="on""#
    ));
    assert!(index.contains(r#"<div class="settings-panel-section-title">Reset</div>"#));

    let system_title = index
        .find(r#"<div class="settings-panel-section-title">System</div>"#)
        .unwrap();
    let developer_mode = index
        .find(r#"<span class="settings-row-label">Developer Mode</span>"#)
        .unwrap();
    let reset_title = index
        .find(r#"<div class="settings-panel-section-title">Reset</div>"#)
        .unwrap();
    let cache_section = index.find(r#"id="system-section-caches""#).unwrap();
    assert!(
        system_title < developer_mode
            && developer_mode < reset_title
            && reset_title < cache_section
    );

    assert!(settings_js.contains("function initDeveloperModeToggle()"));
    assert!(settings_js.contains("initDeveloperModeToggle();"));
    assert!(settings_js.contains("var _settingsDeveloperModeStorageKey"));
    assert!(settings_js.contains("function readDeveloperModePreference()"));
    assert!(settings_js.contains("function setDeveloperModeEnabled(enabled)"));
    assert!(settings_js.contains("window.ratspeakDeveloperModeEnabled = function()"));
    assert!(settings_js.contains("ratspeak-developer-mode-changed"));
    assert!(settings_js.contains("if (on.checked) setDeveloperModeEnabled(true);"));
    assert!(settings_js.contains("setDeveloperModeEnabled(false);"));
    assert!(!settings_js.contains("function rejectDeveloperModeEnable()"));
    assert!(!settings_js.contains("Developer mode is coming soon."));
    assert!(!settings_js.contains("title: 'Enable Developer Mode?'"));
    assert!(!settings_js.contains("confirmText: 'Enable'"));
    assert!(!settings_js.contains("_settingsDeveloperModeEnabled = !!ok;"));
    // Durable store is SQLite (see developer_mode_persists_in_sqlite_not_only_localstorage).
    assert!(settings_js.contains("RS.invoke('set_developer_mode'"));

    assert!(views_css.contains(".settings-panel-section-title"));
    assert!(views_css.contains(".settings-radio-group"));
    assert!(views_css.contains(".settings-radio-option input:checked + span"));
    assert!(
        responsive_css
            .contains(".settings-radio-option span { min-height: 38px; min-width: 58px; }")
    );
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
    assert!(responsive_css.contains(".activity-profile-btn,\n    .activity-filter-chip"));
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
    assert!(responsive_css.contains("#bottom-sheet .bottom-sheet-item"));
    assert!(responsive_css.contains("background: transparent;"));
    assert!(responsive_css.contains("border: 0;"));
    assert!(responsive_css.contains("--mobile-list-avatar-size: 42px;"));
}

#[test]
fn network_interface_sections_scroll_without_compressing_rows() {
    let root = repo_root();
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");

    assert!(views_css.contains(".network-layout {\n    display: flex;"));
    assert!(views_css.contains("box-sizing: border-box;\n    overflow: hidden;"));
    assert!(views_css.contains(".network-main {\n    display: grid;"));
    assert!(views_css.contains("min-height: 0;\n    min-width: 0;\n    overflow: hidden;"));
    assert!(views_css.contains(".network-connections {\n    display: flex;"));
    assert!(views_css.contains("min-height: 0;\n    min-width: 0;\n    overflow-y: auto;"));
    assert!(views_css.contains("overscroll-behavior: contain;"));
    assert!(views_css.contains("scrollbar-gutter: stable;"));
    assert!(views_css.contains(".conn-section {\n    background: var(--surface-panel);"));
    assert!(views_css.contains("flex-shrink: 0;"));
    assert!(views_css.contains(".conn-section-body {\n    max-height: min(44vh, 420px);"));
    assert!(views_css.contains("overflow-y: auto;"));
    assert!(views_css.contains(
        ".conn-section.collapsed .conn-section-body {\n    max-height: 0;\n    overflow: hidden;"
    ));

    assert!(responsive_css.contains(".network-main {\n        display: flex;"));
    assert!(responsive_css.contains("padding-bottom: calc(62px + var(--sab) + var(--space-5));"));
    assert!(responsive_css.contains(
        ".conn-section:not(.collapsed) .conn-section-body {\n        max-height: none;\n        overflow: visible;"
    ));
    assert!(responsive_css.contains(
        ".network-layout {\n        grid-template-columns: 1fr;\n        overflow: hidden;"
    ));
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
    assert!(peers.contains("var mobileRows = window.innerWidth <= 768;"));
    assert!(peers.contains("var baseRowHeight = mobileRows ? 58 : 36;"));
    assert!(peers.contains("var statusRowHeight = mobileRows ? 68 : 48;"));
    assert!(peers.contains("_peersRowHeight = baseRowHeight;"));
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
fn cache_clear_buttons_clear_reticulum_db_and_frontend_caches() {
    let root = repo_root();
    let system =
        read_source(root.join("crates/ratspeak-tauri/src/commands/system.rs")).expect("system rs");
    let db = read_source(root.join("crates/ratspeak-db/src/db.rs")).expect("db rs");
    let events =
        read_source(root.join("dashboard/static/js/tauri_events.js")).expect("tauri events");
    let peers_cache =
        read_source(root.join("dashboard/static/js/peers_cache.js")).expect("peers cache");
    let settings = read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");

    assert!(system.contains("TransportQuery::DropPathTable"));
    assert!(system.contains("TransportQuery::DropRecentAnnounces"));
    assert!(system.contains("clear_discovered_identity_activity"));
    assert!(system.contains("emit_to_all(\"paths_cleared\""));
    assert!(system.contains("emit_to_all(\n        \"announces_cleared\""));
    assert!(db.contains("pub fn clear_discovered_identity_activity"));
    assert!(db.contains("DELETE FROM identity_activity AS ia"));
    assert!(db.contains("NOT EXISTS (\n                 SELECT 1 FROM contacts"));
    assert!(events.contains("RS.listen('paths_cleared'"));
    assert!(events.contains("RS.listen('announces_cleared'"));
    assert!(events.contains("announceCache = [];"));
    assert!(events.contains("RS.invoke('api_get_peers_snapshot')"));
    assert!(peers_cache.contains("function replace(rows)"));
    assert!(settings.contains("Path table cleared."));
    assert!(!settings.contains("Hub node restarting"));
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
    let js_qr = read_source(root.join("dashboard/static/js/vendor/jsQR.js")).expect("jsQR vendor");
    let js_qr_license = read_source(root.join("dashboard/static/js/vendor/jsQR.LICENSE.txt"))
        .expect("jsQR license");
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    let android_main = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("android main activity");
    let tauri_build = read_source(root.join("src-tauri/build.rs")).expect("tauri build script");
    let contact_card_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/contact_card.rs"))
            .expect("contact card command");
    let lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    let db = read_source(root.join("crates/ratspeak-db/src/db.rs")).expect("db");

    assert!(index.contains(r#"/static/js/contact_card.js"#));
    let js_qr_script = index
        .find(r#"/static/js/vendor/jsQR.js"#)
        .expect("jsQR script is loaded");
    let contact_card_script = index
        .find(r#"/static/js/contact_card.js"#)
        .expect("contact card script is loaded");
    assert!(
        js_qr_script < contact_card_script,
        "QR decoder must load before contact card scanner"
    );
    assert!(js_qr.contains("root[\"jsQR\"] = factory();"));
    assert!(js_qr_license.contains("Apache License"));
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
    assert!(contact_card_js.contains("window.jsQR(image.data, width, height"));
    assert!(contact_card_js.contains("contact-scan-file-input"));
    assert!(contact_card_js.contains("'<span>Live Scan</span></button>'"));
    assert!(contact_card_js.contains("'<span>Scan Photo</span></button>'"));
    assert!(!contact_card_js.contains("Take Photo"));
    assert!(!contact_card_js.contains("Choose Photo"));
    assert!(contact_card_js.contains("getQrScannerEnvironment"));
    assert!(contact_card_js.contains("env.prefer_live_scanner === false"));
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
    assert!(android_main.contains("WebViewCompat.getCurrentWebViewPackage(this)"));
    assert!(android_main.contains("gmsLabel.contains(\"microg\", ignoreCase = true)"));
    assert!(android_main.contains("fun getQrScannerEnvironment(): String"));
    assert!(android_main.contains("put(\"microg_detected\", microGDetected)"));
    assert!(android_main.contains("put(\"prefer_live_scanner\", preferLive)"));

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
    let gestures = read_source(root.join("dashboard/static/js/gestures.js")).expect("gestures js");
    let emoji_picker =
        read_source(root.join("dashboard/static/js/emoji_picker.js")).expect("emoji picker js");
    let runtime =
        read_source(root.join("crates/ratspeak-runtime/src/lxmf.rs")).expect("runtime lxmf");
    let inbound =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime lib");
    let messaging = read_source(root.join("crates/ratspeak-tauri/src/commands/messaging.rs"))
        .expect("messaging command");

    assert!(lxmf.contains("RS.gestures.attachLongPress(bubble"));
    assert!(lxmf.contains("preventDefaultOnStart: function()"));
    assert!(lxmf.contains("if (e.defaultPrevented) return;"));
    assert!(lxmf.contains("(t.closest('.lxmf-msg') && _shouldPreserveLxmfComposerKeyboard())"));
    assert!(lxmf.contains("function _bindMessageFocusPreservingActivation"));
    assert!(lxmf.contains("preserveComposerKeyboard"));
    assert!(lxmf.contains("var _suppressImageOpenUntil = 0;"));
    assert!(lxmf.contains("container.querySelectorAll('.lxmf-send-cancel, .msg-send-cancel-inline').forEach(function(btn)"));
    assert!(lxmf.contains("_bindMessageFocusPreservingActivation(btn, function()"));
    assert!(lxmf.contains("_cancelLxmfSend(btn.getAttribute('data-msg-id'));"));
    assert!(lxmf.contains("_suppressImageOpenUntil = Date.now() + 900;"));
    assert!(lxmf.contains("if (Date.now() < _suppressImageOpenUntil)"));
    assert!(lxmf.contains("function _restoreLxmfComposerKeyboard"));
    assert!(lxmf.contains("window.RS.closeMessageActionMenu"));
    assert!(lxmf.contains("var ICON_SEND_OPPORTUNISTIC"));
    assert!(lxmf.contains("var ICON_SEND_DIRECT"));
    assert!(lxmf.contains("label: 'Opportunistic', icon: ICON_SEND_OPPORTUNISTIC"));
    assert!(lxmf.contains("label: 'Direct', icon: ICON_SEND_DIRECT"));
    assert!(!lxmf.contains("label: 'Direct', icon: ICON_ROUTE"));
    assert!(lxmf.contains("function _copyToClipboard(text)"));
    assert!(lxmf.contains("function _messageMediaContextAction(msgData)"));
    assert!(lxmf.contains("function _resolveMessageImageFile(msgData)"));
    assert!(lxmf.contains("function _resolveMessageAttachmentFile(att)"));
    assert!(lxmf.contains("var mediaAction = _messageMediaContextAction(msgData);"));
    assert!(lxmf.contains("_messageActionIcon(mediaAction ? mediaAction.icon : 'copy')"));
    assert!(lxmf.contains("mediaAction ? mediaAction.label : 'Copy'"));
    assert!(lxmf.contains("function _optimisticApplyReaction"));
    assert!(lxmf.contains("showToast(ok ? 'Message copied'"));
    assert!(gestures.contains("var preventDefaultOnStart = opts.preventDefaultOnStart || null;"));
    assert!(gestures.contains(
        "var touchStartOpts = preventDefaultOnStart ? { passive: false } : { passive: true };"
    ));
    assert!(emoji_picker.contains("btn.addEventListener('touchstart', function(e) { e.preventDefault(); }, { passive: false });"));
    assert!(messaging_css.contains(".lxmf-messages.msg-action-mode .msg-row"));
    assert!(messaging_css.contains(".msg-row.msg-action-selected .lxmf-msg"));
    assert!(messaging_css.contains("position: fixed; z-index: calc(var(--z-modal) + 3);"));
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
    assert!(animations.contains("bottom: calc(62px + var(--sab) + 20px);"));
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
    assert!(index.contains("title=\"Import or restore identity\""));
    assert!(index.contains(r#"<path d="M7 10l5 5 5-5"/>"#));
    assert!(index.contains("M2.6 17.4A2 2 0 0 0 2 18.8V21"));
    let identity_nav_start = index
        .find(r#"<a class="nav-item" data-view="identity""#)
        .expect("identity nav item");
    let identity_nav_rest = &index[identity_nav_start + 1..];
    let identity_nav_end = identity_nav_rest
        .find(r#"<a class="nav-item""#)
        .map(|offset| identity_nav_start + 1 + offset)
        .unwrap_or(index.len());
    let identity_nav = &index[identity_nav_start..identity_nav_end];
    assert!(identity_nav.contains("M2.6 17.4A2 2 0 0 0 2 18.8V21"));
    assert!(!identity_nav.contains(r#"<circle cx="7.5" cy="15.5" r="5.5""#));
    assert!(!index.contains(r#"<circle cx="7.5" cy="15.5" r="5.5""#));
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
    assert!(identity_js.contains("PIN-encrypted .rsi identity backup"));
    assert!(identity_js.contains("function openRatspeakBackupImportPinModal"));
    assert!(identity_js.contains("function openEncryptedIdentityExportModal"));
    assert!(identity_js.contains("passcode: importPasscode"));
    assert!(identity_js.contains("passcode: passcode || ''"));
    assert!(identity_js.contains("protectIdentityWithPasscode(data.hash, importPasscode)"));
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
    assert!(!identity_js.contains("navigator.share({ files"));
    assert!(!identity_js.contains("Identity backup ready"));
    assert!(!identity_js.contains("Export Backup"));
    assert!(identity_js.contains("function openIdentityActions(hash)"));
    assert!(identity_js.contains("function deleteIdentityByHash(hash)"));
    assert!(identity_js.contains("id=\"identity-change-pin-detail-btn\""));
    assert!(identity_js.contains("Change PIN"));
    assert!(identity_js.contains("function viewActiveRecoveryPhrase()"));
    assert!(identity_js.contains("var active = activeIdentity();"));
    assert!(identity_js.contains("viewRecoveryPhrase(active);"));
    assert!(identity_js.contains("function exportActiveIdentity()"));
    assert!(
        identity_js
            .contains("exportIdentityBackup((active && active.hash) || activeIdentityHash);")
    );
    assert!(identity_js.contains("function openHardwareChangePinModal"));
    assert!(identity_js.contains("RS.invoke('hw_change_pin', { hash: target.hash"));
    assert!(identity_js.contains("M2.6 17.4A2 2 0 0 0 2 18.8V21"));

    let active_card_start = identity_js
        .find("function renderActiveIdentityCard()")
        .expect("active identity card renderer");
    let active_card_tail = &identity_js[active_card_start..];
    let active_card_end = active_card_tail
        .find("function renderIdentityList()")
        .expect("active card renderer end");
    let active_card_source = &active_card_tail[..active_card_end];
    assert!(!active_card_source.contains("id=\"identity-export-detail-btn\""));
    assert!(!active_card_source.contains("id=\"identity-view-phrase-btn\""));

    let actions_start = identity_js
        .find("function openIdentityActions(hash)")
        .expect("identity actions renderer");
    let actions_tail = &identity_js[actions_start..];
    let actions_end = actions_tail
        .find("// Add or change a passcode")
        .expect("identity actions renderer end");
    let actions_source = &actions_tail[..actions_end];
    assert!(!actions_source.contains("value: 'export'"));
    assert!(!actions_source.contains("value: 'view-phrase'"));

    let dialogs_js = read_source(root.join("dashboard/static/js/dialogs.js")).expect("dialogs js");
    assert!(dialogs_js.contains("built.sheet.addEventListener('keydown'"));
    assert!(!dialogs_js.contains("built.overlay.addEventListener('keydown'"));
    assert!(dialogs_js.contains("title.classList.add('bottom-sheet-title-with-icon');"));
    assert!(dialogs_js.contains("icon.className = 'rs-dialog-choice-icon';"));
    assert!(dialogs_js.contains("text.appendChild(hint);"));

    let index = read_source(root.join("dashboard/index.html")).expect("index html");
    assert!(index.contains("Identity Management"));
    assert!(index.contains("Identity Detail"));
    assert!(!index.contains("id=\"identity-export-btn\""));
    assert!(!index.contains("identity-panel-actions"));
    assert!(index.contains(r#"data-settings-panel="panel-settings-identity""#));
    assert!(index.contains(r#"id="panel-settings-identity""#));
    assert!(index.contains(r#"id="settings-active-identity-desc""#));
    assert!(index.contains(r#"id="settings-identity-status-desc""#));
    assert!(index.contains(r#"id="settings-edit-status-btn""#));
    assert!(index.contains(r#"id="settings-clear-status-btn""#));
    assert!(index.contains(r#"id="settings-backup-identity-btn""#));
    assert!(index.contains(r#"id="settings-view-recovery-phrase-btn""#));
    let general_nav = index
        .find(r#"data-settings-panel="panel-settings-general""#)
        .unwrap();
    let identity_nav = index
        .find(r#"data-settings-panel="panel-settings-identity""#)
        .unwrap();
    let privacy_nav = index
        .find(r#"data-settings-panel="panel-settings-privacy""#)
        .unwrap();
    assert!(general_nav < identity_nav && identity_nav < privacy_nav);

    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    assert!(settings_js.contains("function settingsCurrentActiveIdentity()"));
    assert!(settings_js.contains("function syncSettingsIdentityActions()"));
    assert!(settings_js.contains("settings-backup-identity-btn"));
    assert!(settings_js.contains("settings-view-recovery-phrase-btn"));
    assert!(settings_js.contains("viewActiveRecoveryPhrase();"));
    assert!(settings_js.contains("settings-edit-status-btn"));
    assert!(settings_js.contains("settings-clear-status-btn"));
    assert!(settings_js.contains("saveIdentityStatus('')"));
    assert!(
        settings_js.contains("window.syncSettingsIdentityActions = syncSettingsIdentityActions;")
    );

    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    assert!(views_css.contains(".identity-page-header"));
    assert!(views_css.contains(".identity-management-grid"));
    assert!(views_css.contains(".identity-detail-hero"));
    assert!(views_css.contains(".identity-address-row"));
    assert!(views_css.contains(".identity-detail-actions"));
    assert!(views_css.contains(".selector-badge:disabled"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    assert!(responsive_css.contains(".identity-toolbar-btn span"));
    assert!(responsive_css.contains("display: none;"));

    let modals_css =
        read_source(root.join("dashboard/static/css/08-modals.css")).expect("modals css");
    assert!(modals_css.contains(".bottom-sheet-title-with-icon"));
    assert!(modals_css.contains(".bottom-sheet-title-icon"));
    assert!(modals_css.contains(".rs-dialog-choice"));
    assert!(modals_css.contains(".rs-dialog-choice-icon"));
    assert!(modals_css.contains("flex-direction: column;"));
    assert!(modals_css.contains("gap: var(--space-3);"));
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
    assert!(tauri_lib.contains("hw_change_pin"));
    assert!(tauri_lib.contains("unlock_identity"));

    let identity_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/identity.rs"))
        .expect("identity command source");
    assert!(identity_rs.contains("identity duplicate check db task panicked"));
    assert!(identity_rs.contains("Identity already exists"));
    assert!(identity_rs.contains("base32-private-key"));
    assert!(identity_rs.contains("api_export_identity_reticulum_base64"));
    assert!(identity_rs.contains("api_export_identity_reticulum_base32"));
    assert!(identity_rs.contains("pub async fn unlock_identity"));
    assert!(identity_rs.contains("pub(crate) async fn unlock_protected_identity"));
}

#[test]
fn hardware_new_identity_reset_flow_handles_initialized_keys() {
    let root = repo_root();
    let identity_js =
        read_source(root.join("dashboard/static/js/identity.js")).expect("identity js");
    let hardware_rs =
        read_source(root.join("crates/ratspeak-runtime/src/hardware.rs")).expect("hardware rs");

    assert!(identity_js.contains("function _hwConfirmOverwriteIfNeeded"));
    assert!(identity_js.contains("title: 'Reset this security key?'"));
    assert!(identity_js.contains("RS.invoke('hw_reset_piv')"));
    assert!(identity_js.contains("function _hwIsFactoryDefaultPinError"));
    assert!(identity_js.contains("function _hwRecoverNonFactoryPinForProvision"));
    assert!(identity_js.contains("_hwRecoverNonFactoryPinForProvision(msg);"));
    assert!(identity_js.contains("? 'Enter your PIN to continue.'"));
    assert!(identity_js.contains(r#"placeholder="PIN""#));
    assert!(!identity_js.contains(r#"placeholder="Passcode""#));
    assert!(identity_js.contains("msg = 'Incorrect PIN.';"));
    assert!(!identity_js.contains("title: 'Overwrite this key?'"));
    assert!(!identity_js.contains("confirmText: 'Overwrite'"));
    assert!(identity_js.contains("RS.invoke('hw_change_pin', { hash: target.hash"));

    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    assert!(views_css.contains(".hw-unlock-input:focus::placeholder { color: transparent; }"));
    assert!(views_css.contains("stroke-linecap: round;"));

    assert!(hardware_rs.contains("not at the factory default"));
    assert!(hardware_rs.contains("Reset the security key to set up a new Ratspeak identity"));
    assert!(hardware_rs.contains("pub fn change_pin("));
    assert!(hardware_rs.contains("Inserted YubiKey does not match this identity"));
}

#[test]
fn software_identity_creation_uses_passcode_and_backup_acknowledgement_flow() {
    let root = repo_root();
    let identity_js =
        read_source(root.join("dashboard/static/js/identity.js")).expect("identity js");
    let setup_js = read_source(root.join("dashboard/static/js/setup.js")).expect("setup js");
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let modals_css =
        read_source(root.join("dashboard/static/css/08-modals.css")).expect("modals css");
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");

    assert!(identity_js.contains("function identityPasscodeOptionHtml"));
    assert!(identity_js.contains("identityPasscodeOptionHtml('identity-create')"));
    assert!(identity_js.contains("bindIdentityPasscodeOption('identity-create')"));
    assert!(identity_js.contains("readIdentityPasscodeOption('identity-create')"));
    assert!(identity_js.contains("function protectIdentityWithPasscode"));
    assert!(identity_js.contains("RS.invoke('set_identity_passcode'"));
    assert!(identity_js.contains("RS.invoke('unlock_identity', { secret: secret })"));
    assert!(!identity_js.contains("RS.invoke('hw_unlock'"));
    assert!(identity_js.contains("identityPasscodeOptionHtml('restore-phrase')"));
    assert!(identity_js.contains("} else {\n            restore();\n        }"));

    assert!(identity_js.contains("Tap to reveal phrase"));
    assert!(
        identity_js.contains("I have written down my ' + RECOVERY_PHRASE_WORDS + '-word phrase")
    );
    assert!(identity_js.contains("id=\"recovery-backup-cover\""));
    assert!(identity_js.contains("id=\"recovery-backup-copy\""));
    assert!(identity_js.contains("opts.requireConfirm !== false"));
    assert!(!identity_js.contains("function pickRecoveryVerifyPositions"));
    assert!(!identity_js.contains("function renderRecoveryVerifyFields"));
    assert!(!identity_js.contains("function validateRecoveryVerifyInputs"));
    assert!(!identity_js.contains("requireVerify"));
    assert!(!identity_js.contains("showVerifyStep"));
    assert!(!identity_js.contains("recovery-verify-fields"));
    assert!(identity_js.contains("passcodeProtected: !!passcode"));
    assert!(setup_js.contains("function showSetupRecoveryStep"));
    assert!(!setup_js.contains("function showSetupRecoveryVerifyStep"));
    assert!(!setup_js.contains("window.renderRecoveryVerifyFields"));
    assert!(!setup_js.contains("window.validateRecoveryVerifyInputs"));
    assert!(
        setup_js.contains("showSetupIdentityStep(document.getElementById('setup-step-backup'))")
    );
    assert!(setup_js.contains("showSetupRecoveryStep(data.mnemonic || '', genStep)"));
    assert!(index.contains(r#"id="setup-step-backup""#));
    assert!(!index.contains(r#"id="setup-step-backup-verify""#));
    assert!(!index.contains(r#"id="setup-verify-fields""#));
    assert!(!index.contains(r#"id="hw-step-verify""#));
    assert!(!index.contains(r#"id="hw-verify-fields""#));
    assert_eq!(index.matches(r#"class="setup-dot"#).count(), 4);
    assert_eq!(index.matches(r#"class="setup-dot active"#).count(), 1);

    assert!(modals_css.contains(".identity-passcode-option"));
    assert!(views_css.contains(".recovery-backup-card .hw-mnemonic-shell"));
    assert!(views_css.contains(".recovery-backup-copy"));
}

#[test]
fn identity_switch_refreshes_interface_state_without_stale_public_servers() {
    let root = repo_root();
    let health = read_source(root.join("dashboard/static/js/health.js")).expect("health js");
    let identity = read_source(root.join("dashboard/static/js/identity.js")).expect("identity js");
    let modals = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    let events =
        read_source(root.join("dashboard/static/js/tauri_events.js")).expect("tauri events js");
    let runtime_lib =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime lib");
    let identity_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/identity.rs"))
        .expect("identity command");

    assert!(health.contains("function clearNetworkInterfaceCaches"));
    assert!(health.contains("function applyNetworkInterfacePayload"));
    assert!(health.contains("window._hubInterfacesData = empty;"));
    assert!(identity.contains("RS.listen('identity_switching'"));
    assert!(identity.contains("clearNetworkInterfaceCaches({ render: true });"));
    assert!(identity.contains("clearConnectPublicPending();"));
    assert!(identity.contains("refreshConnectPublicServers(null, { force: true });"));
    assert!(modals.contains("function refreshConnectPublicServers(ifaces, opts)"));
    assert!(modals.contains("function resumePublicServerInterface(server, match)"));
    assert!(modals.contains("RS.invoke('resume_interface'"));
    assert!(
        modals.contains("!opts.force && (window._hubInterfacesData || window._cachedConfigIfaces)")
    );
    assert!(events.contains("'resume_interface': 'Resuming'"));
    assert!(
        events.contains("applyNetworkInterfacePayload(data, { render: isViewActive('network') });")
    );
    assert!(runtime_lib.contains("teardown_rns_runtime_interfaces(&mgr.handle).await;"));
    assert!(runtime_lib.contains("TransportQuery::GetInterfaceStats"));
    assert!(
        runtime_lib.contains("rns_runtime::reticulum::teardown_interface(handle, iface.id).await;")
    );
    assert!(identity_rs.contains(
        "let ifaces = crate::rns_config::get_all_interfaces(&active_rns_config_dir(&state));"
    ));
    assert!(identity_rs.contains("emit_hub_interfaces(&state, ifaces);"));
}

#[test]
fn activity_producers_are_sealed_and_legacy_rows_have_one_masked_source() {
    let root = repo_root();
    let activity_mod = read_source(root.join("crates/ratspeak-runtime/src/activity/mod.rs"))
        .expect("activity mod");
    let producer = read_source(root.join("crates/ratspeak-runtime/src/activity/producer.rs"))
        .expect("activity producer facade");
    let emitter = read_source(root.join("crates/ratspeak-runtime/src/activity/emitter.rs"))
        .expect("activity emitter");

    assert!(activity_mod.contains("mod catalog;"));
    assert!(!activity_mod.contains("pub mod catalog;"));
    assert!(activity_mod.contains("pub mod producer;"));
    assert!(!activity_mod.contains("pub use classified::{ActivityDraft"));
    assert!(producer.contains("pub struct ProducerEvent(Payload);"));
    assert!(!producer.contains("pub struct ActivityDraft"));
    assert!(!producer.contains("pub time:"));
    assert!(!producer.contains("pub kind:"));
    assert!(!producer.contains("pub summary:"));
    assert!(!producer.contains("pub classification:"));
    assert!(emitter.contains("fn from_masked(event: &ActivityEventV1)"));
    assert!(!emitter.contains("fn from_masked(event: &ActivityDraft)"));

    let lifecycle = read_source(root.join("crates/ratspeak-runtime/src/activity/lifecycle.rs"))
        .expect("activity lifecycle");
    let admission = lifecycle
        .find("let Some(lease) = self.inner.shared.gate.try_admit()")
        .expect("recorder admission gate");
    let origin_validation = lifecycle
        .find("if !validate_origin()")
        .expect("origin validation under admission");
    let producer_build = lifecycle
        .find("let mut draft = match make()")
        .expect("lazy producer construction");
    assert!(admission < origin_validation);
    assert!(origin_validation < producer_build);

    for relative in [
        "crates/ratspeak-runtime/src/lib.rs",
        "crates/ratspeak-runtime/src/voice.rs",
        "crates/ratspeak-tauri/src/commands/interface_activity.rs",
        "crates/ratspeak-tauri/src/commands/messaging.rs",
        "crates/ratspeak-tauri/src/commands/network.rs",
    ] {
        let source = read_source(root.join(relative)).expect(relative);
        assert!(
            !source.contains(".record_event("),
            "async-capable migrated adapter bypasses its origin fence in {relative}"
        );
        assert!(
            source.contains("record_event_fenced("),
            "migrated adapter has no fenced Activity producer in {relative}"
        );
    }

    let runtime =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime lib");
    assert!(runtime.contains("pub async fn send_announce_from_origin("));
    assert!(runtime.contains("send_announce_from_origin(&state, activity_origin).await"));
    assert!(runtime.contains("biased;\n                        _ = tick_shutdown.wait()"));
    assert!(runtime.contains("biased;\n            _ = shutdown.wait() => break"));
    assert!(runtime.contains("let poll_activity_origin = state.activity_request_fence();"));
    assert!(
        runtime.contains("poll_stats_loop(poll_state, poll_shutdown, poll_activity_origin).await")
    );
    let poll_loop = runtime
        .split("async fn poll_stats_loop(")
        .nth(1)
        .and_then(|tail| tail.split("async fn ").next())
        .expect("poll stats loop body");
    assert!(poll_loop.contains("if shutdown.is_triggered()"));
    let poll_startup = poll_loop
        .split("let mut prev_online")
        .next()
        .expect("poll startup marker segment");
    assert!(!poll_startup.contains("activity_request_fence()"));
    assert!(poll_startup.contains("runtime_activity_origin"));
    let poll_unit = poll_loop.split("loop {").nth(1).expect("poll receive unit");
    let poll_select = poll_unit.find("tokio::select!").unwrap();
    let poll_origin = poll_unit
        .find("let poll_activity_origin = state.activity_request_fence();")
        .unwrap();
    let poll_shutdown = poll_unit.find("if shutdown.is_triggered()").unwrap();
    assert!(poll_select < poll_origin && poll_origin < poll_shutdown);

    let direct_inbound = runtime
        .split("async fn handle_inbound_lxmf(")
        .nth(1)
        .and_then(|tail| tail.split("enum InboundLxmfSource").next())
        .expect("direct inbound loop");
    let direct_select = direct_inbound.find("let event = tokio::select!").unwrap();
    let direct_origin = direct_inbound
        .find("let activity_origin = state.activity_request_fence();")
        .unwrap();
    let direct_shutdown = direct_inbound.find("if shutdown.is_triggered()").unwrap();
    assert!(direct_select < direct_origin && direct_origin < direct_shutdown);

    let link_inbound = runtime
        .split("let link_inbound_state = state.clone();")
        .nth(1)
        .and_then(|tail| tail.split("tracing::info!(").next())
        .expect("link inbound loop");
    let link_select = link_inbound
        .find("let (data, link_id) = tokio::select!")
        .unwrap();
    let link_origin = link_inbound
        .find("link_inbound_state.activity_request_fence()")
        .unwrap();
    let link_shutdown = link_inbound
        .find("if link_inbound_shutdown.is_triggered()")
        .unwrap();
    assert!(link_select < link_origin && link_origin < link_shutdown);

    let decrypt_from_origin = runtime
        .split("async fn handle_decrypted_lxmf_from_origin(")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("origin-bound decrypted inbound handler");
    assert!(!decrypt_from_origin.contains("activity_request_fence()"));
    assert!(decrypt_from_origin.contains("activity_origin: ActivityRequestFence"));

    let startup_announce = runtime
        .split("fn schedule_startup_auto_announce(")
        .nth(1)
        .and_then(|tail| {
            tail.split("async fn send_announce_from_state_inner(")
                .next()
        })
        .expect("startup auto-announce task");
    let startup_wait = startup_announce
        .find("_ = tokio::time::sleep(Duration::from_secs(2))")
        .unwrap();
    let startup_origin = startup_announce
        .find("let activity_origin = state.activity_request_fence();")
        .unwrap();
    let startup_shutdown = startup_announce.find("if shutdown.is_triggered()").unwrap();
    let startup_send = startup_announce
        .find("send_announce_from_origin(&state, activity_origin).await")
        .unwrap();
    let startup_success = startup_announce.find("if report.queued > 0").unwrap();
    let startup_fenced = startup_announce
        .find("record_activity_if_current(&state, activity_origin, ||")
        .unwrap();
    let startup_aggregate = startup_announce
        .find("method: producer::AnnounceMethod::Startup")
        .unwrap();
    assert!(
        startup_wait < startup_origin
            && startup_origin < startup_shutdown
            && startup_shutdown < startup_send
            && startup_send < startup_success
            && startup_success < startup_fenced
            && startup_fenced < startup_aggregate
    );

    let periodic_announce = runtime
        .split("// Auto-announce loop; wakes on timer or interval change.")
        .nth(1)
        .and_then(|tail| tail.split("let poll_state = state.clone();").next())
        .expect("periodic auto-announce loop");
    let periodic_wait = periodic_announce
        .find("tokio::time::sleep(Duration::from_secs(interval_secs))")
        .unwrap();
    let periodic_origin = periodic_announce
        .find("periodic_state.activity_request_fence()")
        .unwrap();
    let periodic_shutdown = periodic_announce
        .find("if periodic_shutdown.is_triggered()")
        .unwrap();
    let periodic_send = periodic_announce
        .find("send_announce_from_origin(")
        .unwrap();
    assert!(
        periodic_wait < periodic_origin
            && periodic_origin < periodic_shutdown
            && periodic_shutdown < periodic_send
    );

    for relative in [
        "crates/ratspeak-tauri/src/commands/games.rs",
        "crates/ratspeak-tauri/src/commands/identity.rs",
        "crates/ratspeak-tauri/src/commands/network.rs",
    ] {
        let source = read_source(root.join(relative)).expect(relative);
        assert!(source.contains("activity_request_fence()"));
        assert!(
            source.contains("_from_origin(") || source.contains("send_announce_from_origin("),
            "delayed announce path recaptures its Activity origin in {relative}"
        );
    }

    let mut production_sources = Vec::new();
    collect_files(
        &root.join("crates/ratspeak-runtime/src"),
        &mut production_sources,
    );
    collect_files(
        &root.join("crates/ratspeak-tauri/src"),
        &mut production_sources,
    );
    for path in production_sources
        .into_iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
    {
        let source = read_source(&path).expect("production Rust source");
        assert!(
            !source.contains("emit_network_event("),
            "legacy producer call remains in {}",
            path.display()
        );
        assert!(
            !source.contains(".add_event("),
            "generic legacy event producer remains in {}",
            path.display()
        );
        let compact = source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        for forbidden in [
            "emit_to_all(\"event\",",
            "emit_to_all(\"event_log\",",
            ".emit(\"event\",",
            ".emit(\"event_log\",",
            ".try_emit(\"event\",",
            ".try_emit(\"event_log\",",
        ] {
            assert!(
                !compact.contains(forbidden),
                "generic legacy event bus producer {forbidden} remains in {}",
                path.display()
            );
        }
    }

    let runtime_state =
        read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("runtime state");
    assert!(!runtime_state.contains("pub fn add_event("));
    assert!(!runtime_state.contains("legacy_activity_capture_enabled"));
    let mut dashboard_sources = Vec::new();
    collect_files(&root.join("dashboard/static/js"), &mut dashboard_sources);
    for path in dashboard_sources
        .into_iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("js"))
    {
        let source = read_source(&path).expect("dashboard JavaScript");
        let compact = source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        for forbidden in [
            "RS.listen('event',",
            "RS.listen('event_log',",
            "RS.listen(\"event\",",
            "RS.listen(\"event_log\",",
        ] {
            assert!(
                !compact.contains(forbidden),
                "generic legacy listener {forbidden} remains in {}",
                path.display()
            );
        }
    }
    let activity_frontend =
        read_source(root.join("dashboard/static/js/activity.js")).expect("activity frontend");
    assert!(!activity_frontend.contains("RS.listen('network_event',"));
    assert!(!activity_frontend.contains("RS.listen('network_log_level_changed',"));
    assert!(!activity_frontend.contains("typeof events !== 'undefined'"));
    let publish_start = emitter
        .find("fn try_publish(&self")
        .expect("typed Activity publisher");
    let publish_end = emitter[publish_start..]
        .find("fn try_publish_status")
        .map(|offset| publish_start + offset)
        .expect("typed status publisher");
    assert!(!emitter[publish_start..publish_end].contains("network_event"));
    let runtime_compact = runtime
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    for product_stream in ["stats_update", "system_status", "announce_received"] {
        assert!(
            runtime_compact.contains(&format!("emit_to_all(\"{product_stream}\",")),
            "product stream {product_stream} must remain independent of Activity"
        );
    }
    let health = read_source(root.join("dashboard/static/js/health.js")).expect("health js");
    assert!(health.contains("RS.listen('alert'"));
    assert!(health.contains("renderAlert(data)"));
}

#[test]
fn activity_bootstrap_is_listener_first_and_session_local() {
    let root = repo_root();
    let activity = read_source(root.join("dashboard/static/js/activity.js")).expect("activity js");
    let state = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    let identity = read_source(root.join("crates/ratspeak-tauri/src/commands/identity.rs"))
        .expect("identity commands");

    assert!(!activity.contains("localStorage"));
    assert!(!activity.contains("sessionStorage"));
    assert!(!activity.contains("enabled: false, level: activityLevel"));
    assert!(activity.contains("{ required: true }"));
    assert!(activity.contains("invoke('activity_status')"));
    assert!(activity.contains("invoke('activity_replay', {"));
    assert!(activity.contains("['activity_status_v1', handleStatusNotification]"));
    assert!(activity.contains("['activity_boundary_v1', handleBoundary]"));
    assert!(activity.contains("['activity_batch_v1', handleBatch]"));
    assert!(activity.contains("onEvents(state.events.slice()"));
    assert!(activity.contains("if (state.identityQuarantine) return;"));
    assert!(activity.contains("if (authoritative && state.identityQuarantine)"));
    assert!(activity.contains("payload.identity_generation !== state.identityGeneration"));
    assert!(activity.contains("after: after"));
    assert!(activity.contains("activityBootstrap.start();"));
    let controller_start = activity
        .find("var ACTIVITY_U64_MAX")
        .expect("Activity controller start");
    let controller_end = activity[controller_start..]
        .find("\nvar activityBootstrap =")
        .map(|offset| controller_start + offset)
        .expect("Activity controller end");
    let controller = &activity[controller_start..controller_end];
    assert!(!controller.contains("parseInt("));
    assert!(!controller.contains("BigInt("));
    assert!(!controller.contains("Number("));

    assert!(state.contains("options.required === true"));
    assert!(state.contains("err.code = 'event_bridge_unavailable'"));
    assert!(state.contains("rs-lifecycle-foreground-handled"));
    assert!(
        identity
            .matches(r#""generation": generation.to_string()"#)
            .count()
            >= 3
    );
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
    assert!(ui_shared_js.contains("var previousText = badge ? badge.textContent : '';"));
    assert!(ui_shared_js.contains("badge.textContent = previousText || 'OFF';"));
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
    assert!(interfaces_rs.contains("PUBLIC_TCP_TRANSPORT_CONNECT_LIMIT_MESSAGE"));
    assert!(interfaces_rs.contains("PUBLIC_TCP_TRANSPORT_ENABLE_LIMIT_MESSAGE"));
    assert!(interfaces_rs.contains("public_tcp_server_id"));
    assert!(interfaces_rs.contains("enabled_public_tcp_server_count"));
    assert!(interfaces_rs.contains("enforce_public_tcp_transport_connect_limit"));
    assert!(interfaces_rs.contains("projected_enabled_public_tcp_server_ids"));
    assert!(
        interfaces_rs.contains(
            "Transport Mode can't be enabled while connected to more than 1 public server."
        )
    );
    assert!(
        interfaces_rs
            .contains("Disable Transport Mode before connecting to more than 1 public server.")
    );
    assert!(interfaces_rs.contains("rns.ratspeak.org\", 4242, \"ratspeak-emerald"));
    assert!(interfaces_rs.contains("has_enabled_non_lora_transport_interface"));
    assert!(interfaces_rs.contains("reconcile_auto_transport_after_interface_change"));
    assert!(interfaces_rs.contains("transport_network_type"));
    assert!(interfaces_rs.contains("db::try_set_setting(&p, \"transport_mode\", &mode_for_db)?;"));
    assert!(interfaces_rs.contains("set_transport_mode db task panicked"));
    assert!(interfaces_rs.contains("configured_enabled"));
    assert!(interfaces_rs.contains("suppressed"));
    assert!(interfaces_rs.contains("InstanceMode::Client"));

    let shared_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/shared.rs"))
        .expect("shared source");
    let network_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/network.rs"))
        .expect("network source");
    assert!(shared_rs.contains("hub_interfaces_payload"));
    assert!(shared_rs.contains("persisted_transport_mode"));
    assert!(shared_rs.contains("config_transport_enabled(state)"));
    assert!(shared_rs.contains("\"transport\".to_string()"));
    assert!(shared_rs.contains("reconcile_auto_transport_after_interface_change"));
    assert!(network_rs.contains("hub_interfaces_payload"));

    let runtime_rs =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime source");
    assert!(
        runtime_rs.contains("reconcile_persisted_transport_mode_for_startup(&state, &config_dir);")
    );
    assert!(runtime_rs.contains("fn startup_auto_transport_enabled_for_interfaces"));
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
    assert!(gradle.contains("RustWebView.kt deprecation warning is not suppressed"));
    assert!(gradle.contains("WryActivity.kt deprecation warning is not suppressed"));
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
fn peer_transport_badges_use_compact_ble_label() {
    let root = repo_root();
    let peers = read_source(root.join("dashboard/static/js/peers.js")).expect("peers js");
    assert!(peers.contains("return 'BLE';"));
    assert!(!peers.contains("return 'Bluetooth Peer';"));

    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf.contains("function _peerCompactIfaceLabel(iface)"));
    assert!(lxmf.contains("return 'BLE';"));
    assert!(!lxmf.contains("return 'Bluetooth Peer';"));
}

#[test]
fn path_resolution_diagnostics_are_not_duplicate_or_stale() {
    let root = repo_root();

    let lxmf = read_source(root.join("crates/ratspeak-runtime/src/lxmf.rs")).expect("lxmf");
    let resolve_destination = lxmf
        .split("pub async fn resolve_destination")
        .nth(1)
        .expect("resolve destination fn");
    let resolve_destination = resolve_destination
        .split("// 5s tighter than transport's 15s for interactive responsiveness.")
        .next()
        .expect("resolve destination pre-timeout section");
    assert!(resolve_destination.contains("TransportMessage::AwaitPath"));
    assert!(
        !resolve_destination.contains("TransportMessage::RequestPath"),
        "AwaitPath already requests a path when none exists"
    );

    let handlers = read_source(root.join("crates/ratspeak-runtime/src/announce_handlers.rs"))
        .expect("announce handlers");
    assert!(handlers.contains("refresh_lxmf_route_cache_and_lookup_iface(state"));
    assert!(handlers.contains("mgr.replace_route_hops_from_path_table(entries);"));
    assert!(
        handlers.find("refresh_lxmf_route_cache_and_lookup_iface(state")
            < handlers.find("trigger_outbound_for_delivery_announce"),
        "delivery announce/path-response must refresh route cache before waking outbound"
    );

    let runtime = read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime");
    assert!(runtime.contains("\"held_announces\": e.held_announces"));
    assert!(runtime.contains("\"burst_active\": e.burst_active"));
    assert!(runtime.contains("PollActivityObservation::AnnounceIngressBurst"));
    assert!(runtime.contains("PollActivityObservation::AnnouncesHeld"));
    assert!(runtime.contains("for observation in activity_observations"));
    assert!(runtime.contains("record_poll_activity(&state, poll_activity_origin, observation)"));

    let rns = read_source(root.join("crates/ratspeak-runtime/src/rns.rs")).expect("rns");
    assert!(rns.contains("\"held_announces\": s.held_announces"));
    assert!(rns.contains("\"burst_active\": s.burst_active"));

    let network =
        read_source(root.join("crates/ratspeak-tauri/src/commands/network.rs")).expect("network");
    assert!(network.contains("dest_hash = dest_hash.to_ascii_lowercase();"));
    assert!(network.contains("async fn ingress_path_diagnostics"));
    assert!(
        network
            .contains("emit_ingress_diagnostics_snapshot(state.inner(), diagnostics_fence).await;")
    );
    assert!(network.contains("\"interfaces_holding_announces\""));
}

#[test]
fn conversation_header_presence_uses_peer_cache_status() {
    let root = repo_root();
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");

    assert!(lxmf.contains("function _peerPresenceClass(peer)"));
    assert!(lxmf.contains("var status = peer && peer.status ? peer.status : 'unknown';"));
    assert!(lxmf.contains("function _applyChatHeaderPresence()"));
    assert!(lxmf.contains("avatarEl.className = 'lxmf-chat-header-avatar' +"));
    assert!(lxmf.contains("statusEl.className = 'lxmf-chat-header-status' +"));
    assert!(lxmf.contains("if (convPeer) statusClass = _peerPresenceClass(convPeer);"));
    assert!(lxmf.contains("_refreshRenderedConversationPresence();"));
    assert!(lxmf.contains("_peerActivityLabel(peer)"));
    assert!(
        !lxmf.contains(
            "var statusOnline = !!(peer && peer.route_state && peer.route_state !== 'none')"
        ),
        "conversation header presence must not be derived from route/path state"
    );

    let css =
        read_source(root.join("dashboard/static/css/09-messaging.css")).expect("messaging css");
    assert!(css.contains(".lxmf-chat-header-status.is-stale"));
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

    let core_types =
        read_source(root.join("crates/ratspeak-core/src/types.rs")).expect("core types");
    assert!(core_types.contains("pub const LXMF_DELIVERY_APP_NAME: &str = \"lxmf.delivery\";"));

    let db = read_source(root.join("crates/ratspeak-db/src/db.rs")).expect("db");
    assert!(db.contains(
        "pub const PEER_SERVICE_LXMF_DELIVERY: &str = ratspeak_core::LXMF_DELIVERY_APP_NAME;"
    ));
    assert!(db.contains("pub const PEER_SERVICE_LXST_TELEPHONY: &str = \"lxst.telephony\";"));
    assert!(db.contains("fn peer_service_filter_sql(column: &str) -> String"));

    let handlers = read_source(root.join("crates/ratspeak-runtime/src/announce_handlers.rs"))
        .expect("handlers");
    assert!(handlers.contains("pub async fn spawn_lxst_telephony_handler"));
    assert!(handlers.contains("const LXST_TELEPHONY_ASPECT: &str = \"lxst.telephony\";"));
    assert!(handlers.contains("Destination::hash_from_name_and_identity(LXMF_DELIVERY_APP_NAME"));
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
