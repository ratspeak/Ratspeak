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
        value: '', innerHTML: '', style: {}, handlers: {}, rows: [], blurred: 0,
        addEventListener(name, handler) { this.handlers[name] = handler; },
        querySelectorAll() { return this.rows; },
        blur() { this.blurred++; }
    };
}
const input = element(), results = element(), conversations = element();
const nodes = { 'msg-search-input': input, 'msg-search-results': results, 'lxmf-conversations-list': conversations };
const timers = new Map(), pending = [], opened = [];
let sequence = 0, identity = 0;
const context = {
    document: { getElementById: id => nodes[id] },
    setTimeout(fn) { timers.set(++sequence, fn); return sequence; },
    clearTimeout(id) { timers.delete(id); },
    escapeHtml: value => String(value).replace(/</g, '&lt;'),
    _conversationNameInfo: hash => ({ name: hash, isHash: true }),
    ratspeakDisplayNameHtml: name => name,
    formatConvTime: () => 'now',
    openConversationWith: hash => opened.push(hash),
    RS: {
        config: { DEBOUNCE_MESSAGE_SEARCH: 300 },
        diag() {}, ui: { bindKeyboardActivation(row) { row.keyboard = true; } },
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
const displayStart = lxmfSource.indexOf('function _messageDisplayContent(');
vm.runInNewContext(lxmfSource.slice(displayStart, lxmfSource.indexOf('\nfunction ', displayStart + 1)), context);
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
    assert.equal(results.style.display, 'none');
    assert.equal(conversations.style.display, '');

    type('first'); fire(); type('other'); type('first'); fire();
    pending[3].resolve([message('old ABA result')]); await settle();
    assert(!results.innerHTML.includes('old ABA result'), 'retyping the same query cannot revive its older request');
    pending[4].resolve([message('current ABA result')]); await settle();
    assert(results.innerHTML.includes('current ABA result'));

    type('important title'); fire();
    pending.pop().resolve([{ ...message(''), title: '<Important title>' }]); await settle();
    assert(results.innerHTML.includes('&lt;Important title>'), 'title-only search results are visible and escaped');

    type('private'); fire();
    identity++; context.RS.messageSearch.reset();
    pending[5].resolve([message('previous identity')]); await settle();
    assert.equal(input.value, ''); assert.equal(results.innerHTML, '');
    type('queued'); context.RS.messageSearch.reset(); fire();
    assert.equal(pending.length, 6, 'reset must retire a debounce before IPC dispatch');

    const row = element(); row.getAttribute = () => 'aa'; results.rows = [row];
    type('select'); fire(); pending[6].resolve([message('<script>')]); await settle();
    assert(results.innerHTML.includes('&lt;script>'), 'message previews must escape markup');
    assert(results.innerHTML.includes('class="conv-row" role="button" tabindex="0"'));
    assert(row.keyboard, 'search rows must use the shared keyboard activation helper');
    row.handlers.click();
    assert.deepEqual(opened, ['aa']); assert.equal(input.value, ''); assert.equal(results.innerHTML, '');
    row.handlers.click(); assert.equal(opened.length, 1, 'a detached retired result cannot navigate again');

    input.handlers.keydown({ key: 'Enter', isComposing: true });
    assert.equal(input.blurred, 0, 'IME confirmation must not dismiss the keyboard');
    type('escape'); input.handlers.keydown({ key: 'Escape', preventDefault() {} }); fire();
    assert.equal(input.value, ''); assert.equal(timers.size, 0);

    const nav = fs.readFileSync(path.join(root, 'static/js/nav.js'), 'utf8');
    assert(nav.includes("['peers-search', 'contacts-search', 'msg-search-input']"),
        'navigation must clear all list searches through their input handlers');
    const lxmf = fs.readFileSync(path.join(root, 'static/js/lxmf.js'), 'utf8');
    assert(lxmf.includes('if (RS.messageSearch) RS.messageSearch.reset();'),
        'identity replacement must purge visible and pending message searches');
    console.log('message search: debounce, ordering, identity, navigation, escaping and keyboard passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
