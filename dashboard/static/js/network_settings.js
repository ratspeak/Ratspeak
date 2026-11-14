// Settings › Network › Discovery, Network Stamps, and Links, the Network view's
// Discovered group, and the restart every start-time setting needs.

(function() {
    'use strict';

    var DEFAULTS = {
        discover: false,
        autojoin: 0,
        autoconnect_unknown_implementations: false,
        trusted_only: false,
        trusted_publishers: [],
        stamp_outgoing: false,
        require_stamps: false,
        link_mtu_discovery_off: false,
    };
    var HEX32 = /^[0-9a-f]{32}$/;
    var REFRESH_MS = 10000;
    var GROUPS = ['discovery-settings-body', 'stamp-settings-body', 'link-settings-body'];

    var NS = {
        settings: null,
        discovered: { enabled: false, entries: [] },
        showOlder: false,
        timer: null,
        busy: false,
    };
    RS.networkSettings = NS;

    function $(id) { return document.getElementById(id); }
    function toast(message, tone, ms) {
        if (typeof showToast === 'function') showToast(message, tone || 'toast-info', ms || 4000);
    }
    function errorText(err, fallback) { return (err && err.message) || fallback; }
    function plural(n, one, many) { return n === 1 ? '1 ' + one : n + ' ' + many; }
    function current() { return Object.assign({}, DEFAULTS, NS.settings || {}); }

    // ------------------------------------------------------------- restart

    function impactSentence(impact) {
        var clients = (impact && impact.clients) || 0;
        var drop = clients
            ? plural(clients, 'connected device will disconnect for a moment.', 'connected devices will disconnect for a moment.')
            : '';
        if (impact && impact.call) return clients ? 'Your call ends, and ' + drop : 'Your call ends.';
        return drop;
    }

    // One commit is one restart. A confirm shows only when something live would drop.
    NS.commitRestart = function(opts) {
        if (NS.busy) return Promise.resolve(false);
        return RS.invoke('api_restart_impact').catch(function() { return null; }).then(function(impact) {
            var sentence = impactSentence(impact);
            if (!sentence || typeof rsConfirm !== 'function') return true;
            return rsConfirm({ title: 'Restart the network?', message: sentence, confirmText: 'Restart' });
        }).then(function(ok) {
            if (!ok) return false;
            setBusy(opts, true);
            return Promise.resolve().then(opts.run).then(function(result) {
                setBusy(opts, false);
                toast('Network restarted', 'toast-success', 3000);
                return result || true;
            }, function(err) {
                setBusy(opts, false);
                toast(errorText(err, 'The network could not restart'), 'toast-error', 8000);
                return false;
            });
        });
    };

    // While a change restarts the network, its switch spins and its line says so.
    function setBusy(opts, busy) {
        NS.busy = busy;
        var row = opts.row ? $(opts.row) : null;
        GROUPS.forEach(function(id) {
            var group = $(id);
            if (group) group.classList.toggle('is-restarting', busy);
        });
        if (row) {
            row.classList.toggle('is-committing', busy);
            var track = row.querySelector('.ui-switch');
            var input = row.querySelector('.ui-switch input');
            if (track) track.classList.toggle('is-busy', busy);
            if (input) input.disabled = busy;
            var desc = row.querySelector('.ui-prop-desc');
            if (desc && busy) {
                desc.setAttribute('data-rest', desc.textContent);
                desc.innerHTML = '<span class="ui-status"><span class="ui-spin ui-spin--ring" aria-hidden="true"></span>Restarting the network…</span>';
            } else if (desc && desc.hasAttribute('data-rest')) {
                desc.textContent = desc.getAttribute('data-rest');
                desc.removeAttribute('data-rest');
            }
        }
        var value = opts.value ? $(opts.value) : null;
        if (value && busy) setBadge(opts.value, 'Restarting…');
        if (!busy) render();
    }

    // -------------------------------------------------------------- settings

    function setBadge(id, text) {
        var el = $(id);
        if (!el) return;
        var v = el.querySelector('.v');
        if (v) {
            v.textContent = text;
            var label = el.getAttribute('data-label') || (el.getAttribute('aria-label') || '').split(':')[0];
            el.setAttribute('data-label', label);
            el.setAttribute('aria-label', label + ': ' + text);
        } else {
            el.textContent = text;
        }
    }
    function show(id, visible) {
        var el = $(id);
        if (el) el.hidden = !visible;
    }
    function setToggle(id, on) {
        var el = $(id);
        if (el) el.checked = !!on;
    }

    function render() {
        if (NS.busy) return;
        var s = current();
        setToggle('ns-discover-toggle', s.discover);
        show('ns-autojoin-row', s.discover);
        setToggle('ns-unknown-impl-toggle', s.autoconnect_unknown_implementations);
        show('ns-unknown-impl-row', s.discover && s.autojoin > 0);
        show('ns-accept-row', s.discover);
        show('ns-trusted-btn', s.discover && s.trusted_only);
        setBadge('ns-autojoin-select', s.autojoin ? plural(s.autojoin, 'server', 'servers') : 'Off');
        setBadge('ns-accept-select', s.trusted_only ? 'Trusted publishers' : 'Anyone');
        var trusted = s.trusted_publishers.length;
        setBadge('ns-trusted-value', trusted ? String(trusted) : 'None');
        setToggle('ns-stamp-toggle', s.stamp_outgoing);
        setToggle('ns-require-stamps-toggle', s.require_stamps);
        setToggle('ns-mtu-toggle', !s.link_mtu_discovery_off);
        renderDiscovered();
    }

    function save(patch, where) {
        var next = Object.assign(current(), patch);
        return NS.commitRestart({
            row: where.row,
            value: where.value,
            run: function() {
                return RS.invoke('set_network_settings', { args: next }).then(function(reply) {
                    NS.settings = reply || next;
                    refreshDiscovered();
                    return NS.settings;
                });
            },
        }).then(function(ok) {
            render();
            return ok;
        });
    }

    function load() {
        return RS.invoke('api_network_settings').then(function(reply) {
            NS.settings = reply || Object.assign({}, DEFAULTS);
            render();
            refreshDiscovered();
        }).catch(function() { render(); });
    }

    function bindBadge(id, open) {
        var el = $(id);
        if (!el) return;
        el.addEventListener('click', open);
    }

    // An inline select's menu: a trailing check on the current value.
    function pick(id, title, choices, currentValue, onPick) {
        var el = $(id);
        if (!el || NS.busy || !RS.kit || typeof RS.kit.menu !== 'function') return;
        RS.kit.menu(el, {
            title: title,
            items: choices.map(function(c) {
                return {
                    label: c.label,
                    hint: c.hint,
                    checked: c.value === currentValue,
                    onSelect: function() { onPick(c.value); }
                };
            })
        });
    }

    // An on/off row: its switch's state becomes one field.
    function bindToggle(id, row, patch) {
        var el = $(id);
        if (!el) return;
        el.addEventListener('change', function() {
            save(patch(!!el.checked), { row: row });
        });
    }

    function wireSettings() {
        bindToggle('ns-discover-toggle', 'ns-discover-row', function(on) { return { discover: on }; });
        bindToggle('ns-unknown-impl-toggle', 'ns-unknown-impl-row', function(on) {
            return { autoconnect_unknown_implementations: on };
        });
        bindToggle('ns-stamp-toggle', 'ns-stamp-row', function(on) { return { stamp_outgoing: on }; });
        bindToggle('ns-require-stamps-toggle', 'ns-require-stamps-row', function(on) { return { require_stamps: on }; });
        bindToggle('ns-mtu-toggle', 'ns-mtu-row', function(on) { return { link_mtu_discovery_off: !on }; });
        bindBadge('ns-autojoin-select', function() {
            var s = current();
            pick('ns-autojoin-select', 'Join automatically', [
                { label: 'Off', value: 0, hint: 'Only connect to servers you add.' },
                { label: '1 server', value: 1 },
                { label: '3 servers', value: 3 },
                { label: '5 servers', value: 5 },
            ], s.autojoin, function(n) {
                if (n === s.autojoin) return;
                save({ autojoin: n }, { row: 'ns-autojoin-row', value: 'ns-autojoin-select' });
            });
        });
        bindBadge('ns-accept-select', function() {
            var s = current();
            pick('ns-accept-select', 'Accept from', [
                { label: 'Anyone', value: 'anyone', hint: 'List servers any publisher shares.' },
                { label: 'Trusted publishers', value: 'trusted', hint: 'List only servers from publishers you add.' },
            ], s.trusted_only ? 'trusted' : 'anyone', function(value) {
                var trusted = value === 'trusted';
                if (trusted === s.trusted_only) return;
                save({ trusted_only: trusted }, { row: 'ns-accept-row', value: 'ns-accept-select' });
            });
        });
        bindBadge('ns-trusted-btn', openTrustedPublishers);
    }

    function openTrustedPublishers() {
        var s = current();
        NS.openHashList({
            title: 'Trusted publishers',
            message: 'Only servers these publishers share are listed and joined. Saving restarts the network for a moment.',
            items: s.trusted_publishers,
            emptyText: 'Add a publisher from a discovered connection, or paste its address.',
            placeholder: 'Paste an address',
            noun: 'A publisher address',
            onSave: function(items) {
                return save({ trusted_publishers: items }, { row: 'ns-trusted-btn', value: 'ns-trusted-value' });
            },
        });
    }

    // ------------------------------------------------------------ list sheet

    function normalizeHash(value) {
        return String(value || '').trim().replace(/^<|>$/g, '').toLowerCase();
    }

    // A list of 32-hex addresses with Add and Remove; one Save commits it.
    NS.openHashList = function(opts) {
        if (typeof _rsBuildSheet !== 'function') return;
        var items = (opts.items || []).slice();
        var built = _rsBuildSheet({ title: opts.title, sheetClass: 'hash-list-sheet' }, function() {});
        built.overlay.addEventListener('click', function(e) { if (e.target === built.overlay) built.dismiss(null); });
        built.sheet.addEventListener('keydown', function(e) {
            if (e.key === 'Escape') { e.stopPropagation(); built.dismiss(null); }
        });

        if (opts.message) {
            var msg = document.createElement('div');
            msg.className = 'rs-dialog-message';
            msg.textContent = opts.message;
            built.body.appendChild(msg);
        }
        var list = document.createElement('ul');
        list.className = 'hash-list';
        var empty = document.createElement('div');
        empty.className = 'hash-list-empty';
        empty.textContent = opts.emptyText || 'None yet.';
        built.body.appendChild(list);
        built.body.appendChild(empty);

        function paint() {
            list.textContent = '';
            items.forEach(function(hash, index) {
                var li = document.createElement('li');
                var code = document.createElement('code');
                code.textContent = hash;
                var remove = document.createElement('button');
                remove.type = 'button';
                remove.className = 'nr-btn nr-btn-sm nr-btn-ghost';
                remove.textContent = 'Remove';
                remove.setAttribute('aria-label', 'Remove ' + hash);
                remove.addEventListener('click', function() {
                    items.splice(index, 1);
                    paint();
                });
                li.appendChild(code);
                li.appendChild(remove);
                list.appendChild(li);
            });
            list.hidden = !items.length;
            empty.hidden = !!items.length;
        }

        var add = document.createElement('div');
        add.className = 'hash-list-add';
        var input = document.createElement('input');
        input.type = 'text';
        input.className = 'modal-input';
        input.placeholder = opts.placeholder || '32 hex characters';
        input.setAttribute('aria-label', opts.placeholder || 'Address');
        input.autocomplete = 'off';
        input.spellcheck = false;
        input.setAttribute('autocapitalize', 'none');
        var addBtn = document.createElement('button');
        addBtn.type = 'button';
        addBtn.className = 'nr-btn nr-btn-sm';
        addBtn.textContent = 'Add';
        add.appendChild(input);
        add.appendChild(addBtn);
        built.body.appendChild(add);
        var error = document.createElement('div');
        error.className = 'rs-dialog-field-error';
        error.hidden = true;
        built.body.appendChild(error);

        function addCurrent() {
            var hash = normalizeHash(input.value);
            if (!hash) return true;
            if (!HEX32.test(hash)) {
                error.textContent = (opts.noun || 'An address') + ' is 32 hex characters.';
                error.hidden = false;
                input.focus();
                return false;
            }
            error.hidden = true;
            if (items.indexOf(hash) === -1) items.push(hash);
            input.value = '';
            paint();
            return true;
        }
        addBtn.addEventListener('click', addCurrent);
        input.addEventListener('keydown', function(e) {
            if (e.key === 'Enter') { e.preventDefault(); addCurrent(); }
        });
        input.addEventListener('input', function() { error.hidden = true; });

        var cancel = document.createElement('button');
        cancel.className = 'rs-dialog-cancel';
        cancel.textContent = 'Cancel';
        cancel.addEventListener('click', function() { built.dismiss(null); });
        var saveBtn = document.createElement('button');
        saveBtn.className = 'rs-dialog-confirm';
        saveBtn.textContent = 'Save';
        saveBtn.addEventListener('click', function() {
            if (!addCurrent()) return;
            built.dismiss(null);
            opts.onSave(items.slice());
        });
        built.footer.appendChild(cancel);
        built.footer.appendChild(saveBtn);
        paint();
        built.present();
        input.focus();
    };

    // ------------------------------------------------------------ discovered

    function joinedNames(entries) {
        return (entries || []).map(function(e) { return e.joined_interface || ''; })
            .filter(Boolean).sort().join('\n');
    }

    function setDiscovered(next) {
        var before = joinedNames(NS.discovered.entries);
        NS.discovered = next;
        renderDiscovered();
        // The Internet/TCP rows mark the connections Join automatically made.
        if (before !== joinedNames(next.entries) && typeof _renderConnectionsFromCache === 'function') {
            _renderConnectionsFromCache();
        }
    }

    // Another app's Reticulum service owns the network; discovery is its business.
    function external() {
        return !!(document.body && document.body.classList.contains('network-external'));
    }

    function refreshDiscovered() {
        if (!current().discover) {
            setDiscovered({ enabled: false, entries: [] });
            return Promise.resolve();
        }
        return RS.invoke('api_discovered_interfaces').then(function(reply) {
            setDiscovered(reply || { enabled: false, entries: [] });
        }).catch(function() {});
    }
    NS.refreshDiscovered = refreshDiscovered;

    function kindLabel(kind) {
        return { backbone: 'Backbone', tcp: 'TCP', radio: 'Radio', i2p: 'I2P' }[kind] || 'Other';
    }
    function mhz(hz) {
        var v = (hz || 0) / 1e6;
        return (Math.round(v * 1000) / 1000).toString();
    }
    function radioDetail(e) {
        var parts = [];
        if (e.frequency) parts.push(mhz(e.frequency) + ' MHz');
        if (e.spreading_factor) parts.push('SF' + e.spreading_factor);
        return parts.join(' · ');
    }
    function detail(e) {
        if (e.kind === 'radio') return radioDetail(e) || 'Radio';
        return e.hops === 1 ? '1 hop away' : e.hops + ' hops away';
    }
    function heard(e) {
        if (!e.last_heard) return '';
        var secs = Math.max(0, Math.floor(Date.now() / 1000) - e.last_heard);
        if (secs < 60) return 'heard just now';
        if (secs < 3600) return 'heard ' + Math.floor(secs / 60) + ' min ago';
        if (secs < 86400) return 'heard ' + Math.floor(secs / 3600) + ' h ago';
        return 'heard ' + Math.floor(secs / 86400) + ' d ago';
    }
    function statusWord(e) {
        if (e.joined) return 'Joined';
        if (e.added) return 'Saved';
        return '';
    }

    function discoveredRow(e) {
        var row = document.createElement('div');
        row.className = 'conn-iface-row conn-discovered-row';
        row.setAttribute('data-discovered-id', e.id);
        row.setAttribute('role', 'button');
        row.setAttribute('tabindex', '0');
        row.setAttribute('aria-haspopup', 'dialog');
        var dot = document.createElement('span');
        dot.setAttribute('aria-hidden', 'true');
        var main = document.createElement('span');
        main.className = 'conn-iface-main';
        var titleline = document.createElement('span');
        titleline.className = 'conn-iface-titleline';
        var name = document.createElement('span');
        name.className = 'conn-iface-name';
        var status = document.createElement('span');
        status.className = 'conn-iface-status-text';
        titleline.appendChild(name);
        titleline.appendChild(status);
        var meta = document.createElement('span');
        meta.className = 'conn-iface-meta';
        main.appendChild(titleline);
        main.appendChild(meta);
        row.appendChild(dot);
        row.appendChild(main);
        row.addEventListener('click', function() {
            if (typeof haptic === 'function') haptic('selection');
            openDiscovered(row.getAttribute('data-discovered-id'));
        });
        return row;
    }

    function paintDiscoveredRow(row, e) {
        var dot = row.firstChild;
        // Freshness reads in neutral tones; green means this device is joined to it.
        dot.className = 'conn-iface-dot discovered-' + e.status + (e.joined ? ' up' : '');
        row.querySelector('.conn-iface-name').textContent = e.name || 'Unnamed';
        row.querySelector('.conn-iface-status-text').textContent = statusWord(e);
        var meta = row.querySelector('.conn-iface-meta');
        meta.textContent = kindLabel(e.kind) + ' · ' + detail(e);
        var when = heard(e);
        if (when) {
            // On a phone the time takes its own line and the separator goes.
            var sep = document.createElement('span');
            sep.className = 'conn-iface-sep';
            sep.textContent = ' · ';
            var span = document.createElement('span');
            span.className = 'conn-iface-heard';
            span.textContent = when;
            meta.appendChild(sep);
            meta.appendChild(span);
        }
        var spoken = { available: 'Heard recently', unknown: 'Heard in the last 3 days', stale: 'Not heard lately' }[e.status] || '';
        row.setAttribute('aria-label', (e.name || 'Unnamed') + ', ' + kindLabel(e.kind) + ', ' + spoken +
            (statusWord(e) ? ', ' + statusWord(e) : ''));
    }

    // Rows keep their elements across refreshes, so hover and focus survive.
    function syncRows(list, entries) {
        var active = document.activeElement;
        var kept = {};
        Array.prototype.slice.call(list.children).forEach(function(row) {
            var key = row.getAttribute('data-discovered-id');
            var wanted = key !== null && !kept[key] && entries.some(function(e) { return e.id === key; });
            if (wanted) kept[key] = row;
            else list.removeChild(row);
        });
        entries.forEach(function(e, index) {
            var row = kept[e.id] || discoveredRow(e);
            if (list.children[index] !== row) list.insertBefore(row, list.children[index] || null);
            paintDiscoveredRow(row, e);
        });
        if (active && active !== document.activeElement && active.isConnected && list.contains(active)) {
            active.focus({ preventScroll: true });
        }
    }

    function renderDiscovered() {
        var section = $('conn-section-discovered');
        if (!section) return;
        var on = current().discover;
        section.hidden = !on;
        if (!on) return;
        var entries = NS.discovered.entries || [];
        // Older entries wait behind Show older; a joined or saved one never does.
        var fresh = entries.filter(function(e) { return e.status !== 'stale' || e.joined || e.added; });
        var older = entries.length - fresh.length;
        var shown = NS.showOlder ? entries : fresh;
        var count = $('conn-count-discovered');
        if (count) {
            count.textContent = shown.length;
            count.classList.toggle('has-active', shown.length > 0);
        }
        // Open unless the person closed it: empty, it says what will appear.
        var collapsed = typeof shouldSectionBeCollapsed === 'function'
            ? shouldSectionBeCollapsed('discovered', true)
            : false;
        section.classList.remove('inactive');
        section.classList.toggle('collapsed', collapsed);
        var header = section.querySelector('.conn-section-header');
        if (header) header.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
        var list = $('conn-list-discovered');
        if (!list) return;
        if (!shown.length) {
            list.textContent = '';
            var none = document.createElement('div');
            none.className = 'conn-iface-empty';
            var strong = document.createElement('strong');
            strong.textContent = 'Nothing discovered yet';
            none.appendChild(strong);
            none.appendChild(document.createElement('br'));
            none.appendChild(document.createTextNode('Published servers and radios appear here as they’re heard.'));
            list.appendChild(none);
        } else {
            syncRows(list, shown);
        }
        var olderBtn = $('conn-discovered-older');
        if (olderBtn) {
            olderBtn.hidden = !older;
            olderBtn.textContent = NS.showOlder ? 'Hide older' : 'Show older (' + older + ')';
        }
    }

    function entryById(id) {
        return (NS.discovered.entries || []).filter(function(e) { return e.id === id; })[0] || null;
    }

    function openDiscovered(id) {
        var e = entryById(id);
        if (!e || typeof rsChoice !== 'function') return;
        var facts = [];
        if (e.address) facts.push(e.address + (e.port ? ':' + e.port : ''));
        if (typeof e.latitude === 'number' && typeof e.longitude === 'number') {
            facts.push('Near ' + e.latitude.toFixed(2) + ', ' + e.longitude.toFixed(2));
        }
        var trusted = current().trusted_publishers.indexOf(e.publisher) !== -1;
        var note = ['Published by ' + e.publisher.slice(0, 8) + '…' + e.publisher.slice(-4)];
        if (trusted) note.push('Trusted publisher');
        if (heard(e)) note.push(heard(e).charAt(0).toUpperCase() + heard(e).slice(1));
        if (e.private_network) note.push('Private network');
        var choices = [];
        // Join automatically already holds a joined server; keeping it saves that connection.
        if (e.connectable && e.joined_interface) choices.push({ label: 'Keep', value: 'keep', hint: 'Make it a saved connection.' });
        else if (e.connectable) choices.push({ label: 'Connect', value: 'connect', hint: 'Add it to your connections.' });
        if (hasRadioSettings(e)) choices.push({ label: 'Use these radio settings', value: 'radio', hint: 'Start adding a radio with its frequency and modulation.' });
        if (!trusted) choices.push({ label: 'Trust this publisher', value: 'trust', hint: 'Add it to your trusted publishers.' });
        if (!choices.length) choices.push({ label: 'Close', value: 'close' });
        rsChoice({
            title: e.name || 'Discovered connection',
            summary: {
                primary: kindLabel(e.kind) + ' · ' + detail(e) + (statusWord(e) ? ' · ' + statusWord(e) : ''),
                secondary: facts.join(' · '),
                note: note.join(' · '),
            },
            choices: choices,
        }).then(function(choice) {
            if (choice === 'connect') connect(e);
            else if (choice === 'keep') keep(e.joined_interface);
            else if (choice === 'radio') useRadioSettings(e);
            else if (choice === 'trust') trust(e);
        });
    }

    // A radio that publishes all four, so the form opens complete.
    function hasRadioSettings(e) {
        return e.kind === 'radio' && !!(e.frequency && e.bandwidth && e.spreading_factor && e.coding_rate);
    }

    function connect(e) {
        RS.invoke('connect_discovered_interface', { id: e.id }).then(function() {
            toast('Connecting to ' + (e.name || 'the server') + '…', 'toast-info', 3000);
            setTimeout(refreshDiscovered, 1500);
        }).catch(function(err) {
            toast(errorText(err, 'Could not add this connection'), 'toast-error', 8000);
        });
    }

    function useRadioSettings(e) {
        if (typeof openRnodeModalWithSettings !== 'function') return;
        openRnodeModalWithSettings({
            freq: e.frequency,
            bw: e.bandwidth,
            sf: e.spreading_factor,
            cr: e.coding_rate,
        });
    }

    function trust(e) {
        var s = current();
        var list = s.trusted_publishers.concat([e.publisher]);
        save({ trusted_publishers: list }, { row: 'ns-trusted-btn', value: 'ns-trusted-value' })
            .then(function(ok) {
                if (ok && !s.trusted_only) toast('Publisher trusted. It counts once Accept From is Trusted publishers.', 'toast-info', 5000);
            });
    }

    function keep(name) {
        RS.invoke('keep_joined_interface', { name: name }).then(function() {
            toast('Kept as a saved connection', 'toast-success', 3000);
            if (typeof refreshConfigInterfaces === 'function') refreshConfigInterfaces();
            setTimeout(refreshDiscovered, 1500);
        }).catch(function(err) {
            toast(errorText(err, 'Could not keep this connection'), 'toast-error', 8000);
        });
    }

    RS.discovery = {
        keep: keep,
        // A live interface Join automatically made, by its name.
        isJoined: function(name) {
            return !!name && (NS.discovered.entries || []).some(function(e) { return e.joined_interface === name; });
        },
    };

    function wireDiscovered() {
        var older = $('conn-discovered-older');
        if (older) older.addEventListener('click', function() {
            NS.showOlder = !NS.showOlder;
            renderDiscovered();
        });
        // Poll only while the Network view is on screen.
        NS.timer = setInterval(function() {
            if (document.hidden || external()) return;
            if (current().discover && typeof isViewActive === 'function' && isViewActive('network')) refreshDiscovered();
        }, REFRESH_MS);
    }

    // ------------------------------------------------------------------ boot

    function init() {
        wireSettings();
        wireDiscovered();
        if (typeof RS.listen === 'function') {
            RS.listen('identity_switched', function() { load(); });
        }
        render();
        load();
    }

    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
    else init();
})();
