// Centralized values used across dashboard modules.

var RS = window.RS || {};

RS.config = {
    TOAST_DURATION: 3000,
    TOAST_ERROR_DURATION: 5000,
    TOAST_CRITICAL_DURATION: 8000,
    TOAST_FADE_MS: 350,

    CONNECTIONS_THROTTLE: 5000,
    CONTACT_STATUS_POLL: 30000,
    CONNECTION_TIMEOUT: 30000,
    VOICE_PLAYBACK_START_TIMEOUT: 2000,

    DEBOUNCE_SEARCH: 150,
    DEBOUNCE_MESSAGE_SEARCH: 300,
    DEBOUNCE_RESIZE: 200,

    TRANSITION_FALLBACK: 350,

    SERVER_RESTART_DELAY: 6000,

    CONNECTION_SUMMARY_DELAY: 10000,

    // Seconds, not ms.
    STATUS_EVENT_THROTTLE_S: 280,

    FIRST_RUN_TOOLTIP_DELAY: 2000,

    MAX_EVENTS: 200,
    MAX_ANNOUNCES: 200,
    MAX_ANNOUNCES_DISPLAY: 50,
    MAX_INTERFACE_HISTORY: 60,

    MOBILE_BREAKPOINT: 768,
    MOBILE_TOUCH_BREAKPOINT: 1024,
};

// Gesture thresholds consumed by gestures.js / view_stack.js.
RS.gestures = {
    EDGE_ZONE_PX: 40,
    EDGE_MARGIN_TAB_SWIPE_PX: 30,
    SWIPE_VELOCITY_PX_MS: 0.3,
    SWIPE_DISTANCE_PX: 60,
    SWIPE_DISTANCE_DRILLBACK_PX: 80,
    SWIPE_DISTANCE_CONV_DELETE_PX: 100,
    SWIPE_DISTANCE_TOAST_DISMISS_PX: 40,
    SWIPE_DWELL_SIDEBAR_OPEN_MS: 200,
    LONG_PRESS_BOTTOM_BAR_MS: 1350,
    LONG_PRESS_BOTTOM_BAR_DELAY_MS: 150,
    LONG_PRESS_GAMES_ROW_MS: 500,
    LONG_PRESS_SEND_MS: 500,
    LONG_PRESS_MOVE_CANCEL_PX: 20,
    DRAG_DISMISS_THRESHOLD_PX: 60,
    DRAG_DISMISS_OPACITY_DENOM_PX: 300,
    DRAG_DISMISS_EXIT_MIN_MS: 140,
    DRAG_DISMISS_EXIT_MAX_MS: 260,
    DRAG_DISMISS_SETTLE_MS: 220,
    PULL_TO_REFRESH_DISTANCE_PX: 60,
    PULL_TO_REFRESH_RUBBER_BAND_FACTOR: 3,
    PULL_TO_REFRESH_MIN_MS: 600,
    PULL_TO_REFRESH_SUCCESS_MS: 300,
    RIPPLE_DURATION_MS: 400,
    RIPPLE_SELECTORS: [
        '.ui-tab', '.bottom-sheet-item',
        '.nr-btn', '.contacts-row', '.conn-row', '.conn-card'
    ],
    RIPPLE_HAPTIC_SELECTORS: [
        '.ui-tab', '.bottom-sheet-item', '.nr-btn'
    ],
    DRILL_DOWN_VIEWS: ['identity', 'network', 'settings', 'eventlog', 'propagation'],
    HAPTIC_DURATION_MAP: { light: 10, medium: 20, heavy: 30, success: 15, warning: 25, error: 40, selection: 8 }
};

// One bottom-sheet shell: create the overlay+sheet pair, animate open after a
// layout flush, animate close and remove after the CSS transition (0.3s).
// Rich dialogs (dialogs.js) layer their chrome on top; simple sheets
// (contact card, games) use it directly.
RS.sheetShell = {
    create: function(opts) {
        opts = opts || {};
        var overlay = document.createElement('div');
        overlay.className = 'bottom-sheet-overlay' + (opts.overlayClass ? ' ' + opts.overlayClass : '');
        var sheet = document.createElement('div');
        // Marker class is intentionally style-free. Native Back can find
        // custom shells (for example the centered QR scanner) without forcing
        // the visual rules of .bottom-sheet onto them.
        sheet.className = 'rs-sheet-shell ' + (opts.sheetClass || 'bottom-sheet');
        var shell = { overlay: overlay, sheet: sheet };
        // Native Back needs one common escape hatch for sheets that are built
        // at runtime. Rich dialogs replace this with their state-aware close.
        sheet._ratspeakDismiss = function() {
            RS.sheetShell.dismiss(shell);
        };
        return shell;
    },
    present: function(shell) {
        document.body.appendChild(shell.overlay);
        document.body.appendChild(shell.sheet);
        // Force layout flush so the .open transition runs from closed state.
        shell.sheet.offsetHeight;
        shell.overlay.classList.add('active');
        shell.sheet.classList.add('open');
    },
    dismiss: function(shell, done) {
        if (shell.sheet) shell.sheet.classList.remove('open');
        if (shell.overlay) {
            shell.overlay.classList.remove('active');
            shell.overlay.classList.add('closing');
        }
        setTimeout(function() {
            if (shell.overlay && shell.overlay.parentNode) shell.overlay.remove();
            if (shell.sheet && shell.sheet.parentNode) shell.sheet.remove();
            if (typeof done === 'function') done();
        }, 300);
    }
};

