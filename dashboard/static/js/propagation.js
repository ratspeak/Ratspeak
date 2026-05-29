// Offline Inbox UI — three-mode toggle (Off / Auto / Manual), Ratspeak-node
// preference toggle, manual hash input, and a path-request refresh button.

var propagationStatus = {
    enabled: false,
    mode: 'auto',
    favor_static: true,
    auto_active_node: null,
    awaiting_discovery: false,
    static_nodes_known: 0,
    pn_parse_failures: 0,
    propagation_node: null,
    message_count: 0,
    hosting_enabled: false,
    local_node_hash: null,
    local_node_message_count: 0,
    local_node_stamp_cost: 16,
    enforce_stamps: false,
    required_stamp_cost: 0,
};

var discoveredRelayNodes = [];
var refreshInFlight = false;
var refreshWatchdog = null;
var RELAY_REFRESH_WATCHDOG_MS = 15000;

// Backend trims to 512 with a 48h TTL; UI shows top 30 (rest still tracked).
var MAX_RELAY_ROWS = 30;

function propagationNodePriority(node) {
    var parsed = parseInt(node && node.priority, 10);
    return isNaN(parsed) ? 1000 : parsed;
}

function renderPropagationStatus(targetId) {
    var container = document.getElementById(targetId || 'settings-propagation-status');
    if (!container) return;

    var mode = propagationStatus.mode || 'auto';
    var favorStatic = propagationStatus.favor_static !== false;
    var nodeHash = propagationStatus.propagation_node || null;
    var autoActive = propagationStatus.auto_active_node || null;
    var selectedNodeHash = mode === 'auto' ? autoActive : nodeHash;
    var msgCount = propagationStatus.message_count || 0;
    var isConnected = propagationStatus.connected || false;
    var awaiting = !!propagationStatus.awaiting_discovery;

    var html = '';

    html += '<div class="inline-hint relay-intro">' +
        'When contacts can\'t reach you directly, your Offline Inbox stores their messages until you come back online.' +
    '</div>';

    html += '<div class="relay-mode-toggle" role="tablist">' +
        modeBtnHtml('off',    'Off',    mode === 'off') +
        modeBtnHtml('auto',   'Auto',   mode === 'auto') +
        modeBtnHtml('manual', 'Manual', mode === 'manual') +
    '</div>';

    if (mode === 'off') {
        html += '<div class="inline-hint">' +
            'Offline Inbox is disabled. Any sync already in progress will finish; no new ones will start.' +
        '</div>';
    } else if (mode === 'auto') {
        html += renderAutoBody(autoActive, isConnected, msgCount, favorStatic, awaiting);
    } else if (mode === 'manual') {
        html += renderManualBody(nodeHash, isConnected, msgCount);
    }

    if (mode === 'manual') {
        html += renderDiscoveredList(mode, favorStatic, selectedNodeHash);
    }

    html += renderHostingSettings();
    html += renderStampSettings();

    container.innerHTML = html;
    wireUpHandlers(container, mode);
}

function modeBtnHtml(value, label, active) {
    return '<button type="button" class="relay-mode-btn' + (active ? ' relay-mode-btn-active' : '') +
        '" data-mode="' + value + '" style="flex:1;padding:6px 12px;">' +
        escapeHtml(label) +
        '</button>';
}

