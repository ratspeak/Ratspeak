#!/usr/bin/env node
// Deterministic lifecycle coverage for the native channel-share inbox bridge.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var source = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'native_channel_share.js'),
    'utf8'
);

async function main() {
    var setupActive = true;
    var dialogOpen = false;
    var blockerSelector = '';
    var observerCallback = null;
    var timers = [];
    var listener = null;
    var invokes = [];
    var presented = [];
    var targets = [
        {
            format: 'ratspeak.channel.v1',
            payload: 'ratspeak://channel?v=1&hub=00112233445566778899aabbccddeeff',
            hub_destination_hash: '00112233445566778899aabbccddeeff',
            room: null
        },
        {
            format: 'ratspeak.channel.v1',
            payload: 'ratspeak://channel?v=1&hub=ffeeddccbbaa99887766554433221100&room=field',
            hub_destination_hash: 'ffeeddccbbaa99887766554433221100',
            room: 'field'
        }
    ];

    var rs = {
        diag: function() {},
        invoke: function(command) {
            invokes.push(command);
            return Promise.resolve(targets.shift() || null);
        },
        listen: function(eventName, handler, options) {
            assert.strictEqual(eventName, 'native_channel_share_available');
            assert.strictEqual(options.required, true);
            assert.deepStrictEqual(Object.keys(options), ['required']);
            listener = handler;
            return Promise.resolve(function() {});
        }
    };
    var context = {
        window: {
            __RATSPEAK_DESKTOP__: true,
            __RATSPEAK_MOBILE__: false,
            RS: rs,
            channelsOpenNativeSharedChannel: function(target) {
                presented.push(target);
                return true;
            }
        },
        RS: rs,
        document: {
            body: {},
            querySelector: function(selector) {
                blockerSelector = selector;
                return dialogOpen ? {} : null;
            }
        },
        _isSetupActive: function() {
            return setupActive;
        },
        MutationObserver: function(callback) {
            observerCallback = callback;
            this.observe = function() {};
        },
        setTimeout: function(callback, delay) {
            timers.push({ callback: callback, delay: delay || 0 });
            return timers.length;
        }
    };

    function microtasks() {
        return Promise.resolve().then(function() {
            return Promise.resolve();
        }).then(function() {
            return Promise.resolve();
        });
    }

    async function settle() {
        await microtasks();
        for (var round = 0; round < 20 && timers.length; round++) {
            var pending = timers;
            timers = [];
            pending.forEach(function(timer) {
                timer.callback();
            });
            await microtasks();
        }
        assert(timers.length === 0, 'native bridge timer loop did not settle');
    }

    vm.runInNewContext(source, context, {
        filename: 'native-channel-share.js'
    });
    await settle();
    assert.strictEqual(invokes.length, 0,
        'cold-start target must remain in Rust throughout setup');
    assert.strictEqual(presented.length, 0);
    assert.strictEqual(typeof listener, 'function');

    setupActive = false;
    observerCallback();
    await settle();
    assert(blockerSelector.indexOf('.bottom-sheet.open') !== -1);
    assert(blockerSelector.indexOf('.modal-overlay.active') !== -1);
    assert(blockerSelector.indexOf('.game-modal-overlay') !== -1);
    assert(blockerSelector.indexOf('.block-list-overlay') !== -1);
    assert(blockerSelector.indexOf('#rs-image-viewer.open') !== -1);
    assert(blockerSelector.indexOf('.action-popover.open') !== -1);
    assert(blockerSelector.indexOf('[class*="-scrim"].active') !== -1);
    assert.deepStrictEqual(invokes, ['take_native_channel_share']);
    assert.strictEqual(presented.length, 1);
    assert.strictEqual(
        presented[0].hub_destination_hash,
        '00112233445566778899aabbccddeeff'
    );

    dialogOpen = true;
    listener();
    await settle();
    assert.strictEqual(invokes.length, 1,
        'a running-app target must stay in Rust while another dialog is open');
    assert.strictEqual(presented.length, 1);

    dialogOpen = false;
    observerCallback();
    await settle();
    assert.deepStrictEqual(invokes, [
        'take_native_channel_share',
        'take_native_channel_share'
    ]);
    assert.strictEqual(presented.length, 2);
    assert.strictEqual(
        presented[1].hub_destination_hash,
        'ffeeddccbbaa99887766554433221100'
    );

    console.log('native channel link tests passed');
}

main().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
