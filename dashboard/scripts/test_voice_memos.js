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
var oggOpus = read('crates/ratspeak-runtime/src/voice_memo/ogg_opus.rs');
var commands = read('crates/ratspeak-tauri/src/commands/voice.rs');
var messagingCommands = read('crates/ratspeak-tauri/src/commands/messaging.rs');
var tauri = read('src-tauri/src/lib.rs');
var state = read('dashboard/static/js/state.js');
var sharedUi = read('dashboard/static/js/ui_shared.js');
var systemCommands = read('crates/ratspeak-tauri/src/commands/system.rs');
var iosPlatform = read('crates/ratspeak-runtime/src/platform_ios.rs');
var androidActivity = read('src-tauri/gen/android/app/src/main/java/org/ratspeak/android/MainActivity.kt');
var androidMemoAudio = read('src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakVoiceMemoAudio.kt');
var androidVoiceAudio = read('src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakVoiceAudio.kt');
var androidService = read('src-tauri/gen/android/app/src/main/java/org/ratspeak/android/RatspeakService.kt');
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
    'send_lxmf_voice_message',
    'voice_memo_playback_start',
    'voice_memo_playback_session_stop',
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
assert(state.includes("ctx.state === 'suspended' || ctx.state === 'interrupted'"),
    'iOS audio interruptions must be resumed before voice-message playback');
assert(!state.includes("_rsAudioPlaybackUnlocked = ctx.state === 'running' || ctx.state === 'interrupted'"),
    'an interrupted iOS audio context must never be treated as already unlocked');
assert(state.includes("ctx.state !== 'interrupted' &&") && state.includes("ctx.state !== 'closed'"),
    'shared audio readiness must exclude interrupted and closed contexts');
assert(state.includes('isTauriMobile() &&') && state.includes('isIOS()'),
    'iOS must request permission through the same native API used for capture');
assert(voice.includes("error.code === 'conflict'") &&
    voice.includes('audioBusy && error.message ? error.message : START_FAILURE_MESSAGE'),
    'only the reviewed microphone-contention error may replace the generic start toast');
assert(voice.includes("record.addEventListener('pointerdown'"),
    'touch-and-hold recording must start immediately on pointer-down');
assert(voice.includes("setRecorderState('review')"),
    'recordings must be reviewed before transfer');
assert(voice.includes("announce('Voice message queued to send')") &&
    !voice.includes("announce('Voice message sent')"),
    'backend admission must not be announced as network delivery');
assert(voice.includes('stop.disabled = busy'),
    'the stable stop-action slot must remain present but inert during native transitions');
assert(voice.includes('conversationOwnerIsCurrent(recordingOwner)'),
    'a draft must never cross canonical conversation ownership');
assert(voice.includes('recordingStartRetirement === startPromise') &&
    voice.includes('recordingStartRetirement = pendingStart'),
    'an unknown native start must remain owned until its returned session is exactly cancelled');
assert(voice.includes('if (!eventSessionId || !recordingSessionId || eventSessionId !== recordingSessionId) return;'),
    'native recording events must require an installed exact session');
assert(messaging.includes("RS.listen('contact_added'") &&
    !messaging.slice(messaging.indexOf("RS.listen('contact_added'"), messaging.indexOf("RS.listen('contact_error'")).includes('lxmfActiveContact ='),
    'contact-added events must remain observational and never navigate');
assert(voice.includes("document.addEventListener('visibilitychange'"),
    'an unseen WebView must not keep the microphone active');
assert(voice.includes("window.addEventListener('pagehide'"),
    'a replaced mobile WebView must not keep the microphone active');
assert(!voice.includes('startVoiceMemoAudioSession') && !voice.includes('stopVoiceMemoAudioSession'),
    'Android memo audio lifetime must not be owned by the replaceable WebView');
assert(runtime.includes('format_recording_session_id(session_id)') &&
    capture.includes('VoiceMemoAudioSessionGuard::start(session_token)'),
    'Rust recording generations must own exact Android native sessions');