function renderAutoBody(autoActive, isConnected, msgCount, favorStatic, awaiting) {
    var html = '';
    html += '<label class="settings-row" style="border-bottom:none;cursor:pointer;">' +
        '<div class="settings-row-info">' +
            '<span class="settings-row-label">Favor Ratspeak inbox nodes</span>' +
            '<span class="settings-row-desc">Prefer reachable Ratspeak inbox nodes, with fallback when none can be reached.</span>' +
        '</div>' +
        '<input type="checkbox" id="prop-favor-static-toggle"' + (favorStatic ? ' checked' : '') + '>' +
    '</label>';

    if (autoActive) {
        var activeNode = findNode(autoActive);
        var nodeName = activeNode ? (activeNode.display_name || findNodeName(autoActive)) : findNodeName(autoActive);
        var activeLabel = activeNode && activeNode.static
            ? 'Using Ratspeak inbox'
            : (favorStatic ? 'Using fallback inbox' : 'Using inbox');
        var statusDot = isConnected
            ? '<span class="text-status-online">●</span>'
            : '<span class="text-muted-color">○</span>';
        html += '<div class="relay-card relay-card-active">' +
            '<div class="relay-card-header">' +
                statusDot + ' <strong>' + escapeHtml(activeLabel) + ': ' + escapeHtml(nodeName) + '</strong>' +
            '</div>' +
            '<div class="relay-card-details">' +
                '<span>' + msgCount + ' message' + (msgCount !== 1 ? 's' : '') + ' stored</span>' +
                ' · <span>Auto</span>' +
            '</div>' +
            '<div class="inline-hint-sm" style="overflow-wrap:anywhere;margin-top:6px;">' +
                'Propagation address<br>' + escapeHtml(autoActive) +
            '</div>' +
            '<div class="relay-card-actions">' +
                '<button class="nr-btn nr-btn-sm" id="prop-sync-btn">Check Now</button>' +
            '</div>' +
        '</div>';
    } else if (awaiting) {
        html += '<div class="relay-card relay-card-empty">' +
            '<div class="inline-hint">' +
                'Looking for Offline Inbox nodes. Ratspeak inbox nodes are checked carefully to avoid unnecessary network traffic; Auto will use another reachable inbox node if one appears first.' +
            '</div>' +
        '</div>';
    } else {
        html += '<div class="relay-card relay-card-empty">' +
            '<div class="inline-hint">Looking for a reachable Offline Inbox&hellip;</div>' +
        '</div>';
    }
    return html;
}

function renderManualBody(nodeHash, isConnected, msgCount) {
    var html = '';
    if (nodeHash) {
        var nodeName = findNodeName(nodeHash);
        var statusDot = isConnected
            ? '<span class="text-status-online">●</span>'
            : '<span class="text-muted-color">○</span>';
        html += '<div class="relay-card relay-card-active">' +
            '<div class="relay-card-header">' +
                statusDot + ' <strong>' + escapeHtml(nodeName) + '</strong>' +
            '</div>' +
            '<div class="relay-card-details">' +
                '<span>' + msgCount + ' message' + (msgCount !== 1 ? 's' : '') + ' stored</span>' +
                ' · <span>Manual</span>' +
            '</div>' +
            '<div class="relay-card-actions">' +
                '<button class="nr-btn nr-btn-sm" id="prop-sync-btn">Check Now</button>' +
                '<button class="nr-btn nr-btn-sm nr-btn-muted" id="prop-clear-btn">Disconnect</button>' +
            '</div>' +
        '</div>';
    } else {
        html += '<div class="relay-card relay-card-empty">' +
            '<div class="inline-hint">Enter an Offline Inbox node hash below to connect manually.</div>' +
        '</div>';
    }

    html += '<div class="relay-manual-section">' +
        '<div class="inline-hint-sm relay-manual-label">Inbox node hash (hex):</div>' +
        '<div class="relay-manual-row">' +
            '<input type="text" id="prop-node-input" class="modal-input text-xs" ' +
                'placeholder="Offline Inbox node hash (hex)" ' +
                'value="' + escapeHtml(nodeHash || '') + '" ' +
                '>' +
            '<button class="nr-btn nr-btn-sm" id="prop-set-btn">Set</button>' +
        '</div>' +
    '</div>';
    return html;
}

