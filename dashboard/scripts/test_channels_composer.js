#!/usr/bin/env node
// Deterministic tests for Channels composer typing and payload preservation.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var channelsPath = path.join(__dirname, '..', 'static', 'js', 'channels.js');
var channelsSource = fs.readFileSync(channelsPath, 'utf8');

function sourceFunction(name, nextName) {
    var start = channelsSource.indexOf('function ' + name);
    var end = channelsSource.indexOf('\nfunction ' + nextName, start);
    assert(start !== -1 && end !== -1, name + ' must exist');
    return channelsSource.slice(start, end);
}

var policySource = sourceFunction(
    '_channelsApplyComposerTypingPolicy',
    'channelsSelectRoom'
);
var sendSource = sourceFunction('channelsSendMessage', '_channelsBindUI');

function fakeInput(attributes) {
    var values = Object.assign({}, attributes || {});
    return {
        value: '',
        style: {},
        setAttribute: function(name, value) { values[name] = String(value); },
        removeAttribute: function(name) { delete values[name]; },
        hasAttribute: function(name) { return Object.prototype.hasOwnProperty.call(values, name); },
        getAttribute: function(name) { return values[name]; },
        focus: function() {}
    };
}

var policyContext = {};
vm.runInNewContext(
    policySource +
        '\nthis.applyPolicy = _channelsApplyComposerTypingPolicy;' +
        '\nthis.handleBeforeInput = _channelsHandleComposerBeforeInput;',
    policyContext,
    { filename: 'channels-composer-policy.js' }
);

var desktopInput = fakeInput();
policyContext.applyPolicy(desktopInput, false);
assert.strictEqual(desktopInput.getAttribute('autocomplete'), 'off');
assert.strictEqual(desktopInput.getAttribute('autocorrect'), 'off');
assert.strictEqual(desktopInput.getAttribute('autocapitalize'), 'off');
assert.strictEqual(desktopInput.getAttribute('spellcheck'), 'false');
assert.strictEqual(desktopInput.getAttribute('writingsuggestions'), 'false');

var mobileInput = fakeInput({
    autocomplete: 'off',
    autocorrect: 'off',
    autocapitalize: 'off',
    spellcheck: 'false',
    writingsuggestions: 'false'
});
policyContext.applyPolicy(mobileInput, true);
['autocomplete', 'autocorrect', 'autocapitalize', 'spellcheck', 'writingsuggestions']
    .forEach(function(attribute) {
        assert.strictEqual(mobileInput.hasAttribute(attribute), false);
    });

var replacementPrevented = false;
policyContext.handleBeforeInput({
    inputType: 'insertReplacementText',
    preventDefault: function() { replacementPrevented = true; }
}, false);
assert.strictEqual(replacementPrevented, true);

replacementPrevented = false;
policyContext.handleBeforeInput({
    inputType: 'insertReplacementText',
    preventDefault: function() { replacementPrevented = true; }
}, true);
assert.strictEqual(replacementPrevented, false);

var sentPayload = null;
var composerInput = fakeInput();
composerInput.value = 'beep FoO';
var sendContext = {
    channelsActiveRoom: 'general',
    _channelsSendPending: false,
    _channelsEl: function() { return composerInput; },
    _channelsRoomByName: function() { return { name: 'general', phase: 'joined' }; },
    _channelsUtf8Length: function(value) { return Buffer.byteLength(value, 'utf8'); },
    _channelsMessageBody: function(value) { return value; },
    _channelsMessageLimit: function() { return 4096; },
    _channelsUpdateComposer: function() {},
    _channelsHandleComposerResult: function() { return Promise.resolve(); },
    RS: {
        invoke: function(_name, payload) {
            sentPayload = payload;
            return Promise.resolve({ accepted: true });
        }
    },
    Buffer: Buffer,
    Promise: Promise
};
vm.runInNewContext(sendSource, sendContext, { filename: 'channels-composer-send.js' });
sendContext.channelsSendMessage();

setImmediate(function() {
    assert(sentPayload, 'composer must invoke the channel send command');
    assert.strictEqual(sentPayload.args.text, 'beep FoO');
    process.stdout.write('\u2713 desktop text assistance disabled\n');
    process.stdout.write('\u2713 mobile typing defaults preserved\n');
    process.stdout.write('\u2713 automatic desktop replacements rejected\n');
    process.stdout.write('\u2713 mixed-case channel payload preserved\n');
});
