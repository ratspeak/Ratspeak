#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var root = path.join(__dirname, '..', '..');
var source = fs.readFileSync(path.join(root, 'dashboard/static/js/voice_memos.js'), 'utf8');

function deferred() {
    var resolve;
    var reject;
    var promise = new Promise(function(onResolve, onReject) {
        resolve = onResolve;
        reject = onReject;
    });
    return { promise: promise, resolve: resolve, reject: reject };
}

function makeElement() {
    var handlers = Object.create(null);
    var classes = new Set();
    var properties = Object.create(null);
    return {
        hidden: false,
        disabled: false,
        value: '',
        textContent: '',
        innerHTML: '',
        title: '',
        dataset: {},
        style: {
            values: properties,
            display: '',
            setProperty: function(name, value) { properties[name] = value; },
            removeProperty: function(name) { delete properties[name]; },
        },
        classList: {
            add: function(value) { classes.add(value); },
            remove: function(value) { classes.delete(value); },
            toggle: function(value, force) {
                if (force === undefined ? !classes.has(value) : force) classes.add(value);
                else classes.delete(value);
            },
        },
        addEventListener: function(name, callback) { handlers[name] = callback; },
        fire: function(name, event) { if (handlers[name]) return handlers[name](event || {}); },
        setAttribute: function(name, value) { this[name] = value; },
        querySelector: function(selector) {
            if (selector === '.voice-memo-waveform-played' &&
                this.innerHTML.includes('voice-memo-waveform-played')) return {};
            return { innerHTML: '' };
        },
        blur: function() {},
    };
}

var ids = [
    'voice-memo-record-btn', 'lxmf-voice-recorder', 'lxmf-compose-bar',
    'voice-memo-live-dot', 'voice-memo-play-btn', 'voice-memo-discard-btn',
    'voice-memo-pause-btn', 'voice-memo-stop-btn', 'voice-memo-send-btn',
    'voice-memo-inline-status', 'voice-memo-timer', 'voice-memo-waveform',
    'voice-memo-announcer', 'voice-memo-alert', 'lxmf-input', 'send-msg-btn',
];
var elements = Object.create(null);
ids.forEach(function(id) { elements[id] = makeElement(); });

var documentHandlers = Object.create(null);
var recordingEvent = null;
var playbackEvent = null;
var startRequests = [];
var pauseRequests = [];
var stopRequests = [];
var playbackStarts = [];
var playbackStopRequests = [];
var permissionRequests = [];
var deferPermission = false;
var cancelIds = [];
var cancelledStageTokens = [];
var toasts = [];
var currentHash = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
var epoch = 1;
var identityGeneration = 1;

var context = {
    window: null,
    document: {
        readyState: 'loading',
        hidden: false,
        activeElement: null,
        getElementById: function(id) { return elements[id] || null; },
        querySelector: function() { return null; },
        addEventListener: function(name, callback) { documentHandlers[name] = callback; },
    },
    navigator: {},
    Promise: Promise,
    Object: Object,
    Array: Array,
    String: String,
    Number: Number,
    Math: Math,
    Date: Date,
    Error: Error,
    Uint8Array: Uint8Array,
    Blob: Blob,
    URL: URL,
    isFinite: isFinite,
    setTimeout: function(callback, delay) {
        if (!delay || delay < 100) callback();
        return 1;
    },
    clearTimeout: function() {},
    requestAnimationFrame: function() { return 1; },
    cancelAnimationFrame: function() {},
    showToast: function(message) { toasts.push(message); },
    escapeHtml: function(value) { return String(value); },
    isIOS: function() { return false; },
    isAndroid: function() { return true; },
    lxmfActiveContact: currentHash,
    addEventListener: function() {},
    RS: {
        diag: function() {},
        composer: { dismissForReplacement: function() { return Promise.resolve(); } },
        mediaPermissions: { ensure: function() {
            if (!deferPermission) return Promise.resolve(true);
            var permission = deferred();
            permissionRequests.push(permission);
            return permission.promise;
        } },
        audioPlayback: { ensure: function() { return Promise.resolve(true); } },
        conversationOwner: {
            canonicalHash: function(value) { return String(value || '').trim().toLowerCase(); },
            snapshot: function() {
                return { hash: currentHash, epoch: epoch, identityGeneration: identityGeneration };
            },
            isCurrent: function(owner) {
                return owner && owner.hash === currentHash && owner.epoch === epoch &&
                    owner.identityGeneration === identityGeneration;
            },
            isIdentityCurrent: function(owner) {
                return owner && owner.identityGeneration === identityGeneration;
            },
        },
        invoke: function(command, payload) {
            if (command === 'voice_memo_status') return Promise.resolve({ state: 'idle' });
            if (command === 'voice_memo_start') {
                var request = deferred();
                startRequests.push(request);
                return request.promise;
            }
            if (command === 'voice_memo_cancel') {
                cancelIds.push(payload && payload.args && payload.args.session_id);
                return Promise.resolve({ ok: true });
            }
            if (command === 'voice_memo_stop') {
                var stopRequest = deferred();
                stopRequests.push(stopRequest);
                return stopRequest.promise;
            }
            if (command === 'voice_memo_pause') {
                var pauseRequest = deferred();
                pauseRequests.push(pauseRequest);
                return pauseRequest.promise;
            }
            if (command === 'voice_memo_playback_start') {
                playbackStarts.push(payload.args);
                return Promise.resolve({
                    lease_id: 'vmp-0000000000000001',
                    position_ms: payload.args.position_ms,
                    duration_ms: 1000,
                    waveform: Array.from({ length: 17 }, function() { return 0; }),
                });
            }
            if (command === 'cancel_attachment_stage') {
                cancelledStageTokens.push(payload && payload.token);
                return Promise.resolve({ cancelled: true });
            }
            if (command === 'voice_memo_playback_session_stop') {
                var playbackStop = deferred();
                playbackStopRequests.push(playbackStop);
                return playbackStop.promise;
            }
            return Promise.reject(new Error('Unexpected command: ' + command));
        },
        listen: function(name, callback) {
            if (name === 'voice_memo_recording') recordingEvent = callback;
            if (name === 'voice_memo_playback') playbackEvent = callback;
            return Promise.resolve(function() {});
        },
    },
};
context.window = context;
vm.runInNewContext(source, context, { filename: 'voice_memos.js' });

