#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var root = path.join(__dirname, '..', '..');
var source = fs.readFileSync(path.join(root, 'dashboard/static/js/voice_memos.js'), 'utf8');
var clickHandler = null;
var decodeCalls = 0;
var sourceStarts = 0;
var sourceStops = 0;
var toasts = [];
var sessionCalls = [];
var failSessionStart = false;
var failSessionStopOnce = false;
var stoppedLeaseIds = [];
var rafCallback = null;
var scheduledTimeouts = [];

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
var waveform = {
    innerHTML: '',
    addEventListener: function() {},
    setAttribute: function(name, value) { this[name] = value; },
    getAttribute: function(name) { return this[name]; },
    getBoundingClientRect: function() { return { left: 0, width: 100 }; },
};
var time = { textContent: '' };
var player = {
    dataset: { voiceKey: 'memo-test', storedName: '' },
    classList: classList(),
    querySelector: function(selector) {
        if (selector === '.voice-memo-player-play') return playButton;
        if (selector === '.voice-memo-player-waveform') return waveform;
        if (selector === '.voice-memo-player-time') return time;
        if (selector === '.voice-memo-player-icon') return icon;
        return null;
    },
};
var container = {
    querySelectorAll: function(selector) {
        return selector === '.voice-memo-player' ? [player] : [];
    },
};

var audioContext = {
    currentTime: 0,
    destination: {},
    decodeAudioData: function(_data, success) {
        decodeCalls += 1;
        success({ duration: 4 });
        return Promise.resolve({ duration: 4 });
    },
    createBufferSource: function() {
        return {
            buffer: null,
            onended: null,
            connect: function() {},
            disconnect: function() {},
            start: function() { sourceStarts += 1; },
            stop: function() { sourceStops += 1; },
        };
    },
};

var context = {
    window: null,
    document: {
        readyState: 'loading',
        addEventListener: function() {},
        getElementById: function() { return null; },
        querySelector: function(selector) {
            return selector.indexOf('memo-test') !== -1 ? player : null;
        },
    },
    navigator: { userAgent: 'iPhone', platform: 'iPhone', maxTouchPoints: 1 },
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
    setTimeout: function(callback) { scheduledTimeouts.push(callback); return scheduledTimeouts.length; },
    clearTimeout: function() {},
    requestAnimationFrame: function(callback) { rafCallback = callback; return 1; },
    cancelAnimationFrame: function() {},
    CustomEvent: function() {},
    CSS: { escape: function(value) { return value; } },
    escapeHtml: function(value) { return value; },
    isIOS: function() { return true; },
    showToast: function(message) { toasts.push(message); },
    RS: {
        diag: function() {},
        invoke: function(command, payload) {
            if (command === 'voice_memo_decode_data') {
                return Promise.resolve({
                    mime: 'audio/wav',
                    data_base64: 'AQIDBA==',
                    duration_ms: 4000,
                    waveform: [30, 80, 120],
                });
            }
            if (command === 'voice_memo_playback_session_start') {
                sessionCalls.push('start');
                return failSessionStart
                    ? Promise.reject(new Error('audio session unavailable'))
                    : Promise.resolve({ ok: true, lease_id: 'vmp-0000000000000001' });
            }
            if (command === 'voice_memo_playback_session_stop') {
                sessionCalls.push('stop');
                stoppedLeaseIds.push(payload && payload.args && payload.args.lease_id);
                if (failSessionStopOnce) {
                    failSessionStopOnce = false;
                    return Promise.reject(new Error('temporary release failure'));
                }
                return Promise.resolve({ ok: true });
            }
            return Promise.reject(new Error('Unexpected command: ' + command));
        },
        audioPlayback: {
            ensure: function() { return Promise.resolve(true); },
            context: function() { return audioContext; },
            isReady: function() { return true; },
        },
    },
};
context.window = context;
vm.runInNewContext(source, context, { filename: 'voice_memos.js' });

async function flush() {
    for (var i = 0; i < 100; i++) await Promise.resolve();
}

