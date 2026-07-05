// Centralized values used across dashboard modules.

var RS = window.RS || {};

// Feature flags. `agentGui` is off for the CLI-first beta: bots are managed via
// ratspeakd/ratspeakctl, not the personal desktop app, and its Tauri command
// surface is gated behind the `agent-gui` build feature. Keep this in sync with
// that feature when re-enabling the in-app agent UX.
RS.FEATURES = RS.FEATURES || { agentGui: false };

RS.config = {
    TOAST_DURATION: 3000,
    TOAST_ERROR_DURATION: 5000,
    TOAST_CRITICAL_DURATION: 8000,
    TOAST_FADE_MS: 350,

    CONNECTIONS_THROTTLE: 5000,
    CONTACT_STATUS_POLL: 30000,
    CONNECTION_TIMEOUT: 30000,

    DEBOUNCE_SEARCH: 150,
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
    PULL_TO_REFRESH_DISTANCE_PX: 60,
    PULL_TO_REFRESH_RUBBER_BAND_FACTOR: 3,
    PULL_TO_REFRESH_MIN_MS: 600,
    PULL_TO_REFRESH_SUCCESS_MS: 300,
    RIPPLE_DURATION_MS: 400,
    RIPPLE_SELECTORS: [
        '.bottom-bar-item', '.bottom-sheet-item', '.nav-item',
        '.nr-btn', '.conv-row', '.contacts-row', '.conn-row', '.conn-card'
    ],
    RIPPLE_HAPTIC_SELECTORS: [
        '.bottom-bar-item', '.bottom-sheet-item', '.nav-item', '.nr-btn'
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
        sheet.className = opts.sheetClass || 'bottom-sheet';
        return { overlay: overlay, sheet: sheet };
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
        var detail = err && err.message ? err.message : (typeof err === 'string' ? err : '');
        var message = failMessage || 'Action failed';
        if (typeof showToast === 'function') {
            showToast(detail ? message + ' (' + detail + ')' : message, 'toast-red', 3500);
        }
        return undefined;
    });
};

// Shared Peers/Connections grouping: same buckets, labels, and collapse
// semantics in both lists. Row markup and virtualization stay view-specific
// (peers uses variable-height profile rows; connections a fixed-height table).
RS.buildPeerGroupItems = function(peers, collapsedGroups, hasName) {
    var contactItems = [], onlineItems = [], onlineStarItems = [], staleItems = [], offlineItems = [];
    peers.forEach(function(c) {
        if (c.is_contact) contactItems.push(c);
        else if (c.status === 'reachable' || c.status === 'direct') {
            if (hasName(c)) onlineItems.push(c);
            else onlineStarItems.push(c);
        } else if (c.status === 'stale') staleItems.push(c);
        else offlineItems.push(c);
    });

    var flatItems = [];
    [
        { items: contactItems, key: 'contacts', label: 'Contacts' },
        { items: onlineItems, key: 'online', label: 'Recent' },
        { items: onlineStarItems, key: 'online_star', label: 'Recent*' },
        { items: staleItems, key: 'stale', label: 'Seen today' },
        { items: offlineItems, key: 'offline', label: 'Older / unknown' }
    ].forEach(function(g) {
        if (g.items.length > 0) {
            flatItems.push({ type: 'header', group: g.key, label: g.label, count: g.items.length });
            if (!collapsedGroups[g.key]) {
                g.items.forEach(function(c) { flatItems.push({ type: 'row', data: c }); });
            }
        }
    });
    return flatItems;
};
