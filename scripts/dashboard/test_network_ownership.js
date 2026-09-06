// Dependency-free controller regression: no real credentials, IPC or storage.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const nodes = new Map();
function node(id) {
    if (!nodes.has(id)) nodes.set(id, { value: '', hidden: false, disabled: false, checked: false,
        textContent: '', events: {}, children: [],
        addEventListener(name, fn) { this.events[name] = fn; },
        appendChild(child) { this.children.push(child); } });
    return nodes.get(id);
}
const field = id => node('network-owner-' + id);
const events = {}, windowEvents = {}, documentEvents = {}, calls = [];
let importReply = null;
let applyReply = null;
let refreshReply = null;
let confirmationReply = true;
const appliedRequests = [];
let developer = false;
let data = { mode: 'managed', share: false, status: 'ready', configured: true,
    local_interfaces_allowed: true, credential_saved: false, unix_supported: false };
let externalClass = false;
const context = {
    document: { readyState: 'complete', getElementById: node,
        hidden: false, addEventListener(name, fn) { documentEvents[name] = fn; },
        querySelectorAll: () => [...nodes.entries()].filter(([id]) => id.startsWith('network-owner-') && !['network-owner-notice', 'network-owner-interfaces'].includes(id)).map(([, value]) => value), createElement: () => node('remote-row'),
        body: { classList: { toggle(name, enabled) { externalClass = enabled; } } } },
    window: { ratspeakDeveloperModeEnabled: () => developer,
        addEventListener(name, fn) { windowEvents[name] = fn; } },
    rsConfirm: async () => confirmationReply,
    RS: { listen(name, fn) { events[name] = fn; }, copyText: async () => true,
        async invoke(name, args) {
            calls.push(name);
            if (name === 'api_network_ownership') return refreshReply || data;
            if (name === 'import_shared_access') return importReply;
            if (name === 'test_shared_instance') throw new Error('RPC key rejected; unchanged');
            if (name === 'set_network_ownership') {
                assert.equal(field('key').value, '', 'clear the form before awaiting IPC');
                appliedRequests.push(JSON.parse(JSON.stringify(args.args)));
                data = {...data, mode: args.args.mode, endpoint: args.args.endpoint,
                    local_interfaces_allowed: args.args.mode !== 'existing', credential_saved: args.args.mode === 'existing'};
                return applyReply || data;
            }
        } },
    localStorage: new Proxy({}, { get() { throw new Error('Credentials must not touch localStorage'); } }),
};
vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../dashboard/static/js/network_ownership.js'), 'utf8'), context);
const settle = () => new Promise(resolve => setImmediate(resolve));
function deferred() {
    let resolve, reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}