assert(voice.includes('MAX_PLAYBACK_BYTES'),
    'decoded WAV object URLs must live in a bounded cache');
assert(voice.includes('cacheGeneration !== mediaCacheGeneration') &&
    voice.includes('var token = ++draftExpirySequence') &&
    voice.includes('if (draftExpiryTokenByKey[key] !== token) return;'),
    'decode/metadata use cache generations while draft expiry uses a non-reusable ABA token');
assert(state.includes('RS.voiceMemos.releaseInactiveMedia(!!(payload && payload.critical))'),
    'critical native pressure must reach the active voice media owner');
assert(voice.includes("RS.audioPlayback.ensure({ installUnlock: true })"),
    'desktop media playback must retain the shared audio unlock path');
assert(voice.includes("RS.invoke('voice_memo_playback_start'"),
    'mobile playback must start native PCM output from the bounded Ogg/Opus source');
assert(voice.includes("RS.invoke('voice_memo_playback_session_stop'"),
    'mobile playback must stop its exact native worker on pause, end, and handoff');
assert(voice.includes('leaseId = stoppingLease') &&
    voice.includes('nativeMobilePlaybackByLease[stoppingLease] = handle'),
    'a failed native stop must retain the exact lease for a later teardown retry');
assert(voice.includes('if (usesNativeVoiceMemoPlayback()) return createNativeMobilePlayback(item);') &&
    voice.includes('return createMediaPlayback(item);'),
    'playback dispatch must use native mobile output without changing desktop media output');
assert(voice.includes('if (usesNativeVoiceMemoPlayback()) {') &&
    voice.includes('var decode = decodeDraftOrStored(source).then'),
    'mobile playback must bypass WAV expansion while desktop retains its media path');
assert(voice.indexOf('stopAnyPlayback().then', voice.indexOf('function cancelForCall()')) <
    voice.indexOf("if (recorderState === 'idle')", voice.indexOf('function cancelForCall()')),
    'starting a call must stop memo playback even when the recorder is idle');
assert(!voice.includes('voice-memo-player-speed') && !voice.includes('playbackSpeed'),
    'voice messages must not expose distracting playback-speed controls');
assert(voice.includes("waveform.setAttribute('aria-disabled', seekAvailable ? 'false' : 'true')"),
    'waveform seeking must only become interactive with an exact live playback handle');
assert(voice.includes('if (!playbackAttemptIsCurrent(coordinator, audio)) return false;'),
    'play and recovery completions must be fenced to their captured coordinator and audio handle');
assert(!html.includes('voice-memo-icon-resume') && !html.includes('voice-memo-icon-pause'),
    'recorder actions must render one stateful icon instead of stacked play/pause glyphs');
assert(voice.includes('class="voice-memo-player-icon"') &&
    !voice.includes('voice-memo-player-icon-play') &&
    !voice.includes('voice-memo-player-icon-pause'),
    'message playback must render exactly one stateful play/pause glyph');
var voiceSend = messaging.split('function sendLxmfVoiceMemo(')[1].split('window.sendLxmfVoiceMemo')[0];
assert(voiceSend.includes("RS.invoke('send_lxmf_voice_message'"));
assert(voiceSend.includes('voiceDraft.staging_token'));
assert(!voiceSend.includes('send_lxmf_with_staged_attachment'));
assert(!voiceSend.includes('_stageAttachmentBlob'));
assert(!voiceSend.includes('data_base64'));
assert(messaging.includes("_updateConversationPreview(targetHash, 'Voice message'"));
assert(messaging.includes('if (msg.audio && window.RS && RS.voiceMemos)'));
assert(messaging.includes('RS.voiceMemos.hydratePlayers(container)'));
assert(voice.includes('Number(audio && audio.mode) === 0x10'));
assert(voice.includes('var unsupported = !isAudio(audio) || audio.supported === false'));
assert(voice.includes('data-audio-supported'));
assert(voice.includes('Array.from({ length: BAR_COUNT }, function() { return 0; })'),
    'received audio without decoded metadata must use a neutral waveform');
