// Settings › Offline Inbox: Off / Automatic / Manual, the Ratspeak-inbox preference,
// the inbox in use with Check now, inboxes nearby, hosting, and stamp protection.

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
    var kit = window.RS && RS.kit && RS.kit.time;
    var full = kit ? kit.full(ts) : (isNaN(date.getTime()) ? '' : date.toLocaleString());
    var words = kit ? kit.sentence(ts) : formatLastHeard(ts);
    return '<time datetime="' + escapeHtml(iso) + '" title="' + escapeHtml(full) + '">' + escapeHtml(words) + '</time>';
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
            return { dot: 'status-reachable', line1: word('Working') + ' · checked ' + inboxTime(node.last_sync_success), line2: stored, busy: false };
        }
        return { dot: 'status-stale', line1: word('Last checked') + ' ' + inboxTime(node.last_sync_success), line2: stored, busy: false };
    }
    return { dot: 'status-unknown', line1: word('Not checked yet'), line2: stored, busy: false };
}

function inboxStatusLines(state) {
    return '<span class="ui-status">' + inboxDot(state) + '<span>' + state.line1 + '</span></span>' +
        (state.line2 ? '<span class="settings-inbox-sub">' + state.line2 + '</span>' : '');
}

function inboxStatusHtml(state) {
    return '<span class="settings-inbox-status" id="inbox-status-lines">' + inboxStatusLines(state) + '</span>';
}

// The card's state as a kit status dot; the words beside it carry the meaning.
var INBOX_DOT_TONES = {
    'status-reachable': ' ui-dot--online',
    'status-checking': ' ui-dot--online ui-dot--pulse',
    'status-stale': ' ui-dot--warning',
    'status-unreachable': ' ui-dot--error'
};

function inboxDotClass(state) {
    return 'ui-dot ' + state.dot + (INBOX_DOT_TONES[state.dot] || '');
}

