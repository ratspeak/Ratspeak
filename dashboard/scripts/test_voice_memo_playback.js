#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');
var childProcess = require('child_process');

var androidScenario = process.argv.includes('--android');
var desktopScenario = process.argv.includes('--desktop');
var platformName = desktopScenario ? 'desktop' : androidScenario ? 'Android' : 'iOS';

var root = path.join(__dirname, '..', '..');
var source = fs.readFileSync(path.join(root, 'dashboard/static/js/voice_memos.js'), 'utf8');
var clickHandler = null;
var seekHandler = null;
var nativeEvents = null;
var decodeCalls = 0;
var mediaConstructs = 0;
var audioUnlockCalls = 0;
var nativeStarts = [];
var nativeStops = [];
var toasts = [];
var failNativeStart = false;
var failNativeStopOnce = false;
var nextLease = 1;
var scheduledTimeouts = [];
var rejectDesktopDecode = null;
var resolveMetadataInspect = null;

function classList() {
    var values = new Set();
    return {
        add: function(value) { values.add(value); },
        remove: function(value) { values.delete(value); },
        toggle: function(value, force) {
            if (force === undefined ? !values.has(value) : force) values.add(value);
            else values.delete(value);
        },
        contains: function(value) { return values.has(value); },
    };
}

var icon = { innerHTML: '' };
var playButton = {
    disabled: false,
    title: '',
    addEventListener: function(name, callback) {
        if (name === 'click') clickHandler = callback;
    },
    setAttribute: function(name, value) { this[name] = value; },
};
var waveformInnerHtml = '';
var waveformInnerHtmlWrites = 0;
var waveformStyle = Object.create(null);
var waveform = {
    dataset: {},
    style: {
        setProperty: function(name, value) { waveformStyle[name] = value; },
        removeProperty: function(name) { delete waveformStyle[name]; },
    },
    tabIndex: -1,
    addEventListener: function(name, callback) {
        if (name === 'click') seekHandler = callback;
    },
    setAttribute: function(name, value) { this[name] = value; },
    getAttribute: function(name) { return this[name]; },
    querySelector: function(selector) {
        return selector === '.voice-memo-waveform-played' &&
            waveformInnerHtml.includes('voice-memo-waveform-played') ? {} : null;
    },
    getBoundingClientRect: function() { return { left: 0, width: 100 }; },
};
Object.defineProperty(waveform, 'innerHTML', {
    get: function() { return waveformInnerHtml; },
    set: function(value) {
        waveformInnerHtml = value;
        waveformInnerHtmlWrites += 1;
    },
});
var time = { textContent: '' };
var status = { textContent: '' };
var player = {
    dataset: { voiceKey: 'memo-test', storedName: '' },
    classList: classList(),
    querySelector: function(selector) {
        if (selector === '.voice-memo-player-play') return playButton;
        if (selector === '.voice-memo-player-waveform') return waveform;
        if (selector === '.voice-memo-player-time') return time;
        if (selector === '.voice-memo-player-icon') return icon;
        if (selector === '.voice-memo-player-status') return status;
        return null;
    },
};
var visiblePlayer = player;
var container = {
    querySelectorAll: function(selector) {
        return selector === '.voice-memo-player' ? [player] : [];
    },
};

function leaseId(value) {
    return 'vmp-' + String(value).padStart(16, '0');
}