(async function() {
    var html = context.RS.voiceMemos.renderAttachment({
        voice_memo_key: 'memo-test',
        voice_memo: { duration_ms: 4000, waveform: [30, 80, 120] },
    }, { id: 'message-test' });
    assert.equal((html.match(/<svg/g) || []).length, 1,
        'the voice player must render one stateful play/pause icon');
    assert(!html.includes('voice-memo-player-speed'));
    assert(!html.includes('1×'));

    context.RS.voiceMemos.registerDraft('memo-test', { data_base64: 'container' });
    var draftExpiry = scheduledTimeouts[scheduledTimeouts.length - 1];
    context.RS.voiceMemos.hydratePlayers(container);
    assert(clickHandler, 'the player must bind its play action');

    clickHandler();
    await flush();
    assert.equal(decodeCalls, 1, 'iOS must decode through the shared Web Audio context');
    assert.equal(sourceStarts, 1, 'the decoded voice message must begin playback');
    assert.deepEqual(sessionCalls, ['start'],
        'iOS must activate its playback-only audio session before starting PCM');
    assert.equal(playButton['aria-label'], 'Starting playback',
        'a resolved start request must not claim playback before time advances');
    assert(icon.innerHTML.includes('M8 5v14l11-7z'), 'starting must not show pause');
    assert.equal(waveform['aria-disabled'], 'true');
    assert.equal(waveform.tabIndex, -1,
        'seeking must remain unavailable until an exact playback handle is active');

    scheduledTimeouts[scheduledTimeouts.length - 1]();
    await flush();
    assert.equal(sourceStarts, 2, 'a frozen first start gets one fresh bounded recovery');
    assert.equal(player.dataset.playbackState, 'recovering');
    scheduledTimeouts[scheduledTimeouts.length - 1]();
    await flush();
    assert.equal(player.dataset.playbackState, 'error',
        'a second frozen start must settle on an honest retryable error');
    assert.equal(sourceStarts, 2, 'recovery must not loop');
    assert.equal(waveform['aria-disabled'], 'true', 'failed playback must not expose a no-op slider');

    failSessionStopOnce = true;
    clickHandler();
    await flush();
    assert.equal(sourceStarts, 3, 'the explicit retry creates one new attempt');
    audioContext.currentTime = 0.08;
    rafCallback();
    await flush();
    assert.equal(playButton['aria-label'], 'Pause voice message');
    assert(icon.innerHTML.includes('M6 5h4v14H6z'), 'proven playback must show pause');
    assert.equal(waveform['aria-disabled'], 'false');
    assert.equal(waveform.tabIndex, 0, 'proven playback may expose exact-handle seeking');
    assert.deepEqual(toasts, []);

    clickHandler();
    await flush();
    assert(sourceStops >= 3, 'recovery and pause must close their exact buffer sources');
    assert.equal(sessionCalls[sessionCalls.length - 1], 'stop',
        'pausing must release the iOS playback session');
    assert.equal(playButton['aria-label'], 'Play voice message');
    assert(icon.innerHTML.includes('M8 5v14l11-7z'), 'the paused state must show only play');
    assert.equal(waveform['aria-disabled'], 'false', 'the retained paused handle must remain seekable');
    var stopCountAfterFailure = stoppedLeaseIds.length;
    await context.RS.voiceMemos.stopPlayback();
    await flush();
    assert.equal(stoppedLeaseIds.length, stopCountAfterFailure + 1,
        'a failed exact native release must retain its lease for teardown retry');
    assert.equal(stoppedLeaseIds[stoppedLeaseIds.length - 1], 'vmp-0000000000000001');

    context.lxstVoiceState = { active: true, incoming: false };
    clickHandler();
    await flush();
    assert.equal(sourceStarts, 3, 'an active call must prevent paused memo playback from resuming');
    assert(toasts.some(function(message) { return message.includes('current call'); }),
        'call-owned audio must explain why memo playback is unavailable');

    context.lxstVoiceState = { active: false, incoming: false };
    failSessionStart = true;
    clickHandler();
    await flush();
    assert.equal(sourceStarts, 3,
        'visual playback must not begin when iOS cannot acquire an audible session');
    assert.equal(player.dataset.playbackState, 'error',
        'an unavailable native output session must surface a retryable playback error');

    context.RS.voiceMemos.releaseInactiveMedia(false);
    draftExpiry();
    var expiredHtml = context.RS.voiceMemos.renderAttachment({
        voice_memo_key: 'memo-test',
        voice_memo: { duration_ms: 4000, waveform: [30, 80, 120] },
    }, { id: 'message-test' });
    assert(expiredHtml.includes('disabled'),
        'memory pressure must not prevent a retained outgoing draft from reaching its expiry');

    console.log('Voice memo playback tests passed');
})().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
