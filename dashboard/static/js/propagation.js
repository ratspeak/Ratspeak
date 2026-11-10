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

var INBOX_IN_FLIGHT_STATES = ['LinkEstablishing', 'LinkEstablished', 'ListRequested',
    'GetRequested', 'Receiving', 'PurgeRequested', 'AwaitingAdmission'];
// Three missed automatic checks (every 5 minutes) and "Working" becomes "Last checked".
var INBOX_WORKING_WINDOW_SECS = 900;
var INBOX_CHECK_OPTIMISTIC_MS = 15000;
var INBOX_CLOCK_MS = 30000;
var INBOX_REASON_TEXT = {
    path_unavailable: 'No route found',
    active_relay_path_missing: 'No route found',
    path_probe_timeout: 'No route found',
    link_not_established: "Couldn't connect",
    'link establishment timeout': "Couldn't connect",
    link_closed: 'Connection dropped',
    'link closed': 'Connection dropped',
    proof_timed_out: 'No confirmation',
    'delivery timeout': 'No confirmation',
    'direct fallback timeout': 'No confirmation',
    inbox_rejected: 'Refused your message',
    'resource rejected': 'Refused your message',
    'propagation node rejected stamp': 'Refused your message',
    transfer_failed: 'Sending stopped partway',
    resource_failed: 'Sending stopped partway',
    too_large: 'Over the size limit',
    no_interface: 'No network connection',
    sync_failed: 'Check failed',
    sync_failure_threshold: 'Checks keep failing'
};
var inboxCheckRequestedAt = 0;
var inboxClock = null;

function inboxTime(ts) {
    var date = new Date(ts * 1000);
    var iso = isNaN(date.getTime()) ? '' : date.toISOString();
    return '<time datetime="' + escapeHtml(iso) + '" title="' + escapeHtml(isNaN(date.getTime()) ? '' : date.toLocaleString()) + '">' +
        escapeHtml(formatLastHeard(ts)) + '</time>';
}

/** The Offline Inbox card's state, first match wins (issue 79 spec A1). */
function inboxCardState(status, nowMs) {
    var node = status.active_node || null;
    var cs = status.client_state;
    var offline = status.network_available === false ||
        (status.network_available == null && typeof _anyInterfaceOnline !== 'undefined' && _anyInterfaceOnline === false);
    var stored = node && node.last_deposit_success ? 'Stored your last message ' + inboxTime(node.last_deposit_success) : '';
    var worked = node && node.last_success ? 'Last worked ' + inboxTime(node.last_success) : '';
    var word = function(text) { return '<span class="relay-status-word">' + text + '</span>'; };
    if (offline) {
        return { dot: 'status-offline', line1: word('Offline') + ' · Checks resume when you reconnect', line2: '', busy: false };
    }
    var progress = Number(status.transfer_progress);
    if (cs === 'Receiving' && progress > 0 && progress < 1) {
        var percent = Math.min(99, Math.max(1, Math.round(progress * 100)));
        return { dot: 'status-checking', line1: word('Receiving messages') + ' · ' + percent + '%', line2: '', busy: true };
    }
    if (INBOX_IN_FLIGHT_STATES.indexOf(cs) !== -1 || nowMs - inboxCheckRequestedAt < INBOX_CHECK_OPTIMISTIC_MS) {
        return { dot: 'status-checking', line1: word('Checking now…'), line2: '', busy: true };
    }
    if (!node || (!(node.last_seen > 0) && !node.last_success)) {
        return { dot: 'status-unknown', line1: word('Not heard on the network yet'), line2: '', busy: false };
    }
    if (node.last_failure_reason === 'sync_partial') {
        return { dot: 'status-stale', line1: word('Last check incomplete'), line2: worked, busy: false };
    }
    if (node.last_failure_reason || node.path_status === 'failed' || node.path_status === 'missing') {
        var reason = INBOX_REASON_TEXT[node.last_failure_reason];
        return {
            dot: 'status-unreachable',
            line1: word('Not working') + (reason ? ' · ' + escapeHtml(reason) : ''),
            line2: worked,
            busy: false
        };
    }
    if (node.last_sync_success) {
        if (nowMs / 1000 - node.last_sync_success <= INBOX_WORKING_WINDOW_SECS) {
            return { dot: 'status-reachable', line1: word('Working') + ' · Checked ' + inboxTime(node.last_sync_success), line2: stored, busy: false };
        }
        return { dot: 'status-stale', line1: word('Last checked') + ' ' + inboxTime(node.last_sync_success), line2: stored, busy: false };
    }
    return { dot: 'status-unknown', line1: word('Not checked yet'), line2: stored, busy: false };
}

