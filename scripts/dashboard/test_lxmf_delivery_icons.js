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
    _messageProgressPercent: function() { return null; },
    escapeHtml: function(value) { return String(value); }
};
vm.createContext(context);
vm.runInContext(source.slice(source.indexOf('var MSG_MARKS = {'), source.indexOf('function _messageMarkSvg(')), context);
['_messageMarkSvg', '_messageMarkKind', '_messageStateIconHtml'].forEach(function(name) {
    vm.runInContext(functionSource(name), context);
});

function icon(state, method) {
    return context._messageStateIconHtml({
        id: 'm1',
        state: state,
        delivery_method: method,
        direction: 'outbound'
    });
}

function assertSent(method) {
    var html = icon('sent', method);
    assert(html.includes('msg-mark--sent'), method + ' sent must use the sent mark');
    assert(html.includes('aria-label="Sent"'), method + ' sent must remain accessible as Sent');
    assert(!html.includes('msg-mark--delivered'), method + ' sent must not claim delivery');
}

function assertDelivered(method) {
    var html = icon('delivered', method);
    assert(html.includes('msg-mark--delivered'), method + ' proof must use the delivered mark');
    assert(html.includes('aria-label="Delivered"'), method + ' proof must be accessible as Delivered');
    assert(!html.includes('msg-mark--sent'), method + ' proof must not be collapsed back to Sent');
    assert(html.includes('<polyline points="1.5 8.5 5 12 11.5 4.5"/><polyline points="8.5 11 9.5 12 16 4.5"/>'),
        method + ' proof must show two checks');
}

['opportunistic', 'direct'].forEach(assertSent);
['opportunistic', 'direct'].forEach(assertDelivered);
assertDelivered(undefined);

var propagated = icon('propagated', 'propagated');
assert(propagated.includes('msg-mark--propagated'));
assert(propagated.includes('aria-label="In the Offline Inbox"'));

// Not delivered and Waiting are buttons that open the message menu; the rest are images.
var failed = icon('failed', 'opportunistic');
assert(failed.startsWith('<button type="button" class="msg-mark msg-mark--failed" data-msg-id="m1"'));
assert(failed.includes('aria-haspopup="dialog" aria-expanded="false"') && failed.includes('aria-label="Not delivered. Options"') && failed.includes('title="Not delivered"'),
    'the bubble shows only the mark; its name is the hover title');
assert(failed.includes('<line x1="8" y1="2.5" x2="8" y2="9.5"/>') && !failed.includes('x1="4" y1="4"'),
    'not delivered is a "!", not the cancel cross');
var waiting = icon('waiting', 'direct');
assert(waiting.startsWith('<button type="button" class="msg-mark msg-mark--waiting"') && waiting.includes('aria-label="Waiting to send. Options"'));
assert(icon('cancelled', 'direct').includes('x1="4" y1="4"'), 'cancelled keeps the cross');
assert(icon('timeout', 'direct').includes('aria-label="Not delivered. Options"'), 'a timeout is Not delivered');
assert(!icon('read', 'direct').includes('msg-mark--read'), 'no outbound message shows a Read state');
assert(icon('routing', 'direct').includes('msg-mark--sending') && icon('routing', 'direct').startsWith('<span'));

var stopped = icon('cancelled', 'direct');
assert(stopped.includes('aria-label="Sending cancelled"'));
assert(source.includes('aria-label="Cancel sending message">Cancel</button>'));
assert(source.includes("message: 'Cancel preparation and retries for this message?'"));
assert(source.includes('Cancelled local retries. A copy already handed to the network may still arrive.'));
assert(source.includes('No live send remained. The local message was cancelled, but a copy may still arrive.'));
assert(source.includes('Cancelled before the message left this device.'));
assert(source.includes("title: 'Cancel sending?'"));

// Paper: "Paper" in the mark's place, terminal everywhere, never a clock or check.
var paperIcon = icon('paper', 'paper');
assert(paperIcon.includes('msg-mark--paper') && paperIcon.includes('aria-label="Paper message"'));
assert(!paperIcon.includes('msg-mark--sending') && !paperIcon.includes('msg-mark--sent'));
vm.runInContext(source.slice(0, source.indexOf('var lxmfIdentity')), context);
['_messageDeliveryMethod', '_messageIsPaper', '_messagePaperChipHtml', '_messageCanCancelSend']
    .forEach(function(name) { vm.runInContext(functionSource(name), context); });
