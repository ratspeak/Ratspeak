#!/usr/bin/env node
'use strict';

const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');
const read = name => fs.readFileSync(path.join(__dirname, '..', '..', 'dashboard', name), 'utf8');
function fn(source, name) {
    const start = source.indexOf('function ' + name + '(');
    assert(start >= 0, name);
    let depth = 0;
    for (let i = source.indexOf('{', start); i < source.length; i++) {
        if (source[i] === '{') depth++;
        if (source[i] === '}' && --depth === 0) return source.slice(start, i + 1);
    }
    throw new Error('unterminated function ' + name);
}
const state = read('static/js/state.js');
const lxmf = read('static/js/lxmf.js');
const health = read('static/js/health.js');
const shared = read('static/js/ui_shared.js');
const html = read('index.html');

// Classic scripts share one global scope. Check the actual loaded first-party
// files so a later Settings controller cannot silently replace another owner.
const owners = new Map();
for (const match of html.matchAll(/<script src="\/(static\/js\/(?!vendor\/)[^"?]+\.js)/g)) {
    for (const declaration of read(match[1]).matchAll(/^function (\w+)\(/gm)) {
        assert(!owners.has(declaration[1]), declaration[1] + ' is declared by both ' + owners.get(declaration[1]) + ' and ' + match[1]);
        owners.set(declaration[1], match[1]);
    }
}

// Connection rows open the kit menu, which picks a sheet or a popover by layout itself.
const networkView = read('static/js/network_view.js');
assert(networkView.includes("case 'c-menu': {") && networkView.includes('RS.kit.menu(el, { title: rowName(c), items: menuItems(c)'),
    'a connection row menu is the kit menu');
assert(!health.includes('function buildIfaceActionItems(') && !health.includes('function showInterfaceActionSheet('),
    'the old interface action sheet is gone');
const context = { window: {} };
vm.createContext(context);
// New message is one sheet: a contact's name or a pasted LXMF address.
assert(lxmf.includes("title: 'New message',") && lxmf.includes('placeholder="Name or LXMF address"'),
    'New message is one sheet that takes a name or an address');
assert(!html.includes('fab-contact-picker'), 'the separate contact picker is retired');
vm.runInContext('var LXMF_ADDRESS_HEX = /^[0-9a-f]{32}$/;\n' + fn(lxmf, '_newMessageAddress'), context);
assert.equal(context._newMessageAddress(' 4F9A2C71 D03B8E56 A1C4F709 2BE1E0B3 '), '4f9a2c71d03b8e56a1c4f7092be1e0b3');
assert.equal(context._newMessageAddress('lxma://4f9a2c71d03b8e56a1c4f7092be1e0b3'), '4f9a2c71d03b8e56a1c4f7092be1e0b3');
assert.equal(context._newMessageAddress('4f9a2c71d03b8e56'), '', 'a short hash is not an address');
// People panes use the kit search field: glyph, input, and clear in one control.
for (const id of ['peers-search', 'contacts-search']) {
    const field = html.match(new RegExp('<div class="ui-search" role="search">(?:(?!</div>).)*id="' + id + '"(?:(?!</div>).)*</div>'));
    assert(field, id + ' must sit in the kit search field');
    assert(field[0].includes('aria-label="Search ') && field[0].includes('aria-label="Clear search"'), id + ' needs a specific accessible name and a clear button');
}
const messageSearch = html.match(/<div class="ui-search" role="search">[^]*?<input[^>]*id="msg-search-input"[^>]*>/);
assert(messageSearch && messageSearch[0].includes('aria-label="Search messages"'), 'Messages search is the kit field');

// The phone compose button: New message straight away, or with Paper messages on, a menu above a scrim.
function element() {
    const classes = new Set();
    return {
        handlers: {}, attributes: {}, innerHTML: '',
        classList: { add: c => classes.add(c), remove: c => classes.delete(c), contains: c => classes.has(c), toggle: (c, on) => on ? classes.add(c) : classes.delete(c) },
        addEventListener(name, cb) { this.handlers[name] = cb; },
        setAttribute(name, value) { this.attributes[name] = value; }
    };
}
const fab = element(), wrap = element(), sheets = [], menus = [];
let menuOptions = null;
context.RS = {
    gestures: { bindViewFabClick: (el, cb) => el.addEventListener('click', cb) },
    kit: { icon: name => '<' + name + '>', menu: (trigger, opts) => { menus.push(opts.items.map(i => i.label)); menuOptions = opts; return {}; }, closeMenu() {} }
};
context.document = { documentElement: { dataset: {} }, getElementById: id => ({ 'lxmf-send-fab': fab, 'lxmf-fab-dial': wrap })[id] };
context.openNewMessageSheet = () => sheets.push('sheet');
context.openPaperDecoder = () => {};
vm.runInContext(fn(lxmf, '_composeItems') + '\n' + fn(lxmf, '_paperMessagesOn') + '\n' + fn(lxmf, 'initComposeFab'), context);
context.initComposeFab();
fab.handlers.click({ stopPropagation() {} });
assert.deepEqual([sheets.length, menus.length], [1, 0], 'without Paper messages the button opens New message');
context.document.documentElement.dataset.paperMessages = 'on';
fab.handlers.click({ stopPropagation() {} });
assert.deepEqual(menus.at(-1), ['New message', 'Open a paper message']);
assert.equal(menuOptions.sheet, false, 'the phone menu opens above the button, not in a sheet');
assert(wrap.classList.contains('is-open') && fab.attributes['aria-label'] === 'Close', 'the open menu turns the button into Close');
menuOptions.onClose();
assert(!wrap.classList.contains('is-open') && fab.attributes['aria-label'] === 'New message');

// Send by: Paper is last, below a separator, disabled with an empty composer.
assert(lxmf.includes("{ separator: true },\n            { label: 'Paper', icon: 'qr', hint: 'A QR code or link you hand over yourself.', disabled: !hasText, onSelect: sendLxmfPaperMessage },"));
assert(lxmf.includes("var hasText = !!(input && input.value.trim());"));

// Shared keyboard activation must not hijack nested controls or composing text.
const binding = shared.slice(shared.indexOf('    RS.ui.bindKeyboardActivation ='), shared.indexOf('    var transportLabels'));
context.RS.ui = {};
vm.runInContext(binding, context);
const control = element(); let clicks = 0;
control.click = () => clicks++;
context.RS.ui.bindKeyboardActivation(control);
for (const key of ['Enter', ' ']) control.handlers.keydown({ key, target: control, preventDefault() {} });
assert.equal(clicks, 2);
control.handlers.keydown({ key: 'Enter', target: {}, preventDefault() { throw new Error('nested control intercepted'); } });
control.handlers.keydown({ key: 'Enter', target: control, isComposing: true });
assert.equal(clicks, 2);

// Execute the actual clipboard helper with platform success/failure callbacks.
const copySource = state.slice(state.indexOf('window.RS.copyText ='), state.indexOf('var _rsAndroidMediaPermissionSeq'));
async function clipboard(execResult, fallbackResult) {
    const children = [], copied = [], restored = [];
    const previous = { isConnected: true, selectionStart: 2, selectionEnd: 5, selectionDirection: 'backward',
        focus() { document.activeElement = this; }, setSelectionRange(...args) { restored.push(args); } };
    const selection = { rangeCount: 1, getRangeAt: () => ({ cloneRange: () => ({ saved: true }) }), removeAllRanges() {}, addRange() {} };
    const document = {
        activeElement: previous, getSelection: () => selection,
        body: { appendChild(el) { children.push(el); el.parentNode = this; }, removeChild(el) { children.splice(children.indexOf(el), 1); document.activeElement = this; } },
        createElement() { return { style: {}, setAttribute() {}, select() { document.activeElement = this; }, setSelectionRange() {} }; },
        execCommand() { if (execResult instanceof Error) throw execResult; return execResult; }
    };
    const copyContext = { document, window: { RS: {} }, navigator: { clipboard: { writeText(text) { copied.push(text); return fallbackResult ? Promise.resolve() : Promise.reject(new Error('denied')); } } } };
    vm.runInNewContext(copySource, copyContext);
    const ok = await copyContext.window.RS.copyText('synthetic clipboard text');
    assert.equal(children.length, 0, 'clipboard success and failure must both remove the temporary textarea');
    assert.strictEqual(document.activeElement, previous, 'clipboard must restore the original input focus');
    assert.deepEqual(restored, [[2, 5, 'backward']], 'clipboard must preserve the selected input range');
    assert.equal(copied.length, execResult === true ? 0 : 1);
    return ok;
}
(async function() {
    assert(await clipboard(true, false));
    assert(await clipboard(false, true));
    assert(await clipboard(new Error('WebView copy failed'), true));
    assert.equal(await clipboard(new Error('WebView copy failed'), false), false);
    console.log('client controls: layout dispatch, New message, compose button, keyboard and clipboard passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