function inboxStatusLines(state) {
    return '<div>' + state.line1 + '</div>' + (state.line2 ? '<div>' + state.line2 + '</div>' : '');
}

function inboxStatusHtml(state) {
    return '<div class="relay-card-details" id="inbox-status-lines">' + inboxStatusLines(state) + '</div>';
}

function inboxDot(state) {
    return '<span class="conn-status-dot ' + state.dot + '" id="inbox-status-dot" aria-hidden="true"></span>';
}

function inboxPanelVisible() {
    var panel = document.getElementById('panel-settings-offline-inbox');
    return !!panel && panel.offsetParent !== null && !document.hidden;
}

// Relative times age, so re-render while the card is on screen, and only then.
function keepInboxClock() {
    if (inboxClock || !inboxPanelVisible()) return;
    inboxClock = setInterval(function() {
        if (!inboxPanelVisible()) {
            clearInterval(inboxClock);
            inboxClock = null;
            return;
        }
        refreshInboxCard();
    }, INBOX_CLOCK_MS);
}

function renderPropagationStatus(targetId) {
    var container = document.getElementById(targetId || 'settings-propagation-status');
    if (!container) return;

    var mode = propagationStatus.mode || 'auto';
    var favorStatic = propagationStatus.favor_static !== false;
    var nodeHash = propagationStatus.propagation_node || null;
    var autoActive = propagationStatus.auto_active_node || null;
    var selectedNodeHash = mode === 'auto' ? autoActive : nodeHash;
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
            'Offline Inbox is disabled. An active sync is canceled, and no new syncs will start.' +
        '</div>';
    } else if (mode === 'auto') {
        html += renderAutoBody(autoActive, favorStatic, awaiting);
    } else if (mode === 'manual') {
        html += renderManualBody(nodeHash);
    }

    if (mode === 'manual') {
        html += renderDiscoveredList(mode, favorStatic, selectedNodeHash);
    }

    if (mode !== 'off') html += renderDeferredDownloads();
    html += renderHostingSettings();
    html += renderStampSettings();

    // A redraw must not take what the user is doing: typed text, open
    // sections, or focus.
    var input = document.getElementById('prop-node-input');
    var typed = input && input.value !== (nodeHash || '') ? input.value : null;
    var focused = document.activeElement && container.contains(document.activeElement)
        ? document.activeElement.id : '';
    var openDetails = [];
    container.querySelectorAll('details[open] > summary').forEach(function(summary) {
        openDetails.push(summary.textContent);
    });

    container.innerHTML = html;
    lastInboxRenderKey = inboxRenderKey(propagationStatus);
    wireUpHandlers(container, mode);

    container.querySelectorAll('details > summary').forEach(function(summary) {
        if (openDetails.indexOf(summary.textContent) !== -1) summary.parentElement.open = true;
    });
    var nextInput = document.getElementById('prop-node-input');
    if (nextInput && typed !== null) nextInput.value = typed;
    var nextFocus = focused ? document.getElementById(focused) : null;
    if (nextFocus && typeof nextFocus.focus === 'function') {
        try { nextFocus.focus({ preventScroll: true }); } catch (_) { nextFocus.focus(); }
    }
    keepInboxClock();
}

// Fields only the selected card shows; a change in any other field redraws
// the whole panel.
var INBOX_CARD_FIELDS = ['client_state', 'sync_state', 'connected', 'transfer_progress',
    'transfer_size', 'transfer_result', 'active_node', 'network_available'];
var lastInboxRenderKey = null;

function inboxRenderKey(status) {
    var rest = {};
    Object.keys(status || {}).sort().forEach(function(key) {
        if (INBOX_CARD_FIELDS.indexOf(key) === -1) rest[key] = status[key];
    });
    return JSON.stringify(rest);
}

/** Updates just the card's dot, status lines, and Check Now; false when the card isn't drawn. */
function refreshInboxCard() {
    var dot = document.getElementById('inbox-status-dot');
    var lines = document.getElementById('inbox-status-lines');
    if (!dot || !lines) return false;
    var state = inboxCardState(propagationStatus, Date.now());
    dot.className = 'conn-status-dot ' + state.dot;
    lines.innerHTML = inboxStatusLines(state);
    var check = document.getElementById('prop-sync-btn');
    if (check) check.disabled = state.busy;
    return true;
}

