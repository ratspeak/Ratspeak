// Several conversations with one contact: cache keys, ownership on switch,
// message isolation, and a header that stays unlabelled for one conversation.
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const root = path.resolve(__dirname, '..', '..');
const lxmf = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
const threads = fs.readFileSync(path.join(root, 'dashboard/static/js/conversation_threads.js'), 'utf8');

function functionSource(name) {
    const start = lxmf.indexOf('function ' + name + '(');
    assert(start >= 0, name + ' must stay extractable');
    return lxmf.slice(start, lxmf.indexOf('\n}\n', start) + 3);
}

const PEER = 'aa'.repeat(16);
const SIDE = 'bb'.repeat(32);
const context = {
    lxmfActiveContact: PEER,
    lxmfConversation: [],
    _conversationEpoch: 0,
    _conversationIdentityGeneration: 0,
    cache: {},
    _notifyConversationOwnerChanged() {},
    cacheGet(key) { return context.cache[key]; },
    cacheSet(key, value) { context.cache[key] = value; },
};
vm.createContext(context);
vm.runInContext("var lxmfActiveThread = ''; var lxmfActiveThreads = null;", context);
[
    '_canonicalConversationHash', '_canonicalConversationThread', '_conversationKey',
    '_activeConversationKey', '_resolveConversationThread', '_conversationOwnerSnapshot',
    '_conversationOwnerIsCurrent', '_activateConversation', '_cacheActiveConversation',
    '_conversationMessagesFor', '_commitConversationMessages', '_appendConversationMessage',
].forEach((name) => vm.runInContext(functionSource(name), context));

assert.strictEqual(context._conversationKey(PEER.toUpperCase(), ''), PEER, 'Main keeps the bare contact key');
assert.strictEqual(context._conversationKey(PEER, SIDE.toUpperCase()), PEER + '~' + SIDE);

const mainOwner = context._conversationOwnerSnapshot();
assert.strictEqual(mainOwner.thread, '');
context._activateConversation(PEER, 'navigation', SIDE);
assert.strictEqual(context.lxmfActiveThread, SIDE);
assert(!context._conversationOwnerIsCurrent(mainOwner), 'switching conversation retires work started in Main');
assert(context._conversationOwnerIsCurrent(context._conversationOwnerSnapshot()));
context._activateConversation(PEER, 'navigation');
assert.strictEqual(context.lxmfActiveThread, SIDE, 'an unspecified thread keeps the open one for the same contact');
context._activateConversation('cc'.repeat(16), 'navigation');
assert.strictEqual(context.lxmfActiveThread, '', 'another contact opens in Main');
context._activateConversation(PEER, 'navigation', SIDE);

const activeWas = context._appendConversationMessage(PEER, { id: 'in-side', thread: SIDE });
const mainWas = context._appendConversationMessage(PEER, { id: 'in-main', thread: '' });
assert.strictEqual(activeWas, true);
assert.strictEqual(mainWas, false, 'a Main message never lands in the open side conversation');
assert.deepStrictEqual(JSON.parse(JSON.stringify(context.lxmfConversation.map((m) => m.id))), ['in-side']);
assert.deepStrictEqual(JSON.parse(JSON.stringify(context.cache[PEER].map((m) => m.id))), ['in-main']);
assert.deepStrictEqual(JSON.parse(JSON.stringify(context.cache[PEER + '~' + SIDE].map((m) => m.id))), ['in-side']);

// Header: nothing for one conversation; a chip naming the open one otherwise.
function element() {
    return {
        hidden: false, innerHTML: '', attrs: {},
        classList: { on: new Set(), toggle(c, on) { if (on) this.on.add(c); else this.on.delete(c); } },
        setAttribute(k, v) { this.attrs[k] = v; },
        querySelector() { return { textContent: '', addEventListener() {} }; },
        querySelectorAll() { return []; },
        appendChild() {},
        addEventListener() {},
    };
}
const nodes = {
    'lxmf-chat-header': element(), 'lxmf-thread-chip': element(),
    'lxmf-thread-tabs': element(), 'lxmf-thread-note': element(),
};
const ui = {
    lxmfActiveContact: PEER,
    lxmfActiveThread: SIDE,
    lxmfActiveThreads: { items: [{ thread: '', label: 'Main', unread: 0 }], next_number: 2 },
    document: {
        getElementById: (id) => nodes[id] || null,
        createElement: () => element(),
        addEventListener() {},
    },
    _conversationNameInfo: () => ({ name: 'Field agent' }),
    _canonicalConversationHash: (h) => String(h || '').toLowerCase(),
    formatConvTime: () => '2:14 PM',
};
ui.window = ui;
vm.createContext(ui);
vm.runInContext(threads, ui);
ui.RS.threads.render();
assert.strictEqual(nodes['lxmf-thread-chip'].hidden, true, 'one conversation shows no chip');
assert.strictEqual(nodes['lxmf-thread-tabs'].hidden, true, 'one conversation shows no tabs');
assert(!nodes['lxmf-chat-header'].classList.on.has('has-threads'));

ui.lxmfActiveThreads = { items: [
    { thread: '', label: 'Main', unread: 2 },
    { thread: SIDE, label: 'Trip plan', unread: 0, started_by: 'self', name: 'Trip plan' },
], next_number: 3 };
ui.RS.threads.render();
assert.strictEqual(nodes['lxmf-thread-chip'].hidden, false);
assert(nodes['lxmf-thread-chip'].innerHTML.includes('conv-thread-chip-dot'), 'unread elsewhere marks the chip');
assert(nodes['lxmf-thread-chip'].attrs['aria-label'].startsWith('Conversation: Trip plan'));
assert(nodes['lxmf-chat-header'].classList.on.has('has-threads'));

console.log('conversation threads: keys, ownership, isolation and header passed');
