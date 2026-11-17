var currentView = 'dashboard';
var _messageUnreadSources = { direct: 0, channels: 0 };

// Direct LXMF and Channels have distinct destinations and attention indicators.
// Keep one source-aware reducer so asynchronous listeners cannot hide the
// other surface's state.
function setMessageUnreadSource(source, count) {
    if (source !== 'direct' && source !== 'channels') return;
    count = Number(count);
    if (!Number.isFinite(count) || count < 0) count = 0;
    count = Math.floor(count);
    _messageUnreadSources[source] = count;

    var direct = _messageUnreadSources.direct;
    var channels = _messageUnreadSources.channels;
    ['nav-unread-dot', 'bb-unread'].forEach(function(id) {
        var dot = document.getElementById(id);
        if (dot) dot.hidden = direct <= 0;
    });
    ['nav-channels-unread', 'bb-channels-unread'].forEach(function(id) {
        var dot = document.getElementById(id);
        if (dot) dot.hidden = channels <= 0;
    });
    var directTab = document.querySelector('#bottom-bar .ui-tab[data-view="message"]');
    if (directTab) directTab.setAttribute('aria-label', 'Messages' + (direct > 0 ? ', ' + direct + ' unread' : ''));
    var channelsTab = document.querySelector('#bottom-bar .ui-tab[data-view="channels"]');
    if (channelsTab) channelsTab.setAttribute('aria-label', 'Channels' + (channels > 0 ? ', ' + channels + ' unread' : ''));
}
window.setMessageUnreadSource = setMessageUnreadSource;
var VIEWS = ['dashboard', 'message', 'channels', 'contacts', 'identity', 'peers', 'network', 'activity', 'games', 'settings'];

// Tab-bar destinations use replaceState; MORE_VIEWS live in the More sheet.
var TAB_VIEWS = ['peers', 'message', 'channels', 'contacts', 'identity', 'network', 'activity', 'games', 'settings'];
var PRIMARY_TAB_VIEWS = ['peers', 'message', 'channels'];
var MORE_VIEWS = ['contacts', 'identity', 'network', 'activity', 'games', 'settings'];
var MOBILE_TAB_SLOTS = ['peers', 'message', 'channels', 'more'];
var DEFAULT_MORE_VIEW = 'identity';
var _lastMoreView = DEFAULT_MORE_VIEW;
var _lastPrimaryView = 'peers';
try {
    var _savedMoreView = localStorage.getItem('ratspeak_more_view');
    if (MORE_VIEWS.indexOf(_savedMoreView) !== -1) _lastMoreView = _savedMoreView;
    var _savedPrimaryView = localStorage.getItem('ratspeak_last_primary_view');
    if (PRIMARY_TAB_VIEWS.indexOf(_savedPrimaryView) !== -1) _lastPrimaryView = _savedPrimaryView;
} catch(e) {}

function _mobileTabSlot(viewId) {
    return MORE_VIEWS.indexOf(viewId) !== -1 ? 'more' : viewId;
}

function _viewForMobileTabSlot(slot) {
    return slot === 'more' ? (_lastMoreView || DEFAULT_MORE_VIEW) : slot;
}

function _rememberPrimaryView(viewId) {
    if (PRIMARY_TAB_VIEWS.indexOf(viewId) === -1) return;
    _lastPrimaryView = viewId;
    try { localStorage.setItem('ratspeak_last_primary_view', viewId); } catch(e) {}
}

function _lastPrimaryTabView() {
    if (PRIMARY_TAB_VIEWS.indexOf(_lastPrimaryView) !== -1) return _lastPrimaryView;
    return 'peers';
}

// A view that is a list and a detail on a phone: Back closes the detail before leaving the view.
// Level: { pane, active(), close(), capture() -> token, restore(token), reset() }.
var VIEW_DETAIL_LEVELS = {
    settings: {
        pane: '#view-settings .settings-detail-pane',
        active: function() { return typeof isSettingsMobileDetailActive === 'function' && isSettingsMobileDetailActive(); },
        close: function() { if (typeof showSettingsMobileSectionIndex === 'function') showSettingsMobileSectionIndex(); },
        capture: function() { return typeof _settingsShownPanelId === 'string' && _settingsShownPanelId ? _settingsShownPanelId : null; },
        restore: function(panelId) {
            if (typeof selectSettingsSection === 'function') selectSettingsSection(panelId, { showMobileDetail: true, skipStore: true });
        },
        reset: function() { if (typeof showSettingsMobileSectionIndex === 'function') showSettingsMobileSectionIndex({ restoreFocus: false }); }
    }
};

function _activeViewDetail() {
    var level = VIEW_DETAIL_LEVELS[currentView];
    return level && level.active() ? level : null;
}

function _viewDetailSwipeActive() {
    return !!_activeViewDetail();
}

// Where each More destination was opened from, so Back returns there rather than to the last tab.
var _viewTrail = [];
var _viewTrailStarted = false;
var VIEW_TRAIL_MAX = 12;

function _recordViewTrail(fromView, toView, opts) {
    // The first switch is the landing, not a visit.
    if (!_viewTrailStarted) { _viewTrailStarted = true; return; }
    if (MORE_VIEWS.indexOf(toView) === -1) { _viewTrail = []; return; }
    for (var i = 0; i < _viewTrail.length; i++) {
        if (_viewTrail[i].view === toView) { _viewTrail.length = i; return; }
    }
    if (opts.back || fromView === toView) return;
    var level = VIEW_DETAIL_LEVELS[fromView];
    _viewTrail.push({ view: fromView, detail: level && level.active() ? level.capture() : null });
    if (_viewTrail.length > VIEW_TRAIL_MAX) _viewTrail.shift();
}

// A visit opens a view at its list; Back reopens the detail it was left on.
function _prepareViewDetail(viewId, opts) {
    var level = VIEW_DETAIL_LEVELS[viewId];
    _pendingDetailRestore = null;
    if (!level) return;
    if (opts.back && opts.detail != null) {
        level.restore(opts.detail);
        _pendingDetailRestore = { view: viewId, detail: opts.detail };
    } else {
        level.reset();
    }
}

// Legacy hashes that predate the current view names.
var VIEW_ALIASES = {
    'eventlog': 'dashboard',
    'propagation': 'network'
};