assert(commands.includes('begin_attachment_staging('));
assert(commands.includes('take_completed_attachment_staging(&args.staging_token)'));
assert(commands.includes('crate::voice_memo::inspect_voice_memo(&inspection_bytes)'));
assert(commands.includes('crate::commands::messaging::queue_prepared_audio('));
assert(messagingCommands.includes('send_audio_message_with_preference_report(AudioMessageRequest'));
assert(!voice.includes('.lxvm') && !messaging.includes('.lxvm') &&
    !commands.includes('.lxvm') && !messagingCommands.includes('.lxvm'));

assert(runtime.includes('const PROFILE: Profile = Profile::QualityMedium'));
assert(runtime.includes('VOICE_MEMO_MAX_DURATION_MS: u32 = 5 * 60 * 1_000'));
assert(runtime.includes('VOICE_MEMO_MAX_GENERATED_OGG_BYTES < rns_protocol::resource::MAX_EFFICIENT_SIZE'));
assert(oggOpus.includes('b"OpusHead"') && oggOpus.includes('b"OpusTags"'));
assert(oggOpus.includes('maximum_recording_ogg_size_matches_the_compile_time_resource_bound'));
assert(oggOpus.includes('data.len() > VOICE_MEMO_MAX_AUDIO_BYTES'));
assert(runtime.includes('struct NativeVoiceMemoSource') &&
    runtime.includes('NATIVE_PLAYBACK_REFILL_TARGET_MS'),
    'native playback must incrementally decode the bounded Ogg/Opus packet stream');
assert(capture.includes('startup_prime_frames: startup_frames') &&
    capture.includes('android_memo_needs_refill(submitted, played, self.startup_prime_frames)') &&
    capture.includes('voice_memo_startup_prime_frames()?') &&
    capture.includes('finish_voice_memo_input()?'),
    'Android memo playback must prime from the effective native AudioTrack requirement');
assert(runtime.includes('runtime.block_on(drive_native_playback(') &&
    runtime.includes('command_tx: mpsc::Sender<PlaybackCommand>'),
    'native mobile audio objects must remain inside their dedicated blocking worker');
assert(capture.includes('MICROPHONE_CAPTURE_RETRY_DELAYS'));
assert(capture.includes('host.input_devices()'),
    'capture startup must fall back from a stale CoreAudio default device');
assert(capture.includes('reserve_call_audio'));
assert(capture.includes('release_call_audio'));
assert(capture.includes('VOICE_MEMO_OUTPUT_BUFFER_MS') &&
    capture.includes('FiniteAudioOutput::bounded(max_samples)') &&
    capture.includes('.try_lock()'),
    'iOS native memo output must use a fixed PCM ring without blocking its realtime callback');
assert(capture.includes('start_voice_memo_playback') &&
    androidVoiceAudio.includes('startVoiceMemoPlayback') &&
    androidVoiceAudio.includes('playbackHeadFrames') &&
    androidVoiceAudio.includes('voiceMemoStartupPrimeFrames') &&
    androidVoiceAudio.includes('voiceMemoPlaybackStarted') &&
    androidVoiceAudio.includes('finishVoiceMemoInput'),
    'Android native memo output must use the proven AudioTrack bridge and its playback clock');
assert(androidVoiceAudio.includes('Build.VERSION.SDK_INT < Build.VERSION_CODES.S') &&
    androidVoiceAudio.includes('RatspeakMobilePolicy.voiceMemoStartupFrames') &&
    androidVoiceAudio.includes('trackSubmittedFrames += writtenFrames.toLong()'),
    'pre-API31 and short memos must fill the actual startup threshold without extending their timeline');
