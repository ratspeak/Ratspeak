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
vm.runInContext(source.slice(0, source.indexOf('var lxmfIdentity')), context);
['_messageDeliveryMethod', '_messageIsPaper', '_messagePaperChipHtml', '_messageCanCancelSend']
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

// One table of outbound states, and every reader agrees with it for every state.
var ON_ITS_WAY = ['sending', 'routing', 'propagating', 'link_establishing', 'sending_via_link',
    'reusing_direct_link', 'reusing_backchannel', 'sent'];
var SETTLED = ['delivered', 'propagated', 'failed', 'cancelled', 'rejected', 'timeout', 'paper'];
var PROGRESS = ['resource_link_ready', 'resource_advertised', 'resource_transferring', 'resource_waiting_for_proof'];
var RETIRED = ['pending', 'outbound', 'generating', 'resolving', 'resending'];
assert.deepStrictEqual(Object.keys(context.LXMF_OUTBOUND_STATES), ON_ITS_WAY.concat(SETTLED, ['waiting'], PROGRESS));
['terminalStates', 'inFlightSteps', 'keepStates'].forEach(function(list) {
    assert(!source.includes('var ' + list + ' = ['), list + ' is read from the table');
});
ON_ITS_WAY.concat(SETTLED, PROGRESS, RETIRED, ['received', 'read', 'unknown', '', undefined]).forEach(function(state) {
    var settled = SETTLED.indexOf(state) !== -1;
    var unsettled = ON_ITS_WAY.indexOf(state) !== -1 || PROGRESS.indexOf(state) !== -1;
    assert.strictEqual(context._isTerminalMessageState(state), settled, state + ' settled');
    assert.strictEqual(context._isInFlightStep(state), unsettled && state !== 'sending', state + ' step');
    assert.strictEqual(context._messageCanCancelSend({ direction: 'outbound', state: state, delivery_method: 'direct' }),
        unsettled, state + ' cancel');
    assert.strictEqual(context._messageCanCancelSend({ direction: 'inbound', state: state }), false);
});
assert.strictEqual(context._messageCanCancelSend({ direction: 'outbound', state: 'sent' }), false,
    'a sent packet has left the device');

// Steps and transfer progress move a message on its way and never one that settled.
function listenerSource(event) {
    var start = source.indexOf("RS.listen('" + event + "', function(");
    assert.notStrictEqual(start, -1, event + ' listener must exist');
    var depth = 0;
    for (var index = source.indexOf('{', start); index < source.length; index++) {
        if (source[index] === '{') depth++;
        if (source[index] === '}' && --depth === 0) return source.slice(start, source.indexOf(';', index) + 1);
    }
    throw new Error('unterminated listener ' + event);
}
var handlers = {};
var events = {
    RS: { listen: function(event, handler) { handlers[event] = handler; } },
    lxmfConversation: [],
    lxmfConversations: [],
    _renderConversationsFromCache: function() {},
    _pendingLxmfCancelByClientId: {},
    _flushPendingLxmfCancel: function() {},
    _cacheActiveConversation: function() {},
    renderConversation: function() {},
    _noticeUndeliveredMessage: function() {},
    showToast: function() {}
};
vm.createContext(events);
vm.runInContext(source.slice(0, source.indexOf('var lxmfIdentity')), events);
vm.runInContext(functionSource('_messageStateAfterStep') + functionSource('_applyStepToConversationList') +
    listenerSource('lxmf_step') + listenerSource('lxmf_delivery_progress'), events);
