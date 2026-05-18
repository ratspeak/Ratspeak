// Gesture primitives: attachSwipe, attachLongPress, attachDragDismiss,
// attachPullToRefresh, attachRipple. Thresholds from RS.gestures.* (constants.js);
// haptics route through nav.js::haptic(). Pinch-zoom blocking is in no_pinch.js.

var RS = window.RS || {};
RS.gestures = RS.gestures || {};

(function() {
    var G = RS.gestures;

    // Names map to native feedback semantics in nav.js::haptic().
    function _hapticByName(name) {
        if (!name) return;
        if (typeof haptic === 'function') haptic(name);
    }
    G._hapticByName = _hapticByName;

    G.bindViewFabClick = function(target, handler, opts) {
        var el = (typeof target === 'string') ? document.getElementById(target) : target;
        if (!el) return null;
        opts = opts || {};
        var feedback = opts.haptic || 'selection';
        el.addEventListener('click', function(e) {
            _hapticByName(feedback);
            if (typeof handler === 'function') return handler.call(this, e);
        });
        return el;
    };

    function _prefersReducedMotionNow() {
        return window.matchMedia &&
               window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    }

    // attachRipple — pointerdown ripple over a delegated selector list.
    G.attachRipple = function(rootEl, opts) {
        opts = opts || {};
        var root = rootEl || document;
        var selectors = opts.selectors || G.RIPPLE_SELECTORS;
        var hapticSelectors = opts.hapticSelectors || G.RIPPLE_HAPTIC_SELECTORS || selectors;
        var hapticOnTap = (opts.hapticOnTap === undefined) ? 'light' : opts.hapticOnTap;

        if (!isMobile()) return function() {};

        function _spawnRipple(event, element) {
            if (!element || _prefersReducedMotionNow()) return;
            var rect = element.getBoundingClientRect();
            var size = Math.max(rect.width, rect.height) * 2;
            var px = (event.clientX || (event.touches && event.touches[0] && event.touches[0].clientX) || (rect.left + rect.width / 2));
            var py = (event.clientY || (event.touches && event.touches[0] && event.touches[0].clientY) || (rect.top + rect.height / 2));
            var x = px - rect.left - size / 2;
            var y = py - rect.top - size / 2;

            var ripple = document.createElement('span');
            ripple.className = 'ripple';
            ripple.style.width = ripple.style.height = size + 'px';
            ripple.style.left = x + 'px';
            ripple.style.top = y + 'px';

            element.classList.add('ripple-host');
            element.appendChild(ripple);

            ripple.addEventListener('animationend', function() { ripple.remove(); });
            // Backstop in case animationend never fires (e.g. visibility loss).
            setTimeout(function() { if (ripple.parentNode) ripple.remove(); }, 500);
        }

        function _onPointerDown(e) {
            var target = e.target;
            for (var i = 0; i < selectors.length; i++) {
                var el = target.closest && target.closest(selectors[i]);
                if (el) {
                    _spawnRipple(e, el);
                    if (hapticOnTap && _matchesAnySelector(el, hapticSelectors)) {
                        _hapticByName(hapticOnTap);
                    }
                    break;
                }
            }
        }

        function _matchesAnySelector(el, selectorList) {
            if (!el || !selectorList) return false;
            for (var i = 0; i < selectorList.length; i++) {
                if (el.matches && el.matches(selectorList[i])) return true;
            }
            return false;
        }

        root.addEventListener('pointerdown', _onPointerDown, { passive: true });

        return function detach() {
            root.removeEventListener('pointerdown', _onPointerDown);
        };
    };

    // attachLongPress — duration-based hold with drift cancel + staged haptics.
    G.attachLongPress = function(el, opts) {
        opts = opts || {};
        var duration = opts.duration || G.LONG_PRESS_BOTTOM_BAR_MS;
        var delayMs = opts.delayMs || 0;
        var moveCancelPx = opts.moveCancelPx || G.LONG_PRESS_MOVE_CANCEL_PX;
        var moveCancelSq = moveCancelPx * moveCancelPx;
        var excludeZone = opts.excludeZone || null;
        var hapticStages = (opts.hapticStages || []).slice();
        var onStart = opts.onStart || function() {};
        var onProgress = opts.onProgress || function() {};
        var onFire = opts.onFire || function() {};
        var onCancel = opts.onCancel || function() {};

        var raf = null;
        var startT = 0;
        var fired = false;
        var startTouch = null;
        var moves = 0;
        var firedStages = [];

        function _loop(now) {
            if (!startT) return;
            var elapsed = now - startT;
            var progress = Math.min(elapsed / duration, 1);

            if (elapsed >= delayMs) {
                onProgress(progress, startTouch);
                for (var i = 0; i < hapticStages.length; i++) {
                    var stage = hapticStages[i];
                    if (!firedStages[i] && progress >= stage.at) {
                        firedStages[i] = true;
                        _hapticByName(stage.level);
                    }
                }
            }

            if (progress >= 1 && !fired) {
                fired = true;
                try { onFire(startTouch); } finally { _reset(false); }
                return;
            }

            raf = requestAnimationFrame(_loop);
        }

        function _reset(notifyCancel) {
            if (raf) { cancelAnimationFrame(raf); raf = null; }
            var hadStart = !!startT;
            startT = 0;
            firedStages = [];
            if (hadStart && !fired && notifyCancel) onCancel();
        }

        function _onTouchStart(e) {
            fired = false;
            moves = 0;
            startTouch = e.touches[0];
            if (!startTouch) return;

            if (excludeZone && excludeZone(startTouch)) {
                startT = 0;
                startTouch = null;
                return;
            }

            startT = performance.now();
            onStart(e);
            raf = requestAnimationFrame(_loop);
        }

        function _onTouchEnd() {
            if (!fired) _reset(true);
        }

        function _onTouchMove(e) {
            if (fired || !startT) return;
            var t = e.touches[0];
            if (!t || !startTouch) return;
            var dx = t.clientX - startTouch.clientX;
            var dy = t.clientY - startTouch.clientY;
            moves++;

            // Early upward flick = system home gesture claiming the touch.
            if (moves <= 3 && dy < -8 && Math.abs(dy) > Math.abs(dx)) {
                _reset(true);
                return;
            }
            if (dx * dx + dy * dy > moveCancelSq) _reset(true);
        }

        function _onTouchCancel() { _reset(true); }
        function _onVisibilityChange() { if (document.hidden) _reset(true); }

        el.addEventListener('touchstart', _onTouchStart, { passive: true });
        el.addEventListener('touchend', _onTouchEnd);
        el.addEventListener('touchmove', _onTouchMove);
        el.addEventListener('touchcancel', _onTouchCancel);
        document.addEventListener('visibilitychange', _onVisibilityChange);

        return function detach() {
            _reset(false);
            el.removeEventListener('touchstart', _onTouchStart);
            el.removeEventListener('touchend', _onTouchEnd);
            el.removeEventListener('touchmove', _onTouchMove);
            el.removeEventListener('touchcancel', _onTouchCancel);
            document.removeEventListener('visibilitychange', _onVisibilityChange);
        };
    };

    // attachSwipe — directional recognizer with optional edge gating,
    // dwell pre-condition, distance + velocity thresholds.
    G.attachSwipe = function(el, opts) {
        opts = opts || {};
        var direction = opts.direction || 'horizontal';
        var distanceThreshold = opts.distanceThreshold || G.SWIPE_DISTANCE_PX;
        var velocityThreshold = opts.velocityThreshold || G.SWIPE_VELOCITY_PX_MS;
        var skipIf = opts.skipIf || null;
        // edgeZone: only touches in the leading-edge strip are recognized.
        var edgeZone = opts.edgeZone || 0;
        // edgeMargin: inverse — touches inside the margin are rejected.
        var edgeMargin = opts.edgeMargin || 0;
        // dwellMs: must remain still inside edgeZone for dwellMs before arming.
        var dwellMs = opts.dwellMs || 0;
        var DWELL_MOVE_TOLERANCE = 6;
        var delegated = opts.delegated || null;
        var hapticAt = opts.hapticAt || {};
        var onProgress = opts.onProgress || function() {};
        var onCommit = opts.onCommit || function() {};
        var onCancel = opts.onCancel || function() {};

        var startX = 0, startY = 0, startT = 0, tracking = false, gestureTarget = null;
        var dwellTimer = null, dwellArmed = false;

        function _passesEdgeGate(touch) {
            if (edgeZone > 0) {
                if (direction === 'right') return touch.clientX <= edgeZone;
                if (direction === 'left')  return touch.clientX >= window.innerWidth - edgeZone;
                if (direction === 'up')    return touch.clientY >= window.innerHeight - edgeZone;
                if (direction === 'down')  return touch.clientY <= edgeZone;
                // 'horizontal' accepts either edge.
                return touch.clientX <= edgeZone ||
                       touch.clientX >= window.innerWidth - edgeZone;
            }
            if (edgeMargin > 0) {
                if (direction === 'horizontal' || direction === 'left' || direction === 'right') {
                    return touch.clientX >= edgeMargin &&
                           touch.clientX <= window.innerWidth - edgeMargin;
                }
                return touch.clientY >= edgeMargin &&
                       touch.clientY <= window.innerHeight - edgeMargin;
            }
            return true;
        }

        function _clearDwell() {
            if (dwellTimer) { clearTimeout(dwellTimer); dwellTimer = null; }
        }

        function _onTouchStart(e) {
            if (skipIf && skipIf(e)) return;
            var t = e.touches[0];
            if (!t) return;
            if (!_passesEdgeGate(t)) return;
            // matched element is passed to onCommit so consumer knows the target.
            if (delegated) {
                gestureTarget = e.target.closest && e.target.closest(delegated);
                if (!gestureTarget) return;
            } else {
                gestureTarget = e.target;
            }
            startX = t.clientX;
            startY = t.clientY;
            startT = performance.now();
            tracking = true;
            if (dwellMs > 0) {
                dwellArmed = false;
                _clearDwell();
                dwellTimer = setTimeout(function() {
                    dwellArmed = true;
                    if (hapticAt.dwellHit) _hapticByName(hapticAt.dwellHit);
                }, dwellMs);
            } else {
                dwellArmed = true;
            }
        }

        function _onTouchMove(e) {
            if (!tracking) return;
            var t = e.touches[0];
            if (!t) return;
            var dx = t.clientX - startX;
            var dy = t.clientY - startY;
            // Movement beyond DWELL_MOVE_TOLERANCE before dwell fires aborts.
            if (dwellMs > 0 && !dwellArmed) {
                if (Math.abs(dx) > DWELL_MOVE_TOLERANCE ||
                    Math.abs(dy) > DWELL_MOVE_TOLERANCE) {
                    tracking = false;
                    _clearDwell();
                    onCancel(dx, dy);
                }
                return;
            }
            // Axis-lock: perpendicular drift > parallel = user is scrolling, abort.
            var horizontal = (direction === 'left' || direction === 'right' || direction === 'horizontal');
            if (horizontal && Math.abs(dy) > Math.abs(dx)) {
                tracking = false;
                onCancel(dx, dy, gestureTarget);
                return;
            }
            if (!horizontal && Math.abs(dx) > Math.abs(dy)) {
                tracking = false;
                onCancel(dx, dy, gestureTarget);
                return;
            }
            var progress = _signedProgress(direction, dx, dy) / distanceThreshold;
            onProgress(dx, dy, Math.max(0, Math.min(1, progress)), gestureTarget);
        }

        function _onTouchEnd(e) {
            if (!tracking) return;
            tracking = false;
            _clearDwell();
            var t = (e.changedTouches && e.changedTouches[0]) || null;
            var dx = t ? t.clientX - startX : 0;
            var dy = t ? t.clientY - startY : 0;
            var elapsed = Math.max(1, performance.now() - startT);
            var distance = _signedProgress(direction, dx, dy);
            var velocity = distance / elapsed;
            if (dwellMs > 0 && !dwellArmed) {
                onCancel(dx, dy);
                return;
            }
            if (distance >= distanceThreshold || velocity >= velocityThreshold) {
                if (hapticAt.commit) _hapticByName(hapticAt.commit);
                onCommit(gestureTarget, dx, dy, velocity);
            } else {
                if (hapticAt.cancel) _hapticByName(hapticAt.cancel);
                onCancel(dx, dy, gestureTarget);
            }
        }

        function _onTouchCancel() {
            if (!tracking) return;
            tracking = false;
            _clearDwell();
            onCancel(0, 0);
        }

        // Negative/off-axis travel reads as zero.
        function _signedProgress(dir, dx, dy) {
            if (dir === 'left')       return Math.max(0, -dx);
            if (dir === 'right')      return Math.max(0,  dx);
            if (dir === 'up')         return Math.max(0, -dy);
            if (dir === 'down')       return Math.max(0,  dy);
            if (dir === 'horizontal') return Math.abs(dx);
            return 0;
        }

        el.addEventListener('touchstart', _onTouchStart, { passive: true });
        el.addEventListener('touchmove', _onTouchMove, { passive: true });
        el.addEventListener('touchend', _onTouchEnd);
        el.addEventListener('touchcancel', _onTouchCancel);

        return function detach() {
            tracking = false;
            el.removeEventListener('touchstart', _onTouchStart);
            el.removeEventListener('touchmove', _onTouchMove);
            el.removeEventListener('touchend', _onTouchEnd);
            el.removeEventListener('touchcancel', _onTouchCancel);
        };
    };

    // Shared drag-dismiss behavior for sheets and modals. Keeps threshold,
    // rubber-band, scroll-blocking, and haptic behavior in one place.
    G.attachDragDismiss = function(el, opts) {
        opts = opts || {};
        var axis = opts.axis || 'y';
        var handle = el;
        if (opts.handleSelector) {
            var queried = el.querySelector(opts.handleSelector);
            if (queried) {
                handle = queried;
                // touch-action:none on the handle only — body stays scrollable.
                handle.style.touchAction = 'none';
            }
        }
        var threshold = opts.dismissThreshold || G.DRAG_DISMISS_THRESHOLD_PX;
        var opacityDenom = G.DRAG_DISMISS_OPACITY_DENOM_PX;
        var blockIfScrolled = (opts.blockIfScrolled !== false);
        var hapticAt = opts.hapticAt || {};
        var rubberBand = !!opts.rubberBand;
        var rubberBandFactor = G.PULL_TO_REFRESH_RUBBER_BAND_FACTOR;
        var parallaxOverlay = opts.parallaxOverlay || null;
        var skipIf = opts.skipIf || null;
        var snapPoints = opts.snapPoints || [0, 1];
        if (snapPoints.length > 2) {
            window.RS.diag('warn', '[gestures] attachDragDismiss: multi-snap snapPoints not yet implemented; treating as binary [0, 1]', snapPoints);
        }

        var onProgress = opts.onProgress || function() {};
        var onCommit = opts.onCommit || function() {};
        var onCancel = opts.onCancel || function() {};

        var startX = 0, startY = 0, currentDelta = 0, tracking = false, dragging = false, startT = 0;
        var scrollOwner = null, scrollLocked = false;
        var DRAG_ACTIVATE_PX = 4;

        function _readDelta(t) {
            if (axis === 'y') return t.clientY - startY;
            return t.clientX - startX;
        }

        function _scrollPos(node) {
            if (!node) return 0;
            return axis === 'y' ? node.scrollTop : node.scrollLeft;
        }

        function _isScrollable(node) {
            if (!node || node.nodeType !== 1) return false;
            var style = window.getComputedStyle ? window.getComputedStyle(node) : null;
            var overflow = style ? (axis === 'y' ? style.overflowY : style.overflowX) : '';
            if (!/(auto|scroll|overlay)/.test(overflow)) return false;
            var scrollSize = axis === 'y' ? node.scrollHeight : node.scrollWidth;
            var clientSize = axis === 'y' ? node.clientHeight : node.clientWidth;
            return scrollSize > clientSize + 1;
        }

        function _findScrollableOwner(target) {
            var preferredSelector = opts.scrollContainerSelector || '[data-sheet-scroll], .bottom-sheet-body';
            if (target && target.closest && preferredSelector) {
                var closestPreferred = target.closest(preferredSelector);
                if (closestPreferred && el.contains(closestPreferred) && _isScrollable(closestPreferred)) {
                    return closestPreferred;
                }
            }

            var node = target;
            while (node && node !== document && node !== el.parentNode) {
                if (node.nodeType === 1 && el.contains(node) && _isScrollable(node)) return node;
                if (node === el) break;
                node = node.parentElement;
            }

            if (preferredSelector) {
                var primary = el.querySelector(preferredSelector);
                if (_isScrollable(primary)) return primary;
            }
            return _isScrollable(el) ? el : null;
        }

        function _resetVisuals() {
            el.style.transform = '';
            el.style.opacity = '';
            if (parallaxOverlay) parallaxOverlay.style.opacity = '';
        }

        function _applyDrag(delta) {
            // Drag-dismiss only commits in positive direction; negative rubber-bands.
            var visual = delta;
            if (delta < 0) visual = rubberBand ? delta / rubberBandFactor : 0;
            var transform = (axis === 'y')
                ? 'translateY(' + visual + 'px)'
                : 'translateX(' + visual + 'px)';
            el.style.transform = transform;
            var op = Math.max(0.5, 1 - Math.max(0, delta) / opacityDenom);
            el.style.opacity = String(op);
            if (parallaxOverlay) parallaxOverlay.style.opacity = String(op);
            onProgress(delta, Math.max(0, Math.min(1, delta / threshold)));
        }

        function _onTouchStart(e) {
            if (skipIf && skipIf(e)) return;
            var t = e.touches[0];
            if (!t) return;
            startX = t.clientX;
            startY = t.clientY;
            startT = performance.now();
            currentDelta = 0;
            scrollOwner = _findScrollableOwner(e.target);
            scrollLocked = !!(blockIfScrolled && scrollOwner && _scrollPos(scrollOwner) > 0);
            tracking = true;
            dragging = false;
        }

        function _onTouchMove(e) {
            if (!tracking) return;
            var t = e.touches[0];
            if (!t) return;
            var delta = _readDelta(t);

            if (!dragging) {
                if (scrollLocked) return;
                if (Math.abs(delta) < DRAG_ACTIVATE_PX) return;

                // Upward/leftward movement belongs to content scrolling. Once
                // a gesture is classified that way, never convert the same
                // touch into a dismiss after the content reaches scrollTop 0.
                if (delta < 0) {
                    tracking = false;
                    scrollOwner = null;
                    scrollLocked = false;
                    return;
                }

                dragging = true;
                el.style.transition = 'none';
            }

            if (e.cancelable) e.preventDefault();
            currentDelta = delta;
            _applyDrag(currentDelta);
        }

        function _onTouchEnd() {
            if (!tracking) return;
            tracking = false;
            if (!dragging) {
                scrollOwner = null;
                scrollLocked = false;
                return;
            }
            dragging = false;
            var elapsed = Math.max(1, performance.now() - startT);
            var velocity = currentDelta / elapsed;
            el.style.transition = '';
            if (currentDelta > threshold) {
                if (hapticAt.commit) _hapticByName(hapticAt.commit);
                onCommit(currentDelta, velocity);
                // Clear inline styles AFTER onCommit — clearing before causes a
                // 1-frame snap-back; never clearing leaves residual transform
                // on the next open.
                _resetVisuals();
            } else {
                if (hapticAt.snap) _hapticByName(hapticAt.snap);
                _resetVisuals();
                onCancel(currentDelta, velocity);
            }
            scrollOwner = null;
            scrollLocked = false;
        }

        function _onTouchCancel() {
            if (!tracking && !dragging) return;
            tracking = false;
            scrollOwner = null;
            scrollLocked = false;
            if (!dragging) return;
            dragging = false;
            el.style.transition = '';
            _resetVisuals();
            onCancel(currentDelta, 0);
        }

        handle.addEventListener('touchstart', _onTouchStart, { passive: true });
        handle.addEventListener('touchmove', _onTouchMove, { passive: false });
        handle.addEventListener('touchend', _onTouchEnd);
        handle.addEventListener('touchcancel', _onTouchCancel);

        return {
            detach: function() {
                tracking = false;
                dragging = false;
                scrollOwner = null;
                scrollLocked = false;
                handle.removeEventListener('touchstart', _onTouchStart);
                handle.removeEventListener('touchmove', _onTouchMove);
                handle.removeEventListener('touchend', _onTouchEnd);
                handle.removeEventListener('touchcancel', _onTouchCancel);
            },
            snapTo: function(point) {
                if (point === 0) {
                    _resetVisuals();
                } else if (point === 1) {
                    onCommit(threshold + 1);
                }
            },
            close: function() { onCommit(threshold + 1); }
        };
    };

    // attachPullToRefresh — async onRefresh hook (sync or Promise).
    // Idempotent via `el._ptrAttached`.
    //
    // State machine:
    //   idle → (touchstart on scrollTop===0) pulling → (touchend ≥ threshold)
    //   refreshing → (settle chain done) idle.
    //
    // All exits route through `_reset()` so a single reset path handles
    // touchcancel, watchdog timeouts, and detach. The watchdog catches the
    // case where an in-flight refresh's setTimeouts get throttled (background
    // tab) or the `onRefresh` promise leaks — without it, `refreshing=true`
    // can pin and block every subsequent pull until reload.
    G.attachPullToRefresh = function(el, opts) {
        opts = opts || {};
        if (!isMobile()) return function() {};
        if (!el || el._ptrAttached) return function() {};

        var refreshDistance = opts.refreshDistance || G.PULL_TO_REFRESH_DISTANCE_PX;
        var rubberBandFactor = opts.rubberBandFactor || G.PULL_TO_REFRESH_RUBBER_BAND_FACTOR;
        var minRefreshMs = opts.minRefreshMs || G.PULL_TO_REFRESH_MIN_MS;
        var successMs = opts.successMs || G.PULL_TO_REFRESH_SUCCESS_MS;
        var hapticAt = opts.hapticAt || {};
        var onRefresh = opts.onRefresh || function() {};
        var skipIf = opts.skipIf || null;
        // Watchdog grace = expected total settle time + 2s slack. Anything
        // longer than this is a stuck state and we forcibly reset.
        var WATCHDOG_GRACE_MS = 2000;

        var indicator = document.createElement('div');
        indicator.className = 'ptr-indicator';
        indicator.innerHTML =
            '<div class="ptr-arrow"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="7 13 12 18 17 13"/><line x1="12" y1="2" x2="12" y2="18"/></svg></div>' +
            '<div class="ptr-spinner"></div>' +
            '<div class="ptr-check"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 12 10 18 20 6"/></svg></div>';
        el.style.position = el.style.position || 'relative';
        el.appendChild(indicator);
        var arrowSvg = indicator.querySelector('.ptr-arrow svg');

        var startY = 0, pulling = false, refreshing = false;
        // All in-flight setTimeouts are tracked so `_reset()` can cancel them
        // atomically. Without this, a stale `setTimeout` fires after reset
        // and re-pollutes the indicator state.
        var settleTimers = [];
        var watchdogTimer = null;

        function _trackTimer(id) {
            settleTimers.push(id);
            return id;
        }

        function _clearTimers() {
            for (var i = 0; i < settleTimers.length; i++) {
                clearTimeout(settleTimers[i]);
            }
            settleTimers = [];
            if (watchdogTimer) {
                clearTimeout(watchdogTimer);
                watchdogTimer = null;
            }
        }

        function _reset() {
            _clearTimers();
            pulling = false;
            refreshing = false;
            indicator.classList.remove('dragging', 'pulling', 'refreshing', 'success');
            indicator.style.top = '';
            if (arrowSvg) arrowSvg.style.transform = '';
        }

        function _onTouchStart(e) {
            if (skipIf && skipIf(e)) return;
            if (refreshing) return;
            if (el.scrollTop > 0) return;
            startY = e.touches[0].clientY;
            pulling = true;
        }

        function _onTouchMove(e) {
            if (!pulling || refreshing) return;
            var dy = e.touches[0].clientY - startY;
            if (dy < 0) { pulling = false; indicator.classList.remove('dragging'); return; }
            // Past 10px, preventDefault so the page doesn't scroll out from under us.
            if (dy > 10) e.preventDefault();
            var damped = dy > refreshDistance
                ? refreshDistance + (dy - refreshDistance) / rubberBandFactor
                : dy;
            indicator.style.top = (damped - 40) + 'px';
            indicator.classList.add('dragging');
            var progress = Math.min(dy / refreshDistance, 1);
            arrowSvg.style.transform = 'rotate(' + (progress * 180) + 'deg)';
        }

        function _settleRefreshState() {
            indicator.classList.remove('refreshing');
            indicator.classList.add('success');
            if (hapticAt.success) _hapticByName(hapticAt.success);
            _trackTimer(setTimeout(function() {
                indicator.classList.remove('success');
                indicator.style.top = '-40px';
                _trackTimer(setTimeout(function() {
                    refreshing = false;
                    indicator.style.top = '';
                    if (watchdogTimer) {
                        clearTimeout(watchdogTimer);
                        watchdogTimer = null;
                    }
                }, 350));
            }, successMs));
        }

        function _onTouchEnd() {
            if (!pulling) return;
            pulling = false;
            indicator.classList.remove('dragging');

            var currentTop = parseFloat(indicator.style.top) || -40;
            if (currentTop >= refreshDistance - 40) {
                refreshing = true;
                indicator.classList.add('pulling');
                indicator.style.top = '12px';
                if (hapticAt.trigger) _hapticByName(hapticAt.trigger);

                _trackTimer(setTimeout(function() {
                    indicator.classList.remove('pulling');
                    indicator.classList.add('refreshing');
                    arrowSvg.style.transform = '';
                }, 50));

                // Wait for refresh promise + minimum-visible-duration.
                var refreshStarted = performance.now();
                var refreshResult;
                try { refreshResult = onRefresh(); } catch (_) {}
                var refreshDone = (refreshResult && typeof refreshResult.then === 'function')
                    ? refreshResult.catch(function() {})
                    : Promise.resolve();
                refreshDone.then(function() {
                    if (!refreshing) return;       // _reset() raced us
                    var elapsed = performance.now() - refreshStarted;
                    var remaining = Math.max(0, minRefreshMs - elapsed);
                    _trackTimer(setTimeout(_settleRefreshState, remaining));
                });

                // Watchdog: forcibly reset if the settle chain hasn't completed
                // within the expected total + 2s slack. Defends against:
                //   - browser timer throttling when the tab is backgrounded
                //   - `onRefresh` returning a Promise that never resolves
                //   - any code path that leaves `refreshing=true` orphaned
                var watchdogBudget = minRefreshMs + successMs + 350 + WATCHDOG_GRACE_MS;
                watchdogTimer = setTimeout(function() {
                    if (refreshing) {
                        if (window.RS && typeof window.RS.diag === 'function') {
                            window.RS.diag('warn', '[gestures] PTR watchdog firing — forcing reset');
                        }
                        _reset();
                    }
                }, watchdogBudget);
            } else {
                indicator.style.top = '-40px';
                arrowSvg.style.transform = '';
                _trackTimer(setTimeout(function() { indicator.style.top = ''; }, 350));
            }
        }

        function _onTouchCancel() {
            // System gesture arbitration, modal opening, or app backgrounding
            // can cancel the touch sequence without dispatching touchend. If
            // we were mid-pull, fully reset so the indicator does not get
            // pinned at a stale top value. If we were already refreshing,
            // touchcancel can't fire (no active touch) — but if it somehow
            // does, leave the in-flight settle chain + watchdog to handle it.
            if (pulling) _reset();
        }

        function _onVisibilityChange() {
            // Coming back from background: a settle chain that crossed the
            // visibility transition may have been throttled. The watchdog
            // already covers this, but eagerly resetting on visibility
            // restore avoids the user seeing a still-spinning indicator
            // for the full watchdog grace window.
            if (!document.hidden && refreshing) {
                // Give the settle chain ~250ms to complete naturally before
                // we force-reset, in case timers fire on the next tick.
                setTimeout(function() {
                    if (!document.hidden && refreshing) _reset();
                }, 250);
            }
        }

        el.addEventListener('touchstart', _onTouchStart, { passive: true });
        el.addEventListener('touchmove', _onTouchMove, { passive: false });
        el.addEventListener('touchend', _onTouchEnd, { passive: true });
        el.addEventListener('touchcancel', _onTouchCancel, { passive: true });
        document.addEventListener('visibilitychange', _onVisibilityChange);
        el._ptrAttached = true;

        return function detach() {
            _reset();
            el.removeEventListener('touchstart', _onTouchStart);
            el.removeEventListener('touchmove', _onTouchMove);
            el.removeEventListener('touchend', _onTouchEnd);
            el.removeEventListener('touchcancel', _onTouchCancel);
            document.removeEventListener('visibilitychange', _onVisibilityChange);
            if (indicator.parentNode) indicator.parentNode.removeChild(indicator);
            el._ptrAttached = false;
        };
    };
})();
