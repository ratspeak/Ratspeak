#!/usr/bin/env node
'use strict';

const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');
const source = fs.readFileSync(path.join(__dirname, '../../dashboard/static/js/lxmf.js'), 'utf8');
const loader = source.slice(source.indexOf('var _loadConversationsTimer = null;'), source.indexOf('function _renderConversationsFromCache'));
const pending = [], timers = new Map(), renders = [], home = [];
let sequence = 0, identity = 0, epoch = 0;
const container = { innerHTML: '', querySelector: () => null };
const context = {
    Date, lxmfConversations: [],
    document: { getElementById: () => container },
    _conversationOwnerSnapshot: () => ({ identity, epoch }),
    _conversationOwnerIdentityIsCurrent: owner => owner.identity === identity,
    _conversationOwnerIsCurrent: owner => owner.identity === identity && owner.epoch === epoch,
    _renderConversationsFromCache: rows => renders.push(rows),
    renderDashboardRecentMessages: () => home.push(context.lxmfConversations),
    setTimeout(fn) { timers.set(++sequence, fn); return sequence; },
    clearTimeout(id) { timers.delete(id); },
    RS: {
        gestures: { attachPullToRefresh() {} },
        invoke(command) {
            return new Promise((resolve, reject) => pending.push({ command, resolve, reject }));
        }
    }
};
vm.createContext(context);
vm.runInContext(loader, context);
async function settle() { for (let i = 0; i < 6; i++) await Promise.resolve(); }
function fire() { const batch = [...timers.values()]; timers.clear(); batch.forEach(fn => fn()); }

(async function() {
    context._loadConversationsReal();
    for (let i = 0; i < 8; i++) context.loadConversationsForce();
    assert.equal(pending.length, 1, 'force refresh must keep a single outstanding IPC read');
    pending[0].resolve([{ hash: 'first' }]); await settle();
    assert.equal(pending.length, 2, 'repeated refreshes coalesce into one subsequent read');
    pending[1].resolve([{ hash: 'second' }]); await settle();
    assert.equal(context.lxmfConversations[0].hash, 'second');
    assert.strictEqual(home.at(-1), renders.at(-1), 'Home and Messages must receive the same accepted snapshot');

    context._loadConversationsReal();
    context._acceptConversations([{ hash: 'broadcast' }]);
    pending[2].resolve([{ hash: 'old snapshot' }]); await settle();
    assert.equal(context.lxmfConversations[0].hash, 'broadcast', 'late reads cannot undo a newer mutation broadcast');

    context._loadConversationsReal();
    identity++; context._resetConversationList();
    assert.equal(context.lxmfConversations.length, 0, 'identity replacement clears the old list immediately');
    context.loadConversationsForce();
    assert.equal(pending.length, 4, 'identity replacement must not pretend the old IPC was cancelled');
    pending[3].resolve([{ hash: 'private old identity' }]); await settle();
    assert.equal(context.lxmfConversations.length, 0);
    assert.equal(pending.length, 5, 'the new identity is loaded when the retained request settles');
    pending[4].resolve([{ hash: 'new identity' }]); await settle();
    assert.equal(context.lxmfConversations[0].hash, 'new identity');

    context._loadConversationsReal();
    pending[5].reject(new Error('temporary failure')); await settle();
    assert.equal(timers.size, 1);
    identity++; context._resetConversationList(); fire();
    assert.equal(pending.length, 6, 'an old identity retry must never run after reset');
    context._loadConversationsReal();
    for (let i = 6; i < 9; i++) {
        pending[i].reject(new Error('unavailable')); await settle(); fire();
    }
    assert.equal(pending.length, 9, 'failures must stop after two retries');
    assert.equal(context._convFetchInFlight, false);
    assert(container.innerHTML.includes("Couldn't load conversations."));

    // Use the actual conversation reader to cover A -> B -> A navigation.
    vm.runInContext(source.slice(source.indexOf('function _loadConversation('), source.indexOf('// Placeholder row shown until')), context);
    let cacheWrites = 0;
    Object.assign(context, {
        _canonicalConversationHash: hash => hash,
        _msgReactions: {}, cacheGet: () => null,
        renderConversation() {},
        _mergeConversationMessages: (_hash, messages) => messages,
        cacheSet() { cacheWrites++; }
    });
    context.lxmfActiveContact = 'a';
    context._loadConversation('a'); epoch += 2;
    pending[9].resolve({ messages: [{ content: 'old A session' }] }); await settle();
    assert.equal(context.lxmfConversation.length, 0, 'a prior A navigation must not replace the new A transcript');
    assert.equal(cacheWrites, 0, 'stale conversation data must not enter the cache');
    const dashboard = source.slice(source.indexOf('function renderDashboardRecentMessages()'), source.indexOf('var _loadConversationsTimer'));
    assert(!dashboard.includes('RS.invoke('), 'Home must not maintain a parallel conversation read path');
    const nav = fs.readFileSync(path.join(__dirname, '../../dashboard/static/js/nav.js'), 'utf8');
    const contactStart = nav.indexOf('    contacts: function() {');
    const contactEnd = nav.indexOf('\n};', contactStart);
    vm.runInContext('var contactViewLoad = ' + nav.slice(contactStart, contactEnd).replace('    contacts: ', '') + ';', context);
    context.RS.conversationOwner = {
        snapshot: context._conversationOwnerSnapshot,
        isIdentityCurrent: context._conversationOwnerIdentityIsCurrent
    };
    context.lxmfContacts = [];
    context.contactViewLoad(); identity++;
    pending[10].resolve([{ hash: 'old contact identity' }]); await settle();
    assert.equal(context.lxmfContacts.length, 0, 'late Contacts fallback must obey identity replacement');
    context.contactViewLoad(); context.lxmfContacts = [{ hash: 'new contact broadcast' }];
    pending[11].resolve([{ hash: 'old contact read' }]); await settle();
    assert.equal(context.lxmfContacts[0].hash, 'new contact broadcast', 'Contacts fallback cannot undo a newer snapshot');
    console.log('conversation loading: single flight, broadcast ordering, identity reset, retries and navigation passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