function after(event, from, step) {
    events.lxmfConversation = [{ id: 'm', direction: 'outbound', state: from }];
    handlers[event]({ msg_id: 'm', step: step });
    return events.lxmfConversation[0].state;
}
var MOVES = ON_ITS_WAY.concat(PROGRESS).filter(function(step) { return step !== 'sending'; });
ON_ITS_WAY.concat(SETTLED, PROGRESS, RETIRED).forEach(function(step) {
    var moves = MOVES.indexOf(step) !== -1;
    assert.strictEqual(after('lxmf_step', 'sending', step),
        moves || ['delivered', 'propagated', 'failed', 'cancelled', 'rejected', 'timeout'].indexOf(step) !== -1 ? step : 'sending',
        step + ' step');
    assert.strictEqual(after('lxmf_delivery_progress', 'sending', step), moves ? step : 'sending', step + ' progress');
    SETTLED.forEach(function(settled) {
        var proof = step === 'delivered' && settled !== 'rejected' && settled !== 'paper';
        assert.strictEqual(after('lxmf_step', settled, step), proof ? 'delivered' : settled, settled + ' after ' + step);
        assert.strictEqual(after('lxmf_delivery_progress', settled, step), settled, settled + ' after progress ' + step);
    });
});
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

// ---------------------------------------------------------------------------
// Waiting: a message Ratspeak keeps trying to send.
// ---------------------------------------------------------------------------
var WAITING_LABEL = 'Waiting to send. Ratspeak keeps trying for up to 24 hours.';
var HOURGLASS = '<path d="M3.5 2.5h9M3.5 13.5h9M5 2.5V5l3 3-3 3v2.5M11 2.5V5L8 8l3 3v2.5"/>';

// The mark: an hourglass, named on hover and to a screen reader, never the clock, no words.
var waitingIcon = icon('waiting', 'opportunistic');
assert(waitingIcon.includes('class="msg-state-icon msg-state-waiting"'));
assert(waitingIcon.includes('aria-label="' + WAITING_LABEL + '"') && waitingIcon.includes('title="' + WAITING_LABEL + '"'));
assert(waitingIcon.includes(HOURGLASS), 'waiting is the hourglass');
assert(!waitingIcon.includes('msg-state-sending') && !waitingIcon.includes('<circle'), 'waiting is not the sending clock');
assert(!waitingIcon.includes('msg-state-label'), 'the bubble carries no words for waiting');
assert(/\.msg-state-waiting svg\s*\{\s*stroke:\s*var\(--text-muted\);\s*\}/.test(css),
    'the hourglass is muted and still: one declaration, no animation');

// Not on its way and not settled: the time shows, nothing offers Cancel, and it cannot be selected.
context._messageIsPaper = function(msg) { return !!msg && msg.state === 'paper'; };
context._isCanonicalLxmfMsgId = function(value) { return /^[a-f0-9]{64}$/i.test(String(value || '')); };
['_messageInlineCancelHtml', '_messageHasTransferPayload', '_messageCanCancelTransfer', '_messageSelectable',
    '_messageIsWaiting'].forEach(function(name) { vm.runInContext(functionSource(name), context); });
var waitingMsg = { id: id, direction: 'outbound', state: 'waiting', delivery_method: 'direct', image: { stored_name: 'i' } };
assert.strictEqual(context._isTerminalMessageState('waiting'), false, 'a late proof or outcome still replaces waiting');
assert.strictEqual(context._isInFlightStep('waiting'), false);
assert.strictEqual(context._messageCanCancelSend(waitingMsg), false);
assert.strictEqual(context._messageInlineCancelHtml(waitingMsg), '', 'no inline Cancel on a waiting message');
assert.strictEqual(context._messageCanCancelTransfer(waitingMsg), false, 'no cancel overlay on a waiting image');
assert(source.includes("(canCancelSend ? _messageInlineCancelHtml(msg) : '<span class=\"msg-time\">' + time + '</span>')"),
    'a message that cannot be cancelled shows its time');
assert.strictEqual(context._messageSelectable(waitingMsg), false, 'a waiting message is stopped, not deleted');
assert.strictEqual(context._messageSelectable({ id: id, direction: 'outbound', state: 'failed' }), true);
assert(!context._messageCanRetry(waitingMsg), 'waiting has no retry');
assert(canExplain('waiting'), 'Activity explains why a message waits');
assert(context._messageIsWaiting(waitingMsg));
['sending', 'sent', 'failed', 'cancelled', 'delivered'].forEach(function(state) {
    assert(!context._messageIsWaiting({ id: id, direction: 'outbound', state: state }), state);
});
assert(!context._messageIsWaiting({ id: id, direction: 'inbound', state: 'waiting' }));
assert(!context._messageIsWaiting({ id: 'out_1', direction: 'outbound', state: 'waiting' }));

