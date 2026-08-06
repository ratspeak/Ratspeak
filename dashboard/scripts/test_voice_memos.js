#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');

var root = path.join(__dirname, '..', '..');
function read(relative) {
    return fs.readFileSync(path.join(root, relative), 'utf8');
}

var html = read('dashboard/index.html');
var voice = read('dashboard/static/js/voice_memos.js');
var messaging = read('dashboard/static/js/lxmf.js');
var css = read('dashboard/static/css/09-messaging.css');
var responsive = read('dashboard/static/css/13-responsive.css');
var capture = read('crates/ratspeak-runtime/src/voice.rs');
var runtime = read('crates/ratspeak-runtime/src/voice_memo.rs');
var commands = read('crates/ratspeak-tauri/src/commands/voice.rs');
var tauri = read('src-tauri/src/lib.rs');
var state = read('dashboard/static/js/state.js');
var sharedUi = read('dashboard/static/js/ui_shared.js');
var systemCommands = read('crates/ratspeak-tauri/src/commands/system.rs');
var iosPlatform = read('crates/ratspeak-runtime/src/platform_ios.rs');
var androidActivity = read('src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt');
var androidManifest = read('src-tauri/gen/android/app/src/main/AndroidManifest.xml');
var iosInfo = read('src-tauri/Info.plist');

assert(html.includes('id="voice-memo-record-btn"'));
assert(html.includes('id="lxmf-voice-recorder"'));
assert(html.includes('id="voice-memo-pause-btn"'));
assert(html.includes('id="voice-memo-stop-btn"'));
assert(html.includes('id="voice-memo-send-btn"'));
assert(html.includes('class="lxmf-compose message-composer voice-memo-recorder"'),
    'voice recording must retain the established composer rails');
assert(html.includes('class="voice-memo-field"'),
    'the recording surface must replace only the text-input slot');
assert(html.indexOf('id="voice-memo-discard-btn"') < html.indexOf('class="voice-memo-field"') &&
    html.indexOf('class="voice-memo-field"') < html.indexOf('id="voice-memo-stop-btn"'),
    'discard, recording field, and primary action must keep the normal composer order');
assert(html.indexOf('/static/js/lxmf.js') < html.indexOf('/static/js/voice_memos.js'),
    'voice memos must extend the established LXMF composer after it loads');

for (var command of [
    'voice_memo_start',
    'voice_memo_status',
    'voice_memo_pause',
    'voice_memo_stop',
    'voice_memo_cancel',
    'voice_memo_decode_data',
    'voice_memo_decode_stored',
    'voice_memo_inspect_stored',
]) {
    assert(commands.includes('fn ' + command), command + ' command must exist');
    assert(tauri.includes('commands::voice::' + command), command + ' must be registered');
}

assert(voice.includes("RS.mediaPermissions.ensure({ audio: true })"),
    'recording must use the unified native permission broker');
assert(voice.includes('RS.composer.dismissForReplacement'),
    'recording must share the composer keyboard-dismiss transition');
assert(sharedUi.includes('RS.composer.dismissForReplacement'),
    'composer replacements need one shared mobile keyboard transition');
assert(state.includes('function _rsNativeMicrophonePermission(audio)'),
    'Apple desktop and iOS must share the native microphone permission broker');
assert(state.includes('isTauriMobile() &&') && state.includes('isIOS()'),
    'iOS must request permission through the same native API used for capture');
assert(voice.includes('showToast(START_FAILURE_MESSAGE'),
    'native audio diagnostics must not leak into the user-facing toast');
assert(voice.includes("record.addEventListener('pointerdown'"),
    'touch-and-hold recording must start immediately on pointer-down');
assert(voice.includes("setRecorderState('review')"),
    'recordings must be reviewed before transfer');
assert(voice.includes('stop.disabled = busy'),
    'the stable stop-action slot must remain present but inert during native transitions');
