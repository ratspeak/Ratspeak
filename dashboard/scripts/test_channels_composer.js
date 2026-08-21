#!/usr/bin/env node
// Deterministic tests for Channels composer typing and payload preservation.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var channelsPath = path.join(__dirname, '..', 'static', 'js', 'channels.js');
var channelsSource = fs.readFileSync(channelsPath, 'utf8');
var lxmfPath = path.join(__dirname, '..', 'static', 'js', 'lxmf.js');
var lxmfSource = fs.readFileSync(lxmfPath, 'utf8');
var uiSharedPath = path.join(__dirname, '..', 'static', 'js', 'ui_shared.js');
var uiSharedSource = fs.readFileSync(uiSharedPath, 'utf8');

function sourceFunctionFrom(source, name, nextName) {
    var start = source.indexOf('function ' + name);
    var end = source.indexOf('\nfunction ' + nextName, start);
    assert(start !== -1 && end !== -1, name + ' must exist');
    return source.slice(start, end);
}

function sourceFunction(name, nextName) {
    return sourceFunctionFrom(channelsSource, name, nextName);
}

function namedFunctionSource(source, name) {
    var start = source.indexOf('function ' + name + '(');
    assert.notStrictEqual(start, -1, name + ' must exist');
    var brace = source.indexOf('{', start);
    var depth = 0;
    for (var index = brace; index < source.length; index++) {
        if (source[index] === '{') depth += 1;
        if (source[index] === '}') {
            depth -= 1;
            if (depth === 0) return source.slice(start, index + 1);
        }
    }
    throw new Error('unterminated function ' + name);
}

var policyStart = uiSharedSource.indexOf('RS.composer.usesNativeTypingDefaults =');
var policyEnd = uiSharedSource.indexOf('RS.text.utf8Length =', policyStart);
assert(policyStart !== -1 && policyEnd !== -1, 'shared composer typing policy must exist');
var policySource = uiSharedSource.slice(policyStart, policyEnd);
var insertionSource = sourceFunction('_channelsCanCompose', '_channelsDurableRoom');
var sendSource = sourceFunction('channelsSendMessage', '_channelsBindUI');
var renderRoomSource = sourceFunction('_channelsRenderRoom', '_channelsTimelineEntries');
var renderConversationSource = namedFunctionSource(lxmfSource, 'renderConversation');

assert(!channelsSource.includes('_channelsTranscriptPinToken'),
    'Channels must not maintain a parallel transcript-follow state machine');
assert(renderRoomSource.includes('RS.chatScroll.capture(transcript)'));
assert(renderRoomSource.includes('RS.chatScroll.applyAfterRender(transcript, scrollState'));
assert(sendSource.includes("RS.chatScroll.pinToBottom(_channelsEl('channel-transcript'))"),
    'sending with the keyboard open must reuse the Direct Messages scroll controller');
assert(!renderConversationSource.includes('preventDefaultOnStart'),
    'message long-press recognition must not cancel Android transcript panning');

var scrollFunctions = [
    '_lxmfMessageScrollStateFor',
    '_lxmfMessageBottomGap',
    '_lxmfMessagesNearBottom',
    '_wireLxmfMessageScroll',
    '_setLxmfMessageScrollTop',
    '_scheduleLxmfScrollToBottom',
    '_captureLxmfMessageScrollState',
    '_restoreLxmfMessageScroll',
    '_lxmfShouldFollowLatest',
    '_applyLxmfMessageScrollAfterRender'
].map(function(name) { return namedFunctionSource(lxmfSource, name); }).join('\n');
var now = 1000;
var scrollHandlers = {};
var pendingSettleCallbacks = [];
var scrollPolicyContext = {
    Date: { now: function() { return now; } },
    WeakMap: WeakMap,
    requestAnimationFrame: function(callback) {
        pendingSettleCallbacks.push(callback);
        return pendingSettleCallbacks.length;
    },
    setTimeout: function(callback) {
        pendingSettleCallbacks.push(callback);
        return pendingSettleCallbacks.length;
    }
};
vm.createContext(scrollPolicyContext);
vm.runInContext(
    'var _lxmfMessageScrollStates = new WeakMap();\n' + scrollFunctions +
    '\nthis.wire = _wireLxmfMessageScroll;' +
    '\nthis.capture = _captureLxmfMessageScrollState;' +
    '\nthis.applyAfterRender = _applyLxmfMessageScrollAfterRender;',
    scrollPolicyContext,
    { filename: 'shared-chat-scroll-policy.js' }
);
var scrollTop = 600;
var transcript = {
    scrollHeight: 1000,
    clientHeight: 400,
    isConnected: true,
    addEventListener: function(name, handler) { scrollHandlers[name] = handler; }
};
Object.defineProperty(transcript, 'scrollTop', {
    get: function() { return scrollTop; },
    set: function(value) {
        scrollTop = Math.max(0, Math.min(Number(value) || 0,
            transcript.scrollHeight - transcript.clientHeight));
    }
});
scrollPolicyContext.wire(transcript);
var followState = scrollPolicyContext.capture(transcript);
transcript.scrollHeight = 1200;
assert.strictEqual(scrollPolicyContext.applyAfterRender(
    transcript, followState, { stickToBottom: true }
), true);
assert.strictEqual(transcript.scrollTop, 800,
    'new traffic follows the recent edge while the reader is already there');