var context = {
    window: null,
    document: {
        readyState: 'complete',
        hidden: false,
        addEventListener: function() {},
        getElementById: function() { return null; },
        querySelector: function(selector) {
            return selector.indexOf('memo-test') !== -1 ? visiblePlayer : null;
        },
    },
    navigator: androidScenario
        ? { userAgent: 'Android', platform: 'Linux armv8l', maxTouchPoints: 5 }
        : desktopScenario
            ? { userAgent: 'Macintosh', platform: 'MacIntel', maxTouchPoints: 0 }
            : { userAgent: 'iPhone', platform: 'iPhone', maxTouchPoints: 1 },
    Promise: Promise,
    Uint8Array: Uint8Array,
    Object: Object,
    Math: Math,
    Number: Number,
    String: String,
    Array: Array,
    Error: Error,
    isFinite: isFinite,
    atob: atob,
    Blob: Blob,
    URL: URL,
    setTimeout: function(callback) {
        var timer = { callback: callback, cleared: false };
        scheduledTimeouts.push(timer);
        return timer;
    },
    clearTimeout: function(timer) { if (timer) timer.cleared = true; },
    requestAnimationFrame: function() { return 1; },
    cancelAnimationFrame: function() {},
    CustomEvent: function() {},
    CSS: { escape: function(value) { return value; } },
    escapeHtml: function(value) { return value; },
    isIOS: function() { return !androidScenario && !desktopScenario; },
    isAndroid: function() { return androidScenario; },
    showToast: function(message) { toasts.push(message); },
    addEventListener: function() {},
    Audio: function() {
        mediaConstructs += 1;
        throw new Error('mobile voice messages must not construct a WebView media element');
    },
    RS: {
        config: { VOICE_PLAYBACK_START_TIMEOUT: 2000 },
        diag: function() {},
        listen: function(name, callback) {
            if (name === 'voice_memo_playback') nativeEvents = callback;
            return Promise.resolve(function() {});
        },
        invoke: function(command, payload) {
            if (command === 'voice_memo_status') return Promise.resolve({ state: 'idle' });
            if (command === 'voice_memo_decode_data' || command === 'voice_memo_decode_stored') {
                decodeCalls += 1;
                if (desktopScenario) {
                    return new Promise(function(_resolve, reject) { rejectDesktopDecode = reject; });
                }
                return Promise.resolve({
                    mime: 'audio/wav',
                    data_base64: 'AQIDBA==',
                    duration_ms: 4000,
                    waveform: [30, 80, 120],
                });
            }
            if (command === 'voice_memo_playback_start') {
                nativeStarts.push(payload.args);
                if (failNativeStart) return Promise.reject(new Error('native output unavailable'));
                return Promise.resolve({
                    lease_id: leaseId(nextLease++),
                    position_ms: payload.args.position_ms,
                    duration_ms: 4000,
                    waveform: [30, 80, 120],
                });
            }
            if (command === 'voice_memo_playback_session_stop') {
                nativeStops.push(payload.args.lease_id);
                if (failNativeStopOnce) {
                    failNativeStopOnce = false;
                    return Promise.reject(new Error('temporary release failure'));
                }
                return Promise.resolve({ ok: true, released: true, position_ms: 1200 });
            }
            if (command === 'voice_memo_inspect_stored') {
                return new Promise(function(resolve) { resolveMetadataInspect = resolve; });
            }
            return Promise.reject(new Error('Unexpected command: ' + command));
        },
        audioPlayback: {
            ensure: function() { audioUnlockCalls += 1; return Promise.resolve(true); },
        },
    },
};
context.window = context;
vm.runInNewContext(source, context, { filename: 'voice_memos.js' });

async function flush() {
    for (var i = 0; i < 100; i++) await Promise.resolve();
}

async function runLatestTimeout() {
    for (var i = scheduledTimeouts.length - 1; i >= 0; i--) {
        var timer = scheduledTimeouts[i];
        if (!timer.cleared) {
            timer.cleared = true;
            timer.callback();
            await flush();
            return;
        }
    }
    throw new Error('No live timeout was scheduled');
}

