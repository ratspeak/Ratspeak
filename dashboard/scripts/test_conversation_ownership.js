#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var root = path.join(__dirname, '..', '..');
var lxmf = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');

var controllerSource = lxmf.slice(
    lxmf.indexOf('function _canonicalConversationHash'),
    lxmf.indexOf('function _detachMessageLongPressHandlers')
);
var contactAddedSource = lxmf.slice(
    lxmf.indexOf("RS.listen('contact_added'"),
    lxmf.indexOf("RS.listen('contact_error'")
);

var notifications = [];
var context = {
    window: null,
    currentView: 'message',
    document: { hidden: false },
    lxmfActiveContact: null,
    RS: {
        isAttentionForeground: function() {
            return !context.document.hidden;
        },
        voiceMemos: {
            onConversationChanged: function(hash, reason) {
                notifications.push({ hash: hash, reason: reason });
            },
        },
    },
};
context.window = context;
vm.runInNewContext(
    'var _conversationEpoch = 0; var _conversationIdentityGeneration = 0;\n' + controllerSource,
    context,
    { filename: 'conversation-owner.js' }
);

assert.equal(context._canonicalConversationHash('  AABBcc  '), 'aabbcc');

var ownerA = context._activateConversation('AABBCC', 'navigation');
assert.equal(ownerA.hash, 'aabbcc');
assert.equal(notifications.length, 1);
assert(context._conversationOwnerIsCurrent(ownerA));
assert(context._isConversationActivelyVisible('AABBCC'),
    'the selected peer is actively visible only in foreground Messages');
context.currentView = 'settings';
assert(!context._isConversationActivelyVisible('aabbcc'),
    'a selected peer on another screen must not suppress message attention');
context.currentView = 'message';
context.document.hidden = true;
assert(!context._isConversationActivelyVisible('aabbcc'),
    'a hidden app must not treat its last selected peer as actively visible');
context.document.hidden = false;

var sameA = context._activateConversation('aabbcc', 'navigation');
assert.equal(sameA.epoch, ownerA.epoch, 'case-equivalent activation must not advance ownership');
assert.equal(notifications.length, 1, 'case-equivalent activation must not notify a navigation');

var ownerB = context._activateConversation('ddeeff', 'navigation');
assert(!context._conversationOwnerIsCurrent(ownerA), 'A work must retire after navigating to B');
assert(context._conversationOwnerIsCurrent(ownerB));

var ownerA2 = context._activateConversation('aabbcc', 'navigation');
assert(ownerA2.epoch > ownerA.epoch, 'A to B to A must allocate a new epoch');
assert(!context._conversationOwnerIsCurrent(ownerA), 'the original A epoch must not revive through ABA');

context._resetConversationSession('identity_replaced');
assert.equal(context.lxmfActiveContact, null);
assert(!context._conversationOwnerIsCurrent(ownerA2), 'identity replacement must retire conversation work');

assert(!contactAddedSource.includes('lxmfActiveContact ='),
    'contact_added must remain observational and never navigate');
assert(!contactAddedSource.includes("get_conversation"),
    'contact_added must not mark an unopened conversation read while refreshing contacts');

var conversationUpdateSource = lxmf.slice(
    lxmf.indexOf("RS.listen('conversation_update'"),
    lxmf.indexOf("RS.listen('lxmf_step'")
);
assert(conversationUpdateSource.includes('var hash = _canonicalConversationHash(data.hash)'));
assert(conversationUpdateSource.includes('msg.source = _canonicalConversationHash(msg.source)'));
assert(conversationUpdateSource.includes('msg.destination = _canonicalConversationHash(msg.destination)'));
assert(conversationUpdateSource.includes('var conversationVisible = _isConversationActivelyVisible(msg.source)'));
assert(conversationUpdateSource.includes('if (conversationVisible) {') &&
    conversationUpdateSource.includes("RS.invoke('mark_read', { hash: msg.source })"),
    'only a conversation the user can currently see may be marked read on arrival');
assert(conversationUpdateSource.includes('if (appForeground && !conversationVisible) {'),
    'foreground attention belongs to an in-app toast only outside the visible conversation');
assert(conversationUpdateSource.includes("!window.__TAURI_INTERNALS__ && !appForeground"),
    'the browser fallback must notify only in background, regardless of the last selected chat');
assert(!conversationUpdateSource.includes('document.hidden &&'),
    'message attention must use the shared focus-aware lifecycle predicate');
var deletionSource = lxmf.slice(
    lxmf.indexOf("RS.listen('conversation_hidden'"),
    lxmf.indexOf('// 30s re-check')
);
assert.equal((deletionSource.match(/var hash = _canonicalConversationHash\(data\.hash\)/g) || []).length, 2,
    'hidden and deleted events must use the same canonical cache boundary');

console.log('Conversation ownership tests passed');