// A `waiting` step parks a message on its way and never one that settled.
ON_ITS_WAY.concat(PROGRESS).forEach(function(from) {
    assert.strictEqual(after('lxmf_step', from, 'waiting'), 'waiting', from + ' parks');
});
SETTLED.forEach(function(from) {
    assert.strictEqual(after('lxmf_step', from, 'waiting'), from, from + ' is never parked');
    assert.strictEqual(after('lxmf_step', from, 'sending'), from, from + ' is never woken');
});
assert.strictEqual(after('lxmf_delivery_progress', 'sending', 'waiting'), 'sending', 'transfer progress never parks a message');
// It leaves only by the `sending` of a new attempt, or by an outcome.
assert.strictEqual(after('lxmf_step', 'waiting', 'sending'), 'sending');
ON_ITS_WAY.concat(PROGRESS).filter(function(from) { return from !== 'sending'; }).forEach(function(from) {
    assert.strictEqual(after('lxmf_step', from, 'sending'), from, 'sending moves only a waiting message, not ' + from);
});
MOVES.forEach(function(step) {
    assert.strictEqual(after('lxmf_step', 'waiting', step), 'waiting', 'a stale ' + step + ' step leaves waiting');
    assert.strictEqual(after('lxmf_delivery_progress', 'waiting', step), 'waiting', 'stale ' + step + ' progress leaves waiting');
});
['delivered', 'propagated', 'failed', 'cancelled', 'rejected', 'timeout'].forEach(function(step) {
    assert.strictEqual(after('lxmf_step', 'waiting', step), step, step + ' replaces waiting');
});
assert.strictEqual(after('lxmf_step', 'waiting', 'error'), 'failed');
// Parking drops the progress of the attempt that ended, and takes the server's id.
events.lxmfConversation = [{ id: 'out_1', direction: 'outbound', state: 'sending_via_link', delivery_progress: 0.4,
    delivery_link_id: 'link', delivery_representation: 'resource' }];
handlers.lxmf_step({ step: 'waiting', msg_id: id, client_msg_id: 'out_1', method: 'direct' });
assert.deepStrictEqual(Object.assign({}, events.lxmfConversation[0]),
    { id: id, direction: 'outbound', state: 'waiting', delivery_method: 'direct' });

// The conversation list follows the step for a chat's last message, and only that message.
var listRenders = 0;
events._renderConversationsFromCache = function() { listRenders++; };
var lastRow = { hash: 'peer', last_direction: 'outbound', last_msg_id: 'm2', last_state: 'sending' };
var inboundRow = { hash: 'other', last_direction: 'inbound', last_msg_id: 'm9', last_state: null };
events.lxmfConversations = [inboundRow, lastRow];
events.lxmfConversation = [];
function listStep(msgId, step, extra) {
    handlers.lxmf_step(Object.assign({ msg_id: msgId, step: step }, extra || {}));
    return lastRow.last_state;
}
assert.strictEqual(listStep('m2', 'waiting'), 'waiting');
assert.strictEqual(listRenders, 1, 'the list is drawn again when its mark changes');
assert.strictEqual(listStep('m2', 'waiting'), 'waiting');
assert.strictEqual(listRenders, 1, 'and not when nothing changed');
assert.strictEqual(listStep('m1', 'failed', { destination: 'peer' }), 'waiting', 'an older message in the same chat is not the row');
assert.strictEqual(listStep('m2', 'routing'), 'waiting');
assert.strictEqual(listStep('m2', 'sending', { wake: 'wake_user' }), 'sending');
assert.strictEqual(listStep('m2', 'sent'), 'sent');
assert.strictEqual(listStep('m2', 'delivered'), 'delivered');
assert.strictEqual(listStep('m2', 'waiting'), 'delivered', 'the row keeps a bubble\'s precedence');
assert.strictEqual(listStep('m2', 'failed'), 'delivered');
assert.strictEqual(listRenders, 4);
handlers.lxmf_step({ msg_id: 'm9', step: 'waiting' });
assert.strictEqual(inboundRow.last_state, null, 'a received message has no delivery mark');
events.lxmfConversations = [];

