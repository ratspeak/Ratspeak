// rsNode Repeaters managed by message: a header badge, a command bar with
// autocomplete, a status card from the newest FIELD_RESULTS, and firmware
// updates started by dropping or picking a .rsota file.
(function() {
    // The node's command set; `form` is what autocomplete fills in.
    var COMMANDS = [
        { form: '/help', hint: 'Commands you can use' },
        { form: '/status', hint: 'Release, radio, and links' },
        { form: '/stats', hint: 'Packets and airtime' },
        { form: '/neighbors', hint: 'Interfaces and paths' },
        { form: '/settings', hint: 'Wi-Fi, uplink, and peers' },
        { form: '/settings wifi off', hint: 'Turn Wi-Fi off' },
        { form: '/settings wifi join "<name>" ["<password>"]', hint: 'Join a network' },
        { form: '/settings wifi own "<password>" [channel <1-13>]', hint: 'Run its own network' },
        { form: '/settings uplink <host>:<port>', hint: 'Connect to a server' },
        { form: '/settings uplink off', hint: 'Stop the uplink' },
        { form: '/settings peers on [port <n>]', hint: 'Accept local peers' },
        { form: '/settings peers off', hint: 'Stop local peers' },
        { form: '/radio', hint: 'Region, preset, and power' },
        { form: '/radio region <americas|europe|uhf433|australia|asia>', hint: 'Set the region' },
        { form: '/radio preset <name>', hint: 'Set the preset' },
        { form: '/radio freq <MHz>', hint: 'Set the frequency' },
        { form: '/radio bw <kHz>', hint: 'Set the bandwidth' },
        { form: '/radio sf <7-12>', hint: 'Set the spreading factor' },
        { form: '/radio cr <5-8>', hint: 'Set the coding rate' },
        { form: '/radio power <dBm>', hint: 'Set the power' },
        { form: '/radio limits <off|default|custom <dBm> [duty on|off]>', hint: 'Set the limits' },
        { form: '/managers', hint: 'Owner and admins' },
        { form: '/add-admin <address>', hint: 'Add an admin' },
        { form: '/remove <address>', hint: 'Remove an admin' },
        { form: '/announce <off|10m|30m|60m|<N>h>', hint: 'Announce interval' },
        { form: '/start', hint: 'Start forwarding' },
        { form: '/stop', hint: 'Pause forwarding' },
        { form: '/reboot', hint: 'Restart the node' },
        { form: '/nearby-only <on|off>', hint: 'Managers nearby only' },
        { form: '/update', hint: 'Last update' },
        { form: '/confirm <code>', hint: 'Confirm a change' },
    ];
    var QUICK = ['/status', '/stats', '/neighbors', '/settings', '/radio', '/help'];
    var REGIONS = { 1: 'Americas 915', 2: 'Europe 868', 3: '433 UHF', 4: 'Australia 915', 5: 'Asia 923' };
    var LIMITS = { off: 'Off', 'default': 'Default', custom: 'Custom' };
    var UPLINK = { off: 'Off', connecting: 'Connecting', up: 'Up' };
    var STAGES = {
        preparing: 'Preparing',
        connecting: 'Connecting',
        waiting: 'Reconnecting',
        sending: 'Sending',
        finishing: 'Finishing',
        restarting: 'Restarting',
        verifying: 'Checking the new release',
        unconfirmed: 'Waiting for the node to confirm',
    };
    var TERMINAL = ['installed', 'rolled_back', 'failed', 'cancelled'];
    var SUGGEST_MAX = 6;

    var updates = {};
    var fetchedFor = null;
    var statusOpen = false;
    var suggestIndex = -1;

    function hashOf(hash) {
        return _canonicalConversationHash(hash || lxmfActiveContact);
    }

    function isDevice(hash) {
        var contact = _lookupContactRecord(hashOf(hash));
        return !!(contact && contact.is_rsnode);
    }

    function canUpdate(hash) {
        var contact = _lookupContactRecord(hashOf(hash));
        return !!(contact && contact.can_update);
    }

    function isRsota(name) {
        return /\.rsota$/i.test(String(name || ''));
    }

    // The text a command form fills in: up to its first placeholder.
    function completion(form) {
        var cut = form.search(/ ["[<]/);
        return cut === -1 ? form : form.slice(0, cut + 1);
    }

    function suggestions(text) {
        var typed = String(text || '').toLowerCase();
        if (typed.charAt(0) !== '/' || typed.indexOf('\n') !== -1) return [];
        return COMMANDS.filter(function(c) {
            var fill = completion(c.form).toLowerCase();
            return fill.indexOf(typed) === 0 && fill !== typed;
        }).slice(0, SUGGEST_MAX);
    }

    function newestResults(messages, command) {
        for (var i = messages.length - 1; i >= 0; i--) {
            var r = messages[i] && messages[i].results;
            if (r && r.command === command) return { results: r, timestamp: messages[i].timestamp };
        }
        return null;
    }

    function duration(seconds) {
        seconds = Number(seconds) || 0;
        if (seconds < 60) return seconds + ' s';
        if (seconds < 3600) return Math.floor(seconds / 60) + ' min';
        if (seconds < 86400) return Math.floor(seconds / 3600) + ' h ' + Math.floor((seconds % 3600) / 60) + ' min';
        return Math.floor(seconds / 86400) + ' d ' + Math.floor((seconds % 86400) / 3600) + ' h';
    }

    function interval(seconds) {
        seconds = Number(seconds) || 0;
        if (!seconds) return 'Off';
        return seconds % 3600 === 0 ? 'Every ' + (seconds / 3600) + ' h' : 'Every ' + Math.round(seconds / 60) + ' min';
    }

    function radioText(radio) {
        return (radio.frequency_hz / 1e6).toFixed(3) + ' MHz · ' + (radio.bandwidth_hz / 1000) + ' kHz · SF' +
            radio.sf + ' · CR 4/' + radio.cr + ' · ' + radio.power_dbm + ' dBm';
    }

    // Labelled rows from /status and /stats results; absent fields are skipped.
    function statusRows(status, stats) {
        var rows = [];
        function add(label, value) { if (value !== undefined && value !== null && value !== '') rows.push([label, String(value)]); }
        if (status) {
            add('Release', status.release);
            if (typeof status.running === 'boolean') add('Forwarding', status.running ? 'Running' : 'Paused');
            if (status.uptime_s !== undefined) add('Uptime', duration(status.uptime_s));
            if (status.region !== undefined) add('Region', status.region === null ? 'Not set' : (REGIONS[status.region] || 'Region ' + status.region));
            if (status.radio) add('Radio', radioText(status.radio));
            add('Limits', LIMITS[status.limits]);
            if (status.announce_interval_s !== undefined) add('Announces', interval(status.announce_interval_s));
        }
        var links = stats || status;
        if (links) {
            add('Uplink', UPLINK[links.uplink]);
            if (links.peers !== undefined) add('Peers', links.peers);
        }
        if (stats) {
            if (stats.packets) add('Packets', stats.packets.rx + ' received · ' + stats.packets.tx + ' sent');
            if (stats.airtime_percent !== undefined) add('Airtime', stats.airtime_percent + '% of the hourly budget');
        }
        return rows;
    }

    function progressText(p) {
        var stage = STAGES[p.stage] || p.stage;
        if (p.stage === 'sending' && p.total) return stage + ' ' + Math.floor(p.sent * 100 / p.total) + '%';
        return stage;
    }

    function renderUpdate(p) {
        if (!p) return '';
        var percent = p.total ? Math.min(100, Math.floor(p.sent * 100 / p.total)) : 0;
        var canCancel = ['restarting', 'verifying', 'unconfirmed'].indexOf(p.stage) === -1;
        return '<div class="rsnode-update" role="status" aria-live="polite">' +
            '<div class="rsnode-update-row">' +
                '<span class="rsnode-update-title">' + escapeHtml(p.release ? 'Updating to ' + p.release : 'Updating') + '</span>' +
                '<span class="rsnode-update-stage">' + escapeHtml(progressText(p)) + '</span>' +
                (canCancel ? '<button type="button" class="rsnode-link-btn" data-rsnode-action="cancel">Cancel</button>' : '') +
            '</div>' +
            '<div class="rsnode-update-bar"><span style="width:' + percent + '%"></span></div>' +
            (p.detail ? '<div class="rsnode-update-detail">' + escapeHtml(p.detail) + '</div>' : '') +
        '</div>';
    }

    // One line at a glance: forwarding and release.
    function summary(status) {
        if (!status) return 'No status yet';
        var parts = [];
        if (typeof status.running === 'boolean') parts.push(status.running ? 'Running' : 'Paused');
        if (status.release) parts.push(status.release);
        return parts.join(' · ') || 'Status received';
    }

    function renderStatus(messages) {
        var status = newestResults(messages, 'status');
        var stats = newestResults(messages, 'stats');
        var rows = statusRows(status && status.results, stats && stats.results);
        var newest = Math.max(status ? status.timestamp : 0, stats ? stats.timestamp : 0);
        var body = rows.length
            ? '<dl class="rsnode-status-list">' + rows.map(function(r) {
                return '<div><dt>' + escapeHtml(r[0]) + '</dt><dd>' + escapeHtml(r[1]) + '</dd></div>';
            }).join('') + '</dl>'
            : '<p class="rsnode-status-empty">Send /status to see how the node is doing.</p>';
        return '<details class="rsnode-status"' + (statusOpen ? ' open' : '') + '>' +
            '<summary>' +
                '<span class="rsnode-status-title">Node status</span>' +
                '<span class="rsnode-status-summary">' + escapeHtml(summary(status && status.results)) + '</span>' +
                (newest ? '<span class="rsnode-status-age">' + escapeHtml(RS.relativeTime(newest)) + '</span>' : '') +
            '</summary>' + body +
            '<div class="rsnode-status-actions">' +
                '<button type="button" class="rsnode-link-btn" data-rsnode-send="/status">Refresh status</button>' +
                '<button type="button" class="rsnode-link-btn" data-rsnode-send="/stats">Refresh stats</button>' +
            '</div>' +
        '</details>';
    }

    function renderCommands(bar) {
        bar.innerHTML = QUICK.map(function(cmd) {
            return '<button type="button" class="rsnode-chip" data-rsnode-send="' + cmd + '">' + escapeHtml(cmd) + '</button>';
        }).join('') +
            '<button type="button" class="rsnode-chip rsnode-chip-more" data-rsnode-action="more" aria-label="All commands">More</button>';
    }

    function render() {
        var device = !!lxmfActiveContact && isDevice();
        var hash = hashOf();
        var badge = document.getElementById('lxmf-rsnode-badge');
        var panel = document.getElementById('rsnode-panel');
        var bar = document.getElementById('rsnode-commands');
        if (badge) badge.hidden = !device;
        if (bar) {
            bar.hidden = !device;
            if (device && !bar.childElementCount) renderCommands(bar);
        }
        if (!device) {
            hideSuggest();
            if (panel) { panel.hidden = true; panel.innerHTML = ''; }
            return;
        }
        if (fetchedFor !== hash) {
            fetchedFor = hash;
            RS.invoke('rsnode_update_state', { dest_hash: hash }).then(function(p) {
                if (p && !updates[hash]) { updates[hash] = p; render(); }
            }).catch(function() {});
        }
        if (panel) {
            panel.hidden = false;
            panel.innerHTML = renderUpdate(updates[hash]) + renderStatus(lxmfConversation || []);
            var details = panel.querySelector('details');
            if (details) details.addEventListener('toggle', function() { statusOpen = details.open; });
        }
    }

    function send(command) {
        var input = document.getElementById('lxmf-input');
        if (!input || !lxmfActiveContact) return;
        if (input.value.trim()) {
            fill(command + ' ');
            return;
        }
        input.value = command;
        sendLxmfMessage('auto');
    }

    function fill(text) {
        var input = document.getElementById('lxmf-input');
        if (!input) return;
        input.value = text;
        input.focus();
        try { input.setSelectionRange(text.length, text.length); } catch (_) {}
        input.dispatchEvent(new Event('input'));
    }

    function hideSuggest() {
        var box = document.getElementById('rsnode-suggest');
        if (box) { box.hidden = true; box.innerHTML = ''; }
        suggestIndex = -1;
    }

    function showSuggest(list) {
        var box = document.getElementById('rsnode-suggest');
        if (!box) return;
        if (!list.length) { hideSuggest(); return; }
        if (suggestIndex >= list.length) suggestIndex = list.length - 1;
        box.innerHTML = list.map(function(c, i) {
            return '<button type="button" role="option" class="rsnode-suggest-item' + (i === suggestIndex ? ' active' : '') +
                '" aria-selected="' + (i === suggestIndex) + '" data-rsnode-fill="' + escapeHtml(completion(c.form)) + '">' +
                '<code>' + escapeHtml(c.form) + '</code><span>' + escapeHtml(c.hint) + '</span></button>';
        }).join('');
        box.hidden = false;
    }

    function refreshSuggest() {
        var input = document.getElementById('lxmf-input');
        if (!input || !isDevice()) { hideSuggest(); return; }
        showSuggest(suggestions(input.value));
    }

    function showAllCommands() {
        var input = document.getElementById('lxmf-input');
        suggestIndex = -1;
        if (input && !input.value.trim()) fill('/');
        else if (input) input.focus();
        var box = document.getElementById('rsnode-suggest');
        if (!box) return;
        var all = COMMANDS.map(function(c) {
            return '<button type="button" role="option" class="rsnode-suggest-item" data-rsnode-fill="' +
                escapeHtml(completion(c.form)) + '"><code>' + escapeHtml(c.form) + '</code><span>' + escapeHtml(c.hint) + '</span></button>';
        }).join('');
        box.innerHTML = all;
        box.hidden = false;
        box.classList.add('rsnode-suggest-all');
    }

    function interceptFile(file, hash) {
        if (!file || !isRsota(file.name) || !canUpdate(hash)) return false;
        startUpdate(file, hashOf(hash));
        return true;
    }

    function startUpdate(file, hash) {
        var name = _conversationNameInfo(hash, null, false).name;
        rsConfirm({
            title: 'Update ' + name + '?',
            message: 'The node restarts into the new release when the transfer ends. Keep it powered until it reports back.',
            confirmText: 'Update',
        }).then(function(ok) {
            if (!ok) return;
            updates[hash] = { contact: hash, stage: 'preparing', sent: 0, total: file.size, release: '' };
            render();
            return _stageAttachmentBlob(file, file.name, 'application/octet-stream', false, hash, null)
                .then(function(token) {
                    return RS.invoke('rsnode_update_start', { args: { dest_hash: hash, stage_token: token } });
                })
                .catch(function(err) {
                    delete updates[hash];
                    render();
                    showToast(err && err.message ? err.message : 'The update did not start', 'toast-error', 5000);
                });
        });
    }

    function onProgress(p) {
        if (!p || !p.contact) return;
        var hash = _canonicalConversationHash(p.contact);
        if (TERMINAL.indexOf(p.stage) !== -1) {
            delete updates[hash];
            if (p.stage === 'cancelled') showToast('Update cancelled', 'toast-info', 3000);
        } else {
            updates[hash] = p;
        }
        if (hash === hashOf()) render();
    }

    function wire() {
        var panel = document.getElementById('rsnode-panel');
        var bar = document.getElementById('rsnode-commands');
        var box = document.getElementById('rsnode-suggest');
        var input = document.getElementById('lxmf-input');
        [panel, bar].forEach(function(el) {
            if (!el) return;
            el.addEventListener('click', function(e) {
                var target = e.target.closest('[data-rsnode-send],[data-rsnode-action]');
                if (!target) return;
                if (target.dataset.rsnodeSend) send(target.dataset.rsnodeSend);
                else if (target.dataset.rsnodeAction === 'more') showAllCommands();
                else if (target.dataset.rsnodeAction === 'cancel') {
                    RS.invoke('rsnode_update_cancel', { dest_hash: hashOf() }).catch(function() {});
                }
            });
        });
        if (box) {
            // Keep the composer focused (and the mobile keyboard up) while choosing.
            box.addEventListener('mousedown', function(e) { e.preventDefault(); });
            box.addEventListener('click', function(e) {
                var item = e.target.closest('[data-rsnode-fill]');
                if (!item) return;
                box.classList.remove('rsnode-suggest-all');
                fill(item.dataset.rsnodeFill);
            });
        }
        if (input) {
            input.addEventListener('input', function() {
                var box = document.getElementById('rsnode-suggest');
                if (box) box.classList.remove('rsnode-suggest-all');
                refreshSuggest();
            });
            input.addEventListener('blur', function() {
                setTimeout(function() { if (document.activeElement !== input) hideSuggest(); }, 150);
            });
            // Capture so a chosen suggestion is filled in rather than sent.
            input.addEventListener('keydown', function(e) {
                var box = document.getElementById('rsnode-suggest');
                if (!box || box.hidden) return;
                var items = box.querySelectorAll('[data-rsnode-fill]');
                if (e.key === 'Escape') { e.preventDefault(); e.stopImmediatePropagation(); hideSuggest(); return; }
                if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                    e.preventDefault();
                    suggestIndex = (suggestIndex + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length;
                    items.forEach(function(item, i) {
                        item.classList.toggle('active', i === suggestIndex);
                        item.setAttribute('aria-selected', String(i === suggestIndex));
                    });
                    return;
                }
                var chosen = suggestIndex >= 0 ? items[suggestIndex] : (e.key === 'Tab' ? items[0] : null);
                if ((e.key === 'Tab' || e.key === 'Enter') && chosen) {
                    e.preventDefault();
                    e.stopImmediatePropagation();
                    box.classList.remove('rsnode-suggest-all');
                    fill(chosen.dataset.rsnodeFill);
                }
            }, true);
        }
    }

    window.RS = window.RS || {};
    RS.rsnode = {
        render: render,
        isDevice: isDevice,
        interceptFile: interceptFile,
        // Pure helpers, exercised by scripts/dashboard/test_rsnode_device.js.
        _suggestions: suggestions,
        _completion: completion,
        _statusRows: statusRows,
        _summary: summary,
        _newestResults: newestResults,
        _isRsota: isRsota,
        _onProgress: onProgress,
        _updates: updates,
    };
    if (typeof document !== 'undefined' && document.getElementById) {
        wire();
        if (RS.listen) {
            RS.listen('rsnode_update_progress', onProgress);
            RS.listen('contacts_update', function() { render(); });
        }
    }
})();