assert(runtime.includes('call_audio_reserved'));
assert(runtime.includes('_platform_audio_session'));
assert(commands.includes('VOICE_MEMO_START_UNAVAILABLE'));
assert(commands.includes('reserve_call_audio'));
assert(commands.includes('release_call_audio'));
assert(commands.includes('spawn_blocking(move || crate::voice_memo::decode_voice_memo'));
assert(systemCommands.includes('mobile_background_voice_memo_cancel_failed'));
assert(iosPlatform.includes('AVAudioSessionCategoryPlayAndRecord'));
assert(iosPlatform.includes('AVAudioSessionModeVoiceChat'));
assert(iosPlatform.includes('AVAudioSessionCategoryPlayback'));
assert(iosPlatform.includes('AVAudioSessionModeDefault'));
assert(iosPlatform.includes('VOICE_MEMO_PLAYBACK_SESSION_ACTIVE'));
assert(iosPlatform.includes('setActive: true'));

assert(/\.voice-memo-field\s*\{[\s\S]*?background:\s*var\(--input-bg\)/.test(css),
    'the recorder field must use the same themed input surface as text composition');
assert(/\.voice-memo-recorder\s*\{[\s\S]*?background:\s*transparent/.test(css),
    'the recorder shell must not become a second full-width pill');
assert(voice.includes("classes = ['is-recorded']") && voice.includes("classes.push('is-live')"),
    'the live waveform must distinguish captured signal from its single newest sample');
assert(voice.includes("class=\"is-empty\""),
    'the live waveform must leave uncaptured timeline slots visibly empty');
assert(/\.voice-memo-waveform\s*>\s*span\.is-recorded\s*\{[\s\S]*?background:\s*var\(--status-error\)/.test(css),
    'only recorded waveform samples should use the recording color');
assert(voice.includes('setPlaybackWaveformProgress(waveform, draft.waveform || [], fraction)') &&
    voice.includes("waveform.style.setProperty('--voice-playback-unplayed'") &&
    voice.includes("voice-memo-waveform-layer voice-memo-waveform-played"),
    'preview and message playback must share one stable elapsed-waveform treatment');
assert(/\.voice-memo-waveform-played\s*\{[\s\S]*?clip-path:\s*inset\(0 var\(--voice-playback-unplayed\) 0 0\)/.test(css) &&
    /\.voice-memo-waveform-played\s*>\s*span\s*\{[\s\S]*?background:\s*var\(--accent\)/.test(css),
    'elapsed waveform audio must reveal the existing orange accent over the muted base');
assert(/prefers-reduced-motion:[^)]*reduce[\s\S]*?voice-memo/.test(css),
    'voice animation must honor reduced motion');
assert(/\.voice-memo-record-btn,[\s\S]*?width:\s*44px/.test(responsive),
    'mobile recorder controls must retain 44px touch targets');
assert(/\.voice-memo-field\s*\{[\s\S]*?min-height:\s*44px/.test(responsive),
    'the mobile recording field must retain the composer touch-height contract');
assert(/\.voice-memo-player-play\s*\{[\s\S]*?width:\s*44px;[\s\S]*?height:\s*44px/.test(responsive),
    'mobile voice-message playback must retain a 44px touch target');
assert(androidManifest.includes('android.permission.RECORD_AUDIO'));
assert(!androidActivity.includes('startVoiceMemoAudioSession'));
assert(androidMemoAudio.includes('AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE'));
assert(androidMemoAudio.includes('MODE_IN_COMMUNICATION'));
assert(androidMemoAudio.includes('fun startForSession(context: Context, sessionToken: String): Int'));
assert(androidMemoAudio.includes('START_BUSY'));
assert(androidMemoAudio.includes('fun stopForSession(context: Context, sessionToken: String): Boolean'));
assert(androidService.includes('setMicrophoneCaptureActive') &&
    androidService.includes('FOREGROUND_SERVICE_TYPE_MICROPHONE'));
assert(iosInfo.includes('voice messages'));

console.log('Voice memo tests passed');
