#!/usr/bin/env node
'use strict';

const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');
const root = path.join(__dirname, '..', '..', 'dashboard');
const source = fs.readFileSync(path.join(root, 'static/js/message_search.js'), 'utf8');

function element() {
    return {
        value: '', innerHTML: '', hidden: false, handlers: {}, blurred: 0, focused: 0,
        addEventListener(name, handler) { this.handlers[name] = handler; },
        blur() { this.blurred++; },
        focus() { this.focused++; }
    };
}
const input = element(), results = element(), conversations = element(), clear = element();
const nodes = { 'msg-search-input': input, 'msg-search-results': results, 'lxmf-conversations-list': conversations, 'msg-search-clear': clear };
const timers = new Map(), pending = [], opened = [];
let sequence = 0, identity = 0;
const context = {
    document: { getElementById: id => nodes[id] },
    setTimeout(fn) { timers.set(++sequence, fn); return sequence; },
    clearTimeout(id) { timers.delete(id); },
    escapeHtml: value => String(value).replace(/</g, '&lt;'),
    _conversationNameInfo: hash => ({ name: hash === 'cc' ? 'Cara Lane' : hash, isHash: hash !== 'cc' }),
    _canonicalConversationHash: hash => String(hash || '').toLowerCase(),
    _conversationPresence: () => ({ active: false, heard: null }),
    lxmfContacts: [{ hash: 'cc', display_name: 'Cara Lane' }],
    lxmfConversations: [],
    ratspeakDisplayNameHtml: name => name,
    openConversationWith: hash => opened.push(hash),
    openConversationAtMessage: (hash, id, ts, thread) => opened.push(hash + '#' + id + '@' + thread),
    RS: {
        config: { DEBOUNCE_MESSAGE_SEARCH: 300 },
        diag() {},
        kit: {
            avatar: () => '<span class="ui-avatar"></span>',
            hash: { short: h => h },
            time: { row: () => 'now', full: () => 'now' },
        },
        conversationOwner: {
            snapshot: () => ({ identity }),
            isIdentityCurrent: owner => owner.identity === identity
        },
        invoke(name, args) {
            assert.equal(name, 'api_search_messages');
            return new Promise((resolve, reject) => pending.push({ args, resolve, reject }));
        }
    }
};
vm.runInNewContext(source, context);
const lxmfSource = fs.readFileSync(path.join(root, 'static/js/lxmf.js'), 'utf8');
for (const name of ['_messageBodyContent', '_messageDisplayContent', '_messagePlainContent', '_messageIsMarkdown']) {
    const start = lxmfSource.indexOf('function ' + name + '(');
    vm.runInNewContext(lxmfSource.slice(start, lxmfSource.indexOf('\nfunction ', start + 1)), context);
}
function type(value) { input.value = value; input.handlers.input(); }
function fire() { const batch = [...timers.values()]; timers.clear(); batch.forEach(fn => fn()); }
async function settle() { for (let i = 0; i < 4; i++) await Promise.resolve(); }
function message(content) { return { source: 'aa', destination: 'bb', direction: 'inbound', timestamp: 1, content }; }

(async function() {
    type('first'); type('second');
    assert.equal(timers.size, 1, 'typing replaces the pending debounce');
    fire();
    assert.equal(pending[0].args.q, 'second');
    type('third'); fire();
    pending[1].resolve([message('new result')]); await settle();
    assert(results.innerHTML.includes('new result'));
    pending[0].resolve([message('old result')]); await settle();
    assert(!results.innerHTML.includes('old result'), 'older replies cannot replace the current results');

    type('fourth'); fire(); type('f');
    pending[2].reject(new Error('late error')); await settle();
    assert.equal(results.innerHTML, '', 'clearing must purge result content, including stale errors');
    assert.equal(results.hidden, true);
    assert.equal(conversations.hidden, false);

    type('first'); fire(); type('other'); type('first'); fire();
    pending[3].resolve([message('old ABA result')]); await settle();
    assert(!results.innerHTML.includes('old ABA result'), 'retyping the same query cannot revive its older request');
    pending[4].resolve([message('current ABA result')]); await settle();
    assert(results.innerHTML.includes('current ABA result'));

    type('important title'); fire();
    pending.pop().resolve([{ ...message(''), title: '<Important title>' }]); await settle();
    assert(results.innerHTML.includes('&lt;<b class="msg-hit">Important title</b>>'), 'title-only search results are visible, escaped and highlighted');

    type('private'); fire();
    identity++; context.RS.messageSearch.reset();
    pending[5].resolve([message('previous identity')]); await settle();
    assert.equal(input.value, ''); assert.equal(results.innerHTML, '');
    type('queued'); context.RS.messageSearch.reset(); fire();
    assert.equal(pending.length, 6, 'reset must retire a debounce before IPC dispatch');

    type('select'); fire(); pending[6].resolve([{ ...message('<script>'), id: 'm1', thread: 't1' }]); await settle();
    assert(results.innerHTML.includes('&lt;script>'), 'message previews must escape markup');
    assert(results.innerHTML.includes('aria-label="Messages"') && results.innerHTML.includes('<button type="button" class="ui-row"'),
        'results are kit rows that are real buttons');
    assert(!results.innerHTML.includes('aria-label="People"'), 'no People group when no name matches');
    const row = { getAttribute: name => ({ 'data-hash': 'aa', 'data-id': 'm1', 'data-thread': 't1' })[name] };
    results.handlers.click({ target: { closest: sel => sel.indexOf('data-search-action') !== -1 ? null : row } });
    assert.deepEqual(opened, ['aa#m1@t1'], 'a message result opens its conversation at that message, in its thread');
    assert.equal(input.value, ''); assert.equal(results.innerHTML, '');
    assert.equal(clear.hidden, true, 'the clear button goes with the text');

    type('cara'); fire(); pending[7].resolve([]); await settle();
    assert(results.innerHTML.includes('aria-label="People"'), 'a name match is grouped under People');
    assert(results.innerHTML.includes('<b class="msg-hit">Cara</b>'), 'the match is in bold');
    assert(!results.innerHTML.includes('aria-label="Messages"'));

    type('nothing'); fire(); pending[8].resolve([]); await settle();
    assert(results.innerHTML.includes('No messages match'), 'no results says so and offers to clear');
    results.handlers.click({ target: { closest: sel => sel.indexOf('data-search-action') !== -1 ? {} : null } });
    assert.equal(input.value, ''); assert(input.focused > 0, 'Clear search returns to the field');

    input.handlers.keydown({ key: 'Enter', isComposing: true });
    assert.equal(input.blurred, 0, 'IME confirmation must not dismiss the keyboard');
    type('escape'); input.handlers.keydown({ key: 'Escape', preventDefault() {} }); fire();
    assert.equal(clear.hidden, true);
    assert.equal(input.value, ''); assert.equal(timers.size, 0);

    const nav = fs.readFileSync(path.join(root, 'static/js/nav.js'), 'utf8');
    assert(nav.includes("['peers-search', 'contacts-search', 'msg-search-input']"),
        'navigation must clear all list searches through their input handlers');
    const lxmf = fs.readFileSync(path.join(root, 'static/js/lxmf.js'), 'utf8');
    assert(lxmf.includes('if (RS.messageSearch) RS.messageSearch.reset();'),
        'identity replacement must purge visible and pending message searches');
    console.log('message search: debounce, ordering, identity, navigation, escaping and keyboard passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
