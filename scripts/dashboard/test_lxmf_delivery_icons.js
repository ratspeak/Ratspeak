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
    assert(html.includes('<polyline points="11 4 4.5 10.5 1.5 7.5"/><polyline points="15 6.5 9.5 12 8.5 11"/>'),
        method + ' proof must show two checks');
}

['opportunistic', 'direct'].forEach(assertSent);
['opportunistic', 'direct'].forEach(assertDelivered);
assertDelivered(undefined);

var propagated = icon('propagated', 'propagated');
assert(propagated.includes('msg-state-propagated'));
assert(propagated.includes('aria-label="Stored in Offline Inbox"'));

var failed = icon('failed', 'opportunistic');
assert(failed.includes('msg-state-failed'));
assert(failed.includes('aria-label="Not delivered"') && failed.includes('title="Not delivered"'),
    'the bubble shows only the icon; its name is the hover title');
assert(failed.includes('<line x1="8" y1="2.5" x2="8" y2="9"/>') && !failed.includes('x1="4" y1="4"'),
    'not delivered is a "!", not the cancel cross');
assert(icon('cancelled', 'direct').includes('x1="4" y1="4"'), 'cancelled keeps the cross');
assert(icon('timeout', 'direct').includes('aria-label="Not delivered"'), 'a timeout is Not delivered');
assert(!icon('read', 'direct').includes('msg-state-read'), 'no outbound message shows a Read state');

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
assert(/\.msg-state-rejected svg\s*\{[^}]*stroke:/s.test(css), 'the rejected icon has a stroke, or it draws nothing');
assert(/\.msg-state-sent svg,\s*\.msg-state-delivered svg\s*\{[^}]*var\(--text-muted\)/s.test(css),
    'Sent and Delivered share one colour; the glyph tells them apart');

// Retry: in the message menu only, for messages with an outbox entry.
['_isCanonicalLxmfMsgId', '_messageCanRetry'].forEach(function(name) {
    vm.runInContext(functionSource(name), context);
});
var id = 'a'.repeat(64);
['failed', 'timeout', 'cancelled'].forEach(function(state) {
    assert(context._messageCanRetry({ id: id, direction: 'outbound', state: state }), state + ' can retry');
});
['sending', 'sent', 'delivered', 'propagated', 'rejected'].forEach(function(state) {
    assert(!context._messageCanRetry({ id: id, direction: 'outbound', state: state }), state + ' has no retry');
});
assert(!context._messageCanRetry({ id: id, direction: 'outbound', state: 'failed', retryable: false }),
    'a row from before the outbox has nothing to send again');
assert(!context._messageCanRetry({ id: 'out_1', direction: 'outbound', state: 'cancelled' }),
    'a send cancelled before it was queued has no stored message');
assert(!context._messageCanRetry({ id: id, direction: 'inbound', state: 'failed' }));
assert(source.includes("retryBtn.innerHTML = _messageActionIcon('retry') + '<span>Retry</span>';"));
assert(source.includes("{ label: 'Auto', icon: LXMF_SEND_METHOD_ICONS.auto,"),
    'Retry offers the send methods, Auto first');
assert(source.includes("container.querySelector('.lxmf-msg[data-msg-id=\"' + msg.id + '\"]')"),
    'the method list opens at the bubble');

// A proof outranks an earlier outcome; Not delivered elsewhere is one toast.
assert(source.includes("var proofOutranks = resolvedState === 'delivered' &&"));
assert(source.includes("showToast('Message to ' + name + ' not delivered', 'toast-action', 4000,"));
assert(!source.includes('Message timed out. The recipient may be unreachable.'));

// Show in Activity explains undelivered sends, never the user's own cancel.
vm.runInContext(functionSource('_messageCanShowInActivity'), context);
context._isCanonicalLxmfMsgId = function(id) { return /^[0-9a-f]{64}$/.test(id); };
context._messageIsPaper = function() { return false; };
function canExplain(state, direction) {
    return context._messageCanShowInActivity({ id: 'a'.repeat(64), state: state, direction: direction || 'outbound' });
}
['failed', 'timeout', 'rejected'].forEach(function(state) { assert(canExplain(state), state); });
['cancelled', 'sent', 'delivered', 'propagated', 'sending'].forEach(function(state) { assert(!canExplain(state), state); });
assert(!canExplain('failed', 'inbound'));
var retryAt = source.indexOf("retryBtn.innerHTML = _messageActionIcon('retry')");
var activityAt = source.indexOf("activityBtn.innerHTML = _messageActionIcon('activity') + '<span>Show in Activity</span>';");
var replyAt = source.indexOf("replyBtn.innerHTML = _messageActionIcon('reply')");
assert(retryAt !== -1 && activityAt > retryAt && replyAt > activityAt, 'Show in Activity sits right after Retry');

console.log('LXMF delivery icon tests passed');
