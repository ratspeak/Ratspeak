// Native `ratspeak://channel` handoff. This frontend never subscribes to the
// plugin's URL event: Rust validates platform-delivered URLs and this bridge
// drains only a typed, key-free preview target from a bounded memory inbox.
(function() {
    'use strict';

    var stagedTarget = null;
    var drainInFlight = false;
    var drainScheduled = false;
    var needsDrain = true;
    var listenerAttached = false;
    var listenerAttempts = 0;
    var observer = null;

    function isNativeShell() {
        return window.__RATSPEAK_DESKTOP__ === true ||
            window.__RATSPEAK_MOBILE__ === true;
    }

    function uiBlocksChannelShare() {
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
            drainNativeChannelShare();
        }, delay || 0);
    }

    function presentStagedTarget() {
        if (!stagedTarget || uiBlocksChannelShare()) return false;
        var target = stagedTarget;
        stagedTarget = null;
        try {
            if (typeof window.channelsOpenNativeSharedChannel !== 'function' ||
                    window.channelsOpenNativeSharedChannel(target) !== true) {
                window.RS.diag(
                    'warn',
                    '[native-channel-share] rejected malformed typed target'
                );
            }
        } catch (error) {
            window.RS.diag(
                'warn',
                '[native-channel-share] could not present typed target:',
                error
            );
        }
        return true;
    }

    function drainNativeChannelShare() {
        if (!isNativeShell()) return;
        if (drainInFlight) return;
        if (uiBlocksChannelShare()) return;
        if (stagedTarget) {
            presentStagedTarget();
            return;
        }
        if (!needsDrain) return;

        needsDrain = false;
        drainInFlight = true;
        RS.invoke('take_native_channel_share').then(function(target) {
            if (target) stagedTarget = target;
        }, function(error) {
            needsDrain = true;
            window.RS.diag(
                'warn',
                '[native-channel-share] inbox unavailable:',
                error
            );
        }).then(function() {
            drainInFlight = false;
            presentStagedTarget();
            if (needsDrain) scheduleDrain(500);
        });
    }

    function signalNativeChannelShare() {
        needsDrain = true;
        scheduleDrain(0);
    }

    function attachNativeListener() {
        if (!isNativeShell() || listenerAttached) return;
        listenerAttempts++;
        RS.listen(
            'native_channel_share_available',
            signalNativeChannelShare,
            { required: true }
        ).then(function() {
            listenerAttached = true;
            signalNativeChannelShare();
        }, function(error) {
            window.RS.diag(
                'warn',
                '[native-channel-share] listener unavailable:',
                error
            );
            if (listenerAttempts < 30) {
                setTimeout(attachNativeListener, 1000);
            }
        });
    }

    function installUiReadinessObserver() {
        if (observer || typeof MutationObserver !== 'function' || !document.body) {
            return;
        }
        observer = new MutationObserver(function() {
            if (needsDrain || stagedTarget) scheduleDrain(0);
        });
        observer.observe(document.body, {
            attributes: true,
            attributeFilter: ['class'],
            childList: true,
            subtree: true
        });
    }

    if (!isNativeShell()) return;
    installUiReadinessObserver();
    scheduleDrain(0);
    attachNativeListener();
})();
