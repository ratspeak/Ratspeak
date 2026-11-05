// Native `lxm://` handoff. Rust keeps the delivered link until this page has
// shown it and the user acted on or dismissed the decoder; only then is the
// exact link acknowledged, so a retiring page cannot drop it.
(function() {
    'use strict';

    var staged = null;
    var presented = null;
    var drainInFlight = false;
    var drainScheduled = false;
    var needsDrain = true;
    var peekAttempts = 0;
    var ackAttempts = 0;
    var listenerAttached = false;
    var listenerAttempts = 0;
    var observer = null;

    function isNativeShell() {
        return window.__RATSPEAK_DESKTOP__ === true ||
            window.__RATSPEAK_MOBILE__ === true;
    }

    function uiBlocksPaperLink() {
        if (document.visibilityState !== 'visible') return true;
        if (typeof _isSetupActive === 'function' && _isSetupActive()) return true;
        return !!document.querySelector(
            '.bottom-sheet.open, .bottom-sheet-overlay.active, ' +
            '.modal-overlay.active, .game-modal-overlay, .block-list-overlay, ' +
            '#rs-image-viewer.open, .action-popover.open, ' +
            '[class*="-scrim"].active, ' +
            '[role="dialog"][aria-modal="true"]:not(.bottom-sheet)'
        );
    }

    function scheduleDrain(delay) {
        if (drainScheduled || !isNativeShell()) return;
        drainScheduled = true;
        setTimeout(function() {
            drainScheduled = false;
            drain();
        }, delay || 0);
    }

    function acknowledge(uri) {
        if (drainInFlight || ackAttempts >= 3) return;
        ackAttempts++;
        drainInFlight = true;
        RS.invoke('ack_native_paper_link', { uri: uri }).then(function() {
            if (presented && presented.uri === uri) presented = null;
            ackAttempts = 0;
        }, function() {
            RS.diag('warn', '[native-paper-link] acknowledgement unavailable');
        }).then(function() {
            drainInFlight = false;
            if (presented && presented.closed) {
                if (ackAttempts < 3) setTimeout(function() { acknowledge(presented.uri); }, 500);
            } else if (needsDrain) scheduleDrain(0);
        });
    }

    function present() {
        if (!staged || presented || uiBlocksPaperLink()) return false;
        if (typeof window.openPaperDecoder !== 'function') return false;
        var uri = staged;
        staged = null;
        var record = { uri: uri, closed: false };
        presented = record;
        ackAttempts = 0;
        var opened = window.openPaperDecoder({
            uri: uri,
            autoReview: true,
            onClose: function() {
                record.closed = true;
                acknowledge(uri);
            }
        });
        if (!opened) {
            presented = null;
            return false;
        }
        return true;
    }

    function drain() {
        if (!isNativeShell() || drainInFlight) return;
        if (presented) {
            if (presented.closed) acknowledge(presented.uri);
            return;
        }
        if (uiBlocksPaperLink()) return;
        if (staged) {
            present();
            return;
        }
        if (!needsDrain || peekAttempts >= 3) return;
        needsDrain = false;
        drainInFlight = true;
        peekAttempts++;
        RS.invoke('peek_native_paper_link').then(function(delivery) {
            peekAttempts = 0;
            var uri = delivery && typeof delivery.uri === 'string' ? delivery.uri.trim() : '';
            if (uri && /^lxm:\/\/[A-Za-z0-9_-]+$/i.test(uri) && uri.length <= 4096) {
                staged = uri;
            } else if (uri) {
                RS.diag('warn', '[native-paper-link] rejected malformed link');
            }
        }, function(error) {
            needsDrain = true;
            RS.diag('warn', '[native-paper-link] inbox unavailable:', error);
        }).then(function() {
            drainInFlight = false;
            present();
            if (needsDrain && peekAttempts < 3) scheduleDrain(500);
        });
    }

    function signal() {
        needsDrain = true;
        peekAttempts = 0;
        if (!presented) staged = null;
        scheduleDrain(0);
    }

    function attachListener() {
        if (!isNativeShell() || listenerAttached) return;
        listenerAttempts++;
        RS.listen('native_paper_link_available', signal, { required: true }).then(function() {
            listenerAttached = true;
            signal();
        }, function(error) {
            RS.diag('warn', '[native-paper-link] listener unavailable:', error);
            if (listenerAttempts < 30) setTimeout(attachListener, 1000);
        });
    }

    function installUiReadinessObserver() {
        if (observer || typeof MutationObserver !== 'function' || !document.body) return;
        observer = new MutationObserver(function() {
            if (needsDrain || staged || (presented && presented.closed)) scheduleDrain(0);
        });
        observer.observe(document.body, {
            attributes: true,
            attributeFilter: ['class'],
            childList: true,
            subtree: true
        });
    }

    if (!isNativeShell()) return;
    document.addEventListener('visibilitychange', function() {
        if (document.visibilityState !== 'visible') return;
        ackAttempts = 0;
        signal();
    });
    installUiReadinessObserver();
    scheduleDrain(0);
    attachListener();
})();