function _normalizeViewId(viewId) {
    var normalized = String(viewId || '').replace(/^#/, '');
    if (VIEW_ALIASES[normalized]) normalized = VIEW_ALIASES[normalized];
    if (VIEWS.indexOf(normalized) === -1) return null;
    // A destination whose screen is not in this build is not a destination.
    return document.getElementById('view-' + normalized) ? normalized : null;
}

function _viewForNavigationSurface(viewId) {
    var normalized = _normalizeViewId(viewId);
    if (normalized === 'dashboard' &&
        typeof appUsesMobileNavigation === 'function' &&
        appUsesMobileNavigation()) {
        return 'peers';
    }
    return normalized;
}

function _resolveInitialView() {
    var hashView = _viewForNavigationSurface(window.location.hash);
    if (hashView) return hashView;
    if (typeof appUsesMobileNavigation === 'function' && appUsesMobileNavigation()) {
        return 'peers';
    }
    var saved = null;
    try { saved = localStorage.getItem('ratspeak_view'); } catch(e) {}
    return _viewForNavigationSurface(saved) ||
        (typeof appLandingView === 'function' ? appLandingView() : 'dashboard');
}

var _navTransitioning = false;
var _navInitialLoad = true;
var _mobileNavigationBlockedUntil = 0;

var TRANSITION_CLASSES = ['entering', 'exiting', 'slide-in-right', 'slide-out-left', 'slide-in-left', 'slide-out-right'];

var _prefersReducedMotion = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
if (window.matchMedia) {
    window.matchMedia('(prefers-reduced-motion: reduce)').addEventListener('change', function(e) {
        _prefersReducedMotion = e.matches;
    });
}

function _mobileBlockingOverlayOpen() {
    return !!document.querySelector(
        '.bottom-sheet.open, .bottom-sheet-overlay.active, .ui-modal:not([hidden]), .ui-pop, ' +
        '.modal-overlay.active, .game-modal-overlay, .block-list-overlay, ' +
        '#rs-image-viewer.open, .action-popover.open, ' +
        '[class*="-scrim"].active, .channels-layout.members-open'
    );
}

function _isMobileNavigationBlocked() {
    return isMobile() && (
        Date.now() < _mobileNavigationBlockedUntil ||
        _mobileBlockingOverlayOpen()
    );
}

function blockMobileNavigation(ms) {
    if (!isMobile()) return;
    var until = Date.now() + Math.max(0, ms || 0);
    if (until > _mobileNavigationBlockedUntil) _mobileNavigationBlockedUntil = until;
}

window.RS = window.RS || {};
window.RS.blockMobileNavigation = blockMobileNavigation;
window.RS.isMobileNavigationBlocked = _isMobileNavigationBlocked;

var HAPTICS_STORAGE_KEY = 'rs-haptics-enabled';

function getHapticsEnabled() {
    try { return localStorage.getItem(HAPTICS_STORAGE_KEY) === '1'; } catch(e) {}
    return false;
}

function setHapticsEnabled(enabled) {
    try { localStorage.setItem(HAPTICS_STORAGE_KEY, enabled ? '1' : '0'); } catch(e) {}
}

window.RS.haptics = window.RS.haptics || {};
window.RS.haptics.isEnabled = getHapticsEnabled;
window.RS.haptics.setEnabled = setHapticsEnabled;
window.getHapticsEnabled = getHapticsEnabled;
window.setHapticsEnabled = setHapticsEnabled;

// Accepts semantic names, duration-ish intensity numbers, or a vibration-style
// pattern array. Mobile routes through tauri-plugin-haptics on both iOS and
// Android; browser fallback uses navigator.vibrate.
function haptic(pattern) {
    if (!getHapticsEnabled()) return;
    if (!isTauriMobile()) {
        if (navigator.vibrate) {
            var fallback = _hapticFallbackPattern(pattern);
            if (fallback !== null) navigator.vibrate(fallback);
        }
        return;
    }
    _dispatchHaptic(pattern);
}

function _dispatchHaptic(pattern) {
    if (typeof pattern === 'string') {
        var named = _nameToHapticStep(pattern);
        if (named) _fireHapticStep(named);
        return;
    }
    if (typeof pattern === 'number') {
        var step = _patternToHaptic(pattern);
        if (step) _fireHapticStep(step);
        return;
    }
    if (Array.isArray(pattern)) {
        // [vibrate_ms, gap_ms, ...] — iOS lacks pattern-array native support,
        // so decompose into a setTimeout chain of single feedback calls.
        var t = 0;
        for (var i = 0; i < pattern.length; i += 2) {
            var dur = pattern[i];
            var gap = pattern[i + 1] || 0;
            if (dur > 0) {
                var step = _patternToHaptic(dur);
                if (step) setTimeout(_fireHapticStep.bind(null, step), t);
            }
            t += dur + gap;
        }
    }
}

function _nameToHapticStep(name) {
    switch (name) {
        case 'selection':
            return { kind: 'selection' };
        case 'light':
        case 'impactLight':
            return { kind: 'impact', payload: { style: 'light' } };
        case 'medium':
        case 'impactMedium':
            return { kind: 'impact', payload: { style: 'medium' } };
        case 'heavy':
        case 'impactHeavy':
            return { kind: 'impact', payload: { style: 'heavy' } };
        case 'success':
            return { kind: 'notify', payload: { type: 'success' } };
        case 'warning':
            return { kind: 'notify', payload: { type: 'warning' } };
        case 'error':
            return { kind: 'notify', payload: { type: 'error' } };
        default:
            return null;
    }
}

function _hapticFallbackPattern(pattern) {
    if (typeof pattern === 'number' || Array.isArray(pattern)) return pattern;
    if (typeof pattern !== 'string') return null;
    var map = (window.RS && RS.gestures && RS.gestures.HAPTIC_DURATION_MAP) || {};
    return typeof map[pattern] === 'number' ? map[pattern] : null;
}

// Boundaries: ≤12 light, ≤22 medium, ≤35 heavy, 40+ error.
function _patternToHaptic(ms) {
    if (ms <= 12)  return { kind: 'impact',  payload: { style: 'light'  } };
    if (ms <= 22)  return { kind: 'impact',  payload: { style: 'medium' } };
    if (ms <= 35)  return { kind: 'impact',  payload: { style: 'heavy'  } };
    return         { kind: 'notify', payload: { type: 'error' } };
}

function _fireHapticStep(step) {
    try {
        var method = step.kind === 'impact'    ? 'impact_feedback'
                   : step.kind === 'notify'    ? 'notification_feedback'
                   : step.kind === 'vibrate'   ? 'vibrate'
                   :                             'selection_feedback';
        var payload = step.payload || {};
        var result = _rsHapticsInvoke(method, payload);
        if (result && typeof result.catch === 'function') result.catch(function() {});
    } catch (e) { /* swallow — haptics never block a gesture */ }
}

function _cleanTransitionClasses(el) {
    TRANSITION_CLASSES.forEach(function(cls) { el.classList.remove(cls); });
}

function _focusView(viewEl) {
    if (!viewEl) return;
    requestAnimationFrame(function() {
        var target = viewEl.querySelector('.panel-header, h2, h3, [tabindex="-1"].view-focus-target');
        if (target) {
            target.setAttribute('tabindex', '-1');
            target.focus({ preventScroll: true });
        } else {
            viewEl.setAttribute('tabindex', '-1');
            viewEl.focus({ preventScroll: true });
        }
    });
}

function _animateViewSwitch(oldView, newView, transitionType) {
    if (!oldView || !newView || oldView === newView || !isCompactLayout()) {
        if (oldView) { oldView.classList.remove('active'); _cleanTransitionClasses(oldView); }
        if (newView) { newView.classList.add('active'); }
        return;
    }

    var enterCls, exitCls;
    if (transitionType === 'slide-right') {
        enterCls = 'slide-in-right';
        exitCls = 'slide-out-left';
    } else if (transitionType === 'slide-left') {
        enterCls = 'slide-in-left';
        exitCls = 'slide-out-right';
    } else {
        if (oldView) { oldView.classList.remove('active'); _cleanTransitionClasses(oldView); }
        if (newView) { newView.classList.add('active'); }
        return;
    }

    _navTransitioning = true;

    newView.classList.add('active', 'entering', enterCls);
    oldView.classList.add('exiting', exitCls);

    var cleaned = false;
    function cleanup() {
        if (cleaned) return;
        cleaned = true;
        oldView.classList.remove('active');
        _cleanTransitionClasses(oldView);
        _cleanTransitionClasses(newView);
        _navTransitioning = false;
        _focusView(newView);
    }

    newView.addEventListener('animationend', cleanup, { once: true });
    // Fallback if animationend is swallowed (background tab, reduced motion).
    setTimeout(cleanup, 350);
}

// One selected look: the fill on the sidebar item (and you, on Identity); the accent tab on a phone.
// The More sheet marks nothing: it opens the same every time.
function _markNavigation(viewId) {
    var mobileSlot = _mobileTabSlot(viewId);
    document.querySelectorAll('#sidebar .ui-nav-item[data-view]').forEach(function(item) {
        if (item.dataset.view === viewId) item.setAttribute('aria-current', 'page');
        else item.removeAttribute('aria-current');
    });
    document.querySelectorAll('#bottom-bar .ui-tab').forEach(function(tab) {
        var slot = tab.id === 'bottom-bar-hamburger' ? 'more' : tab.dataset.view;
        if (slot === mobileSlot) tab.setAttribute('aria-current', 'page');
        else tab.removeAttribute('aria-current');
    });
}

function switchView(viewId, opts) {
    viewId = _viewForNavigationSurface(viewId) ||
        (typeof appLandingView === 'function' ? appLandingView() : 'dashboard');
    if (viewId === currentView && !_navInitialLoad) return;
    if (_navTransitioning) return;

    opts = opts || {};
    var previousView = currentView;
    var oldEl = document.getElementById('view-' + previousView);
    var newEl = document.getElementById('view-' + viewId);

    var transitionType = 'fade';
    if (!_navInitialLoad && isCompactLayout()) {
        if (opts.transition) {
            transitionType = opts.transition;
        } else if (opts.back) {
            transitionType = 'slide-left';
        } else if (TAB_VIEWS.indexOf(viewId) !== -1 && TAB_VIEWS.indexOf(previousView) !== -1) {
            // Native-feeling tab-to-tab: no animation.
            transitionType = null;
        }
    }

    if (_navInitialLoad) {
        transitionType = null;
    }

    _recordViewTrail(previousView, viewId, opts);
    _prepareViewDetail(viewId, opts);

    VIEWS.forEach(function(v) {
        var el = document.getElementById('view-' + v);
        if (el && el !== oldEl && el !== newEl) {
            el.classList.remove('active');
            _cleanTransitionClasses(el);
        }
    });

    if (transitionType && !_navInitialLoad) {
        _animateViewSwitch(oldEl, newEl, transitionType);
    } else {
        if (oldEl) { oldEl.classList.remove('active'); _cleanTransitionClasses(oldEl); }
        if (newEl) newEl.classList.add('active');
    }

    _markNavigation(viewId);
    var isMoreView = MORE_VIEWS.indexOf(viewId) !== -1;
    if (isMoreView) {
        _lastMoreView = viewId;
        try { localStorage.setItem('ratspeak_more_view', viewId); } catch(e) {}
    }
    _rememberPrimaryView(viewId);

    // Dismiss keyboard so new view doesn't inherit stale focus/viewport.
    var active = document.activeElement;
    if (active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA')) {
        active.blur();
    }

    ['peers-search', 'contacts-search', 'msg-search-input'].forEach(function(id) {
        var el = document.getElementById(id);
        if (el && el.value) {
            el.value = '';
            el.dispatchEvent(new Event('input'));
        }
    });
    if (previousView === 'message' && viewId !== 'message') {
        if (typeof _removeGhostRow === 'function') _removeGhostRow();
        if (typeof window._closeFabDial === 'function') window._closeFabDial();
        // Pop chat-detail so view-chat-detail body/layout classes clear.
        var top = RS.viewStack.top();
        if (top && top.viewId === 'chat-detail') RS.viewStack.pop();
    }

    if (previousView === 'channels' && viewId !== 'channels') {
        var topChannel = RS.viewStack.top();
        if (topChannel && topChannel.viewId === 'channel-detail') RS.viewStack.pop();
        if (typeof channelsCloseMemberPane === 'function') channelsCloseMemberPane();
    }

    if (previousView === 'games' && viewId !== 'games') {
        var topGame = RS.viewStack.top();
        if (topGame && topGame.viewId === 'game-detail') RS.viewStack.pop();
    }

    currentView = viewId;

    if (!opts.skipHistory) {
        var historyMethod = 'replaceState';
        if (opts.pushState) {
            historyMethod = 'pushState';
        } else if (TAB_VIEWS.indexOf(viewId) !== -1) {
            historyMethod = 'replaceState';
        }
        history[historyMethod]({ view: viewId }, '', '#' + viewId);
    }

    try { localStorage.setItem('ratspeak_view', viewId); } catch(e) {}

    // Defer lifecycle past animation so heavy renders don't fight outgoing frame.
    if (transitionType && !_navInitialLoad && newEl) {
        var _lcFired = false;
        function _fireLc() {
            if (_lcFired) return;
            _lcFired = true;
            if (currentView !== viewId) return;
            _fireViewLifecycle(viewId);
        }
        newEl.addEventListener('animationend', _fireLc, { once: true });
        setTimeout(_fireLc, 400);
    } else {
        _fireViewLifecycle(viewId);
    }
}

// `dirty`: the view's layout changed while hidden, so it re-measures on entry.
var VIEW_LIFECYCLE = {
    dashboard: function(dirty) {
        if (typeof renderDashboardRecentMessages === 'function') renderDashboardRecentMessages();
        if (typeof loadConversations === 'function') loadConversations();
        if (typeof renderDashboardSummaries === 'function' && typeof lastStats !== 'undefined' && lastStats) renderDashboardSummaries(lastStats);
    },

    network: function() {
        if (typeof window.showNetworkStartupWarnings === 'function') window.showNetworkStartupWarnings();
        requestAnimationFrame(function() {
            if (typeof lastStats !== 'undefined' && lastStats) {
                if (typeof renderNetworkOverview === 'function') renderNetworkOverview(lastStats);
            }
            if (typeof loadSettingsInterfacesWithRetry === 'function') loadSettingsInterfacesWithRetry(1);
        });
        if (typeof loadIdentities === 'function') loadIdentities();
        if (typeof renderMergedConnections === 'function') renderMergedConnections();
    },

    settings: function() {
        if (typeof initThemeToggle === 'function') initThemeToggle();
        if (typeof initHapticsToggle === 'function') initHapticsToggle();
        if (typeof initSettingsSectionNav === 'function') initSettingsSectionNav();
        // Settings is opened through switchView(), not openSettings(). Bind
        // backend-backed controls from the real navigation lifecycle so a
        // native radio change cannot look selected without being persisted.
        if (typeof initChannelHostingToggle === 'function') initChannelHostingToggle();
        if (typeof initDeveloperModeToggle === 'function') initDeveloperModeToggle();
        if (typeof initWindowDecorationsToggle === 'function') initWindowDecorationsToggle();
        if (typeof initTrayIconToggle === 'function') initTrayIconToggle();
        if (typeof initCloseButtonToggle === 'function') initCloseButtonToggle();
    },

    identity: function() {
        if (typeof loadIdentities === 'function') loadIdentities();
    },

    games: function() {
        if (typeof gamesTabLoad === 'function') gamesTabLoad();
    },

    peers: function(dirty) {
        if (dirty && typeof _invalidatePeerRowMetrics === 'function') _invalidatePeerRowMetrics();
        if (typeof initPeersView === 'function') requestAnimationFrame(initPeersView);
    },

    message: function() {
        // Cache-first; only re-fetch on stuck error state or empty initial load.
        var _convList = document.getElementById('lxmf-conversations-list');
        var _convStuck = _convList && !!_convList.querySelector('[data-list-state="error"]');
        if (_convStuck && typeof loadConversationsForce === 'function') {
            loadConversationsForce();
        } else if (typeof lxmfConversations !== 'undefined' && lxmfConversations.length > 0) {
            if (typeof _renderConversationsFromCache === 'function') {
                _renderConversationsFromCache(lxmfConversations);
            }
        } else if (typeof _conversationsFirstLoadDone !== 'undefined' && !_conversationsFirstLoadDone
                   && typeof loadConversations === 'function') {
            loadConversations();
        }
    },

    channels: function() {
        if (typeof channelsLoad === 'function') {
            Promise.resolve(channelsLoad()).then(function() {
                if (typeof channelsPrepareVisibleRead === 'function') {
                    channelsPrepareVisibleRead();
                }
                if (typeof channelsRefreshDirectory === 'function') {
                    channelsRefreshDirectory(false);
                }
            });
        }
    },

    contacts: function() {
        if (typeof contactsViewActivated === 'function') contactsViewActivated();
    }
};

function _fireViewLifecycle(viewId) {
    var dirty = !!_viewDirty[viewId];
    clearViewDirty(viewId);
    var handler = VIEW_LIFECYCLE[viewId];
    if (handler) handler(dirty);
    _restorePendingDetail(viewId);
}

// Settings' lifecycle reopens its index; a detail restored by Back wins.
var _pendingDetailRestore = null;

function _restorePendingDetail(viewId) {
    if (!_pendingDetailRestore || _pendingDetailRestore.view !== viewId) return;
    var pending = _pendingDetailRestore;
    _pendingDetailRestore = null;
    VIEW_DETAIL_LEVELS[viewId].restore(pending.detail);
}

function _closeOpenBottomSheet() {
    if (window.RS && RS.kit && RS.kit.closeMenu && document.querySelector('.ui-pop:not(.msg-menu)')) {
        RS.kit.closeMenu();
        return true;
    }
    var openSheets = document.querySelectorAll('.rs-sheet-shell.open, .bottom-sheet.open');
    if (openSheets.length) {
        var topSheet = openSheets[openSheets.length - 1];
        if (typeof topSheet._ratspeakDismiss === 'function') {
            topSheet._ratspeakDismiss();
            return true;
        }
    }
    var more = document.getElementById('bottom-sheet-overlay');
    if (!more || more.hidden) return false;
    _closeMoreSheet();
    return true;
}

function _pushCurrentHistoryAnchor() {
    history.pushState({ view: currentView }, '', '#' + currentView);
}

function _handleAppBackNavigation(opts) {
    opts = opts || {};
    var fromPopState = opts.source === 'popstate';
    var state = opts.state || null;

    if (
        (window.RS && typeof RS.closeImageViewer === 'function' && RS.closeImageViewer()) ||
        (window.RS && typeof RS.closeMessageActionMenu === 'function' && RS.closeMessageActionMenu()) ||
        _closeOpenBottomSheet() ||
        (typeof channelsHandleMemberPaneBack === 'function' && channelsHandleMemberPaneBack())
    ) {
        if (fromPopState) _pushCurrentHistoryAnchor();
        return true;
    }

    // Only the view on screen: a detail left open in a hidden view must not swallow Back.
    var detail = _activeViewDetail();
    if (detail) {
        detail.close();
        if (fromPopState) _pushCurrentHistoryAnchor();
        return true;
    }

    // chat-detail / game-detail are tracked on the view-stack — pop() clears
    // their classes as a side effect.
    if (RS.viewStack && typeof RS.viewStack.depth === 'function' && RS.viewStack.depth() > 1) {
        RS.viewStack.pop();
        if (fromPopState) _pushCurrentHistoryAnchor();
        return true;
    }

    if (isCompactLayout() && MORE_VIEWS.indexOf(currentView) !== -1) {
        _backFromMoreView(fromPopState, state);
        return true;
    }

    return false;
}

// Back from a More destination: the screen it was opened from, else the last tab.
function _backFromMoreView(fromPopState, state) {
    if (_navTransitioning) {
        if (fromPopState) _pushCurrentHistoryAnchor();
        return;
    }
    while (_viewTrail.length && _viewTrail[_viewTrail.length - 1].view === currentView) _viewTrail.pop();
    var entry = _viewTrail.pop() || null;
    var target = entry ? entry.view
        : (fromPopState && state && PRIMARY_TAB_VIEWS.indexOf(state.view) !== -1 ? state.view : _lastPrimaryTabView());
    switchView(target, { back: true, skipHistory: !!fromPopState, detail: entry ? entry.detail : null });
    // Still inside More: keep an entry so the next history Back reaches this handler.
    if (fromPopState && MORE_VIEWS.indexOf(currentView) !== -1) _pushCurrentHistoryAnchor();
}

window.RS = window.RS || {};
window.RS.nav = { registerDetail: function(viewId, level) { VIEW_DETAIL_LEVELS[viewId] = level; } };
window.RS.handleAppBackNavigation = _handleAppBackNavigation;
window.RS.handleAndroidBack = function() {
    return _handleAppBackNavigation({ source: 'android' });
};

function _initHistoryNavigation() {
    // Anchor prevents a back-swipe from landing on about:blank.
    history.replaceState({ view: currentView, anchor: true }, '', '#' + currentView);

    window.addEventListener('popstate', function(e) {
        var state = e.state;

        if (_handleAppBackNavigation({ source: 'popstate', state: state })) {
            return;
        }

        if (state && state.view && VIEWS.indexOf(state.view) !== -1) {
            switchView(state.view, { skipHistory: true, back: true });
        } else {
            // Re-push anchor so the WebView doesn't exit on next back.
            history.pushState({ view: currentView, anchor: true }, '', '#' + currentView);
        }
    });
}

function initSidebarCollapse() {
    var btn = document.getElementById('sidebar-collapse-btn');
    var sidebar = document.getElementById('sidebar');
    if (!btn || !sidebar) return;

    // Narrow desktop windows, including small screens the shell zooms out, use the icon rail.
    var narrow = window.matchMedia
        ? window.matchMedia('(min-width: ' + (RS.config.MOBILE_BREAKPOINT + 1) + 'px) and (max-width: 879px)')
        : null;

    function stored() {
        try { return localStorage.getItem('rs-sidebar-collapsed') === '1'; } catch(e) { return false; }
    }

    function apply() {
        var isCollapsed = stored() || !!(narrow && narrow.matches);
        sidebar.classList.toggle('collapsed', isCollapsed);
        var label = isCollapsed ? 'Expand sidebar' : 'Collapse sidebar';
        btn.setAttribute('aria-label', label);
        btn.title = label;
    }

    apply();
    if (narrow) narrow.addEventListener('change', apply);
    btn.addEventListener('click', function() {
        var next = !sidebar.classList.contains('collapsed');
        try { localStorage.setItem('rs-sidebar-collapsed', next ? '1' : '0'); } catch(e) {}
        apply();
    });
}

var _bbDidLongPress = false;

// Set by long-press completion; settings.js's authoritative IPC completion
// handler reads it to render the burst where the gesture happened.
var _pendingAnnounceOrigin = null;

function initBottomBar() {
    var bar = document.getElementById('bottom-bar');
    if (!bar) return;

    bar.querySelectorAll('.ui-tab[data-view]').forEach(function(item) {
        item.addEventListener('click', function(e) {
            e.preventDefault();
            if (_isMobileNavigationBlocked()) {
                e.stopPropagation();
                return;
            }
            if (_bbDidLongPress) { _bbDidLongPress = false; return; }
            // Tap haptic comes from RS.gestures.attachRipple (RIPPLE_SELECTORS).
            var view = this.dataset.view;
            if (view) switchView(view);
        });
        item.addEventListener('keydown', function(e) {
            if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                this.click();
            }
        });
    });

    var DURATION = RS.gestures.LONG_PRESS_BOTTOM_BAR_MS;
    var DELAY = RS.gestures.LONG_PRESS_BOTTOM_BAR_DELAY_MS;
    var THRESHOLD_PROGRESS = 0.9;
    var ARC_R = 55;
    var ARC_C = 2 * Math.PI * ARC_R;  // ~345.58

    function _easeOutCubic(t) { return 1 - Math.pow(1 - t, 3); }

    // Home-indicator inset: system swipe-home wins over long-press in that strip.
    // Read per touch; a resize listener forced a style pass on every IME step.
    function _sabPx() {
        return parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--sab')) || 0;
    }

    // Captured by closures so attachLongPress stays UI-agnostic.
    var ringEl = null;
    var arcEl = null;
    var saturated = false;

    function _disposeRing() {
        if (!ringEl) return;
        var stale = ringEl;
        ringEl = null;
        arcEl = null;
        saturated = false;
        // Only a visible ring (opacity:1 in onProgress) needs the cancel fade.
        if (stale.style.opacity === '1') {
            stale.classList.add('cancelling');
            setTimeout(function() { stale.remove(); }, 120);
        } else {
            stale.remove();
        }
    }

    RS.gestures.attachLongPress(bar, {
        duration: DURATION,
        delayMs: DELAY,
        moveCancelPx: RS.gestures.LONG_PRESS_MOVE_CANCEL_PX,
        // Skip when touch lands in the OS home-indicator strip or during setup.
        excludeZone: function(touch) {
            if (typeof _isSetupActive === 'function' && _isSetupActive()) return true;
            var sab = _sabPx();
            return sab > 0 && touch.clientY > window.innerHeight - sab;
        },
        // begin = at DELAY (ringProgress 0); almost = 70% through post-delay window.
        hapticStages: [
            { at: DELAY / DURATION,                                  level: 'light'  },
            { at: DELAY / DURATION + (1 - DELAY / DURATION) * 0.7,   level: 'medium' }
        ],
        onStart: function(e) {
            _bbDidLongPress = false;
            if (_prefersReducedMotion) return;
            var touch = e.touches[0];
            var ring = document.createElement('div');
            ring.className = 'hold-ring';
            ring.style.left = touch.clientX + 'px';
            ring.style.top = touch.clientY + 'px';
            ring.innerHTML =
                '<svg viewBox="0 0 120 120">' +
                    '<circle class="hold-ring-track" cx="60" cy="60" r="' + ARC_R + '"/>' +
                    '<circle class="hold-ring-arc" cx="60" cy="60" r="' + ARC_R + '" ' +
                        'stroke-dasharray="' + ARC_C + '" ' +
                        'stroke-dashoffset="' + ARC_C + '" ' +
                        'transform="rotate(-90 60 60)"/>' +
                '</svg>';
            document.body.appendChild(ring);
            ringEl = ring;
            arcEl = ring.querySelector('.hold-ring-arc');
            saturated = false;
        },
        onProgress: function(progress) {
            // Convert overall progress to ringProgress (0..1 over post-delay window).
            var ringProgress = Math.min(
                Math.max(0, (progress * DURATION - DELAY) / (DURATION - DELAY)),
                1
            );
            if (ringEl) {
                ringEl.style.opacity = '1';
                ringEl.style.setProperty('--hold-progress', _easeOutCubic(ringProgress));
            }
            if (arcEl) {
                // Linear time fill so the arc reads as a true progress meter.
                arcEl.setAttribute('stroke-dashoffset', String(ARC_C * (1 - ringProgress)));
            }
            if (!saturated && ringEl && ringProgress >= THRESHOLD_PROGRESS) {
                saturated = true;
                ringEl.classList.add('threshold-reached');
            }
        },
        onCancel: _disposeRing,
        onFire: function(touch) {
            _bbDidLongPress = true;

            // Staged hold already primed the gesture; fire with a firm, non-error pop.
            haptic('heavy');

            var hit = document.elementFromPoint(touch.clientX, touch.clientY);
            var bbItem = (hit && hit.closest) ? hit.closest('.ui-tab') : null;
            if (bbItem) {
                bbItem.classList.add('announcing');
                setTimeout(function() { bbItem.classList.remove('announcing'); }, 1500);
            }

            var origin = { el: bar, cx: touch.clientX, cy: touch.clientY, t: Date.now() };
            var fired = tryTriggerAnnounce();
            if (fired) {
                // settings.js plays the burst once the backend confirms success.
                _pendingAnnounceOrigin = origin;
            } else {
                // Frontend gate (rate-limit / no interface); dampened animation only.
                showAnnounceFailAnimation(origin.el, origin.cx, origin.cy);
            }
            _disposeRing();
        }
    });
}