(async function() {
    if (desktopScenario) {
        context.RS.voiceMemos.registerDraft('memo-test', {
            data_base64: 'container',
            duration_ms: 4000,
            waveform: [30, 80, 120],
        });
        context.RS.voiceMemos.hydratePlayers(container);
        clickHandler();
        var decodeReplacement = {
            dataset: { voiceKey: 'memo-test', storedName: '', playbackState: 'idle' },
            classList: classList(),
            querySelector: player.querySelector,
        };
        visiblePlayer = decodeReplacement;
        await flush();
        assert(rejectDesktopDecode, 'desktop decode must remain pending for the rerender race');
        rejectDesktopDecode(new Error('decode unavailable'));
        await flush();
        assert.equal(decodeReplacement.dataset.playbackState, 'error',
            'an async decode failure must settle on the current rerendered player');
        console.log('desktop voice memo async rerender tests passed');
        return;
    }
    assert(nativeEvents, platformName + ' must subscribe to exact native playback progress events');
    var unsupportedHtml = context.RS.voiceMemos.renderAudio({
        mode: 0x20,
        supported: false,
        stored_name: 'unknown-audio.bin',
    }, { id: 'unsupported-message' });
    assert(unsupportedHtml.includes('Unsupported audio') && unsupportedHtml.includes('disabled'),
        'unknown FIELD_AUDIO modes must render a clear disabled row without decode admission');
    var html = context.RS.voiceMemos.renderAudio({
        mode: 0x10,
        supported: true,
        voice_memo_key: 'memo-test',
        voice_memo: { duration_ms: 4000, waveform: [30, 80, 120] },
    }, { id: 'message-test' });
    assert.equal((html.match(/<svg/g) || []).length, 1,
        'the voice player must render one stateful play/pause icon');
    assert(!html.includes('voice-memo-player-speed'));

    context.RS.voiceMemos.registerDraft('memo-test', {
        data_base64: 'container',
        duration_ms: 4000,
        waveform: [30, 80, 120],
    });
    var draftExpiry = scheduledTimeouts[scheduledTimeouts.length - 1];
    context.RS.voiceMemos.hydratePlayers(container);
    assert(clickHandler, 'the player must bind its play action');
    assert(seekHandler, 'the player must bind its seek action');

    clickHandler();
    var startReplacement = {
        dataset: { voiceKey: 'memo-test', storedName: '', playbackState: 'idle' },
        classList: classList(),
        querySelector: player.querySelector,
    };
    visiblePlayer = startReplacement;
    await flush();
    assert.equal(startReplacement.dataset.playbackState, 'starting',
        'async startup must bind to the current rerendered player');
    visiblePlayer = player;
    delete player.dataset.voiceBound;
    context.RS.voiceMemos.hydratePlayers(container);
    assert.equal(nativeStarts.length, 1, platformName + ' must start one native playback worker');
    assert.equal(nativeStarts[0].data_base64, 'container');
    assert.equal(nativeStarts[0].position_ms, 0);
    assert.equal(decodeCalls, 0, platformName + ' must not expand Ogg/Opus into WAV/base64 IPC');
    assert.equal(mediaConstructs, 0, platformName + ' must not use WebView media output');
    assert.equal(audioUnlockCalls, 0, 'native mobile output must not depend on Web Audio readiness');
    assert.equal(player.dataset.playbackState, 'starting');
    assert.equal(playButton['aria-label'], 'Starting playback',
        'a successful start command must not claim playback before the audio clock advances');
    assert(icon.innerHTML.includes('M8 5v14l11-7z'));
    assert.equal(waveform['aria-disabled'], 'true');

    nativeEvents({
        lease_id: leaseId(99),
        state: 'playing',
        position_ms: 100,
        duration_ms: 4000,
    });
    assert.equal(player.dataset.playbackState, 'starting', 'stale native leases must be ignored');

    nativeEvents({
        lease_id: leaseId(1),
        state: 'playing',
        position_ms: 80,
        duration_ms: 4000,
    });
    await flush();
    assert.equal(player.dataset.playbackState, 'playing');
    assert.equal(playButton['aria-label'], 'Pause voice message');
    assert(icon.innerHTML.includes('M6 5h4v14H6z'));
    assert.equal(waveform['aria-disabled'], 'false');
    assert.equal(waveform.tabIndex, 0);
    assert.equal(waveformStyle['--voice-playback-unplayed'], '98%',
        'native output position must reveal the matching orange waveform fraction');
    var stableWaveformWrites = waveformInnerHtmlWrites;
    nativeEvents({
        lease_id: leaseId(1),
        state: 'playing',
        position_ms: 1200,
        duration_ms: 4000,
    });
    await flush();
    assert.equal(waveformStyle['--voice-playback-unplayed'], '70%');
    assert.equal(waveformInnerHtmlWrites, stableWaveformWrites,
        'playback progress must update one CSS value without rebuilding waveform bars');

    clickHandler();
    await flush();
    assert.equal(nativeStops[nativeStops.length - 1], leaseId(1),
        'pause must stop only the exact native playback lease');
    assert.equal(player.dataset.playbackState, 'paused');
    var pausedHtml = context.RS.voiceMemos.renderAudio({
        mode: 0x10,
        supported: true,
        voice_memo_key: 'memo-test',
        voice_memo: { duration_ms: 4000, waveform: [30, 80, 120] },
    }, { id: 'message-test' });
    assert(pausedHtml.includes('aria-valuetext="0:01 of 0:04"'),
        'a paused rerender must restore the elapsed accessibility value');
    assert(pausedHtml.includes('<span class="voice-memo-player-time">0:01</span>'),
        'a paused rerender must restore the elapsed visible time');
    player.dataset.storedName = 'stored-voice.ogg';
    context.RS.voiceMemos.releaseInactiveMedia(false);
    var storedReplacement = {
        dataset: { voiceKey: 'memo-test', storedName: 'stored-voice.ogg', playbackState: 'idle' },
        classList: classList(),
        querySelector: player.querySelector,
    };
    player = storedReplacement;
    visiblePlayer = storedReplacement;
    context.RS.voiceMemos.hydratePlayers(container);
    assert(resolveMetadataInspect, 'stored metadata must rehydrate after noncritical media pressure');
    resolveMetadataInspect({ duration_ms: 4000, waveform: [30, 80, 120] });
    await flush();
    assert.equal(player.dataset.playbackState, 'paused');
    assert.equal(waveformStyle['--voice-playback-unplayed'], '70%');
    assert.equal(waveform['aria-valuenow'], '30');
    assert.equal(waveform['aria-valuetext'], '0:01 of 0:04');
    assert.equal(waveform['aria-disabled'], 'false');
    assert.equal(time.textContent, '0:01');
    seekHandler({ clientX: 50 });
    await flush();
    clickHandler();
    await flush();
    assert.equal(nativeStarts[nativeStarts.length - 1].position_ms, 2000,
        'resume after a paused seek must restart native output at the requested position');

    nativeEvents({ lease_id: leaseId(2), state: 'playing', position_ms: 2080, duration_ms: 4000 });
    await flush();
    failNativeStopOnce = true;
    clickHandler();
    await flush();
    var failedStopCount = nativeStops.length;
    await context.RS.voiceMemos.stopPlayback();
    await flush();
    assert.equal(nativeStops.length, failedStopCount + 1,
        'a failed exact stop must retain its lease for teardown retry');
    assert.equal(nativeStops[nativeStops.length - 1], leaseId(2));

    clickHandler();
    await flush();
    var frozenFirstLease = leaseId(3);
    assert.equal(nativeStarts.length, 3);
    await runLatestTimeout();
    assert.equal(player.dataset.playbackState, 'recovering');
    assert.equal(nativeStops[nativeStops.length - 1], frozenFirstLease,
        'bounded recovery must close its exact frozen worker');
    assert.equal(nativeStarts.length, 4, 'a frozen native start gets one fresh recovery');
    await runLatestTimeout();
    assert.equal(player.dataset.playbackState, 'error',
        'a second frozen native start must settle on an honest retry state');
    assert.equal(nativeStarts.length, 4, 'native recovery must not loop');

    clickHandler();
    await flush();
    assert.equal(waveformStyle['--voice-playback-unplayed'], '100%',
        'retrying an ended or failed memo must reset stale orange progress before playback');
    nativeEvents({ lease_id: leaseId(5), state: 'playing', position_ms: 80, duration_ms: 4000 });
    clickHandler();
    await flush();
    var startsBeforeCall = nativeStarts.length;
    context.lxstVoiceState = { active: true, incoming: false };
    clickHandler();
    await flush();
    assert.equal(nativeStarts.length, startsBeforeCall,
        'an active LXST call must prevent memo playback from acquiring output');
    assert(toasts.some(function(message) { return message.includes('current call'); }));

    context.lxstVoiceState = { active: false, incoming: false };
    failNativeStart = true;
    clickHandler();
    await flush();
    assert.equal(player.dataset.playbackState, 'error',
        'native output startup failure must surface a retryable playback error');

    context.RS.voiceMemos.releaseInactiveMedia(false);
    draftExpiry.callback();
    var expiredHtml = context.RS.voiceMemos.renderAudio({
        mode: 0x10,
        supported: true,
        voice_memo_key: 'memo-test',
        voice_memo: { duration_ms: 4000, waveform: [30, 80, 120] },
    }, { id: 'message-test' });
    assert(expiredHtml.includes('disabled'),
        'memory pressure must not prevent a retained outgoing draft from reaching its expiry');

    console.log(platformName + ' voice memo playback tests passed');
    if (!androidScenario && !desktopScenario) {
        childProcess.execFileSync(process.execPath, [__filename, '--android'], { stdio: 'inherit' });
        childProcess.execFileSync(process.execPath, [__filename, '--desktop'], { stdio: 'inherit' });
    }
})().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