function renderDiscoveredList(mode, favorStatic, activeHash) {
    var html = '';
    html += '<div class="relay-section-header">' +
        '<span>Available inbox nodes</span>' +
        '<button class="nr-btn nr-btn-xs" id="prop-refresh-btn"' +
            (refreshInFlight ? ' disabled' : '') + '>' +
            (refreshInFlight ? 'Refreshing…' : 'Refresh') +
        '</button>' +
    '</div>';

    if (!discoveredRelayNodes.length) {
        html += '<div class="inline-hint" style="padding:8px 0;">' +
            'No inbox nodes discovered yet. Click Refresh or wait for announces.' +
        '</div>';
        return html;
    }

    var sorted = discoveredRelayNodes.slice().sort(function(a, b) {
        if (mode === 'auto' && favorStatic) {
            var aStatic = a.static ? 1 : 0;
            var bStatic = b.static ? 1 : 0;
            if (aStatic !== bStatic) return bStatic - aStatic;
            if (aStatic && bStatic) {
                var aPriority = propagationNodePriority(a);
                var bPriority = propagationNodePriority(b);
                if (aPriority !== bPriority) return aPriority - bPriority;
            }
        }
        var aHops = (a.hops == null) ? 99 : a.hops;
        var bHops = (b.hops == null) ? 99 : b.hops;
        return aHops - bHops;
    });

    var visible = sorted.slice(0, MAX_RELAY_ROWS);
    var truncated = sorted.length > MAX_RELAY_ROWS;

    html += '<div class="relay-node-list">';
    for (var i = 0; i < visible.length; i++) {
        var node = visible[i];
        var isActive = activeHash && node.hash === activeHash;
        var hopLabel;
        if (node.hops === 0) {
            hopLabel = 'direct';
        } else if (node.hops === null || node.hops === undefined) {
            hopLabel = node.static_status === 'probing' ? 'checking' : 'not reached';
        } else {
            hopLabel = node.hops + ' hop' + (node.hops > 1 ? 's' : '');
        }
        var nameHtml = escapeHtml(node.display_name || ('Inbox ' + (node.hash || '').substring(0, 8)));
        if (node.static) {
            nameHtml = '<span class="relay-static-badge" title="Bundled Ratspeak inbox node">★</span> ' + nameHtml;
        }
        var actionHtml;
        if (isActive) {
            actionHtml = '<span class="relay-node-badge">Active</span>';
        } else if (mode === 'manual') {
            actionHtml = '<button class="nr-btn nr-btn-xs relay-select-btn" data-hash="' +
                escapeHtml(node.hash) + '">Select</button>';
        } else {
            actionHtml = '<span class="text-muted-color text-xs">tracked</span>';
        }
        html += '<div class="relay-node-row' + (isActive ? ' relay-node-active' : '') +
            '" data-hash="' + escapeHtml(node.hash) + '">' +
            '<span class="relay-node-name">' + nameHtml + '</span>' +
            '<span class="relay-node-hops">' + hopLabel + '</span>' +
            actionHtml +
        '</div>';
    }
    html += '</div>';

    var footnotes = [];
    if (truncated) {
        footnotes.push('Showing ' + MAX_RELAY_ROWS + ' of ' + sorted.length +
            ' inbox nodes. Lower-ranked nodes are still tracked.');
    }
    var failures = propagationStatus.pn_parse_failures || 0;
    if (failures > 0) {
        footnotes.push(failures + ' announce' + (failures === 1 ? '' : 's') +
            ' ignored this session (unparseable PN format).');
    }
    if (footnotes.length) {
        html += '<div class="relay-footnotes inline-hint">' +
            footnotes.map(escapeHtml).join('<br>') +
        '</div>';
    }
    return html;
}