// The list draws the same mark as the bubble, before the preview.
var listContainer = { innerHTML: '', _convSwipeAttached: true, querySelectorAll: function() { return []; } };
var list = {
    document: { getElementById: function() { return listContainer; } },
    lxmfActiveContact: null,
    _ghostConversationHash: null,
    _messageProgressPercent: function() { return null; },
    _mergeOptimisticConversation: function(rows) { return rows; },
    _canonicalConversationHash: function(hash) { return hash; },
    _conversationNameInfo: function(hash, name) { return { name: name, isHash: false }; },
    _conversationListPreview: function(row) { return row.last_message; },
    formatConvTime: function() { return 'now'; },
    identityAvatar: function() { return ''; },
    _peerInfo: function() { return null; },
    escapeHtml: function(value) { return String(value); },
    ratspeakDisplayNameHtml: function(name) { return name; },
    _refreshRenderedConversationPresence: function() {}
};
vm.createContext(list);
vm.runInContext(functionSource('_messageStateIconHtml') + functionSource('_renderConversationsFromCache'), list);
list._renderConversationsFromCache([
    { hash: 'a', display_name: 'Mara', last_message: 'Back by six', last_direction: 'outbound', last_state: 'waiting', last_msg_id: 'm2' },
    { hash: 'b', display_name: 'Ivy', last_message: 'Bring the charger', last_direction: 'inbound', last_state: null, last_msg_id: 'm9' },
    { hash: 'c', display_name: 'Tess', last_message: 'Saturday?', last_direction: 'outbound', last_state: 'rejected', last_msg_id: 'm4' }
]);
var listRows = listContainer.innerHTML.split('<div role="button"').slice(1);
assert.strictEqual(listRows.length, 3);
assert(listRows[0].includes('msg-state-waiting') && listRows[0].includes(HOURGLASS) &&
    listRows[0].indexOf('msg-state-waiting') < listRows[0].indexOf('You: Back by six'), 'the hourglass sits before the preview');
assert(!listRows[0].includes('Waiting to send</') && !listRows[0].includes('conv-unread-badge'), 'no status words and no badge');
assert(!listRows[1].includes('msg-state-icon'), 'a chat that ends on a received message shows no mark');
assert(listRows[2].includes('msg-state-rejected') && !listRows[2].includes('msg-state-label'),
    'the list shows a mark, never a word');
assert(icon('rejected', 'direct').includes('<span class="msg-state-label">Rejected</span>'), 'the bubble keeps its word');

// The notice: 24 hours is named when that is why; turning the setting off says nothing.
var noticeToasts = [];
var opened = [];
var notice = {
    _canonicalConversationHash: function(hash) { return hash || ''; },
    _lxmfAttentionForeground: function() { return true; },
    _isConversationActivelyVisible: function() { return false; },
    _conversationNameInfo: function() { return { name: 'Mara' }; },
    _hashFallbackName: function(hash) { return hash; },
    lxmfActiveThread: '',
    openConversationWith: function(hash) { opened.push(hash); },
    showToast: function(text, kind, ms, action) { noticeToasts.push([text, kind]); if (action) action(); }
};
vm.createContext(notice);
vm.runInContext(functionSource('_noticeUndeliveredMessage'), notice);
notice._noticeUndeliveredMessage('peer', 'wait_expired');
notice._noticeUndeliveredMessage('peer', 'path_unavailable');
notice._noticeUndeliveredMessage('peer');
notice._noticeUndeliveredMessage('peer', 'keep_trying_off');
notice._noticeUndeliveredMessage('', 'keep_trying_off');
assert.deepStrictEqual(noticeToasts, [
    ['Message to Mara not delivered after 24 hours', 'toast-action'],
    ['Message to Mara not delivered', 'toast-action'],
    ['Message to Mara not delivered', 'toast-action']
]);
assert.deepStrictEqual(opened, ['peer', 'peer', 'peer'], 'tapping the notice opens the chat');
var noticed = [];
events._noticeUndeliveredMessage = function(destination, reason) { noticed.push([destination, reason]); };
events.lxmfConversation = [{ id: 'm', direction: 'outbound', state: 'waiting' }];
handlers.lxmf_step({ msg_id: 'm', step: 'failed', reason: 'wait_expired', destination: 'peer' });
handlers.lxmf_step({ msg_id: 'm', step: 'waiting', destination: 'peer' });
assert.strictEqual(events.lxmfConversation[0].state, 'failed', 'the expired message shows Not delivered');
assert.deepStrictEqual(noticed, [['peer', 'wait_expired']], 'the step hands its reason to the notice; waiting itself is silent');

