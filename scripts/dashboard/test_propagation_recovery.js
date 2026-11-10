'use strict';
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');
const source = fs.readFileSync(path.join(__dirname, '../../dashboard/static/js/propagation.js'), 'utf8');
const calls = [], toasts = [], handlers = {};
let confirm = false;
const ctx = {
    window: {},
    document: { getElementById: id => id === 'prop-retry-deferred-btn' ? {
        addEventListener: (event, fn) => { handlers.retry = fn; }
    } : null },
    escapeHtml: value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])),
    activeIdentity: () => ({ is_hardware: false }),
    rsConfirm: async () => confirm,
    rsPrompt: async args => args.placeholder.includes('64') ? 'a'.repeat(64) : '/tmp/approved.json',
    showToast: (...args) => toasts.push(args),
    RS: { invoke: async (command, args) => { calls.push({command,args}); return {quarantined_files:2,quarantined_bytes:24}; } },
    setTimeout: () => 0,
};
vm.createContext(ctx);
vm.runInContext(source.slice(0, source.indexOf("RS.listen(")), ctx);
(async () => {
    ctx.propagationStatus = {};
    assert.equal(ctx.renderDeferredDownloads(), '');
    ctx.propagationStatus = {suppressed_count:2,deferred_count:1,admission_paused:true,suppression_full:true,next_retry:1800000000};
    const deferred = ctx.renderDeferredDownloads();
    assert(deferred.includes('2 messages could not be accepted'));
    assert(deferred.includes('1 message will be retried'));
    assert(deferred.includes('retry list is full'));
    assert(deferred.includes('Next retry:'));
    ctx.propagationStatus = {hosting_requested:true,hosting_enabled:false,hosting_status:{identity:'<profile>',readiness:'migration_required',reason:'<unverified>',quarantined_files:2,quarantined_bytes:24}};
    const blocked = ctx.renderHostingSettings();
    assert(blocked.includes('id="prop-host-toggle" checked'));
    assert(blocked.includes('Hosting unavailable'));
    assert(blocked.includes('&lt;unverified&gt;') && !blocked.includes('<profile>'));
    assert(blocked.includes('2 archived files preserved (24 bytes)'));
    assert(!blocked.includes('id="prop-host-announce-btn"'));
    await ctx.recoverHostedInbox(false);
    assert.equal(calls.length,0,'declining writer exclusion must not start migration');
    confirm = true;
    await ctx.recoverHostedInbox(true);
    const recovery = calls.find(c => c.command === 'recover_propagation_host');
    assert(recovery);
    assert.equal(recovery.args.args.identity,'<profile>');
    assert.equal(recovery.args.args.manifest_path,'/tmp/approved.json');
    assert.equal(recovery.args.args.approved_manifest_sha256,'a'.repeat(64));
    assert.equal(recovery.args.args.supervisor_writers_stopped,true);
    ctx.wireUpHandlers({querySelectorAll:()=>[]},'manual');
    await handlers.retry();
    assert(calls.some(c => c.command === 'retry_deferred_propagation'));
    assert(calls.some(c => c.command === 'sync_propagation'));
    ctx.activeIdentity = () => ({is_hardware:true});
    assert(!ctx.renderHostingSettings().includes('id="prop-host-toggle"'));
    ctx.discoveredRelayNodes = Array.from({length: 31}, (_, i) => ({hash: String(i).padStart(32, '0'), display_name: i ? 'Inbox ' + i : '<a very long inbox name>', hops: i}));
    const list = ctx.renderDiscoveredList('manual', true, null);
    assert(list.includes('class="relay-node-name" title="&lt;a very long inbox name&gt;"'), 'a truncated name stays readable on hover');
    assert(list.includes('Showing 30 of 31 inbox nodes'));
    // Offline Inbox card state, first match wins.
    ctx.formatLastHeard = function(ts) { return 't' + ts; };
    ctx.inboxCheckRequestedAt = 0;
    var now = 1000000;
    var node = function(extra) { return Object.assign({ last_seen: 990, path_status: 'reachable' }, extra); };
    var state = function(status) { return ctx.inboxCardState(Object.assign({ network_available: true }, status), now * 1000); };
    assert.strictEqual(state({ network_available: false, client_state: 'Receiving' }).dot, 'status-offline');
    assert(state({ client_state: 'ListRequested', active_node: node({}) }).line1.includes('Checking now'));
    var receiving = state({ client_state: 'Receiving', transfer_progress: 0.4, active_node: node({}) });
    assert(receiving.line1.includes('Receiving messages') && receiving.line1.includes('40%') && receiving.busy);
    assert(state({ client_state: 'Idle' }).line1.includes('Not heard on the network yet'));
    assert(state({ active_node: { last_seen: 0 } }).line1.includes('Not heard on the network yet'));
    assert(state({ active_node: node({ last_failure_reason: 'sync_partial', last_success: 900 }) }).line1.includes('Last check incomplete'));
    var failing = state({ active_node: node({ last_failure_reason: 'link establishment timeout', last_success: 900 }) });
    assert(failing.line1.includes('Not working</span> · Couldn&#39;t connect') && failing.line2.includes('Last worked'));
    assert.strictEqual(state({ active_node: node({ last_failure_reason: 'something new' }) }).line1.indexOf('·'), -1, 'unknown reasons stay quiet');
    var working = state({ active_node: node({ last_sync_success: now - 60, last_deposit_success: now - 120 }) });
    assert(working.line1.includes('Working') && working.line2.includes('Stored your last message') && working.dot === 'status-reachable');
    assert(state({ active_node: node({ last_sync_success: now - 901 }) }).line1.includes('Last checked'));
    assert(state({ active_node: node({}) }).line1.includes('Not checked yet'));
    ctx.inboxCheckRequestedAt = now * 1000 - 1000;
    assert(state({ active_node: node({}) }).busy, 'Check Now shows Checking right away');
    // Live updates refresh only the card, so typing and open sections survive.
    ctx.inboxCheckRequestedAt = 0;
    const els = { 'inbox-status-dot': { className: '' }, 'inbox-status-lines': { innerHTML: '' }, 'prop-sync-btn': { disabled: false } };
    let fullRenders = 0;
    ctx.document = { getElementById: id => els[id] || null, querySelector: () => null };
    ctx.renderPropagationStatus = () => { fullRenders++; };
    ctx.propagationStatus = { mode: 'manual', network_available: true, client_state: 'Idle', active_node: { last_seen: 1 } };
    ctx.lastInboxRenderKey = ctx.inboxRenderKey(ctx.propagationStatus);
    ctx.propagationStatus = Object.assign({}, ctx.propagationStatus, { client_state: 'ListRequested' });
    ctx.updateInboxStatus();
    assert.equal(fullRenders, 0, 'a check in progress touches only the card');
    assert(els['inbox-status-lines'].innerHTML.includes('Checking now'));
    assert(els['inbox-status-dot'].className.includes('status-checking') && els['prop-sync-btn'].disabled);
    ctx.propagationStatus = Object.assign({}, ctx.propagationStatus, { mode: 'auto' });
    ctx.updateInboxStatus();
    assert.equal(fullRenders, 1, 'a mode change redraws the panel');
    console.log('propagation recovery: 42 assertions passed');
})().catch(error => {console.error(error);process.exitCode=1;});