function renderHostingSettings() {
    // Inbox hosting needs an always-available key; a hardware (PIV) identity can
    // be unplugged or its session can lock, so hosting isn't offered for it.
    var ai = (typeof activeIdentity === 'function') ? activeIdentity() : null;
    if (ai && ai.is_hardware) {
        return '<div class="relay-advanced-block">' +
            '<div class="propagation-section-title">Hosted Offline Inbox</div>' +
            '<div class="settings-row-desc">Not available for hardware-key identities — a hosted inbox would stop whenever the security key is removed or the session locks.</div>' +
        '</div>';
    }
    var enabled = !!propagationStatus.hosting_enabled;
    var nodeHash = propagationStatus.local_node_hash || '';
    var count = propagationStatus.local_node_message_count || 0;
    var cost = propagationStatus.local_node_stamp_cost;
    if (cost === undefined || cost === null) cost = 16;
    var mobile = (typeof isMobile === 'function') ? isMobile() : !!window.__RATSPEAK_MOBILE__;

    var html = '<div class="relay-advanced-block">' +
        '<div class="propagation-section-title">Hosted Offline Inbox</div>' +
        '<div class="settings-row propagation-settings-row">' +
            '<div class="settings-row-info">' +
                '<span class="settings-row-label">Host inbox node</span>' +
                '<span class="settings-row-desc">' +
                    (mobile
                        ? 'Mobile inbox hosting can be unreliable when the app is backgrounded.'
                        : 'Store offline LXMF messages for other people using this device.') +
                '</span>' +
            '</div>' +
            '<label class="prop-toggle">' +
                '<input type="checkbox" id="prop-host-toggle"' + (enabled ? ' checked' : '') + '>' +
                '<span class="prop-slider"></span>' +
            '</label>' +
        '</div>';

    if (enabled) {
        html += '<div class="relay-card relay-card-active">' +
            '<div class="relay-card-header"><strong>' + escapeHtml(nodeHash || 'Local inbox node') + '</strong></div>' +
            '<div class="relay-card-details">' +
                '<span>' + count + ' stored message' + (count === 1 ? '' : 's') + '</span>' +
                ' · <span>Stamp cost ' + cost + '</span>' +
            '</div>' +
            '<div class="relay-card-actions">' +
                '<button class="nr-btn nr-btn-sm" id="prop-host-announce-btn">Announce</button>' +
                '<button class="nr-btn nr-btn-sm nr-btn-muted" id="prop-host-cost-btn">Stamp Cost</button>' +
            '</div>' +
        '</div>';
    }

    html += '</div>';
    return html;
}

function renderStampSettings() {
    var enforce = !!propagationStatus.enforce_stamps;
    var cost = propagationStatus.required_stamp_cost || 0;
    var label = enforce && cost > 0 ? ('Cost ' + cost) : 'Off';
    return '<details class="relay-advanced-block relay-details">' +
        '<summary>Message stamp protection</summary>' +
        '<div class="settings-row propagation-settings-row">' +
            '<div class="settings-row-info">' +
                '<span class="settings-row-label">Require stamps</span>' +
                '<span class="settings-row-desc">Advertise and require proof-of-work on messages sent directly to you.</span>' +
            '</div>' +
            '<label class="prop-toggle">' +
                '<input type="checkbox" id="stamp-enforce-toggle"' + (enforce ? ' checked' : '') + '>' +
                '<span class="prop-slider"></span>' +
            '</label>' +
        '</div>' +
        '<div class="settings-row propagation-settings-row" style="border-bottom:none;">' +
            '<div class="settings-row-info">' +
                '<span class="settings-row-label">Required work</span>' +
                '<span class="settings-row-desc">Higher values make spam harder but slow down senders.</span>' +
            '</div>' +
            '<button class="selector-badge" id="stamp-cost-btn">' + escapeHtml(label) + '</button>' +
        '</div>' +
    '</details>';
}