// Invoke a backend command for a user-initiated action; failures surface as a
// toast instead of vanishing in a silent .catch. The rejection is swallowed
// (callers are fire-and-forget; chained .then handlers see undefined).
RS.invokeOrToast = function(command, args, failMessage) {
    return RS.invoke(command, args).catch(function(err) {
        // A locked identity answers with its PIN sheet, not a toast.
        if (err && err.code === 'locked') return undefined;
        var message = failMessage || 'Action failed';
        if (typeof showToast === 'function') {
            var copy = typeof toastErrorCopy === 'function'
                ? toastErrorCopy(err, message)
                : message;
            showToast(copy, 'toast-error', 3500);
        }
        return undefined;
    });
};

// Rows a capped group shows when no search is active; search reaches every peer.
RS.PEERS_OLDER_DISPLAY_CAP = 300;

RS.newestPeerItems = function(items, cap) {
    if (!(cap > 0) || items.length <= cap) return items;
    var ranked = items.map(function(c, i) {
        return { i: i, t: c.last_seen != null ? c.last_seen : -Infinity };
    });
    ranked.sort(function(a, b) { return (b.t - a.t) || (a.i - b.i); });
    var keep = {};
    for (var k = 0; k < cap; k++) keep[ranked[k].i] = true;
    return items.filter(function(c, i) { return keep[i]; });
};

// The Active tier (heard in the last 2 hours): the Peers view's Active groups and Network's Active peers.
RS.peerIsActive = function(c) { return c.status === 'reachable' || c.status === 'direct'; };

// Shared Peers/Connections grouping: same buckets, labels, and collapse
// semantics in both lists. Row markup and virtualization stay view-specific
// (peers uses variable-height profile rows; connections a fixed-height table).
// `opts.olderDisplayCap` limits the not-heard-recently group to its newest
// rows (in the list's own order), followed by a `cap` item.
RS.buildPeerGroupItems = function(peers, collapsedGroups, hasName, opts) {
    var olderCap = opts && opts.olderDisplayCap;
    var contactItems = [], onlineItems = [], onlineStarItems = [], staleItems = [], offlineItems = [];
    peers.forEach(function(c) {
        if (c.is_contact) contactItems.push(c);
        else if (RS.peerIsActive(c)) {
            if (hasName(c)) onlineItems.push(c);
            else onlineStarItems.push(c);
        } else if (c.status === 'stale') staleItems.push(c);
        else offlineItems.push(c);
    });

    var flatItems = [];
    [
        { items: contactItems, key: 'contacts', label: 'Contacts' },
        { items: onlineItems, key: 'online', label: 'Active' },
        { items: onlineStarItems, key: 'online_star', label: 'Active, unnamed' },
        { items: staleItems, key: 'stale', label: 'Seen recently' },
        { items: offlineItems, key: 'offline', label: 'Older / unknown', cap: olderCap }
    ].forEach(function(g) {
        if (g.items.length > 0) {
            flatItems.push({ type: 'header', group: g.key, label: g.label, count: g.items.length });
            if (!collapsedGroups[g.key]) {
                var shown = RS.newestPeerItems(g.items, g.cap);
                shown.forEach(function(c) { flatItems.push({ type: 'row', data: c }); });
                if (shown.length < g.items.length) {
                    flatItems.push({
                        type: 'cap',
                        group: g.key,
                        shown: shown.length,
                        total: g.items.length,
                        label: 'Showing the ' + shown.length.toLocaleString('en-US') + ' most recent of ' +
                            g.items.length.toLocaleString('en-US') + '.'
                    });
                }
            }
        }
    });
    return flatItems;
};
