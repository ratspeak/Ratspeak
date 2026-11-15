// Private network: the Reticulum network identity (Settings › Network › Private network) and the
// host discovery switches that use it (Host setup and a running Host's detail).
(function() {
    'use strict';

    var state = null;
    var listeners = [];

    function K() { return RS.kit; }
    function esc(v) { return escapeHtml(v == null ? '' : String(v)); }
    function toast(msg, tone, ms) { if (typeof showToast === 'function') showToast(msg, tone || 'toast-success', ms || 3000); }
    function fail(err, fallback) { toast((err && err.message) || fallback, 'toast-error', 8000); }

    function adopt(s) {
        state = { exists: !!(s && s.exists), hash: (s && s.hash) || null };
        render();
        listeners.slice().forEach(function(fn) { try { fn(state); } catch (_) {} });
        return state;
    }
    function refresh() {
        return RS.invoke('api_network_identity').then(adopt, function() { return state; });
    }

    // Each change restarts a running network, like the other settings under the Network notice.
    function change(command, args, done) {
        var ns = RS.networkSettings;
        var run = function() { return RS.invoke(command, args || {}).then(adopt); };
        if (ns && typeof ns.commitRestart === 'function') {
            return ns.commitRestart({ row: 'settings-private-network', run: run, done: done }).then(function(result) {
                return result ? state : null;
            });
        }
        return run().then(function(s) { toast(done); return s; }, function(err) { fail(err, 'The private network could not change'); return null; });
    }

    function create() { return change('create_network_identity', {}, 'Private network created'); }

    function bytesToBase64(bytes) {
        var s = '';
        for (var i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
        return btoa(s);
    }
    function importFile() {
        return new Promise(function(resolve) {
            var input = document.createElement('input');
            input.type = 'file';
            input.hidden = true;
            input.addEventListener('change', function() {
                var file = input.files && input.files[0];
                input.remove();
                if (!file) return resolve(null);
                file.arrayBuffer().then(function(buf) {
                    return change('import_network_identity', { args: { data_base64: bytesToBase64(new Uint8Array(buf)) } }, 'Private network imported');
                }).then(resolve, function(err) { fail(err, 'That file couldn’t be read'); resolve(null); });
            });
            document.body.appendChild(input);
            input.click();
        });
    }
    function importQr() {
        if (!RS.qr || typeof RS.qr.openScanner !== 'function') {
            toast('QR scanning isn’t available in this build', 'toast-warning');
            return;
        }
        RS.qr.openScanner({
            title: 'Scan a private network',
            scanningText: 'Point the camera at a private network QR.',
            invalidText: 'That QR isn’t a private network.',
            invalidPasteText: 'That isn’t a private network.',
            invalidImageText: 'That image doesn’t hold a private network QR.',
            emptyImageText: 'No QR code found in that image.',
            preview: function(text) { return RS.invoke('import_network_identity', { args: { text: String(text || '').trim() } }); },
            onPreview: function(_body, _payload, s, closeAll) {
                closeAll();
                adopt(s);
                toast('Private network imported');
            }
        });
    }
    function chooseImport(trigger) {
        K().menu(trigger, {
            title: 'Import a private network',
            items: [
                { label: 'From a file', icon: 'file', onSelect: importFile },
                { label: 'Scan a QR code', icon: 'qr', onSelect: importQr }
            ]
        });
    }

    function base64ToBytes(b64) {
        var raw = atob(b64 || ''), out = new Uint8Array(raw.length);
        for (var i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
        return out;
    }
    // Same pattern as the contact card: the QR plate, the ID, and what sharing it reveals.
    function share() {
        return RS.invoke('export_network_identity').then(function(data) {
            var phone = K().compact();
            var built = K().dialog({
                title: 'Share private network',
                className: 'netid-share-dialog',
                body: '<div class="ui-qr"><canvas role="img" aria-label="QR code for your private network"></canvas></div>' +
                    '<p class="ui-help">Scan it in Ratspeak to join this private network.</p>' +
                    '<div class="ui-field"><span class="ui-label">Network ID</span>' + K().hash.copy(data.hash, { noun: 'Network ID' }) + '</div>' +
                    '<div class="ui-notice ui-notice--warning">' + K().icon('alert') + '<div class="ui-notice-body">Anyone with it can find your private connections and their passphrases. Share it only with your group.</div></div>',
                foot: '<button type="button" class="ui-btn" data-netid="save-file">' + K().icon(phone ? 'file' : 'download') + (phone ? 'Share file' : 'Save file') + '</button>' +
                    '<button type="button" class="ui-btn ui-btn--primary" data-netid="save-qr">' + K().icon(phone ? 'qr' : 'image') + (phone ? 'Share QR code' : 'Save image') + '</button>'
            });
            var canvas = built.q('canvas');
            if (canvas && RS.qr && typeof RS.qr.renderCanvas === 'function') RS.qr.renderCanvas(canvas, data.qr_text);
            built.foot.addEventListener('click', function(e) {
                var b = e.target.closest('[data-netid]');
                if (!b) return;
                var job = b.getAttribute('data-netid') === 'save-file'
                    ? saveBytesToUserFile(base64ToBytes(data.data_base64), data.file_name, 'application/octet-stream', data.data_base64)
                    : RS.qr.shareCanvas(canvas, data.file_name + '.png', 'Ratspeak private network');
                Promise.resolve(job).catch(function(err) {
                    if (err && err.name === 'AbortError') return;
                    fail(err, 'Couldn’t save it');
                });
            });
            return built;
        }).catch(function(err) { fail(err, 'Couldn’t read the private network'); });
    }

    function remove() {
        var built = K().dialog({
            title: 'Remove private network?',
            size: 'sm',
            body: '<p>Your private hosts stop showing in discovery, and you won’t see your group’s private connections. Connections you’ve joined keep working.</p>',
            foot: '<button type="button" class="ui-btn ui-btn--quiet ui-desk-only" data-netid="cancel">Cancel</button>' +
                '<button type="button" class="ui-btn ui-btn--danger ui-btn--primary" data-netid="remove">Remove</button>'
        });
        built.foot.addEventListener('click', function(e) {
            var b = e.target.closest('[data-netid]');
            if (!b) return;
            built.close();
            if (b.getAttribute('data-netid') === 'remove') change('remove_network_identity', {}, 'Private network removed');
        });
    }

    // ------------------------------------------------------------------ Settings › Network › Private network
    function sectionHtml() {
        var foot = '<p class="ui-group-foot">Only people with it can find your private connections.</p>';
        if (!state || !state.exists) {
            return '<div class="ui-group"><div class="ui-prop settings-prop-wrap"><span class="ui-prop-text"><span class="ui-prop-label">Not set up</span></span>' +
                '<div class="settings-btns"><button type="button" class="ui-btn ui-btn--sm" data-netid="create">Create</button>' +
                '<button type="button" class="ui-btn ui-btn--sm" data-netid="import" aria-haspopup="menu" aria-expanded="false">Import</button></div></div></div>' + foot;
        }
        return '<div class="ui-group"><div class="ui-prop"><span class="ui-prop-text"><span class="ui-prop-label">Network ID</span>' + K().hash.copy(state.hash, { noun: 'Network ID' }) + '</span></div>' +
                '<button type="button" class="ui-prop" data-netid="share">' + K().icon('qr', 'lead') + '<span class="ui-prop-text"><span class="ui-prop-label">Share</span></span>' + K().icon('chevron-right') + '</button></div>' + foot +
            '<div class="ui-group"><button type="button" class="ui-prop ui-prop--danger" data-netid="remove">' + K().icon('trash', 'lead') + '<span class="ui-prop-text"><span class="ui-prop-label">Remove private network</span></span></button></div>';
    }
    function render() {
        var el = document.getElementById('settings-private-network-body');
        if (el && RS.kit) el.innerHTML = sectionHtml();
    }
    function onSectionClick(e) {
        var b = e.target.closest('[data-netid]');
        if (!b) return;
        var act = b.getAttribute('data-netid');
        if (act === 'create') create();
        else if (act === 'import') chooseImport(b);
        else if (act === 'share') share();
        else if (act === 'remove') remove();
    }

    // ------------------------------------------------------------------ host discovery switches
    // o: { ifac, disc, share, priv }. Share access needs a network name and passphrase; Private network
    // needs a network identity, offered inline when there is none.
    function hprop(pre, key, label, desc, on, disabled) {
        var id = pre + key + '-label';
        return '<div class="ui-prop"><span class="ui-prop-text"><span class="ui-prop-label" id="' + id + '">' + label + '</span><span class="ui-prop-desc">' + desc + '</span></span>' +
            '<label class="ui-switch"><input type="checkbox" role="switch" data-hd="' + key + '"' + (on ? ' checked' : '') + (disabled ? ' disabled' : '') + ' aria-labelledby="' + id + '"><span class="track"></span></label></div>';
    }
    function hostDiscoveryHtml(o, pre) {
        var rows = [hprop(pre, 'disc', 'Show in discovery', 'People with Discover servers on can find it.', o.disc)];
        if (o.disc) {
            rows.push('<div class="ui-prop settings-prop-stack"><div class="ui-field"><label class="ui-label" for="' + pre + 'reachable">Address others use</label>' +
                '<input class="ui-input ui-input--mono" id="' + pre + 'reachable" data-hd="reachable" type="text" inputmode="url" placeholder="rns.example.org" value="' + esc(o.reachable || '') + '" autocomplete="off" autocapitalize="none" spellcheck="false">' +
                '<p class="ui-help">The address or IP that Discover servers lists.</p></div></div>');
            rows.push(hprop(pre, 'share', 'Share access in discovery', o.ifac ? 'Includes the passphrase, so people join without typing it.' : 'Set a network name and passphrase first.', o.share && o.ifac, !o.ifac));
            rows.push(hprop(pre, 'priv', 'Private network', 'Only people with your private network can find it.', o.priv));
            if (o.priv && !(state && state.exists)) {
                rows.push('<div class="ui-prop settings-prop-stack"><div class="ui-notice ui-notice--info">' + K().icon('lock') + '<div class="ui-notice-body">You don’t have a private network yet. Create one, or import your group’s.</div></div>' +
                    '<div class="settings-btns"><button type="button" class="ui-btn ui-btn--sm" data-netid="create">Create</button>' +
                    '<button type="button" class="ui-btn ui-btn--sm" data-netid="import" aria-haspopup="menu" aria-expanded="false">Import</button></div></div>');
            }
        }
        return '<div class="ui-group">' + rows.join('') + '</div>' +
            (o.disc && o.share && o.ifac && !o.priv ? '<div class="ui-notice ui-notice--warning">' + K().icon('alert') + '<div class="ui-notice-body">Anyone who finds it can read the passphrase. Turn on Private network to limit it to your group.</div></div>' : '');
    }
    // Reads a host's saved discovery flags from its config record.
    function hostDiscoveryState(iface) {
        var on = function(k) { return typeof _ifaceBool === 'function' ? _ifaceBool(iface || {}, k, false) : false; };
        var ifac = typeof _ifaceHasIfac === 'function' ? _ifaceHasIfac(iface || {}) : !!(iface && (iface.network_name || iface.passphrase));
        return { ifac: ifac, disc: on('discoverable'), share: on('publish_ifac'), priv: on('discovery_encrypt'), reachable: (iface && iface.reachable_on) || '' };
    }
    // The add and update server commands take these as they are.
    function hostDiscoveryRead(root) {
        var q = function(k) { return root.querySelector('[data-hd="' + k + '"]'); };
        var v = function(k) { var el = q(k); return !!(el && el.checked && !el.disabled); };
        var reach = q('reachable');
        return { discoverable: v('disc'), reachable_on: reach ? reach.value.trim() : '', publish_ifac: v('share'), discovery_encrypt: v('priv') };
    }
    // Clicks on the inline Create/Import inside a host's discovery group.
    function handleInline(e) {
        var b = e.target.closest('[data-netid]');
        if (!b) return false;
        var act = b.getAttribute('data-netid');
        if (act === 'create') create();
        else if (act === 'import') chooseImport(b);
        return true;
    }

    document.addEventListener('DOMContentLoaded', function() {
        var el = document.getElementById('settings-private-network');
        if (el) el.addEventListener('click', onSectionClick);
        render();
    });

    RS.networkIdentity = {
        state: function() { return state; },
        refresh: refresh,
        create: create,
        chooseImport: chooseImport,
        share: share,
        remove: remove,
        onChange: function(fn) { listeners.push(fn); }
    };
    RS.hostDiscovery = { html: hostDiscoveryHtml, state: hostDiscoveryState, read: hostDiscoveryRead, handleInline: handleInline };
})();