function _wobbleCirclePath(r, seed) {
    var points = 8;
    var d = 'M';
    for (var i = 0; i <= points; i++) {
        var angle = (i % points) * (2 * Math.PI / points);
        var wobble = r * 0.06 * Math.sin(angle * 3 + seed * 2.3);
        var rr = r + wobble;
        var x = 60 + rr * Math.cos(angle);
        var y = 60 + rr * Math.sin(angle);
        if (i === 0) {
            d += x.toFixed(1) + ' ' + y.toFixed(1);
        } else {
            var prevAngle = ((i - 1) % points) * (2 * Math.PI / points);
            var prevWobble = r * 0.06 * Math.sin(prevAngle * 3 + seed * 2.3);
            var prevR = r + prevWobble;
            var step = 2 * Math.PI / points;
            var cpLen = (4 / 3) * Math.tan(step / 4);
            var cp1x = 60 + prevR * (Math.cos(prevAngle) - cpLen * Math.sin(prevAngle));
            var cp1y = 60 + prevR * (Math.sin(prevAngle) + cpLen * Math.cos(prevAngle));
            var cp2x = 60 + rr * (Math.cos(angle) + cpLen * Math.sin(angle));
            var cp2y = 60 + rr * (Math.sin(angle) - cpLen * Math.cos(angle));
            d += ' C' + cp1x.toFixed(1) + ' ' + cp1y.toFixed(1) +
                 ' ' + cp2x.toFixed(1) + ' ' + cp2y.toFixed(1) +
                 ' ' + x.toFixed(1) + ' ' + y.toFixed(1);
        }
    }
    return d + 'Z';
}

