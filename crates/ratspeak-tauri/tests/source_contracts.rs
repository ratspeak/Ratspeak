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
fn channels_keep_hubs_live_only_and_wire_bounded_local_history_across_the_product() {
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
    let channel_hub = read_source(root.join("crates/ratspeak-runtime/src/channel_hub.rs"))
        .expect("channel hub runtime");
    let commands = read_source(root.join("crates/ratspeak-tauri/src/commands/channels.rs"))
        .expect("channels commands");
    let snapshot_order_test =
        read_source(root.join("dashboard/scripts/test_channels_snapshot_order.js"))
            .expect("channels snapshot ordering test");
    let history_test = read_source(root.join("dashboard/scripts/test_channels_history.js"))
        .expect("channels history test");
    let unread_test = read_source(root.join("dashboard/scripts/test_channels_unread.js"))
        .expect("channels unread test");
    let notification_route_test =
        read_source(root.join("dashboard/scripts/test_channels_notification_route.js"))
            .expect("channels notification route test");
    let share_test = read_source(root.join("dashboard/scripts/test_channels_share.js"))
        .expect("channels share test");
    let hub_switcher_test =
        read_source(root.join("dashboard/scripts/test_channels_hub_switcher.js"))
            .expect("channels hub switcher test");
    let hub_profile_test = read_source(root.join("dashboard/scripts/test_channels_hub_profile.js"))
        .expect("channels hub profile test");
    let tauri_events =
        read_source(root.join("dashboard/static/js/tauri_events.js")).expect("tauri event bridge");
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
    assert!(index.contains("id=\"channel-hub-summary\""));
    assert!(index.contains("data-channel-action=\"hub-actions\""));
    assert!(index.contains("id=\"channel-hub-menu-btn\" type=\"button\" title=\"Manage Hub\""));
    assert!(!index.contains("channel-hub-add-btn"));
    assert!(!index.contains("channel-hub-switcher-chevron"));
    assert!(!index.contains("channel-live-beacon"));
    assert!(index.contains("id=\"channel-owned-hub\""));
    assert!(index.contains("id=\"channel-owned-hub-manage\""));
    assert!(!index.contains("channels-refresh-btn"));
    assert!(index.contains("id=\"channel-members-back\""));
    assert!(!index.contains("Messages are not saved and disappear when this session ends."));
    assert!(!index.contains("id=\"channel-session-banner\""));
    assert_eq!(
        channels_js
            .matches("hub relays and can read channel messages")
            .count(),
        1,
        "hub-readability disclosure belongs in the one-time consent, not repeated channel chrome"
    );
    assert!(channels_js.contains("Ratspeak saves only identity-sealed ciphertext"));
    assert!(channels_js.contains("only after the hub confirms membership"));
    assert!(channels_js.contains("has_stored_join_key"));
    assert!(channels_js.contains("remember_key: !!key && rememberKey.checked"));
    assert!(channels_js.contains("keyInput.value = '';"));
    assert!(channels_js.contains("RS.listen('channels_snapshot'"));
    assert!(channels_js.contains("function _channelsSnapshotIsNewer"));
    assert!(channels_js.contains("incoming.generation > existing.generation"));
    assert!(channels_js.contains("incoming.revision > existing.revision"));
    assert!(channels_js.contains("RS.listen('announce_received'"));
    assert!(channels_js.contains("function _channelsBuildHubMark"));
    assert!(channels_js.contains("function _channelsHubDistance"));
    assert!(channels_js.contains("function channelsConnectToHub"));
    assert!(channels_js.contains("function _channelsHubSwitcherModel"));
    assert!(channels_js.contains("function _channelsHubConnectMode"));
    assert!(channels_js.contains("function channelsOpenHubSwitcher"));
    assert!(channels_js.contains("action === 'add' || action === 'manage-hub'"));
    assert!(channels_js.contains("action === 'hub-actions'"));
    assert!(channels_js.contains("channelsOpenHubOptions(actionEl)"));
    assert!(
        !channels_js.contains("hubSwitcher.addEventListener('click', channelsOpenHubSwitcher)")
    );
    assert!(channels_js.contains("One hub can be live at a time"));
    assert!(channels_js.contains("history stays on this device"));
    assert!(channels_js.contains("list.setAttribute('aria-live', 'polite')"));
    assert!(channels_js.contains("list.setAttribute('aria-busy', 'true')"));
    assert!(channels_js.contains("? 'Switch hub'"));
    assert!(channels_js.contains(": 'Choose a hub'"));
    assert!(channels_js.contains("switching: connectMode.kind === 'switch'"));
    assert!(channels_js.contains("'Could not switch channel hubs.'"));
    assert!(channels_js.contains("openedEpoch === _channelsHistoryEpoch"));
    assert!(
        channels_js.contains("openedGeneration === (Number(channelsSnapshot.generation) || 0)")
    );
    assert!(channels_js.contains("dismissHubSwitcher()"));
    assert!(channels_js.contains("channelHubOwnDestinationHash"));
    assert!(!channels_js.contains("hub.nearby ? 'Nearby' : 'Recent'"));
    assert!(channels_js.contains("TextEncoder"));
    assert!(channels_js.contains("channel-transition-card"));
    assert!(channels_js.contains("Messages unlock when the hub confirms your membership."));
    assert!(channels_js.contains("dataset.channelAction = 'retry-room'"));
    assert!(channels_js.contains("case 'joined': return 'Live';"));
    assert!(channels_js.contains("case 'reconnecting': return 'Reconnecting';"));
    assert_eq!(channels_js.matches("return 'Live'").count(), 1);
    assert!(channels_js.contains("function _channelsIdentityTone"));
    assert!(!channels_js.contains("function _channelsInitials"));
    assert!(channels_css.contains(".channel-hub-row-mark"));
    assert!(channels_css.contains(".channel-hub-row-distance"));
    assert!(channels_css.contains(".channel-hub-summary"));
    assert!(channels_css.contains(".channel-directory-section .channels-list-section-action"));
    assert!(channels_css.contains("@keyframes channelHubSignalLap"));
    assert!(channels_css.contains(".channel-hub-strip.link-arrived::before"));
    assert!(channels_css.contains(".channel-hub-switcher-list .channel-hub-row.current"));
    assert!(channels_css.contains(".channel-hub-switch-impact"));
    assert!(!channels_css.contains(".channel-connection-trust"));
    assert!(channels_js.contains("function _channelsBuildHubNotice"));
    assert!(channels_js.contains("function _channelsBuildHubGreeting"));
    assert!(channels_js.contains("function _channelsIsRemotePresenceItem"));
    assert!(channels_js.contains("if (_channelsIsRemotePresenceItem(item)) return;"));
    assert!(channels_js.contains("function _channelsGroupConsecutiveMessages"));
    assert!(channels_js.contains("function _channelsLoadHistory"));
    assert!(channels_js.contains("RS.invoke('api_channel_history'"));
    assert!(channels_js.contains("RS.invoke('api_channel_participants'"));
    assert!(channels_js.contains("function _channelsMemberRosterModel"));
    assert!(channels_js.contains("'Recently visible'"));
    assert!(channels_js.contains("'Seen here'"));
    assert!(!channels_js.contains("'Offline'"));
    assert!(channels_js.contains("before: older ? entry.next_before : null"));
    assert!(channels_js.contains("function _channelsSyncHistory"));
    assert!(channels_js.contains("after: after"));
    assert!(channels_js.contains("function _channelsTimelineEntries"));
    assert!(channels_js.contains("function _channelsRememberedRoomTopic"));
    assert!(!channels_js.contains("Saved locally \\u00b7 open"));
    assert!(!channels_js.contains("\\u00b7 stored on this device"));
    assert!(channels_js.contains("_channelsListSection('History'"));
    assert!(channels_js.contains("_channelsActiveEmpty('No currently active channels')"));
    assert!(channels_css.contains(".channel-active-empty"));
    assert!(channels_js.contains("function channelsSelectHistoryRoom"));
    assert!(channels_js.contains("function channelsApplyUnread"));
    assert!(channels_js.contains("RS.invoke('api_channel_unread')"));
    assert!(channels_js.contains("RS.invoke('mark_channel_room_read'"));
    assert!(channels_js.contains("RS.invoke('set_channel_room_notification_level'"));
    assert!(channels_js.contains("function channelsPrepareVisibleRead"));
    assert!(channels_js.contains("function _channelsInsertQuote"));
    assert!(channels_js.contains("function _channelsInsertMemberMention"));
    assert!(channels_js.contains("function channelsOpenNotificationRoute"));
    assert!(channels_js.contains("api_channel_room_index"));
    assert!(channels_js.contains("latest_recorded_at_ms"));
    assert!(!channels_js.contains("Local timeline"));
    assert!(channels_js.contains("Load earlier"));
    assert!(!channels_js.contains("localStorage.setItem"));
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
    assert!(responsive_css.contains("calc(64px + var(--sat))"));
    assert!(responsive_css.contains("body.view-channel-detail .main-content"));
    assert!(responsive_css.contains(".channels-layout.room-live.members-open"));
    assert!(responsive_css.contains("max(var(--space-4), var(--sab))"));
    assert!(responsive_css.contains("calc(var(--space-4) + var(--sar))"));
    assert!(responsive_css.contains("calc(var(--space-4) + var(--sal))"));
    assert!(responsive_css.contains("calc(var(--space-5) + var(--sar))"));
    assert!(responsive_css.contains("calc(var(--space-5) + var(--sal))"));
    assert!(responsive_css.contains("width: var(--touch-target);"));
    assert!(responsive_css.contains(
        ".channel-room-row-title,\n    .channel-hub-row-title {\n        font-size: var(--mobile-list-title-size);"
    ));
    assert!(responsive_css.contains(
        ".channel-event-text {\n        color: var(--text-primary);\n        font-size: 1rem;"
    ));
    assert!(responsive_css.contains(".channel-system-event {\n        margin: var(--space-6) 0;"));
    assert!(channels_css.contains(".channel-members-scrim"));
    assert!(channels_css.contains(".channels-layout:not(.room-live)"));
    assert!(channels_css.contains(".channel-transition-rail"));
    assert!(channels_css.contains(".channel-hub-greeting"));
    assert!(channels_css.contains(".channel-hub-home"));
    assert!(channels_css.contains(".channel-hub-profile-capabilities"));
    assert!(!channels_css.contains(".channel-hub-greeting-delivery"));
    assert!(channels_css.contains(".channel-event.message-group-start"));
    assert!(channels_css.contains(".channel-event.message-group-middle"));
    assert!(channels_css.contains(".channel-event.message-group-end"));
    assert!(!channels_css.contains(".channel-presence-event"));
    assert!(!channels_css.contains(".channel-presence-summary"));
    assert!(channels_css.contains(".channel-history-rail"));
    assert!(channels_css.contains(".channel-day-separator"));
    assert!(channels_css.contains(".channel-member-detail-fields"));
    assert!(channels_css.contains(".channel-unread-badge"));
    assert!(channels_css.contains(".channel-event.mentioned"));
    assert!(channels_css.contains(".channel-quote-button"));
    assert!(channels_js.contains("layout.classList.remove('members-open')"));
    assert!(build_css.contains("09-channels.css"));
    assert!(tauri_build.contains(r#""09-channels.css""#));

    assert!(nav_js.contains("var MOBILE_TAB_SLOTS = ['peers', 'message', 'channels', 'more'];"));
    assert!(
        nav_js
            .contains("var MORE_VIEWS = ['contacts', 'identity', 'network', 'games', 'settings'];")
    );
    assert!(!nav_js.contains("if (viewId === 'channels') return 'message';"));
    assert!(nav_js.contains("function setMessageUnreadSource"));
    assert!(nav_js.contains("var _messageUnreadSources = { direct: 0, channels: 0 };"));
    assert!(
        index.contains(
            "<button class=\"bottom-bar-item\" type=\"button\" data-view=\"channels\" aria-label=\"Channels\">"
        )
    );
    assert!(index.contains("id=\"bb-channels-unread\""));
    assert!(
        index.contains(
            "<button class=\"bottom-sheet-item\" type=\"button\" data-view=\"contacts\">"
        )
    );
    assert!(!index.contains("data-message-mode="));
    assert!(nav_js.contains("item.setAttribute('aria-current', 'page')"));
    assert!(nav_js.contains("'Channels' + (_messageUnreadSources.channels > 0"));
    let keyboard_detection = nav_js
        .split("function initKeyboardDetection()")
        .nth(1)
        .and_then(|tail| tail.split("function initTextareaAutoGrow()").next())
        .expect("keyboard detection function");
    assert!(keyboard_detection.contains("'view-chat-detail'"));
    assert!(keyboard_detection.contains("'view-channel-detail'"));
    assert!(keyboard_detection.contains("keyboardOpen && inConversationDetail"));
    assert!(tauri_events.contains("function _decodeChannelNotificationRoute"));
    assert!(tauri_events.contains("window.channelsOpenNotificationRoute"));

    assert!(runtime.contains("Observed room membership remains session-scoped"));
    assert!(runtime.contains("bounded client-local append log"));
    assert!(runtime.contains("never routed through the"));
    assert!(runtime.contains("LXMF conversation store"));
    assert!(runtime.contains("TRANSCRIPT_LIMIT"));
    assert!(runtime.contains("pub struct ChannelsHistorySnapshot"));
    assert!(runtime.contains("HISTORY_COMMAND_BUFFER"));
    assert!(runtime.contains("ChannelHistoryCommand::Barrier"));
    assert!(runtime.contains("pub async fn flush_history"));
    assert!(runtime.contains("command_tx.try_send"));
    assert!(runtime.contains("db::append_channel_history_events"));
    assert!(runtime.contains("fn contains_exact_mention"));
    assert!(runtime.contains("fn channel_text_mentions"));
    assert!(runtime.contains("NativeNotification::channel"));
    assert!(runtime.contains("4_000_000"));
    assert!(runtime.contains("JOIN_CONFIRM_TIMEOUT"));
    assert!(runtime.contains("apply_rrcd_room_status_notice"));
    assert!(runtime.contains("parse_rrcd_room_status"));
    assert!(runtime.contains("hub-attested observation of its source"));
    assert!(runtime.contains("room.members_complete = false"));
    assert!(runtime.contains("pub struct ChannelHubGreetingSnapshot"));
    assert!(runtime.contains("pub hub_greeting: Option<ChannelHubGreetingSnapshot>"));
    assert!(runtime.contains("pub greeting: Option<ChannelHubGreetingSnapshot>"));
    assert!(runtime.contains("HUB_GREETING_RESOURCE_MAX_BYTES: usize = 16 * 1024"));
    assert!(runtime.contains("handle_hub_greeting_resource_offer"));
    assert!(runtime.contains("apply_hub_greeting_resource_completion"));
    assert!(runtime.contains("offer.total_segments() == 1"));
    assert!(runtime.contains("pub generation: u64"));
    assert!(runtime.contains("pub revision: u64"));
    assert!(runtime.contains("pub const CHANNELS_CONNECTION_BUDGET: usize = 1"));
    assert!(runtime.contains("pub service_model_version: u16"));
    assert!(runtime.contains("pub selected_hub_destination: Option<String>"));
    assert!(runtime.contains("pub struct ChannelHubDesiredSnapshot"));
    assert!(runtime.contains("pub struct ChannelHubObservedSnapshot"));
    assert!(runtime.contains("pub struct ChannelHubDurableSnapshot"));
    assert!(runtime.contains("pub struct ChannelHubRecoverySnapshot"));
    assert!(runtime.contains("RECONNECT_MAX_DELAY"));
    assert!(runtime.contains("RECONNECT_STABLE_RESET"));
    assert!(runtime.contains("prepare_auto_rejoin"));
    assert!(!runtime.contains("\"Reconnected to hub\""));
    assert!(runtime.contains("nickname_only_join"));
    assert!(channels_js.contains("function _channelsIsConnectionLifecycleItem"));
    assert!(channels_js.contains("item.text === 'Reconnected to hub'"));
    assert!(runtime.contains("ROOM_SECRET_SEAL_SCHEME"));
    assert!(runtime.contains(".encrypt(&plaintext, None)"));
    assert!(runtime.contains("complete_pending_join_secret"));
    assert!(runtime.contains("SAVED_ROOM_KEY_REJECTED"));
    assert!(runtime.contains("ROOM_KEY_REQUIRED"));
    assert!(runtime.contains("pub has_stored_join_key: bool"));
    assert!(runtime.contains("pub join_key_required: bool"));
    assert!(runtime.contains("db::set_channel_hub_desired"));
    assert!(runtime.contains("db::set_channel_room_desired"));
    assert!(runtime.contains("snapshot.revision = revision.saturating_add(1)"));
    assert!(runtime.contains("WELCOME source does not match the authenticated hub"));
    assert!(commands.contains("fn parse_local_composer_command"));
    assert!(runtime.contains("pub async fn connect_known"));
    assert!(runtime.contains("channel hub identity does not match its destination"));
    assert!(channel_hub.contains("public identity data and is never serialized"));
    assert!(channel_hub.contains("pub fn public_key(&self) -> [u8; 64]"));
    assert!(commands.contains("Identity::from_file(&identity_path)"));
    assert!(commands.contains(".connect_known("));
    assert!(commands.contains("LocalComposerCommand::Join"));
    assert!(commands.contains("LocalComposerCommand::Part"));
    assert!(commands.contains("pub remember_key: bool"));
    assert!(commands.contains(".join_with_key_policy("));
    assert!(commands.contains("\"/list\""));
    assert!(
        commands
            .matches("\"snapshot\": channels.snapshot()")
            .count()
            >= 4
    );
    assert!(commands.matches("\"snapshot\": snapshot").count() >= 2);
    assert!(
        snapshot_order_test.contains("a delayed API batch must not overwrite a newer live event")
    );
    assert!(snapshot_order_test.contains("an older manager generation can never supersede"));
    assert!(
        snapshot_order_test.contains("direct joins must not start the stale multi-query reload")
    );
    assert!(history_test.contains("the opaque 64-bit cursor must remain a string"));
    assert!(history_test.contains("receive sequence, not peer timestamps"));
    assert!(history_test.contains("forward catch-up closes the gap"));
    assert!(history_test.contains("previous identity epoch must be discarded"));
    assert!(unread_test.contains("background rooms must never be marked read"));
    assert!(unread_test.contains("dedicated mobile Channels badge"));
    assert!(notification_route_test.contains("invalid UTF-8 must never reach navigation"));
    assert!(notification_route_test.contains("must never reconnect or carry a room key"));
    for command in [
        "api_channels",
        "api_channel_history",
        "api_channel_participants",
        "api_channel_unread",
        "mark_channel_room_read",
        "set_channel_room_notification_level",
        "discover_channel_hubs",
        "refresh_channel_directory",
        "api_channel_share",
        "api_preview_channel_share",
        "connect_channel_hub",
        "disconnect_channel_hub",
        "join_channel",
        "part_channel",
        "send_channel_message",
        "api_saved_channel_hubs",
        "api_saved_channel_rooms",
        "api_channel_room_index",
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
    assert!(channel_schema.contains("desired_connected"));
    assert!(channel_schema.contains("desired_joined"));
    assert!(channel_schema.contains("join_key_required"));
    assert!(channel_schema.contains("CREATE TABLE IF NOT EXISTS channel_room_secrets"));
    assert!(channel_schema.contains("ciphertext           BLOB NOT NULL"));
    assert!(channel_schema.contains("idx_channel_hubs_identity_desired"));
    assert!(db.contains("CREATE TABLE IF NOT EXISTS channel_history"));
    assert!(db.contains("recorded_at_ms"));
    assert!(db.contains("idx_channel_history_room_sequence"));
    assert!(db.contains("idx_channel_history_identity_sequence"));
    assert!(db.contains("CHANNEL_HISTORY_MAX_EVENTS_PER_ROOM"));
    assert!(db.contains("CHANNEL_HISTORY_MAX_EVENTS_PER_IDENTITY"));
    assert!(db.contains("CHANNEL_HISTORY_MAX_EVENTS_GLOBAL"));
    assert!(db.contains("CHANNEL_HISTORY_MAX_PAYLOAD_BYTES_PER_ROOM"));
    assert!(db.contains("CHANNEL_HISTORY_MAX_PAYLOAD_BYTES_PER_IDENTITY"));
    assert!(db.contains("CHANNEL_HISTORY_MAX_PAYLOAD_BYTES_GLOBAL"));
    assert!(db.contains("CREATE TABLE IF NOT EXISTS channel_history_room_usage"));
    assert!(db.contains("channel_history_usage_after_insert"));
    assert!(db.contains("channel_history_usage_after_delete"));
    assert!(db.contains("pub struct ChannelHistoryPage"));
    assert!(db.contains("pub fn list_channel_history_after"));
    assert!(db.contains("pub next_after: Option<String>"));
    assert!(db.contains("pub struct ChannelRoomIndexEntry"));
    assert!(db.contains("pub fn list_channel_room_index"));
    assert!(db.contains("pub topic: Option<String>"));
    assert!(db.contains("mentioned             INTEGER NOT NULL DEFAULT 0"));
    assert!(db.contains("CREATE TABLE IF NOT EXISTS channel_room_state"));
    assert!(db.contains("pub struct ChannelUnreadSummary"));
    assert!(db.contains("pub fn mark_channel_room_read"));
    assert!(db.contains("pub fn set_channel_room_notification_level"));
    assert!(db.contains("pub fn get_channel_unread_summary"));
    assert!(channels_js.contains("service_model_version: 3"));
    assert!(channels_js.contains("connection_budget: 1"));
    assert!(channels_js.contains("selected_hub_destination: null"));
    assert!(channels_js.contains("durability: {"));
    assert!(channels_js.contains("directory: {"));
    assert!(channels_js.contains("RS.invoke('refresh_channel_directory')"));
    assert!(commands.contains(r#"CHANNEL_SHARE_SCHEME: &str = "ratspeak""#));
    assert!(commands.contains(r#"CHANNEL_SHARE_HOST: &str = "channel""#));
    assert!(commands.contains("CHANNEL_SHARE_MAX_BYTES: usize = 230"));
    assert!(commands.contains("Channel share contains an unsupported field"));
    assert!(commands.contains("target.payload != payload"));
    assert!(channels_js.contains("RS.invoke('api_channel_share'"));
    assert!(channels_js.contains("RS.invoke('api_preview_channel_share'"));
    assert!(channels_js.contains("previewCommand: 'api_preview_channel_share'"));
    assert!(channels_js.contains("preserve_pending_share: true"));
    assert!(channels_css.contains(".channel-share-qr-shell"));
    assert!(channels_css.contains(".channel-share-input"));
    assert!(share_test.contains("channel share tests passed"));
    assert!(hub_switcher_test.contains("channel hub switcher tests passed"));
    assert!(hub_profile_test.contains("channel hub profile tests passed"));
}

#[test]
fn native_channel_share_lifecycle_uses_rust_inbox_and_requires_preview() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let channels =
        read_source(root.join("dashboard/static/js/channels.js")).expect("channels frontend");
    let bridge = read_source(root.join("dashboard/static/js/native_channel_share.js"))
        .expect("native channel-share bridge");
    let bridge_test = read_source(root.join("dashboard/scripts/test_channels_native_link.js"))
        .expect("native channel-share test");
    let native =
        read_source(root.join("src-tauri/src/channel_deep_link.rs")).expect("native Rust bridge");
    let lib = read_source(root.join("src-tauri/src/lib.rs")).expect("Tauri entry point");
    let cargo = read_source(root.join("src-tauri/Cargo.toml")).expect("Tauri manifest");
    let base_config: serde_json::Value = serde_json::from_str(
        &read_source(root.join("src-tauri/tauri.conf.json")).expect("base Tauri config"),
    )
    .expect("valid base Tauri config");
    let android_config: serde_json::Value = serde_json::from_str(
        &read_source(root.join("src-tauri/tauri.android.conf.json")).expect("Android Tauri config"),
    )
    .expect("valid Android Tauri config");
    let ios_config: serde_json::Value = serde_json::from_str(
        &read_source(root.join("src-tauri/tauri.ios.conf.json")).expect("iOS Tauri config"),
    )
    .expect("valid iOS Tauri config");
    assert!(base_config["plugins"]["deep-link"].is_null());
    assert!(android_config["plugins"]["deep-link"].is_null());
    assert!(ios_config["plugins"]["deep-link"].is_null());
    for platform in ["linux", "macos", "windows"] {
        let config: serde_json::Value = serde_json::from_str(
            &read_source(
                root.join("src-tauri")
                    .join(format!("tauri.{platform}.conf.json")),
            )
            .expect("desktop Tauri config"),
        )
        .expect("valid desktop Tauri config");
        assert_eq!(
            config["plugins"]["deep-link"]["desktop"]["schemes"],
            serde_json::json!(["ratspeak"])
        );
        assert!(config["plugins"]["deep-link"]["mobile"].is_null());
    }

    let android_manifest =
        read_source(root.join("src-tauri/gen/android/app/src/main/AndroidManifest.xml"))
            .expect("Android manifest");
    let ios_info = read_source(root.join("src-tauri/gen/apple/ratspeak_iOS/Info.plist"))
        .expect("iOS Info.plist");
    let ios_entitlements =
        read_source(root.join("src-tauri/gen/apple/ratspeak_iOS/ratspeak_iOS.entitlements"))
            .expect("iOS entitlements");
    assert!(android_manifest.contains(r#"android:scheme="ratspeak""#));
    assert!(android_manifest.contains(r#"android:host="channel""#));
    assert!(android_manifest.contains("android.intent.category.BROWSABLE"));
    assert!(ios_info.contains("<key>CFBundleURLTypes</key>"));
    assert!(ios_info.contains("<string>ratspeak</string>"));
    assert!(ios_entitlements.contains("Multicast Networking entitlement"));
    assert!(!ios_entitlements.contains("com.apple.developer.associated-domains"));
    assert!(cargo.contains(r#"tauri-plugin-deep-link = "2.4.9""#));
    assert!(
        cargo.contains(
            r#"tauri-plugin-single-instance = { version = "2", features = ["deep-link"] }"#
        )
    );

    let single_instance = lib
        .find(".plugin(tauri_plugin_single_instance::init")
        .expect("single-instance plugin");
    let deep_link = lib
        .find(".plugin(tauri_plugin_deep_link::init())")
        .expect("deep-link plugin");
    let notification = lib
        .find(".plugin(tauri_plugin_notification::init())")
        .expect("notification plugin");
    assert!(
        single_instance < deep_link && single_instance < notification,
        "single-instance must be the first plugin for secondary-process URLs"
    );
    assert!(lib.contains("channel_deep_link::NativeChannelShareInbox::default()"));
    assert!(lib.contains("channel_deep_link::take_native_channel_share"));
    assert!(lib.contains("channel_deep_link::install(app)"));

    assert!(native.contains("Mutex<Option<ChannelShareTarget>>"));
    assert!(native.contains("parse_channel_share_target(payload)"));
    assert!(native.contains("app.emit(NATIVE_CHANNEL_SHARE_AVAILABLE, ())"));
    assert!(native.contains("app.deep_link().on_open_url"));
    assert!(native.contains("app.deep_link().get_current()"));
    assert!(!native.contains("std::fs"));
    assert!(!native.contains("localStorage"));

    let mut capability_files = Vec::new();
    collect_files(&root.join("src-tauri/capabilities"), &mut capability_files);
    let capabilities = capability_files
        .iter()
        .map(|path| read_source(path).expect("capability source"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !capabilities.contains("deep-link:"),
        "the deep-link plugin command API must not be callable from JavaScript"
    );

    let channels_pos = index
        .find("/static/js/channels.js")
        .expect("channels script");
    let bridge_pos = index
        .find("/static/js/native_channel_share.js")
        .expect("native channel-share bridge");
    let events_pos = index
        .find("/static/js/tauri_events.js")
        .expect("general Tauri event bridge");
    assert!(channels_pos < bridge_pos && bridge_pos < events_pos);
    assert!(channels.contains("function channelsOpenNativeSharedChannel(target)"));
    assert!(channels.contains("_channelsPresentSharedTarget(target);"));
    assert!(channels.contains("hasOwnProperty.call(target, 'key')"));
    assert!(channels.contains("hasOwnProperty.call(target, 'join_key')"));

    assert!(bridge.contains("RS.invoke('take_native_channel_share')"));
    assert!(bridge.contains("'native_channel_share_available'"));
    assert!(bridge.contains("_isSetupActive()"));
    assert!(bridge.contains(".bottom-sheet.open"));
    assert!(bridge.contains(".modal-overlay.active"));
    assert!(bridge.contains(".game-modal-overlay"));
    assert!(bridge.contains(".block-list-overlay"));
    assert!(bridge.contains("#rs-image-viewer.open"));
    assert!(bridge.contains(".action-popover.open"));
    assert!(bridge.contains("MutationObserver"));
    assert!(!bridge.contains("deep-link://new-url"));
    assert!(!bridge.contains("localStorage"));
    assert!(!bridge.contains("connect_channel_hub"));
    assert!(!bridge.contains("join_channel"));
    assert!(bridge_test.contains("native channel link tests passed"));
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
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");
    let admin_ui_test = read_source(root.join("dashboard/scripts/test_channel_hub_admin.js"))
        .expect("channel hub admin UI tests");
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
        "channel_hub::api_channel_hub_admin",
        "channel_hub::channel_hub_admin_mutate",
        "channel_hub::channel_hub_start",
        "channel_hub::channel_hub_stop",
        "channel_hub::set_channel_hosting_enabled",
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
        6,
        "every hosting-specific hub command must reject mobile hosting"
    );
    assert!(runtime.contains("channel_hub_hosting_supported()"));

    // Desktop hosting becomes discoverable from the Channels add action after
    // explicit Settings opt-in, stays separate from client session state, and
    // obeys backend support.
    assert!(index.contains("/static/js/channel_hub.js"));
    assert!(hub_ui.contains("RS.invoke('api_channel_hub')"));
    assert!(hub_ui.contains("RS.invoke('api_channel_hub_admin')"));
    assert!(hub_ui.contains("RS.invoke('channel_hub_start')"));
    assert!(hub_ui.contains("RS.invoke('channel_hub_stop')"));
    assert!(hub_ui.contains("RS.invoke('channel_hub_set_config'"));
    assert!(hub_ui.contains("RS.listen('channel_hub_snapshot'"));
    assert!(hub_ui.contains("function channelHubRenderHome"));
    assert!(hub_ui.contains("overview.supported && _channelHubHostingEnabled(overview)"));
    assert!(hub_ui.contains(
        "return !!(overview && overview.supported && _channelHubHostingEnabled(overview));"
    ));
    assert!(hub_ui.contains("function channelHubOpenOwnHub"));
    assert!(hub_ui.contains("overview.created"));
    assert!(hub_ui.contains("Some channel changes are still waiting to be saved."));
    assert!(hub_ui.contains("copyAddress.hidden = !destination"));
    assert!(channels_css.contains(".channel-host-admin-sheet"));
    assert!(channels_css.contains(".channel-host-admin-tabs"));
    assert!(channels_css.contains(".channel-host-admin-timeline"));
    assert!(channels_css.contains(".channel-host-admin-edit-sheet"));
    assert!(channels_css.contains(".channel-host-admin-editor-section"));
    assert!(channels_css.contains("unicode-bidi: plaintext"));
    assert!(responsive_css.contains(".channel-host-admin-metrics"));
    assert!(responsive_css.contains(".channel-host-admin-edit-sheet"));
    assert!(channels_css.contains(".channel-host-registry-warning"));
    assert!(channels_css.contains(".channel-host-copy-btn[hidden]"));
    assert!(channels_css.contains(".channel-owned-hub-card"));

    // Owner projections are pull-only, identity-fenced, and rendered as text.
    // No hub-provided nickname, topic, or excerpt may become HTML or browser
    // persistence, and evidence does not poll in the background.
    let admin_renderers = hub_ui
        .split("function _channelHubAdminNode")
        .nth(1)
        .and_then(|tail| tail.split("\nfunction channelHubOpenManager").next())
        .expect("Admin Center renderers");
    for forbidden in ["innerHTML", "localStorage", "sessionStorage", "setInterval"] {
        assert!(
            !admin_renderers.contains(forbidden),
            "Admin Center renderers must not contain `{forbidden}`"
        );
    }
    let admin_manager = hub_ui
        .split("function channelHubOpenManager")
        .nth(1)
        .and_then(|tail| tail.split("\nfunction channelHubOpenOwnHub").next())
        .expect("Admin Center manager");
    assert!(admin_manager.contains("Number(nextAdmin.model_version) !== 1"));
    assert!(admin_manager.contains("nextAdmin.evidence_policy.persistent !== false"));
    assert!(admin_manager.contains("request !== adminRequest"));
    assert!(admin_manager.contains("_channelHubManagerSequence !== sequence"));
    assert!(admin_manager.contains("_channelHubIdentityGeneration === identityGeneration"));
    assert!(admin_manager.contains("RS.invoke('channel_hub_admin_mutate', { args: args })"));
    assert!(admin_manager.contains("mutationError.code === 'registry_unavailable'"));
    assert!(admin_manager.contains("adminMutationBusy"));
    assert!(!admin_manager.contains("setInterval"));
    assert!(!admin_manager.contains("localStorage"));
    for action in [
        "create_channel",
        "update_channel",
        "unregister_channel",
        "set_room_role",
        "set_room_ban",
        "set_invitation",
        "kick",
        "set_hub_ban",
    ] {
        assert!(
            admin_renderers.contains(&format!("'{action}'")),
            "Admin Center must project typed `{action}` intents"
        );
    }
    assert!(admin_renderers.contains("secretMutation.join_key = '';"));
    assert!(admin_renderers.contains("mutation.join_key = '';"));
    assert!(admin_renderers.contains("autocomplete = 'new-password'"));
    assert!(admin_renderers.contains("fixedLiveName && keyConfigured"));
    assert!(admin_renderers.contains("!!modes.join_key_configured"));
    assert!(admin_renderers.contains("roomInput.value.trim().toLowerCase();"));
    assert!(admin_renderers.contains("cancel.textContent = 'Close and review'"));
    assert!(!admin_renderers.contains("action: '/"));
    assert!(!hub_ui.contains("Recent context, not a transcript"));
    assert!(!hub_ui.contains("Memory-only and incomplete"));
    assert!(!hub_ui.contains("Policy is durable. Conversation traffic is not."));
    assert!(hub_ui.contains("Recent activity is off"));
    assert!(hub_ui.contains("recent_activity_retention_secs"));
    assert!(hub_ui.contains("[86400, '24 hours']"));
    assert!(hub_ui.contains("At startup and on this schedule, so nearby people can find it"));
    for interval in [
        "[900, 'Every 15 minutes']",
        "[1800, 'Every 30 minutes']",
        "[3600, 'Every hour']",
        "[43200, 'Every 12 hours']",
        "[86400, 'Every 24 hours']",
    ] {
        assert!(hub_ui.contains(interval));
    }
    for removed in [
        "Operating limits",
        "Large welcome messages",
        "Large room notices",
        "[0, 'When started']",
        "[300, 'Every 5 min']",
        "[21600, 'Every 6 hours']",
    ] {
        assert!(!hub_ui.contains(removed));
    }
    assert!(hub_ui.contains("var _channelHubIdentityGeneration = 0;"));
    assert!(hub_ui.contains("identityGeneration !== _channelHubIdentityGeneration"));
    assert!(hub_ui.contains("_channelHubIdentityGeneration += 1;"));
    assert!(hub_ui.contains("dismissManager();"));
    assert!(hub_ui.contains("_channelHubAdminDismissChildren();"));
    assert!(admin_ui_test.contains("channel hub Admin Center tests passed"));

    // Configuration reads independently of live state, writes as one SQLite
    // transaction, and serializes every lifecycle mutation.
    assert!(commands.contains("pub struct ChannelHubOverview"));
    assert!(commands.contains("pub created: bool"));
    assert!(commands.contains("pub destination_hash: Option<String>"));
    assert!(runtime.contains("channel_hub::hub_identity_path"));
    assert!(commands.contains("ChannelHubSettings::load"));
    assert!(commands.contains("valid_channel_hub_announce_interval_secs"));
    assert!(commands.contains("try_set_settings"));
    assert!(commands.contains("hub.status()"));
    assert!(commands.contains("hub.admin_snapshot()"));
    assert!(commands.contains("HubStore::new"));
    assert!(commands.contains("let status = current_snapshot(state).await"));
    assert!(commands.contains("existing_hub_destination_hash(&identity_path)"));
    assert!(db.contains("pub fn try_set_settings"));
    assert!(db.contains("let transaction = conn.transaction()"));
    assert!(state.contains("pub channel_hub_control_lock: tokio::sync::Mutex<()>"));
    for command in [
        "pub async fn api_channel_hub_admin",
        "pub async fn channel_hub_admin_mutate",
        "pub async fn channel_hub_start",
        "pub async fn channel_hub_stop",
        "pub async fn set_channel_hosting_enabled",
        "pub async fn channel_hub_set_config",
    ] {
        let body = commands
            .split(command)
            .nth(1)
            .expect("hub lifecycle command");
        assert!(body.contains("channel_hub_control_lock.lock().await"));
    }

    // Owner evidence is an explicit pull through the actor, bounded in live
    // memory, and kept off the content-free Activity/event path.
    assert!(hub.contains("HubCommand::AdminSnapshot"));
    assert!(hub.contains("result_tx.send(core.admin_snapshot())"));
    assert!(hub.contains("CHANNEL_HUB_EVIDENCE_RETENTION_DEFAULT_SECS: u64 = 0"));
    assert!(hub.contains("valid_evidence_retention_secs"));
    assert!(hub.contains("CHANNEL_HUB_EVIDENCE_MAX_EVENTS"));
    assert!(hub.contains("CHANNEL_HUB_EVIDENCE_MAX_BYTES"));
    assert!(hub.contains("persistent: false"));
    assert!(hub.contains("struct HubEvidenceRecord"));
    assert!(hub.contains("Do not derive"));
}

#[test]
fn channel_hub_admin_mutations_are_actor_owned_and_durable_before_ack() {
    let root = repo_root();
    let hub = read_source(root.join("crates/ratspeak-runtime/src/channel_hub.rs"))
        .expect("channel hub runtime");
    let commands = read_source(root.join("crates/ratspeak-tauri/src/commands/channel_hub.rs"))
        .expect("channel hub commands");

    // Plaintext join-key input has no formatting or serialization surface and
    // becomes a zeroizing runtime value immediately at the IPC boundary.
    for declaration in [
        "pub struct ChannelHubAdminSecret",
        "pub enum ChannelHubAdminMutation",
    ] {
        let prefix = hub
            .split(declaration)
            .next()
            .expect("sensitive runtime declaration");
        let declaration_attributes = prefix.rsplit("\n\n").next().unwrap_or_default();
        assert!(
            !declaration_attributes.contains("derive("),
            "{declaration} must not derive formatting or serialization"
        );
    }
    let args_prefix = commands
        .split("pub enum ChannelHubAdminMutationArgs")
        .next()
        .expect("sensitive IPC declaration");
    let args_attributes = args_prefix.rsplit("\n\n").next().unwrap_or_default();
    assert!(!args_attributes.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("#[derive") && line.contains("Debug")
    }));
    assert!(hub.contains("pub struct ChannelHubAdminSecret(Zeroizing<String>);"));
    assert!(commands.contains("join_key.map(ChannelHubAdminSecret::new)"));
    assert!(commands.contains("let join_key = join_key.map(ChannelHubAdminSecret::new);"));

    // The Tauri command serializes with lifecycle changes, rejects stopped
    // mutation, requires a full identity hash, and never writes HubStore.
    let mutation_command = commands
        .split("pub async fn channel_hub_admin_mutate")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]").next())
        .expect("admin mutation command");
    assert!(mutation_command.contains("channel_hub_control_lock.lock().await"));
    assert!(mutation_command.contains("active_operator_identity(&state)"));
    assert!(mutation_command.contains("state.channel_hub_handle().ok_or_else"));
    assert!(mutation_command.contains("hub.admin_mutate(actor_identity, mutation)"));
    assert!(!mutation_command.contains("HubStore"));
    assert!(commands.contains("validate_hex(value, 32, 32)"));

    // Authorization and every state transition happen in HubCore. Durable
    // access state requires a registered room; live kick remains live-only;
    // server operators cannot be deopped, kicked, or banned.
    assert!(hub.contains("pub(crate) fn admin_mutate("));
    assert!(hub.contains("if !self.server_ops.contains(&actor_identity)"));
    assert!(hub.contains("fn require_registered_admin_room("));
    assert!(hub.contains("\"A server operator cannot be removed through a channel role\""));
    assert!(hub.contains("\"A server operator cannot be banned from a channel\""));
    assert!(hub.contains("\"A server operator cannot be kicked\""));
    assert!(hub.contains("\"A server operator cannot be banned from the hub\""));
    assert!(hub.contains("\"Invitations require an invite-only or join-key channel\""));

    // Local actions write only private evidence for their moderation/trust
    // intent; they do not fabricate a link-scoped Activity transition.
    let local_evidence = hub
        .split("fn note_admin_moderated")
        .nth(1)
        .and_then(|tail| tail.split("fn broadcast_admin_topic").next())
        .expect("local owner evidence helpers");
    assert!(local_evidence.contains("self.push_evidence("));
    assert!(!local_evidence.contains("self.events.push"));

    // The actor applies the intent, flushes its complete SQLite projection,
    // publishes state, and only then acknowledges with a fresh owner model.
    let run_hub = hub
        .split("async fn run_hub(")
        .nth(1)
        .and_then(|tail| tail.split("/// Every hub event").next())
        .expect("hub service loop");
    let applies = run_hub
        .find("core.admin_mutate(actor_identity, mutation, &mut out)")
        .expect("actor applies typed mutation");
    let flushes = run_hub
        .find("let persisted = flush_sends")
        .expect("actor flushes mutation");
    let replies = run_hub
        .find("if let Some((result_tx, result)) = admin_reply")
        .expect("actor replies after flush");
    let sends = run_hub
        .find("result_tx.send(result)")
        .expect("actor sends mutation result");
    assert!(applies < flushes && flushes < replies && replies < sends);
    assert!(run_hub.contains("ADMIN_ERROR_REGISTRY_UNAVAILABLE"));
    assert!(
        hub.contains("const ADMIN_ERROR_REGISTRY_UNAVAILABLE: &str = \"registry_unavailable\";")
    );
    assert!(run_hub.contains("Ok(()) => Ok(core.admin_snapshot())"));
    assert!(hub.contains("async fn flush_sends("));
    assert!(hub.contains(") -> bool {"));
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
    assert!(
        runtime.contains("matches!(*new_state, \"propagating\" | \"propagated\")")
            && runtime.contains(".then_some(\"propagated\".to_string())"),
        "an Auto fallback must persist the observable Propagated method before UI emission"
    );
    assert!(runtime.contains("producer::LxmfDeliveryState::Propagating"));
    assert!(runtime.contains("producer::LxmfDeliveryState::Propagated"));

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
    assert!(index.contains("Activity identity protection and presence sharing."));
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
fn incoming_lxmf_limit_setting_is_normal_default_on_and_backend_authoritative() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    assert!(index.contains("Limit incoming messages to 1 MB"));
    assert!(index.contains("id=\"lxmf-limit-1mb-toggle\" checked"));

    let settings = read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    assert!(settings.contains("data.lxmf_limit_1mb"));
    assert!(settings.contains("RS.invoke('set_lxmf_limit_1mb', { enabled: enabled })"));

    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    assert!(interfaces.contains("pub async fn set_lxmf_limit_1mb"));
    assert!(interfaces.contains("db::get_setting(&p, \"lxmf_limit_1mb\")"));
    assert!(interfaces.contains("state.set_lxmf_limit_1mb_enabled(enabled)"));

    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    assert!(tauri_lib.contains("set_lxmf_limit_1mb"));
}

#[test]
fn activity_identity_protection_is_default_on_durable_and_event_scoped() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let settings = read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let activity = read_source(root.join("dashboard/static/js/activity.js")).expect("activity js");
    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");

    assert!(index.contains("Protect Activity identities"));
    assert!(index.contains("id=\"settings-activity-identity-protection-on\" value=\"on\" checked"));
    assert!(settings.contains("set_activity_identity_protection"));
    assert!(settings.contains("adoptActivityIdentityProtectionFromBackend"));
    assert!(activity.contains("function activityRevealEvent(event)"));
    assert!(activity.contains("activityIdentityProtectionEnabled = true"));
    assert!(!activity.contains("activityRevealField(event, 'destination');"));
    assert!(interfaces.contains("pub async fn set_activity_identity_protection"));
    assert!(interfaces.contains("\"activity_identity_protection\""));
    assert!(interfaces.contains(".is_none_or(|value| value != \"false\")"));
    assert!(tauri_lib.contains("set_activity_identity_protection"));
}

#[test]
fn text_scale_presets_are_durable_and_backend_validated() {
    let root = repo_root();
    let settings = read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let scale = read_source(root.join("dashboard/static/js/text_scale.js")).expect("scale js");
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let views_css = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");

    assert!(settings.contains("RS.invoke('set_text_scale'"));
    assert!(settings.contains("data.text_scale_percent"));
    assert!(scale.contains("var MAX = 140"));
    assert!(interfaces.contains("pub async fn set_text_scale"));
    assert!(interfaces.contains("\"text_scale_percent\""));
    assert!(interfaces.contains("(percent.clamp(100, 140) + 5) / 10 * 10"));
    assert!(tauri_lib.contains("set_text_scale"));
    assert!(index.contains("/static/style.css?v=ui-20260813-2"));
    assert!(views_css.contains(".settings-theme-family-row > .settings-row-info"));
    assert!(views_css.contains("html[data-text-scale-tier=\"large\"] .settings-theme-family-row"));
    assert!(views_css.contains("justify-content: flex-start;\n    flex-wrap: nowrap;"));
    assert!(views_css.contains("html[data-text-scale-tier=\"xlarge\"] .settings-row"));
    assert!(!views_css.contains("html[data-text-scale-tier=\"xlarge\"] .settings-page-shell"));
}

#[test]
fn appearance_families_are_durable_validated_and_native_aware() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let theme = read_source(root.join("dashboard/static/js/theme.js")).expect("theme js");
    let settings = read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    let android = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("Android activity");

    assert!(index.contains("id=\"theme-family-picker\""));
    assert!(index.contains("id=\"theme-toggle\""));
    for family in [
        "ratspeak",
        "nord",
        "everforest",
        "gruvbox",
        "catppuccin",
        "rose-pine",
    ] {
        assert!(theme.contains(&format!("id: '{family}'")));
        assert!(interfaces.contains(&format!("\"{family}\" => Some(\"{family}\")")));
    }
    assert!(theme.contains("data-theme-family"));
    assert!(theme.contains("data-theme-preference"));
    assert!(theme.contains("ratspeak-theme-changed"));
    assert!(theme.contains("'rs-theme-family'"));
    assert!(!settings.contains("label.title = family.name"));
    assert!(settings.contains("'Use ' + family.name + ' theme'"));
    assert!(theme.contains("if (value === 'solarized') return 'everforest'"));
    assert!(settings.contains("RS.invoke('set_appearance'"));
    assert!(settings.contains("data.theme_family"));
    assert!(settings.contains("data.theme_mode"));
    assert!(interfaces.contains("pub async fn set_appearance"));
    assert!(interfaces.contains("db::try_set_settings("));
    assert!(interfaces.contains("\"solarized\" => Some(\"everforest\")"));
    assert!(interfaces.contains("pub fn set_native_theme"));
    assert!(tauri_lib.contains("set_appearance"));
    assert!(tauri_lib.contains("set_native_theme"));
    assert!(android.contains("fun setColorMode(mode: String)"));
    assert!(android.contains("applySystemBarColorMode(mode)"));
}

#[test]
fn mobile_shells_advertise_only_portrait_orientations() {
    let root = repo_root();
    let manifest = read_source(root.join("src-tauri/gen/android/app/src/main/AndroidManifest.xml"))
        .expect("android manifest");
    let ios_info = read_source(root.join("src-tauri/gen/apple/ratspeak_iOS/Info.plist"))
        .expect("iOS Info.plist");
    let ios_project =
        read_source(root.join("src-tauri/gen/apple/project.yml")).expect("iOS project source");

    assert!(manifest.contains("android:screenOrientation=\"portrait\""));
    assert!(manifest.contains("tools:ignore=\"DiscouragedApi,LockedOrientationActivity\""));
    assert!(ios_info.contains("UIInterfaceOrientationPortrait"));
    assert!(!ios_info.contains("UIInterfaceOrientationLandscape"));
    assert!(ios_project.contains("UISupportedInterfaceOrientations:"));
    assert!(ios_project.contains("UISupportedInterfaceOrientations~ipad:"));
    assert!(!ios_project.contains("UIInterfaceOrientationLandscape"));
    assert!(ios_project.contains("TARGETED_DEVICE_FAMILY: \"1,2\""));
}

#[test]
fn ble_rnode_runtime_spawns_enable_flow_control() {
    let root = repo_root();
    let ble_rs =
        read_source(root.join("crates/ratspeak-tauri/src/commands/ble.rs")).expect("ble commands");
    let interfaces_rs = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");

    let native_blocks = rust_struct_literal_blocks(&ble_rs, "BleRNodeInterfaceConfig");
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
        "rns_runtime::reticulum::spawn_android_usb_rnode_runtime_with_config_and_options",
        2,
    );
    assert_strict_calls(
        &interfaces_rs,
        "rns_runtime::reticulum::spawn_rnode_runtime_observed_with_options",
        2,
    );
    assert_strict_calls(
        &ble_rs,
        "rns_runtime::reticulum::spawn_ble_rnode_runtime_native_with_config_and_options",
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
            "rns_runtime::reticulum::spawn_android_usb_rnode_runtime_with_config_and_options",
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

    let bridge = rust_function_block(&ble, "apply_ble_rnode_bridge_ready");
    assert_eq!(
        rust_call_blocks(
            bridge,
            "rns_runtime::reticulum::spawn_ble_rnode_runtime_native_with_config_and_options"
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
    assert!(gatt.contains("sendRnodeDetachFallbackIfNeeded(\"explicit disconnect\")"));
    assert!(gatt.contains("if (rustDetachObserved.get()) return"));
    assert!(gatt.contains("fun forwardClientGenerations(listener: ServerSocket)"));
    assert!(gatt.contains("rustDetachObserved.set(false)"));
    assert!(gatt.contains("detachFrameMatch = 0"));
    assert!(gatt.contains("closeBridgeClient(accepted.socket)"));
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
    assert!(runtime_rs.contains("should_reannounce_for_interface_online("));
    assert!(runtime_rs.contains("auto_announce_interval > 0"));
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

    // The shell and Cargo builders must concatenate the same ordered modules
    // without a one-sided minification/rewrite pass.
    let build_css = read_source(root.join("dashboard/build-css.sh")).expect("build-css.sh");
    assert!(build_css.contains("MODULES=("));
    assert!(build_css.contains("for module in \"${MODULES[@]}\"; do"));
    assert!(build_css.contains("printf '\\n' >> \"$OUT\""));
    assert!(!build_css.contains("perl -0777 -pi"));
    assert!(!build_css.contains("sed -i"));

    // Every local optimistic image URL is escaped at the image render site.
    let lxmf_js = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf_js.contains("escapeHtml(localImageUrl)"));
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
fn android_native_release_lint_is_strict_and_api_guarded() {
    let root = repo_root();
    let gradle = read_source(root.join("src-tauri/gen/android/app/build.gradle.kts"))
        .expect("Android app Gradle source");
    let manifest = read_source(root.join("src-tauri/gen/android/app/src/main/AndroidManifest.xml"))
        .expect("Android manifest");
    let main_activity = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("Android MainActivity");
    let platform_supervisor = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakPlatformSupervisor.kt",
    ))
    .expect("Android platform supervisor");
    let service =
        read_source(root.join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakService.kt",
        ))
        .expect("Android service");
    let gatt =
        read_source(root.join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakBleGatt.kt",
        ))
        .expect("Android BLE GATT bridge");
    let release = read_source(root.join(".github/workflows/release-android.yml"))
        .expect("Android release workflow");

    assert!(gradle.contains("warningsAsErrors = true"));
    assert!(gradle.contains("abortOnError = true"));
    assert!(!gradle.contains("baseline ="));
    for deliberate_exclusion in [
        "AndroidGradlePluginVersion",
        "GradleDependency",
        "IconDuplicates",
    ] {
        assert!(gradle.contains(deliberate_exclusion));
    }
    for unsafe_exclusion in ["MissingPermission", "NewApi", "WakelockTimeout"] {
        assert!(!gradle.contains(unsafe_exclusion));
    }
    assert!(release.contains("./gradlew :app:lintArm64Release --warning-mode all"));

    assert!(manifest.contains(r#"android.hardware.touchscreen"#));
    assert!(manifest.contains(r#"android.hardware.wifi"#));
    assert!(manifest.contains(r#"android:banner="@mipmap/ic_launcher""#));
    assert!(manifest.contains(r#"android:roundIcon="@mipmap/ic_launcher_round""#));
    assert!(main_activity.contains("ContextCompat.startForegroundService(this, serviceIntent)"));
    assert!(platform_supervisor.contains("ContextCompat.registerReceiver("));
    assert!(platform_supervisor.contains("ContextCompat.RECEIVER_NOT_EXPORTED"));
    assert!(main_activity.contains("Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q"));
    assert_eq!(
        service
            .matches("if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return")
            .count(),
        3
    );
    assert!(gatt.contains("Manifest.permission.BLUETOOTH_CONNECT"));
    assert!(gatt.contains("ContextCompat.checkSelfPermission("));
    assert!(gatt.contains("catch (_: SecurityException)"));
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
        "@media (min-width: 769px) {\n    .bottom-sheet.open.games-new-dialog {\n        width: min(640px, calc(100vw - 48px));\n    }\n}"
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
fn games_ui_uses_runtime_manifests_and_accessible_atomic_actions() {
    let root = repo_root();
    let games_js = read_source(root.join("dashboard/static/js/games_tab.js")).expect("games js");
    let game_registry =
        read_source(root.join("dashboard/static/js/game_registry.js")).expect("game registry js");
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let games_css = read_source(root.join("dashboard/static/css/11-games.css")).expect("games css");

    assert!(games_js.contains("RS.invoke('get_available_games')"));
    assert!(games_js.contains("var _manifestsById = {};"));
    assert!(games_js.contains("RS.games.views.register('ttt'"));
    assert!(games_js.contains("RS.games.views.register('chess'"));
    assert!(games_js.contains("gameView.renderBoard(session, _gameViewContext(session, panel))"));
    assert!(games_js.contains("gameView.bindBoard(session, _gameViewContext(session, panel))"));
    assert!(games_js.contains("RS.games.views.supportedManifests("));
    assert!(games_js.contains("app_id: 'four_in_a_row', display_name: 'Four in a Row'"));
    assert!(games_js.contains("view.activeStatusText(session, _gameViewContext(session))"));
    assert!(games_js.contains("view.detailChips(session, _gameViewContext(session))"));
    assert!(games_js.contains("view.renderActiveControls(session)"));
    assert!(games_js.contains("view.bindControls(session, {"));
    assert!(!games_js.contains("if (appId === 'ttt') {\n            html += _renderTTTBoard"));
    assert!(game_registry.contains("function register(appId, adapter)"));
    assert!(game_registry.contains("function get(appId)"));
    assert!(game_registry.contains("function listIds()"));
    assert!(game_registry.contains("function supportedManifests(manifests)"));
    let registry_script = index.find("/static/js/game_registry.js").unwrap();
    let four_script = index.find("/static/js/four_in_a_row_view.js").unwrap();
    let games_script = index.find("/static/js/games_tab.js").unwrap();
    assert!(registry_script < four_script && four_script < games_script);
    assert!(games_js.contains("function _beginSessionAction(sessionId)"));
    assert!(games_js.contains("function _drawOfferOwner(session)"));
    assert!(games_js.contains("function _canDeleteSession(session)"));
    assert!(games_js.contains("function _handleGameActionFailure(data)"));
    assert!(games_js.contains("function _activeMoveDeliveryText(state)"));
    assert!(games_js.contains("escapeHtml(_statusText(s))"));
    assert!(games_js.contains("escapeHtml(RS.relativeTime(s.updated_at || s.last_action_at))"));
    assert!(games_js.contains("finishPromotion(null);"));
    assert!(games_js.contains("_isMe(session, _drawOfferOwner(session))"));
    assert!(games_js.contains("if (!_beginSessionAction(session.game_id)) return;"));
    assert!(games_js.contains("if (!_beginSessionAction(sid)) return;"));
    assert!(games_js.contains("RS.listen('game_protocol_error'"));
    assert!(games_js.contains("role=\"gridcell\""));
    assert!(games_js.contains("aria-label=\"Tic-Tac-Toe board\""));
    assert!(games_js.contains("function _chessPieceName(piece)"));
    assert!(games_css.contains(".ttt-cell:focus-visible"));
    assert!(games_css.contains(".chess-square:focus-visible"));
    assert!(games_css.contains(".four-lane-action:focus-visible"));
    assert!(games_css.contains("background: var(--surface-scrim);"));
    assert!(
        !games_css.contains(".game-modal"),
        "Games must use the shared bottom sheet instead of a parallel legacy modal"
    );
}

#[test]
fn four_in_a_row_view_is_accessible_theme_native_and_protocol_thin() {
    let root = repo_root();
    let games_js = read_source(root.join("dashboard/static/js/games_tab.js")).expect("games js");
    let registry =
        read_source(root.join("dashboard/static/js/game_registry.js")).expect("registry js");
    let four = read_source(root.join("dashboard/static/js/four_in_a_row_view.js"))
        .expect("four in a row view js");
    let css = read_source(root.join("dashboard/static/css/11-games.css")).expect("games css");

    assert!(four.contains("var APP_ID = 'four_in_a_row';"));
    assert!(four.contains("RS.games.views.register(APP_ID"));
    assert!(four.contains("var CELL_COUNT = ROWS * COLUMNS;"));
    assert!(four.contains("role=\"grid\" aria-label=\"Four in a Row board\""));
    assert!(four.contains("role=\"gridcell\""));
    assert!(four.contains("aria-rowindex=\""));
    assert!(four.contains("aria-colindex=\""));
    assert!(four.contains("class=\"four-lane-action\""));
    assert!(four.contains("role=\"group\" aria-label=\"Column drop controls\""));
    assert!(four.contains("context.sendMove({ c: column }"));
    assert!(!four.contains("payload: { board:"));
    assert!(!four.contains("payload: { turn:"));
    assert!(four.contains("fields: ['board', 'last_column', 'last_row', 'last_cell']"));
    assert!(four.contains("event.key === 'ArrowLeft'"));
    assert!(four.contains("event.key === 'ArrowRight'"));
    assert!(four.contains("event.key === 'Home'"));
    assert!(four.contains("event.key === 'End'"));
    assert!(games_js.contains("function _sendGameViewMove(session, payload, optimistic)"));
    assert!(games_js.contains("function _sessionValue(session, key, fallback)"));
    assert!(games_js.contains("_sessionValue(session, 'move_count', '')"));
    assert!(games_js.contains("_sessionValue(record, 'move_count', null)"));
    assert!(games_js.contains("_sessionValue(session, 'winner', '')"));
    assert!(games_js.contains("RS.games.optimistic.restoreFields(session, backup.adapter_fields)"));
    assert!(registry.contains("function sessionValue(session, key, fallback)"));
    assert!(registry.contains("RS.games.state = Object.freeze"));
    assert!(registry.contains("function captureFields(target, fields)"));
    assert!(registry.contains("function restoreFields(target, snapshot)"));
    assert!(css.contains(".four-token-a"));
    assert!(css.contains(".four-token-b"));
    assert!(css.contains(".four-win-trace line"));
    assert!(css.contains(".four-win-trace.animate line"));
    assert!(css.contains("@keyframes fourSettle"));
    assert!(css.contains("@media (prefers-reduced-motion: reduce)"));
}

#[test]
fn games_transport_uses_native_lxmf_fields_and_a_durable_outbox() {
    let root = repo_root();
    let lxmf = read_source(root.join("crates/ratspeak-runtime/src/lxmf.rs")).expect("lxmf rs");
    let games = read_source(root.join("crates/ratspeak-tauri/src/commands/games.rs"))
        .expect("games commands");
    let db = read_source(root.join("crates/ratspeak-db/src/db.rs")).expect("db source");
    let runtime =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime source");
    let state =
        read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("state source");

    assert!(lxmf.contains("apply_lrgp_fields_to_message"));
    assert!(lxmf.contains(".set_msgpack_field(field_id, bytes)"));
    assert!(games.contains("db::persist_outbound_game_action("));
    assert!(games.contains("db::rollback_outbound_game_action("));
    assert!(games.contains("reason = \"resend_required\";"));
    assert!(db.contains("pub fn persist_outbound_game_action("));
    assert!(db.contains("pub fn rollback_outbound_game_action("));
    assert!(!db.contains("INSERT OR REPLACE INTO app_actions"));
    assert!(runtime.contains("fn lrgp_sender_authenticated("));
    assert!(runtime.contains(".rollback_incoming("));
    assert!(runtime.contains(".forget_incoming_nonce("));
    assert!(runtime.contains("fn game_delivery_state_is_in_flight(state: &str)"));
    assert!(runtime.contains("sweep_stale_game_deliveries(&tick_state).await"));
    let proof_completion =
        rust_function_block(&runtime, "complete_authenticated_lxmf_delivery_proof");
    assert!(proof_completion.contains(".lrgp_msg_to_session"));
    assert!(proof_completion.contains("update_game_session_delivery_state("));
    assert!(state.contains("LrgpRouter::with_builtin_apps()"));
    assert!(!state.contains("register(Box::new(lrgp::apps::tictactoe"));
    for field in ["validation", "preferred_delivery", "ttl"] {
        assert!(games.contains(&format!("\"{field}\": manifest.{field}")));
    }
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
    let mobile_native =
        read_source(root.join("src-tauri/src/mobile_native.rs")).expect("mobile native bridge");

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
    assert!(shell.contains("mod mobile_native"));
    assert!(mobile_native.contains("nativeBleRnodeState"));
    assert!(mobile_native.contains("take_pending_ble_request(&activity_operation, generation)"));
    assert!(mobile_native.contains("apply_ble_rnode_bridge_failed("));
    assert!(mobile_native.contains("failure_code: native_ble_failure_code(&code)"));
    assert!(!events.contains("RS.invoke('ble_rnode_bridge_failed'"));

    let bridge_failure = ble
        .split("pub async fn apply_ble_rnode_bridge_failed")
        .nth(1)
        .and_then(|tail| tail.split("pub async fn cancel_ble_connect").next())
        .expect("typed native bridge failure command");
    let token_accept = bridge_failure
        .find("take_active_ble_rnode_activity_operation_with_completion")
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
        "mobile_hardware_state",
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
    let native_bridge = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakNativeBridge.kt",
    ))
    .expect("Android native bridge");
    let supervisor = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakPlatformSupervisor.kt",
    ))
    .expect("Android platform supervisor");

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
    assert!(ble.contains("apply_ble_rnode_bridge_ready("));
    assert_eq!(
        rust_call_blocks(
            &ble,
            "rns_runtime::reticulum::spawn_ble_rnode_runtime_native_with_config_and_options",
        )
        .len(),
        1
    );
    let readiness_calls = rust_call_blocks(&ble, "await_spawned_rnode_ready");
    assert_eq!(readiness_calls.len(), 1);
    assert!(readiness_calls[0].contains("rnode_activity_origin"));
    assert!(ble.contains("teardown_spawned_rnode_exact(&rns, &spawned)"));
    assert!(!ble.contains("online.load(std::sync::atomic::Ordering::SeqCst)"));

    assert!(interfaces.contains("start_or_replace_ble_rnode(NativeBleRnodeRequest"));
    assert!(interfaces.contains("activity_operation"));
    assert!(interfaces.contains("native_generation: context.origin().native_generation()"));
    assert!(!events.contains("ble_rnode_connect_native"));
    assert!(interfaces.contains("schedule_android_ble_rnode_operation_watchdog"));
    assert!(interfaces.contains("couple_android_ble_operation_to_rnode_lease"));
    assert!(interfaces.contains("begin_ble_rnode_activity_operation_owned"));
    assert!(interfaces.contains("begin_ble_rnode_activity_operation_with_completion_owned"));
    assert!(interfaces.contains("Duration::from_secs(180)"));
    assert!(interfaces.contains("take_pending_ble_rnode_activity_operation"));
    assert!(interfaces.contains("rollback_fresh_lora_add_marker"));
    assert!(interfaces.contains("RnodeActivityOutcome::SetupTimedOut"));
    assert!(interfaces.contains("Some(\"setup_timeout\")"));

    assert!(!events.contains("result.activity_operation !== activityOperation"));
    assert!(!events.contains("_BLE_RNODE_NATIVE_TIMEOUT_MS"));
    assert!(!events.contains("disconnectBleDeviceForOperation"));
    assert!(events.contains("RS.listen('mobile_hardware_state'"));
    assert!(!events.contains("ble_rnode_bridge_ready"));
    assert!(!events.contains("ble_rnode_bridge_failed"));

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

    assert!(!main_activity.contains("fun connectBleDevice("));
    assert!(!main_activity.contains("fun disconnectBleDeviceForOperation("));
    assert!(native_bridge.contains("fun startOrReplaceBleRnode("));
    assert!(native_bridge.contains("operationToken: String"));
    assert!(native_bridge.contains("installedGeneration: Long"));
    assert!(supervisor.contains("requestUsbPermissionForSelector"));
    assert!(native_bridge.contains("nativeBleRnodeState("));
    assert!(native_bridge.contains("operationToken"));
    assert!(native_bridge.contains("installedGeneration"));
    assert!(!gatt.contains("WebView"));
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
            "rns_runtime::reticulum::spawn_android_usb_rnode_runtime_with_config_and_options",
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
fn channel_hosting_is_an_explicit_durable_settings_capability() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let settings_js =
        read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let nav_js = read_source(root.join("dashboard/static/js/nav.js")).expect("navigation js");
    let hub_ui =
        read_source(root.join("dashboard/static/js/channel_hub.js")).expect("channel hub frontend");
    let channels_css =
        read_source(root.join("dashboard/static/css/09-channels.css")).expect("channels css");
    let commands = read_source(root.join("crates/ratspeak-tauri/src/commands/channel_hub.rs"))
        .expect("channel hub commands");
    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    let runtime_hub = read_source(root.join("crates/ratspeak-runtime/src/channel_hub.rs"))
        .expect("channel hub runtime");
    let runtime =
        read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime lifecycle");
    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("src-tauri lib");

    let general_nav = index
        .find(r#"data-settings-panel="panel-settings-general""#)
        .expect("General settings navigation");
    let channels_nav = index
        .find(r#"data-settings-panel="panel-settings-channels""#)
        .expect("Channels settings navigation");
    let identity_nav = index
        .find(r#"data-settings-panel="panel-settings-identity""#)
        .expect("Identity settings navigation");
    assert!(general_nav < channels_nav && channels_nav < identity_nav);
    assert!(index.contains(r#"id="panel-settings-channels""#));
    assert!(index.contains(r#"<html lang="en" data-channel-hosting="off">"#));
    assert!(index.contains(r#"role="radiogroup" aria-label="Channel hosting""#));
    assert!(index.contains(r#"id="settings-channel-hosting-desc" aria-live="polite""#));
    assert!(index.contains(
        r#"type="radio" name="settings-channel-hosting" id="settings-channel-hosting-off" value="off" checked"#
    ));
    assert!(index.contains(
        r#"type="radio" name="settings-channel-hosting" id="settings-channel-hosting-on" value="on""#
    ));

    assert!(settings_js.contains("function initChannelHostingToggle()"));
    assert!(settings_js.contains("RS.invoke('set_channel_hosting_enabled'"));
    assert!(settings_js.contains("adoptChannelHostingFromBackend(data.channel_hosting_enabled)"));
    assert!(settings_js.contains("var _settingsChannelHostingRequested = null;"));
    assert!(settings_js.contains("Stopping your hub and hiding hosting controls…"));
    assert!(settings_js.contains("document.documentElement.dataset.channelHosting"));
    assert!(settings_js.contains("channelHubRenderHome(channelHubOverview)"));
    let settings_lifecycle = nav_js
        .split("settings: function()")
        .nth(1)
        .and_then(|tail| tail.split("identity: function()").next())
        .expect("Settings view lifecycle");
    assert!(settings_lifecycle.contains("initChannelHostingToggle()"));
    assert!(channels_css.contains(r#"html[data-channel-hosting="off"] .channel-owned-hub"#));
    let hosting_toggle = settings_js
        .split("function setChannelHostingEnabled(enabled)")
        .nth(1)
        .and_then(|tail| tail.split("function initChannelHostingToggle").next())
        .expect("channel hosting toggle");
    assert!(!hosting_toggle.contains("_settingsChannelHostingEnabled = !!enabled;"));
    assert!(hosting_toggle.contains("RS.invoke('api_channel_hub')"));
    assert!(!settings_js.contains("ratspeak-channel-hosting"));
    assert!(hub_ui.contains("overview.supported && _channelHubHostingEnabled(overview)"));
    assert!(hub_ui.contains(
        "return !!(overview && overview.supported && _channelHubHostingEnabled(overview));"
    ));

    assert!(commands.contains("pub async fn set_channel_hosting_enabled"));
    assert!(commands.contains("CHANNEL_HOSTING_ENABLED_KEY"));
    assert!(commands.contains("CHANNEL_HOSTING_PREFERENCE_VERSION_KEY"));
    assert!(commands.contains("settings.enabled = false;"));
    assert!(commands.contains("hub.shutdown().await"));
    let preference_command = commands
        .split("pub async fn set_channel_hosting_enabled")
        .nth(1)
        .and_then(|tail| tail.split("#[tauri::command]").next())
        .expect("channel hosting preference command");
    let teardown = preference_command
        .find("shutdown_channel_hub(&state).await?")
        .expect("preference Off waits for hub teardown");
    let persist = preference_command
        .find("crate::db::try_set_settings")
        .expect("preference persistence");
    assert!(teardown < persist);
    assert!(commands.contains("ensure_hosting_enabled(&state"));
    assert!(interfaces.contains(r#""channel_hosting_enabled": channel_hosting_enabled"#));
    assert!(
        runtime_hub
            .contains("pub const CHANNEL_HOSTING_ENABLED_KEY: &str = \"channel_hosting_enabled\";")
    );
    assert!(runtime_hub.contains("pub const CHANNEL_HOSTING_PREFERENCE_VERSION_KEY: &str ="));
    assert!(runtime_hub.contains("channel_hub_enabled\".to_string(), \"0\".to_string()"));
    assert!(!runtime_hub.contains("legacy_hub_enabled"));
    assert!(runtime.contains("channel_hub::channel_hosting_enabled("));
    assert!(runtime.contains("reason = \"hosting_disabled\""));
    assert!(
        tauri_lib.contains("ratspeak_tauri::commands::channel_hub::set_channel_hosting_enabled")
    );
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
    assert!(health_js.contains("waitingForAndroidUsb"));
    assert!(health_js.contains("Waiting for USB"));
    assert!(health_js.contains("enabled && !waitingForAndroidUsb"));
    assert!(!health_js.contains("Display Name"));
    assert!(!health_js.contains("dangerDivider"));

    let modals_js = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");
    assert!(modals_js.contains("name: name || (host + ':' + port)"));
    assert!(modals_js.contains("if (!live || live.online === false) continue;"));
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
    assert!(interfaces_rs.contains("resolve_android_usb_runtime_selector"));
    assert!(interfaces_rs.contains("preflight_android_usb_selector_for_interface"));
    assert!(interfaces_rs.contains("request_android_usb_permission"));
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
    // Resume/update rollback is backend-owned and revision-guarded; only a
    // versioned marker installed by a fresh add may delete that add.
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
    assert!(modals_js.contains("var _RNODE_NEW_INTERFACE_MODE = 'roaming';"));
    assert!(modals_js.contains("var _RNODE_LEGACY_INTERFACE_MODE = 'full';"));
    assert!(modals_js.contains("if (!editIface) return _RNODE_NEW_INTERFACE_MODE;"));
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
    assert!(index.contains(r#"<option value="roaming" selected>Roaming (recommended)</option>"#));
    assert!(index.contains("Best for mobile radios."));
    assert!(
        rns_config_rs.contains(r#"pub const RNODE_NEW_INTERFACE_DEFAULT_MODE: &str = "roaming";"#)
    );
    assert!(
        rns_config_rs.contains(r#"pub const RETICULUM_DEFAULT_INTERFACE_MODE: &str = "full";"#)
    );
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
    assert!(!tauri_events_js.contains("ble_rnode_connect_native"));
    assert!(ble_rs.contains("native_mode"));
    assert!(ble_rs.contains("InterfaceMode::from_u8"));
    assert!(rns_runtime_rs.contains("pub mode: rns_interface::traits::InterfaceMode"));
    assert!(interfaces_rs.contains("mode: rnode_runtime_mode(mode)"));
    assert!(ble_rs.contains("BleRNodeInterfaceConfig"));
    assert!(ble_rs.contains("spawn_ble_rnode_runtime_native_with_config_and_options"));
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
    assert!(
        lxmf.contains("if (!_ghostConversationHash || _ghostConversationHash !== exitingHash)")
    );
    assert!(lxmf.contains("_activateConversation(null, 'left_conversation');"));
    assert!(lxmf.contains("if (_conversationHasVisibleMessages())"));
    assert!(lxmf.contains("_removeGhostRow();"));
    assert!(lxmf.contains("cacheDel(exitingHash);"));
    assert!(lxmf.contains("_activateConversation(null, 'left_conversation');"));
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
    let channels = read_source(root.join("dashboard/static/js/channels.js")).expect("channels js");
    let nav = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    let ui_shared =
        read_source(root.join("dashboard/static/js/ui_shared.js")).expect("shared ui js");
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
    assert!(
        lxmf.contains("function _finishLxmfComposerSend(input, shouldRestoreFocus, targetHash)")
    );
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
    assert!(
        send_function
            .contains("_finishLxmfComposerSend(input, shouldRestoreComposerFocus, targetHash);")
    );
    assert!(
        !send_function.contains("input.focus();"),
        "send must not unconditionally focus the composer after a button send"
    );
    assert!(ui_shared.contains("RS.composer.captureFocus = function(input)"));
    assert!(ui_shared.contains("RS.composer.consumeFocus = function(input)"));
    assert!(ui_shared.contains("RS.composer.focusWithoutScroll = function(input)"));
    assert!(ui_shared.contains("RS.composer.bindTapToSend = function(button, input, onSend)"));
    assert!(channels.contains("RS.composer.bindTapToSend(send, input, channelsSendMessage)"));
    assert!(channels.contains("var shouldRestoreComposerFocus = RS.composer"));
    assert!(channels.contains("RS.composer.consumeFocus(input)"));
    assert!(
        !channels
            .split("function channelsSendMessage()")
            .nth(1)
            .and_then(|tail| tail.split("function _channelsBindUI()").next())
            .expect("channel send function")
            .contains("input.focus();")
    );
    assert!(channels.contains("!event.isComposing && !isMobile()"));
    assert!(nav.contains("el.id === 'lxmf-input' || el.id === 'channel-message-input'"));
    assert!(nav.contains("document.getElementById('channel-transcript')"));

    let components_css =
        read_source(root.join("dashboard/static/css/07-components.css")).expect("css");
    assert!(components_css.contains(".message-composer-input {"));
    assert!(components_css.contains("overflow-y: auto;"));
    assert!(components_css.contains("scrollbar-width: none;"));
    assert!(components_css.contains("-webkit-appearance: none;"));
    assert!(components_css.contains(".message-composer-input::-webkit-scrollbar"));

    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("css");
    assert!(responsive_css.contains("overflow-y: auto;"));
    assert!(responsive_css.contains("scrollbar-width: none;"));
    assert!(responsive_css.contains("-webkit-appearance: none;"));
}

#[test]
fn conversation_view_scrolls_to_recent_messages_without_yanking_history() {
    let root = repo_root();
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    let channels = read_source(root.join("dashboard/static/js/channels.js")).expect("channels js");
    let nav = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");

    assert!(lxmf.contains("function _wireLxmfMessageScroll(container)"));
    assert!(lxmf.contains("function _captureLxmfMessageScrollState(container)"));
    assert!(lxmf.contains("function _scheduleLxmfScrollToBottom(container)"));
    assert!(
        lxmf.contains("function _applyLxmfMessageScrollAfterRender(container, state, options)")
    );
    assert!(lxmf.contains("function _watchLxmfImagesForBottomPin(container, shouldPin)"));
    assert!(lxmf.contains("var _lxmfMessageScrollStates = new WeakMap();"));
    assert!(lxmf.contains("state.followLatest = false;"));
    assert!(lxmf.contains("state.followLatest = true;"));
    assert!(lxmf.contains("RS.chatScroll.applyAfterRender = _applyLxmfMessageScrollAfterRender;"));
    assert!(channels.contains("RS.chatScroll.wire(transcript)"));
    assert!(channels.contains("RS.chatScroll.capture(transcript)"));
    assert!(channels.contains("RS.chatScroll.applyAfterRender(transcript, scrollState"));
    assert!(!channels.contains("_channelsTranscriptPinToken"));
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
    assert!(nav.contains("RS.chatScroll.nearBottom(msgContainer)"));
    assert!(nav.contains("RS.chatScroll.pinToBottom(msgContainer)"));
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
    assert!(lxmf.contains("function _chooseImageSize(file, inspection)"));
    assert!(lxmf.contains("RS.invoke('inspect_image_attachment_stage'"));
    assert!(lxmf.contains("RS.invoke('prepare_image_attachment_stage'"));
    assert!(lxmf.contains("meta: '~' + prettySize(estimate)"));
    assert!(lxmf.contains("Location and camera details are removed."));
    assert!(!lxmf.contains("createImageBitmap("));
    assert!(!lxmf.contains("document.createElement('canvas')"));
    assert!(lxmf.contains("pending-file-thumbnail"));
    assert!(lxmf.contains("pendingFile.preview_url = _imagePreviewUrl("));
    assert!(lxmf.contains("escapeHtml(lxmfPendingFile.preview_url || '')"));
    assert!(lxmf.contains("URL.revokeObjectURL(pending.preview_url)"));
    assert!(lxmf.contains("container.classList.toggle('pending-file-has-image', isImage);"));

    let messaging_css =
        read_source(root.join("dashboard/static/css/09-messaging.css")).expect("css");
    assert!(messaging_css.contains("#lxmf-pending-file.file-transfer-info"));
    assert!(messaging_css.contains(".pending-file-thumbnail img"));
    assert!(messaging_css.contains("object-fit: cover;"));
    assert!(messaging_css.contains(".pending-file-copy"));
}

#[test]
fn image_size_choices_are_bounded_shared_and_outcome_level() {
    let root = repo_root();
    let runtime = read_source(root.join("crates/ratspeak-runtime/src/image_attachment.rs"))
        .expect("image attachment runtime");
    let state =
        read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("runtime state");
    let messaging = read_source(root.join("crates/ratspeak-tauri/src/commands/messaging.rs"))
        .expect("messaging commands");
    let shell = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri shell");
    let dialogs = read_source(root.join("dashboard/static/js/dialogs.js")).expect("shared dialogs");
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("messaging js");

    assert!(runtime.contains("pub const IMAGE_SIZE_PROMPT_BYTES: usize = 1_000_000;"));
    assert!(runtime.contains("Self::Small => Some(250_000)"));
    assert!(runtime.contains("Self::Medium => Some(750_000)"));
    assert!(runtime.contains("Self::Large => Some(2_000_000)"));
    assert!(runtime.contains("pub const IMAGE_MAX_PIXELS: u64 = 16_000_000;"));
    assert!(runtime.contains("pub const IMAGE_PREVIEW_MAX_EDGE: u32 = 192;"));
    assert!(runtime.contains("Animated images must be sent as files"));
    assert!(runtime.contains("original.apply_orientation(orientation)"));
    assert!(state.contains("pub image_preparation_lock: tokio::sync::Mutex<()>"));
    assert!(state.contains("!staged.image_preparing"));
    assert!(state.contains("finish_staged_image_preparation"));
    assert!(messaging.contains("tokio::task::spawn_blocking(move ||"));
    assert!(messaging.contains("prepare_image_attachment("));
    assert!(messaging.contains("\"image_size_prompt_bytes\""));
    for command in [
        "inspect_image_attachment_stage",
        "prepare_image_attachment_stage",
        "mark_image_attachment_stage_as_file",
    ] {
        assert!(shell.contains(command));
    }
    assert!(dialogs.contains("function rsChoice(opts)"));
    assert!(dialogs.contains("opts.sheetClass"));
    assert!(dialogs.contains("rs-dialog-choice-meta"));
    assert!(lxmf.contains("sheetClass: 'image-size-sheet'"));
    assert!(lxmf.contains("title: 'Photo size'"));
    assert!(lxmf.contains("meta: '~' + prettySize(estimate)"));
    assert!(lxmf.contains("if (pendingAttachment.preparing)"));
    assert!(!lxmf.contains("createImageBitmap("));
    assert!(!lxmf.contains("document.createElement('canvas')"));
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
    assert!(lxmf.contains("aria-label=\"Cancel message delivery\">Cancel</button>"));
    assert!(lxmf.contains("message: 'Cancel this message?'"));
    assert!(lxmf.contains("canCancelSend ? _messageInlineCancelHtml(msg) : '<span class=\"msg-time\">' + time + '</span>'"));

    let state_js = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    assert!(state_js.contains("saveImageToPhotos"));
    assert!(state_js.contains("saveFileDocument"));
    assert!(state_js.contains("window.RS.invoke('save_stored_attachment_native'"));
    assert!(!state_js.contains("data_base64: result.data_base64 || ''"));
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
    assert!(tauri_lib.contains("async fn open_external_url(app: tauri::AppHandle, url: String)"));
    assert!(tauri_lib.contains("app.run_on_main_thread(move ||"));
    assert!(tauri_lib.contains("rx.recv_timeout(Duration::from_secs(15))"));
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
    let call_audio =
        read_source(root.join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakCallAudio.kt",
        ))
        .expect("Android call audio owner");
    let memo_audio = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakVoiceMemoAudio.kt",
    ))
    .expect("Android voice memo audio owner");
    let service =
        read_source(root.join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakService.kt",
        ))
        .expect("Android foreground service");
    assert!(activity.contains("MEDIA_PERMISSION_REQUEST_CODE"));
    assert!(activity.contains("fun hasMediaPermissions(audio: Boolean, camera: Boolean): Boolean"));
    assert!(activity.contains(
        "fun requestMediaPermissions(audio: Boolean, camera: Boolean, requestId: String)"
    ));
    assert!(activity.contains("window._onAndroidMediaPermissionResult"));
    assert!(activity.contains("mediaPlaybackRequiresUserGesture = false"));
    assert!(activity.contains("fun playCallRingtone(mode: String)"));
    assert!(activity.contains("fun stopCallRingtone()"));
    assert!(activity.contains("fun playCallTimeoutCue(): Boolean"));
    assert!(activity.contains("fun primeCallAudioRoute(role: String)"));
    assert!(activity.contains("fun startCallAudioRoute(role: String, sessionToken: String)"));
    assert!(activity.contains("fun stopCallAudioRoute()"));
    assert!(call_audio.contains("fun startForSession("));
    assert!(call_audio.contains("fun promoteCaptureForSession("));
    assert!(call_audio.contains("fun demoteCaptureForSession("));
    assert!(activity.contains("fun playCallRingtone(mode: String): Boolean"));
    assert!(activity.contains("runOnMainForBoolean"));
    assert!(activity.contains("AUDIOFOCUS_REQUEST_GRANTED"));
    assert!(activity.contains("AudioManager.STREAM_RING"));
    assert!(activity.contains("AudioAttributes.USAGE_VOICE_COMMUNICATION"));
    assert!(activity.contains("volumeControlStream = AudioManager.STREAM_VOICE_CALL"));
    assert!(call_audio.contains("syncProximity(application, preferEarpiece)"));
    assert!(call_audio.contains("PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK"));
    assert!(call_audio.contains("isWakeLockLevelSupported"));
    assert!(call_audio.contains("PowerManager.RELEASE_FLAG_WAIT_FOR_NO_PROXIMITY"));
    assert!(call_audio.contains("route = requestedRoute"));
    assert!(activity.contains("AudioAttributes.USAGE_VOICE_COMMUNICATION_SIGNALLING"));
    assert!(activity.contains("AudioAttributes.USAGE_NOTIFICATION_RINGTONE"));
    assert!(activity.contains("audioManager.setCommunicationDevice(route)"));
    assert!(call_audio.contains("private fun requestFocus(manager: AudioManager): Boolean"));
    assert!(call_audio.contains("RatspeakMobilePolicy.callSessionOwns(ownerToken, sessionToken)"));
    assert!(activity.contains("RatspeakVoiceAudio.stop()"));
    assert!(call_audio.contains("fun stopForSession("));
    assert!(service.contains("CountDownLatch"));
    assert!(service.contains("ensureReadyForMicrophoneCapture"));
    assert!(service.contains("ready.await"));
    assert!(memo_audio.contains("fun lastStartFailureCode(): String"));
    assert!(memo_audio.contains("lateinit var listener"));
    assert!(memo_audio.contains("voiceMemoInterruptionOwns"));
    assert!(memo_audio.contains("RatspeakAndroidObservers.voiceMemoAudioInterruption"));
    assert!(activity.contains("isVoiceMemoAudioSessionActive(token)"));
    assert!(activity.contains("handleAudioInterruption"));

    let voice_audio = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakVoiceAudio.kt",
    ))
    .expect("android voice audio");
    assert!(voice_audio.contains("object RatspeakVoiceAudio"));
    assert!(voice_audio.contains("AudioAttributes.USAGE_VOICE_COMMUNICATION"));
    assert!(voice_audio.contains("AudioAttributes.USAGE_MEDIA"));
    assert!(voice_audio.contains("fun startVoiceMemoPlayback("));
    assert!(voice_audio.contains("fun playbackHeadFrames(): Long"));
    assert!(voice_audio.contains("AudioAttributes.CONTENT_TYPE_SPEECH"));
    assert!(voice_audio.contains("AudioFormat.ENCODING_PCM_FLOAT"));
    assert!(voice_audio.contains("AudioFormat.ENCODING_PCM_16BIT"));
    assert!(voice_audio.contains("AudioTrack.MODE_STREAM"));
    assert!(voice_audio.contains("AudioTrack.WRITE_BLOCKING"));
    assert!(voice_audio.contains("AudioTrack.WRITE_NON_BLOCKING"));
    assert!(voice_audio.contains("setStartThresholdInFrames"));
    assert!(voice_audio.contains("if (written > 0 && starting)"));
    assert!(!voice_audio.contains("created.play()"));
    let first_write = voice_audio
        .find("val written = if (trackEncoding")
        .expect("initial AudioTrack write");
    let first_play = voice_audio[first_write..]
        .find("active.play()")
        .map(|offset| first_write + offset)
        .expect("AudioTrack starts after its initial write");
    assert!(first_write < first_play);
    assert!(voice_audio.contains("fun lastError(): String"));

    let state_js = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    assert!(state_js.contains("window.RS.mediaPermissions"));
    assert!(state_js.contains("window.RS.audioPlayback"));
    assert!(state_js.contains("function _rsNativeAndroidAudioAvailable()"));
    assert!(state_js.contains("if (_rsNativeAndroidAudioAvailable()) return null;"));
    assert!(state_js.contains("installUnlock: _rsInstallAudioPlaybackUnlock"));
    assert!(state_js.contains("window.RatspeakAndroid.requestMediaPermissions"));
    assert!(state_js.contains("function _rsNativeMicrophonePermission(audio)"));
    assert!(state_js.contains("ctx.state === 'suspended' || ctx.state === 'interrupted'"));
    assert!(!state_js.contains(
        "_rsAudioPlaybackUnlocked = ctx.state === 'running' || ctx.state === 'interrupted'"
    ));
    assert!(state_js.contains("ctx.state !== 'interrupted' &&"));
    assert!(state_js.contains("ctx.state !== 'closed'"));
    assert!(state_js.contains("isTauriMobile() &&"));
    assert!(state_js.contains("isIOS()"));
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
    let playback_ready = lxmf
        .split("function _voiceEnsurePlaybackReady()")
        .nth(1)
        .and_then(|tail| tail.split("function _voiceSyncRingtone()").next())
        .expect("voice playback readiness function");
    assert!(
        playback_ready.contains("if (_androidCallRouteBridge()) return Promise.resolve(true);")
    );
    assert!(
        playback_ready.find("_androidCallRouteBridge()").unwrap()
            < playback_ready.find("RS.audioPlayback.ensure").unwrap()
    );
    assert!(lxmf.contains("function _voiceAfterNextPaint()"));
    assert!(lxmf.contains("function _voiceSetOptimisticOutgoing(hash)"));
    assert!(lxmf.contains("function _voiceBlockMobileNavigation(ms)"));
    assert!(lxmf.contains("var dialToken = ++_voiceDialToken;"));
    assert!(lxmf.contains("function _voiceCancelMemoForCall()"));
    assert!(lxmf.contains("var _voiceAnswerToken = 0;"));
    assert!(lxmf.contains("function _voiceIncomingIsExact(linkId)"));
    assert!(lxmf.contains("RS.invoke('voice_answer', { args: { link_id: expectedLinkId } })"));
    let answer_call = lxmf
        .split("function _voiceAnswerCall()")
        .nth(1)
        .and_then(|tail| tail.split("function _voiceRejectCall()").next())
        .expect("voice answer function");
    assert!(answer_call.contains("incoming.status = 'answering';"));
    assert!(answer_call.contains("_voiceIncomingIsExact(expectedLinkId)"));
    assert!(!answer_call.contains("lxstVoiceState.incoming = null"));
    assert!(lxmf.contains("if (!incoming || incoming.status !== 'ringing')"));
    assert!(lxmf.contains("var terminatedMatches = (!data.link_id) ||"));
    assert!(lxmf.contains(
        "return _voiceCancelMemoForCall().then(_voiceAfterNextPaint).then(_voiceEnsurePlaybackReady).then(_voiceEnsureMicrophonePermission)"
    ));
    assert!(lxmf.contains(
        "return _voiceCancelMemoForCall().then(_voiceEnsurePlaybackReady).then(_voiceEnsureMicrophonePermission)"
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
    assert!(lxmf.contains("_voiceNativeAudioRouteLastSyncAt = Date.now();"));
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
    assert!(tauri_lib.contains("fn request_microphone_permission_apple("));
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
    assert!(voice_rs.contains("request_answer(&tx, expected_link_id)"));
    assert!(voice_rs.contains("\"snapshot\": snapshot"));
    assert!(voice_rs.contains("*persisted = Some(payload.clone());"));
    assert!(lxmf.contains("if (status && status.snapshot)"));
    assert!(lxmf.contains("_voiceHandleUpdate(status.snapshot);"));
    let voice_command = read_source(root.join("crates/ratspeak-tauri/src/commands/voice.rs"))
        .expect("voice command");
    assert!(voice_command.contains("pub struct VoiceAnswerArgs"));
    assert!(voice_command.contains("crate::voice::answer(&app_state, expected_link_id)"));
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
    assert!(notification_rs.contains("NativeNotificationKind::Channel"));
    assert!(notification_rs.contains("pub fn call("));
    assert!(notification_rs.contains("pub fn channel("));

    let notifier_rs =
        read_source(root.join("crates/ratspeak-tauri/src/notifier.rs")).expect("notifier");
    assert!(notifier_rs.contains("NativeNotificationKind::Call => \"ratspeak_calls\""));
    assert!(notifier_rs.contains("| ratspeak_core::NativeNotificationKind::Channel"));
    assert!(notifier_rs.contains("builder.action_type_id(thread_id)"));
    let tauri_events =
        read_source(root.join("dashboard/static/js/tauri_events.js")).expect("tauri events");
    assert!(tauri_events.contains("notification.actionTypeId"));

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
    assert!(ringtone_js.contains("playCallTimeoutCue"));
    assert!(ringtone_js.contains("stopCallRingtone"));
    assert!(ringtone_js.contains("if (!activeNodes.length) return;"));
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
    assert!(activity.contains("private const val CALL_TIMEOUT_CUE_MS = 520L"));
    assert!(activity.contains("mode.equals(\"timeout\", ignoreCase = true) -> \"timeout\""));
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
    assert!(index.contains("/static/js/state.js?v=ui-20260813-2"));
    assert!(index.contains("/static/js/voice_ringtones.js?v=ui-20260813-2"));
    assert!(index.contains("/static/js/lxmf.js?v=ui-20260813-2"));
    assert!(index.contains("/static/js/tauri_events.js?v=ui-20260813-2"));
    assert!(index.contains("id=\"lxst-call-global-mute-btn\""));
    assert!(index.contains("id=\"lxst-call-global-speaker-btn\""));
    assert!(index.contains("id=\"lxst-call-mute-btn\""));
    assert!(index.contains("id=\"lxst-call-speaker-btn\""));
    let ringtone_pos = index
        .find("/static/js/voice_ringtones.js")
        .expect("ringtone script");
    let lxmf_pos = index.find("/static/js/lxmf.js").expect("lxmf script");
    assert!(ringtone_pos < lxmf_pos);

    let tauri_events =
        read_source(root.join("dashboard/static/js/tauri_events.js")).expect("tauri events js");
    assert!(tauri_events.contains("RS.audioPlayback.installUnlock();"));
    assert!(!tauri_events.contains("RS.audioPlayback.ensure({ installUnlock: true })"));

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
fn ios_project_model_owns_app_store_info_declarations() {
    let root = repo_root();
    let project =
        read_source(root.join("src-tauri/gen/apple/project.yml")).expect("iOS project model");
    let info = read_source(root.join("src-tauri/gen/apple/ratspeak_iOS/Info.plist"))
        .expect("generated iOS Info.plist");

    for key in [
        "CFBundleURLTypes",
        "NSBluetoothAlwaysUsageDescription",
        "NSBluetoothPeripheralUsageDescription",
        "NSBonjourServices",
        "NSCameraUsageDescription",
        "NSLocalNetworkUsageDescription",
        "NSMicrophoneUsageDescription",
        "NSPhotoLibraryAddUsageDescription",
        "NSPhotoLibraryUsageDescription",
        "UIBackgroundModes",
    ] {
        assert!(project.contains(key), "project.yml must own {key}");
        assert!(
            info.contains(key),
            "generated Info.plist must contain {key}"
        );
    }
    for mode in ["audio", "bluetooth-central", "bluetooth-peripheral"] {
        assert!(project.contains(mode), "project.yml must own {mode}");
        assert!(
            info.contains(mode),
            "generated Info.plist must contain {mode}"
        );
    }
}

#[test]
fn ios_voice_capture_classifies_higher_priority_microphone_ownership() {
    let root = repo_root();
    let platform = read_source(root.join("crates/ratspeak-runtime/src/platform_ios.rs"))
        .expect("iOS platform audio bridge");
    let commands = read_source(root.join("crates/ratspeak-tauri/src/commands/voice.rs"))
        .expect("voice commands");

    assert!(platform.contains("AV_AUDIO_SESSION_ERROR_INSUFFICIENT_PRIORITY"));
    assert!(platform.contains("AV_AUDIO_SESSION_ERROR_SIRI_IS_RECORDING"));
    assert!(platform.contains("msg_send![error, code]"));
    assert!(platform.contains("Another app or call is using the microphone"));
    assert!(commands.contains("microphone_in_use"));
    assert!(commands.contains("AppError::conflict(VOICE_MEMO_AUDIO_BUSY)"));
    assert!(!commands.contains("tracing::warn!(error"));
}

#[test]
fn ios_project_links_runtime_resolved_audio_without_copying_rust_archives() {
    let root = repo_root();
    let model =
        read_source(root.join("src-tauri/gen/apple/project.yml")).expect("iOS project model");
    let generated =
        read_source(root.join("src-tauri/gen/apple/ratspeak.xcodeproj/project.pbxproj"))
            .expect("generated iOS project");

    assert!(model.contains("- path: Externals\n        excludes:\n          - \"**/*.a\""));
    assert!(model.contains("- sdk: AVFAudio.framework"));
    assert!(generated.contains("AVFAudio.framework in Frameworks"));
    assert!(!generated.contains("libapp.a in Resources"));
}

#[test]
fn apple_bundle_identifier_is_consistent_without_migrating_desktop() {
    let root = repo_root();
    let ios_config: serde_json::Value = serde_json::from_str(
        &read_source(root.join("src-tauri/tauri.ios.conf.json")).expect("iOS Tauri config"),
    )
    .expect("valid iOS Tauri config");
    let desktop_config: serde_json::Value = serde_json::from_str(
        &read_source(root.join("src-tauri/tauri.conf.json")).expect("base Tauri config"),
    )
    .expect("valid base Tauri config");
    let project_model =
        read_source(root.join("src-tauri/gen/apple/project.yml")).expect("iOS project model");
    let project = read_source(root.join("src-tauri/gen/apple/ratspeak.xcodeproj/project.pbxproj"))
        .expect("generated iOS project");
    let runtime = read_source(root.join("src-tauri/src/lib.rs")).expect("Tauri runtime");
    let workflow =
        read_source(root.join(".github/workflows/release-ios.yml")).expect("iOS workflow");

    assert_eq!(ios_config["identifier"], "org.ratspeak.apple");
    assert_eq!(desktop_config["identifier"], "org.ratspeak.desktop");
    assert!(project_model.contains("bundleIdPrefix: org.ratspeak.apple"));
    assert!(project_model.contains("PRODUCT_BUNDLE_IDENTIFIER: org.ratspeak.apple"));
    assert_eq!(
        project
            .matches("PRODUCT_BUNDLE_IDENTIFIER = org.ratspeak.apple;")
            .count(),
        2
    );
    let normalized_runtime = runtime.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(normalized_runtime.contains("OsLogger::new( \"org.ratspeak.apple\", \"default\", )"));
    assert!(workflow.contains("org.ratspeak.apple)"));
}

#[test]
fn ios_release_assets_use_supported_single_size_appearance_catalog() {
    let root = repo_root();
    let catalog_dir = root.join("src-tauri/gen/apple/Assets.xcassets/AppIcon.appiconset");
    let catalog: serde_json::Value = serde_json::from_str(
        &read_source(catalog_dir.join("Contents.json")).expect("iOS app-icon catalog"),
    )
    .expect("valid iOS app-icon catalog JSON");
    let images = catalog["images"]
        .as_array()
        .expect("iOS app-icon image list");
    assert_eq!(images.len(), 2, "single-size iOS catalog has Any and Dark");

    for image in images {
        assert_eq!(image["idiom"], "universal");
        assert_eq!(image["platform"], "ios");
        assert_eq!(image["size"], "1024x1024");
    }
    assert_eq!(images[0]["filename"], "AppIcon-512@2x.png");
    assert!(images[0]["appearances"].is_null());
    assert_eq!(images[1]["filename"], "AppIcon-512@2x-dark.png");
    assert_eq!(images[1]["appearances"][0]["appearance"], "luminosity");
    assert_eq!(images[1]["appearances"][0]["value"], "dark");

    let mut pngs = fs::read_dir(&catalog_dir)
        .expect("read iOS app-icon catalog")
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("png"))
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    pngs.sort();
    assert_eq!(
        pngs,
        ["AppIcon-512@2x-dark.png", "AppIcon-512@2x.png"],
        "unreferenced images make actool report unassigned children"
    );

    let project =
        read_source(root.join("src-tauri/gen/apple/project.yml")).expect("iOS project model");
    assert!(project.contains("CFBundleURLTypes:"));
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
    assert_eq!(version_file.trim(), "1.0.26k");

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
    assert!(tauri_conf.contains(r#""versionCode": 1000040"#));

    let android_gradle = read_source(root.join("src-tauri/gen/android/app/build.gradle.kts"))
        .expect("android gradle");
    assert!(android_gradle.contains("fun ratspeakDisplayVersionName()"));
    assert!(android_gradle.contains("../../../VERSION"));
    assert!(android_gradle.contains("versionName = ratspeakDisplayVersionName()"));
}

#[test]
fn release_workflows_pin_reviewed_dependencies_and_stage_tag_builds_as_prereleases() {
    let root = repo_root();
    let rsreticulum_commit = "RATSPEAK_RSRETICULUM_REF: 2e0d0a688881040a0a12639490f494b162466be1";
    let rslxmf_commit = "RATSPEAK_RSLXMF_REF: b40dcd37f2e9961ef5fdae535ecafe823f98255a";
    let rslxst_commit = "RATSPEAK_RSLXST_REF: fd81d7155c6e0af799aaa398aee21028ce535924";
    let dependency_refs = [
        "RATSPEAK_RSRETICULUM_REF: ratspeak-v1.0.26k",
        "RATSPEAK_RSLXMF_REF: ratspeak-v1.0.26k",
        "RATSPEAK_RSLXST_REF: ratspeak-v1.0.26k",
        "RATSPEAK_LRGP_REF: ratspeak-v1.0.26d",
    ];

    for workflow_path in [
        ".github/workflows/release-android.yml",
        ".github/workflows/release-desktop.yml",
        ".github/workflows/release-macos.yml",
        ".github/workflows/release-windows.yml",
    ] {
        let workflow = read_source(root.join(workflow_path)).expect("release workflow");
        for dependency_ref in dependency_refs {
            assert!(
                workflow.contains(dependency_ref),
                "{workflow_path} must pin {dependency_ref}"
            );
        }
        assert!(workflow.contains("default: true\n        type: boolean"));
        assert!(
            workflow
                .contains("prerelease: ${{ github.event_name == 'push' || inputs.prerelease }}")
        );
    }

    for workflow_path in [
        ".github/workflows/ci.yml",
        ".github/workflows/build-desktop.yml",
    ] {
        let workflow = read_source(root.join(workflow_path)).expect("build workflow");
        assert!(
            workflow.contains(rsreticulum_commit),
            "{workflow_path} must build the reviewed rsReticulum commit"
        );
        assert!(
            workflow.contains(rslxmf_commit),
            "{workflow_path} must build the synchronized rsLXMF commit"
        );
        assert!(
            workflow.contains(rslxst_commit),
            "{workflow_path} must build the reviewed rsLXST commit"
        );
    }

    for workflow_path in [
        ".github/workflows/release-android.yml",
        ".github/workflows/release-desktop.yml",
        ".github/workflows/release-macos.yml",
    ] {
        let workflow = read_source(root.join(workflow_path)).expect("release workflow");
        assert!(workflow.contains(r#""$(basename "$artifact")""#));
    }
    let windows =
        read_source(root.join(".github/workflows/release-windows.yml")).expect("Windows release");
    assert!(windows.contains(r#""$hash  $($_.Name)""#));
    assert!(!windows.contains("$hash  $path"));
    let linux =
        read_source(root.join(".github/workflows/release-desktop.yml")).expect("Linux release");
    assert!(linux.contains(r#"test -n "$rpm""#));
    assert!(linux.contains(r#"test "$artifact_count" = "4""#));

    let ios =
        read_source(root.join(".github/workflows/release-ios.yml")).expect("iOS release workflow");
    for dependency_ref in dependency_refs {
        assert!(ios.contains(dependency_ref));
    }
    assert!(ios.contains(r#"--build-number "${GITHUB_RUN_NUMBER}""#));
    assert!(ios.contains("--export-method app-store-connect"));
    assert!(ios.contains("APPLE_DEVELOPMENT_TEAM: ${{ vars.APPLE_TEAM_ID }}"));
    for required in [
        "IOS_DISTRIBUTION_CERTIFICATE_BASE64",
        "IOS_DISTRIBUTION_CERTIFICATE_PASSWORD",
        "IOS_PROVISIONING_PROFILE_BASE64",
        "APPSTORE_API_PRIVATE_KEY",
        "APPLE_TEAM_ID",
        "APPSTORE_API_KEY_ID",
        "APPSTORE_ISSUER_ID",
    ] {
        assert!(ios.contains(required));
    }
    assert!(!ios.contains("PlistBuddy"));
    for release_gate in [
        "assert-apple-toolchain.sh",
        "assert-ios-project-metadata.sh",
        "assert-ios-signing-profile.sh",
        "assert-ios-bundle.sh simulator",
        "assert-ios-bundle.sh testflight",
        "assert-no-tauri-dev-url.sh",
    ] {
        assert!(
            ios.contains(release_gate),
            "missing iOS gate: {release_gate}"
        );
    }
    assert!(ios.contains("runs-on: macos-26"));
    assert!(ios.contains("security set-key-partition-list"));
    assert!(ios.contains("$PROFILE_UUID.mobileprovision"));
    assert!(ios.contains("Remove temporary signing keychain"));

    let ios_project = read_source(root.join("src-tauri/gen/apple/project.yml"))
        .expect("iOS project specification");
    assert!(ios_project.contains("path: ratspeak_iOS/PrivacyInfo.xcprivacy"));
    assert!(ios_project.contains("buildPhase: resources"));
    assert!(!ios_project.contains("entitlements:"));

    let ios_pbx = read_source(root.join("src-tauri/gen/apple/ratspeak.xcodeproj/project.pbxproj"))
        .expect("generated iOS project");
    assert!(ios_pbx.contains("PrivacyInfo.xcprivacy in Resources"));
    assert!(!ios_pbx.contains("CODE_SIGN_ENTITLEMENTS"));

    let app_cargo = read_source(root.join("src-tauri/Cargo.toml")).expect("app Cargo.toml");
    assert!(app_cargo.contains(r#"tauri = { version = "2", features = [] }"#));
    assert!(
        app_cargo.contains(r#"tauri = { version = "2", features = ["tray-icon", "devtools"] }"#)
    );

    let privacy_manifest =
        read_source(root.join("src-tauri/gen/apple/ratspeak_iOS/PrivacyInfo.xcprivacy"))
            .expect("iOS privacy manifest");
    assert!(privacy_manifest.contains("NSPrivacyTracking"));
    assert!(privacy_manifest.contains("NSPrivacyAccessedAPITypes"));
    assert!(privacy_manifest.contains("Public channel"));
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
    assert!(general_panel.contains(r#"<span class="settings-row-label">Notifications</span>"#));
    assert!(general_panel.contains(r#"id="settings-row-notifications""#));
    assert!(general_panel.contains(r#"id="desktop-notifications-toggle""#));
    assert!(general_panel.contains(r#"id="settings-notification-action""#));
    assert!(general_panel.contains(r#"id="settings-row-keep-connected""#));
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
    assert!(identity_panel.contains(r#"id="settings-status-action-btn">Set</button>"#));
    assert!(!identity_panel.contains(r#"id="settings-clear-status-btn""#));
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
    assert!(settings_js.contains("window.RatspeakAndroid.batteryOptimizationStatus()"));
    assert!(settings_js.contains("requestBatteryOptimizationExemption()"));
    assert!(settings_js.contains("rs-notification-permission-changed"));
    assert!(!settings_js.contains("document.getElementById('panel-settings-notifications')"));
    assert!(settings_js.contains("function syncSettingsIdentityStatus()"));
    assert!(settings_js.contains("actionBtn.textContent = status ? 'Edit' : 'Set';"));
    assert!(settings_js.contains("openIdentityStatusEditor()"));
    assert!(settings_js.contains("clearStatusBtn.textContent = 'Clear status';"));
    assert!(
        settings_js.contains(
            "clearStatusBtn.addEventListener('click', function() { submitStatus(''); });"
        )
    );
    assert!(settings_js.contains("var saveLabel = initialStatus ? 'Save changes' : 'Set status';"));
    assert!(
        settings_js
            .contains("setActiveProfileStatus(savedStatus === null ? nextStatus : savedStatus);")
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
    assert!(!index.contains("settings-mobile-detail-eyebrow"));
    for duplicated_title in [
        r#"<div class="panel-header">General</div>"#,
        r#"<div class="panel-header">Channels</div>"#,
        r#"<div class="panel-header">Identity</div>"#,
        r#"<div class="panel-header">Privacy</div>"#,
        r#"<div class="panel-header">Network</div>"#,
        r#"<div class="panel-header">System</div>"#,
    ] {
        assert!(
            !index.contains(duplicated_title),
            "settings detail should not repeat its page title: {duplicated_title}"
        );
    }
    assert!(!index.contains("settings-relay-dot"));
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
    assert!(!responsive_css.contains(".settings-mobile-detail-eyebrow"));
    assert!(
        responsive_css
            .contains(".settings-page:not(.settings-mobile-detail-active) .settings-detail-pane")
    );
    assert!(
        responsive_css.contains(".settings-detail-mode .settings-panel.settings-panel-selected")
    );
    assert!(responsive_css.contains(".settings-row-label {\n        font-size: 1rem;"));
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
    assert!(!index.contains(r#"<div class="panel-header">System</div>"#));
    assert!(!index.contains(r#"<div class="settings-panel-section-title">System</div>"#));
    assert!(index.contains(r#"<span class="settings-row-label">Developer Mode</span>"#));
    assert!(index.contains(r#"role="radiogroup" aria-label="Developer Mode""#));
    assert!(index.contains(r#"type="radio" name="settings-developer-mode" id="settings-developer-mode-off" value="off" checked"#));
    assert!(index.contains(
        r#"type="radio" name="settings-developer-mode" id="settings-developer-mode-on" value="on""#
    ));
    assert!(index.contains(r#"<div class="settings-panel-section-title">Reset</div>"#));

    let developer_mode = index
        .find(r#"<span class="settings-row-label">Developer Mode</span>"#)
        .unwrap();
    let reset_title = index
        .find(r#"<div class="settings-panel-section-title">Reset</div>"#)
        .unwrap();
    let cache_section = index.find(r#"id="system-section-caches""#).unwrap();
    assert!(developer_mode < reset_title && reset_title < cache_section);

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
            .contains(".settings-radio-option span { min-height: 40px; min-width: 58px; }")
    );
}

#[test]
fn settings_machine_states_share_uppercase_outfit_typography() {
    let root = repo_root();
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    let tokens = read_source(root.join("dashboard/static/css/00-tokens.css")).expect("type tokens");
    let views = read_source(root.join("dashboard/static/css/10-views.css")).expect("views css");
    let settings = read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    let propagation =
        read_source(root.join("dashboard/static/js/propagation.js")).expect("propagation js");
    let modals = read_source(root.join("dashboard/static/js/modals.js")).expect("modals js");

    assert!(tokens.contains("--type-state-size:     var(--text-xs);"));
    assert!(tokens.contains("--type-state-weight:   var(--type-weight-semibold);"));
    assert!(tokens.contains("--type-state-tracking: 0.04em;"));
    assert!(views.contains(".settings-radio-option span {"));
    assert!(views.contains("font-family: var(--font-sans);"));
    assert!(views.contains("font-size: var(--type-state-size);"));
    assert!(views.contains("text-transform: uppercase;"));
    assert!(views.contains(".settings-state-value {"));
    assert!(views.contains(".relay-mode-btn {"));

    assert!(
        index.contains(
            r#"class="selector-badge settings-state-value" id="transport-mode-select">OFF"#
        )
    );
    assert!(index.contains(r#"id="hw-lock-timeout-select">OFF</button>"#));
    assert!(settings.contains("if (!secs || secs <= 0) return 'OFF';"));
    assert!(settings.contains("{ label: 'OFF', value: '0'"));
    assert!(propagation.contains("? ('Cost ' + cost) : 'OFF'"));
    assert!(propagation.contains("relayBadge.textContent = 'OFF';"));
    assert!(modals.contains("{ label: 'Always on', value: '0' }"));
}

#[test]
fn mobile_primary_lists_share_readable_row_scale() {
    let root = repo_root();
    let responsive_css =
        read_source(root.join("dashboard/static/css/13-responsive.css")).expect("responsive css");

    assert!(responsive_css.contains("--mobile-list-avatar-size: 44px;"));
    assert!(responsive_css.contains("--mobile-list-min-height: 58px;"));
    assert!(responsive_css.contains("--mobile-list-title-size: 1rem;"));
    assert!(responsive_css.contains("--mobile-list-detail-size: 0.875rem;"));
    assert!(responsive_css.contains("--mobile-list-meta-size: 0.8125rem;"));
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
    assert!(responsive_css.contains(".conn-group-header {\n        font-size: var(--text-sm);"));
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
    assert!(peers.contains("var mobileRows = isCompactLayout();"));
    assert!(peers.contains("function _measurePeerRowHeights(compact, scrollContainer)"));
    assert!(peers.contains("var minimumBase = compact ? 58 : 36;"));
    assert!(peers.contains("var minimumStatus = compact ? 68 : 48;"));
    assert!(peers.contains("var baseRowHeight = measuredRows.base;"));
    assert!(peers.contains("var statusRowHeight = measuredRows.status;"));
    assert!(peers.contains("_peersRowHeight = baseRowHeight;"));
    assert!(peers.contains("var avatarSize = isCompactLayout() ? 44 : 28;"));
    assert!(peers.contains("window.addEventListener('ratspeak-text-scale-changed'"));
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
    let conn_sheet_css = responsive_css
        .split(".conn-detail-sheet {")
        .nth(1)
        .and_then(|tail| tail.split('}').next())
        .expect("mobile connection detail sheet rule");
    assert!(conn_sheet_css.contains("max-width: 100vw;"));
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
    assert!(
        contact_card_js
            .contains("var previewCommand = options.previewCommand || 'api_preview_contact_card'")
    );
    assert!(contact_card_js.contains("RS.invoke(previewCommand, { payload: payload })"));
    assert!(contact_card_js.contains("RS.invoke('import_contact_card'"));
    assert!(contact_card_js.contains("window.RS.qr = {"));
    assert!(contact_card_js.contains("openScanner: openContactQrScanner"));
    assert!(contact_card_js.contains("renderQrCanvas(canvas, card.payload || '')"));
    let share_start = contact_card_js
        .find("function showIdentityShareScreen(identityHash)")
        .expect("identity share flow");
    let share_end = contact_card_js[share_start..]
        .find("function showScannedCardPreview")
        .map(|offset| share_start + offset)
        .expect("identity share flow end");
    let share_flow = &contact_card_js[share_start..share_end];
    assert!(share_flow.contains("Preparing contact card&hellip;"));
    assert!(share_flow.contains("built.sheet.setAttribute('aria-busy', 'true')"));
    assert!(share_flow.contains("window.requestAnimationFrame"));
    let share_sheet_pos = share_flow
        .find("buildSheet('contact-share-sheet')")
        .expect("share sheet is created");
    let share_request_pos = share_flow
        .find("RS.invoke('api_contact_card'")
        .expect("contact-card request is made");
    assert!(
        share_sheet_pos < share_request_pos,
        "share sheet must appear before contact-card generation begins"
    );
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
    assert!(views_css.contains(".contact-share-loading-qr"));
    assert!(views_css.contains(".contact-share-error-title"));
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
    assert!(nav.contains("var MOBILE_TAB_SLOTS = ['peers', 'message', 'channels', 'more'];"));
    assert!(
        nav.contains("var MORE_VIEWS = ['contacts', 'identity', 'network', 'games', 'settings'];")
    );
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
    assert!(!lxmf.contains("preventDefaultOnStart: function()"));
    assert!(lxmf.contains("container.addEventListener('touchstart', function()"));
    assert!(lxmf.contains("state.settleToken++;"));
    assert!(lxmf.contains("state.programmaticScrollUntil = 0;"));
    assert!(lxmf.contains("}, { passive: true });"));
    assert!(lxmf.contains("if (e.defaultPrevented) return;"));
    assert!(lxmf.contains("(t.closest('.lxmf-msg') && _shouldPreserveLxmfComposerKeyboard())"));
    assert!(lxmf.contains("function _bindMessageFocusPreservingActivation"));
    assert!(lxmf.contains("preserveComposerKeyboard"));
    assert!(lxmf.contains("var _suppressImageOpenUntil = 0;"));
    assert!(lxmf.contains("container.querySelectorAll('.lxmf-send-cancel, .msg-send-cancel-inline').forEach(function(btn)"));
    assert!(lxmf.contains("_bindMessageFocusPreservingActivation(btn, function()"));
    assert!(lxmf.contains("_cancelLxmfSend(btn.getAttribute('data-msg-id'));"));
    assert!(lxmf.contains("title: 'Cancel delivery?'"));
    assert!(messaging.contains("\"may_have_left_device\": cancelled"));
    assert!(inbound.contains("lxmf_step_starts_delivery_timeout"));
    assert!(inbound.contains("manager.cancel_outbound_message(msg_id)"));
    assert!(inbound.contains("\"step\": \"timeout\""));
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
fn optimistic_lxmf_cancel_is_native_before_canonical_reconciliation() {
    let root = repo_root();
    let state = read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("state rs");
    let messaging = read_source(root.join("crates/ratspeak-tauri/src/commands/messaging.rs"))
        .expect("messaging command");
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");

    assert!(state.contains("pub fn begin_lxmf_client_send"));
    assert!(state.contains("pub fn cancel_lxmf_client_send"));
    assert!(state.contains("pub fn publish_canonical"));
    assert!(state.contains("self.clear_lxmf_client_sends();"));
    assert!(messaging.contains("finalize_lxmf_client_send"));
    assert!(messaging.contains("LxmfClientSendCancellation::Preparing"));
    assert!(messaging.contains("LxmfClientSendCancellation::Queued"));
    assert!(lxmf.contains("_pendingLxmfCancelByClientId[msgId] = true;"));
    assert!(lxmf.contains("return _invokeLxmfCancel(msgId).then(function(resp)"));
    assert!(lxmf.contains("title: 'Cancel delivery?'"));
    assert!(lxmf.contains("var eventMsgId = data.msg_id || data.client_msg_id;"));
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
    assert!(index.contains(r#"id="settings-status-action-btn""#));
    assert!(!index.contains(r#"id="settings-clear-status-btn""#));
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
    assert!(settings_js.contains("settings-status-action-btn"));
    assert!(settings_js.contains("rs-dialog-clear-status"));
    assert!(settings_js.contains("submitStatus('')"));
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
    assert!(setup_js.contains("function setupCompletionView()"));
    assert!(setup_js.contains("window.location.href = '/#' + setupCompletionView()"));
    assert!(!setup_js.contains("window.location.href = '/#dashboard'"));
    let nav_js = read_source(root.join("dashboard/static/js/nav.js")).expect("nav js");
    assert!(nav_js.contains("function _viewForNavigationSurface(viewId)"));
    assert!(nav_js.contains("appUsesMobileNavigation()"));
    let state_js = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    assert!(state_js.contains("function appUsesMobileNavigation()"));
    assert!(state_js.contains("function appLandingView()"));
    let identity_js =
        read_source(root.join("dashboard/static/js/identity.js")).expect("identity js");
    assert!(!identity_js.contains("window.location.href = '/#dashboard'"));

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
    let setup_js = read_source(root.join("dashboard/static/js/setup.js")).expect("setup js");
    let state_js = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    let hardware_rs =
        read_source(root.join("crates/ratspeak-runtime/src/hardware.rs")).expect("hardware rs");

    assert!(state_js.contains("function supportsHardwareIdentities()"));
    assert!(state_js.contains("if (isTauriMobile()) return false;"));
    assert!(setup_js.contains("!supportsHardwareIdentities()"));
    assert!(identity_js.contains("!supportsHardwareIdentities()"));
    assert!(!setup_js.contains("typeof isMobile === 'function') && isMobile()"));

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
fn mobile_native_ownership_and_usb_recovery_remain_closed_and_single_flight() {
    let root = repo_root();
    let native = read_source(root.join("src-tauri/src/mobile_native.rs")).expect("mobile native");
    let supervisor = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakPlatformSupervisor.kt",
    ))
    .expect("Android platform supervisor");
    let activity = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("Android main activity");
    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    let shared = read_source(root.join("crates/ratspeak-tauri/src/commands/shared.rs"))
        .expect("shared commands");
    let health = read_source(root.join("dashboard/static/js/health.js")).expect("health js");

    assert!(native.contains("requestUsbPermissionForSelector"));
    assert!(native.contains(r#""(IILjava/lang/String;)V""#));
    assert!(supervisor.contains("@JvmStatic\n    fun requestUsbPermissionForSelector"));
    let on_create = activity
        .split("override fun onCreate(savedInstanceState: Bundle?)")
        .nth(1)
        .expect("MainActivity onCreate");
    assert!(
        on_create.find("RatspeakNativeBridge.initialize(applicationContext)")
            < on_create.find("super.onCreate(savedInstanceState)"),
        "native Application context must exist before Tauri can restore saved BLE"
    );
    assert!(interfaces.contains("pub async fn request_android_usb_permission("));
    assert!(interfaces.contains("selector.serial_number.as_deref()"));
    assert!(interfaces.contains("preflight_android_usb_selector_for_interface"));
    assert!(interfaces.contains("id_interval: cfg_u64(entry, \"id_interval\")"));
    assert!(interfaces.contains("id_callsign: cfg_non_empty_str(entry, \"id_callsign\")"));
    assert!(interfaces.contains("config.id_interval = id_interval;"));
    assert!(interfaces.contains("config.id_callsign = id_callsign.map"));
    assert!(native.contains("requestUsbPermissionForLegacyPath"));
    let runtime = read_source(root.join("crates/ratspeak-runtime/src/lib.rs")).expect("runtime");
    assert!(
        runtime.contains("migrate_android_usb_selectors_for_startup(&state, &config_dir).await")
    );
    assert!(runtime.contains("enforce_android_single_ble_rnode_for_startup(&state, &config_dir)"));
    assert!(runtime.contains("state.wait_for_mobile_platform_bridge().await;"));
    assert!(shared.contains("fields.remove(\"usb_vendor_id\")"));
    assert!(shared.contains("fields.remove(\"usb_product_id\")"));
    assert!(shared.contains("fields.remove(\"usb_serial_number\")"));
    assert!(shared.contains("state.mobile_hardware_state_snapshot()"));
    assert!(health.contains("var _androidUsbResumePermission = null;"));
    assert!(health.contains("return Promise.resolve(false)"));
    assert!(health.contains("_androidUsbResumePermission.cancel("));
    assert!(health.contains("window._onUsbSelectorPermissionResult === ownedCallback"));
    assert!(health.contains("function applyMobileHardwareState(data)"));
    assert!(health.contains("if (online) mobileHealth = null;"));
}

#[test]
fn android_audio_initializes_process_context_before_cpal_access() {
    let root = repo_root();
    let runtime =
        read_source(root.join("crates/ratspeak-runtime/src/voice.rs")).expect("voice runtime");
    let runtime_manifest =
        read_source(root.join("crates/ratspeak-runtime/Cargo.toml")).expect("runtime manifest");

    assert!(runtime_manifest.contains("ndk-context = \"0.1.1\""));
    assert!(runtime.contains("static ANDROID_AUDIO_CONTEXT: OnceLock<"));
    assert!(runtime.contains("ndk_context::initialize_android_context("));
    assert!(runtime.contains(".new_global_ref(application)"));

    let call_start = runtime
        .find("async fn start(\n        link_id: [u8; 16]")
        .expect("call audio start");
    let call_start = &runtime[call_start..];
    assert!(
        call_start.find("ensure_android_audio_context()?")
            < call_start.find("let host = cpal::default_host()"),
        "live calls must establish ndk-context before CPAL"
    );

    let memo_start = runtime
        .find("pub(crate) fn start_microphone_capture(")
        .expect("voice memo capture start");
    let memo_start = &runtime[memo_start..];
    assert!(
        memo_start.find("ensure_android_audio_context()?")
            < memo_start.find("let host = cpal::default_host()"),
        "voice memos must establish ndk-context before CPAL"
    );
}

#[test]
fn mobile_memory_pressure_reaches_bounded_attachment_owners_without_webview_authority() {
    let root = repo_root();
    let native = read_source(root.join("src-tauri/src/mobile_native.rs")).expect("mobile native");
    let state =
        read_source(root.join("crates/ratspeak-runtime/src/state.rs")).expect("runtime state");
    let activity = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("Android main activity");
    let bridge = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakNativeBridge.kt",
    ))
    .expect("Android native bridge");
    let shell = read_source(root.join("src-tauri/src/lib.rs")).expect("mobile shell");
    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("messaging js");
    let state_js = read_source(root.join("dashboard/static/js/state.js")).expect("state js");

    assert!(activity.contains("override fun onTrimMemory(level: Int)"));
    assert!(activity.contains("RatspeakMobilePolicy.attachmentMemoryPressure(level)"));
    assert!(activity.contains("RatspeakNativeBridge.publishMemoryPressure(it)"));
    assert!(bridge.contains("private external fun nativeMemoryPressure(critical: Boolean)"));
    assert!(native.contains("RatspeakNativeBridge_nativeMemoryPressure"));
    assert!(native.contains("state.handle_attachment_memory_pressure(critical)"));
    assert!(shell.contains("UIApplicationDidReceiveMemoryWarningNotification"));
    assert!(shell.contains("register_ios_memory_warning_observer"));
    assert!(state.contains("Active router\n    /// deliveries retain their exact lease"));
    assert!(lxmf.contains("function handleAttachmentMemoryPressure(critical)"));
    assert!(state_js.contains("RS.listen('attachment_memory_pressure'"));
    assert!(state_js.contains("window.RS.invoke('save_stored_attachment_native'"));
    assert!(bridge.contains("fun saveStoredFile("));
    assert!(activity.contains("FileInputStream(pending.privateFile"));
    assert!(activity.contains("input.copyTo(output, 64 * 1024)"));
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
    assert!(!lxmf.contains("pub async fn resolve_destination"));
    assert!(lxmf.contains("self.router.try_send(msg).ok()?;"));

    let messaging = read_source(root.join("crates/ratspeak-tauri/src/commands/messaging.rs"))
        .expect("messaging commands");
    assert!(!messaging.contains("TransportMessage::AwaitPath"));
    assert!(!messaging.contains("resolve_before_send"));
    assert!(messaging.contains("hydrate_contact_identity_for_send"));
    assert!(messaging.contains("schedule_announce_after_user_send"));

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
    assert!(peers.contains("/^[a-f0-9]{8}$/i.test(name)"));
    assert!(peers.contains("var BARE_HEX_CLUSTER_MIN = 3"));
    assert!(peers.contains("function _hasKnownPeerEvidence(entry)"));
    assert!(peers.contains("_hasConversationWith(entry.hash)"));
    assert!(peers.contains("entry.is_contact || _supportsRatspeakFeatures(entry)"));
    assert!(peers.contains("services.indexOf('lxst.telephony') !== -1"));
    assert!(peers.contains("var _hideKnownSpamPeers = true"));
    assert!(peers.contains("function setHideKnownSpamPeers(enabled)"));
    assert!(peers.contains("if (_isSuppressedPeerEntry(_cache[h], context)) continue;"));
    assert!(peers.contains("_isSuppressedPeerEntry(entry, _visibilityContext()) ? null : entry"));
    assert!(peers.contains("function visibilityContextChanged()"));

    let lxmf = read_source(root.join("dashboard/static/js/lxmf.js")).expect("lxmf js");
    assert!(lxmf.contains("PeersCache.visibilityContextChanged();"));

    let health = read_source(root.join("dashboard/static/js/health.js")).expect("health js");
    assert!(health.contains("var peers = PeersCache.enriched();"));

    let index = read_source(root.join("dashboard/index.html")).expect("dashboard index");
    assert!(index.contains("Hide known spam peers"));
    assert!(
        index
            .contains("Hide repeated bridge-style IDs unless you have saved or messaged the peer.")
    );
    assert!(index.contains("id=\"settings-hide-known-spam-peers-on\" value=\"on\" checked"));

    let settings = read_source(root.join("dashboard/static/js/settings.js")).expect("settings js");
    assert!(
        !settings.contains("_isSuppressedPeerDisplayName"),
        "the classifier must stay centralized in PeersCache"
    );
    assert!(settings.contains("set_hide_known_spam_peers"));
    assert!(settings.contains("PeersCache.setHideKnownSpamPeers"));
    assert!(settings.contains("renderDashboardPeersList()"));

    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("interfaces commands");
    assert!(interfaces.contains("pub async fn set_hide_known_spam_peers"));
    assert!(interfaces.contains("\"hide_known_spam_peers\""));
    assert!(interfaces.contains(".is_none_or(|value| value != \"false\")"));

    let tauri_lib = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri lib");
    assert!(tauri_lib.contains("set_hide_known_spam_peers"));
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
    let attachment_helper = messaging
        .split("async fn queue_prepared_attachment")
        .nth(1)
        .and_then(|source| source.split("\n#[tauri::command]").next())
        .expect("shared attachment queue helper");
    assert!(attachment_helper.contains("ensure_propagation_ready_for_send("));
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
    // Match the call rather than its line layout so `cargo fmt` cannot break
    // this source contract by wrapping the receiver onto a preceding line.
    assert!(
        runtime.contains("tick_with_auto_propagation_download_ready(auto_inbox_download_ready)")
    );
    assert!(runtime.contains("lxmf tick worker failed; skipping this tick"));
    assert!(lxmf.contains("OutboundAction::Failed(message) =>"));
    assert!(lxmf.contains("try_auto_propagation_fallback("));
    assert!(lxmf.contains("OutboundAction::Expired(message) =>"));
    assert!(lxmf.contains("Do not reinterpret expiry as an"));
    assert!(lxmf.contains("attempt_exhausted_outbound_surfaces_failed_state"));
    assert!(lxmf.contains("expired_auto_live_send_does_not_fall_back_to_offline_inbox"));
}

#[test]
fn voice_memos_share_lxst_capture_and_the_bounded_lxmf_attachment_path() {
    let root = repo_root();
    let memo = read_source(root.join("crates/ratspeak-runtime/src/voice_memo.rs"))
        .expect("voice memo runtime");
    let voice =
        read_source(root.join("crates/ratspeak-runtime/src/voice.rs")).expect("voice runtime");
    let commands = read_source(root.join("crates/ratspeak-tauri/src/commands/voice.rs"))
        .expect("voice commands");
    let messaging = read_source(root.join("dashboard/static/js/lxmf.js")).expect("messaging js");
    let messaging_commands =
        read_source(root.join("crates/ratspeak-tauri/src/commands/messaging.rs"))
            .expect("messaging commands");
    let contact_commands = read_source(root.join("crates/ratspeak-tauri/src/commands/contacts.rs"))
        .expect("contact commands");
    let tauri = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri entrypoint");
    let system = read_source(root.join("crates/ratspeak-tauri/src/commands/system.rs"))
        .expect("system commands");
    let state_js = read_source(root.join("dashboard/static/js/state.js")).expect("state js");
    let shared_ui = read_source(root.join("dashboard/static/js/ui_shared.js")).expect("shared ui");
    let voice_memos =
        read_source(root.join("dashboard/static/js/voice_memos.js")).expect("voice memo js");
    let android = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("android activity");
    let android_memo_audio = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakVoiceMemoAudio.kt",
    ))
    .expect("android memo audio");
    let android_voice_audio = read_source(root.join(
        "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakVoiceAudio.kt",
    ))
    .expect("android voice output");
    let android_service =
        read_source(root.join(
            "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakService.kt",
        ))
        .expect("android service");
    let ios_audio = read_source(root.join("crates/ratspeak-runtime/src/platform_ios.rs"))
        .expect("ios audio session");

    assert!(memo.contains("const PROFILE: Profile = Profile::QualityMedium"));
    assert!(
        memo.contains("crate::voice::start_microphone_capture(PROFILE, &native_session_token)")
    );
    assert!(voice.contains("pub(crate) fn start_microphone_capture"));
    assert!(voice.contains("MICROPHONE_CAPTURE_RETRY_DELAYS"));
    assert!(memo.contains("RECORDING_STOP_DRAIN_TIMEOUT"));
    assert!(memo.contains("drain_capture_on_stop("));
    assert!(voice.contains("MICROPHONE_CONFIG_ATTEMPT_LIMIT"));
    assert!(voice.contains("fn select_input_configs("));
    assert!(voice.contains("android_microphone_candidate_sample_rates"));
    assert!(voice.contains("ANDROID_MICROPHONE_NATIVE_SAMPLE_RATES"));
    assert!(voice.contains("host.input_devices()"));
    assert!(voice.contains("pub fn reserve_call_audio"));
    assert!(voice.contains("pub fn release_call_audio"));
    let hangup = voice
        .split("pub async fn hangup")
        .nth(1)
        .and_then(|source| source.split("pub async fn reject").next())
        .expect("hangup implementation");
    assert_eq!(
        hangup.matches("release_call_audio").count(),
        1,
        "hangup may release an orphaned reservation when the service is absent, \
         but successful signalling must wait for LXST's terminal event"
    );
    assert!(memo.contains("call_audio_reserved"));
    assert!(memo.contains("_platform_audio_session"));
    assert!(
        memo.contains(
            "VOICE_MEMO_MAX_CONTAINER_BYTES < rns_protocol::resource::MAX_EFFICIENT_SIZE"
        )
    );
    assert!(memo.contains("pub fn parse_recording_session_id"));
    assert!(memo.contains("pub fn parse_playback_lease_id"));
    assert!(memo.contains("take_matching_recording(state, session_id)"));
    assert!(memo.contains("command_tx: mpsc::Sender<PlaybackCommand>"));
    assert!(memo.contains("runtime.block_on(drive_native_playback("));
    assert!(memo.contains("struct NativeVoiceMemoSource"));
    assert!(memo.contains("NATIVE_PLAYBACK_REFILL_TARGET_MS"));
    assert!(voice.contains("VOICE_MEMO_OUTPUT_BUFFER_MS"));
    assert!(voice.contains("FiniteAudioOutput::bounded(max_samples)"));
    assert!(voice.contains(".try_lock()"));
    assert!(commands.contains("VOICE_MEMO_START_UNAVAILABLE"));
    assert!(commands.contains("crate::voice_memo::cancel_recording(&app_state)"));
    assert!(commands.contains("spawn_blocking(move || crate::voice_memo::decode_voice_memo"));
    assert!(commands.contains("pub session_id: String"));
    assert!(commands.contains("pub lease_id: String"));
    assert!(commands.contains("read_bounded_voice_memo"));
    assert!(
        commands.contains(".take((crate::voice_memo::VOICE_MEMO_MAX_CONTAINER_BYTES as u64) + 1)")
    );
    assert!(commands.contains("voice_memo_decode_lock.lock().await"));
    assert!(messaging.contains("RS.invoke('send_lxmf_with_staged_attachment'"));
    assert!(messaging.contains("_conversationOwnerIsCurrent(sendOwner)"));
    assert!(messaging.contains("_cancelStagedAttachmentToken(stageToken)"));
    assert!(messaging.contains("_conversationOwnerIdentityIsCurrent(sendOwner)"));
    assert!(messaging.contains("msg.source = _canonicalConversationHash(msg.source)"));
    assert!(messaging.contains("msg.destination = _canonicalConversationHash(msg.destination)"));
    assert!(
        messaging_commands.contains("sanitize_text(&args.dest_hash, 128).to_ascii_lowercase()")
    );
    assert!(messaging_commands.contains("sanitize_text(&hash, 128).to_ascii_lowercase()"));
    assert!(contact_commands.contains("sanitize_text(&args.hash, 128).to_ascii_lowercase()"));
    assert!(messaging.contains("_voiceCancelMemoForCall().then(function()"));
    assert!(state_js.contains("function _rsNativeMicrophonePermission(audio)"));
    assert!(shared_ui.contains("RS.composer.dismissForReplacement"));
    assert!(voice_memos.contains("window.addEventListener('pagehide'"));
    assert!(!voice_memos.contains("startVoiceMemoAudioSession"));
    assert!(voice_memos.contains("recordingStartRetirement = pendingStart"));
    assert!(voice_memos.contains(
        "if (!eventSessionId || !recordingSessionId || eventSessionId !== recordingSessionId) return;"
    ));
    assert!(voice_memos.contains("cacheGeneration !== mediaCacheGeneration"));
    assert!(voice_memos.contains("var token = ++draftExpirySequence"));
    assert!(voice_memos.contains("leaseId = stoppingLease"));
    assert!(voice_memos.contains("nativeMobilePlaybackByLease[stoppingLease] = handle"));
    assert!(voice_memos.contains("playbackAttemptIsCurrent(coordinator, audio)"));
    assert!(voice_memos.contains("handleAudioInterruption"));
    assert!(voice_memos.contains("RS.audioPlayback.ensure({ installUnlock: true })"));
    assert!(voice_memos.contains("RS.invoke('voice_memo_playback_start'"));
    assert!(voice_memos.contains("'voice_memo_playback_session_stop'"));
    assert!(voice_memos.contains("return createNativeMobilePlayback(item)"));
    assert!(voice_memos.contains("return createMediaPlayback(item)"));
    assert!(voice_memos.contains("classes = ['is-recorded']"));
    assert!(voice_memos.contains("classes.push('is-live')"));
    assert!(voice_memos.contains("class=\"is-empty\""));
    assert!(voice_memos.contains("typeof isIOS === 'function' && isIOS()"));
    assert!(!voice_memos.contains("voice-memo-player-speed"));
    assert!(!voice_memos.contains("playbackSpeed"));
    let call_handoff = voice_memos
        .split("function cancelForCall()")
        .nth(1)
        .and_then(|source| source.split("function handleAudioInterruption()").next())
        .expect("voice memo call handoff");
    let stop_playback = call_handoff
        .find("stopAnyPlayback().then")
        .expect("call handoff stops memo playback");
    let idle_branch = call_handoff
        .find("if (recorderState === 'idle')")
        .expect("call handoff handles an idle recorder");
    assert!(stop_playback < idle_branch);
    assert!(system.contains("mobile_background_voice_memo_cancel_failed"));
    assert!(!android.contains("startVoiceMemoAudioSession"));
    assert!(android_memo_audio.contains("AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE"));
    assert!(android_memo_audio.contains("MODE_IN_COMMUNICATION"));
    assert!(
        android_memo_audio
            .contains("fun startForSession(context: Context, sessionToken: String): Int")
    );
    assert!(android_memo_audio.contains("START_BUSY"));
    assert!(android_memo_audio.contains("fun startPlaybackForSession("));
    assert!(android_memo_audio.contains("fun stopPlaybackForSession("));
    assert!(android_memo_audio.contains("AudioAttributes.USAGE_MEDIA"));
    assert!(android_voice_audio.contains("fun startVoiceMemoPlayback("));
    assert!(android_voice_audio.contains("fun playbackHeadFrames(): Long"));
    assert!(
        android_memo_audio
            .contains("fun stopForSession(context: Context, sessionToken: String): Boolean")
    );
    assert!(android_service.contains("setMicrophoneCaptureActive"));
    assert!(android_service.contains("FOREGROUND_SERVICE_TYPE_MICROPHONE"));
    assert!(ios_audio.contains("AVAudioSessionCategoryPlayAndRecord"));
    assert!(ios_audio.contains("AVAudioSessionModeVoiceChat"));
    assert!(ios_audio.contains("AVAudioSessionCategoryPlayback"));
    assert!(ios_audio.contains("AVAudioSessionModeDefault"));
    assert!(ios_audio.contains("VOICE_MEMO_PLAYBACK_SESSION_ACTIVE"));
    assert!(ios_audio.contains("compare_exchange(lease_id, 0"));
    for command in [
        "voice_memo_start",
        "voice_memo_status",
        "voice_memo_pause",
        "voice_memo_stop",
        "voice_memo_cancel",
        "voice_memo_playback_start",
        "voice_memo_playback_session_stop",
        "voice_memo_decode_data",
        "voice_memo_decode_stored",
        "voice_memo_inspect_stored",
    ] {
        assert!(tauri.contains(&format!("commands::voice::{command}")));
    }
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
    assert!(announce_handlers.contains("mgr.update_lxmf_announce_app_data("));
    assert!(announce_handlers.contains("LXMF_PROPAGATION_APP_NAME"));
}

#[test]
fn public_channels_are_adult_gated_reportable_and_link_to_public_policies() {
    let root = repo_root();
    let db = read_source(root.join("crates/ratspeak-db/src/db.rs")).expect("database source");
    let runtime = read_source(root.join("crates/ratspeak-runtime/src/channels.rs"))
        .expect("channels runtime");
    let commands = read_source(root.join("crates/ratspeak-tauri/src/commands/channels.rs"))
        .expect("channels commands");
    let interfaces = read_source(root.join("crates/ratspeak-tauri/src/commands/interfaces.rs"))
        .expect("settings commands");
    let channels =
        read_source(root.join("dashboard/static/js/channels.js")).expect("channels frontend");
    let nav = read_source(root.join("dashboard/static/js/nav.js")).expect("navigation frontend");
    let legal = read_source(root.join("dashboard/static/js/legal_documents.js"))
        .expect("offline legal documents");
    let index = read_source(root.join("dashboard/index.html")).expect("dashboard entrypoint");
    let state = read_source(root.join("dashboard/static/js/state.js")).expect("shared frontend");
    let tauri = read_source(root.join("src-tauri/src/lib.rs")).expect("tauri entrypoint");
    let android = read_source(
        root.join("src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt"),
    )
    .expect("android activity");

    assert!(db.contains("PUBLIC_CHANNEL_CONSENT_VERSION"));
    assert!(db.contains("PUBLIC_CHANNEL_CONSENT_ACCEPTED_AT_SETTING"));
    assert!(runtime.contains("has_current_public_channel_consent"));
    assert!(runtime.contains("hub.desired_connected = false"));
    assert!(runtime.contains("room.desired_joined = false"));
    assert!(commands.contains("require_public_channel_consent"));
    for command in ["connect_channel_hub", "join_channel"] {
        let block = rust_function_block(&commands, command);
        assert!(block.contains("require_public_channel_consent"));
    }
    assert!(interfaces.contains("adult_confirmed: bool"));
    assert!(interfaces.contains("independent_hubs_understood: bool"));
    assert!(interfaces.contains("policies_accepted: bool"));
    assert!(interfaces.contains("db::try_set_settings"));

    for copy in [
        "Before you enter public channels",
        "I am 18 or older.",
        "I agree to the Terms and Community Guidelines.",
        "independent hubs may contain unmoderated content",
    ] {
        assert!(channels.contains(copy));
    }
    for url in [
        "https://ratspeak.org/privacy.html",
        "https://ratspeak.org/terms.html",
        "https://ratspeak.org/community-guidelines.html",
        "https://ratspeak.org/support.html",
    ] {
        assert!(legal.contains(url));
    }
    assert!(legal.contains("version: '2026-08-11'"));
    assert!(legal.contains("Available offline"));
    assert!(legal.contains("function openDocument(documentId)"));
    assert!(legal.contains("View current version online"));
    assert!(legal.contains("Ratspeak does not currently operate a public channel hub."));
    assert!(legal.contains("Network blackholing may also be available for known identities."));
    assert!(channels.contains("RS.legal.open(documentId)"));
    assert!(channels.contains("RS.legal.open('support')"));
    assert!(nav.contains("data-about-document"));
    assert!(nav.contains("RS.legal.open(documentId)"));
    assert!(index.contains("/static/js/legal_documents.js"));
    assert!(channels.contains("api_blocked_contacts"));
    assert!(channels.contains("block_contact"));
    assert!(channels.contains("Report channel content"));
    assert!(channels.contains("Nothing is sent automatically"));
    assert!(channels.contains("mail@ratspeak.org"));
    assert!(state.contains("window.RS.openSupportEmail"));
    assert!(tauri.contains("fn open_support_email"));
    assert!(tauri.contains("open_support_email,"));
    assert!(android.contains("fun openSupportEmail"));
}
