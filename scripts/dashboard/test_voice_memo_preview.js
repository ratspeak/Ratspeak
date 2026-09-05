#!/usr/bin/env node
'use strict';

// Exercise the actual record -> Stop -> draft preview controller, not an
// already-sent bubble populated through registerDraft(). No audio hardware is
// simulated as audible: native clock/error/ended events are explicit inputs.
var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');
var source = fs.readFileSync(path.join(__dirname, '../../dashboard/static/js/voice_memos.js'), 'utf8');

async function flush() {
    for (var i = 0; i < 100; i++) await Promise.resolve();
}
function deferred() {
    var resolve, reject;
    var promise = new Promise(function(ok, fail) { resolve = ok; reject = fail; });
    return { promise: promise, resolve: resolve, reject: reject };
}
function element() {
    var handlers = {}, classes = new Set(), icon = { innerHTML: '' };
    return {
        dataset: {}, value: '', textContent: '', innerHTML: '', disabled: false,
        style: { setProperty: function() {}, removeProperty: function() {} },
        classList: {
            add: function(v) { classes.add(v); }, remove: function(v) { classes.delete(v); },
            toggle: function(v, on) { if (on) classes.add(v); else classes.delete(v); },
        },
        setAttribute: function(k, v) { this[k] = v; },
        getAttribute: function(k) { return this[k]; },
        addEventListener: function(k, fn) { handlers[k] = fn; },
        fire: function(k, value) { return handlers[k] && handlers[k](value || {}); },
        querySelector: function(selector) {
            return selector === '.voice-memo-state-icon' ? icon :
                this.innerHTML.includes('voice-memo-waveform-played') ? {} : null;
        },
    };
}
function harness(platform) {
    var nodes = {}, events = {}, timers = [], clock = 0, starts = [], stops = [], recordings = [], media = [], sent = [], toasts = [];
    var draft = { data_base64: 'b2dnLW9wdXMtYnl0ZXM=', duration_ms: 4000,
        waveform: [20, 80, 120], staging_token: 'stage-preview', size: 14 };
    var hooks = {}, nextLease = 0;
    function node(id) { return nodes[id] || (nodes[id] = element()); }
    function lease(n) { return 'vmp-' + String(n).padStart(16, '0'); }
    var context = {
        document: {
            readyState: 'complete', hidden: false, getElementById: node,
            querySelector: function() { return null; }, addEventListener: function() {},
        },
        window: null, navigator: {}, Promise: Promise, Object: Object, Array: Array,
        String: String, Number: Number, Math: Math, Error: Error, Uint8Array: Uint8Array,
        Blob: Blob, URL: URL, atob: atob, isFinite: isFinite,
        setTimeout: function(fn, delay) {
            var timer = { fn: fn, at: clock + (delay || 0), active: true };
            timers.push(timer); return timer;
        },
        clearTimeout: function(timer) { if (timer) timer.active = false; },
        addEventListener: function() {}, showToast: function(message) { toasts.push(message); },
        isAndroid: function() { return platform === 'android'; },
        isIOS: function() { return platform === 'ios'; },
        lxmfActiveContact: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
        sendLxmfVoiceMemo: function(value) { sent.push(value); return Promise.resolve(); },
        Audio: function() {
            assert.equal(platform, 'desktop', 'mobile preview must not use WebView audio');
            var handlers = {};
            this.currentTime = 0; this.duration = 4; this.paused = true;
            this.addEventListener = function(k, fn) { handlers[k] = fn; };
            this.emit = function(k) { if (handlers[k]) handlers[k](); };
            this.play = function() { this.paused = false; return Promise.resolve(); };
            this.pause = function() { this.paused = true; };
            media.push(this);
        },
        RS: {
            diag: function() {}, config: { VOICE_PLAYBACK_START_TIMEOUT: 2000 },
            mediaPermissions: { ensure: function() { return Promise.resolve(true); } },
            composer: { dismissForReplacement: function() { return Promise.resolve(); } },
            listen: function(k, fn) { events[k] = fn; return Promise.resolve(function() {}); },
            invoke: function(command, payload) {
                if (command === 'voice_memo_status') return Promise.resolve({ state: 'idle' });
                if (command === 'voice_memo_start') {
                    recordings.push(command);
                    return Promise.resolve({ session_id: 'vmr-0000000000000001' });
                }
                if (command === 'voice_memo_stop') return Promise.resolve(draft);
                if (command === 'voice_memo_decode_data') {
                    assert.equal(platform, 'desktop', 'mobile preview must decode natively');
                    return Promise.resolve({ data_base64: 'AQIDBA==', duration_ms: draft.duration_ms, waveform: draft.waveform });
                }
                if (command === 'voice_memo_playback_start') {
                    var call = { args: payload.args, lease: lease(++nextLease) };
                    starts.push(call);
                    return hooks.start ? hooks.start(call) : Promise.resolve({
                        lease_id: call.lease, position_ms: call.args.position_ms, duration_ms: draft.duration_ms,
                    });
                }
                if (command === 'voice_memo_playback_session_stop') {
                    stops.push(payload.args.lease_id);
                    return hooks.stop ? hooks.stop(payload.args.lease_id) : Promise.resolve({ released: true });
                }
                if (command === 'cancel_attachment_stage') return Promise.resolve({ cancelled: true });
                throw new Error('Unexpected command: ' + command);
            },
        },
    };
    context.window = context;
    vm.runInNewContext(source, context, { filename: 'voice_memos.js' });
    return {
        starts: starts, stops: stops, recordings: recordings, sent: sent, draft: draft, hooks: hooks, context: context,
        toasts: toasts, previewButton: node('voice-memo-play-btn'),
        state: function() { return node('voice-memo-play-btn').dataset.playbackState; },
        timer: function() { return node('voice-memo-timer').textContent; },
        click: async function(id) { node('voice-memo-' + id + '-btn').fire('click'); await flush(); },
        record: async function() {
            await flush(); node('voice-memo-record-btn').fire('click'); await flush();
            node('voice-memo-stop-btn').fire('click'); await flush();
            assert.equal(node('lxmf-voice-recorder').dataset.state, 'review');
            assert.equal(node('voice-memo-timer').textContent, '0:04');
        },
        event: function(state, position, leaseId) {
            if (platform === 'desktop') {
                var audio = media[media.length - 1];
                audio.currentTime = position / 1000;
                if (state !== 'playing') audio.paused = true;
                audio.emit(state === 'playing' ? 'timeupdate' : state);
            } else {
                events.voice_memo_playback({ lease_id: leaseId || starts[starts.length - 1].lease,
                    state: state, position_ms: position, duration_ms: draft.duration_ms });
            }
        },
        advance: async function(ms) {
            var end = clock + ms, count = 0;
            while (true) {
                var due = timers.filter(function(t) { return t.active && t.at <= end; })
                    .sort(function(a, b) { return a.at - b.at; })[0];
                if (!due) break;
                assert(++count < 100, 'watchdog must not spin');
                clock = due.at; due.active = false; due.fn(); await flush();
            }
            clock = end; await flush();
        },
    };
}