// The menu on a waiting message: Try now, Stop trying, Show in Activity, then the usual rows. No caption.
function FakeElement(tag) {
    this.tagName = tag;
    this.children = [];
    this.attrs = {};
    this.className = '';
    this.innerHTML = '';
    this.style = { setProperty: function() {} };
    this.classList = { add: function() {}, remove: function() {} };
}
FakeElement.prototype.setAttribute = function(name, value) { this.attrs[name] = value; };
FakeElement.prototype.appendChild = function(child) { this.children.push(child); return child; };
FakeElement.prototype.addEventListener = function() {};
FakeElement.prototype.querySelector = function() { return null; };
FakeElement.prototype.querySelectorAll = function() { return []; };
var menuCalls = [];
var menuToasts = [];
var menuConfirms = 0;
var cancelReply = { cancelled: true, row_marked_stopped: true };
var menuBody = new FakeElement('body');
var menuContext = {
    lxmfConversation: [],
    lxmfActiveContact: 'peer',
    _msgSelection: null,
    _activeContextMenu: null,
    document: { body: menuBody, activeElement: null, getElementById: function() { return null; },
        createElement: function(tag) { return new FakeElement(tag); } },
    window: { addEventListener: function() {} },
    RS: { invoke: function(command, args) { menuCalls.push([command, JSON.parse(JSON.stringify(args))]); return Promise.resolve(cancelReply); } },
    rsConfirm: function() { menuConfirms++; return Promise.resolve(true); },
    showToast: function(text, kind) { menuToasts.push([text, kind]); },
    showMessageInActivity: function() {},
    _dismissContextMenu: function() { return true; },
    _shouldPreserveLxmfComposerKeyboard: function() { return false; },
    _messageActionShouldPreserveComposer: function() { return false; },
    _restoreLxmfComposerKeyboard: function() {},
    _preventMessageActionFocusSteal: function() {},
    _hasOwnReaction: function() { return false; },
    _bindMessageFocusPreservingActivation: function(element, handler) { element.activate = handler; },
    _messageSelectionActive: function() { return false; },
    _messageCopyText: function() { return 'text'; },
    _messageMediaContextAction: function() { return null; },
    _canonicalConversationHash: function(hash) { return hash; },
    _layoutMessageActionPanels: function() {},
    _cacheActiveConversation: function() {},
    renderConversation: function() {}
};
vm.createContext(menuContext);
vm.runInContext(source.slice(0, source.indexOf('var lxmfIdentity')), menuContext);
['_isCanonicalLxmfMsgId', '_messageDeliveryMethod', '_messageIsPaper', '_messageCanCancelSend', '_messageCanRetry',
    '_messageCanShowInActivity', '_messageIsWaiting', '_messageSelectable', '_messageActionIcon',
    '_prepareMessageActionTarget', '_findLxmfMessageById', '_markLxmfMessageCancelled', '_invokeLxmfCancel',
    '_showLxmfStopResult', '_tryWaitingLxmfMessage', '_stopWaitingLxmfMessage', '_showMsgContextMenu']
    .forEach(function(name) { vm.runInContext(functionSource(name), menuContext); });