function wireUpHandlers(container, mode) {
    container.querySelectorAll('.relay-mode-btn').forEach(function(btn) {
        btn.addEventListener('click', function() {
            var newMode = this.getAttribute('data-mode');
            if (!newMode || newMode === mode) return;
            applyPropagationMode(newMode);
        });
    });

    var favorToggle = document.getElementById('prop-favor-static-toggle');
    if (favorToggle) {
        favorToggle.addEventListener('change', function() {
            applyPropagationMode('auto', { favor_static: !!this.checked });
        });
    }

    var syncBtn = document.getElementById('prop-sync-btn');
    if (syncBtn) syncBtn.addEventListener('click', syncPropagationMailbox);

    var clearBtn = document.getElementById('prop-clear-btn');
    if (clearBtn) clearBtn.addEventListener('click', clearPropagationNode);

    var refreshBtn = document.getElementById('prop-refresh-btn');
    if (refreshBtn) refreshBtn.addEventListener('click', refreshRelayNodes);

    var setBtn = document.getElementById('prop-set-btn');
    if (setBtn) setBtn.addEventListener('click', setPropagationNode);

    container.querySelectorAll('.relay-select-btn').forEach(function(btn) {
        btn.addEventListener('click', function() {
            var hash = this.getAttribute('data-hash');
            if (hash) selectRelayNode(hash);
        });
    });

    var hostToggle = document.getElementById('prop-host-toggle');
    if (hostToggle) {
        hostToggle.addEventListener('change', function() {
            setPropagationHosting(!!this.checked);
        });
    }

    var hostCostBtn = document.getElementById('prop-host-cost-btn');
    if (hostCostBtn) hostCostBtn.addEventListener('click', choosePropagationHostCost);

    var hostAnnounceBtn = document.getElementById('prop-host-announce-btn');
    if (hostAnnounceBtn) {
        hostAnnounceBtn.addEventListener('click', function() {
            RS.invoke('trigger_announce').catch(function() {});
            showToast('Offline Inbox announce queued', 'toast-blue', 2500);
        });
    }

    var stampToggle = document.getElementById('stamp-enforce-toggle');
    if (stampToggle) {
        stampToggle.addEventListener('change', function() {
            setStampSettings(!!this.checked);
        });
    }

    var stampCostBtn = document.getElementById('stamp-cost-btn');
    if (stampCostBtn) stampCostBtn.addEventListener('click', chooseMessageStampCost);
}

function stampCostChoice(title, current, includeOff) {
    var choices = includeOff ? [{ label: 'Off', value: '0', hint: 'Do not require proof-of-work.' }] : [];
    choices = choices.concat([
        { label: 'Cost 8', value: '8', hint: 'Light protection.' },
        { label: 'Cost 12', value: '12', hint: 'Balanced protection.' },
        { label: 'Cost 16', value: '16', hint: 'Offline Inbox default.' }
    ]);
    if (typeof rsChoice === 'function') {
        return rsChoice({ title: title, choices: choices }).then(function(val) {
            if (val === null || val === undefined) return null;
            return parseInt(val, 10);
        });
    }
    if (typeof rsPrompt === 'function') {
        return rsPrompt({
            title: title,
            message: 'Enter a stamp cost from 0 to 32.',
            defaultValue: String(current || (includeOff ? 0 : 16)),
            placeholder: includeOff ? '0, 8, 12, or 16' : '8, 12, or 16'
        }).then(function(input) {
            if (input === null) return null;
            return parseInt(input, 10);
        });
    }
    return Promise.resolve(null);
}

function setPropagationHosting(enabled, cost) {
    var currentCost = propagationStatus.local_node_stamp_cost || 16;
    var apply = function() {
        RS.invoke('set_propagation_hosting', {
            args: {
                enabled: !!enabled,
                stamp_cost: cost === undefined ? currentCost : cost
            }
        }).catch(function(err) {
            showToast('Could not update hosted inbox: ' + ((err && err.message) || 'Unknown'),
                'toast-red', 4000);
        });
    };
    var mobile = (typeof isMobile === 'function') ? isMobile() : !!window.__RATSPEAK_MOBILE__;
    if (enabled && mobile && typeof rsConfirm === 'function') {
        rsConfirm({
            message: 'Mobile devices may stop serving Offline Inbox requests when the app is backgrounded. Enable anyway?',
            confirmText: 'Enable'
        }).then(function(ok) {
            if (ok) apply();
            else renderPropagationStatus();
        });
    } else {
        apply();
    }
}

function choosePropagationHostCost() {
    var current = propagationStatus.local_node_stamp_cost || 16;
    stampCostChoice('Hosted Inbox Stamp Cost', current, false).then(function(cost) {
        if (cost === null || isNaN(cost)) return;
        setPropagationHosting(!!propagationStatus.hosting_enabled, Math.max(0, Math.min(32, cost)));
    });
}

function setStampSettings(enforce, cost) {
    var currentCost = propagationStatus.required_stamp_cost || 8;
    RS.invoke('set_stamp_settings', {
        args: {
            enforce: !!enforce,
            required_cost: cost === undefined ? currentCost : cost
        }
    }).catch(function(err) {
        showToast('Could not update stamps: ' + ((err && err.message) || 'Unknown'),
            'toast-red', 4000);
    });
}