async function flush() {
    for (var i = 0; i < 40; i++) await Promise.resolve();
}

(async function() {
    documentHandlers.DOMContentLoaded();
    await flush();
    assert(recordingEvent, 'native recording events must be registered');

    elements['voice-memo-record-btn'].fire('click');
    await flush();
    assert.strictEqual(startRequests.length, 1);
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'starting');

    var discard = context.RS.voiceMemos.discard();
    await flush();
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'stopping',
        'unknown-session cancellation must remain busy until native start identifies itself');
    elements['voice-memo-record-btn'].fire('click');
    await flush();
    assert.strictEqual(startRequests.length, 1,
        'a replacement recording must not start while an unidentified native session is retiring');

    recordingEvent({ state: 'recording', session_id: 'vmr-0000000000000001', level: 200 });
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'stopping',
        'events are ignored until the exact returned recording session is installed');
    startRequests[0].resolve({ session_id: 'vmr-0000000000000001' });
    await discard;
    await flush();
    assert.deepStrictEqual(cancelIds, ['vmr-0000000000000001']);
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'idle');

    currentHash = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
    context.lxmfActiveContact = currentHash;
    epoch += 1;
    elements['voice-memo-record-btn'].fire('click');
    await flush();
    assert.strictEqual(startRequests.length, 2);
    recordingEvent({ state: 'paused', session_id: 'vmr-0000000000000001' });
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'starting');
    startRequests[1].resolve({ session_id: 'vmr-0000000000000002' });
    await flush();
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'recording');
    recordingEvent({ state: 'paused', session_id: 'vmr-0000000000000001' });
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'recording',
        'a retired session event must not mutate its replacement');
    recordingEvent({ state: 'paused', session_id: 'vmr-0000000000000002' });
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'paused');

    elements['voice-memo-stop-btn'].fire('click');
    await flush();
    assert.strictEqual(stopRequests.length, 1);
    var discardDuringStop = context.RS.voiceMemos.discard();
    await discardDuringStop;
    assert.strictEqual(cancelIds[cancelIds.length - 1], 'vmr-0000000000000002');
    stopRequests[0].resolve({
        session_id: 'vmr-0000000000000002',
        staging_token: 'staged-voice-after-retirement',
        data_base64: 'container',
        duration_ms: 60,
        waveform: [32],
    });
    await flush();
    assert.deepStrictEqual(cancelledStageTokens, ['staged-voice-after-retirement'],
        'a stop completion retired by cancellation must remove its exact private staging token');

    elements['voice-memo-record-btn'].fire('click');
    await flush();
    startRequests[2].resolve({ session_id: 'vmr-0000000000000003' });
    await flush();
    elements['voice-memo-pause-btn'].fire('click');
    await flush();
    assert.strictEqual(pauseRequests.length, 1);
    elements['voice-memo-stop-btn'].fire('click');
    await flush();
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'stopping');
    pauseRequests[0].resolve({ paused: true });
    await flush();
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'stopping',
        'a late pause completion cannot move a command-owned stop backwards');
    recordingEvent({
        state: 'recording',
        session_id: 'vmr-0000000000000003',
        duration_ms: 940,
        level: 220,
    });
    recordingEvent({
        state: 'paused',
        session_id: 'vmr-0000000000000003',
        duration_ms: 960,
    });
    recordingEvent({
        state: 'idle',
        session_id: 'vmr-0000000000000003',
        duration_ms: 1_000,
    });
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'stopping',
        'late exact-session events cannot move a command-owned stop backwards');
    stopRequests[1].resolve({
        session_id: 'vmr-0000000000000003',
        staging_token: 'staged-one-second-silence',
        data_base64: 'container',
        duration_ms: 1000,
        waveform: Array.from({ length: 17 }, function() { return 0; }),
    });
    await flush();
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'review',
        'a valid one-second draft must survive late recording events and remain sendable');
    assert.deepStrictEqual(cancelledStageTokens, ['staged-voice-after-retirement'],
        'the valid stop result must not be misclassified and cancelled as stale');
    assert.strictEqual(elements['voice-memo-timer'].textContent, '0:01');
    assert(elements['voice-memo-waveform'].innerHTML.includes('voice-memo-waveform-played'));

    elements['voice-memo-play-btn'].fire('click');
    await flush();
    assert.strictEqual(playbackStarts.length, 1);
    playbackEvent({
        lease_id: 'vmp-0000000000000001',
        state: 'playing',
        position_ms: 400,
        duration_ms: 1000,
    });
    await flush();
    assert.strictEqual(
        elements['voice-memo-waveform'].style.values['--voice-playback-unplayed'],
        '60%',
        'pre-send review must reveal the orange waveform at the native playback position',
    );

    var discardPlayedMemo = context.RS.voiceMemos.discard();
    await flush();
    assert.strictEqual(playbackStopRequests.length, 1,
        'discard must retire the exact active native preview lease');
    elements['voice-memo-play-btn'].fire('click');
    await flush();
    assert.strictEqual(playbackStarts.length, 1,
        'preview admission must close immediately while discard teardown is pending');
    playbackStopRequests[0].resolve({ released: true, position_ms: 400 });
    await discardPlayedMemo;
    await flush();
    assert(cancelledStageTokens.includes('staged-one-second-silence'),
        'discard must cancel the exact private stage owned by memo A');
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'idle');

    elements['voice-memo-record-btn'].fire('click');
    await flush();
    startRequests[3].resolve({ session_id: 'vmr-0000000000000004' });
    await flush();
    elements['voice-memo-stop-btn'].fire('click');
    await flush();
    stopRequests[2].resolve({
        session_id: 'vmr-0000000000000004',
        staging_token: 'staged-replacement-memo',
        data_base64: 'replacement-container',
        duration_ms: 1000,
        waveform: Array.from({ length: 17 }, function(_, index) { return index + 1; }),
    });
    await flush();
    elements['voice-memo-play-btn'].fire('click');
    await flush();
    assert.strictEqual(playbackStarts.length, 2);
    assert.strictEqual(playbackStarts[0].data_base64, 'container');
    assert.strictEqual(playbackStarts[1].data_base64, 'replacement-container',
        'memo B preview must never reuse memo A after A was discarded');

    var discardReplacement = context.RS.voiceMemos.discard();
    await flush();
    playbackStopRequests[1].resolve({ released: true, position_ms: 0 });
    await discardReplacement;
    deferPermission = true;
    elements['voice-memo-record-btn'].fire('click');
    await flush();
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'requesting_permission');
    assert.strictEqual(permissionRequests.length, 1);
    context.document.hidden = true;
    documentHandlers.visibilitychange();
    await flush();
    assert.strictEqual(elements['lxmf-voice-recorder'].dataset.state, 'idle',
        'a hidden native permission sheet may retire the unadmitted request safely');
    permissionRequests[0].resolve(true);
    await flush();
    assert.strictEqual(startRequests.length, 4,
        'permission completion after lifecycle retirement must not start a hidden microphone');
    assert(!toasts.some(function(message) { return message.includes('discarded while Ratspeak'); }),
        'the first microphone permission sheet must not claim a voice message was discarded');
    console.log('Voice recording ownership tests passed');
})().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