function showAnnounceAnimation(originEl, cx, cy) {
    if (!originEl || _prefersReducedMotion) return;
    if (cx === undefined || cy === undefined) {
        var rect = originEl.getBoundingClientRect();
        cx = rect.left + rect.width / 2;
        cy = rect.top + rect.height / 2;
    }

    originEl.classList.add('announcing');
    setTimeout(function() { originEl.classList.remove('announcing'); }, 1500);

    var overlay = document.createElement('div');
    overlay.className = 'announce-overlay';
    overlay.style.setProperty('--ring-cx', cx + 'px');
    overlay.style.setProperty('--ring-cy', cy + 'px');

    // 3 rings, each with a unique wobbly SVG path
    for (var i = 0; i < 3; i++) {
        var ring = document.createElement('div');
        ring.className = 'announce-ring';
        ring.style.animationDelay = (i * 0.2) + 's';

        var ns = 'http://www.w3.org/2000/svg';
        var svg = document.createElementNS(ns, 'svg');
        svg.setAttribute('viewBox', '0 0 120 120');
        var path = document.createElementNS(ns, 'path');
        path.setAttribute('d', _wobbleCirclePath(55, i));
        path.setAttribute('fill', 'none');
        path.setAttribute('stroke', 'var(--accent)');
        path.setAttribute('stroke-width', '2.5');
        svg.appendChild(path);
        ring.appendChild(svg);

        overlay.appendChild(ring);
    }

    document.body.appendChild(overlay);
    setTimeout(function() { overlay.remove(); }, 2500);
}