function chooseMessageStampCost() {
    var current = propagationStatus.required_stamp_cost || 0;
    stampCostChoice('Message Stamp Cost', current, true).then(function(cost) {
        if (cost === null || isNaN(cost)) return;
        cost = Math.max(0, Math.min(32, cost));
        setStampSettings(cost > 0, cost);
    });
}

function findNodeName(hash) {
    var node = findNode(hash);
    if (node) return node.display_name || 'Inbox ' + hash.substring(0, 8);
    if (!hash) return '';
    return hash.length > 16 ? hash.substring(0, 8) + '...' + hash.substring(hash.length - 4) : hash;
}

function findNode(hash) {
    if (!hash) return '';
    for (var i = 0; i < discoveredRelayNodes.length; i++) {
        if (discoveredRelayNodes[i].hash === hash) {
            return discoveredRelayNodes[i];
        }
    }
    return null;
}

function clearRelayRefreshWatchdog() {
    if (refreshWatchdog) {
        clearTimeout(refreshWatchdog);
        refreshWatchdog = null;
    }
}

function beginRelayRefresh(timeoutMs) {
    refreshInFlight = true;
    clearRelayRefreshWatchdog();
    refreshWatchdog = setTimeout(function() {
        refreshInFlight = false;
        refreshWatchdog = null;
        renderPropagationStatus();
    }, timeoutMs || RELAY_REFRESH_WATCHDOG_MS);
}

function finishRelayRefresh() {
    refreshInFlight = false;
    clearRelayRefreshWatchdog();
    renderPropagationStatus();
}

function applyPropagationMode(mode, opts) {
    var args = { mode: mode };
    if (opts && opts.favor_static !== undefined) args.favorStatic = !!opts.favor_static;
    propagationStatus.mode = mode;
    if (mode === 'off') {
        propagationStatus.connected = false;
    } else if (opts && opts.favor_static !== undefined) {
        propagationStatus.favor_static = !!opts.favor_static;
    }
    renderPropagationStatus();
    RS.invoke('set_propagation_mode', args).catch(function(err) {
        showToast('Could not change Offline Inbox mode: ' + (err && err.message ? err.message : 'Unknown'),
            'toast-red', 4000);
        RS.invoke('api_propagation').then(function(data) {
            propagationStatus = data || propagationStatus;
            renderPropagationStatus();
        }).catch(function() {
            renderPropagationStatus();
        });
    });
}

function selectRelayNode(hash) {
    RS.invoke('set_propagation_node', { hash: hash })
        .then(function() {
            if (propagationStatus.mode !== 'manual') {
                applyPropagationMode('manual');
            }
        })
        .catch(function(err) {
            showPreConditionToast((err && err.message) || 'Invalid Offline Inbox node hash');
        });
    showToast('Connecting to Offline Inbox node…', 'toast-orange', 2000);
}

function setPropagationNode() {
    var input = document.getElementById('prop-node-input');
    var hash = (input.value || '').trim();
    if (!hash || hash.length !== 32 || !/^[0-9a-fA-F]+$/.test(hash)) {
        showPreConditionToast('Enter a valid Offline Inbox node hash (32 hex chars)');
        return;
    }
    selectRelayNode(hash);
}

function clearPropagationNode() {
    RS.invoke('set_propagation_node', { hash: '' }).catch(function() {});
    showToast('Disconnected from Offline Inbox node', 'toast-blue', 2000);
}

function syncPropagationMailbox() {
    var btn = document.getElementById('prop-sync-btn');
    if (btn) {
        btn.disabled = true;
        btn.textContent = 'Checking...';
        setTimeout(function() {
            btn.disabled = false;
            btn.textContent = 'Check Now';
        }, 5000);
    }
    RS.invoke('sync_propagation').catch(function() {});
}

