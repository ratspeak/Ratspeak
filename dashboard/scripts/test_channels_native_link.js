#!/usr/bin/env node
// Deterministic lifecycle coverage for acknowledged native channel previews.
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');
var source = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'native_channel_share.js'), 'utf8');

function target(hub, room) {
    return { format: 'ratspeak.channel.v1', payload: 'ratspeak://channel?v=1&hub=' + hub,
        hub_destination_hash: hub, room: room || null };
}
var first = target('00112233445566778899aabbccddeeff');
var second = target('ffeeddccbbaa99887766554433221100', 'field');

function inbox() {
    return { pending: null, revision: 0, accept: function(value) {
        this.pending = { revision: String(++this.revision), activityGeneration: '1', target: value };
    } };
}

function page(native, options) {
    options = options || {};
    var state = { setup: !!options.setup, dialog: false, timers: [], presented: [], invokes: [],
        blockerSelector: '', listener: null, observer: null, visibility: null, deferred: [],
        deferPeek: !!options.deferPeek, failPeek: false, failAck: false,
        failPresentation: false, malformed: null, onPresent: null };
    var document = {
        visibilityState: options.hidden ? 'hidden' : 'visible', body: {},
        addEventListener: function(name, callback) {
            assert.strictEqual(name, 'visibilitychange'); state.visibility = callback;
        },
        querySelector: function(selector) {
            state.blockerSelector = selector;
            return state.dialog ? {} : null;
        }
    };
    var rs = {
        diag: function() {},
        listen: function(name, handler, settings) {
            assert.strictEqual(name, 'native_channel_share_available');
            assert.strictEqual(settings.required, true);
            state.listener = handler;
            return Promise.resolve(function() {});
        },
        invoke: function(command, args) {
            state.invokes.push({ command: command, revision: args && args.revision,
                activityGeneration: args && args.activityGeneration });
            if (command === 'peek_native_channel_share') {
                if (state.failPeek) return Promise.reject(new Error('synthetic IPC failure'));
                var value = native.ready === false ? null : (state.malformed || native.pending);
                if (state.deferPeek) return new Promise(function(resolve) {
                    state.deferred.push(function() { resolve(value); });
                });
                return Promise.resolve(value);
            }
            assert.strictEqual(command, 'ack_native_channel_share');
            if (state.failAck) return Promise.reject(new Error('synthetic IPC failure'));
            var matched = !!native.pending && native.pending.revision === args.revision &&
                native.pending.activityGeneration === args.activityGeneration;
            if (matched) native.pending = null;
            return Promise.resolve(matched);
        }
    };
    vm.runInNewContext(source, {
        window: { __RATSPEAK_DESKTOP__: true, RS: rs,
            channelsOpenNativeSharedChannel: function(value) {
                if (state.failPresentation) return false;
                state.presented.push(value);
                state.dialog = true; // Real preview opens a blocking sheet.
                if (state.onPresent) state.onPresent(value);
                return true;
            } },
        RS: rs, document: document, _isSetupActive: function() { return state.setup; },
        MutationObserver: function(callback) { state.observer = callback; this.observe = function() {}; },
        setTimeout: function(callback) { state.timers.push(callback); return state.timers.length; }
    }, { filename: 'native-channel-share.js' });
    state.document = document;
    state.settle = async function() {
        for (var round = 0; round < 30; round++) {
            await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
            var pending = state.timers.splice(0);
            pending.forEach(function(callback) { callback(); });
        }
        // Promise continuations after the final timer may schedule another
        // retry. Flush them before asserting quiescence, not just before timers.
        for (var flush = 0; flush < 8; flush++) await Promise.resolve();
        assert.strictEqual(state.timers.length, 0, 'native bridge must not spin');
    };
    state.foreground = function() { document.visibilityState = 'visible'; state.visibility(); };
    state.background = function() { document.visibilityState = 'hidden'; state.visibility(); };
    state.dismiss = function() { state.dialog = false; state.observer(); };
    state.count = function(command) { return state.invokes.filter(function(call) { return call.command === command; }).length; };
    return state;
}

