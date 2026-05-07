var currentView = 'dashboard';
var VIEWS = ['dashboard', 'message', 'contacts', 'peers', 'network', 'games', 'settings'];

// Tab-bar destinations use replaceState; MORE_VIEWS live under the hamburger.
var TAB_VIEWS = ['peers', 'message', 'contacts', 'network', 'games', 'settings'];
var MORE_VIEWS = ['network', 'games', 'settings'];
var MOBILE_TAB_SLOTS = ['peers', 'message', 'contacts', 'more'];
var DEFAULT_MORE_VIEW = 'network';
var _lastMoreView = DEFAULT_MORE_VIEW;
try {
    var _savedMoreView = localStorage.getItem('ratspeak_more_view');
    if (MORE_VIEWS.indexOf(_savedMoreView) !== -1) _lastMoreView = _savedMoreView;
} catch(e) {}

function _mobileTabSlot(viewId) {
    return MORE_VIEWS.indexOf(viewId) !== -1 ? 'more' : viewId;
}

function _viewForMobileTabSlot(slot) {
    return slot === 'more' ? (_lastMoreView || DEFAULT_MORE_VIEW) : slot;
}

// Legacy hashes that predate the current view names.
var VIEW_ALIASES = {
    'identity': 'settings',
    'eventlog': 'dashboard',
    'propagation': 'network'
};

var _navTransitioning = false;
var _navInitialLoad = true;

var TRANSITION_CLASSES = ['entering', 'exiting', 'slide-in-right', 'slide-out-left', 'slide-in-left', 'slide-out-right'];

var _prefersReducedMotion = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
if (window.matchMedia) {
    window.matchMedia('(prefers-reduced-motion: reduce)').addEventListener('change', function(e) {
        _prefersReducedMotion = e.matches;
    });
}

// Accepts duration in ms or a vibrate-pattern array. WKWebView lacks
// navigator.vibrate; mobile routes through tauri-plugin-haptics on both
// iOS and Android. Browser fallback uses navigator.vibrate.
function haptic(pattern) {
    if (!isTauriMobile()) {
        if (navigator.vibrate) navigator.vibrate(pattern);
        return;
    }
    _dispatchHaptic(pattern);
}