function updateInboxStatus() {
    if (lastInboxRenderKey === inboxRenderKey(propagationStatus) && refreshInboxCard()) return;
    renderPropagationStatus();
}

function modeBtnHtml(value, label, active) {
    return '<button type="button" class="relay-mode-btn' + (active ? ' relay-mode-btn-active' : '') +
        '" data-mode="' + value + '" style="flex:1;padding:6px 12px;">' +
        escapeHtml(label) +
        '</button>';
}

function renderAutoBody(autoActive, favorStatic, awaiting) {
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
        var autoState = inboxCardState(propagationStatus, Date.now());
        html += '<div class="relay-card relay-card-active">' +
            '<div class="relay-card-header">' +
                inboxDot(autoState) + '<strong>' + escapeHtml(activeLabel) + ': ' + escapeHtml(nodeName) + '</strong>' +
            '</div>' +
            inboxStatusHtml(autoState) +
            '<div class="inline-hint-sm" style="overflow-wrap:anywhere;margin-top:6px;">' +
                'Propagation address<br>' + escapeHtml(autoActive) +
            '</div>' +
            '<div class="relay-card-actions">' +
                '<button class="nr-btn nr-btn-sm" id="prop-sync-btn"' + (autoState.busy ? ' disabled' : '') + '>Check Now</button>' +
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

function renderManualBody(nodeHash) {
    var html = '';
    if (nodeHash) {
        var nodeName = findNodeName(nodeHash);
        var manualState = inboxCardState(propagationStatus, Date.now());
        html += '<div class="relay-card relay-card-active">' +
            '<div class="relay-card-header">' +
                inboxDot(manualState) + '<strong>' + escapeHtml(nodeName) + '</strong>' +
            '</div>' +
            inboxStatusHtml(manualState) +
            '<div class="relay-card-actions">' +
                '<button class="nr-btn nr-btn-sm" id="prop-sync-btn"' + (manualState.busy ? ' disabled' : '') + '>Check Now</button>' +
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
        var name = node.display_name || ('Inbox ' + (node.hash || '').substring(0, 8));
        var nameHtml = escapeHtml(name);
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
            '<span class="relay-node-name" title="' + escapeHtml(name) + '">' + nameHtml + '</span>' +
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

function renderDeferredDownloads() {
    var suppressed = Math.max(0, Number(propagationStatus.suppressed_count) || 0);
    var deferred = Math.max(0, Number(propagationStatus.deferred_count) || 0);
    var paused = !!propagationStatus.admission_paused;
    var rejected = Math.max(0, Number(propagationStatus.rejected_count) || 0);
    var committed = Math.max(0, Number(propagationStatus.committed_count) || 0);
    if (!suppressed && !deferred && !paused && !rejected) return '';
    var html = '<div class="relay-card"><div class="relay-card-header"><strong>' +
        (paused ? 'Inbox downloads paused' : 'Some messages are waiting') + '</strong></div>';
    if (rejected) html += '<div class="settings-row-desc">Last check: ' + committed + ' new message' + (committed === 1 ? '' : 's') + ' saved; ' + rejected + ' could not be saved.</div>';
    if (suppressed) html += '<div class="settings-row-desc">' + suppressed + ' message' + (suppressed === 1 ? '' : 's') + ' could not be accepted. They remain on the inbox node and will be checked again when your keys or settings change.</div>';
    if (deferred) html += '<div class="settings-row-desc">' + deferred + ' message' + (deferred === 1 ? '' : 's') + ' will be retried after a temporary problem.</div>';
    if (propagationStatus.suppression_full) html += '<div class="settings-row-desc">The retry list is full. New downloads will resume when entries expire or you retry them.</div>';
    else if (paused) html += '<div class="settings-row-desc">Local storage is being checked. Messages stay on the inbox node until they can be saved safely.</div>';
    var next = Number(propagationStatus.next_retry);
    if (Number.isFinite(next) && next > 0) {
        var date = new Date(next * 1000);
        if (!isNaN(date.getTime())) html += '<div class="settings-row-desc">Next retry: ' + escapeHtml(date.toLocaleString()) + '</div>';
    }
    html += '<div class="relay-card-actions"><button class="nr-btn nr-btn-sm" id="prop-retry-deferred-btn">Retry waiting messages</button></div></div>';
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
    var enabled = !!propagationStatus.hosting_requested;
    var effective = !!propagationStatus.hosting_enabled;
    var host = propagationStatus.hosting_status || {};
    var readiness = host.readiness || 'disabled';
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

    if (enabled && !effective) {
        html += '<div class="relay-card"><div class="relay-card-header"><strong>Hosting unavailable</strong></div>' +
            '<div class="settings-row-desc">' + escapeHtml(host.reason || (readiness === 'opening' || readiness === 'recovering' ? 'Preparing inbox storage…' : 'Inbox storage is not ready.')) + '</div>' +
            '<div class="settings-row-desc">Identity: ' + escapeHtml(host.identity || 'Current identity') + '</div>';
        if (readiness !== 'opening' && readiness !== 'recovering') {
            html += '<div class="relay-card-actions"><button class="nr-btn nr-btn-sm" id="prop-host-retry-btn">Retry storage recovery</button></div>';
        }
        html += '</div>';
    }
    if (host.quarantined_files > 0) {
        html += '<div class="inline-hint">' + Number(host.quarantined_files) + ' archived file' + (host.quarantined_files === 1 ? '' : 's') +
            ' preserved (' + Number(host.quarantined_bytes || 0).toLocaleString() + ' bytes). These files are not shared because their original encrypted form could not be confirmed.</div>';
    }
    if (effective) {
        html += '<div class="relay-card relay-card-active">' +
            '<div class="relay-card-header"><strong>' + escapeHtml(nodeHash || 'Local inbox node') + '</strong></div>' +
            '<div class="relay-card-details">' +
                '<span>' + count + ' stored message' + (count === 1 ? '' : 's') + '</span>' +
                ' · <span>Stamp cost ' + cost + '</span>' +
            '</div>' +
            '<div class="relay-card-actions">' +
                '<button class="nr-btn nr-btn-sm" id="prop-host-announce-btn">Announce</button>' +
                '<button class="nr-btn nr-btn-sm nr-btn-muted" id="prop-host-cost-btn">Stamp Cost</button>' +
                '<button class="nr-btn nr-btn-sm nr-btn-muted" id="prop-host-import-btn">Recover archived messages</button>' +
            '</div>' +
        '</div>';
    }

    html += '</div>';
    return html;
}

function renderStampSettings() {
    var enforce = !!propagationStatus.enforce_stamps;
    var cost = propagationStatus.required_stamp_cost || 0;
    var label = enforce && cost > 0 ? ('Cost ' + cost) : 'OFF';
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
            '<button class="selector-badge' + (label === 'OFF' ? ' settings-state-value' : '') + '" id="stamp-cost-btn">' + escapeHtml(label) + '</button>' +
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
    var retryDeferredBtn = document.getElementById('prop-retry-deferred-btn');
    if (retryDeferredBtn) retryDeferredBtn.addEventListener('click', async function() {
        retryDeferredBtn.disabled = true;
        try {
            await RS.invoke('retry_deferred_propagation');
            showToast('Waiting messages will be checked again.', 'toast-success', 3000);
            await syncPropagationMailbox();
        } catch (err) {
            showToast('Could not retry messages: ' + ((err && err.message) || String(err)), 'toast-error', 4000);
        } finally { retryDeferredBtn.disabled = false; }
    });

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

    var hostRetryBtn = document.getElementById('prop-host-retry-btn');
    if (hostRetryBtn) hostRetryBtn.addEventListener('click', function() { recoverHostedInbox(false); });
    var hostImportBtn = document.getElementById('prop-host-import-btn');
    if (hostImportBtn) hostImportBtn.addEventListener('click', function() { recoverHostedInbox(true); });

    var hostCostBtn = document.getElementById('prop-host-cost-btn');
    if (hostCostBtn) hostCostBtn.addEventListener('click', choosePropagationHostCost);

    var hostAnnounceBtn = document.getElementById('prop-host-announce-btn');
    if (hostAnnounceBtn) {
        hostAnnounceBtn.addEventListener('click', function() {
            // All manual announce surfaces share the pre-IPC pending guard.
            if (typeof tryTriggerAnnounce === 'function') tryTriggerAnnounce();
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
    var choices = includeOff ? [{ label: 'OFF', value: '0', hint: 'Do not require proof-of-work.' }] : [];
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
                'toast-error', 4000);
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

async function recoverHostedInbox(importManifest) {
    if (typeof rsConfirm !== 'function') return;
    var host = propagationStatus.hosting_status || {};
    var identity = host.identity;
    if (!identity) return;
    var manifest = null;
    var digest = null;
    if (importManifest) {
        if (typeof rsPrompt !== 'function') return;
        manifest = await rsPrompt({ title: 'Recover archived messages', message: 'Enter the full local path to a trusted recovery manifest. It must identify the original encrypted messages; retrying storage alone never releases archived files.', placeholder: 'Recovery manifest path' });
        if (!manifest) return;
        digest = await rsPrompt({ title: 'Verify recovery manifest', message: 'Enter the SHA-256 digest approved when this manifest was independently verified.', placeholder: '64 hexadecimal characters' });
        if (!digest) return;
    }
    var confirmed = await rsConfirm({
        title: 'Recover hosted inbox storage',
        message: 'Stop every other Ratspeak or LXMF relay using this storage and disable its automatic restart. This app will briefly restart messaging. Existing files are preserved; uncertain old messages remain archived. Confirm that the other writers will stay stopped.',
        confirmText: 'Writers stopped — continue'
    });
    if (!confirmed) return;
    try {
        var report = await RS.invoke('recover_propagation_host', { args: {
            identity: identity, supervisor_writers_stopped: true,
            manifest_path: manifest, approved_manifest_sha256: digest
        }});
        showToast('Storage recovered. ' + report.quarantined_files + ' original files preserved; ' + report.imported + ' verified messages imported.', 'toast-success', 6000);
    } catch (error) {
        showToast('Storage recovery could not finish: ' + ((error && error.message) || String(error)), 'toast-error', 6000);
    }
}

function choosePropagationHostCost() {
    var current = propagationStatus.local_node_stamp_cost || 16;
    stampCostChoice('Hosted Inbox Stamp Cost', current, false).then(function(cost) {
        if (cost === null || isNaN(cost)) return;
        setPropagationHosting(!!propagationStatus.hosting_requested, Math.max(0, Math.min(32, cost)));
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
            'toast-error', 4000);
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
            'toast-error', 4000);
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
    RS.invoke('set_propagation_node', { hash: '' }).catch(function(err) {
        showToast((err && err.message) || 'Could not disconnect from the Offline Inbox node',
            'toast-error', 4000);
    });
}

function syncPropagationMailbox() {
    inboxCheckRequestedAt = Date.now();
    refreshInboxCard();
    // Clears the optimistic Checking state if no update arrives.
    setTimeout(refreshInboxCard, INBOX_CHECK_OPTIMISTIC_MS + 50);
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
            showToast('Connect a hub interface before checking Offline Inbox.',
                'toast-warning', 4000);
        } else if (outcome.kind === 'throttled') {
            showToast('Wait a few seconds before checking again.',
                'toast-warning', 3000);
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
    inboxCheckRequestedAt = 0;
    refreshInFlight = false;
    clearRelayRefreshWatchdog();
    updateInboxStatus();
});

document.addEventListener('click', function(event) {
    var item = event.target && event.target.closest
        ? event.target.closest('[data-settings-panel="panel-settings-offline-inbox"]')
        : null;
    if (item) setTimeout(renderPropagationStatus, 0);
});

document.addEventListener('visibilitychange', function() {
    if (!document.hidden && inboxPanelVisible()) renderPropagationStatus();
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
    showToast('Offline Inbox error: ' + (data.error || 'Unknown'), 'toast-error', 4000);
});

RS.listen('propagation_sync_result', function(data) {
    if (!(data && data.started)) inboxCheckRequestedAt = 0;
    if (data && (data.success || data.ok)) {
        if (data.started) {
            return;
        }
        var count = data.downloaded || 0;
        if (count > 0) {
            showToast(count + ' message' + (count > 1 ? 's' : '') + ' downloaded from Offline Inbox', 'toast-success', 4000);
        } else {
            showToast(data.message || 'No new messages in Offline Inbox', 'toast-info', 3000);
        }
    } else {
        showToast('Offline Inbox check failed: ' + ((data && (data.error || data.message)) || 'Unknown'), 'toast-error', 4000);
    }
    refreshInboxCard();
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
