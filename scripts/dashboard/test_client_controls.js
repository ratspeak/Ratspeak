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
const contact = read('static/js/contact_card.js');
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

// Exercise real layout dispatch for small pointer windows and wide touch tablets.
const handlers = {}, actions = [];
const context = {
    window: { innerWidth: 390, RS: { config: { MOBILE_BREAKPOINT: 768, MOBILE_TOUCH_BREAKPOINT: 1024 } } },
    navigator: { maxTouchPoints: 0 },
    document: { querySelector: () => ({ addEventListener: (name, cb) => { handlers[name] = cb; } }) },
    buildIfaceActionItems: () => [],
    actionPopover: () => actions.push('popover'),
    showInterfaceActionSheet: () => actions.push('sheet')
};
vm.createContext(context);
vm.runInContext(['isCompactLayout', 'isTouchDevice', 'isMobile'].map(name => fn(state, name)).join('\n') +
    '\n' + fn(contact, 'isMobileContactFlow') + '\n' + health.slice(health.lastIndexOf('(function() {')), context);
const row = { dataset: { ifaceType: 'tcp', ifaceName: 'Test' } };
for (const [width, touch, expected] of [[390, 0, 'sheet'], [768, 1, 'sheet'], [769, 1, 'popover'], [1024, 1, 'popover'], [1366, 0, 'popover']]) {
    context.window.innerWidth = width; context.navigator.maxTouchPoints = touch;
    handlers.click({ target: { closest: () => row } });
    assert.equal(actions.at(-1), expected, 'interface actions at ' + width + 'px');
    assert.equal(context.isMobileContactFlow(), width <= 768, 'contact actions must follow the same layout');
}
const picker = html.match(/<div class="[^"]+" id="fab-contact-picker-sheet"[^>]*>/)[0];
assert(!picker.includes('--mobile-only'), 'desktop Contacts must open a visible picker');
assert(picker.includes('bottom-sheet rs-dialog-sheet'), 'the shared picker must use the canonical shell');
assert(html.includes('class="bottom-sheet-body fab-contact-picker-list"'), 'picker content must own its scroll region');
assert(!read('static/css/13-responsive.css').includes('.fab-picker-row {'), 'contact rows cannot be styled only on phones');
for (const id of ['peers-search', 'msg-search-input', 'contacts-search']) {
    const input = html.match(new RegExp('<input[^>]*id="' + id + '"[^>]*>'))[0];
    assert(input.includes('nr-input-sm list-search-input'), id + ' must use the shared form primitive');
    assert(input.includes('aria-label="Search '), id + ' needs a specific accessible name');
}

// A FAB can become visible after startup/resizing, even with no touch input.
function element() {
    const classes = new Set();
    return {
        handlers: {}, attributes: {},
        classList: { add: c => classes.add(c), remove: c => classes.delete(c), contains: c => classes.has(c) },
        addEventListener(name, cb) { this.handlers[name] = cb; },
        setAttribute(name, value) { this.attributes[name] = value; }
    };
}
const fab = element(), dial = element(), scrim = element();
context.RS = { gestures: { bindViewFabClick: (el, cb) => el.addEventListener('click', cb) } };
context.window.addEventListener = (name, cb) => { handlers['window-' + name] = cb; };
context.document = {
    getElementById: id => ({ 'lxmf-send-fab': fab, 'fab-dial-actions': dial })[id],
    createElement: () => scrim, body: { appendChild() {} }
};
vm.runInContext(fn(lxmf, 'initFabSpeedDial'), context);
context.initFabSpeedDial();
assert.equal(typeof fab.handlers.click, 'function', 'wide startup must still wire the responsive FAB');
context.window.innerWidth = 390;
fab.handlers.click({ stopPropagation() {} });
assert(dial.classList.contains('open'));
context.window.innerWidth = 1024; handlers['window-resize']();
assert(!dial.classList.contains('open'), 'widening must retire the now-hidden dial and its scrim');

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
    console.log('client controls: layout dispatch, shared picker, resize, keyboard and clipboard passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
