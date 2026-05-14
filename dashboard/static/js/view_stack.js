// Push/pop view navigation. Stack is in-memory only; main tab persists
// via localStorage('ratspeak_view'), drill-down history is ephemeral.

var RS = window.RS || {};
RS.viewStack = RS.viewStack || {};
RS._viewStack = RS._viewStack || [];

(function() {
    var V = RS.viewStack;

    function _toggleViewClass(layoutSelector, className, remove) {
        var l = document.querySelector(layoutSelector);
        if (l) {
            if (remove) l.classList.remove(className);
            else        l.classList.add(className);
        }
        if (remove) document.body.classList.remove(className);
        else        document.body.classList.add(className);
    }

    var CLASS_TOGGLE_VIEWS = {
        'chat-detail': function(remove) {
            _toggleViewClass('.lxmf-layout', 'view-chat-detail', remove);
        },
        'game-detail': function(remove) {
            _toggleViewClass('.games-layout', 'view-game-detail', remove);
        }
    };

    function _captureScroll(viewId) {
        var el = document.getElementById('view-' + viewId);
        return el ? el.scrollTop : 0;
    }

    function _ensureRoot() {
        if (RS._viewStack.length === 0) {
            var rootView = (typeof currentView === 'string') ? currentView : 'dashboard';
            RS._viewStack.push({ viewId: rootView, scrollY: 0, opts: {} });
        }
    }

    V.push = function(viewId, opts) {
        opts = opts || {};
        _ensureRoot();
        var top = RS._viewStack[RS._viewStack.length - 1];
        if (top) top.scrollY = _captureScroll(top.viewId);
        RS._viewStack.push({ viewId: viewId, scrollY: 0, opts: opts });
        // chat-detail / game-detail are stack-only sentinel views handled
        // via class toggles; real views dispatch through switchView.
        var classToggle = CLASS_TOGGLE_VIEWS[viewId];
        if (classToggle) {
            classToggle(false);
        } else {
            if (typeof switchView === 'function') switchView(viewId, opts);
        }
    };

    V.pop = function(opts) {
        opts = opts || {};
        if (RS._viewStack.length <= 1) return;
        var popped = RS._viewStack.pop();
        var prev = RS._viewStack[RS._viewStack.length - 1];

        var classOff = CLASS_TOGGLE_VIEWS[popped.viewId];
        if (classOff) {
            classOff(true);
            if (popped.viewId === 'chat-detail' && typeof _onChatDetailExit === 'function') {
                _onChatDetailExit(popped);
            }
            if (typeof opts.onPop === 'function') opts.onPop();
            return;
        }

        if (typeof switchView === 'function') {
            switchView(prev.viewId, { back: true });
            // _animateViewSwitch is async — defer scroll restore one frame.
            requestAnimationFrame(function() {
                var prevEl = document.getElementById('view-' + prev.viewId);
                if (prevEl) prevEl.scrollTop = prev.scrollY;
            });
        }
        if (typeof opts.onPop === 'function') opts.onPop();
    };

    V.replace = function(viewId, opts) {
        opts = opts || {};
        if (RS._viewStack.length === 0) {
            RS._viewStack.push({ viewId: viewId, scrollY: 0, opts: opts });
        } else {
            RS._viewStack[RS._viewStack.length - 1] = { viewId: viewId, scrollY: 0, opts: opts };
        }
        if (!CLASS_TOGGLE_VIEWS[viewId]) {
            if (typeof switchView === 'function') switchView(viewId, opts);
        }
    };

    V.depth = function() { return RS._viewStack.length; };

    V.top = function() {
        var s = RS._viewStack;
        return s.length ? s[s.length - 1] : null;
    };

    // chat-detail / game-detail bind their own attachSwipe scoped to their
    // container instead of using this document-level binding.
    V.attachBackGesture = function() {
        if (!RS.gestures || typeof RS.gestures.attachSwipe !== 'function') return;
        return RS.gestures.attachSwipe(document, {
            direction: 'right',
            edgeZone: RS.gestures.EDGE_ZONE_PX,
            distanceThreshold: RS.gestures.SWIPE_DISTANCE_DRILLBACK_PX,
            hapticAt: { commit: 'selection' },
            skipIf: function() { return V.depth() <= 1; },
            onCommit: function() { V.pop(); }
        });
    };
})();