assert(voice.includes('recordingTarget !== window.lxmfActiveContact'),
    'a draft must never cross conversation boundaries');
assert(voice.includes("document.addEventListener('visibilitychange'"),
    'an unseen WebView must not keep the microphone active');
assert(voice.includes("window.addEventListener('pagehide'"),
    'a replaced mobile WebView must not keep the microphone active');
assert(voice.includes('startVoiceMemoAudioSession'),
    'Android recording must acquire transient audio focus');
assert(voice.includes('stopVoiceMemoAudioSession'),
    'Android recording must release transient audio focus');
assert(voice.includes('handleAudioInterruption'),
    'native audio interruptions must stop the recorder visibly');
assert(voice.includes('MAX_PLAYBACK_BYTES'),
    'decoded WAV object URLs must live in a bounded cache');
assert(messaging.includes("RS.invoke('send_lxmf_with_attachment'"));
assert(messaging.includes("_updateConversationPreview(targetHash, 'Voice message'"));
assert(messaging.includes('RS.voiceMemos.hydratePlayers(container)'));

assert(runtime.includes('const MAGIC: &[u8; 4] = b"LXVM"'));
assert(runtime.includes('const VERSION: u8 = 1'));
assert(runtime.includes('const PROFILE: Profile = Profile::QualityMedium'));
assert(runtime.includes('VOICE_MEMO_MAX_DURATION_MS: u32 = 5 * 60 * 1_000'));
assert(runtime.includes('MAX_CONTAINER_BYTES < rns_protocol::resource::MAX_EFFICIENT_SIZE'));
assert(runtime.includes('parser_rejects_unbounded_counts_before_allocating'));
assert(capture.includes('MICROPHONE_CAPTURE_RETRY_DELAYS'));
assert(capture.includes('host.input_devices()'),
    'capture startup must fall back from a stale CoreAudio default device');
assert(capture.includes('reserve_call_audio'));
assert(capture.includes('release_call_audio'));
assert(runtime.includes('call_audio_reserved'));
assert(runtime.includes('_platform_audio_session'));
assert(commands.includes('VOICE_MEMO_START_UNAVAILABLE'));
assert(commands.includes('reserve_call_audio'));
assert(commands.includes('release_call_audio'));
assert(commands.includes('spawn_blocking(move || crate::voice_memo::decode_voice_memo'));
assert(systemCommands.includes('mobile_background_voice_memo_cancel_failed'));
assert(iosPlatform.includes('AVAudioSessionCategoryPlayAndRecord'));
assert(iosPlatform.includes('AVAudioSessionModeVoiceChat'));
assert(iosPlatform.includes('setActive: true'));

assert(/\.voice-memo-field\s*\{[\s\S]*?background:\s*var\(--input-bg\)/.test(css),
    'the recorder field must use the same themed input surface as text composition');
assert(/\.voice-memo-recorder\s*\{[\s\S]*?background:\s*transparent/.test(css),
    'the recorder shell must not become a second full-width pill');
assert(/prefers-reduced-motion:[^)]*reduce[\s\S]*?voice-memo/.test(css),
    'voice animation must honor reduced motion');
assert(/\.voice-memo-record-btn,[\s\S]*?width:\s*44px/.test(responsive),
    'mobile recorder controls must retain 44px touch targets');
assert(/\.voice-memo-field\s*\{[\s\S]*?min-height:\s*44px/.test(responsive),
    'the mobile recording field must retain the composer touch-height contract');
assert(androidManifest.includes('android.permission.RECORD_AUDIO'));
assert(androidActivity.includes('AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE'));
assert(androidActivity.includes('fun startVoiceMemoAudioSession(): Boolean'));
assert(androidActivity.includes('fun stopVoiceMemoAudioSession()'));
assert(androidActivity.includes('dispatchVoiceMemoAudioInterruption'));
assert(iosInfo.includes('voice messages'));

console.log('Voice memo tests passed');