function beginImport() {
    const reply = deferred();
    importReply = reply.promise;
    field('import').value = 'synthetic access configuration';
    return { ...reply, done: field('import-button').events.click() };
}
function access(port, key) {
    return { endpoint: { carrier: 'tcp', packet_port: port, control_port: port + 1 }, rpc_key: key };
}
(async () => {
    await settle();
    assert.equal(node('network-ownership-settings').hidden, true);
    developer = true; windowEvents['ratspeak-developer-mode-changed']();
    assert.equal(node('network-ownership-settings').hidden, false);
    field('mode').value = 'existing'; field('mode').events.change();
    field('key').value = '1234';
    await field('test').events.click();
    assert.match(field('status').textContent, /rejected/);
    assert.equal(data.mode, 'managed');
    assert.equal(field('unix-option').disabled, true, 'busy completion cannot enable unsupported Unix');
    events.stats_update({network_ownership: data});
    assert.equal(field('mode').value, 'existing', 'poll must not overwrite a dirty form');
    await field('apply').events.click();
    assert.equal(data.mode, 'existing');
    assert.equal(externalClass, true);
    assert.equal(node('transport-mode-select').disabled, true);
    assert.equal(field('key').value, '');
    field('key').value = 'abcd'; field('import').value = 'secret';
    developer = false; windowEvents['ratspeak-developer-mode-changed']();
    assert.equal(field('key').value, ''); assert.equal(field('import').value, '');
    assert.equal(data.mode, 'existing'); assert.equal(externalClass, true);
    assert.equal(field('notice').hidden, false, 'ownership remains visible outside Developer Mode');
    const before = calls.length;
    await field('apply').events.click(); await field('test').events.click();
    assert.equal(calls.length, before, 'hidden advanced controls cannot invoke mutations');
    events.identity_switching();
    assert.equal(field('key').value, '');
    developer = true; windowEvents['ratspeak-developer-mode-changed']();
    field('key').value = 'abcd'; field('import').value = 'secret';
    context.document.hidden = true;
    if (documentEvents.visibilitychange) documentEvents.visibilitychange();
    assert.equal(field('key').value, '', 'backgrounding clears the secret fields');
    assert.equal(field('import').value, '');
    context.document.hidden = false;
    for (const hide of ['developer', 'pagehide', 'visibility']) {
        let resolveImport;
        importReply = new Promise(resolve => { resolveImport = resolve; });
        field('import').value = 'disposable access object';
        const importing = field('import-button').events.click();
        if (hide === 'developer') {
            developer = false; windowEvents['ratspeak-developer-mode-changed']();
            developer = true; windowEvents['ratspeak-developer-mode-changed']();
        } else if (hide === 'pagehide') windowEvents.pagehide();
        else {
            context.document.hidden = true; documentEvents.visibilitychange();
            context.document.hidden = false;
        }
        resolveImport({ endpoint: { carrier: 'tcp', packet_port: 37428, control_port: 37429 }, rpc_key: '1234' });
        await importing;
        assert.equal(field('key').value, '', hide + ' must invalidate pending imports, even after reopening');
    }

    const failures = [];
    async function regression(name, check) {
        try { await check(); }
        catch (error) { failures.push(new Error(name + ': ' + error.message)); }
    }
    for (const editedField of ['packet', 'control', 'name', 'carrier', 'key', 'import']) {
        for (const outcome of ['success', 'error']) {
            await regression('manual ' + editedField + ' edit fences pending import ' + outcome, async () => {
                field('reset').events.click(); await settle();
                field('mode').value = 'existing'; field('mode').events.change();
                const pending = beginImport();
                field(editedField).value = editedField === 'carrier' ? 'unix' : 'manually-edited-value';
                node('network-ownership-settings').events.input();
                const expected = ['packet', 'control', 'name', 'carrier', 'key', 'import']
                    .map(name => field(name).value);
                const expectedStatus = field('status').textContent;
                if (outcome === 'success') pending.resolve(access(40000, 'obsolete-key'));
                else pending.reject(new Error('obsolete import failure'));
                await pending.done;
                assert.deepEqual(['packet', 'control', 'name', 'carrier', 'key', 'import']
                    .map(name => field(name).value), expected, 'manual values remain authoritative');
                assert.equal(field('status').textContent, expectedStatus, 'obsolete import cannot overwrite status');
            });
        }
    }
    await regression('editing the form invalidates an outstanding Apply confirmation', async () => {
        field('reset').events.click(); await settle();
        field('mode').value = 'existing'; field('mode').events.change();
        const confirmation = deferred();
        confirmationReply = confirmation.promise;
        const before = appliedRequests.length;
        const applying = field('apply').events.click();
        field('packet').value = 44000;
        node('network-ownership-settings').events.input();
        confirmation.resolve(true);
        await applying;
        confirmationReply = true;
        assert.equal(appliedRequests.length, before, 'confirmation may not approve a replacement form');
        assert.equal(field('packet').value, 44000);
    });
    for (const action of ['mode', 'discard', 'apply']) {
        for (const outcome of ['success', 'error']) {
            await regression(action + ' fences a pending import ' + outcome, async () => {
                field('reset').events.click(); await settle();
                field('mode').value = 'existing'; field('mode').events.change();
                const pending = beginImport();
                if (action === 'mode') {
                    field('mode').value = 'managed'; field('mode').events.change();
                    assert.equal(field('existing').hidden, true);
                } else if (action === 'discard') {
                    field('reset').events.click(); await settle();
                } else {
                    field('key').value = 'aabb';
                    const beforeApply = appliedRequests.length;
                    await field('apply').events.click();
                    assert.equal(appliedRequests.length, beforeApply + 1, 'confirmed Apply still commits');
                    assert.equal(appliedRequests.at(-1).rpc_key, 'aabb', 'Apply keeps the explicitly submitted key');
                }
                const expectedPacket = field('packet').value;
                const expectedStatus = field('status').textContent;
                if (outcome === 'success') pending.resolve(access(40000, 'discarded-key'));
                else pending.reject(new Error('obsolete import failure'));
                await pending.done;
                assert.equal(field('key').value, '', 'obsolete import cannot repopulate the key');
                assert.equal(field('packet').value, expectedPacket, 'obsolete import cannot replace the endpoint');
                assert.equal(field('status').textContent, expectedStatus, 'obsolete import cannot replace current status');
            });
        }
    }
    for (const firstToSettle of ['older', 'newer']) {
        for (const oldOutcome of ['success', 'error']) {
            await regression('overlapping imports: ' + firstToSettle + ' first, old ' + oldOutcome, async () => {
                field('reset').events.click(); await settle();
                field('mode').value = 'existing'; field('mode').events.change();
                const older = beginImport();
                const newer = beginImport();
                async function settleOlder() {
                    if (oldOutcome === 'success') older.resolve(access(40000, 'older-key'));
                    else older.reject(new Error('older import failed'));
                    await older.done;
                }
                if (firstToSettle === 'older') {
                    const expectedStatus = field('status').textContent;
                    await settleOlder();
                    assert.equal(field('key').value, '', 'older result is already obsolete while newer import waits');
                    assert.equal(field('status').textContent, expectedStatus);
                }
                newer.resolve(access(41000, 'newer-key'));
                await newer.done;
                const expectedStatus = field('status').textContent;
                if (firstToSettle === 'newer') await settleOlder();
                assert.equal(field('key').value, 'newer-key');
                assert.equal(field('packet').value, 41000);
                assert.equal(field('status').textContent, expectedStatus);
            });
        }
    }
    await regression('a failed newest import still supersedes an older success', async () => {
        field('reset').events.click(); await settle();
        field('mode').value = 'existing'; field('mode').events.change();
        const older = beginImport();
        const newer = beginImport();
        newer.reject(new Error('newest configuration is invalid'));
        await newer.done;
        assert.equal(field('status').textContent, 'newest configuration is invalid');
        older.resolve(access(40000, 'obsolete-key'));
        await older.done;
        assert.equal(field('key').value, '');
        assert.equal(field('status').textContent, 'newest configuration is invalid');
    });
    await regression('a pending Apply remains authoritative while its backend reply waits', async () => {
        field('reset').events.click(); await settle();
        field('mode').value = 'existing'; field('mode').events.change();
        field('packet').value = 43000; field('control').value = 43001;
        field('key').value = 'aabb';
        const pending = beginImport();
        const reply = deferred();
        applyReply = reply.promise;
        const applying = field('apply').events.click();
        await settle();
        const expectedStatus = field('status').textContent;
        pending.resolve(access(40000, 'obsolete-key'));
        await pending.done;
        const duringApply = { key: field('key').value, status: field('status').textContent };
        reply.resolve(data);
        await applying;
        applyReply = null;
        assert.equal(duringApply.key, '');
        assert.equal(duringApply.status, expectedStatus);
        assert.equal(field('packet').value, 43000, 'the confirmed backend selection is adopted');
        assert.match(field('status').textContent, /Ready/);
    });
    await regression('an obsolete refresh error cannot replace current form status', async () => {
        const reply = deferred();
        refreshReply = reply.promise;
        field('reset').events.click();
        field('mode').value = 'managed'; field('mode').events.change();
        const expectedStatus = field('status').textContent;
        reply.reject(new Error('obsolete refresh failure'));
        await settle();
        refreshReply = null;
        assert.equal(field('status').textContent, expectedStatus);
    });
    await regression('hiding during failed Apply recovery suppresses its late error', async () => {
        field('reset').events.click(); await settle();
        const failedApply = deferred();
        const recovery = deferred();
        applyReply = failedApply.promise;
        refreshReply = recovery.promise;
        const applying = field('apply').events.click();
        await settle();
        failedApply.reject(new Error('obsolete Apply failure'));
        await settle();
        context.document.hidden = true; documentEvents.visibilitychange();
        const expectedStatus = field('status').textContent;
        recovery.resolve(data);
        await applying;
        context.document.hidden = false;
        applyReply = null; refreshReply = null;
        assert.equal(field('status').textContent, expectedStatus);
    });
    await regression('cancelling Apply preserves the pending import', async () => {
        field('reset').events.click(); await settle();
        field('mode').value = 'existing'; field('mode').events.change();
        const pending = beginImport();
        const beforeApply = appliedRequests.length;
        confirmationReply = false;
        await field('apply').events.click();
        confirmationReply = true;
        assert.equal(appliedRequests.length, beforeApply);
        pending.resolve(access(42000, 'retained-key'));
        await pending.done;
        assert.equal(field('key').value, 'retained-key');
        assert.equal(field('packet').value, 42000);
    });
    if (failures.length) throw new AggregateError(failures, failures.length + ' import-ordering regressions');
    console.log('Network ownership controller: all assertions passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
