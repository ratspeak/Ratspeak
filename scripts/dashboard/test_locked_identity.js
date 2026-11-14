#!/usr/bin/env node
'use strict';

// A locked identity's data stays out of the page: the bootstrap waits for the
// unlock, an auto-lock starts the page over, a refusal while locked raises no
// toast, and Auto-Lock is offered for any identity with a PIN.

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const js = path.join(__dirname, '..', '..', 'dashboard', 'static', 'js');
const read = name => fs.readFileSync(path.join(js, name), 'utf8');
const events = read('tauri_events.js');
const identity = read('identity.js');
const constants = read('constants.js');

function block(source, opening) {
    const start = source.indexOf(opening);
    assert.notEqual(start, -1, opening + ' must exist');
    let depth = 0;
    for (let index = source.indexOf('{', start); index < source.length; index++) {
        if (source[index] === '{') depth++;
        if (source[index] === '}' && --depth === 0) return source.slice(start, index + 1);
    }
    throw new Error('unterminated ' + opening);
}

const settle = () => new Promise(resolve => setImmediate(resolve));

async function boot(stage) {
    const calls = [];
    const record = name => function() { calls.push(name); return Promise.resolve(); };
    const context = {
        window: {},
        document: { getElementById: () => null },
        announceCache: null,
        lxmfActiveContact: 'cd'.repeat(16),
        RS: {
            invoke(name) {
                calls.push(name);
                return name === 'api_startup_progress'
                    ? Promise.resolve({ stage: stage, hw_locked: 'ab'.repeat(16), hw_locked_kind: 'passcode' })
                    : Promise.resolve([]);
            }
        },
        showHwUnlock(hash, kind) { calls.push('showHwUnlock:' + kind); },
        loadIdentities: record('loadIdentities'),
        loadConversations: record('loadConversations'),
        updateBlockedCount: record('updateBlockedCount'),
        PeersCache: { init: record('PeersCache.init') }
    };
    vm.runInNewContext([
        block(events, 'function _rsBootstrapOnLoad('),
        block(events, 'function _rsLoadIdentityViews('),
        '_rsBootstrapOnLoad();'
    ].join('\n'), context, { filename: 'tauri_events.js' });
    await settle();
    return calls;
}

async function toastsFor(code) {
    const toasts = [];
    const context = {
        RS: { invoke: () => Promise.reject(Object.assign(new Error('Unlock this identity first.'), { code })) },
        showToast(message) { toasts.push(message); }
    };
    vm.runInNewContext(block(constants, 'RS.invokeOrToast = function(') + ';',
        context, { filename: 'constants.js' });
    await context.RS.invokeOrToast('add_contact', {}, 'Could not add contact');
    return toasts;
}

(async () => {
    const identityReads = ['api_announces', 'loadConversations', 'updateBlockedCount',
        'PeersCache.init', 'get_conversation'];

    const locked = await boot('hw_locked');
    assert(locked.includes('showHwUnlock:passcode'), 'the PIN sheet');
    assert(locked.includes('loadIdentities'), 'identities stay manageable');
    for (const read of identityReads) {
        assert(!locked.includes(read), read + ' waits for the unlock');
    }

    const ready = await boot('ready');
    assert(!ready.some(call => call.startsWith('showHwUnlock')));
    for (const read of identityReads) assert(ready.includes(read), read + ' loads when unlocked');

    // The lock event: an auto-lock reloads, a lock at startup shows the sheet.
    const seen = { reloads: 0, sheets: [] };
    const listeners = {};
    vm.runInNewContext(
        identity.slice(identity.lastIndexOf("if (typeof RS !== 'undefined' && RS.listen) {")),
        {
            RS: { listen(name, fn) { listeners[name] = fn; } },
            window: { location: { reload() { seen.reloads++; } } },
            showHwUnlock(hash, kind) { seen.sheets.push(kind); }
        },
        { filename: 'identity.js' }
    );
    listeners.hardware_locked({ hash: 'ab'.repeat(16), kind: 'passcode', reason: 'secret_required' });
    assert.equal(seen.reloads, 0);
    assert.deepEqual(seen.sheets, ['passcode']);
    listeners.hardware_locked({ hash: 'ab'.repeat(16), kind: 'passcode', reason: 'timeout' });
    assert.equal(seen.reloads, 1, 'an auto-lock starts the page over');
    assert.deepEqual(seen.sheets, ['passcode']);

    assert.deepEqual(await toastsFor('locked'), [], 'the PIN sheet answers a locked identity');
    assert.equal((await toastsFor('conflict')).length, 1);


    console.log('Locked identity tests passed');
})().catch(error => {
    console.error(error);
    process.exit(1);
});
