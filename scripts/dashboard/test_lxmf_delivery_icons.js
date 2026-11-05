#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..', '..', 'dashboard');
var source = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'lxmf.js'), 'utf8');

function functionSource(name) {
    var start = source.indexOf('function ' + name + '(');
    assert.notStrictEqual(start, -1, name + ' must exist');
    var brace = source.indexOf('{', start);
    var depth = 0;
    for (var index = brace; index < source.length; index++) {
        if (source[index] === '{') depth++;
        if (source[index] === '}') {
            depth--;
            if (depth === 0) return source.slice(start, index + 1);
        }
    }
    throw new Error('unterminated function ' + name);
}

var context = {
    _messageProgressPercent: function() { return null; }
};
vm.createContext(context);
vm.runInContext(functionSource('_messageStateIconHtml'), context);

function icon(state, method) {
    return context._messageStateIconHtml({
        state: state,
        delivery_method: method,
        direction: 'outbound'
    });
}

function assertSent(method) {
    var html = icon('sent', method);
    assert(html.includes('msg-state-sent'), method + ' sent must use the muted sent treatment');
    assert(html.includes('aria-label="Sent"'), method + ' sent must remain accessible as Sent');
    assert(!html.includes('msg-state-delivered'), method + ' sent must not claim delivery');
}

function assertDelivered(method) {
    var html = icon('delivered', method);
    assert(html.includes('msg-state-delivered'), method + ' proof must use the confirmed treatment');
    assert(html.includes('aria-label="Delivered"'), method + ' proof must be accessible as Delivered');
    assert(!html.includes('msg-state-sent'), method + ' proof must not be collapsed back to Sent');
}

['opportunistic', 'direct'].forEach(assertSent);
['opportunistic', 'direct'].forEach(assertDelivered);
assertDelivered(undefined);

var propagated = icon('propagated', 'propagated');
assert(propagated.includes('msg-state-propagated'));
assert(propagated.includes('aria-label="Stored in Offline Inbox"'));

var failed = icon('failed', 'opportunistic');
assert(failed.includes('msg-state-failed'));
assert(failed.includes('aria-label="Failed"'));

var stopped = icon('cancelled', 'direct');
assert(stopped.includes('aria-label="Sending cancelled"'));
assert(source.includes('aria-label="Cancel sending message">Cancel</button>'));
assert(source.includes("message: 'Cancel preparation and retries for this message?'"));
assert(source.includes('Cancelled local retries. A copy already handed to the network may still arrive.'));
assert(source.includes('No live send remained. The local message was cancelled, but a copy may still arrive.'));
assert(source.includes('Cancelled before the message left this device.'));
assert(source.includes("title: 'Cancel sending?'"));

// Paper: a neutral chip in the state slot, terminal everywhere, never a clock or check.
var paperIcon = icon('paper', 'paper');
assert(paperIcon.includes('msg-state-paper') && paperIcon.includes('aria-label="Paper message"'));
assert(!paperIcon.includes('msg-state-sending') && !paperIcon.includes('msg-state-sent'));
context.escapeHtml = function(value) { return String(value); };
vm.runInContext(source.slice(source.indexOf('var RS_QR_GLYPH_PATHS'), source.indexOf('var RS_PAPER_LINK_GLYPH')), context);
['_isTerminalMessageState', '_messageDeliveryMethod', '_messageIsPaper', '_messagePaperChipHtml', '_messageCanCancelSend']
    .forEach(function(name) { vm.runInContext(functionSource(name), context); });
assert(context._isTerminalMessageState('paper'));
assert.strictEqual(context._messageCanCancelSend({ direction: 'outbound', state: 'paper' }), false,
    'paper messages have no cancel, retry, or progress');
assert(context._messageIsPaper({ state: 'delivered', delivery_method: 'paper' }), 'inbound filed paper rows are paper');
var outboundChip = context._messagePaperChipHtml({ id: 'm1' }, true);
assert(outboundChip.includes('<button type="button" class="msg-paper-state" data-msg-id="m1"'));
assert(outboundChip.includes('aria-label="Paper message, not sent over the network. Show QR"'));
assert(outboundChip.includes('<span class="msg-state-label" aria-hidden="true">Paper</span>'));
var inboundChip = context._messagePaperChipHtml({ id: 'm2' }, false);
assert(!inboundChip.includes('<button') && inboundChip.includes('aria-label="Received as a paper message"'));
assert(outboundChip.includes('<path d="M14 21h7v-2"/>'), 'the chip and the chat link share one QR glyph');
assert.strictEqual((source.match(/var terminalStates = \['delivered', 'propagated', 'failed', 'cancelled', 'rejected', 'timeout', 'paper'\];/g) || []).length, 2,
    'step and progress events must not overwrite a paper state');
assert(source.includes("var stateIcon = _messageIsPaper(msg)\n            ? _messagePaperChipHtml(msg, isOut)"));
assert(source.includes("'.md-spoiler:not(.is-revealed), .md-code-copy, .msg-paper-state'"),
    'a hold on the chip opens message actions and swallows the trailing tap');
assert(source.includes("setAttribute('data-message-action', 'paper-qr')") &&
    source.includes("'<span>Show QR</span>'") && source.includes("'<span>Copy paper link</span>'"));
assert(source.includes("showToast(ok ? 'Paper link copied' : 'Could not copy'"));

var css = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '09-messaging.css'), 'utf8');
assert(/button\.msg-paper-state::after\s*\{[^}]*width:\s*44px;[^}]*height:\s*44px;/s.test(css),
    'the paper chip keeps a 44px hit target');
assert(/\.msg-state-sent svg\s*\{[^}]*var\(--text-muted\)/s.test(css),
    'sent checks must use the muted foreground');
assert(/\.msg-state-delivered svg\s*\{[^}]*var\(--accent\)/s.test(css),
    'proof-confirmed checks must use the accent foreground');

console.log('LXMF delivery icon tests passed');