// Subdued ring + head-shake when announce is rejected (rate-limited / offline).
function showAnnounceFailAnimation(originEl, cx, cy) {
    if (!originEl || _prefersReducedMotion) return;
    if (cx === undefined || cy === undefined) {
        var rect = originEl.getBoundingClientRect();
        cx = rect.left + rect.width / 2;
        cy = rect.top + rect.height / 2;
    }

    originEl.classList.add('announce-rejected');
    setTimeout(function() { originEl.classList.remove('announce-rejected'); }, 500);

    var overlay = document.createElement('div');
    overlay.className = 'announce-overlay';
    overlay.style.setProperty('--ring-cx', cx + 'px');
    overlay.style.setProperty('--ring-cy', cy + 'px');

    var ring = document.createElement('div');
    ring.className = 'announce-ring announce-ring-dampened';

    var ns = 'http://www.w3.org/2000/svg';
    var svg = document.createElementNS(ns, 'svg');
    svg.setAttribute('viewBox', '0 0 120 120');
    var path = document.createElementNS(ns, 'path');
    path.setAttribute('d', _wobbleCirclePath(55, 0));
    path.setAttribute('fill', 'none');
    path.setAttribute('stroke', 'var(--text-muted)');
    path.setAttribute('stroke-width', '2');
    svg.appendChild(path);
    ring.appendChild(svg);
    overlay.appendChild(ring);

    document.body.appendChild(overlay);
    setTimeout(function() { overlay.remove(); }, 750);
}

// The phone More sheet: you first (Identity, and your contact card), then the other destinations.
function _openMoreSheet() {
    var modal = document.getElementById('bottom-sheet-overlay');
    var sheet = document.getElementById('bottom-sheet');
    var trigger = document.getElementById('bottom-bar-hamburger');
    if (!modal || !sheet || !modal.hidden) return;
    modal.hidden = false;
    if (trigger) trigger.setAttribute('aria-expanded', 'true');
    if (typeof _trapFocus === 'function') _trapFocus(sheet);
    // Push state so OS back closes the sheet before navigating.
    history.pushState({ view: currentView, sheet: true }, '', '#' + currentView);
}

function _closeMoreSheet() {
    var modal = document.getElementById('bottom-sheet-overlay');
    var sheet = document.getElementById('bottom-sheet');
    var trigger = document.getElementById('bottom-bar-hamburger');
    if (!modal || modal.hidden) return;
    modal.hidden = true;
    if (trigger) trigger.setAttribute('aria-expanded', 'false');
    if (sheet && typeof _releaseFocus === 'function') _releaseFocus(sheet);
    if (sheet) { sheet.style.transform = ''; sheet.style.transition = ''; }
}

function initBottomSheet() {
    var trigger = document.getElementById('bottom-bar-hamburger');
    var sheet = document.getElementById('bottom-sheet');
    var modal = document.getElementById('bottom-sheet-overlay');
    if (!trigger || !sheet || !modal) return;
    sheet._ratspeakDismiss = _closeMoreSheet;

    trigger.addEventListener('click', function(e) {
        e.preventDefault();
        e.stopPropagation();
        if (_bbDidLongPress) { _bbDidLongPress = false; return; }
        modal.hidden ? _openMoreSheet() : _closeMoreSheet();
    });

    modal.addEventListener('click', function(e) {
        if (e.target === modal) _closeMoreSheet();
    });

    document.addEventListener('keydown', function(e) {
        if (e.key === 'Escape' && !modal.hidden) _closeMoreSheet();
    });

    // The sheet extends the tab bar; unfolding or resizing into the sidebar layout retires both.
    if (window.matchMedia) {
        window.matchMedia('(max-width: ' + RS.config.MOBILE_BREAKPOINT + 'px)').addEventListener('change', function(e) {
            if (!e.matches) _closeMoreSheet();
        });
    }

    sheet.querySelectorAll('[data-view]').forEach(function(item) {
        item.addEventListener('click', function(e) {
            e.preventDefault();
            var targetView = this.dataset.view;
            _closeMoreSheet();
            // Let the sheet go before switching views.
            setTimeout(function() {
                // The destination already on screen returns to its list.
                if (targetView === currentView) {
                    var level = VIEW_DETAIL_LEVELS[targetView];
                    if (level) level.reset();
                    history.replaceState({ view: currentView }, '', '#' + currentView);
                    return;
                }
                var isDrillDown = TAB_VIEWS.indexOf(targetView) === -1;
                switchView(targetView, {
                    pushState: isDrillDown,
                    transition: isDrillDown ? 'slide-right' : undefined
                });
            }, 50);
        });
    });

    var copy = document.getElementById('more-copy-address');
    if (copy) copy.addEventListener('click', function() {
        if (!_shellAddress || typeof RS.copyText !== 'function') return;
        Promise.resolve(RS.copyText(_shellAddress)).then(function(ok) {
            if (!ok) {
                if (typeof showToast === 'function') showToast('Could not copy your LXMF address', 'toast-error', 3000);
                return;
            }
            if (typeof showCopyConfirmationToast === 'function') showCopyConfirmationToast('LXMF address');
            copy.innerHTML = RS.kit.icon('check');
            clearTimeout(copy._rsCopiedTimer);
            copy._rsCopiedTimer = setTimeout(function() { copy.innerHTML = RS.kit.icon('copy'); }, 2000);
        });
    });

    var share = document.getElementById('more-share-card');
    if (share) share.addEventListener('click', function() {
        _closeMoreSheet();
        if (typeof openActiveIdentityContactCard === 'function') openActiveIdentityContactCard();
    });

    RS.gestures.attachDragDismiss(sheet, {
        axis: 'y',
        blockIfScrolled: true,
        scrim: modal,
        onCommit: _closeMoreSheet
    });
}

// You, in the sidebar foot and at the top of the phone More sheet.
var _shellAddress = '';
function renderShellIdentity(hash, name) {
    if (!hash) return;
    var label = name || 'Unnamed';
    var short = RS.kit.hash.short(hash);
    _shellAddress = hash;
    var sidebarIcon = document.getElementById('sidebar-identity-icon');
    if (sidebarIcon) sidebarIcon.innerHTML = identityAvatar(hash, 32);
    var moreIcon = document.getElementById('more-identity-icon');
    if (moreIcon) moreIcon.innerHTML = identityAvatar(hash, 48);
    ['sidebar-identity-name', 'more-identity-name'].forEach(function(id) {
        var el = document.getElementById(id);
        if (el) el.textContent = label;
    });
    var sidebarHash = document.getElementById('sidebar-identity-hash');
    if (sidebarHash) sidebarHash.textContent = short;
    // The sheet has room for 8…8 beside its buttons on any phone.
    var moreHash = document.getElementById('more-identity-hash');
    if (moreHash) moreHash.textContent = RS.kit.hash.mid(hash);
    var me = document.getElementById('sidebar-identity');
    if (me) me.setAttribute('aria-label', label + ', open Identity');
    var more = document.getElementById('more-identity');
    if (more) more.setAttribute('aria-label', label + ', open Identity');
}
// Games waiting for you: a dot in the sidebar and on More, the count in the More sheet.
function setShellGamesWaiting(count) {
    count = Math.max(0, Math.floor(Number(count) || 0));
    var dot = document.getElementById('nav-games-unread');
    if (dot) dot.hidden = count === 0;
    var more = document.getElementById('bb-more-unread');
    if (more) more.hidden = count === 0;
    var badge = document.getElementById('bs-games-unread');
    if (badge) {
        badge.hidden = count === 0;
        badge.textContent = count > 99 ? '99+' : String(count);
        badge.setAttribute('aria-label', count + ' waiting for you');
    }
    var tab = document.getElementById('bottom-bar-hamburger');
    if (tab) tab.setAttribute('aria-label', 'More' + (count ? ', ' + count + ' waiting for you' : ''));
}

window.RS = window.RS || {};
RS.shell = { setIdentity: renderShellIdentity, setGamesWaiting: setShellGamesWaiting, openMore: _openMoreSheet, closeMore: _closeMoreSheet };

// Wires swipe-down + overlay-tap dismissal in one call.
function initSheetSwipeDismiss(sheetId, overlayId, closeFn) {
    var sheet = document.getElementById(sheetId);
    if (!sheet) return;
    var overlay = overlayId ? document.getElementById(overlayId) : null;
    var close = closeFn || function() {};
    sheet._ratspeakDismiss = close;
    if (overlay && !overlay._sheetDismissWired) {
        overlay.addEventListener('click', close);
        overlay._sheetDismissWired = true;
    }
    return RS.gestures.attachDragDismiss(sheet, {
        axis: 'y',
        blockIfScrolled: true,
        parallaxOverlay: overlay,
        onCommit: close
    });
}

var _waitingForKeyboard = false;
var _keyboardStableTimer = null;
var _focusedFieldRevealFrame = null;
var _maxViewportHeight = 0;
// A viewport unchanged for this long has finished a multi-step IME resize.
var KEYBOARD_SETTLE_MS = 100;