assert(context._isTerminalMessageState('paper'));
assert.strictEqual(context._messageCanCancelSend({ direction: 'outbound', state: 'paper' }), false,
    'paper messages have no cancel, retry, or progress');
assert(context._messageIsPaper({ state: 'delivered', delivery_method: 'paper' }), 'inbound filed paper rows are paper');
var outboundChip = context._messagePaperChipHtml({ id: 'm1' }, true);
assert(outboundChip.includes('<button type="button" class="msg-mark msg-mark--paper" data-msg-id="m1"'));
assert(outboundChip.includes('aria-label="Paper message, not sent over the network. Show QR code"'));
assert(outboundChip.includes('<span aria-hidden="true">Paper</span>'));
var inboundChip = context._messagePaperChipHtml({ id: 'm2' }, false);
assert(!inboundChip.includes('<button') && inboundChip.includes('aria-label="Received as a paper message"'));

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
    _pendingLxmfCancelByClientId: {},
    _flushPendingLxmfCancel: function() {},
    _cacheActiveConversation: function() {},
    renderConversation: function() {},
    _noticeUndeliveredMessage: function() {},
    showToast: function() {}
};
vm.createContext(events);
vm.runInContext(source.slice(0, source.indexOf('var lxmfIdentity')), events);
vm.runInContext(functionSource('_messageStateAfterStep') +
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
assert(source.includes("var mark = paper ? _messagePaperChipHtml(msg, isOut) : (isOut ? _messageStateIconHtml(msg) : '');"));
assert(source.includes("'.md-spoiler:not(.is-revealed), .md-code-copy, .msg-mark--paper'"),
    'a hold on the paper mark opens message actions and swallows the trailing tap');
assert(source.includes("_messageMenuButton(itemClass + ' msg-ctx-paper-qr', 'paper-qr', 'qr', 'Show QR code')") &&
    source.includes("'copy-paper-link', 'copy', 'Copy paper link'"));
assert(source.includes("showToast(ok ? 'Paper link copied' : 'Could not copy'"));

var css = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '09-messaging.css'), 'utf8');
assert(/button\.msg-mark::before\s*\{[^}]*inset:\s*-5px;/s.test(css) && /button\.msg-mark::before\s*\{\s*inset:\s*-15px;/.test(css),
    'a mark that is a button keeps a 24 px hit area, 44 on touch');
assert(/\.msg-mark--failed, \.msg-mark--rejected\s*\{\s*color:\s*var\(--danger-ink\);/.test(css), 'Not delivered is the one coloured mark');
assert(!/\.msg-mark--(?:sent|delivered|propagated|sending)\s*\{/.test(css),
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
assert(source.includes("_messageMenuButton(itemClass + ' msg-ctx-retry', 'retry', 'refresh', 'Retry…')"));
assert(source.includes("{ label: 'Automatic', icon: 'refresh', onSelect: function() { _retryLxmfMessage(msg, 'auto'); } },"),
    'Retry… offers the send methods, Automatic first');
assert(source.includes("anchor = anchor && anchor.isConnected ? anchor : _findRenderedMessageBubble(container, msg.id);"),
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
var retryAt = source.indexOf("'retry', 'refresh', 'Retry…'");
var activityAt = source.indexOf("'show-activity', 'activity', 'Show in Activity'");
var replyAt = source.indexOf("' msg-ctx-reply', 'reply', 'reply', 'Reply'");
assert(retryAt !== -1 && activityAt > retryAt && replyAt > activityAt, 'Show in Activity sits right after Retry');

// ---------------------------------------------------------------------------
// Waiting: a message Ratspeak keeps trying to send.
// ---------------------------------------------------------------------------
var WAITING_LABEL = 'Waiting to send';
var HOURGLASS = '<path d="M4 2h8M4 14h8M5 2v2.5L8 8l-3 3.5V14M11 2v2.5L8 8l3 3.5V14"/>';

// The mark: an hourglass, named on hover and to a screen reader, never the clock, no words.
var waitingIcon = icon('waiting', 'opportunistic');
assert(waitingIcon.includes('class="msg-mark msg-mark--waiting"'));
assert(waitingIcon.includes('aria-label="' + WAITING_LABEL + '. Options"') &&
    waitingIcon.includes('title="' + WAITING_LABEL + '. Ratspeak keeps trying for up to 24 hours."'),
    'the hover title says how long it keeps trying');
assert(waitingIcon.includes(HOURGLASS), 'waiting is the hourglass');
assert(!waitingIcon.includes('msg-mark--sending') && !waitingIcon.includes('<circle'), 'waiting is not the sending clock');
assert(!waitingIcon.includes('<span'), 'the bubble carries no words for waiting');
assert(!/\.msg-mark--waiting[^{]*\{[^}]*animation/.test(css), 'the hourglass is still');

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
assert(source.includes("(canCancelSend && !imageCancels ? '' : '<time class=\"msg-clock\""),
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

// The conversation list shows no delivery mark, whatever the last message's state; the bubble keeps its own.
var listContainer = { innerHTML: '', querySelector: function() { return null; } };
var list = {
    document: { getElementById: function() { return listContainer; } },
    lxmfActiveContact: null,
    _ghostConversationHash: null,
    _conversationsFirstLoadDone: true,
    _mergeOptimisticConversation: function(rows) { return rows; },
    _canonicalConversationHash: function(hash) { return hash; },
    _conversationNameInfo: function(hash, name) { return { name: name, isHash: false }; },
    _conversationListPreview: function(row) { return row.last_message; },
    _conversationPresence: function() { return { active: false, heard: 0 }; },
    _bindConversationList: function() {},
    _renderFirstRunDetail: function() {},
    escapeHtml: function(value) { return String(value); },
    ratspeakDisplayNameHtml: function(name) { return name; },
    RS: { kit: { avatar: function() { return '<avatar>'; }, time: { full: function() { return 'full'; }, row: function() { return 'now'; } }, hash: { text: function(h) { return h; } } } }
};
vm.createContext(list);
vm.runInContext(functionSource('_conversationRowHtml') + functionSource('_renderConversationsFromCache'), list);
var LIST_STATES = Object.keys(context.LXMF_OUTBOUND_STATES);
['sent', 'delivered', 'waiting', 'failed'].forEach(function(state) {
    assert(LIST_STATES.indexOf(state) !== -1, state + ' is a state the list is checked for');
});
list._renderConversationsFromCache(LIST_STATES.map(function(state) {
    return { hash: 'out-' + state, display_name: 'Mara', last_message: 'Back by six', last_direction: 'outbound',
        last_state: state, last_delivery_method: 'direct', last_msg_id: 'm-' + state, timestamp: 1 };
}).concat([
    { hash: 'in', display_name: 'Ivy', last_message: 'Bring the charger', last_direction: 'inbound',
        last_state: null, last_msg_id: 'm9', timestamp: 1, unread: 2 }
]));
var listRows = listContainer.innerHTML.split('<li>').slice(1);
assert.strictEqual(listRows.length, LIST_STATES.length + 1);
LIST_STATES.forEach(function(state, index) {
    var row = listRows[index];
    assert(row.includes('data-hash="out-' + state + '"'));
    assert(!row.includes('msg-mark') && !row.includes('<svg'), 'the list shows no delivery mark for ' + state);
    assert(row.includes('<span class="ui-row-sub"><span class="t">You: Back by six</span></span>'), state + ' previews as You: and the text alone');
    assert(row.includes('<span class="ui-time" title="full">now</span>'));
});
var inboundListRow = listRows[LIST_STATES.length];
assert(!inboundListRow.includes('msg-mark') && !inboundListRow.includes('You: '));
assert(inboundListRow.includes('<span class="t">Bring the charger</span><span class="ui-count" aria-label="2 unread">2</span>'),
    'the unread count follows the preview');
assert(inboundListRow.includes('class="ui-row msg-conv-row is-unread"'));
assert(icon('rejected', 'direct').includes('<span>Rejected</span>'), 'the bubble keeps its word');
LIST_STATES.forEach(function(state) {
    assert(icon(state, 'direct').includes('class="msg-mark '), 'the bubble keeps its mark for ' + state);
});

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
FakeElement.prototype.closest = function() { return null; };
var menuCalls = [];
var menuToasts = [];
var menuConfirms = 0;
var cancelReply = { cancelled: true, row_marked_stopped: true, only_waiting: true };
var menuBody = new FakeElement('body');
var menuContext = {
    lxmfConversation: [],
    lxmfActiveContact: 'peer',
    _msgSelection: null,
    _activeContextMenu: null,
    document: { body: menuBody, activeElement: null, getElementById: function() { return null; },
        createElement: function(tag) { return new FakeElement(tag); } },
    window: { addEventListener: function() {} },
    isCompactLayout: function() { return false; },
    escapeHtml: function(value) { return String(value); },
    RS: { kit: { icon: function(name) { return '<' + name + '>'; } }, invoke: function(command, args) { menuCalls.push([command, JSON.parse(JSON.stringify(args))]); return Promise.resolve(cancelReply); } },
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
vm.runInContext(source.slice(source.indexOf('var MSG_QUICK_REACTIONS'), source.indexOf('// Visible transcript band')), menuContext);
['_isCanonicalLxmfMsgId', '_messageDeliveryMethod', '_messageIsPaper', '_messageCanCancelSend', '_messageCanRetry',
    '_messageCanShowInActivity', '_messageIsWaiting', '_messageSelectable', '_messageMenuButton',
    '_prepareMessageActionTarget', '_findLxmfMessageById', '_markLxmfMessageCancelled', '_invokeLxmfCancel',
    '_showLxmfStopResult', '_tryWaitingLxmfMessage', '_stopWaitingLxmfMessage', '_showMsgContextMenu']
    .forEach(function(name) { vm.runInContext(functionSource(name), menuContext); });
function openMenu(msg) {
    var bubble = new FakeElement('div');
    bubble.querySelector = function(selector) { return selector === '.msg-text' ? {} : null; };
    menuContext._showMsgContextMenu(msg, 0, 0, bubble, null);
    var panel = menuBody.children[menuBody.children.length - 1].children[2];
    var buttons = panel.children.filter(function(child) { return child.tagName === 'button'; });
    return {
        panel: panel,
        actions: buttons.map(function(child) { return child.attrs['data-message-action']; }),
        labels: buttons.map(function(child) { return (/<span class="lbl">([^<]*)<\/span>/.exec(child.innerHTML) || [])[1]; }),
        button: function(action) {
            return buttons.filter(function(child) { return child.attrs['data-message-action'] === action; })[0];
        }
    };
}
function sentMessage(state) { return { id: id, direction: 'outbound', state: state, delivery_method: 'opportunistic' }; }
var waitingMenu = openMenu(sentMessage('waiting'));
assert.deepStrictEqual(waitingMenu.actions, ['try-now', 'stop-trying', 'show-activity', 'reply', 'copy-message']);
assert.deepStrictEqual(waitingMenu.labels, ['Try now', 'Stop trying', 'Show in Activity', 'Reply', 'Copy']);
assert(waitingMenu.panel.children.every(function(child) { return child.tagName === 'button' || child.className === 'ui-menu-sep'; }),
    'the menu holds buttons and one rule: no caption row');
assert.strictEqual(waitingMenu.button('try-now').className, 'ui-menu-item msg-ctx-try-now');
assert.strictEqual(waitingMenu.button('stop-trying').className, 'ui-menu-item msg-ctx-stop-trying');
assert(waitingMenu.button('stop-trying').innerHTML.startsWith('<x>'));
assert.deepStrictEqual(openMenu(sentMessage('failed')).actions, ['retry', 'show-activity', 'reply', 'copy-message', 'select'],
    'an ended message keeps Retry… and gains nothing');
assert.deepStrictEqual(openMenu(sentMessage('sending')).actions, ['reply', 'copy-message']);
assert.deepStrictEqual(openMenu(sentMessage('delivered')).actions, ['reply', 'copy-message', 'select']);

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
    assert.deepStrictEqual(menuToasts, [], 'a message that only waited was never in the air: the mark says it all');

    // A new attempt that was in the air when it was stopped gets the existing result.
    waiting.state = 'waiting';
    cancelReply = { cancelled: true, live_owner_stopped: true, row_marked_stopped: true, only_waiting: false };
    openMenu(waiting).button('stop-trying').activate();
    for (turn = 0; turn < 4; turn++) await Promise.resolve();
    assert.strictEqual(waiting.state, 'cancelled');
    assert.deepStrictEqual(menuToasts, [['Cancelled local retries. A copy already handed to the network may still arrive.', 'toast-info']]);

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
