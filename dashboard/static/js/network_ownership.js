// Ownership is backend state. Developer Mode only controls editing visibility.
(function() {
    'use strict';
    var current = null;
    var dirty = false;
    var busy = false;
    var epoch = 0;
    function el(id) { return document.getElementById('network-owner-' + id); }
    function status(message) { if (el('status')) el('status').textContent = message || ''; }
    function clearSecrets() {
        if (el('key')) el('key').value = '';
        if (el('import')) el('import').value = '';
    }
    function developer() { return !!(window.ratspeakDeveloperModeEnabled && window.ratspeakDeveloperModeEnabled()); }
    function gate() {
        var section = document.getElementById('network-ownership-settings');
        if (section) section.hidden = !developer();
        if (!developer()) clearSecrets();
    }
    function fields() {
        var existing = el('mode').value === 'existing';
        el('managed').hidden = existing;
        el('existing').hidden = !existing;
        var unix = el('carrier').value === 'unix';
        el('tcp').hidden = unix;
        el('unix').hidden = !unix;
        el('unix-option').disabled = busy || !current || !current.unix_supported;
        el('export').disabled = busy || !current || !current.share || current.mode !== 'managed' || current.status !== 'ready';
    }
    function setBusy(value) {
        busy = value;
        document.querySelectorAll('#network-ownership-settings input, #network-ownership-settings select, #network-ownership-settings textarea, #network-ownership-settings button').forEach(function(node) { node.disabled = value; });
        fields();
    }
    function applyEndpoint(endpoint) {
        endpoint = endpoint || { carrier: 'tcp', packet_port: 37428, control_port: 37429 };
        el('carrier').value = endpoint.carrier;
        el('packet').value = endpoint.packet_port || 37428;
        el('control').value = endpoint.control_port || 37429;
        el('name').value = endpoint.instance_name || 'default';
        fields();
    }
    function statusText(data) {
        if (data.error) return data.error;
        if (data.warnings && data.warnings.length) return data.warnings.map(function(warning) { return warning.interface + ': ' + warning.message; }).join('\n');
        var states = { ready: 'Ready', reconnecting: 'Waiting for the shared instance; reconnecting automatically',
            authentication_rejected: 'The shared instance rejected the RPC key. Update it in Settings → Network.',
            control_unavailable: 'The shared control service is unavailable. No local interfaces have been started.',
            stopped: 'Stopped', unavailable: 'Network unavailable', legacy_client: 'Using a legacy configured shared instance' };
        return (data.mode === 'existing' ? 'Existing local instance · ' : 'Managed by Ratspeak · ') + (states[data.status] || data.status || 'Checking');
    }
    function adopt(data, reset) {
        if (!data) return;
        current = Object.assign({}, current || {}, data);
        var external = current.local_interfaces_allowed === false;
        document.body.classList.toggle('network-external', external);
        var notice = el('notice');
        notice.hidden = !external && !current.error && !current.share && !(current.warnings && current.warnings.length);
        notice.textContent = statusText(current) + (external ? ' Interfaces are managed in the other app. Your saved Ratspeak interfaces are retained.' : '');
        var remote = el('interfaces');
        remote.hidden = !external;
        if (!external) remote.textContent = '';
        var transport = document.getElementById('transport-mode-select');
        if (transport) {
            transport.disabled = external;
            transport.title = external ? 'Transport is managed by the existing instance' : '';
        }
        if (!dirty || reset) {
            dirty = false;
            el('mode').value = current.mode || 'managed';
            el('share').checked = !!current.share;
            applyEndpoint(current.mode === 'existing' ? current.endpoint : null);
            el('key').placeholder = current.credential_saved ? 'Saved securely — leave blank to keep' : 'Key from the shared instance';
            el('unix-option').hidden = !current.unix_supported;
            el('unix-option').disabled = !current.unix_supported;
            status(statusText(current) + (!current.configured ? '. Existing configuration-file settings are preserved until you apply a choice.' : ''));
        }
        gate();
        fields();
    }
    function refresh(reset) {
        var requestEpoch = epoch;
        return RS.invoke('api_network_ownership').then(function(data) {
            if (requestEpoch === epoch) adopt(data, reset);
        }).catch(function(error) { status(error.message || 'Cannot read network ownership'); });
    }
    function request() {
        var existing = el('mode').value === 'existing';
        var endpoint = el('carrier').value === 'unix'
            ? { carrier: 'unix', instance_name: el('name').value.trim() }
            : { carrier: 'tcp', packet_port: Number(el('packet').value), control_port: Number(el('control').value) };
        return { mode: existing ? 'existing' : 'managed', share: !existing && el('share').checked,
            endpoint: existing ? endpoint : null, rpc_key: existing && el('key').value.trim() ? el('key').value.trim() : null };
    }
    async function test() {
        if (busy || !developer()) return;
        var args = request();
        var requestEpoch = epoch;
        setBusy(true);
        status('Checking packet service and authenticated RPC…');
        try {
            var result = await RS.invoke('test_shared_instance', { args: args });
            if (requestEpoch === epoch) status(result.message);
        } catch (error) { if (requestEpoch === epoch) status(error.message || String(error)); }
        finally { args.rpc_key = null; setBusy(false); }
    }
    async function apply() {
        if (busy || !developer()) return;
        var requestEpoch = epoch;
        var ok = await rsConfirm({ message: 'Apply this network choice? Current calls and transfers will be interrupted. Your identity, messages and saved interface settings are kept.', confirmText: 'Apply' });
        if (!ok || requestEpoch !== epoch || !developer()) return;
        var args = request();
        setBusy(true);
        clearSecrets();
        status('Changing network ownership…');
        try {
            var result = await RS.invoke('set_network_ownership', { args: args });
            if (requestEpoch === epoch) adopt(result, true);
        } catch (error) {
            if (requestEpoch === epoch) { await refresh(false); status(error.message || String(error)); }
        } finally { args.rpc_key = null; setBusy(false); }
    }
    async function importAccess() {
        if (busy || !developer()) return;
        var content = el('import').value;
        el('import').value = '';
        var requestEpoch = epoch;
        try {
            var result = await RS.invoke('import_shared_access', { content: content });
            if (requestEpoch !== epoch || !developer()) return;
            applyEndpoint(result.endpoint);
            el('key').value = result.rpc_key;
            result.rpc_key = null;
            dirty = true;
            status('Configuration loaded into the form. Test it, then apply when ready.');
        } catch (error) { status(error.message || String(error)); }
        finally { content = ''; }
    }
    async function exportAccess() {
        if (busy || !developer()) return;
        var requestEpoch = epoch;
        var ok = await rsConfirm({ message: 'This configuration includes a secret granting access to your local Reticulum control service. Copy it only to apps you trust. Clipboard history may retain it.', confirmText: 'Copy secret' });
        if (!ok || requestEpoch !== epoch || !developer()) return;
        var content = '';
        try {
            content = await RS.invoke('export_shared_access');
            if (requestEpoch !== epoch || !developer()) return;
            var copied = await RS.copyText(content);
            status(copied ? 'Access configuration copied. Clear your clipboard after use.' : 'Copy failed. The secret was not displayed.');
        } catch (error) { status(error.message || String(error)); }
        finally { content = ''; }
    }
    function remoteInterfaces(stats) {
        var container = el('interfaces');
        if (!current || current.local_interfaces_allowed !== false) return;
        container.textContent = '';
        var interfaces = stats && stats.interface_stats && stats.interface_stats.interfaces || [];
        interfaces.forEach(function(iface) {
            var row = document.createElement('div');
            row.textContent = (iface.name || 'Interface') + ' · ' + (iface.online ? 'Online' : 'Offline') + ' · Managed externally';
            container.appendChild(row);
        });
        if (!interfaces.length) container.textContent = 'Waiting for interface status from the shared instance.';
    }
    function init() {
        if (!el('mode')) return;
        document.getElementById('network-ownership-settings').addEventListener('input', function() { dirty = true; fields(); });
        el('mode').addEventListener('change', function() { dirty = true; clearSecrets(); fields(); });
        el('carrier').addEventListener('change', fields);
        el('test').addEventListener('click', test);
        el('apply').addEventListener('click', apply);
        el('reset').addEventListener('click', function() { clearSecrets(); dirty = false; refresh(true); });
        el('import-button').addEventListener('click', importAccess);
        el('export').addEventListener('click', exportAccess);
        window.addEventListener('ratspeak-developer-mode-changed', gate);
        window.addEventListener('pagehide', clearSecrets);
        RS.listen('network_ownership', function(data) { adopt(data, false); });
        RS.listen('stats_update', function(data) { if (data.network_ownership) adopt(data.network_ownership, false); remoteInterfaces(data); });
        RS.listen('identity_switching', function() { epoch += 1; dirty = false; clearSecrets(); current = null; });
        RS.listen('identity_switched', function() { refresh(true); });
        RS.listen('system_status', function() { refresh(false); });
        gate();
        refresh(true);
    }
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
    else init();
})();