function _visualViewportGeometry(visualHeight, offsetTop, pageTop, bodyTop, scrollTop) {
    var normalizedScroll = Math.max(0, Number(scrollTop) || 0);
    var reportedOffset = Math.max(0, Number(offsetTop) || 0);
    var pageOffset = Math.max(0, (Number(pageTop) || 0) - normalizedScroll);
    var renderedBodyOffset = Math.max(0, -(Number(bodyTop) || 0) - normalizedScroll);
    return {
        height: Math.max(0, Number(visualHeight) || 0),
        offset: Math.max(reportedOffset, pageOffset, renderedBodyOffset)
    };
}

function _keyboardViewportMetrics(innerHeight, visualHeight, offsetTop, maxViewportHeight) {
    var keyboardHeight = innerHeight - visualHeight;
    var heightDrop = maxViewportHeight > 0 ? maxViewportHeight - visualHeight : 0;
    var open = keyboardHeight > 150 || heightDrop > 150;
    return {
        open: open,
        inset: open ? Math.max(0, innerHeight - visualHeight - (offsetTop || 0)) : 0
    };
}

// IME resizes come in steps: open on the first past the threshold, close at full height or once settled.
function _keyboardOpenAfterResize(wasOpen, innerHeight, visualHeight, maxViewportHeight, settled) {
    if (_keyboardViewportMetrics(innerHeight, visualHeight, 0, maxViewportHeight).open) return true;
    return wasOpen && !settled && visualHeight < maxViewportHeight - 1;
}

// A root custom property write restyles the whole page; the inline read is free.
function _setRootStyleProperty(name, value) {
    var style = document.documentElement.style;
    if (style.getPropertyValue(name) !== value) style.setProperty(name, value);
}

function _revealFocusedSheetField(el) {
    if (!el || document.activeElement !== el) return;
    var sheet = el.closest && el.closest('.bottom-sheet.open');
    if (!sheet) {
        el.scrollIntoView({ block: 'nearest', behavior: 'auto' });
        return;
    }

    var scrollOwner = el.closest('.bottom-sheet-body') || sheet;
    var ownerRect = scrollOwner.getBoundingClientRect();
    var fieldRect = el.getBoundingClientRect();
    var visibleBottom = window.visualViewport
        ? window.visualViewport.offsetTop + window.visualViewport.height
        : window.innerHeight;
    var ownerBottom = Math.min(ownerRect.bottom, visibleBottom);
    var edge = parseFloat(
        getComputedStyle(document.documentElement).getPropertyValue('--space-6')
    ) || 0;
    if (fieldRect.bottom > ownerBottom - edge) {
        scrollOwner.scrollTop += fieldRect.bottom - ownerBottom + edge;
    } else if (fieldRect.top < ownerRect.top + edge) {
        scrollOwner.scrollTop -= ownerRect.top + edge - fieldRect.top;
    }
}

function _scheduleFocusedFieldReveal(el) {
    if (_focusedFieldRevealFrame) cancelAnimationFrame(_focusedFieldRevealFrame);
    _focusedFieldRevealFrame = requestAnimationFrame(function() {
        _focusedFieldRevealFrame = null;
        _revealFocusedSheetField(el);
    });
}

function _chatMessagesForKeyboard() {
    return document.body.classList.contains('view-channel-detail')
        ? document.getElementById('channel-transcript')
        : document.getElementById('lxmf-messages');
}

function _chatMessagesNearBottomForKeyboard() {
    var msgContainer = _chatMessagesForKeyboard();
    if (!msgContainer) return true;
    return RS.chatScroll.nearBottom(msgContainer);
}

function _chatMessagesFollowingForKeyboard() {
    var msgContainer = _chatMessagesForKeyboard();
    if (!msgContainer) return true;
    return RS.chatScroll.isFollowing(msgContainer);
}

function _pinChatMessagesToBottomForKeyboard() {
    var msgContainer = _chatMessagesForKeyboard();
    if (!msgContainer) return;
    RS.chatScroll.pinToBottom(msgContainer);
}

function initKeyboardDetection() {
    var bar = document.getElementById('bottom-bar');
    document.documentElement.setAttribute(
        'data-keyboard-platform',
        isIOS() ? 'ios' : (isAndroid() ? 'android' : 'other')
    );
    if (!window.visualViewport) return;

    var _prevKeyboardOpen = false;
    var _fullAppHeight = 0;
    var _viewportFrame = null;
    var _viewportFramePending = false;
    var _viewportOffsetTop = window.visualViewport.offsetTop;

    function setKeyboardOpen(open) {
        if (open === _prevKeyboardOpen) return;
        _prevKeyboardOpen = open;
        if (bar) bar.classList.toggle('keyboard-open', open);
        document.documentElement.classList.toggle('keyboard-open', open);
    }

    // Android: IME padding resizes WebView; geometry is read before any write.
    function applyViewport(settled) {
        var currentHeight = window.visualViewport.height;
        var innerHeight = window.innerHeight;
        var mobile = isMobile();
        if (currentHeight > _maxViewportHeight) _maxViewportHeight = currentHeight;
        var inConversationDetail =
            document.body.classList.contains('view-chat-detail') ||
            document.body.classList.contains('view-channel-detail');
        var keyboardOpen = _keyboardOpenAfterResize(
            _prevKeyboardOpen, innerHeight, currentHeight, _maxViewportHeight, settled);
        var documentScrolled = mobile && keyboardOpen && inConversationDetail &&
            (window.scrollY > 0 || document.documentElement.scrollTop > 0);

        if (mobile) {
            if (!keyboardOpen) _fullAppHeight = currentHeight;
            // Outside chat: leave --app-height alone so other inputs don't reflow.
            if (!keyboardOpen || inConversationDetail) {
                _setRootStyleProperty('--app-height', currentHeight + 'px');
            }
            if (documentScrolled) window.scrollTo(0, 0);
        }
        setKeyboardOpen(keyboardOpen);

        clearTimeout(_keyboardStableTimer);
        if (!settled) {
            _keyboardStableTimer = setTimeout(function() { applyViewport(true); }, KEYBOARD_SETTLE_MS);
        }
        if (!_waitingForKeyboard) return;
        if (keyboardOpen) _pinChatMessagesToBottomForKeyboard();
        if (settled) _waitingForKeyboard = false;
    }

    // Apply in the event (a rAF paints a frame of old layout at the new size); coalesce the rest.
    function scheduleViewport() {
        if (_viewportFrame !== null) {
            _viewportFramePending = true;
            return;
        }
        applyViewport(false);
        _viewportFrame = requestAnimationFrame(function() {
            _viewportFrame = null;
            if (!_viewportFramePending) return;
            _viewportFramePending = false;
            applyViewport(false);
        });
    }

    function onResize() {
        if (!isIOS()) {
            scheduleViewport();
            return;
        }
        var vv = window.visualViewport;
        var currentHeight = vv.height;

        if (currentHeight > _maxViewportHeight) {
            _maxViewportHeight = currentHeight;
        }

        var inConversationDetail =
            document.body.classList.contains('view-chat-detail') ||
            document.body.classList.contains('view-channel-detail');
        var activeComposer = document.activeElement;
        // Capture intent before changing viewport geometry. A smaller viewport
        // can create a large bottom gap without the user scrolling at all.
        var followIOSChat = inConversationDetail && activeComposer &&
            _isChatComposerField(activeComposer) &&
            _chatMessagesFollowingForKeyboard();

        var documentScroll = Math.max(
            Number(window.scrollY) || 0,
            Number(document.documentElement.scrollTop) || 0
        );
        if (inConversationDetail && documentScroll > 0) {
            window.scrollTo(0, 0);
            documentScroll = Math.max(
                Number(window.scrollY) || 0,
                Number(document.documentElement.scrollTop) || 0
            );
        }
        var bodyRect = document.body.getBoundingClientRect();
        var visualGeometry = _visualViewportGeometry(
            currentHeight,
            vv.offsetTop,
            vv.pageTop,
            bodyRect.top,
            documentScroll
        );

        // WKWebView can report offsetTop late, so use the effective
        // rendered offset when anchoring fixed sheets and conversations.
        var keyboardMetrics = _keyboardViewportMetrics(
            window.innerHeight,
            currentHeight,
            visualGeometry.offset,
            _maxViewportHeight
        );
        var keyboardOpen = keyboardMetrics.open;

        // Fixed sheets are laid out against WKWebView's layout viewport,
        // while the keyboard reduces the visual viewport. Publish both
        // measurements so every canonical form sheet can stay above the
        // IME instead of relying on device-specific offsets.
        document.documentElement.style.setProperty('--visual-viewport-height', currentHeight + 'px');
        document.documentElement.style.setProperty('--visual-viewport-top', visualGeometry.offset + 'px');
        document.documentElement.style.setProperty('--keyboard-inset', keyboardMetrics.inset + 'px');
        if (keyboardOpen && inConversationDetail) {
            // Size and anchor the whole conversation to the visual
            // viewport. On iOS 26 WKWebView can pan the rendered body while
            // scrollTop remains zero; height alone leaves the composer
            // behind the keyboard, so the effective top offset is required.
            document.documentElement.style.setProperty('--app-height', currentHeight + 'px');
        } else if (!keyboardOpen) {
            _fullAppHeight = currentHeight;
            document.documentElement.style.setProperty('--app-height', currentHeight + 'px');
        }
        // Outside chat: leave --app-height alone so other inputs don't reflow.

        if (keyboardOpen) {
            if (bar) bar.classList.add('keyboard-open');
            document.documentElement.classList.add('keyboard-open');

            var active = document.activeElement;
            if (active && active.closest && active.closest('.bottom-sheet.open')) {
                _scheduleFocusedFieldReveal(active);
            }
        } else {
            if (bar) bar.classList.remove('keyboard-open');
            document.documentElement.classList.remove('keyboard-open');
        }

        _prevKeyboardOpen = keyboardOpen;
        // Every iOS viewport adjustment uses the shared cancellable scroll
        // controller, including late keyboard/accessory-bar changes. A real
        // history scroll owns followLatest and cancels its pending pins.
        if (followIOSChat) _pinChatMessagesToBottomForKeyboard();
    }

    window.visualViewport.addEventListener('resize', onResize);
    if (isIOS()) window.addEventListener('resize', onResize);
    window.visualViewport.addEventListener('scroll', function() {
        if (!isIOS()) {
            // A scroll without an offset change cannot move the conversation.
            var offsetTop = window.visualViewport.offsetTop;
            if (offsetTop === _viewportOffsetTop) return;
            _viewportOffsetTop = offsetTop;
            scheduleViewport();
            return;
        }
        // Pin scroll while chat keyboard is open; WKWebView otherwise scrolls
        // the header behind the notch as the viewport pans.
        var inConversationDetail =
            document.body.classList.contains('view-chat-detail') ||
            document.body.classList.contains('view-channel-detail');
        if (document.documentElement.classList.contains('keyboard-open') &&
                inConversationDetail) {
            if (window.scrollY > 0 || document.documentElement.scrollTop > 0) {
                window.scrollTo(0, 0);
            }
        }
        onResize();
    });
    if (isIOS()) onResize();
    else applyViewport(true);

    // Rotating with the keyboard up leaves the layout half-resized.
    window.addEventListener('orientationchange', function() {
        _maxViewportHeight = 0;
        var active = document.activeElement;
        if (active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA')) {
            active.blur();
        }
        setTimeout(function() {
            if (window.visualViewport) {
                var h = window.visualViewport.height;
                _maxViewportHeight = h;
                _fullAppHeight = h;
                document.documentElement.style.setProperty('--app-height', h + 'px');
            }
        }, 400);
    });

    // Per-input scroll behaviour: search bars float above keyboard already,
    // chat compose only pins messages if the user was already at the latest
    // messages; modal/other inputs scrollIntoView.
    document.addEventListener('focusin', function(e) {
        var el = e.target;
        if (el.tagName !== 'INPUT' && el.tagName !== 'TEXTAREA') return;

        if (el.closest('.connections-header, .lxmf-sidebar, .ui-pane-search')) {
            return;
        }

        if (_isChatComposerField(el)) {
            // iOS follows viewport changes directly. Its initial focus resize
            // can precede keyboard opening, which cleared the old one-shot flag.
            _waitingForKeyboard = !isIOS() && _chatMessagesNearBottomForKeyboard();
            if (isIOS()) onResize();
            return;
        }

        if (el.closest('.modal, .bottom-sheet')) {
            if (isIOS()) {
                _scheduleFocusedFieldReveal(el);
            } else {
                setTimeout(function() {
                    el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
                }, 150);
            }
            return;
        }

        if (isIOS()) {
            requestAnimationFrame(function() {
                el.scrollIntoView({ block: 'nearest', behavior: 'auto' });
            });
        } else {
            setTimeout(function() {
                el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
            }, 300);
        }
    });

    document.addEventListener('focusout', function(e) {
        var el = e.target;
        // The settle timer stays: it may still owe the keyboard-closed state.
        if (_isChatComposerField(el)) {
            _waitingForKeyboard = false;
        }
    });
}

