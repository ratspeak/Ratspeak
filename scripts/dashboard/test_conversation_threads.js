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

// Chips: nothing for one conversation; one chip row on every size otherwise.
function element(tag) {
    const el = {
        tag, hidden: false, attrs: {}, children: [], handlers: {}, text: '', html: '',
        classList: { on: new Set(), toggle(c, on) { if (on) this.on.add(c); else this.on.delete(c); } },
        setAttribute(k, v) { this.attrs[k] = v; },
        getAttribute(k) { return this.attrs[k]; },
        removeAttribute(k) { delete this.attrs[k]; },
        appendChild(child) { this.children.push(child); return child; },
        addEventListener(name, cb) { this.handlers[name] = cb; },
        querySelector(selector) {
            if (selector === 'span') return { set textContent(v) { el.text = v; } };
            if (selector === 'button') return element('button');
            return null;
        },
        querySelectorAll() { return []; },
        contains() { return false; },
        get firstChild() { return this.children[0] || null; },
        get lastChild() { return this.children[this.children.length - 1] || null; },
        set innerHTML(v) {
            this.html = v;
            this.children = [];
            if (/^<div class="ui-chips/.test(v)) this.children.push(element('div'), element('button'));
            if (/^<span><\/span> <button/.test(v)) this.children.push(element('span'), element('button'));
        },
        get innerHTML() { return this.html; },
    };
    return el;
}
const nodes = { 'lxmf-thread-tabs': element('div'), 'lxmf-thread-note': element('p') };
const ui = {
    lxmfActiveContact: PEER,
    lxmfActiveThread: SIDE,
    lxmfActiveThreads: { items: [{ thread: '', label: 'Main', unread: 0 }], next_number: 2 },
    document: {
        activeElement: null,
        getElementById: (id) => nodes[id] || null,
        createElement: (tag) => element(tag),
        addEventListener() {},
    },
    _conversationNameInfo: () => ({ name: 'Field agent', isHash: false }),
    _canonicalConversationHash: (h) => String(h || '').toLowerCase(),
};
ui.window = ui;
vm.createContext(ui);
vm.runInContext('var RS = { kit: { icon: (n) => "<" + n + ">" }, gestures: { attachLongPress() {} } };', ui);
vm.runInContext(threads, ui);
ui.RS.threads.render();
assert.strictEqual(nodes['lxmf-thread-tabs'].hidden, true, 'one conversation shows no chips');
assert.strictEqual(nodes['lxmf-thread-note'].hidden, true);

ui.lxmfActiveThreads = { items: [
    { thread: '', label: 'Main', unread: 2 },
    { thread: SIDE, label: 'Trip plan', unread: 0, started_by: 'self', name: 'Trip plan' },
], next_number: 3 };
ui.RS.threads.render();
const row = nodes['lxmf-thread-tabs'];
assert.strictEqual(row.hidden, false, 'several conversations show one chip row');
const chips = row.firstChild.children;
assert.strictEqual(row.firstChild.attrs['aria-label'], 'Conversations with Field agent');
assert.strictEqual(row.lastChild.attrs['aria-label'], 'New conversation with Field agent');
assert.deepStrictEqual(chips.map((c) => [c.text, c.attrs['aria-checked']]), [['Main', 'false'], ['Trip plan', 'true']]);
assert(chips[1].innerHTML.startsWith('<check>'), 'the chosen chip carries the check');
assert.strictEqual(chips[0].children[0].textContent, '2', 'another conversation shows its unread count');
assert(!chips[1].children.length, 'the open conversation has no count');
assert.strictEqual(nodes['lxmf-thread-note'].hidden, true);

ui.lxmfActiveThreads.items[1] = { thread: SIDE, label: 'Conversation 2', unread: 0, started_by: 'contact' };
ui.RS.threads.render();
assert.strictEqual(nodes['lxmf-thread-note'].hidden, false, 'a conversation they started says so once');
assert.strictEqual(nodes['lxmf-thread-note'].children[0].textContent, 'Field started this conversation.');

console.log('conversation threads: keys, ownership, isolation and chips passed');
