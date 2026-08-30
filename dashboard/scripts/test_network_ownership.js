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
const events = {}, windowEvents = {}, calls = [];
let developer = false;
let data = { mode: 'managed', share: false, status: 'ready', configured: true,
    local_interfaces_allowed: true, credential_saved: false, unix_supported: false };
let externalClass = false;
const context = {
    document: { readyState: 'complete', getElementById: node,
        querySelectorAll: () => [...nodes.entries()].filter(([id]) => id.startsWith('network-owner-') && !['network-owner-notice', 'network-owner-interfaces'].includes(id)).map(([, value]) => value), createElement: () => node('remote-row'),
        body: { classList: { toggle(name, enabled) { externalClass = enabled; } } } },
    window: { ratspeakDeveloperModeEnabled: () => developer,
        addEventListener(name, fn) { windowEvents[name] = fn; } },
    rsConfirm: async () => true,
    RS: { listen(name, fn) { events[name] = fn; }, copyText: async () => true,
        async invoke(name, args) {
            calls.push(name);
            if (name === 'api_network_ownership') return data;
            if (name === 'test_shared_instance') throw new Error('RPC key rejected; unchanged');
            if (name === 'set_network_ownership') {
                assert.equal(field('key').value, '', 'clear the form before awaiting IPC');
                data = {...data, mode: args.args.mode, local_interfaces_allowed: false, credential_saved: true};
                return data;
            }
        } },
    localStorage: new Proxy({}, { get() { throw new Error('Credentials must not touch localStorage'); } }),
};
vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../static/js/network_ownership.js'), 'utf8'), context);
const settle = () => new Promise(resolve => setImmediate(resolve));
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
    console.log('Network ownership controller: all assertions passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