// A chat composer's own fields: the message, and in Messages its optional title.
function _isChatComposerField(el) {
    return !!el && (el.id === 'lxmf-input' || el.id === 'lxmf-title' || el.id === 'channel-message-input');
}

function initTextareaAutoGrow() {
    var textarea = document.getElementById('lxmf-input');
    if (!textarea) return;

    var _growRaf = null;
    textarea.addEventListener('input', function() {
        var ta = this;
        if (RS.composer && typeof RS.composer.resize === 'function') RS.composer.resize(ta);
        // rAF so scroll happens after the browser applies the new height.
        if (document.documentElement.classList.contains('keyboard-open') && _chatMessagesNearBottomForKeyboard()) {
            if (_growRaf) cancelAnimationFrame(_growRaf);
            _growRaf = requestAnimationFrame(function() {
                _growRaf = null;
                _pinChatMessagesToBottomForKeyboard();
            });
        }
    });
}

function initDrillDownSwipeBack() {
    if (!isTouchDevice()) return;

    function _animateOutAndPop() {
        var viewEl = document.getElementById('view-' + currentView);
        function _doPop() {
            if (RS.viewStack.depth() > 1) {
                RS.viewStack.pop();
                return;
            }
            _backFromMoreView(false);
        }
        if (!viewEl) { _doPop(); return; }
        viewEl.style.transition = 'transform 0.2s ease, opacity 0.2s ease';
        viewEl.style.transform = 'translateX(100%)';
        viewEl.style.opacity = '0';
        setTimeout(function() {
            viewEl.style.transition = '';
            viewEl.style.transform = '';
            viewEl.style.opacity = '';
            _doPop();
        }, 200);
    }

    RS.gestures.attachSwipe(document, {
        direction: 'right',
        edgeZone: RS.gestures.EDGE_ZONE_PX,
        distanceThreshold: RS.gestures.SWIPE_DISTANCE_DRILLBACK_PX,
        hapticAt: { commit: 'selection' },
        skipIf: function(e) {
            if (!isCompactLayout()) return true;
            if (_isMobileNavigationBlocked()) return true;
            if (_navTransitioning) return true;
            if (e.target.closest('button, a, input, select, .selector-badge')) return true;
            if (_viewDetailSwipeActive()) return true;
            // Fire when something's on the stack OR sitting on a More destination.
            if (RS.viewStack.depth() > 1) return false;
            if (MORE_VIEWS.indexOf(currentView) !== -1) return false;
            return true;
        },
        onProgress: function(dx) {
            if (_viewDetailSwipeActive()) return;
            var viewEl = document.getElementById('view-' + currentView);
            if (viewEl && dx > 0) {
                viewEl.style.transition = 'none';
                viewEl.style.transform = 'translateX(' + dx + 'px)';
                viewEl.style.opacity = Math.max(0.3, 1 - dx / RS.gestures.DRAG_DISMISS_OPACITY_DENOM_PX);
            }
        },
        onCommit: _animateOutAndPop,
        onCancel: function() {
            if (_viewDetailSwipeActive()) return;
            var viewEl = document.getElementById('view-' + currentView);
            if (viewEl) {
                viewEl.style.transition = 'transform 0.2s cubic-bezier(0.2, 0, 0, 1), opacity 0.2s ease';
                viewEl.style.transform = '';
                viewEl.style.opacity = '';
                setTimeout(function() { viewEl.style.transition = ''; }, 200);
            }
        }
    });
}

// A swipe right anywhere on an open detail (Settings section, identity) returns to its list.
function initViewDetailSwipeBack() {
    if (!isTouchDevice()) return;

    var swiping = null;

    function _pane(level) {
        return level ? document.querySelector(level.pane) : null;
    }

    function _resetPane(pane) {
        if (!pane) return;
        pane.style.transition = '';
        pane.style.transform = '';
        pane.style.opacity = '';
    }

    RS.gestures.attachSwipe(document, {
        direction: 'right',
        distanceThreshold: RS.gestures.SWIPE_DISTANCE_DRILLBACK_PX,
        hapticAt: { commit: 'selection' },
        skipIf: function(e) {
            if (!isCompactLayout()) return true;
            if (_isMobileNavigationBlocked()) return true;
            if (_navTransitioning) return true;
            swiping = _activeViewDetail();
            if (!swiping) return true;
            if (e.target.closest('button, a, input, select, textarea, .selector-badge, .theme-toggle, .prop-toggle')) return true;
            return false;
        },
        onProgress: function(dx) {
            var pane = _pane(swiping);
            if (!pane || dx <= 0) return;
            pane.style.transition = 'none';
            pane.style.transform = 'translateX(' + dx + 'px)';
            pane.style.opacity = Math.max(0.35, 1 - dx / RS.gestures.DRAG_DISMISS_OPACITY_DENOM_PX);
        },
        onCommit: function() {
            var level = swiping;
            var pane = _pane(level);
            if (!level) return;
            if (!pane) { level.close(); return; }
            pane.style.transition = 'transform 0.18s ease, opacity 0.18s ease';
            pane.style.transform = 'translateX(100%)';
            pane.style.opacity = '0';
            setTimeout(function() {
                _resetPane(pane);
                level.close();
            }, 180);
        },
        onCancel: function() {
            var pane = _pane(swiping);
            if (!pane) return;
            pane.style.transition = 'transform 0.18s cubic-bezier(0.2, 0, 0, 1), opacity 0.18s ease';
            pane.style.transform = '';
            pane.style.opacity = '';
            setTimeout(function() { _resetPane(pane); }, 180);
        }
    });
}