function _dispatchHaptic(pattern) {
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

// Boundaries: ≤12 light, ≤22 medium, ≤35 heavy, 40+ error.
function _patternToHaptic(ms) {
    if (ms <= 12)  return { kind: 'impact',  payload: { style: 'light'  } };
    if (ms <= 22)  return { kind: 'impact',  payload: { style: 'medium' } };
    if (ms <= 35)  return { kind: 'impact',  payload: { style: 'heavy'  } };
    return         { kind: 'notify', payload: { type: 'error' } };
}

function _fireHapticStep(step) {
    try {
        var method = step.kind === 'impact'  ? 'impactFeedback'
                   : step.kind === 'notify'  ? 'notificationFeedback'
                   :                           'selectionFeedback';
        _rsHapticsInvoke(method, { payload: step.payload });
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
    if (!oldView || !newView || oldView === newView || !isMobile()) {
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

function switchView(viewId, opts) {
    if (VIEW_ALIASES && VIEW_ALIASES[viewId]) viewId = VIEW_ALIASES[viewId];
    if (VIEWS.indexOf(viewId) === -1) viewId = 'dashboard';
    if (viewId === currentView && !_navInitialLoad) return;
    if (_navTransitioning) return;

    opts = opts || {};
    var previousView = currentView;
    var oldEl = document.getElementById('view-' + previousView);
    var newEl = document.getElementById('view-' + viewId);

    var transitionType = 'fade';
    if (!_navInitialLoad && isMobile()) {
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

    document.querySelectorAll('.nav-item').forEach(function(item) {
        item.classList.remove('active');
        if (item.dataset.view === viewId) item.classList.add('active');
    });
    var isMoreView = MORE_VIEWS.indexOf(viewId) !== -1;
    if (isMoreView) {
        _lastMoreView = viewId;
        try { localStorage.setItem('ratspeak_more_view', viewId); } catch(e) {}
    }
    document.querySelectorAll('.bottom-bar-item').forEach(function(item) {
        item.classList.remove('active');
        if (item.dataset.view === viewId) item.classList.add('active');
    });
    var hamburger = document.getElementById('bottom-bar-hamburger');
    if (hamburger) {
        if (isMoreView) hamburger.classList.add('active');
        else hamburger.classList.remove('active');
    }
    document.querySelectorAll('.bottom-sheet-item').forEach(function(item) {
        item.classList.remove('active');
        if (item.dataset.view === viewId) item.classList.add('active');
    });

    // Dismiss keyboard so new view doesn't inherit stale focus/viewport.
    var active = document.activeElement;
    if (active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA')) {
        active.blur();
    }

    ['peers-search', 'contacts-search'].forEach(function(id) {
        var el = document.getElementById(id);
        if (el && el.value) {
            el.value = '';
            el.dispatchEvent(new Event('input'));
        }
    });
    var msgSearch = document.getElementById('msg-search-input');
    if (msgSearch && msgSearch.value) {
        msgSearch.value = '';
        var sr = document.getElementById('msg-search-results');
        var cl = document.getElementById('lxmf-conversations-list');
        if (sr) sr.style.display = 'none';
        if (cl) cl.style.display = '';
    }

    if (previousView === 'message' && viewId !== 'message') {
        if (typeof _removeGhostRow === 'function') _removeGhostRow();
        if (typeof window._closeFabDial === 'function') window._closeFabDial();
        // Pop chat-detail so view-chat-detail body/layout classes clear.
        var top = RS.viewStack.top();
        if (top && top.viewId === 'chat-detail') RS.viewStack.pop();
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

function _fireViewLifecycle(viewId) {
    clearViewDirty(viewId);
    if (viewId === 'dashboard') {
        if (typeof _connectionsThrottleTimer !== 'undefined' && _connectionsThrottleTimer) clearTimeout(_connectionsThrottleTimer);
        if (typeof _connectionsRenderScheduled !== 'undefined') _connectionsRenderScheduled = false;
        if (typeof _connectionsThrottleTimer !== 'undefined') _connectionsThrottleTimer = null;
        requestAnimationFrame(function() {
            if (typeof refreshConnectionsTable === 'function') refreshConnectionsTable();
        });
    }

    if (viewId === 'network') {
        requestAnimationFrame(function() {
            if (typeof lastStats !== 'undefined' && lastStats) {
                if (typeof renderNetworkOverview === 'function') renderNetworkOverview(lastStats);
            }
            if (typeof loadSettingsInterfacesWithRetry === 'function') loadSettingsInterfacesWithRetry(1);
        });
    }

    if (viewId === 'settings') {
        if (typeof loadIdentities === 'function') loadIdentities();
        if (typeof initThemeToggle === 'function') initThemeToggle();
    }

    if (viewId === 'games' && typeof gamesTabLoad === 'function') gamesTabLoad();
    if (viewId === 'peers' && typeof initPeersView === 'function') requestAnimationFrame(initPeersView);
    if (viewId === 'message') {
        // Cache-first; only re-fetch on stuck error state or empty initial load.
        var _convList = document.getElementById('lxmf-conversations-list');
        var _convStuck = _convList && (_convList.textContent || '').indexOf('Couldn') !== -1;
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
        if (typeof renderMsgProfileStrip === 'function') requestAnimationFrame(renderMsgProfileStrip);
    }

    if (viewId === 'network') {
        if (typeof loadIdentities === 'function') loadIdentities();
        if (typeof renderNetworkPulse === 'function' && typeof lastStats !== 'undefined' && lastStats) {
            requestAnimationFrame(function() { renderNetworkPulse(lastStats); });
        }
        if (typeof renderMergedConnections === 'function') renderMergedConnections();
        if (typeof renderNetworkContactList === 'function') renderNetworkContactList();
        if (typeof renderPropagationStatus === 'function') renderPropagationStatus('net-propagation-status');
    }

    if (viewId === 'contacts') {
        if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
        // Re-fetch if contacts_update event was missed.
        if (typeof lxmfContacts !== 'undefined' && lxmfContacts.length === 0) {
            RS.invoke('api_contacts').then(function(data) {
                if (Array.isArray(data) && data.length > 0) {
                    lxmfContacts = (typeof normalizeContactList === 'function') ? normalizeContactList(data) : data;
                    if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
                    if (typeof renderContactList === 'function') renderContactList();
                }
            }).catch(function() {});
        }
    }

    if (viewId === 'dashboard') {
        if (typeof renderDashboardRecentMessages === 'function') renderDashboardRecentMessages();
        if (typeof renderDashboardSummaries === 'function' && typeof lastStats !== 'undefined' && lastStats) renderDashboardSummaries(lastStats);
    }
}

function _initHistoryNavigation() {
    // Anchor prevents a back-swipe from landing on about:blank.
    history.replaceState({ view: currentView, anchor: true }, '', '#' + currentView);

    window.addEventListener('popstate', function(e) {
        var state = e.state;

        var sheet = document.getElementById('bottom-sheet');
        if (sheet && sheet.classList.contains('open')) {
            sheet.classList.remove('open');
            var sheetOverlay = document.getElementById('bottom-sheet-overlay');
            if (sheetOverlay) sheetOverlay.classList.remove('active');
            history.pushState({ view: currentView }, '', '#' + currentView);
            return;
        }

        var fabPicker = document.getElementById('fab-contact-picker-sheet');
        if (fabPicker && fabPicker.classList.contains('open')) {
            if (typeof closeFabContactPicker === 'function') closeFabContactPicker();
            history.pushState({ view: currentView }, '', '#' + currentView);
            return;
        }

        var contactSheet = document.getElementById('contact-detail-sheet');
        if (contactSheet) {
            contactSheet.remove();
            var contactOverlay = document.getElementById('contact-detail-overlay');
            if (contactOverlay) contactOverlay.remove();
            history.pushState({ view: currentView }, '', '#' + currentView);
            return;
        }

        // chat-detail / game-detail are tracked on the view-stack — pop()
        // clears their classes as a side effect.
        if (RS.viewStack.depth() > 1) {
            RS.viewStack.pop();
            history.pushState({ view: currentView }, '', '#' + currentView);
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

function showAboutModal() {
    var existing = document.getElementById('about-modal-overlay');
    if (existing) existing.remove();

    var overlay = document.createElement('div');
    overlay.id = 'about-modal-overlay';
    overlay.className = 'modal-overlay active';
    overlay.innerHTML =
        '<div class="modal" style="max-width:420px;">' +
            '<div class="modal-header">' +
                '<h3>About Ratspeak</h3>' +
                '<button class="modal-close" id="about-modal-close">&times;</button>' +
            '</div>' +
            '<div class="modal-body about-modal-body">' +
                '<p class="font-600 about-modal-title">Ratspeak <span class="mono text-muted-color about-modal-version">v1.0.3</span></p>' +
                '<p>Real-time dashboard for Reticulum mesh networks. Encrypted messaging, dynamic node management, and network health monitoring.</p>' +
                '<p class="about-modal-link-row">' +
                    '<a href="https://ratspeak.org" target="_blank" rel="noopener" class="text-link">ratspeak.org</a>' +
                    ' &middot; ' +
                    '<a href="https://reticulum.network" target="_blank" rel="noopener" class="text-link">reticulum.network</a>' +
                '</p>' +
            '</div>' +
        '</div>';
    document.body.appendChild(overlay);

    function close() { overlay.remove(); }
    document.getElementById('about-modal-close').addEventListener('click', close);
    overlay.addEventListener('click', function(e) {
        if (e.target === overlay) close();
    });

    var content = overlay.querySelector('.modal');
    if (content) {
        RS.gestures.attachDragDismiss(content, {
            axis: 'y',
            blockIfScrolled: true,
            onCommit: close
        });
    }
}

function initSidebarCollapse() {
    var btn = document.getElementById('sidebar-collapse-btn');
    var sidebar = document.getElementById('sidebar');
    if (!btn || !sidebar) return;

    var collapsed = false;
    try { collapsed = localStorage.getItem('rs-sidebar-collapsed') === '1'; } catch(e) {}
    if (collapsed) sidebar.classList.add('collapsed');

    btn.addEventListener('click', function() {
        sidebar.classList.toggle('collapsed');
        var isCollapsed = sidebar.classList.contains('collapsed');
        try { localStorage.setItem('rs-sidebar-collapsed', isCollapsed ? '1' : '0'); } catch(e) {}
        var icon = btn.querySelector('.nav-icon');
        if (icon) icon.style.transform = isCollapsed ? 'rotate(180deg)' : '';
    });
}

function initMobileSidebar() {
    var hamburger = document.getElementById('hamburger-btn');
    var sidebar = document.getElementById('sidebar');
    var overlay = document.getElementById('sidebar-overlay');
    if (!hamburger || !sidebar || !overlay) return;

    function openSidebar() {
        sidebar.classList.add('open');
        overlay.classList.add('active');
    }

    function closeSidebar() {
        sidebar.classList.remove('open');
        overlay.classList.remove('active');
    }

    hamburger.addEventListener('click', function(e) {
        e.stopPropagation();
        if (sidebar.classList.contains('open')) {
            closeSidebar();
        } else {
            openSidebar();
        }
    });

    overlay.addEventListener('click', closeSidebar);

    document.addEventListener('keydown', function(e) {
        if (e.key === 'Escape' && sidebar.classList.contains('open')) {
            closeSidebar();
        }
    });

    document.querySelectorAll('.nav-item').forEach(function(item) {
        item.addEventListener('click', function() {
            closeSidebar();
        });
    });
}

var _bbDidLongPress = false;

// Set by long-press completion; settings.js's `announce_triggered` listener
// reads it to render the burst centered on where the gesture happened.
var _pendingAnnounceOrigin = null;

function initBottomBar() {
    var bar = document.getElementById('bottom-bar');
    if (!bar) return;

    bar.querySelectorAll('.bottom-bar-item').forEach(function(item) {
        item.addEventListener('click', function(e) {
            e.preventDefault();
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
    var _sabPx = 0;
    function _readSab() {
        var v = getComputedStyle(document.documentElement).getPropertyValue('--sab');
        _sabPx = parseFloat(v) || 0;
    }
    _readSab();
    window.addEventListener('resize', _readSab);

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
            return _sabPx > 0 && touch.clientY > window.innerHeight - _sabPx;
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

            // Staged pattern reads as a tactile pop rather than a single buzz.
            haptic([0, 30, 50, 30]);

            var hit = document.elementFromPoint(touch.clientX, touch.clientY);
            var bbItem = (hit && hit.closest) ? hit.closest('.bottom-bar-item') : null;
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

function initSidebarCloseBtn() {
    var closeBtn = document.getElementById('sidebar-close-btn');
    var sidebar = document.getElementById('sidebar');
    var overlay = document.getElementById('sidebar-overlay');
    if (!closeBtn || !sidebar) return;

    closeBtn.addEventListener('click', function() {
        sidebar.classList.remove('open');
        if (overlay) overlay.classList.remove('active');
    });
}

function initSidebarSwipe() {
    var sidebar = document.getElementById('sidebar');
    var overlay = document.getElementById('sidebar-overlay');
    if (!sidebar) return;

    RS.gestures.attachSwipe(sidebar, {
        direction: 'left',
        distanceThreshold: RS.gestures.SWIPE_DISTANCE_PX,
        skipIf: function() {
            return typeof _isSetupActive === 'function' && _isSetupActive();
        },
        onProgress: function(dx) {
            if (dx < 0) {
                sidebar.style.transition = 'none';
                sidebar.style.transform = 'translateX(' + dx + 'px)';
            }
        },
        onCommit: function() {
            sidebar.style.transition = '';
            sidebar.style.transform = '';
            sidebar.classList.remove('open');
            if (overlay) overlay.classList.remove('active');
        },
        onCancel: function() {
            sidebar.style.transition = 'transform 0.2s cubic-bezier(0.2, 0, 0, 1)';
            sidebar.style.transform = '';
        }
    });
}

function initBottomSheet() {
    var trigger = document.getElementById('bottom-bar-hamburger');
    var sheet = document.getElementById('bottom-sheet');
    var overlay = document.getElementById('bottom-sheet-overlay');
    if (!trigger || !sheet || !overlay) return;

    function openSheet() {
        sheet.classList.add('open');
        overlay.classList.add('active');
        // Push state so OS back closes the sheet before navigating.
        history.pushState({ view: currentView, sheet: true }, '', '#' + currentView);
    }
    function closeSheet() {
        sheet.classList.remove('open');
        overlay.classList.remove('active');
    }

    trigger.addEventListener('click', function(e) {
        e.preventDefault();
        e.stopPropagation();
        sheet.classList.contains('open') ? closeSheet() : openSheet();
    });

    overlay.addEventListener('click', closeSheet);

    document.addEventListener('keydown', function(e) {
        if (e.key === 'Escape' && sheet.classList.contains('open')) closeSheet();
    });

    sheet.querySelectorAll('.bottom-sheet-item[data-view]').forEach(function(item) {
        item.addEventListener('click', function(e) {
            e.preventDefault();
            haptic(10);
            var targetView = this.dataset.view;
            closeSheet();
            // Let close animation clear before switching views.
            setTimeout(function() {
                var isDrillDown = TAB_VIEWS.indexOf(targetView) === -1;
                switchView(targetView, {
                    pushState: isDrillDown,
                    transition: isDrillDown ? 'slide-right' : undefined
                });
            }, 50);
        });
    });

}

// Wires swipe-down + overlay-tap dismissal in one call.
function initSheetSwipeDismiss(sheetId, overlayId, closeFn) {
    var sheet = document.getElementById(sheetId);
    if (!sheet) return;
    var overlay = overlayId ? document.getElementById(overlayId) : null;
    var close = closeFn || function() {};
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
var _maxViewportHeight = 0;

function initKeyboardDetection() {
    var bar = document.getElementById('bottom-bar');
    if (!window.visualViewport) return;

    var _prevKeyboardOpen = false;
    var _fullAppHeight = 0;

    function onResize() {
        var vv = window.visualViewport;
        var kbHeight = window.innerHeight - vv.height;
        var currentHeight = vv.height;

        if (currentHeight > _maxViewportHeight) {
            _maxViewportHeight = currentHeight;
        }

        // Samsung One UI resizes both viewports together (kbHeight stays ~0);
        // fall back to comparing against the tallest viewport we've seen.
        var heightDrop = _maxViewportHeight > 0 ? (_maxViewportHeight - currentHeight) : 0;
        var keyboardOpen = kbHeight > 150 || heightDrop > 150;
        var inChat = document.body.classList.contains('view-chat-detail');

        if (isMobile()) {
            if (keyboardOpen && inChat) {
                // WKWebView pushes content behind the notch on focus; clamp
                // --app-height and pin scrollTop to keep the chat header visible.
                document.documentElement.style.setProperty('--app-height', currentHeight + 'px');
                if (window.scrollY > 0 || document.documentElement.scrollTop > 0) {
                    window.scrollTo(0, 0);
                }
            } else if (!keyboardOpen) {
                _fullAppHeight = currentHeight;
                document.documentElement.style.setProperty('--app-height', currentHeight + 'px');
            }
            // Outside chat: leave --app-height alone so other inputs don't reflow.
        }

        if (keyboardOpen) {
            if (bar) bar.classList.add('keyboard-open');
            document.documentElement.classList.add('keyboard-open');

            if (_waitingForKeyboard) {
                clearTimeout(_keyboardStableTimer);
                _keyboardStableTimer = setTimeout(function() {
                    _waitingForKeyboard = false;
                    var msgContainer = document.getElementById('lxmf-messages');
                    if (msgContainer) msgContainer.scrollTop = msgContainer.scrollHeight;
                }, 100);
            }
        } else {
            if (bar) bar.classList.remove('keyboard-open');
            document.documentElement.classList.remove('keyboard-open');
            _waitingForKeyboard = false;
            clearTimeout(_keyboardStableTimer);
        }

        _prevKeyboardOpen = keyboardOpen;
    }

    window.visualViewport.addEventListener('resize', onResize);
    window.visualViewport.addEventListener('scroll', function() {
        // Pin scroll while chat keyboard is open; WKWebView otherwise scrolls
        // the header behind the notch as the viewport pans.
        var inChat = document.body.classList.contains('view-chat-detail');
        if (document.documentElement.classList.contains('keyboard-open') && inChat) {
            if (window.scrollY > 0 || document.documentElement.scrollTop > 0) {
                window.scrollTo(0, 0);
            }
        }
        onResize();
    });
    onResize();

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
    // chat compose pins messages to bottom, modal/other inputs scrollIntoView.
    document.addEventListener('focusin', function(e) {
        var el = e.target;
        if (el.tagName !== 'INPUT' && el.tagName !== 'TEXTAREA') return;

        if (el.closest('.connections-header, .lxmf-sidebar, .contacts-standalone')) {
            return;
        }

        if (el.id === 'lxmf-input') {
            _waitingForKeyboard = true;
            return;
        }

        if (el.closest('.modal, .bottom-sheet')) {
            setTimeout(function() {
                el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
            }, 150);
            return;
        }

        setTimeout(function() {
            el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
        }, 300);
    });

    document.addEventListener('focusout', function(e) {
        var el = e.target;
        if (el.id === 'lxmf-input') {
            _waitingForKeyboard = false;
            clearTimeout(_keyboardStableTimer);
        }
    });
}

function initTextareaAutoGrow() {
    var textarea = document.getElementById('lxmf-input');
    if (!textarea) return;

    var _growRaf = null;
    textarea.addEventListener('input', function() {
        var ta = this;
        ta.style.height = 'auto';
        ta.style.height = Math.min(ta.scrollHeight, 124) + 'px';
        // rAF so scroll happens after the browser applies the new height.
        if (document.documentElement.classList.contains('keyboard-open')) {
            if (_growRaf) cancelAnimationFrame(_growRaf);
            _growRaf = requestAnimationFrame(function() {
                _growRaf = null;
                var msgContainer = document.getElementById('lxmf-messages');
                if (msgContainer) msgContainer.scrollTop = msgContainer.scrollHeight;
            });
        }
    });
}

function initDrillDownSwipeBack() {
    if (!isMobile()) return;

    function _animateOutAndPop() {
        var viewEl = document.getElementById('view-' + currentView);
        function _doPop() {
            if (RS.viewStack.depth() > 1) {
                RS.viewStack.pop();
                return;
            }
            // Drill-down reached via plain switchView (no push) → go back to last-tab.
            var lastTab = 'dashboard';
            try {
                var saved = localStorage.getItem('ratspeak_view');
                if (saved && TAB_VIEWS.indexOf(saved) !== -1) lastTab = saved;
            } catch(e) {}
            if (TAB_VIEWS.indexOf(lastTab) === -1) lastTab = 'dashboard';
            switchView(lastTab, { back: true });
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
        skipIf: function(e) {
            if (_navTransitioning) return true;
            if (e.target.closest('button, a, input, select, .selector-badge')) return true;
            // Fire when something's on the stack OR sitting on a legacy drill-down.
            if (RS.viewStack.depth() > 1) return false;
            if (RS.gestures.DRILL_DOWN_VIEWS.indexOf(currentView) !== -1) return false;
            return true;
        },
        onProgress: function(dx) {
            var viewEl = document.getElementById('view-' + currentView);
            if (viewEl && dx > 0) {
                viewEl.style.transition = 'none';
                viewEl.style.transform = 'translateX(' + dx + 'px)';
                viewEl.style.opacity = Math.max(0.3, 1 - dx / RS.gestures.DRAG_DISMISS_OPACITY_DENOM_PX);
            }
        },
        onCommit: _animateOutAndPop,
        onCancel: function() {
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

function initEdgeSwipeOpenSidebar() {
    // Dwell-then-swipe gate prevents scroll-flick from the left edge
    // accidentally opening the sidebar. Mobile uses bottom bar instead.
    if (isMobile()) return;
    var sidebar = document.getElementById('sidebar');
    var overlay = document.getElementById('sidebar-overlay');
    if (!sidebar) return;

    RS.gestures.attachSwipe(document, {
        direction: 'right',
        edgeZone: RS.gestures.EDGE_ZONE_PX,
        dwellMs: RS.gestures.SWIPE_DWELL_SIDEBAR_OPEN_MS,
        skipIf: function() {
            if (typeof _isSetupActive === 'function' && _isSetupActive()) return true;
            return sidebar.classList.contains('open');
        },
        hapticAt: { dwellHit: 'light' },
        onCommit: function() {
            sidebar.classList.add('open');
            if (overlay) overlay.classList.add('active');
        }
    });
}

function initTabSwipe() {
    if (!isMobile()) return;

    RS.gestures.attachSwipe(document, {
        direction: 'horizontal',
        edgeMargin: RS.gestures.EDGE_MARGIN_TAB_SWIPE_PX,
        distanceThreshold: RS.gestures.SWIPE_DISTANCE_PX,
        skipIf: function(e) {
            if (typeof _isSetupActive === 'function' && _isSetupActive()) return true;
            if (_navTransitioning) return true;
            if (MOBILE_TAB_SLOTS.indexOf(_mobileTabSlot(currentView)) === -1) return true;
            if (e.target.closest('button, a, input, select, .selector-badge')) return true;
            // Conversation rows own horizontal swipes for message actions. The
            // document-level tab recognizer must not also navigate tabs.
            if (e.target.closest('.conv-row, .conv-swipe-delete')) return true;
            // Active chat and game sessions own horizontal gestures.
            var lxmfLayout = document.querySelector('.lxmf-layout');
            if (lxmfLayout && lxmfLayout.classList.contains('view-chat-detail')) return true;
            var gamesLayout = document.querySelector('.games-layout');
            if (gamesLayout && gamesLayout.classList.contains('view-game-detail')) return true;
            return false;
        },
        onCommit: function(_target, dx) {
            var currentIdx = MOBILE_TAB_SLOTS.indexOf(_mobileTabSlot(currentView));
            if (currentIdx === -1) return;
            var nextIdx = dx < 0 ? currentIdx + 1 : currentIdx - 1;
            if (nextIdx < 0 || nextIdx >= MOBILE_TAB_SLOTS.length) return;
            var targetView = _viewForMobileTabSlot(MOBILE_TAB_SLOTS[nextIdx]);
            if (!targetView || targetView === currentView) return;
            haptic(10);
            switchView(targetView);
        }
    });
}

var _firstRunDismiss = null;

function showFirstRunTooltip() {
    if (localStorage.getItem('ratspeak_first_run')) return;
    var setupView = document.getElementById('view-setup');
    if (setupView && setupView.classList.contains('active')) return;
    // Hint points at the bottom-bar announce button (mobile only).
    var bar = document.querySelector('.bottom-bar');
    if (!bar || getComputedStyle(bar).display === 'none') return;

    var hint = document.createElement('div');
    hint.className = 'first-run-hint';
    hint.innerHTML = '<span class="first-run-hint-icon"><svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M2 12 7 2l5 10-5 10z"/><path d="M7 12h15"/><path d="M13 5l9 7-9 7"/></svg></span>' +
        '<span class="first-run-hint-text">Press and hold to announce.</span>';
    document.body.appendChild(hint);

    // Double-rAF so the initial-state paint happens before transition.
    requestAnimationFrame(function() {
        requestAnimationFrame(function() { hint.classList.add('visible'); });
    });

    var dismissed = false;
    function dismiss() {
        if (dismissed) return;
        dismissed = true;
        _firstRunDismiss = null;
        hint.classList.add('dismissing');
        hint.classList.remove('visible');
        setTimeout(function() { hint.remove(); }, 400);
        localStorage.setItem('ratspeak_first_run', 'done');
    }

    _firstRunDismiss = dismiss;
    hint.addEventListener('click', dismiss);
    setTimeout(dismiss, 5000);

    RS.listen('announce_triggered', function(data) {
        if (data && data.success && _firstRunDismiss) _firstRunDismiss();
    });
}

document.addEventListener('DOMContentLoaded', function() {
    document.querySelectorAll('.nav-item').forEach(function(item) {
        item.addEventListener('click', function(e) {
            e.preventDefault();
            switchView(this.dataset.view);
        });
    });

    initSidebarCollapse();
    initMobileSidebar();
    initBottomBar();
    initBottomSheet();
    initSidebarCloseBtn();
    initSidebarSwipe();
    initKeyboardDetection();
    initTextareaAutoGrow();
    _initHistoryNavigation();
    RS.gestures.attachRipple(document, {
        selectors: RS.gestures.RIPPLE_SELECTORS,
        hapticOnTap: 'light'
    });
    initDrillDownSwipeBack();
    initEdgeSwipeOpenSidebar();
    initTabSwipe();

    [
        { id: 'bottom-sheet', overlayId: 'bottom-sheet-overlay', closeFn: function() {
            var s = document.getElementById('bottom-sheet');
            var o = document.getElementById('bottom-sheet-overlay');
            if (s) s.classList.remove('open');
            if (o) o.classList.remove('active');
        } },
        { id: 'conn-detail-sheet', overlayId: 'conn-detail-sheet-overlay', closeFn: function() {
            if (typeof closeConnectionDetailSheet === 'function') closeConnectionDetailSheet();
        } },
        { id: 'conn-sort-sheet', overlayId: 'conn-sort-sheet-overlay', closeFn: function() {
            if (typeof closeSortSheet === 'function') closeSortSheet();
        } },
        { id: 'fab-contact-picker-sheet', overlayId: 'fab-contact-picker-overlay', closeFn: function() {
            if (typeof closeFabContactPicker === 'function') closeFabContactPicker();
        } },
        { id: 'iface-action-sheet', overlayId: 'iface-action-overlay', closeFn: function() {
            if (typeof closeInterfaceActionSheet === 'function') closeInterfaceActionSheet();
        } }
    ].forEach(function(cfg) {
        initSheetSwipeDismiss(cfg.id, cfg.overlayId, cfg.closeFn);
    });

    // Delay so initial layout settles before hint paints over it.
    setTimeout(showFirstRunTooltip, 2000);

    if (typeof needsSetup !== 'undefined' && needsSetup) return;

    // Mobile lands on peers; desktop: hash -> last-saved view -> dashboard.
    if (isMobile()) {
        switchView('peers');
    } else {
        var hash = window.location.hash.replace('#', '');
        if (hash && VIEWS.indexOf(hash) !== -1) {
            switchView(hash);
        } else {
            var saved = null;
            try { saved = localStorage.getItem('ratspeak_view'); } catch(e) {}
            if (saved && VIEWS.indexOf(saved) !== -1) {
                switchView(saved);
            } else {
                switchView('dashboard');
            }
        }
    }

    _navInitialLoad = false;
});