var cases = [
    ['controller reports the original recorded duration before any playback', async function(platform) {
        var h = harness(platform); await h.record();
        assert.equal(h.timer(), '0:04');
        await h.advance(10000); assert.equal(h.timer(), '0:04', 'review must not keep a recording clock running');
        await h.click('play'); assert.equal(h.timer(), '0:00');
        assert.equal(h.state(), 'starting', 'command admission alone is not audible progress');
    }],
    ['preview handles native/media errors after playback has started', async function(platform) {
        var h = harness(platform); await h.record(); await h.click('play');
        h.event('playing', 800); assert.equal(h.state(), 'playing');
        h.event('error', 800); await flush();
        assert.equal(h.state(), 'error', 'preview must not keep claiming playback after an output error');
        assert(h.toasts.some(function(message) { return message.includes('preview'); }), 'failure must be visible, not only written into a hidden status node');
        assert(h.previewButton['aria-label'].includes('Try'));
        assert.equal(h.previewButton.disabled, false, 'failed preview must remain retryable');
        await h.click('send');
        assert.equal(h.sent[0].data_base64, h.draft.data_base64, 'a playback error must not destroy or alter sendable audio');
    }],
    ['preview cannot run beyond its recorded duration if ended is lost', async function(platform) {
        var h = harness(platform); await h.record(); await h.click('play');
        h.event('playing', 1000); h.event('playing', 9000); await flush();
        assert.equal(h.timer(), '0:04', 'elapsed preview time must stay within the actual memo');
        assert.equal(h.state(), 'ended', 'proven native/media completion must retire the preview');
    }],
    ['preview detects a stall after initial progress with one bounded recovery', async function(platform) {
        var h = harness(platform); await h.record(); await h.click('play');
        h.event('playing', 800); await h.advance(2100);
        assert.equal(h.state(), 'recovering', 'first progress must not disable liveness checks forever');
        if (platform !== 'desktop') assert.equal(h.starts[1].args.position_ms, 800, 'recovery retains the last proven position');
        await h.advance(2100); assert.equal(h.state(), 'error');
        var starts = h.starts.length; await h.advance(10000); assert.equal(h.starts.length, starts);
    }],
    ['paused preview does not time out and resumes at the same position', async function(platform) {
        var h = harness(platform); await h.record(); await h.click('play');
        h.event('playing', 800); await h.click('play'); assert.equal(h.state(), 'paused');
        await h.advance(10000); assert.equal(h.state(), 'paused');
        await h.click('play'); h.event('playing', 1000); assert.equal(h.state(), 'playing');
        h.event('ended', 4000); assert.equal(h.state(), 'ended');
    }],
    ['healthy preview advances until the exact end and replays the same draft', async function(platform) {
        var h = harness(platform); await h.record(); await h.click('play');
        for (var position of [800, 1800, 2800, 3800]) {
            h.event('playing', position); await h.advance(900);
            assert.equal(h.state(), 'playing', 'healthy clock progress must refresh liveness');
        }
        h.event('ended', 4000); await flush();
        assert.equal(h.state(), 'ended'); assert.equal(h.timer(), '0:04');
        await h.click('play');
        assert.equal(h.state(), 'starting'); assert.equal(h.timer(), '0:00');
        if (platform !== 'desktop') {
            assert.equal(h.starts.length, 2);
            assert.equal(h.starts[1].args.data_base64, h.draft.data_base64);
            assert.equal(h.starts[1].args.position_ms, 0);
            assert.equal(h.starts[1].args.stored_name, undefined, 'preview uses the actual unsent bytes');
        }
    }],
    ['a backwards position change does not trigger a false stall', async function(platform) {
        var h = harness(platform); await h.record(); await h.click('play');
        h.event('playing', 3000);
        for (var position of [200, 500, 1000, 1500]) {
            h.event('playing', position); await h.advance(900);
            assert.equal(h.state(), 'playing');
        }
        if (platform !== 'desktop') assert.equal(h.starts.length, 1, 'backward seek must not recreate healthy output');
    }],
    ['discard during recovery cannot restart retired preview or modify its replacement', async function(platform) {
        var h = harness(platform); await h.record(); await h.click('play');
        h.event('playing', 800); await h.advance(2100);
        var retiredLease = platform === 'desktop' ? null : h.starts[h.starts.length - 1].lease;
        await h.click('discard'); await h.advance(10000);
        assert.equal(h.state(), 'idle');
        await h.record(); await h.click('play');
        if (retiredLease) {
            h.event('ended', 4000, retiredLease); h.event('error', 1000, retiredLease);
            assert.equal(h.state(), 'starting', 'stale native events must not terminate a replacement preview');
        }
    }],
    ['early native end/error is retained until its start reply installs the exact lease', async function(platform) {
        if (platform === 'desktop') return;
        for (var state of ['ended', 'error']) {
            var h = harness(platform), reply = deferred(); await h.record();
            h.hooks.start = function(call) {
                h.event(state, state === 'ended' ? 4000 : 0, call.lease);
                return reply.promise;
            };
            await h.click('play');
            reply.resolve({ lease_id: h.starts[0].lease, position_ms: 0, duration_ms: 4000 }); await flush();
            assert.equal(h.state(), state, 'the start reply must not overwrite an earlier terminal native event');
            await h.advance(5000); assert.equal(h.starts.length, 1, 'completed/failed output must not spuriously restart');
        }
    }],
    ['a late start reply is retired before any replacement opens native output', async function(platform) {
        if (platform === 'desktop') return;
        var h = harness(platform), reply = deferred(); await h.record();
        h.hooks.start = function() { return reply.promise; };
        await h.click('play'); await h.advance(3000);
        assert.equal(h.state(), 'starting', 'a slower native decode/IPC reply gets a separate admission deadline');
        await h.advance(7100); assert.equal(h.state(), 'error', 'native start cannot leave an endless spinner');
        h.hooks.start = null;
        await h.click('play'); assert.equal(h.starts.length, 1, 'replacement must wait for exact pending-start retirement');
        reply.resolve({ lease_id: h.starts[0].lease, position_ms: 0, duration_ms: 4000 }); await flush();
        assert.equal(h.stops[0], h.starts[0].lease);
        assert.equal(h.starts.length, 2);
        assert.equal(h.state(), 'starting');
        h.event('playing', 1000, h.starts[0].lease);
        assert.equal(h.state(), 'starting', 'late events cannot revive retired output');
        h.event('playing', 1000); assert.equal(h.state(), 'playing');
    }],
    ['unsettled native start bounds retries and recording without losing late lease ownership', async function(platform) {
        if (platform === 'desktop') return;
        var h = harness(platform), reply = deferred(); await h.record();
        h.hooks.start = function() { return reply.promise; };
        await h.click('play'); await h.advance(10100);
        await h.click('play'); await h.advance(10100);
        assert.equal(h.state(), 'error', 'retry cannot silently wait forever for retirement');
        assert(h.toasts.some(function(message) { return /restart Ratspeak/i.test(message); }), 'explain the safe recovery path');
        assert.equal(h.starts.length, 1, 'never open replacement output while the old start is unresolved');
        await h.click('discard'); await h.advance(10100);
        await h.click('record'); await h.advance(10100);
        assert.equal(h.context.document.getElementById('lxmf-voice-recorder').dataset.state, 'idle');
        assert.equal(h.recordings.length, 1, 'recording cannot bypass unresolved output retirement');
        h.hooks.start = null;
        reply.resolve({ lease_id: h.starts[0].lease, position_ms: 0, duration_ms: 4000 }); await flush();
        assert(h.stops.includes(h.starts[0].lease), 'late output still receives exact-lease cleanup');
        await h.record(); await h.click('play');
        assert.equal(h.starts.length, 2, 'a settled late retirement permits a fresh explicit action');
    }],
    ['unsettled native stop bounds recovery and rejected stops remain retryable', async function(platform) {
        if (platform === 'desktop') return;
        var h = harness(platform), reply = deferred(); await h.record();
        await h.click('play'); h.event('playing', 1000);
        h.hooks.stop = function() { return reply.promise; };
        await h.advance(2100); assert.equal(h.state(), 'recovering');
        await h.advance(10100); assert.equal(h.state(), 'error', 'recovery teardown has a deadline too');
        reply.reject(new Error('native stop rejected')); await flush();
        await h.click('play');
        assert.equal(h.starts.length, 1, 'a failed release cannot be treated as permission to start');
        assert.equal(h.state(), 'error');
        h.hooks.stop = null;
        await h.click('play');
        assert.equal(h.stops[h.stops.length - 1], h.starts[0].lease, 'retry the exact failed lease');
        assert.equal(h.starts.length, 2);
    }],
    ['failed native start does not keep unrelated early events for a later attempt', async function(platform) {
        if (platform === 'desktop') return;
        var h = harness(platform); await h.record();
        h.hooks.start = function(call) {
            h.event('ended', 4000, 'vmp-0000000000000002');
            throw new Error('native invocation failed');
        };
        await h.click('play'); assert.equal(h.state(), 'error');
        h.hooks.start = null;
        await h.click('play'); assert.equal(h.state(), 'starting');
        h.event('playing', 1000); assert.equal(h.state(), 'playing');
    }],
];

(async function() {
    var failed = 0;
    for (var platform of ['android', 'ios', 'desktop']) {
        for (var entry of cases) {
            if (platform === 'desktop' && entry[0].includes('native') && !entry[0].includes('native/media')) continue;
            try { await entry[1](platform); console.log('PASS ' + platform + ': ' + entry[0]); }
            catch (error) { failed++; console.error('FAIL ' + platform + ': ' + entry[0] + '\n' + error.stack); }
        }
    }
    assert.equal(failed, 0, failed + ' preview regressions failed');
})().catch(function(error) { console.error(error); process.exitCode = 1; });