function openMenu(msg) {
    var bubble = new FakeElement('div');
    bubble.querySelector = function(selector) { return selector === '.lxmf-msg-content' ? {} : null; };
    menuContext._showMsgContextMenu(msg, 0, 0, bubble, null);
    var panel = menuBody.children[menuBody.children.length - 1].children[1];
    return {
        panel: panel,
        actions: panel.children.map(function(child) { return child.attrs['data-message-action']; }),
        labels: panel.children.map(function(child) { return (/<span>([^<]*)<\/span>/.exec(child.innerHTML) || [])[1]; }),
        button: function(action) {
            return panel.children.filter(function(child) { return child.attrs['data-message-action'] === action; })[0];
        }
    };
}
function sentMessage(state) { return { id: id, direction: 'outbound', state: state, delivery_method: 'opportunistic' }; }
var waitingMenu = openMenu(sentMessage('waiting'));
assert.deepStrictEqual(waitingMenu.actions, ['try-now', 'stop-trying', 'show-activity', 'reply', 'copy-message']);
assert.deepStrictEqual(waitingMenu.labels, ['Try now', 'Stop trying', 'Show in Activity', 'Reply', 'Copy']);
assert(waitingMenu.panel.children.every(function(child) { return child.tagName === 'button'; }),
    'the menu holds buttons only: no caption row');
assert.strictEqual(waitingMenu.button('try-now').className, 'msg-ctx-btn msg-ctx-try-now');
assert.strictEqual(waitingMenu.button('stop-trying').className, 'msg-ctx-btn msg-ctx-stop-trying');
assert(waitingMenu.button('stop-trying').innerHTML.includes('<path d="M18 6 6 18M6 6l12 12"/>'));
assert.deepStrictEqual(openMenu(sentMessage('failed')).actions, ['retry', 'show-activity', 'reply', 'select', 'copy-message'],
    'an ended message keeps Retry and gains nothing');
assert.deepStrictEqual(openMenu(sentMessage('sending')).actions, ['reply', 'copy-message']);
assert.deepStrictEqual(openMenu(sentMessage('delivered')).actions, ['reply', 'select', 'copy-message']);

(async function() {
    // Try now asks the runtime and leaves the bubble to the attempt's own step.
    var waiting = sentMessage('waiting');
    menuContext.lxmfConversation = [waiting];
    openMenu(waiting).button('try-now').activate();
    await Promise.resolve();
    assert.deepStrictEqual(menuCalls, [['try_waiting_now', { args: { msg_id: id } }]]);
    assert.strictEqual(waiting.state, 'waiting');
    assert.deepStrictEqual(menuToasts, [], 'Try now says nothing');

    // Stop trying is the existing cancel, with no confirmation, and ends as Cancelled.
    menuCalls.length = 0;
    openMenu(waiting).button('stop-trying').activate();
    for (var turn = 0; turn < 4; turn++) await Promise.resolve();
    assert.deepStrictEqual(menuCalls, [['cancel_lxmf_message', { args: { msg_id: id } }]]);
    assert.strictEqual(menuConfirms, 0, 'Stop trying asks nothing');
    assert.strictEqual(waiting.state, 'cancelled');
    assert(context._messageCanRetry(waiting), 'and Retry is offered again');
    assert.deepStrictEqual(menuToasts, [['No live send remained. The local message was cancelled, but a copy may still arrive.', 'toast-info']],
        'the existing stop toasts apply');

    // A refusal leaves the message waiting and says why.
    menuToasts.length = 0;
    waiting.state = 'waiting';
    menuContext.RS.invoke = function() { return Promise.reject(new Error('Messaging is not ready')); };
    openMenu(waiting).button('try-now').activate();
    openMenu(waiting).button('stop-trying').activate();
    for (turn = 0; turn < 4; turn++) await Promise.resolve();
    assert.strictEqual(waiting.state, 'waiting');
    assert.deepStrictEqual(menuToasts, [['Messaging is not ready', 'toast-error'],
        ['Could not stop sending: Messaging is not ready', 'toast-error']]);

    console.log('LXMF delivery icon tests passed');
})().catch(function(error) { console.error(error); process.exit(1); });