function initTabSwipe() {
    if (!isTouchDevice()) return;

    RS.gestures.attachSwipe(document, {
        direction: 'horizontal',
        edgeMargin: RS.gestures.EDGE_MARGIN_TAB_SWIPE_PX,
        distanceThreshold: RS.gestures.SWIPE_DISTANCE_PX,
        skipIf: function(e) {
            if (!isCompactLayout()) return true;
            if (typeof _isSetupActive === 'function' && _isSetupActive()) return true;
            if (_isMobileNavigationBlocked()) return true;
            if (_navTransitioning) return true;
            if (_viewDetailSwipeActive()) return true;
            if (RS.viewStack && typeof RS.viewStack.depth === 'function' && RS.viewStack.depth() > 1) return true;
            if (MOBILE_TAB_SLOTS.indexOf(_mobileTabSlot(currentView)) === -1) return true;
            if (e.target.closest('button, a, input, select, .selector-badge')) return true;
            // Active chat and game sessions own horizontal gestures.
            var lxmfLayout = document.querySelector('.lxmf-layout');
            if (lxmfLayout && lxmfLayout.classList.contains('view-chat-detail')) return true;
            var gamesLayout = document.querySelector('.games-layout');
            if (gamesLayout && gamesLayout.classList.contains('view-game-detail')) return true;
            var channelsLayout = document.querySelector('.channels-layout');
            if (channelsLayout && channelsLayout.classList.contains('view-channel-detail')) return true;
            return false;
        },
        onCommit: function(_target, dx) {
            if (MORE_VIEWS.indexOf(currentView) !== -1 && dx > 0) {
                haptic('selection');
                _backFromMoreView(false);
                return;
            }
            var currentIdx = MOBILE_TAB_SLOTS.indexOf(_mobileTabSlot(currentView));
            if (currentIdx === -1) return;
            var nextIdx = dx < 0 ? currentIdx + 1 : currentIdx - 1;
            if (nextIdx < 0 || nextIdx >= MOBILE_TAB_SLOTS.length) return;
            var targetView = _viewForMobileTabSlot(MOBILE_TAB_SLOTS[nextIdx]);
            if (!targetView || targetView === currentView) return;
            haptic('selection');
            switchView(targetView);
        }
    });
}

var FIRST_RUN_ANNOUNCE_HINT_KEY = 'ratspeak_first_run';
var _firstRunDismiss = null;
var _firstRunHintEl = null;
var _firstRunHintTimer = null;
var _firstRunHintAutoHiddenThisSession = false;
var _firstRunAnnounceListenerBound = false;
var _firstRunHasConfiguredInterface = false;

function _firstRunHintDone() {
    try { return !!localStorage.getItem(FIRST_RUN_ANNOUNCE_HINT_KEY); } catch (_) { return false; }
}

function _setFirstRunHintDone() {
    try { localStorage.setItem(FIRST_RUN_ANNOUNCE_HINT_KEY, 'done'); } catch (_) {}
}

function clearFirstRunAnnounceHintDone() {
    try { localStorage.removeItem(FIRST_RUN_ANNOUNCE_HINT_KEY); } catch (_) {}
    _firstRunHintAutoHiddenThisSession = false;
}

function _firstRunMobileEligible() {
    if (window.__RATSPEAK_DESKTOP__) return false;
    if (window.__RATSPEAK_MOBILE__ === true) return true;
    return typeof isMobile === 'function' && isMobile();
}

function _firstRunInterfaceEnabled(entry) {
    if (!entry || typeof entry !== 'object') return false;
    var enabled = entry.enabled;
    if (enabled === undefined || enabled === null) return true;
    return !/^(false|no|0)$/i.test(String(enabled).trim());
}

function _firstRunConfiguredInterfaceCount(data) {
    if (!data || typeof data !== 'object') return 0;
    return ['rnode', 'auto', 'tcp_client', 'tcp_server', 'backbone_client', 'backbone_server']
        .reduce(function(count, group) {
            var entries = Array.isArray(data[group]) ? data[group] : [];
            return count + entries.filter(_firstRunInterfaceEnabled).length;
        }, 0);
}

function updateFirstRunInterfaceHintGate(data) {
    _firstRunHasConfiguredInterface = _firstRunConfiguredInterfaceCount(data) > 0;
    if (_firstRunHasConfiguredInterface &&
            typeof _anyInterfaceOnline !== 'undefined' &&
            _anyInterfaceOnline === true &&
            typeof scheduleFirstRunTooltip === 'function') {
        scheduleFirstRunTooltip(600);
    }
}

function _firstRunAnnounceHintEligible() {
    if (_firstRunHintDone() || _firstRunHintEl || _firstRunHintAutoHiddenThisSession) return false;
    if (!_firstRunMobileEligible()) return false;
    if (typeof _isSetupActive === 'function' && _isSetupActive()) return false;
    if (_firstRunHasConfiguredInterface !== true) return false;
    if (typeof _anyInterfaceOnline === 'undefined' || _anyInterfaceOnline !== true) return false;

    var bar = document.getElementById('bottom-bar');
    if (!bar) return false;
    var style = getComputedStyle(bar);
    return style.display !== 'none' &&
        style.visibility !== 'hidden' &&
        style.opacity !== '0' &&
        style.pointerEvents !== 'none';
}

function showFirstRunTooltip() {
    if (!_firstRunAnnounceHintEligible()) return false;

    var hint = document.createElement('div');
    hint.className = 'first-run-hint';
    hint.innerHTML = '<span class="first-run-hint-icon" aria-hidden="true"><svg class="first-run-hint-svg" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">' +
            '<rect x="4" y="16" width="16" height="4.5" rx="2.25"/>' +
            '<circle cx="12" cy="18.25" r="1.15" fill="currentColor" stroke="none"/>' +
            '<path d="M12 16v-3"/>' +
            '<path d="M9.2 12.2a4 4 0 0 1 5.6 0"/>' +
            '<path d="M6.8 9.4a7.4 7.4 0 0 1 10.4 0"/>' +
        '</svg></span>' +
        '<span class="first-run-hint-text">Tap and hold to announce</span>';
    document.body.appendChild(hint);
    _firstRunHintEl = hint;

    // Double-rAF so the initial-state paint happens before transition.
    requestAnimationFrame(function() {
        requestAnimationFrame(function() { hint.classList.add('visible'); });
    });

    var dismissed = false;
    function dismiss(opts) {
        if (dismissed) return;
        opts = opts || {};
        dismissed = true;
        _firstRunDismiss = null;
        _firstRunHintEl = null;
        if (_firstRunHintTimer) {
            clearTimeout(_firstRunHintTimer);
            _firstRunHintTimer = null;
        }
        if (opts.persist) _setFirstRunHintDone();
        if (opts.auto) _firstRunHintAutoHiddenThisSession = true;
        hint.classList.add('dismissing');
        hint.classList.remove('visible');
        setTimeout(function() { hint.remove(); }, 400);
    }

    _firstRunDismiss = dismiss;
    hint.addEventListener('click', function() { dismiss({ persist: true }); });
    _firstRunHintTimer = setTimeout(function() {
        if (_firstRunDismiss) _firstRunDismiss({ auto: true });
    }, 7000);
    return true;
}

function scheduleFirstRunTooltip(delayMs) {
    if (_firstRunHintTimer || _firstRunHintDone() || _firstRunHintAutoHiddenThisSession) return;
    _firstRunHintTimer = setTimeout(function() {
        _firstRunHintTimer = null;
        showFirstRunTooltip();
    }, delayMs || 0);
}

function bindFirstRunAnnounceListener() {
    if (_firstRunAnnounceListenerBound) return;
    _firstRunAnnounceListenerBound = true;
    window.addEventListener('ratspeak-manual-announce-result', function(event) {
        var data = event && event.detail;
        if (!data || !data.success || _firstRunHintDone()) return;
        _setFirstRunHintDone();
        if (_firstRunDismiss) _firstRunDismiss();
    });
}

document.addEventListener('DOMContentLoaded', function() {
    // Native mobile routes Home to Peers at every width, so wide sidebars (iPad, iPhone Duo) omit it.
    if (isTauriMobile()) document.documentElement.dataset.nativeMobile = 'true';
    var initialView = _resolveInitialView();
    // Destinations whose screen is not in this build stay out of the navigation.
    document.querySelectorAll('#sidebar [data-view], #bottom-sheet [data-view], #bottom-bar [data-view]').forEach(function(item) {
        if (!document.getElementById('view-' + item.dataset.view)) item.hidden = true;
    });
    document.querySelectorAll('#sidebar .ui-nav-item[data-view]').forEach(function(item) {
        item.addEventListener('click', function(e) {
            e.preventDefault();
            switchView(this.dataset.view);
        });
    });

    initSidebarCollapse();
    initBottomBar();
    initBottomSheet();
    initKeyboardDetection();
    initTextareaAutoGrow();
    RS.gestures.attachRipple(document, {
        selectors: RS.gestures.RIPPLE_SELECTORS,
        hapticOnTap: 'light'
    });
    initDrillDownSwipeBack();
    initViewDetailSwipeBack();
    initTabSwipe();

    bindFirstRunAnnounceListener();
    // Delay so initial layout settles; actual display waits for an online interface.
    scheduleFirstRunTooltip(2000);

    if (typeof needsSetup !== 'undefined' && needsSetup) {
        _initHistoryNavigation();
        return;
    }

    switchView(initialView);
    _initHistoryNavigation();

    _navInitialLoad = false;
});
