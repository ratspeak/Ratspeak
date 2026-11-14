// Transcript scrolling shared by Messages and Channels: open and follow at the
// latest message; never move a reader who scrolled into history.

var RS = window.RS || {};

(function() {
    var NEAR_BOTTOM_PX = 160;
    var states = new WeakMap();

    function stateFor(container) {
        var state = states.get(container);
        if (!state) {
            state = {
                scrollTop: container ? container.scrollTop : 0,
                lastUserScrollUpAt: 0,
                programmaticScrollUntil: 0,
                settleToken: 0,
                followLatest: nearBottom(container),
                pinWhenShown: false
            };
            states.set(container, state);
        }
        return state;
    }

    function bottomGap(container) {
        if (!container) return 0;
        return Math.max(0, container.scrollHeight - container.clientHeight - container.scrollTop);
    }

    function nearBottom(container) {
        if (!container) return true;
        return bottomGap(container) <= NEAR_BOTTOM_PX;
    }

    // A hidden view reports zero geometry and ignores scrollTop writes; the
    // browser restores its previous offset when it is shown again.
    function isHidden(container) {
        return !container.clientHeight;
    }

    function wire(container) {
        if (!container || container._chatScrollAttached) return;
        container._chatScrollAttached = true;
        var state = stateFor(container);
        state.scrollTop = container.scrollTop;
        state.followLatest = nearBottom(container);
        // A real touch takes ownership from any post-render settle callbacks.
        // Without this, Android can begin a history pan while an optimistic send
        // still has delayed bottom pins queued, making the transcript appear
        // frozen as those callbacks repeatedly pull it back to the latest edge.
        container.addEventListener('touchstart', function() {
            state.settleToken++;
            state.programmaticScrollUntil = 0;
            state.scrollTop = container.scrollTop;
        }, { passive: true });
        container.addEventListener('scroll', function() {
            var now = Date.now();
            var currentTop = container.scrollTop;
            if (now >= state.programmaticScrollUntil && currentTop < state.scrollTop - 2) {
                state.lastUserScrollUpAt = now;
                state.followLatest = false;
            }
            if (nearBottom(container)) {
                state.lastUserScrollUpAt = 0;
                state.followLatest = true;
            }
            state.scrollTop = currentTop;
        }, { passive: true });
        container.addEventListener('wheel', function(e) {
            if (e.deltaY < -1) {
                state.settleToken++;
                state.programmaticScrollUntil = 0;
                state.lastUserScrollUpAt = Date.now();
                state.followLatest = false;
            }
        }, { passive: true });
        if (typeof ResizeObserver === 'function') {
            new ResizeObserver(function() {
                if (state.pinWhenShown && !isHidden(container)) pinToBottom(container);
            }).observe(container);
        }
    }

    function setTop(container, top) {
        if (!container) return;
        var state = stateFor(container);
        state.programmaticScrollUntil = Date.now() + 150;
        container.scrollTop = top;
        state.scrollTop = container.scrollTop;
    }

    function pinToBottom(container) {
        if (!container) return;
        var state = stateFor(container);
        state.followLatest = true;
        var token = ++state.settleToken;
        function pin() {
            if (token !== state.settleToken || !container.isConnected) return;
            if (isHidden(container)) {
                state.pinWhenShown = true;
                return;
            }
            state.pinWhenShown = false;
            setTop(container, container.scrollHeight);
        }
        pin();
        if (typeof requestAnimationFrame === 'function') requestAnimationFrame(pin);
        [40, 140, 360].forEach(function(delay) {
            setTimeout(pin, delay);
        });
    }

    function compensateImageLoad(container, img, before) {
        if (!container || !img || !before) return;
        if (!container.isConnected || !img.isConnected || !container.contains(img)) return;
        var afterHeight = container.scrollHeight;
        var delta = afterHeight - before.scrollHeight;
        if (Math.abs(delta) < 1) return;
        var containerRect = container.getBoundingClientRect();
        // If an older image above the viewport gains height, preserve the user's
        // current reading position by moving the scroll offset by the exact delta.
        if (before.imgTop < containerRect.top + 8) {
            setTop(container, container.scrollTop + delta);
        }
    }

    function capture(container) {
        var controller = container ? stateFor(container) : null;
        if (controller && isHidden(container)) {
            return {
                scrollTop: controller.scrollTop,
                scrollHeight: 0,
                nearBottom: controller.followLatest,
                followLatest: controller.followLatest,
            };
        }
        var isNearBottom = nearBottom(container);
        if (controller && isNearBottom) {
            controller.followLatest = true;
            controller.lastUserScrollUpAt = 0;
        }
        return {
            scrollTop: container ? container.scrollTop : 0,
            scrollHeight: container ? container.scrollHeight : 0,
            nearBottom: isNearBottom,
            followLatest: controller ? controller.followLatest && isNearBottom : isNearBottom,
        };
    }

    function restore(container, state) {
        if (!container || !state) return;
        var controller = stateFor(container);
        controller.settleToken++;
        controller.pinWhenShown = false;
        controller.followLatest = !!state.followLatest;
        if (isHidden(container)) return;
        var maxScroll = Math.max(0, container.scrollHeight - container.clientHeight);
        setTop(container, Math.min(maxScroll, Math.max(0, state.scrollTop)));
    }

    function isFollowing(container) {
        if (!container) return true;
        var state = stateFor(container);
        if (!isHidden(container) && nearBottom(container)) {
            state.followLatest = true;
            state.lastUserScrollUpAt = 0;
        }
        return state.followLatest;
    }

    function applyAfterRender(container, state, options) {
        options = options || {};
        var shouldFollow = !!(state && state.followLatest && state.nearBottom);
        var shouldPin = !!options.forceScrollBottom ||
            (!!options.stickToBottom && shouldFollow) ||
            (!options.preserveScroll && shouldFollow);

        if (shouldPin) {
            pinToBottom(container);
        } else {
            restore(container, state);
        }
        return shouldPin;
    }

    function watchImages(container, shouldPin) {
        if (!container) return;
        container.querySelectorAll('img').forEach(function(img) {
            if (img.complete && img.naturalWidth) return;
            var before = {
                scrollHeight: container.scrollHeight,
                imgTop: img.getBoundingClientRect().top
            };
            img.addEventListener('load', function() {
                // An old render may finish decoding after its nodes were replaced.
                // It no longer owns any geometry in this transcript.
                if (!img.isConnected || !container.contains(img)) return;
                if (shouldPin && isFollowing(container)) {
                    pinToBottom(container);
                    return;
                }
                if (typeof requestAnimationFrame === 'function') {
                    requestAnimationFrame(function() {
                        compensateImageLoad(container, img, before);
                    });
                } else {
                    compensateImageLoad(container, img, before);
                }
            }, { once: true });
        });
    }

    RS.chatScroll = {
        wire: wire,
        capture: capture,
        applyAfterRender: applyAfterRender,
        pinToBottom: pinToBottom,
        restore: restore,
        setTop: setTop,
        nearBottom: nearBottom,
        isFollowing: isFollowing,
        watchImages: watchImages
    };
})();