now = 1200;
scrollHandlers.touchstart();
transcript.scrollTop = 300;
scrollHandlers.scroll();
pendingSettleCallbacks.forEach(function(callback) { callback(); });
pendingSettleCallbacks = [];
assert.strictEqual(transcript.scrollTop, 300,
    'touching history must cancel delayed send-settle pins before Android pans');
var readingState = scrollPolicyContext.capture(transcript);
transcript.scrollHeight = 1300;
assert.strictEqual(scrollPolicyContext.applyAfterRender(
    transcript, readingState, { stickToBottom: true }
), false);
assert.strictEqual(transcript.scrollTop, 300,
    'fast incoming traffic must preserve a deliberate history position');

now = 1400;
transcript.scrollTop = 900;
scrollHandlers.scroll();
var resumedState = scrollPolicyContext.capture(transcript);
transcript.scrollHeight = 1400;
assert.strictEqual(scrollPolicyContext.applyAfterRender(
    transcript, resumedState, { stickToBottom: true }
), true);
assert.strictEqual(transcript.scrollTop, 1000,
    'returning to the recent edge must automatically resume live following');

function fakeInput(attributes) {
    var values = Object.assign({}, attributes || {});
    return {
        value: '',
        style: {},
        scrollHeight: 40,
        selectionStart: 0,
        selectionEnd: 0,
        setAttribute: function(name, value) { values[name] = String(value); },
        removeAttribute: function(name) { delete values[name]; },
        hasAttribute: function(name) { return Object.prototype.hasOwnProperty.call(values, name); },
        getAttribute: function(name) { return values[name]; },
        setSelectionRange: function(start, end) {
            this.selectionStart = start;
            this.selectionEnd = end;
        },
        focus: function() {}
    };
}

var mobileTypingPlatform = false;
var policyContext = {
    window: {},
    isTauriMobile: function() { return mobileTypingPlatform; },
    isIOS: function() { return false; },
    isAndroid: function() { return false; }
};
policyContext.window.RS = { composer: {} };
policyContext.RS = policyContext.window.RS;
vm.runInNewContext(
    policySource +
        '\nthis.applyPolicy = RS.composer.applyTypingPolicy;' +
        '\nthis.handleBeforeInput = RS.composer.handleBeforeInput;',
    policyContext,
    { filename: 'channels-composer-policy.js' }
);

var desktopInput = fakeInput();
policyContext.applyPolicy(desktopInput);
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
mobileTypingPlatform = true;
policyContext.applyPolicy(mobileInput);
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

assert(channelsSource.includes('RS.composer.bindTypingPolicy(input);'),
    'Channels must bind the shared typing policy');
assert(lxmfSource.includes('RS.composer.bindTypingPolicy(textarea);'),
    'Direct Messages must bind the shared typing policy');
assert(!channelsSource.includes('function _channelsApplyComposerTypingPolicy'),
    'Channels must not duplicate the shared typing policy');

var insertionInput = fakeInput();
var insertionContext = {
    channelsActiveRoom: 'general',
    channelsHistorySelection: null,
    channelsSnapshot: { phase: 'active' },
    _channelsEl: function() { return insertionInput; },
    _channelsRoomByName: function() { return { name: 'general', phase: 'joined' }; },
    _channelsMessageLimit: function() { return 350; },
    _channelsMessageBody: function(value) { return value; },
    _channelsUtf8Length: function(value) { return Buffer.byteLength(value, 'utf8'); },
    _channelsUtf8Truncate: function(value, limit) {
        var result = '';
        Array.from(value).some(function(character) {
            if (Buffer.byteLength(result + character, 'utf8') > limit) return true;
            result += character;
            return false;
        });
        return result;
    },
    _channelsUpdateComposer: function() {},
    showToast: function() {},
    Buffer: Buffer
};
vm.runInNewContext(insertionSource, insertionContext, {
    filename: 'channels-composer-insertion.js'
});

insertionInput.value = 'hello';
insertionInput.selectionStart = insertionInput.selectionEnd = 5;
assert.strictEqual(
    insertionContext._channelsInsertMemberMention({ nickname: 'Field Rat', is_self: false }),
    true
);
assert.strictEqual(insertionInput.value, 'hello @Field Rat ',
    'member mentions must be literal composer text with a safe boundary');

insertionInput.value = 'Draft';
insertionInput.selectionStart = insertionInput.selectionEnd = 5;
assert.strictEqual(insertionContext._channelsInsertQuote({
    kind: 'message',
    text: 'line one\nline two'
}, 'Scout'), true);
assert.strictEqual(
    insertionInput.value,
    'Draft\n> Scout: line one line two\n\n',
    'quotes must remain plain, interoperable text rather than a wire extension'
);

insertionContext.channelsHistorySelection = { room_name: 'general' };
assert.strictEqual(
    insertionContext._channelsInsertMemberMention({ nickname: 'Scout', is_self: false }),
    false,
    'read-only history must never unlock composer insertion'
);

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
    document: {
        activeElement: composerInput,
        documentElement: { classList: { contains: function() { return false; } } }
    },
    RS: {
        composer: {
            consumeFocus: function() { return true; },
            focusWithoutScroll: function() {}
        },
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
    process.stdout.write('\u2713 quote and mention insertion stays wire-compatible\n');
});