async function main() {
    // Setup/modal gates retain the inbox; visible presentation ACKs its exact
    // revision and native owner even though its own sheet is now open.
    var native = inbox(); native.accept(first);
    var current = page(native, { setup: true }); await current.settle();
    assert.strictEqual(current.invokes.length, 0);
    current.setup = false; current.observer(); await current.settle();
    ['.bottom-sheet.open', '.modal-overlay.active', '.game-modal-overlay', '.block-list-overlay',
        '#rs-image-viewer.open', '.action-popover.open', '[class*="-scrim"].active'].forEach(function(selector) {
        assert(current.blockerSelector.includes(selector));
    });
    assert.strictEqual(current.presented[0], first);
    assert.strictEqual(native.pending, null);
    assert.strictEqual(current.count('ack_native_channel_share'), 1);
    assert.strictEqual(current.invokes.find(function(call) { return call.command === 'ack_native_channel_share'; }).activityGeneration, '1');
    native.accept(second); current.listener(); await current.settle();
    assert.strictEqual(current.presented.length, 1, 'open preview must not be replaced behind the user');
    assert.strictEqual(native.pending.target, second);
    current.dismiss(); await current.settle();
    assert.strictEqual(current.presented[1], second);
    assert.strictEqual(native.pending, null);

    // Hidden/retiring pages cannot request/ACK a newly emitted target.
    // Foregrounding resamples even if a native event was missed.
    native = inbox(); native.accept(first);
    current = page(native, { hidden: true }); await current.settle();
    current.listener(); await current.settle();
    assert.strictEqual(current.invokes.length, 0);
    current.foreground(); await current.settle();
    assert.strictEqual(current.presented.length, 1);
    assert.strictEqual(native.pending, null);

    // Native Activity readiness may lag the visible document. A blocked peek
    // keeps Rust's pending value and a native-ready signal wakes this same UI.
    native = inbox(); native.accept(first); native.ready = false;
    current = page(native); await current.settle();
    assert.strictEqual(current.presented.length, 0);
    assert.strictEqual(native.pending.target, first);
    native.ready = true; current.listener(); await current.settle();
    assert.strictEqual(current.presented[0], first);
    assert.strictEqual(native.pending, null);

    // Desktop/iOS use a null native Activity generation, echoed without
    // fabricating an Android lease.
    native = inbox(); native.accept(first); native.pending.activityGeneration = null;
    current = page(native); await current.settle();
    assert.strictEqual(current.presented[0], first);
    assert.strictEqual(native.pending, null);

    // Regression: old page starts IPC before retirement; reply arrives after
    // it is hidden. The replacement still obtains the unconsumed link.
    native = inbox(); native.accept(first);
    var retired = page(native, { deferPeek: true }); await retired.settle();
    assert.strictEqual(retired.count('peek_native_channel_share'), 1);
    retired.background(); retired.deferred.shift()(); await retired.settle();
    assert.strictEqual(retired.presented.length, 0);
    assert.strictEqual(retired.count('ack_native_channel_share'), 0);
    assert.strictEqual(native.pending.target, first);
    current = page(native); await current.settle();
    assert.strictEqual(current.presented[0], first);
    assert.strictEqual(native.pending, null);

    // An IPC response lost altogether is equally harmless.
    native = inbox(); native.accept(first);
    retired = page(native, { deferPeek: true }); await retired.settle();
    current = page(native); await current.settle();
    assert.strictEqual(current.presented[0], first);
    assert.strictEqual(native.pending, null);

    // A newer signal supersedes an older response before presentation.
    native = inbox(); native.accept(first);
    current = page(native, { deferPeek: true }); await current.settle();
    native.accept(second); current.listener();
    current.deferPeek = false; current.deferred.shift()(); await current.settle();
    assert.strictEqual(current.presented[0], second);
    assert.strictEqual(current.presented.length, 1);
    assert.strictEqual(native.pending, null);

    // If the first target was already shown, its stale ACK still cannot
    // remove a newer arrival; the next preview waits until the sheet closes.
    native = inbox(); native.accept(first);
    current = page(native);
    current.onPresent = function(value) {
        if (value === first) { native.accept(second); current.listener(); }
    };
    await current.settle();
    assert.strictEqual(current.presented[0], first);
    assert.strictEqual(native.pending.target, second);
    current.dismiss(); await current.settle();
    assert.strictEqual(current.presented[1], second);
    assert.strictEqual(native.pending, null);

    // Failed presentation never ACKs; retry once UI readiness changes.
    native = inbox(); native.accept(first);
    current = page(native); current.failPresentation = true; await current.settle();
    assert.strictEqual(current.count('ack_native_channel_share'), 0);
    assert.strictEqual(native.pending.target, first);
    current.failPresentation = false; current.observer(); await current.settle();
    assert.strictEqual(current.presented.length, 1);
    assert.strictEqual(native.pending, null);

    // A rejected staged preview cannot monopolize a later valid arrival.
    native = inbox(); native.accept(first);
    current = page(native); current.failPresentation = true; await current.settle();
    assert.strictEqual(current.presented.length, 0);
    native.accept(second); current.failPresentation = false; current.listener(); await current.settle();
    assert.strictEqual(current.presented[0], second);
    assert.strictEqual(native.pending, null);

    // Malformed envelope is never presented/ACKed.
    native = inbox(); native.accept(first);
    current = page(native); current.malformed = { revision: 1, target: first }; await current.settle();
    assert.strictEqual(current.presented.length, 0);
    assert.strictEqual(current.count('ack_native_channel_share'), 0);
    assert.strictEqual(native.pending.target, first);

    // Failed peeks are bounded too; arbitrary DOM mutation is not permission
    // to retry indefinitely. Native availability/foreground explicitly retries.
    native = inbox(); native.accept(first);
    current = page(native); current.failPeek = true; await current.settle();
    assert.strictEqual(current.count('peek_native_channel_share'), 3);
    current.observer(); await current.settle();
    assert.strictEqual(current.count('peek_native_channel_share'), 3);
    current.failPeek = false; current.listener(); await current.settle();
    assert.strictEqual(current.presented[0], first);
    assert.strictEqual(native.pending, null);

    // ACK retry is bounded and never re-presents. Foreground recovery can
    // finish the ACK; destructive reads are never a fallback.
    native = inbox(); native.accept(first);
    current = page(native); current.failAck = true; await current.settle();
    assert.strictEqual(current.count('ack_native_channel_share'), 3);
    assert.strictEqual(current.presented.length, 1);
    assert.strictEqual(native.pending.target, first);
    current.failAck = false; current.background(); current.foreground(); await current.settle();
    assert.strictEqual(current.presented.length, 1);
    assert.strictEqual(native.pending, null);

    console.log('native channel link tests passed (setup, modal, visibility, retired/lost reply, stale ACK, presentation, bounded retry)');
}
main().catch(function(error) { console.error(error); process.exitCode = 1; });
