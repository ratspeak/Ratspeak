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
    return {
        hidden: false,
        disabled: false,
        value: '',
        textContent: '',
        innerHTML: '',
        title: '',
        dataset: {},
        style: {},
        classList: { toggle: function() {} },
        addEventListener: function(name, callback) { handlers[name] = callback; },
        fire: function(name, event) { if (handlers[name]) return handlers[name](event || {}); },
        setAttribute: function(name, value) { this[name] = value; },
        querySelector: function() { return { innerHTML: '' }; },
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
var startRequests = [];
var stopRequests = [];
var cancelIds = [];
var cancelledStageTokens = [];
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
    setTimeout: function(callback) { callback(); return 1; },
    clearTimeout: function() {},
    requestAnimationFrame: function() { return 1; },
    cancelAnimationFrame: function() {},
    showToast: function() {},
    escapeHtml: function(value) { return String(value); },
    isIOS: function() { return false; },
    lxmfActiveContact: currentHash,
    addEventListener: function() {},
    RS: {
        diag: function() {},
        composer: { dismissForReplacement: function() { return Promise.resolve(); } },
        mediaPermissions: { ensure: function() { return Promise.resolve(true); } },
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
            if (command === 'cancel_attachment_stage') {
                cancelledStageTokens.push(payload && payload.token);
                return Promise.resolve({ cancelled: true });
            }
            if (command === 'voice_memo_playback_session_stop') return Promise.resolve({ released: true });
            return Promise.reject(new Error('Unexpected command: ' + command));
        },
        listen: function(name, callback) {
            if (name === 'voice_memo_recording') recordingEvent = callback;
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
    console.log('Voice recording ownership tests passed');
})().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