function inboxDot(state) {
    return '<span class="' + inboxDotClass(state) + '" id="inbox-status-dot" aria-hidden="true"></span>';
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

function propSwitch(id, label, desc, checked, extra) {
    return '<div class="ui-prop"' + (extra || '') + '><span class="ui-prop-text"><span class="ui-prop-label" id="' + id + '-label">' + escapeHtml(label) + '</span>' +
        (desc ? '<span class="ui-prop-desc" id="' + id + '-desc">' + escapeHtml(desc) + '</span>' : '') + '</span>' +
        '<label class="ui-switch"><input type="checkbox" role="switch" id="' + id + '"' + (checked ? ' checked' : '') +
        ' aria-labelledby="' + id + '-label"' + (desc ? ' aria-describedby="' + id + '-desc"' : '') + '><span class="track"></span></label></div>';
}

function propTitle(text) {
    return '<h3 class="ui-section-title">' + escapeHtml(text) + '</h3>';
}

var INBOX_MODE_DESC = {
    off: 'Messages wait on the sender’s device until you’re reachable.',
    auto: 'Automatic picks a reachable inbox for you.',
    manual: 'You choose the inbox.'
};

function renderPropagationStatus(targetId) {
    var container = document.getElementById(targetId || 'settings-propagation-status');
    if (!container) return;

    var mode = propagationStatus.mode || 'auto';
    var favorStatic = propagationStatus.favor_static !== false;
    var nodeHash = propagationStatus.propagation_node || null;
    var autoActive = propagationStatus.auto_active_node || null;
    var selectedNodeHash = mode === 'auto' ? autoActive : nodeHash;
    var awaiting = !!propagationStatus.awaiting_discovery;

    var html = '<div class="ui-group">' +
        '<div class="ui-prop settings-prop-wrap"><span class="ui-prop-text"><span class="ui-prop-label" id="prop-mode-label">Use an Offline Inbox</span>' +
            '<span class="ui-prop-desc">' + escapeHtml(INBOX_MODE_DESC[mode] || '') + '</span></span>' +
            '<div class="ui-seg" role="radiogroup" id="prop-mode-seg" aria-labelledby="prop-mode-label">' +
                modeBtnHtml('off', 'Off', mode === 'off') +
                modeBtnHtml('auto', 'Automatic', mode === 'auto') +
                modeBtnHtml('manual', 'Manual', mode === 'manual') +
            '</div></div>' +
        (mode === 'auto' ? renderFavorStatic(favorStatic) : '') +
    '</div>';

    if (mode === 'auto') {
        html += renderAutoBody(autoActive, favorStatic, awaiting);
    } else if (mode === 'manual') {
        html += renderManualBody(nodeHash);
        html += renderDiscoveredList(mode, favorStatic, selectedNodeHash);
    }

    if (mode !== 'off') html += renderDeferredDownloads();
    html += renderHostingSettings();
    html += renderStampSettings();

    // A redraw must not take what the user is doing: typed text or focus.
    var input = document.getElementById('prop-node-input');
    var typed = input && input.value !== '' ? input.value : null;
    var focused = document.activeElement && container.contains(document.activeElement)
        ? document.activeElement.id : '';

    container.innerHTML = html;
    lastInboxRenderKey = inboxRenderKey(propagationStatus);
    wireUpHandlers(container, mode);

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
    dot.className = inboxDotClass(state);
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
    return '<button type="button" role="radio" aria-checked="' + (active ? 'true' : 'false') + '" data-mode="' + value + '">' +
        escapeHtml(label) + '</button>';
}

function renderFavorStatic(favorStatic) {
    return propSwitch('prop-favor-static-toggle', 'Prefer Ratspeak inboxes',
        'Uses another reachable inbox when these can’t be reached.', favorStatic, ' id="prop-favor-static-row"');
}

// Two arrows in a circle; a spinner takes its place while a check runs.
var INBOX_CHECK_ICON = '<svg class="ui-ico" viewBox="0 0 24 24" aria-hidden="true" focusable="false">' +
    '<polyline points="23 4 23 10 17 10"/><polyline points="1 20 1 14 7 14"/>' +
    '<path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15"/></svg>';

/** The inbox in use: its name and status, with Check now beside them. `title` is escaped HTML. */
function inboxCardTop(title, state) {
    return '<div class="ui-prop settings-inbox-row"><span class="ui-prop-text"><span class="ui-prop-label">' + title + '</span>' +
            inboxStatusHtml(state) + '</span>' +
        '<button type="button" class="ui-icon-btn" id="prop-sync-btn" aria-label="Check now" title="Check now"' +
            (state.busy ? ' disabled' : '') + '>' + INBOX_CHECK_ICON + '</button>' +
    '</div>';
}

function inboxAddressRow(hash) {
    var copy = window.RS && RS.kit ? RS.kit.hash.copy(hash, { noun: 'Inbox address' }) : '<span class="ui-hash">' + escapeHtml(hash) + '</span>';
    return '<div class="ui-prop"><span class="ui-prop-text"><span class="ui-prop-label">Inbox address</span>' + copy + '</span></div>';
}

function renderAutoBody(autoActive, favorStatic, awaiting) {
    var html = propTitle('In use');
    if (autoActive) {
        var activeNode = findNode(autoActive);
        var nodeName = activeNode ? (activeNode.display_name || findNodeName(autoActive)) : findNodeName(autoActive);
        var autoState = inboxCardState(propagationStatus, Date.now());
        var fallback = activeNode && !activeNode.static && favorStatic;
        html += '<div class="ui-group">' + inboxCardTop(escapeHtml(nodeName) + (fallback ? ' <span class="settings-inbox-note">(fallback)</span>' : ''), autoState) +
            inboxAddressRow(autoActive) + '</div>';
    } else {
        html += '<div class="ui-group"><div class="ui-prop"><span class="ui-prop-text"><span class="ui-prop-desc">' +
            (awaiting
                ? 'Looking for an Offline Inbox. Ratspeak inboxes are checked carefully to save network traffic; Automatic uses another reachable inbox if one appears first.'
                : 'Looking for a reachable Offline Inbox…') +
        '</span></span></div></div>';
    }
    return html;
}

function renderManualBody(nodeHash) {
    var html = propTitle('Your inbox');
    if (nodeHash) {
        var manualState = inboxCardState(propagationStatus, Date.now());
        html += '<div class="ui-group">' + inboxCardTop(escapeHtml(findNodeName(nodeHash)), manualState) + inboxAddressRow(nodeHash) +
            '<button type="button" class="ui-prop ui-prop--danger" id="prop-clear-btn"><span class="ui-prop-text"><span class="ui-prop-label">Stop using this inbox</span></span></button>' +
        '</div>';
    } else {
        html += '<div class="ui-group"><div class="ui-prop"><span class="ui-prop-text"><span class="ui-prop-desc">Choose an inbox nearby, or paste its address.</span></span></div></div>';
    }
    return html;
}

function renderDiscoveredList(mode, favorStatic, activeHash) {
    var html = '<div class="settings-sec">' + propTitle('Inboxes nearby') +
        '<button type="button" class="ui-icon-btn ui-icon-btn--sm" id="prop-refresh-btn" aria-label="Look again" title="Look again"' +
            (refreshInFlight ? ' disabled aria-busy="true">' + '<span class="ui-spin" aria-hidden="true"></span>' : '>' + INBOX_CHECK_ICON) + '</button></div>';

    var rows = [];
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

    if (!visible.length) {
        rows.push('<div class="ui-prop"><span class="ui-prop-text"><span class="ui-prop-desc">No inboxes found yet. Look again, or wait for them to announce.</span></span></div>');
    }
    visible.forEach(function(node) {
        var isActive = activeHash && node.hash === activeHash;
        var hopLabel;
        if (node.hops === 0) {
            hopLabel = 'Direct';
        } else if (node.hops === null || node.hops === undefined) {
            hopLabel = node.static_status === 'probing' ? 'Checking' : 'Not reached';
        } else {
            hopLabel = node.hops + (node.hops === 1 ? ' hop' : ' hops');
        }
        var name = node.display_name || ('Inbox ' + (node.hash || '').substring(0, 8));
        var action = isActive
            ? '<span class="ui-prop-value">In use</span>'
            : (mode === 'manual'
                ? '<button type="button" class="ui-btn ui-btn--sm relay-select-btn" data-hash="' + escapeHtml(node.hash) + '" aria-label="Use ' + escapeHtml(name) + '">Use</button>'
                : '');
        rows.push('<div class="ui-prop relay-node-row" data-hash="' + escapeHtml(node.hash) + '"><span class="ui-prop-text">' +
            '<span class="ui-prop-label relay-node-name" title="' + escapeHtml(name) + '">' + escapeHtml(name) + '</span>' +
            '<span class="ui-prop-desc">' + (node.static ? 'Ratspeak inbox' : 'Inbox') + ' · ' + hopLabel + '</span></span>' + action + '</div>');
    });

    rows.push('<div class="ui-prop settings-prop-stack"><span class="ui-prop-text"><label class="ui-prop-label" for="prop-node-input">Inbox address</label></span>' +
        '<div class="settings-inline-field"><input type="text" id="prop-node-input" class="ui-input ui-input--mono" placeholder="Paste an inbox address" ' +
            'autocomplete="off" autocapitalize="none" spellcheck="false">' +
        '<button type="button" class="ui-btn" id="prop-set-btn">Use</button></div></div>');
    html += '<div class="ui-group">' + rows.join('') + '</div>';

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
        html += '<p class="ui-group-foot">' + footnotes.map(escapeHtml).join('<br>') + '</p>';
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
    var lines = ['<b>' + (paused ? 'Inbox downloads paused' : 'Some messages are waiting') + '</b>'];
    if (rejected) lines.push('Last check: ' + committed + ' new message' + (committed === 1 ? '' : 's') + ' saved; ' + rejected + ' could not be saved.');
    if (suppressed) lines.push(suppressed + ' message' + (suppressed === 1 ? '' : 's') + ' could not be accepted. They remain on the inbox node and will be checked again when your keys or settings change.');
    if (deferred) lines.push(deferred + ' message' + (deferred === 1 ? '' : 's') + ' will be retried after a temporary problem.');
    if (propagationStatus.suppression_full) lines.push('The retry list is full. New downloads will resume when entries expire or you retry them.');
    else if (paused) lines.push('Local storage is being checked. Messages stay on the inbox node until they can be saved safely.');
    var next = Number(propagationStatus.next_retry);
    if (Number.isFinite(next) && next > 0) {
        var date = new Date(next * 1000);
        if (!isNaN(date.getTime())) lines.push('Next retry: ' + escapeHtml(date.toLocaleString()));
    }
    return '<div class="ui-notice ui-notice--warning settings-inbox-notice">' +
        '<svg class="ui-ico" viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>' +
        '<div class="ui-notice-body">' + lines.map(function(line) { return '<p>' + line + '</p>'; }).join('') + '</div>' +
        '<button type="button" class="ui-btn ui-btn--sm" id="prop-retry-deferred-btn">Retry waiting messages</button></div>';
}

function renderHostingSettings() {
    // Inbox hosting needs an always-available key; a hardware (PIV) identity can
    // be unplugged or its session can lock, so hosting isn't offered for it.
    var ai = (typeof activeIdentity === 'function') ? activeIdentity() : null;
    if (ai && ai.is_hardware) {
        return propTitle('Host an inbox') + '<div class="ui-group"><div class="ui-prop"><span class="ui-prop-text">' +
            '<span class="ui-prop-label">Host an Offline Inbox</span>' +
            '<span class="ui-prop-desc">Not available for hardware-key identities: a hosted inbox would stop whenever the key is removed or its session locks.</span>' +
        '</span></div></div>';
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

    var rows = [propSwitch('prop-host-toggle', 'Host an Offline Inbox', mobile
        ? 'Phones host poorly while Ratspeak is in the background.'
        : 'Stores messages for other people on this device.', enabled)];

    if (enabled && !effective) {
        rows.push('<div class="ui-prop"><span class="ui-prop-text"><span class="ui-prop-label">Hosting unavailable</span>' +
            '<span class="ui-prop-desc">' + escapeHtml(host.reason || (readiness === 'opening' || readiness === 'recovering' ? 'Preparing inbox storage…' : 'Inbox storage is not ready.')) + '</span>' +
            '<span class="ui-prop-desc">Identity: ' + escapeHtml(host.identity || 'Current identity') + '</span></span>' +
            (readiness !== 'opening' && readiness !== 'recovering'
                ? '<button type="button" class="ui-btn ui-btn--sm" id="prop-host-retry-btn">Retry storage recovery</button>' : '') +
        '</div>');
    }
    if (effective) {
        rows.push('<div class="ui-prop"><span class="ui-prop-text"><span class="ui-prop-label">Your inbox</span>' +
            (nodeHash && window.RS && RS.kit ? RS.kit.hash.copy(nodeHash, { noun: 'Inbox address' }) : '') +
            '<span class="ui-prop-desc">' + count + ' stored message' + (count === 1 ? '' : 's') + '</span></span>' +
            '<button type="button" class="ui-btn ui-btn--sm" id="prop-host-announce-btn">Announce</button></div>');
        rows.push('<div class="ui-prop"><span class="ui-prop-text"><span class="ui-prop-label">Stamp cost</span>' +
            '<span class="ui-prop-desc">Work senders do before your inbox stores a message.</span></span>' +
            '<button type="button" class="ui-select ui-select--inline" id="prop-host-cost-btn" aria-haspopup="menu" aria-expanded="false" aria-label="Stamp cost: ' + cost + '">' +
                '<span class="v">' + cost + '</span>' + (window.RS && RS.kit ? RS.kit.icon('chevron-down') : '') + '</button></div>');
        rows.push('<button type="button" class="ui-prop" id="prop-host-import-btn"><span class="ui-prop-text"><span class="ui-prop-label">Recover archived messages</span></span>' +
            (window.RS && RS.kit ? RS.kit.icon('chevron-right') : '') + '</button>');
    }

    var html = propTitle('Host an inbox') + '<div class="ui-group">' + rows.join('') + '</div>';
    if (host.quarantined_files > 0) {
        html += '<p class="ui-group-foot">' + Number(host.quarantined_files) + ' archived file' + (host.quarantined_files === 1 ? '' : 's') +
            ' preserved (' + Number(host.quarantined_bytes || 0).toLocaleString() + ' bytes). These files are not shared because their original encrypted form could not be confirmed.</p>';
    }
    return html;
}

function renderStampSettings() {
    var enforce = !!propagationStatus.enforce_stamps;
    var cost = propagationStatus.required_stamp_cost || 0;
    var label = enforce && cost > 0 ? ('Cost ' + cost) : 'Off';
    return propTitle('Stamp protection') + '<div class="ui-group">' +
        propSwitch('stamp-enforce-toggle', 'Require stamps', 'Senders do a little work first, which stops spam.', enforce) +
        '<div class="ui-prop"' + (enforce ? '' : ' hidden') + '><span class="ui-prop-text"><span class="ui-prop-label">Required work</span>' +
            '<span class="ui-prop-desc">More work stops more spam, but slows senders down.</span></span>' +
            '<button type="button" class="ui-select ui-select--inline" id="stamp-cost-btn" aria-haspopup="menu" aria-expanded="false" aria-label="Required work: ' + escapeHtml(label) + '">' +
                '<span class="v">' + escapeHtml(label) + '</span>' + (window.RS && RS.kit ? RS.kit.icon('chevron-down') : '') + '</button></div>' +
    '</div>';
}

function wireUpHandlers(container, mode) {
    container.querySelectorAll('#prop-mode-seg [data-mode]').forEach(function(btn) {
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

function stampCostChoice(title, current, includeOff, anchor, onPick) {
    var choices = includeOff ? [{ label: 'Off', value: 0, hint: 'Don’t require any work.' }] : [];
    choices = choices.concat([
        { label: 'Cost 8', value: 8, hint: 'Light protection.' },
        { label: 'Cost 12', value: 12, hint: 'Balanced protection.' },
        { label: 'Cost 16', value: 16, hint: 'The Offline Inbox default.' }
    ]);
    if (!anchor || !window.RS || !RS.kit || typeof RS.kit.menu !== 'function') return;
    RS.kit.menu(anchor, {
        title: title,
        items: choices.map(function(c) {
            return { label: c.label, hint: c.hint, checked: c.value === current, onSelect: function() { onPick(c.value); } };
        })
    });
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
            title: 'Host an inbox on this phone?',
            message: 'Phones can stop serving Offline Inbox requests while Ratspeak is in the background.',
            confirmText: 'Host anyway'
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
    stampCostChoice('Stamp cost', current, false, document.getElementById('prop-host-cost-btn'), function(cost) {
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
    stampCostChoice('Required work', current, true, document.getElementById('stamp-cost-btn'), function(cost) {
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
