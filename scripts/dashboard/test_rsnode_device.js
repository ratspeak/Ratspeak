// rsNode device conversations: command autocomplete, the status card rows,
// .rsota interception only for device contacts, and the update progress store.
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const root = path.resolve(__dirname, '..', '..');
const source = fs.readFileSync(path.join(root, 'dashboard/static/js/rsnode_device.js'), 'utf8');
const lxmf = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
const index = fs.readFileSync(path.join(root, 'dashboard/index.html'), 'utf8');

const NODE = 'aa'.repeat(16);
const PERSON = 'bb'.repeat(16);
const SENSOR = 'cc'.repeat(16);
const contacts = {
    [NODE]: { hash: NODE, kind: 'device', caps: ['rsnode', 'rsnode.update'], is_rsnode: true, can_update: true },
    [PERSON]: { hash: PERSON, kind: 'person', caps: [], is_rsnode: false, can_update: false },
    [SENSOR]: { hash: SENSOR, kind: 'device', caps: ['future'], is_rsnode: false, can_update: false },
};
const started = [];
const context = {
    window: {},
    lxmfActiveContact: NODE,
    lxmfConversation: [],
    _canonicalConversationHash: (h) => String(h || '').toLowerCase(),
    _lookupContactRecord: (h) => contacts[h] || null,
    _conversationNameInfo: () => ({ name: 'rsNode 1A2B' }),
    rsConfirm: () => ({ then(fn) { started.push('confirm'); return fn(false); } }),
    escapeHtml: (s) => String(s),
    document: { getElementById: () => null },
    console,
};
context.window = context;
context.RS = { invoke: () => Promise.resolve(null) };
vm.createContext(context);
vm.runInContext(source, context);
const rs = context.RS.rsnode;

// Autocomplete fills up to the first placeholder and never offers the exact text.
assert.strictEqual(rs._completion('/radio region <americas|europe>'), '/radio region ');
assert.strictEqual(rs._completion('/settings wifi join "<name>" ["<password>"]'), '/settings wifi join ');
assert.strictEqual(rs._completion('/status'), '/status');
const forms = (text) => JSON.parse(JSON.stringify(rs._suggestions(text).map((c) => c.form)));
assert.deepStrictEqual(forms('/st'), ['/status', '/stats', '/stamps', '/start', '/stop']);
assert.deepStrictEqual(forms('/announce '), ['/announce now'], 'the interval form is already filled in');
assert.deepStrictEqual(forms('/h'), ['/help', '/help <command>', '/health']);
assert.deepStrictEqual(forms('/STATUS'), [], 'an exact command has nothing left to complete');
assert.strictEqual(forms('/').length, 6, 'the list stays short');
assert.deepStrictEqual(forms('/radio r'), ['/radio region <americas|europe|uhf433|australia|asia>']);
assert.deepStrictEqual(forms('status'), [], 'only slash commands');
assert.deepStrictEqual(forms('/status\nmore'), []);
const offered = rs._commands.map((c) => c.form);
for (const command of ['/help', '/help <command>', '/status', '/stats', '/neighbors', '/settings', '/radio',
    '/managers', '/add-admin <address>', '/remove <address>', '/announce <off|10m|30m|60m|<N>h>', '/start', '/stop',
    '/reboot', '/nearby-only <on|off>', '/update', '/confirm <code>', '/ping [text]', '/echo <text>', '/signal',
    '/version', '/health', '/network', '/paths [n]', '/log', '/whoami', '/stamps', '/blink', '/announce now',
    '/reset-stats']) {
    assert(offered.includes(command), command + ' is offered');
}
assert.strictEqual(new Set(offered).size, offered.length, 'no command is listed twice');

// Every command sits in a group, and Owner-only ones say so.
const groups = [...new Set(rs._commands.map((c) => c.group))];
assert.deepStrictEqual(groups, ['Status', 'Diagnostics', 'Settings', 'Managers', 'Control']);
assert(rs._commands.every((c) => c.hint && c.group));
const ownerOnly = rs._commands.filter((c) => c.owner).map((c) => c.form);
for (const command of ['/reset-stats', '/announce now', '/announce <off|10m|30m|60m|<N>h>', '/start', '/stop',
    '/reboot', '/nearby-only <on|off>', '/add-admin <address>', '/remove <address>', '/settings wifi off',
    '/radio power <dBm>']) {
    assert(ownerOnly.includes(command), command + ' is Owner-only');
}
for (const command of ['/help', '/status', '/settings', '/radio', '/managers', '/ping [text]', '/signal',
    '/whoami', '/blink', '/update', '/confirm <code>']) {
    assert(!ownerOnly.includes(command), command + ' is for every manager');
}
const reset = rs._commands.find((c) => c.form === '/reset-stats');
assert.strictEqual(rs._hintText(reset), 'Zero the counters · Owner only');
assert.strictEqual(rs._hintText(rs._commands.find((c) => c.form === '/signal')), 'How your message arrived');
const all = rs._allHtml();
assert.strictEqual(all.split('rsnode-suggest-group').length - 1, 5, 'one heading per group');
assert.strictEqual(all.split('data-rsnode-fill=').length - 1, rs._commands.length, 'every command is listed');
assert(all.indexOf('>Diagnostics<') < all.indexOf('/ping [text]') && all.indexOf('/ping [text]') < all.indexOf('>Settings<'));
assert.deepStrictEqual(JSON.parse(JSON.stringify(rs._quick)), ['/status', '/stats', '/signal', '/ping', '/help']);
assert(rs._quick.every((q) => offered.some((f) => f === q || f.startsWith(q + ' ['))), 'quick chips are real commands');

