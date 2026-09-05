// Native `ratspeak://channel` handoff. This frontend never subscribes to the
// plugin's URL event: Rust validates platform-delivered URLs and this bridge
// previews a typed, key-free target and acknowledges its exact inbox revision
// only after a visible page has presented it. A retiring page cannot consume a
// link merely by requesting it and then losing its IPC response.
(function() {
    'use strict';

    var stagedDelivery = null;
    var pendingAcknowledgement = null;
    var acknowledgementAttempts = 0;
    var peekAttempts = 0;
    var availabilityEpoch = {};
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
            drainNativeChannelShare();
        }, delay || 0);
    }

    function acknowledgePresentedTarget() {
        if (!pendingAcknowledgement || drainInFlight ||
                document.visibilityState !== 'visible' || acknowledgementAttempts >= 3) return;
        var delivery = pendingAcknowledgement;
        acknowledgementAttempts++;
        drainInFlight = true;
        RS.invoke('ack_native_channel_share', {
            revision: delivery.revision,
            activityGeneration: delivery.activityGeneration
        }).then(function() {
            // False means a newer revision or a retired/paused native owner.
            // Rust retains any pending target and re-signals on readiness;
            // this old presentation must not remove it.
            pendingAcknowledgement = null;
            acknowledgementAttempts = 0;
        }, function() {
            RS.diag('warn', '[native-channel-share] acknowledgement unavailable');
        }).then(function() {
            drainInFlight = false;
            if (pendingAcknowledgement) {
                if (acknowledgementAttempts < 3) scheduleDrain(500);
            } else if (needsDrain) scheduleDrain(0);
        });
    }

    function presentStagedTarget() {
        if (!stagedDelivery || uiBlocksChannelShare()) return false;
        var delivery = stagedDelivery;
        try {
            if (typeof window.channelsOpenNativeSharedChannel !== 'function' ||
                    window.channelsOpenNativeSharedChannel(delivery.target) !== true) {
                window.RS.diag(
                    'warn',
                    '[native-channel-share] rejected malformed typed target'
                );
                return false;
            }
        } catch (error) {
            window.RS.diag(
                'warn',
                '[native-channel-share] could not present typed target:',
                error
            );
            return false;
        }
        stagedDelivery = null;
        pendingAcknowledgement = delivery;
        acknowledgementAttempts = 0;
        acknowledgePresentedTarget();
        return true;
    }

    function drainNativeChannelShare() {
        if (!isNativeShell()) return;
        if (drainInFlight) return;
        if (pendingAcknowledgement) {
            acknowledgePresentedTarget();
            return;
        }
        if (uiBlocksChannelShare()) return;
        if (stagedDelivery) {
            presentStagedTarget();
            return;
        }
        if (!needsDrain) return;
        if (peekAttempts >= 3) return;

        needsDrain = false;
        drainInFlight = true;
        peekAttempts++;
        var requestEpoch = availabilityEpoch;
        RS.invoke('peek_native_channel_share').then(function(delivery) {
            peekAttempts = 0;
            // A newer native signal supersedes any unpresented response.
            // Keep the newer target in Rust until this page samples it again.
            if (requestEpoch !== availabilityEpoch) return;
            if (delivery && typeof delivery.revision === 'string' &&
                    /^[1-9][0-9]{0,19}$/.test(delivery.revision) && delivery.target &&
                    (delivery.activityGeneration === null ||
                        (typeof delivery.activityGeneration === 'string' &&
                            /^[1-9][0-9]{0,19}$/.test(delivery.activityGeneration)))) {
                stagedDelivery = delivery;
            } else if (delivery) {
                RS.diag('warn', '[native-channel-share] rejected malformed inbox delivery');
            }
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
            if (needsDrain && peekAttempts < 3) scheduleDrain(500);
        });
    }

    function signalNativeChannelShare() {
        needsDrain = true;
        acknowledgementAttempts = 0;
        peekAttempts = 0;
        availabilityEpoch = {};
        // A failed/malformed unpresented target must not monopolize later
        // valid arrivals. Never discard an acknowledgement of a shown target.
        stagedDelivery = null;
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
            if (needsDrain || stagedDelivery) scheduleDrain(0);
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
        acknowledgementAttempts = 0;
        // Always re-sample Rust on foregrounding, including after a dropped
        // signal while no visible WebView existed.
        signalNativeChannelShare();
    });
    installUiReadinessObserver();
    scheduleDrain(0);
    attachNativeListener();
})();