function refreshRelayNodes() {
    if (refreshInFlight) return;
    beginRelayRefresh(RELAY_REFRESH_WATCHDOG_MS);
    var btn = document.getElementById('prop-refresh-btn');
    if (btn) {
        btn.disabled = true;
        btn.textContent = 'Refreshing…';
    }
    RS.invoke('refresh_propagation_nodes').then(function(outcome) {
        outcome = outcome || { kind: 'sent', count: 0 };
        if (outcome.kind === 'offline') {
            showToast('No transport connection — connect to a hub interface first.',
                'toast-red', 4000);
        } else if (outcome.kind === 'throttled') {
            showToast('Refresh throttled — try again in a few seconds.',
                'toast-blue', 3000);
        }
        // Re-pull so newly-arrived nodes appear without waiting for the
        // ~4s propagation_update follow-up emit.
        return RS.invoke('api_propagation_nodes');
    }).then(function(data) {
        if (Array.isArray(data)) {
            discoveredRelayNodes = data;
        }
        finishRelayRefresh();
    }).catch(function() {
        finishRelayRefresh();
    });
}

RS.listen('propagation_update', function(data) {
    propagationStatus = data || propagationStatus;
    refreshInFlight = false;
    clearRelayRefreshWatchdog();
    renderPropagationStatus();
    var relayDot = document.getElementById('settings-relay-dot');
    if (relayDot) {
        relayDot.className = 'panel-header-dot' + (propagationStatus.connected ? ' connected' : '');
    }
    var relayBadge = document.getElementById('settings-relay-status');
    if (relayBadge) {
        var modeLabel = propagationStatus.mode || 'auto';
        if (propagationStatus.mode === 'off') {
            relayBadge.textContent = 'Off';
            relayBadge.className = 'settings-relay-badge';
        } else if (propagationStatus.connected) {
            relayBadge.textContent = (modeLabel === 'auto' ? 'Auto: ' : '') + 'Ready';
            relayBadge.className = 'settings-relay-badge connected';
        } else if (propagationStatus.auto_active_node ||
            (propagationStatus.mode === 'manual' && propagationStatus.propagation_node)) {
            relayBadge.textContent = modeLabel === 'auto' ? 'Auto selected' : 'Selected';
            relayBadge.className = 'settings-relay-badge';
        } else {
            relayBadge.textContent = 'Finding inbox';
            relayBadge.className = 'settings-relay-badge';
        }
    }
});

RS.listen('propagation_refresh_started', function(data) {
    var count = (data && data.count) || 0;
    beginRelayRefresh(Math.max(RELAY_REFRESH_WATCHDOG_MS, 8000 + count * 2000));
    var btn = document.getElementById('prop-refresh-btn');
    if (btn) {
        btn.disabled = true;
        btn.textContent = 'Requesting ' + count + ' path(s)…';
    }
});

RS.listen('propagation_error', function(data) {
    showToast('Offline Inbox error: ' + (data.error || 'Unknown'), 'toast-red', 4000);
});

RS.listen('propagation_sync_result', function(data) {
    if (data && (data.success || data.ok)) {
        if (data.started) {
            showToast(data.message || 'Offline Inbox check started', 'toast-blue', 3000);
            return;
        }
        var count = data.downloaded || 0;
        if (count > 0) {
            showToast(count + ' message' + (count > 1 ? 's' : '') + ' downloaded from Offline Inbox', 'toast-green', 4000);
        } else {
            showToast(data.message || 'No new messages in Offline Inbox', 'toast-blue', 3000);
        }
    } else {
        showToast('Offline Inbox check failed: ' + ((data && (data.error || data.message)) || 'Unknown'), 'toast-red', 4000);
    }
    var btn = document.getElementById('prop-sync-btn');
    if (btn) {
        btn.disabled = false;
        btn.textContent = 'Check Now';
    }
});

RS.invoke('api_propagation').then(function(data) {
    propagationStatus = data || propagationStatus;
    return RS.invoke('api_propagation_nodes');
}).then(function(data) {
    if (Array.isArray(data)) {
        discoveredRelayNodes = data;
    }
    renderPropagationStatus();
}).catch(function() {
    renderPropagationStatus();
});