// The status card reads the newest /status and /stats results.
const messages = [
    { timestamp: 1, results: { schema: 1, command: 'status', release: '1.0.0', running: false } },
    { timestamp: 2, results: null },
    { timestamp: 3, results: { schema: 1, command: 'status', release: '1.0.1', running: true, uptime_s: 3720,
        region: 2, radio: { frequency_hz: 869525000, bandwidth_hz: 250000, sf: 9, cr: 5, power_dbm: 14 },
        limits: 'off', announce_interval_s: 1800, uplink: 'up', peers: 1 } },
    { timestamp: 4, results: { schema: 1, command: 'stats', uptime_s: 3800, uplink: 'connecting', peers: 2,
        packets: { rx: 120, tx: 33 }, airtime_percent: 4 } },
];
const status = rs._newestResults(messages, 'status');
const stats = rs._newestResults(messages, 'stats');
assert.strictEqual(status.timestamp, 3);
assert.strictEqual(rs._summary(status.results), 'Running · 1.0.1');
assert.strictEqual(rs._summary(null), 'No status yet');
const rows = Object.fromEntries(JSON.parse(JSON.stringify(rs._statusRows(status.results, stats.results))));
assert.deepStrictEqual(rows, {
    Release: '1.0.1',
    Forwarding: 'Running',
    Uptime: '1 h 2 min',
    Region: 'Europe 868',
    Radio: '869.525 MHz · 250 kHz · SF9 · CR 4/5 · 14 dBm',
    Limits: 'Off',
    Announces: 'Every 30 min',
    Uplink: 'Connecting',
    Peers: '2',
    Packets: '120 received · 33 sent',
    Airtime: '4% of the hourly budget',
});
const unset = Object.fromEntries(rs._statusRows({ region: null, announce_interval_s: 0 }, null));
assert.strictEqual(unset.Region, 'Not set');
assert.strictEqual(unset.Announces, 'Off');

// Only a .rsota for a device contact starts an update; everything else attaches.
assert(rs._isRsota('heltec-v4-1.0.1.RSOTA'));
assert(!rs._isRsota('notes.rsota.txt'));
assert.strictEqual(rs.interceptFile({ name: 'fw.rsota', size: 10 }, PERSON), false);
assert.strictEqual(rs.interceptFile({ name: 'fw.rsota', size: 10 }, SENSOR), false, 'devices that are not rsNodes attach');
assert.strictEqual(rs.interceptFile({ name: 'photo.png', size: 10 }, NODE), false);
assert.strictEqual(rs.interceptFile({ name: 'fw.rsota', size: 10 }, NODE), true);
assert.deepStrictEqual(started, ['confirm'], 'the update asks first');
assert(rs.isDevice(NODE) && !rs.isDevice(PERSON));

// Progress is kept per node until an outcome arrives.
rs._onProgress({ contact: NODE.toUpperCase(), stage: 'sending', sent: 50, total: 100, release: '1.0.1' });
assert.strictEqual(rs._updates[NODE].stage, 'sending');
rs._onProgress({ contact: NODE, stage: 'installed', sent: 100, total: 100, release: '1.0.1' });
assert.strictEqual(rs._updates[NODE], undefined);

// Wiring: every attach path goes through handleFileSelected, which asks rsNode first.
const handle = lxmf.slice(lxmf.indexOf('function handleFileSelected('));
assert(handle.indexOf('RS.rsnode.interceptFile(file, lxmfActiveContact)') < handle.indexOf('maxSize'));
assert(lxmf.includes("handleFileSelected({ files: [files[0]], value: '' });"), 'desktop drop');
assert(lxmf.includes("fileInput.addEventListener('change', function() { handleFileSelected(this); });"), 'picker');
assert(lxmf.includes('if (RS.rsnode) RS.rsnode.render();'));
assert(lxmf.includes("if (_messageIsPaper(msg) || _messageIsNote(msg)) return;"), 'notes never alert');
assert(lxmf.includes("is_rsnode: c.kind === 'device' && caps.indexOf('rsnode') !== -1"));
assert(lxmf.includes("can_update: c.kind === 'device' && (caps.indexOf('rsnode.update') !== -1 || caps.indexOf('rsnode') !== -1)"));
assert(index.indexOf('rsnode_device.js') > index.indexOf('lxmf.js'), 'loads after lxmf.js');
for (const id of ['lxmf-rsnode-badge', 'rsnode-panel', 'rsnode-commands', 'rsnode-suggest']) {
    assert(index.includes('id="' + id + '"'), id);
}
console.log('rsnode device ok');
