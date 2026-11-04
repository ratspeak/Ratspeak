#!/usr/bin/env node
'use strict';
// A pasted file attaches as before; on Linux desktop a paste with no file and
// no text reads the image natively, since WebKitGTK hands it over as nothing.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const root = path.join(__dirname, '../..');
const source = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
const state = fs.readFileSync(path.join(root, 'dashboard/static/js/state.js'), 'utf8');
const start = source.indexOf("(function() {\n    var chatArea = document.getElementById('lxmf-chat-area');");
const end = source.indexOf('function openChatHeaderDropdown');
assert(start > 0 && end > start, 'paste block must stay extractable');
const rawBytesStart = state.indexOf('function _rsRawIpcBytes(');
const rawBytes = state.slice(rawBytesStart, state.indexOf('\n}\n', rawBytesStart) + 2);

function harness(opts) {
    let listener = null;
    const calls = [], selected = [], toasts = [];
    const context = {
        Array, ArrayBuffer, Date, Promise, Uint8Array,
        lxmfActiveContact: 'peer',
        navigator: { userAgent: opts.userAgent },
        isTauriDesktop: () => !!opts.desktop,
        showPreConditionToast: text => toasts.push(text),
        showToast: (text, kind) => toasts.push([text, kind]),
        handleFileSelected: input => selected.push(input.files[0]),
        File: function(parts, name, options) { this.parts = parts; this.name = name; this.type = options.type; },
        RS: {
            invoke: (cmd) => {
                calls.push(cmd);
                return opts.native instanceof Error ? Promise.reject(opts.native) : Promise.resolve(opts.native);
            },
        },
        document: { getElementById: () => ({ addEventListener: (type, fn) => { listener = fn; } }) },
    };
    vm.createContext(context);
    vm.runInContext(rawBytes + '\n' + source.slice(start, end), context);
    // Like WebKitGTK: no types listed, text only through getData.
    const paste = (text, files) => {
        let prevented = false;
        listener({
            clipboardData: {
                types: [],
                items: (files || []).map(f => ({ kind: 'file', getAsFile: () => f })),
                getData: type => (type === 'text/plain' ? text : ''),
            },
            preventDefault() { prevented = true; },
        });
        return prevented;
    };
    return { calls, selected, toasts, paste };
}

const settle = () => new Promise(r => setImmediate(r));
const LINUX = 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15';
const MAC = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15';
const ANDROID = 'Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36';

(async function() {
    let h = harness({ desktop: true, userAgent: MAC });
    assert.equal(h.paste('', [{ name: 'image.png', type: 'image/png' }]), true);
    assert.match(h.selected[0].name, /^paste-\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}\.png$/);
    assert.equal(h.paste('', []), false, 'only WebKitGTK needs the native read');
    assert.deepEqual(h.calls, []);

    h = harness({ desktop: false, userAgent: ANDROID });
    assert.equal(h.paste('', []), false);
    assert.deepEqual(h.calls, []);

    h = harness({ desktop: true, userAgent: LINUX, native: new Uint8Array([0x89, 0x50]).buffer });
    assert.equal(h.paste('hello', []), false, 'text pastes are left to the textarea');
    assert.equal(h.paste('', [{ name: 'shot.png', type: 'image/png' }]), true);
    assert.equal(h.selected[0].name, 'shot.png', 'a file WebKitGTK does hand over attaches directly');
    assert.deepEqual(h.calls, []);
    assert.equal(h.paste('', []), true);
    await settle();
    assert.deepEqual(h.calls, ['read_clipboard_image']);
    assert.equal(h.selected.length, 2);
    assert.match(h.selected[1].name, /^paste-.*\.png$/);
    assert.equal(h.selected[1].type, 'image/png');
    assert.equal(h.selected[1].parts[0].parts[0].byteLength, 2, 'the native PNG bytes are what attach');

    h = harness({ desktop: true, userAgent: LINUX, native: new ArrayBuffer(0) });
    h.paste('', []);
    await settle();
    assert.deepEqual(h.selected, [], 'an empty clipboard attaches nothing');
    assert.deepEqual(h.toasts, []);

    h = harness({ desktop: true, userAgent: LINUX, native: new Error('gone') });
    h.paste('', []);
    await settle();
    assert.deepEqual(h.toasts, [['Could not paste the image', 'toast-error']]);

    console.log('Clipboard image paste tests passed.');
})().catch(err => { console.error(err); process.exit(1); });
